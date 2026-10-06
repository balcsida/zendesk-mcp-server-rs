//! The MCP server: tools, prompts, the knowledge-base resource, and the two transports.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use axum::response::IntoResponse;
use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
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
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tower_http::validate_request::ValidateRequestHeaderLayer;

use zendesk::auth::Auth;
use zendesk::config::{self, Credentials};
use zendesk::{ArticleSearch, ZendeskClient};

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

    /// Bearer token MCP clients must present. Required unless --per-user-auth, so the
    /// server's Zendesk login is not exposed to anyone who can reach the port.
    #[arg(
        long,
        env = "MCP_BEARER_TOKEN",
        hide_env_values = true,
        value_name = "TOKEN"
    )]
    pub bearer_token: Option<String>,

    /// Act as each caller: clients send their own Zendesk token as the bearer token, and
    /// Zendesk applies that user's permissions. Only ZENDESK_SUBDOMAIN is needed.
    #[arg(long, env = "MCP_PER_USER_AUTH")]
    pub per_user_auth: bool,
}

/// How the server talks to its MCP client.
#[derive(Debug, Clone)]
pub enum Transport {
    Stdio,
    Http(HttpArgs),
}

#[derive(Clone)]
pub struct ZendeskServer {
    login: Arc<Login>,
    http: reqwest::Client,
    /// The knowledge base with the time it was fetched; reused for [`KB_TTL`].
    kb_cache: Arc<Mutex<Option<(Instant, Value)>>>,
    tool_router: ToolRouter<ZendeskServer>,
    prompt_router: PromptRouter<ZendeskServer>,
}

/// Whose Zendesk login the tools act with.
enum Login {
    /// The server's own, described by the environment.
    Shared(Arc<ZendeskClient>),
    /// Each caller's own: the Zendesk token their HTTP request carries as its bearer
    /// token, held in [`CALLER`] while the request is served.
    PerUser {
        subdomain: String,
        /// `https://{subdomain}.zendesk.com/api/v2` in production; tests point it at a mock.
        base_url: String,
    },
}

tokio::task_local! {
    /// In per-user mode, the Zendesk credential of the request being served.
    static CALLER: Auth;
}

const KB_TTL: Duration = Duration::from_secs(3600);
const KB_URI: &str = "zendesk://knowledge-base";

const INSTRUCTIONS: &str = "Zendesk server. Tool families: tickets and comments; search (ZQL) with count_tickets; users, organizations, groups, brands and account settings; views, macros and triggers; custom objects; Help Center articles; SLA data; deleted and suspended tickets; satisfaction ratings.

Conventions: list tools page with page/per_page or page_size/after_cursor and report has_more. update_tickets_bulk and merge_tickets return a job result; follow it with get_job_status. update_ticket's tags replace the whole list; use update_ticket_tags to add or remove tags. Attachments: get_ticket_comments gives content_url for get_ticket_attachment; upload_attachment gives tokens to attach to comments.

Admin-only: get_sla_breaches, get_sla_policies, list_satisfaction_ratings, list_suspended_tickets. list_deleted_tickets needs a role that can view deleted tickets.

Cautions: delete_ticket, merge_tickets, mark_ticket_as_spam, redact_comment_text, make_comment_private and update_tickets_bulk are destructive and flagged as such. apply_macro only previews; execute_macro saves. create_article makes drafts.

For anything else, the whole Zendesk API is reachable: search_api_operations, then get_api_operation, then call_api_read or call_api_write.";

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

mod catalog;
mod custom_objects;
mod help_center;
mod people;
mod ticket_ops;
mod tickets;
mod workflows;

impl ZendeskServer {
    /// A server acting with its own Zendesk login, described by `credentials`.
    pub fn new(credentials: Credentials, http: reqwest::Client) -> Self {
        let (subdomain, auth) = Auth::from_credentials(&credentials, &http);
        let client = Arc::new(ZendeskClient::new(&subdomain, auth, http.clone()));
        Self::with_login(Login::Shared(client), http)
    }

