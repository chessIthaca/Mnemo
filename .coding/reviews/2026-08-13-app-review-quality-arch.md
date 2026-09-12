# Review: Code Quality & Architecture — myharness

**Date:** 2026-08-13 · **Scope:** src/tool/mod.rs, src/workflow/mod.rs, src/error.rs, src-tauri/src/ipc/{state,settings,files}.rs, frontend/src/hooks/useAgentStore.ts, frontend/src/lib/tauri.ts, frontend/src/components/settings/types.ts · **Working tree:** clean at HEAD

## Verdict

Overall architecture is strong: an enforced (not prompt-level) plan-first state machine, a clean Tool trait with category/safety/filter axes, genuinely layered IPC with typed DTOs, and exemplary doc-comment and test coverage. The main risks are size concentration — two frontend/IPC files approaching god-module scale — and an error enum that is typed in name only (string payload variants) plus full string flattening at the IPC boundary.

## Critical

- None.

## High

- **`settings.rs` is a god-module in progress** — src-tauri/src/ipc/settings.rs:1 (1750 lines; header alone claims config + endpoints + keys + pricing + settings patch + live model listing + runtime rewires). One file owning seven concerns invites merge conflicts and review fatigue. *Fix:* split along the header's own seams (endpoints.rs, keys.rs, pricing.rs, rewire.rs).
- **`useAgentStore.ts` facade at 958 lines still large** — frontend/src/hooks/useAgentStore.ts:1. The H3 facade split is the right design (pure reducers in agentEventReducer.ts, appearance in appearance.ts), but 958 lines of store composition + re-exports remains the biggest frontend file. *Fix:* acceptable as-is; if it grows, extract the steer/backlog action block into its own hook module.
- **Error variants are string buckets** — src/error.rs:11-36. `Config/Project/Provider/Tool/Workflow/Memory/Safety/Runtime/Browser` are all `(String)` newtypes, so `match` can discriminate only by subsystem, never by failure kind; callers can't programmatically distinguish e.g. "no plan exists" from "update outside Executing" (both `Workflow(String)`). *Fix:* promote the few most-branched-on cases (Workflow no-plan / wrong-state, NotFound vs InvalidInput) to struct variants or dedicated variants.

## Medium

- **Every IPC command flattens errors to `String`** — src-tauri/src/ipc/settings.rs:27, files.rs:24 (`Result<T, String>`). The typed `Error` enum is lost at the boundary; the frontend can only display, never branch (e.g. can't distinguish validation error from IO error to offer different UX). *Fix:* return a serializable `{ kind, message }` error DTO for commands where the frontend could reasonably react differently.
- **`never_auto()` is dead weight** — src/tool/mod.rs:126-128. Doc comment itself states "Today no tool overrides this"; the blanket invariant lives in `never_auto_for`. Kept "for future tools" — speculative generality. *Fix:* remove it and re-add when a real blanket-prompt tool appears, or add a `#[cfg(test)]`-only override to keep it honest.
- **`ToolFilter::from_state` maps `Skill` → `Planning` as a "defensive default"** — src/tool/mod.rs:197. A state whose correct filter must always come from elsewhere silently degrades to a wrong-but-plausible filter if a future caller forgets. *Fix:* make `from_state` non-exhaustive for `Skill` (return `Option` or panic/unreachable with a clear message) so misuse fails loudly.
- **Dual `kind` parsing paths can drift** — src-tauri/src/ipc/settings.rs:116-118 accepts both serde ("openai") and Debug ("OpenAI") forms, and frontend/src/components/settings/types.ts:131-134 (`kindFromConfig`) duplicates the same normalization on the TS side. Two normalizers for one legacy payload shape. *Fix:* emit only the serde form from `get_config` (it's typed — the fixture test would catch it) and delete both lenient paths.
- **Unsanitized sidecar parse fallback** — src/workflow/mod.rs:699: `serde_json::from_str(&text).unwrap_or_default()` on the legacy bare-array form silently treats an unparseable sidecar as an empty stack (masking corruption as "no plan"). *Fix:* log a warning (or surface a recoverable error) when the sidecar exists but fails both parses.

## Low

- **`execute(&self, args: Value)` clones arguments at dispatch** — src/tool/mod.rs:385 (`call.arguments.clone()`). Minor cost per call; harmless at this scale.
- **`ToolRegistry::schemas` iterates a HashMap then sorts** — src/tool/mod.rs:355-379. Correct and well-justified (prompt-cache stability); just noting the sort must not be "optimized" away.
- **Legacy mtime fallback is best-effort with `?`-in-`ok()` chains** — src/workflow/mod.rs:762-778: `entry.ok()?` aborts the whole scan on one bad dir entry, returning `None`. Benign (legacy path only), but `filter_map` would be more robust.
- **`makeUid` Math.random fallback** — frontend/src/components/settings/types.ts:105-110. Fine for React keys; just ensure it's never reused for anything security-adjacent.
- **Double blank line** — frontend/src/components/settings/types.ts:224-225. Cosmetic.

## Areas checked and clean

- **Tool trait / registry design** (tool/mod.rs:99-159): tight surface (name/category/schema/safety/never_auto_for/approval_preview/execute), argument-aware `never_auto_for` is the right shape for the git-subcommand invariant, `approval_preview` default-None with documented swallow semantics.
- **ToolFilter gating** (tool/mod.rs:202-321): visibility-not-just-approval is the correct hard-rule enforcement; spawn_agent carve-out is documented and tested in every state; Skill allow-list is explicit; tests pin every arm.
- **Workflow state machine** (workflow/mod.rs): Reviewing gated on `finish` + report is unskippable by construction; `reviewed` flag solves the Reviewing-vs-Complete restart ambiguity (with a regression test for the skill-path bug); update_plan refuses out-of-order-completion data loss; empty-steps plans rejected both at create and at load; sidecar has backward-compat + explicit-empty-stack semantics, all pinned by tests.
- **Plan-stack model** (workflow/mod.rs:111-146): sub-plan push/pop with parent resume is clean; `tool_allowlist` deliberately not persisted (spawn-time property) — documented why.
- **IPC state layering** (ipc/state.rs): IpcState decomposed into runtime/project/backlog contexts with a documented lock-ordering invariant; every `Option` field explains the degraded-startup case.
- **Settings DTO hygiene** (ipc/settings.rs:57-113): secrets travel in a separate `api_keys` map, never in the DTO; validation (trailing-slash, non-empty name) lives in `into_endpoint`, not the frontend.
- **Frontend reducer split** (useAgentStore.ts:1-8): pure event reducers separated from the zustand store; types re-exported for a stable consumer interface.
- **Sandbox validation at IPC edge** (ipc/files.rs:25-28): path validated before any fs access.
- **Doc-comment coverage**: every public item in all nine files is documented, including rationale ("why") comments, not just "what".
- **Determinism for prompt cache**: schemas sorted by name with a test pinning it.
