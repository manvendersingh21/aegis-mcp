//! AegisMCP server: wires the clean room, receipts, gateway, agent scenarios
//! and per-person dashboards into one axum app.
//!
//! Env: AEGIS_PORT (8787), AEGIS_DB (aegis.db), AEGIS_SIGNING_SEED (64 hex),
//! AEGIS_REQUIRE_MCP_TOKEN (0), AEGIS_CONSENT_TIMEOUT (90), AEGIS_LLM_URL/KEY/MODEL.

use aegis_agent::Scenarios;
use aegis_cleanroom::{LlmPurposeJudge, LocalCleanRoom, RegexRedactor, SchemaGuard};
use aegis_core::{demo_vaults, LlmClient, PurposeJudge, ScenarioRunner};
use aegis_gateway::{router, GatewayDeps};
use aegis_receipts::{Ed25519Signer, SqliteLedger};
use std::sync::Arc;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

fn signer() -> Ed25519Signer {
    let seed = std::env::var("AEGIS_SIGNING_SEED").ok().and_then(|h| {
        let mut out = [0u8; 32];
        let bytes = (0..h.len()).step_by(2).map(|i| u8::from_str_radix(h.get(i..i + 2)?, 16).ok()).collect::<Option<Vec<u8>>>()?;
        (bytes.len() == 32).then(|| {
            out.copy_from_slice(&bytes);
            out
        })
    });
    match seed {
        Some(s) => Ed25519Signer::from_seed(s),
        None => Ed25519Signer::generate(),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(env_or("RUST_LOG", "info,aegis=debug")).init();

    let port = env_or("AEGIS_PORT", "8787");
    let base_url = format!("http://127.0.0.1:{port}");
    let llm = LlmClient::from_env();
    match &llm {
        Some(_) => tracing::info!("local LLM enabled ({})", env_or("AEGIS_LLM_URL", "")),
        None => tracing::info!("AEGIS_LLM_URL unset: running fully deterministic"),
    }

    let ledger = SqliteLedger::open(env_or("AEGIS_DB", "aegis.db")).map_err(|e| anyhow::anyhow!("{e}"))?;
    let signer = signer();
    let vaults = demo_vaults();
    for v in &vaults {
        tracing::info!("person {:>5}: dashboard http://127.0.0.1:{port}/u/{}  (password {}-demo, MCP token {})",
            v.subject(), v.subject(), v.subject(), v.mcp_token());
    }

    let deps = GatewayDeps {
        vaults,
        redactor: Arc::new(RegexRedactor::new()),
        guard: Arc::new(SchemaGuard::new()),
        clean_room: Arc::new(LocalCleanRoom::new()),
        judge: llm.clone().map(|c| Arc::new(LlmPurposeJudge::new(c)) as Arc<dyn PurposeJudge>),
        signer: Arc::new(signer),
        ledger: Arc::new(ledger),
        scenarios: Some(Arc::new(Scenarios::new(llm)) as Arc<dyn ScenarioRunner>),
        base_url,
        consent_timeout_secs: env_or("AEGIS_CONSENT_TIMEOUT", "90").parse().unwrap_or(90),
        require_mcp_token: env_or("AEGIS_REQUIRE_MCP_TOKEN", "0") == "1",
    };

    let app = router(deps).merge(aegis_dashboard::router());
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    tracing::info!("AegisMCP listening on http://127.0.0.1:{port}  (MCP endpoint: /mcp)");
    axum::serve(listener, app).await?;
    Ok(())
}
