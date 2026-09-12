# Batch C Review — Quick Hardening (C1 allowlist fail-closed + C2 NTFS ADS)

**Branch:** `feat/deep-review-c-hardening`
**Files changed:** `src-tauri/src/ipc/spawn.rs`, `src/tool/agent/sandbox.rs` (+ plan `.md` checkbox)
**Verdict:** ✅ **Approve with one MEDIUM finding (stale doc comment) + one LOW observation (duplicate test).** Both fixes are **correct and complete** for their stated threat models. The security properties hold. The one actionable finding is a documentation defect on a security-critical function that contradicts the very fix it documents.

---

## C1 — allowlist fail-closed (`src-tauri/src/ipc/spawn.rs`) ✅ CORRECT

### Does C1 actually fail-closed now? — YES (traced end-to-end)

1. `compute_subagent_allowlist(&map, Some(999), …)` with 999 absent from the map → the `match loops.get(&parent_id)` `None` arm (spawn.rs:298-303) returns `Some(Vec::new())`.
2. Caller `spawn_agent_shared` (spawn.rs:153-158): `if let Some(list) = allowlist { wf.set_tool_allowlist(Some(list)); }` → `set_tool_allowlist(Some(vec![]))`.
3. `Workflow::allowed_tools()` (workflow/mod.rs:284-286): `Some(list)` → returns `ToolFilter::Skill(list.clone())` = `ToolFilter::Skill(vec![])`.
4. `ToolFilter::Skill(allowed)` arm of `allows()` (tool/mod.rs:293-305): `Memory => true`; `_ => name == "ask_user" || name == "current_plan" || allowed.iter().any(|n| n == name)`. With `allowed` empty, only Memory tools + `ask_user` + `current_plan` are admitted. **Every Agent/Browser/Workflow mutation tool (file_write, file_edit, shell, git, spawn_agent, …) is denied.**
5. `ToolRegistry::schemas()` (tool/mod.rs:348): `included = category == Memory || filter.allows(...)`. So the LLM never even *sees* the denied tools.

The three tools that remain (Memory, ask_user, current_plan) are non-escalating: Memory is sandboxed to `.coding/memory`; ask_user/current_plan are read-only and available in *every* parent state. So the fail-closed state grants nothing the parent wouldn't already have. **No escalation path exists.** ✓

### Is `None` returned ONLY for genuinely parentless spawns? — YES

- `parent_id = None` → `let parent_id = parent_id?;` (spawn.rs:280) returns `None` immediately. ✓
- `parent_id = Some(id)`, id in map → proceeds, returns `Some(list)`. ✓
- `parent_id = Some(id)`, id NOT in map → returns `Some(Vec::new())` (spawn.rs:302). ✓

`None` is reachable **only** via `parent_id = None`. Confirmed.

### Any caller that depends on `None` for missing-parent? — NO

`compute_subagent_allowlist` has exactly **one** production caller: `spawn_agent_shared` (spawn.rs:153), which does `if let Some(list) = allowlist { … }`. `Some(vec![])` flows into `set_tool_allowlist(Some(vec![]))` (deny all); `None` skips the override (state-derived filter). No caller branches on `None`-means-missing-parent. The plan's "audit the spawn_agent-tool fallback paths (:264-274)" is satisfied: there is a **single chokepoint** — `SpawnAgentTool` → `IpcSpawner::spawn_with_parent` → `spawn_agent_shared` → `compute_subagent_allowlist`. No separate fallback path exists in the tool. ✓

### Can a subagent still get the full tool set via a missing parent? — NO

The only route to an unrestricted subagent is `parent_id = None` (UI button / main agent). A tool-spawned subagent always has `parent_id = Some(id)`; if that loop is missing, it now gets `Some(vec![])` (deny all), never `None`. ✓

### Test quality

- `missing_parent_loop_denies_all` (spawn.rs:707): calls with `Some(999)` on an empty map, asserts `is_some_and(|v| v.is_empty())`. Under the **old** code `loops.get(&999)?` propagated `None`, so this assertion would **fail** (None is not Some). The test genuinely exercises the fix. ✓
- `parentless_spawn_returns_none` (spawn.rs:724): asserts `None` for `parent_id = None`. Verifies the parentless path is unchanged — a valid guard, not a no-op. ✓ (See LOW finding below re: duplication.)

---

## C2 — NTFS ADS protection (`src/tool/agent/sandbox.rs`) ✅ CORRECT

### Does C2 catch ADS on protected files? — YES

