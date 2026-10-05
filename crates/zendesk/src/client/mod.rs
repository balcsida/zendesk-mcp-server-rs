//! Zendesk REST API client.
//!
//! Every method returns `serde_json::Value` shaped exactly like the Python server's
//! output, so MCP clients see no difference after the rewrite.

use std::collections::HashSet;
use std::fmt::Display;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use base64::Engine;
use pulldown_cmark::{Event, Options, Parser, html};
use serde_json::{Map, Value, json};

use crate::auth::Auth;

/// 10 MB hard cap on attachments, against image bombs and token budget blowout.
pub const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;

/// Image types a tool may return. SVG is excluded: it can contain active content.
pub const ALLOWED_IMAGE_TYPES: [&str; 4] = ["image/jpeg", "image/png", "image/gif", "image/webp"];

/// Retries of a request answered 429, or 503 when it is a GET or DELETE.
const MAX_RETRIES: u32 = 3;
/// Longest `Retry-After` honoured, in seconds.
const MAX_RETRY_AFTER_SECS: u64 = 30;
/// Pages followed per listing before returning what was collected.
const MAX_PAGES: usize = 1000;
/// Largest `days_back` accepted by `get_sla_breaches` (100 years).
const MAX_DAYS_BACK: u64 = 36_500;

/// Render Markdown (or plain text) to the HTML Zendesk stores as `html_body`.
///
/// CommonMark plus tables and strikethrough; a single newline becomes `<br>` so plain
/// text keeps its line breaks; raw HTML in the input is passed through for Zendesk to
/// sanitize server-side, and input that is entirely HTML is returned unchanged.
pub fn markdown_to_html(text: &str) -> String {
    // ponytail: text that starts with a tag and ends with `>` is taken to be HTML already
    // (e.g. a `get_article` body written back); Markdown that happens to look like that is
    // left unconverted. Upgrade to a real HTML check if that bites.
    let trimmed = text.trim();
    let mut chars = trimmed.chars();
    if chars.next() == Some('<')
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '!')
        && trimmed.ends_with('>')
    {
        return text.to_string();
    }
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let events = Parser::new_ext(text, options).map(|event| match event {
        Event::SoftBreak => Event::HardBreak,
        other => other,
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

/// A fetched, validated image attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub content_type: String,
    /// Base64 (standard alphabet, padded) of the file bytes.
    pub data_base64: String,
}

/// Fields accepted when creating a ticket.
#[derive(Debug, Clone, Default)]
pub struct CreateTicket {
    pub subject: String,
    pub description: String,
    pub requester_id: Option<u64>,
    pub assignee_id: Option<u64>,
    pub priority: Option<String>,
    pub ticket_type: Option<String>,
    pub tags: Option<Vec<String>>,
    pub custom_fields: Option<Vec<Value>>,
    pub group_id: Option<u64>,
    pub ticket_form_id: Option<u64>,
    pub brand_id: Option<u64>,
    pub problem_id: Option<u64>,
    pub via_followup_source_id: Option<u64>,
    pub custom_status_id: Option<u64>,
    pub due_at: Option<String>,
    pub external_id: Option<String>,
    /// Whether the description is a public comment; Zendesk defaults to public.
    pub public: Option<bool>,
    /// `{name, email}` of the requester, created as an end user if needed.
    pub requester: Option<Value>,
    pub email_ccs: Option<Vec<String>>,
    /// Tokens from `upload_attachment`, sent as `comment.uploads`.
    pub upload_tokens: Option<Vec<String>>,
}

#[derive(Clone)]
pub struct ZendeskClient {
    http: reqwest::Client,
    subdomain: String,
    /// `https://{subdomain}.zendesk.com/api/v2` in production; tests point it at a mock.
    base_url: String,
    auth: Auth,
    /// Delay between polls of a background job.
    pub(super) job_poll_interval: Duration,
}

/// Copy `keys` out of `obj`, defaulting to null (or `[]` for `array_keys`) when absent.
pub(super) fn pick(obj: &Value, keys: &[&str], array_keys: &[&str]) -> Value {
    let mut out = Map::new();
    for key in keys {
        let value = match obj.get(*key) {
            Some(v) if !v.is_null() => v.clone(),
            _ if array_keys.contains(key) => json!([]),
            _ => Value::Null,
        };
        out.insert((*key).to_string(), value);
    }
    Value::Object(out)
}

/// `pick` applied to every element of `data[key]`.
pub(super) fn pick_all(data: &Value, key: &str, keys: &[&str], array_keys: &[&str]) -> Value {
    let items = data
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[][..], |a| a);
    Value::Array(items.iter().map(|i| pick(i, keys, array_keys)).collect())
}

