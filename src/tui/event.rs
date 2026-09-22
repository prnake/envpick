//! Key handling.
//!
//! Overlays are checked before the active view, so a popup always gets the
//! keystroke first — otherwise `q` inside a text field would quit the app
//! instead of typing a letter.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::tui::app::{App, Confirm, ConfirmPurpose, Input, Purpose, View};

pub fn handle(app: &mut App, key: KeyEvent) {
    // Esc closes whatever is on top, innermost first.
    if app.input.is_some() {
        handle_input(app, key);
        return;
    }
    if app.confirm.is_some() {
        handle_confirm(app, key);
        return;
    }
    if app.conflict.is_some() {
        handle_conflict(app, key);
        return;
    }
    if app.show_help {
        app.show_help = false;
        return;
    }

    match key.code {
        KeyCode::Char('q') => {
            app.quit = true;
            return;
        }
        KeyCode::Char('?') => {
            app.show_help = true;
            return;
        }
        KeyCode::Tab => {
            app.view = app.view.shifted(1);
            app.status = None;
            clamp_rows(app);
            return;
        }
        KeyCode::BackTab => {
            app.view = app.view.shifted(-1);
            app.status = None;
            clamp_rows(app);
            return;
        }
        _ => {}
    }

    // Shortcuts that mean the same thing everywhere, so they do not have to be
    // repeated per view.
    match key.code {
        KeyCode::Char('s') if !matches!(app.view, View::Editor) => {
            app.sync_now();
            return;
        }
        KeyCode::Char('p') if !matches!(app.view, View::Editor) => {
            // Pushing over a remote we could not decrypt is the one sync action
            // that destroys something unrecoverable. It stays available — it is
            // the escape hatch the CLI documents too — but not on one keystroke
            // that reads like a plain "push my changes".
            if app.remote_unreadable() {
                app.confirm = Some(Confirm {
                    title: "覆盖无法解密的远端".to_string(),
                    body: vec![
                        "远端 paste 不是本工具写的数据，或密钥与该 paste 创建时不一致。"
                            .to_string(),
                        "推送会覆盖它，覆盖之后原内容无法恢复。".to_string(),
                    ],
                    purpose: ConfirmPurpose::PushOverUnreadable,
                });
            } else {
                app.push();
            }
            return;
        }
        KeyCode::Char('u') if !matches!(app.view, View::Editor) => {
            app.pull();
            return;
        }
        KeyCode::Char('g') if app.view == View::Sync => {
            app.show_browser_url();
            return;
        }
        KeyCode::Char('r') if app.view == View::Sync => {
            app.refresh_sync_status();
            app.info("已重新检查远端状态");
            return;
        }
        KeyCode::Char('D') if app.view == View::Sync => {
            app.confirm = Some(Confirm {
                title: "删除远端 paste".to_string(),
                body: vec![
                    "这会删除远端的 paste，其他机器将无法再同步到它。".to_string(),
                    "本地 profile 不会被改动。".to_string(),
                ],
                purpose: ConfirmPurpose::DeleteRemote,
            });
            return;
        }
        _ => {}
    }

    match app.view {
        View::Profiles => handle_profiles(app, key),
        View::Editor => handle_editor(app, key),
        View::Sync => {}
        View::Settings => handle_settings(app, key),
    }
}

fn clamp_rows(app: &mut App) {
    // Moving between views must not leave the cursor past the end of a list
    // that is shorter than the one before it.
    let len = match app.view {
        View::Editor => app.selected_name().map_or(0, |n| {
            let vars = app.store.get(&n).map_or(0, |p| p.vars.len());
            let reqs = app.store.get(&n).map_or(0, |p| p.requires.len());
            if app.edit_section == 0 { vars } else { reqs }
        }),
        _ => 5,
    };
    if len == 0 {
        app.edit_row = 0;
    } else if app.edit_row >= len {
        app.edit_row = len - 1;
    }
}

