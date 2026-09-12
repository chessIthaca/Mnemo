# Review: Phase 0 architecture remediation (+ full uncommitted tree)

**Reviewer:** read-only closing-sequence reviewer (`spawn_agent`)  
**Date:** 2026-04-08  
**Scope:** ALL uncommitted changes (`git status` / `git diff HEAD`), including Settings redesign, config atomic save, vision/embedder rewire, vitest, backlog, plans — not only Phase 0.

## Phase 0 items verified

| ID | Intent | Verdict |
|----|--------|---------|
| **C1** Run-All terminal exclusivity | `TurnResolveLatch` in `src-tauri/src/ipc/events.rs`; final `Error` clears running + resolves failure once; `Finished` after failure is not success; MAX_RETRIES in `src/agent/turn.rs` emits only final Error (no trailing Finished); unit tests on latch + `max_retries_aborts_after_consecutive_tool_errors` | **Implemented correctly** |
| **H1** ToolFilter at dispatch | `execute_tool_call` re-checks `wf.allowed_tools()` before approval/exec; Planning/Skill/Executing tests in `src/agent/tests.rs`; existing approval tests create plans so writes are filter-allowed | **Implemented correctly** |
| **H2/H3** Shell harden | `ShellTool` takes `Sandbox`, validates `cwd`, timeout + `kill_on_drop`, factory passes sandbox; traversal/outside/timeout tests | **Implemented correctly** (cwd only — command body can still leave the tree; still `NeedsApproval`) |
| **H4** Git AutoApproveProject | `is_git_read_only` — status/diff/log + branch list only; commit/checkout/stash/branch mutate/merge/push false; tests cover both | **Implemented correctly** |

---

## Critical

_No Critical findings._

---

## High

### H1 — `save_all` / `write_atomic` crash window can delete live config without auto-recovery

**Files:** `src/config/mod.rs` (`commit_atomic_rename`, `save_all`, `write_atomic`)

On Windows the commit path is:

1. copy live → `.bak`
2. `remove_file(path)` if it exists
3. `rename(path.tmp → path)`

If the process dies after step 2 and before step 3 succeeds, the live `config.toml` / `endpoints.toml` / `keys.toml` is **gone**. A `.bak` (and possibly `.tmp`) may remain, but **nothing on `Config::load` restores from `.bak`**. Next startup can fall back to defaults / empty keys — silent loss of API keys and endpoints.

`write_atomic` (single-file) has the same remove-then-rename window and **no** `.bak` at all.

**Fix direction:** Prefer rename-over without delete where the OS allows; or on load, if target missing and `.bak`/`.tmp` present, recover; or write to a unique temp and use replace APIs that don’t delete first. Treat keys.toml with extra care.

### H2 — `build_embedder` always constructs `OllamaEmbedder` for any matching endpoint

**Files:** `src/provider/client_factory.rs` (`build_embedder`); wired from `src-tauri/src/main.rs`, `rewire_vision_and_embedder` in `src-tauri/src/ipc/commands.rs`

```rust
if let Some(ep) = config.endpoint(provider) {
    return Arc::new(OllamaEmbedder::new(ep.base_url.clone(), model.to_string()));
}
```

Any configured endpoint name — including OpenAI-compatible cloud endpoints — is treated as Ollama’s native `/api/embed` API (after `/v1` strip). Settings → Memory encourages “endpoint name from Providers” without restricting kind. Saving a non-Ollama provider as embedding_provider yields a live embedder that will fail at recall/write time (or worse, hit the wrong host path), while UI reports success after `save_settings`.

Also: swapping embedder dimension mid-store (e.g. hash 384 vs nomic 768) leaves old rows with incompatible vectors; `cosine` over mismatched lengths is undefined/noisy. `set_embedder` docs mention this lightly; Settings does not warn.

**Fix direction:** Only use `OllamaEmbedder` for `EndpointKind::Local` (or explicit Ollama URL heuristic); otherwise hash fallback or a real OpenAI embeddings client. Warn in UI when provider kind ≠ local. Document/guard dimension changes.

---

## Medium

### M1 — Settings close confirm only tracks Providers + Appearance dirty

**Files:** `frontend/src/components/settings/SettingsDialog.tsx`; sections `SafetySection`, `MemorySection`, `VisionSection`, `PricingSection`, `AdvancedSection`

Shell `anyDirty` is `providersDirty || appearanceDirty`. Safety / memory / vision / pricing / advanced each have local dirty state and independent Save, but closing Settings (Done / Escape / outside) **does not warn** if those sections have unsaved edits. Easy to lose safety-mode or pricing edits.

**Fix direction:** Plumb `onDirtyChange` for every section (or a shared dirty registry), and include them in `requestClose` confirm copy.

### M2 — Import bundle can partially apply then fail (non-atomic cross-command)

