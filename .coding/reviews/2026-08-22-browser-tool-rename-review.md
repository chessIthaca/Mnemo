# Review: rename browser* → offscreen_browser_* and game_* → browser_*

**Branch:** `fix/browser-tool-rename` · **Date:** 2026-08-22
**Scope reviewed:** ALL uncommitted changes (`git diff HEAD`): `src/tool/browser/mod.rs`, `src/agent/factory.rs`, `src/browser/mod.rs`, `README.md`, `.coding/browser-debugging.md`, `.coding/backlog.json`, `.coding/plans/stack.json` (+ new plan file). Independent tree-wide searches for leftover old names, double-prefixes, old struct names, and missed call sites.

## Verdict

The 16-tool rename inside `src/tool/browser/mod.rs` + `src/agent/factory.rs` is **correct and complete**: every `fn name()` matches its `ToolSchema::new` name, error strings/doc comments/description cross-references were updated to the correct new namespace (headless → `offscreen_browser_*`, live → `browser_*`), the two-pass rename order left no double-prefixes (`offscreen_offscreen` / `browser_browser` / `offscreen_browser_game` / `browser_offscreen` — zero hits), no old struct names remain in live code, and the parity test lists exactly the 10 offscreen + 6 live (cfg(windows)) tools that `register_browser_tools` registers, with a count assertion.

**However, the rename is incomplete outside the tool module:** 4 user-facing strings and ~19 doc comments in live (non-`.coding/`) files still say `game_*` — and two of them also say `browser_*` where they now mean `offscreen_browser_*`. Findings below.

---

## Findings

### Critical
None.

### High
None.

### Medium

**M1 — User-facing strings still reference `game_*` (and one doubly-stale `browser_*`).**
These are rendered to users, not just internal docs:

1. `src-tauri/src/ipc/browser_webview.rs:59-62` — `UNSUPPORTED_BROWSER_TAB_MSG` (shown as the Browser tab's red panel on non-Windows and returned by `browser_webview_ensure`) says: *"…the agent's game_\* inspection tools are not registered; the headless browser_\* agent tools remain available."* Both halves are now wrong: `game_*` no longer exists (→ `browser_*`) and the headless tools are now `offscreen_browser_*`.
2. `src-tauri/src/ipc/browser_webview.rs:608` — test `ensure_platform_gate_both_branches` asserts `err.message.contains("game_*")`, locking in the stale message. Must be updated together with the constant (the change is still test-covered via the `assert_eq!` on the constant at :609).
3. `frontend/src/components/settings/sections/AdvancedSection.tsx:469` — rendered JSX in the Settings → Advanced section: *"`<code>game_*</code>` tools can inspect the Browser tab."*
4. `frontend/src/components/views/BrowserView.tsx:124` — rendered JSX in the Browser tab placeholder: *"the agent's `<code>game_*</code>` tools attach to it through its…"*.

Fix: `game_*` → `browser_*` in all four; in M1.1 also `browser_* agent tools` → `offscreen_browser_* agent tools`.

**M2 — Stale `game_*` doc comments in live source (documentation sync, constitution).**
~16 comment sites still name the removed namespace:

- `src/browser/mod.rs:753` ("the agent's `game_*` tools must drive"), `:865` ("would let game_* operate on the app's UI"), `:1542` ("would make `game_*` silently operate on the app's own UI"), `:2330` ("game_* mutations must never operate on the app's own UI")
- `src/config/general.rs:103` (`enable_browser_inspection` field doc: "so the agent's `game_*` tools can attach")
- `src-tauri/src/ipc/browser_webview.rs:12, :16, :20, :64, :66` (module + `child_webview_supported` docs)
- `src-tauri/src/ipc/settings.rs:478, :918`
- `frontend/src/lib/tauri.ts:367, :440` (`enable_browser_inspection` docs, both interfaces)
- `frontend/src/components/views/BrowserView.tsx:20, :39, :165` (component doc comments)

Additionally `src-tauri/src/ipc/browser.rs:9-11` is stale **both ways**: "the agent drives it via the `game_*` tools" (→ `browser_*`) and "the agent's `browser_*` tools call `BrowserManager` directly" (those headless tools are now `offscreen_browser_*`).

Fix: `game_*` → `browser_*` everywhere above; the two headless references (`browser.rs:10`, and M1.1) → `offscreen_browser_*`.

### Low

**L1 — Article grammar broken by the rename.** `src/tool/browser/mod.rs:1051` (vision-test doc comment): *"a `offscreen_browser_screenshot` PNG path must feed straight…"* — "a" → "an". (Was "a `browser_screenshot`" before the rename.) Several other doc-comment lines also now exceed their previous wrap width (e.g. :54-55, :188-189, :559-560, :912-913); cosmetic only, no lint enforces it.

**L2 — Informational: `game`/`game-browser` concept prose.** `src-tauri/src/main.rs:135, :178, :180, :191` still describe the child webview as the thing "the human plays the game in" / "game-browser child". Not tool names, so outside the strict rename scope, but the updated docs (`.coding/browser-debugging.md`) have moved to "Browser tab" language — consider aligning while nearby.

**L3 — Informational: namespace overlap to be aware of (no action required).** The live agent tools now share the `browser_*` prefix with the pre-existing Tauri IPC commands (`browser_pages`, `browser_console`, `browser_open`, `browser_screenshot_latest`, `browser_normalize_url`, `browser_webview_*`). These are disjoint namespaces (agent-tool registry vs `invoke()` commands) so there is no functional collision or shadowing — verified no IPC command was renamed (the diff touches no `src-tauri/` or `frontend/` files). Noting only because `browser_console` now exists as *both* an IPC command (headless console, `tauri.ts:1369`) and conceptually near agent tool names.

---

## Areas verified clean (no findings)

- **Rename mechanics:** all 16 `fn name()` ↔ `ToolSchema::new` first-arg pairs match; error strings (`"offscreen_browser_navigate failed: {e}"` etc.) updated; zero double-prefixes tree-wide; zero leftover quoted old headless names (`"browser_navigate"` etc. as quoted strings survive only as the 6 *live* tools' new names + the parity test + the intentional `browser_console` IPC command); zero old struct names (`NavigateTool`, `GameScreenshotTool`, …) in live code — only `.coding/` history, as deliberately scoped.
- **Cross-references inside descriptions:** `OffscreenClickTool` → `offscreen_browser_snapshot`; `OffscreenSwitchPageTool` → `offscreen_browser_list_pages`; `BrowserClickTool` → `browser_snapshot`/`browser_screenshot`; live-tool doc comments → `offscreen_browser_navigate` allow-list / `offscreen_browser_eval`. All point at the correct NEW namespace.
- **Parity test** (`src/agent/factory.rs:1349-1374`): exactly the 10 `offscreen_browser_*` + 6 `browser_*` under `#[cfg(windows)]`, matching the registered structs 1:1, with a registry-count assertion; the `#[cfg(windows)]` gate on both registration and expectation is preserved (multi-platform neutrality ✓).
- **Safety levels unchanged:** the `safety_rules` test asserts the same AutoRun/NeedsApproval split with the renamed structs; no `safety()` override changed. The `browser_eval` (live) security doc comment (arbitrary JS in the app's own frame, not confined to the scheme allow-list) is intact and now correctly cites `offscreen_browser_navigate`.
- **Screenshot filename prefix:** `game-<ts>.png` → `browser-<ts>.png` (`src/tool/browser/mod.rs:754`) is consistent with both updated tables in `.coding/browser-debugging.md` (`browser-<ts>.png` live, `<page>-<ts>.png` headless); no other code consumes the old prefix.
- **Docs:** `README.md` names both namespaces; `.coding/browser-debugging.md` tables/narrative/notes fully converted (spot-checked all ~15 name sites); `PLAN.md` contains zero references to these tool names (searched) — not stale; `.coding/skills/*.toml` clean (category-based, no `browser` hits); approval rules are `ToolCategory::Browser`-based — no tool-name references anywhere.
- **Constitution:** all pub structs keep doc comments; no `#[allow]` added; no IPC command renamed; `.coding/` history untouched as scoped. (Tests not run — reviewer is read-only; the change is mechanical and the parity + safety tests were updated in the diff.)
