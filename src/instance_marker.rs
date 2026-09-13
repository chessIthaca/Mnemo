// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Same-project instance markers: `<project>/.coding/instance.json` records
//! which mnemo instance (pid + started_at) was the last to launch on this
//! project. When a second instance opens the same project while the first is
//! still alive, the frontend asks before opening it (2027-01-13: the
//! per-instance WebView2 user data folder allows multiple instances, so the
//! shared project deserves an explicit warning).
//!
//! The marker is best-effort — `write` failures (read-only project, disk
//! trouble) must never block startup. Stale markers from dead instances are
//! ignored: `conflict_for` consults a caller-supplied aliveness predicate
//! (`startup::instance_pid_alive` in the app — Windows `OpenProcess` probe,
//! elsewhere a `ps` check) instead of enumerating processes here, which
//! keeps this module platform-neutral and unit-testable.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::project::Project;

/// One instance's claim on a project — the `.coding/instance.json` shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceMarker {
    /// The process id of the launching mnemo instance.
    pub pid: u32,
    /// Unix epoch seconds when that instance launched.
    pub started_at: u64,
}

impl InstanceMarker {
    /// A marker for the current process.
    pub fn new_self() -> Self {
        Self {
            pid: std::process::id(),
            started_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }

    /// Best-effort write of this marker to `<dir>/.coding/instance.json`
    /// (last writer wins — the next launch overwrites the previous one).
    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        let coding = dir.join(Project::CODING_DIR_NAME);
        std::fs::create_dir_all(&coding)?;
        let json = serde_json::to_string_pretty(self)
            .unwrap_or_else(|_| "{}".to_string());
        std::fs::write(coding.join("instance.json"), json)
    }
}

/// Read the marker another instance left in `<dir>/.coding/instance.json`.
pub fn read_marker(dir: &Path) -> Option<InstanceMarker> {
    let bytes =
        std::fs::read(dir.join(Project::CODING_DIR_NAME).join("instance.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The conflict a fresh instance faces on a project: `Some(marker)` when the
/// stored marker belongs to ANOTHER, still-alive instance; `None` when the
/// marker is absent, stale (its pid is dead), or our own (a crash re-launch
/// — the last writer is treated as the incumbent).
pub fn conflict_for(dir: &Path, own_pid: u32, alive: impl Fn(u32) -> bool) -> Option<InstanceMarker> {
    let marker = read_marker(dir)?;
    if marker.pid == own_pid {
        return None;
    }
    alive(marker.pid).then_some(marker)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh temp dir per test (the module's tests run in PARALLEL — a
    /// shared dir would leak one test's marker into another).
    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mnemo-instance-marker-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = temp_dir("roundtrip");
        let marker = InstanceMarker {
            pid: 1234,
            started_at: 42,
        };
        marker.write(&dir).expect("marker write must succeed");
        assert_eq!(read_marker(&dir), Some(marker));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn last_writer_wins() {
        let dir = temp_dir("lww");
        InstanceMarker {
            pid: 11,
            started_at: 1,
        }
        .write(&dir)
        .unwrap();
        let incumbent = InstanceMarker {
            pid: 22,
            started_at: 2,
        };
        incumbent.write(&dir).unwrap();
        assert_eq!(read_marker(&dir), Some(incumbent));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn conflict_requires_another_live_pid() {
        let dir = temp_dir("conflict");
        // No marker — no conflict.
        assert_eq!(conflict_for(&dir, 99, |_| true), None);
        // Our own marker is never a conflict (crash re-launch).
        let mine = InstanceMarker {
            pid: 99,
            started_at: 7,
        };
        mine.write(&dir).unwrap();
        assert_eq!(conflict_for(&dir, 99, |_| true), None);
        // Another instance's marker, alive → conflict.
        let other = InstanceMarker {
            pid: 7,
            started_at: 7,
        };
        other.write(&dir).unwrap();
        assert_eq!(conflict_for(&dir, 99, |_| true), Some(other));
        // ... but with the pid dead the marker is stale → ignored.
        assert_eq!(conflict_for(&dir, 99, |_| false), None);
        assert_eq!(conflict_for(&dir, 99, |pid| pid == 7), Some(other));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
