# Vendored tao 0.35.4 — 0.35.3 + PR #1215 backport

This directory is a pristine copy of `tao 0.35.3` (from the crates.io
registry cache) with **one** upstream fix backported and the version
renumbered to `0.35.4`.

## Why this exists

The app's recurring `AppHangB1` UI hang (2026-08-20, three byte-identical
watchdog stacks in `.coding/logs/hang-1787574592659.txt`,
`hang-1787575710423.txt`, `hang-1787586964715.txt`) is a self-deadlock in
tao 0.35.3's Windows keyboard path:

- `public_window_callback` takes the `KEY_EVENT_BUILDERS` lock, then calls
  `KeyEventBuilder::process_message`.
- `process_message` calls `PeekMessageW` **while the lock is held**.
- `PeekMessageW` dispatches inbound sent messages (most plausibly WebView2
  COM marshalling to the UI thread), which re-enters the window proc on the
  same thread.
- The re-entered callback re-locks the non-reentrant `parking_lot` mutex →
  permanent self-deadlock (0% CPU, `AppHangB1`).

Trigger: typing while the agent streams (the `is_keyboard_related` gate only
fires while typing).

## The fix

Upstream tao fixed this in 0.36.0 via **PR #1215** (changelog entry
`c704261c`, "Avoid Windows keyboard and IME deadlocks caused by re-entrant
message processing while input state locks are held"): the `PeekMessageW`
calls are hoisted **out of the locked regions** — `public_window_callback_inner`
peeks the next key message *before* taking `KEY_EVENT_BUILDERS.lock()` (and
before the IME `window_state` lock) and passes the peeked `MSG` /
`more_char_coming` flag into `process_message`.

We cannot upgrade to 0.36.0: `tauri-runtime-wry 2.11.4` (the latest
published) and `wry 0.55.1` both require `tao = "^0.35"`, and 0.36.0 bundles
unrelated breaking changes (window-subclassing removal #1231, keyboard
refactor #1238, Android JNI renames). So the fix is backported onto the exact
0.35.3 the app runs, renumbered `0.35.4` so `[patch.crates-io]` satisfies
the `^0.35` pin.

## What changed vs 0.35.3

- `src/platform_impl/windows/event_loop.rs`
  - `update_modifiers` uses the new free `get_agnostic_mods()` instead of
    locking `LAYOUT_CACHE` directly.
  - New helpers `peek_next_key_message`, `next_key_message_for_keyboard`,
    `more_ime_char_coming` (peek hoisted out of the locked regions).
  - `public_window_callback_inner` peeks **before**
    `KEY_EVENT_BUILDERS.lock()` (keyboard path) and before the
    `window_state.lock()` (IME path), passing the peeked data into
    `process_message`.
- `src/platform_impl/windows/keyboard.rs`
  - `KeyEventBuilder::process_message` drops the `hwnd` param and takes
    `next_key_message: Option<MSG>`; the three in-lock `PeekMessageW` calls
    (WM_KEYDOWN / WM_CHAR / WM_KEYUP) are gone.
  - `LAYOUT_CACHE` lock scopes narrowed: `get_kbd_state()` is called before
    the lock, and the lock is held only around `from_message` / `finalize`.
  - `PartialKeyEventInfo::from_message` takes `kbd_state` as a parameter.
- `src/platform_impl/windows/keyboard_layout.rs`
  - `get_agnostic_mods` is now a free function (locks `LAYOUT_CACHE` itself);
    the method is private.
- `src/platform_impl/windows/minimal_ime.rs`
  - `MinimalIme::process_message` drops the `hwnd`/`lparam` params and takes
    `more_char_coming: bool`; the in-lock `PeekMessageW` call is gone.

## Keeping this honest

`src-tauri/tests/tao_backport.rs` asserts the backport's source-level
invariants (no `PeekMessageW` in `keyboard.rs` / `minimal_ime.rs`, the peek
hoisted before `KEY_EVENT_BUILDERS.lock()`, version renumbered 0.35.4). It
fails on pristine 0.35.3 and passes on this tree, so a vendor refresh or a
botched merge cannot silently reintroduce the deadlock.

## Upstream references

- tao PR #1215: https://github.com/tauri-apps/tao/pull/1215
- tao changelog entry `c704261c` (0.36.0)
- `PeekMessageW` docs: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-peekmessagew
