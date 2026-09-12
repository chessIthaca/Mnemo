## Verdict: FINDINGS (2 high, 1 low)

Review of ALL uncommitted changes on `wt/agenticcoding` for plan afa81f0a "Chat readability: thread line, prose cap, turn tint, hover timestamps (each a setting, default ON)" — `git diff HEAD` (~18 files: Rust config + IPC, frontend settings plumbing, transcript display restructure, tests, .coding bookkeeping) plus untracked files.

The display restructure, prose cap, stamping soundness, and test updates are all correct and well-executed. But the settings plumbing is missing its **save-side Rust layers**, so all four toggles silently fail to persist; and the hover-timestamp stamp misses the primary user-prompt creation path.

---

## Findings

### H1 (HIGH): The four chat-readability settings do not persist — the Rust save path is missing the fields

The read side is fully wired, but the **save side was never extended**, so every toggle reverts on the next app start.

Evidence chain:
- `ChatSection.tsx:96-99` sends `chat_thread_line` / `chat_prose_cap` / `chat_turn_tint` / `chat_hover_timestamps` in the `saveSettings` patch (pinned by `ChatSection.test.ts:66-69`); `tauri.ts:480-483` declares them in `SettingsSavePatch`; `saveSettings` (`tauri.ts:527-531`) invokes the `save_settings` command with `{ patch }`.
- The Rust `save_settings` command (`src-tauri/src/ipc/settings.rs:799-801`) deserializes the patch into **`SettingsSaveDto`** (`src/config/settings_dto.rs:173-236`). Its ui block ends at `show_knowledge_activity` (:207-213) — **no chat_* fields**. Serde (no `deny_unknown_fields`) silently drops the four unknown keys.
- `validate_and_apply_settings_patch` (`settings_dto.rs:507-521`) has apply blocks for `show_token_usage` / `show_tool_images` / `show_tool_activity` / `show_knowledge_activity` — none for the chat_* fields.
- The lib-side twin is missing them too: `SettingsPatch` (`src/config/patch.rs:565-590`, `show_knowledge_activity` at :586) and `apply_settings_patch` (show_* apply blocks at :465-477).

Net effect: toggle any of the four settings and click Save → the in-session store updates (the save handler runs the `setChat*` setters), the save "succeeds", but config.toml is written with the **unchanged** values; on next app start `get_settings` rehydrates from config.toml and the toggle silently reverts. All four settings are effectively read-only-from-config.

Why the green suites didn't catch it: `chat_readability_round_trips` (`general.rs:1039-1060`) tests config.toml parse/save directly, not the IPC save path; the frontend tests are source-contract pins (they pin that ChatSection *sends* the keys, not that the backend *accepts* them).

Root cause is in the plan itself: the exploration notes (`.coding/plans/afa81f0a.md:10`) list only the read-side layers (general.rs, GetSettingsUi, fixture, tauri.ts, store, App, ChatSection) — the save-side layers (`patch.rs`, `settings_dto.rs`) that the `show_knowledge_activity` precedent (plan bc09914f) explicitly included were missed.

**Fix:** add the four `Option<bool>` fields to `SettingsSaveDto` (after `show_knowledge_activity`, `settings_dto.rs:213`) + apply blocks in `validate_and_apply_settings_patch` (after :521), and mirror in `SettingsPatch` (`patch.rs:586`) + `apply_settings_patch` (:475 area). Add a regression test that fails without the fix — e.g. extend the `patch.rs` tests module (:592+) or the settings.rs wire tests to push a patch with the four fields through `validate_and_apply_settings_patch` and assert the UiConfig values change.

### H2 (HIGH): User prompt entries never get a hover timestamp — the stamp misses the optimistic push path

The design is "each transcript entry's creation time as a native hover tooltip", but the stamp lives **only** in `applyAgentEvent` (`agentEventReducer.ts:1404-1417`) — and the primary creation path for user prompts bypasses it.

Evidence:
- A regular typed prompt's user entry is created by the optimistic push in `InputBar.tsx:312-330` (direct `useAgentStore.setState`, entry literal at :321-325) — **never stamped**.
- The backend's `emit_prompt_dispatched` fires only for backlog dispatch (`backlog_cmds.rs:298`), run-all (`run_all.rs:920`), and spawned agents (`spawn.rs:484`) — **not** for regular typed prompts. So `reducePromptDispatched` (which does stamp, via applyAgentEvent) never runs for them.
- `pushTranscriptMessage` (`InputBar.tsx:158-171`, used by `handleSlashCommand` at :362) — transient system/assistant messages — also never stamped.

Net effect: the typed user prompt — the anchor entry of every turn and the most common entry kind — gets no hover tooltip, while the response/tools below it do. Backlog-dispatched prompts *do* get `ts` (via the reducer), an inconsistency. The reducer test (`useAgentStore.test.ts:1166`, `kind: "user"` + `ts: expect.any(Number)`) shows the intent clearly covers user entries.

**Fix:** stamp at the creation sites — add `ts: Date.now()` to the entry literal at `InputBar.tsx:321-325` and to the entry built in `pushTranscriptMessage` (`InputBar.tsx:158-171`). The reducer stamp stays as the safety net for event-born entries. (A tiny shared `stampEntry` helper would keep the three sites consistent.)

### L1 (LOW): Turn tint excludes the in-flight streaming response (transient cosmetic)

The streaming text block (`Conversation.tsx:216-221`) renders *outside* the turn groups, so during a long stream the current turn's tint band ends above the streaming text; when the text flushes into the transcript it lands *inside* the band — a subtle visual "jump" at finalize. The streaming text also has no hover-title wrapper (it is not an entry yet — no `ts` — acceptable). If the jump is deemed acceptable transient state, fine — but it should be a conscious decision; alternatively render the streaming block inside the last turn's group while that turn is active.

