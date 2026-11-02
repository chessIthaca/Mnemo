// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! The Plans tab's backlog: the project's `.coding/backlog.jsonl` ideas —
//! plans-to-be — read and edited in place.
//!
//! The kernel store [`mnemo::app::backlog::BacklogStore`] is the one source of
//! truth. Every command here opens it FRESH on
//! `<project root>/.coding/backlog.jsonl` and keeps no long-lived in-memory
//! copy. That is deliberate: the 2026-08-31 bug
//! (`.coding/knowledge/bug/2026-08-31-backlog-jsonl-clobbered-to-empty-by-stale-in-mem.md`)
//! was a stale in-memory store persisting over entries committed by a git
//! operation. A fresh open per call — on top of the store's own re-read before
//! every mutation — makes that clobber impossible here, and
//! [`every_write_starts_from_the_file_on_disk`] pins the property.
//!
//! Every mutating command returns the FULL live item list in display order, so
//! the view replaces its state with the store's answer instead of patching a
//! local copy, and emits [`BACKLOG_CHANGED_CHANNEL`] so any other open view
//! refreshes.
//!
//! The section is the plan inbox only — add / edit / reorder / defer / remove.
//! Mnemo2's work runs on the plan board, so nothing here dispatches: a later
//! plan may add "Plan this" (the lead turns an idea into a plan).

use std::path::{Path, PathBuf};

use mnemo::app::backlog::{BacklogItem, BacklogStore};
use tauri::{AppHandle, Emitter, State};

use crate::mnemo2::ipc::Mnemo2State;

/// The payload-less event emitted after every successful backlog write, so
/// every open view re-reads the file.
pub const BACKLOG_CHANGED_CHANNEL: &str = "mnemo2://backlog-changed";

/// The longest idea text the section accepts, in characters. The backlog is a
/// plan inbox, not a dispatched prompt: a short paragraph is plenty, and the
/// cap keeps one paste from turning the JSONL into a document.
const MAX_IDEA_CHARS: usize = 2000;

/// The backlog file under a project root.
fn backlog_file(root: &Path) -> PathBuf {
    root.join(".coding").join("backlog.jsonl")
}

/// The open project's backlog path, or the same clear "no open lead" error the
/// other Mnemo2 commands return.
async fn backlog_path(state: &Mnemo2State) -> Result<PathBuf, String> {
    let lead = state
        .open_lead()
        .await
        .ok_or_else(|| "Mnemo2 has no open lead; call lead_open first".to_string())?;
    Ok(backlog_file(lead.workspace_root()))
}

/// The live items in the store's display order, read fresh from `path`.
///
/// A missing file is an empty backlog (the store opens it empty); a corrupt
/// line is skipped by the store's parser — the same tolerance every other
/// backlog reader has.
fn read_items(path: &Path) -> Vec<BacklogItem> {
    BacklogStore::open(path.to_path_buf()).items()
}

/// Trim and validate a backlog idea's text.
///
/// The store's add path never validates, and this section is a plan inbox (no
/// dispatch), so Mnemo1's dispatch detail bar (headline + a long body naming
/// exact paths) is deliberately NOT imposed here: one trimmed, non-empty idea
/// is enough to capture a thought.
fn normalize_idea(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("a backlog item needs some text".to_string());
    }
    let chars = text.chars().count();
    if chars > MAX_IDEA_CHARS {
        return Err(format!(
            "the backlog item is {chars} characters long; keep an idea under {MAX_IDEA_CHARS}"
        ));
    }
    Ok(text.to_string())
}

/// The error for an id that is not a live item — removed elsewhere, or never
/// there (the view's list is one refresh behind the file).
fn unknown_item(id: &str) -> String {
    format!("no backlog item with id {id}")
}

/// Append one idea at the end of the queue and return the new list.
fn add_item(path: &Path, text: &str) -> Result<Vec<BacklogItem>, String> {
    let text = normalize_idea(text)?;
    let mut store = BacklogStore::open(path.to_path_buf());
    store.add(text, Vec::new());
    Ok(store.items())
}

/// Replace one idea's text and return the new list.
///
/// The item's existing image attachments are kept: [`BacklogStore::edit`]
/// deletes the item's sidecar image directory and re-writes it from the
/// strings it is handed, so passing the stored PATHS would leave dangling
/// references. Resolving them back to data URLs first re-creates the very
/// same files (this section has no image upload, so the set never changes).
fn edit_item(path: &Path, id: &str, text: &str) -> Result<Vec<BacklogItem>, String> {
    let text = normalize_idea(text)?;
    let mut store = BacklogStore::open(path.to_path_buf());
    let Some(item) = store.items().into_iter().find(|item| item.id == id) else {
        return Err(unknown_item(id));
    };
    let images = store.resolve_images(&item);
    if !store.edit(id, text, images) {
        return Err(unknown_item(id));
    }
    Ok(store.items())
}

