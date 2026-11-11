// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Mnemo2's hang watchdog wiring.
//!
//! The watchdog itself lives in
//! [`crate::watchdog`](crate::watchdog); this module owns Mnemo2's side:
//! it starts the detector once on the main thread during setup, points its
//! reports at the open project's `.coding/logs` (a temp folder until a
//! project opens) and stops it when the exit begins, so the app's own
//! shutdown is never reported as a freeze.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::watchdog::Watchdog;

/// The one watchdog Mnemo2 runs; [`start`] fills it, the exit path stops it.
static WATCHDOG: OnceLock<Arc<Watchdog>> = OnceLock::new();

/// Reports before a project is open: %TEMP%/mnemo2-hang-reports.
pub fn fallback_dir() -> PathBuf {
    std::env::temp_dir().join("mnemo2-hang-reports")
}

/// A project's report folder: <root>/.coding/logs.
pub fn project_dir(root: &Path) -> PathBuf {
    root.join(".coding").join("logs")
}

/// Start the watchdog once. Must run on the main thread (setup). A second
/// call does nothing.
pub fn start(app: &tauri::AppHandle) {
    if WATCHDOG.get().is_some() {
        return;
    }
    let _ = WATCHDOG.set(Watchdog::start(app.clone(), fallback_dir()));
}

/// Add an activity note to the next report; a no-op before start.
pub fn note(activity: impl Into<String>) {
    if let Some(watchdog) = WATCHDOG.get() {
        watchdog.note(activity);
    }
}

/// Move where reports go; a no-op before start.
pub fn set_report_dir(dir: PathBuf) {
    if let Some(watchdog) = WATCHDOG.get() {
        watchdog.set_report_dir(dir);
    }
}

/// Stop the detector (exit); a no-op before start.
pub fn stop() {
    if let Some(watchdog) = WATCHDOG.get() {
        watchdog.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_dir_is_coding_logs() {
        assert_eq!(
            project_dir(Path::new("C:/work/app")),
            PathBuf::from("C:/work/app/.coding/logs")
        );
    }

    #[test]
    fn calls_before_start_do_nothing() {
        // No test in this binary may call start: it needs a main-thread
        // AppHandle, which only setup has. These calls must therefore be
        // calm no-ops on a watchdog that never started.
        note("a note before start");
        set_report_dir(std::env::temp_dir());
        stop();
        assert!(WATCHDOG.get().is_none());
    }
}
