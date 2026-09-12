// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * The unified File viewer: the project file tree (top pane) merged with the
 * content viewer/editor (bottom pane), separated by a drag-to-resize handle.
 *
 * Replaces the former separate "Markdown" (MdViewer) and "Files"
 * (FileBrowser) tabs. Clicking a file in the tree displays it below via the
 * SHARED SourceEditor (components/common/SourceEditor.tsx) — the same
 * component the Graph tab's browse-to-source pane uses, so both surfaces
 * are literally the same editor: markdown rendering + the Edit toggle's
 * toolbar/split-preview/persist flow, syntax-highlighted code for other
 * text files, and a binary placeholder. SourceEditor also owns the editor
 * session cache (a dirty edit survives this tab unmounting) and reports the
 * dirty flag back so the tree's file-switch guard can prompt.
 *
 * Files picked via Browse (outside the project sandbox) are local-preview
 * only and say so. Tool cards elsewhere in the app dispatch a store-held
 * pending path (`lib/openFile.ts`); the listener below loads that file here
 * (the caller also opens the right panel + selects this tab).
 */
import { useEffect, useRef, useState, useCallback } from "react";
import {
  Folder,
  File as FileIcon,
  ChevronRight,
  ChevronDown,
  Loader2,
  RefreshCw,
  FolderOpen,
} from "lucide-react";
import {
  listFiles,
  browseMarkdownFile,
  getSettings,
  errMsg,
} from "../../lib/tauri";
import type { FileEntry } from "../../lib/types";
import { useAgentStore } from "../../hooks/useAgentStore";
import { SourceEditor, editorSession } from "../common/SourceEditor";
import { isTextPath } from "../../lib/language";

/** Key for the directory-contents cache. Root is the empty string. */
function dirKey(dir: string | undefined): string {
  return dir ?? "";
}

/** Min/max height (px) for the tree pane. */
const TREE_MIN = 60;

