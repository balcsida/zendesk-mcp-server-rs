mod commands;
mod mobile_auth;

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use serde_json::Value;
use tracing_subscriber::EnvFilter;
use zendesk::ZendeskClient;
use zendesk::auth::Auth;
use zendesk::config;

/// Command-line client for Zendesk.
///
/// Configuration comes from the environment and a .env file in the working directory or
/// any parent. With nothing configured, not even ZENDESK_SUBDOMAIN, commands use the token
/// saved by `mobile-auth`.
#[derive(Parser)]
#[command(name = "zendesk", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Authorize this machine with Zendesk OAuth (PKCE) and store the tokens.
    Auth {
        /// Paste the redirect URL instead of running a local callback server.
        #[arg(long)]
        manual: bool,
    },
    /// Sign in through the Zendesk mobile app's OAuth flow (no OAuth client needed).
    MobileAuth,
    /// Print the bearer access token the other commands would use.
    Token {
        /// Use the token saved by mobile-auth even when other credentials are configured.
        #[arg(long)]
        mobile: bool,
    },
    /// Call the Zendesk API and print the JSON response.
    Api {
        /// Path under /api/v2/ (e.g. tickets/1.json), or an absolute URL on this account.
        path: String,
        /// HTTP method. Defaults to GET, or POST when --data is given.
        #[arg(short = 'X', long, value_parser = parse_method)]
        method: Option<reqwest::Method>,
        /// JSON request body: inline, `@file`, or `@-` for stdin.
        #[arg(short, long)]
        data: Option<String>,
        /// Query parameter as key=value; repeatable.
        #[arg(short, long, value_parser = parse_query)]
        query: Vec<(String, String)>,
    },
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    // A CLI must not log on every call; RUST_LOG overrides.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let matches = commands::command(Cli::command()).get_matches();
    let result = match commands::selected(&matches) {
        Some((op, op_matches)) => commands::run(op, op_matches).await.map(|()| 0),
        None => run(Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit())).await,
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
    }
}

fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("zendesk-cli/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()?)
}

async fn run(cli: Cli) -> Result<i32> {
    let http = http_client()?;

    match cli.command {
        Command::Auth { manual } => return zendesk::authorize::run(http, manual).await,
        Command::MobileAuth => mobile_auth::run_auth_cli(http).await?,
        Command::Token { mobile } => {
            let (_, auth) = resolve_auth(&http, mobile).await?;
            let value = auth.value().await?;
            let token = value.bearer_token().ok_or_else(|| {
                anyhow!(
                    "the configured credentials are not a bearer token (API token or session cookie)"
                )
            })?;
            println!("{token}");
        }
        Command::Api {
            path,
            method,
            data,
            query,
        } => {
            let body = data
                .map(|d| read_data(&d, &mut std::io::stdin()))
                .transpose()?;
            let method = method.unwrap_or(if body.is_some() {
                reqwest::Method::POST
            } else {
                reqwest::Method::GET
            });
            let (subdomain, auth) = resolve_auth(&http, false).await?;
            let client = ZendeskClient::new(&subdomain, auth, http);
            let value = client.api(method, &path, &query, body.as_ref()).await?;
            print_json(&value)?;
        }
    }
    Ok(0)
}

/// Print `value` as pretty JSON, nothing for null.
fn print_json(value: &Value) -> Result<()> {
    if !value.is_null() {
        println!("{}", serde_json::to_string_pretty(value)?);
    }
    Ok(())
}

/// The configured credentials, else the saved (or freshly signed-in) mobile token.
async fn resolve_auth(http: &reqwest::Client, mobile: bool) -> Result<(String, Auth)> {
    if !mobile && let Some(creds) = config::load_credentials()? {
        return Ok(Auth::from_credentials(&creds, http));
    }
    let subdomain = config::load_subdomain().ok();
    let token = mobile_auth::ensure_auth(http, subdomain.as_deref()).await?;
    Ok((token.subdomain, Auth::bearer(&token.access_token)))
}

fn parse_method(s: &str) -> Result<reqwest::Method> {
    reqwest::Method::from_bytes(s.to_ascii_uppercase().as_bytes())
        .map_err(|_| anyhow!("invalid HTTP method: {s}"))
}

/// Split `key=value` on the first `=`.
fn parse_query(s: &str) -> Result<(String, String)> {
    s.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .ok_or_else(|| anyhow!("expected key=value, got: {s}"))
}

/// Parse `--data`: inline JSON, `@file`, or `@-` to read `stdin`.
fn read_data(arg: &str, stdin: &mut impl Read) -> Result<Value> {
    let text = match arg.strip_prefix('@') {
        Some("-") => {
            let mut buf = String::new();
            stdin.read_to_string(&mut buf).context("reading stdin")?;
            buf
        }
        Some(file) => {
            std::fs::read_to_string(Path::new(file)).with_context(|| format!("reading {file}"))?
        }
        None => arg.to_string(),
    };
    serde_json::from_str(&text).map_err(|e| anyhow!("--data is not valid JSON: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_is_case_insensitive() {
        assert_eq!(parse_method("post").unwrap(), reqwest::Method::POST);
        assert_eq!(parse_method("Get").unwrap(), reqwest::Method::GET);
        assert!(parse_method("not a method").is_err());
    }

    #[test]
    fn query_splits_on_the_first_equals() {
        assert_eq!(
            parse_query("query=type:ticket status=open").unwrap(),
            ("query".into(), "type:ticket status=open".into())
        );
        assert_eq!(parse_query("k=").unwrap(), ("k".into(), String::new()));
        assert!(parse_query("novalue").is_err());
    }

    #[test]
    fn data_reads_inline_file_and_stdin() {
        let mut none = std::io::empty();
        assert_eq!(
            read_data(r#"{"a":1}"#, &mut none).unwrap(),
            serde_json::json!({"a": 1})
        );

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("body.json");
        std::fs::write(&file, r#"{"b":2}"#).unwrap();
        assert_eq!(
            read_data(&format!("@{}", file.display()), &mut none).unwrap(),
            serde_json::json!({"b": 2})
        );

        assert_eq!(
            read_data("@-", &mut "[3]".as_bytes()).unwrap(),
            serde_json::json!([3])
        );
    }

    #[test]
    fn data_must_be_json() {
        let err = read_data("{nope", &mut std::io::empty()).unwrap_err();
        assert!(err.to_string().contains("not valid JSON"), "{err}");
        assert!(read_data("@/nonexistent/body.json", &mut std::io::empty()).is_err());
    }
}
