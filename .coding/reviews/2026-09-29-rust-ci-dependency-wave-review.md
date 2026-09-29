## Verdict: FINDINGS (1 high, 4 low)

Review of the Rust + CI half of the merged dependency-modernization wave, committed range d158fb7..HEAD on `wt/mnemo` (commits 430ab92, 7e955e1, 3b00931, f2fd909, d0ad6a3, 9e22cc8, e96f51f, 4f9dc26, 8aafd1c, 8e45ae8, 012119c, 81582d1, 4b0764a, 755feee). Acceptance criteria 1, 2, 3 verify clean; criteria 4 and 5 verify with guard-robustness and staleness notes below. The one high finding is a guardrail gap, not a shipped defect: the highest-consequence risk of the wave (vendored-pin loss) is protected against every trigger EXCEPT the one Dependabot will actually propose weekly (`tauri`), and by no automated test.

Reviewed-state: 06fe9ee65aa256002d8b37d230e0c5fa51dabf2e

## What I read

- `git_read op=show` (full diff): 8e45ae8, 3b00931, 7e955e1, f2fd909, 430ab92, 8aafd1c, 4f9dc26, e96f51f, 012119c, 81582d1, 4b0764a, 755feee; `op=show --stat` for d0ad6a3, 9e22cc8 (lock-only).
- Current state: `Cargo.toml:150-189` (patch section), `Cargo.lock` (tao/wry/tauri-runtime-wry entries + `[[patch.unused]]` search), `vendor/` listing, `src-tauri/Cargo.toml`, `src/browser/mod.rs:420-444`, `src-tauri/src/main.rs:985-1014`, `src/agent/context.rs` (tiktoken surface), `src/mcp/oauth.rs` + `src-tauri/src/ipc/laya.rs` (sha2/zip surface), rusqlite call-surface sweep (src/memory, src/codegraph), `tests/integration/ci_workflow.rs` (all 200 lines), `.github/workflows/build.yml`, `.github/workflows/codeql.yml`, `.github/codeql/codeql-config.yml`, `.github/dependabot.yml`, `SECURITY.md`, doc-staleness sweep over `**/*.md`.
- Process remark: the untracked `.coding/tmp/*` and `.coding/plans/41becd2a.md` carry-over files are plan-harness scratch, not wave code — one line, not re-reviewed.

## Criterion 1 — vendored pins (VERIFIED CLEAN)

- No `[[patch.unused]]` entries anywhere in `Cargo.lock` (content-indexed search over the whole file).
- `Cargo.lock:5360` — `tao 0.35.4` has **no** `source = "registry+..."` line and no checksum: it resolves to the `[patch.crates-io]` path override (`Cargo.toml:187`). Same for `wry 0.55.3` at `Cargo.lock:7246`. Exactly one `tao` and one `wry` package entry each — no registry twin. (`tao-macros` staying registry-sourced is correct: only the `tao` library crate is overridden.)
- `tauri-runtime-wry 2.11.4` (`Cargo.lock:5643`) **is** registry-sourced — the acceptance criterion's phrasing ("resolve to path sources") does not match the repo's design, and does not need to: `vendor/` holds only `tao/` and `wry/`; the patch applies transitively, and tauri-runtime-wry's lock dependency list (`Cargo.lock:5658`, `:5665`) names bare `tao` and `wry`, which can only resolve to the single, path-sourced entries. The registry wry 0.57 fallback that would ship the deadlock/SSO bugs is provably absent.
- d0ad6a3 and 9e22cc8 (the two dependabot-group landings) hold the vendored set exactly as documented, with the `--precise 2.11.4` pin described in their messages.

## Criterion 2 — chromiumoxide 0.9.1 / HeadlessMode (VERIFIED CLEAN)

- One production launch site: `src/browser/mod.rs:428-442` — `BrowserConfig::builder()… .new_headless_mode()` with an accurate comment (0.9 made the `config` module private; `new_headless_mode()` sets exactly `HeadlessMode::New`). Eight test sites (mod.rs:2605, 2728, 2907, 3138, 3250, 3347, 3514, 3575) all use `.new_headless_mode()`.
- Repo-wide search for `HeadlessMode|headless_mode`: zero remaining typed `HeadlessMode::` references or `.headless_mode(` calls outside comments; `src-tauri/src` contains no launch/config site at all (prose comments only). No removed API survives.
- Manifest stays `chromiumoxide = 0.9` optional behind the `browser` feature; the reqwest 0.12/0.13 coexistence is lock-only.

