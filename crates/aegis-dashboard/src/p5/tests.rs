//! PHASE 5 dashboard checks: wallet-only `/login` (SIWE + labelled DEMO
//! wallets, no password anywhere) and the Keys card + `KEYS_JS` (write-only
//! provider secrets, fake keys shown once, escaping of server strings).

use super::*;
use aegis_core::demo_vaults;

fn assert_has(hay: &str, needles: &[&str], ctx: &str) {
    for n in needles {
        assert!(hay.contains(n), "{ctx}: missing {n:?}");
    }
}

/// Body of the JS function / handler starting at `marker`, up to the next `marker_end`.
fn section<'a>(hay: &'a str, start: &str, end: &str) -> &'a str {
    let i = hay.find(start).unwrap_or_else(|| panic!("missing {start:?}"));
    let rest = &hay[i..];
    let j = rest[start.len()..].find(end).map(|j| j + start.len()).unwrap_or(rest.len());
    &rest[..j]
}

#[test]
fn login_page_is_wallet_only() {
    let html = login_page().into_string();
    let lower = html.to_lowercase();
    assert!(!lower.contains("type=\"password\""), "login page must not have a password input");
    assert!(!lower.contains("password\""), "no password-named field on login page");
    // The password route is gone: only the SIWE login endpoint may be called.
    assert!(!html.contains("'/api/login'") && !html.contains("\"/api/login\""), "password /api/login must not be called");
    assert_has(
        &html,
        &["Sign in with Ethereum", "/api/siwe/nonce", "/api/login/siwe", "/api/demo-wallet/", "/api/demo-wallet/sign"],
        "login",
    );
}

#[test]
fn login_page_offers_labelled_demo_wallet_per_person() {
    let html = login_page().into_string();
    let people = demo_vaults();
    assert!(!people.is_empty());
    for v in &people {
        let p = v.person();
        let attr = format!("data-subject=\"{}\"", p.subject);
        let i = html.find(&attr).unwrap_or_else(|| panic!("no demo button for {}", p.subject));
        let button = section(&html[i..], &attr, "</button>");
        assert_has(button, &[p.display_name.as_str(), "DEMO"], &format!("demo button {}", p.subject));
    }
    assert_eq!(html.matches("data-subject=").count(), people.len(), "one demo button per person");
}

#[test]
fn keys_card_markup() {
    let html = keys_card("maya").into_string();
    assert_has(&html, &["id=\"keys-card\"", "id=\"key-secret-form\"", "id=\"key-fake-form\"", "id=\"keys-issued\"", "id=\"keys-fake\""], "keys card");
    let i = html.find("id=\"ks-value\"").expect("secret input");
    let tag_start = html[..i].rfind('<').unwrap();
    let tag = &html[tag_start..tag_start + html[tag_start..].find('>').unwrap()];
    assert!(tag.contains("type=\"password\""), "secret input must be type=password: {tag}");
    assert!(!tag.contains("value="), "secret input must not be prefilled: {tag}");
    // Only one password input (the secret) and no other place a secret could be prefilled.
    assert_eq!(html.matches("type=\"password\"").count(), 1);
}

#[test]
fn keys_js_calls_keys_api() {
    assert_has(KEYS_JS, &["'/api/keys'", "'/api/keys/secrets'", "'/api/keys/fake'", "'/api/keys/revoke'"], "KEYS_JS");
    assert!(KEYS_JS.contains("JSON.stringify({prefix: prefix})"), "revoke sends {{prefix}}");
    assert!(KEYS_JS.contains("revoke(null)"), "kill switch revokes all (prefix null)");
    assert!(KEYS_JS.contains("window.keysRevokeAll"));
}

#[test]
fn keys_js_secret_is_write_only() {
    let submit = section(KEYS_JS, "g('key-secret-form')", "g('key-fake-form')");
    assert!(submit.contains("/api/keys/secrets"));
    // Cleared on both the success and the failure path.
    assert!(submit.matches("g('ks-value').value = ''").count() >= 2, "secret input cleared after submit (ok + error paths)");
    // The submitted value is never written into the DOM.
    for bad in ["innerHTML", "textContent", "+ value", "value +"] {
        assert!(!submit.contains(bad), "secret submit handler must not render anything: found {bad:?}");
    }
    // Rendering of stored secrets shows only provider/label/last4, never a value.
    let draw = section(KEYS_JS, "function draw(", "function drawFake(");
    assert_has(draw, &["kesc(s.provider)", "kesc(s.label)", "kesc(s.last4)"], "draw secrets");
    assert!(!draw.contains("s.value") && !draw.contains("s.secret"), "secret value must never be rendered");
}

#[test]
fn keys_js_fake_key_shown_once_safely() {
    let issue = section(KEYS_JS, "g('key-fake-form')", "window.keysRefresh");
    assert_has(issue, &["worthless outside Aegis", "shown once", "fk-copy"], "issue fake key");
    // The fake key goes in via textContent, never concatenated into HTML.
    assert!(issue.contains("textContent = k.fake_key"));
    assert!(!issue.contains("+ k.fake_key") && !issue.contains("k.fake_key +"), "fake key must not be HTML-concatenated");
    // Listing never shows the fake key, only its prefix.
    let list = section(KEYS_JS, "function drawFake(", "async function refresh(");
    assert!(!list.contains("fake_key"), "fake key list must not include the key");
    // Kill switch also clears the shown-once key.
    assert!(section(KEYS_JS, "window.keysRevokeAll", "\n").contains("g('keys-issued').innerHTML = ''"));
}

#[test]
fn keys_js_escapes_server_strings() {
    let def = section(KEYS_JS, "const kesc", "\n");
    assert_has(def, &["'&amp;'", "'&lt;'", "'&gt;'", "'&quot;'", "'&#39;'"], "kesc");
    let list = section(KEYS_JS, "function drawFake(", "async function refresh(");
    assert_has(list, &["kesc(k.prefix)", "kesc(k.agent_id)", "kesc(k.provider)"], "fake key list");
    for raw in ["+ k.prefix", "+ k.agent_id", "+ k.provider"] {
        assert!(!list.contains(raw), "unescaped {raw:?} in fake key list");
    }
    let draw = section(KEYS_JS, "function draw(", "function drawFake(");
    for raw in ["+ s.label", "+ s.provider", "+ s.last4"] {
        assert!(!draw.contains(raw), "unescaped {raw:?} in secrets list");
    }
    let issue = section(KEYS_JS, "g('key-fake-form')", "window.keysRefresh");
    assert!(issue.contains("kesc(k.info && k.info.agent_id)"), "issued-key header escapes agent_id");
}
