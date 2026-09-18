// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Per-instance WebView2 user data folder.
//!
//! Every webview in mnemo is built as a plain `WebviewBuilder::new(...)`
//! with no data directory, so WebView2 resolves the SAME default user data
//! folder (`%LOCALAPPDATA%\com.mnemo.desktop\EBWebView`) for every process. One
//! user data folder supports one live browser process, so a second mnemo
//! instance's session never initializes — its window comes up as a blank
//! white frame (user report 2027-01-13, evidence
//! `.coding/analysis/second-instance-before.png`).
//!
//! `install_default_data_dir` arbitrates at startup with a named mutex
//! derived from the default UDF: the FIRST instance keeps the persistent
//! default profile; every ADDITIONAL instance gets its own private per-pid
//! folder (`<app-local-data>\WebView2\mnemo-<pid>`) — and therefore its own
//! browser process. `apply` wraps every webview builder so the private
//! folder is baked into the environment at creation time (WebView2 reads the
//! folder when the environment is created, so the builder data_directory
//! must be in place before the first webview exists).
//!
//! Windows-only by construction: the named mutex + per-pid profile solve the
//! Windows multi-instance WebView2 collision. On other platforms the module
//! is a no-op — every builder passes through unchanged.

use mnemo::webview_args;
use tauri::webview::WebviewBuilder;

/// Decide this instance's WebView2 user data folder. MUST run before the
/// first webview is built (the folder is baked into the WebView2 environment
/// at creation). Best-effort: any failure degrades to "first instance,
/// default profile" — the app must never fail to start over this.
pub fn install_default_data_dir(_app: &tauri::AppHandle, _pid: u32) {
    #[cfg(windows)]
    {
        // The WebView2 default UDF lives under `%LOCALAPPDATA%\<identifier>`
        // (the app's local-data dir — the observed `...\EBWebView`), so the
        // private per-pid profiles sit next to the persistent profile.
        let Some(base) = std::env::var_os("LOCALAPPDATA")
            .map(|la| std::path::PathBuf::from(la).join(_app.config().identifier.clone()))
        else {
            return;
        };

        let default_udf = base.join("EBWebView");
        let another_instance_owns_default =
            acquire_default_profile_mutex(&webview_args::mutex_name_for(&default_udf));

        if another_instance_owns_default {
            webview_args::set_webview_data_dir(webview_args::secondary_webview_data_dir(
                &base, _pid,
            ));
        }
    }
}

/// Wrap any webview builder: on additional instances the builder gets the
/// per-pid data directory; the first instance (and every non-Windows
/// platform) passes through unchanged. Call for EVERY webview, so all of an
/// instance's webviews share one profile.
pub fn apply(builder: WebviewBuilder<tauri::Wry>, _label: &str) -> WebviewBuilder<tauri::Wry> {
    if let Some(dir) = webview_args::webview_data_dir() {
        builder.data_directory(dir.clone())
    } else {
        builder
    }
}

/// Take the named-mutex arbitration for the default profile. Returns true
/// when ANOTHER live instance already holds it (this instance must move to
/// its own per-pid folder). The handle is deliberately kept open for the
/// process lifetime (leaked): the mutex OBJECT exists only while a handle
/// to it is open, and the open handle is what later instances observe.
#[cfg(windows)]
fn acquire_default_profile_mutex(name: &str) -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` outlives the call; null security attributes = default
    // ACL; bInitialOwner = 0 — the mutex is a presence marker, not a lock.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
    if handle.is_null() {
        // CreateMutex failed (e.g. an exotic ACL) — degrade to the default
        // profile rather than crash; the app's behavior is unchanged.
        return false;
    }
    let already_taken = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    // Hold the handle for the process lifetime: the mutex OBJECT exists
    // only while a handle to it is open, and the open handle is what later
    // instances observe. `Box::leak` is the deliberate keep-alive (the
    // `mem::forget` on the Copy handle would be a no-op).
    let _ = Box::leak(Box::new(handle));
    already_taken
}

#[cfg(all(test, windows))]
mod tests {
    /// The named-mutex round trip: the first holder keeps the default
    /// profile, the second holder must move to its per-pid folder.
    /// Unique name per pid so parallel runs never interfere.
    #[test]
    fn default_profile_mutex_reports_second_holder() {
        let name = format!("mnemo-webview2-udf-test-{}", std::process::id());
        let first = super::acquire_default_profile_mutex(&name);
        assert!(!first, "first holder is the primary (default profile)");
        let second = super::acquire_default_profile_mutex(&name);
        assert!(
            second,
            "second holder must move to its per-pid folder"
        );
    }
}
