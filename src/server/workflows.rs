use super::*;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ExecuteViewParams {
    /// The view ID to execute
    view_id: u64,
    #[serde(default = "page_1")]
    page: u64,
    #[serde(default = "per_page_25")]
    per_page: u64,
    /// Column to sort by, e.g. created_at or updated_at (see Zendesk's view columns)
    sort_by: Option<String>,
    /// asc or desc
    sort_order: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListMacrosParams {
    #[serde(default = "default_true")]
    active_only: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ApplyMacroParams {
    /// The ticket to apply the macro to
    ticket_id: u64,
    /// The macro to apply
    macro_id: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ViewCountsParams {
    /// IDs of the views to count (1 to 20)
    view_ids: Vec<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct MacroParams {
    /// The macro ID
    macro_id: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchMacrosParams {
    /// Text to match against macro titles
    query: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListTriggersParams {
    /// Only active triggers (defaults to true)
    #[serde(default = "default_true")]
    active_only: bool,
    /// Only triggers in this trigger category
    category_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TriggerParams {
    /// The trigger ID
    trigger_id: u64,
}

#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)]
enum SlaMetric {
    ReplyTime,
    FirstReplyTime,
    AgentWorkTime,
    RequesterWaitTime,
    PeriodicUpdateTime,
}

impl SlaMetric {
    fn as_str(self) -> &'static str {
        match self {
            SlaMetric::ReplyTime => "reply_time",
            SlaMetric::FirstReplyTime => "first_reply_time",
            SlaMetric::AgentWorkTime => "agent_work_time",
            SlaMetric::RequesterWaitTime => "requester_wait_time",
            SlaMetric::PeriodicUpdateTime => "periodic_update_time",
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SlaBreachesParams {
    /// Number of days to look back (default 7)
    #[serde(default = "days_back_7")]
    days_back: u64,
    /// Optional filter by metric type
    metric: Option<SlaMetric>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListCustomStatusesParams {
    /// Only active statuses (defaults to true)
    #[serde(default = "default_true")]
    active_only: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SatisfactionRatingsParams {
    /// offered, unoffered, received, received_with_comment, received_without_comment, good, good_with_comment, good_without_comment, bad, bad_with_comment, bad_without_comment
    score: Option<String>,
    /// Only ratings from the last N days (default 30)
    #[serde(default = "days_back_30")]
    days_back: u64,
    #[serde(default = "page_1")]
    page: u64,
    /// Ratings per page (max 100)
    #[serde(default = "per_page_25")]
    per_page: u64,
}

#[tool_router(router = workflows_router, vis = "pub(super)")]
impl ZendeskServer {
    #[tool(
        description = "List all available Zendesk views (saved ticket queues)",
        annotations(read_only_hint = true)
    )]
    async fn list_views(&self) -> CallToolResult {
        self.call_json(|c| async move { c.list_views().await })
            .await
    }

    #[tool(
        description = "Execute a Zendesk view and return its tickets",
        annotations(read_only_hint = true)
    )]
    async fn execute_view(&self, Parameters(p): Parameters<ExecuteViewParams>) -> CallToolResult {
        self.call_json(|c| async move {
            c.execute_view(
                p.view_id,
                p.page,
                p.per_page,
                p.sort_by.as_deref(),
                p.sort_order.as_deref(),
            )
            .await
        })
        .await
    }

    #[tool(
        description = "List all ticket fields (system + custom) with their types and valid options",
        annotations(read_only_hint = true)
    )]
    async fn list_ticket_fields(&self) -> CallToolResult {
        self.call_json(|c| async move { c.list_ticket_fields().await })
            .await
    }

    #[tool(
        description = "List all ticket forms and their associated field IDs",
        annotations(read_only_hint = true)
    )]
    async fn list_ticket_forms(&self) -> CallToolResult {
        self.call_json(|c| async move { c.list_ticket_forms().await })
            .await
    }

    #[tool(
        description = "List available Zendesk macros (canned responses and actions)",
        annotations(read_only_hint = true)
    )]
    async fn list_macros(&self, Parameters(p): Parameters<ListMacrosParams>) -> CallToolResult {
        self.call_json(|c| async move { c.list_macros(p.active_only).await })
            .await
    }

    #[tool(
        description = "Preview the result of applying a macro to a ticket (does not save changes)",
        annotations(read_only_hint = true)
    )]
    async fn apply_macro(&self, Parameters(p): Parameters<ApplyMacroParams>) -> CallToolResult {
        self.call_json(|c| async move {
            let result = c.apply_macro(p.ticket_id, p.macro_id).await?;
            Ok(wrapped("Macro preview (not saved)", "result", result))
        })
        .await
    }

    #[tool(
        description = "Get ticket counts for up to 20 views at once as {view_id, value, pretty, fresh}. `value` is null while Zendesk is still computing it, so retry later. Limited to 6 calls per minute.",
        annotations(read_only_hint = true)
    )]
    async fn get_view_counts(&self, Parameters(p): Parameters<ViewCountsParams>) -> CallToolResult {
        self.call_json(|c| async move { c.get_view_counts(&p.view_ids).await })
            .await
    }

    #[tool(
        description = "Get one macro with its actions ({field, value}). Shows exactly what a macro changes before apply_macro (preview) or execute_macro (save).",
        annotations(read_only_hint = true)
    )]
    async fn get_macro(&self, Parameters(p): Parameters<MacroParams>) -> CallToolResult {
        self.call_json(|c| async move { c.get_macro(p.macro_id).await })
            .await
    }

    #[tool(
        description = "Find macros by title, with their actions. Returns one page of up to 100; list_macros returns all of them.",
        annotations(read_only_hint = true)
    )]
    async fn search_macros(&self, Parameters(p): Parameters<SearchMacrosParams>) -> CallToolResult {
        self.call_json(|c| async move { c.search_macros(&p.query).await })
            .await
    }

    #[tool(
        description = "Apply a macro to a ticket for real: its changes and comment are saved and the macro is recorded in the ticket audit. apply_macro only previews. Returns the updated ticket.",
        annotations(destructive_hint = false)
    )]
    async fn execute_macro(&self, Parameters(p): Parameters<ApplyMacroParams>) -> CallToolResult {
        self.call_json(|c| async move {
            let ticket = c.execute_macro(p.ticket_id, p.macro_id).await?;
            Ok(wrapped("Macro applied", "ticket", ticket))
        })
        .await
    }

    #[tool(
        description = "List ticket triggers, the business rules that change tickets automatically (active ones by default). Use with get_trigger to explain changes seen in get_ticket_audits.",
        annotations(read_only_hint = true)
    )]
    async fn list_triggers(&self, Parameters(p): Parameters<ListTriggersParams>) -> CallToolResult {
        self.call_json(|c| async move {
            c.list_triggers(p.active_only, p.category_id.as_deref())
                .await
        })
        .await
    }

    #[tool(
        description = "Get one ticket trigger with its conditions ({all, any}) and actions.",
        annotations(read_only_hint = true)
    )]
    async fn get_trigger(&self, Parameters(p): Parameters<TriggerParams>) -> CallToolResult {
        self.call_json(|c| async move { c.get_trigger(p.trigger_id).await })
            .await
    }

    #[tool(
        description = "Find tickets that breached SLA within a specified time period",
        annotations(read_only_hint = true)
    )]
    async fn get_sla_breaches(
        &self,
        Parameters(p): Parameters<SlaBreachesParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            c.get_sla_breaches(p.days_back, p.metric.map(SlaMetric::as_str))
                .await
        })
        .await
    }

    #[tool(
        description = "Get all SLA policies with their metric targets per priority level",
        annotations(read_only_hint = true)
    )]
    async fn get_sla_policies(&self) -> CallToolResult {
        self.call_json(|c| async move { c.get_sla_policies().await })
            .await
    }

    #[tool(
        description = "List the custom ticket statuses with their labels and status category. Maps the custom_status_id on tickets to labels; a ticket's 'status' is only the category.",
        annotations(read_only_hint = true)
    )]
    async fn list_custom_statuses(
        &self,
        Parameters(p): Parameters<ListCustomStatusesParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.list_custom_statuses(p.active_only).await })
            .await
    }

    #[tool(
        description = "List CSAT satisfaction ratings (score, comment, reason, ticket and agent IDs) from the last days_back days, optionally filtered by score. Admin-only: agents get 403.",
        annotations(read_only_hint = true)
    )]
    async fn list_satisfaction_ratings(
        &self,
        Parameters(p): Parameters<SatisfactionRatingsParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            c.list_satisfaction_ratings(p.score.as_deref(), p.days_back, p.page, p.per_page)
                .await
        })
        .await
    }
}
