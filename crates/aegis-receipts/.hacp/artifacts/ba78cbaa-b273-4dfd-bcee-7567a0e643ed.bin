use aegis_core::*;
use aegis_receipts::{Ed25519Signer, SqliteLedger};
use serde_json::json;

fn claims(subject: &str, agent: &str, ttl: i64) -> ReceiptClaims {
    let now = now_unix();
    ReceiptClaims { receipt_id: new_id("rcpt"), subject: subject.into(), grantee_agent: agent.into(),
        purpose: "flight_booking_verification".into(), resource: Resource::Financials, resolution_mode: Mode::CleanRoom,
        allowed_fields: vec!["checking_balance".into()], issued_at: now, expires_at: now + ttl }
}

fn entry(i: usize) -> LedgerEntry {
    LedgerEntry { id: format!("e{i}"), ts: now_unix(), subject: if i == 0 { "dev".into() } else { "maya".into() }, call_id: format!("c{i}"), agent_id: "a".into(), tool: "query_financials".into(),
        purpose: "p".into(), raw_requested: vec!["ssn".into()], policy_applied: Mode::CleanRoom, outcome: Outcome::Released,
        payload_returned: json!({"can_afford": true}), bytes_raw: 900, bytes_released: 40, receipt_id: None, entry_hash: String::new() }
}

#[test]
fn sign_and_verify() {
    let s = Ed25519Signer::generate();
    let r = s.issue(claims("maya", "bot", 60));
    assert!(r.signature.starts_with("ed25519:"));
    assert!(s.verify(&r));
    let mut tampered = r.clone();
    tampered.claims.expires_at += 3600;
    assert!(!s.verify(&tampered));
    assert_eq!(s.public_key_hex().len(), 64);
}

#[test]
fn receipts_leases_and_revoke() {
    let s = Ed25519Signer::generate();
    let l = SqliteLedger::open_in_memory().unwrap();
    let r = s.issue(claims("maya", "bot", 60));
    l.store_receipt(&r).unwrap();
    let devs = s.issue(claims("dev", "bot", 60));
    l.store_receipt(&devs).unwrap();
    let expired = s.issue(claims("maya", "bot2", -5));
    l.store_receipt(&expired).unwrap();
    let now = now_unix();
    assert_eq!(l.get_receipt(&r.claims.receipt_id).unwrap().claims, r.claims);
    assert!(l.active_receipt("maya", "bot", Resource::Financials, now).is_some());
    assert!(l.active_receipt("maya", "bot", Resource::Health, now).is_none());
    assert!(l.active_receipt("maya", "bot2", Resource::Financials, now).is_none());
    assert_eq!(l.active_receipts(Some("maya"), now).len(), 1);
    assert_eq!(l.active_receipts(None, now).len(), 2);
    assert_eq!(l.revoke_all(Some("maya")), vec![r.claims.receipt_id.clone()]);
    assert!(l.active_receipt("maya", "bot", Resource::Financials, now).is_none());
    assert!(l.active_receipt("dev", "bot", Resource::Financials, now).is_some(), "dev's lease untouched");
    assert!(l.get_receipt(&r.claims.receipt_id).unwrap().revoked);
}

#[test]
fn ledger_chain_and_metrics() {
    let l = SqliteLedger::open_in_memory().unwrap();
    for i in 0..3 { let e = l.append(entry(i)).unwrap(); assert_eq!(e.entry_hash.len(), 64); }
    let recent = l.recent(None, 10);
    assert_eq!(recent.len(), 3);
    assert_eq!(l.recent(Some("maya"), 10).len(), 2);
    assert_eq!(recent[0].id, "e2", "newest first");
    assert!(l.verify_chain());
    let m = l.metrics(None, now_unix());
    assert_eq!(m.total_calls, 3);
    assert_eq!(m.bytes_raw, 2700);
    assert_eq!(l.metrics(Some("dev"), now_unix()).total_calls, 1);
}
