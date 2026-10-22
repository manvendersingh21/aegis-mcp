//! aegis-cleanroom: deterministic PII redaction, schema-boundary injection
//! guard, local data clean room, and the optional LLM purpose judge.

use aegis_core::{
    BoxFuture, CleanRoom, InjectionGuard, LlmClient, PurposeJudge, Redacted, Redactor, Resource,
    ToolCall, ToolSpec, Violation,
};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::time::Duration;

const POLICY_MARKER: &str = "[REDACTED BY AEGIS POLICY]";
const TX_MARKER: &str = "[REDACTED BY AEGIS POLICY #402]";
const ENTITY_MARKER: &str = "<REDACTED_ENTITY>";
const BUSY_TITLE: &str = "Busy [REDACTED]";

const SENSITIVE_KEYS: &[&str] = &[
    "account_holder",
    "checking_balance",
    "savings_balance",
    "monthly_income",
    "credit_score",
    "account_number",
    "routing_number",
    "ssn",
    "conditions",
    "medications",
    "last_location_trace",
    "insurance_member_id",
    "full_name",
    "date_of_birth",
    "home_address",
    "phone",
    "email",
    "passport_number",
    "drivers_license",
    "location",
    "attendees",
    "notes",
    "name",
    "relation",
];

// ------------------------------------------------------------- redactor ----

/// Layer 1: key-based redaction plus regex PII scrubbing inside free text.
pub struct RegexRedactor {
    patterns: Vec<Regex>,
}

impl RegexRedactor {
    pub fn new() -> Self {
        let sources = [
            r"\b\d{3}-\d{2}-\d{4}\b",
            r"\b\d[\d -]{12,24}\d\b",
            r"\+\d{1,2}[ .-]\d{4,6}[ .-]\d{4,6}\b",
            r"(?:\+?\d{1,2}[ .-]?)?\(?\d{3}\)?[ .-]?\d{3}[ .-]?\d{4}\b",
            r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}",
            r"(?i)\b\d{1,5}\s+(?:[A-Za-z0-9.'\-]+\s+){0,3}(?:st|street|ave|avenue|rd|road|blvd|boulevard|ln|lane|dr|drive|ct|court|way|ter|terrace|pl|place|cir|circle|hwy|highway|pkwy|parkway|broadway)\b\.?(?:\s+(?:apt|apartment|suite|ste|unit|#)\.?\s*[A-Za-z0-9\-]+)?(?:,\s*[A-Za-z][A-Za-z .]*?,?\s*[A-Z]{2}\s+\d{5}(?:-\d{4})?)?",
        ];
        let patterns = sources.iter().filter_map(|s| Regex::new(s).ok()).collect();
        RegexRedactor { patterns }
    }

    fn scrub(&self, s: &str) -> (String, usize) {
        let mut out = s.to_string();
        let mut hits_total = 0;
        for re in &self.patterns {
            let hits = re.find_iter(&out).filter(|m| !m.as_str().is_empty()).count();
            if hits > 0 {
                out = re.replace_all(&out, ENTITY_MARKER).into_owned();
                hits_total += hits;
            }
        }
        (out, hits_total)
    }

    fn walk(&self, key: Option<&str>, v: &Value, depth: usize, count: &mut usize) -> Value {
        if depth > 24 {
            *count += 1;
            return Value::String(POLICY_MARKER.to_string());
        }
        if let Some(k) = key {
            if k == "title" && v.is_string() {
                *count += 1;
                return Value::String(BUSY_TITLE.to_string());
            }
            if k == "transactions" {
                *count += 1;
                return Value::String(TX_MARKER.to_string());
            }
            if !v.is_null() && SENSITIVE_KEYS.contains(&k) {
                *count += 1;
                return Value::String(POLICY_MARKER.to_string());
            }
        }
        match v {
            Value::Object(m) => {
                let mut out = Map::with_capacity(m.len());
                for (k, val) in m {
                    out.insert(k.clone(), self.walk(Some(k), val, depth + 1, count));
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(
                a.iter()
                    .map(|e| self.walk(None, e, depth + 1, count))
                    .collect(),
            ),
            Value::String(s) => {
                if matches!(key, Some("start") | Some("end")) {
                    return v.clone();
                }
                let (scrubbed, hits) = self.scrub(s);
                *count += hits;
                Value::String(scrubbed)
            }
            _ => v.clone(),
        }
    }
}

impl Default for RegexRedactor {
    fn default() -> Self {
        Self::new()
    }
}

impl Redactor for RegexRedactor {
    fn redact(&self, resource: Resource, raw: &Value) -> Redacted {
        let _ = resource;
        let mut count = 0;
        let value = self.walk(None, raw, 0, &mut count);
        Redacted { value, redactions: count }
    }
}

// --------------------------------------------------------- schema guard ----

const INJECTION_PHRASES: &[&str] = &[
    "ignore previous",
    "ignore prior",
    "previous instructions",
    "prior instructions",
    "export all",
    "dump",
    "system prompt",
    "medical notes",
    "bypass",
    "override",
];

/// Deterministic checks at the schema boundary: unknown fields, unknown
/// argument keys, and prompt-injection phrases in purpose or string args.
#[derive(Debug, Clone, Copy, Default)]
pub struct SchemaGuard;

impl SchemaGuard {
    pub fn new() -> Self {
        Self
    }
}

fn collect_strings(v: &Value, depth: usize, out: &mut Vec<String>) {
    if depth > 16 {
        return;
    }
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => {
            for e in a {
                collect_strings(e, depth + 1, out);
            }
        }
        Value::Object(m) => {
            for e in m.values() {
                collect_strings(e, depth + 1, out);
            }
        }
        _ => {}
    }
}

