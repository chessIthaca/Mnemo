# Review — StepInput string-or-map steps for create_plan/update_plan (backlog #38)

**Scope reviewed:** all uncommitted changes (`git status` / `git diff HEAD`):
- `src/tool/workflow/plan.rs` (the only source change: new `StepInput` untagged enum + `to_text`/`steps_to_text` normalization, `CreatePlanArgs.steps`/`UpdatePlanArgs.steps` retyped, both JSON schemas updated to `oneOf` items, normalization wired into both `execute()`s, 3 unit + 4 tool regression tests).
- `.coding/backlog.json` + `.coding/plans/stack.json` — workflow bookkeeping only (backlog item #38 → done; plan-stack id swap). No source impact.

## Verdict: no findings

The change is correct, back-compatible, schema-valid, and constitution-compliant. Details per review focus below.

### Correctness — verified

- **Untagged order safety** (`plan.rs:52-65`): `Text(String)` is declared before `Map{..}`, and serde untagged tries variants in order. The two variants accept *disjoint* JSON types (string vs object), so a plain string can never parse as `Map` (and an object never as `Text`) regardless of order — the doc comment's "order matters" phrasing slightly overstates it, but the behavior is exactly as intended. A string step always hits `Text` and passes through verbatim; an object step always hits `Map`.
- **`to_text` format** (`plan.rs:72-84`): produces exactly the documented `**{header}** — {body}` (em-dash U+2014) and `**{header}**` alone when body is empty. Both forms are recognized by the pre-existing `extract_bold_header_impl` (`src/workflow/plan_file.rs:401-428`): the em-dash arm (`after_trimmed.starts_with('—')`) and the header-only arm (`after_trimmed.is_empty()`). Normalized steps therefore acquire proper `Step.header` values through the existing machinery — verified for both `PlanFile::new` (plan_file.rs:114-135) and `Workflow::update_plan`'s replaced-steps loop (`mod.rs:466-475`, which re-extracts headers via `extract_bold_header_pub`).
- **Normalization ordering — create_plan** (`plan.rs:191-208`): `steps_to_text(args.steps)` at line 193 runs BEFORE the `is_empty()` check (194) and before `create_plan_with_kind(...)` (203). Success output and `data` both derive `n`/`step_count` from the normalized `steps.len()` (207-208) — no stale `args.steps.len()` (which wouldn't even compile, since `args.steps` was moved). `steps.clone()` at 203 mirrors the pre-existing pattern.
- **Normalization ordering — update_plan** (`plan.rs:288-305`): `args.steps.map(steps_to_text)` at line 289 runs before `wf.update_plan(...)` at 300. `None` (omitted steps) stays `None` — the "keep current steps" path is preserved.
- **`#[serde(default)] body`** (`plan.rs:62-63`): an omitted body deserializes to `""` → header-only form. Covered by `step_input_to_text_header_only_when_body_empty` (both omitted and explicit-empty cases asserted).
- **No un-normalized leak path**: `CreatePlanArgs.steps` is `Vec<StepInput>` and `UpdatePlanArgs.steps` is `Option<Vec<StepInput>>` — every step entering either tool passes through `steps_to_text` before reaching the workflow layer, which still takes `Vec<String>` (`mod.rs:345-352`, `mod.rs:399-405`). There is no other route into `create_plan_with_kind`/`update_plan` from tool args.
- **Leniency note (not a defect)**: a map step with unknown extra keys (e.g. `{"header","body","extra":1}`) is accepted and extras ignored (no `deny_unknown_fields`). Reasonable for model-facing input.
- **Edge: `steps: []` with map support** — still caught by the post-normalization `is_empty()` check (test `create_plan_requires_steps` unchanged, still valid).

### Back-compat — verified

- `CreatePlanArgs` / `UpdatePlanArgs` are private (no `pub`); a repo-wide search confirms they are only constructed via `serde_json::from_value` inside `plan.rs` — no other code constructs them, so the retype is fully contained.
- The workflow layer (`src/workflow/mod.rs`, `src/workflow/plan_file.rs`) is untouched and still stores `Vec<String>`; all existing callers (including the sub-plan push path through the same tool) are unaffected.
- String-step behavior preserved verbatim: new test `create_plan_accepts_string_step_verbatim` asserts byte-exact passthrough; pre-existing tests (`create_plan_works`, `update_plan_appends_steps_preserving_completed`, `update_plan_noop_errors`, etc.) all exercise string steps and compile against the new arg types unchanged.
- `update_plan`'s "omit steps to keep current" path is preserved (`Option::map` keeps `None`).

### Schema validity — verified

- Both schemas (`plan.rs:159-164`, `257-262`) emit well-formed JSON Schema: `items: { oneOf: [ {"type":"string"}, {"type":"object","properties":{"header":{"type":"string"},"body":{"type":"string"}},"required":["header"]} ] }`. `required: ["header"]` on the object branch correctly mirrors `#[serde(default)] body`.
- Provider compatibility: `ToolSchema.parameters` is passed verbatim into the request body (`src/provider/openai.rs:932`) and `strict` is always `None` (`ToolSchema::new`, `src/provider/mod.rs:236-243`), so OpenAI strict-mode's `oneOf` restriction is never triggered. For non-strict OpenAI-compatible endpoints (the only kind this client speaks to), `oneOf` in tool parameters is accepted. This is the first `oneOf` use in repo tool schemas — no conflicting precedent.
- Descriptions on both tools accurately document the string-or-object acceptance and the normalized form, consistent with what `to_text` produces.

### Constitution — verified

- **Doc comments**: the new private enum `StepInput`, both variants, both map fields, `to_text`, and `steps_to_text` all carry accurate doc comments (public-function rule is for `pub` items; these exceed it appropriately without over-documentation).
- **No `#[allow(...)]` added**; no dead code, no unused imports introduced (the change reuses existing imports; `Deserialize` was already imported).
- **Warning-free**: nothing in the diff would trip `#![deny(warnings)]` — the moved `args.steps` at line 193 is consumed before any later use, and lines 207-208 correctly use the surviving `steps` binding (a use-after-move here would be a compile error, and it isn't one).
- **Regression tests for the defect**: `create_plan_accepts_map_step` (plan.rs:855-874) fails on the old code with the exact reported error (`invalid arguments: invalid type: map, expected a string`) and passes with the fix; `update_plan_accepts_map_step` (918-935) is the update-side equivalent. `create_plan_accepts_mixed_string_and_map_steps` (894-916) covers the mixed-array case. The 3 `to_text` unit tests pin the join rule. All assert the stored `plan.steps[i].text` — the right observable.

### Non-source diffs

- `.coding/backlog.json`: item #38 flipped `in_flight` → `done` (expected — the plan's completion bookkeeping).
- `.coding/plans/stack.json`: active-plan id swap (expected workflow artifact).

## Summary

**No findings.** The fix is minimal, correctly ordered, fully tested (including a true regression test that fails without the fix), back-compatible with the unchanged workflow layer, and the new `oneOf` schema is valid for every provider path this client can reach.
