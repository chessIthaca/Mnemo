## Verdict: PASS

Round-2 verification of commit `d4853ff` on `wt/agenticcoder` (plan 0aeb92cd). Both round-1 findings are verifiably fixed as specified, the required regression test is present and non-vacuous, and the doc/knowledge updates are in place. Working tree is clean (`git status`/`git diff HEAD` empty), so the commit is exactly the reviewed state. Scope reviewed: the full `d4853ff` diff — `src/agent/loop_impl.rs`, `src/runtime/agent.rs`, `README.md`, the new knowledge/plan/review files, and `backlog.jsonl` (user bookkeeping, already vetted round-1).

### Finding 1 (HIGH) — pin path ignored 429 stickiness: FIXED

`resolve_turn_provider` pin branch (`src/agent/loop_impl.rs:877-903`) now does exactly what round-1's concrete fix prescribed:
- Builds `pinned_ref` from the pinned provider (`provider_name()`/`model()`), consults `self.sticky_endpoint(&pinned_ref)` (887), and on a hit builds the alternate via `resolver.build_turn_provider(&alt, self.fill_rate)` (889-890), sets `resolved_model`/`resolved_provider` from the **built alternate** (892-895 — spans reflect the endpoint actually served), and returns it (896).
- On a miss — no sticky entry, no resolver attached, or the build fails — it falls through to the unchanged `set_resolved_*` + `return Some((p, cm))` (900-902): the pinned provider is still preferred when no stickiness applies. ✓
- Priority order intact: skill override (843-865) still beats the pin; the pin still beats forced model (912) and the state/subagent chain. ✓
- Endpoint reroute only: `sticky_endpoint` is keyed by model id and entries are inserted with the alternate from `find_alternate_endpoint(&model, …)` (1323), so the pin's model choice never changes — the same guarantee the other four paths get. ✓
- Consistency on the 429 itself: the pin branch sets `resolved_model`/`resolved_provider` from the pin before returning, so `try_429_fallback` reads the correct model+endpoint and the sticky entry matches the later `pinned_ref` lookup. ✓
- Doc comments updated as claimed: `fallback_endpoints` field doc (200-203) and `try_429_fallback` doc (1282-1284) both now list the explicit picker pin in the consulted-path set. ✓

Degradation edge (same as the forced branch, not a regression): if the sticky alternate's `build_turn_provider` fails after a config reload, the pin is returned as before; a re-429 is then terminal via `tried_fallback` with the actionable message.

### Finding 2 (LOW) — smaller-context alternate overflowed later sticky turns: FIXED

`try_429_fallback` (`src/agent/loop_impl.rs:1336-1345`) now implements round-1's option (b) verbatim: the validation build (`build_turn_provider(&alt, …)?.1`) still proves buildability, and if `alt_cm.max_tokens() < self.context_manager.read()…max_tokens()` it returns `None` **before** the sticky insert at 1352 — no entry is recorded, the turn fails once with the actionable message, and later turns keep the original endpoint. The comparison target (live CM budget) is the conservative superset of "the live conversation" and is exactly what round-1's concrete fix specified; on the pin path it correctly guards against the pin's window. The doc's Returns-None list (1294-1299) mentions the condition and that no sticky entry is recorded, and the body comment (1324-1335) documents why the old `pending_swap` deferral is replaced. ✓

### Regression test — present, non-vacuous, exercises the pin branch specifically

