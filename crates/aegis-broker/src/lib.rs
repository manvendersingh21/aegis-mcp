//! Fake-key broker: encrypted provider secrets, short-lived agent credentials,
//! and a small HTTP proxy that swaps fake keys for real upstream keys.
use aegis_core::{now_unix, AegisError, BoxFuture, FakeKeyInfo, IssuedFakeKey, SecretMeta};
use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
use axum::{
    body::{to_bytes, Body},
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct Sealer(Aes256Gcm);
impl Sealer {
    pub fn new(key: [u8; 32]) -> Self {
        Self(Aes256Gcm::new_from_slice(&key).expect("32-byte AES key"))
    }
    pub fn seal(&self, plaintext: &[u8]) -> (String, String) {
        let mut nonce = [0u8; 12];
        OsRng.fill_bytes(&mut nonce);
        let ciphertext = self
            .0
            .encrypt(Nonce::from_slice(&nonce), plaintext)
            .expect("AES-GCM encryption cannot fail for valid input");
        (B64.encode(nonce), B64.encode(ciphertext))
    }
    pub fn open(&self, nonce_b64: &str, ct_b64: &str) -> Result<Vec<u8>, AegisError> {
        let nonce = B64
            .decode(nonce_b64)
            .map_err(|e| AegisError::Other(format!("invalid nonce: {e}")))?;
        let ciphertext = B64
            .decode(ct_b64)
            .map_err(|e| AegisError::Other(format!("invalid ciphertext: {e}")))?;
        if nonce.len() != 12 {
            return Err(AegisError::Other("invalid AES-GCM nonce length".into()));
        }
        self.0
            .decrypt(Nonce::from_slice(&nonce), ciphertext.as_ref())
            .map_err(|_| AegisError::Other("secret authentication failed".into()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSecret {
    pub meta: SecretMeta,
    pub nonce: String,
    pub ciphertext: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredFakeKey {
    pub sha256_hex: String,
    pub info: FakeKeyInfo,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRow {
    pub fake_key_hash: String,
    pub subject: String,
    pub agent_id: String,
    pub provider: String,
    pub path: String,
    pub ok: bool,
    pub ts: i64,
}

pub trait BrokerStore: Send + Sync {
    fn put_secret(&self, secret: StoredSecret) -> BoxFuture<'_, Result<(), AegisError>>;
    fn secrets<'a>(
        &'a self,
        subject: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<StoredSecret>, AegisError>>;
    fn secret<'a>(&'a self, id: &'a str)
        -> BoxFuture<'a, Result<Option<StoredSecret>, AegisError>>;
    fn put_fake_key(&self, key: StoredFakeKey) -> BoxFuture<'_, Result<(), AegisError>>;
    fn fake_key_by_hash<'a>(
        &'a self,
        sha256_hex: &'a str,
    ) -> BoxFuture<'a, Result<Option<StoredFakeKey>, AegisError>>;
    fn fake_keys<'a>(
        &'a self,
        subject: &'a str,
    ) -> BoxFuture<'a, Result<Vec<StoredFakeKey>, AegisError>>;
    fn update_fake_key(&self, key: StoredFakeKey) -> BoxFuture<'_, Result<(), AegisError>>;
    fn log_usage(&self, row: UsageRow) -> BoxFuture<'_, Result<(), AegisError>>;
}

#[derive(Default)]
pub struct MemoryStore {
    secrets: RwLock<HashMap<String, StoredSecret>>,
    fake_keys: RwLock<HashMap<String, StoredFakeKey>>,
    usage: RwLock<Vec<UsageRow>>,
}
impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub async fn usage_rows(&self) -> Vec<UsageRow> {
        self.usage.read().await.clone()
    }
}
impl BrokerStore for MemoryStore {
    fn put_secret(&self, secret: StoredSecret) -> BoxFuture<'_, Result<(), AegisError>> {
        Box::pin(async move {
            self.secrets
                .write()
                .await
                .insert(secret.meta.secret_id.clone(), secret);
            Ok(())
        })
    }
    fn secrets<'a>(
        &'a self,
        subject: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<StoredSecret>, AegisError>> {
        Box::pin(async move {
            Ok(self
                .secrets
                .read()
                .await
                .values()
                .filter(|s| s.meta.subject.as_deref() == subject)
                .cloned()
                .collect())
        })
    }
    fn secret<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<StoredSecret>, AegisError>> {
        Box::pin(async move { Ok(self.secrets.read().await.get(id).cloned()) })
    }
    fn put_fake_key(&self, key: StoredFakeKey) -> BoxFuture<'_, Result<(), AegisError>> {
        Box::pin(async move {
            self.fake_keys
                .write()
                .await
                .insert(key.sha256_hex.clone(), key);
            Ok(())
        })
    }
    fn fake_key_by_hash<'a>(
        &'a self,
        hash: &'a str,
    ) -> BoxFuture<'a, Result<Option<StoredFakeKey>, AegisError>> {
        Box::pin(async move { Ok(self.fake_keys.read().await.get(hash).cloned()) })
    }
    fn fake_keys<'a>(
        &'a self,
        subject: &'a str,
    ) -> BoxFuture<'a, Result<Vec<StoredFakeKey>, AegisError>> {
        Box::pin(async move {
            Ok(self
                .fake_keys
                .read()
                .await
                .values()
                .filter(|k| k.info.subject == subject)
                .cloned()
                .collect())
        })
    }
    fn update_fake_key(&self, key: StoredFakeKey) -> BoxFuture<'_, Result<(), AegisError>> {
        Box::pin(async move {
            self.fake_keys
                .write()
                .await
                .insert(key.sha256_hex.clone(), key);
            Ok(())
        })
    }
    fn log_usage(&self, row: UsageRow) -> BoxFuture<'_, Result<(), AegisError>> {
        Box::pin(async move {
            self.usage.write().await.push(row);
            Ok(())
        })
    }
}

