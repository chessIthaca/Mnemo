// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Command classification for `command_class` safety rules.
//!
//! A [`classify`] normalizes a shell command into a **safety class**: the set
//! of *primary* commands across all pipeline segments, ignoring cosmetic
//! output filtering (cmdlets like `Select-String`, `Out-String`, redirections
//! like `2>&1`). Two commands with the same class are considered equivalent
//! for auto-approval — `cargo test 2>&1 | Select-String -Pattern "foo"` and
//! `cargo test | Select-String -Pattern "bar"` both classify to `cargo test`.
//!
//! ## Fail-safe design
//!
//! The classifier is deliberately conservative: anything it cannot confidently
//! classify returns `None`, and a `None` class is **never** auto-approved by a
//! `command_class` rule (the call falls through to a prompt). Specifically:
//!
//! - **Command chaining** (`;`, `&&`, `||`, `&`) splits into multiple
//!   statements. If *any* statement is unclassifiable, the whole command is
//!   unclassifiable. This closes the chained-command hole that pure prefix
//!   regex can't: `^shell:cargo test` would match `cargo test && rm -rf x`,
//!   but the classifier splits on `&&`, sees `rm` is unknown, and returns
//!   `None` → prompt.
//! - **Non-filter pipeline stages**: a pipeline stage after the first that
//!   isn't a known filter cmdlet makes the statement unclassifiable.
//! - **Unknown primaries / subcommands**: a primary command not in
//!   [`SAFE_COMMANDS`], or an unrecognized subcommand for a command that
//!   takes one, makes the statement unclassifiable.
//! - **Assignments / subexpressions**: a statement that's a PowerShell
//!   assignment (`$x = ...`) or starts with `(` is unclassifiable (we don't
//!   parse PowerShell expressions). A bare string/variable output expression
//!   (`"exit=$LASTEXITCODE"`) is benign and skipped — this is the common
//!   trailing pattern the codebase uses to print an exit code.
//!
//! This is a heuristic tokenizer, not a real PowerShell parser. The safe
//! failure mode is always `None` → prompt.

/// Primary commands considered safe to classify (the operation itself is
/// trusted; only the *class* — which operation — is what a rule approves). A
/// primary not in this set makes the statement unclassifiable.
const SAFE_COMMANDS: &[&str] = &[
    "cargo",
    "rustc",
    "npm",
    "npx",
    "node",
    "git",
    "Get-ChildItem",
    "Get-Location",
    "Get-Item",
    "ls",
    "dir",
    "echo",
    "Write-Output",
];

/// Subcommands kept as part of the class for `cargo`/`rustc`. An unrecognized
/// non-flag second token → unclassifiable (conservative: `cargo test123` →
/// `None`, since we can't be sure what operation that is).
const CARGO_SUBS: &[&str] = &[
    "test",
    "build",
    "check",
    "clippy",
    "fmt",
    "run",
    "bench",
    "doc",
    "clean",
    "add",
    "rm",
    "init",
    "update",
    "fetch",
    "verify-project",
];

/// Subcommands kept as part of the class for `git`.
const GIT_SUBS: &[&str] = &[
    "diff", "log", "status", "branch", "stash", "fetch", "pull", "show", "tag", "add", "rm",
    "commit", "merge", "rebase", "switch", "checkout", "restore", "reset",
];

/// Subcommands kept as part of the class for `npm`.
const NPM_SUBS: &[&str] = &[
    "run",
    "install",
    "uninstall",
    "update",
    "test",
    "ci",
    "init",
];

/// Recognized subcommands for a tool invoked via `npx <tool> <sub>` (the third
/// token, e.g. `npx vite build`). The tool name (2nd token) is kept
/// unvalidated — it's whatever tool npx runs.
const NPX_TOOL_SUBS: &[&str] = &["build", "dev", "preview", "test", "lint", "serve"];

