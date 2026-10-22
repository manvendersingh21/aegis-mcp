use aegis_core::{
    demo_vaults, now_unix, Ledger, LedgerEntry, Mode, Outcome, Receipt, ReceiptClaims,
    ReceiptSigner, Resource,
};
use aegis_receipts::{Ed25519Signer, SqliteLedger};
use aegis_store::{MirroredLedger, SupabaseStore, WalletStore};
use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::any,
    Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[allow(dead_code)]
#[derive(Clone, Debug)]
struct RecordedRequest {
    method: String,
    path: String,
    table: String,
    query: String,
    headers: HeaderMap,
    body: Value,
    raw_body: String,
}

#[derive(Clone, Default)]
struct StubState {
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    canned_selects: Arc<Mutex<HashMap<(String, String), Vec<Value>>>>,
}

impl StubState {
    fn set_canned(&self, table: &str, query: &str, rows: Vec<Value>) {
        self.canned_selects
            .lock()
            .unwrap()
            .insert((table.to_string(), query.to_string()), rows);
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }
}

async fn stub_handler(State(state): State<StubState>, req: Request) -> Response {
    let method = req.method().to_string();
    let uri = req.uri().clone();
    let path = uri.path().to_string();
    let query = uri.query().unwrap_or("").to_string();
    let headers = req.headers().clone();

    // Table is the last path component (handles /rest/v1/{table} and /{table})
    let table = path.rsplit('/').next().unwrap_or("").to_string();

    let body_bytes = axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap_or_default();
    let raw_body = String::from_utf8_lossy(&body_bytes).to_string();
    let body_val: Value = serde_json::from_str(&raw_body).unwrap_or(Value::Null);

    let rec = RecordedRequest {
        method: method.clone(),
        path,
        table: table.clone(),
        query: query.clone(),
        headers,
        body: body_val,
        raw_body,
    };
    state.requests.lock().unwrap().push(rec);

    if method == "GET" {
        let canned = state.canned_selects.lock().unwrap();
        if let Some(rows) = canned.get(&(table.clone(), query.clone())) {
            return (StatusCode::OK, Json(json!(rows))).into_response();
        }
        if let Some(rows) = canned.get(&(table, String::new())) {
            return (StatusCode::OK, Json(json!(rows))).into_response();
        }
        return (StatusCode::OK, Json(json!([]))).into_response();
    }

    (StatusCode::CREATED, Json(json!({"ok": true}))).into_response()
}

async fn spawn_stub() -> (String, StubState) {
    let state = StubState::default();
    let app = Router::new()
        .route("/{*path}", any(stub_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), state)
}

