# Review: file_edit line-range mode (start_line / end_line)

**Date:** 2026-08-12
**Reviewer:** read-only subagent (spawned review)
**Scope:** ALL uncommitted changes in the working tree (`git diff HEAD` + untracked plan file).
**Files changed:**
- `src/tool/agent/file_edit.rs` (feature + tests)
- `src/agent/prompt.rs` (guidance line)
- `.coding/plans/stack.json`, `.coding/plans/c75501aa-*.md` (bookkeeping — skim only)
**Build status:** ✅ `cargo test --lib file_edit` → 36 passed / 0 failed.

---

## Correctness

### C1 — Empty-needle literal edit silently corrupts the whole file (HIGH)

**File:** `src/tool/agent/file_edit.rs:27-28` (the `#[serde(default)]` on `old_string`) combined with `:314` (`"required": ["path", "new_string"]`) and `:195-234` (`prepare_edit_literal`).

Making `old_string` optional (default `""`) and dropping it from `required` opens a previously-unreachable code path. A call that supplies **only** `path` + `new_string` (no `old_string`, no `start_line`) now deserializes successfully and dispatches to `prepare_edit_literal` with `old_string == ""`.

Trace:
- `prepare_edit_literal:200` → `old_string = normalize_line_endings("", le)` = `""`.
- `:203` `old_string == new_string` → `"" == "X"` → false, so the identical-strings guard does **not** fire.
- `:208-234` → `content.replace("", &new_string)`. Rust's `str::replace` with an **empty needle inserts the replacement between every character and at both ends**: `"abc".replace("","X")` == `"XaXbXcX"`.
- `:235` `new_content == content` → false (it changed), so the no-change guard does not fire.
- Result: the edit **succeeds and writes to disk**, injecting `new_string` between every character of the file. Silent, surprising, effectively data corruption. `file_edit` is `NeedsApproval` so a human sees the diff — but the diff is enormous and the failure mode is unintuitive.

Before this change, `required: ["path","old_string","new_string"]` made serde **reject** any call missing `old_string`, so the empty-needle path was unreachable. The change removed that guard without adding an equivalent one in `prepare_edit_literal`.

**Fix (pick one):**
1. **Preferred** — guard the literal path explicitly at the top of `prepare_edit_literal`:
   ```rust
   if args.old_string.is_empty() && !args.use_regex {
       return Err(crate::error::Error::InvalidInput(
           "old_string is empty — provide old_string (string matching) or start_line+end_line (line range)".into(),
       ));
   }
   ```
   (The regex path is unaffected: `regex::Regex::new("")` compiles to a zero-width pattern and `replacen` would also misbehave, so guard the regex path too, or guard once in `prepare_edit` before dispatch.)
2. Alternatively, keep `old_string` required at the schema level **unless** `start_line`/`end_line` are present — but JSON schema can't express that conditional, so the runtime guard in (1) is the robust fix.

A test should be added: `prepare_edit_literal_empty_old_string_errors` asserting the call errors rather than corrupting.

### C2 — Trailing newline in `new_string` doubles up (LOW / by-design, but undocumented)

**File:** `src/tool/agent/file_edit.rs:166-178`.

If `new_string` itself ends with a line terminator (e.g. `"X\n"`) and the file had a trailing newline, the result contains an extra blank line. Trace for replacing line 2 of `"a\nb\nc\n"` with `"X\n"`:
- parts = `["a", "X\n", "c"]`, `join("\n")` = `"a\nX\n\nc"`, + trailing `"\n"` → `"a\nX\n\nc\n"`.

This is arguably correct (the caller asked for a trailing newline in the replacement), and the literal path has the same property (it inserts `new_string` verbatim). But because line-range mode is the *recommended* path going forward, the surprising case is worth a one-line note in the doc comment / prompt guidance: "`new_string` replaces the whole line range verbatim — do not include a trailing line terminator unless you intend an extra blank line." Not blocking.

### C3 — Single-empty-line file `"\n"` works correctly (NO finding — verified)

