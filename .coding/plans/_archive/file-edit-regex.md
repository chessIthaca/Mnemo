# Plan: regex + count support for `file_edit`

## Goal
Extend `file_edit` with vi-inspired capabilities while staying backward-compatible:
- `use_regex: bool` (default false) — treat `old_string` as a Rust regex.
- `count: Option<usize>` (default None) — replace the first N matches (vi-style);
  `None` + `replace_all=false` = first only; `replace_all=true` = all (unchanged).
- Regex capture groups usable in `new_string` via `$1`/`${name}` (regex crate
  `Replacer` semantics).
- Line endings stay normalized for the pattern/replacement so CRLF files still work.

## Files to change
1. `src/tool/agent/file_edit.rs`
   - Add `use_regex` + `count` to `FileEditArgs` (`#[serde(default)]` so old calls
     keep working).
   - Add `prepare_edit_regex()` alongside existing literal path; dispatch in
     `prepare_edit()`.
   - Regex path: normalize line endings of pattern; compile `Regex`; use
     `replacen` with `count`/`replace_all` to control number of replacements;
     error if zero matches or zero-change.
   - Update tool schema description + JSON schema with new fields.
   - Add doc comments (public items already have them; keep style).
2. `src/tool/agent/file_edit.rs` tests
   - regex replace first/all/count
   - capture groups ($1)
   - zero-match error, invalid-regex error
   - CRLF + regex interplay
3. `src/agent/prompt.rs`
   - One line noting regex/count are available when exact match is too brittle.

## Out of scope (deferred vi features — will list in final summary)
- Line-range edits (`:10,20s/...`), per-line `g`/`c` confirm flags, `\r` magic in
  replacement. `count` + capture groups cover the highest-value vi workflows.

## Verification
- `cargo test` (per agent.md), read `test result:` lines.
