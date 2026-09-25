// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Per-agent read cache for `read_files` delta + skeleton re-reads
//! (token-optimizer lever 1, backlog e4a50d22).
//!
//! Most whole-file reads are RE-reads of files already in context: the
//! agent re-opens a file after an edit, or reads again what it read a few
//! turns ago. Serving the full content again burns the same context twice.
//! The cache keeps the last-served full content per path, so a re-read can
//! serve a signature skeleton (unchanged file) or a unified diff (changed
//! file) instead — with a clear note pointing back at the full-read escape
//! hatch (`start_line`/`max_lines` always serve exact lines).
//!
//! The cache is per-agent: the factory builds a fresh one per tool
//! registry, never shared across subagents — a subagent's first read must
//! not inherit a false "already in context" state.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::codegraph::walk::Lang;

/// Maximum number of files tracked per agent cache. When full, the whole
/// cache resets (rare — a fresh epoch is simpler and safer than partial
/// eviction: stale entries never silently outlive their usefulness).
pub(crate) const MAX_CACHED_FILES: usize = 64;

/// A skeleton serves at most this many numbered lines.
pub(crate) const SKELETON_MAX_LINES: usize = 120;

/// Serve the full file (not a diff) once the diff would exceed this
/// fraction of the new content — a mostly-rewritten file is cheaper to
/// re-serve in full than to make the model decode a huge diff.
const DIFF_FULL_SERVE_RATIO: f64 = 0.4;

/// The last-served full content of one file.
struct FileSnapshot {
    content: String,
}

/// Per-agent cache of last-served file contents (the delta baseline for
/// `read_files` re-reads). Interior-mutable; the tool clones the `Arc`
/// into its `spawn_blocking` closure.
pub struct ReadCache {
    inner: RwLock<HashMap<PathBuf, FileSnapshot>>,
}

impl ReadCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// The last-served full content for `path`, when this agent already
    /// read the file whole.
    pub(crate) fn snapshot(&self, path: &Path) -> Option<String> {
        self.inner
            .read()
            .expect("read cache lock poisoned")
            .get(path)
            .map(|s| s.content.clone())
    }

    /// Record the full content served for `path` (the delta baseline for
    /// the next re-read). Resets the whole cache once the entry cap is
    /// reached.
    pub(crate) fn record(&self, path: PathBuf, content: String) {
        let mut inner = self.inner.write().expect("read cache lock poisoned");
        if !inner.contains_key(&path) && inner.len() >= MAX_CACHED_FILES {
            inner.clear();
        }
        inner.insert(path, FileSnapshot { content });
    }
}

/// A skeleton render result: the numbered signature lines, how many were
/// kept, and whether the cap cut the skeleton short.
pub(crate) struct Skeleton {
    /// The numbered signature lines (each `"{n}: {line}"`), newline-ended.
    pub(crate) body: String,
    /// How many signature lines were kept.
    pub(crate) kept: usize,
    /// Whether [`SKELETON_MAX_LINES`] cut the skeleton short.
    pub(crate) capped: bool,
}

/// Render the signature/import skeleton for `content` in `lang`: every
/// structural line with its ORIGINAL line number (so the agent can jump
/// to any region with `start_line`), capped at [`SKELETON_MAX_LINES`].
pub(crate) fn skeleton_for(content: &str, lang: Lang) -> Skeleton {
    let mut body = String::new();
    let mut kept = 0usize;
    let mut capped = false;
    for (i, line) in content.lines().enumerate() {
        if kept >= SKELETON_MAX_LINES {
            capped = true;
            break;
        }
        if is_signature_line(lang, line) {
            kept += 1;
            body.push_str(&format!("{:>4}: {line}\n", i + 1));
        }
    }
    Skeleton { body, kept, capped }
}

/// Whether `line` is a structural line for `lang` — an import, a type or
/// function signature: a header that tells the model WHAT a file contains
/// without the bodies. Line-based by design: cheap, deterministic,
/// testable; tree-sitter-grade precision is the code graph's job (and the
/// SYMBOL NUDGE already points there).
fn is_signature_line(lang: Lang, line: &str) -> bool {
    let t = line.trim_start();
    let starts = |pats: &[&str]| pats.iter().any(|p| t.starts_with(p));
    match lang {
        Lang::Rust => starts(&[
            "pub fn", "fn ", "pub(crate) fn", "pub(super) fn", "async fn", "pub async fn",
            "unsafe fn", "pub unsafe fn", "pub struct", "struct ", "pub enum", "enum ",
            "pub trait", "trait ", "impl ", "pub mod", "mod ", "use ", "pub use", "const ",
            "pub const", "static ", "pub static", "type ", "pub type", "macro_rules!",
            "extern crate",
        ]),
        Lang::Ts | Lang::Tsx | Lang::Js => starts(&[
            "export ", "import ", "function ", "async function", "class ", "abstract class",
            "interface ", "enum ", "type ", "namespace ", "declare ",
        ]),
        Lang::Python => starts(&["def ", "async def", "class ", "import ", "from "]),
        Lang::Go => starts(&[
            "func ", "type ", "package ", "import ", "var ", "const ",
        ]),
        Lang::Java => starts(&[
            "public ", "private ", "protected ", "class ", "interface ", "enum ", "record ",
            "import ", "package ", "static ", "final ", "abstract ",
        ]),
        Lang::CSharp => starts(&[
            "public ", "private ", "protected ", "internal ", "static ", "class ", "interface ",
            "enum ", "struct ", "record ", "namespace ", "using ",
        ]),
        Lang::C => starts(&[
            "#include", "#define", "typedef ", "struct ", "enum ", "union ", "extern ",
        ]),
        Lang::Cpp => starts(&[
            "#include", "#define", "typedef ", "struct ", "enum ", "union ", "extern ",
            "class ", "template", "namespace ", "using ",
        ]),
        Lang::Ruby => starts(&["def ", "class ", "module ", "require "]),
        Lang::Php => starts(&[
            "function ", "class ", "interface ", "trait ", "enum ", "use ", "namespace ",
        ]),
        // HTML has no line-level signatures — a skeleton would be noise.
        // Whole-file HTML re-reads serve the full content (the baseline is
        // still cached, so DIFF serves work).
        Lang::Html => false,
    }
}

