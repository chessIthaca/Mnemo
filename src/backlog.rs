// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The backlog store — a persistent, ordered list of prompts waiting to be
//! dispatched to the main agent.
//!
//! Backed by `.coding/backlog.jsonl` under the project root — one JSON item
//! per line, so concurrent `backlog_add` calls from multiple worktrees merge
//! cleanly through git (the file is tracked; a `.gitattributes` union merge
//! driver concatenates both sides' lines, and the UUID ids make the union
//! collision-free). Every mutation writes through to disk atomically (temp
//! file + rename) so the list survives restarts and crashes. Loading is
//! tolerant: a missing or corrupt file yields an empty store (with a warning
//! logged), never a panic. A legacy `.coding/backlog.json` (the pre-jsonl
//! envelope format) is migrated on open.
//!
//! ## Deletion lifecycle
//!
//! Deleting an item ([`BacklogStore::remove`],
//! [`BacklogStore::clear_finished`]) is a SOFT delete: the item is marked
//! `deleted_at` and disappears from every read path (UI, dispatch, the
//! memory indexer), but its JSONL line and image sidecar files stay on disk.
//! Keeping the line makes deletion survive the git union merge — a
//! hard-removed line would be resurrected by the other side's copy of the
//! file. [`BacklogStore::open`] hard-purges items soft-deleted more than
//! [`PURGE_AFTER_SECS`] (30 days) ago, so every startup cleans up expired
//! deletions.

use std::path::PathBuf;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

// base64 decode for image sidecar files (data URLs → binary files).
use base64::Engine as _;

/// How long a soft-deleted backlog item survives before
/// [`BacklogStore::open`] hard-purges it (and its image sidecar files):
/// 30 days.
pub const PURGE_AFTER_SECS: u64 = 30 * 24 * 60 * 60;

/// The maximum length of a backlog item's text, in characters — enforced by
/// [`normalize_item_text`] at every write path (the `backlog_add` agent tool
/// and the IPC `backlog_add` command).
pub const MAX_ITEM_TEXT_CHARS: usize = 4000;

/// The maximum length of a backlog item's first line (the headline). The
/// Backlog tab renders the first line as a styled headline (backlog
/// 40763a24), so a wall of text there breaks the card — enforced by
/// [`normalize_item_text`].
pub const MAX_HEADLINE_CHARS: usize = 100;

/// The minimum length of a backlog item's BODY, in characters — the detail
/// bar (2027-01-07): Run-All dispatches items to whatever model/effort is
/// configured, and a terse body leaves a lesser-reasoning session
/// floundering (re-deriving context, missing file paths, fixing the wrong
/// thing). Enforced by [`normalize_item_text`] alongside
/// [`body_carries_detail_marker`]; the whole item still fits
/// [`MAX_ITEM_TEXT_CHARS`].
pub const MIN_BODY_CHARS: usize = 160;

/// The detail-bar requirement shared by both rejection messages — names the
/// five elements a dispatched (possibly lesser-reasoning) model needs, so an
/// LLM caller can retry in one shot.
const DETAIL_BAR_REQUIREMENT: &str = "the body must carry: (1) the problem/symptom with dates + \
     how to reproduce it, (2) exact file paths and symbol names, (3) the fix direction, (4) \
     acceptance criteria / how to verify, (5) pointers to related memories, commits, reviews";

/// Whether a text body carries a detail marker — the structural half of the
/// backlog detail bar (2027-01-07): at least one file-path-like token
/// (a path with two separators, a separator plus an extension, a Windows
/// drive prefix, or a bare filename with a code-ish extension) or an
/// explicit no-code marker for research items. Prose like "and/or" must
/// NOT count: a single separator without an extension is not a path.
///
/// Shared with the plan resumability gate (2027-01-09,
/// `src/tool/workflow/plan.rs`): plan steps must name concrete file paths
/// (or carry the same no-code markers), so both gates speak the same
/// path/marker vocabulary and reject with the same UX.
pub(crate) fn body_carries_detail_marker(body: &str) -> bool {
    static PATHISH: OnceLock<regex::Regex> = OnceLock::new();
    let pathish = PATHISH.get_or_init(|| {
        regex::Regex::new(
            // Two separators (src/lib/foo.rs, frontend/src/lib/tauri.ts,
            // URLs) OR a separator plus an extension (src/foo.rs) OR a
            // Windows drive prefix (C:\repo\src) OR a bare filename with a
            // code-ish extension (README.md, Cargo.toml).
            r"(?:\S*[\\/]\S*[\\/]\S*)|(?:\S*[\\/]\S*\.\w{1,6})|(?:[A-Za-z]:[\\/]\S*)|(?:\b[\w.-]+\.(?:rs|ts|tsx|toml|md|json|js|jsx|css|html|py|go|yml|yaml|sql|sh|ps1)\b)",
        )
        .expect("detail-marker regex is valid")
    });
    // A match counts only when the matched token carries at least one
    // alphabetic character (review LOW-2, round 1): the two-separator
    // alternative alone would accept M/D/Y dates ("10/10/2027" — digits
    // and separators only), and the detail bar itself demands dates in
    // the problem section, so a date-only body would slip the marker
    // check without an exact pointer. Real paths always carry letters (a
    // path segment, an extension, a drive letter, a URL host).
    if pathish
        .find_iter(body)
        .any(|m| m.as_str().chars().any(|c| c.is_alphabetic()))
    {
        return true;
    }
    // Explicit no-code escape hatch: research items carry no paths.
    let lower = body.to_lowercase();
    lower.contains("no-code research")
        || lower.contains("no code changes")
        || lower.contains("research only")
        || lower.contains("research-only")
}

/// A live item is one that has not been soft-deleted (`deleted_at` unset).
/// Soft-deleted items stay in the store's full list but are invisible to
/// every read path — [`BacklogStore::items`], dispatch, the memory indexer —
/// until the startup purge hard-removes them.
fn is_live(item: &BacklogItem) -> bool {
    item.deleted_at.is_none()
}

/// Normalize + validate a backlog item's text at a write path — the single
/// shape contract for NEW items, routed through by both the `backlog_add`
/// agent tool and the IPC `backlog_add` command.
///
/// The Backlog tab renders the first line as a styled headline and the rest
/// as the body (backlog 40763a24), so items must carry that shape on the way
/// in: a short headline (≤ [`MAX_HEADLINE_CHARS`]) followed by a body. The
/// check is structural — any non-whitespace content after the first line
/// counts as the body (the recommended form separates them with a blank
/// line, but `Headline\nbody` renders identically, so the blank line is
/// described, not enforced). Existing items are grandfathered: the store
/// itself never validates, only the add paths do.
///
/// The body also passes the detail bar (2027-01-07): at least
/// [`MIN_BODY_CHARS`] characters AND at least one file-path-like token (or
/// an explicit no-code marker — see [`body_carries_detail_marker`]).
/// Run-All dispatches items to whatever model/effort is configured, so a
/// terse body leaves a lesser-reasoning session floundering; the rejection
/// message names the five elements the body must carry so an LLM caller
/// can retry in one shot.
///
/// Returns the trimmed text on success. The error names the violated rule
/// and the required shape so an LLM caller can retry in one shot.
pub fn normalize_item_text(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("backlog item text must not be empty or whitespace-only".into());
    }
    if text.chars().count() > MAX_ITEM_TEXT_CHARS {
        return Err(format!(
            "backlog item text is {} chars (max {}); shorten the body",
            text.chars().count(),
            MAX_ITEM_TEXT_CHARS
        ));
    }
    let mut lines = text.split('\n');
    let headline = lines.next().unwrap_or("").trim_end();
    if headline.chars().count() > MAX_HEADLINE_CHARS {
        return Err(format!(
            "backlog item headline (first line) is {} chars (max {}) — move the detail into \
             the body. Required shape: first line a short headline, then a blank line, then \
             the body",
            headline.chars().count(),
            MAX_HEADLINE_CHARS
        ));
    }
    let body = lines.collect::<Vec<_>>().join("\n");
    let body = body.trim();
    if body.is_empty() {
        return Err(
            "backlog item needs a headline AND a body — required shape: first line a short \
             headline (a few words, no trailing period), then a blank line, then the body \
             carrying the detail (problem, fix direction, tests)"
                .into(),
        );
    }
    // Detail bar (2027-01-07): a dispatched (possibly lesser-reasoning)
    // model cannot re-derive missing context — reject terse bodies with a
    // message that names the full requirement so the retry is one shot.
    let body_chars = body.chars().count();
    if body_chars < MIN_BODY_CHARS {
        return Err(format!(
            "backlog item body is {} chars (min {}) — too terse for a dispatched (possibly \
             lesser-reasoning) model that cannot re-derive context. Detail bar: \
             {DETAIL_BAR_REQUIREMENT}",
            body_chars, MIN_BODY_CHARS
        ));
    }
    if !body_carries_detail_marker(body) {
        return Err(format!(
            "backlog item body names no file path — a dispatched (possibly lesser-reasoning) \
             model needs exact pointers, not references it must guess. Detail bar: \
             {DETAIL_BAR_REQUIREMENT}; at minimum name one file path (or carry an explicit \
             'no-code research' marker for research items)"
        ));
    }
    Ok(text.to_string())
}

/// Validate the text of a NEW backlog item at a write path that can carry
/// image attachments (the IPC `backlog_add` command): image-only items —
/// empty/whitespace text WITH at least one image — pass with empty text
/// (the UI's add path explicitly supports pasting a screenshot with no
/// caption, and there is no headline to validate); anything else routes
/// through [`normalize_item_text`]. The agent-facing `backlog_add` tool
/// takes no images, so it calls [`normalize_item_text`] directly.
///
/// Returns the trimmed text on success (empty string for the image-only
/// exemption).
pub fn normalize_new_item_text(text: &str, images: &[String]) -> Result<String, String> {
    if text.trim().is_empty() && !images.is_empty() {
        return Ok(String::new());
    }
    normalize_item_text(text)
}

/// `true` when the bool is `false` — the `skip_serializing_if` helper for
/// [`BacklogItem::deferred`] (the field is omitted when unset, mirroring the
/// `Option::is_none` pattern of the other optional fields).
fn is_false(b: &bool) -> bool {
    !*b
}

/// A single backlog entry: a prompt (text + optional image attachments) plus
/// its dispatch status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacklogItem {
    /// Unique id — a UUIDv4 string. Random (not sequential) so two
    /// worktrees adding items concurrently can never collide: the union
    /// merge of their jsonl files just contains both lines.
    pub id: String,
    /// The prompt text.
    pub text: String,
    /// Image attachments. Stored as relative paths to gitignored sidecar
    /// files (`backlog-images/<id>/<i>.<ext>`); resolved back to base64 data
    /// URLs at the IPC boundary for the frontend + agent dispatch. May be
    /// empty. (Pre-upgrade JSONL carried inline base64 here — migrated on
    /// open.)
    pub images: Vec<String>,
    /// Current dispatch status.
    pub status: BacklogStatus,
    /// Creation time as seconds since the Unix epoch.
    pub created_at: u64,
    /// Optional note (e.g. the error message for a failed/cant-resolve item).
    pub note: Option<String>,
    /// Deferred (skip Run-All): the item stays in the backlog — visible,
    /// still pending work, manually dispatchable — but Run-All never
    /// selects or counts it (user request 2027-01-07: a "keep but do not
    /// auto-run" state for items reserved for manual in-app handling or
    /// later re-inclusion). ORTHOGONAL to [`BacklogStatus`]: the item
    /// keeps its status and every existing transition keeps working; only
    /// the Run-All selection path ([`BacklogStore::next_pending_eligible`])
    /// skips deferred items — auto-feed and the per-item dispatch button
    /// still see them. Serialized only when `true`: old JSONL lines without
    /// the field parse as `false`, and non-deferred lines stay
    /// byte-identical to the pre-change format (union-merge friendly).
    #[serde(default, skip_serializing_if = "is_false")]
    pub deferred: bool,
    /// The ROOT plan id of the plan dispatched for this item — the
    /// item↔plan linkage (backlog 45dcf577). Recorded when the workflow
    /// enters `Executing` (the `create_plan` moment) alongside the
    /// `InFlight` stamp; constant across sub-plan pushes/pops (it is the
    /// bottom of the plan stack). Status is derived from the plan
    /// lifecycle: the plan finishing → `Done`, the root plan being
    /// abandoned → `Failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    /// The plan's TITLE at dispatch — recorded alongside [`Self::plan_id`]
    /// (read from the plan file's `# Plan: <title>` heading at the
    /// `InFlight` stamp, backlog f45513b2) so the Backlog tab can show a
    /// human-friendly identifier that survives plan completion. `None` =
    /// not yet stamped (pre-change items) — the UI falls back to the short
    /// plan-id chip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_title: Option<String>,
    /// Soft-delete timestamp (seconds since the Unix epoch), set by
    /// [`BacklogStore::remove`]. A soft-deleted item stays in the JSONL —
    /// invisible to the UI, dispatch, and the memory indexer — until
    /// [`BacklogStore::open`] purges it [`PURGE_AFTER_SECS`] (30 days) later.
    /// Keeping the line (instead of dropping it) makes deletion survive the
    /// git union merge of concurrently-written backlog files, which would
    /// otherwise resurrect a hard-removed line from the other side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<u64>,
}

