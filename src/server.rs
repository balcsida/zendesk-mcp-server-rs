//! The MCP server: tools, prompts, the knowledge-base resource, and the two transports.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use rmcp::ServerHandler;
use rmcp::model::*;
use tokio::sync::OnceCell;

use crate::config::Credentials;
use crate::zendesk::ZendeskClient;

/// Command-line / environment options for serving.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct ServeArgs {
    /// Serve over streamable HTTP on this address (for example 0.0.0.0:8080) instead of
    /// stdio. Requires MCP_BEARER_TOKEN.
    #[arg(long, env = "MCP_HTTP_ADDR", value_name = "ADDR")]
    pub http: Option<SocketAddr>,

    /// Bearer token MCP clients must present when serving over HTTP.
    #[arg(
        long,
        env = "MCP_BEARER_TOKEN",
        hide_env_values = true,
        value_name = "TOKEN"
    )]
    pub bearer_token: Option<String>,
}

#[derive(Clone)]
pub struct ZendeskServer {
    credentials: Arc<Credentials>,
    http: reqwest::Client,
    /// Built on first use so a configuration or sign-in problem surfaces as a tool
    /// error the MCP client can display, and is retried on the next call.
    client: Arc<OnceCell<Arc<ZendeskClient>>>,
}

impl ZendeskServer {
    pub fn new(credentials: Credentials, http: reqwest::Client) -> Self {
        ZendeskServer {
            credentials: Arc::new(credentials),
            http,
            client: Arc::new(OnceCell::new()),
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

impl ServerHandler for ZendeskServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::from_build_env())
    }
}

/// Authenticate up front (so a browser sign-in happens at startup, not mid-call), then
/// serve over stdio or streamable HTTP.
pub async fn run(args: ServeArgs, http: reqwest::Client) -> Result<()> {
    let _ = (args, http);
    todo!("worker: server")
}
