use super::*;
use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderMap, Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};
use tokio::sync::{oneshot, Mutex};
use tower::ServiceExt;

struct TestRedactor;
impl Redactor for TestRedactor {
    fn redact(&self, _: Resource, _: &Value) -> Redacted {
        Redacted { value: json!({"redacted": true}), redactions: 1 }
    }
}

struct TestGuard;
impl InjectionGuard for TestGuard {
    fn inspect(&self, _: &ToolSpec, _: &ToolCall) -> Option<Violation> { None }
}

struct TestCleanRoom;
impl CleanRoom for TestCleanRoom {
    fn evaluate(&self, _: Resource, _: &Value, _: &Value) -> Value { json!({"clean_room": true}) }
}

struct TestSigner;
impl ReceiptSigner for TestSigner {
    fn issue(&self, claims: ReceiptClaims) -> Receipt {
        Receipt { claims, signature: "ed25519:test".into(), revoked: false }
    }
    fn verify(&self, _: &Receipt) -> bool { true }
    fn public_key_hex(&self) -> String { "00".repeat(32) }
}

#[derive(Default)]
struct TestLedger {
    entries: StdMutex<Vec<LedgerEntry>>,
    receipts: StdMutex<Vec<Receipt>>,
}

impl Ledger for TestLedger {
    fn append(&self, entry: LedgerEntry) -> Result<LedgerEntry, AegisError> {
        self.entries.lock().unwrap().push(entry.clone());
        Ok(entry)
    }

    fn recent(&self, subject: Option<&str>, limit: usize) -> Vec<LedgerEntry> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .rev()
            .filter(|entry| subject.map_or(true, |s| s == entry.subject))
            .take(limit)
            .cloned()
            .collect()
    }

    fn verify_chain(&self) -> bool { true }

    fn store_receipt(&self, receipt: &Receipt) -> Result<(), AegisError> {
        self.receipts.lock().unwrap().push(receipt.clone());
        Ok(())
    }

    fn get_receipt(&self, id: &str) -> Option<Receipt> {
        self.receipts.lock().unwrap().iter().find(|receipt| receipt.claims.receipt_id == id).cloned()
    }

    fn active_receipt(&self, subject: &str, agent_id: &str, resource: Resource, now: i64) -> Option<Receipt> {
        self.receipts
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|receipt| {
                receipt.claims.subject == subject
                    && receipt.claims.grantee_agent == agent_id
                    && receipt.claims.resource == resource
                    && receipt.is_active(now)
            })
            .cloned()
    }

    fn active_receipts(&self, subject: Option<&str>, now: i64) -> Vec<Receipt> {
        self.receipts
            .lock()
            .unwrap()
            .iter()
            .filter(|receipt| receipt.is_active(now) && subject.map_or(true, |s| s == receipt.claims.subject))
            .cloned()
            .collect()
    }

    fn revoke_all(&self, subject: Option<&str>) -> Vec<String> {
        let mut ids = Vec::new();
        for receipt in self.receipts.lock().unwrap().iter_mut() {
            if !receipt.revoked && subject.map_or(true, |s| s == receipt.claims.subject) {
                receipt.revoked = true;
                ids.push(receipt.claims.receipt_id.clone());
            }
        }
        ids
    }

    fn metrics(&self, _: Option<&str>, _: i64) -> Metrics { Metrics::default() }
}

#[derive(Default)]
struct TestPayments {
    requests: StdMutex<Vec<PaymentRequest>>,
}

impl PaymentProvider for TestPayments {
    fn charge<'a>(&'a self, request: PaymentRequest) -> BoxFuture<'a, Result<PaymentReceipt, AegisError>> {
        Box::pin(async move {
            let receipt = PaymentReceipt {
                provider: "demo".into(),
                payment_id: format!("demo_pi_{}", self.requests.lock().unwrap().len()),
                status: "succeeded".into(),
                amount_cents: request.amount_cents,
                currency: request.currency.clone(),
                merchant: request.merchant.clone(),
                livemode: false,
            };
            self.requests.lock().unwrap().push(request);
            Ok(receipt)
        })
    }
}

fn build_router(ledger: Arc<TestLedger>, payments: Option<Arc<dyn PaymentProvider>>) -> axum::Router {
    router(GatewayDeps {
        vaults: demo_vaults(),
        redactor: Arc::new(TestRedactor),
        guard: Arc::new(TestGuard),
        clean_room: Arc::new(TestCleanRoom),
        judge: None,
        risk: None,
        risk_threshold: 0.8,
        payments,
        signer: Arc::new(TestSigner),
        ledger,
        scenarios: None,
        base_url: "http://127.0.0.1:8787".into(),
        consent_timeout_secs: 2,
        require_mcp_token: false,
        advocate: None,
        policy: None,
        verifier: None,
    })
}

