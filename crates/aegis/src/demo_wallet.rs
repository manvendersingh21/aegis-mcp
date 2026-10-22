//! Labelled demo wallets so the stage demo never depends on a browser
//! extension. The dashboard fetches the person's demo address, builds the same
//! SIWE message a real wallet would sign, asks the server to sign it, then logs
//! in through the normal `POST /api/login/siwe`.

use aegis_chain::{DemoWallets, SiweAuth};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Clone)]
struct DemoState {
    wallets: Arc<DemoWallets>,
    domain: String,
}

/// Demo wallets are on unless `AEGIS_DEMO_WALLETS=0` (hackathon default).
pub fn enabled() -> bool {
    std::env::var("AEGIS_DEMO_WALLETS").as_deref() != Ok("0")
}

/// Generates one demo wallet per subject and allows it to log that person in.
/// The server only signs SIWE messages for its own `domain`.
pub fn router(siwe: &SiweAuth, subjects: &[String], domain: String) -> Router {
    let wallets = DemoWallets::generate(subjects);
    for subject in subjects {
        if let Some(address) = wallets.address(subject) {
            siwe.allow_demo_login(subject, &address);
        }
    }
    Router::new()
        .route("/api/demo-wallet/{subject}", get(address))
        .route("/api/demo-wallet/sign", post(sign))
        .with_state(DemoState { wallets: Arc::new(wallets), domain })
}

async fn address(State(st): State<DemoState>, Path(subject): Path<String>) -> Response {
    match st.wallets.address(&subject) {
        Some(address) => Json(json!({"address": address, "demo": true})).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Deserialize)]
struct SignBody {
    subject: String,
    message: String,
}

async fn sign(State(st): State<DemoState>, Json(body): Json<SignBody>) -> Response {
    let first = body.message.lines().next().unwrap_or_default();
    if first != format!("{} wants you to sign in with your Ethereum account:", st.domain) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "demo wallet only signs sign-in messages for this server"}))).into_response();
    }
    match st.wallets.sign(&body.subject, &body.message) {
        Ok(signature) => Json(json!({"signature": signature, "demo": true})).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))).into_response(),
    }
}
