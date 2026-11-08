// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! Host-side batching of streamed deltas before they cross into the window.
//!
//! The lead and every worker session stream one `TextDelta` /
//! `ReasoningDelta` / `ToolCallArgDelta` per model chunk; emitted raw, each
//! chunk pays a JSON serialization plus a webview IPC crossing. The batcher
//! merges ADJACENT same-kind deltas of one stream into a single pending
//! event and flushes it on the next event of that stream, a 16 ms deadline
//! from the first pending delta, a 64 KiB per-delta cap, or channel close.
//! Order inside a stream stays exact — only adjacent same-kind deltas merge
//! (tool-call arguments additionally require the same call index), and any
//! other event of the stream flushes the pending delta ahead of itself.
//! Streams (the lead generation + agent, or worker + generation + agent)
//! never interact; `ToolOutputDelta` never merges (the tool already
//! throttles its own live output).

use mnemo::headless::SessionEvent;
use mnemo::{AgentId, SerializableAgentEvent};
use tokio::sync::broadcast::{self, error::RecvError};

/// The flush deadline: fixed 16 ms from the FIRST pending delta, never
/// extended while further deltas of the batch keep arriving.
pub(crate) const DELTA_FLUSH: std::time::Duration = std::time::Duration::from_millis(16);

/// The per-pending-delta byte cap: a merged delta that reaches this size is
/// emitted immediately instead of pending further growth.
pub(crate) const DELTA_CAP_BYTES: usize = 64 * 1024;

/// An event envelope that carries one session event and belongs to one
/// stream.
///
/// The stream key identifies the stream whose order the batcher preserves:
/// the live session generation plus emitting agent for the lead, or the
/// worker id plus generation plus agent for a team.
pub(crate) trait Batchable: Sized {
    /// What identifies the stream this event belongs to.
    type Key: Eq + Clone;

    /// The stream key of this event.
    fn stream_key(&self) -> Self::Key;
    /// The carried session event.
    fn session_event(&self) -> &SessionEvent;
    /// The carried session event, mutably — the batcher merges text into
    /// it.
    fn session_event_mut(&mut self) -> &mut SessionEvent;
}

/// The lead's events: one stream per live session generation + agent.
impl Batchable for crate::mnemo2::runtime::LeadEvent {
    type Key = (u64, AgentId);

    fn stream_key(&self) -> Self::Key {
        (self.generation, self.event.agent)
    }

    fn session_event(&self) -> &SessionEvent {
        &self.event
    }

    fn session_event_mut(&mut self) -> &mut SessionEvent {
        &mut self.event
    }
}

/// A worker's events: one stream per worker, generation + agent.
impl Batchable for crate::mnemo2::team::TeamEvent {
    type Key = (String, u64, AgentId);

    fn stream_key(&self) -> Self::Key {
        (self.worker_id.clone(), self.generation, self.event.agent)
    }

    fn session_event(&self) -> &SessionEvent {
        &self.event
    }

    fn session_event_mut(&mut self) -> &mut SessionEvent {
        &mut self.event
    }
}

/// Which mergeable delta stream an event belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DeltaKind {
    /// Assistant answer text.
    Text,
    /// Reasoning text.
    Reasoning,
    /// A tool call's argument fragments, keyed by the call index.
    ToolCallArg(u32),
}

/// The delta stream an event belongs to, or `None` for every other event —
/// including `ToolOutputDelta`, which passes through unmerged (the tool
/// already throttles its own stream).
fn delta_kind(event: &SerializableAgentEvent) -> Option<DeltaKind> {
    match event {
        SerializableAgentEvent::TextDelta { .. } => Some(DeltaKind::Text),
        SerializableAgentEvent::ReasoningDelta { .. } => Some(DeltaKind::Reasoning),
        SerializableAgentEvent::ToolCallArgDelta { index, .. } => {
            Some(DeltaKind::ToolCallArg(*index))
        }
        _ => None,
    }
}