/// A unified diff of `old` → `new` (the `similar` crate), or `None` when
/// the diff is empty or too large to be worth serving — the caller then
/// serves the full new content (a mostly-rewritten file is cheaper to
/// re-read than to decode through a huge diff).
pub(crate) fn diff_if_small(old: &str, new: &str) -> Option<String> {
    let diff = similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header("last read", "current")
        .to_string();
    if diff.is_empty() {
        return None;
    }
    if diff.chars().count() as f64 > DIFF_FULL_SERVE_RATIO * (new.chars().count().max(1)) as f64 {
        return None;
    }
    Some(diff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_keeps_signatures_and_line_numbers() {
        let src = "//! doc\nuse std::fmt;\n\npub struct A { x: u32 }\n\nimpl A {\n    \
                   pub fn new() -> Self {\n        let b = 1 + 2;\n    }\n}\n\nfn helper() {}\n";
        let sk = skeleton_for(src, Lang::Rust);
        assert!(sk.body.contains("   2: use std::fmt;"), "{}", sk.body);
        assert!(sk.body.contains("pub struct A { x: u32 }"));
        assert!(sk.body.contains("impl A {"));
        assert!(sk.body.contains("fn helper() {}"));
        assert!(sk.body.contains("pub fn new() -> Self {"));
        assert!(!sk.body.contains("let b = 1 + 2;"));
        assert_eq!(sk.kept, 5); // use + struct + impl + pub fn + fn helper
        assert!(!sk.capped);
    }

    #[test]
    fn skeleton_caps_at_max_lines() {
        let src: String = (0..SKELETON_MAX_LINES + 20)
            .map(|i| format!("fn f_{i}() {{\n    body_{i}\n}}\n"))
            .collect();
        let sk = skeleton_for(&src, Lang::Rust);
        assert!(sk.capped);
        assert_eq!(sk.kept, SKELETON_MAX_LINES);
    }

    #[test]
    fn skeleton_knows_python_and_ts() {
        let py = skeleton_for("import os\n\ndef main():\n    return 1\n", Lang::Python);
        assert!(py.body.contains("import os"));
        assert!(py.body.contains("def main():"));
        assert!(!py.body.contains("return 1"));
        let ts = skeleton_for(
            "import { x } from 'y';\nexport function main(): void {\n    return;\n}\n",
            Lang::Ts,
        );
        assert!(ts.body.contains("import { x } from 'y';"));
        assert!(ts.body.contains("export function main(): void {"));
        assert!(!ts.body.contains("return;"));
    }

    #[test]
    fn diff_small_change_is_served() {
        // A one-line change inside a file large enough that the diff
        // (headers + hunk + context included) is well under the 40%
        // full-serve ratio — a TINY file's diff overhead exceeds the file
        // itself, and that case must fall back to a full serve.
        let old: String = (0..30)
            .map(|i| format!("line {i} unchanged content\n"))
            .collect();
        let new = old.replace("line 15 unchanged content", "line 15 CHANGED");
        let diff = diff_if_small(&old, &new).unwrap();
        assert!(diff.contains("-line 15 unchanged content"), "{diff}");
        assert!(diff.contains("+line 15 CHANGED"));
    }

    #[test]
    fn diff_mostly_rewritten_returns_none() {
        let old = "a\n".repeat(200);
        let new = "b\n".repeat(200);
        assert!(diff_if_small(&old, &new).is_none());
    }

    #[test]
    fn cache_round_trips_and_resets_at_cap() {
        let cache = ReadCache::new();
        assert!(cache.snapshot(Path::new("/x")).is_none());
        cache.record(PathBuf::from("/x"), "content".into());
        assert_eq!(cache.snapshot(Path::new("/x")).as_deref(), Some("content"));
        for i in 0..MAX_CACHED_FILES {
            cache.record(PathBuf::from(format!("/p{i}")), "c".into());
        }
        // The cap reset cleared the whole cache, /x included.
        assert!(cache.snapshot(Path::new("/x")).is_none());
    }
}