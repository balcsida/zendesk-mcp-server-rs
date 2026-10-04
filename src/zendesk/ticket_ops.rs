use anyhow::{Result, bail};
use serde_json::{Map, Value, json};

use super::*;

impl ZendeskClient {
    pub async fn list_deleted_tickets(&self, page: u64, per_page: u64) -> Result<Value> {
        async {
            let per_page = per_page.min(100);
            let data = self
                .api_get(
                    "deleted_tickets.json",
                    &[("page", &page), ("per_page", &per_page)],
                )
                .await?;
            let deleted = pick_all(
                &data,
                "deleted_tickets",
                &["id", "subject", "deleted_at", "actor", "previous_state"],
                &[],
            );
            Ok(json!({
                "count": deleted.as_array().map_or(0, Vec::len),
                "deleted_tickets": deleted,
                "has_more": !data["next_page"].is_null(),
            }))
        }
        .await
        .map_err(ctx("Failed to list deleted tickets"))
    }

    pub async fn restore_deleted_ticket(&self, ticket_id: u64) -> Result<()> {
        self.api_put(
            &format!("deleted_tickets/{ticket_id}/restore.json"),
            &json!({}),
        )
        .await
        .map(drop)
        .map_err(ctx(format!("Failed to restore ticket {ticket_id}")))
    }

    /// One page of suspended tickets (cursor pagination only). `content` is untrusted
    /// text and is only returned when `include_content` is set.
    pub async fn list_suspended_tickets(
        &self,
        page_size: u64,
        after_cursor: Option<&str>,
        include_content: bool,
    ) -> Result<Value> {
        async {
            let data = self
                .get_cursor_page("suspended_tickets.json", &[], page_size, after_cursor)
                .await?;
            let suspended: Vec<Value> = data["suspended_tickets"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|t| {
                    let mut out = pick(
                        t,
                        &[
                            "id",
                            "subject",
                            "cause",
                            "cause_id",
                            "recipient",
                            "created_at",
                            "ticket_id",
                        ],
                        &[],
                    );
                    out["author"] = pick(&t["author"], &["id", "name", "email"], &[]);
                    out["channel"] = t["via"]["channel"].clone();
                    if include_content {
                        out["content"] = t["content"].clone();
                    }
                    out
                })
                .collect();
            Ok(json!({
                "suspended_tickets": suspended,
                "has_more": data["meta"]["has_more"].as_bool().unwrap_or(false),
                "after_cursor": data["meta"]["after_cursor"],
            }))
        }
        .await
        .map_err(ctx("Failed to list suspended tickets"))
    }

    /// Recovers one suspended ticket; a 422 (why it could not be recovered) is an error.
    pub async fn recover_suspended_ticket(&self, suspended_ticket_id: u64) -> Result<Value> {
        async {
            let data = self
                .api_put(
                    &format!("suspended_tickets/{suspended_ticket_id}/recover.json"),
                    &json!({}),
                )
                .await?;
            // The spec's example shows `ticket` as an array and its text says `tickets`.
            let ticket = [&data["ticket"], &data["tickets"]]
                .into_iter()
                .find_map(|t| {
                    if t.is_array() {
                        t.get(0)
                    } else {
                        t.as_object().map(|_| t)
                    }
                })
                .ok_or_else(|| anyhow!("Zendesk response has no recovered ticket"))?;
            let mut keys = TICKET_SUMMARY_KEYS.to_vec();
            keys.push("description");
            Ok(pick(ticket, &keys, &[]))
        }
        .await
        .map_err(ctx(format!(
            "Failed to recover suspended ticket {suspended_ticket_id}"
        )))
    }

    pub async fn make_comment_private(&self, ticket_id: u64, comment_id: u64) -> Result<()> {
        self.api_put(
            &format!("tickets/{ticket_id}/comments/{comment_id}/make_private.json"),
            &json!({}),
        )
        .await
        .map(drop)
        .map_err(ctx(format!(
            "Failed to make comment {comment_id} on ticket {ticket_id} private"
        )))
    }

