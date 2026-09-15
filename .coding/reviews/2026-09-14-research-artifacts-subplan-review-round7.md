## Verdict: FINDINGS (0 high, 1 low)

Round-7 verification of the two round-6 fixes on the same uncommitted change (plan ee65fd4b — "Research plans may write .coding artifacts; close the sub-plan review leak", options A + D; branch `wt/mnemo`, HEAD `0e1d466`, no new commits, no rewrite of rounds 1–6 reports).

**Fix 2 (`PLAN.md`) is fully true as written** and complete. **Fix 1's scoping half works** (the count sentence is now tool-layer-scoped and can no longer be read as "four tree-wide"), and the four bullets are still exact and complete for the layer they describe — but the **added app-crate clause over-generalizes**: it says the app's IPC file commands validate "before their own spawns", while two of the seven `src-tauri/src/ipc/files.rs` validate sites validate *inside* their `spawn_blocking` closure. That is the same set-characterization failure class round 6 corrected, re-introduced in miniature (one low, precision-only finding F1-R7). Everything else checked out: the census has no fifth tool-layer caller, the stale-claim sweep confirms the parent's list exactly and names nothing new, and the diff is still the same 10 files with the substantive hunks unchanged from rounds 3–5.

## F1-R7 (low) — the new app-crate clause states an ordering that is true for 5 of the 7 sites it covers

**Claim under test** (`src/tool/agent/sandbox.rs:23-26`): "Four sync-path callers remain in THIS crate's tool layer; the app's IPC file commands (`src-tauri/src/ipc/files.rs`) validate inline by design as well — before their own spawns, so `State` never crosses an await."

**Re-derived from the tree** (a tree-wide `\.validate(_for_creation|_for_write)?\(` search shows `src-tauri/src/ipc/files.rs` is the only app-crate file with call sites — 7 in total, 5 production-inline + 2 wrapped, exactly as round 6 derived):

| site | validate | spawn | ordering |
|---|---|---|---|
| `read_file` | `:36-38` | `:39` | inline, before ✓ |
| `list_files` | `:697-699` | none — the whole body incl. `read_dir` `:703` runs inline | inline, before ✓ |
| `repo_relative_pathspec` ← `git_diff_head` | `:857-859` (call `:974`) | `:977` | inline, before ✓ |
| `save_conversation` | `:1345-1347` | `:1355` | inline, before ✓ |
| `load_conversation` | `:1382-1384` | `:1385` | inline, before ✓ |
| `write_sandboxed` ← `write_file` | `:127-129` | `:149` — the command spawns the helper; validate runs **in the closure** | **inside the spawn** ✗ |
| `read_image_data_url_sync` ← `read_image_data_url` | `:63-65` | `:110` — the command spawns the helper; validate runs **in the closure** | **inside the spawn** ✗ |

The clause's *purpose* holds for all seven — each command clones the `Sandbox` and moves owned data into its task, so `State` never crosses the await. The *ordering* does not: for `write_file` and `read_image_data_url` the validate runs on the blocking pool inside the closure, and their own doc comments say so (`files.rs:30-32` "validated BEFORE the spawn" for `read_file`; `:100-103` "the closure sandbox-validates the path FIRST, inside the blocking thread" for the image read; `:117-118` "the [`write_file`] command wraps this on `spawn_blocking`"; `:139-141`). A reader auditing "which callers run `validate` on the async runtime?" from this paragraph now gets 7 app-crate sites where the true count is 5 — the same audit the round-6 L1 finding was about, in the other direction.

**Why this is not a PASS:** the parent's own round-6 derivation explicitly split the file into "five places" inline **plus** "Wrapped in that file: `read_image_data_url_sync :64` and `write_sandboxed :128`". The fix sentence drops that split and asserts the inline-before-spawn shape of all seven. The corrected census — 5 async-runtime + 2 wrapped — is what the sentence needs to say.

**Minimal fix (one clause, comment-only):** e.g. "…in THIS crate's tool layer; the app's IPC file commands (`src-tauri/src/ipc/files.rs`) validate by design as well — `read_file`/`list_files`/`repo_relative_pathspec` (← `git_diff_head`)/`save_conversation`/`load_conversation` inline before their own spawns, `write_file`/`read_image_data_url` inside the spawn — so `State` never crosses an await either way." (Shorter alternative: keep the current sentence but replace "before their own spawns" with "outside or inside their own spawns, but never with `State` held".)

Severity low: comment text only, no behavior, no test impact, and every per-function doc comment in `files.rs` is already accurate.

## Q1 — Fix 1's scoping, and the four bullets: exact and complete for the tool layer

