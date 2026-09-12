## Verdict: FINDINGS (0 high, 1 low)

Round-3 verification of the round-2 doc-only fix (commit eeb28e7, on d92b117, wt/agenticcoding, working tree clean). The fix is exactly what it claims: +6 doc-comment lines on `between_items_compact_gate` (run_all.rs:2774-2779) plus bookkeeping (plan step-6 checkbox, the round-2 report file) — no code change. The round-2 overclaim is gone and the caveat's substance is accurate: every technical anchor checks out (model_resolver.rs:379 live-config ceiling read, turn.rs:1461-1468 per-iteration CM selection, preflight backstop = window − headroom at context.rs:189-191), and nothing else from rounds 1-2 is unaddressed. **One new LOW**: the caveat's enumeration over-includes the picker pin — a plain pin's CM is factory-built with the same startup-snapshot ceiling the gate reads (no divergence after a mid-session edit); only the pin's 429-sticky-reroute sub-path runs on a resolver-built live-config CM. Round-2's finding text carried the qualifier ("a picker pin with a 429-sticky reroute"); the doc dropped it. One-phrase doc fix.

## Scope & method

Reviewed the full eeb28e7 diff (git show), the round-1 and round-2 reports, and the live code every claim in the caveat references: run_all.rs (gate :2765-2810, helper :2739-2763), model_resolver.rs (`build_turn_provider` :366-417 — live-config reads :373-379, CM builds :396-398 and :409-411), turn.rs (`resolve_iteration_provider` :1439-1504 — the Some/None CM selection :1461-1468), loop_impl.rs (`resolve_turn_provider` :1070-1315 — skill slot :1086-1113, pin :1134-1217, forced model :1218-1247, state chain :1248-1315; `set_explicit_provider` :1568-1612), agent.rs (IPC `set_model` :573-690 — the factory CM build :613), config_io.rs (`swap_provider_into_loop` :277-310 — verbatim forward at :291), factory.rs (`context_manager_for` :586-592), context.rs (preflight hard ceiling :189-191). Plus a repo-wide search for the overclaim phrasing. Static review; the parent's post-fix test runs (src-tauri 278/0/0 + integration 4/2) taken as given, the same standard round 2 applied.

## Task 1 — Does the caveat accurately describe the exception?

Verified claims, each against the code:

- **"a `[models.*]` slot serving the turn"** — accurate. The skill slot (loop_impl.rs:1097-1102) and the workflow-state/subagent/plan-kind chain (:1256-1306) both build via `resolver.build_turn_provider` → live-config CM.
- **"a forced model"** — accurate. loop_impl.rs:1232, `resolver.build_turn_provider(&forced, self.fill_rate)` → live-config CM.
- **"resolver-built CMs that read the LIVE config's ceiling"** — accurate for those paths. model_resolver.rs:373-379 reads `config.general.context.proxy_cache_ceiling_tokens` from `self.config.read()` (the live config handle), applied on both the cache-hit (:398) and cache-miss (:411) CM builds.
- **turn.rs per-iteration CM selection** — accurate. `resolve_iteration_provider` re-runs every iteration; :1465-1468 — `resolve_turn_provider` `Some` → the resolver-built CM serves the turn, `None` → the loop's default (factory-built) CM.
- **"a mid-session ceiling edit diverges until restart"** — accurate for the slot/forced paths: the factory snapshot is set only at startup, the resolver reads live per-iteration.
- **"bounded, restart-healed"** — accurate: restart rebuilds the factory from the new config; divergence magnitude is capped by the ceiling delta.
- **"the preflight backstop (window − headroom) still guards the window"** — accurate, and more precise than round 2's "(window − 32k)": context.rs:189-191 defines the preflight hard ceiling as `max_tokens − compact_headroom_tokens`, where headroom is configurable (default 32_000).

**One inaccuracy — see LOW-1 below: the "a picker pin" element is over-broad.**

## LOW-1 (round 3): the caveat's pin element over-includes plain picker pins

**Where:** run_all.rs:2774-2775 — "per-context-override turns (a `[models.*]` slot serving the turn, a picker pin, or a forced model) run on resolver-built CMs that read the LIVE config's ceiling".

