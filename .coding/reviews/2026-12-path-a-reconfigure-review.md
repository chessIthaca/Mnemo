# Review: Path (a) — bare-window + add_child reconfigure

**Date:** 2026-12
**Scope:** All uncommitted changes (`git diff HEAD`): `src-tauri/tauri.conf.json`, `src-tauri/src/ipc/state.rs`, `src-tauri/src/main.rs` (plus unrelated `.coding/` plan/bookkeeping files).
**Goal:** Reconfigure the window model from `WebviewWindowBuilder` (auto-created by tauri.conf.json) to a bare `WindowBuilder` + an `add_child` agent-chat webview, for renderer crash isolation. The game-browser child stays lazy (browser_webview.rs, unchanged).

---

## Findings

### HIGH — Unused `PhysicalSize` import breaks the src-tauri build under `#![deny(warnings)]`

**File:** `src-tauri/src/main.rs:27-29`

`PhysicalSize` is imported but never used by name anywhere in `main.rs` (a literal search for `PhysicalSize` returns exactly one hit — the import line itself). The `add_child` call passes `window.inner_size()?` as its size argument, and `Window::inner_size()` in Tauri 2 returns `Result<PhysicalSize<u32>>`; the `?` unwraps the `Result` and the `PhysicalSize` type is **inferred** — it is never spelled out. So the named import is dead.

`src-tauri/src/main.rs:7` has `#![deny(warnings)]`, which promotes `unused_imports` to a hard error. **The src-tauri package does not compile.**

**Why the user's `cargo test` (940 passed) did not catch this:** the root `Cargo.toml` is a package (`myharness`) that is also a workspace root with `members = ["src-tauri"]` and **no `default-members`**. Per Cargo's default-member rule, bare `cargo test` operates on the root package only — it does **not** compile `src-tauri`. This matches the project's own documented convention (plan 64ab0050 step 4: "Run `cargo test --workspace` unpiped (NOT bare `cargo test` — src-tauri is a separate package)"). The 940 tests are the `myharness` lib tests; src-tauri was never built.

**Fix:** remove `PhysicalSize` from the import list:

```rust
use tauri::{
    Emitter, Manager, PhysicalPosition, WebviewBuilder, WebviewUrl, WindowBuilder,
};
```

Then verify with `cargo build -p myharness-app` (or `cargo test --workspace`) — the definitive check that bare `cargo test` skips.

---

### LOW — `--disable-extensions` hardening flag lost in default release builds

**Files:** `src-tauri/tauri.conf.json` (window removed), `src-tauri/src/main.rs:62-67` (env var, unchanged).

The old `tauri.conf.json` window carried `"additionalBrowserArgs": "--disable-extensions"` **unconditionally** (all builds). The reconfigure sets `windows: []`, so that config is gone. The only remaining source of WebView2 browser args is the process-global env var set in `main`:

```rust
if cfg!(debug_assertions) || browser_inspection_enabled() {
    std::env::set_var(
        "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
        "--disable-extensions --remote-debugging-port=9222",
    );
}
```

This is set only in debug builds or release builds with inspection opted in. In the **default release build (inspection off)**, the env var is never set, so `--disable-extensions` is no longer applied to any WebView2 — a regression from the prior always-on behavior. WebView2 does not load Chrome-Web-Store extensions by default (it is an embedded control), so the practical impact is likely nil, but the hardening flag was deliberately present before.

**Fix:** set the env var unconditionally with `--disable-extensions`, and append the CDP port only when inspection is enabled:

```rust
let mut args = "--disable-extensions".to_string();
if cfg!(debug_assertions) || browser_inspection_enabled() {
    args.push_str(" --remote-debugging-port=9222");
}
std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", args);
```

---

## Verified clean (addresses each review question)

