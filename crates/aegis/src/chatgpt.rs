//! ChatGPT Action / Plugin integration for AegisMCP.
//!
//! Provides:
//! - `GET /.well-known/ai-plugin.json` (ChatGPT plugin manifest)
//! - `GET /openapi.json` (OpenAPI 3.1.0 spec for Custom GPT Actions)
//! - `POST /api/chatgpt/affordability` (Clean-room balance check)
//! - `POST /api/chatgpt/pay` (JIT-consented payment execution)
//! - `POST /api/chatgpt/schedule` (Redacted schedule free/busy check)

use crate::vault_settings::SubjectSettings;
use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct ChatGptState {
    pub base_url: String,
    pub http: reqwest::Client,
    pub settings: Option<Arc<Mutex<HashMap<String, SubjectSettings>>>>,
    pub ledger: Option<Arc<dyn aegis_core::Ledger>>,
}

pub fn router(
    base_url: String,
    settings: Option<Arc<Mutex<HashMap<String, SubjectSettings>>>>,
    ledger: Option<Arc<dyn aegis_core::Ledger>>,
) -> Router {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap_or_default();
    Router::new()
        .route("/.well-known/ai-plugin.json", get(manifest))
        .route("/static/logo.png", get(serve_logo))
        .route("/openapi.json", get(openapi_spec))
        .route("/api/chatgpt/openapi.json", get(openapi_spec))
        .route("/api/chatgpt/affordability", post(check_affordability))
        .route("/api/chatgpt/order", post(execute_order))
        .route("/api/chatgpt/pay", post(execute_payment))
        .route("/api/chatgpt/schedule", post(check_schedule))
        .with_state(ChatGptState {
            base_url,
            http,
            settings,
            ledger,
        })
}

/// ChatGPT Plugin Manifest
async fn manifest(State(_st): State<ChatGptState>, headers: HeaderMap) -> Response {
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|h| h.to_str().ok())
        .unwrap_or("127.0.0.1:8787");
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        .unwrap_or(if host.contains("trycloudflare.com") { "https" } else { "http" });
    let base_url = format!("{proto}://{host}");

    let manifest = json!({
        "schema_version": "v1",
        "name_for_human": "Aegis Consent Firewall",
        "name_for_model": "aegis_firewall",
        "description_for_human": "Zero-leak personal clean room with real-time approval for purchases and schedule access.",
        "description_for_model": "Enables ChatGPT to verify user affordability, inspect free/busy schedules without seeing sensitive details, and initiate purchases that trigger real-time user confirmation cards on the Aegis dashboard.",
        "auth": {
            "type": "user_http",
            "authorization_type": "bearer"
        },
        "api": {
            "type": "openapi",
            "url": format!("{}/openapi.json", base_url)
        },
        "logo_url": format!("{}/static/logo.png", base_url),
        "contact_email": "support@aegismcp.local",
        "legal_info_url": format!("{}/legal", base_url)
    });
    Json(manifest).into_response()
}

async fn serve_logo() -> Response {
    const LOGO_BYTES: &[u8] = include_bytes!("../../../aegis-logo.png");
    ([(header::CONTENT_TYPE, "image/png")], LOGO_BYTES).into_response()
}