const SETTINGS_ROWS: usize = 5;

fn move_down(app: &mut App, count: usize) {
    if count == 0 {
        return;
    }
    if app.edit_row + 1 < count {
        app.edit_row += 1;
    }
}

fn move_up(app: &mut App) {
    app.edit_row = app.edit_row.saturating_sub(1);
}

// ---- profiles ----------------------------------------------------------

fn handle_profiles(app: &mut App, key: KeyEvent) {
    let count = app.store.profiles.len();
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => {
            if count > 0 && app.selected + 1 < count {
                app.selected += 1;
            }
            app.status = None;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.selected = app.selected.saturating_sub(1);
            app.status = None;
        }
        KeyCode::Enter | KeyCode::Char('e') => {
            if app.selected_name().is_some() {
                app.view = View::Editor;
                app.edit_section = 0;
                app.edit_row = 0;
            }
        }
        KeyCode::Char('a') => {
            app.input = Some(Input::new(
                "新建 profile",
                "名字（字母、数字、-、_、.）",
                "",
                Purpose::NewProfile,
            ));
        }
        KeyCode::Char('d') => {
            if let Some(name) = app.selected_name() {
                let dependents = crate::graph::dependents_of(&app.store, &name);
                let mut body = vec![format!("将删除 profile '{name}'。")];
                if !dependents.is_empty() {
                    // The surprising part, so spell it out before the user
                    // commits rather than after.
                    body.push(format!(
                        "以下 profile 依赖它，依赖关系也会被移除：{}",
                        dependents.join(", ")
                    ));
                }
                app.confirm = Some(Confirm {
                    title: "删除 profile".to_string(),
                    body,
                    purpose: ConfirmPurpose::DeleteProfile(name),
                });
            }
        }
        _ => {}
    }
}

// ---- editor ------------------------------------------------------------

fn handle_editor(app: &mut App, key: KeyEvent) {
    let Some(profile_name) = app.selected_name() else {
        return;
    };
    let vars: Vec<String> = app
        .store
        .get(&profile_name)
        .map(|p| p.vars.keys().cloned().collect())
        .unwrap_or_default();
    let reqs: Vec<String> = app
        .store
        .get(&profile_name)
        .map(|p| p.requires.clone())
        .unwrap_or_default();
    let current = if app.edit_section == 0 {
        vars.len()
    } else {
        reqs.len()
    };

    match key.code {
        KeyCode::Esc => app.view = View::Profiles,
        KeyCode::Tab | KeyCode::Char(' ') => {
            app.edit_section = 1 - app.edit_section;
            app.edit_row = 0;
        }
        KeyCode::Down | KeyCode::Char('j') => move_down(app, current),
        KeyCode::Up | KeyCode::Char('k') => move_up(app),
        KeyCode::Enter => {
            if app.edit_section == 0
                && let Some(var) = vars.get(app.edit_row)
            {
                let existing = app
                    .store
                    .get(&profile_name)
                    .and_then(|p| p.vars.get(var))
                    .cloned()
                    .unwrap_or_default();
                app.input = Some(Input::new(
                    "修改变量",
                    var,
                    &existing,
                    Purpose::SetVarValue {
                        profile: profile_name.clone(),
                        key: var.clone(),
                    },
                ));
            }
        }
        KeyCode::Char('a') => {
            if app.edit_section == 0 {
                app.input = Some(Input::new(
                    "新增变量",
                    "变量名（只允许 [A-Za-z_][A-Za-z0-9_]*）",
                    "",
                    Purpose::NewVarName {
                        profile: profile_name.clone(),
                    },
                ));
            } else {
                app.input = Some(Input::new(
                    "新增依赖",
                    "要先激活的 profile 名",
                    "",
                    Purpose::AddRequires {
                        profile: profile_name.clone(),
                    },
                ));
            }
        }
        KeyCode::Char('d') => {
            if app.edit_section == 0 {
                if let Some(var) = vars.get(app.edit_row).cloned() {
                    let removed = app.unset_var(&profile_name, &var);
                    if app.report(removed).is_some() {
                        app.info(format!("已删除变量 {var}"));
                    }
                    clamp_rows(app);
                }
            } else if let Some(dep) = reqs.get(app.edit_row).cloned() {
                let removed = app.remove_requires(&profile_name, &dep);
                if app.report(removed).is_some() {
                    app.info(format!("已移除依赖 {dep}"));
                }
                clamp_rows(app);
            }
        }
        _ => {}
    }
}

