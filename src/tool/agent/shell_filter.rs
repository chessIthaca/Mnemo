// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Deterministic, command-aware filtering of shell tool output.
//!
//! The in-process take on the PandaFilter idea, minus the ML model: plan-step
//! commands (`cargo build/test`, `npm test`, `npm run build`, `git status`)
//! print hundreds of noise lines (Compiling…, passing test lines, asset
//! tables) that would otherwise flood the LLM transcript. A small pipeline of
//! pure regex handlers collapses that noise before the output reaches the
//! context window — fully local, deterministic, and unit-testable.
//!
//! Pipeline per stream (stdout and stderr are filtered separately):
//! 1. Strip ANSI escape sequences.
//! 2. Normalize lines (`\r\n` → `\n`, trailing whitespace trimmed).
//! 3. Apply the per-command handler — a **whitelist-drop** filter: only lines
//!    matching well-known noise patterns are removed, everything unclassified
//!    is kept. Handlers never drop what they don't recognize.
//! 4. Apply the user's `[shell_filter]` override (if one matches the command):
//!    `keep` patterns veto built-in drops as well as the override's own
//!    `drop` patterns.
//! 5. Dedup runs of identical consecutive lines.
//!
//! # Safety rules (hard)
//!
//! - Lines carrying an error marker ([`is_error_line`]: `error`, `failed`,
//!   `fail`, `panicked`, `err!`, case-insensitive for the words, and the
//!   glyphs `✗` / `×` / `✖`) are NEVER dropped — not by a built-in handler,
//!   not by a user `drop` override, not by dedup. A failing build/test must
//!   still show its failure.
//! - **One deliberate, structural exception:** a line a built-in handler
//!   identifies as a *passing* test (`test … … ok`, `✓ …`, `PASS …`, TAP
//!   `ok N …`) is dropped even when the test NAME contains a marker word
//!   (`test error_recovery … ok`, `✓ src/errors.test.ts`). Such a line
//!   structurally asserts success and can never represent a failure (failures
//!   print `FAILED` / `✗` / `✖` / `×`), so the never-hide-a-failure rule
//!   still holds. User `drop` overrides remain strictly vetoed by error
//!   markers.
//! - Unknown commands pass through with only the generic pipeline (ANSI strip
//!   + dedup + progress/spinner strip) — no content lines are removed.
//! - The filter only shapes the tool *result*; the raw unfiltered output stays
//!   in the tool result's `data` field and command execution is unaffected.

use std::sync::OnceLock;

use regex::Regex;

use crate::config::ShellFilterConfig;

/// A compiled user override from a `[[shell_filter.override]]` config entry.
pub(crate) struct CompiledOverride {
    /// Regex matched against the full command line (first match wins).
    command: Regex,
    /// Lines matching any of these are dropped (unless error-marked or kept).
    drop: Vec<Regex>,
    /// Lines matching any of these are always kept.
    keep: Vec<Regex>,
    /// Use the generic fallback instead of the built-in handler.
    disable_builtin: bool,
}

/// The built-in handler a command maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandlerKind {
    /// `cargo build` / `cargo check` / `cargo clippy` — drop compile-progress
    /// lines, keep errors, warnings, and the `Finished` summary.
    CargoBuild,
    /// `cargo test` — drop passing-test lines, keep FAILED blocks + the
    /// `test result:` summary.
    CargoTest,
    /// `npm test` / `npm run test` — vitest/jest: drop ✓/PASS lines, keep
    /// FAIL blocks + summaries.
    NpmTest,
    /// `npm run build` — vite/tsc: keep errors + the `built in` line, drop
    /// progress + asset tables.
    NpmBuild,
    /// `git status` — keep branch lines + section headers, collapse the
    /// per-section file lists into a single counts line.
    GitStatus,
    /// Anything else — generic fallback (dedup + progress/spinner strip only).
    Unknown,
}

/// Strip ANSI escape sequences (CSI colors/cursor codes, OSC hyperlinks,
/// charset selectors) from `text`.
pub(crate) fn strip_ansi(text: &str) -> String {
    static ANSI: OnceLock<Regex> = OnceLock::new();
    let re = ANSI.get_or_init(|| {
        Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[()][A-Z0-9]")
            .expect("static ANSI regex must compile")
    });
    re.replace_all(text, "").into_owned()
}

/// Does `line` carry an error marker that makes it unsinkable?
///
/// Hard safety rule: the words `error`, `failed`, `fail`, `panicked`
/// (case-insensitive) and the failure glyphs `✗` / `×` / `✖` mark a line as
/// error-bearing; every handler and every user override must keep such lines.
/// Deliberately a substring match — `failures:`, `AssertionError`, `not ok`
/// diagnostics and test names containing `fail` all count, and erring toward
/// "keep" is the whole point. `err!` covers npm's `npm ERR! code …` /
/// `npm ERR! Exit status 1` lines, which contain neither `error` nor `fail`
/// (review L1; the `!` keeps it from matching words like "merry"/"terrible").
pub(crate) fn is_error_line(line: &str) -> bool {
    if line.contains('✗') || line.contains('×') || line.contains('✖') {
        return true;
    }
    let lower = line.to_lowercase();
    lower.contains("error")
        || lower.contains("failed")
        || lower.contains("fail")
        || lower.contains("panicked")
        || lower.contains("err!")
}

/// Does `line` carry a warning marker (kept by the cargo/vite handlers)?
pub(crate) fn is_warning_line(line: &str) -> bool {
    line.to_lowercase().contains("warning")
}

/// Does the trimmed `line` look like a compiler-diagnostic continuation
/// (`--> file.rs:1:2`, source excerpts ` |`, ` = note:`)? Used to keep the
/// context of an error/warning that cargo/tsc split across lines.
fn is_diagnostic_continuation(trimmed: &str) -> bool {
    trimmed.starts_with("-->")
        || trimmed.starts_with('|')
        || trimmed.starts_with('=')
        || trimmed.starts_with("...")
}

/// Is the trimmed `line` a progress bar or terminal spinner frame?
///
/// Matches spinner-glyph-only lines (`⠙`, `|`, `\`, `-`, `...`) and bar
/// lines made exclusively of bar glyphs + a percentage (`[####----] 50%`).
/// A line with any other text (e.g. `tests: 50% passed`) never matches.
pub(crate) fn is_progress_line(trimmed: &str) -> bool {
    static SPINNER: OnceLock<Regex> = OnceLock::new();
    static BAR: OnceLock<Regex> = OnceLock::new();
    let spinner = SPINNER.get_or_init(|| {
        Regex::new(r"^[\s./\\|\-⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏]+$").expect("static spinner regex must compile")
    });
    let bar = BAR.get_or_init(|| {
        Regex::new(r"^[\s#=█▓▒░\-\[\]]*\d+(\.\d+)?%[\s#=█▓▒░\-\[\]]*$")
            .expect("static progress-bar regex must compile")
    });
    spinner.is_match(trimmed) || bar.is_match(trimmed)
}

