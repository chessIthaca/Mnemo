# Executive Summary - 2027-01-09 Review Round
## Provider comms vs public docs | Performance & memory-loss | Cache hit rate 5 | Security

**Baseline:** HEAD c18b5d0 on wt/agenticcoding, clean tree. Four parallel read-only reviewers; reports in `.coding/reviews/` (file names dated 2026-09-09 by the system clock):

| Axis | Report | Verdict |
|---|---|---|
| Provider communications vs public docs | `2026-09-09-provider-comms-review.md` | FINDINGS (1 high, 8 low)* |
| Performance + memory-loss in agent communication | `2026-09-09-perf-comm-review.md` | FINDINGS (2 high, 6 low) |
| Cache hit rate round 5 (fresh log data) | `2026-09-09-cache-hit-5-review.md` | FINDINGS (1 high, 3 low) |
| Whole-app security | `2026-09-09-security-review.md` | FINDINGS (1 high, 3 low) |

\* The provider-comms verdict line counts 8 low but the report file details 4 findings (1 high + 3 low) - the file appears truncated at write time; the detailed findings are what this summary consolidates.

**Data basis:** `.coding/analysis/cache-hit-5-aggregates.txt` - 28,506 `request_stats` rows (2026-08-10..09-09 UTC, epoch clock; 15,713 post-R12 rows carry real provider-reported cached tokens), the traces.jsonl summary, and the 431-row provider-errors histogram. Extraction script: `.coding/analysis/cache-hit-5-extract.py`.

## 1. The headline

**Cache:** the honest post-R12 cache hit rate is **70.5%** - but that number is not our prompt shape. Split by whether the model group ever reports cached tokens: on **reporting** models the hit is **~81%** with ~17% resets; a **non-reporting / cache-hostile tier** (all five `glm-5.3-flash*` groups, `gemini-vlm-gcp`, `glm-5.2-maas-gcp`, `deepseek-v4-flash` - 15.9% of traffic, ~245M prompt tokens) reports **0% cached on every request** and supplies **53% of all resets** (~46% of all uncached tokens). The single biggest lever is provider-side (R18: get prefix caching enabled - or cached-token reporting fixed - on the flash tier; route long-context away from flash until then). The biggest ours-side leak is the **~340K litellm proxy cliff** - 47 post-R12 requests still crossed it at 16.3% hit (BUG 4).

**Performance:** the "fastest agent" goal currently has **no TTFT signal** - the metric measures a structurally ~0ms window on every current backend (BUG 2). The dominant end-to-end latency cost is server-side prefill of the ~1.4MB request body on the uncached prefix (47-86s connect_ms on the biggest turns) - the same lever as the cache work. During parallel run-all, the event forwarder awaits git-checkpoint/embedder/worktree dispatch work inline, freezing event delivery app-wide for seconds at a time (BUG 3).

**Provider comms:** the OpenAI-compatible request surface verifies clean against current public docs (`max_completion_tokens` + R9 quantized cap, `stream_options.include_usage`, current `{"type":"function"}` tool schema, `reasoning_effort` enum). The Anthropic path is contract-correct and **does** use prompt caching - but never parses the cache usage fields (BUG 6) and leaves the message history uncached (2 of 4 breakpoints; ~10x input-cost miss on that path). The known OPEN reasoning-leak bug is confirmed still open, with a precise leak path and a proposed stream-level heuristic fix (BUG 5). The glm-5.3-flash* zero-cache signal is **proxy-side, not ours** - request shape is provably identical across model groups and our parse site matches Zhipu's documented usage shape.

**Security:** one fresh HIGH - the agent file tools can write the **`.git/` control plane** (hooks, `core.fsmonitor`), turning a routine commit or an auto-approved `git status` into unsandboxed arbitrary code execution (BUG 1). Everything else verified solid at HEAD: strict CSP, sandboxed IPC, argv-based git, redaction, XSS-safe markdown, MCP isolation, backlog union-merge robustness.

## 2. Consolidated bug list (with explanations)

Ten bugs across the four reports, ranked by severity. Each: what happens (symptom), why (root cause), where, and the fix.

