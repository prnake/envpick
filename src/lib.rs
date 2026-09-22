//! envpick — profile-based environment variable manager with a TUI and
//! end-to-end encrypted sync over a pastebin backend.
//!
//! Module map:
//! - [`cli`]      — the clap command definitions
//! - [`clock`]    — minimal UTC timestamp formatting/parsing
//! - [`config`]   — on-disk settings and profile definitions
//! - [`graph`]    — profile dependency resolution
//! - [`handlers`] — one module per command group
//! - [`shell`]    — computing and emitting activation code
//! - [`sync`]     — HKDF/AES-GCM crypto and the pastebin transport
//! - [`text`]     — every user-facing string
//! - [`tui`]      — the interactive interface
//! - [`update`]   — checking GitHub for, and installing, a newer release

pub mod cli;
pub mod clock;
pub mod config;
pub mod graph;
pub mod handlers;
pub mod shell;
pub mod sync;
pub mod text;
pub mod tui;
pub mod update;

#[cfg(test)]
pub mod testing;

pub use config::{Paths, Settings};
