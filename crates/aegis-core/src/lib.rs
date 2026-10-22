//! AegisMCP shared contract. Every other crate codes against the types and
//! traits in this file; changing them requires the orchestrator's sign-off.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;

pub mod llm;
pub use llm::LlmClient;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

// ---------------------------------------------------------------- vault ----

pub const DEMO_VAULT_JSON: &str = include_str!("../vault_data.json");

/// One person's raw personal data. Never leaves the gateway unfiltered.
/// Every person has their own vault, consent queue, receipts, ledger view and
/// dashboard (`/u/{subject}`); `subject` is the tenant key everywhere.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vault(pub Value);

/// Public listing entry for a person (no PII beyond display name).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub subject: String,
    pub display_name: String,
}

/// Find the person an MCP bearer token belongs to.
pub fn vault_for_token<'a>(vaults: &'a [Vault], token: &str) -> Option<&'a Vault> {
    vaults.iter().find(|v| !token.is_empty() && v.mcp_token() == token)
}

/// All demo people, in file order (first = default subject).
pub fn demo_vaults() -> Vec<Vault> {
    let all: Vec<Value> = serde_json::from_str(DEMO_VAULT_JSON).expect("vault_data.json is valid");
    all.into_iter().map(Vault).collect()
}

impl Vault {
    /// First demo person ("maya").
    pub fn demo() -> Self {
        demo_vaults().remove(0)
    }
    /// Demo person by subject.
    pub fn demo_for(subject: &str) -> Option<Self> {
        demo_vaults().into_iter().find(|v| v.subject() == subject)
    }
    pub fn subject(&self) -> String {
        self.0["subject"].as_str().unwrap_or("unknown").to_string()
    }
    /// Dashboard login check. Demo passwords are "<subject>-demo".
    pub fn verify_password(&self, password: &str) -> bool {
        use sha2::{Digest, Sha256};
        let want = self.0["credentials"]["password_sha256"].as_str().unwrap_or("");
        !want.is_empty() && hex::encode(Sha256::digest(password.as_bytes())) == want
    }
    /// This person's MCP credential: agents send `Authorization: Bearer <token>`.
    pub fn mcp_token(&self) -> String {
        self.0["credentials"]["mcp_token"].as_str().unwrap_or("").to_string()
    }
    /// Per-person hard cap for `make_payment` (deterministic, never LLM-decided).
    pub fn max_payment_cents(&self) -> i64 {
        self.0["policy"]["max_payment_cents"].as_i64().unwrap_or(0)
    }
    pub fn person(&self) -> Person {
        Person { subject: self.subject(), display_name: self.0["display_name"].as_str().unwrap_or("Unknown").to_string() }
    }
    /// Raw JSON slice for one resource (e.g. the whole `financials` object).
    pub fn resource(&self, r: Resource) -> Value {
        self.0.get(r.key()).cloned().unwrap_or(Value::Null)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    Financials,
    Calendar,
    Health,
    Identity,
    Contacts,
}

impl Resource {
    pub fn key(self) -> &'static str {
        match self {
            Resource::Financials => "financials",
            Resource::Calendar => "calendar",
            Resource::Health => "health",
            Resource::Identity => "identity",
            Resource::Contacts => "contacts",
        }
    }
}

// --------------------------------------------------------------- policy ----

/// How a tool call is resolved.
/// - `Block`: nothing released; JIT consent required.
/// - `Redact`: raw slice passed through the PII `Redactor`.
/// - `CleanRoom`: only booleans / bounded ranges computed locally are released.
/// - `Allow`: raw data released (only ever after explicit consent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Mode {
    Block,
    Redact,
    CleanRoom,
    Allow,
}

