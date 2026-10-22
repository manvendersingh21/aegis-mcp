use aegis_core::{
    tool_spec, AegisError, Ledger, LedgerEntry, Metrics, Mode, Outcome, Receipt, ReceiptClaims,
    ReceiptSigner, Resource,
};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Mutex;

const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn storage_err(err: impl ToString) -> AegisError {
    AegisError::Storage(err.to_string())
}

fn resource_label(resource: Resource) -> &'static str {
    match resource {
        Resource::Financials => "financials",
        Resource::Calendar => "calendar",
        Resource::Health => "health",
        Resource::Identity => "identity",
        Resource::Contacts => "contacts",
        Resource::DeveloperSecrets => "developer_secrets",
    }
}

fn chain_hash(prev_hash: &str, unhashed_entry_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash.as_bytes());
    hasher.update(unhashed_entry_json.as_bytes());
    hex::encode(hasher.finalize())
}

#[derive(Clone)]
pub struct Ed25519Signer {
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
}

impl Ed25519Signer {
    pub fn generate() -> Self {
        let seed: [u8; 32] = rand::random();
        Self::from_seed(seed)
    }

    pub fn from_seed(seed: [u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(&seed);
        let verifying_key = signing_key.verifying_key();
        Self {
            signing_key,
            verifying_key,
        }
    }
}

impl ReceiptSigner for Ed25519Signer {
    fn issue(&self, claims: ReceiptClaims) -> Receipt {
        let payload = serde_json::to_vec(&claims).unwrap_or_default();
        let signature = self.signing_key.sign(&payload);
        Receipt {
            claims,
            signature: format!("ed25519:{}", hex::encode(signature.to_bytes())),
            revoked: false,
        }
    }

    fn verify(&self, receipt: &Receipt) -> bool {
        let Some(encoded_sig) = receipt.signature.strip_prefix("ed25519:") else {
            return false;
        };
        let sig_bytes = match hex::decode(encoded_sig) {
            Ok(bytes) => bytes,
            Err(_) => return false,
        };
        let signature = match Signature::from_slice(&sig_bytes) {
            Ok(sig) => sig,
            Err(_) => return false,
        };
        let payload = match serde_json::to_vec(&receipt.claims) {
            Ok(bytes) => bytes,
            Err(_) => return false,
        };
        self.verifying_key.verify(&payload, &signature).is_ok()
    }

    fn public_key_hex(&self) -> String {
        hex::encode(self.verifying_key.to_bytes())
    }
}

pub struct SqliteLedger {
    conn: Mutex<Connection>,
}

impl SqliteLedger {
    pub fn open_in_memory() -> Result<Self, AegisError> {
        let conn = Connection::open_in_memory().map_err(storage_err)?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, AegisError> {
        let conn = Connection::open(path).map_err(storage_err)?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), AegisError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS receipts (
                json TEXT NOT NULL,
                subject TEXT NOT NULL,
                agent TEXT NOT NULL,
                resource TEXT NOT NULL,
                expires_at INTEGER NOT NULL,
                revoked INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS audit_ledger (
                seq INTEGER PRIMARY KEY,
                subject TEXT NOT NULL,
                json TEXT NOT NULL,
                entry_hash TEXT NOT NULL
            );",
        )
        .map_err(storage_err)
    }

