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

async fn login(client: &reqwest::Client, base: &str, subject: &str, password: &str) -> String {
    let resp = client
        .post(format!("{base}/api/login"))
        .json(&json!({"subject": subject, "password": password}))
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

        let maya = login(&client, &base, "maya", "maya-demo").await;
        let dev = login(&client, &base, "dev", "dev-demo").await;
        let bad = post_json(
            &client,
            &base,
            "/api/login",
            None,
            &json!({"subject": "maya", "password": "nope"}),
        )
        .await;
        assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);

        let maya_me: Value = get(&client, &base, "/api/me", Some(&maya)).await.json().await.unwrap();
        assert_eq!(maya_me["subject"], json!("maya"));
        assert_eq!(maya_me["mcp_token"], json!("aegis_sk_maya_4f9c2e7a1b8d"));

        let stop = Arc::new(AtomicBool::new(false));
        let approver = {
            let stop = stop.clone();
            let client = client.clone();
            let base = base.clone();
            let header = cookie_header(&maya);
            tokio::spawn(async move {
                while !stop.load(Ordering::Relaxed) {
                    if let Some(pending) = get(&client, &base, "/api/state?subject=maya", Some(&header))
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
                                Some(&header),
                                &decision,
                            )
                            .await;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
            })
        };

        eprintln!("CK: pre-scenarios");
        let scenarios = Scenarios::new(None);
        let _ = tokio::join!(
            scenarios.run(1, "maya".to_string(), base.clone()),
            scenarios.run(2, "maya".to_string(), base.clone()),
        );

        eprintln!("CK: post-join");
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

        eprintln!("CK: post-ledger-asserts");
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

        eprintln!("CK: post-dev-403");
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

        eprintln!("CK: pre-payment");
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

        eprintln!("CK: post-pay-ok");
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

        eprintln!("CK: end");
        stop.store(true, Ordering::Relaxed);
        let _ = tokio::time::timeout(Duration::from_secs(2), approver).await;
        server.abort();
    })
    .await
    .expect("e2e completed within 55s");
}
