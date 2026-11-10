// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The main window's remembered size and position.
//!
//! `mnemo2-window.json` in the config dir stores the last geometry
//! ([`SavedGeometry`]; every value is a PHYSICAL pixel: the outer top-left,
//! the inner size and the outer size, so the window frame is derivable).
//! On startup [`clamp_restored`] pulls a saved window that is now off screen
//! or too small fully onto a monitor — the same rules Mnemo1's restore
//! uses: a monitor keeps a position only when at least 80 logical px
//! overlap its work area on both axes, an unusable one centres the window,
//! and the size stays between 800x560 logical and the monitor's work area.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The geometry file's name inside the config dir.
pub const FILE_NAME: &str = "mnemo2-window.json";

/// The smallest restored window width, in LOGICAL px (times the scale).
pub const RESTORE_MIN_WIDTH: f64 = 800.0;

/// The smallest restored window height, in LOGICAL px (times the scale).
pub const RESTORE_MIN_HEIGHT: f64 = 560.0;

/// The LOGICAL px that must overlap a work area on both axes for a saved
/// position to count as on screen.
pub const MIN_VISIBLE: f64 = 80.0;

/// The persisted geometry: `x`/`y` are the outer top-left, `width`/`height`
/// the inner size and `outer_*` the outer size (inner + frame). Every value
/// is a PHYSICAL pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub outer_width: u32,
    pub outer_height: u32,
    /// True when the window was maximized; a maximized window keeps its
    /// pre-maximize bounds and only flips this flag.
    #[serde(default)]
    pub maximized: bool,
}

/// One monitor's work area in PHYSICAL pixels, plus its scale factor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorkArea {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

impl WorkArea {
    /// A monitor's work area and scale, in physical pixels.
    pub fn of(monitor: &tauri::Monitor) -> WorkArea {
        let area = monitor.work_area();
        WorkArea {
            x: area.position.x,
            y: area.position.y,
            width: area.size.width,
            height: area.size.height,
            scale: monitor.scale_factor(),
        }
    }
}

/// Where the window should open: the inner size and outer position to apply,
/// and whether to maximize afterwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
    pub maximized: bool,
}

/// Pull a saved window onto a usable monitor. Returns `None` when there are
/// no monitors (the caller keeps today's centred default).
pub fn clamp_restored(saved: &SavedGeometry, areas: &[WorkArea]) -> Option<Placement> {
    if areas.is_empty() {
        return None;
    }
    // The window frame in physical px: the saved outer size minus the inner.
    let frame_w = (saved.outer_width.saturating_sub(saved.width)) as i64;
    let frame_h = (saved.outer_height.saturating_sub(saved.height)) as i64;

    // Score every area by the window's overlap with it. A usable area keeps
    // at least MIN_VISIBLE logical px of overlap on both axes; the target is
    // the usable area holding the largest overlap (ties go to the first),
    // or — when none is usable — the area with the largest overlap, the
    // first when every overlap is zero.
    let mut usable_best: Option<(usize, i64)> = None;
    let mut any_best: Option<(usize, i64)> = None;
    for (index, area) in areas.iter().enumerate() {
        let overlap_x = i64::min(
            saved.x as i64 + saved.outer_width as i64,
            area.x as i64 + area.width as i64,
        ) - i64::max(saved.x as i64, area.x as i64);
        let overlap_y = i64::min(
            saved.y as i64 + saved.outer_height as i64,
            area.y as i64 + area.height as i64,
        ) - i64::max(saved.y as i64, area.y as i64);
        let overlap_area = overlap_x.max(0) * overlap_y.max(0);
        if overlap_x as f64 >= MIN_VISIBLE * area.scale && overlap_y as f64 >= MIN_VISIBLE * area.scale
            && usable_best.is_none_or(|(_, best)| overlap_area > best)
        {
            usable_best = Some((index, overlap_area));
        }
        if any_best.is_none_or(|(_, best)| overlap_area > best) {
            any_best = Some((index, overlap_area));
        }
    }
    let (target, positioned) = match usable_best {
        Some((index, _)) => (areas[index], true),
        None => (any_best.map(|(index, _)| areas[index]).unwrap_or(areas[0]), false),
    };

    // Size: never below the scaled minimum, never past the work area.
    let min_w = (RESTORE_MIN_WIDTH * target.scale).round() as i64;
    let min_h = (RESTORE_MIN_HEIGHT * target.scale).round() as i64;
    let max_w = min_w.max(target.width as i64 - frame_w);
    let max_h = min_h.max(target.height as i64 - frame_h);
    let width = (saved.width as i64).clamp(min_w, max_w);
    let height = (saved.height as i64).clamp(min_h, max_h);

    // Position: keep the saved one when the window is on screen, centre in
    // the target area otherwise; then clamp fully inside the work area.
    let outer_w = width + frame_w;
    let outer_h = height + frame_h;
    let (wanted_x, wanted_y) = if positioned {
        (saved.x as i64, saved.y as i64)
    } else {
        (
            target.x as i64 + (target.width as i64 - outer_w) / 2,
            target.y as i64 + (target.height as i64 - outer_h) / 2,
        )
    };
    let x = wanted_x.clamp(
        target.x as i64,
        (target.x as i64).max(target.x as i64 + target.width as i64 - outer_w),
    );
    let y = wanted_y.clamp(
        target.y as i64,
        (target.y as i64).max(target.y as i64 + target.height as i64 - outer_h),
    );

    Some(Placement {
        width: width as u32,
        height: height as u32,
        x: x as i32,
        y: y as i32,
        maximized: saved.maximized,
    })
}

