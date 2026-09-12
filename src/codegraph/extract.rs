// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Tree-sitter symbol/edge extraction for CodeGraph.
//!
//! [`extract_file`] parses one source file into [`Symbol`]s (definitions with
//! line ranges) and [`Ref`]s (unresolved references: calls + imported names).
//! [`resolve_edges`] then turns refs into [`Edge`]s across a whole file set by
//! matching names against the collected symbols (same-file first, then
//! project-wide). Extraction is cursor-based tree walking rather than
//! query-files: no query assets to keep in sync with grammar versions, and the
//! node kinds used below are stable across tree-sitter releases.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::codegraph::walk::Lang;

/// The kind of a defined symbol. The set is unioned across every indexed
/// language (e.g. Rust `struct_item` maps to [`SymbolKind::Struct`], TS/JS
/// `class_declaration` to [`SymbolKind::Class`]). Serializes snake_case
/// (`"function"`, `"type_alias"`,
/// …) — the same strings [`SymbolKind::as_str`] produces, so DB rows and JSON
/// output share one canonical form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    /// A free function (`fn` / `function`).
    Function,
    /// A function inside a container (impl block / class).
    Method,
    /// A struct or TS class.
    Struct,
    /// A Rust enum.
    Enum,
    /// A Rust trait or TS interface.
    Trait,
    /// A Rust `impl` block.
    Impl,
    /// A type alias (`type` in both languages).
    TypeAlias,
    /// A TS class.
    Class,
    /// A TS interface.
    Interface,
    /// A TS enum.
    TsEnum,
    /// The synthetic per-file module symbol — the anchor for import edges and
    /// the fallback scope for top-level references.
    Module,
}

impl SymbolKind {
    /// Stable lowercase string form (DB storage + JSON output).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Impl => "impl",
            Self::TypeAlias => "type_alias",
            Self::Class => "class",
            Self::Interface => "interface",
            Self::TsEnum => "ts_enum",
            Self::Module => "module",
        }
    }

    /// Parse back from [`SymbolKind::as_str`] output; `None` on unknown.
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "function" => Self::Function,
            "method" => Self::Method,
            "struct" => Self::Struct,
            "enum" => Self::Enum,
            "trait" => Self::Trait,
            "impl" => Self::Impl,
            "type_alias" => Self::TypeAlias,
            "class" => Self::Class,
            "interface" => Self::Interface,
            "ts_enum" => Self::TsEnum,
            "module" => Self::Module,
            _ => return None,
        })
    }
}

/// The kind of an edge between two symbols. Serializes snake_case
/// (`"calls"`, `"imports"`, `"contains"`), matching [`EdgeKind::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// `from` calls `to` (call / new expression).
    Calls,
    /// `from` (always the file's module symbol) imports the name `to`.
    Imports,
    /// `from` lexically contains `to` (module→item, impl→method, …).
    Contains,
}

impl EdgeKind {
    /// Stable lowercase string form (DB storage + JSON output).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::Imports => "imports",
            Self::Contains => "contains",
        }
    }

    /// Parse back from [`EdgeKind::as_str`] output; `None` on unknown.
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "calls" => Self::Calls,
            "imports" => Self::Imports,
            "contains" => Self::Contains,
            _ => return None,
        })
    }
}

/// A defined symbol (one row in the graph).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Symbol {
    /// Stable id: `{relpath}::{name}::{start_line}`. Unique within a file;
    /// file paths are unique within the project, so unique in the store.
    pub id: String,
    /// The short declared name (no paths, no generics).
    pub name: String,
    /// What kind of thing this is.
    pub kind: SymbolKind,
    /// Project-relative path with `/` separators.
    pub file: String,
    /// 1-based line where the definition starts.
    pub start_line: usize,
    /// 1-based line where the definition ends.
    pub end_line: usize,
}

/// An unresolved reference found while parsing one file. Resolution to a
/// target symbol id happens later ([`resolve_edges`]) when all files' symbols
/// are known — a call may target a definition in another file.
#[derive(Debug, Clone, PartialEq)]
pub struct Ref {
    /// The symbol id the reference comes from (enclosing function, else the
    /// file's module symbol).
    pub from: String,
    /// The referenced name (callee identifier / imported name).
    pub name: String,
    /// Call vs import — decides the resulting edge kind.
    pub kind: RefKind,
}

/// Whether a [`Ref`] is a call or an import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    /// A call or `new` expression.
    Call,
    /// A name brought in by `use` / `import`.
    Import,
}

/// The parse result for one file.
#[derive(Debug, Clone, Default)]
pub struct FileExtract {
    /// Symbols defined in the file (module symbol included).
    pub symbols: Vec<Symbol>,
    /// Unresolved references found in the file (calls + imports).
    pub refs: Vec<Ref>,
    /// Containment pairs (`parent id`, `child id`) computed from line-range
    /// nesting — already fully resolved, so they bypass the ref pipeline.
    pub contains: Vec<(String, String)>,
}

/// The synthetic module symbol id for a file.
fn module_id(rel: &str) -> String {
    format!("{rel}::module")
}

/// A per-worker cache of tree-sitter parsers, one per language (backlog
/// 5a85e36c): the parallel index workers pay `Parser::new` +
/// `set_language` once per language instead of once per file — and the
/// HTML script-block sub-parses reuse the JS parser instead of
/// constructing one per block. Not `Sync` — each worker owns its own.
pub struct ParserCache {
    by_lang: HashMap<Lang, tree_sitter::Parser>,
}

impl ParserCache {
    /// An empty cache — parsers materialize lazily per language on first
    /// use.
    pub fn new() -> Self {
        Self {
            by_lang: HashMap::new(),
        }
    }

    /// The parser for `lang`, constructing + configuring it on first use.
    /// `None` only on a grammar/runtime ABI mismatch — nothing sensible
    /// to extract (the caller skips the file, mirroring the old
    /// construct-per-file fallback).
    fn parser(&mut self, lang: Lang) -> Option<&mut tree_sitter::Parser> {
        if self.by_lang.contains_key(&lang) {
            return self.by_lang.get_mut(&lang);
        }
        let mut parser = tree_sitter::Parser::new();
        if parser.set_language(&grammar_for(lang)).is_err() {
            return None;
        }
        self.by_lang.insert(lang, parser);
        self.by_lang.get_mut(&lang)
    }
}

/// The tree-sitter grammar for `lang` (every [`Lang`] variant has one).
fn grammar_for(lang: Lang) -> tree_sitter::Language {
    match lang {
        Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        Lang::Ts => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Lang::Js => tree_sitter_javascript::LANGUAGE.into(),
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::Go => tree_sitter_go::LANGUAGE.into(),
        Lang::Java => tree_sitter_java::LANGUAGE.into(),
        Lang::C => tree_sitter_c::LANGUAGE.into(),
        Lang::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        Lang::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
        Lang::Ruby => tree_sitter_ruby::LANGUAGE.into(),
        Lang::Php => tree_sitter_php::LANGUAGE_PHP.into(),
        Lang::Html => tree_sitter_html::LANGUAGE.into(),
    }
}

/// Parse one file into symbols + unresolved refs. Never fails: tree-sitter
/// returns a tree even for syntactically broken source (error nodes are simply
/// skipped by the walk), and unknown node kinds are ignored. Constructs a
/// fresh parser — the parallel index path uses [`extract_file_with`] with a
/// per-worker [`ParserCache`] instead (backlog 5a85e36c).
pub fn extract_file(rel: &str, source: &str, lang: Lang) -> FileExtract {
    let mut parsers = ParserCache::new();
    extract_file_with(rel, source, lang, &mut parsers)
}

/// [`extract_file`] with the caller's [`ParserCache`] (backlog 5a85e36c):
/// the parallel index workers keep one cache per worker, so parser
/// construction is paid once per language per worker, not once per file —
/// and the HTML script-block sub-parses reuse the cache's JS parser.
pub fn extract_file_with(
    rel: &str,
    source: &str,
    lang: Lang,
    parsers: &mut ParserCache,
) -> FileExtract {
    let Some(tree) = parsers
        .parser(lang)
        .and_then(|p| p.parse(source, None))
    else {
        // Grammar/runtime ABI mismatch or a parse failure — nothing
        // sensible to extract.
        return FileExtract::default();
    };

    let mut out = FileExtract {
        symbols: vec![Symbol {
            id: module_id(rel),
            name: rel.to_string(),
            kind: SymbolKind::Module,
            file: rel.to_string(),
            start_line: 1,
            end_line: source.lines().count().max(1),
        }],
        refs: Vec::new(),
        contains: Vec::new(),
    };
    let mut ctx = Ctx {
        rel,
        src: source,
        out: &mut out,
        scope: vec![module_id(rel)],
        in_impl: 0,
        line_offset: 0,
        parsers,
    };
    let root = tree.root_node();
    match lang {
        Lang::Rust => visit_rust(root, &mut ctx),
        // JS shares the TS walk: the JavaScript grammar uses the same node
        // kinds for everything visit_ts handles (TS-only kinds simply never
        // appear in JS sources).
        Lang::Ts | Lang::Tsx | Lang::Js => visit_ts(root, &mut ctx),
        Lang::Python => visit_python(root, &mut ctx),
        Lang::Go => visit_go(root, &mut ctx),
        Lang::Java => visit_java(root, &mut ctx),
        Lang::C | Lang::Cpp => visit_c(root, &mut ctx),
        Lang::CSharp => visit_csharp(root, &mut ctx),
        Lang::Ruby => visit_ruby(root, &mut ctx),
        Lang::Php => visit_php(root, &mut ctx),
        Lang::Html => visit_html(root, &mut ctx),
    }
    add_contains_edges(&mut out);
    out
}

