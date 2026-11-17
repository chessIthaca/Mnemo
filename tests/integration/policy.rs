// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Repository policy ratchets.
//!
//! Two rules that used to live only as prose in `agent.md` are enforced here
//! by tests that freeze today's exceptions and fail on a new one:
//!
//! - `cfg_windows_stays_in_known_files`: library and app code must build and
//!   behave on macOS too. The only sanctioned Windows-only feature is the
//!   WebView2 Browser pane; every other platform split is listed with the
//!   reason it must differ.
//! - `allow_attributes_do_not_grow`: `#![deny(warnings)]` sits at the crate
//!   roots, so `#[allow(...)]` may not spread. Existing attributes are frozen
//!   per file; adding one fails, and removing one asks to tighten the entry.
//!
//! Both ratchets are plain text scans over the Rust sources of the library and
//! the app crates (std only, no new dependencies), so they run with every
//! `cargo test` on every platform.

use std::path::Path;

/// Directories scanned for `*.rs` files, relative to the repo root.
const ROOTS: &[&str] = &["src", "src-tauri/src", "src-tauri/mnemo2-app/src"];

/// Every source file allowed to carry Windows-only code, with the reason.
///
/// Keep sorted by path. Adding a file here means the code really must differ
/// on Windows; everything else must be platform-neutral.
const WINDOWS_CFG_FILES: &[(&str, &str)] = &[
    (
        "src-tauri/mnemo2-app/src/mnemo2_mcp.rs",
        "helper executable name (.exe)",
    ),
    (
        "src-tauri/mnemo2-app/src/mnemo2_mcp/instance.rs",
        "app executable name, detached spawn, stdio inheritance",
    ),
    (
        "src-tauri/mnemo2-app/src/mnemo2_mcp/instance_tests.rs",
        "cmd child processes, Windows-only stdio tests",
    ),
    (
        "src-tauri/mnemo2-app/src/mnemo2_mcp/permissions.rs",
        "USERPROFILE vs HOME environment variables",
    ),
    (
        "src-tauri/src/instance_marker.rs",
        "pid liveness via OpenProcess vs ps",
    ),
    (
        "src-tauri/src/lib.rs",
        "Windows resource manifest link for the feature-gated tests",
    ),
    (
        "src-tauri/src/mnemo2/browser.rs",
        "WebView2 Browser pane availability",
    ),
    (
        "src-tauri/src/mnemo2/claude_code.rs",
        "claude executable names and config paths",
    ),
    (
        "src-tauri/src/mnemo2/files_view.rs",
        "Windows drive/UNC path rejection test",
    ),
    (
        "src-tauri/src/mnemo2/lock_holders.rs",
        "Restart Manager lock probing (rstrtmgr.dll)",
    ),
    (
        "src-tauri/src/mnemo2/mcp_settings.rs",
        "powershell stand-in process in a timeout test",
    ),
    (
        "src-tauri/src/mnemo2/mod.rs",
        "window-title assertion with a Windows path",
    ),
    (
        "src-tauri/src/mnemo2/phase3.rs",
        "CREATE_NO_WINDOW for git, Windows directory symlinks",
    ),
    (
        "src-tauri/src/mnemo2/project.rs",
        "ping stand-in process, junction refusal test",
    ),
    (
        "src-tauri/src/mnemo2/run_file.rs",
        "icacls privacy test for run files",
    ),
    (
        "src-tauri/src/mnemo2/specs_settings.rs",
        "cmd /C for the npm .cmd shim, symlink helpers",
    ),
    (
        "src-tauri/src/mnemo2/team/deletion.rs",
        "case-insensitive path comparison, junction detection",
    ),
    (
        "src-tauri/src/mnemo2/team/tests.rs",
        "junction creation, held-checkout fallback test",
    ),
    (
        "src-tauri/src/mnemo2/trace_view.rs",
        "reveal-in-file-manager command (explorer vs open)",
    ),
    (
        "src-tauri/src/mnemo2/workbench.rs",
        "junction detection, FILE_FLAG_OPEN_REPARSE_POINT",
    ),
    (
        "src-tauri/src/watchdog.rs",
        "thread stack and wait-chain capture via windows_sys",
    ),
    (
        "src-tauri/src/webview_udf.rs",
        "per-instance WebView2 user data folder (Browser pane)",
    ),
    (
        "src/agent/factory.rs",
        "PowerShell vs sh test commands, Windows browser schema-weight test",
    ),
    (
        "src/agent/step_verify.rs",
        "CREATE_NO_WINDOW for the verifier's own process spawn",
    ),
    (
        "src/agent/tests.rs",
        "cmd vs sh test commands",
    ),
    (
        "src/app/worktrees.rs",
        "checkout path comparison, junction detection, process kill",
    ),
    (
        "src/browser/mod.rs",
        "Chromium profile tests rely on Windows file locking",
    ),
    (
        "src/config/keys.rs",
        "Windows user-only DACL via windows-sys and icacls",
    ),
    (
        "src/host/laya.rs",
        "Job Objects, CREATE_NO_WINDOW, process-tree kill",
    ),
    (
        "src/process_tree.rs",
        "Windows Job Objects for process-tree ownership",
    ),
    (
        "src/project/git_ops.rs",
        "CREATE_NO_WINDOW for spawned git commands",
    ),
    (
        "src/tool/agent/git.rs",
        "CREATE_NO_WINDOW for spawned git commands",
    ),
    (
        "src/tool/agent/git_diff.rs",
        "CREATE_NO_WINDOW for spawned git commands",
    ),
    (
        "src/tool/agent/git_read.rs",
        "CREATE_NO_WINDOW for spawned git commands",
    ),
    (
        "src/tool/agent/sandbox.rs",
        "junction, hard-link and 8.3 alias differences",
    ),
    (
        "src/tool/agent/shell.rs",
        "Job Objects, CREATE_NO_WINDOW, PowerShell chain translation",
    ),
    (
        "src/tool/agent/write_review_report.rs",
        "Windows absolute-path rejection test",
    ),
];

