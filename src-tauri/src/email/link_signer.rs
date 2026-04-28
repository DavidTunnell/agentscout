//! HMAC-signed disposition links — see plan pinned decision #1.
//!
//! Per the build plan we use a stable per-install secret (already in
//! the keychain via `storage::crypto::load_or_init_install_secret`)
//! rather than rotating session tokens, so emailed links survive an
//! app restart. Default soft expiry is 60 days, matching the archive
//! retention window plus a buffer.

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

const DEFAULT_EXPIRY_DAYS: i64 = 60;
const SIGNATURE_LEN_BYTES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispositionAction {
    Implemented,
    NotInterested,
    MaybeLater,
}

impl DispositionAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            DispositionAction::Implemented => "implemented",
            DispositionAction::NotInterested => "not_interested",
            DispositionAction::MaybeLater => "maybe_later",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "implemented" => Some(Self::Implemented),
            "not_interested" => Some(Self::NotInterested),
            "maybe_later" => Some(Self::MaybeLater),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedToken {
    pub rec_id: String,
    pub action: DispositionAction,
    pub issued_at: i64,
    pub expires_at: i64,
    pub signature: String,
}

pub struct LinkSigner {
    secret: Vec<u8>,
    expiry_days: i64,
}

impl LinkSigner {
    pub fn new(secret: Vec<u8>) -> Self {
        Self {
            secret,
            expiry_days: DEFAULT_EXPIRY_DAYS,
        }
    }

    pub fn with_expiry_days(mut self, days: i64) -> Self {
        self.expiry_days = days;
        self
    }

    pub fn sign(&self, rec_id: &str, action: DispositionAction) -> SignedToken {
        let now = Utc::now().timestamp();
        let expires_at = now + self.expiry_days * 86_400;
        let signature = self.compute_signature(rec_id, action, now, expires_at);
        SignedToken {
            rec_id: rec_id.to_string(),
            action,
            issued_at: now,
            expires_at,
            signature,
        }
    }

    /// Build the URL query string carrying a signed token. Designed to
    /// be appended to `http://127.0.0.1:<port>/disposition`.
    pub fn build_query(&self, rec_id: &str, action: DispositionAction) -> String {
        let token = self.sign(rec_id, action);
        format!(
            "?rec={}&action={}&issued={}&exp={}&sig={}",
            urlencoding::encode(&token.rec_id),
            token.action.as_str(),
            token.issued_at,
            token.expires_at,
            urlencoding::encode(&token.signature)
        )
    }

    pub fn verify(
        &self,
        rec_id: &str,
        action: DispositionAction,
        issued_at: i64,
        expires_at: i64,
        signature: &str,
    ) -> Result<()> {
        if Utc::now().timestamp() > expires_at {
            bail!("link expired");
        }
        if expires_at - issued_at > self.expiry_days * 86_400 + 86_400 {
            bail!("link claims a longer lifetime than the signer permits");
        }
        let expected = self.compute_signature(rec_id, action, issued_at, expires_at);
        if !constant_time_eq(expected.as_bytes(), signature.as_bytes()) {
            bail!("invalid signature");
        }
        Ok(())
    }

