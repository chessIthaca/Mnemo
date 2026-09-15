// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `search` — grep + glob tools for finding files and content.
//!
//! Literal queries are served by the CodeGraph FTS content index when it is
//! populated (`engine: index` in the output); everything else — regex
//! patterns, multi-line or punctuation-only literals (no FTS tokens), and
//! broken-regex fallbacks (the walk engine keeps "matched literally"
//! semantics exact) — walks the project tree (skipping build artifacts +
//! deps) and matches line-by-line (`engine: walk`). Auto-run (read-only).
//!
//! AUTO-DELEGATION (backlog b804012f): when the pattern names an indexed
//! symbol — a bare identifier, a definition-prefixed form (`fn X`), or a
//! symbol-INTENT shape (`X(`, `a::b`, "who calls X") — the graph answers
//! INLINE: a compact block (definition, top callers/callees, the
//! graph_context pointer) rides above the output and the file walk is
//! skipped entirely. A query targeting semantic memory — a glob under
//! `.coding/knowledge`/`.coding/reviews` or a typed prefix (SPEC:/DECISION:/
//! BUG:/PLAN:/HOW:/REVIEW:) — gets the memory_search recall answer inline
//! the same way. The block opens with "AUTO-DELEGATED to …" and carries the
//! escape line: re-issuing the SAME query (pattern+glob+literal) skips
//! delegation and runs the plain file search (sticky per exact query — a
//! bounded set of bypassed keys; interleaved delegating queries never
//! break an earlier escape). Best-effort: an unindexed symbol, an empty
//! recall, or any store hiccup falls through to the normal search.
//!
//! SYMBOL NUDGE (the advisory fallback): on the escape repeat — and for
//! alternation hunts whose branches resolve only fuzzily — the prepended
//! note still points at graph_search/graph_context; symbol lookups belong
//! to the graph tools, grep is for text. The nudge is advisory (one indexed
//! SELECT, best-effort) and never changes the result rows.
//!
//! LITERAL TIP: a regex-mode walk whose pattern has no regex metacharacters
//! could have been served exactly by the content index (`literal:true`) —
//! the note suggests that for next time. Advisory only, never auto-routed
//! (an FTS phrase and a regex are not semantically equivalent), and
//! suppressed when the symbol nudge fires (symbol steering wins — one
//! note, not two).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::provider::ToolSchema;
use crate::tool::agent::pattern;
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// Directory names that are never searched — build output, dependencies, or
/// VCS metadata. Searching these is slow and almost never what the user wants.
const IGNORED_DIRS: &[&str] = &[
    "target",       // Rust build output
    "node_modules", // JS dependencies
    ".git",         // VCS metadata
    ".next",        // Next.js build
    ".nuxt",        // Nuxt build
    "dist",         // generic build output
    "build",        // generic build output
    ".cache",       // caches
    "__pycache__",  // Python bytecode
    ".venv",        // Python venv
    "venv",         // Python venv
    ".idea",        // JetBrains
    ".vscode",      // VS Code (keep config, skip nothing — but skip anyway)
    ".claude",      // Claude Code's own bookkeeping (plans, settings) — not project source
];

/// Whether a path component is an ignored directory.
pub(crate) fn is_ignored_component(name: &str) -> bool {
    IGNORED_DIRS.contains(&name)
}

/// Whether a file should be searched: skip ignored dirs, the app's own data
/// stores under `.coding/`, and files that are likely huge.
///
/// Fails CLOSED: if `path` does not lie under `root` (strip_prefix fails), the
/// file is NOT searched. This is defense-in-depth against a glob that escapes
/// the sandbox root — an escaped path must never be read, even if the upstream
/// glob validation is somehow bypassed.
pub(crate) fn should_search(path: &Path, root: &Path) -> bool {
    // Skip any path that passes through an ignored directory. If the path is
    // not under root at all, fail closed (return false) — never search a file
    // outside the sandbox.
    let rel = match path.strip_prefix(root) {
        Ok(r) => r,
        Err(_) => return false,
    };
    for component in rel.components() {
        if let std::path::Component::Normal(name) = component {
            if is_ignored_component(&name.to_string_lossy()) {
                return false;
            }
        }
    }
    // Never scan the app's own SQLite stores under `.coding/` (codegraph +
    // memory DBs and their journal/WAL sidecars): they are binary caches
    // whose contents are meaningless as search results, and indexing the
    // graph DB into itself is self-referential (the DB changes during every
    // pass, so it would re-index forever). read_to_string only skips them
    // opportunistically (when a page happens to be non-UTF-8) — excluded
    // deterministically here so the walk engine and the FTS content index
    // stay in exact agreement.
    if rel.components().next().and_then(|c| c.as_os_str().to_str()) == Some(".coding") {
        const DATA_FILES: &[&str] = &[
            "codegraph.db",
            "codegraph.db-wal",
            "codegraph.db-shm",
            "codegraph.db-journal",
            "memory.db",
            "memory.db-wal",
            "memory.db-shm",
            "memory.db-journal",
        ];
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if DATA_FILES.contains(&name) {
                return false;
            }
        }
    }
    // Skip files over 1 MB — likely minified bundles or data files.
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > 1_000_000 {
            return false;
        }
    }
    true
}

/// Validate a user-supplied glob pattern so it cannot escape the sandbox root.
///
/// Rejects absolute paths (`/...`), Windows drive prefixes (`C:...`), UNC
/// paths (`\\...`), and any `..` path component (on both `/` and `\`). A glob
/// that escapes the root would let `search`/`search_read` read files outside
/// the project — this is the primary guard; [`should_search`] + the
/// `starts_with(root)` check in the glob loop are defense-in-depth.
pub(crate) fn validate_glob(glob: &str) -> Result<(), String> {
    // Absolute POSIX path or a Windows drive prefix.
    if glob.starts_with('/') || glob.starts_with('\\') {
        return Err(format!(
            "glob must be relative to the project root (got absolute path: {glob:?})"
        ));
    }
    // Windows drive prefix (e.g. `C:\...` or `C:/...`).
    let bytes = glob.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Err(format!(
            "glob must be relative to the project root (got drive path: {glob:?})"
        ));
    }
    // UNC path (`\\server\share\...`).
    if glob.starts_with("\\\\") {
        return Err(format!(
            "glob must be relative to the project root (got UNC path: {glob:?})"
        ));
    }
    // Any `..` component (on either separator) — reject traversal.
    for component in glob.split(['/', '\\']) {
        if component == ".." {
            return Err(format!(
                "glob must not contain a parent-directory (`..`) component: {glob:?}"
            ));
        }
    }
    Ok(())
}

/// Expand brace alternation (`**/*.{ts,tsx}` → two concrete globs). The
/// glob crate has no alternation — `{…}` would otherwise match only paths
/// containing literal braces, i.e. nothing, and the tool would silently
/// answer "searched 0 files" (F7, diagnosed 2026-09-17 by probe: the crate
/// DOES match bare `**` tails; braces are the unsupported shape). Nesting
/// expands recursively; the total expansion is capped so a pathological
/// pattern cannot explode; an unbalanced `{` stays literal.
fn expand_braces(pattern: &str) -> Vec<String> {
    const MAX_BRACE_EXPANSIONS: usize = 32;
    let mut out = vec![String::new()];
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '{' {
            for s in out.iter_mut() {
                s.push(c);
            }
            continue;
        }
        // Collect the alternation body up to the matching '}'.
        let mut depth = 1usize;
        let mut body = String::new();
        for inner in chars.by_ref() {
            match inner {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            body.push(inner);
        }
        if depth != 0 {
            // Unbalanced '{' — keep it literal; the compiled pattern
            // behaves exactly as before this helper existed.
            for s in out.iter_mut() {
                s.push('{');
                s.push_str(&body);
            }
            continue;
        }
        let mut next: Vec<String> = Vec::new();
        for alt in split_top_level(&body) {
            for expanded in expand_braces(&alt) {
                for prefix in out.iter() {
                    if next.len() >= MAX_BRACE_EXPANSIONS {
                        break;
                    }
                    next.push(format!("{prefix}{expanded}"));
                }
            }
        }
        if !next.is_empty() {
            out = next;
        }
    }
    out
}

/// Split an alternation body on top-level commas (commas inside nested
/// braces belong to the inner alternation).
fn split_top_level(body: &str) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut depth = 0usize;
    for c in body.chars() {
        match c {
            '{' => {
                depth += 1;
                parts.last_mut().expect("parts never empty").push(c);
            }
            '}' => {
                depth = depth.saturating_sub(1);
                parts.last_mut().expect("parts never empty").push(c);
            }
            ',' if depth == 0 => parts.push(String::new()),
            _ => parts.last_mut().expect("parts never empty").push(c),
        }
    }
    parts
}

/// Compile a (possibly brace-containing) glob into concrete patterns —
/// brace alternation is expanded ([`expand_braces`]) and each alternative
/// compiled. A path matches when ANY alternative matches. An invalid
/// alternative invalidates the whole glob (the single-pattern contract).
fn compile_globs(glob: Option<&str>) -> std::io::Result<Vec<glob::Pattern>> {
    expand_braces(glob.unwrap_or("**/*"))
        .into_iter()
        .map(|p| glob::Pattern::new(&p).map_err(std::io::Error::other))
        .collect()
}

/// Walk the project tree PRUNED: never descend into ignored directories
/// (their children are never stat'd — the old glob-everything-then-skip
/// walk enumerated ~107k files under node_modules/target/.git/dist only to
/// reject each one), sort each directory's entries for deterministic
/// glob-crate-equivalent order, and keep files that pass the (already
/// validated) `glob` filter AND [`should_search`] (kept as
/// defense-in-depth for files inside non-ignored dirs — e.g. the app's
/// own data stores under `.coding/` and files over 1 MB).
///
/// Returns `(files, pruned_dirs)`; `pruned_dirs` counts ignored
/// DIRECTORIES cut wholesale — a semantic change from the old walk's
/// per-skipped-FILE count (enumerating those files to count them is
/// exactly the waste this prunes). Only an unreadable ROOT is an `Err`
/// (callers treat that as an empty walk, matching the old glob walk's
/// silent-empty behavior); unreadable subdirectories are skipped
/// silently, as before. Symlinked directories are not followed (bounded
/// to the real tree); symlinked files are still searched, as the glob
/// walk did.
pub(crate) fn walk_searchable(
    root: &Path,
    glob: Option<&str>,
) -> std::io::Result<(Vec<PathBuf>, u64)> {
    let patterns = compile_globs(glob)?;
    // The root itself must be readable — otherwise the walk is genuinely
    // empty (propagated as Err for callers that care).
    std::fs::read_dir(root)?;
    let mut files = Vec::new();
    let mut pruned_dirs = 0u64;
    descend(root, root, &patterns, &mut files, &mut pruned_dirs);
    Ok((files, pruned_dirs))
}

/// Depth-first visitor behind [`walk_searchable`]: sorted entries per
/// directory, ignored directories cut (and counted), files filtered
/// through the glob (project-relative, `/`-separated — the same shape
/// `try_index` filters index hits by, so the walk and index engines
/// scope identically) and [`should_search`].
fn descend(
    dir: &Path,
    root: &Path,
    patterns: &[glob::Pattern],
    files: &mut Vec<PathBuf>,
    pruned: &mut u64,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return; // unreadable subdir — skip silently (old walk parity)
    };
    let mut sorted: Vec<_> = entries.flatten().collect();
    sorted.sort_by_key(|e| e.file_name());
    for entry in sorted {
        let path = entry.path();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if is_ignored_component(&entry.file_name().to_string_lossy()) {
                *pruned += 1;
                continue; // never stat inside ignored dirs — the whole point
            }
            descend(&path, root, patterns, files, pruned);
        } else if path.is_file() {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let rel = rel.to_string_lossy().replace('\\', "/");
            if patterns.iter().any(|p| p.matches_path(Path::new(&rel)))
                && should_search(&path, root)
            {
                files.push(path);
            }
        }
    }
}

/// Maximum number of matches returned. If a search produces more, the results
/// are capped and a note is appended advising the caller to narrow the
/// pattern or use a glob filter. This prevents unbounded conversation bloat
/// from broad searches.
const MAX_MATCHES: usize = 100;

/// Maximum number of stale files re-indexed INLINE inside a search tool
/// call (F10): a small staleness — the watcher's reindex lagging one or two
/// edits — is repaired on the spot ([`CodeGraph::reindex_stale_files`]) and
/// the query re-served from the fresh index. Above the cap (a checkout-scale
/// staleness), or while an index pass is running, the tree walk stays the
/// answer: parsing dozens of files inside a tool call would blow the call's
/// latency budget, and the walk is authoritative anyway.
const STALE_REINDEX_CAP: usize = 8;

/// Maximum number of bypassed delegation keys retained. Once a query has
/// delegated, its identical re-issues never re-delegate in-session; the set
/// is bounded so a long session cannot grow it unboundedly (oldest keys are
/// evicted — a re-delegation after eviction is harmless: the escape hatch
/// just re-arms).
pub(crate) const DELEGATION_SET_CAP: usize = 32;

/// Arguments for `search`.
#[derive(Debug, Deserialize)]
struct SearchArgs {
    /// A regex pattern to search for in file contents.
    pattern: String,
    /// A glob pattern to limit which files to search (e.g. "**/*.rs").
    #[serde(default)]
    glob: Option<String>,
    /// Whether to do a literal (non-regex) search.
    #[serde(default)]
    literal: bool,
}

/// The identity of a delegated query — pattern + glob + literal — recorded
/// when a search auto-delegates (backlog b804012f) so a repeat of the SAME
/// query skips delegation and runs the plain file search (the model's
/// escape hatch). Set semantics (2026-01-03): once a query has delegated,
/// identical repeats keep walking FOREVER in-session (no delegate/escape
/// ping-pong) and interleaved delegating queries never break an earlier
/// escape — the bypass set accumulates keys (bounded, oldest evicted). The
/// 2026-12-29 session saw the old single-key design re-delegate an escaped
/// query twice because another delegating search had replaced the key.
#[derive(Clone, PartialEq)]
pub(crate) struct DelegatedKey {
    pattern: String,
    glob: Option<String>,
    literal: bool,
}

impl DelegatedKey {
    /// Build the key from the tool arguments.
    pub(crate) fn new(pattern: &str, glob: Option<&str>, literal: bool) -> Self {
        Self {
            pattern: pattern.to_string(),
            glob: glob.map(str::to_string),
            literal,
        }
    }
}

