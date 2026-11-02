/**
 * The Memory tab's data layer.
 *
 * The transport calls the six `memory_*` Tauri commands from
 * `src-tauri/src/mnemo2/memory_view.rs`; the wire is plain serde, so the field
 * names below mirror the Rust structs exactly (snake_case). The tab is
 * READ-ONLY — every method here is a query, there is no write path.
 *
 * The formatting helpers (chips, kind labels, dates, embedder status) live
 * here rather than in the component so they can be unit-tested without
 * mounting Svelte.
 */

import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./runtime";

// ---------------------------------------------------------------------------
// Wire types — see src-tauri/src/mnemo2/memory_view.rs (serde keeps snake_case).
// ---------------------------------------------------------------------------

/** One record as the list renders it (`MemoryRow`). */
export interface MemoryRow {
  /** The memory id — what `read` and `backlinks` take. */
  id: string;
  /** The stored title, typed prefix included (`SPEC: …`). */
  title: string;
  /** The typed-record token: spec / decision / bug / how / plan / review / none. */
  kind: string;
  /** The store tier: working / episodic / semantic / procedural. */
  tier: string;
  /** The body's first chars, whitespace-collapsed. */
  preview: string;
  /** `.coding/knowledge/<type>/<slug>.md` when the row derives from a file. */
  path: string | null;
  /** Unix seconds the record was written. */
  created_at: number;
  /** Unix seconds of the last recall. */
  last_accessed_at: number;
  /** How often the record has been recalled. */
  access_count: number;
  /** The store's decayed strength (1.0 = fresh). */
  strength: number;
  /** True when a successor replaced this record — history, not live truth. */
  superseded: boolean;
  /** The startup digest sweep's quarantine reason, when it flagged the row. */
  quarantined: string | null;
}

/** One page of the list, newest first (`MemoryListPage`). */
export interface MemoryListPage {
  rows: MemoryRow[];
  /** The offset this page starts at. */
  offset: number;
  /** True when more rows match beyond this page. */
  has_more: boolean;
}

/** One search or recall-test hit: a row plus its score (`MemoryHit`). */
export interface MemoryHit extends MemoryRow {
  /** The recall score (higher is closer; not comparable across queries). */
  score: number;
}

/** One record in full, as the reading pane renders it (`MemoryDetail`). */
export interface MemoryDetail {
  id: string;
  title: string;
  kind: string;
  tier: string;
  /** The Markdown to render: the knowledge file's body, else the digest. */
  markdown: string;
  /** The knowledge file's repo path when the row derives from one. */
  path: string | null;
  /** The file's `YYYY-MM-DD` creation date from its front matter. */
  created: string | null;
  /** The file's publication status; null for rows without a file. */
  status: "live" | "superseded" | null;
  /** The successor record's id when this record is history. */
  superseded_by: string | null;
  /** The startup digest sweep's quarantine reason, when it flagged the row. */
  quarantined: string | null;
  created_at: number;
  last_accessed_at: number;
  access_count: number;
  strength: number;
  /** Wiki-link targets the file body carries (`[[target]]`, verbatim). */
  links: string[];
  /** The records whose links point here. */
  backlinks: MemoryRow[];
}

/** One stored vector fingerprint (`ModelFingerprint`). */
export interface ModelFingerprint {
  /** The model id the vectors were written with. */
  model: string;
  /** That model's vector dimension. */
  dim: number;
}

/** Per-tier record counts (`MemoryCounts`). */
export interface MemoryCounts {
  working: number;
  episodic: number;
  semantic: number;
  procedural: number;
  total: number;
}

/** The derived index's bookkeeping (`MemoryIndexStatus`). */
export interface MemoryIndexStatus {
  /** Authored (not indexer-derived) rows. */
  authored_count: number;
  /** Indexer-derived rows (plans, reviews, knowledge digests). */
  derived_count: number;
  /** Unix seconds of the last derived build, null when never. */
  last_built_at: number | null;
}

/**
 * The Diagnostics card (`MemoryOverviewView`).
 *
 * `embedder` is the engine's `EmbedderStatus`, externally tagged: a bare
 * string (`"ready"`, `"checking"`, …) or `{downloading: {model, progress}}`.
 */
