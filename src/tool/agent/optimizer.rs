// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Shared helpers for the token-optimizer levers (backlog e4a50d22).
//!
//! Tools that serve a reduced form (delta/skeleton re-reads, compressed
//! command output, archive previews, expansions) report their before/after
//! token counts as `savings` events in the tool result's `data`; the agent
//! loop's dispatch layer (`turn.rs::execute_tool_batch`) records each one
//! as a `savings_events` ledger row, fire-and-forget.

/// Rough token estimate for a text string: one token per ~4 characters.
///
/// A documented estimate, not a metered count — every savings event built
/// from it carries `measured = false`, so the dashboard can show metered
/// vs estimated savings apart (backlog 652ae094). Good enough for the
/// ledger's purpose (order-of-magnitude before/after ratios); never used
/// to gate behavior.
pub(crate) fn estimate_tokens(text: &str) -> i64 {
    (text.chars().count() as i64) / 4
}

/// Build one `data.savings` event object for a tool that served a reduced
/// form. `kind` is the ledger kind (`delta_read`, `skeleton`,
/// `compression`, `archive`, `archive_expand`), `detail` the file path or
/// command, and the token counts the estimates of what WOULD have entered
/// the context vs. what actually did.
pub(crate) fn savings_event(
    kind: &str,
    detail: &str,
    tokens_before: i64,
    tokens_after: i64,
) -> serde_json::Value {
    serde_json::json!({
        "kind": kind,
        "detail": detail,
        "tokens_before": tokens_before,
        "tokens_after": tokens_after,
        // chars/4 estimate — see [`estimate_tokens`].
        "measured": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_tokens_is_chars_over_four() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcdeabcd"), 2);
    }

    #[test]
    fn savings_event_carries_kind_detail_and_counts() {
        let ev = savings_event("skeleton", "src/a.rs", 100, 10);
        assert_eq!(ev["kind"], "skeleton");
        assert_eq!(ev["detail"], "src/a.rs");
        assert_eq!(ev["tokens_before"], 100);
        assert_eq!(ev["tokens_after"], 10);
        assert_eq!(ev["measured"], false);
    }
}