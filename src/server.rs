//! The MCP server: tools, prompts, the knowledge-base resource, and the two transports.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use axum::response::IntoResponse;
use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, ServiceExt, prompt, prompt_handler,
    prompt_router, schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, OnceCell};
use tokio_util::sync::CancellationToken;
use tower_http::validate_request::ValidateRequestHeaderLayer;

use crate::config::Credentials;
use crate::zendesk::ZendeskClient;

/// Options for the `http` subcommand.
#[derive(Debug, Clone, clap::Args)]
pub struct HttpArgs {
    /// Address to listen on. The MCP endpoint is http://ADDR/mcp.
    #[arg(
        long,
        env = "MCP_HTTP_ADDR",
        value_name = "ADDR",
        default_value = "127.0.0.1:8080"
    )]
    pub bind: SocketAddr,

    /// Bearer token MCP clients must present. Required, so the Zendesk credentials are
    /// not exposed to anyone who can reach the port.
    #[arg(
        long,
        env = "MCP_BEARER_TOKEN",
        hide_env_values = true,
        value_name = "TOKEN"
    )]
    pub bearer_token: Option<String>,
}

/// How the server talks to its MCP client.
#[derive(Debug, Clone)]
pub enum Transport {
    Stdio,
    Http(HttpArgs),
}

#[derive(Clone)]
pub struct ZendeskServer {
    credentials: Arc<Credentials>,
    http: reqwest::Client,
    /// Built on first use so a configuration or sign-in problem surfaces as a tool
    /// error the MCP client can display, and is retried on the next call.
    client: Arc<OnceCell<Arc<ZendeskClient>>>,
    /// The knowledge base with the time it was fetched; reused for [`KB_TTL`].
    kb_cache: Arc<Mutex<Option<(Instant, Value)>>>,
    tool_router: ToolRouter<ZendeskServer>,
    prompt_router: PromptRouter<ZendeskServer>,
}

const KB_TTL: Duration = Duration::from_secs(3600);
const KB_URI: &str = "zendesk://knowledge-base";

const INSTRUCTIONS: &str = "Zendesk server: read and manage tickets, users, organizations, views, macros, SLA data and Help Center articles. get_ticket_comments returns attachment URLs that can be passed to get_ticket_attachment to fetch the file.";

const TICKET_ANALYSIS_TEMPLATE: &str = "
You are a helpful Zendesk support analyst. You've been asked to analyze ticket #{ticket_id}.

Please fetch the ticket info and comments to analyze it and provide:
1. A summary of the issue
2. The current status and timeline
3. Key points of interaction

Remember to be professional and focus on actionable insights.
";

const COMMENT_DRAFT_TEMPLATE: &str = "
You are a helpful Zendesk support agent. You need to draft a response to ticket #{ticket_id}.

Please fetch the ticket info, comments and knowledge base to draft a professional and helpful response that:
1. Acknowledges the customer's concern
2. Addresses the specific issues raised
3. Provides clear next steps or ask for specific details need to proceed
4. Maintains a friendly and professional tone
5. Ask for confirmation before commenting on the ticket

The response should be formatted well and ready to be posted as a comment.
";

impl ZendeskServer {
    pub fn new(credentials: Credentials, http: reqwest::Client) -> Self {
        ZendeskServer {
            credentials: Arc::new(credentials),
            http,
            client: Arc::new(OnceCell::new()),
            kb_cache: Arc::new(Mutex::new(None)),
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
        }
    }

    /// The shared client, authenticating on first use.
    pub async fn client(&self) -> Result<Arc<ZendeskClient>> {
        self.client
            .get_or_try_init(|| async {
                let (subdomain, auth) =
                    crate::auth::Auth::from_credentials(&self.credentials, &self.http).await?;
                Ok(Arc::new(ZendeskClient::new(
                    &subdomain,
                    auth,
                    self.http.clone(),
                )))
            })
            .await
            .cloned()
    }
}

