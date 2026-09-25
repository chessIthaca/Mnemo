// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useState, useEffect, useRef, memo } from "react";
import { ChevronDown, ChevronRight, Compass, Globe, Image as ImageIcon } from "lucide-react";
import { Markdown } from "./Markdown";
import { InlineMarkdown } from "./InlineMarkdown";
import { MarkdownLink } from "./MarkdownLink";
import { CodeBlock } from "./CodeBlock";
import { UnifiedDiffView } from "./DiffView";
import { openDiffInViewer, openFileInViewer } from "../../lib/openFile";
import { openChatLink } from "../../lib/openChatLink";
import { argLabel, argPaths, browserResultInfo, buildPathChips, displayName, fileEditDiff, liveTailPreview, memorySearchLabel, parseReadFilesSections, parseShellOutput, searchResultInfo, shellCallFromArgs, toolErrorSummary, webFetchUrl, type ToolCardChip } from "../../lib/toolCardPaths";
import { fmtToolDuration, fmtTs } from "../../lib/timeFormat";
import { arePropsEqual, type MessageProps } from "../../lib/messageEquality";
import { toolImagePaths, ToolImage } from "./ToolImage";
import { useAgentStore } from "../../hooks/useAgentStore";
import { filterSteeringNote, stripSteeringNotes } from "../../lib/delegationNotes";
import type { TranscriptEntry, ToolInvocation } from "../../lib/types";

/** A memory read/write activity line (visible only when the user opted in
 *  via Settings → Chat → "Show tool activity in chat"): icon + tier
 *  badge + title + content snippet + ✓/✗, instead of the generic ToolCard.
 *  Frameless like the ToolCard (backlog 900fe8d8): no border/bg tint, body
 *  text at the chat content size, em-based spacing; the violet identity
 *  lives in the icon/name/badges.
 *  Cards carrying a parsed hit list get a chevron that expands to show every
 *  hit's tier + title (+ score where parsed) — mirroring the ToolCard's
 *  expand interaction (user request). That's auto-recall entries (the full
 *  recalled-hit list is the event payload since 2026-08-20) and memory_search
 *  results (the per-hit list parsed from the output — backlog 41f39672: "12
 *  matched, alas no expanding to see the 12"). Other memory entries stay
 *  non-interactive.
 *
 *  Link chips are NOT rendered here (review scope note, 2026-08-23): the
 *  chat `memory` entry carries only tier/title/snippet — the tool-result
 *  event payload has no row `data.links` — so the wiki-link surface lives
 *  in the Memory debug view (MemoryDebugView.tsx), which reads the rows
 *  directly. Extending the chat card would mean plumbing row data through
 *  the tool-result event. */
