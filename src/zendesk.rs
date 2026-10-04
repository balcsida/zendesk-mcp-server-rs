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

/// Retries of an idempotent request answered 429 or 503.
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
/// sanitize server-side.
pub fn markdown_to_html(text: &str) -> String {
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
}

#[derive(Clone)]
pub struct ZendeskClient {
    http: reqwest::Client,
    subdomain: String,
    /// `https://{subdomain}.zendesk.com/api/v2` in production; tests point it at a mock.
    base_url: String,
    auth: Auth,
}

/// Copy `keys` out of `obj`, defaulting to null (or `[]` for `array_keys`) when absent.
fn pick(obj: &Value, keys: &[&str], array_keys: &[&str]) -> Value {
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
fn pick_all(data: &Value, key: &str, keys: &[&str], array_keys: &[&str]) -> Value {
    let items = data
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[][..], |a| a);
    Value::Array(items.iter().map(|i| pick(i, keys, array_keys)).collect())
}

/// `[{id, value}]` from a ticket's raw `custom_fields`.
fn custom_fields(ticket: &Value) -> Value {
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
fn full_ticket(data: &Value) -> Result<Value> {
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
        ],
        &["tags"],
    );
    out["custom_fields"] = custom_fields(ticket);
    Ok(out)
}

fn object<'a>(data: &'a Value, key: &str) -> Result<&'a Value> {
    data.get(key)
        .ok_or_else(|| anyhow!("Zendesk response has no '{key}' object"))
}

