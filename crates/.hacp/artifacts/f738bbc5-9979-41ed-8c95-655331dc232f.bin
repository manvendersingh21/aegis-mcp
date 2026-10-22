use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn page() -> String {
    let resp = aegis_dashboard::router().oneshot(Request::get("/u/maya").body(Body::empty()).unwrap()).await.unwrap();
    String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap()
}

#[tokio::test]
async fn handles_phase2_events() {
    let html = page().await;
    for needle in ["case 'risk_scored'", "case 'payment_executed'", "Jev risk", "promptInjection", "dataExfiltration", "Stripe TEST", "[TOKENIZED BY AEGIS]", "Approve payment"] {
        assert!(html.contains(needle), "missing {needle}");
    }
}

#[tokio::test]
async fn pending_rebuilt_and_raw_gated() {
    let html = page().await;
    assert!(html.contains("delete pending[k]"), "pending rebuilt from snapshot");
    assert!(html.contains("deadline) <= t"), "expired consents dropped");
    assert!(html.contains("raw release disabled for regulated identifiers"));
    for f in ["ssn", "account_number", "routing_number", "passport_number"] {
        assert!(html.contains(&format!("'{f}'")), "regulated {f}");
    }
}
