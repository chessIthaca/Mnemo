# Review — Drop the Ollama embedding model (2026-08-11)

**Reviewer:** main agent (self-review). Two spawned reviewer subagents were
unable to write their reports — see finding **B2** below, which is the root
cause of that failure and was fixed as part of this pass.

**Scope:** all uncommitted changes in the working tree (`git diff HEAD`),
covering two related changes: (1) removal of the external Ollama embedding
model in favor of the built-in `HashEmbedder`, and (2) a tool-gating
regression fix that the review itself surfaced.

## Summary

The diff is clean after remediation. Two real bugs were found and fixed in
this pass; no findings remain open.

## Findings

### Bugs

#### B1 — Frontend still referenced removed embedding settings (FIXED)

**Severity:** correctness / runtime crash.

The backend removed `embedding_model` / `embedding_provider` and the
`[memory]` config section (`MemoryConfig`, the `memory` field on
`GeneralConfig`, the `memory` block in `get_settings` JSON, the
`embedding_*` fields in `SettingsSaveDto`). The frontend was not updated,
leaving dangling references:

- `frontend/src/components/settings/sections/MemorySection.tsx` read
  `s.memory.embedding_model` / `s.memory.embedding_provider` — now `undefined`
  on the wire, which would crash the Memory tab at runtime ("Cannot read
  properties of undefined").
- `frontend/src/components/settings/sections/AdvancedSection.tsx` import path
  sent `patch.embedding_model` / `patch.embedding_provider` (silently ignored
  by the backend).
- `frontend/src/lib/tauri.ts` `AppSettings` + `SettingsSavePatch` types still
  declared the fields.
- `frontend/src/components/settings/types.ts` + `types.test.ts` referenced a
  `memory` section in the export/import bundle and nav.

**Fix:** deleted `MemorySection.tsx`; removed the `MemorySection` import +
`{section === "memory" …}` block + `Brain` icon + `memory` nav icon key in
`SettingsDialog.tsx`; removed the `"memory"` `SettingsSectionId` variant, its
nav entry, the `isSettingsSectionId` check, and all `memory` fields in the
export/import bundle in `types.ts`; removed the `memory` block from
`AppSettings` and `embedding_*` from `SettingsSavePatch` in `tauri.ts`;
removed the memory import block + stale comments in `AdvancedSection.tsx`;
removed `memory` fixtures in `types.test.ts`. Verified: `tsc --noEmit` exit 0,
24 vitest pass, no remaining `embedding_*`/`MemorySection`/`.memory`
references in frontend.

#### B2 — Sub-agents denied all write/exec tools (FIXED) — blocked this review

**Severity:** correctness / feature-breaking. This is why the spawned
reviewer subagents could not `file_write` their reports.

`Workflow::allowed_tools()` (`src/workflow/mod.rs:182`) early-returned
`ToolFilter::Planning` whenever `plan_mutations_allowed` was `false` (i.e. for
every sub-agent), regardless of the real workflow state. The `Planning`
filter only allows `SafetyLevel::AutoRun` agent tools, so **every sub-agent
was denied all write/exec tools** — `file_write`, `file_edit`, `file_append`,
`shell`, `git`, `spawn_agent`, `describe_image` — even while the workflow was
in `Executing`. Sub-agents could do no real work; a reviewer subagent
specifically could not write its report file.

Root cause: the Phase 4 "main-agent-only plan mutations" change over-broadened.
Its intent was to restrict **only the four plan-mutation tools**
(create_plan/update_plan/complete_step/abandon_plan) to the main agent — and
that restriction is already enforced in **four** other, correctly-scoped
places: `dispatch.rs:113` (dispatch gate), `turn.rs:295` (schema retain),
`plan.rs` (each tool wrapper re-checks `plan_mutations_allowed()`), and
`prompt.rs`. The `allowed_tools()` collapse was a redundant, over-broad fifth
enforcement that stripped the entire agent surface for sub-agents.

**Fix:** removed the early `return ToolFilter::Planning` for
`!plan_mutations_allowed` in `allowed_tools()`; it now derives from the real
state/skill as the `match` already did. Plan-mutation restriction is left to
the existing targeted gates. Updated the doc comment to state this explicitly.
Added a regression-guard unit test
`allowed_tools_follows_state_even_when_plan_mutations_disallowed` asserting a
`plan_mutations_allowed=false` workflow in `Executing` returns
`ToolFilter::Executing` (not `Planning`). No existing test codified the buggy
behavior. Verified: 508 Rust tests pass.

### Correctness (verified clean)

- No leftover references to `embedding_model`, `embedding_provider`,
  `general.memory`, `MemoryConfig`, or `OllamaEmbedder` in any Rust source
  (search confirmed). `build_embedder()` (no-arg) is called consistently at
  both call sites (`main.rs:317`, `commands.rs:1171`).
- Removing the `[memory]` section does **not** break loading an existing user
  `config.toml` that still has a `[memory]` block: `GeneralConfig` uses
  `#[serde(default)]` with no `deny_unknown_fields`, so unknown sections are
  silently ignored on load and dropped on next save. Verified by reading
  `src/config/general.rs`.
- `build_embedder` now always returns `HashEmbedder`; `MemoryStore` keeps its
  `Embedder` trait + `set_embedder`/`embedder_handle` plumbing (always holds
  `HashEmbedder` now) — valid, harmless abstraction, not refactored.
- The `run_all.rs` `extract_checkpoint_sha` fix (trim before split) handles
  leading whitespace and does not break the other `extract_tests` cases
  (plain sha, halt-reason suffix, non-sha notes) — all pass.

### Security (verified clean)

- Removing the `embedding_provider` endpoint-validation block is correct: the
  field no longer exists, so there is nothing to validate. No input-validation
  regression.
- No changes to sandbox, approval gates, or core operations (`git merge`/`push`
  still approval-gated). The gating fix **restores** sub-agent tool access but
  does not bypass any approval or `never_auto` check — those still run per-tool
  at dispatch.

### Constitution compliance (verified clean)

- Windows paths / PowerShell used throughout; no Linux paths or bash syntax.
- All new/changed public Rust functions have doc comments
  (`build_embedder`, `allowed_tools` doc updated, test fns internal).
- Line-ending style preserved (targeted `file_edit` used for existing files).
- Nothing committed to `main`; commit will target the current feature branch.

## Outcome

Both findings fixed; `cargo test` (508+9+5 passed) and `vitest` (24 passed)
and `tsc --noEmit` (exit 0) all green. Report file included in the commit.