fn build_state(ledger: Arc<TestLedger>) -> Arc<AppState> {
    let (events, _) = tokio::sync::broadcast::channel(32);
    Arc::new(AppState {
        deps: GatewayDeps {
            vaults: demo_vaults(),
            redactor: Arc::new(TestRedactor),
            guard: Arc::new(TestGuard),
            clean_room: Arc::new(TestCleanRoom),
            judge: None,
            risk: None,
            risk_threshold: 0.8,
            payments: None,
            signer: Arc::new(TestSigner),
            ledger,
            scenarios: None,
            base_url: "http://127.0.0.1:8787".into(),
            consent_timeout_secs: 2,
            require_mcp_token: false,
            advocate: None,
            policy: None,
            verifier: None,
        },
        events,
        pending: Mutex::new(HashMap::new()),
        lease_tools: Mutex::new(HashMap::new()),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        wallet: None,
    })
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn rpc(app: &axum::Router, tool: &str, arguments: Value) -> Value {
    let request = Request::post("/mcp")
        .header("content-type", "application/json")
        .header("x-aegis-user", "maya")
        .header("x-aegis-agent", "review_bot")
        .header("x-aegis-purpose", "regression test")
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": {"name": tool, "arguments": arguments}
            })
            .to_string(),
        ))
        .unwrap();
    response_json(app.clone().oneshot(request).await.unwrap()).await
}

async fn login_cookie(app: &axum::Router, subject: &str) -> String {
    let request = Request::post("/api/login")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"subject": subject, "password": format!("{subject}-demo")}).to_string(),
        ))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

async fn state_json(app: &axum::Router, cookie: &str, subject: &str) -> Value {
    let request = Request::get(format!("/api/state?subject={subject}"))
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap();
    response_json(app.clone().oneshot(request).await.unwrap()).await
}