/// Shared walk context: the file being parsed, the output accumulators, and
/// the enclosing-scope stack (innermost last). The stack is seeded with the
/// module symbol so top-level references anchor to the file.
struct Ctx<'a> {
    rel: &'a str,
    src: &'a str,
    out: &'a mut FileExtract,
    /// Enclosing definition ids (module → … → innermost function).
    scope: Vec<String>,
    /// Depth inside `impl_item` / `class` — decides Function vs Method.
    in_impl: u32,
    /// Lines added to every definition's position — HTML `<script>` blocks
    /// are sub-parsed as standalone JavaScript, so their symbols must be
    /// shifted to their true lines in the HTML file.
    line_offset: usize,
    /// The caller's parser cache — only the HTML walk reads it (the
    /// script-block sub-parses), but every Ctx carries it so the sub-Ctx
    /// construction stays uniform (backlog 5a85e36c).
    parsers: &'a mut ParserCache,
}

impl Ctx<'_> {
    /// Node text, empty on invalid UTF-8 boundaries (never for our sources).
    fn text(&self, node: tree_sitter::Node) -> &str {
        node.utf8_text(self.src.as_bytes()).unwrap_or("")
    }

    /// Add a definition symbol and push it on the scope stack. Returns the id.
    fn def(&mut self, name: &str, kind: SymbolKind, node: tree_sitter::Node) -> String {
        let start = node.start_position().row + 1 + self.line_offset;
        let id = format!("{}::{}::{}", self.rel, name, start);
        self.out.symbols.push(Symbol {
            id: id.clone(),
            name: name.to_string(),
            kind,
            file: self.rel.to_string(),
            start_line: start,
            end_line: node.end_position().row + 1 + self.line_offset,
        });
        self.scope.push(id.clone());
        id
    }

    /// Record a call/import reference from the innermost enclosing scope.
    fn reference(&mut self, name: &str, kind: RefKind) {
        if name.is_empty() || SELF_NAMES.contains(&name) {
            return;
        }
        let from = self.scope.last().cloned().unwrap_or_default();
        self.out.refs.push(Ref {
            from,
            name: name.to_string(),
            kind,
        });
    }
}

/// Names that are never interesting reference targets (`self::x` tails).
const SELF_NAMES: &[&str] = &["self", "super", "crate"];

// =============================== Rust walk ===============================

/// Visit a Rust node's named children, extracting definitions and references.
fn visit_rust(node: tree_sitter::Node, ctx: &mut Ctx) {
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i as u32) else {
            continue;
        };
        match child.kind() {
            "function_item" => {
                let name = field_text(child, "name", ctx);
                let kind = if ctx.in_impl > 0 {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                let _id = ctx.def(&name, kind, child);
                visit_rust(child, ctx);
                ctx.scope.pop();
            }
            "function_signature_item" => {
                // Trait method declaration (no body) — still a symbol.
                let name = field_text(child, "name", ctx);
                let kind = if ctx.in_impl > 0 {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                ctx.def(&name, kind, child);
                ctx.scope.pop();
            }
            "struct_item" => {
                let name = field_text(child, "name", ctx);
                ctx.def(&name, SymbolKind::Struct, child);
                ctx.scope.pop();
            }
            "enum_item" => {
                let name = field_text(child, "name", ctx);
                ctx.def(&name, SymbolKind::Enum, child);
                ctx.scope.pop();
            }
            "trait_item" => {
                let name = field_text(child, "name", ctx);
                ctx.def(&name, SymbolKind::Trait, child);
                ctx.in_impl += 1;
                visit_rust(child, ctx);
                ctx.in_impl -= 1;
                ctx.scope.pop();
            }
            "impl_item" => {
                let name = impl_name(child, ctx);
                if name.is_empty() {
                    // Anonymous/invalid impl — recurse without a symbol; its
                    // methods anchor to the enclosing scope instead.
                    ctx.in_impl += 1;
                    visit_rust(child, ctx);
                    ctx.in_impl -= 1;
                } else {
                    let _id = ctx.def(&name, SymbolKind::Impl, child);
                    ctx.in_impl += 1;
                    visit_rust(child, ctx);
                    ctx.in_impl -= 1;
                    ctx.scope.pop();
                }
            }
            "type_item" => {
                let name = field_text(child, "name", ctx);
                ctx.def(&name, SymbolKind::TypeAlias, child);
                ctx.scope.pop();
            }
            "call_expression" => {
                if let Some(fn_node) = child.child_by_field_name("function") {
                    let name = callee_name(fn_node, ctx);
                    ctx.reference(&name, RefKind::Call);
                }
                visit_rust(child, ctx);
            }
            "use_declaration" => {
                let mut names = Vec::new();
                // The argument is a plain named child (scoped_identifier /
                // use_list / use_as_clause / …), so walk all named children.
                for i in 0..child.named_child_count() {
                    if let Some(c) = child.named_child(i as u32) {
                        use_leaf_names(c, ctx, &mut names);
                    }
                }
                for name in names {
                    ctx.reference(&name, RefKind::Import);
                }
            }
            _ => visit_rust(child, ctx),
        }
    }
}

// ================================ TS walk ================================

/// Visit a TS/TSX node's named children. `export function f` arrives as an
/// `export_statement` wrapper whose inner `function_declaration` is handled by
/// the generic-recursion fallthrough.
fn visit_ts(node: tree_sitter::Node, ctx: &mut Ctx) {
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i as u32) else {
            continue;
        };
        match child.kind() {
            // `function* gen() {}` is a separate grammar rule from
            // `function gen() {}` (round-3 LOW-1, 2026-09-11) — both carry
            // the same `name` field, so one arm covers them.
            "function_declaration" | "generator_function_declaration" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Function, child);
                visit_ts(child, ctx);
                ctx.scope.pop();
            }
            "class_declaration" | "abstract_class_declaration" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Class, child);
                ctx.in_impl += 1;
                visit_ts(child, ctx);
                ctx.in_impl -= 1;
                ctx.scope.pop();
            }
            "interface_declaration" => {
                let name = field_text(child, "name", ctx);
                ctx.def(&name, SymbolKind::Interface, child);
                ctx.scope.pop();
            }
            "type_alias_declaration" => {
                let name = field_text(child, "name", ctx);
                ctx.def(&name, SymbolKind::TypeAlias, child);
                ctx.scope.pop();
            }
            "enum_declaration" => {
                let name = field_text(child, "name", ctx);
                ctx.def(&name, SymbolKind::TsEnum, child);
                ctx.scope.pop();
            }
            "method_definition" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Method, child);
                visit_ts(child, ctx);
                ctx.scope.pop();
            }
            "lexical_declaration" | "variable_declaration" => {
                // Function-valued const/let/var bindings — `const testFoo =
                // () => {}`, the dominant jest/vitest test shape — are
                // Function symbols (review HIGH-1, 2026-09-11: without
                // this arm the finish gate dead-ended on real arrow-const
                // regression tests). Non-function bindings keep yielding
                // nothing but still recurse (calls in initializers).
                for i in 0..child.named_child_count() {
                    let Some(decl) = child.named_child(i as u32) else {
                        continue;
                    };
                    if decl.kind() != "variable_declarator" {
                        visit_ts(decl, ctx);
                        continue;
                    }
                    let name = decl
                        .child_by_field_name("name")
                        .filter(|n| n.kind() == "identifier")
                        .map(|n| ctx.text(n).to_string());
                    let value = decl.child_by_field_name("value");
                    let is_fn = value
                        .map(|v| {
                            matches!(
                                v.kind(),
                                "arrow_function" | "function_expression" | "generator_function"
                            )
                        })
                        .unwrap_or(false);
                    match (name, is_fn) {
                        (Some(name), true) => {
                            let _id = ctx.def(&name, SymbolKind::Function, decl);
                            if let Some(value) = value {
                                visit_ts(value, ctx);
                            }
                            ctx.scope.pop();
                        }
                        _ => visit_ts(decl, ctx),
                    }
                }
            }
            "call_expression" => {
                if let Some(fn_node) = child.child_by_field_name("function") {
                    let name = callee_name(fn_node, ctx);
                    ctx.reference(&name, RefKind::Call);
                }
                visit_ts(child, ctx);
            }
            "new_expression" => {
                if let Some(ctor) = child.child_by_field_name("constructor") {
                    let name = callee_name(ctor, ctx);
                    ctx.reference(&name, RefKind::Call);
                }
                visit_ts(child, ctx);
            }
            "import_statement" => {
                let mut names = Vec::new();
                // The clause is a plain named child (no field name), so walk
                // all named children; `ts_import_names` ignores non-clause
                // kinds (e.g. the module source string).
                for i in 0..child.named_child_count() {
                    if let Some(c) = child.named_child(i as u32) {
                        ts_import_names(c, ctx, &mut names);
                    }
                }
                for name in names {
                    ctx.reference(&name, RefKind::Import);
                }
            }
            _ => visit_ts(child, ctx),
        }
    }
}

