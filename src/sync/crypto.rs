//! End-to-end encryption for sync, deliberately compatible with the pastebin's
//! own browser client so a synced document can also be read at
//! `https://<endpoint>/d/~ep-<id>#<key>` without this tool.
//!
//! # What the server sees
//!
//! Two values are derived from the user's single passphrase, with HKDF domain
//! separation:
//!
//! - `k_enc` — AES-256-GCM key. **Never leaves this machine.**
//! - `k_pw`  — the paste's management password. This one *is* sent to the
//!   server, in the URL path of `PUT`/`DELETE`.
//!
//! Because they are independent HKDF outputs, learning `k_pw` does not reveal
//! `k_enc`. That is what lets a user configure only a sync id and a passphrase
//! while the server still never holds anything it can decrypt.
//!
//! # Wire format
//!
//! `base64variant(iv[12] || AES-256-GCM(k_enc, iv, plaintext))`, where
//! `base64variant` is standard base64 with `/` replaced by `_` and padding
//! stripped — byte-for-byte what `frontend/utils/encryption.ts` produces.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Result, bail};
use base64::Engine;
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

/// Prefix on the plaintext, so a successful decryption with the wrong
/// passphrase (or a paste that isn't ours at all) produces a precise message
/// instead of a confusing JSON parse error.
pub const MAGIC: &[u8] = b"ENVPICK1\n";

const INFO_ENC: &[u8] = b"envpick/v1/encryption";
const INFO_PW: &[u8] = b"envpick/v1/manage-password";
const IV_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// Keys derived from the user's passphrase. Zeroized on drop.
pub struct SyncCrypto {
    enc: [u8; 32],
    manage_password: String,
    browser_key: String,
}

impl Drop for SyncCrypto {
    fn drop(&mut self) {
        self.enc.zeroize();
        self.manage_password.zeroize();
        self.browser_key.zeroize();
    }
}

impl SyncCrypto {
    /// Derive from a passphrase and the sync id.
    ///
    /// The sync id is the HKDF salt, which binds the keys to it: the same
    /// passphrase under a different sync id yields unrelated keys, so a user
    /// can reuse one memorable passphrase across separate syncs safely.
    pub fn derive(passphrase: &str, sync_id: &str) -> Result<Self> {
        if passphrase.is_empty() {
            bail!(crate::text::errors::SYNC_KEY_MISSING);
        }
        crate::config::validate_sync_id(sync_id)?;

        let hk = Hkdf::<Sha256>::new(Some(sync_id.as_bytes()), passphrase.as_bytes());

        let mut enc = [0u8; 32];
        hk.expand(INFO_ENC, &mut enc)
            .map_err(|_| anyhow::anyhow!("HKDF 派生加密密钥失败"))?;

        let mut pw = [0u8; 32];
        hk.expand(INFO_PW, &mut pw)
            .map_err(|_| anyhow::anyhow!("HKDF 派生管理密码失败"))?;

        // base64url, no padding: 43 chars of `[A-Za-z0-9_-]`. Satisfies the
        // pastebin's 8..=128 length rule and needs no escaping in a URL path.
        let manage_password = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(pw);
        pw.zeroize();

        let browser_key = b64variant_encode(&enc);

        Ok(Self {
            enc,
            manage_password,
            browser_key,
        })
    }

    /// Sent to the server in the `PUT`/`DELETE` URL. Not secret from the server.
    pub fn manage_password(&self) -> &str {
        &self.manage_password
    }

    /// The AES key, re-encoded the way the pastebin's web UI expects it in the
    /// URL fragment. Still secret — it is the encryption key.
    pub fn browser_key(&self) -> &str {
        &self.browser_key
    }

    /// Encrypt, prepending the [`MAGIC`] marker. Owning the marker here — rather
    /// than at every call site — is what lets [`decrypt`](Self::decrypt) treat
    /// "our payload" and "some other ciphertext" as different outcomes.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<String> {
        let cipher =
            Aes256Gcm::new_from_slice(&self.enc).map_err(|_| anyhow::anyhow!("初始化 AES 失败"))?;
        let mut iv = [0u8; IV_LEN];
        rand::fill(&mut iv);
        let nonce = Nonce::from(iv);

        let mut payload = Vec::with_capacity(MAGIC.len() + plaintext.len());
        payload.extend_from_slice(MAGIC);
        payload.extend_from_slice(plaintext);

        let mut ct = cipher
            .encrypt(&nonce, payload.as_slice())
            .map_err(|_| anyhow::anyhow!("加密失败"))?;

