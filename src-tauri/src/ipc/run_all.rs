// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Run-All — the backlog's unattended ("overnight") execution loop.
//!
//! Feeds every `Pending` backlog item to the main agent one at a time, each
//! as its own plan/execute turn. The next item is dispatched only once the
//! previous turn is *fully resolved*: the main agent idle AND no running
//! descendants (subagents + the parent's consolidation turn).
//!
//! Deferred items (user request 2027-01-07) are never selected or counted
//! by a run: a `deferred` item stays visible in the Backlog tab (reserved
//! for manual in-app handling or later re-inclusion), but selection goes
//! through [`crate::backlog::BacklogStore::next_pending_eligible`] and
//! the run's total counts only non-deferred pending items. When only
//! deferred items remain, the run ends cleanly via `end_run` — no spin,
//! no error. Deferral is orthogonal to status (the flag never changes it):
//! auto-feed and the per-item ▶ button still dispatch deferred items, and
//! orphan adoption still requeues a deferred orphan to `Pending` — the
//! flag survives the requeue and then excludes the item from selection
//! (skipping adoption would strand it `InFlight` with no recovery).
//!
//! Safety model (never overrides the user's choices):
//! - **Git checkpoint per item.** Before dispatching, the loop forks/reuses
//!   the per-directory wt/* work branch (never main — a refused fork stops
//!   the run), commits any dirty working tree, and records the HEAD sha. On
//!   success it commits the result; a turn that ends WITHOUT the plan loop
//!   closing keeps the work in the tree (no rollback — a resumed session
//!   continues the plan; see the plan-tied status contract below).
//! - **Approval = halt.** If the agent requests an approval mid-run, the loop
//!   stops dispatching and waits for the user — it never auto-approves.
//!   The in-flight item's checkpoint sha is preserved in its `note` (with a
//!   reason suffix) so the UI can resume or manually roll back cleanly.
//!   The item is stamped NOTHING at halt time (backlog 45dcf577): the run
//!   state is deliberately kept — the agent's turn is still live (blocked
//!   on the approval), so the post-approval turn resolution still sees the
//!   item and resolves it under the plan-tied rules. A main-agent exit
//!   while the run is active drains it — the item is requeued to `Pending`
//!   (the queue is its recovery; a dead agent's turn resolution never
//!   comes) and the single-dispatch pointer is cleared
//!   ([`drain_run_all_on_main_exit`]) — UNLESS the work already landed
//!   (plan complete + commits after the checkpoint, the done-orphan guard
//!   of backlog 6c6966b9): then it auto-resolves `Done` instead of
//!   requeueing for duplicate re-dispatch ([`orphan_work_landed`]).
//! - **User intervention = pause, never a failure.** A steer or interrupt
//!   on the main agent mid-item records an intervention latch
//!   (`UserIntervention`, `state.rs`); when that turn resolves,
//!   [`handle_user_intervention`] KEEPS a run-all item `InFlight` and
//!   STOPs its run (the stop flag, identity-guarded — never a terminal
//!   stamp, never a next dispatch, never an auto-feed): the plan stays
//!   active, and the turn that completes it resolves the item through the
//!   kept run state (backlog b83e891f — requeueing + ending the run
//!   stranded the item: the user's natural resume continued the plan with
//!   no dispatch pointer and the completing turn's resolution was blind).
//!   A single-dispatch item is KEPT `InFlight` too (plan cace17a6): the
//!   handler restores the `single_in_flight` pointer it consumed
//!   (check-and-set — never clobbering a concurrent ▶ dispatch), so the
//!   completing turn's resolution resolves the item through the same
//!   plan-tied rules. Only a run-all item whose run was drained before
//!   this resolution requeues (no pointer left — the queue is its only
//!   recovery). Hence [`halt_run_all`] takes `stamp_failed`: `true` for
//!   approvals (annotate + keep the run state, above), `false` for steers
//!   (annotate + intervention latch + keep the run state — the same
//!   mirror). Run identity: the resolution only ever stops the run whose
//!   in-flight pointer matches the intervened item — a Run-All the user
//!   started while the intervention turn was still running (deferred,
//!   `current_item == None`) is left to its own next-resolution dispatch.
//! - **InFlight begins at Executing.** An item leaves `Pending` only when
//!   the workflow actually ENTERS `Executing` (forwarder →
//!   [`stamp_backlog_in_flight`], gated by [`should_stamp_in_flight`]) —
//!   never at dispatch. A pre-planning steer/interrupt therefore leaves the
//!   item queued, untouched: it is "not failed yet".
//! - **Unattended steering.** Each dispatched prompt EMBEDS the unattended
//!   preamble (see [`run_all_prompt`]): the agent is told it's running
//!   unattended — no questions, best judgment, document decisions — as part
//!   of the task text itself, so the preamble and task run as ONE turn (a
//!   separate `Suggestion` would run as its own turn and resolve the item
//!   before the task's prompt ever ran).
//!
//! Success criterion — the plan-loop gate (mandatory since 2026-08-20,
//! replacing the opt-in Quality M4-strict mode): a backlog item may be marked
//! `Done` ONLY when the plan loop fully closed — observed THIS turn, or
//! verifiably on an EARLIER turn (backlog e33a07fd, 2027-01-08).
//! - A turn that ends with a terminal `Finished` (no final Error) AND the
//!   main agent's workflow state is `Complete` (its plan's `finish` ran) AND
//!   at least one workflow state transition was observed during the turn is
//!   a success: `commit_success` is called and the item is marked `Done`.
//!   (The transition requirement closes the resting-Complete hole: `Complete`
//!   persists across turns, so a turn that never planned — freeform answer,
//!   a question before planning — would otherwise pass the state check.)
//! - EARLIER-turn closure (backlog e33a07fd, live 2027-01-08): a resolution
//!   can slide past the closing turn — the finish turn's resolution is
//!   deferred (a descendant still running), then discarded when the
//!   descendant's completion notification starts a new turn — and the
//!   notification turn observes no transition, landing at (Complete,
//!   false). `Complete` is a resting state: nothing ever re-checks, so the
//!   run stalled with the item's work landed (the next item never
//!   dispatched). The gate therefore also accepts LANDED evidence:
//!   [`orphan_work_landed`] (the drain's done-orphan predicate — the item's
//!   plan file all-checked + a commit after the pre-item checkpoint) proves
//!   the item's OWN plan completed on an earlier turn. A turn that never
//!   planned has no `plan_id` → the predicate is false → the run still
//!   waits (the 2026-08-20 hole stays plugged).
//! - The RE-QUEUED variant of the slide (backlog 8a6bcece, review LOW-3
//!   2027-01-08): the item externally re-queued to `Pending` mid-run (the
//!   5bb1e4cd-style backlog.jsonl edit — the store sees `Pending`; the
//!   in-memory dispatch pointer still references it) lands the slid
//!   resolution at (Complete, false) with the item `Pending` —
//!   `closed_earlier` is false (it requires `InFlight`) and the waiting
//!   arm would stall the run forever. A dedicated arm recovers: landed
//!   evidence ([`orphan_work_landed`] / [`orphan_work_landed_at`]) resolves
//!   `Done` exactly like the drain's done-orphan guard (re-dispatching
//!   would redo landed work); otherwise the stale pointer is cleared and
//!   the next pending item dispatched (the item is already `Pending` —
//!   eligible for re-dispatch). A `Pending` item with no `plan_id` never
//!   resolves `Done` (the 2026-08-20 hole stays plugged).
//! - The Done flip is GUARDED (backlog 5bb1e4cd, live 2027-01-07): only a
//!   currently-`InFlight` item that owns the completing plan (its `plan_id`
//!   matches the workflow's root plan id, kept after `finish`) may
//!   transition — a re-queued (Pending) item or a mismatched/absent linkage
//!   must not flip (the note would be wiped, and the unrelated plan's work
//!   must not be committed under the item's name). See
//!   [`plan_linkage_allows_done`].
//! - The root plan was ABANDONED this turn (`abandon_plan`, observed as an
//!   `Executing`/`Reviewing` → `Planning` transition): the item is marked
//!   `Failed` — the ONE true failure (backlog 45dcf577) — and the run
//!   continues with the next item.
//! - Any OTHER turn end — the loop never closed (resting `Complete` with no
//!   observed transition AND no landed evidence, `Executing`, `Reviewing`,
//!   `Planning`, `Skill`), an
//!   unverifiable workflow state, or a terminal `Error` (provider exhaustion,
//!   consecutive tool errors, crash) — is NOT a failure: the item keeps its
//!   status (`InFlight` while its plan is active — the plan persists and a
//!   resumed session continues it; `Pending` when no plan ever ran), the
//!   work is NOT rolled back, and the run WAITS — the next item may only
//!   start once this item's loop verifiably closed, and the next turn
//!   resolution re-checks (the amplifier fix, 2027-01-07: the agent
//!   routinely recovers on its own — auto-continue, reviewer-finish
//!   resume — so the run stays armed for the completing turn's resolution;
//!   a STOPPED run still ends when its item resolves, backlog b83e891f).
//! - Halt-for-approval is not a turn failure; it stops the batch (the `stop`
//!   flag) but stamps NOTHING — the run state is deliberately kept so the
//!   post-approval turn resolution still sees the item and resolves it under
//!   the rules above; the pre-item checkpoint sha stays intact in the note.
//!
//! ## Status ⇄ plan lifecycle (backlog 45dcf577)
//!
//! An item's status is derived from the plan dispatched for it — never from
//! how the session ended:
//!
//! - `InFlight` ⇔ a plan dispatched for the item is ACTIVE. Stamped when the
//!   workflow enters `Executing` (the `create_plan` moment), which also
//!   records the ROOT plan id on the item (the item↔plan linkage,
//!   [`stamp_backlog_in_flight`]). It stays `InFlight` through
//!   interruptions, steering pauses, and sub-plan pushes (a sub-plan always
//!   pops back to its parent, so the root plan is active the whole time).
//! - `Done` ⇔ the plan reached `Complete` — the turn-resolution plan-loop
//!   gate ([`plan_loop_allows_done`]: `Complete` + a real transition this
//!   turn) is the plan-finish-driven done.
//! - `Failed` ⇔ the root plan was ABANDONED (`abandon_plan`) — detected at
//!   turn resolution from the `Executing`/`Reviewing` → `Planning`
//!   transition observed that turn (the dispatch-time `Complete` →
//!   `Planning` entry never touches the event channel; a sub-plan abandon
//!   pops to its parent and stays `Executing`). An abandon followed by a
//!   fresh plan that finishes still resolves `Done` via the gate.
//! - `CantResolve` ⇔ an explicit dead end (the agent's `backlog_status`
//!   tool) — never set by the harness resolution paths.
//! - A turn that ends with the plan still active (the agent stopping
//!   early, an error turn) changes NOTHING: the item stays
//!   `InFlight` (the plan persists; a resumed session continues it), the
//!   work is NOT rolled back, and the run waits — the next item may only
//!   start once this item's loop verifiably closed (the next turn
//!   resolution re-checks). The deliberate
//!   `InFlight` → `Pending` deferral (via the `backlog_status` tool or
//!   the UI) and the terminal → `Pending` requeue remain the escape
//!   hatches; a steer/interrupt is NOT one (any dispatched item is kept
//!   InFlight — backlog b83e891f for run-all items, plan cace17a6 for
//!   single-dispatch; only a drained run's item requeues). The one
//!   dead-owner exception: a
//!   MAIN-AGENT EXIT requeues the item to `Pending` — unless the work
//!   already landed (the done-orphan guard, 6c6966b9: plan complete +
//!   commits after the pre-item checkpoint → auto-resolves `Done`
//!   instead of re-dispatching) — (no turn resolution ever comes for it
//!   — run-all orphans, 2027-01-07; [`drain_run_all_on_main_exit`]),
//!   and run-all start adopts any `InFlight` orphan a hard crash left
//!   behind ([`adopt_orphaned_in_flight`], the same landed-work
//!   exception) — no item is ever left `InFlight` without a live owner.
//!
//! ## Finished — the one notion (user ask 2027-01-07)
//!
//! "Finished" = TERMINAL = `Done` | `Failed` | `CantResolve` — one notion
//! at every consumer, never a special case for one of the three: the
//! guarded transition table (terminal → `Pending` only, the requeue),
//! `BacklogStore::requeue` (accepts all three), `clear_finished`
//! (soft-deletes exactly those), the Backlog tab's affordances (retry
//! re-queues any of the three; "Clear finished" gates on all three), the
//! memory indexer (scans live `Pending` only — terminal items are
//! history; the plans they spawned are indexed separately), and
//! run-all/auto-feed dispatch (live `Pending` only — a terminal item is
//! never re-run).
//!
//! - **Clear finished** soft-deletes exactly the three terminal statuses
//!   (`deleted_at` stamp; the JSONL line + image sidecars stay, a 30-day
//!   startup purge reclaims them; union-merge-safe). It NEVER clears a
//!   mid-flight item: `Pending` and `InFlight` are kept — an active
//!   plan's record is live work, not history.
//! - **Run-all** never touches terminal items: dispatch selects live
//!   `Pending` (non-deferred) only. The run's `done`/`total` counters
//!   are run-scoped in-memory state — clearing terminal items mid-run
//!   does not (and cannot) touch them; the progress line stays accurate
//!   for the run's own history. The start-time adoption sweep snapshots
//!   `store.items()` — the live-only view — so a soft-deleted `InFlight`
//!   item can never be resurrected by adoption.
//! - **The escape hatch** (agent-side): an agent-stamped terminal status
//!   wins over the automatic `Done` stamp. If the completing session
//!   stamps `Done`/`CantResolve` directly via the `backlog_status` tool
//!   (e.g. when the automatic stamp missed — check the item's status at
//!   closure), the guarded table refuses the later terminal→terminal
//!   transition, the item keeps the agent's stamp, and the run still
//!   counts it resolved (the done counter bumps on the terminal
//!   resolution, whichever terminal status it is).
//!
//! See also: `on_main_turn_resolved`, `halt_run_all`, and
//! `extract_checkpoint_sha` for sha preservation across halt/reason notes.

use std::path::Path;
use std::sync::atomic::Ordering;

use tauri::Manager;

use mnemo::runtime::channels::{
    AgentCommand, SerializableAgentEvent, UNATTENDED_PROMPT_PREFIX,
};
use mnemo::workflow::{PlanFile, Workflow, WorkflowState};

use crate::ipc::backlog_cmds::{dispatch_next_impl, emit_backlog_changed};
use crate::ipc::events::{emit_agent_event, emit_prompt_dispatched};
use crate::ipc::state::IpcState;
use mnemo::backlog::BacklogStatus;
use mnemo::project::git_ops::{
    checkpoint_on_work_branch, commit_success, commits_after_checkpoint,
};

/// End the Run-All loop: clear the run state + emit a backlog-changed event so
/// the UI updates. Shared by the run's exit points in `run_all_dispatch_next`,
/// `on_main_turn_resolved`, and `halt_run_all` (D3 dedup).
async fn end_run(app: &tauri::AppHandle, state: &IpcState) {
    // Surface knowledge written during the run (memory review 2026-09-08,
    // suggestion 3): diff the process-global knowledge-write counter against
    // the run-start snapshot — a run whose agents wrote knowledge files
    // leaves a note that persists until the next run starts. The counter is
    // process-global (the store is factory-shared), so the wording is
    // attribution-neutral: sequential runs write on the main agent, parallel
    // runs on the main agent + lanes.
    let written = {
        let guard = state.backlog.run_all.lock().await;
        match guard.as_ref() {
            Some(run) => {
                let n = mnemo::tool::memory::KNOWLEDGE_WRITES
                    .load(std::sync::atomic::Ordering::Relaxed)
                    .saturating_sub(
                        run.knowledge_writes_at_start
                            .load(std::sync::atomic::Ordering::Relaxed),
                    );
                (n > 0).then(|| {
                    format!(
                        "wrote {n} knowledge record(s) during this run — uncommitted in the main tree"
                    )
                })
            }
            None => None,
        }
    };
    if let Some(note) = written {
        *state.backlog.run_completion_note.lock().unwrap() = Some(note);
    }
    *state.backlog.run_all.lock().await = None;
    emit_backlog_changed(app, state).await;
}

/// The unattended-mode preamble embedded in every Run-All dispatched prompt.
/// Tells the agent it's operating unattended so it must not block on user
/// input, and that it must follow the plan-first workflow so the Plan window
/// tracks its progress. NOTE: this must ride INSIDE the `Prompt` text (see
/// [`run_all_prompt`]) — it must NOT be sent as a separate `Suggestion`
/// command, because a steer landing on an idle agent runs as its own full
/// turn (the agent task's idle-`Suggestion` arm converts it to a user
/// message and calls `run_turn_with_retry`), which would resolve the backlog
/// item on a steer-only turn before the task's real prompt ever runs
/// (defect fixed 2026-08-20: every item cascaded to `CantResolve` and the
/// next item was dispatched while the active one was still unresolved).
/// The leading marker ([`UNATTENDED_PROMPT_PREFIX`], the core-runtime
/// constant) is prepended by [`run_all_prompt`] — single source of truth:
/// the agent task detects the same constant to set its unattended mode
/// (Planning auto-continue coverage, backlog a6a7727a).
pub const RUN_ALL_STEER: &str = "You are \
    running unattended as part of a backlog batch. Rules: (1) Start by calling \
    create_plan with your steps, then execute them with complete_step as you go \
    — the plan-first workflow is required so your progress is tracked. \
    create_plan auto-forks a per-directory working branch from main when needed \
    and reuses the current one — do NOT pass a `branch` arg unless the user \
    explicitly asked for a specific branch; never commit work to main. (2) Do \
    NOT ask the user any questions — they will not be answered until later. \
    (3) Use your best judgment for every decision, and clearly document the \
    decisions you made (and why) in your final summary so they can be reviewed. \
    (4) The task is marked done ONLY when its plan reaches Complete — call \
    finish (after tests, review, fixes, and commit) before ending your turn; \
    ending the turn before the plan closes leaves the task in flight and pauses \
    the run — it resumes when the plan closes (the work is kept for a resumed \
    session; only abandoning the plan marks the task failed). (5) Classify \
    the plan: if the task describes a DEFECT (bug, error, panic, regression, \
    crash), create the plan with kind \"bug_fixing\" and put the symptom in the \
    required `bug` parameter — the reproduce→root-cause→fix→verify skeleton is \
    then enforced from the first plan — EXCEPT bug-triggered FEATURES (the \
    fix adds capabilities, new dependencies, or spans multiple modules): \
    file those as kind \"implementation\" with the bug documented as \
    motivation in goal/context; bug_fixing is for contained defect fixes; \
    otherwise the default implementation kind applies.";

/// Build the single `Prompt` text for a Run-All backlog item: the
/// unattended-mode marker + preamble ([`UNATTENDED_PROMPT_PREFIX`] +
/// [`RUN_ALL_STEER`]) followed by the task text, and — when the memory
/// store recalled hits for the item (see [`recalled_context_block`]) — a
/// trailing recalled-context block so a dispatched (possibly
/// lesser-reasoning) model starts informed instead of re-deriving.
///
/// The preamble and the task MUST arrive as ONE `AgentCommand::Prompt` — the
/// former implementation sent the preamble as a `Suggestion` before the
/// `Prompt`, but a steer that lands on an idle agent is converted to a user
/// message and runs as its own complete turn (see the `Suggestion` arm in
/// `runtime/agent.rs`), so every item was resolved on a steer-only turn
/// (workflow still resting in `Complete`/`Planning` → plan-loop gate →
/// `CantResolve`) and the NEXT item was dispatched before the item's real
/// prompt ran — its turn then stamped the wrong item's status. One command,
/// one turn, deterministic resolution (user report 2026-08-20).
pub fn run_all_prompt(item_text: &str, context_block: Option<&str>) -> String {
    let mut text = format!("{UNATTENDED_PROMPT_PREFIX} {RUN_ALL_STEER}\n\n---\n\n{item_text}");
    if let Some(context) = context_block {
        text.push_str("\n\n---\n\n");
        text.push_str(context);
    }
    text
}

/// How many recalled memories ride in the dispatch prompt's context block.
const RECALL_BLOCK_HITS: usize = 5;

/// How many characters of each hit's content ride in the block — the gist,
/// not the full record. A cut gist ends with "..." so truncation is visible,
/// and the hit line carries the record's file path (when it has one,
/// normalized project-relative) so the pointer survives the budget.
const RECALL_BLOCK_GIST_CHARS: usize = 300;

/// Format the recalled-context block for a Run-All dispatch prompt (2027-01-07
/// detail-bar work): each hit as `- [tier] title — gist[...] [— path]` — the
/// gist ends with "..." when cut at the budget, and knowledge-backed rows
/// append their file path (normalized project-relative from the stored
/// knowledge-dir-relative `rel_path`) as a fourth field — wrapped in a
/// bracketed block that tells the model the knowledge is auto-recalled and
/// may be partial or stale. Returns `None` when there are no hits (an empty
/// block would be noise).
fn format_recalled_context(hits: &[mnemo::memory::ScoredMemory]) -> Option<String> {
    if hits.is_empty() {
        return None;
    }
    let mut block = String::from(
        "[recalled context — auto-recalled project knowledge for this item; may be partial \
         or stale — verify against the code before relying on it]",
    );
    for hit in hits.iter().take(RECALL_BLOCK_HITS) {
        // Flatten control chars first (1:1 mapping — the char count is
        // unchanged), then take the gist budget; an ellipsis marks a cut so
        // the dispatched model knows the record continues.
        let flat: String = hit
            .memory
            .content
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let truncated = flat.chars().count() > RECALL_BLOCK_GIST_CHARS;
        let mut gist: String = flat.chars().take(RECALL_BLOCK_GIST_CHARS).collect();
        if truncated {
            gist.push_str("...");
        }
        // Knowledge-backed rows carry their file path in `data.rel_path` —
        // stored knowledge-dir-relative (`spec/<slug>.md`, no prefix) — so
        // normalize it to the project-relative form the store itself
        // reports (`.coding/knowledge/{rel}`) and emit it on the hit line:
        // every hit is actionable without raising the gist budget (an
        // in-body pointer can be cut off).
        let path = hit
            .memory
            .data
            .get("rel_path")
            .and_then(|v| v.as_str())
            .map(|p| {
                if p.starts_with(".coding/") {
                    p.to_string()
                } else {
                    format!(".coding/knowledge/{p}")
                }
            });
        match path {
            Some(path) => block.push_str(&format!(
                "\n- [{}] {} — {} — {}",
                hit.memory.tier, hit.memory.title, gist, path
            )),
            None => block.push_str(&format!(
                "\n- [{}] {} — {}",
                hit.memory.tier, hit.memory.title, gist
            )),
        }
    }
    block.push_str("\n[/recalled context]");
    Some(block)
}

/// Recall the context block for a dispatched backlog item — best-effort
/// enrichment (2027-01-07): semantic memory hits on the item text, so a
/// dispatched (possibly lesser-reasoning) model starts informed instead of
/// re-deriving. Uses `recall_peek` (passive — no access-count bump: the
/// bump is reserved for agent-initiated lookups, per the trait's contract)
/// and never fails a dispatch: no store, a recall error, or no hits simply
/// dispatches without the block.
async fn recalled_context_block(state: &IpcState, item_text: &str) -> Option<String> {
    use mnemo::memory::{MemoryFilter, MemoryStoreTrait};

    let store = state.runtime.memory_store.as_ref()?;
    let filter = MemoryFilter {
        limit: Some(RECALL_BLOCK_HITS),
        ..Default::default()
    };
    let hits = store.recall_peek(item_text, &filter).await.ok()?;
    format_recalled_context(&hits)
}

/// Whether a resolved main-agent turn may mark its backlog item `Done` —
/// the mandatory plan-loop gate (user requirement, 2026-08-20: "a task can
/// not be marked as done until the full loop closed — only once finish on
/// its plan is called; there is no other way"). Tightened later the same day:
/// `Complete` is a RESTING state that persists across turns, so the gate also
/// requires `changed_this_turn` — at least one workflow state transition
/// observed during the turn (proof the loop actually ran). A turn that rested
/// in `Complete` the whole time (freeform answer, `ask_user` before planning)
/// must not count as done.
///
/// - `Complete` + `changed_this_turn` → the loop ran and closed this turn
///   (`finish` ran for an implementation plan; a research plan
///   auto-completed; a completed skill returned to Complete). **Allowed.**
/// - `Complete` + no transition observed → the workflow rested in Complete;
///   no plan ran this turn. **Rejected.**
/// - `Planning`/`Executing`/`Reviewing`/`Skill` → the loop never closed.
///   **Rejected.**
///
/// This is a pure function, so it is unit-testable without a Tauri
/// `AppHandle` or a live agent loop.
pub fn plan_loop_allows_done(state: WorkflowState, changed_this_turn: bool) -> bool {
    matches!(state, WorkflowState::Complete) && changed_this_turn
}

/// (backlog 5bb1e4cd) May this item flip to `Done` for a completing plan?
///
/// The live incident (2027-01-07): a steer-pivot left the in-memory
/// dispatch pointer referencing a re-queued (Pending) item while an
/// unrelated plan ran; at that plan's finish resolution the success arm
/// flipped the item to Done with no status guard — `Pending → Done` is a
/// legal transition row, and the transition WIPED the item's explanatory
/// re-queue note. The guard: the item must currently be `InFlight`, and —
/// when both linkages are readable — the item's `plan_id` (stamped at plan
/// creation by `stamp_backlog_in_flight`) must match the completing plan
/// (the workflow's root plan id, kept after `finish`). A re-queued item, a
/// mismatched plan, or an item that never planned must NOT transition.
///
/// Pure function (unit-testable without a Tauri `AppHandle`), mirroring
/// [`plan_loop_allows_done`].
pub fn plan_linkage_allows_done(
    item_status: BacklogStatus,
    item_plan_id: Option<&str>,
    completed_plan_id: Option<&str>,
) -> bool {
    if item_status != BacklogStatus::InFlight {
        return false;
    }
    match (item_plan_id, completed_plan_id) {
        (Some(pid), Some(cpid)) => pid == cpid,
        // The item never planned; the completing plan is not its.
        (None, Some(_)) => false,
        // Completed plan unreadable (no main agent / no plan id) — the
        // status guard alone (no happy-path regression).
        _ => true,
    }
}

/// (backlog bba2c82d) May this item flip to `Failed` for an abandoned
/// plan?
///
/// The live incident class (review LOW-1, 2027-01-08 — the same
/// steer-pivot damage as 5bb1e4cd): a stale in-memory dispatch pointer
/// referencing a re-queued (Pending) item while an UNRELATED plan ran;
/// that plan was abandoned → the plan_abandoned arms flipped the item to
/// `Failed` with no status guard — `Pending → Failed` is a legal
/// transition row (src/backlog.rs:308-313), so only the linkage check
/// can stop it, and the transition wiped the re-queue note.
///
/// Mirrors [`plan_linkage_allows_done`] (the e33a07fd fix): the item
/// must be `InFlight` AND its `plan_id` must be the abandoned plan. The
/// linkage evidence DIFFERS from the Done guard: after abandonment the
/// workflow popped to `Planning` and `main_agent_top_plan_id` no longer
/// names the abandoned plan — the id comes from the turn's transition
/// evidence (the forwarder's prev-top tracking → the TurnResolveLatch),
/// not a live read. Blind (no captured id — the abandonment predates the
/// tracking or the agent's first event was the abandonment): allow,
/// matching the pre-guard behavior (no happy-path regression).
///
/// Pure function (unit-testable without a Tauri `AppHandle`), mirroring
/// [`plan_linkage_allows_done`].
pub fn plan_linkage_allows_failed(
    item_status: BacklogStatus,
    item_plan_id: Option<&str>,
    abandoned_plan_id: Option<&str>,
) -> bool {
    if item_status != BacklogStatus::InFlight {
        return false;
    }
    match (item_plan_id, abandoned_plan_id) {
        (Some(pid), Some(apid)) => pid == apid,
        // The item never planned; the abandoned plan is not its.
        (None, Some(_)) => false,
        // Abandoned plan id not captured (the evidence predates the
        // tracking) — the status guard alone (no happy-path regression).
        _ => true,
    }
}

/// Whether a workflow state transition is a ROOT-plan abandonment (backlog
/// 45dcf577): `Executing`/`Reviewing` → `Planning` is the only
/// channel-visible path INTO `Planning` mid-flight — the dispatch-time
/// `Complete` → `Planning` entry never touches the event channel, and a
/// sub-plan abandon pops to its parent and stays `Executing` (so a
/// sub-plan push/pop never looks like an abandonment). `finish` goes to
/// `Complete`. Drives the plan-tied `Failed` transition: failure means
/// exactly "the plan was abandoned".
pub fn is_root_plan_abandonment(prev: Option<WorkflowState>, new: WorkflowState) -> bool {
    matches!(
        prev,
        Some(WorkflowState::Executing | WorkflowState::Reviewing)
    ) && new == WorkflowState::Planning
}

/// Build the note recorded on an item whose dispatched turn ended WITHOUT
/// the plan loop closing (backlog 45dcf577: that is NOT a failure — the item
/// keeps its status, the work stays in the tree, and the run WAITS: the
/// next turn resolution re-checks — the amplifier fix, 2027-01-07. The
/// agent routinely recovers on its own (auto-continue resumes mid-plan
/// turn ends; reviewer-finish notifications resume the closing sequence)
/// and the plan closes on a later turn, so the run must stay armed for the
/// completing turn's resolution; auto-feed stays suppressed on the
/// single-dispatch path until the item resolves). The note says exactly
/// what happened, for the resumed session and the user.
fn plan_open_note(main_state: Option<WorkflowState>) -> String {
    match main_state {
        // Resting Complete with NO transition observed this turn: no plan
        // ever ran (e.g. the agent answered freeform). The item never left
        // `Pending` (the Executing-entry stamp never fired) — it stays
        // queued.
        Some(WorkflowState::Complete) => "plan loop never ran this turn (workflow rested in Complete) — item left pending; run waits for the plan to run (the next turn resolution re-checks)".to_string(),
        // The turn ended with the plan still open: the item stays `InFlight`
        // — the plan persists and a resumed session continues it. The work
        // is NOT rolled back.
        Some(ws) => format!("plan loop did not close (workflow: {ws}) — item left in_flight; work kept in tree; run waits for the plan to close (the next turn resolution re-checks)"),
        // The workflow state could not be verified — never mark Done on an
        // unverifiable closure, never destroy possibly-good work.
        None => "could not verify plan-loop closure (agent state unavailable) — work left in tree; run waits (the next turn resolution re-checks)".to_string(),
    }
}

/// Read the main agent's current workflow state (for the plan-loop gate).
///
/// Returns `None` when the state cannot be verified (no main agent registered
/// or its agent loop missing, e.g. during teardown). Callers must treat
/// `None` as "unverifiable" — NEVER as success: the item must not be marked
/// `Done` when we cannot prove the plan loop closed.
pub(crate) async fn main_agent_workflow_state(state: &IpcState) -> Option<WorkflowState> {
    let main_id = state.runtime.manager.lock().await.main_agent_id()?;
    agent_workflow_state(state, main_id).await
}

/// Read ONE agent's workflow state (plan ffd7a86f — the per-agent sibling
/// of [`main_agent_workflow_state`] for spawned run-all agents: the
/// resolution gate must read the item's OWN agent's workflow, never the
/// main agent's).
pub(crate) async fn agent_workflow_state(
    state: &IpcState,
    agent_id: mnemo::runtime::AgentId,
) -> Option<WorkflowState> {
    let agent_loops = state.runtime.agent_loops.lock().await;
    let agent_loop = agent_loops.get(&agent_id)?;
    let workflow = agent_loop.workflow_handle();
    let workflow = workflow.lock().await;
    Some(workflow.state())
}

