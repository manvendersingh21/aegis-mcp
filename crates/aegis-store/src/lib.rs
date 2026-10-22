//! aegis-store: Supabase (PostgREST) system of record for AegisMCP.
//!
//! - [`SupabaseStore`]: thin async PostgREST client (apikey + service-key
//!   Bearer auth) with an injectable base URL so tests can point it at a stub.
//! - [`MirroredLedger`]: `aegis_core::Ledger` over `aegis_receipts::SqliteLedger`
//!   (SQLite stays the hot path) that write-through mirrors receipts and
//!   ledger rows into Supabase on a background writer task. Mirroring never
//!   blocks or panics the caller; failures are logged as warnings.
//! - [`WalletStore`]: linked wallets + people seeding (`aegis_wallets`,
//!   `aegis_people`; only a sha256 of the MCP token ever leaves the box).

use aegis_core::{
    AegisError, Ledger, LedgerEntry, Metrics, Receipt, Resource, Vault,
};
use aegis_receipts::SqliteLedger;
use serde_json::{json, Value};
use std::sync::Mutex;
use tokio::sync::mpsc;

// ---------------------------------------------------------------- sha256 ----

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Dependency-free SHA-256 (no `sha2` crate in this crate's Cargo.toml).
fn sha256(data: &[u8]) -> [u8; 32] {
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
                .wrapping_add(SHA256_K[i])
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
    out
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

fn sha256_hex(data: &[u8]) -> String {
    hex_encode(&sha256(data))
}

fn storage_err(context: &str, err: impl std::fmt::Display) -> AegisError {
    AegisError::Storage(format!("{context}: {err}"))
}

// -------------------------------------------------------- SupabaseStore ----

/// Minimal async PostgREST client. The base URL is injectable so tests can
/// point it at a local stub instead of a real Supabase project.
#[derive(Clone, Debug)]
pub struct SupabaseStore {
    base_url: String,
    service_key: String,
    client: reqwest::Client,
}

impl SupabaseStore {
    pub fn new(url: impl Into<String>, service_key: impl Into<String>) -> Self {
        let base_url = url.into().trim_end_matches('/').to_string();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        Self {
            base_url,
            service_key: service_key.into(),
            client,
        }
    }

    /// From `SUPABASE_URL` + `SUPABASE_SECRET_KEY`; `None` if either is unset.
    pub fn from_env() -> Option<Self> {
        let url = std::env::var("SUPABASE_URL").ok()?;
        let key = std::env::var("SUPABASE_SECRET_KEY").ok()?;
        if url.trim().is_empty() || key.trim().is_empty() {
            return None;
        }
        Some(Self::new(url, key))
    }

    fn rest_url(&self, table: &str) -> String {
        format!("{}/rest/v1/{table}", self.base_url)
    }

    fn auth_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(v) = reqwest::header::HeaderValue::from_str(&self.service_key) {
            headers.insert("apikey", v);
        }
        if let Ok(v) = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", self.service_key)) {
            headers.insert("authorization", v);
        }
        headers
    }

    async fn check_ok(
        resp: reqwest::Response,
        table: &str,
        what: &str,
    ) -> Result<(), AegisError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        let body: String = body.chars().take(300).collect();
        Err(storage_err(
            &format!("supabase {what} on {table}"),
            format!("HTTP {status}: {body}"),
        ))
    }

    /// Upsert one object or an array of objects (`Prefer:
    /// resolution=merge-duplicates,return=minimal`, conflict target
    /// `on_conflict`).
    pub async fn upsert(
        &self,
        table: &str,
        body: &Value,
        on_conflict: &str,
    ) -> Result<(), AegisError> {
        let url = format!("{}?on_conflict={on_conflict}", self.rest_url(table));
        let resp = self
            .client
            .post(&url)
            .headers(self.auth_headers())
            .header("prefer", "resolution=merge-duplicates,return=minimal")
            .json(body)
            .send()
            .await
            .map_err(|e| storage_err(&format!("supabase upsert on {table}"), e))?;
        Self::check_ok(resp, table, "upsert").await
    }

    /// PostgREST `GET` with the query string passed verbatim
    /// (e.g. `"subject=eq.maya&order=seq.asc"`).
    pub async fn select(&self, table: &str, query: &str) -> Result<Vec<Value>, AegisError> {
        let url = if query.is_empty() {
            self.rest_url(table)
        } else {
            format!("{}?{query}", self.rest_url(table))
        };
        let resp = self
            .client
            .get(&url)
            .headers(self.auth_headers())
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| storage_err(&format!("supabase select on {table}"), e))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let body: String = body.chars().take(300).collect();
            return Err(storage_err(
                &format!("supabase select on {table}"),
                format!("HTTP {status}: {body}"),
            ));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| storage_err(&format!("supabase select on {table} body"), e))?;
        serde_json::from_str(&text)
            .map_err(|e| storage_err(&format!("supabase select on {table} decode"), e))
    }

    /// PostgREST `PATCH` with a filter query string.
    pub async fn update(
        &self,
        table: &str,
        query: &str,
        body: &Value,
    ) -> Result<(), AegisError> {
        let url = if query.is_empty() {
            self.rest_url(table)
        } else {
            format!("{}?{query}", self.rest_url(table))
        };
        let resp = self
            .client
            .patch(&url)
            .headers(self.auth_headers())
            .header("prefer", "resolution=merge-duplicates,return=minimal")
            .json(body)
            .send()
            .await
            .map_err(|e| storage_err(&format!("supabase update on {table}"), e))?;
        Self::check_ok(resp, table, "update").await
    }
}

