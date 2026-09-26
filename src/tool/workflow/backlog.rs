// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `backlog_add` — append an item to the user's backlog (or, with
//! `position: "top"`, insert an urgent item at the FRONT of the queue —
//! dispatched first; the default appends).
//! `backlog_status` — change an item's status through the guarded transition
//! table (available in every workflow state).
//! `backlog_list` — read-only query of the backlog (available everywhere,
//! including read-only reviewers: querying the backlog is allowed, modifying
//! it is not).
//!
//! The backlog (`.coding/backlog.jsonl`, see [`crate::backlog`]) is the
//! user-owned list of prompts waiting to be dispatched to the main agent.
//! It is PROTECTED harness state — the file tools refuse to edit it — so
//! these tools are the agent's sanctioned write paths: when the user asks
//! for a follow-up ("add X to the backlog"), the agent calls `backlog_add`
//! instead of touching the file; when an item's status must change (an
//! explicit dead end, or a requeue), it calls `backlog_status`.
//!
//! Item shape (backlog 45a4eb88): every NEW item is headline + body — the
//! first line a short headline (≤ 100 chars), then the body carrying the
//! detail. The Backlog tab renders that split (backlog 40763a24), so
//! `backlog_add` validates it via [`crate::backlog::normalize_item_text`]
//! (the same shape contract the IPC add command enforces through the
//! image-aware [`crate::backlog::normalize_new_item_text`] — image-only
//! items are exempt there) and rejects shape-less text with an error
//! naming the required shape. Existing items are grandfathered.
//!
//! Plan-tied status contract (backlog 45dcf577): an item's status is derived
//! from the plan dispatched for it — `in_flight` ⇔ a plan is active, `done`
//! ⇔ the plan reached `Complete`, `failed` ⇔ the plan was abandoned. The
//! harness drives those transitions from plan lifecycle events; the agent's
//! manual uses are the explicit dead end (`cant_resolve`), pre-dispatch
//! resolutions, and requeues — never `failed` for merely unfinished work
//! (that stays `in_flight`; a resumed session continues the plan).
//!
//! The tools share the app's ONE [`BacklogStore`] handle (an
//! `Arc<tokio::sync::Mutex<..>>` — the same instance the Tauri `backlog_*`
//! IPC commands lock), so agent changes and UI mutations serialize through
//! one mutex and can never clobber each other. The store persists on every
//! mutation. An optional `on_changed` notifier (injected by the app layer —
//! the library crate has no Tauri dependency) fires after every successful
//! mutation so the UI can refresh the Backlog tab live; when no notifier is
//! wired (console mode, tests) the on-disk state is still always correct and
//! the tab picks the item up on its next refresh.
//!
//! `AutoRun` + [`ToolCategory::Workflow`]: adding a pending item is a
//! benign, easily-deletable note (not a code mutation), and it is most
//! useful in Planning — the same gating as `create_plan`. It is registered
//! when the factory has the shared store wired (GUI mode); in console mode
//! (`-console`) no store is opened, so the tool is omitted there too —
//! headless agents have no Backlog tab.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::backlog::{
    BacklogPosition, BacklogStatus, BacklogStore, MAX_HEADLINE_CHARS, MAX_ITEM_TEXT_CHARS,
    MIN_BODY_CHARS, normalize_item_text,
};
use crate::provider::ToolSchema;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// How many characters of the added text to echo back in the tool output.
const OUTPUT_PREVIEW_CHARS: usize = 120;

/// Arguments for `backlog_add`.
#[derive(Debug, Deserialize)]
struct BacklogAddArgs {
    /// The backlog item text — a self-contained task for a future session.
    text: String,
    /// Where the item lands in the queue — `Top` inserts at the front
    /// (dispatched first; urgent items only), `End` (the default, also
    /// for an explicit null) appends.
    #[serde(default)]
    position: Option<BacklogPosition>,
}

