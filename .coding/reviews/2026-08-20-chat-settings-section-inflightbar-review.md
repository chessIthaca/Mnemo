# Review — Chat settings section move + InflightBar empty-reasoning fixes

**Date:** 2026-08-20 · **Scope:** all uncommitted changes (`git status` / `git diff HEAD`), excluding `.coding/**` bookkeeping.
**Plan intent:** (1) move the chat-display toggles (`showTokenUsage` / `showMemoryActivity`) out of Settings → Appearance into a new Settings → Chat section, persistence unchanged; (2) fix two InflightBar reasoning-panel bugs (missing chevron + inert bar while the reasoning log is empty; "—" artifact in the expanded-but-empty panel).

**Changed files reviewed:** `frontend/src/components/settings/types.ts`, `sections/ChatSection.tsx` (new, untracked — read directly), `sections/ChatSection.test.ts` (new, untracked), `sections/AppearanceSection.tsx`, `SettingsDialog.tsx`, `chat/InflightBar.tsx`, `chat/InflightBar.test.ts`, `chat/Message.tsx`.

## Verdict

**No correctness, bug, or security findings.** One **minor constitution-compliance finding** (line endings). The two defects fixed by this batch each have regression tests that fail on the pre-fix code. Details below.

## Findings

### Constitution compliance (minor) — CRLF introduced into working copies of 3 files

`git diff` warns: *"CRLF will be replaced by LF the next time Git touches it"* for `frontend/src/components/chat/InflightBar.test.ts`, `frontend/src/components/chat/InflightBar.tsx`, `frontend/src/components/settings/sections/AppearanceSection.tsx`.

