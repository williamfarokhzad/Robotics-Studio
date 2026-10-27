//! Private cloud MCP gateway. Each OAuth grant owns isolated, expiring CLI processes.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
mod account;
mod pages;

use axum::{
    Form, Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

const TTL: Duration = Duration::from_secs(1800);
const BODY_LIMIT: usize = 1024 * 1024;
const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
const FILE_LIMIT: u64 = 64 * 1024 * 1024;
const MAX_RECORDS: usize = 128;
type App = Arc<Shared>;
type Failure = (StatusCode, String);
struct Shared {
    base: String,
    cli: PathBuf,
    password: Option<[u8; 32]>,
    client_key: [u8; 32],
    inner: Mutex<Store>,
    // Single CAD operation at a time on the free instance. HTTP/auth stay responsive.
    compute: Mutex<()>,
}
#[derive(Default)]
struct Store {
    pending: HashMap<String, (Authorize, Instant)>,
    codes: HashMap<String, Code>,
    tokens: HashMap<String, Token>,
    refresh: HashMap<String, Token>,
    sessions: HashMap<String, Arc<Mutex<Worker>>>,
    downloads: HashMap<String, Download>,
    login_attempts: VecDeque<Instant>,
}
#[derive(Clone)]
struct Token {
    principal: String,
    client: String,
    expires: Instant,
}
struct Code {
    request: Authorize,
    expires: Instant,
    principal: String,
}
#[derive(Clone, Deserialize)]
struct Authorize {
    client_id: String,
    redirect_uri: String,
    response_type: String,
    code_challenge: String,
    code_challenge_method: String,
    resource: String,
    state: String,
    #[serde(default)]
    scope: String,
}
#[derive(Deserialize)]
struct Login {
    ticket: String,
    password: String,
}
struct Download {
    session: String,
    filename: String,
    expires: Instant,
}
struct Worker {
    principal: String,
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    directory: tempfile::TempDir,
    touched: Instant,
}
fn random() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}
fn hash(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}
fn fail(code: StatusCode, message: &str) -> Failure {
    (code, message.into())
}
fn bad(message: &str) -> Failure {
    fail(StatusCode::BAD_REQUEST, message)
}
fn busy() -> Failure {
    fail(StatusCode::TOO_MANY_REQUESTS, "Capacity reached. Retry later.")
}
fn field<'a>(h: &'a HeaderMap, key: &str) -> Option<&'a str> {
    h.get(key).and_then(|v| v.to_str().ok())
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}
fn json_response(value: Value) -> Response {
    Json(value).into_response()
}
fn resource(app: &Shared) -> String {
    format!("{}/mcp", app.base)
}
fn unauthorized(app: &Shared) -> Response {
    let mut r = (StatusCode::UNAUTHORIZED, "Sign in to Robotics Studio").into_response();
    let challenge = format!("Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\", scope=\"cad\"", app.base);
    if let Ok(v) = challenge.parse() {
        r.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    r
}
async fn identity(app: &Shared, h: &HeaderMap) -> Option<String> {
    let bearer = field(h, "authorization")?.strip_prefix("Bearer ")?;
    let db = app.inner.lock().await;
    db.tokens.get(&hash_key(bearer)).filter(|t| t.expires > Instant::now()).map(|t| t.principal.clone())
}
fn hash_key(s: &str) -> String {
    URL_SAFE_NO_PAD.encode(hash(s))
}
fn allowed_origin(app: &Shared, h: &HeaderMap) -> bool {
    match field(h, "origin") {
        None => true,
        Some(o) => o == app.base || o == "https://chatgpt.com",
    }
}
fn redirect_allowed(s: &str) -> bool {
    let Ok(u) = url::Url::parse(s) else {
        return false;
    };
    u.scheme() == "https"
        && u.host_str() == Some("chatgpt.com")
        && u.username().is_empty()
        && u.password().is_none()
        && u.fragment().is_none()
        && (u.path() == "/connector_platform_oauth_redirect" || u.path().starts_with("/connector/oauth/"))
}
async fn protected(State(app): State<App>) -> Json<Value> {
    Json(json!({"resource":resource(&app),"authorization_servers":[app.base],"scopes_supported":["cad"],"bearer_methods_supported":["header"]}))
}
async fn metadata(State(app): State<App>) -> Json<Value> {
    Json(
        json!({"issuer":app.base,"authorization_endpoint":format!("{}/oauth/authorize",app.base),"token_endpoint":format!("{}/oauth/token",app.base),"registration_endpoint":format!("{}/oauth/register",app.base),"response_types_supported":["code"],"grant_types_supported":["authorization_code","refresh_token"],"token_endpoint_auth_methods_supported":["none"],"code_challenge_methods_supported":["S256"],"scopes_supported":["cad"],"authorization_response_iss_parameter_supported":true}),
    )
}
fn signed_client(app: &Shared, uris: &[String]) -> Result<String, Failure> {
    let body = URL_SAFE_NO_PAD.encode(json!({"issuer":app.base,"redirect_uris":uris,"nonce":random()}).to_string());
    let mut mac = Hmac::<Sha256>::new_from_slice(&app.client_key).map_err(|_| bad("Invalid registration key"))?;
    mac.update(body.as_bytes());
    Ok(format!("{body}.{}", URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())))
}
fn client_uris(app: &Shared, id: &str) -> Option<Vec<String>> {
    if id.len() > 4096 {
        return None;
    }
    let (body, signature) = id.split_once('.')?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&app.client_key).ok()?;
    mac.update(body.as_bytes());
    mac.verify_slice(&URL_SAFE_NO_PAD.decode(signature).ok()?).ok()?;
    let v: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(body).ok()?).ok()?;
    if v.get("issuer")?.as_str()? != app.base {
        return None;
    }
    v.get("redirect_uris")?.as_array()?.iter().map(|v| v.as_str().map(str::to_string)).collect()
}
async fn register(State(app): State<App>, Json(v): Json<Value>) -> Result<Response, Failure> {
    if v.get("token_endpoint_auth_method").and_then(Value::as_str).is_some_and(|s| s != "none") {
        return Err(bad("Only public PKCE clients are supported"));
    }
    let uris: Vec<String> = v
        .get("redirect_uris")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("redirect_uris required"))?
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect();
    if uris.is_empty()
        || uris.len() > 8
        || uris.iter().any(|s| s.len() > 512 || !redirect_allowed(s))
        || uris.iter().map(String::len).sum::<usize>() > 1800
    {
        return Err(bad("Only ChatGPT OAuth callbacks are allowed"));
    }
    if app.password.is_none() {
        return Err(fail(StatusCode::SERVICE_UNAVAILABLE, "Owner login not configured"));
    }
    let id = signed_client(&app, &uris)?;
    Ok((StatusCode::CREATED,Json(json!({"client_id":id,"redirect_uris":uris,"token_endpoint_auth_method":"none","grant_types":["authorization_code","refresh_token"],"response_types":["code"]}))).into_response())
}
async fn authorize(State(app): State<App>, Query(a): Query<Authorize>) -> Result<Response, Failure> {
    if app.password.is_none() {
        return Err(fail(StatusCode::SERVICE_UNAVAILABLE, "Owner must set STUDIO_ADMIN_PASSWORD in Render first."));
    }
    if a.response_type != "code"
        || a.code_challenge_method != "S256"
        || a.code_challenge.len() != 43
        || !a.code_challenge.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        || a.resource != resource(&app)
        || a.state.len() > 2048
        || (!a.scope.is_empty() && a.scope != "cad")
    {
        return Err(bad("Invalid OAuth request"));
    }
    let mut db = app.inner.lock().await;
    if !client_uris(&app, &a.client_id).is_some_and(|u| u.contains(&a.redirect_uri)) {
        return Err(bad("Unknown client or callback"));
    }
    db.pending.retain(|_, (_, expiry)| *expiry > Instant::now());
    if db.pending.len() >= MAX_RECORDS {
        return Err(busy());
    }
    let ticket = random();
    let client = escape(&a.client_id);
    db.pending.insert(ticket.clone(), (a, Instant::now() + Duration::from_secs(300)));
    let html = pages::consent(&ticket, &client);
    Ok(Html(html).into_response())
}
async fn login(state: State<App>, headers: HeaderMap, form: Form<Login>) -> Response {
    match login_inner(state, headers, form).await {
        Ok(response) => response,
        Err((status, message)) => (status, Html(pages::shell("Connection needs attention", &format!("<span class=\"eyebrow\">ROBOTICS STUDIO</span><h1>Let’s reconnect.</h1><p role=\"alert\">{}</p><a class=\"primary\" href=\"https://chatgpt.com/plugins\">Return to ChatGPT</a>", escape(&message))))).into_response(),
    }
}
async fn login_inner(State(app): State<App>, h: HeaderMap, Form(f): Form<Login>) -> Result<Response, Failure> {
    if !allowed_origin(&app, &h) {
        return Err(fail(StatusCode::FORBIDDEN, "Origin rejected"));
    }
    let mut db = app.inner.lock().await;
    let now = Instant::now();
    db.login_attempts.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
    if db.login_attempts.len() >= 10 {
        return Err(fail(StatusCode::TOO_MANY_REQUESTS, "Too many sign-in attempts. Wait one minute, then start Connect again in ChatGPT."));
    }
    db.login_attempts.push_back(now);
    let (a, expiry) = db
        .pending
        .get(&f.ticket)
        .cloned()
        .ok_or_else(|| bad("This connection form expired or the server restarted. Start Connect again in ChatGPT."))?;
    if expiry <= now {
        db.pending.remove(&f.ticket);
        return Err(bad("This connection form expired. Start Connect again in ChatGPT."));
    }
    let digest = hash(&f.password);
    if !app.password.is_some_and(|p| bool::from(p.ct_eq(&digest))) {
        let html = pages::consent_error(
            &f.ticket,
            &escape(&a.client_id),
            "The Studio owner password did not match. Try again below. This is the STUDIO_ADMIN_PASSWORD value in Render, not your Google or ChatGPT password.",
        );
        return Ok((StatusCode::UNAUTHORIZED, Html(html)).into_response());
    }
    if db.codes.len() >= MAX_RECORDS {
        return Err(busy());
    }
    let mut redirect = url::Url::parse(&a.redirect_uri).map_err(|_| bad("Invalid callback"))?;
    db.pending.remove(&f.ticket);
    let code = random();
    redirect.query_pairs_mut().append_pair("code", &code).append_pair("state", &a.state).append_pair("iss", &app.base);
    db.codes.insert(hash_key(&code), Code { request: a, expires: now + Duration::from_secs(60), principal: random() });
    Ok(Redirect::to(redirect.as_str()).into_response())
}
async fn token(state: State<App>, form: Form<HashMap<String, String>>) -> Response {
    match token_inner(state, form).await {
        Ok(r) => r,
        Err((status, message)) => (status, Json(json!({"error": message}))).into_response(),
    }
}
async fn token_inner(State(app): State<App>, Form(f): Form<HashMap<String, String>>) -> Result<Response, Failure> {
    let get = |k: &str| f.get(k).map(String::as_str).unwrap_or("");
    if get("resource") != resource(&app) {
        return Err(bad("invalid_target"));
    }
    let mut db = app.inner.lock().await;
    let now = Instant::now();
    db.tokens.retain(|_, t| t.expires > now);
    db.refresh.retain(|_, t| t.expires > now);
    db.codes.retain(|_, c| c.expires > now);
    let grant = match get("grant_type") {
        "authorization_code" => {
            let c = db.codes.remove(&hash_key(get("code"))).ok_or_else(|| bad("invalid_grant"))?;
            let verifier = get("code_verifier");
            if c.expires <= now
                || c.request.client_id != get("client_id")
                || c.request.redirect_uri != get("redirect_uri")
                || !(43..=128).contains(&verifier.len())
                || URL_SAFE_NO_PAD.encode(hash(verifier)) != c.request.code_challenge
            {
                return Err(bad("invalid_grant"));
            }
            Token { principal: c.principal, client: c.request.client_id, expires: now + TTL }
        }
        "refresh_token" => {
            let key = hash_key(get("refresh_token"));
            let t = db.refresh.get(&key).filter(|t| t.expires > now && t.client == get("client_id")).cloned().ok_or_else(|| bad("invalid_grant"))?;
            db.refresh.remove(&key);
            t
        }
        _ => return Err(bad("unsupported_grant_type")),
    };
    if db.tokens.len() >= MAX_RECORDS || db.refresh.len() >= MAX_RECORDS {
        return Err(busy());
    }
    let access = random();
    let refresh = random();
    db.tokens.insert(hash_key(&access), Token { expires: now + TTL, ..grant.clone() });
    db.refresh.insert(hash_key(&refresh), Token { expires: now + Duration::from_secs(86400), ..grant });
    Ok(json_response(json!({"access_token":access,"token_type":"Bearer","expires_in":1800,"refresh_token":refresh,"scope":"cad"})))
}
impl Worker {
    async fn start(app: &Shared, principal: String) -> Result<Self, Failure> {
        let directory = tempfile::Builder::new()
            .prefix("solidcraft-cloud-")
            .tempdir()
            .map_err(|_| fail(StatusCode::INTERNAL_SERVER_ERROR, "Could not create workspace"))?;
        let root = directory.path();
        let home = root.join("home");
        let tmp = root.join("tmp");
        tokio::fs::create_dir_all(&home).await.map_err(|_| bad("Workspace unavailable"))?;
        tokio::fs::create_dir_all(&tmp).await.map_err(|_| bad("Workspace unavailable"))?;
        let mut child = Command::new(&app.cli)
            .args(["mcp", "--allow-path"])
            .arg(root)
            .current_dir(root)
            .env_clear()
            .env("HOME", &home)
            .env("XDG_DATA_HOME", home.join("data"))
            .env("TMPDIR", &tmp)
            .env("PATH", "/usr/bin:/bin")
            .env("RAYON_NUM_THREADS", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| fail(StatusCode::SERVICE_UNAVAILABLE, "CAD worker could not start"))?;
        let input = child.stdin.take().ok_or_else(|| bad("Worker stdin unavailable"))?;
        let output = BufReader::new(child.stdout.take().ok_or_else(|| bad("Worker stdout unavailable"))?);
        Ok(Self { principal, child, input, output, directory, touched: Instant::now() })
    }
    async fn exchange(&mut self, msg: &Value) -> Result<Option<Value>, Failure> {
        self.touched = Instant::now();
        let mut line = serde_json::to_vec(msg).map_err(|_| bad("Invalid JSON"))?;
        line.push(b'\n');
        self.input.write_all(&line).await.map_err(|_| fail(StatusCode::GONE, "CAD worker stopped"))?;
        self.input.flush().await.map_err(|_| fail(StatusCode::GONE, "CAD worker stopped"))?;
        if msg.get("id").is_none() {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        loop {
            let chunk = self.output.fill_buf().await.map_err(|_| bad("Worker read failed"))?;
            if chunk.is_empty() {
                return Err(fail(StatusCode::GONE, "CAD worker stopped"));
            }
            let n = chunk.iter().position(|b| *b == b'\n').map(|p| p + 1).unwrap_or(chunk.len());
            if bytes.len() + n > OUTPUT_LIMIT {
                return Err(fail(StatusCode::PAYLOAD_TOO_LARGE, "CAD response too large"));
            }
            bytes.extend_from_slice(chunk.get(..n).ok_or_else(|| bad("Worker framing error"))?);
            self.output.consume(n);
            if bytes.last() == Some(&b'\n') {
                break;
            }
        }
        serde_json::from_slice(&bytes).map(Some).map_err(|_| bad("Invalid worker response"))
    }
}
fn filename(s: &str) -> bool {
    !s.is_empty() && s.len() <= 120 && !s.starts_with('.') && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}
async fn file_path(w: &Worker, name: &str) -> Result<PathBuf, Failure> {
    if !filename(name) {
        return Err(bad("Use a filename only, such as part.step"));
    }
    let path = tokio::fs::canonicalize(w.directory.path().join(name)).await.map_err(|_| fail(StatusCode::NOT_FOUND, "File not found"))?;
    if !path.starts_with(w.directory.path()) {
        return Err(fail(StatusCode::FORBIDDEN, "File outside workspace"));
    }
    let m = tokio::fs::metadata(&path).await.map_err(|_| bad("Cannot read file"))?;
    if !m.is_file() || m.len() > FILE_LIMIT {
        return Err(bad("File exceeds download limit"));
    }
    Ok(path)
}
fn download_tool() -> Value {
    json!({"name":"download_file","description":"Get a temporary download link for an exported or saved file in this CAD session. Use only the filename, e.g. part.step. Download work before the session expires or server restarts.","inputSchema":{"type":"object","properties":{"filename":{"type":"string"}},"required":["filename"],"additionalProperties":false},"annotations":{"readOnlyHint":true,"destructiveHint":false,"openWorldHint":false,"idempotentHint":false}})
}
async fn mcp(State(app): State<App>, h: HeaderMap, Json(msg): Json<Value>) -> Response {
    match mcp_inner(&app, &h, msg).await {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}
async fn mcp_inner(app: &App, h: &HeaderMap, msg: Value) -> Result<Response, Failure> {
    if !allowed_origin(app, h) {
        return Err(fail(StatusCode::FORBIDDEN, "Origin rejected"));
    }
    let Some(principal) = identity(app, h).await else {
        return Ok(unauthorized(app));
    };
    if msg.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || !msg.is_object() || msg.get("method").and_then(Value::as_str).is_none() {
        return Err(bad("A single JSON-RPC 2.0 message is required"));
    }
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    if method != "initialize" && field(h, "mcp-protocol-version").is_some_and(|v| !["2025-06-18", "2025-03-26", "2024-11-05"].contains(&v)) {
        return Err(bad("Unsupported MCP protocol version"));
    }
    if method == "tools/call" && msg.pointer("/params/name").and_then(Value::as_str) == Some("crash_reports") {
        return Err(fail(StatusCode::FORBIDDEN, "Local diagnostics are unavailable in cloud sessions"));
    }
    if !field(h, "accept").is_some_and(|v| v.contains("application/json") && v.contains("text/event-stream")) {
        return Err(fail(StatusCode::NOT_ACCEPTABLE, "Accept must include application/json and text/event-stream"));
    }
    let _compute = app.compute.try_lock().map_err(|_| busy())?;
    let (id, worker) = if method == "initialize" {
        if msg.get("id").is_none() {
            return Err(bad("Initialization requires an id"));
        }
        let mut db = app.inner.lock().await;
        if db.sessions.len() >= 2 {
            return Err(busy());
        }
        let id = random();
        let w = Arc::new(Mutex::new(Worker::start(app, principal.clone()).await?));
        db.sessions.insert(id.clone(), w.clone());
        (id, w)
    } else {
        let id = field(h, "mcp-session-id").ok_or_else(|| bad("MCP-Session-Id required"))?.to_string();
        let w = app
            .inner
            .lock()
            .await
            .sessions
            .get(&id)
            .cloned()
            .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Session expired; initialize a new session"))?;
        (id, w)
    };
    let mut w = worker.lock().await;
    if w.principal != principal {
        return Err(fail(StatusCode::NOT_FOUND, "Session not found"));
    }
    if w.touched.elapsed() > TTL {
        return Err(fail(StatusCode::NOT_FOUND, "Session expired"));
    }
    let download = method == "tools/call" && msg.pointer("/params/name").and_then(Value::as_str) == Some("download_file");
    let result = if download {
        let name = msg.pointer("/params/arguments/filename").and_then(Value::as_str).unwrap_or("");
        if app.inner.lock().await.downloads.len() >= MAX_RECORDS {
            return Err(busy());
        }
        file_path(&w, name).await?;
        w.touched = Instant::now();
        let cap = random();
        app.inner
            .lock()
            .await
            .downloads
            .insert(hash_key(&cap), Download { session: id.clone(), filename: name.into(), expires: Instant::now() + TTL });
        Ok(Some(
            json!({"jsonrpc":"2.0","id":msg.get("id"),"result":{"content":[{"type":"text","text":format!("Download {}: {}/files/{} (expires in 30 minutes or when the session ends)",name,app.base,cap)}],"isError":false}}),
        ))
    } else {
        match tokio::time::timeout(Duration::from_secs(90), w.exchange(&msg)).await {
            Ok(r) => r,
            Err(_) => {
                let _ = w.child.kill().await;
                Err(fail(StatusCode::GATEWAY_TIMEOUT, "CAD operation timed out; this session was ended. Initialize a new session."))
            }
        }
    };
    let mut reply = match result {
        Ok(v) => v,
        Err(e) => {
            drop(w);
            app.inner.lock().await.sessions.remove(&id);
            return Err(e);
        }
    };
    if method == "tools/list"
        && let Some(Value::Array(t)) = reply.as_mut().and_then(|r| r.pointer_mut("/result/tools"))
    {
        t.retain(|t| t.get("name").and_then(Value::as_str) != Some("crash_reports"));
        t.push(download_tool());
    }
    // Private cloud does not expose local diagnostics to clients.
    if method == "initialize"
        && let Some(r) = reply.as_mut().and_then(|r| r.get_mut("result"))
    {
        r["instructions"] = json!(format!(
            "{} Cloud session is temporary and private. Export/save with simple filenames, then call download_file to get a downloadable URL. Download work before 30 minutes idle or a restart. This does not control the browser CAD tab.",
            r.get("instructions").and_then(Value::as_str).unwrap_or("")
        ));
    }
    let mut response = reply.map(json_response).unwrap_or_else(|| StatusCode::ACCEPTED.into_response());
    if let Ok(v) = id.parse() {
        response.headers_mut().insert("mcp-session-id", v);
    }
    Ok(response)
}
async fn mcp_get(State(app): State<App>, h: HeaderMap) -> Response {
    if !allowed_origin(&app, &h) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if identity(&app, &h).await.is_none() {
        return unauthorized(&app);
    }
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}
async fn mcp_delete(State(app): State<App>, h: HeaderMap) -> Response {
    let Some(principal) = identity(&app, &h).await else {
        return unauthorized(&app);
    };
    if !allowed_origin(&app, &h) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(id) = field(&h, "mcp-session-id") else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let worker = app.inner.lock().await.sessions.get(id).cloned();
    if let Some(worker) = worker {
        let w = worker.lock().await;
        if w.principal == principal {
            drop(w);
            app.inner.lock().await.sessions.remove(id);
            return StatusCode::NO_CONTENT.into_response();
        }
    }
    StatusCode::NOT_FOUND.into_response()
}
async fn file(State(app): State<App>, Path(cap): Path<String>) -> Result<Response, Failure> {
    let (worker, name) = {
        let db = app.inner.lock().await;
        let d = db
            .downloads
            .get(&hash_key(&cap))
            .filter(|d| d.expires > Instant::now())
            .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Download expired"))?;
        (db.sessions.get(&d.session).cloned().ok_or_else(|| fail(StatusCode::NOT_FOUND, "Session expired"))?, d.filename.clone())
    };
    let w = worker.lock().await;
    if w.touched.elapsed() > TTL {
        return Err(fail(StatusCode::NOT_FOUND, "Session expired"));
    }
    let path = file_path(&w, &name).await?;
    let file = tokio::fs::File::open(path).await.map_err(|_| bad("Download failed"))?;
    let length = file.metadata().await.map_err(|_| bad("Download failed"))?.len();
    if length > FILE_LIMIT {
        return Err(bad("Download too large"));
    }
    let stream = tokio_util::io::ReaderStream::new(file.take(length));
    let mut r = axum::body::Body::from_stream(stream).into_response();
    if let Ok(v) = length.to_string().parse() {
        r.headers_mut().insert(header::CONTENT_LENGTH, v);
    }
    r.headers_mut().insert(header::CONTENT_TYPE, header::HeaderValue::from_static("application/octet-stream"));
    if let Ok(v) = format!("attachment; filename=\"{name}\"").parse() {
        r.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(r)
}
#[cfg(test)]
fn router(app: App) -> Router {
    match account::Account::new() {
        Ok(account) => router_with_account(app, account),
        Err(_) => Router::new().route("/health", get(|| async { StatusCode::SERVICE_UNAVAILABLE })),
    }
}
fn router_with_account(app: App, account: account::Account) -> Router {
    Router::new().route("/", get(|State(app): State<App>| async move { Html(pages::connect(&app.base)) }))
    .route("/health",get(||async{Json(json!({"status":"ok"}))}))
    .route("/.well-known/oauth-protected-resource",get(protected)).route("/.well-known/oauth-protected-resource/mcp",get(protected))
    .route("/.well-known/oauth-authorization-server",get(metadata))
    .route("/oauth/register",post(register)).route("/oauth/authorize",get(authorize).post(login)).route("/oauth/token",post(token))
    .route("/mcp",post(mcp).get(mcp_get).delete(mcp_delete)).route("/files/{cap}",get(file))
    .with_state(app)
    .merge(account::routes(account))
    .merge(pages::routes())
    .layer(DefaultBodyLimit::max(BODY_LIMIT))
    .layer(axum::middleware::map_response(|mut r:Response|async move{
        r.headers_mut().insert(header::CACHE_CONTROL,header::HeaderValue::from_static("no-store"));
        r.headers_mut().insert("x-content-type-options",header::HeaderValue::from_static("nosniff"));
        r.headers_mut().insert("referrer-policy",header::HeaderValue::from_static("no-referrer"));
        r.headers_mut().insert("x-robots-tag",header::HeaderValue::from_static("noindex, nofollow"));
        r.headers_mut().insert("content-security-policy",header::HeaderValue::from_static("default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"));r
    }))
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = std::env::var("STUDIO_PUBLIC_URL")?.trim_end_matches('/').to_string();
    let parsed = url::Url::parse(&base)?;
    if parsed.scheme() != "https" && !(parsed.scheme() == "http" && parsed.host_str() == Some("127.0.0.1")) {
        return Err("STUDIO_PUBLIC_URL requires HTTPS (or loopback for tests)".into());
    }
    if parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() || !parsed.username().is_empty() || parsed.password().is_some()
    {
        return Err("STUDIO_PUBLIC_URL must be an origin without credentials or query".into());
    }
    let cli = std::fs::canonicalize(std::env::var("STUDIO_CLI").unwrap_or_else(|_| "/usr/local/bin/solidcraft-cli".into()))?;
    if !FsPath::new(&cli).is_file() {
        return Err("CAD executable missing".into());
    }
    let password = std::env::var("STUDIO_ADMIN_PASSWORD").ok().filter(|p| p.len() >= 20).map(|p| hash(&p));
    if password.is_none() {
        eprintln!("Owner login disabled: set STUDIO_ADMIN_PASSWORD to at least 20 characters.");
    }
    let signing = std::env::var("STUDIO_OAUTH_SIGNING_KEY")?;
    if signing.len() < 32 {
        return Err("STUDIO_OAUTH_SIGNING_KEY must have at least 32 characters".into());
    }
    let client_key = hash(&signing);
    let app = Arc::new(Shared { base, cli, password, client_key, inner: Mutex::new(Store::default()), compute: Mutex::new(()) });
    let cleanup = app.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let workers: Vec<_> = cleanup.inner.lock().await.sessions.iter().map(|(id, w)| (id.clone(), w.clone())).collect();
            for (id, w) in workers {
                if let Ok(mut w) = w.try_lock()
                    && w.touched.elapsed() > TTL
                {
                    let _ = w.child.kill().await;
                    drop(w);
                    cleanup.inner.lock().await.sessions.remove(&id);
                }
            }
            let mut db = cleanup.inner.lock().await;
            let now = Instant::now();
            db.pending.retain(|_, (_, e)| *e > now);
            db.codes.retain(|_, c| c.expires > now);
            db.tokens.retain(|_, t| t.expires > now);
            db.refresh.retain(|_, t| t.expires > now);
            db.downloads.retain(|_, d| d.expires > now);
        }
    });
    let port: u16 = std::env::var("PORT").unwrap_or_else(|_| "10000".into()).parse()?;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).await?;
    eprintln!("Robotics Studio cloud listening on port {port}");
    let account = account::Account::new()?;
    axum::serve(listener, router_with_account(app, account))
        .with_graceful_shutdown(async {
            #[cfg(unix)]
            if let Ok(mut terminate) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
        })
        .await?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    fn app() -> App {
        Arc::new(Shared {
            base: "https://studio.test".into(),
            cli: std::env::var_os("STUDIO_TEST_CLI").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/bin/cat")),
            password: Some(hash("test-password-at-least-20-characters")),
            client_key: hash("test-signing-key-at-least-32-characters"),
            inner: Mutex::new(Store::default()),
            compute: Mutex::new(()),
        })
    }
    #[allow(clippy::too_many_arguments)]
    async fn request(
        app: &App,
        method: &str,
        path: &str,
        payload: &str,
        bearer: Option<&str>,
        sid: Option<&str>,
        origin: Option<&str>,
        form: bool,
    ) -> (StatusCode, HeaderMap, Value, String) {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", if form { "application/x-www-form-urlencoded" } else { "application/json" })
            .header("accept", "application/json, text/event-stream");
        if let Some(t) = bearer {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        if let Some(t) = sid {
            req = req.header("mcp-session-id", t);
        }
        if let Some(t) = origin {
            req = req.header("origin", t);
        }
        let r = router(app.clone()).oneshot(req.body(Body::from(payload.to_string())).unwrap()).await.unwrap();
        let status = r.status();
        let headers = r.headers().clone();
        let bytes = to_bytes(r.into_body(), 32 * 1024 * 1024).await.unwrap();
        let text = String::from_utf8_lossy(&bytes).to_string();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, headers, json, text)
    }
    fn form(pairs: &[(&str, &str)]) -> String {
        url::form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs.iter().copied()).finish()
    }
    async fn grant(app: &App) -> (String, String, String) {
        let callback = "https://chatgpt.com/connector_platform_oauth_redirect";
        let (status, _, v, _) = request(
            app,
            "POST",
            "/oauth/register",
            &json!({"redirect_uris":[callback],"token_endpoint_auth_method":"none"}).to_string(),
            None,
            None,
            None,
            false,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let client = v["client_id"].as_str().unwrap().to_string();
        let verifier = "a".repeat(64);
        let challenge = URL_SAFE_NO_PAD.encode(hash(&verifier));
        let params = form(&[
            ("client_id", &client),
            ("redirect_uri", callback),
            ("response_type", "code"),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("resource", "https://studio.test/mcp"),
            ("scope", "cad"),
            ("state", "state-must-roundtrip"),
        ]);
        let (status, _, _, html) = request(app, "GET", &format!("/oauth/authorize?{params}"), "", None, None, None, false).await;
        assert_eq!(status, StatusCode::OK);
        let ticket = html.split("name=\"ticket\" value=\"").nth(1).unwrap().split('"').next().unwrap();
        let (status, headers, _, _) = request(
            app,
            "POST",
            "/oauth/authorize",
            &form(&[("ticket", ticket), ("password", "test-password-at-least-20-characters")]),
            None,
            None,
            Some("https://studio.test"),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let location = url::Url::parse(headers.get("location").unwrap().to_str().unwrap()).unwrap();
        let q: HashMap<_, _> = location.query_pairs().into_owned().collect();
        assert_eq!(q.get("state").unwrap(), "state-must-roundtrip");
        assert_eq!(q.get("iss").unwrap(), "https://studio.test");
        let exchange = form(&[
            ("grant_type", "authorization_code"),
            ("client_id", &client),
            ("redirect_uri", callback),
            ("code", q.get("code").unwrap()),
            ("code_verifier", &verifier),
            ("resource", "https://studio.test/mcp"),
        ]);
        let (status, _, v, _) = request(app, "POST", "/oauth/token", &exchange, None, None, None, true).await;
        assert_eq!(status, StatusCode::OK);
        let (replay, _, err, _) = request(app, "POST", "/oauth/token", &exchange, None, None, None, true).await;
        assert_eq!(replay, StatusCode::BAD_REQUEST);
        assert_eq!(err["error"], "invalid_grant");
        (client, v["access_token"].as_str().unwrap().into(), v["refresh_token"].as_str().unwrap().into())
    }
    #[test]
    fn registered_client_survives_restart_but_not_tampering_or_key_rotation() {
        let app = app();
        let uri = "https://chatgpt.com/connector_platform_oauth_redirect".to_string();
        let id = signed_client(&app, std::slice::from_ref(&uri)).unwrap();
        let restarted = super::tests::app();
        assert_eq!(client_uris(&restarted, &id), Some(vec![uri]));
        assert!(client_uris(&restarted, &format!("{id}tampered")).is_none());
        let mut different = Shared {
            base: app.base.clone(),
            cli: app.cli.clone(),
            password: app.password,
            client_key: hash("a different signing key"),
            inner: Mutex::new(Store::default()),
            compute: Mutex::new(()),
        };
        assert!(client_uris(&different, &id).is_none());
        different.client_key = app.client_key;
        different.base = "https://other.test".into();
        assert!(client_uris(&different, &id).is_none());
    }
    #[tokio::test]
    async fn oauth_discovery_refresh_and_access_boundaries() {
        let app = app();
        let (status, h, _, _) = request(&app, "POST", "/mcp", "{}", None, None, None, false).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(h.get("www-authenticate").is_some());
        assert_eq!(h["cache-control"], "no-store");
        let (_, _, metadata, _) = request(&app, "GET", "/.well-known/oauth-protected-resource/mcp", "", None, None, None, false).await;
        assert_eq!(metadata["resource"], "https://studio.test/mcp");
        let (client, access, refresh) = grant(&app).await;
        let (_, _, v, _) = request(
            &app,
            "POST",
            "/oauth/token",
            &form(&[("grant_type", "refresh_token"), ("client_id", &client), ("refresh_token", &refresh), ("resource", "https://studio.test/mcp")]),
            None,
            None,
            None,
            true,
        )
        .await;
        assert!(v["access_token"].is_string());
        assert_ne!(v["refresh_token"], refresh);
        let (status, _, _, _) = request(&app, "GET", "/mcp", "", Some(&access), None, None, false).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        let (status, _, _, _) = request(
            &app,
            "POST",
            "/mcp",
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize"}).to_string(),
            Some(&access),
            None,
            Some("https://evil.test"),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _, _, _) =
            request(&app, "POST", "/oauth/register", &json!({"redirect_uris":["https://evil.test/callback"]}).to_string(), None, None, None, false)
                .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    #[tokio::test]
    async fn invalid_pkce_expired_tokens_and_missing_resource_fail_closed() {
        let app = app();
        let verifier = "b".repeat(64);
        let a = Authorize {
            client_id: "client".into(),
            redirect_uri: "https://chatgpt.com/connector_platform_oauth_redirect".into(),
            response_type: "code".into(),
            code_challenge: URL_SAFE_NO_PAD.encode(hash(&verifier)),
            code_challenge_method: "S256".into(),
            resource: resource(&app),
            state: "state".into(),
            scope: "cad".into(),
        };
        app.inner.lock().await.codes.insert(hash_key("one-use"), Code { request: a, expires: Instant::now() + TTL, principal: "owner".into() });
        let (status, _, v, _) = request(
            &app,
            "POST",
            "/oauth/token",
            &form(&[
                ("grant_type", "authorization_code"),
                ("client_id", "client"),
                ("redirect_uri", "https://chatgpt.com/connector_platform_oauth_redirect"),
                ("code", "one-use"),
                ("code_verifier", &"c".repeat(64)),
                ("resource", "https://studio.test/mcp"),
            ]),
            None,
            None,
            None,
            true,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(v["error"], "invalid_grant");
        app.inner.lock().await.tokens.insert(
            hash_key("expired"),
            Token { principal: "owner".into(), client: "client".into(), expires: Instant::now() - Duration::from_secs(1) },
        );
        let (status, _, _, _) = request(&app, "GET", "/mcp", "", Some("expired"), None, None, false).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _, v, _) = request(&app, "POST", "/oauth/token", &form(&[("grant_type", "authorization_code")]), None, None, None, true).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(v["error"], "invalid_target");
    }
    #[tokio::test]
    async fn owner_login_retry_expiry_rate_limit_and_replay() {
        let app = app();
        let a = Authorize {
            client_id: "registered-client".into(),
            redirect_uri: "https://chatgpt.com/connector_platform_oauth_redirect".into(),
            response_type: "code".into(),
            code_challenge: URL_SAFE_NO_PAD.encode(hash(&"v".repeat(64))),
            code_challenge_method: "S256".into(),
            resource: resource(&app),
            state: "roundtrip-state".into(),
            scope: "cad".into(),
        };
        app.inner.lock().await.pending.insert("retry-ticket".into(), (a.clone(), Instant::now() + Duration::from_secs(300)));
        let wrong = form(&[("ticket", "retry-ticket"), ("password", "wrong-password-marker")]);
        let correct = form(&[("ticket", "retry-ticket"), ("password", "test-password-at-least-20-characters")]);
        let (status, _, _, html) = request(&app, "POST", "/oauth/authorize", &wrong, None, None, Some("https://evil.test"), true).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(!html.contains("name=\"password\""));
        let (status, _, _, html) = request(&app, "POST", "/oauth/authorize", &wrong, None, None, Some("https://studio.test"), true).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(html.contains("did not match"));
        assert!(html.contains("name=\"ticket\" value=\"retry-ticket\""));
        assert!(!html.contains("wrong-password-marker"));
        assert!(app.inner.lock().await.codes.is_empty());
        let (status, h, _, _) = request(&app, "POST", "/oauth/authorize", &correct, None, None, Some("https://studio.test"), true).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert!(h["location"].to_str().unwrap().contains("iss=https%3A%2F%2Fstudio.test"));
        assert!(!app.inner.lock().await.pending.contains_key("retry-ticket"));
        let (status, _, _, _) = request(&app, "POST", "/oauth/authorize", &correct, None, None, Some("https://studio.test"), true).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        app.inner.lock().await.pending.insert("expired-ticket".into(), (a.clone(), Instant::now() - Duration::from_secs(1)));
        let expired = form(&[("ticket", "expired-ticket"), ("password", "test-password-at-least-20-characters")]);
        let (status, _, _, html) = request(&app, "POST", "/oauth/authorize", &expired, None, None, Some("https://studio.test"), true).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(html.contains("form expired"));
        assert!(!app.inner.lock().await.pending.contains_key("expired-ticket"));
        app.inner.lock().await.pending.insert("limited-ticket".into(), (a, Instant::now() + Duration::from_secs(300)));
        app.inner.lock().await.login_attempts = (0..10).map(|_| Instant::now()).collect();
        let limited = form(&[("ticket", "limited-ticket"), ("password", "test-password-at-least-20-characters")]);
        let (status, _, _, html) = request(&app, "POST", "/oauth/authorize", &limited, None, None, Some("https://studio.test"), true).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert!(html.contains("Too many sign-in attempts"));
        assert!(app.inner.lock().await.pending.contains_key("limited-ticket"));
    }
    #[tokio::test]
    async fn real_cad_sessions_export_download_and_isolation() {
        if std::env::var_os("STUDIO_TEST_CLI").is_none() {
            eprintln!("Set STUDIO_TEST_CLI to run the real CAD integration test");
            return;
        }
        let app = app();
        let (_, access, _) = grant(&app).await;
        let init=json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}).to_string();
        let (status, h, v, _) = request(&app, "POST", "/mcp", &init, Some(&access), None, None, false).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["result"]["protocolVersion"], "2025-06-18");
        let sid = h["mcp-session-id"].to_str().unwrap().to_string();
        let (_, _, v, _) =
            request(&app, "POST", "/mcp", &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}).to_string(), Some(&access), Some(&sid), None, false)
                .await;
        assert!(v["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "download_file"));
        async fn tool(app: &App, a: &str, s: &str, n: &str, args: Value) -> Value {
            let (status, _, v, _) = request(
                app,
                "POST",
                "/mcp",
                &json!({"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":n,"arguments":args}}).to_string(),
                Some(a),
                Some(s),
                None,
                false,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{v}");
            assert_ne!(v["result"]["isError"], true, "{v}");
            v
        }
        tool(&app, &access, &sid, "execute", json!({"command":"solid.box","params":{"length":20,"width":30,"height":10}})).await;
        let measured = tool(&app, &access, &sid, "measure", json!({})).await;
        let measured: Value = serde_json::from_str(measured["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(measured["total"]["volume_mm3"], 6000.0);
        let denied = request(
            &app,
            "POST",
            "/mcp",
            &json!({"jsonrpc":"2.0","id":20,"method":"tools/call","params":{"name":"save","arguments":{"path":"/tmp/cloud-escape.solidcraft"}}})
                .to_string(),
            Some(&access),
            Some(&sid),
            None,
            false,
        )
        .await;
        assert_eq!(denied.2["result"]["isError"], true, "Path escape was accepted");
        for name in ["part.step", "part.stl"] {
            tool(&app, &access, &sid, "export", json!({"path":name})).await;
            let v = tool(&app, &access, &sid, "download_file", json!({"filename":name})).await;
            let link = v["result"]["content"][0]["text"].as_str().unwrap().split_whitespace().find(|s| s.starts_with("https://")).unwrap();
            let u = url::Url::parse(link).unwrap();
            let (status, headers, _, body) = request(&app, "GET", u.path(), "", None, None, None, false).await;
            assert_eq!(status, StatusCode::OK);
            assert!(headers["content-disposition"].to_str().unwrap().contains(name));
            if name.ends_with("step") {
                assert!(body.contains("ISO-10303-21"));
            } else {
                assert!(body.len() > 84);
            }
        }
        let (_, h, _, _) = request(&app, "POST", "/mcp", &init, Some(&access), None, None, false).await;
        let sid2 = h["mcp-session-id"].to_str().unwrap().to_string();
        let measured = tool(&app, &access, &sid2, "measure", json!({})).await;
        assert!(!measured.to_string().contains("6000"));
        let (_, other, _) = grant(&app).await;
        let (status, _, _, _) =
            request(&app, "POST", "/mcp", &json!({"jsonrpc":"2.0","id":3,"method":"ping"}).to_string(), Some(&other), Some(&sid), None, false).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _, _, _) = request(&app, "DELETE", "/mcp", "", Some(&access), Some(&sid), None, false).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _, _, _) =
            request(&app, "POST", "/mcp", &json!({"jsonrpc":"2.0","id":3,"method":"ping"}).to_string(), Some(&access), Some(&sid), None, false).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    #[test]
    fn callbacks_and_filenames_are_confined() {
        assert!(redirect_allowed("https://chatgpt.com/connector_platform_oauth_redirect"));
        for s in [
            "https://evil.test/connector/oauth/x",
            "https://chatgpt.com.evil.test/connector/oauth/x",
            "https://chatgpt.com@evil.test/connector/oauth/x",
            "http://chatgpt.com/connector/oauth/x",
            "https://chatgpt.com/connector/oauth/x#fragment",
        ] {
            assert!(!redirect_allowed(s), "{s}");
        }
        assert!(filename("bracket-v1.step"));
        for s in ["../secret", "/etc/passwd", ".env", "home/file", "x\".step", "a\\b"] {
            assert!(!filename(s), "{s}");
        }
    }
}