// ------------------------------------------------------- MirroredLedger ----

/// One queued write-through to Supabase.
enum MirrorOp {
    Ledger {
        seq: i64,
        subject: String,
        entry_hash: String,
        entry: Value,
    },
    Receipt {
        row: Value,
    },
}

fn receipt_row(receipt: &Receipt) -> Value {
    json!({
        "receipt_id": receipt.claims.receipt_id,
        "subject": receipt.claims.subject,
        "grantee_agent": receipt.claims.grantee_agent,
        "resource": receipt.claims.resource,
        "expires_at": receipt.claims.expires_at,
        "revoked": receipt.revoked,
        "receipt": receipt,
    })
}

/// `aegis_core::Ledger` whose hot path is the local `SqliteLedger`; every
/// `append`, `store_receipt` and `revoke_all` is mirrored into Supabase by a
/// background writer. The caller is never blocked by Supabase and a down
/// Supabase never fails a call: mirror errors are logged as warnings.
pub struct MirroredLedger {
    inner: SqliteLedger,
    store: Option<SupabaseStore>,
    tx: Option<mpsc::UnboundedSender<MirrorOp>>,
    /// Serializes append + seq discovery (SQLite assigns seq under its own
    /// lock; the count must be read before the next append slips in).
    append_lock: Mutex<()>,
}

impl MirroredLedger {
    pub fn new(inner: SqliteLedger, store: Option<SupabaseStore>) -> Self {
        let tx = store.clone().and_then(|writer_store| {
            let (tx, mut rx) = mpsc::unbounded_channel::<MirrorOp>();
            // Dedicated writer thread with its own runtime so mirroring works
            // regardless of whether a tokio runtime surrounds `new`.
            let spawned = std::thread::Builder::new()
                .name("aegis-store-mirror".to_string())
                .spawn(move || {
                    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    else {
                        return;
                    };
                    rt.block_on(async move {
                        while let Some(op) = rx.recv().await {
                            let res = match op {
                                MirrorOp::Ledger { seq, subject, entry_hash, entry } => {
                                    let row = json!({
                                        "seq": seq,
                                        "subject": subject,
                                        "entry_hash": entry_hash,
                                        "entry": entry,
                                    });
                                    writer_store.upsert("aegis_ledger", &row, "entry_hash").await
                                }
                                MirrorOp::Receipt { row } => {
                                    writer_store.upsert("aegis_receipts", &row, "receipt_id").await
                                }
                            };
                            if let Err(err) = res {
                                tracing::warn!(
                                    target: "aegis_store",
                                    error = %err,
                                    "supabase mirror write failed; sqlite hot path unaffected"
                                );
                            }
                        }
                    });
                })
                .is_ok();
            if spawned { Some(tx) } else { None }
        });
        Self {
            inner,
            store,
            tx,
            append_lock: Mutex::new(()),
        }
    }

    /// Load receipts + ledger rows from Supabase into SQLite on boot. Only
    /// fills an empty SQLite ledger (order `seq.asc` rebuilds the hash chain);
    /// a no-op when no store is configured or SQLite already has data.
    pub async fn rehydrate(&self) -> Result<(), AegisError> {
        let Some(store) = &self.store else {
            return Ok(());
        };

        let ledger_empty = self.inner.recent(None, 1).is_empty();
        // `now` far in the future so every non-revoked receipt counts.
        let receipts_empty = self.inner.active_receipts(None, i64::MAX - 1).is_empty();
        if ledger_empty {
            let rows = store.select("aegis_ledger", "order=seq.asc").await?;
            for row in rows {
                let entry: LedgerEntry = serde_json::from_value(
                    row.get("entry").cloned().unwrap_or(Value::Null),
                )
                .map_err(|e| storage_err("rehydrate ledger row", e))?;
                self.inner.append(entry)?;
            }
        }
        if receipts_empty {
            let rows = store.select("aegis_receipts", "order=receipt_id.asc").await?;
            for row in rows {
                let receipt: Receipt = serde_json::from_value(
                    row.get("receipt").cloned().unwrap_or(Value::Null),
                )
                .map_err(|e| storage_err("rehydrate receipt row", e))?;
                self.inner.store_receipt(&receipt)?;
            }
        }
        Ok(())
    }
}

