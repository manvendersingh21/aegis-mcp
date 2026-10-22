//! aegis-policy: standing consent rules ("always free/busy for calendar",
//! "never share health data", "clean room for payments under $X").
//!
//! Rules live in memory per subject (the hot path) and are written through to
//! Supabase table `aegis_policies (rule_id text pk, subject text, rule jsonb)`
//! by best-effort background tasks. Supabase being down never blocks or fails
//! a decision.

use aegis_core::{
    new_id, now_unix, AegisError, Mode, PolicyEngine, PolicyRule, RuleDecision, ToolCall, ToolSpec,
};
use aegis_store::SupabaseStore;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

pub const POLICY_TABLE: &str = "aegis_policies";
const PAYMENT_TOOL: &str = "make_payment";

/// `PolicyEngine` backed by an in-memory map with Supabase write-through.
pub struct StoredPolicy {
    rules: Mutex<HashMap<String, Vec<PolicyRule>>>,
    store: Option<SupabaseStore>,
}

impl StoredPolicy {
    pub fn new(store: Option<SupabaseStore>) -> Self {
        Self {
            rules: Mutex::new(HashMap::new()),
            store,
        }
    }

    /// Load every persisted rule from Supabase into memory (replacing any
    /// in-memory rule with the same id). Rows whose `rule` is null are
    /// tombstones of removed rules and are skipped. Returns the number of
    /// rules loaded; errors are logged and reported as `Err`.
    pub async fn load(&self) -> Result<usize, AegisError> {
        let Some(store) = &self.store else {
            return Ok(0);
        };
        let rows = store.select(POLICY_TABLE, "select=rule_id,subject,rule").await?;
        let mut loaded = 0;
        let mut map = self.lock();
        for row in rows {
            let Some(rule_val) = row.get("rule") else { continue };
            if rule_val.is_null() {
                // Tombstone: make sure it is gone from memory too.
                if let Some(id) = row.get("rule_id").and_then(Value::as_str) {
                    for list in map.values_mut() {
                        list.retain(|r| r.rule_id != id);
                    }
                }
                continue;
            }
            match serde_json::from_value::<PolicyRule>(rule_val.clone()) {
                Ok(rule) => {
                    let list = map.entry(rule.subject.clone()).or_default();
                    list.retain(|r| r.rule_id != rule.rule_id);
                    list.push(rule);
                    loaded += 1;
                }
                Err(e) => tracing::warn!("aegis-policy: skipping undecodable rule row: {e}"),
            }
        }
        Ok(loaded)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<PolicyRule>>> {
        self.rules.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Fire-and-forget upsert of one row; no-op without a store or runtime.
    fn persist(&self, row: Value) {
        let Some(store) = self.store.clone() else { return };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::debug!("aegis-policy: no tokio runtime; skipping Supabase write-through");
            return;
        };
        handle.spawn(async move {
            if let Err(e) = store.upsert(POLICY_TABLE, &row, "rule_id").await {
                tracing::warn!("aegis-policy: Supabase write-through failed: {e}");
            }
        });
    }
}

/// Specificity score among Allows: agent+tool > tool > resource > any.
fn specificity(rule: &PolicyRule) -> u8 {
    let mut s = 0;
    if rule.tool.is_some() {
        s += 4;
    }
    if rule.agent_id.is_some() {
        s += 2;
    }
    if rule.resource.is_some() {
        s += 1;
    }
    s
}

fn matches(
    rule: &PolicyRule,
    call: &ToolCall,
    spec: &ToolSpec,
    requested_fields: &[String],
    amount_cents: Option<i64>,
) -> bool {
    if rule.subject != call.subject {
        return false;
    }
    if let Some(tool) = &rule.tool {
        if tool != &call.tool && tool != spec.name {
            return false;
        }
    }
    if let Some(res) = rule.resource {
        if res != spec.resource {
            return false;
        }
    }
    if let Some(agent) = &rule.agent_id {
        if agent != &call.agent_id {
            return false;
        }
    }
    if let Some(within) = &rule.fields_within {
        if !requested_fields.iter().all(|f| within.contains(f)) {
            return false;
        }
    }
    if let Some(max) = rule.max_amount_cents {
        match amount_cents {
            Some(a) if a <= max => {}
            _ => return false,
        }
    }
    true
}

impl PolicyEngine for StoredPolicy {
    fn decide(
        &self,
        call: &ToolCall,
        spec: &ToolSpec,
        requested_fields: &[String],
        amount_cents: Option<i64>,
    ) -> Option<(PolicyRule, RuleDecision)> {
        let map = self.lock();
        let list = map.get(&call.subject)?;
        let matching: Vec<&PolicyRule> = list
            .iter()
            .filter(|r| matches(r, call, spec, requested_fields, amount_cents))
            .collect();

        // Within a decision class the most specific, then newest, wins.
        let best = |pred: &dyn Fn(&RuleDecision) -> bool| -> Option<PolicyRule> {
            matching
                .iter()
                .enumerate()
                .filter(|(_, r)| pred(&r.decision))
                .max_by_key(|(i, r)| (specificity(r), r.created_at, *i))
                .map(|(_, r)| (*r).clone())
        };

        let chosen = best(&|d| matches!(d, RuleDecision::Deny))
            .or_else(|| best(&|d| matches!(d, RuleDecision::Ask)))
            .or_else(|| best(&|d| matches!(d, RuleDecision::Allow { .. })))?;
        let decision = chosen.decision.clone();
        Some((chosen, decision))
    }

