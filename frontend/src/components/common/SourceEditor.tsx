// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * SourceEditor — THE shared file content viewer/editor, used by the Files
 * tab (FileViewer's bottom pane) and the Graph tab's browse-to-source pane.
 *
 * One component owns the whole editing experience so both surfaces are
 * literally the same editor:
 * - Markdown (.md/.markdown/.mdx) renders via the shared <Markdown> stack;
 *   the Edit toggle opens the toolbar (lib/mdEdit transforms) + split live
 *   preview, with real persistence via the sandboxed `write_file` IPC
 *   (Save / Ctrl+S, line-ending preserving via normalizeForSave).
 * - Other text/code files render syntax-highlighted through the same
 *   Markdown/rehype-highlight pipeline (wrapCodeFence + CodeBlock).
 * - Binary extensions show a "not a text file" placeholder.
 * - Browse-picked files (absolute, outside the project sandbox) are local
 *   preview only and refuse to save.
 *
 * Session persistence is OPT-IN via `persistSession` (the Files tab passes
 * it): state is mirrored into the module-level `editorSession` after every
 * render and restored on mount, so a dirty edit survives the right-panel
 * tab switch unmounting the viewer (review B1, 2026-04-20). Non-persisting
 * consumers (the Graph tab's peek view) get plain ephemeral state — two
 * live instances never fight over the one session.
 *
 * `revealLine` (Graph usage) scrolls the pane to a 1-based line after the
 * content loads — approximate (uniform line-height assumption), measured
 * from the rendered <pre> so font-size changes are honored. No-op for
 * rendered markdown, where line numbers are meaningless.
 */
import { useEffect, useRef, useState } from "react";
import {
  File as FileIcon,
  Edit3,
  Save,
  Check,
  X,
  Braces,
  Bold,
  Italic,
  Code,
  Link2,
  List,
  ListOrdered,
  Heading1,
  Heading2,
  Heading3,
} from "lucide-react";
import { readFile, writeFile, errMsg } from "../../lib/tauri";
import { Markdown } from "../chat/Markdown";
import { CodeBlock } from "../chat/CodeBlock";
import { isMarkdownPath, isTextPath, languageForPath, wrapCodeFence } from "../../lib/language";
import {
  hasUnsavedChanges,
  insertBlock,
  normalizeForSave,
  setLinePrefix,
  wrapSelection,
  type TextSel,
} from "../../lib/mdEdit";

/**
 * The scroll offset (px) that puts 1-based `line` at the top of the pane:
 * `(line - 1) * perLinePx`, clamped to `[0, (totalLines - 1) * perLinePx]`.
 * Pure — extracted so the vitest suite can pin the clamp table.
 */
export function clampRevealScroll(
  line: number,
  totalLines: number,
  perLinePx: number,
): number {
  if (line < 1 || totalLines < 1 || perLinePx <= 0) return 0;
  const target = (line - 1) * perLinePx;
  const max = Math.max(0, (totalLines - 1) * perLinePx);
  return Math.min(target, max);
}

/**
 * Whether a read that resolves AFTER the component unmounted (the B1
 * unmount-straddle) should still write the session. The straddle write is
 * the whole point of the direct session writes: the mirror effect dies with
 * the unmount, so without this the remount restores the PREVIOUS file's
 * body under the new path — and Save could then write that body into the
 * wrong file. The path equality keeps a superseded read (file A's late
 * result landing after the switch to B) from clobbering B's session.
 * Pure — pinned by a table test (review H1, 2026-08-20).
 */
export function shouldPersistStraddledRead(
  sessionPath: string,
  readPath: string,
  disposed: boolean,
  persistSession: boolean,
): boolean {
  return disposed && persistSession && sessionPath === readPath;
}

/** One toolbar action: an icon, a tooltip, and the pure transform it applies. */
interface ToolbarButton {
  icon: typeof Heading1;
  title: string;
  run: (text: string, selStart: number, selEnd: number) => TextSel;
}

/** The markdown formatting toolbar: headings, emphasis, code, links, lists. */
const TOOLBAR_BUTTONS: ToolbarButton[] = [
  { icon: Heading1, title: "Heading 1", run: (t, s, e) => setLinePrefix(t, s, e, "# ") },
  { icon: Heading2, title: "Heading 2", run: (t, s, e) => setLinePrefix(t, s, e, "## ") },
  { icon: Heading3, title: "Heading 3", run: (t, s, e) => setLinePrefix(t, s, e, "### ") },
  { icon: Bold, title: "Bold", run: (t, s, e) => wrapSelection(t, s, e, "**", "**", "bold") },
  { icon: Italic, title: "Italic", run: (t, s, e) => wrapSelection(t, s, e, "*", "*", "italic") },
  { icon: Code, title: "Inline code", run: (t, s, e) => wrapSelection(t, s, e, "`", "`", "code") },
  { icon: Braces, title: "Code block", run: (t, s, e) => insertBlock(t, s, e, "```\n\n```") },
  { icon: List, title: "Bullet list", run: (t, s, e) => setLinePrefix(t, s, e, "- ") },
  { icon: ListOrdered, title: "Numbered list", run: (t, s, e) => setLinePrefix(t, s, e, "1. ") },
  { icon: Link2, title: "Link", run: (t, s, e) => wrapSelection(t, s, e, "[", "](url)", "label") },
];

/**
 * The editor session cache — survives the viewer unmounting when the right
 * panel switches to another tab (only the active tab stays mounted). A
 * `persistSession` SourceEditor mirrors its state here after every render
 * and restores it on mount, so a dirty edit (amber dot, Save button,
 * discard guard) is never silently discarded by a tab switch (review B1,
 * 2026-04-20). App close still loses unsaved edits — the webview can't
 * reliably prompt from beforeunload.
 */
export interface EditorSession {
  /** The active file path ("" = none). */
  path: string;
  /** The Browse-picked absolute path, or null for project files. */
  browsed: string | null;
  /** The current editor text. */
  content: string;
  /** The last loaded/saved text. */
  savedContent: string;
  /** Whether edit mode is active. */
  editing: boolean;
}

// Module-level: one live persisted session (the Files tab's editor).
export const editorSession: EditorSession = {
  path: "",
  browsed: null,
  content: "",
  savedContent: "",
  editing: false,
};

export interface SourceEditorProps {
  /** The file to show (project-relative, forward slashes; "" = none). */
  path: string;
  /** A Browse-picked absolute path (outside the sandbox), or null. */
  browsed?: string | null;
  /**
   * Content arriving WITH a browsed file (the picker returns path + text in
   * one shot; the sandboxed `read_file` cannot read absolute paths). Applied
   * once when the editor switches into that browsed file.
   */
  browsedText?: string | null;
  /** Mirror/restore via the module-level session (the Files tab). */
  persistSession?: boolean;
  /** Reports the dirty flag so the parent can guard file switches. */
  onDirtyChange?: (dirty: boolean) => void;
  /** Renders a close (✕) button on the toolbar — the Graph peek view. */
  onClose?: () => void;
  /** 1-based line to scroll to after the content loads. */
  revealLine?: number | null;
  /**
   * Monotonic "explicit open" counter. Bumped by the parent on EVERY user
   * intent to open a file (tree click, Browse pick, deep link) — including
   * when the target equals the current one, so re-opening the same file
   * re-reads it from disk (the pre-extraction behavior). Without it the
   * effect below only reacts to path/browsed CHANGES, and a confirmed
   * "discard unsaved changes?" on the already-open file would silently keep
   * the edits (review M1, 2026-08-20).
   */
  reloadToken?: number;
}

export function SourceEditor({
  path,
  browsed = null,
  browsedText = null,
  persistSession = false,
  onDirtyChange,
  onClose,
  revealLine = null,
  reloadToken = 0,
}: SourceEditorProps) {
  // Initialized from the session when persisting (see editorSession) so a
  // dirty edit survives the right-panel tab switch unmounting this
  // component (review B1, 2026-04-20); plain empty state otherwise.
  const [content, setContent] = useState<string>(
    persistSession ? editorSession.content : "",
  );
  const [savedContent, setSavedContent] = useState<string>(
    persistSession ? editorSession.savedContent : "",
  );
  const [editing, setEditing] = useState(persistSession ? editorSession.editing : false);
  const [error, setError] = useState<string>("");
  const [saveError, setSaveError] = useState<string>("");
  const [saving, setSaving] = useState(false);
  const taRef = useRef<HTMLTextAreaElement>(null);
  // The pane's scroll container — the revealLine effect scrolls this.
  const paneRef = useRef<HTMLDivElement>(null);
  // Synchronous mirror of `saving` — a second Ctrl+S within the same render
  // frame would still read the stale state value (B2).
  const savingRef = useRef(false);
  // The path this instance showed on its FIRST load effect run — used to
  // skip the re-read when a persisted session is being restored (a dirty
  // session must not be silently reverted from disk).
  const firstPath = useRef<string | null>(null);

  // Mirror the editor state after every render so the session cache always
  // holds the latest values for the next remount (no deps — cheap writes).
  useEffect(() => {
    if (!persistSession) return;
    editorSession.path = path;
    editorSession.content = content;
    editorSession.editing = editing;
    editorSession.browsed = browsed;
    editorSession.savedContent = savedContent;
  });

  // Dirty tracking (trailing-newline-insensitive) reported to the parent so
  // ITS file-switch guards read fresh state. NOT gated on `editing`:
  // leaving edit mode via Done with unsaved changes keeps the dot, the Save
  // button, and the discard guard armed (C1).
  const dirty = hasUnsavedChanges(savedContent, content);
  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  // Load the file whenever the target changes OR an explicit open is
  // requested (reloadToken — every tree click / Browse pick / deep link,
  // even of the same file). The PARENT guards discards BEFORE changing
  // `path`/bumping the token — this effect never prompts.
  useEffect(() => {
    const first = firstPath.current === null;
    const restoring =
      first && persistSession && editorSession.path === path && editorSession.browsed === browsed;
    firstPath.current = path;
    // A persisted session being restored on mount: keep the session's
    // (possibly dirty) content AND its edit mode — re-reading would
    // silently revert it, and kicking out of edit mode would lose the
    // user's place (B1; review L2, 2026-08-20). The early return must stay
    // ABOVE the setEditing(false) reset so only REAL target switches reset.
    if (restoring) return;
    setEditing(false);
    setError("");
    setSaveError("");
    if (path === "") return;
    if (browsed !== null) {
      // Browse-picked file: content arrives via `browsedText` (the picker
      // returns it; the sandboxed read can't). A remount without it falls
      // back to the restored session content (persistSession consumers).
      if (browsedText !== null) {
        setContent(browsedText);
        setSavedContent(browsedText);
        if (persistSession) {
          editorSession.content = browsedText;
          editorSession.savedContent = browsedText;
        }
      }
      return;
    }
    if (!isTextPath(path)) {
      // Binary: show the placeholder (no content read needed).
      setContent("");
      setSavedContent("");
      return;
    }
    let disposed = false;
    readFile(path)
      .then((text) => {
        if (disposed) {
          // Unmount-straddle (B1): the setStates and the mirror effect died
          // with the unmount, so the session still holds the PREVIOUS
          // file's body under this path — persist the read body so the
          // remount shows the right content (without it, Save could write
          // the stale body across files). shouldPersistStraddledRead's path
          // equality skips a superseded read (file A's late result after
          // the switch to B) — B's own read owns the session now.
          if (shouldPersistStraddledRead(editorSession.path, path, true, persistSession)) {
            editorSession.content = text;
            editorSession.savedContent = text;
          }
          return;
        }
        setContent(text);
        setSavedContent(text);
        // Also write the session directly: if the component unmounts while
        // the read is in flight, the setStates no-op and the mirror effect
        // never runs — the remount would restore a stale file under the new
        // path. (B1)
        if (persistSession) {
          editorSession.content = text;
          editorSession.savedContent = text;
        }
      })
      .catch((err) => {
        if (disposed) {
          // Same straddle: clear the session path so a remount shows "no
          // file" rather than a stale body under the failed path.
          if (shouldPersistStraddledRead(editorSession.path, path, true, persistSession)) {
            editorSession.path = "";
          }
          return;
        }
        setError(errMsg(err));
        // On a failed read, clear the session path so a remount shows "no
        // file" rather than a stale body under the new path.
        if (persistSession) editorSession.path = "";
      });
    return () => {
      disposed = true;
    };
    // browsedText is intentionally NOT a dependency: it is a one-shot
    // payload that rides a browsed-file switch (the reloadToken bump re-runs
    // this effect to apply a fresh pick), not a re-edit trigger.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [path, browsed, persistSession, reloadToken]);

  // Scroll to `revealLine` after the content settles — the Graph tab's
  // browse-to-source peek AND the Files tab's read_files deep-links (the
  // tool-card links open the file at the line the read started at).
  // Approximate: uniform line-height within the highlighted <pre>; the
  // per-line height is MEASURED from the rendered code element so the
  // user's font settings are honored. No-op for rendered markdown (line
  // numbers are meaningless there — which also means the edit-mode textarea
  // never renders in this effect's reachable paths; review N1 removed that
  // dead branch).
  //
  // `reloadToken` is a dependency so a RE-click on the same file at the same
  // line re-scrolls: the FileViewer bumps the token on every explicit open,
  // and without it a byte-identical re-read leaves every dep unchanged
  // (React bails) — a user who scrolled away and re-clicked "line 40" would
  // see no scroll. Manual opens reset revealLine to null, so the guard keeps
  // them no-ops.
  useEffect(() => {
    if (revealLine === null || revealLine < 1) return;
    if (isMarkdownPath(path)) return;
    const pane = paneRef.current;
    if (!pane) return;
    const pre = pane.querySelector("pre");
    if (!pre) return;
    const lines = content.split("\n").length;
    const code = pre.querySelector("code") ?? pre;
    const perLine = code.getBoundingClientRect().height / Math.max(1, lines);
    pane.scrollTop = clampRevealScroll(revealLine, lines, perLine);
  }, [content, revealLine, path, reloadToken]);

  /** Persist the edit through the sandboxed `write_file` IPC. */
  async function handleSave() {
    // Ref-based re-entrancy guard: a second Ctrl+S inside the same render
    // frame must not start a concurrent write (B2). Also no-op when clean.
    if (savingRef.current || !dirty || path === "") return;
    if (browsed !== null) {
      setSaveError(
        "Cannot save a file outside the project — edits are local preview only.",
      );
      return;
    }
    savingRef.current = true;
    setSaving(true);
    try {
      // Preserve the file's dominant line-ending style (CRLF stays CRLF).
      const normalized = normalizeForSave(savedContent, content);
      await writeFile(path, normalized);
      setContent(normalized);
      setSavedContent(normalized);
      setSaveError("");
    } catch (err) {
      setSaveError(errMsg(err));
    } finally {
      savingRef.current = false;
      setSaving(false);
    }
  }

  /** Apply an mdEdit transform to the editor textarea, restoring selection. */
  function applyTransform(
    fn: (text: string, selStart: number, selEnd: number) => TextSel,
  ) {
    const ta = taRef.current;
    if (!ta) return;
    const r = fn(ta.value, ta.selectionStart, ta.selectionEnd);
    setContent(r.text);
    // Restore after React re-renders the textarea with the new value.
    requestAnimationFrame(() => {
      ta.focus();
      ta.setSelectionRange(r.selStart, r.selEnd);
    });
  }

  /** Ctrl/Cmd+S inside the editor saves — never the browser's page save. */
  function onEditorKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if ((e.ctrlKey || e.metaKey) && (e.key === "s" || e.key === "S")) {
      e.preventDefault();
      if (browsed === null) {
        void handleSave();
      } else {
        setSaveError(
          "Cannot save a file outside the project — edits are local preview only.",
        );
      }
    }
  }

  const lang = languageForPath(path);
  const isMd = isMarkdownPath(path);
  const isBinary = path !== "" && !isTextPath(path);

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* Compact toolbar row: file identity + edit actions (+ ✕ for the
          Graph peek view). The Files tab's outer header keeps the tree
          controls; THIS row owns the editor controls so both surfaces show
          literally the same editor chrome. */}
      <div className="flex shrink-0 items-center justify-between gap-2 border-b border-border px-3 py-1.5">
        <div className="flex min-w-0 items-center gap-2">
          <FileIcon className="h-3.5 w-3.5 shrink-0 text-slate-400" />
          <span className="min-w-0 truncate text-[0.75em] text-slate-300" title={path}>
            {path || "No file selected"}
          </span>
          {revealLine !== null && revealLine >= 1 && (
            <span className="shrink-0 text-[0.65rem] text-slate-500">:L{revealLine}</span>
          )}
          {dirty && (
            <span
              className="h-1.5 w-1.5 shrink-0 rounded-full bg-amber-400"
              title="Unsaved changes"
            />
          )}
          {path && lang && (
            <span className="shrink-0 rounded bg-bg-tertiary px-1.5 py-0.5 text-[0.65rem] uppercase tracking-wide text-slate-500">
              {lang}
            </span>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {path && isMd && !isBinary && browsed === null && (editing || dirty) && (
            <button
              onClick={() => void handleSave()}
              disabled={saving || !dirty}
              title="Save (Ctrl+S)"
              className={`flex items-center gap-1 text-[0.75em] ${
                dirty ? "text-cyan-400 hover:text-cyan-300" : "text-slate-500"
              } disabled:cursor-default disabled:opacity-50`}
            >
              <Save className="h-3.5 w-3.5" />
              {saving ? "Saving…" : "Save"}
            </button>
          )}
          {path && isMd && !isBinary && (
            <button
              onClick={() => setEditing(!editing)}
              className="flex items-center gap-1 text-[0.75em] text-slate-500 hover:text-slate-300"
            >
              {editing ? <Check className="h-3.5 w-3.5" /> : <Edit3 className="h-3.5 w-3.5" />}
              {editing ? "Done" : "Edit"}
            </button>
          )}
          {onClose && (
            <button
              onClick={onClose}
              aria-label="Close source view"
              title="Back to the memory-access log"
              className="flex items-center text-slate-500 hover:text-slate-300"
            >
              <X className="h-3.5 w-3.5" />
            </button>
          )}
        </div>
      </div>

      {/* Content: the shared viewer/editor body. */}
      <div ref={paneRef} className="flex-1 overflow-y-auto p-3">
        {saveError && (
          <div className="mb-2 text-[0.8em] text-red-400">{saveError}</div>
        )}
        {error ? (
          <div className="text-[0.875em] text-red-400">{error}</div>
        ) : !path ? (
          <div className="text-[0.875em] text-slate-500">No file selected.</div>
        ) : isBinary ? (
          <div className="text-[0.875em] text-slate-500">
            Not a text file — no preview available for <span className="font-mono">{path}</span>.
          </div>
        ) : editing && isMd ? (
          <div className="flex h-full min-h-0 flex-col gap-1">
            {browsed !== null && (
              <div className="shrink-0 text-[0.7em] text-slate-500">
                Local preview only — files outside the project can't be saved.
              </div>
            )}
            {/* Formatting toolbar: each button applies one lib/mdEdit transform. */}
            <div className="flex shrink-0 flex-wrap items-center gap-0.5 rounded border border-border bg-bg-secondary px-1 py-0.5">
              {TOOLBAR_BUTTONS.map((b) => (
                <button
                  key={b.title}
                  onClick={() => applyTransform(b.run)}
                  title={b.title}
                  className="flex items-center rounded px-1.5 py-0.5 text-slate-400 hover:bg-bg-tertiary hover:text-slate-200"
                >
                  <b.icon className="h-3.5 w-3.5" />
                </button>
              ))}
            </div>
            {/* Split editor: raw markdown left, live rendered preview right. */}
            <div className="flex min-h-0 flex-1 gap-2">
              <textarea
                ref={taRef}
                value={content}
                onChange={(e) => setContent(e.target.value)}
                onKeyDown={onEditorKeyDown}
                spellCheck={false}
                className="min-w-0 flex-1 resize-none rounded border border-border bg-bg-primary p-2 font-mono text-[0.75em] leading-relaxed text-slate-300 focus:outline-none"
              />
              <div className="prose prose-invert prose-sm min-w-0 max-w-none flex-1 overflow-y-auto rounded border border-border bg-bg-primary p-3 prose-pre:bg-bg-primary prose-h1:border-b prose-h1:border-border prose-h1:pb-1 prose-h2:border-b prose-h2:border-border/60 prose-h2:pb-1">
                <Markdown>{content}</Markdown>
              </div>
            </div>
          </div>
        ) : isMd ? (
          <div className="prose prose-invert prose-sm max-w-none prose-pre:bg-bg-primary prose-h1:border-b prose-h1:border-border prose-h1:pb-1 prose-h2:border-b prose-h2:border-border/60 prose-h2:pb-1">
            <Markdown>{content}</Markdown>
          </div>
        ) : (
          // Any other text file: wrap in a fenced code block so the shared
          // Markdown/rehype-highlight stack syntax-highlights it.
          <div className="prose prose-invert prose-sm max-w-none prose-pre:m-0 prose-pre:bg-bg-primary">
            <Markdown
              components={{
                code({ className, children, ...props }) {
                  const isInline = !className;
                  if (isInline) {
                    return (
                      <code className="inline-code rounded bg-bg-tertiary px-1.5 py-0.5 text-xs" {...props}>
                        {children}
                      </code>
                    );
                  }
                  return <CodeBlock className={className}>{children}</CodeBlock>;
                },
              }}
            >
              {wrapCodeFence(content, lang)}
            </Markdown>
          </div>
        )}
      </div>
    </div>
  );
}
