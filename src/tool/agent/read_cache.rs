// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Per-agent read state for `read_files`'s outline lever (token-optimizer
//! lever 1, backlog e4a50d22).
//!
//! A whole read of a big source file serves its signature outline instead of
//! the content: [`skeleton_for`] renders the file's signature lines, and the
//! model then reads the region it needs with `start_line`/`max_lines`.
//! [`ReadCache`] keeps, per file, the tokens that outline saved and that have
//! not yet been charged to follow-up ranged reads (`outline_followup`).
//!
//! The cache is per-agent: the factory builds a fresh one per tool
//! registry, never shared across subagents — a subagent's first read must
//! not inherit another agent's read state.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use super::read_files::numbered_line;
use crate::codegraph::walk::Lang;

/// Maximum number of files tracked per agent cache. When full, the whole
/// cache resets (rare — a fresh epoch is simpler and safer than partial
/// eviction: stale entries never silently outlive their usefulness).
pub(crate) const MAX_CACHED_FILES: usize = 64;

/// A skeleton serves at most this many numbered lines.
pub(crate) const SKELETON_MAX_LINES: usize = 120;

/// Per-agent cache of the outline debt owed to `read_files` ranged reads.
/// Interior-mutable; the tool clones the `Arc` into its `spawn_blocking`
/// closure.
pub struct ReadCache {
    /// Per-file tokens a served outline saved and that have not yet been
    /// charged to a follow-up ranged read (the `outline_followup` ledger
    /// row).
    outlined: RwLock<HashMap<PathBuf, i64>>,
}

impl Default for ReadCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self {
            outlined: RwLock::new(HashMap::new()),
        }
    }

    /// Note the tokens a served outline saved for `path`, to be charged to
    /// follow-up ranged reads (the `outline_followup` ledger row). Saturating,
    /// and resets the whole debt map once the entry cap is reached — a fresh
    /// epoch is simpler and safer than partial eviction.
    pub(crate) fn note_outline(&self, path: PathBuf, saved_tokens: i64) {
        let mut outlined = self.outlined.write().expect("read cache lock poisoned");
        if !outlined.contains_key(&path) && outlined.len() >= MAX_CACHED_FILES {
            outlined.clear();
        }
        let entry = outlined.entry(path).or_insert(0);
        *entry = entry.saturating_add(saved_tokens);
    }

    /// Charge `tokens` of follow-up ranged-read cost against what the outline
    /// for `path` saved. Returns the charge — `min(tokens, remaining)` — and
    /// reduces the remainder, or `None` when the path has no remainder left or
    /// `tokens` is non-positive (nothing to charge).
    pub(crate) fn charge_outline(&self, path: &Path, tokens: i64) -> Option<i64> {
        if tokens <= 0 {
            return None;
        }
        let mut outlined = self.outlined.write().expect("read cache lock poisoned");
        let remaining = outlined.get_mut(path)?;
        if *remaining <= 0 {
            return None;
        }
        let charged = tokens.min(*remaining);
        *remaining -= charged;
        Some(charged)
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
            body.push_str(&numbered_line(i + 1, line));
            body.push('\n');
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
        // HTML has no line-level signatures, so it is never outlined.
        Lang::Html => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_keeps_signatures_and_line_numbers() {
        let src = "//! doc\nuse std::fmt;\n\npub struct A { x: u32 }\n\nimpl A {\n    \
                   pub fn new() -> Self {\n        let b = 1 + 2;\n    }\n}\n\nfn helper() {}\n";
        let sk = skeleton_for(src, Lang::Rust);
        assert!(sk.body.starts_with("2: use std::fmt;\n"), "{}", sk.body);
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
    fn charge_outline_charges_up_to_what_the_outline_saved() {
        let cache = ReadCache::new();
        cache.note_outline(PathBuf::from("/x"), 100);
        assert_eq!(cache.charge_outline(Path::new("/x"), 60), Some(60));
        assert_eq!(cache.charge_outline(Path::new("/x"), 60), Some(40));
        // The debt is paid: nothing more is charged.
        assert_eq!(cache.charge_outline(Path::new("/x"), 60), None);
        // Non-positive charges and unknown paths are no-ops.
        assert_eq!(cache.charge_outline(Path::new("/x"), 0), None);
        assert_eq!(cache.charge_outline(Path::new("/never"), 10), None);
    }

    #[test]
    fn outline_debt_resets_at_cap() {
        let cache = ReadCache::new();
        cache.note_outline(PathBuf::from("/x"), 100);
        for i in 0..MAX_CACHED_FILES {
            cache.note_outline(PathBuf::from(format!("/p{i}")), 1);
        }
        // The cap reset cleared the whole debt map, /x included.
        assert_eq!(cache.charge_outline(Path::new("/x"), 10), None);
        // The entry that triggered the reset survives.
        assert_eq!(
            cache.charge_outline(Path::new(&format!("/p{}", MAX_CACHED_FILES - 1)), 10),
            Some(1)
        );
    }
}
