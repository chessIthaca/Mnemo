## Verdict: FINDINGS (0 high, 3 low)

# Review: Merge hygiene overhaul (backlog #87)

Reviewed all uncommitted changes (`git diff HEAD`): `src/memory/finish_capture.rs`, `src/project/git_ops.rs`, `src/tool/workflow/plan.rs`, `src/agent/prompt.rs`, `src/skill/mod.rs`, `.coding/skills/merge_to_main.toml`, `PLAN.md`, `README.md`, plus bookkeeping (`.coding/plans/stack.json`, new plan md — glanced, fine).

## What checks out

- **Correctness of `branch_hint`** (`src/project/git_ops.rs:482-498`): reuses the existing platform-neutral `git()` helper (args array, no shell; the `CREATE_NO_WINDOW` cfg predates this change). Detached HEAD / unborn HEAD / main / master / any git failure all yield `None` via `is_empty()` / `.ok()?`. `spawn_blocking` + owned `PathBuf` matches the module's established `'static + Send` pattern and is documented. The deviation from the plan's `Option<&Path>` sketch to `Option<PathBuf>` is forced by `spawn_blocking` and documented in the doc comments — fine.
- **Budget safety**: the hint is appended to the *pointer*, and `budgeted_digest` (`src/memory/indexer.rs:365-373`) only ever truncates the gist via char-safe `chars().take()` — the pointer (with its multi-byte em-dash) is never byte-sliced, so no char-boundary panic and no budget overflow in realistic cases. The new feature-branch test asserts ≤400 chars with the hint present.
- **All call sites updated**: 6 pre-existing `capture_finish` tests pass `None`, 2 new tests pass `Some(...)`, and `FinishTool` (`src/tool/workflow/plan.rs:867-882, 972-979`) passes `wf.project_root()` — computed synchronously inside the lock block, lock dropped before the capture awaits (no lock-across-await violation).
- **Memory-tools-in-skill claim is TRUE**: every `ToolFilter` arm returns `true` for `ToolCategory::Memory` (`src/tool/mod.rs:223,257,288,320,329`), corroborated by `src/workflow/mod.rs:1340-1345` and `src-tauri/src/ipc/spawn.rs:274-276`. The skill TOML step 7, PLAN.md, and README.md statements are accurate.
- **Skill TOML**: valid triple-quoted-string escaping (embedded `"SHIPPED"`, `"ACTIVE:"` quotes are legal TOML), step numbering sequential (cleanup = 7, skill_end = 8), step instructions actionable (recall on branch+sha, memory_list prefix SHIPPED, working-tier markers, supersede with merge sha). The new `src/skill/mod.rs` test loads the real `.coding/skills/` via `CARGO_MANIFEST_DIR` and asserts the cleanup step — guards the shipped asset against rot.
- **Docs sync**: README.md feature bullet, PLAN.md workflow + merge-hygiene paragraphs, the `finish_capture.rs` module-doc "Branch hint" bullet, and the prompt.rs `Branch-status default` rule all match the shipped behavior. Prompt test `stable_head_carries_branch_status_default` pins the rule.
- **Constitution**: no `#[allow(...)]` added; public functions documented; no Windows-only APIs/paths in library code; test fixture (`git_repo_on_branch`) uses portable git invocations and normalizes the default branch with `branch -M main`.

## Findings

### Low 1 — BUG: digest hint path is never exercised by a test

`src/memory/finish_capture.rs:144-147` appends the branch hint to the BUG digest pointer — one of the plan's core deliverables ("BOTH the PLAN and BUG digest pointer lines") — but both new tests (`capture_on_a_feature_branch_carries_the_unmerged_hint`, `capture_on_main_has_no_branch_hint`, lines ~441-501) build `PlanKind::Implementation` plans, so no BUG digest is even created and this branch never runs under test. The pre-existing bug-digest test passes `None` for `repo_root`, so it can't see a hint either. Fix: add a `PlanKind::BugFixing` capture on a feature-branch git repo asserting `bug_memory_id`'s content contains `branch <name> @ ` and stays ≤600 chars.

### Low 2 — probe-failure arm ("absent without git repo") untested

The plan's stated test list included "absent without git repo". What shipped covers absence via `repo_root = None` (probe skipped entirely — all six pre-existing tests) and absence on `main`, but never `Some(<dir that is not a git repo>)`, which is the arm that exercises `git()` erroring and `.ok()?` collapsing to `None` (`src/project/git_ops.rs:486,490`). `branch_hint` also has no direct tests in `git_ops.rs`'s own test module (detached-HEAD arm likewise uncovered). Fix: one test passing a plain `tempdir()` (no `git init`) as `Some(root)`, asserting the digest carries no hint.

### Low 3 — `git_ops.rs` module doc now under-describes the module

`src/project/git_ops.rs:5` still reads "Run-All git operations — checkpoint, commit-success, and rollback", but the module now also hosts the finish-capture probe `branch_hint` (and, pre-existing, `prepare_branch`). The constitution's doc-sync rule makes stale module docs a finding; the staleness partly predates this diff (prepare_branch), but this change adds a second non-Run-All public op without touching the header. Fix: broaden the module doc's first line to cover branch preparation/probing.

## Notes (non-findings)

- `.coding/skills/merge_to_main.toml` is CRLF; the diff preserves the file's existing line-ending style (git's "CRLF will be replaced by LF" notice is pre-existing, not introduced by this change).
- `stack.json` / the new plan `.md` are workflow bookkeeping, as expected.
