//! Sign-in through this server: the Zendesk logins it keeps for its MCP clients, the
//! `/mcp` middleware that finds them, and the OAuth metadata that tells clients where to
//! sign in.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use axum::Json;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Form, routing};
use serde_json::{Value, json};

use zendesk::ZendeskClient;
use zendesk::auth::Auth;
use zendesk::authorize::{parse_pasted, validate_callback};
use zendesk::config::{self, Credentials, OAuthSettings};
use zendesk::oauth::{
    OAuthProvider, PkcePair, ReauthRequired, build_authorization_url, challenge_for,
    exchange_code_at, generate_pkce_pair, generate_state,
};
use zendesk::tokens::{TokenSet, TokenStore};

/// Prefix of the tokens this server hands to its MCP clients.
const SERVER_TOKEN_PREFIX: &str = "zmcp_";

/// How long a started sign-in waits for its Zendesk code.
const PENDING_TTL: Duration = Duration::from_secs(600);

/// The most sign-ins that may be in progress at once.
const MAX_PENDING: usize = 1000;

/// How many failed code redemptions end a sign-in.
const MAX_FAILURES: u8 = 3;

/// How long a stored login may go unused before it is deleted.
const GRANT_IDLE: Duration = Duration::from_secs(90 * 24 * 3600);

/// How long a one-time code for `/token` stays valid.
const CODE_TTL: Duration = Duration::from_secs(60);

/// Sign-ins in progress and the one-time codes issued for finished ones.
#[derive(Default)]
struct Flows {
    /// By the `state` sent to Zendesk.
    pending: HashMap<String, Pending>,
    /// By the code handed to the MCP client.
    codes: HashMap<String, Issued>,
}

#[derive(Clone)]
struct Pending {
    started: Instant,
    pkce: PkcePair,
    /// `None` for a sign-in started by the CLI rather than by an MCP client.
    client: Option<Client>,
    /// The PKCE challenge of a sign-in started by the CLI; `None` for an MCP client's.
    cli_challenge: Option<String>,
    /// Codes Zendesk refused so far.
    failures: u8,
}

/// The MCP client's side of a sign-in, as sent to `/authorize`.
#[derive(Clone)]
struct Client {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    state: Option<String>,
}

struct Issued {
    issued: Instant,
    client: Client,
    signed_in: SignedIn,
}

/// A completed Zendesk sign-in: its tokens and the user they belong to.
pub(super) struct SignedIn {
    pub tokens: TokenSet,
    pub user: Value,
}

