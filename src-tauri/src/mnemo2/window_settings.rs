// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The `[window] close` behaviour: what closing Mnemo2's main window does
//! (plan c5b76b54).
//!
//! The setting lives in Mnemo2's own `mnemo2.toml` — the file the Laya page's
//! `[routing]` table already occupies — as `close = "auto" | "tray" | "quit"`,
//! default `auto`, and never in the shared `config.toml`:
//!
//! - `auto` is today's behaviour: a hidden-start Mnemo2 (launched with
//!   `--hidden`, as the Claude Code plugin's helper does) hides to the tray
//!   when a tray icon exists, every other start closes and exits;
//! - `tray` always hides — but only when a tray icon exists, because
//!   without one there would be no way back to the window;
//! - `quit` routes the close through `request_quit`, so running workers
//!   still get their confirmation dialog.
//!
//! The stored word is loaded once at startup into the managed
//! [`WindowBehavior`] and read live on every close, so an Appearance-page
//! save applies without a restart. Writes follow the read-modify-write
//! discipline `dispatch_routing::save_routing_settings` established, so
//! every other table in the file survives.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::mnemo2::dispatch_routing;

/// What the stored `[window] close` word says closing the window does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CloseBehavior {
    /// Hide to the tray when the window started hidden and a tray icon
    /// exists, else close and exit (today's behaviour).
    #[default]
    Auto,
    /// Always hide to the tray when a tray icon exists; without one there
    /// is no way back, so the window closes and exits normally.
    Tray,
    /// Always quit through the tray-quit path, which asks first while
    /// workers are running.
    Quit,
}

/// What one close of the window actually does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseAction {
    /// Park in the tray: the window hides, the app keeps running.
    HideToTray,
    /// Quit through `request_quit` (running workers confirm first).
    Quit,
    /// Do nothing: the window closes and the app exits as before.
    CloseNormally,
}

/// Pure decision: what closing the main window does, given the stored
/// behaviour, whether the window started hidden and whether a tray icon
/// exists. [`CloseBehavior::Auto`] hides only for a hidden start behind a
/// tray; [`CloseBehavior::Tray`] hides whenever a tray exists;
/// [`CloseBehavior::Quit`] always quits — a tray is never required, because
/// quitting never strands the window.
pub fn close_action(behavior: CloseBehavior, started_hidden: bool, tray_ok: bool) -> CloseAction {
    match behavior {
        CloseBehavior::Auto => {
            if started_hidden && tray_ok {
                CloseAction::HideToTray
            } else {
                CloseAction::CloseNormally
            }
        }
        CloseBehavior::Tray => {
            if tray_ok {
                CloseAction::HideToTray
            } else {
                CloseAction::CloseNormally
            }
        }
        CloseBehavior::Quit => CloseAction::Quit,
    }
}

/// The `[window]` table as this module writes it — the shape the load path
/// reads back.
#[derive(Debug, Serialize)]
struct WindowTable {
    close: CloseBehavior,
}

/// Read the stored `[window] close` word from Mnemo2's settings file. A
/// missing file or table is a fresh install and reads as the default; an
/// unreadable file or an invalid value also reads as the default, with a
/// warning on stderr.
pub fn load_close_behavior(config_dir: &Path) -> CloseBehavior {
    let path = dispatch_routing::routing_settings_path(config_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CloseBehavior::default();
        }
        Err(error) => {
            eprintln!(
                "mnemo2: using the default window close behaviour; cannot read {}: {error}",
                path.display()
            );
            return CloseBehavior::default();
        }
    };
    let document = match toml::from_str::<toml::Value>(&text) {
        Ok(document) => document,
        Err(error) => {
            eprintln!(
                "mnemo2: using the default window close behaviour; invalid {}: {error}",
                path.display()
            );
            return CloseBehavior::default();
        }
    };
    let Some(close) = document.get("window").and_then(|window| window.get("close")) else {
        return CloseBehavior::default();
    };
    match close.clone().try_into() {
        Ok(behavior) => behavior,
        Err(error) => {
            eprintln!(
                "mnemo2: using the default window close behaviour; invalid [window] close in {}: {error}",
                path.display()
            );
            CloseBehavior::default()
        }
    }
}