**File:** `frontend/src/components/settings/sections/AdvancedSection.tsx` (`handleImportFile`)

Flow: optional `saveEndpoints(...)` then `saveSettings(patch)`. If the second call fails (validation, disk), endpoints/keys may already be rewritten while general/memory/ui/pricing are not. No rollback. Combined with H1, this raises config-corruption risk.

**Fix direction:** Single backend import command that validates fully then commits; or save settings first when no endpoint dependency, and document failure modes; surface “partial import” on error.

### M3 — Final `Error` resolves Run-All without waiting for running descendants

**File:** `src-tauri/src/ipc/events.rs` (final `Error` arm vs `Finished` arm)

`Finished` only calls `on_main_turn_resolved(true, …)` when `!descendants_running`. Final `Error` always resolves failure for main immediately, even if subagents are still running. Pre-existing shape was similar; Phase 0 moved/extended Error handling (running clear + latch) but did not align descendant gating. Can mark Run-All failed while children still work, or race with later child lifecycle.

**Fix direction:** Mirror Finished’s descendant check (or explicitly document fail-fast and cancel descendants).

### M4 — Safety UI copy outdated after H4

**File:** `frontend/src/components/settings/sections/SafetySection.tsx`

`auto-approve-project` description still implies only shell/unscoped tools prompt. After H4, mutating git (commit/checkout/stash/branch create|delete) also prompts under AutoApproveProject. Users may expect git commit to auto-run.

### M5 — `describe_image` always in schema when vision empty

**Files:** `src/agent/factory.rs`, `src/provider/vision.rs` (`SwappableVision`)

Intentional for Settings rewire without registry rebuild. Side effect: model can call `describe_image` when unconfigured and burn a tool turn on a hard error. Prefer schema omission when `!is_configured()` **or** keep always-on but ensure system prompt documents the requirement. Not a security hole; quality/token cost.

### M6 — Theme dual-write: localStorage wins over backend `[ui].theme` forever after first pick

**Files:** `frontend/src/App.tsx`, `frontend/src/hooks/useAgentStore.ts`, Appearance save via `saveSettings`

Backend theme applies only when `mh.theme` is absent. Appearance Save writes both store/localStorage and config.toml. Import/other machines reading only config.toml won’t override a stale localStorage key. Documented in App comments but easy to misread as “config is source of truth.”

### M7 — `grok.md` at repo root is a large untracked product-spec dump

**File:** `grok.md` (untracked)

Not harmful at runtime, but easy to commit accidentally (noise / possible internal review text). Prefer `.coding/` or delete before commit if not intentional.

---

## Low

### L1 — Settings dirty badge asymmetry

Providers/Appearance show amber unsaved indicators in nav; other dirty sections do not (related to M1).

### L2 — `TurnResolveLatch` unit tests cover latch math only

No integration test that the IPC forwarder + `on_main_turn_resolved` see Error-then-Finished as a single failure. Latch tests + turn emission test are good; end-to-end still gap.

### L3 — Shell timeout kills process group? 

`kill_on_drop` kills the direct child (PowerShell/sh). Grandchildren may outlive timeout on some platforms. Acceptable for v1; document.

### L4 — `backlog.json` / plan stack churn in same WIP tree

Expected for multi-plan work; ensure Phase 0 commit doesn’t bury unrelated backlog-only noise without message clarity.

### L5 — Vitest added but narrow include

`frontend/vitest.config.ts` only runs `settings/**/*.test.ts`. Fine bootstrap; `package.json` scripts are correct.

### L6 — Constitution: public Rust APIs generally documented

New public helpers (`write_atomic`, `set_embedder`, `set_vision`, `SwappableVision`, `get_settings`/`save_settings`) have doc comments. Good.

---

## Other WIP notes (no separate severity)

- **Settings redesign** (`frontend/src/components/settings/**`, ConfigDialog re-export, `get_settings`/`save_settings`, StatusBar `saveSettings` on safety change): substantial and mostly coherent; main residual risks are M1–M2 and H1–H2.
- **Atomic multi-file save** intent is right; Windows commit implementation undercuts the durability claim (H1).
- **Vision `SwappableVision` + always-on tool:** runtime rewire works; see M5.
- **Phase 0 security posture (H1–H4, C1):** solid; dispatch filter + git read-only split + shell cwd/timeout are the right controls.

---

## Summary

Phase 0 (C1, H1–H4) is **correctly implemented** with meaningful tests. The rest of the uncommitted tree is dominated by Settings + config durability + embedder rewire. **Ship-blockers to fix before relying on Settings saves in production:** **H1** (config delete-before-rename crash window / no load-time `.bak` recovery) and **H2** (Ollama embedder forced for every endpoint). Then address **M1** (dirty close) and **M2** (partial import).

**Recommendation:** Fix H1 and H2 in this pass; do not treat “transactional save” as done until load-path recovery or non-destructive replace is proven on Windows.
