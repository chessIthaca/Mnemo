# Job 5 — the final acceptance (the G1–G10 proof)

Master spec `.coding/specs/headless-free-agent.md` §5.9.10 (:740-753), §6
row 5 (:767), §7 row 34 (:889-892), §8 (:895-906), §9 (:908-933). The last
job: prove the split — the feature lattice, the second consumer, the
src-tauri boundary, and the G1–G10 acceptance sweep. Branch `wt/mnemo`
(the arc's single working branch — supersedes the spec's `alf/<job>`).

## B. Probe facts (2026-10-13, tree = 10ccef4)

1. **Features today** (root Cargo.toml :12-28): `browser`, `embeddings`,
   `test-support`. NO `default` key (default set = empty), NO
   `kernel`/`app`/`headless`. src-tauri deps: `mnemo = { path = "..",
   features = ["browser", "embeddings"] }` + dev `test-support` — a new
   `default` unifies automatically for it (no line change needed).
2. **Gates today** (src/lib.rs :13-34): `pub mod app;` (:14) and
   `pub mod headless;` (:20) are UNGATED; `pub mod browser` (:15-16) is
   the gated precedent (cfg + a features-table doc line).
3. **Kernel→app isolation is LIVE**: `kernel_does_not_import_app`
   (src/lib.rs:227, the 2b boundary mod) — 5 banned tokens, src/app
   excluded from the walk, `#[cfg(test)]` items stripped (the literal-
   aware stripper at src/lib.rs:105), comments exempt. Green ⇒ NO kernel
   source references `crate::app` ⇒ gating `pub mod app` breaks nothing
   inside the lib. The same walk covers src/headless ⇒ headless never
   imports app either.
4. **The public façade pattern is established** (src/headless/mod.rs
   :24-56): headless re-exports kernel-side types for the public surface
   ("headless defines no data of its own" — ToolSelection, Guardrails
   wire, KeySource/Secret, SharedResources, McpServerWire). D4 extends
   this pattern, not invents one.
