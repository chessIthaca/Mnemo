// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Protected-path tokens whose presence in a shell command signals a write to
 * a control-plane location (security review 2026-09-09 LOW-2): the project's
 * agent-state directory, the safety rules file inside it, and the git control
 * plane. The file tools refuse writes to the control-plane locations under
 * them (.coding/plans|reviews|knowledge/, safety.toml, backlog.jsonl, the
 * DBs, and any .git component), but shell cannot be sandboxed the same way —
 * an approved shell command can write any of them.
 */
export const PROTECTED_PATH_TOKENS = [
  ".coding/",
  "safety.toml",
  ".git/",
] as const;

/**
 * Advisory scan for the approval dialog (security review 2026-09-09 LOW-2):
 * which protected-path tokens does this shell invocation text mention? The
 * caller passes the command text joined with the resolved `cwd` (appended
 * with a `/`, so a bare `.git`/`.coding` cwd matches the slash-bearing
 * tokens). Shell bypasses the file-tool sandbox, so an approved command CAN
 * write the protected dirs (.coding/reviews/, .coding/plans/) and .git/ —
 * the approval badge makes that residual risk visible at decision time. The
 * scan is a static substring check, deliberately coarse: it is advisory
 * only and never gates the approval, and slash-less command phrasings
 * (`cd .git && …`, `git -C .git …`) do not match.
 *
 * Case-insensitive (Windows paths are, and a command can reference `.GIT/`
 * or `.Coding/` and still hit the same location), and backslashes are
 * normalized to forward slashes before matching — mirroring the Rust
 * guard's Windows-path handling (`Sandbox::is_protected_write_target`) —
 * so `.git\hooks` matches too. The `.git/` token keeps its trailing slash
 * so `.gitignore` (a normal project file) does not match.
 *
 * Returns the matched tokens in `PROTECTED_PATH_TOKENS` order (deduped by
 * construction), or `[]` when the text is clean.
 */
export function protectedPathTokens(command: string): string[] {
  const lower = command.replace(/\\/g, "/").toLowerCase();
  return PROTECTED_PATH_TOKENS.filter((token) => lower.includes(token));
}
