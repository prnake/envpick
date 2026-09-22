//! TUI state and the mutations the interface performs.
//!
//! The app owns its own copy of the store and settings rather than borrowing
//! [`Ctx`](crate::handlers::Ctx), because almost every action both mutates the
//! store and then saves it — a single owned value keeps those two steps from
//! fighting over the borrow.
//!
//! It cannot activate profiles. A child process cannot change its parent
//! shell's environment, so the TUI shows the exact `ep use ...` command to run
//! instead of pretending a keystroke did it.

use anyhow::{Context, Result, bail};

use crate::config::{
    Paths, Profile, ProfileStore, Settings, is_reserved, validate_new_profile_name,
    validate_sync_id, validate_var_name,
};
use crate::graph;
use crate::shell;
use crate::sync::{SyncDoc, SyncEngine, SyncOutcome, SyncState, SyncStatus};

/// The four top-level views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Profiles,
    Editor,
    Sync,
    Settings,
}

impl View {
    pub const ALL: [View; 4] = [View::Profiles, View::Editor, View::Sync, View::Settings];

    pub fn title(&self) -> &'static str {
        match self {
            View::Profiles => "Profiles",
            View::Editor => "Editor",
            View::Sync => "Sync",
            View::Settings => "Settings",
        }
    }

    pub fn index(&self) -> usize {
        View::ALL.iter().position(|v| v == self).unwrap_or(0)
    }

    pub fn shifted(&self, delta: isize) -> View {
        let n = View::ALL.len() as isize;
        let i = (self.index() as isize + delta).rem_euclid(n) as usize;
        View::ALL[i]
    }
}

/// What the input popup is collecting.
#[derive(Debug, Clone)]
pub enum Purpose {
    NewProfile,
    /// Collecting a new variable's name; the value is asked for next.
    NewVarName {
        profile: String,
    },
    SetVarValue {
        profile: String,
        key: String,
    },
    AddRequires {
        profile: String,
    },
    SyncId,
    SyncKey,
    Endpoint,
    Expire,
    DeviceName,
}

#[derive(Debug, Clone)]
pub struct Input {
    pub title: String,
    pub prompt: String,
    pub value: String,
    pub mask: bool,
    pub purpose: Purpose,
}

impl Input {
    pub fn new(title: &str, prompt: &str, value: &str, purpose: Purpose) -> Self {
        Self {
            title: title.to_string(),
            prompt: prompt.to_string(),
            value: value.to_string(),
            mask: false,
            purpose,
        }
    }

    pub fn display(&self) -> String {
        if self.mask {
            "*".repeat(self.value.chars().count())
        } else {
            self.value.clone()
        }
    }
}

#[derive(Debug, Clone)]
pub enum ConfirmPurpose {
    DeleteProfile(String),
    DeleteRemote,
    /// Overwrite a remote we could not decrypt.
    PushOverUnreadable,
}

#[derive(Debug, Clone)]
pub struct Confirm {
    pub title: String,
    pub body: Vec<String>,
    pub purpose: ConfirmPurpose,
}

/// A sync conflict awaiting a decision. Held as whole documents so the screen
/// can show what each side contains before anything is written.
#[derive(Debug, Clone)]
pub struct Conflict {
    pub local: SyncDoc,
    pub remote: SyncDoc,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub text: String,
    pub error: bool,
}

#[derive(Debug, Default)]
pub struct SyncPanel {
    pub status: Option<SyncStatus>,
    pub error: Option<String>,
    pub browser_url: Option<String>,
}

pub struct App {
    pub paths: Paths,
    pub store: ProfileStore,
    pub settings: Settings,

    pub view: View,
    pub quit: bool,
    pub status: Option<Status>,

    pub selected: usize,
    /// 0 = the variables table, 1 = the `requires` list.
    pub edit_section: usize,
    pub edit_row: usize,
    pub settings_row: usize,

    pub show_help: bool,
    pub input: Option<Input>,
    pub confirm: Option<Confirm>,
    pub conflict: Option<Conflict>,
    pub sync: SyncPanel,
}

impl App {
    pub fn new(paths: Paths, store: ProfileStore, settings: Settings) -> Self {
        let mut app = Self {
            paths,
            store,
            settings,
            view: View::Profiles,
            quit: false,
            status: None,
            selected: 0,
            edit_section: 0,
            edit_row: 0,
            settings_row: 0,
            show_help: false,
            input: None,
            confirm: None,
            conflict: None,
            sync: SyncPanel::default(),
        };
        app.refresh_sync_status();
        app
    }

    // ---- shared helpers -------------------------------------------------

    pub fn names(&self) -> Vec<String> {
        self.store.names()
    }

    pub fn selected_name(&self) -> Option<String> {
        self.names().get(self.selected).cloned()
    }

