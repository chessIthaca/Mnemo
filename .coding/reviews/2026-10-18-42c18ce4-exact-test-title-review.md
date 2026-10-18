## Verdict: FINDINGS (0 high, 2 low)

Summary: the implementation matches the plan frame (42c18ce4) and the task description in every acceptance criterion; the byte scan is precise and the escalation order holds. Two LOW findings — a containment-guard failure-path inconsistency vs the mirrored precedent, and an undocumented escape-handling contract — both cheap fixes, neither blocking correctness of the live case.

## What was read (scope verification)

- Uncommitted diff (base 8351520) via git_read: 4 files — the two code files (`src/codegraph/mod.rs` +276, `src/app/tool/workflow/plan.rs` +201) plus side-car churn (`.coding/backlog.jsonl`, `.coding/plans/c52bf713.md`) and the untracked plan frame `.coding/plans/42c18ce4.md` — matching the task's stated scope. No third code file touched.
- The plan frame `.coding/plans/42c18ce4.md` in full (30 lines).
- All new code read in the current files, not just the diff: `TestTitleHit` + `TEST_TITLE_CALLS` + `is_test_file` + `scan_exact_test_title` (mod.rs :168-288), `find_exact_test_title` (mod.rs :958-1009), the gate tier (plan.rs :2395-2534, incl. both updated error messages), and all six gate/store tests (plan.rs :6773-6924, mod.rs :1405-1504), plus the untouched neighbors `finish_passes_when_regression_test_is_in_non_indexed_language` (:6927) and `finish_force_reindexes_when_meta_fresh_but_symbols_missing` (:6733, verified present and outside the diff hunks).
- Precedents for comparison: `reindex_files_containing`'s walk + containment guard (mod.rs :900-956) and `contains_whole_identifier` (mod.rs :140-157).

## Acceptance criteria — verified against the code

1. **Exact title passes with the note** — gate arm plan.rs :2457-2494: `find_exact_test_title` in `spawn_blocking`, on hit `found = true` + note "…found as an exact it()/test()/describe() title literal in {path}; symbol check satisfied by the exact test-title literal". Gate test `finish_accepts_exact_vitest_it_title_as_regression_test` (:6774) records the exact live-case title against a `use_agent_store.test.ts` fixture and asserts success + the note + state Complete. Path traced end-to-end: title flanked by quotes → `contains_whole_identifier` match → forced reindex reparses → `matched_unparsed == 0`, `reparsed > 0` → title tier. ✓
2. **Suffix paraphrase rejects** — `finish_rejects_string_titled_test_suffix_paraphrase` (:6817): the recorded suffix-composed name is NOT contiguous on disk (the fixture splices `it("<title>" + " — …")`), so it routes to the **absent-error** arm and rejects with the both-forms hint. The store-level twin (:1448) pins the same rejection at the scan. ✓
3. **Partial title rejects** — `finish_rejects_partial_string_titled_test_title` (:6854): the partial title IS a word-bounded substring of the on-disk title, so it reaches the final-else hard reject and the scan correctly refuses it (the byte after the needle is a space, not the closing quote). This test is the one exercising the updated hard-reject message :2496-2514. ✓
4. **Comment-only mention rejects** — `finish_rejects_comment_only_title_mention` (:6888): comment containment reaches the final else (matched>0, reparsed>0), the scan finds no call, hard reject with hint. ✓
5. **Unparsable tier unchanged** — `finish_passes_when_regression_test_is_in_non_indexed_language` (:6927) read verbatim: the arm (:2417-2439) and its note are byte-identical in style and untouched; the test still asserts "disk containment". ✓
6. **Both error paths name both forms** — absent-error :2407-2415 ("…test function name OR the exact it()/test()/describe() title literal for a string-titled test") and hard reject :2496-2514 (same both-forms phrasing). Every reject test asserts the phrase. ✓
7. **Escalation order** — the title tier sits strictly INSIDE the post-reindex outcome chain (:2457), after the unparsable tier (:2417) and the reparse-inconclusive arm (:2440); the forced re-index runs at :2370-2398 before any of it. The doc comment states the contract. ✓

## Risk-focus verification

