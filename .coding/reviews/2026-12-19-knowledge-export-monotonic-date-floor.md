## Verdict: FINDINGS (0 high, 2 low)

Review of ALL uncommitted changes on `wt/agenticcoder` for bug-fix plan c8b3228d "Knowledge-export dating: monotonic artifact floor beats stale wall clock" (backlog d7b2e722). The `src/memory/knowledge.rs` fix is correct, well-tested, well-documented, and multi-platform clean; regression value is genuine. The 2 findings are a small junk-guard gap that contradicts the fix's own stated behavior (LOW 1) and an unexplained backlog-entry reduction that needs an id-level sanity check before commit (LOW 2). Neither blocks.

**Changed files:** `src/memory/knowledge.rs` (the only source change), `.coding/backlog.jsonl` (d7b2e722 → in_flight, plus net −4 entries — see LOW 2); untracked: `.coding/knowledge/bug/2026-09-01-knowledge-exports-back-dated-by-stale-os-clock-f.md`, `.coding/plans/c8b3228d.md`.

## Verification notes (what was checked)

**Correctness of the floor logic.** `fmt_date` (Hinnant `civil_from_days`, knowledge.rs:812-825) emits fixed-width `%04d-%02d-%02d` — lexicographic max is valid ordering within the format, including across year boundaries (`2026-12-31` < `2027-01-02`). `stamped_date()` (555-561) is exactly `max(wall, floor)`. `artifact_date_floor()` (568-588) scans the four knowledge type dirs plus the sibling reviews dir; the ceiling `wall + 400*86_400` is i64-safe and the slack trade-off is sound: a stale clock at 2026-02-01 admits a real 2026-12-19 floor (ceiling ≈ 2027-03-07) while 2999 junk is rejected; conversely a clock >~13 months BEHIND the artifacts loses the floor — a deliberate bounded-trust choice, correctly documented in the doc comments ("~13 months"). A clock far-future-of-artifacts (the normal case) wins unchanged — tested.

**Lock interplay / cost.** `stamped_date` is called with the store mutex held at all three sites (write:610, write_at:662, supersede:740) and only does `read_dir` inside — no re-entrancy, no deadlock. Five small directory reads per write is negligible on a path that already does `create_dir_all` + `fs::write`. The date is computed once before the collision-suffix loop, and the `-2` suffix keeps the slug parseable by `date_from_slug`.

**Stamp-site sweep.** The only remaining `fmt_date((self.now)())` in the repo is inside `stamped_date` itself — no missed site. `update()` preserves `record.created` (706) as before. `write_at` keeps a slug-derived `created` when present (idempotency by design); both callers verified: `finish_capture.rs:176` passes a plan-id slug (no date → the floored fallback applies), `indexer.rs:1047` passes a slug dated from the memory row's `created_at` (data, not the clock — correctly out of scope; the floor must not rewrite migration identity).

**Edge cases.** Missing/unreadable/empty dirs contribute nothing (`scan_dir_max_date` returns on `read_dir` err — existence-guarded). `knowledge_dir` without a parent component yields `Some("")` → a CWD-relative `reviews` scan — read-only no-op if absent, and all four production constructors pass `<root>/.coding/knowledge` (factory.rs:243-247, tool/memory/mod.rs:1101, finish_capture.rs:626, src-tauri main.rs:1267), so the sibling resolution is right in practice.

**Security.** The scan is read-only: `read_dir` + `file_stem` parsing only; no writes into scanned dirs, no symlink-following writes, all paths joined from the store's own `knowledge_dir`. Far-future names cannot pin exports (ceiling).

**Regression value.** `write_dates_from_artifact_floor_when_clock_stale` (1032-1052) pins the clock to 2026-02-01, pre-creates `reviews/2026-12-19-levers-review.md`, and asserts both the filename date and `created = "2026-12-19"` — under the pre-fix clock-only stamping it produces `2026-02-01` and fails both asserts; the agent reports it was confirmed red pre-fix. Companion coverage is real: `clock_ahead_of_artifact_floor_wins` (epoch arithmetic independently re-checked: 1_769_904_000 = 2026-02-01 UTC, 1_799_107_200 = 2027-01-05, exactly 338 days), `supersede_successor_never_backdates_below_floor`, `malformed_floor_candidates_are_ignored`. The pre-existing `supersede_dates_the_successor_from_the_clock_not_the_predecessor` still holds (wall CLOCK+1d ≥ floor). Root cause + fix + regression-test name are documented in the BUG memory (ae5b6fec), the knowledge file, and the plan (c8b3228d.md records `knowledge::tests::write_dates_from_artifact_floor_when_clock_stale`).

