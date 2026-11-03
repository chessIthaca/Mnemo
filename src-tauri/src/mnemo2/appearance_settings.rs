// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Appearance & sounds settings commands for Mnemo2's Settings dialog — the
//! backend of its **Appearance** page (plan f1bd54e5, step 3).
//!
//! The page's three toggles are the `[ui] sound_complete`,
//! `sound_input_needed` and `sound_stopped_errors` flags the engine already
//! persists (Mnemo1's Settings → Sounds section reads the same keys). `[ui]`
//! is deliberately absent from the engine's `SettingsSaveDto` patch — the GUI
//! owns that table — so this module writes it app-side: load the shared
//! `config.toml` through [`Config::load`], apply the flags, and save the whole
//! set back through [`Config::save_all`] (the atomic four-file commit Mnemo1's
//! own app-side settings writer uses). Every unrelated section survives
//! because the full config round-trips through memory.
//!
//! The sounds themselves are synthesized in the frontend
//! (`mnemo2/src/lib/sounds.ts`); nothing here plays audio. The theme is the
//! `[ui] theme` word (`light`, `dark` or `system`) the same page writes in
//! step 4 of the plan; `mnemo2/src/lib/theme.ts` paints it.
//!
//! The close behaviour is the `[window] close` word the page's "When I
//! close the window" group writes to Mnemo2's own `mnemo2.toml` through
//! [`window_settings::save_close_behavior`] (plan c5b76b54); the save also
//! pushes it into the managed [`window_settings::WindowBehavior`], so the
//! next window close obeys it without a restart.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use mnemo::config::general::UiConfig;
use mnemo::config::Config;
use mnemo::headless::Setup;

use crate::mnemo2::ipc::{settings_setup, Mnemo2State};
use crate::mnemo2::window_settings;

// ── Wire shapes ─────────────────────────────────────────────────────────────

/// The Appearance page's snapshot: the three `[ui]` sound toggles, the
/// stored theme word and the stored `[window] close` behaviour.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AppearanceSettingsWire {
    /// Play a short sound when a lead or worker turn finishes.
    pub sound_complete: bool,
    /// Play a short sound when the lead or a worker needs the human (an
    /// approval or a question).
    pub sound_input_needed: bool,
    /// Play a short sound when a turn stops with errors.
    pub sound_stopped_errors: bool,
    /// The stored `[ui] theme` word (`"dark"`, `"light"` or `"system"`).
    pub theme: String,
    /// The stored `[window] close` behaviour word (`"auto"`, `"tray"` or
    /// `"quit"`), read from Mnemo2's own `mnemo2.toml`.
    pub close_behavior: window_settings::CloseBehavior,
}

impl AppearanceSettingsWire {
    /// The snapshot for a config's `[ui]` table plus a close behaviour read
    /// from Mnemo2's own `mnemo2.toml`.
    fn from_parts(ui: &UiConfig, close_behavior: window_settings::CloseBehavior) -> Self {
        Self {
            sound_complete: ui.sound_complete,
            sound_input_needed: ui.sound_input_needed,
            sound_stopped_errors: ui.sound_stopped_errors,
            theme: ui.theme.clone(),
            close_behavior,
        }
    }
}

/// The page's save payload: every field optional, so one toggle flip never
/// rewrites the other two (and the theme lands here in step 4).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AppearanceSettingsPatch {
    /// New value for `[ui] sound_complete`.
    pub sound_complete: Option<bool>,
    /// New value for `[ui] sound_input_needed`.
    pub sound_input_needed: Option<bool>,
    /// New value for `[ui] sound_stopped_errors`.
    pub sound_stopped_errors: Option<bool>,
    /// New value for `[ui] theme` (`light`, `dark` or `system`).
    pub theme: Option<String>,
    /// New `[window] close` behaviour (`auto`, `tray` or `quit`), stored in
    /// Mnemo2's own `mnemo2.toml` and applied to the running app at once.
    pub close_behavior: Option<window_settings::CloseBehavior>,
}

/// The `[ui] theme` words this app writes — the same three Mnemo1's
/// Appearance section understands (`frontend/src/hooks/appearance.ts`).
const THEME_WORDS: [&str; 3] = ["light", "dark", "system"];

