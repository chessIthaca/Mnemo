# Batch A Review — Security + Stall Fixes (feat/deep-review-a-security)

Reviewed all uncommitted changes (`git diff HEAD`) across 7 files implementing
5 findings (A1–A5). Each item was traced in context against the surrounding
code, not just the diff.

## Summary

| Item | Verdict | Notes |
|------|---------|-------|
| A1 — glob sandbox escape | ✅ Correct | `validate_glob` is airtight for the documented vectors; defense-in-depth holds. |
| A2 — cmd_class bypass | ⚠️ 2 incomplete-closure gaps | `$(` guard + metachar guard miss two subexpression vectors (see S1, S2). |
| A3 — cap_tool_output panic | ✅ Correct | Char-boundary truncation; multibyte test is meaningful. |
| A4 — forwarder git stall | ✅ Correct | Closures `Send+'static`, no lock held across `.await`, no borrow-after-move. |
| A5 — 401/403 body suppression | ✅ Correct (1 verify) | 3 paths wired; 1 local-trace logging path to verify (see S3). |

---

## Findings

### S1 — `cd` short-circuits BEFORE the new metachar guard (security, medium)

`src/safety_rules/cmd_class.rs:304-306` — `classify_primary` returns
`Some("cd")` for any `cd <arg>` **before** the new `(`/`)`/backtick metachar
guard runs at lines 314-318:

```rust
if cmd == "cd" {
    return Some("cd".to_string());   // returns before the metachar guard
}
```

So `cd (Remove-Item -Recurse -Force C:\evil)` tokenizes to `["cd", "(Remove-Item ...)"]`,
hits the `cd` branch, and returns `Some("cd")` — the `(evil)` subexpression is
never inspected. PowerShell evaluates a parenthesized argument as a
subexpression, so this is arbitrary code execution that classifies as `cd` and
would be auto-approved by any saved `command_class:cd` rule.

