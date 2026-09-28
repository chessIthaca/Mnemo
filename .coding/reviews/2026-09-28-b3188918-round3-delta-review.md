## Verdict: FINDINGS (0 high, 1 low)

Round-3 delta review of plan b3188918 on `wt/mnemo`: commit 71c8d9e against its parent 60878df (confirmed via `git log` — exactly one commit in range, touching the three dispatch-named files). Read: the full 71c8d9e diff (plan-file review stamp, the round-2 report now committed, and both `EndpointCard.tsx` hunks), then the changed code in context — `EndpointCard.tsx:100–263` (`capsRefModel`/`refEfforts`/`refEffectiveEffort`/`refEffortsFillable`/`refResolvedEffort`, the autofill effect and its deps, caps discovery via `applyServerModels`), `:425–504` (the `supports_reasoning_effort` checkbox + effort dropdown), `:563–665` (the caps-hints block, all four note/hint branches and their gates), `:775–804` (per-model dropdown gating), and `types.ts:330–448` (`capsAutofillPatch`, `effortAutofillEligible`, `effortSurvivesAllowList`, `effortFromSelectValue`). Uncommitted tree = one plan-file line (the round-3 stamp) — bookkeeping only. Verification state (tsc, vitest 136, cargo at 60878df) taken per dispatch, not re-run.

The round-2 LOW-1 fix itself is correct and complete for every switch-on state. One residual state — the `supports_reasoning_effort: false` interaction the dispatch named — renders a clamp claim the request path cannot honor; it is LOW because the direction is safe and the imprecision is inherited from round-2-verified helper semantics, but the delta newly widens it to the unconfigured case.

### Item 1 — full state enumeration (switch ON)

Prerequisites for anything effort-related to render: `refCaps` present, reference model in `endpoint.models` (block gate at 571), and `refEfforts.length > 0` (gate at 617). With those, the cross-product:

| configured list | autofill | renders | truthful? |
|---|---|---|---|
| set, differs from reported | n/a (list non-empty → never eligible) | amber conflict note: "reports X (configured Y)" + Apply | ✓ factual both sides |
| set, equal to reported | n/a | nothing | ✓ nothing to do |
| empty | eligible | muted autofill note ("auto-fills with them") | ✓ mirrors `capsAutofillPatch` by construction (same helper, same inputs, same render) |
| empty | declined (effective ∉ list, ≠ "off") | amber clamp hint naming `refResolvedEffort` + Apply | ✓ — value sent = configured ?? app default "max", outside list ⇒ backend clamps to first entry; naming the resolved value is exactly right |
| empty | declined, configured "off" | **not declined** — `effortSurvivesAllowList` returns true for "off" unconditionally, so this is the eligible row (note branch). Dropping the `!== "off"` sub-check was behaviorally dead: "off" can never reach the hint | ✓ |
| `capsRefModel` null / `refEfforts` null or empty | n/a | nothing (616–617 gate) | ✓ no discovered list, nothing to do |
| model absent from `endpoint.models` | n/a | nothing (571 gate — ghost model) | ✓ Apply would no-op via upsert pruning |

No silent should-inform state remains **while the switch is on**. The one untruthful state is the switch-off row — the finding below.

### Item 2 — is `!refEffortsFillable` (empty list) exactly the clamp-risk case?

Almost. The three alternates the dispatch named:

