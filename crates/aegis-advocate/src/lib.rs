//! aegis-advocate: an agent that works for the person.
//!
//! `RulesAdvocate` is deterministic and offline. `LlmAdvocate` asks the local
//! model (metadata only, never vault values) and then keeps whichever of the
//! model's and the rules' recommendation is MORE protective, because the local
//! model tends to approve everything.

use aegis_core::{Advice, Advocate, BoxFuture, ConsentRequest, LlmClient, Mode, ToolSpec};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

// ------------------------------------------------------------- purpose ----

/// Coarse purpose classes the rules understand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Travel,
    LeaseCredit,
    Scheduling,
    HealthInsurance,
    Shopping,
    Unknown,
}

/// Keyword classification of a free-text purpose (header `x-aegis-purpose`).
pub fn classify_purpose(purpose: &str) -> Purpose {
    let p = purpose.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| p.contains(w));
    if has(&["flight", "travel", "trip", "hotel", "booking", "book a", "airline", "itinerary", "vacation", "rental car"]) {
        Purpose::Travel
    } else if has(&["lease", "rent", "apartment", "landlord", "tenant", "credit", "loan", "mortgage", "underwrit"]) {
        Purpose::LeaseCredit
    } else if has(&["schedul", "meeting", "calendar", "appointment", "availability", "free/busy", "free busy", "call with", "sync"]) {
        Purpose::Scheduling
    } else if has(&["health", "insurance", "medical", "doctor", "clinic", "wellness", "fitness", "premium"]) {
        Purpose::HealthInsurance
    } else if has(&["shop", "purchase", "buy", "order", "checkout", "payment", "pay ", "merchant", "cart", "subscription"]) {
        Purpose::Shopping
    } else {
        Purpose::Unknown
    }
}

/// Identifiers / medical data whose release is irreversible. When a purpose
/// doesn't need one of these, the advocate recommends Deny (except on
/// `query_financials`, where Clean room never releases raw values).
const REGULATED: &[&str] = &[
    "ssn", "account_number", "routing_number", "passport_number", "drivers_license",
    "conditions", "medications", "insurance_member_id", "last_location_trace",
];

/// Fields a purpose legitimately needs, per tool.
fn needed_fields(purpose: Purpose, tool: &str) -> &'static [&'static str] {
    use Purpose::*;
    match (tool, purpose) {
        ("query_financials", Travel | Shopping) => &["checking_balance", "savings_balance"],
        ("query_financials", LeaseCredit) => {
            &["checking_balance", "savings_balance", "monthly_income", "credit_score", "ssn"]
        }
        ("query_financials", HealthInsurance) => &["monthly_income"],
        ("query_financials", Scheduling | Unknown) => &[],
        ("check_schedule", Scheduling) => &["start", "end", "title", "location", "attendees"],
        ("check_schedule", Travel) => &["start", "end", "location"],
        ("check_schedule", _) => &["start", "end"],
        ("get_health_metrics", HealthInsurance) => &[
            "resting_heart_rate", "sleep_hours_avg", "steps_avg", "conditions", "medications", "insurance_member_id",
        ],
        ("get_health_metrics", _) => &[],
        ("fetch_identity", Travel) => &["full_name", "date_of_birth", "email", "phone", "passport_number", "citizenship"],
        ("fetch_identity", LeaseCredit) => &["full_name", "date_of_birth", "home_address", "email", "phone", "drivers_license"],
        ("fetch_identity", Scheduling) => &["full_name", "email"],
        ("fetch_identity", HealthInsurance) => &["full_name", "date_of_birth", "home_address", "email", "phone"],
        ("fetch_identity", Shopping) => &["full_name", "home_address", "email", "phone", "age_over_21"],
        ("fetch_identity", Unknown) => &["age_over_21"],
        _ => &[],
    }
}

fn field_label(f: &str) -> String {
    match f {
        "ssn" => "SSN".into(),
        "account_number" => "bank account number".into(),
        "routing_number" => "bank routing number".into(),
        "passport_number" => "passport number".into(),
        "drivers_license" => "driver's license".into(),
        "date_of_birth" => "date of birth".into(),
        "home_address" => "home address".into(),
        "insurance_member_id" => "insurance member ID".into(),
        "last_location_trace" => "location history".into(),
        "conditions" => "medical conditions".into(),
        "transactions" => "transaction history".into(),
        "title" => "event titles".into(),
        "notes" => "event notes".into(),
        "attendees" => "meeting attendees".into(),
        other => other.replace('_', " "),
    }
}

