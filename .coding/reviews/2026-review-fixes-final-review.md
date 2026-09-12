# Final Review — 2026 Review-Fixes Round (all uncommitted changes)

**Reviewer:** read-only closing-sequence reviewer (spawn_agent)
**Date:** 2026-04-04
**Scope:** ALL uncommitted changes in the working tree (`git status` / `git diff HEAD`), per the
agent.md closing sequence. Findings fixed are in `.coding/reviews/2026-consolidated-review.md`.
**Branch:** `feat/reviewer-report-instructions` (feature branch — constitution-compliant, not main).

## Verification performed

- `cargo test --lib` → **396 passed, 0 failed** (exit=0)
- `cargo test --test ipc_bridge --test workflow_integration` → **9 + 5 passed** (exit=0)
- `cargo test -p myharness-app` → **18 passed** incl. the new `run_all` git tests (exit=0)
- `npx tsc --noEmit` → **clean** (exit=0)
- Read every diff hunk for all 15 change areas; traced data flow for the concurrency changes
  (H4, M6, H3), the serde casing (H2), and the store refactor (M5).

## Summary verdict

The change set is **largely correct and high quality**. The Rust changes (H2, H4, M9, M2, M1, H3,
H7, M6, P3) are faithful, well-documented, and behavior-preserving; the new tests genuinely exercise
the risky paths. **Two real findings must be fixed before commit** (one High functional bug in the
M4 slash menu, one Medium steer-removal mis-target introduced by the M5 refactor), plus a cluster of
**PLAN.md staleness** where the doc-reconciliation was written *before* H1/H3/H4/M9 landed and now
contradicts the code it shipped alongside.

---

## 1. H2 — WorkflowStateInfo type drift — **CLEAN**

- `WorkflowStateInfo.state: String → WorkflowState` (`src-tauri/src/ipc/commands.rs:94-99`), serde
  `#[serde(rename_all = "lowercase")]` on the enum (`src/workflow/mod.rs:21`) emits
  `"planning"|"executing"|"complete"`. TS mirror `state: WorkflowState` = the identical union
  (`frontend/src/lib/types.ts:41,140-146`). Casing matches at runtime. `get_workflow_state` passes
  `workflow.state()` not `.to_string()` (`commands.rs:494`). `as never` casts removed from
  `App.tsx:98` and `StatusBar.tsx:87`. `depth`+`parents` added to both sides.
- **No findings.**

## 2. H4 — approval isolation — **CLEAN**

- Map keyed `(AgentId, String)` (`approval.rs:19`); `insert` takes `agent_id` (`:42`);
  `cleanup_for_agent` uses `retain(|(aid,_),_| *aid != agent_id)` (`:88`) — only the exited agent's
  entries drop. The new test `cleanup_for_agent_drops_only_that_agents_pending` proves agent B's
  sender survives agent A's cleanup.
- `resolve()` scans by `tool_call_id` alone (`:60-72`). This is correct given the stated invariant
  (tool_call_id globally unique per call); the doc comment explains why cleanup—not resolution—is
  the agent-scoped operation. Frontend `approve(toolCallId, approval)` is unaffected.