### BUG 1 - HIGH (security): `.git/` control plane writable by agent file tools -> sandbox escape
- **Where:** `src/tool/agent/sandbox.rs:149-190` (`is_protected_write_target` - `.git/` absent), `src/agent/approval.rs:124-139`, `src/tool/agent/git.rs:655-720`.
- **What happens:** a prompt-injected agent can `file_write` `.git/hooks/pre-commit` or `.git/config` (`core.fsmonitor = <command>`) - both are inside the project root, so the sandbox accepts them and `AutoApproveProject` auto-approves. The next routine commit - or an auto-approved read-only `git status` (fsmonitor chain, **zero approvals**) - executes the payload with full user privileges: API-key exfiltration from `~/.mnemo/keys.toml`, permanent safety-rule tampering.
- **Why:** the protected-write list covers `.coding/reviews/`, `.coding/plans/`, the DBs, `safety.toml`, `backlog.jsonl` - but not `.git/`, the git control plane. The git tool never passes `--no-verify`, so planted hooks run. An oversight, not a design decision.
- **Fix:** add `.git/` to `is_protected_write_target` (covers file_write/file_edit/convert_line_endings + the IPC mirror; also the `.git` plain file used by linked worktrees) + regression tests. Optional defense-in-depth: `--no-verify` on commit/merge.

### BUG 2 - HIGH (perf): TTFT is a dead metric - the anchor measures a structurally ~0ms window
- **Where:** `src/provider/openai/stream.rs:86-88` + Usage arm ~:278-330; `src/provider/anthropic.rs:~1460-1529`; anchor stamped in `spawn_stream` (`src/provider/openai.rs:683-757`).
- **What happens:** `ttft_ms=0` on 99.2% of rows (28,267/28,506). The metric cannot rank models/endpoints by first-token latency; the trace graph's Waiting bucket conflates upload with server prefill/queue.
- **Why:** `request_start` is stamped at **response-headers arrival**. LiteLLM and the direct providers only begin the HTTP response when the first token is ready, so headers and the first SSE chunk arrive in the same burst - the measured window is ~0ms by construction. The real first-token wait (upload + prefill + queue) lands entirely inside connect_ms.
- **Fix:** stamp ttft from POST-send initiation -> first chunk; keep connect_ms as the network/upload bucket. One anchor change per stream loop + test updates.

### BUG 3 - HIGH (perf, run-all): the event forwarder awaits run-all dispatch/landing work inline -> app-wide event blackouts
- **Where:** `src-tauri/src/ipc/events.rs:709-760` (resolution handlers awaited inline in the single event loop); the awaited work: `src-tauri/src/ipc/run_all.rs:3758+` (DISPATCH_LOCK across git checkpoint, embedder recall, worktree provisioning; LANDING_LOCK merges).
- **What happens:** when a lane/main turn resolves during a parallel run-all, the forwarder stops recv()-ing for the duration of git + embedder + worktree work - seconds to tens of seconds. The bounded fan-in channel (capacity 256) fills; every agent's sends block; mid-stream lanes stall; the UI shows nothing app-wide. No data is lost - everything freezes.
- **Why:** the single event-forwarder task is also the run-all dispatch driver; multi-second work runs inline on the event-delivery critical path under global locks. The locks are correct; the defect is who awaits them.
- **Fix:** `tokio::spawn` the resolution->dispatch continuation - the forwarder only needs the turn-resolve latch result, not the dispatch completion.

### BUG 4 - HIGH (cache): requests still cross the ~340K proxy cliff post-R13
- **Where:** `src/agent/turn.rs:1743-1750` (no send-gate), `src/agent/context.rs:437-485` (Rule-4 retreat), `:151-163` (trigger math).
- **What happens:** post-R12, 47 requests >340K prompt tokens at 16.3% hit (340-400K bucket: 83.7% resets); daily maxP reaches 408,956. R13's fill 0.3 + 340K ceiling did not prevent over-cliff prompts.
- **Why (three stacked gaps):** (1) **Rule-4 skip** - when the conversation ends in an open tool loop spanning back to messages[1] (exactly where context grows fastest), summarize returns messages unchanged and the request is sent at full size; (2) **no send-gate** - when compaction runs but cannot shrink below the threshold, up to 4 over-cliff requests go out per turn (the 5-attempt budget aborts only on the 5th; the next turn resets it); (3) **tokenizer drift** - the trigger counts cl100k tokens, the provider counts GLM tokens (~10% higher on code-heavy content), so a 300K-cl100k conversation can be 330-345K provider tokens, and one iteration's tool batch jumps past the trigger between checks.
- **Fix (R20):** (a) send-gate at the cliff - allow under-pressure historical-reasoning stripping for GLM as a cliff exception, or fail the turn fast instead of burning 4 over-cliff requests; (b) lower the effective trigger to ~280K for 1M-window endpoints; (c) surface a "skipped: open loop" signal from summarize so maybe_compact can fail fast with an actionable error.

