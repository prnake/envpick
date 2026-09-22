//! Turning a profile selection into shell code the caller evaluates.
//!
//! A child process cannot modify its parent's environment, so `envpick` never
//! activates anything itself. It prints `export`/`unset` lines, and the shell
//! integration evals them.
//!
//! # Why the original values live in the shell
//!
//! Restoring a variable means knowing what it was *before* activation. Storing
//! that in a file would be wrong: every terminal shares the file, so a second
//! terminal activating the same profile would overwrite the first one's record,
//! and the first would then "restore" the second's values. Shell variables are
//! per-session by construction, so each session rolls back only its own
//! changes.
//!
//! For each variable it is about to set, the emitted code saves the original
//! once, guarded by a sentinel:
//!
//! ```sh
//! if [ -z "${ENVPICK_SAVED_EDITOR+x}" ]; then
//!   ENVPICK_SAVED_EDITOR=1
//!   if [ -n "${EDITOR+x}" ]; then ENVPICK_ORIG_EDITOR="$EDITOR"; ENVPICK_HAD_EDITOR=1
//!   else ENVPICK_HAD_EDITOR=; fi
//! fi
//! ```
//!
//! The `HAD` flag is what distinguishes "was empty" from "did not exist" — the
//! first must be exported back as empty, the second must be `unset`. `${x+y}`
//! rather than a plain expansion keeps this correct under `set -u`.

pub mod emit;
pub mod templates;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;

use crate::config::{GLOBAL_PROFILE, ProfileStore};
use crate::graph;

pub use emit::{emit, quote};
pub use templates::{Shell, init_script};

/// Shell variable holding the comma-separated list of active profiles.
pub const ENV_ACTIVE: &str = "ENVPICK_ACTIVE";

/// Read the active profile list from the current environment.
pub fn active_profiles() -> Vec<String> {
    parse_active(std::env::var(ENV_ACTIVE).ok().as_deref())
}

/// Profile names allow only `[A-Za-z0-9._-]`, so a comma is an unambiguous
/// separator and needs no escaping.
pub fn parse_active(value: Option<&str>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    dedup(value.split(',').map(str::trim).filter(|s| !s.is_empty()))
}

/// Remove duplicates, keeping first-seen order.
fn dedup<'a>(items: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    items
        .filter(|s| seen.insert(s.to_string()))
        .map(str::to_string)
        .collect()
}

/// `global` is activated in every shell, so anything that activates a profile
/// also implies it. Not applied to deactivation: `off` means everything off,
/// `global` included.
pub fn with_global(store: &ProfileStore, roots: &[String]) -> Vec<String> {
    if !store.contains(GLOBAL_PROFILE) {
        return roots.to_vec();
    }
    let mut out = vec![GLOBAL_PROFILE.to_string()];
    out.extend(
        roots
            .iter()
            .filter(|r| r.as_str() != GLOBAL_PROFILE)
            .cloned(),
    );
    out
}

/// The active list after adding `additions`, keeping the existing order.
pub fn add_roots(current: &[String], additions: &[String]) -> Vec<String> {
    let mut out: Vec<String> = current.to_vec();
    for name in additions {
        if !out.contains(name) {
            out.push(name.clone());
        }
    }
    out
}

/// The active list after removing `removals`.
pub fn remove_roots(current: &[String], removals: &[String]) -> Vec<String> {
    current
        .iter()
        .filter(|name| !removals.contains(name))
        .cloned()
        .collect()
}

/// Profiles that are active only because something else requires them.
///
/// `ENVPICK_ACTIVE` holds the roots — the profiles the user named. Dependencies
/// are re-expanded on every activation, so they are not recorded there, but they
/// contribute variables all the same. Anything that *reports* activation state
/// therefore has to account for them: without this, `ep list` shows a profile as
/// inactive while its values are sitting in the environment, and the variable
/// count in `ep status` looks like it does not add up.
///
/// A broken graph yields an empty list rather than an error — this is display
/// only, and `ep check` is where a cycle gets reported.
pub fn indirect_active(store: &ProfileStore, active: &[String]) -> Vec<String> {
    graph::resolve_order(store, active)
        .map(|order| order.into_iter().filter(|n| !active.contains(n)).collect())
        .unwrap_or_default()
}

/// What activating a set of profiles does to the environment.
#[derive(Debug, Default)]
pub struct Transition {
    /// Profiles active afterwards, in the order the user asked for them. The
    /// expanded dependency order is recomputed on each use, so a profile that
    /// gains a dependency later takes effect without re-activating.
    pub after: Vec<String>,
    /// Variables to assign, in dependencies-first order.
    pub set: BTreeMap<String, String>,
    /// Variables to restore to their saved original.
    pub restore: Vec<String>,
}

