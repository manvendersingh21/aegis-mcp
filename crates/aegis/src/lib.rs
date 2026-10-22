//! AegisMCP app wiring, shared by the `aegis` binary and the e2e tests.

use aegis_agent::Scenarios;
use aegis_cleanroom::{LlmPurposeJudge, LocalCleanRoom, RegexRedactor, SchemaGuard};
use aegis_core::{demo_vaults, LlmClient, PaymentProvider, PurposeJudge, RiskScorer, ScenarioRunner};
use aegis_gateway::{router, GatewayDeps};
use aegis_integrations::{DemoPayments, JevScorer, StripePayments};
use aegis_receipts::{Ed25519Signer, SqliteLedger};
use std::sync::Arc;

pub mod anchor;
pub mod redteam;
pub use aegis_integrations::load_dotenv;

/// Everything that varies between the live demo and an offline test run.
#[derive(Debug, Clone)]
pub struct Options {
    pub port: u16,
    /// SQLite path; `None` = in-memory.
    pub db_path: Option<String>,
    /// 64 hex chars; `None` = fresh random key.
    pub signing_seed_hex: Option<String>,
    /// Local 9B judge + in-character agent narration (needs AEGIS_LLM_URL).
    pub use_llm: bool,
    /// Jev risk scoring (needs JEV_API_KEY).
    pub use_jev: bool,
    /// Stripe test mode (needs STRIPE_API_KEY); otherwise the labelled demo provider.
    pub use_stripe: bool,
    pub risk_threshold: f64,
    pub consent_timeout_secs: u64,
    pub require_mcp_token: bool,
    /// Anchor ledger head hashes to Supabase Storage (needs SUPABASE_URL + SUPABASE_SECRET_KEY).
    pub use_supabase: bool,
}

impl Options {
    /// No network dependencies at all: deterministic layers + demo payments.
    pub fn offline(port: u16) -> Self {
        Options {
            port,
            db_path: None,
            signing_seed_hex: None,
            use_llm: false,
            use_jev: false,
            use_stripe: false,
            risk_threshold: 0.8,
            consent_timeout_secs: 90,
            require_mcp_token: false,
            use_supabase: false,
        }
    }

    /// Read from env (call `load_dotenv` first). Integrations switch on when
    /// their keys are present unless AEGIS_OFFLINE=1.
    pub fn from_env() -> Self {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let offline = get("AEGIS_OFFLINE").as_deref() == Some("1");
        Options {
            port: get("AEGIS_PORT").and_then(|p| p.parse().ok()).unwrap_or(8787),
            db_path: Some(get("AEGIS_DB").unwrap_or_else(|| "aegis.db".into())),
            signing_seed_hex: get("AEGIS_SIGNING_SEED"),
            use_llm: !offline && get("AEGIS_LLM_URL").is_some(),
            use_jev: !offline && get("JEV_API_KEY").is_some(),
            use_stripe: !offline && (get("STRIPE_API_KEY").is_some() || get("STRIPE_SECRET_KEY").is_some()),
            risk_threshold: get("AEGIS_RISK_THRESHOLD").and_then(|t| t.parse().ok()).unwrap_or(0.8),
            consent_timeout_secs: get("AEGIS_CONSENT_TIMEOUT").and_then(|t| t.parse().ok()).unwrap_or(90),
            require_mcp_token: get("AEGIS_REQUIRE_MCP_TOKEN").as_deref() == Some("1"),
            use_supabase: !offline && get("SUPABASE_URL").is_some() && get("SUPABASE_SECRET_KEY").is_some(),
        }
    }
}

fn parse_seed(hex_seed: &str) -> Option<[u8; 32]> {
    let bytes = (0..hex_seed.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex_seed.get(i..i + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    bytes.try_into().ok()
}

/// Human-readable lines describing which layers are live.
pub fn describe(opts: &Options) -> Vec<String> {
    let on = |b: bool| if b { "ON" } else { "off" };
    vec![
        format!("layer 1 deterministic redaction + schema guard: ON"),
        format!("layer 2 clean room: ON   local 9B judge/narration: {}", on(opts.use_llm)),
        format!("layer 3 Ed25519 receipts + hash-chained ledger: ON"),
        format!("Jev risk scoring: {} (threshold {:.2})", on(opts.use_jev), opts.risk_threshold),
        format!("payments: {}", match opts.use_stripe.then(StripePayments::from_env).flatten() {
            Some(Ok(_)) => "Stripe TEST mode".to_string(),
            Some(Err(e)) => format!("demo provider (labelled) — Stripe key rejected: {e}"),
            None => "demo provider (labelled)".to_string(),
        }),
        format!("ledger anchoring to Supabase Storage: {}", on(opts.use_supabase)),
    ]
}

/// Build the full app: gateway API + MCP endpoint + per-person dashboards.
pub fn build_app(opts: &Options) -> anyhow::Result<axum::Router> {
    let ledger = match &opts.db_path {
        Some(path) => SqliteLedger::open(path),
        None => SqliteLedger::open_in_memory(),
    }
    .map_err(|e| anyhow::anyhow!("ledger: {e}"))?;
    let ledger: Arc<dyn aegis_core::Ledger> = Arc::new(ledger);
    let signer = match opts.signing_seed_hex.as_deref().and_then(parse_seed) {
        Some(seed) => Ed25519Signer::from_seed(seed),
        None => Ed25519Signer::generate(),
    };

    let llm = if opts.use_llm { LlmClient::from_env() } else { None };
    let llm_for_redteam = llm.clone();
    let risk: Option<Arc<dyn RiskScorer>> =
        if opts.use_jev { JevScorer::from_env().map(|j| Arc::new(j) as Arc<dyn RiskScorer>) } else { None };
    let payments: Arc<dyn PaymentProvider> = match opts.use_stripe.then(StripePayments::from_env).flatten() {
        Some(Ok(stripe)) => Arc::new(stripe),
        Some(Err(e)) => {
            tracing::warn!("Stripe disabled ({e}); using demo payments");
            Arc::new(DemoPayments)
        }
        None => Arc::new(DemoPayments),
    };

    let deps = GatewayDeps {
        vaults: demo_vaults(),
        redactor: Arc::new(RegexRedactor::new()),
        guard: Arc::new(SchemaGuard::new()),
        clean_room: Arc::new(LocalCleanRoom::new()),
        judge: llm.clone().map(|c| Arc::new(LlmPurposeJudge::new(c)) as Arc<dyn PurposeJudge>),
        risk,
        risk_threshold: opts.risk_threshold,
        payments: Some(payments),
        signer: Arc::new(signer),
        ledger: ledger.clone(),
        scenarios: Some(Arc::new(Scenarios::new(llm)) as Arc<dyn ScenarioRunner>),
        base_url: format!("http://127.0.0.1:{}", opts.port),
        consent_timeout_secs: opts.consent_timeout_secs,
        require_mcp_token: opts.require_mcp_token,
    };
    let redteam = redteam::RedTeam::new(llm_for_redteam, format!("http://127.0.0.1:{}", opts.port));
    let mut app = router(deps).merge(aegis_dashboard::router()).merge(redteam.router());
    if let Some(anchorer) = opts.use_supabase.then(anchor::Anchorer::from_env).flatten() {
        app = app.merge(anchorer.router());
        let subjects = demo_vaults().iter().map(|v| v.subject()).collect();
        anchorer.spawn(ledger, subjects, std::time::Duration::from_secs(20));
    }
    Ok(app)
}
