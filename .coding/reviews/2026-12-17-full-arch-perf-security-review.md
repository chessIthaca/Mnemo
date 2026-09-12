## Verdict: FINDINGS (0 high, 0 medium, 3 low)

**Three-axis review (architecture + performance + security) of Mnemo at HEAD (`bdd3b42` on `wt/agenticcoding`).** The codebase is structurally sound, performance-clean, and security-hardened. Zero high or medium findings across all three axes. Three unique LOW findings (one discovered independently by two reviewers from different angles). All prior findings from the 2026-10-15 architecture review and 2026-12-15 performance review are verified resolved or remain as documented accepted trade-offs. This is the first security review — no prior security findings to build on, and the security posture is strong.

---

# Full Architecture, Performance & Security Review — Mnemo at HEAD

**Date:** 2026-12-17
**Branch:** `wt/agenticcoding` (HEAD: `bdd3b42`)
**Scope:** Whole codebase — `src/` (mnemo brain crate), `src-tauri/` (Tauri app shell + IPC), `frontend/` (React/TS)
**Method:** Three parallel read-only reviewer subagents (architecture on glm-5.2, performance on qwen-3.6, security on deepseek-v4-flash), each with prior-review context. Reports synthesized below.

**Source reports:**
- `.coding/reviews/2026-12-17-architecture-review.md` — Verdict: FINDINGS (0 high, 0 medium, 2 low)
- `.coding/reviews/2026-12-17-performance-review.md` — Verdict: FINDINGS (0 high, 0 medium, 2 low)
- `.coding/reviews/2026-12-17-security-review.md` — Verdict: FINDINGS (0 high, 0 medium, 1 low)

---

## Combined findings

### Finding 1 — LOW: Repetition guard is provider-specific, not shared (abstraction leak + asymmetric streaming efficiency)

**Discovered by:** Architecture reviewer (A1) AND Performance reviewer (F1) — independently, from different angles.

**Evidence:** The `LlmClient` trait (`src/provider/mod.rs:376`) defines a uniform streaming contract — `complete() -> Result<BoxStream<'_, LlmEvent>>`. The shared `stream.rs` module (209 lines) houses cross-provider streaming helpers (`DeltaAccumulator`, `ToolCallAccumulator`). But the repetition guard — `detect_repetition` (`openai.rs:1590`), `bound_repetition_buffer` (`openai.rs:1568`), and constants `REPETITION_WINDOW=200` / `REPETITION_THRESHOLD=3` / `REPETITION_BUFFER_CAP=2048` (`openai.rs:115,117,138`) — are **private to the OpenAI client**. The Anthropic provider's streaming loop (`anthropic.rs:940`) accumulates `TextDelta` events with no repetition check (zero matches for `detect_repetition`/`bound_repetition_buffer`/`REPETITION` in `anthropic.rs`). Its only stream-termination guard is the per-chunk read timeout (`:943-975`), which fires only on a *dead* connection, not on an *active* connection stuck in a repetition loop.

**Why it matters:**
- *Architecture lens:* The trait promises a uniform streaming interface, but one implementation carries a safety feature the other lacks. Stream-safety is a cross-cutting concern (every provider can degenerate into a repetition loop), yet it's implemented as OpenAI-specific code. The shared `stream.rs` module is the natural home.
- *Performance lens:* A Claude model that degenerates into a repetition loop streams indefinitely, wasting output tokens until the model self-terminates or hits `max_output_tokens`. The `max_output_tokens` cap and read-timeout bound the worst case, but the guard exists for one provider family and not the other.

**Recommendation:** Lift `detect_repetition` + `bound_repetition_buffer` + the three constants into `stream.rs` as provider-agnostic free functions (or a small `RepetitionGuard` struct). Apply the guard in the Anthropic loop's `TextDelta` arm, mirroring the OpenAI call site (`openai.rs:1103-1137`). Future providers inherit it for free.

---

### Finding 2 — LOW: `spawn_consolidation` has no dedup guard — overlapping mid-session consolidations can double-spend LLM tokens

**Discovered by:** Performance reviewer (F2).

**Evidence:** The auto-continuation path fires `spawn_consolidation()` every `CONSOLIDATE_EVERY_N_TURNS` (= 8) auto-continues (`src/runtime/agent.rs:213-215`). `spawn_consolidation` (`:767-824`) is fire-and-forget (`tokio::spawn` at `:785`) with no "already running" flag — it unconditionally spawns a new task each call. `consolidate_session` (`src/memory/consolidation.rs:107-121`) re-lists the working tier *live* (not from a snapshot) before delegating to `consolidate_session_with_events` (`:133`), which makes 1–3 LLM calls and then `delete_working_for_session`. If a second consolidation spawns before the first has run `delete_working_for_session`, both list the same working rows, both issue LLM synthesis/extraction calls, and both write an episodic row — duplicate LLM token spend + a duplicate episodic memory. If the first has finished, the second's list is empty and no-ops cheaply, so the worst case is bounded to the overlap window.

