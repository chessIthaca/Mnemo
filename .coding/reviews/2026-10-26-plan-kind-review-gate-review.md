# Review: Plan-kind review gate + workflow lifecycle in system prompt

Reviewed ALL uncommitted changes (`git diff HEAD`) for the plan titled
"Plan-kind review gate + workflow lifecycle in system prompt".

Files in scope: `src/workflow/plan_file.rs`, `src/workflow/mod.rs`,
`src/tool/workflow/plan.rs`, `src/agent/prompt.rs`, `agent.md`. Also inspected
transitive consumers (`src-tauri/src/ipc/agent.rs`, `src-tauri/src/ipc/contract_fixtures.rs`,
`frontend/src/lib/types.ts`, `frontend/src/lib/ipc-fixtures/dto-workflow-state-info.json`).

## Summary

The core logic is correct and well-tested: the `PlanKind` enum, the `## Kind`
section parse/serialize, the `complete_step` root-vs-sub-plan branching, the
borrow-checker fix (copying `kind` out before the branch), the `reviewed=true`
path for research plans, and `load_latest` re-deriving `Complete` for a
completed research plan are all sound. The `finish` tool is correctly unchanged
(research plans never enter `Reviewing`, so the report gate is never reached
for them). The `agent.md` and prompt changes are consistent in tone.

**One real bug** (a broken test) and one minor consistency note, below.

---

## Findings

### Bugs

#### B1 (HIGH) — Contract fixture drift: `PlanFile.kind` leaks into the frontend wire shape and breaks `dto_fixtures_match_serde`

`PlanFile` is serialized directly to the frontend: `WorkflowStateInfo.plan` is
`Option<PlanFile>` (`src-tauri/src/ipc/agent.rs:146`), and `get_plan` returns a
`PlanFile` (`src-tauri/src/ipc/agent.rs:502`). The new `kind` field
(`src/workflow/plan_file.rs:73-77`) carries `#[serde(default)]` but **no**
`skip_serializing_if`, so it is always emitted as `"kind": "implementation"`.

The golden fixture `frontend/src/lib/ipc-fixtures/dto-workflow-state-info.json`
was **not** updated (it is absent from the diff) and contains no `kind` key in
its `plan` object:

```json
"plan": {
  "context": "Context notes here.",
  "goal": "Add the thing.",
  "steps": [ ... ],
  "title": "Implement feature X"
}
```

`assert_fixture` (`src-tauri/src/ipc/contract_fixtures.rs:55-82`) does exact
`serde_json::Value` equality and panics on mismatch:

```rust
if actual != expected {
    panic!("fixture drift for {}: ...", name, ...);
}
```

After this change, `serde_json::to_value(&ws_executing)` produces a `plan`
object that includes `"kind": "implementation"`, which differs from the
committed fixture (no `kind`). The `dto_fixtures_match_serde` test
(`src-tauri/src/ipc/contract_fixtures.rs:84`, declared via
`#[cfg(test)] mod contract_fixtures;` at `src-tauri/src/ipc/mod.rs:29-30`)
will therefore **panic with "fixture drift"** when `cargo test` runs the
`src-tauri` crate (or the workspace).

The plan's FILES CHANGED list omitted `src-tauri/src/ipc/contract_fixtures.rs`,
`frontend/src/lib/ipc-fixtures/dto-workflow-state-info.json`, and
`frontend/src/lib/types.ts`. This is a test failure that the closing-sequence
Test step (`cargo test`) is required to catch — so either the full test suite
was not run, or the failure was not fixed.

**Fix (pick one):**
- **(a) Intended to expose `kind` to the frontend** (e.g. a research/implementation
  badge): update the fixture `dto-workflow-state-info.json` to add
  `"kind": "implementation"` inside `plan`, add `kind: "implementation" | "research"`
  to the `PlanFile` interface in `frontend/src/lib/types.ts:79-84`, and add a
  frontend assertion in `ipc-contract.test.ts` (the existing assertions at
  lines 283-284 tolerate the extra field, so the TS test alone won't catch it —
  the Rust `assert_fixture` is the lock).
