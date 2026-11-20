// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `find_code` — semantic code search (plan d3336a8a step 11).
//!
//! Describe what you are looking for in words, before you know any name:
//! the tool embeds the query with the SAME code model the symbol-embed pass
//! uses (`jina-embeddings-v2-base-code`, through the process-global slot or
//! the shared `mnemo-models` host), ranks every stored symbol vector by
//! cosine, and blends that with the content index's bm25 and a name match:
//!
//! ```text
//! score = 0.6·cosine + 0.25·fts + 0.15·name
//! ```
//!
//! - `cosine` — the query's dot product with the symbol's unit-normalised
//!   vector, floored at 0 (a negative cosine carries no signal). Symbols
//!   without a vector contribute 0 here but still rank on the other terms,
//!   so a strong text or name hit is never dropped because the embed pass
//!   has not reached it yet.
//! - `fts` — the content index's bm25, min-max normalised over THIS query's
//!   candidate set (best hit 1.0, worst 0.0; a single hit 0.5 — there is no
//!   spread to normalise against). A content line becomes a symbol by its
//!   innermost enclosing definition; the best rank per symbol wins.
//! - `name` — [`name_match`]'s score, defined there.
//!
//! Output is one line per symbol, `<file>:<start>-<end> <kind> <name> —
//! <signature>`, plus the doc comment's first line on an indented second
//! line when the symbol has one.
//!
//! When no query embedder is available yet (the shared host is still
//! starting, the model is not installed, or a failed build sits in its
//! retry window) the tool NEVER waits for the tens-of-seconds build: it
//! starts that build in the background and answers with the
//! `code embeddings are still being built (N of M symbols)` note plus the
//! content index's hits, in the `search` tool's `path:line: text` shape.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::codegraph::store::ContentHit;
use crate::codegraph::{CodeGraph, SymbolSearchIndex};
use crate::error::Result;
use crate::memory::embedder::EmbedderStatus;
use crate::provider::ToolSchema;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// Ranked results returned when the caller names no limit.
const DEFAULT_LIMIT: usize = 8;

/// The most results a caller may ask for — the step's contract.
const MAX_LIMIT: usize = 20;

/// Content-index hits pulled per query before they are mapped to symbols.
/// The pool only has to be deep enough that the best lexical hit for each
/// plausible symbol is in it; 300 lines covers it comfortably.
const FTS_POOL: usize = 300;

/// The most query tokens sent to the content index — a long question would
/// otherwise widen the OR query into noise.
const MAX_QUERY_TOKENS: usize = 16;

/// The weight of the cosine term in the blended score.
const COSINE_WEIGHT: f32 = 0.6;

/// The weight of the normalised bm25 term in the blended score.
const FTS_WEIGHT: f32 = 0.25;

/// The weight of the name-match term in the blended score.
const NAME_WEIGHT: f32 = 0.15;

/// How much of a signature or doc line the output shows.
const TEXT_MAX_CHARS: usize = 160;

/// The `find_code` arguments.
#[derive(Debug, Deserialize)]
struct FindCodeArgs {
    /// What to look for, in words.
    query: String,
    /// How many results (default 8, max 20).
    limit: Option<usize>,
}

/// The `find_code` tool — finds code by meaning (plan d3336a8a step 11).
pub struct FindCodeTool {
    /// The project's code graph. `None` → the tool answers an explanatory
    /// error; the factory only registers it with a graph.
    graph: Option<Arc<CodeGraph>>,
}

impl FindCodeTool {
    /// Create the tool bound to a project's code graph.
    pub fn new(graph: Option<Arc<CodeGraph>>) -> Self {
        Self { graph }
    }
}

/// Words a code question carries but a symbol never contains — dropped from
/// the query tokens so they cannot dominate the content-index OR.
const STOPWORDS: &[&str] = &[
    "a", "about", "an", "and", "any", "are", "as", "at", "be", "been", "but", "by", "can", "could",
    "did", "do", "does", "for", "from", "had", "has", "have", "how", "if", "in", "into", "is", "it",
    "its", "just", "like", "me", "my", "no", "not", "of", "on", "only", "or", "our", "out", "over",
    "should", "so", "some", "than", "that", "the", "their", "them", "then", "there", "they", "this",
    "to", "under", "up", "us", "was", "we", "were", "what", "when", "where", "which", "who", "why",
    "will", "with", "would", "you", "your",
];

/// The query's searchable tokens (plan d3336a8a step 11): lowercased words
/// split on non-alphanumerics, common question stopwords dropped, duplicate
/// tokens removed in first-seen order, at most [`MAX_QUERY_TOKENS`] kept.
fn query_tokens(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for token in query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(str::to_lowercase)
    {
        if STOPWORDS.contains(&token.as_str()) || out.contains(&token) {
            continue;
        }
        out.push(token);
        if out.len() == MAX_QUERY_TOKENS {
            break;
        }
    }
    out
}

/// An identifier flattened for comparison: its [`name_words`] joined with
/// single spaces — lowercased, snake_case separators and camelCase humps
/// alike. `stale_edit_gate`, `StaleEditGate` and `stale edit gate` all
/// flatten to `stale edit gate`.
fn flatten_ident(text: &str) -> String {
    name_words(text).join(" ")
}