/// The geometry file's path inside a config dir.
pub fn path(config_dir: &Path) -> PathBuf {
    config_dir.join(FILE_NAME)
}

/// Read the saved geometry; `None` when there is no usable file — missing,
/// broken, or with an empty size.
pub fn load(config_dir: &Path) -> Option<SavedGeometry> {
    let text = std::fs::read_to_string(path(config_dir)).ok()?;
    let saved: SavedGeometry = serde_json::from_str(&text).ok()?;
    if saved.width == 0 || saved.height == 0 {
        return None;
    }
    Some(saved)
}

/// Write the geometry atomically (a `.tmp` file renamed over the real one).
/// Errors come back as `String`s for the saver thread to log.
pub fn save(config_dir: &Path, geometry: &SavedGeometry) -> Result<(), String> {
    std::fs::create_dir_all(config_dir)
        .map_err(|err| format!("could not create {}: {err}", config_dir.display()))?;
    let file = path(config_dir);
    let tmp = config_dir.join(format!("{FILE_NAME}.tmp"));
    let text = serde_json::to_string_pretty(geometry)
        .map_err(|err| format!("could not encode the window geometry: {err}"))?;
    std::fs::write(&tmp, text)
        .map_err(|err| format!("could not write {}: {err}", tmp.display()))?;
    std::fs::rename(&tmp, &file)
        .map_err(|err| format!("could not replace {}: {err}", file.display()))?;
    Ok(())
}

/// The window's geometry right now, or `None` when it cannot be read (a
/// minimised window's position is meaningless, so it never counts). While
/// the window is maximized, `last` is carried over with the `maximized` flag
/// flipped; with no `last`, the current bounds are recorded as maximized.
pub fn observe(window: &tauri::Window, last: Option<SavedGeometry>) -> Option<SavedGeometry> {
    if window.is_minimized().unwrap_or(false) {
        return None;
    }
    if window.is_maximized().unwrap_or(false) {
        return match last {
            Some(saved) => Some(SavedGeometry {
                maximized: true,
                ..saved
            }),
            None => Some(SavedGeometry {
                maximized: true,
                ..read_bounds(window)?
            }),
        };
    }
    Some(SavedGeometry {
        maximized: false,
        ..read_bounds(window)?
    })
}

