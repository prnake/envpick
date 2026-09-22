//! Inspecting and editing profiles — everything that does not touch the
//! environment of the calling shell.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

use crate::config::{GLOBAL_PROFILE, Profile, is_reserved, validate_new_profile_name};
use crate::graph;
use crate::handlers::Ctx;
use crate::shell;
use crate::text;

/// `value` with control characters made visible, so a multi-line value cannot
/// silently break the table it is printed in.
fn escape_display(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// A variable name can be at most 64 characters and still be importable, but
/// names of 20+ are rare enough that a wider column is just noise.
fn padded(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - len))
    }
}

pub fn list(ctx: &Ctx) -> Result<()> {
    if ctx.store.profiles.is_empty() {
        println!("还没有任何 profile。用 `envpick new <名字>` 新建一个");
        return Ok(());
    }

    let active = shell::active_profiles();
    let indirect = shell::indirect_active(&ctx.store, &active);
    let name_width = ctx
        .store
        .profiles
        .keys()
        .map(|n| n.chars().count())
        .max()
        .unwrap_or(7)
        .max(7);

    println!(
        "{}  {}  {}  状态",
        padded("PROFILE", name_width),
        padded("变量", 4),
        padded("依赖", 4),
    );
    for (name, profile) in &ctx.store.profiles {
        let mark = if active.iter().any(|a| a == name) {
            "激活中"
        } else if indirect.contains(name) {
            "激活中（依赖）"
        } else {
            ""
        };
        println!(
            "{}  {}  {}  {}",
            padded(name, name_width),
            padded(&profile.vars.len().to_string(), 4),
            padded(&profile.requires.len().to_string(), 4),
            mark
        );
    }
    Ok(())
}

pub fn show(ctx: &Ctx, name: &str, plain: bool) -> Result<()> {
    let profile = ctx
        .store
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("{}: '{name}'", text::errors::PROFILE_NOT_FOUND))?;

    let order = graph::resolve_order(&ctx.store, &[name.to_string()])?;

    if !plain {
        println!("# {name}");
        if !profile.requires.is_empty() {
            println!("# requires: {}", profile.requires.join(", "));
        }
        if order.len() > 1 {
            // The order is the activation order, so showing it explains which
            // profile wins a conflict.
            println!("# 激活顺序: {}", order.join(" -> "));
        }
    }

    // Later profiles override earlier ones, so track the last writer of each
    // variable and show where the winning value came from.
    let mut resolved: BTreeMap<String, (String, String)> = BTreeMap::new();
    for profile_name in &order {
        let Some(p) = ctx.store.get(profile_name) else {
            continue;
        };
        for (k, v) in &p.vars {
            resolved.insert(k.clone(), (v.clone(), profile_name.clone()));
        }
    }

    if resolved.is_empty() {
        println!("# （没有变量）");
        return Ok(());
    }

    let width = resolved
        .keys()
        .map(|k| k.chars().count())
        .max()
        .unwrap_or(0);
    for (key, (value, from)) in &resolved {
        if plain {
            println!("{key}={}", escape_display(value));
        } else {
            println!("{key}={}  # {from}", quote_display(key, value));
            let _ = width;
        }
    }
    Ok(())
}

/// Values are shown the way they will be exported, so what you read is what the
/// shell will end up with — including for values with spaces or quotes.
fn quote_display(_key: &str, value: &str) -> String {
    shell::quote(value)
}

pub fn status(ctx: &Ctx) -> Result<()> {
    let active = shell::active_profiles();

    println!("配置目录  {}", ctx.paths.root.display());
    println!(
        "设备 ID   {}{}",
        ctx.settings.device_id,
        match &ctx.settings.device_name {
            Some(n) => format!(" ({n})"),
            None => String::new(),
        }
    );

    if active.is_empty() {
        println!("激活      （无）");
    } else {
        // Resolving gives the real variable count, and surfaces a broken
        // dependency graph right where the user is looking.
        match shell::transition(&ctx.store, &shell::saved_vars(), &active) {
            Ok(t) => {
                println!(
                    "激活      {}   （{} 个变量）",
                    active.join(", "),
                    t.set.len()
                );
                // Dependencies contribute variables too, so naming them keeps
                // the count above from looking like it does not add up.
                let indirect = shell::indirect_active(&ctx.store, &active);
                if !indirect.is_empty() {
                    println!("          另含依赖 {}", indirect.join(", "));
                }
            }
            Err(e) => println!("激活      {}   （解析失败：{e}）", active.join(", ")),
        }
    }

    println!("profile   共 {} 个", ctx.store.profiles.len());

    match ctx.settings.sync.sync_id.as_deref() {
        Some(id) => println!("同步 ID   {id}（`envpick sync status` 查看详情）"),
        None => println!("同步      未配置（`envpick sync init` 开始）"),
    }
    Ok(())
}

