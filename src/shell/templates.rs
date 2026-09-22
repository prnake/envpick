//! The shell integration script, and the shells we support it on.
//!
//! The same POSIX function works in zsh and bash. It exists because the
//! commands that change the environment have to be `eval`ed by the *calling*
//! shell, while everything else should just run the binary — so `ep` dispatches
//! on its first argument.

/// A shell we can install the integration into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Zsh,
    Bash,
}

impl Shell {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "zsh" => Some(Shell::Zsh),
            "bash" => Some(Shell::Bash),
            _ => None,
        }
    }

    /// The file a user would normally add this to, for the hint we print.
    pub fn rc_file(&self) -> &'static str {
        match self {
            Shell::Zsh => "~/.zshrc",
            Shell::Bash => "~/.bashrc",
        }
    }
}

/// The `ep` function, plus the initial activation of the configured default
/// profiles.
///
/// `use`/`unuse`/`off` need the environment of the calling shell, so they are
/// captured and evaluated here; everything else is a plain pass-through, which
/// keeps `ep --help`, pipes and exit codes behaving exactly as if the binary had
/// been called directly.
///
/// The two-step capture in that branch is load-bearing, not decoration. Writing
/// the obvious `eval "$(command envpick __shell "$@")"` loses the exit status:
/// the binary reports a bad profile on stderr and prints nothing on stdout, so
/// the `eval` is handed an empty string — and evaluating empty text *succeeds*.
/// `ep use typo` would then exit 0 while printing an error, and
/// `ep use prod || alert` would sail straight past the failure. Splitting the
/// capture from the `eval` is what keeps the status, and the `return` is what
/// hands it to the caller.
///
/// The trailing `__init` line is what makes `global` active in every shell. It
/// is guarded by `command -v` so a shell whose rc runs before the binary is on
/// `PATH` still loads cleanly instead of printing an error at every startup.
pub fn init_script(shell: Shell) -> String {
    let command = "ep";
    let _ = shell;
    format!(
        r#"# envpick shell integration -- add to your shell rc file:
#   eval "$(envpick init {name})"

{command}() {{
  case "${{1:-}}" in
    use|unuse|off)
      # These must run in this shell: a child process cannot change its
      # parent's environment, so the binary prints code for us to eval.
      local _envpick_code
      _envpick_code="$(command envpick __shell "$@")" || return $?
      eval "$_envpick_code"
      ;;
    *)
      command envpick "$@"
      ;;
  esac
}}

# Activate `global` and the configured default profiles in every new shell.
if command -v envpick >/dev/null 2>&1; then
  eval "$(command envpick __shell __init)"
fi
"#,
        name = match shell {
            Shell::Zsh => "zsh",
            Shell::Bash => "bash",
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_supported_shells() {
        assert_eq!(Shell::parse("zsh"), Some(Shell::Zsh));
        assert_eq!(Shell::parse("bash"), Some(Shell::Bash));
        assert_eq!(Shell::parse("fish"), None);
        assert_eq!(Shell::parse(""), None);
    }

    #[test]
    fn the_script_defines_ep_and_dispatches_the_environment_commands() {
        let script = init_script(Shell::Zsh);
        assert!(script.contains("ep() {"));
        for cmd in [
            "use|unuse|off",
            "command envpick __shell",
            "command envpick \"$@\"",
        ] {
            assert!(script.contains(cmd), "script missing {cmd}:\n{script}");
        }
        assert!(script.contains("init zsh"));
        assert!(init_script(Shell::Bash).contains("init bash"));
    }

    /// `global` has to come up in every shell, and `use` cannot imply it on its
    /// own — activation only happens on `use`, so the rc file needs its own
    /// activation step.
    #[test]
    fn the_script_activates_default_profiles_at_startup() {
        let script = init_script(Shell::Bash);
        assert!(
            script.contains("__shell __init"),
            "no startup activation:\n{script}"
        );
        // Guarded, so an rc file that runs before `envpick` is on PATH still
        // loads without printing an error on every shell start.
        assert!(script.contains("command -v envpick"));
    }

    /// The function must survive `set -u`, since it tests `$1`. As a bonus this
    /// covers the `command -v` guard being skipped when envpick is absent, which
    /// is exactly the situation in this test process.
    #[test]
    fn the_script_runs_under_set_u() {
        let script = init_script(Shell::Bash);
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("set -u\n{script}\nprintf 'ok\\n'"))
            .output()
            .expect("sh should be available");
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "ok\n");
    }

    /// `ep use typo` must not exit 0.
    ///
    /// The binary reports the failure on stderr and prints nothing on stdout, so
    /// the `eval` is handed an empty string — and evaluating empty text
    /// succeeds. Without an explicit `return $?`, the function would end on that
    /// successful `eval` and report success, which breaks `ep use prod || alert`
    /// and every `set -e` script around it.
    ///
    /// The stub stands in for the binary so the test pins the *function's*
    /// behaviour rather than the handler's, and the environment commands here
    /// are exactly the ones the real binary never fails on — so the assertions
    /// cannot pass because of something else.
    #[test]
    fn a_failing_environment_command_reports_its_status() {
        let script = format!(
            r#"set -u
mkdir -p "$TMPDIR_STUB"
cat > "$TMPDIR_STUB/envpick" <<'STUB'
#!/bin/sh
printf 'boom\n' >&2
exit 3
STUB
chmod +x "$TMPDIR_STUB/envpick"
PATH="$TMPDIR_STUB:$PATH"
{}

ep use work >/dev/null 2>&1
printf 'failure=%s\n' "$?"
ep --help >/dev/null 2>&1
printf 'passthrough=%s\n' "$?"
"#,
            init_script(Shell::Bash)
        );

        let dir = std::env::temp_dir().join(format!("envpick-stub-{}", std::process::id()));
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .env("TMPDIR_STUB", &dir)
            .output()
            .expect("sh should be available");
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            // 3 from the stub, through the function. The pass-through branch
            // must keep working too -- a `return` outside the case would have
            // swallowed this one.
            "failure=3\npassthrough=3\n"
        );
    }
}