**Multi-platform neutrality.** Pure `std::fs` / `std::time` / `std::path` — no `cfg(windows)`, no OS paths, no shell. Complies with the constitution's Windows/macOS rule.

**Docs sync.** Module doc gained the "Monotonic dates" invariant bullet (47-50); all four new private fns carry doc comments explaining the why (incident, ceiling, sibling). README/PLAN don't describe export dating, so no staleness. No public API changed.

**Tests.** Both suites reported green (root 1751+16, src-tauri 178+4); this reviewer is read-only and cannot re-execute — static verification is consistent with the claim. Under `#![deny(warnings)]` at both crate roots a green `cargo test` also proves zero warnings.

## Findings

### LOW 1 — `valid_floor_date` accepts impossible day-of-month; the fix's stated guard doesn't hold
`src/memory/knowledge.rs:437-441` checks month 1-12 and day 1-31 but not day-vs-month length: a stem like `2027-02-31-x.md` or `2027-04-31-x.md` (within the slack ceiling) passes and, if it is the newest artifact, every new export is stamped with a date that does not exist on any calendar (filename prefix + `created` front matter). This contradicts the fix's own stated behavior — "impossible month/day rejected" — and the guard's purpose (rejecting typo'd stems): `2026-13-99` is rejected while `2026-02-31` is not. Impact is cosmetic and self-limiting (requires a typo'd file to be the newest artifact; lexicographic ordering and dedupe still work), hence LOW.

**Fix:** validate day against month length in `valid_floor_date` (Feb 29 may be accepted unconditionally — a one-day harmless slack — or properly leap-checked; Apr/Jun/Sep/Nov ≤ 30 must be rejected). Extend `malformed_floor_candidates_are_ignored` with e.g. `2026-02-31-bad-day.md` (and ideally `2026-04-31-bad-day.md`) asserting they contribute nothing, per the constitution's regression-test rule.

### LOW 2 — backlog.jsonl diff removes 4 entries beyond the d7b2e722 flip; verify no live entry was dropped
The uncommitted `.coding/backlog.jsonl` delta is 8 lines at HEAD (58d48be) → 4 in the working tree: beyond flipping d7b2e722 to `in_flight`, four entries present at HEAD are gone. `git log -- .coding/backlog.jsonl` shows NO commit after 58d48be touched the file, so the removals are all riding uncommitted in this diff — most likely legitimate completed-item cleanup from the intermediate plans (their commits never included backlog bookkeeping). However, this file has a known clobber-history (BUG memory 707580e9: stale in-memory state persisted over committed entries).

**Fix (verification):** before committing, diff ids against `git show HEAD:.coding/backlog.jsonl` and confirm every removed id belongs to a completed plan (e.g. 86c90ff6 / 63cbc20f class items), none is live pending work dropped by a clobber recurrence. The 4 surviving entries (24e1c98e, 8b8f40d2, 21e23788 pending + d7b2e722 in_flight) are exactly the four 2026-12-06 review LOWs, which is consistent — but confirm the removed four, and remember d7b2e722 itself must flip to done/remove at finish per backlog hygiene.

## Observations (no action required)

- The 400-day ceiling means a clock >~13 months behind the artifacts loses the floor — deliberate bounded trust (unbounded slack would let far-future junk pin exports forever); documented in the fn docs and covered by `malformed_floor_candidates_are_ignored`.
- `knowledge_dir` without a parent component scans a CWD-relative `reviews` — read-only no-op; unreachable from the four production constructors.
- Known and accepted (per task brief, agreed): the BUG knowledge export itself stays 2026-09-01-dated (written by the pre-fix running binary; history is not rewritten). Note its stem is itself a 2026-09-01 floor candidate — dominated by any newer review date, so harmless.
- The bug file's front-matter `created`/filename will remain an odd 2026-09-01 artifact; superseding it later under a rebuilt binary would date it correctly via the floor.

## Commit checklist for the main agent

1. Fix LOW 1 (+ regression-test extension), re-run `cargo test` (root + src-tauri) — must stay green and warning-free.
2. Verify LOW 2 id-by-id, then commit to `wt/agenticcoder`: `src/memory/knowledge.rs`, `.coding/backlog.jsonl`, the untracked `.coding/plans/c8b3228d.md` and `.coding/knowledge/bug/2026-09-01-...f.md`, and this review report. Never `memory.db` / `codegraph.db` / `plans/stack.json` (gitignored caches).
3. `finish` with this report's path.