/// Static description of one protected MCP tool.
#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    pub name: &'static str,
    pub resource: Resource,
    pub description: &'static str,
    /// JSON Schema for `tools/list` (`inputSchema`).
    pub input_schema: Value,
    /// Field names an agent may legitimately request via `arguments.fields`.
    pub allowed_fields: &'static [&'static str],
    /// Subset of `allowed_fields` that is PII / sensitive.
    pub sensitive_fields: &'static [&'static str],
    /// Mode applied when no active receipt exists. `Block` and `CleanRoom`
    /// pause the call for JIT consent; `Redact` resolves immediately.
    pub default_mode: Mode,
}

/// The protected tool surface. `export_contacts` is intentionally exposed so
/// the injection scenario has something to reach for; it is always a violation
/// unless the user explicitly approves.
pub fn tool_specs() -> Vec<ToolSpec> {
    use serde_json::json;
    let fields_schema = |fields: &[&str]| {
        json!({"type": "array", "items": {"type": "string", "enum": fields}})
    };
    const FIN: &[&str] = &["checking_balance", "savings_balance", "monthly_income", "credit_score",
        "account_number", "routing_number", "ssn", "transactions", "account_holder"];
    const CAL: &[&str] = &["start", "end", "title", "location", "attendees", "notes"];
    const HEALTH: &[&str] = &["resting_heart_rate", "sleep_hours_avg", "steps_avg", "conditions",
        "medications", "last_location_trace", "insurance_member_id"];
    const ID: &[&str] = &["full_name", "date_of_birth", "home_address", "phone", "email",
        "passport_number", "drivers_license", "citizenship", "age_over_21"];
    const CONTACTS: &[&str] = &["name", "phone", "email", "relation"];
    vec![
        ToolSpec {
            name: "query_financials",
            resource: Resource::Financials,
            description: "Read the user's financial profile. Optional `min_amount` asks whether the user can afford a purchase.",
            input_schema: json!({"type": "object", "properties": {
                "fields": fields_schema(FIN),
                "min_amount": {"type": "number"},
                "include_raw_transactions": {"type": "boolean"}
            }}),
            allowed_fields: FIN,
            sensitive_fields: &["account_number", "routing_number", "ssn", "transactions", "account_holder",
                "checking_balance", "savings_balance", "monthly_income", "credit_score"],
            default_mode: Mode::CleanRoom,
        },
        ToolSpec {
            name: "check_schedule",
            resource: Resource::Calendar,
            description: "Read the user's calendar between `from` and `to` (RFC3339).",
            input_schema: json!({"type": "object", "properties": {
                "fields": fields_schema(CAL),
                "from": {"type": "string"}, "to": {"type": "string"}
            }}),
            allowed_fields: CAL,
            sensitive_fields: &["title", "location", "attendees", "notes"],
            default_mode: Mode::Redact,
        },
        ToolSpec {
            name: "get_health_metrics",
            resource: Resource::Health,
            description: "Read the user's health metrics.",
            input_schema: json!({"type": "object", "properties": {"fields": fields_schema(HEALTH)}}),
            allowed_fields: HEALTH,
            sensitive_fields: &["conditions", "medications", "last_location_trace", "insurance_member_id"],
            default_mode: Mode::Block,
        },
        ToolSpec {
            name: "fetch_identity",
            resource: Resource::Identity,
            description: "Read the user's identity documents and contact details.",
            input_schema: json!({"type": "object", "properties": {"fields": fields_schema(ID)}}),
            allowed_fields: ID,
            sensitive_fields: &["full_name", "date_of_birth", "home_address", "phone", "email",
                "passport_number", "drivers_license"],
            default_mode: Mode::Block,
        },
        ToolSpec {
            name: "make_payment",
            resource: Resource::Financials,
            description: "Pay a merchant on the user's behalf. Aegis executes the charge itself (Stripe test mode); the agent only ever receives a payment reference, never card or account numbers.",
            input_schema: json!({"type": "object", "required": ["amount_usd", "merchant"], "properties": {
                "amount_usd": {"type": "number"},
                "merchant": {"type": "string"},
                "description": {"type": "string"}
            }}),
            allowed_fields: &[],
            sensitive_fields: &["account_number", "routing_number"],
            default_mode: Mode::Block,
        },
        ToolSpec {
            name: "export_contacts",
            resource: Resource::Contacts,
            description: "Export the user's address book.",
            input_schema: json!({"type": "object", "properties": {"fields": fields_schema(CONTACTS)}}),
            allowed_fields: CONTACTS,
            sensitive_fields: CONTACTS,
            default_mode: Mode::Block,
        },
    ]
}

