# Code Review — Phase 6a: spawn_blocking for FS/search/sandbox (Perf H1)

**Reviewer:** read-only reviewer (spawn_agent)
**Date:** 2026-04 (session)
**Scope:** The Phase 6a "spawn_blocking" structural-perf change that offloads all
synchronous file-system I/O in the async agent tools onto tokio's blocking pool
via `tokio::task::spawn_blocking`, so concurrent multi-agent tool use cannot
stall the async runtime.

**What was reviewed (the Phase 6a diff):**
- `src/tool/agent/file_read.rs` — wrap validate + read_to_string + line/byte-cap formatting.
- `src/tool/agent/file_edit.rs` — wrap validate + protected-check + read + prepare_edit + write.
- `src/tool/agent/file_write.rs` — wrap protected-check + validate/validate_for_creation + create_dir_all + line-ending detection + write.
- `src/tool/agent/file_append.rs` — wrap validate + protected-check + line-ending detection + OpenOptions append + write + metadata.
- `src/tool/agent/search.rs` — wrap regex compile + glob + per-file metadata + read_to_string + match loop.
- `src/tool/agent/describe_image.rs` — wrap `load_image_data_url` (validate + std::fs::read + base64) in `describe_image_file`; the async vision call stays async.
- `src/tool/agent/sandbox.rs` — added a module-level doc comment documenting the async-callers contract; NO signature/behavior change.

**How the review was done:** Each new tool body was read in full and compared
line-by-line against its pre-change state (`git show 72dfb86^:<file>`), then
re-verified against the final committed tip of the feature branch
(`git show 1387279:<file>` — confirmed identical for all six tool files; the
only tip delta is the sandbox.rs doc comment). The deferred items were verified
by reading the actual call sites. Tests were run (see Test Results).

**Branch / working-tree note:** At review time the working tree on `main` was
clean; the Phase 6a work lives on branch `feat/bookkeeping-tools-autorun`
(commits `72dfb86` steps 1-5 and `1387279` steps 6-9, tip `1387279`). The review
was performed on that branch. `git status` on the branch is clean (the sandbox
doc comment is committed in `1387279`, not left uncommitted).

## Test Results (run on the feature branch, tip 1387279)

- `cargo test --workspace` — **exit 0**. Per-binary results, all `0 failed`:
  lib unittests 508 passed; 1 passed; 9 passed; integration 5 passed; ipc 47 passed;
  doc-tests 0. The existing per-tool unit tests pass unchanged.
- `npx tsc --noEmit` (frontend) — **exit 0**.
- `npx vitest run` (frontend) — **exit 0**, 78 passed / 5 files.
- Compiler warnings on the changed files: **none**. The only 2 warnings are
  pre-existing `unused_mut` in `src/runtime/agent.rs:448,511` (unrelated, as the
  commit message states). No unused imports were introduced by the wrapping.

## Findings by severity

### Correctness / behavior preservation

**No findings.** The wrap is behavior-preserving across all seven files. Each
tool's `execute` parses args *before* the closure (so the `invalid arguments`
error still returns directly from `execute`), then moves the owned args + a
cheap `Sandbox` clone (or an owned `PathBuf`, in search) into one
`spawn_blocking` closure and `.await.unwrap_or_else(|e| ToolResult::error(...))`
the `JoinError`. The blocking body is moved verbatim — same statements, same
order, same strings. Verified per file:

- **file_read.rs** — error ordering: validate → read → formatting → caps.
  Strings identical: `"path validation failed: {e}"`, `"failed to read '{}': {e}"`.
  Caps preserved: `DEFAULT_MAX_LINES` (2000), `DEFAULT_MAX_BYTES` (102_400),
  the `default_capped` vs `truncated_by_bytes` note logic, and the
  `available_from_start` computation all byte-identical inside the closure.
- **file_edit.rs** — error ordering: validate → protected-check → read →
  `prepare_edit` → write. Strings identical, including the protected message
  `"refused: '{}' is a protected live-state file (the memory DB) — use the
  memory tools instead of file_edit"` and the success `edited {}` + `diff` data.
  The pure checks (`prepare_edit`, `is_protected_write_target`) run inside the
  closure consuming `args`/`content`/`validated` produced there — ordering
  unchanged.
- **file_write.rs** — error ordering: protected-check (best-effort `validate`
  inside `if let Ok`) → validate / `validate_for_creation` fallback →
  `create_dir_all` → re-validate → line-ending detection → write. The
  `Path::new(&args.path)` is now constructed *inside* the closure (after `args`
  moved in), so `path` borrows `args.path` which the closure owns — **no
  borrow-after-move** (the borrow lives only within the closure body).
  Line-ending preservation (`detect_line_ending_path` + `normalize_line_endings`,
  `None => args.content.clone()` for new/empty files) identical.
- **file_append.rs** — error ordering: validate / `validate_for_creation`
  fallback → protected-check → line-ending → `OpenOptions` open → `write_all` →
  `metadata`. The **metadata-after-write ordering is preserved**:
  `std::fs::metadata(&validated)` is called after `write_all`, so the reported
  `file is now {} bytes` reflects the appended content.
