## Verdict: FINDINGS (0 high, 1 low)

Round-2 delta review of plan b3188918 on `wt/mnemo`: commit 60878df against its parent ed8fb1d0 (the round-1 reviewed state). Scope = the delta only. Read: `frontend/src/components/settings/types.ts` (`capsAutofillPatch`, the new `effortAutofillEligible`, `effortSurvivesAllowList`, `REASONING_EFFORTS`), `types.test.ts` (new effort/eligibility tests), `EndpointCard.tsx` (autofill effect + `guardKey`, `refEffortsFillable`, all four note/hint render branches), `src/provider/models.rs` (`EFFORT_LADDER`, `model_effort_levels`, dedupe merge, `effort_ladder_index`, `xhigh_only_effort_support_is_not_discovered`), `src/provider/anthropic.rs` (wire table + `effort_maps_onto_output_config_effort_levels`), and the LOW-3 doc hunks in `client_factory.rs` (120–125, 244–245) and `endpoints.rs` (476–482); round-1 report re-read for each finding. Verification state (tests/tsc/vitest) taken per dispatch — not re-run.

Summary: LOW 2 and LOW 3 are genuinely and completely fixed; LOW 1's fix is correct for every configured-effort state but leaves one reachable declined state (nothing configured → app default "max") with neither note nor hint — a residual of the exact class the fix claimed to remove.

### LOW 2 — VERIFIED FIXED (xhigh not selectable-but-discoverable)

- `EFFORT_LADDER` (models.rs:246) = `["minimal","low","medium","high","max"]` — the invariant "UI ladder minus off" holds exactly against `REASONING_EFFORTS` (types.ts:188–195: max,high,medium,low,minimal,off — same membership, and `minimal` is probed as documented).
- No remaining path surfaces xhigh into a discovered allow-list: `model_effort_levels` filters over `EFFORT_LADDER` only (models.rs:260–272), the dedupe merge (202–209) only merges lists that came from it, and `effort_ladder_index`'s out-of-ladder arm (278–283) is defensive dead code for discovery, correctly commented as such. The wire table pass-through (anthropic.rs:442) remains, documented as hand-edited-config support — intended.
- The regression test `xhigh_only_effort_support_is_not_discovered` (models.rs:837–852) is genuinely red on the pre-fix code (the old ladder probed `xhigh`, so the xhigh-only entry would yield `Some(["xhigh"])`) and pins the fix.

### LOW 3 — VERIFIED FIXED (doc precision)

- `client_factory.rs:120–125` now carries the full `reasoning_effort_off_wire` caveat, including that the client's effort table drops the wire value so `off` still sends nothing — matches the code path verified in round 1 probe (a).
- Both display-vocabulary lists (`client_factory.rs:244–245`, `endpoints.rs:479–482`) now read `off | minimal | low | medium | high | max | xhigh`, matching what `display_normalize_reasoning_effort_for` actually passes through (frontend ladder includes `minimal`; configured values pass unclamped).

### LOW 1 — FIX MOSTLY CORRECT; one reachable declined state still silent

The shared-helper architecture is right and the note/patch can no longer disagree where it's used:

- `effortAutofillEligible` (types.ts:361–375) is called by both `capsAutofillPatch` (347) and the card (`refEffortsFillable`, EndpointCard.tsx:145–146) with identical inputs (`endpoint`, `capsRefModel`, `refEfforts` = `refCaps.efforts`) computed from the same render's state — no stale closure: `refEffectiveEffort` is derived purely from `endpoint`, and `endpoint` is an effect dep (155–172), so the two can't skew.
- Empty list is un-writable on every path: the patch writes `reasoning_efforts` only through the eligibility helper (non-empty required), and the Apply buttons write `refEfforts` gated by `refEfforts.length > 0` (612–613, 640–643).
- Note and hint are mutually exclusive by construction: the note requires `refEffortsFillable` (658), the hint's empty-list branch requires `!refEffortsFillable` (618) — never both.
- The extra `guardKey` component (`refEffectiveEffort`, 160–164) is sound: no render/effect loop (the guard is set before `onEndpointChange`, and a null patch mutates nothing), and manual cap clears keep their cleared state (a clear doesn't change the key). The one re-arm cost — a manually-cleared effort list refills if the user then changes the effective effort — is the documented, deliberate tradeoff in the effect's comment (157–159) and the commit message.
- The new `effortAutofillEligible` test block (types.test.ts:310–336) pins the mirror contract (eligible/declined/empty/null/non-empty-list/off).

**The residual (EndpointCard.tsx:617–620):** the declined-with-empty-list hint branch requires `refEffectiveEffort != null`. When neither the per-model nor the endpoint `reasoning_effort` is configured — the default state of a freshly added Anthropic endpoint (`REASONING_EFFORT_DEFAULT` sentinel → `null`) — the effective effort at request time is the app default `"max"` (`effortSurvivesAllowList`'s `?? "max"`, mirroring the backend, types.ts:386–390). A model whose discovered list omits `"max"` (e.g. `[high, medium, low]`) is then declined by the autofill, the fillable note is hidden (`refEffortsFillable` false), AND the hint is hidden (`refEffectiveEffort == null`) — silence, exactly the state the commit message says was removed ("the declined state gets its own actionable hint instead of silence"). Failure is in the safe direction (no write, no downgrade), and it resolves the moment the user picks any explicit effort. Fix is one expression: gate on `(refEffectiveEffort ?? "max")` and display the resolved default in the hint text. This also makes the commit-message claim slightly overstate the shipped behavior.

One-line out-of-scope remark (pre-existing, unchanged by this delta): after the user manually clears a per-model ctx/out field, the "auto-fill with these values" note (651–657) reappears while the once-per guard correctly blocks the refill — the same note-vs-guard mismatch class, but in the round-1-accepted ctx/out semantics; noting for a future pass, not a finding of this delta.

### Constitution checks

- **Documentation sync** — pass: the LOW-3 wording fixes landed; PLAN.md and the doc comments match the shipped code. The commit-message "hint instead of silence" claim is slightly overbroad per the finding above.
- **Multi-platform neutrality** — pass: pure TS/Rust logic in every fix hunk; no platform API, path, or shell syntax.
- **File-tools-first** — pass: no shell-based file mutation anywhere in the delta.
- **Warning-free build** — pass by construction: `#![deny(warnings)]` at both crate roots + green `cargo test` (2881/19/1) per the dispatch verification state; no `#[allow(...)]` added in the fix hunks.
- **Bookkeeping accuracy** — pass: plan file, knowledge records, and the round-1 report all match what shipped in 60878df; the one overstatement is the commit-message/hint claim noted above.

Reviewed-state: 60878dffd4ce4be8a53a1d2a5459fe44d22e8dd6