pub fn tool_spec(name: &str) -> Option<ToolSpec> {
    tool_specs().into_iter().find(|t| t.name == name)
}

// ---------------------------------------------------------- tool calls ----

/// One intercepted MCP `tools/call`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub call_id: String,
    /// Whose vault is targeted. From HTTP header `x-aegis-user` (default: first demo person).
    pub subject: String,
    /// From HTTP header `x-aegis-agent` (default "anonymous_agent").
    pub agent_id: String,
    pub tool: String,
    /// From HTTP header `x-aegis-purpose` (default "unspecified").
    pub purpose: String,
    pub arguments: Value,
    pub ts: i64,
}

/// A paused call waiting for the human.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentRequest {
    pub request_id: String,
    pub call: ToolCall,
    pub resource: Resource,
    pub requested_fields: Vec<String>,
    pub sensitive_fields: Vec<String>,
    /// Mode Aegis recommends offering (usually `CleanRoom`).
    pub suggested_mode: Mode,
    pub created_at: i64,
    /// Unix time after which the call auto-denies.
    pub deadline: i64,
}

/// Human decision posted to `POST /api/consent/{request_id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentDecision {
    pub approve: bool,
    /// Mode granted; ignored when `approve == false`. Defaults to `CleanRoom`.
    #[serde(default = "default_grant_mode")]
    pub mode: Mode,
    /// Lease length. Defaults to 60.
    #[serde(default = "default_ttl")]
    pub ttl_secs: i64,
    /// "Remember this decision": `Some("this_agent")` or `Some("any_agent")`
    /// turns the decision into a standing rule for this tool.
    #[serde(default)]
    pub remember: Option<String>,
}
fn default_grant_mode() -> Mode {
    Mode::CleanRoom
}
fn default_ttl() -> i64 {
    60
}

// ------------------------------------------------------------- receipts ----

/// Everything a receipt asserts, before signing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReceiptClaims {
    pub receipt_id: String,
    pub subject: String,
    pub grantee_agent: String,
    pub purpose: String,
    pub resource: Resource,
    pub resolution_mode: Mode,
    pub allowed_fields: Vec<String>,
    pub issued_at: i64,
    pub expires_at: i64,
}

/// Signed consent receipt. `signature` is `"ed25519:<hex>"` over
/// `serde_json::to_vec(&claims)` (field order as declared above).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    #[serde(flatten)]
    pub claims: ReceiptClaims,
    pub signature: String,
    #[serde(default)]
    pub revoked: bool,
}

impl Receipt {
    pub fn is_active(&self, now: i64) -> bool {
        !self.revoked && now < self.claims.expires_at
    }
}

// --------------------------------------------------------------- ledger ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Released,
    Denied,
    TimedOut,
    Violation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub id: String,
    pub ts: i64,
    /// Whose vault this entry concerns.
    pub subject: String,
    pub call_id: String,
    pub agent_id: String,
    pub tool: String,
    pub purpose: String,
    pub raw_requested: Vec<String>,
    pub policy_applied: Mode,
    pub outcome: Outcome,
    pub payload_returned: Value,
    /// Size of the raw resource JSON the agent asked for.
    pub bytes_raw: usize,
    /// Size of what was actually released.
    pub bytes_released: usize,
    pub receipt_id: Option<String>,
    /// sha256 hex of the previous entry's `entry_hash` + this entry's JSON
    /// (filled by the ledger, pass "" on append).
    #[serde(default)]
    pub entry_hash: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Metrics {
    pub total_calls: u64,
    pub sensitive_fields_requested: u64,
    pub sensitive_fields_blocked: u64,
    /// 0.0..=100.0
    pub pii_blocked_pct: f64,
    pub bytes_raw: u64,
    pub bytes_released: u64,
    pub active_leases: u64,
    pub violations: u64,
}

