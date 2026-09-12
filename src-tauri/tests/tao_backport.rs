// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Regression guard for the tao keyboard/IME self-deadlock (AppHangB1).
//!
//! The deadlock (2026-08-20, three byte-identical hang reports in
//! `.coding/logs/hang-1787574592659.txt`, `hang-1787575710423.txt`,
//! `hang-1787586964715.txt`): tao 0.35.3's `public_window_callback` takes the
//! `KEY_EVENT_BUILDERS` lock and then calls `KeyEventBuilder::process_message`,
//! which calls `PeekMessageW` *while the lock is held*. `PeekMessageW`
//! dispatches inbound sent messages (most plausibly WebView2 COM marshalling
//! to the UI thread), which re-enters the window proc on the same thread and
//! re-locks the non-reentrant `parking_lot` mutex → permanent self-deadlock,
//! 0% CPU, `AppHangB1`. Trigger: typing while the agent streams (the
//! `is_keyboard_related` gate only fires while typing).
//!
//! Fixed upstream in tao 0.36.0 by PR #1215 (changelog c704261c, "Avoid
//! Windows keyboard and IME deadlocks caused by re-entrant message processing
//! while input state locks are held"): the peek is hoisted OUT of the locked
//! region — `public_window_callback_inner` peeks the next key message BEFORE
//! taking `KEY_EVENT_BUILDERS.lock()` (and before the IME `window_state`
//! lock) and passes the peeked `MSG` / `more_char_coming` flag into
//! `process_message`, which no longer calls `PeekMessageW` at all.
//!
//! The deadlock itself is a third-party race — no deterministic runtime
//! reproduction exists. This test is the deterministic stand-in: it asserts
//! the backport's source-level invariants against the vendored tree
//! (`vendor/tao`, workspace root). It FAILS on the pristine 0.35.3 source
//! (the bug) and PASSES once the PR #1215 backport is applied (the fix), so
//! the defect can never silently reappear via a vendor refresh or a botched
//! merge.

use std::path::PathBuf;

/// The vendored tao crate root, resolved relative to this package
/// (`src-tauri`) so the test is hermetic and repo-relative.
fn vendor_tao_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("vendor")
        .join("tao")
}

fn read_vendored(rel: &str) -> String {
    let path = vendor_tao_dir().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read vendored tao file {}: {e}", path.display()))
}

/// The deadlock's core: `KeyEventBuilder::process_message` must never call
/// `PeekMessageW` — the peek must be hoisted out of the `KEY_EVENT_BUILDERS`
/// lock (PR #1215). Pristine 0.35.3 has three in-lock peeks here.
#[test]
fn key_event_builder_never_peeks_under_the_lock() {
    let keyboard = read_vendored("src/platform_impl/windows/keyboard.rs");
    assert!(
        !keyboard.contains("PeekMessageW"),
        "KeyEventBuilder::process_message must not call PeekMessageW — the peek must be \
         hoisted out of the KEY_EVENT_BUILDERS lock (tao PR #1215). In-lock PeekMessageW \
         dispatches inbound SEND messages, re-entering the window proc on the same thread \
         and self-deadlocking the non-reentrant parking_lot mutex (AppHangB1, 2026-08-20)."
    );
}

/// The IME twin: `MinimalIme::process_message` must take the
/// `more_char_coming` flag instead of peeking under the `window_state` lock.
#[test]
fn minimal_ime_never_peeks_under_the_lock() {
    let ime = read_vendored("src/platform_impl/windows/minimal_ime.rs");
    assert!(
        !ime.contains("PeekMessageW"),
        "MinimalIme::process_message must not call PeekMessageW — the peek must be hoisted \
         out of the IME window_state lock (tao PR #1215)."
    );
}

/// The hoist: `public_window_callback_inner` must peek the next key message
/// BEFORE taking the `KEY_EVENT_BUILDERS` lock. Asserted by byte offset so a
/// reordering (peek moved back under the lock) fails the test.
#[test]
fn event_loop_peeks_before_taking_the_key_event_builders_lock() {
    let event_loop = read_vendored("src/platform_impl/windows/event_loop.rs");
    let peek_call = event_loop
        .find("next_key_message_for_keyboard(window, msg, wparam)")
        .unwrap_or_else(|| {
            panic!(
                "public_window_callback_inner must call next_key_message_for_keyboard \
                 (the hoisted peek, tao PR #1215); missing in {}",
                vendor_tao_dir()
                    .join("src/platform_impl/windows/event_loop.rs")
                    .display()
            )
        });
    let lock = event_loop
        .find("KEY_EVENT_BUILDERS.lock()")
        .unwrap_or_else(|| {
            panic!(
                "KEY_EVENT_BUILDERS.lock() not found in {}",
                vendor_tao_dir()
                    .join("src/platform_impl/windows/event_loop.rs")
                    .display()
            )
        });
    assert!(
        peek_call < lock,
        "the next-key-message peek must happen BEFORE KEY_EVENT_BUILDERS.lock() — peeking \
         under the lock re-enters the window proc on the same thread and self-deadlocks \
         (tao PR #1215)."
    );
}

/// The renumber: the vendored crate must present itself as 0.35.4 so
/// `[patch.crates-io]` satisfies tauri-runtime-wry's `tao = "^0.35"` pin
/// (0.36.0 does not satisfy it, and tauri-runtime-wry 2.11.4 — the latest
/// published — still requires `^0.35`).
#[test]
fn vendored_tao_is_renumbered_0_35_4() {
    let manifest = read_vendored("Cargo.toml");
    assert!(
        manifest.contains("version = \"0.35.4\""),
        "vendored tao must be renumbered 0.35.4 (0.35.3 + PR #1215 backport) so the \
         [patch.crates-io] entry satisfies tauri-runtime-wry's ^0.35 requirement"
    );
}
