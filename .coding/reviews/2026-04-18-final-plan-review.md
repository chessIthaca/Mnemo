# Final Plan Review — 6-Batch Deep-Review Remediation (plan e342c141)

**Scope:** Cross-batch integration review of all 6 batches (A–F) remediation of the
2026-04-18 deep-review backlog (15 findings). Each batch was already individually
reviewed + fixed + merged; this review covers the **composed** result.

**Methodology + limitations (read this first):** I am a read-only reviewer with no
shell access, so I could NOT run `git diff 4469c3d..HEAD` (the user's requested
entry point) nor `cargo test`. Instead I verified the **current source state at HEAD**
against (a) the 6 per-batch review reports (`.coding/reviews/2026-04-18-batch-{a..f}-review.md`)
and (b) the per-batch specs, then inspected the actual source files to confirm every
per-batch finding flagged "fix before commit" was actually fixed AND that the cross-batch
composition questions compose. The per-batch reviews each confirm their own closing
sequence ran `cargo test` green + warning-free under `#![deny(warnings)]`; I re-verified
the constitution-critical properties (no `#[allow]`, no dead code, doc comments) by
source inspection. Findings below reflect what source inspection could prove; items
requiring a build/run are marked "[build-gated]".

---

## (1) Cross-batch composition — all compose correctly ✅

### CB1 — Batch A `spawn_blocking` + Batch E `git_ops` move — async semantics PRESERVED ✅
The user's key question: does A4's "no lock held across `.await`" survive E1's move of
the git ops from `run_all.rs` into `myharness::project::git_ops`?

- **The move:** `src/project/git_ops.rs:58` uses `tokio::task::spawn_blocking` (E1
  swapped it from `tauri::async_runtime::spawn_blocking`). The module doc (`:3-7`)
  explains the rationale — a multi-second `git commit` must not stall the async runtime.
  This is semantically equivalent (Tauri v2's async runtime IS tokio; the lib crate
  already uses `tokio::task::spawn_blocking` in `BundledEmbedder::embed`), so no new
  dependency, same blocking pool.
- **The callers release the root lock BEFORE the spawn_blocking await:**
  - `run_all.rs:214-215`: `let root = state.project.root.lock().await.root.clone();`
    then `checkpoint(root, item.clone()).await`. The `root.lock().await` guard is a
    temporary dropped at the `;` — `checkpoint` receives an owned `PathBuf`, no lock held.
  - `run_all.rs:307`: `let root = state.project.root.lock().await.root.clone();` →
    `:322 rollback(root.clone(), sha.clone()).await` and `:335 commit_success(root.clone(), item.clone()).await`.
    Same pattern — root cloned out of the lock, lock dropped before the await.
- **A4's `Send + 'static` closure requirement holds:** `checkpoint`/`commit_success`/`rollback`
  own `PathBuf` + `BacklogItem`/`String` (all `Send + 'static`), moved into the
  `spawn_blocking` closure. No borrows. ✅

**Verdict:** A4's no-lock-across-await + Send+'static properties are preserved through
E1's move. The two batches compose correctly.

### CB2 — Batch C ADS guard + Batch B `validate_for_write` ladder — compose correctly ✅
- **C2 ADS guard** (`sandbox.rs:188-192`): rejects any final path component containing
  `:`, runs BEFORE the exact-match checks (`:195-200`).
- **B3 `validate_for_write` ladder** (`sandbox.rs:224-249`): step 3 calls
  `is_protected_write_target` (which contains the ADS guard) at `:231` BEFORE step 4
  `create_dir_all` at `:238`.
- **Composition:** an ADS path like `.coding/memory.db:evil` flows: validate →
  creation-fallback → `is_protected_write_target` (ADS guard fires, returns `true`) →
  `protected_refusal` → return. The mkdir at step 4 is never reached. The ADS guard is
  also reached from `refuse_if_protected` (`:255`, the file_edit path), so all three
  file tools (write/append via `validate_for_write`, edit via `refuse_if_protected`)
  get the ADS guard. ✅

**Verdict:** C2's ADS guard is correctly reachable through B3's ladder; the protected-
BEFORE-mkdir ordering is intact.