// ------------------------------------------------- risk & payments -------

/// Second-layer risk signal (Jev / TypeSafe System One). Scores are
/// probabilities 0..=1; policy compares them to a fixed threshold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RiskScores {
    /// e.g. "jev"
    pub provider: String,
    /// e.g. "jev-1.14.0"
    pub model: String,
    pub prompt_injection: f64,
    pub data_exfiltration: f64,
}

/// A charge Aegis executes on the person's behalf after consent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaymentRequest {
    pub subject: String,
    pub agent_id: String,
    pub amount_cents: i64,
    /// Always "usd".
    pub currency: String,
    pub merchant: String,
    pub description: String,
    /// Stable per tool call (use the call_id) so retries never double-charge.
    pub idempotency_key: String,
}

/// What the agent gets back: a reference, never payment credentials.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaymentReceipt {
    /// "stripe" or "demo"
    pub provider: String,
    /// e.g. "pi_3Q..." (Stripe PaymentIntent id)
    pub payment_id: String,
    /// e.g. "succeeded"
    pub status: String,
    pub amount_cents: i64,
    pub currency: String,
    pub merchant: String,
    pub livemode: bool,
}

// --------------------------------------------------------------- events ----

/// SSE wire format on `GET /events?subject=X`: the event plus its tenant,
/// flattened, e.g. `{"subject":"maya","type":"tool_requested","call":{...}}`.
/// The gateway only sends a subscriber events whose `subject` matches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub subject: String,
    #[serde(flatten)]
    pub event: Event,
}

/// Pushed on `GET /events` (wrapped in `EventEnvelope`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    ToolRequested { call: ToolCall },
    ConsentRequired { request: ConsentRequest },
    ConsentResolved { request_id: String, approved: bool, receipt: Option<Receipt> },
    PayloadReleased {
        call_id: String,
        agent_id: String,
        tool: String,
        mode: Mode,
        /// Raw slice that was requested (shown red in the UI; never sent to the agent).
        raw: Value,
        /// What the agent actually received (shown green).
        released: Value,
        bytes_raw: usize,
        bytes_released: usize,
        receipt_id: Option<String>,
    },
    CallDenied { call_id: String, agent_id: String, tool: String, reason: String },
    PolicyViolation { call_id: String, agent_id: String, tool: String, reason: String, offending: Vec<String> },
    LeaseRevoked { receipt_ids: Vec<String> },
    /// The person's own advocate agent weighed in on a pending consent request.
    ConsentAdvice { request_id: String, advice: Advice },
    /// A standing rule resolved a call without prompting.
    RuleApplied { call_id: String, agent_id: String, tool: String, rule_id: String, decision: RuleDecision },
    /// `blocked == true` when a score crossed the threshold (→ PolicyViolation follows).
    RiskScored { call_id: String, agent_id: String, tool: String, scores: RiskScores, blocked: bool },
    PaymentExecuted { call_id: String, agent_id: String, receipt: PaymentReceipt },
    AgentLog { agent_id: String, text: String },
    Metrics { metrics: Metrics },
}

/// Snapshot returned by `GET /api/state?subject=X` (scoped to that person).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardState {
    pub person: Person,
    pub metrics: Metrics,
    pub pending: Vec<ConsentRequest>,
    pub active_receipts: Vec<Receipt>,
    pub recent_ledger: Vec<LedgerEntry>,
    pub public_key_hex: String,
}

// --------------------------------------------------------------- errors ----