// ============================== Python walk ===============================

/// Visit a Python node's named children. `@decorator` wrappers
/// (`decorated_definition`) and nested statements recurse generically.
fn visit_python(node: tree_sitter::Node, ctx: &mut Ctx) {
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i as u32) else {
            continue;
        };
        match child.kind() {
            "function_definition" => {
                let name = field_text(child, "name", ctx);
                let kind = if ctx.in_impl > 0 {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                let _id = ctx.def(&name, kind, child);
                visit_python(child, ctx);
                ctx.scope.pop();
            }
            "class_definition" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Class, child);
                ctx.in_impl += 1;
                visit_python(child, ctx);
                ctx.in_impl -= 1;
                ctx.scope.pop();
            }
            "call" => {
                if let Some(fn_node) = child.child_by_field_name("function") {
                    let name = callee_name(fn_node, ctx);
                    ctx.reference(&name, RefKind::Call);
                }
                visit_python(child, ctx);
            }
            "import_statement" | "import_from_statement" => {
                let mut names = Vec::new();
                for i in 0..child.named_child_count() {
                    if let Some(c) = child.named_child(i as u32) {
                        if child.child_by_field_name("module_name") == Some(c)
                            || child.child_by_field_name("module") == Some(c)
                        {
                            // `from X import …` — X is a prefix, not a name
                            // used bare in code.
                            continue;
                        }
                        py_import_names(c, ctx, &mut names);
                    }
                }
                for name in names {
                    ctx.reference(&name, RefKind::Import);
                }
            }
            _ => visit_python(child, ctx),
        }
    }
}

// =============================== Ruby walk ================================

/// Visit a Ruby node's named children. `class << self` (`sclass`) and
/// blocks recurse generically; method definitions inside them still land.
fn visit_ruby(node: tree_sitter::Node, ctx: &mut Ctx) {
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i as u32) else {
            continue;
        };
        match child.kind() {
            "method" | "singleton_method" => {
                let name = field_text(child, "name", ctx);
                let kind = if ctx.in_impl > 0 {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                let _id = ctx.def(&name, kind, child);
                visit_ruby(child, ctx);
                ctx.scope.pop();
            }
            "class" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Class, child);
                ctx.in_impl += 1;
                visit_ruby(child, ctx);
                ctx.in_impl -= 1;
                ctx.scope.pop();
            }
            "module" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Module, child);
                ctx.in_impl += 1;
                visit_ruby(child, ctx);
                ctx.in_impl -= 1;
                ctx.scope.pop();
            }
            "call" => {
                if let Some(m) = child.child_by_field_name("method") {
                    let name = callee_name(m, ctx);
                    ctx.reference(&name, RefKind::Call);
                }
                visit_ruby(child, ctx);
            }
            _ => visit_ruby(child, ctx),
        }
    }
}

// ================================ PHP walk ================================

/// Visit a PHP node's named children. `namespace` blocks and attribute
/// lists recurse generically.
fn visit_php(node: tree_sitter::Node, ctx: &mut Ctx) {
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i as u32) else {
            continue;
        };
        match child.kind() {
            "function_definition" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Function, child);
                visit_php(child, ctx);
                ctx.scope.pop();
            }
            "class_declaration" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Class, child);
                ctx.in_impl += 1;
                visit_php(child, ctx);
                ctx.in_impl -= 1;
                ctx.scope.pop();
            }
            "interface_declaration" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Interface, child);
                ctx.in_impl += 1;
                visit_php(child, ctx);
                ctx.in_impl -= 1;
                ctx.scope.pop();
            }
            "trait_declaration" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Trait, child);
                ctx.in_impl += 1;
                visit_php(child, ctx);
                ctx.in_impl -= 1;
                ctx.scope.pop();
            }
            "enum_declaration" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Enum, child);
                ctx.in_impl += 1;
                visit_php(child, ctx);
                ctx.in_impl -= 1;
                ctx.scope.pop();
            }
            "method_declaration" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Method, child);
                visit_php(child, ctx);
                ctx.scope.pop();
            }
            "function_call_expression" | "member_call_expression"
            | "scoped_call_expression" => {
                let fn_node = child
                    .child_by_field_name("function")
                    .or_else(|| child.child_by_field_name("name"));
                if let Some(fn_node) = fn_node {
                    let name = callee_name(fn_node, ctx);
                    ctx.reference(&name, RefKind::Call);
                }
                visit_php(child, ctx);
            }
            "object_creation_expression" => {
                // `new Greeter()` / `new Foo\Bar()` — the first name node is
                // the class; its leaf segment is the call target.
                let mut leaf = String::new();
                walk_for_kind(child, &["name", "qualified_name"], &mut |n| {
                    if leaf.is_empty() {
                        if let Some(last) = ctx.text(n).split('\\').filter(|s| !s.is_empty()).last() {
                            leaf = last.to_string();
                        }
                    }
                });
                ctx.reference(&leaf, RefKind::Call);
                visit_php(child, ctx);
            }
            "namespace_use_declaration" | "function_use_declaration"
            | "const_use_declaration" => {
                let mut names = Vec::new();
                for i in 0..child.named_child_count() {
                    if let Some(c) = child.named_child(i as u32) {
                        php_use_names(c, ctx, &mut names);
                    }
                }
                for name in names {
                    ctx.reference(&name, RefKind::Import);
                }
            }
            _ => visit_php(child, ctx),
        }
    }
}

// ================================ Go walk =================================

/// Visit a Go node's named children. `type_declaration` wraps its specs
/// (`type_spec`), so it is handled explicitly; everything else recurses.
fn visit_go(node: tree_sitter::Node, ctx: &mut Ctx) {
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i as u32) else {
            continue;
        };
        match child.kind() {
            "function_declaration" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Function, child);
                visit_go(child, ctx);
                ctx.scope.pop();
            }
            "method_declaration" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Method, child);
                visit_go(child, ctx);
                ctx.scope.pop();
            }
            "type_declaration" => {
                for i in 0..child.named_child_count() {
                    let Some(spec) = child.named_child(i as u32) else {
                        continue;
                    };
                    if spec.kind() != "type_spec" {
                        continue;
                    }
                    let name = field_text(spec, "name", ctx);
                    let kind = if has_named_child_kind(spec, "struct_type") {
                        SymbolKind::Struct
                    } else if has_named_child_kind(spec, "interface_type") {
                        SymbolKind::Interface
                    } else {
                        SymbolKind::TypeAlias
                    };
                    ctx.def(&name, kind, spec);
                    ctx.scope.pop();
                }
            }
            "call_expression" => {
                if let Some(fn_node) = child.child_by_field_name("function") {
                    let name = callee_name(fn_node, ctx);
                    ctx.reference(&name, RefKind::Call);
                }
                visit_go(child, ctx);
            }
            "import_declaration" => {
                // Grouped imports wrap their specs in an `import_spec_list`;
                // single imports have the spec directly — walk for the
                // specs either way.
                let mut names = Vec::new();
                walk_for_kind(child, &["import_spec"], &mut |spec| {
                    if let Some(name) = go_import_name(spec, ctx) {
                        names.push(name);
                    }
                });
                for name in names {
                    ctx.reference(&name, RefKind::Import);
                }
            }
            _ => visit_go(child, ctx),
        }
    }
}

// ================================ Java walk ================================

/// Visit a Java node's named children. Modifiers and annotations are
/// fields, so declarations arrive directly.
fn visit_java(node: tree_sitter::Node, ctx: &mut Ctx) {
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i as u32) else {
            continue;
        };
        match child.kind() {
            "class_declaration" | "record_declaration" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Class, child);
                ctx.in_impl += 1;
                visit_java(child, ctx);
                ctx.in_impl -= 1;
                ctx.scope.pop();
            }
            "interface_declaration" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Interface, child);
                ctx.in_impl += 1;
                visit_java(child, ctx);
                ctx.in_impl -= 1;
                ctx.scope.pop();
            }
            "enum_declaration" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Enum, child);
                ctx.in_impl += 1;
                visit_java(child, ctx);
                ctx.in_impl -= 1;
                ctx.scope.pop();
            }
            "method_declaration" | "constructor_declaration" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Method, child);
                visit_java(child, ctx);
                ctx.scope.pop();
            }
            "method_invocation" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = callee_name(name_node, ctx);
                    ctx.reference(&name, RefKind::Call);
                }
                visit_java(child, ctx);
            }
            "object_creation_expression" => {
                if let Some(ty) = child.child_by_field_name("type") {
                    let name = callee_name(ty, ctx);
                    ctx.reference(&name, RefKind::Call);
                }
                visit_java(child, ctx);
            }
            "import_declaration" => {
                if let Some(name) = last_identifier_text(child, ctx) {
                    ctx.reference(&name, RefKind::Import);
                }
            }
            _ => visit_java(child, ctx),
        }
    }
}

// ================================ C/C++ walk ==============================

