## Verdict: FINDINGS (1 high, 2 low)

Review of ALL uncommitted changes on `wt/agenticcoding` (`git diff HEAD` + untracked) for plan 7956f41c — the `[ui] show_delegation_notes` display-layer filter for the search/search_read AUTO-DELEGATED note. The config plumbing chain, both render sites, tests, and docs are correct and complete **for the emission shape they cover** — but the backend has a second, more common emission shape the filter misses entirely (High 1).

---

## High 1 — The filter misses the fast-path delegation output (no `note: ` prefix) — the common case

**The backend emits delegated results in TWO shapes; the filter only handles one.**

- **Prepend path (handled):** a symbol hunt narrowed by a glob rides inside `merged_note` → `with_note` → output starts with `note: AUTO-DELEGATED …` (search.rs:1351/:1404, search_read.rs:259/:385/:441). `stripDelegationNotes` (delegationNotes.ts:23, `NOTE_PREFIX = "note: AUTO-DELEGATED"`) strips this line, and `isDelegationNote` skips the chip. ✓
- **Fast path (NOT handled):** when the delegation fully serves the query — a memory hunt, or a symbol hunt with **no glob** — both tools return the block RAW with **no `note: ` prefix**: `return ToolResult::success(block);` at **src/tool/agent/search.rs:1328-1330** and **src/tool/agent/search_read.rs:235-237**. The output starts directly with `AUTO-DELEGATED to the code graph — 'X' is an indexed symbol (re-issue this exact search to get the plain file search instead):` (memory twin: `AUTO-DELEGATED to memory — …`, search.rs:1166). The backend's own tests pin this shape: search.rs:2131/:2459/:2499/:2957/:2960 and search_read.rs:892 all assert `r.output.starts_with("AUTO-DELEGATED …")`.

**User impact:** for the typical delegated search (symbol-shaped pattern, no glob — the primary auto-delegation use case) and for ALL memory delegations, the expanded ToolCard `<pre>` still shows the full AUTO-DELEGATED header line — the exact steering line the user asked to hide, escape-hatch hint included — with the toggle OFF. The chip never fires there anyway (`searchResultInfo` returns null: no `note: ` prefix and no engine marker in the block), so the `<pre>` is the only surface and it is unfiltered. The toggle only works for the rarer glob-narrowed shape.

**Root cause:** the plan's exploration item (3) modeled all delegation output as `with_note`-composed (`note: {n}\n\n{body}`) — true only for the prepend arm; the fast-path arm was missed. The same incomplete model is baked into the new docs: the DECISION file ("Scope: line-level ONLY — lines starting with `note: AUTO-DELEGATED`") and echo-map exception 5 ("the steering line that `search`/`search_read` **prepend**"). The tests mirror the gap: delegationNotes.test.ts only exercises `note: `-prefixed fixtures.

**Fix:**
1. `stripDelegationNotes`: also strip lines starting with bare `AUTO-DELEGATED` (i.e. `line.startsWith("note: AUTO-DELEGATED") || line.startsWith("AUTO-DELEGATED")`). Safe: no other emission starts a line with `AUTO-DELEGATED` (matched content lines are `path:line: text`; staleness/reindex/fallback notes start with `note: `), and stripping the fast-path header leaves exactly the delegated answer (`  def:` / `  callers:` / `  full 360° view:` lines, or the memory hit lines).
2. Add regression tests with the bare fast-path shape for BOTH twins (code-graph block with no `note: ` prefix; memory block with no prefix) asserting the header goes and the answer lines stay — these fail on the current implementation.
3. Update the DECISION knowledge file + echo-map exception 5 to describe both shapes (the fast-path block is returned raw, not prepended).

## Low 1 — `.coding/safety.toml` widening rides in this commit

Three new shell `command_class` rules (`npx tsc;npx vitest`, `npx tsc`, `npx vitest`) are in the diff — a persistent safety-config change unrelated to the plan goal (presumably added so the verification commands could run). Benign in itself (same class as the existing `npm run build`), but it should be disclosed in the commit message or split out so the safety surface never widens silently.

