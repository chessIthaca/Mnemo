## Verdict: FINDINGS (0 high, 1 low)

Delta review of step 2a.6 (commit `cad67fc`, 13 files, HEAD on `wt/mnemo`) plus the
uncommitted remainder. The only uncommitted change is `.coding/plans/5ade43bc.md` —
bookkeeping (accuracy-checked below, no line review). Scope verified: `git status` clean
except that file; the commit's diff read via the boundary commit, the landed files read
directly (model_resolver.rs head/golden/walk/guards, workflow_policy.rs, factory.rs tail,
loop_impl call sites, policy.rs, general.rs, tauri.ts, spawn_agent.rs mock + consumers,
runtime/agent.rs mock signatures, tests).

### Resolution-parity walk (check a) — VERIFIED

Old arm order (pre-2a.6 @fc2b37d, anchors verified against the plan file's checked
transcript): arm 1 skill `models.skill.get(name)` :317-323 → arm 2 `models.subagent`
when `ctx.is_subagent` :326-330 → arm 3 `models.bug_fixing` when
`plan_kind==BugFixing && state ∈ {Executing, Reviewing}` :343-352 → arm 4 the state
match :364-383 (Planning→planning, Executing→executing, Reviewing→executing,
Complete→complete, Skill/Subagent→none) → default.

New `build_route_key` (model_resolver.rs :109-130) emits exactly that chain:
`skill:<name>` iff `Some`, `subagent` iff flag, `bug_fixing` under the identical
plan-kind/state predicate, then the state slot with the identical mapping. The
`ConfigModelResolver::resolve` walk (:358-386) reproduces the arms slot-for-slot:
skill miss → `continue` (arm-1 fall-through), subagent → flag guard + slot, then
direct slot→config mapping; every candidate goes through `resolve_model_ref`, and a
dangling reference falls through to the NEXT slot — matching the old arms' fall-through
exactly. The state slot is last in every key, so the old tail
(override-or-default) is preserved; after the loop, `resolve_model_ref(None)` = default.
The skill-then-subagent fall-through and both dangling cases (mid-chain and tail) are
behaviorally identical. Parity holds.

### Acceptance judgment (the golden, §2a.6 :745-747) — GENUINELY MET, no rework

The golden table test (:845-945) constructs every cell via `ModelContext::new(...)`,
whose body (`:93`) builds the RouteKey through `build_route_key` before the walk. The
table therefore IS "walked from the build_route_key output" — both the key builder and
the slot walk are pinned by the golden, and a drift in either fails it. Hand-building
`RouteKey` literals instead would pin the hand-built key rather than the production
builder — strictly weaker. No golden rework needed.

Grep-count acceptance: 38 + 1 + 4 = 43 verified by literal search — model_resolver.rs
38 real call sites (the 39th hit, `:70`, is prose in the `ModelContext` doc comment,
not a call), turn.rs 1, loop_impl.rs 4.

### The seams — VERIFIED

- **OnceLock attach vs factory tail (check b).** `factory.rs :1147-1151`: `Arc::new`
  → `attach_loop(Arc::downgrade(&agent))` → return — post-Arc-wrap as specified;
  `OnceLock::set` is idempotent (first attach wins, `let _ =` discards the re-attach
  error). The plan sketched `Option<Weak>`; the landed `OnceLock` is the documented
  deviation and is the sounder choice (no interior-mutability cell). `route()`
  (:105-118) reads `is_subagent()` while holding the workflow lock — the getter
  (loop_impl :1198-1200) is a plain `Mutex` read of the loop's own flag, no workflow
  re-entrancy; unattached/dropped loop reads as false = the flag's default. Safe.
- **Deviation 1** — `is_subagent()` at :1192-1200 pre-existed verbatim; zero edits
  needed there, as documented.
- **Deviation 2** — `resolve_routed`'s new key-based guard (:418-432) refuses any
  `skill:`/`subagent`/`bug_fixing` slot; the state field left the ctx, so the old
  guard's nameless-Skill-state and false-flag-Subagent-state corners are key-invisible.
  Audited the production call sites: all four loop_impl ctx constructions (:1562, :1645,
  :1738, :1762) take state, skill name, flag, and plan-kind from the same per-request
  workflow snapshot — state/name/flag are coherent (Skill state carries an active skill;
  subagent loops carry the stamped flag). The corner is unreachable in production;
  even hypothetically it would only differ in choosing the routed target over the
  default. Deviation accepted.
- **spawn_agent mock** (:1317-1334): the deliberate-slot check replaces the old
  state==Reviewing bit (key-invisible since Reviewing→"executing"); consumers
  (:1386-1389, :1415, and the :1382-1512 family) assert `asked` EMPTY only, so no
  information is lost. The mock change is test-only; the parked-while-subagents gate
  is untouched (check e).

### Constitution checks

- **Docs sync (check d)** — current and consistent: module doc :13-22 (the 4-slot
  chain + default), `ModelContext` doc :60-79, `RouteKey` doc (policy.rs :107-115,
  NoPolicy `["free"]` contract), `ModelsConfig` doc (general.rs :820-826),
  `free` field docs (general.rs :857-861, tauri.ts :439-442). No stale ordering text.
- **Multi-platform (check c)** �� std-only additions (`OnceLock`, `Weak`); no
  platform-specific code; no shell-based mutation anywhere in the diff
  (file-tools-first clean).
- **Bookkeeping accuracy** — `.coding/plans/5ade43bc.md` matches what shipped
  (anchors, deviations, and acceptance notes all check out against the landed code).
- **Warning-free** — I am read-only (no shell); `cargo test -p mnemo` green is
  asserted by the dispatch and is the deny(warnings) proof. No `#[allow(...)]`
  anywhere in the diff.

### Findings

**1. LOW — the `[models.free]` contract has zero test coverage.** The step's new
resolve arm `"free" => models.free.as_ref()` (model_resolver.rs :378) and `NoPolicy`'s
`["free"]` emission (policy.rs :218) are exercised by no test: the golden's fixture
sets `free: None` (:876), and no test constructs a `["free"]` key. The compat ctor
can't produce one (every (state, skill, flag) combination yields a state slot or a
skill/subagent slot), so a future refactor that drops or mistypes the free arm fails
nothing. Suggested fix: a small unit test in model_resolver building the ctx directly
(`ModelContext { route: RouteKey { slots: vec!["free".into()] }, is_subagent: false }`)
asserting that with `[models.free]` set it resolves that model, and that unset/dangling
falls to default. Not blocking — the 2a.6 landing itself is correct; the free-agent
kernel consult (2b/2c) will exercise the path, but the slot mapping deserves a pin now
that the config surface exists.

### Summary

The riskiest step landed cleanly: the parity walk holds line-for-line, the golden
acceptance is genuinely met through the compat ctor, both deviations check out, and
all five extra checks pass. One low finding (missing `[models.free]` test pin).

Reviewed-state: cad67fcecfe432df06dd591d01231851efdddd30
