use serde::Serialize;

use super::*;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RequesterParams {
    /// Requester name
    name: String,
    /// Requester email address
    email: String,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum ListAction {
    Put,
    Delete,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
struct EmailCcChange {
    /// ID of the agent or end user (give this or user_email)
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<u64>,
    /// Email address of the agent or end user (give this or user_id)
    #[serde(skip_serializing_if = "Option::is_none")]
    user_email: Option<String>,
    /// put adds the CC, delete removes it
    action: ListAction,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
struct FollowerChange {
    /// ID of the agent
    user_id: u64,
    /// put adds the follower, delete removes it
    action: ListAction,
}

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
    /// The user who requested the ticket
    requester_id: Option<u64>,
    /// Requester as {name, email}; the end user is created if needed. Not together with requester_id.
    requester: Option<RequesterParams>,
    /// The agent to assign the ticket to
    assignee_id: Option<u64>,
    priority: Option<TicketPriority>,
    #[serde(rename = "type")]
    ticket_type: Option<TicketType>,
    /// Tags to put on the ticket
    tags: Option<Vec<String>>,
    /// Custom field values as [{"id": 1, "value": "x"}]
    custom_fields: Option<Vec<serde_json::Map<String, Value>>>,
    /// The group to assign the ticket to
    group_id: Option<u64>,
    /// The ticket form to use
    ticket_form_id: Option<u64>,
    /// The brand the ticket belongs to
    brand_id: Option<u64>,
    /// For an incident, the ID of the problem ticket it is linked to
    problem_id: Option<u64>,
    /// The ID of the closed ticket this ticket follows up
    via_followup_source_id: Option<u64>,
    /// The custom ticket status ID
    custom_status_id: Option<u64>,
    /// Due date (ISO 8601), for tickets of type task
    due_at: Option<String>,
    /// An ID linking the ticket to a record in another system
    external_id: Option<String>,
    /// Whether the description is a public comment; false makes it an internal note
    #[serde(default = "default_true")]
    public: bool,
    /// Email addresses to add as CCs
    email_ccs: Option<Vec<String>>,
    /// Upload tokens from upload_attachment to attach to the description
    upload_tokens: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TicketCommentsParams {
    /// The ID of the ticket
    ticket_id: u64,
    /// asc (oldest first) or desc (newest first)
    #[serde(default = "sort_asc")]
    sort_order: String,
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
    /// Whether the comment is public; a public comment is emailed to the requester, false makes it an internal note
    #[serde(default = "default_true")]
    public: bool,
    /// Also set the ticket status in the same update
    status: Option<TicketStatus>,
    /// Upload tokens from upload_attachment to attach to the comment
    upload_tokens: Option<Vec<String>>,
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
    /// New ticket subject
    subject: Option<String>,
    status: Option<TicketStatus>,
    priority: Option<TicketPriority>,
    /// New ticket type
    #[serde(rename = "type")]
    ticket_type: Option<TicketType>,
    /// The agent to assign the ticket to
    assignee_id: Option<u64>,
    /// The user who requested the ticket
    requester_id: Option<u64>,
    /// Replaces ALL tags on the ticket; use update_ticket_tags to add or remove individual tags
    tags: Option<Vec<String>>,
    /// Custom field values as [{"id": 1, "value": "x"}]
    custom_fields: Option<Vec<serde_json::Map<String, Value>>>,
    /// ISO8601 datetime
    due_at: Option<String>,
    /// The group to assign the ticket to
    group_id: Option<u64>,
    /// The custom ticket status ID
    custom_status_id: Option<u64>,
    /// For an incident, the ID of the problem ticket it is linked to
    problem_id: Option<u64>,
    /// An ID linking the ticket to a record in another system
    external_id: Option<String>,
    /// CCs to add or remove, e.g. [{"user_email": "a@example.com", "action": "put"}]
    email_ccs: Option<Vec<EmailCcChange>>,
    /// Followers to add or remove, e.g. [{"user_id": 1, "action": "delete"}]
    followers: Option<Vec<FollowerChange>>,
    /// Fail with a conflict instead of overwriting changes made since updated_stamp
    safe_update: Option<bool>,
    /// The ticket's current updated_at; required when safe_update is true
    updated_stamp: Option<String>,
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
    /// updated_at, created_at, priority, status, ticket_type (omit to sort by relevance)
    sort_by: Option<String>,
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
    /// Comment added to the target ticket; private unless target_comment_is_public is true
    #[serde(default = "default_target_comment")]
    target_comment: String,
    /// Comment added to each source ticket; private unless source_comment_is_public is true
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
    /// requested, assigned, ccd, or followed
    #[serde(default = "role_requested")]
    role: String,
    #[serde(default = "page_1")]
    page: u64,
    /// Number of tickets per page (max 100)
    #[serde(default = "per_page_25")]
    per_page: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CountTicketsParams {
    /// ZQL query, e.g. 'type:ticket status:open'; defaults to 'type:ticket' (all tickets)
    query: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchProblemTicketsParams {
    /// Text the problem ticket's subject contains; leave out to list the most recently updated problems
    text: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct OrganizationTicketsParams {
    /// The organization ID
    organization_id: u64,
    #[serde(default = "page_1")]
    page: u64,
    /// Number of tickets per page (max 100)
    #[serde(default = "per_page_25")]
    per_page: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UploadAttachmentParams {
    /// The name the file gets on the comment; keep the extension matching the content type
    filename: String,
    /// MIME type of the file, e.g. image/png or application/pdf
    content_type: String,
    /// The file content, base64-encoded (max 10 MB decoded)
    data_base64: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UpdateTicketTagsParams {
    /// The ID of the ticket
    ticket_id: u64,
    /// Tags to add
    add: Option<Vec<String>>,
    /// Tags to remove (no commas)
    remove: Option<Vec<String>>,
}

#[tool_router(router = ticket_router, vis = "pub(super)")]
impl ZendeskServer {
    #[tool(
        description = "Retrieve a Zendesk ticket by its ID, including type, tags, group, due date, form, brand, custom status, CC and follower IDs, comment count, channel and satisfaction rating",
        annotations(read_only_hint = true)
    )]
    async fn get_ticket(&self, Parameters(p): Parameters<TicketIdParams>) -> CallToolResult {
        self.call_json(|c| async move { c.get_ticket(p.ticket_id).await })
            .await
    }

    #[tool(
        description = "Create a new Zendesk ticket. The requester can be an existing user (requester_id) or {name, email} (created if needed). Set public=false to make the description an internal note.",
        annotations(destructive_hint = false)
    )]
    async fn create_ticket(&self, Parameters(p): Parameters<CreateTicketParams>) -> CallToolResult {
        self.call_json(|c| async move {
            let ticket = crate::zendesk::CreateTicket {
                subject: p.subject,
                description: p.description,
                requester_id: p.requester_id,
                requester: p
                    .requester
                    .map(|r| json!({"name": r.name, "email": r.email})),
                assignee_id: p.assignee_id,
                priority: p.priority.map(|v| v.as_str().to_string()),
                ticket_type: p.ticket_type.map(|v| v.as_str().to_string()),
                tags: p.tags,
                custom_fields: p
                    .custom_fields
                    .map(|f| f.into_iter().map(Value::Object).collect()),
                group_id: p.group_id,
                ticket_form_id: p.ticket_form_id,
                brand_id: p.brand_id,
                problem_id: p.problem_id,
                via_followup_source_id: p.via_followup_source_id,
                custom_status_id: p.custom_status_id,
                due_at: p.due_at,
                external_id: p.external_id,
                public: Some(p.public),
                email_ccs: p.email_ccs,
                upload_tokens: p.upload_tokens,
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
        description = "Retrieve all comments for a Zendesk ticket by its ID, oldest first unless sort_order is desc. Each comment has author_id and, when known, author_name.",
        annotations(read_only_hint = true)
    )]
    async fn get_ticket_comments(
        &self,
        Parameters(p): Parameters<TicketCommentsParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.get_ticket_comments(p.ticket_id, &p.sort_order).await })
            .await
    }

    #[tool(
        description = "Create a new comment on an existing Zendesk ticket. Public by default, which emails the requester; set public=false for an internal note. Set status to also change the ticket status in the same call, e.g. reply and set it pending. Returns the new comment's id.",
        annotations(destructive_hint = false)
    )]
    async fn create_ticket_comment(
        &self,
        Parameters(p): Parameters<CreateCommentParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            let comment = c
                .post_comment(
                    p.ticket_id,
                    &p.comment,
                    p.public,
                    p.status.map(TicketStatus::as_str),
                    &p.upload_tokens.unwrap_or_default(),
                )
                .await?;
            Ok(wrapped("Comment created", "comment", comment))
        })
        .await
    }

    #[tool(
        description = "Fetch a Zendesk ticket attachment by its content_url and return it as an image. Images only (JPEG, PNG, GIF, WebP, up to 10 MB); other types are rejected. Use the attachment URLs returned by get_ticket_comments.",
        annotations(read_only_hint = true)
    )]
    async fn get_ticket_attachment(
        &self,
        Parameters(p): Parameters<AttachmentParams>,
    ) -> CallToolResult {
        self.call(|c| async move {
            let a = c.get_ticket_attachment(&p.content_url).await?;
            Ok(ContentBlock::image(a.data_base64, a.content_type))
        })
        .await
    }

    #[tool(
        description = "Upload a file (base64, up to 10 MB) to attach to a ticket. Returns a token, valid for 60 minutes, to pass to create_ticket_comment or create_ticket as upload_tokens.",
        annotations(destructive_hint = false)
    )]
    async fn upload_attachment(
        &self,
        Parameters(p): Parameters<UploadAttachmentParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            c.upload_attachment(&p.filename, &p.content_type, &p.data_base64)
                .await
        })
        .await
    }

    #[tool(
        description = "Update fields on an existing Zendesk ticket (e.g., status, priority, assignee_id, group_id), and add or remove CCs and followers. To avoid overwriting concurrent changes, set safe_update=true with updated_stamp (the ticket's updated_at from get_ticket): Zendesk then rejects the update with a 409 conflict if the ticket changed in the meantime.",
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
            set("status", p.status.map(|v| v.as_str().into()));
            set("priority", p.priority.map(|v| v.as_str().into()));
            set("type", p.ticket_type.map(|v| v.as_str().into()));
            set("assignee_id", p.assignee_id.map(Value::from));
            set("requester_id", p.requester_id.map(Value::from));
            set("tags", p.tags.map(Value::from));
            set(
                "custom_fields",
                p.custom_fields
                    .map(|f| Value::Array(f.into_iter().map(Value::Object).collect())),
            );
            set("due_at", p.due_at.map(Value::from));
            set("group_id", p.group_id.map(Value::from));
            set("custom_status_id", p.custom_status_id.map(Value::from));
            set("problem_id", p.problem_id.map(Value::from));
            set("external_id", p.external_id.map(Value::from));
            set("email_ccs", p.email_ccs.map(|v| json!(v)));
            set("followers", p.followers.map(|v| json!(v)));
            set("safe_update", p.safe_update.map(Value::from));
            set("updated_stamp", p.updated_stamp.map(Value::from));
            let updated = c.update_ticket(p.ticket_id, fields).await?;
            Ok(wrapped("Ticket updated successfully", "ticket", updated))
        })
        .await
    }

    #[tool(
        description = "Search Zendesk using Zendesk Query Language (ZQL). Searches tickets, users, organizations and groups, one page at a time; Zendesk returns at most 1,000 results per query. Example queries: 'type:ticket status:open priority:urgent', 'type:ticket assignee:me', 'type:user email:john@example.com'",
        annotations(read_only_hint = true)
    )]
    async fn search(&self, Parameters(p): Parameters<SearchParams>) -> CallToolResult {
        self.call_json(|c| async move {
            c.search(
                &p.query,
                p.page,
                p.per_page,
                p.sort_by.as_deref(),
                &p.sort_order,
            )
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
            let message = job_message(&result, "Merge", "Tickets merged successfully");
            Ok(wrapped(&message, "result", result))
        })
        .await
    }

    #[tool(
        description = "Get tickets for a specific user by role (requested, assigned, ccd, or followed)",
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
        description = "Soft-delete a Zendesk ticket. It is recoverable for 30 days with restore_deleted_ticket (see list_deleted_tickets). Needs permission to delete tickets.",
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

    #[tool(
        description = "Count tickets: all tickets, or those matching a ZQL query (include type:ticket, since a search counts users and organizations too). A cheap way to size a result set before searching.",
        annotations(read_only_hint = true)
    )]
    async fn count_tickets(&self, Parameters(p): Parameters<CountTicketsParams>) -> CallToolResult {
        self.call_json(|c| async move { c.count_tickets(p.query.as_deref()).await })
            .await
    }

    #[tool(
        description = "List the followers and email CCs of a ticket (id, name, email, role). Requires the CCs and followers feature; update_ticket changes them.",
        annotations(read_only_hint = true)
    )]
    async fn get_ticket_collaborators(
        &self,
        Parameters(p): Parameters<TicketIdParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.get_ticket_collaborators(p.ticket_id).await })
            .await
    }

    #[tool(
        description = "Find problem tickets, by text in the subject or, without text, the 100 most recently updated. Use it to find a problem to link incidents to via update_ticket's problem_id.",
        annotations(read_only_hint = true)
    )]
    async fn search_problem_tickets(
        &self,
        Parameters(p): Parameters<SearchProblemTicketsParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.search_problem_tickets(p.text.as_deref()).await })
            .await
    }

    #[tool(
        description = "List the tickets of an organization with pagination, like get_tickets, with requester and assignee names",
        annotations(read_only_hint = true)
    )]
    async fn get_organization_tickets(
        &self,
        Parameters(p): Parameters<OrganizationTicketsParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            c.get_organization_tickets(p.organization_id, p.page, p.per_page)
                .await
        })
        .await
    }

    #[tool(
        description = "Add and/or remove specific tags on a ticket and return its current tags. Unlike update_ticket's tags, which replaces the whole list, this changes only the tags given.",
        annotations(destructive_hint = false, idempotent_hint = true)
    )]
    async fn update_ticket_tags(
        &self,
        Parameters(p): Parameters<UpdateTicketTagsParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            c.update_ticket_tags(
                p.ticket_id,
                &p.add.unwrap_or_default(),
                &p.remove.unwrap_or_default(),
            )
            .await
        })
        .await
    }
}
