# Mnemo2 LLM Trace — design

**Plan:** c3ba2b58, step 1

**Status:** proposed for lead review; no production code is included

**Audience:** Mnemo2 lead review, then implementation workers

## Sources and existing behavior

This design is based on Mnemo1's `LlmTraceView.tsx`, `TraceStats.tsx`, the 2026-08-13 trace review, `src/provider/trace.rs`, and the ratified phase-attribution spec. It also accounts for Mnemo2's current `TraceTab.svelte`, `trace.ts`, `trace_view.rs`, Cost view, pricing/usage ledger, routes, and preview seams.

Important existing constraints:

- One engine records the lead and all workers into one `LlmRequestLog`, but records currently have no actor, session, turn, or plan attribution.
- The live ring holds 32 records. Request bodies are capped at 256 KiB by default; raw responses at 2 MiB each and 16 MiB across the ring. Truncation and eviction are explicit record state, not silent absence.
- `version`, `is_complete`, live usage estimates, cancellation, guard cuts, errors, and HTTP status already support a useful live view. Preserve them.
- Full-body file logging is opt-in, except that failed requests are currently always mirrored with secrets redacted. Cancellation metadata is always persisted. Opt-in terminal history rotates at 64 MiB and keeps four archives.
- Cost uses durable ledger usage and shared configured/vendor-default pricing. Trace must reuse that pricing rule, not invent a second one.

## Recommendation in one paragraph

Add a full top-level **Trace** route for investigation and keep a smaller **Trace** Workbench tab for immediate context. Both use the same transport and view models. Make attribution request-scoped and explicit in the kernel, so concurrent agents sharing a provider cannot be mislabeled. Show a calm chronological call list, a readable detail view, and one collapsible chart panel. Persist a bounded metadata-only journal by default for restart-safe analysis; keep request/response bodies off disk unless explicitly enabled. Export selected calls as JSONL.

## 1. Placement: full page and compact tab

### Full Trace route

Add `"trace"` to `Route`, a TopBar item between **Cost** and **Graph**, and an `Activity` icon. This is the primary trace experience because attribution filters, a readable message wall, timing, charts, and export do not fit responsibly in the side panel.

```text
Lead | Team | Cost | Trace | Graph | Plans | Memory

┌ Trace ─ Every model call in this project ────────────────────────────────┐
│ [All agents▾] [All models▾] [All statuses▾] [Time: Session▾] [Search…] │
│ [Group: Time▾] [✓ Follow new calls]                 [Export] [•••]      │
├───────────────────────────────┬──────────────────────────────────────────┤
│ 18 calls · 124.8k tokens · ≈$0.42 │ Selected call                       │
│ ┌───────────────────────────┐ │ Lead · Turn · kimi-k3.1-pro      Live  │
│ │● Lead       Turn           │ │ 14:32:08.114 · request #184            │
│ │14:32:08  92% cached  1.8s  │ │ [Overview] [Context] [Response] [Raw]  │
│ │18.2k in · 1.4k out · $0.08│ │                                          │
│ ├───────────────────────────┤ │ Friendly summary, tokens/cost, timing,   │
│ │● Worker: design-trace      │ │ then purpose-specific content.           │
│ │Plan c3ba2b58 · Step 1      │ │                                          │
│ └───────────────────────────┘ │                                          │
├───────────────────────────────┴──────────────────────────────────────────┤
│ Insights [Timing] [Tokens] [Cost]                         [Hide charts] │
└──────────────────────────────────────────────────────────────────────────┘
```

On narrow screens the list and detail become two views with a Back button; charts move below the current view. The list defaults to 420 px and is resizable on wide screens, but there is no always-open resizable statistics strip consuming the first screen.

### Compact Workbench Trace tab

Keep the existing tab as a focused recent-call view: the same status, actor, model, token, cache, cost, and duration fields; no charts, prefix diff, export, or history search. Show the latest 20 calls, with **All agents** / **This lead** / active worker filters. Selecting a row opens its Overview, Context, and Response. **Open full Trace** carries the selected call and filters into the full route.

