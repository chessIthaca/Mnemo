## Verdict: FINDINGS (0 high, 1 low)

Round-2 verification for plan be16ea36 (file-tool reliability) at commit eb202c6 (HEAD on `wt/agenticcoding`, clean tree). All six round-1 findings are fixed correctly, and every behavioral fix is pinned by a test that fails without it; the one low is a stale word in a test comment left over from the L1 fix. Method: full `git show eb202c6` (stat + diff), then targeted reads of every fix site and its tests.

## Fix verification

### H1 — empty-anchor batch item panic: FIXED, pinned
- Guard: `validate_batch_args` (file_edit.rs:1036-1050) rejects any item with an empty `old_string` with a clean `InvalidInput` naming the 1-based index ("edit {N}/{M} has an empty old_string — every batch item needs an anchor …"). It runs first in `prepare_edit_batch` (:823), before any splicing.
- Defense in depth: `map_lf_region_to_original` (:1171-1177) gained the `m_end == 0` branch — `lf_offsets.get(m_start).copied().unwrap_or(content.len())` → `(pos, pos)`. This contains BOTH panic sites round-1 traced: the `lf_offsets[m_end - 1]` underflow and the empty-map `lf_offsets[m_start]` index (`.get()` + `unwrap_or`). It is unreachable via validated input (single mode rejects empty needles at :470, batch at :1041) — exactly what a backstop should be.
- Input shapes: `EditItem.old_string` is a required `String` (no `serde(default)`, :119) — a missing/null field fails deserialization cleanly; an explicit `""` hits the new guard. Only two `literal_splice` call sites exist (:775 single-literal, :843 batch), both guarded upstream.
- Tests: `multi_edit_batch_rejects_empty_anchor_with_clean_error` (:2697) drives `prepare_edit` — the exact path the underflow used to take (`count_matches` returns 0 for an empty needle → ambiguity guard passes → `find("")` = Some(0) → map(0,0)) — and asserts "empty old_string" + "edit 1/1"; RED without the fix (panic). `multi_edit_batch_empty_anchor_is_contained_on_the_approval_path` (:2719) calls `tool.approval_preview(...)` — the synchronous, uncontained path (`approval_preview` :1296-1298 maps Err→None via `.ok()` → `prepare_for_approval` :1408 → `prepare_edit` :1420) — and asserts `is_none()`; RED without the fix (panic on the agent-loop task).

### L1 — "Five modes" → "Six modes": FIXED
Schema (:1261) now reads "Six modes:" followed by exactly the six enumerated modes (LITERAL, REGEX, LINE-RANGE, FUZZY, BATCH, APPEND). No other stale count in shipped code — except the one-word comment echo that is this round's finding (below). Text-only fix with no pinning test (round-1 asked only for the count fix; the factory ceiling test pins the schema's char budget, not its wording) — acceptable for a docs-class defect.

### L2 — per-item emission checks: FIXED, pinned
- `validate_emission_artifacts` split into `validate_emission_artifacts_lines` (checks a/b/c scoped to ONE replacement text, :244-292) + `validate_emission_artifacts_braces` (check d, delta-scoped, :299-316); the wrapper (:226-234) preserves the single-edit path's behavior exactly — no regression there.
- `prepare_edit_batch` now runs the line checks PER ITEM (:872-874) plus ONE combined brace-delta check on (original content, final spliced content) (:877) — the right scope for a delta check. `replacement_join` is gone (zero matches in src/).
- Rejection stays pre-write, so batch atomicity is untouched.
- Test `multi_edit_batch_emission_checks_are_per_item` (:2739) pins both directions: (1) an item whose new_string is exactly "placeholder" is rejected even with a second item joining around it — under the join the trimmed text was "placeholder\nfn b2() {}", escaping the exact-match (RED without the fix: `unwrap_err` panics); (2) two items whose new_strings share a boundary "/// foo" doc line, separated by the untouched middle line, are accepted — the join made those lines adjacent (false positive) while the spliced result keeps "fn b() {}" between them, asserted via `contains("/// foo\nfn b() {}\n/// foo")` (RED without the fix: `unwrap` on the artifact error panics).

### L3 — successful file_write/file_append lifts the path's drift state: FIXED, pinned
- `observe_edit_freshness` (dispatch.rs) gained the `"file_write" | "file_append"` arm: on `result.success`, `steering.clear_edit_drift_for(agent_id, path)`; failed writes don't lift. Wired at all three result arms of the funnel, beside the other bookkeeping.
- `clear_edit_drift_for` (steering_stats.rs:681-685, doc comment) sits alongside the wholesale `clear_edit_drift` (:673); keying uses the same raw-path convention as `note_edit_drift`/`should_gate_file_edit`, so the clear always hits the armed entry.
- Test `file_write_success_clears_the_path_drift_state`: armed → `file_edit_redirect` Some → successful file_write → None (RED without the fix) → re-armed → failed file_write → still Some (pins the failure arm).

