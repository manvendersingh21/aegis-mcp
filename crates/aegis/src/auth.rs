//! Session lookup for routes served outside the gateway crate: forwards the
//! caller's cookie to the gateway's `GET /api/me`.

use axum::http::HeaderMap;
use serde_json::Value;
use std::time::Duration;

#[derive(Clone)]
pub struct SessionAuth {
    base_url: String,
    http: reqwest::Client,
}

impl SessionAuth {
    pub fn new(base_url: String) -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().unwrap_or_default();
        SessionAuth { base_url, http }
    }

    /// Subject of the logged-in person, if the cookie carries a valid session.
    pub async fn subject(&self, headers: &HeaderMap) -> Option<String> {
        let cookie = headers.get("cookie")?.to_str().ok()?;
        let me: Value = self
            .http
            .get(format!("{}/api/me", self.base_url))
            .header("cookie", cookie)
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        me["subject"].as_str().map(str::to_string)
    }
}