/// Frozen count of `#[allow(...)]` / `#![allow(...)]` attributes per file.
///
/// Files not listed are allowed zero. A line that starts with a `//` comment
/// and an occurrence inside a double-quoted string literal on the same line
/// are not counted.
const ALLOW_COUNTS: &[(&str, usize)] = &[
    // Test-only helpers: `#![allow(dead_code)]` on the module.
    ("src-tauri/mnemo2-app/src/mnemo2_mcp/testutil.rs", 1),
    ("src-tauri/src/mnemo2/ipc.rs", 1),
    ("src-tauri/src/mnemo2/runtime.rs", 1),
    ("src-tauri/src/mnemo2/team/dispatch.rs", 3),
    ("src-tauri/src/mnemo2/team/pump.rs", 1),
    ("src-tauri/src/mnemo2/team/spawn.rs", 1),
    ("src/agent/failure_triage.rs", 2),
    // A prose mention inside the rendered reviewer prompt (a multi-line
    // string literal, so the line-scoped string check cannot see it); kept in
    // the baseline because removing it never fails, it only asks to lower the
    // entry to 0.
    ("src/agent/review_scope.rs", 1),
];

/// Every `*.rs` file under [`ROOTS`] as (repo-relative path with `/`
/// separators, file text), sorted by path.
fn source_files() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for rel in ROOTS {
        collect_rust_files(root, &root.join(rel), &mut files);
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}

/// Recursively collect every `*.rs` file under `dir` into `files`.
fn collect_rust_files(root: &Path, dir: &Path, files: &mut Vec<(String, String)>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("policy ratchet cannot read {}: {error}", dir.display()));
    for entry in entries {
        let path = entry.expect("policy ratchet directory entry").path();
        if path.is_dir() {
            collect_rust_files(root, &path, files);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path.as_path())
                .to_string_lossy()
                .replace('\\', "/");
            let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
                panic!("policy ratchet cannot read {}: {error}", path.display())
            });
            files.push((rel, text));
        }
    }
}

/// True when `text` contains a `cfg(...)` or `cfg!(...)` argument that names
/// `windows`.
///
/// Every spelling the compiler accepts counts, not only the bare
/// `cfg(windows)`: combined forms such as `cfg(all(windows, feature = "x"))`
/// and `cfg(any(windows, ...))`, negations such as
/// `cfg(not(target_os = "windows"))`, and the macro form `cfg!(...)`. For each
/// `cfg(` / `cfg!(` the argument is the balanced-parenthesis body; it counts
/// when it contains the word `windows` (not preceded or followed by an ASCII
/// alphanumeric or `_`), so `windows_sys` and `windows_like` do not count.
fn has_windows_cfg(text: &str) -> bool {
    let mut offset = 0;
    while let Some(open) = find_next_cfg_open(&text[offset..]) {
        let open = offset + open;
        let argument_start = open + 1;
        let mut depth = 1usize;
        let mut argument_end = text.len();
        for (index, ch) in text[argument_start..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        argument_end = argument_start + index;
                        break;
                    }
                }
                _ => {}
            }
        }
        if contains_word(&text[argument_start..argument_end], "windows") {
            return true;
        }
        // Continue after the opening paren, not after the close, so a `cfg(`
        // without a matching `)` cannot loop forever.
        offset = argument_start;
    }
    false
}

/// The byte offset of the `(` in the earliest `cfg(` or `cfg!(` in `text`.
fn find_next_cfg_open(text: &str) -> Option<usize> {
    let plain = text.find("cfg(").map(|index| index + 3);
    let macro_form = text.find("cfg!(").map(|index| index + 4);
    match (plain, macro_form) {
        (Some(plain), Some(macro_form)) => Some(plain.min(macro_form)),
        (Some(plain), None) => Some(plain),
        (None, Some(macro_form)) => Some(macro_form),
        (None, None) => None,
    }
}

