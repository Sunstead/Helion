//! Types shared between `helion-agent` and the desktop app.
//!
//! Every type here derives `ts_rs::TS` and exports to `app/src/generated/`.
//! Run `cargo test -p helion-common` to regenerate the TypeScript bindings —
//! that directory is gitignored, so a fresh clone has none until you do.
//!
//! Note `export_to` is resolved against `helion-common/bindings/`, not against
//! the source file, so every module here uses the identical path string.
//!
//! ## `#[ts(type = "number")]` on 64-bit integers
//!
//! ts-rs maps `u64`/`i64` to TypeScript `bigint`, but serde writes them as
//! plain JSON numbers and `JSON.parse` produces `number` — so the unannotated
//! binding would be a type that never matches the value at runtime. Every
//! 64-bit field here is a byte count, a unix timestamp or a counter, all of
//! which sit far below `Number.MAX_SAFE_INTEGER` (9 PB, in the case of bytes),
//! so `number` is both accurate and what the JSON actually contains.

pub mod backups;
pub mod docker;
pub mod error;
pub mod events;
pub mod hardware;
pub mod history;
pub mod host;
pub mod meta;
pub mod notify;
pub mod peers;
pub mod services;
pub mod tailnet;
pub mod updates;
pub mod uptime;
pub mod wol;

// Flat re-export: `helion_common::types::HostInfo` keeps working, and callers
// don't have to care which file a type happens to live in.
pub use backups::*;
pub use docker::*;
pub use error::*;
pub use events::*;
pub use hardware::*;
pub use history::*;
pub use host::*;
pub use meta::*;
pub use notify::*;
pub use peers::*;
pub use services::*;
pub use tailnet::*;
pub use updates::*;
pub use uptime::*;
pub use wol::*;