#[derive(Clone)]
pub struct BrokerState {
    pub store: Arc<dyn BrokerStore>,
    pub sealer: Sealer,
    pub broker_token: String,
    pub stripe_base_url: String,
    pub jev_base_url: String,
    pub llm_base_url: String,
    pub http: reqwest::Client,
}
impl BrokerState {
    pub fn new(
        store: Arc<dyn BrokerStore>,
        sealer: Sealer,
        broker_token: impl Into<String>,
    ) -> Self {
        Self {
            store,
            sealer,
            broker_token: broker_token.into(),
            stripe_base_url: "https://api.stripe.com".into(),
            jev_base_url: "https://api.typesafe.ai".into(),
            llm_base_url: std::env::var("AEGIS_LLM_UPSTREAM")
                .unwrap_or_else(|_| "https://api.openai.com".into()),
            http: reqwest::Client::new(),
        }
    }
    pub fn with_upstreams(
        mut self,
        stripe: impl Into<String>,
        jev: impl Into<String>,
        llm: impl Into<String>,
    ) -> Self {
        self.stripe_base_url = stripe.into();
        self.jev_base_url = jev.into();
        self.llm_base_url = llm.into();
        self
    }
    pub async fn issue_fake_key(
        &self,
        subject: &str,
        agent_id: &str,
        provider: &str,
        ttl_secs: u64,
    ) -> Result<IssuedFakeKey, AegisError> {
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        let mut random = [0u8; 24];
        OsRng.fill_bytes(&mut random);
        let suffix: String = random
            .iter()
            .map(|x| alphabet[(*x as usize) % alphabet.len()] as char)
            .collect();
        let fake_key = format!("aegis_fk_{provider}_{suffix}");
        let info = FakeKeyInfo {
            prefix: fake_key.chars().take(20).collect(),
            subject: subject.into(),
            agent_id: agent_id.into(),
            provider: provider.into(),
            issued_at: now_unix(),
            expires_at: now_unix().saturating_add(ttl_secs.min(i64::MAX as u64) as i64),
            revoked: false,
            last_used_at: None,
            uses: 0,
        };
        self.store
            .put_fake_key(StoredFakeKey {
                sha256_hex: hash(&fake_key),
                info: info.clone(),
            })
            .await?;
        Ok(IssuedFakeKey { fake_key, info })
    }
}

pub fn router(state: BrokerState) -> Router {
    Router::new()
        .route("/admin/secrets", post(add_secret).get(list_secrets))
        .route("/admin/fake-keys", post(issue_key).get(list_keys))
        .route("/admin/fake-keys/revoke", post(revoke_keys))
        .route("/admin/vault/{subject}", get(get_vault))
        .route("/proxy/stripe/{*path}", any(proxy_stripe))
        .route("/proxy/jev/{*path}", any(proxy_jev))
        .route("/proxy/llm/{*path}", any(proxy_llm))
        .with_state(state)
}

