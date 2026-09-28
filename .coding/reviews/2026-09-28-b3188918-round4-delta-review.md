## Verdict: PASS

Round-4 delta review of plan b3188918 on `wt/mnemo`: commit 33de125 against its parent 71c8d9e (confirmed via `git show` — exactly one commit in range, touching the four dispatch-named code/bookkeeping files). Read: the full 33de125 diff (the round-3 report now committed, the plan stamp line, and the three code hunks), then the changed code in context — `EndpointCard.tsx:128–172` (`refResolvedEffort` via the new helper, `refEffortsFillable`, the autofill effect, guard key, dep array) and `:560–665` (the caps block: all four effort note/hint arms and their gates), `types.ts:295–405` (`capsAutofillPatch`, `effortAutofillEligible`, `effectiveReasoningEffort`, `effortSurvivesAllowList`), `types.test.ts:311–380` (the mirror test and both new tests), and the backend the resolver claims to mirror — `src/config/endpoints.rs:415–499` (`normalize_reasoning_effort_for`: switch gate FIRST at 429–431, then off-wire encoding, then the allow-list clamp at 457–469; `display_reasoning_effort_for`: switch gate → "off" at 484–486, per-model → endpoint → "max" at 488–491, then the display clamp). Uncommitted remainder = one plan-file line (`4 33de125…` review stamp) — bookkeeping only. Verification state (tsc clean, vitest 92/1313, Rust untouched since 60878df) taken per dispatch, not re-run.

The round-3 LOW is closed by the reviewer's own suggested fix (option 2, resolver-in-chain), and every enumerated consequence of that choice is sound.

### Item 1 — round-3 finding closed: full switch-OFF cross-product

With the switch off, `effectiveReasoningEffort` returns `"off"` (types.ts:384) regardless of what is configured. Prereqs for any effort arm: `refCaps` present, reference model in `endpoint.models` (gate 567), `refEfforts.length > 0` (gate 612–613). Cross-product of configured-list × configured-effort (effort is irrelevant — the resolver short-circuits it):

| configured list | effort set/unset | renders | truthful? |
|---|---|---|---|
| empty | either | `refEffortsFillable` = true (`"off"` always survives, 404) → note branch (655–661) renders; empty-list clamp-hint arm (617) requires `!refEffortsFillable` → **unreachable by construction** | ✓ — `capsAutofillPatch` fills via `effortAutofillEligible` (347) which resolves through the same helper, so the promised fill really happens |
| set, differs from reported | either | amber conflict note (619–624) — factual both sides ("reports X / configured Y"), switch-independent; unaffected by the fix and not a clamp claim | ✓ |
| set, equal to reported | either | nothing (differ false; empty-list arm requires length 0) | ✓ |
| `refEfforts` null/empty, or model absent | either | no effort arm at all | ✓ |

No state with the switch off renders the clamp claim. Round-3's finding is closed.

### Item 2 — pre-clamp resolver vs backend's clamp: divergence matters for no render

The backend applies the allow-list clamp after resolution (457–469); `effectiveReasoningEffort` returns the pre-clamp value. But the resolver's value is rendered in exactly one place — the hint at 629 — and that arm is reachable only with an EMPTY configured list, where the backend's `allowed` is `None` (433–434) and no clamp occurs either: pre-clamp = post-clamp in every reachable render. With a non-empty configured list the card renders only the conflict arm (which names the two lists, never the effective effort) or nothing — the divergent state (list set, resolved ∉ list) is display-invisible. The guardKey use is memo-only. Naming the PRE-clamp value in the hint is also semantically right: that is the value the clamp acts on. One nuance, noted not a finding: the helper's doc claims "the UI can never disagree with the wire" — true for every current consumer, but a future consumer rendering the resolver beside a set allow-list would show the unclamped value; the doc's own "Mirrors … `endpoints.rs`" plus the clamp living in `effortSurvivesAllowList` makes the boundary discoverable.

### Item 3 — guard key folds in the switch: no re-arm hazard, no loop

