# Review: backlog #48 — LF/CRLF trip-hazards (majority-vote detection, file_edit dual-attempt fallback, new-file LF pin, .gitattributes eol=lf)

Branch `fix/line-endings`. Reviewed ALL uncommitted changes (`git status --short`, `git diff HEAD`):
`.gitattributes` (new), `src/tool/agent/line_endings.rs`, `src/tool/agent/file_edit.rs`,
`src/tool/agent/file_write.rs`, `src/tool/agent/file_append.rs`, `.coding/plans/stack.json` (bookkeeping),
`.coding/plans/a7f33226-*.md` (untracked bookkeeping).

**Verdict: the implementation is correct.** Three LOW findings (two wrong/stale comments, one
missing mirrored test). No correctness, security, or blocking bugs found.

---

## Findings

### LOW-1 (docs — factually wrong comment) — `src/tool/agent/file_edit.rs:788-789`
The comment in `mixed_file_needle_spanning_minority_lines_matches_via_fallback` says:
"CRLF majority (a,b) with a bare-LF tail (c,d,e)". The test's content
`"a\r\nb\r\nc\nd\ne\n"` is crlf=2 vs bare_lf=3 — an **LF majority** file whose needle spans
the *CRLF minority* head lines. The comment labels the majority backwards. (Consequence: the
author believed the CRLF-majority fallback direction was covered here — see LOW-3.)
**Fix:** correct the comment to "LF majority (c,d,e tail) with a CRLF head (a,b)".

### LOW-2 (docs — stale comment) — `src/tool/agent/line_endings.rs:100`
`detect_mixed_treats_as_crlf`'s comment still says "Any CRLF present → treat as CRLF
(dominant style)" — that is the OLD any-CRLF-wins rule this diff replaces. Under majority
vote, `"a\r\nb\nc"` is a 1-vs-1 tie that CRLF wins by the tie-break. **Fix:** update the
comment (e.g. "1 CRLF vs 1 bare LF → tie → CRLF (tie-break preserves the historical rule)").

### LOW-3 (test coverage — untested fallback direction) — `src/tool/agent/file_edit.rs:786-793`
The dual-attempt fallback is only exercised in ONE direction: LF-majority file, first attempt
with `le="\n"` misses, retry with `"\r\n"` hits. The mirror — CRLF-majority file, needle
spanning the bare-LF minority lines (first attempt `le="\r\n"` normalizes the needle to CRLF
and misses; retry with `"\n"` hits) — is not tested anywhere. Note
`mixed_majority_crlf_file_matches_crlf_old_string` does NOT cover it: its needle `"b\r\nc"`
matches on the FIRST attempt (present verbatim after CRLF normalization), no retry.
**Fix:** add a mirrored test, e.g. content `"a\r\nb\r\nc\r\nd\ne\nf\n"` (crlf=3, bare=2 → CRLF
majority), `old_string "d\ne"` → first attempt normalizes to `"d\r\ne"` (absent) → retry LF
matches; assert the edit lands (`"a\r\nb\r\nc\r\nD\nE\nf\n"` for new `"D\nE"`).

---

## Verified clean (focus points from the task)

### (a) Majority-vote `detect_line_ending` — correct
- Pure LF (`crlf=0`) → `"\n"`; pure CRLF (`bare_lf=0 ≤ crlf`) → `"\r\n"`; no-newline/`""` → `"\n"`; lone-`\r`-only → `"\n"` (same as before). No `usize` underflow: every `"\r\n"` occurrence contains exactly one `'\n'`, so `matches('\n').count() ≥ matches("\r\n").count()` always (`line_endings.rs:24-26`).
- Tie `"a\r\nb\nc"` (1v1) → CRLF → `detect_mixed_treats_as_crlf` stays green by tie-break, not by the old rule (comment stale — LOW-2).
- All pre-existing cross-style tests operate on **pure** files (`crlf_file_matches_lf_old_string`, `lf_file_matches_crlf_old_string`, `crlf_file_preserves_crlf_after_edit`, `crlf_file_replace_all_with_lf_strings`, `regex_crlf_file_lf_pattern`, `fuzzy_crlf_file`, `overwrite_*_preserves_*`, `append_*_normalizes`) — detection for pure files is byte-identical to before, so their semantics are preserved. The `file_write`/`file_append` tests use `detect_line_ending_path`, which is unchanged.
- Only callers of the string-based `detect_line_ending`: `file_edit` literal/regex wrappers + `prepare_edit_lines` (line-range). Line-range mode now joins mixed files with the majority style — a consistent improvement; unchanged for pure files. No other callers exist (verified by repo-wide search).

### (b) Dual-attempt fallback — correct, no loops, no wrong-region matches
- Retry fires ONLY on `Err(NotFound(_))` AND `args.old_string.contains('\n')`; it is a single tail call, not a loop (`file_edit.rs:331-337`, `463-469`). `InvalidInput` (identical strings, invalid regex, empty old_string, bad line bounds) never triggers a retry. Found-on-first-attempt returns the first result untouched → write-back style for pure files unchanged.
- The retry re-runs the identical deterministic matching with the other normalization; any hit is a genuine substring/regex match of the re-normalized needle, so it cannot produce a wrong-region match. `new_string` is re-normalized with the flipped `le` too, so the replacement matches the matched region's style (asserted by the fallback test).
- All literal branches surface `NotFound` on miss: count branch (`replaced == 0`), fuzzy branch (`file_edit.rs:447-451`), replace_all/no-change guard (`file_edit.rs:403-407`). The regex path maps both no-match and matched-but-no-change to `NotFound` (`file_edit.rs:492-496`), so the retry covers the plan's "produced no change" case exactly.