fn page_1() -> u64 {
    1
}
fn per_page_25() -> u64 {
    25
}
fn sort_by_created_at() -> String {
    "created_at".into()
}
fn sort_by_relevance() -> String {
    "relevance".into()
}
fn sort_desc() -> String {
    "desc".into()
}
fn role_requested() -> String {
    "requested".into()
}
fn default_true() -> bool {
    true
}
fn days_back_7() -> u64 {
    7
}
fn default_target_comment() -> String {
    "Merged from related tickets.".into()
}
fn default_source_comment() -> String {
    "This ticket has been merged.".into()
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
struct UserIdParams {
    /// The user ID
    user_id: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchUsersParams {
    /// Name, email, or external_id to search for
    query: String,
}

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
struct OrganizationIdParams {
    /// The organization ID
    organization_id: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchOrganizationsParams {
    /// Organization name to search for
    query: String,
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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchArticlesParams {
    /// Search query string to find relevant articles
    query: String,
    /// Optional locale filter (e.g., 'en-us', 'fr', 'es')
    locale: Option<String>,
    /// Number of results per page (max 100)
    #[serde(default = "per_page_25")]
    per_page: u64,
    /// Page number (1-based)
    #[serde(default = "page_1")]
    page: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ArticleParams {
    /// The ID of the article to retrieve
    article_id: u64,
    /// Optional locale (e.g., 'en-us', 'fr', 'es')
    locale: Option<String>,
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

/// MCP sends prompt arguments as strings; accept an integer too.
fn ticket_id_from_string_or_int<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrInt {
        Int(u64),
        Str(String),
    }
    match StringOrInt::deserialize(d)? {
        StringOrInt::Int(n) => Ok(n),
        StringOrInt::Str(s) => s.trim().parse().map_err(serde::de::Error::custom),
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PromptArgs {
    /// The ID of the ticket
    #[serde(deserialize_with = "ticket_id_from_string_or_int")]
    ticket_id: u64,
}

fn json_block(value: &Value) -> Result<ContentBlock> {
    Ok(ContentBlock::text(serde_json::to_string_pretty(value)?))
}

fn wrapped(message: &str, key: &str, value: Value) -> Value {
    json!({ "message": message, key: value })
}

fn prompt_result(template: &str, description: &str, ticket_id: u64) -> GetPromptResult {
    let text = template
        .replace("{ticket_id}", &ticket_id.to_string())
        .trim()
        .to_string();
    GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)])
        .with_description(format!("{description} #{ticket_id}"))
}

impl ZendeskServer {
    /// Run a tool body against the client; any error becomes an `isError` tool result.
    async fn call<Fut>(&self, f: impl FnOnce(Arc<ZendeskClient>) -> Fut) -> CallToolResult
    where
        Fut: Future<Output = Result<ContentBlock>>,
    {
        let result = async { f(self.client().await?).await }.await;
        match result {
            Ok(block) => CallToolResult::success(vec![block]),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(format!("Error: {e:#}"))]),
        }
    }

    /// Like [`Self::call`] for tools whose result is pretty-printed JSON.
    async fn call_json<Fut>(&self, f: impl FnOnce(Arc<ZendeskClient>) -> Fut) -> CallToolResult
    where
        Fut: Future<Output = Result<Value>>,
    {
        self.call(|c| async move { json_block(&f(c).await?) }).await
    }

    /// The knowledge base, fetched at most once per [`KB_TTL`].
    async fn knowledge_base(&self) -> Result<Value> {
        let mut cache = self.kb_cache.lock().await;
        if let Some((fetched, kb)) = cache.as_ref()
            && fetched.elapsed() < KB_TTL
        {
            return Ok(kb.clone());
        }
        let kb = self.client().await?.get_all_articles().await?;
        *cache = Some((Instant::now(), kb.clone()));
        Ok(kb)
    }
}

#[tool_router]
impl ZendeskServer {
    #[tool(description = "Retrieve a Zendesk ticket by its ID")]
    async fn get_ticket(&self, Parameters(p): Parameters<TicketIdParams>) -> CallToolResult {
        self.call_json(|c| async move { c.get_ticket(p.ticket_id).await })
            .await
    }

    #[tool(description = "Create a new Zendesk ticket")]
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

    #[tool(description = "Fetch the latest tickets with pagination support")]
    async fn get_tickets(&self, Parameters(p): Parameters<GetTicketsParams>) -> CallToolResult {
        self.call_json(|c| async move {
            c.get_tickets(p.page, p.per_page, &p.sort_by, &p.sort_order)
                .await
        })
        .await
    }

    #[tool(description = "Retrieve all comments for a Zendesk ticket by its ID")]
    async fn get_ticket_comments(
        &self,
        Parameters(p): Parameters<TicketIdParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.get_ticket_comments(p.ticket_id).await })
            .await
    }

    #[tool(description = "Create a new comment on an existing Zendesk ticket")]
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
        description = "Fetch a Zendesk ticket attachment by its content_url and return the file as base64-encoded data. Use the attachment URLs returned by get_ticket_comments."
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
        description = "Update fields on an existing Zendesk ticket (e.g., status, priority, assignee_id)"
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
        description = "Search Zendesk using Zendesk Query Language (ZQL). Searches tickets, users, and organizations. Example queries: 'type:ticket status:open priority:urgent', 'type:ticket assignee:me', 'type:user email:john@example.com'"
    )]
    async fn search(&self, Parameters(p): Parameters<SearchParams>) -> CallToolResult {
        self.call_json(|c| async move {
            c.search(&p.query, p.page, p.per_page, &p.sort_by, &p.sort_order)
                .await
        })
        .await
    }

    #[tool(
        description = "Get a Zendesk user by their ID. Use this to resolve requester_id or assignee_id from tickets."
    )]
    async fn get_user(&self, Parameters(p): Parameters<UserIdParams>) -> CallToolResult {
        self.call_json(|c| async move { c.get_user(p.user_id).await })
            .await
    }

    #[tool(description = "Get the currently authenticated Zendesk user")]
    async fn get_current_user(&self) -> CallToolResult {
        self.call_json(|c| async move { c.get_current_user().await })
            .await
    }

    #[tool(description = "Search Zendesk users by name, email, or external_id")]
    async fn search_users(&self, Parameters(p): Parameters<SearchUsersParams>) -> CallToolResult {
        self.call_json(|c| async move { c.search_users(&p.query).await })
            .await
    }

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

    #[tool(description = "Get a Zendesk organization by its ID")]
    async fn get_organization(
        &self,
        Parameters(p): Parameters<OrganizationIdParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.get_organization(p.organization_id).await })
            .await
    }

    #[tool(description = "Search Zendesk organizations by name")]
    async fn search_organizations(
        &self,
        Parameters(p): Parameters<SearchOrganizationsParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.search_organizations(&p.query).await })
            .await
    }

    #[tool(description = "Fetch multiple tickets by IDs in a single request (max 100)")]
    async fn get_tickets_bulk(
        &self,
        Parameters(p): Parameters<TicketsBulkParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.get_tickets_bulk(&p.ticket_ids).await })
            .await
    }

    #[tool(description = "List assignable Zendesk groups for ticket routing")]
    async fn list_groups(&self) -> CallToolResult {
        self.call_json(|c| async move { c.list_groups().await })
            .await
    }

    #[tool(description = "Merge source tickets into a target ticket")]
    async fn merge_tickets(&self, Parameters(p): Parameters<MergeTicketsParams>) -> CallToolResult {
        self.call_json(|c| async move {
            let result = c
                .merge_tickets(
                    p.target_id,
                    &p.source_ids,
                    &p.target_comment,
                    &p.source_comment,
                )
                .await?;
            Ok(wrapped("Tickets merged successfully", "result", result))
        })
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

    #[tool(description = "Get tickets for a specific user by role (requested, assigned, or ccd)")]
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

    #[tool(description = "List all ticket forms and their associated field IDs")]
    async fn list_ticket_forms(&self) -> CallToolResult {
        self.call_json(|c| async move { c.list_ticket_forms().await })
            .await
    }

    #[tool(description = "Permanently delete a Zendesk ticket. Use with caution.")]
    async fn delete_ticket(&self, Parameters(p): Parameters<TicketIdParams>) -> CallToolResult {
        self.call_json(|c| async move {
            c.delete_ticket(p.ticket_id).await?;
            Ok(json!({ "message": format!("Ticket {} deleted successfully", p.ticket_id) }))
        })
        .await
    }

    #[tool(description = "Search Zendesk help center articles by query string")]
    async fn search_articles(
        &self,
        Parameters(p): Parameters<SearchArticlesParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            c.search_articles(&p.query, p.locale.as_deref(), p.per_page, p.page)
                .await
        })
        .await
    }

    #[tool(description = "Get a specific Zendesk help center article by its ID")]
    async fn get_article(&self, Parameters(p): Parameters<ArticleParams>) -> CallToolResult {
        self.call_json(|c| async move { c.get_article(p.article_id, p.locale.as_deref()).await })
            .await
    }

    #[tool(
        description = "Get performance/SLA metrics for a specific ticket (reply time, resolution time, wait times, etc.)"
    )]
    async fn get_ticket_metrics(
        &self,
        Parameters(p): Parameters<TicketIdParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.get_ticket_metrics(p.ticket_id).await })
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

