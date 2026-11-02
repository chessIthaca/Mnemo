// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * The Files tab's data access, tree model and syntax highlighting.
 *
 * The tree is lazy: the tab asks the backend for one directory at a time and
 * remembers the answer, so opening a project costs one listing, not a walk.
 * Paths are always project-relative; the backend refuses anything that leaves
 * the project, so this module never reinterprets a path it is given.
 */

import { invoke } from "@tauri-apps/api/core";
import DOMPurify from "dompurify";
import hljs from "highlight.js/lib/common";
import { isTauriRuntime } from "./runtime";

/** What a tree row is: a directory (expanded on demand) or a file. */
export type FileEntryKind = "dir" | "file";

/** One row of the project tree. */
export interface FileEntry {
  /** Project-relative, `/`-separated path. */
  path: string;
  /** The last path segment, which the row shows. */
  name: string;
  kind: FileEntryKind;
}

/** One directory's immediate children — the tree's lazy unit. */
export interface DirListing {
  /** The listed directory; an empty string is the project root. */
  path: string;
  entries: FileEntry[];
  /** True when the backend cut a very large directory short. */
  truncated: boolean;
}

/** What the read-only viewer shows for one file. */
export interface FileContent {
  path: string;
  /** The file's text; empty when `binary` is set. */
  text: string;
  /** The size on disk, which may exceed `text`. */
  bytes: number;
  /** True when the viewer shows only the beginning of the file. */
  truncated: boolean;
  /** True when the file is not text and is deliberately not shown. */
  binary: boolean;
}

/** The IPC surface the Files tab needs. */
export interface FilesTransport {
  /** List one project directory, one level deep. */
  list(path: string): Promise<DirListing>;
  /** Read one project file for the viewer. */
  read(path: string): Promise<FileContent>;
  /** Find project paths containing `query`. */
  find(query: string): Promise<FileEntry[]>;
}

/** The dev-only preview's stand-in transport; never set in production. */
let previewSeed: FilesTransport | null = null;

/**
 * Point the Files tab at fixture data for the dev-only preview page
 * (`npm run check:no-mock` keeps this out of the production entry).
 */
export function seedFilesPreview(transport: FilesTransport | null): void {
  previewSeed = transport;
}

/** Build the Files tab's transport, or `null` outside the desktop app. */
export function createFilesTransport(): FilesTransport | null {
  if (previewSeed) return previewSeed;
  if (!isTauriRuntime()) return null;
  return {
    list: (path: string) => invoke<DirListing>("files_list", { path }),
    read: (path: string) => invoke<FileContent>("files_read", { path }),
    find: (query: string) => invoke<FileEntry[]>("files_find", { query }),
  };
}

/** One row as the tree renders it: the entry, its indent and its open state. */
export interface TreeRow {
  entry: FileEntry;
  /** How deep the row sits; the root's children are 0. */
  depth: number;
  /** True for an expanded directory. */
  expanded: boolean;
}

/**
 * Flatten the listings the tab has already fetched into the rows it renders:
 * an expanded directory's children follow it, indented one level further.
 *
 * Directories whose listing has not arrived yet contribute no children, which
 * is exactly how a lazy tree behaves while it loads.
 */
export function visibleRows(
  listings: Record<string, FileEntry[]>,
  expanded: Record<string, boolean>,
): TreeRow[] {
  const rows: TreeRow[] = [];
  const walk = (path: string, depth: number) => {
    for (const entry of listings[path] ?? []) {
      const isOpen = entry.kind === "dir" && expanded[entry.path] === true;
      rows.push({ entry, depth, expanded: isOpen });
      if (isOpen) walk(entry.path, depth + 1);
    }
  };
  walk("", 0);
  return rows;
}

/** Render a byte count for the viewer's header. */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** The highlight.js grammar ids this repository's files use. */
const LANGUAGE_BY_EXTENSION: Record<string, string> = {
  rs: "rust",
  ts: "typescript",
  tsx: "typescript",
  mts: "typescript",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  svelte: "xml",
  html: "xml",
  xml: "xml",
  svg: "xml",
  css: "css",
  scss: "scss",
  json: "json",
  md: "markdown",
  markdown: "markdown",
  toml: "ini",
  ini: "ini",
  cfg: "ini",
  conf: "ini",
  yaml: "yaml",
  yml: "yaml",
  sh: "bash",
  bash: "bash",
  zsh: "bash",
  ps1: "powershell",
  bat: "dos",
  cmd: "dos",
  py: "python",
  go: "go",
  java: "java",
  c: "c",
  h: "c",
  cc: "cpp",
  cpp: "cpp",
  hpp: "cpp",
  cs: "csharp",
  rb: "ruby",
  php: "php",
  sql: "sql",
  diff: "diff",
  patch: "diff",
};

/** The highlight.js language id for a path, or `"text"` when none fits. */
export function languageFor(path: string): string {
  const name = path.split("/").pop() ?? path;
  const lower = name.toLowerCase();
  if (lower === "dockerfile") return "dockerfile";
  if (lower === "makefile") return "makefile";
  if (!lower.includes(".")) return "text";
  const extension = lower.slice(lower.lastIndexOf(".") + 1);
  return LANGUAGE_BY_EXTENSION[extension] ?? "text";
}

/** Files longer than this are shown unhighlighted: the grammar pass would stall. */
const HIGHLIGHT_LIMIT = 120_000;

/** Escape text for insertion into HTML. */
function escapeHtml(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

/**
 * Render file text as sanitized, highlighted HTML for the viewer.
 *
 * Highlighting is best-effort: an unknown grammar, a broken parse or a very
 * large file falls back to escaped plain text, which always renders.
 */
export function highlightFile(text: string, path: string): string {
  const language = languageFor(path);
  if (text.length <= HIGHLIGHT_LIMIT && language !== "text" && hljs.getLanguage(language)) {
    try {
      return DOMPurify.sanitize(hljs.highlight(text, { language }).value);
    } catch {
      // Fall through to escaped plain text for a broken grammar.
    }
  }
  return DOMPurify.sanitize(escapeHtml(text));
}
