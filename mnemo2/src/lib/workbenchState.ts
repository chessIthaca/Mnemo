import type { LeadTranscript } from "./types";
import { messageText } from "./leadState";

const screenshotPattern = /\.coding\/browser\/screenshots\/[A-Za-z0-9._/-]+\.png/g;

/** Extract only genuine sandboxed screenshot paths from a worker transcript. */
export function workerScreenshotPaths(transcript: LeadTranscript): string[] {
  const paths = new Set<string>();
  for (const message of transcript.messages) {
    const text = messageText(message.content);
    for (const path of text.matchAll(screenshotPattern)) {
      if (!path[0].split("/").includes("..")) paths.add(path[0]);
    }
  }
  return [...paths];
}