/// A name split into its words: snake_case separators, camelCase humps and
/// acronym boundaries (`HTTPServer` → `http`, `server`), all lowercased.
fn name_words(name: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut previous: Option<char> = None;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            previous = None;
            continue;
        }
        if c.is_uppercase() {
            let hump = previous.is_some_and(|p| p.is_lowercase() || p.is_numeric());
            if hump && !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        }
        current.extend(c.to_lowercase());
        previous = Some(c);
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// The name-match score of one symbol name for a query (plan d3336a8a step
/// 11), in `0.0..=1.0`:
///
/// - `1.0` — the name IS the query: both flattened by [`flatten_ident`], so
///   `recall peek` matches `recall_peek` and `RecallPeek`.
/// - `0.5` — query and name contain one another, either direction, with the
///   contained side at least 4 characters (shorter fragments match too much).
/// - otherwise `0.5 × (fraction of the name's [`name_words`] that a query
///   token matches)`.
/// - `0.0` — no relation (an empty name never matches).
fn name_match(query: &str, name: &str) -> f32 {
    let flat_query = flatten_ident(query);
    let flat_name = flatten_ident(name);
    if flat_name.is_empty() {
        return 0.0;
    }
    if flat_query == flat_name {
        return 1.0;
    }
    if flat_name.len() >= 4 && flat_query.contains(&flat_name) {
        return 0.5;
    }
    if flat_query.len() >= 4 && flat_name.contains(&flat_query) {
        return 0.5;
    }
    let words = name_words(name);
    if words.is_empty() {
        return 0.0;
    }
    let tokens = query_tokens(query);
    let matched = words
        .iter()
        .filter(|word| tokens.iter().any(|t| t == *word))
        .count();
    0.5 * (matched as f32 / words.len() as f32)
}

/// One candidate symbol's score breakdown (plan d3336a8a step 11).
struct Ranked {
    /// The symbol's stable id.
    symbol_id: String,
    /// 0.6·cosine term input.
    cosine: f32,
    /// 0.25·bm25 term input, normalised over the candidate set.
    fts: f32,
    /// 0.15·name term input.
    name: f32,
    /// The blended score the sort uses.
    score: f32,
}

/// One prepared answer from the blocking ranking pass.
enum Prepared {
    /// The blended ranking, already rendered.
    Ranked(String),
    /// No symbol carries a vector yet — the text fallback plus its counts.
    NoVectors((usize, usize)),
}

/// The meaning term for one stored vector (plan d3336a8a step 11): the dot
/// product of the unit-normalised stored vector with the raw query vector,
/// divided by the query's norm and floored at 0 (a negative cosine carries
/// no signal). Mismatched widths score 0 — a vector of another model's
/// dimension can never be compared.
fn cosine_term(vector: &[f32], query: &[f32], query_norm: f32) -> f32 {
    if vector.len() != query.len() || query_norm <= 1e-6 {
        return 0.0;
    }
    let dot: f32 = vector.iter().zip(query).map(|(a, b)| a * b).sum::<f32>() / query_norm;
    dot.max(0.0)
}

/// Blend the three terms for one query (plan d3336a8a step 11) and return
/// the top `limit` candidates, best first:
///
/// ```text
/// score = 0.6·cosine + 0.25·fts + 0.15·name
/// ```
///
/// `cosine` is the dot product with the unit-normalised symbol vector,
/// floored at 0. `fts` is the content index's bm25 mapped to its innermost
/// enclosing symbol and min-max normalised over this query's lexical
/// candidates — the best hit is 1.0, the worst 0.0, and a lone hit is 0.5
/// (there is no spread to normalise against; bm25 is lower-is-better, so
/// the mapping is `(max − rank) / (max − min)`). `name` is [`name_match`].
///
/// A candidate enters on ANY of the three terms, so a strong text or name
/// hit still ranks while its vector is pending — it simply contributes 0
/// to the cosine term.
fn rank(
    index: &SymbolSearchIndex,
    hits: &[ContentHit],
    query: &str,
    query_vec: &[f32],
    limit: usize,
) -> Vec<Ranked> {
    let mut candidates: HashMap<String, Ranked> = HashMap::new();
    let blank = |id: &str| Ranked {
        symbol_id: id.to_string(),
        cosine: 0.0,
        fts: 0.0,
        name: 0.0,
        score: 0.0,
    };

    // The meaning term: every vector the snapshot holds.
    let query_norm = query_vec.iter().map(|v| v * v).sum::<f32>().sqrt();
    if query_norm > 1e-6 {
        for (id, vector) in index.vectors() {
            let cosine = cosine_term(vector, query_vec, query_norm);
            if cosine <= 0.0 {
                continue;
            }
            let mut entry = blank(id);
            entry.cosine = cosine;
            candidates.insert(id.to_string(), entry);
        }
    }

    // The lexical term: content hits mapped to their innermost enclosing
    // symbol, the best (lowest) bm25 rank per symbol.
    let mut ranks: HashMap<String, f64> = HashMap::new();
    for hit in hits {
        let Some(symbol) = index.enclosing(&hit.path, hit.line) else {
            continue;
        };
        if index.text(&symbol.id).is_none() {
            continue;
        }
        ranks
            .entry(symbol.id.clone())
            .and_modify(|current| *current = current.min(hit.rank))
            .or_insert(hit.rank);
    }
    if !ranks.is_empty() {
        let mut lowest = f64::INFINITY;
        let mut highest = f64::NEG_INFINITY;
        for rank in ranks.values() {
            lowest = lowest.min(*rank);
            highest = highest.max(*rank);
        }
        let normalise = |rank: f64| -> f32 {
            if (highest - lowest).abs() < f64::EPSILON {
                0.5
            } else {
                (((highest - rank) / (highest - lowest)) as f32).clamp(0.0, 1.0)
            }
        };
        for (id, rank) in &ranks {
            let entry = candidates.entry(id.clone()).or_insert_with(|| blank(id));
            entry.fts = normalise(*rank);
        }
    }

    // The name term: every renderable symbol, whether or not it has a
    // vector.
    for symbol in index.symbols() {
        if index.text(&symbol.id).is_none() {
            continue;
        }
        let name = name_match(query, &symbol.name);
        if name <= 0.0 {
            continue;
        }
        let entry = candidates
            .entry(symbol.id.clone())
            .or_insert_with(|| blank(&symbol.id));
        entry.name = name;
    }

    let mut ranked: Vec<Ranked> = candidates
        .into_values()
        .map(|mut entry| {
            entry.score = COSINE_WEIGHT * entry.cosine
                + FTS_WEIGHT * entry.fts
                + NAME_WEIGHT * entry.name;
            entry
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.symbol_id.cmp(&b.symbol_id))
    });
    ranked.truncate(limit);
    ranked
}

