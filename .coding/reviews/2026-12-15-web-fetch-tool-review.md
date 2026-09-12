# Review: `web_fetch` tool (read-only URL fetch for research)

**Scope:** All uncommitted changes — `src/tool/agent/web_fetch.rs` (new),
`src/tool/agent/mod.rs`, `src/agent/factory.rs`, `src/agent/prompt.rs`.

**Plan goal:** A read-only `web_fetch` agent tool — fetch an http/https URL,
strip HTML to readable text, cap output, return status + content-type + final
URL + body. `SafetyLevel::AutoRun` + `ToolCategory::Agent` so it's usable in
every workflow state (incl. Planning/Complete) for research.

---

## Findings

### BUG (high) — Panic on multi-byte UTF-8 truncation; the char-boundary loop is dead code

**Ref:** `src/tool/agent/web_fetch.rs:173-183`

```rust
let body_out = if text.len() > max_length {
    let mut cut = text[..max_length].to_string();   // ← line 174: PANICS here
    // Back up to a char boundary if we sliced mid-character.
    while !cut.is_char_boundary(cut.len()) {         // ← dead code in the panic path
        cut.pop();
    }
    cut.push_str("\n... (truncated: body exceeded max_length)");
    cut
} else {
    text
};
```

**Root cause.** `text[..max_length]` is a **byte-range** slice. Per the `str`
index docs, it panics if `max_length` does not fall on a character boundary
(and `max_length <= text.len()`). For multi-byte content (CJK, emoji) longer
than `max_length`, the cut point lands inside a multi-byte char with high
probability — e.g. 60 000 bytes of a 3-byte CJK char with the default
`max_length = 50_000` panics immediately at line 174.

**The "fix-up" loop never runs.** The `while !cut.is_char_boundary(cut.len())`
loop is intended to back up to a boundary, but it operates on `cut` *after* the
slice. If the slice panicked, we never reach the loop; if the slice succeeded
(`max_length` happened to land on a boundary), `cut.len() == max_length` is
already a boundary so the loop body never executes. The loop is dead code in
both cases — the "char-boundary-safe truncation" claim in the comment (line
171) is false.

**Reachability.** Any fetched HTML page whose stripped text exceeds 50 000
*bytes* and contains multi-byte characters (CJK docs, emoji, accented Latin
runs) — exactly the "reading a library's docs site" research use case the tool
exists for. This crashes the agent turn.

**Contradicts the codebase's own established pattern.** `read_files.rs:39-48`
defines `pub(crate) fn truncate_to_boundary(s: &mut String, max: usize)` for
exactly this — it finds the boundary index *first* (`while end > 0 &&
!s.is_char_boundary(end) { end -= 1 }`) and only then truncates. `cap_tool_output`
(`mod.rs:46-53`) already uses it. `web_fetch.rs` even imports `cap_tool_output`
but reinvents the truncation incorrectly instead of reusing the tested helper.

**Fix.** Reuse the existing helper (already `pub(crate)`, already imported
module):

```rust
let mut body_out = text;
if body_out.len() > max_length {
    crate::tool::agent::read_files::truncate_to_boundary(&mut body_out, max_length);
    body_out.push_str("\n... (truncated: body exceeded max_length)");
}
```

**Regression test required** (project constitution: every bug fix gets a
reproducing regression test that fails without the fix and passes with it).
Add a test that builds a multi-byte string longer than `max_length` and asserts
`execute` returns success with a truncation note and no panic — mirroring
`cap_tool_output_multibyte_no_panic` (`mod.rs:74-85`) and
`multibyte_utf8_under_caps_no_panic` (`read_files.rs:470-488`). The current 3
tests cover `html_to_text` + scheme validation but **none** cover the
truncation path, which is why this bug shipped.

---

### CORRECTNESS (low) — `max_length` documented as "chars" but enforced as bytes

**Ref:** `src/tool/agent/web_fetch.rs:99` (schema description "Max chars of
body text to return"), `:45` (field doc "Max chars of body text"), `:173`
(`text.len() > max_length` — byte length), `:22` (`DEFAULT_MAX_LENGTH` doc
says "chars").

The schema and doc comments promise `max_length` is in **chars**, but the
comparison `text.len() > max_length` and the slice operate in **bytes**. For
ASCII this is identical; for CJK/emoji 50 000 bytes ≈ 16 666 chars. This is a
doc/code mismatch, not a crash. The codebase convention is byte-based caps
(`cap_tool_output`, `truncate_to_boundary`, `DEFAULT_MAX_BYTES` all use bytes),
so the efficient + consistent fix is to correct the wording to "bytes" (or
"max length") in the schema description (`:99`), the field doc (`:45`), and
the constant doc (`:19-21`). (Switching to `.chars().count()` would be O(n)
per call and is not recommended.)

---

## Areas verified clean

- **SSRF / scheme validation** (`:120-127`) — scheme is lowercased and checked
  to start with `http://`/`https://` *before* any network call. `file://`
  (local file read), `javascript:`, `data:`, and all other schemes are rejected
  with a clear error. No `Authorization` header is sent, so there is no
  credential to leak on redirect. Internal-network SSRF (e.g. `http://127.0.0.1`,
  cloud metadata IPs) is not blocked, but this matches the existing threat
  model — the agent is the caller (not untrusted external input) and
  `browser_navigate` already allows arbitrary URLs. Not a finding against this
  diff.
- **Redirect policy** (`:136`) — `Policy::limited(5)` bounds redirects; final
  URL captured via `resp.url()` (`:156`). ✓
- **Timeout** (`:135`) — 30s. ✓
- **No panic on malformed HTML / empty body / missing content-type** —
  `html_to_text` is regex-based (no parser panic on malformed HTML); empty body
  → empty string through the regexes → trim → empty; missing content-type →
  `unwrap_or("")` then displayed as "unknown" (`:189-193`). ✓
- **Regex `unwrap()`s** (`:56,57,63,67`) — all on compile-time-constant literal
  patterns; `Regex::new` on a valid literal never errors. Safe. ✓
- **AutoRun safety level** (`:107-109`) — justified: read-only GET, no project
  mutation, same posture as `file_read`/`browser_snapshot`. ✓
- **Registration + wiring** — `factory.rs:53` (import), `:522` (register in
  `register_agent_tools`), `:1102` (added to `expected_tool_names` test),
  `mod.rs:35` (module), `prompt.rs:173-174` (strategy mention). All consistent
  with the existing tool pattern. ✓
- **Doc comments** — `WebFetchTool` (`:24`), `new()` (`:28`), `html_to_text`
  (`:50-53`, documented despite being private), `WebFetchArgs` + fields
  (`:40-47`), `DEFAULT_MAX_LENGTH` (`:19-21`) all documented. The `Default`
  impl (`:34-38`) has no doc comment, consistent with the three other `Default`
  impls in the codebase (`ask_user.rs:68`, `image_tools/zoom.rs:38`,
  `tool/mod.rs:392`) — trait impl methods are not flagged by `missing_docs`.
  Not a finding.
- **Warning-free** — all imports are used; no dead code; no `#[allow(...)]`.
  The `Default` impl mirrors `ask_user.rs` (satisfies `clippy::new_without_default`).
  ✓

---

## Summary

One **high-severity bug** (panic on multi-byte truncation — the char-boundary
loop is dead code; reuse `truncate_to_boundary` + add a regression test) and
one **low-severity** doc/code mismatch (`max_length` says "chars", enforces
bytes). Security, SSRF/scheme validation, redirect/timeout bounds, registration,
and doc-comment coverage are clean.
