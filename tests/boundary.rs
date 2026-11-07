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
//!   below is its twin — the lib-side cfg(test) `boundary` module in
//!   src/lib.rs — so this test target stands alone. The scan covers
//!   `src-tauri/src` plus the separate `mnemo2-app` crate (`src` and
//!   `examples`), and skips test-only files.
//!
//! D3 lands the scanner `#[ignore]`d — D4 routes the ~60 production
//! references (job-05 §B fact 5's worklist) and D4's commit drops the
//! ignore; until then the scan would fail the battery by design.

use std::fs;
use std::path::{Path, PathBuf};

/// The scanned surface: the app crate plus the separate `mnemo2-app`
/// crate (`src` and `examples`) — its helper binary imports the kernel
/// directly, so the boundary must cover it too.
const ROOTS: [&str; 3] = [
    "src-tauri/src",
    "src-tauri/mnemo2-app/src",
    "src-tauri/mnemo2-app/examples",
];

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
    for root in ROOTS {
        app_sources(Path::new(root), &mut sources);
    }
    assert!(
        !sources.is_empty(),
        "the app sources must be found (run from the workspace root)"
    );
    // The scan is only a boundary proof if the mnemo2-app crate is
    // really in it — a wrong root (a typo, a move) must fail loudly
    // rather than pass silently over one crate.
    let normalised: Vec<String> = sources
        .iter()
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect();
    assert!(
        normalised
            .iter()
            .any(|path| path.contains("mnemo2-app/src/mnemo2_mcp/")),
        "the mnemo2-app crate must be scanned"
    );
    assert!(
        normalised
            .iter()
            .any(|path| path.contains("mnemo2-app/examples/")),
        "the mnemo2-app crate must be scanned"
    );
    // The three banned module PATHS — BARE forms (the substring
    // subsumes every `::`-qualified use) — plus the alias/grouped-
    // import forms that would smuggle them past a path-only scan
    // (review round 1, L2a: unlike the lib side, where the later
    // crate split makes the crate-alias route structurally
    // impossible, src-tauri imports mnemo as an external crate
    // FOREVER — these forms can never be legitimate here).
    // Residual, documented as the lib side documents its holes:
    // NESTED grouped aliases (`use mnemo::{app::{x}, agent}`) are not
    // token-matched — the practical guard is eyeballing every new
    // `use mnemo::{` line against the ten pure-facade groups.
    const BANNED: [&str; 7] = [
        "mnemo::agent",
        "mnemo::runtime",
        "mnemo::host",
        "use mnemo as",
        "mnemo::{agent",
        "mnemo::{runtime",
        "mnemo::{host",
    ];
    let mut violations = Vec::new();
    for path in &sources {
        let src = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let stripped = strip_test_items(&src);
        for (idx, raw) in stripped.lines().enumerate() {
            // Line comments are exempt (docs may NAME a banned path
            // while explaining a migration) — STRING-AWARE (review
            // round 1, L2b): a `//` inside a string literal (a URL, a
            // regex) is not a comment start, so a banned token later
            // on such a line is still scanned.
            let code = code_part(raw);
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

/// Guard: a file is only exempt from the boundary scan when it really
/// is a test-only file — some declaring file in the same directory (or
/// one level up) puts it under `#[cfg(test)]` with a `#[path =
/// "..."]` attribute or a `mod <stem>;` declaration. A production
/// file cannot hide by being called `tests.rs`.
#[test]
fn test_only_files_are_declared_under_cfg_test() {
    let mut declaring = Vec::new();
    let mut candidates = Vec::new();
    for root in ROOTS {
        all_sources(Path::new(root), &mut declaring);
        test_sources(Path::new(root), &mut candidates);
    }
    assert!(
        !candidates.is_empty(),
        "the test-only sources must be found (run from the workspace root)"
    );
    let mut undeclared = Vec::new();
    for candidate in &candidates {
        let name = candidate
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let stem = candidate
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        let dir = candidate.parent();
        let up = dir.and_then(|dir| dir.parent());
        let declared = declaring.iter().any(|file| {
            let file_dir = file.parent();
            (file_dir == dir || file_dir == up) && declares_test_module(file, name, stem)
        });
        if !declared {
            undeclared.push(candidate.display().to_string());
        }
    }
    assert!(
        undeclared.is_empty(),
        "a test-only-named file must be declared under #[cfg(test)] — a \
         production file cannot hide behind tests.rs / *_tests.rs / \
         testutil.rs (declare it with #[path = \"...\"] or `mod <stem>;`):\n{}",
        undeclared.join("\n")
    );
}

/// `is_test_source`'s names: the three test-only spellings, and the
/// production lookalikes that must keep being scanned.
#[test]
fn test_source_names_are_recognised() {
    for path in [
        "tests.rs",
        "serve_tests.rs",
        "testutil.rs",
        "src/x/tests.rs",
    ] {
        assert!(
            is_test_source(Path::new(path)),
            "`{path}` is a test-only source"
        );
    }
    for path in ["serve.rs", "latest.rs", "contests.rs", "main.rs"] {
        assert!(
            !is_test_source(Path::new(path)),
            "`{path}` is production code and must be scanned"
        );
    }
}

/// The code part of a line — everything before a `//` line comment,
/// STRING-AWARE (review round 1, L2b): a `//` inside a normal or raw
/// string literal (a URL, a regex) is not a comment start, so a
/// banned token later on such a line is still scanned. The literal
/// discrimination mirrors the stripper's (escapes; raw strings with
/// any hash count; `r#ident` falls through as a plain identifier).
/// Multi-line raw strings are approximated: a continuation line's
/// `//` still cuts — exempt-direction-only, the same narrow class the
/// lib side documents.
fn code_part(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => return &line[..i],
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'r' if i + 1 < bytes.len() && (bytes[i + 1] == b'#' || bytes[i + 1] == b'"') => {
                let mut k = i + 1;
                let mut hashes = 0;
                while k < bytes.len() && bytes[k] == b'#' {
                    hashes += 1;
                    k += 1;
                }
                if k < bytes.len() && bytes[k] == b'"' {
                    let mut m = k + 1;
                    let mut found = false;
                    while m < bytes.len() {
                        if bytes[m] == b'"'
                            && (0..hashes).all(|h| bytes.get(m + 1 + h) == Some(&b'#'))
                        {
                            i = m + 1 + hashes;
                            found = true;
                            break;
                        }
                        m += 1;
                    }
                    if !found {
                        // Unterminated raw string: the whole line is
                        // literal — no code part to scan after here.
                        return line;
                    }
                } else {
                    i += 1; // `r#ident` — a plain identifier char
                }
            }
            _ => i += 1,
        }
    }
    line
}

