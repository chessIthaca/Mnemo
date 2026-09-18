## Verdict: FINDINGS (0 high, 2 low)

Review of plan 90c15456 "Shell output preview: fixed height + on/off toggle" (backlog 6f25fb7e) — the full uncommitted changeset on `wt/mnemo`: Message.tsx, appearance.ts(+test), useAgentStore.ts, settings/types.ts, ChatSection.tsx(+test), toolCardPaths.test.ts, vitest.config.ts, docs/FEATURES.md, the two new test files (Message.preview.test.tsx / Message.preview.off.test.tsx), and .coding bookkeeping.

The core fix is correct and complete: the preview block is gated on `showShellPreview && name === "shell" && running`, rendered at a fixed height reserved from the first paint, with overflow-auto + the retained stick-to-bottom effect; the toggle is fully wired (localStorage key + reader default-on, store state/setter/init, ChatDraft + serializeChat, ChatSection draft/apply/checkbox) and persists across restarts; tests genuinely pin the reservation, the streamed tail, the non-shell gate, the finished gate, and the toggle-off path (including no text leak); both new test files are registered in vitest.config.ts's include allowlist. All backlog acceptance criteria hold. Two LOW findings below — one math/doc inaccuracy, one comment imprecision.

## Verified correct

- **Reservation semantics** (Message.tsx:688-696): the block renders for every running shell call from the first paint, empty until the first chunk — exactly the user-specified backlog design ("reserved from the first paint of the running card"). The empty-box trade-off for quick/silent commands is per-spec; the comment ("whether empty or full") is accurate on this point. During streaming the card's only variable content is `liveTail`, which sits inside the fixed-height box — the card height cannot change. The one-time reflow when the result lands (block unmounts) is the pre-existing result-replaces-tail behavior, out of the criterion's scope.
- **Toggle wiring completeness**: repo-wide search finds every reference consistent — appearance.ts `LS_SHOW_SHELL_PREVIEW` + `readShowShellPreview()` (default true, mirroring `readShowTokenUsage` exactly); useAgentStore state field + doc comment, setter decl, init (`readShowShellPreview()`), impl (`writeLs` + `set`), import + re-export entries; types.ts `ChatDraft.showShellPreview` + doc; ChatSection draft capture (:43), apply (:96, with the deviation comment), checkbox (:183-191); Message.tsx gate (:448/:688). No missed reference: correctly absent from `resetAppearance` (the Chat-section siblings showToolImages/showToolActivity/showKnowledgeActivity aren't there either) and from config.toml hydration (localStorage-only — nothing to hydrate).
- **localStorage-only deviation from the showTokenUsage sibling**: matches the backlog's own wording ("persisted like the other appearance options in frontend/src/hooks/appearance.ts" — appearance.ts prefs are localStorage-backed). The deviation is documented at both ChatSection.tsx:93-95 and types.ts:385-389 with the backlog citation — sufficient. Consequence (accepted, per scope): the pref doesn't sync across machines via config.toml.
- **serializeChat/dirty detection**: `serializeChat` is `JSON.stringify(d)` so the new field flows automatically; the ChatSection.test.ts base-literal + `not.toBe(serializeChat({...base, showShellPreview: false}))` case proves it (would fail if the field were dropped).
- **Test quality**: Message.preview.test.tsx (a) running shell call with NO liveOutput → block present with `h-[7.75em]` + `aria-live="polite"` — genuinely pins reservation-before-first-chunk, the exact old-`liveTail !== ""` regression; (b) streamed tail inside the fixed block; (c) running NON-shell call with a stray liveOutput → no block (pins the name gate); (d) finished call → no block. Message.preview.off.test.tsx seeds `mh.showShellPreview="false"` via vi.hoisted window stub BEFORE the store module initializes (the RightPanel.width.test.tsx pattern — correct, the store reads at module-eval time) → no block, no class, no text leak. toolCardPaths.test.ts contract updated (toggle gate string, `h-[7.75em]`, `not.toContain("max-h-[12em]")`, kept the calls.find/el.scrollTop/liveTailPreview/aria-live pins). appearance.test.ts reader cases (default true / explicit false) with stubbed window. Both new files registered in the include allowlist (guarded by vitestInclude.test.ts).
- **neverGroups claim (the load-bearing half)**: only `shell` emits `ToolOutputDelta` (backend sink in src/tool/agent/shell.rs) and shell never merges (agentEventReducer.ts:557) — so at most one call per card carries a tail; the `find`-based render is total. Verified.
- **Docs sync**: FEATURES.md:25 updated (fixed height + toggle + persistence); ChatSection doc comment (:19-26) lists the new toggle; Message.tsx block comment and the stick-to-bottom comment (:562-563) updated. README/PLAN.md need no change (no config.toml key introduced).
- **Multi-platform neutrality**: pure frontend TS/TSX; window/localStorage access is typeof-guarded + try/catch like the siblings; no platform APIs. ✓
- **File-tools-first**: no shell-based file mutation anywhere in the changeset. ✓
- **Security**: display-only gate on an existing block; no new IPC/surface, localStorage reads guarded. No issues.

## LOW 1 — the `h-[7.75em]` fixed-height math is wrong: the box fits 4.5 lines, not the claimed 6 (comment + FEATURES.md inaccurate)

Message.tsx:681-683 claims the block occupies "h-[7.75em] — six lines of 0.75em text at 1.5 line-height plus padding, fitting the liveTailPreview window", and FEATURES.md:25 says "occupies a FIXED height (six lines)". The arithmetic conflates parent-em with element-em:

- The `<pre>` carries `text-[0.75em]` → its font-size is 0.75 × the parent's font-size (em in font-size is parent-relative).
- But `h-[7.75em]` and `p-[0.5em]` resolve against the ELEMENT's OWN font-size (CSS: em in any property other than font-size refers to the element the property is on) = 0.75F. So: height = 7.75 × 0.75F = 5.8125F; padding = 2 × 0.5 × 0.75F = 0.75F; content = 5.0625F.
- Line-height: the pre sets no `leading-*` and no ancestor in the chat column does; no `line-height` rule exists in any project CSS — so the pre inherits Tailwind preflight's `html { line-height: 1.5 }` → 1.5 × 0.75F = 1.125F per line.
- Visible lines = 5.0625 / 1.125 = **4.5 lines** — not 6. The comment's sum (6 × 0.75em × 1.5 + 2 × 0.5em = 7.75em) is computed in PARENT em units; the correct element-unit class for six lines is `h-[10em]` (6 × 1.5em + 2 × 0.5em). The old `max-h-[12em]` fit ~7.3 lines, so the 6-line window fit fully; the new box clips the top ~1.5 lines of the window (scrollable — stick-to-bottom keeps the newest visible).

Impact: the primary no-jump goal is unaffected (height is fixed regardless of content) and no backlog acceptance criterion is violated — but the visible tail shrank from 6 to ~4.5 lines vs the design intent, and both the code comment and FEATURES.md state a falsehood. Fix: either `h-[10em]` (keeping the comment's six-line claim true), or correct the comment + FEATURES.md to the delivered line count. Note the tests pin the class string, so they stay green either way — the comment is the only guard on the arithmetic.

## LOW 2 — comment misattributes streaming to the merge policy (Message.tsx:684-685)

"Only `shell` streams (the reducer's neverGroups)" — `neverGroups` (agentEventReducer.ts:557) is the MERGE policy (shell/search/search_read/browser never merge into grouped cards); the reason only shell carries a tail is that only shell EMITS `ToolOutputDelta` (the shell.rs sink) AND shell never merges. The pre-existing comment at :551-556 states the relationship correctly ("only `shell` streams, and shell never merges — the reducer's neverGroups"); the new compressed parenthetical cites neverGroups as the source of streaming. The `name === "shell"` gate is what actually makes the reservation shell-only. Doc-accuracy nit only — no behavior implication.

## Acceptance-criteria matrix (backlog 6f25fb7e)

| Criterion | Result |
|---|---|
| While a shell command streams, the running card's total height never changes after the preview appears | ✅ fixed height reserved from first paint; only variable content sits inside the box |
| With the toggle off no preview block renders | ✅ gate; pinned by Message.preview.off.test.tsx incl. no text leak |
| The toggle persists across restarts | ✅ writeLs on set + readShowShellPreview at store init; reader unit-tested |
| Frontend tests cover the fixed-height render and the toggle-off path | ✅ both new files registered; reservation, tail, gates, off-path all pinned |

Stated verification (full vitest green, tsc + vite build clean, cargo test 2410 passed / 0 failed / 5 ignored + 16 integration) accepted as evidence; no re-run was possible from this read-only session.