    fn read_entries(&self, subject: Option<&str>) -> Vec<LedgerEntry> {
        let conn = match self.conn.lock() {
            Ok(conn) => conn,
            Err(_) => return Vec::new(),
        };

        let mut out = Vec::new();
        if let Some(subject) = subject {
            let mut stmt = match conn.prepare(
                "SELECT json FROM audit_ledger WHERE subject = ?1 ORDER BY seq DESC",
            ) {
                Ok(stmt) => stmt,
                Err(_) => return Vec::new(),
            };
            let rows = match stmt.query_map(params![subject], |row| row.get::<_, String>(0)) {
                Ok(rows) => rows,
                Err(_) => return Vec::new(),
            };
            for row in rows {
                if let Ok(json) = row {
                    if let Ok(entry) = serde_json::from_str::<LedgerEntry>(&json) {
                        out.push(entry);
                    }
                }
            }
        } else {
            let mut stmt = match conn.prepare("SELECT json FROM audit_ledger ORDER BY seq DESC") {
                Ok(stmt) => stmt,
                Err(_) => return Vec::new(),
            };
            let rows = match stmt.query_map([], |row| row.get::<_, String>(0)) {
                Ok(rows) => rows,
                Err(_) => return Vec::new(),
            };
            for row in rows {
                if let Ok(json) = row {
                    if let Ok(entry) = serde_json::from_str::<LedgerEntry>(&json) {
                        out.push(entry);
                    }
                }
            }
        }

        out
    }
}

impl Ledger for SqliteLedger {
    fn append(&self, entry: LedgerEntry) -> Result<LedgerEntry, AegisError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| storage_err("sqlite mutex poisoned"))?;
        let prev_hash = conn
            .query_row(
                "SELECT entry_hash FROM audit_ledger ORDER BY seq DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(storage_err)?
            .unwrap_or_else(|| ZERO_HASH.to_string());

        let mut unhashed = entry;
        unhashed.entry_hash.clear();
        let unhashed_json = serde_json::to_string(&unhashed).map_err(storage_err)?;
        let entry_hash = chain_hash(&prev_hash, &unhashed_json);

        unhashed.entry_hash = entry_hash.clone();
        let stored_json = serde_json::to_string(&unhashed).map_err(storage_err)?;
        conn.execute(
            "INSERT INTO audit_ledger (subject, json, entry_hash) VALUES (?1, ?2, ?3)",
            params![unhashed.subject.as_str(), stored_json.as_str(), entry_hash.as_str()],
        )
        .map_err(storage_err)?;

        Ok(unhashed)
    }

    fn recent(&self, subject: Option<&str>, limit: usize) -> Vec<LedgerEntry> {
        if limit == 0 {
            return Vec::new();
        }

        let conn = match self.conn.lock() {
            Ok(conn) => conn,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();

        if let Some(subject) = subject {
            let mut stmt = match conn.prepare(
                "SELECT json FROM audit_ledger
                 WHERE subject = ?1
                 ORDER BY seq DESC
                 LIMIT ?2",
            ) {
                Ok(stmt) => stmt,
                Err(_) => return Vec::new(),
            };
            let rows = match stmt.query_map(params![subject, limit as i64], |row| {
                row.get::<_, String>(0)
            }) {
                Ok(rows) => rows,
                Err(_) => return Vec::new(),
            };
            for row in rows {
                if let Ok(json) = row {
                    if let Ok(entry) = serde_json::from_str::<LedgerEntry>(&json) {
                        out.push(entry);
                    }
                }
            }
            return out;
        }

        let mut stmt = match conn.prepare(
            "SELECT json FROM audit_ledger
             ORDER BY seq DESC
             LIMIT ?1",
        ) {
            Ok(stmt) => stmt,
            Err(_) => return Vec::new(),
        };
        let rows = match stmt.query_map(params![limit as i64], |row| row.get::<_, String>(0)) {
            Ok(rows) => rows,
            Err(_) => return Vec::new(),
        };
        for row in rows {
            if let Ok(json) = row {
                if let Ok(entry) = serde_json::from_str::<LedgerEntry>(&json) {
                    out.push(entry);
                }
            }
        }

        out
    }

    fn verify_chain(&self) -> bool {
        let conn = match self.conn.lock() {
            Ok(conn) => conn,
            Err(_) => return false,
        };

        let mut stmt = match conn.prepare("SELECT json, entry_hash FROM audit_ledger ORDER BY seq ASC")
        {
            Ok(stmt) => stmt,
            Err(_) => return false,
        };

        let rows = match stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) {
            Ok(rows) => rows,
            Err(_) => return false,
        };

        let mut prev_hash = ZERO_HASH.to_string();
        for row in rows {
            let (json, stored_hash) = match row {
                Ok(row) => row,
                Err(_) => return false,
            };
            let entry = match serde_json::from_str::<LedgerEntry>(&json) {
                Ok(entry) => entry,
                Err(_) => return false,
            };

            let mut unhashed = entry.clone();
            unhashed.entry_hash.clear();
            let unhashed_json = match serde_json::to_string(&unhashed) {
                Ok(json) => json,
                Err(_) => return false,
            };
            let expected = chain_hash(&prev_hash, &unhashed_json);
            if expected != stored_hash || stored_hash != entry.entry_hash {
                return false;
            }
            prev_hash = stored_hash;
        }

        true
    }

