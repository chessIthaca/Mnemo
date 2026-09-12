# Review 2026-08-20 — Resize-handle highlight line + inset scrollbar thumb

**Scope:** all uncommitted changes (`git status --short` / `git diff HEAD`), excluding `.coding/**`
(app bookkeeping per convention — the `backlog.json` hunk is the plan-loop-gate crash-recovery note,
`plans/stack.json` + the untracked plan md are session state, all unrelated to this task).

**Change set reviewed:**
- `frontend/src/App.tsx` (modified) — ResizeHandle grip pill + hover wash restored
- `frontend/src/styles/globals.css` (modified) — scrollbar thumb 1px inset
- `frontend/src/styles/scrollbar.test.ts` (new) — CSS-contract regression test
- `frontend/src/node-shims.d.ts` (new) — ambient `node:fs` declaration
- `frontend/vitest.config.ts` (modified) — test registration

## Verdict

**No correctness, bug, or security findings.** The change does exactly what the plan
described; the one deviation (fs-read instead of `?raw` import) is justified, verified
empirically by the author, and correctly memorialized. Details below, then
non-blocking informational notes.

## 1. App.tsx — ResizeHandle (verified clean)

Diff (and full read of the component, :580–681) confirms the edit touches **only**:
the doc comment (:589–592), the outer div className (:671), the hit-area comment
(:673–674), and the two added comment/pill lines (:676–678).

- **Drag logic untouched:** `onPointerDown` clamp math (`[300, innerWidth*0.8]`),
  `setPointerCapture`/`releasePointerCapture` with the try/catch, `pointermove`/
  `pointerup`/`pointercancel` window listeners, `cleanupRef` teardown and the
  unmount `useEffect` (:605–662) are byte-identical to HEAD.
- **role="separator", aria-orientation, aria-label, title untouched** (:667–670).
- **No leftover `group` class** — correct, since nothing inside this handle uses a
  `group-hover:` variant (the pill is static `bg-border`). FileViewer keeps `group`
  at :564 but that's its own pre-existing markup.
- **Motif match:** outer `flex … items-center justify-center transition-colors
  hover:bg-cyan-500/20` + centered `h-8 w-0.5 rounded-full bg-border` pill is the
  vertical variant of InflightBar.tsx:129–133 (`h-1.5` bar, `hover:bg-cyan-500/20`,
  `h-0.5 w-8` pill) and FileViewer.tsx (components/views/FileViewer.tsx:558–567,
  same plus `transition-colors`). The `pointer-events-none` on the pill is the
  pre-removal class string and guarantees the drag/hit-area targeting is unaffected.
- **Comments accurate:** the rewritten doc comment and both inline comments match
  what the JSX now does. The hover wash covers only the `w-1` column (not the ±4px
  hit area), same as the reference handles — consistent, not a defect.

## 2. globals.css — scrollbar thumb inset (verified correct)

- **Technique is the canonical one:** a 1px transparent border shrinks the paintable
  box, and `background-clip: padding-box` keeps the background out of the border
  ring, so the track shows through as a 1px gap on all four sides. With
  `border-radius: 4px` the inner edge rounds to 3px — intended, cosmetic.
- **The `background`-shorthand claim is spec-true:** per CSS Backgrounds, the
  `background` shorthand resets every background longhand it doesn't set — including
  `background-clip` (initial `border-box`). Both thumb rules now use the
  `background-color` longhand, which does not touch `background-clip`, so the base
  rule's `padding-box` clip survives into `:hover` (the base rule keeps applying on
  hover; the hover rule only overrides the color). Gap persists on hover — matches
  the stated intent, and the comment (:99–103) documents the trap for future editors.
- **No conflicting rules:** a frontend-wide search for `-webkit-scrollbar-thumb`
  finds exactly the two rules in globals.css (:104, :110); no component injects
  scrollbar styles via arbitrary Tailwind variants (`webkit-scrollbar` across all
  .tsx: zero matches).

## 3. scrollbar.test.ts — genuine regression test (verified)

