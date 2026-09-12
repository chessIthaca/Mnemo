## Verdict: FINDINGS (0 high, 4 low)

Reviewed all uncommitted changes on `wt/agenticcoder` (11 modified files, +534/−51) against the plan goal: 1-indexed step addressing end-to-end, short collision-checked plan ids, forgiving plan_id guard with did-you-mean hints, plan_id echoes, out-of-order completion warning, clearer index errors.

**Verified clean (no findings):** StepNumber untagged enum + 0-rejection + `step_number - 1` conversion (plan.rs:181-207, 649-659); turn.rs event extraction accepts int OR numeric string and stays gated on `result.success`, so rejected calls (0, out-of-range, id mismatch) never emit StepCompleted (turn.rs:1687-1738); 1-indexed payload coherent across channels.rs docs → console.rs `✓ step N` → contract_fixtures.rs(3) ↔ event-step-completed.json(3) pairing → events.rs sample(1) → types.ts doc → useAgentStore.test.ts(1) → workflow_integration.rs fragments/loops; `unique_plan_id` terminates unconditionally (len increments to `end == hex.len()` = full-hex fallback), slices ASCII hex safely, `exists()` works pre-`create_dir_all`, and no functional 36-char assumption survives (stack.json, memory v5 slugs in finish_capture.rs:121-124, and the get_plan blocklist all treat ids as opaque strings; legacy 36-char plan files coexist fine); update_plan echo is borrow-correct (all-immutable borrows, `title`/`plan_id` cloned before `format!`/`json!`, additive data shape); out-of-order detection computed pre-completion is correct for in-order last step (`step_index == split`, no warn), prefix re-completion (no warn), and genuinely out-of-order (warns, data flag set); parent-hint fires before the stale-file scan so a parent id is never mislabeled an older plan file; empty/whitespace plan_id falls through to the generic advice; prefix-accept is ≥4 chars against the ACTIVE id only with a correct normalization note; multi-platform neutrality holds (Path methods only, `read_dir` degrades via `if let Ok` + `flatten`, no cfg(windows), no shell syntax). No `complete_step` Tauri command exists (the plan's step-2 worry resolves as moot; only spawn.rs tests call the internal 0-indexed `Workflow::complete_step`, correctly). Regression-test coverage is strong: 1-indexing, zero rejection, numeric-string accept/reject, prefix accept + note, parent hint, stale-file hint, out-of-order warn/no-warn, update_plan echo, short-id minting, collision extension, out-of-range convention hint. Tests were reported green by the main agent (cargo test 1542 passed, exit 0 under `#![deny(warnings)]`; contract_fixtures + vitest green) — reviewer is read-only and did not re-run.

Numbered findings below (all low; none block correctness of the shipped behavior).


## Finding 1 (low) — Out-of-range error echoes the internal 0-indexed value next to the 1-indexed valid range

**File:** `src/workflow/plan_file.rs:314-317` (surfaced via `src/tool/workflow/plan.rs:745`)

`PlanFile::complete_step` receives the already-converted 0-indexed index, and its error prints it raw:

```
"step index {step_index} out of range (have {len} steps) — complete_step step numbers are 1-indexed, valid 1..={len}"
```

Because the tool converts with `step_number - 1` before calling, the number shown to the model is **one less than what it passed**, and the added convention suffix makes it actively contradictory: passing `step_index: 4` for a 3-step plan yields *"step index 3 out of range (have 3 steps) — ... valid 1..=3"* — the echoed 3 lies **inside** the stated valid range, so the model cannot tell what it did wrong. The existing test (`complete_step_out_of_range_errors`, plan.rs:2261) passes `99` and only asserts the `"1-indexed"` substring, so this incoherence is not pinned either way.

**Fix (either is fine):**
- (a) Pre-validate in `CompleteStepTool::execute` before converting: after the 0-check, compare `step_number as usize > plan.steps.len()` and return `ToolResult::error(format!("step {step_number} out of range (plan has {len} steps; step numbers are 1-indexed, valid 1..={len})"))`; leave `PlanFile`'s internal message alone. This also needs the workflow lock taken before the range check — the lock is already taken at plan.rs:660 before any plan access, so the check can sit after the `plan_mutations_allowed` gate using `wf.plan()`.
- (b) Change `PlanFile::complete_step`'s message to print `step_index + 1` as the failing number (the message now addresses the model-facing convention, so it should speak it).

Add/adjust a test asserting the echoed failing number equals what was passed (e.g. pass `4` with 3 steps → error contains "step 4 out of range ... valid 1..=3").


## Finding 2 (low) — `plan_id_hint` can name the ACTIVE plan as "not the active plan"

**File:** `src/tool/workflow/plan.rs:757-775` (stack loop includes the active frame)

`plan_id_hint` iterates the **whole** stack (`wf.plan_stack()` is root→active, so the last frame IS the active plan). The guard upstream already rules out an exact match and a provided-prefix-of-active (≥4) match against the active id — but not the third resemblance direction: a provided id **longer** than the active id that starts with it. Example: active id `abcd1234`, model passes `abcd1234ff00` → guard mismatches → hint loop hits the active frame via `id.len() >= 4 && provided.starts_with(id)` → error claims *"'abcd1234ff00' is the id of plan 'T' on the plan stack — that is not the active plan"* — factually wrong, since `T` IS the active plan. Rare (requires an over-long transcription), but the audience of this hint is exactly a model that already mis-transcribed once.

**Fix:** exclude the active frame from the stack scan — iterate `&stack[..stack.len().saturating_sub(1)]` — so only genuinely *other* stacked plans can be hinted. (No test covers this edge today; add one: pass `format!("{active_id}ff")` and assert the error does NOT claim the active plan "is not the active plan".)


## Finding 3 (low) — `get_plan`'s security comment + error message still claim plan ids are UUIDs

**File:** `src-tauri/src/ipc/agent.rs:576-578` (doc comment), `:587-593` (inline comment), `:600-602` (error text)

After this change `create_plan` mints an 8-hex handle (`unique_plan_id`), but `get_plan` still documents *"`plan_id` is validated as a UUID — plan ids are always UUIDs (`create_plan` generates `Uuid::new_v4()`)"* and rejects with *"invalid plan id '{plan_id}': not a UUID (plan ids are UUIDs generated by create_plan; ...)"*. **No functional break**: the check was always a blocklist (rejects empty / `/` / `\` / `..` / `.`), not a real UUID parse, and 8-hex ids pass it; legacy 36-char ids still pass too; path traversal is still rejected. But the security rationale text now describes the wrong invariant, and the error message is misleading — a constitution documentation-sync finding (stale docs). Note also an 8-hex id never contains `.` or separators, so the blocklist remains sufficient; if the project wants the check tightened to reality, `plan_id.chars().all(|c| c.is_ascii_hexdigit())` would match the new invariant — optional.

**Fix:** reword the comment + error to describe the actual invariant, e.g. *"plan ids are hex handles minted by create_plan (legacy ids are 36-char UUIDs); reject anything containing path separators or dots before joining"*, and the error: *"invalid plan id '{plan_id}' (path separators / '..' / '.' are rejected)"*. No test change needed (the existing traversal-rejection test, if any, keeps passing).


## Finding 4 (low) — Documentation/nit bundle: PLAN.md convention, stale assertion message, missing sentence boundary

Three small doc/text nits, grouped:

1. **PLAN.md:457** — the architecture doc lists `StepCompleted { step_index: u32 },` with no indexing convention. The payload semantics changed from 0-indexed to 1-indexed in this diff; the doc line is still type-accurate but silent on the convention that this whole change exists to pin down. Per the constitution's documentation-sync check (the plan's own step 6 conditioned a PLAN.md update on it documenting the StepCompleted event — it does), add a trailing comment: `StepCompleted { step_index: u32 }, // 1-indexed step number (1 = first step)`. README.md:83 mentions `complete_step` without any indexing claim — no change needed there.

2. **src/tool/workflow/plan.rs:2615** — stale assertion message: `assert!(data["id"].is_string(), "id is the plan's uuid")` in `current_plan_returns_active_plan`. The assertion itself is fine (the id is still a string); the failure message should say "short hex handle" / just "plan id" now.

3. **src/tool/workflow/plan.rs:683-694** — when a did-you-mean hint is present, the error reads *"...— not the active plan Call current_plan to confirm..."* (both hint strings end without a period and the format is `"{hint}Call current_plan..."` with hint = `format!("{h} ")`). Add a trailing `.` to the two hint templates (plan.rs:769-773, 789-791, 798-800) so the sentence boundary exists.

**Fix:** three one-line edits as above; no behavior change, no test impact (the hint tests assert `contains("on the plan stack")` / `contains("older plan file")`, both preserved).

---

## Notes for the commit step (not findings)

- The untracked plan document `.coding/plans/f1d88e59-e5a3-4d49-af33-564200a3f4f7.md` (this plan) must be included in the commit — `.coding/` plans travel with git per the constitution.
- On a sub-plan's final step, `complete_step`'s result data pairs `plan_id` (captured pre-pop = the completed sub-plan) with `plan_title`/`completed`/`total` (read post-pop = the resumed parent). Verified deliberate and documented in-code (plan.rs:701-703); the output string stays coherent because it names the *now-active* plan, which is the intended orientation aid. No change requested.