/// Report problems, and with `fix` repair the ones that have an unambiguous
/// repair. Cycles are reported but never guessed at.
pub fn check(ctx: &mut Ctx, fix: bool) -> Result<()> {
    let mut problems = 0usize;
    let mut changed = false;

    // A missing `global` is only cosmetic, but `use` implies it, so creating it
    // keeps later behaviour predictable.
    if !ctx.store.contains(GLOBAL_PROFILE) {
        problems += 1;
        if fix {
            ctx.store.upsert(GLOBAL_PROFILE, Profile::default())?;
            changed = true;
            println!("已创建缺失的 '{GLOBAL_PROFILE}' profile");
        } else {
            println!("缺少 '{GLOBAL_PROFILE}' profile（--fix 可创建）");
        }
    }

    let dangling = ctx.store.prune_dangling_requires();
    if !dangling.is_empty() {
        problems += 1;
        if fix {
            changed = true;
            for (profile, dep) in &dangling {
                println!("已移除悬空依赖: {profile} -> {dep}");
            }
        } else {
            for (profile, dep) in &dangling {
                println!("悬空依赖: {profile} -> {dep}（--fix 可移除）");
            }
        }
    }

    // Every profile is independently activatable, so a cycle anywhere in the
    // graph is a real problem rather than an unreachable corner. Cycles are
    // reported but never guessed at — only the user knows the intended order.
    for name in ctx.store.names() {
        if let Err(e) = graph::resolve_order(&ctx.store, std::slice::from_ref(&name)) {
            problems += 1;
            println!("{e}");
        }
    }

    for (name, profile) in &ctx.store.profiles {
        if profile.is_empty() && name != GLOBAL_PROFILE {
            println!("提示: profile '{name}' 是空的");
        }
    }

    if changed {
        ctx.save_store()?;
    }

    if problems == 0 {
        println!("检查通过");
    } else {
        println!("发现 {problems} 处问题");
        if !fix {
            println!("加上 --fix 可自动修复其中标注的部分");
        }
    }
    Ok(())
}

pub fn edit(ctx: &Ctx) -> Result<()> {
    ctx.paths.ensure_dir()?;
    let file = ctx.paths.profiles_file();
    if !file.exists() {
        ctx.save_store()?;
    }

    // `$EDITOR` may carry arguments (`code -w`), so split rather than treating
    // the whole value as a program name.
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let mut parts = editor.split_whitespace();
    let program = parts
        .next()
        .filter(|p| !p.is_empty())
        .context("$EDITOR 是空的")?;

    let status = std::process::Command::new(program)
        .args(parts)
        .arg(&file)
        .status()
        .with_context(|| format!("无法启动编辑器 {program}"))?;

    if !status.success() {
        bail!("编辑器退出状态为 {status}");
    }

    // Re-read so a syntax error surfaces now, while the user still has the file
    // open and knows what they just typed, rather than at the next activation.
    match crate::config::ProfileStore::load(&file) {
        Ok(store) => {
            println!("已保存 {} 个 profile", store.profiles.len());
            Ok(())
        }
        Err(e) => {
            eprintln!("警告：编辑后的文件无法解析，请修正后再激活 profile");
            Err(e)
        }
    }
}

pub fn new_profile(ctx: &mut Ctx, name: &str) -> Result<()> {
    validate_new_profile_name(name)?;
    if ctx.store.contains(name) {
        bail!("profile 已存在: '{name}'");
    }
    ctx.store.upsert(name, Profile::default())?;
    ctx.save_store()?;
    println!("已创建 profile '{name}'");
    Ok(())
}

pub fn remove(ctx: &mut Ctx, name: &str, yes: bool) -> Result<()> {
    if is_reserved(name) {
        bail!("{}: '{name}'", text::errors::PROFILE_NAME_RESERVED);
    }
    if !ctx.store.contains(name) {
        bail!("{}: '{name}'", text::errors::PROFILE_NOT_FOUND);
    }

    // Deleting a profile others depend on also breaks them, which is the part
    // that is easy to miss — so that is the case that needs confirmation.
    let dependents = graph::dependents_of(&ctx.store, name);
    if !dependents.is_empty() && !yes {
        bail!(
            "以下 profile 依赖 '{name}'，删除会让它们的依赖变成悬空：{}\n确认请加 --yes",
            dependents.join(", ")
        );
    }

    ctx.store.remove(name);
    let pruned = ctx.store.prune_dangling_requires();
    ctx.save_store()?;

    println!("已删除 profile '{name}'");
    for (profile, dep) in &pruned {
        println!("同时移除了悬空依赖: {profile} -> {dep}");
    }
    Ok(())
}

