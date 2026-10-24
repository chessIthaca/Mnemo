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
/// The window is declared in `mnemo2.conf.json`; the frontend is served by
/// Vite on port 5180 during development and embedded from
/// `target/mnemo2-dist` for a production build.
pub fn run() {
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
        .run(tauri::generate_context!("mnemo2.conf.json"))
        .expect("failed to run the Mnemo2 desktop host");
}