// ---- settings ----------------------------------------------------------

fn handle_settings(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => {
            if app.settings_row + 1 < SETTINGS_ROWS {
                app.settings_row += 1;
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.settings_row = app.settings_row.saturating_sub(1);
        }
        KeyCode::Enter => {
            let s = &app.settings.sync;
            let (title, prompt, value, purpose, mask) = match app.settings_row {
                0 => (
                    "同步 ID",
                    "3-64 个字符，只允许字母、数字、-、_",
                    s.sync_id.clone().unwrap_or_default(),
                    Purpose::SyncId,
                    false,
                ),
                1 => (
                    "同步密钥",
                    "密钥短语（只在本机用于派生加解密密钥）",
                    String::new(),
                    Purpose::SyncKey,
                    true,
                ),
                2 => (
                    "端点",
                    "pastebin 服务地址",
                    s.endpoint.clone(),
                    Purpose::Endpoint,
                    false,
                ),
                3 => (
                    "有效期",
                    "例如 30d / 12h（服务端可能缩短，实际值以响应为准）",
                    s.expire.clone(),
                    Purpose::Expire,
                    false,
                ),
                _ => (
                    "设备名",
                    "用于在冲突界面区分来源，留空则用设备 ID",
                    app.settings.device_name.clone().unwrap_or_default(),
                    Purpose::DeviceName,
                    false,
                ),
            };
            let mut input = Input::new(title, prompt, &value, purpose);
            input.mask = mask;
            app.input = Some(input);
        }
        _ => {}
    }
}

// ---- overlays ----------------------------------------------------------

fn handle_input(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.input = None,
        KeyCode::Enter => {
            if let Some(input) = app.input.take() {
                commit_input(app, input);
            }
        }
        KeyCode::Backspace => {
            if let Some(input) = app.input.as_mut() {
                input.value.pop();
            }
        }
        KeyCode::Char(c) => {
            // Ctrl-modified characters arrive as Char too; ignore them so a
            // stray Ctrl-C does not insert a literal 'c'.
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && let Some(input) = app.input.as_mut()
            {
                input.value.push(c);
            }
        }
        _ => {}
    }
}

/// Apply the popup's contents. A failure puts the popup back with the text
/// still in it, so a typo can be corrected instead of retyped.
fn commit_input(app: &mut App, input: Input) {
    if let Err(e) = apply_input(app, &input) {
        app.fail(format!("{e:#}"));
        app.input = Some(input);
    }
}

