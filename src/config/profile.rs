//! A profile: a named set of environment variables, plus the other profiles it
//! builds on.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::config::GLOBAL_PROFILE;

/// One profile. Maps are `BTreeMap` rather than `HashMap` on purpose: the sync
/// document is hashed to detect local edits, so serialization has to be
/// byte-for-byte deterministic.
///
/// `deny_unknown_fields` is deliberate: this file is meant to be hand-edited,
/// and without it a typo (`require`, or `requires` filed under the wrong table)
/// parses cleanly and is then silently ignored — the user sees a config that
/// looks right and an environment that ignores it. Failing loudly costs a
/// forward-compatibility guarantee we don't need, since both sides of a sync run
/// the same version of this tool.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Profiles that must be activated before this one. Their variables are
    /// applied first, so this profile's own values win on conflict.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vars: BTreeMap<String, String>,
}

impl Profile {
    pub fn is_empty(&self) -> bool {
        self.requires.is_empty() && self.vars.is_empty()
    }
}

/// Profile names are used as TOML keys and in shell output, so keep them to a
/// conservative, obviously-safe set.
pub fn validate_profile_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if !ok {
        bail!("{}: {name}", crate::text::errors::PROFILE_NAME_INVALID);
    }
    Ok(())
}

/// POSIX environment variable names. Shells silently refuse to import anything
/// outside this set, so a name like `my.var` would appear to save fine and then
/// never actually reach the environment.
pub fn validate_var_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let ok = match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    };
    if !ok {
        bail!("{}: {name}", crate::text::errors::VAR_NAME_INVALID);
    }
    Ok(())
}

/// `global` is activated in every shell, so it can't be deleted or renamed away.
pub fn is_reserved(name: &str) -> bool {
    name == GLOBAL_PROFILE
}

pub fn validate_new_profile_name(name: &str) -> Result<()> {
    validate_profile_name(name)?;
    if is_reserved(name) {
        bail!("{}: {name}", crate::text::errors::PROFILE_NAME_RESERVED);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_names() {
        for n in ["work", "db.local", "my-profile", "a_b", "A1"] {
            assert!(validate_profile_name(n).is_ok(), "{n} should be valid");
        }
    }

    #[test]
    fn rejects_bad_profile_names() {
        for n in ["", "-lead", "has space", "sla/sh", "emoji😀"] {
            assert!(validate_profile_name(n).is_err(), "{n} should be rejected");
        }
    }

    #[test]
    fn rejects_non_posix_var_names() {
        for n in ["", "1ABC", "has.dot", "has-dash", "a b", "="] {
            assert!(validate_var_name(n).is_err(), "{n} should be rejected");
        }
        for n in ["_A", "A", "A1_B2", "EDITOR"] {
            assert!(validate_var_name(n).is_ok(), "{n} should be valid");
        }
    }
}