    fn compute_signature(
        &self,
        rec_id: &str,
        action: DispositionAction,
        issued_at: i64,
        expires_at: i64,
    ) -> String {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.secret).expect("HMAC accepts any key length");
        mac.update(rec_id.as_bytes());
        mac.update(b"|");
        mac.update(action.as_str().as_bytes());
        mac.update(b"|");
        mac.update(issued_at.to_be_bytes().as_ref());
        mac.update(b"|");
        mac.update(expires_at.to_be_bytes().as_ref());
        let digest = mac.finalize().into_bytes();
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&digest[..SIGNATURE_LEN_BYTES])
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Parse a query string like `rec=...&action=...&issued=...&exp=...&sig=...`
/// into a SignedToken. Caller must still pass the result to [`LinkSigner::verify`].
pub fn parse_token_from_query(query: &str) -> Result<SignedToken> {
    let mut rec_id = None;
    let mut action = None;
    let mut issued_at = None;
    let mut expires_at = None;
    let mut signature = None;

    for pair in query.split('&') {
        let (k, v) = pair
            .split_once('=')
            .ok_or_else(|| anyhow!("malformed query pair: {pair}"))?;
        let v = urlencoding::decode(v)
            .with_context(|| format!("decoding query value for {k}"))?
            .into_owned();
        match k {
            "rec" => rec_id = Some(v),
            "action" => action = DispositionAction::parse(&v),
            "issued" => issued_at = v.parse().ok(),
            "exp" => expires_at = v.parse().ok(),
            "sig" => signature = Some(v),
            _ => {}
        }
    }

    Ok(SignedToken {
        rec_id: rec_id.ok_or_else(|| anyhow!("missing rec"))?,
        action: action.ok_or_else(|| anyhow!("missing or unknown action"))?,
        issued_at: issued_at.ok_or_else(|| anyhow!("missing issued"))?,
        expires_at: expires_at.ok_or_else(|| anyhow!("missing exp"))?,
        signature: signature.ok_or_else(|| anyhow!("missing sig"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> LinkSigner {
        LinkSigner::new(vec![0x42; 32])
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let s = signer();
        let token = s.sign("rec-abc", DispositionAction::Implemented);
        s.verify(
            &token.rec_id,
            token.action,
            token.issued_at,
            token.expires_at,
            &token.signature,
        )
        .unwrap();
    }

    #[test]
    fn tampered_rec_id_fails_verification() {
        let s = signer();
        let token = s.sign("rec-abc", DispositionAction::NotInterested);
        let result = s.verify(
            "rec-different",
            token.action,
            token.issued_at,
            token.expires_at,
            &token.signature,
        );
        assert!(result.is_err());
    }

    #[test]
    fn tampered_action_fails_verification() {
        let s = signer();
        let token = s.sign("rec-abc", DispositionAction::NotInterested);
        let result = s.verify(
            &token.rec_id,
            DispositionAction::Implemented,
            token.issued_at,
            token.expires_at,
            &token.signature,
        );
        assert!(result.is_err());
    }

    #[test]
    fn expired_link_fails_verification() {
        let s = signer();
        let now = Utc::now().timestamp();
        let issued_at = now - 100 * 86_400;
        let expires_at = now - 86_400;
        let sig = s.compute_signature(
            "rec-x",
            DispositionAction::MaybeLater,
            issued_at,
            expires_at,
        );
        let result = s.verify(
            "rec-x",
            DispositionAction::MaybeLater,
            issued_at,
            expires_at,
            &sig,
        );
        assert!(result.is_err());
    }

    #[test]
    fn link_lifetime_longer_than_signer_permits_is_rejected() {
        let s = LinkSigner::new(vec![1; 32]).with_expiry_days(7);
        let now = Utc::now().timestamp();
        // 90-day claimed lifetime against a 7-day signer
        let issued_at = now;
        let expires_at = now + 90 * 86_400;
        let sig = s.compute_signature(
            "rec-y",
            DispositionAction::Implemented,
            issued_at,
            expires_at,
        );
        let result = s.verify(
            "rec-y",
            DispositionAction::Implemented,
            issued_at,
            expires_at,
            &sig,
        );
        assert!(result.is_err());
    }

    #[test]
    fn different_secrets_produce_different_signatures() {
        let a = LinkSigner::new(vec![1; 32]).sign("r", DispositionAction::Implemented);
        let b = LinkSigner::new(vec![2; 32]).sign("r", DispositionAction::Implemented);
        assert_ne!(a.signature, b.signature);
    }

    #[test]
    fn build_query_round_trips_via_parse() {
        let s = signer();
        let q = s.build_query("rec-roundtrip", DispositionAction::MaybeLater);
        let q_no_prefix = q.trim_start_matches('?');
        let token = parse_token_from_query(q_no_prefix).unwrap();
        assert_eq!(token.rec_id, "rec-roundtrip");
        assert_eq!(token.action, DispositionAction::MaybeLater);
        s.verify(
            &token.rec_id,
            token.action,
            token.issued_at,
            token.expires_at,
            &token.signature,
        )
        .unwrap();
    }

    #[test]
    fn parse_rejects_missing_fields() {
        assert!(parse_token_from_query("rec=abc").is_err());
        assert!(parse_token_from_query("").is_err());
    }

    #[test]
    fn action_parse_rejects_unknown() {
        assert!(DispositionAction::parse("delete_forever").is_none());
    }

    #[test]
    fn constant_time_eq_handles_unequal_lengths() {
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
    }
}
