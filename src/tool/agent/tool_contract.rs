// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The shared empty-call contract — one definition for every tool descriptor.
//!
//! Before this module a four-sentence empty-call block was hand-copied into
//! every tool with a required field (7 descriptors across 5 files, ~350 chars
//! each). That block ships inside the tool schema on EVERY request, while the
//! identical sentence in a tool's argument-error hint is read only when a call
//! actually fails.
//!
//! The evidence (2027-01 session, plan aff95a51): `read_files`' description LED
//! with the block and an empty call was still emitted — what recovered the
//! turn was the error-side hint on the next call. And the block's last
//! sentence (the content-first anti-pattern) is already stated once for EVERY
//! tool in the universal `TOOL_CALL_DISCIPLINE` prompt block
//! (`src/agent/prompt.rs`), which is what makes the per-tool copy redundant
//! rather than load-bearing.
//!
//! So the description keeps only what is decision-relevant per tool — the
//! required fields, an exact call shape, and the retry rule — while
//! [`recovery_hint`] keeps the retry-time clause verbatim on the error path.

/// The leading contract sentence for a tool description: what to pass, an
/// inline example of the exact call shape, and the no-zero-argument rule with
/// its retry rule.
///
/// `required` is the RENDERED required-fields phrase INCLUDING its backticks
/// (`"`op`"`, or `"`from` and `to`"` for a two-field tool) — rendered by the
/// caller so a multi-field tool still reads naturally. `example` is the JSON
/// call shape (e.g. `{"op":"log"}`).
///
/// Callers prepend this to their own substance, separated by a space.
pub fn contract(required: &str, example: &str) -> String {
    format!(
        "Always pass {required} — e.g. {example}. No zero-argument form; on a \
         required-field error rewrite the full call, do not resend the empty \
         shape."
    )
}

/// The retry-time recovery hint for a tool's argument-parse error: what to
/// pass and the rule for the retry.
///
/// This is the mechanism with the evidence behind it — the model reads it on
/// the failed call, so the FIRST retry succeeds instead of waiting for the
/// identical-failure circuit breaker (backlog d9ad618e, the read_files
/// precedent 26cdbaf8). It is passed to
/// [`super::read_files::invalid_args_error`].
///
/// `field` is a plain rendered phrase (e.g. `"command + purpose"`).
pub fn recovery_hint(field: &str) -> String {
    format!(
        "Always pass {field} — there is no zero-argument form; rewrite the \
         full call, do not resend the empty shape."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_leads_with_the_required_fields_and_their_shape() {
        let c = contract("`op`", "{\"op\":\"log\"}");
        assert!(
            c.starts_with("Always pass `op`"),
            "the contract sentence LEADS the description: {c}"
        );
        assert!(c.contains("e.g. {"), "the inline example shows the call shape: {c}");
        assert!(c.contains("{\"op\":\"log\"}"), "{c}");
        assert!(c.contains("No zero-argument form"), "{c}");
        assert!(c.contains("do not resend the empty shape"), "{c}");
    }

    #[test]
    fn contract_renders_a_multi_field_phrase_naturally() {
        let c = contract("`from` and `to`", "{\"from\":\"main\",\"to\":\"db\"}");
        assert!(c.starts_with("Always pass `from` and `to`"), "{c}");
    }

    #[test]
    fn contract_drops_the_clause_the_universal_prompt_already_states() {
        // TOOL_CALL_DISCIPLINE (src/agent/prompt.rs) states the content-first
        // anti-pattern once for every tool. Keeping a per-tool copy was the
        // trim's whole point of contention — pin the drop so it cannot
        // silently creep back into the 7 migrated descriptors.
        let c = contract("`files`", "{\"files\":[{\"path\":\"a.js\"}]}");
        assert!(
            !c.contains("If you catch yourself"),
            "the universal rule must not be re-copied per tool: {c}"
        );
    }

    #[test]
    fn contract_is_compact_so_it_cannot_silently_regrow() {
        // The pre-migration hand-copied block ran ~350 chars in the
        // description of every tool with a required field. This bound is the
        // trim's regression guard: an edit that re-inflates the contract
        // fails here.
        let c = contract("`id` (or `name`)", "{\"id\":\"src/a.rs::B::12\"}");
        assert!(
            c.chars().count() <= 190,
            "the contract must stay compact ({} chars): {c}",
            c.chars().count()
        );
    }

    #[test]
    fn recovery_hint_carries_the_retry_rule() {
        let h = recovery_hint("command + purpose");
        assert!(h.contains("Always pass command + purpose"), "{h}");
        assert!(h.contains("there is no zero-argument form"), "{h}");
        assert!(h.contains("rewrite the full call"), "{h}");
        assert!(h.contains("do not resend the empty shape"), "{h}");
    }

    #[test]
    fn every_migrated_description_leads_with_the_shared_contract() {
        // Plan aff95a51 drift guard: the descriptors that carried the
        // hand-copied empty-call block (across 5 files) were migrated to
        // `contract(...)`. A site that drifts back to a hand-copy (or
        // re-copies the content-first clause the universal
        // TOOL_CALL_DISCIPLINE already states) fails here — one test covers
        // every site. graph_impact and the memory hygiene tools never
        // carried the block (compact by design) and stay out.
        use std::sync::Arc;

        use crate::codegraph::CodeGraph;
        use crate::memory::embedder::HashEmbedder;
        use crate::memory::{Embedder, MemoryStore, MemoryStoreTrait};
        use crate::tool::agent::codegraph::{GraphContextTool, GraphPathTool, GraphSearchTool};
        use crate::tool::agent::expand_result::ExpandResultTool;
        use crate::tool::agent::git_read_tool::GitReadTool;
        use crate::tool::agent::read_files::ReadFilesTool;
        use crate::tool::agent::sandbox::Sandbox;
        use crate::tool::agent::shell::ShellTool;
        use crate::tool::memory::MemoryWriteTool;
        use crate::tool::Tool;

        let dir = tempfile::tempdir().unwrap();
        let graph = Arc::new(CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap());
        let read_sandbox = Sandbox::new(dir.path()).unwrap();
        let shell_sandbox = Sandbox::new(dir.path()).unwrap();
        let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::new());
        let store: Arc<dyn MemoryStoreTrait> =
            Arc::new(MemoryStore::open_in_memory(embedder).unwrap());

        let tools = vec![
            ("graph_search", GraphSearchTool::new(graph.clone()).schema()),
            ("graph_context", GraphContextTool::new(graph.clone()).schema()),
            ("graph_path", GraphPathTool::new(graph).schema()),
            ("read_files", ReadFilesTool::new(read_sandbox).schema()),
            ("shell", ShellTool::new(shell_sandbox).schema()),
            ("git_read", GitReadTool::new(dir.path()).schema()),
            ("expand_result", ExpandResultTool::new(Some(store.clone())).schema()),
            ("memory_write", MemoryWriteTool::new(store).schema()),
        ];
        assert_eq!(tools.len(), 8, "the migrated set is 8 descriptors");

        for (name, schema) in tools {
            assert!(
                schema.description.starts_with("Always pass "),
                "{name}: the description must LEAD with the shared contract: {}",
                schema.description
            );
            assert!(
                !schema.description.contains("If you catch yourself"),
                "{name}: the content-first clause lives once in TOOL_CALL_DISCIPLINE: {}",
                schema.description
            );
        }
    }
}
