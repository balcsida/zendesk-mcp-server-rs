//! OAuth 2.0 authorization code flow with PKCE against Zendesk.
//!
//! The MCP server runs on each operator's own machine, so it is registered as a
//! *public* client: there is no client secret to distribute, and PKCE is what proves
//! the party redeeming the authorization code is the one that requested it.
//!
//! Every operator authorizes with their own Zendesk login, so the resulting token
//! carries their identity and Zendesk applies the same role, group and ticket
//! permissions it applies in the UI.

use anyhow::{Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, MutexGuard};

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
    let verifier = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let challenge = challenge_for(&verifier);
    PkcePair {
        verifier,
        challenge,
    }
}

pub fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Opaque value echoed back by Zendesk, used to detect a forged callback.
pub fn generate_state() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}

/// `{authorize_endpoint}?response_type=code&client_id=..&redirect_uri=..&scope=..&state=..&code_challenge=..&code_challenge_method=S256`
pub fn build_authorization_url(settings: &OAuthSettings, state: &str, pkce: &PkcePair) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("response_type", "code")
        .append_pair("client_id", &settings.client_id)
        .append_pair("redirect_uri", &settings.redirect_uri)
        .append_pair("scope", &settings.scopes)
        .append_pair("state", state)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", PkcePair::METHOD)
        .finish();
    format!("{}?{query}", settings.authorize_endpoint())
}

/// POST a form to the token endpoint. Deliberately carries no Authorization header,
/// and a public client sends no client_secret.
async fn post_token_request(
    http: &reqwest::Client,
    endpoint: &str,
    payload: &[(&str, &str)],
) -> Result<serde_json::Value> {
    let response = http
        .post(endpoint)
        .form(payload)
        .send()
        .await
        .map_err(|err| anyhow!("Could not reach {endpoint}: {err}"))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|err| anyhow!("Could not reach {endpoint}: {err}"))?;

    if status.as_u16() >= 400 {
        return Err(token_error(status.as_u16(), &body));
    }
    serde_json::from_slice(&body).map_err(|_| {
        anyhow!(
            "{endpoint} returned a non-JSON response (HTTP {}).",
            status.as_u16()
        )
    })
}

/// The stored login cannot be renewed; only signing in again helps.
#[derive(Debug)]
pub struct ReauthRequired(pub String);

impl std::fmt::Display for ReauthRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ReauthRequired {}

/// Translate an error body into an error, without echoing credentials.
fn token_error(status: u16, body: &[u8]) -> anyhow::Error {
    let parsed: Option<serde_json::Value> = serde_json::from_slice(body).ok();
    let field = |key: &str| {
        parsed
            .as_ref()
            .and_then(|b| b.get(key))
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
    };
    let error = field("error");
    let detail = match (error, field("error_description")) {
        (Some(error), Some(description)) => format!("{error}: {description}"),
        (Some(error), None) => error.to_owned(),
        (None, _) => "no error detail".to_owned(),
    };
    let message = format!("Zendesk rejected the token request (HTTP {status}). {detail}");
    match error {
        Some("invalid_grant") => anyhow::Error::new(ReauthRequired(format!(
            "{message}\nThe authorization code or refresh token is expired, revoked or already \
             used. Run zendesk-mcp-server auth (or zendesk auth) to authorize again."
        ))),
        Some("invalid_scope") => anyhow!(
            "{message}\nThe requested scopes exceed the OAuth client's allowed scopes. Widen \
             them in Admin Center or narrow ZENDESK_OAUTH_SCOPES."
        ),
        _ => anyhow!(message),
    }
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
    exchange_code_at(http, &settings.token_endpoint(), settings, code, pkce).await
}