/// The mergeable text of the delta kinds — `text` / `fragment` — mutably:
/// the batcher appends into it.
fn delta_text_mut(event: &mut SerializableAgentEvent) -> Option<&mut String> {
    match event {
        SerializableAgentEvent::TextDelta { text } | SerializableAgentEvent::ReasoningDelta { text } => {
            Some(text)
        }
        SerializableAgentEvent::ToolCallArgDelta { fragment, .. } => Some(fragment),
        _ => None,
    }
}

/// The byte length of a mergeable delta's text, or `None` for any other
/// event.
fn delta_text_len(event: &SerializableAgentEvent) -> Option<usize> {
    match event {
        SerializableAgentEvent::TextDelta { text } | SerializableAgentEvent::ReasoningDelta { text } => {
            Some(text.len())
        }
        SerializableAgentEvent::ToolCallArgDelta { fragment, .. } => Some(fragment.len()),
        _ => None,
    }
}

/// Merges adjacent same-kind deltas of one stream before they are emitted.
///
/// At most one event pends per stream key, in insertion order, so every
/// flush emits each stream's deltas in exact stream order.
pub(crate) struct DeltaBatcher<E: Batchable> {
    /// Pending events, at most one per stream key, in insertion order.
    pending: Vec<E>,
}

impl<E: Batchable> Default for DeltaBatcher<E> {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
        }
    }
}

impl<E: Batchable> DeltaBatcher<E> {
    /// Feed one event; returns the events to emit now, in order.
    ///
    /// A same-kind delta of a stream merges into that stream's pending
    /// delta while the merged text stays within [`DELTA_CAP_BYTES`] (a
    /// merged delta that reaches the cap is emitted immediately). Any other
    /// event of the stream first flushes the pending delta, then passes
    /// through — or, when it is itself a delta, becomes the stream's new
    /// pending one. Events of other streams never interact.
    pub(crate) fn push(&mut self, mut event: E) -> Vec<E> {
        let kind = delta_kind(&event.session_event().event);
        let position = self
            .pending
            .iter()
            .position(|pending| pending.stream_key() == event.stream_key());
        let Some(i) = position else {
            // First event of its stream. A delta pends (it can still merge
            // with the next chunk of the same kind); a single delta larger
            // than the cap can never merge, so it is emitted immediately
            // instead. Anything else passes straight through.
            if kind.is_some() {
                if delta_text_len(&event.session_event().event)
                    .is_some_and(|len| len > DELTA_CAP_BYTES)
                {
                    return vec![event];
                }
                self.pending.push(event);
                return Vec::new();
            }
            return vec![event];
        };
        let pending_kind = delta_kind(&self.pending[i].session_event().event);
        let pending_len = delta_text_len(&self.pending[i].session_event().event).unwrap_or(0);
        let new_len = delta_text_len(&event.session_event().event).unwrap_or(0);
        if kind.is_some() && kind == pending_kind && pending_len + new_len <= DELTA_CAP_BYTES {
            // Same-kind adjacent delta: append its text to the pending one.
            if let (Some(target), Some(incoming)) = (
                delta_text_mut(&mut self.pending[i].session_event_mut().event),
                delta_text_mut(&mut event.session_event_mut().event),
            ) {
                target.push_str(incoming);
            }
            if pending_len + new_len >= DELTA_CAP_BYTES {
                // The merged delta is full: emit it instead of pending it.
                return vec![self.pending.remove(i)];
            }
            return Vec::new();
        }
        // A different kind, or the merged text would exceed the cap: flush
        // the pending delta first; the new event then passes through, or
        // pends as the stream's next delta.
        let mut out = vec![self.pending.remove(i)];
        if kind.is_some() {
            self.pending.push(event);
        } else {
            out.push(event);
        }
        out
    }

    /// Every pending event, in insertion order (timer tick or channel
    /// close).
    pub(crate) fn flush_all(&mut self) -> Vec<E> {
        std::mem::take(&mut self.pending)
    }

