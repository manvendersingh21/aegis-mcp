use aegis_core::*;
use aegis_gateway::{router, GatewayDeps};
use axum::{body::Body, http::{Request, StatusCode}};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

struct TestRedactor;
impl Redactor for TestRedactor {
    fn redact(&self, _: Resource, _: &Value) -> Redacted { Redacted { value: json!({"redacted":true}), redactions: 1 } }
}
struct Guard;
impl InjectionGuard for Guard {
    fn inspect(&self, _: &ToolSpec, _: &ToolCall) -> Option<Violation> { None }
}
struct Clean;
impl CleanRoom for Clean {
    fn evaluate(&self, _: Resource, _: &Value, _: &Value) -> Value { json!({"free_busy":true}) }
}
struct Signer;
impl ReceiptSigner for Signer {
    fn issue(&self, claims: ReceiptClaims) -> Receipt { Receipt { claims, signature:"ed25519:test".into(), revoked:false } }
    fn verify(&self, _: &Receipt) -> bool { true }
    fn public_key_hex(&self) -> String { "00".repeat(32) }
}
#[derive(Default)]
struct TestLedger { entries: Mutex<Vec<LedgerEntry>>, receipts: Mutex<Vec<Receipt>> }
impl Ledger for TestLedger {
    fn append(&self, entry: LedgerEntry) -> Result<LedgerEntry, AegisError> { self.entries.lock().unwrap().push(entry.clone()); Ok(entry) }
    fn recent(&self, subject: Option<&str>, limit: usize) -> Vec<LedgerEntry> {
        self.entries.lock().unwrap().iter().rev().filter(|e| subject.map_or(true, |s| s == e.subject)).take(limit).cloned().collect()
    }
    fn verify_chain(&self) -> bool { true }
    fn store_receipt(&self, receipt: &Receipt) -> Result<(), AegisError> { self.receipts.lock().unwrap().push(receipt.clone()); Ok(()) }
    fn get_receipt(&self, id: &str) -> Option<Receipt> { self.receipts.lock().unwrap().iter().find(|r| r.claims.receipt_id == id).cloned() }
    fn active_receipt(&self, subject: &str, agent: &str, resource: Resource, now: i64) -> Option<Receipt> {
        self.receipts.lock().unwrap().iter().rev().find(|r| r.claims.subject == subject && r.claims.grantee_agent == agent && r.claims.resource == resource && r.is_active(now)).cloned()
    }
    fn active_receipts(&self, subject: Option<&str>, now: i64) -> Vec<Receipt> {
        self.receipts.lock().unwrap().iter().filter(|r| r.is_active(now) && subject.map_or(true, |s| s == r.claims.subject)).cloned().collect()
    }
    fn revoke_all(&self, subject: Option<&str>) -> Vec<String> {
        let mut ids = Vec::new();
        for receipt in self.receipts.lock().unwrap().iter_mut() {
            if !receipt.revoked && subject.map_or(true, |s| s == receipt.claims.subject) { receipt.revoked = true; ids.push(receipt.claims.receipt_id.clone()); }
        }
        ids
    }
    fn metrics(&self, _: Option<&str>, _: i64) -> Metrics { Metrics::default() }
}
struct Risk(Option<RiskScores>);
impl RiskScorer for Risk {
    fn score<'a>(&'a self, _: &'a ToolCall) -> BoxFuture<'a, Option<RiskScores>> { Box::pin(async move { self.0.clone() }) }
}
#[derive(Default)]
struct Payments { requests: Mutex<Vec<PaymentRequest>> }
impl PaymentProvider for Payments {
    fn charge<'a>(&'a self, request: PaymentRequest) -> BoxFuture<'a, Result<PaymentReceipt, AegisError>> {
        Box::pin(async move {
            let receipt = PaymentReceipt { provider:"demo".into(), payment_id:format!("demo_pi_{}", self.requests.lock().unwrap().len()), status:"succeeded".into(), amount_cents:request.amount_cents, currency:request.currency.clone(), merchant:request.merchant.clone(), livemode:false };
            self.requests.lock().unwrap().push(request);
            Ok(receipt)
        })
    }
}

