//! HTTP gateway for the Aegis consent firewall.

use aegis_core::*;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    response::{sse::{Event as SseEvent, KeepAlive, Sse}, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashMap, convert::Infallible, sync::Arc, time::Duration};
use tokio::sync::{broadcast, oneshot, Mutex};
use tokio_stream::wrappers::BroadcastStream;
use tower_http::cors::CorsLayer;

/// Dependencies injected by the application binary.
pub struct GatewayDeps {
    pub vaults: Vec<Vault>,
    pub redactor: Arc<dyn Redactor>,
    pub guard: Arc<dyn InjectionGuard>,
    pub clean_room: Arc<dyn CleanRoom>,
    pub judge: Option<Arc<dyn PurposeJudge>>,
    pub risk: Option<Arc<dyn RiskScorer>>,
    pub risk_threshold: f64,
    pub payments: Option<Arc<dyn PaymentProvider>>,
    pub signer: Arc<dyn ReceiptSigner>,
    pub ledger: Arc<dyn Ledger>,
    pub scenarios: Option<Arc<dyn ScenarioRunner>>,
    pub base_url: String,
    pub consent_timeout_secs: u64,
    pub require_mcp_token: bool,
    /// The person's advocate: advises on pending consent requests (Phase 6).
    pub advocate: Option<Arc<dyn Advocate>>,
    /// Standing rules checked before any consent prompt (Phase 6).
    pub policy: Option<Arc<dyn PolicyEngine>>,
    /// Resolves `aegis_fk_mcp_…` agent credentials to a verified identity.
    pub verifier: Option<Arc<dyn AgentVerifier>>,
}

struct AppState {
    deps: GatewayDeps,
    events: broadcast::Sender<EventEnvelope>,
    pending: Mutex<HashMap<String, (ConsentRequest, oneshot::Sender<ConsentDecision>)>>,
    /// Receipt ids are associated with the tool that caused them. ReceiptClaims
    /// identify resources, so the gateway keeps the additional tool scope here.
    lease_tools: Mutex<HashMap<String, String>>,
    sessions: Arc<Mutex<HashMap<String, String>>>,
    wallet: Option<Arc<dyn WalletAuth>>,
}

struct PendingDropGuard { state: Arc<AppState>, request_id: String }

impl Drop for PendingDropGuard {
    fn drop(&mut self) {
        let state = self.state.clone();
        let request_id = self.request_id.clone();
        tokio::spawn(async move { state.pending.lock().await.remove(&request_id); });
    }
}

pub fn router(deps: GatewayDeps) -> Router {
    router_with_wallet(deps, None)
}

/// Like [`router`], plus Sign-In-With-Ethereum routes when `wallet` is set:
/// `GET /api/siwe/nonce`, `POST /api/login/siwe`, `POST /api/wallet/link`.
pub fn router_with_wallet(deps: GatewayDeps, wallet: Option<Arc<dyn WalletAuth>>) -> Router {
    router_with_wallet_and_sessions(deps, wallet, Arc::new(Mutex::new(HashMap::new())))
}

/// Like [`router_with_wallet`], but uses a shared session map (e.g. for passkeys).
pub fn router_with_wallet_and_sessions(
    deps: GatewayDeps,
    wallet: Option<Arc<dyn WalletAuth>>,
    sessions: Arc<Mutex<HashMap<String, String>>>,
) -> Router {
    let (event_tx, _) = broadcast::channel(512);
    let state = Arc::new(AppState {
        deps,
        events: event_tx,
        pending: Mutex::new(HashMap::new()),
        lease_tools: Mutex::new(HashMap::new()),
        sessions,
        wallet,
    });
    // Wallet-only when SIWE is configured: the password route only exists on
    // the plain `router()` used by offline tests.
    let password_login = state.wallet.is_none();
    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/api/people", get(people))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/siwe/nonce", get(siwe_nonce))
        .route("/api/login/siwe", post(siwe_login))
        .route("/api/wallet/link", post(wallet_link))
        .route("/api/state", get(api_state))
        .route("/events", get(events))
        .route("/api/consent/{request_id}", post(consent))
        .route("/api/revoke", post(revoke))
        .route("/api/receipts/{id}", get(receipt))
        .route("/api/scenario/{n}", post(scenario))
        .route("/api/agent-log", post(agent_log))
        .route("/mcp", get(mcp_sse).post(mcp))
        .route("/sse", get(mcp_sse));
    let router = if password_login { router.route("/api/login", post(login)) } else { router };
    router
        .with_state(state)
        .layer(CorsLayer::permissive())
}

async fn healthz() -> &'static str { "ok" }

async fn people(State(state): State<Arc<AppState>>) -> Json<Vec<Person>> {
    Json(state.deps.vaults.iter().map(Vault::person).collect())
}

fn json_response(status: StatusCode, value: Value) -> Response {
    (status, Json(value)).into_response()
}

fn session_cookie(headers: &HeaderMap) -> Option<String> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    cookie.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == "aegis_session" && !value.is_empty()).then(|| value.to_string())
    })
}