- **search.rs** (highest-leverage) — the regex compile, glob, per-file
  `should_search` (metadata + ignored-dir filter), `read_to_string`, the match
  loop, and the early-exit-at-`MAX_MATCHES` cap are all byte-identical inside the
  closure. `total_matches` increments on every `re.is_match` (even after the
  cap), `results.push` only while `results.len() < MAX_MATCHES`, the `capped`
  flag, `files_searched`/`files_matched`/`skipped` counters, and the summary +
  `"... and more matches (narrow your pattern or use a glob filter)"` note are
  unchanged. The regex is now *built inside* the closure, so the
  `"invalid regex: {e}"` error still returns a `ToolResult::error` with the same
  string. `root = self.sandbox.root().to_path_buf()` clones the root to an owned
  `PathBuf` moved into the closure — no `&self` leak.
- **describe_image.rs** — the change is only in `describe_image_file`. The
  blocking `load_image_data_url` (validate + `std::fs::read` + base64) runs in
  `spawn_blocking`; the async `describe_image_data_url` (vision network call)
  stays async and runs after `.await`. The double-`??`
  (`.map_err(|e| Error::Tool(...))??`) **correctly flattens** the
  `Result<Result<String>, JoinError>`:
  - success `Ok(Ok(String))` → yields `String`;
  - `JoinError` (panic/cancel only) → `.map_err` → `Err(Error::Tool(...))` → first `?` returns it;
  - a real load error (`InvalidInput`/`PathOutsideRoot`/`Io`) → `Ok(Err(Error))` → first `?` yields the inner `Err` → second `?` returns it.
  The only error-type change is that a *JoinError* (never occurs in normal
  operation) now surfaces as `Error::Tool(...)` instead of whatever the inner
  error would have been — but a JoinError only happens on task panic/cancel, so
  real-failure semantics are identical, and `execute` wraps any error in
  `"describe_image failed: {e}"` regardless of variant. `path.to_string()` clones
  the path into an owned `String` moved into the closure (the `vision`
  reference is borrowed *outside* the closure and used after `.await` — no
  `'static` violation).

