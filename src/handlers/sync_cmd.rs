//! The `sync` command group.
//!
//! Everything here is a thin wrapper over [`SyncEngine`]: the engine owns the
//! state machine and the crypto, and these functions only decide what to print
//! and when to ask before doing something irreversible.

use anyhow::{Context, Result, bail};

use crate::config::{Settings, validate_sync_id};
use crate::handlers::Ctx;
use crate::sync::{RemoteState, SyncEngine, SyncOutcome, SyncState, SyncStatus};
use crate::text;

/// Build an engine from the loaded context. The engine takes ownership of the
/// settings and persists them itself after a successful transfer.
fn engine(ctx: &Ctx) -> Result<SyncEngine> {
    SyncEngine::new(ctx.paths.clone(), ctx.settings.clone())
}

pub fn human_duration(secs: i64) -> String {
    if secs <= 0 {
        return "已过期".to_string();
    }
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    if days > 0 {
        format!("{days} 天 {hours} 小时")
    } else if hours > 0 {
        format!("{hours} 小时")
    } else {
        format!("{} 分钟", secs / 60)
    }
}

pub fn status(ctx: &Ctx) -> Result<()> {
    if ctx.settings.sync.sync_id.is_none() {
        println!("{}", text::errors::SYNC_NOT_CONFIGURED);
        println!("用 `envpick sync init` 配置同步 ID 与密钥");
        return Ok(());
    }
    if ctx.settings.sync.effective_key().is_none() {
        println!("{}", text::errors::SYNC_KEY_MISSING);
        return Ok(());
    }

    let engine = engine(ctx)?;
    let st = engine.status()?;

    println!("端点      {}", ctx.settings.sync.endpoint);
    println!(
        "同步 ID   {}  (paste: {})",
        ctx.settings.sync.sync_id.as_deref().unwrap_or("-"),
        ctx.settings.sync.paste_name()?
    );
    println!("状态      {}", st.state.label());
    println!("本地      {} 个 profile", st.local_profiles);

    match &st.remote {
        RemoteState::Missing => {}
        RemoteState::Synced(rev) => println!("远端      修订 r{rev}"),
        // Deliberately not "上次同步的是 rN": the revision may well be the
        // same number, because detection compares content, not numbers.
        RemoteState::Moved(rev) => println!("远端      修订 r{rev}（内容与上次同步的不同）"),
        RemoteState::Unreadable => {
            println!("远端      存在，但无法解密为本工具的数据");
        }
    }

    if let Some(expire_at) = &st.remote_expires_at {
        match st.seconds_until_expiry() {
            Some(secs) if secs > 0 => {
                println!(
                    "          过期于 {expire_at}（剩余 {}）",
                    human_duration(secs)
                )
            }
            Some(_) => println!("          已于 {expire_at} 过期"),
            None => println!("          过期于 {expire_at}"),
        }
    }

    if let Some(at) = &ctx.settings.sync.last_synced_at {
        println!("上次同步  {at}");
    }

    warn_if_expiring_soon(&st, &ctx.settings);

    match st.state {
        SyncState::RemoteMissing => {
            println!("\n远端还没有这份数据，`envpick sync push` 创建它");
        }
        SyncState::LocalAhead => println!("\n本地有未推送的改动：`envpick sync push`"),
        SyncState::RemoteAhead => println!("\n远端有新的改动：`envpick sync pull`"),
        SyncState::Diverged => {
            println!("\n两边都有改动，需要你决定保留哪一边：`envpick ui`，或");
            println!("  envpick sync --keep-local    保留本地并覆盖远端");
            println!("  envpick sync --keep-remote   用远端覆盖本地");
        }
        SyncState::RemoteUnreadable => {
            println!("\n{}", text::errors::REMOTE_UNREADABLE);
        }
        SyncState::UpToDate | SyncState::Unconfigured => {}
    }
    Ok(())
}

/// Pastes expire. Say so before it happens, not after, because the failure mode
/// is silent: the next push would just create a fresh paste and the history is
/// gone.
fn warn_if_expiring_soon(st: &SyncStatus, settings: &Settings) {
    let (Some(remaining), Some(total)) = (
        st.seconds_until_expiry(),
        settings.sync.remote_expiration_seconds,
    ) else {
        return;
    };
    if total > 0 && remaining * 4 < total as i64 {
        println!(
            "⚠ 远端 paste 即将过期（剩余 {}），推送一次即可续期",
            human_duration(remaining)
        );
    }
}

pub fn push(ctx: &Ctx) -> Result<()> {
    let mut engine = engine(ctx)?;
    match engine.push()? {
        SyncOutcome::Pushed { revision } => {
            println!("{}（修订 r{revision}）", text::messages::PUSHED);
            Ok(())
        }
        _ => unreachable!("push 只会返回 Pushed"),
    }
}

pub fn pull(ctx: &Ctx) -> Result<()> {
    let mut engine = engine(ctx)?;
    match engine.pull()? {
        SyncOutcome::Pulled { revision } => {
            println!("{}（修订 r{revision}）", text::messages::PULLED);
            Ok(())
        }
        _ => unreachable!("pull 只会返回 Pulled"),
    }
}