5. **The src-tauri reference surface**: 101 matches / 18 files for
   `mnemo::(agent|runtime|host|app)::`. Triage: `mnemo::app::*` =
   sanctioned (§5.9.10 bans only `agent::`/`runtime::`/`host::`
   internals "other than the re-exported public types"; the app graph is
   the app crate's own public API). The `agent::`/`runtime::`/`host::`
   references (~60, concentrated: console.rs, ipc/events.rs,
   ipc/run_all.rs, ipc/agent.rs, ipc/config_io.rs, ipc/approval.rs,
   ipc/questions.rs, ipc/backlog_cmds.rs, ipc/state.rs) need routing
   through sanctioned re-exports. Several sit inside `#[cfg(test)]`
   blocks (console.rs :1497+, :2203-2429 — AgentHandle unit tests): the
   boundary rule EXEMPTS test code (mirrors the lib-side exemption,
   fact 3's stripper).
6. **examples/ does not exist** — `examples/second_consumer.rs` (~60
   lines, §5.9.10 :744-746) is new: NoPolicy, non-coding identity,
   `ToolSelection = [Read, Host]`, ONE host tool answered by the example
   itself, limits set, events printed as JSON. Compile-tested on
   `kernel,headless` (NO app/embeddings/browser).
7. **tests/boundary.rs does not exist** — §9 :923 names
   `kernel_builds_without_app_feature` + `tauri_uses_only_public_api`.
8. **The verify set** (§8 :897-903) adds TWO commands the arc's
   batteries never ran: `cargo build -p mnemo --no-default-features
   --features headless` and `cargo clippy --workspace --all-targets --
   -D warnings`. The D-final battery runs all five (build, test -p,
   test --workspace, clippy, tauri-tree empty). NOTE: clippy on the whole
   workspace all-targets may surface pre-existing lints — budget a fix
   pass (deny-warnings crate roots keep `cargo test` clean, but clippy
   is a stricter lint set; stop after 3 failed fix attempts per §8).
9. **The remaining "app rebuilt on the public API" gap** (§5.9.10
   :747-753): IpcState (src-tauri/src/ipc/state.rs:47) still holds
   `AgentRuntimeContext` (AgentManager + AgentLoopFactory + a loops map)
   — NOT an Engine; console.rs wires `AgentLoopFactory`/`HostRuntime`/
   `AgentManager` directly (:33-41, :1216, :1497-1501). The ipc/* thinning
   (jobs 3b/4) covered the commands; the console REPL + the state core
   are the last direct-wiring holdouts. console.rs is ~2,500 lines — if
   the rebuild delta exceeds ~3k lines, split it into its own follow-up
   job BEFORE review (master :770-772 rule) and finish job 5 on the
   lattice + example + boundary + acceptance sweep.
10. **§9 test-name audit**: most named tests landed across jobs 2a-4;
    D-final enumerates against the actual files. Known name deviation:
    the spec's `tests/integration/headless_kernel.rs` set landed in
    `tests/integration/headless.rs` (the G8 fixtures, job 4 D7) —
    recorded, not renamed (renaming churns the battery filters for zero
    contract gain).
11. **Row 35 guard**: no job may change the plan-first behavior of a
    Workflow-profile agent — the 2a goldens (stable head bytes, routing
    table) protect it; the D-final battery proves them green.
12. **Battery discipline** (job-4 D9 lesson, agent.md): `cargo test
    --workspace` (compiles src-tauri), exit 0 + zero warnings under
    `#![deny(warnings)]`; feature-enabled run `--features embeddings`
    for the lib's gated tests.

## C. Steps

- **D1 — the feature lattice.** Root Cargo.toml: `default = ["kernel",
  "app", "headless"]`, `kernel = []`, `headless = ["kernel"]`, `app = []`
  (+ doc lines naming G10 §5.9.10); src/lib.rs: `#[cfg(feature = "app")]
  pub mod app;` + `#[cfg(feature = "headless")] pub mod headless;`.
  Proofs: `cargo build -p mnemo --no-default-features --features
  headless` AND the §5.9.10 form `--features kernel,headless` both exit 0
  (first-ever app-less compile — the red-check: it fails iff anything
  kernel-side references a gated module); battery `cargo test
  --workspace` green (defaults = today's code set, zero behavior
  change). Commit `feat(5.1)`.
- **D2 — examples/second_consumer.rs.** The §5.9.10 :744-6 sketch on the
  kernel+headless feature set; events as JSON lines; the host tool
  answered from the example's own loop; SessionLimits set. Compile-test:
  `cargo build --examples --no-default-features --features
  kernel,headless`; the run path stays mock/dummy-provider (no network —
  CI runs it per §9 :924; a `--once` smoke path or an immediate limit
  trip proves the loop without an endpoint). Commit `feat(5.2)`.
- **D3 — tests/boundary.rs.** The two §9 tests: (a)
  `kernel_builds_without_app_feature` — source-scan form: lib.rs gates
  `pub mod app`/`pub mod headless` with the cfg attrs + the features
  table carries the three names (the real app-less BUILD rides the
  battery as a command, like the tauri-tree proof — deviation recorded
  in §E); (b) `tauri_uses_only_public_api` — scan src-tauri/src
  production sources for `mnemo::agent::`/`mnemo::runtime::`/
  `mnemo::host::` path tokens, `#[cfg(test)]` stripped (reuse the
  lib-side stripper pattern; whole-file test mods named), `mnemo::app::`
  + `mnemo::headless::` + root re-exports allowed. Lands RED against
  today's tree (fact 5) — D4 turns it green. Commit `feat(5.3)` (test +
  the strip helper only).
- **D4 — the src-tauri public-API route.** Curated root re-exports in
  src/lib.rs for the sanctioned types the app names (AgentEvent/
  Approval/UserAnswer/QuestionOption/AgentCommand/AgentId/AgentHandle/
  AgentManager/SteerPayload/ContextManager/AgentLoopFactory/HostRuntime
  pieces/needs_approval/RouteKey/... — the D3 scan output is the exact
  worklist; keep it a TYPE re-export list, not a blanket `pub use`),
  then rewrite the ~60 references file-by-file (console.rs in slices).
  `mnemo::app::*` references stay. Battery per chunk; D3's scan green at
  the end. Commit `feat(5.4)` (may split a/b by file group).
- **D5 — the Engine core for the app** (the §5.9.10 :747-753 holdouts):
  IpcState holds an Engine; console.rs rebuilt on Engine/AgentSession/
  Setup/*Api. STOP-SPLIT: if the delta exceeds ~3k lines, land the
  IpcState core + file a follow-up job for the console rebuild (§B
  fact 9) — job 5 closes on the acceptance either way. Commit
  `feat(5.5)`.
- **D6 — the acceptance sweep.** §7 rows 1-35 each verified by name/
  evidence (most = landed fixtures; the sweep enumerates); §8 all five
  commands run (clippy fix pass budgeted, 3-strike rule); §9 name audit
  (§B fact 10 deviation); README/PLAN.md + module docs updated (the
  feature lattice is user-facing: a library-consumer quickstart naming
  `--features kernel,headless`). Boundary commit `feat(5.6)`.
- **D7 — review rounds to PASS → finish → master 11/11.** Reviewer over
  the job-5 delta (base = 10ccef4); fix findings with red-checked
  regressions; re-review to PASS; `docs(5.7)` close; finish; tick the
  umbrella's step 11. Write the job-5 close memory + the arc-close
  record (the whole master arc done).

## E. Deviations (recorded, with the spec text that motivated them)

1. `alf/<job>` per-job worktree branches → the single `wt/mnemo` branch
   (umbrella decision, job 1; the whole arc runs there).
2. `kernel_builds_without_app_feature` is a source-scan structural test
   + a battery COMMAND (the real app-less build), not an in-test cargo
   invocation (nested cargo risks target-lock stalls and doubles battery
   time; the command proof is how tauri-tree runs — §8 both, one shape).
3. The spec's `headless_kernel.rs` test file → landed as
   `tests/integration/headless.rs` (§B fact 10).
4. `headless = ["kernel"]` (not bare `[]`) so §8's exact form
   `--no-default-features --features headless` carries the kernel set —
   documentation-honest the day `kernel` gains real gates.

## F. Landing notes (per step, appended as each lands)

(none yet — D1 pending)

### 5.1 D1 — the feature lattice (landed 2026-10-13)

- Cargo.toml: `default = ["kernel", "app", "headless"]`, `kernel = []`,
  `headless = ["kernel"]` (§E deviation 4 — §8's exact `--features
  headless` form carries the kernel set), `app = []`, with the G10 doc
  block. src/lib.rs: `pub mod app` + `pub mod headless` cfg-gated
  (browser is the precedent shape); doc comments name the slices.
- Both spec-form proofs GREEN first try after one fix: `cargo build -p
  mnemo --no-default-features --features headless` exit 0 AND
  `--features kernel,headless` exit 0 — the first-ever app-less compile.
- The proof earned its keep immediately: it caught `gh_raw`
  (src/project/git_ops.rs:65) — a kernel-resident `pub(crate)` helper
  whose ONLY callers are the app-graph landing paths
  (`main_is_protected`, `land_via_pull_request`, both
  src/app/worktrees.rs) — dead code the moment `app` is off. MOVED
  app-side (module-private in worktrees.rs, doc adjusted); `git_raw`
  stays (it has kernel callers). This is exactly the coupling class G10
  exists to surface.
- Battery `cargo test --workspace` exit 0 (83 integration; defaults =
  today's code set, zero behavior change). src-tauri needed no dep-line
  change (defaults unify automatically).