- **Invisible configured effort** (hand-edited config value outside the ladder, e.g. `"xhigh"`): if it's in the reported list it survives → note branch; if not, the hint names it truthfully (the backend clamps any configured value outside the list). No wrong wording.
- **Model absent from `ep.models`**: the render gate at 571 excludes it before the hint can render. (`refEffortsFillable` uses `effortAutofillEligible`, which skips the membership check `capsAutofillPatch` has — but the divergence is never visible since the block doesn't render. Noted, not a defect.)
- **`supports_reasoning_effort: false`** — this one breaks the equivalence, and it is the finding:

### LOW 1 — the clamp hint renders a claim the request path cannot honor when `supports_reasoning_effort` is unchecked

State: user unchecks "Supports reasoning effort" (checkbox title: "Unchecked = omit the reasoning control entirely") on an endpoint whose `/v1/models` nonetheless reports effort levels for the reference model (caps discovery — `applyServerModels`/`capsById` — never consults the switch; the caps block at 571 isn't gated by it either). Per-model list empty, effective effort (configured or app-default) not in the reported list.

- Request-time reality while the switch is off: the reasoning control is omitted entirely — no effort is sent, nothing can clamp. (Consistent app-wide: `StatusBar.tsx:218` resolves an unsupported endpoint's effort to "off", and "off" always survives a list.)
- `effortAutofillEligible` (types.ts:361–375) resolves the effective effort as `configured ?? "max"` **ignoring the switch**, declines the fill, and the hint (621) then claims "applying them would clamp the effective effort (**max** is not in the list)" — naming an effort that isn't in play and a clamp that cannot occur until the switch is re-enabled.

Why LOW and why reported against this delta rather than remarked as pre-existing: for a *configured* non-"off" effort this imprecision existed at 60878df and round 2 accepted it, but the fix's dropped `!= null` gate **newly renders it in the unconfigured state** (switch off + nothing configured → hint now shows "(max …)" where pre-fix it was silent — and silence was correct there, since nothing needs doing while the switch is off). The hint↔autofill consistency invariant (LOW 1's actual design) is intact — the hint faithfully mirrors the helper's decision; the imprecision lives in the helper's effort resolution ignoring the switch. Failure direction is safe (over-caution, no write, no downgrade). Suggested fix: when `supports_reasoning_effort === false`, render nothing for the empty-list effort branch (nothing needs doing), or resolve the effective effort to "off" in the card's `refResolvedEffort` and the helper's chain alike (which also makes the declined-because-declined fill correctly eligible-but-harmless while off).

### Item 3 — the hint can never render a null/empty value

`refResolvedEffort = refEffectiveEffort ?? "max"` renders only inside the empty-list branch, where `capsRefModel` is non-null. `refEffectiveEffort` is `null` or a string; `null` maps to "max"; `"off"` can never appear here ("off" always survives → eligible → note branch instead). UI writes only ladder values or null (`effortFromSelectValue`: sentinel → null, else passthrough of `REASONING_EFFORTS` options). The one theoretical blank — a hand-edited `reasoning_effort: ""` in config.toml, which `??` doesn't catch — is pre-existing exposure (the old code displayed `refEffectiveEffort` directly under the same non-null gate), unchanged by this delta, and outside the UI write contract. No finding.

### Item 4 — no new defect in the fix itself

- `refResolvedEffort` is a render-scope const derived from `endpoint`; it is not an effect dependency and the effect's dep array (`[capsRefModel, refCaps, endpoint]`) is untouched — no re-run, no loop.
- No stale closure: note and hint both compute from the same render's `endpoint` through the same helper with the same inputs (re-established at 140–150 and via `effortAutofillEligible`'s internal `modelConfigFor` on the same `endpoint`).
- Note/hint mutual exclusion holds structurally: the autofill note (659) requires `refEffortsFillable`; the hint's empty-list arm (621) requires `!refEffortsFillable`; the conflict-note arm requires a non-empty configured list, which forces not-fillable. The two hint arms are the arms of one ternary. Never both, never double.
- No setState added; no extra render.

### Round-2 commit-message claim — now holds

"the declined state gets its own actionable hint instead of silence": with this delta, **every declined state with a discovered non-empty list renders the hint** — including the previously-silent fresh-endpoint/unconfigured/"max"-omitted state that round 2 flagged. The claim is no longer overbroad. (The new commit's own stronger phrasing — "renders a truthful note, an actionable hint, or nothing that needs doing" — is the one LOW-1 above overstates, in the switch-off state.)

### Constitution checks

- **Documentation sync** — pass: the fix is display wording; the accompanying comment (147–149) documents the resolution chain. No README/PLAN/module-doc change implied by the delta. The plan's "no silent state" claim carries the switch-off caveat above.
- **Multi-platform neutrality** — pass: pure TSX logic; no platform API, path, or shell syntax.
- **File-tools-first** — pass: no shell-based file mutation in the delta.
- **Warning-free build** — pass by construction per dispatch verification state (tsc clean, vitest 136 green; frontend-only delta, Rust untouched since 60878df); no `#[allow(...)]`.
- **Bookkeeping accuracy** — pass: the plan file's `## Reviews` stamps (1 ed8fb1d, 2 60878df, 3 71c8d9e uncommitted stamp pending this round) match the actual reviewed commits; the committed round-2 report matches the findings I re-verified against the code.

Reviewed-state: 71c8d9e2e5ce9e8423d6f3a1090815ccfc09583d

Reviewed-state: 71c8d9e2e5ce9e8423d6f3a1090815ccfc09583d