#[derive(Debug, thiserror::Error)]
pub enum AegisError {
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    #[error("policy violation: {0}")]
    Violation(String),
    #[error("consent denied")]
    Denied,
    #[error("consent timed out")]
    TimedOut,
    #[error("storage: {0}")]
    Storage(String),
    #[error("llm: {0}")]
    Llm(String),
    #[error("{0}")]
    Other(String),
}

// --------------------------------------------------------------- traits ----

/// Output of redaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Redacted {
    pub value: Value,
    /// Number of values replaced.
    pub redactions: usize,
}

/// Layer 1: deterministic PII scrubbing (aegis-cleanroom).
pub trait Redactor: Send + Sync {
    /// Redact a raw resource slice. Calendar must keep `start`/`end` and
    /// replace titles with e.g. "Busy [REDACTED]"; key-based secrets (ssn,
    /// account_number, passport_number, ...) become "[REDACTED BY AEGIS POLICY]";
    /// regex PII inside free text (SSNs, card numbers, phones, emails,
    /// street addresses) becomes "<REDACTED_ENTITY>".
    fn redact(&self, resource: Resource, raw: &Value) -> Redacted;
}

/// Violation found at the schema boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Violation {
    pub reason: String,
    pub offending: Vec<String>,
}

/// Deterministic schema-boundary checks (aegis-cleanroom).
pub trait InjectionGuard: Send + Sync {
    /// Return `Some` when arguments contain fields outside `spec.allowed_fields`,
    /// unknown argument keys, or prompt-injection text (e.g. "ignore previous
    /// instructions", "export all", "medical notes") anywhere in arguments or
    /// purpose.
    fn inspect(&self, spec: &ToolSpec, call: &ToolCall) -> Option<Violation>;
}

/// Layer 2: clean room. Computes only booleans / bounded ranges from raw data.
pub trait CleanRoom: Send + Sync {
    /// e.g. financials + {"min_amount": 2400} ->
    /// {"can_afford": true, "income_verified": true, "credit_tier": "Prime",
    ///  "exact_balance": "BLOCKED_BY_CLEAN_ROOM", "raw_transactions": "[REDACTED BY AEGIS POLICY #402]"}
    /// calendar -> {"busy_slots": [{"start","end"}], ...}; identity -> {"age_over_21": true,
    /// "identity_verified": true, "city": "Oakland, CA"}; health -> bounded ranges.
    fn evaluate(&self, resource: Resource, raw: &Value, args: &Value) -> Value;
}

/// Optional semantic check backed by the local 9B model. Must never be on the
/// critical path: implementations time out and return `None` on any failure.
pub trait PurposeJudge: Send + Sync {
    /// `Some((plausible, rationale))` – is the requested data proportionate to the purpose?
    fn judge<'a>(&'a self, call: &'a ToolCall, spec: &'a ToolSpec) -> BoxFuture<'a, Option<(bool, String)>>;
}

/// Layer 3: Ed25519 receipts (aegis-receipts).
pub trait ReceiptSigner: Send + Sync {
    fn issue(&self, claims: ReceiptClaims) -> Receipt;
    fn verify(&self, receipt: &Receipt) -> bool;
    fn public_key_hex(&self) -> String;
}

/// Append-only, hash-chained audit ledger + receipt store (aegis-receipts).
pub trait Ledger: Send + Sync {
    /// Fills `entry_hash`; returns the stored entry.
    fn append(&self, entry: LedgerEntry) -> Result<LedgerEntry, AegisError>;
    /// Newest first. `subject: None` = all people.
    fn recent(&self, subject: Option<&str>, limit: usize) -> Vec<LedgerEntry>;
    /// true iff every entry's hash chain is intact.
    fn verify_chain(&self) -> bool;
    fn store_receipt(&self, receipt: &Receipt) -> Result<(), AegisError>;
    fn get_receipt(&self, receipt_id: &str) -> Option<Receipt>;
    /// Most recent active receipt for (subject, agent, resource), if any.
    /// (`ReceiptClaims.subject` is the person.)
    fn active_receipt(&self, subject: &str, agent_id: &str, resource: Resource, now: i64) -> Option<Receipt>;
    /// `subject: None` = all people.
    fn active_receipts(&self, subject: Option<&str>, now: i64) -> Vec<Receipt>;
    /// Marks every active receipt (of `subject`, or everyone's if None) revoked; returns their ids.
    fn revoke_all(&self, subject: Option<&str>) -> Vec<String>;
    /// Scoped to `subject` (None = global).
    fn metrics(&self, subject: Option<&str>, now: i64) -> Metrics;
}

