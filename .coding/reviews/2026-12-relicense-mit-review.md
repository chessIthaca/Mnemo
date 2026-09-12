## Verdict: PASS

Reviewed ALL uncommitted changes on `feat/relicense-mit` via `git diff HEAD`
(275 files: +580 / −682). The change is a PolyForm Noncommercial 1.0.0 → MIT
relicense across the whole project plus a completion of the About dialog's
dependency manifest (7 newly-added direct deps). Every check item below
passes; one optional, pre-existing, non-blocking doc-comment nit is noted at
the end.

---

### 1. License accuracy — PASS

Each of the 7 newly-added dependencies has the correct SPDX label and
license-text URL. Independently re-verified the three Rust crates via the
crates.io API; the npm/DefinitelyTyped ones match established facts.

| Dep | Manifest label | licenseUrl | Source of truth | Match |
|---|---|---|---|---|
| notify | CC0-1.0 | CC0 | crates.io `notify` 8.0.0/8.2.0 → `"license":"CC0-1.0"` | ✓ |
| image | MIT OR Apache-2.0 | MIT | crates.io `image` 0.25.x → `"license":"MIT OR Apache-2.0"` | ✓ |
| tree-sitter | MIT | MIT | crates.io `tree-sitter` 0.26.12 → `"license":"MIT"` | ✓ |
| tree-sitter-rust | MIT | MIT | tree-sitter org, GitHub LICENSE (plan-verified) | ✓ |
| tree-sitter-typescript | MIT | MIT | tree-sitter org, GitHub LICENSE (plan-verified) | ✓ |
| d3-force | ISC | ISC | npm package.json (D3 packages are ISC) | ✓ |
| @types/d3-force | MIT | MIT | DefinitelyTyped standard (MIT) | ✓ |

License-URL constants all point at the right canonical pages:
- `MIT` / `APACHE` / `ISC` → `https://opensource.org/licenses/{MIT,Apache-2.0,ISC}` ✓
- `CC0` → `https://creativecommons.org/publicdomain/zero/1.0/` (canonical CC0 1.0 URL) ✓
- `APP_LICENSE_URL` → `https://opensource.org/licenses/MIT` (was the PolyForm URL) ✓

Dual-license convention honored: `image` ("MIT OR Apache-2.0") links to MIT
(the first/more-permissive option), consistent with the file's documented rule
and with every other dual-licensed crate already in the manifest.

### 2. Manifest completeness — PASS

Cross-checked `dependencies.ts` against all three manifests. Every direct
dependency appears exactly once; nothing missing, nothing stale.

- **Root `Cargo.toml`** — 27 `[dependencies]` + 1 `[target.'cfg(windows)'.dependencies]`
  (windows-sys) = 28. The "Rust — library" group lists exactly these 28. ✓
- **`src-tauri/Cargo.toml`** — `mnemo` (path, internal — correctly omitted);
  shared crates (tokio, serde, serde_json, anyhow, async-trait, base64,
  windows-sys) already credited in the library group; shell-only crates
  (tauri, tauri-build, tauri-plugin-shell, tauri-plugin-dialog, log) = 5 in
  "Rust — app shell (additional)". ✓
- **`frontend/package.json`** — 12 `dependencies` → "Frontend — runtime"
  (now incl. d3-force); 12 `devDependencies` → "Frontend — build & dev"
  (now incl. @types/d3-force). ✓
- Dev-dependencies (tokio test-util, tempfile, futures) are all duplicates of
  already-credited regular deps, so no unique dev-only crate is uncredited. ✓

### 3. Copyright header correctness — PASS

Spot-checked Rust (`//`), TypeScript (`//`), and PowerShell (`#`) files across
the diff. Every header now reads:
```
SPDX-License-Identifier: MIT
See LICENSE in the repository root.
```
The "(non-commercial use only)" clause is dropped everywhere. A repo-wide
search for `LicenseRef-PolyForm` and `non-commercial|noncommercial|PolyForm`
found matches ONLY in `.coding/` plan/review files (out of scope) and in
`scripts/relicense-to-mit.ps1`'s own replacement-target strings (legitimate).
Zero stale PolyForm identifiers remain in any live source file.

**Relicense script** (`scripts/relicense-to-mit.ps1`) is BOM-safe and
line-ending-preserving, matching `add-copyright-headers.ps1`'s byte-level
pattern: `[System.IO.File]::ReadAllBytes`/`WriteAllBytes`, BOM detected via
`$text[0] -eq [char]0xFEFF` then stripped and re-emitted at byte 0 with
`$bomBytes`, payload encoded with `UTF8Encoding($false)`. Line endings are
preserved because both replacements are within-line `.Replace()` calls (no EOL
characters touched) — correct, since the script adds/removes no lines.
`add-copyright-headers.ps1` was also updated to emit the MIT header
(`$headerLines` lines 39-40) and carries the MIT header itself.

### 4. Documentation sync — PASS

All surfaces consistently say MIT:
- `README.md`: "Mnemo is licensed under the [MIT License](LICENSE)." ✓
- `AboutDialog.tsx` body: "MIT License" (was "PolyForm Noncommercial License
  1.0.0 (free for non-commercial use)"). ✓
- Both `Cargo.toml`s: `license = "MIT"` (was `license-file`). ✓
- Both `package.json`s: `"license": "MIT"` added. ✓
- `dependencies.ts` doc comment: "The license Mnemo itself is published under:
  MIT (both Cargo.toml files declare `license = "MIT"`)." ✓
- `LICENSE`: canonical MIT text (Copyright (c) 2026 Carsten Hess). ✓

No stale PolyForm references in any live (non-`.coding/`) file.

### 5. Multi-platform neutrality — PASS

The change is purely textual (headers, license metadata, manifest data). The
relicense script is PowerShell but is a dev/maintenance tool, not app or
library code — no Windows-only APIs, paths, or shell syntax introduced into
shippable code. No `cfg(windows)`-only additions outside the existing
sanctioned Browser/game tooling gate.

### 6. AboutDialog.tsx JSX — PASS

The intro paragraph renders correctly:
```
Mnemo is licensed under the{" "}
<button ...>MIT License</button>
. Built on these open-source
projects:
```
Rendered text: **"Mnemo is licensed under the MIT License. Built on these
open-source projects:"** — the `{" "}` yields a single space before the button;
the period follows the button with no intervening space; the newline between
"open-source" and "projects:" collapses to one space (JSX whitespace rules).
No stray spaces, no missing punctuation.

---

### Optional, non-blocking observation (not a finding)

`dependencies.ts:15-18` — the doc comment lists the crates shared between the
library and app shell as "(tokio, serde, serde_json, anyhow, async-trait,
base64)" but omits `windows-sys`, which is also a direct dep of BOTH manifests
(both declare it under `[target.'cfg(windows)'.dependencies]`) and is credited
once in the library group. This is pre-existing text (untouched by this PR),
the "Several" framing is illustrative rather than exhaustive, and the actual
manifest data is correct (windows-sys IS credited exactly once). No action
required to merge; optionally add `windows-sys` to the parenthetical for full
accuracy if desired.

---

**Conclusion.** The relicense is complete and accurate across LICENSE, both
Cargo.tomls, both package.jsons, README, the About dialog (body + manifest +
APP_LICENSE_URL), the copyright-header script, and ~268 source-file headers.
The 7 newly-added manifest entries have correct SPDX labels and license URLs
(independently re-verified via crates.io). The manifest is complete — every
direct dependency appears exactly once. No correctness, security, or
constitution violations. No findings.