/// True when `text` contains `word` delimited by non-word characters, where a
/// word character is an ASCII alphanumeric or `_`.
fn contains_word(text: &str, word: &str) -> bool {
    let bytes = text.as_bytes();
    let boundary = |byte: Option<u8>| {
        byte.is_none_or(|byte| !byte.is_ascii_alphanumeric() && byte != b'_')
    };
    let mut cursor = 0;
    while let Some(relative) = text[cursor..].find(word) {
        let index = cursor + relative;
        let before = index.checked_sub(1).map(|at| bytes[at]);
        let after = bytes.get(index + word.len()).copied();
        if boundary(before) && boundary(after) {
            return true;
        }
        cursor = index + 1;
    }
    false
}

/// Windows-only `cfg` code may live only in the files listed in
/// [`WINDOWS_CFG_FILES`]; a new one fails with the platform-neutral fix, and a
/// listed file that lost its Windows code asks to be removed from the list.
#[test]
fn cfg_windows_stays_in_known_files() {
    let files = source_files();
    assert!(
        !files.is_empty(),
        "no Rust sources found under {ROOTS:?} — the ratchet walk is broken"
    );

    let mut windows_files: Vec<&str> = files
        .iter()
        .filter(|(_, text)| has_windows_cfg(text))
        .map(|(path, _)| path.as_str())
        .collect();
    windows_files.sort_unstable();

    let mut findings = Vec::new();
    for path in &windows_files {
        if !WINDOWS_CFG_FILES.iter().any(|(known, _)| known == path) {
            findings.push(format!(
                "{path} adds Windows-only code. Library and app code must build \
                 and behave on macOS too; the only sanctioned Windows-only \
                 feature is the WebView2 Browser pane. Make the code \
                 platform-neutral, or, if it really must differ by platform, \
                 add the file to WINDOWS_CFG_FILES in tests/integration/policy.rs \
                 with the reason and give macOS its own branch."
            ));
        }
    }
    for (path, _) in WINDOWS_CFG_FILES {
        if !windows_files.contains(path) {
            findings.push(format!(
                "{path} no longer has Windows-specific code; remove it from \
                 WINDOWS_CFG_FILES."
            ));
        }
    }
    assert!(findings.is_empty(), "{}", findings.join("\n"));
}

/// The `#[allow(...)]` count per file may not grow; a removal asks to tighten
/// the frozen entry so the ratchet keeps closing in.
#[test]
fn allow_attributes_do_not_grow() {
    let mut findings = Vec::new();
    for (path, text) in source_files() {
        let count = allow_attribute_count(&text);
        let max = allow_limit(&path);
        if count > max {
            findings.push(format!(
                "{path} has {count} #[allow(...)] attributes, more than the \
                 {max} allowed. Fix the warning's cause instead. If a lint must \
                 stay off on purpose, use #[expect(lint, reason = \"why\")], \
                 which also fails once the lint no longer fires."
            ));
        } else if count < max {
            findings.push(format!(
                "{path} now has {count} #[allow(...)]; lower its ALLOW_COUNTS \
                 entry to {count}."
            ));
        }
    }
    assert!(findings.is_empty(), "{}", findings.join("\n"));
}

/// Windows detection must see combined forms, negations and the macro form,
/// not only the bare `cfg(windows)` spelling.
#[test]
fn windows_cfg_detection_covers_combined_forms() {
    for windows in [
        "#[cfg(all(windows, feature = \"x\"))]",
        "#[cfg(any(windows, target_os = \"linux\"))]",
        "cfg!(windows)",
        "cfg!(target_os = \"windows\")",
        "#[cfg(not(target_os = \"windows\"))]",
    ] {
        assert!(
            has_windows_cfg(windows),
            "{windows} must count as Windows-only"
        );
    }
    for other in [
        "#[cfg(unix)]",
        "use windows_sys::Win32;",
        "#[cfg(feature = \"windows_like\")]",
    ] {
        assert!(
            !has_windows_cfg(other),
            "{other} must not count as Windows-only"
        );
    }
}

/// The frozen `#[allow(...)]` limit for `path`, zero when the file has none.
fn allow_limit(path: &str) -> usize {
    ALLOW_COUNTS
        .iter()
        .find(|(known, _)| *known == path)
        .map_or(0, |(_, limit)| *limit)
}

/// Count `#[allow(...)]` and `#![allow(...)]` attributes in `text`.
///
/// A line that starts with a `//` comment is skipped, and occurrences inside a
/// double-quoted string literal on the same line are not counted — a simple
/// quote scan is enough because a real attribute is always code.
fn allow_attribute_count(text: &str) -> usize {
    text.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .map(count_allow_attributes_on_line)
        .sum()
}

/// Count `#[allow(...)]` and `#![allow(...)]` occurrences on one line that are
/// outside a double-quoted string literal.
fn count_allow_attributes_on_line(line: &str) -> usize {
    let mut code = String::with_capacity(line.len());
    let mut in_string = false;
    let mut escaped = false;
    for ch in line.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            continue;
        }
        code.push(ch);
    }
    ["#[allow(", "#![allow("]
        .iter()
        .map(|needle| code.matches(needle).count())
        .sum()
}
