//! `zendesk-mcp-server auth` — one-time OAuth authorization for this machine.
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

use anyhow::Result;

/// Zendesk authorization codes expire 120 seconds after being issued, so there is
/// no point waiting much longer than that for the redirect.
pub const CALLBACK_TIMEOUT_SECONDS: u64 = 180;

/// Run the authorization flow. Returns the process exit code: 0 on success, 1 when
/// authorization or the token exchange failed, 2 when OAuth is not configured
/// (no `ZENDESK_CLIENT_ID`), 130 on Ctrl-C.
pub async fn run(http: reqwest::Client, manual: bool) -> Result<i32> {
    let _ = (http, manual);
    todo!("worker: oauth")
}
