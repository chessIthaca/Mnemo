## Verdict: FINDINGS (0 high, 1 low)

Round-2 verification of the three round-1 fixes for plan afa81f0a "Chat readability: thread line, prose cap, turn tint, hover timestamps (each a setting, default ON)" on `wt/agenticcoding` (all changes uncommitted, `git diff HEAD` + untracked). All three fixes are correctly and completely applied; the fix code itself introduced no new bugs. One residual gap of the same class as round-1 H2 remains in the feature (LOW): `qa` entries never receive a `ts`, so they never get a hover timestamp.

## Fix 1 (round-1 H1 — settings did not persist): VERIFIED CORRECT

- **Fields**: `SettingsSaveDto` (src/config/settings_dto.rs:172-244) derives `Default` and carries the four `Option<bool>` fields `chat_thread_line`, `chat_prose_cap`, `chat_turn_tint`, `chat_hover_timestamps` (:214-221), placed after `show_knowledge_activity`, each with `#[serde(default)]` — exactly the neighboring-field pattern.
- **Apply blocks**: `validate_and_apply_settings_patch` (:530-541) has four `if let Some(on) = patch.<field> { general.ui.<field> = on; }` blocks. Field names match the `UiConfig` fields in src/config/general.rs exactly; assignments target `general.ui.*`.
- **Live save path**: `save_settings` (src-tauri/src/ipc/settings.rs:799-808) deserializes the frontend payload into `SettingsSaveDto` and calls `validate_and_apply_settings_patch(&current, &patch)` — the four fields now ride this exact path. The lib-side twin is consistent: `SettingsPatch` (src/config/patch.rs:596-600) plus four apply blocks in `apply_settings_patch` (:478-489).
- **Regression test**: `chat_readability_flags_persist_through_the_save_patch` (settings_dto.rs:625-646) builds a `SettingsSaveDto` with the four `Some(false)`, calls `validate_and_apply_settings_patch(&current, &patch).unwrap()`, and asserts all four `general.ui.*` values flipped plus an untouched field keeping its default. Without the fields the test does not compile; without the apply blocks the assertions fail (defaults are `true`). It genuinely exercises the save patch path.
- **Repo style**: inline `#[cfg(test)] mod tests { use super::*; }` matches the general.rs/patch.rs precedent; no `#[allow]`; explanatory comment present.

## Fix 2 (round-1 H2 — typed prompts got no hover timestamp): VERIFIED CORRECT on both sites; one same-class gap remains (Finding 1)

