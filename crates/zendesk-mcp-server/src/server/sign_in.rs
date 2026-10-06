//! Sign-in through this server: the Zendesk logins it keeps for its MCP clients, the
//! `/mcp` middleware that finds them, and the OAuth metadata that tells clients where to
//! sign in.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, bail};
use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use zendesk::auth::Auth;
use zendesk::config::{self, Credentials, OAuthSettings};
use zendesk::oauth::{OAuthProvider, ReauthRequired, challenge_for};

/// Prefix of the tokens this server hands to its MCP clients.
const SERVER_TOKEN_PREFIX: &str = "zmcp_";

/// What the server needs to sign MCP clients in to Zendesk and to find their logins.
pub struct SignIn {
    /// The server's origin, such as `https://host`.
    public: String,
    settings: OAuthSettings,
    /// `https://{subdomain}.zendesk.com` in production; tests point it at a mock.
    zendesk: String,
    http: reqwest::Client,
}

impl SignIn {
    pub fn new(
        public: String,
        settings: OAuthSettings,
        zendesk: String,
        http: reqwest::Client,
    ) -> Arc<SignIn> {
        Arc::new(SignIn {
            public,
            settings,
            zendesk,
            http,
        })
    }

    /// Sign-in settings from the environment, which must describe Zendesk OAuth.
    pub fn from_env(public: String, http: reqwest::Client) -> Result<Arc<SignIn>> {
        let Some(Credentials::OAuth { settings }) = config::load_credentials()? else {
            bail!(
                "--public-url needs Zendesk OAuth settings: set ZENDESK_SUBDOMAIN, and do not set ZENDESK_OAUTH_TOKEN, ZENDESK_EMAIL + ZENDESK_API_KEY or ZENDESK_SESSION_COOKIE."
            );
        };
        let zendesk = format!("https://{}.zendesk.com", settings.subdomain);
        let sign_in = SignIn::new(public, settings, zendesk, http);
        tracing::info!("Zendesk logins are kept in {}", sign_in.grants().display());
        Ok(sign_in)
    }

    /// The routes that need no token: the OAuth metadata documents.
    pub fn routes(self: &Arc<Self>) -> axum::Router {
        let resource = {
            let sign_in = self.clone();
            move || async move { Json(sign_in.protected_resource()) }
        };
        let server = {
            let sign_in = self.clone();
            move || async move { Json(sign_in.authorization_server()) }
        };
        axum::Router::new()
            .route(
                "/.well-known/oauth-protected-resource",
                axum::routing::get(resource.clone()),
            )
            .route(
                "/.well-known/oauth-protected-resource/mcp",
                axum::routing::get(resource),
            )
            .route(
                "/.well-known/oauth-authorization-server",
                axum::routing::get(server),
            )
    }

    fn protected_resource(&self) -> serde_json::Value {
        json!({
            "resource": format!("{}/mcp", self.public),
            "authorization_servers": [self.public],
            "bearer_methods_supported": ["header"],
        })
    }

    fn authorization_server(&self) -> serde_json::Value {
        let public = &self.public;
        json!({
            "issuer": public,
            "authorization_endpoint": format!("{public}/authorize"),
            "token_endpoint": format!("{public}/token"),
            "registration_endpoint": format!("{public}/register"),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none"],
        })
    }

    /// Where all stored logins live, next to the operator's own token file.
    fn grants(&self) -> PathBuf {
        let token_file = &self.settings.token_file;
        token_file.parent().unwrap_or(token_file).join("grants")
    }

    /// The directory holding the login that server token `token` stands for.
    fn grant_dir(&self, token: &str) -> PathBuf {
        self.grants().join(challenge_for(token))
    }

    /// The Zendesk login stored for `token`, renewed if it has expired. `None` means
    /// there is none, or there was a dead one and it has just been deleted.
    async fn caller(&self, token: &str) -> Result<Option<Auth>> {
        let dir = self.grant_dir(token);
        let token_file = dir.join("tokens.json");
        if !token_file.exists() {
            return Ok(None);
        }
        let settings = OAuthSettings {
            token_file,
            ..self.settings.clone()
        };
        let provider = OAuthProvider::new(settings, self.http.clone())
            .with_token_endpoint(&format!("{}/oauth/tokens", self.zendesk));
        match provider.access_token().await {
            Ok(_) => Ok(Some(Auth::OAuth(Arc::new(provider)))),
            Err(err) if err.is::<ReauthRequired>() => {
                std::fs::remove_dir_all(&dir)?;
                tracing::info!("Removed a Zendesk login that can no longer be renewed");
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }

    /// The 401 that sends an MCP client to the metadata to sign in.
    fn challenge(&self) -> Response {
        let value = format!(
            "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\"",
            self.public
        );
        let value = HeaderValue::from_str(&value).expect("the public URL is an ASCII origin");
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, value)],
        )
            .into_response()
    }
}

