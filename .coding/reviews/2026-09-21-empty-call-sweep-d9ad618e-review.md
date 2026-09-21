## Verdict: FINDINGS (0 high, 1 low)

Review of ALL uncommitted changes on `wt/macos-fix` for plan e02134b2 / backlog d9ad618e (the evidence-based empty-call sweep). The code is correct: all six descriptions carry the rule + example + recovery rule, all five serde arms pass the original `args` correctly to `invalid_args_error`, graph_context's semantic error surfaces the hint through `run_query`, valid calls are behaviorally unchanged, and every new test asserts text that is absent without the changes. One LOW finding: the factory.rs budget comments' recorded measurement story does not reconcile with the diff's own text (~900 chars of Planning-riding additions vs a recorded +514 delta and a "~+700" estimate), with a murky baseline provenance. The budget test itself (the actual guard) is reported green workspace-unified and the current figures are internally consistent, so this is a comment-accuracy issue, not a code bug. Details below.


## Scope reviewed

`git diff HEAD` + untracked, branch `wt/macos-fix`: `src/tool/agent/shell.rs`, `src/tool/agent/codegraph.rs`, `src/tool/agent/git_read_tool.rs`, `src/tool/memory/mod.rs`, `src/agent/factory.rs`, `.coding/backlog.jsonl`, `.coding/plans/e02134b2.md` (untracked plan file). Read against the plan file, the read_files precedent (`src/tool/agent/read_files.rs:75-103`, `:225`), `sanitize_arguments_error` (`src/tool/error_message.rs:52`), the filter membership (`src/tool/mod.rs:533-601`), and `run_query` (`src/tool/agent/codegraph.rs:58-76`).

## Verified correct

**Descriptions (all six tools).** Each carries the no-zero-argument rule, one inline example of the exact call shape, and the rewrite-don't-resend recovery rule — shell.rs:395-410, codegraph.rs:282-284 (graph_search), 440-443 (graph_context), 618-620 (graph_path), git_read_tool.rs:81-83, memory/mod.rs:137-140. Wording is compact and consistent with the read_files precedent. The shell example shows both required fields (`command` + `purpose`); graph_context's example shows the `id` form while the rule says "id (or name)" — correct for its either-or design (no `required` array added, by design decision (e) in the plan; adding one would break the name-only form).

**Error paths reach the output.**
- The five serde arms (shell.rs:454-468, codegraph.rs:300-313 graph_search, 638-651 graph_path, git_read_tool.rs:109-122, memory/mod.rs:160-174) all call `invalid_args_error(tool, &e, &args, hint)`. The `args.clone()` before `from_value` is required and correct: the `&args` inside the `Err` arm refers to the original `serde_json::Value` (the `let args: …` shadowing is not initialized until the match completes), so the received-keys list is the real call. This matches the read_files precedent exactly (read_files.rs:225 does `from_value(args.clone())`).
- `invalid_args_error` (read_files.rs:75-103) produces `sanitized base + " (received keys: …)." + " " + hint`. For an empty call the sanitized base is "parameter 'X' is required but was not provided" — so the tests' assertions (`parameter 'command'/'op'/'tier' is required`) match the actual output. `MemoryWriteArgs` declares `tier` first (memory/mod.rs:34-38), so the empty-call error names `tier` as the test asserts.
- graph_context's semantic either-or error (codegraph.rs:465-470) carries the hint, and `run_query`'s `Ok(Err(msg)) → ToolResult::error(msg)` arm (codegraph.rs:73) surfaces it verbatim in `result.output`. Its serde arm (codegraph.rs:457) correctly stays on `sanitize_arguments_error` — an empty call cannot fire it (`SymbolArgs` is two `Option`s), which is exactly why the hint rides the semantic check instead. Correct design decision.

**No behavior change for valid calls.** The only success-path change anywhere is the added `args.clone()` (a perf-only deep copy of the args Value; git_read already cloned for sub-tool dispatch). Error output for invalid non-empty calls is strictly richer (received keys + hint appended to the same sanitized base) — an improvement, consistent with the read_files/file_read precedent. No dispatch-layer changes.

