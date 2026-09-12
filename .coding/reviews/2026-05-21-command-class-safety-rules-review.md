# Review: Command-class safety rules + safety.toml cleanup

**Date:** 2026-05-21
**Scope:** All uncommitted changes (`git diff HEAD`) — `src/safety_rules.rs`, `src/safety_rules/cmd_class.rs` (new), `src-tauri/src/ipc/agent.rs`, `src-tauri/src/main.rs`, `frontend/src/lib/tauri.ts`, `frontend/src/components/chat/ApprovalPrompt.tsx`, `.coding/safety.toml`. (Pre-existing `.coding/plans/*` bookkeeping changes are out of scope.)
**Verdict:** ❌ **Do not merge.** One critical security vulnerability + several regressions.

---

## 🔴 CRITICAL — Security: redirect-stripping regex hides chained commands (auto-approve bypass)

**File:** `src/safety_rules/cmd_class.rs:402` (`strip_redirections`)

The `strip_redirections` regex is:
```rust
Regex::new(r"\d*>{1,2}(?:&\d+|\$null|[^\s|]+)?")
```

The `[^\s|]+` alternative (matching a redirect *target* like `>file.txt`) is **greedy and not quote-aware**, and it runs **before** statement splitting. It consumes every non-space, non-`|` character — including `;`, `&`, and command names.

**Exploit (verified by running the classifier):**
```
classify("cargo test>f;rm -rf x")      → Some("cargo test")   ❌ should be None
classify("cargo test>f&&rm -rf x")     → Some("cargo test")   ❌ should be None
classify("cargo test>out.txt;rm -rf x") → Some("cargo test")   ❌ should be None
```

Trace for `cargo test>f;rm -rf x`:
1. `strip_redirections` matches `>f;rm` (the `[^\s|]+` gobbles `f;rm` up to the space before `-rf`), leaving `cargo test -rf x`.
2. `split_statements` sees no `;` → one statement `cargo test -rf x`.
3. `classify_primary` → `cargo` + subcommand `test` → `Some("cargo test")`.

With the `cargo test` class rule now in `.coding/safety.toml`, `is_safe("shell", {command: "cargo test>f;rm -rf x"})` returns **`true`** → the call is **auto-approved without a prompt**, yet the shell actually executes `rm -rf x`. This is exactly the chained-command hole the classifier was designed to close. An attacker (or a model) who can influence the `command` argument can append `>f;<anything>` to smuggle arbitrary commands past a `command_class` rule.

**Why the existing 17 tests missed it:** every test uses `2>&1` (matched by the safe `&\d+` alternative) or `> ` with a space before a filter cmdlet. No test exercises a bare `>file` target adjacent to a `;`/`&&`.

**Fix:** make `strip_redirections` quote-aware AND restrict the file-target alternative so it cannot cross statement separators. At minimum, the target class `[^\s|]+` must also exclude `;`, `&`, `|` is already excluded but `;`/`&` are not. Better: exclude `;&|<>` and quotes, or run redirection-stripping *after* quote-aware statement splitting (strip per-statement). Add regression tests for `cargo test>f;rm x`, `cargo test>f&&rm x`, `cargo test>out.txt;rm x`, and `echo "a > b"; rm x`.

---

## 🟠 HIGH — Correctness/Regression: 4 previously-approved commands now prompt

**File:** `.coding/safety.toml` (consolidated rules)

The old literal rules approved these exact commands; the new class rules do **not** cover them (verified by running `classify`):

| Old literal rule (approved) | `classify` result | New rule that should match | Status |
|---|---|---|---|
| `rustc --version; cargo --version` | `rustc;cargo` | none (`rustc` and `cargo` exist separately, not joined) | ❌ prompts |
| `git branch --show-current; git log --oneline -3` | `git branch;git log` | none (`git branch` + `git log` separate) | ❌ prompts |
| `Get-Location; Get-ChildItem -Name \| Select-Object -First 20` | `Get-Location;Get-ChildItem` | none (separate) | ❌ prompts |
| `cd frontend; npx eslint src/...` | `cd;npx eslint` | none (`eslint` not in `NPX_TOOL_SUBS`, and no rule) | ❌ prompts |

These are not security issues (prompting is the safe default), but they are **functional regressions**: commands the user previously marked safe will now re-prompt. The plan's goal was to consolidate without losing coverage.

**Fix options:** (a) add the 4 missing class patterns (`rustc;cargo`, `git branch;git log`, `Get-Location;Get-ChildItem`, `cd;npx eslint`) to `safety.toml`; or (b) accept the regressions if these commands are no longer used. Note `eslint` is not in `NPX_TOOL_SUBS` — if eslint should be classifiable, add it; otherwise document that `npx eslint` is intentionally unclassifiable.

---

## 🟡 MEDIUM — Correctness: `FILE_HEADER` constant is stale, `persist()` clobbers the improved header

**File:** `src/safety_rules.rs:32-47` (`FILE_HEADER`), `src/safety_rules.rs:400` (`persist`)