async fn wait_for_request<F>(
    state: &StubState,
    timeout: Duration,
    predicate: F,
) -> Option<RecordedRequest>
where
    F: Fn(&RecordedRequest) -> bool,
{
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if let Some(req) = state.requests().into_iter().find(|r| predicate(r)) {
            return Some(req);
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    None
}

fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[4 * i],
                block[4 * i + 1],
                block[4 * i + 2],
                block[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(64);
    for b in out {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

fn make_test_entry(subject: &str, call_id: &str) -> LedgerEntry {
    LedgerEntry {
        id: aegis_core::new_id("entry"),
        ts: now_unix(),
        subject: subject.to_string(),
        call_id: call_id.to_string(),
        agent_id: "agent_stub".to_string(),
        tool: "query_financials".to_string(),
        purpose: "offline stub testing".to_string(),
        raw_requested: vec!["checking_balance".to_string()],
        policy_applied: Mode::CleanRoom,
        outcome: Outcome::Released,
        payload_returned: json!({"can_afford": true}),
        bytes_raw: 120,
        bytes_released: 35,
        receipt_id: None,
        entry_hash: String::new(),
    }
}

fn make_test_receipt(subject: &str, ttl_secs: i64) -> Receipt {
    let signer = Ed25519Signer::from_seed([42u8; 32]);
    let now = now_unix();
    signer.issue(ReceiptClaims {
        receipt_id: aegis_core::new_id("rcpt"),
        subject: subject.to_string(),
        grantee_agent: "agent_stub".to_string(),
        purpose: "offline stub testing".to_string(),
        resource: Resource::Financials,
        resolution_mode: Mode::CleanRoom,
        allowed_fields: vec!["checking_balance".to_string()],
        issued_at: now,
        expires_at: now + ttl_secs,
    })
}

// ---------------------------------------------------------------- tests ----

#[tokio::test]
async fn test_headers_include_apikey_and_bearer() {
    let (url, stub) = spawn_stub().await;
    let service_key = "aegis-secret-service-key-999";
    let store = SupabaseStore::new(&url, service_key);

    // 1. select
    let _ = store
        .select("aegis_wallets", "order=linked_at.asc")
        .await
        .unwrap();

    // 2. upsert
    store
        .upsert(
            "aegis_wallets",
            &json!({"address": "0x123", "subject": "maya"}),
            "address",
        )
        .await
        .unwrap();

    // 3. update
    store
        .update(
            "aegis_wallets",
            "address=eq.0x123",
            &json!({"kind": "demo"}),
        )
        .await
        .unwrap();

    let requests = stub.requests();
    assert_eq!(requests.len(), 3, "Expected 3 recorded requests");

    for req in requests {
        let apikey = req
            .headers
            .get("apikey")
            .expect("missing apikey header")
            .to_str()
            .unwrap();
        assert_eq!(apikey, service_key);

        let auth = req
            .headers
            .get("authorization")
            .expect("missing authorization header")
            .to_str()
            .unwrap();
        assert_eq!(auth, format!("Bearer {service_key}"));
    }
}

#[tokio::test]
async fn test_upsert_sends_prefer_merge_duplicates_and_on_conflict() {
    let (url, stub) = spawn_stub().await;
    let store = SupabaseStore::new(&url, "key");

    let payload = json!({"seq": 42, "subject": "maya", "entry_hash": "hash42"});
    store.upsert("aegis_ledger", &payload, "seq").await.unwrap();

    let req = wait_for_request(&stub, Duration::from_secs(1), |r| {
        r.table == "aegis_ledger" && r.method == "POST"
    })
    .await
    .expect("upsert request should be recorded");

    // Verify on_conflict in query string
    assert!(
        req.query.contains("on_conflict=seq"),
        "Expected query to contain 'on_conflict=seq', got: {}",
        req.query
    );

    // Verify Prefer header
    let prefer = req
        .headers
        .get("prefer")
        .expect("missing prefer header")
        .to_str()
        .unwrap();
    assert!(
        prefer.contains("resolution=merge-duplicates"),
        "Expected prefer header to contain resolution=merge-duplicates, got: {prefer}"
    );
    assert!(
        prefer.contains("return=minimal"),
        "Expected prefer header to contain return=minimal, got: {prefer}"
    );

    assert_eq!(req.body["seq"], 42);
    assert_eq!(req.body["subject"], "maya");
}

#[tokio::test]
async fn test_mirrored_ledger_append_returns_immediately_and_eventually_mirrored() {
    let (url, stub) = spawn_stub().await;
    let store = SupabaseStore::new(&url, "service-key");
    let inner = SqliteLedger::open_in_memory().unwrap();
    let mirrored = MirroredLedger::new(inner, Some(store));

    let entry = make_test_entry("maya", "call_bench_1");

    let start = std::time::Instant::now();
    let stored = mirrored.append(entry).unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(100),
        "mirrored.append must return immediately without awaiting HTTP (took {elapsed:?})"
    );
    assert!(
        !stored.entry_hash.is_empty(),
        "stored entry must have computed entry_hash"
    );

    // Eventually the stub receives an aegis_ledger row with the same entry_hash
    let req = wait_for_request(&stub, Duration::from_secs(3), |r| {
        r.table == "aegis_ledger" && r.method == "POST"
    })
    .await
    .expect("stub should receive mirrored aegis_ledger row");

    // Identity is the chain hash (a fresh local db must not overwrite history),
    // and the hosted mirror never carries the released payload in cleartext.
    assert!(req.query.contains("on_conflict=entry_hash"), "{}", req.query);
    assert_eq!(req.body["entry"]["payload_returned"]["mirrored"], "hash-only");
    assert_eq!(
        req.body["entry_hash"].as_str().unwrap(),
        stored.entry_hash,
        "mirrored row entry_hash must match stored entry_hash"
    );
    assert_eq!(req.body["subject"], stored.subject);
    assert_eq!(req.body["seq"], 1);
    assert_eq!(
        req.body["entry"]["entry_hash"].as_str().unwrap(),
        stored.entry_hash
    );
    assert_eq!(req.body["entry"]["call_id"], "call_bench_1");
}

#[tokio::test]
async fn test_receipts_mirrored_on_store_receipt_and_revoke_all() {
    let (url, stub) = spawn_stub().await;
    let store = SupabaseStore::new(&url, "service-key");
    let inner = SqliteLedger::open_in_memory().unwrap();
    let mirrored = MirroredLedger::new(inner, Some(store));

    // 1. store_receipt
    let rcpt = make_test_receipt("maya", 300);
    mirrored.store_receipt(&rcpt).unwrap();

    let req1 = wait_for_request(&stub, Duration::from_secs(3), |r| {
        r.table == "aegis_receipts"
            && r.body["receipt_id"] == rcpt.claims.receipt_id
            && r.body["revoked"] == false
    })
    .await
    .expect("receipt should be mirrored on store_receipt");

    assert!(req1.query.contains("on_conflict=receipt_id"));
    assert_eq!(req1.body["subject"], "maya");
    assert_eq!(req1.body["resource"], "financials");
    assert_eq!(
        req1.body["receipt"]["receipt_id"],
        rcpt.claims.receipt_id
    );

    // 2. revoke_all
    let revoked = mirrored.revoke_all(Some("maya"));
    assert_eq!(revoked, vec![rcpt.claims.receipt_id.clone()]);

    let req2 = wait_for_request(&stub, Duration::from_secs(3), |r| {
        r.table == "aegis_receipts"
            && r.body["receipt_id"] == rcpt.claims.receipt_id
            && r.body["revoked"] == true
    })
    .await
    .expect("revoked receipt should be mirrored on revoke_all");

    assert_eq!(req2.body["revoked"], true);
}

#[test]
fn test_supabase_down_never_panics_or_blocks_ledger() {
    // Port 1 is tcpmux, refused immediately
    let store = SupabaseStore::new("http://127.0.0.1:1", "test-key");
    let inner = SqliteLedger::open_in_memory().unwrap();
    let mirrored = MirroredLedger::new(inner, Some(store));

    let start = std::time::Instant::now();
    for i in 0..10 {
        let entry = make_test_entry("maya", &format!("call_down_{i}"));
        mirrored.append(entry).unwrap();
    }
    let rcpt = make_test_receipt("maya", 60);
    mirrored.store_receipt(&rcpt).unwrap();
    mirrored.revoke_all(Some("maya"));
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(500),
        "Supabase down must never block hot path (took {elapsed:?})"
    );
    assert_eq!(mirrored.recent(None, 20).len(), 10);
    assert!(mirrored.verify_chain());
}

#[tokio::test]
async fn test_rehydrate_loads_rows_into_empty_sqlite() {
    let (url, stub) = spawn_stub().await;
    let store = SupabaseStore::new(&url, "key");

    let e1 = make_test_entry("maya", "call_rehydrate_1");
    let e2 = make_test_entry("maya", "call_rehydrate_2");
    let rcpt = make_test_receipt("maya", 600);

    // Seed canned selects
    let ledger_rows = vec![
        json!({
            "seq": 1,
            "subject": "maya",
            "entry_hash": "dummy_hash_1",
            "entry": e1,
        }),
        json!({
            "seq": 2,
            "subject": "maya",
            "entry_hash": "dummy_hash_2",
            "entry": e2,
        }),
    ];
    let receipt_rows = vec![json!({
        "receipt_id": rcpt.claims.receipt_id,
        "subject": "maya",
        "grantee_agent": "agent_stub",
        "resource": "financials",
        "expires_at": rcpt.claims.expires_at,
        "revoked": false,
        "receipt": rcpt,
    })];

    stub.set_canned("aegis_ledger", "order=seq.asc", ledger_rows);
    stub.set_canned("aegis_receipts", "order=receipt_id.asc", receipt_rows);

    let inner = SqliteLedger::open_in_memory().unwrap();
    let mirrored = MirroredLedger::new(inner, Some(store));

    // Initially empty
    assert_eq!(mirrored.recent(None, 10).len(), 0);

    // Rehydrate
    mirrored.rehydrate().await.unwrap();

    // Verify ledger loaded
    let recent = mirrored.recent(None, 10);
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[1].call_id, "call_rehydrate_1");
    assert_eq!(recent[0].call_id, "call_rehydrate_2");
    assert!(mirrored.verify_chain());

    // Verify receipts loaded
    let active = mirrored.active_receipts(Some("maya"), now_unix());
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].claims.receipt_id, rcpt.claims.receipt_id);
    assert!(mirrored.get_receipt(&rcpt.claims.receipt_id).is_some());

    // Calling rehydrate on non-empty ledger is a no-op (no duplicates)
    mirrored.rehydrate().await.unwrap();
    assert_eq!(mirrored.recent(None, 10).len(), 2);
}

