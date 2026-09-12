// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useState } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";

/**
 * JsonView — an interactive, collapsible, syntax-highlighted JSON tree.
 *
 * Used by the Trace panel to render request/response JSON readably: objects
 * and arrays collapse to a one-line count preview (`{N keys}` / `[N items]`)
 * and expand in place via a chevron toggle; leaf values are colored by type
 * (strings emerald, numbers cyan, booleans/null amber) and keys are slate
 * with a trailing colon. A 1px indent guide runs down each nesting level.
 *
 * `defaultDepth` (default 1) controls how many levels open on first render —
 * deeper nodes start collapsed. Pure React + tailwind; no
 * dangerouslySetInnerHTML.
 */

/** How many children a collapsed node advertises ("{3 keys}" / "[12 items]"). */
export function summarize(data: unknown): string {
  if (Array.isArray(data)) {
    const n = data.length;
    return `[${n} item${n === 1 ? "" : "s"}]`;
  }
  if (data !== null && typeof data === "object") {
    const n = Object.keys(data).length;
    return `{${n} key${n === 1 ? "" : "s"}}`;
  }
  return "";
}

/** Whether a node at `depth` (0 = root) starts expanded for `defaultDepth`. */
export function shouldAutoExpand(depth: number, defaultDepth: number): boolean {
  return depth < defaultDepth;
}

/** True for values that render as a single leaf row (no children). */
function isLeaf(data: unknown): boolean {
  return data === null || typeof data !== "object";
}

/** The tailwind color class for a leaf value. */
function leafColor(data: unknown): string {
  if (data === null) return "text-amber-400/80";
  switch (typeof data) {
    case "string":
      return "text-emerald-400";
    case "number":
      return "text-cyan-300";
    case "boolean":
      return "text-amber-300";
    default:
      return "text-slate-400";
  }
}

/** Render a leaf value as it appears in JSON (strings get quotes). */
function leafText(data: unknown): string {
  if (data === null) return "null";
  if (typeof data === "string") return JSON.stringify(data);
  return String(data);
}

/** One collapsible object/array node (or leaf) at a given depth. */
function Node({
  name,
  data,
  depth,
  defaultDepth,
}: {
  /** The key (or array index) this node sits under; undefined at the root. */
  name?: string;
  data: unknown;
  depth: number;
  defaultDepth: number;
}) {
  const [open, setOpen] = useState(shouldAutoExpand(depth, defaultDepth));

  const keyLabel =
    name !== undefined ? (
      <span className="shrink-0 text-slate-500">{name}:</span>
    ) : null;

  if (isLeaf(data)) {
    return (
      <div className="flex items-baseline gap-1.5 leading-relaxed">
        {keyLabel}
        <span className={`font-mono ${leafColor(data)}`}>{leafText(data)}</span>
      </div>
    );
  }

  const entries: [string, unknown][] = Array.isArray(data)
    ? data.map((v, i) => [String(i), v] as [string, unknown])
    : Object.entries(data as Record<string, unknown>);

  return (
    <div>
      <button
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-baseline gap-1.5 rounded px-0.5 text-left leading-relaxed hover:bg-bg-tertiary"
      >
        {open ? (
          <ChevronDown className="h-3 w-3 shrink-0 self-center text-slate-500" />
        ) : (
          <ChevronRight className="h-3 w-3 shrink-0 self-center text-slate-500" />
        )}
        {keyLabel}
        {open ? (
          <span className="font-mono text-slate-600">
            {Array.isArray(data) ? "[" : "{"}
          </span>
        ) : (
          <span className="font-mono text-slate-500">{summarize(data)}</span>
        )}
      </button>
      {open && (
        <div className="ml-2 border-l border-border pl-2">
          {entries.length === 0 ? (
            <div className="font-mono text-slate-600">
              {Array.isArray(data) ? "]" : "}"}
              <span className="text-slate-600"> (empty)</span>
            </div>
          ) : (
            entries.map(([k, v]) => (
              <Node
                key={k}
                name={Array.isArray(data) ? undefined : k}
                data={v}
                depth={depth + 1}
                defaultDepth={defaultDepth}
              />
            ))
          )}
        </div>
      )}
    </div>
  );
}

/** The exported viewer — a scrollable, syntax-highlighted JSON tree. */
export function JsonView({
  data,
  defaultDepth = 1,
}: {
  data: unknown;
  defaultDepth?: number;
}) {
  return (
    <div className="max-h-80 overflow-auto p-2 font-mono text-[0.68em]">
      <Node data={data} depth={0} defaultDepth={defaultDepth} />
    </div>
  );
}
