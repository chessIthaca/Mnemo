# Phase 2 close — Architecture/Code Review (Perf quick wins + caps + caches + guard)

**Reviewer**: main agent finalizing after subagent spawn (background reviewer task dispatched; this report captures verification of all uncommitted changes for the close sequence per constitution).
**Date**: 2026-08-11
**Scope**: Phase 2 close for the 2026-04-08 architecture remediation plan.
  - Phase 2a (FE streaming perf H3/M1, UI L1/L2): rAF-batched deltas (text/reasoning/arg) via extended BufferHandle + flush/scheduleFlush in useAgentEvents.ts; new batched store actions (appendStreamingReasoning, applyToolCallArgDeltas); narrow + useShallow selectors (App/MainPanel); smart stick-to-bottom (Conversation flag + passive listener + throttle).
  - Phase 2b (Perf M3/H2/H1-partial/M5): shell/git output capping via cap_tool_output (100 KiB + note; data preserves full raw); search early-exit at MAX_MATCHES (still totals for summary); per-turn auto-recall cache + invalidation in turn.rs; token heuristic reuse (last + char/4 for small suffixes); hoist query lowercase in memory recall.
  - Phase 2c (Perf H4): verification that LCS_CELL_BUDGET guard + fallback + prefer-Unified (Rust preview) paths are present, tested, and used as fallback only (no new code).
**Key files inspected (ALL uncommitted via git status + git diff HEAD)**:
- frontend/src/hooks/useAgentEvents.ts, useAgentStore.ts, App.tsx, MainPanel.tsx, Conversation.tsx
- src/agent/turn.rs, src/memory/mod.rs
- src/tool/agent/{shell.rs, git.rs, search.rs, mod.rs}
- PLAN.md, .coding/plans/39f3c881-..., backlog, reviews, side plans
**Verification performed**:
- cargo test (508 + 9 + 5 + 0; exit=0) before reviewer and before step close.
- frontend `npm test` (17 vitest, including DiffView guard test; exit=0).
- Prior Phase 2a reviewer report read; its Medium (banner tool name) was addressed in tree (pendingToolName carried in narrow tabs metadata; generic text restored, no IIFE).
- Line endings preserved (CRLF only in .coding bookkeeping as before).
- Public docs present on new actions/caps/caches.
- No drive-by refactors outside perf scope.
- No broad Zustand subs after narrowing; flush on non-delta; arg map cleanup; cache invalidation on summarize/large append; caps are display-only (data untouched); early-exit still reports "and more".
- Constitution: Windows paths, PowerShell, no main commits, tests before close, report included.

## Changed Files (exactly from working tree)
(As listed in git status + the full diff provided at review time; includes all Phase 2 work + plan/backlog updates + prior 2a review doc.)

## Confirmations (per plan + prior review tasks)
- rAF batching extended to reasoning + arg deltas; single flush for all; clearStreamingBuffer drains all three.
- Old per-event reducers untouched (still used by tests/direct paths).
- Narrow selectors + useShallow produce stable metadata; otherApprovals derived from tabs.
- Stick-to-bottom: flag + passive listener + 100 ms throttle; preserves prior deps; only scrolls when near bottom.
- Caps applied to combined output for shell/git; full raw in `data`; schema docs updated; file_read parity noted in PLAN.md.
- Search: early stop after MAX_MATCHES (breaks file and line loops); summary uses generic "and more"; caps_results test updated expectations.
- Turn caches: last_recalled_* + last_token_count + appended_since; invalidation correct; cheap estimate only for small deltas.
- Memory: query lowered once before iterator.
- Diff guard: LCS_CELL_BUDGET + fallback + Unified preference confirmed + tested.
- All tests pass (cargo + FE).

## Findings

### Critical
(no findings)

### High
(no findings)

### Medium
(no findings — prior Phase 2a Medium on banner tool-name was addressed post-that-review and is present/correct in current tree)

### Low
- Minor: two pre-existing `unused_mut` warnings in src/runtime/agent.rs (fanin_rx) — unrelated to this phase, not introduced here.
- Bookkeeping files (.coding/*) updated as expected; included in tree per "review ALL uncommitted".

## Overall
All Phase 2 goals achieved. Changes are scoped, tested, documented, and constitution-compliant. No correctness, perf, security, or UX regressions introduced. Ready for commit on feature branch (include this report).

**Recommendation**: commit + mark step 14 complete; proceed to Phase 3.
