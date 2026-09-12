## Verdict: FINDINGS (0 high, 1 low)

Verification review of commit `52e07e1` on `feat/llm-tool-error-handling`. The
prior review (`.coding/reviews/2026-08-25-llm-tool-error-handling-review.md`)
found 4 low-severity findings. I verified each fix against the committed changes
(`git show 52e07e1`) and the current working tree (`git diff HEAD`,
`git status --short`). **3 of 4 fixes are correct and complete; Low 1 is NOT
fixed** — the on-disk HOW knowledge file is still stale. The core source-code
correctness (re-confirmed below) is sound.

---

### Fix verification

#### Low 1 — Stale HOW knowledge file — NOT FIXED

**File:** `.coding/knowledge/how/2026-08-25-three-strikes-error-caps-tool-card-error-display.md:6-9`

The task states the fix was applied "via memory_update" and asks to "verify the
on-disk file now reflects 4 caps and the correct increment sites." The on-disk
file does NOT. It is byte-identical to the stale version committed in `52e07e1`
(the commit added it as a *new* file with the old content). It still reads:

- Line 6: `Three "three-strikes" caps exist; do not conflate:` (should be four).
- Line 7: `MAX_RETRIES=3` … `Incremented in TWO places: (a) turn.rs:~1146
  has_bad_json block … (b) turn.rs:~1405 tool-execution failure`. After this
  change, `tool_error_count`/`MAX_RETRIES` is incremented in **ONE** place only
  — the tool-execution failure; the `has_bad_json` block now increments
  `bad_json_count` toward `MAX_BAD_JSON_RETRIES`.
- Lines 8-9: only 3 caps listed (`MAX_RETRIES`, `MAX_PROVIDER_TURN_ATTEMPTS`,
  `DOOM_ERROR_STREAK`); there is **no 4th entry for `MAX_BAD_JSON_RETRIES=8`**.

The derived DB digest agrees it is stale: `memory_search` for the HOW record
returns the digest "Three 'three-strikes' caps exist; do not conflate" —
matching the stale file, not a corrected one.

Why the `memory_update` approach did not land on disk: `MemoryUpdateTool::execute`
(`src/tool/memory/mod.rs:663-712`) routes a file-backed knowledge record through
`KnowledgeStore::update` (`src/memory/knowledge.rs:597-632`), which rewrites the
file via `std::fs::write` and reindexes — its success message is "file
.coding/knowledge/{rel} rewritten; the memory row re-indexed". For that path to
fire, the supplied `id` must resolve to the record's file via `rel_for_id`. Since
neither the file nor the DB digest changed, the update did not take effect on
this record (wrong/no id, or it fell through to the DB-only path and was
re-derived from the unchanged file). `git status` confirms the HOW file is
unmodified relative to the commit, so no post-commit edit occurred either.

The file is also internally inconsistent as shipped: line 11 already states
"bad-JSON (LLM) errors should NOT count toward the 3-strike abort," yet line 7
still lists the `has_bad_json` block as a `MAX_RETRIES` increment site. The
source-code doc comments in `src/agent/mod.rs:36-59` ARE correct; only the
knowledge file is stale.

**Fix:** rewrite the on-disk file (via `memory_update` with the record's correct
file-backed id, or a direct file edit) so cap #1 says `tool_error_count` is
incremented in ONE place (tool-execution failure, `result.success==false` unless
`is_user_denial_tool_output`), and add a 4th cap entry for `MAX_BAD_JSON_RETRIES=8`
(bad-JSON, incremented in the `has_bad_json` block at `turn.rs:1157`, resets on
valid JSON at `turn.rs:1232`, aborts at `turn.rs:1158`). Severity: low
(documentation/knowledge sync; no functional impact — the code is correct).

#### Low 2 — toolErrorSummary doc/caller mismatch — FIXED

**File:** `frontend/src/components/chat/Message.tsx:598`

The caller guard now reads
`{failed && lastCall.result && toolErrorSummary(lastCall.result.output) !== "" && (...)}`.
When the summary is empty (empty/whitespace-only output) the guard short-circuits
to false and the `<div>` is not rendered — honoring the `toolErrorSummary` doc
contract ("the caller hides the summary line when empty",
`frontend/src/lib/toolCardPaths.ts:188`). ✓

#### Low 3 — Stray unrelated changes — FIXED

`git status --short` shows only ` M .coding/plans/028e39cf-…md` (the step-5
checkbox flip — legitimate bookkeeping). Neither webview-context-menu spec file
(`.coding/knowledge/spec/2026-08-24-suppress-webview-context-menu-in-mnemo-ui-except.md`
nor the `-2.md` duplicate) appears in the working tree or in commit `52e07e1`.
The commit contains only the LLM tool-error handling source changes + legitimate
bookkeeping (plan, prior review, the HOW knowledge file, PLAN.md). ✓