`picker_pin_429_fallback_reroutes_endpoint` (`src/runtime/agent.rs:2174-2310`):
- Pins `pin-model@primary` via `set_explicit_provider` with `ContextManager::new(128_000, 0.5)`; `FallbackTestProvider.max_context = 128_000` equals the live CM, so the swap is immediate (no `pending_swap` deferral interferes).
- **Pin-branch specificity:** `PinResolver::resolve` returns `None` unconditionally (no `[models.*]` overrides), no skill is active, and the pin is set — `resolve_turn_provider` reaches step 2 (pin) before the resolver chain on every iteration; the resolver only serves `build_turn_provider`/`find_alternate_endpoint`. ✓
- Assertions match the claim: turn finishes (`finished`), non-terminal (`!terminal`), a 429 switching note was emitted, `pinned_calls == 1` (exactly one attempt on the pinned endpoint), `fallback_calls >= 1` (alternate served the retry).
- **Fails on the round-1 code:** the retry would re-enter the pin branch and re-hit the 429 → `tried_fallback` → terminal failure → `assert!(!terminal)` fails, `pinned_calls == 2` → `assert_eq!(…, 1)` fails, `finished == false`. Three independent assertions trip, so the test cannot pass vacuously.
- Viability path is coherent in-test: alt CM `max_tokens` (128_000·fill_rate) equals the live CM's, not less, so the Finding-2 guard correctly does not suppress this fallback.
- `wait_for_turn_outcome` (1778-1808): 5s deadline + 500ms recv timeout, detects the switching note (`retrying && contains "429" && contains "switching"`), terminal errors, and `Finished` — inherited robust pattern; a regression fails loudly rather than hanging.
- The two round-1 tests (`state_override_survives_429_fallback` :1811, `default_path_429_fallback_does_not_pin` :2002) are unchanged in substance and now share the 3-tuple helper.

### Tests — could not execute (no shell in reviewer surface); static + recorded evidence

My tool surface is read-only by construction (reads, git reads, graph tools, memory queries, `write_review_report`) — same limitation round-1 documented. I could not run `cargo test picker_pin_429_fallback_reroutes_endpoint state_override_survives_429_fallback default_path_429_fallback_does_not_pin` or the full suite myself. Evidence relied on instead: plan 0aeb92cd step 4 is checked off ("Run the regression test + the full test suite (cargo test unpiped, warning-free)") and the session memory record confirms both findings fixed and committed at d4853ff. Static warning-risk scan of the diff is clean (no unused imports — each test's `use ModelResolver` is consumed; underscore-prefixed unused bindings; all new fields initialized at both constructors, lines 527/575). Recommend the main agent re-confirm the three named tests green in its closing sequence, as it already planned.

### Docs / project-rule checks

- **Knowledge file** (`.coding/knowledge/bug/2026-09-01-…md`): the sticky-endpoint path list now reads "skill, the explicit picker pin, state/subagent override, forced model, and the default-provider path", the viability check is described, and all three regression tests are named. ✓
- **README.md:59**: updated with "the alternate stays sticky for that model id on later turns, but it is endpoint routing only — per-phase model overrides … keep switching". It does not name the picker pin explicitly, but round-1 judged this user-facing sentence accurate as-is and required pin wording only in the two doc comments + knowledge file (all done); the sentence's "endpoint routing only" covers the pin path's behavior correctly.
- **Multi-platform neutrality:** pure platform-neutral Rust (locks, maps, atomics, tokio tests); no OS-specific APIs, paths, or shell syntax in the diff. ✓
- **Security:** no `unsafe`; map keys are config-derived model-id strings with no interpolation into commands/paths. ✓

### Non-blocking notes (no action required for PASS)

1. Knowledge-file wording nit: "…+ picker_pin_429_fallback_reroutes_endpoint (…) — **both** failed on the pin (exec-model never called, left: 0)" — "both" is a leftover from the two-test version; three tests are now listed, and the pin test fails differently (terminal error + `pinned_calls == 2`). Cosmetic; the root-cause/fix content is accurate.
2. The sticky-hit reroute rebuilds via the resolver, so the pin's pre-resolved reasoning effort is not carried into the fallback request — the same accepted tradeoff as the forced-model branch (noted round-1, unchanged).

All round-1 findings are verifiably fixed as specified; the fix is committed on the working branch with the required regression test. PASS.
