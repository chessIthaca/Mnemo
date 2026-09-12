# Architecture Performance Review — myharness

**Perspective:** Performance (hot path, streaming, frontend render, memory, multi-agent)  
**Reviewer:** read-only architecture reviewer  
**Date:** 2026-04-08  
**Scope:** `src/` (Rust brain), `src-tauri/src/ipc/` (event forwarder + commands), `frontend/src/` (React streaming UI)  
**Method:** structural code inspection of critical paths; no new benchmarks. Severity distinguishes measured/structural risks from speculative ones.

---

## Executive summary

The architecture is **already performance-aware in several high-traffic places**: text-delta rAF batching, plain-text streaming (no per-token markdown), message memoization, constitution mtime caching, FTS-prefilter memory recall, WAL read/write split, shared `reqwest` client, fan-in lock avoidance on delta events, and token-count reuse on the no-summarize path.

The remaining risks cluster in a few places that dominate real wall time or UI jank:

1. **Blocking filesystem work on the async runtime** (`std::fs` / `glob` / `canonicalize` inside async tools) can stall the tokio worker pool under concurrent agents.
2. **Per-token IPC fan-out** (Rust → Tauri emit → JS) for every `TextDelta` / `ReasoningDelta` / `ToolCallArgDelta` — frontend batches *text*, but IPC and the agent still emit per token; reasoning and tool-arg deltas are **not** rAF-batched.
3. **Tiktoken full-conversation re-encode every tool iteration** plus **auto-recall (embed + score) every tool iteration**, even when only a tool result was appended.
4. **Quadratic LCS diff** in the frontend for approval/diff views (`O(n·m)` DP table) with no size guard.
5. **`search` walks + fully reads every matching file** on the async task with no early-exit once results are capped (still scans for total match counts).
6. **Shell/tool outputs can be unbounded** in the conversation context (file_read is capped; shell/git/search are only partially capped).
7. **Broad Zustand subscriptions** (`MainPanel` / `App` take whole `agents` map) re-render the active conversation tree on any agent’s stream update.

None of these are correctness bugs; they are latency/scalability ceilings. For a single interactive agent on a modest project they are often masked by LLM network time. They become visible with multi-agent spawn, large files, long sessions, or reasoning models that flood deltas.

---

## Hot-path map (turn lifecycle)

```
UI send_prompt (IPC)
  → AgentManager.send try_send → agent cmd mpsc (cap 64)
  → AgentTask::run Prompt arm
      · optional memory start_session + correction detect (DB write)
      · optional vision describe (HTTP, sequential per image)
  → AgentLoop::run_turn  [loop until no tool calls]
      1. ContextManager::count_tokens(messages)          // tiktoken BPE whole history
      2. maybe summarize_with_interrupt (extra LLM stream)
      3. memory.recall(last user text)                   // embed + FTS/full scan + cosine
      4. constitution.reload_if_changed()                // 2× stat, rare read
      5. build_system_prompt + tools.schemas(filter)
      6. complete_with_retry → OpenAiClient SSE
           · build_request_json (clone/serialize full msgs+tools)
           · bytes_stream → parse_sse → mpsc(128) LlmEvent
      7. for each stream event:
           · fanin_tx.send(TextDelta|ReasoningDelta|ToolCall*)  // await send
      8. for each tool call:
           · approval gate (oneshot; may block on UI)
           · tools.dispatch → often std::fs / glob / Command
           · fanin ToolResult (full output string)
           · append tool msg; loop to (1)
  → fan-in mpsc (cap 256) → event forwarder
      · into_serializable + app.emit("agent://event")     // every event
  → frontend useAgentEvents
      · text_delta → rAF buffer → appendStreamingText ~60fps
      · other events → immediate handleAgentEvent
  → Conversation re-render (memoized finalized messages)
```

**Dominant cost today (expected order, structural):**  
LLM network/generation ≫ tool process I/O (shell/cargo) ≫ request JSON + tiktoken + recall embed ≫ UI render ≫ channel/IPC overhead — *except* when multi-agent + search + large diffs invert the middle of that list.

---

## Findings by severity

### Critical

_None observed as guaranteed production breakers from inspection alone._ No unbounded channel drop of text deltas on the production fan-in path (capacity 256, agent uses awaiting `send`). No obvious runaway tight loop without backoff.

### High

#### H1. Blocking FS/search on the async runtime (worker-pool stall)