impl InjectionGuard for SchemaGuard {
    fn inspect(&self, spec: &ToolSpec, call: &ToolCall) -> Option<Violation> {
        let mut offending: Vec<String> = Vec::new();

        let allowed_arg_keys: Vec<&str> = spec
            .input_schema
            .get("properties")
            .and_then(Value::as_object)
            .map(|m| m.keys().map(String::as_str).collect())
            .unwrap_or_default();

        if let Some(args) = call.arguments.as_object() {
            for k in args.keys() {
                if !allowed_arg_keys.contains(&k.as_str()) {
                    offending.push(format!("unknown_argument:{k}"));
                }
            }
        }

        if let Some(fields) = call.arguments.get("fields").and_then(Value::as_array) {
            for f in fields {
                match f.as_str() {
                    Some(name) => {
                        if !spec.allowed_fields.contains(&name) {
                            offending.push(format!("unknown_field:{name}"));
                        }
                    }
                    None => offending.push("non_string_field".to_string()),
                }
            }
        }

        let mut texts = vec![call.purpose.clone()];
        collect_strings(&call.arguments, 0, &mut texts);
        let haystack = texts.join(" \n ").to_lowercase();
        for phrase in INJECTION_PHRASES {
            if haystack.contains(phrase) {
                offending.push(format!("injection:{phrase}"));
            }
        }

        if offending.is_empty() {
            None
        } else {
            let reason = format!("AEGIS POLICY VIOLATION: {}", offending.join("; "));
            Some(Violation { reason, offending })
        }
    }
}

// ----------------------------------------------------------- clean room ----

/// Layer 2: releases only booleans, bounded bands and counts — never raw
/// values — computed locally from the vault slice.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalCleanRoom;

impl LocalCleanRoom {
    pub fn new() -> Self {
        Self
    }
}

fn credit_tier(score: i64) -> &'static str {
    if score >= 740 {
        "Prime"
    } else if score >= 670 {
        "Near-prime"
    } else {
        "Subprime"
    }
}

fn balance_band(total: f64) -> String {
    match total as i64 {
        i64::MIN..=4_999 => "0-5k".to_string(),
        5_000..=9_999 => "5k-10k".to_string(),
        10_000..=24_999 => "10k-25k".to_string(),
        25_000..=49_999 => "25k-50k".to_string(),
        50_000..=99_999 => "50k-100k".to_string(),
        _ => "100k+".to_string(),
    }
}

fn city_state(addr: &str) -> Option<String> {
    let parts: Vec<&str> = addr.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    if parts.len() >= 3 {
        let state = parts[parts.len() - 1].split_whitespace().next()?;
        if state.len() == 2 && state.chars().all(|c| c.is_ascii_alphabetic()) {
            return Some(format!("{}, {}", parts[parts.len() - 2], state.to_uppercase()));
        }
    }
    None
}

fn non_empty_str(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_str).map(|s| !s.is_empty()).unwrap_or(false)
}