`refResolvedEffort` is a pure function of `endpoint` + `capsRefModel`, both in the effect's dep array `[capsRefModel, refCaps, endpoint]` (172) — no stale-key window. Flipping the switch changes the key → a previously-declined fill is re-evaluated and now fills (safe while off; announced by the note) — the intended re-arm semantics, same mechanism as the round-2 effort-change re-arm. Loop check: after a fill the configured list is non-empty → `effortAutofillEligible` false → patch null → effect returns without `onEndpointChange`; and filling `reasoning_efforts` does not change `refResolvedEffort` (it reads `reasoning_effort`, not the list), so the stored key still matches on the next run. No extra renders, no setState added. One follow-on state enumerated: fill-while-off then re-enable leaves list = reported with effective effort outside it → the backend clamps at request time with the set-and-equal state rendering nothing. This is the identical end state the switch-ON path produces when the user clicks "Apply discovered" on the hint (informed there, announced by the note here), and the clamp is the backend's designed behavior (the list is the supported surface — the provider does not honor the out-of-list value). Not a defect; worth remembering if a set-and-equal + clamped-effective state ever wants its own hint.

### Item 4 — the two new tests genuinely pin the behavior

- `effectiveReasoningEffort mirrors the backend chain, switch first` (339–358): all four links — app default `"max"` (which also pins `ep()`'s switch-on default), endpoint override `"high"`, per-model `"low"` winning, and switch-off → `"off"`. Red against the pre-fix code by construction (the function did not exist; the chain was inline in `effortAutofillEligible`).
- `a disabled support switch makes an effort list safe to fill` (360–380): asserts `effortAutofillEligible(e, "m", ["low"])` is `true` with the switch off — this assertion is red pre-fix (old resolution `configured ?? "max"` = `"max"` ∉ `["low"]` → declined) — and pins the end-to-end write the note promises (`capsAutofillPatch` returns the `model_configs` patch with `reasoning_efforts: ["low"]`, the created entry's `reasoning_effort: null` matching `upsertModelConfig`'s merge of the default config). The note-branch render itself gates on exactly this `refEffortsFillable`, so the helper-level pin covers the card arm.

### Item 5 — the commit-message claims hold

"No clamp claim while effort is off": verified by construction (item 1 — the hint's empty-list arm is unreachable with the switch off). "The autofill-note branch renders instead": verified — `refEffortsFillable` true whenever the hint would previously have fired (empty list, non-empty report, switch off), and the note (655) gates on exactly that flag, same helper, same inputs as the patch. "EndpointCard drops `refEffectiveEffort`": confirmed — both the definition and its two uses (guardKey, autofill log line) were removed/replaced in the diff; the log line now emits `refResolvedEffort`, which truthfully logs `"off"` while the switch is off. The stronger phrasing round 3 flagged ("renders a truthful note, an actionable hint, or nothing that needs doing") now holds in every enumerated state.

### Constitution checks

- **Documentation sync** — pass: display-semantics fix with the resolution chain documented at the helper (types.ts:374–382) and the call site (EndpointCard.tsx:138–142); no README/PLAN/module-doc change implied.
- **Multi-platform neutrality** — pass: pure TS/TSX logic; no platform API, path, or shell syntax.
- **File-tools-first** — pass: no shell-based file mutation in the delta.
- **Warning-free build** — pass per dispatch verification state (tsc clean, vitest 92 files / 1313 green; frontend-only, Rust untouched since 60878df); no `#[allow(...)]`.
- **Bookkeeping accuracy** — pass: plan `## Reviews` stamps 1–4 (ed8fb1d, 60878df, 71c8d9e, 33de125) match the actually reviewed commits; the committed round-3 report matches what I verified against the code. Process remark (one line): the uncommitted plan-file line is the round-4 review stamp — bookkeeping only, not re-reviewed material.

Reviewed-state: 33de1253b8998f3df841240b9b6791563c2f6c40

Reviewed-state: 33de1253b8998f3df841240b9b6791563c2f6c40