/// Filter cmdlets that may appear as non-first pipeline stages. They only
/// transform output and are ignored for classification. A non-first stage
/// that isn't in this set makes the statement unclassifiable.
const FILTER_CMDLETS: &[&str] = &[
    "Select-String",
    "Out-String",
    "Select-Object",
    "Where-Object",
    "Sort-Object",
    "ForEach-Object",
    "Out-Null",
    "Out-Default",
    "Out-Host",
    "Tee-Object",
    "Group-Object",
    "Measure-Object",
];

/// Classify a shell command into a normalized safety class.
///
/// Returns the semicolon-joined primaries (e.g. `"cargo test"`,
/// `"cd;npm run build"`, `"git diff"`), or `None` when the command cannot be
/// classified. A `None` result must never be auto-approved — the caller
/// prompts.
pub fn classify(command: &str) -> Option<String> {
    // A PowerShell subexpression `$(...)` executes arbitrary code. Check the
    // RAW command BEFORE strip_redirections — a redirect target with no space
    // (e.g. `>out$(evil).txt`) is consumed wholesale by the redirect stripper,
    // which would remove the `$(` before this guard could see it. Checking the
    // raw input catches `$(` anywhere, regardless of later stripping.
    if command.contains("$(") {
        return None;
    }
    // Strip redirection tokens (2>&1, >file, etc.) FIRST, before statement
    // splitting — otherwise the `&` in `2>&1` would look like the background
    // operator and split the command wrongly.
    let stripped = strip_redirections(command);
    let statements = split_statements(&stripped);
    let mut primaries: Vec<String> = Vec::new();
    for stmt in statements {
        let stmt = stmt.trim();
        if stmt.is_empty() {
            continue;
        }
        // A PowerShell subexpression `$(...)` executes arbitrary code. If any
        // statement contains one, the whole command is unclassifiable — we
        // cannot know what it runs. This is the primary guard against the
        // interpolation/subexpression injection vectors (a `$(...)` inside a
        // double-quoted string, a bare `$var`, or an argument all execute).
        if stmt.contains("$(") {
            return None;
        }
        // A bare output expression (string literal or bare variable) is
        // benign — skip it without adding a primary or failing. This is the
        // common `; "exit=$LASTEXITCODE"` trailing pattern.
        if is_benign_expression(stmt) {
            continue;
        }
        let primary = classify_statement(stmt)?;
        primaries.push(primary);
    }
    if primaries.is_empty() {
        return None;
    }
    Some(primaries.join(";"))
}

/// Split a command into statements on the chaining operators `;`, `&&`,
/// `||`, and `&` (background), respecting quotes. Multi-char operators (`&&`,
/// `||`) are consumed as one separator so a single `&`/`|` left behind is a
/// genuine pipeline pipe (handled by [`split_pipeline`]). Consecutive
/// separators produce empty segments the caller skips.
fn split_statements(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut chars = s.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut in_backtick = false;
    while let Some(ch) = chars.next() {
        // Toggle quote state FIRST (regardless of whether we're at the top
        // level) — a closing quote inside a quoted region must toggle the
        // state off so the following `;` is seen as a separator.
        match ch {
            '\'' if !in_double && !in_backtick => {
                in_single = !in_single;
                current.push(ch);
                continue;
            }
            '"' if !in_single && !in_backtick => {
                in_double = !in_double;
                current.push(ch);
                continue;
            }
            '`' if !in_single && !in_double => {
                in_backtick = !in_backtick;
                current.push(ch);
                continue;
            }
            _ => {}
        }
        let at_top = !in_single && !in_double && !in_backtick;
        if at_top {
            match ch {
                ';' | '&' => {
                    // `&&` is one separator; consume the second `&` if present.
                    if ch == '&' && matches!(chars.peek(), Some('&')) {
                        chars.next();
                    }
                    parts.push(current.clone());
                    current.clear();
                    continue;
                }
                '|' => {
                    // `||` is one separator; a single `|` is a pipeline pipe
                    // (kept in the current statement for split_pipeline).
                    if matches!(chars.peek(), Some('|')) {
                        chars.next();
                        parts.push(current.clone());
                        current.clear();
                        continue;
                    }
                    // Single `|` — pipeline separator, keep it in the stage.
                    current.push(ch);
                    continue;
                }
                _ => {}
            }
        }
        current.push(ch);
    }
    parts.push(current);
    parts
}

