//! The interactive interface.
//!
//! Two things make this safe to run in a real terminal:
//!
//! - a [`TerminalGuard`] restores the terminal through `Drop`, so an early
//!   return anywhere still leaves the shell usable;
//! - a panic hook restores it *before* printing the panic, because `Drop` does
//!   not run reliably during a panic unwind and a terminal left in raw mode with
//!   no cursor is a genuinely bad state to hand back to a user.

pub mod app;
pub mod event;
pub mod ui;

use std::io::{IsTerminal, Stdout};

use anyhow::{Context, Result};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use crate::handlers::Ctx;
use app::App;

pub fn run(ctx: &mut Ctx) -> Result<()> {
    if !std::io::stdout().is_terminal() {
        anyhow::bail!(crate::text::errors::NOT_TTY);
    }

    install_panic_hook();
    let _guard = TerminalGuard::enter()?;

    let mut terminal =
        Terminal::new(CrosstermBackend::new(std::io::stdout())).context("初始化终端失败")?;
    let mut app = App::new(ctx.paths.clone(), ctx.store.clone(), ctx.settings.clone());
    let result = event_loop(&mut terminal, &mut app);

    // Give the caller back whatever the user did, so a later `save` sees it.
    ctx.store = app.store;
    ctx.settings = app.settings;
    result
}

fn event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    while !app.quit {
        terminal.draw(|frame| ui::draw(frame, app))?;

        match ratatui::crossterm::event::read()? {
            Event::Key(key) => {
                // Windows reports both press and release; acting on both would
                // double every keystroke.
                if key.kind == KeyEventKind::Press {
                    event::handle(app, key);
                }
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
    Ok(())
}

/// Puts the terminal back the way it was found.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("无法进入 raw 模式")?;
        // If this fails we are in raw mode with no way back, so undo it.
        if let Err(e) = execute!(std::io::stdout(), EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(e).context("无法切换到备用屏幕");
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
    }
}

fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(
        std::io::stdout(),
        LeaveAlternateScreen,
        ratatui::crossterm::cursor::Show
    );
}

/// Restore the terminal, then defer to the previous hook so the panic is still
/// reported normally.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
}
