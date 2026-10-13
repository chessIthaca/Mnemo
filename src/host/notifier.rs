// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The seam between the brain and whatever hosts it (the Tauri GUI, the
//! console, the headless API). The brain reports background startup work
//! here; the host decides what to do with it (the GUI forwards each variant
//! to the webview channel it already listens on).

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::host::index_progress::IndexProgressEvent;
use crate::host::maintenance::ReconcileEvent;

/// A background event the brain reports to its host. Each variant maps 1:1 to
/// a webview channel the Tauri app already uses: `Reconcile` →
/// `memory://reconcile`, `IndexProgress` → `codegraph://index-progress`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "channel", content = "event", rename_all = "snake_case")]
pub enum HostEvent {
    /// The startup derived-index reconcile (memory maintenance).
    Reconcile(ReconcileEvent),
    /// The startup code-graph index pass.
    IndexProgress(IndexProgressEvent),
    /// The embedder status transitions (`embedder://status`): the
    /// current status at engine start + each deferred-load change
    /// (3b.3, §5.1/§5.6 row 17). §5.1's sketch also named
    /// `CodegraphProgress`/`CodegraphReady` — job 1 landed that stream
    /// as [`HostEvent::IndexProgress`] (the 1:1 `codegraph://index-progress`
    /// mapping) and the startup pass (wire_brain's spawn) already emits
    /// it on this bus; the sketch's names predate the landing and are
    /// served by it.
    EmbedderStatus(crate::memory::embedder::EmbedderStatus),
}

/// Receives [`HostEvent`]s from the brain. Implemented by each frontend.
pub trait HostNotifier: Send + Sync + 'static {
    /// Deliver one event. Must not block.
    fn notify(&self, event: HostEvent);
}

/// The shared handle the brain holds.
pub type SharedNotifier = Arc<dyn HostNotifier>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_event_serde_roundtrip() {
        let events = vec![
            HostEvent::Reconcile(ReconcileEvent::Started),
            HostEvent::Reconcile(ReconcileEvent::Progress { done: 1, total: 3 }),
            HostEvent::Reconcile(ReconcileEvent::Done {
                summary: "ok".to_string(),
            }),
            HostEvent::Reconcile(ReconcileEvent::Failed {
                error: "boom".to_string(),
            }),
            HostEvent::IndexProgress(IndexProgressEvent::Started),
            HostEvent::IndexProgress(IndexProgressEvent::Progress { done: 2, total: 5 }),
            HostEvent::IndexProgress(IndexProgressEvent::Done {
                summary: "ok".to_string(),
            }),
            HostEvent::IndexProgress(IndexProgressEvent::Failed {
                error: "boom".to_string(),
            }),
            HostEvent::EmbedderStatus(crate::memory::embedder::EmbedderStatus::Ready),
        ];
        for ev in events {
            let json = serde_json::to_string(&ev).expect("serialize");
            let back: HostEvent = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(
                serde_json::to_string(&back).expect("re-serialize"),
                json,
                "round-trip must be byte-stable"
            );
        }
    }
}