/// Visit a C/C++ node's named children — one shared walk: the C grammar
/// never produces the C++-only kinds (`class_specifier`, `alias_declaration`,
/// `using_declaration`), so both arms are safe in one match. Preprocessor
/// directives and plain declarations recurse generically (calls inside
/// initializers are still found).
fn visit_c(node: tree_sitter::Node, ctx: &mut Ctx) {
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i as u32) else {
            continue;
        };
        match child.kind() {
            "function_definition" => {
                let name = c_function_name(child, ctx);
                if !name.is_empty() {
                    let kind = if ctx.in_impl > 0 {
                        SymbolKind::Method
                    } else {
                        SymbolKind::Function
                    };
                    let _id = ctx.def(&name, kind, child);
                    visit_c(child, ctx);
                    ctx.scope.pop();
                } else {
                    // Anonymous/invalid declarator — recurse without a
                    // symbol; calls inside still anchor to the scope.
                    visit_c(child, ctx);
                }
            }
            "struct_specifier" | "union_specifier" => {
                let name = field_text(child, "name", ctx);
                // Only a specifier WITH a body is a definition — `struct
                // Point p` inside a declaration is a usage and yields no
                // symbol.
                if !name.is_empty()
                    && has_named_child_kind(child, "field_declaration_list")
                {
                    let _id = ctx.def(&name, SymbolKind::Struct, child);
                    ctx.in_impl += 1;
                    visit_c(child, ctx);
                    ctx.in_impl -= 1;
                    ctx.scope.pop();
                } else {
                    // Anonymous or usage-only — recurse without a symbol.
                    visit_c(child, ctx);
                }
            }
            "enum_specifier" => {
                let name = field_text(child, "name", ctx);
                if !name.is_empty() && has_named_child_kind(child, "enumerator_list") {
                    ctx.def(&name, SymbolKind::Enum, child);
                    ctx.scope.pop();
                }
                visit_c(child, ctx);
            }
            "type_definition" => {
                let name = c_typedef_name(child, ctx);
                if !name.is_empty() {
                    ctx.def(&name, SymbolKind::TypeAlias, child);
                    ctx.scope.pop();
                }
                visit_c(child, ctx);
            }
            "class_specifier" => {
                let name = field_text(child, "name", ctx);
                // Only a specifier WITH a body is a definition — a C++
                // forward declaration (`class Foo;`) yields no symbol.
                if !name.is_empty()
                    && has_named_child_kind(child, "field_declaration_list")
                {
                    let _id = ctx.def(&name, SymbolKind::Class, child);
                    ctx.in_impl += 1;
                    visit_c(child, ctx);
                    ctx.in_impl -= 1;
                    ctx.scope.pop();
                } else {
                    visit_c(child, ctx);
                }
            }
            "alias_declaration" => {
                let name = field_text(child, "name", ctx);
                ctx.def(&name, SymbolKind::TypeAlias, child);
                ctx.scope.pop();
            }
            "using_declaration" => {
                if let Some(name) = last_identifier_text(child, ctx) {
                    ctx.reference(&name, RefKind::Import);
                }
            }
            "field_declaration" => {
                // C++ class members: a member that declares a function
                // (`void bar();` / inline bodies) is a method; plain data
                // members yield no symbol. C struct fields land here too —
                // they never pass the function-declarator test.
                if let Some(name) = c_field_method_name(child, ctx) {
                    let _id = ctx.def(&name, SymbolKind::Method, child);
                    visit_c(child, ctx);
                    ctx.scope.pop();
                } else {
                    visit_c(child, ctx);
                }
            }
            "call_expression" => {
                if let Some(fn_node) = child.child_by_field_name("function") {
                    let name = callee_name(fn_node, ctx);
                    ctx.reference(&name, RefKind::Call);
                }
                visit_c(child, ctx);
            }
            _ => visit_c(child, ctx),
        }
    }
}

// ================================ C# walk ==================================

/// Visit a C# node's named children. Attribute lists and modifiers are
/// fields, so declarations arrive directly; `namespace` blocks recurse.
fn visit_csharp(node: tree_sitter::Node, ctx: &mut Ctx) {
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i as u32) else {
            continue;
        };
        match child.kind() {
            "class_declaration" | "record_declaration" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Class, child);
                ctx.in_impl += 1;
                visit_csharp(child, ctx);
                ctx.in_impl -= 1;
                ctx.scope.pop();
            }
            "struct_declaration" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Struct, child);
                ctx.in_impl += 1;
                visit_csharp(child, ctx);
                ctx.in_impl -= 1;
                ctx.scope.pop();
            }
            "interface_declaration" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Interface, child);
                ctx.in_impl += 1;
                visit_csharp(child, ctx);
                ctx.in_impl -= 1;
                ctx.scope.pop();
            }
            "enum_declaration" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Enum, child);
                ctx.in_impl += 1;
                visit_csharp(child, ctx);
                ctx.in_impl -= 1;
                ctx.scope.pop();
            }
            "method_declaration" | "constructor_declaration" => {
                let name = field_text(child, "name", ctx);
                let _id = ctx.def(&name, SymbolKind::Method, child);
                visit_csharp(child, ctx);
                ctx.scope.pop();
            }
            "invocation_expression" => {
                if let Some(fn_node) = child.child_by_field_name("function") {
                    let name = callee_name(fn_node, ctx);
                    ctx.reference(&name, RefKind::Call);
                }
                visit_csharp(child, ctx);
            }
            "object_creation_expression" => {
                if let Some(ty) = child.child_by_field_name("type") {
                    let name = callee_name(ty, ctx);
                    ctx.reference(&name, RefKind::Call);
                }
                visit_csharp(child, ctx);
            }
            "using_directive" => {
                if let Some(name) = last_identifier_text(child, ctx) {
                    ctx.reference(&name, RefKind::Import);
                }
            }
            _ => visit_csharp(child, ctx),
        }
    }
}

// ================================ HTML walk ================================

/// Visit an HTML tree: the file's code lives in its `<script>` blocks —
/// each block's text is sub-parsed with the JavaScript grammar, with a
/// line offset so definitions land at their true file lines. Non-JS
/// script types (templates, JSON data blocks) are skipped.
fn visit_html(node: tree_sitter::Node, ctx: &mut Ctx) {
    let rel = ctx.rel;
    let html_src = ctx.src;
    // Collect the JS blocks first (raw_text slices + their line offsets)
    // so the sub-parses never borrow the walk context.
    let mut scripts: Vec<(&str, usize)> = Vec::new();
    walk_for_kind(node, &["script_element"], &mut |script| {
        if !is_js_script(script, html_src) {
            return;
        }
        let Some(raw) = (0..script.named_child_count())
            .filter_map(|i| script.named_child(i as u32))
            .find(|c| c.kind() == "raw_text")
        else {
            return;
        };
        let js = raw.utf8_text(html_src.as_bytes()).unwrap_or("");
        if js.trim().is_empty() {
            return;
        }
        // The block's first line within the HTML file (0-based row).
        scripts.push((js, raw.start_position().row));
    });
    for (js, offset) in scripts {
        // The sub-parse reuses the caller's cached JS parser (backlog
        // 5a85e36c) — one construction per worker, not one per block.
        let Some(tree) = ctx
            .parsers
            .parser(Lang::Js)
            .and_then(|p| p.parse(js, None))
        else {
            continue;
        };
        let mut sub = Ctx {
            rel,
            src: js,
            out: &mut *ctx.out,
            scope: vec![module_id(rel)],
            in_impl: 0,
            line_offset: offset,
            parsers: &mut *ctx.parsers,
        };
        visit_ts(tree.root_node(), &mut sub);
    }
}

/// Whether a `<script>` block carries JavaScript (no `type` attribute, or
/// a JS MIME type / `module`) — templates and JSON data blocks are skipped.
/// The attributes live on the script's start tag (a child of the
/// `script_element`), so the whole element is walked for them.
fn is_js_script(script: tree_sitter::Node, src: &str) -> bool {
    let mut ty: Option<String> = None;
    walk_for_kind(script, &["attribute"], &mut |attr| {
        let Some(name) = attr.named_child(0) else {
            return;
        };
        if name.kind() != "attribute_name"
            || !name
                .utf8_text(src.as_bytes())
                .unwrap_or("")
                .eq_ignore_ascii_case("type")
        {
            return;
        }
        let mut value = String::new();
        for j in 0..attr.named_child_count() {
            if let Some(v) = attr.named_child(j as u32) {
                if v.kind() == "quoted_attribute_value" {
                    value = v
                        .utf8_text(src.as_bytes())
                        .unwrap_or("")
                        .trim_matches(|c| c == '"' || c == '\'')
                        .trim()
                        .to_ascii_lowercase();
                }
            }
        }
        ty = Some(value);
    });
    match ty.as_deref() {
        None => true,
        Some("")
        | Some("text/javascript")
        | Some("application/javascript")
        | Some("module")
        | Some("text/ecmascript")
        | Some("application/ecmascript")
        | Some("text/babel") => true,
        Some(_) => false,
    }
}

// ============================== helpers ==================================

/// Text of a named field (empty string when absent or not valid UTF-8).
fn field_text(node: tree_sitter::Node, field: &str, ctx: &Ctx) -> String {
    node.child_by_field_name(field)
        .map(|n| ctx.text(n).to_string())
        .unwrap_or_default()
}