/// Deterministic task-entry for a dispatched backlog item (plan 9057fa1f):
/// transition the main agent's RESTING `Complete` workflow to `Planning`
/// before the item's prompt runs — the UI flips to Planning immediately and
/// the turn starts under Planning-state guidance instead of relying on the
/// model to call `create_plan` unprompted (the Complete-state PLAN_NUDGE
/// stays advisory for interactive chat).
///
/// Returns `Some((new_state, top_plan_id))` when the transition ran — the
/// caller emits the `WorkflowStateChanged` UI event — and `None` when the
/// workflow was not `Complete` or the transition errored (logged; dispatch
/// proceeds either way).
///
/// Gate safety: the transition is deliberately invisible to the plan-loop
/// gate. It never touches the runtime event channel (so the forwarder's
/// `TurnResolveLatch` never notes it), and even a channel-observed
/// transition would be wiped by `on_started` when the dispatched turn
/// begins — a turn that never plans still cannot be marked `Done`. The
/// workflow transition itself is also NOT persisted: a crash before the
/// agent's `create_plan` reloads the workflow as `Complete` (the finished
/// plan's stored state is untouched) and the item re-dispatches.
fn enter_planning_if_complete(workflow: &mut Workflow) -> Option<(WorkflowState, Option<String>)> {
    if workflow.state() != WorkflowState::Complete {
        return None;
    }
    match workflow.enter_planning_for_task() {
        Ok(()) => Some((
            workflow.state(),
            workflow.top_plan_id().map(|id| id.to_string()),
        )),
        Err(e) => {
            eprintln!("backlog: dispatch planning-entry skipped: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mnemo::workflow::{PlanKind, Workflow, WorkflowState};

    // ---- deferred (skip Run-All) selection — backlog f2d2809b ----

    /// Backlog f2d2809b (user request 2027-01-07): Run-All selection must
    /// go through `next_pending_eligible` — deferred items are never
    /// selected by a run (they stay pending and visible; auto-feed and the
    /// per-item ▶ button still see them via `next_pending`/`pending_item`).
    /// When nothing eligible remains, the existing `end_run` arm ends the
    /// run cleanly. The command needs a Tauri AppHandle, so the selection
    /// wiring is pinned as a source contract; the store method itself is
    /// unit-tested in `mnemo::backlog::tests`.
    #[test]
    fn run_all_dispatch_next_selects_via_next_pending_eligible() {
        let src = include_str!("run_all.rs");
        // NOTE: this file's tests module sits BEFORE the command functions,
        // so a plain `find` would match this test's own quoted signature —
        // `rfind` anchors on the real definition (the last occurrence).
        let start = src
            .rfind("pub(crate) async fn run_all_dispatch_next")
            .expect("run_all_dispatch_next present");
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(
            body.contains("next_pending_eligible()"),
            "Run-All selection must skip deferred items"
        );
        assert!(
            !body.contains("next_pending()"),
            "Run-All must not select via the unfiltered next_pending"
        );
    }

    /// Backlog 2027-01-07 (bug-fixing model + reasoning-effort slot): a
    /// defect item dispatched by Run-All must land on the bug-fixing slot's
    /// model/effort, a feature item on the executing slot. The dispatch
    /// itself sends a plain prompt through the main agent's turn loop — the
    /// model is resolved per iteration from the workflow state + active plan
    /// kind, so the moment the steered `create_plan(kind=bug_fixing)`
    /// (RUN_ALL_STEER) flips the workflow to Executing, the very next
    /// provider request in the same turn resolves `[models.bug_fixing]`. No
    /// dispatch code change is needed (and none is wanted: pre-classifying
    /// the item at dispatch would duplicate the agent's create_plan judgment
    /// and could pin the wrong model for the whole session). This test pins
    /// that exact resolution — the ModelContext the agent loop builds from
    /// `wf.state()` + `wf.active_plan_kind()` — against the acceptance
    /// configuration: bug-fixing = max-reasoning model + effort max,
    /// executing = lesser model + effort low.
    #[test]
    fn run_all_dispatch_lands_bug_items_on_the_bug_fixing_slot() {
        use mnemo::config::{Endpoint, GeneralConfig, ModelRef, ModelsConfig};
        use mnemo::model_resolver::{ConfigModelResolver, ModelContext, ModelResolver};
        use std::sync::{Arc, RwLock};

        let config = mnemo::config::Config {
            general: GeneralConfig {
                models: ModelsConfig {
                    bug_fixing: Some(ModelRef {
                        endpoint: "openai".into(),
                        model: "o3".into(),
                        reasoning_effort: Some("max".into()),
                    }),
                    executing: Some(ModelRef {
                        endpoint: "deepseek".into(),
                        model: "deepseek-v4-flash".into(),
                        reasoning_effort: Some("low".into()),
                    }),
                    ..ModelsConfig::default()
                },
                ..GeneralConfig::default()
            },
            endpoints: vec![
                Endpoint {
                    name: "openai".into(),
                    base_url: "https://api.openai.com/v1/".into(),
                    models: vec!["o3".into()],
                    ..Endpoint::test_default()
                },
                Endpoint {
                    name: "deepseek".into(),
                    base_url: "https://api.deepseek.com/v1/".into(),
                    models: vec!["deepseek-v4-flash".into()],
                    ..Endpoint::test_default()
                },
            ],
            ..Default::default()
        };
        let resolver = ConfigModelResolver::new(
            Arc::new(RwLock::new(config)),
            Arc::new(mnemo::provider::trace::LlmRequestLog::new()),
        );

        // A dispatched BUG item: the agent creates a bug_fixing plan (steered
        // by RUN_ALL_STEER) → the loop resolves (Executing, BugFixing) → the
        // bug-fixing slot's model + effort.
        let bug = resolver
            .resolve(ModelContext::new(
                WorkflowState::Executing,
                None,
                false,
                Some(PlanKind::BugFixing),
            ))
            .expect("a dispatched bug item resolves the bug-fixing slot");
        assert_eq!(bug.endpoint, "openai");
        assert_eq!(bug.model, "o3");
        assert_eq!(bug.reasoning_effort.as_deref(), Some("max"));

        // A dispatched FEATURE item: implementation plan → the executing slot.
        let feature = resolver
            .resolve(ModelContext::new(
                WorkflowState::Executing,
                None,
                false,
                Some(PlanKind::Implementation),
            ))
            .expect("a dispatched feature item resolves the executing slot");
        assert_eq!(feature.endpoint, "deepseek");
        assert_eq!(feature.model, "deepseek-v4-flash");
        assert_eq!(feature.reasoning_effort.as_deref(), Some("low"));

        // An unset bug-fixing slot inherits today's behavior: the executing
        // slot (model + effort), not the default.
        let config = mnemo::config::Config {
            general: GeneralConfig {
                models: ModelsConfig {
                    executing: Some(ModelRef {
                        endpoint: "deepseek".into(),
                        model: "deepseek-v4-flash".into(),
                        reasoning_effort: Some("low".into()),
                    }),
                    ..ModelsConfig::default()
                },
                ..GeneralConfig::default()
            },
            endpoints: vec![Endpoint {
                name: "deepseek".into(),
                base_url: "https://api.deepseek.com/v1/".into(),
                models: vec!["deepseek-v4-flash".into()],
                ..Endpoint::test_default()
            }],
            ..Default::default()
        };
        let resolver = ConfigModelResolver::new(
            Arc::new(RwLock::new(config)),
            Arc::new(mnemo::provider::trace::LlmRequestLog::new()),
        );
        let inherited = resolver
            .resolve(ModelContext::new(
                WorkflowState::Executing,
                None,
                false,
                Some(PlanKind::BugFixing),
            ))
            .expect("an unset bug-fixing slot inherits the executing slot");
        assert_eq!(inherited.model, "deepseek-v4-flash");
        assert_eq!(inherited.reasoning_effort.as_deref(), Some("low"));
    }

    /// Backlog 998f85fc (user ask 2027-01-07): the run-start adoption sweep
    /// must snapshot the LIVE-ONLY view — `store.items()` filters
    /// soft-deleted items, so a cleared (soft-deleted) InFlight item can
    /// never be resurrected by adoption. The sweep needs Tauri state, so
    /// the read is pinned as a source contract; the
    /// items()-filters-deleted behavior is unit-tested in
    /// `mnemo::backlog::tests`.
    #[test]
    fn adopt_orphaned_in_flight_snapshots_the_live_only_view() {
        let src = include_str!("run_all.rs");
        // NOTE: this file's tests module sits BEFORE the command functions,
        // so a plain `find` would match this test's own quoted signature —
        // `rfind` anchors on the real definition (the last occurrence).
        let start = src
            .rfind("pub(crate) async fn adopt_orphaned_in_flight")
            .expect("adopt_orphaned_in_flight present");
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(
            body.contains(".items()"),
            "the adoption snapshot must iterate store.items() — the live-only view (a soft-deleted InFlight item is invisible to it and can never be resurrected by adoption)"
        );
        assert!(
            body.contains("BacklogStatus::InFlight"),
            "the sweep must filter for InFlight orphans only"
        );
    }

    /// Backlog 6c6966b9 (user request 2027-01-07, live case: item 35b94671 /
    /// plan 225e0dad / commit eac25dc — fully landed, then re-dispatched): a
    /// session that dies between its closing-sequence commit and `finish`
    /// leaves its item orphaned InFlight with the work already landed, and
    /// only `finish` marks an item done — so an unconditional requeue
    /// re-executes the landed task as duplicate work. The main-agent-exit
    /// drain must consult landed-work evidence (`orphan_work_landed`) and
    /// auto-resolve landed orphans to `Done` instead of requeueing. The drain
    /// needs Tauri state, so the guard is pinned as a source contract.
    #[test]
    fn drain_run_all_on_main_exit_consults_landed_evidence() {
        let src = include_str!("run_all.rs");
        // NOTE: this file's tests module sits BEFORE the command functions,
        // so a plain `find` would match this test's own quoted signature —
        // `rfind` anchors on the real definition (the last occurrence).
        let start = src
            .rfind("pub(crate) async fn drain_run_all_on_main_exit")
            .expect("drain_run_all_on_main_exit present");
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(
            body.contains("orphan_work_landed("),
            "the main-exit drain must consult landed-work evidence before requeueing — a died-after-commit session's item must auto-resolve Done, not requeue for duplicate re-dispatch"
        );
        assert!(
            body.contains("BacklogStatus::Done"),
            "the drain must carry a Done auto-resolve arm for landed orphans"
        );
        assert!(
            body.contains("BacklogStatus::Pending"),
            "the drain must still requeue non-landed orphans to Pending (the recovery path)"
        );
    }

    /// Backlog 6c6966b9 (user request 2027-01-07): the run-start adoption
    /// sweep is the other orphan-requeue site — it must consult the same
    /// landed-work evidence (`orphan_work_landed`) and auto-resolve landed
    /// orphans to `Done` instead of requeueing them for duplicate
    /// re-dispatch. The sweep needs Tauri state, so the guard is pinned as a
    /// source contract.
    #[test]
    fn adopt_orphaned_in_flight_consults_landed_evidence() {
        let src = include_str!("run_all.rs");
        // NOTE: this file's tests module sits BEFORE the command functions,
        // so a plain `find` would match this test's own quoted signature —
        // `rfind` anchors on the real definition (the last occurrence).
        let start = src
            .rfind("pub(crate) async fn adopt_orphaned_in_flight")
            .expect("adopt_orphaned_in_flight present");
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(
            body.contains("orphan_work_landed("),
            "the adoption sweep must consult landed-work evidence before requeueing — a died-after-commit session's item must auto-resolve Done, not requeue for duplicate re-dispatch"
        );
        assert!(
            body.contains("BacklogStatus::Done"),
            "the sweep must carry a Done auto-resolve arm for landed orphans"
        );
        assert!(
            body.contains("BacklogStatus::Pending"),
            "the sweep must still requeue non-landed orphans to Pending (the recovery path)"
        );
    }

    /// Backlog 6c6966b9: the plan-side landed evidence — every step
    /// checked. A plan with an unchecked step, a missing file, or zero
    /// steps is NOT evidence of landed work (the safe default requeues).
    #[test]
    fn plan_steps_all_done_requires_every_step_checked() {
        let dir = tempfile::tempdir().expect("tempdir");
        let all_done = dir.path().join("all.md");
        std::fs::write(
            &all_done,
            "# Plan: t\n\n## Goal\ng\n\n## Kind\nimplementation\n\n## Context\nc\n\n## Steps\n- [x] 1. first\n- [x] 2. second\n",
        )
        .expect("write");
        assert!(
            plan_steps_all_done(&all_done),
            "every step checked — the plan verifiably completed"
        );

        let partial = dir.path().join("partial.md");
        std::fs::write(
            &partial,
            "# Plan: t\n\n## Goal\ng\n\n## Kind\nimplementation\n\n## Context\nc\n\n## Steps\n- [x] 1. first\n- [ ] 2. second\n",
        )
        .expect("write");
        assert!(
            !plan_steps_all_done(&partial),
            "an unchecked step — the dead session was mid-execution, not landed"
        );

        // Missing file — not evidence.
        assert!(!plan_steps_all_done(&dir.path().join("missing.md")));

        // Zero steps — not evidence (an empty plan proves nothing).
        let empty = dir.path().join("empty.md");
        std::fs::write(
            &empty,
            "# Plan: t\n\n## Goal\ng\n\n## Kind\nimplementation\n\n## Context\nc\n\n## Steps\n",
        )
        .expect("write");
        assert!(
            !plan_steps_all_done(&empty),
            "a stepless plan is not evidence of landed work"
        );
    }

    // ---- run_all_prompt: the preamble rides IN the prompt ----

    #[test]
    fn run_all_prompt_embeds_preamble_and_task_in_one_text() {
        // Regression (user report 2026-08-20: run-all "starts new items
        // before the active item is fully resolved, resulting in can't
        // resolve"): the preamble used to be sent as a separate Suggestion
        // BEFORE the Prompt — but a steer landing on an idle agent runs as
        // its own full turn (runtime/agent.rs Suggestion arm), so every item
        // resolved on a steer-only turn (plan-loop gate → CantResolve) and
        // the next item was dispatched before the real prompt ran. The
        // preamble must be part of the one Prompt text.
        let text = run_all_prompt("Fix the login bug", None);
        assert!(
            text.starts_with(UNATTENDED_PROMPT_PREFIX),
            "the preamble leads the prompt (the core-runtime marker the \
             agent task detects for unattended mode)"
        );
        assert!(
            text.contains("marked done ONLY when its plan reaches Complete"),
            "rule (4) — the plan-loop contract — is embedded"
        );
        assert!(
            text.contains("\n\n---\n\n"),
            "a separator splits preamble from task"
        );
        assert!(
            text.ends_with("Fix the login bug"),
            "the task text rides verbatim at the end"
        );
    }

    #[test]
    fn run_all_prompt_relies_on_auto_fork_not_a_branch_arg() {
        // The preamble must NOT steer agents onto the create_plan `branch`
        // parameter — create_plan auto-forks a per-directory working branch
        // when needed (one branch per agent directory; passing a per-item
        // slug forked a new unmerged branch every item → accumulation). A
        // `branch` arg is reserved for an explicit user request only.
        let text = run_all_prompt("anything", None);
        assert!(
            !text.contains("branch: \"fix/<short-slug>\""),
            "the preamble must not tell the agent to pass a branch slug: {text}"
        );
        assert!(
            text.contains("never commit work to main"),
            "the never-main rule still rides with it: {text}"
        );
        assert!(
            text.contains("auto-forks"),
            "the preamble explains the auto-fork: {text}"
        );
    }

    #[test]
    fn run_all_prompt_steers_defect_items_to_bug_fixing() {
        // Plan 9057fa1f: the preamble steers defect-shaped items to the
        // bug_fixing plan KIND so the locked reproduce→root-cause→fix→verify
        // skeleton engages from the FIRST plan, not after a wasted
        // implementation plan. bug_fixing is a plan kind (chosen at
        // create_plan), not a workflow state — the preamble is the steer.
        let text = run_all_prompt("Fix the login bug", None);
        assert!(
            text.contains("bug_fixing"),
            "the preamble must name the bug_fixing kind: {text}"
        );
        assert!(
            text.contains("`bug` parameter"),
            "bug_fixing requires the symptom in the `bug` parameter — the \
             preamble must say so: {text}"
        );
    }

    // ---- Dispatch enrichment (2027-01-07): the recalled-context block ----

    #[test]
    fn run_all_prompt_appends_the_recalled_context_block() {
        // Without a block the task text ends the prompt (today's shape).
        let plain = run_all_prompt("Fix the login bug", None);
        assert!(plain.ends_with("Fix the login bug"));
        assert!(!plain.contains("recalled context"));
        // With a block it rides AFTER the task, separated like the preamble.
        let block = "[recalled context — auto-recalled project knowledge for this item; may be \
                     partial or stale — verify against the code before relying on it]\n- \
                     [semantic] SPEC: login flow — the session flag lives in src/auth/session.rs\n\
                     [/recalled context]";
        let enriched = run_all_prompt("Fix the login bug", Some(block));
        assert!(
            enriched.starts_with(UNATTENDED_PROMPT_PREFIX),
            "the preamble still leads: {enriched}"
        );
        let task_pos = enriched.find("Fix the login bug").expect("task present");
        let block_pos = enriched
            .find("[recalled context")
            .expect("block present");
        assert!(task_pos < block_pos, "the task precedes the context block");
        assert!(enriched.ends_with("[/recalled context]"));
    }

    #[test]
    fn format_recalled_context_formats_hits_with_a_caveat() {
        use mnemo::memory::{Memory, MemoryTier, ScoredMemory};

        fn hit(title: &str, content: &str) -> ScoredMemory {
            ScoredMemory {
                memory: Memory::new(MemoryTier::Semantic, title, content, 0),
                score: 0.9,
            }
        }

        // No hits → no block (an empty block would be noise).
        assert!(format_recalled_context(&[]).is_none());

        let block = format_recalled_context(&[
            hit("SPEC: login flow", "The session flag lives in src/auth/session.rs."),
            hit(
                "BUG: login loop",
                "line one\nline two\twith a tab",
            ),
        ])
        .expect("hits format a block");
        assert!(
            block.starts_with("[recalled context"),
            "the block opens with the caveat header: {block}"
        );
        assert!(
            block.contains("verify against the code"),
            "the stale-knowledge caveat rides in the header: {block}"
        );
        assert!(
            block.contains("- [semantic] SPEC: login flow — The session flag lives in \
                            src/auth/session.rs."),
            "each hit is a tier-prefixed title + gist line: {block}"
        );
        assert!(
            block.contains("line one line two with a tab"),
            "control characters are flattened so the block stays single-line \
             per hit: {block}"
        );
        assert!(block.ends_with("[/recalled context]"));

        // The gist truncates at RECALL_BLOCK_GIST_CHARS and the hit count
        // caps at RECALL_BLOCK_HITS — the block must stay lean.
        let long: String = "x".repeat(RECALL_BLOCK_GIST_CHARS + 50);
        let many: Vec<ScoredMemory> = (0..RECALL_BLOCK_HITS + 3)
            .map(|i| hit(&format!("T{i}"), &long))
            .collect();
        let block = format_recalled_context(&many).expect("block");
        assert_eq!(
            block.matches("- [semantic] T").count(),
            RECALL_BLOCK_HITS,
            "only the top {RECALL_BLOCK_HITS} hits ride: {block}"
        );
        assert!(
            !block.contains(&"x".repeat(RECALL_BLOCK_GIST_CHARS + 1)),
            "gists truncate at {RECALL_BLOCK_GIST_CHARS} chars: {block}"
        );
        assert!(
            block.contains(&format!("{}...", "x".repeat(RECALL_BLOCK_GIST_CHARS))),
            "a truncated gist ends with an ellipsis: {block}"
        );

        // A knowledge-backed hit carries its file path as a fourth field on
        // the hit line — seeded in the REAL stored shape (knowledge-dir-
        // relative, no prefix, per the indexer) and emitted project-relative
        // (the store's own reporting convention); a hit without a path keeps
        // the three-field shape (asserted above).
        let mut knowledge_hit = hit(
            "SPEC: login flow",
            "The session flag lives in src/auth/session.rs.",
        );
        knowledge_hit.memory.data = serde_json::json!({
            "kind": "knowledge",
            "rel_path": "spec/2026-09-08-login.md"
        });
        let block = format_recalled_context(&[knowledge_hit]).expect("block");
        assert!(
            block.contains(" — .coding/knowledge/spec/2026-09-08-login.md"),
            "the record's file path rides on the hit line, project-relative: {block}"
        );
    }

    #[test]
    fn run_all_dispatch_next_attaches_the_recalled_context_block() {
        // Source contract: the dispatch path computes the block
        // (best-effort — never blocks the dispatch) and threads it into
        // run_all_prompt. A regression that drops the wiring fails here
        // before it ships a lesser model an unenriched prompt. The compute
        // stays in the orchestrator; the prompt threading lives in the
        // dispatch-stage helper it feeds.
        let body = fn_body(include_str!("run_all.rs"), "run_all_dispatch_next");
        assert!(
            body.contains("recalled_context_block(state, &item.text)"),
            "the dispatch computes the recalled-context block: {body}"
        );
        let dispatch_body = fn_body(include_str!("run_all.rs"), "dispatch_run_all_to_main");
        assert!(
            dispatch_body.contains("run_all_prompt(&item.text, context_block.as_deref())"),
            "the block rides into the dispatched prompt: {dispatch_body}"
        );
    }

    // ---- Parallel run-all (plan ffd7a86f) ----

    #[test]
    fn parallel_dispatch_fills_the_spawned_window_on_every_dispatch_exit() {
        // Source contract: the spawned lanes fill on EVERY dispatch exit —
        // the success path, the busy-guard defer, and the checkpoint-race
        // defer — so a busy main agent never starves the parallel window.
        // Sequential runs (concurrency 1) return before the first spawn.
        let body = include_str!("run_all.rs");
        assert!(
            body.matches("fill_spawned_window(app, state).await;").count() >= 3,
            "the fill must run on all three dispatch exits"
        );
        let fill = fn_body(body, "fill_spawned_window");
        assert!(
            fill.contains("if concurrency <= 1"),
            "sequential runs never spawn: {fill}"
        );
        assert!(
            fill.contains("next_pending_eligible_excluding(&exclude)"),
            "the parallel selection skips handed-out ids: {fill}"
        );
    }

    #[test]
    fn spawned_dispatch_records_the_run_before_the_prompt_send() {
        // Source contract: the SpawnedRun record lands BEFORE the prompt
        // send — a fast turn can never resolve against a missing record
        // (the same deterministic-ordering discipline as the main path's
        // checkpoint-before-dispatch).
        let body = fn_body(include_str!("run_all.rs"), "dispatch_spawned_item");
        let record = body.find("Record the SpawnedRun").expect("the record");
        let send = body.find("AgentCommand::Prompt").expect("the send");
        assert!(record < send, "the record must land before the send");
    }

    #[test]
    fn spawned_resolution_is_routed_per_agent_never_main() {
        // Source contract (events.rs): a spawned worktree agent's turn
        // resolution routes through owns_spawned_run →
        // on_spawned_turn_resolved — the item resolved is THAT agent's,
        // never the main agent's (no cross-stamping). The Exited arm
        // drains spawned runs (requeue or landed auto-Done), and the
        // Executing stamp routes to stamp_spawned_in_flight.
        let events = include_str!("events.rs");
        assert!(
            events.matches("owns_spawned_run(&app, agent_id).await").count() >= 2,
            "both the Error and Finished arms route spawned agents"
        );
        assert!(
            events.contains("on_spawned_turn_resolved("),
            "the per-agent resolution is wired"
        );
        assert!(
            events.contains("drain_spawned_on_exit(&app, agent_id).await"),
            "the Exited arm drains spawned runs"
        );
        assert!(
            events.contains("stamp_spawned_in_flight("),
            "the Executing stamp routes per-agent"
        );
    }

    #[test]
    fn lanes_in_flight_covers_both_lane_kinds() {
        // Review H1: every end_run site gates on this — the main lane's
        // current_item OR any spawned entry keeps the run alive (ending
        // with lanes in flight orphans their items, branches, and agents).
        let run = crate::ipc::state::RunAllState {
            stop: std::sync::atomic::AtomicBool::new(false),
            current_item: std::sync::Mutex::new(None),
            done: std::sync::atomic::AtomicU64::new(0),
            total: std::sync::atomic::AtomicU64::new(2),
            concurrency: 2,
            spawned: std::sync::Mutex::new(Vec::new()),
            knowledge_writes_at_start: std::sync::atomic::AtomicU64::new(0),
        };
        assert!(!lanes_in_flight(&run), "nothing in flight");
        *run.current_item.lock().unwrap() = Some("main-item".into());
        assert!(lanes_in_flight(&run), "the main lane is in flight");
        *run.current_item.lock().unwrap() = None;
        run.spawned
            .lock()
            .unwrap()
            .push(crate::ipc::state::SpawnedRun {
                agent_id: 7,
                item_id: "item-2".into(),
                worktree: std::path::PathBuf::from("/wt"),
                branch: "wt/runall-item2xx".into(),
            });
        assert!(lanes_in_flight(&run), "a spawned lane is in flight");
    }

    #[test]
    fn spawned_plans_dir_lives_under_the_agent_subdir() {
        // Review H3: a spawned agent's plan files live under
        // `.coding/plans/agents/<agent_id>/` (the own-plans-dir
        // convention) — the landed predicate read the top-level plans dir,
        // making it always-false for spawned items (broken closed-earlier
        // recovery + exit-drain landed guard).
        let dir = spawned_plans_dir(std::path::Path::new("/wt"), 7);
        assert_eq!(
            dir,
            std::path::Path::new("/wt/.coding/plans/agents/7")
        );
    }

    #[test]
    fn end_run_is_gated_on_lanes_in_flight_everywhere() {
        // Reviews H1 + R3: every end_run call site must gate on the
        // in-flight check — the six guard-form sites (no-item, stopped,
        // halt, compact stop, checkpoint-failure, no-main-agent) plus the
        // wind-down variants (the main-exit drain + the intervention).
        let body = include_str!("run_all.rs");
        assert!(
            body.matches("if !any_lane_in_flight").count() >= 6,
            "the no-item, stopped, halt, compact-stop, checkpoint-failure, \
             and no-main-agent paths all gate"
        );
        assert!(
            body.matches("spawned_in_flight").count() >= 2,
            "the main-exit drain AND the intervention wind down instead of ending"
        );
        // R2: the main-exit drain clears the stale current_item pointer —
        // without the clear the wind-down can never terminate.
        let drain = fn_body(body, "drain_run_all_on_main_exit");
        assert!(
            drain
                .contains("*r.current_item.lock().expect(\"current_item lock poisoned\") = None;"),
            "the drain clears the main lane's pointer: {drain}"
        );
        // R3-H1: the intervention's closed-loop `ours` arm ALSO clears the
        // pointer (the item was just resolved Done) — the R2 pin covered
        // only the main-exit drain, which is exactly the gap this slipped
        // through.
        assert!(
            body.contains("R3-H1: the steered item was just resolved"),
            "the ours arm clears the main lane's pointer before the wind-down"
        );
        // R3-L1: a spawned lane's deferred turn resolution has a flush
        // path (a child's Finished / any Exited), routed by ownership and
        // running BEFORE the main flush (post-main-exit main_agent_id()
        // resolves to a lane — the main flush would mis-consume it).
        // R4-L2: the ordering is LOAD-BEARING — a main-flush-first
        // ordering would mis-consume a lane's deferred failure and
        // deliver it through the main path while current_item is still
        // Some (the R2 clear runs later, in the drain). The two call
        // sites' flush calls must interleave [lane, main] — never
        // [main, lane].
        let events = include_str!("events.rs");
        assert!(
            events.matches("try_flush_deferred_spawned_resolutions(").count() >= 3,
            "the lane flush is defined and wired at both flush sites"
        );
        let mut calls: Vec<&str> = Vec::new();
        for (i, _) in events.match_indices("try_flush_deferred_") {
            // Skip the definitions (preceded by "async fn ").
            if events[..i].ends_with("async fn ") {
                continue;
            }
            if events[i..].starts_with("try_flush_deferred_spawned_resolutions(") {
                calls.push("lane");
            } else if events[i..].starts_with("try_flush_deferred_main_resolution(") {
                calls.push("main");
            }
        }
        assert_eq!(
            calls,
            vec!["lane", "main", "lane", "main"],
            "both flush sites must run the lane flush BEFORE the main flush"
        );
    }

    #[test]
    fn spawned_routing_is_ownership_first_never_live_main_id() {
        // Review R1: the forwarder routes a spawned lane's events by
        // OWNERSHIP (owns_spawned_run) BEFORE the live main_agent_id()
        // comparison — spawned agents are parentless, and after the main
        // agent's exit the smallest parentless id is a spawned lane;
        // routing its events through the main paths cross-stamps the
        // requeued main item and strands the lane.
        let events = include_str!("events.rs");
        assert!(
            events.matches("owns_spawned_run(&app, agent_id).await").count() >= 4,
            "the Error, Finished, Executing-stamp, and Exited arms all check ownership"
        );
        assert!(
            events.contains("!owns_spawned && mgr.main_agent_id() == Some(agent_id)"),
            "the Exited arm's was_main is gated on ownership"
        );
    }

    #[test]
    fn dispatch_decisions_are_serialized() {
        // Review R4: the selection→record window spans tens of seconds —
        // a concurrent dispatch_next would select the SAME item (Pending,
        // absent from every exclude list) and double-dispatch it. The
        // DISPATCH_LOCK closes the window.
        let body = include_str!("run_all.rs");
        assert!(
            body.contains("let _dispatch_guard = DISPATCH_LOCK.lock().await;"),
            "run_all_dispatch_next holds the dispatch lock"
        );
        // Review R5-L1: the intervention ours arm's DETERMINATION sits
        // inside the lock too (hoisted above `let ours`) — R4-L1's race
        // (a) needed the check and the clear under the same hold.
        let resolved = fn_body(body, "on_main_turn_resolved");
        let lock_at = resolved
            .find("let _dispatch_guard = DISPATCH_LOCK.lock().await;")
            .expect("the ours arm holds the dispatch lock");
        let ours_at = resolved
            .find("let ours = {")
            .expect("the ours determination");
        assert!(
            lock_at < ours_at,
            "the ours determination must run UNDER the dispatch lock (R5-L1)"
        );
    }

    #[test]
    fn spawned_exit_drain_re_drives_the_run() {
        // Review R5: a crashed lane with an idle main lane must dispatch
        // the requeued item — otherwise the run stalls until the user
        // intervenes. During a stopped wind-down, the last lane exiting
        // ends the run.
        let body = fn_body(include_str!("run_all.rs"), "drain_spawned_on_exit");
        assert!(
            body.contains("run_all_dispatch_next(app, &state).await"),
            "the drain re-drives the run: {body}"
        );
        assert!(
            body.contains("if !any_lane_in_flight(&state).await {"),
            "the stopped wind-down ends the run when the last lane exits"
        );
    }

    #[test]
    fn exit_drain_done_bumps_the_run_counter() {
        // Review round-1 LOW-1 (plan a0d3defd): with the resolution
        // continuations spawned off the forwarder, a lane (or the main
        // agent) that exits right after its item's work landed can be
        // auto-resolved Done by the EXIT DRAIN before the delayed
        // continuation runs — the continuation then finds the row already
        // Done and bumps nothing. The drain's own successful Done
        // transition must bump the run's done counter (symmetric with the
        // resolution paths), gated on the transition's bool so a blocked
        // flip (the continuation won the race) never double-counts.
        let src = include_str!("run_all.rs");
        let spawned = fn_body(src, "drain_spawned_on_exit");
        assert!(
            spawned.contains("let transitioned ="),
            "the spawned drain captures its Done transition's result: {spawned}"
        );
        assert!(
            spawned.contains("r.done.fetch_add(1, Ordering::Relaxed);"),
            "the spawned drain bumps the done counter on its own Done \
             transition: {spawned}"
        );
        let main = fn_body(src, "drain_run_all_on_main_exit");
        assert!(
            main.contains("main_done_bump = store.transition("),
            "the main drain captures its Done transition's result: {main}"
        );
        assert!(
            main.contains("r.done.fetch_add(1, Ordering::Relaxed);"),
            "the main drain bumps the done counter on its own Done \
             transition: {main}"
        );
    }

    #[test]
    fn continuation_done_bumps_are_gated_on_the_transition() {
        // Review round-2 LOW-1 (plan a0d3defd): the continuation-side done
        // bumps must be gated on the transition's OWN result, not a status
        // pre-check — the exit drain's Done flip can land in the
        // commit_success / lock-re-acquisition gap between the pre-check
        // and the transition, and an ungated bump double-counts the item
        // (display-only: the counter feeds the UI progress line).
        let src = include_str!("run_all.rs");
        // flip_done_if_linked: the Done flip's result IS the return value
        // (no unconditional `true` after the transition).
        let flip = fn_body(src, "flip_done_if_linked");
        assert!(
            !flip.contains("\n        true\n"),
            "flip_done_if_linked returns the transition's own result, not \
             an unconditional true: {flip}"
        );
        // The spawned success arm gates terminal_resolution on the
        // transition's bool.
        let spawned = fn_body(src, "on_spawned_turn_resolved");
        assert!(
            spawned.contains("terminal_resolution = state"),
            "the spawned success arm gates its done bump on the \
             transition's result: {spawned}"
        );
    }

    #[test]
    fn spawned_send_failure_cleans_up_the_recorded_entry() {
        // Review R6: a failed prompt send must remove the recorded entry +
        // retire the promptless agent — otherwise the lane never resolves
        // and the run strands.
        let body = fn_body(include_str!("run_all.rs"), "dispatch_spawned_item");
        assert!(
            body.contains("remove_spawned_run(state, agent_id).await;"),
            "the send-failure path removes the entry: {body}"
        );
    }

    #[test]
    fn run_start_adoption_cleans_stale_spawned_worktrees() {
        // Review R7: a requeued orphan may be a crashed spawned lane's
        // item — its stale worktree + branch would block every later
        // spawned-lane dispatch of the item ("branch already exists").
        let body = fn_body(include_str!("run_all.rs"), "adopt_orphaned_in_flight");
        assert!(
            body.contains("remove_item_worktree("),
            "the adoption sweep removes derivable stale worktrees: {body}"
        );
    }

    #[test]
    fn main_lane_selection_excludes_spawned_items() {
        // Review H2: the main lane must not double-dispatch an item a
        // spawned agent is working (spawned items stay Pending until
        // their workflow enters Executing — invisible to the plain
        // selector).
        let body = fn_body(include_str!("run_all.rs"), "run_all_dispatch_next");
        assert!(
            body.contains("next_pending_eligible_excluding(&exclude)"),
            "the main-lane selection excludes handed-out ids: {body}"
        );
    }

    // ---- Deterministic task-entry: enter_planning_if_complete ----

    #[test]
    fn enter_planning_if_complete_transitions_a_resting_complete_workflow() {
        // Plan 9057fa1f: dispatching a backlog item into a Complete-state
        // agent must move the workflow to Planning BEFORE the item's prompt
        // runs. The helper transitions only from Complete (a resting agent)
        // and reports the new state + root-plan id for the UI event.
        let dir = tempfile::tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        // A fresh workflow rests in Planning — nothing to transition.
        assert!(enter_planning_if_complete(&mut wf).is_none());
        assert_eq!(wf.state(), WorkflowState::Planning);
        // Drive to Complete (research plan: last step → Complete directly).
        wf.create_plan_with_kind(
            "Investigate",
            "G",
            "C",
            vec!["Look around".to_string()],
            PlanKind::Research,
            None,
        )
        .unwrap();
        wf.complete_step(0).unwrap();
        assert_eq!(wf.state(), WorkflowState::Complete);

        let (state, top_plan_id) =
            enter_planning_if_complete(&mut wf).expect("Complete transitions");
        assert_eq!(state, WorkflowState::Planning);
        assert!(
            top_plan_id.is_some(),
            "the finished plan stays on the stack and rides in the UI event"
        );
        assert_eq!(wf.state(), WorkflowState::Planning);
        // Executing is never transitioned (an active plan is in flight) —
        // the busy guards upstream already keep dispatch away from those.
        wf.create_plan("T", "G", "C", vec!["S".to_string()])
            .unwrap();
        assert!(enter_planning_if_complete(&mut wf).is_none());
        assert_eq!(wf.state(), WorkflowState::Executing);
    }

    // ---- Plan-loop gate: plan_loop_allows_done ----

    #[test]
    fn plan_loop_allows_done_requires_complete_and_transition() {
        // Regression (user-reported): backlog items were marked Done when the
        // turn merely ended successfully, before the plan loop closed. An item
        // may be marked Done ONLY when the workflow reached Complete — for an
        // implementation plan that means `finish` ran (steps + review +
        // commit); a research plan auto-completes — AND a state transition
        // was observed during the turn (Complete is a resting state).
        assert!(plan_loop_allows_done(WorkflowState::Complete, true));
    }

    #[test]
    fn plan_loop_rejects_every_non_complete_state() {
        // Planning (no plan was ever created — a backlog task MUST enter
        // planning), Executing (plan still in flight), Reviewing (the
        // review→fix→commit closing sequence never finished), Skill
        // (mid-flight) — the loop did NOT close, so the item must not be
        // marked Done, even with a transition observed. (Planning was allowed
        // by the old opt-in strict mode; that hole let freeform no-plan
        // turns count as done.)
        assert!(!plan_loop_allows_done(WorkflowState::Planning, true));
        assert!(!plan_loop_allows_done(WorkflowState::Executing, true));
        assert!(!plan_loop_allows_done(WorkflowState::Reviewing, true));
        assert!(!plan_loop_allows_done(WorkflowState::Skill, true));
    }

    #[test]
    fn plan_loop_rejects_resting_complete_without_transition() {
        // Regression (user-reported, 2026-08-20): Complete is a RESTING state
        // that persists across turns — a backlog turn in which the agent
        // never created a plan (freeform answer, or ask_user before entering
        // planning) ends with workflow == Complete and was wrongly marked
        // Done. The gate requires an observed in-turn state transition on top
        // of the terminal Complete state.
        assert!(!plan_loop_allows_done(WorkflowState::Complete, false));
        // And the combination that DOES mean "loop closed this turn":
        assert!(plan_loop_allows_done(WorkflowState::Complete, true));
    }

    #[test]
    fn is_root_plan_abandonment_detects_only_root_abandons() {
        // Backlog 45dcf577: `Executing`/`Reviewing` → `Planning` is the only
        // channel-visible root-abandonment signal. A sub-plan push/pop stays
        // `Executing` (never an abandonment); the dispatch-time
        // `Complete` → `Planning` entry never touches the event channel (and
        // must not count even if observed); an unchanged-state emit is not a
        // transition.
        assert!(is_root_plan_abandonment(
            Some(WorkflowState::Executing),
            WorkflowState::Planning
        ));
        assert!(is_root_plan_abandonment(
            Some(WorkflowState::Reviewing),
            WorkflowState::Planning
        ));
        // Sub-plan abandon pops to the parent — still Executing.
        assert!(!is_root_plan_abandonment(
            Some(WorkflowState::Executing),
            WorkflowState::Executing
        ));
        // The dispatch-time planning entry (invisible to the forwarder, but
        // defensive: never count it).
        assert!(!is_root_plan_abandonment(
            Some(WorkflowState::Complete),
            WorkflowState::Planning
        ));
        // Unchanged-state emit (a failed workflow tool).
        assert!(!is_root_plan_abandonment(
            Some(WorkflowState::Planning),
            WorkflowState::Planning
        ));
        // First-ever observation (no prev) is never an abandonment.
        assert!(!is_root_plan_abandonment(None, WorkflowState::Planning));
    }

    #[test]
    fn plan_open_note_names_what_happened() {
        // Backlog 45dcf577: a turn that ends without the plan loop closing
        // is NOT a failure — the note must say what happened (never ran /
        // did not close / unverifiable) and that the item keeps its status,
        // the work stays, and the run WAITS: the next turn resolution
        // re-checks (the amplifier fix, 2027-01-07 — the agent routinely
        // recovers via auto-continue / reviewer-finish resume and finishes
        // the plan; a halted run never re-checked and the item stranded).
        let never_ran = plan_open_note(Some(WorkflowState::Complete));
        assert!(never_ran.contains("never ran"));
        assert!(never_ran.contains("left pending"));
        assert!(never_ran.contains("run waits"));

        let open = plan_open_note(Some(WorkflowState::Executing));
        assert!(open.contains("did not close (workflow: Executing)"));
        assert!(open.contains("left in_flight"));
        assert!(open.contains("work kept in tree"));
        assert!(open.contains("run waits for the plan to close"));

        let reviewing = plan_open_note(Some(WorkflowState::Reviewing));
        assert!(reviewing.contains("did not close (workflow: Reviewing)"));

        let unverified = plan_open_note(None);
        assert!(unverified.contains("could not verify"));
        assert!(unverified.contains("work left in tree"));
        assert!(unverified.contains("run waits"));
    }

    // ---- User intervention: steer/interrupt must not fail the item ----

    /// Locate a top-level fn's body in this file's source WITHOUT the search
    /// literal matching this test module's own text: the tests module
    /// PRECEDES the code in run_all.rs, so a plain `src.find("fn name")`
    /// would hit the test's own `find` call first. Composing the needle from
    /// parts keeps the composed string out of this module's source. Returns
    /// the text from the signature through the fn's closing brace (the
    /// column-0 `}`).
    fn fn_body(src: &str, name: &str) -> String {
        let needle = format!("fn {name}(");
        let start = src
            .find(&needle)
            .unwrap_or_else(|| panic!("{name} not found in source"));
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .unwrap_or(src.len());
        src[start..end].to_string()
    }

    #[test]
    fn should_stamp_in_flight_only_on_executing_entry() {
        // Regression (user report, 2026-12): items were stamped InFlight at
        // DISPATCH, so a pre-planning steer/interrupt resolved a "running"
        // item to Failed — but nothing had actually started ("that is not
        // failed yet. It is pre-planning"). The stamp must fire only when
        // the workflow ENTERS Executing.
        assert!(should_stamp_in_flight(None, WorkflowState::Executing));
        assert!(should_stamp_in_flight(
            Some(WorkflowState::Planning),
            WorkflowState::Executing
        ));
        assert!(should_stamp_in_flight(
            Some(WorkflowState::Complete),
            WorkflowState::Executing
        ));
        // No re-entry: already-Executing is not a NEW execution entry.
        assert!(!should_stamp_in_flight(
            Some(WorkflowState::Executing),
            WorkflowState::Executing
        ));
        // Anything else is not an entry into Executing.
        assert!(!should_stamp_in_flight(
            Some(WorkflowState::Planning),
            WorkflowState::Reviewing
        ));
        assert!(!should_stamp_in_flight(
            Some(WorkflowState::Planning),
            WorkflowState::Planning
        ));
    }

    #[test]
    fn halt_run_all_stamp_failed_selects_the_item_disposition() {
        // Regression (user report, 2026-12): a steer during Run-All stamped
        // the in-flight item Failed even while pre-planning. BOTH halt paths
        // must annotate ONLY (backlog 45dcf577: a halt is not a plan
        // abandonment — the one true `Failed`); the status is decided at
        // turn resolution (approval: the deliberately-kept run state; steer:
        // the intervention latch → handle_user_intervention).
        // Source-contract: halt_run_all needs a Tauri AppHandle.
        let body = fn_body(include_str!("run_all.rs"), "halt_run_all");
        assert!(
            body.contains("stamp_failed: bool"),
            "halt_run_all must take the stamp_failed disposition"
        );
        let stamp_branch = body
            .find("if stamp_failed {")
            .expect("the stamp_failed disposition branch must exist");
        let else_branch = body[stamp_branch..]
            .find("} else {")
            .expect("stamp_failed must have an else (the steer path)");
        let approval_block = &body[stamp_branch..stamp_branch + else_branch];
        let steer_block = &body[stamp_branch + else_branch..];
        assert!(
            approval_block.contains(".annotate("),
            "approval halt must annotate the note (sha + reason), not stamp"
        );
        assert!(
            !approval_block.contains("BacklogStatus::Failed"),
            "approval halt must NOT stamp a terminal status (backlog 45dcf577: \
             a halt is not a plan abandonment)"
        );
        assert!(
            steer_block.contains(".annotate("),
            "steer halt must annotate the note (sha + reason), not stamp"
        );
        assert!(
            !steer_block.contains("BacklogStatus::Failed"),
            "the steer path must NOT stamp a terminal status"
        );
        let annotate = steer_block
            .find(".annotate(")
            .expect("annotate present in the steer block");
        let latch_fill = steer_block
            .find("iv.item_id = Some(item_id.clone())")
            .expect("steer halt must hand the item id to the intervention latch");
        assert!(
            annotate < latch_fill,
            "annotate the note, then hand the item id to the latch"
        );
    }

    #[test]
    fn resolution_consumes_the_intervention_latch_before_any_stamping() {
        // Regression (user report, 2026-12): the soft-stop Finished of a
        // steered/interrupted turn resolved the in-flight item through the
        // plan-loop gate and marked it Failed/CantResolve. The latch must be
        // consumed FIRST — before the run-all branch — so the item is
        // requeued non-terminally and the run never auto-continues.
        let body = fn_body(include_str!("run_all.rs"), "on_main_turn_resolved");
        let latch = body
            .find("user_intervention")
            .expect("resolution must consume the intervention latch");
        let run_all_branch = body
            .find("Run-All takes priority")
            .expect("the run-all branch anchor comment must exist");
        assert!(
            latch < run_all_branch,
            "the intervention latch must be consumed BEFORE the run-all \
             resolution branch (an intervention is not a task failure)"
        );
        assert!(
            body.contains("handle_user_intervention("),
            "the latch path must route through handle_user_intervention"
        );
    }

    #[test]
    fn intervention_handler_is_never_terminal_and_never_continues() {
        // The handler must never stamp a terminal status and must never
        // dispatch/auto-feed the next item. A run-all item is KEPT
        // InFlight with its run STOPPED (backlog b83e891f: the turn that
        // completes the plan must still be able to resolve it through the
        // kept current_item pointer); a single-dispatch item is KEPT
        // InFlight too with its single_in_flight pointer restored (plan
        // cace17a6 — the same plan-tied rules resolve it).
        let body = fn_body(include_str!("run_all.rs"), "handle_user_intervention");
        assert!(
            !body.contains("BacklogStatus::Failed")
                && !body.contains("BacklogStatus::Done")
                && !body.contains("BacklogStatus::CantResolve"),
            "a user intervention must never stamp a terminal status"
        );
        assert!(
            !body.contains("run_all_dispatch_next") && !body.contains("auto_feed"),
            "the handler must never dispatch the next item or auto-feed"
        );
        assert!(
            !body.contains("end_run("),
            "the handler must STOP the run, never end it (backlog b83e891f): \
             the kept current_item pointer is what lets the resumed turn's \
             resolution resolve the item"
        );
        let identity_guard = body
            .find("interrupted ==")
            .expect("the run-identity match guard is present");
        let stop_set = body
            .find("r.stop.store(true")
            .expect("the handler must set the run's stop flag (the interrupt \
             path does not halt, so the handler is what stops the run)");
        assert!(
            identity_guard < stop_set,
            "the stop flag must be set only under the run-identity match \
             (review round-2 LOW A: never stop a run whose in-flight pointer \
             does not match the interrupted item — a deferred Run-All the \
             user started mid-turn is not ours to stop)"
        );
        assert!(
            !body[identity_guard..stop_set].contains("run_all.lock()"),
            "the stop flag is set under the SAME run_all hold as the identity \
             check (review round-1 LOW C): no re-acquisition window between \
             the check and the set"
        );
    }

    #[test]
    fn steer_halt_keeps_the_run_state_for_the_in_flight_item() {
        // Regression (backlog b83e891f, live 2027-01-07 — three dispatches
        // of item 45a4eb88): the steer arm of halt_run_all ENDED the run
        // after filling the latch, destroying the current_item pointer —
        // the user's natural resume continued the plan with no dispatch
        // pointer, the completing turn's resolution was blind, and the
        // item re-dispatched forever. The steer arm must mirror the
        // approval arm: annotate + latch + KEEP the run (the stop flag,
        // set above, prevents the next dispatch).
        // Source-contract: halt_run_all needs a Tauri AppHandle.
        let body = fn_body(include_str!("run_all.rs"), "halt_run_all");
        let stamp_branch = body
            .find("if stamp_failed {")
            .expect("the stamp_failed disposition branch must exist");
        let else_branch = body[stamp_branch..]
            .find("} else {")
            .expect("stamp_failed must have an else (the steer path)");
        let approval_block = &body[stamp_branch..stamp_branch + else_branch];
        let steer_block = &body[stamp_branch + else_branch..];
        assert!(
            !approval_block.contains("end_run("),
            "the approval arm keeps the run (the post-approval resolution needs it)"
        );
        assert!(
            !steer_block.contains("end_run("),
            "the steer arm must KEEP the run (backlog b83e891f): the kept \
             current_item pointer is what lets the resumed turn's resolution \
             resolve the item — the stop flag already prevents the next dispatch"
        );
        // The no-item idle case still ends the run (nothing to hold).
        let no_item = body
            .find("let Some(item_id) = &item_id else")
            .expect("the no-item arm must exist");
        let no_item_return = body[no_item..]
            .find("return;")
            .expect("the no-item arm returns");
        assert!(
            body[no_item..no_item + no_item_return].contains("end_run("),
            "a steer with no in-flight item still clears the idle run"
        );
    }

    #[test]
    fn heartbeat_redispatches_a_stalled_run_without_turn_resolution() {
        // Regression (2026-09-08 live incident, plan 0ed9d24f): item
        // a117e827 resolved done cleanly (null note) with 10 eligible
        // pending items, but no next item dispatched — the dispatch chain
        // stalled in an eprintln-only arm and NOTHING re-drove it: the
        // gate re-checks only on turn resolutions, and an idle main agent
        // produces none. The heartbeat is the class-level fix — while a
        // run is armed, a periodic check re-drives the dispatch when the
        // run is stalled (no current item, not compacting, main agent
        // idle, eligible items exist) WITHOUT any turn resolution.
        let body = fn_body(include_str!("run_all.rs"), "run_all_heartbeat");
        // Self-terminates when the run has ended (no zombie task).
        assert!(
            body.contains("break"),
            "the heartbeat must exit when the run has ended"
        );
        // The heartbeat consults the stall preconditions BEFORE
        // re-driving the dispatch.
        let stalled = body
            .find("run_all_stalled")
            .expect("the heartbeat checks the stall preconditions");
        let dispatch = body
            .find("run_all_dispatch_next")
            .expect("the heartbeat re-drives the dispatch");
        assert!(
            stalled < dispatch,
            "the stall check precedes the dispatch"
        );

        // The stall preconditions themselves (run_all_stalled), in order:
        // item in flight → not stalled; compacting → not stalled; busy
        // main agent → not stalled; no eligible item → not stalled.
        let check = fn_body(include_str!("run_all.rs"), "run_all_stalled");
        let in_flight = check
            .find("current_item")
            .expect("an item in flight means not stalled");
        let compacting = check
            .find("compacting")
            .expect("a running between-items compact means not stalled");
        let busy = check
            .find("has_running_descendants")
            .expect("a busy main agent or descendant means not stalled");
        let eligible = check
            .find("next_pending_eligible_excluding")
            .expect("no eligible item means not stalled");
        assert!(
            in_flight < compacting && compacting < busy && busy < eligible,
            "the stall preconditions are checked in order"
        );
        // The heartbeat never ends runs — end_run stays with the
        // resolution paths (a heartbeat ending runs would race the
        // normal lifecycle).
        assert!(
            !body.contains("end_run("),
            "the heartbeat must never end a run"
        );
    }

    #[test]
    fn heartbeat_spawned_for_the_runs_lifetime() {
        // The heartbeat only heals if it runs for the run's whole
        // lifetime: backlog_run_all must spawn it right after arming the
        // run state (and before the first dispatch — the first item's own
        // resolution is already covered by the gate; the heartbeat covers
        // every stall AFTER that).
        let body = fn_body(include_str!("backlog_cmds.rs"), "backlog_run_all");
        let armed = body
            .find("RunAllState {")
            .expect("the run state is armed here");
        let spawn = body
            .find("run_all_heartbeat")
            .expect("the heartbeat is spawned with the run");
        let first_dispatch = body
            .find("run_all_dispatch_next")
            .expect("the first dispatch");
        assert!(
            armed < spawn,
            "the heartbeat is spawned after the run state is armed"
        );
        assert!(
            spawn < first_dispatch,
            "the heartbeat is spawned before the first dispatch"
        );
    }

    #[test]
    fn busy_guard_deferral_is_logged_persistently() {
        // Regression (plan 0ed9d24f): the busy-guard deferral relied on
        // "the in-flight turn's terminal event re-invokes
        // on_main_turn_resolved" — which never happens when the busy
        // thing is a spawned lane or a stale descendant — and logged
        // only to stderr (invisible in the launched app). The deferral
        // must write the persistent run-all log; the recovery is the
        // heartbeat (see the test above), which re-drives the stalled
        // run within one period without any turn resolution. A separate
        // delayed retry was dropped: spawning it from inside
        // run_all_dispatch_next is async recursion (an opaque-type
        // cycle, E0391), and the heartbeat subsumes it at a better
        // cadence.
        let body = fn_body(include_str!("run_all.rs"), "run_all_dispatch_next");
        let busy_arm = body
            .find("main agent still busy")
            .expect("the busy-guard deferral arm");
        // The run_all_diag call wrapping the busy-arm message: its call
        // syntax precedes the message string, so search backwards from
        // the message.
        let logged = body[..busy_arm]
            .rfind("run_all_diag(")
            .expect("the deferral writes the persistent run-all log");
        let ret = body[busy_arm..]
            .find("return Ok(())")
            .expect("the deferral still returns Ok (the item stays pending)");
        assert!(
            logged < busy_arm + ret,
            "the deferral logs BEFORE returning"
        );
    }

    #[test]
    fn stall_arms_write_the_persistent_run_all_log() {
        // Regression (plan 0ed9d24f): every dispatch-chain stall arm
        // logged only to stderr — invisible in the launched app — which
        // is why the 2026-09-08 incident (a117e827) is unidentifiable
        // from artifacts. Every arm must also write a timestamped line
        // to .coding/logs/run-all.log via run_all_diag (the Q2-fixed
        // diag_line pattern: CWD-independent, project .coding/logs in
        // dev, temp_dir packaged).
        let helper = fn_body(include_str!("run_all.rs"), "run_all_diag");
        assert!(
            helper.contains("run-all.log"),
            "the helper writes .coding/logs/run-all.log"
        );
        // The busy-guard deferral + dispatch arms route through it.
        let dispatch_body = fn_body(include_str!("run_all.rs"), "run_all_dispatch_next");
        assert!(
            dispatch_body.contains("run_all_diag("),
            "the dispatch arms log persistently"
        );
        // The manager-lock RE-check busy arm moved into the dispatch-stage
        // helper — its persistent log line is pinned there too (the old
        // whole-function assert covered both arms; the decomposition split
        // them).
        let dispatch_stage = fn_body(include_str!("run_all.rs"), "dispatch_run_all_to_main");
        assert!(
            dispatch_stage.contains("run_all_diag("),
            "the re-check busy arm (main agent became busy during checkpoint) \
             logs persistently"
        );
        // The between-items compact chain routes through it.
        let compact_body = fn_body(include_str!("run_all.rs"), "compact_then_dispatch_next");
        assert!(
            compact_body.contains("run_all_diag("),
            "the between-items compact chain logs persistently"
        );
    }

    #[test]
    fn intervention_keeps_a_run_all_item_in_flight_with_its_run() {
        // Regression (backlog b83e891f): handle_user_intervention requeued
        // an InFlight run-all item and ended the run — the plan stayed
        // active, the user's resume continued it with no dispatch
        // pointer, and the completing turn's resolution was blind (run
        // gone, latch consumed, single_in_flight empty): the item
        // stranded and re-dispatched forever. The run-all arm must KEEP
        // the item InFlight (InFlight ⇔ plan active, the status
        // contract); the single-dispatch arm keeps the item InFlight too
        // with its pointer restored (plan cace17a6); only a run-all item
        // whose run was drained before this resolution requeues (no
        // pointer left — the queue is its recovery).
        let body = fn_body(include_str!("run_all.rs"), "handle_user_intervention");
        let run_all_arm = body
            .find("BacklogStatus::InFlight if from_run_all")
            .expect("the run-all InFlight arm must exist (split from the \
             single-dispatch arm by the item's source)");
        let single_arm = body[run_all_arm..]
            .find("BacklogStatus::InFlight if !from_run_all")
            .expect("the single-dispatch InFlight arm must follow");
        let arm = &body[run_all_arm..run_all_arm + single_arm];
        assert!(
            arm.contains(".annotate("),
            "the run-all arm records the reason in the note"
        );
        assert!(
            !arm.contains(".transition("),
            "a run-all item stays InFlight — the plan is active (the \
             contract); requeueing it stranded the item (backlog b83e891f)"
        );
        assert!(
            arm.contains("interrupted_run_active"),
            "the keep-InFlight disposition requires the identity-matched run \
             to still be active (review round-1 LOW B): a drained run's \
             pointer is gone — the item requeues instead of stranding at \
             InFlight with no recovery"
        );
        // The drained-run arm still requeues: a run drained before this
        // resolution has no pointer left — the queue (+ auto-feed) is its
        // only recovery (review round-1 LOW B).
        let after_single = &body[run_all_arm + single_arm..];
        assert!(
            after_single
                .find("store.transition(id, BacklogStatus::Pending")
                .is_some(),
            "the drained-run item still requeues to Pending"
        );
    }

    #[test]
    fn intervention_keeps_a_single_dispatch_item_in_flight() {
        // Regression (plan cace17a6, user request 2027-01-09 — item
        // 0296d448 steered mid-plan): the single-dispatch arm requeued the
        // item to Pending AND consumed the single_in_flight pointer, so
        // the continuation turn that completed the plan resolved nothing —
        // the item sat Pending with finished work (the same out-of-sync
        // b83e891f fixed for run-all items). The arm must KEEP the item
        // InFlight (InFlight ⇔ plan active, the status contract) and
        // RESTORE the pointer so the completing turn's resolution
        // resolves it through the normal plan-tied rules.
        let body = fn_body(include_str!("run_all.rs"), "handle_user_intervention");
        let single_arm = body
            .find("BacklogStatus::InFlight if !from_run_all")
            .expect("the single-dispatch InFlight arm must exist (keep + \
              restore, split from the run-all arm by the item's source)");
        let drained_arm = body[single_arm..]
            .find(
                "BacklogStatus::InFlight if from_run_all && !interrupted_run_active",
            )
            .expect("the drained-run requeue arm must follow (the queue is \
              genuinely the only recovery when the run was drained before \
              this resolution — review round-1 LOW B)");
        let arm = &body[single_arm..single_arm + drained_arm];
        assert!(
            arm.contains(".annotate("),
            "the single-dispatch arm records the reason in the note"
        );
        assert!(
            !arm.contains(".transition("),
            "a single-dispatch item stays InFlight — its plan is active \
             (the contract); requeueing it desynced the item from the plan \
             (plan cace17a6: item 0296d448 sat Pending with finished work)"
        );
        assert!(
            arm.contains("single_in_flight"),
            "the arm restores the single_in_flight pointer — the consumed \
             pointer is what blinded the completing turn's resolution"
        );
        assert!(
            arm.contains("slot.is_none()"),
            "the restore is check-and-set under one lock hold (the \
             96e2862a round-2 pattern): never clobber a concurrent ▶ \
             dispatch that set the slot inside the take→restore window"
        );
    }

    #[test]
    fn non_closure_resolution_keeps_the_run_armed() {
        // Regression (the amplifier, live 2027-01-07 twice — items a6a7727a
        // and 207dc316): the still-open arm ended a NATURALLY-running run
        // (no stop requested) on the first turn end that did not close the
        // plan loop. But the agent routinely recovers on its own (the
        // auto-continue resumes mid-plan turn ends; reviewer-finish
        // notifications resume the closing sequence) and finishes the plan
        // — the completing turn's resolution then found run_all == None and
        // the item stranded in_flight with the next item never dispatching.
        // The arm must KEEP the run armed for stopped AND natural runs
        // alike (backlog b83e891f generalized): no dispatch happens without
        // a successful resolution, and the post-resolution stopped check
        // still ends an intervention-stopped run without dispatching.
        // The arm moved into annotate_run_all_waiting (the per-outcome
        // helper); the pin follows the code. The helper runs to the end of
        // its body — no early return (the caller's Waited disposition does
        // the returning).
        let body = fn_body(include_str!("run_all.rs"), "annotate_run_all_waiting");
        let still_open = body
            .find("plan_open_note(main_state)")
            .expect("the still-open arm must annotate");
        let still_open_arm = &body[still_open..];
        assert!(
            !still_open_arm.contains("end_run"),
            "a non-closure resolution must NOT end the run — the item's plan \
             is still active and the turn that completes it must resolve it \
             through the kept run pointer (the amplifier: two live incidents)"
        );
        assert!(
            still_open_arm.contains("already_waiting"),
            "the annotate must be guarded against repetition (annotate \
             APPENDS — repeated non-closures would grow the note unboundedly)"
        );
        assert!(
            still_open_arm.contains("n.contains(note.as_str())"),
            "the guard must key on the EXACT note text (review LOW-3) — a \
             coarse marker would suppress different-flavor notes (a later \
             abort reason, a workflow-state progression) and lose the \
             information"
        );
        assert!(
            still_open_arm.contains("emit_backlog_changed"),
            "the arm must emit backlog-changed itself (end_run used to emit; \
             the UI must still refresh the note)"
        );
        // The intervention semantics survive: the post-resolution stopped
        // check still ends a stopped run (without dispatching next) — it
        // lives in resolve_run_all_turn AFTER the disposition match, so a
        // Waited disposition returns before the stopped check is reached.
        let turn = fn_body(include_str!("run_all.rs"), "resolve_run_all_turn");
        let waited = turn
            .find("RunAllItemDisposition::Waited => return")
            .expect("the Waited disposition returns early");
        let stopped_check = turn
            .find("if stopped {")
            .expect("the post-resolution stopped check must exist");
        assert!(
            stopped_check > waited,
            "the stopped check follows the arms — it ends a stopped run only \
             after the item resolves"
        );
    }

    #[test]
    fn turn_failure_resolution_keeps_the_run_armed() {
        // Regression (the amplifier, live 2027-01-07 — item a6a7727a): a
        // turn aborted on 3 consecutive tool errors annotated "run halted"
        // and ENDED the run — yet the auto-continue resumed the agent, the
        // plan finished cleanly (finish → Complete, review PASS), and the
        // completing turn's resolution was blind (run_all == None). The
        // arm must keep the run armed; the next turn resolution re-checks.
        // The arm moved into annotate_run_all_error (the per-outcome
        // helper); the pin follows the code. The helper runs to the end of
        // its body — no early return (the caller's Waited disposition does
        // the returning).
        let body = fn_body(include_str!("run_all.rs"), "annotate_run_all_error");
        let error_arm = body
            .find("\"turn failed\"")
            .expect("the terminal-error arm must exist");
        let error_arm_body = &body[error_arm..];
        assert!(
            !error_arm_body.contains("end_run"),
            "a turn-failure resolution must NOT end the run — the \
             auto-continue routinely resumes the agent and the plan closes \
             on a later turn (the amplifier: the a6a7727a incident)"
        );
        assert!(
            error_arm_body.contains("run waits"),
            "the note must say the run WAITS (the next turn resolution \
             re-checks), not that it halted"
        );
        assert!(
            error_arm_body.contains("already_waiting"),
            "the annotate must be guarded against repetition (annotate \
             APPENDS — repeated aborts would grow the note unboundedly)"
        );
        assert!(
            error_arm_body.contains("n.contains(note.as_str())"),
            "the guard must key on the EXACT note text (review LOW-3) — a \
             different abort reason must still annotate"
        );
        assert!(
            error_arm_body.contains("emit_backlog_changed"),
            "the arm must emit backlog-changed itself (end_run used to emit; \
             the UI must still refresh the note)"
        );
    }

    #[test]
    fn single_dispatch_non_closure_restores_the_pointer() {
        // Regression (the amplifier's single-dispatch mirror): the
        // non-run-all branch takes single_in_flight UNCONDITIONALLY, so a
        // non-closure resolution consumed the pointer — the completing
        // turn's resolution was blind and the ▶-dispatched item stranded
        // in_flight. The None-status arm must RESTORE the pointer so the
        // next resolution re-checks the same item; auto-feed suppression
        // (resolved_terminally = false) stays. The branch moved into
        // resolve_single_dispatch_turn (the per-outcome helper); the pin
        // follows the code.
        let body = fn_body(include_str!("run_all.rs"), "resolve_single_dispatch_turn");
        let take = body
            .find("single_in_flight")
            .expect("the single-dispatch branch must take the pointer");
        let not_resolved = body
            .find("resolved_terminally = false")
            .expect("the not-resolved arm must exist");
        assert!(
            take < not_resolved,
            "the take precedes the not-resolved arm"
        );
        assert!(
            body[take..not_resolved].contains("= Some(in_flight_id)"),
            "the not-resolved arm must RESTORE the consumed pointer — the \
             completing turn's resolution must re-check the same item, not \
             run blind (the single-dispatch amplifier)"
        );
        assert!(
            body[take..not_resolved].contains("already_waiting"),
            "the single-dispatch arm must guard its annotate too (review \
             LOW-2) — with the pointer restored, every main turn end \
             re-checks this arm and an unguarded annotate would grow the \
             note once per turn"
        );
        assert!(
            body[take..not_resolved].contains("n.contains(note.as_str())"),
            "the single-dispatch guard must key on the EXACT note text \
             (review LOW-3), mirroring the run-all arms"
        );
        assert!(
            body[take..not_resolved].contains("slot.is_none()"),
            "the restore must be a check-and-set (review LOW-4) — a \
             concurrent ▶ dispatch landing inside the take→restore window \
             must win, not be clobbered by the restore"
        );
    }

    #[test]
    fn in_flight_stamp_only_applies_to_pending_items() {
        // The Executing-entry stamp is idempotent and only lifts still-
        // Pending items; the note (checkpoint sha) passes through untouched,
        // and the same moment records the item↔plan linkage (backlog
        // 45dcf577): the ROOT plan id the item's status is derived from —
        // plus its title (backlog f45513b2), read from the plan file's
        // heading so the Backlog tab shows a human-friendly identifier.
        let body = fn_body(include_str!("run_all.rs"), "stamp_backlog_in_flight");
        assert!(
            body.contains("Some((BacklogStatus::Pending, note))"),
            "only a still-Pending item is stamped InFlight"
        );
        assert!(
            body.contains("store.transition(&id, BacklogStatus::InFlight, note)"),
            "the stamp preserves the note (checkpoint sha)"
        );
        assert!(
            body.contains("store.set_plan_id(&id, top_plan_id, plan_title.as_deref())"),
            "the Executing-entry stamp must also record the item↔plan linkage (id + title)"
        );
    }

    #[test]
    fn on_main_turn_resolved_is_plan_tied() {
        // Backlog 45dcf577: status ⇔ plan lifecycle. The resolution marks
        // `Done` ONLY via the plan-loop gate, `Failed` ONLY on root-plan
        // abandonment, and leaves the item untouched (annotate + the run
        // waits) on every other turn end — never `CantResolve`, never a
        // rollback.
        // Source-contract: the resolution paths need a Tauri AppHandle. The
        // contract is split across the per-outcome helpers that now own each
        // piece (the decomposition kept every assertion string).
        let success = fn_body(include_str!("run_all.rs"), "run_all_success_disposition");
        // Done: the gate arm (the gate + the linkage-guarded flip).
        assert!(success.contains("plan_loop_allows_done(main_state, true)"));
        assert!(success.contains("_ if plan_abandoned =>"));
        let done_flip = fn_body(include_str!("run_all.rs"), "flip_done_if_linked");
        assert!(done_flip.contains("commit_success(root.clone(), item.clone())"));
        assert!(done_flip.contains("BacklogStatus::Done"));
        let failed_flip = fn_body(
            include_str!("run_all.rs"),
            "flip_failed_if_abandonment_linked",
        );
        // Failed: ONLY the abandonment arms.
        assert!(failed_flip.contains("\"plan abandoned (abandon_plan)\".to_string()"));
        // Every other turn end: no transition — annotate + the run waits
        // (the next turn resolution re-checks).
        let waiting = fn_body(include_str!("run_all.rs"), "annotate_run_all_waiting");
        assert!(waiting.contains("plan_open_note(main_state)"));
        assert!(waiting.contains(".annotate(&item.id,"));
        // No spurious setters survive: no CantResolve, no rollback call —
        // checked on every helper that could transition or annotate,
        // INCLUDING the single-dispatch disposition (it performs the
        // Done/Failed transitions on this path) and its apply site.
        let single = fn_body(include_str!("run_all.rs"), "resolve_single_dispatch_turn");
        let single_disp = fn_body(include_str!("run_all.rs"), "single_dispatch_disposition");
        for body in [
            &success, &done_flip, &failed_flip, &waiting, &single_disp, &single,
        ] {
            assert!(
                !body.contains("BacklogStatus::CantResolve"),
                "the harness resolution paths must never stamp CantResolve \
                 (the explicit dead end is the agent's backlog_status tool)"
            );
            assert!(
                !body.contains("rollback("),
                "a turn that ends mid-plan must NOT roll back — the work stays \
                 in the tree for the resumed session"
            );
        }
        // The single-dispatch path auto-feeds only past a terminally
        // resolved item.
        assert!(single.contains("resolved_terminally"));
        assert!(single.contains("if resolved_terminally && state.backlog.auto_feed"));
    }

    #[test]
    fn complete_state_turn_end_with_landed_plan_dispatches_next() {
        // Regression (backlog e33a07fd, live 2027-01-08): a run-all session
        // finished an item's plan through the full closing sequence (tests
        // → review → commit 08a9295 → finish) but the next pending item was
        // never dispatched — the session sat idle with the run active,
        // requiring a manual restart. Root cause: the gate requires
        // loop_evidence=true, but a resolution that slides to a
        // notification-resumed turn sees (Complete, false) — the deferral
        // (a descendant still running at the finish turn's end) is
        // discarded when the descendant's completion notification starts a
        // new turn (on_started clears pending_finished), and that turn
        // observes no workflow transition. Complete is a RESTING state: no
        // transition ever fires again, no auto-continue — the non-closure
        // arm's annotate-and-return waits forever. The fix: a verifiably
        // LANDED plan counts as a closed loop — orphan_work_landed (the
        // drain's done-orphan predicate: the item's plan file all-checked
        // + a commit after the pre-item checkpoint) proves the item's OWN
        // plan completed on an earlier turn.
        // The landed predicate + the gate scrutinee live in
        // run_all_success_disposition (the per-outcome helper); the pin
        // follows the code.
        let body = fn_body(include_str!("run_all.rs"), "run_all_success_disposition");
        assert!(
            body.contains("orphan_work_landed(state, item.plan_id.as_deref(), item.note.as_deref())"),
            "the run-all arm must consult the landed predicate — a \
             Complete-state turn end with a verifiably-landed plan must \
             resolve the item and dispatch the next, not wait forever (the \
             slid-resolution stall, backlog e33a07fd)"
        );
        assert!(
            body.contains("loop_evidence || closed_earlier"),
            "a landed plan must count as a closed loop even without \
             this-turn transition evidence — the gate scrutinee must OR in \
             closed_earlier"
        );
        // The never-planned hole (2026-08-20) stays plugged: the
        // non-closure arm still annotates + returns for a Complete state
        // with NO landed evidence (a turn that never planned).
        let waiting = fn_body(include_str!("run_all.rs"), "annotate_run_all_waiting");
        assert!(
            waiting.contains("plan_open_note(main_state)"),
            "the non-closure arm must keep annotating (the run waits) for \
             non-landed turn ends — the amplifier behavior is unchanged"
        );
        // A blocked Done flip (backlog 5bb1e4cd) must not count the item
        // done — the done-counter bump is gated on an actual transition.
        let turn = fn_body(include_str!("run_all.rs"), "resolve_run_all_turn");
        assert!(
            turn.contains("terminal_resolution"),
            "the done-counter bump must be gated on an actual transition — \
             a blocked flip must not count done"
        );
    }

    #[test]
    fn slid_resolution_with_requeued_pending_item_dispatches_next() {
        // Regression (backlog 8a6bcece, review LOW-3 2027-01-08, found in
        // the e33a07fd closing review): the in-flight item externally
        // re-queued to Pending mid-run (the 5bb1e4cd-style backlog.jsonl
        // edit — the store sees Pending; the in-memory current_item
        // pointer still references it) AND the completing turn's
        // resolution slid (the e33a07fd slide) — the notification turn
        // lands at (Complete, false) with the item Pending:
        // closed_earlier is false (it requires InFlight) and the waiting
        // arm's annotate-and-return stalled the run forever (Complete is
        // a resting state; nothing ever re-checks). The fix: a dedicated
        // arm at (Complete, false) gated on the item being Pending —
        // landed evidence resolves Done (the drain's done-orphan
        // semantics), otherwise the stale pointer is cleared and the
        // next pending item dispatched (the item is already Pending —
        // eligible for re-dispatch).
        // The arm moved into run_all_success_disposition (the match) +
        // resolve_slid_pending_item (the recovery); the pin follows the
        // code. The arm-ordering contract: the (Complete, false) + Pending
        // arm precedes the catch-all arm that delegates to the waiting
        // helper.
        let body = fn_body(include_str!("run_all.rs"), "run_all_success_disposition");
        let arm = "(Some(WorkflowState::Complete), false)";
        let arm_ix = body
            .find(arm)
            .expect("the (Complete, false) re-queued-item arm");
        let catch_all_ix = body
            .find("(main_state, _)")
            .expect("the catch-all arm (delegates to the waiting helper)");
        // The arm must intercept BEFORE the catch-all — the catch-all
        // matches (Complete, false) too, and its annotate-and-return
        // (annotate_run_all_waiting) is exactly the stall being fixed.
        assert!(
            arm_ix < catch_all_ix,
            "the re-queued-item arm must precede the waiting arm — the \
             waiting arm would otherwise swallow the (Complete, false) + \
             Pending landing and stall the run (backlog 8a6bcece)"
        );
        let region = &body[arm_ix..catch_all_ix];
        assert!(
            region.contains("item.status == BacklogStatus::Pending"),
            "the arm must be gated on the item being Pending — the \
             re-queued-item recovery, not a general Complete-state bypass"
        );
        // The recovery body lives in resolve_slid_pending_item.
        let recovery = fn_body(include_str!("run_all.rs"), "resolve_slid_pending_item");
        assert!(
            recovery.contains("orphan_work_landed("),
            "the arm must consult the landed predicate — work that \
             verifiably landed (the drain's done-orphan evidence) resolves \
             Done, never re-dispatched as duplicate work (the 6c6966b9 \
             class)"
        );
        // The 2026-08-20 never-planned hole stays plugged: the Done flip
        // is gated on the landed predicate AND a still-Pending re-verify —
        // a Pending item with no plan_id never resolves Done (the
        // predicate requires a plan_id; anything unverifiable is false).
        assert!(
            recovery.contains("landed && still_pending"),
            "the Done flip must be gated on the landed predicate AND the \
             still-Pending re-verify — a Pending item with no plan_id must \
             not resolve Done (the 2026-08-20 hole)"
        );
        // The arm must FALL THROUGH to the tail's pointer clear +
        // dispatch — an early exit here is the stall being fixed (the
        // helper returns a disposition; the caller's Proceed arm does the
        // falling through).
        assert!(
            !region.contains("return;"),
            "the arm must fall through to the pointer clear + dispatch — \
             an early exit would stall the run exactly like the waiting \
             arm did"
        );
    }

    #[test]
    fn slid_resolution_spawned_lane_requeued_pending_recovers() {
        // Regression (backlog 8a6bcece, review LOW-3 2027-01-08): the
        // spawned-lane mirror — the same (Complete, false) + Pending
        // landing stalled the lane forever (the waiting arm exits
        // early; the spawned run never resolves; the run waits for the
        // lane). The same recovery, with worktree-aware landed evidence
        // (the plan file lives in the worktree's plans dir; the commits
        // are on the item's own branch).
        let body = fn_body(include_str!("run_all.rs"), "on_spawned_turn_resolved");
        let arm = "(Some(WorkflowState::Complete), false)";
        let arm_ix = body
            .find(arm)
            .expect("the (Complete, false) re-queued-item arm");
        let waiting_ix = body
            .find("plan_open_note(ws)")
            .expect("the waiting arm");
        assert!(
            arm_ix < waiting_ix,
            "the spawned re-queued-item arm must precede the waiting arm — \
             the waiting arm would otherwise swallow the (Complete, false) \
             + Pending landing and stall the lane (backlog 8a6bcece)"
        );
        let region = &body[arm_ix..waiting_ix];
        assert!(
            region.contains("item.status == BacklogStatus::Pending"),
            "the arm must be gated on the item being Pending — the \
             re-queued-item recovery, not a general Complete-state bypass"
        );
        assert!(
            region.contains("orphan_work_landed_at("),
            "the arm must consult the worktree-aware landed predicate — \
             the plan file lives in the worktree's plans dir and the \
             commits are on the item's own branch"
        );
        assert!(
            region.contains("land_spawned_branch"),
            "the landed branch must land the item's branch app-side — the \
             work counts as Done only once it is in main (a conflict \
             keeps the branch + annotates)"
        );
        assert!(
            region.contains("landed && still_pending"),
            "the Done flip must be gated on the landed predicate AND the \
             still-Pending re-verify (the 2026-08-20 hole stays plugged)"
        );
        // Both branches remove the worktree: the spawned run is over and
        // the worktree has no future owner — a stale wt/runall-* branch
        // would block every later lane dispatch of the re-queued item
        // ("branch already exists").
        assert!(
            region.contains("remove_worktree = true"),
            "the arm must remove the worktree — the spawned run is over; a \
             stale wt/runall-* branch would block every later lane \
             dispatch of the re-queued item"
        );
        assert!(
            !region.contains("return;"),
            "the arm must fall through to the spawned-run cleanup + \
             dispatch — an early exit would stall the lane"
        );
    }

    #[test]
    fn done_transitions_are_guarded_on_status_and_plan_linkage() {
        // Regression (backlog 5bb1e4cd, live 2027-01-07): a steer-pivot
        // left the in-memory pointer referencing a re-queued (Pending)
        // item while an unrelated plan ran; at that plan's finish
        // resolution the success arm flipped the item to Done with NO
        // status guard — Pending → Done is a legal row, and the
        // transition wiped the re-queue note. The Done transition must be
        // guarded in BOTH the run-all gate arm and the single-dispatch
        // Done row: only a currently-InFlight item that owns the
        // completing plan (item.plan_id == the workflow's top_plan_id,
        // kept after finish) may flip.
        // The two Done sites now live in their owning helpers (the run-all
        // gate arm in flip_done_if_linked, the single-dispatch Done row in
        // single_dispatch_disposition); the pin follows the code.
        let run_all = fn_body(include_str!("run_all.rs"), "flip_done_if_linked");
        let single = fn_body(include_str!("run_all.rs"), "single_dispatch_disposition");
        for (name, body) in [
            ("flip_done_if_linked", &run_all),
            ("single_dispatch_disposition", &single),
        ] {
            assert!(
                body.contains("plan_linkage_allows_done"),
                "the guard must be consulted at both Done sites ({name} is \
                 one of them) — the run-all gate arm and the single-dispatch \
                 Done row"
            );
        }
        // The guard must precede commit_success in the run-all arm: a
        // blocked flip must not commit the unrelated plan's work under the
        // item's name.
        let guard = run_all
            .find("plan_linkage_allows_done")
            .expect("the run-all gate arm must consult the guard");
        let commit = run_all
            .find("commit_success(root.clone(), item.clone())")
            .expect("the gate arm must commit");
        assert!(
            guard < commit,
            "the guard must run BEFORE commit_success — a blocked flip must \
             not commit under the item's name"
        );
        // (review LOW-2, 2026-09-08) The blocked flip must be observable on
        // the item in BOTH arms — the run-all gate arm's else annotates,
        // mirroring the single-dispatch blocked row.
        for (name, body) in [
            ("flip_done_if_linked", &run_all),
            ("single_dispatch_disposition", &single),
        ] {
            assert!(
                body.contains("completing plan is not this item's plan — item kept, no transition"),
                "the blocked flip must annotate in both the run-all gate arm \
                 and the single-dispatch blocked row ({name})"
            );
        }
    }

    #[test]
    fn failed_transitions_are_guarded_on_status_and_plan_linkage() {
        // Regression (backlog bba2c82d, review LOW-1 2027-01-08): the
        // plan_abandoned arms transitioned the pointer-referenced item to
        // Failed with NO status/linkage guard — the same steer-pivot
        // damage class as 5bb1e4cd: a stale in-memory pointer referencing
        // a re-queued (Pending) item while an UNRELATED plan ran; that
        // plan was abandoned → the re-queued item flipped to Failed and
        // its re-queue note was wiped (Pending → Failed is a legal row,
        // src/backlog.rs:308-313). The Failed transition must be guarded
        // at ALL SIX sites: only a currently-InFlight item whose plan_id
        // IS the abandoned plan may flip.
        let spawned = fn_body(include_str!("run_all.rs"), "on_spawned_turn_resolved");
        let spawned_guards = spawned.matches("plan_linkage_allows_failed").count();
        assert!(
            spawned_guards >= 2,
            "the spawned-lane resolution must guard BOTH Failed arms (the \
             success path and the failure path) — found {spawned_guards}"
        );
        // The run-all sites deduped into the shared helper
        // (flip_failed_if_abandonment_linked — the success match and the
        // failure path were byte-identical); the single-dispatch guard
        // computation lives in single_dispatch_disposition. The pin follows
        // the code: both owners must consult the guard.
        let shared = fn_body(
            include_str!("run_all.rs"),
            "flip_failed_if_abandonment_linked",
        );
        let single = fn_body(include_str!("run_all.rs"), "single_dispatch_disposition");
        for (name, body) in [
            ("flip_failed_if_abandonment_linked", &shared),
            ("single_dispatch_disposition", &single),
        ] {
            assert!(
                body.contains("plan_linkage_allows_failed"),
                "{name} must consult the Failed linkage guard — the \
                 run-all success/failure arms (deduped into the shared \
                 helper) and the single-dispatch guard computation"
            );
        }
        // The single-dispatch Failed rows consult the pre-computed guard
        // (may_flip_failed) in BOTH the success and the failure path.
        let row_guards = single.matches("plan_abandoned && may_flip_failed").count();
        assert!(
            row_guards >= 2,
            "the single-dispatch Failed rows must consult may_flip_failed \
             in both paths — found {row_guards}"
        );
        // The blocked flip must NOT annotate: a re-queued Pending item's
        // note is recovery evidence (the re-queue reason) — annotating
        // would wipe it, the exact damage this guard prevents. Unlike the
        // Done blocked row (an InFlight item whose note is transient),
        // the Failed blocked case leaves the item untouched: a dedicated
        // arm (no transition, no annotate, eprintln diagnostic) before
        // the waiting arm.
        for (name, body) in [
            ("on_spawned_turn_resolved", &spawned),
            ("flip_failed_if_abandonment_linked", &shared),
            ("single_dispatch_disposition", &single),
        ] {
            assert!(
                body.contains("abandoned plan is not this item's plan"),
                "the blocked Failed flip in {name} must have its own arm \
                 with the eprintln diagnostic — no transition, no annotate"
            );
        }
    }

    #[test]
    fn abandonment_evidence_captures_the_abandoned_plan_id() {
        // (backlog bba2c82d) The linkage evidence for the Failed guard
        // DIFFERS from the Done guard: after abandonment the workflow
        // popped to Planning and main_agent_top_plan_id no longer names
        // the abandoned plan — the id must be captured from the turn's
        // transition evidence. The forwarder tracks the PREVIOUS top
        // plan id per agent (updated on every WorkflowStateChanged) and
        // passes the pre-event top to the latch at the abandonment
        // transition (the event's own top_plan_id is the POST-pop top).
        let forwarder = include_str!("events.rs");
        assert!(
            forwarder.contains("prev_top_plan_id"),
            "the forwarder must track the previous top plan id per agent"
        );
        let note_site = forwarder
            .find("note_plan_abandoned")
            .expect("the forwarder must latch plan abandonment");
        // Pin the read-before-update ordering concretely (review LOW-3):
        // the pre-event top must be READ before the tracking map is
        // updated for this event — the abandonment event's own
        // top_plan_id is the post-pop top, so reading after the insert
        // would degrade the guard to blind.
        let read_site = forwarder
            .find("let prev_top = prev_top_plan_id.get")
            .expect("the forwarder must read the pre-event top");
        let insert_site = forwarder
            .find("prev_top_plan_id.insert")
            .expect("the forwarder must update the tracking map");
        assert!(
            read_site < insert_site,
            "the pre-event top read must precede the map update — reading \
             after the insert would capture the post-pop top (None)"
        );
        assert!(
            read_site < note_site && insert_site < note_site,
            "both the read and the update must precede the latch call"
        );
    }

    #[test]
    fn plan_linkage_allows_done_matrix() {
        // (backlog 5bb1e4cd) The Done-transition guard matrix. The live
        // incident: a re-queued (Pending) item flipped to Done and its note
        // was wiped — the status guard dominates every linkage outcome.
        // The happy path: InFlight + the completing plan is the item's.
        assert!(plan_linkage_allows_done(
            BacklogStatus::InFlight,
            Some("X"),
            Some("X")
        ));
        // The regression: a Pending item must stay pending regardless of
        // the linkage (the steer-pivot incident — the item was re-queued
        // by an external .coding/backlog.jsonl edit while the in-memory
        // pointer kept referencing it).
        assert!(!plan_linkage_allows_done(
            BacklogStatus::Pending,
            Some("X"),
            Some("Y")
        ));
        assert!(!plan_linkage_allows_done(
            BacklogStatus::Pending,
            Some("X"),
            Some("X")
        ));
        // A mismatched plan (a steer-pivot completing an unrelated plan
        // while the dispatched item waits).
        assert!(!plan_linkage_allows_done(
            BacklogStatus::InFlight,
            Some("X"),
            Some("Y")
        ));
        // The item never planned; the completing plan is not its (the
        // 2026-08-20 hole — a turn that never planned must not resolve).
        assert!(!plan_linkage_allows_done(
            BacklogStatus::InFlight,
            None,
            Some("Y")
        ));
        // Completed plan unreadable (no main agent / no plan id): the
        // status guard alone — no happy-path regression.
        assert!(plan_linkage_allows_done(
            BacklogStatus::InFlight,
            Some("X"),
            None
        ));
        assert!(plan_linkage_allows_done(BacklogStatus::InFlight, None, None));
        // Terminal statuses never flip.
        assert!(!plan_linkage_allows_done(
            BacklogStatus::Done,
            Some("X"),
            Some("X")
        ));
        assert!(!plan_linkage_allows_done(
            BacklogStatus::Failed,
            Some("X"),
            Some("X")
        ));
    }

    #[test]
    fn plan_linkage_allows_failed_matrix() {
        // (backlog bba2c82d) The Failed-transition guard matrix — the
        // mirror of the Done matrix above. The live incident (review
        // LOW-1, 2027-01-08): a stale pointer referencing a re-queued
        // (Pending) item while an UNRELATED plan ran; that plan was
        // abandoned → the item flipped to Failed and its re-queue note
        // was wiped. The status guard dominates every linkage outcome.
        // The happy path: InFlight + the abandoned plan is the item's.
        assert!(plan_linkage_allows_failed(
            BacklogStatus::InFlight,
            Some("X"),
            Some("X")
        ));
        // The regression: a Pending item must stay Pending regardless
        // of the linkage (the re-queue note is recovery evidence).
        assert!(!plan_linkage_allows_failed(
            BacklogStatus::Pending,
            Some("X"),
            Some("X")
        ));
        assert!(!plan_linkage_allows_failed(
            BacklogStatus::Pending,
            Some("X"),
            Some("Y")
        ));
        // A mismatched plan (an unrelated plan abandoned while the
        // dispatched item waits on its own).
        assert!(!plan_linkage_allows_failed(
            BacklogStatus::InFlight,
            Some("X"),
            Some("Y")
        ));
        // The item never planned; the abandoned plan is not its.
        assert!(!plan_linkage_allows_failed(
            BacklogStatus::InFlight,
            None,
            Some("Y")
        ));
        // Abandoned plan id not captured (the evidence predates the
        // tracking): the status guard alone — no happy-path regression.
        assert!(plan_linkage_allows_failed(
            BacklogStatus::InFlight,
            Some("X"),
            None
        ));
        assert!(plan_linkage_allows_failed(
            BacklogStatus::InFlight,
            None,
            None
        ));
        // Terminal statuses never flip.
        assert!(!plan_linkage_allows_failed(
            BacklogStatus::Done,
            Some("X"),
            Some("X")
        ));
        assert!(!plan_linkage_allows_failed(
            BacklogStatus::Failed,
            Some("X"),
            Some("X")
        ));
    }

    #[test]
    fn checkpoint_failure_leaves_the_item_pending() {
        // Backlog 45dcf577: a git checkpoint failure is infrastructure, not
        // a plan abandonment — the item never left Pending (the
        // Executing-entry stamp never ran) and must stay queued with the
        // reason in its note.
        // Source-contract: the checkpoint stage needs a Tauri AppHandle
        // (end_run on failure), so the wiring is pinned on source.
        let body = fn_body(include_str!("run_all.rs"), "checkpoint_run_all_item");
        assert!(
            body.contains(".annotate(&item.id, &format!(\"git checkpoint failed: {e}\"))"),
            "checkpoint failure must annotate the reason, not stamp"
        );
        assert!(
            !body.contains("BacklogStatus::Failed"),
            "dispatch must never stamp Failed (backlog 45dcf577: failure means \
             exactly 'the plan was abandoned', and no plan exists at dispatch time)"
        );
    }

    #[test]
    fn run_all_dispatch_rides_the_main_agent_surface() {
        // Backlog 66c65db9: a run-all/unattended session's tool surface IS
        // the main agent's normal state-filtered surface — the dispatch
        // sends a plain AgentCommand::Prompt and never restricts tools (no
        // allow-list, no filter, no schema manipulation). Combined with the
        // filter tests (backlog_add visible in every workflow state and,
        // since 66c65db9, inside skill allow-lists via the always-available
        // set), a direct user request to queue a backlog item is never
        // impossible mid-run — the 2026-12-30 incident (the surface had NO
        // backlog_add while backlog_list/backlog_status were present)
        // cannot recur. Source-contract: the dispatch stage needs a Tauri
        // AppHandle, so the wiring is pinned on source.
        let body = fn_body(include_str!("run_all.rs"), "dispatch_run_all_to_main");
        assert!(
            body.contains("AgentCommand::Prompt"),
            "the dispatch must send a plain Prompt to the main agent"
        );
        for banned in [
            "set_reviewer_allowlist",
            "tool_allowlist",
            "ToolFilter",
            ".schemas(",
        ] {
            assert!(
                !body.contains(banned),
                "the run-all dispatch must not restrict the tool surface \
                 (found '{banned}' in the dispatch body)"
            );
        }
    }

    #[test]
    fn main_agent_exit_drains_an_active_run_all() {
        // Backlog 45dcf577 (review LOW-2): a cancel/crash while a run is
        // active (e.g. blocked on an approval halt, whose kept run state
        // expects a later turn resolution) bypasses every designed drain
        // path — the forwarder's Exited arm must drain the run state so it
        // cannot strand ("run-all is already active" + an InFlight item
        // with no UI recovery).
        // Source-contract: both fns need a Tauri AppHandle.
        let forwarder = include_str!("events.rs");
        assert!(
            forwarder.contains("drain_run_all_on_main_exit"),
            "the forwarder's Exited arm must drain an active run-all when the main agent exits"
        );
        // Pin the ordering (round-2 review note): was_main must be captured
        // BEFORE mgr.remove — after removal the main agent id is gone and
        // the drain would never fire. (Parallel run-all review R1: was_main
        // is additionally gated on ownership — a spawned lane is never the
        // main lane, even when it holds the smallest parentless id after
        // the main agent's exit.)
        let was_main = forwarder
            .find("!owns_spawned && mgr.main_agent_id() == Some(agent_id)")
            .expect("the Exited arm must capture was_main under the manager lock");
        let remove = forwarder
            .find("mgr.remove(agent_id);")
            .expect("the Exited arm must remove the agent");
        assert!(
            was_main < remove,
            "was_main must be captured BEFORE mgr.remove — after removal the \
             main agent id is gone and the drain would never fire"
        );
        // The Exited arm must also clear the single-dispatch pointer: a
        // stale single_in_flight misattributes the next agent's first turn
        // resolution (the plan gate would resolve the dead item against
        // the new agent's state) and blocks adoption of single-dispatch
        // orphans.
        assert!(
            forwarder.contains(".single_in_flight"),
            "the Exited arm must clear single_in_flight on a main-agent exit"
        );
        let drain = fn_body(include_str!("run_all.rs"), "drain_run_all_on_main_exit");
        // Backlog (run-all orphans, 2027-01-07): the drain must REQUEUE the
        // item to Pending — leaving it InFlight stranded it forever (a dead
        // agent's turn resolution never comes; dispatch only considers
        // Pending and the guarded requeue refuses InFlight). The queue is
        // the recovery; the work stays in the tree.
        assert!(
            drain.contains("BacklogStatus::Pending"),
            "the drain must requeue the interrupted item to Pending — an \
             InFlight item with a dead owner has no other recovery"
        );
        assert!(
            drain.contains(".transition("),
            "the requeue must go through the guarded transition table"
        );
        // The checkpoint sha must survive the requeue: the note is
        // snapshotted BEFORE the transition (transition overwrites the
        // note).
        let snapshot = drain
            .find(".note.clone()")
            .expect("the drain must snapshot the item's note before mutating it");
        let transition = drain
            .find(".transition(")
            .expect("the drain must transition the item");
        assert!(
            snapshot < transition,
            "the note snapshot must happen BEFORE the transition — transition \
             overwrites the note and the checkpoint sha must survive"
        );
        assert!(
            drain.contains("end_run(app, &state)"),
            "the drain must end the run"
        );
    }

    /// Backlog (run-all orphans, 2027-01-07): an InFlight item whose owner is
    /// gone (an app crash killed the in-memory run state; a legacy drain
    /// left it in flight) is skipped by every future run-all — dispatch only
    /// considers Pending and the guarded requeue refuses InFlight, so the
    /// work strands with no recovery. Run-all START must adopt orphans:
    /// requeue every InFlight item with no live dispatch pointer to Pending
    /// so the normal dispatch order picks them up.
    /// Source-contract: adopt_orphaned_in_flight needs a Tauri AppHandle.
    #[test]
    fn run_all_start_adopts_orphaned_in_flight_items() {
        let adopt = fn_body(include_str!("run_all.rs"), "adopt_orphaned_in_flight");
        // Ownership: an InFlight item is owned by the single-dispatch pointer
        // (a live single dispatch) or by being the main agent's active root
        // plan (the user manually resumed an orphan's session and is
        // mid-plan on it — requeueing would break the InFlight ⇔ plan-active
        // contract while work happens). Everything else is an orphan.
        assert!(
            adopt.contains("single_in_flight"),
            "adoption must exclude the live single-dispatch item"
        );
        // Review LOW-1 (2027-01-07): the pointer must be read AFTER the
        // store lock is held — a dispatch sets the pointer strictly before
        // the Executing-entry InFlight stamp (which needs the store lock),
        // so under the lock any InFlight snapshot item's pointer is already
        // visible; reading it before the lock had a TOCTOU window that
        // adopted a just-dispatched item.
        let store_lock = adopt
            .find("store.lock().await")
            .expect("adoption must take the store lock");
        let single_read = adopt
            .find("single_in_flight")
            .expect("adoption must read the single-dispatch pointer");
        assert!(
            store_lock < single_read,
            "the single_in_flight read must happen AFTER the store lock is \
             held (TOCTOU: a just-dispatched item would look orphaned)"
        );
        assert!(
            adopt.contains("main_agent_top_plan_id"),
            "adoption must exclude the main agent's active root plan (a \
             manually resumed session mid-plan)"
        );
        assert!(
            adopt.contains("BacklogStatus::Pending"),
            "orphans must be requeued to Pending — the queue is the recovery"
        );
        assert!(
            adopt.contains(".transition("),
            "the requeue must go through the guarded transition table"
        );
        // The checkpoint sha must survive: the note is snapshotted before
        // the transition (transition overwrites the note).
        let snapshot = adopt
            .find(".note.clone()")
            .expect("adoption must snapshot the item's note before mutating it");
        let transition = adopt
            .find(".transition(")
            .expect("adoption must transition the item");
        assert!(
            snapshot < transition,
            "the note snapshot must happen BEFORE the transition"
        );
        // Wiring: run-all START calls the adoption sweep after the
        // already-active check and BEFORE the pending count — adopted
        // items count toward the run's total and dispatch in queue
        // order. The merged-branch sweeper (backlog 64662ef2) sits
        // between the adoption sweep and the pending count: a run-start
        // that finds no eligible items must still sweep.
        let cmds = include_str!("backlog_cmds.rs");
        let adopt_call = cmds
            .find("adopt_orphaned_in_flight")
            .expect("backlog_run_all must call adopt_orphaned_in_flight");
        let sweep_call = cmds
            .find("sweep_merged_runall_branches")
            .expect("backlog_run_all must call the merged-branch sweeper");
        let active_check = cmds
            .find("run-all is already active")
            .expect("backlog_run_all must keep the already-active check");
        let pending_count = cmds
            .find("no pending backlog items to run")
            .expect("backlog_run_all must keep the pending-count check");
        assert!(
            active_check < adopt_call
                && adopt_call < sweep_call
                && sweep_call < pending_count,
            "adoption must run AFTER the already-active check and BEFORE the \
             pending count — adopted items count toward the run's total; the \
             sweeper must run BEFORE the pending-count early-return so a \
             run-start that finds no eligible items still sweeps (backlog \
             64662ef2, review L2)"
        );
    }

    #[test]
    fn auto_compact_gate_is_spawned_and_run_all_only() {
        // Backlog: auto-compact between run-all items. Source-contract:
        // on_main_turn_resolved needs a Tauri AppHandle, so the wiring is
        // asserted on source. The gate must (a) read the setting from the
        // live config, (b) SPAWN the compact-then-dispatch task — awaiting
        // it inline would deadlock the event forwarder on its own output
        // (the Compacted event it waits for flows through that forwarder),
        // and (c) sit in the run-all branch only, before the single-dispatch
        // / auto-feed path (interactive completions never trigger it).
        // The gate moved into resolve_run_all_turn (the run-all branch's
        // per-outcome helper); the pin follows the code.
        let body = fn_body(include_str!("run_all.rs"), "resolve_run_all_turn");
        assert!(
            body.contains("auto_compact_on_plan_complete"),
            "the run-all dispatch must be gated on the setting"
        );
        assert!(
            body.contains("tokio::spawn(compact_then_dispatch_next(app.clone()))"),
            "the auto-compact path must be spawned, never awaited inline"
        );
        assert!(
            body.contains("let auto_compact = item_resolved"),
            "the compaction must be gated on an item actually resolving this turn — one compact per completed plan; an interactive turn during the window must not re-compact"
        );
        let clear = body
            .find("*r.current_item.lock().expect(\"current_item lock poisoned\") = None;")
            .expect("must clear current_item once the item resolves");
        let gate = body
            .find("auto_compact_on_plan_complete")
            .expect("the setting read must exist");
        assert!(
            clear < gate,
            "the in-flight pointer must be cleared before the auto-compact window opens"
        );
        let dispatch = body
            .find("run_all_dispatch_next(app, state)")
            .expect("the direct dispatch must exist for the setting-off path");
        assert!(
            gate < dispatch,
            "the gate must precede the dispatch it wraps"
        );
        // The gate lives in the run-all branch's helper; the single-dispatch
        // path (auto_feed) is the orchestrator's next call — the ordering
        // contract survives across the decomposition.
        let orchestrator = fn_body(include_str!("run_all.rs"), "on_main_turn_resolved");
        let run_all_call = orchestrator
            .find("resolve_run_all_turn(")
            .expect("the orchestrator must call the run-all resolution first");
        let auto_feed = orchestrator
            .find("resolve_single_dispatch_turn(")
            .expect("the single-dispatch path must exist below the run-all branch");
        assert!(
            run_all_call < auto_feed,
            "the gate must live in the run-all branch, before the single-dispatch path"
        );
    }

    #[test]
    fn compact_then_dispatch_next_orders_compact_wait_dispatch() {
        // Source-contract: compact_then_dispatch_next needs a Tauri
        // AppHandle. The order IS the contract: subscribe → send Compact →
        // wait for the signal → dispatch next. And the dispatch is
        // unconditional — compaction is an optimization, never a blocker.
        let body = fn_body(include_str!("run_all.rs"), "compact_then_dispatch_next");
        let subscribe = body
            .find("compact_signal.subscribe()")
            .expect("must subscribe to the compact signal");
        let send = body
            .find("AgentCommand::Compact")
            .expect("must send the compact command");
        let wait = body
            .find("wait_for_compact_signal")
            .expect("must wait for the compact signal");
        let dispatch = body
            .rfind("run_all_dispatch_next")
            .expect("must dispatch the next item");
        assert!(
            subscribe < send,
            "subscribe BEFORE sending so the completion increment can't be missed"
        );
        assert!(send < wait, "send the compact command before waiting");
        assert!(
            wait < dispatch,
            "dispatch only after the wait resolves (or times out)"
        );
        // HIGH-2 (review): the dispatch is gated on a run-state re-check — a
        // stop/halt that landed during the (minutes-long) compaction window
        // must not be overridden. The re-check sits between the wait and the
        // dispatch; a stopped run ends (end_run), a gone run just logs.
        let recheck = body[wait..]
            .find("r.stop.load(Ordering::Relaxed)")
            .expect("must re-check the run state after the wait");
        let dispatch_after = body[wait..]
            .rfind("run_all_dispatch_next")
            .expect("must dispatch the next item");
        assert!(
            recheck < dispatch_after,
            "the run-state re-check must precede the dispatch"
        );
        assert!(
            body.contains("end_run(&app, &state)"),
            "a stop requested during the window must end the run, not dispatch"
        );
    }

    #[test]
    fn should_between_items_compact_respects_the_fill_rate_dial() {
        // Backlog ffd4bac3: the gate is the SAME effective threshold the
        // fill-rate path uses — 50% of a 500k window = 250k. Below →
        // skip; at/above → compact (>= is inclusive).
        assert!(!should_between_items_compact(
            Some((100_000, 500_000)),
            0.5,
            None
        ));
        assert!(should_between_items_compact(
            Some((250_000, 500_000)),
            0.5,
            None
        ));
        assert!(should_between_items_compact(
            Some((300_000, 500_000)),
            0.5,
            None
        ));
    }

    #[test]
    fn should_between_items_compact_includes_the_proxy_ceiling_cap() {
        // 90% of 500k = 450k, but the 340k ceiling − 32k margin caps the
        // threshold at 308k — the gate must use the CAPPED value, exactly
        // like the fill-rate path.
        assert!(should_between_items_compact(
            Some((310_000, 500_000)),
            0.9,
            Some(340_000)
        ));
        assert!(!should_between_items_compact(
            Some((300_000, 500_000)),
            0.9,
            Some(340_000)
        ));
    }

    #[test]
    fn should_between_items_compact_is_conservative_when_blind() {
        // No usage data or a zero window → compact (the pre-ffd4bac3
        // behavior — the overnight-stability default when the fill is
        // unknown).
        assert!(should_between_items_compact(None, 0.5, None));
        assert!(should_between_items_compact(Some((5, 0)), 0.5, None));
    }

    #[test]
    fn between_items_gate_precedes_the_compact_send() {
        // Backlog ffd4bac3: the fill-rate gate must run BEFORE the compact
        // command is sent (a skip never sends it), and the skip path must
        // still reach the dispatch — compaction is an optimization, never
        // a blocker.
        let body = fn_body(include_str!("run_all.rs"), "compact_then_dispatch_next");
        let gate = body
            .find("between_items_compact_gate(&state)")
            .expect("must consult the fill-rate gate");
        let send = body
            .find("AgentCommand::Compact")
            .expect("must send the compact command");
        assert!(
            gate < send,
            "the fill-rate gate must precede the compact send — a skip never sends it"
        );
        assert!(
            body.contains("below the fill-rate threshold"),
            "the skip path must log why it skipped"
        );
        // The gate reads the LIVE loop's fill rate (what the engine's
        // fill-rate path uses) and the ceiling from the factory's
        // startup snapshot.
        let gate_body = fn_body(include_str!("run_all.rs"), "between_items_compact_gate");
        assert!(
            gate_body.contains("l.fill_rate()"),
            "the gate must read the live loop's fill rate"
        );
        assert!(
            gate_body.contains("f.proxy_cache_ceiling()"),
            "the gate must read the proxy cache ceiling from the factory snapshot (the same source the engine's context managers are built from)"
        );
        assert!(
            gate_body.contains("context_usage"),
            "the gate must read the tracked per-agent usage"
        );
    }

    #[test]
    fn forwarder_tracks_context_usage_per_agent() {
        // Backlog ffd4bac3: the app's only context-fill source — the
        // forwarder must store every ContextUsage event into the per-agent
        // map, and the Exited arm must drop the entry so the map doesn't
        // leak across a long session.
        let forwarder = include_str!("events.rs");
        let arm = forwarder
            .find("SerializableAgentEvent::ContextUsage { used, max, .. } => {")
            .expect("the forwarder must track ContextUsage events");
        assert!(
            forwarder[arm..].contains(".insert(agent_id,"),
            "the arm must store the usage keyed by agent id"
        );
        let cleanup = forwarder
            .find("compact_in_flight.remove(&agent_id);")
            .expect("the Exited cleanup block must exist");
        assert!(
            forwarder[cleanup..cleanup + 400].contains("context_usage"),
            "Exited must remove the agent's tracked usage so the map doesn't leak"
        );
    }

    #[test]
    fn forwarder_signals_compact_completion_for_main_agent() {
        // Backlog: auto-compact between run-all items. Source-contract: the
        // forwarder needs a Tauri AppHandle, so the wiring is asserted on
        // source. The compact signal must be incremented on BOTH the
        // Compacted event and the Error paired with a CompactStarted (a
        // FAILED compaction must not burn the wait timeout), and only for
        // the main agent (the wait is the main agent's).
        let forwarder = include_str!("events.rs");
        assert!(
            forwarder.contains("SerializableAgentEvent::CompactStarted => {"),
            "the forwarder must track compaction starts"
        );
        assert!(
            forwarder.contains("SerializableAgentEvent::Compacted { .. } => {"),
            "the forwarder must resolve the wait on Compacted"
        );
        assert!(
            forwarder.contains("compact_signal"),
            "the forwarder must increment the shared compact signal"
        );
        // The pairing: CompactStarted inserts into compact_in_flight, and
        // both completion arms remove from it — the Error arm must be
        // guarded by the pairing so ordinary turn errors never increment.
        let insert = forwarder
            .find("compact_in_flight.insert(agent_id)")
            .expect("CompactStarted must insert into the pairing set");
        let compacted = forwarder
            .find("SerializableAgentEvent::Compacted { .. } => {")
            .expect("the Compacted arm must exist");
        assert!(
            insert < compacted,
            "the pairing set must be declared before its use"
        );
        assert!(
            forwarder.contains("if compact_in_flight.remove(&agent_id)"),
            "both completion arms must be guarded by the pairing removal"
        );
    }

    #[tokio::test]
    async fn wait_for_compact_signal_resolves_on_increment() {
        let (tx, mut rx) = tokio::sync::watch::channel(0u64);
        let before = *rx.borrow();
        tx.send_if_modified(|c| {
            *c += 1;
            true
        });
        assert!(
            wait_for_compact_signal(&mut rx, before, std::time::Duration::from_secs(1)).await,
            "an increment past `before` must resolve the wait"
        );
    }

    #[tokio::test]
    async fn wait_for_compact_signal_times_out() {
        let (_tx, mut rx) = tokio::sync::watch::channel(0u64);
        let before = *rx.borrow();
        let start = std::time::Instant::now();
        assert!(
            !wait_for_compact_signal(&mut rx, before, std::time::Duration::from_millis(50))
                .await,
            "no increment must time out (proceed anyway — never stall)"
        );
        assert!(
            start.elapsed() >= std::time::Duration::from_millis(50),
            "the timeout must actually elapse, not return early"
        );
    }

    #[tokio::test]
    async fn wait_for_compact_signal_proceeds_on_channel_close() {
        let (tx, mut rx) = tokio::sync::watch::channel(0u64);
        let before = *rx.borrow();
        drop(tx);
        assert!(
            !wait_for_compact_signal(&mut rx, before, std::time::Duration::from_secs(5)).await,
            "a closed signal channel (app shutdown) must proceed, not hang"
        );
    }

    #[test]
    fn plan_title_from_file_parses_the_heading() {
        // Backlog f45513b2: the plan file's first line is the
        // `# Plan: <title>` heading create_plan writes — the Backlog tab's
        // human-friendly identifier.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("abc12345.md"),
            "# Plan: Fix the widget\n\n## Goal\nDo it.",
        )
        .unwrap();
        assert_eq!(
            plan_title_from_file(dir.path(), "abc12345"),
            Some("Fix the widget".to_string())
        );
    }

    #[test]
    fn plan_title_from_file_handles_missing_or_malformed() {
        let dir = tempfile::tempdir().unwrap();
        // Missing file.
        assert_eq!(plan_title_from_file(dir.path(), "ghost"), None);
        // First line without the heading (the heading must be line 1).
        std::fs::write(dir.path().join("nohead.md"), "Just text\n# Plan: late\n").unwrap();
        assert_eq!(plan_title_from_file(dir.path(), "nohead"), None);
        // Empty title after the prefix.
        std::fs::write(dir.path().join("empty.md"), "# Plan: \n").unwrap();
        assert_eq!(plan_title_from_file(dir.path(), "empty"), None);
    }

    #[test]
    fn stamp_backlog_in_flight_records_the_plan_title() {
        // Backlog f45513b2: the stamp needs a Tauri AppHandle, so the wiring
        // is asserted on source. The plan TITLE must be read from the plan
        // file (via the project's plans dir) and passed to set_plan_id
        // alongside the plan id — the Backlog tab's human-friendly
        // identifier instead of the raw checkpoint sha.
        let body = fn_body(include_str!("run_all.rs"), "stamp_backlog_in_flight");
        let read = body
            .find("plan_title_from_file")
            .expect("the stamp must read the plan title");
        let set = body
            .find("set_plan_id")
            .expect("the stamp must record the linkage");
        assert!(
            read < set,
            "the title must be read before recording the linkage"
        );
        assert!(
            body.contains("plans_dir"),
            "the stamp must resolve the plans dir from the project state"
        );
    }

    #[test]
    fn closed_loop_with_captured_item_commits_and_marks_done() {
        // Regression (review finding HIGH-1, 2026-12-05): when the agent
        // absorbed a steer and STILL closed the plan loop, the resolution
        // fell through to the normal Done path — invisible to a run-all item
        // whose halt already cleared the run state (run_all = None, never in
        // single_in_flight) — stranding the item at InFlight with no UI
        // recovery (requeue refuses InFlight; dispatch requires Pending).
        // The closed-loop branch must resolve a CAPTURED item id itself.
        let body = fn_body(include_str!("run_all.rs"), "on_main_turn_resolved");
        let closed = body
            .find("if !closed_loop {")
            .expect("the closed_loop disposition branch must exist");
        let run_all_branch = body[closed..]
            .find("Run-All takes priority")
            .expect("the run-all branch anchor must follow");
        let intervention_block = &body[closed..closed + run_all_branch];
        assert!(
            intervention_block.contains("iv.item_id"),
            "the closed-loop branch must check the captured item id"
        );
        assert!(
            intervention_block.contains("finish_captured_item_done("),
            "a captured run-all item must be resolved (commit + Done) by the \
             closed-loop branch itself — the fall-through cannot see it"
        );
        assert!(
            intervention_block.contains("interrupted =="),
            "the closed-loop arm must guard its end_run by run identity too \
             (review round-2 LOW A)"
        );
        // The helper mirrors the run-all success arm: commit + Done.
        let helper = fn_body(include_str!("run_all.rs"), "finish_captured_item_done");
        assert!(
            helper.contains("commit_success(") && helper.contains("BacklogStatus::Done"),
            "the captured-item success path commits and marks Done"
        );
        assert!(
            !helper.contains("run_all_dispatch_next") && !helper.contains("auto_feed"),
            "the run is over — no next dispatch, no auto-feed"
        );
    }
}

/// Extract a git sha from a backlog item note that may contain a reason suffix.
/// Run-All stores the checkpoint sha in the item's `note` while InFlight.
/// On halt we may append " | reason" for UI visibility; this recovers the sha
/// prefix (hex, >=7 chars) so the checkpoint sha stays recoverable from an
/// annotated note (the manual resume/rollback anchor — surfaced machine-
/// readable in the backlog UI payload via `BacklogItemView::checkpoint_sha`).
/// Returns None if no plausible sha prefix is present (e.g. manual dispatch).
pub(crate) fn extract_checkpoint_sha(note: &str) -> Option<String> {
    let token = note
        .trim()
        .split(|c: char| c.is_whitespace() || c == '|')
        .next()
        .unwrap_or(note);
    if token.len() >= 7 && token.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(token.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod extract_tests {
    use super::extract_checkpoint_sha;

    #[test]
    fn extracts_plain_sha() {
        assert_eq!(
            extract_checkpoint_sha("a1b2c3d4e5f6789012345678901234567890abcd"),
            Some("a1b2c3d4e5f6789012345678901234567890abcd".to_string())
        );
        assert_eq!(
            extract_checkpoint_sha("abcdef1"),
            Some("abcdef1".to_string())
        );
    }

    #[test]
    fn extracts_sha_with_halt_reason_suffix() {
        let note = "a1b2c3d4e5f6789012345678901234567890abcd | approval requested during unattended run — halted";
        assert_eq!(
            extract_checkpoint_sha(note),
            Some("a1b2c3d4e5f6789012345678901234567890abcd".to_string())
        );
    }

    #[test]
    fn returns_none_for_non_sha_notes() {
        assert_eq!(extract_checkpoint_sha("approval requested"), None);
        assert_eq!(extract_checkpoint_sha(""), None);
        assert_eq!(extract_checkpoint_sha("short"), None);
        assert_eq!(extract_checkpoint_sha("nothexbutlongenough123"), None);
    }

    #[test]
    fn tolerates_whitespace_and_trailing() {
        let note = "  deadbeef1234567   | some reason";
        assert_eq!(
            extract_checkpoint_sha(note),
            Some("deadbeef1234567".to_string())
        );
    }
}

/// How long the between-items auto-compact waits for the `Compacted` event
/// before dispatching the next item anyway. Generous on purpose: the
/// compaction is a summarization LLM call over a near-full context, which
/// is slow on any provider. Compaction is an optimization, never a blocker
/// — on timeout the loop logs and proceeds.
const AUTO_COMPACT_WAIT: std::time::Duration = std::time::Duration::from_secs(600);

/// Wait until the compact signal counter moves past `before` (the value
/// recorded before `AgentCommand::Compact` was sent). Returns `true` when
/// the compaction finished — the forwarder increments on both the
/// `Compacted` event and the `Error` paired with its `CompactStarted`, so
/// a FAILED compaction also resolves — and `false` on timeout or when the
/// signal channel closed (app shutdown; proceed either way).
async fn wait_for_compact_signal(
    rx: &mut tokio::sync::watch::Receiver<u64>,
    before: u64,
    timeout: std::time::Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if *rx.borrow() > before {
            return true;
        }
        match tokio::time::timeout_at(deadline, rx.changed()).await {
            // Counter moved — re-check the value.
            Ok(Ok(())) => continue,
            // Signal channel closed (app shutdown) — proceed.
            Ok(Err(_)) => return false,
            // Timed out — proceed.
            Err(_) => return false,
        }
    }
}

/// Backlog ffd4bac3: decide whether the between-items auto-compact should
/// fire for the given post-item usage. The threshold is the SAME effective
/// fill-rate trigger the engine's summarization path uses —
/// `summarize_at_fill_rate` × window, proxy-ceiling cap included — computed
/// through the shared `ContextManager::effective_summarize_threshold`
/// helper so the two can never drift. No usage data or a zero window means
/// we are blind, so compact (the pre-ffd4bac3 behavior).
fn should_between_items_compact(
    usage: Option<(u64, u64)>,
    fill_rate: f64,
    ceiling: Option<usize>,
) -> bool {
    match usage {
        Some((used, max)) if max > 0 => {
            let threshold = mnemo::agent::context::ContextManager::effective_summarize_threshold(
                max as usize,
                fill_rate,
                ceiling,
            );
            used as usize >= threshold
        }
        // Blind (no usage event seen, or a zero window) — compact anyway.
        _ => true,
    }
}

/// Backlog ffd4bac3: read the main agent's post-item context fill and
/// decide whether the between-items auto-compact should fire. Both the
/// fill rate (from the LIVE loop) and the proxy cache ceiling (from the
/// factory's startup snapshot) are the SAME values the engine's
/// fill-rate path uses — the factory threads them into every loop and
/// every rebuilt context manager, so the gate's threshold matches the
/// dial the engine actually applies even after a mid-session settings
/// edit (review LOW-1: reading the ceiling from the current config
/// instead would diverge from the engine's snapshot until restart).
/// Exception (round-2 LOW-1): turns served by resolver-built CMs (a
/// `[models.*]` slot serving the turn, a 429-sticky reroute — on a
/// picker pin or the default provider — or a forced model) read the
/// LIVE config's ceiling — on those paths a mid-session ceiling edit
/// diverges until restart; the impact is bounded, restart-healed, and
/// the preflight backstop (window − headroom) still guards the window.
/// Missing pieces (no main agent, no loop, no factory) mean we are
/// blind, so compact.
async fn between_items_compact_gate(state: &tauri::State<'_, IpcState>) -> bool {
    let main_id = {
        let mgr = state.runtime.manager.lock().await;
        match mgr.main_agent_id() {
            Some(id) => id,
            None => return true,
        }
    };
    let usage = state
        .runtime
        .context_usage
        .lock()
        .await
        .get(&main_id)
        .copied();
    let fill_rate = {
        let loops = state.runtime.agent_loops.lock().await;
        match loops.get(&main_id) {
            Some(l) => l.fill_rate(),
            None => return true,
        }
    };
    let ceiling = match &state.runtime.factory {
        Some(f) => f.proxy_cache_ceiling(),
        // No factory (brain failed to build) — blind: compact.
        None => return true,
    };
    should_between_items_compact(usage, fill_rate, ceiling)
}

/// The run-all heartbeat period (backlog 0ed9d24f): while a run is armed,
/// [`run_all_heartbeat`] re-checks the stall preconditions this often —
/// every silent dispatch-chain arm (the busy-guard deferral, the
/// between-items compact window, a swallowed dispatch error) is bounded
/// to one period — unless the run reads busy: a zombie descendant
/// keeping `has_running_descendants` true forever stalls the run
/// identically to today's behavior, and the persistent deferral line
/// makes that arm diagnosable.
const RUN_ALL_HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(30);

/// A persistent, timestamped diagnostic line for the run-all dispatch
/// chain — the primary channel for stall forensics (backlog 0ed9d24f):
/// every dispatch-chain arm (the busy-guard deferral, the between-items
/// compact outcomes, dispatch errors, the heartbeat and its re-drives)
/// writes here, so a stalled run is diagnosable from the log in one
/// read. A release build is a GUI-subsystem binary and `eprintln!` can
/// vanish entirely (see [`crate::ipc::events::diag_line`] — live
/// 2026-09-08); the file sink is the primary channel, stderr kept as a
/// bonus for the console/dev paths. Best-effort: a diagnostic that
/// cannot be written must never affect the app.
///
/// The sink is resolved once: the project-local `.coding/logs/run-all.log`
/// when a `.coding` side-car exists under the current working directory
/// (dev launches), else `std::env::temp_dir().join("mnemo-run-all.log")`
/// — the packaged-build anchoring pattern from [`diag_line`].
fn run_all_diag(line: &str) {
    {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "[run-all] {line}");
    }
    let path = RUN_ALL_DIAG_SINK.get_or_init(|| {
        if std::path::Path::new(".coding").exists() {
            std::path::PathBuf::from(".coding/logs/run-all.log")
        } else {
            std::env::temp_dir().join("mnemo-run-all.log")
        }
    });
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{stamp}] {line}");
    }
}