/// Soft-delete one idea (its line stays in the file) and return the new list.
fn remove_item(path: &Path, id: &str) -> Result<Vec<BacklogItem>, String> {
    let mut store = BacklogStore::open(path.to_path_buf());
    if !store.remove(id) {
        return Err(unknown_item(id));
    }
    Ok(store.items())
}

/// Resequence the queue to `ids` (listed ids first, in that order; anything
/// unlisted keeps its relative position after them) and return the new list.
fn reorder_items(path: &Path, ids: &[String]) -> Vec<BacklogItem> {
    let mut store = BacklogStore::open(path.to_path_buf());
    store.reorder(ids);
    store.items()
}

/// Set or clear one idea's deferred flag and return the new list.
fn set_deferred(path: &Path, id: &str, deferred: bool) -> Result<Vec<BacklogItem>, String> {
    let mut store = BacklogStore::open(path.to_path_buf());
    if !store.set_deferred(id, deferred) {
        return Err(unknown_item(id));
    }
    Ok(store.items())
}

/// Soft-delete every finished (done / failed / cant-resolve) idea and return
/// the new list.
fn clear_finished(path: &Path) -> Vec<BacklogItem> {
    let mut store = BacklogStore::open(path.to_path_buf());
    store.clear_finished();
    store.items()
}

/// The project backlog's live items, in display order.
#[tauri::command]
pub async fn backlog_items(state: State<'_, Mnemo2State>) -> Result<Vec<BacklogItem>, String> {
    Ok(read_items(&backlog_path(&state).await?))
}

/// Add an idea at the end of the backlog; returns the new list.
#[tauri::command]
pub async fn backlog_add(
    state: State<'_, Mnemo2State>,
    app: AppHandle,
    text: String,
) -> Result<Vec<BacklogItem>, String> {
    let items = add_item(&backlog_path(&state).await?, &text)?;
    let _ = app.emit(BACKLOG_CHANGED_CHANNEL, ());
    Ok(items)
}

/// Rewrite one idea's text; returns the new list.
#[tauri::command]
pub async fn backlog_edit(
    state: State<'_, Mnemo2State>,
    app: AppHandle,
    id: String,
    text: String,
) -> Result<Vec<BacklogItem>, String> {
    let items = edit_item(&backlog_path(&state).await?, &id, &text)?;
    let _ = app.emit(BACKLOG_CHANGED_CHANNEL, ());
    Ok(items)
}

/// Soft-delete one idea; returns the new list.
#[tauri::command]
pub async fn backlog_remove(
    state: State<'_, Mnemo2State>,
    app: AppHandle,
    id: String,
) -> Result<Vec<BacklogItem>, String> {
    let items = remove_item(&backlog_path(&state).await?, &id)?;
    let _ = app.emit(BACKLOG_CHANGED_CHANNEL, ());
    Ok(items)
}

/// Resequence the queue; returns the new list.
#[tauri::command]
pub async fn backlog_reorder(
    state: State<'_, Mnemo2State>,
    app: AppHandle,
    ids: Vec<String>,
) -> Result<Vec<BacklogItem>, String> {
    let items = reorder_items(&backlog_path(&state).await?, &ids);
    let _ = app.emit(BACKLOG_CHANGED_CHANNEL, ());
    Ok(items)
}

/// Set or clear one idea's deferred flag; returns the new list.
#[tauri::command]
pub async fn backlog_set_deferred(
    state: State<'_, Mnemo2State>,
    app: AppHandle,
    id: String,
    deferred: bool,
) -> Result<Vec<BacklogItem>, String> {
    let items = set_deferred(&backlog_path(&state).await?, &id, deferred)?;
    let _ = app.emit(BACKLOG_CHANGED_CHANNEL, ());
    Ok(items)
}

