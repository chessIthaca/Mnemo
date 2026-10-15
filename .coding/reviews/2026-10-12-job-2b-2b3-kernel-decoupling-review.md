## Verdict: FINDINGS (0 high, 3 low)

The 2b.3 kernel decoupling is landed as described and every acceptance criterion I could verify against the working tree holds: the moves are structurally verbatim with the consumers re-pointed, the skills regression is fixed in both bin arms, and the boundary test's pass condition is independently confirmed by my own banned-token sweep. Three low findings: two latent soundness gaps in the boundary test's strip logic (false-negative channels, all currently inert) and one stale crate-doc line.

Reviewed-state: aeba3b657acd05816417826f09d21b3c86fc1452

## What I read and verified (evidence trail)

Read directly from the working tree (uncommitted, base aeba3b6 — the stat shows the full expected file set):

- **Boundary test** (`src/lib.rs` :34-180): traced the strip state machine line by line — per-line `in_string` reset, `depth` guarded decrement (`}` only at depth>0, no underflow), braceless-item break (`!opened && !starts_with('#') && ends_with(';')`), whole-file exemption list, comment-line skip. The walk excludes `app` by directory name at any depth under `src/`, and `read_to_string` fails loud on non-UTF-8.
- **Independent pass-condition check**: my own sweep for the five tokens (`crate::(workflow|backlog|skill|app)|tool::workflow`) over `src/**` found matches in exactly three files — `agent/factory.rs` (first `crate::app` at :1634, inside the `#[cfg(test)]` at :1629), `agent/prompt.rs` (first at :615, inside the :611 `#[cfg(test)]`), and `agent/tests.rs` (the whole-file exemption, declared `#[cfg(test)] mod tests` at `agent/mod.rs` :108-118). Zero matches in any other kernel file — including comments. The boundary test's green verdict is confirmed independently.
- **workflow_handle.rs** (new, 208 lines): trait surface + `UnwiredWorkflow` + `capitalize_token`. Traced both `state()` consumers in the kernel: the pinned_state stamp/compare (`loop_impl.rs` :1637/:1644 — stamp vs current token, self-consistent within one agent) and `workflow_state_label` (:2439-2442 — capitalized for display). `is_complete()`'s only kernel consumer is `turn.rs` :3527 (finish-capture guard); `UnwiredWorkflow::is_complete() = false` is coherent with the documented intent. `workflow_expects_progress` is gone from the working tree (no search hits; the graph's :2440 hit is the stale pre-move index).
- **plan_format move**: `memory/mod.rs` :20 `pub mod plan_format`; `plan_file.rs` :15-16 re-exports `PlanFile/PlanKind/ReviewRoundStamp/Step` + imports the private helpers; both memory consumers re-pointed (`finish_capture.rs` :46, `indexer/plans.rs` :11); the parse-side tests moved with the code into `plan_format.rs`'s cfg(test) mod.
- **ask_user_args move**: `dispatch.rs` :898 re-pointed to `crate::agent::ask_user_args::parse_ask_user_args`; the app tool keeps the arg type via `use crate::agent::ask_user_args::AskUserArgs` (:34) and its execute fallback/schema path unchanged.
- **from_state**: zero `WorkflowState`/`workflow::` references left in `src/tool/mod.rs`; the impl sits at `app/workflow/mod.rs` :214 (inherent impl in same crate, legal) next to its one live caller (:515).
- **skills chain**: kernel factory has no skills field/handle (only the two re-wired tests at :3063/:3219 and the :1753 test-base helper, all inside the cfg(test) mod, documented as the None test base). `WorkflowPolicyFactory::new` sweep: production sites are exactly `main.rs` :508 (Some) and `console.rs` :1230 (Some); both load `mnemo::app::skill::SkillLibrary` from `brain.project.skills_dir` — the None regression is fixed with no other production path left. `enter_skill` reads the app factory's `skills_handle` (ipc/agent.rs :853-860). The GUI arm's `tauri::async_runtime::block_on` (:503) follows the established setup-closure precedent (:397, :455); the console arm awaits the same lock natively. Deadlock risk assessed: setup runs pre-event-loop on the main thread, no concurrent task holds `brain.project` at that point.
- **git_ops slim**: signatures neutral `(root, item_id, item_text)` (:238-249, :332-343); all 5 run_all call sites re-shaped (:4397, :4560, :4883, :6028, :7259) and both source-assert strings re-shaped in place (:2338, :2624 — same containment assertions, new argument shapes, not weakened); no `BacklogItem` left in `src/project/**`.
- **skills_seed**: kernel-side, `include_str!("../../.coding/skills/...")` resolves to repo root from `src/project/` — correct; app/skill/mod.rs :32 re-imports `SHIPPED_SKILLS`; the doc's two-edit rule + the guard test reference remain coherent.
- **Evasion routes**: swept for `mnemo::app|super::app` in `src/**` — only doc comments; no production aliasing.
- **No new `#[allow]`**: the only `#[allow(...)]` in `src/**` are pre-existing `clippy::too_many_arguments` in `failure_triage.rs` (:326/:661, not in this diff) — nothing added or weakened.
- **Bookkeeping accuracy (one line)**: the step file's ## 2b.3 section + the A1-A5 audit records match the landed code (the handle trait, the state-label token + `capitalize_token`, the `workflow_expects_progress` retirement per decision 2 — all verified above); the `3a30be71.md` frame change is harness bookkeeping. Accurate.

