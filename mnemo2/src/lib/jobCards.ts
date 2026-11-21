// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Job tool calls rendered like shell calls (plan 5b498359 step 2): a collapsed
 * title `job · <purpose> — <state>`, and an expanded body carrying the command
 * and the output instead of the generic Arguments/Result JSON.
 *
 * Every job tool's result `data` follows the plan's interface: `job_id`,
 * `purpose`, `command`, `state` (`running` / `exited` / `killed` / `failed`)
 * and `exit_code` for an exited job. A pre-plan transcript has none of those
 * fields — its `state` carried the human text instead (`exited 2`,
 * `still running`, `killed (job_kill)`) — so the call's own arguments, and the
 * legacy state string, are the fallback. React-free so the node-env unit tests
 * can exercise it directly.
 */

/** The tool names that start a job (`job`, the legacy `job_start`) or act on
 *  one by id (`job_wait`, `job_tail`, `job_kill`). */
export const JOB_TOOL_NAMES = ["job", "job_start", "job_wait", "job_tail", "job_kill"] as const;

/** Whether `toolName` starts a job or acts on one by id. */
export function isJobTool(toolName: string): boolean {
  return (JOB_TOOL_NAMES as readonly string[]).includes(toolName);
}

/** One job call's arguments, parsed. */
export interface JobCallArgs {
  /** The id a follow-up call acts on. */
  jobId: string | null;
  /** The short label given at start — the title's `job · <purpose>` part. */
  purpose: string | null;
  /** The command a start call runs. */
  command: string | null;
  /** The start call's working directory. */
  cwd: string | null;
}

/** The part of a tool result the job card reads: the structured `data`
 *  payload (absent in a pre-plan transcript). */
export interface JobResultLike {
  /** Whether the call succeeded. */
  success?: boolean;
  /** The call's display output. */
  output?: string;
  /** The structured payload carrying the job fields. */
  data?: unknown;
}

/** A job result's state, as the collapsed title shows it. */
export interface JobOutcome {
  /** The suffix after the em dash: `running`, `still running`, `exited 0`,
   *  `killed` or `failed`. */
  label: string;
  /** The exit code of an exited job, for the body's exit-code chip. */
  exitCode: number | null;
}

/** One job call as displayed: its arguments completed by the result's data. */
export interface JobCall extends JobCallArgs {
  /** Whether the call starts a job rather than acting on one by id. */
  start: boolean;
  /** The reported state: `running` while the call is in flight, null when the
   *  result carries none (an old transcript). */
  outcome: JobOutcome | null;
}

/** `value` as a plain object, or null for anything else. */
function asRecord(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

/** A non-blank trimmed string field, or null. */
function textField(record: Record<string, unknown> | null, key: string): string | null {
  const value = record?.[key];
  return typeof value === "string" && value.trim() !== "" ? value.trim() : null;
}

/** The arguments of a job call, parsed; null fields when the JSON is not a
 *  complete object (a still-streaming fragment). */
export function jobCallFromArgs(argsJson: string): JobCallArgs {
  let parsed: unknown = null;
  try {
    parsed = JSON.parse(argsJson);
  } catch {
    // A fragment carries no complete field yet — same fallback as the shell card.
  }
  const record = asRecord(parsed);
  return {
    jobId: textField(record, "job_id"),
    purpose: textField(record, "purpose"),
    command: textField(record, "command"),
    cwd: textField(record, "cwd"),
  };
}

/** The state a job result reports, or — with no result yet — that the call is
 *  running. Null when the result carries no recognisable state. */
export function jobOutcome(result: JobResultLike | null): JobOutcome | null {
  if (result === null) return { label: "running", exitCode: null };
  const data = asRecord(result.data);
  const state = textField(data, "state");
  if (state === null) return null;
  const rawCode = data?.exit_code;
  const exitCode = typeof rawCode === "number" && Number.isFinite(rawCode) ? rawCode : null;
  if (state === "running") return { label: "still running", exitCode: null };
  if (state === "exited") {
    return { label: exitCode === null ? "exited" : `exited ${exitCode}`, exitCode };
  }
  if (state === "killed") return { label: "killed", exitCode: null };
  if (state === "failed") return { label: "failed", exitCode: null };
  // A pre-plan transcript carried the human state text instead of the enum
  // (`JobStatus::text` in src/tool/agent/jobs.rs).
  if (state === "still running") return { label: "still running", exitCode: null };
  const exited = /^exited (-?\d+)$/.exec(state);
  if (exited !== null) return { label: state, exitCode: Number(exited[1]) };
  if (state.startsWith("killed")) return { label: "killed", exitCode: null };
  if (state.startsWith("failed")) return { label: "failed", exitCode: null };
  return null;
}

/** One job call as displayed: its arguments completed by the result's data. A
 *  follow-up call's own arguments name only the id, while the result carries
 *  the purpose and command the job was started with. Null for every other
 *  tool. */
export function jobCallFrom(
  toolName: string,
  argsJson: string,
  result: JobResultLike | null,
): JobCall | null {
  if (!isJobTool(toolName)) return null;
  const args = jobCallFromArgs(argsJson);
  const data = asRecord(result?.data);
  return {
    start: toolName === "job" || toolName === "job_start",
    jobId: args.jobId ?? textField(data, "job_id"),
    purpose: args.purpose ?? textField(data, "purpose"),
    command: args.command ?? textField(data, "command"),
    cwd: args.cwd,
    outcome: jobOutcome(result),
  };
}

/** The longest label a title shows when the purpose is missing. */
const LABEL_MAX = 100;

/** The collapsed title's budget, matching the other headlines. */
const TITLE_MAX = 120;

/** The first line of a text, trimmed; null when blank. */
function oneLine(text: string | null): string | null {
  if (text === null) return null;
  const line = text.split(/\r?\n/, 1)[0].trim();
  return line === "" ? null : line;
}

/** Clamp to `max` characters, with an ellipsis when cut. */
function clamp(text: string, max: number): string {
  return text.length > max ? `${text.slice(0, max - 1).trimEnd()}…` : text;
}

/**
 * The collapsed row title for a job call: `job · <purpose>` plus the state
 * suffix (`— exited 0`), or null for every other tool. Without a purpose the
 * label is the shortened command, or the job id for a follow-up call whose
 * result carries no purpose (an old transcript).
 */
export function jobHeadline(
  toolName: string,
  argsJson: string,
  result: JobResultLike | null,
): { text: string; title: string } | null {
  const call = jobCallFrom(toolName, argsJson, result);
  if (call === null) return null;
  const purpose = oneLine(call.purpose);
  const command = oneLine(call.command);
  const label = purpose ?? (command === null ? call.jobId : clamp(command, LABEL_MAX));
  if (label === null) return null;
  const suffix = call.outcome === null ? "" : ` — ${call.outcome.label}`;
  const text = clamp(`job · ${label}${suffix}`, TITLE_MAX);
  // A shortened command keeps its full text in the row tooltip.
  const title = purpose !== null || command === null ? text : `job · ${command}${suffix}`;
  return { text, title };
}