#[derive(Deserialize)]
struct AddSecret {
    subject: Option<String>,
    provider: String,
    label: String,
    value: String,
}
async fn add_secret(
    State(s): State<BrokerState>,
    headers: HeaderMap,
    Json(body): Json<AddSecret>,
) -> Response {
    if !admin(&s, &headers) {
        return unauthorized();
    }
    let (nonce, ciphertext) = s.sealer.seal(body.value.as_bytes());
    let meta = SecretMeta {
        secret_id: aegis_core::new_id("sec"),
        subject: body.subject,
        provider: body.provider,
        label: body.label,
        last4: body
            .value
            .chars()
            .rev()
            .take(4)
            .collect::<String>()
            .chars()
            .rev()
            .collect(),
        created_at: now_unix(),
        revoked: false,
    };
    match s
        .store
        .put_secret(StoredSecret {
            meta: meta.clone(),
            nonce,
            ciphertext,
        })
        .await
    {
        Ok(()) => Json(meta).into_response(),
        Err(_) => internal_error(),
    }
}
#[derive(Deserialize)]
struct SubjectQuery {
    subject: Option<String>,
}
async fn list_secrets(
    State(s): State<BrokerState>,
    headers: HeaderMap,
    Query(q): Query<SubjectQuery>,
) -> Response {
    if !admin(&s, &headers) {
        return unauthorized();
    }
    match s.store.secrets(q.subject.as_deref()).await {
        Ok(rows) => Json(rows.into_iter().map(|r| r.meta).collect::<Vec<_>>()).into_response(),
        Err(_) => internal_error(),
    }
}
#[derive(Deserialize)]
struct IssueKey {
    subject: String,
    agent_id: String,
    provider: String,
    ttl_secs: u64,
}
async fn issue_key(
    State(s): State<BrokerState>,
    headers: HeaderMap,
    Json(b): Json<IssueKey>,
) -> Response {
    if !admin(&s, &headers) {
        return unauthorized();
    }
    match s
        .issue_fake_key(&b.subject, &b.agent_id, &b.provider, b.ttl_secs)
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(_) => internal_error(),
    }
}
async fn list_keys(
    State(s): State<BrokerState>,
    headers: HeaderMap,
    Query(q): Query<SubjectQuery>,
) -> Response {
    if !admin(&s, &headers) {
        return unauthorized();
    }
    let Some(subject) = q.subject else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"subject required"})),
        )
            .into_response();
    };
    match s.store.fake_keys(&subject).await {
        Ok(rows) => Json(rows.into_iter().map(|r| r.info).collect::<Vec<_>>()).into_response(),
        Err(_) => internal_error(),
    }
}
#[derive(Deserialize)]
struct RevokeKeys {
    subject: String,
    prefix: Option<String>,
}
async fn revoke_keys(
    State(s): State<BrokerState>,
    headers: HeaderMap,
    Json(b): Json<RevokeKeys>,
) -> Response {
    if !admin(&s, &headers) {
        return unauthorized();
    }
    let rows = match s.store.fake_keys(&b.subject).await {
        Ok(v) => v,
        Err(_) => return internal_error(),
    };
    let mut revoked = Vec::new();
    for mut row in rows {
        if b.prefix.as_ref().is_none_or(|p| row.info.prefix == *p) && !row.info.revoked {
            row.info.revoked = true;
            revoked.push(row.info.prefix.clone());
            if s.store.update_fake_key(row).await.is_err() {
                return internal_error();
            }
        }
    }
    Json(json!({"revoked":revoked})).into_response()
}

