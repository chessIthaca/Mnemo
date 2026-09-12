// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Map a file path to a highlight.js language id, for the File viewer's
 * syntax highlighting.
 *
 * Markdown (`md`/`markdown`) maps to `"markdown"` — but the File viewer
 * renders Markdown files via the ReactMarkdown component (not a code block),
 * so `"markdown"` here is only a display label / a hint that the file is the
 * primary "key" format. Every other known text/code extension maps to the
 * highlight.js grammar name; unknown extensions map to `null` (plain text,
 * rendered escaped, no highlighting).
 */

/** The highlight.js language id for a path's extension, or `null` if unknown. */
export function languageForPath(path: string): string | null {
  const ext = extension(path);
  if (ext === null) return null;
  return EXT_TO_LANG[ext] ?? null;
}

/**
 * Whether the path looks like a text file the File viewer should display.
 * True when the extension is a known text/code/markup format, OR when there
 * is no extension at all (e.g. `Makefile`, `LICENSE`, `Dockerfile`) — those
 * are overwhelmingly plain text. Returns `false` for clearly binary
 * extensions (images, archives, executables, …) so the viewer can show a
 * "not a text file" placeholder instead of garbage.
 */
export function isTextPath(path: string): boolean {
  const ext = extension(path);
  if (ext === null) return true; // no extension → assume text
  if (BINARY_EXTS.has(ext)) return false;
  // Known text/code, or an unknown-but-not-flagged-binary extension → text.
  return true;
}

/** Extract the lowercase extension (no dot) from a path, or `null` if none. */
function extension(path: string): string | null {
  const base = path.replace(/\\/g, "/").split("/").pop() ?? path;
  const dot = base.lastIndexOf(".");
  // No dot, or a leading-dot dotfile with no other extension (".gitignore").
  if (dot <= 0) return null;
  return base.slice(dot + 1).toLowerCase();
}

/**
 * Whether the path is a Markdown file — the File viewer renders these with
 * the ReactMarkdown component directly (formatted prose, not a code block).
 */
export function isMarkdownPath(path: string): boolean {
  return languageForPath(path) === "markdown";
}

/**
 * Wrap raw source text in a fenced code block so the File viewer can render
 * ANY text file through the shared `<Markdown>` (ReactMarkdown + rehype-highlight)
 * pipeline and get syntax highlighting with the existing theme — the exact
 * same renderer chat uses, so formatting is consistent.
 *
 * The fence length is chosen to exceed the longest run of backticks in
 * `content`, so a source file that itself contains ``` fences can't break out
 * of the wrapper (CommonMark allows fences of 3+ backticks).
 */
export function wrapCodeFence(content: string, language: string | null): string {
  const lang = language ?? "";
  // Find the longest backtick run in the content; the fence must be longer.
  const runs = content.match(/`+/g) ?? [];
  const longest = runs.reduce((m, r) => Math.max(m, r.length), 2); // at least 2 → fence of 3
  const fence = "`".repeat(Math.max(3, longest + 1));
  return `${fence}${lang}\n${content}\n${fence}`;
}

/** Extension (lowercase, no dot) → highlight.js language id. */
const EXT_TO_LANG: Record<string, string> = {
  // Markdown — the key format (rendered via ReactMarkdown in the viewer).
  md: "markdown",
  markdown: "markdown",
  mdx: "markdown",
  // Systems / native.
  rs: "rust",
  c: "c",
  h: "c",
  cpp: "cpp",
  cc: "cpp",
  cxx: "cpp",
  hpp: "cpp",
  cs: "csharp",
  go: "go",
  java: "java",
  kt: "kotlin",
  swift: "swift",
  // Web.
  ts: "typescript",
  tsx: "typescript",
  mts: "typescript",
  cts: "typescript",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  json: "json",
  jsonc: "json",
  css: "css",
  scss: "scss",
  less: "less",
  html: "xml",
  htm: "xml",
  xml: "xml",
  svg: "xml",
  vue: "xml",
  // Data / config.
  toml: "ini",
  yaml: "yaml",
  yml: "yaml",
  ini: "ini",
  cfg: "ini",
  // Scripting.
  py: "python",
  rb: "ruby",
  php: "php",
  lua: "lua",
  r: "r",
  // Shell.
  sh: "shell",
  bash: "shell",
  zsh: "shell",
  ps1: "powershell",
  psm1: "powershell",
  bat: "dos",
  cmd: "dos",
  // Docs / misc text.
  txt: "plaintext",
  sql: "sql",
  dockerfile: "dockerfile",
  // Diff / patch (used by the Diff view and .patch files).
  diff: "diff",
  patch: "diff",
};

/**
 * Extensions that are definitely NOT text — images, archives, binaries,
 * fonts, media. The File viewer shows a placeholder for these.
 */
const BINARY_EXTS = new Set([
  "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "icns",
  "zip", "gz", "tar", "rar", "7z", "xz", "bz2",
  "exe", "dll", "so", "dylib", "bin", "o", "obj", "a", "lib",
  "woff", "woff2", "ttf", "otf", "eot",
  "mp3", "mp4", "wav", "ogg", "avi", "mov", "mkv", "webm",
  "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx",
  "db", "sqlite", "sqlite3", "pyc", "class", "jar", "wasm",
]);