- Forwarder passes `agent_id` to insert (`events.rs:191`).
- **No findings.** (Security: one agent cannot resolve or drop another's approval.)

## 3. M9/L1/L2 — dead code — **CLEAN**

- `convert_message`/`convert_tool_choice` + their `#[cfg(test)]` tests deleted; `async-openai`
  import block and the masked `#[allow(unused_imports] use futures::StreamExt` removed;
  `_dbg_test.rs` deleted; `async-openai` removed from `Cargo.toml`/`Cargo.lock`.
- `build_request_json` (`openai.rs:337`) and ALL its dedicated tests are intact: multimodal image
  blocks (`:1338`), strip-when-not-multimodal (`:1379`), plain-text (`:1420`), strict (`:1135,1178`),
  reasoning_effort (`:1209,1237`). `Role`/`ToolChoice` imports still used by `build_request_json`
  (`:347-350,448-451`) — no dangling-import breakage. `reqwest` gained the `stream` feature
  (justified: replaces async-openai's streaming).
- **No findings.**

## 4. M2 — provider key-resolution dedup — **CLEAN**

- New `client_factory.rs`: `resolve_api_key` = `key_for → OPENAI_API_KEY → ANTHROPIC_AUTH_TOKEN →
  "dummy"` (`:20-27`) — byte-identical to the three original copies. `provider_kind` mapping
  identical (`:30-35`). `openai_client_config` forces `multimodal`/`reasoning_effort` from the
  caller (`:42-59`).
- Call sites: main provider passes `ep.multimodal, ep.effective_reasoning_effort()` (main.rs);
  vision passes `true, None` (forces multimodal + no reasoning effort — preserved); `set_model`
  passes `ep.multimodal, reasoning_effort` (`commands.rs:457`). All three match the originals.
- Fallback semantics unchanged (security: no credential-resolution drift).
- **No findings.**

## 5. M3 — light-theme code highlight — **CLEAN**

- `globals.css:137-153` adds `html.light .hljs-*` rules setting `color` directly (github-light
  palette). The specificity reasoning in the comment is sound: `applyCodeColors` writes the dark
  `--code-*` as inline styles on `<html>`, which the base `.hljs-* { color: var(--code-*) }` rules
  read; the new `html.light .hljs-*` selectors (specificity 0-2-1) set `color` directly, overriding
  the var-based base rules (0-1-0). Dark theme untouched.
- Minor: the comment says "1-1-0+" specificity; the actual value is 0-2-1. The conclusion is
  correct; only the inline number is imprecise. **Low (doc nit), not worth a fix on its own.**

## 6. M4 — slash discoverability — **HIGH BUG**

- **HIGH — `completeSlashCommand` stale-closure breaks argument-less commands.**
  `frontend/src/components/layout/InputBar.tsx:126-135`: for `clear`/`panel`/`help` it does
  `setText(`/${cmd.name}`)` then `setTimeout(() => void handleSend(), 0)`. `handleSend` reads
  `const input = text` (`:155`) from the **current render's closure**. `setText` schedules a
  re-render but does NOT mutate the captured `text`; the deferred `handleSend` is the stale closure
  and sees the *old* partial input (e.g. `/c` or `/`), not `/clear`. `parseSlash("/c")` returns
  `{type:"unknown"}` (slash.ts:37) and `parseSlash("/")` returns null → the command does **not**
  execute; the partial text is sent as a chat prompt or hits the unknown-command path instead.
  **Impact:** selecting "clear"/"panel"/"help" from the autocomplete menu silently does the wrong
  thing. The argument-taking commands (`/model`, `/provider`, `/load`, `/save`) only *complete* the
  text (no deferred send) so they are unaffected.
  **Fix:** don't route through `handleSend` for the no-arg commands. Either call the command
  directly — `handleSlashCommand(parseSlash(`/${cmd.name}`))` — or pass the explicit value, e.g.
  set the text then call a `handleSendWith(text)` that takes the input as a parameter. (A `text` ref
  would also work but is the less clean fix.)
- The rest of M4 is sound: `filterSlashCommands` prefix-matches correctly; `defaultSavePath()`
  mirrors the backend default; Enter/Tab/Escape/arrow nav is guarded by `menuOpen &&
  menuItems.length>0`; Enter-to-send for a typed command-with-args still works (`:344-352`); the
  menu re-arms on text change. The `/model`+`/provider` "restart" hint now matches reality (see §14
  note on the runtime-swap nuance).

## 7. M5 — store refactor — **MEDIUM BUG**

- The 13 reducers faithfully reproduce the original switch for every case I traced (started,
  text_delta, reasoning_delta, tool_call_start merge, tool_call_arg_delta, tool_result + lastDiff,
  usage, context_usage, approval_request, workflow_state_changed, step_completed, finished, error,
  child_finished). The exited fallback-to-main-agent (`selectMainAgentId`) and cross-map removal are
  preserved in the dispatcher's `removeAgent` branch. The text_delta→streamingText and
  flushStreamingText-before-tool/approval/finish contracts are intact.
- **MEDIUM — steer-removal timer mis-targets under rapid successive steers.**
  Original `suggestion_injected` captured `landedId = steers[pendingIdx].id` at reduce-time and
  scheduled `setTimeout` for exactly that steer. New code splits it: the reducer returns
  `scheduleSteerRemoval: { agentId, steerId }` (correct id), **but** `handleAgentEvent`
  (`useAgentStore.ts`) ignores `effects.scheduleSteerRemoval.steerId` and instead re-queries
  `get().agents[agent_id].steers.find(e => e.status === "landed")` — the **first** landed steer.
  If steer X landed at t=0 (timer set for t=5s) and steer Y lands at t=2s, both are "landed"; the
  new timer targets X again (the first found), so **Y never gets a removal timer** and lingers
  as a stale "landed" chip until a later steer event happens to clean it up. The reducer already
  computes the correct id — the dispatcher should use `effects.scheduleSteerRemoval.steerId`
  directly rather than re-finding. Cosmetic (no conversation corruption; bounded by subsequent
  events) but a real behavior regression vs. the original.
- **Test-coverage note (Low):** `useAgentStore.test.ts` is type-checked but **not executed** — no
  vitest/jest runner is configured (the file's own header admits this). The M5 finding's intent
  ("independently testable reducers") is only half-met: the reducers are pure and the tests are
  written, but they don't run in CI. Consider wiring vitest, or at minimum acknowledge the gap.

## 8. M6 — fan-in lock contention — **CLEAN**

- Started: one lock (`events.rs:144`). Finished: one lock covers `set_running(false)` + both
  structural reads (`:152-155`). Exited: one lock for `remove` (`:175-176`); the redundant
  pre-`remove` `set_running(false)` is correctly dropped (unobservable — `remove` destroys the
  handle). Delta events take zero locks. The approval branch takes a separate scoped lock for
  `is_main` (`:195-198`) — acceptable (approvals are rare, not per-token).
- Exited removal + Run-All `on_main_turn_resolved` (Finished arm, `:164-166`) + the
  `cleanup_for_agent` defensive call (`:159`) are preserved. `has_running_descendants` is read
  *after* `set_running(false)` under the same lock, so the Run-All advance logic sees the settled
  state. The NOTE comment about `running` being a bare `AtomicBool` (not `Arc`) is an accurate
  scoping explanation.
- **No findings.**

## 9. H6 — empty state + auto-show plan — **CLEAN**

- `Conversation.tsx`: `isEmpty` gate (transcript empty + not streaming + no pending approval) shows
  the welcome panel with example prompts; `sendExample` optimistically appends the user message and
  calls `sendPrompt`, mirroring InputBar. Existing transcript rendering unchanged.
- `useAgentEvents.ts:131-143`: `autoRevealPlan()` fires on `workflow_state_changed→executing`
  (prev ≠ executing) and on `step_completed`. `autoRevealPlan` (`useAgentStore.ts:1146-1153`) no-ops
  when the panel is already visible OR the Plan tab is disabled — so it never yanks the panel away
  from a user's manual toggle or fights their tab choice.
- **No findings.**

## 10. H7 — Run-All tests — **CLEAN**

- `run_all.rs:121-308`: real temp git repos (unique per test via pid+nanos, parallel-safe),
  `git init` + local user config + `core.autocrlf=false` (so `reset --hard` preserves exact bytes on
  Windows). Tests cover: checkpoint-on-clean (no commit), checkpoint-commits-dirty-tree,
  checkpoint→commit_success round-trip (content + message + clean tree), checkpoint→rollback
  (tracked restored, HEAD reset, untracked survives — matching `reset --hard` semantics), and
  commit_success-noop-on-clean. These genuinely exercise the checkpoint/commit/rollback contract
  against a real repo.
- **No findings.**

## 11. H1 — shadcn/Radix adoption — **CLEAN** (but see §15 PLAN.md staleness)

- `@radix-ui/react-dialog` + `@radix-ui/react-tabs` added (`package.json`). `ui/dialog.tsx` +
  `ui/tabs.tsx` are faithful shadcn-style thin wrappers (overlay + portal + focus-trap via Radix;
  styling left to callers). `ConfigDialog`/`SafetyToggleDialog` migrated to Radix Dialog with
  `onOpenChange`/`onEscapeKeyDown`/`onPointerDownOutside` preserving close/cancel semantics and
  `DialogTitle`/`DialogDescription` for ARIA. `MainPanel`/`RightPanel` migrated to Radix Tabs with
  `activationMode="automatic"` (arrow-key nav). ApprovalPrompt adds guarded A/D keyboard shortcuts
  (skips editable targets, ignores modifiers). `prefers-reduced-motion` block added to globals.css;
  `useCountUp` snaps to target under reduced motion.
- Dialogs open/close on the same triggers; tabs still switch; visual styling preserved via the same
  Tailwind classes.
- **No findings.**

## 12. M1 — AgentLoop split — **CLEAN**

- `mod.rs` is now a 41-line façade: `pub use loop_impl::{AgentLoop, TurnOutcome}` (public API
  unchanged) + `pub(crate) const MAX_RETRIES` (visibility preserved). `loop_impl.rs` holds the
  struct + ctors + accessors; `turn.rs` the `run_turn` driver; `dispatch.rs` tool dispatch +
  approval + provider retry; `tests.rs` all tests. I diffed `turn.rs`/`dispatch.rs` against the
  removed `mod.rs` hunks line-by-line: identical logic, same lock/await ordering, same
  malformed-JSON sanitize-and-retry, same MAX_RETRIES abort. `execute_tool_call`/`complete_with_retry`
  are `pub(super)` (were private) — a minor, module-internal visibility widening, acceptable.
  `tests.rs` accesses `complete_with_retry` via the same module tree; 396 lib tests pass.
- **No findings.**

## 13. H3 — memory connection pool — **CLEAN**

- `MemoryStore` splits `conn` (writer) vs `read_conn` (`memory/mod.rs`). Reads (`load_all:264`,
  `recall:432`, `session_stats:616`, `project_stats:669`, `session_list:736`) use `read_conn()`;
  writes (`access:485`, `batch_access:502`, `delete_working_for_session:529`, `record_tool_event`,
  stats recording) use `conn`. `access()` (a write) is correctly on the writer. `recall` reads on
  read_conn then `batch_access` writes on `conn` — sequential within one call, no WAL snapshot
  conflict.
- In-memory stores use a shared-cache URI (`file:memdb-<uuid>?mode=memory&cache=shared`) so the two
  connections see the SAME DB (a plain `:memory:` would split it) — correct. `apply_pragmas`
  (schema.rs:22-30) sets WAL + busy_timeout=5000 + synchronous=NORMAL inside `apply_schema` (runs on
  both connections). `MemoryStoreTrait` unchanged.
- **No findings.**

## 14. P3 low batch — **CLEAN** (one nuance noted)

- `get_config` doc comment added (commands.rs:578). `SafetyMode`/`WorkflowState` `#[default]`
  derives preserve the exact default VALUES (`ApproveEachAction`, `Planning`) — verified the
  `#[default]` attribute is on the correct variant in both. `sandbox.rs` `is_some_and` and
  `search.rs` `contains` are behavior-identical (path validation NOT weakened — the `is_some_and`
  closure is logically equal to the prior `map_or(false,…)`). `MAX_PROVIDER_TURN_ATTEMPTS` const
  added with a clarifying doc. `factory.rs` `load_latest` intent comment. PlanProgress/StatsView
  eslint-disable reasons. `.inline-code` theme-aware contrast (Message.tsx + globals.css).
- **Nuance (Low, no action):** The `/model`+`/provider` "restart to take effect" hint (M4) and the
  InputBar transcript note now say a restart is required. Since M2 wired `set_model` through the
  shared builder, the model swap DOES apply to live agents' next turn via `set_provider`. The
  "restart" wording is conservative (the slash path updates config; the live-swap is a separate
  step) and matches PLAN.md's "not atomic / restart-or-fresh-agent" framing — acceptable, but the
  hint slightly understates the runtime capability. Not a defect.

## 15. PLAN.md reconciliation — **MEDIUM (stale/contradicts shipped code)**

The doc reconciliation (item 15) was written as if H1/H3/H4/M9 were NOT yet adopted, but those
changes landed in the SAME change set. PLAN.md now contradicts the code it ships alongside:

- **MEDIUM — `PLAN.md:58` Component library** still reads "Hand-built Tailwind components; shadcn/ui
  (Radix + Tailwind) planned … today's components are hand-built Tailwind **and not yet migrated**."
  This is now false: Radix Dialog+Tabs ARE adopted (§11). Should say shadcn/Radix is the adopted
  baseline for dialogs/tabs.
- **MEDIUM — `PLAN.md:64` LLM client** still lists "**async-openai**" as the LLM client. M9 removed
  the `async-openai` dependency; the production path is a hand-rolled OpenAI-compatible client over
  `reqwest` SSE (`build_request_json`). The table row is stale.
- **MEDIUM — `PLAN.md` "Multi-agent readiness" limits list (~:197-204)** still states "pending
  approvals are keyed by `tool_call_id` only (not per-agent), and `cleanup_for_agent` clears *all*"
  and "no WAL read-concurrency pool yet." Both are now FIXED by H4 (keyed `(agent_id, tool_call_id)`,
  scoped cleanup) and H3 (WAL read/write split). The two "known limits" should be struck or rewritten
  to reflect the shipped fixes; as written they describe the pre-fix code.
- The beyond-spec features section and the agent.md-location correction are accurate.

---

## Constitution compliance

- Public fns have doc comments: `client_factory.rs` (`openai_client_config`, `build_openai_client`),
  `schema::apply_pragmas`, `loop_impl.rs` accessors, `get_config` — all documented. ✓
- Windows/PowerShell honored in this review. ✓
- Plan-first honored (changes trace to the consolidated-review plan). ✓
- Feature branch (`feat/reviewer-report-instructions`), not main. ✓

---

## Must-fix before commit (prioritized)

1. **HIGH — M4 slash menu stale-closure** (`InputBar.tsx:126-135`): argument-less commands
   (`/clear`, `/panel`, `/help`) selected from the autocomplete menu don't run — the deferred
   `handleSend` reads stale `text`. Fix by invoking the command directly (or passing the explicit
   text), not via the closure `handleSend`.
2. **MEDIUM — M5 steer-removal mis-target** (`useAgentStore.ts` `handleAgentEvent`): use
   `effects.scheduleSteerRemoval.steerId` (the id the reducer already computed) instead of
   re-finding the first "landed" steer, so a rapidly-landed second steer still gets removed.
3. **MEDIUM — PLAN.md staleness** (§15): update the Component-library row (shadcn/Radix now
   adopted), the LLM-client row (async-openai removed), and the Multi-agent-readiness limits
   (H3/H4 fixed) to match the shipped code.

## Should-consider (not blocking)

4. **Low — M5 test runner**: `useAgentStore.test.ts` is type-checked but never executed (no
   vitest/jest). Wire a runner or accept the gap explicitly.
5. **Low — M3 comment nit** (`globals.css:139`): specificity value in the comment (0-2-1, not
   "1-1-0+"); conclusion correct.

Everything else reviewed is clean.
