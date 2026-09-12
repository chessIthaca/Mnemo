# Whole-App Review — myharness (consolidated)

**Date:** 2026-08-13
**Scope:** full codebase at HEAD — `src/` (Rust lib: agent, tools, provider, workflow, memory, browser, config, safety), `src-tauri/` (Tauri 2 shell + IPC), `frontend/` (React/zustand), build config (Cargo workspace, vite, tauri.conf.json).
**Method:** three read-only review passes (quality+architecture, security, performance+build+memory) plus main-agent consolidation. No code executed; findings reasoned from source. All three per-area reports are independent sub-agent reviews (the quality/arch one was re-run time-boxed after an initial stream-abort lost the first attempt; the main agent wrote a fallback that was superseded by the sub-agent's report).
**Per-area source reports:**
- `.coding/reviews/2026-08-13-app-review-quality-arch.md`
- `.coding/reviews/2026-08-13-app-review-security.md`
- `.coding/reviews/2026-08-13-app-review-perf-build-memory.md`

## Summary verdict table

| Area | Critical | High | Medium | Low | Verdict |
|---|---|---|---|---|---|
| Quality & architecture | 0 | 2 (Q-H1–H3) | 5 (Q-M1–M5) | 5 (Q-L1–L5) | Strong architecture; size concentration + error-enum nits |
| Security | 0 | 2 (H1–H2) | 4 (M1–M4) | 3 (L1–L3) | Well-hardened; 2 cheap High fixes |
| Performance | 0 | 0 | 2 (R1–R2) | 2 (R3–R4) | Hot paths well-optimized |
| Build | 0 | 2 (B1–B2) | 0 | 2 (B3–B4) | Two dead heavyweight deps |
| Memory | 0 | 0 | 1 (M1) | 1 (M2) | Well-bounded |
| **Total** | **0** | **6** | **12** | **13** | **No critical issues; six cheap-to-moderate High fixes** |

**Headline:** the codebase is in good shape. No Critical findings. Six Highs, all cheap-to-moderate: two **dead build dependencies** (`syntect`, `markdown`), two **security hardening gaps** (approval-gated `describe_image` exfiltration channel; plaintext, world-readable `.coding/logs/*.jsonl`), and two **quality/architecture** items (`ipc/settings.rs` god-module at 1750 lines; error enum that is typed in name only — string-bucket variants). Everything else is Medium/Low.

---

## HIGH

### Build

- **B1 — `syntect = "5"` declared in `Cargo.toml:20` but never used.** Zero `syntect::` references in Rust source; highlighting is done frontend-side (`rehype-highlight`, `Message.tsx:110`). Pulls in embedded grammar tables + regex machinery. Adds tens of seconds to clean builds + binary bloat.
  **Fix:** delete the dep; `cargo update` to drop from lockfile.
- **B2 — `markdown = "1"` declared in `Cargo.toml:48` but never used.** No `mod markdown` in the lib; plan parsing is hand-rolled (`workflow/plan_file.rs`), rendering is `react-markdown` frontend-side. The only "markdown" hits are the unrelated `[markdown]` config section.
  **Fix:** delete the dep.

### Quality & architecture

- **Q-H1 — `settings.rs` is a god-module in progress.** `src-tauri/src/ipc/settings.rs:1` (1750 lines; the header alone claims config + endpoints + keys + pricing + settings patch + live model listing + runtime rewires). One file owning seven concerns invites merge conflicts and review fatigue. **Fix:** split along the header's own seams (endpoints.rs, keys.rs, pricing.rs, rewire.rs).
- **Q-H2 — `useAgentStore.ts` facade at 958 lines is still large.** `frontend/src/hooks/useAgentStore.ts:1`. The facade split is the right design (pure reducers in `agentEventReducer.ts`, appearance in `appearance.ts`), but 958 lines of store composition + re-exports remains the biggest frontend file. **Fix:** acceptable as-is; if it grows, extract the steer/backlog action block into its own hook module.
- **Q-H3 — Error variants are string buckets.** `src/error.rs:11-36`. `Config/Project/Provider/Tool/Workflow/Memory/Safety/Runtime/Browser` are all `(String)` newtypes, so `match` can discriminate only by subsystem, never by failure kind; callers can't distinguish e.g. "no plan exists" from "update outside Executing" (both `Workflow(String)`). **Fix:** promote the few most-branched-on cases (Workflow no-plan / wrong-state, NotFound vs InvalidInput) to struct or dedicated variants.

### Security

- **H1 — `describe_image` is an un-gated file→network exfiltration channel.** `src/tool/agent/describe_image.rs:52-70` reads any sandbox file → base64 data-URL → POSTs to the vision endpoint (`src/provider/vision.rs:139-146`). It is `SafetyLevel::AutoRun` (`describe_image.rs:178-181`), and the gate (`mime_from_ext`) checks only the **extension**, not content — so a project file named `.png` (agent's own `file_write` can create it) is exfiltratable to a third-party vision endpoint with **no approval**, even under ApproveEachAction.
  **Fix:** gate `describe_image` behind `NeedsApproval` (or project-scoped auto-approve), and/or verify magic bytes match the claimed MIME before sending.
- **H2 — `provider-errors.jsonl` (always-on) and `traces.jsonl` (opt-in) persist full request/response bodies to plaintext, non-restricted `.coding/logs/`.** `src/provider/trace.rs:345-385` (always-on error log, no enable flag), `:448-487` (write_record_to_file); paths at `src-tauri/src/main.rs:500-512`. Request bodies routinely embed file contents / tool outputs / pasted secrets; unlike `keys.toml` (user-only DACL), these logs get **no permission restriction** — readable by any local user on a shared machine.
  **Fix:** apply the same `restrict_permissions` used for `keys.toml` to both log files at creation; consider redacting `Authorization`-shaped strings / `api_key` fields from persisted bodies.

---

## MEDIUM

### Quality & architecture

- **Q-M1 — Every IPC command flattens errors to `String`.** `src-tauri/src/ipc/settings.rs:27`, `files.rs:24` (`Result<T, String>`). The typed `Error` enum is lost at the boundary; the frontend can only display, never branch (e.g. can't distinguish validation from IO error to offer different UX). **Fix:** return a serializable `{ kind, message }` error DTO for commands where the frontend could react differently.
- **Q-M2 — `never_auto()` is dead weight.** `src/tool/mod.rs:126-128`. The doc itself says "Today no tool overrides this"; the blanket invariant lives in `never_auto_for`. Kept "for future tools" — speculative generality. **Fix:** remove it and re-add when a real blanket-prompt tool appears, or add a `#[cfg(test)]`-only override to keep it honest.
- **Q-M3 — `ToolFilter::from_state` maps `Skill` → `Planning` as a "defensive default".** `src/tool/mod.rs:197`. A state whose correct filter must always come from elsewhere silently degrades to a wrong-but-plausible filter if a future caller forgets. **Fix:** make `from_state` non-exhaustive for `Skill` (return `Option` or panic/unreachable with a clear message) so misuse fails loudly.
- **Q-M4 — Dual `kind` parsing paths can drift.** `src-tauri/src/ipc/settings.rs:116-118` accepts both serde ("openai") and Debug ("OpenAI") forms; `frontend/src/components/settings/types.ts:131-134` (`kindFromConfig`) duplicates the same normalization on the TS side. Two normalizers for one legacy payload shape. **Fix:** emit only the serde form from `get_config` (typed; the fixture test would catch it) and delete both lenient paths.
- **Q-M5 — Unsanitized sidecar parse fallback.** `src/workflow/mod.rs:699`: `serde_json::from_str(&text).unwrap_or_default()` on the legacy bare-array form silently treats an unparseable sidecar as an empty stack (masking corruption as "no plan"). **Fix:** log a warning (or surface a recoverable error) when the sidecar exists but fails both parses.

### Security

- **M1 — `is_protected_write_target` is case-sensitive and ASCII-only; a case-variant path bypasses it on Windows.** `src/tool/agent/sandbox.rs:170-180` compares with `==` against `.coding/memory.db`, `.coding/safety.toml`, etc. — NTFS is case-insensitive, so `.coding/SAFETY.TOML` resolves to the real protected file and defeats the guard.
  **Fix:** lowercase (`to_ascii_lowercase`) the relative path before comparison on Windows.
- **M2 — TOCTOU window between sandbox `validate()` and the subsequent file write/read.** `sandbox.rs:78-109` canonicalizes + checks `starts_with(root)` and returns a path that is used later (after an await/`spawn_blocking` boundary). A local attacker swapping a dir for a symlink between check and use could redirect the write outside the root. Low likelihood for a single-user dev box; the tools already do validate+use inside one closure, which narrows but does not close the race.
  **Fix:** accept as documented low risk; if raised, re-canonicalize immediately before the syscall in the same closure.
- **M3 — `data:` is allow-listed, and `browser_eval` on a `data:` page runs JS with no origin, gated only `NeedsApproval` (not `never_auto`).** `src/browser/mod.rs:85` (ALLOWED_SCHEMES), `:418-431` (eval); `src/tool/browser/mod.rs:171`. Under AutoApproveProject/Autonomous a broad safety rule could auto-approve `eval`. Bounded by the Chromium sandbox, but questionable in production.
  **Fix:** ensure `browser_eval` is never auto-approved by a broad safety rule; reconsider `data:` in the allow-list (or restrict to test builds).
- **M4 — `get_api_keys` returns all API keys to the frontend over one IPC call.** `src-tauri/src/ipc/settings.rs:155-167`; held in React state (`ProvidersSection.tsx:34,93` — correctly cleared on deactivate at `:93`). Same-origin webview, not a remote hole, but the full key map lives in JS memory while the section is active.
  **Fix:** return keys only for the endpoint being edited, or masked values; keep the clear-on-deactivate behavior.

### Performance

- **R1 — `cl100k_base()` BPE tokenizer re-initialized on every exact `count_tokens`** (`agent/context.rs:246`, called from `turn.rs:142-217`). **Fix:** cache in a `OnceLock`/`Lazy`.
- **R2 — Memory-store rusqlite calls run on the async runtime without `spawn_blocking`** (`memory/mod.rs` recall :522, write :502, stats :743-861). Sub-ms today; tail-latency risk as store grows. **Fix:** `spawn_blocking` (contrast `describe_image.rs:107` which does it right).

### Memory

- **M1 — `sweep_stale_profiles` does blocking `read_dir` + `remove_dir_all` of the temp dir under the browser state lock on every spawn** (`browser/mod.rs:201-220`, called from `ensure_browser` :168). **Fix:** `spawn_blocking` + outside the lock.

---

## LOW

- **Q-L1** — `execute(&self, args: Value)` clones arguments at dispatch (`src/tool/mod.rs:385`, `call.arguments.clone()`). Minor cost per call; harmless at this scale.
- **Q-L2** — `ToolRegistry::schemas` iterates a HashMap then sorts (`src/tool/mod.rs:355-379`). Correct and well-justified (prompt-cache stability); note: the sort must not be "optimized" away.
- **Q-L3** — Legacy mtime fallback uses `?`-in-`ok()` chains (`src/workflow/mod.rs:762-778`): `entry.ok()?` aborts the whole scan on one bad dir entry, returning `None`. Benign (legacy path only); `filter_map` would be more robust.
- **Q-L4** — `makeUid` Math.random fallback (`frontend/src/components/settings/types.ts:105-110`). Fine for React keys; ensure it's never reused for anything security-adjacent.
- **Q-L5** — Double blank line (`frontend/src/components/settings/types.ts:224-225`). Cosmetic.
- **L1 — reqwest client follows redirects by default; a cross-host redirect could carry `Authorization` to the redirected host** (`src/provider/openai.rs:99-119`, auth at :394). reqwest strips sensitive headers on cross-host redirects in current versions, but it's implicit. **Fix:** explicit `.redirect(Policy::none())` or same-host policy.
- **L2 — Shell tool's `data.stdout`/`stderr` are uncapped** (only the display string is capped; `src/tool/agent/shell.rs:186-197`) — full bytes flow into the LLM context. **Fix:** cap `stdout`/`stderr` before placing in `data`.
- **L3 — `browse_markdown_file` is intentionally unsandboxed (native picker)** — user-driven, not agent-reachable; confirmed by design, no finding beyond noting the boundary is sound.
- **R3** — `navigate` holds the state lock across `Browser::launch().await` (`browser/mod.rs:235-239`) — first-use/respawn only; acceptable, note for later.
- **R4** — Screenshot base64-encodes inline per Browser-tab refresh (user-initiated, not polled) — fine.
- **B3** — `reqwest` enables `gzip`/`brotli`/`deflate` for an LLM/embeddings client that rarely sees compressed bodies. Audit + trim.
- **B4** — Frontend single ~1 MB+ bundle; `chunkSizeWarningLimit: 1500` suppresses instead of splitting. Acceptable for local Tauri load; optional lazy-load of markdown/highlight stack.
- **M2 (perf)** — `streamingText` concat is O(n²) per flush, mitigated by rAF batching + JS ropes. Micro-opt; join-once if profiling shows it.

---

## Clean areas (explicitly verified, no findings)

- **Path sandbox** (`sandbox.rs`): canonicalize + `starts_with`; symlink/traversal/drive-letter/UNC safe; `validate_for_creation` closes the non-existent-parent gap; `normalize_path` refuses to pop past root. Single choke point for tools + IPC. (M1 case-sensitivity + M2 TOCTOU are the only caveats.)
- **Browser URL allow-list**: single choke point for `browser_navigate` + `browser_open`; `file:`/`javascript:`/`about:` cannot reach Chromium; `--disable-extensions` set.
- **git tool**: fixed argv (never shell string); `valid_branch_name` rejects leading `-`/whitespace (flag-injection guard); `merge`/`push` `never_auto_for` — enforced before the safety-rule shortcut in `dispatch.rs:193-210`, so a stored rule can never auto-approve them.
- **Approval gate**: `force_prompt` checked before safety-rule shortcut (`dispatch.rs:201`); `shell` is never `is_project_scoped`; AutoApproveProject only covers in-sandbox file tools + read-only git. No bypass found.
- **Safety-rules classifier** (`safety_rules/cmd_class.rs`): fail-safe — chaining, unknown primaries, assignments → `None` → prompt; `rm`/`Remove-Item` absent from allow-lists.
- **keys.toml**: no `Debug` on values; atomic temp+rename write; user-only DACL on Windows / 0600 on Unix; permission-failure is warn-not-block (acceptable).
- **Secret leakage in provider errors**: `fetch_models` omits the response body on 401/403 (can't echo the Bearer key into the UI); traces record host-only base_url. (H2 covers persisted request *bodies*.)
- **IPC validation**: all file commands sandbox-validate; `browser_open` funnels through `normalize_url`; `save_settings`/`save_endpoints` validate range/enum/uniqueness/cross-refs; `backlog_*` take only ids/text/images, no paths. No unvalidated path/URL arg found.
- **Webview hardening**: prod CSP strict (`script-src 'self'`, `object-src 'none'`, `connect-src` limited); permissive CSP is `devCsp` only; capabilities grant `shell:allow-open` but NOT `shell:allow-execute`.
- **SQL injection**: rusqlite bound params everywhere; FTS MATCH built by quoting/escaping tokens and passed bound; dynamic `IN` interpolates only `?N` placeholders.
- **Temp files**: `tempfile::TempDir` random suffix; stale sweep only removes `myharness-browser-*` >1h; no zip extraction anywhere (settings import is single JSON, validated twice).
- **Console ring buffer** (capped 500) + bounded broadcast (512, lagging subscribers dropped) — no leak.
- **Frontend streaming**: rAF-batched flushes, throttled scroll, markdown parsed once on finalize — correctly done.
- **Trace ring bounded** (32 × 2 MiB); context growth managed by `summarize_at_fill_rate`; backlog persisted atomically.
- **No spawned-task leaks**: agent loops removed on `Exited`; console forwarders aborted on close; stale subagents cancelled on fresh plan.
- **Process spawning**: `shell`/`git` use `tokio::process::Command` (async) with `kill_on_drop` + timeout + `CREATE_NO_WINDOW`; `std::process::Command` only in test helpers.
- **Tool trait / plan lifecycle / three-layer architecture**: coherent, documented, enforced (the sub-agent gate we hit during this review is the lifecycle working as designed). The quality/arch reviewer verified: plan-first state machine enforced (not prompt-level), `reviewed` flag unskippable-review-by-construction, sub-plan push/pop with parent resume clean, IPC state decomposed into runtime/project/backlog contexts with documented lock-ordering, secrets travel in a separate `api_keys` map (never in DTOs), schemas sorted by name with a test pinning it.
- **Doc comments** on public items per constitution (verified across all nine files the quality/arch reviewer read — "why" rationale included); strong unit + golden-fixture test coverage.

---

## Recommended action order (impact-ordered)

1. **Remove `syntect` + `markdown` deps** (B1, B2) — cheapest, biggest build win.
2. **Gate `describe_image` behind approval + content-sniff MIME** (H1) — closes the AutoRun exfiltration channel.
3. **Apply keys.toml-style permission restriction to `.coding/logs/*.jsonl`; redact secrets from persisted bodies** (H2).
4. **Cache the tiktoken BPE tokenizer** (R1) — stop re-decoding per exact count.
5. **Case-insensitive `is_protected_write_target` on Windows** (M1-security) — cheap correctness fix to a guard.
6. **Promote error-enum string buckets to structured variants** (Q-H3) — unlocks typed IPC errors (Q-M1) and branchable frontend UX.
7. **`spawn_blocking` for memory-store + IPC file reads** (R2, S4-equivalent) — future-proofs the async runtime.
8. **Move the browser temp sweep off the locked async path** (M1-perf).
9. **Explicit reqwest redirect policy; cap shell `data.stdout/stderr`** (L1, L2).
10. **Split `ipc/settings.rs`** (Q-H1) and **`useAgentStore`** (Q-H2) — maintainability; `never_auto()` removal + `ToolFilter` Skill default (Q-M2/M3) — small cleanups.
11. **Provenance-mark browser-derived content** — prompt-injection hygiene (action-layer gating already mitigates).
