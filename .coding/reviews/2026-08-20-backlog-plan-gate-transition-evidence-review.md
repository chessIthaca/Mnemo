# Review — Plan-loop gate transition evidence (resting-Complete hole)

**Date:** 2026-08-20 · **Branch:** feat/backlog-plan-gate · **Reviewer:** reviewer subagent
**Scope:** ALL uncommitted changes per `git status` / `git diff HEAD`, ignoring `.coding/**`
bookkeeping. Reviewed files: `src-tauri/src/ipc/events.rs`,
`src-tauri/src/ipc/run_all.rs`, `PLAN.md`. (Also in the diff: `.coding/backlog.json`,
`.coding/plans/stack.json`, untracked `.coding/plans/4bc070c9*.md` — bookkeeping, not
reviewed.)

**What the change does:** tightens the mandatory plan-loop gate so `Done` additionally
requires an observed `WorkflowStateChanged` for the main agent during the turn
(`Complete` is a resting state; a freeform/ask_user turn that never planned used to
pass the old state-only check). Implemented via `TurnResolveLatch::workflow_changed`
in events.rs, consumed as `loop_evidence` by `on_main_turn_resolved` in run_all.rs.

---

## Findings

### 1. BUG (correctness — the gate's core guarantee can still be violated): failed workflow tool calls emit `WorkflowStateChanged` and count as false evidence

- **Root cause (pre-existing, but this diff is the first consumer that depends on it):**
  `src/agent/turn.rs:1312–1352` — the event-emission block is gated only on the tool
  **name** (`tc.name == "create_plan" || … "abandon_skill"`), **not on
  `result.success`**. The comment says "If this was a workflow tool that succeeded",
  but the code never implemented the "succeeded" part. A workflow tool call that
  **fails** (rejected by the state gate, bad arguments, or filtered/hallucinated)
  still emits `AgentEvent::WorkflowStateChanged { state: <unchanged current state> }`.
- **New consumer:** `src-tauri/src/ipc/events.rs:207–220` — the forwarder now calls
  `turn_resolve.note_workflow_changed(agent_id)` on **every** such event, and
  `on_main_turn_resolved` treats it as proof the loop ran
  (`run_all.rs:403–404`, `run_all.rs:522–523`).
- **Reopened hole (the exact user-reported case):** agent rests in `Complete` →
  backlog turn starts (`Started` clears evidence) → the agent calls a workflow tool
  that fails **without changing state** — reachable with tools that are *visible* in
  the Complete filter (`src/tool/mod.rs:285–290`), e.g. `create_plan` with empty
  steps (`src/workflow/mod.rs:355–359` errors before the transition) or `skill_start`
  with an unknown skill — the event fires with `state: Complete` → evidence = true →
  turn ends `Finished` → gate sees `(Some(Complete), true)` →
  `plan_loop_allows_done(Complete, true)` = true → item marked **Done** +
  `commit_success`, with zero work performed. Identically exploitable in the
  single-dispatch/auto-feed branch (`run_all.rs:522–532`). The
  `RUN_ALL_STEER` text ("call finish … before ending your turn") makes a doomed
  workflow-tool call from a confused model plausible, not theoretical.