## Findings

### L1 (low) — boundary-test strip logic: latent false-negative channels

All currently inert (the tree is clean and the test is green), but the tripwire can silently stop watching:

1. **Single-line attribute + braceless item** — `#[cfg(test)] use crate::app::X;` on ONE line: the braceless-break guard `!t.trim_start().starts_with('#')` never fires on that line, so the consumer keeps eating until it finds a balanced `{...}` — swallowing the entire following production item (its tokens go unscanned). The repo style puts attributes on their own line today; one reformat silently opens the hole. Fix: treat a line that both starts with `#[cfg(test)]` and ends with `;` as self-contained.
2. **Brace-depth skew inside stripped items** — the `in_string` machine tracks only `"` and resets per line, so `'{'`/`'}'` char literals (e.g. `matches!(c, '{')` in a test) and multi-line raw strings with unbalanced braces inflate depth: over-consumption past the item's closing brace, swallowing following production lines (false negative), then re-syncing later — a silent unscanned gap. Fix: skip `'…'` char literals in the scanner and reset on raw-string delimiters, or document the limitation.
3. **Premature braceless-break** — a doc-comment line between `#[cfg(test)]` and the item's first `{` that happens to end with `;` (e.g. `/// see foo();`) breaks early, leaking the test item's body into the token scan → a confusing false positive. Fix: also require `!starts_with('/')` on the braceless-break guard.
4. `#[cfg(all(test, …))]`/`#[cfg(any(test, …))]` items are NOT stripped (exact-prefix match only). None exist in the kernel tree today and a future one would fail loud (positive direction), so this is acceptable — worth one doc line in the test.

### L2 (low) — token scan has documented-by-spec evasion routes; note them

The five spec tokens (:255-258) miss `super::app`, `mnemo::app`, and crate-alias imports (`use crate as x; x::app::…`). I swept: none are used in kernel code today (doc comments only). The test implements the spec's contract as written, so this needs no code change — but the tripwire's guarantee is weaker than its name implies; one doc line in the test (or the spec's boundary-enforcement bullet) naming the covered spellings would prevent someone concluding the scan is exhaustive.

### L3 (low) — stale crate doc in `src/lib.rs` :8-10

The crate doc still enumerates the brain as "(config, project, provider, tool, memory, **workflow**, runtime, agent)". Post-2b the workflow is app-side (`src/app/workflow`); the sentence is still true of the crate as a whole, but after a whole job spent drawing this exact boundary the crate root should say so. Suggested fix, e.g.: "…the kernel brain (config, project, provider, tool, memory, runtime, agent) plus the app-side workflow graph (see `app`) are fully usable without the TUI."

## Acceptance-criteria checklist

- **Zero behavior change / verbatim contracts**: verified structurally (re-export + helper-import shape identical, consumers re-pointed 1:1, parse tests moved with the code, source-assert strings re-shaped not weakened). The battery + goldens gates are the parent's re-run claim (I cannot execute cargo); the working-tree evidence is consistent with them.
- **Boundary test sound**: verdict confirmed on the current tree by an independent sweep; latent gaps reported as L1 (all false-negative direction except noted).
- **from_state restore, git_ops slim, skills regression fix**: verified complete (L0 details above); no other None path remains.
- **Multi-platform neutrality**: no `cfg(windows)` or platform paths in any new/moved code; the moves are verbatim.
- **Warning-free / no `#[allow]` tricks**: no new allows in the diff.
- **Documentation sync**: module docs updated (plan_format/skills_seed/workflow_handle carry 2b.3 notes; factory/prompt tests carry move comments); one stale line remains (L3).

Fix L1 (at minimum cases 1 and 3 — both one-line guard tweaks), L2 (one doc line), L3 (one doc line), re-run `cargo test --workspace`, and this should pass.