/// The callee name for a call/new expression's function node: a bare
/// identifier is itself; a path (`a::b`) or member (`obj.m`) reduces to its
/// final segment.
fn callee_name(node: tree_sitter::Node, ctx: &Ctx) -> String {
    match node.kind() {
        // `name` is PHP's identifier node (function/class/method names).
        "identifier" | "property_identifier" | "field_identifier" | "type_identifier" | "name" => {
            ctx.text(node).to_string()
        }
        _ => {
            for field in ["name", "field", "property", "attribute", "method"] {
                if let Some(inner) = node.child_by_field_name(field) {
                    return callee_name(inner, ctx);
                }
            }
            // Fallback: the last named child (trailing segment of a path).
            (0..node.named_child_count())
                .rev()
                .filter_map(|i| node.named_child(i as u32))
                .find_map(|c| {
                    let name = callee_name(c, ctx);
                    (name != "arguments" && !name.is_empty()).then_some(name)
                })
                .unwrap_or_default()
        }
    }
}

/// The declared name of a Rust `impl` block: the first type identifier inside
/// its `type` field (`impl Foo<T>` → `Foo`).
fn impl_name(node: tree_sitter::Node, ctx: &Ctx) -> String {
    let Some(ty) = node.child_by_field_name("type") else {
        return String::new();
    };
    let mut found = String::new();
    walk_for_kind(ty, &["type_identifier", "identifier"], &mut |n| {
        if found.is_empty() {
            found = ctx.text(n).to_string();
        }
    });
    found
}

/// Depth-first search for the first node whose kind is in `kinds`.
fn walk_for_kind(node: tree_sitter::Node, kinds: &[&str], f: &mut impl FnMut(tree_sitter::Node)) {
    if kinds.contains(&node.kind()) {
        f(node);
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            walk_for_kind(child, kinds, f);
        }
    }
}

/// Collect the leaf names of a Rust `use` declaration: `use a::b` → `b`;
/// `use a::{b, c}` → `b`, `c`; `use a::b as d` → `b` (the original name).
fn use_leaf_names(node: tree_sitter::Node, ctx: &Ctx, out: &mut Vec<String>) {
    match node.kind() {
        "identifier" => out.push(ctx.text(node).to_string()),
        "scoped_identifier" | "scoped_use_path" => {
            if let Some(tail) = node.child_by_field_name("name") {
                use_leaf_names(tail, ctx, out);
            } else if let Some(last) = (0..node.named_child_count())
                .rev()
                .filter_map(|i| node.named_child(i as u32))
                .next()
            {
                use_leaf_names(last, ctx, out);
            }
        }
        "use_list" => {
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i as u32) {
                    use_leaf_names(child, ctx, out);
                }
            }
        }
        "scoped_use_list" => {
            // `use a::{b, c}` — only the `list` field holds the imported
            // leaves; the `path` field is the module prefix, not an import.
            if let Some(list) = node.child_by_field_name("list") {
                use_leaf_names(list, ctx, out);
            }
        }
        "use_as_clause" => {
            // The original (pre-alias) name is the first identifier child.
            if let Some(orig) = node.named_child(0) {
                use_leaf_names(orig, ctx, out);
            }
        }
        _ => {}
    }
}

/// Collect the imported names of a TS import clause: default binding,
/// namespace binding, and each named-import specifier's original name.
fn ts_import_names(node: tree_sitter::Node, ctx: &Ctx, out: &mut Vec<String>) {
    match node.kind() {
        "identifier" => out.push(ctx.text(node).to_string()),
        "namespace_import" | "named_imports" => {
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i as u32) {
                    ts_import_names(child, ctx, out);
                }
            }
        }
        "import_specifier" => {
            // `name` is the original export; the alias (if any) is a different
            // field — we want the original so the edge targets the definition.
            if let Some(name) = node.child_by_field_name("name") {
                out.push(ctx.text(name).to_string());
            }
        }
        _ => {
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i as u32) {
                    ts_import_names(child, ctx, out);
                }
            }
        }
    }
}

/// Collect the names a Python import brings into scope: `import a.b.c`
/// → the leaf module `c`; `from x import y, z` → `y`, `z` (the module
/// prefix is skipped by the caller — it is not used bare in code).
fn py_import_names(node: tree_sitter::Node, ctx: &Ctx, out: &mut Vec<String>) {
    match node.kind() {
        "dotted_name" => {
            if let Some(last) = last_named_child(node) {
                out.push(ctx.text(last).to_string());
            }
        }
        "alias" | "aliased_import" => {
            // `import a as b` / `from x import y as z` — the original
            // (pre-alias) name is what the edge should target.
            if let Some(name) = node.child_by_field_name("name") {
                py_import_names(name, ctx, out);
            }
        }
        _ => {
            for i in 0..node.named_child_count() {
                if let Some(c) = node.named_child(i as u32) {
                    py_import_names(c, ctx, out);
                }
            }
        }
    }
}

/// Collect the names a PHP `use` declaration brings in: `use Foo\Bar;`
/// → `Bar` (the leaf of the qualified name).
fn php_use_names(node: tree_sitter::Node, ctx: &Ctx, out: &mut Vec<String>) {
    match node.kind() {
        "name" | "qualified_name" => {
            let text = ctx.text(node);
            if let Some(last) = text.split('\\').filter(|s| !s.is_empty()).last() {
                out.push(last.to_string());
            }
        }
        _ => {
            for i in 0..node.named_child_count() {
                if let Some(c) = node.named_child(i as u32) {
                    php_use_names(c, ctx, out);
                }
            }
        }
    }
}

/// The name a Go import brings into scope: the alias when present, else
/// the last path segment (`"github.com/x/y"` → `y`).
fn go_import_name(spec: tree_sitter::Node, ctx: &Ctx) -> Option<String> {
    if let Some(alias) = spec.child_by_field_name("name") {
        return Some(ctx.text(alias).to_string());
    }
    let path = spec.child_by_field_name("path")?;
    let inner = ctx.text(path).trim_matches('"');
    let last = inner.rsplit('/').next().unwrap_or("");
    (!last.is_empty()).then(|| last.to_string())
}

/// The declared name of a C/C++ `function_definition`: follow the
/// `declarator` chain (`int *f(void)` wraps the identifier in
/// pointer/function declarators) down to the identifier; a C++
/// `Foo::bar` qualified identifier reduces to its final segment.
fn c_function_name(node: tree_sitter::Node, ctx: &Ctx) -> String {
    let mut cur = node.child_by_field_name("declarator");
    while let Some(d) = cur {
        match d.kind() {
            "identifier" | "field_identifier" => return ctx.text(d).to_string(),
            "qualified_identifier" => {
                return d
                    .child_by_field_name("name")
                    .map(|n| ctx.text(n).to_string())
                    .unwrap_or_default();
            }
            _ => cur = d.child_by_field_name("declarator"),
        }
    }
    String::new()
}

/// The declared name of a C/C++ `typedef`: the declarator's identifier
/// (`typedef struct {...} Foo;` → `Foo`).
fn c_typedef_name(node: tree_sitter::Node, ctx: &Ctx) -> String {
    let mut cur = node.child_by_field_name("declarator");
    while let Some(d) = cur {
        match d.kind() {
            "type_identifier" | "identifier" => return ctx.text(d).to_string(),
            _ => cur = d.child_by_field_name("declarator"),
        }
    }
    String::new()
}

/// The method name of a C++ `field_declaration` that declares a function
/// (`class Foo { void bar(); };`) — `None` for plain data members (the
/// declarator chain never passes through a `function_declarator`).
fn c_field_method_name(node: tree_sitter::Node, ctx: &Ctx) -> Option<String> {
    let mut cur = node.child_by_field_name("declarator");
    let mut saw_function = false;
    while let Some(d) = cur {
        match d.kind() {
            "function_declarator" => saw_function = true,
            "identifier" | "field_identifier" => {
                return saw_function.then(|| ctx.text(d).to_string());
            }
            "qualified_identifier" => {
                return saw_function
                    .then(|| {
                        d.child_by_field_name("name")
                            .map(|n| ctx.text(n).to_string())
                            .unwrap_or_default()
                    })
                    .filter(|s| !s.is_empty());
            }
            _ => {}
        }
        cur = d.child_by_field_name("declarator");
    }
    None
}

/// The last named child of a node, or `None`.
fn last_named_child(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    (0..node.named_child_count())
        .rev()
        .filter_map(|i| node.named_child(i as u32))
        .next()
}

/// Whether `node` has a direct named child of kind `kind`.
fn has_named_child_kind(node: tree_sitter::Node, kind: &str) -> bool {
    (0..node.named_child_count()).any(|i| {
        node.named_child(i as u32)
            .map(|c| c.kind() == kind)
            .unwrap_or(false)
    })
}

/// The text of the LAST identifier leaf in a subtree — the final segment
/// of a dotted import path (`import a.b.C;` → `C`, `using A.B.C;` → `C`).
fn last_identifier_text(node: tree_sitter::Node, ctx: &Ctx) -> Option<String> {
    let mut found: Option<String> = None;
    walk_for_kind(node, &["identifier", "type_identifier"], &mut |n| {
        found = Some(ctx.text(n).to_string());
    });
    found
}

// =========================== containment edges ===========================