- **Optimistic user entry**: InputBar.tsx:324-329 — `ts: Date.now()` in the entry literal, before the conditional `images` spread.
- **pushTranscriptMessage**: InputBar.tsx:160-174 — `const stamped = { ...msg, ts: msg.ts ?? Date.now() }` creates a fresh object (no mutation of the caller's `msg`), preserves an existing `ts`, and pushes `stamped`.
- **Type**: `TranscriptEntry` (frontend/src/lib/types.ts:311-323) is the parenthesized union intersected with `{ ts?: number }` — construction and narrowing both work (tsc clean).
- **No double-stamping / no shared-object mutation**: the reducer stamp (agentEventReducer.ts:1411-1417) only touches entries that lack `ts` (`e.ts === undefined`) and are absent from the pre-event transcript (`!old.has(e)`), so pre-stamped entries are never re-stamped and pre-event entries are never mutated. The stamped `result.agent` flows into the store through the uniform write path (agentEventReducer.ts:1474-1477) — the stamp happens pre-write.
- **All direct-push sites audited**: `capTranscript` is called at exactly three sites in InputBar.tsx — :169 (pushTranscriptMessage, stamped), :322 (optimistic user entry, stamped), :517 (`/load` restore — parsed entries keep their saved `ts`; legacy saves stay unstamped, by design). `flushStreamingText`/`pushTranscriptEntry` (agentState.ts:464-484) are reducer-internal and thus stamped via `applyAgentEvent` (confirmed by useAgentStore.test.ts:1165-1166 asserting `ts: expect.any(Number)` on interrupt-flushed entries).
- **Residual gap — see Finding 1**: the `qa` entry created by `recordQuestionAnswer` bypasses the stamp.

## Fix 3 (round-1 L1 — streaming response outside the tint band): VERIFIED CORRECT

- Conversation.tsx: `streamingBlock` extracted (:180-185), rendered inside the LAST turn's group at :227 (`{ti === turns.length - 1 && streamingBlock}`), with a standalone fallback at :197 (`{turns.length === 0 && streamingBlock}`); the old standalone block after the turns map is gone.
- **No double render**: the two guards are mutually exclusive (`turns.length === 0` vs the map, which renders nothing when empty) — `streamingBlock` renders at most once per render.
- **No jump at finalize**: the streaming text renders inside the last turn's div, which carries the tint class when `chatTurnTint && ti % 2 === 1`; when the stream flushes, the assistant entry joins the same turn group (it is not user/steer), so the band does not change. Message.tsx renders assistant streaming and finalized text with the same `pl-[1em]` indent, so there is no indent jump either.
- **Fallback coverage**: `turns.length === 0` with live `streamingText` is real (e.g., a spawned subagent streaming before any entry lands in its transcript) — covered.
- **Keys**: turn divs keyed by `ti`; chunk wrappers keyed by `ri`; non-activity `renderEntry` keyed by `ri`; inner run entries keyed by `i` — each chunk index is used exactly once per turn div, no collisions. `streamingBlock` is a conditional child, not a list item — no key needed.
- **Indent-ownership move is safe**: `Message` is rendered only from Conversation.tsx (the only `<Message` usages are Conversation.tsx:172/:181; other search hits are lucide icons and an unrelated `MessageRow` in LlmTraceView), so activity cards losing their in-component `pl-[2em]` in favor of the run wrapper's indent has no second surface. No `pl-[2em]` remains anywhere in frontend/src. The `ACTIVITY` set (Conversation.tsx:29-35) mirrors `isActivityEntry` (agentState.ts:417-424: tool/memory/vision/skill), with the mirror noted in a comment and pinned by Conversation.test.ts.

## Finding 1 (LOW) — `qa` entries never get a hover timestamp (same class as round-1 H2, rarer site)

`recordQuestionAnswer` (frontend/src/hooks/useAgentStore.ts:978-990) calls the pure reducer `reduceQuestionAnswered` (frontend/src/hooks/agentEventReducer.ts:991-1011) directly through `set()`, bypassing `applyAgentEvent`'s identity-based stamping. `reduceQuestionAnswered` creates a fresh `qa` entry via `pushTranscriptEntry` (agentEventReducer.ts:996-1000) with no `ts`, and it is never stamped later: any subsequent `applyAgentEvent` includes it in the pre-event `old` set, so the stamp skips it. Result: the collapsed question→answer record renders without a hover tooltip even with `chat_hover_timestamps` ON — the exact gap class round-1 H2 fixed for user prompts and transient messages, on the one remaining direct-push site.

Impact is cosmetic and rare (only ask_user question-answer flows; graceful absence, no crash, no wrong data), hence LOW.

Suggested fix (one line + test update): add `ts: Date.now()` to the `qa` entry literal in `reduceQuestionAnswered` (agentEventReducer.ts:996-1000), and update the pinned shape in useAgentStore.test.ts:704 (`expect(qa).toEqual({ kind: "qa", question: "Favorite color?", answer: "Blue" })`) to include `ts: expect.any(Number)`. Stamping inside the reducer is safe against double-stamping: the `applyAgentEvent` stamp only touches entries lacking `ts`.

## Full-diff sweep (beyond the three fixes)

- **Settings plumbing end-to-end**: App.tsx hydration uses `!== false` for all four (absent field reads as ON — older-backend tolerance); ChatSection reads the store draft, saves via `saveSettings` with snake_case keys, commits the setters; `serializeChat` is `JSON.stringify(d)` over the `ChatDraft` that now includes the four fields, so dirty detection works (covered by the "serializes differently when any chat pref differs" test toggling all four). Contract fixtures stay in sync: contract_fixtures.rs ↔ dto-get-settings.json both all-true; the settings.rs wire-test literals extended with matching JSON assertions. Store reset literals (useAgentStore.test.ts, ipc-contract.test.ts) extended.
- **Turn/tint stability**: turn boundaries are user/steer entries only; activity filtering never changes turn membership, so the tint alternation is stable under show_tool_activity / show_knowledge_activity toggles. `chunkRuns` builds maximal runs of consecutive visible activity entries — correct.
- **Multi-platform neutrality**: `Date.now()`, `toLocaleTimeString()`, `toLocaleString()`, `Set`/`filter`/`map` — platform-neutral; the Rust changes are pure config code with no OS-specific APIs or paths. No findings.
- **Documentation sync**: README.md and PLAN.md document no `[ui]` toggles (verified — the only matches for `show_tool_activity`/`chat_thread_line` in `*.md` are under `.coding/`), so the four new settings follow the established undocumented precedent of `show_tool_activity`/`show_knowledge_activity`; `endpoints.toml` examples are unrelated to chat settings. New public Rust fields carry doc comments (UiConfig, GetSettingsUi); the `SettingsSaveDto` fields match their neighbors' no-doc `#[serde(default)]` pattern. No doc findings.
- **Warning policy**: no `#[allow(...)]` anywhere in the diff; the parent's verification runs (root `cargo test` 1987+16, src-tauri 186+4, frontend vitest 853, `tsc --noEmit` clean) are green under `deny(warnings)`.

## Verification status

Fixes 1-3 verified correct and complete against the working tree (git diff HEAD + targeted file reads). Finding 1 is the only outstanding item; it does not regress anything the round-1 fixes delivered — it is the last uncovered direct-push site for the hover-timestamp feature.