pub async fn exchange_code_at(
    http: &reqwest::Client,
    endpoint: &str,
    settings: &OAuthSettings,
    code: &str,
    pkce: &PkcePair,
) -> Result<TokenSet> {
    let issued_at = Utc::now();
    let access_ttl = ACCESS_TOKEN_TTL_SECONDS.to_string();
    let refresh_ttl = REFRESH_TOKEN_TTL_SECONDS.to_string();
    let payload = post_token_request(
        http,
        endpoint,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", &settings.client_id),
            ("code_verifier", &pkce.verifier),
            ("redirect_uri", &settings.redirect_uri),
            ("scope", &settings.scopes),
            ("expires_in", &access_ttl),
            ("refresh_token_expires_in", &refresh_ttl),
        ],
    )
    .await?;
    TokenSet::from_token_response(
        &payload,
        &settings.subdomain,
        &settings.client_id,
        issued_at,
    )
}

/// Exchange a refresh token for a new token pair at `endpoint`.
///
/// Zendesk rotates both tokens and invalidates the old pair immediately, so the
/// caller must persist the result before any further request is made. A response
/// without a new refresh token means the old one stays valid, so it is carried over.
/// Errors when no refresh token is stored.
async fn refresh_at(
    http: &reqwest::Client,
    endpoint: &str,
    settings: &OAuthSettings,
    tokens: &TokenSet,
) -> Result<TokenSet> {
    let Some(refresh_token) = tokens.refresh_token.as_deref() else {
        bail!(
            "No refresh token is stored, so the access token cannot be renewed. Run \
             zendesk-mcp-server auth (or zendesk auth) to authorize again."
        );
    };
    let issued_at = Utc::now();
    let access_ttl = ACCESS_TOKEN_TTL_SECONDS.to_string();
    let refresh_ttl = REFRESH_TOKEN_TTL_SECONDS.to_string();
    let payload = post_token_request(
        http,
        endpoint,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", &settings.client_id),
            ("expires_in", &access_ttl),
            ("refresh_token_expires_in", &refresh_ttl),
        ],
    )
    .await?;
    let mut refreshed = TokenSet::from_token_response(
        &payload,
        &settings.subdomain,
        &settings.client_id,
        issued_at,
    )?;
    // A response without a new refresh token means the old one stays valid.
    if refreshed.refresh_token.is_none() {
        refreshed.refresh_token = tokens.refresh_token.clone();
        refreshed.refresh_token_expires_at = tokens.refresh_token_expires_at;
    }
    tracing::info!("Renewed the Zendesk OAuth access token.");
    Ok(refreshed)
}

/// True only when a 401 body says the token itself is bad:
/// `{"error": "invalid_token", ...}` (case-insensitive on the value).
pub fn is_invalid_token_body(body: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error")?
                .as_str()
                .map(|e| e.eq_ignore_ascii_case("invalid_token"))
        })
        .unwrap_or(false)
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
    token_endpoint: String,
    tokens: Mutex<Option<TokenSet>>,
}

impl OAuthProvider {
    pub fn new(settings: OAuthSettings, http: reqwest::Client) -> Self {
        let store = TokenStore::new(settings.token_file.clone());
        Self::with_store(settings, store, http)
    }

    pub fn with_store(settings: OAuthSettings, store: TokenStore, http: reqwest::Client) -> Self {
        OAuthProvider {
            token_endpoint: settings.token_endpoint(),
            settings,
            store,
            http,
            tokens: Mutex::new(None),
        }
    }

    /// Point token requests somewhere other than Zendesk, such as a test server.
    pub fn with_token_endpoint(mut self, endpoint: &str) -> Self {
        self.token_endpoint = endpoint.to_string();
        self
    }