async fn get_vault(
    State(s): State<BrokerState>,
    headers: HeaderMap,
    Path(subject): Path<String>,
) -> Response {
    if !admin(&s, &headers) {
        return unauthorized();
    }
    let existing = match s.store.secrets(Some(&subject)).await {
        Ok(rows) => rows.into_iter().find(|r| r.meta.provider == "__vault"),
        Err(_) => return internal_error(),
    };
    if let Some(row) = existing {
        return match s.sealer.open(&row.nonce, &row.ciphertext).and_then(|b| {
            serde_json::from_slice::<Value>(&b).map_err(|e| AegisError::Other(e.to_string()))
        }) {
            Ok(v) => Json(v).into_response(),
            Err(_) => internal_error(),
        };
    }
    let Some(vault) = aegis_core::demo_vaults()
        .into_iter()
        .find(|v| v.subject() == subject)
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"unknown subject"})),
        )
            .into_response();
    };
    let raw = vault.0;
    let (nonce, ciphertext) = s.sealer.seal(raw.to_string().as_bytes());
    let meta = SecretMeta {
        secret_id: aegis_core::new_id("vault"),
        subject: Some(subject),
        provider: "__vault".into(),
        label: "encrypted vault".into(),
        last4: String::new(),
        created_at: now_unix(),
        revoked: false,
    };
    if s.store
        .put_secret(StoredSecret {
            meta,
            nonce,
            ciphertext,
        })
        .await
        .is_err()
    {
        return internal_error();
    }
    Json(raw).into_response()
}

#[derive(Clone, Copy)]
enum Provider {
    Stripe,
    Jev,
    Llm,
}
impl Provider {
    fn name(self) -> &'static str {
        match self {
            Self::Stripe => "stripe",
            Self::Jev => "jev",
            Self::Llm => "llm",
        }
    }
}
async fn proxy_stripe(
    State(s): State<BrokerState>,
    Path(path): Path<String>,
    req: Request<Body>,
) -> Response {
    proxy(s, Provider::Stripe, path, req).await
}
async fn proxy_jev(
    State(s): State<BrokerState>,
    Path(path): Path<String>,
    req: Request<Body>,
) -> Response {
    proxy(s, Provider::Jev, path, req).await
}
async fn proxy_llm(
    State(s): State<BrokerState>,
    Path(path): Path<String>,
    req: Request<Body>,
) -> Response {
    proxy(s, Provider::Llm, path, req).await
}
async fn proxy(s: BrokerState, provider: Provider, path: String, req: Request<Body>) -> Response {
    let (parts, body) = req.into_parts();
    let fake = match provider {
        Provider::Stripe => basic_username(parts.headers.get(header::AUTHORIZATION)),
        _ => bearer(parts.headers.get(header::AUTHORIZATION)),
    };
    let Some(fake) = fake else {
        return invalid_key();
    };
    let key_hash = hash(&fake);
    let mut row = match s.store.fake_key_by_hash(&key_hash).await {
        Ok(Some(r)) => r,
        _ => return invalid_key(),
    };
    if row.info.provider != provider.name() || row.info.revoked || row.info.expires_at <= now_unix()
    {
        let _ = s
            .store
            .log_usage(usage(&row, provider, &path, false, &key_hash))
            .await;
        return invalid_key();
    }
    let secret = find_real_secret(&s, &row.info.subject, provider.name()).await;
    let Some(real) = secret else {
        let _ = s
            .store
            .log_usage(usage(&row, provider, &path, false, &key_hash))
            .await;
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error":"provider secret unavailable"})),
        )
            .into_response();
    };
    if matches!(provider, Provider::Stripe)
        && !(real.starts_with("sk_test_") || real.starts_with("rk_test_"))
    {
        let _ = s
            .store
            .log_usage(usage(&row, provider, &path, false, &key_hash))
            .await;
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"Stripe broker accepts test keys only"})),
        )
            .into_response();
    }
    let base = match provider {
        Provider::Stripe => &s.stripe_base_url,
        Provider::Jev => &s.jev_base_url,
        Provider::Llm => &s.llm_base_url,
    };
    let mut url = format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    if let Some(q) = parts.uri.query() {
        url.push('?');
        url.push_str(q);
    }
    let bytes = match to_bytes(body, 32 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid request body").into_response(),
    };
    let mut upstream = s.http.request(parts.method.clone(), url).body(bytes);
    for (name, value) in &parts.headers {
        if name == header::AUTHORIZATION
            || name == header::HOST
            || name == header::CONTENT_LENGTH
            || name == header::CONNECTION
        {
            continue;
        }
        if name == header::CONTENT_TYPE || name.as_str().eq_ignore_ascii_case("idempotency-key") {
            upstream = upstream.header(name, value);
        }
    }
    upstream = match provider {
        Provider::Stripe => upstream.basic_auth(&real, Some("")),
        _ => upstream.bearer_auth(&real),
    };
    let result = upstream.send().await;
    row.info.last_used_at = Some(now_unix());
    row.info.uses = row.info.uses.saturating_add(1);
    let mut response = match result {
        Ok(r) => {
            let status =
                StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let headers = r.headers().clone();
            let bytes = match r.bytes().await {
                Ok(b) => b,
                Err(_) => {
                    let _ = s.store.update_fake_key(row.clone()).await;
                    let _ = s
                        .store
                        .log_usage(usage(&row, provider, &path, false, &key_hash))
                        .await;
                    return (StatusCode::BAD_GATEWAY, "upstream response unavailable")
                        .into_response();
                }
            };
            let mut out = Response::builder().status(status);
            for (name, value) in &headers {
                // The body is re-sent fully buffered: drop framing/hop-by-hop
                // headers (a forwarded `transfer-encoding: chunked` makes hyper
                // abort the connection).
                let hop_by_hop = name == header::CONTENT_LENGTH
                    || name == header::CONNECTION
                    || name == header::TRANSFER_ENCODING
                    || name == header::CONTENT_ENCODING
                    || name.as_str().eq_ignore_ascii_case("keep-alive");
                if !hop_by_hop {
                    out = out.header(name, scrub_bytes(value.as_bytes(), &real));
                }
            }
            let scrubbed = scrub_bytes(&bytes, &real);
            out.body(Body::from(scrubbed))
                .unwrap_or_else(|_| internal_error())
        }
        Err(_) => (StatusCode::BAD_GATEWAY, "upstream unavailable").into_response(),
    };
    let ok = response.status().is_success();
    // Scrub key occurrences in any response header passed through from upstream.
    for value in response.headers_mut().values_mut() {
        if let Ok(clean) = HeaderValue::from_bytes(&scrub_bytes(value.as_bytes(), &real)) {
            *value = clean;
        }
    }
    let _ = s.store.update_fake_key(row.clone()).await;
    let _ = s
        .store
        .log_usage(usage(&row, provider, &path, ok, &key_hash))
        .await;
    response
}

