# Deep Review 2026-04-18 — Consolidated Report (security · architecture · maintainability)

**Scope:** whole codebase at HEAD (main, post startup-context-caps merge). **Method:** three parallel read-only deep reviewers (security / maintainability / architecture) + an in-session verification pass that confirmed every High/Medium finding against the code before accepting it. Detail lives in the three area reports (`.coding/reviews/2026-04-18-deep-review-{security,maintainability,architecture}.md`); this document is the prioritized merge.

---

## Verdict

The three-layer architecture (Tauri-free lib / thin IPC adapter / React frontend) is **sound and drifting slowly**; the security posture is **strong overall but has 2 real holes**, both in exactly the escalation class the design exists to prevent. Maintainability debt is concentrated and worth **≈ −1,000 LOC** of safe reduction. All prior "known-fixed" security items were re-verified as holding at HEAD (one partial — see M2), and one previously-recorded finding was **downgraded** (shell `data` never reaches LLM context — the old framing was factually wrong).

**Recommended remediation order (this is the backlog):**

| # | Item | Type | Sev | Effort | Ref |
|---|------|------|-----|--------|-----|
| 1 | `search`/`search_read` glob argument escapes the sandbox (arbitrary read, no approval) | Security bug | **High** | S | S-H1 |
| 2 | `command_class` auto-approve bypass via PowerShell `$(…)`, `"…$(…)"`, leading flags | Security bug | **High** | S | S-H2 |
| 3 | `cap_tool_output` panics on multi-byte output at the 100 KiB boundary (kills the agent task) | Bug | Med | XS | S-M1 |
| 4 | Event forwarder runs blocking git (stalls ALL agent event delivery; project lock held across `checkpoint`) | Perf/correctness | **High** | S | A-A1 |
| 5 | 401/403 body suppression only on `/models` — chat + vision error paths can echo the bearer key | Security | Med | S | S-M2 |
| 6 | Shared `plans_dir`: two UI-spawned agents clobber each other's `stack.json` | Correctness | **High**-ish | M | A-A2 |
| 7 | Unify 3 config-write pipelines (kills the live `resolved_model` drift bug) | Refactor | bug+−150 LOC | M | M-#2 |
| 8 | `Sandbox::validate_for_write()` shared ladder (security-relevant dedup) | Refactor | −60 LOC | M | M-#5 |
| 9 | Subagent allowlist fails OPEN on missing parent loop | Security | Low | XS | S-L1 |
| 10 | NTFS ADS attach to protected files | Security | Low | XS | S-L2 |
| 11 | `agent/tests.rs` fixture builder (28 copies) | Refactor | **−400 LOC** | low risk | M-#1 |
| 12 | `IpcState` accessors + send/stats command helpers | Refactor | −115 LOC | low | M-#3 |
| 13 | Move `BacklogStore`→lib, git ops→lib; dedup re-embed block | Architecture | ≈0/−60 LOC | low | A-R1/R3 |
| 14 | `startup_snapshot()` command (closes residual emit-race gaps) | Architecture | +35 net | low | A-R7 |
| 15 | Color-prefs table (frontend), delete `get_config`, settings validation→lib, forwarder `EventCoordinator`, per-agent plans_dir decision | Refactor/Arch | −185 LOC net | med | M-#4/#6, A-R4/R2/R6 |

Suggested batching into implementation plans: **Plan A = items 1–5** (security+stall, all small, independently shippable), **Plan B = 6–8** (structural correctness), **Plan C = 9–10** (quick hardening), **Plans D–F = refactors 11–15** in the maintainability report's sequencing (mechanical IPC first, semantic unifications each as own plan).

---

## 1. Security findings (all verified in-session)

### H1 — `search`/`search_read` glob escapes the sandbox [S-H1]
`src/tool/agent/search.rs:150-176`, `src/tool/agent/search_read.rs:118-144`. The LLM-controlled `glob` arg is concatenated onto the root (`format!("{}/{}", root, glob)`) and never routed through `Sandbox::validate`; the `glob` crate follows literal `..` components, so `glob: "../**/*.toml"` reads files outside the project (sibling projects, user profile — including the global `keys.toml`). `should_search` **fails open** (`strip_prefix(root).unwrap_or(path)`, search.rs:44) so it never detects the escape. Both tools are `AutoRun` — no approval prompt. **Verified:** the concatenation, the fail-open, and the unguarded `read_to_string` (search.rs:151,171-176) are all present at HEAD. Fix: reject `..`/absolute/drive-prefix globs; skip entries not `starts_with(root)`; make `should_search` fail closed; regression test with `../` glob from a tempdir.

