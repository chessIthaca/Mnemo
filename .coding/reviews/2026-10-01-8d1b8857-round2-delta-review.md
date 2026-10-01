## Verdict: PASS

Round-2 delta review of plan 8d1b8857 (base `3838a4e2`). Scope read: `git log` since the base → two commits; `git show d3349d7` (the fix: MarkdownLink.tsx, Message.tsx, MarkdownLink.test.tsx, linkGesture.ts/.test.ts, vitest.config.ts, `.coding/**` records); `git show --stat 7175e02` (backlog split, `.coding/backlog.jsonl` 2+/2- only); `git diff HEAD` (uncommitted remainder = the one-line round-2 review stamp in the plan file, nothing else). Both round-1 findings are fixed in `d3349d7`.

### LOW 1 — stale press record: FIXED

- `MarkdownLink.tsx:102-109`: `onAuxClick` middle branch now `e.preventDefault(); takeLinkPress(); openMarkdownTarget(trimmed, { osBrowser: true })` — drained before routing, value deliberately discarded.
- `Message.tsx:657-661`: chip's `onAuxClick` drains (`if (e.button === 1) takeLinkPress();`) — the chip has no middle-click route of its own, so drain-only is correct.
- Both surfaces gained `onKeyDown` draining on Enter/Space (`MarkdownLink.tsx:111-115`, `Message.tsx:662-665`), neither calls `preventDefault`, so native activation proceeds.

**(a) Stale-flag path enumeration — closed.** The record is armed only by the two `onMouseDown` handlers. Any mouse click that could consume it is preceded by its own `mousedown`, and `noteLinkPress` (linkGesture.ts:48) *unconditionally overwrites* — so a stale `true` can never be consumed by a later plain mouse click on either surface. Aborted press (release elsewhere), drag-away, and macOS ctrl-click context-menu all leave the record armed but unconsumed: the next press overwrites it, the next keyboard activation drains it, the middle-click auxclick drains it. Two rapid gestures: press #2 overwrites before its click reads. The only clicks that fire with no preceding press are keyboard activations — exactly the path the new `onKeyDown` drains, before the synthesized `click` reads `takeLinkPress()`. Residual: a *synthetic* `element.click()` with no press and no keydown landing after an armed aborted press would consume a stale record — unreachable by user interaction.

**(b) Legitimate behavior intact.** Genuine ctrl/cmd+click: press arms `true` → click's `takeLinkPress() || e.ctrlKey || e.metaKey` → OS browser even if the engine consumed the modifier. Middle-click on the anchor: hardcoded `osBrowser: true`; on the chip: drain-only, which is pre-existing behavior (round 1 reviewed the full change and raised no finding there; the fix removes nothing). Plain click: press records `false` → app's Browser tab. Keyboard Enter/Space: keydown drains → click reads `false` and no modifier → app tab. Ordering hazards checked: Enter fires click *after* keydown (drain precedes read — correct); Space activates the `<button>` on keyUP (drain at keyDOWN precedes the click — correct); even the theoretical interleave of a keydown between a ctrl-press and its mouseup loses only the press record, and the click's own `e.ctrlKey` fallback restores the OS-browser route. The chip's `e.stopPropagation()` remains first in its `onClick` (Message.tsx:652).

### LOW 2 — unrelated churn split out: FIXED

`7175e02` contains only `.coding/backlog.jsonl` (2 insertions, 2 deletions) with the commit message naming both run-all items; `d3349d7`'s diff carries no backlog changes — the fix commit is exactly the fix.

### (c) The contract genuinely bites

`MarkdownLink.test.tsx:319-327` asserts `takeLinkPress();` (with semicolon) in both raw sources. The only semicolon-terminated occurrences are the drain statements — the `onClick` expression is `takeLinkPress() || e.ctrlKey || e.metaKey`, which does not match. Without the fix the assertion fails in both files; it is not matching incidental text. (The paired `onKeyDown` containment is weaker for MarkdownLink — its localPath branch has a pre-existing `onKeyDown` at line 136 — but the `takeLinkPress();` assertion carries the bite.) `linkGesture.test.ts` is registered in `vitest.config.ts` (hunk verified in `d3349d7`) with six named cases covering the symptom, cmd-click, middle button, plain press, single consumption, and stale-overwrite.

### (d) No scope creep

The local-file branch (MarkdownLink.tsx:121-146) carries no gesture wiring and is untouched; `openChatLink`/`openMarkdownTarget` call sites are unchanged apart from the drains; non-http scheme handling, `browserWebviewSupported()`, and the safety levels appear nowhere in the fix hunks.

### (e) Test evidence

I cannot re-run tests on this read-only surface, so I did not independently reproduce the green run — but the tree is structurally consistent with it: both new/changed test files exist and are wired into the suite, the node-env suite needs no DOM, and the contract tests read real source via `?raw`. Nothing in the tree contradicts the reported 93 files / 1330 tests, `npm run build`, and `cargo test` all exiting 0.

### Constitution lines

- Documentation sync: `.coding/knowledge/bug/2027-01-11-…` record matches the shipped mechanism (press-time sampling, both surfaces, onAuxClick hardcode) — accurate.
- Multi-platform neutrality: pure DOM event handlers, no platform API — holds on macOS and Windows.
- File-tools-first: no shell-based mutation in the delta.
- Warning-free build: frontend-only change plus `.coding/**`; no `#[allow]` introduced.
- Bookkeeping accuracy: backlog split commit contains exactly the churn LOW 2 named; uncommitted remainder is only the harness review stamp. One process remark (exempt class): the delta re-shows the round-1 hunks (they were uncommitted at round 1 and shipped inside `d3349d7`); I verified the two fixes within them rather than re-line-reviewing the whole change, per the carry-over rule.

Reviewed-state: d3349d7778129ffcb255876a029ac225bc9019be
