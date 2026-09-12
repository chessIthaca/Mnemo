# Review: `read_files` (plural) batch read tool

**Date:** 2026-08-12
**Scope:** All uncommitted changes in `C:\myHarness` working tree.
**Files reviewed:**
- `src/tool/agent/read_files.rs` (new)
- `src/tool/agent/mod.rs` (module registration + doc)
- `src/agent/factory.rs` (registration + expected-names test)
- `src/agent/prompt.rs` (guidance line)
- Bookkeeping: `.coding/plans/*`, `.coding/reviews/2026-08-12-file-edit-fuzzy-whitespace-review.md` (skimmed only — plan/stack state, no code concerns)

## Correctness

**Per-file error isolation — VERIFIED CORRECT.** `read_one` (read_files.rs:148) returns an inline error string for both sandbox-validation failure (line 152) and read failure (line 158); it never propagates. The `execute` loop (line 128) pushes whatever `read_one` returns, so one bad path cannot blank out the others. The `path_traversal_rejected` test (line 307) correctly asserts `result.success == true` and checks for the inline `"path validation failed"` substring — it does **not** expect the call to fail. ✓

**Sandbox validation on every spec — VERIFIED CORRECT.** `sandbox.validate(Path::new(&spec.path))` (line 149) is called inside the per-spec loop for every spec unconditionally; no spec can bypass it. The `Sandbox::validate` impl (sandbox.rs:78) canonicalizes existing paths (catching `..` and symlinks) and rejects non-existent parents. ✓

**Line-number correctness — VERIFIED CORRECT.** Line 176: `format!("{:>4}: {}", start + i + 1, line)` where `start = start_line.unwrap_or(1).saturating_sub(1)` (0-indexed offset) and `i` is the post-`skip`/`take` enumerate index. `start + i + 1` yields the original 1-indexed file position. The `line_numbers_reflect_original_positions` test (line 328) confirms lines 3–4 are emitted as `   3: three` / `   4: four` with `start_line: 3`. ✓

**MAX_FILES enforcement — VERIFIED CORRECT.** Empty array → error (line 115); `len() > MAX_FILES` → error (line 118). Both tested (lines 286, 295). ✓

**AutoRun safety — VERIFIED APPROPRIATE.** `SafetyLevel::AutoRun` (line 106). The tool only performs `std::fs::read_to_string` (read-only); no mutation. Matches `file_read`'s safety classification and the `ToolCategory::Agent => safety == AutoRun` rule in tool/mod.rs:196. ✓

**Truncation-note logic — VERIFIED to mirror file_read.** The `default_capped` / `truncated_by_bytes` note logic (read_files.rs:190–199) is byte-for-byte equivalent to file_read.rs:146–155: the note fires only when the *default* line cap was the limiting factor (`max_lines.is_none() && showed < available_from_start`) or when the byte cap bit. An explicit caller range produces no note. ✓

**`showed == 0` header — VERIFIED clean.** When the range is empty (e.g. `start_line` beyond EOF), `showed == 0` → header `"=== {path} (empty range) ==="` (line 204) and `body` is `""`, yielding `"{header}\n"` — no malformed output, no spurious truncation note (because `available_from_start` is 0 when `start >= total_lines`, so `default_capped` is false). ✓

**`start_line: 0` handling — graceful, consistent.** `0u.saturating_sub(1) == 0` → reads from line 1, same as file_read. Not a bug. ✓

**Factory test balance — VERIFIED.** `read_files` was added to both the registration list (factory.rs:356) and the `expected` array (factory.rs:787), so the `registry.iter().count() == expected.len()` assertion (factory.rs:812) still balances. ✓

No correctness findings.

## Bugs

### B1 (Medium) — Total byte-cap `String::truncate` can panic on multi-byte UTF-8, failing the whole call
**File:** `src/tool/agent/read_files.rs:135`
```
output.truncate(TOTAL_BYTE_CAP);
```
`String::truncate` **panics** if the new length does not lie on a UTF-8 char boundary. The joined output is the concatenation of up to 10 sections (each ≤ ~100 KB), so it can realistically exceed the 512 KB `TOTAL_BYTE_CAP`. If the 512000th byte lands in the middle of a multi-byte character (CJK, emoji, accented letters — common in source comments/strings), this panics **inside `spawn_blocking`**. The panic surfaces as a `JoinError`, caught by `.unwrap_or_else` at line 141, so the **entire** `read_files` call returns `ToolResult::error("read_files task failed: …")` — every file's output is lost, not just one. This is newly-introduced code (no equivalent in `file_read`) and it is reachable.