**Where:**  
- `src/tool/agent/file_read.rs` — `std::fs::read_to_string`  
- `src/tool/agent/file_edit.rs` / `file_write.rs` / `file_append.rs` — `std::fs::{read,write,create_dir_all}`  
- `src/tool/agent/search.rs` — `glob::glob` + full-file `read_to_string` per file  
- `src/tool/agent/sandbox.rs` — `canonicalize` / `metadata` on every validate  
- `src/tool/agent/describe_image.rs` — `std::fs::read`  

**Why it matters:** Tools run as `async fn execute` but do **synchronous** disk I/O. Under multi-agent concurrent tool use, this blocks tokio workers. Shell/git correctly use `tokio::process::Command`; file tools do not use `spawn_blocking`.

**Complexity:** search is `O(files × lines)` with full content loads; capped at 100 *returned* matches but **continues scanning** after cap to count totals (`search.rs` ~166–181).

**Kind:** structural / multi-agent scalability.

#### H2. Auto-recall + full tiktoken count every tool iteration

**Where:** `src/agent/turn.rs` ~81–171, `src/agent/context.rs` `count_tokens` / `try_tiktoken_count`, `src/memory/mod.rs` `recall`.

**Behavior:** Each inner-loop iteration (often one per tool call) does:
1. Full-history BPE encode (`cl100k_base` per message content + tool args).
2. Auto-recall against the **latest user message** (not the latest tool activity): embed query (Ollama HTTP or hash), FTS candidate load or **full table scan**, cosine over candidates, `batch_access` write.

**Why it matters:** A turn with 20 tool calls pays ~20× token counting and ~20× recall/embed even when the user text is unchanged. Token counting was correctly de-duplicated vs a second count on the no-summarize path, but the *per-iteration* cost remains.

**Complexity:** tiktoken ≈ `O(total_chars)`; recall best case `O(K·d)` with FTS (`K=50`, `d=768`), worst case `O(N·d)` full scan + keyword `to_lowercase` on every candidate.

**Kind:** structural hot-path tax.

#### H3. Reasoning + tool-arg deltas bypass rAF batching (IPC + React flood)

**Where:**  
- Emit: `src/agent/turn.rs` TextDelta / ReasoningDelta / ToolCallArgDelta (awaiting `fanin_tx.send`)  
- Forward: `src-tauri/src/ipc/events.rs` emits every serializable event  
- UI: `frontend/src/hooks/useAgentEvents.ts` batches **only** `text_delta`; all other kinds flush + `handleAgentEvent` immediately  
- Store: `reduceReasoningDelta` / `reduceToolCallArgDelta` in `useAgentStore.ts` clone transcript/activityLog per fragment  

**Why it matters:** Text streaming was carefully optimized (plain text + rAF + memo). Reasoning models and large tool JSON arguments can still drive **per-fragment** store updates and re-renders (InflightBar activity log; tool card args). Each update clones agent state and may re-render subscribers of the whole `agents` map.

**Kind:** structural UI/IPC hotspot; severity rises with reasoning models.

#### H4. Frontend LCS diff is `O(n·m)` with full DP matrix

**Where:** `frontend/src/components/chat/DiffView.tsx` `computeDiff`.

**Behavior:** Allocates `(n+1)×(m+1)` number table; used by approval UI and DiffViewer for `old_string`/`new_string`. No line-count guard. Large file rewrites (thousands of lines) → multi‑MB DP + multi‑second main-thread freeze.

Rust side uses `similar` for preview strings in tools, but approval UI often diffs raw args client-side (and `dispatch.rs` currently sends `preview: None`, so the UI leans on arg fields + LCS).

**Kind:** structural; user-visible jank on large edits.

### Medium

#### M1. Coarse Zustand subscriptions re-render the chat chrome on every stream tick

**Where:**  
- `frontend/src/App.tsx` — `const agents = useAgentStore(s => s.agents)`  
- `frontend/src/components/layout/MainPanel.tsx` — same  
- Active `Conversation` receives full `AgentState`; streaming path re-renders list container every rAF  

**Mitigations already present:** `Message` is `memo` with content equality; streaming uses plain text; scroll throttled 100ms.

**Gap:** Any field change on *any* agent (including background agents’ running flags / text) updates the `agents` object identity → MainPanel re-renders tab bar + conversation shell. Background agent streams can tax the *visible* agent’s React tree.

**Kind:** structural React cost.

#### M2. Fan-in is a single consumer; multi-agent streams serialize at emit

**Where:** `AgentManager::new(256)` (`src-tauri/src/main.rs`), single forwarder loop (`events.rs`).

