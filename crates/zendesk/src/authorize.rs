//! `auth` (on `zendesk-mcp-server` and on `zendesk`) — one-time OAuth authorization for this machine.
//!
//! Run once per operator. It sends the operator to Zendesk in a browser, captures
//! the authorization code, exchanges it for tokens using PKCE, and stores the result
//! locally. From then on the MCP server renews the access token on its own.
//!
//! Two ways to receive the code:
//!
//! * loopback (default) — a local HTTP server on the redirect URI's host:port
//!   catches Zendesk's redirect.
//! * `--manual` — the operator pastes the redirect URL (or a bare code). Needed when
//!   the OAuth client is registered with a redirect URL that this machine cannot
//!   serve, such as `https://localhost`.

use std::collections::{BTreeSet, HashMap};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use axum::Router;
use axum::extract::{RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::config::{Credentials, MISSING_SUBDOMAIN, OAuthSettings, load_credentials};
use crate::oauth::{
    PkcePair, build_authorization_url, exchange_authorization_code, generate_pkce_pair,
    generate_state,
};
use crate::tokens::{TokenSet, TokenStore};

/// Zendesk authorization codes expire 120 seconds after being issued, so there is
/// no point waiting much longer than that for the redirect.
pub const CALLBACK_TIMEOUT_SECONDS: u64 = 180;

const SUCCESS_PAGE: &str = "<!doctype html>
<html lang=\"en\"><head><meta charset=\"utf-8\"><title>Zendesk authorization complete</title></head>
<body><h1>Authorization complete</h1>
<p>You can close this tab and return to the terminal.</p></body></html>
";

const FAILURE_PAGE: &str = "<!doctype html>
<html lang=\"en\"><head><meta charset=\"utf-8\"><title>Zendesk authorization failed</title></head>
<body><h1>Authorization failed</h1>
<p>Return to the terminal for details.</p></body></html>
";

/// Query parameters of the redirect (first value of each key).
type Captured = HashMap<String, String>;

/// Run the authorization flow. Returns the process exit code: 0 on success, 1 when
/// authorization or the token exchange failed, 2 when OAuth is not configured
/// (no `ZENDESK_SUBDOMAIN`, or another credential takes precedence), 130 on Ctrl-C.
pub async fn run(http: reqwest::Client, manual: bool) -> Result<i32> {
    let settings = match load_credentials() {
        Ok(Some(Credentials::OAuth { settings })) => settings,
        Ok(Some(_)) => {
            eprintln!(
                "error: ZENDESK_OAUTH_TOKEN, ZENDESK_EMAIL + ZENDESK_API_KEY or \
                 ZENDESK_SESSION_COOKIE is set and takes precedence over OAuth. Unset it, or \
                 set ZENDESK_CLIENT_ID to put OAuth first."
            );
            return Ok(2);
        }
        Ok(None) => {
            eprintln!("error: {MISSING_SUBDOMAIN}");
            return Ok(2);
        }
        Err(err) => {
            eprintln!("error: {err}");
            return Ok(2);
        }
    };

    let pkce = generate_pkce_pair();
    let state = generate_state();
    let authorization_url = build_authorization_url(&settings, &state, &pkce);
    let store = TokenStore::new(settings.token_file.clone());

    let flow = authorize(
        &http,
        &settings,
        &store,
        &pkce,
        &state,
        &authorization_url,
        manual,
    );
    let outcome = tokio::select! {
        outcome = flow => outcome,
        _ = tokio::signal::ctrl_c() => {
            eprintln!("\nCancelled.");
            return Ok(130);
        }
    };
    match outcome {
        Ok(tokens) => {
            print!("{}", format_report(&tokens, &store, &settings));
            Ok(0)
        }
        Err(err) => {
            eprintln!("\nerror: {err}");
            Ok(1)
        }
    }
}

async fn authorize(
    http: &reqwest::Client,
    settings: &OAuthSettings,
    store: &TokenStore,
    pkce: &PkcePair,
    state: &str,
    authorization_url: &str,
    manual: bool,
) -> Result<TokenSet> {
    let captured = if manual {
        receive_code_manually(authorization_url).await?
    } else {
        receive_code_via_loopback(settings, state, authorization_url).await?
    };
    let code = validate_callback(&captured, state)?;
    let tokens = exchange_authorization_code(http, settings, &code, pkce).await?;
    let _lock = store.lock().await?;
    store.save(&tokens)?;
    Ok(tokens)
}

/// Serves the single redirect Zendesk makes back to this machine.
///
/// Only a redirect carrying the expected `state` is accepted. Anything else is
/// answered but ignored, so a stray or forged request can neither abort nor hijack
/// the sign-in while it waits.
fn callback_router(sender: oneshot::Sender<Captured>, expected_state: &str) -> Router {
    let shared = Arc::new(CallbackShared {
        slot: Mutex::new(Some(sender)),
        expected_state: expected_state.to_string(),
    });
    // No request logging: the request line carries the authorization code.
    Router::new().fallback(get(callback)).with_state(shared)
}

struct CallbackShared {
    slot: Mutex<Option<oneshot::Sender<Captured>>>,
    expected_state: String,
}

async fn callback(
    State(shared): State<Arc<CallbackShared>>,
    RawQuery(query): RawQuery,
) -> impl IntoResponse {
    let captured = parse_query(query.as_deref().unwrap_or_default());
    let is_redirect = captured.contains_key("code") || captured.contains_key("error");
    let accepted = is_redirect && captured.get("state") == Some(&shared.expected_state);
    let (status, body) = match (is_redirect, accepted, captured.contains_key("code")) {
        (false, _, _) => (StatusCode::NOT_FOUND, FAILURE_PAGE),
        // A redirect with the wrong state is not ours: answer it, keep waiting.
        (true, false, _) => (StatusCode::BAD_REQUEST, FAILURE_PAGE),
        (true, true, true) => (StatusCode::OK, SUCCESS_PAGE),
        (true, true, false) => (StatusCode::BAD_REQUEST, FAILURE_PAGE),
    };
    if accepted
        && let Ok(mut slot) = shared.slot.lock()
        && let Some(sender) = slot.take()
    {
        let _ = sender.send(captured);
    }
    (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
}

fn parse_query(query: &str) -> Captured {
    let mut captured = Captured::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        captured
            .entry(key.into_owned())
            .or_insert_with(|| value.into_owned());
    }
    captured
}

async fn bind_loopback(redirect_uri: &str) -> Result<TcpListener> {
    let cannot_listen = || {
        anyhow!(
            "ZENDESK_OAUTH_REDIRECT_URI is {redirect_uri:?}, which this machine cannot listen \
             on. Register an http://localhost:PORT/... redirect URL on the OAuth client, or \
             re-run with --manual."
        )
    };
    let redirect = url::Url::parse(redirect_uri).map_err(|_| cannot_listen())?;
    let host = match redirect.host_str() {
        Some(host @ ("localhost" | "127.0.0.1")) if redirect.scheme() == "http" => host,
        _ => return Err(cannot_listen()),
    };
    let port = redirect.port().unwrap_or(80);
    TcpListener::bind((host, port)).await.map_err(|err| {
        anyhow!(
            "Could not listen on {host}:{port} ({err}). Free the port, point \
             ZENDESK_OAUTH_REDIRECT_URI elsewhere, or use --manual."
        )
    })
}

async fn receive_code_via_loopback(
    settings: &OAuthSettings,
    state: &str,
    authorization_url: &str,
) -> Result<Captured> {
    let listener = bind_loopback(&settings.redirect_uri).await?;
    let (sender, receiver) = oneshot::channel();
    let router = callback_router(sender, state);
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    println!("Opening your browser to authorize with Zendesk:\n  {authorization_url}\n");
    if webbrowser::open(authorization_url).is_err() {
        println!("Could not open a browser automatically. Open the URL above manually.\n");
    }
    println!("Waiting up to {CALLBACK_TIMEOUT_SECONDS}s for the redirect...");

    let result =
        tokio::time::timeout(Duration::from_secs(CALLBACK_TIMEOUT_SECONDS), receiver).await;
    server.abort();
    match result {
        Ok(Ok(captured)) => Ok(captured),
        _ => bail!(
            "No redirect received within {CALLBACK_TIMEOUT_SECONDS}s. Re-run, or use --manual \
             if the browser cannot reach this machine."
        ),
    }
}

async fn receive_code_manually(authorization_url: &str) -> Result<Captured> {
    println!("Open this URL in a browser and approve access:\n");
    println!("  {authorization_url}\n");
    println!(
        "Zendesk then redirects to your OAuth client's redirect URL. The page may fail to \
         load; that is expected.\nCopy the full URL from the address bar and paste it here."
    );
    print!("\nRedirect URL (or just the code): ");
    let _ = std::io::stdout().flush();
    let line = BufReader::new(tokio::io::stdin())
        .lines()
        .next_line()
        .await
        .map_err(|err| anyhow!("Could not read from stdin: {err}"))?;
    parse_pasted(line.as_deref().unwrap_or_default())
}

/// A pasted redirect URL yields its query pairs; anything else is a bare code.
fn parse_pasted(pasted: &str) -> Result<Captured> {
    let pasted = pasted.trim();
    if pasted.is_empty() {
        bail!("Nothing was pasted, so authorization cannot continue.");
    }
    if let Ok(parsed) = url::Url::parse(pasted)
        && let Some(query) = parsed.query().filter(|q| !q.is_empty())
    {
        return Ok(parse_query(query));
    }
    // Bare code pasted: there is no state to compare against.
    Ok(Captured::from([("code".to_owned(), pasted.to_owned())]))
}

fn validate_callback(captured: &Captured, expected_state: &str) -> Result<String> {
    if let Some(error) = captured.get("error") {
        let detail = captured
            .get("error_description")
            .filter(|d| !d.is_empty())
            .unwrap_or(error);
        bail!("Zendesk declined the authorization request: {detail}");
    }
    let Some(code) = captured.get("code").filter(|c| !c.is_empty()) else {
        bail!("The redirect did not include an authorization code.");
    };
    match captured.get("state") {
        None => tracing::warn!(
            "No state value was returned, so the callback could not be verified. This is \
             expected only when pasting a bare code."
        ),
        Some(returned) if returned != expected_state => bail!(
            "The state value returned by Zendesk does not match the one sent. Discarding this \
             response and not exchanging the code."
        ),
        Some(_) => {}
    }
    Ok(code.clone())
}

fn format_report(tokens: &TokenSet, store: &TokenStore, settings: &OAuthSettings) -> String {
    let until = |moment: Option<chrono::DateTime<chrono::Utc>>| {
        moment.map_or_else(
            || "no expiry".to_owned(),
            |m| m.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        )
    };
    let scope = tokens.scope.as_deref().filter(|s| !s.is_empty());
    let mut report = format!(
        "\nAuthorization complete.\n  Tokens stored at:   {}\n  Granted scope:      {}\n  \
         Access token until: {}\n  Refresh until:      {}\n",
        store.path.display(),
        scope.unwrap_or("(not reported by Zendesk)"),
        until(tokens.expires_at),
        until(tokens.refresh_token_expires_at),
    );

    let granted: BTreeSet<&str> = scope.unwrap_or_default().split_whitespace().collect();
    let requested: BTreeSet<&str> = settings.scopes.split_whitespace().collect();
    if !granted.is_empty() && granted != requested {
        let requested: Vec<&str> = requested.into_iter().collect();
        report.push_str(&format!(
            "\nNote: the granted scope differs from the requested [{}]. Zendesk accepts \
             unknown scope names but then rejects requests with 403, so check for typos in \
             ZENDESK_OAUTH_SCOPES.\n",
            requested.join(", ")
        ));
    }
    if tokens.refresh_token.is_none() {
        report.push_str(
            "\nWarning: Zendesk issued no refresh token, so the server cannot renew access \
             automatically and you will have to re-run this command. This happens with OAuth \
             clients created before 2026-04-30 in some configurations.\n",
        );
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured(pairs: &[(&str, &str)]) -> Captured {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn settings() -> OAuthSettings {
        OAuthSettings {
            subdomain: "acme".into(),
            client_id: "client".into(),
            token_file: "/tmp/unused/tokens.json".into(),
            scopes: "tickets:read users:read".into(),
            redirect_uri: "http://localhost:4567/callback".into(),
        }
    }

    #[test]
    fn validate_returns_code_when_state_matches() {
        let ok = captured(&[("code", "abc"), ("state", "s")]);
        assert_eq!(validate_callback(&ok, "s").unwrap(), "abc");
        // A bare code has no state to compare.
        assert_eq!(
            validate_callback(&captured(&[("code", "abc")]), "s").unwrap(),
            "abc"
        );
    }

    #[test]
    fn validate_rejects_errors_missing_code_and_state_mismatch() {
        let declined = captured(&[
            ("error", "access_denied"),
            ("error_description", "No thanks"),
        ]);
        assert_eq!(
            validate_callback(&declined, "s").unwrap_err().to_string(),
            "Zendesk declined the authorization request: No thanks"
        );
        let declined = captured(&[("error", "access_denied")]);
        assert!(
            validate_callback(&declined, "s")
                .unwrap_err()
                .to_string()
                .ends_with("access_denied")
        );
        assert_eq!(
            validate_callback(&captured(&[("state", "s")]), "s")
                .unwrap_err()
                .to_string(),
            "The redirect did not include an authorization code."
        );
        let forged = captured(&[("code", "abc"), ("state", "other")]);
        assert!(
            validate_callback(&forged, "s")
                .unwrap_err()
                .to_string()
                .contains("does not match")
        );
    }

    #[test]
    fn pasted_input_is_a_url_or_a_bare_code() {
        let url = parse_pasted("  http://localhost:4567/callback?code=abc%20d&state=s\n").unwrap();
        assert_eq!(url, captured(&[("code", "abc d"), ("state", "s")]));
        assert_eq!(
            parse_pasted("abc123").unwrap(),
            captured(&[("code", "abc123")])
        );
        assert_eq!(
            parse_pasted("  ").unwrap_err().to_string(),
            "Nothing was pasted, so authorization cannot continue."
        );
    }

    #[test]
    fn report_flags_scope_mismatch_and_missing_refresh_token() {
        let store = TokenStore::new("/tmp/unused/tokens.json");
        let mut tokens = TokenSet {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at: None,
            refresh_token_expires_at: None,
            subdomain: "acme".into(),
            client_id: "client".into(),
            scope: Some("users:read tickets:read".into()),
        };
        let clean = format_report(&tokens, &store, &settings());
        assert!(clean.contains("/tmp/unused/tokens.json") && clean.contains("no expiry"));
        assert!(!clean.contains("Note:") && !clean.contains("Warning:"));

        tokens.scope = Some("tickets:read".into());
        tokens.refresh_token = None;
        let flagged = format_report(&tokens, &store, &settings());
        assert!(flagged.contains("Note: the granted scope differs"));
        assert!(flagged.contains("Warning: Zendesk issued no refresh token"));

        tokens.scope = None;
        assert!(format_report(&tokens, &store, &settings()).contains("(not reported by Zendesk)"));
    }

    #[tokio::test]
    async fn bind_rejects_non_loopback_redirects() {
        for uri in ["https://localhost/cb", "http://example.com/cb", "not a url"] {
            let err = bind_loopback(uri).await.unwrap_err().to_string();
            assert!(err.contains("--manual"), "{uri}: {err}");
        }
    }

    #[tokio::test]
    async fn bind_reports_port_in_use() {
        let taken = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!(
            "http://127.0.0.1:{}/callback",
            taken.local_addr().unwrap().port()
        );
        let err = bind_loopback(&uri).await.unwrap_err().to_string();
        assert!(err.starts_with("Could not listen on 127.0.0.1:"));
        assert!(err.contains("use --manual"));
    }

    #[tokio::test]
    async fn loopback_server_captures_the_redirect() {
        let listener = bind_loopback("http://127.0.0.1:0/callback").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (sender, receiver) = oneshot::channel();
        let router = callback_router(sender, "s");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();

        let stray = client
            .get(format!("{base}/favicon.ico"))
            .send()
            .await
            .unwrap();
        assert_eq!(stray.status(), 404);
        assert!(stray.text().await.unwrap().contains("Authorization failed"));

        // A redirect with the wrong state is answered but must not consume the slot.
        let forged = client
            .get(format!("{base}/callback?code=evil&state=wrong"))
            .send()
            .await
            .unwrap();
        assert_eq!(forged.status(), 400);

        let ok = client
            .get(format!("{base}/callback?code=abc&state=s"))
            .send()
            .await
            .unwrap();
        assert_eq!(ok.status(), 200);
        assert_eq!(ok.headers()["content-type"], "text/html; charset=utf-8");
        assert!(ok.text().await.unwrap().contains("Authorization complete"));

        assert_eq!(
            receiver.await.unwrap(),
            captured(&[("code", "abc"), ("state", "s")])
        );
        server.abort();
    }

    #[tokio::test]
    async fn loopback_server_answers_400_to_an_error_redirect() {
        let listener = bind_loopback("http://localhost:0/callback").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (sender, receiver) = oneshot::channel();
        let router = callback_router(sender, "s");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let denied = client
            .get(format!("{base}/?error=access_denied&state=s"))
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), 400);
        assert_eq!(
            receiver.await.unwrap(),
            captured(&[("error", "access_denied"), ("state", "s")])
        );
        server.abort();
    }
}
