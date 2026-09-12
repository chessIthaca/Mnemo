# Review — About dialog (myharness Tauri app)

**Scope:** All uncommitted changes. Feature goal: an About dialog opened by
clicking the app logo (Code2 icon, top-left of Sidebar) that credits all direct
dependencies and lists each one's license with a clickable link to the license
text.

**Changed files (frontend only — no Rust changes):**
- `frontend/src/components/about/dependencies.ts` (NEW, untracked)
- `frontend/src/components/about/AboutDialog.tsx` (NEW, untracked)
- `frontend/src/components/layout/Sidebar.tsx` (MODIFIED)
- `.coding/backlog.json`, `.coding/plans/stack.json` (bookkeeping — out of scope)

Note: the two new `about/` files are **untracked** (`??` in `git status`), so
`git diff HEAD` does not show them; they were read directly for this review.

**Verdict:** No blocking findings. The feature is functionally correct, secure,
and constitution-compliant. A few minor/low-severity items below — none block
merge.

---

## Correctness

### Dependency data vs. manifests (verified by reading all three manifests)

- **"Rust — library"** ↔ root `Cargo.toml` `[dependencies]` + `[target.'cfg(windows)'.dependencies]`:
  all 23 crates match exactly (incl. `windows-sys`). ✓
- **"Frontend — runtime"** ↔ `package.json` `dependencies`: all 11 match. ✓
- **"Frontend — build & dev"** ↔ `package.json` `devDependencies`: all 10 match. ✓
- **License labels / URLs** spot-checked (tokio=MIT, serde="MIT OR Apache-2.0",
  clap="MIT OR Apache-2.0", lucide-react=ISC, typescript=Apache-2.0, react=MIT,
  tauri="MIT OR Apache-2.0"): all correct. opensource.org license-text URLs and
  crates.io/npmjs.com registry URLs are well-formed. ✓

### [minor] "Rust — app shell" group is incomplete vs. its stated scope
`dependencies.ts:103-112` — The group is documented (plan + top-of-file
comment) as "src-tauri/Cargo.toml direct deps", but it lists only 5 of the 10
direct deps there. It omits `tokio`, `serde`, `serde_json`, `anyhow`,
`async-trait`, `base64` — all of which ARE direct deps of `src-tauri/Cargo.toml`
but are already credited in the "Rust — library" group.

This is a deliberate de-duplication (each crate credited once), so **no crate is
left uncredited and there is no licensing gap**. The only issue is that the
group label overstates what it shows. Suggested fix (either):
- List all 10 src-tauri direct deps in the group (accept the duplication), or
- Relabel/clarify, e.g. title `"Rust — app shell (additional)"` and note in the
  doc comment that shared crates appear only in the library group.

Severity: low (accuracy/clarity, not a licensing gap).

### Version fetch, error handling, useEffect cleanup — all correct
`AboutDialog.tsx:52-68` — `getVersion()` is called when `open` becomes true;
initial/fallback state is `"0.1.0"` (matches the manifests). On error it logs and
leaves the fallback. The `cancelled` flag + cleanup function correctly prevent a
`setState` after unmount/close. Dep array `[open]` is correct. ✓

`getVersion` is permitted: `core:app:default` (incl. `allow-version`) is in the
granted capability set. ✓

### Dialog open/close — all paths work
- Opens: Sidebar button `onClick={() => setAboutOpen(true)}` → controlled
  `<Dialog open={open}>`. ✓
- Closes: Escape / outside-click / Close button / `onOpenChange(false)` all route
  to `onClose` → `setAboutOpen(false)`. ✓

### [nit] Triple close-handlers are redundant
`AboutDialog.tsx:71-75` — `onOpenChange={(o) => { if (!o) onClose(); }}` already
covers Escape and outside-click (Radix fires `onOpenChange(false)` for both).
The additional `onEscapeKeyDown={onClose}` and `onInteractOutside={onClose}` are
redundant. `onClose` is idempotent (`setAboutOpen(false)`), so the double-call is
harmless — but the handlers can be dropped in favor of `onOpenChange` alone.
Not a bug.

