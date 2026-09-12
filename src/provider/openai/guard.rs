// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The client-side stream guard for the OpenAI-compatible client.
//!
//! [`apply_stream_guard`] scans one batch of parsed events for the configured
//! stop boundaries and truncates at the first hit — the defense-in-depth
//! backstop for backends that leak stop tokens as text (config-driven via
//! `stop` + `stop_boundary_strings`).

use crate::provider::LlmEvent;
/// Find the earliest occurrence of any stop sequence in `text`.
/// Returns `Some((start_byte_idx, match_len))` or `None`.
pub fn find_boundary_cutoff(text: &str, stop_sequences: &[String]) -> Option<(usize, usize)> {
    let mut earliest: Option<(usize, usize)> = None;
    for seq in stop_sequences {
        if seq.is_empty() {
            continue;
        }
        if let Some(idx) = text.find(seq) {
            match earliest {
                None => earliest = Some((idx, seq.len())),
                Some((cur_idx, _)) if idx < cur_idx => earliest = Some((idx, seq.len())),
                _ => {}
            }
        }
    }
    earliest
}

/// Apply the client-side stream guard to one batch of parsed events:
/// TextDeltas are scanned for stop boundaries; on the first match the
/// triggering delta is truncated to its prefix, all later events in the
/// batch are dropped, and the hit is reported as
/// [`crate::provider::trace::GuardCutDetails`] (the boundary-cut
/// observability tap — see [`LlmRequestLog::set_guard_cut`]). Non-text
/// events pass through untouched. Pure, so it is unit-testable without
/// a live stream. `chars_before` is the answer text streamed before this
/// batch (recorded on the hit for post-hoc diagnosis).
pub fn apply_stream_guard(
    events: Vec<LlmEvent>,
    stop_sequences: &[String],
    chars_before: usize,
) -> (
    Vec<LlmEvent>,
    Option<crate::provider::trace::GuardCutDetails>,
) {
    let mut guarded = Vec::with_capacity(events.len());
    let mut hit: Option<crate::provider::trace::GuardCutDetails> = None;
    for ev in events {
        if hit.is_some() {
            // First cut wins — the rest of the batch is dropped.
            break;
        }
        match ev {
            LlmEvent::TextDelta { text } => {
                if let Some((idx, _)) = find_boundary_cutoff(&text, stop_sequences) {
                    let prefix_empty = idx == 0;
                    if !prefix_empty {
                        guarded.push(LlmEvent::TextDelta {
                            text: text[..idx].to_string(),
                        });
                    }
                    // Re-find the matched sequence (find_boundary_cutoff
                    // reports position + length only).
                    let boundary = stop_sequences
                        .iter()
                        .find(|s| !s.is_empty() && text[idx..].starts_with(s.as_str()))
                        .cloned()
                        .unwrap_or_default();
                    hit = Some(crate::provider::trace::GuardCutDetails {
                        boundary,
                        byte_idx: idx,
                        delta_len: text.len(),
                        chars_before,
                        prefix_empty,
                    });
                } else {
                    guarded.push(LlmEvent::TextDelta { text });
                }
            }
            other => guarded.push(other),
        }
    }
    (guarded, hit)
}
