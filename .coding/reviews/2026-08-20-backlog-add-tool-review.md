# Review: backlog_add tool + memory-supersession backlog item 68

**Branch:** feat/backlog-add-tool · **Date:** 2026-08-20
**Scope:** all uncommitted changes (`git status --short` / `git diff HEAD`), not just the named files.

Changed files:
- `src/tool/workflow/backlog.rs` — NEW FILE (untracked, see finding 2)
- `src/tool/workflow/mod.rs` — `pub mod backlog;` + module doc
- `src/agent/factory.rs` — `backlog` field, `set_backlog`, gated registration, test updates
- `src-tauri/src/main.rs` — `set_backlog(Arc::clone(&backlog))` in the Ready branch
- `.coding/backlog.json` — item 68 added, `next_id` 68→69, whole-file reformat (PowerShell)
- `.coding/plans/stack.json` — bookkeeping churn (expected)

---

## CRITICAL

### 1. `backlog_add` is registered but NEVER visible to the model — the feature is functionally dead
`src/tool/mod.rs` — `ToolFilter::allows` (lines ~189–306). Every state's `ToolCategory::Workflow` arm is an explicit name allow-list:
- **Planning** (line ~205): `create_plan | skill_start | ask_user | current_plan`
- **Executing** (line ~231): `complete_step | create_plan | update_plan | abandon_plan | ask_user | current_plan`
- **Reviewing** (line ~259): `finish | ask_user | current_plan`
- **Complete** (line ~285): `create_plan | skill_start | ask_user | current_plan`
- **Skill** (line ~293): `ask_user | current_plan | allow-list`

`"backlog_add"` appears in NONE of them, so `ToolRegistry::schemas()` (line ~348: `filter.allows(category, safety, name)`) drops it from the LLM tools array in every workflow state. The module doc claims "the same gating as `create_plan`" (`backlog.rs:18–20`), but `create_plan` IS allow-listed and `backlog_add` is not. The agent can therefore never call the tool — the entire feature does nothing in practice.

The registry tests went green because `expected_tool_names_registered_when_fully_wired` checks `registry.get(name)` (raw registry lookup), not filter visibility — the exact gap that let this slip through.

**Fix:** add `name == "backlog_add"` to the `Workflow` arms of Planning, Executing, Reviewing, and Complete in `ToolFilter::allows` (a benign, AutoRun, user-visible-and-deletable note is safe in all base states; at minimum Planning + Executing, but nothing in the design justifies hiding it in Reviewing/Complete). **Add a regression test** asserting `backlog_add` is present in `registry.schemas(&caps, &ToolFilter::Planning)` (and the other states) — mirroring `planning_filter_hides_write_tools_until_a_plan_exists` in `src/tool/mod.rs`.

### 2. `src/tool/workflow/backlog.rs` is UNTRACKED — the commit will not include it
`git diff HEAD --stat` lists exactly 5 files (`.coding/backlog.json`, `.coding/plans/stack.json`, `src-tauri/src/main.rs`, `src/agent/factory.rs`, `src/tool/workflow/mod.rs`). The new module file is absent from the diff because it is untracked (`git diff HEAD` does not show untracked files). Since `src/tool/workflow/mod.rs` now contains `pub mod backlog;`, committing only the tracked changes would break the build (`couldn't read src/tool/workflow/backlog.rs`). **The commit step must explicitly `git add src/tool/workflow/backlog.rs`** (or add -A) before committing.

---

## MEDIUM

### 3. Security: `AutoRun` is justified, with one residual spam consideration
`backlog.rs:87–89` — `SafetyLevel::AutoRun`, category Workflow. Justification holds per the `create_plan` precedent (AutoRun, writes only sandboxed `.coding/` state; see the ToolFilter comment at `src/tool/mod.rs:200–201`): the item is a benign, pending note, fully visible in the Backlog tab, UI-deletable (remove/clear), and item size is bounded (4000 chars). The `.coding/backlog.json` file itself stays protected (file tools refuse it); the only write path is this tool + IPC. No privilege escalation, no arbitrary file access.

Residual: the cap bounds item SIZE, not COUNT — a runaway generation could enqueue many items in one turn, and pending items are auto-dispatched to the main agent in future sessions (persistent self-prompting). Acceptable for a user-facing planning list (the user sees and can clear it), but the spam risk is real enough to be worth a sentence in the tool description or a per-turn cap if it ever becomes a problem. Not a required change — noting for the record. (Also moot until finding 1 is fixed.)

---

## LOW