/// Map a shell command to its built-in handler.
///
/// Matching is deliberately conservative: only clear, exact subcommand
/// prefixes classify. A `|` anywhere forces [`HandlerKind::Unknown`] (the
/// output has already been reshaped by a pipe, e.g. `| grep`). For `;`/`&&`
/// chains the LAST segment wins, so `cd frontend && npm test` still gets the
/// npm-test handler; since every handler only removes whitelisted noise lines
/// and never hides error-marked lines, a misclassification can only keep
/// *more* output than ideal — never less.
pub(crate) fn classify(command: &str) -> HandlerKind {
    if command.contains('|') {
        return HandlerKind::Unknown;
    }
    let last = command
        .rsplit(['&', ';'])
        .next()
        .unwrap_or(command)
        .trim()
        .to_string();
    let c = last.as_str();

    if let Some(rest) = c.strip_prefix("cargo ") {
        if rest == "build" || rest.starts_with("build ") {
            return HandlerKind::CargoBuild;
        }
        if rest == "check" || rest.starts_with("check ") {
            return HandlerKind::CargoBuild;
        }
        if rest == "clippy" || rest.starts_with("clippy ") {
            return HandlerKind::CargoBuild;
        }
        if rest == "test" || rest.starts_with("test ") {
            return HandlerKind::CargoTest;
        }
        return HandlerKind::Unknown;
    }

    if let Some(rest) = c.strip_prefix("npm ") {
        if rest == "test" || rest.starts_with("test ") {
            return HandlerKind::NpmTest;
        }
        if rest == "run test" || rest.starts_with("run test ") {
            return HandlerKind::NpmTest;
        }
        if rest == "run build" || rest.starts_with("run build") {
            return HandlerKind::NpmBuild;
        }
        return HandlerKind::Unknown;
    }

    if c == "git status" || c.starts_with("git status ") {
        return HandlerKind::GitStatus;
    }

    HandlerKind::Unknown
}

/// Compile the user overrides from config, silently skipping invalid regexes.
///
/// A broken `command` regex drops the whole override; a broken `drop`/`keep`
/// pattern drops just that pattern. A typo in config.toml must degrade the
/// rule, never crash the shell tool (and never disable the safety rules, which
/// are independent of overrides).
pub(crate) fn compile_overrides(cfg: &ShellFilterConfig) -> Vec<CompiledOverride> {
    cfg.overrides
        .iter()
        .filter_map(|o| {
            let command = Regex::new(&o.command).ok()?;
            let drop = o.drop.iter().filter_map(|p| Regex::new(p).ok()).collect();
            let keep = o.keep.iter().filter_map(|p| Regex::new(p).ok()).collect();
            Some(CompiledOverride {
                command,
                drop,
                keep,
                disable_builtin: o.disable_builtin,
            })
        })
        .collect()
}

/// The first override (if any) whose `command` regex matches the command.
fn matching_override<'a>(
    command: &str,
    overrides: &'a [CompiledOverride],
) -> Option<&'a CompiledOverride> {
    overrides.iter().find(|o| o.command.is_match(command))
}

/// The main entry: filter shell tool output for `command`.
///
/// Returns `(filtered_stdout, filtered_stderr)`. When the config disables the
/// filter the inputs are returned unchanged. The caller (the shell tool) is
/// responsible for keeping the raw output in its `data` field.
pub(crate) fn filter_shell_output(
    command: &str,
    stdout: &str,
    stderr: &str,
    cfg: &ShellFilterConfig,
) -> (String, String) {
    if !cfg.enabled {
        return (stdout.to_string(), stderr.to_string());
    }
    let overrides = compile_overrides(cfg);
    let ov = matching_override(command, &overrides);
    let kind = match ov {
        Some(o) if o.disable_builtin => HandlerKind::Unknown,
        _ => classify(command),
    };
    (
        filter_stream(stdout, kind, ov),
        filter_stream(stderr, kind, ov),
    )
}

/// Run the full pipeline on one stream (stdout or stderr).
fn filter_stream(text: &str, kind: HandlerKind, ov: Option<&CompiledOverride>) -> String {
    let plain = strip_ansi(text);
    let lines: Vec<String> = plain
        .split('\n')
        .map(|l| l.trim_end_matches('\r').trim_end().to_string())
        .collect();

    // 1. Built-in handler (whitelist drops only; `keep` patterns veto them).
    let keep: &[Regex] = ov.map(|o| o.keep.as_slice()).unwrap_or(&[]);
    let mut kept = match kind {
        HandlerKind::CargoBuild => filter_cargo_build(&lines, keep),
        HandlerKind::CargoTest => filter_cargo_test(&lines, keep),
        HandlerKind::NpmTest => filter_npm_test(&lines, keep),
        HandlerKind::NpmBuild => filter_npm_build(&lines, keep),
        HandlerKind::GitStatus => filter_git_status(&lines, keep),
        HandlerKind::Unknown => lines.clone(),
    };
    // 2. Progress-bar / spinner frames never carry information (applied to
    //    every handler; error-marked lines can't match these patterns).
    kept.retain(|l| !is_progress_line(l.trim()));
    // 3. User override: keep wins over drop; error markers win over both.
    if let Some(ov) = ov {
        apply_override(&mut kept, ov);
    }
    // 4. Dedup runs of identical consecutive lines (error lines excluded).
    dedup_lines(&mut kept);
    // 5. Trim leading/trailing blank lines (cargo/npm frame their output).
    while kept.first().is_some_and(|l| l.is_empty()) {
        kept.remove(0);
    }
    while kept.last().is_some_and(|l| l.is_empty()) {
        kept.pop();
    }
    kept.join("\n")
}

/// Apply a compiled user override: `keep` vetoes `drop`, and the global
/// error-marker rule vetoes both.
fn apply_override(lines: &mut Vec<String>, ov: &CompiledOverride) {
    let mut kept: Vec<String> = Vec::with_capacity(lines.len());
    for line in lines.drain(..) {
        if is_error_line(&line) || ov.keep.iter().any(|re| re.is_match(&line)) {
            kept.push(line);
            continue;
        }
        if ov.drop.iter().any(|re| re.is_match(&line)) {
            continue;
        }
        kept.push(line);
    }
    *lines = kept;
}

/// Collapse runs of identical consecutive lines into one occurrence, noting
/// large runs with a `... (N identical lines collapsed)` marker line.
/// Error-marked lines are never collapsed (a repeated failure is information).
fn dedup_lines(lines: &mut Vec<String>) {
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut hidden = 0usize;
    for line in lines.drain(..) {
        let repeats = out.last().is_some_and(|last| last == &line);
        if repeats && !is_error_line(&line) {
            hidden += 1;
            continue;
        }
        if hidden >= 2 {
            out.push(format!("  ... ({hidden} identical lines collapsed)"));
        }
        hidden = 0;
        out.push(line);
    }
    if hidden >= 2 {
        out.push(format!("  ... ({hidden} identical lines collapsed)"));
    }
    *lines = out;
}

/// Compile-progress prefixes dropped by the cargo handlers. Pure noise: each
/// crate printed a `Compiling …` line at every plan step.
fn cargo_noise_prefix(t: &str) -> bool {
    [
        "Compiling ",
        "Checking ",
        "Downloading ",
        "Downloaded ",
        "Fresh ",
        "Blocking ",
        "Locking ",
        "Generating ",
        "Building ",
        "Created ",
        "Uplifting ",
        "Skipping ",
        "Documenting ",
        "Installing ",
        "Uninstalling ",
    ]
    .iter()
    .any(|p| t.starts_with(p))
}

/// Does any `keep` pattern match `line`? Consulted at every built-in handler
/// drop point so a user `keep` override vetoes built-in drops as well as the
/// override's own `drop` patterns (review M1).
fn keep_matches(keep: &[Regex], line: &str) -> bool {
    keep.iter().any(|re| re.is_match(line))
}

