//! The `zendesk-mcp-server` binary: command-line parsing and startup.

mod server;

use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

/// Unofficial Model Context Protocol server for the Zendesk API. Not affiliated with Zendesk, Inc.
///
/// Without a subcommand it serves over stdio. Configuration comes from the environment
/// and a .env file in the working directory.
#[derive(Parser)]
#[command(name = "zendesk-mcp-server", version, about)]
struct Cli {
    /// List and run only the tools that do not write. Accepts true/false, yes/no, on/off or 1/0.
    #[arg(long, global = true, env = "MCP_READ_ONLY", value_parser = clap::builder::BoolishValueParser::new())]
    read_only: bool,

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
    /// Install or list the bundled agent skills that teach AI coding agents to use this server and the CLI.
    Skills {
        #[command(subcommand)]
        command: zendesk::skills::Command,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    match dotenvy::from_path(".env") {
        Err(e) if !e.not_found() => return Err(e).context("Could not load .env"),
        _ => {}
    }
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
        Command::Stdio => server::run(server::Transport::Stdio, http, cli.read_only).await,
        Command::Http(args) => {
            server::run(server::Transport::Http(args), http, cli.read_only).await
        }
        Command::Skills { command } => zendesk::skills::run(command),
        Command::Auth { manual } => {
            let code = zendesk::authorize::run(http, manual).await?;
            std::process::exit(code);
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn read_only_flag_works_with_and_without_a_subcommand() {
        for args in [
            &["zendesk-mcp-server", "--read-only"][..],
            &["zendesk-mcp-server", "--read-only", "stdio"],
            &["zendesk-mcp-server", "stdio", "--read-only"],
            &[
                "zendesk-mcp-server",
                "http",
                "--bearer-token",
                "0123456789abcdef",
                "--read-only",
            ],
            &["zendesk-mcp-server", "--read-only", "http"],
        ] {
            assert!(Cli::try_parse_from(args).unwrap().read_only, "{args:?}");
        }
    }
}
