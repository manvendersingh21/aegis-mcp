//! Agent credential resolution (PHASE6.md §3).
//!
//! `POST /admin/agent-credentials/resolve {"credential"}` (admin bearer token)
//! returns the `AgentIdentity` behind a valid `provider:"mcp"` fake key, and
//! records the use. `mcp` keys need no stored provider secret. Anything else
//! (unknown, revoked, expired, or a non-mcp key) is a 404.
use crate::{admin, hash, internal_error, unauthorized, BrokerState, UsageRow};
use aegis_core::{now_unix, AgentIdentity};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

pub fn resolve_router(state: BrokerState) -> Router {
    Router::new()
        .route("/admin/agent-credentials/resolve", post(resolve_credential))
        .with_state(state)
}

#[derive(Deserialize)]
struct ResolveBody {
    credential: String,
}

async fn resolve_credential(
    State(s): State<BrokerState>,
    headers: HeaderMap,
    Json(body): Json<ResolveBody>,
) -> Response {
    if !admin(&s, &headers) {
        return unauthorized();
    }
    let credential = body.credential.trim();
    let credential = credential.strip_prefix("Bearer ").unwrap_or(credential);
    let key_hash = hash(credential);
    let mut row = match s.store.fake_key_by_hash(&key_hash).await {
        Ok(Some(r)) => r,
        Ok(None) => return not_found(),
        Err(_) => return internal_error(),
    };
    if row.info.provider != "mcp" {
        return not_found();
    }
    let now = now_unix();
    let valid = !row.info.revoked && row.info.expires_at > now;
    let usage = UsageRow {
        fake_key_hash: key_hash.clone(),
        subject: row.info.subject.clone(),
        agent_id: row.info.agent_id.clone(),
        provider: "mcp".into(),
        path: "resolve".into(),
        ok: valid,
        ts: now,
    };
    if !valid {
        let _ = s.store.log_usage(usage).await;
        return not_found();
    }
    row.info.last_used_at = Some(now);
    row.info.uses = row.info.uses.saturating_add(1);
    let identity = AgentIdentity {
        subject: row.info.subject.clone(),
        agent_id: row.info.agent_id.clone(),
        credential_prefix: row.info.prefix.clone(),
    };
    let _ = s.store.update_fake_key(row).await;
    let _ = s.store.log_usage(usage).await;
    Json(identity).into_response()
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error":"unknown agent credential"})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{router, BrokerStore, MemoryStore, Sealer};
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use std::sync::Arc;
    use tower::ServiceExt;

    const TOKEN: &str = "admin-token";

    fn setup() -> (BrokerState, Arc<MemoryStore>) {
        let store = Arc::new(MemoryStore::new());
        let state = BrokerState::new(store.clone(), Sealer::new([7u8; 32]), TOKEN);
        (state, store)
    }

    async fn call(
        state: &BrokerState,
        auth: Option<&str>,
        credential: &str,
    ) -> (StatusCode, serde_json::Value) {
        let mut req = Request::post("/admin/agent-credentials/resolve")
            .header("content-type", "application/json");
        if let Some(a) = auth {
            req = req.header("authorization", format!("Bearer {a}"));
        }
        let req = req
            .body(Body::from(json!({"credential": credential}).to_string()))
            .unwrap();
        let resp = router(state.clone()).oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    #[tokio::test]
    async fn resolves_valid_mcp_key_and_records_use() {
        let (state, store) = setup();
        let issued = state
            .issue_fake_key("alice", "claude-code", "mcp", 3600)
            .await
            .unwrap();
        let (status, body) = call(&state, Some(TOKEN), &issued.fake_key).await;
        assert_eq!(status, StatusCode::OK);
        let id: AgentIdentity = serde_json::from_value(body).unwrap();
        assert_eq!(id.subject, "alice");
        assert_eq!(id.agent_id, "claude-code");
        assert_eq!(id.credential_prefix, issued.info.prefix);
        let keys = store.fake_keys("alice").await.unwrap();
        assert_eq!(keys[0].info.uses, 1);
        assert!(keys[0].info.last_used_at.is_some());
        let rows = store.usage_rows().await;
        assert_eq!(rows.len(), 1);
        assert!(rows[0].ok);
        assert_eq!(rows[0].provider, "mcp");
    }

    #[tokio::test]
    async fn requires_admin_token() {
        let (state, _) = setup();
        let issued = state
            .issue_fake_key("alice", "a", "mcp", 3600)
            .await
            .unwrap();
        assert_eq!(
            call(&state, None, &issued.fake_key).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&state, Some("wrong"), &issued.fake_key).await.0,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn rejects_unknown_revoked_expired_and_non_mcp() {
        let (state, store) = setup();
        assert_eq!(
            call(&state, Some(TOKEN), "aegis_fk_mcp_nope").await.0,
            StatusCode::NOT_FOUND
        );

        let llm = state
            .issue_fake_key("alice", "a", "llm", 3600)
            .await
            .unwrap();
        assert_eq!(
            call(&state, Some(TOKEN), &llm.fake_key).await.0,
            StatusCode::NOT_FOUND
        );

        let expired = state.issue_fake_key("bob", "a", "mcp", 0).await.unwrap();
        assert_eq!(
            call(&state, Some(TOKEN), &expired.fake_key).await.0,
            StatusCode::NOT_FOUND
        );

        let revoked = state
            .issue_fake_key("carol", "a", "mcp", 3600)
            .await
            .unwrap();
        let mut row = store.fake_keys("carol").await.unwrap().remove(0);
        row.info.revoked = true;
        store.update_fake_key(row).await.unwrap();
        assert_eq!(
            call(&state, Some(TOKEN), &revoked.fake_key).await.0,
            StatusCode::NOT_FOUND
        );
    }
}
