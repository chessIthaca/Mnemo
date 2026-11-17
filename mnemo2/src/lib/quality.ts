/**
 * The Quality tab's data layer.
 *
 * The transport calls the one `quality_report` Tauri command from
 * `src-tauri/src/mnemo2/quality.rs`. Both of that file's structs carry
 * `#[serde(rename_all = "camelCase")]`, so the field names below mirror the
 * Rust fields exactly in camelCase — no translation layer. The tab is
 * READ-ONLY: there is a single query and no write path.
 */

import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./runtime";

/** One model's or one tier's record over the window. */
export interface QualityRow {
  /** The final tier name, or the final model spec string. */
  key: string;
  /** Merges in the window. */
  merged: number;
  /** Share (0-1) of those merges that needed no fix round and had no high finding. */
  firstPass: number;
  /** Mean fix rounds per merge. */
  avgFixRounds: number;
  /** Mean findings per merge. */
  avgFindings: number;
  /** Findings recorded as high, summed over the merges. */
  highFindings: number;
  /** Merges a later `reverted` row reports for the same worker. */
  reverted: number;
  /** Merges a later bug-fix plan's merge changed the same files for, within 14 days. */
  bugfixFollowups: number;
  /** Total cost of the merges divided by the merges. */
  costPerMergedMicros: number;
}

/** The whole Quality tab's data. */
export interface QualityReport {
  /** The window the report was computed over, in days. */
  windowDays: number;
  /** Unix epoch milliseconds the report was generated at. */
  generatedMs: number;
  /** One row per final model, most merges first. */
  byModel: QualityRow[];
  /** One row per final tier, most merges first. */
  byTier: QualityRow[];
  /** The window's most recurring finding categories, as (category, distinct workers). Empty when no review logged a category. */
  categories: [string, number][];
  /** Merges whose routing-log row predates `merge_commit`, so a follow-up bug fix cannot be detected for them. */
  missingMergeCommit: number;
}

/** The Quality tab's transport: one query, the whole report. */
export interface QualityTransport {
  /** The report over the last `windowDays` days. */
  report(windowDays: number): Promise<QualityReport>;
}

/** The IPC function the native transport calls; tests and the preview page plant a stand-in. */
export interface QualityTransportBindings {
  invoke: typeof invoke;
}

/** The native transport: the `quality_report` command, one call per window. */
function nativeTransport({ invoke: call }: QualityTransportBindings): QualityTransport {
  return { report: (windowDays) => call<QualityReport>("quality_report", { windowDays }) };
}

/**
 * The transport for the running page: the Tauri IPC inside the desktop host and
 * `null` in a plain browser, where the tab says it needs the desktop app.
 */
export function createQualityTransport(bindings?: QualityTransportBindings): QualityTransport | null {
  if (bindings) return nativeTransport(bindings);
  return isTauriRuntime() ? nativeTransport({ invoke }) : null;
}

/** The windows the tab offers, in days. */
export const QUALITY_WINDOWS: readonly number[] = [7, 30, 90];

/** The window the tab opens on — the backend's own default. */
export const DEFAULT_QUALITY_WINDOW = 30;

/** A share (0-1) as a whole-percent label, e.g. 0.666 → `67%`. */
export function formatShare(share: number): string {
  const value = Number.isFinite(share) ? Math.min(Math.max(share, 0), 1) : 0;
  return `${Math.round(value * 100)}%`;
}

/** A mean with one decimal, e.g. 1.25 → `1.3`. */
export function formatMean(value: number): string {
  return (Number.isFinite(value) ? value : 0).toFixed(1);
}