    fn rules(&self, subject: &str) -> Vec<PolicyRule> {
        let mut out = self.lock().get(subject).cloned().unwrap_or_default();
        out.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        out
    }

    fn add(&self, mut rule: PolicyRule) -> Result<PolicyRule, AegisError> {
        if rule.subject.trim().is_empty() {
            return Err(AegisError::Other("policy rule needs a subject".into()));
        }
        if rule.tool.as_deref() == Some(PAYMENT_TOOL)
            && rule.decision != (RuleDecision::Allow { mode: Mode::CleanRoom })
        {
            return Err(AegisError::Other(
                "make_payment rules may only be Allow{CLEAN_ROOM}".into(),
            ));
        }
        if rule.rule_id.trim().is_empty() {
            rule.rule_id = new_id("rule");
        }
        if rule.created_at == 0 {
            rule.created_at = now_unix();
        }
        {
            let mut map = self.lock();
            let list = map.entry(rule.subject.clone()).or_default();
            list.retain(|r| r.rule_id != rule.rule_id);
            list.push(rule.clone());
        }
        let rule_json = serde_json::to_value(&rule).unwrap_or(Value::Null);
        self.persist(json!({
            "rule_id": rule.rule_id,
            "subject": rule.subject,
            "rule": rule_json,
        }));
        Ok(rule)
    }

    fn remove(&self, subject: &str, rule_id: &str) -> bool {
        let removed = {
            let mut map = self.lock();
            match map.get_mut(subject) {
                Some(list) => {
                    let before = list.len();
                    list.retain(|r| r.rule_id != rule_id);
                    list.len() != before
                }
                None => false,
            }
        };
        if removed {
            // SupabaseStore has no DELETE helper: write a tombstone (rule: null)
            // which `load` skips.
            self.persist(json!({
                "rule_id": rule_id,
                "subject": subject,
                "rule": Value::Null,
            }));
        }
        removed
    }
}

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Block => "BLOCK",
        Mode::Redact => "REDACT",
        Mode::CleanRoom => "CLEAN_ROOM",
        Mode::Allow => "ALLOW",
    }
}

/// Build the standing rule for a "Remember this decision" click.
/// `scope` is `"this_agent"` (pin to `call.agent_id`) or `"any_agent"`.
pub fn rule_from_remember(
    call: &ToolCall,
    spec: &ToolSpec,
    requested_fields: &[String],
    approve: bool,
    mode: Mode,
    scope: &str,
    amount_cents: Option<i64>,
) -> PolicyRule {
    let this_agent = scope != "any_agent";
    let decision = if approve {
        RuleDecision::Allow { mode }
    } else {
        RuleDecision::Deny
    };
    let who = if this_agent { call.agent_id.as_str() } else { "any agent" };
    let what = if approve { mode_label(mode) } else { "DENY" };
    let max_amount_cents = if spec.name == PAYMENT_TOOL { amount_cents } else { None };
    PolicyRule {
        rule_id: String::new(),
        subject: call.subject.clone(),
        tool: Some(spec.name.to_string()),
        resource: Some(spec.resource),
        agent_id: this_agent.then(|| call.agent_id.clone()),
        fields_within: Some(requested_fields.to_vec()),
        max_amount_cents,
        decision,
        label: format!("{who} · {} · {what} (remembered)", spec.name),
        created_at: 0,
    }
}