/// The dispatch status of a backlog item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BacklogStatus {
    /// Waiting to be dispatched.
    Pending,
    /// Dispatched to the main agent; turn in progress.
    InFlight,
    /// Resolved successfully.
    Done,
    /// The root plan was abandoned (backlog 45dcf577); the work stays in
    /// the tree.
    Failed,
    /// An explicit dead end named by the agent (`backlog_status`); the work
    /// stays in the tree.
    CantResolve,
}

impl BacklogStatus {
    /// The legal transition destinations FROM this status — the same rows
    /// [`BacklogStore::transition`] guards, exposed so error messages can
    /// name the allowed set for the item's current status instead of
    /// punting to the full table (an LLM parses a per-status list directly).
    pub fn allowed_destinations(self) -> &'static [BacklogStatus] {
        match self {
            BacklogStatus::Pending => &[
                BacklogStatus::InFlight,
                BacklogStatus::Done,
                BacklogStatus::Failed,
                BacklogStatus::CantResolve,
            ],
            BacklogStatus::InFlight => &[
                BacklogStatus::Done,
                BacklogStatus::Failed,
                BacklogStatus::CantResolve,
                BacklogStatus::Pending,
            ],
            BacklogStatus::Done | BacklogStatus::Failed | BacklogStatus::CantResolve => {
                &[BacklogStatus::Pending]
            }
        }
    }
}

/// Where a NEW backlog item lands in the store's display order — the
/// optional `position` parameter of the add paths (the `backlog_add`
/// agent tool and the IPC `backlog_add` command; backlog 06a31736).
///
/// `End` (the default) appends — the queue is FIFO, and every
/// pre-position caller keeps exactly today's behavior. `Top` inserts at
/// the FRONT so the item is dispatched first — for urgent/priority items
/// only (the motivating incident: the run-all stall item had to be
/// manually sent to top through the UI because the agent tool could only
/// append).
///
/// Union-merge safety (`.coding/backlog.jsonl` merges by line
/// concatenation, see `.gitattributes`): a `Top` insert PREPENDS one
/// line — a minimal diff that merges cleanly with a concurrent append (a
/// single line at the bottom — non-overlapping hunks). In the union
/// driver's worst case (overlapping rewrites concatenate both sides'
/// lines), [`parse_jsonl`] collapses same-id lines with the FIRST
/// occurrence winning, so no item is ever lost or duplicated — the same
/// guarantee the UI's send-to-top reorder already rides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BacklogPosition {
    /// Insert at the front — the item becomes the next dispatched pending
    /// item. Urgent/priority items only; the queue is otherwise FIFO.
    Top,
    /// Append at the end (the default — unchanged behavior for every
    /// existing caller).
    End,
}

impl BacklogPosition {
    /// The wire string for this position (`"top"` / `"end"`) — tool
    /// output, data payloads, and error messages.
    pub fn as_str(self) -> &'static str {
        match self {
            BacklogPosition::Top => "top",
            BacklogPosition::End => "end",
        }
    }
}

/// The persistent backlog store.
///
/// Holds the items in display order. All mutating methods persist
/// immediately; persistence failures are logged and otherwise ignored (the
/// in-memory state is never lost).
pub struct BacklogStore {
    items: Vec<BacklogItem>,
    path: PathBuf,
}