`"\n".lines()` → `[""]` (len 1). `start_line=1,end_line=1` → `start_idx=0`, `end_idx=0`, `start_idx >= len` false. parts=`[new_string]`, join, +trailing `"\n"`. Replacing line 1 of `"\n"` with `"X"` → `"X\n"`. Correct. No fix needed.

### C4 — CRLF trailing-newline preservation correct (NO finding — verified)

`"a\r\n".ends_with('\n')` is true (the LF half), so `le="\r\n"` is appended. The `prepare_edit_lines_crlf_preserved` test confirms `"a\r\nb\r\nc\r\nd\r\n"` → `"a\r\nX\r\nY\r\nd\r\n"`. Correct.

---

## Bugs

### B1 — Same as C1 (empty-needle corruption). Listed under Correctness; it is the only true bug.

### B2 — Slice bounds are safe (NO finding — verified)

- `lines[..start_idx]` with `start_idx=0` → `&lines[..0]` is empty (OK).
- `lines[end_idx + 1..]` with `end_idx = lines.len()-1` → `lines[len..]` is empty (OK).
- `start_idx >= lines.len()` is checked at `:147` before the slices are taken, so no out-of-bounds panic.
- `end_idx = (end-1).min(lines.len()-1)` clamps before use; `end < start` is rejected at `:126`, and `start==0` at `:121`, so `end-1` cannot underflow (end≥start≥1 ⇒ end≥1 ⇒ end-1≥0).

No panic risk. No fix needed.

---

## Security

### S1 — No new path exposure (NO finding)

Line-range mode does not touch path validation. `execute` (`:341`) still calls `sandbox.validate(Path::new(&args.path))` and `is_protected_write_target` (`:348`) before any edit, identical to the literal/regex paths. The sandbox confinement and protected-file checks apply uniformly. The `required` change (dropping `old_string`) does not affect `key_argument` in `safety_rules.rs:417`, which keys on `"path"` (still required). No new attack surface.

The one security-adjacent note is C1: an empty-needle edit can overwrite a file's content in a way a human approver might not immediately recognize as corruption — but it is still gated behind `NeedsApproval`, so it is not an unattended-write risk.

---

## Constitution compliance

### K1 — Public functions documented (NO finding)

`prepare_edit` (`:86-91`) and `compute_diff` have doc comments. `prepare_edit_lines` is private (`fn`, not `pub`) — the constitution requires doc comments on *public* functions, so a private helper without one is compliant; it has a thorough comment block anyway. `FileEditArgs` fields all have doc comments (`old_string:24-27`, `start_line:43-48`, `end_line:50-53`). `FileEditTool::new` (`:62`) is documented.

### K2 — Windows / PowerShell / line-ending style (NO finding)

No Linux paths or bash syntax introduced. The change reuses the existing `detect_line_ending` / `normalize_line_endings` helpers, preserving the file's line-ending style (CRLF test passes). No mixed endings introduced.

### K3 — No commits to main (NO finding)

This is a review of uncommitted working-tree changes only; no commit has been made. The plan's closing sequence will commit to the feature branch, not main.

### K4 — Tests run before step complete (NO finding)

`cargo test --lib file_edit` passes (36/36). The full `cargo test` run is the plan owner's responsibility at step-close; this review confirms the file_edit subset is green.

---

## Other uncommitted changes (skim)

- `.coding/plans/stack.json` — plan-stack bookkeeping; the `skill`/`merge_to_main` block was replaced with a plain stack entry. Expected for a plan transition. No code impact.
- `.coding/plans/c75501aa-*.md` (untracked) — the active plan file. Bookkeeping only.

No findings on either.

---

## Overall verdict

**Fix-then-ship.**

The line-range feature itself is correct, well-tested (8 new tests covering range/single-line/clamp/through-last-line/start>end/zero-start/past-EOF/empty-file/one-bound/CRLF/multiline/diff/no-change/end-to-end), and the slice math is panic-free. The CRLF and single-empty-line edge cases the plan flagged are handled correctly.

**The one blocking finding is C1/B1:** making `old_string` optional without guarding `prepare_edit_literal` (and `prepare_edit_regex`) against an empty needle re-opens a silent whole-file-corruption path that the old `required` schema previously blocked. Add the empty-needle guard (a few lines + one test) and this is ready to ship.