/// Normalise a requested palette word. An unknown word is a caller error, not
/// a silent write: the page would otherwise show a choice the next launch
/// does not paint.
fn normalize_theme(theme: &str) -> Result<String, String> {
    let word = theme.trim().to_ascii_lowercase();
    if THEME_WORDS.contains(&word.as_str()) {
        Ok(word)
    } else {
        Err(format!(
            "unknown theme {theme:?}: expected \"light\", \"dark\" or \"system\""
        ))
    }
}

// ── Read / write ────────────────────────────────────────────────────────────

/// Read the page snapshot from `setup`'s config dir. A missing `config.toml`
/// is not an error: every `[ui]` value then reads at its default.
fn read_settings(setup: &Setup) -> Result<AppearanceSettingsWire, String> {
    let config = Config::load(setup.config_dir()).map_err(|err| err.to_string())?;
    Ok(AppearanceSettingsWire::from_parts(
        &config.general.ui,
        window_settings::load_close_behavior(setup.config_dir()),
    ))
}

/// Apply the patch to `[ui]` and persist the whole config, returning the
/// saved snapshot so the page shows exactly what the next launch will read.
fn save_settings(
    setup: &Setup,
    patch: &AppearanceSettingsPatch,
) -> Result<AppearanceSettingsWire, String> {
    let mut config = Config::load(setup.config_dir()).map_err(|err| err.to_string())?;
    if let Some(value) = patch.sound_complete {
        config.general.ui.sound_complete = value;
    }
    if let Some(value) = patch.sound_input_needed {
        config.general.ui.sound_input_needed = value;
    }
    if let Some(value) = patch.sound_stopped_errors {
        config.general.ui.sound_stopped_errors = value;
    }
    if let Some(theme) = &patch.theme {
        config.general.ui.theme = normalize_theme(theme)?;
    }
    config
        .save_all(setup.config_dir())
        .map_err(|err| err.to_string())?;
    // `[window]` lives in Mnemo2's own mnemo2.toml, not in config.toml; the
    // read-modify-write keeps every other table there intact. An omitted
    // patch reports the stored behaviour unchanged.
    let close_behavior = match patch.close_behavior {
        Some(behavior) => {
            window_settings::save_close_behavior(setup.config_dir(), behavior)?;
            behavior
        }
        None => window_settings::load_close_behavior(setup.config_dir()),
    };
    Ok(AppearanceSettingsWire::from_parts(
        &config.general.ui,
        close_behavior,
    ))
}

/// Apply the patch and persist it; a changed close behaviour also lands in
/// the managed [`window_settings::WindowBehavior`], so the very next window
/// close obeys it without a restart. Without managed state (no app shell)
/// only the file changes.
fn save_settings_live(
    setup: &Setup,
    patch: &AppearanceSettingsPatch,
    live: Option<&window_settings::WindowBehavior>,
) -> Result<AppearanceSettingsWire, String> {
    let saved = save_settings(setup, patch)?;
    if let (Some(behavior), Some(live)) = (patch.close_behavior, live) {
        live.set_close_behavior(behavior);
    }
    Ok(saved)
}

// ── Commands ────────────────────────────────────────────────────────────────

/// The Appearance page snapshot: the three `[ui]` sound toggles and the
/// stored theme word.
#[tauri::command]
pub async fn appearance_settings_get(
    state: State<'_, Mnemo2State>,
) -> Result<AppearanceSettingsWire, String> {
    let (setup, _runtime) = settings_setup(&state).await?;
    read_settings(&setup)
}

