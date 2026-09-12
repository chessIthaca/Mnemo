## Verdict: FINDINGS (0 high, 1 low)

Round-2 verification of commit d92b117 (12 files, +410/−29) on wt/agenticcoding. **Round-1 LOW-1 is resolved**: the gate's ceiling now traces to the factory's startup snapshot — the exact value the engine's default-path context managers carry — which is round-1's suggested fix (a) in substance, with no regressions (accessor, blind path, test pin, locks all verified; both suites green under `deny(warnings)`). **One new LOW**: the gate's doc comment claims the threshold matches "the dial the engine actually applies even after a mid-session settings edit" unconditionally, but the per-context-override path (resolver-built per-iteration CMs) reads the ceiling from the LIVE config — on that path the gate diverges after a mid-session ceiling edit (the mirror of round-1's LOW-1, which round 1 itself documented as the exception). The code is the best available single-provenance choice; only the comment needs a caveat. Doc-only fix, no code change requested.

**Scope & method:** reviewed the full d92b117 diff (`git show`), the round-1 report, and the surrounding live code: factory.rs (field :143-146, snapshot :274, rebuilds :501/:591, accessor :614, `with_fill_rate` :767), run_all.rs (gate :2765-2804, `compact_then_dispatch_next` :2819-2864, tests), context.rs (shared helper + delegation), loop_impl.rs (`fill_rate` accessor :758, `resolve_turn_provider` :1070-1315, `with_fill_rate` :1021), turn.rs (per-iteration CM selection :1440-1504, the `effective_summarize_at` trigger contract :1506-1508), model_resolver.rs (`build_turn_provider` :366-417 — the live-config ceiling read :379), settings.rs (`resync_runtime_state` :240-309 — confirms no save path updates the factory's stored ceiling), state.rs, main.rs (startup CM :1424-1432, resolver wiring :1451-1459), events.rs, README/AdvancedSection. Static review; the parent's test runs (root 2133/0/4, src-tauri 278/0/0, frontend 1060/75, both builds clean) taken as given.


### Round-1 LOW-1 — verified resolved (the default path)

The provenance chain, all four links checked in the live code:

1. **Startup snapshot**: main.rs:1424-1432 builds the startup CM with `config.general.context.proxy_cache_ceiling_tokens`; factory.rs:274 snapshots it (`context_manager.proxy_cache_ceiling()`) into the private field (declared :143-146, `Option<usize>`).
2. **Every factory-built/rebuilt CM carries that snapshot**: `set_provider` (factory.rs:499-501) and `context_manager_for` (:589-591) both `.with_proxy_cache_ceiling(self.proxy_cache_ceiling)`. The settings-save resync path (settings.rs:279-284) also goes through `context_manager_for`, so even a provider swap after a Settings save keeps the snapshot. The field is private and set only in `new` — no settings-save path updates it (confirmed against settings.rs:890-895, which pushes only `core_operations` and `shell_filter`).
3. **The engine's default path uses exactly that CM**: turn.rs:1461-1469 — when `resolve_turn_provider` returns `None` (no skill/pin/forced/state override), the iteration runs on `default_context_manager` (the loop's own CM, factory-built and factory-rebuilt with the snapshot). The fill-rate trigger is `token_count >= context_manager.effective_summarize_at()` (turn.rs:1506-1508 contract).
4. **The gate reads the same value**: run_all.rs:2798-2802 → `f.proxy_cache_ceiling()` (factory.rs:614-616, returns the snapshot field). `fill_rate` is likewise uniform on both engine paths: the factory threads `self.fill_rate` into every loop (factory.rs:767), and the resolver's per-turn CMs are sized with the LOOP's fill rate (loop_impl.rs:1102/1196/1232/1701 pass `self.fill_rate`), so `l.fill_rate()` is the value both paths use.

Net: for the default path, gate threshold ≡ engine `effective_summarize_at` bit-for-bit (same shared helper, same inputs), before AND after any mid-session edit. This is round-1's suggested fix (a) in substance — reading the loop's own CM ceiling would return the identical value, since the loop's CM is always factory-built or factory-rebuilt with the snapshot.

### Finding LOW-1 (round 2) — the gate's doc comment overclaims uniformity; the per-context-override path still diverges after a mid-session ceiling edit