/// `cargo build` / `check` / `clippy`: drop compile progress, keep errors,
/// warnings (with their diagnostic context), and the `Finished` summary line.
/// `keep` patterns veto every drop below.
fn filter_cargo_build(lines: &[String], keep: &[Regex]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_diag = false;
    for line in lines {
        let t = line.trim_start();
        // Structural noise first: a `Compiling thiserror v1.0` line is
        // progress, not a failure — the "error" is the CRATE NAME, and the
        // line cannot represent a failing build (failures print `error:` /
        // `error[E..]:`, which don't match these prefixes).
        if cargo_noise_prefix(t) && !keep_matches(keep, line) {
            continue; // drop
        }
        if is_error_line(line) || is_warning_line(line) {
            out.push(line.clone());
            in_diag = true;
            continue;
        }
        if in_diag && is_diagnostic_continuation(t) {
            out.push(line.clone());
            continue;
        }
        in_diag = false;
        if t.starts_with("Finished ") {
            out.push(line.clone()); // the final summary (time + profile)
            continue;
        }
        out.push(line.clone()); // unclassified → keep (never hide)
    }
    out
}

/// `cargo test`: drop passing-test lines and the build phase, keep FAILED
/// lines, the per-failure output blocks, and the `test result:` summary.
/// `keep` patterns veto every drop below.
fn filter_cargo_test(lines: &[String], keep: &[Regex]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut capture = false; // inside a failure block (---- / failures: / FAILED)
    let mut in_diag = false;
    for line in lines {
        let t = line.trim_start();
        // Structural pass first: `test error_recovery_helper ... ok` asserts
        // success — the "error" is the TEST NAME and the line cannot
        // represent a failure (a failing test ends `... FAILED`, which is
        // caught below). This is the deliberate, documented exception to the
        // error-marker veto.
        if t.starts_with("test ") && t.ends_with("... ok") && !keep_matches(keep, line) {
            continue; // passing test line — the bulk of the noise
        }
        if cargo_noise_prefix(t) && !keep_matches(keep, line) {
            continue; // the build phase before the tests run
        }
        if is_error_line(line) || is_warning_line(line) {
            out.push(line.clone());
            in_diag = true;
            // Every summary line carries "failed" ("0 failed"), so it lands
            // here rather than in the capture branch — reset capture here
            // too, otherwise a failure block would over-capture the whole
            // tail of the stream (review L2).
            if t.starts_with("test result:") {
                capture = false;
            } else if (t.starts_with("test ") && t.ends_with("FAILED"))
                || t.starts_with("failures:")
            {
                // A FAILED test line starts the failure block that follows it.
                capture = true;
            }
            continue;
        }
        if in_diag && is_diagnostic_continuation(t) {
            out.push(line.clone());
            continue;
        }
        in_diag = false;
        if capture {
            // Keep everything inside the block up to the summary — failure
            // output (expected/actual values) rarely carries error markers,
            // and over-keeping is the safe direction.
            out.push(line.clone());
            if t.starts_with("test result:") {
                capture = false;
            }
            continue;
        }
        if t.starts_with("---- ") {
            // A failure block header (or doc-test section).
            out.push(line.clone());
            capture = true;
            continue;
        }
        if t.starts_with("test result:") {
            out.push(line.clone()); // the summary: ok. N passed … / FAILED. …
            continue;
        }
        if t.starts_with("running ") && !keep_matches(keep, line) {
            continue; // "running N tests" header
        }
        out.push(line.clone()); // unclassified → keep
    }
    out
}

/// Is the trimmed line a passing-test line in a vitest/jest run?
fn is_npm_pass_line(t: &str) -> bool {
    t.starts_with('✓') || t.starts_with('✔') || t.starts_with('√') || t.starts_with("PASS ")
}

/// Summary/terminator lines that end a vitest/jest failure block (kept or
/// dropped by the caller's summary rules — they just end the capture).
fn is_npm_test_terminator(t: &str) -> bool {
    is_npm_pass_line(t)
        || t.starts_with("Test Files")
        || t.starts_with("Tests ")
        || t.starts_with("Snapshots") // vitest: "Snapshots  0"; jest: "Snapshots:"
        || t.starts_with("Start at ")
        || t.starts_with("Duration ")
        || t.starts_with("Test Suites:")
        || t.starts_with("Time:")
        || t.starts_with("Ran all test suites")
}

/// `npm test` (vitest/jest/node:test): drop ✓/PASS/ok lines and all-pass
/// summary lines, keep FAIL blocks (with their detail) and failed summaries.
/// `keep` patterns veto every drop below.
fn filter_npm_test(lines: &[String], keep: &[Regex]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut capture = false; // inside a FAIL / ✗ block
    for line in lines {
        let t = line.trim_start();
        // Structural passes first: a `✓ src/errors.test.ts …` or TAP
        // `ok 2 - error handling` line asserts success — the marker word is
        // in the NAME and the line cannot represent a failure (failures
        // print ✗/×/FAIL, caught below). Same documented exception as cargo.
        if is_npm_pass_line(t) && !keep_matches(keep, line) {
            continue; // ✓ / PASS line — the bulk of the noise
        }
        if t.starts_with("ok ")
            && t.len() > 3
            && t[3..].starts_with(|c: char| c.is_ascii_digit())
            && !keep_matches(keep, line)
        {
            continue; // node:test TAP passing line ("ok 1 - name")
        }
        if is_error_line(line) {
            out.push(line.clone());
            // A failing test line starts the detail block that follows it.
            if t.starts_with('✗') || t.starts_with('×') || t.starts_with("FAIL ") {
                capture = true;
            }
            continue;
        }
        if capture {
            // Exit the failure block on a terminator line (next ✓/PASS test,
            // summary, timings) and classify that line normally (so passing
            // terminators are dropped, not kept as block content). Non-error
            // terminators are all droppable; error-marked ones never reach
            // this branch.
            if is_npm_test_terminator(t) && !keep_matches(keep, line) {
                capture = false;
            } else {
                out.push(line.clone());
                continue;
            }
        }
        if is_npm_test_terminator(t) && !keep_matches(keep, line) {
            continue; // all-pass summaries / timings (failed ones were kept above)
        }
        out.push(line.clone()); // unclassified → keep
    }
    out
}

/// Is the trimmed line a bundler asset-table entry (`dist/x.js  1.2 kB │
/// gzip: 0.3 kB` from vite, or `asset x.js 12 KiB [emitted]` from webpack)?
///
/// Patterns are end-anchored on the row SHAPE (the full gzip tail for vite,
/// the `[…]` status bracket for webpack) so a hypothetical row that continues
/// with failure text (`dist/x.js 1.2 kB │ ERROR: hash mismatch`) does NOT
/// match and still gets the error-marker veto (review L3).
fn is_asset_table_line(t: &str) -> bool {
    static VITE: OnceLock<Regex> = OnceLock::new();
    static WEBPACK: OnceLock<Regex> = OnceLock::new();
    let vite = VITE.get_or_init(|| {
        Regex::new(r"^[^\s│]+\s+\d+(\.\d+)?\s*(B|kB|KB|MB|KiB)\s*│\s*gzip:\s+\d+(\.\d+)?\s*kB\s*$")
            .expect("static vite-asset regex must compile")
    });
    let webpack = WEBPACK.get_or_init(|| {
        Regex::new(r"^asset\s+\S+\s+\d+(\.\d+)?\s*(B|kB|KB|MB|KiB|bytes)\s+\[.*$")
            .expect("static webpack-asset regex must compile")
    });
    vite.is_match(t) || webpack.is_match(t)
}

