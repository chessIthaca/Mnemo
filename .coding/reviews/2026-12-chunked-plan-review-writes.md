## Verdict: FINDINGS (0 high, 5 low)

# Review — plan 96e2601f: Chunked writes for review reports + long plans

Reviewed ALL uncommitted changes (`git diff HEAD` + untracked): `src/tool/agent/write_review_report.rs`, `src/tool/workflow/plan.rs`, `src/workflow/mod.rs`, `PLAN.md`, `.coding/backlog.jsonl`, plus untracked `.coding/plans/96e2601f-….md` and `.coding/knowledge/spec/2026-08-26-one-decimal-….md`.

No correctness, security, or contract-stability defects. All five findings are documentation/bookkeeping level. Fix them, re-run `cargo test`, and commit.

---

## Findings

### 1. LOW — `Workflow::update_plan` doc comment not updated for the new `append` parameter
`src/workflow/mod.rs:453-481` (public API doc comment)

The doc comment still describes only replace semantics: the `steps` bullet (464-468) says "when `Some`, it **replaces the remaining…** steps" with no mention of `append: true` adding entries after the remaining steps; the `title`/`goal`/`context` bullet (461-463) says they "replace the corresponding fields" with no mention of context extension; and the BugFixing-lock paragraph (478-481) still says "`steps` replacement is refused" while the code at 506-513 now refuses replacement **and** append. The project requires accurate doc comments on public functions, and the in-code comment at 503-505 already carries the new wording — the doc comment is the stale one.

**Fix:** in the doc comment, add an `append` bullet ("when `true`, `steps` entries are added AFTER the remaining steps instead of replacing them, and a non-empty `context` is extended as `{old}\n\n{new}`; default `false`") and update the lock paragraph to "steps replacement and append are refused".

### 2. LOW — `update_plan` tool description tail still says "steps cannot be replaced"
`src/tool/workflow/plan.rs:463-465`

The LLM-facing `ToolSchema` description reads "bug_fixing plans have a LOCKED skeleton (steps cannot be replaced)" — now inaccurate: append is refused too. The new `append` property description (492) does say "Refused for bug_fixing plans (locked skeleton)", so the schema is internally inconsistent rather than wrong.

**Fix:** change the tail to "(steps cannot be replaced or appended)".

### 3. LOW — no direct test for append to an existing 0-byte / whitespace-only file
`src/tool/agent/write_review_report.rs:230-245`

The create-path-on-append edge (file exists, `read_to_string` ok, `existing.trim().is_empty()` → falls to the `_ => None` create arm, verdict enforced on the chunk, file replaced) is exercised only via the missing-file variant (`append_creates_missing_file_and_enforces_verdict`). Same branch, so risk is minimal — but it's exactly the seam the checklist called out, and a pre-existing empty/whitespace file (e.g. from an interrupted earlier attempt) is a realistic input.

**Fix:** add a small case to the append test: pre-create the file with `""` (or `"\n\n"`), call append with a verdict-less chunk → rejected; with a verdict chunk → file contains exactly the chunk.

### 4. LOW — `backlog.jsonl` diff deletes three unrelated items beyond the described in_flight flip
`.coding/backlog.jsonl` (working-tree diff)

The change was described as "automatic bookkeeping only (item 0085ccc0 InFlight flip)", but the diff also **deletes** three other items: `e893d2b2` (pending, note says "Requeued"), `4f87731f` (pending), and `9042b47c` (failed, awaiting requeue). `backlog_list` confirms only `0085ccc0` remains live. This looks like a stale in-memory rewrite rather than an intentional resolution. The git union merge driver would likely resurrect the lines at merge to main, but committing the deletion hides/drops them on the working branch in the meantime.

**Fix:** before committing, restore the three lines (only the `0085ccc0` status flip should change vs HEAD) — or, if the deletion is intentional, note why in the commit message.

### 5. LOW — untracked `.coding/` side-car files should ride with the commit
Untracked: `.coding/plans/96e2601f-9607-4f12-9730-cafc6c26aece.md`, `.coding/knowledge/spec/2026-08-26-one-decimal-percentage-display-rule-fmtpct-every.md`