#[tokio::test]
async fn test_wallet_store_seed_people_never_sends_raw_mcp_token() {
    let (url, stub) = spawn_stub().await;
    let store = SupabaseStore::new(&url, "key");
    let wallet_store = WalletStore::new(store);

    let vaults = demo_vaults();
    assert!(!vaults.is_empty());
    let raw_tokens: Vec<String> = vaults.iter().map(|v| v.mcp_token()).collect();
    for token in &raw_tokens {
        assert!(!token.is_empty(), "demo vault must have non-empty mcp token");
    }

    wallet_store.seed_people(&vaults).await.unwrap();

    let req = wait_for_request(&stub, Duration::from_secs(2), |r| {
        r.table == "aegis_people" && r.method == "POST"
    })
    .await
    .expect("seed_people should upsert to aegis_people");

    assert!(req.query.contains("on_conflict=subject"));

    // Check raw_body to make sure raw tokens NEVER leave the box
    for token in &raw_tokens {
        assert!(
            !req.raw_body.contains(token),
            "CRITICAL SECURITY VIOLATION: Raw MCP token '{token}' found in request body!"
        );
    }

    // Verify sha256 hashes in body
    let rows = req.body.as_array().expect("body should be an array of people");
    assert_eq!(rows.len(), vaults.len());

    for (row, vault) in rows.iter().zip(vaults.iter()) {
        assert_eq!(row["subject"], vault.subject());
        assert_eq!(row["display_name"], vault.person().display_name);
        assert_eq!(row["max_payment_cents"], vault.max_payment_cents());

        let expected_hash = sha256_hex(vault.mcp_token().as_bytes());
        assert_eq!(
            row["mcp_token_sha256"].as_str().unwrap(),
            expected_hash,
            "mcp_token_sha256 must match sha256_hex of raw token"
        );
    }
}