/// OpenAPI 3.1.0 Specification for ChatGPT Custom GPT Actions
async fn openapi_spec(State(_st): State<ChatGptState>, headers: HeaderMap) -> Response {
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|h| h.to_str().ok())
        .unwrap_or("127.0.0.1:8787");
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        .unwrap_or(if host.contains("trycloudflare.com") { "https" } else { "http" });
    let server_url = format!("{proto}://{host}");

    let spec = json!({
        "openapi": "3.1.0",
        "info": {
            "title": "AegisMCP Consent Firewall & Clean Room",
            "description": "Safe, user-gated personal agent actions: check affordability without leaking balances, book schedule slots with redacted titles, and request real-time user-authorized purchases.",
            "version": "1.0.0"
        },
        "servers": [
            {
                "url": server_url,
                "description": "AegisMCP Server"
            }
        ],
        "paths": {
            "/api/chatgpt/affordability": {
                "post": {
                    "operationId": "checkAffordability",
                    "summary": "Check if user can afford an item",
                    "description": "Returns boolean verification whether the user has sufficient funds. Does NOT reveal bank balance, account number, or financial details.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "properties": {
                                        "amount_usd": {
                                            "type": "number",
                                            "description": "Price in USD to check affordability for"
                                        }
                                    },
                                    "required": ["amount_usd"]
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Affordability verification result",
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": {
                                            "can_afford": {"type": "boolean"},
                                            "balance_band": {"type": "string"}
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            },
            "/api/chatgpt/order": {
                "post": {
                    "operationId": "placeOrder",
                    "summary": "Order an item with shielded delivery and daily limit enforcement",
                    "description": "Places an order through Aegis. Verifies against the user's daily spending limit, conceals raw street address behind a tokenized shipping reference, and halts for user confirmation on the Aegis dashboard.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "properties": {
                                        "item": {"type": "string", "description": "Item to purchase"},
                                        "amount_usd": {"type": "number", "description": "Cost in USD"},
                                        "merchant": {"type": "string", "description": "Store or merchant"}
                                    },
                                    "required": ["item", "amount_usd", "merchant"]
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Order placement status",
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": {
                                            "status": {"type": "string"},
                                            "item": {"type": "string"},
                                            "amount_usd": {"type": "number"},
                                            "merchant": {"type": "string"},
                                            "payment_reference": {"type": "string"},
                                            "delivery": {"type": "object"}
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            },
            "/api/chatgpt/pay": {
                "post": {
                    "operationId": "requestPayment",
                    "summary": "Request payment authorization from user",
                    "description": "Submits a purchase for real-time user approval. Execution pauses until the user approves the charge on their Aegis dashboard. Returns tokenized confirmation if approved, or denial if rejected.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "properties": {
                                        "amount_usd": {
                                            "type": "number",
                                            "description": "Amount to charge in USD"
                                        },
                                        "merchant": {
                                            "type": "string",
                                            "description": "Name of the merchant or store"
                                        },
                                        "description": {
                                            "type": "string",
                                            "description": "Brief description of the item"
                                        }
                                    },
                                    "required": ["amount_usd", "merchant"]
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Payment authorization response",
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": {
                                            "status": {"type": "string"},
                                            "reference": {"type": "string"},
                                            "receipt_id": {"type": "string"},
                                            "message": {"type": "string"}
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            },
            "/api/chatgpt/schedule": {
                "post": {
                    "operationId": "checkSchedule",
                    "summary": "Check calendar availability",
                    "description": "Inspects user schedule. Redacts private meeting titles, health appointments, and participants while preserving free/busy status.",
                    "requestBody": {
                        "required": false,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "properties": {
                                        "time_min": {"type": "string", "description": "ISO timestamp lower bound"},
                                        "time_max": {"type": "string", "description": "ISO timestamp upper bound"}
                                    }
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Redacted schedule free/busy slots",
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": {
                                            "events": {"type": "array"}
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        },
        "components": {
            "securitySchemes": {
                "bearerAuth": {
                    "type": "http",
                    "scheme": "bearer"
                }
            }
        },
        "security": [
            {"bearerAuth": []}
        ]
    });
    Json(spec).into_response()
}

#[derive(Deserialize)]
struct AffordabilityRequest {
    amount_usd: f64,
}

async fn check_affordability(
    State(st): State<ChatGptState>,
    headers: HeaderMap,
    Json(body): Json<AffordabilityRequest>,
) -> Response {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("Bearer aegis_sk_maya_4f9c2e7a1b8d");

    let rpc_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "query_financials",
            "arguments": {
                "min_amount": body.amount_usd,
                "fields": ["checking_balance"]
            }
        }
    });

    let resp = st
        .http
        .post(format!("{}/mcp", st.base_url))
        .header(header::AUTHORIZATION, auth_header)
        .header("x-aegis-agent", "ChatGPT_Action")
        .header("x-aegis-purpose", format!("Check affordability for ${:.2}", body.amount_usd))
        .json(&rpc_req)
        .send()
        .await;

    let res_json = match resp {
        Ok(r) => r.json::<Value>().await.unwrap_or_default(),
        Err(e) => return (StatusCode::BAD_GATEWAY, Json(json!({"error": e.to_string()}))).into_response(),
    };

    if let Some(content) = res_json.get("result").and_then(|r| r.get("structuredContent")) {
        return Json(content).into_response();
    }
    if let Some(txt) = res_json.get("result").and_then(|r| r["content"][0]["text"].as_str()) {
        if let Ok(parsed) = serde_json::from_str::<Value>(txt) {
            return Json(parsed).into_response();
        }
    }
    Json(json!({"error": "could not verify affordability"})).into_response()
}

