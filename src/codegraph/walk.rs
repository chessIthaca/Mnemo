// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Project file walker for CodeGraph — finds the files `search` would scan.
//!
//! Reuses the search tool's ignore rules ([`IGNORED_DIRS`] +
//! [`should_search`]) so the graph indexes exactly the files `search` sees:
//! build output, dependencies, and VCS metadata are skipped, as are files
//! outside the project root (fail closed) and files over 1 MB.
//!
//! Two walkers over that same tree:
//! - [`walk_searchable`] — EVERY searchable file (any extension), backing
//!   the FTS content index. Index ≡ walk coverage is what makes the
//!   `search` tool's index engine trustworthy: a literal query answered
//!   from a partial index would silently miss file types (docs, config, …).
//! - [`walk_project`] — the parseable source subset ([`Lang`]), for symbol
//!   extraction (tree-sitter grammars exist only for those).

use std::path::{Path, PathBuf};

use crate::tool::agent::search::should_search;

/// The source languages CodeGraph can parse — the major programming
/// languages with maintained tree-sitter grammars, so the graph's symbol
/// tools work on any common project, not just this repo's own Rust + TS —
/// plus HTML, whose `<script>` blocks are sub-parsed as JavaScript.
/// Languages outside this set are still covered by the finish gate's disk
/// check ([`is_code_extension`]) — they carry no symbol rows, but a
/// regression test written in them satisfies the gate by disk containment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    /// Rust (`.rs`) via `tree-sitter-rust`.
    Rust,
    /// TypeScript (`.ts`) via `tree-sitter-typescript`.
    Ts,
    /// TSX (`.tsx`) via `tree-sitter-typescript`'s TSX grammar.
    Tsx,
    /// JavaScript (`.js`/`.jsx`/`.mjs`/`.cjs`) via `tree-sitter-javascript`.
    Js,
    /// Python (`.py`/`.pyi`) via `tree-sitter-python`.
    Python,
    /// Go (`.go`) via `tree-sitter-go`.
    Go,
    /// Java (`.java`) via `tree-sitter-java`.
    Java,
    /// C (`.c`/`.h`) via `tree-sitter-c`.
    C,
    /// C++ (`.cpp`/`.cc`/`.cxx`/`.hpp`/`.hh`/`.hxx`) via `tree-sitter-cpp`.
    Cpp,
    /// C# (`.cs`) via `tree-sitter-c-sharp`.
    CSharp,
    /// Ruby (`.rb`) via `tree-sitter-ruby`.
    Ruby,
    /// PHP (`.php`) via `tree-sitter-php`.
    Php,
    /// HTML (`.html`/`.htm`) via `tree-sitter-html` — the file's `<script>`
    /// blocks are sub-parsed as JavaScript (see `visit_html`).
    Html,
}

/// (extension, [`Lang`]) pairs — the single source of truth for what the
/// graph can parse. `Lang::from_extension` and the `is_code_extension`
/// superset test both consume it, so the gate's disk check can never drift
/// from the parseable set again (review LOW-3, 2026-09-11: `.mts`/`.cts`
/// drifted past the sampled test).
const LANG_EXTENSIONS: &[(&str, Lang)] = &[
    ("rs", Lang::Rust),
    ("ts", Lang::Ts),
    ("mts", Lang::Ts),
    ("cts", Lang::Ts),
    ("tsx", Lang::Tsx),
    ("js", Lang::Js),
    ("jsx", Lang::Js),
    ("mjs", Lang::Js),
    ("cjs", Lang::Js),
    ("py", Lang::Python),
    ("pyi", Lang::Python),
    ("go", Lang::Go),
    ("java", Lang::Java),
    ("c", Lang::C),
    ("h", Lang::C),
    ("cpp", Lang::Cpp),
    ("cc", Lang::Cpp),
    ("cxx", Lang::Cpp),
    ("hpp", Lang::Cpp),
    ("hh", Lang::Cpp),
    ("hxx", Lang::Cpp),
    ("cs", Lang::CSharp),
    ("rb", Lang::Ruby),
    ("php", Lang::Php),
    ("html", Lang::Html),
    ("htm", Lang::Html),
];

