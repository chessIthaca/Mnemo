// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Platform-gating guard for `src/watchdog.rs`.
//!
//! The macOS CI leg compiles this crate with none of the Windows-only
//! DbgHelp/threading types in scope, so a top-level item that references one
//! without a `#[cfg(windows)]` gate is a hard E0425 there while Windows stays
//! green. Regression: `resolve_frame` lost its gate when its doc comment was
//! separated from the function and the stray `#[cfg(windows)]` attached to
//! `module_name_for` instead — the macOS leg of run 35514715816 (tag v0.1.1)
//! failed `cargo test --workspace` with exit 101.

/// Windows-only identifiers: the DbgHelp/threading surface plus the gated
/// helper names, so ungated *callers* of gated helpers are caught too.
const WINDOWS_ONLY: &[&str] = &[
    "HANDLE",
    "SYMBOL_INFO",
    "IMAGEHLP_LINE64",
    "STACKFRAME64",
    "SymFromAddr",
    "SymGetLineFromAddr64",
    "SymInitialize",
    "SymGetModuleBase64",
    "GetModuleFileNameW",
    "StackWalk64",
    "OpenThread",
    "SuspendThread",
    "CloseHandle",
    "GetThreadContext",
    "GetCurrentProcess",
    "resolve_frame",
    "module_name_for",
    "ensure_symbols_ready",
    "capture_thread_stack_once",
];

/// True when the line opens a top-level item the guard scans.
fn opens_item(line: &str) -> bool {
    line.starts_with("fn ")
        || line.starts_with("pub fn ")
        || line.starts_with("pub(crate) fn ")
        || line.starts_with("unsafe fn ")
        || line.starts_with("struct ")
        || line.starts_with("pub struct ")
        || line.starts_with("static ")
        || line.starts_with("const ")
        || line.starts_with("pub const ")
        || line.starts_with("type ")
        || line.starts_with("trait ")
        || line.starts_with("enum ")
        || line.starts_with("mod ")
        || line.starts_with("use ")
        || line.starts_with("impl ")
}

#[test]
fn watchdog_windows_only_items_stay_cfg_gated() {
    let text = include_str!("../src/watchdog.rs");
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        if !opens_item(line) {
            i += 1;
            continue;
        }
        // One-line items (`struct S(...);`, `const X: T = v;`) end at the
        // semicolon; block items run to the next column-0 `}` or `};`
        // (the latter is how multi-line `use` blocks close).
        let end = if line.ends_with(';') {
            i
        } else {
            let mut j = i + 1;
            while j < lines.len() && lines[j] != "}" && lines[j] != "};" {
                j += 1;
            }
            j
        };
        // Comment lines are stripped so doc-comment mentions never trip the
        // guard — only real code references count.
        let mut code = String::new();
        for l in &lines[i..=end.min(lines.len() - 1)] {
            if !l.trim_start().starts_with("//") {
                code.push_str(l);
                code.push('\n');
            }
        }
        if WINDOWS_ONLY.iter().any(|id| code.contains(id)) {
            let window = &lines[i.saturating_sub(10)..i];
            assert!(
                window.iter().any(|l| l.trim() == "#[cfg(windows)]"),
                "watchdog.rs line {} (`{}`) references Windows-only identifiers \
                 but carries no #[cfg(windows)] gate — the macOS build fails \
                 with E0425 while Windows stays green",
                i + 1,
                line
            );
        }
        i = end + 1;
    }
}