The repo pins LF as canonical (`​.gitattributes` line 12: `* text=auto eol=lf`, with the header explicitly stating the goal is "a uniform LF working tree"; backlog #48). The edits wrote CRLF line endings onto the touched lines of these three files (the other edited files — `SettingsDialog.tsx`, `Message.tsx`, `types.ts` — produced no warning). Per the project constitution ("preserve the line-ending style of existing files") this should be normalized.

**Impact:** low — git normalizes to LF at commit, so the *committed* content will be correct either way; the harm is limited to mixed line endings in the working tree until git rewrites the files.
**Fix:** rewrite the three working-copy files with LF endings before commit (e.g. re-save them via the file tools, or `git add --renormalize .`). Also worth spot-checking the two new untracked files (`ChatSection.tsx`, `ChatSection.test.ts`) end in LF while normalizing.

## Verified clean (correctness / bugs / security)

### Chat section move
- **Draft model correct.** `ChatSection.tsx:40-49` snapshots from the store on `active` (dialog open), dirty via `serializeChat(draft) !== snapshot` (51-54), imperative save handle (65-90) commits `setShowTokenUsage`/`setShowMemoryActivity` + `saveSettings({show_token_usage, show_memory_activity})`. Discard-on-close needs no rollback because the toggles have no live side effects and the draft is recaptured from the store on reopen — the comment at 38-39 documents this correctly. Reopen restores the committed snapshot (effect re-runs on `active`).
- **OK saves all dirty sections including Chat.** `SettingsDialog.tsx:89` (chatRef), `:101` (sectionRefs entry), `:160-187` (`handleOk` loops `dirtySectionIds` → `sectionRefs[id].current.save()`); the Cancel/discard confirm lists "Chat" via `SETTINGS_NAV` labels (143-157).
- **Type exhaustiveness.** `NAV_ICONS: Record<SettingsSectionId, …>` (`SettingsDialog.tsx:51-63`) includes `chat: MessageSquare` — a missing key would fail tsc under the `Record` type, so the union addition (`types.ts:9`) is provably covered everywhere it must be (`isSettingsSectionId` at `types.ts:256`, render block at `SettingsDialog.tsx:323-332`).
- **Appearance fully cleaned.** All six removal sites covered: `readDraftFromStore`, `commitDraft`, `saveSettings` payload, `handleResetDefaults`, and both JSX checkboxes. Repo-wide search confirms zero remaining `showTokenUsage`/`showMemoryActivity`/`show_token_usage`/`show_memory_activity` references in `AppearanceSection.tsx`; `AppearanceDraft` has no other constructors (only `AppearanceSection.tsx` + `types.ts`), so the field removal breaks nothing.
- **`saveSettings({theme})` (Appearance) and `saveSettings({show_…})` (Chat) are both valid partial patches.** `SettingsSavePatch` (`lib/tauri.ts:373-422`) declares every field optional with documented patch semantics ("only set fields you want to change") — dropping the toggles from Appearance's payload cannot clobber them in config.toml. No IPC or Rust changes.
- **Persistence unchanged.** localStorage keys `mh.showTokenUsage`/`mh.showMemoryActivity` (`hooks/appearance.ts:13-14`), store setters (`useAgentStore.ts:705-720`), and the config.toml seeding path (`App.tsx:239-248`) are all untouched.
- **No stale "Appearance" references.** Searched all `frontend/src` `*.ts*`: the only remaining mentions are the legit Appearance section itself. `Message.tsx:82,282` comments now correctly say "Settings → Chat". No deep-link call site routes to `"appearance"` for these toggles (`openSettings`/`settingsSection` references are confined to the store + SettingsDialog).
- **Nav search still finds the toggles.** "token usage" moved from appearance keywords to chat keywords (`types.ts:41-54`), so `filteredNav` matching still surfaces them.

### InflightBar fixes
- **Chevron always rendered** (`InflightBar.tsx:160-170`); **bar always toggles** (`:139`) with `aria-expanded`/`aria-label` (`:140-141`). The `(expanded || hasActivity)` gate is gone from both paths — no way to reach a stuck-open or inert-bar state anymore (previously an open-but-empty panel could not be closed via clicks? — it could, via the `expanded` term, but an empty *collapsed* bar could not be opened; both directions now always work).
- **Empty expanded panel = empty gray space.** `:353-356` renders entries only under `hasActivity &&`; the panel div (`:338-339`) keeps its fixed `height` inside the `bg-bg-secondary` wrapper, so the open-empty state is exactly the requested gray space. The `"—"` string literals that remain (`:251` etc.) are inside the ctx hover popup expressions, not the placeholder.
- **`hasActivity` still used correctly** — defined `:74`, consumed by `showBar` (`:119`) and the entries gate (`:353`). No dead variable, no warning risk.
- **Drag-to-resize coherent.** `startDrag` (`:101-110`) still expands when collapsed before entering resize; `onMove` math unchanged; MIN/MAX clamps unchanged. No interaction regression with the always-toggle bar.

### Tests pin the fixes
- `InflightBar.test.ts:40-68` (4 new tests, all fail on pre-fix code): `not.toContain("(expanded || hasActivity) &&")` catches the old chevron gate; `toContain("onClick={() => setExpanded(!expanded)}")` + `not.toContain("if (expanded || hasActivity) setExpanded(")` catch the old click gate; the aria assertions pin the new a11y contract; `not.toContain(">—<")` catches the old `<div …>—</div>` placeholder while the remaining `"—"` string literals don't false-positive, and `hasActivity &&` pins the entries gate.
- `ChatSection.test.ts` (10 tests, new file): placement (`chat` at `indexOf("appearance")+1`, `isSettingsSectionId("chat")`), keyword ownership both directions (appearance keywords no longer claim "token usage"), ownership of both toggles incl. negative assertions against `AppearanceSection.tsx?raw` (would fail pre-move since Appearance contained `showTokenUsage`), and `serializeChat` dirty-detection semantics.
- **Vitest wiring correct.** `ChatSection.test.ts` matches the include glob `src/components/settings/**/*.test.ts` in `vitest.config.ts:13` (it is `.test.ts`, not `.test.tsx` — required, since the include list only picks up `.test.ts`); `InflightBar.test.ts` is explicitly listed (`:15`). The `?raw` import pattern is the established, `tsc`-compiling precedent (`vite/client` types; identical to the existing hover-bridge test).
- **Existing suites unaffected:** `types.test.ts`, `useAgentStore.test.ts`, `ipc-contract.test.ts` reference the store/reducer fields (unchanged), not `AppearanceDraft`.

### Constitution & security
- New public symbols documented: `ChatDraft` (`types.ts:213-221`), `serializeChat` (`types.ts:223-226`), `ChatSection` component (`ChatSection.tsx:14-21`); `ChatSectionProps` members documented, matching the existing `AppearanceSectionProps` style.
- No unused imports/vars introduced (all new imports used; removed code leaves nothing dangling), so no new `deny(warnings)`-class breakage — but this batch's proof obligation is the frontend matrix: run `npm test` + `npm run build` (tsc) in `frontend/` (and the standard `cargo test` at root, which is unaffected — zero Rust changes).
- Security: no keys, no IPC surface changes, no new data flows; `saveSettings` payloads use the pre-existing partial-patch API.

## Non-blocking observations (no action required)
- `aria-label="Toggle reasoning panel"` (`InflightBar.tsx:141`) replaces the button's accessible name, so screen readers no longer hear the "thinking…"/"idle" status text inside the bar. Intentional tradeoff (the button's role is toggling; the label matches its purpose) — noting for the record.
- `ChatSection`'s `saving` state is written but never read in JSX, exactly mirroring `AppearanceSection`'s shipped pattern — consistent with existing style, no warning under the project's tsc config.
