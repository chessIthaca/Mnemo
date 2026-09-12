// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useState, useEffect } from "react";
import { Check, X, FileCode, ShieldCheck, ShieldAlert, FolderCheck, Layers } from "lucide-react";
import type { PendingApproval } from "../../hooks/useAgentStore";
import { approve, addSafetyRule, addSafetyRuleBroad, addSafetyRuleClass } from "../../lib/tauri";
import { DiffView, NewFileView, UnifiedDiffView } from "./DiffView";
import { protectedPathTokens } from "../../lib/protectedPathWarning";

interface ApprovalPromptProps {
  approval: PendingApproval;
}

/**
 * Whether a tool call only touches files inside the project directory.
 * Computed client-side (a simpler heuristic than the Rust `is_project_scoped`):
 * file_edit/file_write/file_append/file_read/convert_line_endings with a
 * `path` arg, search, and git are project-scoped; shell is not (it can run
 * anything).
 */
function isProjectScoped(toolName: string, args: Record<string, unknown>): boolean {
  switch (toolName) {
    case "file_edit":
    case "file_write":
    case "file_append":
    case "file_read":
    case "convert_line_endings":
      return typeof args.path === "string" && args.path.length > 0;
    case "search":
    case "git":
      return true;
    default:
      return false;
  }
}

