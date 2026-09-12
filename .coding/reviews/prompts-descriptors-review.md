## Verdict: FINDINGS (0 high, 3 low)

# Review: plan 79997137 — prompts.toml tool descriptors + workflow-state texts (branch wt/prompts-descriptors)

Reviewed ALL uncommitted changes (`git diff HEAD`): `src/agent/prompt.rs`, `src/agent/turn.rs`, `src/agent/factory.rs`, `src-tauri/src/main.rs`, `README.md`, `PLAN.md` (plus the untracked plan file `.coding/plans/79997137-*.md`).

---

## Verified correct (the requested CHECK list)

**1. Fallback semantics (missing sections / v1 files).** `tool_descriptors` carries `#[serde(default)]` (empty map = no overrides) and `workflow_states` carries `#[serde(default)]` → `WorkflowStateBlocks::default()` → per-field `#[serde(default = "default_state_*")]` compiled consts (prompt.rs:385-390, 307-330). A v1 file with neither new section parses cleanly; `load_str` (prompt.rs:460-468) emits only an `eprintln!` version warning and still returns the parsed blocks — parsing never breaks on version mismatch. Regression coverage exists: `prompt_source_reload_picks_up_edits_and_deletion` writes a `version = 1` file *after* the bump and asserts the reload still applies the preamble; `load_prompts_parses_new_v2_sections` covers partial `[workflow_states]` tables falling back per state.

**2. Empty-section omission.** `push_state_text` (prompt.rs:~826) does trim_end → skip-if-empty → push one `\n` — mirroring `push_section`'s deliberate-empty semantics. `workflow_states_override_and_empty_semantics` proves an empty `[workflow_states.reviewing]` omits the static guidance while the dynamic scaffolding (`# WORKFLOW STATE` header, `Current state:` line, `# CAPABILITIES`) still renders.