    /// Whether any event is pending (drives the flush deadline).
    pub(crate) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

/// Receive from `events` until it closes, emitting through `emit` with
/// deltas batched.
///
/// The flush deadline is 16 ms from the FIRST pending delta and is never
/// extended while further deltas arrive; when it passes, everything
/// pending is emitted. On channel close every pending delta is emitted
/// before the loop ends. (`broadcast::Receiver::recv` is cancel-safe, so
/// racing it against the deadline timer loses nothing.)
pub(crate) async fn forward_batched<E, F>(
    mut events: broadcast::Receiver<E>,
    mut emit: F,
) where
    E: Batchable + Clone + Send,
    F: FnMut(E) + Send,
{
    let mut batcher = DeltaBatcher::<E>::default();
    // `None` while nothing is pending; otherwise the FIXED deadline armed
    // when the current batch's first delta arrived.
    let mut deadline: Option<tokio::time::Instant> = None;
    loop {
        let received = match deadline {
            Some(at) => tokio::select! {
                biased;
                r = events.recv() => Some(r),
                _ = tokio::time::sleep_until(at) => None,
            },
            None => Some(events.recv().await),
        };
        match received {
            None => {
                for event in batcher.flush_all() {
                    emit(event);
                }
                deadline = None;
            }
            Some(Ok(event)) => {
                for event in batcher.push(event) {
                    emit(event);
                }
                if batcher.is_empty() {
                    deadline = None;
                } else {
                    deadline.get_or_insert_with(|| tokio::time::Instant::now() + DELTA_FLUSH);
                }
            }
            Some(Err(RecvError::Lagged(_))) => continue,
            Some(Err(RecvError::Closed)) => {
                for event in batcher.flush_all() {
                    emit(event);
                }
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mnemo2::team::TeamEvent;
    use mnemo::provider::FinishReason;
    use mnemo::tool::ToolOutputStream;
    use std::sync::{Arc, Mutex};

    /// A team event for `worker` on stream agent 7, generation 1.
    fn team_event(worker: &str, event: SerializableAgentEvent) -> TeamEvent {
        TeamEvent {
            worker_id: worker.into(),
            generation: 1,
            event: SessionEvent {
                agent: 7,
                event,
                rates: None,
            },
        }
    }

    fn text_delta(text: &str) -> SerializableAgentEvent {
        SerializableAgentEvent::TextDelta { text: text.into() }
    }

    fn reasoning_delta(text: &str) -> SerializableAgentEvent {
        SerializableAgentEvent::ReasoningDelta { text: text.into() }
    }

    /// Three adjacent same-kind deltas merge into one pending event;
    /// `flush_all` returns it as a single merged delta.
    #[test]
    fn adjacent_text_deltas_merge_into_one() {
        let mut batcher = DeltaBatcher::<TeamEvent>::default();
        assert!(batcher.push(team_event("w1", text_delta("a"))).is_empty());
        assert!(batcher.push(team_event("w1", text_delta("b"))).is_empty());
        assert!(batcher.push(team_event("w1", text_delta("c"))).is_empty());
        assert!(!batcher.is_empty(), "the merged delta is still pending");
        let out = batcher.flush_all();
        assert_eq!(out.len(), 1);
        assert!(matches!(
            &out[0].event.event,
            SerializableAgentEvent::TextDelta { text } if text == "abc"
        ));
        assert!(batcher.is_empty());
    }

    /// A kind change flushes the pending delta ahead of the new event: the
    /// reasoning pair leaves as one merged delta, the text delta behind the
    /// tool-call start.
    #[test]
    fn a_kind_change_flushes_the_pending_delta_first() {
        let mut batcher = DeltaBatcher::<TeamEvent>::default();
        assert!(batcher.push(team_event("w1", reasoning_delta("r1"))).is_empty());
        assert!(batcher.push(team_event("w1", reasoning_delta("r2"))).is_empty());
        let out = batcher.push(team_event("w1", text_delta("t")));
        assert_eq!(out.len(), 1);
        assert!(matches!(
            &out[0].event.event,
            SerializableAgentEvent::ReasoningDelta { text } if text == "r1r2"
        ));
        let out = batcher.push(team_event(
            "w1",
            SerializableAgentEvent::ToolCallStart {
                index: 0,
                id: "c0".into(),
                name: "shell".into(),
            },
        ));
        assert_eq!(out.len(), 2);
        assert!(matches!(
            &out[0].event.event,
            SerializableAgentEvent::TextDelta { text } if text == "t"
        ));
        assert!(matches!(
            &out[1].event.event,
            SerializableAgentEvent::ToolCallStart { index: 0, .. }
        ));
        assert!(batcher.is_empty());
    }

    /// A new tool-call index flushes the previous index's arguments; the
    /// new fragment pends on its own.
    #[test]
    fn a_new_tool_call_index_flushes_the_previous_arguments() {
        let mut batcher = DeltaBatcher::<TeamEvent>::default();
        assert!(batcher
            .push(team_event(
                "w1",
                SerializableAgentEvent::ToolCallArgDelta {
                    index: 0,
                    fragment: "x".into(),
                },
            ))
            .is_empty());
        let out = batcher.push(team_event(
            "w1",
            SerializableAgentEvent::ToolCallArgDelta {
                index: 1,
                fragment: "y".into(),
            },
        ));
        assert_eq!(out.len(), 1, "the first push of index 1 flushes index 0");
        assert!(matches!(
            &out[0].event.event,
            SerializableAgentEvent::ToolCallArgDelta { index: 0, fragment } if fragment == "x"
        ));
        let out = batcher.flush_all();
        assert_eq!(out.len(), 1);
        assert!(matches!(
            &out[0].event.event,
            SerializableAgentEvent::ToolCallArgDelta { index: 1, fragment } if fragment == "y"
        ));
    }

    /// An event of one stream never flushes another stream's pending
    /// delta: w1's Finished returns w1's text plus itself, w2's delta is
    /// still pending.
    #[test]
    fn streams_stay_independent() {
        let mut batcher = DeltaBatcher::<TeamEvent>::default();
        assert!(batcher.push(team_event("w1", text_delta("a"))).is_empty());
        assert!(batcher.push(team_event("w2", text_delta("b"))).is_empty());
        let out = batcher.push(team_event(
            "w1",
            SerializableAgentEvent::Finished {
                reason: FinishReason::Stop,
            },
        ));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].worker_id, "w1");
        assert!(matches!(
            &out[0].event.event,
            SerializableAgentEvent::TextDelta { text } if text == "a"
        ));
        assert!(matches!(
            &out[1].event.event,
            SerializableAgentEvent::Finished { .. }
        ));
        // w2's delta is still pending, untouched by w1's structural event.
        let out = batcher.flush_all();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].worker_id, "w2");
        assert!(matches!(
            &out[0].event.event,
            SerializableAgentEvent::TextDelta { text } if text == "b"
        ));
    }

