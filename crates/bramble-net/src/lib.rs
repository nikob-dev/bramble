//! Trust between nodes: signing envelopes, and deciding whether to believe one.
//!
//! A receiver accepts an envelope only if, in this order:
//! 1. its major version is one we speak,
//! 2. its sender is a peer whose public key we hold,
//! 3. its timestamp is within [`MAX_SKEW_SECS`] of our clock,
//! 4. its Ed25519 signature verifies over [`Envelope::signing_bytes`],
//! 5. its nonce hasn't been seen from that sender inside the replay window,
//! 6. its payload parses as a known message.
//!
//! The nonce is only remembered after the signature checks out, so forged envelopes
//! can't fill the replay cache. Transport (HTTP) lives in the node; this crate does no IO.

use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bramble_core::net::{Envelope, NET_VERSION, NetError, NetMsg};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

/// How far a sender's clock may drift from ours. Containers on one host share a clock,
/// so this is generous; it bounds how long nonces must be remembered.
pub const MAX_SKEW_SECS: i64 = 120;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Rejection {
    #[error("protocol version {got} (we speak {NET_VERSION})")]
    UnsupportedVersion { got: u16 },
    #[error("unknown sender {0:?}")]
    UnknownSender(String),
    #[error("timestamp is {skew_secs}s from our clock (limit {MAX_SKEW_SECS}s)")]
    Stale { skew_secs: i64 },
    #[error("signature does not verify")]
    BadSignature,
    #[error("nonce {nonce:?} from {from:?} was already used")]
    Replay { from: String, nonce: String },
    #[error(transparent)]
    Message(#[from] NetError),
}

/// Wraps and signs a message from this node.
pub fn seal(msg: &NetMsg, from: &str, key: &SigningKey, now_unix: i64) -> Envelope {
    let mut envelope = Envelope::new(msg, from, now_unix, &new_nonce());
    envelope.sig = STANDARD.encode(key.sign(&envelope.signing_bytes()).to_bytes());
    envelope
}

/// 128 random bits, hex: collisions are not a practical concern.
pub fn new_nonce() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS randomness is available");
    hex::encode(bytes)
}

/// Checks incoming envelopes against known peers and remembers nonces it has accepted.
pub struct Verifier {
    peers: HashMap<String, VerifyingKey>,
    /// `(sender, nonce)` → timestamp, for envelopes accepted inside the window.
    seen: HashMap<(String, String), i64>,
}

impl Verifier {
    pub fn new(peers: HashMap<String, VerifyingKey>) -> Self {
        Self {
            peers,
            seen: HashMap::new(),
        }
    }

    pub fn add_peer(&mut self, id: &str, key: VerifyingKey) {
        self.peers.insert(id.to_owned(), key);
    }

    pub fn knows(&self, id: &str) -> bool {
        self.peers.contains_key(id)
    }

    pub fn verify(&mut self, envelope: &Envelope, now_unix: i64) -> Result<NetMsg, Rejection> {
        if envelope.v != NET_VERSION {
            return Err(Rejection::UnsupportedVersion { got: envelope.v });
        }
        let key = self
            .peers
            .get(&envelope.from)
            .ok_or_else(|| Rejection::UnknownSender(envelope.from.clone()))?;
        let skew_secs = envelope.ts - now_unix;
        if skew_secs.abs() > MAX_SKEW_SECS {
            return Err(Rejection::Stale { skew_secs });
        }
        let signature = STANDARD
            .decode(&envelope.sig)
            .ok()
            .and_then(|bytes| Signature::from_slice(&bytes).ok())
            .ok_or(Rejection::BadSignature)?;
        key.verify_strict(&envelope.signing_bytes(), &signature)
            .map_err(|_| Rejection::BadSignature)?;

        self.forget_expired(now_unix);
        let id = (envelope.from.clone(), envelope.nonce.clone());
        if self.seen.contains_key(&id) {
            return Err(Rejection::Replay {
                from: id.0,
                nonce: id.1,
            });
        }
        let msg = envelope.message()?;
        self.seen.insert(id, envelope.ts);
        Ok(msg)
    }

    /// An envelope older than the skew limit is rejected as stale before its nonce is
    /// checked, so its nonce no longer needs remembering.
    fn forget_expired(&mut self, now_unix: i64) {
        self.seen.retain(|_, ts| now_unix - *ts <= MAX_SKEW_SECS);
    }

