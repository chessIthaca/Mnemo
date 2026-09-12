# Review: AgentLoop god-object split (M1) + full-tree uncommitted changes

**Reviewer:** read-only reviewer subagent (spawn_agent)
**Date:** 2026-04-04
**Scope:** ALL uncommitted changes in the working tree (`git status` / `git diff HEAD`) — not just the agent split. Primary scrutiny on the AgentLoop god-object split (finding M1), which is intended to be a PURE, behavior-preserving refactor.

**Primary artifacts reviewed (the split):**
- `src/agent/mod.rs` (2376 → 41 lines): module docs, `pub mod` decls, `pub use loop_impl::{AgentLoop, TurnOutcome}`, `pub(crate) const MAX_RETRIES`, `#[cfg(test)] mod tests;`
- `src/agent/loop_impl.rs` (new, 308 lines): `AgentLoop` struct (13 fields), `ConstitutionHolder`, `TurnOutcome`, both constructors, all public handle accessors.
- `src/agent/turn.rs` (new, 674 lines): `run_turn` driver.
- `src/agent/dispatch.rs` (new, 196 lines): `execute_tool_call`, `available_tool_names`, `complete_with_retry`.
- `src/agent/tests.rs` (new, 1401 lines): the full `#[cfg(test)]` module.

**Also reviewed (in-tree):** `src/memory/{mod,schema}.rs` (H3 read/write connection split + WAL), `src/provider/client_factory.rs` (new), `src/provider/openai.rs` (dead-code removal), `src-tauri/src/ipc/{approval,commands,events,run_all}.rs`, `src-tauri/src/main.rs`, `src/runtime/agent.rs`, `src/workflow/mod.rs`, `src/config/general.rs`, `src/tool/agent/{sandbox,search}.rs`, `tests/ipc_bridge.rs`, and the frontend (`useAgentStore.ts`, `useAgentEvents.ts`, `useCountUp.ts`, `slash.ts`, `types.ts`, `components/ui/*`).

---

## 1. M1 split fidelity (the main question)

**Method:** extracted `git show HEAD:src/agent/mod.rs` and compared each region against its new home.

- **Struct + fields:** All 13 `AgentLoop` fields present in `loop_impl.rs` with identical types, identical doc comments, and identical initializer values in both constructors. Visibility widened from private to `pub(crate)` — required so `turn.rs`/`dispatch.rs` (sibling modules) can read them. This is the correct, minimal widening; no field was dropped, reordered in a way that matters, or given different lock types.
- **`ConstitutionHolder` + `constitution()`:** moved verbatim (enum + `constitution()` accessor, mtime-reload logic unchanged). Widened to `pub(crate)`.
- **`TurnOutcome`:** identical (fields, derives).
- **Constructors (`new`, `with_constitution_source`):** signatures byte-identical to HEAD; bodies identical (same field order, same `Arc::new(RwLock::new(...))` wrapping, same `None` defaults for `safety_rules`/`session_id`/`last_prompt_tokens`/`agent_id`).
- **All public accessors** (`with_safety_rules`, `with_safety_mode_handle`, `safety_rules_handle`, `safety_mode_handle`, `memory_handle`, `session_id`, `set_session_id`, `agent_id`, `with_agent_id`, `provider`, `set_provider`, `vision_handle`, `is_multimodal`, `describe_image`, `workflow_handle`): compared one-by-one — signatures and bodies unchanged.
- **`run_turn` (turn.rs):** read HEAD lines ~333–~1065 in full and diffed mentally against turn.rs. Token-count reuse, summarization + buffered-command re-injection, auto-recall, system-prompt assembly, streaming `tokio::select!`, usage/cache heuristic (incl. `did_summarize` suppression), suggestion drain, malformed-JSON sanitize-and-retry, tool-exec loop, and the MAX_RETRIES abort are all present, in the same order, with the same lock/await ordering. No logic edits detected.
- **`execute_tool_call` / `available_tool_names` / `complete_with_retry` (dispatch.rs):** identical to HEAD. The only change is visibility: `execute_tool_call` and `complete_with_retry` went from private `async fn` to `pub(super) async fn` so `turn.rs` (a sibling) can call them; `available_tool_names` stays private to `dispatch.rs`. This is correct and minimal.
- **`MAX_RETRIES`:** `const` → `pub(crate) const` in mod.rs; `turn.rs` imports via `super::MAX_RETRIES`. Same value (3), same doc comment.
- **tests.rs:** all 16 test attributes present in both (15 `#[tokio::test]` + 1 `#[test]` for `is_multimodal_reflects_provider_caps`), all 3 provider mocks + `make_registry` present in identical order. The only change is the import block: HEAD used `use super::*;` inside the nested `mod tests`; the new flat file uses explicit imports (`use super::loop_impl::AgentLoop; use super::context; ...`). This is the necessary consequence of the file split. No test body changed (apparent diff noise was indentation from de-nesting and console mojibake on em-dashes — verified no U+FFFD bytes exist in the real file).

