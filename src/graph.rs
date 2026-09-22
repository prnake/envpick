//! Profile dependency resolution.
//!
//! `requires` means "apply that profile first". Resolution is a depth-first
//! post-order walk, which yields dependencies before dependents and detects
//! cycles with the offending path still on the stack.

use anyhow::{Result, bail};
use std::collections::{BTreeMap, BTreeSet};

use crate::config::ProfileStore;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// On the current DFS stack — meeting one of these again is a cycle.
    InProgress,
    Done,
}

/// Expand `roots` into a de-duplicated, dependencies-first activation order.
///
/// Ordering matters: a dependent profile is applied after the profiles it
/// requires, so its own values win where they overlap.
pub fn resolve_order(store: &ProfileStore, roots: &[String]) -> Result<Vec<String>> {
    let mut marks: BTreeMap<String, Mark> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut stack: Vec<String> = Vec::new();

    for root in roots {
        visit(store, root, &mut marks, &mut order, &mut stack)?;
    }
    Ok(order)
}

fn visit(
    store: &ProfileStore,
    name: &str,
    marks: &mut BTreeMap<String, Mark>,
    order: &mut Vec<String>,
    stack: &mut Vec<String>,
) -> Result<()> {
    match marks.get(name) {
        Some(Mark::Done) => return Ok(()),
        Some(Mark::InProgress) => {
            // Reconstruct the cycle for the message: from wherever `name` first
            // appeared on the stack, through to here.
            let start = stack.iter().position(|n| n == name).unwrap_or(0);
            let mut path: Vec<&str> = stack[start..].iter().map(String::as_str).collect();
            path.push(name);
            bail!("{}: {}", crate::text::errors::CYCLE, path.join(" -> "));
        }
        None => {}
    }

    // A missing profile is an error rather than a silent skip: a typo in
    // `requires` should surface, not quietly produce the wrong environment.
    let profile = store
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("{}: '{name}'", crate::text::errors::PROFILE_NOT_FOUND))?;

    marks.insert(name.to_string(), Mark::InProgress);
    stack.push(name.to_string());

    for dep in &profile.requires {
        visit(store, dep, marks, order, stack)?;
    }

    stack.pop();
    marks.insert(name.to_string(), Mark::Done);
    order.push(name.to_string());
    Ok(())
}

/// Add a `requires` edge, refusing anything that would leave the graph
/// unresolvable.
///
/// The cycle check runs against the already-mutated store and the edge is
/// removed again on failure. Checking *after* inserting rather than predicting
/// the outcome keeps one implementation of the cycle rule; the rollback is what
/// keeps a cycle from ever being observable, or from being written out by a
/// caller that saves immediately afterwards.
pub fn add_requires(store: &mut ProfileStore, name: &str, dep: &str) -> Result<()> {
    if name == dep {
        bail!("profile 不能依赖自己");
    }
    if !store.contains(dep) {
        bail!("{}: '{dep}'", crate::text::errors::PROFILE_NOT_FOUND);
    }
    let mut profile = store
        .get(name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("{}: '{name}'", crate::text::errors::PROFILE_NOT_FOUND))?;
    if profile.requires.iter().any(|r| r == dep) {
        bail!("'{name}' 已经依赖 '{dep}'");
    }
    profile.requires.push(dep.to_string());
    store.upsert(name, profile)?;

    if let Err(e) = resolve_order(store, &[name.to_string()]) {
        if let Some(mut reverted) = store.get(name).cloned() {
            reverted.requires.retain(|r| r != dep);
            store.profiles.insert(name.to_string(), reverted);
        }
        return Err(e);
    }
    Ok(())
}

/// Remove a `requires` edge. Removing one can never introduce a cycle, so there
/// is nothing to re-check.
pub fn remove_requires(store: &mut ProfileStore, name: &str, dep: &str) -> Result<()> {
    let mut profile = store
        .get(name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("{}: '{name}'", crate::text::errors::PROFILE_NOT_FOUND))?;
    let before = profile.requires.len();
    profile.requires.retain(|r| r != dep);
    if profile.requires.len() == before {
        bail!("'{name}' 并未依赖 '{dep}'");
    }
    store.upsert(name, profile)
}

/// Merge the variables of an already-ordered profile list. Later entries win.
pub fn merged_vars(store: &ProfileStore, order: &[String]) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for name in order {
        let profile = store.get(name).ok_or_else(|| {
            anyhow::anyhow!("{}: '{name}'", crate::text::errors::PROFILE_NOT_FOUND)
        })?;
        for (k, v) in &profile.vars {
            out.insert(k.clone(), v.clone());
        }
    }
    Ok(out)
}