/// What the server needs to sign MCP clients in to Zendesk and to find their logins.
pub struct SignIn {
    /// The server's origin, such as `https://host`.
    public: String,
    settings: OAuthSettings,
    /// `https://{subdomain}.zendesk.com` in production; tests point it at a mock.
    zendesk: String,
    http: reqwest::Client,
    flows: Mutex<Flows>,
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
            flows: Mutex::new(Flows::default()),
        })
    }

    /// Sign-in settings from the environment, which must describe Zendesk OAuth.
    pub fn from_env(public: String, http: reqwest::Client) -> Result<Arc<SignIn>> {
        const NEEDS_OAUTH: &str = "--public-url needs Zendesk OAuth settings: set ZENDESK_SUBDOMAIN, and do not set ZENDESK_OAUTH_TOKEN, ZENDESK_EMAIL + ZENDESK_API_KEY or ZENDESK_SESSION_COOKIE.";
        if other_credentials_set(|name| std::env::var(name).ok()).is_some() {
            bail!(NEEDS_OAUTH);
        }
        let Some(Credentials::OAuth { settings }) = config::load_credentials()? else {
            bail!(NEEDS_OAUTH);
        };
        let zendesk = format!("https://{}.zendesk.com", settings.subdomain);
        let sign_in = SignIn::new(public, settings, zendesk, http);
        tracing::info!("Zendesk logins are kept in {}", sign_in.grants().display());
        sign_in.sweep_grants();
        Ok(sign_in)
    }

    /// The routes that need no token: the OAuth metadata documents and the sign-in
    /// endpoints.
    pub fn routes(self: &Arc<Self>) -> axum::Router {
        let resource = {
            let sign_in = self.clone();
            move || async move { Json(sign_in.protected_resource()) }
        };
        let server = {
            let sign_in = self.clone();
            move || async move { Json(sign_in.authorization_server()) }
        };
        let sign_in_routes = axum::Router::new()
            .route("/register", routing::post(register))
            .route(
                "/authorize",
                routing::get(authorize_page).post(authorize_paste),
            )
            .route("/token", routing::post(token))
            .route("/cli/login", routing::post(cli_login))
            .route("/cli/login/finish", routing::post(cli_login_finish))
            .layer(DefaultBodyLimit::max(16 * 1024))
            .with_state(self.clone());
        axum::Router::new()
            .merge(sign_in_routes)
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

    fn flows(&self) -> MutexGuard<'_, Flows> {
        self.flows.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Begin a sign-in. Returns the Zendesk authorize URL and its state, or `None` when
    /// too many sign-ins are in progress.
    fn start(
        &self,
        client: Option<Client>,
        cli_challenge: Option<String>,
    ) -> Option<(String, String)> {
        let mut flows = self.flows();
        flows
            .pending
            .retain(|_, p| p.started.elapsed() < PENDING_TTL);
        if flows.pending.len() >= MAX_PENDING {
            return None;
        }
        let pkce = generate_pkce_pair();
        let state = generate_state();
        let url = build_authorization_url(&self.settings, &state, &pkce);
        flows.pending.insert(
            state.clone(),
            Pending {
                started: Instant::now(),
                pkce,
                client,
                cli_challenge,
                failures: 0,
            },
        );
        Some((url, state))
    }

    /// The sign-in started with `state`, which stays in progress; `None` if it is unknown
    /// or too old.
    fn pending(&self, state: &str) -> Option<Pending> {
        let pending = self.flows().pending.get(state)?.clone();
        (pending.started.elapsed() < PENDING_TTL).then_some(pending)
    }

    /// End the sign-in started with `state`. `false` means it was already gone: a
    /// concurrent request completed it first.
    fn finish(&self, state: &str) -> bool {
        self.flows().pending.remove(state).is_some()
    }

    /// Count a code Zendesk refused for the sign-in started with `state`. `false` means
    /// the sign-in has now ended.
    fn note_failure(&self, state: &str) -> bool {
        let mut flows = self.flows();
        let Some(pending) = flows.pending.get_mut(state) else {
            return false;
        };
        pending.failures += 1;
        let ended = pending.failures >= MAX_FAILURES;
        if ended {
            flows.pending.remove(state);
        }
        !ended
    }

    /// Exchange Zendesk's authorization `code` for tokens, and look up who they belong to.
    async fn redeem(&self, pkce: &PkcePair, code: &str) -> Result<SignedIn> {
        let tokens = exchange_code_at(
            &self.http,
            &format!("{}/oauth/tokens", self.zendesk),
            &self.settings,
            code,
            pkce,
        )
        .await?;
        let client = ZendeskClient::with_base_url(
            &self.settings.subdomain,
            Auth::bearer(&tokens.access_token),
            self.http.clone(),
            format!("{}/api/v2", self.zendesk),
        );
        let user = client.get_current_user().await?;
        Ok(SignedIn { tokens, user })
    }

    /// Keep a sign-in on disk under a new server token, and return that token.
    fn store(&self, signed_in: &SignedIn) -> Result<String> {
        self.sweep_grants();
        let token = format!("{SERVER_TOKEN_PREFIX}{}", generate_state());
        let grants = self.grants();
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&grants)?;
        let dir = self.grant_dir(&token);
        TokenStore::new(dir.join("tokens.json")).save(&signed_in.tokens)?;
        let user = &signed_in.user;
        let record = json!({
            "id": user["id"],
            "name": user["name"],
            "email": user["email"],
            "signed_in_at": chrono::Utc::now().to_rfc3339(),
        });
        zendesk::tokens::write_private(
            &dir.join("user.json"),
            serde_json::to_string_pretty(&record)?.as_bytes(),
        )?;
        Ok(token)
    }

    /// Delete the stored logins whose `tokens.json` has not been written for
    /// [`GRANT_IDLE`]; a login in use is rewritten whenever its access token renews.
    fn sweep_grants(&self) {
        let Ok(entries) = std::fs::read_dir(self.grants()) else {
            return;
        };
        let removed = entries
            .flatten()
            .filter(|entry| {
                std::fs::metadata(entry.path().join("tokens.json"))
                    .and_then(|m| m.modified())
                    .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age > GRANT_IDLE))
            })
            .filter(|entry| std::fs::remove_dir_all(entry.path()).is_ok())
            .count();
        if removed > 0 {
            tracing::info!("Removed {removed} Zendesk logins unused for 90 days");
        }
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
        let tokens_file = dir.join("tokens.json");
        if !tokens_file.exists() {
            return Ok(None);
        }
        let settings = OAuthSettings {
            token_file: tokens_file.clone(),
            ..self.settings.clone()
        };
        let provider = OAuthProvider::new(settings, self.http.clone())
            .with_token_endpoint(&format!("{}/oauth/tokens", self.zendesk));
        match provider.access_token().await {
            Ok(_) => Ok(Some(Auth::OAuth(Arc::new(provider)))),
            Err(err) if err.is::<ReauthRequired>() => {
                match std::fs::remove_dir_all(&dir) {
                    Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                        return Err(err.into());
                    }
                    _ => {}
                }
                tracing::info!("Removed a Zendesk login that can no longer be renewed");
                Ok(None)
            }
            Err(_) if !tokens_file.exists() => {
                // A concurrent request removed the login; waiting on its lock made the
                // directory again.
                let _ = std::fs::remove_dir_all(&dir);
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

/// Whether `uri` is an `http` URL on this machine, the only kind of redirect an MCP
/// client may register.
fn is_loopback_redirect(uri: &str) -> bool {
    reqwest::Url::parse(uri).is_ok_and(|url| {
        url.scheme() == "http"
            && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
            && url.fragment().is_none()
    })
}

fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// An HTML answer that browsers will not cache, leak the address of or frame.
fn html(status: StatusCode, body: String) -> Response {
    (
        status,
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::X_FRAME_OPTIONS, "DENY"),
            (header::CONTENT_SECURITY_POLICY, "frame-ancestors 'none'"),
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
        ],
        body,
    )
        .into_response()
}

fn error_page(status: StatusCode, message: &str) -> Response {
    html(
        status,
        format!(
            "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><title>Sign in to Zendesk</title></head>\n<body>\n<h1>Sign in to Zendesk</h1>\n<p>{}</p>\n</body></html>\n",
            escape_html(message)
        ),
    )
}

/// An OAuth error answer: status 400 with `error` and `error_description`, uncached.
fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "error": error, "error_description": description })),
    )
        .into_response()
}

fn bad_request(error: &str, description: &str) -> Response {
    oauth_error(StatusCode::BAD_REQUEST, error, description)
}

/// `POST /register`: dynamic client registration. Nothing is stored, because every
/// client is public and may only redirect to this machine.
async fn register(body: Bytes) -> Response {
    let Ok(Value::Object(mut metadata)) = serde_json::from_slice(&body) else {
        return bad_request(
            "invalid_client_metadata",
            "The request body must be a JSON object.",
        );
    };
    let uris = metadata.get("redirect_uris").and_then(Value::as_array);
    let valid = uris.is_some_and(|uris| {
        !uris.is_empty()
            && uris
                .iter()
                .all(|uri| uri.as_str().is_some_and(is_loopback_redirect))
    });
    if !valid {
        return bad_request(
            "invalid_redirect_uri",
            "redirect_uris must be a non-empty list of http URLs on 127.0.0.1, localhost or [::1].",
        );
    }
    metadata.insert("client_id".into(), generate_state().into());
    metadata.insert(
        "client_id_issued_at".into(),
        chrono::Utc::now().timestamp().into(),
    );
    metadata.insert("token_endpoint_auth_method".into(), "none".into());
    (StatusCode::CREATED, Json(Value::Object(metadata))).into_response()
}

