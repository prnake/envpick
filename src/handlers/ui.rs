//! The TUI entry point.

use anyhow::Result;

use crate::handlers::Ctx;

pub fn run(ctx: &mut Ctx) -> Result<()> {
    crate::tui::run(ctx)
}