/// `envpick sync` with no subcommand: do whatever the current state calls for,
/// and refuse to guess when both sides changed.
pub fn sync_smart(ctx: &Ctx, keep_local: bool, keep_remote: bool) -> Result<()> {
    let mut engine = engine(ctx)?;
    match engine.sync()? {
        SyncOutcome::UpToDate => {
            println!("{}", text::messages::UP_TO_DATE);
            Ok(())
        }
        SyncOutcome::Pushed { revision } => {
            println!("{}（修订 r{revision}）", text::messages::PUSHED);
            Ok(())
        }
        SyncOutcome::Pulled { revision } => {
            println!("{}（修订 r{revision}）", text::messages::PULLED);
            Ok(())
        }
        SyncOutcome::Conflict { local, remote } => {
            if keep_local {
                engine.push()?;
                println!("冲突已按「保留本地」解决，远端已被覆盖");
                return Ok(());
            }
            if keep_remote {
                engine.pull()?;
                println!("冲突已按「保留远端」解决，本地已被覆盖");
                return Ok(());
            }

            // Neither side is silently preferred. Print enough to decide, then
            // exit non-zero so a script can notice that nothing happened.
            println!("{}", text::prompts::CONFLICT_TITLE);
            println!("{}", text::prompts::CONFLICT_BODY);
            println!();
            println!("本地  {}", local.summary());
            println!("远端  {}", remote.summary());
            if !remote.device_id.is_empty() && remote.device_id != local.device_id {
                let who = remote.device_name.as_deref().unwrap_or(&remote.device_id);
                println!("      远端来自 {who}");
            }
            println!();
            println!("  envpick ui                   在 TUI 里逐项对比");
            println!("  envpick sync --keep-local    保留本地并覆盖远端");
            println!("  envpick sync --keep-remote   用远端覆盖本地");
            std::process::exit(2);
        }
    }
}

pub fn genid() -> Result<()> {
    println!("{}", new_sync_id());
    Ok(())
}

/// 15 random bytes → 20 base64url characters, comfortably inside the 3..=64
/// limit and made only of characters that need no escaping in a URL path.
fn new_sync_id() -> String {
    use base64::Engine;
    let mut buf = [0u8; 15];
    rand::fill(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

pub fn init(ctx: &mut Ctx, id: Option<&str>, key: Option<String>, key_stdin: bool) -> Result<()> {
    let id = match id {
        Some(id) => {
            validate_sync_id(id)?;
            id.to_string()
        }
        None => new_sync_id(),
    };

    // Changing the id points at a different paste, so everything we believed
    // about the old remote is meaningless.
    if ctx.settings.sync.sync_id.as_deref() != Some(&id) {
        ctx.settings.sync.forget_remote_state();
    }
    ctx.settings.sync.sync_id = Some(id.clone());

    let key = if key_stdin {
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .context("从标准输入读取密钥失败")?;
        Some(line.trim_end_matches(['\n', '\r']).to_string())
    } else {
        key
    };
    if let Some(key) = key {
        if key.is_empty() {
            bail!("密钥不能为空");
        }
        ctx.settings.sync.key = Some(key);
    }

    if ctx.settings.sync.effective_key().is_none() {
        bail!(
            "{}\n用 `envpick sync init --key-stdin` 从标准输入读入（避免留在 shell 历史里），\
             或设置 {} 环境变量",
            text::errors::SYNC_KEY_MISSING,
            crate::config::ENV_SYNC_KEY
        );
    }

    // Derive once here so an unusable id or key fails now rather than at the
    // first sync, when the user is not looking at the config.
    let key = ctx.settings.sync.effective_key().unwrap();
    crate::sync::SyncCrypto::derive(&key, &id)?;

    ctx.save_settings()?;

    println!(
        "同步 ID 已设为 {id}（paste: {}）",
        ctx.settings.sync.paste_name()?
    );
    println!("配置已写入 {}", ctx.paths.settings_file().display());
    println!("\n在另一台机器上执行同样的 `envpick sync init {id}` 并输入相同密钥即可同步");
    println!("密钥不会离开本机（只以密文形式上传）");
    Ok(())
}

pub fn url(ctx: &Ctx) -> Result<()> {
    let engine = engine(ctx)?;
    println!("{}", engine.browser_url()?);
    eprintln!();
    eprintln!("⚠ 上面链接的 # 后面是加密密钥，浏览器不会把它发给服务器，");
    eprintln!("  但拿到这个链接的人就能解密内容，请勿公开分享。");
    eprintln!();
    eprintln!("仅用于覆盖/删除（不含加密密钥）:");
    eprintln!("  {}", engine.manage_url()?);
    Ok(())
}

pub fn delete(ctx: &mut Ctx, yes: bool) -> Result<()> {
    if !yes {
        bail!(
            "这会删除远端的 paste，其他机器将无法再同步到它（本地 profile 保留）。\n\
             确认请加 --yes"
        );
    }
    let mut engine = engine(ctx)?;
    engine.delete_remote()?;
    // The engine updated its own copy; mirror that so nothing downstream uses
    // the stale remote bookkeeping.
    ctx.settings.sync.forget_remote_state();
    println!("已删除远端 paste，本地 profile 未改动");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally() {
        assert_eq!(human_duration(0), "已过期");
        assert_eq!(human_duration(-5), "已过期");
        assert_eq!(human_duration(90), "1 分钟");
        assert_eq!(human_duration(3_600 * 5), "5 小时");
        assert_eq!(human_duration(86_400 * 89 + 3_600 * 4), "89 天 4 小时");
    }

    /// Generated ids must satisfy the same rule we validate against, and be
    /// distinct — a collision would silently point two machines at one paste.
    #[test]
    fn generated_ids_are_valid_and_distinct() {
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..64 {
            let id = new_sync_id();
            validate_sync_id(&id).unwrap();
            assert_eq!(id.len(), 20);
            assert!(seen.insert(id), "generated a duplicate id");
        }
    }
}
