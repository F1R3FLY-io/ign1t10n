//! ign1t10n: installs and manages a local F1R3Node-Rust shard (a bootstrap
//! node, 2 to 10 validators, an observer and, optionally, Embers) for the
//! F1R3Gaze browser, and F1R3Games served to any web browser. See
//! `docs/ign1t10n-macos-installer-spec` (v0.4) and
//! `docs/IMPLEMENTATION.md` for the decisions this code applies.
//!
//! Everything except `platform::macos` and `ui::appkit` is portable and is
//! built and tested on Linux CI.

pub mod admin;
pub mod amounts;
pub mod api;
pub mod control;
pub mod embers;
pub mod games;
pub mod gaze;
pub mod genesis;
pub mod keys;
pub mod lifecycle;
pub mod logging;
pub mod manifest;
pub mod nodeconf;
pub mod paths;
pub mod platform;
pub mod ports;
pub mod procs;
pub mod provision;
pub mod resize;
pub mod rho;
pub mod secrets;
pub mod supervisor;
pub mod ui;

pub const BUNDLE_ID: &str = "io.f1r3fly.ign1t10n";
pub const AGENT_LABEL: &str = "io.f1r3fly.ign1t10n.supervisor";
pub const AGENT_PLIST: &str = "io.f1r3fly.ign1t10n.supervisor.plist";
pub const GAZE_BUNDLE_ID: &str = "io.f1r3fly.f1r3gaze";
pub const MIN_VALIDATORS: u8 = 2;
pub const MAX_VALIDATORS: u8 = 10;
pub const DEFAULT_VALIDATORS: u8 = 2;
pub const SHARD_ID: &str = "root";

/// Milliseconds since the epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Random bytes from the operating system.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("the operating system's random source failed");
    b
}
