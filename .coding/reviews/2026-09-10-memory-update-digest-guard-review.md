## Verdict: FINDINGS (0 high, 2 low)

The digest-shape guard, regression test, and three git-history repairs are correct and fully verified (guard placement, edge cases, no-partial-write semantics, blast radius, byte-level repair verification against 1df6150/47c36ca); two low nits remain — the memory_update tool schema still describes the old contract, and the BUG record's created date contradicts the incident date in its own body.


# Review: memory_update digest-rewrite data loss — refuse digest-shaped body replacements (plan e8565822, backlog 63f882c9)

Scope: ALL uncommitted changes on wt/agenticcoding — src/memory/knowledge.rs (guard + `looks_like_row_digest` + doc comment), src/tool/memory/mod.rs (regression test), the three repaired knowledge files, .coding/backlog.jsonl (in_flight bookkeeping), .coding/plans/e8565822.md, and the untracked BUG record .coding/knowledge/bug/2027-01-07-memory-update-digest-rewrite-destroys-knowledge.md. Method: read the working-tree code, cross-checked every repair against git history (`git show 1df6150:<path>`, `47c36ca:<path>`), and traced the guard's blast radius via the code graph. No shell — test evidence taken from the plan's recorded runs.

## 1. The guard (src/memory/knowledge.rs:699-737, 759-775) — CORRECT

- **Placement**: after `parse_file`, before the title match and the `fs::write` — a refusal returns `Err` before any mutation. The tool layer (src/tool/memory/mod.rs:733-750) wraps it as `ToolResult::error("failed to update knowledge record: refused: …")` and runs `reindex` only on `Ok` — **no partial writes**: file AND derived row stay untouched (the test asserts both).
- **Shape**: strictly shorter (trimmed byte length) AND last non-empty line contains `.coding/knowledge/` and ends `.md` — exactly the incident signature; covers the observed tail conventions (`path …`, `Path: …`, `Full detail path …`, `File: …`).
- **Edge cases verified**: empty/whitespace-only candidate → `lines().rev().find()` → `None` → false (passes; a deliberate empty-body clear is not digest-shaped — accepted scope); CRLF → `str::lines()` splits `\r\n` and `.trim()` strips residual `\r`; equal length → `>=` → passes (the F1 "v3."→"v4." update is equal-length AND tail-less — safe on two independent grounds); empty current body → can never be strictly shorter → never fires; byte-length comparison is symmetric on both sides.
- **False positives** (a legitimate shorter body ending in a see-also knowledge link) are refusals — safe, no data loss, documented in the doc comment (759-765). False negatives (a digest without a pointer tail) pass — inherent to the heuristic, acknowledged in the plan ("false negatives are the danger"); the observed incident shape always carried the tail.
- **Error message** names `.coding/knowledge/{rel_path}`, states the invariant (the file is the truth), and gives both correct paths (full corrected body, or append a dated amendment) — contract-teaching as specified.
- **Blast radius**: `KnowledgeStore::update`'s only production caller is `MemoryUpdateTool::execute` (src/tool/memory/mod.rs:735); src-tauri only constructs the store (main.rs:1280). `write_at`'s callers (finish-capture, one-time migration) write their own idempotent slugs — no agent-driven clobber path. `memory_write` never overwrites (slug collision → `-2` suffix). `supersede` writes a NEW successor file; the predecessor keeps its body on disk (flipped to superseded) — the thin-successor pattern the plan asked me to sanity-check **destroys no data**; confirmed out of scope.

## 2. Regression test (src/tool/memory/mod.rs:1354-1420) — CORRECT, exercises the changed path

- Drives the exact incident path end-to-end: `memory_write` (knowledge-backed) creates a full 3-paragraph body → `memory_update` with a one-paragraph digest ending `path .coding/knowledge/{rel}` → asserts `!result.success`, "refused" in output, the file **byte-unchanged** (`assert_eq!(after, before)`), and the derived row unchanged.
- Control: a shorter replacement WITHOUT a pointer tail still succeeds and rewrites the file — the F1 legitimate path pinned in the same test.
- The pre-existing F1 test (:1338, "We use postgres for storage. v4." on the "v3." successor) passes the guard on two independent grounds — stays green (full-suite evidence: root 2159 + 16 doc-tests, src-tauri 293+4+2, 0 failed).
- RED→GREEN documented in the plan (ROOT CAUSE CONFIRMED note: pre-fix the test panicked at `!result.success` — the update succeeded and the file shrank).

