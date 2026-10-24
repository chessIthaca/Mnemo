## Verdict: PASS

Both round-1 findings are genuinely closed by the uncommitted delta on `wt/mnemo`: F1's test expansion now passes `test = true` on the pinned macro, and F2's feature gate removes the Mnemo2 context from every default link unit while the feature-enabled paths (Mnemo2 lib tests + host bin) stay covered in CI on both platforms — no coverage weakened, no new hazard, no stale live reference. The delta is committable.

Reviewed-state: base `d46fd7789e75eac8fc5c1ad2afc83fda93ca7649` (still HEAD — no intervening commits; the uncommitted diff IS the delta).

### Scope and method

- Read the full uncommitted diff + status (10 changed files + untracked `src-tauri/mnemo2-app/`, 3 plans, 3 knowledge records, the round-1 report). No commits to walk between rounds — the round-1 host-fix hunks re-show uncommitted (process remark, not re-litigated; I re-checked only the surfaces the task named).
- Read in full or in the changed regions: `.github/workflows/build.yml` (windows job steps; macOS job lines 120–179), `src-tauri/Cargo.toml` (all 75 lines incl. tail), `src-tauri/src/lib.rs`, `src-tauri/mnemo2-app/tauri.conf.json`, `package.json`, `mnemo2/README.md` (Run/native sections), the `mod.rs` / `bin/mnemo2.rs` / design-doc / build.rs hunks, `Cargo.lock` at the tauri-macros entry, and all six bookkeeping files.
- Tree-wide literal searches: `mnemo2.conf`, `--bin mnemo2`, `mnemo2:native`, `mnemo_app::mnemo2`.

### F1 (high) — CLOSED

