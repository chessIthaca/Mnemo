# Consolidated Code Review — uncommitted changes

Three parallel background reviewers covered the diff (~4,071 insertions / 42 files
+ several new files). Sources: `rust-core-review.md`, `tauri-ipc-review.md`,
`frontend-review.md`. This document merges them, records what was **fixed** in this
pass, and what is **deferred**.

**Baseline health (verified by the reviewers):** `cargo build` clean, `cargo test`
357/0 (now 364/0 after the new tests below), frontend `tsc --noEmit` clean.
**No critical or safety-compromising findings** — path confinement, shell handling,
tool exposure, and the safety gate are all sound.

---

## Fixed in this pass

| # | Severity | Where | Finding | Fix |
|---|----------|-------|---------|-----|
| rust#1 | **Major** | `src/agent/mod.rs` | Cache-token heuristic over-reported `cached_tokens` after a context summarization (the prefix is rewritten, so `min(prev,curr)` was wrong) and on a fresh session. | Added a per-turn `did_summarize` flag; the heuristic is skipped (`cached = 0`) when the turn summarized. New test `cache_heuristic_skipped_after_summarization`. |
| rust#2 | Minor | `src/agent/mod.rs:559` | Stats `session_id` read from loop state instead of the `run_turn` parameter (implicit invariant). | Use the authoritative `session_id` param for stats. |
| rust#3 | Minor | `src/tool/agent/spawn_agent.rs:41` | `SpawnAgentTool::new` missing doc comment (project rule). | Added `/// Create the tool with the spawner that starts agents.` (plus docs for the new `with_parent`). |
| ipc#4 | Minor | `start.bat` | The `tauri.cmd` existence check sat *after* the `dev` branch, so `start.bat dev` with no `node_modules` failed with a raw "not recognized". | Moved the guard above the `dev` branch. |

### New feature (this pass's main work): spawn_agent completion feedback loop
The headline gap the review round exposed — a spawned background agent was
**fire-and-forget** (no parent linkage, no completion signal, no auto-combine).
Fixed end-to-end:
- `AgentHandle.parent_id` + `AgentManager::parent_id` record the spawning agent.
- `AgentLoop.agent_id` + `AgentLoopFactory::build_with_id` stamp each agent with its
  runtime id; the `spawn_agent` tool is made parent-aware via a new
  `ParentAwareSpawner` seam (base `AgentSpawner` trait unchanged / object-safe).
- `AgentEvent::ChildFinished{child_id,name,success}` (+ serializable mirror).
- The event forwarder (`events.rs`), on a tool-spawned agent's **first** `Finished`
  or final `Error`, sends the parent a `Suggestion` ("read its report and combine
  the results into a plan") and emits `ChildFinished` for the sidebar. Deduped
  per-child, cleaned up on `Exited`, no-op for main/UI agents.
- Frontend: `child_finished` kind typed + handled; ToolCard shows
  `spawn_agent (name)`; the parent sees the notification as a `steer` bubble.

**Verification after all changes:** `cargo test` 364/0, `tsc --noEmit` clean.

---

## Deferred (resolved in the follow-up pass — see below; none remain open)

| # | Severity | Where | Finding | Status |
|---|----------|-------|---------|--------|
| fe#2/#3 | Minor | `StatsView.tsx` | Real-time effect over-fetches `getProjectStats` + full `getSessionList` on every token bump; double-fetch on agent switch. | **FIXED** — session-only real-time refresh, full refresh on mount/agent change, agent-switch guard. |
| fe#1 | Minor | `StatsView.tsx` | Inconsistent dropped-error handling (some silent-empty, one error banner). | **FIXED** — aggregate failures set a single banner; "no session yet" stays a legitimate state. |
| fe#4 | Minor | `StatusBar.tsx` | `set_model`/`set_effort` failure only `console.error`s — toolbar label can desync from backend. | **FIXED** — inline transient error + re-sync from `getConfig()` on failure. |
| fe#5 | Minor (a11y) | `StatusBar.tsx` | Model/effort dropdowns are mouse-only (no Escape/aria/keyboard nav). | **FIXED** — Escape close, aria-haspopup/expanded, role=menu/menuitem, ArrowUp/Down roving focus. (Plan-step + safety pickers share the same pattern but were out of the finding's scope.) |
| fe#6 | Nit | `useAgentStore.ts` | Seven near-identical `setCode*Color` setters (~110 lines). | **FIXED** — one shared `setCodeColor(key, value)` helper; the 7 setters are one-line delegates. |
| fe#7 | Nit | `Message.tsx` `CodeBlock` | Copy uses `String(children)` → `[object Object]` for nested/highlighted code. | **FIXED** — copies `codeRef.textContent` instead. |
| rust#4 | Minor | `src/memory/types.rs` | New public stats structs have sparse field docs. | **FIXED** — full field docs on all six stats structs. |
| rust#5 | Nit | `src/runtime/correction.rs:58` | `"summar"` trigger is broad — matches benign "summarize this file". | **FIXED** — replaced with summary-specific phrases + regression tests. |
| rust#6 | Nit | `src/agent/mod.rs` | Fire-and-forget stats write can be lost on shutdown. | Reviewed — deliberate trade-off (never block the turn), documented in code; no change. |
| ipc#1 | Minor | `commands.rs:371` `set_model` | Model-allowlist can silently reject the endpoint's *current* model after a config edit. | **FIXED** — `default_model` is implicitly allowed. |
| ipc#2 | Minor | `commands.rs` `spawn_agent_shared` | Initial prompt sent after releasing the manager lock — cosmetic `running:false` window. | Reviewed — harmless (the `Started` event flips it immediately), documented; no change. |
| ipc#3 | Minor | `commands.rs` `set_model` | Provider built before the factory-presence check. | **FIXED** — factory checked (and cloned) first, before any provider/env work. |
| ipc#6/#7 | Nit | `commands.rs`, `main.rs` | Missing blank line after `get_session_list`; `eprintln!` vs `log` mix. | Blank line **FIXED**; eprintln-vs-log reviewed — consistent with surrounding style, no change. |
| ipc#8 | Hardening | `tauri.conf.json` | `csp: null` — no CSP on the webview. | **FIXED** — strict production `csp` + permissive dev-mode `devCsp` (Vite HMR needs inline/eval/ws). Manual dev smoke test recommended. |

## Verified clean by the reviewers (no action)
- All new IPC commands are read/state ops or validated; no filesystem/shell strings.
- `shell:allow-open` is the scoped open (http/https/tel/mailto); no `allow-all`.
- `spawn_agent` tool is `NeedsApproval` and governed identically to UI spawns.
- `.coding/safety.toml` additions are read-only/diagnostic auto-approves only.
- Frontend↔backend event/IPC contracts (`Usage`, stats, `get_config`, `set_model`)
  match field-for-field; event-listener singleton has no leak; ports align.
