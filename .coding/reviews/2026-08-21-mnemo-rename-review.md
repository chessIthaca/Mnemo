# Review: myharness → Mnemo rename + go-public hygiene

**Scope:** all uncommitted changes on `feat/mnemo-rename` (~257 tracked files in `git diff HEAD` + 4 new untracked files: `LICENSE`, `README.md`, `docs/workflow.svg`, `scripts/add-copyright-headers.ps1`). Read-only review: no shell; files inspected via file_read + fresh tree-walk searches.

> **Tooling caveat:** the `search` tool's literal FTS index is **stale** (it still reports pre-rename content, e.g. `Cargo.toml:2 name = "myharness"` and hits in the deleted `test_output.txt`). All completeness checks below were therefore re-run as regex `(?i)myharness` queries served by a fresh tree walk (`engine: walk`) and confirmed against direct file reads.

## Verdict

The rename is complete and correct. Migration logic is right on all edge cases and is called first in `main()`. LICENSE is the intact canonical PolyForm Noncommercial 1.0.0 text. Contract fixtures match. README claims verified against config/code. No correctness, security, or constitution violations. All findings are low severity.

---

## Findings (ordered by severity — all LOW; no critical/high/medium)

### L1 — Stale doc comment reintroduces the MIT claim
**File:** `frontend/src/components/about/dependencies.ts:47-50`

The doc comment on `APP_LICENSE_URL` still reads:

> `The license Mnemo itself is published under (both Cargo.toml files declare \`license = "MIT"\`).`

