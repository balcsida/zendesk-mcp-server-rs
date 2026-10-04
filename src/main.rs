mod auth;
mod auth_cli;
mod config;
mod mobile_auth;
mod oauth;
mod server;
mod tokens;
mod zendesk;

use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

/// Model Context Protocol server for Zendesk.
///
/// With no subcommand the server runs, speaking MCP over stdio (or over streamable
/// HTTP with --http). Configuration comes from the environment and a .env file in the
/// working directory or any parent.
#[derive(Parser)]
#[command(name = "zendesk-mcp-server", version, about)]
struct Cli {
    #[command(flatten)]
    serve: server::ServeArgs,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Authorize this machine with Zendesk OAuth (PKCE) and store the tokens for the server.
    Auth {
        /// Paste the redirect URL instead of running a local callback server.
        #[arg(long)]
        manual: bool,
    },
    /// Sign in through the Zendesk mobile app's OAuth flow (no OAuth client needed).
    MobileAuth,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    // stdout is the MCP channel in stdio mode, so logs go to stderr.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let cli = Cli::parse();
    let http = reqwest::Client::builder()
        .user_agent(concat!("zendesk-mcp-server/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()?;

    match cli.command {
        Some(Command::Auth { manual }) => {
            let code = auth_cli::run(http, manual).await?;
            std::process::exit(code);
        }
        Some(Command::MobileAuth) => mobile_auth::run_auth_cli(http).await,
        None => server::run(cli.serve, http).await,
    }
}