**Why it matters:** Consolidation makes LLM calls that can take 10–60s; 8 auto-continue turns can complete in a comparable window. LOW because the consequence is wasted tokens + a redundant memory row, not a stall or data loss.

**Recommendation:** Add an `AtomicBool` (or `tokio::sync::Mutex<()>` try-lock) guard in `AgentTask` / `AgentLoop`: `spawn_consolidation` returns early if a consolidation for this session is already in flight, clearing the flag in the spawned task's conclusion. Mirrors the `BUSY` AtomicBool already used by the IPC maintenance layer (`src-tauri/src/ipc/memory_maintenance.rs`).

---

### Finding 3 — LOW: `web_fetch` has no SSRF protection (internal-IP / cloud-metadata endpoints reachable)

**Discovered by:** Security reviewer (SEC1). First security review — no prior finding.

**Evidence:** `src/tool/agent/web_fetch.rs:135-145` validates the URL scheme (only `http://`/`https://` — blocks `file://`, `javascript:`, `data:`), but performs no host/IP validation. The tool is `SafetyLevel::AutoRun` (`:125-127`), so the model can fetch any http/https URL without approval. The fetched content (capped at 50 KB) enters the conversation, which is sent to the configured LLM provider. There is no blocklist for link-local / loopback / private ranges — `http://127.0.0.1:PORT`, `http://169.254.169.254/latest/meta-data/` (AWS/GCP cloud metadata), `http://localhost:9222/json` (the app's own CDP port), or `http://10.x.x.x` are all reachable.

**Why it matters:** Theoretical attack path: a malicious fetched webpage contains prompt-injection instructions → the model is convinced to fetch `http://169.254.169.254/...` → the cloud-metadata response (IAM credentials) enters the conversation → the conversation is sent to the remote LLM provider, exfiltrating credentials. LOW for a local single-user desktop app where (a) the user is the trust boundary, (b) the model must be prompt-injected, (c) output is capped at 50 KB, and (d) cloud-metadata endpoints are only reachable on a cloud VM. But it is a real SSRF surface that a server-side or multi-tenant deployment would need to close.

**Recommendation:** Add an optional internal-IP blocklist to `web_fetch`: resolve the hostname and reject if it resolves to loopback (`127.0.0.0/8`, `::1`), link-local (`169.254.0.0/16`, `fe80::/10`), or private (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`) range — unless an explicit allowlist permits it. For the current desktop-only threat model, documenting the gap is sufficient; the blocklist becomes important if the app is ever deployed on a cloud VM.

---

## Prior findings — verification summary

### Architecture (from 2026-10-15 review)

| # | Finding | Status at HEAD |
|---|---------|---------------|
| M1 | `events.rs` carried domain orchestration | **RESOLVED** — adapter extracted in remediation sweep (merged f680b88) |
| M2 | `settings.rs` god-module | **RESOLVED** — SettingsAssembler extracted in remediation sweep |
| L1 | Error type uses String buckets with substring matching | **STILL-OPEN** (accepted trade-off, downgraded to LOW, partially mitigated by `is_rate_limited()` classification) |
| L2 | Core lib files exceed 2000 lines | **PARTIALLY RESOLVED** — memory/mod.rs split; other large files remain (accepted) |
| L3 | AgentLoop field count ~25 | **RESOLVED** — `AgentLoopConfig` struct introduced |

### Performance (from 2026-12-15 review)

| # | Finding | Status at HEAD |
|---|---------|---------------|
| M1 | Codegraph re-reads every file every pass | **RESOLVED** — mtime fast-path at `codegraph/mod.rs:271-294` |
| L1 | Constitution reload blocking stat per turn | **STILL-OPEN** (accepted — micro-cost, OS-cached) |
| L2 | Safety-rules reload blocking stat per tool call | **STILL-OPEN** (accepted — micro-cost) |
| L3 | `build_stable_head` rebuilds String every turn | **RESOLVED** — cached in `Mutex<Option<String>>` at `loop_impl.rs:273-289` |
| L4 | Heavy native deps not feature-gated | **STILL-OPEN** (deferred, documented in Cargo.toml) |
| L5 | R10 repetition-guard buffer unbounded | **RESOLVED** — bounded at 2048 bytes via `bound_repetition_buffer` |

### Security (first review — no prior findings)

| # | Finding | Status at HEAD |
|---|---------|---------------|
| M1 (2026-08-13) | Protected-write-target deny-list case-sensitive | **RESOLVED** — case-insensitive via `to_ascii_lowercase()` at `sandbox.rs:195-198` |
| ADS (2026-04-18) | NTFS ADS bypasses protected check | **RESOLVED** — rejects `:` in final component at `sandbox.rs:207-211` |
| CDP-HIGH (2026-11-22) | Unauthenticated CDP debug port | **MITIGATED** — opt-in setting, defaults off, UI warning |

---

## Strengths (verified across all three axes)

**Architecture:**
- Brain/shell decoupling is real — `src/lib.rs` has zero `tauri` imports; IPC layer is an explicit adapter over the channel contract
- Enforced workflow gates (defense in depth) — `ToolFilter` applied at schema-build AND dispatch-time re-check
- Multi-agent-ready from day one — `AgentLoopFactory` builds fresh `AgentLoop` per agent; `take_fanin_rx()` avoids manager-lock-while-waiting deadlock
- Clean provider abstraction — minimal async trait, `Capabilities` per-kind with config overrides
- Documented lock-ordering invariant in `IpcState`
- At-most-once terminal resolution via `TurnResolveLatch`

**Performance:**
- Incremental token counting (`TokenAccounting::update`) — BPE-encodes only new messages
- Memory recall off async runtime (`spawn_blocking` for FTS5 + ONNX)
- SSE streaming allocation-light (single buffer with in-place `drain()`)
- IPC delta batching bounded (`DeltaBatcher`, 64 KiB cap, 16ms flush)
- Trace log bounded (ring buffer, `MAX_RECORDS=32`)
- No lock held across `.await`
- Frontend hot path memoized (`React.memo`, rAF-batched flushing)
- Provider cache reuses `reqwest::Client` connection pools across turns
- Auto-continuation hot path is clean: `u32` increment, single integer compare, one `Mutex` lock per turn (not per tool-call), one small `Message` allocation per auto-continue

**Security:**
- Path sandbox is correct and comprehensive — canonicalize + `starts_with(root)` + traversal/symlink rejection, single choke point for all file tools
- Protected-write-target deny-list is case-insensitive + ADS-guarded, covering all live-state files
- No shell injection anywhere — argv-based execution (shell, git, MCP stdio), no `format!`-into-shell-string
- Flag-injection guards — `valid_branch_name` rejects leading `-` + whitespace, `--` separator before pathspecs
- Core-operation invariant — git merge/push always prompt, even under Autonomous, enforced via `Tool::never_auto_for`
- Secret handling sound — redacted `Debug`, atomic temp-file + rename, restrictive permissions (Unix `0600` / Windows user-only DACL with PROTECTED flag)
- Reviewer-only authorship enforced by construction — `write_review_report` is reviewer-only, `.coding/reviews/` is sandbox-protected
- MCP OAuth token store is origin-bound (exact `scheme://host[:port]` match)
- CDP debug port gated (debug-only or opt-in setting with UI warning)

---

## Constitution checks

- **Documentation sync:** Auto-continuation doc comments accurately describe behavior (verified in the prior remediation review). README documents `[context]` compaction knobs. PLAN.md reflects the working-branch topology. ✓
- **Multi-platform neutrality:** All three axes confirm platform neutrality. The brain uses no platform-specific APIs. The one Windows-only surface (WebView2 browser tab / `game_*` tooling) is gated with `cfg(windows)`. Security controls (sandbox canonicalization, `restrict_permissions`) have both `#[cfg(unix)]` and `#[cfg(windows)]` branches. ✓
- **Warning-free build:** `#![deny(warnings)]` at both crate roots. No `#[allow(...)]` observed in any examined file. Green `cargo test` (1758 tests, exit=0) proves zero warnings. ✓

---

## Summary

The Mnemo codebase is well-engineered across all three axes. The architecture is sound: a fully decoupled brain speaking channels, adapted to Tauri IPC by a thin shell, with an enforced workflow state machine, defense-in-depth tool gating, and a clean multi-agent runtime. The performance posture is strong: the auto-continuation feature adds negligible overhead (a `u32` increment, one integer compare, one cheap `Mutex` lock per turn), three of six prior performance findings are now resolved, and no new bottlenecks were introduced. The security model is defense-in-depth: a correct path sandbox, argv-based command execution (no shell injection), flag-injection guards, a hard core-operation invariant, sound secret handling, and a protected review-gate mechanism. The first-ever security audit found no critical or high-severity issues.

The three LOW findings are incremental improvements, not structural defects: (1) lift the repetition guard into the shared streaming layer so all providers benefit, (2) add a dedup guard to mid-session consolidation to prevent double-spending LLM tokens, and (3) add SSRF protection to `web_fetch` for defense-in-depth against prompt-injection-driven exfiltration. None are blockers for the current desktop-only, single-user threat model.
