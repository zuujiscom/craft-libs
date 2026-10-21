#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

//! Crash recovery for the Craft apps' web builds, and the page hooks a web build needs around
//! saving. On the desktop the apps keep recovery copies in a folder; in the browser there is no
//! folder, so this crate keeps them in IndexedDB (the browser-storage store that #124 leaves for
//! later), with the same rules:
//!
//! - **One area per running app.** Each page load is a session and holds a Web Lock named after
//!   it for as long as it runs, so another tab never offers, adopts or deletes the copies of a tab
//!   that is still open. Copies left by sessions nobody holds any more (the tab was closed or
//!   crashed) are what [`open`] recovers, and it claims them for the new session under a short
//!   exclusive lock, so two tabs opened together never both take the same copy.
//! - **Commit, then acknowledge.** [`Store::put`] returns at once and the write runs in the
//!   background; its outcome is reported by [`Store::take_written`] only after IndexedDB has
//!   committed the transaction (a full disk ends up there as an error), so an app records a
//!   revision as saved only when it really is.
//! - **An entry stays until the app removes it** ([`Store::remove`]: the document was saved or
//!   closed), never just because it was recovered.
//!
//! The app keeps everything about its documents: what a copy contains (`bytes`, usually the
//! native file format, plus optional `meta` text), the autosave timer and its preferences, the
//! restore prompt and the "(Recovered)" title.
//!
//! Also here, for the web builds' Save flow:
//! - [`now_ms`]: a wall clock that works in the browser (`std::time::Instant` and `SystemTime`
//!   panic or return nothing useful on `wasm32-unknown-unknown`, which silently stops autosave
//!   timers);
//! - [`UnloadGuard`]: the browser's "Leave site?" question while there are unsaved changes;
//! - [`HiddenHook`]: runs when the tab is hidden (switched away from, minimised, the phone
//!   locked), the last moment a browser promises before it may discard the page;
//! - [`download`]: Save as a browser download, done in a way Safari accepts.
//!
//! Everything except [`now_ms`] exists only on `wasm32`.

mod plan;

#[cfg(target_arch = "wasm32")]
mod idb;
#[cfg(target_arch = "wasm32")]
mod kv;
#[cfg(target_arch = "wasm32")]
mod locks;
#[cfg(target_arch = "wasm32")]
mod page;
#[cfg(target_arch = "wasm32")]
mod store;

#[cfg(target_arch = "wasm32")]
pub use kv::{ForeignChange, Kv, open_kv};
#[cfg(target_arch = "wasm32")]
pub use page::{HiddenHook, UnloadGuard, download};
#[cfg(target_arch = "wasm32")]
pub use store::{Opened, Recovered, Snapshot, Store, Written, open};

pub use plan::Warning;

/// Milliseconds since 1970 by the wall clock, on every target (in the browser: `Date.now()`).
pub fn now_ms() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64() * 1000.0)
    }
}

#[cfg(test)]
mod tests;