### H2 — `command_class` auto-approve classifier bypass [S-H2]
`src/safety_rules/cmd_class.rs`. Four vectors, all confirmed by reading the code:
1. `$(Remove-Item …)` as a standalone statement is treated as a "bare variable" (is_benign_expression :249) — but `$(…)` executes in PowerShell.
2. Double-quoted strings are unconditionally benign (:246) — but PowerShell interpolates `"…$(…)…"`.
3. `cargo test $(evil)` — args after a recognized subcommand are dropped (:319-331).
4. Leading flags collapse the class: `git -C <anywhere> checkout -- .` → class `"git"` (:329), so a saved bare-`git` class rule auto-approves any leading-flag git invocation.

Consequence: one saved class rule (e.g. `cargo test`) silently auto-approves command strings the user never saw. Fix (conservative only): `None` on any `$(` at statement top level; only single-quoted strings benign; flag-as-tokens[1] → `None`; reject tokens containing `(`/`)`/backtick. Table-driven tests for all four vectors. (`merge`/`push` remain safe — `never_auto_for` beats rules, verified.)

### M1 — `cap_tool_output` UTF-8 panic [S-M1]
`src/tool/agent/mod.rs:40-46` calls `String::truncate` at exactly 100 KiB with no char-boundary check; shell/git output is `from_utf8_lossy` and routinely multi-byte. A multi-byte char straddling byte 102400 panics inside the tool and unwinds the agent task. The codebase already has the correct helper (`truncate_to_boundary` in read_files). Fix: reuse it (pub(crate)), + test with 200 KB of `"é"`.

### M2 — 401/403 body suppression incomplete [S-M2]
Present only on `/models` discovery (openai.rs:205-222). The main chat path (`openai.rs:487-496`) and vision path (`vision.rs:154-159`) format the raw response body into the error string — a hostile endpoint can echo the bearer key into UI/persisted errors. Fix: extract the suppression branch into a shared helper, apply at both sites, test both.

### Low
- **L1 [S-L1]** — subagent allowlist fails OPEN when the parent loop is missing (spawn.rs: `compute_subagent_allowlist` → `None` = unrestricted). Bounded by plan-mutation denial + Planning-state filter, but the fail direction is wrong: known-parent-missing-loop should → empty allowlist.
- **L2 [S-L2]** — NTFS alternate data streams: `.coding/memory.db:evil` passes the protected-name check (exact string compare) and `fs::write` creates an ADS on the protected file. Reject final components containing `:` (post-drive-prefix).
- **Correction to prior review** — shell `data: {stdout, stderr}` is **not** LLM-context bloat: the tool message fed back to the model is built from the capped `output` only (turn.rs:1119-1127); the uncapped JSON crosses IPC to the webview only (UI payload bloat). Recommend capping for symmetry, severity Low.

### Regression check — known-fixed items all hold
describe_image NeedsApproval + magic sniffing ✓; logs user-only perms (traces + provider-errors) ✓; reqwest `Policy::none()` (main + /models + vision via shared client) ✓; keys.toml DACL/redacted Debug/atomic write ✓; protected-path case-insensitivity ✓; CSP strict in prod, capabilities minimal (no `shell:allow-execute`) ✓; no `innerHTML`/`dangerouslySetInnerHTML` in frontend ✓; SQL/FTS fully parameterized ✓; unsafe FFI soundness ✓.

---

## 2. Architecture assessment

**Baseline holds** (three-layer split, dependency direction, per-agent factory, single spawn path). New findings:

- **A1 (High)** — the event forwarder (single fan-in consumer) awaits `run_all_dispatch_next`/`on_main_turn_resolved`, which run **synchronous git subprocesses**; and `run_all_dispatch_next` holds the `project.root` lock across `checkpoint()` while the resolution path correctly clones first (the two paths are visibly inconsistent — run_all.rs:472-475 vs :568). While a commit runs, every agent's events queue (cap 256) and agents block on `fanin_tx.send().await` — i.e. **all generation stalls**. Fix: `spawn_blocking` + clone-root-before-checkpoint (~15 LOC).
- **A2 (High-ish)** — every agent's `Workflow` loads the same `plans_dir` (factory.rs:326-330); subagents are fenced (`set_plan_mutations_allowed(false)`), but **UI-spawned parentless agents are not** — two of them uncoordinate-write `stack.json` and last-writer-wins clobbers the other's plan. Also `main_agent_id()` = min parentless id, so after the main agent exits a UI-spawned agent silently becomes the backlog dispatch target. Fix: per-agent plans subdir for non-main spawns, or enforce main-only plan writes + document.
- **A3/A4/A5 (Medium, drift)** — domain logic accumulating in the adapter: events.rs is now an orchestration brain (~85% untestable without `AppHandle`), BacklogStore + run-all git ops are Tauri-free domain code, settings.rs holds validation policy. Structural fixes R1/R2/R4 in the arch report (moves + seam extraction), each converting untested code into unit-testable code.
- **A7 (Low)** — the ContextUsage emit-race is fixed twice-over (registration emit + `context_caps` pull); residual gaps: non-active agents' `workflowStates` never pulled at startup; two direct `app.emit` call-sites (PromptDispatched, SkillStarted) emit from command contexts parallel to the forwarder. `startup_snapshot()` command closes the class.
- **A8 (verified clean)** — the frontend does NOT re-implement backend-owned state; the one dual config copy (resolver vs state) can't diverge through existing write paths.
- **Lock-order invariant verified site-by-site** (arch report's map): manager → agent_loops, workflow always innermost, no `L→M`/`W→M/L` edge exists anywhere at HEAD. The dispatch descendant-gate is the most delicate seam — recommendation adopted: make "no W held across the tracker" structural (comment/debug_assert now).
- **Extensibility touch-points** — new tool: 4 files (good); new IPC command: 2+2 (acceptable; the manual `generate_handler` list doubles as census); new agent event: 8–12 across mirror/reducer/fixtures (highest friction, but it is *audited* friction — exhaustive reducer switch + golden fixtures are the safety trip-wires); new right-panel view: 2–3 (the registry pattern others should converge toward).

---

## 3. Refactoring backlog (ranked by LOC × payoff; total ≈ −1,013)

1. **agent/tests.rs fixture builder** — 28 identical ~20-line AgentLoop constructions (+6 mock providers in runtime/agent.rs tests). **−400**, low risk. Constructor changes stop touching 28 sites.
2. **Unify `save_endpoints`/`save_settings`/`set_model` pipelines** (`persist_and_reload` + `swap_live_provider` + `set_runtime_safety` in new `ipc/config_io.rs`). **−150**, med risk. **Kills a live bug today:** `set_model` clears each loop's `resolved_model` (agent.rs:514) but `save_endpoints` doesn't (settings.rs:371-373) — verified drift.
3. **IpcState accessors + send/stats helpers** — 6 identical send-command bodies, 3 stats bodies, 15+ repeated ok_or_else messages. **−115**, low.
4. **Frontend COLOR_PREFS table** — 11 hand-written color actions + two ~40-line resets. **−90**, low-med. New color = 1 row instead of 6 edits.
5. **`Sandbox::validate_for_write()` ladder** — the validate→creation-fallback→protected-check→mkdir→revalidate sequence hand-rolled 3× (file_write/file_append/file_edit) with drift (append doesn't mkdir; refusal message ×5). **−60**, med, security-positive: the M5 creation-gap fix had to be applied twice because of this duplication.
6. **Delete `get_config`** (strict subset of `get_settings`; FE migrates). **−55**, med.
7. **Shared `maybe_reembed`** — main.rs:741-771 ≡ rewire.rs:58-92 verbatim, data-integrity logic in the shell. **−30**, trivial.
8. **`fallback_ipc_state()`** for the 2 degraded startup arms. **−30**, low.
9. **In-file dedup** (pricing map ×2, mangled comment block, `AgentLoopMap` alias ×8, run_all exit-sequence ×5 + self-qualified paths ×7, openai parse arms + u64 extractor ×4, StatusBar click-away effects ×4). **−110**, low.
10. **openai.rs misc** (doc-line dup, arm merge). **−28**.

Non-findings (explicitly kept): tauri.ts per-command wrappers (the JSDoc+arg-mapping IS the type boundary); no Zustand-slice rewrite (store is already a facade); agentEventReducer switch design is a strength; `spawn_agent_shared` exemplary; `OpenAiClient::complete` length is legitimate; `agent/turn.rs` needs a dedicated follow-up read before proposing extraction (not counted above).

---

## 4. Test-architecture gaps the fixes should close

Forwarder arms (running flags, Exited cleanup, notify-parent, cleanup-FIFO) — untestable today, fixed by R2 seam; run-all state machines — fixed by R1 move; settings validation paths — fixed by R4 move; H1/H2/M1/M2 each get table/regression tests per their fix notes. None of the refactors hurt existing tests; the contract fixtures survive untouched (wire structs stay in the adapter).

---

## 5. Explicit non-findings

AgentManager single mutex (accepted, single-user scale); channel serde mirrors (accepted); AgentLoop field count (documented); shell arbitrary commands behind gate (by design); browser `data:` scheme (accepted-low); TOCTOU validate→use (accepted-low, documented); keys `.bak` DACL inheritance (accepted); frontend state duplication (verified none); no dead production symbols found (`deny(warnings)` keeps it near zero — the "dead" mass is stale comments + duplicated logic, itemized in §3.9).