impl Lang {
    /// Map a lowercase file extension (no dot) to a language, or `None` when
    /// the extension is not a parseable source file.
    pub fn from_extension(ext: &str) -> Option<Self> {
        LANG_EXTENSIONS
            .iter()
            .find(|(e, _)| *e == ext)
            .map(|(_, lang)| *lang)
    }
}

/// Extensions recognized as source files for the finish gate's disk check
/// ([`CodeGraph::reindex_files_containing`](crate::codegraph::CodeGraph::reindex_files_containing))
/// — a strict superset of [`Lang`]'s parseable set (pinned exhaustively by
/// `is_code_extension_is_a_superset_of_lang_extensions`). The gate must never
/// falsely error on a regression test written in a language the graph does
/// not parse (`.js`, `.py`, `.swift`, …), so every common source extension
/// is admitted: programming languages, plus web markup and style
/// (`.html`/`.css` and their preprocessor/component variants — a test can
/// live in a browser test page's script block). Pure data, config, and
/// documentation extensions are deliberately excluded: the plan file
/// itself records the test name, so admitting `.md`/`.json`/`.toml`
/// would let ANY recorded name satisfy the check and defeat the gate's
/// hallucination guard.
pub fn is_code_extension(ext: &str) -> bool {
    matches!(
        ext,
        // Lang-parseable (pinned against `LANG_EXTENSIONS` by the superset
        // test — the single source of truth).
        "rs" | "ts" | "mts" | "cts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "py" | "pyi"
        | "go" | "java" | "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "cs"
        | "rb" | "php" | "html" | "htm"
        // Languages with mature grammars that are not (yet) wired into Lang.
        | "swift" | "kt" | "kts" | "scala" | "sc" | "dart" | "zig" | "nim" | "cr" | "v"
        | "sv" | "vhdl" | "m" | "mm" | "f" | "f90" | "for" | "pas" | "asm" | "s"
        | "sh" | "bash" | "zsh" | "ksh" | "fish" | "ps1" | "psm1" | "bat" | "cmd"
        | "lua" | "pl" | "pm" | "r" | "jl" | "ex" | "exs" | "erl" | "hrl" | "hs"
        | "lhs" | "ml" | "mli" | "fs" | "fsi" | "fsx" | "clj" | "cljs" | "cljc"
        | "coffee" | "elm" | "purs" | "rkt" | "scm" | "ss" | "tcl" | "awk"
        | "sql" | "groovy" | "gradle" | "tf" | "vue" | "svelte" | "astro"
        | "proto" | "graphql"
        // Web style — markup-with-script (html/vue/svelte/astro) is
        // Lang-parseable above; style sheets round out the web set.
        | "css" | "scss" | "sass" | "less"
    )
}

/// Walk `root` recursively and return every searchable file — any
/// extension, exactly the set the `search` tool's walk engine scans —
/// sorted for deterministic indexing order.
///
/// Skips the same directories `search` skips (build output, deps, VCS
/// metadata) and applies the same per-file guards (must lie under `root`,
/// max 1 MB) by delegating to [`should_search`]. This is the coverage the
/// FTS content index must mirror so the `search` tool's index engine can
/// answer a literal query authoritatively (review C1: an index covering
/// only parseable source files silently reported "no matches" for content
/// living in docs/config/other file types).
pub fn walk_searchable(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    visit(root, root, &mut out, |p, r| should_search(p, r));
    out.sort();
    out
}

/// Walk `root` recursively and return every parseable source file (see
/// [`Lang`]), sorted for deterministic indexing order.
///
/// Skips the same directories the `search` tool skips (build output, deps,
/// VCS metadata) and applies the same per-file guards (must lie under `root`,
/// max 1 MB) by delegating to [`should_search`]. Symlinked directories are not
/// followed (only `read_dir` entries are visited), which keeps the walk bounded
/// to the real tree.
pub fn walk_project(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    visit(root, root, &mut out, is_source_file);
    out.sort();
    out
}

