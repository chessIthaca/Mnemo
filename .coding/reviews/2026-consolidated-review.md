# Consolidated Review — myharness (2026 whole-codebase review)

**Source reports:**
- `.coding/reviews/2026-chief-architect-review.md` (Architecture)
- `.coding/reviews/2026-code-quality-review.md` (Code Quality)
- `.coding/reviews/2026-ui-usability-review.md` (UI / Usability)
- `.coding/reviews/2026-prd-review.md` (PRD vs implementation)

This report deduplicates findings across the four perspectives, prioritizes them, and cross-references each to its source report + file:line. The codebase is in good shape: build/test all green (`cargo test`, `tsc --noEmit`, `vite build`), the prior round's Critical (shared singleton workflow) is **fixed**, and all six PLAN.md critical gotchas are handled with tests. The findings below are scaling cliffs, quality debt, spec deviations, and usability gaps — **not** correctness breakage.

---

## Cross-perspective headline

Two independent reviewers (UI/Usability and PRD) converged on the **same root finding**: PLAN.md locked in **shadcn/ui (Radix + Tailwind)** as the component library, but it was never adopted (`frontend/package.json` has no `@radix-ui/*`). The UI reviewer identifies this as the **root cause of the accessibility gaps** (Radix ships focus traps, ARIA roles, keyboard nav, scroll-area for free) — a codebase-wide search found **zero** `aria-*`/`role=`/`tabIndex`/`prefers-reduced-motion` matches. The PRD reviewer records it as a locked-in-decision deviation. **Adopting shadcn/ui would close the largest UI finding and the largest frontend spec deviation in one move.** Sources: UI §6, §8; PRD §1, Deliberate-deviation #2.

---

## Priority levels

| Level | Meaning |
|---|---|
| **P0 — Critical** | User-facing breakage or data loss. *(None found.)* |
| **P1 — High** | Correctness fragility, scaling cliff, or high-impact UX/spec gap. Fix soon. |
| **P2 — Medium** | Quality debt, throughput, or usability issue. Fix before scale. |
| **P3 — Low** | Cleanup, polish, minor drift. Fix when convenient. |

---

## P1 — High Priority

### H1. Accessibility infrastructure is absent (root cause: no shadcn/Radix)
**Sources:** UI §6; PRD §1-deviation #2; Quality §8 (adjacent)
**Evidence:** zero matches for `aria-`/`role=`/`tabIndex`/`onKeyDown`/`prefers-reduced-motion`/`prefers-color-scheme` across all 23 frontend files; `frontend/package.json:11-32` (no `@radix-ui`); PLAN.md:54 (specifies shadcn/ui).
**Impact:** Tab bars lack `role="tab"`; dialogs (`ConfigDialog`, `SafetyToggleDialog`) have no focus trap/`role="dialog"`/focus-restore; no keyboard shortcuts for Approve/Deny; motion animations can't be disabled; OS color-scheme ignored. The app is a keyboard-heavy power-user tool with minimal keyboard/AT support.
**Fix:** Adopt shadcn/ui (Radix) as PLAN.md specified — Dialog, Tabs, ScrollArea provide most of this for free. Add `prefers-reduced-motion` guards and Approve/Deny keyboard shortcuts. **Cross-perspective: closes a UI High + a PRD deviation simultaneously.**

### H2. `WorkflowStateInfo` TS type drift forces `as never` escape casts (latent casing bug)
**Source:** Quality §8 (High)
**Evidence:** Rust `WorkflowStateInfo` (`commands.rs:93-103`) has `state` (Display, **capitalized**) + `depth` + `parents`; TS (`types.ts:140-143`) has only `state: string` + `plan`. Store's `setWorkflowState` expects lowercase serde union. Papered over with `wf.state as never` (`App.tsx:98`, `StatusBar.tsx:87`).
**Impact:** `as never` accepts any string silently; a Rust Display change or new state variant produces a store value that won't match the `WorkflowState` union and `tsc` won't catch it. The capitalized-vs-lowercase mismatch is a latent runtime bug.
**Fix:** Make Rust emit serde (lowercase) state in `WorkflowStateInfo`; type `state: WorkflowState` in TS; add `depth`+`parents`; drop `as never`. Verify casing matches at runtime.

