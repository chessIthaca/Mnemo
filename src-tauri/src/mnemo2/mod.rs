// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! Mnemo2's minimal Tauri host.
//!
//! Mnemo2 is an application built on the headless engine, not a second copy
//! of Mnemo1's IPC/tool surface. This module owns only the desktop shell and
//! the lead-session bridge.

/// The headless-engine-backed lead conversation.
pub mod runtime;

/// The real Phase 2 worker team runtime.
pub mod team;

/// The typed Tauri command surface for the lead conversation.
pub mod ipc;

/// A liveness command used by the frontend to distinguish the Mnemo2 host
/// from any other Tauri WebView (for example Mnemo1's Browser tab), which
/// also exposes `__TAURI_INTERNALS__`.
#[tauri::command]
fn mnemo2_host_ping() -> &'static str {
    "pong"
}

/// Build and run the Mnemo2 desktop host.
///
/// The window is declared in `mnemo2-app/tauri.conf.json`; the frontend is
/// served by Vite on port 5180 during development and embedded from
/// `target/mnemo2-dist` for a production build.
pub fn run() {
    // Mnemo1 exports WebView2 browser arguments (including CDP port 9222) into
    // the environment inherited by tools it launches. Mnemo2 is a separate
    // desktop app and must not share that debug port or its WebView2 command
    // line, so drop inherited arguments before Tauri creates a webview.
    std::env::remove_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS");
    mnemo::app::install_panic_hook();
    tauri::Builder::default()
        .manage(ipc::Mnemo2State::default())
        .invoke_handler(tauri::generate_handler![
            mnemo2_host_ping,
            ipc::lead_open,
            ipc::lead_snapshot,
            ipc::lead_prompt,
            ipc::lead_interrupt,
            ipc::lead_set_model,
            ipc::lead_approve,
            ipc::lead_answer,
            ipc::lead_close,
        ])
        .run(tauri::generate_context!("mnemo2-app/tauri.conf.json"))
        .expect("failed to run the Mnemo2 desktop host");
}

#[cfg(test)]
mod tests {
    /// The native shell must compile with Mnemo2's own Tauri identity. Sharing
    /// Mnemo1's identity also shares its WebView2 user-data folder, which
    /// deadlocks the second process before its webview initializes.
    #[test]
    fn context_uses_mnemo2_identifier() {
        // Tauri loads `tauri.conf.json` from the path's parent directory; the
        // path must point at the Mnemo2 config directory, not a custom filename.
        let context: tauri::Context<tauri::Wry> =
            tauri::generate_context!("mnemo2-app/tauri.conf.json", test = true);
        assert_eq!(context.config().identifier, "com.mnemo.mnemo2");
    }
}