/// Save the sound toggles, the theme and the close behaviour through the
/// app-side writers; returns the saved snapshot.
#[tauri::command]
pub async fn appearance_settings_save(
    app: AppHandle,
    state: State<'_, Mnemo2State>,
    settings: AppearanceSettingsPatch,
) -> Result<AppearanceSettingsWire, String> {
    use tauri::Manager as _;

    let (setup, _runtime) = settings_setup(&state).await?;
    let live = app.try_state::<window_settings::WindowBehavior>();
    save_settings_live(&setup, &settings, live.as_deref())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp config dir with no `config.toml` yet, plus a setup over it.
    fn fixture() -> (tempfile::TempDir, Setup) {
        let dir = tempfile::tempdir().expect("config dir");
        let setup = Setup::open(Some(dir.path().to_path_buf())).expect("setup");
        (dir, setup)
    }

    /// Every toggle defaults on and the theme defaults to dark: a fresh
    /// install hears its notifications and reads the same palette Mnemo1
    /// ships.
    #[test]
    fn read_defaults_when_nothing_is_saved() {
        let (_dir, setup) = fixture();
        let wire = read_settings(&setup).expect("read the defaults");
        assert!(wire.sound_complete);
        assert!(wire.sound_input_needed);
        assert!(wire.sound_stopped_errors);
        assert_eq!(wire.theme, "dark");
    }

    /// The snapshot reports exactly what `[ui]` holds, including a theme the
    /// page does not edit yet.
    #[test]
    fn read_reports_the_saved_flags_and_theme() {
        let (dir, setup) = fixture();
        std::fs::write(
            dir.path().join("config.toml"),
            "[ui]\ntheme = \"light\"\nsound_complete = false\nsound_input_needed = true\nsound_stopped_errors = false\n",
        )
        .expect("write config.toml");
        let wire = read_settings(&setup).expect("read the snapshot");
        assert!(!wire.sound_complete);
        assert!(wire.sound_input_needed);
        assert!(!wire.sound_stopped_errors);
        assert_eq!(wire.theme, "light");
    }

    /// A save persists the patched flag and leaves every unrelated section —
    /// the safety mode, another `[ui]` value, a section this module never
    /// touches — exactly as the file had it.
    #[test]
    fn save_persists_the_patch_and_keeps_unrelated_sections() {
        let (dir, setup) = fixture();
        std::fs::write(
            dir.path().join("config.toml"),
            "[general]\nsafety = \"auto-approve-project\"\n\n[ui]\ntheme = \"light\"\nsound_complete = true\nsound_input_needed = true\nsound_stopped_errors = true\n",
        )
        .expect("write config.toml");

        let saved = save_settings(
            &setup,
            &AppearanceSettingsPatch {
                sound_complete: Some(false),
                ..AppearanceSettingsPatch::default()
            },
        )
        .expect("save the patch");
        assert!(!saved.sound_complete);
        assert!(saved.sound_input_needed, "an omitted toggle is untouched");
        assert_eq!(saved.theme, "light", "the theme round-trips untouched");

        let config = Config::load(dir.path()).expect("reload the config");
        assert!(!config.general.ui.sound_complete);
        assert!(config.general.ui.sound_input_needed);
        assert!(config.general.ui.sound_stopped_errors);
        assert_eq!(config.general.ui.theme, "light");
        assert_eq!(
            config.general.general.safety,
            mnemo::config::SafetyMode::AutoApproveProject,
            "the safety section survives the [ui] write"
        );
        // And the snapshot reads back through the same path the next launch
        // takes.
        let wire = read_settings(&setup).expect("re-read the snapshot");
        assert_eq!(wire, saved);
    }

    /// Two toggles in one patch both land, and the response is the saved
    /// state rather than the request echoed back.
    #[test]
    fn save_applies_every_supplied_toggle() {
        let (_dir, setup) = fixture();
        let saved = save_settings(
            &setup,
            &AppearanceSettingsPatch {
                sound_complete: Some(false),
                sound_input_needed: Some(false),
                sound_stopped_errors: Some(true),
                ..AppearanceSettingsPatch::default()
            },
        )
        .expect("save every toggle");
        assert!(!saved.sound_complete);
        assert!(!saved.sound_input_needed);
        assert!(saved.sound_stopped_errors);
        assert_eq!(read_settings(&setup).expect("re-read"), saved);
    }

    /// A theme save persists `[ui] theme` and leaves the sound toggles alone;
    /// the snapshot returned is the saved state, so the page paints exactly
    /// what the next launch paints.
    #[test]
    fn save_persists_the_theme_and_keeps_the_toggles() {
        let (dir, setup) = fixture();
        let saved = save_settings(
            &setup,
            &AppearanceSettingsPatch {
                theme: Some(" Dark ".to_string()),
                ..AppearanceSettingsPatch::default()
            },
        )
        .expect("save the theme");
        assert_eq!(saved.theme, "dark", "the word is normalised");
        assert!(saved.sound_complete, "an omitted toggle is untouched");

        let config = Config::load(dir.path()).expect("reload the config");
        assert_eq!(config.general.ui.theme, "dark");
        assert!(config.general.ui.sound_input_needed);
        assert_eq!(read_settings(&setup).expect("re-read").theme, "dark");
    }

    /// An unknown theme word is rejected and nothing is written — the file keeps
    /// the palette it had.
    #[test]
    fn save_rejects_an_unknown_theme() {
        let (dir, setup) = fixture();
        std::fs::write(dir.path().join("config.toml"), "[ui]\ntheme = \"light\"\n")
            .expect("write config.toml");
        let error = save_settings(
            &setup,
            &AppearanceSettingsPatch {
                theme: Some("midnight".to_string()),
                ..AppearanceSettingsPatch::default()
            },
        )
        .expect_err("an unknown theme is refused");
        assert!(error.contains("unknown theme"), "{error}");
        let config = Config::load(dir.path()).expect("reload the config");
        assert_eq!(config.general.ui.theme, "light", "nothing was written");
    }

    /// Every word the page offers round-trips, including `system`, which
    /// Mnemo1's Appearance section also stores.
    #[test]
    fn every_theme_word_round_trips() {
        let (_dir, setup) = fixture();
        for word in THEME_WORDS {
            let saved = save_settings(
                &setup,
                &AppearanceSettingsPatch {
                    theme: Some(word.to_string()),
                    ..AppearanceSettingsPatch::default()
                },
            )
            .expect("save the theme");
            assert_eq!(saved.theme, word);
        }
    }

    /// The snapshot carries the `[window] close` word stored in Mnemo2's
    /// own `mnemo2.toml`, and defaults to `auto` when the file does not
    /// exist.
    #[test]
    fn snapshot_carries_the_close_behavior() {
        let (dir, setup) = fixture();
        let wire = read_settings(&setup).expect("read the defaults");
        assert_eq!(wire.close_behavior, window_settings::CloseBehavior::Auto);
        std::fs::write(
            dir.path().join("mnemo2.toml"),
            "[routing]\nmode = \"off\"\n\n[window]\nclose = \"tray\"\n",
        )
        .expect("write mnemo2.toml");
        let wire = read_settings(&setup).expect("read the snapshot");
        assert_eq!(wire.close_behavior, window_settings::CloseBehavior::Tray);
    }

    /// Saving `quit` writes `[window]` into `mnemo2.toml` (keeping the
    /// `[routing]` table) and pushes it into the managed live state, so
    /// the next window close obeys it without a restart.
    #[test]
    fn saving_quit_writes_mnemo2_toml_and_updates_the_managed_state() {
        let (dir, setup) = fixture();
        std::fs::write(
            dir.path().join("mnemo2.toml"),
            "[routing]\nmode = \"shadow\"\n",
        )
        .expect("write mnemo2.toml");
        let live = window_settings::WindowBehavior::new(window_settings::CloseBehavior::Auto);
        let saved = save_settings_live(
            &setup,
            &AppearanceSettingsPatch {
                close_behavior: Some(window_settings::CloseBehavior::Quit),
                ..AppearanceSettingsPatch::default()
            },
            Some(&live),
        )
        .expect("save the close behaviour");
        assert_eq!(saved.close_behavior, window_settings::CloseBehavior::Quit);
        assert_eq!(
            live.close_behavior(),
            window_settings::CloseBehavior::Quit,
            "the live state takes the saved behaviour at once"
        );
        let text = std::fs::read_to_string(dir.path().join("mnemo2.toml"))
            .expect("read mnemo2.toml");
        assert!(
            text.contains("close = \"quit\""),
            "the word is stored: {text}"
        );
        assert!(
            text.contains("mode = \"shadow\""),
            "the routing table survives the [window] write: {text}"
        );
    }
}
