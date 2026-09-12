// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Pure memo-equality for the Message component (chat transcript rows) —
 * React-free so the node-env unit tests can exercise it directly.
 */

import type { TranscriptEntry, ToolInvocation } from "./types";

export interface MessageProps {
  entry: TranscriptEntry;
  /**
   * When true, the entry is the actively-streaming assistant text. We render
   * it as plain text (whitespace-pre-wrap) instead of parsing markdown —
   * ReactMarkdown re-parses the entire string on every render, which is
   * O(text_length) per delta. Plain text is O(1) per append. Markdown is
   * parsed once when the text is finalized and flushed to the transcript.
   */
  streaming?: boolean;
}

/**
 * Custom equality check for `React.memo`. Finalized transcript entries never
 * change, so we can skip re-renders entirely:
 * - assistant/user/error: compare `kind` + `text` (string equality).
 * - tool: compare `calls` by reference — the store creates a new array when
 *   a call is added or a result is updated, so reference equality is correct.
 * - `streaming` prop: must match (toggles between plain-text and markdown).
 */
export function arePropsEqual(prev: MessageProps, next: MessageProps): boolean {
  if (prev.streaming !== next.streaming) return false;
  const pe = prev.entry;
  const ne = next.entry;
  if (pe.kind !== ne.kind) return false;
  // After the kind check, both entries are the same variant. TypeScript
  // can't narrow two variables simultaneously, so we switch on pe.kind
  // and cast ne to the matching type.
  switch (pe.kind) {
    case "assistant":
    case "user":
    case "error":
      // For user entries, also compare images (if present) and the
      // evicted-image count (the transcript image budget can flip an entry
      // from `images` to `imagesEvicted` — that must re-render).
      if (pe.kind === "user") {
        const peImages = (pe as { images?: string[] }).images;
        const neImages = (ne as { images?: string[] }).images;
        if (peImages !== neImages) return false;
        const peEvicted = (pe as { imagesEvicted?: number }).imagesEvicted;
        const neEvicted = (ne as { imagesEvicted?: number }).imagesEvicted;
        if (peEvicted !== neEvicted) return false;
      }
      return pe.text === (ne as { text: string }).text;
    case "tool":
      // `calls` is an array — reference equality. The store spreads a new
      // array on every mutation, so same reference = no change.
      return pe.calls === (ne as { calls: ToolInvocation[] }).calls;
    case "steer":
      // Steered images ride the entry (same payload class as user images):
      // compare images + the evicted-image count so a budget flip
      // re-renders (mirrors the user arm above).
      {
        const peImages = (pe as { images?: string[] }).images;
        const neImages = (ne as { images?: string[] }).images;
        if (peImages !== neImages) return false;
        const peEvicted = (pe as { imagesEvicted?: number }).imagesEvicted;
        const neEvicted = (ne as { imagesEvicted?: number }).imagesEvicted;
        if (peEvicted !== neEvicted) return false;
      }
      return pe.text === (ne as { text: string }).text;
    case "qa":
      return (
        pe.question === (ne as { question: string }).question &&
        pe.answer === (ne as { answer: string }).answer
      );
    case "skill":
      return (
        pe.name === (ne as { name: string }).name &&
        pe.prompt === (ne as { prompt: string }).prompt
      );
    case "memory":
      return (
        pe.name === (ne as { name: string }).name &&
        pe.tier === (ne as { tier?: string }).tier &&
        pe.title === (ne as { title?: string }).title &&
        pe.snippet === (ne as { snippet?: string }).snippet &&
        // Reference equality is fine: hits are stamped once when the entry
        // is pushed and never mutated afterwards.
        pe.hits === (ne as { hits?: { tier: string; title: string }[] }).hits &&
        pe.success === (ne as { success: boolean }).success &&
        pe.running === (ne as { running: boolean }).running
      );
    case "vision":
      // The card transitions running→done (description/success fill in) and
      // toggles its own local expansion — all covered by these fields.
      return (
        pe.index === (ne as { index: number }).index &&
        pe.total === (ne as { total: number }).total &&
        pe.query === (ne as { query: string }).query &&
        pe.description === (ne as { description: string | null }).description &&
        pe.success === (ne as { success: boolean }).success &&
        pe.running === (ne as { running: boolean }).running
      );
    default:
      return false;
  }
}