This serves two different tasks: the tab answers “what is this agent doing now?”, while the page answers “why did behavior or cost change?” Duplicating only the shell is intentional; formatting, status derivation, and transport remain shared.

## 2. Attribution: explicit, request-scoped kernel data

### Record contract

Add a serializable `TraceAttribution` in `src/provider/trace.rs` and place `attribution: Option<TraceAttribution>` on `LlmRequestRecord`, `LlmRequestSummary`, and therefore detail responses. Add `run_id: String` to the record and summary as well; request ids restart with the process, so the durable key is `run_id:id`, not `id` alone.

| Field | Meaning and source |
|---|---|
| `actor_kind` | `"lead"`, `"worker"`, `"subagent"`, or `"system"`; supplied by the host spawn path. |
| `agent_id` | Runtime `AgentId` string, filled immediately after allocation in `spawn_agent_shared`. |
| `session_id` | Headless `SessionId` string. It may equal the numeric agent id today but remains a separate wire concept. |
| `agent_name` | Spawn name for display; snapshots avoid depending on a worker that may later be deleted. |
| `worker_id` | Mnemo2 TeamManager worker id when applicable; `None` for the lead and ordinary kernel agents. |
| `plan_id`, `plan_step` | Immutable dispatch context from the worker record; `plan_step` is the 1-based step number. |
| `turn_id` | The existing `model_routing::new_turn_id` value, attached at turn start. This groups several model requests and their tool phases. |
| `purpose` | `"turn"`, `"compaction"`, `"memory_consolidation"`, `"routing"`, or `"other"`; attached at the call site, never inferred from prompt text. |

Keep provider fields (`model`, `provider`, `base_url`) separate from attribution. Model fallbacks may change per request; actor identity does not.

### Stamping path

1. Extend headless `SpawnOptions` and host `SessionOptions` with optional trace identity (`actor_kind`, display name, `worker_id`, `plan_id`, `plan_step`). Mnemo2's lead spawn supplies Lead; `spawn_worker_session` supplies Worker plus the authoritative worker/plan fields. Subagents inherit their parent's worker/plan identity but get their own agent/session ids and `actor_kind=subagent`.
2. Store the identity on `AgentLoop`, just as permission and limits are per-loop. The runtime fills the allocated agent/session ids; the turn loop adds `turn_id`. Compaction and other auxiliary call sites replace only `purpose`.
3. Introduce an `LlmRequestContext` containing the attribution snapshot. Add `LlmClient::complete_with_context(..., &LlmRequestContext)` with a default implementation delegating to today's `complete`. OpenAI and Anthropic override it through a shared internal implementation; existing `complete` calls use an empty context. Agent-loop call sites use the new method.
4. Pass that context to a new `LlmRequestLog::start_with_context`; keep `start` as the empty-context compatibility wrapper. The record receives attribution at creation, before any retry, streaming, spawn-blocking, or ring eviction.

This is deliberately not a mutable label on a provider factory or a “latest record” update. Providers are shared across concurrent loops; both approaches can cross-label requests. A task-local global was also rejected because spawned blocking/task boundaries make the dependency invisible. Explicit context makes missing attribution testable and keeps concurrent requests correct.

### Mnemo1 remains unaffected

Mnemo1 keeps its route, React components, polling, and configuration UI. Existing `complete` and `start` entry points retain their behavior; unknown additive JSON fields are ignored by its TypeScript consumer. Calls without host identity render as Unattributed only in Mnemo2. No Mnemo1 workflow, record timing, pricing, or logging default changes as a side effect. The new Mnemo2 privacy setting described below is explicitly configured by Mnemo2 rather than changing Mnemo1's compatibility default.

### Required concurrency tests

