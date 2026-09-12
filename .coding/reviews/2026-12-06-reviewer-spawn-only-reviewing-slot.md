## Verdict: FINDINGS (0 high, 3 low)

Review of ALL uncommitted changes on `wt/agenticcoder` for plan 0d124d58 — "[models.reviewing] is reviewer-spawn-only; main agent stays on [models.executing]". The functional change is correct and well-tested; the 3 findings are documentation/knowledge-hygiene items, none blocking.

**Summary:** `ConfigModelResolver::resolve` now maps `WorkflowState::Reviewing => models.executing.as_ref()` (main agent never consults the reviewing slot; reviewing-set-but-executing-unset → None → default — exactly the intended semantics). The new trait method `resolve_reviewer_model()` (default `None`) implements the reviewer pin chain reviewing → executing → subagent → None with per-link endpoint validation, and `spawn_agent` calls it once inside the unchanged `role=="reviewer" && no-model` guard. Precedence, spawn paths, config parse/save/patch, and the workflow state machine are all preserved. Tests fail under the old code (real regression value). Docs are synced in README, PLAN.md, module/field/wire docs, and the Settings hint.


## Detailed verification (per the review checklist)

### 1. Main-agent resolution semantics — CORRECT
- `src/model_resolver.rs:266`: `WorkflowState::Reviewing => models.executing.as_ref()` — the reviewing slot is never consulted by the state chain; reviewing-set-but-executing-unset resolves `None` → default provider (pinned by the second block of `reviewing_state_keeps_executing_model_not_reviewing_slot`).
- Arm order untouched: skill (1) > subagent-if-subagent (2) > state (3) > None, read directly at `model_resolver.rs:232-270`. The plan's key reading is honored — a literal `Reviewing => None` would have fallen to the default provider, not executing; `executing.as_ref()` is the correct fix.
- All remaining `WorkflowState::Reviewing` usages in the tree (prompt.rs, tool/mod.rs ToolFilter, workflow transitions, plan.rs finish gates, run_all, events, integration tests) are state-machine/UI — none resolve models. The only per-turn model consumer (loop_impl → `resolve` with live state) now flows through the new arm.

### 2. Reviewer precedence chain — CORRECT
- Explicit `model` arg > pin: `forced_model.or(reviewer_pin)` at `spawn_agent.rs:314`, and the pin guard `role=="reviewer" && forced_model.is_none()` (`:256`) means an explicit arg never even consults `resolve_reviewer_model` (asserted: `reviewer_asks == 0` in `explicit_model_arg_beats_reviewer_pin`).
- `ConfigModelResolver::resolve_reviewer_model` (`model_resolver.rs:273-288`): three independent `config.resolve_model_ref(...)` calls chained with `or_else`. `Config::resolve_model_ref` (`src/config/mod.rs:109-116`) returns `None` for both a `None` input and a dangling endpoint reference — so each link is validated independently and a dangling reviewing ref falls through to executing (a real improvement over the old combined `reviewing.or(executing)` single-link validation, which dropped the whole spec and would have landed on subagent/None). Deliberate, documented in the trait doc + decision record.
- Chain result: reviewing → executing (back-compat) → subagent → None (default). Matches README, PLAN.md, tool schema text, and the wire-DTO/config field docs.

