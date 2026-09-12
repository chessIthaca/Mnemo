# Review: Memory Primer + Complete-Hook Episodic Capture

Reviewed all uncommitted changes (`git diff HEAD`) across `src/memory/mod.rs`,
`src/agent/loop_impl.rs`, `src/agent/prompt.rs`, `src/agent/turn.rs` (plus
bookkeeping-only `.coding/plans/*` changes, which are not source code).

The plan's three goals: (1) session-start primer injected into the stable head
once per session; (2) quick episodic memory at the Complete transition; (3)
working-tier exclusion (verified intact — `MemoryFilter::exclude_working`
defaults true, `recall` honors it, `recall_excludes_working_by_default` passes;
no code change).

---

## Findings

### BUG (medium) — Primer is re-fetched every turn on the "no memories / error" path; the caching is broken and the comment is wrong

**File:** `src/agent/turn.rs:310-334`, `src/agent/loop_impl.rs:553-558`

The primer cache is a `Mutex<Option<String>>` where `None` is the *initial*
("never fetched") state. The fetch logic is:

```rust
if let Some(primer) = self.session_primer() {   // Some → use cached
    head_content.push_str("\n"); head_content.push_str(&primer); head_content.push_str("\n");
} else if let Some(store) = &self.memory {       // None → fetch
    match store.strongest(...).await {
        Ok(memories) if !memories.is_empty() => { ...; self.set_session_primer(Some(primer)); }
        _ => { self.set_session_primer(None); }   // ← caches None
    }
}
```

`set_session_primer(None)` stores `None` (the `.filter(|p| !p.trim().is_empty())`
on `None` yields `None`). But `None` is **indistinguishable from the initial
"never fetched" state**, so on the *next* turn `session_primer()` again returns
`None` and the `else if` fetch branch fires again. The fetch is retried **every
turn** whenever there are no distilled memories (or the fetch errored).

The inline comment at `turn.rs:329-330` is factually wrong:
> `// No distilled memories yet (or the fetch failed) — cache None so we don't
> retry the fetch every turn.`

