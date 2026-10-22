//! SIWE wallet authentication and privacy-preserving chain anchors.

use alloy::{
    network::TransactionBuilder,
    primitives::{Address, Signature, U256},
    providers::{Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
    rpc::types::TransactionRequest,
};
use aegis_core::{AegisError, BoxFuture, ChainAnchor, WalletAuth};
use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, str::FromStr, sync::Mutex, time::Duration as StdDuration};

const NONCE_TTL: Duration = Duration::minutes(10);

/// Format a standards-shaped EIP-4361 message. The wallet signs this exact text
/// with EIP-191 `personal_sign`.
pub fn siwe_message(
    domain: &str,
    address: &str,
    statement: &str,
    uri: &str,
    chain_id: u64,
    nonce: &str,
    issued_at: &str,
) -> String {
    format!(
        "{domain} wants you to sign in with your Ethereum account:\n{address}\n\n{statement}\n\nURI: {uri}\nVersion: 1\nChain ID: {chain_id}\nNonce: {nonce}\nIssued At: {issued_at}"
    )
}

/// Build the on-chain payload for a ledger head. Only a SHA-256 digest of the
/// subject is included, so the person's identifier never appears in calldata.
pub fn anchor_calldata(subject: &str, head_hash: &str) -> Vec<u8> {
    let subject_hash = hex::encode(Sha256::digest(subject.as_bytes()));
    format!("AEGIS1:{subject_hash}:{head_hash}").into_bytes()
}

/// Stateful nonce and wallet mapping store used by the gateway SIWE routes.
pub struct SiweAuth {
    domain: String,
    chain_id: u64,
    nonces: Mutex<HashMap<String, DateTime<Utc>>>,
    links: Mutex<WalletLinks>,
}

#[derive(Default)]
struct WalletLinks {
    by_subject: HashMap<String, String>,
    by_address: HashMap<String, String>,
}

struct ParsedSiwe {
    address: Address,
    nonce: String,
    expires_at: Option<DateTime<Utc>>,
}

impl SiweAuth {
    pub fn new(domain: String, chain_id: u64) -> Self {
        Self {
            domain,
            chain_id,
            nonces: Mutex::new(HashMap::new()),
            links: Mutex::new(WalletLinks::default()),
        }
    }

    fn verify_signed(&self, message: &str, signature: &str) -> Result<Address, AegisError> {
        let parsed = self.parse_message(message)?;
        let signature = Signature::from_str(signature)
            .map_err(|_| AegisError::Other("invalid SIWE signature".into()))?;
        let recovered = signature
            .recover_address_from_msg(message.as_bytes())
            .map_err(|_| AegisError::Other("invalid SIWE signature".into()))?;
        if recovered != parsed.address {
            return Err(AegisError::Other("SIWE signature address mismatch".into()));
        }

        // Consume only after signature verification so invalid requests cannot
        // burn another user's nonce. Removal is done while holding the lock to
        // make concurrent replay attempts single-use as well.
        let now = Utc::now();
        if parsed.expires_at.is_some_and(|expires| expires <= now) {
            return Err(AegisError::Other("SIWE message expired".into()));
        }
        let mut nonces = self.nonces.lock().unwrap_or_else(|e| e.into_inner());
        match nonces.remove(&parsed.nonce) {
            Some(expires) if expires > now => Ok(parsed.address),
            Some(_) => Err(AegisError::Other("SIWE nonce expired".into())),
            None => Err(AegisError::Other("invalid or already used SIWE nonce".into())),
        }
    }

    fn parse_message(&self, message: &str) -> Result<ParsedSiwe, AegisError> {
        let invalid = || AegisError::Other("invalid SIWE message".into());
        let mut lines = message.lines();
        let header = lines.next().ok_or_else(invalid)?;
        let prefix = format!("{} wants you to sign in with your Ethereum account:", self.domain);
        if header != prefix {
            return Err(AegisError::Other("SIWE domain mismatch".into()));
        }
        let address = lines
            .next()
            .ok_or_else(invalid)?
            .parse::<Address>()
            .map_err(|_| AegisError::Other("invalid SIWE address".into()))?;

        let mut fields: HashMap<&str, Vec<&str>> = HashMap::new();
        for line in lines {
            if let Some((key, value)) = line.split_once(": ") {
                if matches!(key, "URI" | "Version" | "Chain ID" | "Nonce" | "Issued At" | "Expiration Time") {
                    fields.entry(key).or_default().push(value);
                }
            }
        }
        let one = |key: &str| -> Result<&str, AegisError> {
            match fields.get(key).map(Vec::as_slice) {
                Some([value]) => Ok(value),
                _ => Err(invalid()),
            }
        };
        let uri = one("URI")?;
        if uri.is_empty() || one("Version")? != "1" {
            return Err(invalid());
        }
        let chain_id = one("Chain ID")?
            .parse::<u64>()
            .map_err(|_| invalid())?;
        if chain_id != self.chain_id {
            return Err(AegisError::Other("SIWE chain ID mismatch".into()));
        }
        let nonce = one("Nonce")?.to_owned();
        if nonce.is_empty() || !nonce.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(invalid());
        }
        let issued_at = DateTime::parse_from_rfc3339(one("Issued At")?)
            .map_err(|_| invalid())?;
        // Require a valid RFC3339 timestamp. A short clock skew allowance is
        // useful for clients while preventing arbitrarily future-dated text.
        if issued_at.with_timezone(&Utc) > Utc::now() + Duration::minutes(5) {
            return Err(AegisError::Other("SIWE issued-at time is in the future".into()));
        }
        let expires_at = match fields.get("Expiration Time").map(Vec::as_slice) {
            None => None,
            Some([value]) => Some(
                DateTime::parse_from_rfc3339(value)
                    .map_err(|_| invalid())?
                    .with_timezone(&Utc),
            ),
            _ => return Err(invalid()),
        };
        Ok(ParsedSiwe { address, nonce, expires_at })
    }
}