/// Split a statement (no chaining operators left) into pipeline stages on `|`,
/// respecting quotes. By this point any `||` has been consumed as a statement
/// separator, so a remaining `|` is a genuine pipeline pipe.
fn split_pipeline(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut in_backtick = false;
    for ch in s.chars() {
        // Toggle quote state FIRST (a closing quote inside a quoted region
        // must toggle the state off so a following `|` is seen as a pipe).
        match ch {
            '\'' if !in_double && !in_backtick => {
                in_single = !in_single;
                current.push(ch);
                continue;
            }
            '"' if !in_single && !in_backtick => {
                in_double = !in_double;
                current.push(ch);
                continue;
            }
            '`' if !in_single && !in_double => {
                in_backtick = !in_backtick;
                current.push(ch);
                continue;
            }
            '|' if !in_single && !in_double && !in_backtick => {
                parts.push(current.clone());
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(ch);
    }
    parts.push(current);
    parts
}

/// Whether a statement is a benign output expression — a string literal or a
/// bare variable reference with no top-level `=` (so not an assignment).
/// These are skipped (they produce output but run no command). An assignment
/// (`$x = ...`) is NOT benign (it's unclassifiable → the caller returns None).
fn is_benign_expression(stmt: &str) -> bool {
    let s = stmt.trim_start();
    match s.chars().next() {
        // A string literal — benign (can't be an assignment target). The `=`
        // inside `"exit=$x"` is within the quotes, not an assignment.
        Some('"') | Some('\'') => true,
        // A variable reference: benign only if there's no top-level `=` (which
        // would make it an assignment). `$x` alone → benign; `$x = ...` → not.
        Some('$') => !has_top_level_equals(s),
        _ => false,
    }
}

/// Whether `s` contains a `=` at the top level (outside quotes). Used to tell
/// `$x` (benign) from `$x = cargo test` (assignment → unclassifiable).
fn has_top_level_equals(s: &str) -> bool {
    let mut in_single = false;
    let mut in_double = false;
    for ch in s.chars() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '=' if !in_single && !in_double => return true,
            _ => {}
        }
    }
    false
}

/// Classify a single statement (no chaining operators at top level) into its
/// primary command class, or `None` if unclassifiable.
fn classify_statement(stmt: &str) -> Option<String> {
    // A statement starting with `(` is a subexpression we can't parse.
    if stmt.trim_start().starts_with('(') {
        return None;
    }
    let stages = split_pipeline(stmt);
    let mut stages = stages.into_iter().filter(|s| !s.trim().is_empty());
    let first = stages.next()?;
    let primary = classify_primary(first.trim())?;
    // Every subsequent stage must be a known filter cmdlet.
    for stage in stages {
        if !is_filter_stage(stage.trim()) {
            return None;
        }
    }
    Some(primary)
}