fn apply_input(app: &mut App, input: &Input) -> anyhow::Result<()> {
    let value = input.value.trim().to_string();

    match input.purpose.clone() {
        Purpose::NewProfile => {
            if value.is_empty() {
                anyhow::bail!("名字不能为空");
            }
            app.create_profile(&value)?;
            app.info(format!("已创建 '{value}'"));
        }

        Purpose::NewVarName { profile } => {
            if value.is_empty() {
                anyhow::bail!("变量名不能为空");
            }
            // Check the name now rather than after the value has been typed:
            // a bad name should not need a value to be discovered.
            crate::config::validate_var_name(&value)?;

            // Not a failure, just the second half of the flow — so this sets
            // the popup directly instead of returning an error.
            let prompt = format!("{value} 的值");
            app.input = Some(Input::new(
                "新增变量",
                &prompt,
                "",
                Purpose::SetVarValue {
                    profile,
                    key: value,
                },
            ));
        }

        Purpose::SetVarValue { profile, key } => {
            // The value is taken verbatim: leading and trailing spaces can be
            // meaningful in an environment variable.
            app.set_var(&profile, &key, &input.value)?;
            app.info(format!("已设置 {key}"));
        }

        Purpose::AddRequires { profile } => {
            if value.is_empty() {
                anyhow::bail!("依赖名不能为空");
            }
            app.add_requires(&profile, &value)?;
            app.info(format!("已添加依赖 {value}"));
        }

        Purpose::SyncId => {
            app.set_sync_id(&value)?;
            app.refresh_sync_status();
            app.info(format!("同步 ID 已设为 {value}"));
        }
        Purpose::SyncKey => {
            app.set_sync_key(&input.value)?;
            app.info("密钥已保存（只存在本机，不会同步到远端）");
        }
        Purpose::Endpoint => {
            app.set_endpoint(&value)?;
            app.refresh_sync_status();
            app.info(format!("端点已设为 {value}"));
        }
        Purpose::Expire => {
            app.set_expire(&value)?;
            app.info(format!("有效期已设为 {value}"));
        }
        Purpose::DeviceName => {
            app.set_device_name(&value)?;
            app.info("设备名已更新");
        }
    }
    Ok(())
}

fn handle_confirm(app: &mut App, key: KeyEvent) {
    let confirmed = matches!(key.code, KeyCode::Char('y') | KeyCode::Enter);
    let Some(confirm) = app.confirm.take() else {
        return;
    };
    if !confirmed {
        app.info("已取消");
        return;
    }

    match confirm.purpose {
        ConfirmPurpose::DeleteProfile(name) => {
            let removed = app.delete_profile(&name);
            app.report(removed);
        }
        ConfirmPurpose::DeleteRemote => app.delete_remote(),
        // Reached only through the prompt set up in `handle_sync_keys`, so the
        // user has already been told what is about to be destroyed.
        ConfirmPurpose::PushOverUnreadable => app.push(),
    }
}