**JoinError `.unwrap_or_else(...)` masking (all tools):** The
`.await.unwrap_or_else(|e| ToolResult::error("... task failed: {e}"))` path only
fires on a `JoinError` (closure panic or runtime shutdown). On the success path
the closure returns the real `ToolResult`, which `.unwrap_or_else` passes
through. A JoinError cannot occur during normal operation, so this does not mask
any legitimate I/O/validation error and does not change success/error
semantics. The five `Tool` tools use `ToolResult::error(...)`; describe_image
uses `Error::Tool(...)` (then `??` + `execute`'s `describe_image failed: {e}`).
Consistent and correct.

### Bugs

**No findings.** The closures are genuinely `'static + Send`:

- `Sandbox` is `#[derive(Debug, Clone)]` over a single `PathBuf` (`sandbox.rs:40-43`).
  `PathBuf: Clone + Send + Sync`, so `Sandbox: Clone + Send + Sync` — a moved
  clone satisfies `'static + Send`.
- The args structs (`FileReadArgs`, `FileEditArgs`, `FileWriteArgs`,
  `FileAppendArgs`, `SearchArgs`, `DescribeImageArgs`) are owned aggregates of
  `String`/`Option<usize>`/`bool`/`Option<String>` — all `Send`. They are moved
  (owned) into the closure.
- No `&self` borrow leaks into any closure. Verified by grep: each tool does
  `let sandbox = self.sandbox.clone();` (or `let root =
  self.sandbox.root().to_path_buf();` in search) immediately before
  `spawn_blocking(move || {...})` and uses only the clone/owned value inside.
  The remaining `self.sandbox.` references (file_edit.rs:278-279,
  file_write.rs:165-169) are in the **sync `prepare_for_approval`** method (the
  deferred approval-preview path), not in the `execute` closure.
- `regex::Regex` is `Send + Sync`, but it is moot here: search builds it
  *inside* the closure, and file_edit builds it inside `prepare_edit_regex`
  called inside the closure — it never crosses the spawn boundary.
- `Path::new(&args.path)` in file_write/file_append is constructed inside the
  closure, borrowing `args.path` which the closure owns — legal, not a
  borrow-after-move.
- `.await` is correctly placed directly on the `spawn_blocking(...)` future in
  every tool; no `.await` is nested inside a `spawn_blocking` closure (the
  describe_image vision `.await` is outside and after the blocking closure).

### Security

**No findings.** Sandbox validation still runs before any I/O in every tool —
the `validate`/`validate_for_creation` calls are the first operations inside
each closure (or, for file_write, the protected-check's best-effort `validate`
runs first, then the authoritative validate/validate_for_creation before any
dir creation or write). No path-traversal regression: `validate` canonicalizes
and checks `starts_with(root)`; `validate_for_creation` lexically normalizes
`..` and rejects outside-root. Protected-file checks (`is_protected_write_target`,
guarding `.coding/memory.db` + WAL/SHM sidecars) still run in file_edit,
file_write, and file_append in the same position. No change to which paths are
allowed or protected.

### Constitution compliance

**No findings.**

- **Doc comments:** No new public functions/structs were added (the closures are
  inline and private). The modified `pub fn`s (`prepare_for_approval` in
  file_edit/file_write, `describe_image_file`, `load_image_data_url`, etc.) kept
  their existing doc comments. The new sandbox.rs module doc comment is accurate.
- **sandbox.rs doc comment accuracy:** Verified the two referenced sync callers
  exist at the stated paths and behave as described:
  - `crate::agent::approval::is_project_scoped` — exists at
    `src/agent/approval.rs:103`; a sync `pub fn` that calls
    `sandbox.validate(Path::new(path_str))` once (line 112), used as the
    `AutoApproveProject` predicate. The "async-ifying it would make
    `needs_approval` + its callers async (a larger cross-cutting seam);
    low-leverage (one syscall, infrequent)" justification is sound.
  - `shell::resolve_cwd` — exists at `src/tool/agent/shell.rs:72`; a sync `fn`
    that calls `self.sandbox.validate(...)` once (line 78) + `is_dir()`, invoked
    from the shell tool's async `execute` whose command runs via
    `tokio::process::Command`. The "one validate + is_dir per command; the
    command itself already runs async; low leverage" justification is sound.
  - The doc correctly states `validate`/`validate_for_creation` stay sync
    `pub fn` and that the six tools now wrap their whole blocking work
    (including the validate call) in one `spawn_blocking` closure.
  - `validate`/`validate_for_creation` signatures and bodies are byte-identical
    to the pre-change state — the only sandbox.rs change is the added doc text.
- **Line-ending style:** The modified `.rs` files are CRLF and stay CRLF.
  Verified the working-tree `sandbox.rs` has CRLF with no bare LF; the
  committed tool files were authored with the file tools (which preserve CRLF).
  No mixed `\r\n`/`\n` endings introduced.
- **No direct commits to main:** The work is on `feat/bookkeeping-tools-autorun`,
  not `main`.
- **Tests run before completion:** `cargo test --workspace` (exit 0), `tsc
  --noEmit` (exit 0), `vitest run` (78 passed) — all green.

## Deferred items — justification verification (per the task)

The plan defers four items; each was verified to be a *real* concern that is
*correctly out of scope* for this behavior-preserving perf pass:

1. **Rust delta coalesce before fan-in** — deferred as "optional"; a larger
   cross-cutting change to the event contract hardened in Phase 5c. Out of
   scope for a behavior-preserving spawn_blocking pass. **Sound.** (Not in the
   reviewed diff; no change to the streaming/delta path.)

2. **Memory read-connection pool** — deferred as "if needed"; WAL read/write
   split already a strength, no measured contention. **Sound.** (No memory-pool
   code in the diff; `src/memory/mod.rs` is unchanged by Phase 6a.)

3. **`approval_preview` / `prepare_for_approval` async-ification** — **real
   concern, correctly deferred.** Verified: `approval_preview` is a *sync* trait
   method (`src/tool/mod.rs:139: fn approval_preview(&self, _args: &Value) ->
   Option<ApprovalPreview>`) called on the async dispatch path at
   `src/agent/dispatch.rs:162` (`tool.approval_preview(&parsed_call.arguments)`).
   The file_edit/file_write impls (`file_edit.rs:209-212`, `file_write.rs:63-66`)
   call the sync `prepare_for_approval`, which does a blocking
   `std::fs::read_to_string` + diff. So a blocking read still happens on the
   async runtime *for the approval-preview path only*. Making it async would
   change the `Tool` trait signature + every tool impl + dispatch — a larger
   seam, correctly deferred. Note for awareness (not a finding against this
   pass): the approval-preview path remains the one remaining sync-FS-on-async
   seam, but it is low-frequency (only for `NeedsApproval` writes, before the
   user approves) and was explicitly out of scope.

4. **`is_project_scoped` + `shell::resolve_cwd`** — **real, correctly kept
   sync.** Each performs a single `validate` (one canonicalize) on the async
   path. Verified both call sites (above). The "single cheap canonicalize,
   low-leverage, async-ification is a larger seam" justification is sound.

## Conclusion

The Phase 6a spawn_blocking change is a clean, behavior-preserving
refactor. All seven files preserve exact validation, error ordering, error
strings, caps, line-ending handling, and (for search) the early-exit/total
semantics. The closures are `'static + Send`, no `&self` borrows leak, and no
new warnings were introduced. The sandbox.rs doc comment is accurate. Tests
are green (`cargo test` exit 0, tsc exit 0, vitest 78 passed). The four
deferred items are real concerns correctly justified as out of scope.

**No findings** — the diff is clean.