#### Low 4 — PLAN.md does not mention the new bad-JSON cap — FIXED

**File:** `PLAN.md:258-264`

The Error Recovery section now documents both caps: `MAX_RETRIES = 3`
(tool-execution failures) and `MAX_BAD_JSON_RETRIES = 8` (malformed tool-call
arguments), with the rationale "the model can self-correct by emitting valid
JSON — three strikes does not make sense for an error the model can recover
from." ✓

---

### Core correctness (re-confirmed)

**1. `bad_json_count` separation — PASS.** `src/agent/turn.rs`:
- Declared at line 115 alongside `tool_error_count`; a per-`run_turn` local
  (cannot leak across turns).
- Incremented ONLY inside the `has_bad_json` block (line 1157); that block
  `continue`s at line 1228, so it skips the reset.
- Reset to 0 at line 1232, reached only when `has_bad_json == false` (valid
  JSON), placed after the block's closing brace and before the
  `tool_calls.is_empty()` check (line 1233).
- Never touches `tool_error_count`: the old `tool_error_count += 1` in the
  bad-JSON block was replaced by `bad_json_count += 1`. `tool_error_count` is
  still incremented only in the tool-execution path (lines 1413-1419: reset on
  `result.success`, skip denials, else increment) and aborts at line 1696 with
  `MAX_RETRIES`. The two counters are fully independent. ✓

**2. `max_retries_aborts_after_consecutive_tool_errors` — PASS.**
`src/agent/tests.rs:1130`: emits `file_read` with VALID JSON args
`{"path":"does_not_exist.txt"}` (so `has_bad_json == false`); the tool executes
and returns `success:false` for the missing file — a genuine tool-execution
failure, not bad JSON. Asserts `saw_final_error`, `!saw_finished`, and
`tool_msgs.len() == 3` (the post-batch cap at `turn.rs:1696` fires after 3
results are pushed). ✓

**3. `canMerge` guard — PASS.** `frontend/src/hooks/agentEventReducer.ts:444-455`:
`lastCallFailed = lastCall !== undefined && lastCall.result !== null &&
!lastCall.result.success`. `result === null` (parallel/running) →
`lastCallFailed = false` → merges; success → merges; completed error →
`lastCallFailed = true` → breaks the chain. Covered by the two new vitest cases
(failed → 2 cards; success → 1 card w/ 2 calls). ✓

**4. `toolErrorSummary` edge cases — PASS.**
`frontend/src/lib/toolCardPaths.ts:195-204` (`ERROR_SUMMARY_MAX = 120`):
empty/whitespace → `find` returns `undefined` → `""`; exactly 120 chars →
`length <= 120` → verbatim, no ellipsis; >120 → `slice(0,119).trimEnd() + "…"`
(120 total). All 6 table tests match. ✓

**5. Tests.** I could not re-execute `cargo test` / `npm test` / `tsc` (read-only
reviewer, no shell). The code changes are internally consistent with the claimed
1484 Rust + 550 vitest + tsc-clean results, and the test logic (counter
separation, both caps, merge break, summary edge cases) is correct by reading.

---

### Constitution compliance

- **Doc comments on public items:** `MAX_BAD_JSON_RETRIES`
  (`src/agent/mod.rs:49-59`) and `toolErrorSummary` (`toolCardPaths.ts:181-194`)
  both documented; `MAX_RETRIES` doc clarified to count tool-EXECUTION failures.
  ✓
- **No `#[allow(...)]`** introduced in the diff. ✓
- **Multi-platform neutrality:** pure Rust logic + TypeScript; no Windows-only
  APIs, paths, or shell syntax. ✓
- **Regression tests** added for every behavior change (3 Rust + 2 TS merge + 6
  summary table tests). ✓
- **Branch policy:** changes on `feat/llm-tool-error-handling`, not `main`. ✓
- **Documentation sync:** PLAN.md updated (Low 4 ✓); the HOW knowledge file is
  the one outstanding doc gap (Low 1 ✗).

---

### Finding

#### Low 1 (still open) — HOW knowledge file not updated on disk

`.coding/knowledge/how/2026-08-25-three-strikes-error-caps-tool-card-error-display.md:6-9`
still lists "Three" caps, still says `MAX_RETRIES` is "Incremented in TWO places"
(including the `has_bad_json` block), and has no `MAX_BAD_JSON_RETRIES=8` entry.
The `memory_update` fix did not take effect on disk (the file is unmodified vs.
the commit; the DB digest is likewise stale). Rewrite the file so cap #1 reflects
a single increment site (tool-execution failure) and a 4th cap entry for
`MAX_BAD_JSON_RETRIES=8` is added. No functional impact — the source code and its
doc comments are already correct.
