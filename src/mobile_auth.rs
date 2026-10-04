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

use std::collections::HashMap;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub const CLIENT_ID: &str = "zendesk_support_android";
pub const USER_AGENT: &str = "Zendesk-SDK/1.0 Android/30 Variant/Core";

/// Default wait for the browser flow.
pub const BROWSER_TIMEOUT: Duration = Duration::from_secs(300);

/// Wait for the interactive `mobile-auth` command.
const INTERACTIVE_TIMEOUT: Duration = Duration::from_secs(300);

const URL_SCHEME: &str = "zendesk-support";

const SUCCESS_HTML: &str = r##"<!DOCTYPE html>
<html><head><title>Zendesk Auth</title>
<script>
// Check if we have a zendesk-support URL in the URL hash (Safari workaround)
window.addEventListener('DOMContentLoaded', function() {
    var hash = window.location.hash;
    if (hash && hash.startsWith('#zendesk-support://')) {
        var callbackUrl = decodeURIComponent(hash.substring(1));
        console.log('Captured callback URL from hash:', callbackUrl);
        // Forward it to our local server
        fetch('/callback/__NONCE__?url=' + encodeURIComponent(callbackUrl))
            .then(function() {
                document.body.innerHTML = '<h2 style="color:#2e7d32">&#9989; Authentication successful!</h2>' +
                    '<p>Token captured and saved. You can close this tab.</p>';
            });
    }
});
</script>
</head>
<body style="font-family:system-ui,sans-serif;text-align:center;padding:60px">
<h2>&#9989; Authentication successful!</h2>
<p>You can close this tab. The MCP server is starting.</p>
</body></html>"##;