/// The resolved file sink for [`run_all_diag`] (see there for the
/// rationale).
static RUN_ALL_DIAG_SINK: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// The stall preconditions for the run-all re-drive paths (backlog
/// 0ed9d24f): a run is STALLED when it is armed, has NO current item,
/// the between-items auto-compact is not running, the main agent exists
/// and is idle (not mid-turn, no running descendants), and an eligible
/// pending item exists. This is the state the 2026-09-08 incident was
/// found in — the dispatch chain had silently stopped and nothing
/// re-drove it (the gate re-checks only on turn resolutions; an idle
/// main agent produces none). Read-only: the caller decides what to do.
async fn run_all_stalled(state: &IpcState) -> bool {
    let (current, compact_running, exclude) = {
        let guard = state.backlog.run_all.lock().await;
        let Some(r) = guard.as_ref() else {
            // No run armed — nothing to re-drive.
            return false;
        };
        // A stop-requested run is not stalled (review 2026-09-09 LOW-3):
        // every other dispatcher checks stop before dispatching — the
        // heartbeat must not dispatch one more item past a user stop.
        if r.stop.load(Ordering::Relaxed) {
            return false;
        }
        // Lock order: current_item BEFORE spawned (the documented
        // invariant — review R8).
        let current = r
            .current_item
            .lock()
            .expect("current_item lock poisoned")
            .clone();
        let mut ids: Vec<String> = r
            .spawned
            .lock()
            .expect("spawned lock poisoned")
            .iter()
            .map(|s| s.item_id.clone())
            .collect();
        if let Some(id) = &current {
            ids.push(id.clone());
        }
        (
            current,
            state.backlog.compacting.load(Ordering::Relaxed),
            ids,
        )
    };
    if current.is_some() || compact_running {
        return false;
    }
    // The main agent must exist and be idle — dispatching into a busy
    // agent is the interleaving the busy guard exists to prevent.
    {
        let mgr = state.runtime.manager.lock().await;
        let Some(main_id) = mgr.main_agent_id() else {
            return false;
        };
        let busy = mgr.get(main_id).map(|h| h.is_running()).unwrap_or(false)
            || mgr.has_running_descendants(main_id);
        if busy {
            return false;
        }
    }
    // An eligible item must exist (a queue with only deferred items is
    // not stalled — the run ends through the normal no-eligible path).
    state
        .backlog
        .store
        .lock()
        .await
        .next_pending_eligible_excluding(&exclude)
        .is_some()
}

