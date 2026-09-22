//! The sync state machine.
//!
//! Deciding what to do needs two independent facts:
//!
//! - **Is the local copy dirty?** — hash the local profiles and compare with
//!   the hash recorded at the last sync.
//! - **Did the remote move?** — read the remote document, hash its profiles,
//!   and compare with that same recorded hash.
//!
//! Both are comparisons of content we hold, so two machines with skewed clocks
//! still agree on who changed what, and neither can be fooled by a rewrite that
//! happens to reuse a revision number. Reading the remote to answer the second
//! question costs one small download, and it is the only way to answer it: the
//! deployment we talk to exposes no metadata endpoint (see [`super::pastebin`]),
//! so there is no server timestamp to compare against.
//!
//! | local dirty | remote moved | meaning                                  |
//! |-------------|--------------|------------------------------------------|
//! | no          | no           | up to date                               |
//! | yes         | no           | local edits → push                       |
//! | no          | yes          | another machine pushed → pull            |
//! | yes         | yes          | both changed → conflict, ask the user    |
//!
//! A remote we cannot decrypt is a fourth case, and it is deliberately *not*
//! folded into "moved": treating it as an ordinary change would make the obvious
//! "resolve the conflict" action overwrite data we never managed to read.

use anyhow::{Context, Result};

use crate::config::{Paths, ProfileStore, Settings};
use crate::sync::crypto::SyncCrypto;
use crate::sync::doc::{SyncDoc, profiles_hash};
use crate::sync::pastebin::{PastebinClient, PastebinError, paste_path};

/// What the remote looks like right now, as far as we can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteState {
    /// No paste under our name yet.
    Missing,
    /// Readable, and holding what we last synced.
    Synced(u64),
    /// Readable, but not holding what we last synced.
    Moved(u64),
    /// Present but not decryptable as ours.
    ///
    /// Either the key differs from the one that wrote it, or the stored bytes
    /// are corrupt. Both mean the same thing for safety: its contents are
    /// unknown, so nothing may overwrite it without the user saying so.
    Unreadable,
}