/// The `search` tool — searches file contents within the project.
pub struct SearchTool {
    sandbox: Sandbox,
    /// The project's code graph, when enabled — its FTS content index serves
    /// literal queries. `None` → every query walks the tree.
    graph: Option<Arc<crate::codegraph::CodeGraph>>,
    /// The memory store, when wired — a uuid-shaped pattern earns a
    /// fired-only "known memory hit" note when a derived row knows the id
    /// (F9: that warranted memory lookup had no nudge surface before).
    memory: Option<Arc<dyn crate::memory::MemoryStoreTrait>>,
    /// The bypassed delegated-query keys (backlog b804012f, set semantics
    /// 2026-01-03): once a query has delegated, its exact key is recorded
    /// here and identical re-issues NEVER re-delegate in-session — the
    /// documented escape ("re-issue this exact search") must survive
    /// interleaved delegating queries. Bounded (most recent
    /// DELEGATION_SET_CAP keys, oldest evicted).
    delegation_state: Arc<std::sync::Mutex<Vec<DelegatedKey>>>,
}

impl SearchTool {
    /// Create the tool, bound to a sandbox. `graph` (when present) enables
    /// the index-backed path for literal queries.
    pub fn new(sandbox: Sandbox, graph: Option<Arc<crate::codegraph::CodeGraph>>) -> Self {
        Self {
            sandbox,
            graph,
            memory: None,
            delegation_state: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// Wire the memory store: uuid-shaped patterns then earn the known-
    /// memory-hit note (F9) when a derived row knows the id.
    pub fn with_memory(mut self, memory: Arc<dyn crate::memory::MemoryStoreTrait>) -> Self {
        self.memory = Some(memory);
        self
    }
}

/// Index-backed literal-search hits plus summary metadata. Produced by
/// [`try_index`]; `search` renders line-level results, `search_read` reads
/// the top-ranked distinct files.
pub(crate) struct IndexHits {
    /// Post-glob hits, best first (bm25), capped at [`MAX_MATCHES`].
    pub hits: Vec<crate::codegraph::store::ContentHit>,
    /// Total post-glob matches (may exceed `hits.len()` when capped).
    pub total: usize,
    /// Distinct matched files (post-glob).
    pub files: usize,
    /// Files refreshed by the inline stale-index re-index (F10) — 0 when
    /// the hits were served without one. The caller discloses the side
    /// effect ("reindexed N stale file(s)") because a read tool mutated
    /// the index DB.
    pub reindexed: usize,
}

/// Whether the literal can produce FTS tokens: the unicode61 tokenizer
/// emits one token per run of alphanumeric characters, so a pattern with no
/// alphanumeric character (`->`, `::`, `&&`, `(`) tokenizes to ZERO tokens
/// and the phrase query matches nothing. Such literals must walk (review
/// C2 — the index path used to answer them with a false "no matches").
fn is_tokenizable(literal: &str) -> bool {
    literal.chars().any(|c| c.is_alphanumeric())
}

/// Whether `s` is a bare ASCII identifier — `[A-Za-z_][A-Za-z0-9_]*`, no
/// metacharacters, no separators. Such a pattern is NAME-SHAPED, so it may
/// be a symbol lookup in disguise (the common drift: grepping for
/// `connectOauth` where graph_search would resolve it instantly). Patterns
/// with spaces, punctuation, or `|` alternations are text searches and
/// never nudge. Also applied to the REMAINDER of definition-prefixed
/// patterns by [`strip_definition_prefix`]. ASCII-only on purpose: the
/// nudge is advisory, and a missed nudge on a rare non-ASCII identifier
/// costs nothing.
fn is_bare_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Definition keywords recognized by [`strip_definition_prefix`] and the
/// alternation-branch reducer — one list so the two never drift.
const DEF_KEYWORDS: &[&str] = &[
    "fn",
    "struct",
    "enum",
    "trait",
    "impl",
    "mod",
    "const",
    "static",
    "type",
    "class",
    "interface",
    "function",
    "def",
];

/// Strip an optional definition prefix from a pattern: any number of
/// leading modifier keywords (`pub`, `pub(crate)`-style visibility,
/// `async`, `unsafe`, `extern`) followed by ONE definition keyword (`fn`,
/// `struct`, `enum`, `trait`, `impl`, `mod`, `const`, `static`, `type`,
/// `class`, `interface`, `function`, `def`) and a single space. Returns
/// the remainder ONLY when it is a bare identifier ([`is_bare_identifier`])
/// — anything else (a second word, trailing `()`, generics, a bare
/// keyword alone) yields `None`, so "fn search_content" resolves to
/// `search_content` while "impl Foo for Bar" and "fn foo()" never do.
fn strip_definition_prefix(s: &str) -> Option<&str> {
    let mut rest = s;
    loop {
        if let Some(r) = rest.strip_prefix("pub ") {
            rest = r;
        } else if rest.starts_with("pub(") {
            // `pub(crate) `, `pub(super) `, `pub(in path) ` — up to the
            // first `)` followed by a space.
            match rest
                .find(')')
                .and_then(|close| rest[close + 1..].strip_prefix(' '))
            {
                Some(r) => rest = r,
                None => break,
            }
        } else if let Some(r) = rest.strip_prefix("async ") {
            rest = r;
        } else if let Some(r) = rest.strip_prefix("unsafe ") {
            rest = r;
        } else if let Some(r) = rest.strip_prefix("extern ") {
            rest = r;
        } else {
            break;
        }
    }
    for kw in DEF_KEYWORDS {
        if let Some(name) = rest
            .strip_prefix(kw)
            .and_then(|after| after.strip_prefix(' '))
        {
            return is_bare_identifier(name).then_some(name);
        }
    }
    None
}

/// Reduce ONE branch of an alternation-shaped pattern to a bare identifier
/// (C1). Accepted branch shapes: a bare identifier (`watcher`), a
/// definition-prefixed name (`fn update`, `pub struct Watcher`), a
/// wildcard-decorated name (`.*watcher`, `update.*`), or a definition
/// keyword followed by an optional space and the wildcard (`fn .*update`,
/// `struct.*Watcher`). The wildcard requirement right after the keyword
/// keeps `classic` (which merely starts with "class") from ever reducing.
/// Anything else — groups, classes, spaces, escapes — yields `None`.
fn reduce_alternation_branch(part: &str) -> Option<&str> {
    if let Some(name) = strip_definition_prefix(part) {
        return Some(name);
    }
    let after_kw = DEF_KEYWORDS.iter().find_map(|kw| {
        let a = part.strip_prefix(kw)?;
        let a = a.strip_prefix(' ').unwrap_or(a);
        a.strip_prefix(".*")
    });
    let name = after_kw.or_else(|| part.strip_prefix(".*")).unwrap_or(part);
    let name = name.strip_suffix(".*").unwrap_or(name);
    is_bare_identifier(name).then_some(name)
}

/// The identifier branches of an ALTERNATION-shaped regex (C1): split on
/// top-level `|` and require EVERY branch to reduce to a bare identifier
/// ([`reduce_alternation_branch`]) — one text branch rejects the whole
/// pattern (it is a genuine content search). Two or three branches only;
/// more would bloat the note. `None` when the pattern has no top-level
/// `|` (the single-name nudge paths own those) or any branch rejects.
fn alternation_identifier_branches(pattern: &str) -> Option<Vec<&str>> {
    if !pattern.contains('|') {
        return None;
    }
    let parts: Vec<&str> = pattern.split('|').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut branches = Vec::with_capacity(parts.len());
    for part in parts {
        branches.push(reduce_alternation_branch(part)?);
    }
    Some(branches)
}

/// C1: the alternation variant of [`symbol_nudge`] — an alternation-shaped
/// regex (`fn .*watcher|struct.*Watcher`) whose every branch reduces to an
/// identifier is a symbol hunt in disguise (the 2026-09-17 tally's biggest
/// waste class, F14). Each branch is resolved LOOSELY
/// ([`crate::codegraph::CodeGraph::symbol_id_fuzzy`] — agents write partial
/// names) and the resolved symbols are ABSORBED into the note with their
/// ids, so one search call answers the symbol question instead of a grep
/// followed by N graph lookups. Branches resolving to the same symbol are
/// deduped by id; when nothing resolves there is nothing to absorb and the
/// nudge stays silent. Best-effort: `None` on any store hiccup — the nudge
/// must never fail a search.
fn alternation_nudge(
    graph: &Option<Arc<crate::codegraph::CodeGraph>>,
    pattern: &str,
) -> Option<String> {
    let branches = alternation_identifier_branches(pattern)?;
    let graph = graph.as_ref()?;
    let mut resolved: Vec<String> = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();
    for branch in branches {
        if let Ok(Some((id, name))) = graph.symbol_id_fuzzy(branch) {
            if seen_ids.insert(id.clone()) {
                let entry = if name == branch {
                    format!("{name} is an indexed symbol — graph_context(id=\"{id}\")")
                } else {
                    format!(
                        "'{branch}' resolves to '{name}', an indexed symbol — \
                         graph_context(id=\"{id}\")"
                    )
                };
                resolved.push(entry);
            }
        }
    }
    (!resolved.is_empty()).then(|| {
        format!(
            "from the alternation pattern '{pattern}': {}; use graph_search/graph_context \
             for symbol hunts — use search only for text",
            resolved.join("; ")
        )
    })
}

/// The graph-tool nudge for symbol-shaped searches: `Some(note)` when the
/// pattern names an indexed symbol through one of the recognized shapes: a
/// bare identifier, a definition-prefixed identifier (`fn foo`,
/// `pub struct Bar` — see [`strip_definition_prefix`]), or a symbol-INTENT
/// pattern (`foo(`, `a::b`, "callers of X" — see [`strip_symbol_intent`];
/// backlog 8b8f40d2: the 2026-12-04 investigation kept reaching for
/// `search` on caller questions whose patterns never nudged). Every shape
/// resolves through the EXACT case-sensitive `symbol_id` lookup (precision
/// over recall; a substring match would fire on ordinary words like
/// "mcp"), so a non-symbol target ("who calls the shots") stays silent.
/// The note embeds the symbol's id, so the model can call
/// `graph_context(id=...)` directly — one hop instead of a `graph_search`
/// resolve followed by a context call. Shared with `search_read` so both
/// grep-style tools nudge identically. `None` for non-symbol patterns,
/// absent graphs, unindexed graphs, or any store hiccup — the nudge is
/// best-effort and must never fail a search.
///
/// Wording contract: BOTH note variants contain the steering-marker
/// substring "is an indexed symbol" (`steering_stats::SEARCH_NUDGE_MARK`),
/// so the metric counts prefixed and bare nudges alike; the
/// bare-identifier sentence is byte-pinned by
/// `bare_identifier_symbol_searches_earn_the_graph_nudge` (wording moved to
/// imperatives 2027-01-14, plan 987fef4c / backlog 56168c38) and must not
/// change casually.
///
/// Since the auto-delegation (backlog b804012f), an exact single-name hit is
/// answered INLINE by `symbol_delegation_block` (codegraph.rs) — the walk is
/// skipped and this advisory note does not ride the delegated answer. The
/// note now fires when the model re-issues a delegated query (the escape
/// hatch: plain search + this nudge above the results) and, via
/// [`alternation_nudge`], for alternation hunts whose branches resolve only
/// fuzzily.
pub(crate) fn symbol_nudge(
    graph: &Option<Arc<crate::codegraph::CodeGraph>>,
    pattern: &str,
) -> Option<String> {
    // C1: an alternation-shaped hunt ("fn .*watcher|struct.*Watcher") gets
    // its own absorption note (fuzzy resolution) before the exact paths.
    if alternation_identifier_branches(pattern).is_some() {
        return alternation_nudge(graph, pattern);
    }
    let names = symbol_hunt_names(pattern)?;
    let graph = graph.as_ref()?;
    // First candidate that EXACTLY names an indexed symbol wins (a
    // `::`-path contributes its full form first, then its last segment).
    let (name, id) = names
        .iter()
        .find_map(|n| graph.symbol_id(n).map(|id| (n.as_str(), id)))?;
    let note = if name == pattern {
        format!(
            "'{pattern}' is an indexed symbol — graph_context(id=\"{id}\") gives its definition + \
             callers in one call; use graph_search/graph_context for symbol lookups — use \
             search only for text"
        )
    } else {
        format!(
            "the symbol '{name}' (from pattern '{pattern}') is an indexed symbol — \
             graph_context(id=\"{id}\") gives its definition + callers in one call; use \
             graph_search/graph_context for symbol lookups — use search only for text"
        )
    };
    Some(note)
}

/// The symbol-hunt names a pattern reduces to, if any — the shared detection
/// core of [`symbol_nudge`] and the auto-delegation path (backlog b804012f):
/// a definition-prefixed identifier (`fn foo`), a bare identifier, a
/// symbol-INTENT pattern (`foo(`, `a::b`, "callers of X"), or an alternation
/// whose every branch reduces to an identifier. `None` when the pattern is
/// none of these — a genuine content search.
pub(crate) fn symbol_hunt_names(pattern: &str) -> Option<Vec<String>> {
    if let Some(n) = strip_definition_prefix(pattern) {
        return Some(vec![n.to_string()]);
    }
    if is_bare_identifier(pattern) {
        return Some(vec![pattern.to_string()]);
    }
    if let Some(names) = strip_symbol_intent(pattern) {
        return Some(names.iter().map(|n| (*n).to_string()).collect());
    }
    alternation_identifier_branches(pattern)
        .map(|branches| branches.iter().map(|b| (*b).to_string()).collect())
}

/// Backlog 8b8f40d2: extract candidate symbol names from a symbol-INTENT
/// pattern — a search whose QUESTION is a symbol lookup ("who calls X",
/// call-site hunting) even though the pattern isn't a bare identifier.
/// Recognized shapes, each gated by [`is_bare_identifier`] so non-symbol
/// targets ("who calls the shots") yield no candidates:
/// - trailing `(` — `foo(` (call-site hunting; `(` is a regex metachar so
///   the pattern would otherwise never nudge);
/// - a `::`-path — `a::b` contributes the full path first, then the last
///   segment (graph ids are `file::name::line`, so the full path usually
///   misses and the segment hits);
/// - phrasing — `callers of X` / `who calls X` / `who uses X` /
///   `where is X defined` (an optional trailing ` defined` / ` used` /
///   ` called` is dropped).
fn strip_symbol_intent(pattern: &str) -> Option<Vec<&str>> {
    let trimmed = pattern.trim();
    // Call-site hunt: `foo(`.
    if let Some(base) = trimmed.strip_suffix('(') {
        let name = base.trim_end();
        if is_bare_identifier(name) {
            return Some(vec![name]);
        }
    }
    // `::`-path (including full graph-id forms like `src/x.rs::name` —
    // dots and slashes are part of the file segment): full path first,
    // then the last segment.
    let path_shaped = trimmed.contains("::")
        && trimmed
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == ':' || c == '.' || c == '/');
    if path_shaped {
        let tail = trimmed.rsplit("::").next().unwrap_or_default();
        if is_bare_identifier(tail) {
            return Some(vec![trimmed, tail]);
        }
    }
    // Phrasing heads with an optional trailing qualifier word.
    for head in ["callers of ", "who calls ", "who uses ", "where is "] {
        if let Some(rest) = trimmed.strip_prefix(head) {
            let mut name = rest.trim();
            for tail in [" defined", " used", " called"] {
                if let Some(base) = name.strip_suffix(tail) {
                    name = base.trim_end();
                    break;
                }
            }
            if is_bare_identifier(name) {
                return Some(vec![name]);
            }
        }
    }
    None
}

/// C2: whether the pattern carries regex metacharacters (the conservative
/// set — the same set [`is_index_eligible_regex`] rejects). A
/// `literal:true` search over such a pattern matches the text VERBATIM; a
/// zero-hit result is then the signature of a mode mistake, and the
/// empty-result branches say so ([`literal_metachar_hint`]).
pub(crate) fn has_regex_metachars(pattern: &str) -> bool {
    pattern.chars().any(|c| {
        matches!(
            c,
            '\\' | '^' | '$' | '.' | '|' | '?' | '*' | '+' | '(' | ')' | '[' | ']' | '{' | '}'
        )
    })
}

/// C2: the retry hint appended to an EMPTY literal-mode result whose
/// pattern carries regex metacharacters — the moment a mode mistake
/// actually bites. Non-empty results never carry it (an intentional
/// literal metachar search that matches is fine), and regex-mode misses
/// never do (the mode was already regex).
pub(crate) fn literal_metachar_hint() -> &'static str {
    "; the pattern has regex metacharacters but ran as literal text — if you meant \
     a regex, retry with literal:false"
}