/// Clip text to `max` characters, ellipsizing (never splitting a character).
fn clip(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Render ranked symbols, one entry per symbol: `<file>:<start>-<end>
/// <kind> <name> — <signature>`, plus the doc comment's first non-empty
/// line on an indented second line (plan d3336a8a step 11).
fn render(index: &SymbolSearchIndex, ranked: &[Ranked]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for entry in ranked {
        let (Some(symbol), Some((signature, doc))) = (
            index.symbol(&entry.symbol_id),
            index.text(&entry.symbol_id),
        ) else {
            continue;
        };
        lines.push(format!(
            "{}:{}-{} {} {} — {}",
            symbol.file,
            symbol.start_line,
            symbol.end_line,
            symbol.kind.as_str(),
            symbol.name,
            clip(signature, TEXT_MAX_CHARS)
        ));
        if let Some(first) = doc.lines().map(str::trim).find(|line| !line.is_empty()) {
            lines.push(format!("    {}", clip(first, TEXT_MAX_CHARS)));
        }
    }
    lines.join("\n")
}

#[async_trait]
impl Tool for FindCodeTool {
    fn name(&self) -> &str {
        "find_code"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "find_code",
            "Find code by what it does. Describe the behaviour in words, not its \
             name, and get the closest functions and types, one per line: \
             `<file>:<start>-<end> <kind> <name> — <signature>`, with the first \
             line of the doc comment below. Use it when you don't know a name \
             yet; once you have one, search or graph_search are faster and \
             exact. While code embeddings are still being built, it says so and \
             returns text matches instead.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "What the code does, in words — e.g. \"rejects a stale edit when the file changed since the read\"."},
                    "limit": {"type": "integer", "description": "How many results (default 8, max 20)."}
                },
                "required": ["query"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: FindCodeArgs = match serde_json::from_value(args.clone()) {
            Ok(args) => args,
            Err(e) => {
                return ToolResult::error(crate::tool::error_message::sanitize_arguments_error_with(
                    self.name(),
                    &e,
                    &args,
                ))
            }
        };
        let query = args.query.trim().to_string();
        if query.is_empty() {
            return ToolResult::error(
                "find_code needs a non-empty query — describe what the code does in words",
            );
        }
        let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let Some(graph) = self.graph.clone() else {
            return ToolResult::error(
                "find_code needs this project's code graph, which is not available here",
            );
        };

        // Never wait for the embedder: the shared build runs tens of seconds
        // and a query must not hold a worker for it (plan d3336a8a step 11).
        let Some(embedder) = graph.query_embedder() else {
            return find_code_fallback(&graph, &query, limit, None).await;
        };
        // The embed pass refuses a degraded embedder's vectors; a query must
        // do the same — a Failed embedder answers hash-fallback vectors, and
        // ranking the query against those would be junk cosines.
        if matches!(embedder.status(), Some(EmbedderStatus::Failed)) {
            return find_code_fallback(&graph, &query, limit, None).await;
        }
        let vector = embedder.embed(&query).await;
        // Checked AGAIN after the call: the remote embedder flips to Failed
        // when its request fails, and the hash fallback it returns then is not
        // the model's vector. A zero vector carries no direction either.
        if matches!(embedder.status(), Some(EmbedderStatus::Failed))
            || vector.is_empty()
            || vector.iter().all(|value| *value == 0.0)
        {
            return find_code_fallback(&graph, &query, limit, None).await;
        }

        let model = embedder.model_id().to_string();
        let tokens = query_tokens(&query);
        let graph_for_task = Arc::clone(&graph);
        let query_for_task = query.clone();
        // Loading or refreshing the ~50 MB snapshot and the FTS query are
        // blocking work: run them on the blocking pool, never a worker.
        let prepared = tokio::task::spawn_blocking(move || -> Result<Prepared> {
            let index = graph_for_task.symbol_search(&model)?;
            if !index.has_vectors() {
                return Ok(Prepared::NoVectors((
                    index.vector_count(),
                    index.symbol_count(),
                )));
            }
            let hits = graph_for_task.content_hits_any(&tokens, FTS_POOL)?;
            let ranked = rank(&index, &hits, &query_for_task, &vector, limit);
            Ok(Prepared::Ranked(render(&index, &ranked)))
        })
        .await;

        match prepared {
            Ok(Ok(Prepared::Ranked(text))) => {
                if text.is_empty() {
                    ToolResult::success(
                        "no matching symbols — try different words, or `search` for a literal",
                    )
                } else {
                    ToolResult::success(text)
                }
            }
            Ok(Ok(Prepared::NoVectors(counts))) => {
                find_code_fallback(&graph, &query, limit, Some(counts)).await
            }
            Ok(Err(e)) => ToolResult::error(format!("find_code could not read the code index: {e}")),
            Err(e) => ToolResult::error(format!("find_code's ranking task failed: {e}")),
        }
    }
}

