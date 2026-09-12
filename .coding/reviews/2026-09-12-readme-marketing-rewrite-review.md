## Verdict: FINDINGS (3 high, 6 low)

Review of ALL uncommitted changes on `wt/mnemo` for plan 2c19aca0 "Rewrite README as a marketing front door + move deep detail to docs/": README.md full rewrite (59,997 B / 157 lines → ~10 KB / 176 lines, 141+/122−), new docs/FEATURES.md (45,146 B) + docs/CONFIGURATION.md (8,362 B), untracked plan file `.coding/plans/2c19aca0.md`. Diff confirms no source code changed. Content preservation is verified byte-exact per ask 1 and all proof numbers match the measured set; the 3 high findings (three malformed H2 headings, a false `cargo test` scope comment, one broken image link in docs/FEATURES.md) should be fixed before commit. Details in the sections below.
### Findings — HIGH

**HIGH 1 — README.md:36, 67, 143: three H2 headings are missing the space after `##` — they do not render as headings.**
- L36: `##Highlights` · L67: `##How the repository works` · L143: `##Development & tests`
- CommonMark ATX headings require a space between the `##` marker and the heading text; `##Highlights` renders as a literal paragraph starting with "##Highlights" on GitHub. The Highlights, repository-anatomy, and tests sections lose their headings and vanish from the page outline/TOC. Every other heading in the file (L18 `## Why Mnemo exists`, L52, L102, L157, L168, L174) is well-formed, so these three look like an edit artifact — note all three immediately follow a `---` rule line.
- Fix: `## Highlights`, `## How the repository works`, `## Development & tests`.

**HIGH 2 — README.md:146: `cargo test  # the whole workspace: library + app` — false; the root run covers only the `mnemo` library.**
- Cargo.toml L159-166: `[workspace] members = ["src-tauri"]` with no `default-members`, and the workspace root is itself a package (`mnemo`) — plain `cargo test` at the root selects the current package only; src-tauri's (`mnemo-app`'s) tests do not run. This is the project's own standing recipe (memory HOW: full test coverage = root `cargo test` + `cargo test -p mnemo-app` + `npx tsc --noEmit` + frontend `npm test`), and the E0425 incident in the standing record — a broken src-tauri test module shipped under a green root-only run — is exactly this misconception in action.
- The command block is otherwise right: the `cargo test -p mnemo-app` line right below it covers the app. Only the comment is wrong, and it errs in the direction that makes a user skip the second line.
- Fix: change the comment to `# the mnemo library crate (the app is the next line)` — or change the command to `cargo test --workspace` and drop the separate `-p mnemo-app` line.
- Related proof-wording note (no separate finding — the number matches your prescribed set): "2,320 Rust" is the library's suites (root run 2304+16); `mnemo-app`'s 305 measured tests are additional Rust tests not covered by that figure. If you want the proof line to describe the whole repo, "≈2,600 Rust" or "2,320 library + 305 app tests" would be the precise form; as-is it matches your prescribed numbers, flagged only for conscious acceptance.

**HIGH 3 — docs/FEATURES.md:77: `<img src="assets/workflow.svg">` is broken from docs/ — needs `../assets/workflow.svg`.**
- The byte-exact move preserved the old README's root-relative image path, but from `docs/FEATURES.md` it resolves to `docs/assets/workflow.svg`, which does not exist (`assets/` contains only the root `workflow.svg`, 4,245 bytes). The workflow diagram will not load on the rendered page.
- This is the one point where ask 1 (byte-exact) and ask 4 (link integrity) collide. It is the only actual hyperlink/image inside the moved content — every other path mention in the preserved text (`vendor/tao/PATCHES.md` at L25, `vendor/wry/PATCHES.md` at L57, config paths) is backtick prose, not a link. The identical `<img>` at README.md:55 resolves correctly from the root.
- Fix: amend the byte-exact rule for this single attribute (`src="../assets/workflow.svg"` — a 3-byte deviation) or consciously accept the broken image. Recommended: fix the path; a dead diagram in the new exhaustive reference undercuts the rewrite's purpose.
### Findings — LOW

**LOW 1 — README.md:131: bundle mode presents Windows installer formats as universal.**
- "`npm start bundle` | produce installers (`.msi`/`.nsis`) under `target/release/bundle`" — on macOS the same command produces a `.app`/`.dmg` (the old README stated this explicitly; `scripts/start.mjs` mode `bundle` is plain `tauri build`, which picks the host bundlers). The table follows a quickstart that addresses both platforms, so this is a Windows-only assumption presented as universal (review ask 5).
- Fix: "produce platform installers (`.msi`/`.nsis` on Windows, `.app`/`.dmg` on macOS)".