Run two attributed loops over one shared provider with interleaved first chunks and completions; assert each record keeps its own actor, turn, and purpose. Cover retries, compaction, model fallback, cancellation, and a worker whose Team record is deleted after capture.

## 3. Information hierarchy for one call

### At a glance: every list row

Each row shows, without opening detail:

- actor name and kind (`Lead`, worker name, or `Subagent`), plus plan/step for a worker;
- purpose and model, with provider on hover;
- local start time and relative age on hover;
- status: Waiting, Streaming, Complete, Failed, Cancelled, or Guard cut;
- elapsed duration, input tokens, output tokens, cache reuse percentage, and estimated cost;
- a small warning only for truncation/eviction, error, cancellation, or guard cut.

Rows never show raw error text, base URLs, finish reasons, or a wall of badges. Those belong in detail. Cost is `—` when pricing is missing, never `$0.00`.

### One click: detail

```text
┌ Worker: design-trace · Turn · kimi-k3.1-pro ─────────────── Streaming ┐
│ Plan c3ba2b58 · Step 1 · Turn 178…-42 · 14:32:08.114 · #184          │
│ [Overview] [Context 31] [Response] [Tools 2] [Raw]      [Copy link]  │
├ Overview ─────────────────────────────────────────────────────────────┤
│ Estimated cost  ≈$0.0842    Total time  8.4 s (still running)         │
│ Input 18,240 · Cached 16,788 (92%) · Output 1,402 · Reasoning 611    │
│ Why this cost? Uncached input + cached input + output, using the       │
│ same model pricing as Cost.                                             │
│                                                                      │
│ Where time went                                                       │
│ Setup 0.2s → Upload 0.1s → Server wait 0.3s → Generation 5.9s        │
│              └ Retry wait 0.8s     └ Reasoning 5.1s · stalls 0.6s    │
│ Tools 0.8s (after the model response; not part of request duration)   │
│ [Timeline bar with proportional segments and numeric fallbacks]       │
│ Finish: tool_calls · HTTP 200 · Provider: zai · Endpoint host on hover│
└──────────────────────────────────────────────────────────────────────┘
```

**Overview** is the default. It contains actor/plan/turn identity, status, cost, non-double-counted token breakdown, timing, finish/HTTP details, and any truncation/eviction/guard explanation. **Context** contains readable request parameters and messages. **Response** contains Reasoning and Answer as separate cards. **Tools** contains delivered tool calls exactly as emitted, with parsed JSON when valid and raw arguments always available. **Raw** contains request and response JSON/text with Copy buttons and explicit truncation notices.

Context messages use friendly role names: System instructions, User, Assistant, Tool result. Text and assistant markdown use `Markdown.svelte`; reasoning stays plain text; tool arguments use monospace JSON. Every message is collapsed to a useful preview and expands inline. Show content arrays as separate text/image blocks rather than serialized content-part noise.

“Request settings” shows `model`, `temperature`, `max_tokens`, `top_p`, `tool_choice`, `stream`, and reasoning parameters only when present. The endpoint host is available under Advanced; credentials and headers are never displayed or exported from redacted records.

### Prefix reuse versus actual cache hits

For Context, compare the request's messages with the previous request from the **same agent, purpose, and model**, not the previous global record. Show:

`Same beginning as call #181: 41,820 / 44,103 characters (95%) · 2,283 new/changed characters`

Explain on hover: “This measures identical request text. It does not prove the provider cached those tokens.” The provider-reported **Cached input** metric remains separate. When bodies are truncated/evicted or there is no predecessor, say why comparison is unavailable. Keep raw per-message prefix badges and copy-prompt-header features out of the default view.

## 4. Call list, filters, search, grouping, and live updates

### Filters and grouping

The toolbar contains:

- **Agent:** All, Lead, each known worker/subagent, or Unattributed. Labels retain deleted workers from captured attribution.
- **Model:** every observed model, with provider in the menu label when ambiguous.
- **Status:** All, Complete, In progress, Failed, Cancelled, Guard cut. **Errors only** is a shortcut for Failed; cancellation is never classified as an error.
- **Time:** This session, Last hour, Last 24 hours, All retained, Custom range. Persisted metadata makes ranges beyond the live ring useful.
- **Purpose:** All, Conversation turn, Compaction, Memory consolidation, Routing, Other.
- **Search:** debounced text search with scope All content, Request, Response, Reasoning, Tool calls, or Metadata.

Default grouping is **Time**, newest first. **Agent → turn** groups a turn's calls in chronological order and aggregates its tokens, cost, and status. **Turn** groups all agents' calls by `turn_id`. Group headers show actor/turn, call count, duration span, tokens, and estimated cost. Do not add grouping by model in the first implementation; the model filter covers that task.

Filter state, grouping, selected call, chart visibility, and list width persist in local storage per project. A shareable `trace` route state may carry agent/time/selection, but prompt text never enters the URL.

### Search implementation

`mnemo2_trace_list` accepts structured filters and returns a page of summaries plus total/unpriced counts. Metadata filters run in Rust. Content search also runs in Rust over loaded detail/history records, case-insensitively, with a 300 ms UI debounce and a result cap; the frontend must not fetch every body to filter locally. The first version may scan retained JSONL sequentially rather than adding a database. Show “Showing first 1,000 matches; narrow the time range” when capped.

### Live behavior

Preserve Mnemo1's cheap `version` poll and skip-detail-refetch rule. Poll the full page every 1.5 s while visible, an open in-flight detail every 1 s, and the Workbench tab every 2.5 s. Suspend polling when the route/tab is hidden.

**Follow new calls** is on by default only when the selection is at the head of the list. Once the user selects an older call, keep list order and selection stable and show `3 new calls` rather than moving content under the pointer. Clicking the pill reveals them. In-flight rows animate only their status/progress, not the whole row. Final provider usage replaces estimates and is labelled Final; estimated streaming output is labelled Live estimate.

Clear live calls and delete retained history are separate destructive actions under the overflow menu. Both require confirmation; neither is a header eraser icon. Export is always non-destructive.

## 5. Charts and their relationship to Cost

Use one collapsible **Insights** panel with three tabs, driven by exactly the filtered result set. A heading always states the scope: `Charts for 18 filtered calls · this session`. Empty series explain what is missing. Legend items can hide series, and values are available in an accessible table, not only through hover.

```text
Insights   ( Timing ) ( Tokens ) ( Cost )                       Hide
┌ Where did request time go? ──────────────────────────────────────────┐
│ ms ▲                        ▁▂██▂▃                                   │
│    │ Setup ■ Upload ■ Server wait ■ Generation ■ Tools ■ Retry ■   │
│    │ Generation bars carry a darker Stall overlay, not extra width. │
│    └──────────────────────── call start time ─────────────────────▶│
└──────────────────────────────────────────────────────────────────────┘
```

**Timing:** keep Mnemo1's per-call stacked concept and error/service/cut indicators, but use friendly labels and the phase spec exactly. Setup (`prep_ms`) contains Compaction (`compact_ms`); the connect window splits into Upload (`upload_ms`, POST sent → request body fully written) and Server wait (`server_ms`, upload done → response headers — for SSE providers headers arrive with the first chunk, so this carries the server's queue + prefill); Connect (`connect_ms`, record creation → headers) and First token (`ttft_ms`) are metric chips, not bar segments; Generation (`generation_ms`) contains Reasoning (`reasoning_ms`) and Stall (`stall_ms`); Retry wait (`backoff_ms`) precedes its attempt; Tools (`tools_ms`) is after the response; `chunks` counts SSE byte-chunks read. Bar segments are non-overlapping; never present them as one additive total. Retain only the Total time column option initially; drop the column customization system unless review finds a need.