**(a) The four bullets re-derived** — production sync-path `Sandbox::validate`/`validate_for_creation` callers in this crate's tool layer are exactly:

| # | bullet | evidence |
|---|---|---|
| 1 | `approval::is_project_scoped` | `src/agent/approval.rs:133`, inline in the sync `needs_approval` seam; the P3 trade-off paragraph `:114-132` matches the code |
| 2 | `shell::resolve_cwd` | `src/tool/agent/shell.rs:217`; inline, and the file contains no `spawn_blocking` at all |
| 3 | `dispatch::research_write_verdict` | `src/agent/dispatch.rs:90` (`validate`), `:108` (`validate_for_creation`), `:110` (`lexical_path_is_link_free`), computed at `:229-241` **with the workflow mutex guard held** (the `let wf = self.workflow.lock().await` guard is live across the call) and gated on `filter == ToolFilter::ExecutingResearch` short-circuiting on the four `RESEARCH_ARTIFACT_TOOLS` names — no other state/kind reaches it |
| 4 | approval-preview hook | `file_edit.rs:1409` ← `approval_preview` `:1296`; `file_write.rs:189/191` ← `:98`; reached inline from `dispatch.rs:512` |

**Every other production site is inside a `spawn_blocking` closure** (re-confirmed from the tree; none newly added by this change): `convert_line_endings.rs:125 ← :116`; `browser/mod.rs:261 ← :260`, `:886 ← :885`; `file_append.rs:110 ← :100`; `file_edit.rs:1335 ← :1334`; `file_read.rs:120 ← :119`; `file_write.rs:140 ← :132`; `image_tools/mod.rs:64 ← :145` and `:163 ← :162`; `read_files.rs:347` inside the closure at `:226`; `search_read.rs` inside `:192`; `search.rs` has no `validate` at all; `sandbox.rs:384/386/401` are the ladder's own internal calls, reachable only through the wrapped `validate_for_write` sites. **No fifth tool-layer sync caller exists**, and the change adds none: its only new sandbox callers are `research_write_verdict` (bullet 3) and tests; `lexical_path_is_link_free`'s only production caller is `research_write_verdict`.

**(b) The sentence cannot be read as "four tree-wide"** — it names the app crate explicitly, so round 6's L1 half is satisfied. The defect is the accuracy of the added clause (F1-R7), not exclusivity.

## Q2 — Fix 2 (`PLAN.md:133-142`): every clause is true; the exception list is complete

- *"Synchronous file-system I/O in the async agent tools' `execute` paths (`file_read`/…/`describe_image`) runs inside `tokio::task::spawn_blocking`"* — correctly scoped to the `execute` paths; the six named tools' execute bodies are wholly wrapped (`file_read.rs:119`, `file_edit.rs:1334`, `file_write.rs:132`, `file_append.rs:100`, `search.rs:1281`, `image_tools/mod.rs:145/162`), and round 6's per-tool verification of exactly this holds — none of those six files is touched by the diff.
- *"The one deliberate exception is the approval-preview hook (`file_edit`/`file_write` `approval_preview` → `prepare_for_approval`, called inline from `dispatch::execute_tool_call`)"* — true. `dispatch.rs:512` calls `tool.approval_preview(&parsed_call.arguments)` directly on the async task (no `spawn_blocking`), before the `ApprovalRequest`; the two overrides are `file_edit.rs:1296→1408` and `file_write.rs:98→187`; the trait default (`src/tool/mod.rs:144`) returns `None`.
- *"one `canonicalize` plus one preview read on the async task"* — true for the whole-file read (`file_edit.rs:1409` + `:1419`; `file_write.rs:189/191` + `:204`). `file_write` additionally does a **bounded 8 KB prefix probe** via `detect_line_ending_path` (`line_endings.rs:37-45`, documented as "reading a **bounded** prefix … without loading the whole file") — that is round 6's verified phrasing, not a second full preview read; not a finding.
- *"the same accepted trade-off as `approval::is_project_scoped`"* — consistent with `approval.rs:114-133`.
- *"the full caller list lives in `src/tool/agent/sandbox.rs`'s async-callers contract"* — accurate: the module doc's "## Async-callers contract (Perf H1)" section (`sandbox.rs:11-57`) enumerates the six wrapped tools, the four sync-path callers, and the app-crate note. It is also the natural landing spot for F1-R7's correction.
- **Exception list complete?** Within the claim's scope the preview hook is the only unwrapped tool-side FS work; `is_project_scoped` is named in the same sentence; the only other async-runtime `validate` (`research_write_verdict`) is dispatch-layer and plan-kind-gated, and is covered by the cross-referenced contract. No unnamed exception found.
- *"Only those two tools override the trait default (`file_append` and `convert_line_endings` have no hook)"* — true: a tree-wide `approval_preview` search returns exactly the trait default plus the two overrides; `convert_line_endings.rs:97` carries the explicit note; `file_append` has none.