- **Fails on every revert mode:** base `background` shorthand → test 2 fails on the
  `(^|;)\s*background\s*:` match; hover shorthand → test 2 fails on the hover body;
  dropped border or dropped `background-clip` → test 1 fails. Author verified both
  tests fail with genuine assertion errors against the pre-fix CSS.
- **Regexes correct:** `(^|;)\s*background\s*:` cannot false-match `background-color:`/
  `background-clip:` — after `background` the pattern requires only whitespace then
  `:`, and `-` breaks it (checked against the leading-space normalized body and
  post-`;` positions). It does catch `background:` and `background :` spellings.
  `thumbRuleBody`'s `/::-webkit-scrollbar-thumb(:hover)?\s*\{([^}]*)\}/g` backtracks
  correctly so the base query doesn't match the `:hover` rule (and would not match a
  hypothetical `:active`/`:vertical` variant either). `normalized()` strips `/* … */`
  comments, so prose mentioning "background" in a rule body can't false-fail the test.
- **Deviation (fs read) is right:** vitest 4 with default `css: false` stubs all
  `.css` imports (`?raw`/`?inline` included) to `""` — the `?raw` approach could
  never see the rules. `readFileSync(new URL("./globals.css", import.meta.url))`
  resolves correctly under vitest's Node ESM. Registered in vitest.config.ts
  include (:35) — required in this enumerated-include setup.

## 4. node-shims.d.ts (verified, one forward-looking note)

- Correct today: `@types/node` is **not** in devDependencies (package.json checked),
  tsconfig has no `types` restriction and `include: ["src"]`, so the ambient
  `declare module "node:fs"` is the only resolution for the single `node:fs` import
  (the sole `node:` usage in src is scrollbar.test.ts:18). `URL` in the signature
  binds to the DOM-lib `URL` the test constructs; runtime is Node's URL — fine.
  An inline `declare module` inside a module file would indeed be TS2664; the
  separate non-module .d.ts is the right shape.

## Informational notes (non-blocking, no action required in this change)

1. **Line endings — could not byte-verify; low residual risk.** Git warns that both
   App.tsx and globals.css working copies are CRLF (pre-existing recurring pattern,
   already flagged in the 2026-08-20 review; `.gitattributes` LF pin stays canonical).
   The read-only search tool strips CR before matching (proven by a control: the
   known-CRLF App.tsx returns zero `\r` hits), so mixed CRLF/LF within globals.css
   cannot be confirmed or excluded from here. Any mix self-heals at commit time —
   git normalizes to LF per `.gitattributes` — so this is cosmetic, same class as
   the prior review's minor finding.
2. **Test brittleness (minor, acceptable for a contract test):** test 1's
   `toContain("border: 1px solid transparent")` / `("background-clip: padding-box")`
   are whitespace-sensitive — a valid reformat like `border:1px solid transparent`
   (no space after the colon) would false-fail. `normalized()` could additionally
   collapse `:\s*` → `:` if this ever bites. Likewise `thumbRuleBody` returns the
   *first* rule per variant, so a second, reverting duplicate rule later in the file
   would evade the contract — fine as long as the canonical rule stays the only one
   (it is; see §2's search).
3. **node-shims.d.ts forward note:** if `@types/node` is ever added, this ambient
   declaration merges into the real module as a redundant overload — not a build
   breaker, but the shim should then be deleted. The file's doc comment already
   explains why it exists.
4. **Drive-by, pre-existing, OUT of diff scope:** `components/views/FileViewer.tsx:564`
   carries a `group` class whose only child (the pill at :566) uses no `group-hover:`
   variant — likely dead since that handle's own restyle. Not introduced by this
   change; noting only so it isn't lost.

## Constitution compliance

- No `#[allow(...)]`, no new warnings (author: `cargo test` green under
  `#![deny(warnings)]`; `npm run build` = tsc + vite green).
- Regression tests required per defect: the scrollbar fix ships a failing-then-passing
  CSS-contract test; the JSX class restoration has no harness (consistent with the
  established precedent for handle restyles).
- `.coding/**` excluded from scope as instructed.