**3. Byte-stability of the defaults through `push_state_text`.** Compared all four new consts line-by-line against the removed inline strings: identical after Rust `\`-continuation collapse (including the Reviewing arm's mid-sentence "Then fix EVERY finding / yourself" split). `push_state_text` round-trips a const ending in exactly one `\n` byte-identically (trim_end strips only that `\n`, then pushes one back). Live confirmation: the reviewer's own volatile tail (produced by this exact new path) renders the full compiled REVIEWING text word-for-word. The round-trip test additionally asserts `load_str(seed) == const` for all four states.

**4. No state arm lost text.** Planning (state text + the conditional sub-agent NOTE after it), Executing main-agent line (now overridable) and its compiled sub-agent variant, Reviewing, Complete — all preserved. The Skill arm stays fully compiled/interpolated, documented on `WorkflowStateBlocks` (prompt.rs:295-303).

**5. The main.rs seeding-order self-heal — claim VERIFIED.** `PromptSource::new(config_dir.join("prompts.toml"))` is evaluated at src-tauri/src/main.rs:1494 as an argument to `AgentLoopFactory::new`, i.e. strictly before `Arc::new(factory)` (:1526) and `ensure_prompts_file_with_descriptors` (:1534). For a missing file, `PromptSource::new` caches `mtime: None` + compiled defaults (prompt.rs:494-498); `reload_if_changed` treats `self.mtime != Some(mtime)` as a change (prompt.rs:510-514); and `PromptHolder::prompt_blocks()` calls `reload_if_changed()` on *every* turn (loop_impl.rs:259-264, invoked at turn.rs:512 before both the tail build and `apply_tool_descriptors`). So the seeded file — including its `[tool_descriptors]` — is active from turn 1. Both entry paths (GUI main.rs:250, console console.rs:1382) flow through `build_brain_inner`, so one call covers both, as the comment claims.

**6. `apply_tool_descriptors` wiring.** turn.rs:616-636: `schemas()` (deterministic `(priority_class, name)` sort happens *inside*, before the override) → sub-agent retain → apply. `complete_with_retry(..., &tool_schemas, ...)` at turn.rs:699 uses the post-override array, so overrides genuinely reach the request. Entries for filtered-out tools are naturally ignored (name lookup misses); empty map is an early-return no-op; sub-agents/reviewers share the same `run_turn` path with an independently-reloading cloned `PromptSource` (factory.rs:583) — single choke point as documented. `strict` flag handling inside `schemas()` is untouched (description-only mutation).

**7. `tool_descriptor_map` (factory.rs:650-665).** The throwaway `Workflow::new` is a pure struct init with no FS side effects (workflow/mod.rs:165-176); `build_registry(&workflow, None)` constructs exactly the tool set real agents get. Spot-checked the highest-risk schema() bodies (`spawn_agent`, `skill_start`, `list_models`, `write_review_report`, browser tools, file/plan/memory tools): every description is a static string literal — no dynamic interpolation found (targeted searches for built descriptions came back empty), so the "schemas are static" doc claim holds and the seeded map equals what built agents advertise.

**8. TOML round-trip + never-clobber.** `default_prompts_toml_with_descriptors` clones the map into `PromptBlocks::default()` and pretty-serializes (seeded `version = 2` → no warning on first load). `seeded_descriptors_round_trip_and_apply` proves the map survives parse-back and actually overrides a schema; `ensure_prompts_file_with_descriptors_seeds_and_never_clobbers` proves seed-once + user-edit survival for the new variant (plain-variant test retained); the updated `default_prompts_toml_round_trips_to_const_defaults` covers the empty-map seed.

**9. mtime test hardening.** `write_until_mtime_advances` (rewrite-until-stamp-strictly-advances, bounded 400×5 ms, panics on failure) is the *correct* fix for fixed-at-write mtime granularity — the stamp is set at write time, so only a rewrite can move it; polling alone could never observe a tick boundary. The deletion case correctly relies on the presence flip (None↔Some) without retry.

**10. Security.** Seed content is compiled constants only, written via the existing `write_atomic` into the user config dir; no user input flows into the seed; the `toml` crate handles all escaping. `apply_tool_descriptors` mutates description strings only. No sandbox or tool-gating changes.

**11. Constitution checks.**
- *Doc comments on public items:* all new public API documented (`apply_tool_descriptors`, `WorkflowStateBlocks` + all 4 fields, `default_prompts_toml_with_descriptors`, `ensure_prompts_file_with_descriptors`, `tool_descriptor_map`). One doc gap → Finding 1.
- *Multi-platform neutrality:* no `cfg(windows)`, no Windows paths/syntax in the changed lib/app code; the test helper uses only `std::time`/`std::thread`. ✓
- *Docs sync:* README.md and PLAN.md accurately describe v2, `[tool_descriptors]`, `[workflow_states.*]`, and delete-to-reseed; `PROMPTS_FILE_HEADER` documents both new sections including the post-app-update reseed note; the prompt.rs module doc is updated. ✓
- *Warning-free build:* I cannot execute cargo (read-only reviewer). All new public API is exercised by production code or tests (no dead-code risk), no unused imports observed; the main agent reports green runs (1483 root tests, src-tauri check clean) — consistent with my inspection.

---

## Findings

### Low 1 — `build_volatile_tail` doc comment not updated for the new `states` parameter
`src/agent/prompt.rs:696-712`. The public fn gained `states: &WorkflowStateBlocks`, but its doc still describes only "the recalled-memories block … + the workflow-state section" and the cache rationale — it never says the static per-state guidance now comes from `states` (overridable via `[workflow_states.*]` in prompts.toml, falling back to compiled consts). That contract currently lives only on the *private* `workflow_section` and on `WorkflowStateBlocks`. Add one sentence to the public fn's doc (e.g. "The workflow section's static per-state guidance comes from `states` (`[workflow_states.*]` overrides, compiled consts as fallback); the dynamic lines stay compiled — see `WorkflowStateBlocks`.").

### Low 2 — stale comment at the `PromptSource::new` argument references the old call
`src-tauri/src/main.rs:1492-1493`. The inline comment still says the file is "seeded on first run by ensure_prompts_file" — but the seeding call was moved and renamed (`ensure_prompts_file_with_descriptors`, now at :1534). The pointer comment added at the old site (:940-947) and the comment at the new call site are accurate; this inner one now names a function/position that no longer matches. Update it to point at the with-descriptors call below the factory (it can double as the one-line justification of the None→Some mtime self-heal).

### Low 3 — `tool_descriptor_map` doc says "first-run-only call" but it runs on every startup
`src/agent/factory.rs:656` vs `src-tauri/src/main.rs:1534-1537`. `factory.tool_descriptor_map()` is evaluated as an argument on *every* `build_brain_inner` run — before `ensure_prompts_file_with_descriptors`'s `exists()` early-return — so the throwaway registry + ~80 schema builds happen on every app start, not just first run. The cost is negligible (static struct wiring, no IO), so this is only a doc/precision issue: either reword the doc to "cheap enough to run once per startup" or, cleaner, gate the map construction in main.rs on the file being missing (`if !config_dir.join("prompts.toml").exists() { ... }`) so the cost and the doc agree.

---

## Design observation (not a finding)
Seeding *all* live descriptors as override entries means a user who never edits the file keeps first-run snapshots overriding compiled descriptions after app updates, until they delete the file/entries — a stale-text (not correctness) risk that is the plan's explicit, documented contract (PROMPTS_FILE_HEADER "after an app update, delete the file to reseed"; PLAN.md same). Called out here so it stays a conscious tradeoff rather than an accident.

## Test coverage
New: `apply_tool_descriptors_overrides_matching_names_only`, `apply_tool_descriptors_empty_map_is_noop`, `workflow_states_override_and_empty_semantics`, `load_prompts_parses_new_v2_sections`, `seeded_descriptors_round_trip_and_apply`, `ensure_prompts_file_with_descriptors_seeds_and_never_clobbers`, `tool_descriptor_map_contains_core_tools` (factory), plus the mtime-hardened reload/malformed tests. Existing state-text assertions keep passing through defaults. Coverage matches the plan's steps 1-4.

**Bottom line:** the implementation is correct on every point the task asked me to verify, including the non-obvious main.rs ordering claim. The three findings are documentation-precision fixes only — no behavior change required (Finding 3 optionally allows a trivial gating change).
