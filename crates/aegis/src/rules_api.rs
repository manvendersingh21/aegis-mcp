//! `/api/rules` — the person's standing consent rules (session-scoped).
//!
//! - `GET /api/rules?subject=X` → `{"rules": [PolicyRule…]}`
//! - `POST /api/rules?subject=X` body = rule fields (server fills
//!   `rule_id`, `subject`, `created_at`) → `{"ok":true,"rule":PolicyRule}`
//! - `DELETE /api/rules/{id}?subject=X` → `{"ok":true}` / 404
//!
//! A session can only touch its own person's rules; `?subject=` defaults to
//! the session's subject.

use crate::auth::SessionAuth;
use aegis_core::{new_id, now_unix, Mode, PolicyEngine, PolicyRule, Resource, RuleDecision};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct RulesState {
    pub policy: Arc<dyn PolicyEngine>,
    pub auth: SessionAuth,
}

pub fn router(policy: Arc<dyn PolicyEngine>, auth: SessionAuth) -> Router {
    Router::new()
        .route("/api/rules", get(list).post(add))
        .route("/api/rules/{id}", axum::routing::delete(remove))
        .with_state(RulesState { policy, auth })
}

/// The `?subject=` query param (defaults to the session's own subject).
async fn own_subject(st: &RulesState, headers: &HeaderMap, query: &HashMap<String, String>) -> Result<String, Response> {
    let Some(session) = st.auth.subject(headers).await else {
        return Err(StatusCode::UNAUTHORIZED.into_response());
    };
    match query.get("subject").map(String::as_str) {
        None | Some("") => Ok(session),
        Some(requested) if requested == session => Ok(session),
        Some(_) => Err((StatusCode::FORBIDDEN, Json(json!({"error": "cannot read another person's rules"}))).into_response()),
    }
}

async fn list(State(st): State<RulesState>, headers: HeaderMap, Query(query): Query<HashMap<String, String>>) -> Response {
    match own_subject(&st, &headers, &query).await {
        Ok(subject) => Json(json!({"rules": st.policy.rules(&subject)})).into_response(),
        Err(response) => response,
    }
}

#[derive(Deserialize)]
struct RuleInput {
    tool: Option<String>,
    resource: Option<Resource>,
    agent_id: Option<String>,
    fields_within: Option<Vec<String>>,
    max_amount_cents: Option<i64>,
    decision: RuleDecision,
    label: String,
}

async fn add(State(st): State<RulesState>, headers: HeaderMap, Query(query): Query<HashMap<String, String>>, Json(body): Json<Value>) -> Response {
    let subject = match own_subject(&st, &headers, &query).await {
        Ok(subject) => subject,
        Err(response) => return response,
    };
    let input: RuleInput = match serde_json::from_value(body) {
        Ok(input) => input,
        Err(error) => return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("invalid rule: {error}")}))).into_response(),
    };
    // Payments may only auto-approve through the clean room (never raw).
    let decision = match (&input.decision, input.tool.as_deref() == Some("make_payment")) {
        (RuleDecision::Allow { mode: Mode::Allow }, true) | (RuleDecision::Allow { mode: Mode::Redact }, true) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "payment rules may only allow CLEAN_ROOM or deny"}))).into_response();
        }
        _ => input.decision.clone(),
    };
    let rule = PolicyRule {
        rule_id: new_id("rule"),
        subject,
        tool: input.tool,
        resource: input.resource,
        agent_id: input.agent_id,
        fields_within: input.fields_within,
        max_amount_cents: input.max_amount_cents,
        decision,
        label: input.label,
        created_at: now_unix(),
    };
    match st.policy.add(rule) {
        Ok(rule) => Json(json!({"ok": true, "rule": rule})).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("{error}")}))).into_response(),
    }
}

async fn remove(State(st): State<RulesState>, Path(id): Path<String>, headers: HeaderMap, Query(query): Query<HashMap<String, String>>) -> Response {
    let subject = match own_subject(&st, &headers, &query).await {
        Ok(subject) => subject,
        Err(response) => return response,
    };
    if st.policy.remove(&subject, &id) {
        Json(json!({"ok": true})).into_response()
    } else {
        (StatusCode::NOT_FOUND, Json(json!({"error": "no such rule"}))).into_response()
    }
}
