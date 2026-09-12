## Verdict: FINDINGS (0 high, 2 medium, 3 low)

Review of the auto-continuation + context-safety feature on `wt/agenticcoding` (2 commits: `74402dc` pre-flight guard, `2891c05` auto-resume + consolidation) against `main` (`f680b88`). 7 files, +217/-4.

The core logic is sound: the Interrupt-vs-None split is correct, `MAX_AUTO_CONTINUE=12` bounds the loop, `is_workflow_executing()` cannot deadlock (called after `run_turn` returns its scoped workflow-lock guards), `hard_ceiling()` uses a correct saturating sub, and `spawn_consolidation()` safely no-ops on `None` session_id. Two medium findings (dead config knobs; zero tests on the auto-resume path) and three low findings follow.

---

### M1 (MEDIUM) — `preflight_compact` / `compact_headroom_tokens` config knobs are dead config (parsed, never applied)

The two new `[context]` knobs are added to `ContextConfig` with doc comments advertising configurability — `src/config/general.rs:274` (`preflight_compact`, doc at :267-273 says *"Set `false` to disable"*) and `:280` (`compact_headroom_tokens`). But no code path ever reads them from config:

- **Startup:** `src-tauri/src/main.rs:1420-1423` builds the factory's context manager with only `summarize_at_fill_rate`:
  ```rust
  let context_manager = ContextManager::new(
      provider.capabilities().max_context,
      config.general.context.summarize_at_fill_rate,   // ← preflight fields NOT read
  );
  ```
  `ContextManager::new` defaults to `preflight_compact: true, compact_headroom_tokens: 32_000` (`src/agent/context.rs:86-87`). The factory's `new()` then snapshots those *defaults* (`preflight_compact: cm.preflight_compact()`), so even the factory's own `with_preflight(...)` calls at `src/agent/factory.rs:459-460` and `:523` re-apply the defaults, not the user's config.

- **Per-turn resolution (the live path):** `resolve_turn_provider` (`src/agent/loop_impl.rs:826`) → `ConfigModelResolver::build_turn_provider` (`src/model_resolver.rs:262` and `:277`) builds `ContextManager::new(max_context, fill_rate)` — again defaults, ignoring config. The resolver holds the live `Arc<RwLock<Config>>` so it *could* read the knobs, but `build_turn_provider` only takes `fill_rate`.

**Impact:** the guard itself works (the safe default `true`/32K fires correctly), but a user who sets `preflight_compact = false` or raises `compact_headroom_tokens` (e.g. to 64K for a model with a huge system-prompt+tools prefix, precisely to be *more* conservative against the `ContextWindowExceededError` this feature targets) gets **zero effect** — silently ignored. The doc comment *"Set `false` to disable"* is misleading.

**Fix:** (a) `main.rs:1420` read `config.general.context.preflight_compact` + `compact_headroom_tokens` and chain `.with_preflight(...)`. (b) `ConfigModelResolver::build_turn_provider` (`model_resolver.rs:262,277`) read the same two fields from its config Arc and apply `.with_preflight(...)` so the per-turn override path honors them too. (c) Add an integration test that a non-default headroom set in `ContextConfig` reaches the live `ContextManager`.

---

### M2 (MEDIUM) — Auto-resume behavior has zero tests

The pre-flight commit added 5 unit tests on `ContextManager` (`context.rs`), but the auto-resume change — the riskier of the two — has **no tests at all**. A search for `auto_continue | is_workflow_executing | MAX_AUTO_CONTINUE | "continue from where you left off"` returns only the implementation (`src/runtime/agent.rs`, `src/agent/loop_impl.rs`) and the plan file; no test references.

The untested safety-critical logic in `run_turn_with_retry` (`src/runtime/agent.rs:200-230`):
- the `streak < MAX_AUTO_CONTINUE` cap (the deadlock-prevention bound),
- the `Interrupt` (parks) vs `None` (auto-resumes) split,
- the `is_workflow_executing()` gate (must NOT fire in Reviewing/Complete),
- the `streak % CONSOLIDATE_EVERY_N_TURNS == 0` consolidation trigger,
- the `Prompt`-arm streak reset (`:502`).

**Fix:** add tests asserting: (1) `None` + Executing + streak<12 → pushes the continue note and loops; (2) `None` + streak==12 → parks (`AfterTurn::Continue`); (3) `Interrupt` → parks, no auto-resume; (4) `None` + Reviewing/Complete → parks; (5) a `Prompt` resets the streak so a fresh 12-budget is available.

---

### L1 (LOW) — Streak resets only on `Prompt`, not on between-turn `Suggestion`

