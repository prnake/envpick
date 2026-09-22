//! Rendering. Every view is a pure function of [`App`], so the same code can be
//! driven by a `TestBackend` in tests.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Clear, List, ListItem, Paragraph, Row, Table, Wrap};

use crate::graph;
use crate::shell;
use crate::tui::app::{App, Confirm, Conflict, Input, View};

const HINT: Color = Color::DarkGray;
const ACCENT: Color = Color::Cyan;
const WARN: Color = Color::Yellow;

fn panel(title: &str) -> Block<'_> {
    Block::bordered()
        .border_style(Style::new().fg(HINT))
        .title(Span::styled(format!(" {title} "), Style::new().fg(ACCENT)))
}

pub fn draw(frame: &mut Frame, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(frame.area());

    draw_tabs(frame, app, chunks[0]);
    match app.view {
        View::Profiles => draw_profiles(frame, app, chunks[1]),
        View::Editor => draw_editor(frame, app, chunks[1]),
        View::Sync => draw_sync(frame, app, chunks[1]),
        View::Settings => draw_settings(frame, app, chunks[1]),
    }
    draw_footer(frame, app, chunks[2]);

    if app.show_help {
        draw_help(frame);
    }
    if let Some(conflict) = &app.conflict {
        draw_conflict(frame, app, conflict);
    }
    if let Some(confirm) = &app.confirm {
        draw_confirm(frame, confirm);
    }
    if let Some(input) = &app.input {
        draw_input(frame, input);
    }
}

fn draw_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![Span::styled(
        " envpick ",
        Style::new().fg(Color::Black).bg(ACCENT).bold(),
    )];
    for view in View::ALL {
        let selected = view == app.view;
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!(" {} ", view.title()),
            if selected {
                Style::new().fg(Color::Black).bg(WARN).bold()
            } else {
                Style::new().fg(HINT)
            },
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let line = match &app.status {
        Some(s) if s.error => Line::from(Span::styled(
            format!(" {} ", s.text),
            Style::new().fg(Color::White).bg(Color::Red),
        )),
        Some(s) => Line::from(Span::styled(
            format!(" {}", s.text),
            Style::new().fg(Color::Green),
        )),
        None => Line::from(Span::styled(
            " Tab 切换视图 · ? 帮助 · q 退出",
            Style::new().fg(HINT),
        )),
    };
    frame.render_widget(Paragraph::new(line), area);
}

// ---- profiles ----------------------------------------------------------

