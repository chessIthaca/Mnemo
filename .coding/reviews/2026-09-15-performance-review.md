# Performance Review — myharness (2026-09-15)

**Perspective:** Performance: hot paths, scalability, blocking I/O, memory.
**Reviewer:** read-only performance review (conducted in-session).
**Grounding:** `src/provider/openai.rs`, `src/memory/mod.rs`, `src/agent/dispatch.rs`,
`src/tool/agent/sandbox.rs`, `src/tool/agent/shell.rs`, `src/tool/agent/git.rs`,
`src/tool/agent/mod.rs`, `src/runtime/mod.rs`, `src/agent/loop_impl.rs`,
`src-tauri/src/ipc/events.rs`.

---

## Overall verdict: **Performance-aware**

The architecture is already performance-aware in the high-traffic places:
connection-pooled `reqwest::Client`, SSE buffer drain (O(1) amortized, not
O(n²)), FTS5 pre-filter for memory recall, WAL-mode read/write split, batched
access-count updates, allocation-free ASCII-case-insensitive contains, output
capping, `spawn_blocking` for FS tools, and a per-chunk read timeout (not a total
timeout) on the SSE stream. The remaining findings are low-severity and mostly
documented trade-offs.

---

## Findings

### F1 — `is_project_scoped` does a blocking `canonicalize` on the async runtime (Low, documented)

**What:** `src/agent/approval.rs:114-129` `is_project_scoped` calls
`sandbox.validate(Path::new(path_str))` for file tools, which does a blocking
`canonicalize()` filesystem syscall. This runs in `dispatch.rs`'s
`needs_approval` call on the async runtime. It's one syscall per `NeedsApproval`
tool call under `AutoApproveProject` mode only.

**Assessment:** Documented trade-off (`approval.rs:104-113`): wrapping it in
`spawn_blocking` would require making `needs_approval` (and its callers) async
— a cross-cutting seam change. Low leverage (one syscall, infrequent, only under
one safety mode). The six heavy FS tools (`file_read`/`file_edit`/`file_write`/
`file_append`/`search`/`describe_image`) already wrap their entire blocking work
in `spawn_blocking` (`sandbox.rs:16-33`). This is the right call for the current
single-user scale.

**Recommendation direction:** If multi-agent concurrency under
`AutoApproveProject` becomes a goal, wrap the `is_project_scoped` call in
`spawn_blocking` at the dispatch site, or cache the canonicalized path per
`(tool_name, path_arg)` within a turn. Low priority.

### F2 — `AgentManager` mutex serializes all manager operations (Low)

**What:** See architecture finding A1. Every `send`/`list`/`get`/`set_running`/
`parent_id`/`main_agent_id`/`has_running_descendants` acquires
`Arc<tokio::sync::Mutex<AgentManager>>`. `has_running_descendants`
(`src/runtime/mod.rs:140-144`) walks all agents for every Run-All resolution
check.

**Impact:** Under multi-agent fan-out, this is the serialization bottleneck.
For a single-user harness with a handful of agents, negligible.

**Recommendation direction:** `DashMap<AgentId, AgentHandle>` for lock-free
reads + a separate mutex only for structural mutations. Low priority for current
scale.

### F3 — Memory recall: FTS pre-filter + capped fallback is well-tuned (Strength)

**What:** `src/memory/mod.rs:522-603` `recall`:
- Embeds the query once (`embedder_handle()` clones the `Arc`, no re-embed).
- FTS5 pre-filter reduces the cosine pass from O(N) to O(K) where K = keyword-
  matching candidates (`mod.rs:532-559`). Over-fetches top 50 for a good re-rank
  pool.
- When FTS is available but returns no matches → returns empty (no O(N) scan).
  When FTS is unavailable → capped full scan of 200 most-recent (`mod.rs:538`).
- `contains_ascii_ci` (`mod.rs:271-290`) is allocation-free (no per-memory
  `to_lowercase()`); `query.to_ascii_lowercase()` is hoisted once per recall
  (`mod.rs:566`).
- `batch_access` (`mod.rs:619-643`) collapses K sequential lock+query cycles
  into one parameterized `IN (...)` update.