/// The run-all heartbeat — the class-level stall recovery (backlog
/// 0ed9d24f). While a run is armed, periodically re-drive the dispatch
/// when the run is stalled ([`run_all_stalled`]) — WITHOUT waiting for a
/// turn resolution (an idle main agent produces none; every silent
/// stall arm is bounded to one [`RUN_ALL_HEARTBEAT`] period — unless the
/// run reads busy: a zombie descendant keeping
/// `has_running_descendants` true forever stalls the run identically to
/// today's behavior, and the persistent deferral line makes that arm
/// diagnosable). Never ends runs: `end_run` stays with the resolution
/// paths (a heartbeat ending runs would race the normal lifecycle).
/// Spawned for the run's lifetime by `backlog_run_all`; bound to the
/// run's generation and self-terminates when the run ends or a newer
/// run is armed.
pub(crate) async fn run_all_heartbeat(app: tauri::AppHandle, generation: u64) {
    loop {
        tokio::time::sleep(RUN_ALL_HEARTBEAT).await;
        let state = app.state::<IpcState>();
        // The run ended, or a newer run was armed within this heartbeat's
        // sleep phase — self-terminate (no zombie task, and no heartbeat
        // from a previous run surviving into its successor: review
        // 2026-09-09 LOW-4).
        if state.backlog.run_all.lock().await.is_none()
            || state
                .backlog
                .run_generation
                .load(Ordering::Relaxed)
                != generation
        {
            break;
        }
        if !run_all_stalled(&state).await {
            continue;
        }
        run_all_diag("heartbeat: stalled run re-driven — dispatching next item");
        if let Err(e) = run_all_dispatch_next(&app, &state).await {
            run_all_diag(&format!("heartbeat: dispatch failed: {e}"));
        }
    }
}