fn draw_profiles(frame: &mut Frame, app: &App, area: Rect) {
    let chunks =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).split(area);

    let names = app.names();
    let active = app.active();

    let items: Vec<ListItem> = names
        .iter()
        .map(|name| {
            let is_active = active.iter().any(|a| a == name);
            let count = app.store.get(name).map_or(0, |p| p.vars.len());
            let marker = if is_active { "●" } else { "○" };
            let style = if is_active {
                Style::new().fg(Color::Green)
            } else {
                Style::new()
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {marker} "), style),
                Span::raw(format!("{name:<24}")),
                Span::styled(format!("{count} 变量"), Style::new().fg(HINT)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(panel("Profiles"))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol(">");
    let mut state = ratatui::widgets::ListState::default();
    if !names.is_empty() {
        state.select(Some(app.selected));
    }
    frame.render_stateful_widget(list, chunks[0], &mut state);

    frame.render_widget(
        Paragraph::new(detail_text(app))
            .block(panel("详情"))
            .wrap(Wrap { trim: false }),
        chunks[1],
    );
}

/// The detail pane doubles as the answer to "how do I actually activate this?".
/// It cannot be done from here, so the command is shown ready to copy.
fn detail_text(app: &App) -> Text<'static> {
    let Some(name) = app.selected_name() else {
        return Text::from(vec![
            Line::raw(""),
            Line::styled(" 还没有任何 profile", Style::new().fg(HINT)),
            Line::raw(""),
            Line::styled(" 按 a 新建一个", Style::new().fg(HINT)),
        ]);
    };
    let Some(profile) = app.store.get(&name) else {
        return Text::raw("");
    };

    let is_active = app.active().iter().any(|a| a == &name);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!(" {name}"), Style::new().bold()),
            Span::raw("  "),
            if is_active {
                Span::styled("激活中", Style::new().fg(Color::Green))
            } else {
                Span::styled("未激活", Style::new().fg(HINT))
            },
        ]),
        Line::raw(""),
    ];

    if !profile.requires.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(" 依赖  ", Style::new().fg(HINT)),
            Span::raw(profile.requires.join(", ")),
        ]));
    }

    match graph::resolve_order(&app.store, std::slice::from_ref(&name)) {
        Ok(order) if order.len() > 1 => lines.push(Line::from(vec![
            Span::styled(" 顺序  ", Style::new().fg(HINT)),
            Span::raw(order.join(" → ")),
        ])),
        Ok(_) => {}
        Err(e) => lines.push(Line::styled(format!(" ⚠ {e}"), Style::new().fg(Color::Red))),
    }

    lines.push(Line::raw(""));
    if profile.vars.is_empty() {
        lines.push(Line::styled(" （没有变量）", Style::new().fg(HINT)));
    } else {
        let width = profile.vars.keys().map(|k| k.len()).max().unwrap_or(0);
        for (key, value) in &profile.vars {
            lines.push(Line::from(vec![
                Span::raw(" "),
                Span::styled(format!("{key:<width$}"), Style::new().fg(ACCENT)),
                Span::raw(" = "),
                Span::raw(shell::quote(value)),
            ]));
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled(
        " 在这个 shell 里激活（TUI 无法修改父 shell 的环境）:",
        Style::new().fg(HINT),
    ));
    lines.push(Line::styled(
        format!("   ep use {name}"),
        Style::new().fg(Color::Green),
    ));
    if is_active {
        lines.push(Line::styled(
            format!("   ep unuse {name}"),
            Style::new().fg(WARN),
        ));
    }
    Text::from(lines)
}

// ---- editor ------------------------------------------------------------

fn draw_editor(frame: &mut Frame, app: &App, area: Rect) {
    let Some(name) = app.selected_name() else {
        frame.render_widget(
            Paragraph::new(" 先在上一个视图里选择一个 profile").block(panel("Editor")),
            area,
        );
        return;
    };
    let Some(profile) = app.store.get(&name) else {
        return;
    };

    let chunks = Layout::vertical([Constraint::Min(4), Constraint::Length(9)]).split(area);
    let focused_vars = app.edit_section == 0;

    let rows: Vec<Row> = profile
        .vars
        .iter()
        .map(|(k, v)| {
            Row::new(vec![
                ratatui::widgets::Cell::from(k.clone()),
                ratatui::widgets::Cell::from(shell::quote(v)),
            ])
        })
        .collect();
    let mut state = ratatui::widgets::TableState::default();
    if focused_vars && !rows.is_empty() {
        state.select(Some(app.edit_row.min(rows.len() - 1)));
    }
    let title = format!("变量 — {name}");
    let table = Table::new(rows, [Constraint::Length(24), Constraint::Min(10)])
        .header(Row::new(vec!["变量名", "值"]).style(Style::new().fg(HINT)))
        .block(if focused_vars {
            panel(&title).border_style(Style::new().fg(ACCENT))
        } else {
            panel(&title)
        })
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol(">");
    frame.render_stateful_widget(table, chunks[0], &mut state);

    let deps: Vec<ListItem> = profile
        .requires
        .iter()
        .map(|d| ListItem::new(format!(" {d}")))
        .collect();
    let mut dep_state = ratatui::widgets::ListState::default();
    if !focused_vars && !deps.is_empty() {
        dep_state.select(Some(app.edit_row.min(deps.len() - 1)));
    }
    let title = format!("依赖（先激活这些）— {name}");
    let list = List::new(deps)
        .block(if focused_vars {
            panel(&title)
        } else {
            panel(&title).border_style(Style::new().fg(ACCENT))
        })
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol(">");
    frame.render_stateful_widget(list, chunks[1], &mut dep_state);
}

