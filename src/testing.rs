//! Shared test fixtures.
//!
//! Several modules need "a store holding these profiles". Writing that inline
//! each time meant repeating the same fixture-builder in seven places, twice
//! over: once as a nested slice type, and once as `Default::default()` followed
//! by field assignment. Both are exactly what `clippy::type_complexity` and
//! `clippy::field_reassign_with_default` exist to flag, and fourteen warnings of
//! fixture noise is how a real warning gets missed.

use crate::config::{Profile, ProfileStore};

/// One profile as a test wants to write it: `(name, requires, vars)`.
///
/// The alias is the point — the tuple spelled out is long enough that clippy
/// flags every signature it appears in.
pub type ProfileSpec<'a> = (&'a str, &'a [&'a str], &'a [(&'a str, &'a str)]);

/// A store holding `entries`, with no other profiles.
pub fn store_with(entries: &[ProfileSpec<'_>]) -> ProfileStore {
    let mut store = ProfileStore::default();
    for (name, requires, vars) in entries {
        store.profiles.insert(
            (*name).to_string(),
            Profile {
                requires: requires.iter().map(|s| (*s).to_string()).collect(),
                vars: vars
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                    .collect(),
            },
        );
    }
    store
}

/// Convenience for the common case of a list of plain strings.
pub fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}
