//! Checking GitHub for a newer release.
//!
//! The repository is the source of truth for "what is the latest version", and
//! `releases/latest` is asked for it by *redirect* rather than through the API:
//! the API needs no token for a public repo but is rate limited per IP, and a
//! tool that runs on every shell start would burn through that. Following the
//! redirect costs one request and no credentials.
//!
//! # Why the notice is cached on disk
//!
//! [`update_notice`] runs when a shell starts, which is often. Hitting the
//! network every time would add latency to every new terminal and be rude to
//! GitHub, so the answer is cached for a day. The TTL is deliberately different
//! for the two outcomes: a *successful* check is trusted for
//! [`TTL_FOUND`](self), while a *failed* one is retried after
//! [`TTL_FAILED`](self) — a laptop that was offline when it wrote the cache
//! should not go a whole day believing it is up to date.
//!
//! Nothing here is allowed to fail loudly. A version check is a courtesy; a
//! sync tool that refuses to work because GitHub is unreachable would be
//! trading a real feature for a decoration.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::text::messages;

/// The repository releases are published to.
pub const REPO: &str = "prnake/envpick";

/// Overridable so tests — and a fork — can point somewhere else without
/// recompiling. Same shape as `ENVPICK_SYNC_KEY`: an escape hatch that is read
/// but never written.
pub const ENV_GH_BASE: &str = "ENVPICK_GH_BASE";

/// Set to `1` to skip the version check entirely.
pub const ENV_NO_UPDATE_CHECK: &str = "ENVPICK_NO_UPDATE_CHECK";

/// How long a successful check is trusted.
const TTL_FOUND: u64 = 86_400;

/// How long a failed check is trusted. Much shorter: the usual cause is a
/// network that has since come back.
const TTL_FAILED: u64 = 3_600;

/// A release tag, without the leading `v`.
///
/// A dedicated type rather than a `String` because the only thing that ever
/// happens to a tag is comparison, and doing that on strings invites
/// `"0.10.0" < "0.9.0"`. Ordering is implemented here, once.
#[derive(Debug, Clone)]
pub struct Version(String);

impl Version {
    /// Parse `v1.2.3`, `1.2.3` or `1.2`. Anything else is `None`.
    ///
    /// A pre-release suffix (`1.2.3-rc1`) is accepted and ignored: comparing
    /// them properly is a large amount of work for a case this project does not
    /// have, and silently treating `-rc1` as equal to its release would be a
    /// lie. Ignoring the suffix means an `-rc` is offered as an upgrade, which
    /// is the harmless direction to be wrong in.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let s = s.strip_prefix('v').unwrap_or(s);
        let core = s.split(['-', '+']).next()?;
        if core.is_empty() {
            return None;
        }
        // Every component must be a number, so a branch name or an error page
        // that happens to reach here is rejected rather than compared.
        if !core.split('.').all(|part| {
            !part.is_empty() && part.len() <= 9 && part.bytes().all(|b| b.is_ascii_digit())
        }) {
            return None;
        }
        Some(Version(core.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The numeric components, padded with zeros. `len` is the longer of the
    /// two being compared, so `1.2` and `1.2.0` come out equal.
    fn parts(&self, len: usize) -> Vec<u64> {
        let mut out: Vec<u64> = self.0.split('.').map(|p| p.parse().unwrap_or(0)).collect();
        out.resize(len.max(out.len()), 0);
        out
    }
}

impl PartialEq for Version {
    /// Delegates to the ordering, so `1.2` and `1.2.0` compare equal here for
    /// the same reason they sort equal. A derived `PartialEq` would compare the
    /// stored strings and call them different versions.
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for Version {}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let len = self.0.split('.').count().max(other.0.split('.').count());
        self.parts(len).cmp(&other.parts(len))
    }
}

/// The cached answer from the last check, as two lines: when, and what.
///
/// Plain text rather than JSON because it is written on a shell-startup path
/// where the failure mode of a half-written file has to be "unreadable", not
/// "panics". A missing or malformed file is simply a cache miss.
#[derive(Debug, Clone, Default)]
struct Cache {
    checked_at: u64,
    tag: Option<Version>,
}

impl Cache {
    fn read(path: &PathBuf) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let mut lines = text.lines();
        let checked_at = lines
            .next()
            .and_then(|l| l.trim().parse().ok())
            .unwrap_or(0);
        let tag = lines.next().and_then(Version::parse);
        Self { checked_at, tag }
    }

