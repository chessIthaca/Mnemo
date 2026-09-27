## Verdict: PASS

Round-2 delta review for pre-prompt model routing (plan d69abf72, backlog 091e694d), branch wt/mnemo. Scope: commit c9dc2f9 (the round-1 fix commit, diffed against b7d6ab5) plus the uncommitted remainder. All four round-1 findings are fixed correctly in the delta; no new findings.

### Scope verified

- `git log`: branch is b62671c (main fork) → b7d6ab5 (feature, round-1-verified) → c9dc2f9 (review fixes). HEAD = c9dc2f9.
- Read the c9dc2f9 commit diff (topic + key hunks), then verified every fix hunk in the current source: `src/agent/turn.rs` (loop top, `route_turn_start`, `TurnRoute`), `src/agent/model_routing.rs` (`RouteDecisionRow` docs), `src/agent/tests.rs` (regression test), `frontend/.../ClassifierSection.tsx` + `ClassifierSection.test.ts`, `docs/CONFIGURATION.md`.
- Uncommitted diff: one line — the harness stamping round-2's base revision into `.coding/plans/d69abf72.md`. Bookkeeping, accurate (bases match the actual review rounds).

### Per-finding verification

**L1 — iteration counter placement (turn.rs:429–435): FIXED.** The counter now sits after the deferred-swap early exit (lines 425–427 `return Ok(outcome)`), so a swap interrupt that ends the turn before any request is built never counts. Read the loop top as instructed and agree with the move: the row's field is documented as "Provider-request iterations this turn ran" (turn.rs:132), and the increment is the only writer — pre-fix, an interrupt-during-pre-swap-summary turn reported `iterations: 1` with zero requests. One line relocated, no other side effects. *Disclosed untested behavior:* no regression test — arming a mid-turn swap requires the pick-command channel, and the change is a pure statement relocation; acceptable as disclosed. Note (not a finding): the counter still counts an iteration whose provider resolution subsequently fails — that is a genuine provider-request *attempt* and was not round 1's finding.

**L2 — memoized routing miss (turn.rs:1069–1077, model_routing.rs:209–217): FIXED.** `arm_target = if routed.is_some() { decision.target } else { None }` — an unresolved turn-start target is memoized as a miss, so the decision row's `enforced: false, model: None` can no longer be contradicted by a mid-turn Settings save. Verified the two required properties:
- *Per-turn:* `TurnRoute` is created fresh in `route_turn_start`, called once per `run_turn` (turn.rs:414, `TurnState::fresh()` at 406) — a later turn re-classifies and re-resolves; the memoized miss cannot block it.
- *Shadow never arms:* in shadow the `(true, _)` match arm makes `routed` always `None`, so `arm_target` is `None` — shadow stays log-only.
- Regression test `routing_memoizes_an_unresolved_target_so_a_mid_turn_target_cannot_route` (tests.rs:13905) genuinely exercises the changed path: `LateTargetResolver::resolve_routed` returns `None` on call 1 (turn start) and the cheap model afterwards, then asserts the turn served `from-default`, the resolver was called exactly once (the arm never re-resolved), and the decision row records the miss. The per-iteration re-resolve path (armed case) remains covered by the existing armed-route tests from round 1.
- The `model` field doc (model_routing.rs:209–217) now spells out both mid-turn directions — armed-but-context-switched and unresolved-then-appears — accurately.

**L3 — enforce-switch wording (docs/CONFIGURATION.md:13, ClassifierSection.tsx:452–461): FIXED.** The disabled expression is `disabled={status !== "ready" && !routingEnforce}`: not-ready + off → disabled (ON is locked); on → never disabled (OFF always works). Docs line 13 and the in-section hint ("locked ON until the classifier is ready … switching it back off always works") state exactly that, and the source-contract test pins both strings. Docs and UI agree with the actual expression.

**L4 — confidence-gate input guard (ClassifierSection.tsx:515–526): FIXED.** `const next = raw === "" ? Number.NaN : Number(raw)` — an empty edit reads as NaN, not `Number("") === 0`; non-finite or out-of-range edits snap the field back to the committed gate via direct DOM mutation (correct for a controlled input whose state didn't change). Guard soundness checked:
- *0 cannot sneak through:* an empty/whitespace edit never commits — and while `Number(" ") === 0`, the `type="number"` input sanitizes invalid input to `""` (HTML value-sanitization algorithm), so whitespace lands in the `raw === ""` branch. An explicit typed "0" is a legitimate in-range edit (the user deliberately gating nothing) — not the bug the fix targets.
- *Snap-back cannot fight legitimate edits:* any finite 0..1 value commits; typing "0.05" passes through intermediate "0" → commits 0 → commits 0.05, the normal controlled-input sequence. Snap-back fires only on "", NaN, <0, >1.
- Pinned by two source-contract tests (test.ts:281–299). *Disclosed limitation, not a finding:* they are `toContain` source contracts, not behavioral render tests — they pin the guard expression, consistent with this test file's established pattern.

### Constitution checks

- **Documentation sync:** docs/CONFIGURATION.md enforce wording updated in the same delta as the behavior it describes; module doc comments (turn.rs loop-top and `model` field) updated with the fixes. Nothing stale in the delta.
- **Multi-platform neutrality:** delta is pure Rust/TS with no platform APIs, paths, or shell syntax. Holds on macOS and Windows.
- **File-tools-first:** no shell-based file mutation in the delta; no `#[allow(...)]` anywhere in the touched code.
- **Warning-free build:** dispatcher evidence `cargo test` exit 0 under `#![deny(warnings)]`, frontend 1305/1305, `npm run build` clean — consistent with the read code; nothing in the delta could newly warn.
- **Bookkeeping accuracy:** backlog.jsonl, the decision knowledge doc, and both plan files match what shipped; the plan's `## Reviews` bases match the actual round boundaries.

### Verdict

The delta is exactly the four round-1 fixes, each correctly implemented, with the L2 regression test exercising the changed path and the two claimed-but-untested behaviors (L1 relocation, L4 source-contract tests) disclosed. No new findings: **PASS**.

Reviewed-state: c9dc2f99816cd4b5a9651beb2366e0b3e20f0d21
