# Memory System Overhaul — Review

**Scope:** All uncommitted changes (`git diff HEAD`) for the memory-overhaul plan
(exclude_working default, selective `record_tool_event`, tier weighting, sim
down-weighting, auto-consolidate at session end, content cap).

**Build:** `cargo test` reported passing (790 tests, 0 warnings) per the task.
No `#[allow(...)]` suppressions added. No commits to `main`.

---

## Bugs

### B1 [Medium] — Consolidation double-fires on the Cancel path
**File:** `src/runtime/agent.rs:316` and `src/runtime/agent.rs:339`

Change (E) added a `self.spawn_consolidation()` call to the post-loop
natural-termination path (line 339) but did **not** remove the existing call in
the `Cancel` arm (line 316). Because `Cancel` does `break;` out of the loop and
then falls through to the post-loop code, **a Cancel spawns consolidation
twice** — two independent `tokio::spawn`'d background tasks for the same
session.

`spawn_consolidation` has no "already spawned" guard, and `consolidate_session`
(`src/memory/consolidation.rs:20`) is **not concurrency-safe** against itself:
both tasks call `list_by_tier(Working)` → filter by session, and if both read
the working events before either's `delete_working_for_session` runs (a real
race — the individual DB ops are serialized by the connection mutex, but the
read→synthesize→write→delete sequence is not atomic), both tasks will:
- synthesize + write a **duplicate** episodic summary,
- extract **duplicate** semantic facts + procedural workflows,
- burn 3 extra LLM calls (the second task's `synthesize_with_llm` /
  `extract_semantic_facts` / `extract_procedural_workflows`),
- call `end_session` twice (harmless — idempotent).

This directly undermines the plan's goal: duplicate semantic/procedural
memories pollute the distilled tiers and resurface in recall. The
natural-termination path (inbox closed → `recv()` returns `None`) only hits the
post-loop call, so it is correct; only Cancel double-fires.

**Fix (simplest):** delete the `self.spawn_consolidation();` line from the
`Cancel` arm (line 316). The post-loop call (line 339) already covers every
exit path (Cancel `break` and natural loop exit) and is equally fire-and-forget
(it spawns in the background before sending `Exited`, so it does not block the
exit the Cancel comment was worried about). Alternatively, guard with an
`already_consolidated: bool` flag set on the first call.

---

## Constitution compliance

### C1 [Low] — Misleading / duplicated doc comment on `tier_weight`
**File:** `src/memory/mod.rs:273-283`

The doc comment above `fn tier_weight` (lines 273-283) begins with 5 lines that
describe `contains_ascii_ci` — the *wrong* function:

```
/// Case-insensitive ASCII substring check WITHOUT allocation (avoids the
/// per-memory `to_lowercase()` allocation in the recall scoring loop —
/// P2). Returns true if `haystack` contains `needle` ignoring ASCII case.
/// Non-ASCII chars compare by exact byte (a reasonable fallback; memory
/// titles/content are predominantly ASCII).
/// The tier-weight component of the recall score. Distilled tiers ...
```

This is a copy-paste artifact from inserting `tier_weight` between the
`contains_ascii_ci` doc and its function: the `contains_ascii_ci` doc preamble
got stranded above `tier_weight`, and `contains_ascii_ci` (line 298) kept its
own correct copy. The result is a misleading doc on `tier_weight` (it claims to
be a "case-insensitive ASCII substring check") and a duplicated doc block.

**Fix:** delete lines 273-277 (the `contains_ascii_ci` preamble) so `tier_weight`'s
doc starts at "The tier-weight component of the recall score." `contains_ascii_ci`
already has its own correct doc at lines 293-297.

### C2 [Low] — Imprecise comment in `is_durable_tool`
**File:** `src/agent/turn.rs:1019`

The comment `// Git — only the mutating subcommands.` is imprecise: `checkout`
and `stash` are also mutating git subcommands but are intentionally excluded
(only `commit`/`merge`/`push` — history-landing ops — are durable). The code
matches the stated design (the task spec says "only commit/merge/push"), but the
comment reads as if all mutating subcommands are included. Suggest rewording to
`// Git — only the history-landing subcommands (commit/merge/push).` Informational
only; no behavior change.

