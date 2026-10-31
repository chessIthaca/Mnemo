/**
 * Terminal output → styled text. Tools stream raw terminal bytes: SGR color
 * and weight codes become styled segments, every other control sequence
 * (cursor moves, erase-line, OSC titles and links) is dropped, and a carriage
 * return redraws its line the way a terminal would (progress bars).
 */

export interface AnsiStyle {
  color?: string;
  bold?: boolean;
  dim?: boolean;
  italic?: boolean;
  underline?: boolean;
}

export interface AnsiSegment {
  text: string;
  style: AnsiStyle;
}

export type AnsiLine = AnsiSegment[];

// The 16 base colors, tuned to stay readable on the light output panel
// (terminal "white" would vanish, so it maps to a mid gray).
const BASE_COLORS = [
  "#242620", "#b3443a", "#3f7d3a", "#8a6a12", "#3a62a8", "#8b4a9c", "#2b7b86", "#83877b",
  "#5e6258", "#c9543f", "#4c9445", "#a07c14", "#4a73bd", "#a35bb5", "#33909c", "#6e7268",
];

/** CSI (`ESC [ … final`), OSC (`ESC ] … BEL|ST`), other two-byte escapes, or an SGR whose ESC was lost. */
// eslint-disable-next-line no-control-regex
const SEQUENCE = /\x1b\[([0-9;?]*)([@-~])|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)?|\x1b[@-_]|\[((?:\d{1,3}(?:;\d{1,3})*)?)m/g;

function color256(code: number): string | undefined {
  if (code < 16) return BASE_COLORS[code];
  if (code < 232) {
    const index = code - 16;
    const level = (value: number) => (value === 0 ? 0 : 55 + value * 40);
    return `rgb(${level(Math.floor(index / 36))} ${level(Math.floor(index / 6) % 6)} ${level(index % 6)})`;
  }
  if (code < 256) {
    const gray = 8 + (code - 232) * 10;
    return `rgb(${gray} ${gray} ${gray})`;
  }
  return undefined;
}

/** Apply one SGR parameter list to the running style. */
function applySgr(style: AnsiStyle, params: string): AnsiStyle {
  const codes = params === "" ? [0] : params.split(";").map((part) => Number(part) || 0);
  let next = { ...style };
  for (let i = 0; i < codes.length; i += 1) {
    const code = codes[i];
    if (code === 0) next = {};
    else if (code === 1) next.bold = true;
    else if (code === 2) next.dim = true;
    else if (code === 3) next.italic = true;
    else if (code === 4) next.underline = true;
    else if (code === 22) { next.bold = false; next.dim = false; }
    else if (code === 23) next.italic = false;
    else if (code === 24) next.underline = false;
    else if (code >= 30 && code <= 37) next.color = BASE_COLORS[code - 30];
    else if (code >= 90 && code <= 97) next.color = BASE_COLORS[code - 90 + 8];
    else if (code === 39) delete next.color;
    else if (code === 38 || code === 48) {
      // Extended color: 5;n (256 colors) or 2;r;g;b. Backgrounds are consumed, not drawn.
      const mode = codes[i + 1];
      if (mode === 5) {
        if (code === 38) next.color = color256(codes[i + 2]);
        i += 2;
      } else if (mode === 2) {
        if (code === 38) next.color = `rgb(${codes[i + 2]} ${codes[i + 3]} ${codes[i + 4]})`;
        i += 4;
      }
    }
    // Backgrounds (40–49, 100–107), blink, inverse and the rest are dropped.
  }
  return next;
}

/** Parse terminal output into lines of styled segments. */
export function parseAnsi(text: string): AnsiLine[] {
  const lines: AnsiLine[] = [[]];
  let style: AnsiStyle = {};
  // A carriage return redraws its line: the next text on that line replaces
  // it (a CRLF's newline arrives first and leaves the line as printed).
  let redraw = false;
  const push = (chunk: string) => {
    chunk.split("\n").forEach((part, index) => {
      if (index > 0) {
        lines.push([]);
        redraw = false;
      }
      part.split("\r").forEach((piece, pieceIndex) => {
        if (pieceIndex > 0) redraw = true;
        if (piece === "") return;
        const line = lines[lines.length - 1];
        if (redraw) {
          line.length = 0;
          redraw = false;
        }
        line.push({ text: piece, style });
      });
    });
  };
  let last = 0;
  for (const match of text.matchAll(SEQUENCE)) {
    push(text.slice(last, match.index));
    last = (match.index ?? 0) + match[0].length;
    if (match[2] === "m") style = applySgr(style, match[1]);
    else if (match[3] !== undefined) style = applySgr(style, match[3]);
  }
  push(text.slice(last));
  return lines;
}

/** Plain text with every terminal sequence removed. */
export function stripAnsi(text: string): string {
  return parseAnsi(text)
    .map((line) => line.map((segment) => segment.text).join(""))
    .join("\n");
}

/**
 * The last `maxLines` non-empty-tail lines of streaming output. Only the end
 * of a long stream is parsed; a style set before that window is lost, which
 * only affects the first visible line.
 */
export function terminalTail(text: string, maxLines = 5, window = 8192): AnsiLine[] {
  const lines = parseAnsi(text.length > window ? text.slice(-window) : text);
  while (lines.length > 1 && lines[lines.length - 1].length === 0) lines.pop();
  return lines.slice(-maxLines);
}

/** Inline CSS for one segment. */
export function ansiCss(style: AnsiStyle): string {
  const rules: string[] = [];
  if (style.color) rules.push(`color:${style.color}`);
  if (style.bold) rules.push("font-weight:650");
  if (style.dim) rules.push("opacity:0.65");
  if (style.italic) rules.push("font-style:italic");
  if (style.underline) rules.push("text-decoration:underline");
  return rules.join(";");
}
