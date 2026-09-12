## Verdict: FINDINGS (0 high, 2 low)

Review of plan **f7fce6b9** (reviewer-only authorship) + concurrent prompt condensation (**2fb308d6**), against HEAD `232a474` and residual uncommitted bookkeeping only.

### Summary

The three planned enforcement layers are **implemented and committed**:

1. **ToolFilter** — `write_review_report` denied unless `ToolFilter::Reviewer(_)` and listed; Skill cannot smuggle it; tests cover all base states.
2. **Sandbox** — `.coding/reviews/` protected for file tools (existing, creation, case); `write_review_report` bypasses sandbox by design.
3. **Protocol** — no live self-review option (dispatch, events, prompt, agent.md, README, PLAN.md). `abandon_plan` visible in Reviewing and pops → Planning.

Reviewer spawn still grants `write_review_report` via `REVIEWER_BASE_TOOLS` + `ALWAYS_SAFE_ROLE_TOOLS` / `set_reviewer_allowlist`. Unrestricted subagents do not get it. Prompt condensation retains hard rules. Multi-platform: case-normalized path checks; no new Windows-only APIs.

Shell residual is **documented** on the primary surfaces (sandbox.rs, agent.md, README). Commit message reports `cargo test` 1430 passed under `deny(warnings)`.

---

### Findings

#### Low

1. **Stale comment in Reviewing filter test** (`src/tool/mod.rs` ~992–994)  
   `reviewing_filter_exposes_closing_tools` still says finish is the *only* workflow tool beyond `ask_user`/`current_plan`, while the body correctly asserts `abandon_plan` is visible. Update the comment so it does not contradict the assertion (list `finish` + `abandon_plan` escape; hide `complete_step`/`create_plan`/`update_plan`).

2. **`abandon_plan` schema omits Reviewing escape** (`src/tool/workflow/plan.rs` schema ~684–689)  
   Description still says “prefer `update_plan`” / last resort when the plan is wrong. In Reviewing, `update_plan` is hidden — abandon is the only non-finish exit. Prompt/docs already say this; add one sentence to the schema the model sees.

#### Not re-filed (resolved or acceptable)

- **Shell residual (prior F3):** Scoped correctly as approval-gated residual in sandbox module doc, agent.md, and README. Design matches `.coding/plans/`. Optional PLAN.md parenthetical would be nice-to-have only — not blocking. No code change required for this plan’s three layers.

---

### Checklist

| # | Check | Result |
|---|--------|--------|
| 1 | `write_review_report` only under Reviewer when listed | **Pass** |
| 2 | `.coding/reviews/` protected; writer tool works | **Pass** |
| 3 | No live self-review instruction | **Pass** |
| 4 | `abandon_plan` in Reviewing → Planning | **Pass** (runtime); schema wording **Low #2** |
| 5 | Spawn grants via ALWAYS_SAFE / set_reviewer_allowlist | **Pass** |
| 6 | Prompt condensation kept hard rules | **Pass** |
| 7 | Multi-platform neutrality | **Pass** |
| 8 | Tests cover new paths | **Pass** |

### Bookkeeping note

Uncommitted: plan stack flip toward `2fb308d6`, plan checkbox, optional repass review file. Feature code is on `232a474`.

### Bottom line

Plan goal met at the three specified layers. **0 high, 2 low** (stale test comment; `abandon_plan` schema Reviewing note). No correctness/security defects in runtime enforcement.
