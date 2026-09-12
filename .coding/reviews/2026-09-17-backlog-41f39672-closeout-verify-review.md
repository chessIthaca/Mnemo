## Verdict: PASS

Round-2 verification of plan 162bd3ef (backlog 41f39672 close-out), scoped to the three round-1 findings (report `.coding/reviews/2026-09-17-backlog-41f39672-closeout-review.md`, FINDINGS 0 high / 3 low). Commit a887b27 fixes all three; the tree is clean (`git diff HEAD` and `git status --short` both empty, and a887b27 carries the round-1 report + plan-file flip). One non-blocking observation on a self-report count is noted at the end (not a finding — nothing committed is wrong).

## Finding 1 (substantive) — cap raised, dead const removed, comments corrected, new discriminating test

- **Const swap verified.** `SEARCH_CAP_CHARS = 6144` at src/tool/memory/retrieval.rs:33 with a doc comment (:24-32) that states the sizing rationale (worst-case default page 12 × ~360 chars ≈ 4.4 KB, matched-hits card parses exactly this row list). `CONTEXT_PACK_CAP_CHARS` is fully removed as live code — a crate-wide grep finds it only in the historical mention inside the successor doc (:31). The SEARCH branch truncates at `SEARCH_CAP_CHARS` (:220-223) with the "narrow the query" note riding the output.
- **"Stays capped" test updated.** `search_without_record_type_spans_all_classes_and_stays_capped` now asserts `<= SEARCH_CAP_CHARS + 128` (:332). (In that fixture the 12 returned rows are ~1.8 KB so the bound is not actively exercised, but the assertion now references the correct const and the new test below does the active work.)
- **Both frontend comments corrected in place.** agentEventReducer.ts:413-416 and Message.tsx:124-127 now state the true residual: the search-mode cap fits a full default page (limit 12), explicit larger limits may still truncate and the chip then counts more rows than the expansion offers. The chip-vs-rows residual is documented, not denied — the stronger of the three options round 1 offered, and it is sound.
- **Sizing math (stronger choice is sound).** `format_hit` (:48-57) caps *content* at 200 chars (titles uncapped, as before). Realistic worst case: 12 rows × (~100-char title + 36-char id + ~55 framing + 200 content) ≈ 4.7 KB — under 6144 with ~1.4 KB headroom; the doc's own 4.4 KB estimate is honest. The 6 KB ceiling is a fixed bound, ~3× the old 2048 but comparable to BROWSE mode's natural size (50 rows × ~120 ≈ 6 KB), so the context-budget concern stays bounded. A pathological multi-KB title could still exceed the cap, but that is pre-existing behavior and the truncation note still rides.
- **New test exists and genuinely discriminates.** `search_default_page_survives_untruncated` (retrieval.rs:338-391): 12 seeded rows × ~197-char content (under the 200-char per-hit content cap), default limit → asserts 12 `(id: ` matches and NO `[truncated` (:359-369); 30-row limit-30 store → asserts `[truncated` present (:384-390). Arithmetic on the pre-fix cap: 12 rows × ~270 chars ≈ 3.3 KB > 2048 → the old cap would cut at ~7 rows, so the first assert (12 `(id: `) *and* the second (no `[truncated`) both fail pre-fix — the test fails without the fix. The 30-row case ≈ 8.1 KB > 6144 → truncation note fires. Both directions pinned.
- **All 12 seeded rows rank.** `Memory::new` defaults `record_class = Authored` (src/memory/types.rs:245, pinned by :785); the scale backstop in `recall` caps only `MemoryClass::Derived` rows per record type (src/memory/mod.rs:1413-1428 — "Authored records are never capped"), so all 12 authored rows pass `retain`; every content contains the query token "storage" and the default limit is 12 with exactly 12 candidates → all 12 returned. The assertion's count of 12 is achievable, not aspirational.

## Finding 2 — knowledge file updated

`.coding/knowledge/how/2026-08-29-search-glob-shapes-extension-anchored-only-bare.md` now lists `walk_searchable_dir_scoped_recursive_braces` in its regression-test enumeration, with the stale-binary context noted ("a live 0-file scare turned out to be a stale app binary, not a code defect"). The HOW is again the complete pointer (round 1's three tests + the new fourth).

## Finding 3 — plan-file checkbox rides with the commit

`.coding/plans/162bd3ef.md` step 3's checkbox is flipped to `[x]` inside a887b27 (not a post-commit edit), and the tree is clean as verified above.

## Constitution / no new issues

- The new test has a doc comment; no `#[allow]` additions; the removed const leaves no dead code (grep clean) — warning-free claim consistent with the code as written.
- Cross-platform: the new test uses only the in-memory store and string asserts — no Windows-only paths or APIs.
- No README/PLAN.md change needed: the shipped behavior change is an internal output cap; the two corrected comments are the only user-facing text and they are accurate.
- Round-1 report and plan file are committed in a887b27, so the review trail travels with the branch.

## Non-blocking observation (not a finding)

The dispatch claims the retrieval suite ran "8/8", but `tool::memory::retrieval` contains **7** test functions (retrieval.rs:271, 301, 338, 393, 423, 443, 455 — grep-verified), and no other crate path contains "retrieval", so any `cargo test retrieval` filter yields 7. The full-suite arithmetic is consistent (1693 verified in round 1 + 1 new test = 1694). The 8-vs-7 slip exists only in the session self-report — no committed file states a test count — and the suite itself is internally consistent with the code, so this does not reopen any finding; the implementer may want to record 7/7 if the number is restated anywhere.
