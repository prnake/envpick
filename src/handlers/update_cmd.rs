//! `envpick update` — install the latest release over the running binary.
//!
//! Deliberately not silent and not automatic. The passive notice printed at
//! shell start is a courtesy; this command is the user deciding, and it is the
//! only path that replaces a file on disk. Making that happen without being
//! asked would be a tool that rewrites itself while you are not looking.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::text::messages;
use crate::update;

pub fn run(force: bool) -> Result<()> {
    let Some(current) = update::current() else {
        anyhow::bail!("无法解析当前版本号");
    };
    println!("当前版本：{}", current.as_str());

    let Some(asset) = update::asset_name() else {
        anyhow::bail!(
            "没有为当前平台发布二进制（{}），请用 cargo install --path . 从源码安装",
            std::env::consts::ARCH
        );
    };

    // The notice cache is ignored here: `update` is the user asking *now*, and
    // answering "you asked yesterday" would be absurd. The network call is the
    // point of the command.
    //
    // No `.context()` on the error: the module already says precisely what went
    // wrong (no such repository, no Location header, connection refused), and
    // wrapping it in "网络不通，或者仓库还没有发布过 release" would replace a
    // specific answer with a guess.
    let Some(latest) = update::latest_version()? else {
        println!("{}", messages::UPDATE_NO_RELEASES);
        return Ok(());
    };
    println!("最新版本：{}", latest.as_str());

    if !force {
        if latest == current {
            println!("{}", messages::ALREADY_LATEST);
            return Ok(());
        }
        if latest < current {
            // A locally built binary ahead of the last release is normal during
            // development; say so rather than reinstalling an older one.
            println!(
                "本地版本不低于线上（{} ≥ {}），不重装；要强制重装加 --force",
                current.as_str(),
                latest.as_str()
            );
            return Ok(());
        }
    }

    let dir = scratch_dir()?;
    let result = (|| -> Result<()> {
        println!("下载 {asset}…");
        let path = update::download_verified(latest.as_str(), asset, &dir)?;
        println!("SHA256 校验通过");
        let installed = update::install_over(&path)?;
        println!("已更新到 {}（{}）", latest.as_str(), installed.display());
        Ok(())
    })();

    // Clean up whichever way it went; a failed update should not leave a
    // half-megabyte binary in the temp directory.
    let _ = std::fs::remove_dir_all(&dir);
    result?;

    println!("新版本下次启动生效；当前这个进程仍然跑的是旧代码");
    Ok(())
}

/// A private directory to download into.
///
/// Not the shared temp directory directly: the file that lands here is about to
/// be executed, so it should not sit somewhere another local process can
/// replace it between the checksum check and the rename. Created with 0700.
fn scratch_dir() -> Result<PathBuf> {
    let base = std::env::temp_dir();
    let dir = base.join(format!("envpick-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("创建临时目录 {} 失败", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(dir)
}