/// Whether a REGEX-mode pattern's matcher is exactly equivalent to a
/// literal match AND the content index could serve it: single line,
/// non-empty, no regex metacharacter (so the regex and the literal agree
/// line-for-line), and at least one alphanumeric character (so the FTS
/// tokenizer emits a token — see [`is_tokenizable`]). Such a walk could
/// have been an indexed lookup; [`literal_tip`] says so.
fn is_index_eligible_regex(pattern: &str) -> bool {
    !pattern.is_empty()
        && !pattern.contains('\n')
        && !has_regex_metachars(pattern)
        && is_tokenizable(pattern)
}

/// The literal-engine TIP for a regex-mode walk that the content index
/// could have served exactly: `Some(note)` when the call is in regex mode,
/// the pattern is [`is_index_eligible_regex`], a graph is present with a
/// populated content index (two cheap COUNT queries — never the search
/// itself), and the symbol nudge did NOT fire for this call. Priority is
/// deliberate: the symbol nudge is the stronger steering (a whole graph
/// lookup beats an engine switch), so when it fires the TIP is suppressed
/// rather than stacking a second note. `None` otherwise — best-effort, and
/// it must never fail a search. Shared with `search_read`.
pub(crate) fn literal_tip(
    graph: &Option<Arc<crate::codegraph::CodeGraph>>,
    pattern: &str,
    literal_mode: bool,
    symbol_nudged: bool,
) -> Option<String> {
    if literal_mode || symbol_nudged || !is_index_eligible_regex(pattern) {
        return None;
    }
    let graph = graph.as_ref()?;
    // Cheap population probe only — the tip claims an engine EXISTS to
    // switch to, not that this query would hit (glob edge cases can still
    // walk under literal:true; the advice stays true either way).
    let (_, with_content) = graph.content_coverage().ok()?;
    if with_content == 0 {
        return None;
    }
    Some(
        "TIP: pattern has no regex metacharacters — literal:true would use the \
         content-index engine (one indexed lookup instead of a tree walk)"
            .to_string(),
    )
}

/// The FTS index attempt's outcome: usable hits, or a STALE index the
/// caller must walk around (F10 — the watcher's reindex lags edits, so the
/// rows can answer from pre-edit content; the walk is the authoritative
/// engine and the staleness is surfaced).
pub(crate) enum FtsOutcome {
    Hits(IndexHits),
    /// The FTS rows lag the working tree for `files` of the returned hits
    /// and the inline refresh did not clear it — above
    /// [`STALE_REINDEX_CAP`], an index pass was already running, the
    /// re-index failed, or the files were edited again during the retry.
    /// The caller falls through to the walk and surfaces the staleness.
    Stale {
        files: usize,
    },
}

/// One FTS query page: the post-glob hits plus the stale-file paths the
/// F10 freshness sweep detected. Private to [`try_index`]'s retry loop.
struct FtsPage {
    hits: Vec<crate::codegraph::store::ContentHit>,
    total: usize,
    files: std::collections::HashSet<String>,
    stale_paths: Vec<String>,
}

/// Run one FTS query page: fetch, glob post-filter, and the F10 freshness
/// sweep. Returns `None` exactly where the index path must walk (index
/// unavailable/unpopulated, non-tokenizable literal, zero post-glob hits —
/// see [`try_index`]); `Some(page)` carries the hits and the stale paths.
fn fts_page(
    graph: &crate::codegraph::CodeGraph,
    root: &Path,
    pattern: &str,
    glob: Option<&str>,
) -> Option<FtsPage> {
    let (_, with_content) = graph.content_coverage().ok()?;
    if with_content == 0 {
        return None;
    }
    // Punctuation-only literals tokenizes to zero FTS tokens — a phrase
    // query would silently match nothing (review C2). Walk instead.
    if !is_tokenizable(pattern) {
        return None;
    }
    // Fetch beyond the display cap so glob post-filtering cannot starve the
    // first page (a restrictive glob discards early hits).
    let raw = graph.search_content(pattern, MAX_MATCHES * 5).ok()?;
    let pats = compile_globs(glob).ok()?;
    let mut hits = Vec::new();
    let mut total = 0usize;
    let mut files = std::collections::HashSet::new();
    for h in raw {
        if !pats
            .iter()
            .any(|p| p.matches_path(std::path::Path::new(&h.path)))
        {
            continue;
        }
        total += 1;
        files.insert(h.path.clone());
        if hits.len() < MAX_MATCHES {
            hits.push(h);
        }
    }
    if total == 0 {
        // Never assert "no matches" from the index — walk for the
        // authoritative answer. Two cases: (1) the fetch returned hits but
        // every one failed the glob, and the fetch is capped, so glob
        // matches may exist beyond it (review C3, the starved page). (2) the
        // fetch returned NOTHING — long treated as authoritative ("the
        // index covers every searchable file"), but the watcher's reindex
        // lags NEW files: a file written minutes ago has no FTS row yet,
        // and the F10 freshness check below only examines files already IN
        // the fetched hits — so an empty fetch silently false-negatived
        // newly written .coding/ files (observed 2026-12-29: 'bf0f71c' in a
        // fresh review file). An empty fetch can never prove absence.
        return None;
    }
    // F10 freshness: the watcher's reindex lags edits, so the FTS rows can
    // answer from pre-edit content. Compare EVERY glob-passing hit file on
    // the fetched page (the `files` set — so the displayed lines AND the
    // match counts stay authoritative; review 2026-09-17 finding 3)
    // against the as-of-index mtime (unix millis — the indexer's
    // `mtime_of`). Any mismatch — an unreadable/vanished file, or a hit
    // without a cg_files row — makes the page stale; the paths are
    // collected so a small staleness can be re-indexed inline.
    let stored = graph.stored_mtimes().ok()?;
    let mut stale_paths = Vec::new();
    for path in &files {
        let Some(&stored_mtime) = stored.get(path) else {
            stale_paths.push(path.clone());
            continue;
        };
        let on_disk = std::fs::metadata(root.join(path))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        if on_disk != stored_mtime {
            stale_paths.push(path.clone());
        }
    }
    Some(FtsPage {
        hits,
        total,
        files,
        stale_paths,
    })
}

/// Try the FTS content index for a single-line literal pattern. Returns
/// `None` when the index is unavailable, unpopulated, or errors — the caller
/// then walks, transparently. Also `None` (→ walk) when the literal cannot
/// tokenize (zero FTS tokens would match nothing) or when the glob post-filter
/// discarded every fetched hit (glob matches may exist beyond the fetch cap —
/// the walk produces the authoritative answer). Hits are post-filtered
/// through the (already validated) glob so index results honor the same
/// scoping as the walk.
///
/// F10 staleness (the FTS rows lag the working tree for some hit files) is
/// REPAIRED, not just surfaced: when at most [`STALE_REINDEX_CAP`] files are
/// stale and no index pass is running, they are re-indexed inline
/// ([`crate::codegraph::CodeGraph::reindex_stale_files`]) and the query is
/// re-run ONCE from the fresh index — the outcome then carries the number of
/// refreshed files so the caller can disclose the side effect. A staleness
/// above the cap, a busy index pass, a failed re-index, or files edited again
/// during the retry yields [`FtsOutcome::Stale`] — the caller walks and
/// surfaces the staleness. A re-index that empties the result set returns
/// `None` (→ walk, no note): the walk is the authoritative answer.
pub(crate) fn try_index(
    graph: &crate::codegraph::CodeGraph,
    root: &Path,
    pattern: &str,
    glob: Option<&str>,
) -> Option<FtsOutcome> {
    let mut reindexed = 0usize;
    for attempt in 0..2 {
        let page = fts_page(graph, root, pattern, glob)?;
        if page.stale_paths.is_empty() {
            return Some(FtsOutcome::Hits(IndexHits {
                hits: page.hits,
                total: page.total,
                files: page.files.len(),
                reindexed,
            }));
        }
        if attempt == 0
            && page.stale_paths.len() <= STALE_REINDEX_CAP
            && graph
                .reindex_stale_files(&page.stale_paths)
                .is_ok_and(|n| n > 0)
        {
            reindexed = page.stale_paths.len();
            continue; // re-query once from the fresh index
        }
        return Some(FtsOutcome::Stale {
            files: page.stale_paths.len(),
        });
    }
    None
}

/// Prepend the literal-fallback note (if any) so the model sees "these are
/// literal matches" before reading the results themselves.
pub(crate) fn with_note(body: String, note: &Option<String>) -> String {
    match note {
        Some(n) => format!("note: {n}\n\n{body}"),
        None => body,
    }
}

/// Merge one more component into a prepended-note option ("; "-joined,
/// like [`merged_note`]) — for notes that only exist on one outcome arm
/// (the stale-index note, the inline re-index disclosure). The component
/// must NOT start with "note: " — [`with_note`] adds that prefix once.
pub(crate) fn push_note(note: Option<String>, add: String) -> Option<String> {
    match note {
        Some(n) => Some(format!("{n}; {add}")),
        None => Some(add),
    }
}

