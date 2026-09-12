// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! App-level utilities — panic hook installation.
//!
//! The TUI's `AppState` / `TranscriptEntry` / `handle_agent_event` types were
//! removed when we swapped to Tauri. The frontend manages its own state via
//! Zustand. Only the panic hook remains here.

/// Install a panic hook that prints the panic before the process exits.
/// (The Tauri webview doesn't need terminal restoration, but we keep a
/// hook so panics in the brain surface cleanly.)
pub fn install_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("mnemo panic: {info}");
        original_hook(info);
    }));
}