async fn session_subject(state: &AppState, headers: &HeaderMap) -> Option<String> {
    let key = session_cookie(headers)?;
    state.sessions.lock().await.get(&key).cloned()
}

#[derive(Deserialize)]
struct LoginBody { subject: String, password: String }

async fn login(State(state): State<Arc<AppState>>, Json(body): Json<LoginBody>) -> Response {
    let Some(vault) = state.deps.vaults.iter().find(|v| v.subject() == body.subject) else {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error":"invalid credentials"}));
    };
    if !vault.verify_password(&body.password) {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error":"invalid credentials"}));
    }
    start_session(&state, body.subject).await
}

async fn start_session(state: &AppState, subject: String) -> Response {
    let token = uuid::Uuid::new_v4().simple().to_string();
    state.sessions.lock().await.insert(token.clone(), subject.clone());
    let mut response = json_response(StatusCode::OK, json!({"ok":true,"subject":subject}));
    let cookie = format!("aegis_session={token}; HttpOnly; Path=/; SameSite=Lax");
    if let Ok(value) = HeaderValue::from_str(&cookie) { response.headers_mut().insert(header::SET_COOKIE, value); }
    response
}

#[derive(Deserialize)]
struct SiweBody { message: String, signature: String }

async fn siwe_nonce(State(state): State<Arc<AppState>>) -> Response {
    match &state.wallet {
        Some(wallet) => json_response(StatusCode::OK, json!({"nonce": wallet.nonce()})),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn siwe_login(State(state): State<Arc<AppState>>, Json(body): Json<SiweBody>) -> Response {
    let Some(wallet) = state.wallet.clone() else { return StatusCode::NOT_FOUND.into_response() };
    match wallet.verify_login(&body.message, &body.signature).await {
        Ok(subject) => start_session(&state, subject).await,
        Err(e) => json_response(StatusCode::UNAUTHORIZED, json!({"error": e.to_string()})),
    }
}

async fn wallet_link(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(body): Json<SiweBody>) -> Response {
    let Some(wallet) = state.wallet.clone() else { return StatusCode::NOT_FOUND.into_response() };
    let Some(subject) = session_subject(&state, &headers).await else { return StatusCode::UNAUTHORIZED.into_response() };
    match wallet.link(&subject, &body.message, &body.signature).await {
        Ok(address) => json_response(StatusCode::OK, json!({"ok": true, "address": address})),
        Err(e) => json_response(StatusCode::BAD_REQUEST, json!({"error": e.to_string()})),
    }
}

async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(cookie) = session_cookie(&headers) { state.sessions.lock().await.remove(&cookie); }
    let mut response = json_response(StatusCode::OK, json!({"ok":true}));
    response.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_static("aegis_session=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0"));
    response
}