**Assessment:** Strong. The recall path is the most-tuned hot path in the
codebase. The WAL read/write split (`mod.rs:110-116`, `read_conn`) means concurrent
recalls don't serialize behind the writer.

### F4 — SSE stream parsing: O(1) drain, no O(n²) re-copy (Strength)

**What:** `src/provider/openai.rs:851-884` `parse_sse_buffer` uses
`String::drain(..=pos)` to drop processed bytes in-place — O(1) amortized per
line instead of the O(n²) `buffer = buffer[pos+1..].to_string()` re-copy. The
stream loop enriches the `Usage` event with timing from the loop-owned
timestamps (`openai.rs:457-486`).

**Assessment:** Correct and efficient. Tests stress the drain path with 200
lines in one chunk (`openai.rs:1017-1036`). The shared `reqwest::Client`
(`openai.rs:79-99`) reuses the connection pool across all `complete()` calls,
avoiding the ~100-300ms TLS handshake per request.

### F5 — Per-chunk read timeout, not total timeout (Strength)

**What:** `src/provider/openai.rs:62-69` — `CONNECT_TIMEOUT` (30s) bounds the
handshake; there is deliberately **no total request timeout** (the SSE body is
long-lived — a reasoning model can think for a long time before the first token).
Instead, a per-chunk `READ_TIMEOUT` (90s) in the stream loop detects dead
connections without killing long-but-active generations (`openai.rs:405-436`).

**Assessment:** Correct. A total timeout would kill active streams mid-flight.
The per-chunk timeout bounds dead-connection detection. Reasoning models still
send keepalive chunks during thinking, so genuine silence means a broken
connection.

### F6 — Output capping prevents unbounded context (Strength)

**What:** `src/tool/agent/mod.rs:40-47` `cap_tool_output` caps at 100 KiB with
a truncation note. Applied by `shell` (`shell.rs:188`) and `git` (`git.rs:112`).
The shell tool preserves full raw data in the structured `data` field
(`shell.rs:197`) while capping the displayed `output`.

**Assessment:** Correct. Prevents a massive build log from blowing the context
window while keeping the structured data for the LLM.

### F7 — `spawn_blocking` for FS tools prevents runtime stalls (Strength)

**What:** `src/tool/agent/sandbox.rs:7-33` documents the contract: the six
async file/search tools wrap their entire blocking work (including `validate`)
in one `spawn_blocking` closure. The two remaining sync-path callers
(`is_project_scoped`, `shell::resolve_cwd`) each do a single `canonicalize` and
are kept sync by design (documented trade-off).

**Assessment:** Correct. Concurrent multi-agent tool use cannot stall the async
runtime on FS I/O.

### F8 — `complete_with_retry` backoff is fixed, not jittered (Low)

**What:** `src/agent/dispatch.rs:359-387` retries up to 3 times with fixed
exponential backoff (1s, 2s, 4s). No jitter.

**Impact:** Under a gateway-wide outage, multiple concurrent agents would retry
in lockstep (thundering herd). For a single-user harness with few agents, low
risk.

**Recommendation direction:** Add ±20% jitter to the backoff if multi-agent
concurrency against a shared gateway becomes common. Low priority.

---

## Strengths

1. **Connection pooling** — shared `reqwest::Client`, no per-request TLS handshake.
2. **SSE drain** — O(1) amortized, not O(n²).
3. **Memory recall** — FTS pre-filter + capped fallback + batched access + alloc-free contains.
4. **WAL read/write split** — concurrent recalls don't queue behind writes.
5. **Per-chunk read timeout** — bounds dead connections without killing active streams.
6. **Output capping** — prevents unbounded context.
7. **`spawn_blocking`** — FS tools don't stall the async runtime.

## Remediation order

1. **F1** (Low, documented) — wrap `is_project_scoped` in `spawn_blocking` only if multi-agent `AutoApproveProject` becomes a goal.
2. **F2** (Low) — DashMap for the agent registry if multi-agent scale becomes a goal.
3. **F8** (Low) — add backoff jitter if multi-agent concurrency against a shared gateway becomes common.
4. Everything else is a strength — no action required.