`src/runtime/agent.rs:502` resets `auto_continue_streak = 0` exclusively in the `Prompt` arm. The `Suggestion` (between-turn steer) arm does not reset it. If the agent parked after exhausting 12 auto-continues (streak==12) and the user then sends a `Suggestion`, the steer drives one turn via `drive_turns`→`run_turn_with_retry`; if that turn ends `None`+Executing, the check `12 < 12` is false and the agent parks immediately — the steer gets a single turn, not a refreshed budget.

This is *defensible* (a steer is guidance, not a fresh task; not refreshing the deadlock budget prevents steer-loops), and mid-turn steers correctly bypass the streak entirely via the `Steer` arm (`:157`). But the task flagged Suggestion/Steer as real-user-input paths, so the asymmetry should at least be documented at the `auto_continue_streak` field (`:47`) and the reset site (`:502`).

---

### L2 (LOW) — Doc comments overstate the guard as a separate "pre-flight before each provider call"

`src/agent/context.rs:62-65` and `src/config/general.rs:267-271` describe *"a forced compaction [that] runs before the request is sent"* / *"pre-flight check before each provider call."* The actual implementation (`src/agent/turn.rs:424-430`) does **not** add a separate pre-flight check before every provider call — it modulates the `keep_recent` of the *existing* compaction, which only fires when `token_count >= summarize_at` (the soft 30% threshold). So between `summarize_at` and `hard_ceiling` the normal `keep_recent=6` runs; the aggressive `keep_recent=3` kicks in only once `token_count > hard_ceiling()`. The behavior is correct and well-placed (compaction runs before the POST), but the docs imply a distinct per-call pre-flight gate that doesn't exist. Tighten the wording to match the implemented "aggressive-compaction-when-over-hard-ceiling" semantics.

---

### L3 (LOW) — Documentation sync: new `[context]` knobs absent from README / config reference

`README.md` has no `[context]` section (search for `summarize_at|preflight|headroom` yields nothing). The existing `summarize_at_fill_rate` is referenced in `.coding/grok.md:216,335` but the two new knobs are not documented anywhere user-facing. Moot until M1 wires them, but once live they should be documented wherever `summarize_at_fill_rate` is.

---

## Verified-correct (explicitly checked per the mandate)

- **Interrupt safety:** `Some(StopReason::Interrupt) => return AfterTurn::Continue` (`agent.rs:192`) parks; auto-resume is only the `None` arm (`:200`). An Interrupt can never trigger auto-resume. ✓
- **Infinite loop:** bounded by `MAX_AUTO_CONTINUE = 12` (`:201`); at streak==12 the condition `12 < 12` is false → parks. No unbounded loop. ✓
- **`is_workflow_executing()` deadlock:** the workflow mutex is `Arc<tokio::sync::Mutex<Workflow>>` (`loop_impl.rs:105`). The only call site (`agent.rs:202`) is reached *after* `run_turn_attempt` returns; `run_turn`'s workflow-lock acquisitions (`turn.rs:339,681,1768`) are all scoped and dropped before `run_turn` returns. Same task, sequential, no lock held → **no deadlock, no re-entrancy.** ✓
- **`hard_ceiling()` math:** `max_tokens.saturating_sub(compact_headroom_tokens)` (`context.rs:122-124`) — correct, no underflow for `max_tokens < headroom` (yields 0). ✓
- **Guard placement:** inside the compaction block, before the provider POST (`turn.rs:424-444`) — fires before the request, not after. ✓
- **`spawn_consolidation` on `None` session_id:** guards `if let Some(sid) = &self.session_id` (`agent.rs:769`); no-op when None. Auto-continue only fires after a Prompt/Suggestion set session_id, so it's always Some in practice — the guard is correctly defensive. Fire-and-forget `tokio::spawn` uses Arc clones (`:707-764`), no dangling refs. ✓
- **Reviewing/Complete states:** `is_workflow_executing` returns `state() == Executing` only (`loop_impl.rs:1313`); Reviewing and Complete → false → parks. Auto-resume correctly does NOT fire when the plan is done or in review. ✓
- **`max_tokens = 0` degenerate:** `summarize_at = 0` and `hard_ceiling = 0` → compaction runs every iteration with `keep_recent=3`. Degenerate but unreachable (max_tokens comes from provider capabilities, never 0 in practice); pre-existing constant-compaction behavior, not a regression. ✓
- **Constitution:** all new public items have doc comments (`is_workflow_executing`, `with_preflight`, `preflight_compact()`, `compact_headroom_tokens()`, `hard_ceiling()`, the `ContextConfig` fields, `MAX_AUTO_CONTINUE`/`CONSOLIDATE_EVERY_N_TURNS` consts, `auto_continue_streak` field). No `#[allow(...)]` added. No platform-specific code (pure Rust logic, multi-platform neutral). Commit messages report `cargo test` green (1741/1751 passed) which under `#![deny(warnings)]` implies zero warnings — could not independently re-run (read-only reviewer). ✓
