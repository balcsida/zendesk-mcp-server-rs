mod server;

use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

/// Model Context Protocol server for Zendesk.
///
/// Without a subcommand it serves over stdio. Configuration comes from the environment
/// and a .env file in the working directory.
#[derive(Parser)]
#[command(name = "zendesk-mcp-server", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve MCP over stdin/stdout (the default when no subcommand is given).
    Stdio,
    /// Serve MCP over streamable HTTP at /mcp, protected by a bearer token.
    Http(server::HttpArgs),
    /// Authorize this machine with Zendesk OAuth (PKCE) and store the tokens for the server.
    Auth {
        /// Paste the redirect URL instead of running a local callback server.
        #[arg(long)]
        manual: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::from_path(".env").ok();
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
        .redirect(zendesk::redirect_policy())
        .build()?;

    match cli.command.unwrap_or(Command::Stdio) {
        Command::Stdio => server::run(server::Transport::Stdio, http).await,
        Command::Http(args) => server::run(server::Transport::Http(args), http).await,
        Command::Auth { manual } => {
            let code = zendesk::authorize::run(http, manual).await?;
            std::process::exit(code);
        }
    }
}