/// Add containment pairs for one file: every non-module symbol is paired with
/// the innermost symbol whose line range encloses it (module → top-level
/// items, impl → methods, function → nested fn, …). Ties on start line favor
/// the later-starting candidate, so an `impl` starting on the same line as its
/// first method wins as the container.
fn add_contains_edges(out: &mut FileExtract) {
    let module_id = module_id(&out.symbols[0].file);
    for symbol in &out.symbols {
        if symbol.kind == SymbolKind::Module {
            continue;
        }
        let mut best: Option<&Symbol> = None;
        for candidate in &out.symbols {
            if candidate.id == symbol.id
                || candidate.kind == SymbolKind::Module
                || candidate.file != symbol.file
            {
                continue;
            }
            let encloses =
                candidate.start_line <= symbol.start_line && symbol.end_line <= candidate.end_line;
            if !encloses {
                continue;
            }
            let better = best
                .map(|b| candidate.start_line >= b.start_line)
                .unwrap_or(true);
            if better {
                best = Some(candidate);
            }
        }
        let parent = best.map(|b| b.id.clone()).unwrap_or(module_id.clone());
        out.contains.push((parent, symbol.id.clone()));
    }
}

// ========================= cross-file resolution =========================

/// Outcome of [`resolve_edges`]: the per-symbol incoming-edge counts, the
/// final edge list, and how many references could not be resolved (dropped).
#[derive(Debug, Clone, Default)]
pub struct Resolved {
    /// Deduplicated edges (`from id`, `to id`, kind).
    pub edges: Vec<(String, String, EdgeKind)>,
    /// Number of refs dropped because no symbol matched their name.
    pub unresolved: usize,
}

/// Resolve all files' refs into cross-file edges, given every symbol.
///
/// Resolution order per ref: an exact-name symbol **in the same file**, else
/// the unique exact-name symbol project-wide, else (calls only) a
/// case-insensitive project-wide match, else dropped. Same-file preference
/// means local helpers win over a same-named import; project-wide requires
/// uniqueness so a common name (`new`, `open`) never produces a false edge.
/// Contains pairs pass straight through (already resolved ids).
pub fn resolve_edges(files: &[FileExtract]) -> Resolved {
    // Index: name → symbols with that name, per file and project-wide.
    let mut by_name_project: HashMap<String, Vec<&Symbol>> = HashMap::new();
    for f in files {
        for s in &f.symbols {
            if s.kind != SymbolKind::Module {
                by_name_project.entry(s.name.clone()).or_default().push(s);
            }
        }
    }

    let mut resolved = Resolved::default();
    // Pass 1: contains edges (pre-resolved) + module import edges.
    for f in files {
        for (parent, child) in &f.contains {
            resolved
                .edges
                .push((parent.clone(), child.clone(), EdgeKind::Contains));
        }
    }
    // Pass 2: call + import refs.
    let mut seen: HashSet<(String, String, EdgeKind)> = HashSet::new();
    for f in files {
        let file = &f.symbols[0].file;
        for r in &f.refs {
            let kind = match r.kind {
                RefKind::Call => EdgeKind::Calls,
                RefKind::Import => EdgeKind::Imports,
            };
            let target = resolve_one(&by_name_project, file, &r.name, r.kind);
            let Some(t) = target else {
                resolved.unresolved += 1;
                continue;
            };
            let key = (r.from.clone(), t.id.clone(), kind);
            if seen.insert(key) {
                resolved.edges.push((r.from.clone(), t.id.clone(), kind));
            }
        }
    }
    resolved
}

/// Resolve one ref against the index, same-file-first (see [`resolve_edges`]).
fn resolve_one<'a>(
    by_name: &HashMap<String, Vec<&'a Symbol>>,
    file: &str,
    name: &str,
    kind: RefKind,
) -> Option<&'a Symbol> {
    let candidates = by_name.get(name)?;
    let same_file: Vec<&&Symbol> = candidates.iter().filter(|s| s.file == file).collect();
    if let [only] = same_file[..] {
        return Some(*only);
    }
    if candidates.len() == 1 {
        return Some(candidates[0]);
    }
    if kind == RefKind::Call {
        let ci: Vec<&&Symbol> = candidates
            .iter()
            .filter(|s| s.name.eq_ignore_ascii_case(name))
            .collect();
        if let [only] = ci[..] {
            return Some(*only);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST_SRC: &str = r#"
use std::collections::HashMap;
use crate::other::helper;

pub struct Widget {
    pub count: u32,
}

pub enum Mode { On, Off }

pub trait Draw {
    fn draw(&self);
}

impl Draw for Widget {
    fn draw(&self) {
        helper(self.count);
    }
}

fn helper(n: u32) -> u32 {
    let m: HashMap<String, u32> = HashMap::new();
    process(&m);
    n + 1
}

fn caller() {
    helper(1);
}
"#;

    const TS_SRC: &str = r#"
import { helper } from "./other";
import Component from "./component";

export function main() {
  helper();
  const c = new Component(1);
  console.log(c);
}

export class Model {
  constructor() {}
  run() {
    return this.fetch();
  }
  fetch() {
    return 1;
  }
}

export interface Shape {
  area(): number;
}

export type Id = string;
export enum Color { Red, Green }
"#;

    /// Find a symbol by name in a file extract (panics when absent).
    fn sym<'a>(f: &'a FileExtract, name: &str) -> &'a Symbol {
        f.symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("symbol {name} not found"))
    }

    #[test]
    fn extracts_rust_symbols_with_kinds() {
        let f = extract_file("w.rs", RUST_SRC, Lang::Rust);
        assert_eq!(sym(&f, "Widget").kind, SymbolKind::Struct);
        assert_eq!(sym(&f, "Mode").kind, SymbolKind::Enum);
        assert_eq!(sym(&f, "Draw").kind, SymbolKind::Trait);
        assert_eq!(sym(&f, "helper").kind, SymbolKind::Function);
        // Method vs function: draw lives inside impl/trait, helper does not.
        let draws: Vec<&Symbol> = f.symbols.iter().filter(|s| s.name == "draw").collect();
        assert_eq!(draws.len(), 2, "trait declaration + impl method");
        assert!(draws.iter().all(|s| s.kind == SymbolKind::Method));
        // The impl block itself is a symbol named after its type.
        let impls: Vec<&Symbol> = f
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Impl)
            .collect();
        assert_eq!(impls.len(), 1);
        assert_eq!(impls[0].name, "Widget");
    }

    #[test]
    fn rust_line_ranges_cover_definition() {
        let f = extract_file("w.rs", RUST_SRC, Lang::Rust);
        let helper = sym(&f, "helper");
        assert!(helper.end_line > helper.start_line, "multi-line fn range");
        let widget = sym(&f, "Widget");
        assert!(widget.end_line >= widget.start_line);
    }

    #[test]
    fn module_symbol_anchors_file() {
        let f = extract_file("w.rs", RUST_SRC, Lang::Rust);
        assert_eq!(f.symbols[0].kind, SymbolKind::Module);
        assert_eq!(f.symbols[0].id, "w.rs::module");
        assert_eq!(f.symbols[0].file, "w.rs");
        assert_eq!(f.symbols[0].end_line, RUST_SRC.lines().count());
    }

    #[test]
    fn rust_calls_recorded_from_enclosing_scope() {
        let f = extract_file("w.rs", RUST_SRC, Lang::Rust);
        // caller() calls helper(1) — ref anchored to caller's id.
        let caller = sym(&f, "caller");
        assert!(f
            .refs
            .iter()
            .any(|r| { r.kind == RefKind::Call && r.name == "helper" && r.from == caller.id }));
        // draw() calls helper — ref anchored to the impl's draw, not caller.
        let draw_impl = f
            .symbols
            .iter()
            .find(|s| s.name == "draw" && s.start_line > 15)
            .unwrap();
        assert!(f
            .refs
            .iter()
            .any(|r| { r.kind == RefKind::Call && r.name == "helper" && r.from == draw_impl.id }));
        // `process` is unresolved at extract time (no definition anywhere).
        assert!(f.refs.iter().any(|r| r.name == "process"));
    }

    #[test]
    fn rust_use_declarations_record_import_refs() {
        let f = extract_file("w.rs", RUST_SRC, Lang::Rust);
        // `use std::collections::HashMap` → import ref for HashMap (leaf).
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Import && r.name == "HashMap"));
        // `use crate::other::helper` → import ref for helper.
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Import && r.name == "helper"));
        // Imports anchor to the module symbol.
        let module = &f.symbols[0];
        assert!(f
            .refs
            .iter()
            .all(|r| r.kind != RefKind::Import || r.from == module.id));
    }

    #[test]
    fn extracts_ts_symbols_and_calls() {
        let f = extract_file("m.ts", TS_SRC, Lang::Ts);
        assert_eq!(sym(&f, "main").kind, SymbolKind::Function);
        assert_eq!(sym(&f, "Model").kind, SymbolKind::Class);
        for m in ["constructor", "run", "fetch"] {
            assert_eq!(sym(&f, m).kind, SymbolKind::Method, "{m} must be a method");
        }
        assert_eq!(sym(&f, "Shape").kind, SymbolKind::Interface);
        assert_eq!(sym(&f, "Id").kind, SymbolKind::TypeAlias);
        assert_eq!(sym(&f, "Color").kind, SymbolKind::TsEnum);
        // Calls: helper() and new Component() recorded from main's scope.
        let main_id = sym(&f, "main").id.clone();
        assert!(f
            .refs
            .iter()
            .any(|r| { r.kind == RefKind::Call && r.name == "helper" && r.from == main_id }));
        assert!(f
            .refs
            .iter()
            .any(|r| { r.kind == RefKind::Call && r.name == "Component" && r.from == main_id }));
    }

    #[test]
    fn extracts_tsx_imports() {
        let f = extract_file("c.tsx", TS_SRC, Lang::Tsx);
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Import && r.name == "helper"));
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Import && r.name == "Component"));
        // Imports anchor to the module symbol.
        let module = &f.symbols[0];
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Import && r.from == module.id && r.name == "helper"));
    }

    const JS_SRC: &str = r#"