function MemoryEntryCard({
  entry,
}: {
  entry: Extract<TranscriptEntry, { kind: "memory" }>;
}) {
  const [expanded, setExpanded] = useState(false);
  // The expandable hit list: auto-recall entries carry the full recalled-hit
  // list; memory_search entries carry the hits parsed from their result
  // (backlog 41f39672 — "12 matched, alas no expanding to see the 12"). The
  // backend's search-mode cap fits a full default page (limit 12), so the
  // expansion loses nothing for default searches; explicit larger limits
  // may still truncate — the matched chip then counts more rows than the
  // expansion offers.
  const hits = entry.hits ?? [];
  const expandable = hits.length > 0;
  // What a memory_search searched — the query + scope filters parsed from
  // the call's streamed args (user report 2026-08-25: "memory_search should
  // show what it searches"). Other memory tools have no meaningful query.
  const queryLabel =
    entry.name === "memory_search" ? memorySearchLabel(entry.args ?? "") : null;
  const line = (
    <>
      {expandable &&
        (expanded ? (
          <ChevronDown className="h-[1em] w-[1em] shrink-0 text-slate-500" />
        ) : (
          <ChevronRight className="h-[1em] w-[1em] shrink-0 text-slate-500" />
        ))}
      <span className="text-violet-400">🧠</span>
      <span className="font-medium text-violet-300">{entry.name}</span>
      {queryLabel !== null && (
        <span className="truncate text-slate-300">{queryLabel}</span>
      )}
      {entry.tier && (
        <span className="rounded bg-violet-900/50 px-[0.375em] py-[0.125em] text-[0.65em] uppercase tracking-wide text-violet-300">
          {entry.tier}
        </span>
      )}
      {entry.title && (
        <span className="truncate text-slate-300">{entry.title}</span>
      )}
      {entry.snippet && (
        <span className="truncate text-slate-500">{entry.snippet}</span>
      )}
      <span className="ml-auto flex shrink-0 items-center gap-[0.25em] text-[0.75em] text-slate-500">
        {/* Matched count mirrors the search card's engine chip — from the
            result's "N memories matched" header, shown once finalized. */}
        {!entry.running && entry.matched !== undefined && (
          <span>
            {entry.matched === 0 ? "no matches" : `${entry.matched} matched`}
          </span>
        )}
        {entry.running ? "…" : entry.success ? "✓" : "✗"}
      </span>
    </>
  );
  return (
    <div className="py-[0.125em]">
      {expandable ? (
        <span
          role="button"
          tabIndex={0}
          onClick={() => setExpanded(!expanded)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              setExpanded(!expanded);
            }
          }}
          className="flex cursor-pointer items-center gap-[0.25em]"
        >
          {line}
        </span>
      ) : (
        <div className="flex items-center gap-[0.25em]">{line}</div>
      )}
      {expandable && expanded && (
        <div className="mt-[0.25em] space-y-[0.15em]">
          {hits.map((h, i) => (
            <div key={i} className="flex items-center gap-[0.25em]">
              <span className="rounded bg-violet-900/50 px-[0.375em] py-[0.125em] text-[0.65em] uppercase tracking-wide text-violet-300">
                {h.tier}
              </span>
              <span className="truncate text-slate-400">{h.title}</span>
              {(h as { score?: number }).score !== undefined && (
                <span className="ml-auto shrink-0 text-slate-500">
                  {(h as { score: number }).score.toFixed(2)}
                </span>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * A vision-model image-description card ("image parsing"): pushed running by
 * `vision_describe`, completed by `vision_described`. Collapsed it shows the
 * parse status (and, once done, the response inline); expanded it shows the
 * full query sent to the vision model and the response it returned — or the
 * error, in red (user request: "I would love to see what the query and
 * response is when I uncollapse"). This card announces the pause that used
 * to be silent: the description round-trip runs BEFORE the turn starts, so
 * nothing else in the transcript moves while it waits. Neutral slate
 * styling — violet is memory's. Frameless like the ToolCard (backlog
 * 900fe8d8): no border/bg tint, body text at the chat content size,
 * em-based spacing.
 */
function VisionEntryCard({
  entry,
}: {
  entry: Extract<TranscriptEntry, { kind: "vision" }>;
}) {
  const [expanded, setExpanded] = useState(false);
  const done = !entry.running;
  return (
    <div className="py-[0.125em]">
      <span
        role="button"
        tabIndex={0}
        onClick={() => setExpanded(!expanded)}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            setExpanded(!expanded);
          }
        }}
        className="flex cursor-pointer items-center gap-[0.25em]"
      >
        {expanded ? (
          <ChevronDown className="h-[1em] w-[1em] shrink-0 text-slate-500" />
        ) : (
          <ChevronRight className="h-[1em] w-[1em] shrink-0 text-slate-500" />
        )}
        <span>🖼️</span>
        <span className="font-medium text-slate-300">image parsing</span>
        {entry.total > 1 && (
          <span className="text-slate-500">
            {entry.index}/{entry.total}
          </span>
        )}
        {entry.running ? (
          <span className="thinking-dots">
            <span></span>
            <span></span>
            <span></span>
          </span>
        ) : (
          <>
            <span className="truncate text-slate-500">{entry.description ?? "—"}</span>
            <span
              className={`ml-auto flex shrink-0 text-[0.75em] ${
                entry.success ? "text-slate-500" : "text-red-400"
              }`}
            >
              {entry.success ? "✓" : "✗"}
            </span>
          </>
        )}
      </span>
      {expanded && (
        <div className="mt-[0.25em] space-y-[0.4em]">
          <div>
            <div className="text-[0.75em] text-slate-500">vision query</div>
            <div className="whitespace-pre-wrap break-words text-slate-300">
              {entry.query}
            </div>
          </div>
          <div>
            <div className="text-[0.75em] text-slate-500">
              {done && !entry.success ? "description unavailable" : "vision response"}
            </div>
            <div
              className={`whitespace-pre-wrap break-words ${
                done && !entry.success ? "text-red-400" : "text-slate-300"
              }`}
            >
              {entry.description ?? (done ? "—" : "…")}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function MessageImpl({ entry, streaming = false }: MessageProps) {
  // Prose width cap (plan afa81f0a): ~100 columns for readability on wide
  // windows — code blocks, diffs, and tool outputs stay full width (the
  // cap targets prose elements only, via the typography variants).
  const chatProseCap = useAgentStore((s) => s.chatProseCap);
  // Readability indent ladder (user request 2027-01-04): level 0 = user
  // prompts (user + steer) flush LEFT — they stick out left of everything
  // else; level 1 = agent prose (assistant/error/qa) at pl-[1em]; level 2 =
  // agent tool-work (tool/memory/vision/skill) — the level-2 indent is
  // owned by Conversation's activity-run wrapper (ml-[2em], or
  // ml-[1.5em] + border-l + pl-[0.5em] when the thread line is on), so
  // these cases render bare. The indents are em-based so the ladder scales
  // with the app font-size setting, and prompts carry pt-[0.75em] so each
  // turn (prompt → response → tools) stands apart. Conversation.tsx indents
  // its interactive prompts (ApprovalPrompt/QuestionPrompt) at level 1 to
  // match.
  switch (entry.kind) {
    case "user":
      return (
        <div className="flex flex-col items-start gap-1 pt-[0.75em]">
          <div className="whitespace-pre-wrap rounded-lg rounded-bl-sm bg-cyan-600 px-4 py-2 text-sm text-white">
            {entry.text}
          </div>
          {entry.images && entry.images.length > 0 && (
            <div className="flex flex-wrap gap-1.5">
              {entry.images.map((dataUrl, i) => (
                <img
                  key={i}
                  src={dataUrl}
                  alt={`attachment ${i + 1}`}
                  className="h-20 w-20 rounded-lg border border-border object-cover"
                />
              ))}
            </div>
          )}
          {entry.imagesEvicted != null && entry.imagesEvicted > 0 && (
            <div
              className="flex items-center gap-1.5 rounded-lg border border-dashed border-border px-3 py-1.5 text-xs text-slate-400"
              title="Old image attachments are unloaded once the transcript's retained image payload exceeds its memory budget; the model already saw them."
            >
              <ImageIcon className="h-3.5 w-3.5" />
              {entry.imagesEvicted} image
              {entry.imagesEvicted > 1 ? "s" : ""} unloaded to save memory
            </div>
          )}
        </div>
      );
    case "assistant":
      // During active streaming, render plain text to avoid re-parsing
      // markdown on every token. Once finalized (flushed to the transcript
      // without `streaming`), the full markdown is parsed once.
      if (streaming) {
        return (
          <div className="pl-[1em]">
            <div className="text-sm text-slate-200">
              <div
                className={`whitespace-pre-wrap break-words ${
                  chatProseCap ? "max-w-[100ch]" : ""
                }`}
              >
                {entry.text}
              </div>
            </div>
          </div>
        );
      }
      return (
        <div className="pl-[1em]">
          <div
            className={`prose prose-invert prose-sm max-w-none prose-pre:m-0 prose-pre:bg-bg-primary ${
              chatProseCap
                ? "prose-p:max-w-[100ch] prose-li:max-w-[100ch] prose-blockquote:max-w-[100ch]"
                : ""
            }`}
          >
            <Markdown
              components={{
                // Markdown file/URL links open through the sanctioned
                // openers (Files-tab deep-link / shell open) — raw <a>
                // anchors are dead under the webview's CSP (user report
                // 2027-01-07: review-report links failed to open).
                a: MarkdownLink,
                code({ className, children, ...props }) {
                  const isInline = !className;
                  if (isInline) {
                    return (
                      <code
                        className="inline-code rounded bg-bg-tertiary px-1.5 py-0.5 text-xs"
                        {...props}
                      >
                        {children}
                      </code>
                    );
                  }
                  return <CodeBlock className={className}>{children}</CodeBlock>;
                },
              }}
            >
              {entry.text}
            </Markdown>
          </div>
        </div>
      );
    case "tool":
      return <ToolCard name={entry.name} calls={entry.calls} />;
    case "error":
      return (
        <div className="pl-[1em]">
          <div className="rounded-lg border border-red-600/50 bg-red-950/30 px-4 py-2 text-sm text-red-400">
            <span className="font-medium">error: </span>
            {entry.text}
          </div>
        </div>
      );
    case "steer":
      // A landed steer — mid-work guidance the user sent while the agent was
      // running, now injected into the conversation. Highlighted in amber so
      // it's visible in the agent window. Image attachments render as
      // thumbnails like a user prompt's (steered images ride the payload
      // end-to-end).
      return (
        <div className="flex justify-start pt-[0.75em]">
          <div className="rounded-lg rounded-bl-sm border border-amber-500/50 bg-amber-950/40 px-4 py-2 text-sm text-amber-200">
            <div className="flex items-center gap-2">
              <Compass className="h-3.5 w-3.5 shrink-0 text-amber-400" />
              <span className="whitespace-pre-wrap break-words">{entry.text}</span>
            </div>
            {entry.images && entry.images.length > 0 && (
              <div className="mt-2 flex flex-wrap gap-2">
                {entry.images.map((dataUrl, i) => (
                  <img
                    key={i}
                    src={dataUrl}
                    alt={`steer attachment ${i + 1}`}
                    className="h-20 w-20 rounded-lg border border-amber-500/30 object-cover"
                  />
                ))}
              </div>
            )}
            {entry.imagesEvicted != null && entry.imagesEvicted > 0 && (
              <div
                className="mt-2 flex items-center gap-1.5 rounded-lg border border-dashed border-amber-500/30 px-3 py-1.5 text-xs text-amber-300/80"
                title="Old image attachments are unloaded once the transcript's retained image payload exceeds its memory budget; the model already saw them."
              >
                <ImageIcon className="h-3.5 w-3.5" />
                {entry.imagesEvicted} image
                {entry.imagesEvicted > 1 ? "s" : ""} unloaded to save memory
              </div>
            )}
          </div>
        </div>
      );
    case "skill":
      // A skill started from the toolbar (e.g. the "Merge to main" button).
      // Announced in the transcript like a tool call — `▶ skill "name" start`
      // — with the goal as a muted subtitle. Mirrors how skill_end appears.
      return (
        <div className="py-[0.125em]">
          <div className="flex items-center gap-[0.4em] text-slate-400">
            <span className="text-slate-500">▶</span>
            <span className="font-medium">
              skill <span className="text-slate-300">&quot;{entry.name}&quot;</span> start
            </span>
          </div>
          {entry.prompt && (
            <div className="mt-[0.25em] text-slate-500 whitespace-pre-wrap break-words">
              {entry.prompt}
            </div>
          )}
        </div>
      );
    case "qa":
      // A collapsed ask_user question + its answer — the live QuestionPrompt
      // collapsed into a static Q→A record after the user answered. Rendered
      // in the transcript right where the prompt was, so the exchange persists.
      return (
        <div className="pl-[1em]">
          <div className="my-2 rounded-lg border border-cyan-600/40 bg-cyan-950/20 px-4 py-3">
            <p className="mb-2 text-sm text-slate-100">{entry.question}</p>
            <div className="flex items-start gap-2 text-sm">
              <span className="shrink-0 text-cyan-400">→</span>
              <span className="whitespace-pre-wrap break-words text-slate-200">
                {entry.answer}
              </span>
            </div>
          </div>
        </div>
      );
    case "memory":
      // A memory read/write activity line (shown only when the user opted in
      // via Settings → Chat → "Show tool activity in chat").
      // Auto-recall entries expand to show every recalled hit — see
      // MemoryEntryCard.
      return <MemoryEntryCard entry={entry} />;
    case "vision":
      // A vision-model image-description card ("image parsing") — announces
      // the image-description round-trip that runs before the turn starts
      // and expands to show the query + response. See VisionEntryCard.
      return <VisionEntryCard entry={entry} />;
    default:
      return null;
  }
}

export const Message = memo(MessageImpl, arePropsEqual);

function ToolCard({
  name,
  calls,
}: {
  name: string;
  calls: ToolInvocation[];
}) {
  const [expanded, setExpanded] = useState(false);
  // Inline images from image commands are gated by the Chat settings toggle
  // (config.toml [ui].show_tool_images, default ON).
  const showToolImages = useAgentStore((s) => s.showToolImages);
  // The live shell-output preview is gated by the Chat settings toggle
  // (localStorage mh.showShellPreview, default ON — backlog 6f25fb7e).
  const showShellPreview = useAgentStore((s) => s.showShellPreview);
  // Steering notes are gated per kind by the Chat settings toggles
  // (config.toml [ui].steering_notes) — a GUI-only display filter; the tool
  // result text is unaffected.
  const hiddenSteeringNotes = useAgentStore((s) => s.hiddenSteeringNotes);
  const running = calls.some((c) => c.result === null);
  // The card reflects the LAST call's outcome (not "any call failed") so a
  // fail→redo→succeed sequence ends green. Per-call ✓/✗ is still shown in the
  // expanded detail below. While any call is still running, stay neutral.
  const lastCall = calls[calls.length - 1];
  const failed = !running && lastCall.result !== null && !lastCall.result.success;
  // The failed call's one-line error summary (the first line of the result
  // output, shown below the header without expanding the card). A steering
  // note can BE that line — a failed `file_edit`'s drift error carries the
  // stale-read note — so it passes through the same per-kind filter as the
  // expanded detail; otherwise a note whose toggle is off would still show
  // here.
  const errorSummary =
    failed && lastCall.result
      ? (filterSteeringNote(toolErrorSummary(lastCall.result.output), hiddenSteeringNotes) ?? "")
      : "";

  // Header chips: file-path chips come first (deduplicated across all calls so
  // each file appears at most once — same file read in multiple grouped calls
  // → one chip; first occurrence wins, keeping its link; DISTINCT files that
  // share a basename get parent-dir-qualified display names, so no name ever
  // prints twice), then per-call label
  // chips for pathless calls (shell purpose, git subcommand, …). A file path
  // gets a clickable link chip (opens that file in the Files viewer — except
  // file_edit, whose chip opens the right-panel Diff tab at that file); a
  // web_fetch label is the fetched URL and its chip opens it in the user's
  // browser; any other label renders as plain text. Path chips are capped at
  // 3 with a `+N` overflow chip — every visible name is the link it appears
  // to be.
  const chips: ToolCardChip[] = buildPathChips(calls, name);
  // Literal-fallback notes from search/search_read results (a broken regex
  // was matched literally) — surfaced under the header so the degraded
  // semantics are visible without expanding the card.
  const searchNotes: { key: string; text: string }[] = [];
  for (const c of calls) {
    const paths = argPaths(c.args, name);
    if (paths.length === 0) {
      const label = argLabel(c.args, name);
      if (label !== null) {
        // backlog_add labels carry the queued item's title — flag the chip
        // so the render site renders it as inline markdown (plan 2026-12).
        // web_fetch labels carry the fetched URL — attach the FULL url so
        // the render site makes the chip a link that opens the page in the
        // user's browser (null/invalid url → plain text).
        chips.push({
          key: c.id,
          text: label,
          path: null,
          line: null,
          md: name === "backlog_add",
          url: webFetchUrl(name, c.args),
        });
      }
    }
    // Engine transparency for search/search_read: which engine served the
    // query (index = FTS content index, walk = tree scan) + the match
    // count, as a trailing chip on each completed call.
    if (
      (name === "search" || name === "search_read") &&
      c.result !== null &&
      c.result.success
    ) {
      const info = searchResultInfo(c.result.output);
      if (info !== null) {
        chips.push({
          key: `${c.id}:engine`,
          text: `${info.engine} · ${info.matches === 0 ? "no matches" : `${info.matches} match${info.matches === 1 ? "" : "es"}`}`,
          path: null,
          line: null,
        });
        const noteText =
          info.note === null ? null : filterSteeringNote(info.note, hiddenSteeringNotes);
        if (noteText !== null && noteText !== "") {
          searchNotes.push({ key: `${c.id}:note`, text: noteText });
        }
      }
    }
    // Browser tools: a trailing chip summarizing WHAT happened (the landed
    // URL for navigate, the page title for snapshot, the PNG filename for
    // screenshot) so the card reads "browser_navigate → https://…"
    // (user report 2026-09-09).
    if (c.result !== null && c.result.success) {
      const info = browserResultInfo(name, c.result.output);
      if (info !== null) {
        chips.push({ key: `${c.id}:browser`, text: info, path: null, line: null });
      }
    }
  }

  // Header color by state: running (yellow), done-ok (slate), failed (red).
  const textColor = running
    ? "text-yellow-400"
    : failed
      ? "text-red-400"
      : "text-slate-400";

  // Live output tail (backlog 7e6385b3): while a call is still RUNNING, show
  // what the command has printed so far instead of leaving the user blind
  // until it exits. Only a running call may contribute — the result replaces
  // the tail (the reducer clears it on tool_result), so a finished card can
  // never show a stale preview, and a chunk that lost the race against its own
  // result is dropped reducer-side. At most one call per card carries a tail
  // in practice (only `shell` streams, and shell never merges — the reducer's
  // neverGroups); the `find` keeps the render total regardless.
  const liveOutput = calls.find((c) => c.result === null && c.liveOutput)?.liveOutput ?? "";
  // Only the newest lines reach the DOM (the reducer's 16 KiB window is what
  // makes a scroll-back possible; this bounds the node re-rendered per chunk).
  const liveTail = liveTailPreview(liveOutput);
  const liveTailRef = useRef<HTMLPreElement | null>(null);
  // Follow the tail as chunks arrive: the retained window is capped (16 KiB in
  // the reducer, 10em here), so the NEWEST lines are the point — the same
  // stick-to-bottom idiom as InflightBar's reasoning panel.
  useEffect(() => {
    const el = liveTailRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [liveTail]);

  return (
    <div className="py-[0.125em]">
      {/* span (role=button) instead of <button> so the nested file-link chips
          (real <button>s) are valid HTML — a <button> cannot contain one. */}
      <span
        role="button"
        tabIndex={0}
        onClick={() => setExpanded(!expanded)}
        onKeyDown={(e) => {
          // Chip buttons (file links, the web_fetch URL chip) handle their
          // own key activation — a keydown bubbling up from a focused chip
          // must not toggle the expand (its preventDefault would also cancel
          // the chip's own click activation).
          if (e.target !== e.currentTarget) return;
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            setExpanded(!expanded);
          }
        }}
        className={`flex cursor-pointer items-center gap-[0.25em] ${textColor}`}
      >
        {expanded ? <ChevronDown className="h-[1em] w-[1em]" /> : <ChevronRight className="h-[1em] w-[1em]" />}
        <span className="font-medium">{displayName(name)}</span>
        {chips.map((chip) => {
          if (chip.path !== null) {
            // file_edit / multi_edit chips deep-link into the Diff tab (user
            // request) — every other tool opens the file in the Files tab as
            // before.
            const opensDiff = name === "file_edit" || name === "multi_edit";
            return (
              <button
                key={chip.key}
                onClick={(e) => {
                  e.stopPropagation();
                  if (opensDiff) {
                    openDiffInViewer(chip.path as string);
                  } else {
                    // read_files chips carry the line the read started at —
                    // deep-link the Files viewer to it.
                    openFileInViewer(chip.path as string, chip.line);
                  }
                }}
                title={
                  opensDiff
                    ? `Open the diff of ${chip.path} in the Diff tab`
                    : `Open ${chip.path} in the Files tab${chip.line !== null ? ` at line ${chip.line}` : ""}`
                }
                className="text-cyan-400 underline-offset-2 hover:underline"
              >
                {chip.text}
              </button>
            );
          }
          // A chip carrying an external url (web_fetch's fetched page) is a
          // link that loads that page in the app's OWN Browser tab (user
          // request 2027-01-16) — the production CSP blocks plain anchors, so
          // the click routes through openChatLink, which falls back to the OS
          // browser where the tab cannot exist. ctrl/cmd-click keeps the old
          // OS-browser behavior. stopPropagation so the click doesn't toggle
          // the card's expand, mirroring the file-link chips above; the Globe
          // icon signals it opens INSIDE the app (unlike an OS-browser launch).
          if (chip.url != null) {
            const url = chip.url;
            return (
              <button
                key={chip.key}
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  void openChatLink(url, { osBrowser: e.ctrlKey || e.metaKey });
                }}
                title={`Open ${url} in the Browser tab — ctrl-click for your browser`}
                className="flex items-center gap-[0.15em] text-cyan-400 underline-offset-2 hover:underline"
              >
                {chip.text}
                <Globe className="h-[0.75em] w-[0.75em] shrink-0" />
              </button>
            );
          }
          // Chips flagged `md` (backlog_add's argLabel chip, which carries
          // the queued item's title) render as inline markdown (bold/code,
          // truncation-safe) like the other read-only backlog surfaces
          // (plan 2026-12). Every other pathless chip stays plain text.
          return (
            <span key={chip.key} className="text-slate-500">
              {chip.md ? <InlineMarkdown text={chip.text} /> : chip.text}
            </span>
          );
        })}
        {running ? (
          <span className="flex items-center gap-[0.3em] text-[0.75em] text-slate-500">
            <span className="thinking-dots">
              <span></span>
              <span></span>
              <span></span>
            </span>
            running
          </span>
        ) : (
          <span className="text-[0.75em] text-slate-500">
            {failed ? "✗" : "✓"}
            {calls.length > 1 && ` ×${calls.length}`}
            {/* Timing (user request 2027-01-24): the LAST call's duration
                (the card reflects the last call's outcome — same rule) and
                the FIRST call's wall-clock start (the card's timeline
                anchor — consecutive cards' start times show the gaps
                between tools). Legacy invocations without stamps render
                exactly as before. */}
            {lastCall.startedAt !== undefined &&
              lastCall.endedAt !== undefined &&
              ` · ${fmtToolDuration(lastCall.endedAt - lastCall.startedAt)}`}
            {calls[0].startedAt !== undefined && ` · ${fmtTs(calls[0].startedAt)}`}
          </span>
        )}
      </span>
      {/* The live tail sits under the header (above the collapsed detail) and
          announces itself politely: a multi-minute build shows progress
          without expanding the card, and screen readers hear the newest lines
          instead of a silent gap. Only the newest lines are rendered; the FULL
          text (byte-identical — what the model consumes) still arrives with
          the result. This block is display-only.
          FIXED HEIGHT, reserved from the first paint of a running shell card
          (backlog 6f25fb7e): the block occupies h-[10em] — six lines at the
          element's own em (6 × 1.5em line-height + 2 × 0.5em padding; the
          pre's text-[0.75em] makes every em below font-size element-relative),
          fitting the liveTailPreview window — whether empty or full, so
          streaming chunks can never change the card's height or jump the chat
          column. Only `shell` emits ToolOutputDelta (the shell.rs sink), and
          shell never merges (the reducer's neverGroups), so the reservation
          is shell-only; the Chat-settings toggle turns the preview off
          entirely. */}
      {showShellPreview && name === "shell" && running && (
        <pre
          ref={liveTailRef}
          aria-live="polite"
          className="mt-[0.15em] h-[10em] overflow-auto whitespace-pre-wrap break-words rounded bg-bg-primary p-[0.5em] text-[0.75em] text-slate-400"
        >
          {liveTail}
        </pre>
      )}
      {searchNotes.length > 0 && (
        <div className="mt-[0.15em] space-y-[0.15em]">
          {searchNotes.map((n) => (
            <div key={n.key} className="text-amber-400/80">
              note: {n.text}
            </div>
          ))}
        </div>
      )}
      {/* A simple one-line error description for a failed call — the first
          line of the result output, shown below the header so the user sees
          what went wrong at a glance (without expanding the card). Filtered
          like the expanded detail; hidden when nothing displayable remains
          (empty/whitespace-only output, or a note fully hidden by its
          toggle). */}
      {errorSummary !== "" && (
        <div className="mt-[0.15em] text-red-400/90">{errorSummary}</div>
      )}
      {expanded && (
        <div className="mt-[0.25em] space-y-[0.4em]">
          {calls.map((call, i) => (
            <CallDetail
              key={call.id}
              index={i}
              call={call}
              showIndex={calls.length > 1}
              name={name}
            />
          ))}
        </div>
      )}
      {/* Inline images from image commands — rendered at CARD level so they
          stay visible even when the card is collapsed ("always show the
          image in the agent window"). One thumbnail per produced image:
          the single analyzed file for the image_* tools (both A and B for
          image_ui_diff), or the captured PNG for the screenshot tools. */}
      {showToolImages && calls.length > 0 && (
        <div className="mt-[0.2em] flex flex-wrap gap-[0.5em]">
          {calls.flatMap((call) => toolImagePaths(name, call.args, call.result)).map((p, i) => (
            <ToolImage key={`${p}:${i}`} path={p} />
          ))}
        </div>
      )}
    </div>
  );
}

function CallDetail({
  call,
  index,
  showIndex,
  name,
}: {
  call: ToolInvocation;
  index: number;
  showIndex: boolean;
  name: string;
}) {
  let prettyArgs = call.args;
  try {
    prettyArgs = JSON.stringify(JSON.parse(call.args), null, 2);
  } catch {
    // keep raw
  }
  const running = call.result === null;
  // Same Chat-settings gate as the card-level note chip (see ToolCard):
  // config.toml [ui].steering_notes, one toggle per steering-note kind.
  const hiddenSteeringNotes = useAgentStore((s) => s.hiddenSteeringNotes);
  // Shell calls render human-readably (backlog 8c1d8a47): the command as a
  // block with its purpose/cwd labels, and the result split into stdout, a
  // labeled stderr section, and an exit-code chip — instead of the raw
  // {"command": ...} JSON and one machine-marked output blob. A successful
  // file_edit similarly renders its Rust-side unified diff (`editDiff`
  // below, data.diff) instead of the raw old_string/new_string args JSON;
  // multi_edit renders its ONE combined diff (every file the call changed)
  // the same way. Any other
  // tool keeps the generic pretty-JSON + raw-output rendering.
  const shellArgs = name === "shell" ? shellCallFromArgs(call.args) : null;
  const shellOut = call.result && name === "shell" ? parseShellOutput(call.result.output) : null;
  // Steering notes ride the shell output too — the grep TIP and the
  // null-sink redirection warning are prepended by the tool (shell.rs) — and
  // this branch renders stdout/stderr directly, so it filters them here; the
  // generic fallback below filters the whole output the same way.
  const shellStdout = shellOut ? stripSteeringNotes(shellOut.stdout, hiddenSteeringNotes) : "";
  const shellStderr =
    shellOut && shellOut.stderr !== null
      ? stripSteeringNotes(shellOut.stderr, hiddenSteeringNotes)
      : null;
  const editDiff =
    name === "file_edit" || name === "multi_edit" ? fileEditDiff(call.result) : null;
  // A read_files result is a wall of numbered file content — the expanded
  // body instead shows just WHAT was read: one row per file with its actual
  // line range (parsed from the result's section headers), clickable to open
  // the file at the line the read started.
  const readSections =
    name === "read_files" && call.result !== null
      ? parseReadFilesSections(call.result.output)
      : [];
  return (
    <div className="space-y-[0.4em]">
      {showIndex && (
        <div className="text-[0.75em] text-slate-500">
          #{index + 1} {argLabel(call.args, name) ? `— ${argLabel(call.args, name)}` : ""}
          {/* Per-call timing (grouped cards): same order as the card header —
              duration, then the call's wall-clock start. */}
          {call.startedAt !== undefined &&
            call.endedAt !== undefined &&
            ` · ${fmtToolDuration(call.endedAt - call.startedAt)}`}
          {call.startedAt !== undefined && ` · ${fmtTs(call.startedAt)}`}
        </div>
      )}
      {shellArgs ? (
        <div>
          {shellArgs.purpose && (
            <div className="mb-[0.15em] text-[0.75em] text-slate-500">
              purpose: {shellArgs.purpose}
            </div>
          )}
          {shellArgs.cwd && (
            <div className="mb-[0.15em] text-[0.75em] text-slate-500">
              cwd: {shellArgs.cwd}
            </div>
          )}
          <pre className="overflow-x-auto rounded bg-bg-primary p-[0.5em] text-[0.75em] text-slate-300">
            {shellArgs.command}
          </pre>
        </div>
      ) : editDiff !== null || readSections.length > 0 ? null : (
        prettyArgs && (
          <div>
            <div className="mb-[0.15em] text-[0.75em] text-slate-500">args</div>
            <pre className="overflow-x-auto rounded bg-bg-primary p-[0.5em] text-[0.75em] text-slate-300">
              {prettyArgs}
            </pre>
          </div>
        )
      )}
      {call.result && (
        <div>
          <div className="mb-[0.15em] text-[0.75em] text-slate-500">
            result {running ? "" : call.result.success ? "✓" : "✗"}
            {shellOut && shellOut.exitCode !== null && (
              <span className={shellOut.exitCode === 0 ? " text-slate-500" : " text-red-400"}>
                {" · exit code "}
                {shellOut.exitCode}
              </span>
            )}
          </div>
          {editDiff !== null ? (
            <UnifiedDiffView diffText={editDiff} maxHeightClass="max-h-[24em]" />
          ) : readSections.length > 0 ? (
            <div className="space-y-[0.15em]">
              {readSections.map((s, i) => (
                <div
                  key={`${s.path}:${i}`}
                  className="flex items-baseline gap-[0.5em] text-[0.75em]"
                >
                  <button
                    type="button"
                    onClick={() =>
                      openFileInViewer(s.path, s.kind === "range" ? s.first : null)
                    }
                    title={`Open ${s.path} in the Files tab${s.kind === "range" ? ` at line ${s.first}` : ""}`}
                    className="truncate text-left text-cyan-400 underline-offset-2 hover:underline"
                  >
                    {s.path}
                  </button>
                  <span
                    className={`ml-auto shrink-0 ${s.kind === "note" ? "text-red-400/90" : "text-slate-500"}`}
                  >
                    {s.kind === "range" ? `lines ${s.first}–${s.last} of ${s.total}` : s.note}
                  </span>
                </div>
              ))}
            </div>
          ) : shellOut ? (
            <>
              {shellStdout !== "" && (
                <pre className="max-h-[24em] overflow-auto rounded bg-bg-primary p-[0.5em] text-[0.75em] text-slate-300">
                  {shellStdout}
                </pre>
              )}
              {shellStderr !== null && shellStderr !== "" && (
                <div className="mt-[0.4em]">
                  <div className="mb-[0.15em] text-[0.75em] text-slate-500">stderr</div>
                  <pre className="max-h-[24em] overflow-auto rounded bg-red-950/40 p-[0.5em] text-[0.75em] text-red-300">
                    {shellStderr}
                  </pre>
                </div>
              )}
            </>
          ) : (
            <pre className="max-h-[24em] overflow-auto rounded bg-bg-primary p-[0.5em] text-[0.75em] text-slate-300">
              {stripSteeringNotes(call.result.output, hiddenSteeringNotes)}
            </pre>
          )}
        </div>
      )}
    </div>
  );
}