/// Recursive visitor shared by both walkers: skip ignored directories
/// wholesale, recurse otherwise, and keep files the per-file `accept` guard
/// admits.
fn visit(dir: &Path, root: &Path, out: &mut Vec<PathBuf>, accept: fn(&Path, &Path) -> bool) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return; // unreadable dir — skip silently; the graph is best-effort
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            // Skip ignored directories wholesale (target/, node_modules/, …)
            // instead of recursing into them just to reject every file.
            let name = entry.file_name().to_string_lossy().into_owned();
            if crate::tool::agent::search::is_ignored_component(&name) {
                continue;
            }
            visit(&path, root, out, accept);
        } else if file_type.is_file() && accept(&path, root) {
            out.push(path);
        }
    }
}

/// Whether `path` is a file CodeGraph should parse: it lies under `root`
/// (per [`should_search`], fail closed) and has a recognized extension.
fn is_source_file(path: &Path, root: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    Lang::from_extension(&ext.to_ascii_lowercase()).is_some() && should_search(path, root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Build a fixture tree and return its root:
    /// a.rs, main.py, page.html, sub/nested.tsx, sub/deep/b.ts — plus files
    /// that must be skipped.
    fn fixture(dir: &std::path::Path) {
        std::fs::write(dir.join("a.rs"), "fn a() {}").unwrap();
        std::fs::create_dir_all(dir.join("sub/deep")).unwrap();
        std::fs::write(dir.join("sub/nested.tsx"), "export const X = 1;").unwrap();
        std::fs::write(dir.join("sub/deep/b.ts"), "export function b() {}").unwrap();
        // Skipped: build output, deps, VCS, non-source, binary-ish extension.
        std::fs::create_dir_all(dir.join("target/debug")).unwrap();
        std::fs::write(dir.join("target/debug/dep.rs"), "fn dep() {}").unwrap();
        std::fs::create_dir_all(dir.join("node_modules/pkg")).unwrap();
        std::fs::write(dir.join("node_modules/pkg/x.ts"), "export const x = 1;").unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/y.rs"), "fn y() {}").unwrap();
        std::fs::write(dir.join("notes.md"), "# notes").unwrap();
        std::fs::write(dir.join("main.py"), "def main(): pass").unwrap();
        std::fs::write(
            dir.join("page.html"),
            "<html><script>function main() {}</script></html>",
        )
        .unwrap();
    }

    #[test]
    fn walks_source_files_sorted_and_skips_ignored() {
        let dir = tempdir().unwrap();
        fixture(dir.path());
        let files = walk_project(dir.path());
        let rel: Vec<String> = files
            .iter()
            .map(|p| {
                p.strip_prefix(dir.path())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(
            rel,
            vec!["a.rs", "main.py", "page.html", "sub/deep/b.ts", "sub/nested.tsx"],
            "must find exactly the source files, sorted, ignoring target/node_modules/.git"
        );
    }

    #[test]
    fn walks_every_searchable_file_for_the_content_index() {
        // Review C1 regression: the content index's coverage must match the
        // search tool's walk engine exactly — every text file, any extension
        // — or the index engine would silently miss docs/config/other types.
        let dir = tempdir().unwrap();
        fixture(dir.path());
        let files = walk_searchable(dir.path());
        let rel: Vec<String> = files
            .iter()
            .map(|p| {
                p.strip_prefix(dir.path())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(
            rel,
            vec![
                "a.rs",
                "main.py",
                "notes.md",
                "page.html",
                "sub/deep/b.ts",
                "sub/nested.tsx"
            ],
            "every searchable file regardless of extension, sorted; \
             target/node_modules/.git still skipped"
        );
    }

    #[test]
    fn lang_from_extension() {
        assert_eq!(Lang::from_extension("rs"), Some(Lang::Rust));
        assert_eq!(Lang::from_extension("ts"), Some(Lang::Ts));
        assert_eq!(Lang::from_extension("mts"), Some(Lang::Ts));
        assert_eq!(Lang::from_extension("cts"), Some(Lang::Ts));
        assert_eq!(Lang::from_extension("tsx"), Some(Lang::Tsx));
        assert_eq!(Lang::from_extension("TSX"), None, "caller lowercases first");
        assert_eq!(Lang::from_extension("js"), Some(Lang::Js));
        assert_eq!(Lang::from_extension("jsx"), Some(Lang::Js));
        assert_eq!(Lang::from_extension("mjs"), Some(Lang::Js));
        assert_eq!(Lang::from_extension("cjs"), Some(Lang::Js));
        assert_eq!(Lang::from_extension("py"), Some(Lang::Python));
        assert_eq!(Lang::from_extension("pyi"), Some(Lang::Python));
        assert_eq!(Lang::from_extension("go"), Some(Lang::Go));
        assert_eq!(Lang::from_extension("java"), Some(Lang::Java));
        assert_eq!(Lang::from_extension("c"), Some(Lang::C));
        assert_eq!(Lang::from_extension("h"), Some(Lang::C));
        assert_eq!(Lang::from_extension("cpp"), Some(Lang::Cpp));
        assert_eq!(Lang::from_extension("cc"), Some(Lang::Cpp));
        assert_eq!(Lang::from_extension("hpp"), Some(Lang::Cpp));
        assert_eq!(Lang::from_extension("cs"), Some(Lang::CSharp));
        assert_eq!(Lang::from_extension("rb"), Some(Lang::Ruby));
        assert_eq!(Lang::from_extension("php"), Some(Lang::Php));
        assert_eq!(
            Lang::from_extension("swift"),
            None,
            "gate-only: no grammar wired"
        );
        assert_eq!(
            Lang::from_extension("html"),
            Some(Lang::Html),
            "script blocks are sub-parsed as JavaScript"
        );
        assert_eq!(Lang::from_extension("htm"), Some(Lang::Html));
        assert_eq!(
            Lang::from_extension("css"),
            None,
            "gate-only: no grammar wired"
        );
        assert_eq!(Lang::from_extension("md"), None);
        assert_eq!(Lang::from_extension(""), None);
    }

    #[test]
    fn is_code_extension_covers_source_not_data() {
        // Programming languages (a sample across families).
        for ext in [
            "rs", "ts", "tsx", "js", "py", "go", "java", "c", "cpp", "cs", "rb", "php", "swift",
            "kt", "lua", "sh",
        ] {
            assert!(is_code_extension(ext), "{ext} is source");
        }
        // Web markup and style — browser test pages hold tests in script
        // blocks; component formats wrap script blocks.
        for ext in ["html", "htm", "css", "scss", "sass", "less", "vue", "svelte", "astro"] {
            assert!(is_code_extension(ext), "{ext} is source");
        }
        // Data, config, and docs never count — the plan file itself
        // records the test name, so admitting these would defeat the
        // hallucination guard.
        for ext in ["md", "txt", "json", "toml", "yaml", "yml", "xml", "csv", "lock", ""] {
            assert!(!is_code_extension(ext), "{ext} is not source");
        }
    }

    #[test]
    fn is_code_extension_is_a_superset_of_lang_extensions() {
        // LOW-3 (review 2026-09-11): the gate's disk check must see every
        // extension the graph can parse — a drift re-creates the original
        // bug for the drifted extension (.mts/.cts did exactly that).
        // Iterating the single-source-of-truth table makes this exhaustive:
        // a new Lang extension that forgets is_code_extension fails here.
        for (ext, _) in LANG_EXTENSIONS {
            assert!(Lang::from_extension(ext).is_some(), "{ext} must map to a Lang");
            assert!(
                is_code_extension(ext),
                "{ext} is Lang-parseable, so the gate's disk check must scan it"
            );
        }
    }

    #[test]
    fn is_source_file_fails_closed_outside_root() {
        let dir = tempdir().unwrap();
        let other = tempdir().unwrap();
        std::fs::write(other.path().join("outside.rs"), "fn o() {}").unwrap();
        // A file that exists but lives outside the root must be rejected
        // (defense-in-depth, mirroring `should_search`).
        assert!(!is_source_file(
            &other.path().join("outside.rs"),
            dir.path()
        ));
        std::fs::write(dir.path().join("inside.rs"), "fn i() {}").unwrap();
        assert!(is_source_file(&dir.path().join("inside.rs"), dir.path()));
    }
}
