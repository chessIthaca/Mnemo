# Review: Actionable invalid-args error for file_read/read_files tool mix-ups

Reviewed: `git diff HEAD` (all uncommitted changes) — src/tool/agent/file_read.rs, src/tool/agent/read_files.rs, PLAN.md, plus .coding/ bookkeeping churn (ignored per instructions: backlog.json status flip, plans/stack.json, new plan .md — workflow state only).

## Findings

### LOW — over-long line on the new helper signature (read_files.rs:63)

`pub(crate) fn invalid_args_error(e: &serde_json::Error, args: &serde_json::Value, hint: &str) -> String {` is 111 chars — the only code line over 100 chars introduced by this change (the other long lines in these files are JSON schema strings that cannot be wrapped). The codebase has no rustfmt.toml and no fmt CI check, so this is cosmetic, but `cargo fmt` would rewrite the signature.

Fix:
```rust
pub(crate) fn invalid_args_error(
    e: &serde_json::Error,
    args: &serde_json::Value,
    hint: &str,
) -> String {
```

## Verified correct (no finding)

1. **Plain missing-field case** (`json!({})` → `file_read`): `as_object()` → empty keys → `"received keys: none"`, message reads `invalid arguments: missing field 'path' (received keys: none).` — natural and still prefixed with `invalid arguments:`, so the pre-existing `invalid_args_error` test (file_read.rs:241-248) and every `contains("invalid arguments")` assertion stay green.
2. **Clone + borrow**: `serde_json::from_value(args.clone())` then borrowing `&e`/`&args` in the error arm is safe and correct (same pattern already used in file_edit.rs:563 and file_write.rs:68). The clone is negligible vs. the tool's file I/O.
3. **Non-object args (array/null/string)**: `Value::get("files")` with a string index returns None on non-objects, and `as_object()` returns None → `"received keys: none"`. `from_value` still fails on a struct target. No panic, no misleading key list.
4. **Hint-guard edge cases**: `{"files": <non-array>}` in file_read still fires the read_files hint (correct diagnosis — the model sent a `files` key). Both-keys calls (`path` + `files`) never reach the error path at all because the valid sibling key makes the parse succeed — so there is no hint-ambiguity case.
5. **Regression tests**: both `wrong_shape_files_array_hints_read_files` (file_read.rs:251-269) and `wrong_shape_top_level_path_hints_file_read` (read_files.rs:358-373) fail on the old code — the old messages (`missing field 'path'` / `missing field 'files'`) contain none of the asserted markers ("received keys", "files"+"read_files", "path"+"file_read" — note "file_read" is not a substring of "read_files" and vice versa, so the assertions are not trivially satisfied) — and pass with the fix. They assert `!success` plus 4 markers each and would catch a regression of the hint OR the keys list OR the prefix.
6. **No other consumer breaks**: searched all `contains("invalid arguments")` and `assert_eq`-style assertions — none depend on the exact old text. The agent-level `max_retries_aborts_after_consecutive_tool_errors` (src/agent/tests.rs:947) uses malformed JSON, which is rejected upstream at the JSON-parse gate (turn.rs:1060-1132) and never reaches the serde-struct parse.
7. **Security**: the produced text is serde's own error + sorted plain key names (joined with ", ") + two static hint strings. No user/model-controlled string is executed or format-interpreted (the braces in `{path, start_line?, max_lines?}` live in the hint *value*, not a format template). Nothing executable changed.
8. **Constitution**: `invalid_args_error` has a full doc comment explaining why it exists (read_files.rs:54-62), consistent with the pub(crate) convention (`truncate_to_boundary`, `read_one`). `#![deny(warnings)]` means a green `cargo test` proves no dead code/unused import; the helper is used by both tools. No cfg(windows) or platform-specific code — multi-platform neutral.
9. **Docs sync**: PLAN.md Terminology entry accurately lists `read_files` as the batch sibling and documents the cross-hinting. README.md never mentions these tools' error text, so nothing else needs updating.

## Verdict

One trivial LOW style finding; everything else is correct, safe, and properly tested. Ship after wrapping the signature line.