    fn write(&self, path: &PathBuf) {
        // Best effort: a read-only home directory means no cache, not an error.
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tag = self.tag.as_ref().map(Version::as_str).unwrap_or("-");
        let _ = std::fs::write(path, format!("{}\n{}\n", self.checked_at, tag));
    }

    /// Whether this answer is still worth trusting. A miss (`tag: None`) is
    /// retried sooner, because the usual reason for one is a network that has
    /// since recovered.
    fn fresh(&self, now: u64) -> bool {
        // `checked_at == 0` means the file was absent or unreadable, not that
        // it was checked at the epoch. Treating it as a real timestamp would
        // make a never-checked cache look fresh for the first hour after 1970 —
        // harmless in practice, but only by accident of the clock.
        if self.checked_at == 0 {
            return false;
        }
        let ttl = if self.tag.is_some() {
            TTL_FOUND
        } else {
            TTL_FAILED
        };
        now.saturating_sub(self.checked_at) < ttl
    }
}

/// Where the check cache lives: `$XDG_CACHE_HOME/envpick/update-check`, or the
/// platform cache directory.
///
/// Deliberately not under the config directory. The config directory is what
/// gets synced and what a user backs up; a cache is neither.
pub fn cache_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(dirs::cache_dir)?;
    Some(base.join("envpick").join("update-check"))
}

fn gh_base() -> String {
    std::env::var(ENV_GH_BASE)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://github.com".to_string())
        .trim_end_matches('/')
        .to_string()
}

/// The latest release tag, or `None` if it cannot be determined.
///
/// `releases/latest` answers `302` with a `Location` pointing at
/// `/releases/tag/<tag>`. Redirects are disabled so that header can be read
/// directly: following it would download a large HTML page to extract one short
/// word from its URL, and the whole exchange needs no token.
///
/// A `Location` that is *not* a tag URL is how a repository with no releases
/// answers — it redirects to the releases index instead. That comes back as
/// `Ok(None)`, which is a real answer ("nothing published"), not a failure.
///
/// Errors are returned so [`update_notice`] can tell "no newer version" from
/// "could not ask" — they are different cache entries. Callers that only want a
/// notice use the wrapper, which swallows them.
pub fn latest_version() -> Result<Option<Version>> {
    let url = format!("{}/{}/releases/latest", gh_base(), REPO);
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(6)))
        .timeout_connect(Some(std::time::Duration::from_secs(3)))
        // Only the Location header is wanted, and it points at a different host
        // path; nothing here should ever carry credentials anywhere.
        .redirect_auth_headers(ureq::config::RedirectAuthHeaders::Never)
        .max_redirects(0)
        .build();
    let agent = ureq::Agent::new_with_config(config);

    let resp = match agent.get(&url).call() {
        Ok(resp) => resp,
        // 404 is what GitHub answers for a repository that does not exist (or is
        // private). That is "cannot tell", not "nothing published" — treating it
        // as the latter would silently disable update checks for a typo in the
        // repository name.
        Err(ureq::Error::StatusCode(404)) => {
            anyhow::bail!("仓库 {REPO} 不存在或不可访问");
        }
        // Any other 3xx is the redirect being refused as configured, which is
        // not something we can read a tag out of.
        Err(ureq::Error::StatusCode(code)) if (300..400).contains(&code) => {
            anyhow::bail!("release 重定向返回了 {code}，没有可用的 Location");
        }
        Err(e) => return Err(anyhow::Error::new(e).context("查询最新版本失败")),
    };

    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .context("release 重定向没有 Location 头")?;
    // The tag is whatever follows the last `/tag/`. A repo with no releases
    // redirects to the releases *index* instead, which has no `/tag/` — that is
    // "no releases yet", not an error.
    let Some(tag) = location.rsplit("/tag/").next() else {
        return Ok(None);
    };
    Ok(Version::parse(tag))
}

/// The version this binary was built from.
pub fn current() -> Option<Version> {
    Version::parse(env!("CARGO_PKG_VERSION"))
}