### 3. Spawn paths — CORRECT
- Parent-aware path passes `forced_model.or(reviewer_pin)`; plain path (`spawner.spawn(name, task, role)`) takes no model — unchanged, as documented. The `forced_model`-without-parent-path rejection pre-check is untouched, and the rejected-spawn-never-pays-for-recall ordering is preserved (pin computation happens before the rider, but it's a cheap in-memory resolver call, not a store round-trip).
- Only `ConfigModelResolver` implements the trait in production; every other implementor (agent/tests.rs, runtime/agent.rs test doubles, list_models.rs MockResolver) inherits the `None` default — the cross-crate trait addition is source-compatible, which the clean src-tauri build confirms.

### 4. Test coverage / regression value — GOOD
- `reviewing_state_keeps_executing_model_not_reviewing_slot`: fails under old code in both directions (old code returned the *reviewing* model when both set; returned `o3` for reviewing-only instead of `None`).
- `reviewer_model_chain_reviewing_executing_subagent` / `reviewer_model_falls_back_to_subagent_then_none`: pin the full chain including the dangling-reviewing → executing fall-through.
- spawn_agent tests: the reworked `StateResolver.resolve()` records and *always returns None*, so under the old two-step pin no model would be forwarded and every `reviewer_*` test fails at its `.expect(...)` — the `asked`-stays-empty + `reviewer_asks == 1` assertions make any regression to the state-chain pin (or double resolution) visible. `non_reviewer_role_gets_no_pin` asserts `reviewer_asks == 0`.
- Minor gap (informational, not a finding): dangling *executing* → subagent fall-through isn't directly tested (only dangling reviewing → executing). The three links are structurally uniform, so risk is negligible.
- Unused-import risk under `#![deny(warnings)]`: `ModelContext`/`WorkflowState` were dropped from the file-top imports and the test module uses fully-qualified paths; the reported green `cargo test` (both crates) proves zero warnings.

### 5. Stale-prose sweep — CLEAN in code/docs
- `loop_impl.rs` 429-stickiness comments corrected; config field doc, wire DTO doc, and Settings hint all now state reviewer-spawn-only. No `*.toml` example mentions `reviewing`. Historical records (old plans/reviews, the 2026-09-01 bug digest listing the old four-slot switching) are point-in-time and correctly left alone. Config parse/save/patch (patch.rs, settings_dto.rs) untouched — slot remains configurable, as intended. The two knowledge-file wrinkles are findings 1–2 below.

### 6. Security — PASS
No new surface: the resolver only reads config under its `RwLock` (same pattern as `resolve`, no cross-lock re-entrancy — `resolve_model_ref` is `&self` on `Config`); spawn remains `NeedsApproval`; the reviewer role's read-only tool filtering is untouched.

### 7. Multi-platform neutrality — PASS
Pure Rust/TSX logic; no paths, shell, or OS APIs. No `cfg(windows)` additions.

### 8. Docs sync — PASS (with findings 1–3 below)
README agents bullet now reads "planning vs. executing vs. complete" for main-agent routing plus the explicit reviewer-spawn-only clause; PLAN.md paragraph is accurate; module doc, trait-method doc, tool doc, schema text, `ModelsConfig.reviewing` field doc, `ModelsConfigWire.reviewing` doc, and the ModelsSection hint all agree. Workflow state machine and finish gates intentionally unchanged — verified untouched.

## Findings

### LOW 1 — On-disk spec export still describes the old two-step pin (cross-instance staleness)
`.coding/knowledge/spec/2026-08-27-spawned-reviewers-run-on-the-models-reviewing-mo.md:6` still documents the old mechanism (`resolve(Reviewing,false).or_else(resolve(Reviewing,true))` and "preference... i.e. precedence explicit spawn_agent(model=…) > reviewing.or(executing)"). The amendment lives only in this instance's gitignored `memory.db` and in the new decision file. Since `.coding/knowledge/` is the git-traveling truth (memory.db is a rebuildable cache), another instance re-deriving its index from files re-imports the stale mechanism (mitigated by the later-dated decision file, README, and PLAN.md, which all carry the new mechanism). Suggested fix if the memory tooling permits: regenerate/supersede the spec export on disk, or record the accepted limitation next to the decision.

### LOW 2 — New decision knowledge file is dated 2026-09-01, decision is 2026-12-06
Filename and `created = "2026-09-01"` in `.coding/knowledge/decision/2026-09-01-models-reviewing-is-reviewer-spawn-only-main-age.md`, while the content (correctly) says 2026-12-06. Same wrong-date pattern as the pre-existing 429-bug export (2026-09-01 for a 2026-12-05 bug) — looks systemic in the memory-export path, not caused by this diff. Recommend a backlog item for the export dating; no code change here.

### LOW 3 — `ModelContext::workflow_state` field doc omits Reviewing (pre-existing)
`src/model_resolver.rs:60` lists "(Planning / Executing / Complete / Skill)". Pre-existing omission, but this change is precisely about Reviewing semantics and the surrounding module doc was updated — completing the sync (e.g. "Planning / Executing / Reviewing (resolves the executing slot) / Complete / Skill") would finish the doc pass.

## Observations (no action required)
- `.coding/backlog.jsonl`: item 86c90ff6 flipped pending → in_flight (matches the plan; remember to flip to done at commit) and a leading blank line was removed (hygiene; all lines remain valid JSONL).
- The per-link validation change means a dangling reviewing ref now lands on *executing* where the old code would have landed on *subagent* (or None). This is deliberate, documented, and strictly closer to user intent — noted for the commit message only.