### CB3 — Batch E backlog move + Batch B per-agent plan dirs + Batch D `AgentLoopMap` alias — compose correctly ✅
- **E1 backlog move + B1 per-agent plan dirs:** orthogonal subsystems. The backlog
  (`myharness::backlog::BacklogStore`) is global run-all state; per-agent plan dirs
  (`.coding/plans/agents/<id>/`) are per-agent workflow state. `startup.rs:96` reads
  `state.backlog.store.lock().await.items()` (the moved `BacklogStore`) and `:13`
  imports `myharness::backlog::BacklogItem` — both resolve. No interaction. ✅
- **B1 per-agent plan dirs + D `AgentLoopMap`:** each UI-spawned agent gets
  `factory.plans_dir().join(format!("agents/{agent_id}"))` keyed by the same `AgentId`
  used as the `AgentLoopMap` key. The `agent_id` is the join key for both, so they
  compose. `Workflow::persist_stack` does `create_dir_all(&self.plans_dir)` so the
  per-agent dir is created on first plan save. ✅
- **D `AgentLoopMap` alias consistency:** the alias (`state.rs:68-69`) is used by the 3
  production callers (`spawn.rs:86, :383, :392`) and `events.rs:150`. See LOW finding
  L1 below for a minor inconsistency in `compute_subagent_allowlist`'s own signature.

**Verdict:** The three batches compose. No cross-batch structural conflict.

### CB4 — Batch B config-write unification + Batch D `set_safety` consolidation — clean ✅
Batch D consumed Batch B's `config_io::set_runtime_safety` into `IpcState::set_safety`
(`state.rs:188-199`), addressing Batch B's LOW finding (panic→`map_err?`). The
`config_io.rs` module doc (`:9`) explicitly cross-refs: "Runtime-safety sync lives on
`IpcState::set_safety`." A repo-wide search confirms ZERO `set_runtime_safety`
references in `src/` (only in `.coding/` docs) — no dangling reference. `config_io.rs`
still exports `persist_and_reload` + `swap_live_provider` (both `pub(crate)`, both with
callers). ✅

### CB5 — Batch E `startup_snapshot` + Batch D `embedder_status()` accessor — compose ✅
`startup.rs:99` calls `state.embedder_status()?` — the D-introduced accessor that
returns `Result<EmbedderStatus, IpcError>` (panic→error on poison, the D LOW
normalization). The `?` propagates the `IpcError`. Composes cleanly. ✅

---

## (2) Deferred items — justifications sound, none block merge ✅

### D1 — test-fixture builder (−400 LOC) — DEFERRED, sound
Test-only mechanical refactor across 46 fixture sites. No runtime/security/correctness
impact. The Batch D review (INFORMATIONAL) accepted the deferral on risk grounds and
noted the LOC shortfall. Tracked in the plan checkbox. **Does not block merge.**

### F1 — EventCoordinator (forwarder extraction) — DEFERRED, sound
Medium-risk async refactor of the hottest loop (`events.rs` forwarder). Batch A already
fixed the stall (A4: `spawn_blocking` for git ops); the forwarder works correctly today.
F1 is a maintainability/testability improvement, not a correctness fix. No half-wired
scaffolding left in source (unlike F2) — tracked in the plan, not the code. **Does not
block merge.**

### F2 apply-block (DTO→SettingsPatch conversion) — DEFERRED with TODO marker, sound
- **Wired extractions** (`validate_endpoint`, `validate_endpoint_set`, `apply_endpoints`,
  `parse_safety_mode`, `apply_models_patch`): behavior-identical to the old inline logic,
  14 lib tests, called by the adapter. ✅
- **Unwired scaffolding** (`validate_settings_patch`, `apply_settings_patch`,
  `SettingsPatch`, `ModelsPatch`): `pub` in `pub mod patch` → treated as public API by
  the compiler → **NOT dead code under `#![deny(warnings)]`** (verified: no build
  violation). Has latent discrepancies (B1-B5: theme lowercase, default_provider ref
  check, empty+trim checks, trim in apply, to_ascii_lowercase) — but these are NOT
  reachable today (the fns are not called). The TODO marker at `patch.rs:181-188`
  explicitly lists B1-B5 so the next batch won't wire them in blind. **Does not block
  merge** — the latent bugs are gated behind the unwired state.

