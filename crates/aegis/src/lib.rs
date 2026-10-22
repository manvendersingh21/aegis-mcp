//! AegisMCP app wiring, shared by the `aegis` binary and the e2e tests.

use aegis_agent::Scenarios;
use aegis_cleanroom::{LlmPurposeJudge, LocalCleanRoom, RegexRedactor, SchemaGuard};
use aegis_core::{demo_vaults, LlmClient, PaymentProvider, PurposeJudge, RiskScorer, ScenarioRunner};
use aegis_gateway::{router_with_wallet, GatewayDeps};
use aegis_integrations::{DemoPayments, JevScorer, StripePayments};
use aegis_receipts::{Ed25519Signer, SqliteLedger};
use std::sync::Arc;

pub mod anchor;
pub mod auth;
pub mod broker_client;
pub mod demo_wallet;
pub mod onchain;
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
    /// Anchor ledger head hashes on Base Sepolia (needs AEGIS_CHAIN_KEY).
    pub use_chain: bool,
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
            use_chain: false,
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
            // With the broker configured, providers are reached through it with fake keys.
            use_llm: !offline && (get("AEGIS_LLM_URL").is_some() || get("AEGIS_BROKER_TOKEN").is_some()),
            use_jev: !offline && (get("JEV_API_KEY").is_some() || get("AEGIS_BROKER_TOKEN").is_some()),
            use_stripe: !offline && (get("STRIPE_API_KEY").is_some() || get("STRIPE_SECRET_KEY").is_some() || get("AEGIS_BROKER_TOKEN").is_some()),
            risk_threshold: get("AEGIS_RISK_THRESHOLD").and_then(|t| t.parse().ok()).unwrap_or(0.8),
            consent_timeout_secs: get("AEGIS_CONSENT_TIMEOUT").and_then(|t| t.parse().ok()).unwrap_or(90),
            require_mcp_token: get("AEGIS_REQUIRE_MCP_TOKEN").as_deref() == Some("1"),
            use_chain: !offline && get("AEGIS_CHAIN_KEY").is_some(),
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
            Some(Ok(_)) => "Stripe TEST mode (direct key)".to_string(),
            Some(Err(e)) => format!("demo provider (labelled) — Stripe key rejected: {e}"),
            None if std::env::var("AEGIS_BROKER_TOKEN").is_ok() => "Stripe TEST mode via aegis-broker (fake key)".to_string(),
            None => "demo provider (labelled)".to_string(),
        }),
        format!("ledger anchoring to Supabase Storage: {}", on(opts.use_supabase)),
        format!("ledger anchoring on Base Sepolia: {}   SIWE wallet login: ON", on(opts.use_chain)),
    ]
}

/// Build the full app: gateway API + MCP endpoint + per-person dashboards.
/// Provider clients the app runs with. Offline/env mode builds them from
/// local keys; live mode builds them from broker-issued FAKE keys.
struct Providers {
    llm: Option<LlmClient>,
    risk: Option<Arc<dyn RiskScorer>>,
    payments: Arc<dyn PaymentProvider>,
    vaults: Vec<aegis_core::Vault>,
    broker: Option<broker_client::BrokerClient>,
    /// Red team's raw-vault model client (loopback via the broker), if any.
    raw_llm: Option<LlmClient>,
    /// Supabase-mirrored ledger (live mode); otherwise a local SQLite ledger.
    ledger: Option<Arc<dyn aegis_core::Ledger>>,
    /// Persisted wallet links + where to save new ones (live mode).
    wallet_links: Vec<(String, String)>,
    wallet_store: Option<Arc<aegis_store::WalletStore>>,
}

fn providers_from_env(opts: &Options) -> Providers {
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
    Providers {
        llm: if opts.use_llm { LlmClient::from_env() } else { None },
        risk,
        payments,
        vaults: demo_vaults(),
        broker: None,
        raw_llm: None,
        ledger: None,
        wallet_links: Vec::new(),
        wallet_store: None,
    }
}