impl BacklogStore {
    /// Open the store at `path`, loading any previously persisted items.
    ///
    /// A missing file yields an empty store. A corrupt or unreadable file
    /// also yields an empty store (a warning is logged) — the backlog is UI
    /// state and must never prevent the app from starting.
    ///
    /// A legacy `.coding/backlog.json` (the pre-jsonl envelope format) is
    /// migrated: its items are loaded, the store is immediately re-persisted
    /// in the jsonl format at the new path, and the legacy file is left in
    /// place (git handles its removal — it may be tracked history).
    pub fn open(path: PathBuf) -> Self {
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let items = parse_jsonl(&content).unwrap_or_else(|e| {
                    eprintln!(
                        "warning: failed to parse backlog file {}: {e}; starting empty",
                        path.display()
                    );
                    Vec::new()
                });
                let mut store = Self { items, path };
                // Migrate any legacy inline data-URL images to gitignored
                // sidecar files (pre-upgrade JSONL carried base64 inline,
                // making diffs huge). Re-persists with file-path refs.
                if store.migrate_inline_images() {
                    store.persist();
                }
                // Startup cleanup: hard-remove items soft-deleted more than
                // PURGE_AFTER_SECS (30 days) ago.
                store.purge_expired();
                store
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Legacy migration: try the sibling `.json` envelope.
                if let Some(legacy) = path.parent().map(|p| p.join("backlog.json")) {
                    if let Ok(content) = std::fs::read_to_string(&legacy) {
                        if let Ok(file) = serde_json::from_str::<BacklogFile>(&content) {
                            eprintln!(
                                "info: migrating legacy backlog {} → {}",
                                legacy.display(),
                                path.display()
                            );
                            let items = file
                                .items
                                .into_iter()
                                .map(|i| BacklogItem {
                                    id: i.id.as_string(),
                                    text: i.text,
                                    images: i.images,
                                    status: i.status,
                                    created_at: i.created_at,
                                    note: i.note,
                                    deferred: false,
                                    plan_id: None,
                                    plan_title: None,
                                    deleted_at: None,
                                })
                                .collect();
                            let mut store = Self { items, path };
                            store.persist();
                            // Migrate any legacy inline data-URL images too
                            // (the legacy .json envelope could carry them).
                            if store.migrate_inline_images() {
                                store.persist();
                            }
                            store.purge_expired();
                            return store;
                        }
                    }
                }
                Self::empty(path)
            }
            Err(e) => {
                eprintln!(
                    "warning: failed to read backlog file {}: {e}; starting empty",
                    path.display()
                );
                Self::empty(path)
            }
        }
    }

    /// Hard-remove every soft-deleted item whose `deleted_at` is at least
    /// [`PURGE_AFTER_SECS`] (30 days) old: the JSONL line is dropped and the
    /// item's image sidecar files are deleted. Called from [`Self::open`] so
    /// every startup cleans up expired deletions. Persists (and logs) only
    /// when something was actually purged.
    fn purge_expired(&mut self) {
        let now = now_secs();
        let expired: Vec<String> = self
            .items
            .iter()
            .filter(|i| {
                matches!(i.deleted_at, Some(at) if now.saturating_sub(at) >= PURGE_AFTER_SECS)
            })
            .map(|i| i.id.clone())
            .collect();
        if expired.is_empty() {
            return;
        }
        for id in &expired {
            self.delete_item_images(id);
        }
        self.items.retain(|i| !expired.contains(&i.id));
        self.persist();
        eprintln!(
            "info: purged {} soft-deleted backlog item(s) older than {} days",
            expired.len(),
            PURGE_AFTER_SECS / 86_400
        );
    }

    /// Add a new pending item with the given text + images, landing at the
    /// given queue position. Returns the item as stored (with its
    /// allocated UUID id and timestamp).
    ///
    /// `BacklogPosition::Top` inserts at the FRONT of the display order
    /// (the item is dispatched first — urgent/priority items only, backlog
    /// 06a31736); `End` appends. See [`BacklogPosition`] for why a top
    /// insert is union-merge safe.
    ///
    /// `images` are base64 data URLs from the frontend. Each is extracted to a
    /// gitignored sidecar file (`.coding/backlog-images/<id>/<i>.<ext>`) and
    /// the item stores the relative PATH — not the data URL — so the
    /// git-tracked JSONL stays small (inline base64 made diffs huge). Strings
    /// that are not data URLs (test fixtures, opaque strings) pass through
    /// unchanged. Use [`resolve_images`](Self::resolve_images) to read the
    /// files back into data URLs for display / dispatch.
    pub fn add_at(
        &mut self,
        position: BacklogPosition,
        text: String,
        images: Vec<String>,
    ) -> BacklogItem {
        self.reload_from_disk();
        let id = uuid::Uuid::new_v4().to_string();
        let image_paths = self.write_image_files(&id, &images);
        let item = BacklogItem {
            id,
            text,
            images: image_paths,
            status: BacklogStatus::Pending,
            created_at: now_secs(),
            note: None,
            deferred: false,
            plan_id: None,
            plan_title: None,
            deleted_at: None,
        };
        match position {
            BacklogPosition::Top => self.items.insert(0, item.clone()),
            BacklogPosition::End => self.items.push(item.clone()),
        }
        self.persist();
        item
    }

    /// Add a new pending item with the given text + images, APPENDED at
    /// the end of the queue — the FIFO default every pre-position caller
    /// uses. Returns the item as stored (with its allocated UUID id and
    /// timestamp). See [`Self::add_at`] for the position-aware variant and
    /// the image-handling details.
    pub fn add(&mut self, text: String, images: Vec<String>) -> BacklogItem {
        self.add_at(BacklogPosition::End, text, images)
    }

    /// Soft-delete the item with the given id: the item is marked
    /// `deleted_at` and disappears from every read path ([`Self::items`],
    /// [`Self::next_pending`], dispatch, the memory indexer), but its JSONL
    /// line and image sidecar files stay on disk until [`Self::open`] purges
    /// it [`PURGE_AFTER_SECS`] (30 days) later. Keeping the line (instead of
    /// dropping it) makes the deletion survive the git union merge — a
    /// hard-removed line would be resurrected by the other side's copy.
    /// Returns `true` if a live item with this id existed; `false` for an
    /// unknown or already-deleted id.
    pub fn remove(&mut self, id: &str) -> bool {
        self.reload_from_disk();
        let Some(item) = self
            .items
            .iter_mut()
            .find(|i| i.id == id && i.deleted_at.is_none())
        else {
            return false;
        };
        item.deleted_at = Some(now_secs());
        self.persist();
        true
    }

    /// Resequence the items to the given id order.
    ///
    /// Ids in `ids` are placed first, in that order (unknown ids are
    /// skipped). Items whose ids are not listed keep their relative order
    /// and go last.
    pub fn reorder(&mut self, ids: &[String]) {
        self.reload_from_disk();
        let mut ordered: Vec<BacklogItem> = Vec::with_capacity(self.items.len());
        for id in ids {
            let already_placed = ordered.iter().any(|i| i.id == *id);
            if !already_placed {
                if let Some(pos) = self.items.iter().position(|i| i.id == *id) {
                    ordered.push(self.items[pos].clone());
                }
            }
        }
        for item in &self.items {
            if !ordered.iter().any(|i| i.id == item.id) {
                ordered.push(item.clone());
            }
        }
        self.items = ordered;
        self.persist();
    }

    /// Change an item's status through the shared guarded transition table.
    ///
    /// The single sanctioned status-mutation path for production code (the
    /// `backlog_status` agent tool AND the harness's dispatch/resolution
    /// sites in `run_all.rs` / `backlog_cmds.rs`). Raw [`Self::set_status`]
    /// remains for tests and internal seed states, but production transitions
    /// must route here so the guard can never drift.
    ///
    /// Status is tied to the plan lifecycle (backlog 45dcf577): `InFlight` ⇔
    /// a plan dispatched for the item is active; `Done` ⇔ the plan reached
    /// `Complete`; `Failed` ⇔ the plan was abandoned. The table (from → to):
    /// - `Pending → InFlight` — execution entry (the forwarder stamps the
    ///   in-flight item when the workflow enters `Executing` — the
    ///   `create_plan` moment, which also records the item↔plan linkage;
    ///   dispatch itself leaves the item `Pending`).
    /// - `Pending → Done / Failed / CantResolve` — resolved before ever being
    ///   dispatched (agent marking; the harness no longer stamps here — a
    ///   checkpoint failure leaves the item `Pending`).
    /// - `InFlight → Done / Failed / CantResolve` — turn resolution: `Done`
    ///   via the plan-loop gate (the plan finished), `Failed` on root-plan
    ///   abandonment, `CantResolve` only as the agent's explicit dead end.
    /// - `InFlight → Pending` — intervention requeue / deferral (the item
    ///   re-waits, checkpoint sha preserved in the note).
    /// - `Done / Failed / CantResolve → Pending` — requeue (the user's retry).
    ///
    /// Anything else (including a same-status transition or an unknown id) is
    /// refused: returns `false`, no mutation, no persist.
    pub fn transition(&mut self, id: &str, to: BacklogStatus, note: Option<String>) -> bool {
        self.reload_from_disk();
        let Some(item) = self.items.iter_mut().find(|i| i.id == id && is_live(i)) else {
            return false;
        };
        let legal = match (item.status, to) {
            (BacklogStatus::Pending, BacklogStatus::InFlight) => true,
            (BacklogStatus::Pending, BacklogStatus::Done)
            | (BacklogStatus::Pending, BacklogStatus::Failed)
            | (BacklogStatus::Pending, BacklogStatus::CantResolve) => true,
            (BacklogStatus::InFlight, BacklogStatus::Done)
            | (BacklogStatus::InFlight, BacklogStatus::Failed)
            | (BacklogStatus::InFlight, BacklogStatus::CantResolve)
            | (BacklogStatus::InFlight, BacklogStatus::Pending) => true,
            (
                BacklogStatus::Done | BacklogStatus::Failed | BacklogStatus::CantResolve,
                BacklogStatus::Pending,
            ) => true,
            _ => false,
        };
        if !legal {
            return false;
        }
        item.status = to;
        item.note = note;
        self.persist();
        true
    }

    /// Append an informational note to an item WITHOUT changing its status.
    ///
    /// Used by harness paths that must record context (a checkpoint sha, a
    /// halt reason) against an item while leaving its queue state untouched —
    /// e.g. a steer halting an unattended run must record the sha + reason on
    /// a still-`Pending` item so a later re-dispatch can pick it up, but the
    /// item itself stays queued (status changes are [`Self::transition`]'s
    /// job). The addition is appended to any existing note as
    /// `" | <addition>"` (or set as the note when there is none); repeated
    /// annotations accumulate. `extract_checkpoint_sha`-style head parsing
    /// keeps working because additions are appended after any leading sha.
    /// Returns `false` (no-op, no persist) for an unknown id.
    pub fn annotate(&mut self, id: &str, addition: &str) -> bool {
        self.reload_from_disk();
        let Some(item) = self.items.iter_mut().find(|i| i.id == id && is_live(i)) else {
            return false;
        };
        item.note = match item.note.as_deref() {
            Some(existing) => Some(format!("{existing} | {addition}")),
            None => Some(addition.to_string()),
        };
        self.persist();
        true
    }

    /// Replace an item's note WITHOUT touching its status — the dispatch-time
    /// counterpart of [`Self::annotate`]: a fresh checkpoint sha must REPLACE
    /// a stale note from a previous run (appending would leave the OLD run's
    /// sha at the note's head, breaking checkpoint-sha head-parsing).
    /// Returns `false` (no-op, no persist) for an unknown id.
    pub fn set_note(&mut self, id: &str, note: Option<String>) -> bool {
        self.reload_from_disk();
        let Some(item) = self.items.iter_mut().find(|i| i.id == id && is_live(i)) else {
            return false;
        };
        item.note = note;
        self.persist();
        true
    }

    /// Record the item↔plan linkage (backlog 45dcf577): set the ROOT plan id
    /// of the plan dispatched for this item — and its title (read from the
    /// plan file's `# Plan: <title>` heading, so the Backlog tab can show a
    /// human-friendly identifier that survives plan completion, backlog
    /// f45513b2) — WITHOUT touching its status. Called when the workflow
    /// enters `Executing` (the `create_plan` moment) alongside the
    /// `InFlight` stamp — and again whenever a fresh plan replaces an
    /// abandoned one mid-dispatch, so the linkage always names the plan the
    /// item's status is derived from (finish → `Done`, root abandon →
    /// `Failed`). Returns `false` (no-op, no persist) for an unknown id.
    pub fn set_plan_id(
        &mut self,
        id: &str,
        plan_id: Option<&str>,
        plan_title: Option<&str>,
    ) -> bool {
        self.reload_from_disk();
        let Some(item) = self.items.iter_mut().find(|i| i.id == id && is_live(i)) else {
            return false;
        };
        item.plan_id = plan_id.map(|p| p.to_string());
        item.plan_title = plan_title.map(|p| p.to_string());
        self.persist();
        true
    }

    /// Set the status (and optional note) of the item with the given id.
    /// No-op if the id doesn't exist.
    ///
    /// NOTE: raw state accessor — production callers must use
    /// [`Self::transition`] (the guarded table); this exists for tests and
    /// internal state construction.
    pub fn set_status(&mut self, id: &str, status: BacklogStatus, note: Option<String>) {
        self.reload_from_disk();
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            item.status = status;
            item.note = note;
            self.persist();
        }
    }

    /// Re-queue a finished item back to `Pending` (the user's "retry" /
    /// "reset" action: retry a failed/cant-resolve item, or reset an item the
    /// agent marked `Done` that the user disagrees with). Only TERMINAL
    /// statuses may be re-queued — a `Pending` or `InFlight` item is refused
    /// (`false`, no-op) so a stale UI click can never double-queue an item
    /// that is already queued or actively being worked on. The note is
    /// cleared. Re-queuing never touches git history: the item's earlier
    /// commits (if any) stay; re-dispatch builds on top of them.
    ///
    /// This guard is deliberately STRICTER than [`Self::transition`]'s
    /// `InFlight → Pending` row (which the dispatch-deferral harness path
    /// needs): `requeue` is the user-facing retry action and must never
    /// appear to "un-dispatch" an item that is actively being worked on.
    pub fn requeue(&mut self, id: &str) -> bool {
        self.reload_from_disk();
        let Some(item) = self.items.iter_mut().find(|i| i.id == id && is_live(i)) else {
            return false;
        };
        if !matches!(
            item.status,
            BacklogStatus::Failed | BacklogStatus::CantResolve | BacklogStatus::Done
        ) {
            return false;
        }
        item.status = BacklogStatus::Pending;
        item.note = None;
        self.persist();
        true
    }

    /// Edit the text + images of the item with the given id. The status and
    /// other fields are preserved. Returns `true` if the item existed (and was
    /// updated + persisted), `false` otherwise (no-op).
    ///
    /// `images` are base64 data URLs from the frontend. The item's OLD image
    /// sidecar files are deleted first (so an edit that removes/replaces
    /// images doesn't leak orphaned files), then the new set is written.
    pub fn edit(&mut self, id: &str, text: String, images: Vec<String>) -> bool {
        self.reload_from_disk();
        // Check existence first (shared borrow ends), then do the file I/O
        // (delete old + write new) before taking the mutable borrow on the
        // item — delete_item_images / write_image_files take &self.
        if !self.items.iter().any(|i| i.id == id && is_live(i)) {
            return false;
        }
        self.delete_item_images(id);
        let image_paths = self.write_image_files(id, &images);
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            item.text = text;
            item.images = image_paths;
            self.persist();
            true
        } else {
            false
        }
    }

    /// Return the first live `Pending` item in display order, if any.
    /// Soft-deleted items are never dispatched.
    pub fn next_pending(&self) -> Option<BacklogItem> {
        self.items
            .iter()
            .find(|i| i.status == BacklogStatus::Pending && is_live(i))
            .cloned()
    }

    /// Return the first live `Pending` item that is NOT deferred — the
    /// Run-All selection (user request 2027-01-07: exclude items from
    /// Run-All). Deferred items ([`BacklogItem::deferred`]) stay pending
    /// and visible but are never selected or counted by a run; when only
    /// deferred items remain, this returns `None` and the run ends
    /// cleanly. [`Self::next_pending`] (auto-feed, the dispatch-next
    /// button) and [`Self::pending_item`] (the per-item ▶ button — manual
    /// in-app handling of a deferred item) still see them.
    pub fn next_pending_eligible(&self) -> Option<BacklogItem> {
        self.items
            .iter()
            .find(|i| i.status == BacklogStatus::Pending && is_live(i) && !i.deferred)
            .cloned()
    }

    /// Return the first live `Pending` non-deferred item whose id is NOT
    /// in `exclude` — the PARALLEL run-all selection (plan ffd7a86f).
    ///
    /// [`Self::next_pending_eligible`] peeks without consuming (items
    /// stay `Pending` until the workflow enters `Executing`), so the
    /// parallel dispatch window must explicitly skip the ids already
    /// handed out: the main agent's `current_item` and every spawned
    /// run's item. Without the exclusion the window would hand the SAME
    /// top item to a spawned agent while the main agent works it.
    pub fn next_pending_eligible_excluding(
        &self,
        exclude: &[String],
    ) -> Option<BacklogItem> {
        self.items
            .iter()
            .find(|i| {
                i.status == BacklogStatus::Pending
                    && is_live(i)
                    && !i.deferred
                    && !exclude.contains(&i.id)
            })
            .cloned()
    }

    /// Set or clear the deferred (skip Run-All) flag on the item with the
    /// given id — the "keep but do not auto-run" state. ORTHOGONAL to
    /// status: the item keeps its current status (typically `Pending`)
    /// and every existing transition keeps working; only Run-All selection
    /// ([`Self::next_pending_eligible`]) skips deferred items. Returns
    /// `false` (no-op, no persist) for an unknown or soft-deleted id.
    pub fn set_deferred(&mut self, id: &str, deferred: bool) -> bool {
        self.reload_from_disk();
        let Some(item) = self.items.iter_mut().find(|i| i.id == id && is_live(i)) else {
            return false;
        };
        item.deferred = deferred;
        self.persist();
        true
    }

    /// Return the item with the given id, but only if it is `Pending`.
    ///
    /// Used by the per-item dispatch path (the ▶ button on a specific card):
    /// unlike `next_pending`, this targets an exact id and refuses non-pending
    /// items so a stale click on an already-dispatched card is a no-op.
    pub fn pending_item(&self, id: &str) -> Option<BacklogItem> {
        self.items
            .iter()
            .find(|i| i.id == id && i.status == BacklogStatus::Pending && is_live(i))
            .cloned()
    }

    /// Soft-delete all finished items (`Done`, `Failed`, `CantResolve`),
    /// keeping `Pending` and `InFlight` ones. Like [`Self::remove`], the
    /// finished items stay in the JSONL (invisible to every read path) until
    /// the startup purge hard-removes them — a hard drop here would be
    /// resurrected by the git union merge from another worktree's copy of
    /// the file. Image sidecar files are likewise kept until the purge.
    pub fn clear_finished(&mut self) {
        self.reload_from_disk();
        let now = now_secs();
        let mut changed = false;
        for item in self.items.iter_mut() {
            if matches!(
                item.status,
                BacklogStatus::Done | BacklogStatus::Failed | BacklogStatus::CantResolve
            ) && is_live(item)
            {
                item.deleted_at = Some(now);
                changed = true;
            }
        }
        if changed {
            self.persist();
        }
    }

    /// All live (non-soft-deleted) items, in display order. Soft-deleted
    /// items ([`Self::remove`], [`Self::clear_finished`]) are excluded — they
    /// stay on disk until the startup purge but are invisible to the UI,
    /// dispatch, and every other consumer of this accessor. Internal paths
    /// that must see the full list (persist, reload, purge) read `self.items`
    /// directly.
    pub fn items(&self) -> Vec<BacklogItem> {
        self.items.iter().filter(|i| is_live(i)).cloned().collect()
    }

    /// An empty store over the given path.
    fn empty(path: PathBuf) -> Self {
        Self {
            items: Vec::new(),
            path,
        }
    }

    /// Re-read the items from disk, replacing the in-memory vector.
    ///
    /// Called at the start of every mutating method so the mutation is applied
    /// to the latest disk state — a read-modify-write that prevents a stale
    /// in-memory state from clobbering external changes (git merge/checkout/
    /// restore that modified the file outside the store). Safe because every
    /// mutation persists immediately: the in-memory state always equals the
    /// last disk write, so reloading only picks up post-write external changes.
    ///
    /// Graceful on error (matching [`Self::open`]): a missing or corrupt file
    /// leaves the in-memory state untouched (the items are never lost to an IO
    /// hiccup).
    fn reload_from_disk(&mut self) {
        let Ok(content) = std::fs::read_to_string(&self.path) else {
            return; // missing/unreadable — keep in-memory state
        };
        match parse_jsonl(&content) {
            Ok(items) => self.items = items,
            Err(e) => {
                eprintln!(
                    "warning: failed to parse backlog file during reload {}: {e}; keeping in-memory state",
                    self.path.display()
                );
            }
        }
    }

    /// The directory holding image sidecar files — a sibling of the JSONL
    /// (`backlog-images/` under the same parent). Gitignored so the binary
    /// image data never enters git (inline base64 in the JSONL made diffs
    /// huge; the sidecar files keep the tracked JSONL small).
    fn image_dir(&self) -> PathBuf {
        self.path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("backlog-images")
    }

    /// Write each data-URL image attachment to a gitignored sidecar file and
    /// return the relative paths to store in the JSONL. Strings that are NOT
    /// data URLs (test fixtures, opaque strings) pass through unchanged so
    /// non-image-attachment usage is unaffected. Invalid base64 or IO errors
    /// degrade gracefully (the original string is kept) — a backlog add must
    /// never fail over an image-file write.
    ///
    /// Defense-in-depth: an `item_id` containing path separators or `..` is
    /// rejected (the image stays inline) so a tampered JSONL can't write
    /// outside `.coding/backlog-images/`. Production ids are UUIDs (always
    /// safe); this guards the union-merge trust boundary.
    fn write_image_files(&self, item_id: &str, images: &[String]) -> Vec<String> {
        if !is_safe_item_id(item_id) {
            // Unsafe id — keep all images inline (don't write files).
            return images.to_vec();
        }
        let item_dir = self.image_dir().join(item_id);
        images
            .iter()
            .enumerate()
            .map(|(i, img)| {
                // Only extract genuine data URLs to files; everything else
                // (plain strings, test fixtures) passes through as-is.
                let Some((mime, b64)) = parse_data_url(img) else {
                    return img.clone();
                };
                let ext = mime_to_ext(&mime);
                let bytes = match base64::engine::general_purpose::STANDARD.decode(b64) {
                    Ok(b) => b,
                    Err(_) => return img.clone(), // invalid base64 — keep as-is
                };
                let file = item_dir.join(format!("{i}.{ext}"));
                if let Err(e) = std::fs::create_dir_all(&item_dir) {
                    eprintln!(
                        "warning: failed to create backlog image dir {}: {e}",
                        item_dir.display()
                    );
                    return img.clone();
                }
                if let Err(e) = std::fs::write(&file, &bytes) {
                    eprintln!(
                        "warning: failed to write backlog image {}: {e}",
                        file.display()
                    );
                    return img.clone();
                }
                format!("backlog-images/{item_id}/{i}.{ext}")
            })
            .collect()
    }

    /// Resolve an item's stored image PATHS back into base64 data URLs (for
    /// the frontend to render and the agent dispatch to consume). Legacy
    /// inline data URLs (strings starting with `data:`) pass through
    /// unchanged. Missing files (cross-worktree, manually deleted) are
    /// skipped gracefully — the caller just sees fewer thumbnails, never a
    /// panic.
    pub fn resolve_images(&self, item: &BacklogItem) -> Vec<String> {
        let parent = self
            .path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let image_root = parent.join("backlog-images");
        item.images
            .iter()
            .filter_map(|p| {
                // Legacy inline data URL (or a test fixture) — return as-is.
                if p.starts_with("data:") {
                    return Some(p.clone());
                }
                // A sidecar file path — read it back into a data URL.
                let file = parent.join(p);
                // Defense-in-depth: verify the resolved path stays under the
                // image dir (a tampered JSONL with `..` could otherwise read
                // an arbitrary file + exfiltrate it as a data URL). Skip
                // (graceful, like a missing file) if it escapes.
                //
                // `Path::starts_with` compares components WITHOUT resolving
                // `..`, so `backlog-images/../../etc/passwd` would match
                // `image_root` and escape. Reject any `ParentDir` component
                // explicitly — a legitimate sidecar path is always
                // `backlog-images/<id>/<i>.<ext>` (no `..`).
                if file
                    .components()
                    .any(|c| c == std::path::Component::ParentDir)
                {
                    return None;
                }
                if !file.starts_with(&image_root) {
                    return None;
                }
                let bytes = std::fs::read(&file).ok()?;
                let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
                let mime = ext_to_mime(ext);
                Some(crate::tool::agent::image_tools::bytes_to_data_url(
                    mime, &bytes,
                ))
            })
            .collect()
    }

    /// Best-effort delete of an item's image sidecar directory. Never fails a
    /// mutation over cleanup — a missing dir is a silent no-op.
    ///
    /// Defense-in-depth: an unsafe `item_id` (path separators / `..`) is a
    /// silent no-op so a tampered JSONL can't remove files outside the image
    /// dir.
    fn delete_item_images(&self, item_id: &str) {
        if !is_safe_item_id(item_id) {
            return;
        }
        let dir = self.image_dir().join(item_id);
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "warning: failed to remove backlog image dir {}: {e}",
                    dir.display()
                );
            }
        }
    }

    /// Migrate legacy inline data-URL images (pre-upgrade JSONL carried base64
    /// inline) to gitignored sidecar files, replacing the inline strings with
    /// file-path refs. Returns `true` if any item was migrated (the caller
    /// re-persists so the JSONL drops the inline data).
    fn migrate_inline_images(&mut self) -> bool {
        // Collect first (shared borrow) so the write_image_files calls (which
        // take &self) don't conflict with a live &mut self.items borrow.
        let to_migrate: Vec<(String, Vec<String>)> = self
            .items
            .iter()
            .filter(|i| i.images.iter().any(|p| p.starts_with("data:")))
            .map(|i| (i.id.clone(), i.images.clone()))
            .collect();
        if to_migrate.is_empty() {
            return false;
        }
        for (id, imgs) in to_migrate {
            let new_paths = self.write_image_files(&id, &imgs);
            if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
                item.images = new_paths;
            }
        }
        true
    }

    /// Write the store to disk atomically: serialize to `path.tmp` (well,
    /// `path` with its extension swapped for `tmp`), then rename over
    /// `path`. Errors are logged and ignored — the in-memory state is the
    /// source of truth and must never be lost to an IO hiccup.
    ///
    /// Format: jsonl — one JSON item per line, in display order. Line-oriented
    /// so a git UNION merge of two concurrently-written files concatenates
    /// both sides' lines (the UUID ids make the union collision-free).
    fn persist(&self) {
        let lines = self
            .items
            .iter()
            .map(|i| serde_json::to_string(i))
            .collect::<Result<Vec<String>, _>>();
        let content = match lines {
            Ok(l) => l.join("\n") + "\n",
            Err(e) => {
                eprintln!("warning: failed to serialize backlog: {e}");
                return;
            }
        };
        if let Some(parent) = self.path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "warning: failed to create backlog dir {}: {e}",
                    parent.display()
                );
                return;
            }
        }
        let tmp = self.path.with_extension("tmp");
        if let Err(e) = std::fs::write(&tmp, content) {
            eprintln!(
                "warning: failed to write backlog tmp {}: {e}",
                tmp.display()
            );
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, &self.path) {
            eprintln!(
                "warning: failed to rename backlog tmp over {}: {e}",
                self.path.display()
            );
        }
    }
}

