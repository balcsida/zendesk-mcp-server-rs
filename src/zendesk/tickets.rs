use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};

use super::*;

impl ZendeskClient {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zendesk::test_support::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

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
}