fn join_labels(fields: &[String]) -> String {
    let labels: Vec<String> = fields.iter().map(|f| field_label(f)).collect();
    match labels.len() {
        0 => "data".into(),
        1 => labels[0].clone(),
        2 => format!("{} and {}", labels[0], labels[1]),
        n => format!("{}, and {}", labels[..n - 1].join(", "), labels[n - 1]),
    }
}

fn purpose_phrase(purpose: Purpose, raw: &str) -> String {
    let p = raw.to_lowercase();
    match purpose {
        Purpose::Travel if p.contains("flight") => "book a flight".into(),
        Purpose::Travel if p.contains("hotel") => "book a hotel".into(),
        Purpose::Travel => "book your trip".into(),
        Purpose::LeaseCredit if p.contains("lease") || p.contains("rent") || p.contains("apartment") => {
            "process a rental application".into()
        }
        Purpose::LeaseCredit => "run a credit check".into(),
        Purpose::Scheduling => "schedule a meeting".into(),
        Purpose::HealthInsurance => "handle a health or insurance request".into(),
        Purpose::Shopping => "complete a purchase".into(),
        Purpose::Unknown => "do something it hasn't explained".into(),
    }
}

fn need_phrase(purpose: Purpose, tool: &str) -> &'static str {
    match (tool, purpose) {
        ("query_financials", Purpose::Travel) => "A booking only needs to know you can afford it",
        ("query_financials", Purpose::Shopping) => "A purchase only needs to know you can afford it",
        ("query_financials", Purpose::LeaseCredit) => "A landlord or lender only needs yes/no answers about income and credit",
        ("query_financials", _) => "This only needs a yes/no answer, never your raw financial records",
        ("check_schedule", Purpose::Scheduling) => "Scheduling only needs your free/busy times; titles and details can be masked",
        ("check_schedule", _) => "Your free/busy times are enough; event details can be masked",
        ("fetch_identity", _) => "Only basic contact details are needed, with sensitive parts masked",
        ("get_health_metrics", Purpose::HealthInsurance) => "Health questions can be answered without releasing raw records",
        ("make_payment", _) => "Aegis pays the merchant itself, so the agent never sees your card or account numbers",
        _ => "Only a minimal, masked answer is needed",
    }
}

fn mode_label(m: Option<Mode>) -> &'static str {
    match m {
        Some(Mode::CleanRoom) => "Clean room",
        Some(Mode::Redact) => "Redact",
        Some(Mode::Allow) => "Allow",
        Some(Mode::Block) | None => "Deny",
    }
}

// --------------------------------------------------------------- rules ----

/// Deterministic, offline advocate.
#[derive(Debug, Clone, Default)]
pub struct RulesAdvocate;

impl RulesAdvocate {
    pub fn new() -> Self {
        RulesAdvocate
    }