---

## Bugs

No runtime bugs found.

### [nit] Imported `open` is shadowed by the `open` prop
`AboutDialog.tsx:3,49` — `import { open } from "@tauri-apps/plugin-shell"` is
shadowed inside the component body by the destructured `{ open }` prop (the
boolean). This is **not an active bug**: every shell-`open` call goes through the
module-level `openExternal` helper (lines 32-38), which closes over the import,
not the prop. But it is a latent footgun — a future direct `open(url)` call
inside the component body would invoke the boolean prop and throw at runtime.
Suggested fix: `import { open as openUrl } from "@tauri-apps/plugin-shell"` and
use `openUrl` in `openExternal`. Severity: low (readability/maintenance).

### React keys / state — correct
Group `key={group.title}` (unique) and entry `key={`${group.title}-${dep.name}`}`
(composite, unique within group). No list/key warnings. ✓

---

## Security

**No injection surface.** Every URL passed to `open()` originates from hardcoded
string literals in `dependencies.ts` (`APP_LICENSE_URL`, `dep.registry`,
`dep.licenseUrl`) — no user input reaches `open()`. All are `https://` URLs.

- `shell:allow-open` is granted (`src-tauri/capabilities/default.json:8`); the
  default shell permission validates URLs against `http(s)://`, `tel:`, `mailto:`
  schemes — all our URLs are `https://` and pass. ✓
- The `openExternal` wrapper catches and logs errors, never rethrows — a dead
  link cannot crash the dialog. ✓
- No XSS: dep names render as React text content (`{dep.name}`) and `title`
  attributes (both auto-escaped); no `dangerouslySetInnerHTML`. ✓
- The CSP rationale (production `default-src 'self'` blocks `<a target="_blank">`,
  hence routing through the native shell `open`) is sound. ✓

---

## Constitution compliance

- **Doc comments on all public exports:** `DependencyEntry`, `DependencyGroup`,
  `APP_LICENSE_URL`, `DEPENDENCY_GROUPS`, `AboutDialog` all documented. ✓
  (`AboutDialogProps` is module-local, not exported — no doc required. `crates`,
  `npm`, `openExternal` are module-private but documented anyway.)
- **No `#[allow]` / `@ts-ignore` / `@ts-expect-error` suppressions.** ✓
- **Build warning-free:** user confirms `npm run build` (tsc + vite) green and
  `cargo test` (848) / `vitest` (135) green. Frontend-only change; no Rust
  touched. ✓
- **Line-ending style:** could not be verified directly as a read-only
  reviewer. The git `LF→CRLF` warning observed is on `.coding/backlog.json`
  (a bookkeeping file), not on the feature files. Recommend confirming the new
  `about/*.tsx`/`.ts` files match the repo's existing line-ending convention
  before commit.

---

## [nit] a11y: DialogDescription wraps the entire body
`AboutDialog.tsx:99-153` — `DialogDescription asChild` wraps a `<div>` containing
the intro paragraph, the full scrollable dependency list, and the footer hint.
Screen readers announce the `aria-describedby` content, so the whole list would
be read as the dialog's "description". Cleaner: wrap only the intro `<p>` (or a
visually-hidden summary) as the Description. Low severity (a11y refinement, not
a violation — a Description element is present, satisfying Radix's requirement).

---

## Summary

| Area | Finding | Severity |
|------|---------|----------|
| Correctness | "Rust — app shell" group omits 6 src-tauri direct deps (all already credited in library group) — label overstates | low |
| Bugs | `open` import shadowed by `open` prop (no active bug; latent footgun) | low |
| Bugs | Redundant `onEscapeKeyDown`/`onInteractOutside` (idempotent, harmless) | nit |
| a11y | `DialogDescription` wraps entire body (verbose for SR) | nit |
| Process | Line-ending style of new files not directly verifiable — confirm before commit | nit |
| Security | None | — |

No blocking findings. The About dialog opens/closes correctly, fetches the
version safely, links only hardcoded `https://` URLs through the granted
`shell:allow-open` capability, and satisfies the project constitution.
