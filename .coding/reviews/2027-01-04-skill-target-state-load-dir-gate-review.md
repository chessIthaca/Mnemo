## Verdict: FINDINGS (0 high, 1 low)

Review of ALL uncommitted changes on `wt/agenticcoding` for plan 7e9d03ec "Close skill target_state invariant at registry load (UI enter_skill path)". Method: `git diff HEAD` + `git status` (3 modified + 3 untracked files), full read of the changed `src/skill/mod.rs`, and cross-checks against `src/workflow/mod.rs`, `src/tool/workflow/skill.rs`, `src-tauri/src/ipc/agent.rs`, `src-tauri/src/main.rs`, and the shipped `.coding/skills/merge_to_main.toml`. The fix is correct, complete, well-tested, and constitution-compliant; one minor diagnostic-wording LOW is the only finding.

### Scope of the diff

- **`src/skill/mod.rs`** (the fix): a `matches!` gate in `SkillRegistry::load_dir`'s `Ok(spec) =>` arm (lines 108-120) rejecting non-lifecycle `target_state`, an updated `load_dir` doc comment (76-80), and a regression test `load_dir_skips_skill_with_non_lifecycle_target_state` (252-306).
- **`.coding/backlog.jsonl`**: bookkeeping — backlog 67608f1b → `in_flight` (this plan), 68c4c9a5 → `done`. Benign.
- **`.coding/plans/bc09914f.md`**: bookkeeping — step 4 of the *other* (show_knowledge_activity) plan marked `[x]`. Benign, unrelated to this plan's code.
- **Untracked**: `.coding/plans/7e9d03ec.md` (this plan), `.coding/knowledge/bug/2027-01-04-ui-skill-start-...md` (BUG record), `.coding/knowledge/decision/2027-01-04-show-knowledge-activity-...md` (decision from the other plan). All benign.

### 1. Correctness — PASS

- **Gate pattern.** `matches!(spec.target_state, WorkflowState::Planning | WorkflowState::Executing | WorkflowState::Complete)` (skill/mod.rs:108-113). `WorkflowState` has exactly six variants (workflow/mod.rs:26-48: Planning, Executing, Reviewing, Complete, Skill, Subagent). The pattern accepts the three lifecycle states and rejects Reviewing/Skill/Subagent — exhaustive of the legal `skill_end` landing states, and identical to the existing `skill_start` tool gate (skill.rs:154-156). Correct.
- **Borrow-before-move.** `WorkflowState` is `Copy` (workflow/mod.rs:24). `matches!(spec.target_state, …)` copies the value — no borrow held. The `eprintln!` borrows `spec.name`/`spec.target_state` transiently (they live until the macro returns). `registry.skills.insert(spec.name.clone(), spec)` (line 121) clones the name (owned `String`) before moving `spec`. No use-after-move; the `continue` branch drops `spec` in place. Sound.
- **Panic mechanism confirmed.** `start_skill` (workflow/mod.rs:865-887) does *no* `target_state` validation — it stores it in `ActiveSkill.target_state` and sets `state = Skill`. `end_skill` (workflow/mod.rs:896) then sets `self.state = skill.target_state`. A bad target lands the workflow in `Subagent` (→ panic at allowed_tools:397-399, no allow-list on a main-agent workflow) or `Skill` (→ fail-loud at allowed_tools:384-390, no active skill) or `Reviewing` (semantic invariant violation). The load-time gate prevents the bad value from ever reaching `ActiveSkill`, so `end_skill` can never transition to a bad state. The bug's claimed mechanism is accurate.

### 2. Completeness / invariant closure — PASS

- **`enter_skill` path closed.** `enter_skill` (src-tauri/src/ipc/agent.rs:808) passes `spec.target_state` straight to `start_skill` with no validation of its own. But `spec` comes from `registry.get(&skill)` (agent.rs:787-789), and the production registry is built via `SkillRegistry::load_dir` (src-tauri/src/main.rs:1662) — so any spec reaching `enter_skill` already passed the gate. Closed.
- **`skill_start` tool path unchanged + defense-in-depth.** skill.rs:154-161 is byte-identical to before this change and still validates `target_state = args.target_state.unwrap_or(spec.target_state)` — covering BOTH the runtime args override (which `load_dir` cannot reach) AND the spec's TOML (defense-in-depth). Confirmed unchanged via `git diff`.
- **No bypass path.** `SkillRegistry::insert` is `#[cfg(test)]`-only (skill/mod.rs:71) — production cannot add specs except via `load_dir`. All `SkillRegistry::new()` call sites are tests (factory.rs:1858, tests.rs:7613/7794, skill.rs:281). Both production callers of `start_skill` (`enter_skill` + `SkillStartTool::execute`) resolve the spec via `registry.get()` — no `SkillSpec` reaches `start_skill` without going through the registry, hence through `load_dir`. The invariant is closed end-to-end.

### 3. Regression test quality — PASS

