## Verdict: FINDINGS (0 high, 1 low)

Review of plan 952aa10b ("Backlog: edit images on existing items") — all uncommitted changes on wt/agenticcoder. The feature works correctly: the editor persists the edited image set, cancel restores it, paste/drop are image-filtered, Enter/Escape still work, and all five regression tests are real (each assertion matches post-fix source and fails pre-fix). One low-severity UI conflict found.

## Finding 1 (low): static display thumbnails render during edit, duplicating and contradicting the editor strip

**File:** `frontend/src/components/views/BacklogView.tsx` (lines 649–665)

The `editing ? <editor> : <display text>` ternary ends at line 647, but the "Image thumbnails" block (`{item.images.length > 0 && (...)}`) sits *outside* it — so it renders unconditionally, including while the inline editor is open. With this change that is a real conflict:

- An item with attachments shows **two** strips while editing: the new removable `editImages` strip (h-14, lines 597–619) plus the static `item.images` strip (h-10) directly below.
- Removing an image in the editor strip leaves it visible in the static strip until save — the remove affordance reads as broken (the image is still on screen).
- Newly pasted/dropped images appear only in the editor strip, so the two strips show different sets at different sizes.

No persistence bug (save writes `editImages`, cancel restores, backend accepts the result), but the duplicated/stale strip undermines the feature's UX.

**Fix:** gate the display strip on `!editing` — e.g. wrap it in `{!editing && item.images.length > 0 && (...)}`, or move it inside the non-editing branch of the ternary above it.

## Checks performed (pass)

**Correctness of persistence.** `handleSaveEdit` calls `backlogEdit(item.id, editText, editImages)` (line 426) and no-ops only when both text and images are empty — matching the button's disabled condition (`!editText.trim() && editImages.length === 0`, line 630). Backend `backlog_edit` (`src-tauri/src/ipc/backlog_cmds.rs:168`) passes text/images through `BacklogStore::edit` with no non-empty-text validation, so image-only saves (empty text + attachments) persist correctly; the frontend guard also fixes a latent pre-fix edge where Enter with empty text would have saved an empty prompt.

**Cancel / reopen.** `handleCancelEdit` (lines 433–437) resets both `editText` and `editImages` from the item; the sync effect (lines 358–363, deps `[item.text, item.images, editing]`) also re-syncs whenever not editing, and its `if (!editing)` guard prevents clobbering an in-progress edit when the item prop changes. No stale-closure or lost-update hazard: `addEditImageFiles` uses functional `setEditImages((prev) => ...)` with stable deps (`[]`); `removeEditImage` uses `filter` (no mutation of the store's array reference).

**Keyboard flow.** Enter (no shift) saves via the unchanged `onKeyDown` — including image-only items (guard passes on `editImages.length > 0`). Escape cancels and now also discards the image edits. Both verified in the source.

**Paste/drop.** `handleEditPaste` preventDefaults only when the clipboard contains images (plain-text paste still works) and both handlers filter to `image/*` via `addEditImageFiles`. Mixed text+image paste drops the text portion — identical to the pre-existing `BacklogInput` behavior, so not new. `onPaste`/`onDrop`/`onDragOver` are on the editor container, so drops onto the textarea or strip both attach.

**Regression tests.** `frontend/src/components/views/BacklogView.test.ts` — all five assertions match exact post-fix source strings (`backlogEdit(item.id, editText, editImages)` line 426; negative `item.images` form absent; `removeEditImage(i)` line 610; `editImages.map((dataUrl, i)` line 599; `onPaste={handleEditPaste}`/`onDrop={handleEditDrop}` lines 573–574; `setEditImages(item.images)` lines 361 & 435; `disabled={!editText.trim() && editImages.length === 0}` line 630). Each fails against pre-fix source (`editImages` did not exist; save was `disabled={!editText.trim()}`). Style matches the file's existing `?raw` vitest pattern (node env, no React DOM).

**Documentation sync.** README.md line 74 describes the Backlog tab at a feature level only and never enumerated editor capabilities; PLAN.md documents backlog architecture/format, not per-card UI — nothing stale. The extracted `fileToDataUrl` carries an updated module doc comment, and the editor placeholder now advertises image attach. No doc updates required.

**Multi-platform neutrality.** Pure web APIs only (`FileReader`, `clipboardData`, `dataTransfer`) — no platform-specific code; TSX-only change.

**Security.** Data URLs are rendered via `<img src>` only (no `dangerouslySetInnerHTML`, no eval, no paths) — same exposure as the pre-existing display strip and `BacklogInput`. No new attack surface.