/// The legacy on-disk representation (the pre-jsonl envelope format), read
/// only for migration from `.coding/backlog.json`.
///
/// Legacy production files carried MONOTONIC `u64` ids (`"id": 1`); the
/// migration accepts both that shape and string ids (a mixed file — e.g. an
/// already-upgraded install re-saved by an older build — loads too), so a
/// real pre-upgrade backlog never silently starts empty.
#[derive(Debug, Serialize, Deserialize)]
struct BacklogFile {
    items: Vec<LegacyItem>,
    #[serde(default)]
    next_id: u64,
}

/// One legacy item: same fields as [`BacklogItem`], but the id may be a
/// number OR a string (`#[serde(untagged)]`).
#[derive(Debug, Serialize, Deserialize)]
struct LegacyItem {
    id: LegacyId,
    text: String,
    #[serde(default)]
    images: Vec<String>,
    status: BacklogStatus,
    #[serde(default)]
    created_at: u64,
    #[serde(default)]
    note: Option<String>,
}

/// A legacy item id: the old monotonic `u64`, or the newer UUID string.
/// Number ids stringify on migration (the id stays stable; it is only
/// required to be unique within the file, and u64s are).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum LegacyId {
    /// The pre-jsonl numeric id.
    Num(u64),
    /// A string id (already-migrated install re-saved by an old build).
    Str(String),
}

impl LegacyId {
    fn as_string(&self) -> String {
        match self {
            LegacyId::Num(n) => n.to_string(),
            LegacyId::Str(s) => s.clone(),
        }
    }
}

/// Parse the jsonl backlog format: one JSON item per line. Blank lines are
/// skipped; a malformed line fails the whole parse (the caller falls back to
/// an empty store with a warning — the backlog is UI state and must never
/// panic over a corrupt file).
///
/// Same-id lines are collapsed into one entry: the git union merge driver
/// concatenates both sides' lines, so a merged file can carry the same id
/// twice (e.g. one worktree soft-deleted the item while another still had it
/// live). Deletion is sticky — if ANY line for an id carries `deleted_at`,
/// the item is deleted (with the latest such timestamp, so retention is
/// never shortened); otherwise the first line wins.
fn parse_jsonl(content: &str) -> Result<Vec<BacklogItem>, String> {
    let mut items: Vec<BacklogItem> = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let item: BacklogItem =
            serde_json::from_str(line).map_err(|e| format!("line {}: {e}", idx + 1))?;
        if let Some(existing) = items.iter_mut().find(|i| i.id == item.id) {
            if let Some(d) = item.deleted_at {
                existing.deleted_at = Some(existing.deleted_at.map_or(d, |a| a.max(d)));
            }
        } else {
            items.push(item);
        }
    }
    Ok(items)
}

/// Seconds since the Unix epoch (0 if the clock is somehow before it).
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Parse a `data:<media_type>;base64,<data>` URL into `(media_type, data)`.
/// Returns `None` when the string is not a base64 data URL (so non-data-URL
/// strings — test fixtures, opaque paths — pass through unchanged).
///
/// INTENTIONALLY DIVERGENT from the provider-shared `parse_data_url`
/// (src/provider/sse_util.rs, quality review backlog 4047c82f): an empty
/// media type stays empty here — it feeds [`mime_to_ext`]'s default `bin`
/// extension, a file-writing concern, whereas the provider contract
/// defaults to `image/png` for API payload media types. Do not "unify"
/// these: the behaviors serve different callers.
fn parse_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let media_type = meta.strip_suffix(";base64")?;
    Some((media_type.to_string(), data.to_string()))
}

/// Map a MIME media type to a file extension (default `bin`).
fn mime_to_ext(mime: &str) -> &str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "bin",
    }
}

