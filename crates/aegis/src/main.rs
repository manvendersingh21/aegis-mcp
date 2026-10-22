//! AegisMCP server binary. Reads `.env` (repo root or cwd), then env:
//! AEGIS_PORT (8787), AEGIS_BIND (127.0.0.1), AEGIS_DB (aegis.db), AEGIS_SIGNING_SEED (64 hex),
//! AEGIS_OFFLINE=1, AEGIS_RISK_THRESHOLD (0.8), AEGIS_CONSENT_TIMEOUT (90),
//! AEGIS_REQUIRE_MCP_TOKEN=1, AEGIS_LLM_URL/KEY/MODEL, JEV_API_KEY, STRIPE_API_KEY.

use aegis::{build_app_live, describe, load_dotenv, Options};
use aegis_core::demo_vaults;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_dotenv(".env");
    load_dotenv(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"));
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    let opts = Options::from_env();
    let app = build_app_live(&opts).await?;
    for line in describe(&opts) {
        tracing::info!("{line}");
    }
    let port = opts.port;
    for v in demo_vaults() {
        tracing::info!(
            "{:<12} dashboard http://127.0.0.1:{port}/u/{}  (sign in with your wallet or the labelled demo wallet)",
            v.person().display_name, v.subject()
        );
    }

    // Loopback by default: /mcp accepts x-aegis-user without a token unless
    // AEGIS_REQUIRE_MCP_TOKEN=1, so never expose it on a shared network by accident.
    let bind = std::env::var("AEGIS_BIND").unwrap_or_else(|_| "127.0.0.1".into());
    let listener = tokio::net::TcpListener::bind((bind.as_str(), port)).await?;
    tracing::info!("AegisMCP on http://127.0.0.1:{port}  (MCP endpoint http://127.0.0.1:{port}/mcp)");
    axum::serve(listener, app).await?;
    Ok(())
}
