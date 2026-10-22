//! aegis-passkey: real WebAuthn (FIDO2) passkeys for AegisMCP.
//!
//! Scope (PHASE6 §4): ES256 (-7) credentials on P-256, attestation "none",
//! discoverable credentials preferred. The service owns the cryptographic
//! ceremony; wiring the HTTP routes to an existing session is the
//! orchestrator's job.
//!
//! Supabase persistence (best-effort; the service also works memory-only):
//!
//! ```text
//! create table if not exists aegis_passkeys (
//!   credential_id      text primary key,   -- base64url (no padding) of the raw credential id
//!   subject            text not null,      -- which person the passkey belongs to
//!   public_key_cose_b64 text not null,     -- base64 (standard, padded) of the raw COSE CBOR key
//!   sign_count         bigint not null,    -- last observed authenticator sign count
//!   created_at         bigint not null     -- unix seconds
//! );
//! ```
//!
//! Wire encoding rules: every WebAuthn binary field on the JSON wire
//! (`challenge`, `id`, `rawId`, `attestationObject`, `clientDataJSON`,
//! `authenticatorData`, `signature`, `userHandle`) is base64url WITHOUT
//! padding; storage fields use standard base64 WITH padding.

use aegis_core::{now_unix, AegisError};
use aegis_store::SupabaseStore;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::engine::Engine as _;
use p256::ecdsa::{signature::Verifier, Signature, VerifyingKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;

const TABLE: &str = "aegis_passkeys";
/// Challenges are single-use and live for five minutes.
const CHALLENGE_TTL_SECS: i64 = 300;
const TIMEOUT_MS: i64 = 300_000;
/// Minimum challenge entropy (bytes); we issue 32.
const CHALLENGE_LEN: usize = 32;

// webauthn authenticatorData flags
const FLAG_UP: u8 = 0x04; // user present
const FLAG_AT: u8 = 0x40; // attested credential data included

fn err(msg: impl Into<String>) -> AegisError {
    AegisError::Other(msg.into())
}

// ------------------------------------------------------------- base64 ------

fn b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn b64url_dec(s: &str, what: &str) -> Result<Vec<u8>, AegisError> {
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|e| err(format!("passkey: {what} is not strict base64url: {e}")))
}