**Tokens:** keep the per-call stacked chart with four non-overlapping segments: uncached input `prompt - cached`, cached input `cached`, visible output `completion - reasoning`, and reasoning output `reasoning`. Saturate at zero for inconsistent provider reports. The total is `prompt + completion`; reasoning is never added twice. A Cache reuse summary reports `sum(cached) / sum(prompt)`, not an average of row percentages.

**Cost over time:** new. Auto-bucket by minute/hour/day depending on range, using per-call `cost_micros` from the shared pricing helper. Toggle stacking by Agent or Model. Unpriced usage appears as a hatched “Unpriced” count/token series and a note; it is never silently treated as free. Hover shows estimated dollars, calls, and tokens. Selecting a bucket applies the corresponding time filter.

The **Cost** tab remains the durable project ledger: all-time spend, per-model totals, savings levers, and 14-day trends. **Trace** explains individual captured calls and filtered behavior. Trace does not reproduce savings levers or project totals. Its header links to Cost for ledger totals; Cost may later link to Trace with a model/time filter. Small differences are explained: Trace covers retained captured calls, while Cost covers all recorded ledger usage.

## 6. Persistence, privacy, export, and limits

### Two retention tiers

**Metadata journal — on by default.** Add `.coding/logs/trace-index.jsonl`, one terminal summary per call: `run_id`, id, timestamp, attribution, model/provider, status/error class, finish reason, usage, timings, cancellation/guard/truncation flags, and pricing inputs. It contains no messages, reasoning, responses, tool arguments, raw errors, or endpoint credentials. Rotate at 8 MiB and keep four archives (about 40 MiB maximum). On startup, load paged metadata from newest to oldest and merge it with the live ring by `run_id:id`; historical rows are marked **Retained metadata** and explain when detail is unavailable.

**Full bodies — opt-in.** Reuse `traces.jsonl` and append-only `traces-history.jsonl` for exact request/response capture. The toggle says `Save full prompts and responses to disk (sensitive)` and names the project log directory. Keep the existing 64 MiB rotation/four archives, 256 KiB request cap, 2 MiB response cap, and 16 MiB live raw budget. Enabling or disabling does not change the metadata journal.

Mnemo2 should set a new `persist_failed_bodies` trace option to **false**, so its default really is no bodies on disk. Failed-call metadata and a redacted error class remain in the journal; full failed bodies are captured only when full-body logging is on. For compatibility the kernel/config default remains `true`, preserving Mnemo1's current failed-request mirroring. The lead should explicitly approve this Mnemo2 privacy tightening and its loss of default failed-prompt diagnostics.

The live ring becomes configurable (`capacity`), with Mnemo2 choosing 256 and Mnemo1 retaining 32. This increases summary retention without relaxing byte caps. Historical list pages contain 200 rows; content scans return at most 1,000 matches before asking for a narrower query.

### What survives restart

Surviving: terminal metadata, attribution, usage/timing, full bodies only when opted in and retained, UI preferences, and export files. Not surviving by default: in-flight calls after a crash, evicted/truncated content, unsaved live-only bodies, and secret request headers. Startup reconciliation marks a formerly in-flight journal row `Interrupted by restart` only if the backend can prove no terminal record exists; it must not guess a provider failure.

### Export selected calls

**Export** offers **Current call**, **Selected calls**, or **All filtered calls**. Multi-select uses checkboxes only after Export is chosen, avoiding permanent row noise. The backend streams a UTF-8 JSONL bundle to a user-selected path rather than building one large string.

Line 1 is a manifest: `format: "mnemo-trace-v1"`, `exported_at_ms`, project name (not absolute path), filters, selected count, `includes_bodies`, and the pricing table/fallback version used for estimates. Each following line is one complete detail record keyed by `run_id:id`, with a `pricing` object (`cost_micros`, `priced`, `input/output/cached rates`) so later analysis is reproducible. Missing/evicted/truncated bodies retain their flags. Export never bypasses redaction or reconstructs discarded content.