/// `GET /authorize`: the page that sends the user to Zendesk and takes the pasted result.
async fn authorize_page(
    State(sign_in): State<Arc<SignIn>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let param = |name: &str| {
        params
            .get(name)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    };
    let invalid = if params.get("response_type").map(String::as_str) != Some("code") {
        Some("response_type must be code")
    } else if param("client_id").is_none() {
        Some("client_id is missing")
    } else if !param("redirect_uri").is_some_and(is_loopback_redirect) {
        Some("redirect_uri must be a loopback http URL")
    } else if param("code_challenge").is_none() {
        Some("code_challenge is missing")
    } else if param("code_challenge_method") != Some("S256") {
        Some("code_challenge_method must be S256")
    } else if ["client_id", "state", "code_challenge"]
        .iter()
        .any(|name| param(name).is_some_and(|v| v.len() > 256))
        || param("redirect_uri").is_some_and(|v| v.len() > 2048)
    {
        Some("a parameter is too long")
    } else {
        None
    };
    if let Some(reason) = invalid {
        return error_page(
            StatusCode::BAD_REQUEST,
            &format!(
                "This sign-in request is not valid ({reason}). Start the sign-in again from your MCP client."
            ),
        );
    }
    let client = Client {
        client_id: params["client_id"].clone(),
        redirect_uri: params["redirect_uri"].clone(),
        code_challenge: params["code_challenge"].clone(),
        state: params.get("state").cloned(),
    };
    let Some((zendesk_url, state)) = sign_in.start(Some(client), None) else {
        return error_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Too many sign-ins are in progress. Try again in a few minutes.",
        );
    };
    html(
        StatusCode::OK,
        format!(
            r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Sign in to Zendesk</title></head>
<body>
<h1>Sign in to Zendesk</h1>
<ol>
<li><a href="{}" target="_blank" rel="noopener noreferrer">Sign in to Zendesk</a> in a new tab and allow access.</li>
<li>That tab then fails to load a <code>localhost</code> page. Copy its address and paste it here within 2 minutes:
<form method="post" action="/authorize">
<input type="hidden" name="session" value="{}">
<input type="text" name="redirect" required autofocus size="60" placeholder="{}?code=...">
<button type="submit">Continue</button>
</form></li>
</ol>
<p>To sign in without pasting, run <code>zendesk login {}/mcp</code> and use the token it prints.</p>
</body></html>
"#,
            escape_html(&zendesk_url),
            escape_html(&state),
            escape_html(&sign_in.settings.redirect_uri),
            escape_html(&sign_in.public),
        ),
    )
}

/// `POST /authorize`: finish the sign-in with the address the user pasted, and send the
/// MCP client back to its redirect with a one-time code.
async fn authorize_paste(
    State(sign_in): State<Arc<SignIn>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let field = |name: &str| form.get(name).map(String::as_str).unwrap_or_default();
    let session = field("session");
    const EXPIRED: &str =
        "This sign-in expired or was already used. Start the sign-in again from your MCP client.";
    let Some((pending, client)) = sign_in
        .pending(session)
        .and_then(|p| p.client.clone().map(|client| (p, client)))
    else {
        return error_page(StatusCode::BAD_REQUEST, EXPIRED);
    };
    let Ok(mut redirect) = reqwest::Url::parse(&client.redirect_uri) else {
        return error_page(
            StatusCode::BAD_REQUEST,
            "This sign-in request is not valid. Start the sign-in again from your MCP client.",
        );
    };
    let code = match parse_pasted(field("redirect"))
        .and_then(|captured| validate_callback(&captured, session))
    {
        Ok(code) => code,
        Err(err) => {
            return error_page(
                StatusCode::BAD_REQUEST,
                &format!("Signing in failed: {err}. Go back and try again."),
            );
        }
    };
    let signed_in = match sign_in.redeem(&pending.pkce, &code).await {
        Ok(signed_in) => signed_in,
        Err(err) => {
            let next = if sign_in.note_failure(session) {
                "Go back and try again."
            } else {
                "This sign-in has ended after too many failed attempts. Start the sign-in again from your MCP client."
            };
            let reason = if err.is::<ReauthRequired>() {
                "Zendesk did not accept the code: it expired (codes last 2 minutes) or was already used.".to_string()
            } else {
                format!("Signing in to Zendesk failed: {err}.")
            };
            return error_page(StatusCode::BAD_REQUEST, &format!("{reason} {next}"));
        }
    };
    if !sign_in.finish(session) {
        return error_page(StatusCode::BAD_REQUEST, EXPIRED);
    }
    let code = generate_state();
    {
        let mut query = redirect.query_pairs_mut();
        query.append_pair("code", &code);
        if let Some(state) = &client.state {
            query.append_pair("state", state);
        }
    }
    {
        let mut flows = sign_in.flows();
        flows.codes.retain(|_, c| c.issued.elapsed() < CODE_TTL);
        flows.codes.insert(
            code,
            Issued {
                issued: Instant::now(),
                client,
                signed_in,
            },
        );
    }
    (
        StatusCode::SEE_OTHER,
        [
            (header::LOCATION, redirect.as_str()),
            (header::CACHE_CONTROL, "no-store"),
        ],
    )
        .into_response()
}

