// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]
//! Mnemo — a coding-first agentic harness in Rust.
//!
//! See `PLAN.md` for the full design. The library is organized so that the
//! kernel "brain" (config, project, provider, tool, memory, runtime, agent)
//! plus the app-side workflow graph (`app::workflow` — the plan-first state
//! machine, job 2b) are fully usable without the TUI.

pub mod agent;
pub mod app;
#[cfg(feature = "browser")]
pub mod browser;
pub mod codegraph;
pub mod config;
pub mod error;
pub mod host;
pub mod index_staleness;
pub mod instance_marker;
pub mod mcp;
pub mod memory;
pub mod model_resolver;
pub mod project;
pub mod provider;
pub mod runtime;
pub mod safety_rules;
pub mod shell_path;
pub mod thread_util;
pub mod tool;
pub mod webview_args;

#[cfg(test)]
mod boundary {
    use std::fs;
    use std::path::{Path, PathBuf};

    /// The banned tokens (master-spec :255-258): a kernel source that carries
    /// any of them in code — directly (`crate::app`, `tool::workflow`) or via
    /// the retired crate-root aliases (`crate::workflow`, `crate::backlog`,
    /// `crate::skill`) — imports the app's modules and breaks the
    /// single-crate boundary this test enforces, so the later `mnemo-kernel`
    /// crate split stays mechanical. Test code is exempt (the harness builds
    /// the real artifacts and may reference the app's types); comments are
    /// exempt (the token rule is code-only).
    ///
    /// Covered spellings are exactly the spec's five: the crate-root paths,
    /// the direct app path, and the tool-tree re-export. Deliberately NOT
    /// covered (the spec :255-258 contract): `super::app`, `mnemo::app` (the
    /// crate's own external name), and crate-alias indirections
    /// (`use crate as x; x::app::…`) — none exist in the kernel tree (swept
    /// 2026-10-12); the later `mnemo-kernel` crate split makes the alias
    /// route structurally impossible.
    const BANNED_TOKENS: [&str; 5] = [
        "crate::workflow",
        "crate::backlog",
        "crate::skill",
        "crate::app",
        "tool::workflow",
    ];

    /// Whole-file `#[cfg(test)]` modules: files whose ONLY reason to exist is
    /// a `#[cfg(test)] mod <name>;` declaration in their parent — the
    /// attribute lives in the PARENT, so `strip_test_items` never fires on
    /// their content. Named explicitly, and fail-loud by design: adding a
    /// new whole-file test module trips the scan until the name is added
    /// here deliberately.
    const WHOLE_FILE_TEST_MODULES: &[&str] = &["agent/tests.rs"];

    /// Collect every `.rs` file under `src/`, excluding the app subtree —
    /// the relocated modules (job 2b) whose consumption this test guards the
    /// kernel side of.
    fn kernel_sources(root: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(root).expect("src/ readable") {
            let path = entry.expect("dir entry").path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if path.is_dir() {
                if name == "app" {
                    continue;
                }
                kernel_sources(&path, out);
            } else if name.ends_with(".rs") {
                out.push(path);
            }
        }
    }    /// Strip `#[cfg(test)]`-attributed items from a source (the 2b.0
    /// point-6 treatment): test code is the test harness — it builds the
    /// real artifacts and may reference the app's types freely — so its
    /// bytes never reach the token scan. Mechanical: the attribute opens a
    /// skip that consumes the decorated item up to its balanced closing
    /// brace, or its `;` for braceless items (`use`/`const`). The brace
    /// counter is literal-aware: string literals (`"…"`), raw strings
    /// (`r"…"`, `r#"…"#`, any hash count), and char literals (`'{'`) never
    /// contribute depth, and lifetimes (`'a`) are discriminated from char
    /// literals by lookahead and ignored; line comments are skipped to
    /// end-of-line. Known limits, inert in the kernel tree (swept
    /// 2026-10-12): multi-line string literals inside a stripped item are
    /// scanned as code (their content could skew depth), and cfg-predicate
    /// variants such as `#[cfg(all(test, …))]` are NOT stripped (exact
    /// prefix only — a future one surfaces its tokens to the scan, the
    /// loud direction).
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
                // A `;`-terminated line that is not a comment closes the item
                // hunt: either the attribute line itself carried a braceless
                // item (`#[cfg(test)] use x;`), or this IS the braceless item
                // (`use x;`). Doc comments between the attribute and the item
                // (`/// see foo();`) must NOT close it.
                if !opened && t.trim_end().ends_with(';') && !t.trim_start().starts_with("//") {
                    break;
                }
                let cs: Vec<char> = t.chars().collect();
                let mut j = 0;
                while j < cs.len() {
                    let c = cs[j];
                    if c == '"' {
                        // A regular string literal — skip to its close
                        // (escape-aware).
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
                        // count) — skip to the close (`"` + the same hash
                        // count).
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
                        // `r#ident` (a raw identifier) — falls through as a
                        // plain char; braces cannot hide in it.
                    }
                    if c == '\'' {
                        // `'…'` (a char literal, ≤4 content chars with
                        // escapes) vs `'a` (a lifetime): lookahead for the
                        // closing quote.
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
                            j = k + 1; // a char literal — braces inside never count
                            continue;
                        }
                        // a lifetime (`'a`) — falls through as a plain char
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
    }    /// The kernel never imports the app (master-spec :255-258). Scans every
    /// kernel module source under `src/` — the app subtree excluded,
    /// `#[cfg(test)]` items stripped, comment/doc lines skipped (the token
    /// rule is code-only) — and fails naming file + line for every hit of a
    /// banned token.
    #[test]
    fn kernel_does_not_import_app() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = Vec::new();
        kernel_sources(&root, &mut sources);
        assert!(!sources.is_empty(), "the walk found no sources under {root:?}");

        let mut offenders = Vec::new();
        for path in &sources {
            let rel = path
                .strip_prefix(&root)
                .expect("the walk starts at src/")
                .to_str()
                .unwrap_or("")
                .replace('\\', "/");
            if WHOLE_FILE_TEST_MODULES.contains(&rel.as_str()) {
                continue; // a whole-file cfg(test) module — the attribute is in the parent
            }
            let raw = fs::read_to_string(path).expect("source readable");
            let stripped = strip_test_items(&raw);
            for line in stripped.lines() {
                let t = line.trim_start();
                if t.starts_with("//") {
                    continue; // comment/doc lines are code-free by the token rule
                }
                for token in BANNED_TOKENS {
                    if line.contains(token) {
                        offenders.push(format!(
                            "{}: {}",
                            path.strip_prefix(&root).unwrap_or(path).display(),
                            line.trim()
                        ));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "kernel sources import the app (master-spec :255-258) — the tokens are banned; the \
             app's surface goes through the handle/policy seams:\n{}",
            offenders.join("\n")
        );
    }
}