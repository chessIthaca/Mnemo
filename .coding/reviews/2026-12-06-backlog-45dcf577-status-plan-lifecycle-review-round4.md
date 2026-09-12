## Verdict: FINDINGS (0 high, 2 low)

All eight round-3 fixes verify exactly as prescribed — every reword is present in the current tree at the stated location, and none introduced a new inconsistency. The re-run exhaustive sweep (every Failed / CantResolve / rollback / roll-back / approval-stamp / intervention mention, both casings, across all changed-file surfaces plus the knowledge corpus, with the intervention path traced end-to-end through the code) confirms the 45dcf577 contract documentation is consistent everywhere except two spots: (1) two "changes NOTHING / leaves the item untouched" turn-end lists that name **interrupt** — contradicting the intervention requeue (steer/interrupt → InFlight→Pending) that the same documents state correctly everywhere else; (2) two **live** knowledge records that still carry pre-45dcf577 halt-disposition claims ("Approval halts keep stamping Failed", "the in-flight item is marked Failed"). Both findings are doc/knowledge-sync class with zero behavioral impact — the behavioral contract is unchanged since round 3 and remains correctly implemented.

## 1. Round-3 fix verification — all eight PASS

- **R3-LOW-1** — src-tauri/src/ipc/run_all.rs:1627-1629: `// Already terminal (e.g. the agent's backlog_status tool marked it, or a prior resolution resolved it): leave it alone.` — the "an approval halt stamped it" claim is gone. ✅
- **R3-LOW-2** — src/backlog.rs:102-107: `Failed` = "The root plan was abandoned (backlog 45dcf577); the work stays in the tree."; `CantResolve` = "An explicit dead end named by the agent (`backlog_status`); the work stays in the tree." ✅
- **R3-LOW-3** — src/backlog.rs:1137: `// Pending → Failed (the agent's explicit backlog_status marking).` ✅
- **R3-LOW-4** — src/backlog.rs:1149: `// InFlight → CantResolve (the agent's explicit dead end).` ✅
- **R3-LOW-5** — src-tauri/src/ipc/events.rs:869-871: the premature-delivery consequence now reads "would close the turn as a non-closure — the run halts with the item left in_flight ('plan loop did not close (workflow: Reviewing)')" — no "mark the item Failed" claim remains. ✅
- **R3-LOW-6** — events.rs:25-29 and src/runtime/turn_resolve.rs:9-12 both now frame "rolled back on Error and then commit-successed on Finished" as the OLD contract and describe today's ("the failure resolution would halt the run and the success resolution would then act on stale state"); run_all.rs:936-937's busy-guard tail says "interleaving corrupts statuses and **resolves the wrong item**" — not "rolls back the wrong item's work". ✅
- **R3-LOW-7** — extract_checkpoint_sha (run_all.rs:841-846): "the manual resume/rollback anchor — no automatic caller since backlog 45dcf577 removed the rollback arm"; git_ops::rollback (src/project/git_ops.rs:166-168): "the manual rollback primitive (git reset --hard to the checkpoint sha; no automatic caller since backlog 45dcf577 removed the rollback arm)". Both "no automatic caller" claims verified against the tree: extract_checkpoint_sha's only references are its definition, the run_all module-doc mention (:110), the backlog.rs annotate-doc mention (:399), and tests; rollback's only callers are tests. ✅
- **R3 nit** — src/backlog.rs:399-400: "`extract_checkpoint_sha`-style head parsing" as plain code text — the dangling `crate::agent::loop_impl` intra-doc link is gone. ✅

## 2. Sweep coverage

