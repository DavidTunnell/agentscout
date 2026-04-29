use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{anyhow, Context, Result};
use hmac::Hmac;
use keyring::Entry;
use rand::RngCore;
use sha2::Sha256;
use std::path::Path;

// v0.5.16: DEK + install secret moved to crate::secrets which has the
// spawn_blocking + verify-after-write + encrypted-file-fallback pattern
// from v0.5.9. The KEYCHAIN_SERVICE / KEYCHAIN_USER_DEK / KEYCHAIN_USER_INSTALL_SECRET
// constants are gone — see secrets::ACCOUNT_FILE_ENCRYPTION_DEK and
// secrets::ACCOUNT_INSTALL_SECRET (same names, secrets module owns
// the values).
//
// Passphrase-related keys (wrapped DEK + salt) still live here because
// they're still spec'd as keyring-stored and the v1 passphrase path
// is opt-in / off by default. Migration to secrets module deferred
// until that path is exercised in real dogfood.
const KEYCHAIN_SERVICE: &str = "AgentScout";
const KEYCHAIN_USER_PASSPHRASE_SALT: &str = "passphrase-salt-v1";
const KEYCHAIN_USER_WRAPPED_DEK: &str = "wrapped-dek-v1";
const NONCE_LEN: usize = 12;
/// PBKDF2 iterations for passphrase derivation. SPEC.md §10.4 calls for
/// 600k SHA-256 rounds — same as 1Password's published v3 config.
pub const PBKDF2_ITERATIONS: u32 = 600_000;
const PBKDF2_SALT_LEN: usize = 16;

pub struct FileCrypto {
    cipher: Aes256Gcm,
}

/// Result of [`FileCrypto::load_or_init`]. Carries a flag indicating
/// whether the DEK was just freshly generated — bootstrap uses this to
/// detect "the user is upgrading from a pre-v0.5.16 install whose
/// silent-no-op DEK writes meant prior captures encrypted with
/// ephemeral keys" and clean up the stale `.enc` files.
pub struct FileCryptoInit {
    pub crypto: FileCrypto,
    pub fresh_dek: bool,
}

impl FileCrypto {
    /// Load the file-encryption DEK from `crate::secrets` (which routes
    /// through keychain-with-fallback) or generate + persist a fresh
    /// one if none is stored. Returns the cipher plus a `fresh_dek`
    /// flag for the caller to handle cleanup.
    pub async fn load_or_init() -> Result<FileCryptoInit> {
        let (key_bytes, fresh_dek) = match crate::secrets::get_file_encryption_dek().await? {
            Some(hex_key) => {
                let bytes = hex::decode(&hex_key).context("decoding DEK from secrets storage")?;
                (bytes, false)
            }
            None => {
                let mut key = [0u8; 32];
                rand::thread_rng().fill_bytes(&mut key);
                crate::secrets::set_file_encryption_dek(&hex::encode(key))
                    .await
                    .context("writing new DEK to secrets storage")?;
                (key.to_vec(), true)
            }
        };

        if key_bytes.len() != 32 {
            anyhow::bail!(
                "DEK in secrets storage has unexpected length {} (expected 32)",
                key_bytes.len()
            );
        }

        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes));
        Ok(FileCryptoInit {
            crypto: Self { cipher },
            fresh_dek,
        })
    }

    /// Construct a FileCrypto with an explicit 32-byte key. Bypasses the
    /// keychain entirely. **For tests and the smoke binary only** — never
    /// use this from production code paths.
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

/// Derive a 32-byte key from a passphrase using PBKDF2-HMAC-SHA256 at
/// the iteration count specified by SPEC.md §10.4. Salt should come
/// from `load_or_init_passphrase_salt`. Memory is zeroed before return.
pub fn derive_key_from_passphrase(passphrase: &str, salt: &[u8]) -> [u8; 32] {
    let mut key = [0u8; 32];
    pbkdf2::pbkdf2::<Hmac<Sha256>>(passphrase.as_bytes(), salt, PBKDF2_ITERATIONS, &mut key)
        .expect("PBKDF2 with valid params should not fail");
    key
}

pub fn load_or_init_passphrase_salt() -> Result<Vec<u8>> {
    let entry = Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER_PASSPHRASE_SALT)
        .context("creating keychain entry for passphrase salt")?;
    match entry.get_password() {
        Ok(hex_salt) => hex::decode(hex_salt).context("decoding passphrase salt"),
        Err(keyring::Error::NoEntry) => {
            let mut salt = [0u8; PBKDF2_SALT_LEN];
            rand::thread_rng().fill_bytes(&mut salt);
            entry
                .set_password(&hex::encode(salt))
                .context("writing new passphrase salt")?;
            Ok(salt.to_vec())
        }
        Err(e) => Err(anyhow!(e)).context("reading passphrase salt"),
    }
}

/// Wrap the file DEK with a passphrase-derived key so it can be stored
/// outside the keychain (e.g., on a machine without a working keyring).
/// Returns the wrapped (encrypted) DEK as a base64 string suitable for
/// storage. The wrapped form is itself an AES-GCM ciphertext.
pub fn wrap_dek_with_passphrase(dek: &[u8; 32], passphrase: &str) -> Result<String> {
    let salt = load_or_init_passphrase_salt()?;
    wrap_dek_with_salt(dek, passphrase, &salt)
}

/// Test-friendly variant that takes the salt explicitly rather than
/// pulling it from the keychain. Production code should use
/// [`wrap_dek_with_passphrase`].
pub fn wrap_dek_with_salt(dek: &[u8; 32], passphrase: &str, salt: &[u8]) -> Result<String> {
    let key = derive_key_from_passphrase(passphrase, salt);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, dek.as_ref())
        .map_err(|e| anyhow!("AES-GCM wrap failed: {e}"))?;
    let mut wrapped = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    wrapped.extend_from_slice(&nonce_bytes);
    wrapped.extend_from_slice(&ciphertext);
    Ok(hex::encode(&wrapped))
}