/// Between-items auto-compact (Run-All only, `auto_compact_on_plan_complete`):
/// compact the main agent's context — gated on the fill-rate dial (backlog
/// ffd4bac3: only when the post-item context is at/above the effective
/// `summarize_at_fill_rate` threshold; below it the compaction is skipped
/// and the next item dispatches directly) — wait for the compaction to
/// finish, then dispatch the next item. Spawned — NEVER awaited inline from
/// [`on_main_turn_resolved`]: that path runs inside the event forwarder, and
/// the `Compacted` event the wait needs flows through that same forwarder;
/// awaiting inline would deadlock the loop on its own output.
///
/// Failure handling: compaction is an optimization, never a blocker. A
/// failed compaction, a missing main agent, a refused command send, or a
/// timeout all log and proceed to the dispatch.
async fn compact_then_dispatch_next(app: tauri::AppHandle) {
    let state = app.state::<IpcState>();
    // Backlog ffd4bac3: the between-items auto-compact respects the
    // fill-rate dial — compact only when the post-item context is at/above
    // the SAME effective threshold the engine's fill-rate summarization
    // path uses. Below it, skip the summarization LLM call and dispatch
    // directly; the mid-item threshold backstop still covers items that
    // grow past the dial mid-flight.
    let should_compact = between_items_compact_gate(&state).await;
    // Subscribe BEFORE sending so the completion increment can't be missed.
    let mut rx = state.backlog.compact_signal.subscribe();
    let before = *rx.borrow();
    if should_compact {
        state.backlog.compacting.store(true, Ordering::Relaxed);
        emit_backlog_changed(&app, &state).await;

        let mut compact_sent = false;
        {
            let mgr = state.runtime.manager.lock().await;
            if let Some(main_id) = mgr.main_agent_id() {
                match mgr.send(main_id, AgentCommand::Compact) {
                    Ok(()) => compact_sent = true,
                    Err(_) => {
                        run_all_diag(
                            "auto-compact: main agent refused the compact command — dispatching anyway",
                        );
                    }
                }
            } else {
                run_all_diag("auto-compact: no main agent — dispatching anyway");
            }
        }
        if compact_sent && !wait_for_compact_signal(&mut rx, before, AUTO_COMPACT_WAIT).await {
            run_all_diag(&format!(
                "auto-compact did not complete within {}s — dispatching anyway",
                AUTO_COMPACT_WAIT.as_secs()
            ));
        }

        state.backlog.compacting.store(false, Ordering::Relaxed);
        emit_backlog_changed(&app, &state).await;
    } else {
        run_all_diag(
            "auto-compact: post-item context below the fill-rate threshold — skipping the compaction",
        );
    }
    // HIGH-2 (review): the wait stretched the between-items window from
    // microseconds to the compaction duration — a user stop or halt that
    // landed during it must not be overridden by an unconditional dispatch.
    // Re-check the run state: no run (halted/ended) or stop requested ⇒ do
    // not dispatch (a stopped run ends here, matching the stopped path in
    // on_main_turn_resolved).
    let run_state = {
        let guard = state.backlog.run_all.lock().await;
        guard.as_ref().map(|r| r.stop.load(Ordering::Relaxed))
    };
    match run_state {
        None => {
            run_all_diag("run-all ended during the between-items auto-compact — not dispatching");
        }
        Some(true) => {
            run_all_diag("run-all stop requested during the between-items auto-compact — ending the run");
            // Parallel run-all (plan ffd7a86f, review H1): stop ends the
            // run only when no lane is still in flight; the lanes'
            // resolutions end it when the last one lands.
            if !any_lane_in_flight(&state).await {
                end_run(&app, &state).await;
            }
        }
        Some(false) => {
            if let Err(e) = run_all_dispatch_next(&app, &state).await {
                run_all_diag(&format!("failed to dispatch next item: {e}"));
            }
        }
    }
}

/// Dispatch the next Run-All item: git-checkpoint the working tree, embed the
/// unattended-mode preamble in the prompt, then dispatch it to the main
/// agent.
///
/// Called by `backlog_run_all` (first item) and by the event forwarder after
/// each resolved turn (next items). Ends the run when nothing eligible is
/// pending — deferred items ([`crate::backlog::BacklogItem::deferred`],
/// user request 2027-01-07) are never selected by a run: they stay pending
/// and visible (reserved for manual in-app handling or later re-inclusion),
/// so a backlog with only deferred items left ends the run cleanly here.
/// DEFERS (returns `Ok` without dispatching) when the main agent is still
/// running a turn or has running descendants — dispatching then would
/// interleave two items' prompts, the exact "next item starts before the
/// active one resolves" defect class; the in-flight turn's resolution
/// re-invokes this and dispatches then.
pub(crate) async fn run_all_dispatch_next(
    app: &tauri::AppHandle,
    state: &IpcState,
) -> Result<(), String> {
    // One dispatch decision at a time (review R4) — see DISPATCH_LOCK.
    let _dispatch_guard = DISPATCH_LOCK.lock().await;
    // The main-lane selection (plan ffd7a86f, review H2): exclude the ids
    // already handed out — the main agent's current item AND every spawned
    // lane's item. A spawned item stays `Pending` until its agent's
    // workflow enters `Executing`; without the exclusion the main lane
    // would double-dispatch an item a spawned agent is working (two
    // agents on one item — the exact cross-corruption class this feature
    // exists to prevent). The store's selection only READS either way.
    let exclude = {
        let guard = state.backlog.run_all.lock().await;
        guard.as_ref().map_or(Vec::new(), |r| {
            // Lock order: current_item BEFORE spawned (the documented
            // invariant — review R8).
            let current = r
                .current_item
                .lock()
                .expect("current_item lock poisoned")
                .clone();
            let mut ids: Vec<String> = r
                .spawned
                .lock()
                .expect("spawned lock poisoned")
                .iter()
                .map(|s| s.item_id.clone())
                .collect();
            if let Some(id) = current {
                ids.push(id);
            }
            ids
        })
    };
    let item = state
        .backlog
        .store
        .lock()
        .await
        .next_pending_eligible_excluding(&exclude);
    let Some(item) = item else {
        // Nothing eligible left. The run is complete ONLY when no lane is
        // still in flight (plan ffd7a86f, review H1): InFlight items are
        // invisible to the Pending-only selector, so parallel lanes (or
        // the main lane) still working must keep the run alive — their
        // resolutions re-drive this function and end the run when the
        // last one lands. Ending here would orphan them: stuck-InFlight
        // items, unlanded branches, leaked agents.
        if !any_lane_in_flight(state).await {
            end_run(app, state).await;
        }
        return Ok(());
    };

    // Busy guard (defect class 2026-08-20: "run-all starts new items before
    // the active item is fully resolved"): never dispatch while the main
    // agent is mid-turn or any descendant (subagent, consolidation) still
    // runs — the new prompt would queue behind / interleave with the active
    // item's work and its resolution would stamp the wrong item's status.
    // Deferring is safe: `next_pending_eligible()` only READS (the item
    // stays Pending), and the in-flight turn's terminal event re-invokes
    // `on_main_turn_resolved` → this function, dispatching then. A deferred
    // run can stall visibly (user can re-trigger via ▶) — stalling is
    // recoverable; interleaving corrupts statuses and resolves the wrong
    // item.
    {
        let mgr = state.runtime.manager.lock().await;
        if let Some(main_id) = mgr.main_agent_id() {
            let busy = mgr.get(main_id).map(|h| h.is_running()).unwrap_or(false)
                || mgr.has_running_descendants(main_id);
            if busy {
                run_all_diag(&format!(
                    "dispatch deferred — main agent still busy (item {} stays pending); the heartbeat re-drives once the run goes idle",
                    item.id
                ));
                // The recovery for a deferral whose assumed terminal event
                // never comes (a spawned lane's resolution goes through
                // on_spawned_turn_resolved; a stale descendant fires
                // nothing) is the run-all heartbeat (backlog 0ed9d24f):
                // it re-drives the run within one RUN_ALL_HEARTBEAT
                // period once the run goes idle (a permanently-busy run —
                // a zombie descendant — is never stalled by that
                // definition and stalls identically to today; the
                // persistent deferral line makes that arm diagnosable).
                // A separate delayed retry was
                // considered and dropped — spawning it from inside this
                // function is async recursion (an opaque-type cycle,
                // E0391), and the heartbeat already subsumes it at a
                // better cadence.
                // Parallel run-all (plan ffd7a86f): the main agent's lane
                // waits, but the spawned lanes still fill — each is a
                // fresh idle agent owning one item, so the interleaving
                // hazard this guard protects against does not apply.
                fill_spawned_window(app, state).await;
                return Ok(());
            }
        }
    }

    // Git checkpoint (fork/reuse the wt/* work branch, commit a dirty tree,
    // record HEAD for rollback) — a failure stops the run (the item stays
    // Pending, annotated with the reason; never stamped Failed).
    let checkpoint = checkpoint_run_all_item(app, state, &item).await?;

    // Record the in-flight item + its checkpoint on the run state (the item
    // stays `Pending` until the workflow enters `Executing`; the fresh sha
    // REPLACES any stale note).
    record_in_flight_item(state, &item, &checkpoint).await;

    // Best-effort enrichment (2027-01-07): attach recalled project knowledge
    // so a dispatched (possibly lesser-reasoning) model starts informed
    // instead of re-deriving. Never blocks the dispatch — no store, a
    // recall error, or no hits dispatches without the block.
    //
    // Computed BEFORE the manager lock is taken (review HIGH-1, round 1):
    // recall_peek awaits the embedder (a network round-trip under a remote
    // embedder, local ONNX inference under the bundled one) plus a sqlite
    // read — running that under the manager lock would stall every
    // lock taker (IPC commands, status queries, the event forwarder) for
    // the embed duration on EVERY item dispatch. Same lock discipline as
    // the git checkpoint above: multi-second ops never hold the manager
    // lock. Cost: a wasted recall in the rare defer-after-recheck path.
    let context_block = recalled_context_block(state, &item.text).await;

    // Lock the manager, re-check busyness, then dispatch the single combined
    // prompt.
    dispatch_run_all_to_main(app, state, item, checkpoint, context_block).await
}

/// Lock the manager, re-check busyness, then dispatch the single combined
/// prompt — the dispatch stage of [`run_all_dispatch_next`]. The whole
/// manager-lock hold/drop scope moves as one unit: the busy re-check under
/// the lock (review finding 3, 2026-08-20), the deterministic Planning flip
/// (plan 9057fa1f), the image resolution, the ONE-command send, and the
/// spawned-lane fill all stay inside the same hold exactly as before.
async fn dispatch_run_all_to_main(
    app: &tauri::AppHandle,
    state: &IpcState,
    item: mnemo::backlog::BacklogItem,
    checkpoint: String,
    context_block: Option<String>,
) -> Result<(), String> {
    let manager = state.runtime.manager.lock().await;
    let Some(main_id) = manager.main_agent_id() else {
        // Parallel run-all (plan ffd7a86f, review R3): end the run only
        // when no lane is in flight (the lanes' resolutions need the run
        // state; their exits drain + end it).
        if !any_lane_in_flight(state).await {
            end_run(app, state).await;
        }
        return Err("no main agent registered".to_string());
    };
    // Re-check under this lock hold (review finding 3, 2026-08-20): the busy
    // guard above ran BEFORE the git checkpoint, which releases the manager
    // lock for potentially multi-second git ops — a turn may have started in
    // that window (a user prompt, or a child's completion wake-up landing as
    // a steer). Sending now would interleave two items' work, the exact
    // defect class this fix closes; same predicate + one-lock-hold pattern
    // as the single-dispatch path (`dispatch_item`).
    //
    // RESIDUAL WINDOW (accepted): the forwarder flips the running flag only
    // when it PROCESSES the Started event, so a just-started turn can still
    // briefly read idle here. That window is narrow and probabilistic (vs.
    // the deterministic double-turn removed above); closing it fully would
    // require moving the running flag into the send path itself.
    let busy_after_checkpoint = manager
        .get(main_id)
        .map(|h| h.is_running())
        .unwrap_or(false)
        || manager.has_running_descendants(main_id);
    if busy_after_checkpoint {
        // Defer: the item never left `Pending` under the Executing-entry
        // stamping scheme — just keep the checkpoint sha in its note (the
        // interfering turn's resolution cannot stamp this item once the
        // in-flight pointer is cleared) and let it re-wait for dispatch.
        drop(manager);
        state
            .backlog
            .store
            .lock()
            .await
            .set_note(&item.id, Some(checkpoint));
        {
            let guard = state.backlog.run_all.lock().await;
            if let Some(r) = guard.as_ref() {
                *r.current_item.lock().expect("current_item lock poisoned") = None;
            }
        }
        run_all_diag(&format!(
            "dispatch deferred — main agent became busy during checkpoint (item {} returned to pending); the heartbeat re-drives once the run goes idle",
            item.id
        ));
        // Parallel run-all (plan ffd7a86f): the main agent's lane waits,
        // but the spawned lanes still fill.
        fill_spawned_window(app, state).await;
        return Ok(());
    }
    // Deterministic task-entry (plan 9057fa1f): flip a resting Complete
    // workflow to Planning BEFORE the prompt lands. Runs under the manager
    // lock hold, after the busy re-check — the agent is idle, so the
    // workflow mutex is uncontended. Best-effort: dispatch proceeds even if
    // the transition can't run (missing loop / unexpected state).
    let planning_event = {
        let agent_loops = state.runtime.agent_loops.lock().await;
        match agent_loops.get(&main_id) {
            Some(loop_handle) => {
                let workflow_handle = loop_handle.workflow_handle();
                let mut workflow = workflow_handle.lock().await;
                enter_planning_if_complete(&mut workflow)
            }
            None => None,
        }
    };
    if let Some((new_state, top_plan_id)) = planning_event {
        // Straight to the UI on the shared agent-event channel — the runtime
        // channel never sees this transition, so it can never count as
        // plan-loop evidence for the turn about to start (and
        // `TurnResolveLatch::on_started` would wipe it anyway).
        emit_agent_event(
            app,
            main_id,
            SerializableAgentEvent::WorkflowStateChanged {
                state: new_state,
                top_plan_id,
            },
        );
    }
    // Resolve image paths → data URLs for the agent dispatch + the
    // prompt-dispatched event (the store keeps paths in the JSONL; the agent
    // loop + frontend need data URLs). Mirrors the single-dispatch path
    // (backlog_cmds.rs::dispatch_item) so the two paths can't drift.
    let images = state.backlog.store.lock().await.resolve_images(&item);
    // ONE command, ONE turn: the unattended-mode preamble is embedded in the
    // prompt text (run_all_prompt). It must NOT be sent as a separate
    // Suggestion — a steer landing on an idle agent runs as its own full
    // turn, which resolved every item on a steer-only turn and dispatched
    // the next item before the real prompt ran (defect 2026-08-20).
    manager
        .send(
            main_id,
            AgentCommand::Prompt {
                text: run_all_prompt(&item.text, context_block.as_deref()),
                images: images.clone(),
            },
        )
        .map_err(|e| format!("failed to dispatch run-all item: {e:?}"))?;
    drop(manager);
    // Show the dispatched prompt as the goal in the transcript (same as the
    // single-dispatch path). NOTE: emits the raw item text — the goal bubble
    // stays clean; the model additionally sees the RUN_ALL_STEER preamble
    // and the recalled-context block (documented, deliberate divergence
    // from the literal prompt text).
    emit_prompt_dispatched(app, main_id, &item.text, &images);
    emit_backlog_changed(app, state).await;
    // Parallel run-all (plan ffd7a86f): fill the spawned lanes around the
    // main agent's item.
    fill_spawned_window(app, state).await;
    Ok(())
}

/// Git-checkpoint the item before dispatch: fork/reuse the wt/* work branch
/// first (never commit to main — backlog 04bd6977), commit a dirty tree, then
/// record HEAD for rollback. Clone the project root BEFORE the checkpoint so
/// the root lock is not held across the (now spawn_blocking) git ops —
/// mirroring the resolution path's `let root = ...`. Holding the lock across
/// a multi-second commit would block every config/project access.
///
/// On failure the item is NOT stamped (backlog 45dcf577: a checkpoint failure
/// is infrastructure, not a plan abandonment — it never left `Pending`, the
/// Executing-entry stamp never ran) and stays queued with the reason in its
/// note; the run ends only when no lane is in flight (plan ffd7a86f, review
/// R3 — the lanes' resolutions end it when the last one lands).
async fn checkpoint_run_all_item(
    app: &tauri::AppHandle,
    state: &IpcState,
    item: &mnemo::backlog::BacklogItem,
) -> Result<String, String> {
    let checkpoint = {
        let root = state.project.root.lock().await.root.clone();
        checkpoint_on_work_branch(root, item.clone()).await
    };
    match checkpoint {
        Ok(sha) => Ok(sha),
        Err(e) => {
            // Can't checkpoint → can't safely roll back. Stop the run (don't
            // proceed without a safety net) — but the item is NOT stamped
            // (backlog 45dcf577: a checkpoint failure is infrastructure, not
            // a plan abandonment): it never left `Pending` (the
            // Executing-entry stamp never ran), so it stays queued with the
            // reason in its note for a later re-dispatch.
            state
                .backlog
                .store
                .lock()
                .await
                .annotate(&item.id, &format!("git checkpoint failed: {e}"));
            // Parallel run-all (plan ffd7a86f, review R3): the failed item
            // stays Pending (annotated above) — end the run only when no
            // lane is in flight; the lanes' resolutions end it when the
            // last one lands.
            if !any_lane_in_flight(state).await {
                end_run(app, state).await;
            }
            Err(format!("git checkpoint failed: {e}"))
        }
    }
}

/// Record the in-flight item + its checkpoint on the run state. The item
/// stays `Pending` — it is stamped `InFlight` only when the workflow enters
/// `Executing` (forwarder → `stamp_backlog_in_flight`). The fresh checkpoint
/// sha REPLACES any stale note: annotate would leave a previous run's sha at
/// the note's head, breaking head-parsing.
async fn record_in_flight_item(
    state: &IpcState,
    item: &mnemo::backlog::BacklogItem,
    checkpoint: &str,
) {
    {
        let guard = state.backlog.run_all.lock().await;
        if let Some(r) = guard.as_ref() {
            *r.current_item.lock().expect("current_item lock poisoned") = Some(item.id.clone());
        }
    }
    state
        .backlog
        .store
        .lock()
        .await
        .set_note(&item.id, Some(checkpoint.to_string()));
}

/// Fill the parallel dispatch window (plan ffd7a86f): while the run's
/// concurrency allows and another eligible item exists, dispatch it to a
/// SPAWNED worktree agent — item 1 stays on the main agent (the path
/// above); items 2..N each get their own parentless agent + git worktree
/// + `wt/runall-*` branch. Sequential runs (concurrency 1) never enter
/// here — zero behavior change.
///
/// The window counts SPAWNED lanes only (`spawned.len() < concurrency -
/// 1`): the main agent's lane is accounted by its own dispatch path
/// above. When the main agent is busy (its item deferred), the window
/// still fills — each spawned lane is a fresh idle agent owning one item,
/// so the interleaving hazard the main-agent busy guard protects against
/// does not apply.
async fn fill_spawned_window(app: &tauri::AppHandle, state: &IpcState) {
    // Items that failed to dispatch THIS pass (review L4): one stale
    // branch (e.g. a previous run of the same item never landed) must not
    // block the whole window — the failing item is annotated + skipped,
    // the fill continues with the next.
    let mut skip: Vec<String> = Vec::new();
    loop {
        // Snapshot the window state under one run-state hold. Lock order:
        // current_item BEFORE spawned (the resolution path must match).
        let (concurrency, spawned_count, exclude) = {
            let guard = state.backlog.run_all.lock().await;
            let Some(r) = guard.as_ref() else {
                return; // no run active
            };
            let current = r
                .current_item
                .lock()
                .expect("current_item lock poisoned")
                .clone();
            let spawned = r.spawned.lock().expect("spawned lock poisoned");
            let mut exclude: Vec<String> =
                spawned.iter().map(|s| s.item_id.clone()).collect();
            let spawned_count = spawned.len();
            if let Some(id) = current {
                exclude.push(id);
            }
            (r.concurrency, spawned_count, exclude)
        };
        // Sequential runs never spawn (today's behavior, byte-identical).
        if concurrency <= 1 {
            return;
        }
        if spawned_count + 1 >= concurrency {
            return; // window full — the +1 is the main agent's lane
        }
        // The parallel selection: skip the ids already handed out (the
        // store's selection PEEKS — without the exclusion the window
        // would hand the SAME top item to a spawned agent while the main
        // agent works it) plus this pass's failed dispatches.
        let mut exclude = exclude;
        exclude.extend(skip.iter().cloned());
        let Some(item) = state
            .backlog
            .store
            .lock()
            .await
            .next_pending_eligible_excluding(&exclude)
        else {
            return; // nothing left to hand out
        };
        let item_id = item.id.clone();
        match dispatch_spawned_item(app, state, item).await {
            Ok(()) => {}
            Err(e) => {
                eprintln!(
                    "backlog: parallel run-all dispatch failed for item {item_id}: {e}"
                );
                state
                    .backlog
                    .store
                    .lock()
                    .await
                    .annotate(&item_id, &format!("parallel dispatch failed: {e}"));
                skip.push(item_id);
            }
        }
        // Loop — the next iteration re-snapshots (the new entry — or the
        // skip — is in the exclude list, so the window count and selection
        // stay correct).
    }
}