The confirmation states exactly what will be included. Full-body export requires an additional acknowledgement when bodies are present. The result shows path and call count; failures leave no partial file presented as successful (write temporary, then rename).

## 7. Friendlier language, states, and Mnemo1 noise to drop

### Metric glossary

Every label below has a hover/focus explanation and an accessible text value. Technical names appear only in Raw/export.

| UI label | Source | Plain-language explanation |
|---|---|---|
| **Status** | `is_complete`, `error`, `cancelled`, `guard_cut`, HTTP | Whether the call is waiting, streaming, finished, failed, cancelled, or cut by a safety limit. |
| **Total time** | captured phase timestamps | Wall time for this call. Tool execution and local setup are identified separately where applicable. |
| **Input** | `usage.prompt` | Tokens sent to the model, including instructions, history, and tool schemas. |
| **Cached input** | `usage.cached` | Input tokens the provider says it served from cache. |
| **Cache reuse** | `cached / prompt` | Percentage of input reported as cached. “Not reported” is distinct from 0% when usage is absent. |
| **Output** | `usage.completion` | Tokens generated by the model, including reasoning tokens. |
| **Reasoning** | `usage.reasoning` | The portion of output used for model reasoning; it is not added to Output twice. |
| **Estimated cost** | shared `price_tokens` rule | Estimate from current model pricing and final token counts. It can differ from the provider bill and is unavailable without a price. |
| **Setup** | `prep_ms` | Local time preparing the request: routing, token accounting, recall, and prompt building. |
| **Compaction** | `compact_ms` | Time spent shortening earlier context. Included in Setup, not extra time. |
| **Connect** | `connect_ms` | Record creation → response headers (compatibility metric, ≈ upload + server wait). For SSE providers headers arrive with the first chunk, so this includes the server's queue + prefill. |
| **Upload** | `upload_ms` | POST sent → request body fully written to the socket. |
| **Server wait** | `server_ms` | Upload done → response headers: the server's queue and prefill before the first token. |
| **First token** | `ttft_ms` | POST sent → first streamed chunk; a metric chip, not a bar segment. |
| **Generation** | `generation_ms` | Time from the first chunk until the final response/usage chunk. |
| **Stalls** | `stall_ms` | Silences of more than two seconds inside Generation. Included in Generation, not extra time. |
| **Tools** | `tools_ms` | Time running the tools requested by this response, after model streaming ends. |
| **Retry wait** | `backoff_ms` | Delay before a retry attempt; never counted again as Connect. |
| **Finish** | `finish_reason` | Why the model stopped, translated where known (Completed, Tool calls, Length limit). |
| **HTTP** | `http_status` | Provider HTTP response code, shown mainly for failures and diagnostics. |
| **Guard cut** | `guard_cut` | A client-side stream limit ended the call; show which limit and its configured threshold. |

### Empty, loading, partial, and error states

- **No calls yet:** “Model calls will appear here when the lead or a worker runs.” Include an Open Lead action, not a blank chart.
- **No filter matches:** show active filters as removable chips and Clear filters.
- **Metadata only:** “This call survived restart without its prompt or response. Enable full-body logging for future calls.” Never show an empty Raw editor as though content existed.
- **Truncated/evicted:** use the existing `request_truncated`, `response_truncated`, and `response_evicted` flags to name the exact missing portion and limit.
- **Live estimate:** label output/reasoning as estimates until final usage arrives. A request that ends without usage says “Provider did not report token usage.”
- **Load failure:** keep the current list visible, show a concise retry banner, and preserve selection. Detail failure never clears the list.
- **No pricing:** show `—`, an Unpriced badge, and the model name; never imply zero spend.
- **Attribution missing:** show Unattributed with a diagnostic tooltip, rather than assigning the row to the lead.

Use only Mnemo2 colour tokens and the existing dark theme, Inter, `--text-meta/--text-body/--text-title/--text-page`, icon buttons with labels/tooltips, and existing sheets/dialogs. All hover explanations also work on keyboard focus.