Per the branch policy `.coding/` is the mergeable side-car — plan files travel with git (past commits include them), and this plan's own file plus the review report belong in the closing commit. The knowledge note is a leftover from the previous fmtPct plan (commit `024ccef`) that apparently never got committed.

**Fix:** `git add` both files (and this review report) in the closing commit.

---

## Verified (no findings)

**write_review_report append semantics** (`src/tool/agent/write_review_report.rs`)
- Fail-closed holds on every file-creation path: overwrite always re-checks the verdict on incoming content (the `(append=true, existing non-empty)` arm is the only one that skips it); append-to-missing and append-to-empty/whitespace-only both fall to the create arm and enforce it. No path leaves a verdict-less report file.
- No verdict leak under repetition: appends only ever add to the tail; the head invariant ("file opens with a verdict") is checked before every append and preserved by it. A chunk carrying `## Verdict:` mid-file is inert: `finish`'s `parse_verdict` (`src/tool/workflow/plan.rs:823`) reads the **first non-blank line only**, so an appended verdict line can never flip or mask the real one.
- Defense-in-depth refusal for an existing verdict-less file is correct and leaves the file untouched (tested).
- Sandbox: `resolve_under_reviews` (absolute/sub-path/`..` rejection + canonicalized containment) runs before any read or write in both modes; invalid `mode` is rejected before filesystem side effects. Tested in append mode incl. `C:\Windows\evil.md` — rejected on all platforms via the backslash/bare-filename guard (multi-platform neutral).
- `\n` seam: `'\n'` added only when the existing body lacks one; the exact-concatenation assertion proves no double newline. Byte counts ("Appended N … now M") are UTF-8 byte lengths; the seam byte lands in M, not N — cosmetic only.

**update_plan append** (`src/workflow/mod.rs:482-613`)
- Completed prefix (`steps[..split]`) preserved verbatim; remaining steps kept via clone-extend; new steps appended; full re-index pass guarantees contiguous indices (tested).
- Out-of-order guard applies identically to append (refuses rather than risk ambiguity); append cannot drop a completed step in any state — combined = prefix + remaining + new, nothing discarded.
- bug_fixing lock is total: `steps.is_some()` refused for replace **and** append; context/title/goal/regression_test still allowed (context-append on a bug plan tested).
- Context seam `"{old}\n\n{new}"` with empty-existing → replace (no leading blank); double extension yields `a\n\nb\n\nc`. Noop detection unchanged (append=true with nothing provided still errors "changed nothing").
- Same plan id / still `Executing` (tested).

**Contract stability**
- Repo-wide search for `.update_plan(`: the only production caller is `src/tool/workflow/plan.rs:524` (updated, `args.append` passed in the correct position before `regression_test`); every other caller is a test in `src/workflow/mod.rs`, all updated. No callers in `tests/`, no fake in `tool/mod.rs` defines it. Signature change is fully propagated.
- Schema additions are modest and consistent with the compiled prompt's contract (the report FILE must open with a verdict — still true; `src/agent/prompt.rs:112` unchanged and not contradicted).

**Security / reviewer-only authorship**
- No visibility, filter, spawn, or sandbox changes; `write_review_report` remains reviewer-only, `.coding/reviews/` still protected from the file tools. Append cannot alter an existing verdict, and overwrite-of-an-existing-report is no new capability (was always possible). The main agent still cannot author a report.

**Tests (genuine)**
- 5 new write_review_report tests + 2 new tool-level + 3 new workflow-level tests. Without the feature, `mode`/`append` would be silently dropped (no `deny_unknown_fields`) → overwrite/replace behavior → the concatenation, append-after-remaining, and context-extension assertions all fail. They genuinely reproduce/lock the feature. Traversal/verdict/mode-rejection tests assert no file side effects. Reported green runs (1528 passed, +10) are consistent with the added tests.

**Docs**
- `PLAN.md` chunking paragraph, bug-lock sentence ("refuses steps replacement AND steps append"), and review-verdict chunked protocol paragraph all match the implementation, including "an existing report that lacks a verdict line is never grown". `README.md:56` and `agent.md` reviewer descriptions make no single-call claims and remain accurate. (The two stale doc spots found are findings 1 and 2.)

**Multi-platform neutrality** — no Windows-only APIs or paths added; the Windows-path string is a rejection test input valid on all platforms.
