//! The pet-bank's Ed25519 identity: signs published files today, network messages in M4.

use anyhow::Context;
use ed25519_dalek::SigningKey;
use ed25519_dalek::pkcs8::EncodePublicKey;
use ed25519_dalek::pkcs8::spki::der::pem::LineEnding;

use super::store::PetStore;

const KEY_DOC: &str = "signing_key";

/// Loads the node's signing key, generating and persisting one on first start.
pub fn load_or_create(store: &PetStore) -> anyhow::Result<SigningKey> {
    if let Some(encoded) = store.load::<String>(KEY_DOC)? {
        let bytes: [u8; 32] = hex::decode(&encoded)
            .context("stored signing key is not hex")?
            .try_into()
            .map_err(|_| anyhow::anyhow!("stored signing key is not 32 bytes"))?;
        return Ok(SigningKey::from_bytes(&bytes));
    }
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| anyhow::anyhow!("no OS randomness: {e}"))?;
    store.save(KEY_DOC, &hex::encode(seed))?;
    tracing::info!("generated a new node signing key");
    Ok(SigningKey::from_bytes(&seed))
}

/// SubjectPublicKeyInfo PEM, the format `openssl pkeyutl -pubin` expects.
pub fn public_key_pem(key: &SigningKey) -> anyhow::Result<String> {
    key.verifying_key()
        .to_public_key_pem(LineEnding::LF)
        .context("encoding public key as PEM")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_load_or_create_is_stable_across_restarts() {
        let store = PetStore::open_in_memory().expect("store");
        let first = load_or_create(&store).expect("create");
        let second = load_or_create(&store).expect("load");
        assert_eq!(first.to_bytes(), second.to_bytes());
        let pem = public_key_pem(&first).expect("pem");
        assert!(pem.starts_with("-----BEGIN PUBLIC KEY-----"), "{pem}");
    }
}
