## Verdict: FINDINGS (0 high, 2 low)

Review of bug plan 9d232076 — offscreen_browser_screenshot preview missing in chat (branch wt/agenticcoder, uncommitted diff + untracked .coding/plans/9d232076.md).

**The fix is correct.** `toolImagePaths` (frontend/src/components/chat/ToolImage.tsx:52) now gates on `name.endsWith("_screenshot")`, structured `data.path` is still preferred with the output-scan fallback intact (lines 53–56), the `image_*` branch is untouched, and null/failed results still resolve `[]` (line 33). No over-match: a full repo sweep of the tool registry shows exactly two live agent tool names end `_screenshot` — `offscreen_browser_screenshot` (src/tool/browser/mod.rs:218) and `browser_screenshot` (src/tool/browser/mod.rs:772). `browser_screenshot_latest` is a Tauri IPC command (src-tauri/src/ipc/browser.rs:75) and `webview_screenshot` a Rust method — neither ever reaches ToolCard as a tool-call name. `graph_context` confirms the only caller is Message.tsx ToolCard, so nothing else changes behavior. The gate matches the sibling convention (`browserResultInfo` screenshot branch, toolCardPaths.ts:516, keys on the same `endsWith("_screenshot")`). The regression tests genuinely exercise the changed path: both offscreen tests return `[]` under the old equality gate, so they fail pre-fix and pass post-fix (verified reasoning, not tautological — the fallback test's result carries no `data`, forcing the output-scan branch). The intentional old-behavior comment at ToolImage.test.ts:57 is the only remaining `game_screenshot` mention in live frontend code; all other mentions are .coding/ history (never rewritten). Module doc comment is accurate. Frontend-only, no platform-specific code, no new attack surface (paths still flow through the sandboxed `read_image_data_url` IPC; scan regex unchanged). Out of scope, as stated: .coding/safety.toml `npm run build` rule (pre-existing local addition) and the untracked plan file.

### Findings

**1. LOW — Docs sync: stale `game_screenshot` in README (README.md:66).**
The live feature bullet still reads "takes a screenshot (`browser_screenshot` / `game_screenshot`)". `game_screenshot` no longer exists (dropped in the 2026-08 rename) and this change is precisely about which screenshot tools render — the bullet is stale and now also incomplete. Per the project's documentation-sync review expectation this is an incomplete change.
*Fix:* change to "`browser_screenshot` / `offscreen_browser_screenshot`" (or "any `*_screenshot` tool") in README.md:66. .coding/browser-debugging.md:45/:88 already names both tools correctly; PLAN.md has no screenshot reference (verified — no impact there).

**2. LOW — Test strength: "prefer structured data.path" cannot prove preference (frontend/src/components/chat/ToolImage.test.ts:42–52).**
The result fixture embeds the *same* path in both `data.path` and the output text (`active-123.png`), so the assertion passes even if the output scan won — the "structured data preferred" behavior the test title claims is unobservable. Pre-existing shape, but this plan's check list explicitly calls for structured-preference, and it's a one-line strengthening.
*Fix:* make the paths differ, e.g. `data.path = ".coding/browser/screenshots/structured-123.png"` while output says `active-123.png`, and assert the returned path is the `data.path` value. (Keep the existing fallback test as-is; it already forces the scan branch by omitting `data`.)

Minor note, no action required: the updated module comment says both tools write `<page>-<ts>.png`; `browser_screenshot` actually writes the literal `browser-<ts>.png` (.coding/browser-debugging.md:45). "<page>" covers it loosely (page id "browser"), but if you touch the comment for any reason, `browser-<ts>.png` vs `<page>-<ts>.png` would be exact.

### What was verified
- Registry sweep: no non-screenshot tool name can match `endsWith("_screenshot")` today; future `*_screenshot` producers render by naming convention, documented in the module comment.
- Failed-call / null-result / non-image-tool paths unchanged and still tested (ToolImage.test.ts:72–77).
- image_* branch byte-identical (lines 34–50).
- Output-scan fallback regex unchanged (`.coding/browser/screenshots/[\w.-]+\.png`).
- Only callers of `toolImagePaths`: Message.tsx ToolCard + the test (graph_context) — no other consumer to break.
- vitest + tsc were reported green by the implementer (51 files / 703 tests; tsc clean); reviewer surface has no shell, so re-run the suite after applying the two fixes before commit.
- Multi-platform neutrality ✓ (pure TS), code style ✓ (conventions match toolCardPaths.ts family handling), no dead code, no unused imports, doc comments intact.