export function ApprovalPrompt({ approval }: ApprovalPromptProps) {
  const [showDiff, setShowDiff] = useState(false);
  const [resolved, setResolved] = useState<"approved" | "denied" | null>(null);
  const [resolveError, setResolveError] = useState<string | null>(null);

  const args = approval.args as {
    path?: string;
    old_string?: string;
    new_string?: string;
    content?: string;
    command?: string;
    cwd?: string;
  };
  const preview = approval.preview;
  const isFileEdit =
    approval.toolName === "file_edit" || approval.toolName === "file_write";
  // Prefer Rust preview path; fall back to args.path for older payloads.
  const filePath =
    (preview && typeof preview.path === "string" && preview.path) ||
    args?.path ||
    "";
  // Core operations (git merge/push) always prompt regardless of mode/rules,
  // so "Mark Safe" (writes a rule) and "Allow for project" (would flip the
  // mode) are both no-ops there — hide them.
  const isCoreOp = approval.coreOperation === true;
  const projectScoped =
    !isCoreOp &&
    isProjectScoped(
      approval.toolName,
      approval.args as Record<string, unknown>
    );
  // Advisory badge (security review 2026-09-09 LOW-2): shell bypasses the
  // file-tool sandbox, so flag invocations whose text mentions a protected
  // path (.coding/, safety.toml, .git/) — the command text plus the resolved
  // cwd (appended "/" so a bare `.git`/`.coding` cwd matches; the space
  // separator keeps a command-final ".git" from junctioning with the slash).
  // File tools refuse those writes; shell cannot be sandboxed — the scan is
  // scoped to shell, the documented residual.
  const protectedTokens =
    approval.toolName === "shell" && typeof args?.command === "string"
      ? protectedPathTokens(`${args.command} ${args.cwd ?? ""}/`)
      : [];

  async function handleApprove() {
    // Honor the backend bool: only mark resolved when the oneshot was found.
    setResolveError(null);
    const ok = await approve(approval.toolCallId, "approve");
    if (ok) setResolved("approved");
    else setResolveError("Approval expired or already resolved.");
  }
  async function handleDeny() {
    setResolveError(null);
    const ok = await approve(approval.toolCallId, "deny");
    if (ok) setResolved("denied");
    else setResolveError("Approval expired or already resolved.");
  }
  async function handleDenyAll() {
    // Quality M1 / UI H2: deny this call and latch the rest of the turn.
    setResolveError(null);
    const ok = await approve(approval.toolCallId, "deny_all");
    if (ok) setResolved("denied");
    else setResolveError("Approval expired or already resolved.");
  }

  /**
   * Keyboard shortcuts while an approval is pending: `A` = Approve, `D` =
   * Deny, `Shift+D` = Deny all. Guarded so we never hijack typing — if the
   * focus is in an editable element (input / textarea / select /
   * contentEditable) the keys fall through to the field. Disabled once
   * resolved. Attached to `window` so it works regardless of where focus is
   * in the chat.
   */
  useEffect(() => {
    if (resolved) return;
    function onKey(e: KeyboardEvent) {
      const t = e.target as HTMLElement | null;
      if (
        t &&
        (t.tagName === "INPUT" ||
          t.tagName === "TEXTAREA" ||
          t.tagName === "SELECT" ||
          t.isContentEditable)
      ) {
        return;
      }
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const key = e.key.toLowerCase();
      if (key === "a" && !e.shiftKey) {
        e.preventDefault();
        void handleApprove();
      } else if (key === "d" && e.shiftKey) {
        e.preventDefault();
        void handleDenyAll();
      } else if (key === "d") {
        e.preventDefault();
        void handleDeny();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [resolved, approval.toolCallId]);

  /**
   * "Mark Safe" — save a regex rule that auto-approves this exact tool call
   * signature in the future, then approve this call now. The rule is persisted
   * to `.coding/safety.toml` and takes effect immediately (the agent loop
   * mtime-checks the file on every call).
   */
  async function handleMarkSafe() {
    try {
      await addSafetyRule(
        approval.toolName,
        JSON.stringify(approval.args)
      );
    } catch (e) {
      // If saving the rule fails, still proceed to approve — the user
      // clicked this to approve the action; the rule is a bonus.
      console.error("failed to save safety rule:", e);
    }
    const ok = await approve(approval.toolCallId, "approve");
    if (ok) setResolved("approved");
  }

  /**
   * "Allow for project" — add a broad safety rule that auto-approves any
   * future call of this tool (e.g. `^file_edit:`), then approve this call
   * now. Unlike the old behavior (which flipped the global safety mode), this
   * is additive and persistent — it writes to `.coding/safety.toml` and never
   * changes the safety mode, so there is no desync risk. Only shown when the
   * current call is project-scoped and not a core operation.
   */
  async function handleAllowForProject() {
    try {
      await addSafetyRuleBroad(approval.toolName);
    } catch (e) {
      // If saving the rule fails, still proceed to approve — the user
      // clicked this to approve the action; the rule is a bonus.
      console.error("failed to save broad safety rule:", e);
    }
    const ok = await approve(approval.toolCallId, "approve");
    if (ok) setResolved("approved");
  }

  /**
   * "Mark Safe (same operation)" — save a command-class rule that
   * auto-approves this shell command's *operation* (ignoring cosmetic output
   * filtering like Select-String patterns and redirections) in future, then
   * approve this call now. Still prompts for chained (`&&`, `;`) or unknown
   * commands. Only shown for shell calls (not core ops). If the command
   * can't be classified, the rule isn't saved but the call is still approved.
   */
  async function handleMarkSafeClass() {
    try {
      await addSafetyRuleClass(
        approval.toolName,
        JSON.stringify(approval.args)
      );
    } catch (e) {
      // Can't classify this command (chained, unknown, or unparseable) —
      // don't save a rule, but still approve the current call.
      console.error("can't classify this command:", e);
    }
    const ok = await approve(approval.toolCallId, "approve");
    if (ok) setResolved("approved");
  }

  if (resolved) {
    return (
      <div className="flex items-center gap-2 rounded-lg border border-border bg-bg-tertiary px-3 py-2 text-sm">
        {resolved === "approved" ? (
          <Check className="h-4 w-4 text-green-400" />
        ) : (
          <X className="h-4 w-4 text-red-400" />
        )}
        <span className="text-slate-400">
          {approval.toolName} {resolved}
        </span>
      </div>
    );
  }

  return (
    <div className="rounded-lg border border-yellow-600/50 bg-yellow-950/20 p-3">
      {resolveError && (
        <div className="mb-2 text-xs text-red-300">{resolveError}</div>
      )}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium text-yellow-400">
            Approve {approval.toolName}?
          </span>
          {isFileEdit && filePath && (
            <button
              onClick={() => setShowDiff(!showDiff)}
              className="flex items-center gap-1 text-xs text-slate-400 hover:text-slate-200"
            >
              <FileCode className="h-3.5 w-3.5" />
              {filePath}
            </button>
          )}
        </div>
        <div className="flex gap-2">
          {projectScoped && (
            <button
              onClick={handleAllowForProject}
              title="Approve this call and add a rule so future calls of this tool auto-approve"
              className="flex items-center gap-1 rounded bg-amber-600 px-3 py-1 text-xs font-medium text-white hover:bg-amber-500"
            >
              <FolderCheck className="h-3.5 w-3.5" />
              Allow for project
            </button>
          )}
          {!isCoreOp && (
            <button
              onClick={handleMarkSafe}
              title="Approve this call and save a rule so future matching calls auto-approve"
              className="flex items-center gap-1 rounded bg-blue-600 px-3 py-1 text-xs font-medium text-white hover:bg-blue-500"
            >
              <ShieldCheck className="h-3.5 w-3.5" />
              Mark Safe
            </button>
          )}
          {approval.toolName === "shell" && !isCoreOp && (
            <button
              onClick={handleMarkSafeClass}
              title="Approve this call and auto-approve the same operation (ignoring output filtering) in future"
              className="flex items-center gap-1 rounded bg-indigo-600 px-3 py-1 text-xs font-medium text-white hover:bg-indigo-500"
            >
              <Layers className="h-3.5 w-3.5" />
              Mark Safe (same op)
            </button>
          )}
          <button
            onClick={handleApprove}
            title="Approve this call (shortcut: A)"
            className="flex items-center gap-1 rounded bg-green-600 px-3 py-1 text-xs font-medium text-white hover:bg-green-500"
          >
            <Check className="h-3.5 w-3.5" />
            Approve
          </button>
          <button
            onClick={handleDeny}
            title="Deny this call (shortcut: D)"
            className="flex items-center gap-1 rounded bg-red-600 px-3 py-1 text-xs font-medium text-white hover:bg-red-500"
          >
            <X className="h-3.5 w-3.5" />
            Deny
          </button>
          <button
            onClick={handleDenyAll}
            title="Deny this and all remaining actions this turn (shortcut: Shift+D)"
            className="flex items-center gap-1 rounded border border-red-500/60 bg-red-950/40 px-3 py-1 text-xs font-medium text-red-200 hover:bg-red-900/50"
          >
            <X className="h-3.5 w-3.5" />
            Deny all
          </button>
        </div>
      </div>
      {protectedTokens.length > 0 && (
        <div className="mt-2 flex items-center gap-1.5 rounded border border-red-500/40 bg-red-950/30 px-2 py-1 text-xs text-red-300">
          <ShieldAlert className="h-3.5 w-3.5 shrink-0" />
          <span>
            Touches protected path{protectedTokens.length > 1 ? "s" : ""}: {" "}
            {protectedTokens.join(", ")} — shell writes bypass the file-tool
            sandbox; review before approving.
          </span>
        </div>
      )}
      {showDiff && isFileEdit && (
        <div className="mt-3">
          {preview?.kind === "diff" && typeof preview.diff === "string" ? (
            <UnifiedDiffView diffText={preview.diff} />
          ) : preview?.kind === "new_file" &&
            typeof preview.content === "string" ? (
            <NewFileView content={preview.content} />
          ) : approval.toolName === "file_edit" &&
            args.old_string !== undefined &&
            args.new_string !== undefined ? (
            <DiffView oldText={args.old_string} newText={args.new_string} />
          ) : args.content !== undefined ? (
            <NewFileView content={args.content} />
          ) : null}
        </div>
      )}
    </div>
  );
}
