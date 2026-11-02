/**
 * Condense a Markdown worker brief into one plain paragraph for scan surfaces.
 *
 * Lead briefs arrive as full Markdown — a heading, paragraphs, bullet lists,
 * backticked paths, emphasized words. The worker list, the avatar tooltip and
 * the detail header only want the words, so the markers that would show as
 * noise (heading `#`s, list bullets, emphasis and backticks, fence or rule
 * lines) are stripped and every run of whitespace collapses into one space.
 * The text is never cut to a character count: the three-line limit is done
 * with CSS where the summary is shown, and the full brief stays one click away
 * in the worker detail.
 */
export function briefSummary(brief: string): string {
  return brief
    .split("\n")
    .map((line) =>
      line
        // A code fence (``` or ```rust), rule or setext underline is pure markup.
        .replace(/^\s*(?:`{3,}.*|-{3,}|={3,}|\*{3,}|_{3,})\s*$/, "")
        .replace(/^\s*#{1,6}\s+/, "") // heading marker
        .replace(/^\s*[-*+]\s+/, "") // list bullet
        .replace(/^\[[ xX]\]\s*/, "") // task-list checkbox left by the bullet
        .replace(/\*\*(.+?)\*\*/g, "$1") // bold
        .replace(/\*(.+?)\*/g, "$1") // italic
        .replace(/(?<!\w)_{1,2}([^_]+)_{1,2}(?!\w)/g, "$1") // __bold__ / _italic_
        .replace(/`([^`]+)`/g, "$1"), // inline code
    )
    .join(" ")
    .replace(/\s+/g, " ")
    .trim();
}