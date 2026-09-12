## Verdict: FINDINGS (0 high, 1 low)

Round-5 verification of the round-4 doc-only fix (commit b461978, HEAD on wt/agenticcoding, tree clean). **The round-4 fix is verified correct**: the widened exception now enumerates exactly the true set of resolver-built-CM turns — all five `build_turn_provider` serving sites in `resolve_turn_provider`, with the plain pin and plain default correctly excluded — and the gate's doc comment is now fully accurate and internally consistent. **One new LOW**: a stale provenance comment inside the test `between_items_gate_precedes_the_compact_send` (run_all.rs:2423-2424) still says the gate reads "the ceiling from the current config" — the pre-round-1-fix design phrasing — contradicting the code it sits above, the assertion message six lines below it, and the gate's own doc comment. Doc-only, one phrase.

## Scope & method

Reviewed the b461978 diff (git show), the round 1-4 reports, and the live code every claim references: loop_impl.rs (`resolve_turn_provider` :1070-1315 — all five build sites; `try_429_fallback` :1640-1717 — the sixth call site), model_resolver.rs (trait decl :135, `build_turn_provider` :366-417 — live-config reads :373-379, both CM builds :396-398/:409-411), turn.rs (`resolve_iteration_provider` :1439-1504 — the Some/None CM selection :1461-1468), factory.rs (snapshot :274, rebuilds :499-501/:589-591, accessor :608-616), agent.rs (IPC `set_model` pin CM :613), run_all.rs (gate :2765-2810, tests :2393-2438), plus a repo-wide `build_turn_provider` call-site search and a "current config" phrasing search across src-tauri. Static review; the parent's post-fix test runs (src-tauri 278/0/0 + integration 4/2) taken as given — the same standard rounds 2-4 applied.

## Task 1 — the widened enumeration covers the true set: VERIFIED

The exception's parenthetical maps one-to-one onto the five serving sites:

| Site (loop_impl.rs) | Path | Enumeration element |
|---|---|---|
| :1102 | skill slot (`[models.skill.<name>]`) | "a `[models.*]` slot serving the turn" |
| :1196 | pin 429-sticky reroute | "a 429-sticky reroute — on a picker pin" |
| :1232 | forced model | "a forced model" |
| :1278 | default-provider 429-sticky reroute | "a 429-sticky reroute — … or the default provider" |
| :1306 | state/subagent/plan-kind chain | "a `[models.*]` slot serving the turn" |

Correctly excluded (factory-built snapshot CMs — no divergence, matching the gate):

- **Plain pin** (:1205-1211): returns the stored `(p, cm)` pair, built at pin time by `factory.context_manager_for` (agent.rs:613 → factory.rs:589-591 `.with_proxy_cache_ceiling(self.proxy_cache_ceiling)` — the startup snapshot; round 3 verified the config_io.rs:291 → `set_explicit_provider` forwarding).
- **Plain default** (:1288-1295 → `None`): turn.rs:1467 serves the turn on the loop's default CM (factory-built/rebuilt with the snapshot).
- **No resolver** (:1248-1254 → `None`): same default CM.

Completeness checks beyond the five sites:

- A repo-wide `\.build_turn_provider\(` search finds exactly six production call sites, all in loop_impl.rs: the five above plus **:1701 in `try_429_fallback`** — a viability probe only: it builds the alternate CM, compares `alt_cm.max_tokens()` against the live CM's, and discards it (`.1` used for the window check, provider dropped); the turn-serving build happens later via the sticky entry through `resolve_turn_provider`. Not a turn-serving CM — correctly not enumerated. All other occurrences are the trait declaration (model_resolver.rs:135), the `ConfigModelResolver` impl (:366), and test mocks (spawn_agent.rs / list_models.rs / runtime/agent.rs / tests.rs).
- The "serving the turn" phrasing is precise: a slot/reroute/forced ref that resolves but fails to build returns `None` → the default snapshot CM serves the turn — correctly not claimed as divergent.

## Task 2 — the doc comment is accurate and internally consistent: VERIFIED