// ---- sync --------------------------------------------------------------

fn draw_sync(frame: &mut Frame, app: &App, area: Rect) {
    frame.render_widget(
        Paragraph::new(sync_text(app))
            .block(panel("Sync"))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn sync_text(app: &App) -> Text<'static> {
    let s = &app.settings.sync;
    let hint = Style::new().fg(HINT);
    let mut lines = Vec::new();

    let field = |label: &'static str, value: String| {
        Line::from(vec![
            Span::styled(format!(" {label:<10}"), Style::new().fg(ACCENT)),
            Span::raw(value),
        ])
    };

    if s.sync_id.is_none() {
        lines.push(Line::styled(
            " 尚未配置同步。到 Settings 视图填写同步 ID 与密钥。",
            Style::new().fg(WARN),
        ));
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            " 同步 ID 决定远端 paste 的名字；密钥只用于本机加解密，",
            hint,
        ));
        lines.push(Line::styled(" 上传的内容始终是密文。", hint));
        return Text::from(lines);
    }

    lines.push(field("端点", s.endpoint.clone()));
    lines.push(field(
        "同步 ID",
        format!(
            "{}  →  paste {}",
            s.sync_id.as_deref().unwrap_or("-"),
            s.paste_name().unwrap_or_else(|_| "-".to_string())
        ),
    ));
    lines.push(field("密钥", app.key_display()));
    lines.push(Line::raw(""));

    match (&app.sync.status, &app.sync.error) {
        (Some(st), _) => {
            lines.push(field("状态", st.state.label().to_string()));
            lines.push(field("本地", format!("{} 个 profile", st.local_profiles)));
            if st.remote != crate::sync::RemoteState::Missing {
                lines.push(field("远端", st.remote.label()));
            }
            if let Some(expire_at) = &st.remote_expires_at {
                let remain = st
                    .seconds_until_expiry()
                    .map(|secs| {
                        format!(
                            "（剩余 {}）",
                            crate::handlers::sync_cmd::human_duration(secs)
                        )
                    })
                    .unwrap_or_default();
                lines.push(field("过期于", format!("{expire_at}{remain}")));
            }
            if let Some(at) = &s.last_synced_at {
                lines.push(field("上次同步", at.clone()));
            }
            lines.push(Line::raw(""));
            match st.state {
                crate::sync::SyncState::Diverged => lines.push(Line::styled(
                    " 两边都有改动：按 l 保留本地，或 r 保留远端。",
                    Style::new().fg(WARN),
                )),
                crate::sync::SyncState::RemoteMissing => lines.push(Line::styled(
                    " 远端还没有这份数据：按 p 创建。",
                    Style::new().fg(WARN),
                )),
                // The one state where pressing the obvious key is destructive.
                // `s` refuses on its own; `p` is the deliberate override, and
                // this is the only place that says so.
                crate::sync::SyncState::RemoteUnreadable => lines.push(Line::styled(
                    " 远端不是本工具写的数据，或密钥不对：已停止，绝不会自动覆盖。",
                    Style::new().fg(WARN),
                )),
                _ => {}
            }
        }
        (None, Some(err)) => lines.push(Line::styled(
            format!(" ⚠ {err}"),
            Style::new().fg(Color::Red),
        )),
        (None, None) => lines.push(Line::styled(" （尚未检查）按 s 同步一次", hint)),
    }

    if let Some(url) = &app.sync.browser_url {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            " 浏览器直读（# 后面是密钥，请勿公开分享）:",
            hint,
        ));
        lines.push(Line::styled(
            format!(" {url}"),
            Style::new().fg(Color::Green),
        ));
    }
    Text::from(lines)
}