## Criterion 3 — major-bump API fallout (VERIFIED CLEAN against real usages)

Every usage site in the current tree is source-compatible with the claimed new APIs; each commit records a green `cargo test`/`cargo check` run, and the PRs' CI is the compile proof:

- **rusqlite 0.40**: the whole surface is `params!`, `query_map`, `query`, `params_from_iter`, `ToSql`, `OptionalExtension`, `Transaction`, `Connection::open_with_flags` + `OpenFlags::SQLITE_OPEN_READ_ONLY` (src/memory/mod.rs, src/codegraph/{store,mod}.rs) — all stable across 0.32→0.40; `bundled` feature kept (lock: libsqlite3-sys 0.38.2).
- **tiktoken-rs 0.12**: only `cl100k_base` + `CoreBPE` (`src/agent/context.rs:2300-2302`).
- **sha2 0.11**: `Sha256::digest` at `src/mcp/oauth.rs:138` and `src-tauri/src/ipc/laya.rs:603`; 0.10 correctly retained in-lock only for tauri-codegen's `^0.10`.
- **toml 1.1**: majors coexist in the lock (tauri/cargo keep 0.8/0.9) — nothing forced forward.
- **zip 8**: three sites, all current-API (`laya.rs:630` `ZipArchive::new`, `:1269` `ZipWriter::new`, `:1270` `zip::write::SimpleFileOptions`).
- **windows-sys 0.61**: feature names in both manifests (`Cargo.toml:123-128`, `src-tauri/Cargo.toml:60-65`) all exist under 0.61 nomenclature; no code change needed.
- **directories 6**: single call site `BaseDirs::new()` in src/config/mod.rs — intact.

## Criterion 4 — CI guards + permissions (VERIFIED, with L4 robustness notes)

- `workflows_pin_actions_to_full_shas` (ci_workflow.rs:160-200) reads the **actual** workflow files (no fixture), covers both files that exist (`.github/workflows/` holds exactly build.yml + codeql.yml), parses every `uses:` line including list-item form, allows only the documented `dtolnay/rust-toolchain@stable` exception, and enforces a real 40-hex ref (length + hexdigit check, comment text after whitespace excluded). It does not match its own comment prose (`#`-prefixed lines never reach the `uses:` strip). Verified-red claim is consistent with the pinned/unpinned diff in 012119c.
- `build_workflow_scopes_token_permissions` (ci_workflow.rs:117-144): the workflow-level block is found via the first column-0 `permissions:` line — in build.yml that is genuinely the workflow level (`build.yml:46-47`), job blocks being indented.
- Permissions semantics are correct: workflow-level `contents: read` (build.yml:46-47) with the release job's explicit override `permissions: contents: write` (build.yml:239-240) — a job-level block fully replaces the workflow-level one on GitHub Actions, so the release job keeps write; no other job widens. CI on the merged PR is the behavioral proof.

## Criterion 5 — SHA comments + dependabot ignores (VERIFIED internally, with H1)

- SHA↔version comments are internally consistent across all occurrences: checkout `3d3c42e5…` # v7 (×4, both files), setup-node `82076278…` # v7 (×2), rust-cache `6323deb1…` # v2 (×2), upload-artifact `043fb46d…` # v7 (×3), download-artifact `3e5f45b2…` # v8 (×3), action-gh-release `efb35369…` # v3, codeql-action init/analyze `2892aa5e…` # v4 (×2). No SHA carries two different version claims; no `uses:` lacks a comment. (The SHA↔tag mapping itself is not checkable from inside the repo — stated as a limitation, not a finding.)
- The `81582d1` YAML-comment fix correctly converts the JS-style `//` headers to `#`; the file as merged parses.
- tao/wry ignore entries match their documented vendor-override reason — see H1 for the missing third leg.

## Findings

### HIGH