/// The text-only answer (plan d3336a8a step 11): a progress note, then the
/// content index's hits in the `search` tool's `path:line: text` shape.
///
/// The note is the step's `code embeddings are still being built (N of M
/// symbols)` while N < M; with every symbol embedded the embedder itself is
/// the missing piece (the shared host is starting, degraded, or a failed
/// build sits in its retry window) and the note says so; an index with no
/// embeddable symbol yet says that instead. `counts` carries numbers the
/// ranked path already read; `None` reads them here.
///
/// Both reads are SQLite and run on the blocking pool like the ranked path —
/// a tokio worker never touches the store here.
async fn find_code_fallback(
    graph: &Arc<CodeGraph>,
    query: &str,
    limit: usize,
    counts: Option<(usize, usize)>,
) -> ToolResult {
    let tokens = query_tokens(query);
    let graph_for_task = Arc::clone(graph);
    let read = tokio::task::spawn_blocking(move || -> ((usize, usize), Vec<ContentHit>) {
        let counts =
            counts.unwrap_or_else(|| graph_for_task.symbol_search_counts().unwrap_or((0, 0)));
        let hits = graph_for_task
            .content_hits_any(&tokens, limit)
            .unwrap_or_default();
        (counts, hits)
    })
    .await;
    let ((embedded, total), hits) = match read {
        Ok(read) => read,
        Err(e) => return ToolResult::error(format!("find_code's text search task failed: {e}")),
    };
    let mut out = if total == 0 {
        "the code index has no embeddable symbols yet".to_string()
    } else if embedded < total {
        format!("code embeddings are still being built ({embedded} of {total} symbols)")
    } else {
        "the code embedder is not ready; showing text matches instead".to_string()
    };
    if hits.is_empty() {
        out.push_str("\nno text matches either — try different words, or `search` for a literal");
    } else {
        for hit in hits {
            out.push_str(&format!("\n{}:{}: {}", hit.path, hit.line, hit.text.trim()));
        }
    }
    ToolResult::success(out)
}

#[cfg(all(test, feature = "embeddings"))]
mod tests {
    use super::*;
    use crate::memory::embedder::{Embedder, CODE_EMBEDDING_MODEL};

    /// A stub embedder answering one fixed vector for any text — the ranking
    /// tests need no model, no host and no download.
    struct FixedEmbedder {
        model: String,
        vector: Vec<f32>,
    }

    impl FixedEmbedder {
        fn new(vector: Vec<f32>) -> Arc<Self> {
            Arc::new(Self {
                model: CODE_EMBEDDING_MODEL.to_string(),
                vector,
            })
        }
    }

    #[async_trait]
    impl Embedder for FixedEmbedder {
        async fn embed(&self, _text: &str) -> Vec<f32> {
            self.vector.clone()
        }

        fn model_id(&self) -> &str {
            &self.model
        }

        fn dim(&self) -> usize {
            self.vector.len()
        }
    }

    /// Two real-shaped functions: one with a doc line about a counter, one
    /// about the stale-edit gate.
    const LIB: &str = "\
/// Adds one to the counter.
pub fn bump_counter(value: u32) -> u32 { value + 1 }

/// Rejects an edit when the file changed since the read.
pub fn stale_edit_gate(read_hash: &str) -> bool { !read_hash.is_empty() }
";

