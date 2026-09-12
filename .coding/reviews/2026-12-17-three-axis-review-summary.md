## Verdict: FINDINGS (0 high, 0 medium, 5 low)

**Consolidated three-axis review of Mnemo at HEAD (`wt/agenticcoding`).** Zero critical/high across architecture, performance, and security. The codebase is well-engineered: the brain is cleanly decoupled from the Tauri shell, the workflow is an enforced state machine with defense-in-depth tool gating, the hot paths are correctly offloaded/cached/bounded, and the security model is defense-in-depth (path sandbox + argv execution + approval gate + secret redaction). The five LOW findings are incremental improvements, not defects. One cross-axis finding (repetition guard) is the single highest-leverage fix.

---

# Three-Axis Review — Mnemo at HEAD (`wt/agenticcoding`)

**Reviewer:** read-only review subagent (spawned for the "Full architecture, performance & security review" plan)
**Scope:** whole codebase at HEAD — `src/` (mnemo crate), `src-tauri/` (app shell + IPC), `frontend/` (React/TS). No code executed; findings reasoned from source with file:line evidence.
**Detailed reports:** this summary consolidates three axis-specific reports in the same directory:
- `2026-12-17-performance-review.md` (2 low)
- `2026-12-17-architecture-review.md` (2 low)
- `2026-12-17-security-review.md` (1 low)

## Findings — all five, ranked by leverage

### 1. [Perf F1 + Arch A1] LOW — Repetition guard is OpenAI-only, not in the shared streaming layer (cross-axis)

**Evidence:** The `LlmClient` trait (`src/provider/mod.rs:376`) defines a uniform streaming contract, and the shared `stream.rs` module houses cross-provider helpers (`DeltaAccumulator`, `ToolCallAccumulator`). But the repetition guard — `detect_repetition` (`src/provider/openai.rs:1590`), `bound_repetition_buffer` (`:1568`), constants `REPETITION_WINDOW=200`/`REPETITION_THRESHOLD=3`/`REPETITION_BUFFER_CAP=2048` (`:115,117,138`) — is **private to the OpenAI client**. The Anthropic provider's streaming loop (`src/provider/anthropic.rs:940`) accumulates `TextDelta` events with no repetition check (zero matches for the guard symbols in `anthropic.rs`). OpenAI is 4481 lines vs Anthropic's 1821 — the size gap reflects this feature asymmetry.

**Why it matters (both axes):**
- *Performance:* a Claude model that degenerates into a repetition loop streams indefinitely, wasting output tokens until `max_output_tokens` or a dead-connection timeout. The guard exists to abort this on the OpenAI path; its absence on Anthropic is an asymmetric efficiency gap.
- *Architecture:* the trait promises a uniform streaming interface, but stream-safety is a cross-cutting concern implemented as provider-specific code. A third provider would inherit the gap. The shared `stream.rs` is the natural home.

**Recommendation:** Lift `detect_repetition` + `bound_repetition_buffer` + the three constants into `stream.rs` as provider-agnostic free functions (or a small `RepetitionGuard` struct). Apply the guard in the Anthropic loop's `TextDelta` arm, mirroring the OpenAI call site (`openai.rs:1103-1137`). **This single change resolves both F1 and A1.**

### 2. [Perf F2] LOW — `spawn_consolidation` has no dedup guard (overlapping runs double-spend LLM tokens)

**Evidence:** The auto-continuation path fires `spawn_consolidation()` every `CONSOLIDATE_EVERY_N_TURNS` (= 8) auto-continues (`src/runtime/agent.rs:213-215`). `spawn_consolidation` (`:767-824`) is fire-and-forget (`tokio::spawn` at `:785`) with no "already running" flag. `consolidate_session` (`src/memory/consolidation.rs:107-121`) re-lists the working tier *live* before delegating to `consolidate_session_with_events` (`:133`), which makes 1–3 LLM calls then `delete_working_for_session`.

**Why it matters:** Consolidation takes 10–60s; 8 auto-continue turns can complete in a comparable window. If a second consolidation spawns before the first's `delete_working_for_session`, both list the same rows, both issue LLM synthesis/extraction calls, both write an episodic row — duplicate token spend + a redundant memory. Bounded to the overlap window (if the first finished, the second's list is empty and it no-ops). LOW: wasted tokens + a redundant memory row, not a stall or data loss.

