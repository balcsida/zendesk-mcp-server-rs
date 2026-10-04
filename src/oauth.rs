//! OAuth 2.0 authorization code flow with PKCE against Zendesk.
//!
//! The MCP server runs on each operator's own machine, so it is registered as a
//! *public* client: there is no client secret to distribute, and PKCE is what proves
//! the party redeeming the authorization code is the one that requested it.
//!
//! Every operator authorizes with their own Zendesk login, so the resulting token
//! carries their identity and Zendesk applies the same role, group and ticket
//! permissions it applies in the UI.

use anyhow::Result;
use tokio::sync::Mutex;

use crate::config::OAuthSettings;
use crate::tokens::{TokenSet, TokenStore};

/// Zendesk allows 5 minutes to 48 hours for access tokens and 7 to 90 days for
/// refresh tokens. Ask for the 90 day maximum on the refresh token: it is the
/// credential that has to survive periods where nobody uses the MCP server.
///
/// `expires_in` is always sent explicitly because OAuth clients created before
/// 2026-04-30 only issue a refresh token when it is present; omitting it yields a
/// non-expiring access token and no way to renew.
pub const ACCESS_TOKEN_TTL_SECONDS: u64 = 1800;
pub const REFRESH_TOKEN_TTL_SECONDS: u64 = 7_776_000;

/// A PKCE `code_verifier` and the S256 `code_challenge` derived from it.
#[derive(Clone, PartialEq, Eq)]
pub struct PkcePair {
    pub verifier: String,
    pub challenge: String,
}

impl PkcePair {
    pub const METHOD: &'static str = "S256";
}

/// 32 random bytes base64url-encoded (no padding) gives a 43 character verifier, the
/// minimum RFC 7636 permits. The challenge is base64url(sha256(verifier)).
pub fn generate_pkce_pair() -> PkcePair {
    todo!("worker: oauth")
}

/// Opaque value echoed back by Zendesk, used to detect a forged callback.
pub fn generate_state() -> String {
    todo!("worker: oauth")
}

/// `{authorize_endpoint}?response_type=code&client_id=..&redirect_uri=..&scope=..&state=..&code_challenge=..&code_challenge_method=S256`
pub fn build_authorization_url(settings: &OAuthSettings, state: &str, pkce: &PkcePair) -> String {
    let _ = (settings, state, pkce);
    todo!("worker: oauth")
}

/// Redeem an authorization code for an access and refresh token.
///
/// Form-encoded POST to `settings.token_endpoint()` with grant_type=authorization_code,
/// code, client_id, code_verifier, redirect_uri, scope, expires_in and
/// refresh_token_expires_in. No Authorization header, no client_secret.
pub async fn exchange_authorization_code(
    http: &reqwest::Client,
    settings: &OAuthSettings,
    code: &str,
    pkce: &PkcePair,
) -> Result<TokenSet> {
    let _ = (http, settings, code, pkce);
    todo!("worker: oauth")
}

/// Exchange a refresh token for a new token pair.
///
/// Zendesk rotates both tokens and invalidates the old pair immediately, so the
/// caller must persist the result before any further request is made. A response
/// without a new refresh token means the old one stays valid, so it is carried over.
/// Errors when no refresh token is stored.
pub async fn refresh_access_token(
    http: &reqwest::Client,
    settings: &OAuthSettings,
    tokens: &TokenSet,
) -> Result<TokenSet> {
    let _ = (http, settings, tokens);
    todo!("worker: oauth")
}

/// True only when a 401 body says the token itself is bad:
/// `{"error": "invalid_token", ...}` (case-insensitive on the value).
pub fn is_invalid_token_body(body: &[u8]) -> bool {
    let _ = body;
    todo!("worker: oauth")
}

/// Authenticates requests with a stored OAuth access token.
///
/// Renewal happens two ways:
///
/// * proactively, in `access_token()`, whenever the stored token is inside its expiry
///   skew window, and
/// * reactively, through `renew()`, when Zendesk answers 401 `invalid_token` and the
///   client wants to retry once.
///
/// Only `invalid_token` triggers a retry. A 401 or 403 caused by insufficient scope
/// or by the operator's Zendesk permissions is passed through untouched.
pub struct OAuthProvider {
    settings: OAuthSettings,
    store: TokenStore,
    http: reqwest::Client,
    tokens: Mutex<Option<TokenSet>>,
}

impl OAuthProvider {
    pub fn new(settings: OAuthSettings, http: reqwest::Client) -> Self {
        let store = TokenStore::new(settings.token_file.clone());
        Self::with_store(settings, store, http)
    }

    pub fn with_store(settings: OAuthSettings, store: TokenStore, http: reqwest::Client) -> Self {
        OAuthProvider {
            settings,
            store,
            http,
            tokens: Mutex::new(None),
        }
    }

    pub fn settings(&self) -> &OAuthSettings {
        &self.settings
    }

    /// A usable access token: the cached one, else the stored one, refreshed first if
    /// it has expired. Warns (once per load) when the stored tokens belong to another
    /// subdomain or client than the configured ones.
    pub async fn access_token(&self) -> Result<String> {
        todo!("worker: oauth")
    }

    /// Refresh the access token, persisting the rotated pair before returning.
    ///
    /// Holds the store lock across read-refresh-write so a second MCP process cannot
    /// refresh at the same time; whichever process gets the lock second finds the token
    /// already renewed and reuses it instead of spending the refresh token twice.
    ///
    /// `rejected_token` is the access token Zendesk just refused. It is what tells
    /// "somebody else already rotated this" (stored token differs: reuse it) apart from
    /// "this really does need refreshing", since a rejected token can still look unexpired.
    /// Without a rejected token, an unexpired stored token counts as already renewed.
    ///
    /// Fails with a message telling the operator to run `zendesk-mcp-server auth` when
    /// the refresh token is missing or expired, or Zendesk answers `invalid_grant`.
    pub async fn renew(&self, reason: &str, rejected_token: Option<&str>) -> Result<TokenSet> {
        let _ = (reason, rejected_token);
        todo!("worker: oauth")
    }
}