- **(a) Scan precision** — word-start check before AND after the call name (`submit(`/`testify(` both rejected, pinned in mod.rs test :1495-1503); quote must match opener; terminator `,`/`)` required after the closing quote so concatenation rejects; length guard prevents overflow. Multi-line titles work (whitespace skip is before the open paren, the title bytes are raw — a real newline in a template literal matches a recorded newline). Python concern is moot at the gate: `.py` files never reach the title tier (unparsed → the `matched_unparsed` tier fires first). Escaped quotes → LOW-2.
- **(b) is_test_file** — `login.test.ts` ✓ (`.test.`), `src/main.ts` ✗ (component `src` ≠ `tests`, no suffix), `tests/late_delta.ts` ✓ (component `tests`); `foo_tests.ts`/`Foo.Spec.ts` covered by case-insensitive `_tests.`/`.spec.`; backslashes normalized before both checks (multi-platform safe). ✓
- **(c) spawn_blocking capture** — `graph.clone()` (Arc by value) + `test.to_string()` moved in; no borrows escape the closure. ✓
- **(d) Unreadable-file skip** — `let Ok(bytes) = fs::read(...) else { continue; }` (:994-996), mirroring the precedent. ✓
- **(e) Zero `#[allow]`** — none in any added region read; the only repo `#[allow(clippy::…)]` hits are pre-existing in `src/agent/failure_triage.rs`, untouched by this diff. ✓
- **(f) Path separators** — store test normalizes `\\` → `/` before the `ends_with` comparison (:1439). ✓

## Findings

### LOW-1 — containment guard is lenient on canonicalize failure (mod.rs :998-1004)
The comment claims "mirroring reindex_files_containing", but the precedent **skips** the file when `path.canonicalize()` fails (`let Ok(canon) = … else { eprintln!; continue; }`, :921-924), while the new code uses `if let Ok(canon)` and — on failure — falls through and **returns the hit anyway**. Practically harmless (the path came from `walk_searchable(&self.root)`, so it is inside the root by construction), but the mirror claim is inaccurate on exactly the failure path the guard exists for. Fix: skip (`continue`) on canonicalize failure like the precedent, or document why leniency is safe here.

### LOW-2 — the no-unescaping contract is not documented (mod.rs :958-1009 doc comment)
The scan is byte-exact with no escape unescaping: `it("says \"hi\"")` can only pass if the recorded name carries the raw escaped bytes (`says \"hi\"`), not the runtime title (`says "hi"`); the same holds for template-literal `${…}` interpolation. That is a defensible conservative contract (the both-forms error lets the agent self-correct in one shot by recording the literal as written), but the doc comment on `find_exact_test_title` — and the gate's error message, which says "the exact … title literal" without saying *as written in source, escapes included* — never states it. One sentence in the doc comment (and optionally "title literal as written" in the error text) closes the gap.

## Constitution checks

- **Documentation sync** — the plan frame documents the disk-walk-vs-store-hint deviation; both new pub items (`TestTitleHit`, `find_exact_test_title`) and both private helpers carry doc comments stating what accepts and what rejects. The gate's user-facing error/note text is self-documenting; no README/PLAN.md entry is warranted for a gate-internal acceptance tier — no finding.
- **Multi-platform neutrality** — pure byte scanning + `std::fs` + `PathBuf`; backslash normalization before path-component checks; no Windows-only API. ✓
- **File-tools-first** — no shell-based mutation in the diff. Test fixtures use `std::fs::write`, the established harness pattern. ✓
- **Warning-free build** — no `#[allow(...)]` added anywhere in the two files. ✓
- **Bookkeeping accuracy** — plan frame 42c18ce4 matches the shipped code exactly (steps 1-4 checked, step 5 open, the deviation documented in context :14); backlog.jsonl/c52bf713 lines are status churn consistent with the work. One line each, as required.
- **Regression tests** — the six new tests directly pin the defect class (the exact live-case title now passes where plan 3d40e529 was double-rejected) and the three rejection shapes; the red-first claim is recorded in step 1 of the frame.

## Test-run caveat

As a read-only reviewer I could not execute `cargo test --workspace` / `tests/boundary.rs`; the claimed "3092 passed; 0 failed" and "84 passed; boundary_exit=0" are the dispatching agent's step-4 record. The code paths traced above are consistent with those claims, and the harness patterns (tempdir fixtures, `open_in_memory`, `bug_plan_to_reviewing`) match the green precedents beside them.

Reviewed-state: 8351520445de202d1543f23cba914f4a7be4dc98