**H1 — The #26 failure mode is guarded against every trigger except the one Dependabot proposes weekly, and by no test.** `.github/dependabot.yml:26-32` documents that "bumping **tauri** past 2.11.6 (or tauri-runtime-wry past 2.11.4) moves wry to 0.57 and tao to 0.37, which turns both patches into `[[patch.unused]]` entries and silently ships the registry crates" — yet the ignore list covers only `tao`, `wry`, `tauri-runtime-wry`. `src-tauri/Cargo.toml:13` declares `tauri = { version = "2", … }` (and `:9` `tauri-build = "2"`, itself held at 2.7.0 per 9e22cc8), so the weekly cargo-minor-patch group will keep opening exactly the tauri-bump PR whose merge recreates the Windows keyboard-deadlock / wry-SSO regression — and it would pass CI (registry wry 0.57 compiles fine), leaving only the YAML comment as the guard. The wave itself treats this as its highest-consequence risk (criterion 1), and the repo already has the pattern for machine-checking workflow hygiene in ci_workflow.rs. Fix: add `tauri` (and consider `tauri-build` / `tauri-plugin-dialog`, both deliberately held per d0ad6a3/9e22cc8) to the ignore list with the same rationale, and/or add an integration test asserting `Cargo.lock` contains no `[[patch.unused]]` section — cheap, deterministic, and it converts the documented review rule into an enforced one.

### LOW

**L1 — Stale doc: SECURITY.md:47 still says "CodeQL (default setup)".** 012119c replaced default setup with the advanced-setup workflow (`.github/workflows/codeql.yml`) the same day; SECURITY.md (landed in 4b0764a) was not updated. A security-policy doc that misdescribes the scanner posture is exactly where accuracy matters. Fix: "CodeQL (advanced setup, .github/workflows/codeql.yml)".

**L2 — Stale comment: `.github/dependabot.yml:42-43` — "The workflows pin major action tags (actions/checkout@v5, …)".** Wrong twice over after 012119c: the workflows now pin full commit SHAs (with `# vN` comments Dependabot still updates), and the named example major is v5 where the file pins v7/v8. The stated purpose ("surfaces the next major as a reviewable PR") survives, but the description should be rewritten for SHA pins.

**L3 — Stale comment: `src-tauri/src/main.rs:1001` — "chromiumoxide **0.7** never sets kill_on_drop".** The tree now runs 0.9.1 (8e45ae8). If 0.9 still never sets `kill_on_drop` (nothing in the migration diff suggests otherwise), drop the version from the comment so it cannot rot again; if unverified, re-verify and restate for 0.9 — the whole teardown rationale (orphaned Chromium child holding profile-dir locks) rests on it. (All other `chromiumoxide 0.7` mentions are historical `.coding/` records — out of scope.)

**L4 — Guard robustness (two small holes in the new safety net):**
(a) `workflows_pin_actions_to_full_shas` enumerates the two current workflow files (`ci_workflow.rs:164`) rather than globbing `.github/workflows/*` — the third workflow someone adds next quarter is silently unpinned and unguarded. Reading the directory (like the fixture-free reads it already does) closes it.
(b) `build_workflow_scopes_token_permissions` asserts only that *some* `contents:` line follows the workflow-level `permissions:` (`ci_workflow.rs:126-132`) — a workflow-level `contents: write` (or `write-all`) would pass the guard while defeating its least-privilege purpose. Assert the exact value `read` (it already string-matches the release job's override at `:140-143`, so exact matching is in-pattern).

## Criteria scorecard

1. Vendored pins in Cargo.lock — **clean** (with the note that tauri-runtime-wry is registry-sourced by design; the patches provably apply).
2. chromiumoxide 0.9.1 HeadlessMode migration — **clean**.
3. Major-bump API fallout — **clean** (all call sites checked; compile evidence from each commit's recorded test run + merged-PR CI; no shell available to this reviewer, so no independent `cargo test` was run).
4. CI guards + workflow permissions — **verified**, L4 holes noted.
5. SHA comments + dependabot ignores — internally **verified**, H1 gap.

Documentation sync: no README/PLAN.md claims about the old dependency versions found; the three stale spots are L1-L3. Multi-platform neutrality: no new platform-specific code anywhere in the wave (all migrations are call-site-preserving); windows-sys/DACL/watchdog code is pre-existing and version-bumped only. File-tools-first: all commits landed via the normal toolchain, no shell-mutation markers in any diff. `.coding/**` accuracy: the plan digest and commit messages match what shipped (ranges, versions, PR numbers all check out).