    /// A graph over a temp tree, indexed and ready.
    fn graph_with(files: &[(&str, &str)]) -> (tempfile::TempDir, CodeGraph) {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in files {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, body).unwrap();
        }
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        (dir, graph)
    }

    /// The stable id of a symbol by name.
    fn symbol_id(graph: &CodeGraph, name: &str) -> String {
        graph
            .view()
            .unwrap()
            .resolve(name)
            .first()
            .map(|symbol| symbol.id.clone())
            .unwrap_or_else(|| panic!("symbol {name} missing from the fixture graph"))
    }

    #[test]
    fn name_match_scores_exact_containment_and_overlap() {
        // Flattened equality across separators and camelCase.
        assert_eq!(name_match("stale edit gate", "stale_edit_gate"), 1.0);
        assert_eq!(name_match("StaleEditGate", "stale_edit_gate"), 1.0);
        // Containment either way, at 4+ characters.
        assert_eq!(
            name_match("where is the stale_edit_gate used", "stale_edit_gate"),
            0.5
        );
        assert_eq!(name_match("stale_edit_gate_mechanism", "stale_edit_gate"), 0.5);
        // Fraction of the name's words a query token matches, halved.
        assert!((name_match("silence timeout task", "silence_watch") - 0.25).abs() < 1e-6);
        assert!((name_match("silence timeout", "silence_watch") - 0.25).abs() < 1e-6);
        // No relation.
        assert_eq!(name_match("embeddings host", "bump_counter"), 0.0);
        assert_eq!(name_match("anything", ""), 0.0);
    }

    #[tokio::test]
    async fn output_follows_the_step_contract() {
        let (_dir, mut graph) = graph_with(&[("src/lib.rs", LIB)]);
        graph.arm_symbol_embedder_stub(FixedEmbedder::new(vec![1.0, 0.0, 0.0, 0.0]));
        let bump = symbol_id(&graph, "bump_counter");
        graph
            .store_symbol_vector_for_test(CODE_EMBEDDING_MODEL, &bump, &[1.0, 0.0, 0.0, 0.0])
            .unwrap();
        let tool = FindCodeTool::new(Some(Arc::new(graph)));
        let result = tool.execute(json!({"query": "adds one to the counter"})).await;
        assert!(result.success, "{}", result.output);
        let first = result.output.lines().next().expect("one result");
        assert!(
            first.starts_with("src/lib.rs:2-2 function bump_counter — "),
            "contract line drifted: {first}"
        );
        assert!(first.contains("pub fn bump_counter"), "{first}");
        let second = result.output.lines().nth(1).expect("the doc line");
        assert!(second.starts_with("    "), "indented doc line: {second}");
        assert!(second.contains("Adds one to the counter"), "{second}");
    }

    #[tokio::test]
    async fn answers_from_text_while_embeddings_are_missing() {
        // An unarmed graph has no code embedder at all — the query must not
        // wait for one and must still answer, from the content index.
        let (_dir, graph) = graph_with(&[("src/lib.rs", LIB)]);
        let tool = FindCodeTool::new(Some(Arc::new(graph)));
        let result = tool.execute(json!({"query": "rejects a stale edit"})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result
                .output
                .starts_with("code embeddings are still being built (0 of "),
            "the progress note leads: {}",
            result.output
        );
        assert!(
            result.output.contains("src/lib.rs:"),
            "text hits follow the note: {}",
            result.output
        );
        assert!(
            result.output.contains("stale_edit_gate"),
            "the matching line is shown: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn cosine_orders_the_results_with_a_stub_embedder() {
        let files: &[(&str, &str)] = &[
            ("src/a.rs", "pub fn alpha_thing() -> u32 { 1 }\n"),
            ("src/b.rs", "pub fn beta_thing() -> u32 { 2 }\n"),
        ];
        let (_dir, mut graph) = graph_with(files);
        graph.arm_symbol_embedder_stub(FixedEmbedder::new(vec![1.0, 0.0, 0.0, 0.0]));
        let beta = symbol_id(&graph, "beta_thing");
        graph
            .store_symbol_vector_for_test(CODE_EMBEDDING_MODEL, &beta, &[1.0, 0.0, 0.0, 0.0])
            .unwrap();
        let alpha = symbol_id(&graph, "alpha_thing");
        graph
            .store_symbol_vector_for_test(CODE_EMBEDDING_MODEL, &alpha, &[0.0, 1.0, 0.0, 0.0])
            .unwrap();
        let tool = FindCodeTool::new(Some(Arc::new(graph)));
        let result = tool
            .execute(json!({"query": "the thing that does the work"}))
            .await;
        let first = result.output.lines().next().expect("a result");
        assert!(
            first.contains("beta_thing"),
            "the aligned vector ranks first: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn a_name_hit_without_a_vector_still_ranks() {
        let (_dir, mut graph) = graph_with(&[("src/lib.rs", LIB)]);
        graph.arm_symbol_embedder_stub(FixedEmbedder::new(vec![1.0, 0.0, 0.0, 0.0]));
        // The other symbol has a weak vector; the name hit has none at all.
        let stale = symbol_id(&graph, "stale_edit_gate");
        graph
            .store_symbol_vector_for_test(
                CODE_EMBEDDING_MODEL,
                &stale,
                &[0.2, 0.979_795_9, 0.0, 0.0],
            )
            .unwrap();
        let tool = FindCodeTool::new(Some(Arc::new(graph)));
        let result = tool.execute(json!({"query": "bump_counter"})).await;
        let first = result.output.lines().next().expect("a result");
        assert!(
            first.contains("function bump_counter"),
            "the exact name hit outranks a weak cosine: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn limit_defaults_to_eight_and_caps_at_twenty() {
        let mut body = String::new();
        for i in 0..30 {
            body.push_str(&format!("pub fn handler_{i:02}() -> u32 {{ {i} }}\n"));
        }
        let (_dir, mut graph) = graph_with(&[("src/many.rs", &body)]);
        graph.arm_symbol_embedder_stub(FixedEmbedder::new(vec![1.0, 0.0, 0.0, 0.0]));
        let first = symbol_id(&graph, "handler_00");
        graph
            .store_symbol_vector_for_test(CODE_EMBEDDING_MODEL, &first, &[1.0, 0.0, 0.0, 0.0])
            .unwrap();
        let tool = FindCodeTool::new(Some(Arc::new(graph)));
        let count = |out: &str| out.lines().filter(|l| !l.starts_with(' ')).count();
        let default = tool.execute(json!({"query": "handler"})).await;
        assert_eq!(count(&default.output), 8, "{}", default.output);
        let capped = tool.execute(json!({"query": "handler", "limit": 50})).await;
        assert_eq!(count(&capped.output), 20, "{}", capped.output);
        let one = tool.execute(json!({"query": "handler", "limit": 1})).await;
        assert_eq!(count(&one.output), 1, "{}", one.output);
    }

    #[tokio::test]
    async fn a_missing_graph_explains_itself() {
        let tool = FindCodeTool::new(None);
        let result = tool.execute(json!({"query": "anything", "limit": 99})).await;
        // No graph: the tool explains itself instead of panicking.
        assert!(!result.success);
        assert!(result.output.contains("code graph"), "{}", result.output);
    }

    /// A generation bump must make the NEXT query reload the snapshot: each
    /// writer path renames or adds symbols and the tool must then read the new
    /// state, so deleting `bump_symbol_search` (or a call site) turns this red.
    #[tokio::test]
    async fn every_symbol_writer_makes_the_next_query_reload() {
        let (dir, mut graph) = graph_with(&[("src/lib.rs", "pub fn alpha_one() -> u32 { 1 }\n")]);
        graph.arm_symbol_embedder_stub(FixedEmbedder::new(vec![1.0, 0.0, 0.0, 0.0]));
        let alpha = symbol_id(&graph, "alpha_one");
        graph
            .store_symbol_vector_for_test(CODE_EMBEDDING_MODEL, &alpha, &[1.0, 0.0, 0.0, 0.0])
            .unwrap();
        let graph = Arc::new(graph);
        let tool = FindCodeTool::new(Some(Arc::clone(&graph)));
        let store_vector = |name: &str| {
            let id = symbol_id(&graph, name);
            graph
                .store_symbol_vector_for_test(CODE_EMBEDDING_MODEL, &id, &[1.0, 0.0, 0.0, 0.0])
                .unwrap();
        };
        let first = tool.execute(json!({"query": "alpha_one"})).await;
        assert!(first.output.contains("alpha_one"), "{}", first.output);

        // reindex_stale_files: the function is renamed on disk.
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn beta_two() -> u32 { 2 }\n").unwrap();
        graph
            .reindex_stale_files(
                &["src/lib.rs".to_string()],
                std::time::Duration::from_secs(5),
            )
            .unwrap();
        store_vector("beta_two");
        let second = tool.execute(json!({"query": "beta_two"})).await;
        assert!(
            second.output.contains("beta_two"),
            "a re-parsed file must invalidate the snapshot: {}",
            second.output
        );
        assert!(
            !second.output.contains("alpha_one"),
            "the vanished symbol must not be served: {}",
            second.output
        );

        // reindex_files_containing: a new file the containment pass adopts.
        std::fs::write(
            dir.path().join("src/gamma.rs"),
            "pub fn gamma_three() -> u32 { 3 }\n",
        )
        .unwrap();
        graph.reindex_files_containing("gamma_three").unwrap();
        store_vector("gamma_three");
        let third = tool.execute(json!({"query": "gamma_three"})).await;
        assert!(
            third.output.contains("gamma_three"),
            "the containment pass must invalidate the snapshot: {}",
            third.output
        );

        // index: the full walk adopts another new file.
        std::fs::write(
            dir.path().join("src/delta.rs"),
            "pub fn delta_four() -> u32 { 4 }\n",
        )
        .unwrap();
        graph.index(None).unwrap();
        store_vector("delta_four");
        let fourth = tool.execute(json!({"query": "delta_four"})).await;
        assert!(
            fourth.output.contains("delta_four"),
            "an index pass must invalidate the snapshot: {}",
            fourth.output
        );
    }
}

#[cfg(all(test, feature = "embeddings"))]
mod eval {
    use super::*;
    use crate::memory::embedder::{is_model_installed, EmbedderStatus, CODE_EMBEDDING_MODEL};

    /// One eval question — `tests/eval/find_code_questions.json`.
    #[derive(serde::Deserialize)]
    struct EvalQuestion {
        question: String,
        symbol: String,
        file: String,
    }

    /// Questions about THIS repository, each naming a real symbol of a real
    /// file (checked against the indexed graph before the run), phrased the
    /// way someone who does NOT know the name would ask.
    const QUESTIONS: &str = include_str!("../../../tests/eval/find_code_questions.json");

    /// The `(file, name)` a `find_code` output line names, when it is a
    /// result line (the indented doc lines are not).
    fn parse_output_line(line: &str) -> Option<(String, String)> {
        if line.starts_with(' ') {
            return None;
        }
        let (location, rest) = line.split_once(' ')?;
        let (file, _range) = location.rsplit_once(':')?;
        let mut parts = rest.splitn(3, ' ');
        let _kind = parts.next()?;
        let name = parts.next()?;
        Some((file.to_string(), name.to_string()))
    }

    /// The manual eval (plan d3336a8a step 11): symbol and file recall@5 for
    /// `find_code` and for the `search` tool on the same 25 questions, plus
    /// the query latency. `#[ignore]`d — it indexes this repository and
    /// embeds every symbol through the real code model, which takes minutes.
    ///
    /// Run it with
    /// `cargo test --lib --features embeddings -- --ignored --nocapture find_code_eval`.
    /// The graph lives in a temp dir; the model cache is READ only, and a
    /// missing model SKIPS the run instead of downloading.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "eval: indexes this repo and embeds every symbol with the code model; run manually"]
    async fn find_code_eval() {
        let questions: Vec<EvalQuestion> =
            serde_json::from_str(QUESTIONS).expect("eval questions parse");
        assert!(
            questions.len() >= 25,
            "the eval set carries at least 25 questions, got {}",
            questions.len()
        );

        let cache_dir = crate::config::embedder_cache_dir(&crate::config::resolve_config_dir(None));
        if !is_model_installed(&cache_dir, CODE_EMBEDDING_MODEL) {
            println!(
                "find_code eval SKIPPED: the {CODE_EMBEDDING_MODEL} model is not cached in {}",
                cache_dir.display()
            );
            return;
        }

        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let temp = tempfile::tempdir().unwrap();
        let mut graph = CodeGraph::open(root, &temp.path().join("codegraph.db")).unwrap();
        let stats = graph.index(None).unwrap();
        println!(
            "indexed {} files / {} symbols from {}",
            stats.files_scanned,
            stats.symbols,
            env!("CARGO_MANIFEST_DIR")
        );

        let embedder = crate::provider::client_factory::build_code_embedder(&cache_dir, None);
        if embedder.model_id() != CODE_EMBEDDING_MODEL
            || matches!(embedder.status(), Some(EmbedderStatus::Failed))
        {
            println!(
                "find_code eval SKIPPED: no usable {CODE_EMBEDDING_MODEL} embedder ({:?})",
                embedder.status()
            );
            return;
        }
        // The same model, through the pass's direct seam — the eval measures
        // the ranking, not the shared slot's attach dance.
        graph.arm_symbol_embedder_dyn(Arc::clone(&embedder));
        graph.index(None).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25 * 60);
        loop {
            let pending = graph.symbol_embed_pending(CODE_EMBEDDING_MODEL).unwrap();
            if pending == 0 {
                break;
            }
            if std::time::Instant::now() > deadline {
                println!("find_code eval SKIPPED: the embed pass did not drain ({pending} pending)");
                return;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        let (vectors, texts) = graph.symbol_search_counts().unwrap();
        println!("embedded {vectors} of {texts} symbols");

        let graph = Arc::new(graph);
        let tool = FindCodeTool::new(Some(Arc::clone(&graph)));
        let index = graph.symbol_search(CODE_EMBEDDING_MODEL).unwrap();
        for question in &questions {
            assert!(
                index
                    .symbols()
                    .iter()
                    .any(|symbol| symbol.file == question.file && symbol.name == question.symbol),
                "eval question names a symbol that does not exist: {} {}::{}",
                question.question,
                question.file,
                question.symbol
            );
        }

        let (mut code_symbol, mut code_file) = (0usize, 0usize);
        let (mut text_symbol, mut text_file) = (0usize, 0usize);
        let mut cold_ms = 0.0f64;
        let mut warm_ms = 0.0f64;
        for (i, question) in questions.iter().enumerate() {
            let started = std::time::Instant::now();
            let result = tool
                .execute(json!({"query": question.question, "limit": 5}))
                .await;
            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
            if i == 0 {
                cold_ms = elapsed_ms;
            } else {
                warm_ms += elapsed_ms;
            }
            let found: Vec<(String, String)> = result
                .output
                .lines()
                .filter_map(parse_output_line)
                .collect();
            let symbol_ok = found
                .iter()
                .any(|(file, name)| file == &question.file && name == &question.symbol);
            let file_ok = found
                .iter()
                .take(5)
                .any(|(file, _)| file == &question.file);
            code_symbol += symbol_ok as usize;
            code_file += file_ok as usize;

            // The baseline the step compares against: the `search` tool's
            // literal phrase query, each hit mapped to its innermost
            // enclosing symbol, deduped in order.
            let hits = graph.search_content(&question.question, 25).unwrap();
            let mut seen: Vec<String> = Vec::new();
            let mut text_found: Vec<(String, String)> = Vec::new();
            for hit in &hits {
                let Some(symbol) = index.enclosing(&hit.path, hit.line) else {
                    continue;
                };
                if seen.contains(&symbol.id) {
                    continue;
                }
                seen.push(symbol.id.clone());
                text_found.push((symbol.file.clone(), symbol.name.clone()));
                if text_found.len() == 5 {
                    break;
                }
            }
            let text_symbol_ok = text_found
                .iter()
                .any(|(file, name)| file == &question.file && name == &question.symbol);
            let text_file_ok = text_found
                .iter()
                .take(5)
                .any(|(file, _)| file == &question.file);
            text_symbol += text_symbol_ok as usize;
            text_file += text_file_ok as usize;
            println!(
                "{:2}. find_code {} / search {} — {}",
                i + 1,
                if symbol_ok { "HIT " } else { "miss" },
                if text_symbol_ok { "HIT " } else { "miss" },
                question.question
            );
        }
        let n = questions.len();
        println!(
            "find_code symbol recall@5: {code_symbol}/{n} ({:.0}%)",
            100.0 * code_symbol as f64 / n as f64
        );
        println!("find_code file   recall@5: {code_file}/{n}");
        println!("search    symbol recall@5: {text_symbol}/{n}");
        println!("search    file   recall@5: {text_file}/{n}");
        println!(
            "query latency: cold {cold_ms:.1} ms, warm {:.1} ms average",
            warm_ms / (n.saturating_sub(1)).max(1) as f64
        );
    }

    /// The cosine scan's cost at this repo's scale (plan d3336a8a step 11):
    /// ~16k stored vectors × 768 dims is the dominant term of a `find_code`
    /// query (one FTS SELECT and one name scan are orders cheaper). Times the
    /// REAL [`cosine_term`], so the number is the shipped arithmetic. Run
    /// `cargo test --lib --features embeddings -- --ignored --nocapture measure_cosine_scan`
    /// for the debug number and add `--release` for the release one.
    #[test]
    #[ignore = "measurement: the cosine scan over 16k x 768 vectors"]
    fn measure_cosine_scan() {
        const COUNT: usize = 16_384;
        const DIMS: usize = 768;
        let vectors: Vec<Vec<f32>> = (0..COUNT)
            .map(|i| {
                let mut vector: Vec<f32> = (0..DIMS)
                    .map(|j| (((i * 31 + j * 17) % 997) as f32 / 997.0) - 0.5)
                    .collect();
                let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
                for x in &mut vector {
                    *x /= norm;
                }
                vector
            })
            .collect();
        let query: Vec<f32> = (0..DIMS)
            .map(|j| ((j % 37) as f32 / 37.0) - 0.5)
            .collect();
        let query_norm = query.iter().map(|x| x * x).sum::<f32>().sqrt();
        let started = std::time::Instant::now();
        let mut best = 0.0f32;
        for vector in &vectors {
            best = best.max(cosine_term(vector, &query, query_norm));
        }
        println!(
            "cosine scan over {COUNT} x {DIMS}: {:?} (best {best:.4})",
            started.elapsed()
        );
    }
}

#[cfg(all(test, feature = "embeddings"))]
mod degraded_embedder_tests {
    use super::*;
    use crate::memory::embedder::{Embedder, EmbedderStatus, CODE_EMBEDDING_MODEL};

    /// A degraded stub: `Failed` status with the remote embedder's
    /// hash-fallback shape (non-zero vectors that are NOT the model's).
    struct FailingEmbedder {
        model: String,
    }

    #[async_trait]
    impl Embedder for FailingEmbedder {
        async fn embed(&self, _text: &str) -> Vec<f32> {
            vec![0.5, 0.5, 0.5, 0.5]
        }

        fn model_id(&self) -> &str {
            &self.model
        }

        fn dim(&self) -> usize {
            4
        }

        fn status(&self) -> Option<EmbedderStatus> {
            Some(EmbedderStatus::Failed)
        }
    }

    /// A one-file graph with the degraded embedder armed and one real vector
    /// stored — the ranked path WOULD apply if the status check let it.
    fn degraded_graph() -> Arc<CodeGraph> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "/// Adds one to the counter.\npub fn bump_counter(v: u32) -> u32 { v + 1 }\n",
        )
        .unwrap();
        let mut graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        graph.arm_symbol_embedder_stub(Arc::new(FailingEmbedder {
            model: CODE_EMBEDDING_MODEL.to_string(),
        }));
        let bump = graph
            .view()
            .unwrap()
            .resolve("bump_counter")
            .first()
            .map(|symbol| symbol.id.clone())
            .expect("fixture symbol");
        graph
            .store_symbol_vector_for_test(CODE_EMBEDDING_MODEL, &bump, &[1.0, 0.0, 0.0, 0.0])
            .unwrap();
        Arc::new(graph)
    }

    #[tokio::test]
    async fn a_degraded_embedder_falls_back_instead_of_ranking_junk() {
        let tool = FindCodeTool::new(Some(degraded_graph()));
        let result = tool
            .execute(json!({"query": "adds one to the counter"}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            !result.output.contains("function bump_counter"),
            "a Failed embedder must not rank: {}",
            result.output
        );
        assert!(
            result.output.starts_with("code embeddings are still being built")
                || result.output.starts_with("the code embedder is not ready"),
            "the fallback note leads: {}",
            result.output
        );
    }
}
