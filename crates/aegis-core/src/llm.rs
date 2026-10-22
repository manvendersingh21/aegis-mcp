//! Minimal OpenAI-compatible client for the local 9B model served by vLLM on
//! cis-a6000 behind a Cloudflare tunnel. Configured by env:
//! `AEGIS_LLM_URL` (e.g. https://xyz.trycloudflare.com/v1), `AEGIS_LLM_KEY`,
//! `AEGIS_LLM_MODEL` (default "aegis-9b"). Every call has a hard timeout;
//! callers must treat errors as "LLM unavailable" and fall back.

use crate::AegisError;
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    base_url: String,
    key: String,
    model: String,
}

impl LlmClient {
    pub fn new(base_url: impl Into<String>, key: impl Into<String>, model: impl Into<String>, timeout: Duration) -> Self {
        let http = reqwest::Client::builder().timeout(timeout).build().expect("reqwest client");
        LlmClient { http, base_url: base_url.into().trim_end_matches('/').to_string(), key: key.into(), model: model.into() }
    }

    /// `None` when `AEGIS_LLM_URL` is unset.
    pub fn from_env() -> Option<Self> {
        let url = std::env::var("AEGIS_LLM_URL").ok().filter(|s| !s.is_empty())?;
        let key = std::env::var("AEGIS_LLM_KEY").unwrap_or_default();
        let model = std::env::var("AEGIS_LLM_MODEL").unwrap_or_else(|_| "aegis-9b".into());
        Some(Self::new(url, key, model, Duration::from_secs(12)))
    }

    /// Chat completion. With `json_schema`, the server enforces structured
    /// output and the returned string is JSON matching it.
    pub async fn chat(&self, system: &str, user: &str, json_schema: Option<Value>) -> Result<String, AegisError> {
        let mut body = json!({
            "model": self.model,
            "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
            "temperature": 0.2,
            "max_tokens": 512,
            "chat_template_kwargs": {"enable_thinking": false}
        });
        if let Some(schema) = json_schema {
            body["response_format"] = json!({"type": "json_schema", "json_schema": {"name": "out", "schema": schema}});
        }
        let resp = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await
            .map_err(|e| AegisError::Llm(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(AegisError::Llm(format!("status {}", resp.status())));
        }
        let v: Value = resp.json().await.map_err(|e| AegisError::Llm(e.to_string()))?;
        v["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| AegisError::Llm("no content".into()))
    }
}