export function FileViewer() {
  // ── Tree state ─────────────────────────────────────────────────────────
  const [rootEntries, setRootEntries] = useState<FileEntry[]>([]);
  const [dirCache, setDirCache] = useState<Map<string, FileEntry[]>>(new Map());
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState<Set<string>>(new Set());
  // Directory names to hide (the `[markdown].skip_dirs` setting, repurposed
  // for the merged file tree — formerly only the MdViewer dropdown used it).
  const [skipDirs, setSkipDirs] = useState<Set<string>>(new Set());

  // ── Content state ──────────────────────────────────────────────────────
  // Initialized from the module-level editor session (see editorSession
  // below) so a dirty edit survives the right-panel tab switch unmounting
  // this component (review B1, 2026-04-20).
  const [path, setPath] = useState<string>(editorSession.path);
  // A file loaded via the Browse button (absolute path outside the sandbox)
  // — plus its content: the picker returns path + text in one shot (the
  // sandboxed read can't load absolute paths), so SourceEditor consumes the
  // injected text when it switches into a browsed file.
  const [browsed, setBrowsed] = useState<string | null>(editorSession.browsed);
  const [browsedText, setBrowsedText] = useState<string | null>(null);
  // Picker/transport failures surface in a thin strip under the header
  // (content-area errors are SourceEditor's own).
  const [error, setError] = useState<string>("");
  // Monotonic "explicit open" counter — bumped on EVERY user intent to open
  // (tree click / Browse / deep link), including re-opening the SAME file,
  // so SourceEditor re-reads it (its load effect keys on this token; a pure
  // path change alone would no-op and a confirmed "discard?" would silently
  // keep the edits — review M1, 2026-08-20).
  const [reloadToken, setReloadToken] = useState(0);
  // The 1-based line a deep-link open (a read_files tool-card link) should
  // scroll the editor to, consumed by SourceEditor's revealLine. Set per
  // open (every manual open resets it to null) so a stale line never
  // re-scrolls a later file.
  const [revealLine, setRevealLine] = useState<number | null>(null);

  /**
   * Record an explicit open: point at the target + force a (re)load. `line`
   * is the 1-based deep-link reveal line (null = top of file); every open
   * resets it so a stale reveal never re-scrolls a later file.
   */
  function openTarget(
    p: string,
    browsedPath: string | null,
    text: string | null,
    line: number | null = null,
  ) {
    setPath(p);
    setBrowsed(browsedPath);
    setBrowsedText(text);
    setError("");
    setRevealLine(line);
    setReloadToken((t) => t + 1);
  }

  // The editor's dirty flag, reported by SourceEditor (onDirtyChange) and
  // held in a ref so the file-switch guards below read fresh state from
  // stable callbacks. NOT gated on edit mode: leaving edit mode via Done
  // with unsaved changes keeps the discard guard armed (C1).
  const dirtyRef = useRef(false);
  const onDirtyChange = useCallback((d: boolean) => {
    dirtyRef.current = d;
  }, []);

  /** Ask before dropping unsaved editor changes (file switch / Browse). */
  function confirmDiscard(): boolean {
    if (!dirtyRef.current) return true;
    return window.confirm("Discard unsaved changes to the current file?");
  }

  // ── Tree pane height (drag-to-resize) ──────────────────────────────────
  const [treeHeight, setTreeHeight] = useState<number>(160);
  const containerRef = useRef<HTMLDivElement>(null);
  const dragCleanup = useRef<(() => void) | null>(null);

  // Remove in-flight drag listeners if the component unmounts mid-drag.
  useEffect(() => {
    return () => {
      dragCleanup.current?.();
      dragCleanup.current = null;
    };
  }, []);

  const loadRoot = useCallback(async () => {
    try {
      setRootEntries(await listFiles(undefined));
    } catch (e) {
      console.error("failed to list root files:", e);
    }
  }, []);

  useEffect(() => {
    loadRoot();
  }, [loadRoot]);

  // Load the skip-directory setting once (filter applied client-side so it
  // matches the old Markdown dropdown's behavior).
  useEffect(() => {
    getSettings()
      .then((s) => setSkipDirs(new Set(s.markdown?.skip_dirs ?? [])))
      .catch(() => setSkipDirs(new Set()));
  }, []);

  /** True when an entry should be hidden from the tree (skip-dir filter). */
  const isHidden = useCallback(
    (entry: FileEntry) => entry.is_dir && skipDirs.has(entry.name),
    [skipDirs],
  );

  /** Load a directory's children (if not cached) and toggle expansion. */
  async function toggleDir(entry: FileEntry) {
    const key = entry.path;
    const nextExpanded = new Set(expanded);
    if (nextExpanded.has(key)) {
      nextExpanded.delete(key);
      setExpanded(nextExpanded);
      return;
    }
    nextExpanded.add(key);
    setExpanded(nextExpanded);
    if (dirCache.has(key)) return;
    const nextLoading = new Set(loading);
    nextLoading.add(key);
    setLoading(nextLoading);
    try {
      const children = await listFiles(key);
      setDirCache((prev) => {
        const updated = new Map(prev);
        updated.set(key, children);
        return updated;
      });
    } catch (e) {
      console.error(`failed to list directory "${key}":`, e);
    } finally {
      const afterLoading = new Set(loading);
      afterLoading.delete(key);
      setLoading(afterLoading);
    }
  }

  /**
   * Point the editor at a project-relative file (SourceEditor loads it).
   * `line` is the optional 1-based deep-link reveal line (read_files links).
   */
  const loadFile = useCallback((p: string, line?: number | null) => {
    if (!confirmDiscard()) return;
    openTarget(p, null, null, line ?? null);
  }, []);

  function openEntry(entry: FileEntry) {
    if (entry.is_dir) {
      void toggleDir(entry);
    } else if (isTextPath(entry.path)) {
      loadFile(entry.path);
    } else {
      if (!confirmDiscard()) return;
      // Binary: SourceEditor detects the extension and shows the placeholder
      // (no content read needed).
      openTarget(entry.path, null, null);
    }
  }

  // Consume a pending file-open request from the store (set by tool-card file
  // links via openFileInViewer → requestFileOpen). Held in the store — not a
  // fire-and-forget event — so it works even when this viewer mounted AFTER
  // the click (the default: the Files tab starts disabled/unmounted). Runs on
  // mount and whenever the pending path changes.
  const pendingFileOpen = useAgentStore((s) => s.pendingFileOpen);
  useEffect(() => {
    if (pendingFileOpen === null) return;
    loadFile(pendingFileOpen.path, pendingFileOpen.line);
    useAgentStore.getState().clearPendingFileOpen();
  }, [pendingFileOpen, loadFile]);

  /** Open the native file picker for an .md file (may live outside the sandbox). */
  async function handleBrowse() {
    if (!confirmDiscard()) return;
    setError("");
    try {
      const result = await browseMarkdownFile();
      if (!result) return;
      // Direct session writes — the awaited dialog may have straddled an
      // unmount, in which case the setStates above no-op (B1); SourceEditor
      // also applies the injected text on the live instance (the
      // reloadToken bump forces the re-read even for a re-pick of the SAME
      // absolute file — review M1).
      editorSession.content = result.content;
      editorSession.savedContent = result.content;
      editorSession.path = result.path;
      editorSession.browsed = result.path;
      editorSession.editing = false;
      openTarget(result.path, result.path, result.content);
    } catch (err) {
      setError(errMsg(err));
    }
  }

  // ── Splitter drag (vertical: the tree is above the content) ────────────
  function onSplitterDown(e: React.PointerEvent<HTMLDivElement>) {
    e.preventDefault();
    const startY = e.clientY;
    const startHeight = treeHeight;
    const total = containerRef.current?.getBoundingClientRect().height ?? 600;
    const max = total * 0.7;
    const handle = e.currentTarget;
    handle.setPointerCapture(e.pointerId);
    const clamp = (px: number) => Math.round(Math.max(TREE_MIN, Math.min(max, px)));

    const onMove = (ev: PointerEvent) => {
      // Dragging down grows the tree pane (it sits at the top).
      setTreeHeight(clamp(startHeight + (ev.clientY - startY)));
    };
    const endDrag = (ev: PointerEvent) => {
      try {
        handle.releasePointerCapture(ev.pointerId);
      } catch {
        // Capture may already be released (pointercancel) — ignore.
      }
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", endDrag);
      window.removeEventListener("pointercancel", endDrag);
      dragCleanup.current = null;
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", endDrag);
    window.addEventListener("pointercancel", endDrag);
    dragCleanup.current = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", endDrag);
      window.removeEventListener("pointercancel", endDrag);
    };
  }

  /** Render one tree entry, recursing into expanded directories. */
  function renderEntry(entry: FileEntry, depth: number): React.ReactNode {
    const isExpanded = expanded.has(entry.path);
    const isLoading = loading.has(entry.path);
    const children = dirCache.get(entry.path) ?? [];
    const selected = !entry.is_dir && entry.path === path;
    return (
      <div key={entry.path}>
        <button
          onClick={() => openEntry(entry)}
          className={`flex w-full items-center gap-1.5 rounded px-2 py-0.5 text-left text-xs hover:bg-bg-tertiary ${
            selected ? "bg-bg-tertiary text-cyan-400" : "text-slate-300"
          }`}
          style={{ paddingLeft: `${depth * 12 + 8}px` }}
        >
          {entry.is_dir ? (
            <>
              {isLoading ? (
                <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin text-slate-500" />
              ) : isExpanded ? (
                <ChevronDown className="h-3.5 w-3.5 shrink-0 text-slate-500" />
              ) : (
                <ChevronRight className="h-3.5 w-3.5 shrink-0 text-slate-500" />
              )}
              <Folder className="h-3.5 w-3.5 shrink-0 text-blue-400" />
            </>
          ) : (
            <>
              <span className="w-3.5 shrink-0" />
              <FileIcon className="h-3.5 w-3.5 shrink-0 text-slate-500" />
            </>
          )}
          <span className="truncate">{entry.name}</span>
        </button>
        {entry.is_dir && isExpanded && (
          <div>
            {isLoading && children.length === 0 ? (
              <div className="px-2 py-1 text-xs text-slate-500" style={{ paddingLeft: `${(depth + 1) * 12 + 8}px` }}>
                loading…
              </div>
            ) : children.length === 0 ? (
              <div className="px-2 py-1 text-xs text-slate-600" style={{ paddingLeft: `${(depth + 1) * 12 + 8}px` }}>
                (empty)
              </div>
            ) : (
              children.filter((c) => !isHidden(c)).map((child) => renderEntry(child, depth + 1))
            )}
          </div>
        )}
      </div>
    );
  }

  return (
    <div ref={containerRef} className="flex h-full flex-col">
      {/* Header: tree actions only. The active file's identity (path, dirty
          dot, language chip, Save/Edit) lives on SourceEditor's own toolbar
          below the splitter — the same chrome the Graph tab shows — so it
          is not duplicated here (review N3, 2026-08-20). */}
      <div className="flex items-center justify-end gap-2 border-b border-border px-3 py-1.5">
        <button
          onClick={() => void handleBrowse()}
          title="Browse for a .md file (outside the project)"
          className="flex items-center text-slate-500 hover:text-slate-300"
        >
          <FolderOpen className="h-3.5 w-3.5" />
        </button>
        <button
          onClick={() => void loadRoot()}
          title="Refresh file list"
          className="flex items-center text-slate-500 hover:text-slate-300"
        >
          <RefreshCw className="h-3.5 w-3.5" />
        </button>
      </div>

      {error && (
        <div className="border-b border-border bg-red-950/40 px-3 py-1 text-[0.8em] text-red-400">
          {error}
        </div>
      )}

      {/* Top pane: the file tree (resizable height). */}
      <div className="shrink-0 overflow-y-auto px-1 py-1" style={{ height: treeHeight }}>
        {rootEntries.length === 0 ? (
          <div className="px-2 text-xs text-slate-500">No files.</div>
        ) : (
          rootEntries.filter((e) => !isHidden(e)).map((entry) => renderEntry(entry, 0))
        )}
      </div>

      {/* Drag handle between the tree and the content. Carries the 1px
          border-y hairlines that flank the grip pill and separate the two
          panes even at rest — the same motif as the App.tsx ResizeHandle
          (border-x) and the InflightBar handle. With border-box sizing the
          6px strip reads as 1px line + 1px gap + 2px pill + 1px gap + 1px
          line. The strip paints the chrome-band background (bg-bg-secondary)
          — the lighter seam the horizontal bars always showed, which the
          vertical handle now matches (see resizeHandleMotif.test.ts). */}
      <div
        onPointerDown={onSplitterDown}
        role="separator"
        aria-orientation="horizontal"
        aria-label="Resize file tree"
        title="Drag to resize the file tree"
        className="group flex h-1.5 shrink-0 cursor-row-resize items-center justify-center border-y border-border bg-bg-secondary transition-colors hover:bg-cyan-500/20"
      >
        <div className="h-0.5 w-8 rounded-full bg-slate-500 group-hover:bg-slate-400" />
      </div>

      {/* Bottom pane: the shared content viewer/editor (SourceEditor) — the
          same component the Graph tab's browse-to-source pane renders. It
          owns the editor toolbar row (path + dirty dot + language chip +
          Save/Edit), the session cache, and all load/save logic. */}
      <div className="min-h-0 flex-1">
        <SourceEditor
          path={path}
          browsed={browsed}
          browsedText={browsedText}
          persistSession
          onDirtyChange={onDirtyChange}
          reloadToken={reloadToken}
          revealLine={revealLine}
        />
      </div>
    </div>
  );
}
