## Verdict: FINDINGS (0 high, 4 low)

Full review of the uncommitted change set on `wt/mnemo` (pre-prompt model routing, backlog 091e694d): the design invariants hold, the acceptance criteria are covered by real red-checkable tests, and no correctness/security problem was found. Four low findings follow — all log/doc-honesty or UX polish, none blocking.

## What I read

- Plan `.coding/plans/d69abf72.md` (incl. the step-6 deviation note) and the decision record `2027-01-11-pre-prompt-model-routing-classifier-section-bloc.md`.
- `git_read` status + full uncommitted diff stat; the new file `src/agent/model_routing.rs` in full (538 lines), and the changed regions of `src/agent/turn.rs` (TurnRoute/TurnState, run_turn head, route_turn_start, failure-flag sites, tool-call counters, resolve_iteration_provider), `src/agent/loop_impl.rs` (gate field, with_routing_gate, the full resolve chain 1276-1570), `src/model_resolver.rs` (trait defaults, ConfigModelResolver::resolve_routed/routing_policy, test names), `src/agent/factory.rs` (field + wiring), `src/config/general.rs` (RoutingConfig + Default + skip guards + round-trip tests), `src/config/settings_dto.rs` (DTO + threshold validation), `src-tauri/src/main.rs` + `ipc/settings.rs` + `ipc/contract_fixtures.rs`, the whole frontend block (`ClassifierSection.tsx` draft/load/save/UI, `types.ts`, `tauri.ts` wire, `ClassifierSection.test.ts`, `dto-get-settings.json`), the 7 turn-level tests + helpers in `src/agent/tests.rs` (13340-13847), and the docs (README.md row 43, PLAN.md decision entry 857, docs/CONFIGURATION.md 11/13).

## Invariant verification (all hold)

- **Arm priority**: verified in code, not just docs — the picker-pin arm returns at loop_impl.rs:1455/1465, the forced-model arm returns at 1496, the routing arm sits at 1517-1541 and only fires when `route` is `Some`, and it falls through to `resolve()` at 1542 when `resolve_routed` answers None. Routing cannot beat a skill (the skill arm precedes; plus `resolve_routed` refuses skill contexts at model_resolver.rs:422), the pin, or a forced model.
- **One classification per turn**: `route_turn_start` is called once at turn.rs:414, after the turn-top deferred-swap early return (so that path writes no rows), memoized in `state.route`; every iteration passes `state.route`'s target (turn.rs:2233-2238). Test `routing_classifies_once_per_turn_across_iterations` proves it with a 2-iteration turn (calls == 1, one decision + one outcome row).
- **Fail-open completeness**: opt-in off → `routing_policy()` None (model_resolver.rs:457) → no classify, no rows (test `routing_disabled_never_classifies_and_keeps_todays_model`: 0 calls, no log file). Empty classifier slot → no rows (`routing_with_an_empty_classifier_slot_keeps_todays_model`). No answer → nothing logged (`routing_no_answer_writes_nothing_and_keeps_todays_model`). Below threshold / unknown label → target None, model unchanged but decision logged. Unset/dangling target → `resolve_model_ref` drops it → today's chain. Shadow → never switches, logs target it *would* use.
- **Subagents/reviewers/compaction**: `route_turn_start` refuses `is_subagent()` before anything else (turn.rs:1046), the resolver guard doubles it (model_resolver.rs:425), and compaction summaries resolve via the separate summarize path — routed only insofar as they already ride the turn's provider today. Test `routing_never_classifies_a_subagent_turn` covers it.
- **Drop safety**: `TurnRoute` lives in the per-turn `TurnState` (fresh at turn.rs:406), so no cross-turn leakage, no double-write, and Drop runs on every exit including `?` unwind; the outcome row reads only its own counters and its own captured `log_path` (the test seam `with_log_path` keeps tests off the real `~/.mnemo` log).
- **Log-row honesty**: `enforced`/`model` are documented as turn-start resolution facts with the mid-turn caveat spelled out in the row docs (model_routing.rs:205-214); `outcome` doc matches the two flagged abort sites + stream-Err site I verified in turn.rs (676, 948, 983); `tool_calls`/`tool_errors` match the batch-accounting code at turn.rs:2078-2085.
- **DTO semantics**: the UI draft loads all routing fields from the wire (ClassifierSection.tsx:135-147) and the save round-trips them (259-265), so a Classifier save never clobbers stored targets. The golden fixture matches the Rust wire (laya.routing:false; routing {capable, cheap, enforce, threshold 0.8}). Threshold rejected outside 0.0..=1.0 (settings_dto.rs:622-624, with a rejection test at 1405).
- **Config convention**: `RoutingConfig::is_default` + named threshold default keep an untouched section omitted; tests `routing_defaults_off_and_round_trips` and `routing_section_is_omitted_while_default_and_written_once_touched` prove the round-trip.
- **Acceptance criteria**: (1) regression pin present at both levels (`routing_config_never_changes_the_resolve_chain` model_resolver.rs:767, `routing_disabled_never_classifies_and_keeps_todays_model`); (2) `routing_enforced_trivial_runs_the_turn_on_the_cheap_model` asserts the cheap provider served and both rows landed; (3) `routing_below_threshold_keeps_the_default_model_but_logs_the_answer`; (4) both paths covered, plus empty-slot, no-answer, shadow, once-per-turn, subagent — 7 turn tests, 3 module tests in model_routing.rs, resolver tests at 743/767.
- **Step-6 deviation**: matches the plan note — no transcript note; enforcement visibility rides `ModelChanged` (note_served_effective at turn.rs:2275 fires on the routed switch), shadow visibility rides the log + Settings block; `with_log_path` test seam landed as noted.