/// Middleware for `/mcp`: a server token must have a stored Zendesk login, which goes
/// on to the tool as the request's [`Auth`]. Any other bearer token is the caller's own
/// Zendesk token and passes on unchanged.
pub async fn require_caller(
    State(sign_in): State<Arc<SignIn>>,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(token) = super::bearer_token(req.headers()) else {
        return sign_in.challenge();
    };
    if token.starts_with(SERVER_TOKEN_PREFIX) {
        match sign_in.caller(token).await {
            Ok(Some(auth)) => {
                req.extensions_mut().insert(auth);
            }
            Ok(None) => return sign_in.challenge(),
            Err(err) => {
                tracing::warn!("Could not renew a stored Zendesk login: {err:#}");
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Could not renew your Zendesk sign-in. Try again shortly.",
                )
                    .into_response();
            }
        }
    }
    next.run(req).await
}

/// Clap parser for `--public-url`: an `https` origin, or `http` on this machine.
/// Returns the origin.
pub fn parse_public_url(raw: &str) -> Result<String, String> {
    const MESSAGE: &str =
        "must be https://HOST with nothing after the host, like https://zendesk-mcp.example.com";
    let url = reqwest::Url::parse(raw).map_err(|_| MESSAGE.to_string())?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && local);
    if !scheme_ok || url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err(MESSAGE.into());
    }
    Ok(url.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::super::tests::post_mcp;
    use super::super::{Login, ZendeskServer, http_router};
    use super::*;
    use serde_json::{Value, json};
    use tokio_util::sync::CancellationToken;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zendesk::tokens::{TokenSet, TokenStore};

    const TOKEN: &str = "zmcp_test";

    struct Harness {
        url: String,
        zendesk: MockServer,
        sign_in: Arc<SignIn>,
        http: reqwest::Client,
        _dir: tempfile::TempDir,
    }

    async fn harness() -> Harness {
        let zendesk = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let public = format!("http://{}", listener.local_addr().unwrap());
        let settings = OAuthSettings {
            subdomain: "acme".into(),
            client_id: "zdg-zcli-oauth".into(),
            token_file: dir.path().join("tokens.json"),
            scopes: "read write".into(),
            redirect_uri: "http://localhost:19186/".into(),
        };
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let sign_in = SignIn::new(public.clone(), settings, zendesk.uri(), http.clone());
        let server = ZendeskServer::with_login(
            Login::PerUser {
                subdomain: "acme".into(),
                base_url: format!("{}/api/v2", zendesk.uri()),
            },
            http.clone(),
        );
        let router = http_router(
            server,
            None,
            Some(sign_in.clone()),
            CancellationToken::new(),
        );
        tokio::spawn(async move { axum::serve(listener, router).await });
        Harness {
            url: public,
            zendesk,
            sign_in,
            http,
            _dir: dir,
        }
    }

    fn zd_tokens(access: &str, refresh: &str, expires_in: i64) -> TokenSet {
        TokenSet {
            access_token: access.into(),
            refresh_token: Some(refresh.into()),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::seconds(expires_in)),
            refresh_token_expires_at: Some(chrono::Utc::now() + chrono::Duration::days(30)),
            subdomain: "acme".into(),
            client_id: "zdg-zcli-oauth".into(),
            scope: None,
        }
    }

    fn seed(h: &Harness, token: &str, tokens: &TokenSet) {
        TokenStore::new(h.sign_in.grant_dir(token).join("tokens.json"))
            .save(tokens)
            .unwrap();
    }

    fn initialize() -> Value {
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" },
            },
        })
    }

    async fn mock_me(h: &Harness, bearer: &str) {
        Mock::given(method("GET"))
            .and(path("/api/v2/users/me.json"))
            .and(header("authorization", format!("Bearer {bearer}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({ "user": { "id": 7, "name": "Alice", "email": "alice@example.com" } }),
            ))
            .mount(&h.zendesk)
            .await;
    }

    /// Mock the refresh of `r1` into access token `zd-new` and refresh token `r2`.
    fn refresh_mock() -> wiremock::MockBuilder {
        Mock::given(method("POST"))
            .and(path("/oauth/tokens"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=r1"))
    }

    async fn call_current_user(h: &Harness, token: &str) -> String {
        let url = format!("{}/mcp", h.url);
        post_mcp(&h.http, &url, token, initialize()).await;
        let call = json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "get_current_user", "arguments": {} },
        });
        let (_, reply) = post_mcp(&h.http, &url, token, call).await;
        reply.expect("a tools/call response")["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    async fn post_raw(h: &Harness, token: &str) -> reqwest::Response {
        h.http
            .post(format!("{}/mcp", h.url))
            .bearer_auth(token)
            .header("accept", "application/json, text/event-stream")
            .json(&initialize())
            .send()
            .await
            .unwrap()
    }

    fn expired_login(h: &Harness) {
        seed(h, TOKEN, &zd_tokens("zd-old", "r1", -3600));
    }

    #[test]
    fn public_url_must_be_an_https_origin() {
        for (raw, origin) in [
            ("https://mcp.example.com", "https://mcp.example.com"),
            ("https://mcp.example.com/", "https://mcp.example.com"),
            (
                "https://mcp.example.com:8443",
                "https://mcp.example.com:8443",
            ),
            ("http://localhost:8080", "http://localhost:8080"),
            ("http://127.0.0.1:9", "http://127.0.0.1:9"),
        ] {
            assert_eq!(parse_public_url(raw).as_deref(), Ok(origin), "{raw}");
        }
        for raw in [
            "http://mcp.example.com",
            "https://mcp.example.com/zendesk",
            "https://mcp.example.com/?a=1",
            "https://mcp.example.com/?",
            "https://mcp.example.com/#x",
            "ftp://mcp.example.com",
            "not a url",
        ] {
            assert!(parse_public_url(raw).is_err(), "{raw}");
        }
    }

    #[tokio::test]
    async fn unauthenticated_requests_are_challenged() {
        let h = harness().await;
        let challenge = format!(
            "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\"",
            h.url
        );
        let anonymous = h
            .http
            .post(format!("{}/mcp", h.url))
            .body("{}")
            .send()
            .await
            .unwrap();
        for response in [anonymous, post_raw(&h, "zmcp_unknown").await] {
            assert_eq!(response.status(), 401);
            assert_eq!(
                response
                    .headers()
                    .get("www-authenticate")
                    .and_then(|v| v.to_str().ok()),
                Some(challenge.as_str())
            );
        }

        let get = |path: &str| {
            let url = format!("{}{path}", h.url);
            async {
                h.http
                    .get(url)
                    .send()
                    .await
                    .unwrap()
                    .json::<Value>()
                    .await
                    .unwrap()
            }
        };
        let resource = json!({
            "resource": format!("{}/mcp", h.url),
            "authorization_servers": [h.url],
            "bearer_methods_supported": ["header"],
        });
        assert_eq!(get("/.well-known/oauth-protected-resource").await, resource);
        assert_eq!(
            get("/.well-known/oauth-protected-resource/mcp").await,
            resource
        );
        assert_eq!(
            get("/.well-known/oauth-authorization-server").await,
            json!({
                "issuer": h.url,
                "authorization_endpoint": format!("{}/authorize", h.url),
                "token_endpoint": format!("{}/token", h.url),
                "registration_endpoint": format!("{}/register", h.url),
                "response_types_supported": ["code"],
                "grant_types_supported": ["authorization_code"],
                "code_challenge_methods_supported": ["S256"],
                "token_endpoint_auth_methods_supported": ["none"],
            })
        );
    }

    #[tokio::test]
    async fn stored_login_reaches_zendesk() {
        let h = harness().await;
        seed(&h, TOKEN, &zd_tokens("zd-access", "zd-refresh", 3600));
        mock_me(&h, "zd-access").await;
        assert!(call_current_user(&h, TOKEN).await.contains("Alice"));
    }

    #[tokio::test]
    async fn expired_login_is_renewed_and_saved() {
        let h = harness().await;
        expired_login(&h);
        refresh_mock()
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "zd-new", "refresh_token": "r2", "expires_in": 1800,
                "token_type": "bearer",
            })))
            .mount(&h.zendesk)
            .await;
        mock_me(&h, "zd-new").await;
        assert!(call_current_user(&h, TOKEN).await.contains("Alice"));
        let saved = TokenStore::new(h.sign_in.grant_dir(TOKEN).join("tokens.json"))
            .load()
            .unwrap();
        assert_eq!(saved.access_token, "zd-new");
        assert_eq!(saved.refresh_token.as_deref(), Some("r2"));
    }

    #[tokio::test]
    async fn concurrent_requests_refresh_once() {
        let h = harness().await;
        expired_login(&h);
        refresh_mock()
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "zd-new", "refresh_token": "r2", "expires_in": 1800,
                "token_type": "bearer",
            })))
            .expect(1)
            .mount(&h.zendesk)
            .await;
        let (a, b) = tokio::join!(post_raw(&h, TOKEN), post_raw(&h, TOKEN));
        for response in [a, b] {
            assert_ne!(response.status(), 401);
            assert_ne!(response.status(), 503);
        }
    }

    #[tokio::test]
    async fn dead_login_is_deleted_and_challenged() {
        let h = harness().await;
        expired_login(&h);
        refresh_mock()
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({ "error": "invalid_grant" })),
            )
            .mount(&h.zendesk)
            .await;
        let response = post_raw(&h, TOKEN).await;
        assert_eq!(response.status(), 401);
        assert!(response.headers().contains_key("www-authenticate"));
        assert!(!h.sign_in.grant_dir(TOKEN).exists());
    }

    #[tokio::test]
    async fn zendesk_outage_keeps_the_login() {
        let h = harness().await;
        expired_login(&h);
        refresh_mock()
            .respond_with(ResponseTemplate::new(500))
            .mount(&h.zendesk)
            .await;
        assert_eq!(post_raw(&h, TOKEN).await.status(), 503);
        assert!(h.sign_in.grant_dir(TOKEN).exists());
    }

    #[tokio::test]
    async fn raw_zendesk_tokens_pass_through() {
        let h = harness().await;
        mock_me(&h, "raw-token").await;
        assert!(call_current_user(&h, "raw-token").await.contains("Alice"));
    }
}