/// Every profile that `name` transitively requires. Used to warn before a
/// delete or rename that would break other profiles.
pub fn dependents_of(store: &ProfileStore, name: &str) -> Vec<String> {
    let mut found = BTreeSet::new();
    for (other, profile) in &store.profiles {
        if other != name && profile.requires.iter().any(|d| d == name) {
            found.insert(other.clone());
        }
    }
    found.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(spec: &[crate::testing::ProfileSpec<'_>]) -> ProfileStore {
        crate::testing::store_with(spec)
    }

    #[test]
    fn dependencies_come_before_dependents() {
        let s = store(&[
            ("app", &["lib"], &[]),
            ("lib", &["base"], &[]),
            ("base", &[], &[]),
        ]);
        let order = resolve_order(&s, &["app".to_string()]).unwrap();
        assert_eq!(order, vec!["base", "lib", "app"]);
    }

    #[test]
    fn dependent_values_override_required_ones() {
        let s = store(&[
            ("app", &["base"], &[("EDITOR", "nvim"), ("ONLY_APP", "1")]),
            ("base", &[], &[("EDITOR", "vim"), ("ONLY_BASE", "1")]),
        ]);
        let order = resolve_order(&s, &["app".to_string()]).unwrap();
        let vars = merged_vars(&s, &order).unwrap();
        assert_eq!(vars.get("EDITOR").unwrap(), "nvim");
        assert_eq!(vars.get("ONLY_BASE").unwrap(), "1");
        assert_eq!(vars.get("ONLY_APP").unwrap(), "1");
    }

    #[test]
    fn diamond_dependencies_are_visited_once() {
        let s = store(&[
            ("a", &["b", "c"], &[]),
            ("b", &["d"], &[]),
            ("c", &["d"], &[]),
            ("d", &[], &[]),
        ]);
        let order = resolve_order(&s, &["a".to_string()]).unwrap();
        assert_eq!(order.iter().filter(|n| *n == "d").count(), 1);
        assert_eq!(order.last().unwrap(), "a");
        assert!(
            order.iter().position(|n| n == "d").unwrap()
                < order.iter().position(|n| n == "b").unwrap()
        );
    }

    #[test]
    fn detects_cycles_and_names_them() {
        let s = store(&[("a", &["b"], &[]), ("b", &["c"], &[]), ("c", &["a"], &[])]);
        let err = resolve_order(&s, &["a".to_string()]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("环"), "should mention a cycle: {msg}");
        for name in ["a", "b", "c"] {
            assert!(msg.contains(name), "cycle path should name {name}: {msg}");
        }
    }

    #[test]
    fn self_cycle_is_caught() {
        let s = store(&[("a", &["a"], &[])]);
        assert!(resolve_order(&s, &["a".to_string()]).is_err());
    }

    #[test]
    fn unknown_requirement_is_an_error() {
        let s = store(&[("a", &["nope"], &[])]);
        let err = resolve_order(&s, &["a".to_string()]).unwrap_err();
        assert!(err.to_string().contains("nope"));
    }

    #[test]
    fn missing_root_is_an_error() {
        let s = store(&[]);
        assert!(resolve_order(&s, &["ghost".to_string()]).is_err());
    }

    #[test]
    fn finds_dependents() {
        let s = store(&[
            ("a", &["shared"], &[]),
            ("b", &["shared"], &[]),
            ("shared", &[], &[]),
        ]);
        assert_eq!(dependents_of(&s, "shared"), vec!["a", "b"]);
    }

    /// A refused edge has to leave the store byte-identical: callers save
    /// immediately afterwards, so a rollback that missed anything would write a
    /// broken graph to disk.
    #[test]
    fn a_cycle_is_refused_and_rolled_back() {
        let mut s = store(&[("a", &[], &[]), ("b", &["a"], &[])]);
        let before = s.clone();
        let err = add_requires(&mut s, "a", "b").unwrap_err();
        assert!(err.to_string().contains("环"), "got: {err}");
        assert_eq!(s, before, "store left modified after a rejected edge");
        assert!(resolve_order(&s, &["b".to_string()]).is_ok());
    }

    #[test]
    fn adding_a_longer_cycle_is_refused() {
        let mut s = store(&[("a", &["b"], &[]), ("b", &["c"], &[]), ("c", &[], &[])]);
        let before = s.clone();
        assert!(add_requires(&mut s, "c", "a").is_err());
        assert_eq!(s, before);
    }

    #[test]
    fn adding_a_bad_edge_is_refused() {
        let mut s = store(&[("a", &["b"], &[]), ("b", &[], &[])]);
        // unknown dependency, unknown profile, self-dependency, duplicate
        assert!(add_requires(&mut s, "a", "ghost").is_err());
        assert!(add_requires(&mut s, "ghost", "a").is_err());
        assert!(add_requires(&mut s, "a", "a").is_err());
        assert!(
            add_requires(&mut s, "a", "b")
                .unwrap_err()
                .to_string()
                .contains("已经依赖")
        );
        assert_eq!(s.get("a").unwrap().requires, vec!["b".to_string()]);
    }

    #[test]
    fn a_valid_edge_is_added() {
        let mut s = store(&[("a", &[], &[]), ("b", &[], &[])]);
        add_requires(&mut s, "a", "b").unwrap();
        assert_eq!(s.get("a").unwrap().requires, vec!["b".to_string()]);
        assert_eq!(
            resolve_order(&s, &["a".to_string()]).unwrap(),
            vec!["b", "a"]
        );
    }

    #[test]
    fn removing_an_edge_that_is_not_there_is_refused() {
        let mut s = store(&[("a", &["b"], &[]), ("b", &[], &[])]);
        assert!(remove_requires(&mut s, "a", "c").is_err());
        assert!(remove_requires(&mut s, "ghost", "b").is_err());
        remove_requires(&mut s, "a", "b").unwrap();
        assert!(s.get("a").unwrap().requires.is_empty());
    }
}