    /// The byte cap: 20 chunks of 4,000 bytes (80,000 total). Merging stops
    /// at 64,000 bytes (the 17th chunk would merge to 68,000, over the
    /// cap), so a push returns the pending 64,000-byte delta; every emitted
    /// delta stays within the cap, and every input byte is emitted exactly
    /// once across pushes and the final flush, in order.
    #[test]
    fn the_byte_cap_flushes_a_long_delta() {
        let chunks: Vec<String> = (0..20)
            .map(|i| format!("{i}{}", "x".repeat(4000 - i.to_string().len())))
            .collect();
        let expected: String = chunks.concat();
        assert_eq!(expected.len(), 80_000);

        let mut batcher = DeltaBatcher::<TeamEvent>::default();
        let mut emitted = String::new();
        let mut a_push_emitted = false;
        for chunk in &chunks {
            let out = batcher.push(team_event("w1", SerializableAgentEvent::TextDelta {
                text: chunk.clone(),
            }));
            if !out.is_empty() {
                a_push_emitted = true;
            }
            for event in out {
                let SerializableAgentEvent::TextDelta { text } = &event.event.event else {
                    panic!("every emitted event is a TextDelta");
                };
                assert!(
                    text.len() <= DELTA_CAP_BYTES,
                    "an emitted delta exceeds the cap: {}",
                    text.len()
                );
                emitted.push_str(text);
            }
        }
        assert!(
            a_push_emitted,
            "some push before the final flush returns a delta"
        );
        for event in batcher.flush_all() {
            let SerializableAgentEvent::TextDelta { text } = &event.event.event else {
                panic!("every flushed event is a TextDelta");
            };
            assert!(
                text.len() <= DELTA_CAP_BYTES,
                "an emitted delta exceeds the cap: {}",
                text.len()
            );
            emitted.push_str(text);
        }
        assert_eq!(emitted, expected, "every input byte, exactly once, in order");
        assert!(batcher.is_empty());
    }