impl Ledger for MirroredLedger {
    fn append(&self, entry: LedgerEntry) -> Result<LedgerEntry, AegisError> {
        let _guard = self
            .append_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let stored = self.inner.append(entry)?;
        if let Some(tx) = &self.tx {
            // seq == row count: SQLite assigns monotonically increasing rowids
            // and nothing deletes audit rows.
            let seq = self.inner.recent(None, usize::MAX).len() as i64;
            // The hosted mirror keeps the audit trail, not the data: the released
            // payload is replaced by its sha256 (entry_hash still verifies locally).
            let mut entry_json = serde_json::to_value(&stored)
                .map_err(|e| storage_err("mirror ledger encode", e))?;
            if let Some(obj) = entry_json.as_object_mut() {
                let payload = obj.remove("payload_returned").unwrap_or(Value::Null);
                let digest = sha256_hex(payload.to_string().as_bytes());
                obj.insert("payload_returned".into(), json!({"sha256": digest, "mirrored": "hash-only"}));
            }
            let _ = tx.send(MirrorOp::Ledger {
                seq,
                subject: stored.subject.clone(),
                entry_hash: stored.entry_hash.clone(),
                entry: entry_json,
            });
        }
        Ok(stored)
    }

    fn recent(&self, subject: Option<&str>, limit: usize) -> Vec<LedgerEntry> {
        self.inner.recent(subject, limit)
    }

    fn verify_chain(&self) -> bool {
        self.inner.verify_chain()
    }

    fn store_receipt(&self, receipt: &Receipt) -> Result<(), AegisError> {
        self.inner.store_receipt(receipt)?;
        if let Some(tx) = &self.tx {
            let _ = tx.send(MirrorOp::Receipt {
                row: receipt_row(receipt),
            });
        }
        Ok(())
    }

    fn get_receipt(&self, receipt_id: &str) -> Option<Receipt> {
        self.inner.get_receipt(receipt_id)
    }

    fn active_receipt(
        &self,
        subject: &str,
        agent_id: &str,
        resource: Resource,
        now: i64,
    ) -> Option<Receipt> {
        self.inner.active_receipt(subject, agent_id, resource, now)
    }

    fn active_receipts(&self, subject: Option<&str>, now: i64) -> Vec<Receipt> {
        self.inner.active_receipts(subject, now)
    }

    fn revoke_all(&self, subject: Option<&str>) -> Vec<String> {
        let revoked_ids = self.inner.revoke_all(subject);
        if let Some(tx) = &self.tx {
            for id in &revoked_ids {
                if let Some(receipt) = self.inner.get_receipt(id) {
                    let _ = tx.send(MirrorOp::Receipt {
                        row: receipt_row(&receipt),
                    });
                }
            }
        }
        revoked_ids
    }

    fn metrics(&self, subject: Option<&str>, now: i64) -> Metrics {
        self.inner.metrics(subject, now)
    }
}

// ---------------------------------------------------------- WalletStore ----

/// Linked wallets (`aegis_wallets`) and people seeding (`aegis_people`).
pub struct WalletStore {
    store: SupabaseStore,
}

impl WalletStore {
    pub fn new(store: SupabaseStore) -> Self {
        Self { store }
    }

    /// All linked wallets as `(subject, address, kind)`.
    pub async fn load(&self) -> Result<Vec<(String, String, String)>, AegisError> {
        let rows = self.store.select("aegis_wallets", "order=linked_at.asc").await?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                let subject = row.get("subject")?.as_str()?.to_string();
                let address = row.get("address")?.as_str()?.to_string();
                let kind = row
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("user")
                    .to_string();
                Some((subject, address, kind))
            })
            .collect())
    }

    /// Upsert a wallet link (one row per address).
    pub async fn save(&self, subject: &str, address: &str, kind: &str) -> Result<(), AegisError> {
        let row = json!({ "address": address, "subject": subject, "kind": kind });
        self.store.upsert("aegis_wallets", &row, "address").await
    }

    /// Seed `aegis_people` from the demo vaults. Only a sha256 of each MCP
    /// token is stored — the token itself never leaves the box.
    pub async fn seed_people(&self, vaults: &[Vault]) -> Result<(), AegisError> {
        let rows: Vec<Value> = vaults
            .iter()
            .map(|v| {
                json!({
                    "subject": v.subject(),
                    "display_name": v.person().display_name,
                    "max_payment_cents": v.max_payment_cents(),
                    "mcp_token_sha256": sha256_hex(v.mcp_token().as_bytes()),
                })
            })
            .collect();
        if rows.is_empty() {
            return Ok(());
        }
        self.store
            .upsert("aegis_people", &Value::Array(rows), "subject")
            .await
    }
}

