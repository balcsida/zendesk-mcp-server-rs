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
    /// List and run only the tools that do not write. Accepts true/false, yes/no, on/off or
    /// 1/0 in any case; an empty MCP_READ_ONLY counts as unset.
    #[arg(long, global = true, env = "MCP_READ_ONLY", value_parser = read_only_value)]
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

/// Parses `--read-only` and `MCP_READ_ONLY`. Anything that is not clearly true or false
/// stops the server at startup, so a typo cannot leave the write tools on.
fn read_only_value(value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "y" | "yes" | "t" | "true" | "on" => Ok(true),
        "" | "0" | "n" | "no" | "f" | "false" | "off" => Ok(false),
        other => Err(format!("{other:?} is neither true nor false")),
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
    fn read_only_reads_mcp_read_only() {
        let cli = Cli::command();
        let arg = cli.get_arguments().find(|a| a.get_id() == "read_only");
        assert_eq!(
            arg.and_then(clap::Arg::get_env),
            Some(std::ffi::OsStr::new("MCP_READ_ONLY"))
        );
    }

    #[test]
    fn read_only_value_accepts_the_usual_true_spellings() {
        for value in ["1", "y", "Yes", "TRUE", "t", "on", " true "] {
            assert_eq!(read_only_value(value), Ok(true), "{value:?}");
        }
    }

    #[test]
    fn read_only_value_accepts_the_usual_false_spellings() {
        for value in ["0", "n", "No", "FALSE", "f", "off"] {
            assert_eq!(read_only_value(value), Ok(false), "{value:?}");
        }
    }

    #[test]
    fn read_only_value_treats_empty_as_unset() {
        assert_eq!(read_only_value(""), Ok(false));
    }

    #[test]
    fn read_only_value_refuses_anything_else() {
        assert!(read_only_value("ture").is_err());
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
