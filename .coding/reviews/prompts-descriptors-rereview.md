## Verdict: PASS

# Re-review: plan 79997137 — verification of the three LOW fixes from prompts-descriptors-review.md (branch wt/prompts-descriptors, commit ee8a036)

Scope: current committed state of the three fix sites + regression check on the gating change + a nothing-else-changed audit. `git diff HEAD` and `git status --short` are both empty — everything reviewed is committed in ee8a036.

---

## Fix 1 (LOW-1) — `build_volatile_tail` doc documents `states` — VERIFIED

`src/agent/prompt.rs:696-703`. The public fn's doc now carries the requested contract sentence: "The workflow section's static per-state guidance comes from `states` (`[workflow_states.*]` overrides in `~/.mnemo/prompts.toml`, the compiled consts as fallback); the dynamic lines — CURRENT PLAN / GOAL / PROGRESS / CURRENT STEP, the skill goal, the sub-agent notes, CAPABILITIES — stay compiled (see [`WorkflowStateBlocks`])."

- Matches the code: `states` is threaded into `workflow_section(caps, workflow, states)` at prompt.rs:733; the enumerated dynamic lines are exactly the ones the first review confirmed stay compiled/interpolated.
- The `[`WorkflowStateBlocks`]` link is same-module to a real public item — resolves.
- Doc grew 17→19 lines (fn start 713→715), consistent with a doc-only edit.

## Fix 2 (LOW-2) — stale `PromptSource::new` comment rewritten — VERIFIED

`src-tauri/src/main.rs:1492-1495` (inside `AgentLoopFactory::new`, arg at :1496). The comment no longer mentions the removed early `ensure_prompts_file` call; it now says the seed call **below the factory** (`ensure_prompts_file_with_descriptors`) is picked up on turn 1 via the None→Some mtime flip in `PromptSource::reload_if_changed`.

Accuracy check against the actual mechanics:
- `PromptSource::new` (prompt.rs:492-498): missing file → `mtime: None` + compiled defaults. ✓
- `reload_if_changed` (prompt.rs:509-515): `self.mtime != Some(mtime)` → re-read + flip; the first review verified `PromptHolder::prompt_blocks()` invokes it on every turn (loop_impl.rs:259-264). ✓
- The seeding call really is below the factory (gate at main.rs:1538, after `Arc::new(factory)` at :1528). ✓
- The old-site pointer comment (main.rs:940-944, "see the call after the factory is built below") remains accurate and untouched.

## Fix 3 (LOW-3) — seeding gated on `!exists()`; `tool_descriptor_map` doc reworded — VERIFIED

- `src-tauri/src/main.rs:1538-1543`: the seeding block is now wrapped in `if !config_dir.join("prompts.toml").exists()`, and `&factory.tool_descriptor_map()` is an argument *inside* the gate (:1541) — the throwaway registry build now runs only when seeding will happen.
- `src/agent/factory.rs:656-658`: doc reworded to "Cheap static wiring (no IO) — `main.rs` builds it only when the seed file is actually missing, but a per-startup call would also be fine." — exactly the agreed wording, and it matches the code: the only production caller is main.rs:1541 (inside the gate); the remaining references are the factory test (`tool_descriptor_map_contains_core_tools`, factory.rs:1101-1107) and a doc mention (prompt.rs:604), neither of which contradicts it.

**Gating regression analysis (check b) — no behavior change.** `ensure_prompts_file_with_descriptors` itself (prompt.rs:640-656) retains its own `if path.exists() { return; }` (:644-647), so it never clobbers regardless of the outer gate. Outcome equivalence: file exists → gate skips; the seeder would have no-oped anyway, so the only thing saved is the registry build (static wiring, no IO). File missing → gate passes → seeding exactly as before, and the turn-1 None→Some mtime self-heal is untouched. TOCTOU: a file appearing between the outer check and the seeder's inner check is still caught by the inner check (no clobber); the only clobber window is between the seeder's own `exists()` and `write_atomic` — pre-existing, unchanged by the outer gate, and requires the user to create prompts.toml mid-startup. The outer `exists()` is therefore purely a cost gate. `factory` remains used after the block (Brain construction), so no ownership change.

## Warnings / doc links (check c)

- The one new doc link added by these fixes (`[`WorkflowStateBlocks`]`, prompt.rs:703) is same-module — resolves.
- The main.rs comment fix is a plain `//` comment — no link semantics involved.
- No new rustc-visible surface: no new items, no signature changes, no imports touched by the fixes. Consistent with the reported green runs (root `cargo test` 1483 passed; src-tauri `cargo check` clean). I cannot execute cargo (read-only reviewer); nothing in the diff could introduce a warning those runs would miss.

## Nothing-else-changed audit (check d)

- Commit ee8a036 stat: exactly the 8 files the first review examined (`prompt.rs`, `turn.rs`, `factory.rs`, `main.rs`, `README.md`, `PLAN.md`, plan file, first review report) — no extra files, and the commit message records the 0-high/3-low fix note.
- Line-drift audit: every line number the first review cited still resolves within ±2 lines of the fix sites it corresponds to — factory.rs 650-665→650-666 (doc reword), main.rs comment 1492-1493→1492-1495 (expanded), main.rs call 1534-1537→1530-1543 (expanded comment + gate), prompt.rs doc 696-712→696-714 (doc expansion). All drift is fully explained by the three fixes themselves; every non-fix area I read (`apply_tool_descriptors`, `WorkflowStateBlocks`, `default_prompts_toml_with_descriptors`, `ensure_prompts_file*`, the factory wiring) matches the first review's verified description byte-for-byte in substance.

## Observation (not a finding)

factory.rs:652's doc link `(see [`ensure_prompts_file_with_descriptors`])` uses a bare name that is not in factory.rs's scope (no import), so `cargo doc` would flag it as an unresolved intra-doc link. This is a rustdoc-only lint — invisible to `cargo test`/`cargo check` and thus outside the project's `#![deny(warnings)]` criterion — and it pre-dates these fixes (line 652 is the doc's untouched first sentence; the reword was 656-658; the first review examined this doc and raised no link finding). Trivially fixable later by fully qualifying the path, e.g. `[`crate::agent::prompt::ensure_prompts_file_with_descriptors`]`. Not counted as a finding because it is not new and not a regression.

**Bottom line:** all three LOW findings are fixed exactly as specified, the comments accurately describe the code (including the mtime self-heal mechanics), the new `exists()` gate is behavior-neutral (the seeder's own never-clobber check is intact), and nothing beyond the three fixes + the commit itself changed since the first review.