The hand-edited `.coding/safety.toml` now has an **updated header** explaining `command_class` rules and the `kind` field. But the `FILE_HEADER` constant still contains the **old** text (describes only regex/literal rules, no mention of `kind`/`command_class`). `persist()` (called by `add_rule`, `add_rule_broad`, `add_rule_class`) writes `format!("{FILE_HEADER}\n{body}")`, so the **first time any rule is added via the UI**, the improved header is overwritten with the stale one.

This is a pre-existing pattern (the header was always regenerated), but the divergence is now more meaningful since the header documents new behavior. **Fix:** update `FILE_HEADER` to match the new header text (or at least mention `kind`/`command_class`), so regeneration doesn't lose the documentation.

---

## 🟡 MEDIUM — Constitution: `safety.toml` has no trailing newline

**File:** `.coding/safety.toml` (last bytes: `65 24 22` = `e$"`, end of `pattern = "^git:merge$"`)

The file ends without a trailing newline (`Ends with LF: False`). The old file also lacked one, so this is preserved rather than introduced — but the constitution emphasizes preserving line-ending style. Not a blocker; noting for completeness. The git warnings about LF→CRLF are cosmetic (Git autocrlf).

---

## 🟢 LOW — Correctness: `strip_redirections` corrupts quoted args (non-exploitable but wrong)

**File:** `src/safety_rules/cmd_class.rs:400-405`

`strip_redirections` is not quote-aware, so `echo "a > b"` becomes `echo "a  b"` (the `> b` inside the quotes is stripped). This doesn't create an exploit (the `;` after the closing quote is preserved, so a trailing `rm` is still seen), but it means the classified class is derived from a *corrupted* command string. Combined with the critical finding above, the right fix (quote-aware stripping) addresses both.

---

## ✅ Verified correct

- **`is_safe` branch** (`safety_rules.rs:250-261`): `CommandClass` rules are guarded by `if tool == "shell"`; a class rule cannot match a non-shell tool. A hand-edited `file_write` class rule is silently inert (never matches) — safe, though undocumented.
- **String comparison** (`safety_rules.rs:258`): `classify(&command) == Some(r.pattern.clone())` is `Option<String> == Option<String>` — correct equality.
- **Backward compat** (`safety_rules.rs:158`, `69-73`): `kind` defaults to `Literal` via `#[serde(default)]` + `impl Default`; old `safety.toml` without `kind` parses as Literal. Test `missing_kind_defaults_to_literal` confirms.
- **`add_rule_class` error paths** (`safety_rules.rs:320-347`): non-shell → `Err` before lock; unclassifiable → `Err` via `?` before lock/push (no rule saved); duplicate → no-op returning `Ok(class)`. Test `add_rule_class_errors_on_unclassifiable` confirms no rule saved.
- **`2>&1` ordering** (`cmd_class.rs:107-108`): `strip_redirections` runs before `split_statements`, so the `&` in `2>&1` (matched by the `&\d+` alternative) is removed before statement splitting — no false split. Correct.
- **dispatch.rs force_prompt gate** (`src/agent/dispatch.rs:151-164`): **unchanged** (`git diff HEAD -- src/agent/dispatch.rs` is empty). `never_auto`/`never_auto_for` tools and core ops still always prompt; `auto_approved` is gated by `!force_prompt`. Class rules cannot bypass core-op approval.
- **Quote handling for `;`/`&&`/`|`** (`split_statements`/`split_pipeline`): single/double/backtick quotes correctly prevent splitting inside strings. Tests `quoted_semicolon_not_split` and `single_quoted_semicolon_not_split` confirm.
- **Fail-safe for non-filter stages, unknown subcommands, assignments, subexpressions, empty**: all return `None`. Tests confirm.
- **IPC command + registration** (`agent.rs`, `main.rs`): mirrors `add_safety_rule`; registered in `invoke_handler`. Doc comments present.
- **Frontend** (`tauri.ts`, `ApprovalPrompt.tsx`): `addSafetyRuleClass` binding + button gated on `toolName === "shell" && !isCoreOp`; `handleMarkSafeClass` mirrors `handleMarkSafe` (approves even if rule save fails). Consistent with existing patterns.
- **Line-ending style**: all changed files are consistently CRLF (Rust/TS) or LF (safety.toml, matching old). No mixed endings introduced. BOM in safety.toml was pre-existing.
- **Doc comments**: all new public Rust functions (`classify`, `Rule::new_class`, `add_rule_class`, `add_safety_rule_class`) have doc comments. ✓

---

## Summary

| Severity | Count | Blocking? |
|---|---|---|
| 🔴 Critical (security) | 1 | ✅ Yes |
| 🟠 High (regression) | 1 (4 commands) | ✅ Yes |
| 🟡 Medium | 2 | No |
| 🟢 Low | 1 | No |

**Recommendation:** Fix the critical redirect-stripping vulnerability (quote-aware + separator-safe target matching, with regression tests) and resolve the 4 regressions before merging. The medium/low items should also be addressed but are not blockers.