**Behavior:** All agents’ events share one mpsc; forwarder is single-threaded `recv → emit`. Delta path correctly avoids manager lock, but Tauri `app.emit` + JSON serialize of every delta is sequential. Agents use `.send().await` (backpressure) so a slow UI/webview **stalls the agent stream loop** (and thus the next LLM read select arm).

Production capacities: fan-in 256, per-agent cmd 64, provider internal SSE channel 128. Commands use `try_send` (can drop/fail under burst — intentional for Cancel/Prompt routing).

**Kind:** multi-agent scalability ceiling (structural).

#### M3. Shell / git outputs unbounded in context and IPC

**Where:** `src/tool/agent/shell.rs` returns full stdout+stderr; no byte cap analogous to `file_read`’s 100 KB / 2000 lines. Search caps matches at 100 lines but can still produce large lines. Tool results are embedded in next LLM request + emitted to UI + `toolOutputLog` (UI log sliced to 50 — good).

**Why it matters:** `cargo test` / build logs can inflate context past summarize threshold quickly, force expensive summarization LLM calls, and bloat JSON request bodies.

PLAN.md claims “Large tool results are truncated with a note” — **file_read implements this; shell does not.** Gap between design and implementation.

**Kind:** structural context blow-up.

#### M4. Summarization rebuilds a giant user prompt string

**Where:** `src/agent/context.rs` `summarize` / `summarize_with_interrupt`.

**Behavior:** Joins all middle messages into one string, then streams a full LLM summary. At 50% of a 128k window this can be tens of thousands of tokens **as a single user message** — itself near context limits, high latency, and high cost. Interruptible (good). Default fill rate 0.5 means summarization fires mid-session often on heavy tool use.

**Kind:** structural cost cliff.

#### M5. Keyword boost in recall is `O(N · |query|)` string work

**Where:** `src/memory/mod.rs` ~475–481 — `to_lowercase()` on title, content, **and query** inside the per-memory map.

Even with FTS limiting to 50 candidates this is minor; on full-scan fallback with thousands of memories and large content blobs it dominates CPU after the embed returns.

**Kind:** structural; worsens as memory DB grows without FTS hits.

#### M6. Approval map resolve is linear scan; manager lock on lifecycle events

**Where:** `src-tauri/src/ipc/approval.rs` `resolve` scans keys; `events.rs` locks manager on Started/Finished/Exited/approval/main-agent checks.

Usually tiny maps; not a steady-state stream cost. Under many concurrent approval prompts could add latency. Not the primary bottleneck.

**Kind:** low-probability structural.

#### M7. Image attachments as base64 in events and messages

**Where:** `AgentCommand::Prompt` / `PromptDispatched` carry full data URLs; multipart messages keep base64 in conversation history → every subsequent `build_request_json` and tiktoken path may re-process huge strings (tiktoken uses `as_text()`, which may or may not include image URLs depending on `MessageContent` impl — still retained in memory).

**Kind:** session-memory / request-size risk when users paste screenshots.

### Low

#### L1. Constitution / safety rules mtime checks — already efficient

**Where:** `src/project/agent_md.rs` — two `stat`s per turn, read only on change. Matches PLAN intent; not a hotspot.

#### L2. Shared HTTP client + connect-only timeout — good

**Where:** `src/provider/openai.rs` — pooled `reqwest::Client`, no total request timeout, 90s per-chunk read timeout. Correct for long SSE.

#### L3. SSE buffer drain uses in-place `drain` — good

Comments in `openai.rs` document avoidance of O(n²) buffer recopy.

#### L4. Stats recording is fire-and-forget

**Where:** `turn.rs` spawns `record_request_stats` — does not block stream. Good.

#### L5. toolOutputLog capped at 50

**Where:** `useAgentStore.ts` `.slice(-50)`. Prevents unbounded Output tab growth. Transcript itself is still unbounded until `/clear` (expected for chat UX; long sessions → large DOM).

#### L6. Conversation list not virtualized

**Where:** `Conversation.tsx` maps entire transcript. Memo helps finalized messages, but DOM node count grows without bound over long sessions. Acceptable for typical sessions; weak for day-long Run-All.

#### L7. Plan file I/O

Plan parse/serialize is small markdown; rewrite-on-step-complete is fine. Not a hot path relative to LLM.

#### L8. Startup

`main.rs` `block_on` for manager setup, opens memory DB + dual connections, builds factory. One-time cost; acceptable. Frontend loads full chat stack (react-markdown, highlight.js) up front — no code-splitting observed; moderate bundle cost, not runtime hot path.

---

## Frontend-specific performance

