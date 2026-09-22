//! Commands that change the environment.
//!
//! These print POSIX shell code on stdout for the calling shell to `eval`; a
//! child process cannot modify its parent's environment. Anything else they
//! want to say goes to stderr, which keeps stdout a clean script.

use std::io::IsTerminal;

use anyhow::{Result, bail};

use crate::config::ProfileStore;
use crate::handlers::Ctx;
use crate::shell::{self, Transition};
use crate::text;

/// Print the integration script, and the reason it has to be evaluated.
pub fn init(shell_name: &str) -> Result<()> {
    let Some(shell) = shell::Shell::parse(shell_name) else {
        bail!("不支持的 shell: {shell_name}（只支持 zsh 和 bash）");
    };
    print!("{}", shell::init_script(shell));
    Ok(())
}

/// Entry point for `envpick __shell ...`, reached only through the `ep`
/// function. Nothing here is guarded, because the contract is explicit.
pub fn dispatch(ctx: &Ctx, args: &[String]) -> Result<()> {
    let (command, rest) = args
        .split_first()
        .map_or(("", &[][..]), |(c, r)| (c.as_str(), r));
    match command {
        "use" => {
            if rest.is_empty() {
                bail!("用法：ep use <profile>...");
            }
            use_profiles(ctx, rest, true)
        }
        "unuse" => {
            let all = rest.iter().any(|a| a == "--all");
            let names: Vec<String> = rest.iter().filter(|a| *a != "--all").cloned().collect();
            unuse_profiles(ctx, &names, all, true)
        }
        "off" => off(ctx, true),
        // Runs at shell start, from the integration script. Only *adds* the
        // configured defaults: a new terminal inherits its parent's exported
        // environment, and deactivating anything here would strand variables
        // this shell has no saved original for.
        "__init" => activate_defaults(ctx),
        other => bail!("未知的内部命令: {other}"),
    }
}

fn activate_defaults(ctx: &Ctx) -> Result<()> {
    let current = shell::active_profiles();
    let after = shell::with_global(
        &ctx.store,
        &shell::add_roots(&current, &ctx.settings.default_profiles),
    );
    let t = shell::transition(&ctx.store, &shell::saved_vars(), &after)?;
    print!("{}", shell::emit(&t));
    Ok(())
}

pub fn use_profiles(ctx: &Ctx, requested: &[String], explicit: bool) -> Result<()> {
    require_known(&ctx.store, requested)?;

    let current = shell::active_profiles();
    let after = shell::with_global(&ctx.store, &shell::add_roots(&current, requested));
    let t = shell::transition(&ctx.store, &shell::saved_vars(), &after)?;

    emit(&t, explicit)?;

    // Feedback goes to stderr so it never lands inside the evaluated script.
    eprintln!(
        "{}: {}",
        text::messages::ACTIVATED,
        t.active_value().replace(',', ", ")
    );
    // Dependencies are not in `ENVPICK_ACTIVE` (they are re-expanded on each
    // activation), so they would otherwise be invisible at the one moment the
    // user is looking to see what just happened to their environment.
    let indirect = shell::indirect_active(&ctx.store, &t.after);
    if !indirect.is_empty() {
        eprintln!("另含依赖: {}", indirect.join(", "));
    }
    Ok(())
}

pub fn unuse_profiles(ctx: &Ctx, requested: &[String], all: bool, explicit: bool) -> Result<()> {
    if !all && requested.is_empty() {
        bail!("用法：ep unuse <profile>... 或 ep unuse --all");
    }

    let current = shell::active_profiles();
    // `--all` is a full teardown, which deactivates `global` as well — that is
    // what "off" means. Removing named profiles keeps everything else.
    let after = if all {
        Vec::new()
    } else {
        shell::remove_roots(&current, requested)
    };

    let t = shell::transition(&ctx.store, &shell::saved_vars(), &after)?;
    emit(&t, explicit)?;
    eprintln!("{}", text::messages::DEACTIVATED);
    Ok(())
}

pub fn off(ctx: &Ctx, explicit: bool) -> Result<()> {
    // Deliberately not derived from `ENVPICK_ACTIVE`: the variables to undo are
    // the ones actually modified, which is a fact about this session rather than
    // about whatever the configuration says now.
    let t = shell::teardown(&ctx.store, &shell::saved_vars())?;
    emit(&t, explicit)?;
    eprintln!("{}", text::messages::DEACTIVATED);
    Ok(())
}

/// A name the user typed should be checked; a name recorded in
/// `ENVPICK_ACTIVE` should not, since `Transition` self-heals stale ones.
fn require_known(store: &ProfileStore, names: &[String]) -> Result<()> {
    for name in names {
        if !store.contains(name) {
            bail!("{}: '{name}'", text::errors::PROFILE_NOT_FOUND);
        }
    }
    Ok(())
}

/// Print the script, or explain that nothing will run it.
///
/// `eval "$(...)"` makes stdout a pipe, so a terminal means the output is going
/// straight to the user's eyes and no shell will ever evaluate it. Saying so is
/// much more useful than printing a script that silently does nothing.
fn emit(t: &Transition, explicit: bool) -> Result<()> {
    if !explicit && std::io::stdout().is_terminal() {
        bail!(
            "{}\n{}",
            text::messages::NEED_SHELL_INIT,
            text::messages::NEED_SHELL_INIT_HINT
        );
    }
    print!("{}", shell::emit(t));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::names;

    fn ctx_with(entries: &[crate::testing::ProfileSpec<'_>]) -> Ctx {
        let store = crate::testing::store_with(entries);
        Ctx {
            paths: crate::config::Paths::resolve().unwrap(),
            settings: crate::config::Settings::default(),
            store,
        }
    }

    /// A typo must fail loudly. Dropping it silently would look like a
    /// successful activation that set nothing.
    #[test]
    fn an_unknown_profile_is_an_error() {
        let ctx = ctx_with(&[("work", &[], &[("A", "1")])]);
        let err = use_profiles(&ctx, &names(&["wrok"]), true).unwrap_err();
        assert!(err.to_string().contains("wrok"), "got: {err}");
    }

    /// The whole point of the dispatch table: the shell function's three
    /// environment-changing verbs all resolve to something.
    #[test]
    fn the_internal_dispatch_covers_the_shell_verbs() {
        let ctx = ctx_with(&[("global", &[], &[("G", "1")]), ("work", &[], &[("W", "1")])]);
        for args in [
            names(&["use", "work"]),
            names(&["unuse", "work"]),
            names(&["unuse", "--all"]),
            names(&["off"]),
            names(&["__init"]),
        ] {
            assert!(dispatch(&ctx, &args).is_ok(), "{args:?} should be handled");
        }
        assert!(dispatch(&ctx, &names(&["bogus"])).is_err());
    }

    #[test]
    fn unuse_without_arguments_is_rejected() {
        let ctx = ctx_with(&[("work", &[], &[("A", "1")])]);
        assert!(unuse_profiles(&ctx, &[], false, true).is_err());
        // `--all` alone is fine.
        assert!(unuse_profiles(&ctx, &[], true, true).is_ok());
    }
}