async fn me(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let Some(subject) = session_subject(&state, &headers).await else { return StatusCode::UNAUTHORIZED.into_response() };
    match state.deps.vaults.iter().find(|v| v.subject() == subject) {
        Some(vault) => {
            let wallet = state.wallet.as_ref().and_then(|w| w.wallet_of(&subject));
            json_response(StatusCode::OK, json!({"subject":subject,"display_name":vault.person().display_name,"mcp_token":vault.mcp_token(),"wallet":wallet}))
        }
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

fn authorized_subject(requested: Option<&str>, session: &str) -> Result<String, StatusCode> {
    if requested.is_some_and(|subject| subject != session) { return Err(StatusCode::FORBIDDEN); }
    Ok(requested.unwrap_or(session).to_string())
}

#[derive(Deserialize)]
struct SubjectQuery { subject: Option<String> }

async fn api_state(State(state): State<Arc<AppState>>, headers: HeaderMap, Query(query): Query<SubjectQuery>) -> Response {
    let Some(session) = session_subject(&state, &headers).await else { return StatusCode::UNAUTHORIZED.into_response() };
    let subject = match authorized_subject(query.subject.as_deref(), &session) { Ok(s) => s, Err(status) => return status.into_response() };
    let Some(vault) = state.deps.vaults.iter().find(|v| v.subject() == subject) else { return StatusCode::NOT_FOUND.into_response() };
    let now = now_unix();
    let pending = {
        let mut entries = state.pending.lock().await;
        entries.retain(|_, (req, _)| req.deadline >= now);
        entries.values().filter(|(req, _)| req.call.subject == subject).map(|(req, _)| req.clone()).collect()
    };
    let snapshot = DashboardState {
        person: vault.person(),
        metrics: state.deps.ledger.metrics(Some(&subject), now),
        pending,
        active_receipts: state.deps.ledger.active_receipts(Some(&subject), now),
        recent_ledger: state.deps.ledger.recent(Some(&subject), 50),
        public_key_hex: state.deps.signer.public_key_hex(),
    };
    Json(snapshot).into_response()
}

async fn events(State(state): State<Arc<AppState>>, headers: HeaderMap, Query(query): Query<SubjectQuery>) -> Response {
    let Some(session) = session_subject(&state, &headers).await else { return StatusCode::UNAUTHORIZED.into_response() };
    let subject = match authorized_subject(query.subject.as_deref(), &session) { Ok(s) => s, Err(status) => return status.into_response() };
    if !state.deps.vaults.iter().any(|v| v.subject() == subject) { return StatusCode::NOT_FOUND.into_response(); }
    let stream = BroadcastStream::new(state.events.subscribe()).filter_map(move |item| {
        let subject = subject.clone();
        async move {
            match item {
                Ok(envelope) if envelope.subject == subject => serde_json::to_string(&envelope).ok().map(|data| Ok::<_, Infallible>(SseEvent::default().data(data))),
                _ => None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))).into_response()
}

async fn consent(State(state): State<Arc<AppState>>, Path(request_id): Path<String>, headers: HeaderMap, Json(decision): Json<ConsentDecision>) -> Response {
    let Some(session) = session_subject(&state, &headers).await else { return StatusCode::UNAUTHORIZED.into_response() };
    let mut pending = state.pending.lock().await;
    let Some((request, _)) = pending.get(&request_id) else { return StatusCode::NOT_FOUND.into_response() };
    if request.call.subject != session { return StatusCode::FORBIDDEN.into_response(); }
    let Some((_, sender)) = pending.remove(&request_id) else { return StatusCode::NOT_FOUND.into_response() };
    drop(pending);
    let _ = sender.send(decision);
    json_response(StatusCode::OK, json!({"ok":true}))
}

async fn revoke(State(state): State<Arc<AppState>>, headers: HeaderMap, Query(query): Query<SubjectQuery>) -> Response {
    let Some(session) = session_subject(&state, &headers).await else { return StatusCode::UNAUTHORIZED.into_response() };
    let subject = match authorized_subject(query.subject.as_deref(), &session) { Ok(s) => s, Err(status) => return status.into_response() };
    if !state.deps.vaults.iter().any(|v| v.subject() == subject) { return StatusCode::NOT_FOUND.into_response(); }
    let ids = state.deps.ledger.revoke_all(Some(&subject));
    emit(&state, &subject, Event::LeaseRevoked { receipt_ids: ids.clone() });
    emit(&state, &subject, Event::Metrics { metrics: state.deps.ledger.metrics(Some(&subject), now_unix()) });
    json_response(StatusCode::OK, json!({"revoked":ids}))
}

async fn receipt(State(state): State<Arc<AppState>>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    let Some(session) = session_subject(&state, &headers).await else { return StatusCode::UNAUTHORIZED.into_response() };
    let Some(receipt) = state.deps.ledger.get_receipt(&id) else { return StatusCode::NOT_FOUND.into_response() };
    if receipt.claims.subject != session { return StatusCode::FORBIDDEN.into_response(); }
    let valid = state.deps.signer.verify(&receipt) && receipt.is_active(now_unix());
    json_response(StatusCode::OK, json!({"receipt":receipt,"valid":valid}))
}

async fn scenario(State(state): State<Arc<AppState>>, Path(n): Path<u8>, headers: HeaderMap, Query(query): Query<SubjectQuery>) -> Response {
    let Some(session) = session_subject(&state, &headers).await else { return StatusCode::UNAUTHORIZED.into_response() };
    let subject = match authorized_subject(query.subject.as_deref(), &session) { Ok(s) => s, Err(status) => return status.into_response() };
    if !state.deps.vaults.iter().any(|v| v.subject() == subject) { return StatusCode::NOT_FOUND.into_response(); }
    let Some(runner) = state.deps.scenarios.clone() else { return StatusCode::NOT_IMPLEMENTED.into_response() };
    let base_url = state.deps.base_url.clone();
    tokio::spawn(async move { runner.run(n, subject, base_url).await; });
    json_response(StatusCode::ACCEPTED, json!({"started":n}))
}

#[derive(Deserialize)]
struct AgentLog { subject: String, agent_id: String, text: String }

async fn agent_log(State(state): State<Arc<AppState>>, Json(body): Json<AgentLog>) -> Response {
    if !state.deps.vaults.iter().any(|v| v.subject() == body.subject) { return StatusCode::NOT_FOUND.into_response(); }
    emit(&state, &body.subject, Event::AgentLog { agent_id: body.agent_id, text: body.text });
    json_response(StatusCode::OK, json!({"ok":true}))
}

fn emit(state: &AppState, subject: &str, event: Event) {
    let _ = state.events.send(EventEnvelope { subject: subject.to_string(), event });
}

async fn mcp_sse(headers: HeaderMap) -> Response {
    let session_id = uuid::Uuid::new_v4().simple().to_string();
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(header::HOST))
        .and_then(|h| h.to_str().ok())
        .unwrap_or("127.0.0.1:8787");
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        .unwrap_or(if host.contains("trycloudflare.com") { "https" } else { "http" });
    let endpoint_uri = format!("{proto}://{host}/mcp?sessionId={session_id}");

    let stream = tokio_stream::iter(vec![
        Ok::<_, Infallible>(
            SseEvent::default()
                .event("endpoint")
                .data(endpoint_uri)
        )
    ]);
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

async fn mcp(State(state): State<Arc<AppState>>, request: Request<Body>) -> Response {
    let headers = request.headers();
    let bearer = headers.get(header::AUTHORIZATION).and_then(|h| h.to_str().ok()).and_then(|h| h.strip_prefix("Bearer "));
    // An agent credential (`aegis_fk_mcp_…`) verifies BOTH the person and the
    // agent id; the self-asserted `x-aegis-agent` header is ignored then.
    let mut verified_agent: Option<String> = None;
    let subject = if let Some(credential) = bearer.filter(|b| b.starts_with("aegis_fk_")) {
        let Some(verifier) = state.deps.verifier.clone() else {
            return StatusCode::UNAUTHORIZED.into_response();
        };
        match verifier.verify(credential).await {
            Some(identity) if state.deps.vaults.iter().any(|v| v.subject() == identity.subject) => {
                verified_agent = Some(identity.agent_id);
                identity.subject
            }
            _ => return StatusCode::UNAUTHORIZED.into_response(),
        }
    } else if let Some(token) = bearer {
        let Some(vault) = vault_for_token(&state.deps.vaults, token) else { return StatusCode::UNAUTHORIZED.into_response() };
        vault.subject()
    } else {
        if state.deps.require_mcp_token { return StatusCode::UNAUTHORIZED.into_response(); }
        let requested = headers.get("x-aegis-user").and_then(|h| h.to_str().ok());
        match requested {
            Some(s) if state.deps.vaults.iter().any(|v| v.subject() == s) => s.to_string(),
            Some(_) => return json_response(StatusCode::OK, rpc_error(Value::Null, -32602, "unknown subject")),
            None => state.deps.vaults.first().map(Vault::subject).unwrap_or_default(),
        }
    };
    if !state.deps.vaults.iter().any(|v| v.subject() == subject) {
        return json_response(StatusCode::OK, rpc_error(Value::Null, -32602, "unknown subject"));
    }
    let agent = verified_agent
        .or_else(|| headers.get("x-aegis-agent").and_then(|h| h.to_str().ok()).map(str::to_string))
        .unwrap_or_else(|| "anonymous_agent".to_string());
    let purpose = headers.get("x-aegis-purpose").and_then(|h| h.to_str().ok()).unwrap_or("unspecified").to_string();
    let bytes = match axum::body::to_bytes(request.into_body(), 1_048_576).await {
        Ok(bytes) => bytes,
        Err(_) => return json_response(StatusCode::BAD_REQUEST, json!({"error":"invalid body"})),
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return json_response(StatusCode::BAD_REQUEST, json!({"error":"invalid JSON"})),
    };
    let id = value.get("id").cloned().unwrap_or(Value::Null);
    let method = value.get("method").and_then(Value::as_str).unwrap_or("");
    if method == "notifications/initialized" { return StatusCode::ACCEPTED.into_response(); }
    let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
    let result = match method {
        "initialize" => json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"aegis-mcp","version":"0.1.0"}}),
        "tools/list" => json!({"tools":tool_specs().into_iter().map(|spec| json!({"name":spec.name,"description":spec.description,"inputSchema":spec.input_schema})).collect::<Vec<_>>()}),
        "tools/call" => match call_tool(&state, &subject, &agent, &purpose, &params).await {
            Ok(value) => value,
            Err((code, message)) => return json_response(StatusCode::OK, rpc_error(id, code, &message)),
        },
        _ => return json_response(StatusCode::OK, rpc_error(id, -32601, "method not found")),
    };
    json_response(StatusCode::OK, json!({"jsonrpc":"2.0","id":id,"result":result}))
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

async fn call_tool(state: &Arc<AppState>, subject: &str, agent: &str, purpose: &str, params: &Value) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let Some(spec) = tool_spec(name) else { return Err((-32602, format!("unknown tool: {name}"))) };
    let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let Some(vault) = state.deps.vaults.iter().find(|v| v.subject() == subject).cloned() else { return Err((-32602, "unknown subject".into())) };
    let call = ToolCall { call_id:new_id("call"), subject:subject.into(), agent_id:agent.into(), tool:name.into(), purpose:purpose.into(), arguments, ts:now_unix() };
    emit(state, subject, Event::ToolRequested { call:call.clone() });
    if let Some(violation) = state.deps.guard.inspect(&spec, &call) {
        emit(state, subject, Event::PolicyViolation { call_id:call.call_id.clone(), agent_id:agent.into(), tool:name.into(), reason:violation.reason.clone(), offending:violation.offending });
        let entry = ledger_entry(&call, spec.resource, Mode::Block, Outcome::Violation, Value::Null, 0, 0, None);
        let _ = state.deps.ledger.append(entry);
        emit(state, subject, Event::Metrics { metrics:state.deps.ledger.metrics(Some(subject), now_unix()) });
        return Ok(tool_error(violation_text(&violation.reason)));
    }

    let is_payment = call.tool == "make_payment" || call.tool == "order_item";
    let payment_amount_cents = if is_payment {
        let amount = call.arguments.get("amount_usd").and_then(Value::as_f64).unwrap_or(f64::NAN);
        let cents = amount * 100.0;
        if !amount.is_finite() || amount <= 0.0 || !cents.is_finite() || cents > i64::MAX as f64 {
            let reason = "amount_usd must be a positive amount";
            emit(state, subject, Event::PolicyViolation { call_id:call.call_id.clone(), agent_id:agent.into(), tool:name.into(), reason:reason.into(), offending:vec!["amount_usd".into()] });
            let _ = state.deps.ledger.append(ledger_entry(&call, spec.resource, Mode::Block, Outcome::Violation, Value::Null, 0, 0, None));
            emit(state, subject, Event::Metrics { metrics:state.deps.ledger.metrics(Some(subject), now_unix()) });
            return Ok(tool_error(format!("AEGIS POLICY VIOLATION: {reason}")));
        }
        let cents = cents.round() as i64;
        if cents > vault.max_payment_cents() {
            let reason = "payment exceeds per-person cap";
            emit(state, subject, Event::PolicyViolation { call_id:call.call_id.clone(), agent_id:agent.into(), tool:name.into(), reason:reason.into(), offending:vec!["amount_usd".into()] });
            let _ = state.deps.ledger.append(ledger_entry(&call, spec.resource, Mode::Block, Outcome::Violation, Value::Null, 0, 0, None));
            emit(state, subject, Event::Metrics { metrics:state.deps.ledger.metrics(Some(subject), now_unix()) });
            return Ok(tool_error(format!("AEGIS POLICY VIOLATION: {reason}")));
        }
        Some(cents)
    } else { None };

    let mut risk_requires_consent = false;
    if let Some(risk) = &state.deps.risk {
        let scores = tokio::time::timeout(Duration::from_secs(8), risk.score(&call)).await.ok().flatten();
        if let Some(scores) = scores {
            let prompt_blocked = scores.prompt_injection >= state.deps.risk_threshold;
            let exfil_blocked = scores.data_exfiltration >= state.deps.risk_threshold;
            let blocked = prompt_blocked || exfil_blocked;
            emit(state, subject, Event::RiskScored { call_id:call.call_id.clone(), agent_id:agent.into(), tool:name.into(), scores:scores.clone(), blocked });
            if blocked {
                let (dimension, score) = if prompt_blocked { ("promptInjection", scores.prompt_injection) } else { ("dataExfiltration", scores.data_exfiltration) };
                let reason = format!("Jev {dimension} {score:.2} ≥ {:.2}", state.deps.risk_threshold);
                let offending = if prompt_blocked { vec!["purpose".to_string()] } else {
                    call.arguments.as_object().map(|args| args.keys().cloned().collect()).filter(|keys: &Vec<String>| !keys.is_empty()).unwrap_or_else(|| vec!["arguments".into()])
                };
                emit(state, subject, Event::PolicyViolation { call_id:call.call_id.clone(), agent_id:agent.into(), tool:name.into(), reason:reason.clone(), offending });
                let _ = state.deps.ledger.append(ledger_entry(&call, spec.resource, Mode::Block, Outcome::Violation, Value::Null, 0, 0, None));
                emit(state, subject, Event::Metrics { metrics:state.deps.ledger.metrics(Some(subject), now_unix()) });
                return Ok(tool_error(format!("AEGIS POLICY VIOLATION: {reason}")));
            }
        } else {
            risk_requires_consent = true;
        }
    }
    if let Some(judge) = state.deps.judge.clone() {
        let call = call.clone();
        let spec = spec.clone();
        let subject = subject.to_string();
        let events = state.events.clone();
        tokio::spawn(async move {
            if let Some((false, rationale)) = judge.judge(&call, &spec).await {
                let _ = events.send(EventEnvelope {
                    subject,
                    event: Event::AgentLog { agent_id:"aegis".into(), text:format!("Purpose warning: {rationale}") },
                });
            }
        });
    }

    let requested_fields = requested_fields(&call, &spec);

    // Standing rules take precedence over leases and prompts (Phase 6).
    let rule = state.deps.policy.as_ref().and_then(|engine| engine.decide(&call, &spec, &requested_fields, payment_amount_cents));
    if let Some((rule, _)) = &rule {
        if matches!(rule.decision, RuleDecision::Deny) {
            let label = rule.label.clone();
            emit(state, subject, Event::RuleApplied { call_id:call.call_id.clone(), agent_id:agent.into(), tool:name.into(), rule_id:rule.rule_id.clone(), decision:rule.decision.clone() });
            emit(state, subject, Event::CallDenied { call_id:call.call_id.clone(), agent_id:agent.into(), tool:name.into(), reason:format!("denied by your rule: {label}") });
            let entry = ledger_entry(&call, spec.resource, Mode::Block, Outcome::Denied, Value::Null, 0, 0, None);
            let _ = state.deps.ledger.append(entry);
            emit(state, subject, Event::Metrics { metrics:state.deps.ledger.metrics(Some(subject), now_unix()) });
            return Ok(tool_error(format!("AEGIS 403: denied by your rule: {label}")));
        }
    }
    let forced_ask = matches!(rule, Some((_, RuleDecision::Ask)));
    let rule_allow = rule.filter(|(_, decision)| matches!(decision, RuleDecision::Allow { .. }))
        .map(|(rule, decision)| (rule, match decision { RuleDecision::Allow { mode } => mode, _ => Mode::CleanRoom }));
    let mut receipt_id = None;
    let active_receipt = if !is_payment && !forced_ask && rule_allow.is_none() {
        let now = now_unix();
        let tool_ids = state.lease_tools.lock().await;
        state.deps.ledger.active_receipts(Some(subject), now).into_iter().find(|receipt| {
            receipt.claims.subject == subject
                && receipt.claims.grantee_agent == agent
                && receipt.claims.resource == spec.resource
                && tool_ids.get(&receipt.claims.receipt_id).is_some_and(|tool| tool == name)
                && requested_fields.iter().all(|field| receipt.claims.allowed_fields.contains(field))
                && state.deps.signer.verify(receipt)
        })
    } else { None };
    let mode = if let Some((rule, rule_mode)) = rule_allow {
        // The person already decided this: resolve at the rule's mode with a
        // signed receipt (purpose = "rule:<id>") and no prompt.
        emit(state, subject, Event::RuleApplied { call_id:call.call_id.clone(), agent_id:agent.into(), tool:name.into(), rule_id:rule.rule_id.clone(), decision:RuleDecision::Allow { mode:rule_mode } });
        if !is_payment {
            let now = now_unix();
            let claims = ReceiptClaims {
                receipt_id:new_id("rcpt"), subject:subject.into(), grantee_agent:agent.into(), purpose:format!("rule:{}", rule.rule_id),
                resource:spec.resource, resolution_mode:rule_mode,
                allowed_fields:requested_fields.clone(),
                issued_at:now, expires_at:now.saturating_add(60),
            };
            let receipt = state.deps.signer.issue(claims);
            let _ = state.deps.ledger.store_receipt(&receipt);
            state.lease_tools.lock().await.insert(receipt.claims.receipt_id.clone(), name.to_string());
            receipt_id = Some(receipt.claims.receipt_id.clone());
        }
        rule_mode
    } else if let Some(receipt) = active_receipt {
        receipt_id = Some(receipt.claims.receipt_id);
        receipt.claims.resolution_mode
    } else if !is_payment && !risk_requires_consent && !forced_ask && spec.default_mode == Mode::Redact {
        Mode::Redact
    } else {
        let request_id = new_id("consent");
        let fields = call.arguments.get("fields").and_then(Value::as_array).map(|xs| xs.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>()).unwrap_or_default();
        let request = ConsentRequest {
            request_id:request_id.clone(), call:call.clone(), resource:spec.resource,
            requested_fields:fields.clone(),
            sensitive_fields:spec.sensitive_fields.iter().map(|s| (*s).to_string()).collect(),
            suggested_mode:Mode::CleanRoom, created_at:now_unix(),
            deadline:now_unix().saturating_add(state.deps.consent_timeout_secs as i64),
        };
        let (tx, rx) = oneshot::channel();
        state.pending.lock().await.insert(request_id.clone(), (request.clone(), tx));
        let _pending_guard = PendingDropGuard { state: state.clone(), request_id:request_id.clone() };
        emit(state, subject, Event::ConsentRequired { request:request.clone() });
        // The person's advocate weighs in off the critical path (Phase 6):
        // the wait itself never depends on the model answering in time.
        if let Some(advocate) = state.deps.advocate.clone() {
            let request = request.clone();
            let spec = spec.clone();
            let subject = subject.to_string();
            let events = state.events.clone();
            tokio::spawn(async move {
                if let Ok(advice) = tokio::time::timeout(Duration::from_secs(12), advocate.advise(&request, &spec)).await {
                    let _ = events.send(EventEnvelope { subject, event: Event::ConsentAdvice { request_id:request.request_id.clone(), advice } });
                }
            });
        }
        let decision = tokio::time::timeout(Duration::from_secs(state.deps.consent_timeout_secs), rx).await;
        state.pending.lock().await.remove(&request_id);
        let decision = match decision {
            Ok(Ok(decision)) => decision,
            _ => {
                emit(state, subject, Event::ConsentResolved { request_id:request_id.clone(), approved:false, receipt:None });
                emit(state, subject, Event::CallDenied { call_id:call.call_id.clone(), agent_id:agent.into(), tool:name.into(), reason:"consent timed out".into() });
                let entry = ledger_entry(&call, spec.resource, Mode::Block, Outcome::TimedOut, Value::Null, 0, 0, None);
                let _ = state.deps.ledger.append(entry);
                emit(state, subject, Event::Metrics { metrics:state.deps.ledger.metrics(Some(subject), now_unix()) });
                return Ok(tool_error("AEGIS 403: consent timed out".into()));
            }
        };
        // "Remember this decision" → a standing rule for next time (Phase 6).
        if let Some(scope) = decision.remember.clone() {
            if let Some(engine) = &state.deps.policy {
                let rule = rule_from_remember(&call, &spec, &requested_fields, decision.approve, decision.mode, &scope, payment_amount_cents);
                if let Err(error) = engine.add(rule) {
                    emit(state, subject, Event::AgentLog { agent_id:"aegis".into(), text:format!("Could not save remembered rule: {error}") });
                }
            }
        }
        if !decision.approve {
            emit(state, subject, Event::ConsentResolved { request_id:request_id.clone(), approved:false, receipt:None });
            emit(state, subject, Event::CallDenied { call_id:call.call_id.clone(), agent_id:agent.into(), tool:name.into(), reason:"consent denied".into() });
            let entry = ledger_entry(&call, spec.resource, Mode::Block, Outcome::Denied, Value::Null, 0, 0, None);
            let _ = state.deps.ledger.append(entry);
            emit(state, subject, Event::Metrics { metrics:state.deps.ledger.metrics(Some(subject), now_unix()) });
            return Ok(tool_error("AEGIS 403: consent denied".into()));
        }
        let mode = decision.mode;
        let now = now_unix();
        if is_payment {
            emit(state, subject, Event::ConsentResolved { request_id, approved:true, receipt:None });
        } else {
            let claims = ReceiptClaims {
                receipt_id:new_id("rcpt"), subject:subject.into(), grantee_agent:agent.into(), purpose:purpose.into(),
                resource:spec.resource, resolution_mode:mode,
                allowed_fields:if fields.is_empty() { spec.allowed_fields.iter().map(|s| (*s).to_string()).collect() } else { fields },
                issued_at:now, expires_at:now.saturating_add(decision.ttl_secs.max(0)),
            };
            let receipt = state.deps.signer.issue(claims);
            let _ = state.deps.ledger.store_receipt(&receipt);
            state.lease_tools.lock().await.insert(receipt.claims.receipt_id.clone(), name.to_string());
            receipt_id = Some(receipt.claims.receipt_id.clone());
            emit(state, subject, Event::ConsentResolved { request_id, approved:true, receipt:Some(receipt) });
        }
        mode
    };

    if let Some(amount_cents) = payment_amount_cents {
        let Some(provider) = &state.deps.payments else {
            return Ok(tool_error("AEGIS: payment failed: no payment provider configured".into()));
        };
        let merchant = call.arguments.get("merchant").and_then(Value::as_str).unwrap_or("").to_string();
        let description = call.arguments.get("description").and_then(Value::as_str).unwrap_or("").to_string();
        let request = PaymentRequest { subject:subject.into(), agent_id:agent.into(), amount_cents, currency:"usd".into(), merchant:merchant.clone(), description, idempotency_key:call.call_id.clone() };
        let receipt = match provider.charge(request).await {
            Ok(receipt) => receipt,
            Err(error) => {
                let reason = format!("payment failed: {error}");
                emit(state, subject, Event::CallDenied { call_id:call.call_id.clone(), agent_id:agent.into(), tool:name.into(), reason:reason.clone() });
                let _ = state.deps.ledger.append(ledger_entry(&call, spec.resource, Mode::CleanRoom, Outcome::Denied, json!({"error": reason}), 0, 0, None));
                emit(state, subject, Event::Metrics { metrics:state.deps.ledger.metrics(Some(subject), now_unix()) });
                return Ok(tool_error(format!("AEGIS: {reason}")));
            }
        };
        emit(state, subject, Event::PaymentExecuted { call_id:call.call_id.clone(), agent_id:agent.into(), receipt:receipt.clone() });
        let financials = vault.resource(Resource::Financials);
        let raw = json!({"account_holder":financials.get("account_holder").cloned().unwrap_or(Value::Null),"account_number":financials.get("account_number").cloned().unwrap_or(Value::Null),"routing_number":financials.get("routing_number").cloned().unwrap_or(Value::Null)});
        let mut released = serde_json::to_value(receipt).unwrap_or_else(|_| json!({}));
        if let Some(object) = released.as_object_mut() {
            object.insert("card".into(), json!("[TOKENIZED BY AEGIS]"));
            if call.tool == "order_item" {
                let item = call.arguments.get("item").and_then(Value::as_str).unwrap_or("Requested Item");
                object.insert("order_status".into(), json!("CONFIRMED_AND_PAID"));
                object.insert("item".into(), json!(item));
                object.insert("delivery".into(), json!({
                    "status": "DISPATCHED_TO_VAULT_ADDRESS",
                    "shipping_token": format!("aegis_ship_{}", &call.call_id[..call.call_id.len().min(12)]),
                    "recipient": vault.person().display_name,
                    "address": "[STORED SECURELY IN VAULT - SHIELDED FROM AI]"
                }));
            }
        }
        let bytes_raw = serde_json::to_vec(&raw).map(|v| v.len()).unwrap_or(0);
        let bytes_released = serde_json::to_vec(&released).map(|v| v.len()).unwrap_or(0);
        emit(state, subject, Event::PayloadReleased { call_id:call.call_id.clone(), agent_id:agent.into(), tool:name.into(), mode:Mode::CleanRoom, raw:raw.clone(), released:released.clone(), bytes_raw, bytes_released, receipt_id:None });
        let _ = state.deps.ledger.append(ledger_entry(&call, spec.resource, Mode::CleanRoom, Outcome::Released, released.clone(), bytes_raw, bytes_released, None));
        emit(state, subject, Event::Metrics { metrics:state.deps.ledger.metrics(Some(subject), now_unix()) });
        return Ok(json!({"content":[{"type":"text","text":released.to_string()}],"structuredContent":released,"_meta":{"aegis":{"mode":Mode::CleanRoom,"payment":true,"receipt_id":null}}}));
    }

    let raw = vault.resource(spec.resource);
    let released = match mode {
        Mode::Allow => project_fields(&raw, &requested_fields),
        Mode::Redact => state.deps.redactor.redact(spec.resource, &raw).value,
        Mode::CleanRoom => state.deps.clean_room.evaluate(spec.resource, &raw, &call.arguments),
        Mode::Block => json!({"status":"BLOCKED_BY_AEGIS"}),
    };
    let bytes_raw = serde_json::to_vec(&raw).map(|v| v.len()).unwrap_or(0);
    let bytes_released = serde_json::to_vec(&released).map(|v| v.len()).unwrap_or(0);
    emit(state, subject, Event::PayloadReleased { call_id:call.call_id.clone(), agent_id:agent.into(), tool:name.into(), mode, raw:raw.clone(), released:released.clone(), bytes_raw, bytes_released, receipt_id:receipt_id.clone() });
    let entry = ledger_entry(&call, spec.resource, mode, Outcome::Released, released.clone(), bytes_raw, bytes_released, receipt_id.clone());
    let _ = state.deps.ledger.append(entry);
    emit(state, subject, Event::Metrics { metrics:state.deps.ledger.metrics(Some(subject), now_unix()) });
    Ok(json!({"content":[{"type":"text","text":released.to_string()}],"structuredContent":released,"_meta":{"aegis":{"mode":mode,"receipt_id":receipt_id}}}))
}

