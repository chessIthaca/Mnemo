## Verdict: FINDINGS (1 high, 5 low)

The gate itself is correctly implemented and wired (semantics, ordering, byte-for-byte gate-1 extraction, multi-platform neutrality, and the bug-plan artifacts all check out), but the documentation-sync goal is not met — the exact "explicit model wins / unless `model` is set" claims the plan was to eliminate remain live in README.md:56 and PLAN.md:470-471 — and the sanction lifecycle has one documented invariant that does not hold (a lingering sanction CAN authorize one later ad-hoc pick), plus four smaller hardening/coverage gaps.


### Verified correct

**Gate semantics & wiring (src/agent/dispatch.rs)**
- `reviewer_spawn_gate` ordering is correct: non-reviewer spawns pass through with the sanction preserved; gate 1 (failure pending) denies with the sanction untouched; gate 2 (non-empty explicit model, no sanction) denies with actionable guidance; the fall-through consumes the sanction for ANY reviewer spawn (with or without a model). All combinations are unit-tested (`reviewer_spawn_gate_*`, dispatch.rs:1082-1161).
- Wiring (dispatch.rs:255-266): runs for every `spawn_agent` call that reaches dispatch past the earlier ToolFilter check (a filter-denied spawn never spawns, so leaving the sanction untouched there is correct); stores the sanction back BEFORE returning any denial; the denial returns pre-approval (immediate feedback, same shape as the old inline block).
- The gate-1 denial text is preserved byte-for-byte from the old inline block (confirmed against the diff; the end-to-end assertions on "do NOT respawn"/"ask_user" in src/agent/tests.rs:3600 and src-tauri/src/console.rs:2485 stay green).
- Whitespace-only `model` counts as absent — matching the tool's own trim semantics (spawn_agent.rs:241-242). Tested.
- ask_user interception (dispatch.rs:241-242) reads `reviewer_failure_pending()` BEFORE clearing it — the sanction opens only when a failure was pending, and an unrelated ask_user closes a lingering one. Correct order.
- The failure latch is set in production only for `!success && role=="reviewer" && no report` (src-tauri/src/console.rs:757-766, src-tauri/src/ipc/events.rs:760-769) — consistent with the gate's assumptions; console.rs:2446 covers that path end-to-end.
- src/agent/loop_impl.rs: the new `reviewer_retry_sanctioned` field + getter/setter mirror the existing `reviewer_failure_pending` pattern; doc comments present (constitution: public functions documented).

**Guidance layer**
- spawn_agent.rs schema + struct doc: the "always wins" claim is gone from the schema/descriptions; role/model descriptions now instruct OMIT for reviewer spawns; the stale "born in Planning"/"PLANNING model" claims are fixed in the struct doc comment.
- prompt.rs: closing-sequence step 2 (lines 115-116) and STATE_REVIEWING (lines 298-299) carry the omit-model instruction; the pointer-not-duplication invariant (`reviewing_state_block_points_at_app_rules_without_duplicating_them`, :1390) still holds — the new text does not duplicate the head's verdict/bug-checklist rules.

