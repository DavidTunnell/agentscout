use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{anyhow, Context, Result};
use keyring::Entry;
use rand::RngCore;
use std::path::Path;

const KEYCHAIN_SERVICE: &str = "AgentScout";
const KEYCHAIN_USER_DEK: &str = "file-dek-v1";
const KEYCHAIN_USER_INSTALL_SECRET: &str = "install-secret-v1";
const NONCE_LEN: usize = 12;

pub struct FileCrypto {
    cipher: Aes256Gcm,
}

impl FileCrypto {
    pub fn load_or_init() -> Result<Self> {
        let entry = Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER_DEK)
            .context("creating keychain entry for DEK")?;

        let key_bytes = match entry.get_password() {
            Ok(hex_key) => hex::decode(hex_key).context("decoding DEK from keychain")?,
            Err(keyring::Error::NoEntry) => {
                let mut key = [0u8; 32];
                rand::thread_rng().fill_bytes(&mut key);
                entry
                    .set_password(&hex::encode(key))
                    .context("writing new DEK to keychain")?;
                key.to_vec()
            }
            Err(e) => return Err(anyhow!(e)).context("reading DEK from keychain"),
        };

        if key_bytes.len() != 32 {
            anyhow::bail!(
                "DEK in keychain has unexpected length {} (expected 32)",
                key_bytes.len()
            );
        }

        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes));
        Ok(Self { cipher })
    }

    #[cfg(test)]
    pub fn with_key(key: [u8; 32]) -> Self {
        Self {
            cipher: Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key)),
        }
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| anyhow!("AES-GCM encrypt failed: {e}"))?;

        let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    pub fn decrypt(&self, blob: &[u8]) -> Result<Vec<u8>> {
        if blob.len() < NONCE_LEN + 16 {
            anyhow::bail!("ciphertext too short: {} bytes", blob.len());
        }
        let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);
        let nonce = Nonce::from_slice(nonce_bytes);
        self.cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow!("AES-GCM decrypt failed (tampered or wrong key?): {e}"))
    }

    pub fn encrypt_to_file(&self, path: &Path, plaintext: &[u8]) -> Result<()> {
        let blob = self.encrypt(plaintext)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, blob)
            .with_context(|| format!("writing encrypted blob to {}", path.display()))?;
        Ok(())
    }

    pub fn decrypt_from_file(&self, path: &Path) -> Result<Vec<u8>> {
        let blob = std::fs::read(path)
            .with_context(|| format!("reading encrypted blob from {}", path.display()))?;
        self.decrypt(&blob)
    }
}

pub fn load_or_init_install_secret() -> Result<Vec<u8>> {
    let entry = Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER_INSTALL_SECRET)
        .context("creating keychain entry for install secret")?;
    match entry.get_password() {
        Ok(hex_secret) => {
            hex::decode(hex_secret).context("decoding install secret from keychain")
        }
        Err(keyring::Error::NoEntry) => {
            let mut secret = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut secret);
            entry
                .set_password(&hex::encode(secret))
                .context("writing new install secret to keychain")?;
            Ok(secret.to_vec())
        }
        Err(e) => Err(anyhow!(e)).context("reading install secret from keychain"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_small_payload() {
        let fc = FileCrypto::with_key([7u8; 32]);
        let msg = b"hello world";
        let blob = fc.encrypt(msg).unwrap();
        assert!(blob.len() >= NONCE_LEN + msg.len() + 16);
        let back = fc.decrypt(&blob).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn tampered_ciphertext_fails_decrypt() {
        let fc = FileCrypto::with_key([3u8; 32]);
        let mut blob = fc.encrypt(b"secret").unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0xFF;
        assert!(fc.decrypt(&blob).is_err());
    }

    #[test]
    fn different_nonces_produce_different_ciphertexts() {
        let fc = FileCrypto::with_key([1u8; 32]);
        let a = fc.encrypt(b"same input").unwrap();
        let b = fc.encrypt(b"same input").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn file_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("as-test-{}.enc", uuid::Uuid::new_v4()));
        let fc = FileCrypto::with_key([42u8; 32]);
        fc.encrypt_to_file(&tmp, b"some payload").unwrap();
        let back = fc.decrypt_from_file(&tmp).unwrap();
        assert_eq!(back, b"some payload");
        std::fs::remove_file(&tmp).ok();
    }
}
