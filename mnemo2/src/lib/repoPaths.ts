// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Recognising the repository paths a memory record shows.
 *
 * A record names paths in three places: its own `path`, its `links` and its
 * Markdown body. Each one should open the thing it names, so this helper turns
 * the text into a reference the views can act on. It is deliberately narrow: a
 * project-relative path that ends in a file extension, with an optional
 * `:line` suffix. URLs, absolute paths and bare words are not paths and stay
 * plain text.
 */

/** The view a recognised path opens. */
export type RepoPathKind = "plan" | "file";

/** One repository path a record names, and where clicking it goes. */
export interface RepoPathRef {
  /** The project-relative path, the `:line` suffix stripped. */
  path: string;
  /** The 1-based line the reference names, or null when it names none. */
  line: number | null;
  /** What a click opens: a plan document, or a file in the Workbench. */
  kind: RepoPathKind;
  /** The plan's on-disk id (the stem of `.coding/plans/<id>.md`), else null. */
  planId: string | null;
}

/** One run of running text: literal words, or a recognised path. */
export interface PathSegment {
  /** The literal text of this run. */
  text: string;
  /** The path this run names, or null for literal text. */
  ref: RepoPathRef | null;
}

/** A plan document under `.coding/plans/`: the Plans route can expand it. */
const PLAN_DOCUMENT = /^\.coding\/plans\/([^/]+)\.md$/;
/** The `:line` suffix a reference may carry. */
const LINE_SUFFIX = /^(.*):(\d+)$/;
/** A candidate path token in running text, `:line` suffix included. */
const TOKEN = /[A-Za-z0-9_.\-/]+(?::\d+)?/g;
/** Trailing characters that belong to the sentence, not the path. */
const TRAILING = /[.,;:]+$/;
/** A URL scheme or Windows drive prefix (`https:`, `mailto:`, `C:`). */
const SCHEME = /^[A-Za-z][A-Za-z0-9+.-]*:/;
/** A last segment that carries a file extension. */
const EXTENSION = /\.[A-Za-z0-9]+$/;

/** Strip the punctuation a sentence leaves hanging off a path. */
function trimTrailing(token: string): string {
  return token.replace(TRAILING, "");
}

/**
 * Read one token as a project-relative repository path.
 *
 * Returns null for anything that is not one: a URL, an absolute path, a path
 * that escapes the project, a bare word, or a reference without a file
 * extension. A `.coding/plans/<id>.md` document is a plan, everything else a
 * file.
 */
export function parseRepoPath(value: string): RepoPathRef | null {
  const text = value.trim();
  if (text === "") return null;
  // Paths never hold spaces, and these shapes are never project-relative.
  if (/\s/.test(text)) return null;
  if (text.includes("//") || text.startsWith("/") || text.startsWith("\\")) return null;
  if (text.startsWith("~")) return null;
  if (SCHEME.test(text)) return null;
  const suffixed = LINE_SUFFIX.exec(text);
  const path = suffixed ? suffixed[1] : text;
  const line = suffixed ? Number.parseInt(suffixed[2], 10) : null;
  if (line !== null && line < 1) return null;
  // A bare word is not a path, and neither is one that leaves the project.
  if (!path.includes("/")) return null;
  const segments = path.split("/");
  if (segments.some((segment) => segment === "" || segment === "..")) return null;
  if (segments[0] === ".") return null;
  if (!EXTENSION.test(segments[segments.length - 1])) return null;
  const plan = PLAN_DOCUMENT.exec(path);
  return {
    path,
    line,
    kind: plan ? "plan" : "file",
    planId: plan ? plan[1] : null,
  };
}

/**
 * Split running text into literal runs and the repository paths it names, so a
 * renderer can make exactly the paths clickable.
 */
export function splitRepoPaths(text: string): PathSegment[] {
  const segments: PathSegment[] = [];
  let last = 0;
  for (const match of text.matchAll(TOKEN)) {
    const start = match.index ?? 0;
    const token = trimTrailing(match[0]);
    const ref = parseRepoPath(token);
    if (!ref) continue;
    if (start > last) segments.push({ text: text.slice(last, start), ref: null });
    segments.push({ text: token, ref });
    last = start + token.length;
  }
  if (last < text.length) segments.push({ text: text.slice(last), ref: null });
  return segments;
}

/** Where a click on this path lands, for the link's title. */
export function repoPathTarget(ref: RepoPathRef): string {
  if (ref.kind === "plan" && ref.planId !== null) {
    return `Open plan ${ref.planId} in the Plans tab`;
  }
  const at = ref.line === null ? "" : ` at line ${ref.line}`;
  return `Open ${ref.path}${at} in the Files tab`;
}