/// `id -> name` from the `users` Zendesk side-loads when a request asks for `include=users`.
fn side_loaded_user_names(data: &Value) -> std::collections::HashMap<u64, String> {
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
fn with_user_names(
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

/// Prefix an error the way the Python server worded it.
fn ctx(prefix: impl Display) -> impl FnOnce(anyhow::Error) -> anyhow::Error {
    move |e| anyhow!("{prefix}: {e:#}")
}

fn magic_matches(content_type: &str, bytes: &[u8]) -> bool {
    match content_type {
        "image/jpeg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "image/png" => bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        _ => false,
    }
}

/// Fail on a non-success status with Zendesk's body (truncated), never the request headers.
async fn ensure_success(resp: reqwest::Response, label: &str) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.bytes().await.unwrap_or_default();
    Err(status_error(status, label, &body))
}

fn status_error(status: reqwest::StatusCode, label: &str, body: &[u8]) -> anyhow::Error {
    let text: String = String::from_utf8_lossy(body).chars().take(500).collect();
    anyhow!("Zendesk API error HTTP {status} for {label}: {text}")
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
        }
    }

    fn url(&self, path: &str, params: &[(&str, &(dyn Display + Sync))]) -> Result<url::Url> {
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
    async fn send(&self, make: impl Fn() -> reqwest::RequestBuilder) -> Result<reqwest::Response> {
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

        let idempotent = request.method() != reqwest::Method::POST;
        let mut attempt = 0;
        while idempotent && attempt < MAX_RETRIES && matches!(resp.status().as_u16(), 429 | 503) {
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

    async fn get_url(&self, url: url::Url) -> Result<Value> {
        Ok(self
            .send(|| self.http.get(url.clone()))
            .await?
            .json()
            .await?)
    }

    async fn api_get(&self, path: &str, params: &[(&str, &(dyn Display + Sync))]) -> Result<Value> {
        self.get_url(self.url(path, params)?).await
    }

    async fn api_post(&self, path: &str, body: &Value) -> Result<Value> {
        let url = self.url(path, &[])?;
        let resp = self.send(|| self.http.post(url.clone()).json(body)).await?;
        Ok(resp.json().await?)
    }

    async fn api_put(&self, path: &str, body: &Value) -> Result<Value> {
        let url = self.url(path, &[])?;
        let resp = self.send(|| self.http.put(url.clone()).json(body)).await?;
        Ok(resp.json().await?)
    }

    async fn api_delete(&self, path: &str) -> Result<()> {
        let url = self.url(path, &[])?;
        self.send(|| self.http.delete(url.clone())).await?;
        Ok(())
    }

    /// Validate a `next_page` link: same scheme, host and port as `base_url`, and not a
    /// page already fetched. `seen` holds the URLs fetched so far; the link is added to it.
    /// Returns `None` (after a warning) once `MAX_PAGES` pages have been fetched.
    fn next_page(&self, seen: &mut HashSet<String>, link: &str) -> Result<Option<url::Url>> {
        let next = url::Url::parse(link)?;
        let base = url::Url::parse(&self.base_url)?;
        if next.scheme() != base.scheme()
            || next.host_str() != base.host_str()
            || next.port_or_known_default() != base.port_or_known_default()
        {
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
    async fn get_paged(&self, path: &str, key: &str) -> Result<Vec<Value>> {
        let mut items = Vec::new();
        let mut url = self.url(path, &[])?;
        let mut seen = HashSet::from([url.to_string()]);
        loop {
            let data = self.get_url(url).await?;
            if let Some(page) = data.get(key).and_then(Value::as_array) {
                items.extend(page.iter().cloned());
            }
            let Some(link) = data.get("next_page").and_then(Value::as_str) else {
                break;
            };
            match self.next_page(&mut seen, link)? {
                Some(next) => url = next,
                None => break,
            }
        }
        Ok(items)
    }

    pub async fn get_ticket(&self, ticket_id: u64) -> Result<Value> {
        async {
            let data = self
                .api_get(
                    &format!("tickets/{ticket_id}.json"),
                    &[("include", &"users")],
                )
                .await?;
            let ticket = object(&data, "ticket")?;
            let mut out = pick(
                ticket,
                &[
                    "id",
                    "subject",
                    "description",
                    "status",
                    "priority",
                    "created_at",
                    "updated_at",
                    "requester_id",
                    "assignee_id",
                    "organization_id",
                ],
                &[],
            );
            out["custom_fields"] = custom_fields(ticket);
            out = with_user_names(out, ticket, &side_loaded_user_names(&data));
            Ok(out)
        }
        .await
        .map_err(ctx(format!("Failed to get ticket {ticket_id}")))
    }

    pub async fn get_ticket_comments(&self, ticket_id: u64) -> Result<Value> {
        async {
            let comments = self
                .get_paged(&format!("tickets/{ticket_id}/comments.json"), "comments")
                .await?;
            let out = comments.iter().map(|c| {
                let mut out = pick(
                    c,
                    &[
                        "id",
                        "author_id",
                        "body",
                        "html_body",
                        "public",
                        "created_at",
                    ],
                    &[],
                );
                out["attachments"] = pick_all(
                    c,
                    "attachments",
                    &["id", "file_name", "content_url", "content_type", "size"],
                    &[],
                );
                out
            });
            Ok(Value::Array(out.collect()))
        }
        .await
        .map_err(ctx(format!(
            "Failed to get comments for ticket {ticket_id}"
        )))
    }

    pub async fn get_ticket_attachment(&self, content_url: &str) -> Result<Attachment> {
        let (url, send_credentials) = self.validate_attachment_url(content_url)?;
        self.fetch_attachment(url, send_credentials).await
    }

    /// Returns the parsed URL and whether Zendesk credentials may be sent to its host.
    fn validate_attachment_url(&self, content_url: &str) -> Result<(url::Url, bool)> {
        let url = url::Url::parse(content_url)
            .map_err(|e| anyhow!("Attachment URL is not valid: {e}"))?;
        if url.scheme() != "https" {
            bail!("Attachment URL must use HTTPS.");
        }
        if !url.username().is_empty() || url.password().is_some() {
            bail!("Attachment URL must not contain credentials.");
        }
        let Some(host) = url.host_str().map(str::to_lowercase) else {
            bail!("Attachment URL must include a valid hostname.");
        };
        // Only the account subdomain gets credentials; Zendesk's CDN needs none.
        let send_credentials = host == format!("{}.zendesk.com", self.subdomain.to_lowercase());
        if !send_credentials && !host.ends_with(".zdusercontent.com") {
            bail!(
                "Attachment host is not trusted. Only Zendesk-hosted attachment URLs are allowed."
            );
        }
        Ok((url, send_credentials))
    }

    async fn fetch_attachment(&self, url: url::Url, send_credentials: bool) -> Result<Attachment> {
        // Zendesk attachment URLs redirect to the zdusercontent.com CDN. reqwest follows
        // redirects by default and strips Authorization and Cookie when the host changes,
        // which the CDN requires (it answers 403 to a request carrying credentials).
        let mut resp = if send_credentials {
            self.send(|| self.http.get(url.clone())).await?
        } else {
            let resp = self.http.get(url.clone()).send().await?;
            ensure_success(resp, &format!("GET {}", url.path())).await?
        };

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if !ALLOWED_IMAGE_TYPES.contains(&content_type.as_str()) {
            let mut allowed = ALLOWED_IMAGE_TYPES;
            allowed.sort_unstable();
            bail!("Attachment type '{content_type}' is not allowed. Supported types: {allowed:?}");
        }

        let mut content = Vec::new();
        while let Some(chunk) = resp.chunk().await? {
            if content.len() + chunk.len() > MAX_ATTACHMENT_BYTES {
                bail!(
                    "Attachment exceeds the {} MB size limit.",
                    MAX_ATTACHMENT_BYTES / (1024 * 1024)
                );
            }
            content.extend_from_slice(&chunk);
        }

        if !magic_matches(&content_type, &content) {
            bail!(
                "File header does not match declared content type '{content_type}'. The attachment may be spoofed."
            );
        }
        Ok(Attachment {
            data_base64: base64::engine::general_purpose::STANDARD.encode(&content),
            content_type,
        })
    }

    pub async fn post_comment(
        &self,
        ticket_id: u64,
        comment: &str,
        public: bool,
    ) -> Result<String> {
        let body = json!({"ticket": {"comment": {
            "html_body": markdown_to_html(comment),
            "public": public,
        }}});
        self.api_put(&format!("tickets/{ticket_id}.json"), &body)
            .await
            .map_err(ctx(format!("Failed to post comment on ticket {ticket_id}")))?;
        Ok(comment.to_string())
    }

    pub async fn get_tickets(
        &self,
        page: u64,
        per_page: u64,
        sort_by: &str,
        sort_order: &str,
    ) -> Result<Value> {
        async {
            let per_page = per_page.min(100);
            let data = self
                .api_get(
                    "tickets.json",
                    &[
                        ("page", &page),
                        ("per_page", &per_page),
                        ("sort_by", &sort_by),
                        ("sort_order", &sort_order),
                        ("include", &"users"),
                    ],
                )
                .await?;
            let names = side_loaded_user_names(&data);
            let tickets: Vec<Value> = data
                .get("tickets")
                .and_then(Value::as_array)
                .map_or(&[][..], |a| a)
                .iter()
                .map(|t| {
                    let mut out = pick(
                        t,
                        &[
                            "id",
                            "subject",
                            "status",
                            "priority",
                            "description",
                            "created_at",
                            "updated_at",
                            "requester_id",
                            "assignee_id",
                        ],
                        &[],
                    );
                    out["custom_fields"] =
                        t.get("custom_fields").cloned().unwrap_or_else(|| json!([]));
                    with_user_names(out, t, &names)
                })
                .collect();
            let has_next = !data["next_page"].is_null();
            let has_previous = !data["previous_page"].is_null() && page > 1;
            Ok(json!({
                "count": tickets.len(),
                "tickets": tickets,
                "page": page,
                "per_page": per_page,
                "sort_by": sort_by,
                "sort_order": sort_order,
                "has_more": has_next,
                "next_page": has_next.then_some(page + 1),
                "previous_page": has_previous.then(|| page - 1),
            }))
        }
        .await
        .map_err(ctx("Failed to get latest tickets"))
    }

    pub async fn get_all_articles(&self) -> Result<Value> {
        async {
            let mut kb = Map::new();
            for section in self
                .get_paged("help_center/sections.json", "sections")
                .await?
            {
                let id = &section["id"];
                // A section's articles live under its own locale; the locale-less path only
                // serves the default one, so non-English help centers came back empty
                // (upstream issue #10).
                let path = match section.get("locale").and_then(Value::as_str) {
                    Some(locale) => {
                        format!("help_center/{locale}/sections/{id}/articles.json")
                    }
                    None => format!("help_center/sections/{id}/articles.json"),
                };
                let articles = self.get_paged(&path, "articles").await?;
                let articles: Vec<Value> = articles
                    .iter()
                    .map(|a| {
                        let mut out = pick(a, &["id", "title", "body", "updated_at"], &[]);
                        out["url"] = a["html_url"].clone();
                        out
                    })
                    .collect();
                let name = section["name"].as_str().unwrap_or_default().to_string();
                kb.insert(
                    name,
                    json!({
                        "section_id": section["id"],
                        "description": section["description"],
                        "articles": articles,
                    }),
                );
            }
            Ok(Value::Object(kb))
        }
        .await
        .map_err(ctx("Failed to fetch knowledge base"))
    }

    pub async fn search_articles(
        &self,
        query: &str,
        locale: Option<&str>,
        per_page: u64,
        page: u64,
    ) -> Result<Value> {
        async {
            let per_page = per_page.min(100);
            let mut params: Vec<(&str, &(dyn Display + Sync))> =
                vec![("query", &query), ("per_page", &per_page), ("page", &page)];
            let locale = locale.filter(|l| !l.is_empty());
            if let Some(locale) = &locale {
                params.push(("locale", locale));
            }
            let data = self
                .api_get("help_center/articles/search.json", &params)
                .await?;
            let mut articles = pick_all(
                &data,
                "results",
                &[
                    "id",
                    "title",
                    "body",
                    "author_id",
                    "section_id",
                    "locale",
                    "html_url",
                    "created_at",
                    "updated_at",
                ],
                &[],
            );
            for (article, raw) in articles
                .as_array_mut()
                .into_iter()
                .flatten()
                .zip(data["results"].as_array().into_iter().flatten())
            {
                article["draft"] = raw.get("draft").cloned().unwrap_or(json!(false));
            }
            let count = articles.as_array().map_or(0, Vec::len);
            Ok(json!({
                "articles": articles,
                "query": query,
                "page": page,
                "per_page": per_page,
                "count": count,
                "total_count": data.get("count").cloned().unwrap_or(json!(count)),
                "next_page": data["next_page"],
                "previous_page": data["previous_page"],
            }))
        }
        .await
        .map_err(ctx("Failed to search articles"))
    }

    pub async fn get_article(&self, article_id: u64, locale: Option<&str>) -> Result<Value> {
        async {
            // Zendesk takes the locale as a path segment, not a query parameter.
            let path = match locale.filter(|l| !l.is_empty()) {
                Some(locale) => {
                    let locale: String = url::form_urlencoded::byte_serialize(locale.as_bytes())
                        .collect::<String>()
                        .replace('+', "%20");
                    format!("help_center/{locale}/articles/{article_id}.json")
                }
                None => format!("help_center/articles/{article_id}.json"),
            };
            let data = self.api_get(&path, &[]).await?;
            let article = data.get("article").cloned().unwrap_or(json!({}));
            let mut out = pick(
                &article,
                &[
                    "id",
                    "title",
                    "body",
                    "author_id",
                    "section_id",
                    "locale",
                    "source_locale",
                    "html_url",
                    "created_at",
                    "updated_at",
                    "edited_at",
                    "position",
                    "vote_sum",
                    "vote_count",
                    "label_names",
                ],
                &["label_names"],
            );
            for key in ["draft", "promoted"] {
                out[key] = article.get(key).cloned().unwrap_or(json!(false));
            }
            Ok(out)
        }
        .await
        .map_err(ctx(format!("Failed to get article {article_id}")))
    }

    pub async fn create_ticket(&self, ticket: CreateTicket) -> Result<Value> {
        async {
            let mut body = json!({
                "subject": ticket.subject,
                "comment": {"body": ticket.description},
            });
            let optional = [
                ("requester_id", ticket.requester_id.map(Value::from)),
                ("assignee_id", ticket.assignee_id.map(Value::from)),
                ("priority", ticket.priority.map(Value::from)),
                ("type", ticket.ticket_type.map(Value::from)),
                ("tags", ticket.tags.map(Value::from)),
                ("custom_fields", ticket.custom_fields.map(Value::from)),
            ];
            for (key, value) in optional {
                if let Some(value) = value {
                    body[key] = value;
                }
            }
            let data = self
                .api_post("tickets.json", &json!({"ticket": body}))
                .await?;
            full_ticket(&data)
        }
        .await
        .map_err(ctx("Failed to create ticket"))
    }

    /// `fields` are the ticket attributes to set (subject, status, priority, type,
    /// assignee_id, requester_id, tags, custom_fields, due_at, ...). Null values are skipped.
    pub async fn update_ticket(&self, ticket_id: u64, fields: Map<String, Value>) -> Result<Value> {
        async {
            let fields: Map<String, Value> =
                fields.into_iter().filter(|(_, v)| !v.is_null()).collect();
            let data = self
                .api_put(
                    &format!("tickets/{ticket_id}.json"),
                    &json!({"ticket": fields}),
                )
                .await?;
            full_ticket(&data)
        }
        .await
        .map_err(ctx(format!("Failed to update ticket {ticket_id}")))
    }

    pub async fn search(
        &self,
        query: &str,
        page: u64,
        per_page: u64,
        sort_by: &str,
        sort_order: &str,
    ) -> Result<Value> {
        async {
            let per_page = per_page.min(100);
            let data = self
                .api_get(
                    "search.json",
                    &[
                        ("query", &query),
                        ("page", &page),
                        ("per_page", &per_page),
                        ("sort_by", &sort_by),
                        ("sort_order", &sort_order),
                        ("include", &"tickets(users)"),
                    ],
                )
                .await?;
            let names = side_loaded_user_names(&data);
            let results = data
                .get("results")
                .and_then(Value::as_array)
                .map(|results| {
                    results
                        .iter()
                        .map(|r| match r.get("result_type").and_then(Value::as_str) {
                            Some("ticket") => with_user_names(r.clone(), r, &names),
                            _ => r.clone(),
                        })
                        .collect()
                })
                .unwrap_or_else(|| json!([]));
            Ok(json!({
                "results": results,
                "count": data.get("count").cloned().unwrap_or(json!(0)),
                "page": page,
                "per_page": per_page,
                "has_more": !data["next_page"].is_null(),
            }))
        }
        .await
        .map_err(ctx("Search failed"))
    }

    /// Every ticket matching `query`, instead of one page like `search`. Zendesk search
    /// returns at most 1,000 results (page 11 at 100 per page is a 422), so this stops after
    /// 10 pages and sets `truncated` when more remain. Non-ticket results are skipped, so
    /// `query` should be scoped to tickets (e.g. `type:ticket status:open`).
    pub async fn search_all_tickets(
        &self,
        query: &str,
        sort_by: &str,
        sort_order: &str,
    ) -> Result<Value> {
        async {
            let mut tickets = Vec::new();
            let mut truncated = false;
            for page in 1..=10u64 {
                let data = self
                    .api_get(
                        "search.json",
                        &[
                            ("query", &query),
                            ("page", &page),
                            ("per_page", &100u64),
                            ("sort_by", &sort_by),
                            ("sort_order", &sort_order),
                            ("include", &"tickets(users)"),
                        ],
                    )
                    .await?;
                let names = side_loaded_user_names(&data);
                tickets.extend(
                    data.get("results")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter(|r| r.get("result_type").and_then(Value::as_str) == Some("ticket"))
                        .map(|t| with_user_names(pick(t, &TICKET_SUMMARY_KEYS, &[]), t, &names)),
                );
                truncated = !data["next_page"].is_null();
                if !truncated {
                    break;
                }
            }
            Ok(json!({ "count": tickets.len(), "truncated": truncated, "tickets": tickets }))
        }
        .await
        .map_err(ctx("Search failed"))
    }

    pub async fn get_user(&self, user_id: u64) -> Result<Value> {
        async {
            let data = self.api_get(&format!("users/{user_id}.json"), &[]).await?;
            let user = object(&data, "user")?;
            let mut out = pick(
                user,
                &[
                    "id",
                    "name",
                    "email",
                    "role",
                    "phone",
                    "organization_id",
                    "time_zone",
                    "active",
                    "suspended",
                    "created_at",
                    "updated_at",
                    "tags",
                ],
                &["tags"],
            );
            out["photo_url"] = user["photo"]["content_url"].clone();
            Ok(out)
        }
        .await
        .map_err(ctx(format!("Failed to get user {user_id}")))
    }

    pub async fn get_current_user(&self) -> Result<Value> {
        async {
            let data = self.api_get("users/me.json", &[]).await?;
            Ok(pick(
                object(&data, "user")?,
                &[
                    "id",
                    "name",
                    "email",
                    "role",
                    "organization_id",
                    "time_zone",
                    "default_group_id",
                ],
                &[],
            ))
        }
        .await
        .map_err(ctx("Failed to get current user"))
    }

    pub async fn search_users(&self, query: &str) -> Result<Value> {
        async {
            let data = self
                .api_get("users/search.json", &[("query", &query)])
                .await?;
            Ok(pick_all(
                &data,
                "users",
                &["id", "name", "email", "role", "organization_id", "active"],
                &[],
            ))
        }
        .await
        .map_err(ctx("User search failed"))
    }

    pub async fn list_views(&self) -> Result<Value> {
        async {
            let data = self.api_get("views.json", &[]).await?;
            Ok(pick_all(
                &data,
                "views",
                &["id", "title", "active", "position"],
                &[],
            ))
        }
        .await
        .map_err(ctx("Failed to list views"))
    }

    pub async fn execute_view(&self, view_id: u64, page: u64, per_page: u64) -> Result<Value> {
        async {
            let per_page = per_page.min(100);
            let data = self
                .api_get(
                    &format!("views/{view_id}/tickets.json"),
                    &[("page", &page), ("per_page", &per_page)],
                )
                .await?;
            let tickets = pick_all(&data, "tickets", &TICKET_SUMMARY_KEYS, &[]);
            Ok(json!({
                "count": tickets.as_array().map_or(0, Vec::len),
                "tickets": tickets,
                "has_more": !data["next_page"].is_null(),
            }))
        }
        .await
        .map_err(ctx(format!("Failed to execute view {view_id}")))
    }

    pub async fn list_ticket_fields(&self) -> Result<Value> {
        async {
            let data = self.api_get("ticket_fields.json", &[]).await?;
            let mut fields = pick_all(
                &data,
                "ticket_fields",
                &["id", "title", "type", "active", "required"],
                &[],
            );
            for (field, raw) in fields
                .as_array_mut()
                .into_iter()
                .flatten()
                .zip(data["ticket_fields"].as_array().into_iter().flatten())
            {
                let options = pick_all(raw, "custom_field_options", &["name", "value"], &[]);
                field["custom_field_options"] = match options.as_array() {
                    Some(o) if !o.is_empty() => options,
                    _ => Value::Null,
                };
            }
            Ok(fields)
        }
        .await
        .map_err(ctx("Failed to list ticket fields"))
    }

    pub async fn get_organization(&self, organization_id: u64) -> Result<Value> {
        async {
            let data = self
                .api_get(&format!("organizations/{organization_id}.json"), &[])
                .await?;
            Ok(pick(
                object(&data, "organization")?,
                &[
                    "id",
                    "name",
                    "domain_names",
                    "details",
                    "notes",
                    "group_id",
                    "tags",
                    "created_at",
                    "updated_at",
                ],
                &["domain_names", "tags"],
            ))
        }
        .await
        .map_err(ctx(format!("Failed to get organization {organization_id}")))
    }

    pub async fn search_organizations(&self, query: &str) -> Result<Value> {
        async {
            let data = self
                .api_get("organizations/autocomplete.json", &[("name", &query)])
                .await?;
            Ok(pick_all(
                &data,
                "organizations",
                &["id", "name", "domain_names"],
                &["domain_names"],
            ))
        }
        .await
        .map_err(ctx("Organization search failed"))
    }

    pub async fn get_tickets_bulk(&self, ticket_ids: &[u64]) -> Result<Value> {
        async {
            let ids = ticket_ids
                .iter()
                .take(100)
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let data = self
                .api_get("tickets/show_many.json", &[("ids", &ids)])
                .await?;
            Ok(pick_all(&data, "tickets", &TICKET_SUMMARY_KEYS, &[]))
        }
        .await
        .map_err(ctx("Bulk ticket fetch failed"))
    }

    pub async fn list_groups(&self) -> Result<Value> {
        async {
            let data = self.api_get("groups/assignable.json", &[]).await?;
            Ok(pick_all(
                &data,
                "groups",
                &["id", "name", "description"],
                &[],
            ))
        }
        .await
        .map_err(ctx("Failed to list groups"))
    }

    pub async fn merge_tickets(
        &self,
        target_id: u64,
        source_ids: &[u64],
        target_comment: &str,
        source_comment: &str,
    ) -> Result<Value> {
        let body = json!({
            "ids": source_ids,
            "target_comment": target_comment,
            "source_comment": source_comment,
        });
        self.api_post(&format!("tickets/{target_id}/merge.json"), &body)
            .await
            .map_err(ctx(format!("Failed to merge tickets into {target_id}")))
    }

    pub async fn list_macros(&self, active_only: bool) -> Result<Value> {
        async {
            let path = if active_only {
                "macros/active.json"
            } else {
                "macros.json"
            };
            let data = self.api_get(path, &[]).await?;
            Ok(pick_all(
                &data,
                "macros",
                &["id", "title", "description", "active"],
                &[],
            ))
        }
        .await
        .map_err(ctx("Failed to list macros"))
    }

    pub async fn apply_macro(&self, ticket_id: u64, macro_id: u64) -> Result<Value> {
        async {
            let data = self
                .api_get(
                    &format!("tickets/{ticket_id}/macros/{macro_id}/apply.json"),
                    &[],
                )
                .await?;
            let result = &data["result"];
            Ok(json!({
                "ticket_changes": result.get("ticket").cloned().unwrap_or(json!({})),
                "comment": result["comment"],
            }))
        }
        .await
        .map_err(ctx(format!(
            "Failed to apply macro {macro_id} to ticket {ticket_id}"
        )))
    }

    /// `role` must be one of `requested`, `assigned`, `ccd`.
    pub async fn get_user_tickets(
        &self,
        user_id: u64,
        role: &str,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        async {
            if !["requested", "assigned", "ccd"].contains(&role) {
                bail!("Invalid role '{role}'. Allowed: [\"assigned\", \"ccd\", \"requested\"]");
            }
            let per_page = per_page.min(100);
            let data = self
                .api_get(
                    &format!("users/{user_id}/tickets/{role}.json"),
                    &[("page", &page), ("per_page", &per_page)],
                )
                .await?;
            Ok(json!({
                "tickets": pick_all(
                    &data,
                    "tickets",
                    &["id", "subject", "status", "priority", "created_at", "updated_at"],
                    &[],
                ),
                "has_more": !data["next_page"].is_null(),
            }))
        }
        .await
        .map_err(ctx(format!("Failed to get tickets for user {user_id}")))
    }

    pub async fn list_ticket_forms(&self) -> Result<Value> {
        async {
            let data = self.api_get("ticket_forms.json", &[]).await?;
            Ok(pick_all(
                &data,
                "ticket_forms",
                &[
                    "id",
                    "name",
                    "display_name",
                    "active",
                    "default",
                    "ticket_field_ids",
                ],
                &["ticket_field_ids"],
            ))
        }
        .await
        .map_err(ctx("Failed to list ticket forms"))
    }

    pub async fn delete_ticket(&self, ticket_id: u64) -> Result<()> {
        self.api_delete(&format!("tickets/{ticket_id}.json"))
            .await
            .map_err(ctx(format!("Failed to delete ticket {ticket_id}")))
    }

    pub async fn get_ticket_metrics(&self, ticket_id: u64) -> Result<Value> {
        async {
            let data = self
                .api_get(&format!("tickets/{ticket_id}/metrics.json"), &[])
                .await?;
            Ok(object(&data, "ticket_metric")?.clone())
        }
        .await
        .map_err(ctx(format!("Failed to get metrics for ticket {ticket_id}")))
    }

    /// `events` are trimmed to the fields relevant across event types: `type`, `body`,
    /// `html_body`, `public`, `value`, `previous_value` and `field_name`. Absent fields are
    /// omitted rather than padded with nulls, since event shape varies by `type`.
    pub async fn get_ticket_audits(&self, ticket_id: u64) -> Result<Value> {
        async {
            let audits = self
                .get_paged(&format!("tickets/{ticket_id}/audits.json"), "audits")
                .await?;
            let audits: Vec<Value> = audits
                .iter()
                .map(|a| {
                    let mut out = pick(a, &["id", "ticket_id", "author_id", "created_at"], &[]);
                    let events = a
                        .get("events")
                        .and_then(Value::as_array)
                        .map_or(&[][..], |e| e);
                    out["events"] = Value::Array(
                        events
                            .iter()
                            .map(|e| {
                                let mut event = Map::new();
                                for key in [
                                    "type",
                                    "body",
                                    "html_body",
                                    "public",
                                    "value",
                                    "previous_value",
                                    "field_name",
                                ] {
                                    if let Some(v) = e.get(key)
                                        && !v.is_null()
                                    {
                                        event.insert(key.to_string(), v.clone());
                                    }
                                }
                                Value::Object(event)
                            })
                            .collect(),
                    );
                    out
                })
                .collect();
            Ok(json!({ "count": audits.len(), "audits": audits }))
        }
        .await
        .map_err(ctx(format!("Failed to get audits for ticket {ticket_id}")))
    }

    /// Incident tickets linked to a problem ticket (`GET /tickets/{ticket_id}/incidents.json`).
    pub async fn get_linked_incidents(&self, ticket_id: u64) -> Result<Value> {
        async {
            let incidents = self
                .get_paged(&format!("tickets/{ticket_id}/incidents.json"), "tickets")
                .await?;
            let incidents = incidents
                .iter()
                .map(|t| pick(t, &TICKET_SUMMARY_KEYS, &[]))
                .collect::<Vec<_>>();
            Ok(json!({ "count": incidents.len(), "incidents": incidents }))
        }
        .await
        .map_err(ctx(format!(
            "Failed to get linked incidents for ticket {ticket_id}"
        )))
    }

    pub async fn get_sla_breaches(&self, days_back: u64, metric: Option<&str>) -> Result<Value> {
        async {
            let days = i64::try_from(days_back.min(MAX_DAYS_BACK))?;
            let start = chrono::Duration::try_days(days)
                .and_then(|d| chrono::Utc::now().checked_sub_signed(d))
                .ok_or_else(|| anyhow!("days_back {days_back} is out of range"))?
                .timestamp();
            let mut url = self.url(
                "incremental/ticket_metric_events.json",
                &[("start_time", &start)],
            )?;
            let mut breaches = Vec::new();
            let mut seen = HashSet::from([url.to_string()]);
            loop {
                let data = self.get_url(url.clone()).await?;
                for event in data["ticket_metric_events"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    if event["type"] == "breach" && metric.is_none_or(|m| event["metric"] == m) {
                        let mut breach = pick(event, &["ticket_id", "metric", "time"], &[]);
                        breach["instance_id"] = event["instance_id"].clone();
                        breaches.push(breach);
                    }
                }
                let Some(link) = data["next_page"].as_str() else {
                    break;
                };
                if data["end_of_stream"] == true {
                    break;
                }
                match self.next_page(&mut seen, link)? {
                    Some(next) => url = next,
                    None => break,
                }
            }

            let mut tickets = std::collections::HashSet::new();
            let mut by_metric = Map::new();
            for breach in &breaches {
                tickets.insert(breach["ticket_id"].to_string());
                let name = breach["metric"].as_str().unwrap_or_default().to_string();
                let count = by_metric.get(&name).and_then(Value::as_u64).unwrap_or(0);
                by_metric.insert(name, json!(count + 1));
            }
            Ok(json!({
                "total_breaches": breaches.len(),
                "unique_tickets": tickets.len(),
                "breaches": breaches,
                "by_metric": by_metric,
                "days_back": days_back,
            }))
        }
        .await
        .map_err(ctx("Failed to get SLA breaches"))
    }

    pub async fn get_sla_policies(&self) -> Result<Value> {
        async {
            let policies = self.get_paged("slas/policies.json", "sla_policies").await?;
            Ok(Value::Array(policies))
        }
        .await
        .map_err(ctx("Failed to get SLA policies"))
    }
}

const TICKET_SUMMARY_KEYS: [&str; 9] = [
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

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    fn client(server: &MockServer) -> ZendeskClient {
        ZendeskClient::with_base_url(
            "acme",
            Auth::bearer("t"),
            reqwest::Client::new(),
            format!("{}/api/v2", server.uri()),
        )
    }

    fn offline_client() -> ZendeskClient {
        ZendeskClient::new("acme", Auth::bearer("t"), reqwest::Client::new())
    }

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];

    fn image(content_type: &str, body: Vec<u8>) -> ResponseTemplate {
        ResponseTemplate::new(200)
            .insert_header("content-type", content_type)
            .set_body_bytes(body)
    }

    async fn serve_image(server: &MockServer, response: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path("/img"))
            .respond_with(response)
            .mount(server)
            .await;
    }

    fn img_url(server: &MockServer) -> url::Url {
        url::Url::parse(&format!("{}/img", server.uri())).unwrap()
    }

    #[tokio::test]
    async fn get_ticket_shape_with_custom_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/7.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ticket": {
                "id": 7, "subject": "s", "description": "d", "status": "open",
                "priority": null, "created_at": "c", "updated_at": "u",
                "requester_id": 1, "assignee_id": null, "organization_id": 3,
                "custom_fields": [{"id": 9, "value": "x"}], "extra": true
            }})))
            .mount(&server)
            .await;
        let out = client(&server).get_ticket(7).await.unwrap();
        assert_eq!(
            out,
            json!({
                "id": 7, "subject": "s", "description": "d", "status": "open",
                "priority": null, "created_at": "c", "updated_at": "u",
                "requester_id": 1, "assignee_id": null, "organization_id": 3,
                "custom_fields": [{"id": 9, "value": "x"}]
            })
        );
    }

    #[tokio::test]
    async fn get_ticket_adds_requester_and_assignee_names_from_side_loaded_users() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/7.json"))
            .and(query_param("include", "users"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ticket": {
                    "id": 7, "subject": "s", "description": "d", "status": "open",
                    "created_at": "c", "updated_at": "u",
                    "requester_id": 1, "assignee_id": 2
                },
                "users": [
                    {"id": 1, "name": "Alice"},
                    {"id": 2, "name": "Bob"}
                ]
            })))
            .mount(&server)
            .await;
        let out = client(&server).get_ticket(7).await.unwrap();
        assert_eq!(out["requester_name"], "Alice");
        assert_eq!(out["assignee_name"], "Bob");
    }

    #[tokio::test]
    async fn get_ticket_omits_names_when_zendesk_did_not_side_load_users() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/7.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ticket": {
                "id": 7, "requester_id": 1, "assignee_id": null
            }})))
            .mount(&server)
            .await;
        let out = client(&server).get_ticket(7).await.unwrap();
        assert!(out.get("requester_name").is_none());
        assert!(out.get("assignee_name").is_none());
    }

    #[tokio::test]
    async fn get_ticket_comments_follows_next_page() {
        let server = MockServer::start().await;
        let next = format!("{}/api/v2/tickets/5/comments.json?page=2", server.uri());
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/5/comments.json"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "comments": [{"id": 2, "author_id": 1, "body": "b2", "html_body": "h2",
                    "public": false, "created_at": "c2"}],
                "next_page": null
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/5/comments.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "comments": [{"id": 1, "author_id": 1, "body": "b1", "html_body": "h1",
                    "public": true, "created_at": "c1",
                    "attachments": [{"id": 4, "file_name": "a.png",
                        "content_url": "u", "content_type": "image/png", "size": 3}]}],
                "next_page": next
            })))
            .mount(&server)
            .await;
        let out = client(&server).get_ticket_comments(5).await.unwrap();
        let comments = out.as_array().unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0]["attachments"][0]["file_name"], "a.png");
        assert_eq!(comments[1]["attachments"], json!([]));
        assert_eq!(comments[1]["public"], false);
    }

    #[tokio::test]
    async fn get_tickets_caps_per_page_and_reports_paging() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets.json"))
            .and(query_param("per_page", "100"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "tickets": [{"id": 1, "subject": "s"}],
                "next_page": "https://x/next", "previous_page": "https://x/prev"
            })))
            .mount(&server)
            .await;
        let out = client(&server)
            .get_tickets(2, 500, "created_at", "desc")
            .await
            .unwrap();
        assert_eq!(out["per_page"], 100);
        assert_eq!(out["count"], 1);
        assert_eq!(out["has_more"], true);
        assert_eq!(out["next_page"], 3);
        assert_eq!(out["previous_page"], 1);
        assert_eq!(out["tickets"][0]["custom_fields"], json!([]));
    }

    #[tokio::test]
    async fn post_comment_sends_html_body_and_privacy() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v2/tickets/3.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        let out = client(&server)
            .post_comment(3, "a\nb", false)
            .await
            .unwrap();
        assert_eq!(out, "a\nb");
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        let comment = &body["ticket"]["comment"];
        assert!(comment["html_body"].as_str().unwrap().contains("<br"));
        assert_eq!(comment["public"], false);
    }

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

    #[test]
    fn attachment_url_rejects_http() {
        let err = offline_client()
            .validate_attachment_url("http://acme.zendesk.com/a.png")
            .unwrap_err();
        assert_eq!(err.to_string(), "Attachment URL must use HTTPS.");
    }

    #[test]
    fn attachment_url_rejects_credentials() {
        let err = offline_client()
            .validate_attachment_url("https://u:p@acme.zendesk.com/a.png")
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "Attachment URL must not contain credentials."
        );
    }

    #[test]
    fn attachment_url_rejects_untrusted_host() {
        for url in [
            "https://evil.com/a.png",
            "https://acme.zendesk.com.evil.com/a.png",
        ] {
            let err = offline_client().validate_attachment_url(url).unwrap_err();
            assert!(
                err.to_string()
                    .starts_with("Attachment host is not trusted"),
                "{url}"
            );
        }
    }

    #[test]
    fn attachment_url_credentials_only_for_account_host() {
        let c = offline_client();
        let (url, creds) = c
            .validate_attachment_url("https://ACME.Zendesk.com/a.png")
            .unwrap();
        assert!(creds);
        assert_eq!(url.host_str(), Some("acme.zendesk.com"));
        let (_, creds) = c
            .validate_attachment_url("https://X.ZDUSERCONTENT.com/a.png")
            .unwrap();
        assert!(!creds);
    }

    #[tokio::test]
    async fn fetch_attachment_rejects_html() {
        let server = MockServer::start().await;
        serve_image(&server, image("text/html", b"<html>".to_vec())).await;
        let err = client(&server)
            .fetch_attachment(img_url(&server), false)
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("Attachment type 'text/html' is not allowed"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn fetch_attachment_rejects_spoofed_header() {
        let server = MockServer::start().await;
        serve_image(&server, image("image/png", vec![0xFF, 0xD8, 0xFF, 0xE0])).await;
        let err = client(&server)
            .fetch_attachment(img_url(&server), false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("may be spoofed"), "{err}");
    }

    #[tokio::test]
    async fn fetch_attachment_rejects_oversized_body() {
        let server = MockServer::start().await;
        let mut body = PNG.to_vec();
        body.resize(MAX_ATTACHMENT_BYTES + 1, 0);
        serve_image(&server, image("image/png", body)).await;
        let err = client(&server)
            .fetch_attachment(img_url(&server), false)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "Attachment exceeds the 10 MB size limit.");
    }

    #[tokio::test]
    async fn fetch_attachment_returns_base64() {
        let server = MockServer::start().await;
        serve_image(&server, image("Image/PNG; charset=binary", PNG.to_vec())).await;
        let out = client(&server)
            .fetch_attachment(img_url(&server), false)
            .await
            .unwrap();
        assert_eq!(out.content_type, "image/png");
        assert_eq!(
            out.data_base64,
            base64::engine::general_purpose::STANDARD.encode(PNG)
        );
    }

    #[tokio::test]
    async fn fetch_attachment_sends_credentials_only_when_asked() {
        let server = MockServer::start().await;
        serve_image(&server, image("image/png", PNG.to_vec())).await;
        let c = client(&server);
        c.fetch_attachment(img_url(&server), true).await.unwrap();
        c.fetch_attachment(img_url(&server), false).await.unwrap();
        let requests = server.received_requests().await.unwrap();
        let has_auth = |r: &Request| r.headers.contains_key("authorization");
        assert!(has_auth(&requests[0]));
        assert!(!has_auth(&requests[1]));
    }

    #[tokio::test]
    async fn get_user_tickets_rejects_unknown_role() {
        let err = offline_client()
            .get_user_tickets(1, "foo", 1, 25)
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("Invalid role 'foo'. Allowed: [\"assigned\", \"ccd\", \"requested\"]"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn sla_breaches_filter_metric_and_sum_pages() {
        let server = MockServer::start().await;
        let next = format!(
            "{}/api/v2/incremental/ticket_metric_events.json?cursor=2",
            server.uri()
        );
        Mock::given(method("GET"))
            .and(path("/api/v2/incremental/ticket_metric_events.json"))
            .and(query_param("cursor", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ticket_metric_events": [
                    {"type": "breach", "metric": "reply_time", "ticket_id": 1, "time": "t3", "instance_id": 0},
                    {"type": "breach", "metric": "reply_time", "ticket_id": 2, "time": "t4", "instance_id": 0}
                ],
                "end_of_stream": true, "next_page": next
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/incremental/ticket_metric_events.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ticket_metric_events": [
                    {"type": "breach", "metric": "reply_time", "ticket_id": 1, "time": "t1", "instance_id": 1},
                    {"type": "breach", "metric": "agent_work_time", "ticket_id": 1, "time": "t2", "instance_id": 1},
                    {"type": "activate", "metric": "reply_time", "ticket_id": 3, "time": "t0", "instance_id": 1}
                ],
                "end_of_stream": false, "next_page": next
            })))
            .mount(&server)
            .await;
        let out = client(&server)
            .get_sla_breaches(7, Some("reply_time"))
            .await
            .unwrap();
        assert_eq!(out["total_breaches"], 3);
        assert_eq!(out["unique_tickets"], 2);
        assert_eq!(out["by_metric"], json!({"reply_time": 3}));
        assert_eq!(out["days_back"], 7);
    }

    fn json_page(items_key: &str, items: Value, next: Option<String>) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({ items_key: items, "next_page": next }))
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
    async fn post_429_is_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .mount(&server)
            .await;
        let err = client(&server)
            .api_post("tickets.json", &json!({}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("429"), "{err}");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
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
        let err = client(&server).get_ticket_comments(1).await.unwrap_err();
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
        let err = client(&server).get_ticket_comments(1).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("Zendesk pagination returned a page it already returned"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn get_sla_policies_merges_pages() {
        let server = MockServer::start().await;
        let next = format!("{}/api/v2/slas/policies.json?page=2", server.uri());
        Mock::given(method("GET"))
            .and(query_param("page", "2"))
            .respond_with(json_page("sla_policies", json!([{"id": 2}]), None))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(json_page("sla_policies", json!([{"id": 1}]), Some(next)))
            .mount(&server)
            .await;
        let out = client(&server).get_sla_policies().await.unwrap();
        assert_eq!(out, json!([{"id": 1}, {"id": 2}]));
    }

    #[tokio::test]
    async fn get_ticket_audits_trims_event_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/9/audits.json"))
            .respond_with(json_page(
                "audits",
                json!([{
                    "id": 1, "ticket_id": 9, "author_id": 4, "created_at": "c",
                    "extra": true,
                    "events": [
                        {"type": "Comment", "body": "hi", "html_body": "<p>hi</p>",
                            "public": true, "extra": "drop me"},
                        {"type": "Change", "field_name": "status",
                            "value": "open", "previous_value": "new"}
                    ]
                }]),
                None,
            ))
            .mount(&server)
            .await;
        let out = client(&server).get_ticket_audits(9).await.unwrap();
        assert_eq!(out["count"], 1);
        let audit = &out["audits"][0];
        assert_eq!(audit["id"], 1);
        assert_eq!(audit["author_id"], 4);
        assert!(audit.get("extra").is_none());
        assert_eq!(
            audit["events"][0],
            json!({"type": "Comment", "body": "hi", "html_body": "<p>hi</p>", "public": true})
        );
        assert_eq!(
            audit["events"][1],
            json!({"type": "Change", "field_name": "status", "value": "open", "previous_value": "new"})
        );
    }

    #[tokio::test]
    async fn get_linked_incidents_follows_pagination() {
        let server = MockServer::start().await;
        let next = format!("{}/api/v2/tickets/3/incidents.json?page=2", server.uri());
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/3/incidents.json"))
            .and(query_param("page", "2"))
            .respond_with(json_page(
                "tickets",
                json!([{"id": 11, "subject": "s2", "status": "open"}]),
                None,
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/3/incidents.json"))
            .respond_with(json_page(
                "tickets",
                json!([{"id": 10, "subject": "s1", "status": "new"}]),
                Some(next),
            ))
            .mount(&server)
            .await;
        let out = client(&server).get_linked_incidents(3).await.unwrap();
        assert_eq!(out["count"], 2);
        assert_eq!(out["incidents"][0]["id"], 10);
        assert_eq!(out["incidents"][1]["id"], 11);
    }

    #[tokio::test]
    async fn search_all_tickets_stops_at_the_search_result_limit() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/search.json"))
            .and(query_param("include", "tickets(users)"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    {"result_type": "ticket", "id": 1, "requester_id": 5, "extra": true},
                    {"result_type": "user", "id": 5}
                ],
                "users": [{"id": 5, "name": "Ann"}],
                "next_page": "more"
            })))
            .expect(10)
            .mount(&server)
            .await;
        let out = client(&server)
            .search_all_tickets("type:ticket", "created_at", "desc")
            .await
            .unwrap();
        assert_eq!(out["count"], 10);
        assert_eq!(out["truncated"], true);
        assert_eq!(out["tickets"][0]["requester_name"], "Ann");
        assert!(out["tickets"][0].get("extra").is_none());
    }

    #[tokio::test]
    async fn sla_breaches_huge_days_back_does_not_panic() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ticket_metric_events": [], "end_of_stream": true, "next_page": null
            })))
            .mount(&server)
            .await;
        let out = client(&server)
            .get_sla_breaches(u64::MAX, None)
            .await
            .unwrap();
        assert_eq!(out["total_breaches"], 0);
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