/// `envpick set work EDITOR=nvim PAGER=less`
pub fn set_vars(ctx: &mut Ctx, profile_name: &str, assignments: &[String]) -> Result<()> {
    let mut profile = ctx.store.get(profile_name).cloned().unwrap_or_default();

    for assignment in assignments {
        // Split on the first `=`, so a value may itself contain `=`.
        let Some((key, value)) = assignment.split_once('=') else {
            bail!("参数格式应为 KEY=VALUE: '{assignment}'");
        };
        let key = key.trim();
        crate::config::validate_var_name(key)
            .with_context(|| format!("profile '{profile_name}'"))?;
        profile.vars.insert(key.to_string(), value.to_string());
    }

    ctx.store.upsert(profile_name, profile)?;
    ctx.save_store()?;
    println!("已更新 profile '{profile_name}'");
    Ok(())
}

pub fn unset_vars(ctx: &mut Ctx, profile_name: &str, keys: &[String]) -> Result<()> {
    let Some(profile) = ctx.store.get(profile_name).cloned() else {
        bail!("{}: '{profile_name}'", text::errors::PROFILE_NOT_FOUND);
    };
    let mut profile = profile;

    let mut missing = Vec::new();
    for key in keys {
        if profile.vars.remove(key).is_none() {
            missing.push(key.clone());
        }
    }

    ctx.store.upsert(profile_name, profile)?;
    ctx.save_store()?;

    if !missing.is_empty() {
        // Not an error: the end state is what the user asked for.
        eprintln!(
            "提示：profile '{profile_name}' 本来就没有这些变量: {}",
            missing.join(", ")
        );
    }
    println!("已更新 profile '{profile_name}'");
    Ok(())
}

/// `require` and `unrequire` differ only in which edge operation they apply, so
/// they share one implementation. Both are all-or-nothing: every edge is
/// applied in memory first and the file is written once at the end, so a failure
/// on the third of three dependencies leaves the config exactly as it was.
fn edit_requires(ctx: &mut Ctx, profile_name: &str, deps: &[String], add: bool) -> Result<()> {
    if !ctx.store.contains(profile_name) {
        bail!("{}: '{profile_name}'", text::errors::PROFILE_NOT_FOUND);
    }

    for dep in deps {
        let result = if add {
            crate::graph::add_requires(&mut ctx.store, profile_name, dep)
        } else {
            crate::graph::remove_requires(&mut ctx.store, profile_name, dep)
        };
        // Nothing has been written yet, so returning here discards every change
        // made so far along with the one that failed.
        result?;
    }

    ctx.save_store()?;

    let requires = ctx
        .store
        .get(profile_name)
        .map(|p| p.requires.clone())
        .unwrap_or_default();
    if requires.is_empty() {
        println!("profile '{profile_name}' 现在不依赖任何 profile");
    } else {
        // Show the activation order, not just the direct edges — that is what
        // actually decides which profile's values win.
        match crate::graph::resolve_order(&ctx.store, &[profile_name.to_string()]) {
            Ok(order) => println!("'{profile_name}' 激活顺序: {}", order.join(" -> ")),
            Err(e) => println!("'{profile_name}' 依赖: {}（{e}）", requires.join(", ")),
        }
    }
    Ok(())
}

pub fn require_deps(ctx: &mut Ctx, profile_name: &str, deps: &[String]) -> Result<()> {
    edit_requires(ctx, profile_name, deps, true)
}

