// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Construction of the `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` value, plus
//! the per-instance CDP port allocation behind it.
//!
//! The env var MUST be set before WebView2 is created (WebView2 reads it at
//! environment-creation time); `mnemo-app`'s `main` does so before the
//! Tauri builder. Centralized here (pure string assembly + a port probe) so
//! the flag combinations are unit-testable without spawning a webview.
//!
//! The CDP remote-debugging port is per-instance: [`DEFAULT_CDP_PORT`] when
//! free (a single instance keeps the documented endpoint), the next free
//! port when several mnemo instances run side by side — a second WebView2
//! environment cannot bind an occupied port (it fails silently, and the
//! second instance's `BrowserManager` would otherwise attach to the FIRST
//! instance's WebView2). The chosen port is recorded in a process-global
//! [`OnceLock`] so `BrowserManager` (constructed later, after the Tauri
//! builder) reads the same port the env var carries.

/// The default CDP remote-debugging port — used whenever it is free, so a
/// single instance keeps the documented `localhost:9222` endpoint.
pub const DEFAULT_CDP_PORT: u16 = 9222;

/// Build the `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` value.
///
/// - Extensions are always disabled (the app must not load Chromium
///   extensions).
/// - `cdp_port`: `Some(port)` appends `--remote-debugging-port=<port>`.
///   Opt-in because the port is unauthenticated — any local process could
///   run arbitrary JS / read the DOM of the app's webview. Debug builds and
///   builds with browser inspection enabled opt in (see `main.rs`), passing
///   the port picked by [`pick_cdp_port`].
/// - `disable_occlusion`: append
///   `--disable-features=CalculateNativeWinOcclusion` — an opt-in escape
///   hatch for RDP users (fix #4 of `.coding/reviews/2026-08-18-rdp-freeze-
///   diagnosis.md`): the renderer stays unthrottled while the window is
///   occluded, eliminating the buffered-events "catch-up" burst, at the cost
///   of background CPU/GPU while hidden. Off by default because the R2
///   delta-buffer cap already bounds the catch-up flush.
pub fn build_webview2_args(cdp_port: Option<u16>, disable_occlusion: bool) -> String {
    let mut args = String::from("--disable-extensions");
    if let Some(port) = cdp_port {
        args.push_str(" --remote-debugging-port=");
        args.push_str(&port.to_string());
    }
    if disable_occlusion {
        args.push_str(" --disable-features=CalculateNativeWinOcclusion");
    }
    args
}