### H3. Single shared `Mutex<Connection>` memory store — multi-agent throughput cliff
**Sources:** Architecture §5 (High); Quality §5 (sound but serialized)
**Evidence:** `src/memory/mod.rs:103` (one `tokio::Mutex<Connection>`); every op does `self.conn.lock().await` (`:218,352,384,437,454,481,499,508,545,568,621,688`); auto-recall (O(N) scan) runs every turn under it.
**Impact:** Concurrency-safe but serializes all agent memory access. With 3+ concurrent agents, recall scans under one lock become the throughput ceiling. SQLite WAL + multi-connection read concurrency is foregone.
**Fix:** Connection pool / read-write split (WAL) or per-agent read connections; keep the single write connection. Not a single-agent bug.

### H4. `PendingApprovals` keyed by `tool_call_id` only; `cleanup_for_agent` clears ALL
**Source:** Architecture §4 (Medium→High under multi-agent)
**Evidence:** `src-tauri/src/ipc/approval.rs:65-77` — `cleanup_for_agent` drops every pending approval on any agent exit; comment admits the map can't target one agent.
**Impact:** Safe today only by an unenforced invariant (≤1 concurrent pending approval, single-primary-agent product). Under concurrent multi-agent approvals, one agent's exit drops another's sender → spurious "approval dropped" tool failure.
**Fix:** Key by `(agent_id, tool_call_id)`; cleanup only that agent's entries.

### H5. (Downgraded to M-medium — see M9.) Dead `async-openai` conversion layer is redundant, not a coverage gap
**Source:** Quality §2 (Medium, corrected)
**Note:** An earlier version of this item (and the Code Quality source) carried this as a **High** with the claim that the production path `build_request_json` had "no dedicated unit test" for multimodal/tool-call/assistant cases. The report-of-reviews audit verified that claim is **false** — `build_request_json` is directly unit-tested (`openai.rs:1536,1577,1618,1376,1407,1435`). The finding is therefore dead-code cleanup, **not** a coverage gap, and is moved to **M9** below with corrected justification.

### H6. First-run empty state + plan discoverability
**Sources:** UI §4 (High); UI walk-through
**Evidence:** `Conversation.tsx:57-72` (no empty-state branch); `Sidebar.tsx:36-43` + `RightPanel.tsx:24-66` (plan behind enable-tab + reveal-panel + select-tab); no auto-show of plan on create/step-complete.
**Impact:** A first-time user lands on a blank conversation with only the input placeholder as guidance; the plan — the central plan-first artifact — is buried behind 3 actions.
**Fix:** Add a centered welcome with example prompts + plan-first explainer when transcript is empty; auto-open the right panel on the Plan tab when a plan is created or a step completes (mirror the DiffViewer approval-auto-show).

### H7. Backlog/Run-All loop (commits + rolls back the working tree) is untested
**Source:** Quality §4 (Medium, but high-risk path)
**Evidence:** `commands.rs:1096-1281` (`run_all_dispatch_next`, `on_main_turn_resolved`, `halt_run_all_for_approval`); `run_all.rs:67-101` (checkpoint/rollback). No test covers the loop.
**Impact:** The riskiest unattended-execution path — it git-checkpoints, dispatches, commits on success, rolls back on failure — has no automated test. A bug here mutates the user's working tree.
**Fix:** Add integration tests for the Run-All loop (checkpoint → dispatch → resolve → commit/rollback), plus a `set_model` swap test and a direct `safety_rules::is_safe` signature-matching test.

---

## P2 — Medium Priority

### M1. `AgentLoop` god-object (2376 lines, 13 responsibilities)
**Sources:** Architecture §10 (High); Quality §3 (Medium)
**Evidence:** `src/agent/mod.rs:41-94` (struct) + 2376 lines mixing prompt-build, streaming, tool dispatch, approval, recall, cache heuristics, constitution reload.
**Fix:** Split along seams: `PromptBuilder`, `ToolDispatcher`, `StreamAccumulator` (partly exists as `DeltaAccumulator`), session/cache heuristics out. → ~3-4 files of 400-700 lines, independently testable.