The `$(` guard in `classify()` (line 118) does **not** catch this either — the
vector uses bare `(`, not `$(`. And `classify_statement`'s `starts_with('(')`
check (line 282) only fires when the *statement* starts with `(`, not when an
arg does.

This is pre-existing, but A2's stated goal is "any subexpression anywhere →
unclassifiable," and the new metachar guard fails to close it because `cd`
short-circuits first. The guard is also inconsistent: `echo (evil)` → None
(caught), but `cd (evil)` → `cd` (not caught).

**Fix:** move the metachar-guard loop (lines 314-318) to *before* the `cd`
short-circuit, or drop the `cd` early-return and let `cd` flow through the
guard like every other command.

### S2 — `$(` in a bare-word redirect target is stripped before the `$(` guard (security, medium-low)

`src/safety_rules/cmd_class.rs:105` — `strip_redirections` runs **before** the
`$(` guard (line 118). For a redirect target with **no space** after `>`, the
bare-word consumer (`strip_redirections`, the `_ =>` arm ~line 521) consumes
the entire target until a terminator — and the terminator set is
`{whitespace, ;, &, |, <, >, ', "}`. Notably `$`, `(`, `)` are **not**
terminators, so a target like `out$(evil).txt` is consumed wholesale:

- `cargo test >out$(evil).txt` → stripped to `cargo test ` → the `$(` is gone
  → the `$(` guard never sees it → classifies as `cargo test` → auto-approved
  by a `cargo test` rule, while PowerShell evaluates `$(evil)` to compute the
  redirect target.

(With a space — `> out$(evil).txt` — the bare-word consumer stops at the
leading space, so `out$(evil).txt` survives into the output and IS caught by
the `$(` guard. The bypass requires the no-space form `>target$(evil)ext`.)

**Fix:** add `if command.contains("$(") { return None; }` at the very top of
`classify()`, before `strip_redirections`. This catches `$(` anywhere in the
raw command regardless of later stripping. (This single change also closes the
`>$($(evil))` nested form, which today only survives because the leftover `(`
happens to trip the metachar guard — fragile.)

### S3 — 401/403 body logged to the provider trace in the stream path (security, low-medium — verify)

`src/provider/openai.rs:525-526` — in the main chat-stream path, the full
response body is logged to the local provider trace even for 401/403:

```rust
Err((status, body, error)) => {
    if let (Some(log), Some(id)) = (&trace, rec_id) {
        log.fail(id, status.as_u16(), &body);   // full body, incl. 401/403
    }
    return Err(error);   // error has body suppressed — good
}
```

The user-visible `error` correctly suppresses the body for 401/403 (A5's
goal). But `body` — which for 401/403 may contain the echoed
`Authorization: Bearer {key}` — is written to the trace log. The `check_response`
doc comment asserts this is "safe to log locally," but that holds **only if**
the trace is never surfaced to any UI panel. If a Settings/debug view ever
renders the trace, the key leaks there (a different path than the one A5
closed).

**Action:** verify the provider trace (`self.trace` / `log.fail(...)`) is never
displayed in the UI. If it is (or could be, e.g. a future debug panel), suppress
the body for 401/403 in the trace call too (log only the status code). The
`fetch_models` and `vision` paths discard the body entirely (no trace logging),
so they are fine.

### U1 — metachar guard false-positives on `git commit -m "msg (parens)"` (usability, low)

`src/safety_rules/cmd_class.rs:314-318` — the metachar guard rejects ANY token
containing `(`, `)`, or backtick, including flag *arguments*. A commit message
with parens is a common, legitimate pattern:

- `git commit -m "fix (issue #123)"` → token `fix (issue #123)` contains `(` →
  `None` → prompts for approval (was `Some("git commit")` before this batch).

This is fail-safe (prompts rather than auto-approves), so not a security
regression, but it is a behavior change for legitimate commands and no test
covers it. The existing `quoted_semicolon_not_split` test only covers `;` in a
message, not parens.

**Recommendation:** either scope the guard to command-position tokens (before
the first flag), or accept the trade-off and add a test documenting that
commit messages with parens now prompt. At minimum add a test so the behavior
is pinned.

---

## Test-coverage gaps (informational)

- **A1 — `starts_with(root)` defense-in-depth is unreachable in tests.**
  `validate_glob` rejects every `..` component first, so no test exercises the
  `starts_with(&root)` / `should_search` fail-closed path in the glob loop. It
  is genuine defense-in-depth (and the `glob` crate does not expand `{a,b}`
  braces into `..`, so the layer is sound), but it has no direct test.
  Acceptable for defense-in-depth; noting for completeness.

- **A2 — no test for the S1/S2 vectors.** `subexpression_vectors_are_none`
  covers the 4 documented vectors but not `cd (evil)` (S1) or
  `cargo test >out$(evil).txt` (S2). Add table rows for both → `None`.

- **A5 — `check_response` wiring is untested.** The 4 tests exercise
  `provider_error` (the pure decision) directly, but not `check_response` (the
  async wrapper that reads the body and calls `provider_error`) nor its
  integration at the 3 call sites. A mock-`Response` test for
  `check_response` would verify the body is actually read and the right
  `Error` returned for 401/403/500.

---

## Constitution compliance

- **Doc comments:** all new/changed `pub` and `pub(crate)` items have doc
  comments — `validate_glob`, `should_search` (updated), `provider_error`,
  `check_response`, `checkpoint`/`commit_success`/`rollback` (updated),
  `cap_tool_output` (updated), `git` (updated). ✅
- **No `#[allow(...)]`:** none present. ✅
- **No dead code:** `provider_error` (used in `check_response` + 4 tests),
  `check_response` (4 call sites), `validate_glob` (2 tools + tests) are all
  used. ✅
- **Line endings:** diff shows no CRLF introductions; existing LF style
  preserved. ✅
- **Warning-free build:** cannot run `cargo test` (read-only reviewer), but the
  code is clean — the `unreachable!()` arms are exhaustive-match-safe, the
  `std::result::Result` qualification avoids the `Result` alias clash, and no
  unused imports/vars were introduced. **The main agent must run `cargo test`
  to confirm a green, warning-free build before commit** (also confirms
  `tauri::async_runtime::spawn_blocking` works under `#[tokio::test]`).

---

## Items verified correct (no findings)

- **A1 `validate_glob`:** rejects `/`, `\`, `C:`-style drive, `\\` UNC, and
  every `..` component (split on both `/` and `\`). The drive check
  (`bytes[1]==':' && ascii_alpha`) correctly leaves mid-path `C:` literals
  alone. `should_search` now fails closed on `strip_prefix` Err. ✅
- **A3:** `truncate_to_boundary` backs up to a char boundary; the 3-byte CJK
  test (70k × `日` ≈ 210 KB, cap 102400) lands mid-char and is backed up
  correctly. `is_char_boundary(out.len())` assertion is sound. ✅
- **A4 concurrency:** `BacklogItem` is `Clone+Send` (all fields `Send`); the
  `spawn_blocking` closures own `PathBuf`+`BacklogItem`/`String` → `'static +
  Send`. The root lock is released at the end of the `let root = ...lock().await.root.clone();`
  statement (temporary guard dropped at `;`), **before** the `.await` —
  confirmed at both `run_all_dispatch_next:510-513` and
  `on_main_turn_resolved:606`. The 3 rollback/commit callers in
  `on_main_turn_resolved` use `root.clone()` / `sha.clone()` / `sha`-by-value
  correctly; `root` is never moved (only cloned) and `checkpoint_sha` is
  consumed only in the failure branch where it's last used. No borrow-after-move. ✅
- **A5 wiring:** all 3 paths (`fetch_models_with_vision`, stream, vision
  `describe_image`/`describe_images`) route through `check_response` →
  `provider_error`; no other `response.text().await` or body-in-error path
  remains (search confirmed only line 450, inside `check_response`). The
  `Error::Provider(format!(...))` calls elsewhere are transport/parse errors,
  not response bodies. ✅
- **A2 design deviation (keeping `"..."` benign):** sound — the `$(` guard
  closes the interpolation vector, and bare `$var` in a double-quoted string is
  a variable read, not code execution. Rejecting all `"` would have broken the
  documented `; "exit=$LASTEXITCODE"` pattern for no security gain. ✅

---

## Required actions before commit

1. **S1 (medium):** move the metachar guard before the `cd` short-circuit (or
   remove the `cd` early-return) so `cd (evil)` → `None`. Add a test.
2. **S2 (medium-low):** add `if command.contains("$(") { return None; }` at the
   top of `classify()` (before `strip_redirections`) so redirect-target
   subexpressions are caught. Add a test for `cargo test >out$(evil).txt`.
3. **S3 (verify):** confirm the provider trace is never UI-surfaced; if it is,
   suppress the 401/403 body in the `log.fail` call at `openai.rs:526`.
4. **U1 (low):** add a test pinning the `git commit -m "msg (parens)"` behavior
   (either it prompts, documented, or the guard is scoped to command tokens).
5. Run `cargo test` (warning-free) to confirm the build and that
   `spawn_blocking` works under `#[tokio::test]`.

S1 and S2 are security-relevant incomplete-closure gaps in A2's stated goal and
should be fixed in this batch. S3 and U1 can be addressed by verification or a
small follow-up.
