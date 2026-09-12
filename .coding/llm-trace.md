# LLM Trace — request/response inspector for cache-hit optimization

The **Trace** tab (right panel, pulse icon) records every request Mnemo
sends to the OpenAI-compatible `/chat/completions` endpoint — the **exact
request JSON** that went out and the **raw response** that came back — and
lays them out for cache-hit work: a message wall of requests, usage/cached
token cards, and a per-message compare against the previous request to see
exactly what breaks the provider's cache prefix.

A resizable stats strip sits at the top of the tab (drag the splitter between
the graphs and the log to resize it): two live vertical column charts —
stacked per-request phase times and token stacks (prompt with a dithered
cached overlay showing the cache-hit rate, completion, reasoning) — with
the newest request always the rightmost column and a **Fill/Relative** scale toggle (Relative, the
default, makes heights comparable across requests: a 2-minute request's
column is 4× taller than a 30-second one; Fill normalizes every column to
full height). Every legend chip is a click-toggle that hides/shows its
series in that chart (persisted across sessions via the
`tracestats.hiddenSegments` localStorage key), and column maxima recompute
over the visible series only — hiding a dominant segment rescales the rest
for more resolution (most useful in Relative mode). The `cached` chip
toggles only the dithered overlay inside the prompt bar; the prompt bar
itself always counts toward the height.

## Opening the tab

1. If the right panel is hidden, toggle it with the panel button in the left
   sidebar.
2. The Trace tab is in the right-panel tab bar (pulse/activity icon). If it
   was disabled, re-enable it from the left sidebar (pulse icon button).
3. Send any prompt — every LLM request (agent turns, tool loops, subagents)
   lands in the log automatically. No recording setup needed; the log is
   wired into every provider the app builds.

## What each request row shows

Each row in the left message wall is one request, newest first:

- **HH:MM:SS** — when the request was POSTed.
- **cache badge** — `N% cached` with at most one decimal (`99.3% cached`;
  the app-wide percentage rule) — green when > 0, grey at 0%. This is the
  response's cached-tokens field (OpenAI `prompt_tokens_details` /
  `input_tokens_details` `cached_tokens`; Anthropic
  `cache_read_input_tokens`) over prompt_tokens. Absent when the provider
  didn't report usage.
- **ERR marker** — red when the request failed (HTTP ≥ 400 or a stream error).
- **model · provider** (provider dimmed — the endpoint name from
  `endpoints.toml`), **prompt in / completion out** token counts, **TTFT** in
  ms.

Select a row to inspect it in the right-hand detail pane.

## The detail pane

### Usage card

Prompt / completion / reasoning / **cached** token counts, a cache-hit ratio
bar (green = some prefix is being served from cache), the finish reason, and
TTFT / generation time.

### Request section

The exact JSON body sent to `/chat/completions`:

- **Smart view** (default): top-level chips (`model`, `stream`,
  `max_completion_tokens`, `reasoning_effort`, `tool_choice`), then a wall of
  collapsed message rows — role chip, name, character count, first-line
  preview. Click a message to expand its full pretty JSON. Tools collapse to
  an `N tools` chip that expands to name + description.
- **Raw JSON** toggle: the entire request body, pretty-printed, in a
  scrollable pre.

### Response section

HTTP status, finish reason, and the raw response text (SSE lines / JSON error
body) exactly as received, collapsed to the first ~2 KB with an Expand toggle.

## Cache-hit optimization workflow

Provider prompt caching works on a **prefix**: the first N tokens of your
request are compared against previous requests; identical prefixes are served
from cache. To find what breaks the prefix:

1. Open Trace, select the request you care about.
2. Turn on **Compare with previous** (top of the detail pane).
3. Every message gets a badge vs the immediately previous request:
   - grey **unchanged** — identical serialized content (cacheable)
   - amber **changed** — same position, different content (prefix breaker)
   - green **added** — new message this request (prefix breaker)
   - red **removed** — messages dropped since the previous request
4. The usage card shows the punchline: *"First k of n messages identical to
   the previous request — the cacheable prefix ends at message k (message
   k+1 breaks it)"*, next to the measured `cached/prompt` ratio.

The usual culprits, in order of frequency:

- **System prompt drift** — any edit to `agent.md` (or anything the system
  message embeds, like the date) changes message 1 and kills the whole
  prefix. Keep the system prompt byte-stable.
- **Tool definitions** — tools are sent on every request; if their order,
  descriptions, or JSON schemas change between turns, the tools block (which
  sits right after the system message) breaks the prefix. Keep tool order
  stable and schemas byte-identical.
- **Non-append-only messages** — a tool result or assistant turn that gets
  rewritten (e.g. context summarization rewriting old turns, or the agent
  loop mutating history) changes a middle message and invalidates everything
  after it. Messages should only ever be appended.
- **Chatty tool results** — even a perfectly stable prefix gives a low
  hit *ratio* when each turn appends a huge tool output; the ratio drops
  even though the cache is working. Compare mode tells you the prefix is
  intact (all messages unchanged) — the cost is the new content, not the
  cache.

## Limits

- **32-record ring buffer** — the newest 32 requests are kept; older ones are
  evicted (ids keep counting up, so compare mode may report "no previous
  request in the log" after heavy use). Clear the log with the **Clear**
  button.
- **2 MiB raw-response cap per record** — responses longer than that are
  stored as a prefix and flagged ("backend capture truncated at 2 MiB").
- **Vision + embedder requests are not captured** — only the main
  `/chat/completions` provider path (agent turns, per-context model
  overrides, `set_model` swaps). `describe_image` / vision-client requests
  and memory embedder calls don't appear.
- **No per-agent attribution** — the log is global across all agents; rows
  show time + model, not which agent sent them.
- **Session-only in memory** — the log is a ring buffer cleared on app
  restart. Persistence is separate: the **Log to file** checkbox (opt-in)
  mirrors each record to `.coding/logs/traces.jsonl`, and failed requests are
  always appended (one compact line each) to
  `.coding/logs/provider-errors.jsonl`.

## Internals

- Capture happens inside the provider clients' `complete()` (`OpenAiClient`
  in `src/provider/openai.rs`, `AnthropicClient` in
  `src/provider/anthropic.rs`), which already builds the exact request body
  and parses the SSE stream — the trace log is mirrored from the same code
  path, so what you inspect is byte-for-byte what the provider received.
- The store is `LlmRequestLog` (`src/provider/trace.rs`): a mutex-protected
  ring buffer shared via `Arc` across the default provider, the model
  resolver, and the IPC layer (`IpcState.trace`).
- IPC commands: `list_llm_requests`, `get_llm_request`, `clear_llm_requests`,
  `get_trace_logging`, `set_trace_logging` (`src-tauri/src/ipc/trace.rs`). All
  five are async + `spawn_blocking`, so a wait on the shared trace locks never
  blocks the UI main thread. The UI polls `list_llm_requests` every
  1.5 s while the tab is open and re-fetches the selected detail on the same
  cadence, so an in-flight stream's response grows live.