/// The `backlog_add` workflow tool — append a Pending item to the backlog.
///
/// Holds a clone of the shared store handle; see the module docs for why
/// this (not file access) is the only sanctioned write path. `on_changed`
/// is an optional notifier the app layer injects to emit the
/// `backlog://changed` Tauri event after each add (the library crate has no
/// Tauri dependency, so the callback is the seam); `None` in console mode
/// and tests.
pub struct BacklogAddTool {
    store: Arc<tokio::sync::Mutex<BacklogStore>>,
    on_changed: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl BacklogAddTool {
    /// Build the tool with the shared store handle and an optional
    /// post-add notifier (see the struct docs).
    pub fn new(
        store: Arc<tokio::sync::Mutex<BacklogStore>>,
        on_changed: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        Self { store, on_changed }
    }
}

#[async_trait]
impl Tool for BacklogAddTool {
    fn name(&self) -> &str {
        "backlog_add"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "backlog_add",
            "Add an item to the user's backlog — the persistent list of tasks/prompts \
             dispatched to future agent sessions. The ONLY sanctioned way to add one: \
             .coding/backlog.jsonl is protected state the file tools refuse to edit. Use it \
             when the user asks to queue a task for later ('add this to the backlog'). The \
             item lands as pending and shows in the Backlog tab immediately. Only add items \
             the user actually asked to queue — pending items are auto-dispatched. Optional \
             position: 'top' inserts the item at the FRONT of the queue (it dispatches \
             first) — reserve it for urgent/priority items; the default 'end' appends.",
            json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        // Built from the enforcing consts (review LOW-2)
                        // so the model's source of truth can never drift
                        // from what normalize_item_text enforces.
                        "description": format!(
                            "The backlog item text — REQUIRED SHAPE: first line a short \
                             headline (a few words, no trailing period), then a blank line, \
                             then the body. The body must be self-sufficient for a \
                             lesser-reasoning model dispatched on it later: (1) the \
                             problem/symptom with dates + how to reproduce, (2) exact file \
                             paths and symbol names, (3) the fix direction, (4) acceptance \
                             criteria / how to verify, (5) pointers to related memories, \
                             commits, reviews. Max {} chars total; single-line texts, \
                             headlines over {} chars, and bodies under {} chars or naming \
                             no file path are rejected (an explicit 'no-code research' \
                             marker stands in for the path on research items).",
                            MAX_ITEM_TEXT_CHARS,
                            MAX_HEADLINE_CHARS,
                            MIN_BODY_CHARS
                        )
                    },
                    "position": {
                        "type": "string",
                        "enum": ["top", "end"],
                        "default": "end",
                        "description": "Where the item lands in the queue. 'end' (the \
                         default, and when omitted) appends — the queue is otherwise \
                         FIFO. 'top' inserts at the FRONT so the item is dispatched \
                         first; use it ONLY for urgent/priority items the user needs \
                         handled before the rest (e.g. an incident fix) — otherwise \
                         leave it out."
                    }
                },
                "required": ["text"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: BacklogAddArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        // Normalize + validate once through the shared shape contract
        // (empty / length / headline+body — the same validator the IPC add
        // command enforces); store the trimmed value (mixed trim/no-trim
        // semantics misread easily — review finding 4).
        let text = match normalize_item_text(&args.text) {
            Ok(text) => text,
            Err(e) => return ToolResult::error(e),
        };
        // Absent (or explicit null) position = append — byte-identical to
        // the pre-position behavior every existing caller relies on.
        let position = args.position.unwrap_or(BacklogPosition::End);
        // Same lock the IPC backlog commands take — one store, one mutex.
        let item = self.store.lock().await.add_at(position, text, Vec::new());
        // Notify the UI (when wired) so the Backlog tab updates live instead
        // of waiting for a refresh/restart. The store lock is dropped by the
        // time this runs (the add statement completed), so the notifier can
        // re-read the store without deadlocking.
        if let Some(f) = &self.on_changed {
            f();
        }
        let preview: String = match item.text.chars().nth(OUTPUT_PREVIEW_CHARS) {
            Some(_) => format!(
                "{}…",
                item.text
                    .chars()
                    .take(OUTPUT_PREVIEW_CHARS)
                    .collect::<String>()
            ),
            None => item.text.clone(),
        };
        // End keeps the pre-position output byte-identical; Top names
        // itself so the dispatch order is visible in the transcript.
        let position_note = if position == BacklogPosition::Top {
            ", top of the queue"
        } else {
            ""
        };
        ToolResult::success(format!(
            "added backlog item #{} (pending{position_note}): {preview}",
            item.id
        ))
        .with_data(json!({
            "id": item.id,
            "status": "pending",
            "position": position.as_str(),
        }))
    }
}