pub fn unwrap_dek_with_salt(wrapped_hex: &str, passphrase: &str, salt: &[u8]) -> Result<[u8; 32]> {
    let wrapped = hex::decode(wrapped_hex).context("decoding wrapped DEK")?;
    if wrapped.len() < NONCE_LEN + 16 {
        anyhow::bail!("wrapped DEK is truncated");
    }
    let key = derive_key_from_passphrase(passphrase, salt);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let (nonce_bytes, ciphertext) = wrapped.split_at(NONCE_LEN);
    let nonce = Nonce::from_slice(nonce_bytes);
    let dek_bytes = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow!("incorrect passphrase or tampered wrapped DEK"))?;
    if dek_bytes.len() != 32 {
        anyhow::bail!(
            "unwrapped DEK has unexpected length {} (expected 32)",
            dek_bytes.len()
        );
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&dek_bytes);
    Ok(out)
}

pub fn store_wrapped_dek(wrapped_hex: &str) -> Result<()> {
    let entry = Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER_WRAPPED_DEK)
        .context("creating keychain entry for wrapped DEK")?;
    entry
        .set_password(wrapped_hex)
        .context("writing wrapped DEK")
}

pub fn load_wrapped_dek() -> Result<Option<String>> {
    let entry = Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER_WRAPPED_DEK)
        .context("creating keychain entry for wrapped DEK")?;
    match entry.get_password() {
        Ok(s) => Ok(Some(s)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(anyhow!(e)).context("reading wrapped DEK"),
    }
}

/// Reverse of [`wrap_dek_with_passphrase`]. Returns Err on bad
/// passphrase (the GCM tag check fails), salt missing, or malformed
/// wrapped blob.
pub fn unwrap_dek_with_passphrase(wrapped_hex: &str, passphrase: &str) -> Result<[u8; 32]> {
    let wrapped = hex::decode(wrapped_hex).context("decoding wrapped DEK")?;
    if wrapped.len() < NONCE_LEN + 16 {
        anyhow::bail!("wrapped DEK is truncated");
    }
    let salt = load_or_init_passphrase_salt()?;
    let key = derive_key_from_passphrase(passphrase, &salt);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let (nonce_bytes, ciphertext) = wrapped.split_at(NONCE_LEN);
    let nonce = Nonce::from_slice(nonce_bytes);
    let dek_bytes = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow!("incorrect passphrase or tampered wrapped DEK"))?;
    if dek_bytes.len() != 32 {
        anyhow::bail!(
            "unwrapped DEK has unexpected length {} (expected 32)",
            dek_bytes.len()
        );
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&dek_bytes);
    Ok(out)
}

/// v0.5.16: install secret moved to `crate::secrets`. Same silent-no-op
/// fix pattern as the file DEK. The HMAC link signer needs this to be
/// stable across restarts so disposition links from yesterday's email
/// remain valid for 60 days.
pub async fn load_or_init_install_secret() -> Result<Vec<u8>> {
    let secret_bytes = match crate::secrets::get_install_secret().await? {
        Some(hex_secret) => {
            hex::decode(&hex_secret).context("decoding install secret from secrets storage")?
        }
        None => {
            let mut secret = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut secret);
            crate::secrets::set_install_secret(&hex::encode(secret))
                .await
                .context("writing new install secret to secrets storage")?;
            secret.to_vec()
        }
    };

    if secret_bytes.len() != 32 {
        anyhow::bail!(
            "install secret has unexpected length {} (expected 32)",
            secret_bytes.len()
        );
    }
    Ok(secret_bytes)
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

    #[test]
    fn pbkdf2_derives_deterministic_key_from_passphrase() {
        let salt = b"test-salt-1234567";
        let k1 = derive_key_from_passphrase("hunter2", salt);
        let k2 = derive_key_from_passphrase("hunter2", salt);
        assert_eq!(k1, k2);
        let k3 = derive_key_from_passphrase("hunter3", salt);
        assert_ne!(k1, k3);
    }

    #[test]
    fn pbkdf2_different_salts_yield_different_keys() {
        let k1 = derive_key_from_passphrase("hunter2", b"salt-a");
        let k2 = derive_key_from_passphrase("hunter2", b"salt-b");
        assert_ne!(k1, k2);
    }

    #[test]
    fn dek_wrap_unwrap_roundtrip() {
        let dek = [0xCDu8; 32];
        let salt = b"test-salt-1234567";
        let wrapped = wrap_dek_with_salt(&dek, "correct horse", salt).unwrap();
        let unwrapped = unwrap_dek_with_salt(&wrapped, "correct horse", salt).unwrap();
        assert_eq!(unwrapped, dek);
    }

    #[test]
    fn unwrap_with_wrong_passphrase_fails() {
        let dek = [0xABu8; 32];
        let salt = b"test-salt-7654321";
        let wrapped = wrap_dek_with_salt(&dek, "right", salt).unwrap();
        let result = unwrap_dek_with_salt(&wrapped, "wrong", salt);
        assert!(result.is_err());
    }

    #[test]
    fn unwrap_with_wrong_salt_fails() {
        let dek = [0x77u8; 32];
        let wrapped = wrap_dek_with_salt(&dek, "passphrase", b"salt-a").unwrap();
        let result = unwrap_dek_with_salt(&wrapped, "passphrase", b"salt-b");
        assert!(result.is_err());
    }

    #[test]
    fn truncated_wrapped_dek_fails_cleanly() {
        let result = unwrap_dek_with_salt("aabbcc", "any", b"salt");
        assert!(result.is_err());
    }
}