/// Dispatch ONE item to a spawned worktree agent (plan ffd7a86f):
/// provision the item's worktree + branch (a fresh checkout of `main`),
/// checkpoint, spawn a parentless agent bound to the worktree, record the
/// `SpawnedRun`, and send the standard run-all prompt. The agent never
/// merges — the app lands the branch when the item resolves Done.
async fn dispatch_spawned_item(
    app: &tauri::AppHandle,
    state: &IpcState,
    item: mnemo::backlog::BacklogItem,
) -> Result<(), String> {
    // Provision the worktree + branch — each item starts from the LANDED
    // state (main), never from another item's in-flight work. Fails when
    // the branch already exists (a previous run of the same item never
    // landed) — surfaced, never silently reused.
    let (worktree, branch) = mnemo::project::worktrees::provision_item_worktree(
        state.project.root.lock().await.root.clone(),
        item.id.clone(),
    )
    .await?;

    // The checkpoint: the same note format as the main path (the landed
    // predicates parse it unchanged). On a fresh worktree this records
    // `main`'s tip — the provision point — as the anchor. A failure cleans
    // up the provisioned worktree (the item never left `Pending` — it
    // stays queued with the reason in its note).
    let checkpoint = match checkpoint_on_work_branch(worktree.clone(), item.clone()).await {
        Ok(sha) => sha,
        Err(e) => {
            state
                .backlog
                .store
                .lock()
                .await
                .annotate(&item.id, &format!("git checkpoint failed: {e}"));
            let _ = mnemo::project::worktrees::remove_item_worktree(
                state.project.root.lock().await.root.clone(),
                worktree,
                branch,
            )
            .await;
            return Err(format!("git checkpoint failed: {e}"));
        }
    };
    state
        .backlog
        .store
        .lock()
        .await
        .set_note(&item.id, Some(checkpoint));

    // The standard run-all prompt — the same rules the main agent gets
    // (the agent never merges; the app lands the branch on Done).
    let context_block = recalled_context_block(state, &item.text).await;
    let prompt = run_all_prompt(&item.text, context_block.as_deref());
    let images = state.backlog.store.lock().await.resolve_images(&item);

    // Spawn the parentless worktree agent (its own plans dir inside the
    // worktree, a fresh code graph over the worktree tree). No prompt yet
    // — the record below lands first, then the send.
    let short = mnemo::project::worktrees::item_short_id(&item.id);
    let factory = state
        .runtime
        .factory
        .clone()
        .ok_or("agent factory unavailable (brain failed to start)")?;
    let (agent_id, _name) = crate::ipc::spawn::spawn_run_all_agent(
        factory,
        state.runtime.manager.clone(),
        state.runtime.agent_loops.clone(),
        worktree.clone(),
        format!("runall-{short}"),
        None,
    )
    .await?;

    // Record the SpawnedRun — the single source of truth for the dispatch
    // AND the later per-agent resolution (which reads worktree + branch
    // from it). Recorded BEFORE the prompt send so a fast turn can never
    // resolve against a missing record.
    {
        let guard = state.backlog.run_all.lock().await;
        if let Some(r) = guard.as_ref() {
            r.spawned
                .lock()
                .expect("spawned lock poisoned")
                .push(crate::ipc::state::SpawnedRun {
                    agent_id,
                    item_id: item.id.clone(),
                    worktree: worktree.clone(),
                    branch: branch.clone(),
                });
        }
    }

    // ONE command, ONE turn (the same discipline as the main path).
    if let Err(e) = state
        .runtime
        .manager
        .lock()
        .await
        .send(
            agent_id,
            AgentCommand::Prompt {
                text: prompt,
                images: images.clone(),
            },
        )
    {
        // The send failed — the recorded entry + the live (promptless)
        // agent would strand the run forever (review R6: the agent never
        // resolves, so its lane never drains). Remove both + the fresh
        // worktree before surfacing; the item stays Pending (annotated by
        // the fill's skip arm) for a later re-dispatch.
        remove_spawned_run(state, agent_id).await;
        retire_spawned_agent(state, agent_id).await;
        let _ = mnemo::project::worktrees::remove_item_worktree(
            state.project.root.lock().await.root.clone(),
            worktree,
            branch,
        )
        .await;
        return Err(format!("failed to dispatch run-all item: {e:?}"));
    }
    emit_prompt_dispatched(app, agent_id, &item.text, &images);
    emit_backlog_changed(app, state).await;
    Ok(())
}

/// Serializes app-side landings (plan ffd7a86f): one `--no-ff` merge into
/// `main` at a time — concurrent merges into the shared landing worktree
/// would race each other.
static LANDING_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Serializes run-all dispatch decisions (plan ffd7a86f, review R4): the
/// selection→record window spans tens of seconds (a worktree checkout + a
/// checkpoint commit + an agent spawn + a cold graph index), during which
/// the selected item is `Pending` and absent from every exclude list — a
/// concurrent dispatch_next (the between-items compact task, the IPC start
/// task, or a lane's resolution) would select the SAME item and
/// double-dispatch it (two agents on one item: two checkpoints,
/// cross-stamping, leaked worktrees). Holding this across the whole
/// dispatch — including the spawned-window fill — closes the window.
/// Nothing inside the dispatch call tree re-enters (no deadlock).
static DISPATCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Land a spawned item's branch under the landing lock (plan ffd7a86f) —
/// see [`land_item_branch`] for the semantics (serialized `--no-ff`
/// merge via the landing worktree on an unprotected `main`, or a PR on
/// a protected one, backlog b52b041a; conflicts aborted + surfaced with
/// the branch kept).
async fn land_spawned_branch(
    state: &IpcState,
    spawned: &crate::ipc::state::SpawnedRun,
) -> Result<mnemo::project::worktrees::Landed, mnemo::project::worktrees::LandError> {
    let _guard = LANDING_LOCK.lock().await;
    let root = state.project.root.lock().await.root.clone();
    mnemo::project::worktrees::land_item_branch(root, spawned.branch.clone()).await
}

/// Retire a spawned run-all agent after its item resolved (plan
/// ffd7a86f): remove it from the manager + the per-agent loops map — the
/// same cleanup the forwarder's `Exited` arm performs (dropping the handle
/// closes the agent's inbox; its `Exited` event then runs the idempotent
/// remainder of that arm).
async fn retire_spawned_agent(state: &IpcState, agent_id: mnemo::runtime::AgentId) {
    state.runtime.manager.lock().await.remove(agent_id);
    state.runtime.agent_loops.lock().await.remove(&agent_id);
}

/// Whether any lane of the active run is still in flight (plan ffd7a86f,
/// review H1): the main agent's `current_item` OR any spawned worktree
/// agent's entry. Every `end_run` call site must gate on this being false
/// — ending a run with lanes in flight orphans their items (stuck
/// InFlight), branches, and agents (the next resolution finds no run
/// state and no-ops). Lock order: current_item BEFORE spawned (the
/// fill/resolution paths).
pub(crate) async fn any_lane_in_flight(state: &IpcState) -> bool {
    let guard = state.backlog.run_all.lock().await;
    guard.as_ref().map_or(false, lanes_in_flight)
}

/// Whether any lane of a run is still in flight (the per-run core of
/// [`any_lane_in_flight`], plan ffd7a86f review H1): the main agent's
/// `current_item` OR any spawned worktree agent's entry. Every `end_run`
/// call site must gate on this being false — ending a run with lanes in
/// flight orphans their items (stuck InFlight), branches, and agents (the
/// next resolution finds no run state and no-ops). Lock order:
/// current_item BEFORE spawned (the fill/resolution paths).
fn lanes_in_flight(r: &crate::ipc::state::RunAllState) -> bool {
    r.current_item
        .lock()
        .expect("current_item lock poisoned")
        .is_some()
        || !r.spawned.lock().expect("spawned lock poisoned").is_empty()
}

/// Remove a spawned run entry from the run state (plan ffd7a86f).
async fn remove_spawned_run(state: &IpcState, agent_id: mnemo::runtime::AgentId) {
    let guard = state.backlog.run_all.lock().await;
    if let Some(r) = guard.as_ref() {
        r.spawned
            .lock()
            .expect("spawned lock poisoned")
            .retain(|s| s.agent_id != agent_id);
    }
}

/// Whether `agent_id` owns a spawned run-all entry (plan ffd7a86f) — the
/// event forwarder's routing check (cheap: one run-state lock + scan).
pub(crate) async fn owns_spawned_run(
    app: &tauri::AppHandle,
    agent_id: mnemo::runtime::AgentId,
) -> bool {
    let state = app.state::<IpcState>();
    let guard = state.backlog.run_all.lock().await;
    guard.as_ref().map_or(false, |r| {
        r.spawned
            .lock()
            .expect("spawned lock poisoned")
            .iter()
            .any(|s| s.agent_id == agent_id)
    })
}

/// Called by the event forwarder when a SPAWNED run-all worktree agent's
/// turn is fully resolved (agent idle, no running descendants) — the
/// per-agent sibling of [`on_main_turn_resolved`] (plan ffd7a86f).
///
/// Resolves THAT agent's item under the same plan-tied contract as the
/// main path (the plan-loop gate → `Done`; abandonment → `Failed`;
/// anything else → no transition + the run waits — the next turn
/// resolution re-checks), reading THIS agent's workflow — never the main
/// agent's (no cross-agent stamping). A re-queued (Pending) item at
/// resting `Complete` recovers instead of waiting: landed evidence →
/// `Done`, else spawned-run cleanup + dispatch next (backlog 8a6bcece,
/// see the module doc). The spawned specifics:
/// - **Done**: the branch lands app-side (serialized `--no-ff` merge into
///   `main` via the landing worktree — the merge_to_main skill cannot run
///   from a linked worktree), then the item worktree is removed and the
///   agent retired. A landing CONFLICT is surfaced, never auto-resolved:
///   the merge is aborted, the branch + worktree are KEPT for manual
///   resolution, and the conflicted paths land in the item note — the
///   work still counts as `Done` (it is complete; only the landing is
///   deferred).
/// - **Failed**: the item worktree + branch are removed (a re-run starts
///   fresh from `main`; the failure note carries the reason) and the agent
///   retired.
///
/// User interventions are NOT consumed here (the latch is main-agent
/// bookkeeping): a steer on a spawned agent is absorbed as a normal user
/// message — the turn continues and resolves normally.
///
/// After a terminal resolution: bump the done counter, remove the
/// `SpawnedRun`, retire the agent, and hand the pacing back to
/// [`run_all_dispatch_next`] (which dispatches the main agent's next item
/// when free, refills the spawned window, and ends the run when nothing
/// eligible remains).
pub async fn on_spawned_turn_resolved(
    app: &tauri::AppHandle,
    agent_id: mnemo::runtime::AgentId,
    success: bool,
    error: Option<String>,
    loop_evidence: bool,
    plan_abandoned: bool,
    abandoned_plan_id: Option<&str>,
) {
    let state = app.state::<IpcState>();

    // Find THIS agent's spawned run (cloned — removed only after a
    // terminal resolution; a crash-drain racing this path re-finds it).
    let spawned = {
        let guard = state.backlog.run_all.lock().await;
        guard.as_ref().and_then(|r| {
            r.spawned
                .lock()
                .expect("spawned lock poisoned")
                .iter()
                .find(|s| s.agent_id == agent_id)
                .cloned()
        })
    };
    let Some(spawned) = spawned else {
        return; // not ours (the run ended / the drain already requeued it)
    };

    // Read the item (status + plan linkage + note) before mutating.
    let item = {
        let store = state.backlog.store.lock().await;
        store
            .items()
            .iter()
            .find(|i| i.id == spawned.item_id)
            .cloned()
    };
    let Some(item) = item else {
        // The item vanished mid-run — drop the entry + clean up.
        remove_spawned_run(&state, agent_id).await;
        let _ = mnemo::project::worktrees::remove_item_worktree(
            state.project.root.lock().await.root.clone(),
            spawned.worktree.clone(),
            spawned.branch.clone(),
        )
        .await;
        retire_spawned_agent(&state, agent_id).await;
        if let Err(e) = run_all_dispatch_next(app, &state).await {
            run_all_diag(&format!("failed to dispatch next item: {e}"));
        }
        return;
    };

    let mut terminal_resolution = false;
    let mut remove_worktree = false;
    // Backlog b52b041a: a PR-path landing keeps the branch (the PR's
    // head — the human merges); every other removal deletes it.
    let mut keep_branch = false;
    if success {
        // The same plan-loop gate as the main path, read from THIS
        // agent's workflow. The closed_earlier recovery (backlog
        // e33a07fd) applies here too — the landed predicate is
        // worktree-aware (the plan file lives in the worktree's plans
        // dir; the commits are on the item's own branch).
        let agent_state = agent_workflow_state(&state, agent_id).await;
        let closed_earlier = agent_state == Some(WorkflowState::Complete)
            && !loop_evidence
            && item.status == BacklogStatus::InFlight
            && orphan_work_landed_at(
                spawned_plans_dir(&spawned.worktree, agent_id),
                spawned.worktree.clone(),
                item.plan_id.as_deref(),
                item.note.as_deref(),
            )
            .await;
        match (agent_state, loop_evidence || closed_earlier) {
            (Some(ws), true) if plan_loop_allows_done(ws, true) => {
                // The same linkage guard as the main path: only a
                // still-InFlight item that owns the completing plan
                // flips.
                let completed_plan = agent_top_plan_id(&state, agent_id).await;
                let may_flip = item.status == BacklogStatus::InFlight
                    && (closed_earlier
                        || plan_linkage_allows_done(
                            item.status,
                            item.plan_id.as_deref(),
                            completed_plan.as_deref(),
                        ));
                if may_flip {
                    // Commit the worktree's bookkeeping on the item's own
                    // branch (the WORKTREE root, never the main tree).
                    if let Err(e) = commit_success(spawned.worktree.clone(), item.clone()).await
                    {
                        eprintln!(
                            "backlog: commit failed for spawned item {}: {e}",
                            item.id
                        );
                    }
                    // Land the branch app-side (serialized). A conflict
                    // is surfaced — the branch + worktree are KEPT and the
                    // note carries the conflicted paths — the work still
                    // counts as Done.
                    let note = match land_spawned_branch(&state, &spawned).await {
                        Ok(mnemo::project::worktrees::Landed::Merged) => {
                            remove_worktree = true;
                            None
                        }
                        Ok(mnemo::project::worktrees::Landed::PullRequest(url)) => {
                            // Backlog b52b041a: a protected main lands via
                            // PR — the branch is KEPT (the PR's head; the
                            // human merges), only the worktree goes, and
                            // the item note carries the URL.
                            remove_worktree = true;
                            keep_branch = true;
                            Some(format!(
                                "landed via pull request (branch {} kept for the human merge): {url}",
                                spawned.branch
                            ))
                        }
                        Err(mnemo::project::worktrees::LandError::Conflict(files)) => {
                            eprintln!(
                                "backlog: landing conflict for branch {} (kept for manual resolution): {}",
                                spawned.branch,
                                files.join(", ")
                            );
                            Some(format!(
                                "merge conflict on landing (branch {} kept for manual resolution): {}",
                                spawned.branch,
                                files.join(", ")
                            ))
                        }
                        Err(mnemo::project::worktrees::LandError::Git(e)) => {
                            eprintln!(
                                "backlog: landing failed for branch {}: {e}",
                                spawned.branch
                            );
                            Some(format!(
                                "landing failed (branch {} kept): {e}",
                                spawned.branch
                            ))
                        }
                    };
                    // Re-verify InFlight under the lock (a concurrent drain
                    // may have requeued it), then transition.
                    let still = {
                        let store = state.backlog.store.lock().await;
                        store
                            .items()
                            .iter()
                            .any(|i| i.id == item.id && i.status == BacklogStatus::InFlight)
                    };
                    if still {
                        // Gate on the transition's OWN result (review
                        // round-2 LOW-1): the `still` pre-check above can
                        // be raced by the exit drain's Done flip in the
                        // lock re-acquisition gap — a refused transition
                        // must not double-count the item in the run's done
                        // counter (the drain's gated bump already counted
                        // it).
                        terminal_resolution = state
                            .backlog
                            .store
                            .lock()
                            .await
                            .transition(&item.id, BacklogStatus::Done, note);
                    }
                } else {
                    // The blocked row (review LOW-2, 2026-09-08): a
                    // completing turn skipped this item — record why. The
                    // worktree is still removed (review LOW-1, backlog
                    // bba2c82d round 1): the spawned run is over and the
                    // worktree has no future owner — a stale
                    // `wt/runall-*` branch would block every later lane
                    // dispatch of the item ("branch already exists").
                    remove_worktree = true;
                    state.backlog.store.lock().await.annotate(
                        &item.id,
                        "completing plan is not this item's plan — item kept, no transition",
                    );
                }
            }
            // The root plan was abandoned this turn — the ONE true failure
            // (backlog 45dcf577). Terminal. Guarded on status + linkage
            // (backlog bba2c82d): only a currently-InFlight item whose
            // plan_id IS the abandoned plan may flip — a stale pointer
            // referencing a re-queued (Pending) item must stay Pending
            // with its note preserved.
            _ if plan_abandoned => {
                let may_flip = {
                    let store = state.backlog.store.lock().await;
                    store
                        .items()
                        .iter()
                        .find(|i| i.id == item.id)
                        .map(|i| {
                            plan_linkage_allows_failed(
                                i.status,
                                i.plan_id.as_deref(),
                                abandoned_plan_id,
                            )
                        })
                        .unwrap_or(false)
                };
                if may_flip {
                    state.backlog.store.lock().await.transition(
                        &item.id,
                        BacklogStatus::Failed,
                        Some("plan abandoned (abandon_plan)".to_string()),
                    );
                    terminal_resolution = true;
                    remove_worktree = true;
                } else {
                    // The blocked flip: no transition, NO annotate. The
                    // wipe damage is `transition`'s (it replaces the
                    // note); `annotate` appends — but the re-queue note
                    // is recovery evidence and stays unpolluted either
                    // way (the deliberate asymmetry with the Done-blocked
                    // row, which annotates a transient note). The worktree
                    // is still removed (review LOW-1): the spawned run is
                    // over and the worktree has no future owner — a
                    // stale `wt/runall-*` branch would block every later
                    // lane dispatch of the item ("branch already
                    // exists").
                    remove_worktree = true;
                    eprintln!(
                        "backlog: abandoned plan is not this item's plan — item kept, no transition (item {})",
                        item.id
                    );
                }
            }
            // (backlog 8a6bcece, review LOW-3 2027-01-08) The spawned-lane
            // mirror of the main path's re-queued-item recovery: the same
            // (Complete, false) + Pending landing — the item externally
            // re-queued mid-run (the store sees Pending; the in-memory
            // spawned-run record still references it) with the slid
            // resolution (the e33a07fd slide) — stalled the lane forever
            // (the waiting arm below exits early; the spawned run never
            // resolves; the run waits for the lane). The same recovery,
            // with worktree-aware landed evidence (the plan file lives
            // in the worktree's plans dir; the commits are on the item's
            // own branch):
            // (1) LANDED → land the branch app-side (a conflict keeps the
            //     branch + the note carries it — the work still counts
            //     as Done) + resolve Done with the drain's done-orphan
            //     note-preserving suffix. A Pending item with NO plan_id
            //     never resolves Done (the 2026-08-20 hole stays plugged).
            // (2) Otherwise: fall through to the spawned-run cleanup +
            //     dispatch next — the item is already Pending (eligible
            //     for re-dispatch); the spawned run is over. No annotate
            //     (the re-queue note is recovery evidence — the bba2c82d
            //     discipline).
            (Some(WorkflowState::Complete), false)
                if item.status == BacklogStatus::Pending =>
            {
                // Snapshot note + plan id BEFORE any transition (the
                // transition overwrites the note; the checkpoint sha
                // must survive).
                let (note, plan_id) = {
                    let store = state.backlog.store.lock().await;
                    match store.items().iter().find(|i| i.id == item.id) {
                        Some(i) => (i.note.clone(), i.plan_id.clone()),
                        None => (None, None),
                    }
                };
                // Consult landed-work evidence WITHOUT the store lock
                // (the git check is a blocking subprocess), then
                // re-verify the item is still Pending under the lock.
                let landed = orphan_work_landed_at(
                    spawned_plans_dir(&spawned.worktree, agent_id),
                    spawned.worktree.clone(),
                    plan_id.as_deref(),
                    note.as_deref(),
                )
                .await;
                let still_pending = {
                    let store = state.backlog.store.lock().await;
                    store
                        .items()
                        .iter()
                        .any(|i| i.id == item.id && i.status == BacklogStatus::Pending)
                };
                if landed && still_pending {
                    // Land the branch app-side (serialized). A conflict
                    // keeps the branch + worktree and the note carries
                    // it — the work still counts as Done (the may_flip
                    // arm's landing discipline).
                    let mut suffix = "work already landed (plan complete + commits after the checkpoint) — auto-resolved done, not re-dispatched".to_string();
                    match land_spawned_branch(&state, &spawned).await {
                        Ok(mnemo::project::worktrees::Landed::Merged) => {
                            remove_worktree = true;
                        }
                        Ok(mnemo::project::worktrees::Landed::PullRequest(url)) => {
                            // Backlog b52b041a: a protected main lands via
                            // PR — the branch is KEPT (the PR's head), only
                            // the worktree goes, and the note carries the
                            // URL.
                            remove_worktree = true;
                            keep_branch = true;
                            suffix = format!(
                                "{suffix} — landed via pull request (branch {} kept for the human merge): {url}",
                                spawned.branch
                            );
                        }
                        Err(mnemo::project::worktrees::LandError::Conflict(files)) => {
                            eprintln!(
                                "backlog: landing conflict for branch {} (kept for manual resolution): {}",
                                spawned.branch,
                                files.join(", ")
                            );
                            suffix = format!(
                                "{suffix} — merge conflict on landing (branch {} kept): {}",
                                spawned.branch,
                                files.join(", ")
                            );
                        }
                        Err(mnemo::project::worktrees::LandError::Git(e)) => {
                            eprintln!(
                                "backlog: landing failed for branch {}: {e}",
                                spawned.branch
                            );
                            suffix = format!(
                                "{suffix} — landing failed (branch {} kept): {e}",
                                spawned.branch
                            );
                        }
                    }
                    let full = match note {
                        Some(n) => format!("{n} | {suffix}"),
                        None => suffix,
                    };
                    state
                        .backlog
                        .store
                        .lock()
                        .await
                        .transition(&item.id, BacklogStatus::Done, Some(full));
                    terminal_resolution = true;
                } else {
                    // The spawned run is over and the worktree has no
                    // future owner — a stale `wt/runall-*` branch would
                    // block every later lane dispatch of the item
                    // ("branch already exists"). The item is Pending
                    // (eligible for re-dispatch); the tail's spawned-run
                    // cleanup + dispatch next are the recovery.
                    remove_worktree = true;
                    eprintln!(
                        "backlog: slid resolution with a re-queued (Pending) item — stale spawned run cleaned up, item re-queued for dispatch (item {})",
                        item.id
                    );
                }
            }
            // Any other turn end is NOT a failure (backlog 45dcf577): the
            // item keeps its status and the run WAITS — the agent
            // routinely recovers on its own; the next turn resolution
            // re-checks.
            (ws, _) => {
                let note = plan_open_note(ws);
                let already_waiting = {
                    let store = state.backlog.store.lock().await;
                    store
                        .items()
                        .iter()
                        .find(|i| i.id == item.id)
                        .and_then(|i| i.note.as_deref())
                        .map_or(false, |n| n.contains(note.as_str()))
                };
                if !already_waiting {
                    state.backlog.store.lock().await.annotate(&item.id, &note);
                }
                emit_backlog_changed(app, &state).await;
                return;
            }
        }
    } else if plan_abandoned {
        // A failed turn whose root plan was abandoned: the abandonment is
        // the disposition (backlog 45dcf577). Guarded on status + linkage
        // (backlog bba2c82d) — same discipline as the success-path arm
        // above.
        let may_flip = {
            let store = state.backlog.store.lock().await;
            store
                .items()
                .iter()
                .find(|i| i.id == item.id)
                .map(|i| {
                    plan_linkage_allows_failed(
                        i.status,
                        i.plan_id.as_deref(),
                        abandoned_plan_id,
                    )
                })
                .unwrap_or(false)
        };
        if may_flip {
            state.backlog.store.lock().await.transition(
                &item.id,
                BacklogStatus::Failed,
                Some("plan abandoned (abandon_plan)".to_string()),
            );
            terminal_resolution = true;
            remove_worktree = true;
        } else {
            // The blocked flip: no transition, NO annotate (the re-queue
            // note is recovery evidence — `annotate` appends rather than
            // wipes, but it stays unpolluted; the wipe damage is
            // `transition`'s). The worktree is still removed (review
            // LOW-1 — same reasoning as the success-path arm above).
            remove_worktree = true;
            eprintln!(
                "backlog: abandoned plan is not this item's plan — item kept, no transition (item {})",
                item.id
            );
        }
    } else {
        // A terminal Error is NOT an abandonment (backlog 45dcf577): no
        // transition — the item keeps its status and the run WAITS (the
        // next turn resolution re-checks).
        let reason = error.unwrap_or_else(|| "turn failed".to_string());
        let note = format!(
            "{reason} — item left in flight; work kept on its branch; run waits for the resumed turn"
        );
        let already_waiting = {
            let store = state.backlog.store.lock().await;
            store
                .items()
                .iter()
                .find(|i| i.id == item.id)
                .and_then(|i| i.note.as_deref())
                .map_or(false, |n| n.contains(note.as_str()))
        };
        if !already_waiting {
            state.backlog.store.lock().await.annotate(&item.id, &note);
        }
        emit_backlog_changed(app, &state).await;
        return;
    }

    // Terminal resolution: bump the done counter (only for an arm that
    // actually transitioned — backlog 5bb1e4cd), remove the entry, clean
    // up, retire the agent, and hand the pacing back to dispatch_next.
    if terminal_resolution {
        let guard = state.backlog.run_all.lock().await;
        if let Some(r) = guard.as_ref() {
            r.done.fetch_add(1, Ordering::Relaxed);
        }
    }
    remove_spawned_run(&state, agent_id).await;
    if remove_worktree {
        // Backlog b52b041a: a PR-path landing keeps the branch (the
        // PR's head — the human merges); every other removal deletes it
        // (a stale `wt/runall-*` branch would block later dispatches).
        let root = state.project.root.lock().await.root.clone();
        let _ = if keep_branch {
            mnemo::project::worktrees::remove_worktree_only(
                root,
                spawned.worktree.clone(),
            )
            .await
        } else {
            mnemo::project::worktrees::remove_item_worktree(
                root,
                spawned.worktree.clone(),
                spawned.branch.clone(),
            )
            .await
        };
    }
    retire_spawned_agent(&state, agent_id).await;
    emit_backlog_changed(app, &state).await;
    // The stopped check (the run's brake): end the run when nothing else
    // is in flight; otherwise hand the pacing back to dispatch_next
    // (which dispatches the main agent's next item when free, refills
    // the spawned window, and ends the run when nothing eligible
    // remains).
    let stopped = {
        let guard = state.backlog.run_all.lock().await;
        guard
            .as_ref()
            .map_or(false, |r| r.stop.load(Ordering::Relaxed))
    };
    if stopped {
        let any_in_flight = {
            let guard = state.backlog.run_all.lock().await;
            guard.as_ref().map_or(false, |r| {
                r.current_item
                    .lock()
                    .expect("current_item lock poisoned")
                    .is_some()
                    || !r.spawned.lock().expect("spawned lock poisoned").is_empty()
            })
        };
        if !any_in_flight {
            end_run(app, &state).await;
        }
        return;
    }
    if let Err(e) = run_all_dispatch_next(app, &state).await {
        run_all_diag(&format!("failed to dispatch next item: {e}"));
    }
}

/// Drain a spawned run-all agent that EXITED without a turn resolution
/// (plan ffd7a86f — the spawned sibling of
/// [`drain_run_all_on_main_exit`]): a cancel or crash mid-item bypasses
/// the resolution path entirely. The item is requeued to `Pending`
/// (checkpoint sha preserved) — unless the work already landed (the
/// done-orphan guard, backlog 6c6966b9: plan complete + commits after the
/// checkpoint → land the branch app-side, then auto-resolve `Done`) — and
/// the worktree + branch are removed (a re-dispatch starts fresh from
/// `main`; the crashed run's incomplete work is not kept — the note
/// explains). No-op when the agent owns no spawned run.
pub(crate) async fn drain_spawned_on_exit(
    app: &tauri::AppHandle,
    agent_id: mnemo::runtime::AgentId,
) {
    let state = app.state::<IpcState>();
    // Find + take THIS agent's spawned run.
    let spawned = {
        let guard = state.backlog.run_all.lock().await;
        guard.as_ref().and_then(|r| {
            let mut spawned = r.spawned.lock().expect("spawned lock poisoned");
            spawned
                .iter()
                .position(|s| s.agent_id == agent_id)
                .map(|ix| spawned.remove(ix))
        })
    };
    let Some(spawned) = spawned else {
        return;
    };
    // Snapshot note + plan id BEFORE the transition (the transition
    // overwrites the note; the checkpoint sha must survive).
    let (note, plan_id) = {
        let store = state.backlog.store.lock().await;
        match store.items().iter().find(|i| i.id == spawned.item_id) {
            Some(item) => (item.note.clone(), item.plan_id.clone()),
            None => (None, None),
        }
    };
    let landed = orphan_work_landed_at(
        spawned_plans_dir(&spawned.worktree, agent_id),
        spawned.worktree.clone(),
        plan_id.as_deref(),
        note.as_deref(),
    )
    .await;
    // Re-verify InFlight under the lock before transitioning (a racing
    // turn resolution may have transitioned it already).
    let still_in_flight = {
        let store = state.backlog.store.lock().await;
        store
            .items()
            .iter()
            .any(|i| i.id == spawned.item_id && i.status == BacklogStatus::InFlight)
    };
    if still_in_flight {
        if landed {
            // The dead session's work already landed on the branch — land
            // it into `main` (best-effort; a conflict keeps the branch +
            // annotates), then auto-resolve Done instead of requeueing
            // for duplicate re-dispatch.
            let mut suffix = "work already landed (plan complete + commits after the checkpoint) — auto-resolved done".to_string();
            match land_spawned_branch(&state, &spawned).await {
                Ok(mnemo::project::worktrees::Landed::Merged) => {
                    let _ = mnemo::project::worktrees::remove_item_worktree(
                        state.project.root.lock().await.root.clone(),
                        spawned.worktree.clone(),
                        spawned.branch.clone(),
                    )
                    .await;
                }
                Ok(mnemo::project::worktrees::Landed::PullRequest(url)) => {
                    // Backlog b52b041a: a protected main lands via PR —
                    // the branch is KEPT (the PR's head), only the
                    // worktree goes, and the note carries the URL.
                    let _ = mnemo::project::worktrees::remove_worktree_only(
                        state.project.root.lock().await.root.clone(),
                        spawned.worktree.clone(),
                    )
                    .await;
                    suffix = format!(
                        "{suffix} — landed via pull request (branch {} kept for the human merge): {url}",
                        spawned.branch
                    );
                }
                Err(mnemo::project::worktrees::LandError::Conflict(files)) => {
                    suffix = format!(
                        "{suffix} — merge conflict on landing (branch {} kept): {}",
                        spawned.branch,
                        files.join(", ")
                    );
                }
                Err(mnemo::project::worktrees::LandError::Git(e)) => {
                    suffix = format!(
                        "{suffix} — landing failed (branch {} kept): {e}",
                        spawned.branch
                    );
                }
            }
            let full = match note {
                Some(n) => format!("{n} | {suffix}"),
                None => suffix,
            };
            let transitioned = state
                .backlog
                .store
                .lock()
                .await
                .transition(&spawned.item_id, BacklogStatus::Done, Some(full));
            if transitioned {
                // Symmetric with the resolution paths
                // (on_spawned_turn_resolved / resolve_run_all_turn): the
                // drain's OWN successful Done transition bumps the run's
                // done counter — the delayed continuation (spawned off the
                // forwarder, review L2) finds the row already Done and
                // bumps nothing, so without this the UI progress line can
                // miss a lane (review round-1 LOW-1).
                let guard = state.backlog.run_all.lock().await;
                if let Some(r) = guard.as_ref() {
                    r.done.fetch_add(1, Ordering::Relaxed);
                }
            }
        } else {
            let suffix = "spawned agent exited while the run was active — returned to queue; a fresh run starts from main";
            let full = match note {
                Some(n) => format!("{n} | {suffix}"),
                None => suffix.to_string(),
            };
            state
                .backlog
                .store
                .lock()
                .await
                .transition(&spawned.item_id, BacklogStatus::Pending, Some(full));
        }
    }
    // Cleanup: a requeued (not-landed) item's worktree + branch are
    // removed — a fresh re-dispatch starts from `main` (the incomplete
    // work of a crashed agent is not kept; the note explains). A landed
    // branch was handled above (removed on Ok, kept on conflict).
    if !landed {
        let _ = mnemo::project::worktrees::remove_item_worktree(
            state.project.root.lock().await.root.clone(),
            spawned.worktree.clone(),
            spawned.branch.clone(),
        )
        .await;
    }
    emit_backlog_changed(app, &state).await;
    // Re-drive the run (plan ffd7a86f, review R5): a crashed lane with an
    // idle main lane must dispatch the requeued item (or the next one) —
    // otherwise the run stalls until the user intervenes. During a
    // stopped wind-down, the LAST lane exiting here ends the run
    // (nothing else would).
    let stopped = {
        let guard = state.backlog.run_all.lock().await;
        guard
            .as_ref()
            .map_or(false, |r| r.stop.load(Ordering::Relaxed))
    };
    if stopped {
        if !any_lane_in_flight(&state).await {
            end_run(app, &state).await;
        }
        return;
    }
    if let Err(e) = run_all_dispatch_next(app, &state).await {
        run_all_diag(&format!("failed to dispatch next item: {e}"));
    }
}

/// Stamp a SPAWNED run-all item `InFlight` when ITS agent's workflow
/// enters `Executing` (plan ffd7a86f — the spawned sibling of
/// [`stamp_backlog_in_flight`]): items leave `Pending` on execution
/// entry, never at dispatch. Records the root plan id + title (the
/// linkage guard's ownership check) from the WORKTREE's plans dir (the
/// spawned agent's plan file lives on its own branch). No-op when the
/// agent owns no spawned run.
pub(crate) async fn stamp_spawned_in_flight(
    app: &tauri::AppHandle,
    agent_id: mnemo::runtime::AgentId,
    top_plan_id: Option<&str>,
) {
    let state = app.state::<IpcState>();
    // Find THIS agent's item + worktree (never the main agent's — no
    // cross-agent stamping).
    let found = {
        let guard = state.backlog.run_all.lock().await;
        guard.as_ref().and_then(|r| {
            r.spawned
                .lock()
                .expect("spawned lock poisoned")
                .iter()
                .find(|s| s.agent_id == agent_id)
                .map(|s| (s.item_id.clone(), s.worktree.clone()))
        })
    };
    let Some((item_id, worktree)) = found else {
        return;
    };
    let mut store = state.backlog.store.lock().await;
    let status = store
        .items()
        .iter()
        .find(|i| i.id == item_id)
        .map(|i| i.status);
    if status == Some(BacklogStatus::Pending) {
        store.transition(&item_id, BacklogStatus::InFlight, None);
    }
    // Item↔plan linkage: refresh on EVERY Executing entry (mirroring
    // stamp_backlog_in_flight) — the plan TITLE rides along, read from
    // the WORKTREE's per-agent plans dir (review H3: the spawned agent's
    // plan file lives under `agents/<agent_id>/`, not the top level).
    let plans_dir = spawned_plans_dir(&worktree, agent_id);
    let plan_title = top_plan_id.and_then(|pid| plan_title_from_file(&plans_dir, pid));
    store.set_plan_id(&item_id, top_plan_id, plan_title.as_deref());
}

