# Security Review — myharness (2026-09-15)

**Perspective:** Security: path traversal, command injection, secret handling,
approval-gate integrity, IPC boundary.
**Reviewer:** read-only security review (conducted in-session).
**Grounding:** `src/tool/agent/sandbox.rs`, `src/tool/agent/shell.rs`,
`src/tool/agent/git.rs`, `src/config/keys.rs`, `src/agent/approval.rs`,
`src/agent/dispatch.rs`, `src-tauri/src/ipc/approval.rs`,
`src-tauri/src/ipc/run_all.rs`, `src/provider/openai.rs`.

---

## Overall verdict: **Strong**

No critical or high-severity security issues found. The security model is
defense-in-depth: path sandboxing + canonicalization, argv-based command
execution (no shell injection), flag-injection guards, an approval gate with a
hard core-operation invariant, secret redaction in Debug, and restrictive file
permissions on `keys.toml`. The protected-write-target set correctly blocks the
file tools from corrupting live app state.

---

## Findings

### S1 — `shell` tool executes arbitrary commands by design (Accepted risk)

**What:** `src/tool/agent/shell.rs:136-148` spawns `powershell -Command <cmd>`
(Windows) or `sh -c <cmd>` (Unix) with the model-supplied `command` string. The
cwd is sandbox-validated (`resolve_cwd`, `shell.rs:72-85`), but the command
itself is arbitrary.

**Assessment:** This is **by design** — the shell tool is the agent's escape
hatch for running builds/tests/git. The security control is the **approval
gate**: `shell` is `SafetyLevel::NeedsApproval` (`shell.rs:121-123`) and
`is_project_scoped` returns `false` for shell (`approval.rs:99-100`), so it
*always* prompts under every mode except Autonomous (and even then the user
chose Autonomous). The `command_class` safety-rule system
(`src/safety_rules.rs:314-353`) auto-approves only same-class commands, rejecting
chained (`&&`/`;`) or unknown commands (`classify` returns `None`).

**Risk:** Accepted. The shell tool is the intended privileged escape hatch,
gated by approval. No action needed.

### S2 — Path sandbox is correct: canonicalization + traversal rejection (Strength)

**What:** `src/tool/agent/sandbox.rs:78-109` `Sandbox::validate`:
- Resolves relative paths against root.
- If the path exists, canonicalizes fully (resolves symlinks + `..`).
- For non-existent paths, canonicalizes the parent (if it exists) + appends the
  file name.
- `check_inside` rejects anything not `starts_with(root)`.
- `validate_for_creation` (`sandbox.rs:121-134`) lexically normalizes `..`
  without touching the filesystem (no symlinks to resolve for a non-existent
  path) and checks `starts_with(root)`.

**Assessment:** Correct. Path traversal (`../../etc/passwd`) is caught by
canonicalization. Symlinks pointing outside are caught by canonicalization.
Absolute paths outside root are caught by `starts_with`. The non-existent-parent
case is rejected. Tests cover traversal, absolute-outside, and the creation gap
(`sandbox.rs:333-356`).

### S3 — Git flag-injection guard is correct (Strength)

**What:** `src/tool/agent/git.rs:71-73` `valid_branch_name` rejects names
starting with `-` or containing whitespace. Applied to `merge`/`checkout`/
`branch create/delete`/`push` branch + remote args (`git.rs:223,244,289,305,327,334`).
Args are passed as discrete `Command` argv (`git.rs:86-87`), never a shell
string — so there's no shell injection; this guards against git-level *flag*
injection (e.g. `--no-commit` as a branch name).

**Assessment:** Correct. Tests verify `merge --no-commit`, `checkout -b`,
`branch --force`, `push --all` are all rejected (`git.rs:596-607,625-638,
747-756,793-800`).

### S4 — Core-operation invariant is enforced by construction (Strength)

**What:** `git merge`/`git push` are core operations
(`git.rs:61-63 is_core_operation`). `GitTool::never_auto_for` returns `true` for
these (`git.rs:173-184`). In dispatch (`dispatch.rs:151`), `force_prompt =
tool.never_auto() || tool.never_auto_for(&args)` forces the interactive approval
prompt **unconditionally** — even under `SafetyMode::Autonomous` and even if a
safety rule would auto-approve. The safety-rules shortcut is skipped when
`force_prompt` is true (`dispatch.rs:159`). `re_evaluate` never auto-resolves
core operations on a mode change (`approval.rs:131-132`).