### Drop or demote from Mnemo1

- Drop the permanently visible, resizable statistics strip; use one collapsible Insights panel.
- Drop raw top-level request keys as default chips; move them to readable Request settings.
- Drop per-message raw JSON as the primary expansion; readable content is primary, JSON is secondary.
- Drop `PROMPT`/`SSE` jargon, cache-percent badges without context, and repeated error badges in several sections.
- Drop the prefix badge/copied-header developer controls from the default detail. Keep one explained prefix comparison.
- Demote endpoint host, numeric id, version, finish reason, HTTP status, and technical guard names to Overview advanced details or Raw.
- Do not port Mnemo1's column-customization controls initially. They add settings without answering the two core questions.

## 8. Backend/frontend shape and proposed build steps

### Shared view and transport additions

Keep `LlmRequestSummary` lightweight. Add attribution, `run_id`, and `cost_micros` (computed with the same pricing function as Cost); keep bodies detail-only. Add a `TraceListQuery`/`TraceListPage` wire contract for filters, pagination, totals, and `has_more`. Extend `mnemo2_trace_view` with list/query, detail, logging/privacy settings, history deletion, and export commands. Preserve the existing no-filter list command during migration so the compact tab does not break.

Extract the pricing calculation from `insights.rs` into a small shared Mnemo2 backend module rather than duplicating its cached/uncached rule. Trace computes estimates only for final provider usage, never for streaming estimates. Cost remains ledger-based; no trace row writes a second usage-ledger event.

Suggested frontend components: `TraceView.svelte` (page/state), `TraceList.svelte`, `TraceDetail.svelte`, `TraceOverview.svelte`, `TraceContext.svelte`, `TraceResponse.svelte`, `TraceCharts.svelte`, and `MetricTip.svelte`. Keep formatting, status, filter, phase, and token calculations in `lib/trace.ts` / `lib/traceCharts.ts` so the page and Workbench tab cannot drift.

### Step A — kernel attribution and capture identity (backend owner)

Files: `src/provider/trace.rs`, `src/provider/mod.rs`, `src/provider/openai.rs`, `src/provider/anthropic.rs`, `src/agent/loop_impl.rs`, `src/agent/turn.rs`, `src/agent/context.rs`, `src/headless/types.rs`, `src/headless/engine.rs`, `src/host/runtime.rs`, and focused tests. Add run/attribution context and the concurrency tests above. This is high-risk shared-contract work and needs independent audit under Mnemo2 policy.

### Step B — Mnemo2 backend query, pricing, persistence, and export (backend owner)

Files: `src-tauri/src/mnemo2/trace_view.rs`, new shared pricing module, `insights.rs`, `runtime.rs`, `team.rs`, trace config startup wiring, `src-tauri/src/mnemo2/mod.rs`, command registration, and backend tests. Supply worker/plan identity, metadata journal reload, filtering/search, cost estimates, settings, and JSONL export. Depends on Step A's record contract.

### Step C — frontend data layer and full page (frontend owner)

Files: `mnemo2/src/lib/trace.ts`, `types.ts`, route/store actions, `App.svelte`, `TopBar.svelte`, new list/detail/overview/context/response components, and unit/component tests. Build against the reviewed wire contract with injected transport and preview fixtures. Can run in parallel with A/B after the contract is frozen; final integration depends on B.

### Step D — charts (frontend owner, parallel with C)

Files: `mnemo2/src/lib/traceCharts.ts`, `TraceCharts.svelte`, chart tests, and chart preview scenes. Depends only on the summary/query contract, not on the detail components. Test non-overlapping token arithmetic and the phase-attribution spec numerically.

### Step E — compact tab, persistence/export UI, and previews (frontend owner)