**Recommendation:** Add an `AtomicBool` (or `tokio::sync::Mutex<()>` try-lock) guard in `AgentTask`/`AgentLoop`: `spawn_consolidation` returns early if a consolidation for this session is already in flight, clearing the flag in the spawned task's conclusion. Mirrors the `BUSY` AtomicBool already used by the IPC maintenance layer (`src-tauri/src/ipc/memory_maintenance.rs`).

### 3. [Arch AE1] LOW — String-based error classification (prior, re-verified, accepted trade-off)

**Evidence:** `src/error.rs:13-84` — the enum is still `Provider(String)`/`Memory(String)`/`Tool(String)`. `is_rate_limited()` matches "http 429"/"too many requests"/"rate limit"/"limit exhausted" case-insensitively on `Error::Provider` only. The 429 cross-provider fallback — a critical recovery path — depends on this string matching. A provider wording change silently breaks the fallback.

**Status:** Flagged HIGH in 2026-08-13, re-flagged in 2026-10-15, left as accepted trade-off. Partially mitigated: the `is_rate_limited()` classification layer (added with the 429-fallback feature) now distinguishes rate-limit vs auth vs transient, even though the underlying type is still `String`. The matching strings are narrow and unit-tested. Accepted twice; a typed sub-enum would be a large refactor for marginal gain.

### 4. [Sec SEC1] LOW — `web_fetch` has no SSRF protection (internal-IP / cloud-metadata reachable)