    /// `ToolOutputDelta` never merges: it flushes the pending text delta
    /// and passes through itself, in order.
    #[test]
    fn tool_output_deltas_pass_through_in_order() {
        let mut batcher = DeltaBatcher::<TeamEvent>::default();
        assert!(batcher.push(team_event("w1", text_delta("t"))).is_empty());
        let out = batcher.push(team_event(
            "w1",
            SerializableAgentEvent::ToolOutputDelta {
                tool_call_id: "c0".into(),
                stream: ToolOutputStream::Stdout,
                text: "out".into(),
            },
        ));
        assert_eq!(out.len(), 2);
        assert!(matches!(
            &out[0].event.event,
            SerializableAgentEvent::TextDelta { text } if text == "t"
        ));
        assert!(matches!(
            &out[1].event.event,
            SerializableAgentEvent::ToolOutputDelta { tool_call_id, text, .. }
                if tool_call_id == "c0" && text == "out"
        ));
        assert!(batcher.is_empty());
    }

    /// The forwarder emits a stream's pending deltas once the fixed
    /// deadline passes, merged.
    #[tokio::test(start_paused = true)]
    async fn the_forwarder_flushes_after_the_deadline() {
        let (tx, rx) = broadcast::channel::<TeamEvent>(64);
        let emitted: Arc<Mutex<Vec<TeamEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = emitted.clone();
        let task = tokio::spawn(forward_batched(rx, move |event| {
            sink.lock().expect("emitted sink").push(event);
        }));
        // Park the forwarder in its plain receive before anything is sent.
        tokio::task::yield_now().await;
        let _ = tx.send(team_event("w1", text_delta("a")));
        let _ = tx.send(team_event("w1", text_delta("b")));
        // Let the forwarder consume both and arm its 16 ms deadline.
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(20)).await;
        tokio::task::yield_now().await;
        // End the forwarder before locking the sink: the deadline already
        // emitted the merged delta, the channel close flushes nothing
        // further, and the guard must never be held across an await.
        drop(tx);
        let _ = task.await;
        let emitted = emitted.lock().expect("emitted");
        assert_eq!(emitted.len(), 1, "exactly one merged event was emitted");
        assert!(matches!(
            &emitted[0].event.event,
            SerializableAgentEvent::TextDelta { text } if text == "ab"
        ));
    }

    /// The forwarder flushes everything pending when the channel closes.
    #[tokio::test]
    async fn the_forwarder_flushes_when_the_channel_closes() {
        let (tx, rx) = broadcast::channel::<TeamEvent>(64);
        let emitted: Arc<Mutex<Vec<TeamEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = emitted.clone();
        let task = tokio::spawn(forward_batched(rx, move |event| {
            sink.lock().expect("emitted sink").push(event);
        }));
        tokio::task::yield_now().await;
        let _ = tx.send(team_event("w1", text_delta("tail")));
        drop(tx);
        let _ = task.await; // the channel closes; the forwarder flushes and ends
        let emitted = emitted.lock().expect("emitted");
        assert_eq!(emitted.len(), 1, "the pending delta was flushed on close");
        assert!(matches!(
            &emitted[0].event.event,
            SerializableAgentEvent::TextDelta { text } if text == "tail"
        ));
    }
}