    /// A usable access token: the cached one, else the stored one, refreshed first if
    /// it has expired. Warns (once per load) when the stored tokens belong to another
    /// subdomain or client than the configured ones.
    pub async fn access_token(&self) -> Result<String> {
        let mut cached = self.tokens.lock().await;
        let tokens = match cached.as_ref() {
            Some(tokens) => tokens.clone(),
            None => {
                let tokens = self.store.load()?;
                if tokens.subdomain != self.settings.subdomain
                    || tokens.client_id != self.settings.client_id
                {
                    tracing::warn!(
                        "The stored Zendesk tokens were issued for subdomain {} and client {}, \
                         not the configured {} and {}. Re-run zendesk-mcp-server auth (or zendesk auth) if \
                         requests fail.",
                        tokens.subdomain,
                        tokens.client_id,
                        self.settings.subdomain,
                        self.settings.client_id
                    );
                }
                *cached = Some(tokens.clone());
                tokens
            }
        };
        if tokens.access_token_expired() {
            let renewed = self
                .renew_locked(&mut cached, "the stored access token has expired", None)
                .await?;
            return Ok(renewed.access_token);
        }
        Ok(tokens.access_token)
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
    /// Fails with a message telling the operator to run `zendesk-mcp-server auth` (or `zendesk auth`) when
    /// the refresh token is missing or expired, or Zendesk answers `invalid_grant`.
    pub async fn renew(&self, reason: &str, rejected_token: Option<&str>) -> Result<TokenSet> {
        let mut cached = self.tokens.lock().await;
        self.renew_locked(&mut cached, reason, rejected_token).await
    }

    /// `renew` with the in-process mutex already held by the caller.
    async fn renew_locked(
        &self,
        cached: &mut MutexGuard<'_, Option<TokenSet>>,
        reason: &str,
        rejected_token: Option<&str>,
    ) -> Result<TokenSet> {
        let _lock = self.store.lock().await?;
        let stored = self.store.load()?;

        let already_renewed = match rejected_token {
            Some(rejected) => stored.access_token != rejected,
            None => !stored.access_token_expired(),
        };
        if already_renewed {
            **cached = Some(stored.clone());
            return Ok(stored);
        }
        if !stored.can_refresh() {
            return Err(anyhow::Error::new(ReauthRequired(format!(
                "The Zendesk refresh token is missing or expired, so access cannot be renewed \
                 ({reason}). Run zendesk-mcp-server auth (or zendesk auth) to authorize this machine again."
            ))));
        }

        tracing::info!("Renewing the Zendesk access token because {reason}.");
        let refreshed =
            refresh_at(&self.http, &self.token_endpoint, &self.settings, &stored).await?;
        // Zendesk invalidated the old refresh token the moment the refresh succeeded, so
        // persist before anything else can fail.
        self.store.save(&refreshed)?;
        **cached = Some(refreshed.clone());
        Ok(refreshed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn settings(dir: &Path) -> OAuthSettings {
        OAuthSettings {
            subdomain: "acme".into(),
            client_id: "client-1".into(),
            token_file: dir.join("tokens.json"),
            scopes: "tickets:read users:read".into(),
            redirect_uri: "http://localhost:4567/callback".into(),
        }
    }

    fn pkce() -> PkcePair {
        PkcePair {
            verifier: "verifier".into(),
            challenge: challenge_for("verifier"),
        }
    }

    fn stored(access: &str, expires_in: i64) -> TokenSet {
        TokenSet {
            access_token: access.into(),
            refresh_token: Some("old-refresh".into()),
            expires_at: Some(Utc::now() + chrono::Duration::seconds(expires_in)),
            refresh_token_expires_at: Some(Utc::now() + chrono::Duration::days(30)),
            subdomain: "acme".into(),
            client_id: "client-1".into(),
            scope: None,
        }
    }

    /// A provider whose store holds `tokens` and whose token endpoint is the mock server.
    fn provider(dir: &Path, server: &MockServer, tokens: &TokenSet) -> OAuthProvider {
        let store = TokenStore::new(dir.join("tokens.json"));
        store.save(tokens).unwrap();
        let mut provider = OAuthProvider::with_store(settings(dir), store, reqwest::Client::new());
        provider.token_endpoint = format!("{}/oauth/tokens", server.uri());
        provider
    }

    fn token_response(access: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": access, "refresh_token": "new-refresh", "expires_in": 1800,
            "refresh_token_expires_in": 7776000, "scope": "tickets:read"
        }))
    }

