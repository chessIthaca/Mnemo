## Verdict: PASS

Round-2 verification of the backlog item "Land the spawn-cancellation + event-delivery delta (quality fixes Q1-Q4)" — commits e66e4b6 + db37b99 on top of the pre-item checkpoint a8546f2 (wt/agenticcoding, HEAD db37b99). All four round-2 findings are verified RESOLVED in the code; the fixes introduced nothing new; the core Q1-Q4 changes are intact. One pre-existing stale doc adjacent to (but outside) the delta is noted as an observation, not counted.

## Scope and method

- Reviewed `git diff a8546f2..HEAD` in full — both commits, every hunk: src/runtime/channels.rs, src-tauri/src/ipc/events.rs, src/agent/factory.rs, frontend/src/hooks/useAgentEvents.ts, frontend/src/hooks/useAgentEvents.registration.test.ts (new), frontend/vitest.config.ts, plus the .coding bookkeeping (backlog.jsonl, plan 660d60af, the landing-review report itself).
- Uncommitted tree: only .coding bookkeeping (the backlog item's queue-stamp note + the plan's step-10 completion mark) — no code, exactly as scoped.
- The committed landing-review report at HEAD is the second reviewer's 4-finding version (opens "## Verdict: FINDINGS (0 high, 4 low)"; e66e4b6 carried the first reviewer's 2-finding version as its git-history predecessor — verified via both commits' diffs).
- Cross-verified against the live code: the load_tools registration/gating chain (factory.rs `build_registry` → `ToolRegistry::schemas` → `hidden_groups_for`), the ToolFilter arms, the latch/filter code, the test files, the vitest include list, and the git history of the deferral landing (commit 5d38585).
- Test-run claims (cargo test --workspace green/warning-free, 76 vitest files / 1066 tests) are the implementing agent's, trusted per the task; corroborated where cheap: the commit messages record 1065 → 1066 frontend tests (matching the one `it` block db37b99 added), every recorded ceiling figure is arithmetically consistent, and the registration test is in the include allowlist (vitest.config.ts:46).

## Finding-by-finding resolution

### Round-2 LOW-1 + LOW-2 (factory.rs ceiling comments: name the real +431 carrier; reconcile Reviewing) — RESOLVED

**Where the fix landed:** src/agent/factory.rs:1652-1662 (Planning), 1685-1695 (Executing), 1703-1708 (ExecutingResearch), 1718-1728 (Reviewing), 1729-1734 (Complete) — commit db37b99.

**Verified against the required content:**

1. *Names the verified +431 carrier with the mechanism.* All five comments attribute the +431 to `load_tools` and state the deferral-loader-only-exists-when-a-deferred-group-does mechanism — Planning: "which registers `load_tools` in every filter — the deferral loader only exists when a deferred group (the browser family) does" (factory.rs:1654-1657); Executing repeats it (:1688-1690); ExecutingResearch (:1704) and Complete (:1730) reference "load_tools, +431 ... as Executing above"; Reviewing: "the +431 is load_tools, as above" (:1720).
2. *Records the standalone baselines.* Planning 14_810 (:1658), Executing 24_957 (:1691), ExecutingResearch 21_543 (:1705-1706), Reviewing 21_425 (:1720), Complete 14_810 (:1731) — all five present, matching the measured figures.
3. *Reviewing's +520 story.* factory.rs:1721-1726: "Growth since the last recorded workspace baseline (21_336, 2026-12-22 — itself a unified measurement that already included load_tools) is +520 of schema growth: the 2026-12-23 update_plan Reviewing-surface expansion plus the 2027-01-07 create_plan/steps-persistence changes riding update_plan into Reviewing." Both named causes are real commits in the history below HEAD (0b72402 "update_plan fully callable in Reviewing"; 41384d6 "bug_fixing create_plan persists caller steps"), and update_plan is indeed in Reviewing's Workflow allow-list (mod.rs:445) while create_plan is not — so the create_plan growth reaching Reviewing via update_plan's steps surface is the correct routing.

**Arithmetic — all internally consistent:**
- Planning 14_810 + 431 = 15_241 ≤ 15_300 (headroom 59)
- Executing 24_957 + 431 = 25_388 ≤ 25_450 (62)
- ExecutingResearch 21_543 + 431 = 21_974 ≤ 22_050 (76)
- Reviewing 21_425 + 431 = 21_856 ≤ 21_950 (94); and 21_336 + 520 = 21_856 — the round-2 contradiction (21_336 + 431 = 21_767 ≠ 21_856) is resolved exactly by the "baseline already included load_tools" clause.
- Complete 14_810 + 431 = 15_241 ≤ 15_300 (59)

The uniform +431 across all five filters is what a single filter-independent schema produces — load_tools's schema is generated from the install-wide group table (load_tools.rs:97), not per-filter.