/// Save `[window] close` without disturbing any other table in
/// `mnemo2.toml` — the same read-modify-write discipline
/// `dispatch_routing::save_routing_settings` uses for `[routing]`.
pub fn save_close_behavior(config_dir: &Path, behavior: CloseBehavior) -> Result<(), String> {
    let path = dispatch_routing::routing_settings_path(config_dir);
    let mut document = match std::fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => toml::from_str::<toml::Value>(&text)
            .map_err(|error| format!("invalid {}: {error}", path.display()))?,
        Ok(_) => toml::Value::Table(toml::map::Map::new()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            toml::Value::Table(toml::map::Map::new())
        }
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let table = document
        .as_table_mut()
        .ok_or_else(|| format!("{} must contain a TOML table", path.display()))?;
    let encoded = toml::to_string_pretty(&WindowTable { close: behavior })
        .map_err(|error| format!("cannot serialize Mnemo2 window settings: {error}"))?;
    let window = toml::from_str::<toml::Value>(&encoded)
        .map_err(|error| format!("cannot encode Mnemo2 window settings: {error}"))?;
    table.insert("window".into(), window);
    let encoded = toml::to_string_pretty(&document)
        .map_err(|error| format!("cannot serialize {}: {error}", path.display()))?;
    std::fs::create_dir_all(config_dir)
        .map_err(|error| format!("cannot create {}: {error}", config_dir.display()))?;
    std::fs::write(&path, encoded)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// The live close behaviour Tauri manages: the loaded setting — read on
/// every close and updated by an Appearance save, so saves apply without a
/// restart — plus the flag that makes the tray notice a once-per-process
/// event.
pub struct WindowBehavior {
    /// The current `[window] close` behaviour.
    pub close: std::sync::RwLock<CloseBehavior>,
    /// Whether the tray notice has been shown this run.
    pub tray_notice_shown: std::sync::atomic::AtomicBool,
}

impl WindowBehavior {
    /// The managed state for a loaded close behaviour; the tray notice is
    /// still owed for this run.
    pub fn new(close: CloseBehavior) -> Self {
        Self {
            close: std::sync::RwLock::new(close),
            tray_notice_shown: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// The current close behaviour. A poisoned lock reads as the default
    /// rather than panicking inside a window event.
    pub fn close_behavior(&self) -> CloseBehavior {
        self.close.read().map(|guard| *guard).unwrap_or_default()
    }

    /// Replace the close behaviour — an Appearance save landing while the
    /// app runs. A poisoned lock keeps the stored behaviour.
    pub fn set_close_behavior(&self, behavior: CloseBehavior) {
        if let Ok(mut guard) = self.close.write() {
            *guard = behavior;
        }
    }

    /// Claim the once-per-process tray notification: `true` the first time
    /// a close hides to the tray, `false` on every later one.
    pub fn take_tray_notice(&self) -> bool {
        self.tray_notice_shown
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The full decision table: every behaviour × whether the window
    /// started hidden × whether a tray icon exists.
    #[test]
    fn close_action_table() {
        use CloseAction::{CloseNormally, HideToTray, Quit as QuitAction};

        // Auto is today's behaviour: hide only when the window started
        // hidden behind a tray icon.
        assert_eq!(close_action(CloseBehavior::Auto, true, true), HideToTray);
        assert_eq!(
            close_action(CloseBehavior::Auto, true, false),
            CloseNormally
        );
        assert_eq!(
            close_action(CloseBehavior::Auto, false, true),
            CloseNormally
        );
        assert_eq!(
            close_action(CloseBehavior::Auto, false, false),
            CloseNormally
        );

        // Tray always hides — but without a tray there is no way back.
        assert_eq!(close_action(CloseBehavior::Tray, true, true), HideToTray);
        assert_eq!(close_action(CloseBehavior::Tray, false, true), HideToTray);
        assert_eq!(
            close_action(CloseBehavior::Tray, true, false),
            CloseNormally
        );
        assert_eq!(
            close_action(CloseBehavior::Tray, false, false),
            CloseNormally
        );

        // Quit always quits, tray or no tray.
        assert_eq!(close_action(CloseBehavior::Quit, true, true), QuitAction);
        assert_eq!(close_action(CloseBehavior::Quit, false, true), QuitAction);
        assert_eq!(close_action(CloseBehavior::Quit, true, false), QuitAction);
        assert_eq!(close_action(CloseBehavior::Quit, false, false), QuitAction);
    }

    /// A missing file, a file without the table or key, and an invalid word
    /// or document all read as the default behaviour.
    #[test]
    fn missing_or_invalid_close_setting_is_auto() {
        let dir = tempfile::tempdir().expect("config dir");
        // No mnemo2.toml at all: a fresh install.
        assert_eq!(load_close_behavior(dir.path()), CloseBehavior::Auto);
        // A file with other tables but no [window].
        std::fs::write(dir.path().join("mnemo2.toml"), "[routing]\nmode = \"off\"\n")
            .expect("write mnemo2.toml");
        assert_eq!(load_close_behavior(dir.path()), CloseBehavior::Auto);
        // A [window] table without the close key.
        std::fs::write(dir.path().join("mnemo2.toml"), "[window]\n").expect("write mnemo2.toml");
        assert_eq!(load_close_behavior(dir.path()), CloseBehavior::Auto);
        // A close word this version does not know.
        std::fs::write(
            dir.path().join("mnemo2.toml"),
            "[window]\nclose = \"minimise\"\n",
        )
        .expect("write mnemo2.toml");
        assert_eq!(load_close_behavior(dir.path()), CloseBehavior::Auto);
        // A close value of the wrong type.
        std::fs::write(dir.path().join("mnemo2.toml"), "[window]\nclose = 3\n")
            .expect("write mnemo2.toml");
        assert_eq!(load_close_behavior(dir.path()), CloseBehavior::Auto);
        // A document that is not valid TOML at all.
        std::fs::write(dir.path().join("mnemo2.toml"), "[window\n").expect("write mnemo2.toml");
        assert_eq!(load_close_behavior(dir.path()), CloseBehavior::Auto);
    }

    /// A save lands in `[window]` and leaves the `[routing]` table the Laya
    /// page wrote exactly as it was; every word round-trips through the
    /// load path.
    #[test]
    fn save_close_behavior_keeps_the_routing_table() {
        let dir = tempfile::tempdir().expect("config dir");
        std::fs::write(
            dir.path().join("mnemo2.toml"),
            "[routing]\nmode = \"shadow\"\n",
        )
        .expect("write mnemo2.toml");

        for behavior in [CloseBehavior::Auto, CloseBehavior::Tray, CloseBehavior::Quit] {
            save_close_behavior(dir.path(), behavior).expect("save the close behaviour");
            assert_eq!(load_close_behavior(dir.path()), behavior);
        }

        let text = std::fs::read_to_string(dir.path().join("mnemo2.toml"))
            .expect("read mnemo2.toml");
        assert!(
            text.contains("close = \"quit\""),
            "the last word is stored: {text}"
        );
        assert!(
            text.contains("mode = \"shadow\""),
            "the routing table survives: {text}"
        );
        let document = toml::from_str::<toml::Value>(&text).expect("the file still parses");
        assert_eq!(
            document
                .get("routing")
                .and_then(|routing| routing.get("mode"))
                .and_then(|mode| mode.as_str()),
            Some("shadow")
        );
    }

    /// The tray notice is claimable exactly once per process, and the live
    /// behaviour reads back whatever an Appearance save pushed into it.
    #[test]
    fn tray_notice_is_taken_once() {
        let behavior = WindowBehavior::new(CloseBehavior::Auto);
        assert!(behavior.take_tray_notice(), "the first close claims the notice");
        assert!(!behavior.take_tray_notice(), "the second close does not");
        assert!(!behavior.take_tray_notice(), "nor any later one");

        assert_eq!(behavior.close_behavior(), CloseBehavior::Auto);
        behavior.set_close_behavior(CloseBehavior::Quit);
        assert_eq!(behavior.close_behavior(), CloseBehavior::Quit);
        // The notice flag and the behaviour are independent.
        assert!(!behavior.take_tray_notice());
    }
}