/// Whether `path` names a test-only source: `tests.rs`, `*_tests.rs`
/// or `testutil.rs`. Test code may reference kernel internals freely,
/// so such a file never reaches the boundary scan — exactly as
/// `strip_test_items` exempts inline `#[cfg(test)]` items. The
/// mnemo2-app crate declares them with `#[path = "..."]` under
/// `#[cfg(test)]`, and the team/runtime splits put their tests in such
/// files too.
fn is_test_source(path: &Path) -> bool {
    match path.file_stem().and_then(|stem| stem.to_str()) {
        Some(stem) => stem == "tests" || stem == "testutil" || stem.ends_with("_tests"),
        None => false,
    }
}

/// Collect every `.rs` under `root` — the unfiltered walk the
/// declaration guard reads module declarations from.
fn all_sources(root: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(root).unwrap_or_else(|_| panic!("{} readable", root.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            all_sources(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Collect every production `.rs` under `root` (the whole tree):
/// test-only files (`tests.rs`, `*_tests.rs`, `testutil.rs`) are
/// skipped because test code may reference kernel internals, exactly
/// as `strip_test_items` exempts inline `#[cfg(test)]` items; the
/// mnemo2-app crate and the team/runtime splits put their tests in
/// such files.
fn app_sources(root: &Path, out: &mut Vec<PathBuf>) {
    let mut found = Vec::new();
    all_sources(root, &mut found);
    for path in found {
        if !is_test_source(&path) {
            out.push(path);
        }
    }
}

/// Collect ONLY the test-only `.rs` under `root` — the files
/// `app_sources` skips; `test_only_files_are_declared_under_cfg_test`
/// proves each one is declared under `#[cfg(test)]`.
fn test_sources(root: &Path, out: &mut Vec<PathBuf>) {
    let mut found = Vec::new();
    all_sources(root, &mut found);
    for path in found {
        if is_test_source(&path) {
            out.push(path);
        }
    }
}

/// Whether `declaring` puts the file named `name` (module stem `stem`)
/// under `#[cfg(test)]` — the declaration is a `#[cfg(test)]` line
/// followed within the next two lines by `#[path = "….rs"]` (the
/// crate's convention) or by `mod <stem>;`.
fn declares_test_module(declaring: &Path, name: &str, stem: &str) -> bool {
    let Ok(src) = fs::read_to_string(declaring) else {
        return false;
    };
    let lines: Vec<&str> = src.lines().collect();
    let mod_line = format!("mod {stem};");
    for (idx, line) in lines.iter().enumerate() {
        if line.trim() != "#[cfg(test)]" {
            continue;
        }
        for next in lines.iter().skip(idx + 1).take(2) {
            let next = next.trim();
            let path_attr = next
                .strip_prefix("#[path = \"")
                .and_then(|rest| rest.strip_suffix("\"]"));
            if path_attr.is_some_and(|attr| attr.ends_with(name)) || next == mod_line {
                return true;
            }
        }
    }
    false
}

/// Strip `#[cfg(test)]`-attributed items from a source — the twin of
/// the lib-side stripper (the cfg(test) `boundary` module in src/lib.rs,
/// copied verbatim so this test
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
