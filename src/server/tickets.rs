use super::*;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TicketIdParams {
    /// The ID of the ticket
    ticket_id: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CreateTicketParams {
    /// Ticket subject
    subject: String,
    /// Ticket description
    description: String,
    requester_id: Option<u64>,
    assignee_id: Option<u64>,
    /// low, normal, high, urgent
    priority: Option<String>,
    /// problem, incident, question, task
    #[serde(rename = "type")]
    ticket_type: Option<String>,
    tags: Option<Vec<String>>,
    custom_fields: Option<Vec<serde_json::Map<String, Value>>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetTicketsParams {
    /// Page number
    #[serde(default = "page_1")]
    page: u64,
    /// Number of tickets per page (max 100)
    #[serde(default = "per_page_25")]
    per_page: u64,
    /// Field to sort by (created_at, updated_at, priority, status)
    #[serde(default = "sort_by_created_at")]
    sort_by: String,
    /// Sort order (asc or desc)
    #[serde(default = "sort_desc")]
    sort_order: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CreateCommentParams {
    /// The ID of the ticket to comment on
    ticket_id: u64,
    /// The comment text. Markdown, plain text, and HTML are all accepted.
    comment: String,
    /// Whether the comment should be public
    #[serde(default = "default_true")]
    public: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct AttachmentParams {
    /// The content_url of the attachment from get_ticket_comments
    content_url: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UpdateTicketParams {
    /// The ID of the ticket to update
    ticket_id: u64,
    subject: Option<String>,
    /// new, open, pending, on-hold, solved, closed
    status: Option<String>,
    /// low, normal, high, urgent
    priority: Option<String>,
    #[serde(rename = "type")]
    ticket_type: Option<String>,
    assignee_id: Option<u64>,
    requester_id: Option<u64>,
    tags: Option<Vec<String>>,
    custom_fields: Option<Vec<serde_json::Map<String, Value>>>,
    /// ISO8601 datetime
    due_at: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchParams {
    /// ZQL search query
    query: String,
    #[serde(default = "page_1")]
    page: u64,
    /// Results per page (max 100)
    #[serde(default = "per_page_25")]
    per_page: u64,
    /// relevance, updated_at, created_at, priority, status, ticket_type
    #[serde(default = "sort_by_relevance")]
    sort_by: String,
    /// asc or desc
    #[serde(default = "sort_desc")]
    sort_order: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchAllTicketsParams {
    /// ZQL search query, e.g. 'type:ticket status:open priority:urgent'
    query: String,
    /// updated_at, created_at, priority, status, ticket_type (defaults to created_at)
    #[serde(default = "sort_by_created_at")]
    sort_by: String,
    /// asc or desc
    #[serde(default = "sort_desc")]
    sort_order: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TicketsBulkParams {
    /// List of ticket IDs
    ticket_ids: Vec<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct MergeTicketsParams {
    /// The ticket to merge into
    target_id: u64,
    /// Tickets to merge from
    source_ids: Vec<u64>,
    #[serde(default = "default_target_comment")]
    target_comment: String,
    #[serde(default = "default_source_comment")]
    source_comment: String,
    /// Whether the comment on the target ticket is public (Zendesk defaults to private)
    target_comment_is_public: Option<bool>,
    /// Whether the comments on the source tickets are public (Zendesk defaults to private)
    source_comment_is_public: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct JobStatusParams {
    /// The job status ID returned by merge_tickets or a bulk operation
    job_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UserTicketsParams {
    /// The user ID
    user_id: u64,
    /// requested, assigned, or ccd
    #[serde(default = "role_requested")]
    role: String,
    #[serde(default = "page_1")]
    page: u64,
    #[serde(default = "per_page_25")]
    per_page: u64,
}

#[tool_router(router = ticket_router, vis = "pub(super)")]
impl ZendeskServer {
    #[tool(
        description = "Retrieve a Zendesk ticket by its ID",
        annotations(read_only_hint = true)
    )]
    async fn get_ticket(&self, Parameters(p): Parameters<TicketIdParams>) -> CallToolResult {
        self.call_json(|c| async move { c.get_ticket(p.ticket_id).await })
            .await
    }

    #[tool(
        description = "Create a new Zendesk ticket",
        annotations(destructive_hint = false)
    )]
    async fn create_ticket(&self, Parameters(p): Parameters<CreateTicketParams>) -> CallToolResult {
        self.call_json(|c| async move {
            let ticket = crate::zendesk::CreateTicket {
                subject: p.subject,
                description: p.description,
                requester_id: p.requester_id,
                assignee_id: p.assignee_id,
                priority: p.priority,
                ticket_type: p.ticket_type,
                tags: p.tags,
                custom_fields: p
                    .custom_fields
                    .map(|f| f.into_iter().map(Value::Object).collect()),
            };
            let created = c.create_ticket(ticket).await?;
            Ok(wrapped("Ticket created successfully", "ticket", created))
        })
        .await
    }

    #[tool(
        description = "Fetch the latest tickets with pagination support",
        annotations(read_only_hint = true)
    )]
    async fn get_tickets(&self, Parameters(p): Parameters<GetTicketsParams>) -> CallToolResult {
        self.call_json(|c| async move {
            c.get_tickets(p.page, p.per_page, &p.sort_by, &p.sort_order)
                .await
        })
        .await
    }

    #[tool(
        description = "Retrieve all comments for a Zendesk ticket by its ID",
        annotations(read_only_hint = true)
    )]
    async fn get_ticket_comments(
        &self,
        Parameters(p): Parameters<TicketIdParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.get_ticket_comments(p.ticket_id).await })
            .await
    }

    #[tool(
        description = "Create a new comment on an existing Zendesk ticket",
        annotations(destructive_hint = false)
    )]
    async fn create_ticket_comment(
        &self,
        Parameters(p): Parameters<CreateCommentParams>,
    ) -> CallToolResult {
        self.call(|c| async move {
            let comment = c.post_comment(p.ticket_id, &p.comment, p.public).await?;
            Ok(ContentBlock::text(format!(
                "Comment created successfully: {comment}"
            )))
        })
        .await
    }

    #[tool(
        description = "Fetch a Zendesk ticket attachment by its content_url and return the file as base64-encoded data. Use the attachment URLs returned by get_ticket_comments.",
        annotations(read_only_hint = true)
    )]
    async fn get_ticket_attachment(
        &self,
        Parameters(p): Parameters<AttachmentParams>,
    ) -> CallToolResult {
        self.call(|c| async move {
            let a = c.get_ticket_attachment(&p.content_url).await?;
            if a.content_type.starts_with("image/") {
                Ok(ContentBlock::image(a.data_base64, a.content_type))
            } else {
                json_block(&json!({ "content_type": a.content_type, "data_base64": a.data_base64 }))
            }
        })
        .await
    }

    #[tool(
        description = "Update fields on an existing Zendesk ticket (e.g., status, priority, assignee_id)",
        annotations(destructive_hint = false, idempotent_hint = true)
    )]
    async fn update_ticket(&self, Parameters(p): Parameters<UpdateTicketParams>) -> CallToolResult {
        self.call_json(|c| async move {
            let mut fields = serde_json::Map::new();
            let mut set = |key: &str, value: Option<Value>| {
                if let Some(v) = value {
                    fields.insert(key.to_string(), v);
                }
            };
            set("subject", p.subject.map(Value::from));
            set("status", p.status.map(Value::from));
            set("priority", p.priority.map(Value::from));
            set("type", p.ticket_type.map(Value::from));
            set("assignee_id", p.assignee_id.map(Value::from));
            set("requester_id", p.requester_id.map(Value::from));
            set("tags", p.tags.map(Value::from));
            set(
                "custom_fields",
                p.custom_fields
                    .map(|f| Value::Array(f.into_iter().map(Value::Object).collect())),
            );
            set("due_at", p.due_at.map(Value::from));
            let updated = c.update_ticket(p.ticket_id, fields).await?;
            Ok(wrapped("Ticket updated successfully", "ticket", updated))
        })
        .await
    }

    #[tool(
        description = "Search Zendesk using Zendesk Query Language (ZQL). Searches tickets, users, and organizations. Example queries: 'type:ticket status:open priority:urgent', 'type:ticket assignee:me', 'type:user email:john@example.com'",
        annotations(read_only_hint = true)
    )]
    async fn search(&self, Parameters(p): Parameters<SearchParams>) -> CallToolResult {
        self.call_json(|c| async move {
            c.search(&p.query, p.page, p.per_page, &p.sort_by, &p.sort_order)
                .await
        })
        .await
    }

    #[tool(
        description = "Search Zendesk tickets with ZQL and return every match instead of one page like 'search', up to Zendesk's 1,000-result search limit ('truncated' is true when more matched; narrow the query to get the rest). Use for 'find all tickets matching X' queries.",
        annotations(read_only_hint = true)
    )]
    async fn search_all_tickets(
        &self,
        Parameters(p): Parameters<SearchAllTicketsParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            c.search_all_tickets(&p.query, &p.sort_by, &p.sort_order)
                .await
        })
        .await
    }

    #[tool(
        description = "Fetch multiple tickets by IDs (requested in batches of 100)",
        annotations(read_only_hint = true)
    )]
    async fn get_tickets_bulk(
        &self,
        Parameters(p): Parameters<TicketsBulkParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.get_tickets_bulk(&p.ticket_ids).await })
            .await
    }

    #[tool(
        description = "Merge source tickets into a target ticket. Irreversible. Waits up to 20 seconds for Zendesk's background job; if it is still running the result says so, and get_job_status checks it later.",
        annotations(destructive_hint = true)
    )]
    async fn merge_tickets(&self, Parameters(p): Parameters<MergeTicketsParams>) -> CallToolResult {
        self.call_json(|c| async move {
            let result = c
                .merge_tickets(
                    p.target_id,
                    &p.source_ids,
                    &p.target_comment,
                    &p.source_comment,
                    p.target_comment_is_public,
                    p.source_comment_is_public,
                )
                .await?;
            let message = match result["status"].as_str() {
                Some("completed") => "Tickets merged successfully",
                Some("failed") => "Merge failed",
                _ => "Merge is still running; check it with get_job_status",
            };
            Ok(wrapped(message, "result", result))
        })
        .await
    }

    #[tool(
        description = "Get tickets for a specific user by role (requested, assigned, or ccd)",
        annotations(read_only_hint = true)
    )]
    async fn get_user_tickets(
        &self,
        Parameters(p): Parameters<UserTicketsParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            c.get_user_tickets(p.user_id, &p.role, p.page, p.per_page)
                .await
        })
        .await
    }

    #[tool(
        description = "Permanently delete a Zendesk ticket. Use with caution.",
        annotations(destructive_hint = true)
    )]
    async fn delete_ticket(&self, Parameters(p): Parameters<TicketIdParams>) -> CallToolResult {
        self.call_json(|c| async move {
            c.delete_ticket(p.ticket_id).await?;
            Ok(json!({ "message": format!("Ticket {} deleted successfully", p.ticket_id) }))
        })
        .await
    }

    #[tool(
        description = "Get performance/SLA metrics for a specific ticket (reply time, resolution time, wait times, etc.)",
        annotations(read_only_hint = true)
    )]
    async fn get_ticket_metrics(
        &self,
        Parameters(p): Parameters<TicketIdParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.get_ticket_metrics(p.ticket_id).await })
            .await
    }

    #[tool(
        description = "Retrieve the audit trail (all changes and events) for a Zendesk ticket by its ID",
        annotations(read_only_hint = true)
    )]
    async fn get_ticket_audits(&self, Parameters(p): Parameters<TicketIdParams>) -> CallToolResult {
        self.call_json(|c| async move { c.get_ticket_audits(p.ticket_id).await })
            .await
    }

    #[tool(
        description = "Get the incident tickets linked to a Zendesk problem ticket by its ID",
        annotations(read_only_hint = true)
    )]
    async fn get_linked_incidents(
        &self,
        Parameters(p): Parameters<TicketIdParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.get_linked_incidents(p.ticket_id).await })
            .await
    }

    #[tool(
        description = "Get the status of a Zendesk background job (status, progress, per-item results), e.g. a merge_tickets job that was still running. 'pending' is true while it is queued or working.",
        annotations(read_only_hint = true)
    )]
    async fn get_job_status(&self, Parameters(p): Parameters<JobStatusParams>) -> CallToolResult {
        self.call_json(|c| async move { c.get_job_status(&p.job_id).await })
            .await
    }
}