### L4 — shell mutation heuristic tightened: FIXED, pinned
- `>>`: the redirect target's first word is checked against `/dev/null`/`$null`/`nul` (command lowercased first, so `NUL` is covered); `null.txt` still counts. Slice safety: `idx + 2 <= len` is guaranteed by the two-char match.
- `python*`: token-wise, counts only when the next token is `-c` or a non-flag argument — `python --version`, bare `python`, `echo python` excluded; `python script.py`, `python -c`, `python3 -c` counted; `C:\Python39\tools.exe` excluded (token starts "c:"). `tee` stays token-wise ("committee" doesn't count).
- Docs: the Trace title says "… null-sink redirects excluded. Heuristic match — may over-count." and README says "null-sink redirects excluded; heuristic — may over-count".
- Tests: `shell_mutation_pattern_matches_the_mutation_vehicles` carries the three null-sink negatives, the null.txt positive, and the python negatives/positives.
- Residual heuristic edges (all observational, inside the documented "may over-count" envelope — no action): only the FIRST `>>` is examined (a null sink earlier in a compound command shadows a later real redirect — under-count); `starts_with("tee")` counts a word like "teeth" and misses `/usr/bin/tee`; `grep python f` counts (explicitly acknowledged in the code comment).

### L5 — degraded-config note: FIXED
factory.rs's `None` branch carries the NOTE comment recording the deliberate acceptance: memory_amend is file-only by construction, the branch has no knowledge-file backing, the compiled prompt names it unconditionally so the model may hit a clean "unknown tool" error in the legacy DB-only config — accepted deliberately, the error is self-explanatory. Exactly round-1's "accept and document" option.

## Commit hygiene

- Exactly the expected 16 files (the 15-file feature changeset + the round-1 review report); no `tmp_test*.txt` anywhere in the commit; working tree clean at HEAD = eb202c6.
- No `#[allow(...)]` added — the only `#[allow` hit in src/ is a prose mention inside a doc comment in loop_impl.rs:641 (pre-existing, not in this commit).
- Public fn doc comments: verified first-hand on every touched pub/pub(crate) item — `clear_edit_drift_for`, `should_gate_file_edit` (re-doc for the per-path signature), `note_edit_drift`/`note_edit_success`/`note_file_read`/`clear_edit_drift`, `is_edit_drift_failure`, `prepare_edit` (routing doc), `prepare_for_approval`, `file_edit_redirect`, `EditItem` + fields, `KnowledgeStore::amend`, `MemoryAmendTool::new`. The private helpers all carry doc comments too.
- Multi-platform neutrality: PASS — PathBuf/HashMap/string ops only; the `C:\Python39\tools.exe` string is a test negative; the null-sink names are heuristic match strings, not API usage.
- Test arithmetic is consistent: root lib 2190 → 2194 = exactly the four new tests (H1×2, L2×1, L3×1); vitest 77 files/1070 tests unchanged (the frontend delta is title text only) and tsc clean.

## Findings

### Low — factory.rs:1740: ceiling comment still says "the five-mode description"
The Executing ceiling-raise comment (added in this commit) attributes the schema growth to "the two new properties and the five-mode description" — but the schema says "Six modes" (the L1 fix); the comment echoes the pre-fix text and was not updated with it. Length-neutral ("Five"→"Six"), so the recorded 27_696 measurement is unaffected. One-word doc staleness in a test comment; fix the word. (The nearby Reviewing raise comment names no count, and no other shipped file does — the only other "five-mode" hits are the round-1 report itself and an unrelated `.coding/analysis/` doc about the approval architecture.)

## Notes (no action required)

- The dispatch.rs fn-level doc shorthand "any `python*` interpreter" (and the `python*` mentions in the types.ts / steering_stats.rs field docs) is looser than the tightened body rule; the precise semantics are stated in the in-body comment, the user-facing Trace title, and the README — defensible shorthand for the pattern family.
- The L4 residual edges listed above are within the documented heuristic envelope.
- Round-1's "Verified correct" spot-checks (EOL-agnostic core, batch atomicity, append mode, per-path gate wiring, memory_amend, IPC/frontend, docs sync) are unchanged by the fixes where untouched, and where the fixes did touch them (batch emission scope, gate lift semantics, heuristic) the re-verification above found no regressions.

## Summary for the parent

All six round-1 findings are correctly fixed and each behavioral fix is pinned by a test that fails without it; the commit is exactly the expected 16 files with no `#[allow]`, documented pub items, and no platform-specific APIs. One low: update "five-mode" → "six-mode" in the factory.rs:1740 ceiling comment (one word), re-run `cargo test` (comment-only change), and this is ready to finish.
