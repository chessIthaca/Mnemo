# Phase 6b — Safety Hardening Leftovers (M5/M6/M4-strict) Review

**Date:** 2026-08-11
**Reviewer:** read-only subagent (spawned review)
**Scope:** ALL uncommitted changes in the working tree (`git diff HEAD`), with focus on the three Phase 6b findings.
**Branch:** feat/bookkeeping-tools-autorun

## Files changed (code)
- `src/tool/agent/sandbox.rs` — M5: expanded `is_protected_write_target` + tests
- `src/tool/agent/file_write.rs` — M5: creation-gap closure + message updates + tests
- `src/tool/agent/file_edit.rs` — M5: message updates
- `src/tool/agent/file_append.rs` — M5: message update
- `src/config/keys.rs` — M6: `restrict_permissions` (Unix chmod 0600 + Windows user-only DACL) + tests
- `Cargo.toml` / `Cargo.lock` — M6: `windows-sys` 0.59 target-specific dependency
- `src/config/general.rs` — M4-strict: `run_all_strict_success` flag + tests
- `src-tauri/src/ipc/run_all.rs` — M4-strict: `strict_success_allows_commit` + `main_agent_workflow_state` + gate wiring + tests
- `start.bat` — unrelated: `npm install` sync step before Tauri CLI guard

## Files changed (bookkeeping, expected)
- `.coding/backlog.json`, `.coding/plans/*.md`, `.coding/plans/stack.json` — backlog item added, plan checkboxes flipped, stack pointer updated. Not code.

## Verification performed
- `cargo check` (Windows host) — clean, exit 0. Confirms all `windows-sys` FFI symbols resolve.
- `cargo test --lib` — **520 passed; 0 failed.**
- `cargo test -p myharness-app` (tauri crate, includes run_all strict-gate tests) — **49 passed; 0 failed.**
- `cargo test --test workflow_integration` — **5 passed; 0 failed.**
- `npx tsc --noEmit` (frontend) — clean, exit 0.
- `npm test` (vitest) — **78 passed; 0 failed.**
- `cargo clippy --lib -p myharness` — 13 warnings, 4 in new code (all minor style nits; see Constitution Compliance).
- Cross-checked every `windows-sys` 0.59 FFI signature against the crate source in `~/.cargo/registry/.../windows-sys-0.59.0/` to validate parameter types, struct layouts, and constant values.
- Confirmed the dedicated workflow tools (`create_plan`/`complete_step`/`update_plan`/`abandon_plan` in `src/workflow/mod.rs`) write `.coding/plans/` via their own `std::fs` paths and never call `is_protected_write_target` (grep: 0 matches in `src/tool/workflow/`).

---

## Correctness

### M5 — Protected write targets
**No findings.** The expansion is correct and complete:

1. **Protected set is right.** `sandbox.rs:174-179` now blocks `.coding/memory.db` (+ wal/shm), `.coding/safety.toml`, `.coding/backlog.json`, and the entire `.coding/plans/` subtree via `starts_with(".coding/plans/")`. The `starts_with` prefix correctly covers `stack.json` and any `<uuid>.md` plan file.

2. **`.coding/reviews/` stays writable — verified.** `starts_with(".coding/plans/")` does not match `.coding/reviews/...` (different second component). Unit test `allows_writing_review_reports_under_coding` (sandbox.rs:358) and integration test `allows_writing_review_reports` (file_write.rs) both assert this. Reviewer subagents can still write reports. ✓

3. **Dedicated workflow tools are NOT blocked — claim verified.** `src/workflow/mod.rs` `create_plan` (line 236-237), `update_plan` (line 357), `complete_step` (line 377), `abandon_plan`/`persist_stack` (line 480) all use `std::fs::write`/`create_dir_all` directly. Grep for `is_protected_write_target` in `src/tool/workflow/` returns 0 matches. Expanding the protected set only blocks the file *agent* tools. ✓

4. **Creation gap closed in `file_write`.** `file_write.rs:106-119`: when `validate` fails (non-existent path), the code now calls `validate_for_creation`, then checks `is_protected_write_target` on that lexical result *before* creating parent dirs. Test `refuses_to_create_nonexistent_protected_path` (file_write.rs) confirms a fresh `.coding/plans/stack.json` is refused and the directory is never created. ✓

