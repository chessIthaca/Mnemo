# Phase 2a FE Streaming Perf Quick Wins — Architecture/Code Review

**Reviewer**: read-only subagent (per task)  
**Date**: 2026-08-11  
**Scope**: Phase 2a (Perf H3/M1, UI L1/L2) — rAF-batch reasoning_delta + tool_call_arg_delta (extend useAgentEvents buffering + new batched store actions); narrow Zustand selectors (App.tsx activeState, MainPanel.tsx tabs + derive otherApprovals with useShallow); smart stick-to-bottom scroll (Conversation.tsx listener + stick flag, preserve 100ms + existing deps).  
**Key files inspected**: frontend/src/hooks/useAgentEvents.ts, frontend/src/hooks/useAgentStore.ts, frontend/src/App.tsx, frontend/src/components/layout/MainPanel.tsx, frontend/src/components/chat/Conversation.tsx.  
**ALL uncommitted changes reviewed**: via `git status`, `git diff HEAD`, targeted `file_read` + `search` on sources + .coding files.

## Changed Files (exactly, from `git diff --name-only HEAD`)
- .coding/backlog.json
- .coding/plans/39f3c881-55a0-4147-a010-9199f6e83aec.md
- frontend/src/App.tsx
- frontend/src/components/chat/Conversation.tsx
- frontend/src/components/layout/MainPanel.tsx
- frontend/src/hooks/useAgentEvents.ts
- frontend/src/hooks/useAgentStore.ts

(Untracked .coding plan files also present in tree: .coding/plans/25d154a4-2361-4e71-9d5d-54c2f07c7308.md and .coding/plans/e9b20e19-a09f-425e-9cce-2f94bb422ec4.md.)

## Confirmations (per task spec)
- `import { useShallow } from 'zustand/shallow'` added (MainPanel.tsx only).
- New store actions: `appendStreamingReasoning`, `applyToolCallArgDeltas` (with JSDoc) that implement net effect of the old reducers but batched/rAF.
- No changes to per-event reducers (`reduceReasoningDelta`, `reduceToolCallArgDelta`, etc.) — they remain in useAgentStore.ts and are exercised by `applyAgentEvent` / tests / direct handleAgentEvent paths.
- No behavior change for text path (text_delta buffering + appendStreamingText unchanged).
- Flush on any non-delta: yes (else branch in dispatch always calls `flushBuffers()` before `handleAgentEvent`).
- Arg delta map cleanup: present (flushBuffers clears inners + deletes empty agent maps; clearStreamingBuffer deletes; early-return on empty deltas).
- Selector identity stability: useShallow on tabs (minimal metadata array); narrow per-agent selects in App/MainPanel return stable slices until that agent mutates.
- Scroll flag not causing missed scrolls on new content: gates correctly (only when stickToBottomRef); programmatic scroll re-sets flag; listener (passive) updates on user scroll.
- No new broad subscriptions (full `agents`/`agentNames` removed from App + MainPanel; replaced by narrow + shallow tabs).
- No use of sleep/polling (rAF, existing throttle setTimeout, passive scroll listener only).
- Line-ending style preserved (source .ts/.tsx diffs clean; CRLF warnings limited to .coding json/md).
- Public doc comments: present on new actions (interface + impl); updated docs on BufferHandle/clearStreamingBuffer.
- Testability: reducers untouched so existing unit tests (e.g. preview.test.ts exercising deltas) unaffected; batched paths only used from events hook.
- Constitution compliance (in scope): no main-branch commits, bookkeeping tools autorun (plan/backlog edits), public fns documented, prior cargo test per plan context; no drive-by refactors outside perf quick-win scope.

## Findings

### Critical
(no findings)

### High
(no findings)

### Medium
- **frontend/src/components/layout/MainPanel.tsx:130** (and surrounding): Other-approvals banner no longer shows the pending tool name (`${toolName}` suffix). Prior code pulled `otherApproval.st.pendingApproval.toolName`; narrowing intentionally drops it (see comment at 132-137: "We don't have the toolName in the narrow metadata"). Results in slightly reduced UX/info for non-active pending approvals (always empty suffix now). Not a text-path change, but a visible side-effect of the selector narrowing.
- (Note: the IIFE ternary always returns `""`; this is the mechanism for the drop.)

### Low
- **frontend/src/components/layout/MainPanel.tsx:131**: Unnecessary immediately-invoked function expression (`(() => { return ""; })()`) inside the JSX conditional for the tool-name suffix. Could be simplified to literal `""` (or conditional) without changing behavior. Minor readability.
- **.coding/backlog.json + .coding/plans/39f3c881-...md + untracked plans**: Bookkeeping/plan state updates are present in the uncommitted working tree (as expected from plan execution). They are outside the FE perf scope but must be included per "inspect ALL uncommitted changes". No correctness impact on reviewed code.
- **frontend/src/components/chat/Conversation.tsx:74**: Scroll effect deps kept exactly as specified ("existing deps"). Note that `streamingReasoning` is absent (was already absent pre-change); pure reasoning streams therefore do not trigger the auto-scroll effect (behavior preserved, but may be observable if reasoning produces visible content before text).
- No other issues: missed non-delta flush, arg cleanup, selector stability, scroll flag correctness, broad subs, sleep/poll, line endings, docs, reducer preservation, or testability.

## Overall Assessment
The changes correctly implement the scoped Phase 2a quick wins with the required buffering extension, narrow selectors (useShallow + derived metadata), and smart stick-to-bottom logic. Reducers and text path are untouched. One Medium finding is a minor approved-scope UX regression in the approval banner (tool name omitted to keep selectors narrow). All other checks (flush, cleanup, identity, no broad subs, no polling, docs, constitution) pass cleanly. No Critical/High bugs or violations.

Report written to .coding/reviews/2026-08-11-phase2a-streaming-perf-review.md (this file). Reviewer performed no source edits.
