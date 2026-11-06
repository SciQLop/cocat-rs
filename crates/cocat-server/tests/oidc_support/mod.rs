//! A fake OpenID Connect provider: discovery, JWKS, a token endpoint that checks the PKCE
//! verifier, and userinfo. Its ID tokens are signed with `tests/fixtures/oidc_test_key.pem`.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::{Router, serve};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use cocat_server::oidc::ProviderConfig;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The modulus of the test key, base64url without padding, ready for a JWKS document.
const MODULUS: &str = "xQw8CEkrhXoyUQjJBE7uH5BwBX22bPwOLaybRt7ktdnnJ4d9eX8sh8JuIZh9801rad7DPeKO1gE0L5j3OkMVP2T9op4e6YYt3D24ald0LMrRRZ3PFDT9YLCN9dPy_ZqZLSFHRyvWgXYaqmTHcSPIl7ded6627fBuJcbHZvdx6jqkrydYNr6-cw0fqacBCmR2447a0OL1pfEIy2sai6SKeOk0HQrt-P-KSJ3fARmtJKu8N45bnwYnMH8RhayHZU0kzAeC_aPDyGDDZVziobKrozFisuN91GydB53ZZxyuskEb18FoP3RYynpVXvzMI0O__4NrzIQclkhJso2NqpELkw";
const KEY: &str = include_str!("../fixtures/oidc_test_key.pem");
const CLIENT_ID: &str = "client";
const CLIENT_SECRET: &str = "hunter2";
const ACCESS_TOKEN: &str = "access-token";

#[derive(Clone)]
struct Options {
    challenge: String,
    nonce: String,
    claim_nonce: Option<String>,
    subject: String,
    userinfo_subject: Option<String>,
    email: Option<String>,
    issuer: Option<String>,
    audience: Option<String>,
    expires_in: i64,
    token_error: bool,
    tamper: bool,
    profile_email: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            challenge: String::new(),
            nonce: String::new(),
            claim_nonce: None,
            subject: "u1".to_owned(),
            userinfo_subject: None,
            email: None,
            issuer: None,
            audience: None,
            expires_in: 300,
            token_error: false,
            tamper: false,
            profile_email: None,
        }
    }
}

type Endpoints = (String, Arc<Mutex<Options>>);

/// A running provider, reachable at `http://127.0.0.1:<port>`.
pub struct Fake {
    base: String,
    options: Arc<Mutex<Options>>,
}

impl Fake {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let options = Arc::new(Mutex::new(Options::default()));
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .route("/token", post(token))
            .route("/userinfo", get(userinfo))
            .with_state((base.clone(), options.clone()));
        tokio::spawn(async move {
            serve(listener, app).await.unwrap();
        });
        Self { base, options }
    }

    pub fn config(&self, verifies_email: bool, name: &str) -> ProviderConfig {
        serde_json::from_value(json!({
            "name": name,
            "discovery_url": format!("{}/.well-known/openid-configuration", self.base),
            "client_id": CLIENT_ID,
            "client_secret": CLIENT_SECRET,
            "verifies_email": verifies_email,
        }))
        .unwrap()
    }

    /// How the test tells the token endpoint which challenge and nonce the server sent.
    pub fn agree(&self, challenge: &str, nonce: &str) {
        let mut options = self.options.lock().unwrap();
        options.challenge = challenge.to_owned();
        options.nonce = nonce.to_owned();
    }

    pub fn claim_nonce(&self, nonce: &str) {
        self.options.lock().unwrap().claim_nonce = Some(nonce.to_owned());
    }

    pub fn subject(&self, subject: &str) {
        self.options.lock().unwrap().subject = subject.to_owned();
    }

    pub fn email(&self, email: Option<&str>) {
        self.options.lock().unwrap().email = email.map(str::to_owned);
    }

    pub fn issuer(&self, issuer: &str) {
        self.options.lock().unwrap().issuer = Some(issuer.to_owned());
    }

    pub fn audience(&self, audience: &str) {
        self.options.lock().unwrap().audience = Some(audience.to_owned());
    }

    pub fn expires_in(&self, seconds: i64) {
        self.options.lock().unwrap().expires_in = seconds;
    }

    pub fn token_error(&self) {
        self.options.lock().unwrap().token_error = true;
    }

    pub fn tamper(&self) {
        self.options.lock().unwrap().tamper = true;
    }

    pub fn userinfo_subject(&self, subject: &str) {
        self.options.lock().unwrap().userinfo_subject = Some(subject.to_owned());
    }

    pub fn profile_email(&self, email: Option<&str>) {
        self.options.lock().unwrap().profile_email = email.map(str::to_owned);
    }
}

async fn discovery(State((base, _)): State<Endpoints>) -> Json<Value> {
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "userinfo_endpoint": format!("{base}/userinfo"),
        "jwks_uri": format!("{base}/jwks"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "scopes_supported": ["openid", "email"],
    }))
}

async fn jwks() -> Json<Value> {
    Json(json!({
        "keys": [{
            "kty": "RSA",
            "kid": "test",
            "use": "sig",
            "alg": "RS256",
            "n": MODULUS,
            "e": "AQAB",
        }],
    }))
}

async fn token(
    State((base, options)): State<Endpoints>,
    Form(form): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    let options = options.lock().unwrap().clone();
    if options.token_error {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_grant" })),
        )
            .into_response();
    }
    let verifier = form.get("code_verifier").cloned().unwrap_or_default();
    if URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) != options.challenge {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_grant" })),
        )
            .into_response();
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mut claims = json!({
        "iss": options.issuer.clone().unwrap_or_else(|| base.clone()),
        "sub": options.subject,
        "aud": options.audience.clone().unwrap_or_else(|| CLIENT_ID.to_owned()),
        "exp": now + options.expires_in,
        "iat": now,
        "nonce": options.claim_nonce.clone().unwrap_or(options.nonce),
    });
    if let Some(email) = &options.email {
        claims["email"] = json!(email);
    }
    let header = Header {
        alg: Algorithm::RS256,
        kid: Some("test".to_owned()),
        ..Header::default()
    };
    let key = EncodingKey::from_rsa_pem(KEY.as_bytes()).unwrap();
    let mut id_token = jsonwebtoken::encode(&header, &claims, &key).unwrap();
    if options.tamper {
        let mut parts: Vec<String> = id_token.split('.').map(str::to_owned).collect();
        let signature = parts.last_mut().unwrap();
        let first = signature.remove(0);
        signature.insert(0, if first == 'A' { 'B' } else { 'A' });
        id_token = parts.join(".");
    }
    Json(json!({
        "access_token": ACCESS_TOKEN,
        "token_type": "Bearer",
        "expires_in": options.expires_in,
        "id_token": id_token,
    }))
    .into_response()
}

async fn userinfo(State((_, options)): State<Endpoints>, headers: HeaderMap) -> impl IntoResponse {
    let authorized = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == format!("Bearer {ACCESS_TOKEN}"));
    if !authorized {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let options = options.lock().unwrap().clone();
    let subject = options
        .userinfo_subject
        .unwrap_or_else(|| options.subject.clone());
    let mut profile = json!({ "sub": subject });
    if let Some(email) = options.profile_email {
        profile["email"] = json!(email);
    }
    Json(profile).into_response()
}
