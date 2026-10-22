//! aegis-broker: the only process that holds real provider keys.
//!
//! Reads `.env.broker` (never the main app's `.env`):
//! AEGIS_MASTER_KEY (64 hex), AEGIS_BROKER_TOKEN, AEGIS_BROKER_PORT (8788),
//! STRIPE_API_KEY, JEV_API_KEY, AEGIS_LLM_UPSTREAM (+ AEGIS_LLM_UPSTREAM_KEY),
//! SUPABASE_URL + SUPABASE_SECRET_KEY (system of record; in-memory if unset).

use aegis_broker::{router, BrokerState, BrokerStore, MemoryStore, Sealer, StoredSecret};
use aegis_core::{new_id, now_unix, SecretMeta};
use std::sync::Arc;

mod supabase_store;

fn master_key() -> anyhow::Result<[u8; 32]> {
    let hex_key = std::env::var("AEGIS_MASTER_KEY")
        .map_err(|_| anyhow::anyhow!("AEGIS_MASTER_KEY is required"))?;
    let bytes =
        hex::decode(hex_key.trim()).map_err(|_| anyhow::anyhow!("AEGIS_MASTER_KEY must be hex"))?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("AEGIS_MASTER_KEY must be 32 bytes (64 hex chars)"))
}

/// Seal platform secrets from the environment into the store (once).
async fn seed_platform_secrets(store: &dyn BrokerStore, sealer: &Sealer) -> anyhow::Result<()> {
    let existing = store
        .secrets(None)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    for (provider, var) in [
        ("stripe", "STRIPE_API_KEY"),
        ("jev", "JEV_API_KEY"),
        ("llm", "AEGIS_LLM_UPSTREAM_KEY"),
    ] {
        let Some(value) = std::env::var(var)
            .ok()
            .map(|v| v.trim().trim_matches('"').to_string())
            .filter(|v| !v.is_empty())
        else {
            continue;
        };
        let last4: String = value
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if existing.iter().any(|s| {
            s.meta.provider == provider
                && s.meta.subject.is_none()
                && !s.meta.revoked
                && s.meta.last4 == last4
        }) {
            continue;
        }
        let (nonce, ciphertext) = sealer.seal(value.as_bytes());
        let meta = SecretMeta {
            secret_id: new_id("sec"),
            subject: None,
            provider: provider.into(),
            label: format!("platform {provider} key"),
            last4,
            created_at: now_unix(),
            revoked: false,
        };
        store
            .put_secret(StoredSecret {
                meta,
                nonce,
                ciphertext,
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        tracing::info!("sealed platform {provider} key into the store");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    aegis_integrations::load_dotenv(".env.broker");
    aegis_integrations::load_dotenv(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env.broker"));
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    let token = std::env::var("AEGIS_BROKER_TOKEN")
        .map_err(|_| anyhow::anyhow!("AEGIS_BROKER_TOKEN is required"))?;
    let store: Arc<dyn BrokerStore> = match supabase_store::SupabaseBrokerStore::from_env() {
        Some(s) => {
            tracing::info!("system of record: Supabase");
            Arc::new(s)
        }
        None => {
            tracing::warn!("SUPABASE_URL/SUPABASE_SECRET_KEY unset: secrets kept in memory only");
            Arc::new(MemoryStore::new())
        }
    };
    let sealer = Sealer::new(master_key()?);
    seed_platform_secrets(store.as_ref(), &sealer).await?;

    let llm_upstream =
        std::env::var("AEGIS_LLM_UPSTREAM").unwrap_or_else(|_| "http://127.0.0.1:11436".into());
    let state = BrokerState::new(store, sealer, token).with_upstreams(
        "https://api.stripe.com",
        "https://api.typesafe.ai",
        llm_upstream,
    );

    let port: u16 = std::env::var("AEGIS_BROKER_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8788);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    tracing::info!(
        "aegis-broker on http://127.0.0.1:{port} (admin: bearer token; proxy: fake keys only)"
    );
    axum::serve(listener, router(state)).await?;
    Ok(())
}
