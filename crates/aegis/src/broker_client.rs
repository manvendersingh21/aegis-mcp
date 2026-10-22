//! The main app's side of the fake-key broker (aegis-broker on :8788).
//!
//! The main process holds only the broker admin token and FAKE keys; real
//! provider keys live in the broker. This module: (1) fetches the fake keys the
//! main process itself uses at boot, (2) serves the per-person Keys API by
//! forwarding to the broker's admin API with the logged-in person's subject.

use aegis_core::IssuedFakeKey;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Clone)]
pub struct BrokerClient {
    pub base_url: String,
    token: String,
    http: reqwest::Client,
}

impl BrokerClient {
    /// `AEGIS_BROKER_URL` (default http://127.0.0.1:8788) + `AEGIS_BROKER_TOKEN`.
    pub fn from_env() -> Option<Self> {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Some(BrokerClient {
            base_url: get("AEGIS_BROKER_URL").unwrap_or_else(|| "http://127.0.0.1:8788".into()).trim_end_matches('/').to_string(),
            token: get("AEGIS_BROKER_TOKEN")?,
            http: reqwest::Client::builder().timeout(Duration::from_secs(10)).build().ok()?,
        })
    }

    pub fn proxy_url(&self, provider: &str, path: &str) -> String {
        format!("{}/proxy/{provider}/{}", self.base_url, path.trim_start_matches('/'))
    }

    async fn admin(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<(StatusCode, Value), String> {
        let mut req = self.http.request(method, format!("{}{path}", self.base_url)).bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.map_err(|e| format!("broker unreachable: {e}"))?;
        let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let value = resp.json().await.unwrap_or(Value::Null);
        Ok((status, value))
    }

    /// Fake key for the main process itself (agent "aegis-gateway").
    pub async fn issue(&self, subject: &str, agent_id: &str, provider: &str, ttl_secs: i64) -> Result<IssuedFakeKey, String> {
        let body = json!({"subject": subject, "agent_id": agent_id, "provider": provider, "ttl_secs": ttl_secs});
        match self.admin(reqwest::Method::POST, "/admin/fake-keys", Some(body)).await? {
            (s, v) if s.is_success() => serde_json::from_value(v).map_err(|e| e.to_string()),
            (s, v) => Err(format!("broker refused fake key for {provider}: {s} {v}")),
        }
    }

    /// Revoke `subject`'s fake keys (`prefix: None` = all of them).
    pub async fn revoke(&self, subject: &str, prefix: Option<&str>) -> Result<Value, String> {
        let body = json!({"subject": subject, "prefix": prefix});
        self.admin(reqwest::Method::POST, "/admin/fake-keys/revoke", Some(body)).await.map(|(_, v)| v)
    }

    /// Decrypted vaults (personal data is sealed at rest in Supabase).
    pub async fn vault(&self, subject: &str) -> Result<Value, String> {
        match self.admin(reqwest::Method::GET, &format!("/admin/vault/{subject}"), None).await? {
            (s, v) if s.is_success() => Ok(v),
            (s, v) => Err(format!("broker vault {subject}: {s} {v}")),
        }
    }

    /// `/api/keys*` for the logged-in person.
    pub fn router(self, app_base_url: String) -> Router {
        Router::new()
            .route("/api/keys", get(list_keys))
            .route("/api/keys/secrets", post(add_secret))
            .route("/api/keys/fake", post(issue_fake))
            .route("/api/keys/revoke", post(revoke))
            .with_state(KeysState { broker: self, auth: crate::auth::SessionAuth::new(app_base_url) })
    }
}

#[derive(Clone)]
struct KeysState {
    broker: BrokerClient,
    auth: crate::auth::SessionAuth,
}

fn reply(result: Result<(StatusCode, Value), String>) -> Response {
    match result {
        Ok((status, value)) => (status, Json(value)).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({"error": e}))).into_response(),
    }
}

async fn list_keys(State(st): State<KeysState>, headers: HeaderMap) -> Response {
    let Some(subject) = st.auth.subject(&headers).await else { return StatusCode::UNAUTHORIZED.into_response() };
    let secrets = st.broker.admin(reqwest::Method::GET, &format!("/admin/secrets?subject={subject}"), None).await;
    let fakes = st.broker.admin(reqwest::Method::GET, &format!("/admin/fake-keys?subject={subject}"), None).await;
    match (secrets, fakes) {
        (Ok((_, s)), Ok((_, f))) => Json(json!({"secrets": s, "fake_keys": f})).into_response(),
        (Err(e), _) | (_, Err(e)) => (StatusCode::BAD_GATEWAY, Json(json!({"error": e}))).into_response(),
    }
}

async fn add_secret(State(st): State<KeysState>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let Some(subject) = st.auth.subject(&headers).await else { return StatusCode::UNAUTHORIZED.into_response() };
    // Internal records (e.g. the sealed `__vault`) are not user-writable.
    let provider = body["provider"].as_str().unwrap_or_default();
    if provider.is_empty() || provider.starts_with("__") || !provider.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid provider name"}))).into_response();
    }
    let forwarded = json!({"subject": subject, "provider": provider, "label": body["label"], "value": body["value"]});
    reply(st.broker.admin(reqwest::Method::POST, "/admin/secrets", Some(forwarded)).await)
}

async fn issue_fake(State(st): State<KeysState>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let Some(subject) = st.auth.subject(&headers).await else { return StatusCode::UNAUTHORIZED.into_response() };
    let ttl = body["ttl_secs"].as_i64().unwrap_or(3600).clamp(60, 86_400);
    let forwarded = json!({"subject": subject, "agent_id": body["agent_id"], "provider": body["provider"], "ttl_secs": ttl});
    reply(st.broker.admin(reqwest::Method::POST, "/admin/fake-keys", Some(forwarded)).await)
}

async fn revoke(State(st): State<KeysState>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let Some(subject) = st.auth.subject(&headers).await else { return StatusCode::UNAUTHORIZED.into_response() };
    let forwarded = json!({"subject": subject, "prefix": body.get("prefix").cloned().unwrap_or(Value::Null)});
    reply(st.broker.admin(reqwest::Method::POST, "/admin/fake-keys/revoke", Some(forwarded)).await)
}