impl RemoteState {
    /// For the UI. Says what the remote *is*, not what to do about it — the
    /// decision belongs to [`SyncState`].
    pub fn label(&self) -> String {
        match self {
            RemoteState::Missing => "（无）".to_string(),
            RemoteState::Synced(rev) => format!("修订 r{rev}"),
            RemoteState::Moved(rev) => format!("修订 r{rev}（有变化）"),
            RemoteState::Unreadable => "存在，但无法解密".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    /// No sync id or key configured yet.
    Unconfigured,
    /// Configured, but nothing has been pushed to the server yet.
    RemoteMissing,
    /// Up to date with the server.
    UpToDate,
    /// Local edits not yet pushed.
    LocalAhead,
    /// Another machine pushed; nothing local to lose.
    RemoteAhead,
    /// Both sides changed — needs a decision.
    Diverged,
    /// The remote exists but is not ours to read — do not touch it.
    RemoteUnreadable,
}

impl SyncState {
    pub fn label(&self) -> &'static str {
        match self {
            SyncState::Unconfigured => "未配置",
            SyncState::RemoteMissing => "远端为空",
            SyncState::UpToDate => "已同步",
            SyncState::LocalAhead => "本地有改动",
            SyncState::RemoteAhead => "远端有改动",
            SyncState::Diverged => "冲突",
            SyncState::RemoteUnreadable => "远端无法解密",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SyncStatus {
    pub state: SyncState,
    pub remote: RemoteState,
    /// When the paste lapses, carried over from settings so callers can warn
    /// without also needing the settings to hand.
    pub remote_expires_at: Option<String>,
    /// Hash of the current local profiles.
    pub local_hash: String,
    pub local_profiles: usize,
}

impl SyncStatus {
    /// Seconds until the paste expires, if we know. Pastes *do* lapse, so the UI
    /// warns before that happens silently.
    pub fn seconds_until_expiry(&self) -> Option<i64> {
        let expiry = crate::clock::parse_rfc3339_epoch(self.remote_expires_at.as_deref()?)?;
        Some(expiry - crate::clock::now_epoch())
    }
}

/// What a sync attempt did, or what it needs the user to decide.
///
/// `Conflict` carries two whole documents inline, which makes this enum much
/// larger than its other variants. Boxing them would quiet the lint, but the
/// conflict path runs at most once per user-initiated command and the two
/// documents are what the caller actually needs — the indirection would cost
/// readability everywhere it is matched to save a copy nobody will measure.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum SyncOutcome {
    UpToDate,
    Pushed {
        revision: u64,
    },
    Pulled {
        revision: u64,
    },
    /// Both sides changed. Nothing has been written; the caller chooses.
    Conflict {
        local: SyncDoc,
        remote: SyncDoc,
    },
}

pub struct SyncEngine {
    client: PastebinClient,
    crypto: SyncCrypto,
    paths: Paths,
    settings: Settings,
    /// Whether `settings` has changes not yet on disk.
    dirty_settings: bool,
}

impl SyncEngine {
    /// Build an engine, or explain what's missing.
    pub fn new(paths: Paths, settings: Settings) -> Result<Self> {
        let sync_id = settings
            .sync
            .sync_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!(crate::text::errors::SYNC_NOT_CONFIGURED))?;
        let key = settings
            .sync
            .effective_key()
            .ok_or_else(|| anyhow::anyhow!(crate::text::errors::SYNC_KEY_MISSING))?;

        let crypto = SyncCrypto::derive(&key, &sync_id)?;
        let client = PastebinClient::new(&settings.sync.endpoint, settings.sync.auth.as_deref())?;

        Ok(Self {
            client,
            crypto,
            paths,
            settings,
            dirty_settings: false,
        })
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn client(&self) -> &PastebinClient {
        &self.client
    }

    /// The URL that decrypts this sync's paste in a browser. The key rides in
    /// the fragment, which browsers never send to the server.
    pub fn browser_url(&self) -> Result<String> {
        Ok(format!(
            "{}/d{}#{}",
            self.client.endpoint(),
            paste_path(&self.settings.sync.paste_name()?),
            self.crypto.browser_key()
        ))
    }

    /// The URL that can overwrite or delete the paste. Worth showing only
    /// because it does *not* reveal the encryption key.
    pub fn manage_url(&self) -> Result<String> {
        Ok(format!(
            "{}{}:{}",
            self.client.endpoint(),
            paste_path(&self.settings.sync.paste_name()?),
            self.crypto.manage_password()
        ))
    }

    fn load_local(&self) -> Result<ProfileStore> {
        ProfileStore::load(&self.paths.profiles_file())
    }

    fn remote_name(&self) -> Result<String> {
        self.settings.sync.paste_name()
    }

    /// Read the remote and say what it is, without changing anything.
    ///
    /// "Moved" is decided by hashing the remote's profiles and comparing that
    /// with the hash recorded at the last sync — the same kind of comparison,
    /// against the same recorded value, as the local-dirty test. Comparing the
    /// document's `revision` instead would be cheaper but wrong: a machine that
    /// resolves a conflict by overwriting the remote can pick a revision
    /// another machine already used (both start from what they last saw), and
    /// then the superseded machine reads back its own number and concludes it is
    /// up to date while the remote holds someone else's copy. Content cannot
    /// collide that way.
    fn read_remote(&self) -> Result<RemoteState> {
        let name = self.remote_name()?;
        let Some(wire) = self.client.fetch(&name)? else {
            return Ok(RemoteState::Missing);
        };
        match self.crypto.decrypt(&wire) {
            Ok(bytes) => {
                let doc = SyncDoc::from_bytes(&bytes)?;
                let remote_hash = profiles_hash(&doc.profiles)?;
                Ok(
                    if self.settings.sync.last_synced_hash.as_deref() == Some(&remote_hash) {
                        RemoteState::Synced(doc.revision)
                    } else {
                        // Includes the never-synced case: a document we have no
                        // recorded hash for is one we have not seen.
                        RemoteState::Moved(doc.revision)
                    },
                )
            }
            // Not decryptable is reported, never raised: `status` is how the
            // user finds out, so it must not be the thing that fails.
            Err(_) => Ok(RemoteState::Unreadable),
        }
    }

    /// Compare local state against the server without changing anything.
    pub fn status(&self) -> Result<SyncStatus> {
        let store = self.load_local()?;
        let local_hash = profiles_hash(&store.profiles)?;
        let remote = self.read_remote()?;

        let local_dirty = self
            .settings
            .sync
            .last_synced_hash
            .as_deref()
            .map(|known| known != local_hash)
            // Never synced. Empty local profiles are not an edit.
            .unwrap_or(!store.profiles.is_empty());

        let state = match &remote {
            RemoteState::Unreadable => SyncState::RemoteUnreadable,
            // "Local ahead" would be the wrong word here: there is nothing to
            // be ahead *of*. Both cases push, so the only thing riding on the
            // distinction is which sentence the user reads, and "远端为空" is
            // the true one whether or not the local side also has edits.
            RemoteState::Missing => SyncState::RemoteMissing,
            RemoteState::Synced(_) | RemoteState::Moved(_) => {
                let remote_moved = matches!(remote, RemoteState::Moved(_));
                match (local_dirty, remote_moved) {
                    (false, false) => SyncState::UpToDate,
                    (true, false) => SyncState::LocalAhead,
                    (false, true) => SyncState::RemoteAhead,
                    (true, true) => SyncState::Diverged,
                }
            }
        };

        Ok(SyncStatus {
            state,
            remote,
            remote_expires_at: self.settings.sync.remote_expires_at.clone(),
            local_hash,
            local_profiles: store.profiles.len(),
        })
    }

    /// Download and decrypt the remote document, without applying it.
    ///
    /// `Ok(None)` covers both "no paste" and "there but not ours" — callers that
    /// need to tell those apart read [`Self::read_remote`] through
    /// [`Self::status`] first.
    pub fn fetch_remote(&self) -> Result<Option<SyncDoc>> {
        let name = self.remote_name()?;
        let Some(wire) = self.client.fetch(&name)? else {
            return Ok(None);
        };
        Ok(Some(SyncDoc::from_bytes(&self.crypto.decrypt(&wire)?)?))
    }

    /// Do the obvious thing. Returns `Conflict` rather than guessing when both
    /// sides changed, and refuses outright when the remote cannot be read.
    pub fn sync(&mut self) -> Result<SyncOutcome> {
        match self.status()?.state {
            SyncState::UpToDate => Ok(SyncOutcome::UpToDate),
            SyncState::LocalAhead | SyncState::RemoteMissing => self.push(),
            SyncState::RemoteAhead => self.pull(),
            SyncState::Diverged => {
                let remote = self
                    .fetch_remote()?
                    .ok_or_else(|| anyhow::anyhow!(crate::text::errors::REMOTE_NOT_FOUND))?;
                Ok(SyncOutcome::Conflict {
                    local: self.local_doc()?,
                    remote,
                })
            }
            // Pushing here would overwrite a document we could not read, which
            // is exactly the data we have no way to recover. `sync push` is the
            // escape hatch, and it says in its help that it overwrites.
            SyncState::RemoteUnreadable => {
                anyhow::bail!(crate::text::errors::REMOTE_UNREADABLE)
            }
            SyncState::Unconfigured => {
                anyhow::bail!(crate::text::errors::SYNC_NOT_CONFIGURED)
            }
        }
    }

    fn next_revision(&self) -> u64 {
        self.settings
            .sync
            .last_remote_revision
            .unwrap_or(0)
            .saturating_add(1)
    }

    /// Encrypt the local profiles and upload. Overwrites the remote
    /// unconditionally — deciding *whether* that is safe is [`Self::sync`]'s job.
    pub fn push(&mut self) -> Result<SyncOutcome> {
        let store = self.load_local()?;
        let revision = self.next_revision();
        let doc = SyncDoc::from_store(
            &store,
            &self.settings.device_id,
            self.settings.device_name.as_deref(),
            revision,
        );
        let local_hash = profiles_hash(&store.profiles)?;

        // `encrypt` adds the magic marker that lets a later read tell "wrong
        // key" apart from "not our data".
        let wire = self.crypto.encrypt(&doc.to_bytes()?)?;

        let name = self.remote_name()?;
        let password = self.crypto.manage_password().to_string();
        let expire = self.settings.sync.expire.clone();

        // `PUT` needs the paste to exist and `POST` needs it not to. Having
        // synced before is what tells us which is true; if the paste expired in
        // the meantime (or another machine created it first) we recover from
        // the mismatch rather than probing with an extra round trip.
        let resp = if self.settings.sync.last_remote_revision.is_some() {
            match self.client.update(&name, &password, &wire, &expire) {
                Ok(r) => r,
                Err(e) if is(&e, |p| matches!(p, PastebinError::NotFound)) => {
                    self.client.create(&name, &password, &wire, &expire)?
                }
                Err(e) => return Err(e),
            }
        } else {
            match self.client.create(&name, &password, &wire, &expire) {
                Ok(r) => r,
                Err(e) if is(&e, |p| matches!(p, PastebinError::NameTaken)) => {
                    self.client.update(&name, &password, &wire, &expire)?
                }
                Err(e) => return Err(e),
            }
        };

        self.record_success(&local_hash, revision, resp.expiration_seconds());
        self.save_settings()?;
        Ok(SyncOutcome::Pushed { revision })
    }

    /// Download the remote document and make it the local one. Unconditional,
    /// so "keep the remote copy" during a conflict needs no second guess.
    pub fn pull(&mut self) -> Result<SyncOutcome> {
        let doc = self
            .fetch_remote()?
            .ok_or_else(|| anyhow::anyhow!(crate::text::errors::REMOTE_NOT_FOUND))?;

        let store = doc.to_store();
        store.save(&self.paths.profiles_file())?;

        let local_hash = profiles_hash(&store.profiles)?;
        // No granted lifetime on a read: pulling does not renew the paste, so
        // whatever we knew about its expiry still stands.
        self.record_success(&local_hash, doc.revision, None);
        self.save_settings()?;
        Ok(SyncOutcome::Pulled {
            revision: doc.revision,
        })
    }

    /// Record a successful transfer so the next `status()` is accurate.
    fn record_success(&mut self, local_hash: &str, revision: u64, granted_seconds: Option<u64>) {
        self.settings.sync.last_synced_hash = Some(local_hash.to_string());
        self.settings.sync.last_remote_revision = Some(revision);
        self.settings.sync.last_synced_at = Some(crate::clock::now_rfc3339());
        // The server clamps `e` to its own maximum and reports back what it
        // actually granted; record that rather than assuming our request won.
        if let Some(secs) = granted_seconds {
            self.settings.sync.remote_expiration_seconds = Some(secs);
            self.settings.sync.remote_expires_at = Some(crate::clock::format_epoch(
                crate::clock::now_epoch().saturating_add(secs as i64),
            ));
        }
        self.dirty_settings = true;
    }

    /// Delete the remote paste. The local profiles are left alone.
    pub fn delete_remote(&mut self) -> Result<()> {
        let name = self.remote_name()?;
        let password = self.crypto.manage_password().to_string();
        self.client.delete(&name, &password)?;
        self.settings.sync.forget_remote_state();
        // `save_settings` writes only what has been marked dirty, and forgetting
        // the remote is exactly such a change. Without this the file keeps the
        // old revision, sync time and expiry, and the next `status` warns about
        // a paste that no longer exists.
        self.dirty_settings = true;
        self.save_settings()
    }

    pub fn save_settings(&mut self) -> Result<()> {
        if self.dirty_settings {
            self.settings.save(&self.paths.settings_file())?;
            self.dirty_settings = false;
        }
        Ok(())
    }

    /// The document we would push right now, for previews and diffs.
    pub fn local_doc(&self) -> Result<SyncDoc> {
        let store = self.load_local().context("读取本地 profiles 失败")?;
        Ok(SyncDoc::from_store(
            &store,
            &self.settings.device_id,
            self.settings.device_name.as_deref(),
            self.next_revision(),
        ))
    }
}

fn is(e: &anyhow::Error, pred: fn(&PastebinError) -> bool) -> bool {
    e.downcast_ref::<PastebinError>().is_some_and(pred)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Profile;
    use crate::sync::mock::{mock, upload_json};

    /// What the deployment grants when we ask for `90d`.
    const EXP_SECS: u64 = 7_776_000;

    /// A temp config dir that cleans itself up.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            let mut buf = [0u8; 6];
            rand::fill(&mut buf);
            let uniq: String = buf.iter().map(|b| format!("{b:02x}")).collect();
            p.push(format!("envpick-test-{tag}-{uniq}"));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn paths(&self) -> Paths {
            Paths {
                root: self.0.clone(),
            }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn settings_for(endpoint: &str) -> Settings {
        let mut s = Settings::default();
        s.sync.endpoint = endpoint.to_string();
        s.sync.sync_id = Some("testsync".into());
        s.sync.key = Some("passphrase".into());
        s
    }

    fn write_profiles(paths: &Paths, vars: &[(&str, &str)]) {
        let mut store = ProfileStore::default();
        let mut p = Profile::default();
        for (k, v) in vars {
            p.vars.insert((*k).to_string(), (*v).to_string());
        }
        store.profiles.insert("work".into(), p);
        store.save(&paths.profiles_file()).unwrap();
    }

    fn engine(dir: &TempDir, settings: Settings) -> SyncEngine {
        SyncEngine::new(dir.paths(), settings).unwrap()
    }

    /// Ciphertext for a document as *another machine* would have left it, so
    /// pull and status tests exercise real decryption rather than a canned
    /// string. Detection reads the document itself — there is no metadata
    /// endpoint to hand back a timestamp (see [`super::super::pastebin`]) — so
    /// the mock has to answer every read with genuinely encrypted bytes.
    fn wire_with(passphrase: &str, vars: &[(&str, &str)], revision: u64) -> String {
        let mut store = ProfileStore::default();
        let mut p = Profile::default();
        for (k, v) in vars {
            p.vars.insert((*k).to_string(), v.to_string());
        }
        store.profiles.insert("work".into(), p);

        let doc = SyncDoc::from_store(&store, "other-machine", Some("laptop"), revision);
        let crypto = SyncCrypto::derive(passphrase, "testsync").unwrap();
        crypto.encrypt(&doc.to_bytes().unwrap()).unwrap()
    }

    /// The same, under the id/key every test here uses, for the tests that only
    /// care which revision the remote is at — nothing is compared but that.
    fn wire(revision: u64) -> String {
        wire_of(&[("A", "1")], revision)
    }

    /// The same, with content the test actually asserts on.
    fn wire_of(vars: &[(&str, &str)], revision: u64) -> String {
        wire_with("passphrase", vars, revision)
    }

    /// The hash the engine would record for these profiles.
    fn hash_of(vars: &[(&str, &str)]) -> String {
        let mut store = ProfileStore::default();
        let mut p = Profile::default();
        for (k, v) in vars {
            p.vars.insert((*k).to_string(), v.to_string());
        }
        store.profiles.insert("work".into(), p);
        profiles_hash(&store.profiles).unwrap()
    }

    /// The two requests a bare `sync()` makes against an empty server: its own
    /// status probe, then the upload. A test that also calls `status()`
    /// beforehand needs one more probe in front (see
    /// [`first_sync_after_a_probe`]).
    fn first_sync() -> Vec<(u16, String)> {
        vec![(404, "nope".into()), (200, upload_json(EXP_SECS))]
    }

    /// As above, but for a test that inspects `status()` before syncing.
    fn first_sync_after_a_probe() -> Vec<(u16, String)> {
        let mut r = vec![(404, "nope".into())];
        r.extend(first_sync());
        r
    }

    #[test]
    fn first_sync_creates_the_paste() {
        let dir = TempDir::new("first");
        write_profiles(&dir.paths(), &[("EDITOR", "nvim")]);
        let m = mock(first_sync_after_a_probe());
        let mut e = engine(&dir, settings_for(&m.base));

        assert_eq!(e.status().unwrap().state, SyncState::RemoteMissing);
        match e.sync().unwrap() {
            SyncOutcome::Pushed { revision } => assert_eq!(revision, 1),
            other => panic!("expected push, got {other:?}"),
        }

        let reqs = m.requests();
        assert_eq!(reqs.len(), 3, "two probes and one upload");
        assert_eq!(reqs[2].method, "POST");
        assert!(reqs[2].body.contains("name=\"n\""));
        assert!(reqs[2].body.contains("ep-testsync"));

        // Recorded, so the next status is accurate.
        let saved = Settings::load(&dir.paths().settings_file()).unwrap();
        assert_eq!(saved.sync.remote_expiration_seconds, Some(EXP_SECS));
        assert!(
            saved.sync.remote_expires_at.is_some(),
            "the granted lifetime should be turned into a date"
        );
        assert_eq!(saved.sync.last_remote_revision, Some(1));
        assert!(saved.sync.last_synced_hash.is_some());
    }

    /// The uploaded bytes must be ciphertext — the whole point of the feature.
    #[test]
    fn push_uploads_ciphertext_not_plaintext() {
        let dir = TempDir::new("cipher");
        write_profiles(&dir.paths(), &[("SECRET_TOKEN", "hunter2")]);
        let m = mock(first_sync());
        let mut e = engine(&dir, settings_for(&m.base));
        e.sync().unwrap();

        let body = m.requests().last().unwrap().body.clone();
        assert!(
            !body.contains("hunter2"),
            "plaintext leaked into the upload:\n{body}"
        );
        assert!(!body.contains("SECRET_TOKEN"), "variable name leaked");

        // And it really is decryptable by the holder of the key.
        let start = body.find("\r\n\r\n").unwrap() + 4;
        let end = body[start..].find("\r\n--").unwrap() + start;
        let c = SyncCrypto::derive("passphrase", "testsync").unwrap();
        let doc = SyncDoc::from_bytes(&c.decrypt(&body[start..end]).unwrap()).unwrap();
        assert_eq!(doc.profiles["work"].vars["SECRET_TOKEN"], "hunter2");
    }

    #[test]
    fn up_to_date_when_nothing_changed() {
        let dir = TempDir::new("uptodate");
        write_profiles(&dir.paths(), &[("A", "1")]);
        let mut responses = first_sync();
        // Two probes: `status()` and then `sync()`'s own check. Both see the
        // remote holding what we just pushed — `wire(1)` is the same content as
        // the local file, which is what "unchanged" now means.
        responses.push((200, wire(1)));
        responses.push((200, wire(1)));
        let m = mock(responses);
        let mut e = engine(&dir, settings_for(&m.base));
        e.sync().unwrap();

        assert_eq!(e.status().unwrap().state, SyncState::UpToDate);
        // An up-to-date sync must not upload anything.
        let before = m.count();
        assert!(matches!(e.sync().unwrap(), SyncOutcome::UpToDate));
        assert_eq!(m.count(), before + 1, "up-to-date sync should only probe");
    }

    #[test]
    fn local_edit_then_sync_pushes() {
        let dir = TempDir::new("localedit");
        write_profiles(&dir.paths(), &[("A", "1")]);
        let mut responses = first_sync();
        responses.push((200, wire(1))); // status(): remote unchanged
        responses.push((200, wire(1))); // sync()'s own status check
        responses.push((200, upload_json(EXP_SECS))); // PUT
        let m = mock(responses);
        let mut e = engine(&dir, settings_for(&m.base));
        e.sync().unwrap();

        write_profiles(&dir.paths(), &[("A", "2")]);
        assert_eq!(e.status().unwrap().state, SyncState::LocalAhead);
        match e.sync().unwrap() {
            SyncOutcome::Pushed { revision } => assert_eq!(revision, 2),
            other => panic!("expected push, got {other:?}"),
        }
        assert_eq!(m.requests().last().unwrap().method, "PUT");
    }

    #[test]
    fn remote_edit_then_sync_pulls() {
        let dir = TempDir::new("remoteedit");
        write_profiles(&dir.paths(), &[("A", "1")]);

        // First machine pushes, establishing the recorded remote state.
        let m1 = mock(first_sync());
        engine(&dir, settings_for(&m1.base)).sync().unwrap();
        drop(m1);

        // Second server: the document is the other machine's, at a revision we
        // have never recorded.
        let theirs = wire_of(&[("A", "from-other-machine")], 5);
        let m2 = mock(vec![
            (200, theirs.clone()), // status()
            (200, theirs.clone()), // sync()'s status check
            (200, theirs.clone()), // pull's fetch
            (200, theirs),         // status() after the pull
        ]);
        let mut s2 = Settings::load(&dir.paths().settings_file()).unwrap();
        s2.sync.endpoint = m2.base.clone();
        let mut e2 = SyncEngine::new(dir.paths(), s2).unwrap();

        assert_eq!(e2.status().unwrap().state, SyncState::RemoteAhead);
        match e2.sync().unwrap() {
            SyncOutcome::Pulled { revision } => assert_eq!(revision, 5),
            other => panic!("expected pull, got {other:?}"),
        }

        let applied = ProfileStore::load(&dir.paths().profiles_file()).unwrap();
        assert_eq!(applied.profiles["work"].vars["A"], "from-other-machine");
        assert_eq!(e2.status().unwrap().state, SyncState::UpToDate);
    }

    /// Resolving a conflict by overwriting the remote lets two machines land on
    /// the same revision number: both compute "what I last saw, plus one", and
    /// the one that loses the race never learns it lost. If detection compared
    /// revisions, the superseded machine would read its own number back and
    /// report "已同步" while the remote held the other machine's copy — its own
    /// push silently gone, with nothing anywhere to notice.
    #[test]
    fn a_rewrite_that_reuses_a_revision_is_still_noticed() {
        let dir = TempDir::new("samerev");
        write_profiles(&dir.paths(), &[("A", "mine")]);
        let m = mock(vec![(200, wire_of(&[("A", "theirs")], 2))]);

        let mut s = settings_for(&m.base);
        // Exactly what machine A would have recorded after pushing r2 itself.
        s.sync.last_remote_revision = Some(2);
        s.sync.last_synced_hash = Some(hash_of(&[("A", "mine")]));
        let e = SyncEngine::new(dir.paths(), s).unwrap();

        assert_eq!(e.status().unwrap().state, SyncState::RemoteAhead);
    }

    #[test]
    fn both_sides_changed_reports_a_conflict_and_writes_nothing() {
        let dir = TempDir::new("conflict");
        write_profiles(&dir.paths(), &[("A", "1")]);
        let m1 = mock(first_sync());
        engine(&dir, settings_for(&m1.base)).sync().unwrap();
        drop(m1);

        let theirs = wire_of(&[("A", "theirs")], 9);
        let m2 = mock(vec![
            (200, theirs.clone()), // status(): remote moved
            (200, theirs.clone()), // sync()'s status check
            (200, theirs),         // fetch_remote for the conflict preview
        ]);

        write_profiles(&dir.paths(), &[("A", "mine")]); // local moved too

        let mut s2 = Settings::load(&dir.paths().settings_file()).unwrap();
        s2.sync.endpoint = m2.base.clone();
        let mut e2 = SyncEngine::new(dir.paths(), s2).unwrap();

        assert_eq!(e2.status().unwrap().state, SyncState::Diverged);
        match e2.sync().unwrap() {
            SyncOutcome::Conflict { local, remote } => {
                assert_eq!(local.profiles["work"].vars["A"], "mine");
                assert_eq!(remote.profiles["work"].vars["A"], "theirs");
            }
            other => panic!("expected conflict, got {other:?}"),
        }

        // The local file must be untouched until the user decides.
        let on_disk = ProfileStore::load(&dir.paths().profiles_file()).unwrap();
        assert_eq!(on_disk.profiles["work"].vars["A"], "mine");
        assert!(
            m2.requests().iter().all(|r| r.method == "GET"),
            "a conflict must not write: {:?}",
            m2.requests().iter().map(|r| &r.method).collect::<Vec<_>>()
        );
    }

    #[test]
    fn resolving_a_conflict_by_keeping_local_pushes() {
        let dir = TempDir::new("keeplocal");
        write_profiles(&dir.paths(), &[("A", "mine")]);
        let m = mock(vec![
            (200, wire(5)),               // status(): remote moved past r3
            (200, upload_json(EXP_SECS)), // PUT
        ]);
        let mut s = settings_for(&m.base);
        s.sync.last_remote_revision = Some(3);
        s.sync.last_synced_hash = Some("stale".into()); // so local looks dirty

        let mut e = SyncEngine::new(dir.paths(), s).unwrap();
        assert_eq!(e.status().unwrap().state, SyncState::Diverged);
        match e.push().unwrap() {
            SyncOutcome::Pushed { revision } => assert_eq!(revision, 4),
            other => panic!("expected push, got {other:?}"),
        }
        assert_eq!(m.requests().last().unwrap().method, "PUT");
        // The recorded revision advances, so conflicts resolve forward.
        let saved = Settings::load(&dir.paths().settings_file()).unwrap();
        assert_eq!(saved.sync.last_remote_revision, Some(4));
    }

    #[test]
    fn resolving_a_conflict_by_keeping_remote_pulls() {
        let dir = TempDir::new("keepremote");
        write_profiles(&dir.paths(), &[("A", "mine")]);
        let m = mock(vec![(200, wire_of(&[("A", "theirs")], 7))]);
        let mut s2 = settings_for(&m.base);
        s2.sync.last_remote_revision = Some(3);
        s2.sync.last_synced_hash = Some("stale".into());

        let mut e = SyncEngine::new(dir.paths(), s2).unwrap();
        match e.pull().unwrap() {
            SyncOutcome::Pulled { revision } => assert_eq!(revision, 7),
            other => panic!("expected pull, got {other:?}"),
        }
        let applied = ProfileStore::load(&dir.paths().profiles_file()).unwrap();
        assert_eq!(applied.profiles["work"].vars["A"], "theirs");
    }

    #[test]
    fn pull_fails_clearly_on_the_wrong_key() {
        let dir = TempDir::new("wrongkey");
        write_profiles(&dir.paths(), &[("A", "1")]);
        let m = mock(vec![(
            200,
            wire_with("a-different-passphrase", &[("A", "x")], 1),
        )]);

        let mut e = engine(&dir, settings_for(&m.base));
        let err = e.pull().unwrap_err().to_string();
        assert!(err.contains("解密失败"), "got: {err}");
        // A failed pull must not have written anything.
        let on_disk = ProfileStore::load(&dir.paths().profiles_file()).unwrap();
        assert_eq!(on_disk.profiles["work"].vars["A"], "1");
    }

    /// The remote exists but is not ours to read. It must be *reported* — the
    /// whole point of `status` is to be how the user finds this out — and it
    /// must never be overwritten by the ordinary sync path, because its
    /// contents are exactly the data we cannot recover.
    #[test]
    fn a_remote_we_cannot_decrypt_is_reported_but_never_overwritten() {
        let dir = TempDir::new("unreadable");
        write_profiles(&dir.paths(), &[("A", "1")]);
        // Three reads and no writes: `status`, then `sync`'s own probe (which
        // must fail *after* looking, not by assuming), then `status` again.
        let theirs = wire_with("a-different-passphrase", &[("A", "x")], 1);
        let m = mock(vec![
            (200, theirs.clone()),
            (200, theirs.clone()),
            (200, theirs),
        ]);
        let mut e = engine(&dir, settings_for(&m.base));

        assert_eq!(e.status().unwrap().state, SyncState::RemoteUnreadable);
        let err = e.sync().unwrap_err().to_string();
        assert!(err.contains("无法解密"), "got: {err}");
        assert_eq!(e.status().unwrap().state, SyncState::RemoteUnreadable);

        assert!(
            m.requests().iter().all(|r| r.method == "GET"),
            "nothing may be written over a remote we could not read: {:?}",
            m.requests()
                .iter()
                .map(|r| r.method.clone())
                .collect::<Vec<_>>()
        );
        // The local profiles are untouched too.
        let on_disk = ProfileStore::load(&dir.paths().profiles_file()).unwrap();
        assert_eq!(on_disk.profiles["work"].vars["A"], "1");
    }

    /// Anyone can take a free name on a public pastebin. Whatever ends up at
    /// ours is not necessarily what we wrote, and the answer is the same as for
    /// a wrong key: read it, say so, do not touch it.
    #[test]
    fn a_remote_holding_someone_elses_paste_is_unreadable_too() {
        let dir = TempDir::new("notours");
        write_profiles(&dir.paths(), &[("A", "1")]);
        let m = mock(vec![(200, "just some text someone else pasted".into())]);
        let e = engine(&dir, settings_for(&m.base));

        assert_eq!(e.status().unwrap().state, SyncState::RemoteUnreadable);
    }

    #[test]
    fn push_recovers_when_the_paste_expired_before_the_update() {
        let dir = TempDir::new("recover404");
        write_profiles(&dir.paths(), &[("A", "1")]);
        let m = mock(vec![
            (404, "gone".into()),         // PUT: expired in the meantime
            (200, upload_json(EXP_SECS)), // POST fallback
        ]);
        let mut s = settings_for(&m.base);
        s.sync.last_remote_revision = Some(4);
        let mut e = SyncEngine::new(dir.paths(), s).unwrap();

        match e.push().unwrap() {
            SyncOutcome::Pushed { revision } => assert_eq!(revision, 5),
            other => panic!("expected push, got {other:?}"),
        }
        let methods: Vec<_> = m.requests().iter().map(|r| r.method.clone()).collect();
        assert_eq!(methods, vec!["PUT", "POST"]);
    }

    #[test]
    fn push_recovers_when_another_machine_created_the_paste_first() {
        let dir = TempDir::new("recover409");
        write_profiles(&dir.paths(), &[("A", "1")]);
        let m = mock(vec![
            (409, "taken".into()),        // POST: someone else got there first
            (200, upload_json(EXP_SECS)), // PUT fallback
        ]);
        let mut e = engine(&dir, settings_for(&m.base));

        match e.push().unwrap() {
            SyncOutcome::Pushed { .. } => {}
            other => panic!("expected push, got {other:?}"),
        }
        let methods: Vec<_> = m.requests().iter().map(|r| r.method.clone()).collect();
        assert_eq!(methods, vec!["POST", "PUT"]);
    }

    #[test]
    fn delete_removes_the_remote_and_forgets_it() {
        let dir = TempDir::new("delete");
        write_profiles(&dir.paths(), &[("A", "1")]);
        let m = mock(vec![(200, "the paste will be deleted in seconds".into())]);
        let mut s = settings_for(&m.base);
        s.sync.last_remote_revision = Some(1);
        s.sync.last_synced_hash = Some("h".into());
        s.sync.last_synced_at = Some("2026-09-20T00:00:00Z".into());
        s.sync.remote_expiration_seconds = Some(EXP_SECS);
        s.sync.remote_expires_at = Some("2026-12-19T00:00:00Z".into());
        // Written *before* the delete: `Settings::load` answers with defaults
        // for a file that isn't there, so asserting on a file that never
        // existed passes no matter what the delete does.
        s.save(&dir.paths().settings_file()).unwrap();

        let mut e = SyncEngine::new(dir.paths(), s).unwrap();
        e.delete_remote().unwrap();
        assert_eq!(m.requests()[0].method, "DELETE");

        let saved = Settings::load(&dir.paths().settings_file()).unwrap();
        assert!(saved.sync.last_remote_revision.is_none());
        assert!(saved.sync.last_synced_hash.is_none());
        // Nothing to expire any more, so the countdown must go too — otherwise
        // the next status would warn about a paste that no longer exists.
        assert!(saved.sync.remote_expires_at.is_none());
        assert!(saved.sync.last_synced_at.is_none());
        // The local profiles survive a remote deletion.
        assert!(dir.paths().profiles_file().exists());
    }

    #[test]
    fn browser_url_puts_the_key_in_the_fragment_only() {
        let dir = TempDir::new("url");
        let m = mock(vec![]);
        let e = engine(&dir, settings_for(&m.base));
        let url = e.browser_url().unwrap();

        assert!(url.starts_with(&format!("{}/d/~ep-testsync#", m.base)));
        // The fragment never reaches the server, and the manage password must
        // not appear in a URL that could be pasted somewhere public.
        let passphrase = e.settings().sync.key.clone().unwrap();
        assert!(!url.contains(&passphrase));
        let manage = e.manage_url().unwrap();
        let manage_password = manage.rsplit(':').next().unwrap();
        assert!(!url.contains(manage_password));
        assert!(!manage.contains(&passphrase));
    }

    #[test]
    fn unconfigured_sync_explains_itself() {
        let dir = TempDir::new("unconfigured");
        let mut s = Settings::default();
        s.sync.sync_id = None;
        let Err(err) = SyncEngine::new(dir.paths(), s) else {
            panic!("a settings file with no sync_id should not build an engine");
        };
        assert!(err.to_string().contains("尚未配置"), "got: {err}");

        // Only meaningful when the ambient environment isn't supplying a key.
        if std::env::var(crate::config::ENV_SYNC_KEY).is_err() {
            let mut s = Settings::default();
            s.sync.sync_id = Some("abc".into());
            s.sync.key = None;
            let Err(err) = SyncEngine::new(dir.paths(), s) else {
                panic!("a settings file with no key should not build an engine");
            };
            assert!(err.to_string().contains("密钥"), "got: {err}");
        }
    }

    /// The countdown is computed against a stored instant rather than a
    /// server-supplied timestamp, because there is no metadata endpoint to
    /// supply one. What matters is that a far-future expiry reads as future and
    /// an unknown expiry claims nothing.
    #[test]
    fn expiry_countdown_is_computed_from_the_recorded_instant() {
        let status = SyncStatus {
            state: SyncState::UpToDate,
            remote: RemoteState::Synced(1),
            remote_expires_at: Some("2099-01-01T00:00:00Z".into()),
            local_hash: String::new(),
            local_profiles: 0,
        };
        let secs = status.seconds_until_expiry().unwrap();
        assert!(
            secs > 0,
            "a 2099 expiry should be in the future, got {secs}"
        );

        // No expiry information means no claim about one.
        let none = SyncStatus {
            remote_expires_at: None,
            ..status.clone()
        };
        assert!(none.seconds_until_expiry().is_none());
    }
}