**Evidence chain.** The pin path has two sub-paths, and only one is resolver-built:

- **Plain pin (the common case):** `resolve_turn_provider` returns the *stored* `(p, cm)` pair (loop_impl.rs:1135-1137, :1205-1211). That CM was built at pin time by `factory.context_manager_for` (agent.rs:613; factory.rs:588-592 applies `.with_proxy_cache_ceiling(self.proxy_cache_ceiling)` — the **startup snapshot**), forwarded verbatim through `swap_provider_into_loop` (config_io.rs:291) into `set_explicit_provider` (loop_impl.rs:1608-1611). The deferred-swap completion stores the same CM. So a plain pin turn runs on a factory-built CM carrying the **same snapshot ceiling the gate reads** — after a mid-session ceiling edit, gate ≡ pin-path threshold; **no divergence**.
- **429-sticky reroute:** loop_impl.rs:1193-1204 — `resolver.build_turn_provider(&alt, self.fill_rate)` → live-config CM (model_resolver.rs:379). This sub-path — and only this one — diverges.

Round-2's finding text was precise: "a picker pin **with a 429-sticky reroute**". The doc (and the eeb28e7 commit message) dropped the qualifier, so the comment now tells a maintainer that pin turns read the live config's ceiling — false for the no-reroute pin, which actually *matches* the gate.

**Why LOW:** doc-only, zero behavioral impact, and the error is in the conservative direction (claims divergence where there is none, rather than the round-2 defect of claiming uniformity where there is none). The material exception — the `[models.*]` slot and forced-model paths, plus the pin's reroute sub-path — is correctly documented, as are all the bounding properties. But this sentence was added specifically to make the provenance claim accurate, one of its three enumerated paths mis-describes the mechanism, and it is checkable against the round-2 report it cites by name.

**Fix (one phrase, no code change):** restore the round-2 qualifier — e.g. "a picker pin (on a 429-sticky reroute)" or "a picker pin with a 429-sticky reroute" — in run_all.rs:2774-2775.

## Task 2 — Internal consistency: verified

- The leading uniformity claim ("the gate's threshold matches the dial the engine actually applies even after a mid-session settings edit") is now immediately followed by the explicit Exception paragraph — claim → exception, so no unconditional overclaim remains in the comment as a whole.
- Repo-wide search: the only "even after a mid-session" occurrence in the codebase is this very line (now qualified); README.md carries no mid-session/provenance claim (round 2 verified its sentence and both AdvancedSection help texts describe the gate accurately; eeb28e7 didn't touch them).
- The fill-rate half of the leading claim is accurate on **both** engine paths (resolver CMs are sized with the loop's fill rate — loop_impl.rs:1102/:1196/:1232/:1278/:1306 all pass `self.fill_rate`), so only the ceiling needed the exception — exactly how the comment is structured. The remaining defect is the exception's own accuracy (LOW-1), not consistency.

## Task 3 — Nothing else unaddressed: verified

- eeb28e7 touches exactly three files: run_all.rs (+6 doc lines), the plan step-6 checkbox flip, and the round-2 report (new file). No code change → round 2's whole-commit verification of d92b117 (correctness, tests, docs sync, multi-platform neutrality, security) stands unimpaired.
- The source-contract test pins are unaffected — the added lines sit above the fn; `gate_body.contains("f.proxy_cache_ceiling()")` etc. still match. Parent reports src-tauri green post-fix (278/0/0 + integration 4/2), taken as given.
- Working tree completely clean (`git diff HEAD` and `git status --short` both empty); HEAD = eeb28e7 directly on d92b117.
- Round-2's "no action required" notes (the unconditional-subscribe note; client_factory.rs:247 test-only path) remain no-action.

## Notes (no action required)

- The eeb28e7 commit message repeats the same over-broad pin phrasing; commit messages are immutable history — this report documents the correction.
- The plan file's step-4 text still describes the round-1 design ("the ceiling from state.project.config") — historical plan text, superseded by the round-1 fix; not a doc-sync gap (round 2 already assessed PLAN.md).