/// Main process holds no real keys: every provider goes through the broker
/// with a fake key issued to agent "aegis-gateway".
async fn providers_from_broker(opts: &Options, broker: broker_client::BrokerClient) -> Providers {
    const ME: &str = "aegis-gateway";
    const PLATFORM: &str = "platform";
    let day = 86_400;
    let llm = match opts.use_llm.then_some(()) {
        Some(()) => match broker.issue(PLATFORM, ME, "llm", day).await {
            Ok(k) => Some(LlmClient::new(
                broker.proxy_url("llm", "v1"),
                k.fake_key,
                std::env::var("AEGIS_LLM_MODEL").unwrap_or_else(|_| "aegis-9b".into()),
                std::time::Duration::from_secs(20),
            )),
            Err(e) => { tracing::warn!("{e}"); None }
        },
        None => None,
    };
    let risk = match opts.use_jev.then_some(()) {
        Some(()) => match broker.issue(PLATFORM, ME, "jev", day).await {
            Ok(k) => Some(Arc::new(JevScorer::via_broker(k.fake_key, broker.proxy_url("jev", "v1/systemone"))) as Arc<dyn RiskScorer>),
            Err(e) => { tracing::warn!("{e}"); None }
        },
        None => None,
    };
    let payments: Arc<dyn PaymentProvider> = match broker.issue(PLATFORM, ME, "stripe", day).await {
        Ok(k) => match StripePayments::via_broker(k.fake_key, broker.proxy_url("stripe", "v1/payment_intents")) {
            Ok(s) => Arc::new(s),
            Err(e) => { tracing::warn!("{e}"); Arc::new(DemoPayments) }
        },
        Err(e) => { tracing::warn!("{e}; using demo payments"); Arc::new(DemoPayments) }
    };
    let mut vaults = Vec::new();
    for v in demo_vaults() {
        match broker.vault(&v.subject()).await {
            Ok(value) if value.is_object() => vaults.push(aegis_core::Vault(value)),
            Ok(_) | Err(_) => vaults.push(v),
        }
    }
    // The broker's LLM upstream is the ssh loopback forward, so the red team's
    // raw-vault prompt never crosses the public tunnel.
    let raw_llm = llm.clone();
    let (ledger, wallet_links, wallet_store) = supabase_state(opts, &vaults).await;
    Providers { llm, risk, payments, vaults, broker: Some(broker), raw_llm, ledger, wallet_links, wallet_store }
}

/// Supabase as the system of record: mirrored ledger (rehydrated on boot),
/// people table, and persisted wallet links. Degrades to local-only on error.
async fn supabase_state(
    opts: &Options,
    vaults: &[aegis_core::Vault],
) -> (Option<Arc<dyn aegis_core::Ledger>>, Vec<(String, String)>, Option<Arc<aegis_store::WalletStore>>) {
    let Some(store) = aegis_store::SupabaseStore::from_env() else { return (None, Vec::new(), None) };
    let sqlite = match &opts.db_path {
        Some(path) => SqliteLedger::open(path),
        None => SqliteLedger::open_in_memory(),
    };
    let ledger = match sqlite {
        Ok(sqlite) => {
            let mirrored = aegis_store::MirroredLedger::new(sqlite, Some(store.clone()));
            if let Err(e) = mirrored.rehydrate().await {
                tracing::warn!("ledger rehydrate from Supabase failed: {e}");
            }
            Some(Arc::new(mirrored) as Arc<dyn aegis_core::Ledger>)
        }
        Err(e) => {
            tracing::warn!("ledger: {e}");
            None
        }
    };
    let wallets = aegis_store::WalletStore::new(store);
    if let Err(e) = wallets.seed_people(vaults).await {
        tracing::warn!("seeding aegis_people failed (did you run supabase/schema.sql?): {e}");
    }
    let links = match wallets.load().await {
        Ok(rows) => rows.into_iter().filter(|(_, _, kind)| kind == "user").map(|(s, a, _)| (s, a)).collect(),
        Err(e) => {
            tracing::warn!("loading wallet links failed: {e}");
            Vec::new()
        }
    };
    tracing::info!("system of record: Supabase (ledger mirrored, {} wallet link(s) restored)", links.len());
    (ledger, links, Some(Arc::new(wallets)))
}