---

## Correctness — no findings

- **`exclude_working` logic** (`src/memory/mod.rs:591`):
  `filter.exclude_working && filter.tier.is_none()` correctly makes an explicit
  `tier` (including `Working`) win over the default exclusion. Verified by
  `recall_filters_by_tier` (Working tier recall still works) and
  `recall_excludes_working_by_default`.
- **SQL shapes:** All three FTS shapes (`fts_candidates_locked:485-501`) and
  both `load_recent_locked` shapes (`:386-392`) have placeholder counts matching
  their param arrays (`?1/?2/?3` vs `[fts_query, tier, limit]`, etc.). The
  `exclude_working` branch uses a literal `'working'` string (correct — `tier` is
  stored as the enum's `as_str()`, which is `"working"`).
- **`is_durable_tool` git detection** (`src/agent/turn.rs:1020-1023`):
  `args.get("subcommand")` matches the git tool's actual argument shape
  (`{"subcommand":"commit",...}`, confirmed in `src/tool/agent/git.rs:28` and
  `src/agent/approval.rs:309`).
- **Score formula** (`src/memory/mod.rs:670`):
  `sim*0.2 + decayed_strength*0.3 + keyword_boost(0.3) + tier_weight(0–0.3)` is
  sound. Tier weight (max 0.3) can lift a distilled fact above a raw snapshot
  even on identical content — proven by `recall_ranks_semantic_above_working_when_both_match`.
- **Char-boundary cap** (`src/memory/mod.rs:791`):
  `tool_output.chars().take(MAX_WORKING_CONTENT_CHARS).collect()` iterates `char`
  (valid Unicode scalars), so it never splits a multi-byte sequence. Safe.
- **Test switch** (`src/agent/tests.rs`): `run_turn_records_session_id_on_tool_events`
  moved from `file_read` → `file_write` (file_read is no longer recorded). The
  test still validates its claim (durable tool events carry the session_id) and
  the comment explains the switch. Correct.

## Security — no findings

- The content cap truncates stored output; it does not leak or expose anything.
- `exclude_working` / `is_durable_tool` only change what is stored/recalled
  locally in the sandboxed memory DB — no new network, FS, or exfiltration surface.
- No new approval-gated operations; no shell/git mutations introduced.

## Constitution compliance — no other findings

- All **public** functions/items have doc comments (`MemoryFilter::include_working`,
  the `exclude_working` field, `is_durable_tool` is `pub(crate)` with a doc,
  `spawn_consolidation` is private but documented). `tier_weight` is private
  (see C1 for its doc issue).
- No `#[allow(...)]` added to silence warnings.
- Build is warning-free under `#![deny(warnings)]` (cargo test passed, 790 tests).
- No Windows/PowerShell or commit-to-main violations in the diff.

---

## Informational (not blocking)

### I1 [Low] — `tool_input` is not capped
**File:** `src/memory/mod.rs:794`

`record_tool_event` caps `tool_output` (→ `capped_output`, used for both
`memory.content` and `data.tool_output`) but stores `tool_input.clone()` verbatim.
For a `file_write`, `tool_input` includes the full `content` argument, so writing
a large file stores its entire body in the `data` JSON blob. The **FTS index and
embedding are protected** (they derive from `memory.content` = `capped_output`),
so the stated goal is met, but the DB row can still grow large. Consider also
capping `tool_input` (or at least its string-valued fields) if DB size matters.
Not a correctness or security issue.

---

## Summary

| Severity | Area | Count |
|----------|------|-------|
| Bug | B1 (consolidation double-fire on Cancel) | 1 |
| Constitution | C1 (misleading `tier_weight` doc), C2 (imprecise git comment) | 2 |
| Correctness | — | 0 |
| Security | — | 0 |
| Informational | I1 (`tool_input` not capped) | 1 |

**Must-fix before commit:** B1 (the double-fire directly produces duplicate
distilled memories, defeating the plan's purpose) and C1 (misleading doc).
C2 and I1 are optional cleanups.
