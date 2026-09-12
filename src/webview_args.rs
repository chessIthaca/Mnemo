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
}