Both Cargo.tomls now declare `license-file = "LICENSE"` (PolyForm Noncommercial) — the comment is factually wrong and contradicts the very rename this plan shipped (the About dialog body was fixed, its data file's comment was not). Rewrite the comment to describe the PolyForm Noncommercial license (drop the parenthetical about `license = "MIT"`).

### L2 — Header script's BOM claim is inaccurate (latent)
**File:** `scripts/add-copyright-headers.ps1:15-18,55-62`

The doc block claims "Byte-level read/write round-trips the UTF-8 BOM". It does not: `[System.Text.Encoding]::UTF8.GetString($bytes)` decodes a leading BOM into a `\uFEFF` char *inside* `$text`, and the write (`$utf8.GetBytes($header + $text)` with a no-BOM encoder) would emit that char **after** the header. rustc only tolerates a BOM at byte 0, so a BOM'd `.rs` input would come out uncompilable. Currently latent (the repo was verified BOM-free and the run was green), but the script is committed for future reuse. Fix either the doc (state that inputs must be BOM-free / BOMs are stripped) or the code (detect a decoded leading `\uFEFF` and re-emit it at byte 0). Line-ending preservation (per-file CRLF/LF detection) and idempotency (500-char marker check, marker present in the header itself) are correct as implemented.

### L3 — postcss.config.js missed by the header sweep
**File:** `frontend/postcss.config.js` (6 lines, no header)

The script covers `frontend/*.ts` configs but not `*.js`; this is the repo's only `.js` config file and the only build/config source file without the header (a `//` header is valid JS). The plan's stated scope was `*.ts` configs, so this is a consistency gap rather than a scope violation — but "every source file" (the script's own `.SYNOPSIS`) argues for adding it.

### L4 — Comment example still uses "myHarness" as a folder name
**File:** `src-tauri/src/main.rs:255`

`(e.g. "Mnemo — myHarness")` — the product half was correctly renamed; the example project-folder name is still the old product name. Cosmetic only; `Mnemo — myproject` (or similar) avoids confusion.

### L5 — Commit must explicitly add the new untracked files (process note)
`LICENSE`, `README.md`, `docs/workflow.svg`, `scripts/add-copyright-headers.ps1` do not appear in `git diff HEAD` (untracked). If the commit step forgets `git add` on these, the branch lands without its LICENSE/README. (Tracked status inferred from absence in the diff stat — the `git status` section of the diff output was truncated.)

---

## Areas verified clean (no findings)

**Migration logic & call order — clean.** `src/config/mod.rs:400-421`: `migrate_legacy_config_dir`/`migrate_legacy_dir` handle all four edge cases correctly (both dirs exist → no-op, new never clobbered; only new → no-op; only old → `fs::rename`, same-parent/same-volume so atomic; neither → no-op). Rename failure is logged via `eprintln!` and swallowed (never fatal) — as specified. Three unit tests (`config/mod.rs:489-524`) cover move/keep/noop; the failure path is not unit-testable portably — acceptable. `src-tauri/src/main.rs:110` calls it as the **first statement of `main()`**, before the console-mode check (:118), `inherit_shell_path` (:131), `browser_inspection_enabled` (:162, which reads `config.toml`), `install_panic_hook` (:170), and `build_brain` (:233). Doc comments on the new public function ✓.

**Header application — clean (except L2/L3).** `//` headers ahead of `#![deny(warnings)]` are legal Rust; both crate roots keep the attribute effective (`src/lib.rs:5`, `src-tauri/src/main.rs:10-11`). Repo-wide walk found the SPDX marker in all 100 `.rs` files under `src/` (87 files searched), `src-tauri/`, `tests/`; spot checks confirm headers on `frontend/src` .ts/.tsx/.d.ts, `src-tauri/build.rs`, `frontend/{vite,vitest,tailwind}.config.ts`. Idempotency and per-file EOL detection verified in the script source. No `.rs` file exists where a leading `//` would be illegal. The two `#[allow(clippy::too_many_arguments)]` in `src/agent/loop_impl.rs:348,388` are **pre-existing** (that file's diff is +4 header lines only) — no `#[allow]` was added.

**Contract-fixture parity — clean.** `"/home/user/.mnemo"` byte-identical in `src-tauri/src/ipc/contract_fixtures.rs:204` and `frontend/src/lib/ipc-fixtures/dto-get-settings.json:2`.

**LICENSE — clean.** All canonical PolyForm Noncommercial 1.0.0 sections present and unmodified (Acceptance, Copyright License, Distribution License, Notices, Changes and New Works License, Patent License, Noncommercial Purposes, Personal Uses, Noncommercial Organizations, Fair Use, No Other Rights, Patent Defense, Violations with the 32-day cure clause at :97-103, No Liability, full Definitions). `Required Notice:` line prepended at :1 in the license's own example format. No secrets or machine-specific absolutes (the github.com URL is public, matching the license's example).

**Rename completeness — clean (except L4).** Fresh case-insensitive tree walks: `src/**/*.rs` → only the justified legacy-config path/tests (`config/mod.rs:362-520`) and the dual-prefix sweep guard (`browser/mod.rs:284,295`); `src-tauri/**/*.rs` → only the migration comment (:107) + L4 nit; `tests/`, `frontend/` (157 files), `docs/`, `scripts/`, `.github/`, root `*.toml`/`*.json`/`*.bat`/`Cargo.lock` → zero hits; root `*.md` → only `README.md:81` (justified migration note) and `fullreview.md` (justified historical). `.coding/*` and root `reviews/*.md` keep the old name as intended. Cargo.lock regenerated (`mnemo` @3131, `mnemo-app` @3165; no `myharness` entries). Env vars: `MNEMO_DISABLE_WIN_OCCLUSION` (`main.rs:164`), `MNEMO_BASE_URL/API_KEY/MODEL` (`tests/provider_integration.rs:23-29`); user-agent `mnemo-agent/0.1` (`web_fetch.rs:155`); panic prefix `mnemo panic:` (`app/mod.rs:17`); `mnemo-settings-<date>.json` (`AdvancedSection.tsx:195`); `About Mnemo` title/aria (`Sidebar.tsx:41-42`); `<title>Mnemo</title>` (`index.html:6`); `productName`/`identifier` (`tauri.conf.json:3,5`); npm names (`package.json:2`, `frontend/package.json:2`, `package-lock.json:2,8,15`); binary name `mnemo-app.exe` (`build.bat:33`, `start.bat:37,51`); workflow step name + `mnemo.app.zip` in BOTH ditto (:164) and upload-artifact path (:171). The old `myharness:open-file` CustomEvent is gone entirely (a pre-existing refactor replaced it with store-held `pendingFileOpen`; zero `open-file` hits in `frontend/src`) — nothing stale. Browser profile prefix `mnemo-browser-` (:257) with the dual-prefix sweep keeping the legacy `myharness-browser-` arm (:295); test-only prefixes renamed (`mnemo-spike-`, `mnemo-webview-*`). `test_output.txt` deleted ✓.

**README accuracy — clean.** Build commands verified: `npm install` at root (workspaces hoist the Tauri CLI), `npx tauri dev`/`build` from `src-tauri/` against `beforeDevCommand: "npm run dev"` (`tauri.conf.json:9`, script exists at `package.json:9`), devUrl port **5179** (`tauri.conf.json:8`, `vite.config.ts:12`), `npm test` = `vitest run` (`frontend/package.json:10`), `cargo test` workspace = lib + app (`Cargo.toml:90-92`). Windows-only claim for the Browser tab + `game_*` tools verified against `browser_webview.rs:15-22,68-70` (`cfg!(windows)` gate, `UNSUPPORTED_BROWSER_TAB_MSG`). macOS 11+ matches `tauri.conf.json:30`; CI builds both platforms (`build.yml` windows + macos jobs). `~/.mnemo` config claim matches `config/mod.rs:384-386`; the `~/.myharness` mention is the justified migration note. License section matches LICENSE. `docs/workflow.svg` exists, is a valid hand-authored SVG (title/desc present), and is referenced via `<img>` as intended (`README.md:45`).

**Security/policy — clean.** No secrets or machine-specific absolutes introduced into README/LICENSE. About dialog no longer claims MIT (`AboutDialog.tsx:109-117` states PolyForm Noncommercial, links `APP_LICENSE_URL`; dependency MIT/Apache/ISC badges are the *dependencies'* licenses — correct).

**Constitution compliance — clean.** New public function `migrate_legacy_config_dir` has a full doc comment; the private helper is documented too. No `#[allow(...)]` added anywhere. Migration behavior has 3 unit tests (the rename is not a defect fix, so no regression test is owed beyond those). `#![deny(warnings)]` intact at both crate roots. `.coding/backlog.json`/`plans/stack.json` churn is live-state bookkeeping, not source.