/// `[{id, value}]` from a ticket's raw `custom_fields`.
pub(super) fn custom_fields(ticket: &Value) -> Value {
    let items = ticket.get("custom_fields").and_then(Value::as_array);
    Value::Array(
        items
            .map_or(&[][..], |a| a)
            .iter()
            .map(|f| pick(f, &["id", "value"], &[]))
            .collect(),
    )
}

/// The shape `create_ticket` and `update_ticket` return.
pub(super) fn full_ticket(data: &Value) -> Result<Value> {
    let ticket = data
        .get("ticket")
        .ok_or_else(|| anyhow!("Zendesk response has no 'ticket' object"))?;
    let mut out = pick(
        ticket,
        &[
            "id",
            "subject",
            "description",
            "status",
            "priority",
            "type",
            "created_at",
            "updated_at",
            "requester_id",
            "assignee_id",
            "organization_id",
            "tags",
            "group_id",
            "ticket_form_id",
            "brand_id",
            "custom_status_id",
            "problem_id",
            "due_at",
            "external_id",
        ],
        &["tags"],
    );
    out["custom_fields"] = custom_fields(ticket);
    Ok(out)
}

pub(super) fn object<'a>(data: &'a Value, key: &str) -> Result<&'a Value> {
    data.get(key)
        .ok_or_else(|| anyhow!("Zendesk response has no '{key}' object"))
}

/// `id -> name` from the `users` Zendesk side-loads when a request asks for `include=users`.
pub(super) fn side_loaded_user_names(data: &Value) -> std::collections::HashMap<u64, String> {
    data.get("users")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|u| Some((u.get("id")?.as_u64()?, u.get("name")?.as_str()?.to_string())))
        .collect()
}

/// Add `requester_name`/`assignee_name` to `out` by resolving `raw`'s `requester_id` and
/// `assignee_id` against the side-loaded `names`. Left unset when Zendesk didn't side-load
/// the user (e.g. it was deleted, or the caller didn't request `include=users`).
pub(super) fn with_user_names(
    mut out: Value,
    raw: &Value,
    names: &std::collections::HashMap<u64, String>,
) -> Value {
    for (id_key, name_key) in [
        ("requester_id", "requester_name"),
        ("assignee_id", "assignee_name"),
    ] {
        if let Some(name) = raw
            .get(id_key)
            .and_then(Value::as_u64)
            .and_then(|id| names.get(&id))
        {
            out[name_key] = json!(name);
        }
    }
    out
}

/// Trim a Zendesk `job_status` object to the shape the job tools return.
/// `pending` is true while the job is `queued` or `working`.
pub(super) fn job_summary(job: &Value) -> Value {
    let mut out = pick(
        job,
        &["id", "status", "progress", "total", "message", "url"],
        &[],
    );
    out["pending"] = json!(matches!(
        job.get("status").and_then(Value::as_str),
        Some("queued" | "working")
    ));
    let results: Vec<Value> = job
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|r| {
            let mut item = pick(r, &["id", "action", "status", "success"], &[]);
            for key in ["errors", "error"] {
                if let Some(v) = r.get(key).filter(|v| !v.is_null()) {
                    item[key] = v.clone();
                }
            }
            item
        })
        .collect();
    out["failed_count"] = json!(results.iter().filter(|r| r["success"] == false).count());
    out["results"] = Value::Array(results);
    out
}

/// Prefix an error the way the Python server worded it.
pub(super) fn ctx(prefix: impl Display) -> impl FnOnce(anyhow::Error) -> anyhow::Error {
    move |e| anyhow!("{prefix}: {e:#}")
}