/// Offline / env-key build (tests, `AEGIS_OFFLINE=1`).
pub fn build_app(opts: &Options) -> anyhow::Result<axum::Router> {
    assemble(opts, providers_from_env(opts))
}

/// Live build: uses the key broker when `AEGIS_BROKER_TOKEN` is set.
pub async fn build_app_live(opts: &Options) -> anyhow::Result<axum::Router> {
    let providers = match broker_client::BrokerClient::from_env() {
        Some(broker) if !opts_offline() => providers_from_broker(opts, broker).await,
        _ => providers_from_env(opts),
    };
    assemble(opts, providers)
}

fn opts_offline() -> bool {
    std::env::var("AEGIS_OFFLINE").as_deref() == Ok("1")
}

fn assemble(opts: &Options, providers: Providers) -> anyhow::Result<axum::Router> {
    let Providers { llm, risk, payments, vaults, broker, raw_llm, ledger: mirrored, wallet_links, wallet_store } = providers;

    let ledger = match &opts.db_path {
        Some(path) => SqliteLedger::open(path),
        None => SqliteLedger::open_in_memory(),
    }
    .map_err(|e| anyhow::anyhow!("ledger: {e}"))?;
    let ledger: Arc<dyn aegis_core::Ledger> = match mirrored {
        Some(m) => m,
        None => Arc::new(ledger),
    };
    let signer = match opts.signing_seed_hex.as_deref().and_then(parse_seed) {
        Some(seed) => Ed25519Signer::from_seed(seed),
        None => Ed25519Signer::generate(),
    };

    let llm_for_redteam = llm.clone();

    let deps = GatewayDeps {
        vaults,
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
    let mut redteam = redteam::RedTeam::new(llm_for_redteam, format!("http://127.0.0.1:{}", opts.port));
    if let Some(raw) = raw_llm {
        redteam = redteam.with_raw_llm(raw);
    }
    if let Some(b) = &broker {
        redteam = redteam.with_broker(b.clone());
    }
    let siwe_domain = std::env::var("AEGIS_SIWE_DOMAIN").unwrap_or_else(|_| format!("127.0.0.1:{}", opts.port));
    let chain = onchain::OnChain::from_env();
    let siwe_domain_for_demo = siwe_domain.clone();
    let siwe = Arc::new(aegis_chain::SiweAuth::new(siwe_domain, 84532));
    for (subject, address) in &wallet_links {
        siwe.preload_link(subject, address);
    }
    if let Some(store) = wallet_store {
        siwe.set_on_link(Box::new(move |subject, address| {
            let (store, subject, address) = (store.clone(), subject.to_string(), address.to_string());
            tokio::spawn(async move {
                if let Err(e) = store.save(&subject, &address, "user").await {
                    tracing::warn!("persisting wallet link failed: {e}");
                }
            });
        }));
    }
    let subjects: Vec<String> = demo_vaults().iter().map(|v| v.subject()).collect();
    let demo = demo_wallet::enabled().then(|| demo_wallet::router(&siwe, &subjects, siwe_domain_for_demo.clone()));
    let wallet = siwe.clone() as Arc<dyn aegis_core::WalletAuth>;
    let base_url = format!("http://127.0.0.1:{}", opts.port);
    let mut app = router_with_wallet(deps, Some(wallet))
        .merge(aegis_dashboard::router())
        .merge(demo.unwrap_or_default())
        .merge(redteam.router())
        .merge(chain.router());
    if let Some(broker) = broker {
        app = app.merge(broker.router(base_url));
    }
    if opts.use_chain {
        let subjects = demo_vaults().iter().map(|v| v.subject()).collect();
        chain.spawn(ledger.clone(), subjects, std::time::Duration::from_secs(60));
    }
    if let Some(anchorer) = opts.use_supabase.then(anchor::Anchorer::from_env).flatten() {
        app = app.merge(anchorer.router());
        let subjects = demo_vaults().iter().map(|v| v.subject()).collect();
        anchorer.spawn(ledger, subjects, std::time::Duration::from_secs(20));
    }
    Ok(app)
}