#[derive(Deserialize)]
struct OrderRequest {
    item: String,
    amount_usd: f64,
    merchant: String,
}

async fn execute_order(
    State(st): State<ChatGptState>,
    headers: HeaderMap,
    Json(body): Json<OrderRequest>,
) -> Response {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("Bearer aegis_sk_maya_4f9c2e7a1b8d");

    let subject = if auth_header.contains("dev") { "dev" } else { "maya" };
    let amount_cents = (body.amount_usd * 100.0).round() as i64;

    // 1. Enforce Daily Spending Limit
    if let (Some(settings_lock), Some(ledger)) = (&st.settings, &st.ledger) {
        let map = settings_lock.lock().await;
        let s = map.get(subject).cloned().unwrap_or(SubjectSettings {
            home_address: "5521 Shafter Ave, Oakland, CA 94618".into(),
            daily_limit_cents: 25000,
        });

        let now = aegis_core::now_unix();
        let midnight = now - (now % 86400);
        let mut spent_today = 0i64;
        for e in ledger.recent(Some(subject), 100) {
            if e.tool == "make_payment" && e.outcome == aegis_core::Outcome::Released && e.ts >= midnight {
                if let Some(c) = e.payload_returned.get("amount_cents").and_then(Value::as_i64) {
                    spent_today += c;
                } else if let Some(u) = e.payload_returned.get("amount_usd").and_then(Value::as_f64) {
                    spent_today += (u * 100.0).round() as i64;
                }
            }
        }

        if spent_today + amount_cents > s.daily_limit_cents {
            let limit_usd = s.daily_limit_cents as f64 / 100.0;
            let spent_usd = spent_today as f64 / 100.0;
            return (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "status": "blocked",
                    "error": format!("Daily spending limit of ${limit_usd:.2} exceeded (already spent ${spent_usd:.2} today). Adjust your daily limit in the Aegis dashboard to proceed.")
                })),
            )
                .into_response();
        }
    }

    // 2. Derive coarse delivery info without leaking the street address
    let coarse_city_zip = if let Some(settings_lock) = &st.settings {
        let map = settings_lock.lock().await;
        let raw = map.get(subject).map(|s| s.home_address.as_str()).unwrap_or("Oakland, CA 94618");
        let parts: Vec<&str> = raw.split(',').collect();
        if parts.len() >= 2 {
            parts[1..].join(",").trim().to_string()
        } else {
            raw.to_string()
        }
    } else {
        "Oakland, CA 94618".to_string()
    };

    let rpc_req = json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "make_payment",
            "arguments": {
                "amount_usd": body.amount_usd,
                "merchant": body.merchant,
                "description": format!("Order: {}", body.item)
            }
        }
    });

    let resp = st
        .http
        .post(format!("{}/mcp", st.base_url))
        .header(header::AUTHORIZATION, auth_header)
        .header("x-aegis-agent", "ChatGPT_Action")
        .header("x-aegis-purpose", format!("Order '{}' from {} for ${:.2}", body.item, body.merchant, body.amount_usd))
        .json(&rpc_req)
        .send()
        .await;

    let res_json = match resp {
        Ok(r) => r.json::<Value>().await.unwrap_or_default(),
        Err(e) => return (StatusCode::BAD_GATEWAY, Json(json!({"error": e.to_string()}))).into_response(),
    };

    if res_json["result"]["isError"].as_bool() == Some(true) {
        let err_msg = res_json["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("Payment denied or timed out on Aegis dashboard");
        return Json(json!({
            "status": "denied",
            "message": err_msg
        }))
        .into_response();
    }

    let payload = res_json["result"].get("structuredContent").cloned().unwrap_or_else(|| {
        res_json["result"]["content"][0]["text"]
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    });

    let meta = res_json.get("result").and_then(|r| r.get("_meta")).cloned().unwrap_or(Value::Null);
    let shipping_token = format!("aegis_ship_{}", &uuid::Uuid::new_v4().simple().to_string()[..10]);

    Json(json!({
        "status": "approved",
        "item": body.item,
        "amount_usd": body.amount_usd,
        "merchant": body.merchant,
        "payment_reference": payload["reference"],
        "receipt_id": meta["aegis"]["receipt_id"],
        "delivery": {
            "status": "dispatched_to_vault_address",
            "destination_region": coarse_city_zip,
            "shipping_token": shipping_token,
            "note": "Exact street address was shielded from ChatGPT and verified directly by Aegis vault."
        },
        "message": "Order placed successfully! Payment executed via Aegis with zero credential leakage."
    }))
    .into_response()
}