#[prompt_router]
impl ZendeskServer {
    /// Analyze a Zendesk ticket and provide insights
    #[prompt(name = "analyze-ticket")]
    async fn analyze_ticket(
        &self,
        Parameters(args): Parameters<PromptArgs>,
    ) -> Result<GetPromptResult, McpError> {
        Ok(prompt_result(
            TICKET_ANALYSIS_TEMPLATE,
            "Analysis prompt for ticket",
            args.ticket_id,
        ))
    }

    /// Draft a professional response to a Zendesk ticket
    #[prompt(name = "draft-ticket-response")]
    async fn draft_ticket_response(
        &self,
        Parameters(args): Parameters<PromptArgs>,
    ) -> Result<GetPromptResult, McpError> {
        Ok(prompt_result(
            COMMENT_DRAFT_TEMPLATE,
            "Response draft prompt for ticket",
            args.ticket_id,
        ))
    }
}

#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for ZendeskServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new(
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(INSTRUCTIONS)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            resources: vec![
                Resource::new(KB_URI, "Zendesk Knowledge Base")
                    .with_description("Access to Zendesk Help Center articles and sections")
                    .with_mime_type("application/json"),
            ],
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        if request.uri != KB_URI {
            return Err(McpError::resource_not_found(
                "resource_not_found",
                Some(json!({ "uri": request.uri })),
            ));
        }
        let kb = self.knowledge_base().await.map_err(|e| {
            tracing::error!("Error fetching knowledge base: {e:#}");
            McpError::internal_error(format!("{e:#}"), None)
        })?;
        let sections = kb.as_object().map_or(0, |s| s.len());
        let total_articles: usize = kb.as_object().map_or(0, |s| {
            s.values()
                .map(|section| section["articles"].as_array().map_or(0, |a| a.len()))
                .sum()
        });
        let body = json!({
            "knowledge_base": kb,
            "metadata": { "sections": sections, "total_articles": total_articles },
        });
        let text = serde_json::to_string_pretty(&body)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(ReadResourceResult::new(vec![ResourceContents::text(text, request.uri)]).into())
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `/mcp` requires the bearer token; `/healthz` is open for load balancers.
fn http_router(server: ZendeskServer, bearer_token: &str, ct: CancellationToken) -> axum::Router {
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        LocalSessionManager::default().into(),
        // rmcp accepts only loopback `Host` headers by default, a DNS-rebinding guard for
        // unauthenticated local servers. This one is bearer-protected and meant to be
        // reached by name, so accept any `Host`.
        StreamableHttpServerConfig::default()
            .with_cancellation_token(ct.child_token())
            .disable_allowed_hosts(),
    );
    let expected = bearer_token.to_string();
    axum::Router::new()
        .nest_service("/mcp", service)
        .layer(ValidateRequestHeaderLayer::custom(
            #[allow(clippy::result_large_err)]
            move |req: &mut axum::http::Request<axum::body::Body>| {
                let presented = req
                    .headers()
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.split_once(' '))
                    .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
                    .map(|(_, token)| token.trim());
                match presented {
                    Some(t) if constant_time_eq(t.as_bytes(), expected.as_bytes()) => Ok(()),
                    _ => Err((
                        axum::http::StatusCode::UNAUTHORIZED,
                        [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
                    )
                        .into_response()),
                }
            },
        ))
        .merge(axum::Router::new().route("/healthz", axum::routing::get(|| async { "ok" })))
}