    #[test]
    fn pkce_matches_rfc7636_vector() {
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let pair = generate_pkce_pair();
        assert_eq!(pair.verifier.len(), 43);
        assert_eq!(pair.challenge, challenge_for(&pair.verifier));
        assert_ne!(generate_state(), generate_state());
    }

    #[test]
    fn authorization_url_has_all_seven_parameters_in_order() {
        let dir = Path::new("/unused");
        let url = build_authorization_url(&settings(dir), "state-1", &pkce());
        let parsed = url::Url::parse(&url).unwrap();
        assert_eq!(parsed.path(), "/oauth/authorizations/new");
        let pairs: Vec<(String, String)> = parsed.query_pairs().into_owned().collect();
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "response_type",
                "client_id",
                "redirect_uri",
                "scope",
                "state",
                "code_challenge",
                "code_challenge_method"
            ]
        );
        assert_eq!(pairs[2].1, "http://localhost:4567/callback");
        assert_eq!(pairs[3].1, "tickets:read users:read");
        assert_eq!(pairs[6].1, "S256");
    }

    #[tokio::test]
    async fn exchange_posts_form_fields_and_parses_tokens() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/tokens"))
            .and(body_string_contains("grant_type=authorization_code"))
            .respond_with(token_response("fresh"))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let endpoint = format!("{}/oauth/tokens", server.uri());
        let tokens = exchange_code_at(
            &reqwest::Client::new(),
            &endpoint,
            &settings(dir.path()),
            "the-code",
            &pkce(),
        )
        .await
        .unwrap();
        assert_eq!(tokens.access_token, "fresh");
        assert_eq!(tokens.refresh_token.as_deref(), Some("new-refresh"));
        assert!(tokens.expires_at.is_some() && tokens.refresh_token_expires_at.is_some());

        let requests = server.received_requests().await.unwrap();
        assert!(requests[0].headers.get("authorization").is_none());
        let form: HashMap<String, String> = url::form_urlencoded::parse(&requests[0].body)
            .into_owned()
            .collect();
        let expect = [
            ("grant_type", "authorization_code"),
            ("code", "the-code"),
            ("client_id", "client-1"),
            ("code_verifier", "verifier"),
            ("redirect_uri", "http://localhost:4567/callback"),
            ("scope", "tickets:read users:read"),
            ("expires_in", "1800"),
            ("refresh_token_expires_in", "7776000"),
        ];
        assert_eq!(form.len(), expect.len());
        for (key, value) in expect {
            assert_eq!(form.get(key).map(String::as_str), Some(value), "{key}");
        }
    }

    use std::collections::HashMap;

    #[tokio::test]
    async fn invalid_grant_tells_operator_to_reauthorize() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant", "error_description": "code used"
            })))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let endpoint = format!("{}/oauth/tokens", server.uri());
        let err = exchange_code_at(
            &reqwest::Client::new(),
            &endpoint,
            &settings(dir.path()),
            "c",
            &pkce(),
        )
        .await
        .unwrap_err();
        assert!(err.is::<ReauthRequired>());
        let err = err.to_string();
        assert!(err.contains("HTTP 400") && err.contains("invalid_grant: code used"));
        assert!(err.contains("zendesk-mcp-server auth"));
    }

    #[tokio::test]
    async fn other_token_errors_are_not_reauth_required() {
        let dir = tempfile::tempdir().unwrap();
        for response in [
            ResponseTemplate::new(400)
                .set_body_json(serde_json::json!({ "error": "invalid_scope" })),
            ResponseTemplate::new(500),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(response)
                .mount(&server)
                .await;
            let endpoint = format!("{}/oauth/tokens", server.uri());
            let err = exchange_code_at(
                &reqwest::Client::new(),
                &endpoint,
                &settings(dir.path()),
                "c",
                &pkce(),
            )
            .await
            .unwrap_err();
            assert!(!err.is::<ReauthRequired>());
        }
    }

    #[tokio::test]
    async fn non_json_success_body_is_reported() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
            .mount(&server)
            .await;
        let err = post_token_request(&reqwest::Client::new(), &server.uri(), &[])
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("non-JSON response (HTTP 200)"));
    }

    #[tokio::test]
    async fn refresh_carries_over_old_refresh_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=old-refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "fresh", "expires_in": 1800
            })))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let old = stored("stale", -10);
        let endpoint = format!("{}/oauth/tokens", server.uri());
        let new = refresh_at(
            &reqwest::Client::new(),
            &endpoint,
            &settings(dir.path()),
            &old,
        )
        .await
        .unwrap();
        assert_eq!(new.access_token, "fresh");
        assert_eq!(new.refresh_token, old.refresh_token);
        assert_eq!(new.refresh_token_expires_at, old.refresh_token_expires_at);
    }

    #[tokio::test]
    async fn provider_refreshes_expired_token_and_persists_it() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(token_response("fresh"))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let provider = provider(dir.path(), &server, &stored("stale", -10));
        assert_eq!(provider.access_token().await.unwrap(), "fresh");
        // Served from cache the second time (the mock expects a single call).
        assert_eq!(provider.access_token().await.unwrap(), "fresh");
        let on_disk = TokenStore::new(dir.path().join("tokens.json"))
            .load()
            .unwrap();
        assert_eq!(on_disk.access_token, "fresh");
        assert_eq!(on_disk.refresh_token.as_deref(), Some("new-refresh"));
    }

    #[tokio::test]
    async fn provider_uses_unexpired_stored_token_without_network() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(token_response("x"))
            .expect(0)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let provider = provider(dir.path(), &server, &stored("good", 3600));
        assert_eq!(provider.access_token().await.unwrap(), "good");
    }

    #[tokio::test]
    async fn renew_with_rejected_token_equal_to_stored_refreshes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(token_response("fresh"))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        // Looks unexpired, but Zendesk refused it.
        let provider = provider(dir.path(), &server, &stored("rejected", 3600));
        let renewed = provider
            .renew("Zendesk rejected it", Some("rejected"))
            .await
            .unwrap();
        assert_eq!(renewed.access_token, "fresh");
    }

    #[tokio::test]
    async fn renew_with_other_rejected_token_reuses_stored_without_calling_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(token_response("x"))
            .expect(0)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let provider = provider(dir.path(), &server, &stored("rotated-by-peer", 3600));
        let renewed = provider
            .renew("Zendesk rejected it", Some("older"))
            .await
            .unwrap();
        assert_eq!(renewed.access_token, "rotated-by-peer");
    }

    #[tokio::test]
    async fn renew_without_usable_refresh_token_fails_with_guidance() {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let mut tokens = stored("stale", -10);
        tokens.refresh_token = None;
        let provider = provider(dir.path(), &server, &tokens);
        let err = provider.access_token().await.unwrap_err();
        assert!(err.is::<ReauthRequired>());
        let err = err.to_string();
        assert!(err.contains("refresh token is missing or expired"));
        assert!(err.contains("the stored access token has expired"));
        assert!(err.contains("zendesk-mcp-server auth"));
    }

    #[test]
    fn invalid_token_body_detection() {
        assert!(is_invalid_token_body(br#"{"error": "invalid_token"}"#));
        assert!(is_invalid_token_body(
            br#"{"error": "Invalid_Token", "x": 1}"#
        ));
        assert!(!is_invalid_token_body(
            br#"{"error": "insufficient_scope"}"#
        ));
        assert!(!is_invalid_token_body(br#"{"error": 5}"#));
        assert!(!is_invalid_token_body(br#"["invalid_token"]"#));
        assert!(!is_invalid_token_body(b"invalid_token"));
        assert!(!is_invalid_token_body(b""));
    }
}
