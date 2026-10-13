// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The G10 boundary tests (job 5 D3, master-spec §9 :923 —
//! `tests/boundary.rs::{kernel_builds_without_app_feature,
//! tauri_uses_only_public_api}`).
//!
//! Two directions:
//! * `kernel_builds_without_app_feature` — the STRUCTURAL half of the
//!   G10 proof: the `app` slice is optional (lib.rs carries the cfg
//!   gates; Cargo.toml the feature lattice). The real app-less BUILD
//!   rides the battery as a command — `cargo build -p mnemo
//!   --no-default-features --features kernel,headless` (job-05 step
//!   file §E deviation 2; the same shape as the tauri-tree proof).
//! * `tauri_uses_only_public_api` — the src-tauri half (§5.9.10
//!   :750-753): the app crate references no `mnemo::agent::` /
//!   `mnemo::runtime::` / `mnemo::host::` internals other than the
//!   re-exported public types — `mnemo::app::` and `mnemo::headless::`
//!   are the sanctioned surfaces, and the crate-root re-exports ride
//!   bare `mnemo::` paths. Test code is exempt (the same rule the
//!   lib-side `kernel_does_not_import_app` applies); the stripper
//!   below is its twin, copied from src/lib.rs:113 so this test
//!   target stands alone.
//!
//! D3 lands the scanner `#[ignore]`d — D4 routes the ~60 production
//! references (job-05 §B fact 5's worklist) and D4's commit drops the
//! ignore; until then the scan would fail the battery by design.

use std::fs;
use std::path::{Path, PathBuf};

/// The structural G10 proof (job 5 D1): the feature lattice exists and
/// gates the slices — `app` and `headless` are optional, `headless`
/// implies `kernel`, and the defaults keep the full library so the mnemo
/// app and the integration tests see zero change.
#[test]
fn kernel_builds_without_app_feature() {
    let manifest = fs::read_to_string("Cargo.toml").expect("the root Cargo.toml");
    for line in [
        "default = [\"kernel\", \"app\", \"headless\"]",
        "kernel = []",
        "headless = [\"kernel\"]",
        "app = []",
    ] {
        assert!(
            manifest.contains(line),
            "Cargo.toml [features] must carry `{line}` — the G10 lattice"
        );
    }
    let lib = fs::read_to_string("src/lib.rs").expect("src/lib.rs");
    assert!(
        lib.contains("#[cfg(feature = \"app\")]\npub mod app;"),
        "src/lib.rs must gate `pub mod app` behind the `app` feature"
    );
    assert!(
        lib.contains("#[cfg(feature = \"headless\")]\npub mod headless;"),
        "src/lib.rs must gate `pub mod headless` behind the `headless` feature"
    );
}