**Fix (prose):** Before truncating, back the cut point up to the nearest UTF-8 char boundary, exactly as the existing helper `truncate_raw_stream` in `src/provider/openai.rs:632` already does (`while end > 0 && !s.is_char_boundary(end) { end -= 1 }`). Apply the same boundary-safe truncation at line 135. (See B2 for the per-file equivalent.)

### B2 (Low) — Per-file byte-cap `String::truncate` has the same latent panic risk (mirrored from `file_read`)
**File:** `src/tool/agent/read_files.rs:184`
```
body.truncate(DEFAULT_MAX_BYTES);
```
Same `String::truncate` char-boundary panic as B1, but this line is a verbatim mirror of `file_read.rs:140`, so it is a **pre-existing** latent issue rather than a regression. It is reachable when a single file's numbered body exceeds 100 KB with multi-byte content at the boundary. Because the task explicitly asked to verify the truncation logic mirrors `file_read`, this is noted for completeness: the mirror is faithful, including the latent panic.

**Fix (prose):** Use the same char-boundary-safe truncation as recommended in B1 for line 184. Ideally fix both `file_read` and `read_files` together (and consider extracting a shared `truncate_to_boundary(s: &mut String, max: usize)` helper) so the pattern isn't duplicated a third time.

## Security

No findings. Every spec is routed through `Sandbox::validate` (canonicalization-based, rejects `..`/symlink escape). No new path exposure, no absolute-path shortcut, no mutation. `read_to_string` is read-only. Sandbox confinement is uniform across all specs.

## Constitution compliance

No findings.
- Public items documented: `ReadFilesTool` struct (line 53) and `ReadFilesTool::new` (line 59) have doc comments; module-level `//!` doc present (lines 1–7). ✓
- Windows 11 / PowerShell: uses `std::path::Path` and `std::fs`; no hardcoded separators, no bash syntax. ✓
- No commits to `main`: this is uncommitted working-tree code; no merge/push performed. ✓
- Code style matches existing `file_read.rs` (same consts, same formatting, same test helper shape). ✓

## Missing tests (described in prose — no test code written)

The existing 8 tests cover the happy path, per-file ranges, error isolation, empty/over-limit arrays, traversal, line-number positions, and the header range. The following cases are **not** exercised and should be added:

1. **Per-file byte-cap truncation.** A single spec whose content is one very long line (~150 KB, exceeding `DEFAULT_MAX_BYTES`). Assert the section body is truncated to ≤ 100 KB and contains the `"... (truncated: … lines total, showed 1)"` note. (Mirrors `file_read`'s `truncates_very_long_lines_by_bytes`.) This would also exercise B2.
2. **Total byte-cap truncation.** Multiple specs whose joined output exceeds `TOTAL_BYTE_CAP` (e.g. 6 files × ~100 KB each). Assert the final output is truncated and ends with `"... (truncated: total output exceeded size limit)"`. This exercises the line-134–137 path and B1.
3. **Default line-cap truncation note.** A single spec with no `max_lines` against a >2000-line file. Assert the `"... (truncated: 3000 lines total, showed 2000)"` note appears. (Mirrors `file_read`'s `truncates_large_file_by_line_count`.)
4. **Empty-range header (`showed == 0`).** A spec with `start_line` beyond the file's end (e.g. 5-line file, `start_line: 99`). Assert the section header is `"=== <path> (empty range) ==="` and no panic / no spurious truncation note.
5. **`start_line: 0` and `max_lines: 0` edge inputs.** Assert `start_line: 0` is treated as line 1 (reads from the top) and `max_lines: 0` yields the empty-range header without error.
6. **Multi-byte UTF-8 content under caps.** A file with CJK/emoji content large enough to hit either byte cap. Assert no panic and valid UTF-8 output. This is the regression test for B1/B2.

## Overall verdict

**Fix-then-ship.** The core design is sound and faithfully mirrors `file_read`: per-file error isolation is correct, sandbox validation is uniform, line numbers and caps are right, and AutoRun is appropriate. The one blocker is **B1** — the newly-introduced total byte-cap `String::truncate` (read_files.rs:135) can panic on multi-byte UTF-8 and take down the entire call, which is reachable with realistic source files. B2 is the same latent issue mirrored from `file_read` (pre-existing, low). Both are quick fixes using the char-boundary-backup pattern already present in `src/provider/openai.rs:632`. Add the missing cap/edge tests (items 1–6) alongside the fix. Once B1 is resolved and tests pass, this is safe to ship.