    /// A server acting as each caller, with the Zendesk token their request carries.
    pub fn per_user(subdomain: String, http: reqwest::Client) -> Self {
        let base_url = format!("https://{subdomain}.zendesk.com/api/v2");
        Self::with_login(
            Login::PerUser {
                subdomain,
                base_url,
            },
            http,
        )
    }

    fn with_login(login: Login, http: reqwest::Client) -> Self {
        ZendeskServer {
            login: Arc::new(login),
            http,
            kb_cache: Arc::new(Mutex::new(None)),
            tool_router: Self::ticket_router()
                + Self::people_router()
                + Self::ticket_ops_router()
                + Self::workflows_router()
                + Self::help_center_router()
                + Self::custom_objects_router()
                + Self::catalog_router(),
            prompt_router: Self::prompt_router(),
        }
    }

    fn is_per_user(&self) -> bool {
        matches!(*self.login, Login::PerUser { .. })
    }

    /// The client to act through: in per-user mode the caller's own, otherwise the
    /// shared one.
    pub async fn client(&self) -> Result<Arc<ZendeskClient>> {
        match &*self.login {
            Login::PerUser {
                subdomain,
                base_url,
            } => {
                // Never a fallback to another login: no caller token, no call.
                let auth = CALLER.try_with(Auth::clone).map_err(|_| {
                    anyhow!("No Zendesk token: send your own as `Authorization: Bearer <token>`.")
                })?;
                Ok(Arc::new(ZendeskClient::with_base_url(
                    subdomain,
                    auth,
                    self.http.clone(),
                    base_url.clone(),
                )))
            }
            Login::Shared(client) => Ok(client.clone()),
        }
    }

    /// In per-user mode, the Zendesk token of the HTTP request behind `context`.
    fn caller(&self, context: &RequestContext<RoleServer>) -> Option<Auth> {
        if !self.is_per_user() {
            return None;
        }
        let parts = context.extensions.get::<axum::http::request::Parts>()?;
        bearer_token(&parts.headers).map(Auth::bearer)
    }
}

/// Run `fut` with `caller` as the credential [`ZendeskServer::client`] acts with.
async fn as_caller<T>(caller: Option<Auth>, fut: impl Future<Output = T>) -> T {
    match caller {
        Some(auth) => CALLER.scope(auth, fut).await,
        None => fut.await,
    }
}

pub(super) fn page_1() -> u64 {
    1
}
pub(super) fn per_page_25() -> u64 {
    25
}
pub(super) fn sort_by_created_at() -> String {
    "created_at".into()
}
pub(super) fn sort_desc() -> String {
    "desc".into()
}
pub(super) fn sort_asc() -> String {
    "asc".into()
}
pub(super) fn role_requested() -> String {
    "requested".into()
}
pub(super) fn default_true() -> bool {
    true
}
pub(super) fn days_back_7() -> u64 {
    7
}
pub(super) fn days_back_30() -> u64 {
    30
}
pub(super) fn default_target_comment() -> String {
    "Merged from related tickets.".into()
}
pub(super) fn default_source_comment() -> String {
    "This ticket has been merged.".into()
}

/// The `status` of a ticket.
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum TicketStatus {
    New,
    Open,
    Pending,
    Hold,
    Solved,
    Closed,
}

impl TicketStatus {
    fn as_str(self) -> &'static str {
        match self {
            TicketStatus::New => "new",
            TicketStatus::Open => "open",
            TicketStatus::Pending => "pending",
            TicketStatus::Hold => "hold",
            TicketStatus::Solved => "solved",
            TicketStatus::Closed => "closed",
        }
    }
}

/// The urgency of a ticket.
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum TicketPriority {
    Low,
    Normal,
    High,
    Urgent,
}

