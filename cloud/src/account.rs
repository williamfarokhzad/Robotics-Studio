//! Identity-only OpenAI OIDC. Credentials stay on the server; no inference scopes.
use crate::{Failure, escape, hash_key, random};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

const ISSUER: &str = "https://auth.openai.com";
const DISCOVERY: &str = "https://auth.openai.com/.well-known/openid-configuration";
const SESSION: &str = "__Host-studio_session";
const TRANSACTION: &str = "__Host-studio_login";
const LIMIT: usize = 128;
#[derive(Clone)]
pub(crate) struct Account(Arc<Service>);
struct Service {
    config: Option<Config>,
    origin: String,
    http: reqwest::Client,
    db: Mutex<Store>,
}
struct Config {
    client: String,
    secret: Option<String>,
}
#[derive(Default)]
struct Store {
    pending: HashMap<String, Transaction>,
    sessions: HashMap<String, Session>,
}
struct Transaction {
    browser: String,
    verifier: String,
    nonce: String,
    expires: Instant,
}
struct Session {
    identity: Identity,
    csrf: String,
    expires: Instant,
}
#[derive(Clone, Serialize)]
struct Identity {
    issuer: String,
    client: String,
    subject: String,
    name: String,
    email: Option<String>,
}
#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
}
#[derive(Deserialize)]
struct Callback {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
}
#[derive(Deserialize)]
struct Tokens {
    id_token: String,
}
#[derive(Deserialize)]
struct Claims {
    sub: String,
    aud: serde_json::Value,
    nonce: String,
    iat: u64,
    azp: Option<String>,
    name: Option<String>,
    email: Option<String>,
    email_verified: Option<bool>,
}