/// `npm run build` (vite/tsc/webpack): keep errors + warnings and the
/// `✓ built in …` summary line, drop progress lines and the asset table.
/// `keep` patterns veto every drop below.
fn filter_npm_build(lines: &[String], keep: &[Regex]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_diag = false;
    for line in lines {
        let t = line.trim_start();
        // Structural noise first: a bundler asset row (`dist/assets/error-404.js
        // 1.2 kB │ gzip: …`) is a size listing, not a failure — the marker
        // word is in the FILE NAME and the row cannot represent a failing
        // build.
        if is_asset_table_line(t) && !keep_matches(keep, line) {
            continue; // the per-file size table — pure noise
        }
        if is_error_line(line) || is_warning_line(line) {
            out.push(line.clone());
            in_diag = true;
            continue;
        }
        if in_diag && is_diagnostic_continuation(t) {
            out.push(line.clone());
            continue;
        }
        in_diag = false;
        if t.contains("built in") {
            out.push(line.clone()); // "✓ built in 1.2s" — the final summary
            continue;
        }
        if (t.starts_with("vite v") || t.starts_with("transforming")) && !keep_matches(keep, line) {
            continue;
        }
        if (t.starts_with("rendering") || t.starts_with("computing")) && !keep_matches(keep, line) {
            continue;
        }
        if t.starts_with('✓') && t.contains("modules transformed") && !keep_matches(keep, line) {
            continue;
        }
        out.push(line.clone()); // unclassified → keep
    }
    out
}