import { helper } from "./other";

export function main() {
  helper();
  const c = new Widget(1);
}

export class Widget {
  run() {
    return this.fetch();
  }
}
"#;

    #[test]
    fn extracts_js_symbols_and_calls() {
        // JS shares the TS walk — the JavaScript grammar uses the same
        // node kinds for everything visit_ts handles.
        let f = extract_file("m.js", JS_SRC, Lang::Js);
        assert_eq!(sym(&f, "main").kind, SymbolKind::Function);
        assert_eq!(sym(&f, "Widget").kind, SymbolKind::Class);
        assert_eq!(sym(&f, "run").kind, SymbolKind::Method);
        let main_id = sym(&f, "main").id.clone();
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "helper" && r.from == main_id));
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "Widget" && r.from == main_id));
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Import && r.name == "helper"));
    }

    #[test]
    fn extracts_js_arrow_const_functions() {
        // HIGH-1 (review 2026-09-11): function-valued const/let bindings —
        // the dominant jest/vitest test shape — must yield Function
        // symbols; non-function bindings still yield nothing.
        let src = "const testFoo = () => {
  helper();
};
const c = new Widget(1);
let v = function () { return 1; };
const gen = function* () { yield 1; };
const a = async () => { await helper(); };
function* genDecl() { yield 1; }
";
        let f = extract_file("m.js", src, Lang::Js);
        assert_eq!(sym(&f, "testFoo").kind, SymbolKind::Function);
        assert_eq!(sym(&f, "v").kind, SymbolKind::Function);
        // Round-2 LOW-1: the generator expression kind is `generator_function`
        // (not `generator_function_expression`), and async arrows are still
        // `arrow_function` nodes — both pinned here.
        assert_eq!(sym(&f, "gen").kind, SymbolKind::Function);
        assert_eq!(sym(&f, "a").kind, SymbolKind::Function);
        // Round-3 LOW-1: a top-level generator DECLARATION is a separate
        // grammar rule (`generator_function_declaration`) — pinned here.
        assert_eq!(sym(&f, "genDecl").kind, SymbolKind::Function);
        assert!(!f.symbols.iter().any(|s| s.name == "c"));
        let foo_id = sym(&f, "testFoo").id.clone();
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "helper" && r.from == foo_id));
    }

    const PY_SRC: &str = r#"
from helpers import assist, util as u
import os.path

class Greeter:
    def greet(self):
        return assist(self)

def main():
    g = Greeter()
    return g.greet()
"#;

    #[test]
    fn extracts_python_symbols_and_calls() {
        let f = extract_file("m.py", PY_SRC, Lang::Python);
        assert_eq!(sym(&f, "Greeter").kind, SymbolKind::Class);
        assert_eq!(sym(&f, "greet").kind, SymbolKind::Method);
        assert_eq!(sym(&f, "main").kind, SymbolKind::Function);
        let greet_id = sym(&f, "greet").id.clone();
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "assist" && r.from == greet_id));
        // g.greet() — the attribute call reduces to its final segment.
        let main_id = sym(&f, "main").id.clone();
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "greet" && r.from == main_id));
        // from helpers import assist, util as u → assist + util (originals);
        // the module prefix is skipped — it is not used bare in code.
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Import && r.name == "assist"));
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Import && r.name == "util"));
        assert!(
            !f.refs.iter().any(|r| r.name == "helpers"),
            "the from-module prefix is not an import ref"
        );
        // import os.path → the leaf module.
        assert!(f.refs.iter().any(|r| r.kind == RefKind::Import && r.name == "path"));
    }

    const RB_SRC: &str = r#"
module App
  class Greeter
    def greet
      assist(self)
    end
  end
end

def main
  g = Greeter.new
  g.greet
end
"#;

    #[test]
    fn extracts_ruby_symbols_and_calls() {
        let f = extract_file("m.rb", RB_SRC, Lang::Ruby);
        assert_eq!(sym(&f, "App").kind, SymbolKind::Module);
        assert_eq!(sym(&f, "Greeter").kind, SymbolKind::Class);
        assert_eq!(sym(&f, "greet").kind, SymbolKind::Method);
        assert_eq!(sym(&f, "main").kind, SymbolKind::Function);
        let greet_id = sym(&f, "greet").id.clone();
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "assist" && r.from == greet_id));
        // Greeter.new — the `new` call targets the class.
        let main_id = sym(&f, "main").id.clone();
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "new" && r.from == main_id));
        // g.greet — a receiver call reduces to the method name.
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "greet" && r.from == main_id));
    }

    const PHP_SRC: &str = r#"
<?php
use App\Util\helper;

class Greeter {
    public function greet(): string {
        return helper($this->name);
    }
}

function main() {
    $g = new Greeter();
    return $g->greet();
}
"#;

    #[test]
    fn extracts_php_symbols_and_calls() {
        let f = extract_file("m.php", PHP_SRC, Lang::Php);
        assert_eq!(sym(&f, "Greeter").kind, SymbolKind::Class);
        assert_eq!(sym(&f, "greet").kind, SymbolKind::Method);
        assert_eq!(sym(&f, "main").kind, SymbolKind::Function);
        let greet_id = sym(&f, "greet").id.clone();
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "helper" && r.from == greet_id));
        // new Greeter() — object creation records the class call.
        let main_id = sym(&f, "main").id.clone();
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "Greeter" && r.from == main_id));
        // $g->greet() — member call reduces to the method name.
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "greet" && r.from == main_id));
        // use App\Util\helper → import ref for the leaf.
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Import && r.name == "helper"));
    }

    const GO_SRC: &str = r#"
package main

import (
    "fmt"
    "github.com/x/y/helper"
)

type Greeter struct {
    Name string
}

type Reader interface {
    Read() error
}

func (g *Greeter) greet() string {
    return helper(g.Name)
}

func main() {
    g := &Greeter{}
    fmt.Println(g.greet())
}
"#;

    #[test]
    fn extracts_go_symbols_and_calls() {
        let f = extract_file("m.go", GO_SRC, Lang::Go);
        assert_eq!(sym(&f, "Greeter").kind, SymbolKind::Struct);
        assert_eq!(sym(&f, "Reader").kind, SymbolKind::Interface);
        assert_eq!(sym(&f, "greet").kind, SymbolKind::Method);
        assert_eq!(sym(&f, "main").kind, SymbolKind::Function);
        let greet_id = sym(&f, "greet").id.clone();
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "helper" && r.from == greet_id));
        // fmt.Println(...) — selector call reduces to the final segment.
        let main_id = sym(&f, "main").id.clone();
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "Println" && r.from == main_id));
        // imports: "fmt" and "helper" (path leaves).
        assert!(f.refs.iter().any(|r| r.kind == RefKind::Import && r.name == "fmt"));
        assert!(f.refs.iter().any(|r| r.kind == RefKind::Import && r.name == "helper"));
    }

    const JAVA_SRC: &str = r#"
import com.example.helper;

public class Greeter {
    private String name;

    public String greet() {
        return helper(name);
    }
}

interface Speaker {
    String speak();
}

enum Mode { ON, OFF }

class Main {
    void run() {
        Greeter g = new Greeter();
        g.greet();
    }
}
"#;

    #[test]
    fn extracts_java_symbols_and_calls() {
        let f = extract_file("M.java", JAVA_SRC, Lang::Java);
        assert_eq!(sym(&f, "Greeter").kind, SymbolKind::Class);
        assert_eq!(sym(&f, "greet").kind, SymbolKind::Method);
        assert_eq!(sym(&f, "Speaker").kind, SymbolKind::Interface);
        assert_eq!(sym(&f, "Mode").kind, SymbolKind::Enum);
        assert_eq!(sym(&f, "run").kind, SymbolKind::Method);
        let greet_id = sym(&f, "greet").id.clone();
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "helper" && r.from == greet_id));
        // new Greeter() — object creation records the class call.
        let run_id = sym(&f, "run").id.clone();
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "Greeter" && r.from == run_id));
        // g.greet() — method invocation reduces to the method name.
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "greet" && r.from == run_id));
        // import com.example.helper → the final segment.
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Import && r.name == "helper"));
    }

    const C_SRC: &str = r#"
typedef struct Point { int x; int y; } Point;

struct Shape {
    int area;
};

enum Mode { ON, OFF };

int helper(int n) {
    return n + 1;
}

int main(void) {
    struct Point p = { 1, 2 };
    return helper(p.x);
}
"#;

    #[test]
    fn extracts_c_symbols_and_calls() {
        let f = extract_file("m.c", C_SRC, Lang::C);
        // `typedef struct Point {...} Point` yields BOTH the struct and the
        // typedef alias.
        let points: Vec<&Symbol> = f.symbols.iter().filter(|s| s.name == "Point").collect();
        assert_eq!(points.len(), 2, "struct_specifier + typedef declarator");
        assert!(points.iter().any(|s| s.kind == SymbolKind::Struct));
        assert!(points.iter().any(|s| s.kind == SymbolKind::TypeAlias));
        assert_eq!(sym(&f, "Shape").kind, SymbolKind::Struct);
        assert_eq!(sym(&f, "Mode").kind, SymbolKind::Enum);
        assert_eq!(sym(&f, "helper").kind, SymbolKind::Function);
        assert_eq!(sym(&f, "main").kind, SymbolKind::Function);
        // Struct data members yield no symbols.
        assert!(!f.symbols.iter().any(|s| s.name == "x" || s.name == "area"));
        let main_id = sym(&f, "main").id.clone();
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "helper" && r.from == main_id));
    }

    const CPP_SRC: &str = r#"