// ---------------------------------------------------------------- tests ----

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_core::{
        now_unix, LedgerEntry, Mode, Outcome, ReceiptClaims, ReceiptSigner,
    };
    use aegis_receipts::Ed25519Signer;

    fn entry(subject: &str, call_id: &str) -> LedgerEntry {
        LedgerEntry {
            id: aegis_core::new_id("entry"),
            ts: now_unix(),
            subject: subject.to_string(),
            call_id: call_id.to_string(),
            agent_id: "agent_x".to_string(),
            tool: "query_financials".to_string(),
            purpose: "test".to_string(),
            raw_requested: vec!["checking_balance".to_string()],
            policy_applied: Mode::CleanRoom,
            outcome: Outcome::Released,
            payload_returned: json!({"can_afford": true}),
            bytes_raw: 100,
            bytes_released: 20,
            receipt_id: None,
            entry_hash: String::new(),
        }
    }

    fn receipt(subject: &str, ttl: i64) -> Receipt {
        let signer = Ed25519Signer::from_seed([7u8; 32]);
        let now = now_unix();
        signer.issue(ReceiptClaims {
            receipt_id: aegis_core::new_id("rcpt"),
            subject: subject.to_string(),
            grantee_agent: "agent_x".to_string(),
            purpose: "test".to_string(),
            resource: Resource::Financials,
            resolution_mode: Mode::CleanRoom,
            allowed_fields: vec!["checking_balance".to_string()],
            issued_at: now,
            expires_at: now + ttl,
        })
    }

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let long = vec![0x61u8; 1000];
        assert_eq!(
            sha256_hex(&long),
            // sha256 of 1000 'a' chars, computed independently.
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    #[tokio::test]
    async fn mirrored_ledger_without_store_behaves_like_inner() {
        let inner = SqliteLedger::open_in_memory().unwrap();
        let ledger = MirroredLedger::new(inner, None);
        ledger.rehydrate().await.unwrap(); // no-op without a store

        let stored = ledger.append(entry("maya", "call_1")).unwrap();
        assert!(!stored.entry_hash.is_empty());
        ledger.append(entry("dev", "call_2")).unwrap();
        let rcpt = receipt("maya", 600);
        ledger.store_receipt(&rcpt).unwrap();

        assert_eq!(ledger.recent(None, 10).len(), 2);
        assert_eq!(ledger.recent(Some("maya"), 10).len(), 1);
        assert!(ledger.verify_chain());
        assert_eq!(ledger.get_receipt(&rcpt.claims.receipt_id).unwrap().claims.receipt_id, rcpt.claims.receipt_id);
        assert!(ledger
            .active_receipt("maya", "agent_x", Resource::Financials, now_unix())
            .is_some());
        assert_eq!(ledger.active_receipts(Some("maya"), now_unix()).len(), 1);

        let revoked = ledger.revoke_all(Some("maya"));
        assert_eq!(revoked, vec![rcpt.claims.receipt_id.clone()]);
        assert!(ledger
            .active_receipt("maya", "agent_x", Resource::Financials, now_unix())
            .is_none());
        assert!(ledger.get_receipt(&rcpt.claims.receipt_id).unwrap().revoked);

        let metrics = ledger.metrics(None, now_unix());
        assert_eq!(metrics.total_calls, 2);
    }

    #[test]
    fn mirrored_ledger_never_blocks_or_panics_when_supabase_is_down() {
        // Port 9 (discard) is closed on every sane host: instant refusal.
        let store = SupabaseStore::new("http://127.0.0.1:9", "test-service-key");
        let ledger = MirroredLedger::new(SqliteLedger::open_in_memory().unwrap(), Some(store));

        let started = std::time::Instant::now();
        for i in 0..5 {
            ledger.append(entry("maya", &format!("call_{i}"))).unwrap();
        }
        let rcpt = receipt("maya", 60);
        ledger.store_receipt(&rcpt).unwrap();
        ledger.revoke_all(Some("maya"));
        assert!(started.elapsed().as_secs() < 5, "writes must not block on Supabase");

        assert_eq!(ledger.recent(None, 100).len(), 5);
        assert!(ledger.verify_chain());
    }

    #[tokio::test]
    async fn supabase_store_error_surfaces_as_storage() {
        let store = SupabaseStore::new("http://127.0.0.1:9", "k");
        let err = store
            .upsert("aegis_people", &json!({"subject": "maya"}), "subject")
            .await
            .unwrap_err();
        assert!(matches!(err, AegisError::Storage(_)));
        let err = store.select("aegis_people", "subject=eq.maya").await.unwrap_err();
        assert!(matches!(err, AegisError::Storage(_)));
    }
}