### BUG 5 - HIGH (provider-comms, known OPEN): proxied DeepSeek/GLM reasoning leaks into the main chat
- **Where:** `src/provider/openai.rs:722` (the gate), `src/provider/openai/sse.rs` (text-delta branch).
- **What happens:** reasoning tokens from proxy-served DeepSeek/GLM render in the main chat window as assistant text, including literal think-tag markers. OPEN on main (e0ac5dd); the config-knob fix was rolled back 2027-01-09.
- **Why:** the gate is `matches!(self.config.kind, ProviderKind::Local)` - ThinkTagFilter only runs for Local-kind endpoints. Proxy deployments that inline reasoning in `choices[].delta.content` (vLLM/SGLang without a reasoning-field adapter) never send `reasoning_content`/`reasoning` deltas, so the parser routes them as TextDelta and the filter is skipped. The gate exists to avoid a false positive on OpenAI-kind gateways (a literal leading think-tag would be swallowed and rerouted), but it is the direct cause of the leak for inline-reasoning deployments.
- **Fix:** replace the static kind gate with a **stream-level heuristic**: enable think-tag extraction only when (a) the stream has emitted zero reasoning-field deltas so far AND (b) the first content delta opens with a think tag. Proper-reasoning-field models are unaffected by construction; inline-reasoning deployments get their tags stripped. The boundary-token escaping already prevents tool-result-borne tag tokens from triggering it.

### BUG 6 - LOW (provider-comms): Anthropic cache usage fields never parsed; cached_tokens hardcoded 0
- **Where:** `src/provider/anthropic.rs:731` (Usage event), `:588` (message_start); same class at `src/provider/openai/sse.rs:104` (Responses API path).
- **What happens:** Anthropic cache activity is invisible - traces/usage always show cached_tokens: 0 on the native path, and prompt_tokens under-reports the true context by the cached prefix (~5-10K tokens once the system+tools breakpoints are cached).
- **Why:** `message_start.usage` carries `input_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`; we parse only `input_tokens` - which per Anthropic semantics **excludes** cached tokens. The OpenAI path's `prompt_tokens` **includes** them, so the two paths feed different meanings into the same accounting.
- **Fix:** parse `cache_read_input_tokens` -> `cached_tokens` and `cache_creation_input_tokens`; record `prompt_tokens = input + cache_read + cache_creation` for parity. Mirror for `input_tokens_details.cached_tokens` on the Responses API path.

### BUG 7 - LOW (perf): user cancels misclassified as provider errors; cancelled requests invisible in stats
- **Where:** drop sites `src/provider/openai/stream.rs:414-446`, `src/provider/anthropic.rs:1608-1640`; trigger `src/agent/turn.rs:2254-2547`.
- **What happens:** 45+ rows in provider-errors.jsonl carry "stream aborted - consumer dropped" - actually the user-interrupt path (Interrupt/Cancel/Compact/Clear). The trace records are marked failed though the HTTP response was healthy; no request_stats row is written, so cancelled requests are invisible to latency/token accounting even though the provider billed the tokens generated so far.
- **Why:** when the turn loop drops the receiver mid-stream, the pump task treats the failed channel send identically to a provider failure - no distinction between user-initiated cancellation and a real stream error.
- **Fix:** a distinct `Cancelled` terminal class (excluded from provider-errors.jsonl or tagged), optionally a partial request_stats row with a cancelled flag.