#include <memory>
using std::string;

class Greeter {
public:
    string greet();
};

string Greeter::greet() {
    return helper();
}

int main() {
    Greeter g;
    return 0;
}
"#;

    #[test]
    fn extracts_cpp_symbols_and_calls() {
        let f = extract_file("m.cpp", CPP_SRC, Lang::Cpp);
        assert_eq!(sym(&f, "Greeter").kind, SymbolKind::Class);
        // The in-class declaration is a Method (field_declaration with a
        // function declarator); the out-of-line `Greeter::greet` definition
        // reduces to its final segment as a top-level Function.
        let greets: Vec<&Symbol> = f.symbols.iter().filter(|s| s.name == "greet").collect();
        assert_eq!(greets.len(), 2, "in-class declaration + out-of-line definition");
        assert!(greets.iter().any(|s| s.kind == SymbolKind::Method));
        assert!(greets.iter().any(|s| s.kind == SymbolKind::Function));
        let out_of_line = greets
            .iter()
            .find(|s| s.kind == SymbolKind::Function)
            .unwrap()
            .id
            .clone();
        assert!(f.refs.iter().any(|r| r.kind == RefKind::Call && r.name == "helper" && r.from == out_of_line));
        // using std::string → import ref for the leaf.
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Import && r.name == "string"));
    }

    const CS_SRC: &str = r#"
using System.Linq;

namespace App {
    public class Greeter {
        private string name;

        public string Greet() {
            return helper(name);
        }
    }

    public interface Speaker {
        string Speak();
    }

    public enum Mode { On, Off }

    class Main {
        void Run() {
            var g = new Greeter();
            g.Greet();
        }
    }
}
"#;

    #[test]
    fn extracts_csharp_symbols_and_calls() {
        let f = extract_file("M.cs", CS_SRC, Lang::CSharp);
        assert_eq!(sym(&f, "Greeter").kind, SymbolKind::Class);
        assert_eq!(sym(&f, "Greet").kind, SymbolKind::Method);
        assert_eq!(sym(&f, "Speaker").kind, SymbolKind::Interface);
        assert_eq!(sym(&f, "Mode").kind, SymbolKind::Enum);
        assert_eq!(sym(&f, "Run").kind, SymbolKind::Method);
        let greet_id = sym(&f, "Greet").id.clone();
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "helper" && r.from == greet_id));
        let run_id = sym(&f, "Run").id.clone();
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "Greeter" && r.from == run_id));
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "Greet" && r.from == run_id));
        // using System.Linq → the final segment.
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Import && r.name == "Linq"));
    }

    const HTML_SRC: &str = r#"<!DOCTYPE html>
<html>
<head>
<script type="text/template">
  function not_a_real_function() {}
</script>
</head>
<body>
<script>
  function main() {
    helper();
  }
  class Widget {
    run() { return this.fetch(); }
  }
</script>
<script type="application/json">
  {"fn": "nope"}
</script>
</body>
</html>
"#;

    #[test]
    fn extracts_html_script_blocks() {
        let f = extract_file("page.html", HTML_SRC, Lang::Html);
        assert_eq!(sym(&f, "main").kind, SymbolKind::Function);
        assert_eq!(sym(&f, "Widget").kind, SymbolKind::Class);
        assert_eq!(sym(&f, "run").kind, SymbolKind::Method);
        // Non-JS script types are skipped entirely.
        assert!(!f.symbols.iter().any(|s| s.name == "not_a_real_function"));
        // Line offsets: definitions land at their true lines in the HTML
        // file, not at their lines within the script block.
        let expected = HTML_SRC
            .lines()
            .position(|l| l.contains("function main()"))
            .unwrap()
            + 1;
        assert_eq!(sym(&f, "main").start_line, expected);
        // Calls anchor to the enclosing function.
        let main_id = sym(&f, "main").id.clone();
        assert!(f
            .refs
            .iter()
            .any(|r| r.kind == RefKind::Call && r.name == "helper" && r.from == main_id));
    }

    #[test]
    fn resolve_edges_same_file_first() {
        let files = vec![
            extract_file("w.rs", RUST_SRC, Lang::Rust),
            extract_file("o.rs", "pub fn helper() {}", Lang::Rust),
        ];
        let r = resolve_edges(&files);
        // caller → helper resolves to the SAME-FILE helper in w.rs.
        let caller_id = files[0]
            .symbols
            .iter()
            .find(|s| s.name == "caller")
            .unwrap()
            .id
            .clone();
        let w_helper = files[0]
            .symbols
            .iter()
            .find(|s| s.name == "helper")
            .unwrap()
            .id
            .clone();
        assert!(r.edges.iter().any(|(from, to, k)| {
            from == &caller_id && to == &w_helper && *k == EdgeKind::Calls
        }));
        // `process` (called in helper) has no definition → unresolved count.
        assert!(r.unresolved >= 1);
    }

    #[test]
    fn resolve_edges_import_targets_definition_file() {
        let files = vec![
            extract_file("m.ts", TS_SRC, Lang::Ts),
            extract_file("other.ts", "export function helper() {}", Lang::Ts),
            extract_file(
                "component.tsx",
                "export default class Component {}",
                Lang::Tsx,
            ),
        ];
        let r = resolve_edges(&files);
        let module_id = "m.ts::module".to_string();
        assert!(r.edges.iter().any(|(from, to, k)| {
            from == &module_id && to.starts_with("other.ts::helper") && *k == EdgeKind::Imports
        }));
        assert!(r.edges.iter().any(|(from, to, k)| {
            from == &module_id
                && to.starts_with("component.tsx::Component")
                && *k == EdgeKind::Imports
        }));
    }

    #[test]
    fn resolve_edges_ambiguous_name_dropped() {
        let files = vec![
            extract_file("m.rs", "fn main() { helper(); }", Lang::Rust),
            extract_file("a.rs", "pub fn helper() {}", Lang::Rust),
            extract_file("b.rs", "pub fn helper() {}", Lang::Rust),
        ];
        let r = resolve_edges(&files);
        // Two same-named helpers and no same-file candidate → no call edge.
        // (Contains edges to the same symbols are fine and expected.)
        assert!(!r.edges.iter().any(|(_, to, k)| {
            *k == EdgeKind::Calls
                && (to.starts_with("a.rs::helper") || to.starts_with("b.rs::helper"))
        }));
    }

    #[test]
    fn contains_edges_link_module_impl_and_methods() {
        let f = extract_file("w.rs", RUST_SRC, Lang::Rust);
        let module = &f.symbols[0].id;
        // Module contains every top-level item.
        for name in ["Widget", "Mode", "Draw", "helper", "caller"] {
            let s = sym(&f, name);
            assert!(
                f.contains.iter().any(|(p, c)| p == module && c == &s.id),
                "module must contain {name}"
            );
        }
        // The impl block contains its draw method; the trait contains its own.
        let impl_sym = f
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Impl)
            .unwrap();
        let impl_draw = f
            .symbols
            .iter()
            .find(|s| s.name == "draw" && s.start_line > impl_sym.start_line)
            .unwrap();
        assert!(f
            .contains
            .iter()
            .any(|(p, c)| p == &impl_sym.id && c == &impl_draw.id));
        // Containment flows into resolve_edges as Contains edges.
        let r = resolve_edges(std::slice::from_ref(&f));
        assert!(r.edges.iter().any(|(from, to, k)| {
            from == &impl_sym.id && to == &impl_draw.id && *k == EdgeKind::Contains
        }));
    }

    #[test]
    fn kind_roundtrips_through_strings() {
        for k in [
            SymbolKind::Function,
            SymbolKind::Method,
            SymbolKind::Struct,
            SymbolKind::Enum,
            SymbolKind::Trait,
            SymbolKind::Impl,
            SymbolKind::TypeAlias,
            SymbolKind::Class,
            SymbolKind::Interface,
            SymbolKind::TsEnum,
            SymbolKind::Module,
        ] {
            assert_eq!(SymbolKind::from_str(k.as_str()), Some(k));
        }
        assert_eq!(SymbolKind::from_str("nope"), None);
        for k in [EdgeKind::Calls, EdgeKind::Imports, EdgeKind::Contains] {
            assert_eq!(EdgeKind::from_str(k.as_str()), Some(k));
        }
        assert_eq!(EdgeKind::from_str("nope"), None);
    }

    #[test]
    fn broken_source_never_fails() {
        // Syntactically invalid input still yields a (partial) tree — the
        // extractor must return the module symbol, not error.
        let f = extract_file("bad.rs", "fn broken( {", Lang::Rust);
        assert_eq!(f.symbols[0].kind, SymbolKind::Module);
        let t = extract_file("bad.ts", "function broken({", Lang::Ts);
        assert_eq!(t.symbols[0].kind, SymbolKind::Module);
    }
}