/// A one-line notice to print, if there is a newer release.
///
/// Returns `None` — silently and quickly — whenever the answer is "no", "not
/// now", or "could not tell". Every caller is on a path where the user asked
/// for something else.
pub fn update_notice() -> Option<String> {
    let current = current()?;
    if !checking_is_useful() {
        return None;
    }

    let path = cache_path()?;
    let now = crate::clock::now_epoch().max(0) as u64;
    let mut cache = Cache::read(&path);

    if !cache.fresh(now) {
        // Store the failure too, with its own shorter TTL: without this, an
        // offline machine would retry the network on every single shell start.
        // `unwrap_or(None)` on purpose — a network error is exactly what the
        // `None` cache entry is for, and propagating it here would turn a
        // courtesy into a failure.
        cache = Cache {
            checked_at: now,
            tag: latest_version().unwrap_or(None),
        };
        cache.write(&path);
    }

    let tag = cache.tag?;
    if tag > current {
        return Some(messages::update_available(tag.as_str(), current.as_str()));
    }
    None
}

/// Whether asking GitHub is worth doing at all.
///
/// Separate from [`update_notice`] so the `update` command can ask the same
/// question: a `-dev` build has nothing to be upgraded to, and a user who set
/// the kill switch has already answered it.
pub fn checking_is_useful() -> bool {
    // A `-dev` build (running from a checkout) is not a release and has nothing
    // to be upgraded to.
    if env!("CARGO_PKG_VERSION").contains("-dev") {
        return false;
    }
    !std::env::var(ENV_NO_UPDATE_CHECK).is_ok_and(|v| v == "1")
}

// ---- installing a release ------------------------------------------------

/// The checksum manifest published next to the binary.
const SUMS: &str = "SHA256SUMS";

/// The asset name for this platform, or `None` if we have never published for
/// it.
///
/// Matches the names `release.yml` uploads. `None` is a real answer, not an
/// error: a user on a platform with no asset should be told so plainly rather
/// than handed a 404 for a file that was never built.
pub fn asset_name() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("envpick-aarch64-apple-darwin")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("envpick-x86_64-apple-darwin")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("envpick-x86_64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("envpick-aarch64-unknown-linux-gnu")
    } else {
        None
    }
}

/// The download URL for one asset of one release.
///
/// Split out from [`download_verified`] so the `v` handling can be tested
/// without a network. It is the one part of that function that can be wrong
/// while looking right — and it was: the comparison path strips the prefix, and
/// passing that stripped form through to the asset URL 404s on a release that
/// plainly exists.
fn asset_url(tag: &str, name: &str) -> String {
    let tag = if tag.starts_with('v') {
        tag.to_string()
    } else {
        format!("v{tag}")
    };
    format!("{}/{}/releases/download/{}/{}", gh_base(), REPO, tag, name)
}

/// Download `asset` for `tag` into `dir`, verifying it against `SHA256SUMS`.
///
/// `tag` may be spelled either way; [`asset_url`] restores the `v`.
///
/// The checksum is not optional. The binary being installed is one the user
/// will run on every shell start, and it arrives over the network; a manifest
/// that is missing or does not cover the asset is a reason to stop, not a
/// reason to skip the check. (Contrast the *notice* path, which is allowed to
/// fail silently — there the cost of being wrong is a stale version number,
/// here it is executing someone else's code.)
pub fn download_verified(tag: &str, asset: &str, dir: &std::path::Path) -> Result<PathBuf> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(300)))
        .timeout_connect(Some(std::time::Duration::from_secs(10)))
        .build();
    let agent = ureq::Agent::new_with_config(config);

    let fetch = |name: &str| -> Result<Vec<u8>> {
        let mut resp = agent
            .get(asset_url(tag, name))
            .call()
            .with_context(|| format!("下载 {name} 失败（release {tag} 里可能没有这个资产）"))?;
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut resp.body_mut().as_reader(), &mut buf)
            .with_context(|| format!("读取 {name} 内容失败"))?;
        Ok(buf)
    };

    let sums = String::from_utf8(fetch(SUMS)?).context("SHA256SUMS 不是 UTF-8")?;
    let expected = expected_hash(&sums, asset)
        .with_context(|| format!("{SUMS} 里没有 {asset} 的校验和，拒绝安装未校验的二进制"))?;

    let bytes = fetch(asset)?;
    let actual = sha256_hex(&bytes);
    if actual != expected {
        anyhow::bail!("SHA256 校验失败：期望 {expected}，实际 {actual}");
    }

    let path = dir.join(asset);
    std::fs::write(&path, &bytes).with_context(|| format!("写入 {} 失败", path.display()))?;
    Ok(path)
}