---

## Verified correct (review-focus walkthrough)

**1. Settings pattern fidelity** — every layer *except the save path* mirrors `show_tool_activity`/`show_knowledge_activity` exactly: `general.rs` fields (:343-356) with house-style doc comments, Default `true` (:378-381), `chat_readability_defaults_true` (:1027), `chat_readability_round_trips` (:1039), combined literal test extended (:1097-1143); `GetSettingsUi` fields (:551-560) + mapping (:755-758) + both test literals + JSON assertions (:1178-1181, :1226-1227); `contract_fixtures.rs:233-236` ↔ `dto-get-settings.json:57-60` — both all-`true`, consistent (the contract test asserts the fixture, so this pairing is required and correct); `tauri.ts` DTO (:415-424) + patch (:480-483); store fields + setters + defaults (`useAgentStore.ts:264-275, :632, :834`); App.tsx hydration `!== false` (:329-332, absent-field-reads-as-ON); ChatDraft + save + four toggle rows. The only missed layers are the save-side ones (H1).

**2. Display restructure correctness** — the filter predicate is EXACTLY the old gate (same three clauses, `Conversation.tsx:140-145`; the old ternary and the new filter are logically identical — verified clause by clause). Turn chunking is correct: user/steer start turns, pre-prompt entries form turn 0. `chunkRuns` (:30-41) produces maximal consecutive ACTIVITY runs; non-activity singletons render unwrapped. The run wrapper owns the level-2 indent consistently in both states (ON: `ml-[1.5em]` + 1px border + `pl-[0.5em]` ≈ 2em; OFF: `ml-[2em]`) — content sits at ~2em either way, and `Message.tsx` activity kinds render bare (tool :397, skill :424) with the ladder comment updated (:313-323). Index keys are the pre-existing pattern (append-only transcript → stable; `capTranscript` front-trim remounts the list, same as before the change — not a regression). `bg-bg-tertiary/30` and `border-slate-700/60` are valid classes (both palettes used widely in the project).

**3. Timestamp stamping soundness** — the identity-based stamp is safe: pure reducers return fresh objects, and every entry-reconstruction path spreads the previous entry (tool merge in `reduceToolCallStart`, arg-delta, tool result, memory finalize, vision described `agentEventReducer.ts:923-928`, `sweepRunningCards` :1191-1215), so `ts` is preserved through updates; `capTranscript` slices (references kept — no re-stamp); steer removal filters `steers`, not the transcript; restored entries keep their saved `ts`; legacy saves stay unstamped → no tooltip (graceful, as designed). The `ts?: number` intersection on the parenthesized union works with discriminated-union narrowing and construction. The only gap is the optimistic push path (H2).

**4. Prose cap** — `prose-p:max-w-[100ch] prose-li:max-w-[100ch] prose-blockquote:max-w-[100ch]` conditional on the final prose container (`Message.tsx:367-371`); `pre`/`code` stay uncapped (only `prose-pre:m-0 prose-pre:bg-bg-primary` — no max-w), so code blocks/diffs/tool outputs stay full width; the streaming text div is capped consistently (`Message.tsx:353-356`). `@tailwindcss/typography` is installed (the `prose-pre:` variants are pre-existing), so the element modifiers compile. Headings/tables uncapped — consistent with "prose elements only".

**5. Test quality** — the `ts: expect.any(Number)` wildcards are the right fix, not a weakening: `ts` is `Date.now()`-based and genuinely dynamic, while kind/text remain exactly pinned. The gate contract moved from the `) : null,` ternary pin to the filter-form pin with the same predicate. `Conversation.layout.test.ts`'s new "chat readability affordances" describe pins all four features (turn tint :74, prose cap :79-81, thread-line classes, hover-ts wrapper + fmtTs) and correctly moves the level-2 ladder contract to the run wrapper (`ml-[2em]` in conversationSource, `pl-[2em]` absent from messageSource). `ChatSection.test.ts` pins labels, config keys, setters, base literal, dirty-detection. `ipc-contract.test.ts` type assertions (:417-420) + store literals extended. What the suite *cannot* catch is the save-path break (H1) — worth closing with the regression test named in H1's fix.

**6. Constitution** — (a) Doc sync: all new public fields carry doc comments (UiConfig, GetSettingsUi, store fields); README/PLAN.md do not document `[ui]` toggles (verified — the precedent isn't documented there either), so code-level documentation is sufficient and consistent. (b) Multi-platform neutrality: `Date.now` / `toLocaleTimeString` / `toLocaleString` are platform-neutral; no Windows-only APIs, paths, or shell syntax anywhere in the change. (c) Warning-free: no unused imports remain (single `useAgentStore` import in `Message.tsx:14`; `TranscriptEntry` type import in Conversation.tsx is used; parent's `tsc --noEmit` clean + `deny(warnings)` cargo green corroborate). (d) The new backlog item (reasoning bar stats live updates) in `.coding/backlog.jsonl` is benign user-queued bookkeeping, out of scope as instructed; the untracked `.coding/knowledge/spec/2027-01-04-chat-transcript-fill-width-…md` is the *previous* plan's (6d51d819) knowledge record riding this commit — fine to commit as mergeable side-car content.

---

## Summary for the parent

Fix H1 (add the four fields + apply blocks to `SettingsSaveDto`/`validate_and_apply_settings_patch` and `SettingsPatch`/`apply_settings_patch`, with a save-path regression test) and H2 (stamp `ts: Date.now()` at the InputBar creation sites), decide on or accept L1, then re-run both cargo suites + frontend tests before committing.