---

## (3) Constitution compliance across the whole diff ✅

### Per-batch findings — all fixed in source (verified)
| Batch | Finding | Status | Evidence |
|-------|---------|--------|----------|
| A | S1 `cd` short-circuits before metachar guard | ✅ Fixed | `cmd_class.rs:311` metachar guard now precedes `:323` `cd` short-circuit; comment `:819` confirms ordering |
| A | S2 `$(` in redirect target stripped before guard | ✅ Fixed | `cmd_class.rs:107` raw `$(` guard runs BEFORE `strip_redirections` at `:113`; comment `:103` confirms |
| A | S3 401/403 body logged to trace | ✅ Fixed | `openai.rs:531` builds `trace_msg` suppressing body for 401/403; doc `:438-439` notes trace IS UI-surfaced so body is suppressed |
| B | LOW `set_runtime_safety` panics on poison | ✅ Fixed (by D) | `state.rs:188-199` `set_safety` uses `map_err?`; `config_io.rs:9` cross-refs; zero `set_runtime_safety` refs in `src/` |
| C | MEDIUM stale doc on `compute_subagent_allowlist` | ✅ Fixed | `spawn.rs:255-259` now states "Returns None ONLY when no parent... missing loop → Some(vec![]) deny all — fail-closed" |
| C | LOW duplicate test | ✅ Fixed | Only `no_parent_returns_none` + `missing_parent_loop_denies_all` remain (`:693`, `:708`) |
| E | M1 `startup_snapshot` lock-ordering | ✅ Fixed | `startup.rs:51-57` snapshots under `manager`, drops, THEN `agent_loops` at `:62`; comment `:47-50` cites invariant |
| E | M2 duplicate `get_settings` registration | ✅ Fixed | `main.rs:400-401` shows a single `ipc::settings::get_settings,` |
| E | M3 A9 swallowed dispatch error | ✅ Fixed | `run_all.rs:373` + `:396` use `if let Err(e) = … { eprintln!(…); }`; comments cite "A9" |
| E | L1 `setCodeColor` dead code | ✅ Fixed | `setCodeColor` absent from all `.ts`/`.tsx` (only in `.coding/` docs); `setColor` doc `:305-308` no longer claims delegation |
| E | L2 backlog not seeded from snapshot | ✅ Fixed | `App.tsx:184` calls `setBacklog(snap.backlog)`; comment cites L2 |
| F | TODO marker for unwired scaffolding | ✅ Present | `patch.rs:181-188` `TODO(F2-full)` lists B1-B5 |
| F | L2 `validate_endpoint_set` doc inaccuracy | ✅ Fixed | `patch.rs:78-81` now says "Unique names checked separately by caller — this fn does NOT check them" |

### Constitution properties (whole diff)
- **Doc comments on all public items:** ✅ Verified across batches — every new `pub`/`pub(crate)`
  fn + struct has `///` docs; modules have `//!`. The F2 unwired scaffolding is documented
  (including the TODO listing the latent bugs).
- **No `#[allow(...)]`:** ✅ Confirmed by source search. The only `#[allow]`s in `src/`
  are the pre-existing `#[allow(clippy::too_many_arguments)]` on `AgentLoop` constructors
  (untouched by any batch). No batch added a suppression.
- **No dead code:** ✅ The F2 unwired scaffolding is `pub` in a lib crate (public API,
  not dead code — verified). `setCodeColor` deleted (E L1). All other new items have
  callers (verified per-batch). The `now_secs` helper in `backlog` is still used by `add`;
  `first_line` still used by the commit fns.
