//! `BrokerStore` backed by Supabase (PostgREST). Secrets arrive already sealed
//! (AES-256-GCM); fake keys are stored only as sha256 hashes.

use aegis_broker::{BrokerStore, StoredFakeKey, StoredSecret, UsageRow};
use aegis_core::{AegisError, BoxFuture, FakeKeyInfo, SecretMeta};
use serde_json::{json, Value};
use std::time::Duration;

pub struct SupabaseBrokerStore {
    url: String,
    key: String,
    http: reqwest::Client,
}

fn err(e: impl std::fmt::Display) -> AegisError {
    AegisError::Storage(e.to_string())
}

impl SupabaseBrokerStore {
    pub fn from_env() -> Option<Self> {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Some(SupabaseBrokerStore {
            url: get("SUPABASE_URL")?.trim_end_matches('/').to_string(),
            key: get("SUPABASE_SECRET_KEY")?,
            http: reqwest::Client::builder().timeout(Duration::from_secs(10)).build().ok()?,
        })
    }

    fn req(&self, method: reqwest::Method, table: &str, query: &str) -> reqwest::RequestBuilder {
        let q = if query.is_empty() { String::new() } else { format!("?{query}") };
        self.http
            .request(method, format!("{}/rest/v1/{table}{q}", self.url))
            .header("apikey", &self.key)
            .bearer_auth(&self.key)
    }

    async fn upsert(&self, table: &str, on_conflict: &str, row: Value) -> Result<(), AegisError> {
        let resp = self
            .req(reqwest::Method::POST, table, &format!("on_conflict={on_conflict}"))
            .header("Prefer", "resolution=merge-duplicates,return=minimal")
            .json(&row)
            .send()
            .await
            .map_err(err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(err(format!("{table} upsert: {} {}", resp.status(), resp.text().await.unwrap_or_default())))
        }
    }

    async fn select(&self, table: &str, query: &str) -> Result<Vec<Value>, AegisError> {
        let resp = self.req(reqwest::Method::GET, table, query).send().await.map_err(err)?;
        if !resp.status().is_success() {
            return Err(err(format!("{table} select: {} {}", resp.status(), resp.text().await.unwrap_or_default())));
        }
        resp.json().await.map_err(err)
    }
}

fn secret_row(s: &StoredSecret) -> Value {
    json!({
        "secret_id": s.meta.secret_id, "subject": s.meta.subject, "provider": s.meta.provider,
        "revoked": s.meta.revoked, "meta": s.meta, "nonce_b64": s.nonce, "ciphertext_b64": s.ciphertext,
    })
}

fn secret_from(row: Value) -> Option<StoredSecret> {
    Some(StoredSecret {
        meta: serde_json::from_value::<SecretMeta>(row.get("meta")?.clone()).ok()?,
        nonce: row.get("nonce_b64")?.as_str()?.to_string(),
        ciphertext: row.get("ciphertext_b64")?.as_str()?.to_string(),
    })
}

fn fake_row(k: &StoredFakeKey) -> Value {
    json!({
        "fake_key_sha256": k.sha256_hex, "subject": k.info.subject, "agent_id": k.info.agent_id,
        "provider": k.info.provider, "revoked": k.info.revoked, "expires_at": k.info.expires_at, "info": k.info,
    })
}

fn fake_from(row: Value) -> Option<StoredFakeKey> {
    Some(StoredFakeKey {
        sha256_hex: row.get("fake_key_sha256")?.as_str()?.to_string(),
        info: serde_json::from_value::<FakeKeyInfo>(row.get("info")?.clone()).ok()?,
    })
}

/// PostgREST filter values must be URL-safe; subjects/ids are [a-z0-9_] here.
fn q(v: &str) -> String {
    v.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect()
}

impl BrokerStore for SupabaseBrokerStore {
    fn put_secret(&self, secret: StoredSecret) -> BoxFuture<'_, Result<(), AegisError>> {
        Box::pin(async move { self.upsert("aegis_secrets", "secret_id", secret_row(&secret)).await })
    }

    fn secrets<'a>(&'a self, subject: Option<&'a str>) -> BoxFuture<'a, Result<Vec<StoredSecret>, AegisError>> {
        Box::pin(async move {
            let filter = match subject {
                Some(s) => format!("subject=eq.{}", q(s)),
                None => "subject=is.null".to_string(),
            };
            Ok(self.select("aegis_secrets", &filter).await?.into_iter().filter_map(secret_from).collect())
        })
    }

    fn secret<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<StoredSecret>, AegisError>> {
        Box::pin(async move {
            Ok(self.select("aegis_secrets", &format!("secret_id=eq.{}", q(id))).await?.into_iter().find_map(secret_from))
        })
    }

    fn put_fake_key(&self, key: StoredFakeKey) -> BoxFuture<'_, Result<(), AegisError>> {
        Box::pin(async move { self.upsert("aegis_fake_keys", "fake_key_sha256", fake_row(&key)).await })
    }

    fn fake_key_by_hash<'a>(&'a self, sha256_hex: &'a str) -> BoxFuture<'a, Result<Option<StoredFakeKey>, AegisError>> {
        Box::pin(async move {
            Ok(self
                .select("aegis_fake_keys", &format!("fake_key_sha256=eq.{}", q(sha256_hex)))
                .await?
                .into_iter()
                .find_map(fake_from))
        })
    }

    fn fake_keys<'a>(&'a self, subject: &'a str) -> BoxFuture<'a, Result<Vec<StoredFakeKey>, AegisError>> {
        Box::pin(async move {
            Ok(self
                .select("aegis_fake_keys", &format!("subject=eq.{}&order=expires_at.desc", q(subject)))
                .await?
                .into_iter()
                .filter_map(fake_from)
                .collect())
        })
    }

    fn update_fake_key(&self, key: StoredFakeKey) -> BoxFuture<'_, Result<(), AegisError>> {
        Box::pin(async move { self.upsert("aegis_fake_keys", "fake_key_sha256", fake_row(&key)).await })
    }

    fn log_usage(&self, row: UsageRow) -> BoxFuture<'_, Result<(), AegisError>> {
        Box::pin(async move {
            let row = json!({
                "fake_key_sha256": row.fake_key_hash, "subject": row.subject, "agent_id": row.agent_id,
                "provider": row.provider, "path": row.path, "ok": row.ok, "at": row.ts,
            });
            let resp = self
                .req(reqwest::Method::POST, "aegis_key_usage", "")
                .header("Prefer", "return=minimal")
                .json(&row)
                .send()
                .await
                .map_err(err)?;
            if resp.status().is_success() { Ok(()) } else { Err(err(format!("usage insert: {}", resp.status()))) }
        })
    }
}
