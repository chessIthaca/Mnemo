/**
 * Tool-call images for the chat tool cards.
 *
 * Two halves: `toolImagePaths` reads the paths a completed tool call reported
 * out of its arguments and result (Mnemo1's rules, ported verbatim), and
 * `createToolImageTransport` fetches one as a `data:` URL from the host —
 * through the lead's project root, or a worker's checkout.
 */

import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./runtime";
import type { LeadToolResult } from "./types";

/** The screenshot path a browser tool's output text reports, when it has no structured path. */
const SCREENSHOT_IN_OUTPUT = /\.coding\/browser\/screenshots\/[\w.-]+\.png/;

/** The distinct paths a completed tool call produced or looked at, in order. */
export function toolImagePaths(
  toolName: string,
  argumentsJson: string,
  result: LeadToolResult | null,
): string[] {
  if (result === null || !result.success) return [];
  if (toolName.startsWith("image_")) {
    const parsed = parseArguments(argumentsJson);
    if (parsed === null) return [];
    const keys = toolName === "image_ui_diff" ? ["image_a", "image_b"] : ["image"];
    return unique(
      keys
        .map((key) => parsed[key])
        .filter((value): value is string => typeof value === "string" && value.length > 0),
    );
  }
  if (toolName.endsWith("_screenshot")) {
    const data = result.data as { path?: unknown } | null | undefined;
    if (typeof data?.path === "string" && data.path.length > 0) return [data.path];
    const match = SCREENSHOT_IN_OUTPUT.exec(result.output);
    return match === null ? [] : [match[0]];
  }
  return [];
}

/** One JSON object of tool arguments, or null when the stream left it unreadable. */
function parseArguments(argumentsJson: string): Record<string, unknown> | null {
  try {
    const parsed: unknown = JSON.parse(argumentsJson);
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return null;
    return parsed as Record<string, unknown>;
  } catch {
    return null;
  }
}

/** The first occurrence of each path, order kept. */
function unique(paths: string[]): string[] {
  return [...new Set(paths)];
}

/** Reads one tool image as a `data:` URL; null outside the desktop app. */
export interface ToolImageTransport {
  read(workerId: string | null, path: string): Promise<string>;
}

export interface ToolImageBindings {
  invoke: typeof invoke;
}

export function createTauriToolImageTransport(bindings: ToolImageBindings): ToolImageTransport {
  return {
    read: (workerId, path) => bindings.invoke<string>("workbench_tool_image", { workerId, path }),
  };
}

export function createToolImageTransport(
  bindings?: ToolImageBindings,
): ToolImageTransport | null {
  if (!bindings && !isTauriRuntime()) return null;
  return createTauriToolImageTransport(bindings ?? { invoke });
}

/** Loaded images, keyed by owner and path, so a re-render reads the host once. */
const cache = new Map<string, Promise<string>>();

/** Read one tool image, sharing an in-flight or finished read of the same path. */
export function loadToolImage(
  transport: ToolImageTransport,
  workerId: string | null,
  path: string,
): Promise<string> {
  const key = `${workerId ?? "lead"}\u0000${path}`;
  const cached = cache.get(key);
  if (cached !== undefined) return cached;
  const pending = transport.read(workerId, path).catch((error: unknown) => {
    // A failed read is not cached: the next render tries the host again.
    cache.delete(key);
    throw error;
  });
  cache.set(key, pending);
  return pending;
}

/** Drop every cached read; tests call this between cases. */
export function clearToolImageCache(): void {
  cache.clear();
}

/** The MIME type a `data:` URL's header declares (image/png when it omits one). */
export function dataUrlMime(dataUrl: string): string {
  const comma = dataUrl.indexOf(",");
  const header = comma === -1 ? dataUrl : dataUrl.slice(0, comma);
  const match = /^data:([^;,]+)/.exec(header);
  return match?.[1] ?? "image/png";
}

/** Decode a `data:` URL into the blob the clipboard takes. */
export function dataUrlToBlob(dataUrl: string): Blob {
  const comma = dataUrl.indexOf(",");
  const base64 = comma === -1 ? dataUrl : dataUrl.slice(comma + 1);
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return new Blob([bytes], { type: dataUrlMime(dataUrl) });
}

/** The last segment of a tool image path, shown as the image's name. */
export function imageName(path: string): string {
  const segments = path.split(/[\\/]/);
  return segments[segments.length - 1] || path;
}