## Findings (high first)

### L1 — `iterations` counts loop passes that make no provider request
`src/agent/turn.rs:416-420` increments `route.iterations` at the top of every loop pass, before `handle_pending_swap` — the deferred-pick early return at :430-431 ends the turn with an iteration counted that never dispatched a provider request. The row doc says "Provider-request iterations the turn ran" (model_routing.rs:242). Why it matters: the effort signal for the fine-tune corpus slightly over-counts on the rare deferred-swap path. Fix: count the iteration after the swap early-return (or amend the doc to "loop iterations").

### L2 — enforce-mode target can land mid-turn after the decision row said `enforced: false`
`route_turn_start` stores `arm_target = decision.target` in enforce mode even when it does not resolve at turn start (turn.rs:1066-1069), and the per-iteration arm re-resolves against the live config every iteration (loop_impl.rs:1517-1524). If the user sets the target in Settings mid-turn, remaining iterations route even though the decision row was written with `enforced: false, model: None`. The row docs cover the opposite direction (a mid-turn skill making a resolved target not apply, model_routing.rs:209-214) but not this one. Why it matters: a rare, self-inflicted log/behavior mismatch while calibrating on the log. Fix: either drop the route when the target fails to resolve at turn start (memoize the miss too), or add the caveat to the `enforced` doc.

### L3 — docs overstate the enforce-switch lock
`frontend/src/components/settings/sections/ClassifierSection.tsx:452` disables the switch only for the ON direction (`disabled={status !== "ready" && !routingEnforce}`) — enforcement can always be turned OFF. docs/CONFIGURATION.md:13 says "Settings → Classifier locks the switch until the classifier reports `ready`". The implementation is the safer behavior (a dead classifier can never trap a user in enforce mode) and matches the plan's intent, but the doc and the plan step-8 wording ("greyed … unless status === 'ready'") say a full lock. Fix: one-line doc rewording ("locks switching it ON until …"), or add the deviation note to the plan.

### L4 — clearing the threshold input silently sets the gate to 0
`ClassifierSection.tsx:515` does `setRoutingThreshold(Number(e.target.value))`; an empty `type="number"` input yields `""`, and `Number("") === 0` — clearing the field sets the gate to 0 (every classified answer routes, if enforcement is on). The value is visible in the UI and passes backend validation (0.0 is in range), so it is a hazard, not a bug. Fix: clamp on save (`Math.min(Math.max(v || 0.8, 0), 1)`) or reject empty input in the draft.

## Constitution checks

- **Documentation sync** — done: README feature row (43), PLAN.md decision entry (857, accurate incl. arm placement and test counts), docs/CONFIGURATION.md block (11, 13), thorough module docs (model_routing.rs head, loop field, resolver trait). Only the L3 wording quibble above.
- **Multi-platform neutrality** — clean: no Windows-only APIs/paths/shell in the new library/app code; the log path rides the existing `training_log_dir()` abstraction; the frontend is plain React.
- **Warning-free build** — no new `#[allow(...)]` anywhere in the change set (the two `#[allow(clippy::too_many_arguments)]` hits in failure_triage.rs are pre-existing, outside this diff).
- **File-tools-first** — no shell-based file mutation in the change set.
- **`.coding/**` accuracy (one line each)** — plan d69abf72: steps 1-9 checked, step 10 open, matches what shipped; backlog.jsonl: item 091e694d `in_flight` with `plan_id: d69abf72` — accurate for an unmerged change set; decision record: matches the shipped `[general.laya] routing` + `[general.routing]` shape and shadow-first staging.

## Security note (considered, not a finding)

The decision row stores up to 2000 chars of raw task text (`cap_task_text`) in the local training log — the same locality and precedent as the failure-triage log it sits beside; no new exposure surface.

No high findings. The change is safe to land once the four low items are triaged (L1/L2 are one-line doc-or-counter tweaks; L3 is a doc rewording; L4 is a small clamp).

Reviewed-state: b62671c07892fcb4443e96e4e9bfddbb0fdb9815
