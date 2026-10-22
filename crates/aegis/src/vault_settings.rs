//! Vault security settings: protected delivery address & daily spending limit.
//!
//! - `delivery_address` can ONLY be viewed/modified by an authenticated person
//!   logged into their Aegis dashboard. External agents & ChatGPT are NEVER given
//!   write access or raw street addresses.
//! - `daily_limit_cents`: Hard spending cap enforced across all agent orders.

use crate::auth::SessionAuth;
use aegis_core::{demo_vaults, now_unix, Ledger};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubjectSettings {
    pub home_address: String,
    pub daily_limit_cents: i64,
}

#[derive(Clone)]
pub struct VaultSettingsState {
    pub settings: Arc<Mutex<HashMap<String, SubjectSettings>>>,
    pub ledger: Arc<dyn Ledger>,
    pub auth: SessionAuth,
}

impl VaultSettingsState {
    pub fn new(ledger: Arc<dyn Ledger>, auth: SessionAuth) -> Self {
        let mut map = HashMap::new();
        for v in demo_vaults() {
            let addr = v.0["identity"]["home_address"]
                .as_str()
                .unwrap_or("5521 Shafter Ave, Oakland, CA 94618")
                .to_string();
            let limit = v.max_payment_cents(); // e.g. 25000 = $250.00
            map.insert(
                v.subject(),
                SubjectSettings {
                    home_address: addr,
                    daily_limit_cents: if limit > 0 { limit } else { 25000 },
                },
            );
        }
        Self {
            settings: Arc::new(Mutex::new(map)),
            ledger,
            auth,
        }
    }

    /// Calculate total amount spent on `make_payment` today (UTC midnight).
    pub fn spent_today_cents(&self, subject: &str) -> i64 {
        let now = now_unix();
        let midnight = now - (now % 86400);
        let entries = self.ledger.recent(Some(subject), 100);
        let mut total = 0i64;
        for e in entries {
            if e.tool == "make_payment" && e.outcome == aegis_core::Outcome::Released && e.ts >= midnight {
                if let Some(cents) = e.payload_returned.get("amount_cents").and_then(Value::as_i64) {
                    total += cents;
                } else if let Some(usd) = e.payload_returned.get("amount_usd").and_then(Value::as_f64) {
                    total += (usd * 100.0).round() as i64;
                }
            }
        }
        total
    }
}

pub fn router(state: VaultSettingsState) -> Router {
    Router::new()
        .route("/api/vault/settings", get(get_settings).post(update_settings))
        .with_state(state)
}

async fn get_settings(
    State(st): State<VaultSettingsState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(session_subject) = st.auth.subject(&headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let target = query.get("subject").map(String::as_str).unwrap_or(&session_subject);
    if target != session_subject {
        return StatusCode::FORBIDDEN.into_response();
    }

    let map = st.settings.lock().await;
    let s = map.get(target).cloned().unwrap_or(SubjectSettings {
        home_address: "5521 Shafter Ave, Oakland, CA 94618".into(),
        daily_limit_cents: 25000,
    });
    let spent_today = st.spent_today_cents(target);

    Json(json!({
        "subject": target,
        "home_address": s.home_address,
        "daily_limit_cents": s.daily_limit_cents,
        "daily_limit_usd": s.daily_limit_cents as f64 / 100.0,
        "spent_today_cents": spent_today,
        "spent_today_usd": spent_today as f64 / 100.0,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct UpdateSettingsBody {
    home_address: Option<String>,
    daily_limit_cents: Option<i64>,
}

async fn update_settings(
    State(st): State<VaultSettingsState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<UpdateSettingsBody>,
) -> Response {
    let Some(session_subject) = st.auth.subject(&headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let target = query.get("subject").map(String::as_str).unwrap_or(&session_subject);
    if target != session_subject {
        return StatusCode::FORBIDDEN.into_response();
    }

    let mut map = st.settings.lock().await;
    let entry = map.entry(target.to_string()).or_insert(SubjectSettings {
        home_address: "5521 Shafter Ave, Oakland, CA 94618".into(),
        daily_limit_cents: 25000,
    });

    if let Some(addr) = body.home_address {
        if !addr.trim().is_empty() {
            entry.home_address = addr.trim().to_string();
        }
    }
    if let Some(limit) = body.daily_limit_cents {
        if limit > 0 {
            entry.daily_limit_cents = limit;
        }
    }

    Json(json!({
        "ok": true,
        "home_address": entry.home_address,
        "daily_limit_cents": entry.daily_limit_cents,
        "daily_limit_usd": entry.daily_limit_cents as f64 / 100.0,
    }))
    .into_response()
}