/// `git status`: keep branch lines, section headers, and the trailing status
/// summary; collapse each section's file list into one `N file(s): …` line.
/// `git status --short` output (no section headers) passes through unchanged.
/// `keep` patterns veto the hint-line drop below.
fn filter_git_status(lines: &[String], keep: &[Regex]) -> Vec<String> {
    const SECTIONS: [&str; 3] = [
        "Changes to be committed:",
        "Changes not staged for commit:",
        "Untracked files:",
    ];
    let mut out: Vec<String> = Vec::new();
    let mut section: Option<String> = None;
    let mut files: Vec<String> = Vec::new();

    fn flush(section: &mut Option<String>, files: &mut Vec<String>, out: &mut Vec<String>) {
        if let Some(header) = section.take() {
            out.push(header);
            if !files.is_empty() {
                let n = files.len();
                out.push(format!("  {n} file(s): {}", files.join(", ")));
            }
            files.clear();
        }
    }

    for line in lines {
        let t = line.trim();
        if t.is_empty() {
            flush(&mut section, &mut files, &mut out);
            continue;
        }
        if t.starts_with("On branch ") || t.starts_with("Your branch ") {
            flush(&mut section, &mut files, &mut out);
            out.push(line.clone());
            continue;
        }
        if SECTIONS.contains(&t) {
            flush(&mut section, &mut files, &mut out);
            section = Some(line.clone());
            continue;
        }
        if section.is_some() {
            // Inside a section: drop the boilerplate hint lines, collect
            // entries ("modified: x", "new file: y", or bare paths under
            // "Untracked files:"), collapsing internal whitespace runs so the
            // counts line stays compact. The hint drop is deliberately scoped
            // to sections: outside them, `(use "git …")` lines are
            // conflict-resolution guidance (rebase/cherry-pick escape hatches)
            // that must survive (review I1).
            if t.starts_with("(use \"git") && !keep_matches(keep, line) {
                continue;
            }
            files.push(t.split_whitespace().collect::<Vec<_>>().join(" "));
            continue;
        }
        out.push(line.clone()); // unclassified → keep (porcelain / summaries)
    }
    flush(&mut section, &mut files, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run the filter with the default config (enabled, no overrides) and
    /// return `(filtered_stdout, filtered_stderr)`.
    fn run(command: &str, stdout: &str, stderr: &str) -> (String, String) {
        filter_shell_output(command, stdout, stderr, &ShellFilterConfig::default())
    }

    fn join(lines: &[&str]) -> String {
        lines.join("\n")
    }

    // ── cargo build / check / clippy ───────────────────────────────────────

    #[test]
    fn cargo_build_noise_collapsed_errors_kept() {
        let stderr = join(&[
            "   Compiling mnemo v0.1.0 (C:\\AgenticCoder\\AgenticCoder)",
            "   Compiling tokio v1.40.0",
            "   Compiling thiserror v1.0.0", // crate name contains "error"
            "   Compiling failure v0.1.0",   // crate name contains "fail"
            "   Compiling serde v1.0.0 (proc-macro)",
            "    Checking regex v1.10.0",
            "    Finished `dev` profile [unoptimized + debuginfo] target(s) in 8.2s",
            "error[E0412]: cannot find type `Foo` in this scope",
            "  --> src/lib.rs:10:5",
            "   |",
            "10 | use Foo;",
            "   |     ^^^ not found in this scope",
            "warning: unused variable: `x`",
            " --> src/lib.rs:12:9",
            "  |",
            "12 | let x = 1;",
            "  |     ^ help: if this is intentional, prefix it with an underscore",
            "error: could not compile `mnemo` (lib) due to 2 previous errors",
        ]);
        let (_, filtered) = run("cargo build", "", &stderr);
        assert!(!filtered.contains("Compiling"), "noise kept: {filtered}");
        assert!(!filtered.contains("Checking"), "noise kept: {filtered}");
        // Structural exception: Compiling lines are dropped even when the
        // CRATE name contains a marker word (thiserror/failure).
        assert!(!filtered.contains("thiserror"), "noise kept: {filtered}");
        assert!(
            !filtered.contains("failure v0.1.0"),
            "noise kept: {filtered}"
        );
        // The final summary survives.
        assert!(
            filtered.contains("Finished `dev` profile"),
            "summary lost: {filtered}"
        );
        // Errors + their diagnostic context survive.
        assert!(filtered.contains("error[E0412]: cannot find type `Foo`"));
        assert!(filtered.contains("--> src/lib.rs:10:5"));
        assert!(filtered.contains("10 | use Foo;"));
        assert!(filtered.contains("not found in this scope"));
        assert!(filtered.contains("error: could not compile `mnemo`"));
        // Warnings + their context survive.
        assert!(filtered.contains("warning: unused variable: `x`"));
        assert!(filtered.contains("12 | let x = 1;"));
    }

    #[test]
    fn cargo_check_and_clippy_share_build_handler() {
        // `cargo check` and `cargo clippy` produce the same Compiling/Finished
        // noise shape; both must classify as CargoBuild.
        let noise = join(&[
            "    Checking mnemo v0.1.0",
            "    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.2s",
        ]);
        for command in [
            "cargo check",
            "cargo clippy --all-targets",
            "cargo build --release",
        ] {
            let (_, filtered) = run(command, "", &noise);
            assert!(
                !filtered.contains("Checking"),
                "{command}: noise kept: {filtered}"
            );
            assert!(
                filtered.contains("Finished"),
                "{command}: summary lost: {filtered}"
            );
        }
    }

    // ── cargo test ─────────────────────────────────────────────────────────

    #[test]
    fn cargo_test_passing_collapsed_failure_block_kept() {
        let stdout = join(&[
            "   Compiling mnemo v0.1.0 (C:\\AgenticCoder\\AgenticCoder)",
            "    Finished `test` profile [unoptimized + debuginfo] target(s) in 26.48s",
            "     Running unittests src\\lib.rs (target\\debug\\deps\\mnemo-abc.exe)",
            "",
            "running 2 tests",
            "test ok_one ... ok",
            "test ok_two ... ok",
            "test error_recovery_helper ... ok",
            "test failed_but_passes ... ok",
            "",
            "running 1 test",
            "test bad_test ... FAILED",
            "",
            "failures:",
            "",
            "---- bad_test stdout ----",
            "thread 'bad_test' panicked at src/lib.rs:5:5:",
            "assertion `left == right` failed",
            "  left: 1",
            " right: 2",
            "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace",
            "",
            "failures:",
            "    bad_test",
            "",
            "test result: FAILED. 3 passed; 1 failed; 0 ignored; 0 measured; 1202 filtered out; finished in 0.00s",
            "",
            "error: test failed, to rerun pass `--lib`",
        ]);
        let (filtered, _) = run("cargo test", &stdout, "");
        // Noise: compile lines, "running N tests" headers, passing test lines.
        assert!(!filtered.contains("Compiling"), "noise kept: {filtered}");
        assert!(
            !filtered.contains("running 2 tests"),
            "noise kept: {filtered}"
        );
        assert!(
            !filtered.contains("running 1 test"),
            "noise kept: {filtered}"
        );
        assert!(
            !filtered.contains("ok_one"),
            "passing test kept: {filtered}"
        );
        assert!(
            !filtered.contains("ok_two"),
            "passing test kept: {filtered}"
        );
        // Structural exception: passing test lines are dropped even when the
        // TEST NAME contains a marker word.
        assert!(
            !filtered.contains("error_recovery_helper"),
            "passing test kept: {filtered}"
        );
        assert!(
            !filtered.contains("failed_but_passes"),
            "passing test kept: {filtered}"
        );
        // The whole failure block survives, including unmarked detail lines.
        assert!(
            filtered.contains("test bad_test ... FAILED"),
            "FAILED line lost"
        );
        assert!(
            filtered.contains("---- bad_test stdout ----"),
            "block header lost"
        );
        assert!(
            filtered.contains("thread 'bad_test' panicked"),
            "panic line lost"
        );
        assert!(filtered.contains("assertion `left == right` failed"));
        assert!(
            filtered.contains("  left: 1"),
            "diff content lost: {filtered}"
        );
        assert!(filtered.contains("    bad_test"), "failures list lost");
        assert!(
            filtered.contains("test result: FAILED. 3 passed; 1 failed"),
            "summary lost: {filtered}"
        );
        assert!(filtered.contains("error: test failed"), "rerun hint lost");
    }

    #[test]
    fn cargo_test_all_pass_keeps_summary_only() {
        let stdout = join(&[
            "running 40 tests",
            "test t01 ... ok",
            "test t02 ... ok",
            "test result: ok. 40 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.42s",
        ]);
        let (filtered, _) = run("cargo test", &stdout, "");
        assert!(!filtered.contains("t01"));
        assert!(!filtered.contains("t02"));
        assert!(
            filtered.contains("test result: ok. 40 passed"),
            "ok summary lost: {filtered}"
        );
    }

    // ── npm test (vitest / jest / node:test) ───────────────────────────────

    #[test]
    fn npm_test_vitest_keeps_failures() {
        let stdout = join(&[
            "> frontend@0.1.0 test",
            "> vitest run",
            "",
            "",
            " RUN  v2.1.9 C:/path/frontend",
            "",
            " ✓ src/a.test.ts (3 tests) 12ms",
            " ✓ src/b.test.ts (2 tests) 5ms",
            " ✓ src/errors.test.ts (2 tests) 4ms",
            " ❯ src/c.test.ts (3 tests | 1 failed) 9ms",
            "   ✓ nested pass",
            "   ✗ nested fail",
            "     → expected \"a\" to be \"b\"",
            "",
            " Test Files  1 failed | 2 passed (3)",
            "      Tests  1 failed | 6 passed (7)",
            "   Start at  12:00:00",
            "   Duration  1.23s",
        ]);
        let (filtered, _) = run("npm test", &stdout, "");
        // Passing ✓ lines (and the nested pass) are gone — including one
        // whose FILE NAME contains a marker word (structural exception).
        assert!(
            !filtered.contains("src/a.test.ts"),
            "pass line kept: {filtered}"
        );
        assert!(
            !filtered.contains("src/errors.test.ts"),
            "pass line kept: {filtered}"
        );
        assert!(
            !filtered.contains("nested pass"),
            "nested pass kept: {filtered}"
        );
        assert!(
            !filtered.contains("Start at"),
            "timing noise kept: {filtered}"
        );
        assert!(
            !filtered.contains("Duration"),
            "timing noise kept: {filtered}"
        );
        // The failing file header, the ✗ line, and its detail survive.
        assert!(
            filtered.contains("1 failed"),
            "failed summary lost: {filtered}"
        );
        assert!(
            filtered.contains("✗ nested fail"),
            "fail line lost: {filtered}"
        );
        assert!(
            filtered.contains("→ expected \"a\" to be \"b\""),
            "failure detail lost: {filtered}"
        );
    }

    #[test]
    fn npm_test_jest_keeps_failures() {
        let stdout = join(&[
            "PASS src/ok.test.ts",
            "PASS src/also-ok.test.ts",
            "FAIL src/bad.test.ts",
            "  ● bad things happen",
            "    expect(received).toBe(expected) // Object.is equality",
            "    Expected: \"a\"",
            "    Received: \"b\"",
            "",
            "Test Suites: 1 failed, 2 passed, 3 total",
            "Tests:       2 failed, 5 passed, 7 total",
            "Snapshots:   0 total",
            "Time:        1.5s",
        ]);
        let (filtered, _) = run("npm run test", &stdout, "");
        assert!(
            !filtered.contains("ok.test.ts"),
            "PASS line kept: {filtered}"
        );
        assert!(
            !filtered.contains("Snapshots"),
            "snapshot noise kept: {filtered}"
        );
        assert!(!filtered.contains("Time:"), "timing noise kept: {filtered}");
        assert!(filtered.contains("FAIL src/bad.test.ts"), "FAIL line lost");
        assert!(
            filtered.contains("● bad things happen"),
            "block detail lost"
        );
        assert!(
            filtered.contains("Expected: \"a\""),
            "failure detail lost: {filtered}"
        );
        assert!(
            filtered.contains("Test Suites: 1 failed"),
            "suite summary lost: {filtered}"
        );
        assert!(
            filtered.contains("Tests:       2 failed"),
            "test summary lost"
        );
    }

    #[test]
    fn npm_test_node_tap_ok_lines_dropped_failures_kept() {
        let stdout = join(&[
            "ok 1 - first thing",
            "ok 2 - error handling works",
            "ok 3 - second thing",
            "not ok 4 - third thing",
            "  ---",
            "  message: expected 1 to equal 2",
            "  ...",
            "# fail 1",
            "# pass 2",
        ]);
        let (filtered, _) = run("npm test", &stdout, "");
        assert!(
            !filtered.contains("first thing"),
            "TAP ok line kept: {filtered}"
        );
        // Structural exception: TAP ok lines are dropped even when the test
        // NAME contains a marker word.
        assert!(
            !filtered.contains("error handling works"),
            "TAP ok line kept: {filtered}"
        );
        assert!(
            !filtered.contains("second thing"),
            "TAP ok line kept: {filtered}"
        );
        // "not ok" carries no marker word — but it is unclassified, so it
        // must survive (whitelist-drop never removes what it doesn't know).
        assert!(filtered.contains("not ok 4 - third thing"));
        assert!(filtered.contains("message: expected 1 to equal 2"));
        assert!(filtered.contains("# fail 1"), "fail count lost");
    }

    // ── npm run build (vite / tsc) ─────────────────────────────────────────

    #[test]
    fn npm_build_vite_collapses_asset_table_keeps_summary() {
        let stdout = join(&[
            "> frontend@0.1.0 build",
            "> tsc -b && vite build",
            "",
            "vite v5.4.8 building for production...",
            "transforming...",
            "✓ 1234 modules transformed.",
            "rendering chunks...",
            "computing gzip size...",
            "dist/index.html                   0.46 kB │ gzip:  0.29 kB",
            "dist/assets/index-abc123.css    36.28 kB │ gzip:  7.72 kB",
            "dist/assets/index-abc123.js   812.44 kB │ gzip: 243.11 kB",
            "dist/assets/error-404.js        1.20 kB │ gzip:  0.60 kB",
            "✓ built in 1.20s",
        ]);
        let (filtered, _) = run("npm run build", &stdout, "");
        assert!(!filtered.contains("vite v"), "noise kept: {filtered}");
        assert!(!filtered.contains("transforming"), "noise kept: {filtered}");
        assert!(
            !filtered.contains("modules transformed"),
            "noise kept: {filtered}"
        );
        assert!(
            !filtered.contains("rendering chunks"),
            "noise kept: {filtered}"
        );
        assert!(
            !filtered.contains("computing gzip"),
            "noise kept: {filtered}"
        );
        assert!(
            !filtered.contains("dist/index.html"),
            "asset table kept: {filtered}"
        );
        assert!(
            !filtered.contains("index-abc123"),
            "asset table kept: {filtered}"
        );
        // Structural exception: asset rows are dropped even when the FILE
        // name contains a marker word (error-404.js).
        assert!(
            !filtered.contains("error-404.js"),
            "asset table kept: {filtered}"
        );
        assert!(filtered.contains("✓ built in 1.20s"), "build summary lost");
    }

    #[test]
    fn npm_build_tsc_and_vite_errors_kept() {
        let stdout = join(&[
            "src/app.ts(12,5): error TS2322: Type 'string' is not assignable to type 'number'.",
            "src/app.ts(13,1): error TS2304: Cannot find name 'missing'.",
        ]);
        let stderr = join(&[
            "vite v5.4.8 building for production...",
            "transforming...",
            "error during build:",
            "src/app.tsx (14:7): \"window.foo\" is not defined",
        ]);
        let (filtered_out, filtered_err) = run("npm run build", &stdout, &stderr);
        assert!(
            filtered_out.contains("error TS2322"),
            "tsc error lost: {filtered_out}"
        );
        assert!(
            filtered_out.contains("error TS2304"),
            "tsc error lost: {filtered_out}"
        );
        assert!(
            !filtered_err.contains("transforming"),
            "noise kept: {filtered_err}"
        );
        assert!(
            filtered_err.contains("error during build:"),
            "vite error lost: {filtered_err}"
        );
        assert!(
            filtered_err.contains("\"window.foo\" is not defined"),
            "vite error detail lost: {filtered_err}"
        );
    }

    // ── git status ─────────────────────────────────────────────────────────

    #[test]
    fn git_status_collapses_file_lists_into_counts() {
        let stdout = join(&[
            "On branch feat/shell-filter",
            "Your branch is up to date with 'origin/feat/shell-filter'.",
            "",
            "Changes not staged for commit:",
            "  (use \"git add <file>...\" to update what will be committed)",
            "  (use \"git restore <file>...\" to discard changes in working directory)",
            "\tmodified:   src/tool/agent/shell.rs",
            "\tmodified:   src/tool/agent/mod.rs",
            "",
            "Untracked files:",
            "  (use \"git add <file>...\" to include in what will be committed)",
            "\t.coding/reviews/foo.md",
            "",
            "no changes added to commit (use \"git add\" and/or \"git commit -a\")",
        ]);
        let (filtered, _) = run("git status", &stdout, "");
        assert!(filtered.contains("On branch feat/shell-filter"));
        assert!(filtered.contains("Your branch is up to date"));
        assert!(
            filtered.contains("Changes not staged for commit:"),
            "header lost: {filtered}"
        );
        assert!(
            filtered.contains(
                "2 file(s): modified: src/tool/agent/shell.rs, modified: src/tool/agent/mod.rs"
            ),
            "count line wrong: {filtered}"
        );
        assert!(
            filtered.contains("1 file(s): .coding/reviews/foo.md"),
            "untracked count wrong: {filtered}"
        );
        assert!(
            !filtered.contains("to update what will be committed"),
            "hint lines kept: {filtered}"
        );
        assert!(
            !filtered.contains("to discard changes in working directory"),
            "hint lines kept: {filtered}"
        );
        assert!(
            filtered.contains("no changes added to commit"),
            "trailing summary lost: {filtered}"
        );
    }

    #[test]
    fn git_status_clean_and_short_pass_through() {
        let clean = "On branch main\nnothing to commit, working tree clean";
        let (filtered, _) = run("git status", clean, "");
        assert!(filtered.contains("nothing to commit, working tree clean"));
        // `--short` has no section headers — entries must pass through.
        let short = " M src/lib.rs\n?? new-file.txt";
        let (filtered, _) = run("git status --short", short, "");
        assert!(filtered.contains(" M src/lib.rs"), "got: {filtered}");
        assert!(filtered.contains("?? new-file.txt"));
    }

    // ── unknown commands ───────────────────────────────────────────────────

    #[test]
    fn unknown_command_passthrough_preserves_content() {
        let stdout = join(&[
            "Epoch 1/10",
            "train loss: 0.4321  val loss: 0.4501",
            "saving checkpoint",
            "saving checkpoint",
            "Epoch 2/10",
            "train loss: 0.4100  val loss: 0.4300",
            "saving checkpoint",
            "saving checkpoint",
            "saving checkpoint",
        ]);
        let (filtered, _) = run("python train.py", &stdout, "");
        // Every distinct content line survives (only dedup applies); the two
        // separate runs of "saving checkpoint" each keep one occurrence and
        // the 3-line run gets a collapse note.
        assert!(filtered.contains("Epoch 1/10"));
        assert!(filtered.contains("train loss: 0.4321  val loss: 0.4501"));
        assert!(filtered.contains("Epoch 2/10"));
        assert!(filtered.contains("train loss: 0.4100  val loss: 0.4300"));
        assert_eq!(
            filtered.matches("saving checkpoint").count(),
            2,
            "got: {filtered}"
        );
        assert!(
            filtered.contains("2 identical lines collapsed"),
            "dedup note lost: {filtered}"
        );
    }

    // ── safety rules ───────────────────────────────────────────────────────

    #[test]
    fn error_lines_survive_every_handler_even_with_drop_all_override() {
        // A user override that tries to drop EVERYTHING must still never
        // remove an error-marked line — the hard safety rule.
        let cfg = ShellFilterConfig {
            enabled: true,
            overrides: vec![crate::config::ShellFilterOverride {
                command: ".*".into(),
                drop: vec!["^.*$".into()],
                keep: vec![],
                disable_builtin: false,
            }],
        };
        let error_lines = [
            "error: something broke",
            "test xyz ... FAILED",
            "FAILURE: boom",
            "✗ vitest failed",
            "thread 'main' panicked at src/lib.rs:1:1",
            // False-positive marker words in a NON-structural line: still kept.
            "note: the failure_rate metric is unexpectedly high",
        ];
        let ordinary = ["   Compiling foo v1.0.0", "test a ... ok", "noise here"];
        let stream = join(&[&error_lines[..], &ordinary[..]].concat());
        for command in [
            "cargo build",
            "cargo test",
            "npm test",
            "npm run build",
            "git status",
            "totally-unknown-cmd",
        ] {
            let (filtered, _) = filter_shell_output(command, &stream, "", &cfg);
            for el in &error_lines {
                assert!(
                    filtered.contains(el),
                    "{command}: error line dropped by drop-all override: {el:?} in {filtered}"
                );
            }
            // The drop-all override removed the ordinary lines.
            assert!(
                !filtered.contains("noise here"),
                "{command}: override ignored"
            );
        }
    }

    #[test]
    fn failing_build_and_failing_test_still_show_failure() {
        // End-to-end shape: a non-zero-exit cargo test must leave the failure
        // fully visible in the filtered transcript.
        let stdout = join(&[
            "   Compiling mnemo v0.1.0",
            "running 2 tests",
            "test good ... ok",
            "test bad ... FAILED",
            "failures:",
            "---- bad stdout ----",
            "boom",
            "test result: FAILED. 1 passed; 1 failed; 0 ignored;",
            "error: test failed, to rerun pass `--lib`",
        ]);
        let (filtered, _) = run("cargo test", &stdout, "");
        let must = [
            "test bad ... FAILED",
            "---- bad stdout ----",
            "boom",
            "test result: FAILED. 1 passed; 1 failed",
            "error: test failed",
        ];
        for m in must {
            assert!(
                filtered.contains(m),
                "failure hidden: {m:?} not in {filtered}"
            );
        }
    }

    // ── pipeline stages ────────────────────────────────────────────────────

    #[test]
    fn ansi_stripped() {
        let stdout = "\x1b[32mCompiling\x1b[0m foo v1.0.0\n\x1b[1;34m   ok\x1b[0m";
        let (filtered, _) = run("cargo build", stdout, "");
        assert!(!filtered.contains('\x1b'), "ANSI codes kept: {filtered:?}");
    }

    #[test]
    fn progress_bar_and_spinner_stripped() {
        let stdout = join(&[
            "building 10%",
            "[#####-----] 50%",
            "⠙",
            "[####----] 75%",
            "⠹",
            "line one",
            "line one",
            "line one",
            "done",
        ]);
        let (filtered, _) = run("make", &stdout, "");
        assert!(!filtered.contains("50%"), "progress bar kept: {filtered}");
        assert!(!filtered.contains("75%"), "progress bar kept: {filtered}");
        assert!(!filtered.contains('⠙'), "spinner kept: {filtered}");
        assert!(!filtered.contains('⠹'), "spinner kept: {filtered}");
        // A line with real words + a percentage is NOT a progress bar.
        assert!(
            filtered.contains("building 10%"),
            "real line lost: {filtered}"
        );
        assert_eq!(filtered.matches("line one").count(), 1);
        assert!(filtered.contains("2 identical lines collapsed"));
        assert!(filtered.contains("done"));
    }

    #[test]
    fn repeated_error_lines_are_never_deduped() {
        let stdout = join(&["error: retry", "error: retry", "error: retry"]);
        let (filtered, _) = run("some-cmd", &stdout, "");
        assert_eq!(filtered.matches("error: retry").count(), 3);
        assert!(!filtered.contains("collapsed"));
    }

    // ── config ─────────────────────────────────────────────────────────────

    #[test]
    fn config_disabled_passthrough_is_byte_identical() {
        let cfg = ShellFilterConfig {
            enabled: false,
            overrides: vec![],
        };
        let stdout = "   Compiling foo\nnoise \x1b[31mred\x1b[0m\n";
        let stderr = "more noise\n";
        let (out, err) = filter_shell_output("cargo build", stdout, stderr, &cfg);
        assert_eq!(out, stdout);
        assert_eq!(err, stderr);
    }

    #[test]
    fn override_drop_and_keep_rules_apply() {
        let cfg = ShellFilterConfig {
            enabled: true,
            overrides: vec![crate::config::ShellFilterOverride {
                command: "^deploy".into(),
                drop: vec!["^INFO ".into()],
                keep: vec!["^INFO critical".into()],
                disable_builtin: false,
            }],
        };
        let stdout = join(&["INFO noise", "INFO critical-keep", "DATA x"]);
        let (filtered, _) = filter_shell_output("deploy.sh", &stdout, "", &cfg);
        assert!(
            !filtered.contains("INFO noise"),
            "drop rule ignored: {filtered}"
        );
        assert!(filtered.contains("INFO critical-keep"), "keep rule ignored");
        assert!(filtered.contains("DATA x"));
    }

    #[test]
    fn override_disable_builtin_falls_back_to_generic() {
        let cfg = ShellFilterConfig {
            enabled: true,
            overrides: vec![crate::config::ShellFilterOverride {
                command: "^cargo test".into(),
                drop: vec![],
                keep: vec![],
                disable_builtin: true,
            }],
        };
        let stdout = join(&["test a ... ok", "   Compiling foo v1.0.0"]);
        let (filtered, _) = filter_shell_output("cargo test", &stdout, "", &cfg);
        // Built-in disabled → generic fallback drops nothing.
        assert!(filtered.contains("test a ... ok"), "got: {filtered}");
        assert!(filtered.contains("Compiling foo"), "got: {filtered}");
    }

    #[test]
    fn override_first_command_match_wins() {
        let cfg = ShellFilterConfig {
            enabled: true,
            overrides: vec![
                crate::config::ShellFilterOverride {
                    command: "^cargo".into(),
                    drop: vec!["^   Compiling ".into()],
                    keep: vec![],
                    disable_builtin: false,
                },
                crate::config::ShellFilterOverride {
                    command: ".*".into(),
                    drop: vec!["^MARK ".into()],
                    keep: vec![],
                    disable_builtin: false,
                },
            ],
        };
        let stdout = join(&["   Compiling foo v1.0.0", "MARK remove-me", "DATA x"]);
        let (filtered, _) = filter_shell_output("cargo build", &stdout, "", &cfg);
        assert!(!filtered.contains("Compiling foo"));
        // Only the FIRST matching override applies — the second's drop rule
        // must not fire for `cargo build`.
        assert!(filtered.contains("MARK remove-me"), "got: {filtered}");
    }

    #[test]
    fn invalid_override_regexes_are_skipped_without_panicking() {
        let cfg = ShellFilterConfig {
            enabled: true,
            overrides: vec![
                // Broken command regex → whole override skipped.
                crate::config::ShellFilterOverride {
                    command: "[unclosed".into(),
                    drop: vec!["^x".into()],
                    keep: vec![],
                    disable_builtin: false,
                },
                // Broken drop pattern → just that pattern skipped.
                crate::config::ShellFilterOverride {
                    command: "^cargo".into(),
                    drop: vec!["[also-unclosed".into(), "^   Compiling ".into()],
                    keep: vec!["(nope".into()],
                    disable_builtin: false,
                },
            ],
        };
        let stdout = join(&["   Compiling foo v1.0.0", "Finished build"]);
        let (filtered, _) = filter_shell_output("cargo build", &stdout, "", &cfg);
        // No panic; the valid drop pattern still applied.
        assert!(!filtered.contains("Compiling foo"));
        assert!(filtered.contains("Finished build"));
    }

    // ── reduction target ───────────────────────────────────────────────────

    #[test]
    fn reduction_target_over_80_percent_on_typical_cargo_test_output() {
        // A realistic plan-step `cargo test`: hundreds of Compiling lines +
        // hundreds of passing test lines + the summary. The filtered
        // transcript must be under 20% of the raw line count.
        let mut lines: Vec<String> = Vec::new();
        for i in 0..150 {
            lines.push(format!("   Compiling crate{i} v0.1.0"));
        }
        lines.push("    Finished `test` profile [unoptimized + debuginfo] target(s) in 30s".into());
        lines.push("     Running unittests src/lib.rs (target/debug/deps/mnemo-abc.exe)".into());
        lines.push("running 300 tests".into());
        for i in 0..300 {
            lines.push(format!("test test_{i:03} ... ok"));
        }
        lines.push("test result: ok. 300 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.85s".into());
        let raw = lines.join("\n");
        let raw_lines = raw.lines().count();
        let (filtered, _) = run("cargo test", &raw, "");
        let filtered_lines = filtered.lines().count();
        assert!(
            filtered_lines * 5 < raw_lines,
            "expected >80% reduction, got {} -> {} lines ({})",
            raw_lines,
            filtered_lines,
            filtered
        );
        assert!(filtered.contains("test result: ok. 300 passed"));
    }

    // ── review fixes: keep-veto, err!/✖ markers, capture exit, asset anchors,
    //    git conflict hints ─────────────────────────────────────────────────

    #[test]
    fn keep_override_vetoes_built_in_drops() {
        // Review M1: `keep` must resurrect lines a built-in handler would
        // drop, not only veto the override's own drop patterns.
        let cfg = ShellFilterConfig {
            enabled: true,
            overrides: vec![crate::config::ShellFilterOverride {
                command: "^cargo".into(),
                drop: vec![],
                keep: vec!["Compiling special".into()],
                disable_builtin: false,
            }],
        };
        let stderr = join(&[
            "   Compiling special-crate v1.0.0",
            "   Compiling ordinary-crate v1.0.0",
            "    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.0s",
        ]);
        let (_, filtered) = filter_shell_output("cargo build", "", &stderr, &cfg);
        assert!(
            filtered.contains("Compiling special-crate"),
            "keep must veto the built-in Compiling drop, got: {filtered}"
        );
        assert!(!filtered.contains("ordinary-crate"));
        assert!(filtered.contains("Finished"));
    }

    #[test]
    fn npm_err_and_node_test_glyph_are_error_marked() {
        // Review L1: `npm ERR! …` (no "error"/"fail" substring) and node:test's
        // ✖ glyph must count as error markers — kept even under a drop-all
        // override, and never deduped.
        let cfg = ShellFilterConfig {
            enabled: true,
            overrides: vec![crate::config::ShellFilterOverride {
                command: ".*".into(),
                drop: vec!["^.*$".into()],
                keep: vec![],
                disable_builtin: false,
            }],
        };
        let stdout = join(&[
            "npm ERR! code ELIFECYCLE",
            "npm ERR! Exit status 1",
            "✖ failing node test",
            "npm ERR! code ELIFECYCLE",
        ]);
        for command in ["npm test", "npm run build", "unknown-cmd"] {
            let (filtered, _) = filter_shell_output(command, &stdout, "", &cfg);
            assert!(filtered.contains("npm ERR! code ELIFECYCLE"), "{command}");
            assert!(filtered.contains("npm ERR! Exit status 1"), "{command}");
            assert!(filtered.contains("✖ failing node test"), "{command}");
        }
        // Repeated npm ERR! lines are information, not noise — never collapsed.
        let (filtered, _) = run("npm test", &stdout, "");
        assert_eq!(filtered.matches("npm ERR! code ELIFECYCLE").count(), 2);
        assert!(!filtered.contains("collapsed"));
    }

    #[test]
    fn cargo_test_capture_exits_at_summary_even_when_it_contains_failed() {
        // Review L2: every real `test result:` line contains "failed"
        // ("0 failed"), so it lands in the error branch — capture must reset
        // there too, or the whole tail of the stream over-captures after the
        // first failing binary.
        let stdout = join(&[
            "running 2 tests",
            "test bad ... FAILED",
            "failures:",
            "---- bad stdout ----",
            "boom",
            "test result: FAILED. 1 passed; 1 failed; 0 ignored;",
            "error: test failed, to rerun pass `--lib`",
            "",
            "     Running unittests src/lib.rs (second binary)",
            "running 2 tests",
            "test post_one ... ok",
            "test post_two ... ok",
            "test result: ok. 2 passed; 0 failed; 0 ignored;",
        ]);
        let (filtered, _) = run("cargo test", &stdout, "");
        // The failure block + both summaries survive.
        assert!(filtered.contains("test bad ... FAILED"));
        assert!(filtered.contains("boom"));
        assert!(
            filtered.contains("test result: FAILED. 1 passed; 1 failed"),
            "failed summary lost: {filtered}"
        );
        assert!(
            filtered.contains("test result: ok. 2 passed"),
            "ok summary lost: {filtered}"
        );
        // Capture exited at the first summary, so the second binary's noise
        // is filtered again (not over-captured).
        assert!(!filtered.contains("post_one"), "over-captured: {filtered}");
        assert!(!filtered.contains("post_two"), "over-captured: {filtered}");
        assert!(
            !filtered.contains("running 2 tests"),
            "over-captured: {filtered}"
        );
    }

    #[test]
    fn npm_build_webpack_rows_collapsed_and_failure_row_kept() {
        // Review L3: webpack `asset … [emitted]` rows collapse; an asset-shaped
        // row that continues with failure text must NOT match the structural
        // pattern and keeps its error-marker veto.
        let stdout = join(&[
            "asset main.js 812 KiB [emitted] [minimized] (name: main)",
            "asset index.html 450 bytes [compared for emit]",
            "webpack 5.90.0 compiled successfully in 3456 ms",
        ]);
        let (filtered, _) = run("npm run build", &stdout, "");
        assert!(!filtered.contains("asset main.js"), "kept: {filtered}");
        assert!(
            !filtered.contains("index.html 450 bytes"),
            "kept: {filtered}"
        );
        assert!(filtered.contains("webpack 5.90.0 compiled successfully"));

        let failure_row = "dist/x.js 1.2 kB │ ERROR: hash mismatch";
        let (filtered, _) = run("npm run build", failure_row, "");
        assert!(
            filtered.contains("ERROR: hash mismatch"),
            "failure text must survive the asset-row check: {filtered}"
        );
    }

    #[test]
    fn git_status_keeps_conflict_guidance_outside_sections() {
        // Review I1: `(use "git …")` hint lines are only dropped INSIDE file
        // sections. During a rebase/cherry-pick conflict the escape-hatch
        // guidance appears outside any section and must survive.
        let stdout = join(&[
            "You are currently rebasing branch 'feat/x' on 'abc1234'.",
            "  (use \"git rebase --abort\" to check out the original branch)",
            "  (use \"git rebase --skip\" to skip this patch)",
            "Unmerged paths:",
            "  (use \"git restore --staged <file>...\" to unstage)",
            "  (use \"git add <file>...\" to mark resolution)",
            "\tboth modified:   src/lib.rs",
        ]);
        let (filtered, _) = run("git status", &stdout, "");
        assert!(filtered.contains("You are currently rebasing"));
        assert!(
            filtered.contains("use \"git rebase --abort\""),
            "conflict hint lost: {filtered}"
        );
        assert!(
            filtered.contains("use \"git rebase --skip\""),
            "conflict hint lost: {filtered}"
        );
        assert!(filtered.contains("Unmerged paths:"));
        // The conflict entry survives verbatim (tab + git's spacing).
        assert!(
            filtered.contains("both modified:"),
            "conflict entry lost: {filtered}"
        );
        assert!(filtered.contains("src/lib.rs"));
    }
}