/// `__AUTH_URL__` is replaced by the login URL as a JSON string literal, `__NONCE__` by the
/// callback nonce.
const AUTH_PAGE_HTML: &str = r##"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Zendesk Auth</title>
<style>
  body { font-family: system-ui, sans-serif; max-width: 520px; margin: 40px auto; padding: 20px; }
  h2 { color: #333; }
  .step { margin: 16px 0; padding: 12px; background: #f5f5f5; border-radius: 8px; }
  .step-num { font-weight: bold; color: #03363D; }
  input { width: 100%; padding: 10px; font-size: 14px; box-sizing: border-box;
           border: 2px solid #ccc; border-radius: 6px; }
  input:focus { border-color: #03363D; outline: none; }
  button { padding: 12px 24px; font-size: 15px; cursor: pointer; border: none;
            background: #03363D; color: white; border-radius: 6px; margin-top: 8px; }
  button:hover { background: #04494F; }
  #result { display: none; text-align: center; padding: 40px; }
  #result h2 { color: #2e7d32; }
  .spinner { display: inline-block; width: 20px; height: 20px; border: 3px solid #ccc;
              border-top-color: #03363D; border-radius: 50%; animation: spin 0.8s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
</style>
<script>
var authWindow = null;
var pollInterval = null;

function openAuth() {
    authWindow = window.open(__AUTH_URL__, '_blank');
    document.getElementById('step2').style.display = 'block';
    // Poll for the popup navigating to the custom scheme (will fail with error)
    pollInterval = setInterval(function() {
        if (authWindow && authWindow.closed) {
            clearInterval(pollInterval);
        }
    }, 1000);
}

function submitUrl() {
    var url = document.getElementById('callback-url').value.trim();
    if (!url) return;
    document.getElementById('steps').style.display = 'none';
    document.getElementById('result').style.display = 'block';
    fetch('/callback/__NONCE__?url=' + encodeURIComponent(url))
        .then(r => r.json())
        .then(function(data) {
            var resultDiv = document.getElementById('result');
            if (data.ok) {
                resultDiv.textContent = '';
                var h = document.createElement('h2');
                h.textContent = '\u2705 Authentication successful!';
                var p = document.createElement('p');
                p.textContent = 'Welcome, ' + (data.username || 'agent') + '! You can close this tab.';
                resultDiv.appendChild(h);
                resultDiv.appendChild(p);
            } else {
                resultDiv.textContent = '';
                var h = document.createElement('h2');
                h.style.color = '#c62828';
                h.textContent = '\u274c Authentication failed';
                var p1 = document.createElement('p');
                p1.textContent = data.error || 'Could not parse token from URL.';
                var p2 = document.createElement('p');
                p2.textContent = 'Make sure you copied the full URL starting with zendesk-support://';
                resultDiv.appendChild(h);
                resultDiv.appendChild(p1);
                resultDiv.appendChild(p2);
                document.getElementById('steps').style.display = 'block';
                resultDiv.style.display = 'none';
            }
        });
}

// Allow Enter key in the input
document.addEventListener('DOMContentLoaded', function() {
    document.getElementById('callback-url').addEventListener('keypress', function(e) {
        if (e.key === 'Enter') submitUrl();
    });
});
</script>
</head>
<body>
<h2>Zendesk Authentication</h2>
<div id="steps">
  <div class="step">
    <p><span class="step-num">Step 1:</span> Sign in to Zendesk</p>
    <button onclick="openAuth()">Open Zendesk Login</button>
  </div>
  <div class="step" id="step2" style="display:none">
    <p><span class="step-num">Step 2:</span> After signing in, the browser will show an error page
    about <code>zendesk-support://</code> not being recognized.</p>
    <p><strong>Where to find the URL:</strong></p>
    <ul style="text-align:left; font-size:14px;">
      <li><strong>Address bar:</strong> Look for a URL starting with <code>zendesk-support://</code></li>
      <li><strong>Chrome console:</strong> Press F12 &rarr; Console tab &rarr; Look for "Failed to launch 'zendesk-support://...'"</li>
      <li><strong>Error page:</strong> Some browsers display the URL in the error message</li>
    </ul>
    <p>Copy the <strong>full URL</strong> (including all parameters) and paste it here:</p>
    <input type="text" id="callback-url" placeholder="zendesk-support://authenticate?access_token=...">
    <br>
    <button onclick="submitUrl()">Authenticate</button>
  </div>
</div>
<div id="result">
  <div class="spinner"></div>
  <p>Verifying...</p>
</div>
</body></html>"##;

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

// Manual Debug so the access token never reaches logs or panics.
impl fmt::Debug for MobileToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MobileToken")
            .field("subdomain", &self.subdomain)
            .field("access_token", &"<redacted>")
            .field("username", &self.username)
            .field("user_id", &self.user_id)
            .field("account_id", &self.account_id)
            .field("user_role", &self.user_role)
            .finish()
    }
}

/// `ZENDESK_MOBILE_TOKEN_FILE`, else `config::default_mobile_token_file()`.
pub fn token_path() -> PathBuf {
    token_path_from(|key| std::env::var(key).ok())
}

fn token_path_from(get: impl Fn(&str) -> Option<String>) -> PathBuf {
    match get("ZENDESK_MOBILE_TOKEN_FILE")
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    {
        Some(path) => crate::config::expand_home(&path),
        None => crate::config::default_mobile_token_file(),
    }
}

/// Saved token, if the file exists, parses, and has a subdomain and access token.
pub fn load_token() -> Option<MobileToken> {
    load_token_from(&token_path())
}

fn load_token_from(path: &Path) -> Option<MobileToken> {
    let text = std::fs::read_to_string(path).ok()?;
    let token: MobileToken = serde_json::from_str(&text).ok()?;
    (!token.subdomain.is_empty() && !token.access_token.is_empty()).then_some(token)
}

/// Write the token (mode 0600, directory 0700). Returns the path written.
pub fn save_token(token: &MobileToken) -> Result<PathBuf> {
    let path = token_path();
    save_token_to(token, &path)?;
    Ok(path)
}

fn save_token_to(token: &MobileToken, path: &Path) -> Result<()> {
    let payload = serde_json::to_string_pretty(token)?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(dir)
            .with_context(|| format!("Could not create {}", dir.display()))?;
    }

    let mut temp_name = path.file_name().unwrap_or_default().to_os_string();
    temp_name.push(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    let temp_path = path.with_file_name(temp_name);

    let write = || -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp_path)?;
        file.write_all(payload.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp_path, path)
    };
    write().map_err(|err| {
        let _ = std::fs::remove_file(&temp_path);
        anyhow!("Could not write {}: {err}", path.display())
    })
}

fn zendesk_base(subdomain: &str) -> String {
    format!("https://{subdomain}.zendesk.com")
}

/// GET `/api/v2/users/me.json` with the token: true on 200.
pub async fn verify_token(http: &reqwest::Client, subdomain: &str, access_token: &str) -> bool {
    verify_token_at(http, &zendesk_base(subdomain), access_token).await
}

async fn verify_token_at(http: &reqwest::Client, base: &str, access_token: &str) -> bool {
    let sent = http
        .get(format!("{base}/api/v2/users/me.json"))
        .bearer_auth(access_token)
        .timeout(Duration::from_secs(10))
        .send()
        .await;
    matches!(sent, Ok(resp) if resp.status() == reqwest::StatusCode::OK)
}

/// The `lookup` object from `/api/mobile/account/lookup.json` (sent with `USER_AGENT`).
pub async fn lookup_subdomain(http: &reqwest::Client, subdomain: &str) -> Result<Value> {
    lookup_at(http, &zendesk_base(subdomain), subdomain).await
}

/// `subdomain` only words the error messages.
async fn lookup_at(http: &reqwest::Client, base: &str, subdomain: &str) -> Result<Value> {
    let resp = http
        .get(format!("{base}/api/mobile/account/lookup.json"))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .with_context(|| format!("Could not reach {subdomain}.zendesk.com"))?;
    let status = resp.status();
    if !status.is_success() {
        match status.as_u16() {
            404 => bail!("subdomain '{subdomain}' not found"),
            403 => bail!("access forbidden (IP restriction or mobile access disabled)"),
            _ => bail!("HTTP {status}"),
        }
    }
    let mut body: Value = resp
        .json()
        .await
        .context("Unexpected account lookup response")?;
    match body.get_mut("lookup") {
        Some(lookup) => Ok(lookup.take()),
        None => bail!("Unexpected account lookup response: no 'lookup' object"),
    }
}

/// Machine name sent to Zendesk as the device name.
fn device_name() -> String {
    let from_file = std::fs::read_to_string("/etc/hostname").ok();
    let from_command = || {
        let out = Command::new("hostname").output().ok()?;
        String::from_utf8(out.stdout).ok()
    };
    [from_file, std::env::var("HOSTNAME").ok(), from_command()]
        .into_iter()
        .flatten()
        .map(|name| name.trim().to_string())
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| "zendesk-mcp".to_string())
}

/// POST `/access/oauth_mobile` with email and password.
pub async fn auth_email_password(
    http: &reqwest::Client,
    subdomain: &str,
    email: &str,
    password: &str,
) -> Result<MobileToken> {
    auth_email_password_at(http, &zendesk_base(subdomain), subdomain, email, password).await
}

async fn auth_email_password_at(
    http: &reqwest::Client,
    base: &str,
    subdomain: &str,
    email: &str,
    password: &str,
) -> Result<MobileToken> {
    let payload = json!({
        "clientId": CLIENT_ID,
        "user": {"email": email, "password": password},
        "device": {"name": device_name(), "identifier": uuid::Uuid::new_v4().to_string()},
        "nativeMobile": true,
    });
    let resp = http
        .post(format!("{base}/access/oauth_mobile"))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .json(&payload)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .with_context(|| format!("Could not reach {subdomain}.zendesk.com"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let detail: String = body.chars().take(500).collect();
        bail!("Authentication failed: HTTP {status}: {detail}");
    }
    let data: Value = resp
        .json()
        .await
        .context("Unexpected authentication response")?;
    let auth = data.get("authentication").unwrap_or(&data);
    let field = |keys: &[&str]| {
        keys.iter().find_map(|key| match auth.get(*key)? {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
    };
    Ok(MobileToken {
        subdomain: subdomain.to_string(),
        access_token: field(&["accessToken", "access_token"])
            .ok_or_else(|| anyhow!("no access token received"))?,
        username: field(&["username"]),
        user_id: field(&["userId", "user_id"]),
        account_id: field(&["accountId", "account_id"]),
        user_role: field(&["userRole", "user_role"]),
    })
}

/// Extract the token from a `zendesk-support://authenticate?access_token=...` URL.
/// Parameters may be in the query or, failing that, the fragment.
pub fn parse_oauth_callback(url: &str, subdomain: &str) -> Option<MobileToken> {
    tracing::info!("Parsing OAuth callback URL...");
    let parsed = match url::Url::parse(url) {
        Ok(parsed) => parsed,
        Err(_) => {
            tracing::error!("Callback URL is empty or not a valid URL");
            return None;
        }
    };
    tracing::info!(scheme = parsed.scheme(), length = url.len(), "Callback URL");

    let mut params: HashMap<String, String> = parsed.query_pairs().into_owned().collect();
    if params.is_empty()
        && let Some(fragment) = parsed.fragment()
    {
        params = url::form_urlencoded::parse(fragment.as_bytes())
            .into_owned()
            .collect();
    }

    let Some(access_token) = params.remove("access_token").filter(|t| !t.is_empty()) else {
        let mut names: Vec<&str> = params.keys().map(String::as_str).collect();
        names.sort_unstable();
        tracing::error!(?names, "No access_token found in callback parameters");
        return None;
    };
    tracing::info!("Access token found in callback URL");

    Some(MobileToken {
        subdomain: subdomain.to_string(),
        access_token,
        username: params.remove("username"),
        user_id: params.remove("user_id"),
        account_id: params.remove("account_id"),
        user_role: params.remove("user_role"),
    })
}

/// Append `client_id`, `device[name]` and `device[identifier]` to the login URL.
pub fn build_auth_url(auth_url: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", CLIENT_ID)
        .append_pair("device[name]", &device_name())
        .append_pair("device[identifier]", &uuid::Uuid::new_v4().to_string())
        .finish();
    let separator = if auth_url.contains('?') { '&' } else { '?' };
    format!("{auth_url}{separator}{query}")
}

// --- Local callback server ---

struct ServerState {
    nonce: String,
    subdomain: String,
    full_auth_url: String,
    /// Interactive mode answers `/callback` with JSON instead of the success page.
    interactive: bool,
    tokens: mpsc::UnboundedSender<MobileToken>,
}

/// Serves the paste page and receives the `zendesk-support://` callback. Stops on drop.
/// Both routes live under a per-run random nonce so other local processes and web pages
/// that scan ports cannot submit a token.
struct CallbackServer {
    port: u16,
    nonce: String,
    tokens: mpsc::UnboundedReceiver<MobileToken>,
    task: JoinHandle<()>,
}

impl Drop for CallbackServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn callback_url_for(port: u16, nonce: &str) -> String {
    format!("http://127.0.0.1:{port}/callback/{nonce}")
}

impl CallbackServer {
    fn callback_url(&self) -> String {
        callback_url_for(self.port, &self.nonce)
    }

    fn auth_url(&self) -> String {
        format!("http://127.0.0.1:{}/auth/{}", self.port, self.nonce)
    }
}

async fn start_callback_server(
    subdomain: &str,
    full_auth_url: String,
    interactive: bool,
) -> Result<CallbackServer> {
    let (tx, rx) = mpsc::unbounded_channel();
    let nonce = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let state = Arc::new(ServerState {
        nonce: nonce.clone(),
        subdomain: subdomain.to_string(),
        full_auth_url,
        interactive,
        tokens: tx,
    });
    let app = Router::new()
        .route(&format!("/callback/{nonce}"), get(callback_handler))
        .route(&format!("/auth/{nonce}"), get(auth_page_handler))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("Could not start the local callback server")?;
    let port = listener.local_addr()?.port();
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::warn!("Local callback server stopped: {e}");
        }
    });
    tracing::info!(port, "Local callback server started");
    Ok(CallbackServer {
        port,
        nonce,
        tokens: rx,
        task,
    })
}

fn render_auth_page(full_auth_url: &str, nonce: &str) -> String {
    // JSON-encode so quotes cannot break out of the JS string; '<' stops `</script>`.
    let literal = serde_json::to_string(full_auth_url)
        .unwrap_or_else(|_| "\"\"".to_string())
        .replace('<', "\\u003c");
    AUTH_PAGE_HTML
        .replace("__AUTH_URL__", &literal)
        .replace("__NONCE__", nonce)
}

async fn callback_handler(
    State(state): State<Arc<ServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let callback_url = params.get("url").map(String::as_str).unwrap_or_default();
    tracing::info!(length = callback_url.len(), "Callback received");
    for marker in ["SAMLRequest", "SAMLResponse"] {
        if callback_url.contains(marker) {
            tracing::warn!(
                "Received a SAML intermediate redirect ({marker}) instead of the OAuth callback; \
                 the SAML flow is not completing"
            );
        }
    }

    match parse_oauth_callback(callback_url, &state.subdomain) {
        Some(token) => {
            let username = token.username.clone();
            let _ = state.tokens.send(token);
            if state.interactive {
                Json(json!({"ok": true, "username": username})).into_response()
            } else {
                Html(SUCCESS_HTML.replace("__NONCE__", &state.nonce)).into_response()
            }
        }
        None => Json(json!({
            "ok": false,
            "error": "Could not parse access token from URL.",
        }))
        .into_response(),
    }
}

async fn auth_page_handler(State(state): State<Arc<ServerState>>) -> Html<String> {
    Html(render_auth_page(&state.full_auth_url, &state.nonce))
}

// --- Browser ---

fn spawn_detached(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

/// Open a URL in a private/incognito window so the mobile OAuth cookies stay out of the
/// operator's normal Zendesk browser session. Chrome comes first on macOS because its
/// SAML support is the most reliable.
pub fn open_in_private_window(url: &str) -> bool {
    let candidates: &[(&str, &str)] = if cfg!(target_os = "macos") {
        &[
            (
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                "--incognito",
            ),
            (
                "/Applications/Firefox.app/Contents/MacOS/firefox",
                "--private-window",
            ),
            (
                "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
                "--inprivate",
            ),
        ]
    } else if cfg!(target_os = "windows") {
        &[
            ("msedge", "--inprivate"),
            ("chrome", "--incognito"),
            ("firefox", "--private-window"),
        ]
    } else {
        // xdg-open cannot force a private window; it is first as in the Python version.
        &[
            ("xdg-open", ""),
            ("google-chrome", "--incognito"),
            ("firefox", "--private-window"),
            ("microsoft-edge", "--inprivate"),
        ]
    };
    candidates.iter().any(|(program, flag)| {
        let args: Vec<&str> = [*flag, url].into_iter().filter(|a| !a.is_empty()).collect();
        let opened = spawn_detached(program, &args);
        if opened {
            tracing::info!(browser = program, "Opened browser window");
        }
        opened
    })
}

fn open_in_default_browser(url: &str) {
    if let Err(e) = webbrowser::open(url) {
        tracing::warn!("Could not open a browser: {e}");
    }
}

// --- OS URL-scheme handler ---

/// Registers a temporary OS-level handler for `zendesk-support://` that forwards the
/// redirect to the local callback server. Cleans up on `cleanup()` or drop.
pub struct UrlSchemeHandler {
    callback_url: String,
    cleanup: Vec<Box<dyn FnOnce() + Send>>,
}

const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework\
/Frameworks/LaunchServices.framework/Support/lsregister";

fn run(command: &mut Command) -> Result<()> {
    let name = command.get_program().to_string_lossy().into_owned();
    let output = command
        .output()
        .with_context(|| format!("Could not run {name}"))?;
    if !output.status.success() {
        bail!(
            "{name} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Write a fresh file that only the current user can read and run.
fn write_private(path: &Path, contents: &str) -> Result<()> {
    let _ = std::fs::remove_file(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o700);
    }
    options
        .open(path)
        .and_then(|mut f| f.write_all(contents.as_bytes()))
        .with_context(|| format!("Could not write {}", path.display()))
}

fn home_dir() -> Result<PathBuf> {
    std::env::home_dir().ok_or_else(|| anyhow!("Could not determine the home directory"))
}

impl UrlSchemeHandler {
    pub fn new(port: u16) -> Self {
        Self {
            callback_url: callback_url_for(port, ""),
            cleanup: Vec::new(),
        }
    }

    /// Target the server's nonced callback URL; the default one (no nonce) answers 404.
    fn with_callback_url(mut self, callback_url: String) -> Self {
        self.callback_url = callback_url;
        self
    }

    /// Create and register the handler. Returns false (after cleaning up) on failure.
    pub fn register(&mut self) -> bool {
        let result = if cfg!(target_os = "macos") {
            self.register_macos()
        } else if cfg!(target_os = "linux") {
            self.register_linux()
        } else if cfg!(target_os = "windows") {
            self.register_windows()
        } else {
            return false;
        };
        match result {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("Could not register URL scheme handler: {e:#}");
                self.cleanup();
                false
            }
        }
    }

    /// Undo all registrations and remove temp files.
    pub fn cleanup(&mut self) {
        for action in self.cleanup.drain(..).rev() {
            action();
        }
    }

    fn register_macos(&mut self) -> Result<()> {
        tracing::info!("Registering macOS URL scheme handler...");
        // Must be in ~/Applications for Launch Services to find it.
        let apps_dir = home_dir()?.join("Applications");
        std::fs::create_dir_all(&apps_dir)?;
        let app_dir = apps_dir.join("ZendeskMCPAuth.app");
        let _ = std::fs::remove_dir_all(&app_dir);

        // `on open location` is how macOS delivers custom-scheme URLs to apps. The script
        // goes in a file to avoid shell-escaping trouble with `osacompile -e`.
        let script_file =
            std::env::temp_dir().join(format!("zendesk_auth_{}.applescript", std::process::id()));
        write_private(
            &script_file,
            &format!(
                r#"on open location theURL
    try
        set curlResult to do shell script "/usr/bin/curl -s -G --max-time 10 --data-urlencode url=" & quoted form of theURL & " '{}'"
        log "Zendesk auth callback sent successfully"
        return curlResult
    on error errMsg
        log "Zendesk auth callback failed: " & errMsg
        return "error: " & errMsg
    end try
end open location
"#,
                self.callback_url
            ),
        )?;
        let compiled = run(Command::new("osacompile")
            .arg("-o")
            .arg(&app_dir)
            .arg(&script_file));
        let _ = std::fs::remove_file(&script_file);
        compiled?;

        let cleanup_dir = app_dir.clone();
        self.cleanup.push(Box::new(move || {
            let _ = Command::new(LSREGISTER).arg("-u").arg(&cleanup_dir).output();
            // lsregister -u does not always clear URL schemes, so reset via CoreServices too.
            let _ = Command::new("swift")
                .args([
                    "-e",
                    "import Foundation; import CoreServices; \
                     LSSetDefaultHandlerForURLScheme(\"zendesk-support\" as CFString, \"\" as CFString)",
                ])
                .output();
            let _ = std::fs::remove_dir_all(&cleanup_dir);
        }));

        let plist = app_dir.join("Contents").join("Info.plist");
        let mut buddy = Command::new("/usr/libexec/PlistBuddy");
        for edit in [
            "Set :CFBundleIdentifier com.zendesk-mcp-server.auth".to_string(),
            "Add :CFBundleURLTypes array".to_string(),
            "Add :CFBundleURLTypes:0 dict".to_string(),
            "Add :CFBundleURLTypes:0:CFBundleURLName string Zendesk Support OAuth".to_string(),
            "Add :CFBundleURLTypes:0:CFBundleURLSchemes array".to_string(),
            format!("Add :CFBundleURLTypes:0:CFBundleURLSchemes:0 string {URL_SCHEME}"),
        ] {
            buddy.arg("-c").arg(edit);
        }
        run(buddy.arg(&plist))?;

        run(Command::new(LSREGISTER).arg("-f").arg(&app_dir))?;
        tracing::info!("Registered macOS URL scheme handler");
        Ok(())
    }

    fn register_linux(&mut self) -> Result<()> {
        let apps_dir = home_dir()?.join(".local/share/applications");
        std::fs::create_dir_all(&apps_dir)?;

        let script_path = std::env::temp_dir().join(format!(
            "zendesk-mcp-auth-handler-{}.sh",
            std::process::id()
        ));
        write_private(
            &script_path,
            &format!(
                "#!/bin/sh\n\
                 curl -s -G --data-urlencode \"url=$1\" \\\n    \
                 \"{}\" \\\n    \
                 >/dev/null 2>&1 &\n",
                self.callback_url
            ),
        )?;
        let cleanup_script = script_path.clone();
        self.cleanup.push(Box::new(move || {
            let _ = std::fs::remove_file(&cleanup_script);
        }));

        let desktop_path = apps_dir.join("zendesk-mcp-auth.desktop");
        std::fs::write(
            &desktop_path,
            format!(
                "[Desktop Entry]\nType=Application\nName=Zendesk MCP Auth\nExec={} %u\n\
                 NoDisplay=true\nMimeType=x-scheme-handler/{URL_SCHEME};\n",
                script_path.display()
            ),
        )?;
        // xdg-mime has no "unregister"; removing the files is enough.
        self.cleanup.push(Box::new(move || {
            let _ = std::fs::remove_file(&desktop_path);
        }));

        run(Command::new("xdg-mime").args([
            "default",
            "zendesk-mcp-auth.desktop",
            &format!("x-scheme-handler/{URL_SCHEME}"),
        ]))?;
        tracing::info!("Registered Linux URL scheme handler via xdg-mime");
        Ok(())
    }

    fn register_windows(&mut self) -> Result<()> {
        let script_path =
            std::env::temp_dir().join(format!("zendesk-mcp-auth-{}.ps1", std::process::id()));
        let (script, command) = windows_handler(&script_path, &self.callback_url);
        write_private(&script_path, &script)?;
        let key = format!("HKCU\\Software\\Classes\\{URL_SCHEME}");
        let cleanup_script = script_path.clone();
        let cleanup_key = key.clone();
        self.cleanup.push(Box::new(move || {
            let _ = Command::new("reg")
                .args(["delete", &cleanup_key, "/f"])
                .output();
            let _ = std::fs::remove_file(&cleanup_script);
        }));

        // HKCU needs no admin rights.
        run(Command::new("reg").args([
            "add",
            &key,
            "/ve",
            "/d",
            &format!("URL:{URL_SCHEME}"),
            "/f",
        ]))?;
        run(Command::new("reg").args(["add", &key, "/v", "URL Protocol", "/d", "", "/f"]))?;
        run(Command::new("reg").args([
            "add",
            &format!("{key}\\shell\\open\\command"),
            "/ve",
            "/d",
            &command,
            "/f",
        ]))?;
        tracing::info!("Registered Windows URL scheme handler via registry");
        Ok(())
    }
}

/// PowerShell script text and registry `open\command` value for the Windows handler. The
/// URL reaches PowerShell as a script argument, so cmd.exe never parses it.
fn windows_handler(script_path: &Path, callback_url: &str) -> (String, String) {
    let script = format!(
        "param([string]$u)\r\nInvoke-RestMethod -Uri (\"{callback_url}?url=\" + \
         [uri]::EscapeDataString($u)) | Out-Null\r\n"
    );
    let command = format!(
        "\"C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe\" -NoProfile \
         -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File \"{}\" \"%1\"",
        script_path.display()
    );
    (script, command)
}

impl Drop for UrlSchemeHandler {
    fn drop(&mut self) {
        self.cleanup();
    }
}

// --- Flows ---

fn login_priority(login: &Value) -> u8 {
    match login.get("service").and_then(Value::as_str) {
        Some("remote") => 0,
        Some("google") => 1,
        Some("office_365") => 2,
        Some("zendesk") => 3,
        _ => 99,
    }
}

/// The lookup response is untrusted: the URL goes to browser binaries as an argument and to
/// `window.open`, so only https is accepted (no `-flag`, `javascript:` or `file:`).
fn require_https_login_url(auth_url: &str) -> Result<String> {
    match url::Url::parse(auth_url) {
        Ok(parsed) if parsed.scheme() == "https" => Ok(auth_url.to_string()),
        _ => bail!("Zendesk returned an unexpected login URL"),
    }
}

const REJECTED_TOKEN: &str =
    "The sign-in returned a token Zendesk does not accept; nothing was saved.";

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
    tracing::info!("Starting browser authentication for {subdomain}.zendesk.com");
    let lookup = lookup_subdomain(http, subdomain).await?;
    let mut logins = lookup
        .get("agent_logins")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if logins.is_empty() {
        bail!("No login methods available for {subdomain}.zendesk.com");
    }
    logins.sort_by_key(login_priority);
    let login = &logins[0];
    let service = login.get("service").and_then(Value::as_str);
    tracing::info!(?service, "Selected auth method");

    let text = |key: &str| {
        login
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let auth_url = if service == Some("zendesk") {
        Some(match login.get("url").and_then(Value::as_str) {
            Some(url) => url.to_string(),
            None => format!("https://{subdomain}.zendesk.com/access/oauth_mobile"),
        })
    } else {
        text("zendesk_url")
            .or_else(|| text("url"))
            .map(String::from)
    };
    let auth_url = auth_url.ok_or_else(|| anyhow!("No authentication URL found."))?;
    let full_auth_url = build_auth_url(&require_https_login_url(&auth_url)?);

    let mut server = start_callback_server(subdomain, full_auth_url.clone(), false).await?;
    let mut scheme_handler =
        UrlSchemeHandler::new(server.port).with_callback_url(server.callback_url());
    let registered = scheme_handler.register();

    if registered {
        // The OS hands the zendesk-support:// redirect to the handler, so send the
        // browser straight to the Zendesk login.
        tracing::info!("Opening Zendesk login in a private window...");
        if !open_in_private_window(&full_auth_url) {
            tracing::warn!(
                "Could not open a private window in Chrome, Firefox or Edge; using the default \
                 browser. If SAML sign-in fails in Safari, install Chrome: brew install --cask google-chrome"
            );
            open_in_default_browser(&full_auth_url);
        }
    } else {
        let local_url = server.auth_url();
        tracing::warn!("Opening the manual sign-in page: {local_url}");
        if !open_in_private_window(&local_url) {
            open_in_default_browser(&local_url);
        }
    }

    tracing::info!(
        timeout_secs = timeout.as_secs(),
        "Waiting for authentication..."
    );
    let received = tokio::time::timeout(timeout, server.tokens.recv()).await;
    scheme_handler.cleanup();
    match received {
        Ok(Some(token)) => {
            tracing::info!("Browser authentication completed");
            Ok(token)
        }
        _ => bail!(
            "Authentication timed out. Run 'zendesk-mcp-server mobile-auth' manually to authenticate."
        ),
    }
}

/// Token to use at server startup: the saved one if it is for `subdomain` and still
/// verifies, else a fresh browser sign-in (saved before returning). `subdomain` may be
/// `None` only when the saved token supplies it.
pub async fn ensure_auth(http: &reqwest::Client, subdomain: Option<&str>) -> Result<MobileToken> {
    let saved = load_token();
    if let Some(token) = &saved {
        if subdomain.is_none_or(|s| s == token.subdomain) {
            if verify_token(http, &token.subdomain, &token.access_token).await {
                tracing::info!("Existing OAuth token is valid");
                return Ok(token.clone());
            }
            tracing::info!("Existing OAuth token is expired or invalid");
        } else {
            tracing::info!("Saved token is for a different subdomain");
        }
    }

    let subdomain = subdomain
        .map(str::to_string)
        .or_else(|| saved.map(|t| t.subdomain))
        .ok_or_else(|| {
            anyhow!(
                "ZENDESK_SUBDOMAIN is required. Set it in .env or run 'zendesk-mcp-server mobile-auth'."
            )
        })?;
    let token = auth_via_browser(http, &subdomain, BROWSER_TIMEOUT).await?;
    if !verify_token(http, &token.subdomain, &token.access_token).await {
        bail!(REJECTED_TOKEN);
    }
    save_token(&token)?;
    tracing::info!("OAuth token saved");
    Ok(token)
}

// --- Interactive CLI ---

fn prompt(message: &str) -> Result<String> {
    print!("{message}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

/// Subdomain for the interactive `mobile-auth` command: `env_subdomain` (trimmed) if set and
/// non-empty, printed since it is not a secret, else whatever `ask` returns. Either way, a
/// pasted `https://mycompany.zendesk.com/` is normalized down to `mycompany`.
fn resolve_subdomain(
    env_subdomain: Option<String>,
    ask: impl FnOnce() -> Result<String>,
) -> Result<String> {
    let env_subdomain = env_subdomain
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let subdomain = match env_subdomain {
        Some(subdomain) => {
            println!("Using subdomain '{subdomain}' from ZENDESK_SUBDOMAIN.");
            subdomain
        }
        None => ask()?,
    };
    Ok(subdomain
        .replace(".zendesk.com", "")
        .replace("https://", "")
        .replace("http://", "")
        .trim_matches('/')
        .to_string())
}

fn login_label(service: &str) -> &str {
    match service {
        "zendesk" => "Email & Password",
        "google" => "Google",
        "office_365" => "Office 365",
        "remote" => "SSO (Corporate)",
        other => other,
    }
}

/// Browser sign-in that also accepts the callback URL pasted into the terminal.
async fn auth_sso_browser_interactive(subdomain: &str, auth_url: &str) -> Result<MobileToken> {
    let full_auth_url = build_auth_url(&require_https_login_url(auth_url)?);
    let mut server = start_callback_server(subdomain, full_auth_url, true).await?;
    let local_url = server.auth_url();
    println!("\nOpening browser for Zendesk login...");
    println!("If the browser doesn't open, visit: {local_url}\n");
    open_in_default_browser(&local_url);
    println!("Waiting for authentication...");
    println!("Or paste the callback URL here and press Enter:");
    println!("(Press Ctrl+C to cancel)\n");

    // A plain thread, not tokio::io::stdin: a pending blocking read would otherwise keep the
    // process alive after the browser flow succeeds.
    let (line_tx, mut lines) = mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });

    let timeout = tokio::time::sleep(INTERACTIVE_TIMEOUT);
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            Some(token) = server.tokens.recv() => return Ok(token),
            line = lines.recv() => {
                let Some(line) = line else { bail!("Authentication timed out or was cancelled.") };
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match parse_oauth_callback(line, subdomain) {
                    Some(token) => return Ok(token),
                    None => eprintln!(
                        "Could not parse an access token from that URL. Paste the full URL starting with zendesk-support://"
                    ),
                }
            }
            _ = &mut timeout => bail!("Authentication timed out or was cancelled."),
            _ = tokio::signal::ctrl_c() => bail!("Cancelled."),
        }
    }
}

/// Interactive `zendesk-mcp-server mobile-auth` command.
pub async fn run_auth_cli(http: reqwest::Client) -> Result<()> {
    println!("=== Zendesk MCP Server - Authentication ===\n");

    if let Some(existing) = load_token()
        && verify_token(&http, &existing.subdomain, &existing.access_token).await
    {
        println!("✓ Valid authentication already exists!");
        println!("  User: {}", existing.username.as_deref().unwrap_or("N/A"));
        println!("  Subdomain: {}", existing.subdomain);
        println!("  Token file: {}", token_path().display());
        println!();
        let answer = prompt("Re-authenticate anyway? [y/N]: ")?.to_lowercase();
        if answer != "y" && answer != "yes" {
            println!("\nKeeping existing authentication.");
            return Ok(());
        }
        println!();
    }

    let subdomain = resolve_subdomain(std::env::var("ZENDESK_SUBDOMAIN").ok(), || {
        prompt("Enter your Zendesk subdomain (e.g., 'mycompany' for mycompany.zendesk.com): ")
    })?;
    if subdomain.is_empty() {
        bail!("subdomain is required.");
    }

    println!("\nLooking up authentication options for {subdomain}.zendesk.com...");
    let lookup = lookup_subdomain(&http, &subdomain).await?;
    let logins = lookup
        .get("agent_logins")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if logins.is_empty() {
        bail!("no login methods found for this subdomain.");
    }

    let account_name = lookup
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(&subdomain);
    println!("\nAvailable login methods for {account_name}:");
    for (i, login) in logins.iter().enumerate() {
        let service = login
            .get("service")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        println!("  [{}] {}", i + 1, login_label(service));
    }

    let choice = if logins.len() == 1 {
        0
    } else {
        let answer = prompt(&format!("\nSelect method [1-{}]: ", logins.len()))?;
        match answer.parse::<usize>() {
            Ok(n) if (1..=logins.len()).contains(&n) => n - 1,
            _ => bail!("Invalid selection."),
        }
    };
    let login = &logins[choice];

    let token = if login.get("service").and_then(Value::as_str) == Some("zendesk") {
        let email = prompt("Email: ")?;
        let password = rpassword::prompt_password("Password: ")?;
        println!("\nAuthenticating...");
        auth_email_password(&http, &subdomain, &email, &password).await?
    } else {
        let auth_url = ["zendesk_url", "url"]
            .iter()
            .find_map(|key| {
                login
                    .get(*key)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            })
            .ok_or_else(|| anyhow!("no authentication URL found."))?;
        let token = auth_sso_browser_interactive(&subdomain, auth_url).await?;
        if !verify_token(&http, &token.subdomain, &token.access_token).await {
            bail!(REJECTED_TOKEN);
        }
        token
    };

    let path = save_token(&token)?;
    println!("\nAuthentication successful!");
    println!("  User: {}", token.username.as_deref().unwrap_or("N/A"));
    println!("  Role: {}", token.user_role.as_deref().unwrap_or("N/A"));
    println!("  Token saved to: {}", path.display());
    println!("\nThe MCP server will use this token automatically on next start.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn token(access_token: &str) -> MobileToken {
        MobileToken {
            subdomain: "acme".into(),
            access_token: access_token.into(),
            username: Some("Ada".into()),
            user_id: None,
            account_id: None,
            user_role: None,
        }
    }

    fn callback_url(server: &CallbackServer, url: &str) -> String {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("url", url)
            .finish();
        format!("{}?{query}", server.callback_url())
    }

    #[test]
    fn debug_redacts_access_token() {
        let shown = format!("{:?}", token("SECRET"));
        assert!(!shown.contains("SECRET"));
        assert!(shown.contains("Ada"));
    }

    #[test]
    fn parse_callback_reads_query_params() {
        let t = parse_oauth_callback(
            "zendesk-support://authenticate?access_token=abc&username=Ada&user_id=7&user_role=agent",
            "acme",
        )
        .unwrap();
        assert_eq!(t.access_token, "abc");
        assert_eq!(t.subdomain, "acme");
        assert_eq!(t.username.as_deref(), Some("Ada"));
        assert_eq!(t.user_id.as_deref(), Some("7"));
        assert_eq!(t.user_role.as_deref(), Some("agent"));
    }

    #[test]
    fn parse_callback_falls_back_to_fragment() {
        let t = parse_oauth_callback(
            "zendesk-support://authenticate#access_token=xyz&username=Bo",
            "acme",
        )
        .unwrap();
        assert_eq!(t.access_token, "xyz");
        assert_eq!(t.username.as_deref(), Some("Bo"));
    }

    #[test]
    fn parse_callback_without_token_is_none() {
        assert!(
            parse_oauth_callback("zendesk-support://authenticate?username=Ada", "acme").is_none()
        );
        assert!(parse_oauth_callback("zendesk-support://authenticate", "acme").is_none());
        assert!(parse_oauth_callback("", "acme").is_none());
    }

    #[test]
    fn build_auth_url_adds_client_and_device_params() {
        let url = build_auth_url("https://acme.zendesk.com/access/oauth_mobile");
        assert!(url.starts_with(
            "https://acme.zendesk.com/access/oauth_mobile?client_id=zendesk_support_android"
        ));
        assert!(url.contains("device%5Bname%5D="));
        assert!(url.contains("device%5Bidentifier%5D="));
    }

    #[test]
    fn build_auth_url_extends_an_existing_query() {
        let url = build_auth_url("https://sso.example.com/login?x=1");
        assert!(url.starts_with("https://sso.example.com/login?x=1&client_id="));
        assert_eq!(url.matches('?').count(), 1);
    }

    #[test]
    fn resolve_subdomain_uses_env_without_asking() {
        let asked = std::cell::Cell::new(false);
        let subdomain = resolve_subdomain(Some("example".to_string()), || {
            asked.set(true);
            Ok(String::new())
        })
        .unwrap();
        assert_eq!(subdomain, "example");
        assert!(
            !asked.get(),
            "should not prompt when ZENDESK_SUBDOMAIN is set"
        );
    }

    #[test]
    fn resolve_subdomain_asks_when_env_is_unset_or_blank() {
        for env in [None, Some("   ".to_string())] {
            let subdomain =
                resolve_subdomain(env, || Ok("https://example.zendesk.com/".to_string())).unwrap();
            assert_eq!(subdomain, "example");
        }
    }

    #[test]
    fn resolve_subdomain_normalizes_env_value() {
        let subdomain = resolve_subdomain(
            Some("  https://example.zendesk.com/ ".to_string()),
            || unreachable!(),
        )
        .unwrap();
        assert_eq!(subdomain, "example");
    }

    #[test]
    fn token_path_uses_override_else_default() {
        let custom = token_path_from(|k| {
            (k == "ZENDESK_MOBILE_TOKEN_FILE").then(|| "/tmp/t.json".to_string())
        });
        assert_eq!(custom, PathBuf::from("/tmp/t.json"));
        assert_eq!(
            token_path_from(|_| None),
            crate::config::default_mobile_token_file()
        );
        assert_eq!(
            token_path_from(|_| Some("  ".into())),
            crate::config::default_mobile_token_file()
        );
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nested").join("token.json");
        save_token_to(&token("abc"), &file).unwrap();
        assert_eq!(load_token_from(&file), Some(token("abc")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&file), 0o600);
            assert_eq!(mode(file.parent().unwrap()), 0o700);
        }
    }

    #[test]
    fn load_token_rejects_incomplete_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("token.json");
        assert_eq!(load_token_from(&file), None);
        std::fs::write(&file, r#"{"subdomain": "acme"}"#).unwrap();
        assert_eq!(load_token_from(&file), None);
        std::fs::write(&file, r#"{"subdomain": "acme", "access_token": ""}"#).unwrap();
        assert_eq!(load_token_from(&file), None);
        std::fs::write(&file, "not json").unwrap();
        assert_eq!(load_token_from(&file), None);
        // The Python version's file format loads as-is.
        std::fs::write(
            &file,
            r#"{"subdomain": "acme", "access_token": "t", "username": "Ada"}"#,
        )
        .unwrap();
        assert_eq!(load_token_from(&file).unwrap().access_token, "t");
    }

    #[tokio::test]
    async fn verify_token_is_true_only_on_200() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/me.json"))
            .and(header("authorization", "Bearer good"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/me.json"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        assert!(verify_token_at(&http, &server.uri(), "good").await);
        assert!(!verify_token_at(&http, &server.uri(), "bad").await);
    }

    #[tokio::test]
    async fn lookup_returns_the_lookup_object() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/mobile/account/lookup.json"))
            .and(header("user-agent", USER_AGENT))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "lookup": {"name": "Acme", "agent_logins": [{"service": "zendesk"}]}
            })))
            .mount(&server)
            .await;
        let lookup = lookup_at(&reqwest::Client::new(), &server.uri(), "acme")
            .await
            .unwrap();
        assert_eq!(lookup["name"], "Acme");
    }

    #[tokio::test]
    async fn lookup_maps_error_statuses() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/mobile/account/lookup.json"))
            .respond_with(ResponseTemplate::new(404))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/mobile/account/lookup.json"))
            .respond_with(ResponseTemplate::new(403))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/mobile/account/lookup.json"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        let message = async |http: &reqwest::Client| {
            lookup_at(http, &server.uri(), "acme")
                .await
                .unwrap_err()
                .to_string()
        };
        assert_eq!(message(&http).await, "subdomain 'acme' not found");
        assert_eq!(
            message(&http).await,
            "access forbidden (IP restriction or mobile access disabled)"
        );
        assert_eq!(message(&http).await, "HTTP 500 Internal Server Error");
    }

    #[tokio::test]
    async fn email_password_sends_payload_and_reads_camel_case() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/access/oauth_mobile"))
            .and(header("user-agent", USER_AGENT))
            .and(body_partial_json(json!({
                "clientId": "zendesk_support_android",
                "user": {"email": "a@b.c", "password": "pw"},
                "nativeMobile": true,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "authentication": {
                    "accessToken": "tok", "username": "Ada", "userId": 42,
                    "accountId": "acc", "userRole": "agent"
                }
            })))
            .mount(&server)
            .await;
        let t = auth_email_password_at(
            &reqwest::Client::new(),
            &server.uri(),
            "acme",
            "a@b.c",
            "pw",
        )
        .await
        .unwrap();
        assert_eq!(t.access_token, "tok");
        assert_eq!(t.user_id.as_deref(), Some("42"));
        assert_eq!(t.account_id.as_deref(), Some("acc"));
        assert_eq!(t.user_role.as_deref(), Some("agent"));
        assert_eq!(t.subdomain, "acme");
    }

    #[tokio::test]
    async fn email_password_failure_reports_status_and_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_string("bad credentials"))
            .mount(&server)
            .await;
        let err = auth_email_password_at(&reqwest::Client::new(), &server.uri(), "acme", "a", "b")
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("401") && err.contains("bad credentials"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn callback_server_receives_token_and_serves_success_page() {
        let mut server = start_callback_server("acme", "https://x/login".into(), false)
            .await
            .unwrap();
        let resp = reqwest::get(callback_url(
            &server,
            "zendesk-support://authenticate?access_token=abc",
        ))
        .await
        .unwrap();
        assert!(
            resp.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
        assert!(
            resp.text()
                .await
                .unwrap()
                .contains("Authentication successful")
        );
        let received = server.tokens.recv().await.unwrap();
        assert_eq!(received.access_token, "abc");
        assert_eq!(received.subdomain, "acme");
    }

    #[tokio::test]
    async fn interactive_callback_answers_json() {
        let mut server = start_callback_server("acme", "https://x/login".into(), true)
            .await
            .unwrap();
        let body: Value = reqwest::get(callback_url(
            &server,
            "zendesk-support://authenticate?access_token=abc&username=Ada",
        ))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        assert_eq!(body, json!({"ok": true, "username": "Ada"}));
        assert!(server.tokens.recv().await.is_some());
    }

    #[tokio::test]
    async fn auth_page_contains_the_json_encoded_login_url() {
        let server = start_callback_server("acme", "https://x/login?a=1&b=\"2\"".into(), false)
            .await
            .unwrap();
        let page = reqwest::get(server.auth_url())
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(page.contains(r#"window.open("https://x/login?a=1&b=\"2\"", '_blank')"#));
        assert!(!page.contains("__AUTH_URL__"));
    }

    #[tokio::test]
    async fn bad_callback_reports_failure() {
        let server = start_callback_server("acme", "https://x/login".into(), false)
            .await
            .unwrap();
        let body: Value = reqwest::get(callback_url(
            &server,
            "zendesk-support://authenticate?foo=1",
        ))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        assert_eq!(
            body,
            json!({"ok": false, "error": "Could not parse access token from URL."})
        );
    }

    #[tokio::test]
    async fn callback_and_auth_routes_require_the_nonce() {
        let server = start_callback_server("acme", "https://x/login".into(), false)
            .await
            .unwrap();
        let base = format!("http://127.0.0.1:{}", server.port);
        let token_url = "zendesk-support://authenticate?access_token=abc";
        let bad_paths = [
            format!("/callback?url={token_url}"),
            format!("/callback/wrong?url={token_url}"),
            "/auth".to_string(),
            "/auth/wrong".to_string(),
            "/".to_string(),
        ];
        for bad in bad_paths {
            let resp = reqwest::get(format!("{base}{bad}")).await.unwrap();
            assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND, "{bad}");
            assert_eq!(resp.text().await.unwrap(), "", "{bad}");
        }
        assert!(server.tokens.is_empty());
        let ok = reqwest::get(server.auth_url()).await.unwrap();
        assert_eq!(ok.status(), reqwest::StatusCode::OK);
        assert_eq!(server.nonce.len(), 43);
    }

    #[tokio::test]
    async fn pages_embed_the_nonce_in_their_javascript() {
        let mut server = start_callback_server("acme", "https://x/login".into(), false)
            .await
            .unwrap();
        let expected = format!("fetch('/callback/{}?url=", server.nonce);
        let page = reqwest::get(server.auth_url())
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(page.contains(&expected));
        assert!(!page.contains("__NONCE__"));
        let success = reqwest::get(callback_url(
            &server,
            "zendesk-support://authenticate?access_token=abc",
        ))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
        assert!(success.contains(&expected));
        assert!(!success.contains("__NONCE__"));
        assert!(server.tokens.recv().await.is_some());
    }

    #[test]
    fn save_token_leaves_no_temp_file_and_sets_0600() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("token.json");
        // An existing loose file is replaced, not reused.
        std::fs::write(&file, "old").unwrap();
        save_token_to(&token("abc"), &file).unwrap();
        save_token_to(&token("def"), &file).unwrap();
        assert_eq!(load_token_from(&file), Some(token("def")));
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("token.json")]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn login_url_must_be_https() {
        for bad in [
            "-foo",
            "javascript:alert(1)",
            "http://x",
            "file:///etc/passwd",
            "",
        ] {
            let err = require_https_login_url(bad).unwrap_err().to_string();
            assert_eq!(err, "Zendesk returned an unexpected login URL", "{bad}");
        }
        assert_eq!(
            require_https_login_url("https://acme.zendesk.com/sso").unwrap(),
            "https://acme.zendesk.com/sso"
        );
    }

    #[test]
    fn windows_handler_uses_powershell_without_cmd() {
        let callback = callback_url_for(4242, "NONCE");
        let (script, command) =
            windows_handler(Path::new(r"C:\Temp\zendesk-mcp-auth-1.ps1"), &callback);
        assert_eq!(
            script,
            "param([string]$u)\r\nInvoke-RestMethod -Uri (\"http://127.0.0.1:4242/callback/NONCE?url=\" + [uri]::EscapeDataString($u)) | Out-Null\r\n"
        );
        assert_eq!(
            command,
            "\"C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe\" -NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File \"C:\\Temp\\zendesk-mcp-auth-1.ps1\" \"%1\""
        );
        assert!(!command.contains("cmd") && !command.contains(".bat"));
    }
}