impl TicketPriority {
    fn as_str(self) -> &'static str {
        match self {
            TicketPriority::Low => "low",
            TicketPriority::Normal => "normal",
            TicketPriority::High => "high",
            TicketPriority::Urgent => "urgent",
        }
    }
}

/// The kind of a ticket.
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum TicketType {
    Problem,
    Incident,
    Question,
    Task,
}

impl TicketType {
    fn as_str(self) -> &'static str {
        match self {
            TicketType::Problem => "problem",
            TicketType::Incident => "incident",
            TicketType::Question => "question",
            TicketType::Task => "task",
        }
    }
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

pub(super) fn wrapped(message: &str, key: &str, value: Value) -> Value {
    json!({ "message": message, key: value })
}

/// The result message for a job summary: what `subject` is doing while pending, how it
/// ended if it failed or some items did, and `success` otherwise.
pub(super) fn job_message(job: &Value, subject: &str, success: &str) -> String {
    if job["pending"] == true {
        let id = job["id"].as_str().unwrap_or_default();
        format!("{subject} still running; call get_job_status with id {id}")
    } else if job["status"] == "failed" {
        format!("{subject} failed")
    } else if let Some(failures) = job["failed_count"].as_u64().filter(|n| *n > 0) {
        format!("{subject} completed with {failures} failures")
    } else {
        success.to_string()
    }
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
        // Articles can be restricted to some users, so one caller's knowledge base must
        // never be served to another.
        // ponytail: per-user mode fetches on every read; cache per caller if that is slow.
        if self.is_per_user() {
            return self.client().await?.get_all_articles().await;
        }
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
    // Written out so every tool runs as the caller; #[tool_handler] then skips its own.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let caller = self.caller(&context);
        let call = self
            .tool_router
            .call(ToolCallContext::new(self, request, context));
        as_caller(caller, call).await
    }

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
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        if request.uri != KB_URI {
            return Err(McpError::resource_not_found(
                "resource_not_found",
                Some(json!({ "uri": request.uri })),
            ));
        }
        let kb = as_caller(self.caller(&context), self.knowledge_base())
            .await
            .map_err(|e| {
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

/// The token of an `Authorization: Bearer <token>` header. The scheme is
/// case-insensitive (RFC 7235).
fn bearer_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

/// `/mcp` requires a bearer token: `shared_token`, or in per-user mode any token, since
/// it is the caller's own Zendesk token and Zendesk checks it on every call. `/healthz`
/// is open for load balancers.
fn http_router(
    server: ZendeskServer,
    shared_token: Option<&str>,
    ct: CancellationToken,
) -> axum::Router {
    // Taken from the server, not from `shared_token`, so a server acting with its own
    // Zendesk login can never be opened to any token.
    let per_user = server.is_per_user();
    let expected = shared_token.map(str::to_string);
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        LocalSessionManager::default().into(),
        // rmcp accepts only loopback `Host` headers by default, a DNS-rebinding guard for
        // unauthenticated local servers. This one is bearer-protected and meant to be
        // reached by name, so accept any `Host`.
        StreamableHttpServerConfig::default()
            .with_cancellation_token(ct.child_token())
            .disable_allowed_hosts()
            // rmcp keys a session by its id alone, so anyone holding a leaked id could read
            // that session's responses, whatever their token. In per-user mode every
            // request stands alone instead.
            .with_legacy_session_mode(!per_user),
    );
    axum::Router::new()
        .nest_service("/mcp", service)
        .layer(ValidateRequestHeaderLayer::custom(
            #[allow(clippy::result_large_err)]
            move |req: &mut axum::http::Request<axum::body::Body>| {
                let allowed = match (bearer_token(req.headers()), &expected) {
                    (Some(_), _) if per_user => true,
                    (Some(t), Some(e)) => constant_time_eq(t.as_bytes(), e.as_bytes()),
                    _ => false,
                };
                if allowed {
                    Ok(())
                } else {
                    Err((
                        axum::http::StatusCode::UNAUTHORIZED,
                        [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
                    )
                        .into_response())
                }
            },
        ))
        .merge(axum::Router::new().route("/healthz", axum::routing::get(|| async { "ok" })))
}

/// Serve over the chosen transport.
pub async fn run(transport: Transport, http: reqwest::Client) -> Result<()> {
    let args = match transport {
        Transport::Stdio => {
            let server = signed_in_server(http)?;
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

    let (server, shared_token) = if args.per_user_auth {
        if args.bearer_token.is_some() {
            bail!(
                "MCP_PER_USER_AUTH cannot be combined with MCP_BEARER_TOKEN: in per-user mode the Authorization header carries each caller's own Zendesk token."
            );
        }
        let server = ZendeskServer::per_user(config::load_subdomain()?, http);
        tracing::info!("Per-user mode: every caller acts with their own Zendesk token");
        (server, None)
    } else {
        let Some(token) = args.bearer_token else {
            bail!(
                "MCP_BEARER_TOKEN (or --bearer-token) is required for the http transport so the Zendesk credentials are not exposed to anyone who can reach the port. To have each caller use their own Zendesk token instead, set MCP_PER_USER_AUTH=true."
            );
        };
        (signed_in_server(http)?, Some(token))
    };
    let ct = CancellationToken::new();
    let router = http_router(server, shared_token.as_deref(), ct.clone());
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

/// A server acting with its own Zendesk login from the environment.
fn signed_in_server(http: reqwest::Client) -> Result<ZendeskServer> {
    let credentials = config::load_credentials()?.ok_or_else(|| {
        anyhow!(
            "No Zendesk credentials are configured. Set ZENDESK_SUBDOMAIN (for https://acme.zendesk.com, 'acme') and run `zendesk-mcp-server auth` once to sign in."
        )
    })?;
    Ok(ZendeskServer::new(credentials, http))
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

    const TOOLS: [&str; 79] = [
        "get_ticket",
        "list_categories",
        "list_sections",
        "list_article_translations",
        "create_article",
        "update_article",
        "get_view_counts",
        "get_macro",
        "search_macros",
        "execute_macro",
        "list_triggers",
        "get_trigger",
        "create_ticket",
        "get_tickets",
        "get_ticket_comments",
        "create_ticket_comment",
        "get_ticket_attachment",
        "update_ticket",
        "search",
        "search_all_tickets",
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
        "list_articles",
        "get_article",
        "get_ticket_metrics",
        "get_ticket_audits",
        "get_linked_incidents",
        "get_sla_breaches",
        "get_sla_policies",
        "get_job_status",
        "count_tickets",
        "get_ticket_collaborators",
        "search_problem_tickets",
        "get_organization_tickets",
        "update_ticket_tags",
        "list_custom_statuses",
        "list_satisfaction_ratings",
        "list_deleted_tickets",
        "restore_deleted_ticket",
        "list_suspended_tickets",
        "recover_suspended_ticket",
        "make_comment_private",
        "redact_comment_text",
        "mark_ticket_as_spam",
        "update_tickets_bulk",
        "upload_attachment",
        "get_users_bulk",
        "get_user_identities",
        "get_user_organizations",
        "create_or_update_user",
        "update_user",
        "list_organization_users",
        "update_organization",
        "get_group_members",
        "list_brands",
        "get_account_settings",
        "list_custom_objects",
        "get_custom_object",
        "search_custom_object_records",
        "get_custom_object_record",
        "search_api_operations",
        "get_api_operation",
        "call_api_read",
        "call_api_write",
    ];

    /// Every tool that only reads.
    const READ_ONLY: [&str; 59] = [
        "get_ticket",
        "list_categories",
        "list_sections",
        "list_article_translations",
        "get_view_counts",
        "get_macro",
        "search_macros",
        "list_triggers",
        "get_trigger",
        "get_tickets",
        "get_ticket_comments",
        "get_ticket_attachment",
        "search",
        "search_all_tickets",
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
        "list_macros",
        "apply_macro",
        "get_user_tickets",
        "list_ticket_forms",
        "search_articles",
        "list_articles",
        "get_article",
        "get_ticket_metrics",
        "get_ticket_audits",
        "get_linked_incidents",
        "get_sla_breaches",
        "get_sla_policies",
        "get_job_status",
        "count_tickets",
        "get_ticket_collaborators",
        "search_problem_tickets",
        "get_organization_tickets",
        "list_custom_statuses",
        "list_satisfaction_ratings",
        "list_deleted_tickets",
        "list_suspended_tickets",
        "get_users_bulk",
        "get_user_identities",
        "get_user_organizations",
        "list_organization_users",
        "get_group_members",
        "list_brands",
        "get_account_settings",
        "list_custom_objects",
        "get_custom_object",
        "search_custom_object_records",
        "get_custom_object_record",
        "search_api_operations",
        "get_api_operation",
        "call_api_read",
    ];

    /// Every tool that deletes, merges, redacts or otherwise cannot be undone.
    const DESTRUCTIVE: [&str; 7] = [
        "delete_ticket",
        "merge_tickets",
        "redact_comment_text",
        "mark_ticket_as_spam",
        "update_tickets_bulk",
        "make_comment_private",
        "call_api_write",
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
    fn lists_exactly_the_79_tools() {
        let mut names: Vec<String> = server()
            .tool_router
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
    fn annotations_follow_the_taxonomy() {
        for name in READ_ONLY.iter().chain(&DESTRUCTIVE) {
            assert!(TOOLS.contains(name), "{name} is not a tool");
        }
        for name in DESTRUCTIVE {
            assert!(!READ_ONLY.contains(&name), "{name} is in both lists");
        }
        for tool in server().tool_router.list_all() {
            let name = tool.name.as_ref();
            let hints = tool
                .annotations
                .as_ref()
                .unwrap_or_else(|| panic!("{name} has no annotations"));
            if READ_ONLY.contains(&name) {
                assert_eq!(hints.read_only_hint, Some(true), "{name}");
            } else if DESTRUCTIVE.contains(&name) {
                assert_eq!(hints.destructive_hint, Some(true), "{name}");
            } else {
                assert_ne!(hints.read_only_hint, Some(true), "{name}");
                assert_eq!(hints.destructive_hint, Some(false), "{name}");
            }
        }
    }

    #[test]
    fn readme_and_instructions_name_only_real_tools() {
        let readme = include_str!("../../../../README.md");
        for name in TOOLS {
            assert!(
                readme.contains(&format!("#### {name}\n")),
                "README has no entry for {name}"
            );
        }
        // Tokens that start like a tool name; parameter names such as per_page are not.
        const VERBS: [&str; 17] = [
            "get_", "list_", "search_", "create_", "update_", "delete_", "apply_", "execute_",
            "merge_", "mark_", "make_", "redact_", "restore_", "recover_", "upload_", "count_",
            "call_",
        ];
        for token in INSTRUCTIONS.split(|c: char| !(c.is_ascii_lowercase() || c == '_')) {
            if token.contains('_') && VERBS.iter().any(|v| token.starts_with(v)) {
                assert!(
                    TOOLS.contains(&token),
                    "INSTRUCTIONS names unknown tool {token}"
                );
            }
        }
        assert!(INSTRUCTIONS.split_whitespace().count() < 200);
    }

    #[test]
    fn ticket_enums_are_closed_sets_in_the_schemas() {
        let tools = server().tool_router.list_all();
        let schema = |name: &str| {
            let tool = tools.iter().find(|t| t.name == name).unwrap();
            serde_json::to_string(&tool.input_schema).unwrap()
        };
        for name in ["create_ticket", "update_ticket", "update_tickets_bulk"] {
            let text = schema(name);
            assert!(text.contains("\"urgent\""), "{name}");
            assert!(text.contains("\"incident\""), "{name}");
        }
        for name in [
            "update_ticket",
            "update_tickets_bulk",
            "create_ticket_comment",
        ] {
            let text = schema(name);
            assert!(
                text.contains("\"hold\"") && !text.contains("on-hold"),
                "{name}"
            );
        }
        assert!(serde_json::from_value::<TicketStatus>(json!("on-hold")).is_err());
        assert_eq!(
            serde_json::from_value::<TicketStatus>(json!("hold"))
                .unwrap()
                .as_str(),
            "hold"
        );
    }

    #[test]
    fn job_messages_follow_the_job_summary() {
        let job = |v: Value| job_message(&v, "Merge", "Merged");
        assert_eq!(
            job(json!({"id": "j1", "pending": true, "status": "working"})),
            "Merge still running; call get_job_status with id j1"
        );
        assert_eq!(
            job(json!({"status": "failed", "pending": false})),
            "Merge failed"
        );
        assert_eq!(
            job(json!({"status": "completed", "pending": false, "failed_count": 2})),
            "Merge completed with 2 failures"
        );
        assert_eq!(
            job(json!({"status": "completed", "pending": false, "failed_count": 0})),
            "Merged"
        );
    }

    #[test]
    fn tool_schemas_are_objects_and_get_ticket_requires_ticket_id() {
        for tool in server().tool_router.list_all() {
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
        let router = http_router(server(), Some("right-token"), CancellationToken::new());
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

    /// POST one JSON-RPC message to `/mcp` as the holder of `token`. Returns the session
    /// id the server answered with, and the JSON-RPC response in the SSE body, if any.
    async fn post_mcp(
        http: &reqwest::Client,
        url: &str,
        token: &str,
        message: Value,
    ) -> (Option<String>, Option<Value>) {
        let response = http
            .post(url)
            .bearer_auth(token)
            .header("accept", "application/json, text/event-stream")
            .json(&message)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "{}", response.status());
        let session = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let body = response.text().await.unwrap();
        let reply = body
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
            .find(|message| message.get("id").is_some());
        (session, reply)
    }

    #[tokio::test]
    async fn per_user_mode_calls_zendesk_with_each_callers_own_token() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Zendesk answers only when the request carries that caller's token.
        let zendesk = MockServer::start().await;
        for token in ["alice-token", "bob-token"] {
            Mock::given(method("GET"))
                .and(path("/api/v2/tickets/1.json"))
                .and(header("authorization", format!("Bearer {token}").as_str()))
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    json!({ "ticket": { "id": 1, "subject": format!("seen with {token}") } }),
                ))
                .mount(&zendesk)
                .await;
        }
        let server = ZendeskServer::with_login(
            Login::PerUser {
                subdomain: "acme".into(),
                base_url: format!("{}/api/v2", zendesk.uri()),
            },
            reqwest::Client::new(),
        );
        let router = http_router(server, None, CancellationToken::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await });
        let http = reqwest::Client::new();

        // A token is still required; Zendesk decides whether it is any good.
        let anonymous = http.post(&url).body("{}").send().await.unwrap();
        assert_eq!(anonymous.status(), 401);

        // Nor is there a standing stream to replay a caller's responses from.
        let stream = http
            .get(&url)
            .bearer_auth("alice-token")
            .send()
            .await
            .unwrap();
        assert_eq!(stream.status(), 405);

        for token in ["alice-token", "bob-token"] {
            let initialize = json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "test", "version": "0" },
                },
            });
            let (session, _) = post_mcp(&http, &url, token, initialize).await;
            // No session another caller could join: every request stands alone.
            assert_eq!(session, None);
            let call = json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": { "name": "get_ticket", "arguments": { "ticket_id": 1 } },
            });
            let (_, reply) = post_mcp(&http, &url, token, call).await;
            let reply = reply.expect("a tools/call response");
            let text = reply["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default();
            assert!(text.contains(&format!("seen with {token}")), "{reply}");
        }
    }
}
