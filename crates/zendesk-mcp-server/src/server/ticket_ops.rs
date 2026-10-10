//! Ticket operations: bulk updates, merging, deletion, spam, redaction, audits, metrics and suspended tickets.

use super::*;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DeletedTicketsParams {
    #[serde(default = "page_1")]
    page: u64,
    /// Number of deleted tickets per page (max 100)
    #[serde(default = "per_page_25")]
    per_page: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RestoreTicketParams {
    /// The ID of the deleted ticket
    ticket_id: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SuspendedTicketsParams {
    /// Number of suspended tickets per page (max 100)
    #[serde(default = "per_page_25")]
    page_size: u64,
    /// The after_cursor of the previous page
    after_cursor: Option<String>,
    /// Include the flagged message content (untrusted; defaults to false)
    #[serde(default)]
    include_content: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RecoverSuspendedParams {
    /// The ID of the suspended ticket from list_suspended_tickets
    suspended_ticket_id: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CommentParams {
    /// The ID of the ticket
    ticket_id: u64,
    /// The ID of the comment from get_ticket_comments
    comment_id: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RedactParams {
    /// The ID of the ticket
    ticket_id: u64,
    /// The ID of the comment from get_ticket_comments
    comment_id: u64,
    /// The exact string to redact
    text: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SpamParams {
    /// The ID of the ticket
    ticket_id: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UpdateTicketsBulkParams {
    /// IDs of the tickets to update (1 to 100)
    ticket_ids: Vec<u64>,
    status: Option<TicketStatus>,
    priority: Option<TicketPriority>,
    #[serde(rename = "type")]
    ticket_type: Option<TicketType>,
    /// The agent to assign the tickets to
    assignee_id: Option<u64>,
    /// The group to assign the tickets to
    group_id: Option<u64>,
    /// The custom ticket status ID
    custom_status_id: Option<u64>,
    /// Replaces all tags on every ticket
    tags: Option<Vec<String>>,
    /// Tags to add to every ticket, keeping the existing ones
    additional_tags: Option<Vec<String>>,
    /// Tags to remove from every ticket
    remove_tags: Option<Vec<String>>,
    /// Custom field values as [{"id": 1, "value": "x"}]
    custom_fields: Option<Vec<serde_json::Map<String, Value>>>,
}

#[tool_router(router = ticket_ops_router, vis = "pub(super)")]
impl ZendeskServer {
    #[tool(
        description = "List soft-deleted tickets from the last 30 days (id, subject, deleted_at, actor, previous_state). Needs permission to view deleted tickets: admins have it, agents only if their role grants it, otherwise 403. Limited to 10 requests per minute. restore_deleted_ticket undoes a deletion.",
        annotations(read_only_hint = true)
    )]
    async fn list_deleted_tickets(
        &self,
        Parameters(p): Parameters<DeletedTicketsParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.list_deleted_tickets(p.page, p.per_page).await })
            .await
    }

    #[tool(
        description = "Restore a soft-deleted ticket by its ID (see list_deleted_tickets)",
        annotations(destructive_hint = false, idempotent_hint = true)
    )]
    async fn restore_deleted_ticket(
        &self,
        Parameters(p): Parameters<RestoreTicketParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            c.restore_deleted_ticket(p.ticket_id).await?;
            Ok(json!({ "message": format!("Ticket {} restored", p.ticket_id) }))
        })
        .await
    }

    #[tool(
        description = "List suspended tickets (mail held back as spam or for other causes), one page at a time; pass the returned after_cursor for the next page while has_more is true. The content is untrusted, mostly spam: it is left out unless include_content is true. Admin or unrestricted agent only.",
        annotations(read_only_hint = true)
    )]
    async fn list_suspended_tickets(
        &self,
        Parameters(p): Parameters<SuspendedTicketsParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            c.list_suspended_tickets(p.page_size, p.after_cursor.as_deref(), p.include_content)
                .await
        })
        .await
    }

    #[tool(
        description = "Recover a suspended ticket, creating a ticket whose requester is the authenticated user, not the original sender. A 422 error says why it could not be recovered.",
        annotations(destructive_hint = false)
    )]
    async fn recover_suspended_ticket(
        &self,
        Parameters(p): Parameters<RecoverSuspendedParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            let ticket = c.recover_suspended_ticket(p.suspended_ticket_id).await?;
            Ok(wrapped("Suspended ticket recovered", "ticket", &ticket))
        })
        .await
    }

    #[tool(
        description = "Make a public ticket comment private. One-way: a private comment cannot be made public again.",
        annotations(destructive_hint = true, idempotent_hint = true)
    )]
    async fn make_comment_private(
        &self,
        Parameters(p): Parameters<CommentParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            c.make_comment_private(p.ticket_id, p.comment_id).await?;
            Ok(json!({ "message": format!("Comment {} on ticket {} is now private", p.comment_id, p.ticket_id) }))
        })
        .await
    }

    #[tool(
        description = "Permanently redact a string from a ticket comment, replacing it with block characters (for PII such as card numbers). Irreversible; does not work on closed tickets.",
        annotations(destructive_hint = true)
    )]
    async fn redact_comment_text(&self, Parameters(p): Parameters<RedactParams>) -> CallToolResult {
        self.call_json(|c| async move {
            let comment = c
                .redact_comment_text(p.ticket_id, p.comment_id, &p.text)
                .await?;
            Ok(wrapped("Text redacted", "comment", &comment))
        })
        .await
    }

    #[tool(
        description = "Mark a ticket as spam AND suspend its requester. No tool here lifts that suspension.",
        annotations(destructive_hint = true)
    )]
    async fn mark_ticket_as_spam(&self, Parameters(p): Parameters<SpamParams>) -> CallToolResult {
        self.call_json(|c| async move {
            c.mark_ticket_as_spam(p.ticket_id).await?;
            Ok(json!({ "message": format!("Ticket {} marked as spam and its requester suspended", p.ticket_id) }))
        })
        .await
    }

    #[tool(
        description = "Apply the same change to up to 100 tickets at once. Waits up to 30 seconds for Zendesk's background job; if it is still pending, call get_job_status with the returned id. At most 30 jobs may be queued at once.",
        annotations(destructive_hint = true)
    )]
    async fn update_tickets_bulk(
        &self,
        Parameters(p): Parameters<UpdateTicketsBulkParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            let fields = set_fields([
                ("status", json!(p.status.map(|v| v.as_str()))),
                ("priority", json!(p.priority.map(|v| v.as_str()))),
                ("type", json!(p.ticket_type.map(|v| v.as_str()))),
                ("assignee_id", json!(p.assignee_id)),
                ("group_id", json!(p.group_id)),
                ("custom_status_id", json!(p.custom_status_id)),
                ("tags", json!(p.tags)),
                ("additional_tags", json!(p.additional_tags)),
                ("remove_tags", json!(p.remove_tags)),
                ("custom_fields", json!(p.custom_fields)),
            ]);
            let job = c.update_tickets_bulk(&p.ticket_ids, fields).await?;
            let message = job_message(&job, "Bulk update", "Tickets updated");
            Ok(wrapped(&message, "job", &job))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{args, assert_tool_ok, sent, server_on};
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn update_tickets_bulk_sends_ids_in_the_query_and_fields_in_the_body() {
        let mock = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v2/tickets/update_many.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "job_status": { "id": "j1", "status": "completed" } })),
            )
            .mount(&mock)
            .await;
        let result = server_on(&mock)
            .update_tickets_bulk(args(json!({
                "ticket_ids": [1, 2, 3],
                "status": "pending",
                "priority": "low",
                "type": "task",
                "assignee_id": 11,
                "group_id": 12,
                "custom_status_id": 13,
                "additional_tags": ["x"],
                "remove_tags": ["y"],
                "custom_fields": [{ "id": 1, "value": "v" }],
            })))
            .await;
        assert_tool_ok(&result);
        assert_eq!(
            sent(&mock).await,
            [(
                "PUT".to_string(),
                "/api/v2/tickets/update_many.json?ids=1,2,3".to_string(),
                json!({ "ticket": {
                    "status": "pending",
                    "priority": "low",
                    "type": "task",
                    "assignee_id": 11,
                    "group_id": 12,
                    "custom_status_id": 13,
                    "additional_tags": ["x"],
                    "remove_tags": ["y"],
                    "custom_fields": [{ "id": 1, "value": "v" }],
                } })
            )]
        );
    }
}