/// `POST /token`: trade the one-time code for a server token.
async fn token(
    State(sign_in): State<Arc<SignIn>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let field = |name: &str| form.get(name).map(String::as_str);
    if field("grant_type") != Some("authorization_code") {
        return bad_request(
            "unsupported_grant_type",
            "Only the authorization_code grant is supported.",
        );
    }
    let issued = sign_in
        .flows()
        .codes
        .remove(field("code").unwrap_or_default());
    let Some(issued) = issued.filter(|i| i.issued.elapsed() < CODE_TTL) else {
        return bad_request(
            "invalid_grant",
            "The code is unknown, expired or already used.",
        );
    };
    let verified = field("code_verifier")
        .is_some_and(|verifier| challenge_for(verifier) == issued.client.code_challenge);
    if !verified {
        return bad_request("invalid_grant", "PKCE verification failed.");
    }
    if field("redirect_uri").is_some_and(|uri| uri != issued.client.redirect_uri) {
        return bad_request("invalid_grant", "redirect_uri does not match the sign-in.");
    }
    if field("client_id").is_some_and(|id| id != issued.client.client_id) {
        return bad_request("invalid_grant", "client_id does not match the sign-in.");
    }
    match sign_in.store(&issued.signed_in) {
        Ok(token) => (
            [(header::CACHE_CONTROL, "no-store")],
            Json(json!({ "access_token": token, "token_type": "Bearer" })),
        )
            .into_response(),
        Err(err) => {
            tracing::error!("Could not store a Zendesk login: {err:#}");
            oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "Could not store the Zendesk login.",
            )
        }
    }
}

/// `POST /cli/login`: start a sign-in for `zendesk login`, which catches the Zendesk
/// redirect on the user's machine.
async fn cli_login(State(sign_in): State<Arc<SignIn>>, body: Bytes) -> Response {
    let body: HashMap<String, String> = serde_json::from_slice(&body).unwrap_or_default();
    let challenge = body
        .get("code_challenge")
        .filter(|c| !c.is_empty() && c.len() <= 256)
        .filter(|_| body.get("code_challenge_method").map(String::as_str) == Some("S256"));
    let Some(challenge) = challenge else {
        return bad_request(
            "invalid_request",
            "Send a code_challenge (at most 256 bytes) with code_challenge_method S256.",
        );
    };
    let Some((authorize_url, state)) = sign_in.start(None, Some(challenge.clone())) else {
        return oauth_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "temporarily_unavailable",
            "Too many sign-ins are in progress. Try again in a few minutes.",
        );
    };
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({
            "authorize_url": authorize_url,
            "state": state,
            "redirect_uri": sign_in.settings.redirect_uri,
        })),
    )
        .into_response()
}

/// `POST /cli/login/finish`: trade the code `zendesk login` caught for a server token.
async fn cli_login_finish(
    State(sign_in): State<Arc<SignIn>>,
    Json(body): Json<HashMap<String, String>>,
) -> Response {
    let field = |name: &str| body.get(name).map(String::as_str).unwrap_or_default();
    let state = field("state");
    let Some((pending, challenge)) = sign_in
        .pending(state)
        .and_then(|p| p.cli_challenge.clone().map(|challenge| (p, challenge)))
    else {
        return bad_request(
            "invalid_grant",
            "This sign-in expired or was already used. Run zendesk login again.",
        );
    };
    if challenge_for(field("code_verifier")) != challenge {
        sign_in.finish(state);
        return bad_request(
            "invalid_grant",
            "PKCE verification failed. Run zendesk login again.",
        );
    }
    let signed_in = match sign_in.redeem(&pending.pkce, field("code")).await {
        Ok(signed_in) => signed_in,
        Err(err) => {
            let next = if sign_in.note_failure(state) {
                "Run zendesk login again."
            } else {
                "This sign-in has ended after too many failed attempts. Run zendesk login again."
            };
            let reason = if err.is::<ReauthRequired>() {
                "Zendesk did not accept the code: it expired (codes last 2 minutes) or was already used.".to_string()
            } else {
                format!("Signing in to Zendesk failed: {err}.")
            };
            return bad_request("invalid_grant", &format!("{reason} {next}"));
        }
    };
    if !sign_in.finish(state) {
        return bad_request("invalid_grant", "This sign-in was already used.");
    }
    match sign_in.store(&signed_in) {
        Ok(token) => (
            [(header::CACHE_CONTROL, "no-store")],
            Json(json!({
                "access_token": token,
                "user": {
                    "id": signed_in.user["id"],
                    "name": signed_in.user["name"],
                    "email": signed_in.user["email"],
                },
            })),
        )
            .into_response(),
        Err(err) => {
            tracing::error!("Could not store a Zendesk login: {err:#}");
            oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "Could not store the Zendesk login.",
            )
        }
    }
}