fn handle_conflict(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('l') => app.resolve_conflict(true),
        KeyCode::Char('r') => app.resolve_conflict(false),
        KeyCode::Esc => {
            app.conflict = None;
            app.info("已取消，两边都没有被改动");
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Paths, Settings};
    use std::path::PathBuf;

    struct TempRoot(PathBuf);
    impl TempRoot {
        fn new() -> Self {
            let mut buf = [0u8; 6];
            rand::fill(&mut buf);
            let uniq: String = buf.iter().map(|b| format!("{b:02x}")).collect();
            let p = std::env::temp_dir().join(format!("envpick-tui-key-{uniq}"));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn app_with(entries: &[crate::testing::ProfileSpec<'_>]) -> (App, TempRoot) {
        let root = TempRoot::new();
        let store = crate::testing::store_with(entries);
        let app = App::new(
            Paths {
                root: root.0.clone(),
            },
            store,
            Settings::default(),
        );
        (app, root)
    }

    fn press(app: &mut App, code: KeyCode) {
        handle(app, KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    #[test]
    fn typing_in_the_input_popup_does_not_trigger_shortcuts() {
        let (mut app, _root) = app_with(&[("global", &[], &[])]);
        press(&mut app, KeyCode::Char('a'));
        assert!(app.input.is_some());
        // 'q' and 's' must be typed, not interpreted as quit/sync.
        type_text(&mut app, "qsvault");
        assert!(!app.quit, "'q' quit while typing");
        assert_eq!(app.input.as_ref().unwrap().value, "qsvault");
    }

    /// Creating a profile is a two-step flow for variables; the name popup
    /// leads to a value popup rather than saving an empty string.
    #[test]
    fn adding_a_variable_asks_for_the_value_next() {
        let (mut app, _root) = app_with(&[("global", &[], &[])]);
        app.view = View::Editor;
        app.edit_section = 0;
        press(&mut app, KeyCode::Char('a'));
        type_text(&mut app, "EDITOR");
        press(&mut app, KeyCode::Enter);
        assert!(app.input.is_some(), "should ask for the value");
        type_text(&mut app, "nvim");
        press(&mut app, KeyCode::Enter);
        assert!(app.input.is_none());
        assert_eq!(app.store.get("global").unwrap().vars["EDITOR"], "nvim");
    }

    #[test]
    fn a_bad_variable_name_is_rejected_before_asking_for_a_value() {
        let (mut app, _root) = app_with(&[("global", &[], &[])]);
        app.view = View::Editor;
        press(&mut app, KeyCode::Char('a'));
        type_text(&mut app, "has.dot");
        press(&mut app, KeyCode::Enter);
        assert!(app.status.as_ref().unwrap().error);
        assert_eq!(
            app.input.as_ref().expect("popup stays open").value,
            "has.dot"
        );
        assert!(app.store.get("global").unwrap().vars.is_empty());
    }

    #[test]
    fn escape_closes_the_popup_without_saving() {
        let (mut app, _root) = app_with(&[("global", &[], &[])]);
        press(&mut app, KeyCode::Char('a'));
        type_text(&mut app, "work");
        press(&mut app, KeyCode::Esc);
        assert!(app.input.is_none());
        assert!(!app.store.contains("work"));
    }

    /// Deleting asks first, and cancelling must leave the profile in place.
    #[test]
    fn delete_requires_confirmation() {
        let (mut app, _root) = app_with(&[("global", &[], &[]), ("work", &[], &[])]);
        app.selected = app.names().iter().position(|n| n == "work").unwrap();
        press(&mut app, KeyCode::Char('d'));
        assert!(app.confirm.is_some());
        press(&mut app, KeyCode::Esc);
        assert!(app.store.contains("work"), "cancel still deleted it");

        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('y'));
        assert!(!app.store.contains("work"));
    }

    fn select(app: &mut App, name: &str) {
        app.selected = app.names().iter().position(|n| n == name).unwrap();
    }

    /// A cycle would break every later `use`, so entering one from the UI has to
    /// be refused rather than saved.
    #[test]
    fn a_dependency_cycle_is_refused() {
        let (mut app, _root) = app_with(&[("a", &[], &[]), ("b", &[], &[])]);
        app.view = View::Editor;
        app.edit_section = 1;

        // b requires a: fine on its own.
        select(&mut app, "b");
        press(&mut app, KeyCode::Char('a'));
        type_text(&mut app, "a");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.store.get("b").unwrap().requires, vec!["a".to_string()]);

        // Now a requires b, which closes the loop and must be refused.
        select(&mut app, "a");
        press(&mut app, KeyCode::Char('a'));
        type_text(&mut app, "b");
        press(&mut app, KeyCode::Enter);
        assert!(app.status.as_ref().unwrap().error, "{:?}", app.status);
        assert!(
            app.store.get("a").unwrap().requires.is_empty(),
            "the cycle was saved: {:?}",
            app.store.get("a").unwrap().requires
        );
        // And the graph still resolves.
        assert!(crate::graph::resolve_order(&app.store, &["b".to_string()]).is_ok());
    }

    /// Re-adding an existing dependency is a no-op, not a duplicate entry.
    #[test]
    fn a_duplicate_dependency_is_rejected() {
        let (mut app, _root) = app_with(&[("a", &[], &[]), ("b", &["a"], &[])]);
        select(&mut app, "b");
        app.view = View::Editor;
        app.edit_section = 1;
        press(&mut app, KeyCode::Char('a'));
        type_text(&mut app, "a");
        press(&mut app, KeyCode::Enter);
        assert!(app.status.as_ref().unwrap().error);
        assert_eq!(app.store.get("b").unwrap().requires, vec!["a".to_string()]);
    }

    #[test]
    fn tab_cycles_views_and_does_not_quit() {
        let (mut app, _root) = app_with(&[("global", &[], &[])]);
        for _ in 0..4 {
            press(&mut app, KeyCode::Tab);
        }
        assert_eq!(app.view, View::Profiles);
        assert!(!app.quit);
    }

    #[test]
    fn q_quits_but_question_mark_opens_help() {
        let (mut app, _root) = app_with(&[("global", &[], &[])]);
        press(&mut app, KeyCode::Char('?'));
        assert!(app.show_help);
        assert!(!app.quit);
        press(&mut app, KeyCode::Char('x'));
        assert!(!app.show_help);
        press(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
    }

    /// A status whose remote is there but not readable.
    fn unreadable() -> crate::sync::SyncStatus {
        crate::sync::SyncStatus {
            state: crate::sync::SyncState::RemoteUnreadable,
            remote: crate::sync::RemoteState::Unreadable,
            remote_expires_at: None,
            local_hash: "h".into(),
            local_profiles: 1,
        }
    }

    /// `p` normally pushes on a single keystroke. Against a remote we could not
    /// decrypt that same keystroke would destroy data we never managed to read,
    /// so it has to ask first.
    ///
    /// No sync id is configured here on purpose: a push that actually runs
    /// fails loudly, so "the status is still clean" is positive evidence that
    /// nothing was attempted, rather than an assertion that would also hold if
    /// the push quietly did nothing.
    #[test]
    fn pushing_over_an_unreadable_remote_asks_before_destroying_it() {
        let (mut app, _root) = app_with(&[("global", &[], &[])]);
        app.sync.status = Some(unreadable());

        press(&mut app, KeyCode::Char('p'));
        assert!(app.confirm.is_some(), "p pushed without asking");
        assert!(app.status.is_none(), "something was attempted anyway");

        // Backing out must leave the remote untouched. `Esc` still reports
        // "已取消", so the thing to check is that it did not *fail* — a push
        // that ran here would have errored, since no sync id is configured.
        press(&mut app, KeyCode::Esc);
        assert!(app.confirm.is_none());
        assert!(
            app.status.as_ref().is_some_and(|s| !s.error),
            "cancelling still pushed: {:?}",
            app.status
        );

        // Confirming does go through — otherwise the gate would be a dead end.
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('y'));
        assert!(app.confirm.is_none());
        assert!(
            app.status.as_ref().is_some_and(|s| s.error),
            "confirming did not reach the push: {:?}",
            app.status
        );
    }

    /// ...and the gate is only for that one state. Every other state, `p` stays
    /// the one-keystroke push it has always been.
    #[test]
    fn an_ordinary_push_is_not_gated() {
        let (mut app, _root) = app_with(&[("global", &[], &[])]);
        app.sync.status = Some(crate::sync::SyncStatus {
            state: crate::sync::SyncState::UpToDate,
            remote: crate::sync::RemoteState::Synced(1),
            remote_expires_at: None,
            local_hash: "h".into(),
            local_profiles: 1,
        });

        press(&mut app, KeyCode::Char('p'));

        assert!(app.confirm.is_none(), "an ordinary push asked first");
        assert!(
            app.status.as_ref().is_some_and(|s| s.error),
            "the push never ran: {:?}",
            app.status
        );
    }

    /// The conflict screen must not write anything until a key is pressed.
    #[test]
    fn escape_on_a_conflict_leaves_both_sides_alone() {
        let (mut app, _root) = app_with(&[("global", &[], &[])]);
        app.conflict = Some(crate::tui::app::Conflict {
            local: crate::sync::SyncDoc::from_store(&app.store, "d1", None, 1),
            remote: crate::sync::SyncDoc::from_store(&app.store, "d2", None, 2),
        });
        press(&mut app, KeyCode::Esc);
        assert!(app.conflict.is_none());
        assert!(app.status.as_ref().is_some_and(|s| !s.error));
    }
}
