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
//! (`mnemo2/src/lib/sounds.ts`); nothing here plays audio. The theme is read
//! back but not written yet — the dark theme lands in step 4 of the same plan,
//! so the page only names the stored `[ui] theme` value until then.

use serde::{Deserialize, Serialize};
use tauri::State;

use mnemo::config::general::UiConfig;
use mnemo::config::Config;
use mnemo::headless::Setup;

use crate::mnemo2::ipc::{settings_setup, Mnemo2State};

// ── Wire shapes ─────────────────────────────────────────────────────────────

/// The Appearance page's snapshot: the three `[ui]` sound toggles plus the
/// stored theme word, so the page can name the current palette while the
/// dark theme itself is still to come.
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
}

impl From<&UiConfig> for AppearanceSettingsWire {
    fn from(ui: &UiConfig) -> Self {
        Self {
            sound_complete: ui.sound_complete,
            sound_input_needed: ui.sound_input_needed,
            sound_stopped_errors: ui.sound_stopped_errors,
            theme: ui.theme.clone(),
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
}

// ── Read / write ────────────────────────────────────────────────────────────

/// Read the page snapshot from `setup`'s config dir. A missing `config.toml`
/// is not an error: every `[ui]` value then reads at its default.
fn read_settings(setup: &Setup) -> Result<AppearanceSettingsWire, String> {
    let config = Config::load(setup.config_dir()).map_err(|err| err.to_string())?;
    Ok(AppearanceSettingsWire::from(&config.general.ui))
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
    config
        .save_all(setup.config_dir())
        .map_err(|err| err.to_string())?;
    Ok(AppearanceSettingsWire::from(&config.general.ui))
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

/// Save the sound toggles through the app-side `[ui]` writer; returns the
/// saved snapshot.
#[tauri::command]
pub async fn appearance_settings_save(
    state: State<'_, Mnemo2State>,
    settings: AppearanceSettingsPatch,
) -> Result<AppearanceSettingsWire, String> {
    let (setup, _runtime) = settings_setup(&state).await?;
    save_settings(&setup, &settings)
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
            },
        )
        .expect("save every toggle");
        assert!(!saved.sound_complete);
        assert!(!saved.sound_input_needed);
        assert!(saved.sound_stopped_errors);
        assert_eq!(read_settings(&setup).expect("re-read"), saved);
    }
}
