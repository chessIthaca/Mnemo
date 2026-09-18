## Verdict: PASS

Round-3 verification of plan 90c15456 ("Shell output preview: fixed height + on/off toggle", backlog 6f25fb7e) on `wt/mnemo` — the full uncommitted changeset (round-1 fix + round-2 L1/L2 fixes + the round-2 residual fix). The single residual LOW is genuinely fixed: the sibling comment in Message.preview.test.tsx now carries the corrected attribution, and an independent repo-wide sweep confirms no misattributed instance remains. No regression anywhere in the round-1/round-2-verified surface.

## The residual LOW — corrected comment: VERIFIED FIXED

Message.preview.test.tsx:61-62 now reads `// Only shell emits ToolOutputDelta (the shell.rs sink) — no reservation / for any other tool, even with a stray liveOutput.` — the round-2 suggested fix, verbatim modulo the line wrap. The parenthetical misattribution is gone: the shell.rs sink is named as the reason only shell streams, and neverGroups is no longer attached to the streaming claim. The enclosing test ("renders no preview block for a running NON-shell call", a `search` call with a stray liveOutput) is unchanged and still asserts the right negatives (`not.toContain('aria-live="polite"')`, `not.toContain("h-[10em]")`).

## Sweep — no remaining misattribution

Independent regex sweep `Only shell streams|neverGroups` over frontend/src (5 matches, 3 files):

- Message.tsx:556 — pre-existing, round-1-judged-correct: "only `shell` streams, and shell never merges — the reducer's neverGroups)" — neverGroups attached to the merge half. ✓
- Message.tsx:686-687 — the round-2-corrected instance: "Only `shell` emits ToolOutputDelta (the shell.rs sink), and shell never merges (the reducer's neverGroups), so the reservation is shell-only". ✓
- agentEventReducer.ts:557/:563 — the neverGroups definition itself. ✓
- useAgentStore.test.ts:905 — merge-attributing ("Shell calls never merge into one card (neverGroups)"). ✓

No "Only shell streams" compression survives anywhere; every remaining neverGroups mention correctly attributes it to merging. The task's sweep claim reproduces exactly.

## No regression in the round-1/round-2-verified surface

- **Gate intact**: `{showShellPreview && name === "shell" && running && (` at Message.tsx:690, selector at :446-448 — byte-identical to the toolCardPaths.test.ts contract pin.
- **Fixed height intact**: `h-[10em]` in the pre's class (Message.tsx:694), no `max-h-[12em]` anywhere in live code; the reservation comment (:680-689) still carries the exact arithmetic (6 × 1.5em line-height + 2 × 0.5em padding = 10em at the element's own em — independently recomputed: 0.75F element font-size, 7.5F height − 0.75F padding = 6.75F content ÷ 1.125F line-height = exactly 6 lines); stick-to-bottom comment (:562-563) still says "10em here".
- **Toggle wiring intact and complete**: appearance.ts `LS_SHOW_SHELL_PREVIEW` + `readShowShellPreview()` (default true, mirrors readShowTokenUsage); useAgentStore state field + `setShowShellPreview` (writeLs + set) + initial-state read + imports/re-exports; types.ts ChatDraft field with the localStorage-only doc; ChatSection draft capture/apply with the deliberate-not-in-config-payload comment + the "Stream shell output live in the tool card" checkbox; serializeChat dirty-detection case in ChatSection.test.ts; the two appearance.test.ts reader cases (default true, explicit false).
- **Pinned tests consistent with the delivered source**: Message.preview.test.tsx (h-[10em] positive :47/:56, max-h-[12em] negative :49, gated-off negatives :67/:86); Message.preview.off.test.tsx (vi.hoisted window stub seeding mh.showShellPreview="false" before store init — the established RightPanel.width.test.tsx pattern; asserts no block, not even the reserved one, and no leak of the streamed text :48); toolCardPaths.test.ts (gate string + h-[10em] positive, max-h-[12em] negative). Both new test files remain registered in vitest.config.ts's include allowlist.
- **Docs synced**: FEATURES.md:25 carries the fixed-height + toggle sentence matching the implementation (six lines, reserved from first paint, Settings → Chat label quoted verbatim, localStorage, default on). ChatSection's doc comment updated. The only other changes are .coding bookkeeping (backlog status transitions, the plan file, the round-1/2 review reports, two knowledge files from the previously-landed right-panel-width plan) — expected side-car artifacts, not part of this feature's code surface.

## Test battery

Not re-runnable from this read-only session. Accepted as stated evidence: the fix round re-ran the three targeted suites green (Message.preview.test.tsx, Message.preview.off.test.tsx, toolCardPaths.test.ts — 116/116), consistent with the plan file's round-1 record (full vitest green, tsc + vite build clean, cargo test green). Every pinned assertion was re-verified against the actual source text in this round, so the pinned tests pass on the delivered tree.

All three VERIFY items hold. The changeset is complete and consistent; nothing remains open.