## 3. The three repairs — verified against git history

- **spec/2026-12-21-multi-provider-prompt-caching…**: the restored 3-tier bullet and the "Documented in PLAN.md… Backlog #8814f81c done." closing line are **byte-identical to the 1df6150 original** (checked via `git show 1df6150:<path>`); 61fb662's legitimate additions (MERGED title/first line, the message-history-breakpoints bullet) are kept; the self-referential "Full detail path" tail is dropped. Surgical.
- **bug/2027-01-07-serving-layer…**: single-commit file (47c36ca); the only change is dropping the trailing `Path: .coding/knowledge/bug/2027-01-07-serving-layer-….md` self-pointer; the body is otherwise byte-identical to the committed version. No fuller prior body exists — correct repair.
- **decision/2026-12-21-message-toolcall…**: the old body was one paragraph + a pointer at the superseded 2026-12-20 predecessor (verified via `git show 1df6150:<path>`); the absorbed detail (the "After the thought_signature…" paragraph, all 7 constructor bullets, the convention, future-field, and ToolCall notes) is **word-for-word the predecessor's body**; the pointer tail is dropped; the predecessor file itself is untouched as superseded history and the `supersedes` front-matter chain is intact.

## 4. Constitution / bug-plan specifics

- **Doc comments**: `update()`'s doc comment documents the guard (pub item ✓); `looks_like_row_digest` is private and documented anyway.
- **Warning-free**: no new imports, no unused items, no `#[allow]`; green suites under `#![deny(warnings)]` prove it.
- **Multi-platform**: `std::fs` only; the `.coding/knowledge/` + `.md` string checks match the store's platform-independent rel-path convention (forward slashes everywhere, including on Windows). No platform-specific code.
- **Bug-plan specifics**: regression test exercises the changed path ✓; root cause documented (plan Context + ROOT CAUSE CONFIRMED) ✓; BUG memory written (knowledge file with symptom → root cause → fix + regression test name; semantic record recalled at spawn) ✓.
- **README:56 invariant** ("memory_write/memory_supersede/memory_update/memory_delete operate on the files (the truth), and the derived index holds budgeted digests … the file is complete") is unchanged and now enforced — not stale.

## Findings

### LOW 1 — memory_update tool schema still describes the old contract (doc sync)

src/tool/memory/mod.rs:682-686 — the schema reads "Use to correct or **tighten** a fact that is still CURRENT … A content change re-embeds; a title change re-classifies typed prefixes." For knowledge-backed records "tighten" now has a hard limit (digest-shaped content is refused) and the content replaces the truth-file body — neither is stated; "tighten" actively invites the condensed shape the guard refuses. The refusal error teaches the contract at failure time (the agent recovers in one turn), but a one-line addition to the `content` property's description (e.g. "for knowledge-backed records this replaces the truth file's full body — pass the full body or full body + dated amendment; digest-shaped content is refused") would prevent the failed round-trip entirely. The `update()` doc comment and the README invariant are fine; this is the one LLM-facing surface that still documents the old contract.

### LOW 2 — BUG record created-date contradicts the incident date in its own body

.coding/knowledge/bug/2027-01-07-memory-update-digest-rewrite-destroys-knowledge.md carries `created = "2027-01-07"` (filename + front matter) while its body dates the live incident and the fix to 2027-01-09 (as do the plan and the backlog item). If the record was authored in this plan's session (plan step 2, memory_write — it is untracked/new), the created date should be the session date; if it genuinely predates the plan (recorded 2027-01-07, incident 2027-01-09), the body's incident date is the anomaly. Cosmetic (indexer ordering only, no functional impact) — but one of the two dates is off; align them or note why not.

## Accepted-by-design observations (no action required)

- An empty-body `memory_update` (no pointer tail) still shrinks the file to empty — out of scope: the guard targets the digest shape (the incident), and a deliberate clear is not digest-shaped. Noted for awareness.
- A digest longer than the current body, or with the pointer mid-body, passes — inherent heuristic limits, acknowledged in the plan; the observed incident shape always carried a trailing pointer.
- Backlog 63f882c9 is marked in_flight with the plan id — correct mid-plan state; the app resolves it at finish.