impl CleanRoom for LocalCleanRoom {
    fn evaluate(&self, resource: Resource, raw: &Value, args: &Value) -> Value {
        match resource {
            Resource::Financials => {
                let checking = raw.get("checking_balance").and_then(Value::as_f64).unwrap_or(0.0);
                let savings = raw.get("savings_balance").and_then(Value::as_f64).unwrap_or(0.0);
                let income = raw.get("monthly_income").and_then(Value::as_f64).unwrap_or(0.0);
                let score = raw.get("credit_score").and_then(Value::as_i64).unwrap_or(0);
                let mut out = Map::new();
                if let Some(min) = args.get("min_amount").and_then(Value::as_f64) {
                    out.insert("can_afford".into(), json!(checking >= min));
                }
                out.insert("income_verified".into(), json!(income > 0.0));
                out.insert("credit_tier".into(), json!(credit_tier(score)));
                out.insert("balance_band".into(), json!(balance_band(checking + savings)));
                out.insert("exact_balance".into(), json!("BLOCKED_BY_CLEAN_ROOM"));
                if raw.get("transactions").is_some() {
                    out.insert("raw_transactions".into(), json!(TX_MARKER));
                }
                Value::Object(out)
            }
            Resource::Calendar => {
                let events = raw.get("events").and_then(Value::as_array).cloned().unwrap_or_default();
                let mut busy = Vec::new();
                let mut free_slots = 0;
                for e in &events {
                    let start = e.get("start").and_then(Value::as_str);
                    let end = e.get("end").and_then(Value::as_str);
                    if let (Some(s), Some(t)) = (start, end) {
                        busy.push(json!({"start": s, "end": t}));
                    }
                    let solo = e
                        .get("attendees")
                        .and_then(Value::as_array)
                        .map(|a| a.is_empty())
                        .unwrap_or(true);
                    if solo {
                        free_slots += 1;
                    }
                }
                json!({"busy_slots": busy, "free_slots": free_slots})
            }
            Resource::Identity => {
                let verified = non_empty_str(raw, "full_name") && non_empty_str(raw, "date_of_birth");
                let city = raw
                    .get("home_address")
                    .and_then(Value::as_str)
                    .and_then(city_state);
                json!({
                    "age_over_21": raw.get("age_over_21").and_then(Value::as_bool).unwrap_or(false),
                    "identity_verified": verified,
                    "city": city,
                })
            }
            Resource::Health => {
                let hr = raw.get("resting_heart_rate").and_then(Value::as_i64);
                let hr_band = match hr {
                    Some(h) if h < 60 => "low",
                    Some(h) if h <= 100 => "normal",
                    Some(_) => "elevated",
                    None => "unknown",
                };
                let sleep = raw.get("sleep_hours_avg").and_then(Value::as_f64);
                let sleep_band = match sleep {
                    Some(s) if s < 7.0 => "low",
                    Some(s) if s <= 9.0 => "normal",
                    Some(_) => "high",
                    None => "unknown",
                };
                let steps = raw.get("steps_avg").and_then(Value::as_i64);
                let steps_band = match steps {
                    Some(s) if s < 5_000 => "low",
                    Some(s) if s <= 10_000 => "moderate",
                    Some(_) => "high",
                    None => "unknown",
                };
                json!({
                    "resting_hr_band": hr_band,
                    "sleep_band": sleep_band,
                    "steps_band": steps_band,
                    "conditions_count": raw.get("conditions").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0),
                    "medications_count": raw.get("medications").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0),
                })
            }
            Resource::Contacts => {
                json!({
                    "contact_count": raw.as_array().map(|a| a.len()).unwrap_or(0),
                })
            }
        }
    }
}

// --------------------------------------------------------- purpose judge ----

/// Optional semantic purpose check backed by the local LLM. Never blocks the
/// pipeline: 8s timeout, `None` on any failure.
pub struct LlmPurposeJudge {
    llm: LlmClient,
}

impl LlmPurposeJudge {
    pub fn new(llm: LlmClient) -> Self {
        LlmPurposeJudge { llm }
    }
}

impl PurposeJudge for LlmPurposeJudge {
    fn judge<'a>(&'a self, call: &'a ToolCall, spec: &'a ToolSpec) -> BoxFuture<'a, Option<(bool, String)>> {
        Box::pin(async move {
            let system = "You are the Aegis consent-firewall purpose judge. Answer whether the data \
                request is plausible and proportionate to the agent's stated purpose.";
            let user = format!(
                "Agent: {}\nTool: {} (resource: {})\nPurpose: {}\nArguments: {}\n\nIs this data \
                request proportionate to the stated purpose? Reply as JSON.",
                call.agent_id, spec.name, spec.resource.key(), call.purpose, call.arguments
            );
            let schema = json!({
                "type": "object",
                "properties": {
                    "plausible": {"type": "boolean"},
                    "rationale": {"type": "string"}
                },
                "required": ["plausible", "rationale"]
            });
            let out = tokio::time::timeout(Duration::from_secs(8), self.llm.chat(system, &user, Some(schema)))
                .await
                .ok()?
                .ok()?;
            let v: Value = serde_json::from_str(&out).ok()?;
            let plausible = v.get("plausible").and_then(Value::as_bool)?;
            let rationale = v
                .get("rationale")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Some((plausible, rationale))
        })
    }
}
