## Verdict: PASS

Reviewed all uncommitted changes on `wt/mnemo` for plan 358656a1 (`git diff HEAD`): `src/tool/agent/shell.rs` (+255/-17), `src/agent/factory.rs` (2 test call sites + the ExecutingResearch ceiling), `.coding/backlog.jsonl` (status → done), `.coding/plans/04a195de.md` (+3 unrelated lines, see L1). Verification is sound: `cargo test` green at 2464 passed / 0 failed / 5 ignored, warning-free under `#![deny(warnings)]` (both crate roots), and the change is confined to two library files — no new dependency, no new public surface, no config schema change. Every check item (a)–(h) below resolves to a pass; the three low findings are non-blocking observations recorded for completeness.

### (a) Translation semantics — PASS

`translate_powershell_chaining` (shell.rs:200-259) collects top-level operator byte positions, then emits segment 0 unconditionally and each later segment gated by the operator *before* it, with the tail gated by the last operator (`prev_is_and` carries the previous split's kind). Trace of the four shapes:

| input | output | semantics |
|---|---|---|
| `a && b` | `a; if ($?) { b }` | b iff a ok |
| `a \|\| b` | `a; if (-not $?) { b }` | b iff a failed |
| `a && b && c` | `a; if ($?) { b }; if ($?) { c }` | c iff b ran and ok |
| `a && b \|\| c` | `a; if ($?) { b }; if (-not $?) { c }` | c iff b failed or skipped |

This matches left-to-right `&&`/`||` short-circuit evaluation. The subtle case is correct: when `a` fails in `a && b || c`, `b` is skipped and `$?` still reflects `a`'s failure (a skipped `if` does not execute a statement, so it does not reset `$?`), so `c` runs — exactly as bash does. Verified for each of the four orderings.

Edge cases checked:
- **Operator at start** (`&& b`): `splits = [(0, true)]`, `prev_end = 2`, first segment `""` → output `; if ($?) { b }` — an empty first statement. PowerShell tolerates the leading `;` and still *fails* with "not a valid statement separator" on the original anyway, so the degenerate input is not made worse; no panic, no index error (byte slicing is at ASCII operator boundaries).
- **Trailing operator** (`a &&`): tail is `""` → `a; if ($?) {  }` — a no-op gate, harmless.
- **Single `&` / `|`, `2>&1`, `;`**: never split (the `&&`/`||` lookahead requires a second identical byte); `2>&1` is covered by an explicit test (shell.rs:893).
- **Quoted operators**: skipped while either quote state is set (shell.rs:889-890).
- **UTF-8**: the scanner iterates bytes, but every byte it branches on (`'`, `"`, `&`, `|`) is ASCII and cannot appear as a continuation byte of a multi-byte sequence, so slicing at `prev_end..pos` is always on a char boundary — no panic risk.

One genuine (pre-existing, not introduced) caveat: `if ($?)` reflects the last executed *statement*, so a segment that itself ends in a pipeline whose last element succeeds reports success. That is the same semantics the bash form has at the statement level, and it is the documented contract in the doc comment — not a defect.

### (b) Quote-state scanner — PASS

Single quotes are toggled only when not inside double quotes, and vice versa (shell.rs:208-209), so `'` inside `"…"` and `"` inside `'…'` are both inert. PowerShell's own backtick escapes and bash's `\` escapes are *not* modelled, so a backslash-escaped quote (`echo \"a && b\"`) would flip the state — but that input is malformed in PowerShell 5.1 regardless, and the failure mode is a missed translation (original command runs and errors visibly), never a mis-split of a valid command. Acceptable.

### (c) Security — PASS

- Approval: `src/agent/dispatch.rs:478` computes `force_prompt = tool.never_auto_for(&parsed_call.arguments)` and `approval::needs_approval(...)` on the **parsed call arguments**, i.e. the original command, *before* `dispatch_with_interrupt` runs the tool. Translation happens strictly inside `execute_streaming` (shell.rs:448-455), after the gate. The `git merge` / `git push` prompt is therefore untouched — pinned by the existing `never_auto_for_reads_the_command_argument` test plus factory.rs:1572-1579.
- Classification: `command_is_core_git_op` (shell.rs:122) and the safety-rules / `cmd_class` path likewise read the original `args.command`; the translated string never reaches them. The filter path (shell.rs:565) also uses `&args.command`, so output filtering keys on the original — correct.
- The translated string is passed as a single `-Command` argument to `powershell` (shell.rs:457-463), unchanged from before; no new injection surface, no shell-string interpolation of the translation.
- Raw stdout/stderr in `data` stay note-free (shell.rs:606-610); `CHAIN_TRANSLATION_NOTE` only shapes the displayed `output`, matching the GREP_NUDGE/REDIRECT_NOTE contract.

### (d) `purpose` required — PASS

`ShellArgs.purpose` is now `String` with no `#[serde(default)]` (shell.rs:262-284) and the `#[expect(dead_code)]` is retained (it is genuinely never read in Rust — it is consumed by the frontend from raw args, `frontend/src/lib/toolCardPaths.ts:683`). Every `shell.execute(...)` / `execute_streaming(...)` call site in the repo carries `purpose`:
- `src/tool/agent/shell.rs` tests: all 14 call sites updated (verified individually — lines 907, 921, 939, 969, 981, 998, 1014, 1028, 1045, 1077, 1091, 1127, 1150, 1175, 1196, 1216, 1259, 1317, 1362, 1366, 1400, 1447, 1498, 1527, 1559, 1564 — including the ones the plan's step 1 did not enumerate).
- `src/agent/factory.rs:1601,1617` updated; `src/agent/tests.rs:4944` already carried it.
- The one remaining raw `{"command":"cargo test"}` at `src/provider/openai/tests.rs:2854` is a **string payload fed to `record_raw_tool_calls`** (a trace-tap test with tool name `file_edit`) — it never reaches `ShellTool` deserialization. Not a call site.
- `never_auto_for`, `has_blinding_redirection`, `is_grep_family`, `approval_preview` and the frontend all read raw `serde_json::Value` args, never `ShellArgs`, so they are unaffected by the tightening. No production code path can now reach `execute_streaming` without `purpose`.

### (e) Multi-platform neutrality — PASS

The sh path is untouched: the `(program, flag)` selection is unchanged (shell.rs:438-442), the translation is inside `if cfg!(target_os = "windows")` and the `else` branch clones the command verbatim with `translated = false`. The integration test `powershell_chain_translation_runs_and_notes` is `#[cfg(windows)]`-gated; the pure translation tests are platform-neutral. No new Windows-only API — the only `#[cfg(windows)]` block is the pre-existing `CREATE_NO_WINDOW` `creation_flags` (shell.rs:467-471), untouched.

### (f) Documentation sync — PASS

- Module doc: the new `CHAIN TRANSLATION` paragraph (shell.rs:24-32) names the PowerShell 5.1 limitation, that approval/classification saw the original, and that `purpose` is now required.
- Schema description (shell.rs:372-384): carries the empty-call trap verbatim ("no zero-argument form") and the chaining advisory ("chain with ; not && / ||"), plus the auto-translate disclosure. The `purpose` property description now says "Required on every call" (shell.rs:390) — consistent with `required: ["command","purpose"]`.
- Budget ceiling comment (factory.rs:1882-1887): follows the file's dated raise convention, names the cause (backlog 79a2755d), the measured figure (26_243) and the deliberate-raise rationale — matching every neighbouring entry.
- No README/PLAN.md surface describes the shell tool's parameter list or chaining behaviour, so nothing there is stale.

### (g) File-tools-first policy — PASS

The diff is two source files edited through the file tools; no `Set-Content`/`Out-File`/`sed -i`/`tee`/python-script mutation anywhere. The only `.coding/` writes are the app-managed backlog status flip and the plan file.

### (h) General correctness — PASS

- The `(command, translated)` tuple avoids the borrow of `args.command` outliving the translation, and `args.command` remains available for the filter/note paths — clean.
- `translated` is only read in the success arm; on spawn failure or timeout the note is irrelevant, which is correct (no misleading note on a call that never ran).
- The note stacks correctly with the grep/redirect notes (each ends in `\n`, `insert_str(0, …)` order documented at shell.rs:584-594).
- Existing invariants preserved: `streaming_result_is_byte_identical_to_the_plain_path` still holds because both paths take the same translation branch.
- Warning-free: `#[expect(dead_code)]` on a now-`String` field is still correct (the field is still never read), and `#![deny(warnings)]` is green.

## Low findings (non-blocking)

**L1 — `.coding/plans/04a195de.md` carries an unrelated 3-line addition.** The diff adds a `## Regression test` section naming `escape_fallback_file_escaped_needle_plain_matches_with_note` to the *previous* plan's file, not this plan's. It is plan bookkeeping for a different plan (04a195de) that travelled in this working tree. Harmless and mergeable, but it is noise in this change set — if the intent was to keep this commit scoped, that hunk could be dropped. Not a correctness issue.

**L2 — degenerate operators produce empty gates.** `&& b` → `; if ($?) { b }` and `a &&` → `a; if ($?) {  }`. Neither panics nor mis-splits; the leading-`;` form is still rejected by PowerShell (the input was invalid to begin with), and the trailing form is a harmless no-op. Documented behaviour is silent on these shapes. Optional hardening: return `None` (no translation) when any segment or the tail is empty, so a malformed chain is surfaced as the original PowerShell error rather than a partially-rewritten one. No test covers these two shapes.

**L3 — escaped-quote blind spot is undocumented.** The doc comment states the scan tracks single/double quote state; it does not mention that backslash/backtick-escaped quotes are not modelled. Since the only consequence is a missed translation of an already-invalid command, this is a comment-completeness nit rather than a defect.

No high findings. The two failure classes named in the goal are addressed with regression tests that fail without the fix (`missing_purpose_is_rejected`, `powershell_chain_translation_rewrites_both_operators`, `powershell_chain_translation_leaves_valid_commands_alone`, `powershell_chain_translation_runs_and_notes`, `schema_description_names_the_calling_traps`), and the security-critical property — that approval and the core-git-op gate see the untranslated command — is verified against the actual dispatch path, not just asserted.