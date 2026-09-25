# Mnemo — Build Plan

A coding-first agentic harness in Rust: a full GUI agentic loop optimized for
OpenAI-compatible models, with local models (Ollama) as a capability-degraded
fallback. **One app instance works on exactly one project** — a directory whose
`.coding/` holds its config, memory, plans, and `agent.md` constitution. Two
`agent.md` files (global + project) form non-negotiable context loaded before
every LLM turn. The agent operates under an **enforced plan-first workflow** —
it must author a plan in `.coding/plans/` and check off steps before writing
code, so progress survives restarts. The agent runs as an independent tokio
task, decoupled from the UI via typed channels, with a **multi-agent-ready**
manager — agents spawn independently and each has its own plan/workflow.
Memory reads run on a separate WAL-mode connection from the single serialized
writer (so concurrent recalls don't queue behind a write), and pending
approvals are keyed per-agent (`(agent_id, tool_call_id)`) with per-agent
cleanup; provider / safety-mode / config
are intentionally global. Structured file tools are primary; shell / grep-glob / git support the
coding workflow. Every mutating action goes through an approve-each-action gate,
with **diff-based approval** for file edits.

## Current status (2026-08-11)

The brain **and** the Tauri + React/TS GUI are built, tested, and working.
The original Ratatui TUI was replaced by a Tauri 2 + React 18 + TypeScript +
Vite frontend; the brain is fully decoupled from the UI via typed channels
(`AgentCommand` UI→Agent, `SerializableAgentEvent` Agent→UI) and an IPC
adapter (`src-tauri/src/ipc/`). The app is a multi-agent, plan-first,
approval-gated coding harness — not a UI swap.

**Test counts:** 522 lib + 49 tauri (IPC) + 5 workflow-integration + 82
vitest (frontend), `tsc --noEmit` clean.

**As-built architecture (post architecture-review remediation, Phases 0–6):**

- **Multi-agent runtime.** Each spawned agent owns its own `AgentLoop`
  (built by `AgentLoopFactory`) and thus its own `Workflow` + `ToolRegistry`
  — no shared singleton workflow. Agents spawn independently via the
  `spawn_agent` tool / IPC command and are tracked in the `AgentManager`
  (fan-in, parent-aware completion notifications). The main agent is the
  parentless smallest-id agent; subagents register as children.
- **Plan-first workflow + skills.** The enforced workflow (`Workflow`:
  Planning → Executing → Complete, with a `Skill` overlay) persists plans
  to `.coding/plans/<id>.md` + a `stack.json` sidecar supporting a
  sub-plan stack. Skills (`skill_start`/`skill_end`/`abandon_skill`) overlay
  a tool allow-list + target state and persist across restart. The library is
  a live `SkillLibrary` (the `.coding/skills/` dir + the registry loaded from
  it): `skill_reload` re-reads it into the running app (every workflow state,
  an active skill included) and `skill_create` authors a validated skill file
  (Executing only), hot-adding it so it is startable immediately.
- **Backlog + Run-All.** A persistent prompt backlog (`.coding/backlog.jsonl` —
  one JSON item per line, UUID string ids, git union merge driver for
  concurrent multi-instance adds) feeds the main agent one item at a time.
  New items carry the headline+body shape the Backlog tab renders (backlog
  45a4eb88): first line a short headline (≤100 chars), then a blank line,
  then the body — enforced at both write paths by the shared
  `normalize_item_text` validator in `src/backlog.rs` (the agent-facing
  `backlog_add` tool directly; the UI's IPC command via the image-aware
  `normalize_new_item_text`, which exempts image-only items — a pasted
  screenshot with no caption has no headline to validate), with the Backlog
  tab surfacing the rejection inline and restoring the draft; existing
  items are grandfathered (no migration, the store's `add` stays
  unvalidated, and `backlog_edit` deliberately doesn't validate so editing
  a grandfathered single-line item never forces restructuring). The body
  also passes a detail bar (2027-01-07): ≥160 chars AND at least one
  file-path-like token (or an explicit "no-code research" marker) —
  Run-All dispatches items to whatever model/effort is configured, and a
  terse pointer-only body leaves a lesser-reasoning session floundering;
  the rejection names the five elements the body must carry (problem+repro,
  paths+symbols, fix direction, acceptance, related pointers) so an LLM
  caller retries in one shot, the `backlog_add` tool schema teaches the
  bar upfront (built from the enforcing consts so it can't drift), and the
  Backlog composer's Template button pre-fills the 5-section skeleton (only
  into an empty composer). Both add paths also take an optional `position`
  ("top" | "end", default/absent = "end" — backlog 06a31736): "top" inserts
  at the front of the queue so an urgent item dispatches first (the
  store-level `BacklogStore::add_at` insert-at-0); a top insert is
  union-merge safe — it prepends one line (a minimal diff that merges
  cleanly with a concurrent append) and `parse_jsonl` collapses same-id
  lines with the first occurrence winning, so the union driver's worst
  case loses nothing. Run-All is the unattended
  ("overnight") loop: per-item git checkpoint (on the per-directory wt/*
  branch — forked/reused first, never main), commit-on-success,
  halt-for-approval (never auto-approves), and a mandatory plan-loop gate.
  The dispatch prompt auto-attaches a best-effort recalled-context block
  (2027-01-07): the top-5 passive semantic-memory hits on the item text
  (`recall_peek` — no access-count bump), each a tier-prefixed title +
  gist under a verify-against-the-code caveat, so a dispatched
  lesser-reasoning model starts informed instead of re-deriving (no
  store, a recall error, or no hits dispatches without the block; the
  single-dispatch path stays raw — the user is watching).
  Item status is tied to the plan lifecycle (backlog 45dcf577): `in_flight`
  ⇔ a plan dispatched for the item is ACTIVE (stamped when the workflow
  enters `Executing` — the `create_plan` moment, which also records the
  root plan id on the item as the item↔plan linkage; it stays `in_flight`
  through interruptions, steering pauses, and sub-plan pushes), `done` ⇔
  the plan reached `Complete` — the gate: `Complete` at turn resolution
  (its plan's `finish` ran) AND at least one workflow state transition
  observed during that turn (`Complete` is a resting state, so a turn that
  never planned — freeform answer, a question before planning — does not
  count), and `failed` ⇔ ONLY the root plan was abandoned (`abandon_plan`,
  observed as an `Executing`/`Reviewing` → `Planning` transition; an
  abandon followed by a successor plan that finishes still resolves
  `Done`). Every other turn end — loop open, resting `Complete`,
  unverifiable state, terminal `Error` (crash, provider exhaustion) — is
  NOT a failure: the item keeps its status, the work is NOT rolled back,
  and the run waits — the next turn resolution re-checks (the next item
  may only start once this item's loop verifiably closed; a resumed
  session continues the plan — the `in_flight`→`pending` deferral and
  terminal requeue remain the escape hatches). The same contract applies to single-dispatch and auto-feed
  items (no git checkpoint, so nothing to roll back; auto-feed only runs
  past a terminally resolved item). A user steer or interrupt on the main
  agent is an **intervention, not a failure**: a run-all item stays
  `in_flight` with its run stopped (the plan stays active — the turn that
  completes it resolves the item; requeueing + ending the run stranded
  interrupted items, backlog b83e891f), a single-dispatch item stays
  `in_flight` with its dispatch pointer restored (the plan stays active —
  the turn that completes it resolves the item; only a run-all item whose
  run was drained before the intervention requeues), and the run never
  dispatches the next item past an intervention — an item leaves `pending`
  (→ `in_flight`) only when the workflow actually enters `executing`, so a
  pre-planning intervention leaves the item queued untouched (approval
  halts stamp nothing — the run state is kept so the post-approval turn
  resolution resolves the item under the same rules). Users can re-queue
  any finished item (done/failed/can't-resolve) back to pending from the
  Backlog tab.
- **Approval contract + safety.** Every mutating action goes through an
  approve-each-action gate with **diff-based approval** for file edits
  (`prepare_for_approval` builds a preview before the prompt). `DenyAll`
  latches at the turn level (skips remaining tool calls). Safety modes
  (ApproveEachAction / AutoReadApproveWrites / AutoApproveProject /
  Autonomous) + regex auto-approve rules (`.coding/safety.toml`). Protected
  `.coding` write targets (memory DB, `safety.toml`, `backlog.jsonl`, the
  `knowledge/` corpus, the `plans/` tree) are refused by the file agent tools,
  as is the `.git` control plane (any path containing a `.git` component -
  planted hooks/`core.fsmonitor`); `keys.toml` is
  written with user-only permissions (Unix `0600` / Windows DACL). A hardlinked
  `.coding/**` file is refused as well (canonicalize cannot see a hardlink —
  there is no link to follow, the path IS the shared inode's file), the refusal
  is re-run on the CANONICAL form of the path so a Win32 8.3 alias
  (`.coding/KNOWLE~1/…`) cannot plant a subtree inside a protected tree, and the
  creation ladder fails closed when the write target resolves through a link —
  a dangling link leaf included; its one lexical fallback (a parent the call
  itself just created) is re-checked for link-freedom, so a link planted between
  the gate and the mkdir is caught too (2027-01-15, plan b4812291; review rounds
  2-4 §Q5 of plan ee65fd4b).
- **Structural perf.** Synchronous file-system I/O in the async agent tools'
  `execute` paths (`file_read`/`file_edit`/`file_write`/`file_append`/`search`/
  `describe_image`) runs inside `tokio::task::spawn_blocking` so concurrent
  multi-agent tool use cannot stall the async runtime. The one deliberate
  exception is the approval-preview hook (`file_edit`/`file_write`
  `approval_preview` → `prepare_for_approval`, called inline from
  `dispatch::execute_tool_call`): one `canonicalize` plus one preview read on the
  async task, the same accepted trade-off as `approval::is_project_scoped` (the
  full caller list lives in `src/tool/agent/sandbox.rs`'s async-callers
  contract).
- **file_edit EOL-agnostic matching.** Literal/fuzzy edits match in LF space
  on BOTH sides (the content is projected to LF with a byte-offset map back
  to the original; the needle is normalized to LF), then the replacement is
  spliced into the ORIGINAL bytes at the mapped offsets and re-emitted in
  the file's detected (majority) style — a mixed-ending file whose needle
  spans both styles no longer false-drifts on verified-identical text, and
  untouched regions keep their exact endings (plan be16ea36). The same
  `literal_splice` core powers the atomic multi-edit batch (`edits`: items
  applied in order, one write, any failing item aborts with the file
  untouched) and the append mode (`append`: new_string at EOF in the file's
  detected style).
- **IPC module split + contract.** The IPC layer (`src-tauri/src/ipc/`) is
  split into domain modules (agent, settings, files, backlog, spawn,
  events, run_all, state) with golden JSON contract fixtures
  (`contract_fixtures.rs` + `ipc-contract.test.ts`) guarding the
  Rust↔TS event/DTO shapes.
- **Frontend store.** `useAgentStore.ts` is a thin Zustand facade over pure
  per-event reducer modules (`agentEventReducer.ts`, `appearance.ts`,
  `agentState.ts`) — every `SerializableAgentEvent` kind is table-tested.
  The right panel is driven by a single view registry
  (`rightPanelViews.tsx`: `{ id, label, icon, component }[]`).
- **Prompt caching strategy.** Multi-provider prompt cache optimization across
  all agent interactions: (1) stable system prompt head isolated in `messages[0]`
  with byte-stable preamble, app rules, and constitution; (2) Anthropic
  Messages API: only the FIRST system message is hoisted into `system` with
  `cache_control: {"type": "ephemeral"}` (breakpoint 1); later system messages
  (volatile tail: steps, progress, memories + the byte-stable CONTEXT_FOOTER)
  are relocated into the final message's content as trailing text blocks — the
  uncached varying suffix — and a 3rd breakpoint on the final message's last
  real block caches the conversation history (tools + head + history + the
  current turn's real content; the next request's 20-position lookback finds
  the prior write since tool_use/tool_result runs coalesce to one position);
  automatic caching (a top-level `cache_control` field) was evaluated and
  rejected — it would land on the varying tail block, a fresh write every turn
  and never a read;
  (3) ephemeral `cache_control` on the final tool schema definition; (4) 3-tier
  tool schema priority ordering (`priority_class`) so universal base tools precede
  state-dependent workflow mutation tools, ensuring state transitions (Planning →
  Executing → Reviewing) append to the array without invalidating the tool prefix;
  (5) context-capped token quantization to 2048-token buckets to prevent parameter
  jitter; (6) policy-driven vendor-specific reasoning retention (Gemini keeps
  `thought_signature`s but drops historical thinking text; DeepSeek drops
  historical `reasoning_content` only under cache pressure — live-verified
  2026-12-23 (bug c9b5cbe4): DeepSeek validates only the request TAIL, the
  tail-owning assistant turn must carry a `reasoning_content` key (any value),
  and the builder injects it into the age-0 raw echo when missing so foreign
  429-fallback turns cannot poison the tail; all others keep the
  Rule-1 verbatim echo); (7) a proxy-cache-cliff compaction ceiling
  (`proxy_cache_ceiling_tokens`, default 340 000, `0` disables — Settings →
  Advanced) so long sessions summarize before crossing the ~340K-token mark
  where LiteLLM-class proxies drop whole-conversation prefix caching (hit rate
  collapse ~99% → ~4%); (8) vendor-specific packaging exception:
  DeepSeek-vendor models echo trailing system blocks instead of answering
  the user (sentinel-mirror 5cf5469c + the 2027-01-11 exit-note loop — the
  model echoed the injected RECALLED MEMORIES block, fabricated sections,
  and looped 366× on a 74-byte unit), so `ProviderPolicy::tail_as_user_messages`
  sends the volatile tail + CONTEXT_FOOTER as trailing USER messages for that
  vendor — the standard request shape (a tool turn ending in a user message),
  with no system block in the tail position, and messages[0] left byte-stable.
  Folding the tail into the leading system message instead (the first
  mitigation) cost that vendor its prefix cache: every complete_step progress
  bump rewrote messages[0], the cache died at the head, and each check-off
  re-billed ~115-120K tokens at 5.1-9.0% hit (2026-09-14 trace window).
  GLM/Anthropic/OpenAI keep trailing SYSTEM messages. Related loop defense: the in-stream R10 repetition
  guard (`detect_repetition`) detects loops of ANY byte period ≤ the
  window — the old exact-window suffix check was structurally blind to
  periods not dividing 200 (the 74-byte exit-note unit evaded it for
  64.5 s / 6,194 tokens until the user cancelled).

---

## Terminology

- **Agent tools** — the coding tools the agent uses to do work: `file_read`,
  `read_files` (batch sibling), `file_edit` (EOL-agnostic literal/fuzzy
  matching, atomic multi-edit batches via `ops` — compact line ops (`i`/`b`/
  `d`/`r` verbs with ranges and payloads) and anchor items sharing one array,
  append mode via `append`), `multi_edit` (atomic multi-file edits,
  `files: [{path, ops}]`, every file prepared before any write — one combined
  diff, one approval), `file_write`, `file_append`, `shell`, `search`, `git`,
  `describe_image`.
  Live in `tool/agent/`. Gated by
  workflow state. The two read tools cross-hint each other's argument shape
  in their invalid-args error when a model mixes up a call. `search`/
  `search_read` serve literal single-line queries from the FTS5 content
  index (`engine: index`, bm25-ranked, glob-filtered) and walk the tree
  otherwise; the walk PRUNES ignored directories (their children are never
  enumerated). When the pattern names an indexed symbol
  (`CodeGraph::symbol_id`, exact, best-effort) through a recognized shape —
  a bare identifier, a definition-prefixed name (`fn X`, `pub struct X` —
  `strip_definition_prefix`), or a symbol-INTENT pattern (`X(`, `a::b`,
  "who calls X" — `strip_symbol_intent`, backlog 8b8f40d2) — the search
  AUTO-DELEGATES to the code graph (backlog b804012f): a compact block
  (definition, top callers/callees, the `graph_context(id="file::name::line")`
  pointer) rides above the output and the file walk is skipped entirely; a
  query targeting semantic memory (a glob under `.coding/knowledge`/
  `.coding/reviews` or a typed `SPEC:`/`DECISION:`/`BUG:`/`PLAN:`/`HOW:`/
  `REVIEW:` prefix) delegates to `memory_search` the same way. The block
  opens with `AUTO-DELEGATED to …` and re-issuing the SAME query
  (pattern+glob+literal) skips delegation and runs the plain file search
  (the escape hatch — sticky per query: a bounded set of bypassed keys,
  so an identical re-issue never re-delegates in-session and interleaved
  delegating queries never break an earlier escape; a delegated answer
  counts as a steering switch, so the C5 intercept gate never punishes
  it). The advisory one-line note (symbol id embedded)
  remains for the escape repeat and fuzzy alternation branches — and a
  regex-mode walk of a metachar-free pattern (where
  regex ≡ literal match semantics) carries a TIP pointing at
  `literal:true` (never auto-routed: an FTS phrase and a regex are not
  semantically equivalent; counted as the fired-only `literal-tip`
  steering marker). The
  whole-file `read_files` read of a large (>300-line) indexed source
  file prepends its own SYMBOL NUDGE pointing at `graph_context(id=...)` +
  line slices — symbol lookups belong to `graph_search`/`graph_context`,
  and the notes make that drift self-correcting at the moment it happens.
  The reverse nudge covers the other direction: every `graph_*` miss carries
  the shared miss hint — near-misses point at candidate ids, total misses
  point at `search` (string literals are not the graph's territory). `shell`
  results of grep-family commands (grep/rg/git grep/findstr/Select-String —
  first token of the command or any piped segment) carry a one-line TIP
  pointing at `search`/graph tools; advisory only, the raw `data` fields
  stay nudge-free. Rider-side steering lives in `tool/steering.rs`
  (`recalled_context_block`, passive `recall_peek` shared by `create_plan`
  and `spawn_agent`; `agent/steering_stats.rs` counts per marker kind how
  often the next call followed the nudge — detection is tool-scoped, so a
  content-bearing result from a tool that emits no marker can never fire a
  kind falsely — surfaced via the `get_steering_stats` IPC command in the
  Trace tab header).
- **Workflow tools** — the plan-lifecycle tools that drive the enforced
  workflow: `create_plan`, `complete_step`, `update_plan`, `abandon_plan`.
  Live in `tool/workflow/plan.rs`. Gated by workflow state; only the main
  agent may mutate plans (sub-agents are denied). `create_plan` results
  carry a RECALLED CONTEXT rider when a memory store is wired: a
  best-effort passive recall (`recall_peek`, never bumps access) over
  title + goal (+ bug symptom; bug plans get an extra record_type BUG
  pass), age-gated to the recent 14 days (bug records exempt — durable
  knowledge never stales), deduped, capped at 5 one-line gist hits —
  structural enforcement
  of the "memory_search before planning" rule: prior knowledge arrives
  WITH the plan.
- **Skill tools** — `skill_start`, `skill_end`, `abandon_skill` plus the
  library tools: `skill_reload` (re-read `.coding/skills/*.toml` into the live
  `SkillLibrary`; allowed in every workflow state, an active skill included,
  and inside a skill's always-available set) and `skill_create` (author +
  validate a new skill file, hot-added to the registry; Executing only —
  PlanFrozen advertises it, and the constructor-granted allow-lists deny it by
  name so a skill file cannot widen the rule; its write is sandbox-mediated —
  a planted symlink or hardlink at the target is refused, and a link at the
  skills dir itself is refused when it leaves the project or resolves into a
  protected tree, and a skill shipped with the app such as `merge_to_main` is
  never rewritten even with `overwrite: true`) (`tool/workflow/skill.rs`).
  Enter/exit a skill overlay (tool allow-list + target state). Omitted when no
  skill library is configured.
- **Memory tools** — `memory_write`, `memory_search`, `memory_consolidate`
  plus the hygiene tools (`memory_update` — refines a record by id, or
  carries the `find`/`replace_with` targeted-repair mode that fixes stray
  text in a record body in place, `memory_amend` — appends a dated amendment
  paragraph to a knowledge record's file, stripping any leading `Amended …:`
  heading the caller supplies so exactly one heading lands, dated by the
   tool, `memory_supersede`, `memory_delete`), and the read-only git bridge
   (`git_read`: diff/log/show/status, `tool/agent/git_read_tool.rs`).
   `memory_search` is the single read path — one
  tool replaced the six former per-purpose read tools (they differed only by
  a record-type constant, a tier, or the presence of a query); its
  query-less browse mode (newest-first, narrowed by record_type/tier/prefix)
  replaces the former list-only tool.
  Persist and retrieve project-scoped memory across the four tiers
  (working/episodic/semantic/procedural); typed title prefixes
  (`SPEC:`/`DECISION:`/`BUG:`/`PLAN:`/`HOW:`/`REVIEW:`) classify records with
  auto-truncated digests (configurable length), and supersede-not-delete keeps history
  (superseded records excluded from default recall). Live in `tool/memory/`.
  **Always available** regardless of workflow state.
- **Spawn tool** — `spawn_agent` (`tool/agent/spawn_agent.rs`). Starts a
  background subagent; parent-aware so completion notifies the spawner.
  Registered only once the IPC layer wires in a spawner.
- **Views** — the tabs in the right UI panel: md viewer, plan progress, diff
  viewer, tool output, file browser, safety rules, stats, backlog. Driven
  by the `rightPanelViews.tsx` registry. (Not called "tools" to avoid
  overloading the word.)

## Technical decisions (locked in by research)

| Concern | Choice | Why |
|---|---|---|
| GUI framework | **Tauri 2.x** | Rust backend (the existing brain) + web frontend. Rich text, markdown, syntax highlighting, KaTeX, mermaid, live preview. Native window, small binary. |
| Frontend framework | **React 18 + TypeScript + Vite** | Largest ecosystem for the rich-text components we need (react-markdown, Shiki, diff viewers). User is building web apps — dogfooding a React UI is a feature. |
| Styling | **Tailwind CSS v3** (v3 toolchain: `autoprefixer` + `postcss`) | Modern, utility-first, fast to iterate. v4 is a possible future upgrade. |
| Component library | **shadcn/ui (Radix + Tailwind)** | Radix primitives adopted for the accessibility-critical components: dialogs (focus trap, `aria-modal`, focus restore) and tab bars (`role="tab"/"tablist"`, arrow-key nav) use thin `components/ui/` wrappers; the rest of the UI stays hand-built Tailwind on the same theme tokens. |
| Markdown | **react-markdown + remark-gfm + rehype-highlight** | GFM tables/task lists, syntax highlighting via highlight.js. Upgradeable to Shiki for VS Code-quality highlighting. |
| Code highlighting | **rehype-highlight (highlight.js)**; Shiki planned as the upgrade | highlight.js is the current baseline everywhere; Shiki (VS Code's highlighter, best-in-class token themes, lazy-loaded grammars) is the intended upgrade. |
| Diff rendering | **Hand-rolled LCS diff (frontend)** + **`similar` (Rust approval preview)**; react-diff-view + refractor as a possible upgrade | The frontend renders a custom LCS diff (no syntax highlighting); the Rust side uses `similar` for the approval-preview payload. react-diff-view + refractor would add proper unified/side-by-side syntax-highlighted diffs. |
| Icons | **lucide-react** | Clean, modern icon set matching shadcn/ui. |
| Async runtime | **tokio** | Already used by the brain; Tauri's async commands run on it. |
| LLM client | **Hand-rolled OpenAI-compatible client over `reqwest` (SSE)** + **hand-rolled native Anthropic Messages client** (`src/provider/anthropic.rs`) | One client per wire protocol, dispatched by endpoint kind; raw-JSON request builders (`build_request_json`) + hand-parsed SSE streams (the byte-identical plumbing shared in `src/provider/sse_util.rs`). Full OpenAI spec (streaming + tool calls, custom base URL) and full Messages spec (`/v1/messages`, `x-api-key` + `anthropic-version`, optional `anthropic-workspace-id` workspace attribution, hoisted `system`, content blocks, `max_tokens` required). (`async-openai` was evaluated and removed — the hand-rolled path is the production path.) |
| Provider strategy | **One OpenAI-compatible client, swap base URL; Anthropic Messages client for the `anthropic` kind** | OpenAI primary; Ollama/vLLM/LM Studio as fallbacks; Anthropic (and any Anthropic-compatible gateway/OpenRouter) via native `/v1/messages` — an Anthropic endpoint is never built as an OpenAI-compatible client. |
| Streaming tool calls | **Parse deltas ourselves, correlate by `index`** | async-openai gives parsed types but doesn't reassemble. |
| Diff library (Rust) | **similar** | Pure-Rust diff for approval prompts (LCS + word diffs). |
| Shell output filtering | **In-process deterministic regex filter** (`[shell_filter]` config, `src/tool/agent/shell_filter.rs`) | The PandaFilter idea minus the 90MB BERT model: per-command handlers (cargo build/test, npm test/build, git status) strip noise lines before the context window; hard safety rule that error-marked lines are never dropped, unknown commands pass through, and the raw output stays in the tool result's `data` field. No external deps, no ML, fully unit-testable. |
| Live shell output streaming | **Per-call `OutputSink` built at the dispatch site** (`AgentLoop::output_sink` → display-only `ToolOutputDelta` events) | Two reader tasks drain the child's piped stdout/stderr WHILE it runs and emit throttled, UTF-8-safe chunks (`STREAM_FLUSH_INTERVAL` 60 ms per stream, `STREAM_FLUSH_BYTES` 8 KiB batch), keyed by `tool_call_id`, so the card shows progress instead of a blank wait. The live view is budgeted (`STREAM_CAP` 256 KiB per call + a one-time truncation note) and the frontend keeps a rolling 16 K-char tail (UTF-16 code units), cleared when the result lands; the final `ToolResult` stays byte-identical (that is what the model consumes), approval/safety semantics are untouched, and the event is silent in the console — display-only, never part of the model's context. |
| Hang diagnostics | **Main-thread hang watchdog** (src-tauri watchdog.rs) | The 2026-08-20 AppHangB1 episodes were "stopped responding" hangs with no captured stack — WER gave nothing. A detector thread pings the main thread every 500 ms and, on a >10 s stall, writes `hang-<ts>.txt` (`.coding/logs/`, or `%TEMP%\mnemo-hang-reports\` pre-project) with the stall window, the agent's recent activity ring, and — Windows only — a GetThreadTimes busy-vs-blocked % sampled over the stall window itself (a busy loop ≈100%, a blocked thread ≈0%). Cross-platform core, `cfg(windows)` enrichment only. The watchdog's named stack capture is what turned the recurring hang into a root cause: three byte-identical stacks (`.coding/logs/hang-1787574592659.txt` et al.) showed a tao 0.35.3 keyboard self-deadlock — `public_window_callback` holds `KEY_EVENT_BUILDERS` while `KeyEventBuilder::process_message` calls `PeekMessageW`, which dispatches an inbound SEND that re-enters the window proc on the same thread and re-locks the non-reentrant `parking_lot` mutex. |
| tao (Windows event loop) | **Vendored `vendor/tao` 0.35.4 = 0.35.3 + PR #1215 backport, wired via `[patch.crates-io]`** | Upstream fixed the keyboard/IME self-deadlock in 0.36.0 (PR #1215, changelog `c704261c`) but `tauri-runtime-wry 2.11.4` / `wry 0.55.1` pin `tao ^0.35` and 0.36.0 bundles unrelated breaking changes — so the fix is backported onto the exact 0.35.3 the app runs and renumbered 0.35.4 to satisfy the pin. The peek is hoisted out of the locked regions (`process_message` takes `next_key_message` / `more_char_coming`). Guarded by `src-tauri/tests/tao_backport.rs` (source-level invariants; fails on pristine 0.35.3). Details: `vendor/tao/PATCHES.md`. |
| wry (WebView2 backend) | **Vendored `vendor/wry` 0.55.3 = 0.55.1 + OS-account SSO patch + hard-reload patch, wired via `[patch.crates-io]`** | Two patches. (1) SSO: the Browser tab's embedded WebView2 demanded interactive logins that the org's Conditional Access rejected (2026-09-07, forms.cloud.microsoft) — unlike Edge it had no OS-account SSO. WebView2's `AllowSingleSignOnUsingOSPrimaryAccount` is exactly that switch, but wry 0.55.1 never sets it and Tauri 2's `WebviewBuilder` exposes no environment hook — so the vendored crate's `create_environment` sets it (`set_allow_single_sign_on_using_os_primary_account(true)`), giving every WebView2 in-process Edge-equivalent silent AAD/MSA auth (the main + agent-chat webviews only load local content, so it is inert there; the CDP debug port comes via the `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` env var, unaffected). (2) Hard reload (2027-01-11): the Browser tab's Reload served cached subresources (`ICoreWebView2::Reload()`), so pages came back out of sync; the vendored `reload()` issues CDP `Page.reload` with `ignoreCache` via `CallDevToolsProtocolMethod` (no native hard-reload API exists; Tauri 2 exposes no hook), falling back to `Reload()` on call failure — only the Browser tab's Reload button ever reloads, so that is the whole blast radius. `tauri-runtime-wry 2.11.4` pins `wry ^0.55`; renumbered 0.55.3. Guarded by `src-tauri/tests/wry_sso_patch.rs` + `src-tauri/tests/wry_hard_reload_patch.rs` (source-level invariants; fail on pristine 0.55.1). Details: `vendor/wry/PATCHES.md`. |
| Multi-instance WebView2 profiles | **Per-instance user data folders (Windows): instance 1 keeps the persistent default (`%LOCALAPPDATA%\com.mnemo.desktop\EBWebView), every additional instance gets `...\WebView2\mnemo-<pid>`; same-project double-start warns** | A second mnemo instance passed an EMPTY user data folder like the first, so both resolved the same default UDF and the second could not start its WebView2 session — a live but blank white window (2027-01-13, user report; a UDF holds at most one WebView2 session and a differently-configured second environment fails to create objects — MS Learn). Decided by a Windows named mutex keyed on the resolved default-UDF path (fresh acquire = primary, else per-pid dir): `src-tauri/src/webview_udf.rs` (`apply` wraps all three WebviewBuilder sites; `install_default_data_dir` decides at startup after project resolution), pure helpers in `src/webview_args.rs`. Primary keeps the persistent profile so cookies/OS-SSO survive restarts; macOS untouched (apply is a pass-through). Same-project detection: `<project>/.coding/instance.json` {pid, started_at} + a PID-only liveness probe (`src/instance_marker.rs`, `instance_pid_alive`) → conflict flag on the startup snapshot → frontend InstanceConflictDialog with the ProjectPicker reachable — no more silent duplication. Guarded by `src-tauri/tests/webview_udf.rs` (four source-contract tests, fail without the fix). Gotcha: plain `cargo build` leaves the opt-in `custom-protocol` feature off, so debug binaries resolve the Vite devUrl and blank-screen even with the fix — acceptance binaries must be built `--features custom-protocol`. |
| Agent event delivery | **Rust-side delta coalescing in the forwarder** (`src-tauri/src/ipc/events.rs` `DeltaBatcher`) | Per-token `TextDelta`/`ReasoningDelta`/`ToolCallArgDelta` are batched per (agent, kind, tool-call index) and emitted as concatenated events on a 16 ms timer / structural event / 64 KiB cap — cutting the per-token IPC/WebView2 chatter that drives the inbound SEND behind the tao deadlock and the posted-queue backlog symptom. The frontend already rAF-buffers deltas (`useAgentEvents.ts` hybrid flush policy), so the batching is UI-invisible. |
| Typed records | **Knowledge-as-files: `.coding/knowledge/<type>/<date>-<slug>.md` (TOML front matter + unbounded body, [[wiki-links]]); DB is a rebuildable cache** | Typed records (SPEC/DECISION/BUG/HOW) live as git-tracked markdown — git IS the merge mechanism across instances. The indexer derives budgeted digest memories (UUIDv5 source keys + content hashes) so both worktrees converge with no import step; `memory.db`/`codegraph.db` are gitignored caches (startup reconciliation on content-hash drift, delete-to-rebuild). Superseded-by-merge-time-bundles decision (LWW updated_at + imports ledger) superseded — files make it unnecessary. |
| Branch topology | **One `wt/*` working branch per agent directory (worktree), reused across plans → protected `main`** | Git can't check out one branch in two worktrees, so each instance gets its own `wt/*` line (named from the directory). `create_plan` auto-forks it from `main` when on `main` (no `branch` arg) and reuses the current branch otherwise — work never silently stays on `main`; the `branch` arg is reserved for an explicit user request. `merge_to_main` lands the branch into `main` — on unprotected repos directly (sync `main` with `origin` first (`git fetch` + `git pull --no-rebase`, so the landed merge is immediately pushable instead of leaving `main` diverged), merge, then delete the branch — no accumulation); where a main-protection ruleset applies (this repo since 2026-09-21, ruleset 23755694) the skill detects it up-front, pushes the branch, opens a PR, and stops for the human approving review (the branch is deleted after the human merges). Parallel run-all's app-managed landings are ruleset-aware the same way (backlog b52b041a): on an unprotected `main` they sync `main` with `origin` before their `--no-ff` merge (2027-01-11) — stale-main drift must not be left for a human to notice hours later — and on a protected `main` they push the worktree branch and open a PR (the URL reported on the item; the branch kept for the human merge, swept at the next run-start once merged into `origin/main` — backlog 64662ef2). The sync is skipped when `main` tracks no upstream; once it tracks one, a fetch/pull failure is surfaced rather than swallowed. `plans/stack.json` is gitignored per-instance local state; `.coding/` (plans, reviews, knowledge, backlog) travels with git. |
| Backlog format | **`.coding/backlog.jsonl`: one JSON item per line, UUID string ids, git union merge driver** | Concurrent `backlog_add` on two branches must not collide — line-oriented jsonl makes git's `merge=union` concatenation correct, and UUID ids never collide across worktrees. Legacy `.coding/backlog.json` (envelope + numeric ids) is migrated on open. |
| Link-time levers | **Feature-gated heavy deps (`browser` = chromiumoxide, `embeddings` = fastembed/ort), dep-side `debug = false`, one merged integration-test binary, Defender exclusions; CGU=1 measured and rejected** | Plan 1c712c30 (2026-12-19), from the link-time research doc. Library-only/light builds skip the browser + embeddings trees entirely (the app always selects both features, so the shipped app is unchanged); dependencies compile without debuginfo while workspace code keeps `line-tables-only` backtraces (smaller link input + PDBs); the four integration test targets merged into one `tests/integration` binary (one full-stack link per `cargo test` instead of four, same 1986 tests); Defender excludes `target\`, `~\.cargo`, `~\.rustup`. `codegen-units = 1` measured no warm-loop win (3.2s vs 3.21s — dev incremental dominates) and was not added. From-clean workspace build 91.4s; warm loop: check 2.3s, test --lib 13.2s, test --workspace 21.0s, app rebuild 8.8s. |

### Critical gotchas baked into the design
1. **Ollama has no `tool_choice`** — the provider exposes a `Capabilities` struct; the agent loop checks it before sending `tool_choice`/`strict`.
2. **`finish_reason` is unreliable on local models** — we detect tool calls by *presence of tool-call deltas*, not solely by `finish_reason: "tool_calls"`.
3. **Local-model tool-call JSON is often malformed** — always validate `arguments` before parsing; never `unwrap()`.
4. **Tool-call argument deltas arrive in fragments** — accumulate per-`index` in a `HashMap<u32, ToolCallAccumulator>`.
5. **`AgentEvent::ApprovalRequest` carries a `oneshot::Sender`** — this is NOT serializable across the Tauri IPC boundary. The IPC adapter must hold the oneshot in Rust and send only a serializable approval-request event to the frontend; the frontend responds via a Tauri command that resolves the oneshot.
6. **Reasoning models (glm-5.2 / DeepSeek thinking mode / Ollama) emit `reasoning_content`, `reasoning`, or inline `thinking` deltas** — surfaced as `ReasoningDelta` events; the frontend renders these in a collapsible "thinking" section (and now mirrors the live answer into the same box).
 7. **Reasoning-state continuity (raw-payload source of truth)** — reasoning models pause their internal reasoning when they call a tool, and every provider resumes it differently (Gemini `thoughtSignature`, Claude thinking-block `signature`, OpenAI Responses `encrypted_content`, DeepSeek `reasoning_content`). The app stores each provider's assistant turn **verbatim** in `Message::raw` (reassembled from streamed deltas via `LlmEvent::RawAssistantDelta`), and the request builder echoes `raw` unchanged instead of reconstructing from view fields — so every key the provider sent (reasoning fields, signatures, unknown keys) round-trips byte-identical. A `ProviderPolicy` data object (`src/provider/policy.rs`) captures each provider's reasoning contract (which field, whether stateful, whether portable) and drives cross-vendor detection (`is_cross_vendor`); full builder consultation of `include_params`/`reasoning_field` is a follow-up (the values are currently hardcoded correctly in the builders). Rule 1 scope is now policy-driven via `ReasoningRetention` (same file): Gemini drops thinking *text* from assistant turns older than the most recent (per-tool-call `thought_signature`s always survive); DeepSeek drops historical `reasoning_content` only under cache pressure (`estimate + PROXY_CACHE_PRESSURE_MARGIN_TOKENS > PROXY_CACHE_CEILING_TOKENS`); all other providers keep the full payload verbatim. DeepSeek's tail contract (live-verified 2026-12-23 against api.deepseek.com, bug c9b5cbe4): with `reasoning_effort` present, the assistant turn that owns the request tail — the last message, or the issuer of trailing tool results — must carry a `reasoning_content` KEY (any value, even `""`); a missing key is the recurring HTTP 400 "The `reasoning_content` in the thinking mode must be passed back to the API" (burst pattern in provider-errors.jsonl: foreign 429-fallback turns from GLM/Kimi issue tool calls whose raw lacks the key, the open-loop tail fails validation, and the provider-error note-retry loop re-sends the same broken conversation). The builder guarantees the key: the age-0 raw echo gets `reasoning_content` injected when the policy requires it and the raw lacks it (`raw_echo_injects_reasoning_content_when_required_but_absent`), and `keep_recent >= 1` keeps the tail key intact even under pressure — historical turns tolerate a missing key, so the strip stays byte-saving. The harness's `off` effort encodes per provider (policy-driven `reasoning_effort_off_wire_value` in `policy.rs`): omitted for most, `"none"` — the enum's explicit thinking-off variant — for DeepSeek-family models, since a literal `"off"` is an instant non-retryable 400 (`unknown variant \`off\``); both config resolvers (`resolve_reasoning_effort`, `effective_reasoning_effort_for`) and the request builder's emission enforce it (`build_request_json_maps_off_effort_to_none_for_deepseek`), with the endpoint/model `reasoning_effort_off_wire` config (model-level wins) overriding the name-based policy when set — the escape hatch for renamed/aliased/fine-tuned DeepSeek models (2027-01-07, backlog 1e40407c). Retention stripping touches the outgoing wire payload only — stored `Message::raw` is never mutated. Cross-vendor model switches strip reasoning + set `reasoning_stripped: true` (Rule 5; one-way — see `strip_cross_vendor_reasoning` doc). Compaction never trims inside an open tool loop (Rule 4). Serialization-bug 400s (missing `thought_signature`, `reasoning_content must be passed back`, `Expected thinking…found text`, `modified prior content`) are classified as non-retryable and surfaced (Rule 6). The OpenAI Responses API (`previous_response_id`) provides server-side stateful mode (Rule 3; assumes every assistant turn receives a `response_id` from `response.created`/`response.completed` — see `message_to_responses_input` doc); the stateless raw-echo path is always available and both pass the same tests. See `.coding/knowledge/spec/2026-12-20-gemini-is-proxy-only-stateless-thought-signature.md`.
 8. **GLM-5.3's new tokenizer boundaries are not server-enforced** — GLM-5.3-Flash introduced role-tag boundaries (the pipe-delimited endoftext, user, assistant, and observation special tokens) plus newline cascades (`\n\n\n\n` and `\n\n\n`) that OpenAI-compatible servers only honor if the request carries explicit stops; without them the model runs past a closing code block or reasoning tag into thousands of empty lines until `max_completion_tokens` (backlog 82a9480c). Config-driven since 2027-01-05 (backlog f322277c): the boundary strings live in `stop_boundary_strings` on `Endpoint`/`ModelSpec` (resolved per model via `stop_boundary_strings_for`, model-level overriding endpoint-level — the same pattern as `stop`/`stop_token_ids`); the request stop list is the configured boundaries + the user's `stop` (deduped, boundaries first), and `stop_token_ids` is a plain config pass-through (the shipped GLM example carries `[151329, 151330, 151336]` — the same boundaries tokenized, so servers honoring either field stop correctly). No model-name prefix matching exists — any model (an alias, a fine-tune, a proxy rename) with `stop_boundary_strings` configured gets the same protection. User-configured stops, stop token IDs, sampling parameters (`temperature`, `top_p`), and `extra_body` on `Endpoint` and `ModelSpec` are supported and merge cleanly over defaults. Additionally, a client-side SSE stream guard terminates turns early with `FinishReason::Stop` if raw boundary tokens appear in text deltas — the guard reads the same configured boundary list (plus the user's `stop`), so it also cuts the runaway-empty-lines symptom at the first configured cascade. See `.coding/knowledge/spec/2026-12-21-glm-5-3-flash-stop-token-boundaries-sampling-par.md`. Transport hardening lesson (review round-1): the raw tag text was stripped in text transports during development and silently turned the stop list into empty strings — the tag literals in the builder and its tests are therefore built from `\u{3c}`/`\u{3e}` escapes, never raw angle-bracket text. Round-trip escaping (backlog 1db26c95, 2027-01-08): the serving layer strips/maps these tokens from request INPUT as well — the model's textual view of a tool result carrying the raw token shows an empty string, so a read-then-write round-trip silently corrupted files (verified: an echo test showed the model perceiving the token position as "a blank line"). The request layer therefore escapes configured boundary tokens in tool-result messages to a visible, reversible marker (`⟦raw:\uXXXX…⟧`, `src/provider/boundary.rs` — a single-pass longest-match escape, so the round trip is an identity for any token set), and the file tools restore the marker to the raw token on write — file_write `content` AND its append path / `file_append` (the chunked-write path large files are steered onto), file_edit `old_string` + `new_string` (regex-mode `old_string` restores to a regex-quoted form so the token's `|` alternation can't change match semantics) — the round-trip is faithful. The Anthropic Messages path is intentionally unescaped (its API doesn't strip input tokens — the raw token round-trips natively). The unicode-escape convention above remains for source authoring (the marker is for tool-result transport, not file content).

## Provider model: OpenAI-primary, local-fallback

Providers are **not** equal peers. OpenAI is the optimized primary path; local
models are a capability-degraded fallback. The design is **capability-aware**,
not lowest-common-denominator.

```rust
struct Capabilities {
    supports_tool_choice: bool,   // OpenAI: true, Ollama: false
    supports_strict_schema: bool,  // strict mode for the mutation/plan tools (normalized schemas); per-endpoint override in endpoints.toml
    supports_parallel_tools: bool,
    reliable_finish_reason: bool,  // OpenAI: true, Ollama: false
    max_context: usize,
}
```

The agent loop queries `provider.capabilities()` and adapts its requests.
Strict schemas (2027-01-24): when `supports_strict_schema` is true,
`ToolRegistry::schemas` normalizes the mutation/plan tools' schemas to
strict-legal form (every property required, optionals widened to nullable
type-arrays, `additionalProperties: false`) and flags them `"strict": true`
— the single decision point, so the advertised array, the token estimate,
and the circuit-breaker's corrective schema all agree. The per-endpoint
`supports_strict_schema` key in `endpoints.toml` overrides the kind default
(litellm/vertex-class proxies reject the field outright). Tool-argument
errors are sanitized (`src/tool/error_message.rs`): the model receives a
clean message naming the tool, the offending parameter, and the expected
type — never raw serde vocabulary or the raw arguments blob.
A `Provider` enum carries the base URL + which capability set applies:
```rust
enum ProviderKind { OpenAI, Local, Anthropic }   // Local = Ollama/vLLM/LM Studio/llama.cpp; Anthropic = native Messages API
```
Config selects a default provider; `--provider` / `/provider` switches at
runtime **for newly built agents** — the factory (`agent/factory.rs`) builds
each new agent loop with the selected provider. Switching the provider for an
already-**live** agent is a separate per-loop step via `set_provider`; the two
paths are not atomic. `/model` and `/provider` therefore require a restart (or
a fresh agent) to take effect for the main provider — the slash-command help
notes this.

## Enforced plan-first workflow

The agent operates under a hard workflow gate, not a soft prompt instruction.
`Workflow::allowed_tools()` returns a `ToolFilter` that the agent loop applies
*before* building the request schema:

- `Planning` → read agent tools + `create_plan` + all memory tools. Write agent
  tools, `shell`, and `complete_step` are **omitted from the `tools` array**.
- `Executing` → all agent tools + `complete_step` + all memory tools.
- `Complete` → read agent tools + all memory tools.

The schema array is the **plan-frozen advertisement** (2027-01-10, perf review
L4; extended 2027-01-11): while a plan is active (Executing/Reviewing), the loop
builds the `tools` array from `Workflow::schema_filter()` — the SAME
`PlanFrozen` (the Executing surface ∪ `finish`) for implementation and
research plans alike — byte-stable for the plan's whole lifetime. A research
plan's narrower surface was tried and rejected: the array shrank at the
plan-kind boundary, resetting the cached prefix at the head of the request and
re-billing the whole conversation (a measured 6,016-token collapse plus
15.7-17.9 s TTFT on the first request after the transition, plan c43ad4a3).
The provider prefix cache reuses the request body only
up to the first changed byte, and the tools array rides at its head, so the
array changing at the Executing→Reviewing transition would reset the cached
prefix and re-bill the entire conversation (~40-75s of server-side prefill at
late-plan context sizes). Dispatch re-checks the per-state `allowed_tools()`,
so tools advertised by the frozen surface but not allowed in the current state
(`complete_step`/`create_plan` during Reviewing, `finish` during Executing,
write tools during a research plan except on `.coding/**` artifacts) are still
rejected at dispatch — advertisement is frozen, enforcement is not. The
research write rule is path-scoped and lives in the dispatch layer, the only
place that sees the call's target path.
Outside an active plan (Planning/Complete) and for allow-listed sub-agents
(reviewer/skill) the per-state filter applies unchanged.

The plan lives on disk at `.coding/plans/<id>.md` as a markdown checklist.
In-memory state is **derived** from the file, never the other way around — so a
crash or restart loses nothing. On startup, the agent reads the plan file,
parses the checkboxes, and derives state + the resume point (first unchecked
step).

Long plans are written in **chunks** (mirroring `file_write`'s append mode):
`create_plan` carries the first few steps, then `update_plan` with
`append: true` adds further steps after the remaining ones (and extends the
context) in as many small calls as needed — no resending of existing steps.
`update_plan` without `append` keeps its replace semantics; both preserve the
completed prefix.

Both tools enforce a **resumability gate** (2027-01-09): the context must be
substantive (≥40 chars — symptom/root-cause, file anchors, verification
commands), every step must name concrete file path(s) or carry an explicit
no-code marker (the same vocabulary as the backlog detail bar), and
`bug_fixing` plans must name the regression-test design; `update_plan`
validates the effective post-update text (a short hardening chunk appended
onto a substantive context passes; a thin replacement fails), and one
actionable error names every violation so the retry fixes all of them in one
shot.

### Plan kinds

`create_plan` takes a `kind` (persisted as the `## Kind` section):

- `implementation` (default) — completing the root plan's last step enters
  `Reviewing`; `finish` is gated on a reviewer report.
- `research` — completes straight to `Complete` (no review; for
  investigation/planning that changes no source code). Its file tools are
  denied by NAME (`ToolFilter::ExecutingResearch`) but pass at dispatch on
  `.coding/**` ARTIFACT paths (2027-01-11) — source, docs, protected side-car
  files, `..` escapes, any link component that leaves the root or cannot be
  resolved, and any hardlinked `.coding/**` file stay denied (the hardlink rule
  since plan b4812291, 2027-01-15; the link rule since plan ee65fd4b,
  2027-01-11) — an in-root link that resolves inside the artifact tree is still
  a grant, so "no source diff" still holds. A root
  that would skip review still owes one when an `implementation`/`bug_fixing`
  SUB-plan completed OR was abandoned under it (`Workflow::review_required`,
  persisted in the stack sidecar): the write filter keys on the ACTIVE plan
  while the review decision keys on the ROOT kind, so without that flag nested
  source edits shipped unreviewed.
- `bug_fixing` — CONTAINED bug fixes use this kind (never `implementation`
  for a contained defect). A bug-triggered FEATURE (the fix adds
  capabilities, new dependencies, or spans multiple modules) belongs in
  `implementation` with the bug documented as motivation in goal/context
  (2027-01-11, backlog 51ee41c1 — cf. plan c87093c1). The
  4-step skeleton is **locked** (reproduce with a failing regression test →
  document root cause → minimal fix → verify; `update_plan` refuses steps
  replacement, and steps append except in Reviewing — the follow-on
  window, backlog 37f8631a: a review-surfaced fix becomes a checkable
  appended step, unchecked through the review, and a crash mid-follow-on
  resumes with it as the active step), provided steps persist as CHECKABLE
  sub-items under `## Detailed steps` (the crash-resumption detail; text
  immutable like the skeleton, each sub-step's checkbox ticked via
  `complete_step detailed_step_index` so a restart shows exactly what
  shipped; escaped if they'd collide with plan-file structure), the `bug: <symptom>` param is required, the
  verify step must
  record the regression test name (`update_plan regression_test`), a fix
  that turns out feature-scale mid-flight records `landed_design` + a
  "Landed design" context amendment (architecture, deviations from the
  sketch, measured numbers, invariants — the finish gate blocks the flag
  without the amendment; agent.md "Documentation expectations"), and
  `finish` validates the name against the code graph, auto-captures PLAN:/
  BUG: memory digests (carrying a `branch <name> @ <sha> (unmerged — exists
  only on this branch)` hint when the finish lands on a non-main branch),
  and supersedes the plan's crash markers.

Merge hygiene: the `merge_to_main` skill's post-merge step supersedes the
branch-status records citing the merged branch (memory tools are always
available inside a skill — no allow-list entry needed), so stale "unmerged"
hints stop recalling once the branch lands. The compiled prompt encodes the
default-assumption rule: a feature/bug with no live unmerged marker is
assumed to be in main (verify with `git_log`/`git_show` when it matters).

### Review verdict contract

Review reports (`.coding/reviews/*.md`, written by the read-only reviewer
subagent via `write_review_report`) MUST open with a verdict line —
`## Verdict: PASS` or `## Verdict: FINDINGS (n high, n low)`. An unparseable
verdict counts as FINDINGS (fail closed): `write_review_report` rejects
verdict-less reports and `finish` blocks on FINDINGS/unparseable verdicts.
Long reports are written in chunks (like `file_write`'s append mode): the
first call carries the verdict line + summary (`mode: "append"` also creates
the file), follow-up calls append finding sections with `mode: "append"`
(no verdict line needed mid-file). The verdict contract is enforced whenever
a call CREATES the report — an existing report that lacks a verdict line is
never grown (fail closed), and the sandbox guards hold identically in both
modes.

A `role:"reviewer"` subagent is read-only by construction: read tools +
`git_diff`/`git_log`/`git_show`/`web_fetch` + graph tools + memory/backlog
QUERIES (`memory_search`, `backlog_list`) +
`write_review_report` — no `ask_user`, no memory/backlog mutations, no plan
tools, no `finish` (strict `ToolFilter::Reviewer` arm, `spawn.rs`
`REVIEWER_BASE_TOOLS`, `backlog_list` tool). **Reviewer-only authorship is
structural:** `write_review_report` is visible under no base-state filter
(only the constructor-granted reviewer allow-list), and `.coding/reviews/` is
protected from the file tools (`Sandbox::is_protected_write_target`) — the
main agent can never author a review. A reviewer that fails without a
report is handled by the failed-reviewer protocol: the parent must `ask_user`
(retry on another model / abandon the review — never self-review), a blind
reviewer respawn is refused while the failure is pending, and an
un-completable review is escaped via `abandon_plan` (Reviewing → Planning).

### Context management

As the agent works, the conversation grows. A `ContextManager` counts tokens
(tiktoken-rs for OpenAI; char-estimate for local) and, at a configurable
fill-rate threshold (default 50%), summarizes the oldest turns into a single
system message, keeping recent turns + the system prompt verbatim. Large tool
results (file reads, shell, git) are truncated with a note.

### Error recovery

The agent loop feeds every error back to the model as a `tool` role message
(standard recovery pattern): tool execution failure, malformed JSON, unknown
tool name, empty response. A retry cap (`MAX_RETRIES = 3`) prevents infinite
loops for tool-execution failures; when hit, the agent surfaces an
`AgentEvent::Error` and pauses. Malformed tool-call arguments (bad JSON) use a
separate, higher cap (`MAX_BAD_JSON_RETRIES = 8`) because the model can
self-correct by emitting valid JSON — three strikes does not make sense for an
error the model can recover from. Provider errors retry with jittered
exponential backoff (equal jitter via `retry_backoff_ms`: ~0.5–1s, ~1–2s) so
concurrent agents retrying the same recovering endpoint don't stay in lockstep.

A repeat-failure circuit breaker covers the self-reinforcing shape
(valid-JSON-wrong-fields): when a failed result's (tool, error text) matches a
prior identical failure in recent history, the loop pushes one
harness-attributed user-role corrective message after the batch — the
error verbatim plus the tool's actual parameter schema — because the
empirically proven loop-breaker is a fresh corrective turn, not the error
string (a live incident re-emitted the same missing-`message` git commit
15 times while the error text repeated identically). The scan is
history-based, so it works across turn boundaries and decays naturally at
the compaction boundary; `MAX_RETRIES` semantics are unchanged.

Bad-JSON repeats get a sibling mechanism (backlog 38040f12): when the same
tool's arguments fail to parse as JSON twice, the per-call retry message
switches from schema advice to a strategy-changing, harness-attributed
correction naming a different formulation per tool — `literal: true` or a
metacharacter-free pattern for search/search_read, chunking for
file_write/file_edit, rebuild-from-scratch otherwise — because a repeat
means the emission itself is failing (typically escaping), which the
model already knows the schema for. The correction rides the Tool role
(context-only, no Error event) and the `MAX_BAD_JSON_RETRIES = 8` backstop
is unchanged.

### Path safety (sandbox)

All file agent tools route their path through `Sandbox::validate()` before any
I/O: canonicalize + `starts_with` check. Path traversal, symlinks pointing
outside, and absolute paths are all caught. The `.coding/` subdir is writable;
the rest of the project is read/write; nothing outside the project root is
accessible.

## Projects & startup

**Hard rule: one app instance works on exactly one project.** A project is a
directory. All project state lives *inside* that directory under `.coding/`.

### Two config scopes

```
~/.mnemo/                          # GLOBAL — configured once, shared across all projects
├── config.toml                    # general preferences (default provider/model, safety, UI,
                                   #   [memory] retrieval knobs: decay half-life, per-query cap,
                                   #   derived per-class cap, digest budgets)
├── endpoints.toml                 # endpoint definitions (base_url, kind, models —
                                   #   plain ids OR per-model tables with their own
                                   #   max_context / max_output_tokens / reasoning_effort
                                   #   / reasoning_efforts)
├── keys.toml                      # secrets (API keys only; ACL-restricted)
└── projects.toml                  # the known-projects registry (name → path)

<project-dir>/.coding/             # PER-PROJECT — self-contained, lives in the project
├── memory.db                      # project-scoped memory store (SQLite + FTS5 + vectors) — CACHE, gitignored
├── codegraph.db                   # tree-sitter knowledge graph — CACHE, gitignored
├── knowledge/                     # typed memory records as files (spec/decision/bug/how) — git-tracked truth
├── plans/                         # plan files (the workflow source of truth); stack.json is per-instance
│                                  #   local state, gitignored (never blocks a checkout, never gets carried)
├── reviews/                       # review reports (verdict-line contract, see above)
├── backlog.jsonl                  # the user's backlog queue (jsonl, UUID ids, git union merge driver)
└── config.toml                    # project-specific overrides (optional)

<project-dir>/agent.md             # PROJECT constitution — hard rules the LLM cannot ignore
                                   # (kept at the project ROOT, not .coding/, so it's visible
                                   #  and easy to edit — see src/project/mod.rs)
```

### The two `agent.md` files (non-negotiable context)

Two markdown files form a "constitution" the LLM cannot ignore. They are loaded
before anything else, every time an agent starts an LLM turn, and prepended to
the system prompt in a fixed order: global first, then project. Re-read from
disk each turn (cheap; small files).

### System prompts (compiled constants)

The system prompt is built entirely from compiled constants in
`src/agent/prompt.rs` — the preamble, workflow lifecycle, app rules,
tool-call discipline, tool strategy, memory records, the per-state workflow
guidance, and the per-tool descriptors in the tools array. They are not
configurable per install: prompt changes ship with the app (update the
constants + their tests), keeping the model-facing context byte-stable and
cache-friendly. The dynamic workflow lines (CURRENT PLAN / GOAL / PROGRESS /
CURRENT STEP, skill goal) and the context footer (the byte-stable prompt-cache
sentinel) are likewise compiled.

### Startup: choose a project

On launch, the app resolves a project directory before anything else runs:
1. **`--project <path>`** flag — use it directly (init if needed).
2. **Auto-detect** — if launched inside a directory containing `.coding/`, use it.
3. **Known-projects list** — `projects.toml` registers projects by name → path.

## Architecture

### Multi-agent runtime (decoupled, channel-only)

The agent and UI are **fully decoupled actors**. They share no mutable state —
all communication is via typed channels. An `AgentManager` sits between them: it
spawns agents, routes commands to the right agent, and fans-in their events.

**Multi-agent readiness.** The runtime is multi-agent-*ready*: spawning works
and each agent gets an independent per-agent plan/workflow via the factory.
The two former scale-out limits are now addressed:

- **Memory** (`src/memory/mod.rs`): reads run on a separate WAL-mode
  connection from the single serialized writer (`journal_mode=WAL`,
  `busy_timeout=5000`), so concurrent agents' recalls no longer queue behind a
  write. (The recall scan is capped at the 200 most-recent rows on both fallback branches — FTS unavailable OR zero keyword matches.)
- **Approvals** (`src-tauri/src/ipc/approval.rs`): pending approvals are keyed
  `(agent_id, tool_call_id)`, and `cleanup_for_agent` drops only the exited
  agent's entries — concurrent agents' approvals are isolated.

One limit remains **by design**: safety-mode and config are **intentionally
global** (shared across agents), not per-agent. Models are **per-agent**: the
status-bar picker switches only the active agent's model (other agents keep
theirs, newly spawned agents keep the configured default — except a spawned
`role:"reviewer"`, which is pinned at spawn to the `[models.reviewing]`
chain: reviewing → executing → subagent → default; an explicit
`spawn_agent` `model` arg on a reviewer spawn is dispatch-denied (`reviewer_spawn_gate`) unless it is the user-sanctioned failed-reviewer retry — for non-reviewer spawns the arg still wins), while the console
`/model` REPL and the Settings endpoints save (`save_endpoints`) stay global —
they change the configured default for every agent. A picked model **pins that
agent** (the loop's `explicit_provider` slot), **state-scoped** (2026-12-20):
the picker's live choice beats the config's `[models.*]` per-state/skill/
subagent routing chain within the workflow state it was first served in
(`pinned_state`), while in any other state it holds only until a configured
slot takes over — on a state change (e.g. `create_plan` flipping Planning →
Executing), a configured `[models.executing]` wins and the pin goes dormant
(`explicit_provider` is kept, so it resumes if the workflow returns to its
state, e.g. `abandon_plan` back to Planning); with nothing configured for the
new state the pin keeps serving instead of falling to the default model.
Per-state routing still applies to agents that were never manually switched.
So: spawning, plan
isolation, concurrent memory reads, and approval isolation all hold today;
the remaining constraint is that safety/config changes are global.

**Reviewer-spawn-only reviewing slot (2026-12-06).** The main agent's
per-state routing keeps `[models.executing]` through the Reviewing closing
sequence — it never switches to `[models.reviewing]`. That slot's sole
consumer is the spawned-reviewer pin (`ModelResolver::resolve_reviewer_model`
in `src/model_resolver.rs`, called by `spawn_agent` at spawn time):
reviewing → executing (back-compat) → subagent → default.

**Bug-fixing plan-kind slot (2027-01-07).** `[models.bug_fixing]` is a
plan-kind override, not a state: while the active (top-of-stack) plan's kind
is `bug_fixing` and the workflow is in that plan's active lifecycle
(Executing or Reviewing), it wins over `[models.executing]` — a bug-fixing
session can run on a max-reasoning model while implementation plans keep the
executing slot. Unset (or dangling — the endpoint no longer exists) inherits
the executing chain, then the default: today's behavior. The agent loop
resolves per iteration from `wf.state()` + `wf.active_plan_kind()`, so the
switch lands on the next provider request after the `create_plan(kind =
bug_fixing)` call — in-app plan creation and Run-All item dispatch (whose
steered defect items create bug_fixing plans) both flow through it with no
dispatch-side change. Deliberately not engaged in the Complete state (the
plan is finished — the complete slot governs), for subagents (the Subagent
role state), or while a skill is active (the skill slot wins).

**Summarize slot (2027-01-09).** `[models.summarize]` routes compaction
summaries to a dedicated — often cheaper — model: the auto-compaction summary
call (`maybe_compact` / `handle_pending_swap` in `src/agent/turn.rs`) and the
run-all between-items compact (which funnels through `compact_context` in
`src/runtime/agent.rs` via `AgentCommand::Compact`) all resolve their summary
provider through `AgentLoop::summarize_provider`
(`ModelResolver::resolve_summarize_model` in `src/model_resolver.rs` — sole
consumer, like the reviewing slot's spawn-time pin). Unset (or dangling —
the endpoint no longer exists, or the provider build fails) rides the turn's
provider: today's behavior, no config, no change. The resolved model is
never stamped into the turn's display state — a summary is a one-off
internal call, not a turn model switch; the `compact_ms` metric stays
attached to the turn's provider trace wherever the summary ran. The summary
prompt is model-agnostic (structured handoff format), so a cheaper model is
low-risk. Caveat: the summarization REQUEST is now budgeted against the
summarize model's own advertised window (`summary_prompt_budget` →
`context_budget`), so a cheap-but-small model no longer fails on large
contexts: the cut marches backward until the serialized region fits (keeping
more recent turns verbatim), an over-budget region caused by a single monster
message is truncated per-message with a marker, and if the provider still
rejects the request for size the turn falls back to a mechanical compaction
(the region becomes a harness note) instead of failing. Naming the summarize
slot to a model that mis-advertises its window therefore degrades the SUMMARY
quality rather than the session (and the downgrade is recorded); pick a cheap
model with a large window, and note the budget comes from the *advertised*
window — only the mechanical fallback catches a wrongly advertised one.

**Subagent role state (2026-01-03).** A parented sub-agent's `Workflow` loads
the main plan's stack from the shared plans dir (the read-only mirror that
feeds `current_plan` for reviewers and the UI staircase) but is stamped
`WorkflowState::Subagent` by the spawn path — a role state like `Skill`, not
a lifecycle phase: it never claims Executing/Reviewing, never transitions,
and `workflow_expects_progress` is false for it (auto-continue parks; the
`is_subagent` guard in `run_turn_with_retry` is belt-and-braces). Tool
visibility comes solely from the spawn-time allow-list
(`ToolFilter::from_state` returns `None` for Subagent — reaching the
state-derived filter without an allow-list fails loudly). Model resolution
for sub-agents is `[models.subagent]` → default: the lifecycle-state tier
deliberately does not apply (pre-2026-01-03 a sub-agent spawned mid-Executing
silently ran `models.executing` via the mirrored state).

**Multi-agent plan ownership policy (Maint H2).** Only the main agent (the
parentless agent with the smallest runtime id, as returned by
`AgentManager::main_agent_id()`) may call plan-mutation tools:
`create_plan`, `update_plan`, `complete_step`, `abandon_plan`. Sub-agents
(spawned via the `spawn_agent` tool, which record a `parent_id`) have their
per-agent `Workflow` and `AgentLoop` constructed with `plan_mutations_allowed =
false`. The restriction is enforced in:
- `Workflow::allowed_tools()` (subs carry a spawn-time allow-list; the
  Subagent state never consults a lifecycle filter — allow-list-less is a
  loud invariant failure)
- `Tool` wrappers for the four plan tools (early error)
- `AgentLoop::execute_tool_call` (dispatch gate, after state `ToolFilter`)
- schema construction in `run_turn` (the LLM is not offered the tools)

This keeps a single authoritative plan for the user's task while still
allowing sub-agents for parallel read-oriented work (reviews, investigation).
Per-agent plan namespaces were considered but rejected for this phase due to
higher complexity (sidecar files, UX "which plan am I looking at?").

The factory continues to give every agent an independent `Workflow` (the
independence tests remain valid); the ownership policy simply denies the
mutation entry points for children.

```
                         ┌──────────────────────────────┐
                         │        AgentManager          │
                         │  spawn · route · fan-in      │
                         └──────────────────────────────┘
                  AgentCommand │            ▲ AgentEvent
              (UI → agent)     │            │ (agent → UI, tagged w/ AgentId)
                              ▼            │
  ┌─────────────┐    ┌──────────────────┐    ┌─────────────┐
  │             │    │     Agent 0      │    │             │
  │  Tauri      │    │  (tokio task)    │    │  Agent 1    │
  │  webview    │    │  own inbox +     │    │  (tokio task)│
  │  (frontend) │    │  outbox sender   │    │             │
  │             │    └──────────────────┘    └─────────────┘
  └─────────────┘
       ▲
       │ Tauri IPC (commands + events)
       │
  ┌─────────────┐
  │  Rust IPC   │  ← bridges AgentCommand/AgentEvent to Tauri commands/events
  │  adapter    │
  └─────────────┘
```

### Channel contract

**UI → Agent** (`AgentCommand`, via the agent's inbox):
```rust
enum AgentCommand {
    Prompt(String),            // start a new turn
    Suggestion(String),        // mid-work guidance — queued, injected at next turn boundary
    Interrupt,                 // cancel the current LLM generation
    Cancel,                    // stop the agent entirely
}
```

**Agent → UI** (`AgentEvent`, fanned-in through the manager, tagged with `AgentId`):
```rust
enum AgentEvent {
    Started,
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallStart { index: u32, id: String, name: String },
    ToolCallArgDelta { index: u32, fragment: String },
    ApprovalRequest {
        tool_call_id: String,
        tool_name: String,
        args: Value,
        preview: Option<ApprovalPreview>,
        responder: oneshot::Sender<Approval>,   // ← NOT serializable; held in Rust
    },
    ToolResult { tool_call_id: String, result: ToolResult },
    WorkflowStateChanged {
        state: WorkflowState,
        top_plan_id: Option<String>,  // root plan id; changes on a new top-level plan
    },
    StepCompleted { step_index: u32 }, // 1-indexed step number (1 = first step)
    Finished { reason: FinishReason },
    Error { error: String },
    // --- beyond the original spec (shipped) ---
    Usage { .. },                 // per-request token usage
    ContextUsage { .. },          // live context-window fill
    SuggestionInjected { .. },    // a queued Suggestion was injected
    Exited,                       // agent task exited
    ChildFinished { .. },         // a spawned sub-agent completed
}
```

### Tauri IPC bridge

The frontend talks to the brain via Tauri commands (frontend → Rust) and Tauri
events (Rust → frontend). A thin adapter in `src/ipc/` bridges the channel
contract to the IPC boundary:

**Frontend → Rust (Tauri commands):**
- `send_prompt(agent_id, text)` → `AgentCommand::Prompt`
- `send_suggestion(agent_id, text)` → `AgentCommand::Suggestion`
- `cancel_suggestion(agent_id, text)` → `AgentCommand::CancelSuggestion` (the "x" on a pending steer — drops it before injection)
- `interrupt(agent_id)` → `AgentCommand::Interrupt`
- `cancel(agent_id)` → `AgentCommand::Cancel`
- `approve(tool_call_id, approval)` → resolves the pending `oneshot::Sender`
- `spawn_agent(name)` → `manager.spawn()`
- `list_agents()` → `manager.list()`
- `get_plan()` / `get_workflow_state()` → reads workflow
- `save_conversation(path)` / `load_conversation(path)`
- `read_file(path)` / `list_files(dir)` → for the file browser + md viewer
- `get_config()` → global config (minus keys)

**Shipped beyond this list:** safety-mode get/set + safety-rules CRUD,
`set_model`, stats (`get_session_stats` / `get_project_stats` /
`get_session_list`), `get_git_branch`, `list_markdown_files`, and the whole
backlog subsystem (`backlog_*`, `run_all` / `stop_all`).

**Rust → Frontend (Tauri events):**
The adapter subscribes to `manager.next_event()` and emits Tauri events. The
`ApprovalRequest` is special: the adapter holds the `oneshot::Sender` in a
pending-approvals map keyed by `tool_call_id`, and emits a *serializable*
`ApprovalRequest` event (without the sender). When the frontend calls
`approve(tool_call_id, approval)`, the adapter looks up the sender and resolves
it. All other events are serialized directly (requires adding `Serialize` to
`AgentEvent`, `ToolResult`, `ApprovalPreview`, `FinishReason`, `WorkflowState`).

### UI layout (the modern GUI)

A native window with a clean, modern layout. Dark theme by default (light
toggle). The layout mirrors the original design but with web-grade rendering:

```
┌──┬──────────────────────────────────┬──────────────────┐
│  │ [agent: main ●] [agent: tests]   │ [md] [plan] [diff]│
│  ├──────────────────────────────────┼──────────────────┤
│  │  user: add a login page          │ # Auth Design    │
│  │                                  │                  │
│  │  assistant: I'll start by        │ We need a login  │
│ S│  reading the existing routes...  │ flow that...     │
│ i│  ▸ file_read src/routes.rs       │                  │
│ d│                                  │ ## Endpoints     │
│ e│  assistant: Here's my plan...    │ - POST /login    │
│ b│  I'll add a /login route...      │ - POST /logout   │
│ a│                                  │                  │
│ r│  ▸ file_edit src/routes.rs       │                  │
│  │    [Approve] [Deny] [Diff]       │                  │
│  ├──────────────────────────────────┴──────────────────┤
│  │  > type your message here_                           │
│  ├──────────────────────────────────────────────────────┤
│  │ gpt-4o · openai │ Executing 2/5 │ 1.2k tok │ main   │
└──┴──────────────────────────────────────────────────────┘
```

**Regions:**
| Region | Behavior |
|---|---|
| **Sidebar** (left) | Agent switcher (one entry per spawned agent, with status dot), quick actions, navigation. Collapsible. |
| **Main panel** (tabbed) | One tab per agent. Each tab shows that agent's conversation: streaming markdown text, user messages, tool-call cards, inline approval prompts with diff preview. Tab bar shows name + model + status (`●` running / ` ` idle / `!` error) in the same flat-rail style as the right-panel tools (fixed h-10 triggers, cyan bottom border on the active tab). |
| **Right panel** (tabbed, on-demand) | Tabbed container of views: md viewer, plan progress, diff viewer, tool output, file browser. Hidden by default; toggled or auto-shown when an approval diff is pending. |
| **Input** | Multi-line textarea with history ring buffer. Slash-command aware. Input is scoped to the active main-panel tab. |
| **Status bar** | `model · provider │ workflow-state step-progress │ token-usage │ git-branch`. |

**Right-panel views:**
| View | What it shows |
|---|---|
| **Md viewer** | Renders a markdown file readably — headings, lists, code blocks (Shiki-highlighted), wrapped prose, tables, task lists. Scrollable. Editable (toggle edit mode). |
| **Plan progress** | The current plan with checkboxes, progress count, active step highlighted. Reflects `.coding/plans/` on disk. |
| **Diff viewer** | Syntax-highlighted unified or side-by-side diff for `file_edit` / `file_write` / `multi_edit` approvals (`multi_edit` shows one combined diff covering every file it changes). Auto-shown when an approval is pending. |
| **Tool output** | Live output from shell commands and search results. |
| **File browser** | Navigable project file tree; opening a file switches to the md viewer. |

### Module layout

```
mnemo/
├── Cargo.toml                    # workspace: lib + tauri app
├── src/                          # the brain (UNCHANGED from phases 1-7)
│   ├── lib.rs
│   ├── error.rs
│   ├── config/
│   ├── project/
│   ├── provider/
│   ├── tool/
│   ├── memory/
│   ├── workflow/
│   ├── runtime/
│   ├── agent/
│   ├── mcp/                      # MCP client (stdio + streamable-HTTP) + lazy manager + tool adapters
│   └── ipc/                      # NEW — Tauri IPC bridge
│       ├── mod.rs                # adapter: AgentEvent → Tauri event, command → AgentCommand
│       ├── commands.rs           # #[tauri::command] functions (frontend → Rust)
│       ├── events.rs             # serializable event types (Rust → frontend)
│       └── approval.rs          # pending-approvals map (holds oneshot senders)
│
├── src-tauri/                    # NEW — the Tauri app shell
│   ├── Cargo.toml                # tauri deps, depends on mnemo lib
│   ├── tauri.conf.json          # window config, bundle id, frontend dist path
│   ├── build.rs
│   └── src/
│       └── main.rs              # tauri::Builder, registers commands, spawns the brain
│
└── frontend/                    # NEW — the web frontend
    ├── package.json             # React, Vite, Tailwind, shadcn/ui, react-markdown, Shiki
    ├── vite.config.ts
    ├── tsconfig.json
    ├── tailwind.config.ts
    ├── index.html
    └── src/
        ├── main.tsx             # React entry
        ├── App.tsx              # top-level layout
        ├── lib/
        │   ├── tauri.ts         # typed wrappers around invoke() + listen()
        │   └── types.ts         # TS types mirroring the Rust IPC types
        ├── components/
        │   ├── layout/          # Sidebar, MainPanel, RightPanel, StatusBar, InputBar
        │   ├── chat/            # Conversation, Message, ToolCallCard, ApprovalPrompt
        │   ├── views/          # MdViewer, PlanProgress, DiffViewer, FileBrowser
        │   └── ui/             # shadcn/ui components (button, tabs, dialog, scroll-area)
        ├── hooks/
        │   ├── useAgentEvents.ts  # subscribe to Tauri events, update app state
        │   └── useAgent.ts        # send commands, manage active agent
        └── styles/
            └── globals.css       # Tailwind + theme tokens
```

### MCP servers (Model Context Protocol)

Third-party tool servers configured in `~/.mnemo/mcp.toml` (`[[server]]` entries; stdio `command`+`args` or remote `url`+`headers_env`; env-var NAMES only — values resolve from the environment at connect time, so the file is safe to log/show). `src/mcp/` implements a minimal in-crate JSON-RPC client (initialize / tools/list / tools/call — the `rmcp` SDK was evaluated 2026-02-13 and rejected to avoid its dependency tree for three calls) over both transports, plus an `McpManager` that is inert until first use: connections are lazy, cached per server, and re-established transparently after failures. Each enabled server becomes one `mcp.<server>` deferred tool group (`deferred_groups_with` extends the static browser/image table); `load_tools` reveals it by connecting, listing, and materializing `McpTool` adapters into the registry's shared dynamic slot (`ToolSlot` — the same shared-handle pattern as `LoadedGroups`, so tools can join a registry that was already built). Agent-facing names are namespaced `mcp__<server>__<tool>`; every MCP tool is Agent-category + `NeedsApproval` (visible in Executing/Reviewing, hidden in Planning/Complete by the safety gate and in research plans by name prefix, never in the reviewer allow-list). Built-ins are untouched — browser/image stay native; only the deferred-group table is shared. Settings → MCP edits the set (validate → atomic write → live manager swap; applies to agents built afterwards) with a Test button (connect + list tools).

**Per-project overrides**: the global set is union-merged with an optional `.coding/mcp.toml` (`merge_servers` — project wins by name, non-colliding globals kept; the brain builds the manager from the merged set at startup). The Settings list tags each server with its source ("global"/"project" — derived from the project file), and a save SPLITS by source back into the two files (each validated alone and as the merged view; the in-memory Config keeps the global subset so `save_all` never clobbers the project file).

**Prompts, resources, sampling**: a server's advertised capabilities (from `initialize`) gate per-server meta tools — `mcp__<server>__get_prompt` (prompts/get) and `mcp__<server>__read_resource` (resources/read) are registered at reveal with the reveal-time listing (names/URIs, capped at 20) embedded in their descriptions (a snapshot; the underlying RPC methods are always live). Server→client REQUESTS are answered over stdio by a shared-writer reader task with a standard `-32601` decline — `sampling/createMessage` is deliberately declined rather than left hanging the server (full sampling needs a provider handle in the manager; future work). The HTTP transport opens no server→client stream, so pushed requests cannot arrive there at all.

**OAuth 2.1 (remote servers)**: `auth = "oauth"` + a pre-registered `client_id` enables the authorization-code flow with PKCE S256 (`src/mcp/oauth.rs` — `sha2` + the existing `base64`/`uuid`; no other new deps). Settings → Connect: RFC 8414 discovery → loopback redirect listener (port bound before the authorize URL is returned) → the frontend opens the URL → the redirect resolves in a background task → tokens (access + refresh + expiry) persist as a JSON blob in `keys.toml` under a sanitized `mcp-<name>` key. Remote requests inject `Authorization: Bearer` per call and refresh automatically on 401 (retry once). Limitations: no dynamic client registration (client_id is hand-configured); the browser/exchange legs are exercised manually via Settings (unit tests cover PKCE, URL/body building, state validation, and token-store round-trips).

**Per-server trust (auto-approve)**: `trusted = true` on a server def (Settings → MCP checkbox) stamps its materialized `McpTool` adapters so `safety()` returns `AutoRun` instead of `NeedsApproval` — the trusted server's tools run **without the per-call approval prompt** (still subject to the user's safety mode + safety rules; default deny preserved for untrusted servers). Trust relaxes ONLY the approval gate, never the plan-first state visibility: the Planning + Complete `ToolFilter` arms exclude `mcp__`-prefixed names from the AutoRun visibility criterion, so a trusted MCP tool stays hidden in read-only states exactly like an untrusted one (research arms already exclude by name prefix). Implementation lives in `safety()` rather than auto-injecting `safety_rules`: rules are user-owned state persisted in `safety.toml`, and a config file auto-managing another would create two sources of truth.

**Connection health UX**: the manager tracks per-server status (`connected`, `tool_count`, `last_error` — cleared on the next success — and `restarts`) in a side map maintained by `ensure`/`call_with_info`; the optional `idle_timeout_secs` def knob enforces a LAZY idle expiry (a cached connection idle past the timeout is dropped inside the next `ensure` and re-established — no background task, counted as a restart). `call_with_info` reports whether the call had to re-establish the connection, and the tool adapter prefixes the result with a "server restarted after a crash" note (the agent-window surface) when it did. The App exposes `mcp_status` (a pure read, never connects) and Settings → MCP renders one status line per card — connected · N tools / error / not connected this session, plus a restarts badge — refreshed on activate, after Test/Save/Connect, and via a manual refresh button.

## Build phases (UI swap — brain is done)

### Phase A — Tauri scaffold + IPC bridge *(no frontend yet)*
- Convert to a Cargo workspace: `mnemo` lib (the brain, unchanged) +
  `src-tauri` binary.
- Add `src/ipc/`: the adapter that bridges `AgentCommand`/`AgentEvent` to Tauri
  commands/events.
- Add `Serialize` to the types that cross the boundary: `AgentEvent` (minus the
  `oneshot::Sender` — split it out), `ToolResult` (already has it), `Approval`,
  `ApprovalPreview`, `FinishReason`, `WorkflowState`, `ToolCall`.
- The approval flow: `ipc/approval.rs` holds a `HashMap<String,
  oneshot::Sender<Approval>>` keyed by `tool_call_id`. The adapter emits a
  serializable `ApprovalRequest` event; the `approve` command resolves the
  sender.
- `src-tauri/src/main.rs`: `tauri::Builder`, registers all commands, spawns the
  brain (AgentManager + first agent) on the tokio runtime, wires the event
  forwarder.
- **Exit criteria:** `cargo tauri dev` opens a blank window; a headless test
  sends a prompt via a Tauri command and receives a `TextDelta` event back
  (using a mock provider); the approval round-trip works (command resolves the
  oneshot).

### Phase B — Frontend foundation + chat
- Scaffold the React + TypeScript + Vite + Tailwind + shadcn/ui frontend in
  `frontend/`.
- `lib/tauri.ts`: typed wrappers around `invoke()` (commands) and `listen()`
  (events). `lib/types.ts`: TS types mirroring the Rust IPC types.
- `hooks/useAgentEvents.ts`: subscribe to `agent://event`, update a Zustand
  store.
- `components/layout/`: Sidebar (agent switcher), MainPanel (tabbed), InputBar
  (textarea + history + slash commands), StatusBar.
- `components/chat/`: Conversation (scrollable transcript), Message (user /
  assistant / tool / error), streaming text.
- **Exit criteria:** the window shows the layout; typing a prompt + Enter sends
  it to the agent; streaming assistant text appears live; the status bar shows
  model/provider/workflow.

### Phase C — Rich rendering (markdown + code + diffs)
- `components/chat/Message`: render assistant messages as markdown via
  `react-markdown` + `remark-gfm` + `rehype-highlight` (or Shiki for
  VS Code-quality highlighting). Code blocks get copy buttons + language labels.
- `components/chat/ToolCallCard`: a card showing the tool name, args (JSON,
  syntax-highlighted), and result (collapsible).
- `components/chat/ApprovalPrompt`: inline approve/deny buttons; for file edits,
  auto-opens the diff viewer in the right panel.
- `components/views/DiffViewer`: `react-diff-view` + `refractor` for
  syntax-highlighted unified + side-by-side diffs.
- **Exit criteria:** assistant markdown renders with headings, lists, tables,
  code blocks (highlighted); file-edit approvals show a proper diff; tool calls
  render as cards with collapsible results.

### Phase D — Right-panel views + plan progress
- `components/views/MdViewer`: render a markdown file (Shiki-highlighted code),
  scrollable, edit-mode toggle.
- `components/views/PlanProgress`: the current plan with checkboxes, progress
  count, active step highlighted. Reads via `get_plan` command.
- `components/views/FileBrowser`: navigable file tree; opening a file loads it
  into the MdViewer.
- Right panel: tabbed, on-demand, auto-shows on approval.
- **Exit criteria:** all four views render; the right panel toggles and hosts
  them; opening a file from the browser shows it in the md viewer.

### Phase E — Polish + slash commands + theming
- Slash commands: `/clear`, `/model`, `/provider`, `/help`, `/save`, `/load`,
  `/panel`.
- Theming: dark (default) + light toggle, persisted. Modern, polished styling
  via Tailwind + shadcn/ui.
- Conversation save/resume to disk (via commands).
- Streaming markdown re-parse (cheap incremental — only re-render the streaming
  tail).
- Redraw throttle (React handles this naturally; ensure no excessive re-renders
  on streaming deltas).
- Provider error retry/backoff (already in the brain; surface errors gracefully
  in the UI).
- **Exit criteria:** full interactive loop works end-to-end with a real model;
  slash commands work; theme toggles and persists; conversation saves/loads.

## Dependencies (new, for the Tauri GUI)

```toml
# src-tauri/Cargo.toml
[dependencies]
mnemo = { path = ".." }           # the brain lib
tauri = { version = "2", features = ["protocol-asset"] }
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

```json
// frontend/package.json (key deps)
{
  "react": "^18",
  "react-dom": "^18",
  "@tauri-apps/api": "^2",
  "react-markdown": "^9",
  "remark-gfm": "^4",
  "rehype-highlight": "^7",
  "shiki": "^1",
  "react-diff-view": "^3",
  "refractor": "^4",
  "zustand": "^4",
  "lucide-react": "^0.4",
  "tailwindcss": "^4",
  "@radix-ui/*": "...",
  "vite": "^5",
  "typescript": "^5"
}
```

## How we'll work

- One phase at a time, review at each exit criterion before moving on.
- The brain (phases 1–7) is DONE and unchanged — we only add `Serialize` derives
  and the `ipc/` adapter.
- Comprehensive tests at every phase (the brain's 184 tests stay green
  throughout).
- Code review after every phase (adversarial; correctness, Rust idioms,
  security, test coverage, plan alignment).
- Modern-looking UI is a first-class requirement — shadcn/ui + Tailwind + proper
  typography, spacing, and dark theme.

## Beyond the original spec (shipped features)

These capabilities are implemented and shipped but were not in the original
PLAN.md scope. They are documented here so the PRD reflects the shipped
product.

- **Opt-in Laya classifier foundation** (`src/memory/classifier.rs`, mirrored
  `build_classifier` in `src/provider/client_factory.rs`) — a `Classifier`
  trait beside `Embedder` plus a Laya HTTP backend (`POST /v1/systemone`) for
  fast, calibrated "System 1" decisions. Disabled by default: with Laya off (or
  enabled without an endpoint) no client is built and no call is ever made, so
  the app behaves exactly as before. Status surfaces via
  `get_classifier_status` + the startup snapshot, and Settings → Classifier
  carries the toggle, endpoint URL, install hints, and live status (2027-01-16,
  backlog bb54bdcc; the consumer features — memory typing (shipped), model
  routing, tool steering, failure triage — each confidence-gated).
- **Managed Laya runtime** (`src-tauri/src/ipc/laya.rs`) — embedding-parity
  UX for the classifier: Settings → Classifier downloads a self-contained
  runtime (uv binary + virtualenv + `laya[serve]` + checkpoint, ~0.8–1 GB
  plus the checkpoint, under the app config dir) with live progress, and
  while managed mode is enabled the app automatically starts, monitors, and
  stops the `laya-serve` sidecar on 127.0.0.1 (startup hook + save-driven
  rewire + app-exit stop). No command line, no Python prerequisites;
  external mode (user-run endpoint) is preserved.
- **Laya memory auto-typing** (`src/memory/auto_typing.rs`) — the
  classifier's first consumer: at `memory_write` time a choice question
  over the six typed prefixes may correct the writer's prefix when the
  calibrated confidence clears 0.80; a low-confidence or missing answer
  keeps the writer's prefix, and with the separate `auto_type_memories`
  opt-in off (the default) nothing changes at all. The tool reads the
  shared classifier slot + a flag mirror at call time (Settings toggles
  are live without a rebuild), and every correction or keep is noted in
  the write's message + data. Base checkpoints are near-chance on this
  task — enable it only against a checkpoint fine-tuned on the seed
  labeled set `seed_dataset` builds from `.coding/knowledge/` (covers
  SPEC/DECISION/BUG/HOW; PLAN/REVIEW ride untrained until those corpora
  can seed them — follow-up) (backlog a147b63c).
- **Laya failure triage + startup fine-tune** (`src/agent/failure_triage.rs`,
  `src-tauri/src/ipc/finetune.rs`) — the classifier's second consumer: at
  every failure-handling site (the tool-execution cap + bad-JSON repair loop
  in `src/agent/turn.rs` and BOTH provider retry layers — the inner
  `complete_with_retry` and the outer `run_turn_attempt`) the error text is
  classified (transient / permanent / needs_user / flaky_test,
  `TRIAGE_THRESHOLD` 0.80 inclusive) and a confident answer lets the harness
  act: a transient READ-ONLY tool failure is auto-retried without a model
  roundtrip (≤1 per call, ≤4 per turn, never mutating tools), other classes
  ride targeted guidance on the fed-back error, and a confident
  needs-user/permanent provider error skips both retry ladders immediately
  (a marked error text makes the outer layer skip without re-classifying; a
  429 is never classified). Every classified failure is logged with its true
  disposition to a JSONL training log; the startup fine-tune (managed mode +
  `auto_finetune`) re-trains the checkpoint from that log when ≥50 new
  labeled rows accrued and hot-swaps the served checkpoint (an ineligible
  run exports the dataset + skips cleanly — laya 0.3.20 ships no training
  surface). Both flags are separate opt-ins (default off); disabled behavior
  is byte-identical (backlog 1a4049c1).
- **Vision fallback** — a `VisionClient` plus a `describe_image` agent tool,
  with an image-attachment fallback path: when the active main model resolves
  to multimodal = false (`Capabilities.multimodal`, resolved per model — the
  endpoint's `multimodal` flag, overridable per model via `ModelSpec.multimodal`
  so one Ollama endpoint can host text-only GLM and vision-capable GLM flash
  side by side), attached images are routed through the vision model and the
  description is fed back to the main model (`src/provider/vision.rs`,
  `src/tool/agent/image_tools/`). Attachments are capped at a 1568 px long
  edge (JPEG) on paste before they are sent — bounding both the model payload
  and the transcript's retained image bytes.
- **`spawn_agent` tool** — the agent can spawn background sub-agents via
  `AgentSpawner` (`src/tool/agent/spawn_agent.rs`); a `ChildFinished` event
  notifies the parent loop on completion (parent-aware spawning).
- **Failed-agent Continue button** — when an agent's turn ends in a FINAL
  error, the InputBar shows a Continue button (the task loop survives final
  errors) that resumes the failed task with a "Continue from where you left
  off." prompt; a `failed` per-agent flag in the frontend store drives it.
- **Failed-reviewer protocol** — a reviewer failure without a report is never
  assumed done: the parent gets distinct failure text instructing `ask_user`
  (retry on another model / abandon the review — the main agent can never
  self-review, since `write_review_report` is reviewer-only and
  `.coding/reviews/` is sandbox-protected), and a blind reviewer respawn is
  dispatch-denied while the failure is pending (`reviewer_failure_pending`
  flag on the parent loop). Reviewer spawns must also OMIT the `model` arg
  (backlog c8e48f81): `reviewer_spawn_gate` (dispatch) denies a reviewer
  spawn carrying an explicit model unless the failed-reviewer retry is
  sanctioned — the `ask_user` interception opens a one-spawn
  `reviewer_retry_sanctioned` when a failure was pending; it is consumed by
  any reviewer spawn and closed by `abandon_plan` or an unrelated `ask_user`.
  The configured reviewing model is authoritative. `abandon_plan` is
  available in the Reviewing state as the escape hatch for an un-completable
  review (returns to Planning — the main agent is never stuck in Reviewing).
- **Extra workflow tools + sub-plan stack** — beyond `create_plan` /
  `complete_step`, the shipped workflow tools include `file_append`,
  `update_plan`, and `abandon_plan`. `create_plan` while already executing
  pushes a **sub-plan** onto a stack persisted in a `stack.json` sidecar, so
  arbitrarily nested plans resume correctly from disk.
- **Backlog subsystem + overnight Run-All loop** — `src-tauri/src/ipc/backlog.rs`
  + `run_all.rs`: add / list / remove / reorder / clear / retry / auto-feed /
  dispatch / run-all / stop-all. Run-All works the backlog unattended, taking a
  **git checkpoint before each item** (on the per-directory wt/* branch —
  forked/reused first, never main; a refused fork stops the run) and
  committing on success. Item status
  is tied to the plan lifecycle (backlog 45dcf577): `Done` only via the
  plan-loop gate (the plan finished), `Failed` only on root-plan
   abandonment, and every other turn end (loop open, crash,
   halt-for-approval) leaves the item untouched — no rollback, the run
   waits (the next turn resolution re-checks); an approval halt stops it
   for the user; a resumed session continues the plan. A user steer or
   interrupt requeues the in-flight item non-terminally; items are stamped
   `InFlight` (and linked to the root plan id) only when execution begins
   (workflow enters `Executing`).
- **Safety-rules system** — regex auto-approve rules in `.coding/safety.toml`
  (`src/safety_rules.rs`, mtime-checked), a Safety tab editor, a "Mark Safe"
  action on approvals, and a global 4-mode `SafetyMode` toggle
  (including `Autonomous`). This is a superset of the original
  "approve-each-action gate."
- **Stats / pricing** — `RequestStats` recorded to `memory.db`
  (`agent/turn.rs`), `get_session_stats` / `get_project_stats` /
  `get_session_list` commands, a Stats view, and per-model pricing in
  `endpoints.toml`. Rows record the serving endpoint (`provider_name` at
  the Usage event; NULL for pre-endpoint rows, added to legacy DBs by
  schema migration), and both breakdowns group by (model, endpoint) — the
  Stats view's per-model table shows an Endpoint column, so the same model
  id served by two endpoints renders two rows with their own tok/s.
- **Per-model endpoint config** — models inside an endpoint can be plain ids
  (`models = ["gpt-4o"]`) or per-model tables (`[[endpoint.models]]`) with
  their own `max_context`, `max_output_tokens`, `reasoning_effort` default,
  and `reasoning_efforts` allow-list (`src/config/endpoints.rs` `ModelSpec`;
  legacy string arrays still parse). Per-model caps override the endpoint
  level in the provider capability set (`client_factory`); the per-model
  `reasoning_effort` (the effort dropdown on the per-model strip in Settings →
  Providers) wins over the endpoint's value in
  `effective_reasoning_effort_for`, so every consumer — client_factory,
  `set_model`/console effort resolution, the status-bar dropdown's initial
  selection — picks it up; the reasoning-effort dropdown clamps into the
  model's list. The Settings → Providers card shows a per-model strip under
  each model row; base_url is forgiving (missing trailing `/`
  auto-appended on save and load).
- **Per-context reasoning-effort overrides** — `[models]` slots (`ModelRef`,
  `src/config/general.rs`) carry an optional `reasoning_effort` (Settings →
  Models, one effort dropdown per context row; hidden for anthropic hosts,
  disabled when the endpoint rejects effort). The slots span the workflow
  states plus the `bug_fixing` plan-kind override (active while the active
  plan's kind is `bug_fixing` in Executing/Reviewing — wins over `executing`)
  and the subagent role. `build_provider_for` resolves
  it first (`resolve_effort`): a set context effort wins over the model's
  own default chain, normalized through the same supports gate / allow-list
  clamp / "off" wire encoding as configured values
  (`Endpoint::normalize_reasoning_effort_for`, extracted from
  `effective_reasoning_effort_for`). The resolver's provider cache keys on
  `(endpoint, model, effort)` so two contexts sharing an endpoint+model
  with different efforts build distinct providers; the wire carries the
  field both ways (`ModelRefDto` save-side, `ModelRefWire` load-side). A
  429-sticky reroute rebuilds without the context effort (inherits the
  model's own default).
- **Conversation transcript windowing** — the chat transcript renders only
  the last TRANSCRIPT_WINDOW (200) visible entries (`windowEntries` +
  `TRANSCRIPT_WINDOW` in Conversation.tsx), with a "Show earlier messages
  (N hidden)" expander above the list growing the window by 200 per click.
  Chosen over virtualization (no react-window/virtuoso dependency):
  windowing bounds both the DOM size and the per-flush reconciliation pass
  with zero deps, and virtualization stays deferred pending profiling
  evidence it's needed. The store's MAX_TRANSCRIPT_ENTRIES (1000) cap still
  bounds memory; the streaming block lives in the last turn's group, so the
  live stream is never windowed away, and the auto-scroll effect is
  untouched (windowSize deliberately not in its deps — expanding doesn't
  yank the user to the bottom).
- **Richer `AgentEvent` variants** — `Usage`, `ContextUsage`,
  `SuggestionInjected`, `Exited`, and `ChildFinished` extend the enum shown in
  the Channel contract.
- **Search/memory findings round (F1–F13, 2026-09-17)** — targeted knowledge
  reindex resolves `supersedes` against the on-disk corpus (predecessor
  context + conservative metadata reconciliation, so `memory_update` on a
  successor file no longer errors with "supersedes 'slug' not found");
  `memory_supersede` rewrites stale hand-written `File:` pointer lines in the
  successor body to the canonical path; `search`/`search_read` globs gain
  brace expansion (`**/*.{ts,tsx}` — the glob crate has no native
  alternation), a distinct "glob matched no files" hint, and a filename
  fallback on zero content hits; `current_plan` reports the active plan's
  on-disk file; FTS index hits are freshness-checked (on-disk mtime vs
  as-of-index) so the edit→watcher gap repairs a small staleness inline (≤ 8
  stale files re-indexed and the query re-served from the fresh index with a
  "reindexed N stale file(s)" note) and serves the walk with a staleness note
  only above that cap or while an index pass runs;
  `create_plan`'s RECALLED CONTEXT rider title-term-boosts on-point rows;
  uuid-shaped search patterns earn a fired-only known-memory-hit note; a
  session crossing 15 working-memory events gets a once-per-session
  consolidation-due nudge (`src/tool/agent/search.rs`, `src/tool/steering.rs`,
  `src/agent/dispatch.rs`, `src/memory/indexer.rs`, `src/agent/
  steering_stats.rs`).
- **Enforcement ladder (C1–C5)** — advisory escalation for the 2026-09-17
  tally's own waste classes; C5 adds a graduated gate for the symbol-lookup
  case (2026-12-04): alternation-shaped symbol-hunt regexes
  (`fn .*watcher|struct.*Watcher`) absorb the graph lookup — every branch
  must reduce to an identifier, then resolves loosely via
  `Store::symbol_id_fuzzy` (exact → ci → shortest substring, one SELECT) and
  lands in the note as `graph_context(id=…)` entries; a zero-hit
  `literal:true` search over metachars carries a `literal:false` retry hint;
  an actionable steering marker firing 2× for one agent with zero switches
  appends a one-shot escalation note at the dispatch funnel (per-agent
  fired/switched counters in `SteeringStats`); shell null-sink redirections
  carry a warning counted as the fired-only `shell-redirect` marker;
  **C5 (2026-12-04):** the SearchNudge escalation now has teeth — after 2
  ignored fires with zero switches, the next `search`/`search_read` call
  whose pattern names an indexed symbol is *intercepted* at the dispatch
  funnel (not just nudged): the search does not run, and a redirect result
  embedding the symbol id is returned instead. The gate lifts the moment the
  agent switches to a graph tool once (`agent_switched > 0`). Justified by
  the 2026-12-04 run-all/steer investigation (6 symbol lookups via `search`,
  nudge ignored). (`src/tool/agent/search.rs`, `src/codegraph/store.rs`,
  `src/agent/steering_stats.rs`, `src/agent/dispatch.rs`,
  `src/tool/agent/shell.rs`, `src/agent/loop_impl.rs`).
