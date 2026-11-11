/**
 * Diffs for a tool row's expanded body, built when the call's own data is not
 * enough: an edit whose result carries no structured `data.diff` — a replayed
 * transcript rebuilds results from the message text, so nothing structured
 * survives — still shows its change, reconstructed from the call's
 * arguments, and a write shows the content it wrote as added lines.
 */

import { fileEditDiff } from "./toolCardPaths";
import { type DiffLine, computeDiff, parseUnifiedDiff } from "./diffLines";
import type { LeadToolResult } from "./types";

/** One rendered diff: the file it belongs to (`null` when a result-carried
 *  unified diff names its files in its own `---`/`+++` headers), where the
 *  lines came from, and the lines. */
export interface ToolDiffSection {
  path: string | null;
  /** `result` — the tool's structured payload; `arguments` — the call's own
   *  old/new strings, so line positions are approximate. */
  source: "result" | "arguments";
  /** A short qualifier rendered beside the path ("Appended content"). */
  note: string | null;
  lines: DiffLine[];
}

/** How an arguments-reconstructed diff labels itself. */
const ARGS_NOTE = "Reconstructed from the call arguments";

/** One renderable change of an ops array: an anchor object's old/new pair.
 *  Compact line-op strings (`d202-205`, `r102:…`) reference file content the
 *  arguments do not carry, so only anchors render. */
function anchorChange(op: unknown): { old: string; new: string } | null {
  if (op === null || typeof op !== "object" || Array.isArray(op)) return null;
  const record = op as Record<string, unknown>;
  if (typeof record.old_string !== "string" || record.old_string === "") return null;
  const replacement = typeof record.new_string === "string" ? record.new_string : "";
  return record.old_string === replacement ? null : { old: record.old_string, new: replacement };
}

/** The diff lines of a call's own old→new pairs, in call order, divided by a
 *  hunk-style separator between pairs. */
function changeLines(changes: { old: string; new: string }[]): DiffLine[] {
  const lines: DiffLine[] = [];
  for (const [index, change] of changes.entries()) {
    if (index > 0) lines.push({ type: "meta", text: "@@ ··· @@" });
    lines.push(...computeDiff(change.old, change.new));
  }
  return lines;
}

/** The path an arguments record names, or null. */
function pathArg(value: unknown): string | null {
  return typeof value === "string" && value !== "" ? value : null;
}

/** Parse the args of an edit call; null for anything unparseable. */
function argsRecord(args: string): Record<string, unknown> | null {
  try {
    const parsed: unknown = JSON.parse(args);
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return null;
    return parsed as Record<string, unknown>;
  } catch {
    return null;
  }
}

/** The renderable changes of one ops array (`ops`, legacy `edits`). */
function opsChanges(ops: unknown): { old: string; new: string }[] {
  if (!Array.isArray(ops)) return [];
  const changes = ops.map(anchorChange).filter((change): change is { old: string; new: string } => change !== null);
  return changes;
}

/** The argument-side edit diff of a `file_edit`/`multi_edit` call. Null when
 *  the arguments carry nothing renderable (line-range mode, compact ops). */
function argsEditDiffSections(toolName: string, args: string): ToolDiffSection[] | null {
  const record = argsRecord(args);
  if (record === null) return null;

  if (toolName === "multi_edit") {
    const sections: ToolDiffSection[] = [];
    for (const file of Array.isArray(record.files) ? record.files : []) {
      if (file === null || typeof file !== "object" || Array.isArray(file)) continue;
      const fileRecord = file as Record<string, unknown>;
      const changes = opsChanges(fileRecord.ops ?? fileRecord.edits);
      if (changes.length === 0) continue;
      sections.push({
        path: pathArg(fileRecord.path),
        source: "arguments",
        note: ARGS_NOTE,
        lines: changeLines(changes),
      });
    }
    return sections.length > 0 ? sections : null;
  }

  // file_edit: the single top-level old→new pair, else its own ops array.
  if (typeof record.old_string === "string" && record.old_string !== "") {
    const replacement = typeof record.new_string === "string" ? record.new_string : "";
    if (record.old_string !== replacement) {
      return [{
        path: pathArg(record.path),
        source: "arguments",
        note: ARGS_NOTE,
        lines: computeDiff(record.old_string, replacement),
      }];
    }
  }
  const changes = opsChanges(record.ops);
  if (changes.length > 0) {
    return [{
      path: pathArg(record.path),
      source: "arguments",
      note: ARGS_NOTE,
      lines: changeLines(changes),
    }];
  }
  return null;
}

/**
 * The diff of a completed edit call: the result's structured `data.diff`
 * (computed Rust-side, positions exact) when present, else — a replayed
 * transcript carries none — a diff rebuilt from the call's own
 * old_string/new_string pairs. Null when neither is possible, and always
 * for a failed call: an edit that did not happen must not render a change.
 */
export function editDiffSections(
  toolName: string,
  args: string,
  result: LeadToolResult,
): ToolDiffSection[] | null {
  if (!result.success) return null;
  const diff = fileEditDiff(result);
  if (diff !== null) {
    return [{ path: null, source: "result", note: null, lines: parseUnifiedDiff(diff) }];
  }
  return argsEditDiffSections(toolName, args);
}

/**
 * A write call's content as an all-added section — what the call wrote, under
 * its path. Null when the arguments carry no path/content (the args are still
 * streaming, or nothing was written).
 */
export function writeDiffSections(toolName: string, args: string): ToolDiffSection[] | null {
  const record = argsRecord(args);
  if (record === null) return null;
  const path = pathArg(record.path) ?? pathArg(record.file);
  if (path === null || typeof record.content !== "string") return null;
  const appended = toolName === "file_append" || record.mode === "append";
  // An empty write is one empty added line, exactly like Mnemo1's NewFileView.
  const lines = (record.content.length === 0 ? [""] : record.content.split("\n")).map(
    (text): DiffLine => ({ type: "add", text }),
  );
  return [{ path, source: "arguments", note: appended ? "Appended content" : null, lines }];
}