fn app(risk: Option<Arc<dyn RiskScorer>>, payments: Option<Arc<dyn PaymentProvider>>, ledger: Arc<TestLedger>) -> axum::Router {
    router(GatewayDeps {
        vaults:demo_vaults(), redactor:Arc::new(TestRedactor), guard:Arc::new(Guard), clean_room:Arc::new(Clean), judge:None,
        risk, risk_threshold:0.8, payments, signer:Arc::new(Signer), ledger, scenarios:None,
        base_url:"http://127.0.0.1:8787".into(), consent_timeout_secs:5, require_mcp_token:false,
        advocate:None, policy:None, verifier:None,
    })
}
async fn body_json(response: axum::response::Response) -> Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}
async fn rpc(app: &axum::Router, tool: &str, args: Value) -> Value {
    let request = Request::post("/mcp").header("content-type","application/json").header("x-aegis-user","maya")
        .header("x-aegis-agent","payment_bot").header("x-aegis-purpose","test purchase")
        .body(Body::from(json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":tool,"arguments":args}}).to_string())).unwrap();
    body_json(app.clone().oneshot(request).await.unwrap()).await
}
async fn login(app: &axum::Router) -> String {
    let response = app.clone().oneshot(Request::post("/api/login").header("content-type","application/json")
        .body(Body::from(json!({"subject":"maya","password":"maya-demo"}).to_string())).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.headers().get("set-cookie").unwrap().to_str().unwrap().split(';').next().unwrap().to_string()
}
async fn pending_id(app: &axum::Router, cookie: &str) -> Option<String> {
    let response = app.clone().oneshot(Request::get("/api/state?subject=maya").header("cookie",cookie).body(Body::empty()).unwrap()).await.unwrap();
    let state = body_json(response).await;
    state["pending"].as_array()?.first()?["request_id"].as_str().map(str::to_string)
}
async fn approve(app: &axum::Router, cookie: &str, id: &str) {
    let response = app.clone().oneshot(Request::post(format!("/api/consent/{id}")).header("content-type","application/json").header("cookie",cookie)
        .body(Body::from(json!({"approve":true,"mode":"REDACT","ttl_secs":600}).to_string())).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn high_jev_score_blocks_without_consent() {
    let ledger = Arc::new(TestLedger::default());
    let app = app(Some(Arc::new(Risk(Some(RiskScores { provider:"jev".into(), model:"jev-1.14.0".into(), prompt_injection:0.98, data_exfiltration:0.1 })))), None, ledger);
    let response = rpc(&app, "check_schedule", json!({})).await;
    assert_eq!(response["result"]["isError"], true, "{response}");
    assert!(response.to_string().contains("Jev promptInjection"));
    let cookie = login(&app).await;
    assert_eq!(pending_id(&app, &cookie).await, None, "blocked call must not create a consent prompt");
}

#[tokio::test]
async fn missing_jev_score_escalates_redacted_schedule_to_consent() {
    let app = app(Some(Arc::new(Risk(None))), None, Arc::new(TestLedger::default()));
    let cookie = login(&app).await;
    let app_call = app.clone();
    let call = tokio::spawn(async move { rpc(&app_call, "check_schedule", json!({})).await });
    let id = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop { if let Some(id) = pending_id(&app, &cookie).await { break id; } tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
    }).await.expect("a missing risk score requires fresh consent");
    approve(&app, &cookie, &id).await;
    let response = call.await.unwrap();
    assert_eq!(response["result"]["structuredContent"], json!({"redacted":true}));
}

#[tokio::test]
async fn payment_cap_is_a_policy_violation() {
    let app = app(None, Some(Arc::new(Payments::default())), Arc::new(TestLedger::default()));
    let response = rpc(&app, "make_payment", json!({"amount_usd":3000.01,"merchant":"Demo Store"})).await;
    assert_eq!(response["result"]["isError"], true, "{response}");
    assert!(response.to_string().contains("exceeds per-person cap"));
}

#[tokio::test]
async fn approved_payment_returns_only_reference_and_never_uses_a_lease() {
    let ledger = Arc::new(TestLedger::default());
    let payments = Arc::new(Payments::default());
    let app = app(None, Some(payments.clone()), ledger.clone());
    let cookie = login(&app).await;
    let app_call = app.clone();
    let call = tokio::spawn(async move { rpc(&app_call, "make_payment", json!({"amount_usd":42.50,"merchant":"Demo Store","description":"Order 123"})).await });
    let id = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop { if let Some(id) = pending_id(&app, &cookie).await { break id; } tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
    }).await.expect("payment must ask for consent");
    approve(&app, &cookie, &id).await;
    let response = call.await.unwrap();
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["payment_id"], "demo_pi_0");
    assert_eq!(content["amount_cents"], 4250);
    assert_eq!(content["card"], "[TOKENIZED BY AEGIS]");
    assert!(!content.to_string().contains("account_number"));
    assert!(!content.to_string().contains("123456789"));
    assert!(ledger.receipts.lock().unwrap().is_empty(), "payment consent must not become a lease");

    let app_call = app.clone();
    let second = tokio::spawn(async move { rpc(&app_call, "make_payment", json!({"amount_usd":42.50,"merchant":"Demo Store"})).await });
    let id2 = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop { if let Some(id) = pending_id(&app, &cookie).await { break id; } tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
    }).await.expect("a previous payment cannot authorize this payment");
    assert_eq!(payments.requests.lock().unwrap().len(), 1, "second charge waits for its own approval");
    approve(&app, &cookie, &id2).await;
    let _ = second.await.unwrap();
    assert_eq!(payments.requests.lock().unwrap().len(), 2);
}