#[tokio::test]
async fn test_wallet_store_save_and_load() {
    let (url, stub) = spawn_stub().await;
    let store = SupabaseStore::new(&url, "key");
    let wallet_store = WalletStore::new(store);

    // 1. save
    wallet_store
        .save("maya", "0x000000000000000000000000000000000000dEaD", "user")
        .await
        .unwrap();

    let req = wait_for_request(&stub, Duration::from_secs(2), |r| {
        r.table == "aegis_wallets" && r.method == "POST"
    })
    .await
    .expect("wallet save should upsert to aegis_wallets");

    assert!(req.query.contains("on_conflict=address"));
    assert_eq!(
        req.body["address"],
        "0x000000000000000000000000000000000000dEaD"
    );
    assert_eq!(req.body["subject"], "maya");
    assert_eq!(req.body["kind"], "user");

    // 2. load
    stub.set_canned(
        "aegis_wallets",
        "order=linked_at.asc",
        vec![
            json!({"address": "0x111", "subject": "maya", "kind": "user"}),
            json!({"address": "0x222", "subject": "dev", "kind": "demo"}),
        ],
    );

    let loaded = wallet_store.load().await.unwrap();
    assert_eq!(
        loaded,
        vec![
            ("maya".to_string(), "0x111".to_string(), "user".to_string()),
            ("dev".to_string(), "0x222".to_string(), "demo".to_string()),
        ]
    );
}