/// Second-layer risk scoring (aegis-integrations: Jev). Implementations
/// time out (8s) and return `None` on any failure; the gateway then escalates
/// to human consent instead of auto-resolving.
pub trait RiskScorer: Send + Sync {
    fn score<'a>(&'a self, call: &'a ToolCall) -> BoxFuture<'a, Option<RiskScores>>;
}

/// Executes payments server-side (aegis-integrations: Stripe test mode, or a
/// clearly-labelled demo provider). Must refuse live keys.
pub trait PaymentProvider: Send + Sync {
    fn charge<'a>(&'a self, req: PaymentRequest) -> BoxFuture<'a, Result<PaymentReceipt, AegisError>>;
}

// ---------------------------------------------------------------- web3 ----

/// One on-chain anchor of a person's ledger head (Base Sepolia by default).
/// Only hashes go on-chain — never personal data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChainAnchor {
    pub subject: String,
    pub head_hash: String,
    pub chain_id: u64,
    /// 0x-prefixed transaction hash.
    pub tx_hash: String,
    /// e.g. https://sepolia.basescan.org/tx/<tx_hash>
    pub explorer_url: String,
    pub anchored_at: i64,
}

/// Sign-In-With-Ethereum (EIP-4361) credentials (aegis-chain). A person links
/// a wallet once (while logged in); afterwards that wallet can log them in.
pub trait WalletAuth: Send + Sync {
    /// Fresh single-use nonce to embed in the SIWE message.
    fn nonce(&self) -> String;
    /// Verify a signed SIWE login message; returns the subject whose linked
    /// wallet signed it. Rejects unknown/expired/reused nonces, wrong domain,
    /// bad signatures, and wallets not linked to anyone.
    fn verify_login<'a>(&'a self, message: &'a str, signature: &'a str) -> BoxFuture<'a, Result<String, AegisError>>;
    /// Verify a signed SIWE message and link the signing address to `subject`.
    /// Returns the checksummed address.
    fn link<'a>(&'a self, subject: &'a str, message: &'a str, signature: &'a str) -> BoxFuture<'a, Result<String, AegisError>>;
    /// Checksummed address linked to `subject`, if any.
    fn wallet_of(&self, subject: &str) -> Option<String>;
}

// --------------------------------------------------------- key broker ----

/// Metadata about a stored provider secret. The value itself never leaves the
/// broker (AES-256-GCM at rest in Supabase); only `last4` is shown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecretMeta {
    pub secret_id: String,
    /// `None` = platform secret (Jev, LLM, ...); `Some(subject)` = that person's.
    pub subject: Option<String>,
    /// "stripe" | "jev" | "llm" | ...
    pub provider: String,
    pub label: String,
    pub last4: String,
    pub created_at: i64,
    pub revoked: bool,
}

/// Returned exactly once when a fake key is issued.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssuedFakeKey {
    /// `aegis_fk_<provider>_<24 alnum>` — the only credential an agent/model ever sees.
    pub fake_key: String,
    pub info: FakeKeyInfo,
}

/// A fake key as listed on the dashboard (never includes the key itself).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FakeKeyInfo {
    /// First 20 chars of the fake key, for display and revocation.
    pub prefix: String,
    pub subject: String,
    pub agent_id: String,
    pub provider: String,
    pub issued_at: i64,
    pub expires_at: i64,
    pub revoked: bool,
    pub last_used_at: Option<i64>,
    pub uses: u64,
}