**LOW 2 — docs/CONFIGURATION.md:18: stray trailing line `Copyright (c) 2026 Carsten Hess.` left over from the old License section.**
- It sits outside the preserved Configuration section (it is the old README's final line, old L157) — a line-range trim slip. As a bare copyright with no license statement it reads oddly in a config reference, and it uses the old "(c)" form while the new README uses "©". Fix: delete it, or keep it deliberately as a footer (if deliberate, note that).

**LOW 3 — README.md:153: "Two optional cargo features exist on the library" — Cargo.toml defines a third (`test-support`).**
- Root Cargo.toml L12-28 defines `browser`, `embeddings`, and `test-support` (the dev-only test-factory gate, enabled by src-tauri via dev-dependencies; never in release builds). The old README explained it; the rewrite drops it and asserts a count of two. Fix: "Two heavyweight optional cargo features…" or add "(plus a dev-only `test-support` feature for cross-crate test factories)".

**LOW 4 — README.md:32 and 170: "every one of them planned, reviewed, and tested" is unverifiable as stated.**
- The only remaining unverifiable claim after the number swap (everything else in the proof section matches the measured set exactly). A universal quantifier over ~210,000 lines cannot be shown; the "1,303 commits" claim was dropped for exactly this class of reason. Consider "every change leaves a plan, an independent review, and a test suite behind" — or keep it as conscious marketing license.

**LOW 5 — README.md:30: the loop order "code → review → test → commit" inverts the enforced sequence.**
- The enforced closing sequence runs tests first, then the review, then commit — docs/FEATURES.md:82 (the preserved text) itself says "the test matrix runs, a read-only reviewer subagent audits the full diff". "spec → plan → code → test → review → commit" would match the machine.

**LOW 6 — file hygiene nits.**
- README.md has no trailing newline (the diff carries `\ No newline at end of file`; the old file had one).
- docs/FEATURES.md and docs/CONFIGURATION.md both start with a blank line 1 before the H1 (copy-trim artifact; markdownlint MD041 class). Harmless on GitHub; fix if you care.
### Verification per review ask

**1. Content preservation — PASS (byte-exact confirmed).**
- Method (no shell available — textual only): full side-by-side of the diff's removed old-README lines against both docs files (all 87 + 18 lines read), plus literal searches confirming the exact tail fragments of the four multi-KB preserved lines — FEATURES.md L43 steering bullet ends "…zero-hit browses say \"no matches\" instead of rendering bare", L69 backlog bullet ends "…the manual resume/rollback anchor"; CONFIGURATION.md L10 ends "…resolution is by config, never by model-name prefix", L16 ends "…deleted files self-heal on the next init". The corruption-prone line tails all survived intact; every compared line matches, and file sizes corroborate the slice arithmetic.
- docs/FEATURES.md L6-85 = old README L30-109 exactly: `## Key ideas` (7 bullets + "A chat window can suggest…" close), `## Key features` (Workflow & safety ×6, Memory ×9, Code intelligence ×4 including the two giant bullets, Agents & interface ×27), `## The enforced workflow` (the `<p>`/img block, numbered items 1-4 in their LONG forms, the "Spec-driven and test-first…" close). Clean boundaries: starts exactly at old L30 (`## Key ideas`), ends exactly at old L109's closing paragraph — nothing missing, no spill into `## Building`.
- docs/CONFIGURATION.md L6-16 = the five Configuration paragraphs (Global config, Models inside an endpoint, MCP servers, MCP two-scope, Per-project state) byte-exact. Modulo-headers rule respected: each file's additions are exactly the H1 (L2) + intro paragraph (L4); the only extra is the stray L18 (LOW 2).
- Copy-Item provenance: outcome verified byte-exact as claimed; the justification (an LLM re-emitting ~8 KB single-line bullets through file_write risks corrupting them) is sound and documented in the plan — no file-tools-policy finding. It is outside the constitution's two literal shell-mutation carve-outs, so keep the plan-file documentation as the record of the exception.

**2. Accuracy — PASS except HIGH 2 and LOWs 1/3.** Verified against the repo:
- Launcher modes table = `scripts/start.mjs` exactly: no-arg = `tauri build --no-bundle` then launch; `dev` = `tauri dev`; `build` = no launch; `bundle` = installers under `target/release/bundle` (workspace-shared root `target/` — correct as written). `npm start` → root package.json `"start": "node scripts/start.mjs"` ✓; `npm install` at root = workspace install (workspaces: ["frontend"]) ✓; "Tauri CLI is hoisted to the root node_modules" ✓ (start.mjs/build.bat comments; workspace hoisting); `npx tauri dev`/`build` from `src-tauri/` ✓.
- `start.bat`/`build.bat` exist (53/33 lines) but are no longer mentioned in the README — no claim, nothing to falsify.
- Repo anatomy L70-91: all ten `src/` entries listed exist (agent/, workflow/, memory/, codegraph/, tool/, safety_rules/, provider/, model_resolver.rs, mcp/, backlog.rs) ✓; `tests/integration/` ✓; `src-tauri/tests/` = tao_backport.rs + wry_sso_patch.rs + wry_hard_reload_patch.rs — exactly the three vendored-patch source-contract tests claimed ✓.
- Vendored patches: `vendor/wry/PATCHES.md` + `vendor/tao/PATCHES.md` exist and say exactly what the README claims (wry 0.55.3 = OS-account SSO + hard-reload; tao 0.35.4 = upstream PR #1215 Windows keyboard/IME self-deadlock backport), wired via `[patch.crates-io]` (root Cargo.toml L184-186: `tao = { path = "vendor/tao" }`, `wry = { path = "vendor/wry" }`) ✓.
- Features: `browser` = dep:chromiumoxide and `embeddings` = dep:fastembed, both off by default; `src-tauri/Cargo.toml` selects both — "the app always selects both" ✓. `.cargo/config.toml` rust-lld is gated to `[target.x86_64-pc-windows-msvc]` only (macOS unaffected per its own comment); the new README makes no linker claim, so nothing to falsify ✓.
- `frontend/package.json`: vitest ^4.1.10 ("vitest 4") ✓, `npm test` = `vitest run` ✓, React 18 + TypeScript + Vite + Tailwind ✓.
- LICENSE = MIT, Copyright (c) 2026 Carsten Hess ✓ (README's "MIT — see LICENSE. Copyright © 2026 Carsten Hess." ✓).

**3. Proof numbers — PASS.** README.md:170 carries exactly the measured set: "As of September 2026: 651 plans, 858 review reports, and 503 knowledge records …, ~210,000 lines …, and 2,320 Rust + 1,100 frontend tests" — all figures and the as-of date match. Stale claims gone: "1,303 commits" absent (repo history is a single squashed initial commit — `git log` shows only `3217714 Initial commit` — so the old number was indeed unmeasurable), "~160,000 lines in first 23 days" absent, the January-2027 count block absent. Remaining unverifiable claim flagged as LOW 4; the 2,320-vs-305 scope nuance noted under HIGH 2.

**4. Link integrity — PASS except HIGH 3.** Every link enumerated:
- README.md: `#proof-not-promises` anchor → "## Proof, not promises" ✓; `assets/workflow.svg` (L55, root-relative) ✓; `docs/FEATURES.md` + `docs/CONFIGURATION.md` ✓ (untracked, will exist at commit); `PLAN.md`, `agent.md`, `vendor/wry/PATCHES.md`, `vendor/tao/PATCHES.md`, `LICENSE` ✓ all exist; external badges/rustup/nodejs links ✓.
- docs/FEATURES.md: `../README.md` ✓, `./CONFIGURATION.md` ✓, plus the one broken img (HIGH 3).
- docs/CONFIGURATION.md: `./FEATURES.md` ✓, `../README.md` ✓.

**5. Multi-platform neutrality — PASS except LOW 1.** Windows and macOS prereq blocks both present; the launcher is genuinely cross-platform (start.mjs branches only for the Windows exe-lock taskkill and cmd.exe npm shim); the clone/build commands work verbatim in PowerShell; the WebView2 Browser tab is properly scoped "(Windows)" (the sanctioned exception, L46 + L109); no other Windows-only assumption presented as universal.

**6. Intentional deviations — all three verified as described, none flagged as errors:**
- (a) README.md:42 "Tree-sitter parses 12 languages (Rust, TS/TSX, JS, Python, Go, Java, C/C++, C#, Ruby, PHP, HTML)" — accurate: Cargo.toml pins exactly those 12 tree-sitter grammars (rust, typescript, javascript, python, go, java, c, cpp, c-sharp, ruby, php, html). docs/FEATURES.md:40 keeps the old "(Rust + TypeScript/TSX)" byte-exact per the verbatim-preserve choice ✓.
- (b) "1,303 commits" dropped ✓ (unmeasurable — see ask 3); "~160,000 lines in first 23 days" replaced by the measured ~210,000 ✓.
- (c) Clone URL is the obvious placeholder `https://github.com/<owner>/mnemo` (README.md:118) — no discoverable remote. Open item for the final summary: the real owner/repo URL must be chosen before publishing.

**Doc-sync check (constitution):** PLAN.md contains zero README references (searched — no matches), so no stale pointers to the moved sections; agent.md's "README.md (feature lists, config examples)" expectation is satisfied (features in README + docs/FEATURES.md, config in docs/CONFIGURATION.md, README's docs map routes to both); no other user-facing doc references the old anchors. Conscious losses outside the preserved ranges, for the record: the old build section's vitest-include registration caveat (still enforced loudly by `src/lib/vitestInclude.test.ts` and a HOW memory) and the old CI mention (`.github/workflows/build.yml` — the new README makes no CI claim either way, so nothing false).

Note on method: the reviewer cannot run shell — all verification is textual (git diff/read/search over the working tree and HEAD). Test results cited are the parent's measured evidence, cross-checked only for internal consistency with the repo's structure.