- `src-tauri/src/mnemo2/mod.rs` `context_uses_mnemo2_identifier` now expands `tauri::generate_context!("mnemo2-app/tauri.conf.json", test = true)` — exactly the round-1 prescribed fix. `test = true` skips only the runtime-only macOS dev plist embedding; `context.config().identifier` still comes from the parsed config, so the `com.mnemo.mnemo2` assertion is unaffected (gate: feature-enabled `--lib` run, 10 passed, includes it).
- Pinned-macro acceptance, first-hand: `Cargo.lock` is untouched by the delta and still pins **tauri-macros 2.7.0** (Cargo.lock:5539–5540, registry source) — the exact version round 1 verified against upstream, whose `test` option exists for precisely this case. The green `cargo test -p mnemo-app --features mnemo2 --lib` is direct machine proof the pinned macro accepts and expands the flag. On macOS the lib test binary is now left with exactly ONE non-test expansion (`run()`'s) → a single `_EMBED_INFO_PLIST` → the reuse-guard compile error round 1 predicted is gone.

### F2 (low, pre-existing) — CLOSED without weakening coverage

- Default `cargo test --workspace` (feature off): `#[cfg(feature = "mnemo2")]` drops the module from the lib (the main bin links only `main.rs`'s own context), and `[[bin]] name="mnemo2" required-features=["mnemo2"]` makes cargo skip the host bin entirely — no Mnemo2 context in any default link unit. Gate: default workspace test exit 0.
- Feature-enabled paths still cover the host: `cargo test -p mnemo-app --features mnemo2 --lib` (10 passed) runs the Mnemo2 lib tests + regression test; `cargo build -p mnemo-app --features mnemo2 --bin mnemo2` (exit 0) builds the host; `npm run test:mnemo2:native` wraps the same command; `dev:mnemo2:native` passes `--features mnemo2` and `build:mnemo2:native` passes `mnemo2,custom-protocol` (release ⇒ dev=false ⇒ no plist block — safe, unchanged from round-1 analysis).
- CI placement is exactly as required. Windows: new step right after the workspace-test step (build.yml:65–68). macOS: after the `id: rust-tests` / `continue-on-error: true` workspace step and the artifact upload, **immediately before** the `steps.rust-tests.outcome == 'failure'` fail gate (build.yml:151–154 before 155–159) — so it runs even when the workspace step fails, its own failure fails the job directly (no continue-on-error), and a passing feature step cannot mask a workspace failure (the gate still runs after it). Coverage is additive in both jobs.
- No new compile/link hazard: valid TOML (`mnemo2 = []` under `[features]`; the single explicit `[[bin]]` at Cargo.toml:42–45 takes precedence over the auto-discovered `src/bin/mnemo2.rs` — no duplicate target, proven by both green builds; the file tail contains no other `[[bin]]`); the cfg-gate's only consumer is `src/bin/mnemo2.rs`, itself feature-required (`mnemo_app::mnemo2` appears nowhere else in live code) — no ungated reference can be stranded; valid YAML (steps indented consistently with siblings, `run: |` block scalars); valid JSON (the npm gates ran). CI's dev-profile builds need no `target/mnemo2-dist` (devUrl present ⇒ no asset embedding), so no Vite pre-build is required on the runners.

### Checklist items 4–5

4. **Stale references — none live.** `mnemo2.conf` and the old no-feature `--bin mnemo2` survive only in `.coding/` plans/reviews/knowledge history (historical narrative; out of scope by rule). Live code, `mnemo2/README.md`, and the design doc all point at `mnemo2-app/tauri.conf.json` and feature-enabled commands; root `README.md` references only the unchanged script names (`dev:mnemo2:native` etc.), which the delta did not rename.
5. **Host fix still holds (carry-over, delta did not break it).** `src-tauri/mnemo2-app/tauri.conf.json` re-read: identical to round 1's verified state (`com.mnemo.mnemo2`, `frontendDist ../../target/mnemo2-dist` = Vite outDir, `../icons/*`, macOS bundle section, no `before*` commands). `build.rs`'s `rerun-if-changed=mnemo2-app/tauri.conf.json` and `run()`'s first-statement `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` clearing are unchanged; the feature gate does not affect codegen path resolution.

### Constitution lines

- **Docs sync — PASS:** the feature and its commands are documented in `mnemo2/README.md` and `.coding/design/mnemo2.md`; module (`lib.rs`, `mod.rs`) and bin doc comments updated; root README/PLAN.md contain nothing that went stale.
- **Multi-platform — PASS:** no `cfg(windows)` additions outside the sanctioned gate; `std::env::remove_var` is std; CI compiles the feature tree on BOTH runners; the macOS bundle config is preserved.
- **File-tools-first — PASS:** no shell-based mutation in the delta; the config-file delete is the round-1-sanctioned filesystem delete, unchanged.
- **Warning-free — PASS:** both feature states green under `#![deny(warnings)]` at both crate roots → zero warnings in changed code; no `#[allow]`.
- **Bug-plan obligations — PASS:** root cause + regression test recorded in plan 59ef5921 and both BUG knowledge records; the F1 guard is inherently macOS-compile-shaped (Windows red is impossible — the plist block is macOS-only), and the new CI macOS step is the standing red/green executor on that platform. The round-1 report itself is the red-evidence analysis.

### Cleared (considered, no finding)

- An ad-hoc `cargo test -p mnemo-app --features mnemo2` (all targets, not `--lib`) would still double-embed in the main-bin test target on macOS dev — no shipped or CI command does this (both are `--lib`); the default workspace test is feature-off by design.
- With the feature off, bare `cargo run -p mnemo-app` now resolves to the Mnemo1 main bin only (required-features-skipped targets are invisible to target selection) — an improvement, not a hazard.
- The `rustfmt --check` drift in `src-tauri/src/mnemo2/ipc.rs`/`runtime.rs` is pre-existing in files the delta does not touch — not flagged per the task.
- Process remark: the round-1-verified host-fix hunks (config relocation, build.rs, env clearing) re-show uncommitted in this delta because nothing was committed between rounds; verified as carry-over only.

### Bookkeeping accuracy (one line)

Accurate: plan 59ef5921's "Landed F1/F2" paragraph matches the shipped diff and gates verbatim; the new double-embed BUG record is faithful to the round-1 mechanism and fix; plan 52d7bb81's main steps remain checked with its known pre-diagnosis "Detailed steps" nits unchanged; the Phase-1 SPEC record still matches base HEAD. One non-blocking accuracy nit: plan 072d6739's step 1 still carries the pre-gate literal command `cargo run --manifest-path src-tauri/Cargo.toml --bin mnemo2` — the plan's own line-12 amendment records that the npm script now passes `--features mnemo2`, so when the parent executes that step it should use `npm run dev:mnemo2:native` (or add the flag); the raw command as written now fails with "target `mnemo2` requires the features: `mnemo2`".

Reviewed-state: d46fd7789e75eac8fc5c1ad2afc83fda93ca7649