- **Exercises the changed path.** `load_dir_skips_skill_with_non_lifecycle_target_state` calls `SkillRegistry::load_dir` directly and asserts the gate skips bad specs. Without the fix, the three bad skills parse + insert (serde accepts them), so `reg.get("bad_subagent").is_none()` is `false` → `assert!` fails. With the fix they are skipped → `is_none()` is `true` → passes. Fails-without / passes-with confirmed by reasoning.
- **Premise accurate.** `WorkflowState` serde is `rename_all = "lowercase"` (workflow/mod.rs:25), and `Subagent`/`Skill`/`Reviewing` are real variants — so `"subagent"`/`"skill"`/`"reviewing"` parse to valid variants (not a serde failure). The rejection therefore comes from the new gate, not the parse-error arm. The test comment (253-266) documents this explicitly and correctly.
- **"reviewing" correctly rejected.** `Reviewing` is entered via `complete_step` (root plan's last step), never a `skill_end` target; landing in it via `skill_end` would be a state-machine invariant violation. Rejecting it is correct and consistent with the `skill_start` gate.
- **Shipped skill unaffected.** `.coding/skills/merge_to_main.toml:33` has `target_state = "planning"` (accepted), so the `shipped_skill_files_parse_and_merge_to_main_cleans_up_memories` test (which `load_dir`s the repo's skills and `.expect`s `merge_to_main`) still passes.

### 4. Constitution checks — PASS

- **(a) Documentation sync.** The `load_dir` doc comment (76-80) accurately notes a spec with a non-lifecycle `target_state` is logged + skipped. This is internal invariant hardening (not a user-facing feature), so no README.md / PLAN.md update is warranted — the code-level doc + the BUG knowledge file + the plan's Bug section are sufficient.
- **(b) Multi-platform neutrality.** Pure Rust `matches!` + `eprintln!` + `continue`. No Windows-only APIs, paths, or shell syntax. Neutral.
- **(c) Warning-free build.** No new imports (`WorkflowState` already imported at skill/mod.rs:25), no dead code, no `#[allow]`. The `eprintln!` is live; the test reuses existing `tempdir()`/`write_skill`/`super::*` imports. Verified by inspection (read-only reviewer cannot run `cargo test` — the parent agent must confirm green + zero warnings unpiped).
- **(d) Public-function doc comments.** `load_dir`'s doc comment is updated; no new public functions added.

### 5. Bug-plan specifics — PASS

- **Root cause documented** in code comments (skill/mod.rs:95-107), the BUG knowledge file (`.coding/knowledge/bug/2027-01-04-…md`), and the plan's `## Bug` section (7e9d03ec.md:19).
- **Regression test name recorded** in the plan (7e9d03ec.md:22: `load_dir_skips_skill_with_non_lifecycle_target_state`).
- **Test exercises the actual changed path** (load_dir's gate), not a tangential one.
### Findings

#### L1 — Low — `eprintln!` diagnostic uses PascalCase variant names while the TOML expects lowercase serde names

**Location:** `src/skill/mod.rs:114-118` (new code).

The skip diagnostic renders the invalid value and the valid set via `WorkflowState`'s `Display` impl (workflow/mod.rs:67-78), which emits PascalCase:

```
skill: 'bad_subagent' has invalid target_state 'Subagent' (must be Planning, Executing, or Complete); skipping
```

But a user hand-editing `.coding/skills/*.toml` writes the serde form — lowercase — because `WorkflowState` is `#[serde(rename_all = "lowercase")]` (workflow/mod.rs:25). A user who reads this stderr message and tries to correct their TOML by writing `target_state = "Planning"` (PascalCase, as the message suggests) would instead hit the *parse-error* arm (skill/mod.rs:123-125) — serde rejects the unknown variant — getting a different, more confusing message. The diagnostic and the accepted TOML syntax are out of sync.

**Why LOW, not higher:** (a) this is a stderr diagnostic, not a user-facing dialog — most users never see it; (b) it is **byte-for-byte consistent with the existing `skill_start` tool's message** (skill.rs:159: `"invalid target_state '{target_state}': must be one of Planning, Executing, Complete"`), which uses the same PascalCase `Display` output — so this change introduces no new inconsistency, it mirrors an established convention; (c) no correctness or security impact — the gate rejects correctly regardless of the message wording.

**Recommended fix (do both together, or neither — consistency matters):** in both `load_dir` (skill/mod.rs:116) and `skill.rs:159`, phrase the valid set in the serde/lowercase form the TOML actually accepts, e.g. `(must be "planning", "executing", or "complete")`. If the team prefers keeping the PascalCase `Display` convention for developer-facing logs, leave as-is — this is a judgment call, not a defect.

---

### Notes (non-findings)

- **Bookkeeping in the diff.** The `backlog.jsonl` status flips (67608f1b → in_flight, 68c4c9a5 → done) and the `bc09914f.md` step-4 `[ ]`→`[x]` are benign side-bookkeeping; the latter belongs to the *other* (show_knowledge_activity) plan, not this one, but it is a plan-file edit, not source code, and does not affect this review's verdict.
- **Test coverage of the accepted set.** The regression test proves rejection of all three bad states (subagent/skill/reviewing) and acceptance of `planning`, but does not assert that `executing`/`complete` *load*. This is a trivial gap — the `matches!` pattern is correct by inspection and identical to the proven skill.rs gate — and not worth expanding.
- **No `enter_skill`-local defense-in-depth.** `enter_skill` relies solely on the `load_dir` gate (it has no `matches!` of its own). This is the explicitly-chosen design ("registry-load validation is the strongest close: bad TOML never reaches any path") and is sound given `insert` is test-only and the production registry is always `load_dir`-built. Not a finding — noted only because a future registry-construction path added outside `load_dir` would re-open `enter_skill` alone; the `skill_start` tool would still be guarded by its own gate.