| Area | Current state | Risk |
|---|---|---|
| Text deltas | rAF batch + plain text while streaming | Good |
| Final markdown | ReactMarkdown + rehype-highlight once on flush | Good for normal msgs; large code fences still expensive once |
| Message list | `memo` + custom equality | Good |
| Scroll | 100ms throttle | Good |
| Reasoning deltas | Immediate store updates + activityLog clone | High with reasoning models |
| Tool arg deltas | Immediate transcript clone per fragment | Medium–High for large JSON tools |
| Diff | Classical LCS DP, no cap | High on large files |
| Subscriptions | Whole `agents` map in App/MainPanel | Medium multi-agent |
| Virtualization | None | Low–Medium long sessions |
| Images in chat | Full data URLs in DOM `<img>` | Medium memory |

**Quick frontend wins:** extend rAF batching to `reasoning_delta` and `tool_call_arg_delta`; select `agents[activeId]` (and running flags separately) instead of whole map; guard `computeDiff` (if `n*m > threshold`, fall back to side-by-side / Myers / `similar`-style or head/tail only); virtualize transcript beyond N entries.

---

## Multi-agent & memory scalability

### Multi-agent

| Resource | Sharing | Bottleneck |
|---|---|---|
| Fan-in channel | Global, cap 256 | Single forwarder serialize + backpressure into agents |
| Provider client | Shared `Arc` / factory swap | Concurrent HTTP OK; rate limits external |
| Memory store | Shared; WAL + read conn | Writer mutex serializes writes; recalls concurrent on read conn |
| Sandbox / project root | Shared | FS contention OS-level |
| Safety mode / config | Global RwLock | Fine |
| Pending approvals | `(agent_id, tool_call_id)` | Cleanup per agent — good; no leak on Exited |
| Per-agent workflow | Own `Mutex<Workflow>` | Fine |
| Conversation `messages` | Per AgentTask `Vec` | Grows until summarize; independent |

**Fan-in bottleneck:** N streaming agents × tokens/sec must pass one emit loop. With backpressured `.send().await`, slow webview slows **all** agents’ generation consumption (provider side may buffer in the 128-event channel then block).

**try_send on commands:** Parent completion `Suggestion` and Cancel cleanup use `try_send` — can silently drop under full inbox (documented as best-effort). Not a perf bug but a reliability edge under load.

### Memory

| Path | Design | Residual risk |
|---|---|---|
| WAL + dual conn | Readers don’t block writer | Good |
| FTS prefilter top 50 | Avoids O(N) cosine when FTS hits | Fallback full scan still exists |
| batch_access | One UPDATE for K ids | Good |
| Embedder | Ollama HTTP (60s timeout) or HashEmbedder | Auto-recall **awaits** embed every iteration — if Ollama is slow/down, every tool loop pays timeout path / fallback |
| Working tier cleanup | delete on consolidate | Prevents unbounded working set — good |
| Vectors in SQLite BLOB | In-process cosine | Fine to low thousands; not ANN |

**Lock note:** `read_conn` is still `Mutex<Connection>` — concurrent recalls serialize on the **connection** even though WAL allows concurrent readers at the SQLite level. Multiple agents recalling simultaneously queue on one mutex. A pool of read connections would scale better (Medium, multi-agent).

---

## Quick wins vs structural improvements

### Quick wins (small diffs, high leverage)

1. **rAF-batch `reasoning_delta` and `tool_call_arg_delta`** the same way as `text_delta` (`useAgentEvents.ts`).  
2. **Narrow Zustand selectors** — subscribe to `agents[activeAgent]` / per-field selectors; tab bar should subscribe to `Record<id, {name,running}>` only.  
3. **Cap shell/git tool output** (e.g. 100–200 KB + note) before returning `ToolResult` and before appending to messages. Aligns code with PLAN.md.  
4. **Skip auto-recall when the latest user message unchanged** since last recall within the turn (cache last query string → results).  
5. **Diff size guard** in `computeDiff` (e.g. if `n*m > 2e6`, show truncated/heuristic diff).  
6. **Stop search scan early** once `MAX_MATCHES` hit if total count isn’t required; or sample count.  
7. **Hoist `query.to_lowercase()`** out of the per-memory loop in `recall`.  
8. **Reuse token count** across tool iterations when only a small suffix changed (incremental estimate, or recount only after summarize / large tool result).

### Structural improvements