### (c) New-file LF pin — never fires for existing/empty files
- `Sandbox::validate_for_write` (`sandbox.rs:227-252`) creates parent DIRS only (step 4 `create_dir_all(parent)`) and never the file itself; the revalidate (step 5) does not create it either. So `!validated.exists()` is true iff the target file does not exist → the LF pin (`file_write.rs:101`, `file_append.rs:109`) cannot fire for an existing file.
- Empty existing file: `detect_line_ending_path` returns `None` (n==0, `line_endings.rs:54-56`) and `exists()` is true → falls through to `None => args.content.clone()` → `empty_existing_file_keeps_content_as_is` stays green.
- `file_write::prepare_for_approval` mirrors `execute` (`file_write.rs:141-147`): `validate` → `validate_for_creation` fallback performs no filesystem mutation, so the preview is pure AND shows the same LF-pinned content `execute` will write (no preview/write divergence).

### (d) .gitattributes — complete for this repo
- `* text=auto eol=lf` + binary guards. Verified against the tree: `src-tauri/icons/` contains 17 binary files (standard Tauri icon set — png/ico/icns, all guarded). No `.bat`/`.cmd`/`.ps1`/`.reg`/`.sln`/`.csproj` files exist anywhere (0 files matched those globs) — the classic eol=lf breakage class (cmd.exe batch labels) is absent. No `.wasm`/`.node`/`.so`/`.dylib`/archive/media/font files outside the guard list.
- SQLite: `.coding/memory.db`, `codegraph.db` + `-wal`/`-shm` are guarded (`*.db -text` etc.) AND already gitignored (`.gitignore:45-48`) — belt and braces.
- `text=auto` content-heuristic means even an unlisted binary would not be eol-converted; the guards add `-diff`/`-merge`. No `spike/`/`docs/` files exist to worry about. Adding `.gitattributes` mid-repo does not create phantom diffs (git compares post-normalization; renormalize was a no-op).

### (e) Constitution compliance
- Doc comments: `detect_line_ending` doc rewritten and accurate (tie/no-newline/majority all match the code); both new `*_with_le` bodies and both retry wrappers carry `///` docs. No public API changed (wrappers keep prior signatures).
- No `#[allow(...)]` anywhere in the diff; no new imports or dead code (static check — full `cargo test` under `#![deny(warnings)]` was green in-session per plan step 5: 1057 root / 67 src-tauri / 221 vitest / clean build).
- Regression tests per behavior change: majority vote ×3, fallback ×4 (one direction missing — LOW-3), new-file LF ×2 in `file_write` + ×1 in `file_append`.
- Replaced test `new_file_keeps_content_as_is` → justified, not gaming: its "CRLF content → CRLF file" half asserts the OLD contract this change deliberately reverses; the LF half is preserved verbatim in `new_file_lf_content_stays_verbatim` and the new contract is pinned by `new_file_written_with_lf`. The contract change is documented in both the code comment and the test comments.
- Bookkeeping verified: `stack.json` swaps the plan id to the active plan `a7f33226-…` (matches the untracked plan md) — expected, no source impact.

### Security — no findings
No new I/O or attack surface: the fallback only re-runs in-memory string matching; the LF pin
sits after `validate_for_write`'s protected-path refusal and before the write, and never
bypasses it (protected-file tests unchanged and still covering the ladder).

---

## Non-blocking notes (no fix required)

1. **Detector asymmetry (pre-existing, deliberate scope):** `detect_line_ending_path` (8 KB
   prefix, any-CRLF → CRLF) still detects a mostly-LF file with one stray CRLF in its first
   8 KB as CRLF, so `file_write` overwrite / `file_append` on such a mixed file normalize to
   CRLF while `file_edit` now reads it as LF. This heals the file to a single style (arguably
   desirable) and git's `text=auto` re-normalizes CRLF→LF in the index on commit, so no
   lasting harm. If mixed-file overwrites ever trip, aligning the path detector to
   majority-within-prefix is the follow-up.
2. **Regex `\n` escapes (pre-existing limitation, unchanged):** a pattern written with the
   two-char escape `\n` (backslash-n) contains no literal newline, so it is never
   line-ending-normalized and never triggers the flipped-ending retry — it still cannot match
   CRLF files. Literal newlines in patterns (and `\R`) work. Worth documenting in the schema
   only if models trip on it.
3. `eol=lf` flips Windows working-tree files to LF on next checkout of touched paths — that
   is the intended deterministic-LF policy, noted for team awareness.

## Required fixes before commit
LOW-1, LOW-2 (comment corrections) and LOW-3 (add the mirrored CRLF-majority fallback test).
All are small; re-run `cargo test` at the repo root after.
