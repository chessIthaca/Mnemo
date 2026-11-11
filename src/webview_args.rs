// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Per-instance WebView2 profile helpers: the named-mutex arbitration and
//! the private per-pid user data folder that keep each Mnemo2 instance on
//! its own WebView2 user data folder.
//!
//! `src-tauri/src/webview_udf.rs::install_default_data_dir` uses these for
//! every Mnemo2 window: the first instance on the machine keeps WebView2's
//! default persistent profile while it holds the mutex named by
//! [`mutex_name_for`]; an additional instance records the private per-pid
//! folder [`secondary_webview_data_dir`] instead (via [`set_webview_data_dir`],
//! read back with [`webview_data_dir`]) so its WebView2 browser process
//! never collides with the in-use default profile. WebView2 bakes the
//! folder into its environment at first-creation time, so it must be
//! recorded before any webview is built.

/// The per-instance WebView2 user data folder, recorded exactly once at
/// startup by the app (`src-tauri/src/webview_udf.rs::install_default_data_dir`):
/// unset = "first instance, keep WebView2's default persistent profile",
/// `Some(dir)` = "additional instance, use this private per-pid profile".
/// WebView2 bakes the folder into its environment at first-creation time,
/// so this must be recorded before any webview is built.
static WEBVIEW_DATA_DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();

/// Record this instance's WebView2 user data folder (first call wins; only
/// the app calls it, once, at startup). Additional instances pass their
/// private per-pid folder here; the first instance never calls it.
pub fn set_webview_data_dir(dir: std::path::PathBuf) {
    let _ = WEBVIEW_DATA_DIR.set(Some(dir));
}

/// The per-instance WebView2 user data folder, when this instance is not the
/// first one on the machine (see [`set_webview_data_dir`]).
pub fn webview_data_dir() -> Option<&'static std::path::PathBuf> {
    WEBVIEW_DATA_DIR.get().and_then(|o| o.as_ref())
}

/// The private per-pid user data folder for additional instances:
/// `<app-local-data>/WebView2/mnemo-<pid>` — one profile per process, so a
/// second (or third …) mnemo instance gets its own WebView2 browser process
/// instead of colliding with the first instance's in-use default user data
/// folder (blank white window, fix 2027-01-13).
pub fn secondary_webview_data_dir(base: &std::path::Path, pid: u32) -> std::path::PathBuf {
    base.join("WebView2").join(format!("mnemo-{pid}"))
}

/// The Windows named-mutex name that arbitrates which instance may keep the
/// default profile: derived from the default user data folder, so builds of
/// different app identities never share the arbitration. Deterministic for
/// a given build (same input — same name on every instance).
pub fn mutex_name_for(default_udf: &std::path::Path) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    default_udf.as_os_str().hash(&mut hasher);
    format!("mnemo-webview2-udf-{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression (macOS CI leg, run 35521221353): the expected path used
    /// to be a hardcoded `\`-separated literal, which can never equal a
    /// `Path::join` result on a `/`-separator host — the production fn is
    /// a plain join (fine); the assertion was platform-shaped. Assert the
    /// structure (`<base>/WebView2/mnemo-<pid>`) instead, so the test
    /// holds on every host.
    #[test]
    fn secondary_udf_is_per_pid_under_webview2() {
        let base = std::path::Path::new(r"C:\Users\carst\AppData\Local\com.mnemo.desktop");
        let dir = secondary_webview_data_dir(base, 4242);
        // `Path::ends_with` compares components, and Windows parses both
        // separators as component boundaries, so the forward-slash literal
        // holds on every host.
        assert!(
            dir.ends_with(std::path::Path::new("WebView2/mnemo-4242")),
            "the folder is <base>/WebView2/mnemo-<pid>, got {dir:?}"
        );
        assert_eq!(
            dir.parent().and_then(|p| p.parent()),
            Some(base),
            "the folder sits directly under the app-local-data base"
        );
        // Distinct pids get distinct folders.
        assert_ne!(dir, secondary_webview_data_dir(base, 4243));
    }

    #[test]
    fn mutex_name_is_deterministic_and_path_scoped() {
        let a = mutex_name_for(std::path::Path::new(r"C:\Apps\mnemo\mnemo-app.exe.WebView2"));
        let b = mutex_name_for(std::path::Path::new(r"C:\Apps\mnemo\mnemo-app.exe.WebView2"));
        let c = mutex_name_for(std::path::Path::new(r"D:\Apps\other\mnemo-app.exe.WebView2"));
        assert_eq!(a, b, "same default UDF must yield the same mutex name");
        assert_ne!(a, c, "different default UDFs must not share arbitration");
        assert!(
            a.starts_with("mnemo-webview2-udf-"),
            "the name carries the app prefix"
        );
        assert!(
            !a.contains('\\') && !a.contains('/'),
            "the name must be a legal Windows mutex name (no path separators)"
        );
    }

    #[test]
    fn webview_data_dir_defaults_to_unset() {
        assert!(
            webview_data_dir().is_none(),
            "no instance is a secondary until the app records a folder"
        );
    }
}