/// The current outer position, inner size and outer size; any probe error
/// means no geometry this round.
fn read_bounds(window: &tauri::Window) -> Option<SavedGeometry> {
    let outer = window.outer_position().ok()?;
    let inner = window.inner_size().ok()?;
    let outer_size = window.outer_size().ok()?;
    Some(SavedGeometry {
        x: outer.x,
        y: outer.y,
        width: inner.width,
        height: inner.height,
        outer_width: outer_size.width,
        outer_height: outer_size.height,
        maximized: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One plain 1080p-monitor work area unless a test says otherwise.
    fn area() -> WorkArea {
        WorkArea {
            x: 0,
            y: 0,
            width: 1920,
            height: 1040,
            scale: 1.0,
        }
    }

    fn saved(x: i32, y: i32, width: u32, height: u32) -> SavedGeometry {
        SavedGeometry {
            x,
            y,
            width,
            height,
            outer_width: width + 16,
            outer_height: height + 39,
            maximized: false,
        }
    }

    #[test]
    fn on_screen_geometry_is_kept() {
        let placement = clamp_restored(&saved(100, 50, 1200, 800), &[area()]).unwrap();
        assert_eq!(
            placement,
            Placement {
                width: 1200,
                height: 800,
                x: 100,
                y: 50,
                maximized: false
            }
        );
    }

    #[test]
    fn off_screen_window_is_centred() {
        let placement = clamp_restored(&saved(5000, 5000, 1200, 800), &[area()]).unwrap();
        assert_eq!(
            placement,
            Placement {
                width: 1200,
                height: 800,
                x: 352,
                y: 100,
                maximized: false
            }
        );
    }

    #[test]
    fn tiny_window_grows_to_the_minimum() {
        let placement = clamp_restored(&saved(10, 10, 300, 200), &[area()]).unwrap();
        assert_eq!(
            placement,
            Placement {
                width: 800,
                height: 560,
                x: 10,
                y: 10,
                maximized: false
            }
        );
    }

    #[test]
    fn huge_window_shrinks_to_the_work_area() {
        let placement = clamp_restored(&saved(0, 0, 4000, 3000), &[area()]).unwrap();
        assert_eq!(
            placement,
            Placement {
                width: 1904,
                height: 1001,
                x: 0,
                y: 0,
                maximized: false
            }
        );
    }

    #[test]
    fn scale_raises_the_minimum() {
        let big = WorkArea {
            x: 0,
            y: 0,
            width: 3840,
            height: 2160,
            scale: 2.0,
        };
        let placement = clamp_restored(&saved(10, 10, 300, 200), &[big]).unwrap();
        assert_eq!(
            placement,
            Placement {
                width: 1600,
                height: 1120,
                x: 10,
                y: 10,
                maximized: false
            }
        );
    }

    #[test]
    fn barely_visible_counts_as_off_screen() {
        let placement = clamp_restored(&saved(1900, 100, 1200, 800), &[area()]).unwrap();
        assert_eq!(
            placement,
            Placement {
                width: 1200,
                height: 800,
                x: 352,
                y: 100,
                maximized: false
            }
        );
    }

    #[test]
    fn the_monitor_holding_most_of_the_window_wins() {
        let areas = [
            area(),
            WorkArea {
                x: 1920,
                y: 0,
                width: 2560,
                height: 1400,
                scale: 1.0,
            },
        ];
        let placement = clamp_restored(&saved(2000, 100, 1200, 800), &areas).unwrap();
        assert_eq!(
            placement,
            Placement {
                width: 1200,
                height: 800,
                x: 2000,
                y: 100,
                maximized: false
            }
        );
    }

    #[test]
    fn no_monitors_keeps_the_default() {
        assert!(clamp_restored(&saved(100, 50, 1200, 800), &[]).is_none());
    }

    #[test]
    fn maximized_survives() {
        let geometry = SavedGeometry {
            maximized: true,
            ..saved(100, 50, 1200, 800)
        };
        let placement = clamp_restored(&geometry, &[area()]).unwrap();
        assert_eq!(
            placement,
            Placement {
                width: 1200,
                height: 800,
                x: 100,
                y: 50,
                maximized: true
            }
        );
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let geometry = saved(100, 50, 1200, 800);
        save(dir.path(), &geometry).unwrap();
        assert_eq!(load(dir.path()), Some(geometry));
        assert!(path(dir.path()).ends_with(FILE_NAME));
        // The temporary file is gone: the rename left only the real one.
        assert!(!dir.path().join(format!("{FILE_NAME}.tmp")).exists());
    }

    #[test]
    fn load_ignores_missing_broken_and_empty_files() {
        let dir = tempfile::tempdir().unwrap();
        // No file at all yet.
        assert_eq!(load(dir.path()), None);
        // Broken JSON.
        std::fs::write(path(dir.path()), "{").unwrap();
        assert_eq!(load(dir.path()), None);
        // Valid JSON, but an empty size is meaningless.
        std::fs::write(
            path(dir.path()),
            r#"{"x":10,"y":10,"width":0,"height":800,"outer_width":16,"outer_height":839}"#,
        )
        .unwrap();
        assert_eq!(load(dir.path()), None);
    }
}
