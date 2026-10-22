//! PHASE 6 dashboard checks: "My rules" standing rules card, "Connect a real
//! agent" MCP credential card, advocate advice and "Remember this decision"
//! on consent cards, and escaping in P6_JS.

use super::*;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn get_page(path: &str) -> (StatusCode, String) {
    let resp = router().oneshot(Request::get(path).body(Body::empty()).unwrap()).await.unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

fn assert_has(hay: &str, needles: &[&str], ctx: &str) {
    for n in needles {
        assert!(hay.contains(n), "{ctx}: missing {n:?}");
    }
}

#[tokio::test]
async fn person_page_renders_p6_cards() {
    let (status, html) = get_page("/u/maya").await;
    assert_eq!(status, StatusCode::OK);
    assert_has(
        &html,
        &[
            "id=\"p6-rules-card\"",
            "Standing rules",
            "My rules",
            "id=\"rules-list\"",
            "Quick add",
            "Calendar: free/busy only, any agent",
            "Never share health data",
            "Payments under $500: clean room",
            "id=\"p6-agent-card\"",
            "For real agents",
            "Connect a real agent",
            "id=\"mcp-form\"",
            "id=\"mcp-agent\"",
            "Issue MCP credential",
            "id=\"p6-passkey-card\"",
            "id=\"passkey-reg\"",
            "Add a passkey",
        ],
        "p6 cards on person page",
    );
}

#[test]
fn p6_cards_markup_standalone() {
    let markup = p6::cards("maya").into_string();
    assert_has(
        &markup,
        &[
            "id=\"p6-rules-card\"",
            "id=\"rules-list\"",
            "data-rule=\"calendar\"",
            "data-rule=\"health\"",
            "data-rule=\"payments\"",
            "id=\"p6-agent-card\"",
            "id=\"mcp-form\"",
            "id=\"mcp-agent\"",
            "id=\"p6-passkey-card\"",
            "id=\"passkey-reg\"",
        ],
        "p6::cards",
    );
}

#[tokio::test]
async fn login_page_renders_passkey_button() {
    let (status, html) = get_page("/login").await;
    assert_eq!(status, StatusCode::OK);
    assert_has(
        &html,
        &[
            "id=\"p6-passkey-login\"",
            "id=\"passkey-login\"",
            "Sign in with a passkey",
            "/api/passkey/login/begin",
            "/api/passkey/login/finish",
        ],
        "login page passkey button and script",
    );
}

#[test]
fn p6_js_exposes_handlers_and_apis() {
    let js = p6::P6_JS;
    assert_has(
        js,
        &[
            "window.renderAdvice",
            "window.rememberChoice",
            "window.ruleApplied",
            "fetch('/api/rules?subject='",
            "fetch('/api/rules/'",
            "fetch('/api/keys/fake'",
            "provider:\"mcp\"",
            "claude mcp add --transport http aegis",
            "/api/passkey/register/begin",
            "/api/passkey/register/finish",
        ],
        "P6_JS handlers and endpoints",
    );
}

#[test]
fn p6_js_escapes_dynamic_strings() {
    let js = p6::P6_JS;
    assert_has(
        js,
        &[
            "const p6esc",
            "'&amp;'",
            "'&lt;'",
            "'&gt;'",
            "'&quot;'",
            "'&#39;'",
            "p6esc(r.label)",
            "p6esc(rid)",
            "p6esc(tool)",
            "p6esc(agent)",
        ],
        "P6_JS escaping",
    );
}

#[test]
fn consent_cards_include_advice_and_remember() {
    // Check that JS template for consent cards includes advice and remember placeholders
    assert_has(
        JS,
        &[
            "class=\"advice\"",
            "Remember this decision",
            "rememberChoice",
            "renderAdvice",
            "advices[",
        ],
        "consent card advice and remember in JS",
    );
}