## Q3 — Stale-claim sweep: the parent's list is exactly right, nothing missed

- **Remaining occurrences of the old blanket wording are only dated historical records** — confirmed: `.coding/plans/fd4a93ee-e5b3-42e6-a639-4926a4516384.md:4` ("offload all synchronous file-system I/O … cannot stall the async runtime"), `.coding/reviews/2026-04-phase6a-spawn-blocking-review.md:6-8`, `.coding/reviews/2026-08-11-phase6c-docs-registry-review.md:71`, plus `.coding/reviews/2026-09-14-research-artifacts-subplan-review-round6.md:46` (which quotes it as the finding). All four must stay as written; none was touched.
- **Live docs carry no such claim** — independently confirmed: the only match for `spawn_blocking` across `README.md`, `docs/**/*.md`, `agent.md`, `PLAN.md` is `PLAN.md:135-136`, inside the corrected sentence itself. `docs/FEATURES.md` (the other synced doc) has no perf claim.
- **Other `.coding` prose checked and not this class:** `.coding/llm-trace.md:146` ("five are async + `spawn_blocking`") is about the trace-lock writers, not the agent file tools; `.coding/reviews/2026-09-15-performance-review.md:125/150` asserts the tools' execute paths don't stall the runtime, which is true for the six wrapped tools and is a dated record besides. Per-tool comments were verified truthful in round 6 and none of those files is in this diff.

## Q4 — Regression check: same 10 files, substantive hunks unchanged

- `git diff HEAD --stat` = **10 files**, identical to round 6: the six source/test files (`src/agent/dispatch.rs`, `src/agent/tests.rs`, `src/tool/agent/sandbox.rs`, `src/tool/mod.rs`, `src/tool/workflow/plan.rs`, `src/workflow/mod.rs`), the two synced docs (`PLAN.md`, `docs/FEATURES.md`), and the two other-session bookkeeping files (`.coding/backlog.jsonl` — the `pending` sandbox-bypass item `1a5bffcf`; `.coding/plans/12284516.md` — a `## Regression test` line). No new files in the diff.
- The only working-tree changes since round 6 are the two doc/comment paragraphs that were the round-6 findings: `sandbox.rs:20-28` (the module-doc sentence) and `PLAN.md:133-142` (the Structural-perf bullet). Every other hunk re-read in the current diff matches round 6's Q5 description verbatim:
  - **dangling-link leaf** — both branches require `lexical_path_is_link_free` before `judge_research_write`: canonical branch `dispatch.rs:96-101`, lexical branch `:108-113`; both fail closed to `NotApplicable`.
  - **`judge_research_write` mutual exclusion** — `is_artifact_write_target` ends in `!is_protected_write_target`, so `Artifact`/`Protected` cannot overlap.
  - **gate** — verdict computed once under `filter == ToolFilter::ExecutingResearch`; three-arm `denial_message` (`Protected` / `NotAnArtifact` / generic) unchanged.
  - **`review_required` lifecycle** — armed on sub-plan completion (`matches!(kind, Implementation | BugFixing)`) and on `abandon_plan` of a non-root frame, cleared when a fresh root replaces the stack, `#[serde(default)]` in `StackSidecar`, restored before the skill early-return; the six new tests are present, incl. the pre-field sidecar test (`workflow/mod.rs:1985-2000`: it strips `review_required` and still resumes `Complete`/`reviewed`).
  - **file-tool ladder** — `validate_for_write` steps 1–5 untouched (no hunk touches it).
  - **prefix-cache invariant** — `schema_filter` (`workflow/mod.rs:489`) untouched and still returns the ONE plan-frozen surface for every plan kind, research included; the research restriction is enforced at dispatch. No hunk there.

## Q5 — What remains before commit

1. **F1-R7** — one clause in `src/tool/agent/sandbox.rs:23-26` (see the fix suggestion above). Comment-only; no re-test is behaviorally required, though `cargo test` was green after the round-6 fixes (2359 lib + 16 integration, exit 0) and re-running it is harmless.
2. Then commit the change **on `wt/mnemo`** (never `main`) including this report.

Nothing else is open: fix 2 is true and complete, the four-bullet census is exact and complete for its layer, the stale-claim sweep is closed, and the substantive change is byte-for-byte what rounds 3–5 verified.