It does retry every turn. The "fetch ONCE per session" design goal (plan item
#1) is only met on the *with-memories* path; the empty/error path re-queries
the DB on every turn. This does not break byte-stability (an empty/no-primer
head is still byte-identical across turns, so the prefix cache is unaffected),
but it defeats the stated optimization and the comment misleads future readers.

**Fix:** use a three-state sentinel so "fetched-and-empty" is distinct from
"never fetched". E.g. change the field to `Mutex<Option<Option<String>>>`
(`None` = not fetched; `Some(None)` = fetched, no primer; `Some(Some(s))` =
cached primer), or cache `Some(String::new())` for the empty case and treat an
empty cached string as "append nothing" (note `set_session_primer` currently
filters empty → `None`, which would need adjusting). The `with-memories` path
is already correct and needs no change.

---

### BUG (low-medium) — Duplicate episodic memory on `skill_end` → Complete (and any re-entry to Complete with the same plan)

**File:** `src/agent/turn.rs:984-1017`

The Complete hook fires whenever `wf_state == WorkflowState::Complete &&
result.success` for any tool in the workflow-event list. `finish` is the
intended trigger (Reviewing → Complete), but `skill_end` (and `abandon_skill`
when `pre_skill_state == Complete`) also land in `Complete` with the same
finished plan still on the stack (`wf.plan()` returns `stack.last()`, and a
completed root plan is retained, not popped — `workflow/mod.rs:500-511,
540-543`).

Concrete double-write sequence (the `merge_to_main` skill is the canonical
case — it starts from Complete and ends back at Complete, per
`workflow/mod.rs:1508,1520`):

1. `finish` → Complete. Episodic memory written: "Completed plan: X".
2. `skill_start` (merge_to_main) → Skill.
3. `skill_end` (target_state = Complete) → Complete again. `wf_state ==
   Complete && result.success` → episodic memory written **again** for the
   same plan ("Completed plan: X").

Each `Memory::new` mints a fresh UUID and `write` does `INSERT OR REPLACE`
keyed by id, so duplicates are new rows — they accumulate (N copies after N
skill cycles). This is a data-quality issue, not a crash, but the plan
explicitly asked "Could it fire more than once for the same transition?" —
yes, via skill re-entry to Complete.

**Fix:** gate on the actual *transition* rather than the resulting state —
e.g. record the plan id + a "captured" flag in `SessionState` and skip if
already captured for this plan, or read the workflow state *before* executing
the tool and only fire when the state changed to Complete this call.

---

### CORRECTNESS (low) — Trait default `strongest` does not exclude Working, contradicting its own doc contract

**File:** `src/memory/mod.rs:109-127` (default impl) vs `:802-812` (override)

The trait doc (`mod.rs:106-108`) states: *"Working-tier memories are never
included (they are consolidation input, not primer material); callers should
pass only distilled tiers."* The `MemoryStore` override enforces this
(`mod.rs:808-812` filters out `Working`). But the **default impl** does not —
it iterates `tiers` verbatim and calls `list_by_tier(*tier)` for each, so a
mock/trait-object caller passing `Working` would get working-tier memories
back, violating the documented contract.

The only production caller (`turn.rs:316`) passes `[Semantic, Procedural]`,
so this is not reachable in production today. It is a latent contract gap:
the default impl should filter `Working` to match the override and the doc.
(The test `strongest_respects_tier_filter_and_excludes_working` exercises the
override only, not the default.)

---

## Verified correct (no findings)

- **Primer byte-stability (with-memories path):** fetched once, cached as
  `Some(primer)`, and the same string is appended (`\n` + primer + `\n`) on
  every subsequent turn → byte-identical head across turns. The primer does not
  touch the volatile tail or `CONTEXT_FOOTER`, so the cache architecture is
  intact. (`turn.rs:310-313`.)
- **Complete hook fires on both paths to Complete:** `finish` (added to the
  workflow-event list at `turn.rs:937`) for impl plans, and `complete_step`
  for a research root plan (`workflow/mod.rs:540-543` sets Complete directly).
  Both reach `wf_state == Complete && result.success`.
- **Plan title/goal available after transition:** the completed root plan is
  *retained* on the stack (not popped) in both `finish` and research
  `complete_step`, so `wf.plan()` returns it inside the lock block
  (`turn.rs:946-948`). Empty title/goal are handled (`turn.rs:998-1007`).
- **Fire-and-forget spawn:** `Arc::clone(store)` + `tokio::spawn(async move {
  let _ = store.write(memory).await; })` — correct Arc clone, `memory` moved,
  no borrows held across the spawn, lock released before spawn (`turn.rs:958`
  closes the lock scope). Consistent with the existing `record_request_stats`
  spawn (`turn.rs:530-533`).
- **`strongest` SQL binding:** N `?` placeholders match N boxed `ToSql` params
  via `params_from_iter` (`mod.rs:818-828`). `parse_memory_row` column order
  (id, tier, title, content, data, strength, access_count, created_at,
  last_accessed_at, source_session_ids, embedding) matches the SELECT
  (`mod.rs:820-822`, `parse_memory_row` at `:379-399`). `drop(rows); drop(stmt);
  drop(conn);` ordering is sound (reverse borrow order).
- **Edge cases in `strongest`:** empty tier slice → early empty (`:803`); limit
  0 → early empty (`:803`); all-working input → `wanted` empty → early empty
  (`:813`). All covered by `strongest_respects_tier_filter_and_excludes_working`.
- **Security:** the episodic write is a direct `store.write(...)` call in the
  agent loop (not a tool call), touching only the sandboxed `.coding/memory.db`
  — identical in trust level to the existing `record_tool_event` and
  `record_request_stats` fire-and-forget writes. No approval gate is bypassed
  (there is no tool to gate). Consistent with `memory_write` being AutoRun.
- **Constitution:** no Linux paths or bash syntax introduced; all new public
  functions (`strongest` trait + override, `session_primer`, `set_session_primer`,
  `format_primer`) have doc comments; no `#[allow(...)]` added; line-ending
  style preserved by the file tools. New imports (`Memory`, `MemoryTier`,
  `WorkflowState` in `turn.rs`; `Memory` in `prompt.rs`) are all used.
- **Working-tier exclusion (plan item #3):** intact — `MemoryFilter::exclude_working`
  defaults true, `recall` honors it (`mod.rs:645`), `recall_excludes_working_by_default`
  passes. No change.
