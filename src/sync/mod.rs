//! End-to-end encrypted sync.
//!
//! - [`crypto`]   — HKDF key derivation and AES-256-GCM in the pastebin's wire format
//! - [`doc`]      — the synced document and its canonical hashing
//! - [`pastebin`] — HTTP transport
//! - [`engine`]   — the push/pull/conflict state machine

pub mod crypto;
pub mod doc;
pub mod engine;
pub mod pastebin;

#[cfg(test)]
pub mod mock;

pub use crypto::SyncCrypto;
pub use doc::SyncDoc;
pub use engine::{RemoteState, SyncEngine, SyncOutcome, SyncState, SyncStatus};
pub use pastebin::PastebinClient;