/// Map a file extension to a MIME media type (default `application/octet-stream`).
fn ext_to_mime(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

/// Whether an item id is safe to use as a directory name under
/// `backlog-images/` — it must not contain path separators or `..` so a
/// tampered JSONL (the union-merge driver concatenates lines from both sides)
/// can't write/remove files outside the image dir. Production ids are UUIDs
/// (always safe); this is defense-in-depth for the trust boundary.
fn is_safe_item_id(id: &str) -> bool {
    !id.is_empty()
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains("..")
        && id != "."
        && id != ".."
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp dir per test, removed on drop.
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "mnemo-backlog-test-{}-{}-{}",
                name,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self) -> PathBuf {
            self.0.join("backlog.jsonl")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // ---- normalize_item_text (the headline+body shape contract) ----

    /// A body that passes the detail bar (2027-01-07): ≥ [`MIN_BODY_CHARS`]
    /// and carries a file path. Shared by the shape tests so they keep
    /// testing THEIR rule (headline/trim/blank-line) on a compliant body.
    fn detailed_body() -> String {
        let body = "Problem: the login loop re-prompts after auth (2027-01-07); repro: log \
                    in twice. Fix direction: clear the session flag in src/auth/session.rs \
                    before redirecting. Acceptance: cargo test login_loop. Related: review \
                    568d405.";
        assert!(body.chars().count() >= MIN_BODY_CHARS);
        body.to_string()
    }

    #[test]
    fn normalize_accepts_headline_blank_line_body_and_trims() {
        let body = detailed_body();
        let ok = normalize_item_text(&format!("Fix the login loop\n\n{body}")).unwrap();
        assert_eq!(ok, format!("Fix the login loop\n\n{body}"));
        // Surrounding whitespace is trimmed off.
        let trimmed = normalize_item_text(&format!("  Fix the login loop\n\n{body}  ")).unwrap();
        assert_eq!(trimmed, format!("Fix the login loop\n\n{body}"));
    }

    #[test]
    fn normalize_accepts_body_on_the_next_line_without_blank_line() {
        // Structural check: content after the first line = body. The blank
        // line is the recommended form, not a hard requirement — this shape
        // renders identically (first line vs rest).
        let body = detailed_body();
        let ok = normalize_item_text(&format!("Fix the login loop\n{body}")).unwrap();
        assert_eq!(ok, format!("Fix the login loop\n{body}"));
    }

    #[test]
    fn normalize_rejects_empty_and_whitespace() {
        assert!(normalize_item_text("").is_err());
        assert!(normalize_item_text("   \n  \n ").is_err());
    }

    #[test]
    fn normalize_rejects_single_line_headline_only() {
        // A single-line item renders as a bare headline with no body —
        // rejected with an error naming the required shape.
        let err = normalize_item_text("Fix the login loop").unwrap_err();
        assert!(err.contains("headline AND a body"), "error: {err}");
        assert!(err.contains("blank line"), "error: {err}");
    }

    #[test]
    fn normalize_rejects_headline_only_with_trailing_blank_lines() {
        // "Headline\n\n" trims to a single line — still no body.
        assert!(normalize_item_text("Headline\n\n").is_err());
    }

    #[test]
    fn normalize_rejects_overlong_headline() {
        let long = "x".repeat(MAX_HEADLINE_CHARS + 1);
        let err = normalize_item_text(&format!("{long}\n\nBody here.")).unwrap_err();
        assert!(err.contains("101"), "error: {err}");
        assert!(err.contains("max 100"), "error: {err}");
        // Exactly at the limit is fine.
        let edge = "x".repeat(MAX_HEADLINE_CHARS);
        assert!(normalize_item_text(&format!("{edge}\n\n{}", detailed_body())).is_ok());
    }

    #[test]
    fn normalize_rejects_oversize_text_and_accepts_the_boundary() {
        let headline = "Task";
        // The boundary body must still pass the detail bar (a path marker) —
        // pad after the marker so the total is exactly MAX_ITEM_TEXT_CHARS.
        let marker = "src/foo.rs ";
        let body = format!(
            "{marker}{}",
            "y".repeat(
                MAX_ITEM_TEXT_CHARS - headline.chars().count() - 2 - marker.chars().count()
            )
        );
        let exact = format!("{headline}\n\n{body}");
        assert_eq!(exact.chars().count(), MAX_ITEM_TEXT_CHARS);
        assert!(normalize_item_text(&exact).is_ok());
        let over = format!("{exact}y");
        let err = normalize_item_text(&over).unwrap_err();
        assert!(err.contains("max 4000"), "error: {err}");
    }

    // ---- the detail bar (2027-01-07): terse bodies are rejected so a
    // dispatched (possibly lesser-reasoning) model cannot be left
    // floundering on a pointer-only item ----

    #[test]
    fn normalize_rejects_terse_body_below_the_char_floor() {
        // A body with a file path but far below MIN_BODY_CHARS — the floor
        // catches one-liner bodies that name a file yet carry no problem,
        // fix direction, or acceptance.
        let err = normalize_item_text("Fix the login loop\n\nFix src/auth/session.rs.").unwrap_err();
        assert!(err.contains("min 160"), "error: {err}");
        assert!(err.contains("Detail bar"), "error: {err}");
        // The message names the five elements so an LLM retries in one shot.
        assert!(err.contains("how to reproduce"), "error: {err}");
        assert!(err.contains("acceptance criteria"), "error: {err}");
    }

    #[test]
    fn normalize_rejects_body_without_a_file_path() {
        // Long enough but names no path and carries no marker — a lesser
        // model would have to guess where to look.
        let body = "z".repeat(MIN_BODY_CHARS + 40);
        let err = normalize_item_text(&format!("Fix the login loop\n\n{body}")).unwrap_err();
        assert!(err.contains("names no file path"), "error: {err}");
        assert!(err.contains("no-code research"), "error: {err}");
    }

    #[test]
    fn normalize_accepts_no_code_research_marker() {
        // The explicit escape hatch: research items carry no paths.
        let body = format!(
            "Research the best approach for the export pipeline (2027-01-07): compare \
             streaming vs buffered writes, note the trade-offs, recommend one. \
             no-code research — findings only, no code changes."
        );
        assert!(body.chars().count() >= MIN_BODY_CHARS);
        assert!(normalize_item_text(&format!("Research export options\n\n{body}")).is_ok());
    }

    #[test]
    fn detail_marker_recognizes_paths_but_not_prose() {
        // Path-like tokens count…
        assert!(body_carries_detail_marker("fix src/auth/session.rs first"));
        assert!(body_carries_detail_marker("see frontend/src/lib/tauri.ts:373"));
        assert!(body_carries_detail_marker("in C:\\repo\\src\\main.rs"));
        assert!(body_carries_detail_marker("update README.md"));
        assert!(body_carries_detail_marker("check Cargo.toml deps"));
        assert!(body_carries_detail_marker("docs at https://example.com/guide"));
        // …but prose with a lone separator does not ("and/or" is not a path).
        assert!(!body_carries_detail_marker("fix the and/or branch in the parser"));
        assert!(!body_carries_detail_marker("no paths here at all, just words"));
        // Slash-dates are NOT paths (review LOW-2): the two-separator
        // alternative requires an alphabetic character in the token —
        // "10/10/2027" is digits and separators only.
        assert!(!body_carries_detail_marker("seen 10/10/2027 and again 01/07/27"));
        // The no-code markers count (case-insensitive).
        assert!(body_carries_detail_marker("No-Code Research: findings only"));
        assert!(body_carries_detail_marker("this is research only, no code changes"));
    }

    // ---- normalize_new_item_text (the image-aware new-item contract) ----

    // Regression (backlog 45a4eb88, surfaced while wiring the UI's error
    // display): the Backlog tab's add path explicitly supports image-only
    // items (a pasted screenshot with no caption — "Image-only items stay
    // valid"), but routing the IPC command through the bare
    // normalize_item_text silently rejected them ("text must not be
    // empty"). The image-aware validator exempts them — there is no
    // headline to validate.
    #[test]
    fn new_item_image_only_passes_with_empty_text() {
        let images = vec!["data:image/png;base64,x".to_string()];
        assert_eq!(normalize_new_item_text("", &images).unwrap(), "");
        assert_eq!(normalize_new_item_text("   \n  ", &images).unwrap(), "");
    }

    #[test]
    fn new_item_text_with_images_still_validates_the_text() {
        let images = vec!["data:image/png;base64,x".to_string()];
        // Shape-less text is rejected even with images attached.
        let err = normalize_new_item_text("Just a headline", &images).unwrap_err();
        assert!(err.contains("headline AND a body"), "error: {err}");
        // Shape-valid text + images passes, trimmed.
        let body = detailed_body();
        let ok =
            normalize_new_item_text(&format!("  Fix the login loop\n\n{body}  "), &images).unwrap();
        assert_eq!(ok, format!("Fix the login loop\n\n{body}"));
    }

    #[test]
    fn new_item_empty_text_without_images_still_rejects() {
        // The exemption needs images — empty text alone still delegates to
        // normalize_item_text and rejects.
        let err = normalize_new_item_text("", &[]).unwrap_err();
        assert!(err.contains("empty"), "error: {err}");
    }

    #[test]
    fn add_allocates_ids_and_persists() {
        let dir = TestDir::new("add");
        let mut store = BacklogStore::open(dir.path());

        let a = store.add("first".into(), vec![]);
        let b = store.add("second".into(), vec!["data:image/png;base64,x".into()]);

        assert_ne!(a.id, b.id, "UUID ids are unique");
        assert!(!a.id.is_empty());
        assert_eq!(a.status, BacklogStatus::Pending);
        assert!(a.note.is_none());
        assert!(b.created_at >= a.created_at);
        assert_eq!(store.items().len(), 2);
        assert!(dir.path().exists(), "add should persist to disk");
        // Atomic write must not leave the tmp file behind.
        assert!(!dir.path().with_extension("tmp").exists());
        // jsonl format: one line per item.
        let raw = std::fs::read_to_string(dir.path()).unwrap();
        assert_eq!(raw.lines().count(), 2, "one JSON line per item");
    }

    #[test]
    fn add_at_top_lands_first_in_store_and_file_order() {
        // Backlog 06a31736: a Top insert must land at the FRONT of the
        // display order — the item is dispatched first (next_pending) —
        // and the persisted line order must match, so the Backlog tab and
        // a reopened store agree.
        let dir = TestDir::new("add_top");
        let mut store = BacklogStore::open(dir.path());
        let a = store.add("first".into(), vec![]);
        let b = store.add("second".into(), vec![]);
        let t = store.add_at(BacklogPosition::Top, "urgent".into(), vec![]);

        let items = store.items();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].id, t.id, "the Top insert is first");
        assert_eq!(items[1].id, a.id);
        assert_eq!(items[2].id, b.id);
        assert_eq!(
            store.next_pending().map(|i| i.id),
            Some(t.id.clone()),
            "the Top insert is dispatched first"
        );

        // The persisted line order matches (reopen from disk).
        let reopened = BacklogStore::open(dir.path());
        assert_eq!(reopened.items().len(), 3);
        assert_eq!(reopened.items()[0].id, t.id);
    }

    #[test]
    fn add_at_end_appends_exactly_as_add() {
        // The default/absent case: End appends — identical to the
        // pre-position `add` every existing caller relies on.
        let dir = TestDir::new("add_end");
        let mut store = BacklogStore::open(dir.path());
        let a = store.add("first".into(), vec![]);
        let e = store.add_at(BacklogPosition::End, "second".into(), vec![]);

        let items = store.items();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, a.id);
        assert_eq!(items[1].id, e.id);
    }

    #[test]
    fn union_merge_of_top_insert_and_concurrent_append_parses_cleanly() {
        // Backlog 06a31736's merge-safety NOTE: a Top insert rewrites line
        // order (the new line is PREPENDED), so it must still merge with a
        // concurrent append from another worktree. Against the base, a top
        // insert is a single prepended line and an append a single
        // appended line — non-overlapping hunks, a clean 3-way merge. This
        // test pins the union driver's WORST case (overlapping rewrites
        // concatenate both sides' lines): parse_jsonl collapses same-id
        // lines with the FIRST occurrence winning, so the top item stays
        // first, the append stays last, and nothing is lost or duplicated.
        let dir = TestDir::new("union_top");
        let base_path = dir.0.join("base.jsonl");
        let mut base = BacklogStore::open(base_path.clone());
        let a = base.add("base a".into(), vec![]);
        let b = base.add("base b".into(), vec![]);

        // The same base file in two worktrees.
        let path_a = dir.0.join("worktree-a.jsonl");
        let path_b = dir.0.join("worktree-b.jsonl");
        std::fs::copy(&base_path, &path_a).unwrap();
        std::fs::copy(&base_path, &path_b).unwrap();

        // Worktree A top-inserts the urgent item; worktree B appends.
        let mut wa = BacklogStore::open(path_a.clone());
        let t = wa.add_at(BacklogPosition::Top, "urgent top insert".into(), vec![]);
        let mut wb = BacklogStore::open(path_b.clone());
        let x = wb.add("appended by B".into(), vec![]);

        // Simulate the git union merge (worst case): concatenate the files.
        let mut union = std::fs::read_to_string(&path_a).unwrap();
        union.push_str(&std::fs::read_to_string(&path_b).unwrap());
        let union_path = dir.0.join("backlog-merged.jsonl");
        std::fs::write(&union_path, union).unwrap();

        let merged = BacklogStore::open(union_path);
        let items = merged.items();
        assert_eq!(items.len(), 4, "no item lost or duplicated");
        assert_eq!(items[0].id, t.id, "the top insert stays first");
        assert_eq!(items[1].id, a.id);
        assert_eq!(items[2].id, b.id);
        assert_eq!(items[3].id, x.id, "the concurrent append stays last");
    }

    #[test]
    fn remove_deletes_existing_only() {
        let dir = TestDir::new("remove");
        let mut store = BacklogStore::open(dir.path());
        let a = store.add("a".into(), vec![]);
        let b = store.add("b".into(), vec![]);

        assert!(store.remove(&a.id));
        assert!(!store.remove(&a.id), "second remove should report false");
        assert!(!store.remove("no-such-id"));
        assert_eq!(store.items().len(), 1);
        assert_eq!(store.items()[0].id, b.id);
    }

    #[test]
    fn reorder_places_listed_ids_first_unlisted_last() {
        let dir = TestDir::new("reorder");
        let mut store = BacklogStore::open(dir.path());
        let a = store.add("a".into(), vec![]);
        let b = store.add("b".into(), vec![]);
        let c = store.add("c".into(), vec![]);
        let d = store.add("d".into(), vec![]);

        // Reorder with an unknown id mixed in: unlisted items keep their
        // relative order and go last.
        store.reorder(&[c.id.clone(), "no-such-id".to_string(), a.id.clone()]);
        let order: Vec<String> = store.items().iter().map(|i| i.id.clone()).collect();
        assert_eq!(
            order,
            vec![c.id.clone(), a.id.clone(), b.id.clone(), d.id.clone()]
        );

        // Duplicate ids in the input are placed only once.
        store.reorder(&[d.id.clone(), d.id.clone(), b.id.clone()]);
        let order: Vec<String> = store.items().iter().map(|i| i.id.clone()).collect();
        assert_eq!(
            order,
            vec![d.id.clone(), b.id.clone(), c.id.clone(), a.id.clone()]
        );
    }

    #[test]
    fn set_status_updates_status_and_note() {
        let dir = TestDir::new("set_status");
        let mut store = BacklogStore::open(dir.path());
        let a = store.add("a".into(), vec![]);

        store.set_status(&a.id, BacklogStatus::InFlight, None);
        assert_eq!(store.items()[0].status, BacklogStatus::InFlight);
        assert!(store.items()[0].note.is_none());

        store.set_status(&a.id, BacklogStatus::Failed, Some("boom".into()));
        assert_eq!(store.items()[0].status, BacklogStatus::Failed);
        assert_eq!(store.items()[0].note.as_deref(), Some("boom"));

        // Unknown id is a no-op.
        store.set_status("no-such-id", BacklogStatus::Done, None);
        assert_eq!(store.items().len(), 1);
        assert_eq!(store.items()[0].status, BacklogStatus::Failed);
    }

    #[test]
    fn requeue_moves_terminal_statuses_back_to_pending() {
        let dir = TestDir::new("requeue_terminal");
        let mut store = BacklogStore::open(dir.path());
        let done = store.add("done".into(), vec![]);
        let failed = store.add("failed".into(), vec![]);
        let cant = store.add("cant".into(), vec![]);
        store.set_status(&done.id, BacklogStatus::Done, None);
        store.set_status(&failed.id, BacklogStatus::Failed, Some("boom".into()));
        store.set_status(
            &cant.id,
            BacklogStatus::CantResolve,
            Some("rolled back".into()),
        );

        assert!(store.requeue(&done.id));
        assert!(store.requeue(&failed.id));
        assert!(store.requeue(&cant.id));
        for item in store.items() {
            assert_eq!(item.status, BacklogStatus::Pending);
            assert!(item.note.is_none(), "requeue must clear the note");
        }

        // Persists: a reopened store sees the re-queued state.
        let store = BacklogStore::open(dir.path());
        assert!(
            store
                .items()
                .iter()
                .all(|i| i.status == BacklogStatus::Pending),
            "requeue must persist to disk"
        );
    }

    #[test]
    fn requeue_refuses_pending_in_flight_and_unknown() {
        let dir = TestDir::new("requeue_refused");
        let mut store = BacklogStore::open(dir.path());
        let pending = store.add("pending".into(), vec![]);
        let inflight = store.add("inflight".into(), vec![]);
        store.set_status(&inflight.id, BacklogStatus::InFlight, Some("sha".into()));

        assert!(!store.requeue(&pending.id));
        assert!(!store.requeue(&inflight.id));
        assert!(!store.requeue("no-such-id"));
        // Refused requeues change nothing — in particular an in-flight item
        // keeps its status + checkpoint note (a stale UI click must not
        // double-queue an item that is actively being worked on).
        assert_eq!(store.items()[0].status, BacklogStatus::Pending);
        assert_eq!(store.items()[1].status, BacklogStatus::InFlight);
        assert_eq!(store.items()[1].note.as_deref(), Some("sha"));
    }

    #[test]
    fn transition_allows_every_legal_row_and_persists() {
        let dir = TestDir::new("transition_legal");
        let mut store = BacklogStore::open(dir.path());
        // Pending → InFlight (execution entry).
        let a = store.add("a".into(), vec![]);
        assert!(store.transition(&a.id, BacklogStatus::InFlight, None));
        assert_eq!(store.items()[0].status, BacklogStatus::InFlight);
        // InFlight → Done (resolution) with a note.
        assert!(store.transition(&a.id, BacklogStatus::Done, Some("finished".into())));
        assert_eq!(store.items()[0].status, BacklogStatus::Done);
        assert_eq!(store.items()[0].note.as_deref(), Some("finished"));
        // Terminal → Pending (requeue row).
        assert!(store.transition(&a.id, BacklogStatus::Pending, None));
        assert_eq!(store.items()[0].status, BacklogStatus::Pending);
        assert!(store.items()[0].note.is_none(), "requeue clears the note");

        // Pending → Failed (the agent's explicit backlog_status marking).
        let pending2 = store.add("b".into(), vec![]);
        assert!(store.transition(&pending2.id, BacklogStatus::Failed, Some("boom".into())));
        assert_eq!(store.items()[1].status, BacklogStatus::Failed);

        // InFlight → Pending (intervention requeue / deferral).
        let inflight = store.add("c".into(), vec![]);
        assert!(store.transition(&inflight.id, BacklogStatus::InFlight, Some("sha".into())));
        assert!(store.transition(&inflight.id, BacklogStatus::Pending, Some("sha".into())));
        assert_eq!(store.items()[2].status, BacklogStatus::Pending);
        assert_eq!(store.items()[2].note.as_deref(), Some("sha"));

        // InFlight → CantResolve (the agent's explicit dead end).
        let inflight2 = store.add("d".into(), vec![]);
        assert!(store.transition(&inflight2.id, BacklogStatus::InFlight, None));
        assert!(store.transition(&inflight2.id, BacklogStatus::CantResolve, None));
        assert_eq!(store.items()[3].status, BacklogStatus::CantResolve);

        // Persists to disk (reopen sees the last state).
        let store = BacklogStore::open(dir.path());
        assert_eq!(store.items()[0].status, BacklogStatus::Pending);
        assert_eq!(store.items()[3].status, BacklogStatus::CantResolve);
    }

    #[test]
    fn transition_refuses_every_illegal_row_and_unknown_ids() {
        let dir = TestDir::new("transition_illegal");
        let mut store = BacklogStore::open(dir.path());
        let pending = store.add("pending".into(), vec![]);
        let inflight = store.add("inflight".into(), vec![]);
        let done = store.add("done".into(), vec![]);
        let failed = store.add("failed".into(), vec![]);
        let cant = store.add("cant".into(), vec![]);
        store.set_status(&inflight.id, BacklogStatus::InFlight, None);
        store.set_status(&done.id, BacklogStatus::Done, None);
        store.set_status(&failed.id, BacklogStatus::Failed, Some("boom".into()));
        store.set_status(&cant.id, BacklogStatus::CantResolve, None);

        // Illegal transitions: same-status, terminal → non-pending, and
        // Pending → Pending must all be refused (no mutation, no persist).
        assert!(!store.transition(&pending.id, BacklogStatus::Pending, None));
        assert!(!store.transition(&inflight.id, BacklogStatus::InFlight, None));
        assert!(!store.transition(&done.id, BacklogStatus::Done, None));
        assert!(!store.transition(&done.id, BacklogStatus::Failed, None));
        assert!(!store.transition(&failed.id, BacklogStatus::CantResolve, None));
        assert!(!store.transition(&cant.id, BacklogStatus::Done, None));
        // Unknown id is refused regardless of target.
        assert!(!store.transition("no-such-id", BacklogStatus::Pending, None));
        assert!(!store.transition("no-such-id", BacklogStatus::Done, None));

        // Nothing changed (statuses + notes untouched).
        assert_eq!(store.items()[0].status, BacklogStatus::Pending);
        assert_eq!(store.items()[1].status, BacklogStatus::InFlight);
        assert_eq!(store.items()[2].status, BacklogStatus::Done);
        assert_eq!(store.items()[3].status, BacklogStatus::Failed);
        assert_eq!(store.items()[4].status, BacklogStatus::CantResolve);
        assert_eq!(store.items()[3].note.as_deref(), Some("boom"));
    }

    #[test]
    fn annotate_appends_note_without_touching_status() {
        let dir = TestDir::new("annotate");
        let mut store = BacklogStore::open(dir.path());
        let plain = store.add("plain".into(), vec![]);
        let noted = store.add("noted".into(), vec![]);
        store.set_status(&noted.id, BacklogStatus::InFlight, Some("abc123".into()));

        // Annotate a note-less item: the addition becomes the note; the
        // status (Pending) is untouched.
        assert!(store.annotate(&plain.id, "steer received during unattended run"));
        assert_eq!(store.items()[0].status, BacklogStatus::Pending);
        assert_eq!(
            store.items()[0].note.as_deref(),
            Some("steer received during unattended run")
        );

        // Annotate an item with an existing note (e.g. a checkpoint sha):
        // the addition is APPENDED — head-sha parsing keeps working — and the
        // status (InFlight) is untouched.
        assert!(store.annotate(&noted.id, "def456"));
        assert_eq!(store.items()[1].status, BacklogStatus::InFlight);
        assert_eq!(store.items()[1].note.as_deref(), Some("abc123 | def456"));

        // Repeated annotations accumulate.
        assert!(store.annotate(&plain.id, "returned to queue"));
        assert_eq!(
            store.items()[0].note.as_deref(),
            Some("steer received during unattended run | returned to queue")
        );

        // Unknown id is refused (no-op).
        assert!(!store.annotate("no-such-id", "ghost"));

        // Persists to disk (reopen sees the annotated notes).
        let store = BacklogStore::open(dir.path());
        assert_eq!(store.items()[1].note.as_deref(), Some("abc123 | def456"));
    }

    #[test]
    fn set_note_replaces_without_touching_status() {
        let dir = TestDir::new("set_note");
        let mut store = BacklogStore::open(dir.path());
        let item = store.add("a".into(), vec![]);
        store.set_status(&item.id, BacklogStatus::InFlight, Some("old-sha".into()));

        // A fresh checkpoint sha REPLACES the stale note — appending would
        // leave the OLD sha at the head and break head-parsing.
        assert!(store.set_note(&item.id, Some("new-sha".into())));
        assert_eq!(store.items()[0].status, BacklogStatus::InFlight);
        assert_eq!(store.items()[0].note.as_deref(), Some("new-sha"));

        // Clearing the note works too.
        assert!(store.set_note(&item.id, None));
        assert!(store.items()[0].note.is_none());

        // Unknown id is refused (no-op).
        assert!(!store.set_note("no-such-id", Some("ghost".into())));

        // Persists to disk.
        store.set_note(&item.id, Some("persisted".into()));
        let store = BacklogStore::open(dir.path());
        assert_eq!(store.items()[0].note.as_deref(), Some("persisted"));
    }

    #[test]
    fn set_plan_id_records_the_item_plan_linkage() {
        // Backlog 45dcf577: the ROOT plan id is recorded on the dispatched
        // item when the workflow enters Executing (the create_plan moment) —
        // the item↔plan linkage the item's status is derived from. It never
        // touches the status, and unknown ids are refused. The plan TITLE
        // rides along (backlog f45513b2) so the Backlog tab shows a
        // human-friendly identifier that survives plan completion.
        let dir = TestDir::new("set_plan_id");
        let mut store = BacklogStore::open(dir.path());
        let item = store.add("a".into(), vec![]);
        store.set_status(&item.id, BacklogStatus::InFlight, None);

        assert!(store.set_plan_id(&item.id, Some("plan-abc123"), Some("First plan")));
        assert_eq!(store.items()[0].status, BacklogStatus::InFlight);
        assert_eq!(store.items()[0].plan_id.as_deref(), Some("plan-abc123"));
        assert_eq!(store.items()[0].plan_title.as_deref(), Some("First plan"));

        // A fresh plan replacing an abandoned one mid-dispatch re-links
        // (id + title together).
        assert!(store.set_plan_id(&item.id, Some("plan-def456"), Some("Second plan")));
        assert_eq!(store.items()[0].plan_id.as_deref(), Some("plan-def456"));
        assert_eq!(store.items()[0].plan_title.as_deref(), Some("Second plan"));

        // Unknown id is refused (no-op).
        assert!(!store.set_plan_id("no-such-id", Some("ghost"), Some("Ghost")));

        // Persists to disk.
        store.set_plan_id(&item.id, Some("persisted-plan"), Some("Persisted"));
        let store = BacklogStore::open(dir.path());
        assert_eq!(store.items()[0].plan_id.as_deref(), Some("persisted-plan"));
        assert_eq!(store.items()[0].plan_title.as_deref(), Some("Persisted"));
    }

    #[test]
    fn edit_updates_text_and_images_preserving_status() {
        let dir = TestDir::new("edit");
        let mut store = BacklogStore::open(dir.path());
        let a = store.add("original".into(), vec!["img1".into()]);
        store.set_status(&a.id, BacklogStatus::InFlight, Some("note".into()));

        // Edit an existing item: text + images change, status + note survive.
        assert!(store.edit(
            &a.id,
            "edited text".into(),
            vec!["img2".into(), "img3".into()]
        ));
        assert_eq!(store.items()[0].text, "edited text");
        assert_eq!(
            store.items()[0].images,
            vec!["img2".to_string(), "img3".to_string()]
        );
        assert_eq!(store.items()[0].status, BacklogStatus::InFlight);
        assert_eq!(store.items()[0].note.as_deref(), Some("note"));

        // Edit persists to disk (reopen and verify).
        let store = BacklogStore::open(dir.path());
        assert_eq!(store.items()[0].text, "edited text");
        assert_eq!(store.items()[0].images.len(), 2);

        // Unknown id is a no-op (returns false, nothing changes).
        let mut store = BacklogStore::open(dir.path());
        assert!(!store.edit("no-such-id", "nope".into(), vec![]));
        assert_eq!(store.items().len(), 1);
        assert_eq!(store.items()[0].text, "edited text");
    }

    #[test]
    fn next_pending_returns_first_pending_in_order() {
        let dir = TestDir::new("next_pending");
        let mut store = BacklogStore::open(dir.path());
        assert!(store.next_pending().is_none());

        let a = store.add("a".into(), vec![]);
        let b = store.add("b".into(), vec![]);
        assert_eq!(store.next_pending().unwrap().id, a.id);

        store.set_status(&a.id, BacklogStatus::InFlight, None);
        assert_eq!(store.next_pending().unwrap().id, b.id);

        store.set_status(&b.id, BacklogStatus::Done, None);
        assert!(store.next_pending().is_none());
    }

    #[test]
    fn pending_item_returns_only_pending() {
        let dir = TestDir::new("pending_item");
        let mut store = BacklogStore::open(dir.path());

        let a = store.add("a".into(), vec![]);
        let b = store.add("b".into(), vec![]);
        store.set_status(&a.id, BacklogStatus::InFlight, None);

        // The pending item is returned by its id.
        assert_eq!(store.pending_item(&b.id).unwrap().id, b.id);
        // The in-flight item is NOT returned (only Pending qualifies).
        assert!(store.pending_item(&a.id).is_none());
        // An unknown id is None.
        assert!(store.pending_item("no-such-id").is_none());
    }

    #[test]
    fn clear_finished_drops_terminal_statuses() {
        let dir = TestDir::new("clear_finished");
        let mut store = BacklogStore::open(dir.path());
        let pending = store.add("pending".into(), vec![]);
        let inflight = store.add("inflight".into(), vec![]);
        let done = store.add("done".into(), vec![]);
        let failed = store.add("failed".into(), vec![]);
        let cant = store.add("cant".into(), vec![]);

        store.set_status(&inflight.id, BacklogStatus::InFlight, None);
        store.set_status(&done.id, BacklogStatus::Done, None);
        store.set_status(&failed.id, BacklogStatus::Failed, None);
        store.set_status(&cant.id, BacklogStatus::CantResolve, None);

        store.clear_finished();
        let remaining: Vec<String> = store.items().iter().map(|i| i.id.clone()).collect();
        assert_eq!(remaining, vec![pending.id, inflight.id]);

        // Clearing again with nothing finished is a no-op (still consistent).
        store.clear_finished();
        assert_eq!(store.items().len(), 2);
    }

    #[test]
    fn persistence_round_trip() {
        let dir = TestDir::new("round_trip");
        let (a_id, b_id);
        {
            let mut store = BacklogStore::open(dir.path());
            let a = store.add("first".into(), vec!["img1".into()]);
            let b = store.add("second".into(), vec![]);
            store.set_status(&a.id, BacklogStatus::Done, Some("finished".into()));
            a_id = a.id;
            b_id = b.id;
        }

        // Reopen in a fresh store — items + statuses + notes survive, and new
        // ids continue past the persisted ones.
        let mut store = BacklogStore::open(dir.path());
        assert_eq!(store.items().len(), 2);
        assert_eq!(store.items()[0].id, a_id);
        assert_eq!(store.items()[0].status, BacklogStatus::Done);
        assert_eq!(store.items()[0].note.as_deref(), Some("finished"));
        assert_eq!(store.items()[0].images, vec!["img1".to_string()]);
        assert_eq!(store.items()[1].id, b_id);
        assert_eq!(store.items()[1].status, BacklogStatus::Pending);

        let c = store.add("third".into(), vec![]);
        assert_ne!(c.id, a_id, "ids are UUIDs — never reissued after reopen");
        assert_ne!(c.id, b_id);
    }

    #[test]
    fn open_tolerates_missing_and_corrupt_files() {
        // Missing file → empty store.
        let dir = TestDir::new("missing");
        let store = BacklogStore::open(dir.path());
        assert!(store.items().is_empty());

        // Corrupt file → empty store (and a warning, not a panic).
        let dir = TestDir::new("corrupt");
        std::fs::write(dir.path(), "{ not valid json !!!").unwrap();
        let mut store = BacklogStore::open(dir.path());
        assert!(store.items().is_empty());

        // The store is still usable — adding overwrites the corrupt file.
        store.add("recovered".into(), vec![]);
        let store = BacklogStore::open(dir.path());
        assert_eq!(store.items().len(), 1);
        assert_eq!(store.items()[0].text, "recovered");
    }

    #[test]
    fn legacy_json_envelope_is_migrated_on_open() {
        // The pre-jsonl `.coding/backlog.json` envelope: opening the jsonl
        // path migrates the items, persists jsonl, and leaves the legacy
        // file in place (git removes it). Production legacy files carried
        // NUMERIC ids ("id": 1) — both that shape and string ids (a mixed
        // file) must load, or a real pre-upgrade backlog starts empty.
        let dir = TestDir::new("migrate");
        let legacy = dir.0.join("backlog.json");
        std::fs::write(
            &legacy,
            "{\"items\":[{\"id\":1,\"text\":\"legacy item\",\"images\":[],\"status\":\"pending\",\"created_at\":1,\"note\":null},{\"id\":\"old-2\",\"text\":\"string-id item\",\"images\":[],\"status\":\"done\",\"created_at\":2,\"note\":null}],\"next_id\":3}",
        )
        .unwrap();

        let mut store = BacklogStore::open(dir.path());
        assert_eq!(store.items().len(), 2);
        assert_eq!(store.items()[0].id, "1", "numeric legacy id stringifies");
        assert_eq!(store.items()[0].text, "legacy item");
        assert_eq!(
            store.items()[1].id,
            "old-2",
            "string legacy id passes through"
        );
        // The jsonl file now exists with the item; the legacy file remains.
        assert!(dir.path().exists(), "jsonl persisted by migration");
        assert!(legacy.exists(), "legacy file left for git to remove");
        let raw = std::fs::read_to_string(dir.path()).unwrap();
        assert_eq!(raw.lines().count(), 2);

        // Reopening reads the migrated jsonl (no double-migration).
        store.add("new item".into(), vec![]);
        let reopened = BacklogStore::open(dir.path());
        assert_eq!(reopened.items().len(), 3);
    }

    #[test]
    fn union_merge_of_two_worktrees_concatenates_lines() {
        // The multi-worktree contract: two stores adding items concurrently
        // produce jsonl files whose union (git's merge=union concatenates
        // both sides' lines) loads back as the merged item list — the UUID
        // ids keep the union collision-free.
        let dir = TestDir::new("union");
        let path_a = dir.0.join("worktree-a.jsonl");
        let path_b = dir.0.join("worktree-b.jsonl");
        let mut a = BacklogStore::open(path_a.clone());
        let mut b = BacklogStore::open(path_b.clone());
        let item_a = a.add("from worktree A".into(), vec![]);
        let item_b = b.add("from worktree B".into(), vec![]);

        // Simulate the git union merge: concatenate the two files.
        let mut union = std::fs::read_to_string(&path_a).unwrap();
        union.push_str(&std::fs::read_to_string(&path_b).unwrap());
        let union_path = dir.0.join("backlog-merged.jsonl");
        std::fs::write(&union_path, union).unwrap();

        let merged = BacklogStore::open(union_path);
        assert_eq!(merged.items().len(), 2);
        assert_eq!(merged.items()[0].id, item_a.id);
        assert_eq!(merged.items()[1].id, item_b.id);
        assert_ne!(
            item_a.id, item_b.id,
            "UUID ids never collide across worktrees"
        );
    }

    #[test]
    fn status_serializes_snake_case() {
        let json = serde_json::to_string(&BacklogStatus::CantResolve).unwrap();
        assert_eq!(json, "\"cant_resolve\"");
        let back: BacklogStatus = serde_json::from_str("\"in_flight\"").unwrap();
        assert_eq!(back, BacklogStatus::InFlight);
    }

    // --- image sidecar files (data URLs → gitignored files + path refs) ---

    /// A valid 1×1 PNG data URL (decodes to the PNG signature bytes).
    const PNG_DATA_URL: &str = "data:image/png;base64,iVBORw0KGgo=";

    #[test]
    fn add_with_data_url_stores_path_and_resolves_back() {
        let dir = TestDir::new("img_add");
        let mut store = BacklogStore::open(dir.path());
        let item = store.add("with image".into(), vec![PNG_DATA_URL.into()]);

        // The stored image is a PATH, not the inline data URL.
        assert_eq!(item.images.len(), 1);
        assert!(
            item.images[0].starts_with("backlog-images/"),
            "stored image should be a path, got: {}",
            item.images[0]
        );
        assert!(
            item.images[0].ends_with(".png"),
            "path should have the png extension, got: {}",
            item.images[0]
        );
        // The sidecar file exists with the decoded bytes.
        let file = dir.0.join(&item.images[0]);
        assert!(file.exists(), "sidecar file should exist");
        let bytes = std::fs::read(&file).unwrap();
        assert_eq!(
            bytes, b"\x89PNG\r\n\x1a\n",
            "file holds the decoded image bytes"
        );
        // resolve_images reads the file back into the original data URL.
        let resolved = store.resolve_images(&item);
        assert_eq!(resolved.len(), 1);
        assert_eq!(
            resolved[0], PNG_DATA_URL,
            "resolve_images round-trips the data URL"
        );
    }

    #[test]
    fn add_with_non_data_url_passes_through_unchanged() {
        // Backward-compat: strings that aren't data URLs (test fixtures,
        // opaque strings) pass through as-is — no file is written.
        let dir = TestDir::new("img_passthrough");
        let mut store = BacklogStore::open(dir.path());
        let item = store.add("fixture".into(), vec!["img1".into()]);
        assert_eq!(
            item.images,
            vec!["img1".to_string()],
            "non-data-URL passes through"
        );
        assert!(
            !dir.0.join("backlog-images").exists(),
            "no sidecar dir created"
        );
    }

    #[test]
    fn edit_replaces_image_files() {
        let dir = TestDir::new("img_edit");
        let mut store = BacklogStore::open(dir.path());
        let item = store.add("orig".into(), vec![PNG_DATA_URL.into()]);
        let file_path = dir.0.join(&item.images[0]);
        assert!(file_path.exists(), "sidecar file exists after add");
        let old_bytes = std::fs::read(&file_path).unwrap();

        // Edit with a different (larger) data URL — the file is replaced
        // in place (same path: both are image/png → 0.png), with the new
        // content. The old bytes must NOT survive.
        let new_url = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==";
        assert!(store.edit(&item.id, "edited".into(), vec![new_url.into()]));
        assert!(file_path.exists(), "sidecar file still exists after edit");
        let new_bytes = std::fs::read(&file_path).unwrap();
        assert_ne!(
            old_bytes, new_bytes,
            "edit must replace the file content (old bytes gone)"
        );
        assert!(
            new_bytes.len() > old_bytes.len(),
            "new content is larger (the new data URL decodes to more bytes)"
        );
    }

    #[test]
    fn remove_soft_deletes_line_stays_on_disk() {
        let dir = TestDir::new("soft_delete");
        let mut store = BacklogStore::open(dir.path());
        let a = store.add("a".into(), vec![]);
        let b = store.add("b".into(), vec![]);

        assert!(store.remove(&a.id));
        // Hidden from every read path...
        assert_eq!(store.items().len(), 1);
        assert_eq!(store.items()[0].id, b.id);
        assert_eq!(store.next_pending().map(|i| i.id), Some(b.id.clone()));
        // ...but the JSONL line persists, marked deleted_at.
        let raw = std::fs::read_to_string(dir.path()).unwrap();
        assert_eq!(raw.lines().count(), 2, "both lines stay on disk");
        assert!(
            raw.contains("deleted_at"),
            "the deleted line carries deleted_at, got: {raw}"
        );
        // The deletion survives a reopen.
        let store = BacklogStore::open(dir.path());
        assert_eq!(store.items().len(), 1);
        assert_eq!(store.items()[0].id, b.id);
    }

    // ---- deferred (skip Run-All) — user request 2027-01-07 ----

    #[test]
    fn deferred_flag_round_trips_and_is_omitted_when_false() {
        let dir = TestDir::new("deferred_round_trip");
        let mut store = BacklogStore::open(dir.path());
        let a = store.add("a".into(), vec![]);

        // Set → the JSONL line carries the field; a reopen reads it back.
        assert!(store.set_deferred(&a.id, true));
        let raw = std::fs::read_to_string(dir.path()).unwrap();
        assert!(
            raw.contains("\"deferred\":true"),
            "the deferred line carries the flag, got: {raw}"
        );
        assert!(BacklogStore::open(dir.path()).items()[0].deferred);

        // Clear → the field is OMITTED: non-deferred lines stay
        // byte-identical to the pre-change format (union-merge friendly).
        assert!(store.set_deferred(&a.id, false));
        let raw = std::fs::read_to_string(dir.path()).unwrap();
        assert!(
            !raw.contains("deferred"),
            "a cleared flag omits the field, got: {raw}"
        );
        assert!(!BacklogStore::open(dir.path()).items()[0].deferred);
    }

    #[test]
    fn old_lines_without_deferred_parse_as_false() {
        // Pre-change backlog.jsonl lines (no `deferred` field) must parse
        // unchanged — the field defaults to false.
        let dir = TestDir::new("deferred_legacy_line");
        let line = r#"{"id":"x-1","text":"legacy","images":[],"status":"pending","created_at":1,"note":null}"#;
        std::fs::write(dir.path(), format!("{line}\n")).unwrap();
        let store = BacklogStore::open(dir.path());
        assert_eq!(store.items().len(), 1);
        assert!(!store.items()[0].deferred);
    }

    #[test]
    fn next_pending_eligible_excluding_skips_handed_out_ids() {
        // Parallel run-all (plan ffd7a86f): the selection PEEKS (items stay
        // `Pending` until the workflow enters `Executing`), so the parallel
        // dispatch window must skip the ids already handed out — the main
        // agent's `current_item` and every spawned run's item. Without the
        // exclusion the window would hand the SAME top item to a spawned
        // agent while the main agent works it.
        let dir = TestDir::new("parallel_exclude");
        let mut store = BacklogStore::open(dir.path());
        let a = store.add("a".into(), vec![]);
        let b = store.add("b".into(), vec![]);
        let c = store.add("c".into(), vec![]);

        // The plain selector returns the top item — twice (it peeks,
        // nothing is consumed).
        assert_eq!(store.next_pending_eligible().unwrap().id, a.id);
        assert_eq!(store.next_pending_eligible().unwrap().id, a.id);
        // The parallel selector skips the handed-out ids: with a+b excluded
        // it hands out c…
        assert_eq!(
            store
                .next_pending_eligible_excluding(&[a.id.clone(), b.id.clone()])
                .expect("c is next")
                .id,
            c.id
        );
        // …and with everything handed out there is nothing left.
        assert!(store
            .next_pending_eligible_excluding(&[a.id.clone(), b.id.clone(), c.id.clone()])
            .is_none());
    }

    #[test]
    fn next_pending_eligible_skips_deferred_items() {
        let dir = TestDir::new("eligible_selection");
        let mut store = BacklogStore::open(dir.path());
        let a = store.add("a".into(), vec![]);
        let b = store.add("b".into(), vec![]);

        // Defer the first item: Run-All selection skips it…
        assert!(store.set_deferred(&a.id, true));
        assert_eq!(
            store.next_pending_eligible().expect("b is eligible").id,
            b.id
        );
        // …while next_pending (auto-feed, dispatch-next) and pending_item
        // (the per-item ▶ button — manual in-app handling) still see it.
        assert_eq!(
            store.next_pending().expect("next_pending unchanged").id,
            a.id
        );
        assert_eq!(
            store.pending_item(&a.id).expect("manual dispatch works").id,
            a.id
        );

        // Un-defer restores eligibility.
        assert!(store.set_deferred(&a.id, false));
        assert_eq!(
            store.next_pending_eligible().expect("a eligible again").id,
            a.id
        );

        // All deferred → nothing eligible (the run ends cleanly).
        assert!(store.set_deferred(&a.id, true));
        assert!(store.set_deferred(&b.id, true));
        assert!(
            store.next_pending_eligible().is_none(),
            "only-deferred backlog yields no eligible item"
        );
    }

    #[test]
    fn set_deferred_unknown_or_deleted_id_is_noop() {
        let dir = TestDir::new("deferred_unknown_id");
        let mut store = BacklogStore::open(dir.path());
        let a = store.add("a".into(), vec![]);
        store.remove(&a.id);
        assert!(!store.set_deferred(&a.id, true), "deleted id is refused");
        assert!(!store.set_deferred("no-such-id", true), "unknown id is refused");
    }

    #[test]
    fn deferral_survives_status_transitions_and_requeue() {
        // The flag is orthogonal to status: transitions/requeue preserve it
        // (a deferred item requeued after a failure stays out of Run-All).
        let dir = TestDir::new("deferred_survives_requeue");
        let mut store = BacklogStore::open(dir.path());
        let a = store.add("a".into(), vec![]);
        assert!(store.set_deferred(&a.id, true));
        assert!(store.transition(&a.id, BacklogStatus::Failed, Some("boom".to_string())));
        assert!(store.requeue(&a.id));
        let item = store.pending_item(&a.id).expect("requeued to pending");
        assert!(item.deferred, "the flag survives transition + requeue");
        assert_eq!(item.note, None, "requeue clears the note but not the flag");
    }

    #[test]
    fn mutators_treat_deleted_id_as_unknown() {
        let dir = TestDir::new("deleted_mutators");
        let mut store = BacklogStore::open(dir.path());
        let a = store.add("a".into(), vec![]);

        assert!(store.remove(&a.id));
        assert!(!store.remove(&a.id), "second remove reports false");
        assert!(
            !store.transition(&a.id, BacklogStatus::Done, None),
            "transition on a deleted id is refused"
        );
        assert!(!store.requeue(&a.id), "requeue on a deleted id is refused");
        assert!(
            !store.edit(&a.id, "new text".into(), vec![]),
            "edit on a deleted id is refused"
        );
        assert!(
            !store.annotate(&a.id, "note"),
            "annotate on a deleted id is refused"
        );
    }

    #[test]
    fn open_purges_items_deleted_more_than_30_days_ago() {
        let dir = TestDir::new("purge");
        let now = now_secs();
        let make_line = |id: &str, deleted_at: Option<u64>| {
            let mut v = serde_json::json!({
                "id": id, "text": id, "images": [], "status": "pending",
                "created_at": 1, "note": null
            });
            if let Some(at) = deleted_at {
                v["deleted_at"] = serde_json::json!(at);
            }
            v.to_string()
        };
        let jsonl = format!(
            "{}\n{}\n{}\n",
            make_line("old", Some(now - PURGE_AFTER_SECS - 86_400)), // 31 days ago
            make_line("fresh", Some(now - 86_400)),                  // 1 day ago
            make_line("live", None),                                 // legacy shape
        );
        std::fs::write(dir.path(), &jsonl).unwrap();
        // An image sidecar dir for the expired item — the purge must remove it.
        let old_img_dir = dir.0.join("backlog-images").join("old");
        std::fs::create_dir_all(&old_img_dir).unwrap();
        std::fs::write(old_img_dir.join("0.png"), b"png").unwrap();

        let store = BacklogStore::open(dir.path());
        // The expired item is hard-removed; the fresh deletion and the live
        // item survive.
        assert_eq!(store.items().len(), 1);
        assert_eq!(store.items()[0].id, "live");
        assert!(!old_img_dir.exists(), "expired item's images are purged");
        let raw = std::fs::read_to_string(dir.path()).unwrap();
        assert!(!raw.contains("\"id\":\"old\""), "expired line gone from disk");
        assert!(raw.contains("\"id\":\"fresh\""), "fresh deletion stays");
        assert!(raw.contains("\"id\":\"live\""), "live line stays");
    }

    #[test]
    fn union_merge_soft_deleted_line_stays_deleted() {
        // The git union merge concatenates both sides' lines: worktree A
        // soft-deleted item X while worktree B still had it live, so the
        // merged file carries the same id twice. The live duplicate must NOT
        // resurrect the item — deletion is sticky.
        let dir = TestDir::new("union_soft_delete");
        let now = now_secs();
        let live =
            r#"{"id":"x-1","text":"live","images":[],"status":"pending","created_at":1,"note":null}"#;
        let deleted = format!(
            r#"{{"id":"x-1","text":"live","images":[],"status":"pending","created_at":1,"note":null,"deleted_at":{now}}}"#
        );
        // Either line order must resolve to deleted.
        std::fs::write(dir.path(), format!("{live}\n{deleted}\n")).unwrap();
        let store = BacklogStore::open(dir.path());
        assert!(store.items().is_empty(), "deleted wins over live duplicate");

        std::fs::write(dir.path(), format!("{deleted}\n{live}\n")).unwrap();
        let store = BacklogStore::open(dir.path());
        assert!(store.items().is_empty(), "order does not matter");
    }

    #[test]
    fn remove_soft_deletes_and_keeps_images_until_purge() {
        let dir = TestDir::new("img_remove");
        let mut store = BacklogStore::open(dir.path());
        let item = store.add("with image".into(), vec![PNG_DATA_URL.into()]);
        let item_dir = dir.0.join("backlog-images").join(&item.id);
        assert!(item_dir.exists(), "image dir exists before remove");

        assert!(store.remove(&item.id));
        // Soft delete: hidden from every read path, but the JSONL line and
        // the image sidecar files stay on disk until the 30-day purge.
        assert!(store.items().is_empty());
        assert!(item_dir.exists(), "image dir survives the soft delete");
        let raw = std::fs::read_to_string(dir.path()).unwrap();
        assert!(
            raw.contains(&item.id) && raw.contains("deleted_at"),
            "soft-deleted line stays on disk with deleted_at set, got: {raw}"
        );
    }

    #[test]
    fn clear_finished_soft_deletes_and_keeps_images() {
        let dir = TestDir::new("img_clear");
        let mut store = BacklogStore::open(dir.path());
        let done = store.add("done".into(), vec![PNG_DATA_URL.into()]);
        let _pending = store.add("pending".into(), vec![PNG_DATA_URL.into()]);
        store.set_status(&done.id, BacklogStatus::Done, None);

        let done_dir = dir.0.join("backlog-images").join(&done.id);
        assert!(
            done_dir.exists(),
            "done item's image dir exists before clear"
        );

        store.clear_finished();
        // Soft delete: the finished item is hidden from every read path but
        // its line and image sidecar files stay on disk until the 30-day
        // purge. The pending item is untouched.
        assert_eq!(store.items().len(), 1);
        assert!(
            done_dir.exists(),
            "done item's image dir survives the soft delete"
        );
        let raw = std::fs::read_to_string(dir.path()).unwrap();
        assert!(raw.contains(&done.id), "finished line stays on disk");
    }

    #[test]
    fn resolve_images_skips_missing_file_gracefully() {
        // A missing sidecar file (cross-worktree, manually deleted) must not
        // panic — resolve_images just returns fewer images.
        let dir = TestDir::new("img_missing");
        let mut store = BacklogStore::open(dir.path());
        let item = store.add("with image".into(), vec![PNG_DATA_URL.into()]);
        // Manually delete the sidecar file.
        let file = dir.0.join(&item.images[0]);
        std::fs::remove_file(&file).unwrap();
        let resolved = store.resolve_images(&item);
        assert!(resolved.is_empty(), "missing file → empty (no panic)");
    }

    #[test]
    fn legacy_inline_data_url_migrated_on_open() {
        // A pre-upgrade JSONL carries an inline base64 data URL. On open, it
        // must be migrated to a sidecar file + a path ref, and the re-persisted
        // JSONL must carry the path (not the inline data).
        let dir = TestDir::new("img_migrate");
        let jsonl = format!(
            "{{\"id\":\"old-1\",\"text\":\"legacy\",\"images\":[\"{PNG_DATA_URL}\"],\
             \"status\":\"pending\",\"created_at\":1,\"note\":null}}\n"
        );
        std::fs::write(dir.path(), &jsonl).unwrap();

        let store = BacklogStore::open(dir.path());
        assert_eq!(store.items().len(), 1);
        let item = &store.items()[0];
        assert!(
            item.images[0].starts_with("backlog-images/"),
            "migrated image should be a path, got: {}",
            item.images[0]
        );
        // The sidecar file was created.
        assert!(
            dir.0.join(&item.images[0]).exists(),
            "sidecar file created by migration"
        );
        // The re-persisted JSONL carries the path, not the inline data URL.
        let raw = std::fs::read_to_string(dir.path()).unwrap();
        assert!(
            !raw.contains("base64"),
            "re-persisted JSONL must not carry inline base64, got: {raw}"
        );
        assert!(
            raw.contains("backlog-images/"),
            "re-persisted JSONL carries the path ref"
        );
        // resolve_images round-trips the migrated file back to the data URL.
        let resolved = store.resolve_images(item);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0], PNG_DATA_URL);
    }

    // --- LOW 2 regression: path-traversal guards (tampered JSONL) ---

    #[test]
    fn write_image_files_rejects_unsafe_item_id() {
        // A tampered JSONL could carry an item_id with `..` — write_image_files
        // must NOT write a file outside the image dir. It keeps the images
        // inline (returns them unchanged) and writes nothing.
        let dir = TestDir::new("img_unsafe_write");
        let store = BacklogStore::open(dir.path());
        let paths = store.write_image_files("../escape", &[PNG_DATA_URL.into()]);
        // The data URL is returned unchanged (kept inline, no file written).
        assert_eq!(paths, vec![PNG_DATA_URL.to_string()]);
        assert!(
            !dir.0.join("backlog-images").exists(),
            "no image dir created for an unsafe item id"
        );
    }

    #[test]
    fn delete_item_images_is_noop_for_unsafe_id() {
        // delete_item_images with an unsafe id must be a silent no-op (never
        // remove a dir outside backlog-images/).
        let dir = TestDir::new("img_unsafe_delete");
        // Create a decoy dir OUTSIDE backlog-images that an unsafe delete must
        // NOT touch.
        let decoy = dir.0.join("decoy");
        std::fs::create_dir_all(&decoy).unwrap();
        std::fs::write(decoy.join("secret.txt"), "do not delete").unwrap();

        let store = BacklogStore::open(dir.path());
        store.delete_item_images("../decoy");
        assert!(
            decoy.exists(),
            "unsafe delete must not remove the decoy dir"
        );
        assert!(
            decoy.join("secret.txt").exists(),
            "unsafe delete must not touch files outside the image dir"
        );
    }

    #[test]
    fn resolve_images_rejects_traversal_path() {
        // A tampered JSONL could carry an image path with `..` — resolve_images
        // must skip it (return None for that entry) instead of reading an
        // arbitrary file and exfiltrating it as a data URL. The `starts_with`
        // check alone is bypassable (it doesn't resolve `..`), so the explicit
        // ParentDir-component check must fire.
        let dir = TestDir::new("img_traversal");
        let mut store = BacklogStore::open(dir.path());
        // Seed a real item, then tamper its images with a traversal path.
        let item = store.add("tampered".into(), vec![]);
        {
            let it = store.items.iter_mut().find(|i| i.id == item.id).unwrap();
            it.images = vec!["backlog-images/../../decoy.txt".into()];
        }
        // Write a decoy file the traversal path would reach if unguarded.
        std::fs::write(dir.0.join("decoy.txt"), "exfiltrated").unwrap();

        let resolved = store.resolve_images(&store.items[0]);
        assert!(
            resolved.is_empty(),
            "a traversal path must be skipped (None), not read + exfiltrated; got: {resolved:?}"
        );
    }

    #[test]
    fn mutation_does_not_clobber_external_disk_changes() {
        // Regression: BacklogStore loads items once at open(); if the disk
        // file changes afterwards (git merge/checkout/restore/merge_to_main),
        // the in-memory state is stale. The next mutation must NOT persist the
        // stale state over the disk changes — it must re-read from disk first
        // (read-modify-write) so external changes are never clobbered.
        //
        // Reproduces the recurring "steering-time backlog clobber": an empty
        // in-memory store persisted empty over committed items after a git op
        // changed the disk file out from under it.
        let dir = TestDir::new("clobber_guard");

        // 1. Open a store on an empty file — in-memory is empty.
        let mut store = BacklogStore::open(dir.path());
        assert!(store.items().is_empty());

        // 2. Simulate a git operation (restore/merge/checkout) that writes
        //    items to the disk file OUTSIDE the store. The in-memory state is
        //    now stale (empty) while the disk has 2 items.
        let disk_item_a = BacklogItem {
            id: "aaaaaaaa-0000-0000-0000-000000000001".into(),
            text: "from git".into(),
            images: vec![],
            status: BacklogStatus::Pending,
            created_at: 100,
            note: None,
            deferred: false,
            plan_id: None,
            plan_title: None,
            deleted_at: None,
        };
        let disk_item_b = BacklogItem {
            id: "aaaaaaaa-0000-0000-0000-000000000002".into(),
            text: "also from git".into(),
            images: vec![],
            status: BacklogStatus::Pending,
            created_at: 101,
            note: None,
            deferred: false,
            plan_id: None,
            plan_title: None,
            deleted_at: None,
        };
        let disk_jsonl = format!(
            "{}\n{}\n",
            serde_json::to_string(&disk_item_a).unwrap(),
            serde_json::to_string(&disk_item_b).unwrap(),
        );
        std::fs::write(dir.path(), &disk_jsonl).unwrap();

        // 3. Call a mutation on the stale store. `reorder(&[])` always
        //    persists (even on an empty store), so it's the most direct
        //    clobber path — without the reload it would write an empty file.
        store.reorder(&[]);

        // 4. The disk items must survive — the mutation must have re-read
        //    from disk before persisting.
        let raw = std::fs::read_to_string(dir.path()).unwrap();
        let reloaded = parse_jsonl(&raw).unwrap();
        assert_eq!(
            reloaded.len(),
            2,
            "external disk changes must survive a mutation; got {} items: {raw}",
            reloaded.len()
        );
        assert_eq!(reloaded[0].id, disk_item_a.id);
        assert_eq!(reloaded[1].id, disk_item_b.id);

        // 5. A second mutation (add) must also preserve the external items —
        //    it reloads, appends, and persists all items.
        store.add("new item".into(), vec![]);
        let raw = std::fs::read_to_string(dir.path()).unwrap();
        let reloaded = parse_jsonl(&raw).unwrap();
        assert_eq!(
            reloaded.len(),
            3,
            "add must preserve external disk items; got {} items: {raw}",
            reloaded.len()
        );
        assert_eq!(reloaded[0].id, disk_item_a.id);
        assert_eq!(reloaded[1].id, disk_item_b.id);
        assert_eq!(reloaded[2].text, "new item");
    }
}
