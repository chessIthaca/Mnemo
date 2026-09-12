# Review: backlog_status tool (all states), structured tool errors, ask_user stuck fix

**Scope reviewed:** ALL uncommitted changes (`git status` / `git diff HEAD`), 2026-08-22.
Files: README.md, frontend/src/hooks/{agentEventReducer.ts, useAgentStore.ts, useAgentStore.test.ts},
src-tauri/src/ipc/{backlog_cmds.rs, run_all.rs}, src/agent/{factory.rs, prompt.rs, tests.rs, turn.rs},
src/backlog.rs, src/tool/mod.rs, src/tool/workflow/backlog.rs, plus .coding/* bookkeeping and the
untracked plan file.

**Verdict: 2 Low findings, no Medium/High. No security issues. Constitution-compliant apart from the
doc-sync gap in finding L2.**

---

## Findings

### L1 (Low — spec deviation / error-feedback quality): illegal-transition error does not list the per-status allowed set

`src/tool/workflow/backlog.rs` (`BacklogStatusTool::execute`, the `!store.transition(...)` error arm).

The plan specified: "illegal transition → ToolResult::error **listing allowed transitions from the
item's current status**". The implementation instead emits:

```
illegal backlog transition #7: pending -> pending (see the tool schema for the allowed
transitions from the current status)
```

It names the current and target statuses (good) but punts the allowed set to the schema description,
which contains the FULL table (all five rows), not the destinations legal from the item's current
status. Error-feedback quality was the explicit motivation of source item 78 ("the error needs to be
parsed by the llm so it can fix and change the way it calls the tool") — a per-status allowed list
(e.g. "allowed from pending: in_flight, done, failed, cant_resolve") is directly machine-actionable;
"see the schema" makes the model re-filter the full table itself. Low severity because the schema
text is in context and recovery is still possible. Fix is small: derive the allowed destinations from
`current.status` (one match) and include them in the message.

### L2 (Low — constitution: documentation sync): stale doc comments describing the OLD pendingQuestion clearing mechanism

The reducer change (agentEventReducer.ts:749-757) makes `recordQuestionAnswer` itself clear
`pendingQuestion`; the `started` event is no longer the clearer of an answered question. Two doc
comments still describe the old contract:

- `frontend/src/components/chat/QuestionPrompt.tsx:26-31` — "The `pendingQuestionAnswered` marker
  (set on confirmed answer) is what hides the prompt immediately, so it doesn't linger until the
  backend's `started` event clears `pendingQuestion`." After this change the prompt unmounts via
  Conversation.tsx:120's `state.pendingQuestion &&` gate the moment the answer is recorded; the
  marker is now belt-and-suspenders, not "what hides the prompt".
- `frontend/src/hooks/agentState.ts:163-168` — "`pendingQuestionAnswered` … Lets the live
  `QuestionPrompt` collapse into a static Q→A view immediately (before the backend's `started`
  event clears `pendingQuestion`)." Same staleness; also QuestionPrompt renders `null` when answered
  (the `qa` transcript entry is the static view), so "collapse into a static Q→A view" is loose.

The project's documentation-sync rule treats stale module doc comments after a behavior change as a
finding. Comments only — no behavior impact. (agentEventReducer.ts and useAgentStore.ts docs WERE
correctly updated; these two files were missed.)

---

## Verified correct (no findings)

**Transition table (src/backlog.rs:183-209).** The match arms implement exactly the documented rows:
Pending→InFlight; Pending→Done/Failed/CantResolve; InFlight→Done/Failed/CantResolve/Pending;
terminal→Pending; all else (same-status, terminal→non-Pending, unknown id) refused with no mutation
and no persist. Doc comment matches the code. `set_status` retained with a "production callers must
use transition" note; `requeue` refactor is semantics-preserving (same guard, same persist, note
cleared) and its stricter-than-transition contract is documented. Tests cover every legal row +
persistence and every illegal row + unknown ids.

**Harness reroute — same from/to semantics at every reachable site.**
- `run_all_dispatch_next`: item comes from `next_pending()` (Pending), so checkpoint-fail
  Pending→Failed (run_all.rs:418) and dispatch Pending→InFlight (:438) are legal; the deferral
  (:471) runs only after the item was set InFlight lines above, so InFlight→Pending is legal.
- `on_main_turn_resolved` run-all branch (:573, :586, :613, :626, :646): the item is InFlight
  (marked at dispatch; tracked via `current_item`), so Done/CantResolve targets are legal. The one
  theoretical double-stamp (halt marks Failed, resolution re-stamps) is NOT reachable:
  `halt_run_all_for_approval` calls `end_run` (run_all.rs:78-81 clears run_all to None), so the
  later resolution takes the single-dispatch path where `single_in_flight` is 0 for run-all items.
- `backlog_stop_all` sets only the stop flag, so a stop-requested resolution still sees an InFlight
  item — InFlight→Done/CantResolve legal.
- Single-dispatch resolution (:718): `single_in_flight` is set only in `dispatch_item` after a
  successful dispatch; InFlight→Done/Failed legal.
- `halt_run_all_for_approval` (:773): item is InFlight (or id 0 → unknown-id no-op);
  InFlight→Failed legal.
- `backlog_cmds.rs:285`: `dispatch_item` only receives items from `next_pending()` /
  `pending_item(id)` (both Pending-filtered), so Pending→InFlight is legal.
- The only semantic deltas vs. `set_status` are in TOCTOU corners (user mutating the item from the
  UI during the multi-second checkpoint window); there the guarded no-op is arguably safer than the
  old silent stomp. Observation only.

**backlog_status tool.** Name/category/safety correct (Workflow + AutoRun, sanctioned bookkeeping
like backlog_add). Schema enum strings match `BacklogStatus`'s `#[serde(rename_all = "snake_case")]`
(pinned by the existing `status_serializes_snake_case` test). Reads the item first; unknown id →
clear "does not exist" error; success fires `on_changed` exactly once after dropping the store lock;
returns data {id, status}. Tests: name/category/safety, legal transition + persistence + note,
illegal → error + no persist, unknown id, notifier fires only on success. Registered in
factory.rs:725-732 inside the same backlog-wiring gate as backlog_add; factory test tool list
updated.

**All-state visibility (src/tool/mod.rs).** Added to the Workflow arms of Planning, Executing,
Reviewing, Complete, and to the Skill arm's always-available set next to ask_user/current_plan —
matching the explicit user requirement. The test registers the REAL tool and asserts visibility in
all five filters, including `Skill(vec![])`. The test store path "test-backlog-never-written.json"
is safe: `BacklogStore::open` only reads (persist happens solely in mutating methods), so no
working-tree pollution — same pattern as the pre-existing backlog_add registration above it.

**`[tool error]` wrap (src/agent/turn.rs:1382-1404).** Applied only to `!result.success`; successes
flow verbatim. Denial safety verified end-to-end: MAX_RETRIES counting and
`is_user_denial_tool_output(&result.output)` run at :1327-1333 on the RAW output BEFORE the wrap;
the classifier (:1658-1664) is `.contains()`-based so even history-side substring scans survive the
prefix; the frontend's `isUserDenialToolOutput` (agentEventReducer.ts:95-103, substring-based,
unchanged) consumes `event.result.output` from the `ToolResult` event, which carries the raw
`result.clone()` (:1411), not the wrapped message. Regression tests exist in BOTH directions:
`unknown_tool_error_recovery` now asserts the prefix, and the new
`successful_tool_results_are_not_wrapped_in_tool_error_marker` asserts successes are unwrapped.
prompt.rs APP_RULES directive added and matches the compiled-prompt wording.
(Observation, not a finding: the two synthetic Tool-message paths — malformed-args JSON repair at
turn.rs:1127-1140 and interrupted-not-run at :1241-1248 — carry no marker. Neither is a
tool-execution result; both predate the change and are outside the plan's stated scope.)

**ask_user stuck fix (frontend).** Reducer clears `pendingQuestion` + `freeformQuestionId`, sets
`pendingQuestionAnswered`, appends the `qa` entry. Consumers verified: Conversation.tsx:120 unmounts
the prompt immediately on answer; a SECOND ask_user in the same turn re-sets `pendingQuestion`
(reduceUserQuestion:713) and QuestionPrompt's hide check is id-specific
(`answeredId === question.questionId`, QuestionPrompt.tsx:62), so the new question renders live —
no stuck-hidden and no double-clear hazard (Started re-clearing is idempotent). InputBar freeform
mode requires both fields, cleared together. InflightBar's "waiting for your answer…" label clears —
the fix's intent. useAgentStore.ts changed ONLY in the recordQuestionAnswer doc comment (verified in
the diff). Tests updated: pendingQuestion asserted null after recordQuestionAnswer, and the
started-clearing path is still asserted.

**Security.** backlog_status touches only .coding/backlog.json through the shared guarded store;
args are id:u64 / status:enum / note:string (the note is persisted as data, never executed) — no
path/command-injection surface, no approval bypass. AutoRun is sanctioned bookkeeping.

**Constitution.** Public functions documented (`transition`, `BacklogStatusTool`, `::new`); no
`#[allow(...)]` added; regression tests present for all fixed defects; multi-platform neutrality
kept (portable Rust/TS only, no Windows-only APIs/paths/shell, no new cfg(windows)); README.md
bullet updated accurately; PLAN.md's backlog mentions (:46, :639-641) are generic and make no claims
this change falsifies. Exception: finding L2 above.

**Bookkeeping files** (.coding/backlog.json dropping source items 77/78/81, stack.json, the
untracked plan file): runtime/plan state, expected to ride along in the commit — not findings.

## Minor style observation (not a finding)

`BacklogStatusTool::execute` defines two near-identical `status_str` closures (one parameterized in
the error arm, one capturing `args.status` in the success arm) — could be one small helper. Purely
cosmetic; compiles clean under `#![deny(warnings)]` either way.