async fn find_real_secret(s: &BrokerState, subject: &str, provider: &str) -> Option<String> {
    for wanted in [Some(subject), None] {
        if let Ok(secrets) = s.store.secrets(wanted).await {
            if let Some(secret) = secrets
                .into_iter()
                .find(|x| x.meta.provider == provider && !x.meta.revoked)
            {
                if let Ok(raw) = s.sealer.open(&secret.nonce, &secret.ciphertext) {
                    if let Ok(value) = String::from_utf8(raw) {
                        return Some(value);
                    }
                }
            }
        }
    }
    None
}
fn usage(
    row: &StoredFakeKey,
    provider: Provider,
    path: &str,
    ok: bool,
    key_hash: &str,
) -> UsageRow {
    UsageRow {
        fake_key_hash: key_hash.into(),
        subject: row.info.subject.clone(),
        agent_id: row.info.agent_id.clone(),
        provider: provider.name().into(),
        path: path.into(),
        ok,
        ts: now_unix(),
    }
}
fn hash(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}
fn admin(s: &BrokerState, h: &HeaderMap) -> bool {
    h.get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == format!("Bearer {}", s.broker_token))
}
fn bearer(value: Option<&HeaderValue>) -> Option<String> {
    let value = value?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(str::to_owned)
}
fn basic_username(value: Option<&HeaderValue>) -> Option<String> {
    let raw = value?.to_str().ok()?.strip_prefix("Basic ")?;
    let decoded = B64.decode(raw).ok()?;
    let pair = String::from_utf8(decoded).ok()?;
    Some(pair.split_once(':')?.0.to_string())
}
fn scrub_bytes(input: &[u8], secret: &str) -> Vec<u8> {
    if secret.is_empty() {
        return input.to_vec();
    }
    String::from_utf8_lossy(input)
        .replace(secret, "[AEGIS-SCRUBBED]")
        .into_bytes()
}
fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error":"unauthorized"})),
    )
        .into_response()
}
fn invalid_key() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error":"invalid aegis key"})),
    )
        .into_response()
}
fn internal_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error":"broker storage error"})),
    )
        .into_response()
}