/// Called by the event forwarder when the MAIN agent's turn is fully resolved
/// (agent idle, no running descendants). Advances the backlog under the
/// plan-tied contract (backlog 45dcf577 — see the module doc):
/// - **Run-All active**: resolve the in-flight item (gate → `Done`;
///   abandoned → `Failed`; anything else → no transition + the run waits —
///   the next turn resolution re-checks), then dispatch the next (unless
///   stopped). A re-queued (Pending) item at resting `Complete` recovers
///   instead of waiting: landed evidence → `Done`, else pointer clear +
///   dispatch next (backlog 8a6bcece, see the module doc).
/// - **Auto-feed on**: dispatch the next pending item — only past a
///   terminally resolved item.
///
/// `success` is `false` when the turn ended in a final error; `error` carries
/// the message for the item note. `loop_evidence` is whether any workflow
/// state transition was observed for the main agent during this turn (see
/// `TurnResolveLatch::workflow_changed`) — required IN ADDITION to a terminal
/// `Complete` state before an item may be marked `Done` (`Complete` is a
/// resting state; a turn that never planned ends there too).
/// `plan_abandoned` is whether the ROOT plan was abandoned this turn (see
/// `TurnResolveLatch::plan_abandoned`) — the one turn event that marks an
/// item `Failed` (backlog 45dcf577).
pub async fn on_main_turn_resolved(
    app: &tauri::AppHandle,
    success: bool,
    error: Option<String>,
    loop_evidence: bool,
    plan_abandoned: bool,
    abandoned_plan_id: Option<&str>,
) {
    let state = app.state::<IpcState>();

    // USER INTERVENTION: a steer or interrupt landed on the main agent while
    // a backlog item was in flight. An intervention is not a task failure —
    // the affected item is never marked Failed/CantResolve (a run-all item
    // is KEPT InFlight with its run stopped — the plan stays active and the
    // turn that completes it resolves the item, backlog b83e891f; a
    // single-dispatch item is KEPT InFlight too with its pointer restored,
    // plan cace17a6), and the run does NOT auto-continue.
    // Consume the latch FIRST so exactly one resolution handles it. If the
    // agent nevertheless closed the plan loop after absorbing the steer, the
    // turn is a real success — fall through to the normal Done path rather
    // than discarding completed work.
    let intervention = state
        .backlog
        .user_intervention
        .lock()
        .expect("user_intervention lock poisoned")
        .take();
    if let Some(iv) = intervention {
        let closed_loop = match main_agent_workflow_state(&state).await {
            Some(ws) => success && plan_loop_allows_done(ws, loop_evidence),
            // Unverifiable state: treat as not-closed (conservative —
            // mirrors the plan gate's "None is never success" rule).
            None => false,
        };
        if !closed_loop {
            handle_user_intervention(app, &state, iv).await;
            return;
        }
        // The agent absorbed the intervention and still closed the plan
        // loop: a real success. But the normal Done path can only resolve
        // items it can SEE — and this latch path returns before the run-all
        // branch, so a captured item id must be resolved HERE (commit +
        // Done, no next dispatch — an intervention must not auto-continue)
        // or the item strands at InFlight with no UI recovery path (requeue
        // refuses InFlight, dispatch requires Pending).
        if let Some(id) = iv.item_id {
            finish_captured_item_done(app, &state, &id).await;
            // The interrupted run, if still active (an interrupt does not
            // halt), ends here: the loop closed, nothing of that item is
            // left to resolve, and an intervention must not auto-continue
            // into the next item. A run with a different — or empty —
            // in-flight pointer is not ours: a deferred Run-All the user
            // started mid-turn (`current_item == None`) dispatches on its
            // own next resolution.
            // Parallel run-all (plan ffd7a86f, reviews R3 + R3-H1 + R4-L1
            // + R5-L1): the whole determination→clear→wind-down runs
            // under DISPATCH_LOCK — the same lock every dispatch decision
            // holds. This is the ONE wind-down that runs while the main
            // agent is alive and dispatchable, and an interleaved
            // dispatch's current_item write (or the fill's SpawnedRun
            // record) landing between the ours check and the
            // clear/end_run would orphan state — the exact class the lock
            // exists to close. The DETERMINATION sits inside the lock
            // (R5-L1): R4-L1's race (a) needed the check and the clear
            // under the same hold — a determination outside the lock
            // leaves the check→clear seam open (a dispatch's pointer
            // write landing there is wiped by the clear). Nothing in the
            // arm calls dispatch_next (no re-entrancy); the guard drops
            // at the return below.
            let _dispatch_guard = DISPATCH_LOCK.lock().await;
            let ours = {
                let guard = state.backlog.run_all.lock().await;
                let live = guard.as_ref().map(|r| {
                    r.current_item
                        .lock()
                        .expect("current_item lock poisoned")
                        .clone()
                });
                matches!(
                    live.as_ref().and_then(|o| o.as_deref()),
                    Some(interrupted) if interrupted == id.as_str()
                )
            };
            if ours {
                // With spawned lanes in flight the intervention WINDS DOWN
                // (the stop flag; each lane's resolution ends the run when
                // the last one lands) instead of ending outright, which
                // would orphan them. The steered item was just resolved
                // Done above — clear the main lane's pointer BEFORE the
                // wind-down (mirroring drain_run_all_on_main_exit's R2
                // clear): a stale `Some` keeps every end-of-run check true
                // forever, so the run would hang active after the last
                // lane drains.
                let spawned_in_flight = {
                    let mut guard = state.backlog.run_all.lock().await;
                    match guard.as_mut() {
                        Some(r) => {
                            // R3-H1: the steered item was just resolved
                            // Done — clear the pointer or the wind-down
                            // never terminates.
                            *r.current_item.lock().expect("current_item lock poisoned") =
                                None;
                            !r.spawned.lock().expect("spawned lock poisoned").is_empty()
                        }
                        None => false,
                    }
                };
                if spawned_in_flight {
                    let guard = state.backlog.run_all.lock().await;
                    if let Some(r) = guard.as_ref() {
                        r.stop.store(true, Ordering::Relaxed);
                    }
                } else {
                    end_run(app, &state).await;
                }
            }
            return;
        }
        // Pointer-less interventions (single-dispatch) fall through: the
        // normal path below still resolves them via `single_in_flight`.
    }

    // Run-All takes priority.
    let run_all_active = state.backlog.run_all.lock().await.is_some();
    if run_all_active {
        resolve_run_all_turn(
            app,
            &state,
            success,
            error,
            loop_evidence,
            plan_abandoned,
            abandoned_plan_id,
        )
        .await;
        return;
    }

    // No Run-All — resolve the single-dispatch / auto-feed in-flight item (if
    // any), then auto-feed the next pending item if enabled.
    resolve_single_dispatch_turn(
        app,
        &state,
        success,
        error,
        loop_evidence,
        plan_abandoned,
        abandoned_plan_id,
    )
    .await;
}

/// Resolve the single-dispatch / auto-feed in-flight item (if any) against a
/// finished main-agent turn, then auto-feed the next pending item when
/// enabled and the item resolved terminally. The (status, note)
/// determination lives in [`single_dispatch_disposition`]; this function
/// owns the pointer lifecycle: take → transition-or-restore → emit →
/// auto-feed.
async fn resolve_single_dispatch_turn(
    app: &tauri::AppHandle,
    state: &IpcState,
    success: bool,
    error: Option<String>,
    loop_evidence: bool,
    plan_abandoned: bool,
    abandoned_plan_id: Option<&str>,
) {
    let in_flight_id = state
        .backlog
        .single_in_flight
        .lock()
        .expect("single_in_flight lock poisoned")
        .take();
    let mut resolved_terminally = true;
    if let Some(in_flight_id) = in_flight_id {
        // The plan-loop gate applies to EVERY backlog dispatch path, not just
        // Run-All — see single_dispatch_disposition for the full gate/linkage
        // rationale (the Done row is linkage-guarded, backlog 5bb1e4cd; the
        // Failed flip is guarded like the Done flip, backlog bba2c82d).
        let (status, note) = single_dispatch_disposition(
            state,
            &in_flight_id,
            success,
            error,
            loop_evidence,
            plan_abandoned,
            abandoned_plan_id,
        )
        .await;
        match status {
            Some(status) => {
                state
                    .backlog
                    .store
                    .lock()
                    .await
                    .transition(&in_flight_id, status, note);
            }
            None => {
                // Not resolved — NOT a failure: record what happened; the
                // item keeps its status. The pointer was consumed by the
                // take above — RESTORE it (the amplifier's single-dispatch
                // mirror, 2027-01-07): the completing turn's resolution
                // must re-check the same item, not run blind (a consumed
                // pointer stranded the ▶-dispatched item in_flight).
                // annotate APPENDS — with the pointer restored, EVERY main
                // turn end re-checks this arm (including the user's
                // ordinary interactive chat turns), so skip only an EXACT
                // repeat (the run-all arms' already_waiting guard,
                // mirrored — review LOW-2/LOW-3): a different note (a
                // later abort reason, a state progression) still annotates.
                if let Some(note) = &note {
                    let already_waiting = {
                        let store = state.backlog.store.lock().await;
                        store
                            .items()
                            .iter()
                            .find(|i| i.id == in_flight_id)
                            .and_then(|i| i.note.as_deref())
                            .map_or(false, |n| n.contains(note.as_str()))
                    };
                    if !already_waiting {
                        state
                            .backlog
                            .store
                            .lock()
                            .await
                            .annotate(&in_flight_id, note);
                    }
                }
                // Check-and-set (review LOW-4): a concurrent ▶ dispatch
                // (an IPC command thread) landing inside the take→restore
                // window sets the slot to the NEW item — only restore when
                // the slot is still empty so the concurrent dispatch wins.
                {
                    let mut slot = state
                        .backlog
                        .single_in_flight
                        .lock()
                        .expect("single_in_flight lock poisoned");
                    if slot.is_none() {
                        *slot = Some(in_flight_id);
                    }
                }
                resolved_terminally = false;
            }
        }
        emit_backlog_changed(app, &state).await;
    }
    if resolved_terminally && state.backlog.auto_feed.load(Ordering::Relaxed) {
        // Auto-feed only past a terminally resolved item (Done/Failed) or
        // with no item in flight (backlog 45dcf577): a still-`InFlight` item
        // means its plan is still active (a new dispatch would land on it),
        // and a still-`Pending` item means no plan ran (a re-dispatch would
        // loop on the same item).
        // A9: log the dispatch error instead of silently swallowing it.
        if let Err(e) = dispatch_next_impl(app, &state).await {
            eprintln!("backlog: auto-feed failed to dispatch next item: {e}");
        }
    }
}

/// The (status, note) determination for the single-dispatch in-flight item:
/// the plan-loop gate applies to EVERY backlog dispatch path, not just
/// Run-All — an item is Done only when the plan loop fully closed (workflow
/// == Complete at turn resolution). Under the plan-tied contract (backlog
/// 45dcf577) every OTHER turn end — loop open, resting Complete,
/// unverifiable state, or a terminal Error — is NOT a failure: the item
/// keeps its status (`InFlight` while its plan is active, `Pending` when no
/// plan ever ran) and the note records what happened. This path has no git
/// checkpoint, so there is nothing to roll back.
/// (backlog 5bb1e4cd) The Done row is guarded: only a currently InFlight
/// item that owns the completing plan (its plan_id matches the workflow's
/// root plan id, kept after finish) may flip — a re-queued (Pending) item or
/// a mismatched/absent linkage must not transition (the note would be
/// wiped). A blocked flip rides the EXISTING non-closure machinery: (None,
/// note) → annotate + pointer restore + auto-feed suppressed (a
/// still-Pending item means no plan ran — a re-dispatch would loop on the
/// same item).
/// (backlog bba2c82d) The Failed flip is guarded like the Done flip: only a
/// currently-InFlight item whose plan_id IS the abandoned plan may flip — a
/// re-queued (Pending) item must stay Pending with its note preserved (the
/// re-queue reason is recovery evidence). The blocked flip is (None, None):
/// no transition, NO annotate — the wipe damage is `transition`'s (it
/// replaces the note); `annotate` appends, but the recovery evidence stays
/// unpolluted either way (the deliberate asymmetry with the Done blocked
/// row, which annotates a transient note).
async fn single_dispatch_disposition(
    state: &IpcState,
    in_flight_id: &str,
    success: bool,
    error: Option<String>,
    loop_evidence: bool,
    plan_abandoned: bool,
    abandoned_plan_id: Option<&str>,
) -> (Option<BacklogStatus>, Option<String>) {
    let may_flip_done = {
        let completed_plan = main_agent_top_plan_id(state).await;
        let store = state.backlog.store.lock().await;
        store
            .items()
            .iter()
            .find(|i| i.id == in_flight_id)
            .map(|i| {
                plan_linkage_allows_done(
                    i.status,
                    i.plan_id.as_deref(),
                    completed_plan.as_deref(),
                )
            })
            .unwrap_or(false)
    };
    let may_flip_failed = {
        let store = state.backlog.store.lock().await;
        store
            .items()
            .iter()
            .find(|i| i.id == in_flight_id)
            .map(|i| {
                plan_linkage_allows_failed(
                    i.status,
                    i.plan_id.as_deref(),
                    abandoned_plan_id,
                )
            })
            .unwrap_or(false)
    };
    if success {
        match (main_agent_workflow_state(state).await, loop_evidence) {
            (Some(main_state), true)
                if plan_loop_allows_done(main_state, true) && may_flip_done =>
            {
                (Some(BacklogStatus::Done), None)
            }
            (Some(main_state), true) if plan_loop_allows_done(main_state, true) => {
                // The plan loop closed but the item does not own the
                // completing plan — keep the item (status + note
                // preserved), no transition.
                (
                    None,
                    Some("completing plan is not this item's plan — item kept, no transition".to_string()),
                )
            }
            // The root plan was abandoned this turn and no successor
            // plan closed the loop — the ONE true failure (backlog
            // 45dcf577): failure means exactly "the plan was
            // abandoned". Guarded on status + linkage (backlog
            // bba2c82d).
            _ if plan_abandoned && may_flip_failed => (
                Some(BacklogStatus::Failed),
                Some("plan abandoned (abandon_plan)".to_string()),
            ),
            _ if plan_abandoned => {
                // (backlog bba2c82d) The abandoned plan is not this
                // item's — a stale pointer referencing a re-queued
                // (Pending) item while an UNRELATED plan ran. No
                // transition, no annotate (the note is preserved);
                // the pointer restore below is the recovery.
                eprintln!(
                    "backlog: abandoned plan is not this item's plan — item kept, no transition (item {in_flight_id})"
                );
                (None, None)
            }
            (main_state, _) => (None, Some(plan_open_note(main_state))),
        }
    } else if plan_abandoned && may_flip_failed {
        (
            Some(BacklogStatus::Failed),
            Some("plan abandoned (abandon_plan)".to_string()),
        )
    } else if plan_abandoned {
        // (backlog bba2c82d) The blocked flip: no transition, no
        // annotate (the re-queue note is preserved).
        eprintln!(
            "backlog: abandoned plan is not this item's plan — item kept, no transition (item {in_flight_id})"
        );
        (None, None)
    } else {
        (
            None,
            Some(error.unwrap_or_else(|| "turn failed".to_string())),
        )
    }
}

/// The outcome of resolving the run-all in-flight item against a finished
/// main-agent turn: [`RunAllItemDisposition::Waited`] means the run waits
/// (the arm already annotated + emitted — the caller returns without
/// touching the pointer or dispatching); [`RunAllItemDisposition::Proceed`]
/// means the caller proceeds to the done-counter/pointer block, the stopped
/// check, and the next dispatch, with `terminal_resolution` gating the
/// done-counter bump (backlog 5bb1e4cd: a BLOCKED flip must not count done).
enum RunAllItemDisposition {
    /// The run waits — the arm annotated + emitted; the caller returns.
    Waited,
    /// The caller proceeds; `terminal_resolution` gates the done bump.
    Proceed { terminal_resolution: bool },
}

/// The Done arm of the run-all success disposition (backlog 5bb1e4cd): only
/// a still-InFlight item that owns the completing plan may flip to Done — a
/// re-queued (Pending) item or a mismatched plan_id must NOT transition (the
/// note would be wiped) and must NOT commit under the item's name. For a
/// closed_earlier resolution the landed predicate already proved the item's
/// OWN plan completed, so only the status guard applies (the drain's
/// done-orphan guard uses exactly that evidence, with no linkage check).
/// Returns whether the item transitioned; a blocked flip leaves a record
/// (review LOW-2, 2026-09-08) and returns false — as does a transition
/// refused because the exit drain's Done flip won the race in the
/// commit_success window (the continuation runs spawned off the
/// forwarder, review L2; round-2 LOW-1: the done counter must count the
/// item exactly once).
async fn flip_done_if_linked(
    state: &IpcState,
    item: &mnemo::backlog::BacklogItem,
    root: std::path::PathBuf,
    closed_earlier: bool,
) -> bool {
    let completed_plan = main_agent_top_plan_id(state).await;
    let may_flip = {
        let store = state.backlog.store.lock().await;
        store
            .items()
            .iter()
            .find(|i| i.id == item.id)
            .map(|i| {
                i.status == BacklogStatus::InFlight
                    && (closed_earlier
                        || plan_linkage_allows_done(
                            i.status,
                            i.plan_id.as_deref(),
                            completed_plan.as_deref(),
                        ))
            })
            .unwrap_or(false)
    };
    if may_flip {
        if let Err(e) = commit_success(root.clone(), item.clone()).await {
            eprintln!("backlog: commit failed for item {}: {e}", item.id);
        }
        // The transition's OWN result is the return value (review round-2
        // LOW-1): the may_flip pre-check above can be raced by the exit
        // drain's Done flip in the commit_success window — a refused
        // transition must return false so the done counter counts the
        // item exactly once (the drain's gated bump already counted it).
        state.backlog.store.lock().await.transition(
            &item.id,
            BacklogStatus::Done,
            None,
        )
    } else {
        // (review LOW-2, 2026-09-08) Mirror the single-dispatch arm's blocked
        // row: leave a record that a completing turn skipped this item. The
        // pointer clear + next dispatch below are the observable recovery,
        // but the item itself should say why. Append-safe
        // (`extract_checkpoint_sha` head-parses; a re-dispatch's set_note
        // replaces the note anyway).
        state.backlog.store.lock().await.annotate(
            &item.id,
            "completing plan is not this item's plan — item kept, no transition",
        );
        false
    }
}

/// The ONE true failure (backlog 45dcf577): the root plan was abandoned this
/// turn (`abandon_plan`, observed as an Executing/Reviewing → Planning
/// transition) and no successor plan closed the loop — failure means exactly
/// "the plan was abandoned". Terminal: the run may continue with the next
/// item. Guarded on status + linkage (backlog bba2c82d): only a
/// currently-InFlight item whose plan_id IS the abandoned plan may flip — a
/// stale pointer referencing a re-queued (Pending) item must stay Pending
/// with its note preserved. Shared by the run-all success match and the
/// failure path (the two sites were byte-identical). Returns whether the
/// item transitioned.
async fn flip_failed_if_abandonment_linked(
    state: &IpcState,
    item: &mnemo::backlog::BacklogItem,
    abandoned_plan_id: Option<&str>,
) -> bool {
    let may_flip = {
        let store = state.backlog.store.lock().await;
        store
            .items()
            .iter()
            .find(|i| i.id == item.id)
            .map(|i| {
                plan_linkage_allows_failed(
                    i.status,
                    i.plan_id.as_deref(),
                    abandoned_plan_id,
                )
            })
            .unwrap_or(false)
    };
    if may_flip {
        state.backlog.store.lock().await.transition(
            &item.id,
            BacklogStatus::Failed,
            Some("plan abandoned (abandon_plan)".to_string()),
        );
        true
    } else {
        // The blocked flip: no transition, NO annotate. The wipe damage is
        // `transition`'s (it replaces the note); `annotate` appends — but
        // the re-queue note is recovery evidence and stays unpolluted either
        // way. The pointer clear + next dispatch below are the observable
        // recovery.
        eprintln!(
            "backlog: abandoned plan is not this item's plan — item kept, no transition (item {})",
            item.id
        );
        false
    }
}

/// The slid-resolution + re-queued-item recovery (backlog 8a6bcece, review
/// LOW-3 2027-01-08): the item was externally re-queued to Pending mid-run
/// (the 5bb1e4cd-style backlog.jsonl edit — the store sees Pending; the
/// in-memory current_item pointer still references it) AND the completing
/// turn's resolution slid (the e33a07fd slide) — this turn lands at
/// (Complete, false) with the item Pending: closed_earlier is false (it
/// requires InFlight) and the waiting arm would stall the run forever
/// (Complete is a resting state; nothing ever re-checks). Recovery: LANDED
/// evidence (the re-queued item's OWN plan completed and committed —
/// orphan_work_landed, the drain's done-orphan evidence) resolves Done
/// exactly like the drain's done-orphan guard — the work is done;
/// re-dispatching would redo landed work (the 6c6966b9 class). A Pending
/// item with NO plan_id never resolves Done (the 2026-08-20 hole stays
/// plugged — the predicate requires a plan_id; anything unverifiable is
/// false). Otherwise the pointer is stale — the item is already Pending
/// (eligible for re-dispatch); the caller's pointer clear + dispatch next
/// are the recovery. No annotate (the re-queue note is recovery evidence —
/// the bba2c82d discipline). Returns whether the item transitioned.
async fn resolve_slid_pending_item(
    state: &IpcState,
    item: &mnemo::backlog::BacklogItem,
) -> bool {
    // Snapshot note + plan id BEFORE any transition (the transition
    // overwrites the note; the checkpoint sha must survive).
    let (note, plan_id) = {
        let store = state.backlog.store.lock().await;
        match store.items().iter().find(|i| i.id == item.id) {
            Some(i) => (i.note.clone(), i.plan_id.clone()),
            None => (None, None),
        }
    };
    // Consult landed-work evidence WITHOUT the store lock (the git check is
    // a blocking subprocess), then re-verify the item is still Pending under
    // the lock (a racing external edit may have moved it — only a
    // still-Pending item flips Done).
    let landed = orphan_work_landed(state, plan_id.as_deref(), note.as_deref()).await;
    let still_pending = {
        let store = state.backlog.store.lock().await;
        store
            .items()
            .iter()
            .any(|i| i.id == item.id && i.status == BacklogStatus::Pending)
    };
    if landed && still_pending {
        let suffix = "work already landed (plan complete + commits after the checkpoint) — auto-resolved done, not re-dispatched";
        let full = match note {
            Some(n) => format!("{n} | {suffix}"),
            None => suffix.to_string(),
        };
        state
            .backlog
            .store
            .lock()
            .await
            .transition(&item.id, BacklogStatus::Done, Some(full));
        true
    } else {
        // The pointer is stale — the item is Pending (eligible for
        // re-dispatch); the tail's pointer clear + dispatch next are the
        // recovery.
        eprintln!(
            "backlog: slid resolution with a re-queued (Pending) item — stale pointer cleared, item re-queued for dispatch (item {})",
            item.id
        );
        false
    }
}

/// The waiting arm of the run-all disposition: any other turn end is NOT a
/// failure (backlog 45dcf577) — the item keeps its status (`InFlight` while
/// its plan is active — the plan persists; a resumed session continues it —
/// `Pending` when no plan ever ran), the work is NOT rolled back, and the
/// run WAITS (the amplifier fix, 2027-01-07): the agent routinely recovers
/// on its own — the auto-continue resumes mid-plan turn ends,
/// reviewer-finish notifications resume the closing sequence — and the plan
/// closes on a later turn. Ending the run here destroyed the current_item
/// pointer the completing turn's resolution needed (the item stranded
/// in_flight, the next item never dispatched — live twice: a6a7727a,
/// 207dc316). No dispatch happens without a successful resolution; the run
/// ends when the item resolves (the caller's stopped check, for an
/// intervention-stopped run) or the agent exits (drain).
async fn annotate_run_all_waiting(
    app: &tauri::AppHandle,
    state: &IpcState,
    item: &mnemo::backlog::BacklogItem,
    main_state: Option<WorkflowState>,
) {
    let note = plan_open_note(main_state);
    // annotate APPENDS — skip only an EXACT repeat: the same note text (the
    // common case — consecutive same-state auto-continue boundaries) is
    // suppressed so it doesn't grow unboundedly, while a different flavor (a
    // workflow-state progression, a later abort reason) still annotates and
    // the information is never lost (review LOW-3).
    let already_waiting = {
        let store = state.backlog.store.lock().await;
        store
            .items()
            .iter()
            .find(|i| i.id == item.id)
            .and_then(|i| i.note.as_deref())
            .map_or(false, |n| n.contains(note.as_str()))
    };
    if !already_waiting {
        state
            .backlog
            .store
            .lock()
            .await
            .annotate(&item.id, &note);
    }
    emit_backlog_changed(app, state).await;
}

/// The terminal-error arm of the run-all failure disposition: a terminal
/// Error (provider exhaustion, consecutive tool errors, crash) is NOT an
/// abandonment (backlog 45dcf577) — no transition, no rollback: the item
/// keeps its status, the work stays in the tree, and the run WAITS (the
/// amplifier fix, 2027-01-07): the auto-continue routinely resumes the agent
/// after an abort and the plan closes on a later turn — ending the run here
/// destroyed the pointer the completing turn's resolution needed (live:
/// a6a7727a — aborted on 3 consecutive tool errors, finished cleanly,
/// resolution blind). The next turn resolution re-checks.
async fn annotate_run_all_error(
    app: &tauri::AppHandle,
    state: &IpcState,
    item: &mnemo::backlog::BacklogItem,
    error: Option<String>,
) {
    let note = error.unwrap_or_else(|| "turn failed".to_string());
    let note = format!("{note} — item left in flight; work kept in tree; run waits for the resumed turn (the next turn resolution re-checks)");
    // annotate APPENDS — skip only an EXACT repeat (the same abort text); a
    // different error still annotates so the reason is never lost (review
    // LOW-3).
    let already_waiting = {
        let store = state.backlog.store.lock().await;
        store
            .items()
            .iter()
            .find(|i| i.id == item.id)
            .and_then(|i| i.note.as_deref())
            .map_or(false, |n| n.contains(note.as_str()))
    };
    if !already_waiting {
        state
            .backlog
            .store
            .lock()
            .await
            .annotate(&item.id, &note);
    }
    emit_backlog_changed(app, state).await;
}

/// The success disposition of the run-all in-flight item. The plan-loop gate
/// is MANDATORY (not a mode): a backlog item is only ever marked Done when
/// the main agent's plan loop fully closed — workflow == Complete AND at
/// least one workflow state transition observed (Complete is a resting
/// state; a turn that never planned ends there too — user-reported hole,
/// 2026-08-20). The evidence may come from an EARLIER turn (backlog
/// e33a07fd, live 2027-01-08): the resolution can slide past the closing
/// turn — the finish turn's resolution is deferred (a descendant still
/// running), then DISCARDED when the descendant's completion notification
/// starts a new turn (on_started clears pending_finished) — and the
/// notification turn observes no transition, landing at (Complete, false).
/// Complete is a resting state: nothing ever re-checks, so the run stalls
/// with the item's work landed (the next item never dispatched).
/// `closed_earlier` recovers exactly that case: the landed predicate (the
/// drain's done-orphan evidence) proves the item's OWN plan completed —
/// plan file all-checked + a commit after the pre-item checkpoint. A turn
/// that never planned has no plan_id → the predicate is false → the
/// non-closure arm below still waits (the 2026-08-20 hole stays plugged).
async fn run_all_success_disposition(
    app: &tauri::AppHandle,
    state: &IpcState,
    item: &mnemo::backlog::BacklogItem,
    root: std::path::PathBuf,
    loop_evidence: bool,
    plan_abandoned: bool,
    abandoned_plan_id: Option<&str>,
) -> RunAllItemDisposition {
    let main_state_opt = main_agent_workflow_state(state).await;
    let closed_earlier = main_state_opt == Some(WorkflowState::Complete)
        && !loop_evidence
        && item.status == BacklogStatus::InFlight
        && orphan_work_landed(state, item.plan_id.as_deref(), item.note.as_deref()).await;
    match (main_state_opt, loop_evidence || closed_earlier) {
        (Some(main_state), true) if plan_loop_allows_done(main_state, true) => {
            if flip_done_if_linked(state, item, root, closed_earlier).await {
                RunAllItemDisposition::Proceed {
                    terminal_resolution: true,
                }
            } else {
                RunAllItemDisposition::Proceed {
                    terminal_resolution: false,
                }
            }
        }
        // The root plan was abandoned this turn (`abandon_plan`, observed as
        // an Executing/Reviewing → Planning transition) and no successor
        // plan closed the loop — the ONE true failure (backlog 45dcf577):
        // failure means exactly "the plan was abandoned". Terminal: the run
        // may continue with the next item. Guarded on status + linkage
        // (backlog bba2c82d) — see flip_failed_if_abandonment_linked.
        _ if plan_abandoned => {
            if flip_failed_if_abandonment_linked(state, item, abandoned_plan_id).await {
                RunAllItemDisposition::Proceed {
                    terminal_resolution: true,
                }
            } else {
                RunAllItemDisposition::Proceed {
                    terminal_resolution: false,
                }
            }
        }
        // (backlog 8a6bcece, review LOW-3 2027-01-08) The slid-resolution +
        // re-queued-item combination — see resolve_slid_pending_item.
        (Some(WorkflowState::Complete), false) if item.status == BacklogStatus::Pending => {
            if resolve_slid_pending_item(state, item).await {
                RunAllItemDisposition::Proceed {
                    terminal_resolution: true,
                }
            } else {
                RunAllItemDisposition::Proceed {
                    terminal_resolution: false,
                }
            }
        }
        // Any other turn end is NOT a failure (backlog 45dcf577) — see
        // annotate_run_all_waiting: the item keeps its status, the run WAITS.
        (main_state, _) => {
            annotate_run_all_waiting(app, state, item, main_state).await;
            RunAllItemDisposition::Waited
        }
    }
}

/// The failure disposition of the run-all in-flight item: an abandoned root
/// plan is the ONE failure (the shared linkage-guarded flip — see
/// [`flip_failed_if_abandonment_linked`]); any other terminal error is NOT —
/// the item keeps its status and the run WAITS (see
/// [`annotate_run_all_error`]).
async fn run_all_failure_disposition(
    app: &tauri::AppHandle,
    state: &IpcState,
    item: &mnemo::backlog::BacklogItem,
    plan_abandoned: bool,
    abandoned_plan_id: Option<&str>,
    error: Option<String>,
) -> RunAllItemDisposition {
    if plan_abandoned {
        // A failed turn whose root plan was abandoned: the abandonment is
        // the disposition (backlog 45dcf577) — the plan is gone; the error
        // is moot. Same linkage-guarded flip as the success-path arm.
        if flip_failed_if_abandonment_linked(state, item, abandoned_plan_id).await {
            RunAllItemDisposition::Proceed {
                terminal_resolution: true,
            }
        } else {
            RunAllItemDisposition::Proceed {
                terminal_resolution: false,
            }
        }
    } else {
        annotate_run_all_error(app, state, item, error).await;
        RunAllItemDisposition::Waited
    }
}

/// Resolve the run-all in-flight item against a finished main-agent turn,
/// then keep the run moving: the stopped check ends an intervention-stopped
/// run (only when no lane is in flight), the emit refreshes the UI, and the
/// between-items auto-compact (or the direct next dispatch) advances the
/// run. The per-outcome dispositions live in
/// [`run_all_success_disposition`] / [`run_all_failure_disposition`].
async fn resolve_run_all_turn(
    app: &tauri::AppHandle,
    state: &IpcState,
    success: bool,
    error: Option<String>,
    loop_evidence: bool,
    plan_abandoned: bool,
    abandoned_plan_id: Option<&str>,
) {
    // The in-flight item's id + stop flag (the checkpoint sha is stashed
    // in the item's note).
    let (item_id, stopped) = {
        let guard = state.backlog.run_all.lock().await;
        match guard.as_ref() {
            Some(r) => (
                r.current_item
                    .lock()
                    .expect("current_item lock poisoned")
                    .clone(),
                r.stop.load(Ordering::Relaxed),
            ),
            None => (None, true),
        }
    };
    // Read the item (for its text + checkpoint note) before mutating.
    let item = match &item_id {
        Some(id) => state
            .backlog
            .store
            .lock()
            .await
            .items()
            .iter()
            .find(|i| &i.id == id)
            .cloned(),
        None => None,
    };

    let mut item_resolved = false;
    if let Some(item) = item {
        let root = state.project.root.lock().await.root.clone();
        let disposition = if success {
            run_all_success_disposition(
                app,
                state,
                &item,
                root,
                loop_evidence,
                plan_abandoned,
                abandoned_plan_id,
            )
            .await
        } else {
            run_all_failure_disposition(
                app,
                state,
                &item,
                plan_abandoned,
                abandoned_plan_id,
                error,
            )
            .await
        };
        // (backlog 5bb1e4cd) Only arms that actually transitioned the item
        // count done — a BLOCKED Done flip must not count the item done (the
        // done-counter bump below is gated on it).
        let terminal_resolution = match disposition {
            RunAllItemDisposition::Waited => return,
            RunAllItemDisposition::Proceed { terminal_resolution } => terminal_resolution,
        };
        // Bump the done counter — only for an arm that actually
        // transitioned the item (backlog 5bb1e4cd): a BLOCKED Done flip
        // (a re-queued item, a plan the item never owned) must not
        // count done, but the stale in-flight pointer must still be
        // cleared so the dispatch below is not resolved against the
        // wrong item (the re-queued item is Pending again — eligible
        // for re-dispatch).
        {
            let mut guard = state.backlog.run_all.lock().await;
            if let Some(r) = guard.as_mut() {
                if terminal_resolution {
                    r.done.fetch_add(1, Ordering::Relaxed);
                }
                // LOW-3 (review): the item is terminally resolved — clear the
                // in-flight pointer NOW, before the (potentially minutes-long)
                // between-items auto-compact window, so an interactive chat
                // turn during the window cannot resolve against the
                // already-completed item (annotate-and-halt on the Done item,
                // or a double Done transition + done-counter overshoot). The
                // None state is already handled gracefully by the resolution
                // path (the dispatch-deferral path uses exactly this).
                *r.current_item.lock().expect("current_item lock poisoned") = None;
            }
        }
        item_resolved = terminal_resolution;
    }

    if stopped {
        // Parallel run-all (plan ffd7a86f, review H1): stop ends the
        // run only when no lane is still in flight — the spawned
        // sibling's equivalent check (on_spawned_turn_resolved)
        // matches. In-flight lanes' resolutions end the run when the
        // last one lands.
        if !any_lane_in_flight(state).await {
            end_run(app, state).await;
        }
        return;
    }
    emit_backlog_changed(app, state).await;
    // Between-items auto-compact (Run-All only, opt-in via
    // `auto_compact_on_plan_complete`): compact the main agent's context
    // before the next item so every item starts with a clean summarized
    // context instead of exhausting the window mid-item overnight.
    // MUST be spawned, not awaited inline: this path runs inside the
    // event forwarder, and the `Compacted` event the wait needs flows
    // through that same forwarder — awaiting here would deadlock the
    // loop on its own output. Interactive completions never reach this
    // branch (Run-All only), and `item_resolved` pins it to exactly one
    // compaction per resolved item — an interactive turn during the
    // window (current_item already cleared) dispatches directly without
    // re-compacting.
    let auto_compact = item_resolved
        && state
            .project
            .config
            .lock()
            .await
            .general
            .general
            .auto_compact_on_plan_complete;
    if auto_compact {
        tokio::spawn(compact_then_dispatch_next(app.clone()));
    } else if let Err(e) = run_all_dispatch_next(app, state).await {
        // A9: log the dispatch error instead of silently swallowing it
        // (the no-main-agent path inside dispatch_next already clears
        // the run and returns Err; this surfaces every dispatch failure).
        run_all_diag(&format!("failed to dispatch next item: {e}"));
    }
}

