//! Reading and writing `profiles.toml`, the file that gets synced.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::config::profile::{validate_profile_name, validate_var_name};
use crate::config::{Profile, write_atomic};

/// The whole profile collection, as stored on disk and as synced.
///
/// One file rather than one-per-profile: it serializes to exactly one encrypted
/// blob, so a sync is a single atomic object with no multi-file consistency
/// problem to reason about.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProfileStore {
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

impl ProfileStore {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("无法读取 {}", path.display()))?;
        let store: ProfileStore =
            toml::from_str(&raw).with_context(|| format!("{} 格式有误", path.display()))?;
        store.validate()?;
        Ok(store)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let raw = toml::to_string_pretty(self).context("序列化 profiles 失败")?;
        write_atomic(path, raw.as_bytes())
    }

    /// Reject anything we'd otherwise write out and have fail confusingly
    /// later — bad names, or variables the shell could never import.
    pub fn validate(&self) -> Result<()> {
        for (name, profile) in &self.profiles {
            validate_profile_name(name)?;
            for var in profile.vars.keys() {
                validate_var_name(var).with_context(|| format!("profile '{name}'"))?;
            }
        }
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.profiles.contains_key(name)
    }

    pub fn names(&self) -> Vec<String> {
        self.profiles.keys().cloned().collect()
    }

    /// Insert or replace, after validating the name.
    pub fn upsert(&mut self, name: &str, profile: Profile) -> Result<()> {
        validate_profile_name(name)?;
        for var in profile.vars.keys() {
            validate_var_name(var)?;
        }
        self.profiles.insert(name.to_string(), profile);
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Option<Profile> {
        self.profiles.remove(name)
    }

    /// Drop references to profiles that no longer exist, so deleting a profile
    /// doesn't leave every dependent permanently broken.
    pub fn prune_dangling_requires(&mut self) -> Vec<(String, String)> {
        let existing: Vec<String> = self.profiles.keys().cloned().collect();
        let mut removed = Vec::new();
        for (name, profile) in self.profiles.iter_mut() {
            profile.requires.retain(|dep| {
                if existing.contains(dep) {
                    true
                } else {
                    removed.push((name.clone(), dep.clone()));
                    false
                }
            });
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(name: &str, vars: &[(&str, &str)]) -> ProfileStore {
        let mut s = ProfileStore::default();
        let mut p = Profile::default();
        for (k, v) in vars {
            p.vars.insert((*k).to_string(), (*v).to_string());
        }
        s.profiles.insert(name.to_string(), p);
        s
    }

    #[test]
    fn round_trips_through_toml() {
        let mut store = store_with("work", &[("EDITOR", "nvim"), ("PAGER", "less")]);
        store
            .profiles
            .get_mut("work")
            .unwrap()
            .requires
            .push("base".into());
        store.profiles.insert("base".into(), Profile::default());

        let raw = toml::to_string_pretty(&store).unwrap();
        let back: ProfileStore = toml::from_str(&raw).unwrap();
        assert_eq!(store, back, "TOML round-trip changed the data:\n{raw}");
    }

    /// Values with characters that are special to TOML must survive the round
    /// trip, since a mangled value would silently corrupt the user's env.
    #[test]
    fn round_trips_awkward_values() {
        let awkward = [
            ("QUOTED", "he said \"hi\""),
            ("BACKSLASH", r"C:\path\to"),
            ("NEWLINE", "line1\nline2"),
            ("UNICODE", "中文值 😀"),
            ("EMPTY", ""),
            ("HASH", "value # not a comment"),
            ("BRACKET", "[not, an, array]"),
            ("TABS", "a\tb"),
        ];
        let store = store_with("odd", &awkward);
        let raw = toml::to_string_pretty(&store).unwrap();
        let back: ProfileStore = toml::from_str(&raw).unwrap();
        assert_eq!(store, back, "awkward values did not survive:\n{raw}");
    }

    /// Env var names are POSIX-restricted, so the TOML key quoting edge cases
    /// are already excluded by validation — this pins that down.
    #[test]
    fn rejects_var_names_toml_would_have_to_quote() {
        let store = store_with("bad", &[("has.dot", "x")]);
        assert!(store.validate().is_err());
    }

    #[test]
    fn prune_drops_only_dangling_requirements() {
        let mut store = store_with("work", &[]);
        store.profiles.get_mut("work").unwrap().requires = vec!["base".into(), "ghost".into()];
        store.profiles.insert("base".into(), Profile::default());

        let removed = store.prune_dangling_requires();
        assert_eq!(removed, vec![("work".to_string(), "ghost".to_string())]);
        assert_eq!(
            store.get("work").unwrap().requires,
            vec!["base".to_string()]
        );
    }

    /// This file is meant to be hand-edited, so a typo has to be an error.
    /// Ignoring unknown keys would hand the user a config that looks correct and
    /// an environment that quietly ignores part of it.
    #[test]
    fn a_typo_in_a_hand_edited_key_is_rejected() {
        // `require` instead of `requires` — the serializer emits no bare
        // `[profiles.work]` header at all when `requires` is empty, so this is
        // the shape a user editing by hand is most likely to end up with.
        let err = toml::from_str::<ProfileStore>("[profiles.work]\nrequire = [\"base\"]\n")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("require"),
            "should name the offending key: {err}"
        );

        // A misspelled table name, and a var table that is not under `vars`.
        assert!(toml::from_str::<ProfileStore>("[profile.work.vars]\nA = \"1\"\n").is_err());
        assert!(toml::from_str::<ProfileStore>("[profiles.work]\nvar = { A = \"1\" }\n").is_err());
    }
}