/// The first of the credential variables besides Zendesk OAuth that `get` finds set to
/// something other than blank, which rules out sign-in mode.
fn other_credentials_set(get: impl Fn(&str) -> Option<String>) -> Option<&'static str> {
    [
        "ZENDESK_OAUTH_TOKEN",
        "ZENDESK_EMAIL",
        "ZENDESK_API_KEY",
        "ZENDESK_SESSION_COOKIE",
    ]
    .into_iter()
    .find(|name| get(name).is_some_and(|v| !v.trim().is_empty()))
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
        harness_with_redirect("http://localhost:19186/").await
    }

    async fn harness_with_redirect(redirect_uri: &str) -> Harness {
        let zendesk = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let public = format!("http://{}", listener.local_addr().unwrap());
        let settings = OAuthSettings {
            subdomain: "acme".into(),
            client_id: "zdg-zcli-oauth".into(),
            token_file: dir.path().join("tokens.json"),
            scopes: "read write".into(),
            redirect_uri: redirect_uri.into(),
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
    async fn concurrent_requests_on_a_dead_login_get_401() {
        let h = harness().await;
        expired_login(&h);
        refresh_mock()
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({ "error": "invalid_grant" })),
            )
            .mount(&h.zendesk)
            .await;
        let (a, b) = tokio::join!(post_raw(&h, TOKEN), post_raw(&h, TOKEN));
        assert_eq!(a.status(), 401);
        assert_eq!(b.status(), 401);
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

    const REDIRECT: &str = "http://127.0.0.1:33333/callback";

    /// Mount Zendesk's token endpoint for code `zcode`, and `users/me` for the token it issues.
    async fn mock_zendesk_sign_in(zendesk: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/oauth/tokens"))
            .and(body_string_contains("code=zcode"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "zd-access", "refresh_token": "zd-refresh",
                "expires_in": 1800, "refresh_token_expires_in": 7776000,
                "token_type": "bearer",
            })))
            .mount(zendesk)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/me.json"))
            .and(header("authorization", "Bearer zd-access"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({ "user": { "id": 7, "name": "Alice", "email": "alice@example.com" } }),
            ))
            .mount(zendesk)
            .await;
    }

    fn assert_page_headers(response: &reqwest::Response) {
        for (name, value) in [
            ("cache-control", "no-store"),
            ("referrer-policy", "no-referrer"),
            ("x-frame-options", "DENY"),
            ("content-security-policy", "frame-ancestors 'none'"),
            ("content-type", "text/html; charset=utf-8"),
        ] {
            assert_eq!(
                response.headers().get(name).and_then(|v| v.to_str().ok()),
                Some(value),
                "{name}"
            );
        }
    }

    /// Register a client, load the sign-in page for `verifier`, and return its `session`.
    async fn open_page(h: &Harness, verifier: &str) -> String {
        let registered = h
            .http
            .post(format!("{}/register", h.url))
            .json(&json!({ "redirect_uris": [REDIRECT] }))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        let response = get_authorize(
            h,
            &[
                ("response_type", "code"),
                ("client_id", registered["client_id"].as_str().unwrap()),
                ("redirect_uri", REDIRECT),
                ("code_challenge", challenge_for(verifier).as_str()),
                ("code_challenge_method", "S256"),
                ("state", "client-state"),
            ],
        )
        .await;
        assert_eq!(response.status(), 200);
        assert_page_headers(&response);
        let page = response.text().await.unwrap();
        assert!(page.contains("zendesk login http://127.0.0.1:"), "{page}");
        assert!(
            page.contains(r#"placeholder="http://localhost:19186/?code=...""#),
            "{page}"
        );
        let marker = "name=\"session\" value=\"";
        let start = page.find(marker).unwrap() + marker.len();
        page[start..][..page[start..].find('"').unwrap()].to_string()
    }

    /// `GET /authorize` with `params` as its query.
    async fn get_authorize(h: &Harness, params: &[(&str, &str)]) -> reqwest::Response {
        let mut url = reqwest::Url::parse(&format!("{}/authorize", h.url)).unwrap();
        url.query_pairs_mut().extend_pairs(params);
        h.http.get(url).send().await.unwrap()
    }

    async fn paste(h: &Harness, session: &str, pasted: &str) -> reqwest::Response {
        h.http
            .post(format!("{}/authorize", h.url))
            .form(&[("session", session), ("redirect", pasted)])
            .send()
            .await
            .unwrap()
    }

    async fn token(h: &Harness, form: &[(&str, &str)]) -> reqwest::Response {
        h.http
            .post(format!("{}/token", h.url))
            .form(form)
            .send()
            .await
            .unwrap()
    }

    /// Mount a Zendesk token endpoint that must never be reached.
    async fn zendesk_must_not_be_called(h: &Harness) {
        Mock::given(method("POST"))
            .and(path("/oauth/tokens"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&h.zendesk)
            .await;
    }

    /// Sign in with `verifier` and return the one-time code from the redirect.
    async fn sign_in_code(h: &Harness, verifier: &str) -> String {
        mock_zendesk_sign_in(&h.zendesk).await;
        let session = open_page(h, verifier).await;
        let response = paste(
            h,
            &session,
            &format!("http://localhost:19186/?code=zcode&state={session}"),
        )
        .await;
        assert_eq!(response.status(), 303);
        let location = response.headers()["location"].to_str().unwrap();
        assert!(location.starts_with(REDIRECT), "{location}");
        assert!(location.contains("state=client-state"), "{location}");
        let url = reqwest::Url::parse(location).unwrap();
        url.query_pairs()
            .find(|(k, _)| k == "code")
            .unwrap()
            .1
            .into_owned()
    }

    #[test]
    fn loopback_redirects_only() {
        for uri in [
            "http://127.0.0.1:1234/callback",
            "http://localhost/cb",
            "http://[::1]:8080/x",
            "http://127.0.0.1:19876/mcp/oauth/callback",
        ] {
            assert!(is_loopback_redirect(uri), "{uri}");
        }
        for uri in [
            "https://127.0.0.1/cb",
            "http://localhost.evil.com/cb",
            "http://evil.com/cb",
            "http://user@evil.com/",
            "http://127.0.0.1/cb#x",
            "cursor://callback",
            "javascript:alert(1)",
            "",
        ] {
            assert!(!is_loopback_redirect(uri), "{uri}");
        }
    }

    #[tokio::test]
    async fn register_accepts_loopback_clients_only() {
        let h = harness().await;
        let register = |body: &'static str| {
            h.http
                .post(format!("{}/register", h.url))
                .header("content-type", "application/json")
                .body(body)
                .send()
        };
        let created = register(
            r#"{"redirect_uris":["http://127.0.0.1:33333/callback"],"client_name":"test"}"#,
        )
        .await
        .unwrap();
        assert_eq!(created.status(), 201);
        let created = created.json::<Value>().await.unwrap();
        assert!(!created["client_id"].as_str().unwrap().is_empty());
        assert_eq!(created["token_endpoint_auth_method"], "none");
        assert_eq!(created["client_name"], "test");

        for body in [
            r#"{"redirect_uris":["https://evil.com/cb"]}"#,
            r#"{"redirect_uris":[]}"#,
        ] {
            let response = register(body).await.unwrap();
            assert_eq!(response.status(), 400, "{body}");
            assert_eq!(
                response.json::<Value>().await.unwrap()["error"],
                "invalid_redirect_uri"
            );
        }
        let response = register("[1,2]").await.unwrap();
        assert_eq!(response.status(), 400);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"],
            "invalid_client_metadata"
        );
    }

    #[tokio::test]
    async fn authorize_rejects_invalid_requests() {
        let h = harness().await;
        let challenge = challenge_for("v");
        for (redirect_uri, method_) in [("https://evil.com/cb", "S256"), (REDIRECT, "plain")] {
            let response = get_authorize(
                &h,
                &[
                    ("response_type", "code"),
                    ("client_id", "c"),
                    ("redirect_uri", redirect_uri),
                    ("code_challenge", challenge.as_str()),
                    ("code_challenge_method", method_),
                ],
            )
            .await;
            assert_eq!(response.status(), 400, "{redirect_uri} {method_}");
            assert!(response.headers().get("location").is_none());
            assert_page_headers(&response);
        }
    }

    #[tokio::test]
    async fn client_sign_in_issues_a_working_token() {
        let h = harness().await;
        let verifier = "v".repeat(43);
        let code = sign_in_code(&h, &verifier).await;
        let form = [
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("code_verifier", verifier.as_str()),
            ("redirect_uri", REDIRECT),
        ];
        let response = token(&h, &form).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let body = response.json::<Value>().await.unwrap();
        let server_token = body["access_token"].as_str().unwrap().to_string();
        assert!(server_token.starts_with("zmcp_"));
        assert_eq!(body["token_type"], "Bearer");

        assert!(call_current_user(&h, &server_token).await.contains("Alice"));
        let user =
            std::fs::read_to_string(h.sign_in.grant_dir(&server_token).join("user.json")).unwrap();
        assert!(user.contains("\"email\": \"alice@example.com\""), "{user}");

        let mut stack = vec![h.sign_in.grants()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap().path();
                if entry.is_dir() {
                    stack.push(entry);
                } else {
                    let content = std::fs::read_to_string(&entry).unwrap();
                    assert!(!content.contains(&server_token), "{}", entry.display());
                }
            }
        }

        let again = token(&h, &form).await;
        assert_eq!(again.status(), 400);
        assert_eq!(
            again.json::<Value>().await.unwrap()["error"],
            "invalid_grant"
        );
    }

    #[tokio::test]
    async fn placeholder_follows_the_configured_redirect_url() {
        let h = harness_with_redirect("http://localhost:4000/cb?x=1&y=2").await;
        let page = get_authorize(
            &h,
            &[
                ("response_type", "code"),
                ("client_id", "c"),
                ("redirect_uri", REDIRECT),
                ("code_challenge", challenge_for("v").as_str()),
                ("code_challenge_method", "S256"),
            ],
        )
        .await
        .text()
        .await
        .unwrap();
        assert!(
            page.contains(r#"placeholder="http://localhost:4000/cb?x=1&amp;y=2?code=...""#),
            "{page}"
        );
    }

    #[tokio::test]
    async fn token_rejects_a_different_redirect_uri_or_client_id() {
        let h = harness().await;
        let verifier = "v".repeat(43);
        let code = sign_in_code(&h, &verifier).await;
        let response = token(
            &h,
            &[
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("code_verifier", verifier.as_str()),
                ("redirect_uri", "http://127.0.0.1:44444/callback"),
            ],
        )
        .await;
        assert_eq!(response.status(), 400);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"],
            "invalid_grant"
        );

        let code = sign_in_code(&h, &verifier).await;
        let response = token(
            &h,
            &[
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("code_verifier", verifier.as_str()),
                ("client_id", "another-client"),
            ],
        )
        .await;
        assert_eq!(response.status(), 400);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"],
            "invalid_grant"
        );
    }

    #[tokio::test]
    async fn token_rejects_other_grant_types() {
        let h = harness().await;
        let response = token(&h, &[("grant_type", "refresh_token")]).await;
        assert_eq!(response.status(), 400);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"],
            "unsupported_grant_type"
        );
    }

    #[tokio::test]
    async fn wrong_verifier_burns_the_code() {
        let h = harness().await;
        let verifier = "v".repeat(43);
        let code = sign_in_code(&h, &verifier).await;
        for attempt in ["w".repeat(43), verifier] {
            let response = token(
                &h,
                &[
                    ("grant_type", "authorization_code"),
                    ("code", code.as_str()),
                    ("code_verifier", attempt.as_str()),
                ],
            )
            .await;
            assert_eq!(response.status(), 400);
            assert_eq!(
                response.json::<Value>().await.unwrap()["error"],
                "invalid_grant"
            );
        }
    }

    #[tokio::test]
    async fn paste_accepts_a_bare_code() {
        let h = harness().await;
        mock_zendesk_sign_in(&h.zendesk).await;
        let session = open_page(&h, &"v".repeat(43)).await;
        assert_eq!(paste(&h, &session, "  zcode \n").await.status(), 303);
    }

    #[tokio::test]
    async fn paste_rejects_another_sign_ins_address() {
        let h = harness().await;
        zendesk_must_not_be_called(&h).await;
        let session = open_page(&h, &"v".repeat(43)).await;
        let response = paste(
            &h,
            &session,
            "http://localhost:19186/?code=zcode&state=other",
        )
        .await;
        assert_eq!(response.status(), 400);
        assert_page_headers(&response);
        assert!(response.text().await.unwrap().contains("does not match"));
    }

    #[tokio::test]
    async fn paste_reports_a_denied_sign_in() {
        let h = harness().await;
        zendesk_must_not_be_called(&h).await;
        let session = open_page(&h, &"v".repeat(43)).await;
        let pasted = format!(
            "http://localhost:19186/?error=access_denied&error_description=The+user+denied&state={session}"
        );
        let response = paste(&h, &session, &pasted).await;
        assert_eq!(response.status(), 400);
        assert!(response.text().await.unwrap().contains("declined"));
    }

    #[tokio::test]
    async fn late_paste_says_the_code_expired() {
        let h = harness().await;
        Mock::given(method("POST"))
            .and(path("/oauth/tokens"))
            .and(body_string_contains("code=late"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({ "error": "invalid_grant" })),
            )
            .mount(&h.zendesk)
            .await;
        let session = open_page(&h, &"v".repeat(43)).await;
        let response = paste(&h, &session, "late").await;
        assert_eq!(response.status(), 400);
        let page = response.text().await.unwrap();
        assert!(page.contains("expired"), "{page}");
        assert!(!page.contains("zendesk-mcp-server auth"), "{page}");
    }

    #[tokio::test]
    async fn used_session_cannot_be_pasted_twice() {
        let h = harness().await;
        mock_zendesk_sign_in(&h.zendesk).await;
        let session = open_page(&h, &"v".repeat(43)).await;
        assert_eq!(paste(&h, &session, "zcode").await.status(), 303);
        let response = paste(&h, &session, "zcode").await;
        assert_eq!(response.status(), 400);
        assert!(
            response
                .text()
                .await
                .unwrap()
                .contains("expired or was already used")
        );
    }

    #[tokio::test]
    async fn failed_paste_keeps_the_sign_in() {
        let h = harness().await;
        mock_zendesk_sign_in(&h.zendesk).await;
        let session = open_page(&h, &"v".repeat(43)).await;
        let wrong = paste(
            &h,
            &session,
            "http://localhost:19186/?code=zcode&state=other",
        )
        .await;
        assert_eq!(wrong.status(), 400);
        let right = paste(
            &h,
            &session,
            &format!("http://localhost:19186/?code=zcode&state={session}"),
        )
        .await;
        assert_eq!(right.status(), 303);
    }

    #[tokio::test]
    async fn third_failed_paste_ends_the_sign_in() {
        let h = harness().await;
        Mock::given(method("POST"))
            .and(path("/oauth/tokens"))
            .and(body_string_contains("code=late"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({ "error": "invalid_grant" })),
            )
            .mount(&h.zendesk)
            .await;
        mock_zendesk_sign_in(&h.zendesk).await;
        let session = open_page(&h, &"v".repeat(43)).await;
        for _ in 0..2 {
            let page = paste(&h, &session, "late").await.text().await.unwrap();
            assert!(!page.contains("has ended"), "{page}");
        }
        let page = paste(&h, &session, "late").await.text().await.unwrap();
        assert!(page.contains("has ended"), "{page}");
        // Even a good code finds nothing to finish.
        let response = paste(&h, &session, "zcode").await;
        assert_eq!(response.status(), 400);
        assert!(h.sign_in.pending(&session).is_none());
    }

    #[tokio::test]
    async fn late_paste_can_be_retried() {
        let h = harness().await;
        Mock::given(method("POST"))
            .and(path("/oauth/tokens"))
            .and(body_string_contains("code=late"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({ "error": "invalid_grant" })),
            )
            .mount(&h.zendesk)
            .await;
        mock_zendesk_sign_in(&h.zendesk).await;
        let session = open_page(&h, &"v".repeat(43)).await;
        assert_eq!(paste(&h, &session, "late").await.status(), 400);
        assert_eq!(paste(&h, &session, "zcode").await.status(), 303);
    }

    #[test]
    fn html_values_are_escaped() {
        assert_eq!(
            escape_html(r#"<a href="x">&'"#),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
    }

    #[tokio::test]
    async fn sign_ins_in_progress_are_capped() {
        let h = harness().await;
        for _ in 0..1000 {
            assert!(h.sign_in.start(None, None).is_some());
        }
        assert!(h.sign_in.start(None, None).is_none());
    }

    #[tokio::test]
    async fn cli_sign_in_issues_a_working_token() {
        let h = harness().await;
        mock_zendesk_sign_in(&h.zendesk).await;
        let verifier = "v".repeat(43);
        let started = cli_start(&h, &challenge_for(&verifier)).await;
        assert_eq!(started.status(), 200);
        let started = started.json::<Value>().await.unwrap();
        let authorize_url = started["authorize_url"].as_str().unwrap();
        assert!(authorize_url.contains("client_id=zdg-zcli-oauth"));
        assert!(authorize_url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A19186%2F"));
        assert_eq!(started["redirect_uri"], "http://localhost:19186/");

        let finish = || async {
            h.http
                .post(format!("{}/cli/login/finish", h.url))
                .json(&json!({
                    "state": started["state"],
                    "code": "zcode",
                    "code_verifier": verifier,
                }))
                .send()
                .await
                .unwrap()
        };
        let response = finish().await;
        assert_eq!(response.status(), 200);
        let body = response.json::<Value>().await.unwrap();
        let server_token = body["access_token"].as_str().unwrap();
        assert!(server_token.starts_with("zmcp_"));
        assert_eq!(body["user"]["name"], "Alice");
        assert_eq!(body["user"]["email"], "alice@example.com");
        assert!(call_current_user(&h, server_token).await.contains("Alice"));

        let again = finish().await;
        assert_eq!(again.status(), 400);
        assert_eq!(
            again.json::<Value>().await.unwrap()["error"],
            "invalid_grant"
        );
    }

    async fn cli_start(h: &Harness, challenge: &str) -> reqwest::Response {
        h.http
            .post(format!("{}/cli/login", h.url))
            .json(&json!({ "code_challenge": challenge, "code_challenge_method": "S256" }))
            .send()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn cli_login_requires_a_challenge() {
        let h = harness().await;
        let post = |body: Value| {
            h.http
                .post(format!("{}/cli/login", h.url))
                .json(&body)
                .send()
        };
        let long = "c".repeat(257);
        for body in [
            json!({}),
            json!({ "code_challenge": "abc" }),
            json!({ "code_challenge_method": "S256" }),
            json!({ "code_challenge": "abc", "code_challenge_method": "plain" }),
            json!({ "code_challenge": long, "code_challenge_method": "S256" }),
        ] {
            let response = post(body).await.unwrap();
            assert_eq!(response.status(), 400);
            assert_eq!(
                response.json::<Value>().await.unwrap()["error"],
                "invalid_request"
            );
        }
        let bare = h
            .http
            .post(format!("{}/cli/login", h.url))
            .send()
            .await
            .unwrap();
        assert_eq!(bare.status(), 400);
    }

    #[tokio::test]
    async fn cli_login_rejects_a_wrong_verifier() {
        let h = harness().await;
        zendesk_must_not_be_called(&h).await;
        let started = cli_start(&h, &challenge_for(&"v".repeat(43)))
            .await
            .json::<Value>()
            .await
            .unwrap();
        let finish = |verifier: &str| {
            h.http
                .post(format!("{}/cli/login/finish", h.url))
                .json(&json!({
                    "state": started["state"],
                    "code": "zcode",
                    "code_verifier": verifier,
                }))
                .send()
        };
        let response = finish(&"w".repeat(43)).await.unwrap();
        assert_eq!(response.status(), 400);
        let body = response.json::<Value>().await.unwrap();
        assert_eq!(body["error"], "invalid_grant");
        assert!(
            body["error_description"]
                .as_str()
                .unwrap()
                .contains("PKCE verification failed")
        );
        // The wrong guess ended the sign-in, so the right verifier is too late.
        assert_eq!(finish(&"v".repeat(43)).await.unwrap().status(), 400);
        assert!(h.zendesk.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn authorize_rejects_overlong_parameters() {
        let h = harness().await;
        let challenge = "c".repeat(257);
        let response = h
            .http
            .get(format!(
                "{}/authorize?response_type=code&client_id=c&redirect_uri=http://localhost:1/&code_challenge={challenge}&code_challenge_method=S256",
                h.url
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        assert!(response.text().await.unwrap().contains("too long"));
    }

    #[tokio::test]
    async fn oversized_sign_in_bodies_are_refused() {
        let h = harness().await;
        let response = h
            .http
            .post(format!("{}/register", h.url))
            .body("x".repeat(20 * 1024))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 413);
    }

    #[tokio::test]
    async fn stored_logins_are_private() {
        let h = harness().await;
        let signed_in = SignedIn {
            tokens: zd_tokens("zd-a", "r1", 3600),
            user: json!({ "id": 7, "name": "Alice", "email": "alice@example.com" }),
        };
        let token = h.sign_in.store(&signed_in).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(h.sign_in.grants()), 0o700);
            assert_eq!(mode(h.sign_in.grant_dir(&token)), 0o700);
            assert_eq!(mode(h.sign_in.grant_dir(&token).join("user.json")), 0o600);
        }
        assert!(h.sign_in.grant_dir(&token).join("user.json").exists());
    }

    #[tokio::test]
    async fn idle_logins_are_swept() {
        let h = harness().await;
        seed(&h, "zmcp_old", &zd_tokens("zd-old", "r1", 3600));
        seed(&h, "zmcp_fresh", &zd_tokens("zd-ok", "r1", 3600));
        let stale = std::time::SystemTime::now() - GRANT_IDLE - Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(h.sign_in.grant_dir("zmcp_old").join("tokens.json"))
            .unwrap()
            .set_modified(stale)
            .unwrap();
        mock_me(&h, "zd-ok").await;
        h.sign_in.sweep_grants();
        assert!(!h.sign_in.grant_dir("zmcp_old").exists());
        assert!(h.sign_in.grant_dir("zmcp_fresh").exists());
        assert!(call_current_user(&h, "zmcp_fresh").await.contains("Alice"));
    }

    #[test]
    fn other_credential_variables_rule_out_sign_in() {
        let with = |set: &'static [(&'static str, &'static str)]| {
            other_credentials_set(move |name| {
                set.iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| v.to_string())
            })
        };
        assert_eq!(with(&[]), None);
        assert_eq!(with(&[("ZENDESK_API_KEY", "  ")]), None);
        assert_eq!(with(&[("ZENDESK_SUBDOMAIN", "acme")]), None);
        for name in [
            "ZENDESK_OAUTH_TOKEN",
            "ZENDESK_EMAIL",
            "ZENDESK_API_KEY",
            "ZENDESK_SESSION_COOKIE",
        ] {
            let set: &'static [(&str, &str)] = Box::leak(Box::new([(name, "x")]));
            assert_eq!(with(set), Some(name));
        }
    }

    #[tokio::test]
    async fn flows_do_not_cross() {
        let h = harness().await;
        let session = open_page(&h, &"v".repeat(43)).await;
        let response = h
            .http
            .post(format!("{}/cli/login/finish", h.url))
            .json(&json!({ "state": session, "code": "zcode" }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"],
            "invalid_grant"
        );

        let started = cli_start(&h, &challenge_for(&"v".repeat(43)))
            .await
            .json::<Value>()
            .await
            .unwrap();
        let response = paste(&h, started["state"].as_str().unwrap(), "zcode").await;
        assert_eq!(response.status(), 400);
        assert_page_headers(&response);
        assert!(h.zendesk.received_requests().await.unwrap().is_empty());

        mock_zendesk_sign_in(&h.zendesk).await;
        assert_eq!(paste(&h, &session, "zcode").await.status(), 303);
    }
}
