//! The synced document: what actually gets encrypted and uploaded.
//!
//! JSON rather than TOML because profile and variable names are arbitrary
//! strings; JSON's nested maps have no quoting ambiguities, so a variable named
//! `a.b` can never round-trip differently than it went in.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::config::{Profile, ProfileStore};
use crate::sync::crypto::content_hash;

/// Bumped only for a breaking change to the document layout.
pub const SCHEMA: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SyncDoc {
    pub schema: u32,
    /// Incremented on every successful push. Used to show progress in the UI.
    #[serde(default)]
    pub revision: u64,
    /// Which machine wrote this version — shown on the conflict screen.
    #[serde(default)]
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    pub updated_at: String,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

impl SyncDoc {
    /// Build a document from the local profiles, ready to push.
    pub fn from_store(
        store: &ProfileStore,
        device_id: &str,
        device_name: Option<&str>,
        revision: u64,
    ) -> Self {
        Self {
            schema: SCHEMA,
            revision,
            device_id: device_id.to_string(),
            device_name: device_name.map(str::to_string),
            updated_at: crate::clock::now_rfc3339(),
            profiles: store.profiles.clone(),
        }
    }

    pub fn to_store(&self) -> ProfileStore {
        ProfileStore {
            profiles: self.profiles.clone(),
        }
    }

    /// Canonical bytes. Pretty-printed so the paste is legible in a browser
    /// once decrypted; the hash is taken over exactly these bytes, so the
    /// choice is consistent on every machine.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec_pretty(self).context("序列化同步文档失败")
    }

    /// Parse a decrypted document. The payload only — `SyncCrypto::decrypt`
    /// has already verified and removed the magic marker.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let doc: SyncDoc = serde_json::from_slice(bytes).context("同步文档 JSON 解析失败")?;
        if doc.schema > SCHEMA {
            bail!(
                "远端文档版本为 {}，高于本程序支持的 {}，请升级 envpick",
                doc.schema,
                SCHEMA
            );
        }
        doc.to_store().validate()?;
        Ok(doc)
    }

    /// Hash of the canonical bytes. Used to display/compare whole documents;
    /// for "is the local copy dirty?" use [`profiles_hash`] instead, because
    /// `revision` and `updated_at` change on every rebuild.
    pub fn hash(&self) -> Result<String> {
        Ok(content_hash(&self.to_bytes()?))
    }

    /// One-line summary for the UI and for conflict messages.
    pub fn summary(&self) -> String {
        format!(
            "r{} · {} 个 profile · {}",
            self.revision,
            self.profiles.len(),
            self.updated_at
        )
    }
}

/// Hash over just the profile content, which is what "has the user changed
/// anything?" actually means. Deliberately excludes `revision`/`updated_at`/`device_id`:
/// those change on every rebuild and from every machine, so including them would
/// make every local copy look permanently dirty.
pub fn profiles_hash(profiles: &BTreeMap<String, Profile>) -> Result<String> {
    Ok(content_hash(
        &serde_json::to_vec(profiles).context("序列化 profiles 失败")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> SyncDoc {
        let mut store = ProfileStore::default();
        let mut p = Profile::default();
        // A name no shell would accept, but which must still round-trip.
        p.vars.insert("EDITOR".into(), "nvim".into());
        p.vars
            .insert("ODD".into(), "with \"quotes\" and \\slashes\\".into());
        store.profiles.insert("work".into(), p);
        SyncDoc::from_store(&store, "dev1", Some("macbook"), 3)
    }

    #[test]
    fn round_trips_through_bytes() {
        let d = doc();
        let back = SyncDoc::from_bytes(&d.to_bytes().unwrap()).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn hash_is_stable_across_serialization() {
        let d = doc();
        let back = SyncDoc::from_bytes(&d.to_bytes().unwrap()).unwrap();
        assert_eq!(d.hash().unwrap(), back.hash().unwrap());
    }

    /// Insertion order must not affect the hash, or two machines with the same
    /// content would disagree about being dirty.
    #[test]
    fn hash_ignores_insertion_order() {
        let mut a = ProfileStore::default();
        let mut pa = Profile::default();
        pa.vars.insert("A".into(), "1".into());
        pa.vars.insert("B".into(), "2".into());
        a.profiles.insert("p".into(), pa);

        let mut b = ProfileStore::default();
        let mut pb = Profile::default();
        pb.vars.insert("B".into(), "2".into());
        pb.vars.insert("A".into(), "1".into());
        b.profiles.insert("p".into(), pb);

        let da = SyncDoc::from_store(&a, "d", None, 1);
        let db = SyncDoc::from_store(&b, "d", None, 1);
        assert_eq!(da.hash().unwrap(), db.hash().unwrap());
    }

    #[test]
    fn hash_changes_when_content_changes() {
        let d = doc();
        let mut edited = d.clone();
        edited
            .profiles
            .get_mut("work")
            .unwrap()
            .vars
            .insert("X".into(), "1".into());
        assert_ne!(d.hash().unwrap(), edited.hash().unwrap());
    }

    #[test]
    fn rejects_a_newer_schema() {
        let mut d = doc();
        d.schema = SCHEMA + 1;
        let err = SyncDoc::from_bytes(&d.to_bytes().unwrap()).unwrap_err();
        assert!(err.to_string().contains("升级"), "got: {err}");
    }

    #[test]
    fn rejects_documents_with_invalid_variable_names() {
        let mut d = doc();
        d.profiles
            .get_mut("work")
            .unwrap()
            .vars
            .insert("not valid".into(), "x".into());
        // A remote could contain this if it was written by a different tool;
        // refuse it rather than writing an unimportable variable.
        assert!(SyncDoc::from_bytes(&d.to_bytes().unwrap()).is_err());
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(SyncDoc::from_bytes(b"{not json").is_err());
        assert!(SyncDoc::from_bytes(b"{}").is_err());
    }

    #[test]
    fn store_conversion_is_lossless() {
        let d = doc();
        assert_eq!(d.to_store().profiles, d.profiles);
    }

    /// The dirty check must survive a rebuild: revision and timestamps move on,
    /// the profile content does not.
    #[test]
    fn profiles_hash_ignores_metadata_changes() {
        let a = doc();
        let mut b = a.clone();
        b.revision = 999;
        b.updated_at = "2030-01-01T00:00:00Z".into();
        b.device_id = "someone-else".into();
        assert_eq!(
            profiles_hash(&a.profiles).unwrap(),
            profiles_hash(&b.profiles).unwrap()
        );
    }

    #[test]
    fn profiles_hash_tracks_content() {
        let a = doc();
        let mut b = a.clone();
        b.profiles
            .get_mut("work")
            .unwrap()
            .vars
            .insert("NEW".into(), "1".into());
        assert_ne!(
            profiles_hash(&a.profiles).unwrap(),
            profiles_hash(&b.profiles).unwrap()
        );
    }
}
