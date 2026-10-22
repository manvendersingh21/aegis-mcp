//! Off-box tamper evidence: periodically anchors each person's ledger head
//! hash into a private Supabase Storage bucket. If someone rewrites the local
//! SQLite ledger, its chain head no longer matches the last external anchor.
//!
//! Env: SUPABASE_URL, SUPABASE_SECRET_KEY. Only hashes leave the machine —
//! never ledger payloads or vault data.

use aegis_core::{now_unix, Ledger};
use axum::{extract::State, routing::get, Json, Router};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BUCKET: &str = "aegis-ledger-anchors";

#[derive(Clone)]
pub struct Anchorer {
    http: reqwest::Client,
    url: String,
    key: String,
    /// subject -> last anchor record
    last: Arc<Mutex<HashMap<String, Value>>>,
}

impl Anchorer {
    pub fn from_env() -> Option<Self> {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().ok()?;
        Some(Anchorer {
            http,
            url: get("SUPABASE_URL")?.trim_end_matches('/').to_string(),
            key: get("SUPABASE_SECRET_KEY")?,
            last: Arc::default(),
        })
    }

    async fn ensure_bucket(&self) {
        // 409/400 "already exists" is fine.
        let _ = self
            .http
            .post(format!("{}/storage/v1/bucket", self.url))
            .bearer_auth(&self.key)
            .header("apikey", &self.key)
            .json(&json!({"id": BUCKET, "name": BUCKET, "public": false}))
            .send()
            .await;
    }

    async fn upload(&self, path: &str, body: &Value) -> Result<(), String> {
        let resp = self
            .http
            .post(format!("{}/storage/v1/object/{BUCKET}/{path}", self.url))
            .bearer_auth(&self.key)
            .header("apikey", &self.key)
            .header("x-upsert", "true")
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("status {}", resp.status()))
        }
    }

    /// Anchor every subject whose chain head moved since the last pass.
    async fn anchor_once(&self, ledger: &dyn Ledger, subjects: &[String]) {
        for subject in subjects {
            let Some(head) = ledger.recent(Some(subject), 1).into_iter().next() else { continue };
            let unchanged = self
                .last
                .lock()
                .map(|m| m.get(subject).and_then(|v| v["head_hash"].as_str().map(str::to_owned)) == Some(head.entry_hash.clone()))
                .unwrap_or(false);
            if unchanged {
                continue;
            }
            let ts = now_unix();
            let record = json!({
                "subject": subject,
                "head_hash": head.entry_hash,
                "head_entry_id": head.id,
                "anchored_at": ts,
                "chain_valid": ledger.verify_chain(),
                "object": format!("{BUCKET}/{subject}/{ts}.json"),
            });
            let latest = self.upload(&format!("{subject}/latest.json"), &record).await;
            let history = self.upload(&format!("{subject}/{ts}.json"), &record).await;
            match latest.and(history) {
                Ok(()) => {
                    tracing::info!("anchored {subject} ledger head {}… to Supabase", &head.entry_hash[..12.min(head.entry_hash.len())]);
                    if let Ok(mut m) = self.last.lock() {
                        m.insert(subject.clone(), record);
                    }
                }
                Err(e) => tracing::warn!("Supabase anchor for {subject} failed: {e}"),
            }
        }
    }

    /// Spawn the anchoring loop (every `every`).
    pub fn spawn(self, ledger: Arc<dyn Ledger>, subjects: Vec<String>, every: Duration) {
        tokio::spawn(async move {
            self.ensure_bucket().await;
            loop {
                self.anchor_once(ledger.as_ref(), &subjects).await;
                tokio::time::sleep(every).await;
            }
        });
    }

    /// `GET /api/anchors` → latest anchor per subject (hashes only, public).
    pub fn router(&self) -> Router {
        Router::new().route("/api/anchors", get(list_anchors)).with_state(self.last.clone())
    }
}

async fn list_anchors(State(last): State<Arc<Mutex<HashMap<String, Value>>>>) -> Json<Value> {
    let map = last.lock().map(|m| m.clone()).unwrap_or_default();
    Json(json!({"provider": "supabase-storage", "bucket": BUCKET, "anchors": map}))
}