/// Render a status as its snake_case wire string (e.g. `cant_resolve`) for
/// tool output and error messages.
fn status_str(s: BacklogStatus) -> String {
    serde_json::to_string(&s)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

/// Arguments for `backlog_status`.
#[derive(Debug, Deserialize)]
struct BacklogStatusArgs {
    /// The backlog item id (shown in its row / in `backlog_add`'s output).
    id: String,
    /// The destination status — required unless the call only toggles
    /// `deferred`.
    #[serde(default)]
    status: Option<BacklogStatus>,
    /// Optional note (e.g. why the item failed / was resolved).
    #[serde(default)]
    note: Option<String>,
    /// Set (`true`) / clear (`false`) the deferred (skip Run-All) flag —
    /// ORTHOGONAL to status: the item keeps its current status (typically
    /// `Pending`) and stays visible, but Run-All never selects or counts
    /// it. Lets a run-all session park a queued item it cannot resolve
    /// instead of failing it.
    #[serde(default)]
    deferred: Option<bool>,
}

/// The `backlog_status` workflow tool — change a backlog item's status through
/// the store's single guarded transition table.
///
/// Available in EVERY workflow state (base states AND inside skills), like
/// `ask_user` / `current_plan` / the memory tools: the backlog is the
/// user-owned queue of tasks waiting to be dispatched, and the agent may
/// legitimately need to mark an item resolved (or an explicit dead end) from
/// any state — mid-execution, during review, or after completing a plan. It
/// holds a clone of the shared store handle + the optional `on_changed`
/// notifier, exactly like [`BacklogAddTool`].
///
/// Status changes go through [`BacklogStore::transition`] — the same guarded
/// table the harness's dispatch/resolution paths use — so the agent can never
/// perform an illegal transition (e.g. marking a `Pending` item `InFlight` is
/// legal; a terminal→non-Pending move is refused). Illegal transitions and
/// unknown ids return a clear error naming the item's current status and its
/// allowed destinations.
///
/// Plan-tied contract (backlog 45dcf577): `done` ⇔ the plan reached
/// `Complete` (normally automatic at turn resolution — manual `done` only for
/// pre-dispatch resolutions); `failed` ⇔ the plan was abandoned; `in_flight`
/// ⇔ a plan is active. Unfinished work is NOT failure — the item stays
/// `in_flight` (a resumed session continues the plan); `cant_resolve` is the
/// explicit dead end.
pub struct BacklogStatusTool {
    store: Arc<tokio::sync::Mutex<BacklogStore>>,
    on_changed: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl BacklogStatusTool {
    /// Build the tool with the shared store handle and an optional
    /// post-change notifier (see [`BacklogAddTool::new`]).
    pub fn new(
        store: Arc<tokio::sync::Mutex<BacklogStore>>,
        on_changed: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        Self { store, on_changed }
    }
}

#[async_trait]
impl Tool for BacklogStatusTool {
    fn name(&self) -> &str {
        "backlog_status"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "backlog_status",
            "Change a backlog item's status and/or its deferred (skip Run-All) \
             flag. The id comes from backlog_add's output or the Backlog tab. \
             Status is tied to the plan lifecycle: done = the plan reached \
             Complete (normally automatic at turn resolution — set it manually \
             only for pre-dispatch resolutions); failed = the plan was \
             abandoned; in_flight = a plan is active. NEVER mark an item failed \
             merely because work is unfinished — unfinished work stays \
             in_flight (a resumed session continues the plan); use \
             cant_resolve for an explicit dead end you cannot recover from. \
             Legal transitions: pending -> in_flight; pending or in_flight -> \
             done/failed/cant_resolve; in_flight -> pending (deferral); \
             done/failed/cant_resolve -> pending (requeue). deferred (boolean, \
             optional): set/clear the skip-Run-All flag — ORTHOGONAL to status \
             (the item stays pending and visible; only Run-All selection skips \
             it). Pass deferred alone, without status, to park/un-park a \
             queued item without a status change — e.g. a run-all session \
              deferring an item it cannot resolve instead of failing it. \
              note alone (no status, no deferred) amends the item's note \
              without touching anything else. At least one of status, \
              deferred, or note is required.",
            json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "The backlog item id (a UUID string from backlog_add's output or the Backlog tab)."
                    },
                    "status": {
                        "type": ["string", "null"],
                        "enum": ["pending", "in_flight", "done", "failed", "cant_resolve"],
                        "description": "The destination status. Required unless the call only toggles `deferred`."
                    },
                    "note": {
                        "type": ["string", "null"],
                        "description": "Optional note (e.g. why the item failed)."
                    },
                    "deferred": {
                        "type": "boolean",
                        "description": "Set (true) / clear (false) the deferred (skip Run-All) flag — orthogonal to status: the item keeps its status and stays visible, but Run-All never selects or counts it. Pass without `status` to park/un-park a queued item."
                    }
                },
                "required": ["id"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: BacklogStatusArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        if args.status.is_none() && args.deferred.is_none() && args.note.is_none() {
            return ToolResult::error(
                "either 'status' (a status transition), 'deferred' (set/clear the \
                 skip-Run-All flag), or 'note' (amend the item's note) is required",
            );
        }
        let mut store = self.store.lock().await;
        let current = store.items().iter().find(|i| i.id == args.id).cloned();
        let Some(current) = current else {
            return ToolResult::error(format!(
                "backlog item '{}' does not exist (evicted or never created)",
                args.id
            ));
        };
        // Status transition first (unchanged semantics): an illegal
        // transition errors BEFORE any mutation — a combined
        // status+deferred call must not leave the flag set when the
        // transition is refused.
        if let Some(status) = args.status {
            if !store.transition(&args.id, status, args.note.clone()) {
                let allowed = current
                    .status
                    .allowed_destinations()
                    .iter()
                    .map(|s| status_str(*s))
                    .collect::<Vec<_>>()
                    .join(", ");
                return ToolResult::error(format!(
                    "illegal backlog transition #{}: {} -> {} (allowed from {}: {})",
                    args.id,
                    status_str(current.status),
                    status_str(status),
                    status_str(current.status),
                    allowed,
                ));
            }
        }
        // Then the deferred flag — orthogonal to status, no transition
        // guard: parking a `Pending` item needs no status change (a
        // same-status transition would be refused by the guarded table).
        if let Some(deferred) = args.deferred {
            store.set_deferred(&args.id, deferred);
        }
        // Note-only (backlog 9118714a): a status-less call with a note
        // amends the note. The transport used to stringify a null status
        // into "null" (enum rejection), forcing a done→pending→done
        // requeue dance just to attach a note; with the dispatch seam
        // dropping the artifact, the note-only call is the natural shape.
        // A deferred+note call lands the note here too (previously the
        // note was silently dropped on that path).
        if args.status.is_none() {
            if let Some(note) = args.note.clone() {
                store.set_note(&args.id, Some(note));
            }
        }
        drop(store);
        if let Some(f) = &self.on_changed {
            f();
        }
        // Name exactly what changed.
        let mut msg = format!("backlog item #{}", args.id);
        if let Some(status) = args.status {
            msg.push_str(&format!(" → {}", status_str(status)));
        }
        if let Some(deferred) = args.deferred {
            msg.push_str(&format!(" deferred (skip Run-All) = {deferred}"));
        }
        if args.status.is_none() && args.note.is_some() {
            msg.push_str(" note amended");
        }
        let mut data = json!({"id": args.id});
        if let Some(status) = args.status {
            data["status"] = json!(status);
        }
        if let Some(deferred) = args.deferred {
            data["deferred"] = json!(deferred);
        }
        ToolResult::success(msg).with_data(data)
    }
}

/// The `backlog_list` workflow tool — read-only query of the backlog.
///
/// Returns every item (id, status, note, text) with pending items first, so
/// an agent can see what is queued without modifying anything. It holds a
/// clone of the shared store handle like [`BacklogAddTool`], but never
/// mutates: it is safe for a read-only reviewer sub-agent (whose strict
/// allow-list names it) — querying the backlog is allowed, modifying it is
/// not (`backlog_add` / `backlog_status` stay out of the reviewer surface).
pub struct BacklogListTool {
    store: Arc<tokio::sync::Mutex<BacklogStore>>,
}

impl BacklogListTool {
    /// Build the tool with the shared store handle.
    pub fn new(store: Arc<tokio::sync::Mutex<BacklogStore>>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for BacklogListTool {
    fn name(&self) -> &str {
        "backlog_list"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "backlog_list",
            "Takes NO arguments — call it with {}. List the user's backlog — the persistent \
             queue of tasks/prompts shown in the Backlog tab, waiting to be dispatched to a \
             future agent session. Read-only: it returns every item (id, status, note, \
             truncated text) with pending items first, and changes nothing. Use it to see \
             what is queued before adding or updating items, and whenever you need the item \
             id for backlog_status. Available in every workflow state (a read-only query, \
             like the memory tools).",
            json!({ "type": "object", "properties": {} }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, _args: serde_json::Value) -> ToolResult {
        let store = self.store.lock().await;
        let mut items = store.items().to_vec();
        // Pending first (the queue's front), then by id for the rest.
        items.sort_by_key(|i| {
            (
                i.status != crate::backlog::BacklogStatus::Pending,
                i.id.clone(),
            )
        });
        if items.is_empty() {
            return ToolResult::success("backlog is empty (no items queued)");
        }
        let lines: Vec<String> = items
            .iter()
            .map(|i| {
                let mut line = format!("{} [{}]", i.id, status_str(i.status));
                if i.deferred {
                    line.push_str(" (deferred — skipped by Run-All)");
                }
                if let Some(note) = &i.note {
                    line.push_str(&format!(" — {note}"));
                }
                line.push_str(&format!(": {}", preview_text(&i.text)));
                line
            })
            .collect();
        ToolResult::success(lines.join("\n")).with_data(json!({
            "items": items.iter().map(|i| json!({
                "id": i.id,
                "status": status_str(i.status),
                "deferred": i.deferred,
                "text": i.text,
                "note": i.note,
            })).collect::<Vec<_>>(),
        }))
    }
}

/// Truncate an item's text for a one-line list rendering.
fn preview_text(text: &str) -> String {
    match text.chars().nth(OUTPUT_PREVIEW_CHARS) {
        Some(_) => format!(
            "{}…",
            text.chars().take(OUTPUT_PREVIEW_CHARS).collect::<String>()
        ),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh in-memory-backed store at a temp path, wrapped the way the
    /// app shares it. No notifier (tests don't need the UI seam).
    fn tool_at(path: std::path::PathBuf) -> BacklogAddTool {
        BacklogAddTool::new(
            Arc::new(tokio::sync::Mutex::new(BacklogStore::open(path))),
            None,
        )
    }

    #[test]
    fn name_category_safety() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_at(dir.path().join("backlog.jsonl"));
        assert_eq!(tool.name(), "backlog_add");
        assert_eq!(tool.category(), ToolCategory::Workflow);
        // Benign, UI-deletable note — never approval-gated (create_plan
        // precedent for Workflow + AutoRun).
        assert_eq!(tool.safety(), SafetyLevel::AutoRun);
    }

    #[tokio::test]
    async fn add_success_allocates_id_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backlog.jsonl");
        let tool = tool_at(path.clone());

        let result = tool
            .execute(json!({"text": "Add the flux capacitor to the build\n\nProblem: the reactor stalls on cold start (2027-01-07); repro: build twice. Fix direction: order the part and wire it into src/reactor/mod.rs, then add a build test. Acceptance: cargo test reactor. Related: review 568d405."}))
            .await;
        assert!(result.success, "output: {}", result.output);
        let id = result.data.clone().unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(!id.is_empty(), "a UUID id is allocated");
        assert!(result.output.contains("pending"));

        // The store persists through every mutation — reopening the file
        // must yield the item verbatim.
        let reopened = BacklogStore::open(path);
        let items = reopened.items().to_vec();
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].text,
            "Add the flux capacitor to the build\n\nProblem: the reactor stalls on cold start (2027-01-07); repro: build twice. Fix direction: order the part and wire it into src/reactor/mod.rs, then add a build test. Acceptance: cargo test reactor. Related: review 568d405."
        );
        assert_eq!(items[0].status, crate::backlog::BacklogStatus::Pending);
    }

    #[tokio::test]
    async fn empty_and_whitespace_text_error() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_at(dir.path().join("backlog.jsonl"));

        for bad in ["", "   ", "\n\t"] {
            let result = tool.execute(json!({"text": bad})).await;
            assert!(!result.success, "should reject {bad:?}");
            assert!(result.output.contains("empty"), "output: {}", result.output);
        }
        // Nothing was persisted.
        assert!(tool.store.lock().await.items().is_empty());
    }

    #[tokio::test]
    async fn oversize_text_errors() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_at(dir.path().join("backlog.jsonl"));

        let result = tool.execute(json!({"text": "x".repeat(4001)})).await;
        assert!(!result.success);
        assert!(result.output.contains("4000"), "output: {}", result.output);
        assert!(tool.store.lock().await.items().is_empty());

        // Exactly 4000 chars is the boundary and must be ACCEPTED (review
        // finding 9 — the > comparison makes it pass, but nothing pinned it).
        // Shape-valid: headline + blank line + body totaling exactly 4000
        // (a single-line 4000-char text would now fail the headline check);
        // the body carries a path marker so it also passes the detail bar.
        let headline = "Boundary task";
        let marker = "src/foo.rs ";
        let body = format!(
            "{marker}{}",
            "y".repeat(4000 - headline.chars().count() - 2 - marker.chars().count())
        );
        let boundary = tool
            .execute(json!({"text": format!("{headline}\n\n{body}")}))
            .await;
        assert!(boundary.success, "output: {}", boundary.output);
        assert_eq!(tool.store.lock().await.items().len(), 1);
    }

    #[tokio::test]
    async fn add_rejects_single_line_text_with_shape_error() {
        // A single-line item renders as a bare headline with no body — the
        // error must name the required shape so the model can retry in one
        // shot (backlog 45a4eb88).
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_at(dir.path().join("backlog.jsonl"));

        let result = tool.execute(json!({"text": "Fix the login loop"})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("headline AND a body"),
            "output: {}",
            result.output
        );
        assert!(
            result.output.contains("blank line"),
            "output: {}",
            result.output
        );
        assert!(tool.store.lock().await.items().is_empty());
    }

    #[tokio::test]
    async fn add_rejects_overlong_headline() {
        // A wall-of-text first line renders as an oversized headline —
        // rejected with the limit named.
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_at(dir.path().join("backlog.jsonl"));

        let long = "x".repeat(101);
        let result = tool
            .execute(json!({"text": format!("{long}\n\nBody here.")}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("max 100"),
            "output: {}",
            result.output
        );
        assert!(tool.store.lock().await.items().is_empty());
    }

    #[tokio::test]
    async fn add_accepts_headline_and_body_storing_trimmed_text() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_at(dir.path().join("backlog.jsonl"));

        // A body that passes the detail bar (2027-01-07): ≥160 chars and a
        // file path — the tool routes through normalize_item_text.
        let text = "Fix the login loop\n\nProblem: the login loop re-prompts after auth \
                    (2027-01-07); repro: log in twice. Fix direction: clear the session flag \
                    in src/auth/session.rs before redirecting. Acceptance: cargo test \
                    login_loop. Related: review 568d405.";
        let result = tool.execute(json!({"text": format!("  {text}  ")})).await;
        assert!(result.success, "output: {}", result.output);
        // The trimmed text is stored verbatim.
        assert_eq!(tool.store.lock().await.items()[0].text, text);
    }

    #[tokio::test]
    async fn add_with_position_top_lands_first() {
        // Backlog 06a31736: position "top" inserts at the FRONT of the
        // queue — the item is dispatched first — and the result names it.
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_at(dir.path().join("backlog.jsonl"));

        let first = tool
            .execute(json!({"text": "Queue the flux capacitor order\n\nProblem: the reactor stalls on cold start (2027-01-07); repro: build twice. Fix direction: order the part and wire it into src/reactor/mod.rs, then add a build test. Acceptance: cargo test reactor. Related: review 568d405."}))
            .await;
        assert!(first.success, "output: {}", first.output);

        let top = tool
            .execute(json!({"text": "Fix the run-all stall first\n\nProblem: run-all stalls after the plan closes (2027-01-08); repro: start run-all with two items. Fix direction: resume the dispatch loop on the finish notification in src-tauri/src/ipc/run_all.rs. Acceptance: cargo test run_all. Related: backlog e33a07fd.", "position": "top"}))
            .await;
        assert!(top.success, "output: {}", top.output);

        let items = tool.store.lock().await.items().to_vec();
        assert_eq!(items.len(), 2);
        assert!(
            items[0].text.starts_with("Fix the run-all stall"),
            "the top item is first in store order"
        );
        // The result names the top insert and reports it in the data.
        assert!(
            top.output.contains("top of the queue"),
            "output: {}",
            top.output
        );
        let data = top.data.clone().unwrap();
        assert_eq!(data["position"], "top");
        assert_eq!(data["id"].as_str().unwrap(), items[0].id);
    }

    #[tokio::test]
    async fn add_without_position_appends_as_today() {
        // The default/absent case appends exactly as today (FIFO); an
        // explicit "end" behaves identically — same output format (no top
        // note), data reporting "end".
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_at(dir.path().join("backlog.jsonl"));

        let text = "Fix the login loop\n\nProblem: the login loop re-prompts after auth \
                    (2027-01-07); repro: log in twice. Fix direction: clear the session flag \
                    in src/auth/session.rs before redirecting. Acceptance: cargo test \
                    login_loop. Related: review 568d405.";
        let first = tool.execute(json!({"text": text})).await;
        assert!(first.success, "output: {}", first.output);
        let second = tool.execute(json!({"text": text, "position": "end"})).await;
        assert!(second.success, "output: {}", second.output);

        // FIFO order: the absent-position add first, the explicit end add
        // second.
        let items = tool.store.lock().await.items().to_vec();
        assert_eq!(items.len(), 2);
        let id1 = first.data.clone().unwrap()["id"].as_str().unwrap().to_string();
        let id2 = second.data.clone().unwrap()["id"].as_str().unwrap().to_string();
        assert_eq!(items[0].id, id1);
        assert_eq!(items[1].id, id2);
        assert_eq!(first.data.clone().unwrap()["position"], "end");
        assert_eq!(second.data.clone().unwrap()["position"], "end");
        // The end output is byte-identical to the absent-position output
        // (same text → same preview; only the ids differ).
        assert_eq!(
            second.output.replace(&id2, "#"),
            first.output.replace(&id1, "#"),
            "absent and explicit end produce the same output format"
        );
        assert!(
            !second.output.contains("top of the queue"),
            "no top note on an end add: {}",
            second.output
        );
    }

    #[tokio::test]
    async fn add_rejects_invalid_position() {
        // An unknown position value errors naming the valid variants —
        // and nothing is persisted.
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_at(dir.path().join("backlog.jsonl"));

        let result = tool
            .execute(json!({"text": "Fix the login loop\n\nProblem: the login loop re-prompts after auth (2027-01-07); repro: log in twice. Fix direction: clear the session flag in src/auth/session.rs before redirecting. Acceptance: cargo test login_loop. Related: review 568d405.", "position": "middle"}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("top") && result.output.contains("end"),
            "the error names the valid variants: {}",
            result.output
        );
        assert!(tool.store.lock().await.items().is_empty());
    }

    #[tokio::test]
    async fn top_position_still_enforces_the_detail_bar() {
        // The position parameter must not bypass the shape contract — a
        // top add with a single-line text is rejected, nothing persisted
        // (the detail bar runs before the store is touched).
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_at(dir.path().join("backlog.jsonl"));

        let result = tool
            .execute(json!({"text": "Fix the login loop", "position": "top"}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("headline AND a body"),
            "output: {}",
            result.output
        );
        assert!(tool.store.lock().await.items().is_empty());
    }

    #[test]
    fn add_schema_documents_the_position_parameter() {
        // The model's source of truth: the position property must declare
        // the enum + default and teach when top is appropriate (urgent
        // items only — the queue is otherwise FIFO); text stays the only
        // required argument.
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_at(dir.path().join("backlog.jsonl"));
        let params = tool.schema().parameters;
        let position = params["properties"]["position"]
            .as_object()
            .expect("schema has properties.position");
        assert_eq!(position["type"], "string");
        assert_eq!(position["enum"], json!(["top", "end"]));
        assert_eq!(position["default"], "end");
        let desc = position["description"].as_str().expect("position description");
        assert!(desc.contains("urgent"), "desc: {desc}");
        assert!(desc.contains("FIFO"), "desc: {desc}");
        assert_eq!(params["required"], json!(["text"]));
        // The top-level description teaches the parameter too.
        let tool_desc = tool.schema().description.to_lowercase();
        assert!(tool_desc.contains("position"), "desc: {tool_desc}");
        assert!(tool_desc.contains("top"), "desc: {tool_desc}");
    }

    #[test]
    fn add_schema_declares_the_required_shape() {
        // The model's source of truth: the text-param description must spell
        // out the headline+body shape (the validator enforces the same
        // contract — see normalize_item_text).
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_at(dir.path().join("backlog.jsonl"));
        let params = tool.schema().parameters;
        let desc = params["properties"]["text"]["description"]
            .as_str()
            .expect("schema has properties.text.description");
        assert!(desc.contains("headline"), "desc: {desc}");
        assert!(desc.contains("blank line"), "desc: {desc}");
        assert!(desc.contains("body"), "desc: {desc}");
        // The limits are built from the enforcing consts (review LOW-2) —
        // assert the const values appear so the description can never
        // drift from what normalize_item_text actually enforces.
        assert!(
            desc.contains(&MAX_ITEM_TEXT_CHARS.to_string()),
            "desc: {desc}"
        );
        assert!(
            desc.contains(&MAX_HEADLINE_CHARS.to_string()),
            "desc: {desc}"
        );
    }

    #[tokio::test]
    async fn notifier_fires_after_each_successful_add() {
        // The UI seam: when the app layer injects an on_changed notifier, it
        // must fire exactly once per successful add (so the Backlog tab can
        // refresh live). Failed adds (empty/oversize) must NOT fire it.
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(tokio::sync::Mutex::new(BacklogStore::open(
            dir.path().join("backlog.jsonl"),
        )));
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fired_clone = std::sync::Arc::clone(&fired);
        let tool = BacklogAddTool::new(
            store,
            Some(std::sync::Arc::new(move || {
                fired_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })),
        );

        let ok = tool
            .execute(json!({"text": "First item\n\nProblem: first notifier fire (2027-01-07); repro: add one item and count the callbacks. Fix direction: patch src/one.rs to fire the notifier after the persist. Acceptance: cargo test notifier_fires. Related: review 568d405."}))
            .await;
        assert!(ok.success, "output: {}", ok.output);
        assert_eq!(fired.load(std::sync::atomic::Ordering::Relaxed), 1);

        let ok = tool
            .execute(json!({"text": "Second item\n\nProblem: second notifier fire (2027-01-07); repro: add a second item and count again. Fix direction: patch src/two.rs the same way so each add fires once. Acceptance: cargo test notifier_counts. Related: review 568d405."}))
            .await;
        assert!(ok.success, "output: {}", ok.output);
        assert_eq!(fired.load(std::sync::atomic::Ordering::Relaxed), 2);

        // A rejected add must not notify (nothing changed on disk).
        let bad = tool.execute(json!({"text": "   "})).await;
        assert!(!bad.success);
        assert_eq!(fired.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn no_notifier_is_a_noop() {
        // Console mode / tests wire no notifier — adds must still succeed
        // and the missing callback must not panic.
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_at(dir.path().join("backlog.jsonl"));
        let result = tool
            .execute(json!({"text": "Headless add\n\nProblem: headless mode (2027-01-07); repro: run without a notifier. Fix direction: verify src/tool/workflow/backlog.rs wiring. Acceptance: cargo test backlog. Related: none."}))
            .await;
        assert!(result.success, "output: {}", result.output);
        assert_eq!(tool.store.lock().await.items().len(), 1);
    }

    /// A status tool over a store with one pending item. Returns the tool,
    /// the store path, and the seeded item's id.
    async fn status_tool_with_item(
        dir: &tempfile::TempDir,
    ) -> (BacklogStatusTool, std::path::PathBuf, String) {
        let path = dir.path().join("backlog.jsonl");
        let id = {
            let store = Arc::new(tokio::sync::Mutex::new(BacklogStore::open(path.clone())));
            let item = store.lock().await.add("task".into(), vec![]);
            item.id
        };
        (
            BacklogStatusTool::new(
                Arc::new(tokio::sync::Mutex::new(BacklogStore::open(path.clone()))),
                None,
            ),
            path,
            id,
        )
    }

    #[test]
    fn status_tool_name_category_safety() {
        let dir = tempfile::tempdir().unwrap();
        let tool = BacklogStatusTool::new(
            Arc::new(tokio::sync::Mutex::new(BacklogStore::open(
                dir.path().join("backlog.jsonl"),
            ))),
            None,
        );
        assert_eq!(tool.name(), "backlog_status");
        assert_eq!(tool.category(), ToolCategory::Workflow);
        assert_eq!(tool.safety(), SafetyLevel::AutoRun);
    }

    #[test]
    fn status_tool_schema_declares_id_as_string() {
        // Regression: the schema declared `id` as "integer" but the struct
        // field is `String` and actual IDs are UUID strings — so every call
        // that followed the schema sent an integer serde rejected with
        // "invalid type: integer, expected a string". The schema must declare
        // "string" to match. This test fails on the old (integer) schema.
        let dir = tempfile::tempdir().unwrap();
        let tool = BacklogStatusTool::new(
            Arc::new(tokio::sync::Mutex::new(BacklogStore::open(
                dir.path().join("backlog.jsonl"),
            ))),
            None,
        );
        let params = tool.schema().parameters;
        let id_type = params["properties"]["id"]["type"]
            .as_str()
            .expect("schema has properties.id.type");
        assert_eq!(
            id_type, "string",
            "id must be a string (UUID), not an integer — else serde rejects the call"
        );
    }

    #[tokio::test]
    async fn status_tool_legal_transition_changes_status_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let (tool, path, id) = status_tool_with_item(&dir).await;

        // Pending → Done (the canonical agent resolution).
        let result = tool.execute(json!({"id": id, "status": "done"})).await;
        assert!(result.success, "output: {}", result.output);
        assert!(result.output.contains("done"), "output: {}", result.output);
        assert_eq!(result.data.unwrap()["status"], "done");

        // Persists through the transition.
        let reopened = BacklogStore::open(path.clone());
        assert_eq!(
            reopened.items()[0].status,
            crate::backlog::BacklogStatus::Done
        );

        // A note is carried onto the record.
        let result = tool
            .execute(json!({"id": id, "status": "pending", "note": "retry please"}))
            .await;
        assert!(result.success, "output: {}", result.output);
        let reopened = BacklogStore::open(path);
        assert_eq!(
            reopened.items()[0].status,
            crate::backlog::BacklogStatus::Pending
        );
        assert_eq!(reopened.items()[0].note.as_deref(), Some("retry please"));
    }

    #[tokio::test]
    async fn status_tool_illegal_transition_errors_and_does_not_persist() {
        let dir = tempfile::tempdir().unwrap();
        let (tool, path, id) = status_tool_with_item(&dir).await;

        // A self-transition (pending → pending) is not in the guarded table —
        // must error, change nothing, persist nothing. The error must name the
        // per-status allowed set so the caller can re-issue a corrected call
        // directly (review finding L1).
        let result = tool.execute(json!({"id": id, "status": "pending"})).await;
        assert!(!result.success, "should reject: {}", result.output);
        assert!(
            result.output.contains("illegal"),
            "output: {}",
            result.output
        );
        assert!(
            result
                .output
                .contains("allowed from pending: in_flight, done, failed, cant_resolve"),
            "output: {}",
            result.output
        );
        let reopened = BacklogStore::open(path.clone());
        assert_eq!(
            reopened.items()[0].status,
            crate::backlog::BacklogStatus::Pending
        );

        // Terminal → terminal is also refused (done → failed); the allowed set
        // from a terminal status is requeue-only.
        let ok = tool.execute(json!({"id": id, "status": "done"})).await;
        assert!(ok.success, "output: {}", ok.output);
        let bad = tool.execute(json!({"id": id, "status": "failed"})).await;
        assert!(!bad.success);
        assert!(bad.output.contains("illegal"), "output: {}", bad.output);
        assert!(
            bad.output.contains("allowed from done: pending"),
            "output: {}",
            bad.output
        );
        let reopened = BacklogStore::open(path);
        assert_eq!(
            reopened.items()[0].status,
            crate::backlog::BacklogStatus::Done
        );
    }

    #[tokio::test]
    async fn status_tool_unknown_id_errors() {
        let dir = tempfile::tempdir().unwrap();
        let (tool, _path, _id) = status_tool_with_item(&dir).await;
        let result = tool
            .execute(json!({"id": "no-such-id", "status": "done"}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("does not exist"),
            "output: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn status_tool_notifier_fires_only_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backlog.jsonl");
        let store = Arc::new(tokio::sync::Mutex::new(BacklogStore::open(path.clone())));
        let id = store.lock().await.add("task".into(), vec![]).id;
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fired_clone = std::sync::Arc::clone(&fired);
        let tool = BacklogStatusTool::new(
            store,
            Some(std::sync::Arc::new(move || {
                fired_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })),
        );

        // Legal transition fires the notifier exactly once.
        let ok = tool.execute(json!({"id": id, "status": "in_flight"})).await;
        assert!(ok.success, "output: {}", ok.output);
        assert_eq!(fired.load(std::sync::atomic::Ordering::Relaxed), 1);

        // Illegal transition must NOT fire it (nothing changed on disk).
        // After in_flight, a self-transition (in_flight → in_flight) is not
        // in the guarded table.
        let bad = tool.execute(json!({"id": id, "status": "in_flight"})).await;
        assert!(!bad.success);
        assert_eq!(fired.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    // ---- backlog_list (read-only query) ----

    #[test]
    fn list_tool_name_category_safety() {
        let dir = tempfile::tempdir().unwrap();
        let tool = BacklogListTool::new(Arc::new(tokio::sync::Mutex::new(BacklogStore::open(
            dir.path().join("backlog.jsonl"),
        ))));
        assert_eq!(tool.name(), "backlog_list");
        assert_eq!(tool.category(), ToolCategory::Workflow);
        assert_eq!(tool.safety(), SafetyLevel::AutoRun);
        assert!(
            tool.schema().description.starts_with("Takes NO arguments"),
            "the zero-arg note LEADS the description: {}",
            tool.schema().description
        );
    }

    #[tokio::test]
    async fn list_tool_returns_items_pending_first_without_mutating() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backlog.jsonl");
        let store = Arc::new(tokio::sync::Mutex::new(BacklogStore::open(path.clone())));
        // Seed: first item done, rest pending.
        let a = store.lock().await.add("first task".into(), vec![]);
        let b = store.lock().await.add("second task".into(), vec![]);
        let c = store.lock().await.add("third task".into(), vec![]);
        let done_id = a.id.clone();
        store
            .lock()
            .await
            .transition(&done_id, crate::backlog::BacklogStatus::Done, None);
        let tool = BacklogListTool::new(store);

        let result = tool.execute(json!({})).await;
        assert!(result.success, "output: {}", result.output);
        // Pending items first, then done.
        let output = result.output.clone();
        let pos_b = output.find(&b.id).expect("item b in output");
        let pos_c = output.find(&c.id).expect("item c in output");
        let pos_a = output.find(&a.id).expect("item a in output");
        assert!(
            pos_b < pos_a,
            "pending item b must precede done item a: {output}"
        );
        assert!(
            pos_c < pos_a,
            "pending item c must precede done item a: {output}"
        );
        // The statuses render in the one-line list.
        assert!(output.contains("[pending]"), "output: {output}");
        assert!(output.contains("[done]"), "output: {output}");

        // Structured data carries the full items.
        let data = result.data.expect("list carries data");
        let items = data["items"].as_array().expect("items array");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["status"], "pending");
        // Pending items are ordered by id (UUID string) — either pending one
        // may lead; the done item is always last.
        assert!(
            items[0]["id"].as_str() == Some(b.id.as_str())
                || items[0]["id"].as_str() == Some(c.id.as_str()),
            "first item is pending: {}",
            items[0]["id"]
        );
        assert_eq!(items[2]["status"], "done");
        assert_eq!(items[2]["id"].as_str(), Some(a.id.as_str()));

        // The query must not mutate anything.
        let reopened = BacklogStore::open(path);
        assert_eq!(reopened.items().len(), 3);
        assert_eq!(
            reopened.items()[0].status,
            crate::backlog::BacklogStatus::Done
        );
    }

    #[tokio::test]
    async fn list_tool_empty_backlog_reports_empty() {
        let dir = tempfile::tempdir().unwrap();
        let tool = BacklogListTool::new(Arc::new(tokio::sync::Mutex::new(BacklogStore::open(
            dir.path().join("backlog.jsonl"),
        ))));
        let result = tool.execute(json!({})).await;
        assert!(result.success, "output: {}", result.output);
        assert!(result.output.contains("empty"), "output: {}", result.output);
    }

    // ---- deferred (skip Run-All) — backlog f2d2809b, user request
    // 2027-01-07 ----

    #[tokio::test]
    async fn status_tool_deferred_only_parks_a_pending_item() {
        // A run-all session parks a queued item it cannot resolve instead
        // of failing it: `deferred` alone (no status) sets the flag without
        // touching the status — a same-status transition would be refused
        // by the guarded table, so the flag must not require one.
        let dir = tempfile::tempdir().unwrap();
        let (tool, path, id) = status_tool_with_item(&dir).await;

        let result = tool.execute(json!({"id": id, "deferred": true})).await;
        assert!(result.success, "output: {}", result.output);
        assert!(
            result.output.contains("deferred"),
            "output: {}",
            result.output
        );
        assert_eq!(result.data.unwrap()["deferred"], true);

        let reopened = BacklogStore::open(path.clone());
        assert!(reopened.items()[0].deferred);
        assert_eq!(
            reopened.items()[0].status,
            crate::backlog::BacklogStatus::Pending,
            "parking keeps the status"
        );

        // Un-park the same way.
        let result = tool.execute(json!({"id": id, "deferred": false})).await;
        assert!(result.success, "output: {}", result.output);
        assert!(!BacklogStore::open(path).items()[0].deferred);
    }

    #[tokio::test]
    async fn status_tool_status_and_deferred_combine() {
        let dir = tempfile::tempdir().unwrap();
        let (tool, path, id) = status_tool_with_item(&dir).await;

        let result = tool
            .execute(json!({"id": id, "status": "in_flight", "deferred": true}))
            .await;
        assert!(result.success, "output: {}", result.output);
        let reopened = BacklogStore::open(path);
        assert_eq!(
            reopened.items()[0].status,
            crate::backlog::BacklogStatus::InFlight
        );
        assert!(reopened.items()[0].deferred);
    }

    #[tokio::test]
    async fn status_tool_requires_status_or_deferred() {
        let dir = tempfile::tempdir().unwrap();
        let (tool, _path, id) = status_tool_with_item(&dir).await;
        let result = tool.execute(json!({"id": id})).await;
        assert!(!result.success);
        assert!(
            result
                .output
                .contains("either 'status' (a status transition), 'deferred'"),
            "output: {}",
            result.output
        );
        assert!(
            result.output.contains("'note' (amend the item's note)"),
            "the error names the note-only shape (review LOW-1): {}",
            result.output
        );
    }

    #[tokio::test]
    async fn status_tool_stringified_null_status_updates_note_only() {
        // Backlog 9118714a: the transport stringifies JSON null for string/
        // enum params into the literal string "null" — a note-only update
        // arrived as status:"null" and was rejected by the enum guard,
        // forcing the done→pending→done requeue dance. The dispatch seam
        // drops the artifact from OPTIONAL properties; composed here with
        // the tool exactly as the seam composes them, the note-only update
        // succeeds and the status is untouched.
        let dir = tempfile::tempdir().unwrap();
        let (tool, path, id) = status_tool_with_item(&dir).await;

        let mut args = json!({"id": id, "status": "null", "note": "amended"});
        crate::tool::drop_stringified_nulls(&tool.schema().parameters, &mut args);
        let result = tool.execute(args).await;
        assert!(result.success, "note-only update: {}", result.output);
        assert!(
            result.output.contains("note amended"),
            "output: {}",
            result.output
        );

        let reopened = BacklogStore::open(path);
        assert_eq!(
            reopened.items()[0].status,
            crate::backlog::BacklogStatus::Pending,
            "the dropped status leaves the item untouched"
        );
        assert!(
            reopened.items()[0]
                .note
                .as_deref()
                .is_some_and(|n| n.contains("amended")),
            "the note persisted"
        );
    }

    #[test]
    fn status_tool_optional_string_params_advertise_nullable() {
        // Backlog 9118714a: optional string/enum params advertise
        // ["string", "null"] so explicit JSON null is legal end-to-end —
        // the spawn_agent.model precedent mirrored onto this tool.
        let dir = tempfile::tempdir().unwrap();
        let tool = BacklogStatusTool::new(
            Arc::new(tokio::sync::Mutex::new(BacklogStore::open(
                dir.path().join("backlog.jsonl"),
            ))),
            None,
        );
        let params = tool.schema().parameters;
        for field in ["status", "note"] {
            let ty = &params["properties"][field]["type"];
            assert!(
                ty.as_array().is_some_and(|t| t.contains(&json!("string"))
                    && t.contains(&json!("null"))),
                "{field} must advertise [\"string\", \"null\"], got: {ty}"
            );
        }
    }

    #[tokio::test]
    async fn status_tool_illegal_transition_does_not_set_deferred() {
        // A combined call whose transition is refused must not leave the
        // flag set — the error path is side-effect-free.
        let dir = tempfile::tempdir().unwrap();
        let (tool, path, id) = status_tool_with_item(&dir).await;

        let result = tool
            .execute(json!({"id": id, "status": "pending", "deferred": true}))
            .await;
        assert!(!result.success, "self-transition is refused");
        let reopened = BacklogStore::open(path);
        assert!(
            !reopened.items()[0].deferred,
            "the refused call must not set the flag"
        );
        assert_eq!(
            reopened.items()[0].status,
            crate::backlog::BacklogStatus::Pending
        );
    }

    #[tokio::test]
    async fn list_tool_marks_deferred_items() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backlog.jsonl");
        let store = Arc::new(tokio::sync::Mutex::new(BacklogStore::open(path)));
        let a = store.lock().await.add("task".into(), vec![]);
        store.lock().await.set_deferred(&a.id, true);
        let tool = BacklogListTool::new(store);

        let result = tool.execute(json!({})).await;
        assert!(result.success, "output: {}", result.output);
        assert!(
            result.output.contains("(deferred — skipped by Run-All)"),
            "output: {}",
            result.output
        );
        let data = result.data.expect("list carries data");
        assert_eq!(data["items"][0]["deferred"], true);
    }
}