        let mut wire = Vec::with_capacity(IV_LEN + ct.len());
        wire.extend_from_slice(&iv);
        wire.append(&mut ct);
        Ok(b64variant_encode(&wire))
    }

    /// Decrypt and verify the [`MAGIC`] marker, returning the payload with the
    /// marker removed. The three failure modes are kept distinct so the user
    /// gets an actionable message: unparseable input and a missing marker both
    /// mean "not our data", while a GCM authentication failure means the key is
    /// wrong or the paste was altered.
    pub fn decrypt(&self, wire: &str) -> Result<Vec<u8>> {
        let raw = b64variant_decode(wire)
            .ok_or_else(|| anyhow::anyhow!("{}", crate::text::errors::NOT_ENVPICK_DATA))?;
        if raw.len() < IV_LEN + TAG_LEN {
            bail!("{}", crate::text::errors::NOT_ENVPICK_DATA);
        }

        let cipher =
            Aes256Gcm::new_from_slice(&self.enc).map_err(|_| anyhow::anyhow!("初始化 AES 失败"))?;
        let (iv, ct) = raw.split_at(IV_LEN);
        let mut iv_arr = [0u8; IV_LEN];
        iv_arr.copy_from_slice(iv);
        let nonce = Nonce::from(iv_arr);

        // GCM authentication failure means the key is wrong or the paste was
        // altered. Both are "can't trust this", and neither is recoverable here.
        let pt = cipher
            .decrypt(&nonce, ct)
            .map_err(|_| anyhow::anyhow!("{}", crate::text::errors::DECRYPT_FAILED))?;

        if !pt.starts_with(MAGIC) {
            bail!("{}", crate::text::errors::NOT_ENVPICK_DATA);
        }
        Ok(pt[MAGIC.len()..].to_vec())
    }
}

/// SHA-256 over the canonical document bytes, used to decide whether the local
/// copy has changed since the last sync.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Standard base64 with `/` -> `_` and no padding.
fn b64variant_encode(src: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD_NO_PAD
        .encode(src)
        .replace('/', "_")
}