/// Classify the primary (first) stage of a pipeline into its class string.
/// Returns `None` if the command is unknown or has an unrecognized subcommand.
fn classify_primary(stage: &str) -> Option<String> {
    let tokens = tokenize_stage(stage);
    let cmd = tokens.first()?;
    // Reject any token containing shell metacharacters that could execute or
    // nest commands: `(` (subexpression/argument injection), `)` (close), or a
    // backtick (PowerShell subexpression). A classified command must be a
    // plain command + recognized subcommand — never an expression. This runs
    // BEFORE the `cd` short-circuit so `cd (Remove-Item ...)` is caught (a
    // parenthesized argument is a subexpression that executes).
    for tok in &tokens {
        if tok.contains('(') || tok.contains(')') || tok.contains('`') {
            return None;
        }
    }
    // `cd <dir>` → just `cd` (the directory is irrelevant to the safety class).
    if cmd == "cd" {
        return Some("cd".to_string());
    }
    if !SAFE_COMMANDS.contains(&cmd.as_str()) {
        return None;
    }
    // Per-command subcommand retention.
    let class = match cmd.as_str() {
        "cargo" | "rustc" => with_subcommand(&cmd, &tokens, CARGO_SUBS)?,
        "git" => with_subcommand(&cmd, &tokens, GIT_SUBS)?,
        "npm" => npm_class(&tokens)?,
        "npx" => npx_class(&tokens)?,
        // Get-ChildItem, Get-Location, Get-Item, ls, dir, echo, Write-Output,
        // node — just the command (args dropped).
        _ => cmd.clone(),
    };
    Some(class)
}

/// `cmd <sub>` where `sub` must be a recognized subcommand. If there's a
/// second token that's a non-flag word but NOT recognized → `None`
/// (conservative: `cargo test123` → None). A leading flag (e.g. `git -C ...`,
/// `cargo --release ...`) → `None` — we can't know what operation follows the
/// flags, and collapsing to just the command name would let a saved bare-`cmd`
/// rule auto-approve a flag-prefixed variant (e.g. `git -C <anywhere> checkout`
/// matching a `git` rule). If the second token is absent → just `cmd`.
fn with_subcommand(cmd: &str, tokens: &[String], subs: &[&str]) -> Option<String> {
    match tokens.get(1) {
        Some(sub) if !sub.starts_with('-') => {
            if subs.contains(&sub.as_str()) {
                Some(format!("{cmd} {sub}"))
            } else {
                // Unrecognized non-flag subcommand → can't classify safely.
                None
            }
        }
        // A leading flag makes the subcommand unclassifiable — the real
        // subcommand (if any) is pushed past the flags and we can't safely
        // determine the operation.
        Some(_) => None,
        // No second token → just the command.
        None => Some(cmd.to_string()),
    }
}

/// `npm <sub> [script]`. `npm run build` → `npm run build` (the script name is
/// kept when the subcommand is `run`). `npm install` → `npm install`.
fn npm_class(tokens: &[String]) -> Option<String> {
    let cmd = &tokens[0];
    let sub = match tokens.get(1) {
        Some(s) if !s.starts_with('-') => s,
        // A leading flag (e.g. `npm --prefix /evil run build`) makes the
        // subcommand unclassifiable — collapsing to bare `npm` would let a
        // saved `npm` rule auto-approve flag-prefixed variants.
        _ => return None,
    };
    if !NPM_SUBS.contains(&sub.as_str()) {
        return None;
    }
    // For `npm run <script>`, keep the script name (3rd token if non-flag).
    if sub == "run" {
        if let Some(script) = tokens.get(2) {
            if !script.starts_with('-') {
                return Some(format!("{cmd} {sub} {script}"));
            }
        }
    }
    Some(format!("{cmd} {sub}"))
}

/// `npx <tool> [sub]`. The tool (2nd token) is kept unvalidated — it's
/// whatever tool npx runs. A recognized subcommand (3rd token) is also kept:
/// `npx vite build` → `npx vite build`, `npx tsc --noEmit` → `npx tsc`.
fn npx_class(tokens: &[String]) -> Option<String> {
    let cmd = &tokens[0];
    let tool = match tokens.get(1) {
        Some(t) if !t.starts_with('-') => t,
        // A leading flag (e.g. `npx --yes <evil>`) makes the tool
        // unclassifiable — collapsing to bare `npx` would let a saved `npx`
        // rule auto-approve flag-prefixed variants.
        _ => return None,
    };
    // Keep a recognized subcommand (3rd token) if present.
    if let Some(sub) = tokens.get(2) {
        if !sub.starts_with('-') && NPX_TOOL_SUBS.contains(&sub.as_str()) {
            return Some(format!("{cmd} {tool} {sub}"));
        }
    }
    Some(format!("{cmd} {tool}"))
}