**Where**: run_all.rs:2765-2775 (the gate's doc comment) — the claim "the gate's threshold matches the dial the engine actually applies even after a mid-session settings edit".

The engine has TWO CM provenances for the fill-rate path:

- **Default (no override)**: the loop's own CM — factory snapshot ceiling. The gate now matches this exactly.
- **Per-context override** (a configured `[models.*]` slot serving the turn, a picker pin with a 429-sticky reroute, or a forced model): `resolve_turn_provider` returns a resolver-built CM (turn.rs:1465-1466), and `ConfigModelResolver::build_turn_provider` reads the ceiling from the resolver's LIVE config handle (model_resolver.rs:379) — the handle the IPC layer pushes reloaded config into after every Settings save (main.rs:1451-1455). Per-iteration CMs on this path carry the NEW ceiling immediately.

After a mid-session `proxy_cache_ceiling_tokens` edit (before restart), the gate computes `min(product, old_ceiling − 32k)` while the override path's engine threshold is `min(product, new_ceiling − 32k)` — the mirror image of round-1's LOW-1 (round 1's live-config gate matched this path and diverged from the default path; the fix flips it). Round 1 documented this exact exception in its finding, and its suggested fix (a) has this consequence — so this is a known residual of the chosen direction, not an implementation oversight.

**Why LOW and doc-only**: (i) the code choice is the best available — no single static ceiling read can match both paths, because the engine itself applies two different ceilings after a mid-session edit; the default path (no `[models]` overrides) is the common configuration and the one round 1 framed as "the engine's" provenance; (ii) impact is bounded identically to round-1's LOW-1 — self-heals on restart, magnitude capped by the ceiling delta, the preflight backstop (window − 32k) still guards the window, and the gate is an optimization, never a blocker; (iii) it is triple-conditioned: an override actually serving the main agent's turns AND a mid-session ceiling edit AND before restart.

The defect is that the doc comment asserts the invariant unconditionally — a future maintainer reading "matches the dial the engine actually applies even after a mid-session settings edit" would not know the override path is excepted. **Fix**: qualify the comment with one sentence (e.g. "Exception: per-context-override turns run on resolver-built CMs that read the live config's ceiling, so on that path a mid-session edit diverges until restart — bounded, restart-healed, and the preflight backstop still guards the window"). No code change requested.


### Regression checks on the fix (task checklist #2)

- **Factory accessor**: correct field (`self.proxy_cache_ceiling`, declared factory.rs:143-146 as `Option<usize>` with an accurate doc comment), correct type, returns the startup snapshot — the same value threaded at :501/:591. Compiles green under `deny(warnings)`.
- **None-factory blind path**: `None => return true` — blind → compact, consistent with the other blind arms (no main agent, no loop, no usage / zero window). In practice factory `None` ⇒ brain failed to build ⇒ no main agent, so the earlier arm fires first; defensive but consistent with the documented blind semantics.
- **Locks**: the fix REMOVED the config-lock read. The gate now takes manager → context_usage → agent_loops sequentially (each scoped and dropped before the next) and then reads the factory field lock-free (`Option<Arc<AgentLoopFactory>>`, immutable after startup) — strictly simpler than round-1's four-stage version; no lock-ordering concerns.
- **Test pin**: `gate_body.contains("f.proxy_cache_ceiling()")` matches the gate body (`Some(f) => f.proxy_cache_ceiling(),`); the old config-read assertion is gone; the `l.fill_rate()` and `context_usage` pins still hold, as do the gate-before-send and skip-log pins (run_all.rs:2827 gate, :2839 send, :2862 skip log).
- **Behavior delta vs the round-1-reviewed version**: the missing-loop case now returns `true` (compact) instead of continuing with a 0.5 fill-rate default — round 1 already established that arm is unreachable (usage `Some` ⇒ loop exists; the Exited handler removes both together), and blind→compact is the documented conservative default. Not a regression.

### Whole-commit pass (task checklist #3)

1. **Gate logic & wiring**: gate runs before the compacting flag is set (a skip never flashes "compacting…"); the skip path logs and falls through the unchanged shared tail (run-state re-check → dispatch); the compact-path body is the old code verbatim inside `if should_compact`; the call-site gate (`item_resolved && auto_compact_on_plan_complete`) is untouched → off (default) is byte-identical. The pre-existing compact-ordering contract still holds (subscribe is unconditional before the `if`, so subscribe < send < wait < dispatch).
2. **Shared helper (context.rs)**: the delegation is bit-identical — `effective_summarize_at` now computes `effective_summarize_threshold(self.max_tokens, self.fill_rate, self.proxy_cache_ceiling)`, and the helper's `(max_tokens as f64 * fill_rate) as usize` is the same expression `new()` used for `summarize_at`; all three fields are immutable post-construction (no mutators; the two builders are pre-use). `summarize_at` is still read (factory.rs:264 derives fill_rate from it) — no dead code.
3. **Usage map (events.rs / state.rs / main.rs)**: the ContextUsage arm inserts `(u64::from(*used), u64::from(*max))` keyed by agent id; the Exited arm removes. All three construction sites initialize the map. Conversions are lossless (u32→u64; `as usize` on 64-bit targets).
4. **Tests**: 3 unit tests (below/at/above, ceiling cap, blind), 2 source-contract tests (gate-before-send + provenance pins; forwarder insert + Exited cleanup), 2 context.rs tests (threshold mirror + ceiling cap). All present in the diff and green per the parent's runs.
5. **Documentation sync**: README's extended sentence and both AdvancedSection help texts accurately describe the gate (README.md:126; AdvancedSection.tsx fill-rate help ~:363-371, auto-compact checkbox help ~:550-560); PLAN.md has no stale auto_compact mentions. The one doc gap is the new LOW-1 above (the gate's own doc comment).
6. **Multi-platform neutrality**: no platform-specific code, paths, or shell syntax anywhere in the diff. Clean.
7. **Doc comments**: present on all new public items (the shared helper, both accessors, the state field) and on the private fns (`should_between_items_compact`, `between_items_compact_gate`). Warning-free build proven by the green suites under `deny(warnings)` at both crate roots.
8. **Security**: the map and gate read only app-internal, engine-computed token counts; no user input flows in; no new attack surface.

### Round-1 leftovers (task checklist #4)

Round 1 returned exactly one finding (LOW-1) — addressed by this fix and verified above. Its three "no action required" notes: the `fill_rate.unwrap_or(0.5)` note is moot (the gate now returns `true` on a missing loop — the blind default); the unconditional-subscribe note still holds with zero impact; the plan-file staleness note was informational. Nothing else from round 1 was left unaddressed.

### Notes (no action required)

- The only uncommitted change is the plan file's step-6 checkbox flip (`M .coding/plans/47cce50b.md`) — expected mid-closing-sequence bookkeeping; it rides the final commit.
- `client_factory.rs:247 context_manager_for_model` also reads the ceiling from a passed config, but it has no production caller in this repo (test-only) — not a gate-relevant path.