### BUG 8 - LOW (cache): request_stats blind spots - error requests and the summarizer's own calls leave no row
- **Where:** `src/agent/turn.rs:~2440` (row written only in the Usage arm), `src/agent/context.rs:355-392` (summarize consumes its own stream, ignores Usage).
- **What happens:** error->retry cycles (each re-sends the full prompt; a LiteLLM fallback switch guarantees a cold next request) and compaction's own mega-prompt (up to ~300K tokens, guaranteed 0% cache) are invisible in the stats. Trace id 255's usage: None is exactly this path (BadGateway before any SSE chunk).
- **Why:** all recording hooks fire only on the success path.
- **Fix (R21):** write error rows with estimated prompt tokens + cached=NULL + outcome=error; forward the summarizer's Usage event with a tag; plumb ttft (BUG 2's fix covers the anchor).

### BUG 9 - LOW (cache): qwen-3.6 endpoint is dead - every request 400s
- **Where:** `src/config/endpoints.rs` (extra_body merged last, can override any request field); user config endpoints.toml.
- **What happens:** 11 all-time / 9 post-R12 status-400 "max_completion_tokens=131072 cannot be greater than max_model_len"; qwen-3.6 has zero request_stats rows - every request failed.
- **Why (hypothesis - user config not in repo):** the endpoint's extra_body sets max_completion_tokens=131072, overriding the R9-capped field after the merge (the cap itself is verified live).
- **Fix (R22):** remove the key from the endpoint's extra_body; optionally clamp/warn at the merge site.

