use aegis_broker::{router, BrokerState, BrokerStore, MemoryStore, Sealer};
use aegis_core::now_unix;
use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;
use tower::ServiceExt;

#[derive(Clone, Debug)]
struct UpstreamHit {
    auth_header: String,
    key_seen: String,
    path_and_query: String,
}

fn basic_auth_username(username: &str) -> String {
    format!("Basic {}", B64.encode(format!("{username}:")))
}

fn key_from_auth(auth: &str) -> String {
    if let Some(token) = auth.strip_prefix("Bearer ") {
        return token.to_string();
    }
    if let Some(raw) = auth.strip_prefix("Basic ") {
        if let Ok(decoded) = B64.decode(raw) {
            if let Ok(pair) = String::from_utf8(decoded) {
                if let Some((username, _)) = pair.split_once(':') {
                    return username.to_string();
                }
            }
        }
    }
    String::new()
}

async fn upstream_echo(
    State(hits): State<Arc<Mutex<Vec<UpstreamHit>>>>,
    req: Request<Body>,
) -> Response {
    let (parts, body) = req.into_parts();
    let _ = to_bytes(body, 1 << 20).await;
    let auth = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let key_seen = key_from_auth(&auth);
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or(parts.uri.path())
        .to_string();

    hits.lock().await.push(UpstreamHit {
        auth_header: auth.clone(),
        key_seen: key_seen.clone(),
        path_and_query,
    });

    let mut resp = Json(json!({
        "upstream_auth": auth,
        "upstream_leak": format!("raw={key_seen}")
    }))
    .into_response();
    let leak = format!("token={key_seen}");
    resp.headers_mut().insert(
        "x-upstream-leak",
        HeaderValue::from_str(&leak).unwrap_or_else(|_| HeaderValue::from_static("invalid")),
    );
    resp
}

async fn spawn_stub_upstream() -> (String, Arc<Mutex<Vec<UpstreamHit>>>) {
    let hits = Arc::new(Mutex::new(Vec::<UpstreamHit>::new()));
    let app = Router::new()
        .route("/{*path}", any(upstream_echo))
        .with_state(hits.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), hits)
}

fn broker_app(store: Arc<MemoryStore>, token: &str, upstream_base: &str) -> Router {
    let state = BrokerState::new(store, Sealer::new([7u8; 32]), token).with_upstreams(
        upstream_base.to_string(),
        upstream_base.to_string(),
        upstream_base.to_string(),
    );
    router(state)
}

