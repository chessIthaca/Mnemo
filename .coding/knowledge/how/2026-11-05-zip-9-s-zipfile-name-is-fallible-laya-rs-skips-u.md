+++
title = "zip 9's ZipFile::name() is fallible; laya.rs skips undecodable entry names"
created = "2026-11-05"
+++

zip 9 (8.6.0 -> 9.3.0) changed `ZipFile::name()` from `&str` to `ZipResult<Cow<'_, str>>`. That is the only zip API break in this tree: `ZipArchive::new`, `by_index()`, `ZipFile::is_dir()`, and the writer (`start_file` + `SimpleFileOptions`, used by the laya fixtures) are unchanged.

Fix in `src/host/laya.rs` `extract_uv_asset` (commit 3f7303bc): `let Ok(name) = entry.name() else { continue };` — an undecodable name is skipped and the scan continues, which is what zip 8 did implicitly (`String::from_utf8_lossy` -> U+FFFD, never equal to the ASCII binary, so the entry never matched). Propagating with `?` would instead abort extraction over a decoy entry.

Entry names are still only compared, never used as write paths, so the traversal refusal is untouched. Pre-existing (unchanged by the bump): the zip branch checks `is_dir()` but not `is_symlink()`, and `io::copy` writes a symlink entry's target text as file content — no traversal, but not a symlink either.

Bump mechanics that worked: `zip = "9"` in both manifests + `cargo update -p zip --precise 9.3.0`; `cargo update -p rustls` (0.23.43 -> 0.23.46, single rustls version in the tree). Targeted `cargo update -p` only, never a blanket update — Cargo.lock must keep the vendored wry/tauri patches (no `[[patch.unused]]`, `wry` entry source-less), enforced by `cargo_lock_keeps_the_vendored_patches` in tests/integration/ci_workflow.rs, which runs as `cargo test --test integration ci_workflow::` (there is no `--test ci_workflow` target).
