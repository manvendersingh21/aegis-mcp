use aegis::{build_app, Options};
use aegis_agent::Scenarios;
use aegis_core::ScenarioRunner;
use reqwest::header::{COOKIE, SET_COOKIE};
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const MAYA_SECRETS: &[&str] = &[
    "523-45-8812",
    "4417 1234 5678 9113",
    "121000358",
    "X41927736",
    "D7731942",
    "KP-99812-4471",
    "maya.okafor@protonmail.com",
    "Sequoia",
    "Oncology",
    "Planned Parenthood",
    "aegis_sk_maya_4f9c2e7a1b8d",
    "78df404b4f38d2011da01ef33b341c9fb9234d532c504c4698fb2e9e1dcca473",
    "85308915824051e8a7333caa84c94651efc8c48a033c111ec9f6f75c5cace888",
];

fn assert_no_maya_secrets(label: &str, body: &Value) {
    let text = serde_json::to_string(body).unwrap_or_default();
    for secret in MAYA_SECRETS {
        assert!(!text.contains(secret), "{label} leaked `{secret}`");
    }
}

fn cookie_header(token: &str) -> String {
    format!("aegis_session={token}")
}

/// Wallet-only sign-in via the labelled demo wallet: fetch its address, build
/// the EIP-4361 message with a fresh nonce, have it signed, POST /api/login/siwe.
async fn login(client: &reqwest::Client, base: &str, subject: &str) -> String {
    let address: Value = client.get(format!("{base}/api/demo-wallet/{subject}")).send().await.unwrap().json().await.unwrap();
    let address = address["address"].as_str().expect("demo wallet address").to_string();
    let nonce: Value = client.get(format!("{base}/api/siwe/nonce")).send().await.unwrap().json().await.unwrap();
    let host = base.trim_start_matches("http://");
    let message = aegis_chain::siwe_message(
        host, &address, "Sign in to AegisMCP — your consent firewall.", base, 84532,
        nonce["nonce"].as_str().unwrap(), &chrono::Utc::now().to_rfc3339(),
    );
    let signed: Value = client
        .post(format!("{base}/api/demo-wallet/sign"))
        .json(&json!({"subject": subject, "message": message}))
        .send().await.unwrap().json().await.unwrap();
    let resp = client
        .post(format!("{base}/api/login/siwe"))
        .json(&json!({"message": message, "signature": signed["signature"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "login as {subject}");
    let set_cookie = resp
        .headers()
        .get(SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("Set-Cookie on login")
        .to_string();
    let pair = set_cookie.split(';').next().unwrap_or_default().trim();
    let token = pair.strip_prefix("aegis_session=").expect("aegis_session cookie");
    assert_eq!(token.len(), 32, "session token is 32 hex chars");
    token.to_string()
}

async fn get(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    session: Option<&str>,
) -> reqwest::Response {
    let mut req = client.get(format!("{base}{path}"));
    if let Some(token) = session {
        req = req.header(COOKIE, cookie_header(token));
    }
    req.send().await.unwrap()
}

async fn post_json(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    session: Option<&str>,
    body: &Value,
) -> reqwest::Response {
    let mut req = client.post(format!("{base}{path}"));
    if let Some(token) = session {
        req = req.header(COOKIE, cookie_header(token));
    }
    req.json(body).send().await.unwrap()
}

async fn mcp(
    client: &reqwest::Client,
    base: &str,
    bearer: Option<&str>,
    user: Option<&str>,
    agent: &str,
    purpose: &str,
    id: i64,
    method: &str,
    params: Value,
) -> Value {
    let mut req = client
        .post(format!("{base}/mcp"))
        .header("x-aegis-agent", agent)
        .header("x-aegis-purpose", purpose);
    if let Some(token) = bearer {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    if let Some(subject) = user {
        req = req.header("x-aegis-user", subject);
    }
    let resp = req
        .json(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "MCP {method} HTTP status");
    resp.json().await.unwrap()
}

async fn subject_state(client: &reqwest::Client, base: &str, session: &str, subject: &str) -> Value {
    let resp = get(client, base, &format!("/api/state?subject={subject}"), Some(session)).await;
    assert_eq!(resp.status(), StatusCode::OK, "state for {subject}");
    resp.json().await.unwrap()
}

#[tokio::test]
async fn full_app_e2e_privacy_multi_tenancy_payments() {
    tokio::time::timeout(Duration::from_secs(55), async {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let app = build_app(&Options::offline(port)).expect("build_app");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let client = reqwest::Client::new();
        for _ in 0..50 {
            if get(&client, &base, "/healthz", None).await.status() == StatusCode::OK {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let people: Value = get(&client, &base, "/api/people", None).await.json().await.unwrap();
        assert_eq!(people.as_array().map(Vec::len), Some(2));

        let maya = login(&client, &base, "maya").await;
        let dev = login(&client, &base, "dev").await;
        // Wallet-only: the password route no longer exists on the live app.
        let pw = post_json(&client, &base, "/api/login", None, &json!({"subject": "maya", "password": "maya-demo"})).await;
        assert!(pw.status() == StatusCode::NOT_FOUND || pw.status() == StatusCode::METHOD_NOT_ALLOWED, "password login must be gone: {}", pw.status());
        // A forged signature is rejected.
        let bad = post_json(&client, &base, "/api/login/siwe", None, &json!({"message": "x", "signature": "0x00"})).await;
        assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);

        let maya_me: Value = get(&client, &base, "/api/me", Some(&maya)).await.json().await.unwrap();
        assert_eq!(maya_me["subject"], json!("maya"));
        assert_eq!(maya_me["mcp_token"], json!("aegis_sk_maya_4f9c2e7a1b8d"));

        let stop = Arc::new(AtomicBool::new(false));
        let approver = {
            let stop = stop.clone();
            let client = client.clone();
            let base = base.clone();
            let session = maya.clone();
            tokio::spawn(async move {
                while !stop.load(Ordering::Relaxed) {
                    if let Some(pending) = get(&client, &base, "/api/state?subject=maya", Some(&session))
                        .await
                        .json::<Value>()
                        .await
                        .ok()
                        .and_then(|s| s["pending"].as_array().cloned())
                    {
                        for request in pending {
                            let Some(request_id) = request["request_id"].as_str() else { continue };
                            let decision = if request["call"]["tool"] == json!("export_contacts") {
                                json!({"approve": false})
                            } else {
                                json!({"approve": true, "mode": "CLEAN_ROOM", "ttl_secs": 120})
                            };
                            let _ = post_json(
                                &client,
                                &base,
                                &format!("/api/consent/{request_id}"),
                                Some(&session),
                                &decision,
                            )
                            .await;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
            })
        };

        let scenarios = Scenarios::new(None);
        let _ = tokio::join!(
            scenarios.run(1, "maya".to_string(), base.clone()),
            scenarios.run(2, "maya".to_string(), base.clone()),
        );

        let state = subject_state(&client, &base, &maya, "maya").await;
        let ledger = state["recent_ledger"].as_array().cloned().unwrap_or_default();
        assert!(ledger.len() >= 5, "scenarios 1 and 2 filled the ledger");
        assert!(
            state["pending"].as_array().map(Vec::is_empty).unwrap_or(true),
            "no consent request left pending"
        );

        for entry in &ledger {
            assert_no_maya_secrets("maya ledger entry", entry);
        }
        let released = ledger.iter().filter(|e| e["outcome"] == json!("released")).count();
        assert!(released >= 3, "scenario 1 released three payloads, got {released}");
        let hostile = ledger
            .iter()
            .filter(|e| e["agent_id"] == json!("shady_coupon_bot") && e["outcome"] == json!("violation"))
            .count();
        assert!(hostile >= 2, "scenario 2 produced >= 2 violations, got {hostile}");
        assert!(state["metrics"]["violations"].as_u64().unwrap_or(0) >= 2);

        let receipt = state["active_receipts"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .find(|r| r["grantee_agent"] == json!("acme_travel_broker_v2") && r["resource"] == json!("financials"))
            .expect("scenario 1 left an active financials receipt");
        let receipt_id = receipt["receipt_id"].as_str().unwrap().to_string();
        let verified: Value = get(&client, &base, &format!("/api/receipts/{receipt_id}"), Some(&maya))
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(verified["valid"], json!(true));
        assert_eq!(verified["receipt"]["subject"], json!("maya"));
        assert_eq!(verified["receipt"]["resolution_mode"], json!("CLEAN_ROOM"));
        let cross = get(&client, &base, &format!("/api/receipts/{receipt_id}"), Some(&dev)).await;
        assert_eq!(cross.status(), StatusCode::FORBIDDEN);

        let dev_state: Value = get(&client, &base, "/api/state", Some(&dev)).await.json().await.unwrap();
        assert_eq!(dev_state["person"]["subject"], json!("dev"));
        assert!(
            dev_state["recent_ledger"].as_array().map(Vec::is_empty).unwrap_or(false),
            "dev ledger untouched"
        );
        assert!(
            dev_state["pending"].as_array().map(Vec::is_empty).unwrap_or(false),
            "dev has no pending consent"
        );
        let dev_on_maya = get(&client, &base, "/api/state?subject=maya", Some(&dev)).await;
        assert_eq!(dev_on_maya.status(), StatusCode::FORBIDDEN);

        let dev_me: Value = get(&client, &base, "/api/me", Some(&dev)).await.json().await.unwrap();
        let dev_token = dev_me["mcp_token"].as_str().unwrap().to_string();
        let bad_token = client
            .post(format!("{base}/mcp"))
            .header("authorization", "Bearer aegis_sk_bogus")
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}))
            .send()
            .await
            .unwrap();
        assert_eq!(bad_token.status(), StatusCode::UNAUTHORIZED);

        let dev_call = mcp(
            &client,
            &base,
            Some(&dev_token),
            None,
            "e2e_probe_agent",
            "Check calendar availability",
            10,
            "tools/call",
            json!({"name": "check_schedule", "arguments": {"fields": ["start", "end"]}}),
        )
        .await;
        assert_ne!(dev_call["result"]["isError"], json!(true), "dev redacted schedule resolved");
        assert_no_maya_secrets("dev MCP response", &dev_call);
        let dev_state: Value = get(&client, &base, "/api/state", Some(&dev)).await.json().await.unwrap();
        let dev_entries = dev_state["recent_ledger"].as_array().cloned().unwrap_or_default();
        assert!(dev_entries.iter().all(|e| e["subject"] == json!("dev")));
        assert!(
            dev_entries
                .iter()
                .any(|e| e["subject"] == json!("dev")
                    && e["tool"] == json!("check_schedule")
                    && e["agent_id"] == json!("e2e_probe_agent")),
            "bearer-token MCP call landed in dev ledger"
        );
        for entry in &dev_entries {
            assert_no_maya_secrets("dev ledger entry", entry);
        }

        let pay_ok = mcp(
            &client,
            &base,
            None,
            Some("maya"),
            "e2e_probe_agent",
            "Pay travel deposit after host approval",
            20,
            "tools/call",
            json!({"name": "make_payment", "arguments": {"amount_usd": 120, "merchant": "Acme Travel", "description": "Trip deposit"}}),
        )
        .await;
        assert_ne!(pay_ok["result"]["isError"], json!(true), "approved payment succeeded");
        let payment = &pay_ok["result"]["structuredContent"];
        let payment_id = payment["payment_id"].as_str().expect("demo payment_id");
        assert!(payment_id.starts_with("demo_pi_"), "demo payment id, got {payment_id}");
        assert_eq!(payment_id.len(), "demo_pi_".len() + 16);
        assert_eq!(payment["provider"], json!("demo"));
        assert_eq!(payment["status"], json!("succeeded"));
        assert_eq!(payment["amount_cents"], json!(12000));
        assert_eq!(payment["merchant"], json!("Acme Travel"));
        assert_eq!(payment["card"], json!("[TOKENIZED BY AEGIS]"));
        assert_no_maya_secrets("approved payment response", &pay_ok);

        let pay_bad = mcp(
            &client,
            &base,
            None,
            Some("maya"),
            "e2e_probe_agent",
            "Pay travel deposit after host approval",
            21,
            "tools/call",
            json!({"name": "make_payment", "arguments": {"amount_usd": 999999, "merchant": "Acme Travel"}}),
        )
        .await;
        assert_eq!(pay_bad["result"]["isError"], json!(true), "over-cap payment is an error");
        let text = pay_bad["result"]["content"][0]["text"].as_str().unwrap_or_default();
        assert!(text.contains("POLICY VIOLATION"), "violation text, got {text}");
        assert!(text.to_lowercase().contains("cap"), "cap reason, got {text}");
        assert_no_maya_secrets("over-cap payment response", &pay_bad);

        let final_state = subject_state(&client, &base, &maya, "maya").await;
        let final_ledger = final_state["recent_ledger"].as_array().cloned().unwrap_or_default();
        for entry in &final_ledger {
            assert_no_maya_secrets("final maya ledger entry", entry);
        }
        let payment_entry = final_ledger
            .into_iter()
            .find(|e| e["tool"] == json!("make_payment") && e["outcome"] == json!("released"))
            .expect("payment recorded as released in ledger");
        assert!(payment_entry["payload_returned"].to_string().contains("demo_pi_"));
        assert!(final_state["pending"].as_array().map(Vec::is_empty).unwrap_or(false));

        stop.store(true, Ordering::Relaxed);
        let _ = tokio::time::timeout(Duration::from_secs(2), approver).await;
        server.abort();
    })
    .await
    .expect("e2e completed within 55s");
}

#[tokio::test]
async fn phase6_rules_and_passkey_flows() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let app = build_app(&Options::offline(port)).expect("build_app");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let client = reqwest::Client::new();

        // 1. Unauthenticated requests to /api/passkey/register/begin must return 401
        let unauth_reg = client
            .post(format!("{base}/api/passkey/register/begin"))
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(unauth_reg.status(), StatusCode::UNAUTHORIZED);

        // 2. Public /api/passkey/login/begin returns options
        let login_opts = client
            .get(format!("{base}/api/passkey/login/begin"))
            .send()
            .await
            .unwrap();
        assert_eq!(login_opts.status(), StatusCode::OK);
        let login_json: Value = login_opts.json().await.unwrap();
        assert!(login_json["challenge"].as_str().is_some());

        // 3. Login as maya via demo wallet
        let maya = login(&client, &base, "maya").await;

        // 4. Authenticated /api/passkey/register/begin returns options
        let reg_opts = client
            .post(format!("{base}/api/passkey/register/begin"))
            .header(COOKIE, cookie_header(&maya))
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(reg_opts.status(), StatusCode::OK);
        let reg_json: Value = reg_opts.json().await.unwrap();
        assert_eq!(reg_json["user"]["name"], "maya");
        assert!(reg_json["challenge"].as_str().is_some());

        // 5. Standing rules CRUD
        let rules_list = client
            .get(format!("{base}/api/rules?subject=maya"))
            .header(COOKIE, cookie_header(&maya))
            .send()
            .await
            .unwrap();
        assert_eq!(rules_list.status(), StatusCode::OK);
        let rules_json: Value = rules_list.json().await.unwrap();
        assert_eq!(rules_json["rules"].as_array().unwrap().len(), 0);

        // Add standing rule: deny health data
        let add_rule = client
            .post(format!("{base}/api/rules?subject=maya"))
            .header(COOKIE, cookie_header(&maya))
            .header("content-type", "application/json")
            .body(
                json!({
                    "tool": "get_health_metrics",
                    "resource": "health",
                    "decision": {"action": "deny"},
                    "label": "Never share health data"
                })
                .to_string(),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(add_rule.status(), StatusCode::OK);
        let add_json: Value = add_rule.json().await.unwrap();
        assert_eq!(add_json["ok"], true);
        let rule_id = add_json["rule"]["rule_id"].as_str().unwrap().to_string();

        // 6. MCP call to get_health_metrics should now be rejected by rule immediately
        let probe = mcp(
            &client,
            &base,
            None,
            Some("maya"),
            "health_bot",
            "check insurance wellness score",
            30,
            "tools/call",
            json!({"name": "get_health_metrics", "arguments": {}}),
        )
        .await;
        assert_eq!(probe["result"]["isError"], true);
        let probe_text = probe["result"]["content"][0]["text"].as_str().unwrap_or_default();
        assert!(
            probe_text.contains("denied by your rule: Never share health data"),
            "expected rule denial text, got: {probe_text}"
        );

        // 7. Delete standing rule
        let del_rule = client
            .delete(format!("{base}/api/rules/{rule_id}?subject=maya"))
            .header(COOKIE, cookie_header(&maya))
            .send()
            .await
            .unwrap();
        assert_eq!(del_rule.status(), StatusCode::OK);

        let rules_after_del: Value = client
            .get(format!("{base}/api/rules?subject=maya"))
            .header(COOKIE, cookie_header(&maya))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(rules_after_del["rules"].as_array().unwrap().len(), 0);

        server.abort();
    })
    .await
    .expect("phase6 e2e completed within 30s");
}

#[tokio::test]
async fn chatgpt_plugin_and_actions() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let app = build_app(&Options::offline(port)).expect("build_app");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let client = reqwest::Client::new();

        // 1. ChatGPT plugin manifest
        let manifest_res = client.get(format!("{base}/.well-known/ai-plugin.json")).send().await.unwrap();
        assert_eq!(manifest_res.status(), StatusCode::OK);
        let manifest: Value = manifest_res.json().await.unwrap();
        assert_eq!(manifest["name_for_model"], "aegis_firewall");
        assert_eq!(manifest["schema_version"], "v1");

        // 2. OpenAPI 3.1.0 spec for Custom GPT Actions
        let openapi_res = client.get(format!("{base}/openapi.json")).send().await.unwrap();
        assert_eq!(openapi_res.status(), StatusCode::OK);
        let openapi: Value = openapi_res.json().await.unwrap();
        assert_eq!(openapi["openapi"], "3.1.0");
        assert!(openapi["paths"]["/api/chatgpt/pay"]["post"].is_object());
        assert!(openapi["paths"]["/api/chatgpt/affordability"]["post"].is_object());

        // 3. Login as maya and add a standing rule for clean-room financials
        let maya = login(&client, &base, "maya").await;
        let add_res = client
            .post(format!("{base}/api/rules?subject=maya"))
            .header(COOKIE, cookie_header(&maya))
            .header("content-type", "application/json")
            .body(
                json!({
                    "tool": "query_financials",
                    "resource": "financials",
                    "decision": {"action": "allow", "mode": "CLEAN_ROOM"},
                    "label": "Auto-verify affordability via clean room"
                })
                .to_string(),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(add_res.status(), StatusCode::OK);

        // 4. ChatGPT check affordability action (now auto-resolved via clean room rule)
        let afford_res = client
            .post(format!("{base}/api/chatgpt/affordability"))
            .header("content-type", "application/json")
            .header("Authorization", "Bearer aegis_sk_maya_4f9c2e7a1b8d")
            .body(json!({"amount_usd": 120.0}).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(afford_res.status(), StatusCode::OK);
        let afford_json: Value = afford_res.json().await.unwrap();
        assert_eq!(afford_json["can_afford"], true);

        // 5. ChatGPT check schedule action (redacted titles)
        let sched_res = client
            .post(format!("{base}/api/chatgpt/schedule"))
            .header("content-type", "application/json")
            .header("Authorization", "Bearer aegis_sk_maya_4f9c2e7a1b8d")
            .body(json!({}).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(sched_res.status(), StatusCode::OK);
        let sched_json: Value = sched_res.json().await.unwrap();
        let text = sched_json.to_string();
        assert!(!text.contains("Sequoia") && !text.contains("Oncology"));

        // 6. Test Vault Settings (delivery address & daily spending limit)
        let settings_res = client
            .get(format!("{base}/api/vault/settings?subject=maya"))
            .header(COOKIE, cookie_header(&maya))
            .send()
            .await
            .unwrap();
        assert_eq!(settings_res.status(), StatusCode::OK);
        let settings_json: Value = settings_res.json().await.unwrap();
        assert_eq!(settings_json["home_address"], "5521 Shafter Ave, Oakland, CA 94618");
        assert_eq!(settings_json["daily_limit_cents"], 300000);

        // Update daily limit to $100.00 and delivery address
        let update_res = client
            .post(format!("{base}/api/vault/settings?subject=maya"))
            .header(COOKIE, cookie_header(&maya))
            .header("content-type", "application/json")
            .body(
                json!({
                    "home_address": "742 Evergreen Terrace, Springfield, OR 97477",
                    "daily_limit_cents": 10000
                })
                .to_string(),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(update_res.status(), StatusCode::OK);
        let update_json: Value = update_res.json().await.unwrap();
        assert_eq!(update_json["home_address"], "742 Evergreen Terrace, Springfield, OR 97477");
        assert_eq!(update_json["daily_limit_usd"], 100.0);

        // 7. Order that exceeds daily limit ($150 > $100) -> blocked with 403
        let over_limit = client
            .post(format!("{base}/api/chatgpt/order"))
            .header("content-type", "application/json")
            .header("Authorization", "Bearer aegis_sk_maya_4f9c2e7a1b8d")
            .body(
                json!({
                    "item": "Dyson Airwrap",
                    "amount_usd": 150.0,
                    "merchant": "BestBuy"
                })
                .to_string(),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(over_limit.status(), StatusCode::FORBIDDEN);
        let over_json: Value = over_limit.json().await.unwrap();
        assert_eq!(over_json["status"], "blocked");
        assert!(over_json["error"].as_str().unwrap().contains("Daily spending limit of $100.00 exceeded"));

        // 8. Order within limit with a standing rule for auto-approving small purchases
        let _ = client
            .post(format!("{base}/api/rules?subject=maya"))
            .header(COOKIE, cookie_header(&maya))
            .header("content-type", "application/json")
            .body(
                json!({
                    "tool": "make_payment",
                    "resource": "financials",
                    "max_amount_cents": 5000,
                    "decision": {"action": "allow", "mode": "CLEAN_ROOM"},
                    "label": "Auto-approve purchases under $50"
                })
                .to_string(),
            )
            .send()
            .await
            .unwrap();

        let ok_order = client
            .post(format!("{base}/api/chatgpt/order"))
            .header("content-type", "application/json")
            .header("Authorization", "Bearer aegis_sk_maya_4f9c2e7a1b8d")
            .body(
                json!({
                    "item": "Desk Fan",
                    "amount_usd": 35.0,
                    "merchant": "Acme Hardware"
                })
                .to_string(),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(ok_order.status(), StatusCode::OK);
        let order_json: Value = ok_order.json().await.unwrap();
        assert_eq!(order_json["status"], "approved");
        assert_eq!(order_json["item"], "Desk Fan");
        // Verify street address was shielded from ChatGPT!
        assert!(!order_json.to_string().contains("742 Evergreen Terrace"));
        assert!(order_json["delivery"]["destination_region"].as_str().unwrap().contains("Springfield"));
        assert!(order_json["delivery"]["shipping_token"].as_str().unwrap().starts_with("aegis_ship_"));

        server.abort();
    })
    .await
    .expect("chatgpt action test completed within 30s");
}