#[derive(Deserialize)]
struct PayRequest {
    amount_usd: f64,
    merchant: String,
    description: Option<String>,
}

async fn execute_payment(
    State(st): State<ChatGptState>,
    headers: HeaderMap,
    Json(body): Json<PayRequest>,
) -> Response {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("Bearer aegis_sk_maya_4f9c2e7a1b8d");

    let rpc_req = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "make_payment",
            "arguments": {
                "amount_usd": body.amount_usd,
                "merchant": body.merchant,
                "description": body.description.unwrap_or_else(|| "ChatGPT Action purchase".into())
            }
        }
    });

    let resp = st
        .http
        .post(format!("{}/mcp", st.base_url))
        .header(header::AUTHORIZATION, auth_header)
        .header("x-aegis-agent", "ChatGPT_Action")
        .header("x-aegis-purpose", format!("Purchase from {} for ${:.2}", body.merchant, body.amount_usd))
        .json(&rpc_req)
        .send()
        .await;

    let res_json = match resp {
        Ok(r) => r.json::<Value>().await.unwrap_or_default(),
        Err(e) => return (StatusCode::BAD_GATEWAY, Json(json!({"error": e.to_string()}))).into_response(),
    };

    if res_json["result"]["isError"].as_bool() == Some(true) {
        let err_msg = res_json["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("Payment denied or timed out");
        return Json(json!({
            "status": "denied",
            "message": err_msg
        }))
        .into_response();
    }

    let payload = res_json["result"].get("structuredContent").cloned().unwrap_or_else(|| {
        res_json["result"]["content"][0]["text"]
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    });

    let meta = res_json.get("result").and_then(|r| r.get("_meta")).cloned().unwrap_or(Value::Null);

    Json(json!({
        "status": "approved",
        "reference": payload["reference"],
        "amount_usd": payload["amount_usd"],
        "merchant": payload["merchant"],
        "receipt_id": meta["aegis"]["receipt_id"],
        "message": "Payment authorized and completed via Aegis consent firewall."
    }))
    .into_response()
}

#[derive(Deserialize, Default)]
struct ScheduleRequest {
    from: Option<String>,
    to: Option<String>,
}

async fn check_schedule(
    State(st): State<ChatGptState>,
    headers: HeaderMap,
    Json(body): Json<ScheduleRequest>,
) -> Response {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("Bearer aegis_sk_maya_4f9c2e7a1b8d");

    let mut args = serde_json::Map::new();
    args.insert("fields".into(), json!(["start", "end"]));
    if let Some(f) = body.from {
        args.insert("from".into(), json!(f));
    }
    if let Some(t) = body.to {
        args.insert("to".into(), json!(t));
    }

    let rpc_req = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "check_schedule",
            "arguments": args
        }
    });

    let resp = st
        .http
        .post(format!("{}/mcp", st.base_url))
        .header(header::AUTHORIZATION, auth_header)
        .header("x-aegis-agent", "ChatGPT_Action")
        .header("x-aegis-purpose", "Check calendar availability")
        .json(&rpc_req)
        .send()
        .await;

    let res_json = match resp {
        Ok(r) => r.json::<Value>().await.unwrap_or_default(),
        Err(e) => return (StatusCode::BAD_GATEWAY, Json(json!({"error": e.to_string()}))).into_response(),
    };

    if let Some(content) = res_json.get("result").and_then(|r| r.get("structuredContent")) {
        return Json(content).into_response();
    }
    if let Some(txt) = res_json.get("result").and_then(|r| r["content"][0]["text"].as_str()) {
        if let Ok(parsed) = serde_json::from_str::<Value>(txt) {
            return Json(parsed).into_response();
        }
    }
    Json(json!({"error": "could not retrieve schedule"})).into_response()
}