### M2. Duplicated provider key-resolution + client-build block (security-drift hazard)
**Source:** Quality §2 (Medium)
**Evidence:** `main.rs:271-304` (main) + `main.rs:364-388` (vision) + `commands.rs:446-466` (`set_model`, commented "Mirror the api-key fallback chain"). Three copies of a credential-resolution chain.
**Fix:** Extract `fn build_client(config, endpoint, model, …) -> Arc<dyn LlmClient>` shared by all three.

### M3. Light-theme code highlighting broken
**Source:** UI §2 (High→Medium-High)
**Evidence:** `globals.css:26-44,122-135` — `.light` overrides bg/text/border but not the `--code-*` vars (defined only in `:root` dark). Code blocks render dark-theme token colors on a light background.
**Fix:** Define `--code-*` in the `.light` block or use a light hljs theme.

### M4. Slash commands undiscoverable; `/model`+`/provider` restart-only; `/save`+`/load` path-only
**Source:** UI §4 (Medium)
**Evidence:** `slash.ts` (no autocomplete); `InputBar.tsx:122-129` (no menu on `/`); `slash.ts:43-44` (help says "restart to take effect"); `InputBar.tsx:191-243` (no file picker).
**Fix:** Add a `/`-triggered command menu with descriptions; surface "restart required" inline for model/provider; add a file picker for save/load.

### M5. `handleAgentEvent` is a 340-line switch mixing early-returns and fall-through breaks
**Source:** Quality §9 (Medium)
**Evidence:** `useAgentStore.ts:821-1160` — some cases `return` early, others `break` to a shared tail; hard to audit; only exercised via live events (not unit-tested).
**Fix:** Split each case into a pure `reduce(agent, event) => AgentState` function, independently testable.

### M6. Fan-in is a single global serial point (forwarder locks manager per event)
**Source:** Architecture §2 (Medium)
**Evidence:** `src-tauri/src/ipc/events.rs` — one forwarder; manager lock acquired on every `Started` (`:135`), `Finished` (`:142`), `Exited` (`:162`), and the approval branch (`:184`).
**Impact:** At 3+ simultaneous streaming agents, per-event lock acquisitions + single emit thread become the throughput ceiling.
**Fix:** Batch running-state updates; consider per-agent emit or a lock-free running flag (already atomic on `AgentHandle`).

### M7. Multi-agent-readiness claim overstated (PLAN.md:6/11)
**Source:** Architecture §9
**Evidence:** Spawning + per-agent workflow ✅; but memory throughput (H3) and approval isolation (H4) break under concurrency; provider/safety-mode/config are intentionally global.
**Fix:** Qualify PLAN.md's "multi-agent-ready from day one" to note shared memory-connection and global safety/config as concurrency limits.

### M8. Provider/model swap is "half-runtime" (factory vs live-loop paths can drift)
**Source:** Architecture §3/§5
**Evidence:** `factory.rs:154-162` swaps for newly-built agents; live agents swapped separately per-loop via `set_provider`. Two paths.
**Fix:** Centralize the swap so factory + all live loops update atomically, or document the two-step contract.

### M9. Dead `async-openai` conversion layer is redundant with `build_request_json` tests
**Source:** Quality §2 (Medium, corrected from High)
**Evidence:** `src/provider/openai.rs:694,772` (`convert_message`/`convert_tool_choice`) called only from `#[cfg(test)]` (`:1181,1197,1214,…`). Production uses `build_request_json` (raw JSON, `:347-476`), which **is** unit-tested for the multimodal/image/stripping/plain-text/strict/reasoning-effort cases (`openai.rs:1536,1577,1618,1376,1407,1435`).
**Impact:** Two parallel serialization paths, only one exercised in prod; the `convert_message` tests are **redundant** with the existing `build_request_json` tests (not a coverage backstop). Dead code + an unnecessary transitive dependency.
**Fix:** Delete the conversion layer + `async-openai` dep + its tests; no new tests needed. *(Corrected after the report-of-reviews audit found the earlier "no dedicated test" claim was false.)*

---

## P3 — Low Priority