**Verdict on M1: faithful, behavior-preserving move. No dropped lines, no logic edits, no changed lock/ordering semantics.**

## 2. Public API stability

`mod.rs` re-exports `pub use loop_impl::{AgentLoop, TurnOutcome};`, so `crate::agent::AgentLoop` / `crate::agent::TurnOutcome` resolve exactly as before. External callers (`src-tauri/src/ipc/commands.rs`, `src/agent/factory.rs`, `src/runtime/agent.rs`, `tests/`) compile against unchanged signatures — confirmed by a green `cargo build -p myharness-app` (exit 0) which compiles all of them.

## 3. Build + test

- `cargo build -p myharness-app`: **exit 0** (green).
- `cargo test` (unpiped): **exit 0**. Unit binary: **396 passed, 0 failed**. All 16 `agent::tests::*` pass by name. Integration suites pass (9 + 5; 3 provider-integration tests are `ignored` as before).
- Frontend `npx tsc --noEmit`: **exit 0** (green).

## 4. Memory pool (H3) + the `open_in_memory` annotation

- `open_in_memory` calls `Self::open_shared_in_memory(embedder, None::<Box<dyn Fn() -> i64 + Send + Sync>>)`. The parameter type is `Option<Box<dyn Fn() -> i64 + Send + Sync + 'static>>`. A bare `Box<dyn Fn() -> i64 + Send + Sync>` carries an implicit `+ 'static` bound, so the annotation unifies with the parameter. **The annotation is correct** and resolves the E0283 inference failure (confirmed by the green build — `None` would otherwise be ambiguous).
- The read/write split is sound: file-backed stores open two connections to the same path; in-memory stores use a shared-cache URI (`file:memdb-<uuid>?mode=memory&cache=shared`) so both connections see one database (a plain `:memory:` would split it). `read_conn()` falls back to `conn` when `read_conn` is `None`. `apply_pragmas` (WAL + busy_timeout + synchronous=NORMAL) is applied inside `apply_schema`, so every connection gets it. Correct.

---

## Findings by severity

### Correctness
**No findings.** The M1 split is faithful; H4 (per-agent approval cleanup) is correct and has a dedicated regression test (`cleanup_for_agent_drops_only_that_agents_pending`); the H3 connection split is correct.

### Bugs
**No findings.** (Note: `approval.rs::resolve` does an O(n) scan over pending approvals keyed by `(agent_id, tool_call_id)` because the frontend supplies no agent id. This is fine — pending approvals are bounded and tiny, and the doc comment explains the design. Not a bug.)

### Security
**No findings.** No new unsafe code, no credential handling changes beyond centralizing the existing key-fallback chain (same order: stored key → `OPENAI_API_KEY` → `ANTHROPIC_AUTH_TOKEN` → `"dummy"`), no path-handling regressions (`sandbox.rs` is a pure clippy-style `is_some_and` cleanup).

### Constitution compliance
- **Doc comments on public functions:** PASS. Scanned all new/modified Rust files; every truly-`pub fn` has a `///` doc. (`pub(crate)`/`pub(super)` are exempt and mostly documented anyway.)
- **cargo test before completion:** PASS (exit 0).
- **No commit to main:** N/A (reviewer did not commit).
- **FINDING (low): dead imports left in `src-tauri/src/ipc/commands.rs`.** After the `client_factory` extraction, four imports are now import-line-only (unused): `OpenAiClient`, `OpenAiClientConfig` (line 17), `LlmClient`, `ProviderKind` (line 18). `cargo build` emits 3 `unused_imports` warnings for this file (the 4th is used only via the grouped import path). These are leftover cruft from the refactor and trip the compiler's `unused` warn lint. Trivial fix: delete the two `use` lines' unused names. (`main.rs` still legitimately uses all four via its dummy-provider fallback — leave those.)

---

## Overall verdict

**SHIP** — the M1 AgentLoop split is a faithful, behavior-preserving refactor with an unchanged public API, green build, and green tests (396 unit + integration, 0 failures). The rest of the tree (H3 pool, H4 approval fix, dead-code/client_factory dedup, frontend store/UX) is correct and well-tested. The single finding is a cosmetic dead-import cleanup in `commands.rs` — non-blocking, but worth a one-line deletion to silence the compiler warnings.