- **Bare-window + add_child model** — correct. `WindowBuilder::new(app, "main")` builds a webview-less window; `window.add_child(WebviewBuilder::new("agent-chat", WebviewUrl::App("index.html".into())), ...)` adds the React UI as a child. The game-browser child path in `browser_webview.rs:134-151` is unchanged and still resolves the window via `app.get_window("main")`, which works because the bare window keeps the label `"main"`.
- **`window.inner_size()` Result handling** — correct. `window.inner_size()?` (main.rs:107) propagates the `Result` via `?` from the `setup` closure. The initial child size matches the window's inner size.
- **`on_window_event` closure `Send + Sync`** — satisfied. The closure captures `resize_handle: Arc<std::sync::Mutex<Option<Webview>>>` by move. `Arc<T>: Send+Sync` when `T: Send+Sync`; `std::sync::Mutex<U>: Send+Sync` when `U: Send`; `Option<Webview>: Send` because `Webview` is `Send + Sync` (documented in `browser_webview.rs:22-23`). The closure borrows `&event` and performs only lock/unlock + an infallible `set_size` — no captured mutable state. (Confirmed compiles.)
- **`std::sync::Mutex` deadlock in the resize handler** — no deadlock. The only other locker of the same `Arc` is the `RunEvent::Exit` handler (main.rs:572), which holds it for a single statement (`*h = None`); a resize firing during exit would simply fail `if let Ok(h) = resize_handle.lock()` and skip — no blocking. Reentrancy: `wv.set_size` resizes the **child** webview, not the parent window, so it does not re-enter the parent's `WindowEvent::Resized` handler (the parent's `WM_SIZE` fires only on parent resize). `std::sync::Mutex` (not reentrant) is therefore safe here. The `std::sync::Mutex` choice is correct because the resize closure is sync (cannot `.await`).
- **Handle shared across all 3 `IpcState` branches** — correct. `agent_chat_handle.clone()` is passed to the `Ready` (main.rs:275), `NeedsProject` (main.rs:369), and `Err` (main.rs:435) branches — all share one `Arc`. The original `agent_chat_handle` and the `resize_handle` clone keep the refcount alive through setup; afterward the `Arc` is held by `IpcState` + the resize closure, so the webview is not dropped prematurely.
- **Exit cleanup (no HWND leak)** — correct. The `RunEvent::Exit` handler (main.rs:568-574) clones the `Arc`, locks, and sets `*h = None`, dropping the `Webview` handle so wry tears down the child HWND. This mirrors the game-browser `bw.take_webview()` cleanup above it. Ordering (game-browser then agent-chat) is harmless.
- **`enable_browser_inspection` gate intact** — yes. The gate is at main.rs:62 (`cfg!(debug_assertions) || browser_inspection_enabled()`) for the env var, and main.rs:912 (`!cfg!(debug_assertions) && config.general.general.enable_browser_inspection`) for `browser.set_webview_enabled`. Neither is touched by this diff. Release builds expose CDP only on explicit opt-in; debug builds always do. (Per the user's instruction, the accepted tradeoff that both children share one WebView2 env / both appear on CDP 9222 is **not** flagged.)
- **Capabilities scoping for the new "agent-chat" webview** — sufficient. `capabilities/default.json` scopes to `windows: ["main"]`. Per the Tauri 2 ACL schema in the project's own `gen/schemas/desktop-schema.json`: *"If a webview or its window is not matching any capability then it has no access to the IPC layer at all."* The "agent-chat" child belongs to window "main", so it inherits the capability — `invoke`/`listen` work. Consistent with spike 914dfb46's GO verdict ("Tauri IPC works in add_child webview"). No config change required.
- **Constitution (doc comments / dead code / style)** — the new public field `agent_chat_webview` on `IpcState` has a doc comment (state.rs:68-74). No new public functions. No `#[allow(...)]` suppressions. The field is used (resize handler + exit cleanup), so no dead code. The pattern mirrors the existing `browser_webview` field and exit-cleanup block. The only constitution violation is the unused import above.

---

## Summary

Two findings, both in the src-tauri package (which bare `cargo test` does not compile):

1. **HIGH (build break):** unused `PhysicalSize` import — remove it from `main.rs:27-29`.
2. **LOW (hardening regression):** `--disable-extensions` lost in default release builds — set the env var unconditionally and append the CDP port conditionally.

Everything else (the bare-window + add_child model, `inner_size()` Result handling, closure `Send+Sync`, mutex deadlock freedom, handle sharing, exit cleanup, the inspection gate, capabilities scoping, doc comments) is correct.