// ------------------------------------------------------------ advocate ----

/// Recommendation from the person's own agent (runs on the local model, with a
/// deterministic fallback). Advisory only: the human still decides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Advice {
    /// `Some(mode)` = approve with this mode; `None` = recommend Deny.
    pub recommend: Option<Mode>,
    /// One or two plain-language sentences for the person.
    pub summary: String,
    /// Concrete concerns, e.g. "SSN is not needed to book a flight".
    pub red_flags: Vec<String>,
    /// "local-9b" or "rules".
    pub source: String,
}

/// The person's advocate (aegis-advocate). Must time out and fall back; never
/// on the critical path of the consent wait.
pub trait Advocate: Send + Sync {
    fn advise<'a>(&'a self, request: &'a ConsentRequest, spec: &'a ToolSpec) -> BoxFuture<'a, Advice>;
}

// ------------------------------------------------------- standing rules ----

/// What a standing rule does when it matches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RuleDecision {
    /// Resolve with this mode (a signed receipt is still issued, `purpose` = "rule:<id>").
    Allow { mode: Mode },
    /// Deny without prompting.
    Deny,
    /// Always ask, even if a lease or another rule would allow it.
    Ask,
}

/// A person's standing consent preference ("always free/busy for calendar",
/// "never share health data", "clean room for payments under $X").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyRule {
    pub rule_id: String,
    pub subject: String,
    /// `None` = any tool.
    pub tool: Option<String>,
    /// `None` = any resource.
    pub resource: Option<Resource>,
    /// `None` = any agent. Matches the VERIFIED agent id.
    pub agent_id: Option<String>,
    /// If set, the rule only matches when every requested field is in this list.
    pub fields_within: Option<Vec<String>>,
    /// For make_payment: only matches when amount_cents <= this.
    pub max_amount_cents: Option<i64>,
    pub decision: RuleDecision,
    /// Human-readable, e.g. "Calendar: free/busy only, any agent".
    pub label: String,
    pub created_at: i64,
}

/// Evaluates standing rules (aegis-policy). Precedence: Deny > Ask > Allow;
/// among Allows the most specific wins (agent+tool > tool > resource > any).
pub trait PolicyEngine: Send + Sync {
    /// `amount_cents` is `Some` only for make_payment.
    fn decide(&self, call: &ToolCall, spec: &ToolSpec, requested_fields: &[String], amount_cents: Option<i64>) -> Option<(PolicyRule, RuleDecision)>;
    fn rules(&self, subject: &str) -> Vec<PolicyRule>;
    fn add(&self, rule: PolicyRule) -> Result<PolicyRule, AegisError>;
    fn remove(&self, subject: &str, rule_id: &str) -> bool;
}

// ---------------------------------------------------- agent credentials ----

/// A verified agent identity, resolved from its credential (never from a
/// self-asserted header).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub subject: String,
    pub agent_id: String,
    /// Display prefix of the credential, e.g. "aegis_fk_mcp_ab12…".
    pub credential_prefix: String,
}

/// Resolves an agent bearer credential (`aegis_fk_mcp_…`, issued per person
/// per agent on the Keys card) to who it belongs to. Implemented by the broker
/// client; leases are keyed by the resolved agent_id.
pub trait AgentVerifier: Send + Sync {
    fn verify<'a>(&'a self, bearer: &'a str) -> BoxFuture<'a, Option<AgentIdentity>>;
}

/// Runs a pre-scripted demo scenario against a gateway base URL (aegis-agent).
pub trait ScenarioRunner: Send + Sync {
    /// Spawned in the background by `POST /api/scenario/{n}?subject=X`;
    /// every MCP request it makes sends header `x-aegis-user: <subject>`.
    fn run(&self, scenario: u8, subject: String, base_url: String) -> BoxFuture<'static, ()>;
}