    #[cfg(test)]
    fn remembered(&self) -> usize {
        self.seen.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_760_000_000;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn msg() -> NetMsg {
        NetMsg::RtpStatus {
            msg_id: "pet-a-0001".into(),
            accepted: true,
            reason: None,
        }
    }

    fn verifier() -> Verifier {
        Verifier::new(HashMap::from([
            ("pet-a".to_owned(), key(1).verifying_key()),
            ("pet-b".to_owned(), key(2).verifying_key()),
        ]))
    }

    #[test]
    fn net_verify_accepts_a_sealed_envelope() {
        let envelope = seal(&msg(), "pet-a", &key(1), NOW);
        assert_eq!(verifier().verify(&envelope, NOW), Ok(msg()));
    }

    #[test]
    fn net_verify_rejects_tampered_payload() {
        let mut envelope = seal(&msg(), "pet-a", &key(1), NOW);
        envelope.payload = envelope.payload.replace("true", "false");
        assert_eq!(
            verifier().verify(&envelope, NOW),
            Err(Rejection::BadSignature)
        );
    }

    #[test]
    fn net_verify_rejects_impersonation() {
        // pet-b signs with its own key but claims to be pet-a.
        let envelope = seal(&msg(), "pet-a", &key(2), NOW);
        assert_eq!(
            verifier().verify(&envelope, NOW),
            Err(Rejection::BadSignature)
        );
    }

    #[test]
    fn net_verify_rejects_garbage_signature() {
        let mut envelope = seal(&msg(), "pet-a", &key(1), NOW);
        envelope.sig = "not base64!".into();
        assert_eq!(
            verifier().verify(&envelope, NOW),
            Err(Rejection::BadSignature)
        );
    }

    #[test]
    fn net_verify_rejects_unknown_sender() {
        let envelope = seal(&msg(), "pet-z", &key(9), NOW);
        assert_eq!(
            verifier().verify(&envelope, NOW),
            Err(Rejection::UnknownSender("pet-z".into()))
        );
    }

    #[test]
    fn net_verify_rejects_a_replay() {
        let envelope = seal(&msg(), "pet-a", &key(1), NOW);
        let mut verifier = verifier();
        assert!(verifier.verify(&envelope, NOW).is_ok());
        assert!(matches!(
            verifier.verify(&envelope, NOW + 1),
            Err(Rejection::Replay { .. })
        ));
    }

    #[test]
    fn net_verify_rejects_stale_and_future_timestamps() {
        let mut verifier = verifier();
        let old = seal(&msg(), "pet-a", &key(1), NOW - MAX_SKEW_SECS - 1);
        assert!(matches!(
            verifier.verify(&old, NOW),
            Err(Rejection::Stale { .. })
        ));
        let future = seal(&msg(), "pet-a", &key(1), NOW + MAX_SKEW_SECS + 1);
        assert!(matches!(
            verifier.verify(&future, NOW),
            Err(Rejection::Stale { .. })
        ));
        let edge = seal(&msg(), "pet-a", &key(1), NOW - MAX_SKEW_SECS);
        assert!(verifier.verify(&edge, NOW).is_ok());
    }

    #[test]
    fn net_verify_rejects_other_major_versions() {
        let mut envelope = seal(&msg(), "pet-a", &key(1), NOW);
        envelope.v = NET_VERSION + 1;
        assert_eq!(
            verifier().verify(&envelope, NOW),
            Err(Rejection::UnsupportedVersion {
                got: NET_VERSION + 1
            })
        );
    }

    #[test]
    fn net_verify_forged_envelopes_do_not_poison_the_nonce_cache() {
        let genuine = seal(&msg(), "pet-a", &key(1), NOW);
        let mut forged = genuine.clone();
        forged.sig = seal(&msg(), "pet-a", &key(2), NOW).sig;
        let mut verifier = verifier();
        assert_eq!(verifier.verify(&forged, NOW), Err(Rejection::BadSignature));
        assert!(verifier.verify(&genuine, NOW).is_ok(), "nonce still free");
    }

    #[test]
    fn net_verify_forgets_nonces_once_they_would_be_stale() {
        let mut verifier = verifier();
        for i in 0..3 {
            let envelope = seal(&msg(), "pet-a", &key(1), NOW + i);
            verifier.verify(&envelope, NOW + i).expect("accepted");
        }
        assert_eq!(verifier.remembered(), 3);
        let later = NOW + MAX_SKEW_SECS + 10;
        let envelope = seal(&msg(), "pet-b", &key(2), later);
        verifier.verify(&envelope, later).expect("accepted");
        assert_eq!(verifier.remembered(), 1, "only the fresh nonce remains");
    }

    #[test]
    fn net_verify_rejects_unknown_kind_after_checking_the_signature() {
        let mut envelope = Envelope::new(&msg(), "pet-a", NOW, &new_nonce());
        envelope.kind = "card_authorization".into();
        envelope.sig = STANDARD.encode(key(1).sign(&envelope.signing_bytes()).to_bytes());
        assert_eq!(
            verifier().verify(&envelope, NOW),
            Err(Rejection::Message(NetError::UnknownKind(
                "card_authorization".into()
            )))
        );
    }

    #[test]
    fn net_new_nonce_is_unique_hex() {
        let a = new_nonce();
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, new_nonce());
    }
}
