## Verdict: FINDINGS (1 high, 5 low)

Review of ALL uncommitted changes on `wt/agenticcoding` (15 modified files, no untracked) for plan be16ea36 "File-tool reliability" — 10/10 steps complete. Method: full `git diff HEAD` read, then targeted full-file reads of the changed cores (`file_edit.rs` matching/splice/batch/append + execute path, `line_endings.rs`, `dispatch.rs` funnel + gates, `steering_stats.rs`, `knowledge.rs::amend`, `memory/mod.rs` KnowledgeSupport, `factory.rs` registration, prompt/agent.md, frontend, docs).

Overall: the EOL-agnostic matching core, the per-(agent, path) stale-read gate, batch/append modes, `memory_amend`, the constitution changes, and the mutation-vehicle counters are correctly implemented, well-documented, and pinned by focused regression tests (including the two original bugs: false drift and gate freeze). One reachable panic from model-controlled input (uncontained on the approval path) must be fixed before commit; the five lows are quality/heuristic issues.

## High

### H1 — `file_edit` batch item with empty `old_string` panics (model-controlled input; uncontained on the approval path)

A multi-edit batch item whose `old_string` is `""` (and `new_string` non-empty) reaches a subtraction-underflow panic in the new EOL-agnostic core:

- `validate_batch_args` (file_edit.rs:952-1009) validates only the top-level single-edit fields — it never checks the items' `old_string` emptiness.
- `count_matches` (file_edit.rs:1014-1019) explicitly returns 0 for an empty needle, so the ambiguity guard (`occurrences > 1`) passes.
- In `literal_splice` (file_edit.rs:672-731): `old_lf = ""` ≠ `new_lf` → the identical-strings guard passes; then `content_lf[lf_pos..].find("")` returns `Some(0)` (an empty needle matches at position 0), yielding `(m_start, m_end) = (0, 0)`.
- `map_lf_region_to_original` (file_edit.rs:1122-1136) then executes `let last = lf_offsets[m_end - 1];` with `m_end == 0` → `0usize - 1` → **panic** ("attempt to subtract with overflow" in debug; wrapped index out-of-bounds in release). An empty `content` with an empty anchor panics one line earlier (`lf_offsets[m_start]` on an empty map).

Repro (model-controlled, plausible mistake — the model may try to "insert" via an empty anchor):
```json
{"path": "a.txt", "edits": [{"old_string": "", "new_string": "x"}]}
```

Blast radius — two paths, only one contained:
- **Approval path (default safety mode): UNCONTAINED.** `file_edit` is `NeedsApproval`, so `execute_tool_call` calls `tool.approval_preview(&parsed_call.arguments)` (dispatch.rs:376) → `prepare_for_approval` (file_edit.rs:1358) → `prepare_edit` — synchronously on the agent-loop task, no `spawn_blocking`, no `catch_unwind`. The panic unwinds the agent session mid-turn.
- Execute path (autonomous mode / post-approval): the panic happens inside `tokio::task::spawn_blocking` and is contained by `.unwrap_or_else(|e| ToolResult::error(...))` on the JoinHandle (file_edit.rs:1346-1347) — but the result is an unactionable "file edit task failed: task … panicked" error, and the drift gate never arms (a panic is not a drift-class result).

Note the inconsistency: the single-edit path guards exactly this input with a clean error (`prepare_edit`, file_edit.rs:443-449: "old_string is empty — provide old_string (string matching) or start_line + end_line (line range)"), and its comment explains why an empty needle is dangerous. The batch path forgot the per-item equivalent. No test covers an empty-anchor batch item (all batch tests use non-empty anchors), which is why the green suite didn't catch it.

Fix: reject empty `old_string` per item in `validate_batch_args` (or at the top of the item loop in `prepare_edit_batch`), mirroring the single-mode guard; optionally also make `map_lf_region_to_original` defensive for `m_end == 0`. Per project rules, add a regression test: a batch containing an empty-`old_string` item returns a clean `InvalidInput` naming the item index (must fail/panic without the fix, pass with it) — via both `prepare_edit` and the tool's `approval_preview`/`execute`.

## Low

### L1 — `file_edit` schema description says "Five modes" but lists six