    /// Permanently replaces every occurrence of `text` in the comment with block characters.
    pub async fn redact_comment_text(
        &self,
        ticket_id: u64,
        comment_id: u64,
        text: &str,
    ) -> Result<Value> {
        async {
            if text.is_empty() {
                bail!("text to redact must not be empty");
            }
            let data = self
                .api_put(
                    &format!("tickets/{ticket_id}/comments/{comment_id}/redact.json"),
                    &json!({ "text": text }),
                )
                .await?;
            Ok(pick(
                object(&data, "comment")?,
                &[
                    "id",
                    "body",
                    "html_body",
                    "plain_body",
                    "public",
                    "created_at",
                ],
                &[],
            ))
        }
        .await
        .map_err(ctx(format!(
            "Failed to redact comment {comment_id} on ticket {ticket_id}"
        )))
    }

    pub async fn mark_ticket_as_spam(&self, ticket_id: u64) -> Result<()> {
        self.api_put(
            &format!("tickets/{ticket_id}/mark_as_spam.json"),
            &json!({}),
        )
        .await
        .map(drop)
        .map_err(ctx(format!("Failed to mark ticket {ticket_id} as spam")))
    }

    /// Applies `fields` (the ticket attributes to set) to 1 to 100 tickets and waits up to
    /// 30 seconds for Zendesk's background job; returns its trimmed status (`pending` is
    /// true if it is still running).
    pub async fn update_tickets_bulk(
        &self,
        ticket_ids: &[u64],
        fields: Map<String, Value>,
    ) -> Result<Value> {
        async {
            if ticket_ids.is_empty() || ticket_ids.len() > 100 {
                bail!(
                    "Give between 1 and 100 ticket_ids, got {}",
                    ticket_ids.len()
                );
            }
            if fields.is_empty() {
                bail!("Give at least one field to update");
            }
            let ids = ticket_ids
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let job = self
                .api_put(
                    &format!("tickets/update_many.json?ids={ids}"),
                    &json!({ "ticket": fields }),
                )
                .await?;
            self.wait_for_job(&job, Duration::from_secs(30)).await
        }
        .await
        .map_err(ctx("Failed to update tickets in bulk"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zendesk::test_support::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn put_ok(server_path: &str) -> Mock {
        Mock::given(method("PUT"))
            .and(path(server_path.to_string()))
            .respond_with(ResponseTemplate::new(200))
    }

    #[tokio::test]
    async fn deleted_tickets_are_trimmed_with_has_more() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/deleted_tickets.json"))
            .and(query_param("per_page", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "deleted_tickets": [{"id": 581, "subject": "s", "deleted_at": "d",
                    "actor": {"id": 1, "name": "A"}, "previous_state": "open", "x": 1}],
                "next_page": "https://x/next",
            })))
            .mount(&server)
            .await;
        let out = client(&server).list_deleted_tickets(1, 500).await.unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["has_more"], true);
        assert_eq!(
            out["deleted_tickets"][0],
            json!({"id": 581, "subject": "s", "deleted_at": "d",
                   "actor": {"id": 1, "name": "A"}, "previous_state": "open"})
        );
    }

    #[tokio::test]
    async fn restore_and_make_private_and_spam_tolerate_empty_bodies() {
        let server = MockServer::start().await;
        put_ok("/api/v2/deleted_tickets/5/restore.json")
            .mount(&server)
            .await;
        put_ok("/api/v2/tickets/5/comments/6/make_private.json")
            .mount(&server)
            .await;
        put_ok("/api/v2/tickets/5/mark_as_spam.json")
            .mount(&server)
            .await;
        let c = client(&server);
        c.restore_deleted_ticket(5).await.unwrap();
        c.make_comment_private(5, 6).await.unwrap();
        c.mark_ticket_as_spam(5).await.unwrap();
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn suspended_tickets_use_cursor_params_and_hide_content_by_default() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/suspended_tickets.json"))
            .and(query_param("page[size]", "100"))
            .and(query_param("page[after]", "abc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "suspended_tickets": [{"id": 435, "subject": "s", "cause": "spam", "cause_id": 0,
                    "author": {"id": 1, "name": "N", "email": "e@x.com", "extra": 1},
                    "recipient": "r", "created_at": "c", "ticket_id": null, "content": "buy now",
                    "via": {"channel": "email"}}],
                "meta": {"has_more": true, "after_cursor": "next"},
            })))
            .mount(&server)
            .await;
        let c = client(&server);
        let out = c
            .list_suspended_tickets(500, Some("abc"), false)
            .await
            .unwrap();
        assert_eq!(out["has_more"], true);
        assert_eq!(out["after_cursor"], "next");
        let t = &out["suspended_tickets"][0];
        assert_eq!(t["channel"], "email");
        assert_eq!(
            t["author"],
            json!({"id": 1, "name": "N", "email": "e@x.com"})
        );
        assert!(t.get("content").is_none());
    }

    #[tokio::test]
    async fn suspended_content_is_included_on_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "suspended_tickets": [{"id": 1, "content": "buy now"}],
                "meta": {"has_more": false, "after_cursor": null},
            })))
            .mount(&server)
            .await;
        let out = client(&server)
            .list_suspended_tickets(25, None, true)
            .await
            .unwrap();
        assert_eq!(out["suspended_tickets"][0]["content"], "buy now");
        assert_eq!(out["has_more"], false);
    }

    #[tokio::test]
    async fn recover_accepts_a_ticket_object_or_array_and_surfaces_422() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v2/suspended_tickets/1/recover.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"ticket": {"id": 9, "subject": "s", "description": "d", "extra": 1}}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/api/v2/suspended_tickets/2/recover.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ticket": [{"id": 10}]})))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/api/v2/suspended_tickets/3/recover.json"))
            .respond_with(ResponseTemplate::new(422).set_body_json(
                json!({"suspended_tickets": [{"id": 3, "error_messages": "Author is suspended"}]}),
            ))
            .mount(&server)
            .await;
        let c = client(&server);
        let one = c.recover_suspended_ticket(1).await.unwrap();
        assert_eq!(one["id"], 9);
        assert_eq!(one["description"], "d");
        assert!(one.get("extra").is_none());
        assert_eq!(c.recover_suspended_ticket(2).await.unwrap()["id"], 10);
        let err = c.recover_suspended_ticket(3).await.unwrap_err().to_string();
        assert!(
            err.contains("HTTP 422") && err.contains("Author is suspended"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn redact_sends_text_and_trims_the_comment() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v2/tickets/5/comments/6/redact.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"comment": {
                "id": 6, "body": "card ▇▇▇▇", "public": true, "author_id": 1}})))
            .mount(&server)
            .await;
        let c = client(&server);
        let out = c.redact_comment_text(5, 6, "1234").await.unwrap();
        assert_eq!(out["body"], "card ▇▇▇▇");
        assert!(out.get("author_id").is_none());
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body, json!({"text": "1234"}));
        assert!(c.redact_comment_text(5, 6, "").await.is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn bulk_update_sends_ids_and_body_and_waits_for_the_job() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v2/tickets/update_many.json"))
            .and(query_param("ids", "1,2,3"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"job_status": {"id": "j1", "status": "queued"}})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/job_statuses/j1.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"job_status": {"id": "j1", "status": "completed", "progress": 3, "total": 3}}),
            ))
            .mount(&server)
            .await;
        let c = client(&server);
        let fields = json!({"status": "solved", "additional_tags": ["a"], "remove_tags": ["b"]})
            .as_object()
            .unwrap()
            .clone();
        let out = c.update_tickets_bulk(&[1, 2, 3], fields).await.unwrap();
        assert_eq!(out["status"], "completed");
        assert_eq!(out["pending"], false);
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(
            body,
            json!({"ticket": {"status": "solved", "additional_tags": ["a"], "remove_tags": ["b"]}})
        );

        let one = json!({"status": "open"}).as_object().unwrap().clone();
        let too_many: Vec<u64> = (1..=101).collect();
        assert!(c.update_tickets_bulk(&too_many, one.clone()).await.is_err());
        assert!(c.update_tickets_bulk(&[], one).await.is_err());
        assert!(c.update_tickets_bulk(&[1], Map::new()).await.is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }
}