- **Leading claim**: fill rate from the LIVE loop — accurate on both engine paths (all five resolver sites pass `self.fill_rate`, the loop's value; factory-built CMs carry the factory's fill rate, the same value threaded into every loop). Ceiling from the factory's startup snapshot — accurate for every factory-built CM (snapshot set only at construction, factory.rs:274; threaded at :499-501/:589-591; no settings-save path updates it — round 2 verified settings.rs).
- **Exception**: "read the LIVE config's ceiling" — model_resolver.rs:373-379 reads `proxy_cache_ceiling_tokens` from `self.config.read()`, applied on both the cache-hit (:398) and cache-miss (:411) builds. "Diverges until restart" — the factory snapshot is set only at startup; restart rebuilds it from the new config. "Bounded, restart-healed, preflight backstop (window − headroom)" — round 3 verified context.rs:189-191; the preflight/headroom reads (model_resolver.rs:377-378) track the live config, so the backstop guards the window regardless of the ceiling divergence.
- **Structure**: claim → complete exception. No over-inclusion (every enumerated element is a real resolver-build path) and no under-inclusion (every resolver-build serving path is enumerated). The round-4 iteration is done: this sentence is now correct.

## Task 3 — one unaddressed residual: LOW-1 (the test comment's stale ceiling provenance)

**Where:** run_all.rs:2423-2424 — the comment inside `between_items_gate_precedes_the_compact_send`:

```rust
// The gate reads the LIVE loop's fill rate (what the engine's
// fill-rate path uses) and the ceiling from the current config.
```

The gate reads the ceiling from `f.proxy_cache_ceiling()` — the **factory's startup snapshot**, not the current config. The comment is the pre-round-1-fix design phrasing (the plan's step-4 design decision (b) said "the ceiling from the current config"; d92b117's round-1 fix changed the gate and the assertion but left this comment). It now contradicts, within six lines:

- the assertion it sits above — `gate_body.contains("f.proxy_cache_ceiling()")` with the message "the gate must read the proxy cache ceiling **from the factory snapshot** (the same source the engine's context managers are built from)";
- the gate's own doc comment (:2765-2773), which uses "the current config" to denote the **rejected** alternative ("review LOW-1: reading the ceiling from the current config instead would diverge from the engine's snapshot until restart").

A maintainer reading the test to learn the gate's provenance would take away the exact misconception rounds 1-4's doc work exists to prevent. Missed by the prior rounds because their repo-wide phrasing searches ("even after a mid-session", "picker pin", "429-sticky reroute") never covered "current config" in the test comment; a src-tauri-wide "current config" search finds exactly two hits — this stale comment and the gate doc's warning against it.

**Why LOW:** doc-only (a comment inside a test), zero behavioral impact, no docs-sync or user-facing implication. But it is a factually wrong provenance claim about the gate, in the changed file, checkable against the assertion message six lines below — the same defect class each prior round found once.

**Fix (one phrase, no code change):** run_all.rs:2424 — "and the ceiling from the current config." → "and the ceiling from the factory's startup snapshot." (matching the assertion message below and the gate's doc comment).

## Everything else: verified unaddressed-free

- Rounds 1-4's findings: all fixed and verified (round 2 verified round 1's; round 3 verified round 2's; round 4 verified round 3's; this round verifies round 4's). Commit chain e6aaabc → d92b117 → eeb28e7 → 4b867f6 → b461978 (HEAD); `git diff HEAD` and `git status --short` both empty.
- b461978 is doc-only (the 6 rewrapped doc lines in run_all.rs + the round-4 report file). The source-contract pins target the gate **body** (`l.fill_rate()`, `f.proxy_cache_ceiling()`, `context_usage`) — the doc change sits above the fn, so no pin can be affected; parent reports src-tauri green post-fix (278/0/0 + integration 4/2), taken as given.
- Docs sync (README, AdvancedSection), multi-platform neutrality, and security: b461978 touched none of those surfaces (a Rust doc comment + a report file); rounds 2-3's whole-commit verification of d92b117 stands.
- Round-4's "no action required" notes (the plan file's historical step-4 text; commit-message phrasing) remain no-action — immutable history, documented by the round reports.
