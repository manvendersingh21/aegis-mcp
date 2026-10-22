//! Anchors each person's ledger head on Base Sepolia (hashes only) and serves
//! the latest anchors. Disabled unless AEGIS_CHAIN_KEY is set.

use aegis_chain::ChainAnchorer;
use aegis_core::{ChainAnchor, Ledger};
use axum::{extract::State, routing::get, Json, Router};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
pub struct OnChain {
    anchorer: Option<Arc<ChainAnchorer>>,
    chain_id: u64,
    explorer: String,
    anchors: Arc<Mutex<HashMap<String, ChainAnchor>>>,
}

impl OnChain {
    pub fn from_env() -> Self {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        OnChain {
            anchorer: ChainAnchorer::from_env().map(Arc::new),
            chain_id: get("AEGIS_CHAIN_ID").and_then(|c| c.parse().ok()).unwrap_or(84532),
            explorer: get("AEGIS_EXPLORER").unwrap_or_else(|| "https://sepolia.basescan.org".into()),
            anchors: Arc::default(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.anchorer.is_some()
    }

    /// Anchor each subject's head when it changes, at most once per `every`.
    pub fn spawn(&self, ledger: Arc<dyn Ledger>, subjects: Vec<String>, every: Duration) {
        let Some(anchorer) = self.anchorer.clone() else { return };
        let anchors = self.anchors.clone();
        tokio::spawn(async move {
            loop {
                for subject in &subjects {
                    let Some(head) = ledger.recent(Some(subject), 1).into_iter().next() else { continue };
                    let already = anchors
                        .lock()
                        .map(|m| m.get(subject).map(|a| a.head_hash == head.entry_hash).unwrap_or(false))
                        .unwrap_or(true);
                    if already {
                        continue;
                    }
                    match anchorer.anchor(subject, &head.entry_hash).await {
                        Ok(anchor) => {
                            tracing::info!("anchored {subject} on-chain: {}", anchor.explorer_url);
                            if let Ok(mut m) = anchors.lock() {
                                m.insert(subject.clone(), anchor);
                            }
                        }
                        Err(e) => tracing::warn!("on-chain anchor for {subject} failed: {e}"),
                    }
                }
                tokio::time::sleep(every).await;
            }
        });
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/api/chain/anchors", get(list_anchors))
            .route("/api/chain/info", get(info))
            .with_state(self.clone())
    }
}

async fn list_anchors(State(chain): State<OnChain>) -> Json<Value> {
    let anchors = chain.anchors.lock().map(|m| m.clone()).unwrap_or_default();
    Json(json!({"chain_id": chain.chain_id, "explorer": chain.explorer, "anchors": anchors}))
}

async fn info(State(chain): State<OnChain>) -> Json<Value> {
    Json(json!({
        "enabled": chain.enabled(),
        "chain_id": chain.chain_id,
        "anchor_address": chain.anchorer.as_ref().map(|a| a.address()),
        "explorer": chain.explorer,
    }))
}