Method: full reads of run_all.rs (1773 lines), events.rs (1358), backlog_cmds.rs (487); complete term-hit audits of src/backlog.rs, src/runtime/turn_resolve.rs, src/tool/workflow/backlog.rs, src/project/git_ops.rs — PascalCase (`Failed|CantResolve|[Rr]oll|stamp|[Aa]pproval`) AND lowercase (`failed|cant|roll`) so snake_case tool-schema text was covered too; README.md / PLAN.md hit audits plus direct reads of both changed sections (PLAN.md :50-80, :887-898); a full walk of `.coding/knowledge/**/*.md` plus complete reads of the canonical 2026-12-06 spec, the superseded 2026-09-01 spec, both 0a58db61 decision records, and the 2026-08-31 steering spec; and the intervention path traced end-to-end in the code (agent.rs steer/interrupt latch recording → halt_run_all's two arms → on_main_turn_resolved's latch-first consumption → handle_user_intervention's requeue).

Clean (no stale claim): events.rs, backlog_cmds.rs, src/backlog.rs, turn_resolve.rs, src/tool/workflow/backlog.rs (the `backlog_status` tool module doc :22-25, tool doc :214-216, and schema description :250-256 all state the new contract, and its transition table matches src/backlog.rs exactly), git_ops.rs (checkpoint doc's "a clean sha to roll back to" describes the sha's purpose as the manual anchor — acceptable), README.md (:76 states the contract correctly, including "a steer or interrupt during a run returns the in-flight item to the queue instead of failing it"), PLAN.md :50-80, the canonical 2026-12-06 spec, the superseded 2026-09-01 spec (banner + historical framing), the inflig decision record (`status = "superseded"`), and contract_fixtures.rs (fixture-only). Also examined and passed: the "stays `in_flight` through interruptions, steering pauses, and sub-plan pushes" phrasing (spec/module doc/PLAN.md :54-55) — a derivation-level claim (the plan stays active through those events, so nothing is stamped at event time); the intervention requeue at turn resolution is correctly carved out as the escape hatch in each of those documents, so it is not a stale claim.

## 3. Findings

### R4-LOW-1 (doc-sync): "interrupt" listed among the leaves-the-item-untouched turn ends — contradicts the intervention requeue stated around it

Two locations:

- **src-tauri/src/ipc/run_all.rs:101-102** (module doc, "Status ⇄ plan lifecycle" summary): "A turn that ends with the plan still active (session end, crash, **interrupt**, the agent stopping early) changes NOTHING: the item stays `InFlight` (the plan persists; a resumed session continues it)…"
- **PLAN.md:893-894** (backlog subsystem bullet): "every other turn end (loop open, crash, **interrupt**, halt-for-approval) leaves the item untouched — no rollback, the run halts, a resumed session continues the plan."

Why wrong: a user interrupt on the main agent mid-item records the intervention latch (agent.rs:314-343, gated only on the agent actually running) and the turn's resolution routes through `handle_user_intervention`, which REQUEUES the item InFlight→Pending (checkpoint sha preserved in the note) and ends the identity-matched run — the item does not stay InFlight, and its continuation is a fresh re-dispatch, not a resumed session. Both texts contradict their own neighbors:

- run_all.rs:28-32 (same module doc): "A steer or interrupt on the main agent mid-item records an intervention latch … [`handle_user_intervention`] returns the item to `Pending` … and stops the run."
- run_all.rs's success-criteria list ("Any OTHER turn end — the loop never closed …, an unverifiable workflow state, or a terminal Error …") correctly omits interrupt.
- PLAN.md:71-73: "A user steer or interrupt on the main agent is an **intervention, not a failure**: the affected item returns to `pending`" — and PLAN.md:895, three lines below the finding: "A user steer or interrupt requeues the in-flight item non-terminally" — a direct self-contradiction within the same bullet.
- README:76 and the canonical spec state the requeue correctly.

Fix: drop "interrupt" from both parentheticals — run_all.rs → "(session end, crash, the agent stopping early)"; PLAN.md → "(loop open, crash, halt-for-approval)". The intervention requeue is already stated correctly in the adjacent sentences of both documents.

### R4-LOW-2 (knowledge-sync): live knowledge records still carry pre-45dcf577 halt-disposition claims

- **.coding/knowledge/decision/2026-09-01-backlog-steer-interrupt-requeues-the-item-merged.md** (live — no status field), point (4): "Approval halts keep stamping Failed (terminal, load-bearing) — halt_run_all takes stamp_failed." 45dcf577 reversed exactly this: approval halts stamp NOTHING (annotate-only), the run state is KEPT so the post-approval turn resolution resolves the item under the same rules, and `halt_run_all`'s `stamp_failed = true` now selects the annotate+keep arm. Points (1)–(3) of the record remain accurate.
- **.coding/knowledge/spec/2026-08-31-steering-the-main-agent-during-run-all-halts-the.md** (live — no status field): "the in-flight item is marked Failed with its checkpoint sha preserved" (the steer/approval-halt UX claim) and "same logic (stop flag, checkpoint-sha snapshot, Failed transition, end_run)". The steer→Failed claim was reversed by 0a58db61 (steer → latch → requeue) and the Failed-transition claim by 45dcf577 (halt_run_all performs no status transition; the approval arm returns before end_run).

Why it matters: these are the live knowledge records for run-all halt/intervention semantics — a future session recalling them gets the pre-45dcf577 answer for exactly the behavior this change rewrote. The project's own convention (applied to the 2026-09-01 resolution-paths spec, which carries `status = "superseded"` plus a banner naming the 2026-12-06 successor) was not applied to these two files.

Fix: the same treatment — `status = "superseded"` plus a one-line banner pointing at `2026-12-06-backlog-status-plan-lifecycle.md` (the steering spec's banner should also note the 0a58db61 steer→requeue change), or an inline amendment on the merged decision's point (4); supersede the corresponding memory records if live. (The inflig decision twin is already `status = "superseded"` — historical, fine.)

Scope note: these files are outside the diff's changed files, but they sit inside the "45dcf577 contract documentation fully consistent end to end" bar this round was asked to confirm, and are the same class as round-3's LOW-3 knowledge-file finding.

## 4. Behavioral contract re-confirmation (unchanged since round 3)

Traced again as part of the sweep; no code changed since round 3 — only the eight comment/doc rewords:

- **Gate → Done**: on_main_turn_resolved's success arm requires `Complete` at turn resolution AND an observed workflow transition (`workflow_changed`) before commit + Done; a resting-Complete turn that never planned does not count.
- **Abandonment → Failed after the gate**: the `plan_abandoned` latch (Executing/Reviewing → Planning, `is_root_plan_abandonment`) is consumed only after the closed-loop gate, so an abandon followed by a successor plan that finishes still resolves Done.
- **Everything else**: annotate-only (`plan_open_note`) + halt — no status transition, no rollback, no next dispatch.
- **Approval halt keeps the run state**: `halt_run_all(…, true)` annotates once (already_halted guard) and returns BEFORE `end_run`; the post-approval turn resolution resolves the item under the same rules.
- **Main-exit drain**: the Exited arm captures `was_main` before removal and drains an active run (item keeps its status; the note records why).
- **Auto-feed only past terminal resolution**; **plan_id linkage** recorded/refreshed at every Executing entry (`stamp_backlog_in_flight` + `set_plan_id`); **intervention requeue**: steer AND interrupt both latch (agent.rs) → `handle_user_intervention` requeues non-terminally (Pending stays + annotated, InFlight→Pending with the sha preserved), ends only the identity-matched run, never auto-continues.

cargo test green (1920+16 passed, 0 failed, warning-free under `#![deny(warnings)]`) per the dispatching agent; both round-4 findings are comment/doc/knowledge-file edits that cannot affect it.

## 5. Summary for the main agent

Round 3's conclusion ("with these rewords, the contract documentation is fully consistent end to end") holds for everything round 3 examined, but was incomplete: the word "interrupt" in the two leaves-untouched lists is not a Failed/CantResolve/rollback/stamp term, so the prior term-based sweeps passed over it, and the two live knowledge records were outside the changed-file scope. Fix R4-LOW-1 (two one-word deletions in run_all.rs:101 and PLAN.md:893) and R4-LOW-2 (two supersede banners, or an inline amendment on the merged decision's point 4), re-run cargo test, and the 45dcf577 contract documentation is fully consistent end to end.
