use aegis_chain::{siwe_message, ChainAnchorer, SiweAuth};
use aegis_core::WalletAuth;
use alloy::signers::{local::PrivateKeySigner, Signer};
use chrono::Utc;
use sha2::{Digest, Sha256};

const DOMAIN: &str = "127.0.0.1:8787";
const URI: &str = "http://127.0.0.1:8787";
const STATEMENT: &str = "Sign in to AegisMCP - your consent firewall.";
const CHAIN_ID: u64 = 84532;

async fn sign_message_hex(signer: &PrivateKeySigner, message: &str) -> String {
    let sig = signer.sign_message(message.as_bytes()).await.expect("sign SIWE message");
    let mut hex = sig.to_string();
    if !hex.starts_with("0x") {
        hex = format!("0x{hex}");
    }
    hex
}

fn siwe_for(address: &str, nonce: &str, domain: &str, chain_id: u64, issued_at: &str) -> String {
    siwe_message(domain, address, STATEMENT, URI, chain_id, nonce, issued_at)
}

#[tokio::test]
async fn link_and_login_round_trip() {
    let auth = SiweAuth::new(DOMAIN.to_string(), CHAIN_ID);
    let signer = PrivateKeySigner::random();
    let address = signer.address().to_string();
    let issued_at = Utc::now().to_rfc3339();

    let link_nonce = auth.nonce();
    let link_msg = siwe_for(&address, &link_nonce, DOMAIN, CHAIN_ID, &issued_at);
    let link_sig = sign_message_hex(&signer, &link_msg).await;
    let linked = auth.link("maya", &link_msg, &link_sig).await.expect("link should succeed");
    assert_eq!(linked.to_lowercase(), address.to_lowercase());
    assert_eq!(auth.wallet_of("maya").map(|v| v.to_lowercase()), Some(address.to_lowercase()));

    let login_nonce = auth.nonce();
    let login_msg = siwe_for(&address, &login_nonce, DOMAIN, CHAIN_ID, &issued_at);
    let login_sig = sign_message_hex(&signer, &login_msg).await;
    let subject = auth.verify_login(&login_msg, &login_sig).await.expect("login should succeed");
    assert_eq!(subject, "maya");
}

#[tokio::test]
async fn reused_nonce_is_rejected() {
    let auth = SiweAuth::new(DOMAIN.to_string(), CHAIN_ID);
    let signer = PrivateKeySigner::random();
    let address = signer.address().to_string();
    let issued_at = Utc::now().to_rfc3339();

    let link_nonce = auth.nonce();
    let link_msg = siwe_for(&address, &link_nonce, DOMAIN, CHAIN_ID, &issued_at);
    let link_sig = sign_message_hex(&signer, &link_msg).await;
    auth.link("maya", &link_msg, &link_sig).await.expect("link should succeed");

    let nonce = auth.nonce();
    let login_msg = siwe_for(&address, &nonce, DOMAIN, CHAIN_ID, &issued_at);
    let login_sig = sign_message_hex(&signer, &login_msg).await;
    auth.verify_login(&login_msg, &login_sig).await.expect("first login should succeed");
    assert!(auth.verify_login(&login_msg, &login_sig).await.is_err(), "reused nonce must fail");
}

#[tokio::test]
async fn unknown_nonce_is_rejected() {
    let auth = SiweAuth::new(DOMAIN.to_string(), CHAIN_ID);
    let signer = PrivateKeySigner::random();
    let address = signer.address().to_string();
    let issued_at = Utc::now().to_rfc3339();

    let link_nonce = auth.nonce();
    let link_msg = siwe_for(&address, &link_nonce, DOMAIN, CHAIN_ID, &issued_at);
    let link_sig = sign_message_hex(&signer, &link_msg).await;
    auth.link("maya", &link_msg, &link_sig).await.expect("link should succeed");

    let unknown_nonce = "NotIssuedNonce1234";
    let login_msg = siwe_for(&address, unknown_nonce, DOMAIN, CHAIN_ID, &issued_at);
    let login_sig = sign_message_hex(&signer, &login_msg).await;
    assert!(auth.verify_login(&login_msg, &login_sig).await.is_err(), "unknown nonce must fail");
}

#[tokio::test]
async fn wrong_domain_is_rejected() {
    let auth = SiweAuth::new(DOMAIN.to_string(), CHAIN_ID);
    let signer = PrivateKeySigner::random();
    let address = signer.address().to_string();
    let issued_at = Utc::now().to_rfc3339();

    let link_nonce = auth.nonce();
    let link_msg = siwe_for(&address, &link_nonce, DOMAIN, CHAIN_ID, &issued_at);
    let link_sig = sign_message_hex(&signer, &link_msg).await;
    auth.link("maya", &link_msg, &link_sig).await.expect("link should succeed");

    let login_nonce = auth.nonce();
    let login_msg = siwe_for(&address, &login_nonce, "evil.example", CHAIN_ID, &issued_at);
    let login_sig = sign_message_hex(&signer, &login_msg).await;
    assert!(auth.verify_login(&login_msg, &login_sig).await.is_err(), "wrong domain must fail");
}