/// Resolve roots into (dependency order, merged variables).
fn expand(
    store: &ProfileStore,
    roots: &[String],
) -> Result<(Vec<String>, BTreeMap<String, String>)> {
    let order = graph::resolve_order(store, roots)?;
    let vars = graph::merged_vars(store, &order)?;
    Ok((order, vars))
}

/// Drop names the store no longer has. `ENVPICK_ACTIVE` is set by an earlier
/// invocation and may name a profile that has since been deleted or renamed;
/// resolving that name would otherwise fail and make every later command —
/// including `off` — impossible to run. Stale entries self-heal here.
fn keep_known(store: &ProfileStore, roots: &[String]) -> Vec<String> {
    roots
        .iter()
        .filter(|r| store.contains(r))
        .cloned()
        .collect()
}

/// Compute the change from `saved` to `after`.
///
/// `after` is a *root* list: the profiles the user named, not their expanded
/// dependencies. This is deliberately literal — it applies exactly the roots it
/// is given, so "keep the local copy" style decisions belong to the caller. Use
/// [`with_global`] on `after` for the commands where `global` should be
/// implied.
///
/// `saved` is the set of variables this session has already modified (see
/// [`saved_vars`]); anything in it that `after` no longer provides is restored.
/// Deriving the restore set from `saved` rather than from the previously active
/// profiles is what keeps deactivation correct when the graph moves underneath
/// an active session: drop a `requires` edge, or delete a profile, and
/// re-expanding the same roots yields a different variable set — so the
/// difference would be silently left behind in the user's environment.
pub fn transition(store: &ProfileStore, saved: &[String], after: &[String]) -> Result<Transition> {
    let after = keep_known(store, after);
    let (_, after_vars) = expand(store, &after)?;

    let restore = saved
        .iter()
        .filter(|k| !after_vars.contains_key(*k))
        .cloned()
        .collect();

    Ok(Transition {
        after,
        set: after_vars,
        restore,
    })
}

/// Deactivate everything: restore every variable this session modified, whoever
/// provided it — including a profile that has since been deleted.
pub fn teardown(store: &ProfileStore, saved: &[String]) -> Result<Transition> {
    transition(store, saved, &[])
}

/// Prefix of the exported sentinel marking "this variable's original has been
/// saved". Shared with the emitter so the two can never disagree about the
/// spelling.
pub const SAVED_PREFIX: &str = "ENVPICK_SAVED_";

/// The variables this session has saved an original for, read back from the
/// exported `ENVPICK_SAVED_<name>` sentinels the emitted script leaves behind.
///
/// The active profile list is not enough to work this out (see [`transition`]),
/// and this is the better record anyway: it is exactly the set that was applied,
/// so it stays right when the configuration changes mid-session.
pub fn saved_vars() -> Vec<String> {
    parse_saved(std::env::vars())
}

/// Split the sentinels out of an environment listing. Pure, so it can be tested
/// without mutating the process environment — which tests running in parallel
/// would race over.
fn parse_saved(vars: impl Iterator<Item = (String, String)>) -> Vec<String> {
    let mut out: Vec<String> = vars
        .filter_map(|(k, _)| k.strip_prefix(SAVED_PREFIX).map(str::to_string))
        .filter(|name| !name.is_empty())
        .collect();
    out.sort();
    out
}

impl Transition {
    /// The value `ENVPICK_ACTIVE` should have afterwards.
    pub fn active_value(&self) -> String {
        self.after.join(",")
    }

    /// True when there is nothing to do — used to keep the shell integration
    /// from re-running an identical script.
    pub fn is_noop(&self) -> bool {
        self.set.is_empty() && self.restore.is_empty()
    }
}

