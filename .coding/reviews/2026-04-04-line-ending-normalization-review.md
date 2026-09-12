# Review: Line-ending-agnostic file_write/file_append + constitution rule

Date: 2026-04-04
Plan: "Make file_write and file_append line-ending-agnostic (normalize to the existing file's style, like file_edit already does), extract the shared helper, and add a constitution rule about preserving line endings."

## Scope reviewed

`git status` / `git diff HEAD`:
- NEW `src/tool/agent/line_endings.rs` (extracted `detect_line_ending` + `normalize_line_endings`, 7 unit tests)
- `src/tool/agent/mod.rs` (`pub mod line_endings;`)
- `src/tool/agent/file_edit.rs` (removed private defs, now imports)
- `src/tool/agent/file_write.rs` (read-existing → normalize before write; 3 tests)
- `src/tool/agent/file_append.rs` (read-existing → normalize before append; 2 tests)
- `agent.md` (new "Preserve the line-ending style" constitution bullet)
- `.coding/plans/*`, `.coding/plans/stack.json` — workflow bookkeeping, no corruption.

## Findings

### Correctness

**[Low] Empty existing file is treated as LF, silently converting CRLF content.**
`src/tool/agent/file_write.rs:117-124` (and the mirror in `file_append.rs:106-113`):
`read_to_string` on a zero-byte file returns `Ok("")`, and `detect_line_ending("")`
returns `"\n"` (since `"".contains("\r\n")` is false). The caller's content is then
normalized to LF. This is inconsistent with the *non-existent* file path, which keeps
the caller's content verbatim: a brand-new file written with CRLF content stays CRLF,
but a zero-byte *existing* file written with CRLF content becomes LF.

Suggested fix: treat an empty existing file like a new file:
```rust
let content_to_write = match std::fs::read_to_string(&validated) {
    Ok(existing) if !existing.is_empty() => {
        let le = detect_line_ending(&existing);
        normalize_line_endings(&args.content, le)
    }
    _ => args.content.clone(),
};
```
Severity is low — empty files have no "style" to preserve, and LF is a defensible
default — but the inconsistency with the non-existent-file branch is surprising.

### Bugs

**[Low] file_append reads the entire existing file into memory solely to detect line endings.**
`src/tool/agent/file_append.rs:106`. `read_to_string` loads the whole file, but
`detect_line_ending` only needs to know whether any `\r\n` occurs. file_append is
explicitly documented (`file_append.rs:3-5`, schema description) as the tool for
writing *large* files in chunks, so this read can be disproportionately expensive
(hundreds of MB into memory to answer a one-byte question). file_write has the same
pattern (`file_write.rs:117`) but is less concerning since the old content is about
to be discarded anyway.

Not a correctness bug — a performance/memory consideration. A streaming scan (read
the first few KB, or iterate bytes until a `\r\n` is seen / EOF) would avoid holding
the whole file. Acceptable to defer; flagging for awareness.

**[Info] TOCTOU between read_to_string and write — benign.**
In both tools there is a window between `read_to_string` (detect style) and the
subsequent write/append. If the file is deleted in that window: file_write's
`std::fs::write` simply creates a new file with the (already-normalized) content;
file_append's `OpenOptions::create(true).append(true)` creates a new file and
appends. Worst case is normalization based on slightly stale data — no corruption,
no panic. No action needed.

**[Info] file_append read-before-open — no handle conflict.**
`read_to_string` opens, reads, and closes the file before `OpenOptions::open` runs.
No leaked/duplicate handle, no sharing violation. Correct.

### Edge cases verified (no findings)

- **No trailing newline**: `detect_line_ending` keys off any `\r\n` anywhere, so a
  file like `"a\r\nb"` (no final newline) is correctly detected as CRLF; appended
  content is normalized and joins cleanly. ✓
- **Lone `\r` (old Mac)**: `normalize_line_endings` collapses lone `\r` → `\n` first
  (`line_endings.rs:32`), then re-expands to `\r\n` if target is CRLF. Tested by
  `normalize_lone_cr`. ✓ (A file containing *only* lone `\r` is detected as LF, so
  appending to it leaves the existing lone-`\r`s untouched while new content is LF —
  a pre-existing-mixed-ending situation, but lone-`\r` files are effectively extinct.)
- **Mixed-ending files**: any `\r\n` present → CRLF chosen as dominant style
  (`detect_mixed_treats_as_crlf`). file_write replaces wholesale so old mixed endings
  are gone; file_append can't repair pre-existing mixed endings in the untouched
  prefix, which is the correct best-effort behavior. ✓
- **Non-UTF-8 files**: `read_to_string` returns `Err` → both tools fall through to
  `args.content.clone()` (write/append as-is). No panic, no corruption. ✓
- **Byte counts in success messages**: both now report `content_to_write.len()`
  (the normalized length actually written), not the raw `args.content.len()`. ✓

### Security

No new injection or path-traversal surface. In both tools, `read_to_string(&validated)`
operates on a path that has already passed `sandbox.validate` / `validate_for_creation`,
so the read is confined to the project sandbox. In file_write, the protected-file guard
(`file_write.rs:73-81`) runs *before* the read at line 117, so protected live-state files
(e.g. the memory DB) are never read. file_append's protected guard (`file_append.rs:95-100`)
likewise precedes its read. No findings.

### Constitution compliance

- Public functions have doc comments: `detect_line_ending` (`line_endings.rs:10-15`)
  and `normalize_line_endings` (`line_endings.rs:24-29`) both carry `///` docs; the
  module has a `//!` header. ✓
- Windows/PowerShell rules: N/A to Rust source. ✓
- No commit to main in this diff. ✓
- `cargo test` reported passing (453 lib tests) per the task; not re-run by this
  read-only reviewer.

## Summary

No blocking findings. Two low-severity items, both optional:
1. Empty existing file → LF inconsistency (correctness, low).
2. file_append reads whole file into memory to detect endings (performance, low).

The core normalization logic, extraction, tests, security posture, and constitution
rule are correct and clean.