/// Pick this instance's CDP remote-debugging port: [`DEFAULT_CDP_PORT`] when
/// free, then the next free port up to `DEFAULT_CDP_PORT + 9`, then any
/// ephemeral port. Probing binds a `TcpListener` on `127.0.0.1:<port>` and
/// drops it; WebView2 binds for real when its environment is created, so a
/// tiny probe-then-bind window remains (as with any such scheme — it takes
/// two instances launching within milliseconds of each other to hit it).
pub fn pick_cdp_port() -> u16 {
    for port in DEFAULT_CDP_PORT..DEFAULT_CDP_PORT + 10 {
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
    // All defaults taken (10+ instances, or other services on the range):
    // take any free ephemeral port.
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .map_or(DEFAULT_CDP_PORT, |addr| addr.port())
}

/// The CDP port chosen for this process. Set once from `main` (before the
/// Tauri builder, hence before any `BrowserManager` is constructed) right
/// after [`pick_cdp_port`]; read by `BrowserManager::new` so the manager
/// attaches to the same port the env var exposes. Mirrors the env var
/// itself: a process-global, set-before-use value.
///
/// Startup-only by construction — which is also why there is no round-trip
/// unit test: a test setting the `OnceLock` would poison parallel tests that
/// construct managers expecting the default.
static CDP_PORT: std::sync::OnceLock<u16> = std::sync::OnceLock::new();

/// Record the CDP port this instance uses (see [`CDP_PORT`]). Later calls
/// are ignored — the first port wins, matching the env var that was already
/// built from it.
pub fn set_cdp_port(port: u16) {
    let _ = CDP_PORT.set(port);
}

/// The CDP port for this process: the port picked at startup, or
/// [`DEFAULT_CDP_PORT`] when startup didn't pick one (CDP off, non-Windows,
/// tests).
pub fn cdp_port() -> u16 {
    CDP_PORT.get().copied().unwrap_or(DEFAULT_CDP_PORT)
}

/// Truthy env-flag parse: the variable is set and is not one of the falsy
/// spellings — empty, `"0"`, `"false"`, `"no"`, `"off"` (case-insensitive).
/// Anything else (`"1"`, `"true"`, `"yes"`, …) enables the flag.
pub fn env_flag_enabled(value: Option<std::ffi::OsString>) -> bool {
    match value {
        Some(v) => {
            let s = v.to_string_lossy().to_ascii_lowercase();
            !matches!(s.as_str(), "" | "0" | "false" | "no" | "off")
        }
        None => false,
    }
}

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

    #[test]
    fn args_all_combinations() {
        assert_eq!(build_webview2_args(None, false), "--disable-extensions");
        assert_eq!(
            build_webview2_args(Some(DEFAULT_CDP_PORT), false),
            "--disable-extensions --remote-debugging-port=9222"
        );
        assert_eq!(
            build_webview2_args(None, true),
            "--disable-extensions --disable-features=CalculateNativeWinOcclusion"
        );
        assert_eq!(
            build_webview2_args(Some(DEFAULT_CDP_PORT), true),
            "--disable-extensions --remote-debugging-port=9222 \
             --disable-features=CalculateNativeWinOcclusion"
        );
        // A dynamically picked port lands in the args verbatim.
        assert_eq!(
            build_webview2_args(Some(9227), false),
            "--disable-extensions --remote-debugging-port=9227"
        );
    }

    /// Regression (backlog 55ba23b1): when the default CDP port (9222) is
    /// already occupied — e.g. a second mnemo instance is running — the
    /// configured remote-debugging port must avoid it, so the second
    /// instance's WebView2 gets its own CDP endpoint instead of silently
    /// failing to bind (and its `ensure_webview` attaching to the FIRST
    /// instance's WebView2).
    #[test]
    fn cdp_port_avoids_an_occupied_default() {
        // Hold 9222 like another instance would. If the bind fails because
        // some other process already holds it, that is equivalent — the
        // point is that 9222 is not free either way.
        let _held = std::net::TcpListener::bind(("127.0.0.1", 9222)).ok();
        let port = pick_cdp_port();
        let args = build_webview2_args(Some(port), false);
        let parsed: u16 = args
            .rsplit_once("--remote-debugging-port=")
            .map(|(_, p)| p.parse().expect("port digits"))
            .expect("args carry the CDP port");
        assert_eq!(parsed, port, "the args carry the picked port verbatim");
        assert_ne!(
            port, 9222,
            "the CDP port must avoid an occupied 9222 (args: {args})"
        );
    }

    /// The picked port is actually usable: a listener can bind it right
    /// after the probe (the probe drops its listener, freeing the port for
    /// WebView2 to bind at environment-creation time).
    #[test]
    fn pick_cdp_port_returns_bindable_port() {
        let port = pick_cdp_port();
        assert_ne!(port, 0, "an ephemeral fallback must never return 0");
        // Retry briefly: a sibling test's concurrent probe may transiently
        // hold the same port (both skip an occupied 9222 and land on 9223).
        let mut bindable = false;
        for _ in 0..20 {
            if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
                bindable = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(bindable, "the picked port {port} must be bindable");
    }

    #[test]
    fn env_flag_requires_set_truthy_value() {
        assert!(!env_flag_enabled(None));
        assert!(!env_flag_enabled(Some(std::ffi::OsString::new())));
        assert!(!env_flag_enabled(Some(std::ffi::OsString::from("0"))));
        // Falsy words — a user writing =false / =no / =off must NOT silently
        // enable the flag (review F2, 2026-08-18).
        assert!(!env_flag_enabled(Some(std::ffi::OsString::from("false"))));
        assert!(!env_flag_enabled(Some(std::ffi::OsString::from("FALSE"))));
        assert!(!env_flag_enabled(Some(std::ffi::OsString::from("No"))));
        assert!(!env_flag_enabled(Some(std::ffi::OsString::from("OFF"))));
        assert!(env_flag_enabled(Some(std::ffi::OsString::from("1"))));
        assert!(env_flag_enabled(Some(std::ffi::OsString::from("yes"))));
    }

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