impl Account {
    pub(crate) fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let origin = std::env::var("STUDIO_LOGIN_ORIGIN").unwrap_or_else(|_| "https://robotics-studio.onrender.com".into());
        let origin = origin.trim_end_matches('/').to_string();
        let url = url::Url::parse(&origin)?;
        if url.scheme() != "https"
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("STUDIO_LOGIN_ORIGIN must be an HTTPS origin".into());
        }
        let client = std::env::var("OPENAI_CLIENT_ID").ok().filter(|c| !c.trim().is_empty());
        let config = client.map(|client| Config { client, secret: std::env::var("OPENAI_CLIENT_SECRET").ok().filter(|s| !s.is_empty()) });
        let http = reqwest::Client::builder().timeout(Duration::from_secs(15)).redirect(reqwest::redirect::Policy::none()).build()?;
        Ok(Self(Arc::new(Service { config, origin, http, db: Mutex::new(Store::default()) })))
    }
}
fn unavailable() -> Failure {
    (StatusCode::SERVICE_UNAVAILABLE, "ChatGPT sign-in is awaiting OpenAI app registration. You can keep designing without an account.".into())
}
fn rejected() -> Failure {
    (StatusCode::BAD_REQUEST, "Sign-in could not be verified. Please start again.".into())
}
fn cookie(h: &HeaderMap, name: &str) -> Option<String> {
    h.get(header::COOKIE)?.to_str().ok()?.split(';').filter_map(|p| p.trim().split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
}
fn set_cookie(r: &mut Response, name: &str, value: &str, seconds: u32) {
    if let Ok(v) = format!("{name}={value}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={seconds}").parse() {
        r.headers_mut().append(header::SET_COOKIE, v);
    }
}
fn sweep(db: &mut Store) {
    let now = Instant::now();
    db.pending.retain(|_, x| x.expires > now);
    db.sessions.retain(|_, x| x.expires > now);
}
fn trusted_endpoint(s: &str) -> bool {
    url::Url::parse(s).is_ok_and(|u| {
        u.scheme() == "https"
            && u.host_str() == Some("auth.openai.com")
            && u.port().is_none()
            && u.username().is_empty()
            && u.password().is_none()
            && u.fragment().is_none()
    })
}
async fn read_json<T: serde::de::DeserializeOwned>(r: reqwest::Response) -> Result<T, Failure> {
    if !r.status().is_success() || r.content_length().is_some_and(|n| n > 262144) {
        return Err(rejected());
    }
    let mut r = r;
    let mut body = Vec::new();
    while let Some(chunk) = r.chunk().await.map_err(|_| rejected())? {
        if body.len() + chunk.len() > 262144 {
            return Err(rejected());
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| rejected())
}
async fn discovery(a: &Account) -> Result<Discovery, Failure> {
    let d: Discovery = read_json(a.0.http.get(DISCOVERY).send().await.map_err(|_| unavailable())?).await?;
    if d.issuer != ISSUER || ![&d.authorization_endpoint, &d.token_endpoint, &d.jwks_uri].iter().all(|u| trusted_endpoint(u)) {
        return Err(rejected());
    }
    Ok(d)
}
async fn start(State(a): State<Account>) -> Result<Response, Failure> {
    let c = a.0.config.as_ref().ok_or_else(unavailable)?;
    let d = discovery(&a).await?;
    let state = random();
    let browser = random();
    let verifier = random();
    let nonce = random();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut u = url::Url::parse(&d.authorization_endpoint).map_err(|_| rejected())?;
    u.query_pairs_mut()
        .append_pair("client_id", &c.client)
        .append_pair("redirect_uri", &format!("{}/auth/openai/callback", a.0.origin))
        .append_pair("response_type", "code")
        .append_pair("scope", "openid profile email")
        .append_pair("state", &state)
        .append_pair("nonce", &nonce)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    let mut db = a.0.db.lock().await;
    sweep(&mut db);
    if db.pending.len() >= LIMIT {
        return Err((StatusCode::TOO_MANY_REQUESTS, "Please try again shortly.".into()));
    }
    db.pending
        .insert(hash_key(&state), Transaction { browser: hash_key(&browser), verifier, nonce, expires: Instant::now() + Duration::from_secs(300) });
    let mut r = Redirect::to(u.as_str()).into_response();
    set_cookie(&mut r, TRANSACTION, &browser, 300);
    Ok(r)
}
fn verify(token: &str, keys: &JwkSet, c: &Config, nonce: &str) -> Result<Identity, Failure> {
    let h = decode_header(token).map_err(|_| rejected())?;
    if h.alg != Algorithm::RS256 {
        return Err(rejected());
    }
    let key = keys.find(h.kid.as_deref().ok_or_else(rejected)?).ok_or_else(rejected)?;
    let mut v = Validation::new(Algorithm::RS256);
    v.set_issuer(&[ISSUER]);
    v.set_audience(&[&c.client]);
    v.leeway = 5;
    v.set_required_spec_claims(&["sub", "exp", "iat", "iss", "aud"]);
    let claims = decode::<Claims>(token, &DecodingKey::from_jwk(key).map_err(|_| rejected())?, &v).map_err(|_| rejected())?.claims;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| rejected())?.as_secs();
    if (claims.aud.as_array().is_some_and(|a| a.len() > 1) && claims.azp.as_deref() != Some(&c.client))
        || claims.sub.is_empty()
        || claims.nonce != nonce
        || claims.iat > now + 5
        || claims.azp.as_ref().is_some_and(|x| x != &c.client)
    {
        return Err(rejected());
    }
    Ok(Identity {
        issuer: ISSUER.into(),
        client: c.client.clone(),
        subject: claims.sub,
        name: claims.name.filter(|x| !x.is_empty()).unwrap_or_else(|| "ChatGPT account".into()),
        email: claims.email.filter(|_| claims.email_verified == Some(true)),
    })
}
async fn callback(State(a): State<Account>, h: HeaderMap, Query(q): Query<Callback>) -> Result<Response, Failure> {
    let c = a.0.config.as_ref().ok_or_else(unavailable)?;
    let state = q.state.filter(|s| s.len() <= 128).ok_or_else(rejected)?;
    let browser = cookie(&h, TRANSACTION).ok_or_else(rejected)?;
    let t = a.0.db.lock().await.pending.remove(&hash_key(&state)).ok_or_else(rejected)?;
    if t.expires <= Instant::now() || t.browser != hash_key(&browser) || q.error.is_some() {
        return Err(rejected());
    }
    let code = q.code.filter(|s| !s.is_empty() && s.len() <= 8192).ok_or_else(rejected)?;
    let d = discovery(&a).await?;
    let redirect = format!("{}/auth/openai/callback", a.0.origin);
    let mut request = a.0.http.post(&d.token_endpoint).form(&[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("client_id", &c.client),
        ("redirect_uri", &redirect),
        ("code_verifier", &t.verifier),
    ]);
    if let Some(secret) = &c.secret {
        request = request.basic_auth(&c.client, Some(secret));
    }
    let tokens: Tokens = read_json(request.send().await.map_err(|_| rejected())?).await?;
    // Fetch current keys, including rotated keys. No credentials are sent to the JWKS endpoint.
    let keys: JwkSet = read_json(a.0.http.get(&d.jwks_uri).send().await.map_err(|_| rejected())?).await?;
    let identity = verify(&tokens.id_token, &keys, c, &t.nonce)?;
    let sid = random();
    let mut db = a.0.db.lock().await;
    sweep(&mut db);
    if db.sessions.len() >= LIMIT {
        return Err((StatusCode::TOO_MANY_REQUESTS, "Please try again shortly.".into()));
    }
    if let Some(old) = cookie(&h, SESSION) {
        db.sessions.remove(&hash_key(&old));
    }
    db.sessions.insert(hash_key(&sid), Session { identity, csrf: random(), expires: Instant::now() + Duration::from_secs(28800) });
    let mut r = Redirect::to(&format!("{}/account", a.0.origin)).into_response();
    set_cookie(&mut r, SESSION, &sid, 28800);
    set_cookie(&mut r, TRANSACTION, "", 0);
    Ok(r)
}
async fn status(State(a): State<Account>, h: HeaderMap) -> Json<serde_json::Value> {
    let mut db = a.0.db.lock().await;
    sweep(&mut db);
    let session = cookie(&h, SESSION).and_then(|s| db.sessions.get(&hash_key(&s)));
    Json(match session {
        Some(s) => json!({"available":a.0.config.is_some(), "authenticated":true,"account":s.identity,"csrf":s.csrf}),
        None => json!({"available":a.0.config.is_some(),"authenticated":false}),
    })
}
async fn logout(State(a): State<Account>, h: HeaderMap) -> Result<Response, Failure> {
    if h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) != Some(&a.0.origin) {
        return Err((StatusCode::FORBIDDEN, "Origin rejected".into()));
    }
    let sid = cookie(&h, SESSION).ok_or_else(rejected)?;
    let mut db = a.0.db.lock().await;
    let s = db.sessions.get(&hash_key(&sid)).ok_or_else(rejected)?;
    if h.get("x-studio-csrf").and_then(|v| v.to_str().ok()) != Some(s.csrf.as_str()) {
        return Err((StatusCode::FORBIDDEN, "Request rejected".into()));
    }
    db.sessions.remove(&hash_key(&sid));
    let mut r = StatusCode::NO_CONTENT.into_response();
    set_cookie(&mut r, SESSION, "", 0);
    Ok(r)
}
async fn page(State(a): State<Account>) -> Html<String> {
    let available = if a.0.config.is_some() {
        "<a class=\"primary\" href=\"/auth/openai\"><img class=\"logo\" src=\"/auth/logo-black.svg\" alt=\"\">Continue with ChatGPT</a>"
    } else {
        "<button class=\"primary\" disabled><img class=\"logo\" src=\"/auth/logo-black.svg\" alt=\"\">Continue with ChatGPT</button><p class=\"muted\">Sign-in is awaiting OpenAI app approval. You can keep designing without an account.</p>"
    };
    Html(super::pages::shell(
        "Your account",
        &format!(
            "<span class=\"eyebrow\">ROBOTICS STUDIO</span><h1>Your creative workspace.</h1><p>Sign in with your ChatGPT account. Your designs stay yours.</p><section id=\"account\">{available}</section><p class=\"muted\">Identity only. This does not access your ChatGPT conversations or enable AI requests.</p><a class=\"secondary\" href=\"{}\">Back to Studio</a>",
            escape(&a.0.origin)
        ),
    ))
}
pub(crate) fn routes(a: Account) -> Router {
    Router::new()
        .route("/account", get(page))
        .route("/auth/session", get(status))
        .route("/auth/openai", get(start))
        .route("/auth/openai/callback", get(callback))
        .route("/auth/logout", post(logout))
        .with_state(a)
}
#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;
    #[test]
    fn rejects_external_discovery_and_cookie_confusion() {
        assert!(trusted_endpoint("https://auth.openai.com/.well-known/jwks.json"));
        for s in
            ["https://auth.openai.com.evil.test/a", "http://auth.openai.com/a", "https://user@auth.openai.com/a", "https://auth.openai.com:443/a#x"]
        {
            assert!(!trusted_endpoint(s));
        }
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, "x__Host-studio_session=bad; __Host-studio_session=good".parse().unwrap());
        assert_eq!(cookie(&h, SESSION).as_deref(), Some("good"));
    }
    #[test]
    fn signed_identity_checks_signature_issuer_audience_nonce_and_time() {
        // Public test fixture. This key is never used by the running service.
        let der = base64::engine::general_purpose::STANDARD
            .decode(include_str!("account_test_key.pem").lines().filter(|l| !l.starts_with("-----")).collect::<String>())
            .unwrap();
        let key = jsonwebtoken::EncodingKey::from_rsa_der(&der);
        let keys: JwkSet = serde_json::from_str(include_str!("account_test_jwks.json")).unwrap();
        let c = Config { client: "oaiapp_test".into(), secret: None };
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let mut h = jsonwebtoken::Header::new(Algorithm::RS256);
        h.kid = Some("test-only".into());
        let claims = json!({"sub":"user-1","iss":ISSUER,"aud":c.client,"exp":now+300,"iat":now,"nonce":"nonce","name":"William","email":"test@example.com","email_verified":true});
        let sign = |v: &serde_json::Value| jsonwebtoken::encode(&h, v, &key).unwrap();
        assert_eq!(verify(&sign(&claims), &keys, &c, "nonce").unwrap().subject, "user-1");
        for (field, value) in [
            ("iss", json!("https://evil.test")),
            ("aud", json!("other-client")),
            ("exp", json!(now - 60)),
            ("iat", json!(now + 60)),
            ("nonce", json!("other")),
            ("sub", json!("")),
            ("aud", json!(["oaiapp_test", "other"])),
        ] {
            let mut v = claims.clone();
            v[field] = value;
            assert!(verify(&sign(&v), &keys, &c, "nonce").is_err(), "{field}");
        }
        let token = sign(&claims);
        let mut damaged = token.into_bytes();
        let n = damaged.len();
        damaged[n - 10] = if damaged[n - 10] == b'A' { b'B' } else { b'A' };
        assert!(verify(std::str::from_utf8(&damaged).unwrap(), &keys, &c, "nonce").is_err());
        let mut v = claims;
        v["email_verified"] = json!(false);
        assert!(verify(&sign(&v), &keys, &c, "nonce").unwrap().email.is_none());
    }
    #[tokio::test]
    async fn callback_binding_replay_and_logout_require_browser_proof() {
        let a = Account(Arc::new(Service {
            config: Some(Config { client: "oaiapp_test".into(), secret: None }),
            origin: "https://studio.test".into(),
            http: reqwest::Client::new(),
            db: Mutex::new(Store::default()),
        }));
        a.0.db.lock().await.pending.insert(
            hash_key("state"),
            Transaction { browser: hash_key("browser"), verifier: random(), nonce: random(), expires: Instant::now() + Duration::from_secs(300) },
        );
        let r = routes(a.clone())
            .oneshot(
                axum::http::Request::builder()
                    .uri("/auth/openai/callback?state=state&code=code")
                    .header("cookie", format!("{TRANSACTION}=wrong"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        assert!(a.0.db.lock().await.pending.is_empty());
        a.0.db.lock().await.sessions.insert(
            hash_key("session"),
            Session {
                identity: Identity { issuer: ISSUER.into(), client: "c".into(), subject: "s".into(), name: "N".into(), email: None },
                csrf: "csrf".into(),
                expires: Instant::now() + Duration::from_secs(300),
            },
        );
        for (origin, csrf, status) in [
            ("https://evil.test", "csrf", StatusCode::FORBIDDEN),
            ("https://studio.test", "wrong", StatusCode::FORBIDDEN),
            ("https://studio.test", "csrf", StatusCode::NO_CONTENT),
        ] {
            let r = routes(a.clone())
                .oneshot(
                    axum::http::Request::builder()
                        .uri("/auth/logout")
                        .method("POST")
                        .header("origin", origin)
                        .header("x-studio-csrf", csrf)
                        .header("cookie", format!("{SESSION}=session"))
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(r.status(), status);
        }
        assert!(a.0.db.lock().await.sessions.is_empty());
    }
    #[tokio::test]
    async fn unconfigured_login_never_fakes_a_session() {
        let a = Account(Arc::new(Service {
            config: None,
            origin: "https://studio.test".into(),
            http: reqwest::Client::new(),
            db: Mutex::new(Store::default()),
        }));
        let router = routes(a);
        let res = router.clone().oneshot(axum::http::Request::builder().uri("/auth/openai").body(axum::body::Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        let res = router.oneshot(axum::http::Request::builder().uri("/auth/session").body(axum::body::Body::empty()).unwrap()).await.unwrap();
        let b = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(v["authenticated"], false);
        assert_eq!(v["available"], false);
    }
}