// ---- settings ----------------------------------------------------------

fn draw_settings(frame: &mut Frame, app: &App, area: Rect) {
    let s = &app.settings.sync;
    let rows = [
        ("同步 ID", s.sync_id.clone().unwrap_or_default()),
        ("密钥", app.key_display()),
        ("端点", s.endpoint.clone()),
        ("有效期", s.expire.clone()),
        (
            "设备名",
            app.settings.device_name.clone().unwrap_or_default(),
        ),
    ];

    let table = Table::new(
        rows.iter()
            .map(|(k, v)| Row::new(vec![(*k).to_string(), v.clone()])),
        [Constraint::Length(12), Constraint::Min(20)],
    )
    .block(panel("Settings"))
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
    .highlight_symbol(">");
    let mut state = ratatui::widgets::TableState::default();
    state.select(Some(app.settings_row.min(rows.len() - 1)));
    frame.render_stateful_widget(table, area, &mut state);
}

// ---- overlays ----------------------------------------------------------

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

fn draw_input(frame: &mut Frame, input: &Input) {
    let area = centered(frame.area(), 64, 7);
    frame.render_widget(Clear, area);
    let text = Text::from(vec![
        Line::raw(""),
        Line::styled(format!(" {}", input.prompt), Style::new().fg(HINT)),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(input.display(), Style::new().fg(ACCENT)),
            Span::styled("▏", Style::new().fg(ACCENT)),
        ]),
        Line::raw(""),
        Line::styled(" Enter 确认 · Esc 取消", Style::new().fg(HINT)),
    ]);
    frame.render_widget(Paragraph::new(text).block(panel(&input.title)), area);
}

fn draw_confirm(frame: &mut Frame, confirm: &Confirm) {
    let body: Vec<Line> = confirm
        .body
        .iter()
        .map(|l| Line::styled(format!(" {l}"), Style::new()))
        .collect();
    let height = (body.len() as u16) + 6;
    let area = centered(frame.area(), 68, height);
    frame.render_widget(Clear, area);

    let mut lines = vec![Line::raw("")];
    lines.extend(body);
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        " y / Enter 确认 · 其它键取消",
        Style::new().fg(WARN),
    ));
    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(panel(&confirm.title)),
        area,
    );
}

fn draw_conflict(frame: &mut Frame, app: &App, conflict: &Conflict) {
    let area = centered(frame.area(), 84, 20);
    frame.render_widget(Clear, area);
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(2),
    ])
    .split(area);

    frame.render_widget(
        Paragraph::new(Text::from(vec![
            Line::raw(""),
            Line::styled(
                format!(" {}", crate::text::prompts::CONFLICT_BODY),
                Style::new().fg(WARN),
            ),
        ]))
        .block(panel(&format!(" {}", crate::text::prompts::CONFLICT_TITLE))),
        chunks[0],
    );

    let s = &app.settings.sync;
    let device = |id: &str, name: &Option<String>| {
        if id.is_empty() {
            "-".to_string()
        } else {
            name.clone().unwrap_or_else(|| id.to_string())
        }
    };
    let rows = vec![
        Row::new(vec![
            "本地".to_string(),
            conflict.local.summary(),
            device(&conflict.local.device_id, &conflict.local.device_name),
        ]),
        Row::new(vec![
            "远端".to_string(),
            conflict.remote.summary(),
            device(&conflict.remote.device_id, &conflict.remote.device_name),
        ]),
    ];
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(6),
                Constraint::Min(30),
                Constraint::Length(16),
            ],
        )
        .header(Row::new(vec!["版本", "内容", "来自"]).style(Style::new().fg(HINT)))
        .block(panel("对比")),
        chunks[1],
    );

    let _ = s;
    frame.render_widget(
        Paragraph::new(Text::from(vec![
            Line::styled(" 放弃的一方会丢失，且无法撤销。", Style::new().fg(HINT)),
            Line::from(vec![
                Span::styled(" l", Style::new().fg(Color::Green).bold()),
                Span::raw(" 保留本地并覆盖远端    "),
                Span::styled("r", Style::new().fg(Color::Green).bold()),
                Span::raw(" 用远端覆盖本地    "),
                Span::styled("Esc", Style::new().bold()),
                Span::raw(" 稍后再说"),
            ]),
        ])),
        chunks[2],
    );
}

