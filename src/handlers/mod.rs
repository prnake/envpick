//! One module per command group, plus the shared load/save context.

pub mod profile;
pub mod shell_cmd;
pub mod sync_cmd;
pub mod ui;

use anyhow::Result;

use crate::cli::{Cli, Command, SyncAction};
use crate::config::{GLOBAL_PROFILE, Paths, Profile, ProfileStore, Settings};

/// Everything a command needs from disk, loaded once at startup.
pub struct Ctx {
    pub paths: Paths,
    pub settings: Settings,
    pub store: ProfileStore,
}

impl Ctx {
    pub fn load() -> Result<Self> {
        let paths = Paths::resolve()?;
        let settings = Settings::load(&paths.settings_file())?;
        let store = ProfileStore::load(&paths.profiles_file())?;
        Ok(Self {
            paths,
            settings,
            store,
        })
    }

    /// `use` always implies `global`, so a first run has to create it — an
    /// empty profile is still meaningful, it is where a user puts the handful
    /// of variables every shell should have.
    ///
    /// Only called by commands that write, so read-only commands never create
    /// files as a side effect.
    pub fn ensure_global(&mut self) -> Result<()> {
        if !self.store.contains(GLOBAL_PROFILE) {
            self.store.upsert(GLOBAL_PROFILE, Profile::default())?;
            self.save_store()?;
        }
        Ok(())
    }

    pub fn save_store(&self) -> Result<()> {
        self.paths.ensure_dir()?;
        self.store.save(&self.paths.profiles_file())
    }

    pub fn save_settings(&self) -> Result<()> {
        self.paths.ensure_dir()?;
        self.settings.save(&self.paths.settings_file())
    }
}

pub fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Init { shell } => shell_cmd::init(&shell),

        Command::Use { profiles } => {
            let mut ctx = Ctx::load()?;
            ctx.ensure_global()?;
            shell_cmd::use_profiles(&ctx, &profiles, false)
        }
        Command::Unuse { profiles, all } => {
            let ctx = Ctx::load()?;
            shell_cmd::unuse_profiles(&ctx, &profiles, all, false)
        }
        Command::Off => {
            let ctx = Ctx::load()?;
            shell_cmd::off(&ctx, false)
        }

        Command::List => profile::list(&Ctx::load()?),
        Command::Show { profile, plain } => profile::show(&Ctx::load()?, &profile, plain),
        Command::Status => profile::status(&Ctx::load()?),
        Command::Check { fix } => {
            if fix {
                let mut ctx = Ctx::load()?;
                profile::check(&mut ctx, true)
            } else {
                profile::check(&mut Ctx::load()?, false)
            }
        }
        Command::Edit => {
            let mut ctx = Ctx::load()?;
            ctx.ensure_global()?;
            profile::edit(&ctx)
        }
        Command::New { name } => {
            let mut ctx = Ctx::load()?;
            ctx.ensure_global()?;
            profile::new_profile(&mut ctx, &name)
        }
        Command::Rm { name, yes } => {
            let mut ctx = Ctx::load()?;
            profile::remove(&mut ctx, &name, yes)
        }
        Command::Set {
            profile,
            assignments,
        } => {
            let mut ctx = Ctx::load()?;
            ctx.ensure_global()?;
            profile::set_vars(&mut ctx, &profile, &assignments)
        }
        Command::Unset { profile, keys } => {
            let mut ctx = Ctx::load()?;
            profile::unset_vars(&mut ctx, &profile, &keys)
        }
        Command::Require { profile, deps } => {
            let mut ctx = Ctx::load()?;
            ctx.ensure_global()?;
            profile::require_deps(&mut ctx, &profile, &deps)
        }
        Command::Unrequire { profile, deps } => {
            let mut ctx = Ctx::load()?;
            profile::unrequire_deps(&mut ctx, &profile, &deps)
        }

        Command::Sync {
            action,
            keep_local,
            keep_remote,
        } => {
            let ctx = Ctx::load()?;
            match action {
                Some(SyncAction::Status) => sync_cmd::status(&ctx),
                Some(SyncAction::Push) => sync_cmd::push(&ctx),
                Some(SyncAction::Pull) => sync_cmd::pull(&ctx),
                Some(SyncAction::Init { id, key, key_stdin }) => {
                    let mut ctx = ctx;
                    sync_cmd::init(&mut ctx, id.as_deref(), key, key_stdin)
                }
                Some(SyncAction::Genid) => sync_cmd::genid(),
                Some(SyncAction::Url) => sync_cmd::url(&ctx),
                Some(SyncAction::Delete { yes }) => {
                    let mut ctx = ctx;
                    sync_cmd::delete(&mut ctx, yes)
                }
                None => sync_cmd::sync_smart(&ctx, keep_local, keep_remote),
            }
        }

        Command::Ui => {
            let mut ctx = Ctx::load()?;
            ctx.ensure_global()?;
            ui::run(&mut ctx)
        }

        // The shell integration's private entry point. `args` is the original
        // command line with `__shell` stripped, so the first element decides.
        Command::Shell { args } => {
            let ctx = Ctx::load()?;
            shell_cmd::dispatch(&ctx, &args)
        }
    }
}