| # | Finding | Source | Evidence |
|---|---|---|---|
| L1 | `_dbg_test.rs` debug shim clutters provider module | Arch §1; Quality §2 | `src/provider/_dbg_test.rs` |
| L2 | `#[allow(unused_imports)] use futures::StreamExt` masks real signal | Quality §2 | `openai.rs:20-21` |
| L3 | `get_config` is the one `#[tauri::command]` missing a `///` (constitution rule) | Quality §7 | `commands.rs:587` |
| L4 | `MemoryTier::from_str` shadows `std::FromStr` | Quality §6 | `memory/types.rs:36` |
| L5 | Several mechanical clippy fixes (`is_some_and`, `contains`, derivable `Default`) | Quality §6 | `sandbox.rs:140`, `search.rs:36`, `general.rs:26,96`, `workflow/mod.rs:32` |
| L6 | `MAX_RETRIES` name duplicated across two retry concepts in two modules | Arch §6 | `runtime/agent.rs:48` vs `agent/mod.rs:38` |
| L7 | `std::sync::Mutex` inside async (`AgentLoop` session/cache/agent_id) — safe today, fragile if held across await | Arch §5 | `agent/mod.rs:82-93` |
| L8 | `let _ = wf.load_latest()` swallows plans-dir I/O error | Arch §3 | `factory.rs:203` |
| L9 | Approval button order: highest-escalation ("Allow for project") first | UI §3 | `ApprovalPrompt.tsx:125-158` |
| L10 | File-path diff toggle doesn't read as a button; sidebar "disabled"=line-through | UI §3 | `ApprovalPrompt.tsx:116-123`, `Sidebar.tsx:62-64` |
| L11 | Right panel split not user-resizable; narrow windows squeeze chat | UI §1 | `RightPanel.tsx:37` |
| L12 | Retry progress + unanswered-approval not visibly surfaced in transcript | UI §5 | `channels.rs:144-147`; `ApprovalPrompt.tsx` (no DenyAll button) |
| L13 | `11× eslint-disable react-hooks/exhaustive-deps` — some lack an explaining comment | Quality §10 | `App.tsx:140,164`, `PlanProgress.tsx:41,52,61`, … |
| L14 | Inline code contrast borderline (`text-pink-300` on `bg-bg-tertiary`, ~3.5:1) | UI §2 | `Message.tsx:107` |

---

## Spec deviations to reconcile (PLAN.md ↔ implementation)

From PRD §1 + Deliberate-deviations. Either adopt the locked-in lib or amend the spec:
1. **Tailwind v3, not v4** (`package.json:29`). 2. **No shadcn/ui/Radix** (→ also H1). 3. **No Shiki** (highlight.js used). 4. **No react-diff-view+refractor** (hand-rolled LCS, `DiffView.tsx:22`). 5. **`agent.md` at project root, not `.coding/agent.md`** (`project/mod.rs:47`).

Also: document the **beyond-spec features** in PLAN.md so the PRD reflects the shipped product — vision fallback, `spawn_agent` tool, backlog/Run-All, safety-rules system, stats/pricing, sub-plan stack, richer `AgentEvent` variants (PRD §"What exceeds the spec").

---

## Top actionable items (cross-perspective, ordered by leverage)

1. **Adopt shadcn/ui (Radix)** — closes H1 (accessibility) + the PRD shadcn deviation in one move; provides Dialog/Tabs/ScrollArea/focus-trap/ARIA/keyboard-nav for free.
2. **Fix `WorkflowStateInfo` type drift + remove `as never`** (H2) — latent runtime casing bug, small change, high confidence-safety payoff.
3. **Key `PendingApprovals` by `(agent_id, tool_call_id)`** (H4) — small structural fix that makes multi-agent approval correct-by-construction.
4. **Add Run-All loop tests** (H7) — the unattended git-commit/rollback path must be tested before it's trusted.
5. **Add first-run empty state + auto-show plan** (H6) — highest user-impact UX fix; the plan-first workflow's central artifact is currently buried.
6. **Delete the dead `async-openai` conversion layer** (M9) — removes redundant dead code + a transitive dep. *(Note: this was originally listed as H5/High on the claim that `build_request_json` was untested; the report-of-reviews audit proved that false — `build_request_json` is tested — so it's now Medium dead-code cleanup, no test backfill needed.)*
7. **Memory connection pool / WAL read concurrency** (H3) — the multi-agent throughput cliff; defer until multi-agent is a real workload.
8. **Split `AgentLoop` god-object** (M1) — the highest evolvability payoff; defer until next brain change touches the loop.

---

*Consolidated from four independent reports. No project source files were modified to produce this consolidation; all four source reports + this summary are under `.coding/reviews/`.*