### 4. Length check counts untrimmed characters while the emptiness check trims
`backlog.rs:96–104` — `trim().is_empty()` is checked first, then `args.text.chars().count() > MAX_TEXT_CHARS` counts the UNtrimmed text. A 4000-char text padded with leading/trailing whitespace is rejected as >4000 even though its trimmed content is within limit. Defensible (the stored item includes the whitespace verbatim, so bounding the stored length is arguably correct) — but the mixed trim/no-trim semantics are easy to misread. Optional cleanup: trim once, validate on the trimmed value, store the trimmed text.

### 5. Preview slicing is correct (checked, no bug)
`backlog.rs:107–113` — `chars().nth(120)` returns `Some` only when the text has ≥121 chars, in which case `take(120)` + `"…"` is used; exactly-120 chars yields no ellipsis and the full text. Slicing happens on char boundaries via `chars()`, so multi-byte UTF-8 cannot panic. No off-by-one, no double ellipsis.

### 6. Synchronous `persist()` under the tokio mutex — pre-existing pattern, no deadlock
`backlog.rs:106` locks the shared `tokio::sync::Mutex<BacklogStore>` and `add()` persists synchronously (`src/backlog.rs:258–290`: `fs::write` + `fs::rename` inside the async context). Correctness: no deadlock risk — a single lock, no re-entrancy, no cross-lock ordering, no `.await` while held beyond the lock acquisition itself. The IPC commands do exactly the same thing (`ipc/backlog_cmds.rs:92`: `state.backlog.store.lock().await.add(...)`), so this introduces no NEW blocking pattern; the file is small (~50 KB). Informational only (blocking fs I/O on an async worker is not ideal, but that ship has sailed for the whole store design and is out of scope here).

### 7. Console mode never wires the backlog store
`src-tauri/src/console.rs:771–781` wires `set_spawner` + `set_descendant_tracker` but NOT `set_backlog`, so `-console` agents get no `backlog_add` tool while GUI agents do. Defensible (headless mode has no Backlog tab), but the factory doc says the tool is "omitted when no store is wired (tests / NeedsProject)" — console is a third case worth documenting, or wiring there too if console agents should be able to queue items. Either fix or document; low priority.

### 8. `.coding/backlog.json` — valid, item 68 correct, cosmetic noise only
- File parses as valid JSON; item 68 has exactly the right shape: `id 68`, `text` matches the plan intent (memory-store supersession support — `memory_write` supersedes/superseded_by, status-changing writes, recall deprioritization of superseded rows), `images []`, `status "pending"`, `created_at 1787211567`, `note null`. `next_id` is `69`. ✓
- Existing items 52–67 preserved verbatim (ids, statuses, notes, the item-67 base64 image). ✓
- Cosmetic, as expected: PowerShell's `ConvertTo-Json` reformatted the entire file (indentation/double-spacing after colons — a ~198-line diff for a 1-item add) and an em-dash→plain-hyphen substitution slipped in somewhere. `\u0027` escapes for apostrophes are valid JSON. Harmless: the app's own `persist()` (`to_string_pretty`) re-normalizes formatting on the next mutation. No action needed.

### 9. No test for the exactly-4000 boundary being ACCEPTED
`oversize_text_errors` covers 4001-char rejection; nothing pins that exactly-4000 passes (`>` comparison makes it pass). Optional: add a 4000-char accept assertion to the same test.

---

## CONSTITUTION COMPLIANCE

- **Doc comments:** all new public items documented (module docs, `BacklogAddTool`, both consts, `BacklogAddArgs`). ✓
- **No `#[allow(...)]`:** none introduced. ✓ (`#![deny(warnings)]` at both crate roots; pre-review `cargo test --workspace` was green: 1151 lib + 129 app tests, including the 4 new tool tests.)
- **Regression tests for validation rules:** present — empty/whitespace rejection, 4001-char rejection, and the reopen-persists assertion (`add_success_allocates_id_and_persists` reopens the file and checks the item verbatim; the test is sound because `add()` persists synchronously). ✓
- **Gap:** the visibility regression (finding 1) has no test — the registry-level tests assert registration, not filter visibility. The fix must add one.

## AREAS CHECKED — NO FINDINGS

- **Shared-Arc design (correctness):** one `Arc<tokio::sync::Mutex<BacklogStore>>` shared by the tool, the IPC `backlog_*` commands, and run_all; single mutex, no nesting, no deadlock. `set_backlog` matches the `set_spawner`/`set_descendant_tracker` interior-mutability precedent; `RwLock<Option<Arc>>` read-then-clone in `register_workflow_tools` (factory.rs:642–651) is safe (no await under the guard).
- **Wiring order:** `set_backlog` runs before the main agent is built (main.rs:320), and the `backlog` Arc is created before the `brain_result` match so all three arms share it. ✓
- **No tool-name collision:** the IPC `backlog_add` is a Tauri command (`invoke_handler`, main.rs:637); the agent registry is a separate namespace — no double-registration.
- **deny(warnings):** no dead code, no unused imports in the new module.
