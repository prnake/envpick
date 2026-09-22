//! On-disk location and the machine-local `settings.toml`.
//!
//! Everything in `settings.toml` is **machine-local and never synced** — in
//! particular the sync key, which must never leave this machine except as
//! ciphertext.

pub mod profile;
pub mod store;

pub use profile::{
    Profile, is_reserved, validate_new_profile_name, validate_profile_name, validate_var_name,
};
pub use store::ProfileStore;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default pastebin endpoint.
pub const DEFAULT_ENDPOINT: &str = "https://pb.pka.moe";
/// Requested paste lifetime. The server clamps this to its own `MAX_EXPIRATION`
/// and reports the real value back, which we then record.
pub const DEFAULT_EXPIRE: &str = "90d";
/// Prefix for the paste name, so synced documents are identifiable and don't
/// collide with unrelated pastes.
pub const PASTE_PREFIX: &str = "ep-";
/// Name of the profile that is activated in every new shell.
pub const GLOBAL_PROFILE: &str = "global";

pub const ENV_SYNC_KEY: &str = "ENVPICK_SYNC_KEY";
pub const ENV_CONFIG_DIR: &str = "ENVPICK_CONFIG_DIR";

/// Resolved configuration directory and the files inside it.
#[derive(Clone, Debug)]
pub struct Paths {
    pub root: PathBuf,
}

impl Paths {
    /// `$ENVPICK_CONFIG_DIR`, else `$XDG_CONFIG_HOME/envpick`, else the
    /// platform config dir. Mirrors what the shell snippet does, so the two
    /// always agree.
    pub fn resolve() -> Result<Self> {
        if let Some(dir) = std::env::var_os(ENV_CONFIG_DIR)
            && !dir.is_empty()
        {
            return Ok(Self {
                root: PathBuf::from(dir),
            });
        }
        let base = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(x) if Path::new(&x).is_absolute() => PathBuf::from(x),
            _ => dirs::config_dir().context(crate::text::errors::NO_CONFIG_DIR)?,
        };
        Ok(Self {
            root: base.join("envpick"),
        })
    }

    pub fn settings_file(&self) -> PathBuf {
        self.root.join("settings.toml")
    }

    pub fn profiles_file(&self) -> PathBuf {
        self.root.join("profiles.toml")
    }

    pub fn ensure_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("无法创建配置目录 {}", self.root.display()))
    }
}

fn default_endpoint() -> String {
    DEFAULT_ENDPOINT.to_string()
}
fn default_expire() -> String {
    DEFAULT_EXPIRE.to_string()
}

/// Sync configuration. `sync_id` + `key` are the only two things a user must
/// supply; everything else is derived or optional.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct SyncSettings {
    #[serde(default = "default_endpoint")]
    pub endpoint: String,

    /// User-chosen identifier. Becomes the paste name `ep-<sync_id>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_id: Option<String>,

    /// The passphrase. Both the encryption key and the paste management
    /// password are derived from it. `ENVPICK_SYNC_KEY` takes precedence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,

    #[serde(default = "default_expire")]
    pub expire: String,

    /// Optional `Authorization` header, for deployments with `BASIC_AUTH` on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,

    /// Hash of the document as of the last successful push/pull. Compared
    /// against the current local document to decide whether we are "dirty".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_hash: Option<String>,

    /// Revision of the remote document as of the last sync.
    ///
    /// This one field answers both "what should the next revision be?" and "did
    /// the remote move?" — the remote document carries its own revision, so
    /// reading it back and comparing is enough. That matters because the
    /// deployment we talk to has no metadata endpoint: there is no
    /// `lastModifiedAt` to compare, and the document is the only source of truth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_remote_revision: Option<u64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<String>,

    /// Lifetime the server granted, learned from its upload receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_expiration_seconds: Option<u64>,

    /// When the paste lapses, as an absolute instant.
    ///
    /// Stored absolute rather than derived on read so that "remaining" means
    /// the same thing however much later it is asked, and so a bogus granted
    /// lifetime shows up as a wrong date the user can see rather than as a
    /// countdown that keeps resetting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_expires_at: Option<String>,
}

impl Default for SyncSettings {
    fn default() -> Self {
        Self {
            endpoint: default_endpoint(),
            sync_id: None,
            key: None,
            expire: default_expire(),
            auth: None,
            last_synced_hash: None,
            last_remote_revision: None,
            last_synced_at: None,
            remote_expiration_seconds: None,
            remote_expires_at: None,
        }
    }
}

impl SyncSettings {
    /// The effective key: environment wins so CI can inject it without a file.
    pub fn effective_key(&self) -> Option<String> {
        match std::env::var(ENV_SYNC_KEY) {
            Ok(v) if !v.is_empty() => Some(v),
            _ => self.key.clone(),
        }
    }

    pub fn paste_name(&self) -> Result<String> {
        let id = self
            .sync_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!(crate::text::errors::SYNC_NOT_CONFIGURED))?;
        validate_sync_id(id)?;
        Ok(format!("{PASTE_PREFIX}{id}"))
    }

    /// Forget everything we know about the remote, so the next sync re-reads it.
    pub fn forget_remote_state(&mut self) {
        self.last_synced_hash = None;
        self.last_remote_revision = None;
        self.last_synced_at = None;
        self.remote_expires_at = None;
    }
}

/// Stricter than the pastebin's own name rule (`[a-zA-Z0-9+_\-[\]*$@,;]{3,}`)
/// on purpose: we stay inside characters that need no percent-encoding in a URL
/// path, so there is no encode/decode ambiguity to get wrong.
pub fn validate_sync_id(id: &str) -> Result<()> {
    let ok = (3..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        bail!(crate::text::errors::SYNC_ID_INVALID);
    }
    Ok(())
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Stable random id for this machine, recorded in synced documents so a
    /// conflict screen can say which side came from where.
    #[serde(default)]
    pub device_id: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,

    /// Profiles activated automatically in every new shell, by `ep init`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_profiles: Vec<String>,

    #[serde(default)]
    pub sync: SyncSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            device_id: new_device_id(),
            device_name: None,
            default_profiles: Vec::new(),
            sync: SyncSettings::default(),
        }
    }
}

impl Settings {
    /// Load, falling back to defaults when the file doesn't exist yet. A
    /// *malformed* file is a hard error — silently overwriting a user's config
    /// because of a typo would be much worse than failing.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("无法读取 {}", path.display()))?;
        let mut s: Settings =
            toml::from_str(&raw).with_context(|| format!("{} 格式有误", path.display()))?;
        if s.device_id.is_empty() {
            s.device_id = new_device_id();
        }
        Ok(s)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let raw = toml::to_string_pretty(self).context("序列化 settings 失败")?;
        write_private(path, raw.as_bytes())
    }
}

/// Write a file that may contain secrets, readable only by the owner.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("无法创建目录 {}", parent.display()))?;
    }
    std::fs::write(path, bytes).with_context(|| format!("无法写入 {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(path, perms);
    }
    Ok(())
}

/// Write via a temp file + rename, so a crash mid-write can't truncate the
/// user's profiles.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("无法创建目录 {}", parent.display()))?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("无法写入 {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("无法替换 {}", path.display()))?;
    Ok(())
}

fn new_device_id() -> String {
    let mut buf = [0u8; 8];
    rand::fill(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}
