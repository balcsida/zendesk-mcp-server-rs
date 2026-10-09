//! Tickets: reading, searching, creating and updating, comments and attachments.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value, json};

use super::*;

/// Tickets of a list response with `requester_name`/`assignee_name` from the side-loaded users.
fn ticket_rows(data: &Value) -> Vec<Value> {
    let names = side_loaded_user_names(data);
    data.get("tickets")
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
            out["custom_fields"] = t.get("custom_fields").cloned().unwrap_or_else(|| json!([]));
            with_user_names(out, t, &names)
        })
        .collect()
}

impl ZendeskClient {
    pub async fn get_ticket(&self, ticket_id: u64) -> Result<Value> {
        async {
            let data = self
                .api_get(
                    &format!("tickets/{ticket_id}.json"),
                    &[("include", &"users,groups,comment_count")],
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
                    "type",
                    "tags",
                    "group_id",
                    "due_at",
                    "ticket_form_id",
                    "brand_id",
                    "custom_status_id",
                    "problem_id",
                    "has_incidents",
                    "is_public",
                    "external_id",
                    "followup_ids",
                    "email_cc_ids",
                    "follower_ids",
                    "comment_count",
                    "satisfaction_rating",
                ],
                &["tags"],
            );
            out["channel"] = ticket["via"]["channel"].clone();
            out["custom_fields"] = custom_fields(ticket);
            out = with_user_names(out, ticket, &side_loaded_user_names(&data));
            let group_name = ticket
                .get("group_id")
                .and_then(Value::as_u64)
                .and_then(|id| {
                    data.get("groups")?
                        .as_array()?
                        .iter()
                        .find(|g| g.get("id").and_then(Value::as_u64) == Some(id))?
                        .get("name")
                        .cloned()
                });
            out["group_name"] = group_name.unwrap_or(Value::Null);
            anyhow::Ok(out)
        }
        .await
        .with_context(|| format!("Failed to get ticket {ticket_id}"))
    }

    /// `sort_order` must be `asc` or `desc`.
    pub async fn get_ticket_comments(&self, ticket_id: u64, sort_order: &str) -> Result<Value> {
        async {
            if !["asc", "desc"].contains(&sort_order) {
                bail!("Invalid sort_order '{sort_order}'. Allowed: [\"asc\", \"desc\"]");
            }
            let pages = self
                .get_pages(
                    &format!("tickets/{ticket_id}/comments.json"),
                    &[("include", &"users"), ("sort_order", &sort_order)],
                )
                .await?;
            let names: std::collections::HashMap<u64, String> =
                pages.iter().flat_map(side_loaded_user_names).collect();
            let comments = pages
                .iter()
                .filter_map(|p| p.get("comments").and_then(Value::as_array))
                .flatten();
            let out = comments.map(|c| {
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
                if let Some(name) = c
                    .get("author_id")
                    .and_then(Value::as_u64)
                    .and_then(|id| names.get(&id))
                {
                    out["author_name"] = json!(name);
                }
                out
            });
            anyhow::Ok(Value::Array(out.collect()))
        }
        .await
        .with_context(|| format!("Failed to get comments for ticket {ticket_id}"))
    }

    /// Fetches an attachment as an allowed image type, at most [`MAX_ATTACHMENT_BYTES`].
    ///
    /// # Errors
    ///
    /// Fails when `content_url` is not an https URL without credentials on the account's
    /// `/attachments/` route or a `*.zdusercontent.com` host, or the response is not an
    /// allowed image type or exceeds the size cap.
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
        // Credentials go only to the attachment route, not to any other API endpoint or port.
        if send_credentials && (url.port().is_some() || !url.path().starts_with("/attachments/")) {
            bail!(
                "content_url must be an attachment URL on {}.zendesk.com (/attachments/...) or a *.zdusercontent.com URL",
                self.subdomain
            );
        }
        Ok((url, send_credentials))
    }

    async fn fetch_attachment(&self, url: url::Url, send_credentials: bool) -> Result<Attachment> {
        // Zendesk attachment URLs redirect to the zdusercontent.com CDN. `redirect_policy`
        // follows redirects only over https to Zendesk hosts, and reqwest strips
        // Authorization and Cookie when the host changes, which the CDN requires (it
        // answers 403 to a request carrying credentials).
        let mut resp = if send_credentials {
            self.send(|| self.http.get(url.clone())).await?
        } else {
            let resp = self
                .http
                .get(url.clone())
                .send()
                .await
                .map_err(reqwest::Error::without_url)?;
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
        while let Some(chunk) = resp.chunk().await.map_err(reqwest::Error::without_url)? {
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

    /// `status`, when given, is set in the same update: new, open, pending, hold or solved.
    /// Returns `{id, public, status?}`; `id` is the new comment's, null if Zendesk's
    /// audit did not list it.
    pub async fn post_comment(
        &self,
        ticket_id: u64,
        comment: &str,
        public: bool,
        status: Option<&str>,
        upload_tokens: &[String],
    ) -> Result<Value> {
        async {
            let mut body = json!({"ticket": {"comment": {
                "html_body": markdown_to_html(comment),
                "public": public,
            }}});
            if let Some(status) = status {
                body["ticket"]["status"] = json!(status);
            }
            if !upload_tokens.is_empty() {
                body["ticket"]["comment"]["uploads"] = json!(upload_tokens);
            }
            let data = self
                .api_put(&format!("tickets/{ticket_id}.json"), &body)
                .await?;
            let id = data["audit"]["events"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|e| e["type"] == "Comment")
                .map_or(Value::Null, |e| e["id"].clone());
            let mut out = json!({ "id": id, "public": public });
            if let Some(status) = status {
                out["status"] = json!(status);
            }
            anyhow::Ok(out)
        }
        .await
        .with_context(|| format!("Failed to post comment on ticket {ticket_id}"))
    }

    /// Uploads a file for attaching to a comment; returns the token (valid for 60 minutes)
    /// to pass as `upload_tokens`.
    ///
    /// # Errors
    ///
    /// Fails without a request when `filename` or `content_type` is empty, or the data is
    /// not valid base64, is empty, or exceeds [`MAX_ATTACHMENT_BYTES`] once decoded.
    pub async fn upload_attachment(
        &self,
        filename: &str,
        content_type: &str,
        data_base64: &str,
    ) -> Result<Value> {
        async {
            if filename.is_empty() || content_type.is_empty() {
                bail!("filename and content_type must not be empty");
            }
            if data_base64.len() > MAX_ATTACHMENT_BYTES / 3 * 4 + 4 {
                bail!(
                    "File exceeds the {} MB size limit.",
                    MAX_ATTACHMENT_BYTES / (1024 * 1024)
                );
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data_base64.trim())
                .map_err(|e| anyhow!("data_base64 is not valid base64: {e}"))?;
            if bytes.is_empty() {
                bail!("data_base64 decodes to an empty file");
            }
            if bytes.len() > MAX_ATTACHMENT_BYTES {
                bail!(
                    "File exceeds the {} MB size limit.",
                    MAX_ATTACHMENT_BYTES / (1024 * 1024)
                );
            }
            let data = self
                .api_post_bytes(
                    "uploads.json",
                    &[("filename", &filename)],
                    content_type,
                    bytes,
                )
                .await?;
            let upload = object(&data, "upload")?;
            anyhow::Ok(json!({
                "token": upload["token"],
                "attachment": pick(
                    &upload["attachment"],
                    &["id", "file_name", "content_type", "size", "content_url"],
                    &[],
                ),
            }))
        }
        .await
        .context("Failed to upload attachment")
    }

    pub async fn get_tickets(
        &self,
        page: u64,
        per_page: u64,
        sort_by: &str,
        sort_order: &str,
    ) -> Result<Value> {
        async {
            let per_page = per_page.min(MAX_PAGE_SIZE);
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
            let tickets = ticket_rows(&data);
            let has_next = !data["next_page"].is_null();
            let has_previous = !data["previous_page"].is_null() && page > 1;
            anyhow::Ok(json!({
                "count": tickets.len(),
                "tickets": tickets,
                "page": page,
                "per_page": per_page,
                "sort_by": sort_by,
                "sort_order": sort_order,
                "has_more": has_next,
                "next_page": has_next.then(|| page.saturating_add(1)),
                "previous_page": has_previous.then(|| page - 1),
            }))
        }
        .await
        .context("Failed to get latest tickets")
    }

    /// Creates a ticket and returns it in the shape `update_ticket` uses.
    ///
    /// # Errors
    ///
    /// Fails without a request when both `requester` and `requester_id` are given.
    pub async fn create_ticket(&self, ticket: CreateTicket) -> Result<Value> {
        async {
            if ticket.requester.is_some() && ticket.requester_id.is_some() {
                bail!("Give either requester or requester_id, not both");
            }
            let mut comment = json!({"body": ticket.description});
            if let Some(tokens) = ticket.upload_tokens.filter(|t| !t.is_empty()) {
                comment["uploads"] = json!(tokens);
            }
            if let Some(public) = ticket.public {
                comment["public"] = json!(public);
            }
            let mut body = json!({
                "subject": ticket.subject,
                "comment": comment,
            });
            let email_ccs = ticket.email_ccs.map(|emails| {
                emails
                    .into_iter()
                    .map(|e| json!({"user_email": e, "action": "put"}))
                    .collect::<Vec<_>>()
            });
            let optional = [
                ("requester", ticket.requester),
                ("group_id", ticket.group_id.map(Value::from)),
                ("ticket_form_id", ticket.ticket_form_id.map(Value::from)),
                ("brand_id", ticket.brand_id.map(Value::from)),
                ("problem_id", ticket.problem_id.map(Value::from)),
                (
                    "via_followup_source_id",
                    ticket.via_followup_source_id.map(Value::from),
                ),
                ("custom_status_id", ticket.custom_status_id.map(Value::from)),
                ("due_at", ticket.due_at.map(Value::from)),
                ("external_id", ticket.external_id.map(Value::from)),
                ("email_ccs", email_ccs.map(Value::from)),
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
        .context("Failed to create ticket")
    }

    /// `fields` are the ticket attributes to set (subject, status, priority, type,
    /// assignee_id, requester_id, tags, custom_fields, due_at, ...). Null values are skipped.
    /// `safe_update: true` requires `updated_stamp`; Zendesk answers 409 on a collision.
    ///
    /// # Errors
    ///
    /// Fails without a request when no non-null field is given, an `email_ccs` entry has
    /// neither `user_id` nor `user_email`, or `safe_update` is set without `updated_stamp`.
    pub async fn update_ticket(&self, ticket_id: u64, fields: Map<String, Value>) -> Result<Value> {
        async {
            let fields: Map<String, Value> =
                fields.into_iter().filter(|(_, v)| !v.is_null()).collect();
            if fields.is_empty() {
                bail!("Give at least one field to update");
            }
            if let Some(ccs) = fields.get("email_ccs").and_then(Value::as_array)
                && ccs
                    .iter()
                    .any(|cc| cc.get("user_id").is_none() && cc.get("user_email").is_none())
            {
                bail!("Each email_ccs entry needs a user_id or a user_email");
            }
            if fields.get("safe_update") == Some(&json!(true))
                && !fields.get("updated_stamp").is_some_and(Value::is_string)
            {
                bail!("safe_update requires updated_stamp (the ticket's current updated_at)");
            }
            let data = self
                .api_put(
                    &format!("tickets/{ticket_id}.json"),
                    &json!({"ticket": fields}),
                )
                .await?;
            full_ticket(&data)
        }
        .await
        .with_context(|| format!("Failed to update ticket {ticket_id}"))
    }

    pub async fn search(
        &self,
        query: &str,
        page: u64,
        per_page: u64,
        sort_by: Option<&str>,
        sort_order: &str,
    ) -> Result<Value> {
        async {
            let per_page = per_page.min(MAX_PAGE_SIZE);
            let mut params: Vec<(&str, &(dyn Display + Sync))> = vec![
                ("query", &query),
                ("page", &page),
                ("per_page", &per_page),
                ("sort_order", &sort_order),
                ("include", &"tickets(users)"),
            ];
            // Zendesk sorts by relevance when sort_by is absent.
            if let Some(sort_by) = &sort_by {
                params.push(("sort_by", sort_by));
            }
            let data = self.api_get("search.json", &params).await?;
            let names = side_loaded_user_names(&data);
            let results = data.get("results").and_then(Value::as_array).map_or_else(
                || json!([]),
                |results| {
                    results
                        .iter()
                        .map(|r| match r.get("result_type").and_then(Value::as_str) {
                            Some("ticket") => with_user_names(r.clone(), r, &names),
                            _ => r.clone(),
                        })
                        .collect()
                },
            );
            anyhow::Ok(json!({
                "results": results,
                "count": data.get("count").cloned().unwrap_or_else(|| json!(0)),
                "page": page,
                "per_page": per_page,
                "has_more": !data["next_page"].is_null(),
            }))
        }
        .await
        .context("Search failed")
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
                            ("per_page", &MAX_PAGE_SIZE),
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
            anyhow::Ok(
                json!({ "count": tickets.len(), "truncated": truncated, "tickets": tickets }),
            )
        }
        .await
        .context("Search failed")
    }

    /// Fetches in chunks of 100 ids, the `show_many` limit.
    pub async fn get_tickets_bulk(&self, ticket_ids: &[u64]) -> Result<Value> {
        async {
            if ticket_ids.len() > 1000 {
                bail!("Give at most 1000 ids per call");
            }
            let mut tickets = Vec::new();
            for chunk in ticket_ids.chunks(100) {
                let ids = chunk
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                let data = self
                    .api_get("tickets/show_many.json", &[("ids", &ids)])
                    .await?;
                if let Value::Array(page) = pick_all(&data, "tickets", &TICKET_SUMMARY_KEYS, &[]) {
                    tickets.extend(page);
                }
            }
            anyhow::Ok(Value::Array(tickets))
        }
        .await
        .context("Bulk ticket fetch failed")
    }

    /// Merges and waits up to 20 seconds for Zendesk's background job; returns its
    /// trimmed status (`pending` is true if the merge is still running).
    ///
    /// # Errors
    ///
    /// Fails without a request when `source_ids` is empty, has more than 100 entries or a
    /// duplicate, or contains `target_id`.
    pub async fn merge_tickets(
        &self,
        target_id: u64,
        source_ids: &[u64],
        target_comment: &str,
        source_comment: &str,
        target_comment_is_public: Option<bool>,
        source_comment_is_public: Option<bool>,
    ) -> Result<Value> {
        async {
            if source_ids.is_empty() {
                bail!("Give at least one source ticket to merge");
            }
            if source_ids.len() > 100 {
                bail!("Give at most 100 source tickets per merge");
            }
            if source_ids.contains(&target_id) {
                bail!("A source ticket cannot be the target ticket");
            }
            if source_ids.iter().collect::<HashSet<_>>().len() != source_ids.len() {
                bail!("source_ids must not contain duplicates");
            }
            let mut body = json!({
                "ids": source_ids,
                "target_comment": target_comment,
                "source_comment": source_comment,
            });
            if let Some(public) = target_comment_is_public {
                body["target_comment_is_public"] = json!(public);
            }
            if let Some(public) = source_comment_is_public {
                body["source_comment_is_public"] = json!(public);
            }
            let job = self
                .api_post(&format!("tickets/{target_id}/merge.json"), &body)
                .await?;
            self.wait_for_job(&job, Duration::from_secs(20)).await
        }
        .await
        .with_context(|| format!("Failed to merge tickets into {target_id}"))
    }

    /// `role` must be one of `requested`, `assigned`, `ccd`, `followed`.
    pub async fn get_user_tickets(
        &self,
        user_id: u64,
        role: &str,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        async {
            if !["requested", "assigned", "ccd", "followed"].contains(&role) {
                bail!(
                    "Invalid role '{role}'. Allowed: [\"assigned\", \"ccd\", \"followed\", \"requested\"]"
                );
            }
            let per_page = per_page.min(MAX_PAGE_SIZE);
            let data = self
                .api_get(
                    &format!("users/{user_id}/tickets/{role}.json"),
                    &[("page", &page), ("per_page", &per_page)],
                )
                .await?;
            let tickets = pick_all(
                &data,
                "tickets",
                &["id", "subject", "status", "priority", "created_at", "updated_at"],
                &[],
            );
            anyhow::Ok(json!({
                "count": tickets.as_array().map_or(0, Vec::len),
                "tickets": tickets,
                "has_more": !data["next_page"].is_null(),
            }))
        }
        .await
        .with_context(|| format!("Failed to get tickets for user {user_id}"))
    }

    pub async fn delete_ticket(&self, ticket_id: u64) -> Result<()> {
        self.api_delete(&format!("tickets/{ticket_id}.json"))
            .await
            .with_context(|| format!("Failed to delete ticket {ticket_id}"))
    }

    pub async fn get_ticket_metrics(&self, ticket_id: u64) -> Result<Value> {
        async {
            let data = self
                .api_get(&format!("tickets/{ticket_id}/metrics.json"), &[])
                .await?;
            anyhow::Ok(object(&data, "ticket_metric")?.clone())
        }
        .await
        .with_context(|| format!("Failed to get metrics for ticket {ticket_id}"))
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
            anyhow::Ok(json!({ "count": audits.len(), "audits": audits }))
        }
        .await
        .with_context(|| format!("Failed to get audits for ticket {ticket_id}"))
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
            anyhow::Ok(json!({ "count": incidents.len(), "incidents": incidents }))
        }
        .await
        .with_context(|| format!("Failed to get linked incidents for ticket {ticket_id}"))
    }

    /// Size a result set with `search/count`. Without a query this counts `type:ticket`:
    /// `tickets/count` leaves out archived tickets and reports exactly 100,000 while it
    /// refreshes a larger count.
    pub async fn count_tickets(&self, query: Option<&str>) -> Result<Value> {
        async {
            let query = query.filter(|q| !q.is_empty()).unwrap_or("type:ticket");
            let data = self
                .api_get("search/count.json", &[("query", &query)])
                .await?;
            anyhow::Ok(json!({ "count": data["count"], "query": query }))
        }
        .await
        .context("Failed to count tickets")
    }

    /// Followers and email CCs of a ticket; both need the CCs and followers feature.
    pub async fn get_ticket_collaborators(&self, ticket_id: u64) -> Result<Value> {
        async {
            let keys = ["id", "name", "email", "role"];
            let followers = self
                .api_get(&format!("tickets/{ticket_id}/followers.json"), &[])
                .await?;
            // Without the CCs feature the email_ccs endpoint answers 403 or 404; Zendesk
            // then keeps the CCs under the older collaborators endpoint. Any other error
            // is a real failure.
            let (email_ccs, source) = match self
                .api_get(&format!("tickets/{ticket_id}/email_ccs.json"), &[])
                .await
            {
                Ok(data) => (data, "email_ccs"),
                Err(err)
                    if err
                        .downcast_ref::<ApiError>()
                        .is_some_and(|e| matches!(e.status.as_u16(), 403 | 404)) =>
                {
                    tracing::debug!("email_ccs unavailable, using collaborators: {err:#}");
                    (
                        self.api_get(&format!("tickets/{ticket_id}/collaborators.json"), &[])
                            .await?,
                        "collaborators",
                    )
                }
                Err(err) => return Err(err),
            };
            anyhow::Ok(json!({
                "followers": pick_all(&followers, "users", &keys, &[]),
                "email_ccs": pick_all(&email_ccs, "users", &keys, &[]),
                "source": source,
            }))
        }
        .await
        .with_context(|| format!("Failed to get collaborators for ticket {ticket_id}"))
    }

    /// Problem tickets whose subject contains `text`, or the 100 most recently updated.
    pub async fn search_problem_tickets(&self, text: Option<&str>) -> Result<Value> {
        async {
            let data = match text.filter(|t| !t.is_empty()) {
                Some(text) => {
                    self.api_post("problems/autocomplete.json", &json!({ "text": text }))
                        .await?
                }
                None => {
                    self.api_get("problems.json", &[("per_page", &MAX_PAGE_SIZE)])
                        .await?
                }
            };
            let tickets = pick_all(&data, "tickets", &TICKET_SUMMARY_KEYS, &[]);
            anyhow::Ok(
                json!({ "count": tickets.as_array().map_or(0, Vec::len), "tickets": tickets }),
            )
        }
        .await
        .context("Failed to search problem tickets")
    }

    pub async fn get_organization_tickets(
        &self,
        organization_id: u64,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        async {
            let per_page = per_page.min(MAX_PAGE_SIZE);
            let data = self
                .api_get(
                    &format!("organizations/{organization_id}/tickets.json"),
                    &[
                        ("page", &page),
                        ("per_page", &per_page),
                        ("include", &"users"),
                    ],
                )
                .await?;
            let tickets = ticket_rows(&data);
            anyhow::Ok(json!({
                "count": tickets.len(),
                "tickets": tickets,
                "page": page,
                "per_page": per_page,
                "has_more": !data["next_page"].is_null(),
            }))
        }
        .await
        .with_context(|| format!("Failed to get tickets for organization {organization_id}"))
    }

    /// Adds then removes specific tags; returns the ticket's tags after the last call.
    ///
    /// # Errors
    ///
    /// Fails without a request when both lists are empty or a tag to remove contains a comma.
    pub async fn update_ticket_tags(
        &self,
        ticket_id: u64,
        add: &[String],
        remove: &[String],
    ) -> Result<Value> {
        async {
            if add.is_empty() && remove.is_empty() {
                bail!("Give at least one tag to add or remove");
            }
            if remove.iter().any(|t| t.contains(',')) {
                bail!("Tags to remove must not contain commas");
            }
            let path = format!("tickets/{ticket_id}/tags.json");
            let mut data = Value::Null;
            if !add.is_empty() {
                data = self.api_put(&path, &json!({ "tags": add })).await?;
            }
            if !remove.is_empty() {
                data = self
                    .api_delete_json(&path, &[("tags", &remove.join(","))])
                    .await
                    .map_err(|e| {
                        if add.is_empty() {
                            e
                        } else {
                            anyhow!("tags added but removing failed: {e:#}")
                        }
                    })?;
            }
            anyhow::Ok(json!({ "tags": data.get("tags").cloned().unwrap_or(json!([])) }))
        }
        .await
        .with_context(|| format!("Failed to update tags on ticket {ticket_id}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_support::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    #[tokio::test]
    async fn count_tickets_uses_search_count_and_defaults_to_all_tickets() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/search/count.json"))
            .and(query_param("query", "type:ticket status:open"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"count": 6})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/search/count.json"))
            .and(query_param("query", "type:ticket"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"count": 102})))
            .mount(&server)
            .await;
        let c = client(&server);
        assert_eq!(
            c.count_tickets(Some("type:ticket status:open"))
                .await
                .unwrap(),
            json!({"count": 6, "query": "type:ticket status:open"})
        );
        assert_eq!(
            c.count_tickets(None).await.unwrap(),
            json!({"count": 102, "query": "type:ticket"})
        );
    }

    #[tokio::test]
    async fn collaborators_combine_followers_and_email_ccs() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/4/followers.json"))
            .respond_with(json_page(
                "users",
                json!([{"id": 1, "name": "A", "email": "a@x.com", "role": "agent", "extra": 1}]),
                None,
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/4/email_ccs.json"))
            .respond_with(json_page("users", json!([{"id": 2, "name": "B"}]), None))
            .mount(&server)
            .await;
        let out = client(&server).get_ticket_collaborators(4).await.unwrap();
        assert_eq!(
            out,
            json!({
                "followers": [{"id": 1, "name": "A", "email": "a@x.com", "role": "agent"}],
                "email_ccs": [{"id": 2, "name": "B", "email": null, "role": null}],
                "source": "email_ccs",
            })
        );
    }

    #[tokio::test]
    async fn collaborators_fall_back_to_the_collaborators_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/4/followers.json"))
            .respond_with(json_page("users", json!([]), None))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/4/email_ccs.json"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/4/collaborators.json"))
            .respond_with(json_page("users", json!([{"id": 7, "name": "C"}]), None))
            .mount(&server)
            .await;
        let out = client(&server).get_ticket_collaborators(4).await.unwrap();
        assert_eq!(out["source"], "collaborators");
        assert_eq!(out["email_ccs"][0]["id"], 7);
    }

    #[tokio::test]
    async fn collaborators_do_not_fall_back_on_a_server_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/4/followers.json"))
            .respond_with(json_page("users", json!([]), None))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/4/email_ccs.json"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/4/collaborators.json"))
            .respond_with(json_page("users", json!([]), None))
            .expect(0)
            .mount(&server)
            .await;
        let err = client(&server)
            .get_ticket_collaborators(4)
            .await
            .unwrap_err();
        let api = err.downcast_ref::<ApiError>().expect("ApiError");
        assert_eq!(api.status.as_u16(), 500);
    }

    #[tokio::test]
    async fn problem_search_posts_text_and_listing_gets_one_page_of_100() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/problems/autocomplete.json"))
            .respond_with(json_page(
                "tickets",
                json!([{"id": 33, "subject": "fire"}]),
                None,
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/problems.json"))
            .and(query_param("per_page", "100"))
            .respond_with(json_page("tickets", json!([{"id": 1}, {"id": 2}]), None))
            .mount(&server)
            .await;
        let c = client(&server);
        let found = c.search_problem_tickets(Some("fire")).await.unwrap();
        assert_eq!(found["count"], 1);
        assert_eq!(found["tickets"][0]["id"], 33);
        assert_eq!(c.search_problem_tickets(None).await.unwrap()["count"], 2);
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body, json!({"text": "fire"}));
    }

    #[tokio::test]
    async fn organization_tickets_have_names_and_paging() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/organizations/8/tickets.json"))
            .and(query_param("include", "users"))
            .and(query_param("per_page", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "tickets": [{"id": 1, "subject": "s", "requester_id": 5}],
                "users": [{"id": 5, "name": "Req"}],
                "next_page": "https://x/next",
            })))
            .mount(&server)
            .await;
        let out = client(&server)
            .get_organization_tickets(8, 1, 500)
            .await
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["tickets"][0]["requester_name"], "Req");
        assert_eq!(out["per_page"], 100);
        assert_eq!(out["has_more"], true);
    }

    #[tokio::test]
    async fn tag_update_puts_added_tags_then_deletes_removed_ones_by_query() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v2/tickets/3/tags.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"tags": ["a", "b"]})))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/v2/tickets/3/tags.json"))
            .and(query_param("tags", "b,c"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"tags": ["a"]})))
            .mount(&server)
            .await;
        let c = client(&server);
        let out = c
            .update_ticket_tags(3, &["a".into()], &["b".into(), "c".into()])
            .await
            .unwrap();
        assert_eq!(out, json!({"tags": ["a"]}));
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body, json!({"tags": ["a"]}));
        let err = c.update_ticket_tags(3, &[], &[]).await.unwrap_err();
        assert!(format!("{err:#}").contains("at least one tag"), "{err}");
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn bulk_fetch_splits_150_ids_into_two_requests() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/show_many.json"))
            .respond_with(json_page("tickets", json!([{"id": 1}]), None))
            .mount(&server)
            .await;
        let ids: Vec<u64> = (1..=150).collect();
        let out = client(&server).get_tickets_bulk(&ids).await.unwrap();
        assert_eq!(out.as_array().unwrap().len(), 2);
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        let counts: Vec<usize> = requests
            .iter()
            .map(|r| {
                let (_, ids) = r.url.query_pairs().find(|(k, _)| k == "ids").unwrap();
                ids.split(',').count()
            })
            .collect();
        assert_eq!(counts, [100, 50]);
    }

    #[tokio::test]
    async fn merge_waits_for_the_job_and_sends_privacy_flags() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/tickets/9/merge.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"job_status": {"id": "j1", "status": "queued"}})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/job_statuses/j1.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"job_status": {"id": "j1", "status": "completed", "progress": 1, "total": 1}}),
            ))
            .mount(&server)
            .await;
        let out = client(&server)
            .merge_tickets(9, &[1], "t", "s", Some(true), None)
            .await
            .unwrap();
        assert_eq!(out["status"], "completed");
        assert_eq!(out["pending"], false);
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["target_comment_is_public"], true);
        assert!(body.get("source_comment_is_public").is_none());
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
                "type": null, "tags": [], "group_id": null, "due_at": null,
                "ticket_form_id": null, "brand_id": null, "custom_status_id": null,
                "problem_id": null, "has_incidents": null, "is_public": null,
                "external_id": null, "followup_ids": null, "email_cc_ids": null,
                "follower_ids": null, "comment_count": null, "channel": null,
                "satisfaction_rating": null, "group_name": null,
                "custom_fields": [{"id": 9, "value": "x"}]
            })
        );
    }

    #[tokio::test]
    async fn get_ticket_adds_requester_and_assignee_names_from_side_loaded_users() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/7.json"))
            .and(query_param("include", "users,groups,comment_count"))
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
    async fn get_ticket_adds_group_name_channel_and_extra_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/7.json"))
            .and(query_param("include", "users,groups,comment_count"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ticket": {
                    "id": 7, "group_id": 4, "type": "incident", "tags": ["a", "b"],
                    "via": {"channel": "email"}, "comment_count": 3, "is_public": true,
                    "email_cc_ids": [8], "satisfaction_rating": {"score": "good"}
                },
                "groups": [{"id": 3, "name": "Other"}, {"id": 4, "name": "Support"}]
            })))
            .mount(&server)
            .await;
        let out = client(&server).get_ticket(7).await.unwrap();
        assert_eq!(out["group_name"], "Support");
        assert_eq!(out["channel"], "email");
        assert_eq!(out["tags"], json!(["a", "b"]));
        assert_eq!(out["comment_count"], 3);
        assert_eq!(out["email_cc_ids"], json!([8]));
        assert_eq!(out["satisfaction_rating"], json!({"score": "good"}));
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
        let out = client(&server)
            .get_ticket_comments(5, "desc")
            .await
            .unwrap();
        let comments = out.as_array().unwrap();
        assert_eq!(comments.len(), 2);
        let requests = server.received_requests().await.unwrap();
        let first: std::collections::HashMap<_, _> = requests[0].url.query_pairs().collect();
        assert_eq!(first["include"], "users");
        assert_eq!(first["sort_order"], "desc");
        assert_eq!(requests[1].url.query(), Some("page=2"));
        assert_eq!(comments[0]["attachments"][0]["file_name"], "a.png");
        assert_eq!(comments[1]["attachments"], json!([]));
        assert_eq!(comments[1]["public"], false);
    }

    #[tokio::test]
    async fn get_ticket_comments_adds_author_names() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/5/comments.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "comments": [{"id": 1, "author_id": 1}, {"id": 2, "author_id": 9}],
                "users": [{"id": 1, "name": "Ann"}],
                "next_page": null
            })))
            .mount(&server)
            .await;
        let c = client(&server);
        let out = c.get_ticket_comments(5, "asc").await.unwrap();
        assert_eq!(out[0]["author_name"], "Ann");
        assert!(out[1].get("author_name").is_none());
    }

    #[tokio::test]
    async fn get_ticket_comments_rejects_a_bad_sort_order_without_a_request() {
        let server = MockServer::start().await;
        let err = client(&server)
            .get_ticket_comments(5, "up")
            .await
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("Invalid sort_order 'up'"),
            "{err}"
        );
        assert!(server.received_requests().await.unwrap().is_empty());
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
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"audit": {"events": [
                    {"id": 76, "type": "Change"},
                    {"id": 77, "type": "Comment"},
                ]}})),
            )
            .mount(&server)
            .await;
        let out = client(&server)
            .post_comment(3, "a\nb", false, Some("pending"), &["tok".into()])
            .await
            .unwrap();
        assert_eq!(out, json!({"id": 77, "public": false, "status": "pending"}));
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        let comment = &body["ticket"]["comment"];
        assert!(comment["html_body"].as_str().unwrap().contains("<br"));
        assert_eq!(comment["public"], false);
        assert_eq!(comment["uploads"], json!(["tok"]));
        assert_eq!(body["ticket"]["status"], "pending");
    }

    #[tokio::test]
    async fn upload_posts_raw_bytes_with_content_type_and_filename() {
        use wiremock::matchers::header;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/uploads.json"))
            .and(query_param("filename", "crash report.png"))
            .and(header("content-type", "image/png"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"upload": {
                "token": "tok1",
                "attachment": {"id": 7, "file_name": "crash report.png", "content_type": "image/png",
                               "size": 10, "content_url": "https://x/a", "deleted": false},
            }})))
            .expect(1)
            .mount(&server)
            .await;
        let c = client(&server);
        let data = base64::engine::general_purpose::STANDARD.encode(PNG);
        let out = c
            .upload_attachment("crash report.png", "image/png", &data)
            .await
            .unwrap();
        assert_eq!(out["token"], "tok1");
        assert_eq!(out["attachment"]["id"], 7);
        assert!(out["attachment"].get("deleted").is_none());
        assert_eq!(server.received_requests().await.unwrap()[0].body, PNG);

        let err = c
            .upload_attachment("a.png", "image/png", "!!!")
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("not valid base64"), "{err}");
        let big =
            base64::engine::general_purpose::STANDARD.encode(vec![0u8; MAX_ATTACHMENT_BYTES + 1]);
        let err = c
            .upload_attachment("a.bin", "application/octet-stream", &big)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("size limit"), "{err}");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn search_omits_sort_by_unless_given() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/search.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
            .mount(&server)
            .await;
        let c = client(&server);
        c.search("type:ticket", 1, 25, None, "desc").await.unwrap();
        c.search("type:ticket", 1, 25, Some("updated_at"), "desc")
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        let sort_by = |i: usize| {
            requests[i]
                .url
                .query_pairs()
                .find(|(k, _)| k == "sort_by")
                .map(|(_, v)| v.to_string())
        };
        assert_eq!(sort_by(0), None);
        assert_eq!(sort_by(1).as_deref(), Some("updated_at"));
    }

    #[tokio::test]
    async fn create_ticket_sends_requester_email_ccs_and_private_comment() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/tickets.json"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"ticket": {
                "id": 1, "group_id": 5, "problem_id": 2, "external_id": "e"
            }})))
            .mount(&server)
            .await;
        let c = client(&server);
        let out = c
            .create_ticket(CreateTicket {
                subject: "s".into(),
                description: "d".into(),
                requester: Some(json!({"name": "Ann", "email": "ann@example.com"})),
                email_ccs: Some(vec!["cc@example.com".into()]),
                upload_tokens: Some(vec!["tok".into()]),
                public: Some(false),
                group_id: Some(5),
                problem_id: Some(2),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out["group_id"], 5);
        assert_eq!(out["problem_id"], 2);
        assert_eq!(out["external_id"], "e");
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        let ticket = &body["ticket"];
        assert_eq!(
            ticket["comment"],
            json!({"body": "d", "uploads": ["tok"], "public": false})
        );
        assert_eq!(ticket["requester"]["email"], "ann@example.com");
        assert_eq!(
            ticket["email_ccs"],
            json!([{"user_email": "cc@example.com", "action": "put"}])
        );
        assert_eq!(ticket["group_id"], 5);
        assert!(ticket.get("requester_id").is_none());

        let err = c
            .create_ticket(CreateTicket {
                requester: Some(json!({"email": "a@example.com"})),
                requester_id: Some(1),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("not both"), "{err}");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn update_ticket_sends_email_ccs_and_safe_update() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v2/tickets/4.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ticket": {"id": 4}})))
            .mount(&server)
            .await;
        let c = client(&server);
        let fields = |v: Value| v.as_object().unwrap().clone();
        c.update_ticket(
            4,
            fields(json!({
                "email_ccs": [{"user_email": "cc@example.com", "action": "delete"}],
                "safe_update": true,
                "updated_stamp": "2026-01-01T00:00:00Z",
            })),
        )
        .await
        .unwrap();
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["ticket"]["safe_update"], true);
        assert_eq!(body["ticket"]["updated_stamp"], "2026-01-01T00:00:00Z");
        assert_eq!(body["ticket"]["email_ccs"][0]["action"], "delete");

        let err = c
            .update_ticket(4, fields(json!({"safe_update": true})))
            .await
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("requires updated_stamp"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn update_ticket_rejects_empty_updates_and_cc_entries_without_a_user() {
        let c = offline_client();
        let err = c.update_ticket(4, Map::new()).await.unwrap_err();
        assert!(format!("{err:#}").contains("at least one field"), "{err}");
        let err = c
            .update_ticket(
                4,
                json!({"email_ccs": [{"action": "put"}]})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .await
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("user_id or a user_email"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn post_comment_without_an_audit_comment_has_a_null_id() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let out = client(&server)
            .post_comment(3, "x", true, None, &[])
            .await
            .unwrap();
        assert_eq!(out, json!({"id": null, "public": true}));
    }

    #[tokio::test]
    async fn merge_rejects_empty_self_and_duplicate_sources() {
        let c = offline_client();
        for sources in [&[][..], &[9][..], &[1, 2, 1][..]] {
            assert!(
                c.merge_tickets(9, sources, "t", "s", None, None)
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn tag_removal_failure_after_a_successful_add_says_so() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"tags": ["a"]})))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(422))
            .mount(&server)
            .await;
        let c = client(&server);
        let err = c
            .update_ticket_tags(3, &["a".into()], &["b".into()])
            .await
            .unwrap_err();
        let err = format!("{err:#}");
        assert!(err.contains("tags added but removing failed"), "{err}");
        let err = c
            .update_ticket_tags(3, &[], &["b".into()])
            .await
            .unwrap_err()
            .to_string();
        assert!(!err.contains("tags added"), "{err}");
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
    fn attachment_url_on_account_host_must_be_an_attachment_route() {
        let c = offline_client();
        for url in [
            "https://acme.zendesk.com/api/v2/users/me.json",
            "https://acme.zendesk.com:8443/attachments/token/abc/?name=x.png",
        ] {
            let err = c.validate_attachment_url(url).unwrap_err().to_string();
            assert!(
                err.starts_with("content_url must be an attachment URL on acme.zendesk.com"),
                "{url}"
            );
        }
        let (_, creds) = c
            .validate_attachment_url("https://acme.zendesk.com/attachments/token/abc/?name=x.png")
            .unwrap();
        assert!(creds);
    }

    #[tokio::test]
    async fn bulk_and_merge_inputs_are_capped() {
        let c = offline_client();
        let ids: Vec<u64> = (1..=1001).collect();
        let err = format!("{:#}", c.get_tickets_bulk(&ids).await.unwrap_err());
        assert!(err.contains("at most 1000 ids per call"), "{err}");
        let err = c
            .merge_tickets(9999, &ids[..101], "t", "s", None, None)
            .await
            .unwrap_err();
        let err = format!("{err:#}");
        assert!(err.contains("at most 100 source tickets"), "{err}");
    }

    #[test]
    fn attachment_url_credentials_only_for_account_host() {
        let c = offline_client();
        let (url, creds) = c
            .validate_attachment_url("https://ACME.Zendesk.com/attachments/token/a.png")
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
            format!("{err:#}").contains(
                "Invalid role 'foo'. Allowed: [\"assigned\", \"ccd\", \"followed\", \"requested\"]"
            ),
            "{err}"
        );
    }

    #[tokio::test]
    async fn get_user_tickets_accepts_followed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/2/tickets/followed.json"))
            .respond_with(json_page("tickets", json!([{"id": 1}]), None))
            .mount(&server)
            .await;
        let out = client(&server)
            .get_user_tickets(2, "followed", 1, 25)
            .await
            .unwrap();
        assert_eq!(out["tickets"][0]["id"], 1);
        assert_eq!(out["count"], 1);
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