- **Warning-free build [build-gated]:** I cannot run `cargo test`. By inspection there is
  no dead code, no unused import, no missing doc, no `#[allow]`, no obvious type mismatch.
  Each per-batch review confirmed this by inspection, and each batch's closing sequence
  ran `cargo test` green + warning-free under `#![deny(warnings)]` (root + `src-tauri`).
  The composed diff introduces no new cross-batch import/type issue (verified: the
  `AgentLoopMap` alias resolves consistently, the `myharness::backlog`/`myharness::project::git_ops`
  imports resolve, the deleted `get_config`/`AppConfig`/`GetConfigResponse` leave zero
  dangling references in source).

---

## Findings

### L1 (LOW, informational) — `compute_subagent_allowlist` spells out the `AgentLoopMap` type instead of using the alias
**Where:** `src-tauri/src/ipc/spawn.rs:275-277` (fn signature) and `:502` (a test).

Batch D introduced the `AgentLoopMap` alias (`state.rs:68-69`) "so the 8+ spellings of
this type don't drift." The 3 production callers of `compute_subagent_allowlist`
(`spawn.rs:86, :383, :392`) use the alias, but the function's OWN signature at `:275-277`
still spells out `&Arc<tokio::sync::Mutex<std::collections::HashMap<AgentId, Arc<AgentLoop>>>>`
instead of `&AgentLoopMap`. A test at `:502` likewise spells out the full type.

This is NOT a bug (the types are identical) and NOT a constitution violation (no warning,
no dead code). It is a missed opportunity for the alias's stated anti-drift purpose —
and slightly ironic that the very function Batch C's security fix touched didn't get the
alias applied. **No fix required; flagged for consistency.** If desired, change the
signature to `agent_loops: &crate::ipc::state::AgentLoopMap` and the test to use the alias.

---

## (4) Overall assessment — goal achieved ✅

The plan's goal was to fix all 15 deep-review findings. Mapping the 15
(`2026-04-18-deep-review.md` row 15 = "Color-prefs table, delete get_config, settings
validation→lib, forwarder EventCoordinator, per-agent plans_dir decision" + the 10
security/structural/hardening/mechanical findings across rows 1-14):

- **Security + stall (A1-A5):** all 5 fixed + the 3 per-batch-review gaps (S1/S2/S3)
  closed. ✅
- **Structural correctness (B1-B3):** all 3 fixed (per-agent plan dirs, config-write
  unification with the live-drift bug closed, `validate_for_write` ladder). ✅
- **Hardening (C1-C2):** both fixed (allowlist fail-closed, NTFS ADS). ✅
- **Mechanical size reduction (D2-D3):** done; D1 (test fixtures) deferred — sound. ✅
- **Moves + snapshot + frontend (E1-E5):** all 5 done + the 3 per-batch-review bugs
  (M1/M2/M3) + 3 cleanups (L1/L2/L3) fixed. ✅
- **Color-prefs table (E4):** done. ✅
- **Delete get_config (E5):** done. ✅
- **Per-agent plans_dir (B1):** done. ✅
- **Settings validation→lib (F2):** PARTIAL — wired extractions done (5 fns + 14 tests,
  behavior-identical); apply-block deferred with TODO marker. The wired pieces achieve
  the extraction's core value; the deferred piece is the DTO→patch conversion layer. ✅ (partial, sound)
- **Forwarder EventCoordinator (F1):** DEFERRED — maintainability/testability only, no
  runtime impact, no half-wired scaffolding. ✅ (deferred, sound)

**Conclusion:** The plan's goal is **substantially achieved**. All security, correctness,
stall, structural, and hardening findings are fixed. The two deferred items (F1, F2
apply-block) are maintainability/testability improvements with no runtime/security
impact, tracked with a TODO marker (F2) and plan checkboxes (F1+D1). The cross-batch
compositions the user flagged (A4+E1 async semantics, C2+B3 ladder, E1+B1+D subsystem
composition) all compose correctly. The diff is **clean to merge**; the single LOW
finding (L1, alias inconsistency) is informational and non-blocking.

**Caveat [build-gated]:** I could not run `git diff 4469c3d..HEAD` or `cargo test`. The
per-batch reviews + their closing sequences cover the build/test gate for each batch
individually; a final consolidated `cargo test` (root + `src-tauri`) + `npm run build`
on the composed HEAD would close the last gap, but source inspection found no
cross-batch issue that would surface only at build time.