- **Fix suggestion (minimal, matches the comment's stated intent):** gate the
  emission in `src/agent/turn.rs` on `result.success` (a failed tool never changed
  state, so suppressing the event is also strictly more correct for the UI), or —
  belt-and-braces in the forwarder — only `note_workflow_changed` when `new_state !=
  prev_workflow_state.get(&agent_id)` (note: first-ever observation has `prev = None`;
  combining both is safest). With success-gating, from resting `Complete` the only
  successful workflow tools are `create_plan` (→ Executing → correctly rejected +
  rolled back) and `skill_start` (→ Skill), so `(Complete, true)` then genuinely
  implies an excursion out of Complete and back.
- **Regression test to add with the fix:** a failed workflow tool call must NOT set
  gate evidence (turn-level test that a failed `create_plan`/`skill_start` emits no
  `WorkflowStateChanged`, or a forwarder-level test that a no-op/unchanged-state
  event is not noted).

### 2. MINOR (docs / overstated claim): "the tree is provably clean" in the resting-Complete rationale is too strong

- `src-tauri/src/ipc/run_all.rs:34–36` (module doc) and `run_all.rs:413–418` (arm
  comment) justify **no rollback** for `(Complete, false)` with "Complete has no
  write tools, so the tree is provably clean". The Complete filter
  (`src/tool/mod.rs:266–292`) indeed hides all project-source write tools
  (file_write/edit/append, shell, git) — but **memory tools are always visible in
  every filter** (`Memory => true`) and **`spawn_agent` is visible from Complete**;
  reviewer subagents may call `write_review_report`, which writes under
  `.coding/reviews/` — a **tracked** directory in this repo. So tracked
  bookkeeping files can still change during a resting-Complete turn.
- **Consequence is benign** (the dispatch-time checkpoint commits the tree; leftover
  bookkeeping dirt just persists into the next item's checkpoint; the note makes no
  false rollback claim — the review-finding-3 rule is respected), but the rationale
  should say "no project-source writes" rather than "provably clean". Suggest
  rewording the two comments; no behavior change required.

### 3. OBSERVATION (accepted edge, flagged as requested): skill excursion Complete → Skill → Complete counts as Done

- Verified real: `start_skill` always sets `state = Skill`
  (`src/workflow/mod.rs:621`) and `end_skill` returns to the skill's `target_state`
  (`mod.rs:633`), so a skill run from Complete inevitably produces observed
  transitions and can end back in Complete → `(Complete, true)` → **Done** without
  any plan loop. This matches the user's literal requirement ("a CHANGE in state
  and get back into the done state") and is documented in the
  `plan_loop_allows_done` doc (`run_all.rs:99–105`).
- **Risk assessment: acceptable, not flagged as a defect.** A skill's tool
  allow-list is curated (`ToolFilter::Skill`), skills are deliberate user-visible
  artifacts (e.g. merge_to_main, which commits by design), and toolbar-started
  skills already work this way outside Run-All. If Run-All ever needs to guarantee
  plan-loop-only completion, it would need to distinguish `skill_start` events from
  plan transitions — out of scope for this change.

### 4. OBSERVATION (tests): compile-level pin is acceptable; latch test is sound

- `plan_loop_rejects_resting_complete_without_transition`
  (`run_all.rs:181–191`) asserts the exact hole
  (`(Complete, false) == false`). It would not compile against the pre-change
  1-arg `plan_loop_allows_done`. A true behavior-level pin would require driving
  `on_main_turn_resolved`, which needs a Tauri `AppHandle` + `IpcState` (not
  unit-testable as structured). Given that constraint, the signature change plus
  the explicit `(Complete,false)`/`(Complete,true)` assertions is a reasonable
  pin, and `workflow_changed_tracks_transitions_per_turn`
  (`events.rs:712–726`) covers the evidence lifecycle (default false, note→true,
  per-agent independence, `on_started`/`on_exited` clears). Sound.
- Nit: the test name `plan_loop_allows_done_only_when_complete`
  (`run_all.rs:155`) now undersells the AND-evidence requirement — cosmetic only.
- Nit: arm 1's tuple already binds `true` and the guard re-passes the literal
  (`run_all.rs:404`, `523`) — redundant but harmless and arguably clearer.

---

## Verified clean (no findings)

- **Event ordering / no stale leak, no premature wipe.** Exactly one send site for
  `WorkflowStateChanged` exists (`src/agent/turn.rs:1344–1352`), inside the
  per-tool-result loop; `Started` is emitted at the very top of the turn
  (`turn.rs:87–89`) before any provider call. The forwarder is a single task over
  one FIFO mpsc channel, so per turn: `Started` (clears) → in-turn transitions
  (sets) → `Finished`/final `Error` (reads at resolution). The next item's `Prompt`
  is only enqueued from inside `on_main_turn_resolved` (i.e. from within the
  `Finished` handler), so its `Started` is processed strictly after the evidence
  read — no wipe-before-read. No emission site exists outside a turn, so no
  between-turns leak. Deferred-failure flush (`events.rs:599–622`) reads
  `workflow_changed(main_id)` at the same resolution moment.
- **Subagent pollution: impossible.** The latch is keyed by `AgentId`;
  `note_workflow_changed` uses the event's own agent id; every gate read uses the
  main agent's id (`events.rs:296,327,336,618`). Subagents additionally cannot
  create plans (plan tools are main-only), so their own workflows stay
  Planning/read-only — they cannot generate writes on main's behalf either.
- **Match-arm ordering (both branches).** `(Some(s), true) if allows(s, true)`
  captures only Complete → Done. `(Some(Complete), _)` is reachable only with
  evidence=false (the true case always passes arm 1's guard) → CantResolve/Failed,
  **no rollback** — correct per the no-write-tools rationale (see finding 2's
  caveat). `(Some(other), _)` correctly catches evidence=true + non-Complete
  (rollback + `plan_gate_note`) and everything else. `(None, _)` → unverifiable,
  no rollback, run halted. Single-dispatch branch mirrors this with
  Failed-instead-of-CantResolve, consistent with each path's pre-existing
  convention.
- **Borrow/semantics at call sites.** `on_finished` / `on_final_error` /
  `flush_deferred_main_failure` return an owned `ResolveAction` (the `&mut` borrow
  ends at the call); the arms then take a fresh shared borrow for
  `workflow_changed(...)`. Evidence is read in the same loop iteration as the
  terminal event — the resolution moment, before any later event can clear it.
- **No-rollback safety core.** `ToolFilter::Complete`
  (`src/tool/mod.rs:266–292`): Agent tools only when `AutoRun` (read-only) or
  `spawn_agent`; Browser only `AutoRun`; Workflow only
  `create_plan`/`skill_start`/`ask_user`/`current_plan` �� no project-source write
  tools. A skill run from Complete necessarily transitions to `Skill`
  (`workflow/mod.rs:621`), so `(Complete, false)` genuinely implies no skill ran
  and no source writes occurred (modulo finding 2's bookkeeping caveat).
- **Constitution compliance.** Doc comments present on every new/changed item
  (latch field + both methods in events.rs; `loop_evidence` param + updated module
  and fn docs in run_all.rs). Regression tests added in both files. Task states
  `cargo build`/`cargo test` passed under `#![deny(warnings)]` (reviewer is
  read-only; the diff shows no dead code or unused imports — `loop_evidence` is
  consumed in both success branches). **Line endings:** byte scan for CR over
  `src-tauri/src/ipc/*.rs` (both changed files) and `PLAN.md` found **zero** CR
  bytes — the mid-session CRLF reintroduction in events.rs was fully renormalized;
  all three files are LF. `PLAN.md` accurately documents the tightened gate.
- **Security.** No IPC surface change: `on_main_turn_resolved` is internal to the
  ipc module (called only from events.rs); `plan_loop_allows_done` was already
  `pub`; the latch is forwarder-internal; no new untrusted input is parsed.

## Verdict

The design is sound and the event-ordering/borrow/no-rollback analysis holds up
under inspection, **but finding 1 must be fixed before commit**: the gate's
evidence signal currently counts failed workflow tool calls, which reopens the
exact resting-Complete hole the change was built to close. Findings 2 (doc
reword) and the finding-1 regression test should ride along in the same fix.