5. **Creation gap also closed in `file_append`.** `file_append.rs:89-106`: the same `validate` → fallback `validate_for_creation` → `is_protected_write_target` pattern. A non-existent protected path appended-to is refused. ✓ (Not explicitly called out in the plan, but correctly handled — the append tool's `create(true)` OpenOptions would otherwise create the file.)

6. **`file_edit` has no creation path** — it `read_to_string`s existing content (file_edit.rs:242), so there is no creation gap to close there. The existing-path protected check (file_edit.rs:234) suffices. ✓

### M4-strict — Run-All Complete gate
**No findings.** The gate is correct:

1. **Reads the RIGHT agent's state.** `main_agent_workflow_state` (run_all.rs:87-99) resolves the main agent via `state.runtime.manager.lock().await.main_agent_id()` — the parentless smallest-id agent (runtime/mod.rs:124-130), which is the one backlog/Run-All prompts are dispatched to. It then reads *that* agent's workflow via `agent_loops.get(&main_id)` → `workflow_handle()`. Subagents (which have a `parent_id`) are never returned by `main_agent_id()`, so a subagent's mid-flight `Executing` state cannot leak into the gate. ✓

2. **Config path is correct.** `state.project.config.lock().await.general.general.run_all_strict_success` (run_all.rs:572): `config` is `Arc<Mutex<Config>>`, `.general` is `GeneralConfig` (config/mod.rs:31), `.general` is `GeneralSection` (general.rs:17), `.run_all_strict_success` is the new `bool` (general.rs:55). Verified against the type definitions. ✓

3. **Timing is correct.** The strict gate reads the workflow state inside `on_main_turn_resolved`, which fires on the `Finished`/final-`Error` event *after* the turn loop has ended. The agent's `complete_step` tool mutates the workflow to `Complete` *during* the turn (turn.rs:789 locks workflow, plan.rs:116 calls `wf.create_plan`/`complete_step`). By the time `on_main_turn_resolved` runs, the final `complete_step` has already persisted `Complete`. So a plan the agent genuinely finished reads as `Complete` (allowed), not `Executing`. ✓

4. **Fail-open default (Planning) is the safe choice.** When the main agent or its loop can't be found (teardown, brain-failed-to-build), `main_agent_workflow_state` returns `Planning` (allowed). This is correct: the data-loss risk is committing *unfinished* work, which only happens when we *can* see the state is mid-plan. A missing agent spuriously downgrading a real success would lose completed work. The asymmetry is right. ✓

5. **Downgrade path is sound.** On a strict-mode rejection (run_all.rs:577-589): it rolls back to the checkpoint sha (if present), sets `CantResolve` with a clear note, and sets `downgraded = true` so the success arm is skipped. The `checkpoint_sha` recovery (run_all.rs:559-563) handles both plain-sha and `"<sha> | reason"` note forms. ✓

### M6 — keys.toml permissions
**No findings.** The fail-safe design is correct:

1. **Atomic write lands before restriction.** `KeyStore::save` (keys.rs:111-118) calls `write_atomic` (temp-file + rename, config/mod.rs:331-335) *first*, then `restrict_permissions`. A permission failure cannot lose data — the secrets are already safely on disk via the rename. The error is logged at `eprintln!` (warn) and swallowed. ✓

2. **Idempotent.** Test `restrict_permissions_is_idempotent` confirms repeated saves don't error (the DACL/mode is simply re-applied). ✓

3. **Unix path is correct.** `chmod 0o600` via `PermissionsExt` (keys.rs:139-143). Test `save_restricts_permissions_to_user_only` asserts `mode & 0o777 == 0o600`. ✓

---

## Bugs

**No findings.** No logic bugs identified. The three features behave as specified, error ordering is preserved inside each tool's `spawn_blocking` closure, and the strict gate's lock handling is sound (see Security for the deadlock analysis).

---

## Security

### Windows DACL FFI (M6) — **No findings (verified correct)**

I cross-checked every FFI call against the `windows-sys` 0.59 source in the cargo registry. All parameter types, struct layouts, and constant values match the call site:

1. **`SetEntriesInAclW(1, &ea, null(), &mut new_acl)`** — signature `(u32, *const EXPLICIT_ACCESS_W, *const ACL, *mut *mut ACL)`. `&ea` coerces to `*const EXPLICIT_ACCESS_W`. Returns `WIN32_ERROR` (u32); 0 = success. ✓

2. **`SetNamedSecurityInfoW(...)`** — signature `(PCWSTR, SE_OBJECT_TYPE, OBJECT_SECURITY_INFORMATION, PSID, PSID, *const ACL, *const ACL)`.
   - `path_w.as_ptr() as *const _` → `PCWSTR` (`*const u16`). The wide string is NUL-terminated (`.chain(once(0))`). ✓
   - `1` for `objecttype` → `SE_OBJECT_TYPE` is `i32`, `SE_FILE_OBJECT = 1i32`. Correct value (could use the named constant for clarity — see Constitution). ✓
   - `DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION` → both are `OBJECT_SECURITY_INFORMATION` (u32): 4 | 0x80000000. The `PROTECTED` flag prevents inherited ACEs from re-granting access — correct hardening. ✓
   - `null_mut()` for owner/group (`PSID = *mut c_void`) — unchanged. ✓
   - `new_acl as *const _` for pDacl (`*const ACL`). ✓
   - `null()` for SACL. ✓

3. **`LocalFree(new_acl as *mut _)`** — `LocalFree(hmem: HLOCAL) -> HLOCAL` where `HLOCAL = *mut c_void`. `new_acl: *mut ACL` cast to `*mut c_void` is valid. **`LocalFree` is the correct deallocator** for buffers allocated by `SetEntriesInAclW` (which uses `LocalAlloc`). ✓

4. **SID lifetime — no use-after-free.** `current_user_sid` (keys.rs:231-288):
   - Opens the process token, calls `GetTokenInformation` twice (size query, then fill).
   - The SID pointer (`user.User.Sid`) aliases into the owned `buf: Vec<u8>`.
   - **The SID bytes are copied into a fresh `Vec<u8>` via `sid_bytes.to_vec()` (line 287) BEFORE `buf` drops.** The returned `Vec<u8>` is independent of the token buffer. ✓
   - `restrict_permissions_windows` holds `user_sid` alive across the `SetEntriesInAclW` call (line 179 → 201), so the `ptstrName` pointer is valid during ACL construction. ✓
   - `TRUSTEE_IS_SID` form tells the API to interpret `ptstrName` as a SID pointer (not a wide string), so the `*const u8 as *mut _` (→ `PWSTR` = `*mut u16`) reinterpret is the documented contract. ✓

5. **SID length computation is correct.** `8 + 4 * sub_auth_count` (keys.rs:285) matches the SID layout: Revision(1) + SubAuthorityCount(1) + IdentifierAuthority(6) + SubAuthority[count](4 each). `sub_auth_count` read from offset 1. `IsValidSid` is called as a sanity check before length computation. ✓

6. **`windows-sys` feature flags are complete.** Cargo.toml enables `Win32_Foundation`, `Win32_Security`, `Win32_Security_Authorization`, `Win32_System_Threading`. All four are used:
   - `Win32_Foundation`: `GENERIC_ALL`, `LocalFree`, `CloseHandle`, `HANDLE`
   - `Win32_Security`: `ACL`, `NO_INHERITANCE`, `DACL_SECURITY_INFORMATION`, `PROTECTED_DACL_SECURITY_INFORMATION`, `GetTokenInformation`, `IsValidSid`, `TOKEN_QUERY`, `TOKEN_USER`, `TokenUser`
   - `Win32_Security_Authorization`: `SetEntriesInAclW`, `SetNamedSecurityInfoW`, `EXPLICIT_ACCESS_W`, `SET_ACCESS`, `TRUSTEE_W`, `TRUSTEE_IS_SID`
   - `Win32_System_Threading`: `GetCurrentProcess`, `OpenProcessToken`
   `cargo check` confirms no missing-feature errors. ✓

7. **Fail-safe behavior is correct.** A restriction failure (e.g. SID lookup fails) returns `Err` from `restrict_permissions`, which `KeyStore::save` logs and swallows (keys.rs:115-119). The atomic write has already landed. A missing restriction is a hardening gap, not a data-loss risk. ✓

### Protected-path expansion (M5) — **No findings**

The expansion correctly prevents a model (or an approved write) from corrupting restart/resume state (`stack.json`, plan markdown), safety rules (`safety.toml`), and the backlog/Run-All state (`backlog.json`) via the file agent tools. The lexical `starts_with` check works on both canonical (`validate`) and normalized (`validate_for_creation`) paths because both are root-absolute and the `strip_prefix(root)` + forward-slash normalization makes the relative-string comparison consistent. The creation gap is closed in both `file_write` and `file_append`.

### Deadlock analysis (M4-strict) — **No findings (no deadlock)**

`main_agent_workflow_state` acquires locks in the order: `manager` (dropped after `main_agent_id()`) → `agent_loops` (held) → `workflow`. The concern is whether any path holds `workflow` and then awaits `agent_loops` or `manager`, creating a cycle.

- **`get_workflow_state`** (agent.rs:344-349): same `agent_loops` → `workflow` order. Established pre-existing convention.
- **`spawn_agent`** (spawn.rs:90-110): locks `workflow` (line 91), **drops it** (end of `if` block, line 93), then locks `manager` (line 96, dropped line 106), then `agent_loops` (line 110). Never holds `workflow` while acquiring `agent_loops`/`manager`.
- **Workflow tools** (`plan.rs`, `skill.rs`): hold `workflow` but only do `std::fs` writes inside — grep confirms 0 `manager.lock`/`agent_loops.lock` calls in `src/tool/`. No nested lock acquisition.
- **Event forwarder `Exited` arm** (events.rs:322-325): locks `manager`, drops it, then `agent_loops`. No `workflow` lock held.

No path holds `workflow` while awaiting `agent_loops` or `manager`. The new code follows the existing `agent_loops` → `workflow` ordering. **No lock-ordering cycle, no deadlock.** ✓

---

## Constitution Compliance

**No blocking findings.** Minor nits only:

1. **Doc comments on public functions — compliant.** `strict_success_allows_commit` (run_all.rs:63-75) has a full doc comment. The private helpers (`restrict_permissions`, `restrict_permissions_windows`, `current_user_sid`) all have doc comments too despite being private (good practice). `run_all_strict_success` field (general.rs:47-55) is documented. ✓

2. **No drive-by refactors — compliant.** Every change is scoped to the three declared findings. The `start.bat` change is explicitly noted as unrelated (and is a legitimate standalone fix). Message-text updates in the three file tools are part of M5 (the protected set changed, so "the memory DB" is no longer accurate). ✓

3. **Line-ending preservation — compliant.** The file tools normalize line endings via their existing `detect_line_ending`/`normalize_line_endings` helpers; no mixed endings introduced. Git's CRLF warnings on `.coding/*.json`/`.md`/`Cargo.lock` are bookkeeping files (expected, not code). ✓

4. **Tests run before marking complete — compliant.** All test suites green (see Verification). ✓

5. **Clippy nits in new code (non-blocking, style only):**
   - `src/config/keys.rs:271, 277` — `std::io::Error::new(ErrorKind::Other, msg)` could be `std::io::Error::other(msg)` (clippy `io_other_error`). These are in `current_user_sid`'s null-SID/invalid-SID error paths. Pure style; functionally identical.
   - `src/config/keys.rs:109-110` — "doc list item without indentation": clippy misreads the `+ rename` continuation line in the `save` doc comment as a Markdown list item. This matches the pre-existing doc style in the file (the original `save` doc used the same `+` continuation). False-positive-ish.
   - `src/config/general.rs:24:1` — "this `impl` can be derived" on `Default for GeneralConfig`: pre-existing (the struct already had `#[serde(default)]` + a manual `Default`); the new `run_all_strict_success: false` line was added to the existing manual impl, not introduced by this phase. Not a Phase 6b regression.

   None of these are constitution violations (the constitution requires doc comments + tests + no drive-bys + line-ending preservation, all satisfied). They are optional polish.

---

## Pre-existing / Unrelated Changes

- **`start.bat`**: adds `call npm install` + `if errorlevel 1` guard before the Tauri CLI existence check. Correct batch idiom (`if errorlevel 1` is true when `%ERRORLEVEL%` >= 1). The friendly-failure message was updated from "Run build.bat first" to "Check frontend\package.json" since `npm install` now runs inline. Sensible standalone fix; no correctness issue. Not part of Phase 6b but reviewed for correctness — clean.

---

## Overall Verdict

**APPROVE.** All three Phase 6b findings are implemented correctly and completely:

- **M5** (protected write targets): the expanded set is correct, the creation gap is closed in both `file_write` and `file_append`, `.coding/reviews/` stays writable, and the dedicated workflow tools are confirmed to bypass the check (so they still function). Verified by unit + integration tests.
- **M6** (keys.toml permissions): the Windows DACL FFI is correct (every signature, struct layout, constant, and the SID lifetime/deallocation verified against `windows-sys` 0.59 source), the fail-safe ordering is right (atomic write before restriction; errors logged not fatal), and Unix `chmod 0600` is correct.
- **M4-strict** (Run-All Complete gate): reads the correct (main) agent's workflow state at the correct time (after the turn resolves), the config path is right, the fail-open default is the safe choice, and there is no deadlock (lock ordering matches the established `agent_loops` → `workflow` convention; no path holds `workflow` while awaiting `agent_loops`/`manager`).

All test suites green: 520 lib + 49 tauri + 5 integration + 78 npm + tsc clean. The only items flagged are optional clippy style nits (use `Error::other`, use the `SE_FILE_OBJECT` named constant) — none are bugs, security issues, or constitution violations. No fixes required before commit.
