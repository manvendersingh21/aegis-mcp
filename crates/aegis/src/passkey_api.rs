//! WebAuthn passkey registration and login endpoints (Phase 6).
//!
//! - `POST /api/passkey/register/begin` (session required)
//! - `POST /api/passkey/register/finish` (session required)
//! - `GET/POST /api/passkey/login/begin` (public)
//! - `POST /api/passkey/login/finish` (public, returns session cookie on success)

use crate::auth::SessionAuth;
use aegis_passkey::PasskeyService;
use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct PasskeyState {
    pub service: Arc<PasskeyService>,
    pub auth: SessionAuth,
    pub sessions: Arc<Mutex<HashMap<String, String>>>,
}

pub fn router(
    service: Arc<PasskeyService>,
    auth: SessionAuth,
    sessions: Arc<Mutex<HashMap<String, String>>>,
) -> Router {
    Router::new()
        .route("/api/passkey/register/begin", post(register_begin))
        .route("/api/passkey/register/finish", post(register_finish))
        .route("/api/passkey/login/begin", get(login_begin).post(login_begin))
        .route("/api/passkey/login/finish", post(login_finish))
        .with_state(PasskeyState {
            service,
            auth,
            sessions,
        })
}

async fn register_begin(State(st): State<PasskeyState>, headers: HeaderMap) -> Response {
    let Some(subject) = st.auth.subject(&headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let options = st.service.begin_registration(&subject).await;
    Json(options).into_response()
}

async fn register_finish(
    State(st): State<PasskeyState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let Some(subject) = st.auth.subject(&headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match st.service.finish_registration(&subject, &body).await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn login_begin(State(st): State<PasskeyState>) -> Response {
    let options = st.service.begin_login().await;
    Json(options).into_response()
}

async fn login_finish(State(st): State<PasskeyState>, Json(body): Json<Value>) -> Response {
    match st.service.finish_login(&body).await {
        Ok(subject) => {
            let token = uuid::Uuid::new_v4().simple().to_string();
            st.sessions.lock().await.insert(token.clone(), subject.clone());
            let mut resp = Json(json!({"ok": true, "subject": subject})).into_response();
            let cookie = format!("aegis_session={token}; HttpOnly; Path=/; SameSite=Lax");
            if let Ok(value) = HeaderValue::from_str(&cookie) {
                resp.headers_mut().insert(header::SET_COOKIE, value);
            }
            resp
        }
        Err(e) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}