fn draw_help(frame: &mut Frame) {
    let section = |title: &'static str, keys: &[(&'static str, &'static str)]| {
        let mut lines = vec![Line::styled(
            format!(" {title}"),
            Style::new().fg(ACCENT).bold(),
        )];
        lines.extend(keys.iter().map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!("   {k:<12}"), Style::new().fg(WARN)),
                Span::raw(*d),
            ])
        }));
        lines.push(Line::raw(""));
        lines
    };

    let mut lines = vec![Line::raw("")];
    lines.extend(section(
        "全局",
        &[("Tab / ⇧Tab", "切换视图"), ("?", "关闭帮助"), ("q", "退出")],
    ));
    lines.extend(section(
        "Profiles",
        &[
            ("↑ ↓ / j k", "移动"),
            ("Enter", "在 Editor 里编辑"),
            ("a", "新建 profile"),
            ("d", "删除 profile"),
        ],
    ));
    lines.extend(section(
        "Editor",
        &[
            ("Tab", "在变量与依赖之间切换"),
            ("a", "新增变量或依赖"),
            ("Enter", "修改值"),
            ("d", "删除"),
        ],
    ));
    lines.extend(section(
        // Only the Editor swallows plain character keys, so these work from any
        // other view. The rest below are Sync-only.
        "Sync / Settings",
        &[
            ("s / p / u", "智能同步 / 推送 / 拉取（Editor 视图除外）"),
            ("g", "生成浏览器直读链接"),
            ("r", "重新检查远端状态"),
            ("D", "删除远端 paste"),
            ("Enter", "编辑设置项（Settings 视图）"),
        ],
    ));

    // Every section appends a blank line, and closing ones read better than
    // trailing ones.
    lines.pop();

    // Sized to the content rather than a fixed height: a hardcoded value
    // silently truncated the last section the moment one was added, which is a
    // bad way to find out your documentation is missing.
    let area = centered(frame.area(), 76, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);

    frame.render_widget(Paragraph::new(Text::from(lines)).block(panel("帮助")), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Paths, Profile, Settings};
    use crate::sync::{RemoteState, SyncState, SyncStatus};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use unicode_width::UnicodeWidthStr;

    fn app_with(entries: &[crate::testing::ProfileSpec<'_>]) -> (App, TempRoot) {
        let root = TempRoot::new();
        let store = crate::testing::store_with(entries);
        let paths = Paths {
            root: root.0.clone(),
        };
        (App::new(paths, store, Settings::default()), root)
    }

    struct TempRoot(std::path::PathBuf);
    impl TempRoot {
        fn new() -> Self {
            let mut buf = [0u8; 6];
            rand::fill(&mut buf);
            let uniq: String = buf.iter().map(|b| format!("{b:02x}")).collect();
            let p = std::env::temp_dir().join(format!("envpick-tui-{uniq}"));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Render into an offscreen buffer and return it as lines of text.
    fn render(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            let mut x = 0;
            while x < buffer.area.width {
                let Some(cell) = buffer.cell((x, y)) else {
                    x += 1;
                    continue;
                };
                let symbol = cell.symbol();
                out.push_str(symbol);
                // A wide character occupies two cells, with the second left
                // blank; stepping by its real width keeps CJK intact instead
                // of splicing spaces into it.
                x += symbol.width().max(1) as u16;
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn the_profiles_view_lists_names_and_variable_counts() {
        let (app, _root) = app_with(&[
            ("global", &[], &[("EDITOR", "nvim")]),
            ("work", &["global"], &[("HTTP_PROXY", "p")]),
        ]);
        let out = render(&app, 100, 24);
        assert!(out.contains("Profiles"), "{out}");
        assert!(out.contains("global"), "{out}");
        assert!(out.contains("work"), "{out}");
        assert!(out.contains("1 变量"), "{out}");
        // The detail pane has to be honest about activation being impossible
        // from inside the TUI.
        assert!(out.contains("ep use global"), "{out}");
    }

    #[test]
    fn the_editor_view_shows_variables_and_requires() {
        let (mut app, _root) = app_with(&[("work", &["global"], &[("EDITOR", "nvim")])]);
        app.store
            .profiles
            .insert("global".into(), Profile::default());
        app.view = View::Editor;
        app.selected = app.names().iter().position(|n| n == "work").unwrap();
        let out = render(&app, 100, 24);
        assert!(out.contains("EDITOR"), "{out}");
        assert!(out.contains("nvim"), "{out}");
        assert!(out.contains("global"), "{out}");
    }

    #[test]
    fn the_help_overlay_explains_the_keys() {
        let (mut app, _root) = app_with(&[("global", &[], &[])]);
        app.show_help = true;
        let out = render(&app, 90, 26);
        assert!(out.contains("帮助"), "{out}");
        assert!(out.contains("切换视图"), "{out}");
        // The sync verbs work outside the Editor, so the help has to say so
        // rather than filing them under one view.
        assert!(out.contains("智能同步"), "{out}");
        assert!(out.contains("Editor 视图除外"), "{out}");
        // The panel is sized to its content, so nothing may be cut off — the
        // last section is the one a fixed height would have dropped.
        assert!(out.contains("编辑设置项"), "{out}");
    }

    #[test]
    fn the_input_overlay_masks_secrets() {
        let (mut app, _root) = app_with(&[("global", &[], &[])]);
        app.input = Some(Input {
            title: "密钥".into(),
            prompt: "密钥短语".into(),
            value: "hunter2".into(),
            mask: true,
            purpose: crate::tui::app::Purpose::SyncKey,
        });
        let out = render(&app, 90, 24);
        assert!(!out.contains("hunter2"), "the key was rendered: {out}");
        assert!(out.contains("*******"), "{out}");
    }

    /// Put the app in a state where the Sync view has something to say.
    fn configured(view: View) -> (App, TempRoot) {
        let (mut app, root) = app_with(&[("global", &[], &[("A", "1")])]);
        app.settings.sync.sync_id = Some("testsyncid".into());
        app.view = view;
        (app, root)
    }

    fn status(state: crate::sync::SyncState, remote: crate::sync::RemoteState) -> SyncStatus {
        SyncStatus {
            state,
            remote,
            remote_expires_at: None,
            local_hash: "h".into(),
            local_profiles: 1,
        }
    }

    fn sync_view(app: &mut App, st: SyncStatus) -> String {
        app.sync.status = Some(st);
        render(app, 100, 24)
    }

    /// The remote state is spelled out, not left to the status line. `修订 r2`
    /// on its own cannot distinguish "we are on r2" from "someone else is".
    ///
    /// This is the display half of the reused-revision hole: two machines
    /// resolving a conflict can each compute revision 2, so a panel that keyed
    /// off the number alone would show them as identical. The label has to
    /// carry the difference.
    #[test]
    fn an_unchanged_and_a_moved_remote_at_the_same_revision_read_differently() {
        let (mut app, _root) = configured(View::Sync);

        let synced = sync_view(
            &mut app,
            status(SyncState::UpToDate, RemoteState::Synced(2)),
        );
        let moved = sync_view(
            &mut app,
            status(SyncState::RemoteAhead, RemoteState::Moved(2)),
        );

        assert!(synced.contains("修订 r2"), "{synced}");
        assert!(moved.contains("修订 r2"), "{moved}");
        assert_ne!(synced, moved, "the two states rendered identically");
        assert!(moved.contains("有变化"), "{moved}");
        assert!(!synced.contains("有变化"), "{synced}");
    }

    /// A missing remote is the absence of a thing, so the panel omits the row
    /// rather than printing a label — "（无）" next to a state line that already
    /// says 远端为空 is just noise.
    #[test]
    fn a_missing_remote_is_omitted_rather_than_labelled() {
        let (mut app, _root) = configured(View::Sync);
        app.settings.sync.remote_expires_at = None;

        let out = sync_view(
            &mut app,
            status(SyncState::RemoteMissing, RemoteState::Missing),
        );

        assert!(out.contains("远端为空"), "{out}");
        assert!(!out.contains("（无）"), "{out}");
    }

    /// Pastes lapse, and they do so silently — the next push just creates a new
    /// one and the history is gone. So the expiry has to be on screen, with the
    /// remaining time, whenever the server told us one.
    #[test]
    fn the_expiry_is_shown_with_the_time_left() {
        let (mut app, _root) = configured(View::Sync);
        let mut st = status(SyncState::UpToDate, RemoteState::Synced(1));
        st.remote_expires_at = Some("2099-01-01T00:00:00Z".into());

        let out = sync_view(&mut app, st);

        assert!(out.contains("过期于"), "{out}");
        assert!(out.contains("2099-01-01T00:00:00Z"), "{out}");
        assert!(out.contains("剩余"), "{out}");
    }

    /// The key that leaves this state is the one that destroys data, so the
    /// panel must say the tool stopped. `App::remote_unreadable` reads the same
    /// state to decide whether `p` needs a confirmation, and the two must agree:
    /// a view that warned without gating, or gated without warning, would both
    /// be worse than useless.
    #[test]
    fn an_unreadable_remote_says_it_stopped_and_gates_the_push() {
        let (mut app, _root) = configured(View::Sync);

        let out = sync_view(
            &mut app,
            status(SyncState::RemoteUnreadable, RemoteState::Unreadable),
        );

        assert!(out.contains("远端无法解密"), "{out}");
        assert!(out.contains("绝不会自动覆盖"), "{out}");
        assert!(out.contains("存在，但无法解密"), "{out}");
        assert!(
            app.remote_unreadable(),
            "the push would not have been gated"
        );
    }

    /// ...and the gate must not fire on the ordinary states, or every push
    /// would nag.
    #[test]
    fn the_push_is_only_gated_when_the_remote_is_unreadable() {
        let (mut app, _root) = configured(View::Sync);
        for st in [
            status(SyncState::UpToDate, RemoteState::Synced(1)),
            status(SyncState::RemoteMissing, RemoteState::Missing),
            status(SyncState::LocalAhead, RemoteState::Moved(1)),
            status(SyncState::Diverged, RemoteState::Moved(2)),
        ] {
            let state = st.state;
            sync_view(&mut app, st);
            assert!(!app.remote_unreadable(), "gated on {state:?}");
        }
    }

    /// Before a sync id is set there is nothing to report, and the panel has to
    /// say where to go rather than render an empty status block.
    #[test]
    fn an_unconfigured_sync_points_at_the_settings_view() {
        let (mut app, _root) = app_with(&[("global", &[], &[])]);
        app.view = View::Sync;

        let out = render(&app, 100, 24);

        assert!(out.contains("尚未配置同步"), "{out}");
        assert!(out.contains("Settings"), "{out}");
    }

    /// A narrow terminal must not panic — the layout code does a lot of
    /// arithmetic on widths.
    #[test]
    fn rendering_survives_a_tiny_terminal() {
        let (mut app, _root) = app_with(&[("global", &[], &[("A", "1")])]);
        for view in View::ALL {
            app.view = view;
            render(&app, 20, 6);
        }
        app.show_help = true;
        render(&app, 20, 6);
    }
}