/// Whether a non-first pipeline stage is a known filter cmdlet (cosmetic
/// output transformation). Anything else makes the statement unclassifiable.
fn is_filter_stage(stage: &str) -> bool {
    let tokens = tokenize_stage(stage);
    match tokens.first() {
        Some(cmd) => FILTER_CMDLETS.contains(&cmd.as_str()),
        None => false,
    }
}

/// Tokenize a pipeline stage into whitespace-separated tokens, respecting
/// quotes (so `-Pattern "a b"` stays one token). Quote chars are stripped from
/// the token content (we only inspect the first 1-3 tokens).
fn tokenize_stage(stage: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    for ch in stage.chars() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    tokens.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Remove redirection tokens from a statement so they don't confuse the
/// pipeline-stage parser. e.g. `cargo test 2>&1 | ...` → `cargo test  | ...`.
///
/// This is a **quote-aware, separator-safe** char scan (not a regex) because
/// the redirect target must NOT cross statement separators (`;`, `&`) or
/// pipeline pipes (`|`) — otherwise `cargo test>f;rm -rf x` would have `>f;rm`
/// gobbled into one redirect target, hiding the chained `rm` and classifying
/// the whole thing as `cargo test` (a security hole). It also must not strip
/// inside quotes (`echo "a > b"` keeps its `>`).
fn strip_redirections(stmt: &str) -> String {
    let mut out = String::with_capacity(stmt.len());
    let mut chars = stmt.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    while let Some(ch) = chars.next() {
        // Track quote state (backticks are PowerShell's escape/substitution
        // char; treat them as a quote context so `>` after a backtick isn't
        // stripped).
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
                out.push(ch);
                continue;
            }
            '"' if !in_single => {
                in_double = !in_double;
                out.push(ch);
                continue;
            }
            _ => {}
        }
        // Redirects only at the top level (not inside quotes).
        if in_single || in_double {
            out.push(ch);
            continue;
        }
        // A redirect is: optional leading digits, then `>` (or `>>`), then an
        // optional target (`&\d+`, `$null`, or a bare word that stops at
        // whitespace / a separator / a quote / another redirect).
        let is_redirect_start = ch == '>' || (ch.is_ascii_digit() && chars.peek() == Some(&'>'));
        if is_redirect_start {
            // Consume leading digits (already have one in `ch` if it's a digit).
            if ch.is_ascii_digit() {
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_digit() {
                        chars.next();
                    } else {
                        break;
                    }
                }
                // Consume the `>`.
                chars.next();
            }
            // Consume an optional second `>` (append redirect `>>`).
            if chars.peek() == Some(&'>') {
                chars.next();
            }
            // Consume the target, if any.
            match chars.peek() {
                Some('&') => {
                    chars.next(); // the &
                    while let Some(&d) = chars.peek() {
                        if d.is_ascii_digit() {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                }
                Some('$') => {
                    chars.next(); // the $
                    while let Some(&w) = chars.peek() {
                        if w.is_alphanumeric() || w == '_' {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                }
                _ => {
                    // A bare-word target: consume until a terminator (whitespace,
                    // separator, pipe, quote, or another redirect char). This is
                    // the key safety guard — it cannot cross `;`/`&`/`|`.
                    while let Some(&w) = chars.peek() {
                        if w.is_whitespace()
                            || w == ';'
                            || w == '&'
                            || w == '|'
                            || w == '<'
                            || w == '>'
                            || w == '\''
                            || w == '"'
                        {
                            break;
                        }
                        chars.next();
                    }
                }
            }
            // The redirect (and its target) is consumed; emit nothing.
            continue;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- basic classification ------------------------------------------------

    #[test]
    fn simple_command_no_filtering() {
        assert_eq!(classify("cargo test"), Some("cargo test".to_string()));
        assert_eq!(classify("cargo build"), Some("cargo build".to_string()));
        assert_eq!(classify("cargo check"), Some("cargo check".to_string()));
    }

    #[test]
    fn command_with_select_string_filter() {
        // The Select-String pattern is cosmetic — same class as bare command.
        assert_eq!(
            classify(r#"cargo test 2>&1 | Select-String -Pattern "foo""#),
            Some("cargo test".to_string())
        );
        assert_eq!(
            classify(r#"cargo test 2>&1 | Select-String -Pattern "bar""#),
            Some("cargo test".to_string())
        );
        // Both variants classify identically → a single rule covers both.
        let a = classify(r#"cargo test 2>&1 | Select-String -Pattern "x""#);
        let b = classify(r#"cargo test 2>&1 | Select-String -Pattern "y""#);
        assert_eq!(a, b);
    }

    #[test]
    fn cd_then_command() {
        assert_eq!(
            classify("cd frontend; npm run build | Select-Object -Last 5"),
            Some("cd;npm run build".to_string())
        );
        assert_eq!(
            classify("cd src-tauri; cargo test"),
            Some("cd;cargo test".to_string())
        );
    }

    #[test]
    fn git_diff_drops_path_arg() {
        // The specific file is irrelevant to the safety class.
        assert_eq!(classify("git diff a.ts"), Some("git diff".to_string()));
        assert_eq!(classify("git diff b.ts"), Some("git diff".to_string()));
        assert_eq!(classify("git diff"), Some("git diff".to_string()));
        assert_eq!(
            classify("git branch --show-current"),
            Some("git branch".to_string())
        );
        assert_eq!(
            classify("git log --oneline -3"),
            Some("git log".to_string())
        );
    }

    #[test]
    fn get_childitem_classified() {
        assert_eq!(
            classify("Get-ChildItem -Path . -Name | Select-Object -First 20"),
            Some("Get-ChildItem".to_string())
        );
        assert_eq!(classify("Get-Location"), Some("Get-Location".to_string()));
    }

    #[test]
    fn npx_and_npm_keep_tool_and_script() {
        assert_eq!(classify("npx tsc --noEmit"), Some("npx tsc".to_string()));
        assert_eq!(
            classify("npx vite build"),
            Some("npx vite build".to_string())
        );
        assert_eq!(classify("npm run build"), Some("npm run build".to_string()));
        assert_eq!(classify("npm install"), Some("npm install".to_string()));
    }

    // --- the trailing "exit=$LASTEXITCODE" pattern --------------------------

    #[test]
    fn trailing_exit_code_string_is_skipped() {
        // The common codebase pattern: `cmd; "exit=$LASTEXITCODE"`. The bare
        // string is a benign output expression — skipped, not unclassifiable.
        assert_eq!(
            classify(r#"cargo test 2>&1 | Out-Null; "exit=$LASTEXITCODE""#),
            Some("cargo test".to_string())
        );
        assert_eq!(
            classify(r#"cd frontend; npx tsc --noEmit; "exit=$LASTEXITCODE""#),
            Some("cd;npx tsc".to_string())
        );
        assert_eq!(
            classify(
                r#"cd frontend; npm run build 2>&1 | Select-Object -Last 30; "exit=$LASTEXITCODE""#
            ),
            Some("cd;npm run build".to_string())
        );
    }

    // --- fail-safe: unclassifiable → None ------------------------------------

    #[test]
    fn chained_unknown_command_is_none() {
        // `rm` is not in SAFE_COMMANDS → the whole command is unclassifiable,
        // even though `cargo test` alone would be safe. This is the key
        // chained-command guard.
        assert_eq!(classify("cargo test && rm -rf x"), None);
        assert_eq!(classify("cargo test; rm -rf x"), None);
        assert_eq!(classify("cargo test || rm -rf x"), None);
    }

    #[test]
    fn unrecognized_subcommand_is_none() {
        // `test123` is not a recognized cargo subcommand → None (we can't be
        // sure what operation `cargo test123` is).
        assert_eq!(classify("cargo test123"), None);
    }

    #[test]
    fn non_filter_pipeline_stage_is_none() {
        // A second stage that isn't a known filter cmdlet → unclassifiable.
        assert_eq!(classify("cargo test | somecmd"), None);
    }

    #[test]
    fn assignment_statement_is_none() {
        // PowerShell property assignment — unparseable by this tokenizer.
        assert_eq!(
            classify("(Get-Item x).LastWriteTime = Get-Date; cargo build"),
            None
        );
        // A variable assignment is also unclassifiable (not a benign expr).
        assert_eq!(classify("$output = cargo test"), None);
    }

    // --- quote handling ------------------------------------------------------

    #[test]
    fn quoted_semicolon_not_split() {
        // A semicolon inside a double-quoted string is not a statement separator.
        assert_eq!(classify(r#"echo "a;b""#), Some("echo".to_string()));
        assert_eq!(
            classify(r#"git commit -m "a; b; c""#),
            Some("git commit".to_string())
        );
    }

    #[test]
    fn single_quoted_semicolon_not_split() {
        assert_eq!(classify(r#"echo 'a;b'"#), Some("echo".to_string()));
    }

    // --- edge cases ----------------------------------------------------------

    #[test]
    fn empty_command_is_none() {
        assert_eq!(classify(""), None);
        assert_eq!(classify("   "), None);
    }

    #[test]
    fn multiple_filters_allowed() {
        // Any number of known filter stages is fine.
        assert_eq!(
            classify("cargo test 2>&1 | Out-String -Stream | Select-String -Pattern x"),
            Some("cargo test".to_string())
        );
    }

    #[test]
    fn redirection_only_stripped() {
        // `2>&1` alone with a command still classifies.
        assert_eq!(classify("cargo test 2>&1"), Some("cargo test".to_string()));
        assert_eq!(
            classify("cargo build 2>&1 | Out-Null"),
            Some("cargo build".to_string())
        );
    }

    #[test]
    fn bare_variable_is_benign() {
        // A bare `$LASTEXITCODE` (no `=`) is a benign output expression.
        assert_eq!(
            classify("cargo test; $LASTEXITCODE"),
            Some("cargo test".to_string())
        );
    }

    // --- CRITICAL: redirect-stripping must not hide chained commands ---------
    // A redirect target must NOT cross statement separators (`;`, `&`) or
    // pipes (`|`). Otherwise `cargo test>f;rm -rf x` would have `>f;rm`
    // gobbled into one redirect, hiding the chained `rm` and classifying the
    // whole thing as `cargo test` (auto-approve bypass).

    #[test]
    fn redirect_target_does_not_cross_semicolon() {
        assert_eq!(classify("cargo test>f;rm -rf x"), None);
        assert_eq!(classify("cargo test>out.txt;rm -rf x"), None);
    }

    #[test]
    fn redirect_target_does_not_cross_ampersand() {
        assert_eq!(classify("cargo test>f&&rm -rf x"), None);
        assert_eq!(classify("cargo test>out.txt&&rm -rf x"), None);
    }

    #[test]
    fn redirect_target_does_not_cross_pipe() {
        // `>f|rm` — the pipe must still split the pipeline, and `rm` is an
        // unknown non-filter stage → None.
        assert_eq!(classify("cargo test>f|rm"), None);
    }

    #[test]
    fn redirect_inside_quotes_not_stripped() {
        // A `>` inside a quoted string is literal text, not a redirect.
        assert_eq!(classify(r#"echo "a > b""#), Some("echo".to_string()));
        // And a chained command after the closing quote is still seen.
        assert_eq!(classify(r#"echo "a > b"; rm x"#), None);
    }

    #[test]
    fn legit_redirect_with_space_still_classifies() {
        // `cargo test 2>&1` (the common pattern) still works.
        assert_eq!(classify("cargo test 2>&1"), Some("cargo test".to_string()));
        assert_eq!(
            classify("cargo test > out.txt"),
            Some("cargo test".to_string())
        );
        assert_eq!(
            classify("cargo build 2>&1 | Out-Null"),
            Some("cargo build".to_string())
        );
    }

    // --- A2: auto-approve bypass vectors (all must classify to None) --------

    #[test]
    fn subexpression_vectors_are_none() {
        // Vector 1: a standalone subexpression statement executes.
        assert_eq!(
            classify("cargo test; $(Remove-Item -Recurse -Force C:\\evil)"),
            None
        );
        // Vector 2: an interpolated double-quoted string executes $(...).
        assert_eq!(
            classify(r#"cargo test; "exit $(Remove-Item -Recurse C:\evil)""#),
            None
        );
        // Vector 3: a subexpression inside an argument.
        assert_eq!(
            classify("cargo test $(Remove-Item -Recurse C:\\evil)"),
            None
        );
        // Vector 4: leading flag collapses the class — must be None, not bare "git".
        assert_eq!(classify(r"git -C ..\other checkout -- ."), None);
    }

    #[test]
    fn leading_flag_makes_command_unclassifiable() {
        // A leading flag after the command means we can't know the real
        // subcommand — must return None, never collapse to bare command.
        assert_eq!(classify("git -c core.autocrlf=false status"), None);
        assert_eq!(classify("cargo --release build"), None);
        assert_eq!(classify("npm --prefix /evil run build"), None);
        assert_eq!(classify("npx --yes some-tool"), None);
    }

    #[test]
    fn subcommand_with_flags_still_classifies() {
        // A recognized subcommand followed by flags is fine — the subcommand
        // is tokens[1] (not a flag), so it classifies normally.
        assert_eq!(
            classify("git branch --show-current"),
            Some("git branch".to_string())
        );
        assert_eq!(
            classify("git log --oneline -3"),
            Some("git log".to_string())
        );
        assert_eq!(classify("npx tsc --noEmit"), Some("npx tsc".to_string()));
    }

    #[test]
    fn token_with_paren_or_backtick_is_none() {
        // Any classified token containing (, ), or backtick → None.
        assert_eq!(classify("cargo test(foo)"), None);
        assert_eq!(classify("cargo test`evil`"), None);
        assert_eq!(classify("git diff a)b"), None);
    }

    #[test]
    fn cd_with_parenthesized_arg_is_none() {
        // A parenthesized argument to `cd` is a subexpression that executes —
        // must be caught (the metachar guard runs BEFORE the cd short-circuit).
        assert_eq!(classify("cd (Remove-Item -Recurse -Force C:\\evil)"), None);
        assert_eq!(classify("cd (evil)"), None);
    }

    #[test]
    fn redirect_target_subexpression_is_none() {
        // A `$(` inside a no-space redirect target is stripped by
        // strip_redirections before the per-statement guard — but the raw
        // command guard at the top of classify() catches it first.
        assert_eq!(classify("cargo test >out$(evil).txt"), None);
        assert_eq!(classify("cargo test>out$(evil).txt"), None);
    }

    #[test]
    fn commit_message_with_parens_prompts() {
        // A commit message containing parens is a common, legitimate pattern,
        // but the metachar guard (which rejects `(` in any token) now makes it
        // unclassifiable → prompts. This is fail-safe (prompts, never
        // auto-approves) and is the documented trade-off of the guard.
        assert_eq!(classify(r#"git commit -m "fix (issue #123)""#), None);
        // A commit message without parens still classifies normally.
        assert_eq!(
            classify(r#"git commit -m "fix issue 123""#),
            Some("git commit".to_string())
        );
    }
}
