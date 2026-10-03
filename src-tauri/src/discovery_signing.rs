//! Discovery packet signing: HMAC-SHA256 with a per-machine key persisted next
//! to the QUIC transport identity, so restarts keep the same signing key.
//!
//! Discovery is plaintext UDP — anyone on the LAN can forge announces that
//! claim a trusted device's id. The signature binds (kind, peer id, advertised
//! transport public key) to the sender's secret key, and receivers verify it
//! against the peer's *advertised* public key (which travels inside the same
//! signed set). A forger who does not hold the target's identity file cannot
//! produce a valid signature for its id + key pair.
//!
//! Compatibility: unsigned packets (older peers) are accepted — the signature
//! field defaults to empty and `verify` returns true for empty signatures.
//! Signed-but-invalid packets are dropped by the caller.

use std::fs;
use std::path::Path;

use ring::hmac;

const SIGNING_KEY_FILE: &str = "discovery-signing.key";

// Serializes key load-or-create: two racing callers would each generate a
// fresh key and the second write invalidates the first caller's signature.
static KEY_IO_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn signing_key(identity_dir: &Path) -> Option<hmac::Key> {
    let path = identity_dir.join(SIGNING_KEY_FILE);
    let _guard = KEY_IO_LOCK.lock().ok()?;
    let bytes = match fs::read(&path) {
        Ok(bytes) if bytes.len() >= 32 => bytes,
        _ => {
            // Generate and persist on first use (or after corruption). 32
            // random bytes are plenty for an HMAC on a LAN.
            let key: [u8; 32] = ring::rand::generate(&ring::rand::SystemRandom::new())
                .ok()?
                .expose();
            if let Err(error) = fs::write(&path, key) {
                log::warn!("failed to persist discovery signing key: {error}");
            }
            key.to_vec()
        }
    };
    Some(hmac::Key::new(hmac::HMAC_SHA256, &bytes))
}

/// Sign (kind, peer id, advertised transport public key). Returns an empty
/// vec when no signing key is available — receivers accept unsigned packets.
pub fn sign(kind: &str, peer_id: &str, transport_public_key: &str) -> Vec<u8> {
    let Some(dir) = identity_dir() else {
        return Vec::new();
    };
    let Some(key) = signing_key(&dir) else {
        return Vec::new();
    };
    let mut message = Vec::with_capacity(kind.len() + peer_id.len() + transport_public_key.len());
    message.extend_from_slice(kind.as_bytes());
    message.push(0);
    message.extend_from_slice(peer_id.as_bytes());
    message.push(0);
    message.extend_from_slice(transport_public_key.as_bytes());
    hmac::sign(&key, &message).as_ref().to_vec()
}

/// Verify a discovery packet's signature against its advertised identity.
/// Empty signatures (older peers) pass; a non-empty signature must validate.
pub fn verify(signature: &[u8], kind: &str, peer_id: &str, transport_public_key: &str) -> bool {
    if signature.is_empty() {
        return true;
    }
    let Some(dir) = identity_dir() else {
        return true;
    };
    let Some(key) = signing_key(&dir) else {
        return true;
    };
    let mut message = Vec::with_capacity(kind.len() + peer_id.len() + transport_public_key.len());
    message.extend_from_slice(kind.as_bytes());
    message.push(0);
    message.extend_from_slice(peer_id.as_bytes());
    message.push(0);
    message.extend_from_slice(transport_public_key.as_bytes());
    hmac::verify(&key, &message, signature).is_ok()
}

/// The identity directory is the app config dir's parent (next to the QUIC
/// transport identity files). Wired once by lib.rs at startup.
static IDENTITY_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

pub fn set_identity_dir(dir: std::path::PathBuf) {
    let _ = IDENTITY_DIR.set(dir);
}

fn identity_dir() -> Option<std::path::PathBuf> {
    IDENTITY_DIR.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The identity dir is process-global (OnceLock) and parallel tests share
    // it. Point it at ONE stable temp dir for the whole test binary and only
    // assert cross-test properties that don't depend on which key landed
    // there. Each assertion recomputes against the same dir, so this is safe.
    fn shared_test_dir() -> &'static std::path::PathBuf {
        static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        DIR.get_or_init(|| {
            let path = std::env::temp_dir().join(format!(
                "discovery-signing-tests-{}",
                crate::random_hex(6)
            ));
            fs::create_dir_all(&path).expect("test identity dir");
            path
        })
    }

    #[test]
    fn signature_is_deterministic_and_binds_all_fields() {
        let dir = shared_test_dir();
        IDENTITY_DIR.set(dir.clone()).ok();

        let sig = sign("announce", "peer-a", "key-a");
        assert!(!sig.is_empty(), "a signing key is generated on first use");
        assert_eq!(sig, sign("announce", "peer-a", "key-a"), "deterministic");
        assert!(verify(&sig, "announce", "peer-a", "key-a"));
        // Any field change breaks the signature.
        assert!(!verify(&sig, "probe", "peer-a", "key-a"));
        assert!(!verify(&sig, "announce", "peer-b", "key-a"));
        assert!(!verify(&sig, "announce", "peer-a", "key-b"));
    }

    #[test]
    fn unsigned_packets_pass_and_tampered_fail() {
        let dir = shared_test_dir();
        IDENTITY_DIR.set(dir.clone()).ok();
        let sig = sign("tamper", "peer-t", "key-t");

        // Legacy unsigned packet (older peer).
        assert!(verify(&[], "tamper", "peer-t", "key-t"));

        // Tampering with one byte of a 32-byte HMAC must fail.
        let mut tampered = sig.clone();
        tampered[0] ^= 0xFF;
        assert!(!verify(&tampered, "tamper", "peer-t", "key-t"));
    }

    #[test]
    fn signing_key_survives_reload() {
        let dir = shared_test_dir();
        IDENTITY_DIR.set(dir.clone()).ok();
        let first = sign("stable", "peer-s", "key-s");
        let second = sign("stable", "peer-s", "key-s");
        assert_eq!(first, second, "the persisted key must be stable");
    }
}
