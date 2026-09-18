## Verdict: PASS

Round-2 verification of commit `28b6a42` (tip of `wt/mnemo`, plan a9875b73 "Tool-call robustness: repeat-failure circuit breaker + schema injection") against the three round-1 findings in `.coding/reviews/2026-09-18-repeat-failure-circuit-breaker-review.md` (FINDINGS 0 high / 3 low). The working tree is clean (`git diff HEAD` and `git status --short` both empty) and `git log` confirms `28b6a42` is the branch tip. All three findings are genuinely fixed at the code level; the fixes introduce no new issues (all three requested checks pass); the new regression test genuinely exercises the skip path; and the commit includes the round-1 report and all amended records. Two residual advisory notes (pre-existing/cosmetic, not counted — round-1's own framing for such items) are recorded at the end. Detail below.

## Finding-by-finding verification

### LOW 1 — auto-recall query hijack by the corrective message: FIXED

- **Shared const.** `HARNESS_CORRECTION_PREFIX` (src/agent/turn.rs:107-110) is `pub(crate)`, value `"[harness tool-call correction"`, with a doc comment stating it is shared by the correction builder and the auto-recall lookup.
- **Lookup.** `last_user_query` (turn.rs:3484-3493) returns the latest User-role message whose text does NOT start with the prefix. The skip is the only delta from the old inline `rev().find(|m| m.role == Role::User)`, so behavior is unchanged when no correction is present (additive filter; any message not prefix-starting passes exactly as before). Edge case: a history of only corrections returns `None` and recall is skipped — strictly better than the old behavior, which would have recalled against the correction text.
- **Call site.** `auto_recall` (turn.rs:2220) now calls `last_user_query(messages)`; the inline lookup is gone.
- **Comment.** The per-turn-cache comment at the lookup site (turn.rs:2212-2215) now reads "reads User-role messages only, and harness-attributed corrections are skipped in the lookup below" — matches the code.
- **Builder.** `tool_call_correction` (turn.rs:3501-3514) formats its prefix from the const: `"{HARNESS_CORRECTION_PREFIX} — not the user]\n..."` (line 3503).

**No drift path (requested check).** Builder and lookup both derive from the single const. A repo-wide literal search finds the prefix string nowhere in source except the const itself; the test suite pins it in six places (five `.contains("[harness tool-call correction")` assertions across the breaker tests — tests.rs:1430/1526/1555/1641/1711 — plus the skip test's hand-written correction at tests.rs:1735), so any change to the const without updating the tests fails the build: builder and lookup cannot silently diverge. I also checked the other user-role pushes in the turn loop for harness messages the skip ought to cover: the steer "User suggestion" message (turn.rs:2332) and the multimodal steer builders (turn.rs:1076-1079, 2089-2092) are genuine user input — correct as a recall query; the volatile-tail/CONTEXT_FOOTER pushes (turn.rs:2512-2518) are transient (pushed immediately before the request, popped right after — comment at 2506-2511) and only user-role for `tail_as_user` vendors, so they never sit in the persistent history when the query is built. The breaker's correction is the only harness-attributed user-role message, and it is covered on both sides.

**Byte-identity of the format! output (requested check).** The pre-fix builder existed only as uncommitted state at round-1 time (the entire change — original plus fixes — landed as the single commit `28b6a42`), so there is no git object to diff against; identity is verified against the two round-1 witnesses instead. The round-1 report quotes the correction prefix as `[harness tool-call correction — not the user]` and suggests `starts_with("[harness tool-call correction")`; plan step 1 specifies the same prefix. The new format string is `"{HARNESS_CORRECTION_PREFIX} — not the user]\n..."` — the const ends in `correction`, the remainder starts with ` — ` (one space, em-dash, space), so the concatenation reproduces `[harness tool-call correction — not the user]` byte-for-byte, and the message body (tool name, verbatim error, rewrite-the-COMPLETE-call instruction, schema) is unchanged — consistent with the round-1-verified assertions (`contains("file_read")`, `contains("[tool error]")`, `contains("parameters")`), which still hold against the new builder.

### LOW 2 — stale git.rs module doc: FIXED

The module doc (src/tool/agent/git.rs:50-52) now reads "The schema marks `subcommand` required, but the runtime still forgives an `action`-only call." — exactly the wording round-1 suggested, and now consistent with `"required": ["subcommand"]` in the schema (git.rs:505-506) and the untouched forgiving `resolve_git_subcommand` runtime path.

### LOW 3 — undelivered README.md update: RESOLVED AS RECORDED-MOOT

The plan record (`.coding/plans/a9875b73.md`, "Review round 1" paragraph) documents why README.md needs no change: it is the lean tour by design. Verified against the file: README.md:46 reads "The [feature reference](docs/FEATURES.md) has the exhaustive detail; this is the tour.", and README carries no git-tool section (round-1 already confirmed its only "git" occurrence is the clone URL). The actual doc deliverables are in the commit — docs/FEATURES.md (git schema/error bullet) and PLAN.md (error-recovery section). This is the reviewer's suggested alternative resolution ("amend the plan/step record to record why README needed no change"), so the finding is genuinely resolved, not merely dropped.

## The new test genuinely exercises the skip path (requested check c)

`last_user_query_skips_harness_corrections` (src/agent/tests.rs:1725-1752, `#[test]`) builds a three-message history whose LAST user message is a hand-written correction starting with the exact const value, and asserts `last_user_query` returns `Some("fix the login bug")` — that assertion fails if the skip is removed (the correction, as the latest User message, would win). The second assertion covers the unchanged-behavior direction: with no correction present, the latest user message wins (`Some("second prompt")`). The hand-written correction also pins the prefix the builder must produce, closing the loop between test, const, and builder.

## Commit contents (requested check d)

`28b6a42` includes, verified in the commit stat and read in full:

- the round-1 report (`.coding/reviews/2026-09-18-repeat-failure-circuit-breaker-review.md`, 53 lines) — content matches the round-1 findings as dispatched;
- the amended bug record `.coding/knowledge/bug/4347c8a3.md` — the "unmerged @ 9b65477" pointer corrected to landed `bb9d310`, consistent with `git log` (bb9d310 is the retune commit; 9b65477 is the round-2 report commit on top of it), and the regression tests now named (the "clampPanelFraction — the panel band after the 2027-01-16 retune" describe block in frontend/src/components/layout/RightPanel.width.test.tsx plus the clampPanelFraction block in frontend/src/hooks/appearance.test.ts);
- the amended decision record `.coding/knowledge/decision/2027-01-11-repeat-failure-circuit-breaker-history-scan-dete.md` (dating note + round-1 outcome);
- the HOW-record amendment (`.coding/knowledge/how/2026-08-26-git-tool-per-subcommand-parameter-reference.md`);
- the plan file `.coding/plans/a9875b73.md` with the round-1 paragraph.

## Test-matrix consistency (by inspection)

Round-1 stated root 2415 passed; the fix adds exactly one new test function (`last_user_query_skips_harness_corrections`) and modifies no other test — 2415 + 1 = 2416, matching the stated round-2 matrix. src-tauri stays 299: the commit touches no src-tauri files. (Read-only reviewer — cannot execute cargo test; consistency verified by inspection, as in round 1. Warning-free by inspection: `last_user_query` and the const are `pub(crate)` and used at non-test sites; doc comments present on both.)

## Residual advisory notes (pre-existing/cosmetic — NOT counted against plan a9875b73, mirroring round-1's separate-notes framing)

1. **TurnState field-doc phrasing.** The `last_recalled_user_query` field doc (turn.rs:184-192) still says the recall query is "just the latest User-role message text, which tool-result appends can never change." Its literal claim (tool-RESULT appends cannot change the query) was true before and remains true — round-1 cited it as evidence that the design assumption predates the breaker, not as a factual contradiction — and post-fix the stability it documents is restored (corrections are skipped). The operative comment at the lookup site (turn.rs:2212-2215) now documents the skip. Residue: the field doc could add the "skipping harness corrections" qualifier when next touched; not counted because the comment's literal assertions were never false and the fix restored the design it documents.
2. **4347c8a3 plan-file dangling section.** `.coding/plans/4347c8a3.md`'s "## Regression test" section (bare `clampPanelFraction`, no test identity) was committed as-is as a pre-existing artifact. The substantive test identity now lives in the amended bug record (which cross-references the plan file); completing or removing the dangling section remains the plan-owner's cleanup, exactly as round-1 framed it.

## Verdict recap

**PASS** — all three round-1 findings genuinely fixed (LOW-1: shared const + `last_user_query` skip wired into `auto_recall`, comment updated, regression test added; LOW-2: module doc corrected to the suggested wording; LOW-3: README recorded as moot-by-design in the plan record, verified against README.md:46). No new issues: unchanged lookup behavior without corrections, no builder/lookup drift path, byte-identical correction output. The commit carries the round-1 report and all amended records; the stated test matrix (2416 root / 299 src-tauri) is consistent by inspection. The two residual notes are pre-existing/cosmetic advisories outside this plan's findings.