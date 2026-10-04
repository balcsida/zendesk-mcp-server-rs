//! Sign-in through the Zendesk mobile app's OAuth flow. No OAuth client is needed.
//!
//! Auth methods are discovered via `/api/mobile/account/lookup.json`:
//! - Email/password: direct POST to `/access/oauth_mobile` (no browser needed).
//! - SSO (SAML/Google/Office365): opens the system browser; the final redirect goes to
//!   `zendesk-support://authenticate?access_token=...`, which is captured either by a
//!   temporary OS URL-scheme handler or by the operator pasting the URL.
//!
//! The access token (no refresh token) is saved to the mobile token file for the
//! MCP server to use.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub const CLIENT_ID: &str = "zendesk_support_android";
pub const USER_AGENT: &str = "Zendesk-SDK/1.0 Android/30 Variant/Core";

/// Default wait for the browser flow.
pub const BROWSER_TIMEOUT: Duration = Duration::from_secs(300);

/// What the mobile flow saves. Same JSON keys as the Python version's `.zendesk_token`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MobileToken {
    pub subdomain: String,
    pub access_token: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub user_role: Option<String>,
}

// TODO(worker): manual `Debug` that redacts access_token.

/// `ZENDESK_MOBILE_TOKEN_FILE`, else `config::default_mobile_token_file()`.
pub fn token_path() -> PathBuf {
    todo!("worker: mobile")
}

/// Saved token, if the file exists, parses, and has a subdomain and access token.
pub fn load_token() -> Option<MobileToken> {
    todo!("worker: mobile")
}

/// Write the token (mode 0600, directory 0700). Returns the path written.
pub fn save_token(token: &MobileToken) -> Result<PathBuf> {
    let _ = token;
    todo!("worker: mobile")
}

/// GET `/api/v2/users/me.json` with the token: true on 200.
pub async fn verify_token(http: &reqwest::Client, subdomain: &str, access_token: &str) -> bool {
    let _ = (http, subdomain, access_token);
    todo!("worker: mobile")
}

/// The `lookup` object from `/api/mobile/account/lookup.json` (sent with `USER_AGENT`).
pub async fn lookup_subdomain(
    http: &reqwest::Client,
    subdomain: &str,
) -> Result<serde_json::Value> {
    let _ = (http, subdomain);
    todo!("worker: mobile")
}

/// POST `/access/oauth_mobile` with email and password.
pub async fn auth_email_password(
    http: &reqwest::Client,
    subdomain: &str,
    email: &str,
    password: &str,
) -> Result<MobileToken> {
    let _ = (http, subdomain, email, password);
    todo!("worker: mobile")
}

/// Extract the token from a `zendesk-support://authenticate?access_token=...` URL.
/// Parameters may be in the query or, failing that, the fragment.
pub fn parse_oauth_callback(url: &str, subdomain: &str) -> Option<MobileToken> {
    let _ = (url, subdomain);
    todo!("worker: mobile")
}

/// Append `client_id`, `device[name]` and `device[identifier]` to the login URL.
pub fn build_auth_url(auth_url: &str) -> String {
    let _ = auth_url;
    todo!("worker: mobile")
}

/// Non-interactive browser sign-in used by the server at startup.
///
/// Discovers auth methods, picks the best (SSO > Google > Office 365 > email/password),
/// serves a local callback/paste page, tries to register a temporary OS handler for
/// `zendesk-support://`, opens a private browser window, and waits up to `timeout`.
pub async fn auth_via_browser(
    http: &reqwest::Client,
    subdomain: &str,
    timeout: Duration,
) -> Result<MobileToken> {
    let _ = (http, subdomain, timeout);
    todo!("worker: mobile")
}

/// Token to use at server startup: the saved one if it is for `subdomain` and still
/// verifies, else a fresh browser sign-in (saved before returning). `subdomain` may be
/// `None` only when the saved token supplies it.
pub async fn ensure_auth(http: &reqwest::Client, subdomain: Option<&str>) -> Result<MobileToken> {
    let _ = (http, subdomain);
    todo!("worker: mobile")
}

/// Interactive `zendesk-mcp-server mobile-auth` command.
pub async fn run_auth_cli(http: reqwest::Client) -> Result<()> {
    let _ = http;
    todo!("worker: mobile")
}