/// Authenticate up front (so a browser sign-in happens at startup, not mid-call), then
/// serve over the chosen transport.
pub async fn run(transport: Transport, http: reqwest::Client) -> Result<()> {
    let credentials = crate::config::load_credentials()?;
    let server = ZendeskServer::new(credentials, http);
    if let Err(e) = server.client().await {
        tracing::error!("Zendesk authentication failed: {e:#}");
    }

    let args = match transport {
        Transport::Stdio => {
            tracing::info!("Serving MCP over stdio");
            server
                .serve(rmcp::transport::stdio())
                .await?
                .waiting()
                .await?;
            return Ok(());
        }
        Transport::Http(args) => args,
    };

    let Some(token) = args.bearer_token else {
        bail!(
            "MCP_BEARER_TOKEN (or --bearer-token) is required for the http transport so the Zendesk credentials are not exposed to anyone who can reach the port."
        );
    };
    let ct = CancellationToken::new();
    let router = http_router(server, &token, ct.clone());
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    tracing::info!(
        "Serving MCP over HTTP at http://{}/mcp",
        listener.local_addr()?
    );
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            ct.cancel();
        })
        .await?;
    Ok(())
}

/// Ctrl-C, or SIGTERM (what `docker stop` sends to PID 1).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                term.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOOLS: [&str; 29] = [
        "get_ticket",
        "create_ticket",
        "get_tickets",
        "get_ticket_comments",
        "create_ticket_comment",
        "get_ticket_attachment",
        "update_ticket",
        "search",
        "get_user",
        "get_current_user",
        "search_users",
        "list_views",
        "execute_view",
        "list_ticket_fields",
        "get_organization",
        "search_organizations",
        "get_tickets_bulk",
        "list_groups",
        "merge_tickets",
        "list_macros",
        "apply_macro",
        "get_user_tickets",
        "list_ticket_forms",
        "delete_ticket",
        "search_articles",
        "get_article",
        "get_ticket_metrics",
        "get_sla_breaches",
        "get_sla_policies",
    ];

    fn server() -> ZendeskServer {
        ZendeskServer::new(
            Credentials::Bearer {
                subdomain: "acme".into(),
                access_token: "secret-token".into(),
            },
            reqwest::Client::new(),
        )
    }

    #[test]
    fn lists_exactly_the_29_tools() {
        let mut names: Vec<String> = ZendeskServer::tool_router()
            .list_all()
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        names.sort();
        let mut expected: Vec<String> = TOOLS.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(names, expected);
    }

    #[test]
    fn tool_schemas_are_objects_and_get_ticket_requires_ticket_id() {
        for tool in ZendeskServer::tool_router().list_all() {
            assert_eq!(tool.input_schema["type"], "object", "{}", tool.name);
            if tool.name == "get_ticket" {
                assert_eq!(tool.input_schema["required"], json!(["ticket_id"]));
            }
        }
    }

    #[test]
    fn lists_both_prompts_with_required_ticket_id() {
        let prompts = ZendeskServer::prompt_router().list_all();
        let mut names: Vec<_> = prompts.iter().map(|p| p.name.to_string()).collect();
        names.sort();
        assert_eq!(names, ["analyze-ticket", "draft-ticket-response"]);
        for p in prompts {
            let args = p.arguments.expect("arguments");
            assert_eq!(args.len(), 1);
            assert_eq!(args[0].name, "ticket_id");
            assert_eq!(args[0].required, Some(true));
        }
    }

    #[test]
    fn prompt_args_accept_string_and_integer_ids() {
        let from_str: PromptArgs = serde_json::from_value(json!({ "ticket_id": "42" })).unwrap();
        let from_int: PromptArgs = serde_json::from_value(json!({ "ticket_id": 42 })).unwrap();
        for args in [from_str, from_int] {
            let result = prompt_result(
                TICKET_ANALYSIS_TEMPLATE,
                "Analysis prompt for ticket",
                args.ticket_id,
            );
            assert_eq!(
                result.description.as_deref(),
                Some("Analysis prompt for ticket #42")
            );
            let text = serde_json::to_string(&result.messages).unwrap();
            assert!(text.contains("ticket #42"));
            assert!(!text.contains("{ticket_id}"));
        }
    }

    #[test]
    fn prompt_args_reject_non_numeric_id() {
        assert!(serde_json::from_value::<PromptArgs>(json!({ "ticket_id": "abc" })).is_err());
    }

    #[test]
    fn info_enables_tools_prompts_and_resources() {
        let caps = server().get_info().capabilities;
        assert!(caps.tools.is_some());
        assert!(caps.prompts.is_some());
        assert!(caps.resources.is_some());
    }

    #[tokio::test]
    async fn http_router_requires_bearer_on_mcp_only() {
        let router = http_router(server(), "right-token", CancellationToken::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await });

        let http = reqwest::Client::new();
        let health = http
            .get(format!("http://{addr}/healthz"))
            .send()
            .await
            .unwrap();
        assert_eq!(health.status(), 200);
        assert_eq!(health.text().await.unwrap(), "ok");

        let url = format!("http://{addr}/mcp");
        let anonymous = http.post(&url).body("{}").send().await.unwrap();
        assert_eq!(anonymous.status(), 401);
        assert_eq!(
            anonymous
                .headers()
                .get("www-authenticate")
                .and_then(|v| v.to_str().ok()),
            Some("Bearer")
        );

        let wrong = http
            .post(&url)
            .bearer_auth("wrong")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(wrong.status(), 401);

        // The scheme is case-insensitive (RFC 7235).
        let lowercase_scheme = http
            .post(&url)
            .header("authorization", "bearer right-token")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_ne!(lowercase_scheme.status(), 401);

        // Remote deployments reach the server by name, which rmcp rejects by default.
        let by_name = http
            .post(&url)
            .bearer_auth("right-token")
            .header("host", "mcp.example.com")
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body("not json")
            .send()
            .await
            .unwrap();
        assert_ne!(by_name.status(), 403);
        assert_ne!(by_name.status(), 401);

        let authorized = http
            .post(&url)
            .bearer_auth("right-token")
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body("not json")
            .send()
            .await
            .unwrap();
        assert_ne!(authorized.status(), 401);
    }
}