/// Soft-delete every finished idea; returns the new list.
#[tauri::command]
pub async fn backlog_clear_finished(
    state: State<'_, Mnemo2State>,
    app: AppHandle,
) -> Result<Vec<BacklogItem>, String> {
    let items = clear_finished(&backlog_path(&state).await?);
    let _ = app.emit(BACKLOG_CHANGED_CHANNEL, ());
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mnemo::app::backlog::BacklogStatus;
    use tempfile::TempDir;

    /// A 1×1 PNG (only the header bytes matter — nothing here decodes the
    /// pixels, the store only round-trips the bytes through a sidecar file).
    const PNG_DATA_URL: &str = "data:image/png;base64,iVBORw0KGgo=";

    fn project() -> TempDir {
        tempfile::tempdir().expect("temporary project directory")
    }

    fn file(root: &TempDir) -> PathBuf {
        backlog_file(root.path())
    }

    fn texts(items: &[BacklogItem]) -> Vec<&str> {
        items.iter().map(|item| item.text.as_str()).collect()
    }

    fn id_of(path: &Path, text: &str) -> String {
        read_items(path)
            .into_iter()
            .find(|item| item.text == text)
            .unwrap_or_else(|| panic!("no backlog item {text:?}"))
            .id
    }

    /// Append one line written by someone else (another worktree, a git union
    /// merge, the Mnemo1 UI) straight to the file — no store involved.
    fn append_on_disk(path: &Path, id: &str, text: &str, status: &str) {
        let mut content = std::fs::read_to_string(path).unwrap_or_default();
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!(
            "{{\"id\":\"{id}\",\"text\":\"{text}\",\"images\":[],\"status\":\"{status}\",\"created_at\":1}}\n"
        ));
        std::fs::write(path, content).expect("append an item on disk");
    }

    /// The failure mode of the 2026-08-31 bug: a write persisted a stale
    /// in-memory list over entries that reached the file after it was read.
    fn assert_on_disk(path: &Path, texts: &[&str]) {
        let stored = std::fs::read_to_string(path).expect("read the backlog file");
        for text in texts {
            assert!(
                stored.contains(text),
                "{text:?} must survive the write; file:\n{stored}"
            );
        }
    }

    #[test]
    fn add_appends_an_idea_and_persists_it() {
        let project = project();
        let path = file(&project);

        let items = add_item(&path, "  Draft the backlog section  ").expect("add");
        assert_eq!(texts(&items), ["Draft the backlog section"]);
        assert_eq!(items[0].status, BacklogStatus::Pending);
        assert!(!items[0].deferred);
        assert!(items[0].images.is_empty());

        let items = add_item(&path, "Second idea").expect("add again");
        assert_eq!(texts(&items), ["Draft the backlog section", "Second idea"]);
        let stored = std::fs::read_to_string(&path).expect("read the backlog file");
        assert_eq!(stored.lines().count(), 2, "one JSON object per line");
    }

    #[test]
    fn a_missing_file_reads_as_an_empty_backlog() {
        let project = project();
        let path = file(&project);

        assert!(read_items(&path).is_empty());
        assert!(!path.exists(), "reading never creates the file");
    }

    #[test]
    fn an_empty_idea_is_refused() {
        let project = project();
        let path = file(&project);

        let error = add_item(&path, " \n\t ").expect_err("empty text is refused");
        assert!(error.contains("needs some text"), "{error}");
        assert!(!path.exists(), "a refused add writes nothing");
    }

    /// The clobber regression test (2026-08-31): an entry appended to the file
    /// on disk after our last read must survive the next write.
    #[test]
    fn a_write_keeps_an_entry_appended_on_disk() {
        let project = project();
        let path = file(&project);
        add_item(&path, "First idea").expect("add");

        // Another writer appends AFTER our store last read the file. A
        // long-lived in-memory store would persist its own copy over it.
        append_on_disk(&path, "from-elsewhere", "Idea from another worktree", "pending");

        let items = add_item(&path, "Second idea").expect("add after the external write");
        assert_eq!(
            texts(&items),
            ["First idea", "Idea from another worktree", "Second idea"],
            "the externally appended entry keeps its position"
        );
        assert_on_disk(&path, &["Idea from another worktree"]);
    }

    /// Every mutator starts from the file as it is on disk, never from a copy
    /// read when the store was opened.
    #[test]
    fn every_write_starts_from_the_file_on_disk() {
        let project = project();
        let path = file(&project);
        add_item(&path, "First idea").expect("add");
        let first = id_of(&path, "First idea");

        append_on_disk(&path, "ext-edit", "Appended before the edit", "pending");
        edit_item(&path, &first, "First idea, edited").expect("edit");
        assert_on_disk(&path, &["Appended before the edit", "First idea, edited"]);

        append_on_disk(&path, "ext-defer", "Appended before the defer", "pending");
        set_deferred(&path, &first, true).expect("defer");
        assert_on_disk(&path, &["Appended before the defer"]);

        append_on_disk(&path, "ext-reorder", "Appended before the reorder", "pending");
        let order: Vec<String> = read_items(&path)
            .iter()
            .rev()
            .map(|item| item.id.clone())
            .collect();
        reorder_items(&path, &order);
        assert_on_disk(&path, &["Appended before the reorder"]);

        append_on_disk(&path, "ext-remove", "Appended before the remove", "pending");
        remove_item(&path, &first).expect("remove");
        assert_on_disk(&path, &["Appended before the remove"]);

        append_on_disk(&path, "ext-done", "Finished elsewhere", "done");
        append_on_disk(&path, "ext-clear", "Appended before the clear", "pending");
        clear_finished(&path);
        assert_on_disk(&path, &["Appended before the clear"]);
    }

    #[test]
    fn edit_updates_the_text_and_keeps_the_attachment() {
        let project = project();
        let path = file(&project);
        // The command surface has no image upload, so seed the attachment
        // through the store itself: the data URL lands in a sidecar file.
        let mut store = BacklogStore::open(path.clone());
        let seeded = store.add("Idea with a picture".to_string(), vec![PNG_DATA_URL.to_string()]);
        assert_eq!(seeded.images.len(), 1, "the attachment was stored");
        let sidecar = project
            .path()
            .join(".coding")
            .join(&seeded.images[0]);
        assert!(sidecar.is_file(), "sidecar image at {}", sidecar.display());

        let items = edit_item(&path, &seeded.id, "Idea with a picture, edited").expect("edit");

        assert_eq!(items[0].text, "Idea with a picture, edited");
        assert_eq!(
            items[0].images, seeded.images,
            "the attachment reference is kept"
        );
        assert!(
            sidecar.is_file(),
            "the sidecar image survives the edit: {}",
            sidecar.display()
        );
    }

    #[test]
    fn editing_an_unknown_id_is_refused() {
        let project = project();
        let path = file(&project);
        add_item(&path, "Only idea").expect("add");

        let error = edit_item(&path, "missing", "new text").expect_err("unknown id");
        assert!(error.contains("no backlog item with id missing"), "{error}");
        assert_eq!(texts(&read_items(&path)), ["Only idea"]);
    }

    #[test]
    fn remove_soft_deletes_the_item_and_keeps_its_line() {
        let project = project();
        let path = file(&project);
        add_item(&path, "Keep me").expect("add");
        add_item(&path, "Remove me").expect("add");
        let doomed = id_of(&path, "Remove me");

        let items = remove_item(&path, &doomed).expect("remove");
        assert_eq!(texts(&items), ["Keep me"]);
        let stored = std::fs::read_to_string(&path).expect("read the backlog file");
        assert!(
            stored.contains("\"deleted_at\""),
            "the removed line stays on disk as a soft delete: {stored}"
        );

        let error = remove_item(&path, &doomed).expect_err("a removed id is gone");
        assert!(error.contains("no backlog item"), "{error}");
    }

    #[test]
    fn reorder_resequences_the_queue() {
        let project = project();
        let path = file(&project);
        for text in ["A", "B", "C"] {
            add_item(&path, text).expect("add");
        }
        let c = id_of(&path, "C");
        let a = id_of(&path, "A");

        let items = reorder_items(&path, &[c, a]);
        assert_eq!(
            texts(&items),
            ["C", "A", "B"],
            "listed ids first in order, the rest after"
        );
    }

    #[test]
    fn deferring_keeps_the_item_in_the_queue() {
        let project = project();
        let path = file(&project);
        add_item(&path, "A").expect("add");
        add_item(&path, "B").expect("add");
        let a = id_of(&path, "A");

        let items = set_deferred(&path, &a, true).expect("defer");
        assert!(items[0].deferred);
        assert_eq!(texts(&items), ["A", "B"], "a deferred idea stays visible");

        let items = set_deferred(&path, &a, false).expect("undefer");
        assert!(!items[0].deferred);

        let error = set_deferred(&path, "missing", true).expect_err("unknown id");
        assert!(error.contains("no backlog item"), "{error}");
    }

    #[test]
    fn clear_finished_drops_finished_ideas_and_keeps_pending_ones() {
        let project = project();
        let path = file(&project);
        add_item(&path, "Still pending").expect("add");
        append_on_disk(&path, "done-item", "Finished idea", "done");

        let items = clear_finished(&path);
        assert_eq!(texts(&items), ["Still pending"]);
        let stored = std::fs::read_to_string(&path).expect("read the backlog file");
        assert!(
            stored.contains("Finished idea"),
            "the cleared line stays as a soft delete: {stored}"
        );
    }
}