fn b64_std(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

fn b64_std_dec(s: &str) -> Result<Vec<u8>, AegisError> {
    STANDARD
        .decode(s)
        .map_err(|e| err(format!("passkey: stored COSE key is not valid base64: {e}")))
}

// --------------------------------------------------------- challenges ------

fn random_challenge() -> Vec<u8> {
    use rand::RngCore;
    let mut buf = vec![0u8; CHALLENGE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    buf
}

// -------------------------------------------------------- credentials ------

#[derive(Debug, Clone)]
struct StoredCredential {
    subject: String,
    /// Raw COSE CBOR bytes (map {1:2, 3:-7, -1:1, -2:x, -3:y}).
    cose: Vec<u8>,
    sign_count: u32,
}

/// WebAuthn passkey service: registration + discoverable-credential login.
///
/// Registration is meant to be called from an authenticated route: the caller
/// resolves `subject` from the existing session and passes it to both
/// `begin_registration` and `finish_registration`.
pub struct PasskeyService {
    rp_id: String,
    origin: String,
    store: Option<SupabaseStore>,
    /// credential_id (base64url) -> credential.
    credentials: Mutex<HashMap<String, StoredCredential>>,
    /// challenge (base64url) -> (subject, expires_at). Single use.
    reg_challenges: Mutex<HashMap<String, (String, i64)>>,
    /// challenge (base64url) -> expires_at. Single use.
    login_challenges: Mutex<HashMap<String, i64>>,
}

impl PasskeyService {
    pub fn new(rp_id: String, origin: String, store: Option<SupabaseStore>) -> Self {
        Self {
            rp_id,
            origin,
            store,
            credentials: Mutex::new(HashMap::new()),
            reg_challenges: Mutex::new(HashMap::new()),
            login_challenges: Mutex::new(HashMap::new()),
        }
    }

    // ------------------------------------------------------ registration ----

    /// `PublicKeyCredentialCreationOptions` for `navigator.credentials.create`.
    pub async fn begin_registration(&self, subject: &str) -> Value {
        let challenge = b64url(&random_challenge());
        {
            let mut map = self.reg_challenges.lock().unwrap_or_else(|p| p.into_inner());
            let now = now_unix();
            map.retain(|_, (_, exp)| *exp > now);
            map.insert(
                challenge.clone(),
                (subject.to_string(), now + CHALLENGE_TTL_SECS),
            );
        }
        json!({
            "challenge": challenge,
            "rp": {"id": self.rp_id, "name": "AegisMCP"},
            "user": {
                "id": b64url(subject.as_bytes()),
                "name": subject,
                "displayName": subject,
            },
            "pubKeyCredParams": [{"type": "public-key", "alg": -7}],
            "attestation": "none",
            "authenticatorSelection": {"residentKey": "preferred", "userVerification": "preferred"},
            "timeout": TIMEOUT_MS,
        })
    }

    /// Consumes the pending challenge and stores the credential.
    ///
    /// `credential` is the parsed `PublicKeyCredential` JSON:
    /// `{id, rawId, type: "public-key", response: {attestationObject, clientDataJSON}}`.
    /// The challenge is single-use: it is consumed by this call whether or not
    /// verification succeeds.
    pub async fn finish_registration(
        &self,
        subject: &str,
        credential: &Value,
    ) -> Result<(), AegisError> {
        if credential["type"].as_str() != Some("public-key") {
            return Err(err("passkey: credential type must be \"public-key\""));
        }
        let response = &credential["response"];
        let attestation_object = b64url_dec(
            response["attestationObject"].as_str().unwrap_or_default(),
            "response.attestationObject",
        )?;
        let client_data_bytes = b64url_dec(
            response["clientDataJSON"].as_str().unwrap_or_default(),
            "response.clientDataJSON",
        )?;

        // clientDataJSON: type / challenge / origin.
        let client_data: Value =
            serde_json::from_slice(&client_data_bytes).map_err(|e| err(format!("passkey: clientDataJSON is not JSON: {e}")))?;
        let challenge = client_data["challenge"]
            .as_str()
            .ok_or_else(|| err("passkey: clientDataJSON.challenge missing"))?
            .to_string();
        self.take_registration_challenge(&challenge, subject)?;
        Self::check_client_data(&client_data, "webauthn.create", &challenge, &self.origin)?;

        // attestationObject: CBOR map {fmt, attStmt, authData}.
        let att: ciborium::Value = ciborium::from_reader(std::io::Cursor::new(&attestation_object))
            .map_err(|e| err(format!("passkey: attestationObject is not CBOR: {e}")))?;
        let att_map = att.as_map().ok_or_else(|| err("passkey: attestationObject is not a CBOR map"))?;
        let get_text = |k: &str| {
            att_map
                .iter()
                .find(|(key, _)| key.as_text() == Some(k))
                .and_then(|(_, v)| v.as_text().map(|s| s.to_string()))
        };
        // "fmt" and "attStmt" must be present (values ignored; attestation is "none").
        if get_text("fmt").is_none() {
            return Err(err("passkey: attestationObject.fmt missing"));
        }
        if att_map.iter().all(|(key, _)| key.as_text() != Some("attStmt")) {
            return Err(err("passkey: attestationObject.attStmt missing"));
        }
        let auth_data = att_map
            .iter()
            .find(|(key, _)| key.as_text() == Some("authData"))
            .and_then(|(_, v)| v.as_bytes())
            .ok_or_else(|| err("passkey: attestationObject.authData missing or not bytes"))?
            .clone();

        let (_, attested) = Self::parse_auth_data(&auth_data, &self.rp_id, true)?;

        // The credential id from authData must match the reported id.
        let cred_id_b64 = b64url(&attested.credential_id);
        if let Some(id) = credential["id"].as_str() {
            if !id.is_empty() && id != cred_id_b64 {
                return Err(err("passkey: credential id does not match attested credential data"));
            }
        }

        // Validate the COSE key (also proves it parses for later logins).
        Self::verifying_key_from_cose(attested.cose)?;

        let stored = StoredCredential {
            subject: subject.to_string(),
            cose: attested.cose.to_vec(),
            // Registration row starts at 0 per the storage contract; the
            // first assertion must then be >= 0 (see finish_login).
            sign_count: 0,
        };
        self.credentials
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(cred_id_b64.clone(), stored.clone());

        // Best-effort write-through; memory stays the source of truth.
        if let Some(store) = &self.store {
            let row = json!({
                "credential_id": cred_id_b64,
                "subject": subject,
                "public_key_cose_b64": b64_std(attested.cose),
                "sign_count": 0,
                "created_at": now_unix(),
            });
            if let Err(e) = store.upsert(TABLE, &row, "credential_id").await {
                tracing::warn!(target: "aegis_passkey", error = %e, "supabase passkey upsert failed; credential kept in memory");
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------- login ----

    /// `PublicKeyCredentialRequestOptions` for `navigator.credentials.get`.
    /// Empty `allowCredentials` = discoverable credentials (passkeys).
    pub async fn begin_login(&self) -> Value {
        let challenge = b64url(&random_challenge());
        {
            let mut map = self.login_challenges.lock().unwrap_or_else(|p| p.into_inner());
            let now = now_unix();
            map.retain(|_, exp| *exp > now);
            map.insert(challenge.clone(), now + CHALLENGE_TTL_SECS);
        }
        json!({
            "challenge": challenge,
            "rpId": self.rp_id,
            "timeout": TIMEOUT_MS,
            "userVerification": "preferred",
            "allowCredentials": [],
        })
    }

    /// Verifies an assertion and returns the subject the credential belongs to.
    /// The login challenge is single-use: consumed whether or not this succeeds.
    pub async fn finish_login(&self, credential: &Value) -> Result<String, AegisError> {
        if credential["type"].as_str() != Some("public-key") {
            return Err(err("passkey: credential type must be \"public-key\""));
        }
        let response = &credential["response"];
        let auth_data = b64url_dec(
            response["authenticatorData"].as_str().unwrap_or_default(),
            "response.authenticatorData",
        )?;
        let client_data_bytes = b64url_dec(
            response["clientDataJSON"].as_str().unwrap_or_default(),
            "response.clientDataJSON",
        )?;
        let signature_bytes = b64url_dec(
            response["signature"].as_str().unwrap_or_default(),
            "response.signature",
        )?;

        let client_data: Value =
            serde_json::from_slice(&client_data_bytes).map_err(|e| err(format!("passkey: clientDataJSON is not JSON: {e}")))?;
        let challenge = client_data["challenge"]
            .as_str()
            .ok_or_else(|| err("passkey: clientDataJSON.challenge missing"))?
            .to_string();
        self.take_login_challenge(&challenge)?;
        Self::check_client_data(&client_data, "webauthn.get", &challenge, &self.origin)?;

        let (new_count, _) = Self::parse_auth_data(&auth_data, &self.rp_id, false)?;

        // Resolve the credential (memory first, then Supabase, then cache).
        let cred_id = credential["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| credential["rawId"].as_str())
            .ok_or_else(|| err("passkey: credential id missing"))?
            .to_string();
        let mut stored = match self.credential(&cred_id).await {
            Some(c) => c,
            None => return Err(err("passkey: unknown credential")),
        };

        // Verify ES256 over authenticatorData || sha256(clientDataJSON).
        let vk = Self::verifying_key_from_cose(&stored.cose)?;
        let sig = Signature::from_der(&signature_bytes)
            .map_err(|e| err(format!("passkey: signature is not valid DER ES256: {e}")))?;
        let mut message = auth_data.clone();
        message.extend_from_slice(&Sha256::digest(&client_data_bytes));
        vk.verify(&message, &sig)
            .map_err(|_| err("passkey: signature verification failed"))?;

        // Sign counter must not go backwards; equal counts are a clone signal
        // once the counter has left zero.
        let old_count = stored.sign_count;
        if new_count < old_count || (new_count == old_count && old_count > 0) {
            return Err(err(format!(
                "passkey: sign count did not increase (stored {old_count}, got {new_count})"
            )));
        }
        stored.sign_count = new_count;
        self.credentials
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(cred_id.clone(), stored.clone());

        if let Some(s) = &self.store {
            if let Err(e) = s
                .update(TABLE, &format!("credential_id=eq.{cred_id}"), &json!({"sign_count": new_count}))
                .await
            {
                tracing::warn!(target: "aegis_passkey", error = %e, "supabase passkey sign_count update failed");
            }
        }
        Ok(stored.subject)
    }

    /// Best-effort hydration of stored credentials into memory at boot.
    /// Never fails: Supabase being unreachable just means memory-only until
    /// the next successful load.
    pub async fn load_all(&self) {
        let Some(store) = &self.store else { return };
        let rows = match store.select(TABLE, "order=created_at.asc").await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(target: "aegis_passkey", error = %e, "supabase passkey load failed; starting memory-only");
                return;
            }
        };
        let mut map = self.credentials.lock().unwrap_or_else(|p| p.into_inner());
        for row in rows {
            let (Some(id), Some(subject)) = (
                row["credential_id"].as_str(),
                row["subject"].as_str(),
            ) else {
                continue;
            };
            let Some(cose) = row["public_key_cose_b64"].as_str().and_then(|b| b64_std_dec(b).ok()) else {
                tracing::warn!(target: "aegis_passkey", credential_id = id, "skipping passkey row with undecodable COSE key");
                continue;
            };
            // Validate before caching so corrupt rows can't poison logins.
            if Self::verifying_key_from_cose(&cose).is_err() {
                tracing::warn!(target: "aegis_passkey", credential_id = id, "skipping passkey row with invalid COSE key");
                continue;
            }
            map.entry(id.to_string()).or_insert_with(|| StoredCredential {
                subject: subject.to_string(),
                cose,
                sign_count: row["sign_count"].as_i64().unwrap_or(0).clamp(0, u32::MAX as i64) as u32,
            });
        }
    }

    // ----------------------------------------------------------- private ----

    /// Consume the pending registration challenge bound to `subject`.
    /// Fails on unknown, expired or subject-mismatched challenges. Removing
    /// here makes the challenge single-use even if validation later fails.
    fn take_registration_challenge(&self, challenge: &str, subject: &str) -> Result<(), AegisError> {
        let mut map = self.reg_challenges.lock().unwrap_or_else(|p| p.into_inner());
        let now = now_unix();
        map.retain(|_, (_, exp)| *exp > now);
        match map.remove(challenge) {
            Some((bound_subject, exp)) => {
                if exp <= now {
                    Err(err("passkey: challenge expired"))
                } else if bound_subject != subject {
                    Err(err("passkey: challenge was issued to a different subject"))
                } else {
                    Ok(())
                }
            }
            None => Err(err("passkey: unknown or already used challenge")),
        }
    }

    /// Single-use login challenge: remove on first sight.
    fn take_login_challenge(&self, challenge: &str) -> Result<(), AegisError> {
        let mut map = self.login_challenges.lock().unwrap_or_else(|p| p.into_inner());
        let now = now_unix();
        map.retain(|_, exp| *exp > now);
        match map.remove(challenge) {
            Some(exp) if exp > now => Ok(()),
            Some(_) => Err(err("passkey: challenge expired")),
            None => Err(err("passkey: unknown or already used challenge")),
        }
    }

    fn check_client_data(cd: &Value, want_type: &str, challenge: &str, origin: &str) -> Result<(), AegisError> {
        let got_type = cd["type"].as_str().unwrap_or_default();
        if got_type != want_type {
            return Err(err(format!("passkey: clientDataJSON.type must be {want_type}, got {got_type:?}")));
        }
        if cd["challenge"].as_str() != Some(challenge) {
            return Err(err("passkey: challenge mismatch"));
        }
        if cd["origin"].as_str() != Some(origin) {
            return Err(err(format!("passkey: origin mismatch (want {origin})")));
        }
        Ok(())
    }

    /// Validate authenticatorData; when `require_attested` also slice out the
    /// attested credential data (credential id + COSE key).
    fn parse_auth_data<'a>(
        auth_data: &'a [u8],
        rp_id: &str,
        require_attested: bool,
    ) -> Result<(u32, AttestedCredData<'a>), AegisError> {
        if auth_data.len() < 37 {
            return Err(err("passkey: authenticatorData too short"));
        }
        let rp_id_hash = Sha256::digest(rp_id.as_bytes());
        if &auth_data[0..32] != rp_id_hash.as_slice() {
            return Err(err("passkey: rpIdHash mismatch"));
        }
        let flags = auth_data[32];
        if flags & FLAG_UP == 0 {
            return Err(err("passkey: user-present flag not set"));
        }
        if require_attested && flags & FLAG_AT == 0 {
            return Err(err("passkey: attested credential data flag not set"));
        }
        let sign_count = u32::from_be_bytes([auth_data[33], auth_data[34], auth_data[35], auth_data[36]]);

        let attested = if flags & FLAG_AT != 0 {
            // 16-byte aaguid + 2-byte credential id length + credential id + COSE key.
            if auth_data.len() < 55 {
                return Err(err("passkey: authenticatorData missing attested credential data"));
            }
            let cred_len = u16::from_be_bytes([auth_data[53], auth_data[54]]) as usize;
            let end = 55usize.checked_add(cred_len).ok_or_else(|| err("passkey: credential id length overflow"))?;
            if auth_data.len() < end {
                return Err(err("passkey: authenticatorData shorter than credential id"));
            }
            AttestedCredData {
                credential_id: &auth_data[55..end],
                cose: &auth_data[end..],
            }
        } else {
            AttestedCredData { credential_id: &[], cose: &[] }
        };
        Ok((sign_count, attested))
    }

    /// COSE Key (EC2 / P-256 / ES256) -> p256 VerifyingKey.
    fn verifying_key_from_cose(cose: &[u8]) -> Result<VerifyingKey, AegisError> {
        let v: ciborium::Value = ciborium::from_reader(std::io::Cursor::new(cose))
            .map_err(|e| err(format!("passkey: COSE key is not CBOR: {e}")))?;
        let map = v.as_map().ok_or_else(|| err("passkey: COSE key is not a CBOR map"))?;
        let field = |key: i64| {
            map.iter()
                .find(|(k, _)| k.as_integer().and_then(|i| i64::try_from(i).ok()) == Some(key))
                .map(|(_, v)| v)
        };
        let is_int = |key: i64, want: i64| {
            field(key)
                .and_then(|v| v.as_integer())
                .and_then(|i| i64::try_from(i).ok())
                == Some(want)
        };
        // 1: kty=2 (EC2), 3: alg=-7 (ES256), -1: crv=1 (P-256).
        if !is_int(1, 2) {
            return Err(err("passkey: COSE key kty must be 2 (EC2)"));
        }
        if !is_int(3, -7) {
            return Err(err("passkey: COSE key alg must be -7 (ES256)"));
        }
        if !is_int(-1, 1) {
            return Err(err("passkey: COSE key crv must be 1 (P-256)"));
        }
        let coord = |key: i64, name: &str| -> Result<[u8; 32], AegisError> {
            let bytes = field(key)
                .and_then(|v| v.as_bytes())
                .ok_or_else(|| err(format!("passkey: COSE key {name} coordinate missing")))?;
            let arr: [u8; 32] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| err(format!("passkey: COSE key {name} coordinate must be 32 bytes")))?;
            Ok(arr)
        };
        let x = coord(-2, "x")?;
        let y = coord(-3, "y")?;
        let mut sec1 = Vec::with_capacity(65);
        sec1.push(0x04); // uncompressed point
        sec1.extend_from_slice(&x);
        sec1.extend_from_slice(&y);
        VerifyingKey::from_sec1_bytes(&sec1).map_err(|e| err(format!("passkey: invalid P-256 public key: {e}")))
    }

    async fn credential(&self, cred_id: &str) -> Option<StoredCredential> {
        if let Some(hit) = self
            .credentials
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(cred_id)
        {
            return Some(hit.clone());
        }
        let store = self.store.as_ref()?;
        let rows = store
            .select(TABLE, &format!("credential_id=eq.{cred_id}"))
            .await
            .ok()?;
        let row = rows.into_iter().next()?;
        let subject = row["subject"].as_str()?.to_string();
        let cose = b64_std_dec(row["public_key_cose_b64"].as_str()?).ok()?;
        // Validate before caching so a corrupt row can't poison future logins.
        Self::verifying_key_from_cose(&cose).ok()?;
        let stored = StoredCredential {
            subject: subject.clone(),
            cose,
            sign_count: row["sign_count"].as_i64().unwrap_or(0).clamp(0, u32::MAX as i64) as u32,
        };
        self.credentials
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(cred_id.to_string(), stored.clone());
        Some(stored)
    }
}

/// Attested credential data sliced out of authenticatorData (empty for login).
struct AttestedCredData<'a> {
    credential_id: &'a [u8],
    cose: &'a [u8],
}

// ---------------------------------------------------------------- tests ----
// An in-test "authenticator": a p256 SigningKey plus hand-built CBOR.

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Signer;
    use p256::ecdsa::SigningKey;

    const RP_ID: &str = "localhost";
    const ORIGIN: &str = "https://localhost";

    fn svc() -> PasskeyService {
        PasskeyService::new(RP_ID.into(), ORIGIN.into(), None)
    }

    /// Deterministic-but-distinct keys per seed (sha256(seed) is a valid scalar).
    fn key(seed: &str) -> SigningKey {
        let d = Sha256::digest(seed.as_bytes());
        SigningKey::from_slice(&d).expect("valid signing key")
    }

    fn challenge_of(options: &Value) -> String {
        options["challenge"].as_str().expect("challenge").to_string()
    }

    /// COSE EC2 P-256 ES256 key for `sk`.
    fn cose_key(sk: &SigningKey) -> Vec<u8> {
        let sec1 = sk.verifying_key().to_sec1_bytes();
        assert_eq!(sec1.len(), 65, "uncompressed point");
        let (x, y) = (&sec1[1..33], &sec1[33..65]);
        let v = ciborium::Value::Map(vec![
            (ciborium::Value::Integer(1.into()), ciborium::Value::Integer(2.into())),   // kty: EC2
            (ciborium::Value::Integer(3.into()), ciborium::Value::Integer((-7).into())), // alg: ES256
            (ciborium::Value::Integer((-1).into()), ciborium::Value::Integer(1.into())), // crv: P-256
            (ciborium::Value::Integer((-2).into()), ciborium::Value::Bytes(x.to_vec())), // x
            (ciborium::Value::Integer((-3).into()), ciborium::Value::Bytes(y.to_vec())), // y
        ]);
        let mut out = Vec::new();
        ciborium::into_writer(&v, &mut out).expect("cose encode");
        out
    }

    /// rpIdHash + flags + sign count (+ attested credential data when registering).
    fn make_auth_data(cred_id: Option<&[u8]>, cose: &[u8], sign_count: u32) -> Vec<u8> {
        let mut ad = Vec::new();
        ad.extend_from_slice(&Sha256::digest(RP_ID.as_bytes()));
        // Registration: UP | UV | AT = 0x45. Login: UP | UV = 0x05.
        ad.push(if cred_id.is_some() { 0x45 } else { 0x05 });
        ad.extend_from_slice(&sign_count.to_be_bytes());
        if let Some(id) = cred_id {
            ad.extend_from_slice(&[0u8; 16]); // aaguid
            ad.extend_from_slice(&(id.len() as u16).to_be_bytes());
            ad.extend_from_slice(id);
            ad.extend_from_slice(cose);
        }
        ad
    }

    fn client_data(kind: &str, challenge: &str, origin: &str) -> Vec<u8> {
        json!({"type": kind, "challenge": challenge, "origin": origin, "crossOrigin": false}).to_string().into_bytes()
    }

    fn attestation_object(auth_data: &[u8]) -> Vec<u8> {
        let v = ciborium::Value::Map(vec![
            (ciborium::Value::Text("fmt".into()), ciborium::Value::Text("none".into())),
            (ciborium::Value::Text("attStmt".into()), ciborium::Value::Map(vec![])),
            (ciborium::Value::Text("authData".into()), ciborium::Value::Bytes(auth_data.to_vec())),
        ]);
        let mut out = Vec::new();
        ciborium::into_writer(&v, &mut out).expect("attestation encode");
        out
    }

    /// A `PublicKeyCredential` as a browser would serialize a `create()` result.
    fn reg_credential(
        sk: &SigningKey,
        cred_id: &[u8],
        challenge: &str,
        origin: &str,
        sign_count: u32,
    ) -> Value {
        let cose = cose_key(sk);
        let ad = make_auth_data(Some(cred_id), &cose, sign_count);
        json!({
            "id": b64url(cred_id),
            "rawId": b64url(cred_id),
            "type": "public-key",
            "response": {
                "attestationObject": b64url(&attestation_object(&ad)),
                "clientDataJSON": b64url(&client_data("webauthn.create", challenge, origin)),
            }
        })
    }

    /// A `PublicKeyCredential` as a browser would serialize a `get()` result.
    /// `sign_over` overrides the signed message (for the bad-signature test).
    fn login_credential(
        sk: &SigningKey,
        cred_id: &[u8],
        challenge: &str,
        origin: &str,
        sign_count: u32,
        sign_over: Option<Vec<u8>>,
    ) -> Value {
        let ad = make_auth_data(None, &[], sign_count);
        let cd = client_data("webauthn.get", challenge, origin);
        let mut msg = ad.clone();
        msg.extend_from_slice(&Sha256::digest(&cd));
        let msg = sign_over.unwrap_or(msg);
        let sig: p256::ecdsa::Signature = sk.sign(&msg);
        json!({
            "id": b64url(cred_id),
            "rawId": b64url(cred_id),
            "type": "public-key",
            "response": {
                "authenticatorData": b64url(&ad),
                "clientDataJSON": b64url(&cd),
                "signature": b64url(&sig.to_der().as_bytes()),
                "userHandle": b64url(b"maya"),
            }
        })
    }

    async fn registered_service() -> (PasskeyService, SigningKey) {
        let svc = svc();
        let sk = key("maya-passkey");
        let options = svc.begin_registration("maya").await;
        let cred = reg_credential(&sk, b"cred-id-01", &challenge_of(&options), ORIGIN, 0);
        svc.finish_registration("maya", &cred).await.expect("registration");
        (svc, sk)
    }

    #[tokio::test]
    async fn valid_registration_and_login_round_trip() {
        let (svc, sk) = registered_service().await;

        let options = svc.begin_login().await;
        assert_eq!(options["rpId"], json!(RP_ID));
        assert_eq!(options["allowCredentials"], json!([]));
        let cred = login_credential(&sk, b"cred-id-01", &challenge_of(&options), ORIGIN, 1, None);
        let subject = svc.finish_login(&cred).await.expect("login");
        assert_eq!(subject, "maya");
    }

    #[tokio::test]
    async fn registration_options_shape() {
        let svc = svc();
        let options = svc.begin_registration("maya").await;
        assert_eq!(options["rp"], json!({"id": RP_ID, "name": "AegisMCP"}));
        assert_eq!(options["pubKeyCredParams"], json!([{"type": "public-key", "alg": -7}]));
        assert_eq!(options["attestation"], json!("none"));
        assert_eq!(options["timeout"], json!(300000));
        assert_eq!(
            options["authenticatorSelection"],
            json!({"residentKey": "preferred", "userVerification": "preferred"})
        );
        assert_eq!(options["user"]["id"], json!(b64url(b"maya")));
        // >= 16 bytes of challenge, base64url without padding.
        let challenge = challenge_of(&options);
        assert!(challenge.len() >= 22 && !challenge.contains('+') && !challenge.contains('/') && !challenge.contains('='));
    }

    #[tokio::test]
    async fn wrong_origin_rejected() {
        let svc = svc();
        let options = svc.begin_registration("maya").await;
        let cred = reg_credential(&key("maya"), b"cred-id-01", &challenge_of(&options), "https://evil.example", 0);
        let e = svc.finish_registration("maya", &cred).await.unwrap_err();
        assert!(e.to_string().contains("origin"), "{e}");
    }

    #[tokio::test]
    async fn wrong_challenge_rejected() {
        let svc = svc();
        let _ = svc.begin_registration("maya").await; // issue a real one...
        let bogus = b64url(&[42u8; 32]); // ...but present a different challenge
        let cred = reg_credential(&key("maya"), b"cred-id-01", &bogus, ORIGIN, 0);
        let e = svc.finish_registration("maya", &cred).await.unwrap_err();
        assert!(e.to_string().contains("challenge"), "{e}");
    }

    #[tokio::test]
    async fn replayed_challenge_rejected() {
        let (svc, sk) = registered_service().await;

        // First login consumes the challenge.
        let options = svc.begin_login().await;
        let challenge = challenge_of(&options);
        let cred = login_credential(&sk, b"cred-id-01", &challenge, ORIGIN, 1, None);
        svc.finish_login(&cred).await.expect("first login ok");

        // Replay of the same assertion (same challenge) must fail.
        let e = svc.finish_login(&cred).await.unwrap_err();
        assert!(e.to_string().contains("challenge"), "{e}");
    }

    #[tokio::test]
    async fn bad_signature_rejected() {
        let (svc, sk) = registered_service().await;
        let options = svc.begin_login().await;
        // Correct ceremony, but the signature is over different bytes.
        let cred = login_credential(&sk, b"cred-id-01", &challenge_of(&options), ORIGIN, 1, Some(b"not the message".to_vec()));
        let e = svc.finish_login(&cred).await.unwrap_err();
        assert!(e.to_string().contains("signature"), "{e}");
    }

    #[tokio::test]
    async fn unknown_credential_rejected() {
        let svc = svc();
        let options = svc.begin_login().await;
        let cred = login_credential(&key("nobody"), b"never-registered", &challenge_of(&options), ORIGIN, 1, None);
        let e = svc.finish_login(&cred).await.unwrap_err();
        assert!(e.to_string().contains("unknown credential"), "{e}");
    }

    #[tokio::test]
    async fn sign_count_must_not_go_backwards_or_repeat() {
        let (svc, sk) = registered_service().await;

        // First login: count 1 (stored 0 -> 1 ok).
        let options = svc.begin_login().await;
        let cred = login_credential(&sk, b"cred-id-01", &challenge_of(&options), ORIGIN, 1, None);
        svc.finish_login(&cred).await.expect("first login");

        // Same count again (stored 1, got 1) -> clone signal, rejected.
        let options = svc.begin_login().await;
        let cred = login_credential(&sk, b"cred-id-01", &challenge_of(&options), ORIGIN, 1, None);
        let e = svc.finish_login(&cred).await.unwrap_err();
        assert!(e.to_string().contains("sign count"), "{e}");

        // Count going backwards, rejected.
        let options = svc.begin_login().await;
        let cred = login_credential(&sk, b"cred-id-01", &challenge_of(&options), ORIGIN, 0, None);
        let e = svc.finish_login(&cred).await.unwrap_err();
        assert!(e.to_string().contains("sign count"), "{e}");

        // Forward works again.
        let options = svc.begin_login().await;
        let cred = login_credential(&sk, b"cred-id-01", &challenge_of(&options), ORIGIN, 2, None);
        assert_eq!(svc.finish_login(&cred).await.unwrap(), "maya");
    }

    #[tokio::test]
    async fn registration_without_attested_credential_data_rejected() {
        let svc = svc();
        let options = svc.begin_registration("maya").await;
        // Login-shaped authData (UP only, no AT bit) must not register.
        let ad = make_auth_data(None, &[], 0);
        let cd = client_data("webauthn.create", &challenge_of(&options), ORIGIN);
        let cred = json!({
            "id": b64url(b"cred-id-01"),
            "rawId": b64url(b"cred-id-01"),
            "type": "public-key",
            "response": {
                "attestationObject": b64url(&attestation_object(&ad)),
                "clientDataJSON": b64url(&cd),
            }
        });
        let e = svc.finish_registration("maya", &cred).await.unwrap_err();
        assert!(e.to_string().contains("attested credential data"), "{e}");
    }

    /// Supabase writes are best-effort: an unreachable store must not fail
    /// registration or boot hydration.
    #[tokio::test]
    async fn supabase_outage_never_fails_the_ceremony() {
        // Port 9 (discard) refuses connections immediately.
        let store = SupabaseStore::new("http://127.0.0.1:9", "test-key");
        let svc = PasskeyService::new(RP_ID.into(), ORIGIN.into(), Some(store));
        svc.load_all().await; // must not panic

        let sk = key("maya");
        let options = svc.begin_registration("maya").await;
        let cred = reg_credential(&sk, b"cred-id-01", &challenge_of(&options), ORIGIN, 0);
        svc.finish_registration("maya", &cred).await.expect("registration despite store outage");

        let options = svc.begin_login().await;
        let cred = login_credential(&sk, b"cred-id-01", &challenge_of(&options), ORIGIN, 1, None);
        assert_eq!(svc.finish_login(&cred).await.unwrap(), "maya");
    }
}