async fn wait_pending_id(app: &axum::Router, cookie: &str, subject: &str) -> String {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let state = state_json(app, cookie, subject).await;
            if let Some(id) = state["pending"].as_array().and_then(|pending| pending.first()).and_then(|first| first["request_id"].as_str()) {
                break id.to_string();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("missing consent prompt")
}

async fn decide(app: &axum::Router, cookie: &str, request_id: &str, body: Value) {
    let request = Request::post(format!("/api/consent/{request_id}"))
        .header("content-type", "application/json")
        .header("cookie", cookie)
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn make_payment_never_releases_account_or_routing_numbers() {
    for mode in ["ALLOW", "CLEAN_ROOM", "REDACT"] {
        let ledger = Arc::new(TestLedger::default());
        let app = build_router(ledger, Some(Arc::new(TestPayments::default())));
        let cookie = login_cookie(&app, "maya").await;

        let app_call = app.clone();
        let call = tokio::spawn(async move {
            rpc(
                &app_call,
                "make_payment",
                json!({"amount_usd": 42.50, "merchant": "Demo Store", "description": "order #123"}),
            )
            .await
        });

        let request_id = wait_pending_id(&app, &cookie, "maya").await;
        decide(
            &app,
            &cookie,
            &request_id,
            json!({"approve": true, "mode": mode, "ttl_secs": 60}),
        )
        .await;
        let response = call.await.unwrap();
        let released = response["result"]["structuredContent"].to_string();
        assert!(!released.contains("account_number"), "{response}");
        assert!(!released.contains("routing_number"), "{response}");
        assert!(!released.contains("123456789"), "{response}");
        assert!(!released.contains("021000021"), "{response}");
    }
}

#[tokio::test]
async fn approving_make_payment_never_creates_a_financials_lease() {
    let ledger = Arc::new(TestLedger::default());
    let app = build_router(ledger.clone(), Some(Arc::new(TestPayments::default())));
    let cookie = login_cookie(&app, "maya").await;

    let app_call = app.clone();
    let payment = tokio::spawn(async move {
        rpc(
            &app_call,
            "make_payment",
            json!({"amount_usd": 15.00, "merchant": "Lease Probe"}),
        )
        .await
    });
    let request_id = wait_pending_id(&app, &cookie, "maya").await;
    decide(&app, &cookie, &request_id, json!({"approve": true, "mode": "ALLOW", "ttl_secs": 600})).await;
    let payment_response = payment.await.unwrap();
    assert_eq!(payment_response["result"]["isError"], Value::Null, "{payment_response}");
    assert!(ledger.receipts.lock().unwrap().is_empty(), "payment approval must not store a lease receipt");

    let app_call = app.clone();
    let financials = tokio::spawn(async move {
        rpc(&app_call, "query_financials", json!({"fields": ["checking_balance"]})).await
    });
    let second_prompt = wait_pending_id(&app, &cookie, "maya").await;
    decide(&app, &cookie, &second_prompt, json!({"approve": false})).await;
    let denied = financials.await.unwrap();
    assert_eq!(denied["result"]["isError"], true, "{denied}");
}

#[tokio::test]
async fn lease_for_checking_balance_cannot_serve_ssn_without_new_prompt() {
    let app = build_router(Arc::new(TestLedger::default()), None);
    let cookie = login_cookie(&app, "maya").await;

    let app_call = app.clone();
    let first = tokio::spawn(async move {
        rpc(&app_call, "query_financials", json!({"fields": ["checking_balance"]})).await
    });
    let first_request = wait_pending_id(&app, &cookie, "maya").await;
    decide(
        &app,
        &cookie,
        &first_request,
        json!({"approve": true, "mode": "CLEAN_ROOM", "ttl_secs": 600}),
    )
    .await;
    let first_response = first.await.unwrap();
    assert_eq!(first_response["result"]["structuredContent"], json!({"clean_room": true}), "{first_response}");

    let app_call = app.clone();
    let second = tokio::spawn(async move {
        rpc(&app_call, "query_financials", json!({"fields": ["ssn"]})).await
    });
    let second_request = wait_pending_id(&app, &cookie, "maya").await;
    assert_ne!(first_request, second_request, "new field set must trigger a fresh consent request");
    decide(&app, &cookie, &second_request, json!({"approve": false})).await;
    let denied = second.await.unwrap();
    assert_eq!(denied["result"]["isError"], true, "{denied}");
}

#[tokio::test]
async fn allow_identity_releases_only_approved_full_name_field() {
    let app = build_router(Arc::new(TestLedger::default()), None);
    let cookie = login_cookie(&app, "maya").await;

    let app_call = app.clone();
    let call = tokio::spawn(async move {
        rpc(&app_call, "fetch_identity", json!({"fields": ["full_name"]})).await
    });
    let request_id = wait_pending_id(&app, &cookie, "maya").await;
    decide(&app, &cookie, &request_id, json!({"approve": true, "mode": "ALLOW", "ttl_secs": 60})).await;
    let response = call.await.unwrap();
    let released = response["result"]["structuredContent"].as_object().cloned().expect("identity object");

    let full_name = Vault::demo_for("maya")
        .expect("demo vault")
        .resource(Resource::Identity)
        .get("full_name")
        .cloned()
        .expect("full_name");
    assert_eq!(released.len(), 1, "ALLOW must only release the approved field");
    assert_eq!(released.get("full_name"), Some(&full_name), "{response}");
}

#[tokio::test]
async fn clean_room_meta_mode_is_screaming_snake_case() {
    let app = build_router(Arc::new(TestLedger::default()), None);
    let cookie = login_cookie(&app, "maya").await;

    let app_call = app.clone();
    let call = tokio::spawn(async move {
        rpc(
            &app_call,
            "query_financials",
            json!({"fields": ["checking_balance"], "min_amount": 100.0}),
        )
        .await
    });
    let request_id = wait_pending_id(&app, &cookie, "maya").await;
    decide(
        &app,
        &cookie,
        &request_id,
        json!({"approve": true, "mode": "CLEAN_ROOM", "ttl_secs": 60}),
    )
    .await;
    let response = call.await.unwrap();
    assert_eq!(response["result"]["_meta"]["aegis"]["mode"], "CLEAN_ROOM", "{response}");
}

#[tokio::test]
async fn expired_or_dropped_waiters_are_hidden_from_api_state_pending() {
    let state = build_state(Arc::new(TestLedger::default()));
    state.sessions.lock().await.insert("sess1".into(), "maya".into());
    let request = ConsentRequest {
        request_id: "consent_expired".into(),
        call: ToolCall {
            call_id: "call_expired".into(),
            subject: "maya".into(),
            agent_id: "review_bot".into(),
            tool: "query_financials".into(),
            purpose: "expired request".into(),
            arguments: json!({}),
            ts: now_unix(),
        },
        resource: Resource::Financials,
        requested_fields: vec!["checking_balance".into()],
        sensitive_fields: vec!["checking_balance".into()],
        suggested_mode: Mode::CleanRoom,
        created_at: now_unix().saturating_sub(120),
        deadline: now_unix().saturating_sub(1),
    };
    let (sender, _receiver) = oneshot::channel();
    state.pending.lock().await.insert(request.request_id.clone(), (request, sender));

    let mut headers = HeaderMap::new();
    headers.insert(header::COOKIE, HeaderValue::from_static("aegis_session=sess1"));
    let response = api_state(
        State(state),
        headers,
        Query(SubjectQuery { subject: Some("maya".into()) }),
    )
    .await;
    let snapshot = response_json(response).await;
    assert!(
        snapshot["pending"].as_array().is_some_and(Vec::is_empty),
        "expired pending requests must not be returned: {snapshot}"
    );
}

#[tokio::test]
async fn oversized_mcp_request_body_is_rejected() {
    let app = build_router(Arc::new(TestLedger::default()), None);
    let padding = "x".repeat(10 * 1024 * 1024);
    let body = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\",\"params\":{{\"padding\":\"{padding}\"}}}}"
    );
    let request = Request::post("/mcp")
        .header("content-type", "application/json")
        .header("x-aegis-user", "maya")
        .body(Body::from(body))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert!(
        response.status() == StatusCode::BAD_REQUEST || response.status() == StatusCode::PAYLOAD_TOO_LARGE,
        "oversized body should be rejected, got status {}",
        response.status()
    );
}