- **(b) `kind` is an internal workflow concern not meant for the wire**: add
  `#[serde(skip)]` (or `skip_serializing_if`) to the `kind` field so it does
  not leak into the serialized `PlanFile`. This keeps the fixture + TS type
  unchanged and is the smaller change. Note `PlanFile::serialize()` (the `.md`
  writer) uses the manual `as_str()` path, not serde, so `#[serde(skip)]` would
  NOT affect on-disk persistence — the `## Kind` section is still written.

Either fix makes `cargo test` green again.

---

### Constitution compliance

#### C1 — Test step not satisfied (consequence of B1)

The "Standard plan closing sequence" Test step requires `cargo test` to pass
before marking the final step complete. B1 introduces a failing test
(`dto_fixtures_match_serde`), so the build is not green. Fix B1 to resolve.

No other constitution issues found:
- No `#[allow(...)]` suppressions added.
- All new public items have doc comments (`PlanKind`, `create_plan_with_kind`,
  the `kind` field). Private helpers `from_str_ci`/`as_str` are documented too.
- Code style matches the existing conventions.
- The `agent.md` change is consistent with the existing tone and correctly
  scopes the test→review→commit sequence to implementation plans with a
  research-plan skip clause.

---

### Correctness (verified clean)

- **Kind flow**: `create_plan` → `create_plan_with_kind(..., Implementation)`;
  `CreatePlanTool` passes `args.kind` (defaulting to `Implementation` via
  `#[serde(default)]`). Correct.
- **`complete_step` branching**: sub-plan (`stack.len() > 1`) always pops to
  the parent regardless of kind; only the root branch reads `kind`
  (`src/workflow/mod.rs:507-526`). Correct — covered by
  `research_sub_plan_pops_to_parent_without_skipping`.
- **Borrow fix**: `let kind = frame.plan.kind;` copies the `Copy` value out of
  the mutably-borrowed frame before `self.stack.len()`/`pop()`/`persist_stack()`
  (`src/workflow/mod.rs:502-506`). Correct — avoids extending the mutable
  borrow.
- **`load_latest` for research plan**: completion sets `reviewed = true`;
  `load_latest` restores `persisted_reviewed` before the skill early-return and
  derives `Complete` for a fully-checked plan with `reviewed == true`
  (`src/workflow/mod.rs:716-745`). Correct — covered by
  `restart_derives_complete_for_research_plan`.
- **`PlanFile::parse` Kind section**: recognizes `## Kind`, takes the first
  non-blank line, case-insensitive, unknown → `Implementation`
  (`src/workflow/plan_file.rs:150-153, 195-202`). Backward compat (no section →
  default) works. Covered by `plan_without_kind_section_defaults_to_implementation`,
  `unknown_kind_value_defaults_to_implementation`, `kind_is_case_insensitive`.
- **`PlanFile::serialize`**: emits `## Kind\n{kind}\n\n` between Goal and
  Context (`src/workflow/plan_file.rs:223-225`). Correct order.
- **`finish` tool**: unchanged; still gates on `Reviewing` state + non-empty
  report (`src/tool/workflow/plan.rs:520-551`). Research plans never enter
  `Reviewing`, so the gate is correctly never reached for them.
- **`reviewed` + skill-active restart**: the existing
  `reviewed_flag_survives_restart_while_skill_active` path applies equally to a
  research plan (which sets `reviewed = true` at completion); no new
  interaction introduced.
- **Prompt `WORKFLOW_LIFECYCLE`**: added to the stable head
  (`src/agent/prompt.rs:93-94`); byte-stable across turns (ignores workflow).
  Does not contain "WORKFLOW STATE"/"CURRENT STEP"/"RECALLED MEMORIES", so the
  existing `stable_head_excludes_volatile_sections` assertion still holds.

---

### Security

No new mutation surfaces. `kind` is a persisted enum with a safe default
(unknown → `Implementation` → reviewed, not silently skipped). No path-traversal
or injection surface introduced. Clean.

---

### Minor (non-blocking)

#### M1 — Frontend `PlanFile` type lacks `kind`

`frontend/src/lib/types.ts:79-84` (`interface PlanFile`) has no `kind` field.
If B1 fix (a) is chosen (expose `kind`), add it there. If fix (b) is chosen
(`#[serde(skip)]`), no change needed. Not a test failure on its own (TS is
structurally typed and the `ipc-contract.test.ts` assertions tolerate extra
fields), but the Rust↔TS mirror should stay in sync with whichever direction
is chosen.
