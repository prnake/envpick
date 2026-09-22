//! Generating the shell code itself.
//!
//! Everything here is POSIX `sh`; zsh and bash both run it unchanged. The two
//! things that make it correct rather than merely plausible:
//!
//! - values are single-quoted, so `$`, backticks and globs stay literal;
//! - presence is tested with `${x+y}`, so the code behaves the same under
//!   `set -u` and can still tell "empty" from "absent".

use crate::shell::Transition;

/// Quote a value for a POSIX shell. Inside single quotes every character is
/// literal, so the only case to handle is the quote itself — which is closed,
/// escaped, and reopened.
pub fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// The three bookkeeping variables tracked per environment variable. The
/// sentinel's spelling comes from [`crate::shell::SAVED_PREFIX`], because the
/// binary reads those names back out of the environment to decide what a
/// deactivation must undo.
fn saved(name: &str) -> String {
    format!("{}{name}", crate::shell::SAVED_PREFIX)
}
fn orig(name: &str) -> String {
    format!("ENVPICK_ORIG_{name}")
}
fn had(name: &str) -> String {
    format!("ENVPICK_HAD_{name}")
}

/// Record the original value of `name`, once. Later activations see the
/// sentinel and leave the record alone, which is what makes repeated
/// `ep use` calls idempotent.
///
/// The sentinel is *exported* on purpose: it is the list of variables this
/// session has modified, and a later `envpick` invocation reads it back to
/// decide what a deactivation has to undo. See [`crate::shell::saved_vars`].
/// `ORIG` and `HAD` stay unexported — only the shell that restores from them
/// needs to read them, and the original value is not something to hand to every
/// child process.
fn save_snippet(name: &str) -> String {
    let (s, o, h) = (saved(name), orig(name), had(name));
    format!(
        "\
if [ -z \"${{{s}+x}}\" ]; then
  export {s}=1
  if [ -n \"${{{name}+x}}\" ]; then
    {o}=\"${name}\"
    {h}=1
  else
    {h}=
  fi
fi"
    )
}

/// Put `name` back the way it was: the saved value if it existed, `unset` if it
/// did not, and drop the bookkeeping either way.
fn restore_snippet(name: &str) -> String {
    let (s, o, h) = (saved(name), orig(name), had(name));
    format!(
        "\
if [ -n \"${{{s}+x}}\" ]; then
  if [ -n \"${{{h}:-}}\" ]; then
    export {name}=\"${o}\"
  else
    unset {name}
  fi
  unset {s} {o} {h}
fi"
    )
}

/// The complete script for one transition: restores, then assignments, then
/// the new active list.
pub fn emit(t: &Transition) -> String {
    let mut parts = Vec::new();

    for name in &t.restore {
        parts.push(restore_snippet(name));
    }
    for (name, value) in &t.set {
        parts.push(save_snippet(name));
        parts.push(format!("export {name}={}", quote(value)));
    }
    parts.push(format!(
        "export {}={}",
        crate::shell::ENV_ACTIVE,
        quote(&t.active_value())
    ));

    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProfileStore;
    use crate::shell::{applied_vars, teardown, transition, with_global};
    use crate::testing::names;

    fn store_with(entries: &[crate::testing::ProfileSpec<'_>]) -> ProfileStore {
        crate::testing::store_with(entries)
    }

    /// Run a script in a real POSIX shell, failing loudly on a non-zero exit so
    /// that a `set -u` violation surfaces as a test failure rather than as
    /// mysteriously empty output.
    fn run_sh(script: &str) -> String {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .output()
            .expect("sh should be available");
        assert!(
            out.status.success(),
            "shell exited {:?}\n--- stderr ---\n{}\n--- script ---\n{script}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr),
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    #[test]
    fn activation_and_restore_round_trip_through_a_real_shell() {
        let store = store_with(&[
            (
                "global",
                &[],
                &[("EDITOR", "nvim"), ("WAS_EMPTY", "filled")],
            ),
            ("work", &["global"], &[("NEWVAR", "hello")]),
        ]);
        // `use` implies global, exactly as the handler will compute it.
        let roots = with_global(&store, &names(&["work"]));
        let activate = emit(&transition(&store, &[], &roots).unwrap());
        // `saved` stands in for the sentinels a real shell would be holding.
        let deactivate = emit(&teardown(&store, &applied_vars(&store, &roots)).unwrap());

        // `set -u` is the point: the emitted code must not reference unset
        // variables while deciding what to save.
        let script = format!(
            r#"set -u
export EDITOR=vim
export WAS_EMPTY=
export UNTOUCHED=keep
{activate}
printf 'active=%s|%s|%s|%s\n' "$EDITOR" "$WAS_EMPTY" "$NEWVAR" "${{ENVPICK_ACTIVE-}}"
{deactivate}
printf 'restored=%s|%s|%s|%s\n' "$EDITOR" "${{WAS_EMPTY+set}}" "${{NEWVAR+set}}" "$UNTOUCHED"
"#
        );

        assert_eq!(
            run_sh(&script),
            "active=nvim|filled|hello|global,work\nrestored=vim|set||keep\n"
        );
    }

    /// A value that would be mangled by naive quoting: single quotes, command
    /// substitution and a glob.
    #[test]
    fn awkward_values_survive_the_shell() {
        let store = store_with(&[("global", &[], &[("TRICKY", "it's $(echo bad) * `x`")])]);
        let roots = names(&["global"]);
        let activate = emit(&transition(&store, &[], &roots).unwrap());
        let deactivate = emit(&teardown(&store, &applied_vars(&store, &roots)).unwrap());

        let script = format!(
            "set -u\n{activate}\nprintf 'tricky=%s\\n' \"$TRICKY\"\n{deactivate}\nprintf 'gone=%s\\n' \"${{TRICKY+set}}\"\n"
        );

        assert_eq!(run_sh(&script), "tricky=it's $(echo bad) * `x`\ngone=\n");
    }

    /// Deactivating twice, or activating twice, must not lose the original.
    #[test]
    fn repeated_activation_does_not_overwrite_the_saved_original() {
        let store = store_with(&[("global", &[], &[("EDITOR", "nvim")])]);
        let roots = names(&["global"]);
        let activate = emit(&transition(&store, &[], &roots).unwrap());
        let deactivate = emit(&teardown(&store, &applied_vars(&store, &roots)).unwrap());

        let script = format!(
            r#"set -u
export EDITOR=vim
{activate}
{activate}
printf 'mid=%s\n' "$EDITOR"
{deactivate}
printf 'end=%s\n' "$EDITOR"
{deactivate}
printf 'twice=%s\n' "$EDITOR"
"#
        );

        assert_eq!(run_sh(&script), "mid=nvim\nend=vim\ntwice=vim\n");
    }

    /// `saved_vars` reads these back out of the environment, so they have to be
    /// genuinely exported. An unexported sentinel is invisible to every later
    /// `envpick` invocation, which would leave deactivation with no record of
    /// what to undo.
    #[test]
    fn activation_exports_the_sentinels_a_later_invocation_reads() {
        let store = store_with(&[("global", &[], &[("EDITOR", "nvim"), ("PAGER", "less")])]);
        let activate = emit(&transition(&store, &[], &names(&["global"])).unwrap());

        // `env` is a child process, so it only sees exported variables.
        let script =
            format!("set -u\n{activate}\nenv | grep '^ENVPICK_SAVED_' | sort | tr '\\n' ' '\n");
        assert_eq!(
            run_sh(&script),
            "ENVPICK_SAVED_EDITOR=1 ENVPICK_SAVED_PAGER=1 "
        );
    }

    /// The leak, end to end in a real shell: activate a profile that pulls in a
    /// dependency, remove that edge, then deactivate. The dependency's variables
    /// must still be restored rather than left in the user's environment.
    #[test]
    fn deactivation_after_the_graph_changed_still_restores() {
        let mut store = store_with(&[
            ("global", &[], &[("G", "1")]),
            ("base", &[], &[("PAGER", "less")]),
            ("child", &["base"], &[("C", "1")]),
        ]);
        let roots = with_global(&store, &names(&["child"]));
        let activate = emit(&transition(&store, &[], &roots).unwrap());
        let saved = applied_vars(&store, &roots);

        // `ep unrequire child base`: the graph moves under the live session.
        crate::graph::remove_requires(&mut store, "child", "base").unwrap();
        let deactivate = emit(&teardown(&store, &saved).unwrap());

        let script = format!(
            "set -u\n{activate}\nprintf 'on=%s|%s\\n' \"$PAGER\" \"$C\"\n{deactivate}\n\
             printf 'off=%s|%s|%s\\n' \"${{PAGER+set}}\" \"${{C+set}}\" \"${{G+set}}\"\n"
        );
        assert_eq!(run_sh(&script), "on=less|1\noff=||\n");
    }

    /// Switching profiles must re-point a shared variable at whichever profile
    /// still sets it, not restore the original.
    #[test]
    fn switching_profiles_reassigns_shared_variables() {
        let store = store_with(&[
            ("global", &[], &[("SHARED", "global"), ("ONLY_GLOBAL", "g")]),
            (
                "work",
                &["global"],
                &[("SHARED", "work"), ("ONLY_WORK", "w")],
            ),
        ]);
        let activate_roots = with_global(&store, &names(&["work"]));
        let activate = emit(&transition(&store, &[], &activate_roots).unwrap());
        // `ep unuse work`: global stays active, so only work's own variable is
        // restored while the shared one is re-pointed at global.
        let leave_work = emit(
            &transition(
                &store,
                &applied_vars(&store, &activate_roots),
                &names(&["global"]),
            )
            .unwrap(),
        );

        let script = format!(
            r#"set -u
export SHARED=original
{activate}
printf 'work=%s|%s|%s\n' "$SHARED" "$ONLY_WORK" "$ONLY_GLOBAL"
{leave_work}
printf 'back=%s|%s\n' "$SHARED" "${{ONLY_WORK+set}}"
"#
        );

        assert_eq!(run_sh(&script), "work=work|w|g\nback=global|\n");
    }
}