### BUG 10 - LOW (provider-comms): policy.rs template_kwargs documented as must-send but never sent
- **Where:** `src/provider/policy.rs` (GLM `clear_thinking: false`, Qwen/Kimi `preserve_thinking: true`); no consumer in `src/provider/openai/request.rs`.
- **What happens:** doc comments assert vendor requirements, but no request builder reads template_kwargs - the params never leave the process. Doc/code drift + dead registry entries. Neither field name appears in current public vendor docs (Zhipu's documented params: do_sample, temperature, top_p, max_tokens, stream, thinking, reasoning_effort).
- **Why:** the registry predates the rolled-back config-driven policy layer; the wiring was never present at HEAD.
- **Fix:** wire the kwargs into the extra-body merge (behind the TBD vendor-policy direction) after verifying the field names against the proxy's accepted params - or delete them to stop the drift.

## 3. Non-bug findings (tightening opportunities)

- **Anthropic message history uncached** (provider-comms LOW): 2 of 4 breakpoints used (system head + last tool); the growing conversation re-bills at full input price (~10x input-cost miss on direct-Anthropic). Fix: a third breakpoint on the last message of the previous turn (+ optionally a fourth on the final block), or adopt automatic caching.
- **web_fetch DNS rebinding TOCTOU** (security LOW): the SSRF gate validates the resolved IP, but reqwest re-resolves at connect time - a rebinding DNS server can pass the gate and connect to 127.0.0.1/169.254.169.254. Fix: pin the validated IP (connect to IP + Host override + SNI) or a custom resolver re-running the gate per connection.
- **Shell residual** (security LOW, documented standing residual): an approved shell command can write protected dirs and `.git/` - inherent to shell approval; optional approval-UI warning badge when the command mentions `.coding/`, `safety.toml`, `.git/`.
- **API keys plaintext at rest** (security LOW): DACL-restricted on Windows, 0600 on Unix; OS keychain (Windows Credential Manager / macOS Keychain) would tighten. Worthwhile once BUG 1 is fixed.
- **Per-iteration O(history) request-rebuild** (perf LOW): raw-echo deep clone + token re-estimate + full re-serialization = ~25-60ms per iteration on 503-message conversations (3-8% overhead on fast cached turns). Fix: borrow-not-clone when the policy does not mutate; cache the serialized prefix; feed TokenAccounting's incremental estimate into the cap.
- **LlmTraceView polls the full trace every 1.5s while streaming** (perf LOW): up to 2MiB per fetch, ~40 fetches over a 60s generation. Fix: skip when the record's dirty-version is unchanged.
- **Trace mirror file rewritten whole per batch** (perf LOW): up to 8MiB redact+serialize+write per drain during long streams (off the critical path - dedicated thread). Fix: debounce to end-of-stream or per-record files.
- **Tool-result truncation is the one true information-loss path** (perf LOW): bounded and structured (keep 20 intact @ ~500 chars), but unrecoverable from context. Optional: append a file+line pointer on truncation so the agent can re-read cheaply.
- **Reset metric conflates "not reported" with "missed"** (cache LOW): 2,495 of 4,711 resets are rows from model groups that never report cached tokens. Fix (R19): split every aggregate by reporting vs non-reporting tier. **Shipped** (backlog fd8f67c0): the extraction emits every aggregate split by tier (.coding/analysis/cache-hit-5-aggregates.txt A0–A5) — post-R12 reporting tier is the prompt-shape baseline (79.8% hit / 18.1% resets on the regenerated window, 29,759 rows through 09-10), non-reporting 0%/100% by construction. The "ever cached_tokens>0 in the window" heuristic self-adapts: glm-5.3-flash-gcp-h200-fp8-01 started reporting (57.8% hit) and moved tiers automatically. 09-09's dip (41.8% combined, 51.9% reporting-tier; 82.8% next day at nonN=0) = flash-tier traffic + fallback-induced cold caches + over-cliff requests, not a prompt-shape regression.

## 4. Ranked top actions

1. **Fix the `.git/` sandbox escape** (BUG 1) - small effort, closes the worst hole; everything else in the security stack verified solid.
2. **Provider ticket: flash-tier prefix caching / cached-token reporting** (R18) - the single biggest cache lever (~46% of uncached tokens), zero code; route long-context away from flash until then.
3. **Close the 340K cliff leak** (BUG 4 / R20a-c) - send-gate + trigger margin + Rule-4 fast-fail signal.
4. **Fix the TTFT anchor + stats blind spots** (BUG 2 + BUG 8 / R21) - the "fastest agent" goal needs a real first-token signal; also unlocks the R18 disambiguation (real miss vs reporting gap).
5. **Un-inline run-all dispatch from the event forwarder** (BUG 3) - one tokio::spawn; removes app-wide blackouts during parallel run-all.
6. **ThinkTagFilter stream-level heuristic** (BUG 5) - the known OPEN leak, with a false-positive-free fix design ready.
7. **Anthropic: parse cache fields + cache the message history** (BUG 6 + the breakpoint finding) - ~10x input-cost miss on that path.
8. **Hygiene batch** (BUGs 7/9/10 + L5/L6 + R19/R22) - Cancelled class, qwen-3.6 config, template_kwargs cleanup, trace-view dirty-version skip, mirror debounce, tier-split aggregates.

## 5. Verified solid (no action needed)

**Security:** strict CSP + minimal capabilities; sandboxed IPC (~110 commands validated); browser child webview with zero capability grants; canonicalization-based path sandbox (symlinks, ADS, parent-canonicalization); five-mode approval architecture failing closed; argv-based git (no shell strings, no argument injection); redaction of keys/Bearer in every trace + error record; XSS-safe markdown (no rehype-raw, no dangerouslySetInnerHTML); MCP config outside the sandbox, env-var secrets, server->client requests declined; backlog union-merge robust to tampered lines. Prior findings verified holding: run-all commit-to-main guard (e20b1df), web_fetch redirect-hop SSRF gate.

**Performance:** fan-in backpressure by design (no silent drops); SSE pump terminal markers (no infinite polling); DeltaBatcher (16ms/64KiB, ordering preserved); run-all lane event routing (no cross-stamping); TokenAccounting incremental (no per-iteration recount in the agent loop); polling hygiene across views.

**Cache:** the cache-stable head design (byte-stable 20,301-char system head + 25 tool schemas); R9/R12/R13/R14/R15/R17 all live and doing their jobs; hysteresis tool-result compaction idempotent.

**Provider comms:** `max_completion_tokens` + R9 quantized cap, `stream_options.include_usage`, current tool schema, `reasoning_effort` enum - all verify against current OpenAI docs; the Anthropic path is contract-correct (version header, header-only workspace-id avoiding LiteLLM #29272, top-level system, required max_tokens, full SSE event set).