/// Merge the delegated-answer block (when a symbol hunt narrowed by a glob
/// is prepended above normal results), the literal-fallback note, the
/// symbol nudge, the literal-engine TIP, and the known-memory-hit note into
/// ONE prepended note (joined with "; ") — every component must stay ABOVE
/// the results: tool output is capped for the context budget, so a note
/// appended at the end could be truncated away exactly when the results are
/// long. The delegation block rides FIRST (it is the answer, not steering).
/// At most two of the steering components can co-occur in practice (a
/// metachar-free pattern never trips the broken-regex fallback; the TIP is
/// suppressed when the nudge fires), but all pairings are joined uniformly.
pub(crate) fn merged_note(
    delegation: Option<String>,
    fallback: Option<String>,
    nudge: Option<String>,
    tip: Option<String>,
    known: Option<String>,
) -> Option<String> {
    let joined: Vec<&str> = [
        delegation.as_deref(),
        fallback.as_deref(),
        nudge.as_deref(),
        tip.as_deref(),
        known.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect();
    if joined.is_empty() {
        None
    } else {
        Some(joined.join("; "))
    }
}

/// F2: filename fallback — when a content search matched nothing, match
/// the pattern against the walked files' project-relative paths (a plan
/// file never contains its own id, so id hunts otherwise walk the whole
/// tree for zero hits — the 2026-09-15 tally's one true MISS). Cheap:
/// reuses the walk's already-collected list, no extra fs access.
pub(crate) fn filename_matches(paths: &[PathBuf], root: &Path, re: &regex::Regex) -> Vec<String> {
    paths
        .iter()
        .filter_map(|p| {
            let rel = p
                .strip_prefix(root)
                .unwrap_or(p)
                .to_string_lossy()
                .replace('\\', "/");
            re.is_match(&rel).then_some(rel)
        })
        .collect()
}

/// The F2 hint line for [`filename_matches`] results: the count plus up to
/// five example paths; empty when there are none.
pub(crate) fn filename_hint(hits: &[String]) -> String {
    if hits.is_empty() {
        return String::new();
    }
    let shown: Vec<&str> = hits.iter().take(5).map(String::as_str).collect();
    format!(
        "; no content matches, but {} file path(s) match the pattern: {}{}",
        hits.len(),
        shown.join(", "),
        if hits.len() > 5 { ", …" } else { "" }
    )
}

/// F9: a pattern that IS a backlog id gets a fired-only "known memory hit"
/// note when a derived row knows it — the warranted memory lookup this
/// session class had no nudge surface for. UUID-shaped patterns only:
/// commit shas have no cheap deterministic store lookup (documented
/// residual). Best-effort: one indexed prefix SELECT; any error → None.
pub(crate) async fn known_memory_note(
    store: &Arc<dyn crate::memory::MemoryStoreTrait>,
    pattern: &str,
) -> Option<String> {
    let id = backlog_id_shape(pattern)?;
    let hits = store
        .list_filtered(
            &crate::memory::MemoryFilter::new()
                .title_prefix(format!("PLAN: backlog {id}"))
                .limit(1),
        )
        .await
        .ok()?;
    hits.first().map(|m| {
        format!(
            "known memory hit: '{}' — this id is a known backlog item; memory_search it for detail",
            m.title
        )
    })
}

/// The backlog id the pattern consists of, when it does — a canonical
/// 8-4-4-4-12 hex UUID (trimmed). Anything else (real content searches,
/// shas, prose) returns `None` and the store is never touched.
fn backlog_id_shape(pattern: &str) -> Option<&str> {
    let t = pattern.trim();
    let parts: Vec<&str> = t.split('-').collect();
    let lens = [8usize, 4, 4, 4, 12];
    let shaped = parts.len() == 5
        && parts
            .iter()
            .zip(lens)
            .all(|(p, l)| p.len() == l && p.chars().all(|c| c.is_ascii_hexdigit()));
    shaped.then_some(t)
}

/// Typed memory-record prefixes that mark a pattern as a memory hunt — the
/// same convention the memory system itself uses for record titles.
const MEMORY_TYPED_PREFIXES: [&str; 6] = [
    "SPEC:", "DECISION:", "BUG:", "PLAN:", "HOW:", "REVIEW:",
];

/// Does this query target the memory store rather than file content?
/// Conservative by design (backlog b804012f): only a glob anchored on the
/// knowledge/reviews directories or an explicit typed prefix fires — a
/// pattern that merely MENTIONS `.coding/knowledge` is a legitimate content
/// search and stays one. False positives cost real results (the walk is
/// skipped); misses cost nothing (plain search).
pub(crate) fn memory_hunt(pattern: &str, glob: Option<&str>) -> bool {
    if let Some(g) = glob {
        if g.contains(".coding/knowledge") || g.contains(".coding/reviews") {
            return true;
        }
    }
    let trimmed = pattern.trim_start();
    MEMORY_TYPED_PREFIXES.iter().any(|p| trimmed.starts_with(p))
}

/// The delegated memory answer for a memory-targeted query (backlog
/// b804012f): run the semantic recall the model would otherwise reach for
/// with `memory_search` and prepend the top hits inline, skipping the file
/// walk. Best-effort — a missed detection, an empty recall, or any store
/// error returns `None` and the caller falls through to the normal search.
pub(crate) async fn memory_delegation_block(
    store: &Arc<dyn crate::memory::MemoryStoreTrait>,
    pattern: &str,
    glob: Option<&str>,
) -> Option<String> {
    if !memory_hunt(pattern, glob) {
        return None;
    }
    let filter = crate::memory::MemoryFilter::new().limit(3);
    let hits = store.recall(pattern, &filter).await.ok()?;
    if hits.is_empty() {
        return None;
    }
    let mut out = String::from(
        "AUTO-DELEGATED to memory — this query targets the memory store \
         (re-issue this exact search to get the plain file search instead):",
    );
    for (i, sm) in hits.iter().enumerate() {
        let gist: String = sm.memory.content.chars().take(160).collect();
        let gist = gist.split_whitespace().collect::<Vec<&str>>().join(" ");
        out.push_str(&format!(
            "\n  {}. [{}] {} (score {:.2}) — {gist}",
            i + 1,
            sm.memory.tier,
            sm.memory.title,
            sm.score,
        ));
    }
    out.push_str(&format!("\n  full detail: memory_search(query=\"{pattern}\")"));
    Some(out)
}

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &str {
        "search"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "search",
            "Searches a pattern (regex by default, or literal) in files matching an optional \
             glob. Literal queries use the full-text content index when populated \
             (engine: index), walking otherwise; metachar-free regex walks carry a TIP \
             pointing at literal:true. A broken regex is matched literally with a note. \
             Returns matching lines with paths and line numbers. Build output and \
             dependencies (target/, node_modules/, .git/, dist/) are pruned from the walk. \
              For symbol questions — where is X defined, who calls X, callers/callees/blast \
              radius — call graph_search (then graph_context(id=...)) FIRST; this tool is for \
              text occurrences (comments, string literals, config keys, log text). \
              Symbol-shaped patterns (bare name, 'fn X', 'X(', 'a::b', 'who calls X') naming \
              an indexed symbol, and memory hunts (.coding/knowledge|reviews globs, typed \
              SPEC:/DECISION:/ prefixes), auto-delegate: the graph/memory answer rides inline \
              and the walk is skipped; re-issue the same search to get the plain file search.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "The pattern to search for."},
                    "glob": {"type": "string", "description": "Glob pattern to filter files (e.g. \"**/*.rs\")."},
                    "literal": {"type": "boolean", "description": "Treat pattern as literal text (default: false, regex)."}
                },
                "required": ["pattern"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: SearchArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(format!("invalid arguments: {e}")),
        };

        // Validate the glob BEFORE use so a pattern that escapes the sandbox
        // root (absolute path, drive prefix, UNC, `..` traversal) is rejected
        // outright rather than read. Auto-run tool — no approval to catch this.
        if let Some(glob) = &args.glob {
            if let Err(e) = validate_glob(glob) {
                return ToolResult::error(e);
            }
        }

        // F9: known-backlog-item note — fired only when the pattern is
        // uuid-shaped AND a derived row knows it (best-effort, one SELECT).
        let memory_note = match &self.memory {
            Some(store) => known_memory_note(store, &args.pattern).await,
            None => None,
        };

        // Auto-delegation escape hatch (backlog b804012f, set semantics
        // 2026-01-03): a repeat of ANY previously delegated query skips
        // delegation entirely and runs the plain file search. Sticky per
        // exact key — interleaved delegating queries never break an earlier
        // escape, and there is no delegate/escape ping-pong.
        let key = DelegatedKey::new(&args.pattern, args.glob.as_deref(), args.literal);
        let escape = self.delegation_state.lock().unwrap().contains(&key);

        // Memory delegation (backlog b804012f): a query targeting the memory
        // store (knowledge/reviews glob or typed prefix) gets the recall
        // answer inline. Async store read — before the blocking scan, like
        // the F9 note above.
        let memory_block = if escape {
            None
        } else {
            match &self.memory {
                Some(store) => {
                    memory_delegation_block(store, &args.pattern, args.glob.as_deref()).await
                }
                None => None,
            }
        };

        // Offload the blocking scan onto the blocking pool (Perf H1). search is
        // O(files × lines) with full content loads per matched file — the
        // highest-leverage tool to offload. The regex compile + glob + per-file
        // metadata + read_to_string + match loop all run in one closure so the
        // exact early-exit/cap/count semantics are preserved (only the *thread*
        // the work runs on changes). The sandbox contributes only its root
        // (a PathBuf), moved in by value.
        let root = self.sandbox.root().to_path_buf();
        let graph = self.graph.clone();
        let delegation_state = self.delegation_state.clone();
        tokio::task::spawn_blocking(move || {
            // Compile via the shared helper: a broken regex degrades to
            // literal matching with a visible note (read tool — no mutation
            // risk), instead of erroring the whole call.
            let compiled = match pattern::compile_with_fallback(&args.pattern, args.literal, true) {
                Ok(c) => c,
                Err(e) => return ToolResult::error(e),
            };
            let re = compiled.regex;
            let fallback_note = compiled.fallback_note;

            // Auto-delegation (backlog b804012f): a symbol hunt the graph
            // can answer gets the answer inline instead of the advisory
            // nudge + a wasted walk; a memory-targeted query gets the recall
            // answer (memory wins when both fire — a knowledge/reviews glob
            // is a memory hunt by definition). When the delegated answer
            // fully serves the query (memory, or a symbol hunt with no
            // glob) the file walk is skipped entirely and the key is
            // recorded so an immediate repeat gets the plain search (the
            // escape hatch); a symbol hunt narrowed by a glob PREPENDS the
            // block above the normal results instead.
            let symbol_block = if escape {
                None
            } else {
                graph.as_deref().and_then(|g| {
                    symbol_hunt_names(&args.pattern).and_then(|names| {
                        crate::tool::agent::codegraph::symbol_delegation_block(g, &names)
                    })
                })
            };
            let memory_delegated = memory_block.is_some();
            let delegated = memory_block.or(symbol_block);
            let mut prepended: Option<String> = None;
            if let Some(block) = delegated {
                // Record the key on BOTH arms: the fast-path return AND the
                // glob-narrowed prepend — the block promises the escape
                // ("re-issue this exact search…") either way, so the repeat
                // must run the plain search (review 2026-12-28 H1). Set
                // semantics: the key joins the bypass set (dedup, bounded —
                // oldest evicted) without replacing other queries' escapes.
                {
                    let mut set = delegation_state.lock().unwrap();
                    set.retain(|k| *k != key);
                    set.push(key);
                    let overflow = set.len().saturating_sub(DELEGATION_SET_CAP);
                    set.drain(..overflow);
                }
                if memory_delegated || args.glob.is_none() {
                    return ToolResult::success(block);
                }
                prepended = Some(block);
            }

            // Symbol-shaped pattern? One advisory line nudging toward the
            // graph tools (cheap: a single indexed SELECT, skipped for
            // non-identifier patterns and absent/unindexed graphs). The
            // literal-engine TIP rides the same line when the nudge did NOT
            // fire (symbol steering wins — one note, not two). Both yield
            // when a delegated block is prepended (it subsumes them). All
            // merged so everything rides ABOVE the results.
            let nudge = if prepended.is_some() {
                None
            } else {
                symbol_nudge(&graph, &args.pattern)
            };
            let tip = if prepended.is_some() {
                None
            } else {
                literal_tip(&graph, &args.pattern, args.literal, nudge.is_some())
            };
            let note = merged_note(prepended, fallback_note, nudge, tip, memory_note);

            // The FTS content index serves literal, single-line queries when
            // populated. A pattern containing \n spans FTS's per-line rows —
            // those stay on the walk path, where match_lines handles them.
            // A fallback-fired pattern (broken regex retried as literal) also
            // walks: the FTS phrase drops non-token characters (e.g. `foo(`
            // queries token `foo`), matching MORE lines than the literal
            // promises — only the walk engine keeps "matched literally"
            // semantics exact (review B1). A stale index (F10) is repaired
            // inline for a small staleness — try_index re-indexes at most
            // STALE_REINDEX_CAP files and re-queries once — so a Stale
            // outcome here means the repair was not attempted or did not
            // clear it (above the cap, a pass running, or re-edited files).
            let mut stale_note: Option<String> = None;
            if args.literal && !args.pattern.contains('\n') {
                if let Some(g) = &graph {
                    match try_index(g, &root, &args.pattern, args.glob.as_deref()) {
                        Some(FtsOutcome::Hits(ix)) => {
                            // try_index returns Hits only with total ≥ 1
                            // (an empty fetch walks — 2026-01-03), so the
                            // hits are never empty here; the C2 metachar
                            // retry hint lives on the walk path.
                            let lines: Vec<String> = ix
                                .hits
                                .iter()
                                .map(|h| format!("{}:{}: {}", h.path, h.line, h.text.trim()))
                                .collect();
                            let mut body = format!(
                                "{} matches in {} files (engine: index)\n\n{}",
                                ix.total,
                                ix.files,
                                lines.join("\n")
                            );
                            if ix.total > ix.hits.len() {
                                body.push_str(
                                    "\n... and more matches (narrow your pattern or use a glob filter)",
                                );
                            }
                            // Disclose the inline re-index (F10): a read
                            // tool mutated the index DB — the note rides
                            // ABOVE the results (truncation-safe).
                            let note = if ix.reindexed > 0 {
                                push_note(
                                    note,
                                    format!(
                                        "reindexed {} stale file(s) — serving fresh index results",
                                        ix.reindexed
                                    ),
                                )
                            } else {
                                note
                            };
                            return ToolResult::success(with_note(body, &note));
                        }
                        Some(FtsOutcome::Stale { files }) => {
                            stale_note = Some(format!(
                                "content index stale for {files} file(s) — serving tree-walk results"
                            ));
                        }
                        None => {}
                    }
                }
            }
            // F10: a stale index outcome already fell through to the walk —
            // merge the staleness into the prepended note so the explanation
            // rides ABOVE the results (truncation-safe). push_note joins
            // without a "note: " prefix — with_note adds it exactly once.
            let note = match stale_note {
                Some(s) => push_note(note, s),
                None => note,
            };

            // Pruned tree walk (shared helper): ignored directories are cut
            // wholesale — their children are never enumerated — and each
            // directory's entries arrive sorted (glob-crate-equivalent
            // order). An unreadable root degrades to an empty walk, exactly
            // like the old glob enumeration failure. Every yielded path is
            // under the sandbox root by construction.
            let (paths, pruned_dirs) =
                walk_searchable(&root, args.glob.as_deref()).unwrap_or((Vec::new(), 0));

            let mut results = Vec::new();
            let mut files_searched = 0u32;
            let mut files_matched = 0u32;
            let mut total_matches = 0usize;
            let mut capped = false;

            for entry in &paths {
                // Early exit once we have collected MAX_MATCHES results.
                // This avoids reading and scanning further file contents (Perf quick win).
                if results.len() >= MAX_MATCHES {
                    capped = true;
                    break;
                }
                files_searched += 1;
                let content = match std::fs::read_to_string(entry) {
                    Ok(c) => c,
                    Err(_) => continue, // skip binary/unreadable
                };
                let mut file_matched = false;
                // match_lines scans per-line for single-line patterns
                // (CRLF-safe via str::lines) and whole-content
                // (CRLF-normalized) for patterns that can span lines.
                for m in pattern::match_lines(&content, &re, &args.pattern) {
                    total_matches += 1;
                    // Only collect up to MAX_MATCHES; stop collecting
                    // and break out of this file's scan early.
                    if results.len() < MAX_MATCHES {
                        let rel = entry
                            .strip_prefix(&root)
                            .unwrap_or(entry)
                            .to_string_lossy()
                            .replace('\\', "/");
                        results.push(format!("{rel}:{}: {}", m.line, m.text.trim()));
                        file_matched = true;
                    } else {
                        capped = true;
                        break;
                    }
                }
                if file_matched {
                    files_matched += 1;
                }
                // After a file, re-check for early exit on next iteration (handled above).
            }

            let body = if results.is_empty() {
                // C2: a literal-mode miss over a regex-shaped pattern is
                // the signature of a mode mistake — the retry hint.
                let mode_hint = if args.literal && has_regex_metachars(&args.pattern) {
                    literal_metachar_hint()
                } else {
                    ""
                };
                // F7: a glob that matched zero FILES is a different failure
                // from a pattern that matched zero content — say so, so an
                // unsupported/garbled shape is diagnosable in one call.
                let glob_hint = if args.glob.is_some() && files_searched == 0 {
                    "; the glob matched no files — if that's unexpected, \
                     use extension-anchored shapes like **/*.rs"
                } else {
                    ""
                };
                // F2: filename fallback — point at path matches when the
                // content search came up empty.
                let name_hint = filename_hint(&filename_matches(&paths, &root, &re));
                format!(
                    "no matches found{mode_hint}{glob_hint}{name_hint} (searched {files_searched} files, skipped {pruned_dirs} ignored dirs, engine: walk)"
                )
            } else {
                let mut summary = format!(
                    "{} matches in {} files (searched {}, skipped {} ignored dirs, engine: walk)\n\n{}",
                    total_matches,
                    files_matched,
                    files_searched,
                    pruned_dirs,
                    results.join("\n")
                );
                if capped {
                    // With early-exit we stop scanning once MAX_MATCHES are collected,
                    // so we do not know the exact remainder. Reflect "and more".
                    summary.push_str(
                        "\n... and more matches (narrow your pattern or use a glob filter)",
                    );
                }
                summary
            };
            ToolResult::success(with_note(body, &note))
        })
        .await
        .unwrap_or_else(|e| ToolResult::error(format!("search task failed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_tool(dir: &std::path::Path) -> SearchTool {
        SearchTool::new(Sandbox::new(dir).unwrap(), None)
    }

    /// A tool backed by a freshly indexed in-memory graph (index path).
    fn make_indexed_tool(dir: &std::path::Path) -> SearchTool {
        let graph = crate::codegraph::CodeGraph::open_in_memory(dir.to_path_buf()).unwrap();
        graph.index(None).unwrap();
        SearchTool::new(Sandbox::new(dir).unwrap(), Some(std::sync::Arc::new(graph)))
    }

    #[tokio::test]
    async fn searches_file_contents() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn hello() {}\nfn world() {}").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn goodbye() {}").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"pattern": "hello|world"})).await;
        assert!(result.success);
        assert!(result.output.contains("a.rs:1"));
        assert!(result.output.contains("a.rs:2"));
        assert!(!result.output.contains("b.rs"));
    }

    #[tokio::test]
    async fn searches_with_glob_filter() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "target").unwrap();
        std::fs::write(dir.path().join("a.txt"), "target").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"pattern": "target", "glob": "**/*.rs"}))
            .await;
        assert!(result.success);
        assert!(result.output.contains("a.rs"));
        assert!(!result.output.contains("a.txt"));
    }

    #[tokio::test]
    async fn literal_search() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "price: $5.00").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"pattern": "$5.00", "literal": true}))
            .await;
        assert!(result.success);
        assert!(result.output.contains("a.txt"));
    }

    #[tokio::test]
    async fn no_matches() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"pattern": "nonexistent"})).await;
        assert!(result.success);
        assert!(result.output.contains("no matches"));
    }

    #[tokio::test]
    async fn invalid_regex_falls_back_to_literal() {
        // Regression: a pattern with unbalanced metacharacters used to
        // hard-error the whole call ("invalid regex"). Read tools degrade to
        // literal matching with a visible note instead.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "call [invalid syntax here").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"pattern": "[invalid"})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("matched literally"),
            "fallback note present: {}",
            result.output
        );
        assert!(
            result.output.contains("a.rs:1"),
            "literal match found: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn multi_line_pattern_matches_crlf_file() {
        // Regression: a pattern containing \n could never match a per-line
        // scan, and CRLF files broke even whole-content matching. The shared
        // matcher normalizes and matches whole-content for spanning patterns.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn foo() {\r\n    bar();\r\n}\r\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"pattern": "fn foo() {\n    bar();"}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("a.rs:1"),
            "match reported at its starting line: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn literal_query_uses_index_engine() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "the memory_recall engine\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn other() {}\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        let result = tool
            .execute(json!({"pattern": "memory_recall", "literal": true}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("engine: index"),
            "index engine: {}",
            result.output
        );
        assert!(result.output.contains("a.rs:1"));
        assert!(!result.output.contains("b.rs"));
    }

    #[tokio::test]
    async fn fallback_literal_walks_even_with_index() {
        // Review B1 regression: a broken regex degrades to literal matching,
        // but must NOT ride the index — the FTS phrase drops non-token
        // characters (`[invalid` queries token `invalid`), which would match
        // MORE lines than the literal promises. Only the walk engine keeps
        // "matched literally" semantics exact.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "call [invalid syntax here\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "just the word invalid alone\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        let result = tool.execute(json!({"pattern": "[invalid"})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("matched literally"),
            "fallback note present: {}",
            result.output
        );
        assert!(
            result.output.contains("engine: walk"),
            "fallback must walk for exact literal semantics: {}",
            result.output
        );
        assert!(
            result.output.contains("a.rs:1"),
            "literal match found: {}",
            result.output
        );
        assert!(
            !result.output.contains("b.rs"),
            "a line without the literal must not match: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn index_covers_non_source_files() {
        // Review C1 regression: the content index must cover EVERY searchable
        // file (any extension), not just parseable source — a literal query
        // answered from a partial index silently missed docs/config files
        // and could report a false "no matches".
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "the needle marker\n").unwrap();
        std::fs::write(dir.path().join("b.md"), "# needle\n").unwrap();
        std::fs::write(dir.path().join("c.json"), "{\"needle\": 1}\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        let r_index = tool
            .execute(json!({"pattern": "needle", "literal": true}))
            .await;
        assert!(r_index.success, "{}", r_index.output);
        assert!(
            r_index.output.contains("engine: index"),
            "{}",
            r_index.output
        );
        assert!(
            r_index.output.contains("a.rs:1")
                && r_index.output.contains("b.md:1")
                && r_index.output.contains("c.json:1"),
            "all file types served by the index: {}",
            r_index.output
        );
        // Totals agree with the walk engine over the same tree.
        assert!(
            r_index.output.contains("3 matches in 3 files"),
            "index totals: {}",
            r_index.output
        );
        let walker = make_tool(dir.path());
        let r_walk = walker
            .execute(json!({"pattern": "needle", "literal": true}))
            .await;
        assert!(
            r_walk.output.contains("3 matches in 3 files"),
            "walk totals: {}",
            r_walk.output
        );
    }

    #[tokio::test]
    async fn doc_only_term_found_via_index() {
        // Review C1 regression (the false negative): a term occurring ONLY
        // in a non-source file must still be found via the index engine.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn other() {}\n").unwrap();
        std::fs::write(
            dir.path().join("notes.md"),
            "see docs_only_term for details\n",
        )
        .unwrap();
        let tool = make_indexed_tool(dir.path());
        let result = tool
            .execute(json!({"pattern": "docs_only_term", "literal": true}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("engine: index"),
            "index engine: {}",
            result.output
        );
        assert!(
            result.output.contains("notes.md:1"),
            "doc-only term found (was a false 'no matches'): {}",
            result.output
        );
    }

    #[tokio::test]
    async fn punctuation_only_literal_walks_even_with_index() {
        // Review C2 regression: a punctuation-only literal tokenizes to zero
        // FTS tokens — the index used to answer "no matches found (engine:
        // index)" while the walk finds plenty. Such literals must walk.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "let x = a -> b;\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        let result = tool
            .execute(json!({"pattern": "->", "literal": true}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("engine: walk"),
            "zero-token literal must walk: {}",
            result.output
        );
        assert!(
            result.output.contains("a.rs:1"),
            "walk finds the punctuation match: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn glob_starved_index_falls_back_to_walk() {
        // Review C3 regression: when every FETCHED FTS hit fails the glob,
        // glob matches may exist beyond the fetch cap — the tool must walk
        // instead of asserting "no matches (engine: index)".
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn needle() {}\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        let result = tool
            .execute(json!({"pattern": "needle", "literal": true, "glob": "**/*.zzz"}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("engine: walk"),
            "a starved index page must defer to the walk: {}",
            result.output
        );
        assert!(result.output.contains("no matches"), "{}", result.output);
    }

    #[tokio::test]
    async fn empty_index_falls_back_to_walk() {
        // Graph present but never indexed → zero content rows → walk.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn helper() {}\n").unwrap();
        let graph = crate::codegraph::CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        // Deliberately no index() call — cg_content_meta stays empty.
        let tool = SearchTool::new(
            Sandbox::new(dir.path()).unwrap(),
            Some(std::sync::Arc::new(graph)),
        );
        let result = tool
            .execute(json!({"pattern": "helper", "literal": true}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("engine: walk"),
            "walk engine: {}",
            result.output
        );
        assert!(result.output.contains("a.rs:1"));
    }

    #[tokio::test]
    async fn index_empty_fetch_falls_through_to_walk_for_new_files() {
        // Backlog (2026-12-29 session, plan 72329f2c): a literal search for
        // 'bf0f71c' with glob .coding/reviews/*.md returned "no matches found
        // (engine: index; 1826 files indexed)" although the file containing
        // it had been written minutes earlier — the watcher's reindex lags
        // new files, and try_index treated the EMPTY fetch as authoritative
        // ("the index covers every searchable file"). An empty fetch can
        // never prove absence: it must fall through to the walk (the
        // authoritative engine) so a newly written .coding/ file is found.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn helper() {}\n").unwrap();
        // Index FIRST (the index is populated and looks fresh)…
        let tool = make_indexed_tool(dir.path());
        // …THEN write the new file the index has never seen.
        std::fs::create_dir_all(dir.path().join(".coding/reviews")).unwrap();
        std::fs::write(
            dir.path().join(".coding/reviews/2026-12-23-security-review.md"),
            "## Verdict: PASS\n\nbf0f71c marker\n",
        )
        .unwrap();
        let result = tool
            .execute(json!({
                "pattern": "bf0f71c",
                "literal": true,
                "glob": ".coding/reviews/*.md"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("2026-12-23-security-review.md"),
            "the newly written file must be found (empty index fetch → walk): {}",
            result.output
        );
    }

    #[tokio::test]
    async fn index_path_respects_glob_filter() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn helper() {}\n").unwrap();
        std::fs::write(dir.path().join("a.ts"), "export const helper = 1;\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        let result = tool
            .execute(json!({"pattern": "helper", "literal": true, "glob": "**/*.rs"}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("engine: index"));
        assert!(result.output.contains("a.rs:1"));
        assert!(!result.output.contains("a.ts"));
    }

    #[tokio::test]
    async fn regex_query_walks_even_with_index() {
        // Regex patterns can't be served by FTS — the engine must stay walk.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn helper() {}\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        let result = tool.execute(json!({"pattern": "hel+per"})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("engine: walk"),
            "regex walks: {}",
            result.output
        );
        assert!(result.output.contains("a.rs:1"));
    }

    #[tokio::test]
    async fn index_matches_walk_on_large_tree() {
        // Synthetic large tree (the feature's target environment): the index
        // engine must report the same totals as the walk engine.
        let dir = tempdir().unwrap();
        for i in 0..2000 {
            let content = if i % 50 == 0 {
                "the needle_fn marker\n"
            } else {
                "fn other() {}\n"
            };
            std::fs::write(dir.path().join(format!("f{i}.rs")), content).unwrap();
        }
        let indexed = make_indexed_tool(dir.path());
        let r_index = indexed
            .execute(json!({"pattern": "needle_fn", "literal": true}))
            .await;
        assert!(r_index.success, "{}", r_index.output);
        assert!(
            r_index.output.contains("engine: index"),
            "{}",
            r_index.output
        );
        assert!(
            r_index.output.contains("40 matches in 40 files"),
            "index totals: {}",
            r_index.output
        );
        let walker = make_tool(dir.path());
        let r_walk = walker
            .execute(json!({"pattern": "needle_fn", "literal": true}))
            .await;
        assert!(r_walk.success, "{}", r_walk.output);
        assert!(r_walk.output.contains("engine: walk"), "{}", r_walk.output);
        assert!(
            r_walk.output.contains("40 matches in 40 files"),
            "walk totals: {}",
            r_walk.output
        );
    }

    #[tokio::test]
    async fn skips_ignored_dirs() {
        // Build output + deps must be skipped, even if they contain matches.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("src.rs"), "fn findme() {}").unwrap();
        // target/ — Rust build output.
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/built.rs"), "fn findme() {}").unwrap();
        // node_modules/ — JS deps.
        std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        std::fs::write(dir.path().join("node_modules/pkg/index.js"), "findme").unwrap();
        // .git/ — VCS.
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git/config"), "findme").unwrap();
        // .claude/ — Claude Code's own bookkeeping (plans, settings), not
        // project source. Must be skipped like the other ignored dirs.
        std::fs::create_dir_all(dir.path().join(".claude/plans")).unwrap();
        std::fs::write(dir.path().join(".claude/plans/old.md"), "findme").unwrap();

        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"pattern": "findme"})).await;
        assert!(result.success);
        // The source file matches.
        assert!(result.output.contains("src.rs"));
        // Build output + deps + VCS + .claude do NOT appear in results.
        assert!(!result.output.contains("target/built.rs"));
        assert!(!result.output.contains("node_modules"));
        assert!(!result.output.contains(".git"));
        assert!(!result.output.contains(".claude"));
        // The summary reports skipped files.
        assert!(result.output.contains("skipped"));
    }

    #[tokio::test]
    async fn skips_app_data_stores() {
        // Regression: the app's own SQLite stores under .coding/ (codegraph +
        // memory DBs and sidecars) must never be searched — their contents are
        // meaningless as results, and a DB whose bytes happen to be valid
        // UTF-8 would otherwise match (read_to_string only fails on invalid
        // UTF-8). Excluded deterministically so walk and index agree.
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding")).unwrap();
        std::fs::write(dir.path().join(".coding/codegraph.db"), "findme").unwrap();
        std::fs::write(dir.path().join(".coding/memory.db-wal"), "findme").unwrap();
        // A normal searchable file still matches — the guard is not a blanket
        // .coding/ exclusion.
        std::fs::write(dir.path().join("src.rs"), "findme").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"pattern": "findme"})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("src.rs"), "{}", result.output);
        assert!(
            !result.output.contains("codegraph.db") && !result.output.contains("memory.db"),
            "app data stores must never appear in results: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn caps_results_at_max_matches() {
        // A broad search producing more than MAX_MATCHES (100) matches should
        // be capped at MAX_MATCHES results, with an early-stop note appended.
        // With early-exit we stop reading/scanning further content once the
        // cap is reached, so the reported total may be close to (but not
        // necessarily exactly) the true count; the note is generic "and more".
        let dir = tempdir().unwrap();
        // 150 lines, each matching "findme".
        let content: String = (0..150)
            .map(|_| "findme".to_string())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.path().join("big.rs"), content).unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"pattern": "findme"})).await;
        assert!(result.success);
        // We collected at most MAX_MATCHES result lines.
        let match_lines: usize = result
            .output
            .lines()
            .filter(|l| l.contains("big.rs:"))
            .count();
        assert_eq!(match_lines, MAX_MATCHES);
        // The cap note is present (generic "and more" because of early stop).
        assert!(
            result.output.contains("more matches"),
            "expected cap note, got: {}",
            &result.output[..result.output.len().min(200)]
        );
        assert!(result.output.contains("narrow your pattern"));
        // Summary still mentions matches and files searched.
        assert!(result.output.contains("matches in "));
        assert!(result.output.contains("searched "));
    }

    #[tokio::test]
    async fn under_cap_not_truncated() {
        // Fewer than MAX_MATCHES — no cap note.
        let dir = tempdir().unwrap();
        let content: String = (0..10)
            .map(|_| "findme".to_string())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.path().join("small.rs"), content).unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"pattern": "findme"})).await;
        assert!(result.success);
        assert!(!result.output.contains("more matches"));
    }

    // --- A1: glob sandbox-escape guards -------------------------------------

    #[tokio::test]
    async fn glob_parent_traversal_rejected() {
        // A `..` glob must be rejected outright — never read files outside root.
        let dir = tempdir().unwrap();
        // Plant a secret file in the tempdir's PARENT so a naive glob would
        // match it if the traversal weren't blocked.
        let outside = dir.path().parent().unwrap().join("escape_a1_search.txt");
        std::fs::write(&outside, "secret-a1").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"pattern": "secret-a1", "glob": "../**/*.txt"}))
            .await;
        // The glob is rejected before any read — a clear tool error.
        assert!(!result.success);
        assert!(
            result.output.contains("parent-directory")
                || result.output.contains("relative to the project root"),
            "expected glob-rejection error, got: {}",
            result.output
        );
        assert!(!result.output.contains("secret-a1"));
        // Clean up the planted file.
        let _ = std::fs::remove_file(&outside);
    }

    #[tokio::test]
    async fn glob_absolute_path_rejected() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"pattern": "x", "glob": "/etc/**/*.txt"}))
            .await;
        assert!(!result.success);
        assert!(result.output.contains("relative to the project root"));
    }

    #[tokio::test]
    async fn glob_drive_prefix_rejected() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"pattern": "x", "glob": "C:/Windows/**/*.ini"}))
            .await;
        assert!(!result.success);
        assert!(result.output.contains("relative to the project root"));
    }

    #[tokio::test]
    async fn glob_unc_path_rejected() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"pattern": "x", "glob": "\\\\server\\share\\**"}))
            .await;
        assert!(!result.success);
        assert!(result.output.contains("relative to the project root"));
    }

    #[tokio::test]
    async fn glob_backslash_parent_traversal_rejected() {
        // Windows-style backslash traversal must also be caught.
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"pattern": "x", "glob": "..\\**\\*.txt"}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("parent-directory")
                || result.output.contains("relative to the project root"),
            "expected glob-rejection error, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn bare_identifier_symbol_searches_earn_the_graph_nudge() {
        // Backlog b804012f: grepping a symbol name DELEGATES — the graph
        // answers inline (definition + callers) and the walk is skipped.
        // The immediate repeat is the escape hatch: plain search results
        // with the advisory nudge riding above them (byte-pinned sentence).
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn hello() {}\nfn world() {}").unwrap();
        let tool = make_indexed_tool(dir.path());
        let r = tool.execute(json!({"pattern": "hello"})).await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.starts_with("AUTO-DELEGATED to the code graph — 'hello' is an indexed symbol"),
            "prepended: {}",
            r.output
        );
        assert!(r.output.contains("def: a.rs::hello::1"), "{}", r.output);
        assert!(
            r.output.contains("graph_context(id=\"a.rs::hello::1\")"),
            "the symbol id is embedded: {}",
            r.output
        );
        assert!(
            r.output.contains("re-issue this exact search"),
            "the escape line is present: {}",
            r.output
        );
        assert!(
            !r.output.contains("engine:"),
            "the file walk is skipped entirely: {}",
            r.output
        );

        // The immediate repeat escapes: plain search results, and the
        // advisory nudge rides above them.
        let r = tool.execute(json!({"pattern": "hello"})).await;
        assert!(r.success, "{}", r.output);
        assert!(r.output.starts_with("note:"), "prepended: {}", r.output);
        assert!(
            r.output.contains("'hello' is an indexed symbol"),
            "{}",
            r.output
        );
        // Byte-pin (2026-09-15 steering spec, review F2 of
        // 2026-12-06-symbol-intent-nudge-review): the FULL bare-identifier
        // sentence is exact, not just its substrings — a future rewording
        // of the tail must fail here rather than silently drift.
        assert!(
            r.output.contains(
                "'hello' is an indexed symbol — graph_context(id=\"a.rs::hello::1\") \
                 gives its definition + callers in one call; use graph_search/graph_context \
                 for symbol lookups — use search only for text"
            ),
            "the byte-pinned bare sentence must match exactly: {}",
            r.output
        );
        assert!(
            r.output.contains("a.rs:1"),
            "result rows are untouched: {}",
            r.output
        );

        // Literal mode delegates too (the escape key includes the literal
        // flag, so this is a fresh delegation, not an escape).
        let r = tool
            .execute(json!({"pattern": "hello", "literal": true}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.starts_with("AUTO-DELEGATED to the code graph"),
            "{}",
            r.output
        );

        // A glob narrows the search: the block PREPENDS above the (empty)
        // results instead of replacing them — "no content matches, but the
        // graph knows this symbol" is the most valuable case.
        let r = tool
            .execute(json!({"pattern": "hello", "glob": "**/*.ts"}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(r.output.contains("no matches found"), "{}", r.output);
        assert!(
            r.output.contains("AUTO-DELEGATED to the code graph"),
            "{}",
            r.output
        );
    }

    #[tokio::test]
    async fn non_identifier_patterns_never_nudge() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn hello() {}\nfn world() {}").unwrap();
        let tool = make_indexed_tool(dir.path());
        // Spaced, metachar'd, and unknown-identifier patterns: no note —
        // those are text searches, not symbol lookups. ("fn hello" used to
        // sit here as a negative; the definition-prefix widening
        // deliberately made it nudge — see
        // definition_prefixed_identifier_searches_earn_the_graph_nudge.)
        for pattern in ["hello\\(", "definitely_not_a_symbol", "fn hello world"] {
            let r = tool.execute(json!({"pattern": pattern})).await;
            assert!(r.success, "{}", r.output);
            assert!(
                !r.output.contains("is an indexed symbol"),
                "pattern {pattern:?} must not nudge: {}",
                r.output
            );
        }
        // No graph (codegraph opted out / absent) → no nudge, even though
        // `hello` IS a symbol in this fixture.
        let tool = make_tool(dir.path());
        let r = tool.execute(json!({"pattern": "hello"})).await;
        assert!(r.success, "{}", r.output);
        assert!(!r.output.contains("is an indexed symbol"), "{}", r.output);
    }

    #[tokio::test]
    async fn symbol_intent_patterns_earn_the_graph_nudge() {
        // Backlog 8b8f40d2 shapes — call-site hunting (`hello(`), `::`-paths,
        // and callers/who-calls phrasing — are symbol LOOKUPS even though
        // the pattern isn't a bare identifier. Backlog b804012f: the first
        // call DELEGATES (the graph answers inline); the escape repeat earns
        // the advisory nudge with the variant sentence ("the symbol 'X'
        // (from pattern '…')").
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn hello() {}\nfn world() {}").unwrap();
        let tool = make_indexed_tool(dir.path());
        for pattern in [
            "hello(",                 // call-site hunt
            "who calls hello",        // phrasing
            "callers of hello",       // phrasing
            "who uses hello",         // phrasing
            "where is hello defined", // phrasing + qualifier
            "a.rs::hello",            // ::-path (last segment hits)
        ] {
            let r = tool.execute(json!({"pattern": pattern})).await;
            assert!(r.success, "{}", r.output);
            assert!(
                r.output.starts_with("AUTO-DELEGATED to the code graph"),
                "pattern {pattern:?} must delegate: {}",
                r.output
            );
            assert!(
                r.output.contains("def: a.rs::hello::1"),
                "the symbol id is embedded for {pattern:?}: {}",
                r.output
            );
            // The escape repeat: plain search + the advisory nudge, via the
            // variant sentence that preserves the intent shape.
            let r = tool.execute(json!({"pattern": pattern})).await;
            assert!(r.success, "{}", r.output);
            let expected =
                format!("the symbol 'hello' (from pattern '{pattern}') is an indexed symbol");
            assert!(
                r.output.contains(&expected),
                "pattern {pattern:?} must nudge via the variant sentence: {}",
                r.output
            );
        }
    }

    #[tokio::test]
    async fn non_symbol_intent_patterns_stay_silent() {
        // Precision guards: a phrasing whose target is not a bare
        // identifier, an unindexed identifier, or prose stays a plain text
        // search — no nudge.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn hello() {}").unwrap();
        let tool = make_indexed_tool(dir.path());
        for pattern in [
            "who calls the shots",    // prose target — not an identifier
            "callers of hello world", // spaced target
            "not_a_symbol(",          // bare but unindexed
            "where is it defined",    // bare but unindexed
        ] {
            let r = tool.execute(json!({"pattern": pattern})).await;
            assert!(r.success, "{}", r.output);
            assert!(
                !r.output.contains("is an indexed symbol"),
                "pattern {pattern:?} must not nudge: {}",
                r.output
            );
        }
    }

    // --- literal-engine TIP (regex-mode walks the index could serve) ------

    #[tokio::test]
    async fn metachar_free_regex_walk_earns_the_literal_tip() {
        // A spaced text phrase: metachar-free and tokenizable, but NOT a
        // bare identifier, so the symbol nudge cannot fire — the TIP rides
        // line 1 while the walk engine (regex mode) stays authoritative.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "some phrase here\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        let r = tool.execute(json!({"pattern": "some phrase"})).await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output
                .starts_with("note: TIP: pattern has no regex metacharacters"),
            "TIP on line 1: {}",
            r.output
        );
        assert!(
            r.output
                .contains("literal:true would use the content-index engine"),
            "TIP names the switch: {}",
            r.output
        );
        assert!(
            r.output.contains("engine: walk"),
            "regex mode still walks: {}",
            r.output
        );
        assert!(
            r.output.contains("a.txt:1"),
            "result rows untouched: {}",
            r.output
        );
    }

    #[tokio::test]
    async fn regex_metachars_literal_mode_or_dead_index_never_earn_the_tip() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.txt"),
            "foo bar\nfoo|bar\nsearched\na.b\n-> x\n",
        )
        .unwrap();
        let tool = make_indexed_tool(dir.path());

        // Metachar-bearing patterns: regex semantics ≠ literal semantics —
        // pointing them at the index would silently change matches (D1);
        // no TIP.
        for pattern in ["foo|bar", "search(ed)?", "a.b"] {
            let r = tool.execute(json!({"pattern": pattern})).await;
            assert!(r.success, "{}", r.output);
            assert!(
                !r.output
                    .contains("TIP: pattern has no regex metacharacters"),
                "pattern {pattern:?} must not earn the TIP: {}",
                r.output
            );
        }
        // Punctuation-only pattern: zero FTS tokens — the index cannot
        // serve it, so literal:true would still walk. No TIP.
        let r = tool.execute(json!({"pattern": "->"})).await;
        assert!(r.success, "{}", r.output);
        assert!(!r.output.contains("TIP: pattern"), "{}", r.output);
        // literal:true is already index-routed — the TIP would be noise.
        let r = tool
            .execute(json!({"pattern": "foo bar", "literal": true}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(!r.output.contains("TIP: pattern"), "{}", r.output);
        // No graph → no index to point at.
        let walker = make_tool(dir.path());
        let r = walker.execute(json!({"pattern": "foo bar"})).await;
        assert!(r.success, "{}", r.output);
        assert!(!r.output.contains("TIP: pattern"), "{}", r.output);
        // Graph present but never indexed → nothing to switch to.
        let graph = crate::codegraph::CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        let unindexed = SearchTool::new(
            Sandbox::new(dir.path()).unwrap(),
            Some(std::sync::Arc::new(graph)),
        );
        let r = unindexed.execute(json!({"pattern": "foo bar"})).await;
        assert!(r.success, "{}", r.output);
        assert!(!r.output.contains("TIP: pattern"), "{}", r.output);
    }

    #[tokio::test]
    async fn symbol_nudge_suppresses_the_literal_tip() {
        // A bare identifier naming an indexed symbol is eligible for BOTH
        // notes; symbol steering wins — the nudge fires, the TIP is absent
        // (one note, not two).
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn hello() {}\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        let r = tool.execute(json!({"pattern": "hello"})).await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.contains("'hello' is an indexed symbol"),
            "nudge present: {}",
            r.output
        );
        assert!(
            !r.output
                .contains("TIP: pattern has no regex metacharacters"),
            "TIP suppressed under the nudge: {}",
            r.output
        );
    }

    // --- definition-prefixed symbol nudge (fn X, pub struct X, …) --------

    #[tokio::test]
    async fn definition_prefixed_identifier_searches_earn_the_graph_nudge() {
        // Backlog b804012f: definition-prefixed hunts delegate on first use
        // (the graph answers inline); the escape repeat earns the advisory
        // nudge with the variant sentence.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn search_content() {}\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        for pattern in [
            "fn search_content",
            "pub fn search_content",
            "pub(crate) fn search_content",
            "async fn search_content",
        ] {
            let r = tool.execute(json!({"pattern": pattern})).await;
            assert!(r.success, "{}", r.output);
            assert!(
                r.output.starts_with(
                    "AUTO-DELEGATED to the code graph — 'search_content' is an indexed symbol"
                ),
                "pattern {pattern:?} delegates: {}",
                r.output
            );
            assert!(
                r.output.contains("def: a.rs::search_content::1"),
                "symbol id embedded for {pattern:?}: {}",
                r.output
            );
            let r = tool.execute(json!({"pattern": pattern})).await;
            assert!(r.success, "{}", r.output);
            assert!(
                r.output
                    .contains("the symbol 'search_content' (from pattern"),
                "pattern {pattern:?} nudges on the escape repeat: {}",
                r.output
            );
        }
        // struct/enum forms too (fresh fixture: struct Bar, enum E).
        std::fs::write(dir.path().join("b.rs"), "struct Bar;\nenum E { A }\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        for pattern in ["struct Bar", "enum E"] {
            let r = tool.execute(json!({"pattern": pattern})).await;
            assert!(r.success, "{}", r.output);
            assert!(
                r.output.starts_with("AUTO-DELEGATED to the code graph"),
                "{pattern:?} delegates: {}",
                r.output
            );
        }
    }

    #[tokio::test]
    async fn non_symbol_prefixed_patterns_never_nudge() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn hello() {}\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        for pattern in [
            "fn hello world",       // two words after the keyword
            "fn hello()",           // trailing parens — a call site, not a name
            "impl Hello for World", // impl … for … spans two names
            "function",             // bare keyword, no name
            "fn zzz",               // definition-shaped but NOT an indexed symbol
        ] {
            let r = tool.execute(json!({"pattern": pattern})).await;
            assert!(r.success, "{}", r.output);
            assert!(
                !r.output.contains("is an indexed symbol"),
                "pattern {pattern:?} must not nudge: {}",
                r.output
            );
        }
    }

    #[tokio::test]
    async fn widened_nudge_still_suppresses_the_literal_tip() {
        // 'fn hello' is metachar-free (literal-eligible) AND names an
        // indexed symbol — the delegated answer subsumes the TIP on first
        // use, and the escape repeat's advisory nudge suppresses it too.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn hello() {}\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        let r = tool.execute(json!({"pattern": "fn hello"})).await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.starts_with("AUTO-DELEGATED to the code graph"),
            "{}",
            r.output
        );
        assert!(
            !r.output
                .contains("TIP: pattern has no regex metacharacters"),
            "TIP suppressed under the delegated answer: {}",
            r.output
        );
        let r = tool.execute(json!({"pattern": "fn hello"})).await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output
                .contains("the symbol 'hello' (from pattern 'fn hello') is an indexed symbol"),
            "{}",
            r.output
        );
        assert!(
            !r.output
                .contains("TIP: pattern has no regex metacharacters"),
            "TIP suppressed under the widened nudge: {}",
            r.output
        );
    }

    // --- pruned walk (ignored dirs are never enumerated) ------------------

    #[tokio::test]
    async fn pruned_walk_matches_the_index_engine_and_never_searches_ignored_dirs() {
        // Equivalence pin: over a tree WITH ignored dirs (node_modules,
        // target, dist), the pruned walk engine reports the same totals as
        // the index engine (whose coverage walker prunes the same dirs),
        // and a needle planted inside an ignored dir never matches.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("src.rs"), "the findme marker\n").unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        std::fs::write(dir.path().join("node_modules/pkg/x.js"), "findme").unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/built.rs"), "findme").unwrap();
        std::fs::create_dir_all(dir.path().join("dist")).unwrap();
        std::fs::write(dir.path().join("dist/out.js"), "findme").unwrap();

        let walker = make_tool(dir.path());
        let r_walk = walker
            .execute(json!({"pattern": "findme", "literal": true}))
            .await;
        assert!(r_walk.success, "{}", r_walk.output);
        assert!(
            r_walk.output.contains("1 matches in 1 files"),
            "ignored-dir needles must not match: {}",
            r_walk.output
        );
        assert!(
            r_walk.output.contains("skipped 3 ignored dirs"),
            "pruned dirs are counted: {}",
            r_walk.output
        );
        assert!(r_walk.output.contains("src.rs:1"), "{}", r_walk.output);
        assert!(
            !r_walk.output.contains("node_modules") && !r_walk.output.contains("built.rs"),
            "{}",
            r_walk.output
        );

        let indexed = make_indexed_tool(dir.path());
        let r_index = indexed
            .execute(json!({"pattern": "findme", "literal": true}))
            .await;
        assert!(r_index.success, "{}", r_index.output);
        assert!(
            r_index.output.contains("1 matches in 1 files"),
            "index totals agree with the pruned walk: {}",
            r_index.output
        );
    }

    /// F7: a bare `**` tail is a SUPPORTED shape (glob 0.3.4 matches it) —
    /// guards the corrected knowledge-file claim (the old "bare ** tails
    /// match 0 files" note did not reproduce; probe evidence 2026-09-17).
    #[test]
    fn walk_searchable_honors_bare_recursive_tails() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("frontend/src/sub")).unwrap();
        std::fs::create_dir_all(dir.path().join("other")).unwrap();
        std::fs::write(dir.path().join("frontend/src/a.ts"), "x\n").unwrap();
        std::fs::write(dir.path().join("frontend/src/sub/b.tsx"), "x\n").unwrap();
        std::fs::write(dir.path().join("other/c.rs"), "x\n").unwrap();
        let (files, _) = walk_searchable(dir.path(), Some("frontend/src/**")).unwrap();
        let mut rels: Vec<String> = files
            .iter()
            .map(|p| {
                p.strip_prefix(dir.path())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        rels.sort();
        assert_eq!(
            rels,
            vec!["frontend/src/a.ts", "frontend/src/sub/b.tsx"],
            "bare ** tail matches the subtree"
        );
    }

    /// F7: brace alternation must expand into alternatives — the glob crate
    /// has none, so `{ts,tsx}` would otherwise match only literal braces
    /// (i.e. nothing) and the tool would silently answer "searched 0 files".
    #[test]
    fn walk_searchable_expands_brace_alternation() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("frontend/src/sub")).unwrap();
        std::fs::create_dir_all(dir.path().join("other")).unwrap();
        std::fs::write(dir.path().join("frontend/src/a.ts"), "x\n").unwrap();
        std::fs::write(dir.path().join("frontend/src/sub/b.tsx"), "x\n").unwrap();
        std::fs::write(dir.path().join("frontend/src/c.css"), "x\n").unwrap();
        std::fs::write(dir.path().join("other/d.rs"), "x\n").unwrap();
        let (files, _) = walk_searchable(dir.path(), Some("**/*.{ts,tsx}")).unwrap();
        let mut rels: Vec<String> = files
            .iter()
            .map(|p| {
                p.strip_prefix(dir.path())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        rels.sort();
        assert_eq!(
            rels,
            vec!["frontend/src/a.ts", "frontend/src/sub/b.tsx"],
            "brace alternation spans both extensions"
        );
        // Dir-scoped braces pick exactly the listed extensions.
        let (files, _) = walk_searchable(dir.path(), Some("frontend/src/*.{ts,css}")).unwrap();
        assert_eq!(files.len(), 2, "ts + css, not the nested tsx");
    }

    /// Backlog-41f39672 verification round: the DIR-SCOPED RECURSIVE brace
    /// shape (`frontend/src/**/*.{ts,tsx}`) — the F7 tests covered
    /// root-level braces and single-star dir scopes, but this shape walked
    /// 0 files live (2026-09-17, the real tree). Must match direct
    /// children AND nested files across both extensions.
    #[test]
    fn walk_searchable_dir_scoped_recursive_braces() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("frontend/src/hooks")).unwrap();
        std::fs::create_dir_all(dir.path().join("frontend/src/deep")).unwrap();
        std::fs::create_dir_all(dir.path().join("other")).unwrap();
        std::fs::write(dir.path().join("frontend/src/a.ts"), "x\n").unwrap();
        std::fs::write(dir.path().join("frontend/src/hooks/b.tsx"), "x\n").unwrap();
        std::fs::write(dir.path().join("frontend/src/deep/c.ts"), "x\n").unwrap();
        std::fs::write(dir.path().join("other/d.txt"), "x\n").unwrap();
        let (files, _) = walk_searchable(dir.path(), Some("frontend/src/**/*.{ts,tsx}")).unwrap();
        let mut rels: Vec<String> = files
            .iter()
            .map(|p| {
                p.strip_prefix(dir.path())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        rels.sort();
        assert_eq!(
            rels,
            vec![
                "frontend/src/a.ts",
                "frontend/src/deep/c.ts",
                "frontend/src/hooks/b.tsx"
            ],
            "dir-scoped recursive braces match direct children and nested files"
        );
    }

    #[tokio::test]
    async fn known_backlog_id_earns_the_memory_note() {
        // F9: a uuid-shaped pattern that a row knows earns the fired-only
        // known-memory-hit note; other patterns never carry it.
        let dir = tempdir().unwrap();
        let store: std::sync::Arc<dyn crate::memory::MemoryStoreTrait> = {
            let embedder: std::sync::Arc<dyn crate::memory::Embedder> =
                std::sync::Arc::new(crate::memory::embedder::HashEmbedder::new());
            std::sync::Arc::new(crate::memory::MemoryStore::open_in_memory(embedder).unwrap())
        };
        store
            .write(crate::memory::Memory::new(
                crate::memory::MemoryTier::Semantic,
                "PLAN: backlog 70f5b248-0b3b-4e90-88c1-b840c5323a83 — findings round",
                "queued work",
                1,
            ))
            .await
            .unwrap();
        let tool = SearchTool::new(Sandbox::new(dir.path()).unwrap(), None)
            .with_memory(std::sync::Arc::clone(&store));
        let r = tool
            .execute(json!({
                "pattern": "70f5b248-0b3b-4e90-88c1-b840c5323a83",
                "literal": true
            }))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(r.output.contains("known memory hit:"), "{}", r.output);
        assert!(
            r.output.contains("memory_search it for detail"),
            "{}",
            r.output
        );

        // Non-uuid patterns (short hex sha, prose) never carry the note —
        // the store is not even consulted.
        for pattern in ["abc1234", "hello world"] {
            let r = tool
                .execute(json!({ "pattern": pattern, "literal": true }))
                .await;
            assert!(r.success, "{}", r.output);
            assert!(!r.output.contains("known memory hit"), "{}", r.output);
        }
    }

    #[tokio::test]
    async fn memory_delegation_fires_on_knowledge_glob_or_typed_prefix() {
        // Backlog b804012f: a query targeting the memory store (knowledge/
        // reviews glob or typed prefix) gets the recall answer inline; a
        // plain pattern never delegates and never touches the store.
        let store: std::sync::Arc<dyn crate::memory::MemoryStoreTrait> = {
            let embedder: std::sync::Arc<dyn crate::memory::Embedder> =
                std::sync::Arc::new(crate::memory::embedder::HashEmbedder::new());
            std::sync::Arc::new(crate::memory::MemoryStore::open_in_memory(embedder).unwrap())
        };
        store
            .write(crate::memory::Memory::new(
                crate::memory::MemoryTier::Semantic,
                "DECISION: Tool cards are frameless by design",
                "Tool invocation cards in the agent chat are frameless; state rides text color.",
                1,
            ))
            .await
            .unwrap();
        let block = memory_delegation_block(&store, "frameless", Some(".coding/knowledge/**"))
            .await
            .expect("knowledge glob delegates");
        assert!(block.starts_with("AUTO-DELEGATED to memory"), "{block}");
        assert!(block.contains("re-issue this exact search"), "{block}");
        assert!(
            block.contains("DECISION: Tool cards are frameless by design"),
            "{block}"
        );
        let block = memory_delegation_block(&store, "DECISION: frameless", None)
            .await
            .expect("typed prefix delegates");
        assert!(block.contains("AUTO-DELEGATED to memory"), "{block}");
        // Plain patterns — no knowledge/reviews glob, no typed prefix — are
        // content searches and never delegate.
        assert!(
            memory_delegation_block(&store, "frameless", None)
                .await
                .is_none()
        );
        assert!(
            memory_delegation_block(&store, "frameless", Some("src/**/*.rs"))
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn memory_targeted_query_delegates_and_skips_the_walk() {
        // Backlog b804012f, execute level: a knowledge-dir glob gets the
        // recall answer inline — the walk is skipped — and the immediate
        // repeat escapes to the plain search.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn hello() {}\n").unwrap();
        let store: std::sync::Arc<dyn crate::memory::MemoryStoreTrait> = {
            let embedder: std::sync::Arc<dyn crate::memory::Embedder> =
                std::sync::Arc::new(crate::memory::embedder::HashEmbedder::new());
            std::sync::Arc::new(crate::memory::MemoryStore::open_in_memory(embedder).unwrap())
        };
        store
            .write(crate::memory::Memory::new(
                crate::memory::MemoryTier::Semantic,
                "DECISION: Tool cards are frameless by design",
                "Tool invocation cards in the agent chat are frameless; state rides text color.",
                1,
            ))
            .await
            .unwrap();
        let tool = SearchTool::new(Sandbox::new(dir.path()).unwrap(), None)
            .with_memory(std::sync::Arc::clone(&store));
        let r = tool
            .execute(json!({"pattern": "frameless", "glob": ".coding/knowledge/**"}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.starts_with("AUTO-DELEGATED to memory"),
            "prepended: {}",
            r.output
        );
        assert!(
            r.output.contains("DECISION: Tool cards are frameless by design"),
            "{}",
            r.output
        );
        assert!(
            r.output.contains("re-issue this exact search"),
            "the escape line is present: {}",
            r.output
        );
        assert!(
            !r.output.contains("engine:"),
            "the file walk is skipped entirely: {}",
            r.output
        );

        // The immediate repeat escapes: the plain search runs (the glob
        // matches nothing in this fixture) and no block rides it.
        let r = tool
            .execute(json!({"pattern": "frameless", "glob": ".coding/knowledge/**"}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            !r.output.contains("AUTO-DELEGATED"),
            "the escape repeat is a plain search: {}",
            r.output
        );
        assert!(
            r.output.contains("no matches found"),
            "the walk ran: {}",
            r.output
        );
    }

    #[tokio::test]
    async fn memory_recall_miss_falls_through_to_the_plain_search() {
        // Backlog b804012f: an empty recall (or any store hiccup) is NOT a
        // delegation — the plain search runs untouched.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn hello() {}\n").unwrap();
        let store: std::sync::Arc<dyn crate::memory::MemoryStoreTrait> = {
            let embedder: std::sync::Arc<dyn crate::memory::Embedder> =
                std::sync::Arc::new(crate::memory::embedder::HashEmbedder::new());
            std::sync::Arc::new(crate::memory::MemoryStore::open_in_memory(embedder).unwrap())
        };
        let tool = SearchTool::new(Sandbox::new(dir.path()).unwrap(), None)
            .with_memory(std::sync::Arc::clone(&store));
        for args in [
            json!({"pattern": "frameless", "glob": ".coding/knowledge/**"}),
            json!({"pattern": "SPEC: nothing like this in the store"}),
        ] {
            let r = tool.execute(args).await;
            assert!(r.success, "{}", r.output);
            assert!(
                !r.output.contains("AUTO-DELEGATED"),
                "an empty recall never delegates: {}",
                r.output
            );
            assert!(
                r.output.contains("no matches found"),
                "the plain search ran: {}",
                r.output
            );
        }
    }

    #[tokio::test]
    async fn exact_alternation_hunt_delegates_per_branch() {
        // Backlog b804012f: an alternation whose branches ALL resolve
        // exactly delegates with one def line per branch; the escape repeat
        // falls back to the advisory absorption note.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn hello() {}\nfn world() {}").unwrap();
        let tool = make_indexed_tool(dir.path());
        let r = tool.execute(json!({"pattern": "hello|world"})).await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.starts_with(
                "AUTO-DELEGATED to the code graph — each hit below is an indexed symbol"
            ),
            "prepended: {}",
            r.output
        );
        assert!(
            r.output.contains("'hello' → a.rs::hello::1"),
            "{}",
            r.output
        );
        assert!(
            r.output.contains("'world' → a.rs::world::2"),
            "{}",
            r.output
        );
        assert!(
            r.output.contains("pass one id to graph_context"),
            "{}",
            r.output
        );
        assert!(
            !r.output.contains("engine:"),
            "the file walk is skipped entirely: {}",
            r.output
        );

        // The escape repeat: plain search + the advisory absorption note.
        let r = tool.execute(json!({"pattern": "hello|world"})).await;
        assert!(r.success, "{}", r.output);
        assert!(r.output.starts_with("note:"), "prepended: {}", r.output);
        assert!(r.output.contains("an indexed symbol"), "{}", r.output);
        assert!(r.output.contains("a.rs:1"), "results: {}", r.output);
    }

    #[tokio::test]
    async fn glob_narrowed_delegation_escapes_on_the_repeat() {
        // H1 (review 2026-12-28): a symbol hunt WITH a glob prepends the
        // block above the results — and the key is recorded on that path
        // too, so the promised escape engages: the immediate repeat runs
        // the plain glob-narrowed search with no block.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn hello() {}\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        let r = tool
            .execute(json!({"pattern": "hello", "glob": "**/*.rs"}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.contains("AUTO-DELEGATED to the code graph"),
            "prepended: {}",
            r.output
        );
        assert!(r.output.contains("a.rs:1"), "results ride below: {}", r.output);
        // The escape repeat: plain glob-narrowed search, no block.
        let r = tool
            .execute(json!({"pattern": "hello", "glob": "**/*.rs"}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            !r.output.contains("AUTO-DELEGATED"),
            "the escape repeat is a plain search: {}",
            r.output
        );
        assert!(r.output.contains("a.rs:1"), "results: {}", r.output);
    }


    #[tokio::test]
    async fn delegation_escape_survives_interleaved_queries() {
        // Backlog (2026-12-29 session, plan 72329f2c): the documented escape
        // — re-issue the identical search — auto-delegated AGAIN (observed
        // twice), because the single escape key was replaced by any
        // interleaved delegating query. The bypass must be sticky per exact
        // query: once a query has delegated, its identical re-issues NEVER
        // re-delegate in-session, no matter what else delegated in between.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn hello() {}\nfn world() {}").unwrap();
        let tool = make_indexed_tool(dir.path());
        // A delegates.
        let r = tool.execute(json!({"pattern": "hello"})).await;
        assert!(r.output.starts_with("AUTO-DELEGATED"), "{}", r.output);
        // B (a different delegating query) delegates.
        let r = tool.execute(json!({"pattern": "world"})).await;
        assert!(r.output.starts_with("AUTO-DELEGATED"), "{}", r.output);
        // A's re-issue must ESCAPE (currently: delegates again — the key was
        // replaced by B).
        let r = tool.execute(json!({"pattern": "hello"})).await;
        assert!(
            !r.output.contains("AUTO-DELEGATED"),
            "A's re-issue escapes even after B delegated in between: {}",
            r.output
        );
        assert!(r.output.contains("a.rs:1"), "plain results: {}", r.output);
        // B's re-issue escapes too — both keys stay bypassed.
        let r = tool.execute(json!({"pattern": "world"})).await;
        assert!(
            !r.output.contains("AUTO-DELEGATED"),
            "B's re-issue also escapes: {}",
            r.output
        );
    }

    #[tokio::test]
    async fn walk_cap_narrower_reissue_returns_tail_files() {
        // Backlog (2026-12-29 session): a '## Verdict:' listing matched 100
        // files with "and more matches" — the tail was hidden. Pin: the cap
        // never PERMANENTLY hides files; a narrower re-issue returns the
        // previously hidden tail. (120 files, no index → the walk path's
        // cap.)
        let dir = tempdir().unwrap();
        for i in 0..120 {
            std::fs::write(
                dir.path().join(format!("f{i:03}.md")),
                "## Verdict: tail-marker\n",
            )
            .unwrap();
        }
        let tool = make_tool(dir.path());
        let r = tool
            .execute(json!({"pattern": "## Verdict:", "literal": true}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.contains("and more matches"),
            "the cap note fires at 100+: {}",
            r.output
        );
        assert!(
            !r.output.contains("f119"),
            "beyond the cap, the tail file is hidden: {}",
            r.output
        );
        // Narrower re-issue: only the tail files match — all returned.
        let r = tool
            .execute(json!({"pattern": "tail-marker", "literal": true, "glob": "**/f1*.md"}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.contains("f119"),
            "the narrower re-issue returns the previously hidden tail: {}",
            r.output
        );
    }

    #[tokio::test]
    async fn literal_metachar_zero_hits_get_the_retry_hint() {
        // C2: a literal-mode search over a regex-shaped pattern that finds
        // nothing is the signature of a mode mistake — say so. Non-empty
        // results and regex-mode misses never carry the hint.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x alpha (beta) y\nalpha beta\n").unwrap();
        let tool = make_tool(dir.path());
        let r = tool
            .execute(json!({"pattern": "(a|b)gamma", "literal": true}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.contains("retry with literal:false"),
            "{}",
            r.output
        );

        // The same metachar pattern matching content → no hint (an
        // intentional literal metachar search is fine).
        let r = tool
            .execute(json!({"pattern": "alpha (beta)", "literal": true}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            !r.output.contains("retry with literal:false"),
            "{}",
            r.output
        );

        // Regex-mode zero hits → no hint (the mode was already regex).
        let r = tool.execute(json!({"pattern": "(a|b)gamma"})).await;
        assert!(r.success, "{}", r.output);
        assert!(
            !r.output.contains("retry with literal:false"),
            "{}",
            r.output
        );

        // Literal plain word zero hits → no hint.
        let r = tool
            .execute(json!({"pattern": "zzznotfound", "literal": true}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            !r.output.contains("retry with literal:false"),
            "{}",
            r.output
        );
    }

    #[tokio::test]
    async fn alternation_symbol_hunts_absorb_the_graph_lookup() {
        // C1 (F14): "fn .*watcher|struct.*Watcher"-shaped patterns are
        // symbol hunts in disguise — branches that resolve to indexed
        // symbols are absorbed into the note with their ids (loose resolve:
        // partial names count), one call instead of a grep + N graph
        // lookups. A text branch rejects the whole alternation; nothing
        // resolving stays silent.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "struct GraphWatcher {}\nstruct Watchtower {}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("b.rs"), "async fn update_index() {}\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        let r = tool
            .execute(json!({"pattern": "fn .*update|struct.*Watcher"}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(r.output.starts_with("note:"), "prepended: {}", r.output);
        assert!(
            r.output
                .contains("'Watcher' resolves to 'GraphWatcher', an indexed symbol"),
            "{}",
            r.output
        );
        assert!(
            r.output
                .contains("'update' resolves to 'update_index', an indexed symbol"),
            "the partial-name branch resolved: {}",
            r.output
        );
        assert!(r.output.contains("graph_context(id="), "{}", r.output);
        assert!(r.output.contains("alternation pattern"), "{}", r.output);

        // A text branch ("hello world") rejects the whole alternation.
        let r = tool
            .execute(json!({"pattern": "watcher|hello world"}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(!r.output.contains("is an indexed symbol"), "{}", r.output);

        // No branch resolves — nothing to absorb, no nudge.
        let r = tool
            .execute(json!({"pattern": ".*zzznope|.*yyynope"}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(!r.output.contains("is an indexed symbol"), "{}", r.output);
    }

    #[tokio::test]
    async fn stale_index_reindexes_and_serves_fresh_index_results() {
        // F10: the watcher's reindex lags edits — an index hit whose file
        // mtime moved past the as-of-index mtime must NOT serve pre-edit
        // content. A small staleness (≤ STALE_REINDEX_CAP files) is
        // repaired INLINE: the stale files are re-indexed, the query is
        // re-run from the fresh index, and the side effect is disclosed
        // with a single "note: " prefix (the doubled "note: note:" bug,
        // user request 2026-12-30).
        let dir = tempdir().unwrap();
        let path = dir.path().join("readme.md");
        std::fs::write(&path, "old bullet list\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        // Simulate the edit→watcher gap: rewrite + bump mtime, no reindex.
        std::fs::write(&path, "old bullet list\nnew bullet (fresh)\n").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let r = tool
            .execute(json!({"pattern": "bullet", "literal": true}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.contains("reindexed 1 stale file(s)"),
            "the inline re-index is disclosed: {}",
            r.output
        );
        assert!(r.output.contains("new bullet (fresh)"), "{}", r.output);
        assert!(r.output.contains("engine: index"), "{}", r.output);
        assert!(
            !r.output.contains("note: note:"),
            "single note prefix: {}",
            r.output
        );
        assert!(
            r.output.starts_with("note: "),
            "the disclosure rides above the results: {}",
            r.output
        );
    }

    #[tokio::test]
    async fn stale_file_beyond_the_display_cap_still_surfaces() {
        // Review 2026-09-17 verify-round L1: a stale file whose matches rank
        // BEYOND the displayed page (feeding only the match counts) must
        // still trip the freshness check — the scan covers every
        // glob-passing file on the fetched page, not just the top
        // MAX_MATCHES lines. This test fails on the hits-scoped pre-fix
        // scan, which served the stale counts silently. Since the inline
        // re-index (F10) the trip now REPAIRS the file: the counts are
        // served fresh from the index (the weak file itself ranks below
        // the 100 displayed hits, so its line is not shown).
        let dir = tempdir().unwrap();
        // 100 strong-match files (3× term frequency, tiny bodies) rank
        // above the weak file everywhere; the display cap is MAX_MATCHES
        // (100), so the weak file is fetched and counted but not shown.
        for i in 0..100 {
            std::fs::write(
                dir.path().join(format!("s{i:03}.md")),
                "needle needle needle\n",
            )
            .unwrap();
        }
        // The weak match: a single occurrence buried in a long body.
        let weak = dir.path().join("a-stale-tail.md");
        let filler = "filler ".repeat(40);
        std::fs::write(&weak, format!("{filler}needle tail-v1\n")).unwrap();
        let tool = make_indexed_tool(dir.path());
        // Edit AFTER indexing + bump mtime past the as-of-index stamp —
        // the edit→watcher gap.
        std::fs::write(&weak, format!("{filler}needle tail-refreshed\n")).unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&weak)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let r = tool
            .execute(json!({"pattern": "needle", "literal": true}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.contains("reindexed 1 stale file(s)"),
            "the beyond-cap stale file must trip the freshness check: {}",
            r.output
        );
        assert!(r.output.contains("engine: index"), "{}", r.output);
        assert!(
            r.output.contains("101 matches in 101 files"),
            "the counts include the repaired weak file: {}",
            r.output
        );
    }

    #[tokio::test]
    async fn stale_above_the_reindex_cap_walks() {
        // A checkout-scale staleness (more than STALE_REINDEX_CAP files)
        // must NOT be repaired inline — parsing dozens of files inside a
        // tool call would blow the latency budget. The walk stays the
        // answer and the staleness is surfaced with a single "note: "
        // prefix (the doubled "note: note:" bug, user request 2026-12-30).
        let dir = tempdir().unwrap();
        let cap = STALE_REINDEX_CAP;
        for i in 0..=cap {
            std::fs::write(dir.path().join(format!("stale{i:02}.md")), "needle v1\n").unwrap();
        }
        std::fs::write(dir.path().join("steady.md"), "needle steady\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        // Edit every stale file AFTER indexing + bump mtimes — the
        // edit→watcher gap at checkout scale.
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        for i in 0..=cap {
            let path = dir.path().join(format!("stale{i:02}.md"));
            std::fs::write(&path, "needle v2\n").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(later)
                .unwrap();
        }
        let r = tool
            .execute(json!({"pattern": "needle", "literal": true}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(r.output.contains("engine: walk"), "{}", r.output);
        assert!(
            r.output.contains(&format!("index stale for {} file(s)", cap + 1)),
            "the staleness is surfaced: {}",
            r.output
        );
        assert!(
            !r.output.contains("note: note:"),
            "single note prefix: {}",
            r.output
        );
        // The walk serves the fresh content.
        assert!(r.output.contains("needle v2"), "{}", r.output);
        assert!(r.output.contains("needle steady"), "{}", r.output);
    }

    #[tokio::test]
    async fn stale_while_an_index_pass_runs_walks() {
        // Watcher coordination: while an index pass is running (the
        // indexing flag set), the inline re-index must not run — and must
        // not clear someone else's flag. The tool walks and surfaces the
        // staleness; the running pass refreshes the index shortly.
        let dir = tempdir().unwrap();
        let path = dir.path().join("readme.md");
        std::fs::write(&path, "old marker line\n").unwrap();
        let graph = crate::codegraph::CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        let graph = std::sync::Arc::new(graph);
        let tool = SearchTool::new(
            Sandbox::new(dir.path()).unwrap(),
            Some(std::sync::Arc::clone(&graph)),
        );
        // Simulate the edit→watcher gap, then a pass "running".
        std::fs::write(&path, "old marker line\nfresh marker line\n").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        graph.set_indexing(true); // a pass is running
        let r = tool
            .execute(json!({"pattern": "marker", "literal": true}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(r.output.contains("engine: walk"), "{}", r.output);
        assert!(
            r.output.contains("index stale for 1 file(s)"),
            "staleness surfaced: {}",
            r.output
        );
        assert!(
            !r.output.contains("reindexed"),
            "no inline re-index while a pass runs: {}",
            r.output
        );
        assert!(
            !r.output.contains("note: note:"),
            "single note prefix: {}",
            r.output
        );
        assert!(
            graph.is_indexing(),
            "the inline path must not clear someone else's flag"
        );
        graph.set_indexing(false); // clean up
    }

    #[tokio::test]
    async fn no_content_matches_falls_back_to_filename_matches() {
        // F2: a plan file never contains its own id — when the content
        // search finds nothing, path matches are surfaced instead.
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding/plans")).unwrap();
        std::fs::write(dir.path().join(".coding/plans/812e1f0a.md"), "step one\n").unwrap();
        let tool = make_tool(dir.path());
        let r = tool
            .execute(json!({"pattern": "812e1f0a", "literal": true}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.contains("file path(s) match the pattern"),
            "{}",
            r.output
        );
        assert!(
            r.output.contains(".coding/plans/812e1f0a.md"),
            "{}",
            r.output
        );

        // A pattern matching neither content nor paths stays a plain miss.
        let r = tool
            .execute(json!({"pattern": "zzz-not-anywhere", "literal": true}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(!r.output.contains("file path(s) match"), "{}", r.output);
    }

    #[tokio::test]
    async fn brace_glob_works_on_the_index_path() {
        // F7: the FTS engine post-filters through the same expanded globs —
        // a literal query with a brace glob matches both extensions and
        // none of the unlisted ones.
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.ts"), "needle here\n").unwrap();
        std::fs::write(dir.path().join("src/b.tsx"), "needle here\n").unwrap();
        std::fs::write(dir.path().join("src/c.css"), "needle here\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        let r = tool
            .execute(json!({
                "pattern": "needle",
                "literal": true,
                "glob": "src/*.{ts,tsx}"
            }))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(r.output.contains("engine: index"), "{}", r.output);
        assert!(r.output.contains("src/a.ts"), "{}", r.output);
        assert!(r.output.contains("src/b.tsx"), "{}", r.output);
        assert!(!r.output.contains("src/c.css"), "{}", r.output);
    }

    #[tokio::test]
    async fn pruned_walk_is_deterministic_and_honors_the_glob() {
        let dir = tempdir().unwrap();
        for name in ["z.rs", "a.rs", "m.rs"] {
            std::fs::write(dir.path().join(name), "findme\n").unwrap();
        }
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/nested.rs"), "findme\n").unwrap();
        std::fs::write(dir.path().join("sub/nested.txt"), "findme\n").unwrap();
        let tool = make_tool(dir.path());
        let r1 = tool.execute(json!({"pattern": "findme"})).await;
        let r2 = tool.execute(json!({"pattern": "findme"})).await;
        assert!(r1.success && r2.success, "{}", r1.output);
        assert_eq!(r1.output, r2.output, "two walks, identical order");
        // Match-line order is per-directory sorted depth-first (glob-crate
        // parity): a.rs, m.rs, sub/nested.rs, sub/nested.txt, z.rs.
        let lines: Vec<&str> = r1.output.lines().filter(|l| l.contains(":1:")).collect();
        assert_eq!(
            lines,
            vec![
                "a.rs:1: findme",
                "m.rs:1: findme",
                "sub/nested.rs:1: findme",
                "sub/nested.txt:1: findme",
                "z.rs:1: findme",
            ],
            "{}",
            r1.output
        );
        // The glob still filters the pruned walk.
        let r = tool
            .execute(json!({"pattern": "findme", "glob": "**/*.txt"}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(r.output.contains("sub/nested.txt"), "{}", r.output);
        assert!(!r.output.contains("z.rs"), "{}", r.output);
    }
}