**Assessment:** This is the hard user-sanction invariant: landing commits on
main / pushing to a remote always requires a contemporaneous user approval. It
cannot be bypassed by mode, rule, or mode-change. Enforced via
`Tool::never_auto_for`, not a prompt instruction. Strong.

### S5 — Secret handling: redacted Debug + restrictive permissions (Strength)

**What:** `src/config/keys.rs`:
- `KeyStore` has a manual `Debug` impl that never prints key values
  (`keys.rs:285-292`) — verified by `debug_does_not_leak_keys` test.
- `KeyStore::save` writes via atomic temp-file + rename, then calls
  `restrict_permissions` (`keys.rs:111-123`).
- `restrict_permissions`: Unix `chmod 0600` (`keys.rs:137-143`); Windows user-only
  DACL via `SetNamedSecurityInfoW` with `PROTECTED_DACL_SECURITY_INFORMATION`
  (`keys.rs:166-225`) — the PROTECTED flag prevents inherited ACEs from
  re-granting access.
- Permission failure is fail-safe: logged at warn, does not block the write
  (secrets are already safely on disk).

**Assessment:** Correct. The 401/403 body-suppression in `fetch_models_with_vision`
(`openai.rs:193-197`) prevents a malicious server from echoing the
`Authorization: Bearer {key}` header back into the UI. Strong.

### S6 — Protected write targets block file-tool corruption (Strength)

**What:** `src/tool/agent/sandbox.rs:170-180` `is_protected_write_target` blocks
the file agent tools from writing: `memory.db` + `-wal`/`-shm` sidecars,
`.coding/safety.toml`, `.coding/backlog.json`, the entire `.coding/plans/` tree.
The check works on both canonical paths (from `validate`) and lexically
normalized ones (from `validate_for_creation`), closing the creation gap
(`sandbox.rs:333-356`). Reviewer reports under `.coding/reviews/` stay writable
(`sandbox.rs:358-372`).

**Assessment:** Correct. A model (or an approved write) cannot corrupt the live
memory DB, safety rules, backlog, or plan workflow. The dedicated workflow tools
write `.coding/plans/` through their own paths (not routed through this check).

### S7 — Run-All never auto-approves (Strength)

**What:** `src-tauri/src/ipc/run_all.rs:13-19` documents the safety model: if
the agent requests an approval mid-run, the loop stops dispatching and waits for
the user — it never auto-approves. The in-flight item's checkpoint sha is
preserved in its `note`. Git checkpoint/commit/rollback use discrete argv
(`run_all.rs:107-129`), not shell strings.

**Assessment:** Correct. The unattended loop respects the approval gate; it
cannot silently approve a mutating action.

### S8 — `restrict_permissions` Windows SID handling uses raw pointer arithmetic (Low)

**What:** `src/config/keys.rs:231-282` `current_user_sid` does manual
`unsafe` pointer arithmetic to extract the SID bytes from `TOKEN_USER`:
`sid_len = 8 + 4 * sub_auth_count` (`keys.rs:278-279`).

**Assessment:** The math is correct per the SID layout
(Revision(1) + SubAuthorityCount(1) + IdentifierAuthority(6) + SubAuthority[]).
`IsValidSid` is checked (`keys.rs:273`). The `unsafe` is unavoidable for the
Win32 API. The only residual risk is if a future Windows version changed the SID
layout — but SIDs are a stable Win32 ABI contract. Low risk; no action needed.

---

## Strengths

1. **Path sandbox** — canonicalization + traversal/symlink rejection + creation-gap closure.
2. **No shell injection** — all commands use discrete `Command` argv.
3. **Flag-injection guards** — `valid_branch_name` rejects leading `-` / whitespace.
4. **Core-operation invariant** — `never_auto_for` enforces approval by construction.
5. **Secret redaction** — manual Debug impl + 401/403 body suppression.
6. **Restrictive permissions** — Unix 0600 / Windows user-only DACL, fail-safe.
7. **Protected write targets** — file tools can't corrupt live app state.
8. **Run-All respects the gate** — never auto-approves.

## Remediation order

1. **S8** (Low) — no action; the `unsafe` SID math is correct and unavoidable.
2. Everything else is a strength — no action required.