Files: `mnemo2/src/components/TraceTab.svelte`, `TraceView.svelte`, `mnemo2/src/test/previewScenes.ts`, focused tests, and user-facing settings copy. Replace the old tab internals with the shared view model; add empty/error/partial scenes, follow-new-calls behavior, export confirmations, and full-page navigation. Depends on C; export/settings integration depends on B.

### Step F — integration and compatibility verification (backend + frontend owners, sequential)

Verify real Tauri command payloads, persisted restart data, live streaming updates, worker deletion, export contents, Mnemo1 compatibility, keyboard/focus behavior, and all preview scenes. Run `cargo test --workspace` plus Mnemo2's established frontend test/build commands. Any feature-gated dependency change would additionally require its feature-enabled test run, though this design needs no dependency bump.

Parallelism summary: **A → B** is the backend spine. **C** and **D** can proceed together against the frozen contract while A/B land. **E** follows C; **F** is last. Only one worker owns the shared kernel files in A; frontend workers do not edit backend files.

## Acceptance scenarios for the integrated result

- Run the lead and two workers concurrently, including a worker on a plan step. Every list/detail/export row identifies the correct actor, session, worker, plan/step, turn, and purpose; no shared-provider interleaving cross-labels a call.
- The full Trace route and Workbench tab both show who called, model, purpose, input/output tokens, reported cache reuse, estimated cost, duration, and live/final status. Opening a call exposes timing, readable context, reasoning, answer, delivered tools, and raw data subject to retention flags.
- Apply agent/model/status/purpose/time filters, turn grouping, and content search. Results and all three charts use the same filtered set; errors and cancellations remain distinct.
- During streaming, output estimates and status update without moving a selected older row. Final usage replaces estimates; unchanged selected details are not refetched.
- For a known pricing fixture, Trace's uncached-input + cached-input + output estimate exactly matches the shared Cost pricing rule. An unpriced model displays `—`, not zero.
- Prefix comparison uses the previous call from the same agent/purpose/model and explicitly distinguishes textual similarity from provider-reported cache hits.
- With default privacy, restart the app: retained metadata and filters remain useful, but no prompt, reasoning, response, or tool arguments are written to the metadata journal. Opting into full-body logging retains diagnosable detail within the documented caps.
- Export one call and a filtered selection. The JSONL begins with the v1 manifest, preserves `run_id:id`, attribution, retention flags, and pricing inputs, and contains only the promised body data.
- Verify request truncation, response truncation/eviction, no-usage, no-pricing, guard-cut, cancelled, failed, empty, and history-detail-unavailable states in preview scenes and backend tests.
- Mnemo1's trace still lists and inspects records without attribution, and the existing trace/provider test suites pass without changes to its UI behavior.

## Open questions for lead review

1. **TopBar density:** this design recommends a first-class Trace item between Cost and Graph. Should all seven items remain visible at the current breakpoint, or should Graph/Plans/Memory collapse earlier into a workspace menu?
2. **Failed bodies:** approve Mnemo2 setting `persist_failed_bodies=false` by default? It makes the privacy promise consistent but removes today's always-available failed prompt/response diagnostics unless full-body logging is enabled.
3. **Metadata sensitivity:** is an always-on per-project journal containing actor/plan names, models, timing, token counts, and error classes acceptable? This design assumes yes; content and raw error bodies remain excluded.
4. **Retention defaults:** are 256 live summaries, roughly 40 MiB of metadata archives, and the existing roughly 320 MiB maximum full-body history reasonable defaults? They are deliberately bounded and independent.
5. **History deletion:** should deleting retained history require only project-level confirmation, or an additional typed confirmation when full bodies exist? This design proposes a normal explicit confirmation with the size/count stated.
6. **Purpose taxonomy:** are the five proposed purposes enough for the first version? The contract permits `"other"` so a new auxiliary call is not forced into a misleading category.
7. **Cross-project analysis:** export is portable, but the UI remains project-scoped. A future global analysis view can aggregate exported bundles; it is intentionally not part of this build.
