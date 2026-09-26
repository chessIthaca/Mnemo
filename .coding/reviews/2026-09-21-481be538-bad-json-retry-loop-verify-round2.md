## Verdict: FINDINGS (0 high, 2 low)

Round-2 verification of the five round-1 remediations for plan 481be538 (backlog 38040f12) on wt/macos-fix, plus a full re-review of the uncommitted diff (7 modified files + 3 untracked) for issues the remediations introduced. All five remediations are landed; the mechanism, regression test, hardened assertions, and ceiling raises are correct. Two LOW documentation-consistency defects remain: the knowledge file still documents the parameter L3 dropped (incomplete remediation arm), and the Planning/Complete ceiling comments cite the round-1 measurement that this change-set's own round-2 growth (+330) has overtaken. Neither blocks; both are one-line repairs.

## Remediation verification (round-1 findings)

**L1 (wrong character-class example) — LANDED, correct.** turn.rs:3552-3553 now reads "for a literal `(?<`, write the character class [(][?][<]" — the correct metacharacter-free encoding (`[(]`+`[?]`+`[<]` matches exactly `(?<`; the string is also JSON-safe, no backslashes). The knowledge file (line 10) now says "metacharacter-free classes like [(][?][<]" — matches the code. The regression test pins it (`repeat_guidance.contains("[(][?][<]")`, tests.rs:~11413) with a comment correctly explaining that the old two-class form matches `(?(<`. No stale copy of the old two-class form remains anywhere (searched the tree: only the round-1 report and the test's explanatory comment, both intentional history).

**L2 (search_read parity) — LANDED, correct.** search_read.rs:116-118 gains the ESCAPE-HATCH note (same wording as search's, minus the search-specific inline example — appropriate) and the `literal` param description (line 130) is byte-identical to search's (search.rs:1222), including the RECOMMENDED wording. Placement is coherent: the note sits after the behavioral summary ("Build output and dependencies are always skipped.") and before "Returns a summary line…", each sentence self-contained — same insertion strategy as search's.

**L3 (unused parameter) — LANDED IN CODE; knowledge file not repaired (finding 1).** `fn bad_json_retry_message() -> String` (turn.rs:3514) takes no parameter; the call site (turn.rs:3351) and the doc comment (3508-3513) are updated; no other reference passes an argument (searched). But the knowledge file's fix paragraph still says "computes the first-failure text via bad_json_retry_message(tc)" — the exact per-call-computation implication round-1 L3 cited. The L1 find/replace repaired this same line's character class but left `(tc)` behind.

**L4 (plan-file bookkeeping) — LANDED, correct.** .coding/plans/481be538.md: Steps 4/4 ticked (lines 30-33) and Detailed steps 4/4 ticked (lines 36-39) — the contradictory crash-resumption state is gone. Detailed step 3(c)'s inline example landed in the search description (search.rs:1207-1208: "e.g. a search for the literal (?< is pattern (?< with literal true") — the "or" branch of the round-1 suggestion, so no deviation note is needed: the step is genuinely complete, not annotated around.

**L5 (PLAN.md doc sync) — LANDED, accurate.** PLAN.md:583-592, inside "### Error recovery", immediately after the circuit-breaker paragraph. Verified against the code: the per-tool formulations match the three match arms (search/search_read → literal/metacharacter-free; file_write/file_edit → chunking; default → rebuild-from-scratch); "rides the Tool role (context-only, no Error event)" matches the `Message::tool_result` push with no event emission (the pre-existing retrying Error event at turn.rs:3295-3303 is the generic batch announcement, not the correction); `MAX_BAD_JSON_RETRIES = 8` is untouched.

## Budget consequence (L2) — verified, with finding 2

The four new raises in factory.rs follow the file's documented measured+headroom+dated-cause pattern and are internally consistent. The round-2 growth is uniform +330 on every filter (search_read's note + RECOMMENDED param + search's inline example — search/search_read ride all six filters), which matches the comments' ~+330 estimate exactly on the two filters with round-1 measurements on record (Reviewing 28_319 → 28_649; Planning 18_275 → 18_605):

- Executing 33_200 → 33_900, measured 33_385 (headroom 515) — factory.rs:1859-1865
- PlanFrozen 34_500 → 35_200, measured 34_671 (529) — 1904-1909
- ExecutingResearch 27_300 → 28_000, measured 27_510 (490) — 1957-1962
- Reviewing 28_400 → 29_200, measured 28_649 (551) — 2019-2023

The test (factory.rs:2048-2058) asserts `chars <= ceiling` for all six filters; the parent's run reports all six under, Planning/Complete at 18_605/18_700, and the four measured figures above match the comments — but the Planning and Complete comments still say "measures … at 18_275 chars" (factory.rs:1773, 2044): the round-1 figures, stale by exactly the +330 this change-set documents for the other four filters. Finding 2. (I could not re-run the test myself — read-only reviewer; the 18_605 figure is the parent's reported output and is arithmetically exact: 18_275 + 330.)

## Test hardening (round-1 non-blocking suggestions) — LANDED, correct

`repeated_bad_json_for_same_tool_changes_strategy` (tests.rs:11285+) now also asserts:
- `retry_guidance[1].starts_with("[harness tool-call correction")` — pins harness attribution on the bad-JSON path; correct: `HARNESS_CORRECTION_PREFIX = "[harness tool-call correction"` (turn.rs:110) and `repeated_bad_json_correction`'s format! opens with it (turn.rs:3580).
- `retry_guidance[1] == retry_guidance[2]` — pins the documented idempotence; sound: the correction is a deterministic function of (tool_name, failed_content), both constant across repeats.
- the `[(][?][<]` pin (L1).

All three match actual code behavior; none is vacuous or over-constrained.

## New-issue sweep of the full diff

- **turn.rs retry arm** — scan-before-push ordering preserved (scan at 3352, push at 3357); the first-failure text is byte-identical to the removed inline string (compared segment-for-segment in the diff); the correction is Tool-role, context-only, embeds only harness-controlled strings; `bad_json_count` is untouched by the correction path, so the 8-strike backstop is intact. Round-1's PASS verification points (a)-(c), (g) remain valid — nothing in the remediations touched them.
- **search description example** — "e.g. a search for the literal (?< is pattern (?< with literal true" is telegraphic but unambiguous in context (immediately after "prefer literal:true (it avoids escaping)") and factually correct; see non-blocking note 1.
- **search_read note placement** — coherent (verified under L2 above).
- **.coding/backlog.jsonl** — app-managed status flip (pending → in_flight with the steer note); fine.
- **Untracked files** (knowledge file, plan file, round-1 report) — all expected, all reviewed.
- No stale reference to any changed string remains (old two-class form, old `(tc)` signature in code, old description text) — searched the tree.

## Findings

**1 (LOW) — Knowledge file still documents the dropped parameter: .coding/knowledge/bug/2027-01-11-bad-json-retry-loop-re-injected-identical-schema.md:10.** "handle_bad_json now computes the first-failure text via bad_json_retry_message(tc)" — the function takes no parameter (turn.rs:3514) and the message is a constant; `(tc)` is the exact per-call-computation implication round-1 L3 flagged, and the L1 find/replace repaired this same line's character class but left `(tc)` behind. Inconsistent with the code (verification check (c)). Fix: memory_update find/replace_with "bad_json_retry_message(tc)" → "bad_json_retry_message()" on the knowledge record (the sanctioned targeted repair, same path used for the L1 repair), and check whether the BUG memory row (407bd11a) carries the same phrase — it was find/replaced only for the character class; repair it in the same pass if so.

**2 (LOW) — Stale measured values in the Planning/Complete ceiling comments: src/agent/factory.rs:1769-1775 and 2042-2045.** Both comments say "measures Planning/Complete at 18_275 chars" — the round-1 measurements. The round-2 remediation (L2 parity + the inline example) added the same ~+330 the four raised filters' new comments document, and search/search_read ride Planning/Complete too: the budget test now prints 18_605 (parent's verification figure; 18_275 + 330 = 18_605 exactly). The comments understate the current measurement by 330 and overstate the available headroom ~4x (implied 425, actual 95) — the exact misjudgment the measured+headroom comments exist to prevent: the next ~100-char description addition to any Planning/Complete-carried tool overflows with no documented cause, and whoever investigates finds a comment claiming 425 chars of room. Fix: append a dated line to each comment in the file's stacked style (e.g. "18_700 holds (2027-02-05, round 2): the search/search_read ESCAPE-HATCH parity (~+330, same cause as the Executing raise) rides Planning too — measures 18_605, 95 under; no raise needed") or update the measured figure to 18_605.

## Non-blocking notes

1. The search description's example is compressed ("a search for the literal (?< is pattern (?< with literal true") — readable and factually correct, but "is" does light work; "e.g. to search for the literal (?<, pass pattern (?< with literal true" would be tighter. The turn.rs remedy's phrasing ("e.g. pattern \"(?<\" with literal true") is the better of the two; optional polish only.
2. The knowledge file's regression-test description (line 12) lists the original three assertions; the hardened test adds harness-attribution, idempotence, and the character-class pin. A true subset — not wrong — but one sentence would keep it exhaustive.
3. Pre-existing, not this diff: turn.rs:3333 `response_id: response_id.take(),` is misindented one level left inside the Message push (cosmetic; untouched by this change — noted only because it is adjacent to the reviewed arm).