async fn read_body(resp: Response) -> String {
    String::from_utf8(
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap()
}

async fn admin_post(
    app: &Router,
    token: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, HeaderMap, String) {
    let req = Request::post(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = read_body(resp).await;
    (status, headers, body)
}

async fn admin_get(
    app: &Router,
    token: Option<&str>,
    uri: &str,
) -> (StatusCode, HeaderMap, String) {
    let mut req = Request::get(uri).body(Body::empty()).unwrap();
    if let Some(token) = token {
        req.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
    }
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = read_body(resp).await;
    (status, headers, body)
}

async fn proxy_call(
    app: &Router,
    method: Method,
    uri: &str,
    auth_value: &str,
) -> (StatusCode, HeaderMap, String) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, auth_value)
        .header(header::CONTENT_TYPE, "application/json")
        .header("idempotency-key", "idem-1")
        .body(Body::from(r#"{"probe":"ok"}"#))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = read_body(resp).await;
    (status, headers, body)
}

async fn add_secret(
    app: &Router,
    token: &str,
    subject: Option<&str>,
    provider: &str,
    label: &str,
    value: &str,
) {
    let (status, _, body) = admin_post(
        app,
        token,
        "/admin/secrets",
        json!({"subject": subject, "provider": provider, "label": label, "value": value}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn issue_fake_key(
    app: &Router,
    token: &str,
    subject: &str,
    agent_id: &str,
    provider: &str,
    ttl_secs: u64,
) -> String {
    let (status, _, body) = admin_post(
        app,
        token,
        "/admin/fake-keys",
        json!({"subject": subject, "agent_id": agent_id, "provider": provider, "ttl_secs": ttl_secs}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_str::<Value>(&body).unwrap()["fake_key"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn admin_requires_correct_bearer_token() {
    let store = Arc::new(MemoryStore::new());
    let app = broker_app(store, "broker-token", "http://127.0.0.1:9");

    let (status, _, _) = admin_get(&app, None, "/admin/secrets").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) = admin_get(&app, Some("wrong-token"), "/admin/secrets").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let req = Request::post("/admin/fake-keys")
        .header(header::AUTHORIZATION, "Bearer wrong-token")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"subject":"maya","agent_id":"a","provider":"stripe","ttl_secs":60}).to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_responses_never_echo_secret_value() {
    let store = Arc::new(MemoryStore::new());
    let app = broker_app(store, "broker-token", "http://127.0.0.1:9");
    let secret = "sk_test_ultra_private_7788";

    let (status, _, added_body) = admin_post(
        &app,
        "broker-token",
        "/admin/secrets",
        json!({"subject":"maya","provider":"stripe","label":"personal","value":secret}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !added_body.contains(secret),
        "admin add response leaked secret: {added_body}"
    );
    let added: Value = serde_json::from_str(&added_body).unwrap();
    assert_eq!(added["last4"], "7788");

    let (status, _, listed_body) =
        admin_get(&app, Some("broker-token"), "/admin/secrets?subject=maya").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !listed_body.contains(secret),
        "admin list response leaked secret: {listed_body}"
    );

    let (status, _, vault_body) = admin_get(&app, Some("broker-token"), "/admin/vault/maya").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !vault_body.contains(secret),
        "vault response leaked unrelated secret: {vault_body}"
    );
}

#[tokio::test]
async fn fake_key_is_bound_to_subject_and_provider() {
    let (upstream, hits) = spawn_stub_upstream().await;
    let store = Arc::new(MemoryStore::new());
    let app = broker_app(store, "broker-token", &upstream);

    let maya_real = "sk_test_maya_real_1111";
    let dev_real = "sk_test_dev_real_2222";
    add_secret(
        &app,
        "broker-token",
        Some("maya"),
        "stripe",
        "maya stripe",
        maya_real,
    )
    .await;
    add_secret(
        &app,
        "broker-token",
        Some("dev"),
        "stripe",
        "dev stripe",
        dev_real,
    )
    .await;

    let maya_fake = issue_fake_key(&app, "broker-token", "maya", "agent-a", "stripe", 600).await;

    let (status, _, body) = proxy_call(
        &app,
        Method::POST,
        "/proxy/stripe/v1/payment_intents",
        &basic_auth_username(&maya_fake),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let hit = hits.lock().await.last().cloned().expect("one upstream hit");
    assert_eq!(
        hit.key_seen, maya_real,
        "maya fake key must resolve to maya's real key"
    );
    assert_ne!(
        hit.key_seen, dev_real,
        "maya fake key must not resolve as dev"
    );

    let (status, _, _) = proxy_call(
        &app,
        Method::POST,
        "/proxy/jev/v1/systemone",
        &format!("Bearer {maya_fake}"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "stripe fake key must not work for jev proxy"
    );
}

#[tokio::test]
async fn expired_and_revoked_keys_return_401() {
    let (upstream, hits) = spawn_stub_upstream().await;
    let store = Arc::new(MemoryStore::new());
    let app = broker_app(store.clone(), "broker-token", &upstream);
    add_secret(
        &app,
        "broker-token",
        Some("maya"),
        "jev",
        "maya jev",
        "jev_real_secret",
    )
    .await;

    let expired_key = issue_fake_key(&app, "broker-token", "maya", "agent-a", "jev", 600).await;
    let expired_prefix: String = expired_key.chars().take(20).collect();
    let mut rows = store.fake_keys("maya").await.unwrap();
    let mut row = rows
        .iter_mut()
        .find(|r| r.info.prefix == expired_prefix)
        .cloned()
        .expect("issued key row");
    row.info.expires_at = now_unix() - 5;
    store.update_fake_key(row).await.unwrap();

    let (status, _, _) = proxy_call(
        &app,
        Method::POST,
        "/proxy/jev/v1/systemone",
        &format!("Bearer {expired_key}"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "expired fake key must be rejected"
    );

    let revoked_key = issue_fake_key(&app, "broker-token", "maya", "agent-a", "jev", 600).await;
    let revoked_prefix: String = revoked_key.chars().take(20).collect();
    let (status, _, body) = admin_post(
        &app,
        "broker-token",
        "/admin/fake-keys/revoke",
        json!({"subject":"maya","prefix": revoked_prefix}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, _, _) = proxy_call(
        &app,
        Method::POST,
        "/proxy/jev/v1/systemone",
        &format!("Bearer {revoked_key}"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "revoked fake key must be rejected"
    );
    assert!(
        hits.lock().await.is_empty(),
        "expired/revoked keys should not call upstream"
    );
}

#[tokio::test]
async fn proxy_uses_real_key_and_scrubs_leaks_from_body_and_headers() {
    let (upstream, hits) = spawn_stub_upstream().await;
    let store = Arc::new(MemoryStore::new());
    let app = broker_app(store.clone(), "broker-token", &upstream);

    let real = "sk_test_real_secret_4242";
    add_secret(
        &app,
        "broker-token",
        Some("maya"),
        "stripe",
        "maya stripe",
        real,
    )
    .await;
    let fake = issue_fake_key(&app, "broker-token", "maya", "agent-a", "stripe", 600).await;

    let client_auth = basic_auth_username(&fake);
    assert!(
        !client_auth.contains(real),
        "client request must never carry the real key"
    );

    let (status, headers, body) = proxy_call(
        &app,
        Method::POST,
        "/proxy/stripe/v1/payment_intents?expand[]=foo",
        &client_auth,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !body.contains(real),
        "response body must scrub real key: {body}"
    );
    assert!(
        body.contains("[AEGIS-SCRUBBED]"),
        "response body should show scrub marker: {body}"
    );

    let leak_header = headers
        .get("x-upstream-leak")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        !leak_header.contains(real),
        "response header must scrub real key: {leak_header}"
    );
    assert!(
        leak_header.contains("[AEGIS-SCRUBBED]"),
        "response header should show scrub marker: {leak_header}"
    );

    let all_hits = hits.lock().await.clone();
    assert_eq!(all_hits.len(), 1, "expected one upstream request");
    let hit = &all_hits[0];
    assert_eq!(hit.path_and_query, "/v1/payment_intents?expand[]=foo");
    assert_eq!(
        hit.key_seen, real,
        "upstream should receive the real provider key"
    );
    assert!(
        !hit.auth_header.contains(&fake),
        "upstream auth must not contain fake key"
    );

    let rows = store.usage_rows().await;
    assert!(
        rows.iter().any(|r| r.provider == "stripe" && r.ok),
        "successful proxy call must log ok=true row"
    );
}

#[tokio::test]
async fn stripe_proxy_refuses_non_test_real_key() {
    let (upstream, hits) = spawn_stub_upstream().await;
    let store = Arc::new(MemoryStore::new());
    let app = broker_app(store.clone(), "broker-token", &upstream);

    add_secret(
        &app,
        "broker-token",
        Some("maya"),
        "stripe",
        "maya stripe",
        "sk_live_not_allowed_9999",
    )
    .await;
    let fake = issue_fake_key(&app, "broker-token", "maya", "agent-a", "stripe", 600).await;

    let (status, _, body) = proxy_call(
        &app,
        Method::POST,
        "/proxy/stripe/v1/payment_intents",
        &basic_auth_username(&fake),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("test keys only"), "{body}");
    assert!(
        hits.lock().await.is_empty(),
        "non-test stripe key should be blocked before upstream"
    );

    let rows = store.usage_rows().await;
    assert!(
        rows.iter()
            .any(|r| !r.ok && r.provider == "stripe" && r.path == "v1/payment_intents"),
        "reject must log usage row with ok=false: {rows:?}"
    );
}

#[tokio::test]
async fn sealer_round_trip_and_wrong_key_fail() {
    let one = Sealer::new([1u8; 32]);
    let two = Sealer::new([2u8; 32]);
    let plaintext = b"top secret broker payload";

    let (nonce, ciphertext) = one.seal(plaintext);
    let opened = one.open(&nonce, &ciphertext).unwrap();
    assert_eq!(opened, plaintext);

    let wrong = two.open(&nonce, &ciphertext);
    assert!(wrong.is_err(), "opening with wrong key must fail");
}

#[tokio::test]
async fn rejects_log_usage_rows_with_ok_false() {
    let (upstream, _) = spawn_stub_upstream().await;
    let store = Arc::new(MemoryStore::new());
    let app = broker_app(store.clone(), "broker-token", &upstream);

    add_secret(
        &app,
        "broker-token",
        Some("maya"),
        "stripe",
        "maya stripe",
        "sk_test_maya_1111",
    )
    .await;
    add_secret(
        &app,
        "broker-token",
        Some("maya"),
        "jev",
        "maya jev",
        "jev_real_1111",
    )
    .await;

    // Provider mismatch reject.
    let stripe_fake = issue_fake_key(&app, "broker-token", "maya", "agent-a", "stripe", 600).await;
    let (status, _, _) = proxy_call(
        &app,
        Method::POST,
        "/proxy/jev/v1/systemone",
        &format!("Bearer {stripe_fake}"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Expired reject.
    let expired_fake = issue_fake_key(&app, "broker-token", "maya", "agent-a", "jev", 600).await;
    let expired_prefix: String = expired_fake.chars().take(20).collect();
    let mut expired_row = store
        .fake_keys("maya")
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.info.prefix == expired_prefix)
        .expect("expired key row");
    expired_row.info.expires_at = now_unix() - 1;
    store.update_fake_key(expired_row).await.unwrap();
    let (status, _, _) = proxy_call(
        &app,
        Method::POST,
        "/proxy/jev/v1/systemone",
        &format!("Bearer {expired_fake}"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Revoked reject.
    let revoked_fake = issue_fake_key(&app, "broker-token", "maya", "agent-a", "jev", 600).await;
    let revoked_prefix: String = revoked_fake.chars().take(20).collect();
    let _ = admin_post(
        &app,
        "broker-token",
        "/admin/fake-keys/revoke",
        json!({"subject":"maya","prefix": revoked_prefix}),
    )
    .await;
    let (status, _, _) = proxy_call(
        &app,
        Method::POST,
        "/proxy/jev/v1/systemone",
        &format!("Bearer {revoked_fake}"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let rows = store.usage_rows().await;
    assert!(
        rows.iter().any(|r| !r.ok && r.path == "v1/systemone"),
        "rejects must log usage rows with ok=false, got: {rows:?}"
    );
}