/// The src-tauri half of G10 (§5.9.10 :750-753): production code in the
/// app crate reaches the kernel ONLY through the public surfaces —
/// `mnemo::headless::*`, `mnemo::app::*`, and crate-root re-exports
/// (bare `mnemo::` paths). `mnemo::agent::` / `mnemo::runtime::` /
/// `mnemo::host::` path tokens are boundary violations.
///
/// `#[ignore]`d from D3: the ~60 pre-existing references (job-05 §B
/// fact 5) are D4's worklist; D4's commit drops this attribute and the
/// scan enforces the boundary from then on.
#[test]
fn tauri_uses_only_public_api() {
    let mut sources = Vec::new();
    app_sources(Path::new("src-tauri/src"), &mut sources);
    assert!(
        !sources.is_empty(),
        "the app sources must be found (run from the workspace root)"
    );
    const BANNED: [&str; 3] = ["mnemo::agent::", "mnemo::runtime::", "mnemo::host::"];
    let mut violations = Vec::new();
    for path in &sources {
        let src = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let stripped = strip_test_items(&src);
        for (idx, raw) in stripped.lines().enumerate() {
            // Line comments are exempt (docs may NAME a banned path
            // while explaining a migration). Naive split on `//` — the
            // banned tokens never appear inside URL strings.
            let code = match raw.find("//") {
                Some(at) => &raw[..at],
                None => raw,
            };
            for token in BANNED {
                if code.contains(token) {
                    violations.push(format!(
                        "{}:{}: `{}` in `{}`",
                        path.display(),
                        idx + 1,
                        token,
                        raw.trim()
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "src-tauri production code references kernel internals — route via \
         mnemo::headless / mnemo::app / the crate-root re-exports:\n{}",
        violations.join("\n")
    );
}

/// Collect every `.rs` under `src-tauri/src` (the app's production
/// tree; no exclusions — the whole crate is the scanned surface).
fn app_sources(root: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).expect("src-tauri/src readable") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            app_sources(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Strip `#[cfg(test)]`-attributed items from a source — the twin of
/// the lib-side stripper (src/lib.rs:113, copied verbatim so this test
/// target stands alone; keep the two in sync deliberately). Test code
/// is the test harness and may reference kernel internals freely, so
/// its bytes never reach the token scan. Mechanical: the attribute
/// opens a skip that consumes the decorated item up to its balanced
/// closing brace, or its `;` for braceless items (`use`/`const`). The
/// brace counter is literal-aware (strings, raw strings, char
/// literals; lifetimes discriminated by lookahead); line comments skip
/// to end-of-line.
fn strip_test_items(src: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if !lines[i].trim_start().starts_with("#[cfg(test)]") {
            kept.push(lines[i]);
            i += 1;
            continue;
        }
        let mut depth: i64 = 0;
        let mut opened = false;
        while i < lines.len() {
            let t = lines[i];
            i += 1;
            // A `;`-terminated line that is not a comment closes the
            // item hunt: the attribute line carried a braceless item,
            // or this IS the braceless item. Doc comments between the
            // attribute and the item must NOT close it.
            if !opened && t.trim_end().ends_with(';') && !t.trim_start().starts_with("//") {
                break;
            }
            let cs: Vec<char> = t.chars().collect();
            let mut j = 0;
            while j < cs.len() {
                let c = cs[j];
                if c == '"' {
                    j += 1;
                    while j < cs.len() {
                        if cs[j] == '\\' {
                            j += 2;
                            continue;
                        }
                        if cs[j] == '"' {
                            j += 1;
                            break;
                        }
                        j += 1;
                    }
                    continue;
                }
                if c == '/' && j + 1 < cs.len() && cs[j + 1] == '/' {
                    break; // the rest of the line is a comment
                }
                if c == 'r' && j + 1 < cs.len() && (cs[j + 1] == '#' || cs[j + 1] == '"') {
                    // A raw-string opener (r"…" / r#"…"#, any hash
                    // count) — skip to the close.
                    let mut k = j + 1;
                    let mut hashes = 0;
                    while k < cs.len() && cs[k] == '#' {
                        hashes += 1;
                        k += 1;
                    }
                    if k < cs.len() && cs[k] == '"' {
                        let mut m = k + 1;
                        let mut end = cs.len();
                        while m < cs.len() {
                            if cs[m] == '"'
                                && m + hashes < cs.len()
                                && (0..hashes).all(|h| cs[m + 1 + h] == '#')
                            {
                                end = m + 1 + hashes;
                                break;
                            }
                            m += 1;
                        }
                        j = end;
                        continue;
                    }
                    // `r#ident` — falls through as a plain char.
                }
                if c == '\'' {
                    // `'…'` (a char literal, ≤4 content chars with
                    // escapes) vs `'a` (a lifetime): lookahead.
                    let mut k = j + 1;
                    let mut len = 0;
                    let mut closed = false;
                    while k < cs.len() && len <= 4 {
                        if cs[k] == '\\' {
                            k += 2;
                            len += 2;
                            continue;
                        }
                        if cs[k] == '\'' {
                            closed = true;
                            break;
                        }
                        k += 1;
                        len += 1;
                    }
                    if closed && len <= 4 {
                        j = k + 1; // a char literal — braces never count
                        continue;
                    }
                    // a lifetime — falls through as a plain char
                }
                if c == '{' {
                    depth += 1;
                    opened = true;
                } else if c == '}' && depth > 0 {
                    depth -= 1;
                }
                j += 1;
            }
            if opened && depth == 0 {
                break;
            }
        }
    }
    kept.join("\n")
}