## Low 2 — Previous plan's bookkeeping rides in this commit

`.coding/plans/dbe57a12.md` (regression-test line edit), untracked `.coding/knowledge/bug/dbe57a12.md` and `.coding/knowledge/spec/2027-01-04-inflightbar-counter-renders-on-the-live-estimate.md`, and the backlog.jsonl dbe57a12 done-flip belong to the already-landed reasoning-bar plan (commit 8dd2d0d). Committing them here is acceptable catch-up, but the commit message should mention it so the commit's scope is honest.

---

## Verified correct (the rest of the checklist)

- **Config plumbing chain — complete end-to-end, both fixture sides together.** general.rs (field + doc + `Default` false + defaults/round-trip/save-round-trip tests) → patch.rs (SettingsPatch + apply) → settings_dto.rs (SettingsSaveDto + apply) → ipc/settings.rs (GetSettingsUi + populate + all three test construction sites + wire assert) → contract_fixtures.rs ↔ dto-get-settings.json (changed together; the Rust contract test asserts the fixture) → tauri.ts (AppSettings.ui + SettingsSavePatch) → App.tsx hydration (`!!` guards an older backend's absent field → false → hidden, matching the default) → useAgentStore (state + setter + interface + default false, doc comments accurate) → types.ts (ChatDraft + SETTINGS_NAV "delegation notes" keyword) → ChatSection.tsx (draft init + store commit + save mapping + checkbox) → vitest.config.ts include list. Rust struct-literal completeness compile-checks every GetSettingsUi site; serde defaulting for old configs is proven by `show_delegation_notes_defaults_false` parsing empty TOML.
- **Filter logic for the shapes it covers.** Chip skip `info.note !== null && (showDelegationNotes || !isDelegationNote(info.note))` is correct boolean logic; `searchResultInfo` extracts the first line minus the `note: ` prefix, trimmed — exactly what `isDelegationNote`'s `startsWith("AUTO-DELEGATED")` expects. `stripDelegationNotes` is a no-op on note-free/empty output, only removes line-start matches (mid-line mentions survive — test-pinned), leaves the staleness/reindex/fallback notes and the def:/callers:/full 360° answer lines, and running it on non-search tools' output in the generic `<pre>` is harmless (no other tool emits the prefix). Merged-note edge case behaves as documented (staleness text appended after the full 360° line stays visible; its own fix is queued item 42302c2d).
- **Render-site completeness.** `searchResultInfo`'s only component caller is Message.tsx:755 (chip); the generic `<pre>` at Message.tsx:1034-1036 is the only raw-output surface — consistent with the corrected echo map (Output tab removed in commit 3ff4845 — verified via `git show`; the ToolCard is the only GUI surface for tool result text). Console REPL and Trace tab untouched, by design.
- **Constitution.** Doc comments on all new public functions/fields (TS + Rust); no platform-specific code (pure string ops); warning-free build per the reported green `cargo test` under `#![deny(warnings)]`; code style follows the show_tool_activity precedent exactly (verified field-by-field against the diff). I relied on the reported green runs (cargo test, tsc, vitest 865) plus my own code reading; I did not re-run the suites.
- **Documentation sync.** Echo-map claim #6 correction is factually right (commit 3ff4845 confirmed); README documents no chat toggles (no `show_tool_activity` there), so no README update is required; the toggle is discoverable via SETTINGS_NAV keywords (test-pinned). The DECISION file + exception 5 need the High-1 scope correction after the fix.
- **Test quality.** The Rust defaults/round-trip tests, the ChatSection source pins + serializeChat dirty case, and the ipc-contract field assert all genuinely discriminate. The delegationNotes pure-helper tests are well-formed — but none cover the bare fast-path shape (folded into High 1's fix).
- **Security.** Display-layer only: no backend/tool-result changes, no new attack surface, no injection risk (string filter on render).