    fn store_receipt(&self, receipt: &Receipt) -> Result<(), AegisError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| storage_err("sqlite mutex poisoned"))?;

        // Replace by logical key (receipt_id) while keeping the required table shape.
        let mut existing_rowids = Vec::new();
        {
            let mut stmt = conn
                .prepare("SELECT rowid, json FROM receipts")
                .map_err(storage_err)?;
            let rows = stmt
                .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
                .map_err(storage_err)?;
            for row in rows {
                let (rowid, json) = row.map_err(storage_err)?;
                if let Ok(existing) = serde_json::from_str::<Receipt>(&json) {
                    if existing.claims.receipt_id == receipt.claims.receipt_id {
                        existing_rowids.push(rowid);
                    }
                }
            }
        }

        for rowid in existing_rowids {
            conn.execute("DELETE FROM receipts WHERE rowid = ?1", params![rowid])
                .map_err(storage_err)?;
        }

        let receipt_json = serde_json::to_string(receipt).map_err(storage_err)?;
        let revoked = if receipt.revoked { 1_i64 } else { 0_i64 };
        conn.execute(
            "INSERT INTO receipts (json, subject, agent, resource, expires_at, revoked)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                receipt_json.as_str(),
                receipt.claims.subject.as_str(),
                receipt.claims.grantee_agent.as_str(),
                resource_label(receipt.claims.resource),
                receipt.claims.expires_at,
                revoked
            ],
        )
        .map_err(storage_err)?;
        Ok(())
    }

    fn get_receipt(&self, receipt_id: &str) -> Option<Receipt> {
        let conn = self.conn.lock().ok()?;
        let mut stmt = conn
            .prepare("SELECT json FROM receipts ORDER BY rowid DESC")
            .ok()?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0)).ok()?;
        for row in rows {
            let Ok(json) = row else {
                continue;
            };
            let Ok(receipt) = serde_json::from_str::<Receipt>(&json) else {
                continue;
            };
            if receipt.claims.receipt_id == receipt_id {
                return Some(receipt);
            }
        }
        None
    }

    fn active_receipt(
        &self,
        subject: &str,
        agent_id: &str,
        resource: Resource,
        now: i64,
    ) -> Option<Receipt> {
        let conn = self.conn.lock().ok()?;
        let mut stmt = conn
            .prepare(
                "SELECT json FROM receipts
                 WHERE subject = ?1
                   AND agent = ?2
                   AND resource = ?3
                   AND revoked = 0
                   AND expires_at > ?4
                 ORDER BY expires_at DESC, rowid DESC",
            )
            .ok()?;
        let rows = stmt
            .query_map(
                params![subject, agent_id, resource_label(resource), now],
                |row| row.get::<_, String>(0),
            )
            .ok()?;

        for row in rows {
            let Ok(json) = row else {
                continue;
            };
            let Ok(receipt) = serde_json::from_str::<Receipt>(&json) else {
                continue;
            };
            if receipt.claims.subject == subject
                && receipt.claims.grantee_agent == agent_id
                && receipt.claims.resource == resource
                && receipt.is_active(now)
            {
                return Some(receipt);
            }
        }
        None
    }

    fn active_receipts(&self, subject: Option<&str>, now: i64) -> Vec<Receipt> {
        let conn = match self.conn.lock() {
            Ok(conn) => conn,
            Err(_) => return Vec::new(),
        };

        let mut out = Vec::new();
        if let Some(subject) = subject {
            let mut stmt = match conn.prepare(
                "SELECT json FROM receipts
                 WHERE subject = ?1
                   AND revoked = 0
                   AND expires_at > ?2
                 ORDER BY expires_at DESC, rowid DESC",
            ) {
                Ok(stmt) => stmt,
                Err(_) => return Vec::new(),
            };
            let rows = match stmt.query_map(params![subject, now], |row| row.get::<_, String>(0))
            {
                Ok(rows) => rows,
                Err(_) => return Vec::new(),
            };
            for row in rows {
                if let Ok(json) = row {
                    if let Ok(receipt) = serde_json::from_str::<Receipt>(&json) {
                        if receipt.is_active(now) {
                            out.push(receipt);
                        }
                    }
                }
            }
            return out;
        }

        let mut stmt = match conn.prepare(
            "SELECT json FROM receipts
             WHERE revoked = 0
               AND expires_at > ?1
             ORDER BY expires_at DESC, rowid DESC",
        ) {
            Ok(stmt) => stmt,
            Err(_) => return Vec::new(),
        };
        let rows = match stmt.query_map(params![now], |row| row.get::<_, String>(0)) {
            Ok(rows) => rows,
            Err(_) => return Vec::new(),
        };
        for row in rows {
            if let Ok(json) = row {
                if let Ok(receipt) = serde_json::from_str::<Receipt>(&json) {
                    if receipt.is_active(now) {
                        out.push(receipt);
                    }
                }
            }
        }

        out
    }

    fn revoke_all(&self, subject: Option<&str>) -> Vec<String> {
        let now = aegis_core::now_unix();
        let conn = match self.conn.lock() {
            Ok(conn) => conn,
            Err(_) => return Vec::new(),
        };

        let mut updated_rows = Vec::new();
        if let Some(subject) = subject {
            let mut stmt = match conn.prepare(
                "SELECT rowid, json FROM receipts
                 WHERE subject = ?1
                   AND revoked = 0
                   AND expires_at > ?2
                 ORDER BY rowid ASC",
            ) {
                Ok(stmt) => stmt,
                Err(_) => return Vec::new(),
            };
            let rows = match stmt.query_map(params![subject, now], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            }) {
                Ok(rows) => rows,
                Err(_) => return Vec::new(),
            };
            for row in rows {
                let Ok((rowid, json)) = row else {
                    continue;
                };
                let Ok(mut receipt) = serde_json::from_str::<Receipt>(&json) else {
                    continue;
                };
                if receipt.is_active(now) {
                    receipt.revoked = true;
                    if let Ok(updated_json) = serde_json::to_string(&receipt) {
                        updated_rows.push((rowid, receipt.claims.receipt_id, updated_json));
                    }
                }
            }
        } else {
            let mut stmt = match conn.prepare(
                "SELECT rowid, json FROM receipts
                 WHERE revoked = 0
                   AND expires_at > ?1
                 ORDER BY rowid ASC",
            ) {
                Ok(stmt) => stmt,
                Err(_) => return Vec::new(),
            };
            let rows = match stmt.query_map(params![now], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            }) {
                Ok(rows) => rows,
                Err(_) => return Vec::new(),
            };
            for row in rows {
                let Ok((rowid, json)) = row else {
                    continue;
                };
                let Ok(mut receipt) = serde_json::from_str::<Receipt>(&json) else {
                    continue;
                };
                if receipt.is_active(now) {
                    receipt.revoked = true;
                    if let Ok(updated_json) = serde_json::to_string(&receipt) {
                        updated_rows.push((rowid, receipt.claims.receipt_id, updated_json));
                    }
                }
            }
        }

        let mut revoked_ids = Vec::new();
        for (rowid, receipt_id, updated_json) in updated_rows {
            if conn
                .execute(
                    "UPDATE receipts SET json = ?1, revoked = 1 WHERE rowid = ?2",
                    params![updated_json.as_str(), rowid],
                )
                .is_ok()
            {
                revoked_ids.push(receipt_id);
            }
        }

        revoked_ids
    }

    fn metrics(&self, subject: Option<&str>, now: i64) -> Metrics {
        let entries = self.read_entries(subject);
        let mut metrics = Metrics {
            total_calls: entries.len() as u64,
            ..Metrics::default()
        };

        for entry in &entries {
            metrics.bytes_raw = metrics.bytes_raw.saturating_add(entry.bytes_raw as u64);
            metrics.bytes_released = metrics
                .bytes_released
                .saturating_add(entry.bytes_released as u64);
            if entry.outcome == Outcome::Violation {
                metrics.violations = metrics.violations.saturating_add(1);
            }

            if let Some(spec) = tool_spec(&entry.tool) {
                let blocked = entry.policy_applied != Mode::Allow || entry.outcome != Outcome::Released;
                for requested_field in &entry.raw_requested {
                    if spec
                        .sensitive_fields
                        .iter()
                        .any(|sensitive| *sensitive == requested_field.as_str())
                    {
                        metrics.sensitive_fields_requested =
                            metrics.sensitive_fields_requested.saturating_add(1);
                        if blocked {
                            metrics.sensitive_fields_blocked =
                                metrics.sensitive_fields_blocked.saturating_add(1);
                        }
                    }
                }
            }
        }

        metrics.active_leases = self.active_receipts(subject, now).len() as u64;
        if metrics.sensitive_fields_requested == 0 {
            metrics.pii_blocked_pct = 0.0;
        } else {
            metrics.pii_blocked_pct = (metrics.sensitive_fields_blocked as f64)
                * 100.0
                / (metrics.sensitive_fields_requested as f64);
        }

        metrics
    }
}
