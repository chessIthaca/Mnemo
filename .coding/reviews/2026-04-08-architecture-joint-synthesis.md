# Joint Architecture Review Synthesis — 2026-04-08

**Sources:**
- `.coding/reviews/2026-04-08-architecture-maintainability.md`
- `.coding/reviews/2026-04-08-architecture-ui-wiring.md`
- `.coding/reviews/2026-04-08-architecture-performance.md`
- `.coding/reviews/2026-04-08-architecture-quality.md`

**Mode:** Synthesis only. Remediation is tracked in the follow-on fix plan.

---

## Overall verdict

myharness has a **strong three-layer architecture** (Tauri-free Rust brain → IPC adapter → React/Zustand UI), a real plan-first workflow, solid multi-agent channel plumbing, and thoughtful streaming optimizations on the assistant-text path.

It is **not yet unattended-safe**. Interactive ApproveEachAction use is solid; Autonomous / AutoApproveProject / Run-All / multi-agent overnight batches need hardening before trusting them.

---

## Cross-cutting themes (appear in ≥2 reports)

| Theme | Reports | Severity band |
|---|---|---|
| Approval preview dead (`preview: None`; FE rebuilds LCS) | Quality H5, UI H3, Perf H4/structural | High |
| Plan-first gate soft at dispatch | Quality H1 | High (correctness/safety) |
| Multi-agent plan disk SOTs shared | Maintainability H2 | High (product + correctness) |
| God modules: `commands.rs`, `useAgentStore.ts` | Maintainability H1/H3 | High (debt) |
| Manual Rust↔TS contract drift | Maintainability H4, UI M9 | High/Medium |
| Reasoning/tool-arg delta flood (no rAF) | Perf H3, UI L1 | High/Low |
| Shell gaps (cwd, timeout, unbounded output) | Quality H2/H3, Perf M3 | High/Medium |
| Run-All terminal/bookkeeping hazards | Quality C1/M3/M4, UI M8 | **Critical**/Medium |
| Invisible non-active-agent approvals | UI H1/H4 | High |
| `deny_all` incomplete (UI + batch latch) | UI H2, Quality M1 | High/Medium |
| PLAN.md drift from shipped reality | Maintainability M7, UI M2 | Medium |

---

## Severity rollup (unique work items)

### Critical (do first)
1. **C1** Run-All: final tool-error abort emits `Error` then `Finished` → can mark failure as success / commit after rollback (`turn.rs` + `events.rs`).

### High — safety & correctness
2. **Q-H1** Enforce `ToolFilter` at dispatch (not only schema build).
3. **Q-H2** Sandbox-validate `shell` cwd.
4. **Q-H3** Shell timeout + kill on expiry/interrupt.
5. **Q-H4** Narrow AutoApproveProject git auto-approval (esp. `commit` / `add -A`).
6. **Q-H5 / UI-H3 / P-H4** Populate + consume `ApprovalPreview`; stop relying solely on client LCS.
7. **UI-H1/H4** Global approval attention (tab badge + optional banner); approvals not only on active tab.
8. **UI-H2 + Q-M1** Wire Deny-all UI **and** turn-level deny-all latch for remaining tool batch.
9. **M-H2** Resolve multi-agent plan ownership (shared+serialized vs per-agent namespaces) and align code/docs/tests.

### High — performance (after safety P0)
10. **P-H1** `spawn_blocking` (or equivalent) for FS tools + search + sandbox validate.
11. **P-H2** Skip/cache auto-recall + reduce per-iteration tiktoken cost.
12. **P-H3** rAF-batch reasoning + tool-arg deltas; later Rust-side coalesce.
13. **P-H4** Diff size guard (until Rust preview is primary).

### Medium (contract, UX, debt)
14. WorkflowStateChanged for all workflow/skill tools.
15. Run-All halt/checkpoint metadata + clearer paused status.
16. Expand protected `.coding` write targets.
17. Shell/git output caps (align PLAN.md).
18. Narrow Zustand selectors; split `useAgentStore`.
19. Split `ipc/commands.rs`; unify spawn paths.
20. Safety-mode single write path; theme prefs consolidation.
21. Plan UI: depth/parents/skill; auto-reveal diff on approval.
22. FE vitest runner + reducer table tests; IPC golden fixtures / codegen.
23. keys.toml ACL; structured logging for forwarder/git failures.
24. Dead UI paths: `spawnAgent`/`cancel` expose or remove; backlog per-item ▶ semantics.
25. Honor `approve()` boolean; deny-on-`/clear`.

### Structural / later
26. Split `provider/openai.rs`, `memory/mod.rs`.
27. Incremental token counting; better summarization strategy.
28. Memory read-connection pool; optional ANN.
29. Transcript virtualization; per-agent event buffering.
30. Refresh PLAN.md / as-built architecture doc.
31. Right-panel view registry; tool registration grouping + name-set test.
32. Optional Run-All success = workflow Complete.

---

## Product decisions required (block some designs)

1. **Plan isolation:** main-agent-only plan mutations vs per-agent plan namespaces?
2. **Run-All success:** Finished vs plan Complete vs tests-green?
3. **Run-All halt on approval:** rollback now vs leave WIP vs `PausedForApproval`?
4. **StatusBar safety mode:** session-only or persist?
5. **UI spawn/cancel:** intentional omission or missing feature?
6. **Subagent InputBar prompts:** keep or steer-only?
7. **Schema tooling:** specta/ts-rs vs golden JSON fixtures?
8. **ApprovalPreview:** was `preview: None` intentional FE-only diffs or unfinished?

---

## Recommended remediation order (phases)

| Phase | Name | Goal |
|---|---|---|
| **0** | Unattended correctness | C1 + dispatch ToolFilter + shell cwd/timeout + git auto-approve narrow |
| **1** | Approval contract complete | Preview populate+consume, deny-all latch+UI, multi-agent approval visibility, approve bool, clear-deny |
| **2** | Performance quick wins | rAF non-text deltas, output caps, diff guard, recall/token cache, selectors, search early-exit |
| **3** | Run-All + workflow UX | Halt status, WorkflowStateChanged completeness, plan stack/skill UI, backlog ▶, safety-mode path |
| **4** | Multi-agent plan policy | Implement chosen isolation model; docs/tests |
| **5** | Maintainability splits | commands.rs, useAgentStore, spawn unify, FE tests/CI fixtures |
| **6** | Structural perf + hardening | spawn_blocking, delta coalesce, protected paths, keys ACL, PLAN.md refresh, larger splits |

Each phase ends with `cargo test` (+ FE tests when runner exists), a read-only reviewer pass on all uncommitted changes, fix findings, commit on feature branch.

---

## Strengths to preserve

- Zero-tauri brain; channel contract + oneshot adapter
- Agent loop module split; factory; disk-backed plans
- never_auto_for merge/push; approval map per-agent cleanup
- Text-delta rAF + plain-text stream + Message memo
- WAL memory + FTS prefilter; constitution mtime cache
- Rich Rust unit/integration tests on core domains

---

*End of synthesis. See fix plan for executable steps.*