**Registration logic — the comments' mechanism is the code's actual mechanism:**
- factory.rs:849-850: browser tools register only under `#[cfg(feature = "browser")]` (register_browser_tools, :1066-1104; the offscreen family is "always present when the `browser` feature is selected", :1059-1060 — no CDP opt-in needed, so `available()` holds with inspection off, as the budget test's own browser block exercises).
- factory.rs:858-864: `LoadToolsTool` is registered unconditionally — but src/tool/mod.rs:845-846 skips it from the `tools` array when `hidden_groups_for(filter)` is empty ("`load_tools` only earns its ~700 chars when there is a group it could usefully reveal").
- mod.rs:749-774: `hidden_groups_for` counts only groups with ≥1 registered + `available()` + filter-allowed tool. The image group never qualifies in the budget test (no vision model — factory.rs:1594 asserts `!factory.vision_slot().is_configured()`; unavailable tools are excluded at mod.rs:817-818 and hidden_groups_for requires `t.available()` at :768). No MCP servers in tests → no `mcp.*` groups.
- Therefore: standalone (`cargo test -p mnemo --lib`, no browser feature) → no group qualifies in any filter → no load_tools anywhere ("no load_tools", factory.rs:1658-1659 — and 14_810 sits under the old 15_000 ceiling, as the comment notes). Workspace (browser on) → the browser group qualifies in every filter that allows `ToolCategory::Browser` — verified true in ExecutingResearch (mod.rs:405), Reviewing (mod.rs:421), Complete (mod.rs:481), and per the measured uniform +431 also Planning/Executing → load_tools joins every filter. Exactly the comments' claim.
- Historical sanity for "21_336 already included load_tools": the deferral machinery landed in commit 5d38585 ("Cut per-request context ~45%: gate, defer, and consolidate tools" — the load_tools.rs creation commit), which predates every ceiling-raise commit in the chain (it sits below e039f56, the first 2026-09-08 restore raise, in git-log order; the 12-04/12-08/12-22 raises are all younger). A 12-22 workspace run therefore had browser deferred + load_tools advertised. Corroborated by the older comments' own arithmetic: had the browser family been in the default array at 12-08, that measurement would have been thousands of chars higher than the recorded ~350-over figure — the recorded raises are only consistent with browser-deferred + load_tools-in. The standalone-baseline identification (24_957 = the 2027-01-07 recorded figure, "taken without unification") rests on the exact equality of today's measured standalone with that figure — the implementing agent's live per-filter tool-list diff, trusted per the task; it is also the only parsimonious reading (the alternative would require schema growth since 01-07 to equal exactly +431 in both Executing and ExecutingResearch simultaneously).

**Resolves the finding:** yes — LOW-1's "name the real +431 carrier" and LOW-2's "record the actual arithmetic" are both satisfied, with the carrier verified against the code's gating logic rather than asserted.

### Round-2 LOW-3 (Q1 doc scope: first-latch-transition guarantee) — RESOLVED

**Where:** src/runtime/channels.rs:1049-1062 (set_running doc) and src-tauri/src/ipc/events.rs:1281-1290 (cleanup filter comment) — commit db37b99.

channels.rs:1050-1053 now scopes the guarantee: "On the FIRST latch transition (false → true — a freshly spawned agent), a cleanup pass running between the two stores must see either 'pending' (latch false — spared) or 'running' (spared) — never 'started + idle'", and :1057-1062 documents the residual: "On RE-starts (turn 2+, latch already true) the Acquire load may read the previous turn's latch store, leaving a nanosecond-scale 'started + idle' residual — equivalent to the cleanup winning the race during the agent's genuine idle gap between turns, which is the designed semantics (idle started subagents are cancellable)."

events.rs:1284-1290 mirrors it: "on the first latch transition (a freshly spawned agent) the two can never present 'started + idle' mid-`set_running(true)` ... On re-starts (turn 2+) a nanosecond-scale residual remains, equivalent to the cleanup winning the race during the agent's designed idle gap between turns."

Both places scope the never-"started + idle" guarantee to the first latch transition and document the re-start (turn 2+) residual as equivalent to the designed idle-gap semantics — exactly LOW-3's fix direction, and an accurate statement of the memory-model situation the round-2 reviewer derived. **Resolved.**

### Round-2 LOW-4 (Q3 test: pin the chain shape) — RESOLVED

**Where:** frontend/src/hooks/useAgentEvents.registration.test.ts:61-64 — commit db37b99.

The new block:

```ts
it("the registration call is chained, not void-discarded", () => {
  expect(fn).not.toContain("void onAgentEvent(");
  expect(fn).toMatch(/onAgentEvent\([\s\S]*?\)\s*\.then\(/);
});
```

Both of LOW-4's suggested assertions were applied (the regex AND the void-discard ban). Verified the regex matches the actual source shape (useAgentEvents.ts:257-260): `onAgentEvent((payload) => { ... })` followed by newline + `.then(` — the non-greedy `[\s\S]*?` lands on the `)` closing the call (the earlier `)`s, after `(payload)` and after `deliverAgentEvent(payload)`, are followed by ` =>` / `;`, not `.then(`). Reverting to a void-discarded registration now fails both assertions; a refactor that re-introduces `void onAgentEvent(` while leaving an unrelated `.then(`/`.catch(` elsewhere in the function body fails the `not.toContain` — the exact contrived hole LOW-4 flagged. **Resolved.**

### Round-1 LOW-1 (superseded "create_plan growth + backlog docs" attribution) — correctly superseded

The round-1 Reviewing comment attributed the residual ~89 chars to "the 2027-01-07 create_plan/steps-schema growth riding update_plan into Reviewing" while attributing the +431 to an unidentified "feature-gated tool". The landed comment replaces this with the measured story: +431 = load_tools (verified carrier), and the +520 since the 12-22 unified baseline = all schema growth with both causes named (2026-12-23 update_plan expansion + 2027-01-07 create_plan/steps-persistence). The supersession is coherent — the schema-growth causes are retained and correctly separated from the unification delta.

## Core changes spot-check (prior-round verified) — intact

- **Q1:** channels.rs:1063-1070 — `running` stored first (Relaxed), then the latch (Release) on `true`; `has_ever_started()` loads Acquire (channels.rs:1041-1044); the cleanup filter evaluates the latch before `!h.is_running()` (events.rs:1291-1292). Intact.
- **Q2:** events.rs:1070-1086 — `DIAG_SINK: OnceLock<PathBuf>`; a `.coding` side-car under CWD → `.coding/logs/emit-diag.log`, else `std::env::temp_dir().join("mnemo-emit-diag.log")`; `diag_line` writes via `diag_sink()` (events.rs:1056) with the rationale documented (events.rs:1036-1044). Intact.
- **Q3:** the source-contract test exists (65 lines) and is in the vitest include allowlist (frontend/vitest.config.ts:46). Intact.
- **Q4:** the whitespace run is collapsed (events.rs:1515: "it is the agent the previous spawn_agent call just registered"); `AGENT_EVENT_CHANNEL` at useAgentEvents.ts:146, tauri.ts:32, and the Rust emit side events.rs:55; zero `AGENT_EVENT_LABEL` references remain in source (only historical .coding records — correct). The comment (useAgentEvents.ts:141-145) names both sides correctly ("tauri.ts (listen wrapper) and src-tauri/src/ipc/events.rs (emit side)") — which also resolves the round-1 LOW-2 wording nit. Intact.

## Did the fixes break anything new? No.

- db37b99 is three comment blocks (factory.rs, channels.rs, events.rs), one 5-line test addition, and the review-report update. The comments cannot change behavior; the test addition was verified against the actual source shape (it passes, and the 1065 → 1066 frontend-test count in the commit messages matches the one added `it` block).
- e66e4b6 was accounted hunk-by-hunk against the prior rounds' verification; the Q1 filter reorder (running-first → latch-first evaluation), the Q2 sink, the Q3 test, and the Q4 cosmetics all match what the prior reviewers approved.
- Commit completeness: the round-2 report's must-include note is satisfied — the previously-untracked registration test and the plan file are both in e66e4b6.
- Ceiling headroom is 59-94 chars per filter ("measured + headroom" as documented); no figure is tight enough to make the green matrix implausible.
- Bookkeeping: backlog b18ef70d is in_flight with the plan linkage; f20bff16 (UI event blackout) is done with a verification note matching the evidence. The uncommitted .coding delta is queue-state only, as scoped.

## Observation (not counted — pre-existing, outside this delta)

src/tool/mod.rs:744-746 (the `hidden_groups_for` doc) and mod.rs:841-844 (the load_tools skip comment) both say the browser family is "blocked by category during Reviewing" — contradicted by the actual filter arm (mod.rs:414-421: `ToolCategory::Browser => true` in Reviewing, with an explicit comment about browser tools being available during the closing sequence) and by the measured +431 (load_tools is advertised in Reviewing). These doc comments describe an older filter design and were not touched by this item (src/tool/mod.rs is outside a8546f2..HEAD). A future reader reconciling the load_tools ceiling story could trip on them; a one-line doc fix in a future pass would close it. Not a finding against this change set.

## Bottom line

All four round-2 findings are verifiably resolved: the five ceiling comments name the measured +431 carrier (load_tools) with a mechanism that matches the code's actual gating (mod.rs:845 + `hidden_groups_for` + the browser feature gate at factory.rs:849-850), record all five standalone baselines, and reconcile Reviewing's +520 as pure schema growth over a 2026-12-22 baseline that already included load_tools (deferral predates it — commit 5d38585); the Q1 docs scope the guarantee to the first latch transition with the re-start residual documented as designed idle-gap semantics; the Q3 test pins the chain shape with both suggested assertions; and the superseded round-1 attribution is correctly replaced. The fixes introduced nothing new — db37b99 is comments plus one verified test block. The delta stands as landed; the item can close.
