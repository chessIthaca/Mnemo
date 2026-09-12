# Link-time research — round 2 (post-lld), 2026-12-07

Goal: find what still hurts dev/test link time after the 2026-09-19 rust-lld +
debug="line-tables-only" changes landed. Toolchain 1.95.0, Windows 11, NVMe SSD,
rust-lld confirmed active (.cargo/config.toml target.x86_64-pc-windows-msvc.linker).

## Measured baseline (warm incremental, real-edit-then-revert probes)

| Operation | Time | Notes |
|---|---|---|
| no-op cargo build | 0.4-0.5s | fingerprint check only |
| lib rlib recompile (real edit) | 3.5s | incremental; NO linking (rlib) |
| lib full recompile | ~33.8s | when incremental cache invalidated |
| cargo build -p mnemo-app | 8.6s | app crate + exe LINK (~98MB exe + ~488MB PDB) |
| cargo test --lib | 13.7s | test-profile rebuild + test-bin link + 1740 tests (4.6s) |
| cargo test FULL | 19.4s | lib + 4 integration bins + tao_backport + doctests |

## Key findings

1. **The warm loop is already healthy.** Post-lld, a full edit->test cycle is
   ~14-20s. The pain is NOT the everyday incremental link.
2. **cargo build at the workspace root links NOTHING.** Default members = root
   package (mnemo rlib) only; the app exe needs `-p mnemo-app`/`--workspace`.
   Also: mtime-only touches do NOT dirty cargo fingerprints (measure with real
   reverted edits).
3. **The pain is full rebuilds** (toolchain update, dep bump, feature/RUSTFLAGS
   flips, cargo clean): ~33.8s full lib recompile + 6-7 separate binaries each
   paying a ~1-8s link against ~470-490MB of PDB mass per binary.
4. **target\debug = 158.1 GB**: 26.9GB / 2,649 rlibs (4-5 stale hash variants
   per crate), 25.1GB / 392 PDBs (~480MB each), 84 exes incl. stale
   `myharness_app` relics from the pre-rename crate. The variant pile-up shows
   recurring fingerprint churn (flag/profile flips between cargo invocations).
5. **Windows Defender real-time protection is ON**; exclusions unverifiable
   without admin. Every link rewrites ~0.5GB of PDB through Defender's filter.

## Ranked recommendations

| # | Lever | Type | Expected win | Risk |
|---|---|---|---|---|
| 1 | `[profile.dev.package."*"] debug = false` + same under `[profile.test.package."*"]` | Cargo.toml, 2 lines | Big: dep rlibs lose debuginfo -> far smaller link input + PDBs for all 6-7 binaries; dep backtraces lose file:line (names stay); workspace code keeps line-tables | low; one full rebuild to take effect |
| 2 | Defender exclusions for `target\`, `~\.cargo`, `~\.rustup` (admin PS) | environment | Unknown but often seconds-per-link on Windows | none |
| 3 | `cargo clean` (one-time) | hygiene | Reclaims ~158GB, kills stale variants + myharness relics; smaller Defender scan surface | one ~3-5min full rebuild |
| 4 | Feature-gate heavyweight deps (chromiumoxide -> browser tools, fastembed/ort -> embeddings) | code, medium project | Largest structural cut to link mass (ort/onnxruntime native lib dominates PDB) | feature plumbing through tauri |
| 5 | Merge 4 integration test bins into 1 binary | code, small | ~3-6s per full-suite run (each bin = own link) | low |
| 6 | codegen-units tune for lib (256 -> 64) | Cargo.toml | marginal; trades compile parallelism | low |

Not applicable: split-debuginfo (MSVC/PDB unsupported), /DEBUG:FASTLINK via
lld-link (not exposed through cargo; moot once #1 lands).

## Follow-ups (implementation, if wanted)
- Plan A (quick): #1 + #3 (config + clean), then re-measure the table above.
- Plan B (structural): #4 feature-gating, biggest long-term dev-loop win.