`.coding/memory.db:evil` → `validate_for_creation` (lexical, no FS) succeeds → `is_protected_write_target`:
- `rel_str` = `.coding/memory.db:evil` (after strip_prefix + replace `\`→`/` + lowercase).
- ADS guard (sandbox.rs:188-192): `rel_str.rsplit('/').next()` = `memory.db:evil`, `.contains(':')` = true → `return true`.

Under the **old** code (no ADS guard), `rel_str == ".coding/memory.db"` is **false** (the `:evil` suffix breaks exact match), and `starts_with(".coding/plans/")` is false → returns **false** (unprotected). The fix closes this. ✓

### Does the ADS check run BEFORE the exact-match checks? — YES

The ADS `if` block (sandbox.rs:188-192) precedes the `==`/`starts_with` checks (sandbox.rs:195-200). An ADS path returns `true` immediately. This ordering is correct and necessary: without the guard, `.coding/memory.db:evil` matches none of the exact literals and would slip through. ✓

### Does C2 false-positive on normal files? — NO

- `src.rs` → final component `src.rs`, no `:` → not blocked. ✓ (test `normal_file_without_colon_not_ads_blocked`)
- `src/main.rs` → final component `main.rs`, no `:`. ✓
- `.coding/reviews/2026-04-18-review.md` → final component `2026-04-18-review.md`, no `:` → not blocked (reviews stay writable). ✓

### Does C2 false-positive on the drive prefix? — NO

`rel = path.strip_prefix(&self.root).unwrap_or(path)` is the path **relative to root** — the drive prefix (`C:`) is stripped (it's part of `root`, not `rel`). For a normal in-root file, `rel_str` has no drive prefix at all. Even in the `unwrap_or(path)` fallback (path outside root — unreachable in practice, since `validate`/`validate_for_creation` reject outside-root first), the drive prefix `c:` is the **first** component, not the final one, so `rsplit('/').next()` never sees it. The `:` check is on the **final component only**, never the full path. ✓

### Does `rsplit('/')` handle backslash paths? — YES

`rel_str` is computed as `rel.to_string_lossy().replace('\\', "/").to_ascii_lowercase()` — backslashes are converted to forward slashes **before** `rsplit('/')` runs. So `rsplit('/')` operates on an already-normalized string. ✓

### Is the `:` check on the final component only, not the full path? — YES

`rel_str.rsplit('/').next()` yields only the last `/`-delimited segment. A `:` in a parent directory component (e.g. a hypothetical `foo:bar/baz.txt`) would NOT trigger the guard — only the final component is inspected. This is the correct scope: NTFS ADS syntax is `file:stream`, where the `:` is in the final (file) component. ✓

### Can an ADS write still bypass the protected check? — NO

All ADS variants are caught: `file:stream`, `file:stream:$DATA`, `file:a:b` — any `:` in the final component returns `true`. The guard is type-agnostic (any `:`), which is **stricter** than the plan's suggested "compare the component before the first `:`" — simpler and has no false negatives. On Windows `:` is illegal in filenames (it *is* the ADS separator), so no legitimate file is blocked. ✓

**Defense-in-depth note (not a finding):** for an *existing* ADS, `validate_for_write` tries `validate` (canonicalize) first; if canonicalize resolves `memory.db:evil` to the base `memory.db`, the exact-match check catches it; if `validate` fails, the `validate_for_creation` fallback + ADS guard catches it. Both paths covered.

### All write paths route through the guard — YES

`is_protected_write_target` is called from `validate_for_write` (sandbox.rs:231, used by file_write + file_append) and `refuse_if_protected` (sandbox.rs:255, used by file_edit + file_write's `prepare_for_approval`). All three file tools (write/append/edit) + both execute and approval-preview paths are covered. No bypass. ✓

### Test quality

- `ntfs_ads_on_protected_file_refused` (sandbox.rs:526): `.coding/memory.db:evil` via `validate_for_creation` → asserts protected. Under old code returns false → **test fails**. Genuinely exercises the fix. ✓
- `ntfs_ads_on_any_file_refused` (sandbox.rs:543): `normal.txt:hidden` → protected. Under old code false → fails. Exercises the fix. ✓
- `normal_file_without_colon_not_ads_blocked` (sandbox.rs:559): `src.rs` → not blocked. Valid negative (no-false-positive) test. ✓

**Windows path-parsing note (verified, not a finding):** `Path::new(".coding/memory.db:evil")` on Windows — `memory.db:evil` is not a disk prefix (disk prefix = single letter + `:`), so Rust parses it as a single Normal component. `validate_for_creation`'s lexical `normalize_path` preserves it, `starts_with(root)` holds. The test is valid on both Windows and Unix.

---

## Findings

### MEDIUM — Stale doc comment on `compute_subagent_allowlist` contradicts the C1 fix
**File:** `src-tauri/src/ipc/spawn.rs:255-258`

The function-level doc comment still reads:
```
/// Returns `None` (no restriction) when:
/// - there's no parent (a UI-button spawn or the main agent — unrestricted), or
/// - the parent's loop can't be found (defensive: fall back to no restriction
///   rather than silently over-constraining).
```
The second bullet is now **factually wrong**. The code (spawn.rs:296-304) returns `Some(Vec::new())` (deny all) when the parent's loop is missing — the *opposite* of "no restriction." The inline comment at spawn.rs:290-295 correctly describes the new fail-closed behavior, but the function-level doc directly contradicts it.

**Why it matters:** this is a security-critical function. A future maintainer reading the doc comment (which is what rustdoc surfaces and what a quick reader sees) could conclude the old `None` behavior is intended and "fix" the `match` back to `?`, silently reintroducing the fail-open vulnerability. The doc should state that a missing parent loop returns `Some(vec![])` (deny all), and that `None` is returned **only** for `parent_id = None`.

**Fix:** update the `Returns None` doc block to:
```
/// Returns `None` (no restriction) ONLY when there is no parent (a UI-button
/// spawn or the main agent — unrestricted). A KNOWN parent_id whose loop is
/// missing from the map returns `Some(vec![])` (deny all tools) — fail-closed.
```

### LOW — Duplicate test: `parentless_spawn_returns_none` ≡ `no_parent_returns_none`
**File:** `src-tauri/src/ipc/spawn.rs:692` and `:724`

Both tests call `compute_subagent_allowlist(&map, None, &Some("reviewer".into()))` and assert `list.is_none()` — identical arguments, identical assertion. The new `parentless_spawn_returns_none` adds no coverage beyond the pre-existing `no_parent_returns_none`. Not a bug (no warning, no dead-code lint on tests), just redundancy. Consider deleting one. (The C1 plan asked for a parentless-returns-None test; the pre-existing `no_parent_returns_none` already covered it.)

---

## Constitution compliance

- **Doc comments on changed pub items:** `is_protected_write_target` (pub fn) has an accurate, updated doc (the ADS logic is explained in an inline comment within the method). No new pub items were added. `compute_subagent_allowlist` is **private** (`async fn`, no `pub`) — its doc comment exists but is stale (MEDIUM finding above). ✓ (modulo the finding)
- **No `#[allow]`:** the diff introduces none. The only `#[allow]` in the codebase is the pre-existing `#[allow(clippy::too_many_arguments)]` on the `AgentLoop` constructors — untouched. ✓
- **No dead code:** all new code has callers. `compute_subagent_allowlist` is called from `spawn_agent_shared`; the ADS guard is inside `is_protected_write_target` (called from `validate_for_write` + `refuse_if_protected`); all tests compile under `cfg(test)`. ✓
- **Warning-free build:** the new code introduces no unused imports/variables. `eprintln!("{parent_id}")` is sound (`AgentId: Display`, confirmed by existing `format!("agents/{agent_id}")` at spawn.rs:117). `return Some(Vec::new())` type-infers `Vec<String>`. `rel_str.rsplit('/').next()` → `Option<&str>`; `"".contains(':')` is `false` (no panic on empty). The main agent should confirm with `cargo test` in the closing sequence. ✓
- **Line endings:** the `.rs` files show no LF→CRLF warning (they match the repo setting); only the `.md` plan file has the cosmetic git warning. ✓

---

## Summary

Both fixes are correct and the security properties hold:
- **C1:** a missing parent loop now denies all tools (`Some(vec![])` → `ToolFilter::Skill(empty)` → only Memory/ask_user/current_plan, none escalating). `None` is reserved for genuinely parentless spawns. Single chokepoint, no caller depends on the old `None`-for-missing-parent semantic.
- **C2:** any final path component containing `:` is rejected before the exact-match checks, closing the ADS exfiltration/tampering vector on protected files. No false positives on normal files or the drive prefix. `rsplit('/')` operates on the forward-slash-normalized string. All three file tools route through the guard.

**Action required before commit:** fix the MEDIUM stale doc comment on `compute_subagent_allowlist` (spawn.rs:255-258) so it reflects the fail-closed behavior. The LOW duplicate-test observation is optional cleanup.