file_edit.rs:1211-1218: "Five modes: LITERAL (default, first occurrence), REGEX (use_regex), LINE-RANGE (…), FUZZY (…), BATCH (edits — …), and APPEND (append — …)" — that is six enumerated modes after the word "Five" (the pre-change text correctly said "Four matching modes" for four). User-facing schema text consumed by the model; fix the count (or fold FUZZY into LITERAL explicitly, as the old text's "literal only" parenthetical implied).

### L2 — Batch emission validation via `replacement_join` weakens/mis-fires two artifact checks

`prepare_edit_batch` (file_edit.rs:797-804, 849) passes every item's `new_string` joined with `"\n"` to `validate_emission_artifacts` as the "replacement text", so the line-scoped checks see the join instead of per-item text:

- **Sentinel whole-values (check b, :253-261) escapes for multi-item batches**: a single-item batch with `new_string` `"unused"` is correctly rejected (join == "unused"), but add any second item and the join is longer than the bare sentinel, so an item whose replacement is exactly `unused`/`placeholder` passes — single mode would reject the same edit.
- **Duplicated adjacent doc lines (check c, :263-277) can false-positive across the join boundary**: item N's `new_string` ending with `/// foo` and item N+1's starting with `/// foo` produces an adjacent identical pair in the join that does not exist in the spliced result (they land at different file locations), rejecting a legitimate batch with a confusing emission-artifact error.

Suggested fix: run the line-scoped checks (a/b/c) per item's `new_string` (plus the combined-content brace-delta check (d), which is correctly delta-scoped as-is). Tests: a two-item batch containing a sentinel-shaped item is rejected; a batch whose items' new_strings share a boundary doc line is accepted.

### L3 — The stale-read gate does not reset on a successful `file_write`/`file_append` of the same path

`observe_edit_freshness` (dispatch.rs) resets drift state only on a successful `file_edit`. But the tool ecosystem explicitly steers drift-failure recovery to `file_write` full-file replacement (the emission-artifact error text and the new prompt bullet both recommend it). Sequence: `file_edit(P)` drift-fails → gate armed for P → agent recovers by rewriting P with `file_write` (its knowledge of P is now fresh by construction) → the next targeted `file_edit(P)` is still intercepted with "re-read the file" — a false positive costing one round-trip (safe, but the gate is policing a path the agent demonstrably knows). Consider resetting the (agent, path) drift entry on successful `file_write`/`file_append` of that path too. Plan step 3 only specified the file_edit reset, so this is a hardening suggestion, not a spec violation.

### L4 — `shell_command_mutates_files` over-counts vs the plan's spec (observability only)

Plan step 9 specified "python with a script file or -c writing files"; the implementation (dispatch.rs) counts ANY token starting with `python` — so `python --version`, `python -c "print(1)"`, `echo python`, and `grep python file` all increment `shell_mutations`. Likewise `lower.contains(">>")` counts `… >> /dev/null` (a null sink the app itself treats as non-mutating — it has a separate fired-only redirection warning for exactly that shape). Purely observational (Trace counter, no gating) and documented as a heuristic, but it inflates the shell-mutation side of the file-tools-first signal the panel exists to display. Tightening (e.g. require an argument after `python`/`python3`, exclude `/dev/null`/`$null`/`nul` redirect targets) would make the counter honest; at minimum note the over-count in the frontend title text, which currently reads as exact.

### L5 — `memory_amend` is named unconditionally in the compiled prompt but registered only when the knowledge backing is wired

prompt.rs's stable head (MEMORY RECORDS hygiene line, asserted by the head test) tells every session that `memory_amend` exists, but `register_memory_tools` (factory.rs:1196-1226) registers it only in the `Some(knowledge)` branch — the `None` branch (plans dir implying no root, the historical DB-only config) registers update/supersede/delete but not amend (it is file-only by construction). In that degraded config the model is instructed to use a tool that does not exist (clean "unknown tool" error, so impact is small). Either accept and document the degraded-config gap, or have the tool's absence degrade gracefully (e.g. register a stub that explains the config) — flagging so the choice is deliberate.

## Verified correct (spot-checks beyond the findings)

- **EOL-agnostic core** (`normalize_lf_with_offsets` / `map_lf_region_to_original` / `literal_splice`): traced CRLF pairs (image tagged with the `\n` byte, leading-`\n` matches extend back across the `\r`, trailing-`\n` matches include the whole pair), lone `\r`, multi-byte UTF-8 (offset map repeats per byte — byte-index lookups stay aligned), and char-boundary safety of every splice slice. The mixed-EOL tests (minority-span needle both directions, replace_all across styles, fuzzy with interior trailing whitespace before CRLF) pin the majority-style re-emission and byte-exact untouched regions — all hand-verified against the implementation.
- **Batch atomicity**: all anchoring/splicing happens in-memory; the single `std::fs::write` runs only after every item succeeded, so any failing item leaves the file byte-identical (pinned by `multi_edit_batch_is_atomic_on_failure`). Sequential in-order application is pinned by the item-2-anchors-on-item-1-output test; ambiguity guard + `count` widening verified; error messages name the 1-based index and anchor excerpt.
- **Append mode**: EOL prefix logic correct (no spurious blank line when the file already ends with a newline; empty file takes the text verbatim); missing file steers to `file_write` (NotFound arm in execute, no drift marker — correct, not drift-class); mutual exclusivity complete against every other mode/knob.
- **Per-path gate**: drift state recorded directly at all three result arms of `dispatch_with_interrupt`; intercepted calls return before the arms, so the gate never feeds itself; the read set (`read_files` per-path, `search_read` wholesale) matches the old `EditStaleRead` switch targets (`["read_files", "search_read"]`, steering_stats.rs:161) — no narrowing regression vs the old gate; the freeze regression (interleaved non-read call) and the success-resets semantics are both pinned by tests. Global marker telemetry untouched.
- **`memory_amend`**: `rel_for_id` restricts to Derived-class rows with a knowledge `rel_path` (prefix-checked) — the id resolves through the store, so no path traversal from model input; superseded records and empty paragraphs refused; append-only by construction (digest guard cannot fire); targeted reindex after write; registered only in the with-knowledge branch with a documenting comment. The row's first-line digest staying stable after an amendment is by design (pointer-first indexer) and documented in the test — the file is the truth.
- **IPC/frontend**: `get_steering_stats` returns the full `SteeringStatsSnapshot` struct (src-tauri/src/ipc/trace.rs:128), so the new `file_tool_mutations`/`shell_mutations` fields serialize through; frontend types (snake_case) match; the Trace span renders only when a counter is nonzero, with an accurate title.
- **Docs sync**: README (memory_amend in the hygiene list, per-path gate semantics, EOL-agnostic/batch/append capabilities, Trace counters), PLAN.md (terminology + the new technical-decision bullet), agent.md (File mutation policy section, reviewer bullet, "the things below" fix), module docs (file_edit header, memory mod doc) — all match the shipped behavior as read in code. The `.coding/plans/be16ea36.md` step checkboxes are updated.

## Constitution compliance

- **Multi-platform neutrality**: PASS — no Windows-only APIs, paths, or shell syntax in library/app code; the PowerShell cmdlet names in `shell_command_mutates_files` are heuristic match strings, not API usage; `PathBuf`/`std::fs` usage is cross-platform; the `C:\Python39\tools.exe` string is test-only.
- **File-tools-first policy**: PASS — the change itself contains no shell-based file mutation; `std::fs::write` in tests is test code (explicitly exempt).
- **Public fn doc comments**: PASS — all new `pub`/`pub(crate)` items (`EditItem`, `MemoryAmendTool::new`, `KnowledgeStore::amend`, `note_edit_drift`/`note_edit_success`/`note_file_read`/`clear_edit_drift`, `is_edit_drift_failure`, `should_gate_file_edit` re-doc, etc.) are documented.
- **No `#[allow(...)]`**: PASS — none added anywhere in the diff.
- **Regression tests per defect**: both original bugs (false drift, gate freeze) have RED-then-green regression tests; H1 above is a NEW defect introduced by this change and needs its own regression test with the fix.
- **Verification**: the parent reports root crate 2190 passed / src-tauri 293+4+2 / vitest 77 files 1070 tests / tsc clean, zero warnings under `#![deny(warnings)]`. As the read-only reviewer I could not re-run cargo; the code-level review above is consistent with those claims (no test covers the H1 input, so a green suite is expected).

## Notes (no action required)

- The mutation counter counts `file_edit`/`file_write`/`file_append` as the file-tool vehicle; plan step 9's text named `memory_amend` instead. Code, frontend title, and README are internally consistent (memory_amend is sanctioned bookkeeping, not a policed file-tool mutation), so this reads as a deliberate, documented deviation from the plan wording — recorded here for the history.
- `note_file_read` fires even for failed `read_files` results (paths taken from args) — self-correcting: the next edit on a still-mismatched path re-arms the gate; negligible.
- Gate path keys compare as raw strings/PathBuf (no case folding); the interception message echoes the exact path string, so the prescribed re-read always clears the entry — acceptable for a heuristic gate.
- The stable-head change invalidating the prompt cache next session start is intended per plan step 8.

## Summary for the parent

Fix H1 (empty-anchor batch item panic — guard per-item `old_string` in `validate_batch_args` + regression test) before committing; L1-L5 are quality items fix-now-or-ticket. Everything else in the plan's scope — both bug fixes, batch/append modes, memory_amend, constitution encoding, observability, docs — is correct and well-tested.