**Constitution checks**
- Multi-platform neutrality: PASS — pure Rust core (std::sync::Mutex, string/JSON ops); no cfg(windows), no paths, no shell syntax.
- Security: the gate only tightens agent behavior; no new surface; the denial text leaks nothing; non-string role/model args degrade safely (the gate treats them as absent; the tool's own deserialize rejects them before any spawn).
- Bug-plan checks: root cause documented (BUG record — the three-layer analysis matches the code); BUG: memory exists (d2d11eb0); the HOW record is rewritten and accurate (it does not repeat the false invariant flagged in L2); the regression test exercises the gate function the dispatch wiring calls (see L4 for the wiring-coverage caveat).
- Tests: reported all green (root 1959 passed — under `#![deny(warnings)]` that also proves zero warnings; src-tauri 186+4; reviewer-filtered 20/20; prompt 19/19). As the read-only reviewer I could not execute cargo test; the code-level verification above is consistent with those results.


### Findings

**H1 — Documentation sync: the stale "explicit model wins" claims remain in README.md and PLAN.md (the exact text this plan was to eliminate).**
- README.md:56 — "a spawned reviewer runs on the `[models.reviewing]` model — falling back `[models.executing]` → `[models.subagent]` → default — unless `spawn_agent` passes an explicit `model`". After the fix, an explicit model on a reviewer spawn is dispatch-DENIED unless it is the user-sanctioned failed-reviewer retry. The "unless" clause is now false for its subject (the spawned reviewer).
- PLAN.md:470-471 — "…pinned at spawn to the `[models.reviewing]` chain: reviewing → executing → subagent → default; an explicit `spawn_agent` `model` arg always wins" — same stale claim, scoped to the reviewer exception.
- The review task explicitly asked for this search ("always wins", "unless `model` is set"); the schema/doc-comment layer was fixed but the two primary docs still re-teach the bypass. Per the project constitution ("a feature that ships with its docs not updated is an incomplete change") this is the one must-fix.
- Suggested wording: an explicit `model` on a reviewer spawn is dispatch-denied unless it is the user-sanctioned failed-reviewer retry (ask_user while a reviewer failure is pending opens a one-spawn sanction); for non-reviewer spawns the arg still wins. While in PLAN.md, the "Failed-reviewer protocol" bullet (PLAN.md:890-898) is the natural place to mention the new gate + sanction — it currently documents only the latch denial.

**L1 — Stale inline comments in spawn_agent.rs execute(): the same claims the struct-doc rewrite fixed, left in the inline comments of the same file.**
- spawn_agent.rs:264-267 — "the reviewer's own loop is born in Planning and never transitions … with no [models.subagent] it would silently run on the PLANNING model". Stale since the 2026-01-03 Subagent-state restructure (the loop is stamped `WorkflowState::Subagent`; the fallback is the default model) — the struct doc comment was corrected in this diff, these lines were not.
- spawn_agent.rs:268 — "Skipped when an explicit `model` arg already won", and :324-327 — "An explicit `model` arg wins; otherwise a spawned reviewer gets the reviewing-model pin". At the tool layer `forced_model` still takes precedence, but for reviewer spawns that path is now dispatch-denied — the comments should say so (or at least drop the "wins" phrasing the plan eliminated everywhere else).

**L2 — The documented sanction invariant does not hold: a lingering sanction CAN authorize one later ad-hoc model pick.**
- The gate's comment and the test comment (`reviewer_spawn_gate_no_model_spawn_consumes_sanction_and_passes`, dispatch.rs:1130-1134) claim "a lingering retry window (e.g. the user chose 'abandon') can never authorize a later ad-hoc pick". But consumption happens AT the next reviewer spawn: if that spawn carries an explicit model, gate 2 passes (sanctioned=true) and the spawn runs on the ad-hoc model — the exact bypass this fix denies, authorized once by the stale sanction.
- Concrete path: reviewer fails → ask_user (sanction opens, latch clears) → user picks "abandon" → abandon_plan → new plan → that plan's review spawn carries a model → allowed. Nothing clears the sanction on abandon_plan/finish; only ask_user (absent a pending failure) or a reviewer spawn closes it.
- Fix (small): clear the sanction in the abandon_plan dispatch path, mirroring the ask_user interception (dispatch.rs:241-242) — abandon is the escape hatch, so any sanction open at abandon time will never be a retry. Alternatively, weaken the comments to the true invariant. Severity LOW: narrow window, one-shot, and it requires the agent to misbehave at exactly that spawn.

**L3 — The sanction is consumed at dispatch time, before approval and before the spawn executes.**
- If the user DENIES the NeedsApproval prompt for the sanctioned retry, or the tool errors (e.g. unknown model id — the resolver rejects it), the sanction is already burned: the corrected retry with a model is denied by gate 2, and since the failure latch is clear, a fresh ask_user cannot re-open the sanction. Recovery is clean (a no-model spawn always passes, and the denial text instructs exactly that), so this is friction, not a deadlock — but it is a one-shot-at-dispatch semantic worth a conscious decision: either note it in the gate's doc comment, or consume only on a successful spawn. (The design-accepted "unrelated ask_user closes the sanction" behavior has the same shape and the same clean recovery — noted as accepted, not a defect.)

**L4 — No end-to-end dispatch test for the gate-2 denial: the wiring is only exercised end-to-end for gate 1.**
- The four new tests call `reviewer_spawn_gate` directly. The wiring — the `tc.name == "spawn_agent"` match, the `arguments.get("role")`/`.get("model")` extraction, and the store-back — is covered end-to-end only for gate 1 (src/agent/tests.rs:3568 `dispatch_denies_reviewer_respawn_while_failure_pending`; console.rs:2446). A wiring typo (wrong JSON key for `model`, dropped store-back) would leave all four gate tests green while the ad-hoc-model denial never fires in production — precisely the user-reported scenario. The `make_dispatch_fixture` harness already exists; a sibling test asserting that a spawn_agent call with role:"reviewer" + model through `execute_tool_call` is denied (and, ideally, that the sanctioned-then-consumed sequence behaves through dispatch) would close the gap and satisfy the constitution's "the test must fail without the fix" in the strict end-to-end sense.

**L5 — The gate's role comparison does not trim; a whitespace-padded role bypasses the model gate.**
- The gate compares the raw `role` string (`if role != Some("reviewer")` → pass-through as non-reviewer), but the tool trims it (spawn_agent.rs:224: `args.role.as_deref().map(str::trim)`) and treats `" reviewer "` as a reviewer. A spawn with a padded role therefore skips gate 2 entirely and can carry an explicit model onto a reviewer. The model check already trims for parity with the tool ("whitespace-only model is no model"); the role check should trim the same way — one-line fix. LOW because it requires deliberate padding (evasion, not the accidental invention this fix targets).