#[tokio::test]
async fn wrong_chain_id_is_rejected() {
    let auth = SiweAuth::new(DOMAIN.to_string(), CHAIN_ID);
    let signer = PrivateKeySigner::random();
    let address = signer.address().to_string();
    let issued_at = Utc::now().to_rfc3339();

    let link_nonce = auth.nonce();
    let link_msg = siwe_for(&address, &link_nonce, DOMAIN, CHAIN_ID, &issued_at);
    let link_sig = sign_message_hex(&signer, &link_msg).await;
    auth.link("maya", &link_msg, &link_sig).await.expect("link should succeed");

    let login_nonce = auth.nonce();
    let login_msg = siwe_for(&address, &login_nonce, DOMAIN, CHAIN_ID + 1, &issued_at);
    let login_sig = sign_message_hex(&signer, &login_msg).await;
    assert!(auth.verify_login(&login_msg, &login_sig).await.is_err(), "wrong chain id must fail");
}

#[tokio::test]
async fn signature_from_different_key_than_message_address_is_rejected() {
    let auth = SiweAuth::new(DOMAIN.to_string(), CHAIN_ID);
    let signer_a = PrivateKeySigner::random();
    let signer_b = PrivateKeySigner::random();
    let address_a = signer_a.address().to_string();
    let issued_at = Utc::now().to_rfc3339();

    let link_nonce = auth.nonce();
    let link_msg = siwe_for(&address_a, &link_nonce, DOMAIN, CHAIN_ID, &issued_at);
    let link_sig = sign_message_hex(&signer_a, &link_msg).await;
    auth.link("maya", &link_msg, &link_sig).await.expect("link should succeed");

    let login_nonce = auth.nonce();
    let login_msg = siwe_for(&address_a, &login_nonce, DOMAIN, CHAIN_ID, &issued_at);
    let wrong_sig = sign_message_hex(&signer_b, &login_msg).await;
    assert!(auth.verify_login(&login_msg, &wrong_sig).await.is_err(), "mismatched signer must fail");
}

#[tokio::test]
async fn login_with_unlinked_wallet_is_rejected() {
    let auth = SiweAuth::new(DOMAIN.to_string(), CHAIN_ID);
    let signer = PrivateKeySigner::random();
    let address = signer.address().to_string();
    let issued_at = Utc::now().to_rfc3339();

    let nonce = auth.nonce();
    let login_msg = siwe_for(&address, &nonce, DOMAIN, CHAIN_ID, &issued_at);
    let login_sig = sign_message_hex(&signer, &login_msg).await;
    assert!(auth.verify_login(&login_msg, &login_sig).await.is_err(), "unlinked wallet must fail");
}

#[tokio::test]
async fn linking_same_wallet_to_second_subject_moves_ownership() {
    let auth = SiweAuth::new(DOMAIN.to_string(), CHAIN_ID);
    let signer = PrivateKeySigner::random();
    let address = signer.address().to_string();
    let issued_at = Utc::now().to_rfc3339();

    let nonce_a = auth.nonce();
    let msg_a = siwe_for(&address, &nonce_a, DOMAIN, CHAIN_ID, &issued_at);
    let sig_a = sign_message_hex(&signer, &msg_a).await;
    auth.link("maya", &msg_a, &sig_a).await.expect("first link should succeed");

    let nonce_b = auth.nonce();
    let msg_b = siwe_for(&address, &nonce_b, DOMAIN, CHAIN_ID, &issued_at);
    let sig_b = sign_message_hex(&signer, &msg_b).await;
    auth.link("dev", &msg_b, &sig_b).await.expect("second link should succeed");

    assert_eq!(auth.wallet_of("dev").map(|v| v.to_lowercase()), Some(address.to_lowercase()));
    assert!(auth.wallet_of("maya").is_none(), "wallet should be moved away from first subject");

    let login_nonce = auth.nonce();
    let login_msg = siwe_for(&address, &login_nonce, DOMAIN, CHAIN_ID, &issued_at);
    let login_sig = sign_message_hex(&signer, &login_msg).await;
    let subject = auth.verify_login(&login_msg, &login_sig).await.expect("login should succeed");
    assert_eq!(subject, "dev");
}

#[test]
fn anchor_calldata_hides_subject_and_has_expected_prefix() {
    let subject = "maya";
    let head_hash = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    let data = ChainAnchorer::anchor_calldata(subject, head_hash);
    let text = std::str::from_utf8(&data).expect("anchor calldata should be UTF-8");
    assert!(text.starts_with("AEGIS1:"), "missing AEGIS1 prefix: {text}");
    assert!(text.contains(head_hash), "head hash missing from calldata: {text}");
    assert!(!text.contains(subject), "subject leaked into calldata: {text}");

    let parts: Vec<&str> = text.split(':').collect();
    assert_eq!(parts.len(), 3, "expected AEGIS1:<subject_hash>:<head_hash>");
    assert_eq!(parts[0], "AEGIS1");
    let expected_subject_hash = hex::encode(Sha256::digest(subject.as_bytes()));
    assert_eq!(parts[1], expected_subject_hash);
    assert_eq!(parts[2], head_hash);
}