/// Parse a response body as JSON; some Zendesk endpoints answer 200 with no body.
async fn json_or_null(resp: reqwest::Response) -> Result<Value> {
    let bytes = resp.bytes().await?;
    if bytes.trim_ascii().is_empty() {
        return Ok(Value::Null);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub(super) fn magic_matches(content_type: &str, bytes: &[u8]) -> bool {
    match content_type {
        "image/jpeg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "image/png" => bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        _ => false,
    }
}

/// Fail on a non-success status with Zendesk's body (truncated), never the request headers.
pub(super) async fn ensure_success(
    resp: reqwest::Response,
    label: &str,
) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.bytes().await.unwrap_or_default();
    Err(status_error(status, label, &body))
}

pub(super) fn status_error(status: reqwest::StatusCode, label: &str, body: &[u8]) -> anyhow::Error {
    let text: String = String::from_utf8_lossy(body).chars().take(500).collect();
    anyhow!("Zendesk API error HTTP {status} for {label}: {text}")
}

/// A path segment that came from the model or from Zendesk: percent-encode everything but
/// unreserved characters so it cannot change the route, and refuse `.` and `..`.
pub(super) fn segment(value: &str) -> Result<String> {
    if matches!(value, "" | "." | "..") {
        bail!("'{value}' is not a valid path segment");
    }
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    Ok(out)
}

/// `help_center`, or `help_center/{locale}` when a locale is given: Zendesk takes the
/// locale as a path segment, not a query parameter.
pub(super) fn help_center_path(locale: Option<&str>) -> Result<String> {
    match locale.filter(|l| !l.is_empty()) {
        Some(locale) => Ok(format!("help_center/{}", segment(locale)?)),
        None => Ok("help_center".into()),
    }
}

impl ZendeskClient {
    pub fn new(subdomain: &str, auth: Auth, http: reqwest::Client) -> Self {
        let base_url = format!("https://{subdomain}.zendesk.com/api/v2");
        Self::with_base_url(subdomain, auth, http, base_url)
    }

    pub fn with_base_url(
        subdomain: &str,
        auth: Auth,
        http: reqwest::Client,
        base_url: String,
    ) -> Self {
        ZendeskClient {
            http,
            subdomain: subdomain.to_string(),
            base_url,
            auth,
            job_poll_interval: Duration::from_secs(1),
        }
    }

    #[cfg(test)]
    pub(super) fn with_job_poll_interval(mut self, interval: Duration) -> Self {
        self.job_poll_interval = interval;
        self
    }

    pub(super) fn url(
        &self,
        path: &str,
        params: &[(&str, &(dyn Display + Sync))],
    ) -> Result<url::Url> {
        let mut url = url::Url::parse(&format!("{}/{path}", self.base_url))?;
        if !params.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in params {
                pairs.append_pair(key, &value.to_string());
            }
        }
        Ok(url)
    }

    /// Send an authenticated request. After a 401 `invalid_token` under OAuth the token is
    /// renewed and the request sent exactly once more.
    pub(super) async fn send(
        &self,
        make: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response> {
        let request = make().build()?;
        let label = format!("{} {}", request.method(), request.url().path());

        let value = self.auth.value().await?;
        let mut resp = value.apply(make()).send().await?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED
            && let Some(provider) = self.auth.oauth()
        {
            let body = resp.bytes().await.unwrap_or_default();
            if !crate::oauth::is_invalid_token_body(&body) {
                return Err(status_error(
                    reqwest::StatusCode::UNAUTHORIZED,
                    &label,
                    &body,
                ));
            }
            provider
                .renew(
                    "Zendesk reported the access token as invalid",
                    value.bearer_token(),
                )
                .await?;
            let fresh = self.auth.value().await?;
            resp = fresh.apply(make()).send().await?;
        }

        // A 503 may come after Zendesk processed a write, so only reads and deletes are
        // retried on it; a 429 was rejected before processing, so it is retried for all.
        let retry_503 = matches!(
            *request.method(),
            reqwest::Method::GET | reqwest::Method::DELETE
        );
        let mut attempt = 0;
        while attempt < MAX_RETRIES
            && (resp.status().as_u16() == 429 || (retry_503 && resp.status().as_u16() == 503))
        {
            let delay = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map_or(1 << attempt, |secs| secs.min(MAX_RETRY_AFTER_SECS));
            tracing::debug!(
                "{label} answered HTTP {}, retry {} of {MAX_RETRIES} in {delay}s",
                resp.status(),
                attempt + 1
            );
            let _ = resp.bytes().await;
            tokio::time::sleep(Duration::from_secs(delay)).await;
            resp = self.auth.value().await?.apply(make()).send().await?;
            attempt += 1;
        }
        ensure_success(resp, &label).await
    }

    pub(super) async fn get_url(&self, url: url::Url) -> Result<Value> {
        Ok(self
            .send(|| self.http.get(url.clone()))
            .await?
            .json()
            .await?)
    }

    pub(super) async fn api_get(
        &self,
        path: &str,
        params: &[(&str, &(dyn Display + Sync))],
    ) -> Result<Value> {
        self.get_url(self.url(path, params)?).await
    }

    pub(super) async fn api_post(&self, path: &str, body: &Value) -> Result<Value> {
        let url = self.url(path, &[])?;
        let resp = self.send(|| self.http.post(url.clone()).json(body)).await?;
        json_or_null(resp).await
    }

    /// POST raw `bytes` as the body with the given `Content-Type` (file uploads).
    pub(super) async fn api_post_bytes(
        &self,
        path: &str,
        params: &[(&str, &(dyn Display + Sync))],
        content_type: &str,
        bytes: Vec<u8>,
    ) -> Result<Value> {
        let url = self.url(path, params)?;
        let resp = self
            .send(|| {
                self.http
                    .post(url.clone())
                    .header(reqwest::header::CONTENT_TYPE, content_type)
                    .body(bytes.clone())
            })
            .await?;
        json_or_null(resp).await
    }

    pub(super) async fn api_put(&self, path: &str, body: &Value) -> Result<Value> {
        let url = self.url(path, &[])?;
        let resp = self.send(|| self.http.put(url.clone()).json(body)).await?;
        json_or_null(resp).await
    }

    /// DELETE with query `params` for endpoints that answer with a body.
    pub(super) async fn api_delete_json(
        &self,
        path: &str,
        params: &[(&str, &(dyn Display + Sync))],
    ) -> Result<Value> {
        let url = self.url(path, params)?;
        let resp = self.send(|| self.http.delete(url.clone())).await?;
        json_or_null(resp).await
    }

    pub(super) async fn api_delete(&self, path: &str) -> Result<()> {
        let url = self.url(path, &[])?;
        self.send(|| self.http.delete(url.clone())).await?;
        Ok(())
    }

    /// Whether `url` has the same scheme, host and port as `base_url`.
    fn is_account_url(&self, url: &url::Url) -> Result<bool> {
        let base = url::Url::parse(&self.base_url)?;
        Ok(url.scheme() == base.scheme()
            && url.host_str() == base.host_str()
            && url.port_or_known_default() == base.port_or_known_default())
    }

    /// Send an arbitrary request and return the JSON body (`Null` when the body is empty).
    ///
    /// `path` is relative to `/api/v2/` (a leading `/` or `api/v2/` is tolerated), or an
    /// absolute URL on this account, so a `next_page` link can be passed back in.
    pub async fn api(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<Value> {
        let url = if path.starts_with("http://") || path.starts_with("https://") {
            let mut url = url::Url::parse(path)?;
            if !self.is_account_url(&url)? {
                bail!(
                    "Refusing to send credentials to another host: {}",
                    url.host_str().unwrap_or("")
                );
            }
            if !query.is_empty() {
                url.query_pairs_mut()
                    .extend_pairs(query.iter().map(|(k, v)| (k.as_str(), v.as_str())));
            }
            url
        } else {
            let path = path.trim_start_matches('/');
            let path = match path.strip_prefix("api/v2") {
                Some("") => "",
                Some(rest) if rest.starts_with('/') => rest.trim_start_matches('/'),
                _ => path,
            };
            let params: Vec<(&str, &(dyn Display + Sync))> = query
                .iter()
                .map(|(k, v)| (k.as_str(), v as &(dyn Display + Sync)))
                .collect();
            self.url(path, &params)?
        };
        let resp = self
            .send(|| {
                let req = self.http.request(method.clone(), url.clone());
                match body {
                    Some(body) => req.json(body),
                    None => req,
                }
            })
            .await?;
        json_or_null(resp).await
    }

    /// Validate a `next_page` link: same scheme, host and port as `base_url`, and not a
    /// page already fetched. `seen` holds the URLs fetched so far; the link is added to it.
    /// Returns `None` (after a warning) once `MAX_PAGES` pages have been fetched.
    pub(super) fn next_page(
        &self,
        seen: &mut HashSet<String>,
        link: &str,
    ) -> Result<Option<url::Url>> {
        let next = url::Url::parse(link)?;
        if !self.is_account_url(&next)? {
            bail!(
                "Zendesk returned a next_page link on another host: {}",
                next.host_str().unwrap_or("")
            );
        }
        if seen.len() >= MAX_PAGES {
            tracing::warn!("Stopped following next_page after {MAX_PAGES} pages");
            return Ok(None);
        }
        if !seen.insert(next.to_string()) {
            bail!("Zendesk pagination returned a page it already returned");
        }
        Ok(Some(next))
    }

    /// Collect `key` from a listing, following the absolute `next_page` URLs until null.
    pub(super) async fn get_paged(&self, path: &str, key: &str) -> Result<Vec<Value>> {
        self.get_paged_with(path, &[], key).await
    }

    /// `get_paged` with query `params` on the first request; `next_page` links are
    /// followed unchanged.
    pub(super) async fn get_paged_with(
        &self,
        path: &str,
        params: &[(&str, &(dyn Display + Sync))],
        key: &str,
    ) -> Result<Vec<Value>> {
        let pages = self.get_pages(path, params).await?;
        Ok(pages
            .iter()
            .filter_map(|data| data.get(key).and_then(Value::as_array))
            .flatten()
            .cloned()
            .collect())
    }

    /// Every page of a listing as returned by Zendesk, so callers can read side-loads.
    pub(super) async fn get_pages(
        &self,
        path: &str,
        params: &[(&str, &(dyn Display + Sync))],
    ) -> Result<Vec<Value>> {
        let mut pages = Vec::new();
        let mut url = self.url(path, params)?;
        let mut seen = HashSet::from([url.to_string()]);
        loop {
            let data = self.get_url(url).await?;
            let link = data
                .get("next_page")
                .and_then(Value::as_str)
                .map(str::to_string);
            pages.push(data);
            let Some(link) = link else { break };
            match self.next_page(&mut seen, &link)? {
                Some(next) => url = next,
                None => break,
            }
        }
        Ok(pages)
    }

    /// One page of a cursor-paginated listing; `page_size` is capped at 100.
    pub(super) async fn get_cursor_page(
        &self,
        path: &str,
        params: &[(&str, &(dyn Display + Sync))],
        page_size: u64,
        after: Option<&str>,
    ) -> Result<Value> {
        let size = page_size.min(100);
        let mut all = params.to_vec();
        all.push(("page[size]", &size));
        if let Some(after) = &after {
            all.push(("page[after]", after));
        }
        self.api_get(path, &all).await
    }

    /// Collect `key` from a cursor-paginated listing, following `links.next` while
    /// `meta.has_more` is true, until `max_items` have been collected.
    pub(super) async fn get_cursor_paged(
        &self,
        path: &str,
        params: &[(&str, &(dyn Display + Sync))],
        key: &str,
        max_items: usize,
    ) -> Result<Vec<Value>> {
        let mut items = Vec::new();
        let mut first_params = params.to_vec();
        first_params.push(("page[size]", &100u64));
        let first = self.url(path, &first_params)?;
        let mut seen = HashSet::from([first.to_string()]);
        let mut data = self.get_url(first).await?;
        loop {
            if let Some(page) = data.get(key).and_then(Value::as_array) {
                items.extend(page.iter().cloned());
            }
            if items.len() >= max_items {
                items.truncate(max_items);
                break;
            }
            if data["meta"]["has_more"].as_bool() != Some(true) {
                break;
            }
            let Some(link) = data["links"]["next"].as_str() else {
                break;
            };
            match self.next_page(&mut seen, link)? {
                Some(next) => data = self.get_url(next).await?,
                None => break,
            }
        }
        Ok(items)
    }

    pub async fn get_job_status(&self, job_id: &str) -> Result<Value> {
        async {
            let data = self
                .api_get(&format!("job_statuses/{}.json", segment(job_id)?), &[])
                .await?;
            Ok(job_summary(object(&data, "job_status")?))
        }
        .await
        .map_err(ctx(format!("Failed to get job status {job_id}")))
    }

    /// Poll the job in a Zendesk response (`{"job_status": {...}}`) every
    /// `job_poll_interval` until it is `completed` or `failed`, or `timeout` elapses
    /// (retry waits inside a poll count against it). Returns the trimmed job status;
    /// `pending` is true if it is still running. A failed poll does not lose the accepted
    /// job: the last known status comes back with `pending: true` and a `poll_error`.
    pub(super) async fn wait_for_job(&self, job: &Value, timeout: Duration) -> Result<Value> {
        let mut status = job_summary(object(job, "job_status")?);
        let id = status["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Zendesk job status has no id"))?
            .to_string();
        let poll = async {
            while status["pending"] == true {
                tokio::time::sleep(self.job_poll_interval).await;
                match self.get_job_status(&id).await {
                    Ok(latest) => status = latest,
                    Err(e) => {
                        status["pending"] = json!(true);
                        status["poll_error"] = json!(format!("{e:#}"));
                        break;
                    }
                }
            }
        };
        // Err means the deadline passed mid-poll; `status` is still the last known one.
        let _ = tokio::time::timeout(timeout, poll).await;
        Ok(status)
    }
}

pub(super) const TICKET_SUMMARY_KEYS: [&str; 9] = [
    "id",
    "subject",
    "status",
    "priority",
    "requester_id",
    "assignee_id",
    "group_id",
    "created_at",
    "updated_at",
];

mod custom_objects;
mod help_center;
pub use help_center::ArticleSearch;
mod people;
mod ticket_ops;
mod tickets;
mod workflows;

#[cfg(test)]
pub(super) mod test_support {
    use super::*;
    use wiremock::{MockServer, ResponseTemplate};

    pub fn client(server: &MockServer) -> ZendeskClient {
        ZendeskClient::with_base_url(
            "acme",
            Auth::bearer("t"),
            reqwest::Client::new(),
            format!("{}/api/v2", server.uri()),
        )
        .with_job_poll_interval(Duration::from_millis(1))
    }

    pub fn offline_client() -> ZendeskClient {
        ZendeskClient::new("acme", Auth::bearer("t"), reqwest::Client::new())
    }

    pub fn json_page(items_key: &str, items: Value, next: Option<String>) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({ items_key: items, "next_page": next }))
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn invalid_token_without_oauth_is_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(401).set_body_json(json!({"error": "invalid_token"})),
            )
            .mount(&server)
            .await;
        let err = client(&server).get_ticket(1).await.unwrap_err().to_string();
        assert!(err.contains("Failed to get ticket 1: "), "{err}");
        assert!(err.contains("HTTP 401"), "{err}");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn forbidden_passes_body_through() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403).set_body_string("nope scope"))
            .mount(&server)
            .await;
        let err = client(&server).list_views().await.unwrap_err().to_string();
        assert!(
            err.contains("HTTP 403 Forbidden for GET /api/v2/views.json: nope scope"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn get_retries_429_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/me.json"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/me.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"user": {"id": 1}})))
            .mount(&server)
            .await;
        let user = client(&server).get_current_user().await.unwrap();
        assert_eq!(user["id"], 1);
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn post_and_put_429_are_retried() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .mount(&server)
            .await;
        let c = client(&server);
        let err = c.api_post("tickets.json", &json!({})).await.unwrap_err();
        assert!(err.to_string().contains("429"), "{err}");
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1 + MAX_RETRIES as usize
        );
        assert!(c.api_put("tickets/1.json", &json!({})).await.is_err());
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            2 * (1 + MAX_RETRIES as usize)
        );
    }

    #[tokio::test]
    async fn get_retries_503_but_put_does_not() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/me.json"))
            .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "0"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/me.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"user": {"id": 1}})))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "0"))
            .mount(&server)
            .await;
        let c = client(&server);
        assert_eq!(c.get_current_user().await.unwrap()["id"], 1);
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        let err = c.api_put("tickets/1.json", &json!({})).await.unwrap_err();
        assert!(err.to_string().contains("503"), "{err}");
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn api_posts_body_and_query_to_a_path_with_the_api_prefix() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/x.json"))
            .and(query_param("a", "b c"))
            .and(body_json(json!({"k": 1})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"ok": true})))
            .mount(&server)
            .await;
        let got = client(&server)
            .api(
                reqwest::Method::POST,
                "/api/v2/x.json",
                &[("a".into(), "b c".into())],
                Some(&json!({"k": 1})),
            )
            .await
            .unwrap();
        assert_eq!(got, json!({"ok": true}));
    }

    #[tokio::test]
    async fn api_accepts_an_absolute_url_on_the_account_host() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets.json"))
            .and(query_param("page", "2"))
            .and(query_param("x", "y"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"tickets": []})))
            .mount(&server)
            .await;
        let url = format!("{}/api/v2/tickets.json?page=2", server.uri());
        let got = client(&server)
            .api(
                reqwest::Method::GET,
                &url,
                &[("x".into(), "y".into())],
                None,
            )
            .await
            .unwrap();
        assert_eq!(got, json!({"tickets": []}));
    }

    #[tokio::test]
    async fn api_rejects_an_absolute_url_on_another_host_without_sending() {
        let server = MockServer::start().await;
        let err = client(&server)
            .api(
                reqwest::Method::GET,
                "https://evil.example/api/v2/x",
                &[],
                None,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("evil.example"), "{err}");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn api_returns_null_for_an_empty_body() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v2/tickets/1.json"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let got = client(&server)
            .api(reqwest::Method::DELETE, "tickets/1.json", &[], None)
            .await
            .unwrap();
        assert_eq!(got, Value::Null);
    }

    #[tokio::test]
    async fn next_page_on_another_host_is_rejected() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(json_page(
                "comments",
                json!([]),
                Some("https://evil.example/api/v2/next".into()),
            ))
            .mount(&server)
            .await;
        let err = client(&server)
            .get_ticket_comments(1, "asc")
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("next_page link on another host: evil.example"),
            "{err}"
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn next_page_cycle_terminates() {
        let server = MockServer::start().await;
        let first = format!("{}/api/v2/tickets/1/comments.json", server.uri());
        Mock::given(method("GET"))
            .and(query_param("p", "b"))
            .respond_with(json_page("comments", json!([]), Some(first.clone())))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(json_page(
                "comments",
                json!([]),
                Some(format!("{first}?p=b")),
            ))
            .mount(&server)
            .await;
        let err = client(&server)
            .get_ticket_comments(1, "asc")
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("Zendesk pagination returned a page it already returned"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn empty_put_and_post_bodies_are_null() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200).set_body_string(" \n"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let c = client(&server);
        assert_eq!(c.api_put("x.json", &json!({})).await.unwrap(), Value::Null);
        assert_eq!(c.api_post("x.json", &json!({})).await.unwrap(), Value::Null);
    }

    fn cursor_page(items: Value, next: Option<String>) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "things": items,
            "meta": { "has_more": next.is_some() },
            "links": { "next": next },
        }))
    }

    #[tokio::test]
    async fn cursor_pages_are_concatenated_following_links_next() {
        let server = MockServer::start().await;
        let next = format!(
            "{}/api/v2/things.json?page%5Bsize%5D=100&page%5Bafter%5D=abc",
            server.uri()
        );
        Mock::given(method("GET"))
            .and(query_param("page[after]", "abc"))
            .respond_with(cursor_page(json!([{"id": 2}]), None))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(query_param("page[size]", "100"))
            .respond_with(cursor_page(json!([{"id": 1}]), Some(next)))
            .mount(&server)
            .await;
        let items = client(&server)
            .get_cursor_paged("things.json", &[], "things", 1000)
            .await
            .unwrap();
        assert_eq!(items, vec![json!({"id": 1}), json!({"id": 2})]);
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn cursor_next_pointing_back_at_the_first_page_is_rejected() {
        let server = MockServer::start().await;
        let first = format!("{}/api/v2/things.json?page%5Bsize%5D=100", server.uri());
        Mock::given(method("GET"))
            .respond_with(cursor_page(json!([{"id": 1}]), Some(first)))
            .mount(&server)
            .await;
        let err = client(&server)
            .get_cursor_paged("things.json", &[], "things", 1000)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("already returned"), "{err}");
    }

    #[tokio::test]
    async fn cursor_next_on_another_host_is_rejected() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(cursor_page(
                json!([]),
                Some("https://evil.example/api/v2/things.json".into()),
            ))
            .mount(&server)
            .await;
        let err = client(&server)
            .get_cursor_paged("things.json", &[], "things", 1000)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("another host: evil.example"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn cursor_paging_stops_at_max_items() {
        let server = MockServer::start().await;
        let next = format!("{}/api/v2/things.json?page%5Bafter%5D=abc", server.uri());
        Mock::given(method("GET"))
            .respond_with(cursor_page(json!([{"id": 1}, {"id": 2}]), Some(next)))
            .mount(&server)
            .await;
        let items = client(&server)
            .get_cursor_paged("things.json", &[], "things", 1)
            .await
            .unwrap();
        assert_eq!(items, vec![json!({"id": 1})]);
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    fn job(status: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({"job_status": {
            "id": "j1", "status": status, "progress": 1, "total": 2,
            "results": [{"id": 5, "action": "merge", "status": "Merged", "success": true, "extra": 1}],
        }}))
    }

    #[tokio::test]
    async fn wait_for_job_polls_until_completed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/job_statuses/j1.json"))
            .respond_with(job("working"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/job_statuses/j1.json"))
            .respond_with(job("completed"))
            .mount(&server)
            .await;
        let first = json!({"job_status": {"id": "j1", "status": "queued"}});
        let done = client(&server)
            .wait_for_job(&first, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(done["status"], "completed");
        assert_eq!(done["pending"], false);
        assert_eq!(done["message"], Value::Null);
        assert_eq!(
            done["results"],
            json!([{"id": 5, "action": "merge", "status": "Merged", "success": true}])
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn wait_for_job_still_working_at_timeout_is_pending() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(job("working"))
            .mount(&server)
            .await;
        let first = json!({"job_status": {"id": "j1", "status": "queued"}});
        let out = client(&server)
            .wait_for_job(&first, Duration::from_millis(30))
            .await
            .unwrap();
        assert_eq!(out["status"], "working");
        assert_eq!(out["pending"], true);
    }

    #[tokio::test]
    async fn wait_for_job_poll_error_keeps_the_accepted_job_pending() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .mount(&server)
            .await;
        let first = json!({"job_status": {"id": "j1", "status": "queued", "total": 2}});
        let out = client(&server)
            .wait_for_job(&first, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(out["status"], "queued");
        assert_eq!(out["total"], 2);
        assert_eq!(out["pending"], true);
        assert!(out["poll_error"].as_str().unwrap().contains("429"), "{out}");
    }

    #[tokio::test]
    async fn wait_for_job_is_bounded_by_retry_after_waits() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "30"))
            .mount(&server)
            .await;
        let first = json!({"job_status": {"id": "j1", "status": "queued"}});
        let started = std::time::Instant::now();
        let out = client(&server)
            .wait_for_job(&first, Duration::from_millis(100))
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(out["pending"], true);
        assert_eq!(out["status"], "queued");
    }

    #[test]
    fn job_summary_counts_failed_results() {
        let out = job_summary(&json!({"id": "j", "status": "completed", "results": [
            {"id": 1, "success": true}, {"id": 2, "success": false}, {"id": 3, "success": false}
        ]}));
        assert_eq!(out["failed_count"], 2);
        assert_eq!(job_summary(&json!({"id": "j"}))["failed_count"], 0);
    }

    #[tokio::test]
    async fn job_ids_that_could_change_the_route_are_rejected() {
        let c = offline_client();
        assert!(c.get_job_status("..").await.is_err());
        assert!(c.get_job_status("").await.is_err());
    }

    #[test]
    fn segment_encodes_everything_but_unreserved_characters() {
        assert_eq!(segment("a-b_c.d~e1").unwrap(), "a-b_c.d~e1");
        assert_eq!(segment("a/b").unwrap(), "a%2Fb");
        assert_eq!(segment("a b?é").unwrap(), "a%20b%3F%C3%A9");
        for bad in ["", ".", ".."] {
            assert!(segment(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn markdown_html_documents_round_trip_unchanged() {
        let html = "<div>\n    <p>One</p>\n\n    <ul>\n      <li>a</li>\n    </ul>\n</div>\n";
        assert_eq!(markdown_to_html(html), html);
        let doc = "<!-- note -->\n<p>x</p>";
        assert_eq!(markdown_to_html(doc), doc);
    }

    #[test]
    fn markdown_that_only_looks_like_a_tag_is_still_converted() {
        assert_eq!(markdown_to_html("<3 you"), "<p>&lt;3 you</p>\n");
        assert!(markdown_to_html("<b>x</b> and **y**").contains("<strong>y</strong>"));
    }

    #[test]
    fn markdown_newline_becomes_br() {
        assert!(markdown_to_html("a\nb").contains("<br"));
    }

    #[test]
    fn markdown_bold_and_table() {
        assert!(markdown_to_html("**b**").contains("<strong>b</strong>"));
        assert!(markdown_to_html("|a|b|\n|-|-|\n|1|2|").contains("<table>"));
    }

    #[test]
    fn markdown_keeps_raw_html() {
        assert!(markdown_to_html("<b>x</b>").contains("<b>x</b>"));
    }

    #[test]
    fn markdown_empty_is_empty() {
        assert_eq!(markdown_to_html(""), "");
    }

    /// The one cross-module path: Zendesk rejects the stored token, the provider
    /// refreshes and persists the new pair, and the request is retried exactly once.
    #[tokio::test]
    async fn invalid_token_401_renews_once_and_retries() {
        use wiremock::matchers::{body_string_contains, header};

        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let settings = crate::config::OAuthSettings {
            subdomain: "acme".into(),
            client_id: "cid".into(),
            token_file: dir.path().join("tokens.json"),
            scopes: "tickets:read".into(),
            redirect_uri: "http://localhost:4567/callback".into(),
        };
        let store = crate::tokens::TokenStore::new(settings.token_file.clone());
        store
            .save(&crate::tokens::TokenSet {
                access_token: "old".into(),
                refresh_token: Some("r1".into()),
                expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                refresh_token_expires_at: None,
                subdomain: "acme".into(),
                client_id: "cid".into(),
                scope: None,
            })
            .unwrap();
        let provider = crate::oauth::OAuthProvider::with_store(
            settings,
            store.clone(),
            reqwest::Client::new(),
        )
        .with_token_endpoint(&format!("{}/oauth/tokens", server.uri()));

        Mock::given(method("GET"))
            .and(path("/api/v2/users/me.json"))
            .and(header("authorization", "Bearer old"))
            .respond_with(
                ResponseTemplate::new(401).set_body_json(json!({"error": "invalid_token"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth/tokens"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=r1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "new", "refresh_token": "r2",
                "expires_in": 1800, "refresh_token_expires_in": 7776000
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/me.json"))
            .and(header("authorization", "Bearer new"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"user": {"id": 1}})))
            .expect(1)
            .mount(&server)
            .await;

        let client = ZendeskClient::with_base_url(
            "acme",
            Auth::OAuth(std::sync::Arc::new(provider)),
            reqwest::Client::new(),
            format!("{}/api/v2", server.uri()),
        );
        let user = client.get_current_user().await.unwrap();
        assert_eq!(user["id"], 1);
        // The rotated pair reached disk before the retry was sent.
        let stored = store.load().unwrap();
        assert_eq!(stored.access_token, "new");
        assert_eq!(stored.refresh_token.as_deref(), Some("r2"));
    }
}
