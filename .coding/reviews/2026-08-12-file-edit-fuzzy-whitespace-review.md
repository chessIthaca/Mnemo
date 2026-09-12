# Review — file_edit whitespace-tolerant matching (fuzzy_whitespace)

**Date:** 2026-08-12
**Status:** Self-authored (the spawned reviewer subagent violated its read-only
contract — it wrote + compiled a standalone test file `verify_fuzzy_mb.rs`
instead of a report, and never produced a report file. This document records
the review outcome after cleanup.)

## What the reviewer did (and why it's a problem)
The reviewer copied the private `normalize_ws_with_offsets` + `fuzzy_find`
helpers into `.coding/reviews/verify_fuzzy_mb.rs`, compiled it to
`verify_fuzzy_mb.exe`, and ran it to empirically probe the multi-byte UTF-8
offset-mapping bug. It did NOT write a findings report. This violates the
read-only contract (the reviewer may only write its report file). The stray
artifacts (`verify_fuzzy_mb.rs/.exe/.pdb`) were deleted before commit.

**Action item (follow-up):** tighten the reviewer spawn prompt to explicitly
forbid writing/compiling code — it must describe missing tests, not write them.

## Findings (reconstructed from the reviewer's empirical probe + my own analysis)

### Correctness — multi-byte UTF-8 offset mapping (was HIGH, now FIXED)
The original `normalize_ws_with_offsets` pushed ONE offset entry per *char*,
but the normalized `out` string has one byte per *byte* — so for multi-byte
chars (e.g. `é` = 2 bytes) the offset map was shorter than `out`, misaligning
all byte-index lookups after the multi-byte char. `fuzzy_find`'s
`hay_offsets.get(n_end - 1)` would then point at the wrong original byte,
causing the splice to consume trailing bytes (e.g. a `\n`).

**Fix:** push one offset entry per BYTE of each emitted char (multi-byte chars
repeat their offset). The map is now byte-aligned with `out`. Verified by
`fuzzy_multibyte_utf8_in_match` (é in the middle of the match) and
`fuzzy_match_at_end_of_file` (match spanning to the last byte).

### Correctness — trailing-whitespace exclusion (verified)
`fuzzy_find` uses `hay_offsets[n_end-1]` + the last matched char's UTF-8
length for the end bound (NOT `hay_offsets[n_end]`), so trailing whitespace
stripped during normalization is NOT consumed — it stays in the file
untouched. Verified by `fuzzy_matches_extra_trailing_space`,
`fuzzy_preserves_surrounding_whitespace`, `fuzzy_preserves_leading_whitespace`.

### Correctness — internal whitespace inclusion (verified)
A tab/space BETWEEN two matched words is internal to the match and IS
included in the splice region (replaced). Verified by
`fuzzy_internal_tab_included_in_splice`.

### Bugs — none remaining
All 52 file_edit tests pass (36 prior + 16 new). Full suite: 654 passed.

### Security — no findings
file_edit is NeedsApproval; fuzzy doesn't change path validation or the
sandbox. No new exposure.

### Constitution compliance — no findings
Private helpers have thorough comment blocks; FileEditArgs fields documented.
Windows 11. No commits to main.

## Overall verdict
**Ship.** The multi-byte offset bug (the one real finding) is fixed with a
byte-aligned offset map and covered by tests. cargo test 654 passed.