/// Halt an active Run-All loop — the user's intervention (an approval request
/// or a mid-run steer) is never auto-resolved; the run stops and waits for the
/// user.
///
/// IDENTIFICATION (Phase 3b): This is a primary "halt" site.
/// - The checkpoint sha for the current item lives in the backlog item's `note`
///   (set at dispatch time in run_all_dispatch_next via `set_note`).
/// - To "preserve checkpoint sha on Run-All halt", the note is annotated with
///   the reason (the sha stays at its head) — so a later manual resume or
///   rollback, or the intervention requeue, can still find it.
/// - Approval halts (`stamp_failed = true`) stamp NOTHING (backlog 45dcf577:
///   a halt is not a plan abandonment — the one true `Failed`). The run state
///   is deliberately KEPT: the agent's turn is still live (blocked on the
///   approval), so the post-approval turn resolution still sees the item and
///   resolves it under the plan-tied rules; the `stop` flag prevents the next
///   dispatch. The steer arm (`stamp_failed = false`) annotates, hands the
///   item id to the intervention latch, and KEEPS the run stopped (see
///   [`handle_user_intervention`]) — the same kept-run mirror, so the
///   resumed turn's resolution still sees the item.
///
/// `reason` is a short phrase describing why the run halted (e.g. "approval
/// requested during unattended run", "steer received during unattended run"),
/// surfaced in the eprintln log + the item's note. `stamp_failed` selects the
/// halt's disposition: `true` (approval halt) annotates the note and returns
/// WITHOUT ending the run — the kept run state is what lets the later turn
/// resolution resolve the item (run-all items never enter
/// `single_in_flight`). `false` (steer/interrupt halt) is NOT a task failure:
/// the status is left untouched, the note is annotated (checkpoint sha
/// preserved), and the item id is handed to the intervention latch — the
/// soft-stop turn's resolution (`handle_user_intervention`) keeps the item
/// InFlight and stops the run (backlog b83e891f). The run state is KEPT in
/// both arms (the stop flag prevents the next dispatch): the kept
/// `current_item` pointer is what lets the resumed turn's resolution
/// resolve the item. No-op when Run-All is inactive, so callers can invoke
/// unconditionally.
pub async fn halt_run_all(app: &tauri::AppHandle, reason: &str, stamp_failed: bool) {
    let state = app.state::<IpcState>();
    let guard = state.backlog.run_all.lock().await;
    if let Some(r) = guard.as_ref() {
        r.stop.store(true, Ordering::Relaxed);
        eprintln!("backlog: run-all halted — {reason}");
        // Mark the in-flight item so it's clear why the run stopped.
        let item_id = r
            .current_item
            .lock()
            .expect("current_item lock poisoned")
            .clone();
        // SNAPSHOT: read the current note (contains the checkpoint sha) so we
        // can preserve it for clean resume/rollback after the user resolves
        // the intervention. Do not lose it when setting the halted status.
        let current_note = match &item_id {
            Some(id) => state
                .backlog
                .store
                .lock()
                .await
                .items()
                .iter()
                .find(|i| &i.id == id)
                .and_then(|i| i.note.clone()),
            None => None,
        };
        let already_halted = current_note
            .as_deref()
            .map_or(false, |n| n.contains("halted"));
        drop(guard);
        let Some(item_id) = &item_id else {
            // No in-flight item to mark, but still clear the run state
            // so the run is not left "stopped but not cleared" (the `stop`
            // flag was set above; without `end_run` the dispatcher would stay
            // `Some` until a later `Finished` drains it). Reachable from the
            // steer path when the agent is idle between items.
            // Parallel run-all (plan ffd7a86f, review H1): only when no
            // spawned lane is still working — their resolutions end the
            // run when the last one lands (the stop flag is already set).
            if !any_lane_in_flight(&state).await {
                end_run(app, &state).await;
            }
            return;
        };
        if stamp_failed {
            // Approval halt (backlog 45dcf577): the run stops and waits for
            // the user, but the item is NOT stamped — a halt-for-approval is
            // not a plan abandonment (the one true `Failed`). The reason is
            // recorded in the note (the checkpoint sha stays at its head;
            // annotate appends). The run state is deliberately KEPT: the
            // agent's turn is still live (blocked on the approval), so the
            // post-approval turn resolution still sees `current_item` and
            // resolves the item under the plan-tied rules (gate → Done,
            // abandoned → Failed, still open → stays InFlight + the run
            // waits — the next turn resolution re-checks). The
            // `stop` flag (set above) prevents the next dispatch.
            if !already_halted {
                state
                    .backlog
                    .store
                    .lock()
                    .await
                    .annotate(item_id, &format!("{reason} — halted"));
            }
            return;
        } else {
            // Steer/interrupt halt: record the reason in the note (the
            // checkpoint sha stays at its head; annotate appends) and hand
            // the item id to the intervention latch — the status itself is
            // decided by `handle_user_intervention` at turn resolution.
            if !already_halted {
                state
                    .backlog
                    .store
                    .lock()
                    .await
                    .annotate(item_id, &format!("{reason} — halted"));
            }
            let mut latch = state
                .backlog
                .user_intervention
                .lock()
                .expect("user_intervention lock poisoned");
            if let Some(iv) = latch.as_mut() {
                if iv.item_id.is_none() {
                    iv.item_id = Some(item_id.clone());
                }
            }
        }
        // The run state is deliberately KEPT (mirroring the approval arm
        // above, backlog b83e891f): the stop flag set at the top prevents
        // the next dispatch, and the kept `current_item` pointer lets the
        // resumed turn's resolution resolve the item under the plan-tied
        // rules — ending the run here destroyed the pointer and stranded
        // the item (the completing turn's resolution was blind).
    }
}

/// Whether every step of the plan file at `path` is checked — the
/// plan-side evidence of the run-all done-orphan guard (backlog
/// 6c6966b9). `false` when the file is missing, unparseable, or has no
/// steps: an unverifiable plan is NOT evidence of landed work (the safe
/// default keeps the requeue).
fn plan_steps_all_done(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(plan) = PlanFile::parse(&text) else {
        return false;
    };
    !plan.steps.is_empty() && plan.steps.iter().all(|s| s.done)
}

/// Landed-work evidence for an orphaned `InFlight` item (backlog
/// 6c6966b9): a session that dies between its closing-sequence commit and
/// `finish` leaves the item looking unfinished — only `finish` marks an
/// item done — and requeueing it re-executes the landed task as
/// duplicate work. The work already landed when BOTH hold:
///
/// 1. **Plan side:** the item's root plan file
///    (`.coding/plans/<plan_id>.md`) exists and every step is checked
///    ([`plan_steps_all_done`]) — the dead session verifiably completed
///    the plan's steps (a still-active sub-plan leaves root steps
///    unchecked, so a fully-checked root means the whole tree completed).
/// 2. **Git side:** at least one commit exists on the branch after the
///    pre-item checkpoint sha (the note head, [`extract_checkpoint_sha`])
///    — [`commits_after_checkpoint`]. During an item's flight its session
///    is the only committer (the next item's checkpoint runs only after
///    this one resolves), so any commit in `sha..HEAD` is that item's
///    work. Deliberately NOT "commits referencing the item id": agent
///    commits don't reliably carry the id, and `commit_success` is a
///    no-op on a clean tree — precisely the died-after-commit case. The
///    commit anchored by the per-item checkpoint IS the "work landed"
///    marker: the death window sits between the agent's own commit and
///    its `finish` call, with no harness-controlled point inside it to
///    stamp a separate one.
///
/// Anything unverifiable (no plan id, missing/incomplete plan, no
/// checkpoint sha, git error) is `false` — the safe default keeps the
/// requeue, never auto-done.
pub(crate) async fn orphan_work_landed(
    state: &IpcState,
    plan_id: Option<&str>,
    note: Option<&str>,
) -> bool {
    // Clone the root BEFORE the git call — never hold the project-root
    // lock across a blocking subprocess.
    let root = state.project.root.lock().await.root.clone();
    let plans_dir = root.join(".coding").join("plans");
    orphan_work_landed_at(plans_dir, root, plan_id, note).await
}

/// The landed-work predicate over explicit paths (plan ffd7a86f): the
/// main-tree form above, and the WORKTREE form for spawned run-all items.
/// `plans_dir` differs between them — the main agent's plans live at
/// `<root>/.coding/plans/`, a spawned agent's at
/// `<worktree>/.coding/plans/agents/<agent_id>/` (see
/// [`spawned_plans_dir`]; review H3 — reading the top-level plans dir for
/// a spawned item made the predicate always-false, breaking the
/// closed-earlier recovery and the exit-drain's landed guard).
pub(crate) async fn orphan_work_landed_at(
    plans_dir: std::path::PathBuf,
    root: std::path::PathBuf,
    plan_id: Option<&str>,
    note: Option<&str>,
) -> bool {
    let Some(plan_id) = plan_id else {
        return false;
    };
    let plan_path = plans_dir.join(format!("{plan_id}.md"));
    if !plan_steps_all_done(&plan_path) {
        return false;
    }
    let Some(sha) = note.and_then(extract_checkpoint_sha) else {
        return false;
    };
    matches!(commits_after_checkpoint(root, sha).await, Ok(true))
}

/// A spawned run-all agent's plans dir (plan ffd7a86f): the worktree's
/// `.coding/plans/agents/<agent_id>/` — the same own-plans-dir convention
/// as UI-spawned parentless agents (spawn.rs). The landed predicate and
/// the in-flight title lookup must read plan files from HERE, not the
/// worktree's top-level plans dir (which only the main agent uses).
fn spawned_plans_dir(
    worktree: &std::path::Path,
    agent_id: mnemo::runtime::AgentId,
) -> std::path::PathBuf {
    worktree
        .join(".coding")
        .join("plans")
        .join("agents")
        .join(agent_id.to_string())
}

/// Drain a stranded Run-All when the MAIN agent exits (backlog 45dcf577,
/// review LOW-2): a cancel or crash while the run is active — e.g. blocked
/// on an approval halt, whose deliberately-kept run state expects a later
/// turn resolution — bypasses every designed drain path: nothing resolves
/// the item and nothing clears `run_all`, leaving "run-all is already
/// active", a stuck UI, and an `InFlight` item with no UI recovery. The
/// item is REQUEUED to `Pending` with the checkpoint sha preserved (run-all
/// orphans, 2027-01-07): a dead agent's turn resolution never comes, and
/// leaving it `InFlight` stranded it forever — dispatch only considers
/// `Pending` and the guarded requeue refuses `InFlight`. The queue is the
/// recovery; the work stays in the tree and the next dispatch builds on
/// it.
///
/// Done-orphan guard (backlog 6c6966b9): BEFORE requeueing, consult
/// [`orphan_work_landed`] — a session that died between its
/// closing-sequence commit and `finish` left the work fully landed, and
/// requeueing it re-executes the task as duplicate work. Landed orphans
/// auto-resolve `Done` with an explanatory note instead. No-op when no
/// run is active.
pub(crate) async fn drain_run_all_on_main_exit(app: &tauri::AppHandle) {
    let state = app.state::<IpcState>();
    let item_id = {
        let guard = state.backlog.run_all.lock().await;
        match guard.as_ref() {
            Some(r) => r
                .current_item
                .lock()
                .expect("current_item lock poisoned")
                .clone(),
            None => return,
        }
    };
    let mut main_done_bump = false;
    if let Some(id) = item_id {
        // Snapshot note + plan id BEFORE the transition — transition
        // overwrites the note, and the checkpoint sha must survive (the
        // manual resume/rollback anchor).
        let (note, plan_id) = {
            let store = state.backlog.store.lock().await;
            match store.items().iter().find(|i| i.id == id) {
                Some(item) => (item.note.clone(), item.plan_id.clone()),
                None => (None, None),
            }
        };
        // Consult landed-work evidence WITHOUT the store lock (the git
        // check is a blocking subprocess — holding the lock would block
        // every backlog read), then re-verify under the lock before
        // transitioning.
        let landed = orphan_work_landed(&state, plan_id.as_deref(), note.as_deref()).await;
        let mut store = state.backlog.store.lock().await;
        // Re-verify the item is still InFlight: a concurrent resolution
        // (the turn resolving as the agent exits) may have transitioned
        // it already — never overwrite that.
        let still_in_flight = store
            .items()
            .iter()
            .any(|i| i.id == id && i.status == BacklogStatus::InFlight);
        if still_in_flight {
            if landed {
                // The dead session's work already landed (plan complete +
                // commits after the checkpoint) — auto-resolve Done instead
                // of requeueing for duplicate re-dispatch.
                let suffix = "work already landed (plan complete + commits after the checkpoint) — auto-resolved done, not re-dispatched";
                let full = match note {
                    Some(n) => format!("{n} | {suffix}"),
                    None => suffix.to_string(),
                };
                main_done_bump = store.transition(&id, BacklogStatus::Done, Some(full));
            } else {
                let suffix =
                    "main agent exited while the run was active — returned to queue; work kept in tree";
                let full = match note {
                    Some(n) => format!("{n} | {suffix}"),
                    None => suffix.to_string(),
                };
                store.transition(&id, BacklogStatus::Pending, Some(full));
            }
        }
    }
    // Symmetric with the resolution paths (on_spawned_turn_resolved /
    // resolve_run_all_turn): the drain's OWN successful Done transition
    // bumps the run's done counter — the delayed continuation (spawned off
    // the forwarder, review L2) finds the row already Done and bumps
    // nothing (review round-1 LOW-1). Deferred past the store guard's scope
    // above to keep the lock order (never run_all under store).
    if main_done_bump {
        let guard = state.backlog.run_all.lock().await;
        if let Some(r) = guard.as_ref() {
            r.done.fetch_add(1, Ordering::Relaxed);
        }
    }
    // Parallel run-all (plan ffd7a86f, review R2): the main lane is gone —
    // clear the pointer BEFORE the wind-down decision. A stale `Some`
    // keeps every end-of-run check true forever (the wind-down can never
    // terminate: the UI shows the run active, new runs are refused, and
    // halt_run_all's annotate-and-keep arms never reach the no-item
    // end_run).
    {
        let guard = state.backlog.run_all.lock().await;
        if let Some(r) = guard.as_ref() {
            *r.current_item.lock().expect("current_item lock poisoned") = None;
        }
    }
    // Parallel run-all (plan ffd7a86f, review H1): the main agent's exit
    // must not end the run while spawned lanes are still working — they
    // are independent parentless agents whose resolutions need the run
    // state. But the main lane is gone (and `main_agent_id()` would now
    // resolve to a spawned agent — mis-dispatching main-lane items into a
    // worktree), so the run WINDS DOWN: set the stop flag; each lane's
    // resolution ends the run when the last one lands.
    let spawned_in_flight = {
        let guard = state.backlog.run_all.lock().await;
        match guard.as_ref() {
            Some(r) => {
                if r.spawned.lock().expect("spawned lock poisoned").is_empty() {
                    false
                } else {
                    r.stop.store(true, Ordering::Relaxed);
                    true
                }
            }
            None => false,
        }
    };
    if !spawned_in_flight {
        end_run(app, &state).await;
    }
}

/// Read the main agent's active root plan id — the adoption ownership
/// check (see [`adopt_orphaned_in_flight`]). Returns `None` when no main
/// agent is registered or its agent loop is missing (e.g. during
/// teardown): no active plan to protect.
pub(crate) async fn main_agent_top_plan_id(state: &IpcState) -> Option<String> {
    let main_id = state.runtime.manager.lock().await.main_agent_id()?;
    agent_top_plan_id(state, main_id).await
}

/// Read ONE agent's top (root) plan id (plan ffd7a86f — the per-agent
/// sibling of [`main_agent_top_plan_id`] for spawned run-all agents).
pub(crate) async fn agent_top_plan_id(
    state: &IpcState,
    agent_id: mnemo::runtime::AgentId,
) -> Option<String> {
    let agent_loops = state.runtime.agent_loops.lock().await;
    let agent_loop = agent_loops.get(&agent_id)?;
    let workflow = agent_loop.workflow_handle();
    let workflow = workflow.lock().await;
    workflow.top_plan_id().map(|id| id.to_string())
}

/// Adopt orphaned `InFlight` backlog items at run-all START (run-all
/// orphans, 2027-01-07): an `InFlight` item whose owner is gone — an app
/// crash killed the in-memory run state, or a legacy drain left it in
/// flight — is skipped by every future run-all (dispatch only considers
/// `Pending`, the guarded requeue refuses `InFlight`), so the work strands
/// with no recovery. Requeue every such item to `Pending` (checkpoint sha
/// preserved) so the normal dispatch order picks them up.
///
/// Done-orphan guard (backlog 6c6966b9): BEFORE requeueing, consult
/// [`orphan_work_landed`] — a session that died between its
/// closing-sequence commit and `finish` left the work fully landed, and
/// requeueing it re-executes the task as duplicate work. Landed orphans
/// auto-resolve `Done` with an explanatory note instead.
///
/// Ownership: an `InFlight` item is owned by the single-dispatch pointer
/// (a live single dispatch) or by being the main agent's active root plan
/// (the user manually resumed an orphan's session and is mid-plan on it —
/// requeueing would break the InFlight ⇔ plan-active contract while work
/// happens). The caller guarantees no run-all is active (this runs before
/// the run state is constructed), so `run_all.current_item` cannot own
/// anything here. Accepted residual: another LIVE app instance sharing
/// this backlog file could own an `InFlight` item this sweep requeues —
/// one-instance-per-directory is the practical model, and the alternative
/// (never sweeping) strands every crash orphan.
pub(crate) async fn adopt_orphaned_in_flight(state: &IpcState) {
    // Phase 1 — snapshot orphan candidates under the store lock. The
    // single-dispatch pointer is read AFTER the lock is held (review
    // LOW-1, 2027-01-07): a dispatch sets the pointer strictly before
    // the Executing-entry InFlight stamp (which needs this lock), so
    // under the lock any InFlight snapshot item's pointer is already
    // visible — reading it before the lock had a TOCTOU window that
    // adopted a just-dispatched item.
    let active_plan = main_agent_top_plan_id(state).await;
    let candidates: Vec<(String, Option<String>, Option<String>)> = {
        let store = state.backlog.store.lock().await;
        let single = state
            .backlog
            .single_in_flight
            .lock()
            .expect("single_in_flight lock poisoned")
            .clone();
        // Snapshot id + note + plan id BEFORE the transitions —
        // transition overwrites the note, and the checkpoint sha must
        // survive (the manual resume/rollback anchor).
        store
            .items()
            .iter()
            .filter(|i| {
                i.status == BacklogStatus::InFlight
                    && Some(i.id.as_str()) != single.as_deref()
                    && !(i.plan_id.is_some() && i.plan_id == active_plan)
            })
            .map(|i| (i.id.clone(), i.note.clone(), i.plan_id.clone()))
            .collect()
    };
    if candidates.is_empty() {
        return;
    }
    // Phase 2 — landed-work evidence per orphan WITHOUT the store lock
    // (the git check is a blocking subprocess — holding the lock would
    // block every backlog read).
    let mut landed = Vec::with_capacity(candidates.len());
    for (_, note, plan_id) in &candidates {
        landed.push(orphan_work_landed(state, plan_id.as_deref(), note.as_deref()).await);
    }
    // Phase 3 — re-verify ownership under the lock (the unlocked evidence
    // window could have seen a manual resume or a single dispatch start),
    // then transition: Done for landed orphans (backlog 6c6966b9 — a
    // died-after-commit session's item must not requeue for duplicate
    // re-dispatch), Pending for the rest (the recovery path).
    let active_plan = main_agent_top_plan_id(state).await;
    let mut store = state.backlog.store.lock().await;
    let single = state
        .backlog
        .single_in_flight
        .lock()
        .expect("single_in_flight lock poisoned")
        .clone();
    let mut requeued: Vec<String> = Vec::new();
    for ((id, note, _), landed) in candidates.iter().zip(landed) {
        let still_orphan = store.items().iter().any(|i| {
            i.id == *id
                && i.status == BacklogStatus::InFlight
                && Some(i.id.as_str()) != single.as_deref()
                && !(i.plan_id.is_some() && i.plan_id == active_plan)
        });
        if !still_orphan {
            continue;
        }
        let suffix = if landed {
            "work already landed (plan complete + commits after the checkpoint) — auto-resolved done, not re-dispatched"
        } else {
            "orphaned in flight (no live dispatch pointer) — requeued by run-all start"
        };
        let full = match note {
            Some(n) => format!("{n} | {suffix}"),
            None => suffix.to_string(),
        };
        let to = if landed {
            BacklogStatus::Done
        } else {
            BacklogStatus::Pending
        };
        if !landed {
            requeued.push(id.clone());
        }
        store.transition(id, to, Some(full));
    }
    drop(store);
    // Parallel run-all (plan ffd7a86f, review R7): a requeued orphan may
    // be a crashed spawned lane's item — its stale worktree + branch
    // (both derivable from the item id; the SpawnedRun entries died with
    // the run state) would block every later spawned-lane dispatch of
    // the item ("branch already exists" → the fill's skip arm →
    // permanently undispatchable via lanes). Remove them best-effort — a
    // main-lane orphan has no runall worktree, so the remove just no-ops.
    if !requeued.is_empty() {
        let root = state.project.root.lock().await.root.clone();
        for id in requeued {
            let _ = mnemo::project::worktrees::remove_item_worktree(
                root.clone(),
                root.join(".worktrees").join(format!(
                    "runall-{}",
                    mnemo::project::worktrees::item_short_id(&id)
                )),
                mnemo::project::worktrees::item_branch(&id),
            )
            .await;
        }
    }
}

/// Non-terminal disposition of a user intervention (steer/interrupt) on the
/// main agent while a backlog item was in flight — the counterpart of
/// [`halt_run_all`]'s non-terminal halt paths (backlog 45dcf577: neither a
/// steer nor an approval halt may stamp a terminal status).
///
/// Locates the affected item — the id captured at halt time (run-all items
/// only), else the run-all's in-flight item (an interrupt does not halt the
/// run), else the single-dispatch in-flight item — and splits on the source:
/// a RUN-ALL item is KEPT `InFlight` with its run STOPPED (backlog b83e891f:
/// the plan is still active — InFlight ⇔ plan active, the status contract —
/// and the turn that completes it must still be able to resolve the item
/// through the kept run state; requeueing + ending the run stranded the
/// item, because the user's natural resume continues the plan with no
/// dispatch pointer and the completing turn's resolution was blind). A
/// SINGLE-DISPATCH item is KEPT `InFlight` too (plan cace17a6): the handler
/// restores the `single_in_flight` pointer it consumed (check-and-set), so
/// the turn that completes the plan resolves it through the normal
/// plan-tied rules. Only a run-all item whose run was drained before this
/// resolution requeues (no pointer left — the queue is its only recovery).
/// A still-`Pending` item (the plan loop never reached
/// `Executing`) stays in the queue with the reason recorded in its note.
/// Never bumps the done counter, never dispatches the next item, never
/// auto-feeds: a user intervention hands control back — the run drains
/// when the item resolves (the stopped check) or the agent exits
/// ([`drain_run_all_on_main_exit`]).
async fn handle_user_intervention(
    app: &tauri::AppHandle,
    state: &IpcState,
    iv: crate::ipc::state::UserIntervention,
) {
    let mut item_id = iv.item_id.clone();
    // The item's SOURCE decides the disposition (backlog b83e891f, plan
    // cace17a6): a run-all item (the latch id — halt_run_all only latches
    // run-all items — or the run's in-flight pointer) is KEPT InFlight
    // with its run stopped; a single-dispatch item is KEPT InFlight too
    // with its single_in_flight pointer RESTORED below — the plan stays
    // active either way, and the turn that completes it resolves the item
    // through the normal plan-tied rules. Only a run-all item whose run
    // was drained before this resolution requeues (no pointer left — the
    // queue is genuinely its only recovery).
    let mut from_run_all = item_id.is_some();
    if item_id.is_none() {
        let run_all_guard = state.backlog.run_all.lock().await;
        if let Some(r) = run_all_guard.as_ref() {
            item_id = r
                .current_item
                .lock()
                .expect("current_item lock poisoned")
                .clone();
            from_run_all = item_id.is_some();
        }
    }
    if item_id.is_none() {
        item_id = state
            .backlog
            .single_in_flight
            .lock()
            .expect("single_in_flight lock poisoned")
            .take();
    }

    // Never auto-continue past a user intervention: STOP the run we
    // interrupted — and ONLY that run (identity-guarded, under a single
    // run_all hold so the matched run cannot end and be replaced between
    // the check and the set). The stop flag, not ending the run, is the
    // intervention's brake (backlog b83e891f): the kept `current_item`
    // pointer is what lets the resumed turn's resolution resolve the item;
    // the run ends when the item resolves (the stopped check in the
    // run-all branch) or the agent exits (drain_run_all_on_main_exit).
    // The steer path already set the flag via halt_run_all (idempotent);
    // the interrupt path does not halt, so this is what stops it. A run
    // the user started WHILE the intervention turn was still running (a
    // deferred Run-All has `current_item == None`) is not ours to stop —
    // it dispatches on its own next resolution. The done-bump /
    // next-dispatch / auto-feed are skipped either way: the intervention
    // hands control back. The result also feeds the disposition below:
    // a RUN-ALL item whose run is no longer active (drained by a
    // main-agent exit before this resolution) has no pointer left — it
    // requeues instead of staying InFlight (review round-1 LOW B); a
    // single-dispatch item keeps InFlight with its pointer restored
    // (plan cace17a6).
    let interrupted_run_active = match item_id.as_deref() {
        Some(id) => {
            let guard = state.backlog.run_all.lock().await;
            let live = guard.as_ref().map(|r| {
                r.current_item
                    .lock()
                    .expect("current_item lock poisoned")
                    .clone()
            });
            let matched = matches!(
                live.as_ref().and_then(|o| o.as_deref()),
                Some(interrupted) if interrupted == id
            );
            if matched {
                if let Some(r) = guard.as_ref() {
                    r.stop.store(true, Ordering::Relaxed);
                }
            }
            matched
        }
        // No item in play (idle-agent intervention): no interrupted run to
        // stop — leave any active run alone.
        None => false,
    };

    if let Some(id) = item_id.as_deref() {
        let mut store = state.backlog.store.lock().await;
        let found = store
            .items()
            .iter()
            .find(|i| i.id == id)
            .map(|item| (item.status, item.note.clone()));
        if let Some((status, note)) = found {
            let suffix = format!("{}, returned to queue", iv.reason);
            match status {
                BacklogStatus::InFlight if from_run_all && interrupted_run_active => {
                    // Run-all item whose run still holds it: the plan is
                    // still active (InFlight ⇔ plan active, the status
                    // contract) — KEEP the item InFlight and record the
                    // reason. Requeueing it here stranded the item
                    // (backlog b83e891f): the user's natural resume
                    // continues the plan with no dispatch pointer, and
                    // the turn that completes it resolved nothing — the
                    // item re-dispatched forever. The kept (stopped) run is
                    // what lets that turn's resolution see the item.
                    store.annotate(
                        id,
                        &format!(
                            "{}, kept in flight — the plan stays active, resume to continue",
                            iv.reason
                        ),
                    );
                }
                BacklogStatus::InFlight if !from_run_all => {
                    // Single-dispatch: the plan is still active (InFlight ⇔
                    // plan active, the status contract) — KEEP the item
                    // InFlight and RESTORE the pointer this handler consumed
                    // above, so the turn that completes the plan resolves it
                    // through the normal plan-tied rules (done via the
                    // plan-loop gate + plan linkage; failed only on
                    // abandonment; non-closure → annotate + re-restore, the
                    // amplifier fix). Requeueing here desynced the item from
                    // its plan (plan cace17a6: item 0296d448 sat Pending
                    // with finished work). Check-and-set under one lock
                    // hold: never clobber a concurrent ▶ dispatch that set
                    // the slot inside the take→restore window (the 96e2862a
                    // round-2 pattern).
                    store.annotate(
                        id,
                        &format!(
                            "{}, kept in flight — the plan stays active, resume to continue",
                            iv.reason
                        ),
                    );
                    let mut slot = state
                        .backlog
                        .single_in_flight
                        .lock()
                        .expect("single_in_flight lock poisoned");
                    if slot.is_none() {
                        *slot = Some(id.to_string());
                    }
                }
                BacklogStatus::InFlight if from_run_all && !interrupted_run_active => {
                    // The run was drained before this resolution (a
                    // main-agent exit ended it — review round-1 LOW B): the
                    // pointer is gone, so the queue (+ auto-feed) is the
                    // only recovery — requeue with the checkpoint sha
                    // preserved.
                    let full = match note {
                        Some(n) => format!("{n} | {suffix}"),
                        None => suffix.clone(),
                    };
                    store.transition(id, BacklogStatus::Pending, Some(full));
                }
                BacklogStatus::Pending => {
                    // Pre-execution: the item never left the queue — record
                    // the reason, no status change.
                    store.annotate(id, &suffix);
                }
                // Already terminal (e.g. the agent's backlog_status tool
                // marked it, or a prior resolution resolved it): leave it
                // alone.
                _ => {}
            }
        }
    }

    crate::ipc::backlog_cmds::emit_backlog_changed(app, state).await;
}

/// Success disposition for a captured run-all item whose turn closed the
/// plan loop after absorbing the steer: commit the work and mark `Done` —
/// the run-all success arm, minus run bookkeeping (the latch path returns
/// before the run-all branch: no done counter to bump, no next item to
/// dispatch — an intervention must not auto-continue).
///
/// Without this the item would strand at `InFlight` forever: the normal
/// resolution branches key off `run_all.current_item` / `single_in_flight`,
/// and the latch path bypasses both (run-all items never enter
/// `single_in_flight`). `requeue` refuses
/// `InFlight` and dispatch requires `Pending`, so there is no UI recovery
/// path — the closed-loop resolution must happen here. The plan-loop gate
/// has ALREADY passed (the caller only routes here when `closed_loop` held),
/// so the commit mirrors the run-all success arm unconditionally.
async fn finish_captured_item_done(app: &tauri::AppHandle, state: &IpcState, id: &str) {
    let item = state
        .backlog
        .store
        .lock()
        .await
        .items()
        .iter()
        .find(|i| i.id == id)
        .cloned();
    let Some(item) = item else {
        return;
    };
    // (No sha recovery needed here: commit_success commits the working tree
    // like the run-all success arm, which also never uses the sha — the
    // automatic rollback arm is gone (backlog 45dcf577); the sha stays in
    // the note as the manual resume/rollback anchor.)
    let root = state.project.root.lock().await.root.clone();
    if let Err(e) = commit_success(root, item.clone()).await {
        eprintln!("backlog: commit failed for item {}: {e}", item.id);
    }
    state
        .backlog
        .store
        .lock()
        .await
        .transition(&item.id, BacklogStatus::Done, None);
    crate::ipc::backlog_cmds::emit_backlog_changed(app, state).await;
}

/// Whether a workflow-state transition should stamp the in-flight backlog
/// item `InFlight`: only ENTERING `Executing` counts — `Pending` → `InFlight`
/// mirrors "real execution began", never "dispatched" (a pre-planning item
/// that gets steered or interrupted must stay queued). Re-entry from another
/// non-Executing state (e.g. Complete → Executing on a fresh plan) stamps
/// again, which is harmless: the stamp only applies to still-`Pending`
/// items. Pure function, unit-testable without a Tauri `AppHandle`.
pub fn should_stamp_in_flight(prev: Option<WorkflowState>, new: WorkflowState) -> bool {
    matches!(new, WorkflowState::Executing) && prev != Some(WorkflowState::Executing)
}

/// Stamp the run-all's in-flight item (or the single-dispatch in-flight item)
/// `Pending` → `InFlight`, preserving its note (the checkpoint sha —
/// transition overwrites the note, so the current note is passed through),
/// and record the item↔plan linkage (backlog 45dcf577): the ROOT plan id
/// carried by the `WorkflowStateChanged` event, so the item's status can be
/// derived from the plan lifecycle (finish → `Done`, root abandon →
/// `Failed`). Call this when the main agent's workflow ENTERS `Executing`
/// (see [`should_stamp_in_flight`]); the stamp no-ops when there is no
/// in-flight item or the item is no longer `Pending` (idempotent under
/// repeated entries), while the linkage refreshes on every entry (a fresh
/// plan replacing an abandoned one mid-dispatch re-links).
///
/// The MAIN-agent gate lives at the call site (the forwarder owns the
/// manager lock budget): child agents entering `Executing` must not stamp
/// the main agent's item while it is still pre-planning.
pub(crate) async fn stamp_backlog_in_flight(app: &tauri::AppHandle, top_plan_id: Option<&str>) {
    let state = app.state::<IpcState>();
    // Run-All takes priority; then the single-dispatch in-flight pointer
    // (read-only — resolution still consumes it).
    let item_id = {
        let guard = state.backlog.run_all.lock().await;
        match guard.as_ref() {
            Some(r) => r
                .current_item
                .lock()
                .expect("current_item lock poisoned")
                .clone(),
            None => state
                .backlog
                .single_in_flight
                .lock()
                .expect("single_in_flight lock poisoned")
                .clone(),
        }
    };
    let Some(id) = item_id else {
        return;
    };
    let mut store = state.backlog.store.lock().await;
    let found = store
        .items()
        .iter()
        .find(|i| i.id == id)
        .map(|i| (i.status, i.note.clone()));
    if let Some((BacklogStatus::Pending, note)) = found {
        store.transition(&id, BacklogStatus::InFlight, note);
    }
    // Item↔plan linkage: refresh on EVERY Executing entry (not just the
    // stamp) so a fresh plan replacing an abandoned one mid-dispatch
    // re-links; sub-plan pushes carry the same ROOT id (bottom of the
    // stack), so they are a no-op refresh. The plan TITLE rides along
    // (backlog f45513b2) — read from the plan file's heading so the
    // Backlog tab shows a human-friendly identifier instead of the raw
    // checkpoint sha.
    let plans_dir = state.project.root.lock().await.plans_dir.clone();
    let plan_title = top_plan_id.and_then(|pid| plan_title_from_file(&plans_dir, pid));
    store.set_plan_id(&id, top_plan_id, plan_title.as_deref());
}

/// Read a plan's TITLE from its plan file — the first line's
/// `# Plan: <title>` heading (the format `create_plan` writes). The
/// Backlog tab shows this as the item's human-friendly identifier
/// (backlog f45513b2); `None` (missing file, unreadable, or a first line
/// without the heading) falls the UI back to the short plan-id chip.
/// Reads only the heading line — plan bodies can be large (the
/// chunked-write protocol).
fn plan_title_from_file(plans_dir: &std::path::Path, plan_id: &str) -> Option<String> {
    use std::io::BufRead;
    let file = std::fs::File::open(plans_dir.join(format!("{plan_id}.md"))).ok()?;
    let mut first = String::new();
    std::io::BufReader::new(file).read_line(&mut first).ok()?;
    first
        .strip_prefix("# Plan: ")
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}