pub fn unrequire_deps(ctx: &mut Ctx, profile_name: &str, deps: &[String]) -> Result<()> {
    edit_requires(ctx, profile_name, deps, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Paths;
    use std::path::PathBuf;

    /// A config directory of our own, removed on drop. Tests must never touch
    /// the user's real `~/.config/envpick`, and an environment-variable override
    /// would race between parallel tests.
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let mut buf = [0u8; 6];
            rand::fill(&mut buf);
            let uniq: String = buf.iter().map(|b| format!("{b:02x}")).collect();
            let path = std::env::temp_dir().join(format!("envpick-handler-{uniq}"));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Owns the temp directory for as long as the context needs it.
    struct TestCtx {
        ctx: Ctx,
        _root: TempRoot,
    }

    impl std::ops::Deref for TestCtx {
        type Target = Ctx;
        fn deref(&self) -> &Ctx {
            &self.ctx
        }
    }

    impl std::ops::DerefMut for TestCtx {
        fn deref_mut(&mut self) -> &mut Ctx {
            &mut self.ctx
        }
    }

    fn ctx_with(entries: &[crate::testing::ProfileSpec<'_>]) -> TestCtx {
        let root = TempRoot::new();
        let store = crate::testing::store_with(entries);
        TestCtx {
            ctx: Ctx {
                paths: Paths {
                    root: root.0.clone(),
                },
                settings: crate::config::Settings::default(),
                store,
            },
            _root: root,
        }
    }

    #[test]
    fn escape_display_makes_control_characters_visible() {
        assert_eq!(escape_display("a\nb\tc"), "a\\nb\\tc");
        assert_eq!(escape_display("plain"), "plain");
    }

    #[test]
    fn padded_never_truncates() {
        assert_eq!(padded("ab", 4), "ab  ");
        assert_eq!(padded("abcdef", 4), "abcdef");
    }

    #[test]
    fn set_splits_on_the_first_equals_only() {
        let mut ctx = ctx_with(&[]);
        set_vars(&mut ctx, "work", &["OPTS=--a=b".to_string()]).unwrap();
        assert_eq!(ctx.store.get("work").unwrap().vars["OPTS"], "--a=b");
    }

    #[test]
    fn set_rejects_a_missing_equals_and_a_bad_name() {
        let mut ctx = ctx_with(&[]);
        assert!(set_vars(&mut ctx, "work", &["NOEQUALS".to_string()]).is_err());
        assert!(set_vars(&mut ctx, "work", &["has.dot=1".to_string()]).is_err());
    }

    #[test]
    fn unsetting_a_missing_key_is_not_an_error() {
        let mut ctx = ctx_with(&[("work", &[], &[("A", "1")])]);
        assert!(unset_vars(&mut ctx, "work", &["NOPE".to_string()]).is_ok());
        assert_eq!(ctx.store.get("work").unwrap().vars.len(), 1);
    }

    /// Deleting a profile that something else requires needs confirmation, and
    /// taking it must clean up the dangling reference rather than leave every
    /// dependent broken.
    #[test]
    fn removing_a_required_profile_needs_yes_and_prunes_dependents() {
        let mut ctx = ctx_with(&[("base", &[], &[("A", "1")]), ("work", &["base"], &[])]);
        let err = remove(&mut ctx, "base", false).unwrap_err();
        assert!(err.to_string().contains("work"), "got: {err}");

        remove(&mut ctx, "base", true).unwrap();
        assert!(!ctx.store.contains("base"));
        assert!(ctx.store.get("work").unwrap().requires.is_empty());
    }

    #[test]
    fn reserved_names_cannot_be_created_or_removed() {
        let mut ctx = ctx_with(&[("global", &[], &[])]);
        assert!(new_profile(&mut ctx, "global").is_err());
        assert!(remove(&mut ctx, "global", true).is_err());
    }

    #[test]
    fn resolving_show_picks_the_last_writer() {
        let ctx = ctx_with(&[
            ("base", &[], &[("EDITOR", "vim"), ("ONLY", "b")]),
            ("work", &["base"], &[("EDITOR", "nvim")]),
        ]);
        // `show` writes to stdout; just assert the resolution underneath it.
        let order = graph::resolve_order(&ctx.store, &["work".to_string()]).unwrap();
        let vars = graph::merged_vars(&ctx.store, &order).unwrap();
        assert_eq!(vars["EDITOR"], "nvim");
        assert_eq!(vars["ONLY"], "b");
    }

    /// `--fix` must persist what it repaired, not just report it.
    #[test]
    fn check_fix_creates_global_and_persists_it() {
        let mut t = ctx_with(&[]);
        check(&mut t, true).unwrap();
        assert!(t.store.contains(GLOBAL_PROFILE));

        let reloaded = crate::config::ProfileStore::load(&t.paths.profiles_file()).unwrap();
        assert!(
            reloaded.contains(GLOBAL_PROFILE),
            "the repair was not written to disk"
        );
    }

    /// Without `--fix` nothing may be written — `check` is a read-only command.
    #[test]
    fn check_without_fix_leaves_the_file_alone() {
        let mut t = ctx_with(&[("work", &["ghost"], &[("A", "1")])]);
        check(&mut t, false).unwrap();
        assert!(
            !t.paths.profiles_file().exists(),
            "check without --fix created a file"
        );
    }

    /// A cycle has no unambiguous repair, so `--fix` must leave it intact for
    /// the user to resolve.
    #[test]
    fn check_fix_does_not_touch_a_cycle() {
        let mut t = ctx_with(&[("a", &["b"], &[]), ("b", &["a"], &[])]);
        check(&mut t, true).unwrap();
        assert_eq!(t.store.get("a").unwrap().requires, vec!["b".to_string()]);
        assert_eq!(t.store.get("b").unwrap().requires, vec!["a".to_string()]);
    }
}