export interface MemoryOverview {
  /** False when the project runs with memory off — read `reason` then. */
  available: boolean;
  reason: string;
  model_id: string;
  dim: number;
  configured_model: string | null;
  fingerprints: ModelFingerprint[];
  /** True when a stored fingerprint matches the live embedder. */
  fingerprint_matches: boolean;
  counts: MemoryCounts;
  index: MemoryIndexStatus | null;
  embedder: unknown;
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/** What the list asks for: a typed-record group, a tier, and a page. */
export interface MemoryListQuery {
  /** `all` (default), `spec` / `decision` / `bug` / `how` / `plan` / `review` /
   * `none`, or the `other` group. */
  kind?: string | null;
  /** `all` (default) or a store tier. */
  tier?: string | null;
  offset?: number;
  limit?: number;
}

/** The Memory tab's transport: one method per `memory_*` command. */
export interface MemoryTransport {
  list(query?: MemoryListQuery): Promise<MemoryListPage>;
  search(query: string, limit?: number): Promise<MemoryHit[]>;
  read(id: string): Promise<MemoryDetail>;
  backlinks(id: string): Promise<MemoryRow[]>;
  overview(): Promise<MemoryOverview>;
  /** The Diagnostics recall test (working tier included, like Mnemo1's). */
  recallTest(query: string, limit?: number): Promise<MemoryHit[]>;
}

export interface MemoryTransportBindings {
  invoke: typeof invoke;
}

/** The stand-in transport planted for the preview page (see `seedMemoryPreview`). */
let previewSeed: MemoryTransport | null = null;

/**
 * Dev-only seam for the preview page: plant a stand-in transport so the
 * `memory` scene renders sample records in a plain browser, where no Tauri
 * host exists. Ignored inside the desktop app — `createMemoryTransport` only
 * consults the seed when there is no Tauri runtime, so production can never
 * serve planted data.
 */
export function seedMemoryPreview(transport: MemoryTransport): void {
  previewSeed = transport;
}

function createTauriMemoryTransport(bindings: MemoryTransportBindings): MemoryTransport {
  return {
    list: (query = {}) =>
      bindings.invoke<MemoryListPage>("memory_list", {
        kind: query.kind ?? null,
        tier: query.tier ?? null,
        offset: query.offset ?? 0,
        limit: query.limit ?? null,
      }),
    search: (query, limit) =>
      bindings.invoke<MemoryHit[]>("memory_search", { query, limit: limit ?? null }),
    read: (id) => bindings.invoke<MemoryDetail>("memory_read", { id }),
    backlinks: (id) => bindings.invoke<MemoryRow[]>("memory_backlinks", { id }),
    overview: () => bindings.invoke<MemoryOverview>("memory_overview"),
    recallTest: (query, limit) =>
      bindings.invoke<MemoryHit[]>("memory_recall_test", { query, limit: limit ?? null }),
  };
}

/**
 * The transport for the Memory tab: the real Tauri transport in the desktop
 * app, the planted preview seed in a plain browser (dev preview, tests), or
 * `null` when a browser session has nothing planted — the view then explains
 * the tab needs the desktop app.
 */
export function createMemoryTransport(bindings?: MemoryTransportBindings): MemoryTransport | null {
  if (bindings) return createTauriMemoryTransport(bindings);
  if (isTauriRuntime()) return createTauriMemoryTransport({ invoke });
  return previewSeed;
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/** The list's filter chips, in render order: `all` first, `other` last. */
export const KIND_CHIPS: ReadonlyArray<{ value: string; label: string }> = [
  { value: "all", label: "All" },
  { value: "spec", label: "Spec" },
  { value: "decision", label: "Decision" },
  { value: "bug", label: "Bug" },
  { value: "how", label: "How" },
  { value: "other", label: "Other" },
];

/** One page size for the list; the backend clamps whatever it is sent. */
export const LIST_PAGE_SIZE = 40;

const KIND_LABELS: Record<string, string> = {
  spec: "SPEC",
  decision: "DECISION",
  bug: "BUG",
  how: "HOW",
  plan: "PLAN",
  review: "REVIEW",
  none: "NOTE",
};

/** The badge text for a typed-record token (`spec` → `SPEC`, `none` → `NOTE`). */
export function kindLabel(kind: string): string {
  return KIND_LABELS[kind] ?? kind.toUpperCase();
}

/**
 * A title without its typed prefix (`SPEC: typed records` → `typed records`).
 * The badge already names the type, so the prefix would only repeat it; a
 * title without a recognised prefix comes back unchanged.
 */
export function displayTitle(title: string): string {
  return title.replace(/^(SPEC|DECISION|BUG|HOW|PLAN|REVIEW|SESSION|CORRECTION):\s*/i, "");
}


/** The ISO date (`YYYY-MM-DD`, UTC) of a unix-seconds stamp. */
export function formatRecordDate(unixSeconds: number | null): string {
  if (unixSeconds === null || !Number.isFinite(unixSeconds)) return "unknown date";
  return new Date(unixSeconds * 1000).toISOString().slice(0, 10);
}

/** A recall score at two decimals — the recall test's ranking hint. */
export function formatScore(score: number): string {
  return Number.isFinite(score) ? score.toFixed(2) : "–";
}

/**
 * The embedder's status in words. Passed through from the engine's
 * `EmbedderStatus`, which is either a bare lowercase string or the
 * externally-tagged `{downloading: {model, progress}}`.
 */
export function embedderLabel(status: unknown): string {
  if (typeof status === "string") {
    switch (status) {
      case "ready":
        return "ready";
      case "checking":
        return "checking";
      case "fallback":
        return "fallback (hash embedder)";
      case "pulling":
        return "pulling model";
      case "failed":
        return "failed — check Settings → Memory";
      default:
        return status;
    }
  }
  if (status && typeof status === "object" && "downloading" in status) {
    const downloading = (status as { downloading?: { model?: unknown; progress?: unknown } })
      .downloading;
    const model = typeof downloading?.model === "string" ? downloading.model : "model";
    const progress = typeof downloading?.progress === "number" ? downloading.progress : null;
    return progress === null
      ? `downloading ${model}`
      : `downloading ${model} (${Math.round(progress * 100)}%)`;
  }
  return "unknown";
}

/** Append a page to the rows already shown, dropping ids that repeat. */
export function mergePage(rows: MemoryRow[], page: MemoryListPage): MemoryRow[] {
  const seen = new Set(rows.map((row) => row.id));
  return [...rows, ...page.rows.filter((row) => !seen.has(row.id))];
}

/**
 * A list entry that came from a search or a recall test carries a score; a
 * plain list row does not. Narrowing with this keeps the template typed.
 */
export function isHit(row: MemoryRow | MemoryHit): row is MemoryHit {
  return "score" in row;
}