**Evidence:** `src/tool/agent/web_fetch.rs:135-145` validates the URL scheme (only `http://`/`https://` — blocks `file://`, `javascript:`, `data:`) but performs no host/IP validation. The tool is `SafetyLevel::AutoRun` (`:125-127`), so the model can fetch any http/https URL without approval. Fetched content (capped 50 KB) enters the conversation, which is sent to the configured LLM provider. No blocklist for loopback/private/link-local ranges — `http://127.0.0.1:PORT`, `http://169.254.169.254/latest/meta-data/` (cloud metadata), `http://localhost:9222/json` (the app's own CDP port) are all reachable.

**Why it matters:** Theoretical attack path: malicious fetched page contains prompt-injection → model fetches `http://169.254.169.254/...` → cloud-metadata (IAM credentials) enters the conversation → sent to the remote LLM provider, exfiltrating credentials. **LOW** for a local single-user desktop app (user is the trust boundary, watches the agent, output capped, cloud-metadata only reachable on a cloud VM). Real SSRF surface that a server-side/multi-tenant deployment would need to close.

**Recommendation:** Add an optional internal-IP blocklist: resolve the hostname, reject if it resolves to loopback (`127.0.0.0/8`, `::1`), link-local (`169.254.0.0/16`, `fe80::/10`), or private (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`) — unless an explicit allowlist permits it (e.g. local dev server). For the current desktop-only threat model, documenting the gap suffices.

### 5. [Accepted trade-offs] LOW — re-verified, still open by design

- **L1/L2 (perf):** Constitution reload (`src/project/agent_md.rs:84`) and safety-rules reload (`src/safety_rules.rs:264-265`) do a blocking `stat` on the async runtime every loop iteration / per tool call. Microseconds, OS-cached; documented inline. Accepted.
- **L4 (perf):** Heavy native deps (fastembed/ONNX, chromiumoxide, tree-sitter) not feature-gated (`Cargo.toml:52-59`). Documented deferral — deeply integrated into the tool registry/IPC/tool categories, not optional plugins. `debug = "line-tables-only"` mitigates link time. Accepted.
- **M2 (sec):** TOCTOU between sandbox `validate()` and the subsequent file write/read (`sandbox.rs:82-113`). Narrowed by running validate+use inside one `spawn_blocking` closure. Accepted for a local single-user desktop app (no adversarial local FS access assumed).


## Strengths verified (cross-axis)

**Architecture:**
- **Brain/shell decoupling is real** — `src/lib.rs` has zero Tauri imports; the IPC layer is an explicit channel adapter (`ipc/mod.rs:5-17`). The brain is fully testable without a GUI.
- **Enforced workflow gates (defense in depth)** — `ToolFilter` applied at schema-build (hides tools from the model) AND re-enforced at dispatch (`dispatch.rs:99-111`); review unskippable by construction (`finish` gated on non-empty review report).
- **Multi-agent-ready from day one** — `AgentLoopFactory` builds independent `AgentLoop`/`Workflow` per agent; `take_fanin_rx()` (`runtime/mod.rs:83-88`) avoids the manager-lock-while-waiting deadlock.
- **Documented lock-ordering invariant** in `IpcState` (`state.rs:42-47`); `TurnResolveLatch` for at-most-once terminal resolution.
- **Clean provider abstraction** — minimal async streaming trait + per-kind `Capabilities` + provider cache reusing `reqwest::Client` connection pools.

**Performance:**
- **Incremental token counting** — `TokenAccounting::update()` BPE-encodes only messages appended since the previous count; tiktoken BPE `OnceLock`-cached.
- **Memory recall off the async runtime** — FTS5 fetch + ONNX embedding inference in `spawn_blocking`; read path uses the read connection (no WAL serialization).
- **SSE streaming allocation-light** — single buffer with in-place `drain()`; bounded `mpsc::channel(128)` backpressure.
- **IPC delta batching bounded** — `DeltaBatcher` 64 KiB cap / 16 ms flush; deadline armed once per batch.
- **Auto-continuation hot path is clean** — streak counter is a `u32` increment, `hard_ceiling()` is one `saturating_sub`, `is_workflow_executing()` locks once per *turn*, consolidation is fire-and-forget on the blocking pool.

**Security:**
- **Path sandbox correct + comprehensive** — canonicalize + `starts_with(root)` + traversal/symlink rejection; single choke point for all file tools + IPC reads.
- **No shell injection anywhere** — `shell`/`git`/MCP stdio all use argv-based `Command`; MCP command comes from config, not the agent.
- **Flag-injection guards** — `valid_branch_name` rejects leading `-`/whitespace; `restore` uses mandatory `--` separator; extra `args` read-only.
- **Secret handling sound** — redacted `Debug`; atomic write + `restrict_permissions` (Unix `0600` / Windows user-only DACL with PROTECTED flag); 401/403 body suppression.
- **Protected-write-target deny-list complete + case-insensitive + ADS-guarded** — covers all live-state files; enforced before any FS mutation.
- **Reviewer-only authorship enforced by construction** — `write_review_report` is reviewer-only; `.coding/reviews/` sandbox-protected from file tools.
- **CDP debug port gated** — debug builds only (intentional); release requires opt-in `enable_browser_inspection` (defaults false) with a clear UI warning.

## Constitution checks (all axes)
- **Multi-platform neutrality:** All controls are platform-neutral or have proper `#[cfg(unix)]`/`#[cfg(windows)]` branches. The Windows-specific hardening (case-insensitivity, NTFS ADS guard, DACL permissions) is a no-op or harmless on Unix. The CDP port is Windows-only (WebView2), gated by `cfg(windows)` + opt-in setting. ✓
- **Warning-free build:** `#![deny(warnings)]` at both crate roots; no `#[allow(...)]` in examined files (the one `#[expect(dead_code, ...)]` in `shell.rs:97` is a documented frontend-consumed field). (Could not independently re-run `cargo test` — read-only reviewer.) ✓

## Recommendation to the parent agent

The five findings are all LOW and independent except for findings #1 (F1+A1), which share a root cause. **Recommended fix order:**

1. **Lift the repetition guard into `stream.rs`** (resolves Perf F1 + Arch A1 in one change — highest leverage).
2. **Add the `spawn_consolidation` dedup guard** (Perf F2 — small, isolated, prevents token double-spend).
3. **Document the `web_fetch` SSRF gap** (Sec SEC1 — a doc note suffices for the desktop threat model; add the IP blocklist if cloud-VM deployment is ever considered).
4. **AE1 + accepted trade-offs** — no action required; AE1 is mitigated by the classification layer, L1/L2/L4/M2 are documented accepted trade-offs.

None of the findings block shipping. The codebase is sound across all three axes.