    /// Synchronous core of the rules advocate.
    pub fn advise_sync(&self, request: &ConsentRequest, spec: &ToolSpec) -> Advice {
        let call = &request.call;
        let agent = &call.agent_id;
        let purpose = classify_purpose(&call.purpose);
        let phrase = purpose_phrase(purpose, &call.purpose);
        let tool = spec.name;

        // Requested fields; fall back to the sensitive list if none were named.
        let mut requested: Vec<String> = request.requested_fields.clone();
        if requested.is_empty() && tool != "make_payment" {
            requested = request.sensitive_fields.clone();
        }
        let is_sensitive = |f: &str| {
            request.sensitive_fields.iter().any(|s| s == f) || spec.sensitive_fields.contains(&f)
        };
        let sensitive_requested: Vec<String> = requested.iter().filter(|f| is_sensitive(f)).cloned().collect();
        let asked = join_labels(if sensitive_requested.is_empty() { &requested } else { &sensitive_requested });

        // export_contacts: always deny.
        if tool == "export_contacts" {
            let red_flags = if requested.is_empty() {
                vec!["Exporting your whole address book exposes other people's data and is never needed".to_string()]
            } else {
                requested
                    .iter()
                    .map(|f| format!("{} of your contacts is not yours to share (export_contacts)", field_label(f)))
                    .collect()
            };
            return Advice {
                recommend: None,
                summary: format!(
                    "{agent} wants to export your entire address book. Nothing it says it is doing needs your contacts' data — I recommend you deny it."
                ),
                red_flags,
                source: "rules".into(),
            };
        }

        // make_payment: Aegis executes the charge; clean room if the purpose fits.
        if tool == "make_payment" {
            let amount = call.arguments.get("amount_usd").and_then(|v| v.as_f64());
            let merchant = call.arguments.get("merchant").and_then(|v| v.as_str()).unwrap_or("a merchant");
            let what = match amount {
                Some(a) => format!("${a:.2} to {merchant}"),
                None => format!("money to {merchant}"),
            };
            if purpose == Purpose::Unknown {
                return Advice {
                    recommend: None,
                    summary: format!("{agent} wants to pay {what} without saying why. I recommend you deny it."),
                    red_flags: vec!["Payment requested with no stated purpose".into()],
                    source: "rules".into(),
                };
            }
            return Advice {
                recommend: Some(Mode::CleanRoom),
                summary: format!(
                    "{agent} wants to pay {what} to {phrase}. {} — approve Clean room.",
                    need_phrase(purpose, tool)
                ),
                red_flags: vec![],
                source: "rules".into(),
            };
        }

        let needed = needed_fields(purpose, tool);
        let mut red_flags = Vec::new();
        let mut regulated_unneeded = false;
        for f in &requested {
            let needed_here = needed.contains(&f.as_str());
            // Raw transactions are never needed for a yes/no money question.
            if needed_here && f != "transactions" {
                continue;
            }
            if !is_sensitive(f) && !REGULATED.contains(&f.as_str()) {
                continue;
            }
            if REGULATED.contains(&f.as_str()) {
                regulated_unneeded = true;
            }
            red_flags.push(format!("{} is not needed to {}", field_label(f), phrase));
        }
        if tool == "query_financials"
            && call.arguments.get("include_raw_transactions").and_then(|v| v.as_bool()) == Some(true)
            && !red_flags.iter().any(|r| r.starts_with("transaction history"))
        {
            red_flags.push(format!("raw transaction history is not needed to {phrase}"));
        }

        // Unknown purpose + any sensitive field: deny.
        let deny = if purpose == Purpose::Unknown && !sensitive_requested.is_empty() {
            true
        } else if tool == "query_financials" {
            // Clean room never releases raw values, so even unneeded
            // identifiers can't leak; deny only if nothing here is needed.
            purpose == Purpose::Scheduling && !sensitive_requested.is_empty()
        } else {
            regulated_unneeded
        };

        if deny {
            let why = if purpose == Purpose::Unknown {
                "It hasn't said why it needs them".to_string()
            } else {
                format!("None of that is needed to {phrase}")
            };
            return Advice {
                recommend: None,
                summary: format!("{agent} asks for your {asked} to {phrase}. {why} — I recommend you deny it."),
                red_flags,
                source: "rules".into(),
            };
        }

        let mode = match tool {
            "query_financials" => Mode::CleanRoom,
            "get_health_metrics"
                if requested.iter().any(|f| f == "conditions" || f == "medications") =>
            {
                Mode::CleanRoom
            }
            _ => Mode::Redact,
        };
        Advice {
            recommend: Some(mode),
            summary: format!(
                "{agent} asks for your {asked} to {phrase}. {} — approve {}.",
                need_phrase(purpose, tool),
                mode_label(Some(mode))
            ),
            red_flags,
            source: "rules".into(),
        }
    }
}

impl Advocate for RulesAdvocate {
    fn advise<'a>(&'a self, request: &'a ConsentRequest, spec: &'a ToolSpec) -> BoxFuture<'a, Advice> {
        Box::pin(async move { self.advise_sync(request, spec) })
    }
}

// ----------------------------------------------------------------- llm ----

/// Higher = more protective. DENY > CLEAN_ROOM > REDACT > ALLOW.
pub fn protectiveness(m: Option<Mode>) -> u8 {
    match m {
        None | Some(Mode::Block) => 3,
        Some(Mode::CleanRoom) => 2,
        Some(Mode::Redact) => 1,
        Some(Mode::Allow) => 0,
    }
}

#[derive(Deserialize)]
struct LlmOut {
    recommend: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    red_flags: Vec<String>,
}

