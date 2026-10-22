use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn get(uri: &str) -> (StatusCode, String) {
    let resp = aegis_dashboard::router().oneshot(Request::get(uri).body(Body::empty()).unwrap()).await.unwrap();
    let st = resp.status();
    (st, String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap())
}

#[tokio::test]
async fn index_lists_people() {
    let (st, html) = get("/").await;
    assert_eq!(st, StatusCode::OK);
    assert!(html.contains("AegisMCP"));
    assert!(html.contains("/u/maya") && html.contains("/u/dev"), "links to each person's dashboard");
}

#[tokio::test]
async fn person_dashboard_wires_api() {
    let (st, html) = get("/u/maya").await;
    assert_eq!(st, StatusCode::OK);
    for needle in ["Maya Okafor", "/events?subject=", "/api/state?subject=", "/api/consent/", "/api/revoke?subject=", "/api/scenario/", "EventSource"] {
        assert!(html.contains(needle), "missing {needle}");
    }
    let (st, _) = get("/u/nobody").await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}