/// Lenient decode matching the browser client, which maps `_` back to `/` and
/// drops anything outside the alphabet. Tolerating stray whitespace and
/// padding costs nothing and makes reads robust against a proxy or an editor
/// that appended a newline to the paste.
fn b64variant_decode(src: &str) -> Option<Vec<u8>> {
    let cleaned: String = src
        .trim()
        .chars()
        .map(|c| if c == '_' { '/' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '/')
        .collect();
    if cleaned.is_empty() {
        return None;
    }
    let mut padded = cleaned;
    let rem = padded.len() % 4;
    if rem != 0 {
        padded.extend(std::iter::repeat_n('=', 4 - rem));
    }
    base64::engine::general_purpose::STANDARD
        .decode(padded)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cross-checked against an independent Python implementation
    /// (`hmac`/`hashlib` HKDF-SHA256). If these ever change, the on-disk
    /// protocol has changed and every existing synced paste becomes unreadable.
    #[test]
    fn hkdf_matches_independent_implementation() {
        let c = SyncCrypto::derive("passphrase", "sync-id").unwrap();

        assert_eq!(
            c.enc.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "1142302049417b854501ae64d19237ee88c0d7ca5d15ac095cbe338f5a508825"
        );
        assert_eq!(
            c.manage_password(),
            "hZ8XX9Af3-LVLDVeoEFZM5_3tTXPAtjY8v0MxhzjoQ8"
        );
        assert_eq!(
            c.browser_key(),
            "EUIwIElBe4VFAa5k0ZI37ojA18pdFawJXL4zj1pQiCU"
        );
    }

    /// The manage password goes into a URL path, so it must stay inside the
    /// characters that need no escaping, and inside the server's length rule.
    #[test]
    fn manage_password_is_url_safe_and_accepted_length() {
        let c = SyncCrypto::derive("passphrase", "sync-id").unwrap();
        let pw = c.manage_password();
        assert!((8..=128).contains(&pw.len()), "len was {}", pw.len());
        assert!(
            pw.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'),
            "not URL-safe: {pw}"
        );
    }

    /// The pastebin's own decoder maps `_` -> `/` and strips padding, so our
    /// fragment key must not contain either character.
    #[test]
    fn browser_key_uses_pastebin_encoding() {
        let c = SyncCrypto::derive("passphrase", "sync-id").unwrap();
        assert!(!c.browser_key().contains('/'));
        assert!(!c.browser_key().contains('='));
        assert_eq!(c.browser_key().len(), 43);
    }

    /// Decrypts a ciphertext produced by an independent implementation
    /// (Python `cryptography`, fixed IV `000102...0b`), which proves the wire
    /// format really is the one the pastebin's web UI reads.
    #[test]
    fn decrypts_ciphertext_from_independent_implementation() {
        let c = SyncCrypto::derive("passphrase", "sync-id").unwrap();
        let wire = "AAECAwQFBgcICQoLk6EE6nsH69vzIdZzyDMD85vDqaocjmER7iawz4pnug92PggCrUc77R_U";
        let pt = c.decrypt(wire).unwrap();
        assert_eq!(pt, b"{\"hello\":\"world\"}");
    }

    #[test]
    fn round_trips_and_uses_a_fresh_iv() {
        let c = SyncCrypto::derive("passphrase", "sync-id").unwrap();
        let msg = b"{\"profiles\":{}}";
        let a = c.encrypt(msg).unwrap();
        let b = c.encrypt(msg).unwrap();
        assert_ne!(a, b, "each encryption must use a new IV");
        assert_eq!(c.decrypt(&a).unwrap(), msg);
        assert_eq!(c.decrypt(&b).unwrap(), msg);
    }

    #[test]
    fn ciphertext_carries_a_gcm_tag() {
        let c = SyncCrypto::derive("passphrase", "sync-id").unwrap();
        // Magic marker + 11 bytes of plaintext + 12 IV + 16 tag.
        let wire = c.encrypt(b"hello world").unwrap();
        assert_eq!(
            b64variant_decode(&wire).unwrap().len(),
            MAGIC.len() + 11 + IV_LEN + TAG_LEN
        );
    }

    #[test]
    fn wrong_passphrase_fails_closed() {
        let a = SyncCrypto::derive("passphrase", "sync-id").unwrap();
        let b = SyncCrypto::derive("wrong", "sync-id").unwrap();
        let wire = a.encrypt(b"secret").unwrap();
        let err = b.decrypt(&wire).unwrap_err().to_string();
        assert!(err.contains("解密失败"), "got: {err}");
    }

    /// Same passphrase, different sync id: the salt must make them unrelated.
    #[test]
    fn sync_id_is_part_of_the_key() {
        let a = SyncCrypto::derive("passphrase", "sync-one").unwrap();
        let b = SyncCrypto::derive("passphrase", "sync-two").unwrap();
        assert_ne!(a.browser_key(), b.browser_key());
        assert_ne!(a.manage_password(), b.manage_password());
        assert!(b.decrypt(&a.encrypt(b"x").unwrap()).is_err());
    }

    #[test]
    fn tampering_is_detected() {
        let c = SyncCrypto::derive("passphrase", "sync-id").unwrap();
        let wire = c.encrypt(b"{\"a\":1}").unwrap();
        let mut raw = b64variant_decode(&wire).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        assert!(c.decrypt(&b64variant_encode(&raw)).is_err());
    }

    #[test]
    fn garbage_is_reported_as_not_our_format() {
        let c = SyncCrypto::derive("passphrase", "sync-id").unwrap();
        assert!(c.decrypt("!!!not base64!!!").is_err());
        assert!(c.decrypt("").is_err());
        // Valid base64, long enough, but not something we wrote.
        assert!(c.decrypt(&b64variant_encode(&[0u8; 64])).is_err());
    }

    #[test]
    fn empty_passphrase_is_rejected() {
        assert!(SyncCrypto::derive("", "sync-id").is_err());
    }

    #[test]
    fn invalid_sync_id_is_rejected() {
        for id in ["", "ab", "has space", "has.dot", "a/b"] {
            assert!(SyncCrypto::derive("passphrase", id).is_err(), "{id}");
        }
    }

    #[test]
    fn content_hash_is_stable_and_distinguishing() {
        assert_eq!(content_hash(b"abc"), content_hash(b"abc"));
        assert_ne!(content_hash(b"abc"), content_hash(b"abd"));
        assert_eq!(content_hash(b"abc").len(), 64);
    }

    /// Tolerating a stray trailing newline costs nothing and prevents a
    /// mystifying failure if the paste ever gets mangled in transit.
    #[test]
    fn decode_tolerates_whitespace_and_padding() {
        let c = SyncCrypto::derive("passphrase", "sync-id").unwrap();
        let wire = c.encrypt(b"payload").unwrap();
        for variant in [format!("{wire}\n"), format!("  {wire}  "), wire.clone()] {
            assert_eq!(c.decrypt(&variant).unwrap(), b"payload");
        }
    }
}