1. **`spawn_blocking` (or dedicated blocking pool) for all FS tools + search + sandbox validate** so multi-agent tool I/O cannot starve the runtime.  
2. **Coalesce deltas in Rust** before fan-in (time- or byte-based batching of Text/Reasoning/Arg fragments) to cut IPC/serialize overhead by 10–50×.  
3. **Incremental or approximate token counting**; consider provider-reported `prompt_tokens` for ContextUsage instead of local BPE every loop.  
4. **Summarization strategy** — hierarchical/map-reduce summaries, or lower keep_recent + cheaper local model; avoid stuffing 50% of context into one summarize prompt.  
5. **Read-connection pool** for memory; optional vector index if N grows past ~10k.  
6. **Transcript virtualization** + optional session compaction in the UI (not only model context).  
7. **Per-agent or buffered event channels** to the UI so one busy agent cannot backpressure others.  
8. **Move heavy diff to Rust `similar`** and ship a compact hunk list over IPC (approval already has `ApprovalPreview` plumbing; `preview: None` in dispatch leaves it unused).

---

## Open questions / what to measure next

Instrumentation already partially exists (`ttft_ms`, `generation_ms`, token usage, context used/max). Suggested measurements (no code in this review):

1. **Per-iteration turn overhead breakdown** (ms): `count_tokens`, `recall`, `build_request_json`, time-to-first-byte, tool wall time, approval wait.  
2. **Delta rates:** events/sec on `agent://event` during normal vs reasoning vs parallel multi-agent runs; JS main-thread long tasks.  
3. **Fan-in queue depth** high-water mark (is 256 ever approached?).  
4. **Recall path mix:** FTS hit rate vs full-scan fallback; embed latency p50/p95 (Ollama).  
5. **Search:** files touched / bytes read / wall time on this monorepo with default `**/*`.  
6. **Diff:** line counts that make `computeDiff` exceed 50ms/200ms.  
7. **React:** commits/sec of `Conversation` / `MainPanel` with React Profiler during streaming; cost of background agent updates on active tab.  
8. **Context growth:** tool-result token share vs assistant text over a full plan execution (validates shell truncation priority).  
9. **Worker pool:** tokio runtime metrics or simple probe — does concurrent `search` + `file_read` stall unrelated timers?

---

## Positive patterns worth preserving

- Text-delta rAF batching + plain-text streaming + `Message` memo (`useAgentEvents.ts`, `Message.tsx`, `Conversation.tsx`).  
- Event forwarder takes fan-in rx so commands don’t deadlock; delta path avoids manager lock (`events.rs`).  
- Constitution mtime cache (`agent_md.rs`).  
- Token count reused for ContextUsage when not summarizing (`turn.rs`).  
- Memory WAL + separate read connection + FTS candidate pool + `batch_access` (`memory/mod.rs`, `schema.rs`).  
- Shared reqwest client, SSE read timeout without total-request kill (`openai.rs`).  
- file_read output caps (2000 lines / 100 KB).  
- Approval cleanup on agent exit; toolOutputLog ring buffer (50).  
- Shell/git use async `tokio::process`.

---

## Summary table

| ID | Severity | Topic | Primary refs |
|---|---|---|---|
| H1 | High | Blocking FS/search on async runtime | `tool/agent/{file_*,search,sandbox}.rs` |
| H2 | High | Tiktoken + recall every tool iteration | `agent/turn.rs`, `agent/context.rs`, `memory/mod.rs` |
| H3 | High | Reasoning/tool-arg delta flood (no rAF) | `useAgentEvents.ts`, `turn.rs`, `events.rs` |
| H4 | High | O(n·m) frontend LCS diff | `chat/DiffView.tsx` |
| M1 | Medium | Whole-`agents` React subscriptions | `App.tsx`, `MainPanel.tsx` |
| M2 | Medium | Single fan-in forwarder backpressure | `runtime/mod.rs`, `main.rs`, `events.rs` |
| M3 | Medium | Unbounded shell/git outputs in context | `tool/agent/shell.rs` vs PLAN.md |
| M4 | Medium | Heavy summarization prompt | `agent/context.rs` |
| M5 | Medium | Recall lowercase / full-scan fallback | `memory/mod.rs` |
| L5–L8 | Low | DOM growth, startup bundle, plan I/O | `Conversation.tsx`, `main.rs` |

**Bottom line:** The streaming UI path for assistant text is thoughtfully optimized. The next performance work should target **(a)** async-offloaded tool I/O, **(b)** per-iteration turn taxes (tokens + recall), **(c)** non-text delta batching + selector hygiene, and **(d)** large-diff / large-tool-output guards — in that order for multi-agent and long-session scale.