**Tests assert the hint and would fail without the changes.** The description tests assert "No zero-argument form" / "do not resend the empty shape" / "e.g. {" (shell: "cargo test") — all absent from the old descriptions. The empty-call tests assert `!success` plus "rewrite the full call" and "do not resend the empty shape" — absent from the old bare `sanitize_arguments_error` output (and from graph_context's old bare either-or string). The pre-existing shell test's `starts_with("Error: The tool 'shell' failed")` still holds since `invalid_args_error` embeds the same sanitized base. The codegraph test pair correctly covers all three graph tools in one loop, including graph_context's distinct semantic path.

**No dead code.** `sanitize_arguments_error` retains 51+ callers (dispatch.rs:168, graph_context, graph_impact, and many other tools) — the five switched arms don't orphan it.

**Scope discipline verified.** graph_impact's identical semantic empty-key check (codegraph.rs:560-562) is deliberately untouched — correct per the item's evidence-based short list (graph_impact was not in the fumble evidence; the backlog item names only the graph trio). Not a finding.

**Budget mechanics.** The test asserts `chars <= ceiling` per filter (factory.rs:2105-2110); all six ceilings were raised with dated stacked cause comments in the file's established style; the recorded current figures are internally consistent (standalone + ~484 load_tools delta = workspace, exact for all six filters: 18_757+484=19_241, 33_685+484=34_169, 34_971+484=35_455, 27_810+484=28_294, 28_949+484=29_433, 18_757+484=19_241); headroom is positive everywhere (259-445). The graph tools are registered unconditionally (factory.rs:890-913, "always present") and the filter admits them (Agent+AutoRun; Memory category → `true`), so the measured arrays do carry the new descriptions.

## Findings

### LOW 1 — factory.rs budget comments: the recorded delta story does not reconcile with the diff's own text

The five Planning-riding description additions (graph_search ~167, graph_context ~195, graph_path ~174, git_read ~145, memory_write ~218 chars, counted from the diff's rendered strings — the measurement uses raw `description.len()`, no JSON escaping) total **~900 chars**, yet:

- the new Planning comment estimates the cause at "~+700", and
- the implied delta against the recorded baseline is only **+514** (19_241 − 18_727), and
- the same gap appears on Executing (+784 recorded vs ~1,048 counted for all six tools incl. shell's ~148).

The baseline provenance is the likely culprit, and it is genuinely murky: the comment stack chains the git_read-status measurement (18_727 = 18_605 + 122) *after* the search escape-hatch figures, but the git_read-status commit (37540e0) landed **before** the escape-hatch pin commit (6f363f8) — the stack was re-chained by a later plan, so the recorded 18_727 baseline is not verifiably apples-to-apples with the pre-sweep tree. The current figures (which the ceilings are built on) are internally consistent and the budget test — the actual guard — is reported green workspace-unified on the final tree, so the ceilings hold; this is a measurement-record accuracy issue, not a code bug. But a future raise computed off this mis-chained baseline would start from a wrong reference.

**Fix:** re-run `cargo test --workspace tools_array_stays_within_context_budget -- --nocapture` on the final tree and pin the printed figures in the comments (replacing the "~+700" estimate and the 18_727-baseline delta story with the actual printout, or annotating the baseline's provenance). If the printed Planning figure differs from 19_241, correct the comment; if it exceeds 19_500 the ceiling needs another look (it should not, per the reported green run).

## Constitution checks

- **Documentation sync:** the change is self-documenting schema text (the descriptions ARE the model-facing docs); no README/PLAN.md updates expected and none needed. The plan file and backlog bookkeeping are consistent with the implementation (all five steps match what landed). PASS.
- **Multi-platform neutrality:** no `cfg()`, no paths, no platform APIs in any change; the new tests are platform-neutral. PASS.
- **File-tools-first policy:** all edits are clean targeted changes (description appends, error-arm switches, test insertions) with no shell-mutation artifacts; `.coding/` changes are app bookkeeping (plan file, backlog status). PASS.
- **Security:** the error output includes received key *names* only (never values); hints are static strings; no new input handling or injection surface. PASS.
- **Tests before completion:** the author reports `cargo test --workspace` green (exit 0, unpiped per agent.md) including the budget test with the raised ceilings; the new test pairs pass and the pre-existing tests for the six tools are extended, not broken. PASS (per the reported run; the reviewer has no shell tool to independently re-execute — the LOW 1 recommendation includes re-confirming the budget printout).