fn parse_mode(s: &str) -> Option<Option<Mode>> {
    match s.trim().to_ascii_uppercase().replace([' ', '-'], "_").as_str() {
        "ALLOW" => Some(Some(Mode::Allow)),
        "REDACT" => Some(Some(Mode::Redact)),
        "CLEAN_ROOM" | "CLEANROOM" => Some(Some(Mode::CleanRoom)),
        "DENY" | "BLOCK" => Some(None),
        _ => None,
    }
}

const SYSTEM_PROMPT: &str = "You are the person's own privacy advocate inside AegisMCP, a consent firewall. \
An outside AI agent is asking to read the person's data. Decide the most protective option that still serves \
the stated purpose: ALLOW (raw data), REDACT (PII masked), CLEAN_ROOM (only yes/no or bounded answers computed \
locally), or DENY. Regulated identifiers (SSN, bank account/routing numbers, passport, driver's license) and \
medical data are almost never needed. Reply with JSON {recommend, summary, red_flags}: summary is 1-2 plain \
sentences addressed to the person; red_flags lists each requested field the purpose does not need.";

/// Advocate backed by the local model, floored by `RulesAdvocate`.
#[derive(Clone)]
pub struct LlmAdvocate {
    llm: LlmClient,
    rules: RulesAdvocate,
    timeout: Duration,
}

impl LlmAdvocate {
    pub fn new(llm: LlmClient) -> Self {
        LlmAdvocate { llm, rules: RulesAdvocate::new(), timeout: Duration::from_secs(10) }
    }

    /// Metadata-only prompt: never includes vault values or call arguments.
    pub fn build_prompt(request: &ConsentRequest, spec: &ToolSpec) -> String {
        json!({
            "agent_id": request.call.agent_id,
            "tool": spec.name,
            "tool_description": spec.description,
            "purpose": request.call.purpose,
            "requested_fields": request.requested_fields,
            "sensitive_fields": request.sensitive_fields,
        })
        .to_string()
    }

    fn schema() -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "recommend": {"type": "string", "enum": ["ALLOW", "REDACT", "CLEAN_ROOM", "DENY"]},
                "summary": {"type": "string"},
                "red_flags": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["recommend", "summary", "red_flags"]
        })
    }

    async fn ask_model(&self, request: &ConsentRequest, spec: &ToolSpec) -> Option<(Option<Mode>, String, Vec<String>)> {
        let user = Self::build_prompt(request, spec);
        let raw = tokio::time::timeout(self.timeout, self.llm.chat(SYSTEM_PROMPT, &user, Some(Self::schema())))
            .await
            .ok()?
            .ok()?;
        let out: LlmOut = serde_json::from_str(raw.trim()).ok()?;
        let mode = parse_mode(&out.recommend)?;
        Some((mode, out.summary.trim().to_string(), out.red_flags))
    }
}

/// Combine model and rules advice: more protective recommendation wins.
pub fn merge_advice(model: (Option<Mode>, String, Vec<String>), rules: Advice) -> Advice {
    let (m_mode, m_summary, m_flags) = model;
    let model_wins = protectiveness(m_mode) > protectiveness(rules.recommend)
        || (protectiveness(m_mode) == protectiveness(rules.recommend) && !m_summary.is_empty());
    let (recommend, summary) = if model_wins && !m_summary.is_empty() {
        (if protectiveness(m_mode) == 3 { None } else { m_mode }, m_summary)
    } else if model_wins {
        (if protectiveness(m_mode) == 3 { None } else { m_mode }, rules.summary.clone())
    } else {
        (rules.recommend, rules.summary.clone())
    };
    let mut red_flags: Vec<String> = Vec::new();
    for f in rules.red_flags.into_iter().chain(m_flags) {
        let f = f.trim().to_string();
        if f.is_empty() || red_flags.iter().any(|x| x.eq_ignore_ascii_case(&f)) {
            continue;
        }
        red_flags.push(f);
    }
    Advice { recommend, summary, red_flags, source: "local-9b".into() }
}

impl Advocate for LlmAdvocate {
    fn advise<'a>(&'a self, request: &'a ConsentRequest, spec: &'a ToolSpec) -> BoxFuture<'a, Advice> {
        Box::pin(async move {
            let rules = self.rules.advise_sync(request, spec);
            match self.ask_model(request, spec).await {
                Some(model) => merge_advice(model, rules),
                None => rules,
            }
        })
    }
}