fn requested_fields(call: &ToolCall, spec: &ToolSpec) -> Vec<String> {
    match call.arguments.get("fields").and_then(Value::as_array) {
        Some(fields) => fields.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        None => spec.allowed_fields.iter().map(|field| (*field).to_string()).collect(),
    }
}

/// Keep only the approved fields: top-level keys of an object, or the same
/// keys inside each element of an array (e.g. `calendar.events`, `contacts`).
fn project_fields(raw: &Value, fields: &[String]) -> Value {
    match raw {
        Value::Array(items) => Value::Array(items.iter().map(|item| project_fields(item, fields)).collect()),
        Value::Object(object) => {
            let mut projected = serde_json::Map::new();
            for (key, value) in object {
                if fields.contains(key) {
                    projected.insert(key.clone(), value.clone());
                } else if value.is_array() && value.as_array().is_some_and(|xs| xs.iter().any(Value::is_object)) {
                    projected.insert(key.clone(), project_fields(value, fields));
                }
            }
            Value::Object(projected)
        }
        _ => Value::Null,
    }
}

fn violation_text(reason: &str) -> String {
    if reason.starts_with("AEGIS POLICY VIOLATION") { reason.to_string() } else { format!("AEGIS POLICY VIOLATION: {reason}") }
}

fn tool_error(message: String) -> Value { json!({"isError":true,"content":[{"type":"text","text":message}]}) }

fn ledger_entry(call: &ToolCall, resource: Resource, mode: Mode, outcome: Outcome, payload: Value, bytes_raw: usize, bytes_released: usize, receipt_id: Option<String>) -> LedgerEntry {
    let mut raw_requested = call.arguments.get("fields").and_then(Value::as_array).map(|xs| xs.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>()).unwrap_or_default();
    if raw_requested.is_empty() { raw_requested = tool_spec(&call.tool).map(|s| s.allowed_fields.iter().map(|x| (*x).to_string()).collect()).unwrap_or_default(); }
    let _ = resource;
    LedgerEntry { id:new_id("entry"), ts:now_unix(), subject:call.subject.clone(), call_id:call.call_id.clone(), agent_id:call.agent_id.clone(), tool:call.tool.clone(), purpose:call.purpose.clone(), raw_requested, policy_applied:mode, outcome, payload_returned:payload, bytes_raw, bytes_released, receipt_id, entry_hash:String::new() }
}

#[cfg(test)]
mod review_tests;
