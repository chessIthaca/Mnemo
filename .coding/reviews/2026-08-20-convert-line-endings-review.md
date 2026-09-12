# Review: `convert_line_endings` agent tool (feat/convert-line-endings)

**Reviewer:** read-only reviewer (spawn_agent)
**Scope:** ALL uncommitted changes on `feat/convert-line-endings` (`git status` / `git diff HEAD`), plus the untracked new tool file `src/tool/agent/convert_line_endings.rs` (read in full, 409 lines) and every integration point it touches.

## What was reviewed

- `src/tool/agent/convert_line_endings.rs` (NEW, untracked) — full file, incl. all 13 tests.
- `src/tool/agent/mod.rs` — `pub mod` + doc-list bullet. ✓
- `src/agent/factory.rs` — import, registration in `register_agent_tools`, expected-name array in `expected_tool_names_registered_when_fully_wired` (asserts each name registered + total count, factory.rs:1183-1189). ✓
- `src/agent/approval.rs` — `is_project_scoped` file-tools arm + doc comment. ✓
- `src/safety_rules.rs` — `key_argument` file-tools arm + doc comment. ✓
- `src/agent/turn.rs` — `is_durable_tool` file-mutations arm (:1568). ✓ (the fn's doc comment does not enumerate the file tools, so no stale doc there.)
- `src/agent/prompt.rs` — one-line TOOL_STRATEGY bullet after `read_files`, before `web_fetch`. ✓
- `.coding/backlog.json`, `.coding/plans/stack.json`, `.coding/plans/85ddb268-….md` — app-generated bookkeeping, normal churn.
- Cross-checked against: `src/tool/agent/line_endings.rs` (`normalize_line_endings` semantics), `src/tool/agent/file_append.rs` (the claimed pattern), `src/tool/agent/sandbox.rs` (`validate_for_write` ladder, `protected_refusal`), `src/tool/mod.rs` (`approval_preview` default `None` at :138-140), `frontend/src/components/chat/ApprovalPrompt.tsx`, `.gitattributes`.

## Verified correct (no findings)

- **Counting logic** (convert_line_endings.rs:148-151): `bare_lf = matches('\n') - crlf` and `lone_cr = matches('\r') - crlf` cannot underflow (every `"\r\n"` contains exactly one `\n` and one `\r`). Hand-verified edge cases: `"\r\n"` → (1,0,0); `"\n\r"` → (0,1,1); `"\r\r\n"` → (1,0,1) with `normalize_line_endings` producing exactly 2 endings — count/conversion consistent.
- **No-op logic** (:153-174): `total == 0` returns before the `already` check; `already` is correct for both targets; both no-op paths return success **without writing**, preserving mtime — matches the code comment.
- **Conversion**: reuses the shared helper (:176); lone `\r` collapses correctly; idempotent (test `conversion_is_idempotent` covers the already-CRLF no-op path).
- **Error ordering**: args → `to` parse (before any FS access; test proves file untouched) → sandbox validate → is_file → UTF-8 read → write. Matches file_append's structure and error strings (`"path validation failed: {e}"`).
- **Security**: full `validate_for_write` ladder (canonicalize → `starts_with(root)` → protected refusal incl. NTFS ADS guard); binary corruption refused via `read_to_string` UTF-8 check with byte-identical-afterwards test; no `approval_preview` override is correct (trait default `None`, consistent with file_append — a whole-file churn diff is noise).
- **Convention/consistency**: struct/schema/safety/spawn_blocking shape mirrors file_append; schema description accurate (`UTF-8 text files only`, no-op claim, enum `lf`/`crlf`; case-insensitive parse accepts strictly more than the enum — safe direction); prompt bullet accurate; 13 tests counted, covering both directions, mixed+lone-CR, no-ops, missing file, invalid `to`, case-insensitivity, protected refusal, non-UTF-8, empty file, idempotence, multi-byte/no-trailing-newline preservation.
- **Constitution**: doc comments on module, struct, `new()`, and the private `parse_target`; no `#[allow]`; `.gitattributes` pins `eol=lf` and the git CRLF warnings on approval.rs/safety_rules.rs are the pre-existing repo-wide state (edited lines show no `^M` in the diff — no mixed endings introduced).

## Findings

### LOW

**L1 — Frontend `isProjectScoped` not updated; "Allow for project" button hidden for this tool.**
`frontend/src/components/chat/ApprovalPrompt.tsx:17-30` hardcodes the project-scoped file tools (`file_edit`/`file_write`/`file_append`/`file_read` + `search`/`git`) and was not updated, while the backend mirror `src/agent/approval.rs:121` was. Consequence: on a `convert_line_endings` approval prompt the "Allow for project" button never appears (it falls to the `default => false` arm), even though the backend considers the call project-scoped and AutoApproveProject mode auto-approves it correctly. Impact is UX-only (the user can still Approve / Mark Safe), but the two `is_project_scoped` implementations are meant to mirror each other and now diverge. Fix: add `case "convert_line_endings":` to the path-arg arm and update the stale doc comment at ApprovalPrompt.tsx:13-15 (which enumerates the file tools). (Pre-existing note, not this change's defect: `search_read`/`read_files` are absent from that list too.)

**L2 — Integration-point tests assert all four *sibling* file tools but not the new one.**
- `src/agent/approval.rs:352-363` (`is_project_scoped_file_inside`) asserts `file_read`/`file_write`/`file_edit`/`file_append` are project-scoped — no `convert_line_endings` assertion, so deleting the new match arm would fail no test.
- `src/safety_rules.rs:486-503` (`signature_file_tools_use_path`) asserts signatures for the same four siblings — no `convert_line_endings:<path>` case, so removing it from the `key_argument` arm would fail no test (it would silently fall to the empty-key default, degrading safety-rule granularity to tool-wide).
The new tool has excellent coverage of its own behavior (13 tests); these two one-line assertions close the gap at the wiring points, matching each file's own testing convention (the factory name-set test already guards registration). (`is_durable_tool` has no per-tool unit tests at all — only the call site at turn.rs:1264 — so no convention is violated there, though an assertion would be equally cheap.)

**L3 — `validate_for_write` creates parent dirs on a path the tool then rejects as missing.**
`convert_line_endings` requires an existing file, but routes through the creation ladder (`sandbox.rs:227-252`), whose step 4 (`create_dir_all`, :239-243) runs before the tool's `is_file()` check (convert_line_endings.rs:125). So `convert_line_endings {path: "no/such/dir/f.txt"}` creates `no/such/dir/` on disk and *then* errors "does not exist or is not a file" — filesystem litter on an error path, inside the sandbox so not a security issue. `file_edit` (the other existing-file-only tool) uses `validate` + `refuse_if_protected` instead, which has no mkdir side effect; that pattern fits this tool better. (The plan explicitly chose `validate_for_write` for consistency with file_write/file_append, so this is a deliberate trade-off — flagged for reconsideration, not as a bug.)

### INFO

**I1 — Stale consumer lists in sandbox.rs doc comments.**
Three pre-existing doc comments enumerate the file-tool consumers and are now incomplete: `validate_for_write` (:210, "used by `file_write` + `file_append`"), `is_protected_write_target` (:160-162, "only blocks the file agent tools (`file_write`/`file_edit`/`file_append`)"), `protected_refusal` (:265-267, "ONE message across all file tools (`file_write`/`file_append`/`file_edit`)"). `convert_line_endings` is a fourth consumer of all three. Doc-accuracy only.

## Conclusion

No correctness, security, or constitution-compliance findings. The tool's logic (counting, no-op, conversion, error paths) is correct and well-tested; all Rust integration points are wired consistently. Three LOW findings: the frontend `isProjectScoped` mirror was missed (L1), two wiring-point tests lack a case for the new tool (L2), and the creation-ladder mkdir side effect on the missing-file error path (L3); plus one INFO doc-staleness note (I1). Recommend fixing L1-L3 before merge.
