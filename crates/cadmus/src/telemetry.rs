//! Wiring-layer telemetry: the real clock, the id sequence, trace-id minting
//! and trace-root resolution. These are the boundary implementations of the
//! contract ports — the core only ever sees the traits.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use cadmus_contract::{Clock, IdSequence};

/// The wall clock, injected into the loop.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_ms(&self) -> u64 {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis());
        u64::try_from(millis).unwrap_or(u64::MAX)
    }
}

/// Sequential ids per run: 1, 2, 3, …
///
/// Deliberately duplicated from `cadmus_core::testing::SeqIds` — the test
/// double must not leak into the binary through a testing module; keep the
/// two in sync (five lines each).
#[derive(Default)]
pub struct SeqIds(AtomicU64);

impl IdSequence for SeqIds {
    fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// Mints the run's trace id (IO wrapper over the pure
/// `cadmus_memory::mint_trace_id`): the current time, this process, and a
/// counter covering coarse-resolution clocks and the eval harness minting
/// many traces in one process. Mint time must be the run's start — the id
/// carries the UTC shard date the JSONL adapter computes paths from.
#[must_use]
pub fn mint_trace_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    cadmus_memory::mint_trace_id(
        nanos,
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    )
}

/// Trace-root resolution when `--trace-root` is absent:
/// `$CADMUS_TRACE_ROOT`, then `$XDG_DATA_HOME/cadmus/traces`, then
/// `~/.local/share/cadmus/traces`, then
/// `%USERPROFILE%/AppData/Roaming/cadmus/traces`.
///
/// Hand-rolled under the zero-new-dependency policy (no `dirs` crate): yes,
/// macOS gets the XDG path and a git-bash Windows shell the `HOME` one —
/// documented rather than clever. Not unit-tested: process-env mutation is
/// `unsafe` in edition 2024 and this workspace forbids `unsafe_code`.
#[must_use]
pub fn default_trace_root() -> Option<PathBuf> {
    fn env(key: &str) -> Option<PathBuf> {
        std::env::var_os(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }
    if let Some(root) = env("CADMUS_TRACE_ROOT") {
        return Some(root);
    }
    if let Some(xdg) = env("XDG_DATA_HOME") {
        return Some(xdg.join("cadmus/traces"));
    }
    if let Some(home) = env("HOME") {
        return Some(home.join(".local/share/cadmus/traces"));
    }
    env("USERPROFILE").map(|profile| profile.join("AppData/Roaming/cadmus/traces"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_ids_are_unique() {
        assert_ne!(
            mint_trace_id(),
            mint_trace_id(),
            "the counter separates rapid mints"
        );
    }
}