impl WalletAuth for SiweAuth {
    fn nonce(&self) -> String {
        let now = Utc::now();
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let mut nonces = self.nonces.lock().unwrap_or_else(|e| e.into_inner());
        nonces.retain(|_, expires| *expires > now);
        nonces.insert(nonce.clone(), now + NONCE_TTL);
        nonce
    }

    fn verify_login<'a>(&'a self, message: &'a str, signature: &'a str) -> BoxFuture<'a, Result<String, AegisError>> {
        Box::pin(async move {
            let address = self.verify_signed(message, signature)?;
            let checksum = address.to_checksum(None);
            let links = self.links.lock().unwrap_or_else(|e| e.into_inner());
            links.by_address.get(&checksum.to_ascii_lowercase()).cloned()
                .ok_or_else(|| AegisError::Other("wallet is not linked to a person".into()))
        })
    }

    fn link<'a>(&'a self, subject: &'a str, message: &'a str, signature: &'a str) -> BoxFuture<'a, Result<String, AegisError>> {
        Box::pin(async move {
            let address = self.verify_signed(message, signature)?;
            let checksum = address.to_checksum(None);
            let address_key = checksum.to_ascii_lowercase();
            let mut links = self.links.lock().unwrap_or_else(|e| e.into_inner());

            // Linking replaces either side of the one-to-one relationship.
            if let Some(old_address) = links.by_subject.remove(subject) {
                links.by_address.remove(&old_address.to_ascii_lowercase());
            }
            if let Some(old_subject) = links.by_address.remove(&address_key) {
                links.by_subject.remove(&old_subject);
            }
            links.by_subject.insert(subject.to_owned(), checksum.clone());
            links.by_address.insert(address_key, subject.to_owned());
            Ok(checksum)
        })
    }

    fn wallet_of(&self, subject: &str) -> Option<String> {
        self.links.lock().unwrap_or_else(|e| e.into_inner()).by_subject.get(subject).cloned()
    }
}

/// Creates zero-value transactions that anchor a ledger head without exposing
/// the person's subject on chain.
pub struct ChainAnchorer {
    signer: PrivateKeySigner,
    rpc: String,
    chain_id: u64,
    explorer: String,
}

impl ChainAnchorer {
    /// Returns `None` when no signing key is configured. The key value is never
    /// included in errors or logs.
    pub fn from_env() -> Option<Self> {
        let key = std::env::var("AEGIS_CHAIN_KEY").ok()?;
        let signer = PrivateKeySigner::from_str(&key).ok()?;
        let chain_id = std::env::var("AEGIS_CHAIN_ID")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(84_532);
        let rpc = std::env::var("AEGIS_CHAIN_RPC")
            .unwrap_or_else(|_| "https://sepolia.base.org".to_owned());
        let explorer = std::env::var("AEGIS_EXPLORER")
            .unwrap_or_else(|_| "https://sepolia.basescan.org".to_owned())
            .trim_end_matches('/')
            .to_owned();
        Some(Self { signer, rpc, chain_id, explorer })
    }

    pub fn address(&self) -> String {
        self.signer.address().to_checksum(None)
    }

    pub fn anchor_calldata(subject: &str, head_hash: &str) -> Vec<u8> {
        anchor_calldata(subject, head_hash)
    }

    pub async fn anchor(&self, subject: &str, head_hash: &str) -> Result<ChainAnchor, AegisError> {
        let url = self.rpc.parse().map_err(|e| AegisError::Other(format!("invalid chain RPC URL: {e}")))?;
        let provider = ProviderBuilder::new()
            .with_chain_id(self.chain_id)
            .wallet(self.signer.clone())
            .connect_http(url);
        let address = self.signer.address();
        let request = TransactionRequest::default()
            .with_to(address)
            .with_value(U256::ZERO)
            .with_input(anchor_calldata(subject, head_hash))
            .with_chain_id(self.chain_id)
            .with_max_fee_per_gas(2_000_000_000)
            .with_max_priority_fee_per_gas(100_000_000);
        let submitted = provider.send_transaction(request).await
            .map_err(|e| AegisError::Other(format!("chain transaction submission failed: {e}")))?;
        let receipt = submitted
            .with_required_confirmations(1)
            .with_timeout(Some(StdDuration::from_secs(60)))
            .get_receipt()
            .await
            .map_err(|e| AegisError::Other(format!("chain transaction confirmation failed: {e}")))?;
        let tx_hash = format!("{:#x}", receipt.transaction_hash);
        Ok(ChainAnchor {
            subject: subject.to_owned(),
            head_hash: head_hash.to_owned(),
            chain_id: self.chain_id,
            tx_hash: tx_hash.clone(),
            explorer_url: format!("{}/tx/{tx_hash}", self.explorer),
            anchored_at: Utc::now().timestamp(),
        })
    }
}