    /// The profiles active in the shell that launched us. Read from the
    /// inherited environment, so it reflects the *real* state rather than
    /// something we maintain separately.
    pub fn active(&self) -> Vec<String> {
        shell::active_profiles()
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            error: false,
        });
    }

    pub fn fail(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            error: true,
        });
    }

    /// Apply a fallible mutation, turning a failure into a status message.
    pub fn report<T>(&mut self, result: Result<T>) -> Option<T> {
        match result {
            Ok(v) => Some(v),
            Err(e) => {
                self.fail(format!("{e:#}"));
                None
            }
        }
    }

    fn clamp_selection(&mut self) {
        let len = self.store.profiles.len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    pub fn save_store(&self) -> Result<()> {
        self.paths.ensure_dir()?;
        self.store.save(&self.paths.profiles_file())
    }

    pub fn save_settings(&self) -> Result<()> {
        self.paths.ensure_dir()?;
        self.settings.save(&self.paths.settings_file())
    }

    // ---- profile mutations ----------------------------------------------

    pub fn create_profile(&mut self, name: &str) -> Result<()> {
        validate_new_profile_name(name)?;
        if self.store.contains(name) {
            bail!("profile 已存在: '{name}'");
        }
        self.store.upsert(name, Profile::default())?;
        self.save_store()?;
        // Leave the cursor on what was just created.
        if let Some(pos) = self.names().iter().position(|n| n == name) {
            self.selected = pos;
        }
        Ok(())
    }

    pub fn delete_profile(&mut self, name: &str) -> Result<()> {
        if is_reserved(name) {
            bail!("profile '{name}' 是保留名，不能删除");
        }
        if self.store.remove(name).is_none() {
            bail!("profile 不存在: '{name}'");
        }
        // Deleting a profile would otherwise leave every dependent broken.
        let pruned = self.store.prune_dangling_requires();
        self.save_store()?;
        self.clamp_selection();

        if pruned.is_empty() {
            self.info(format!("已删除 '{name}'"));
        } else {
            let list: Vec<String> = pruned.iter().map(|(p, d)| format!("{p} -> {d}")).collect();
            self.info(format!(
                "已删除 '{name}'，同时移除悬空依赖: {}",
                list.join(", ")
            ));
        }
        Ok(())
    }

    /// Set a variable, creating the profile if it does not exist yet.
    pub fn set_var(&mut self, profile_name: &str, key: &str, value: &str) -> Result<()> {
        validate_var_name(key)?;
        let mut profile = self.store.get(profile_name).cloned().unwrap_or_default();
        profile.vars.insert(key.to_string(), value.to_string());
        self.store.upsert(profile_name, profile)?;
        self.save_store()
    }

    pub fn unset_var(&mut self, profile_name: &str, key: &str) -> Result<()> {
        let mut profile = self
            .store
            .get(profile_name)
            .cloned()
            .context("profile 不存在")?;
        if profile.vars.remove(key).is_none() {
            bail!("变量不存在: {key}");
        }
        self.store.upsert(profile_name, profile)?;
        self.save_store()
    }

    /// Add a dependency, rejecting anything that would make the graph
    /// unresolvable. A cycle introduced here would break every later `use`, so
    /// it is worth checking at the moment it is entered.
    pub fn add_requires(&mut self, profile_name: &str, dep: &str) -> Result<()> {
        graph::add_requires(&mut self.store, profile_name, dep)?;
        self.save_store()
    }

    pub fn remove_requires(&mut self, profile_name: &str, dep: &str) -> Result<()> {
        graph::remove_requires(&mut self.store, profile_name, dep)?;
        self.save_store()
    }

    // ---- sync -----------------------------------------------------------

    fn engine(&self) -> Result<SyncEngine> {
        SyncEngine::new(self.paths.clone(), self.settings.clone())
    }

    /// Re-read from disk what the engine wrote, so the panel never shows stale
    /// bookkeeping after a transfer.
    fn reload_after_sync(&mut self) {
        if let Ok(s) = Settings::load(&self.paths.settings_file()) {
            self.settings = s;
        }
        if let Ok(s) = ProfileStore::load(&self.paths.profiles_file()) {
            self.store = s;
        }
        self.clamp_selection();
    }

    pub fn refresh_sync_status(&mut self) {
        if self.settings.sync.sync_id.is_none() || self.settings.sync.effective_key().is_none() {
            self.sync.status = None;
            self.sync.error = None;
            return;
        }
        match self.engine().and_then(|e| e.status()) {
            Ok(st) => {
                self.sync.status = Some(st);
                self.sync.error = None;
            }
            Err(e) => {
                self.sync.status = None;
                self.sync.error = Some(format!("{e:#}"));
            }
        }
    }

    /// Whether the last status check found a remote we cannot read.
    ///
    /// Pushing is the only way past that state, and it destroys data we never
    /// managed to read — so the key that does it asks first. The status is a
    /// snapshot from the last refresh; if the remote changed since, the prompt
    /// is simply one refresh stale, which errs toward asking.
    pub fn remote_unreadable(&self) -> bool {
        self.sync
            .status
            .as_ref()
            .is_some_and(|st| st.state == SyncState::RemoteUnreadable)
    }

    pub fn sync_now(&mut self) {
        let result = self.engine().and_then(|mut e| e.sync());
        match result {
            Ok(SyncOutcome::UpToDate) => {
                self.reload_after_sync();
                self.info(crate::text::messages::UP_TO_DATE);
            }
            Ok(SyncOutcome::Pushed { revision }) => {
                self.reload_after_sync();
                self.info(format!(
                    "{}（修订 r{revision}）",
                    crate::text::messages::PUSHED
                ));
            }
            Ok(SyncOutcome::Pulled { revision }) => {
                self.reload_after_sync();
                self.info(format!(
                    "{}（修订 r{revision}）",
                    crate::text::messages::PULLED
                ));
            }
            Ok(SyncOutcome::Conflict { local, remote }) => {
                self.conflict = Some(Conflict { local, remote });
            }
            Err(e) => self.fail(format!("{e:#}")),
        }
        self.refresh_sync_status();
    }

    pub fn push(&mut self) {
        let result = self.engine().and_then(|mut e| e.push());
        match result {
            Ok(SyncOutcome::Pushed { revision }) => {
                self.reload_after_sync();
                self.info(format!(
                    "{}（修订 r{revision}）",
                    crate::text::messages::PUSHED
                ));
            }
            Ok(_) => {}
            Err(e) => self.fail(format!("{e:#}")),
        }
        self.refresh_sync_status();
    }

    pub fn pull(&mut self) {
        let result = self.engine().and_then(|mut e| e.pull());
        match result {
            Ok(SyncOutcome::Pulled { revision }) => {
                self.reload_after_sync();
                self.info(format!(
                    "{}（修订 r{revision}）",
                    crate::text::messages::PULLED
                ));
            }
            Ok(_) => {}
            Err(e) => self.fail(format!("{e:#}")),
        }
        self.refresh_sync_status();
    }

    /// Resolve a conflict by keeping one side. `push`/`pull` are unconditional,
    /// so no state needs re-checking.
    pub fn resolve_conflict(&mut self, keep_local: bool) {
        self.conflict = None;
        if keep_local {
            self.push();
            if self.status.as_ref().is_some_and(|s| !s.error) {
                self.info("冲突已解决：保留本地，远端已覆盖");
            }
        } else {
            self.pull();
            if self.status.as_ref().is_some_and(|s| !s.error) {
                self.info("冲突已解决：保留远端，本地已覆盖");
            }
        }
    }

    pub fn delete_remote(&mut self) {
        let result = self.engine().and_then(|mut e| e.delete_remote());
        match result {
            Ok(()) => {
                self.settings.sync.forget_remote_state();
                let _ = self.save_settings();
                self.info("已删除远端 paste，本地 profile 未改动");
            }
            Err(e) => self.fail(format!("{e:#}")),
        }
        self.refresh_sync_status();
    }

    pub fn show_browser_url(&mut self) {
        match self.engine().and_then(|e| e.browser_url()) {
            Ok(url) => {
                self.sync.browser_url = Some(url);
                self.info("链接的 # 后面是加密密钥，请勿公开分享");
            }
            Err(e) => self.fail(format!("{e:#}")),
        }
    }

    // ---- settings -------------------------------------------------------

    pub fn set_sync_id(&mut self, value: &str) -> Result<()> {
        validate_sync_id(value)?;
        // A different id means a different paste, so what we knew about the old
        // remote says nothing about this one.
        if self.settings.sync.sync_id.as_deref() != Some(value) {
            self.settings.sync.forget_remote_state();
        }
        self.settings.sync.sync_id = Some(value.to_string());
        self.save_settings()
    }

    pub fn set_sync_key(&mut self, value: &str) -> Result<()> {
        if value.is_empty() {
            bail!("密钥不能为空");
        }
        // Fail now rather than at the first sync, while the user is looking at
        // the field they just typed.
        let id = self
            .settings
            .sync
            .sync_id
            .clone()
            .unwrap_or_else(|| "default".to_string());
        crate::sync::SyncCrypto::derive(value, &id)?;
        self.settings.sync.key = Some(value.to_string());
        self.save_settings()
    }

    pub fn set_endpoint(&mut self, value: &str) -> Result<()> {
        if value.is_empty() {
            bail!("endpoint 不能为空");
        }
        self.settings.sync.endpoint = value.to_string();
        self.save_settings()
    }

    pub fn set_expire(&mut self, value: &str) -> Result<()> {
        self.settings.sync.expire = value.to_string();
        self.save_settings()
    }

    pub fn set_device_name(&mut self, value: &str) -> Result<()> {
        self.settings.device_name = if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        };
        self.save_settings()
    }

    /// The key as it should be displayed: masked, and marked when it comes from
    /// the environment rather than the file, since editing the file will not
    /// change what a sync actually uses.
    pub fn key_display(&self) -> String {
        match self.settings.sync.effective_key() {
            None => "（未设置）".to_string(),
            Some(_) if std::env::var(crate::config::ENV_SYNC_KEY).is_ok_and(|v| !v.is_empty()) => {
                "（来自 ENVPICK_SYNC_KEY，文件中的值不生效）".to_string()
            }
            Some(k) => "*".repeat(k.chars().count().min(24)),
        }
    }
}