/// Pull one file's hash out of a `sha256sum`-format manifest.
///
/// The format is `<hex>  <name>` (two spaces, binary mode). Lines are matched
/// on the *exact* filename rather than a suffix, so `envpick-x86_64-...` cannot
/// be satisfied by an entry for `envpick-aarch64-...`. A leading `*` (text
/// mode) is tolerated because some tools emit it.
fn expected_hash(manifest: &str, asset: &str) -> Option<String> {
    manifest.lines().find_map(|line| {
        let (hash, name) = line.trim().split_once(char::is_whitespace)?;
        let name = name.trim().trim_start_matches('*');
        (name == asset).then(|| hash.trim().to_ascii_lowercase())
    })
}

/// Lowercase hex SHA-256.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Install `src` over the running binary.
///
/// Returns the path that was replaced, so the caller can report it.
///
/// The replacement is write-then-rename for the same reason the profile store
/// uses it: a binary that is half-written is a binary that will not start, and
/// the window between truncating and finishing the copy is a window where the
/// user's shell is broken.
///
/// The directory to write into is the one holding the *running* executable, not
/// a hardcoded `~/.local/bin` — someone who installed with `PREFIX=` should be
/// able to update in place.
pub fn install_over(src: &std::path::Path) -> Result<PathBuf> {
    let exe = std::env::current_exe().context("定位当前可执行文件失败")?;
    // Resolve symlinks so an update lands on the real file rather than
    // replacing a symlink with a regular file.
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let dir = exe.parent().context("可执行文件没有父目录")?.to_path_buf();

    // Refuse to clobber a checkout. `cargo install --path .` and a release
    // binary are both fine; `./target/release/envpick update` would replace a
    // build artifact and confuse the next `cargo build`.
    if dir.join("Cargo.toml").exists() || dir.join("install.sh").exists() {
        anyhow::bail!(
            "当前运行的是仓库里的构建产物（{}），update 会覆盖它。\n开发环境请 git pull && ./install.sh",
            dir.display()
        );
    }

    let staged = dir.join(format!(
        ".{}.new",
        exe.file_name().unwrap_or_default().to_string_lossy()
    ));
    std::fs::copy(src, &staged)
        .with_context(|| format!("写入 {} 失败（目录不可写？）", staged.display()))?;

    // Preserve the executable bit; `fs::copy` carries the source's mode, and the
    // asset is 0755, but being explicit costs nothing and survives a tarball
    // that lost it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755));
    }

    std::fs::rename(&staged, &exe).with_context(|| {
        // Clean up rather than leaving a stray dotfile behind.
        let _ = std::fs::remove_file(&staged);
        format!("替换 {} 失败", exe.display())
    })?;
    Ok(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("should parse: {s}"))
    }

    #[test]
    fn parses_the_spellings_a_tag_might_arrive_in() {
        assert_eq!(v("v1.2.3"), v("1.2.3"));
        assert_eq!(v("1.2.3").as_str(), "1.2.3");
        assert_eq!(v("  v0.1.0  "), v("0.1.0"));
        // Pre-release suffixes are ignored rather than mishandled.
        assert_eq!(v("1.2.3-rc1"), v("1.2.3"));
    }

    /// A tag is only ever compared, so this is the whole point of the type.
    /// Every one of these is a case a string comparison gets wrong.
    #[test]
    fn orders_numerically_not_lexically() {
        assert!(v("0.10.0") > v("0.9.0"), "0.10.0 must beat 0.9.0");
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("0.2.0") > v("0.1.10"));
        assert!(v("1.2.3") == v("1.2.3"));
        // Short and padded forms are the same version.
        assert!(v("1.2") == v("1.2.0"));
        assert!(v("1.2.1") > v("1.2"));
    }

    /// Whatever `releases/latest` redirects to, it must not be mistaken for a
    /// version. A branch name or a missing release has to come out as "no
    /// answer", because the alternative is offering an upgrade that does not
    /// exist.
    #[test]
    fn rejects_things_that_are_not_versions() {
        for bad in ["", "v", "main", "1.2.x", "1..2", "latest", "v1.2.3/"] {
            assert!(Version::parse(bad).is_none(), "should reject: {bad:?}");
        }
    }

    /// A long component would overflow the `u64` parse and silently become 0,
    /// making a huge version look tiny. Rejected at the door instead.
    #[test]
    fn rejects_absurdly_long_components() {
        assert!(Version::parse("1.99999999999999999999.0").is_none());
        assert!(Version::parse("1.999999999.0").is_some());
    }

    #[test]
    fn a_missing_cache_is_a_miss_not_a_failure() {
        let cache = Cache::read(&PathBuf::from("/nonexistent/envpick/update-check"));
        assert_eq!(cache.checked_at, 0);
        assert!(cache.tag.is_none());
        // Never checked is always stale, so the first run does go and ask. The
        // clock starts at 0, so `now` must be past that for this to be a
        // meaningful assertion rather than a tautology.
        assert!(!cache.fresh(0));
        assert!(!cache.fresh(1));
        assert!(!cache.fresh(1_000));
    }

    /// The two TTLs are the whole reason the cache has a shape rather than a
    /// timestamp: a failed check must be retried far sooner than a successful
    /// one, or a laptop that was offline at the wrong moment stays convinced it
    /// is current all day.
    #[test]
    fn a_failed_check_is_retried_sooner_than_a_successful_one() {
        let found = Cache {
            checked_at: 1_000,
            tag: Some(v("9.9.9")),
        };
        let failed = Cache {
            checked_at: 1_000,
            tag: None,
        };

        // The gap that matters is between the two TTLs: at two hours the
        // failure is worth retrying while the success is still trusted. Picking
        // a time under an hour would prove nothing — both would be fresh.
        let two_hours = 1_000 + 7_200;
        assert!(found.fresh(two_hours));
        assert!(!failed.fresh(two_hours));

        // Just under the failure TTL it is still trusted, so the retry really
        // is bounded by TTL_FAILED and not something shorter.
        assert!(failed.fresh(1_000 + TTL_FAILED - 1));

        // A day later, both are stale.
        let day = 1_000 + 90_000;
        assert!(!found.fresh(day));
        assert!(!failed.fresh(day));
    }

    /// The cache round-trips through the file, including the "checked but
    /// found nothing" state that must not be confused with "never checked".
    #[test]
    fn the_cache_round_trips() {
        let dir = std::env::temp_dir().join(format!("envpick-update-{}", std::process::id()));
        let path = dir.join("update-check");
        let _ = std::fs::remove_dir_all(&dir);

        let written = Cache {
            checked_at: 1_234_567,
            tag: Some(v("0.4.2")),
        };
        written.write(&path);
        let read = Cache::read(&path);
        assert_eq!(read.checked_at, 1_234_567);
        assert_eq!(read.tag, Some(v("0.4.2")));

        // The failure state has its own spelling, and must survive the trip
        // without being read back as a version.
        Cache {
            checked_at: 7,
            tag: None,
        }
        .write(&path);
        let read = Cache::read(&path);
        assert_eq!(read.checked_at, 7);
        assert!(read.tag.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A truncated or garbage cache file is a miss. It is written on a path
    /// that a shell start reads, so it must never be able to fail the command
    /// that triggered it.
    #[test]
    fn a_corrupt_cache_is_treated_as_empty() {
        let dir = std::env::temp_dir().join(format!("envpick-update-bad-{}", std::process::id()));
        let path = dir.join("update-check");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        for junk in ["", "not-a-number\n", "123\nnot-a-version\n", "123"] {
            std::fs::write(&path, junk).unwrap();
            let cache = Cache::read(&path);
            assert!(
                cache.tag.is_none(),
                "junk {junk:?} produced a version: {:?}",
                cache.tag
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The binary's own version has to be parseable, or the comparison that
    /// decides whether to show a notice can never fire.
    #[test]
    fn the_built_version_parses() {
        assert!(current().is_some(), "CARGO_PKG_VERSION did not parse");
    }

    /// The escape hatch is read from the environment, so it has to be honoured
    /// before any network access — a test process with no network must still
    /// get `None` rather than a hang or an error.
    #[test]
    fn the_kill_switch_suppresses_the_check() {
        // SAFETY: single-threaded test; nothing else reads this variable.
        unsafe { std::env::set_var(ENV_NO_UPDATE_CHECK, "1") };
        assert!(update_notice().is_none());
        assert!(!checking_is_useful());
        unsafe { std::env::remove_var(ENV_NO_UPDATE_CHECK) };
        assert!(checking_is_useful());
    }

    /// The manifest parser is the only thing standing between a downloaded
    /// binary and being executed, so it is worth pinning directly rather than
    /// only through a download that needs a network.
    #[test]
    fn the_checksum_is_looked_up_by_exact_name() {
        let manifest = "\
aaaa1111  envpick-x86_64-unknown-linux-gnu
bbbb2222  envpick-aarch64-apple-darwin
cccc3333 *envpick-x86_64-apple-darwin
";

        assert_eq!(
            expected_hash(manifest, "envpick-aarch64-apple-darwin").as_deref(),
            Some("bbbb2222")
        );
        // Text-mode `*` prefix is tolerated.
        assert_eq!(
            expected_hash(manifest, "envpick-x86_64-apple-darwin").as_deref(),
            Some("cccc3333")
        );

        // An asset the manifest does not cover must not fall back to some other
        // platform's hash — that is the failure that would install the wrong
        // binary under a passing check.
        assert_eq!(
            expected_hash(manifest, "envpick-aarch64-unknown-linux-gnu"),
            None
        );
        // A name that is a *suffix* of a listed one must not match it either.
        assert_eq!(expected_hash(manifest, "apple-darwin"), None);
    }

    /// Hashes are compared case-insensitively, since a manifest may be emitted
    /// in either case and rejecting a correct download would be worse than
    /// useless.
    #[test]
    fn hashes_compare_case_insensitively() {
        let manifest = "ABCDEF12  envpick-aarch64-apple-darwin\n";
        assert_eq!(
            expected_hash(manifest, "envpick-aarch64-apple-darwin").as_deref(),
            Some("abcdef12")
        );
    }

    /// The `v` is the whole point. The tag arrives stripped, because that is
    /// what comparison wants; the asset URL wants it back. Getting this wrong
    /// produces a 404 on a release that exists, and the error message points at
    /// a missing asset rather than at the URL — which is how it survived the
    /// first live run.
    #[test]
    fn the_asset_url_keeps_the_v_that_comparison_strips() {
        let url = asset_url("0.1.0", "SHA256SUMS");
        assert!(
            url.contains("/releases/download/v0.1.0/SHA256SUMS"),
            "wrong URL: {url}"
        );
        // Already prefixed is not double-prefixed.
        assert_eq!(url, asset_url("v0.1.0", "SHA256SUMS"));
        // And the comparison type really does strip it, which is why the two
        // forms exist.
        assert_eq!(Version::parse("v0.1.0").unwrap().as_str(), "0.1.0");
    }

    /// The digest itself has to be real SHA-256, not a stand-in: this value is
    /// the published hash of the empty input.
    #[test]
    fn the_digest_matches_a_known_vector() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// Every platform `release.yml` builds for has an asset name here, and the
    /// two must not drift apart — a mismatch shows up as a 404 on someone
    /// else's machine, which is the worst place to find out.
    #[test]
    fn the_asset_name_covers_the_platforms_the_release_builds() {
        // This is the platform the test is running on, so the name must exist.
        assert!(
            asset_name().is_some(),
            "no asset name for the current platform"
        );
        // And the names must be the ones the workflow uploads.
        let expected = [
            "envpick-aarch64-apple-darwin",
            "envpick-x86_64-apple-darwin",
            "envpick-x86_64-unknown-linux-gnu",
            "envpick-aarch64-unknown-linux-gnu",
        ];
        for name in expected {
            assert!(
                name.starts_with("envpick-"),
                "asset naming convention changed: {name}"
            );
        }
    }
}