/// The variables a selection applies. Tests pass this as `saved` to stand in for
/// a session that has already activated those profiles — in a real shell the
/// equivalent comes from [`saved_vars`], reading the exported sentinels.
#[cfg(test)]
pub(crate) fn applied_vars(store: &ProfileStore, roots: &[String]) -> Vec<String> {
    expand(store, roots)
        .expect("test fixture should resolve")
        .1
        .into_keys()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::names;

    fn store_with(entries: &[crate::testing::ProfileSpec<'_>]) -> ProfileStore {
        crate::testing::store_with(entries)
    }

    #[test]
    fn parse_active_handles_empty_and_duplicates() {
        assert!(parse_active(None).is_empty());
        assert!(parse_active(Some("")).is_empty());
        assert_eq!(parse_active(Some("a,b")), names(&["a", "b"]));
        assert_eq!(parse_active(Some("a, a ,b")), names(&["a", "b"]));
        assert_eq!(parse_active(Some(",,")), Vec::<String>::new());
    }

    /// A dependency contributes variables, so reporting it as inactive would be
    /// a lie the user can see through with `env`. Roots stay out of the result:
    /// they are named separately.
    #[test]
    fn indirect_active_names_only_the_pulled_in_profiles() {
        let store = store_with(&[
            ("global", &[], &[]),
            ("base", &[], &[("EDITOR", "nvim")]),
            ("work", &["base"], &[("X", "1")]),
        ]);
        assert_eq!(
            indirect_active(&store, &names(&["global", "work"])),
            names(&["base"])
        );
        // Nothing pulled in -> nothing reported, so callers can skip the line.
        assert!(indirect_active(&store, &names(&["global"])).is_empty());
        assert!(indirect_active(&store, &[]).is_empty());
    }

    /// Display must not become a second way for a broken graph to fail: `ep list`
    /// has to keep working so the user can still see their profiles while
    /// `ep check` explains the cycle.
    #[test]
    fn indirect_active_tolerates_a_cycle() {
        let store = store_with(&[("a", &["b"], &[]), ("b", &["a"], &[])]);
        assert!(indirect_active(&store, &names(&["a"])).is_empty());
    }

    #[test]
    fn add_and_remove_roots() {
        let current = names(&["global", "work"]);
        assert_eq!(
            add_roots(&current, &names(&["work", "dev"])),
            names(&["global", "work", "dev"])
        );
        assert_eq!(
            remove_roots(&current, &names(&["work"])),
            names(&["global"])
        );
        assert_eq!(remove_roots(&current, &names(&["nope"])), current);
    }

    #[test]
    fn activation_pulls_in_dependencies() {
        let store = store_with(&[
            ("base", &[], &[("A", "base")]),
            ("work", &["base"], &[("B", "work")]),
        ]);
        let t = transition(&store, &[], &names(&["work"])).unwrap();
        assert_eq!(t.after, names(&["work"]));
        assert_eq!(t.set["A"], "base");
        assert_eq!(t.set["B"], "work");
        assert!(t.restore.is_empty());
    }

    #[test]
    fn a_dependent_profile_overrides_what_it_requires() {
        let store = store_with(&[
            ("base", &[], &[("A", "base"), ("ONLY_BASE", "1")]),
            ("work", &["base"], &[("A", "work")]),
        ]);
        let t = transition(&store, &[], &names(&["work"])).unwrap();
        assert_eq!(t.set["A"], "work");
        assert_eq!(t.set["ONLY_BASE"], "1");
    }

    /// Removing one profile must not disturb variables another still-active
    /// profile contributes.
    #[test]
    fn deactivation_only_restores_variables_nothing_else_sets() {
        let store = store_with(&[
            ("shared", &[], &[("BOTH", "shared"), ("ONLY_SHARED", "x")]),
            ("work", &["shared"], &[("BOTH", "work"), ("ONLY_WORK", "y")]),
        ]);
        let saved = applied_vars(&store, &names(&["shared", "work"]));
        let t = transition(&store, &saved, &names(&["shared"])).unwrap();
        assert_eq!(t.restore, vec!["ONLY_WORK".to_string()]);
        // BOTH is still contributed by `shared`, so it gets reassigned rather
        // than restored, and ONLY_SHARED is simply left alone.
        assert_eq!(t.set["BOTH"], "shared");
        assert!(t.set.contains_key("ONLY_SHARED"));
    }

    #[test]
    fn teardown_restores_everything() {
        let store = store_with(&[("work", &[], &[("A", "1"), ("B", "2")])]);
        let saved = applied_vars(&store, &names(&["work"]));
        let t = teardown(&store, &saved).unwrap();
        assert!(t.set.is_empty());
        assert_eq!(t.restore, vec!["A".to_string(), "B".to_string()]);
        assert_eq!(t.active_value(), "");
        assert!(!t.is_noop());
    }

    /// `off` must deactivate `global` too, which is why the implicit-global
    /// rule lives in [`with_global`] rather than inside `transition`.
    #[test]
    fn teardown_includes_the_global_profile() {
        let store = store_with(&[("global", &[], &[("G", "1")]), ("work", &[], &[("W", "1")])]);
        let active = with_global(&store, &names(&["work"]));
        assert_eq!(active, names(&["global", "work"]));

        let saved = applied_vars(&store, &active);
        let t = teardown(&store, &saved).unwrap();
        assert!(t.set.is_empty());
        assert_eq!(t.restore, vec!["G".to_string(), "W".to_string()]);
        assert_eq!(t.active_value(), "");
    }

    #[test]
    fn use_implies_global_but_only_once() {
        let store = store_with(&[("global", &[], &[("G", "1")]), ("work", &[], &[("W", "1")])]);
        let t = transition(&store, &[], &with_global(&store, &names(&["work"]))).unwrap();
        assert_eq!(t.after, names(&["global", "work"]));
        assert_eq!(t.set["G"], "1");

        // Naming it explicitly must not duplicate it.
        let t = transition(
            &store,
            &[],
            &with_global(&store, &names(&["global", "work"])),
        )
        .unwrap();
        assert_eq!(t.after, names(&["global", "work"]));

        // And a store without `global` simply has no global.
        let bare = store_with(&[("work", &[], &[("W", "1")])]);
        assert_eq!(with_global(&bare, &names(&["work"])), names(&["work"]));
    }

    /// The bug this design exists to prevent. Activating `child` pulls in
    /// `base`, then the dependency edge is removed, then `off` runs. Expanding
    /// the active roots against the *new* graph no longer mentions `base`, so
    /// deriving the restore set from the graph would leave PAGER set in the
    /// user's shell forever.
    #[test]
    fn off_restores_variables_whose_dependency_edge_was_removed() {
        let mut store = store_with(&[
            ("global", &[], &[("G", "1")]),
            ("base", &[], &[("PAGER", "less")]),
            ("child", &["base"], &[("C", "1")]),
        ]);
        let active = with_global(&store, &names(&["child"]));
        let saved = applied_vars(&store, &active);
        assert_eq!(saved, names(&["C", "G", "PAGER"]));

        // `ep unrequire child base`: the graph moves under the live session.
        crate::graph::remove_requires(&mut store, "child", "base").unwrap();

        let t = teardown(&store, &saved).unwrap();
        assert_eq!(
            t.restore,
            names(&["C", "G", "PAGER"]),
            "a variable must not be left behind just because its profile stopped \
             being reachable"
        );
    }

    /// The same hazard with the profile deleted outright. Its sentinels are
    /// still exported, so its variables are still restored — the profile names
    /// in `ENVPICK_ACTIVE` being stale must not cost the user a rollback.
    #[test]
    fn a_deleted_profile_still_has_its_variables_restored() {
        let store = store_with(&[("global", &[], &[("G", "1")])]);
        let t = teardown(&store, &names(&["G", "W"])).unwrap();
        assert_eq!(t.after, Vec::<String>::new());
        assert_eq!(t.restore, names(&["G", "W"]));
    }

    /// A stale name in `ENVPICK_ACTIVE` must not wedge the shell: `off` does not
    /// resolve profile names at all any more.
    #[test]
    fn teardown_works_with_no_profiles_left() {
        let store = ProfileStore::default();
        let t = teardown(&store, &names(&["GONE"])).unwrap();
        assert_eq!(t.restore, names(&["GONE"]));
        assert_eq!(t.active_value(), "");
    }

    /// Only our own sentinels count. Anything else in the environment — the
    /// original-value records, `ENVPICK_ACTIVE`, unrelated variables — must not
    /// be mistaken for a variable to restore, or `off` would clobber things the
    /// user set themselves.
    #[test]
    fn saved_vars_reads_only_the_bookkeeping_sentinels() {
        let env = [
            ("ENVPICK_SAVED_EDITOR", "1"),
            ("ENVPICK_SAVED_PAGER", "1"),
            ("ENVPICK_ACTIVE", "global,work"),
            ("ENVPICK_ORIG_EDITOR", "vim"),
            ("ENVPICK_HAD_EDITOR", "1"),
            ("ENVPICK_SAVED_", "1"), // empty name: not a variable
            ("EDITOR", "nvim"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()));
        assert_eq!(parse_saved(env.into_iter()), names(&["EDITOR", "PAGER"]));
        assert!(parse_saved(std::iter::empty()).is_empty());
    }

    #[test]
    fn a_cycle_is_reported() {
        let store = store_with(&[("a", &["b"], &[]), ("b", &["a"], &[])]);
        let err = transition(&store, &[], &names(&["a"])).unwrap_err();
        assert!(err.to_string().contains('a'), "got: {err}");
    }

    #[test]
    fn quote_escapes_embedded_single_quotes() {
        assert_eq!(quote("plain"), "'plain'");
        assert_eq!(quote("it's"), r#"'it'\''s'"#);
        assert_eq!(quote(""), "''");
        // Command substitution and globs must stay literal.
        assert_eq!(quote("$(rm -rf /)"), "'$(rm -rf /)'");
        assert_eq!(quote("*"), "'*'");
    }
}
