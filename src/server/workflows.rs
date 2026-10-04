use super::*;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ExecuteViewParams {
    /// The view ID to execute
    view_id: u64,
    #[serde(default = "page_1")]
    page: u64,
    #[serde(default = "per_page_25")]
    per_page: u64,
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

#[tool_router(router = workflows_router, vis = "pub(super)")]
impl ZendeskServer {
    #[tool(description = "List all available Zendesk views (saved ticket queues)")]
    async fn list_views(&self) -> CallToolResult {
        self.call_json(|c| async move { c.list_views().await })
            .await
    }

    #[tool(description = "Execute a Zendesk view and return its tickets")]
    async fn execute_view(&self, Parameters(p): Parameters<ExecuteViewParams>) -> CallToolResult {
        self.call_json(|c| async move { c.execute_view(p.view_id, p.page, p.per_page).await })
            .await
    }

    #[tool(
        description = "List all ticket fields (system + custom) with their types and valid options"
    )]
    async fn list_ticket_fields(&self) -> CallToolResult {
        self.call_json(|c| async move { c.list_ticket_fields().await })
            .await
    }

    #[tool(description = "List all ticket forms and their associated field IDs")]
    async fn list_ticket_forms(&self) -> CallToolResult {
        self.call_json(|c| async move { c.list_ticket_forms().await })
            .await
    }

    #[tool(description = "List available Zendesk macros (canned responses and actions)")]
    async fn list_macros(&self, Parameters(p): Parameters<ListMacrosParams>) -> CallToolResult {
        self.call_json(|c| async move { c.list_macros(p.active_only).await })
            .await
    }

    #[tool(
        description = "Preview the result of applying a macro to a ticket (does not save changes)"
    )]
    async fn apply_macro(&self, Parameters(p): Parameters<ApplyMacroParams>) -> CallToolResult {
        self.call_json(|c| async move {
            let result = c.apply_macro(p.ticket_id, p.macro_id).await?;
            Ok(wrapped("Macro preview (not saved)", "result", result))
        })
        .await
    }

    #[tool(description = "Find tickets that breached SLA within a specified time period")]
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

    #[tool(description = "Get all SLA policies with their metric targets per priority level")]
    async fn get_sla_policies(&self) -> CallToolResult {
        self.call_json(|c| async move { c.get_sla_policies().await })
            .await
    }
}
