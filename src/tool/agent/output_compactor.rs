// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Semantic command-output compression (token-optimizer lever 2, backlog
//! e4a50d22).
//!
//! A second, DEFAULT-OFF layer on top of the deterministic
//! [`shell_filter`](super::shell_filter): for a known command family whose
//! (already-filtered) output still exceeds
//! [`compress_min_chars`](crate::config::OptimizerConfig::compress_min_chars),
//! the output collapses to what an agent actually needs — the DISTINCT
//! error/warning lines (kept verbatim, globally deduplicated with a
//! "repeated N×" annotation), the command's own summary lines, a header
//! carrying the exit status and a line census, and count notes for the
//! collapsed noise — instead of hundreds of progress lines. Everything the
//! model sees is credential-redacted ([`redact_secrets`]). The raw
//! stdout/stderr ride the result's `data` untouched (the shell_filter
//! contract), and the lever only substitutes when the compact form is
//! meaningfully smaller — otherwise the original survives byte-identically
//! (fail-open).
//!
//! Families: cargo build/check/clippy, cargo test, npm/yarn/pnpm test,
//! npm/yarn/pnpm build (vitest/jest/tsc/vite), pytest (incl.
//! `python -m pytest`), go test, go build — plus the user-extensible
//! `compress_extra_commands` regexes (a generic dedup compressor).

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

use crate::config::OptimizerConfig;

/// A compressed command-output form. `text` replaces the LLM-facing
/// combined output; the raw stdout/stderr stay in the result's `data`.
pub(crate) struct CompactedOutput {
    /// The compact form the model sees (credential-redacted).
    pub text: String,
}

/// One command family with a semantic compressor.
struct FamilySpec {
    /// The family label in the header line (e.g. `cargo-build`).
    label: &'static str,
    /// The noise-bucket label in the collapse note (e.g.
    /// `compile-progress`).
    noise_label: &'static str,
    /// The family's line classifier.
    classify: fn(&str) -> LineKind,
}

/// How a family's classifier buckets one output line.
enum LineKind {
    /// Error-bearing — kept (distinct, deduplicated, verbatim).
    Error,
    /// Warning-bearing — kept (distinct, deduplicated, verbatim).
    Warning,
    /// A passing-test line — collapsed into a count.
    Passing,
    /// Recognized noise (progress/asset/session chatter) — collapsed into
    /// a count.
    Noise,
    /// Everything else — kept verbatim (deduplicated with counts). The
    /// never-drop-what-you-don't-recognize rule: failure detail, code
    /// snippets, and the command's own summary lines all land here.
    Keep,
}

/// cargo build/check/clippy: drop compile-progress lines, keep errors,
/// warnings, and the build's own summary (`Finished`/`could not compile`).
static CARGO_BUILD: FamilySpec = FamilySpec {
    label: "cargo-build",
    noise_label: "compile-progress",
    classify: classify_cargo_build,
};

/// cargo test: drop passing-test lines and build noise, keep FAILED
/// blocks, panics, and the `test result:` summary.
static CARGO_TEST: FamilySpec = FamilySpec {
    label: "cargo-test",
    noise_label: "build-progress",
    classify: classify_cargo_test,
};

/// npm/yarn/pnpm/pnpm test (vitest/jest): drop ✓/PASS lines and runner
/// arrows, keep FAIL blocks, `npm ERR!` lines, and the Test Files/Tests
/// summaries.
static NPM_TEST: FamilySpec = FamilySpec {
    label: "npm-test",
    noise_label: "runner-progress",
    classify: classify_npm_test,
};

/// npm/yarn/pnpm build (vite/tsc): drop asset tables and bundler progress,
/// keep errors, warnings, and the `built in` summary.
static NPM_BUILD: FamilySpec = FamilySpec {
    label: "npm-build",
    noise_label: "build-progress",
    classify: classify_npm_build,
};

/// pytest: drop the session header and progress dots, keep FAILED blocks,
/// assertion detail, and the `N failed, M passed` summary.
static PYTEST: FamilySpec = FamilySpec {
    label: "pytest",
    noise_label: "session-header",
    classify: classify_pytest,
};

/// go test: drop RUN/PAUSE/CONT/SKIP lines and passing `--- PASS:` lines,
/// keep FAIL output and the package summary.
static GO_TEST: FamilySpec = FamilySpec {
    label: "go-test",
    noise_label: "run-progress",
    classify: classify_go_test,
};

/// go build: keep errors and warnings; everything unrecognized is kept.
static GO_BUILD: FamilySpec = FamilySpec {
    label: "go-build",
    noise_label: "build-progress",
    classify: classify_generic,
};

/// `compress_extra_commands` matches: the generic compressor — errors and
/// warnings kept, everything else kept verbatim with global dedup (the
/// saving comes from collapsing repeats and blank lines).
static GENERIC: FamilySpec = FamilySpec {
    label: "generic",
    noise_label: "repeated",
    classify: classify_generic,
};

/// True when the line is blank — pure layout, safe to collapse in every
/// family (the compact form reflows sections anyway).
fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// cargo build/check/clippy line classifier.
fn classify_cargo_build(line: &str) -> LineKind {
    if is_blank(line) {
        return LineKind::Noise;
    }
    if super::shell_filter::is_error_line(line) {
        return LineKind::Error;
    }
    if super::shell_filter::is_warning_line(line) {
        return LineKind::Warning;
    }
    let t = line.trim_start();
    for prefix in [
        "Compiling ", "Checking ", "Downloading", "Downloaded ", "Locking ", "Updating ",
        "Adding ", "Blocking ", "Fresh ", "Collecting ",
    ] {
        if t.starts_with(prefix) {
            return LineKind::Noise;
        }
    }
    LineKind::Keep
}

/// cargo test line classifier.
fn classify_cargo_test(line: &str) -> LineKind {
    if is_blank(line) {
        return LineKind::Noise;
    }
    if super::shell_filter::is_error_line(line) {
        return LineKind::Error;
    }
    if super::shell_filter::is_warning_line(line) {
        return LineKind::Warning;
    }
    let t = line.trim();
    if t.starts_with("test ") && t.ends_with(" ok") {
        return LineKind::Passing;
    }
    for prefix in [
        "Compiling ", "Finished ", "Running ", "Doc-tests", "Collecting ", "Downloading",
        "Fresh ",
    ] {
        if line.trim_start().starts_with(prefix) {
            return LineKind::Noise;
        }
    }
    LineKind::Keep
}

/// npm/yarn/pnpm test (vitest/jest) line classifier.
fn classify_npm_test(line: &str) -> LineKind {
    if is_blank(line) {
        return LineKind::Noise;
    }
    if super::shell_filter::is_error_line(line) {
        return LineKind::Error;
    }
    if super::shell_filter::is_warning_line(line) {
        return LineKind::Warning;
    }
    let t = line.trim_start();
    if t.starts_with('✓') || t.starts_with('√') || t.starts_with("PASS ") {
        return LineKind::Passing;
    }
    if t.starts_with('❯') || t.starts_with('→') || t.starts_with('·') {
        return LineKind::Noise;
    }
    LineKind::Keep
}

/// npm/yarn/pnpm build (vite/tsc) line classifier.
fn classify_npm_build(line: &str) -> LineKind {
    if is_blank(line) {
        return LineKind::Noise;
    }
    if super::shell_filter::is_error_line(line) {
        return LineKind::Error;
    }
    if super::shell_filter::is_warning_line(line) {
        return LineKind::Warning;
    }
    let t = line.trim_start();
    if t.starts_with("dist/")
        || t.starts_with("transforming (")
        || t.starts_with("rendering chunks (")
        || t.starts_with("computing gzip")
        || t.contains("kB │ gzip")
    {
        return LineKind::Noise;
    }
    LineKind::Keep
}

/// Is this a pytest progress line — an optional path followed by a run of
/// `.FExsX*` characters and an optional `[ 66%]` trailer?
fn is_pytest_progress(t: &str) -> bool {
    static PROGRESS: OnceLock<Regex> = OnceLock::new();
    let re = PROGRESS.get_or_init(|| {
        Regex::new(r"^(?:[\w.\-/\\:]+\s+)?[.FExsX*]+(?:\s*\[[\s\d]*%+\])?$")
            .expect("static pytest progress regex must compile")
    });
    re.is_match(t)
}

/// pytest line classifier.
fn classify_pytest(line: &str) -> LineKind {
    if is_blank(line) {
        return LineKind::Noise;
    }
    if super::shell_filter::is_error_line(line) {
        return LineKind::Error;
    }
    if super::shell_filter::is_warning_line(line) {
        return LineKind::Warning;
    }
    let t = line.trim();
    // The session banner is `===== test session starts =====` — the label
    // sits inside a run of `=`, so match it anywhere on the line.
    if t.contains("test session starts") {
        return LineKind::Noise;
    }
    for prefix in ["platform ", "rootdir:", "configfile:", "plugins:", "collected "] {
        if t.starts_with(prefix) {
            return LineKind::Noise;
        }
    }
    if is_pytest_progress(t) {
        return LineKind::Passing;
    }
    LineKind::Keep
}

/// go test line classifier.
fn classify_go_test(line: &str) -> LineKind {
    if is_blank(line) {
        return LineKind::Noise;
    }
    if super::shell_filter::is_error_line(line) {
        return LineKind::Error;
    }
    if super::shell_filter::is_warning_line(line) {
        return LineKind::Warning;
    }
    let t = line.trim();
    if t.starts_with("--- PASS:") {
        return LineKind::Passing;
    }
    if t.starts_with("=== RUN")
        || t.starts_with("=== PAUSE")
        || t.starts_with("=== CONT")
        || t.starts_with("--- SKIP:")
    {
        return LineKind::Noise;
    }
    LineKind::Keep
}

/// The generic classifier: errors and warnings kept, blanks collapsed,
/// everything else kept verbatim (deduplicated by the compact pass).
fn classify_generic(line: &str) -> LineKind {
    if is_blank(line) {
        return LineKind::Noise;
    }
    if super::shell_filter::is_error_line(line) {
        return LineKind::Error;
    }
    if super::shell_filter::is_warning_line(line) {
        return LineKind::Warning;
    }
    LineKind::Keep
}/// An insert-ordered dedup bucket: distinct lines with their repeat
/// counts, first-seen order preserved. The "global dedup" the plan calls
/// for — the same diagnostic printed by several files shows once, with
/// a `repeated N×` annotation.
struct Bucket {
    order: Vec<String>,
    counts: HashMap<String, usize>,
}

impl Bucket {
    fn new() -> Self {
        Self { order: Vec::new(), counts: HashMap::new() }
    }

    /// Record one line (already redacted).
    fn bump(&mut self, line: String) {
        match self.counts.get_mut(&line) {
            Some(n) => *n += 1,
            None => {
                self.order.push(line.clone());
                self.counts.insert(line, 1);
            }
        }
    }

    /// Render the distinct lines, each annotated with its repeat count
    /// when it occurred more than once.
    fn render(&self) -> String {
        let mut out = String::new();
        for line in &self.order {
            let n = self.counts[line];
            if n > 1 {
                out.push_str(&format!("{line} (repeated {n}×)\n"));
            } else {
                out.push_str(line);
                out.push('\n');
            }
        }
        out
    }

    /// Number of distinct lines (what the header's E/W counts display).
    fn distinct(&self) -> usize {
        self.order.len()
    }
}

/// Compact one command's output per a family spec: a header with the exit
/// status and a line census, the distinct error/warning lines, count
/// notes for the collapsed noise, then every other kept line. `None`
/// when the compact form is not meaningfully smaller (≤70% of the
/// original characters) — the caller keeps the original (fail-open).
fn compact(spec: &FamilySpec, stdout: &str, stderr: &str, exit_code: i32) -> Option<CompactedOutput> {
    let orig_chars = stdout.chars().count() + stderr.chars().count();
    if orig_chars == 0 {
        return None;
    }
    let mut total = 0usize;
    let mut errors = Bucket::new();
    let mut warnings = Bucket::new();
    let mut others = Bucket::new();
    let mut passing = 0usize;
    let mut noise = 0usize;
    for text in [stdout, stderr] {
        for line in text.lines() {
            total += 1;
            match (spec.classify)(line) {
                LineKind::Error => errors.bump(redact_secrets(line)),
                LineKind::Warning => warnings.bump(redact_secrets(line)),
                LineKind::Passing => passing += 1,
                LineKind::Noise => noise += 1,
                LineKind::Keep => others.bump(redact_secrets(line)),
            }
        }
    }
    let mut out = format!(
        "compressed {} output — exit {}: {} error / {} warning / {} passing lines kept of {} total\n",
        spec.label,
        exit_code,
        errors.distinct(),
        warnings.distinct(),
        passing,
        total
    );
    out.push_str(&errors.render());
    out.push_str(&warnings.render());
    if noise > 0 {
        out.push_str(&format!(
            "[compactor] {noise} {} lines collapsed\n",
            spec.noise_label
        ));
    }
    if passing > 0 {
        out.push_str(&format!("[compactor] {passing} passing-test lines collapsed\n"));
    }
    out.push_str(&others.render());
    (out.chars().count() * 10 <= orig_chars * 7).then_some(CompactedOutput { text: out })
}

/// Detect the command family from the leading words. Path prefixes
/// (`./cargo`), `.exe` suffixes, and trailing punctuation
/// (`build;`) are normalized away.
fn detect_family(command: &str) -> Option<&'static FamilySpec> {
    let mut words = command.split_whitespace();
    let first = words.next()?;
    let first = first
        .rsplit(['/', '\\', ':'])
        .next()
        .unwrap_or(first)
        .trim_end_matches(".exe")
        .to_ascii_lowercase();
    let norm = |w: &str| {
        w.trim_matches(|c: char| !c.is_ascii_alphanumeric())
            .to_ascii_lowercase()
    };
    let word = words.next().map(&norm);
    let word2 = words.next().map(&norm);
    match (first.as_str(), word.as_deref(), word2.as_deref()) {
        ("cargo", Some("build" | "check" | "clippy"), _) => Some(&CARGO_BUILD),
        ("cargo", Some("test"), _) => Some(&CARGO_TEST),
        ("pytest" | "py.test", _, _) => Some(&PYTEST),
        ("python" | "python3" | "py", Some("m"), Some("pytest")) => Some(&PYTEST),
        ("npm" | "yarn" | "pnpm" | "bun", Some("test" | "t" | "vitest" | "jest"), _) => {
            Some(&NPM_TEST)
        }
        ("npm" | "yarn" | "pnpm" | "bun", Some("run"), Some("test" | "t" | "vitest" | "jest")) => {
            Some(&NPM_TEST)
        }
        ("npm" | "yarn" | "pnpm" | "bun", Some("build"), _) => Some(&NPM_BUILD),
        ("npm" | "yarn" | "pnpm" | "bun", Some("run"), Some("build" | "vite" | "tsc")) => {
            Some(&NPM_BUILD)
        }
        ("vitest" | "jest", _, _) => Some(&NPM_TEST),
        ("tsc" | "vite", _, _) => Some(&NPM_BUILD),
        ("go", Some("test"), _) => Some(&GO_TEST),
        ("go", Some("build"), _) => Some(&GO_BUILD),
        _ => None,
    }
}

/// Does the command match any user-extensible `compress_extra_commands`
/// regex? Invalid patterns are skipped (fail-open — a typo'd pattern
/// disables nothing else).
fn matches_extra(command: &str, patterns: &[String]) -> bool {
    patterns
        .iter()
        .any(|p| Regex::new(p).map(|re| re.is_match(command)).unwrap_or(false))
}

/// Compress a command's output (token-optimizer lever 2, backlog
/// e4a50d22). Semantic per family: distinct error/warning lines kept
/// verbatim (global dedup), noise/passing lines collapsed to counts, a
/// header carrying the exit status, everything credential-redacted.
///
/// `None` when the command matches no known family and no
/// `compress_extra_commands` pattern — or when the compact form is not
/// meaningfully smaller (fail-open: the caller keeps the original).
///
/// Called by the `shell` tool AFTER
/// [`shell_filter`](super::shell_filter) shaping, and only when
/// `compress_output` is on and the filtered output exceeds
/// `compress_min_chars`; the raw stdout/stderr ride the result's `data`
/// untouched.
pub(crate) fn compress(
    command: &str,
    stdout: &str,
    stderr: &str,
    exit_code: i32,
    cfg: &OptimizerConfig,
) -> Option<CompactedOutput> {
    let spec = detect_family(command).or_else(|| {
        matches_extra(command, &cfg.compress_extra_commands).then_some(&GENERIC)
    })?;
    compact(spec, stdout, stderr, exit_code)
}

/// Redact credentials from a line the model will see:
/// - URI credentials `scheme://user:password@host` → `user:[REDACTED]@`
/// - `Authorization: Bearer <value>`-shaped values → `[REDACTED]`
/// - key/value forms of api-key/token/password/secret/credential →
///   `[REDACTED]`
/// - well-known token prefixes (`sk-…`, `ghp_…`, `AKIA…`, `xox…`) →
///   `[REDACTED]`
///
/// Bias: err toward redaction — a redacted placeholder is recoverable
/// from the command's context; a leaked credential is not. Applied to
/// every line the compactor serves; the raw stdout/stderr in the result's
/// `data` stay untouched (the frontend renders those, the model does not).
pub(crate) fn redact_secrets(text: &str) -> String {
    // Compiled once each — this runs on every line the compactor serves.
    static URI: OnceLock<Regex> = OnceLock::new();
    static BEARER: OnceLock<Regex> = OnceLock::new();
    static KEYVAL: OnceLock<Regex> = OnceLock::new();
    static TOKEN: OnceLock<Regex> = OnceLock::new();
    let uri = URI.get_or_init(|| {
        Regex::new(r"([a-z][a-z0-9+.\-]*://[^:/\s@]+):([^@\s]+)@")
            .expect("static uri-credentials regex must compile")
    });
    let bearer = BEARER.get_or_init(|| {
        Regex::new(r"(?i)\b(bearer|basic)\s+[A-Za-z0-9_\-./+=]{6,}")
            .expect("static bearer regex must compile")
    });
    // Group 3 absorbs an optional scheme word and the value may absorb a
    // leading `scheme://`, so a URI value redacts as ONE unit instead of
    // producing a nested `[REDACTED]]`.
    let keyval = KEYVAL.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(api[-_ ]?key|apikey|access[-_ ]?token|auth[-_ ]?token|token|password|passwd|secret|authorization|credential)\b(\s*["'=:+]\s*)((?:bearer|basic)\s+)?(?:[a-z][a-z0-9+.\-]*://)?([^\s,;]{6,})"#,
        )
        .expect("static key-value regex must compile")
    });
    let token = TOKEN.get_or_init(|| {
        Regex::new(
            r"\b(sk-[A-Za-z0-9_\-]{16,}|ghp_[A-Za-z0-9]{20,}|gho_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|AKIA[0-9A-Z]{16}|xox[baprs]-[A-Za-z0-9\-]{10,})\b",
        )
        .expect("static well-known token regex must compile")
    });
    let out = uri.replace_all(text, "${1}:[REDACTED]@").into_owned();
    let out = bearer.replace_all(&out, "$1 [REDACTED]").into_owned();
    let out = keyval.replace_all(&out, "$1$2$3[REDACTED]").into_owned();
    token.replace_all(&out, "[REDACTED]").into_owned()
}
#[cfg(test)]
mod tests {
    use super::*;

    /// A config with compression on and a low `compress_min_chars` (the
    /// shell-tool gate is exercised at the call site; these tests target
    /// `compress` directly).
    fn cfg() -> OptimizerConfig {
        OptimizerConfig {
            compress_output: true,
            compress_min_chars: 0,
            ..Default::default()
        }
    }

    /// Realistic `cargo build` output: 24 compile-progress lines around a
    /// repeated error block, a distinct warning, and the summary.
    fn cargo_build_fixture() -> String {
        let mut s = String::new();
        for i in 0..8 {
            s.push_str(&format!("   Compiling dep{i} v1.0.{i} (/x)\n"));
        }
        s.push_str("error[E0425]: cannot find value `x` in this scope\n  --> src/a.rs:3:5\n");
        s.push_str("   |\n 3 |     x\n   |     ^\n");
        s.push_str("warning: unused variable: `y`\n  --> src/b.rs:9:9\n");
        for i in 8..24 {
            s.push_str(&format!("   Compiling dep{i} v1.0.{i} (/x)\n"));
        }
        // The same error block a second time — the dedup must collapse it.
        s.push_str("error[E0425]: cannot find value `x` in this scope\n  --> src/a.rs:3:5\n");
        s.push_str("error: could not compile `mnemo` (bin \"mnemo\") due to 1 previous error\n");
        s
    }

    #[test]
    fn cargo_build_keeps_distinct_errors_and_collapses_progress() {
        let out = compress("cargo build", &cargo_build_fixture(), "", 101, &cfg())
            .expect("a large cargo build output must compact");
        // Exit status + census header.
        assert!(out.text.contains("compressed cargo-build output — exit 101"), "{}", out.text);
        assert!(out.text.contains("lines kept of"), "{}", out.text);
        // Distinct error line kept VERBATIM, deduplicated with a count.
        assert!(out.text.contains("error[E0425]: cannot find value `x` in this scope"), "{}", out.text);
        assert!(out.text.contains("(repeated 2×)"), "{}", out.text);
        assert!(out.text.contains("error: could not compile"), "{}", out.text);
        // Warning kept.
        assert!(out.text.contains("warning: unused variable: `y`"), "{}", out.text);
        // Compile progress collapsed to a count note.
        assert!(out.text.contains("compile-progress lines collapsed"), "{}", out.text);
        assert!(!out.text.contains("Compiling dep"), "{}", out.text);
        // Meaningfully smaller.
        let orig = cargo_build_fixture().chars().count();
        assert!(out.text.chars().count() < orig, "{} -> {}", orig, out.text.chars().count());
    }

    #[test]
    fn cargo_test_keeps_failures_and_collapses_passing_tests() {
        let mut stdout = String::new();
        stdout.push_str("   Compiling mnemo v0.1.0\n   Compiling serde v1.0\n");
        stdout.push_str("    Finished `test` profile [unoptimized] target(s) in 1.2s\n");
        stdout.push_str("     Running unittests src/lib.rs\n");
        for i in 0..12 {
            stdout.push_str(&format!("test suite::passing_case_{i} ... ok\n"));
        }
        stdout.push_str("test suite::broken_case ... FAILED\n");
        stdout.push_str("thread 'suite::broken_case' panicked at src/b.rs:12:5:\n");
        stdout.push_str("assertion `left == right` failed\n");
        stdout.push_str("test result: FAILED. 12 passed; 1 failed; 0 ignored\n");
        let out = compress("cargo test", &stdout, "", 101, &cfg()).expect("must compact");
        assert!(out.text.contains("compressed cargo-test output — exit 101"), "{}", out.text);
        // Failure detail kept verbatim.
        assert!(out.text.contains("test suite::broken_case ... FAILED"), "{}", out.text);
        assert!(out.text.contains("assertion `left == right` failed"), "{}", out.text);
        assert!(out.text.contains("test result: FAILED. 12 passed; 1 failed"), "{}", out.text);
        // Passing lines collapsed to a count.
        assert!(out.text.contains("passing-test lines collapsed"), "{}", out.text);
        assert!(!out.text.contains("passing_case_0 ... ok"), "{}", out.text);
    }

    #[test]
    fn family_detection_covers_the_known_commands() {
        for (cmd, label) in [
            ("cargo build", "cargo-build"),
            ("cargo check --all", "cargo-build"),
            ("cargo clippy", "cargo-build"),
            ("cargo test --lib", "cargo-test"),
            ("pytest -q", "pytest"),
            ("python -m pytest tests/", "pytest"),
            ("npm test", "npm-test"),
            ("npm run test", "npm-test"),
            ("yarn vitest run", "npm-test"),
            ("pnpm t", "npm-test"),
            ("npm run build", "npm-build"),
            ("tsc --noEmit", "npm-build"),
            ("vite build", "npm-build"),
            ("go test ./...", "go-test"),
            ("go build ./...", "go-build"),
            // Path/extension/punctuation normalization.
            ("./cargo.exe build", "cargo-build"),
            ("cargo build;", "cargo-build"),
        ] {
            // Assert the MAPPING directly: each family's own noise shape
            // differs, so driving this loop through `compress` would
            // conflate detection with that family's collapsing.
            let spec = detect_family(cmd)
                .unwrap_or_else(|| panic!("{cmd} should map to a family"));
            assert_eq!(spec.label, label, "{cmd}");
        }
    }

    #[test]
    fn unknown_command_passes_through_untouched() {
        // No family match and no extra pattern — no compaction at all.
        assert!(compress("echo hello", "hello\n", "", 0, &cfg()).is_none());
        assert!(compress("python script.py", "output\n", "", 0, &cfg()).is_none());
    }

    #[test]
    fn tiny_output_is_left_alone_fail_open() {
        // A family command whose output is already small: the header +
        // notes would cost MORE than the original — keep it byte-identical.
        assert!(compress("cargo build", "   Compiling a v1\n", "", 0, &cfg()).is_none());
    }

    #[test]
    fn npm_test_keeps_failures_and_collapses_passing_tests() {
        let mut stdout = String::new();
        stdout.push_str("\n RUN  v5.0.0 /x\n\n");
        for i in 0..12 {
            stdout.push_str(&format!(" ✓ test/mod_{i}.test.ts (3 tests) 12ms\n"));
        }
        stdout.push_str(" ❯ test/broken.test.ts > does the thing\n");
        stdout.push_str("   → expected 1 to be 2\n");
        stdout.push_str(" FAIL  test/broken.test.ts > does the thing\n");
        stdout.push_str("AssertionError: expected 1 to be 2\n");
        stdout.push_str("\n Test Files  1 failed | 1 passed (2)\n      Tests  1 failed | 12 passed (13)\n");
        let out = compress("npm test", &stdout, "", 1, &cfg()).expect("must compact");
        assert!(out.text.contains("compressed npm-test output — exit 1"), "{}", out.text);
        assert!(out.text.contains("FAIL  test/broken.test.ts"), "{}", out.text);
        assert!(out.text.contains("AssertionError: expected 1 to be 2"), "{}", out.text);
        assert!(out.text.contains("Tests  1 failed | 12 passed"), "{}", out.text);
        assert!(out.text.contains("passing-test lines collapsed"), "{}", out.text);
        assert!(!out.text.contains("mod_0.test.ts"), "{}", out.text);
    }

    #[test]
    fn npm_build_collapses_the_asset_table() {
        let mut stdout = String::from("vite v5.0.0 building for production...\n");
        for i in 0..20 {
            stdout.push_str(&format!("dist/assets/chunk-{i}.js   {i}.34 kB │ gzip: 4.56 kB\n"));
        }
        stdout.push_str("error during build: failed to resolve import \"nope\"\n");
        stdout.push_str("✓ built in 1.23s\n");
        let out = compress("npm run build", &stdout, "", 1, &cfg()).expect("must compact");
        assert!(out.text.contains("compressed npm-build output"), "{}", out.text);
        assert!(out.text.contains("error during build: failed to resolve import"), "{}", out.text);
        assert!(out.text.contains("build-progress lines collapsed"), "{}", out.text);
        assert!(!out.text.contains("chunk-0.js"), "{}", out.text);
    }

    #[test]
    fn pytest_keeps_failure_detail_and_collapses_the_session_header() {
        let stdout = concat!(
            "============================= test session starts ==============================\n",
            "platform linux -- Python 3.11.0, pytest-7.4.0, pluggy-1.0.0\n",
            "rootdir: /x\n",
            "plugins: cov-4.1.0\n",
            "collected 24 items\n",
            "\n",
            "tests/test_a.py ........................F                            [ 10%]\n",
            "tests/test_b.py ...............................                      [ 20%]\n",
            "tests/test_c.py ...............................                      [ 30%]\n",
            "tests/test_d.py ...............................                      [ 40%]\n",
            "tests/test_e.py ...............................                      [ 50%]\n",
            "tests/test_f.py ...............................                      [ 60%]\n",
            "tests/test_g.py ...............................                      [ 70%]\n",
            "tests/test_h.py ...............................                      [ 80%]\n",
            "\n",
            "=================================== FAILURES ===================================\n",
            "_________________________________ test_x ______________________________________\n",
            "\n",
            "    def test_x():\n",
            ">       assert 1 == 2\n",
            "E       assert 1 == 2\n",
            "\n",
            "tests/test_a.py:10: AssertionError\n",
            "=========================== short test summary info ============================\n",
            "FAILED tests/test_a.py::test_x - assert 1 == 2\n",
            "========================= 1 failed, 23 passed in 0.12s ========================\n",
        );
        let out = compress("pytest -q", stdout, "", 1, &cfg()).expect("must compact");
        assert!(out.text.contains("compressed pytest output — exit 1"), "{}", out.text);
        assert!(out.text.contains("E       assert 1 == 2"), "{}", out.text);
        assert!(out.text.contains("FAILED tests/test_a.py::test_x"), "{}", out.text);
        assert!(out.text.contains("1 failed, 23 passed"), "{}", out.text);
        assert!(out.text.contains("session-header lines collapsed"), "{}", out.text);
        assert!(!out.text.contains("plugins: cov-4.1.0"), "{}", out.text);
    }

    #[test]
    fn go_test_keeps_failures_and_collapses_run_noise() {
        let mut stdout = String::new();
        for i in 0..12 {
            stdout.push_str(&format!("=== RUN   TestPass{i}\n"));
            stdout.push_str(&format!("--- PASS: TestPass{i} (0.00s)\n"));
        }
        stdout.push_str("=== RUN   TestBroken\n");
        stdout.push_str("    broken_test.go:12: got 1, want 2\n");
        stdout.push_str("--- FAIL: TestBroken (0.00s)\n");
        stdout.push_str("FAIL\nFAIL\texample.com/x\t0.003s\n");
        let out = compress("go test ./...", &stdout, "", 1, &cfg()).expect("must compact");
        assert!(out.text.contains("compressed go-test output — exit 1"), "{}", out.text);
        assert!(out.text.contains("broken_test.go:12: got 1, want 2"), "{}", out.text);
        assert!(out.text.contains("--- FAIL: TestBroken"), "{}", out.text);
        assert!(out.text.contains("run-progress lines collapsed"), "{}", out.text);
        assert!(!out.text.contains("=== RUN"), "{}", out.text);
    }

    #[test]
    fn extra_command_patterns_use_the_generic_compressor() {
        let mut cfg = cfg();
        cfg.compress_extra_commands = vec!["dotnet build".to_string()];
        let mut stdout = String::new();
        stdout.push_str("Determining projects to restore...\n");
        for _ in 0..30 {
            stdout.push_str("warn line: something repeated here\n");
        }
        stdout.push_str("Build FAILED.\n");
        let out = compress("dotnet build My.sln", &stdout, "", 1, &cfg).expect("must compact");
        assert!(out.text.contains("compressed generic output"), "{}", out.text);
        assert!(out.text.contains("Build FAILED."), "{}", out.text);
        // The 30 identical lines collapse to one distinct line + count.
        assert!(out.text.contains("(repeated 30×)"), "{}", out.text);
    }

    #[test]
    fn credentials_are_redacted_in_the_served_lines() {
        // URI credentials.
        assert_eq!(
            redact_secrets("fatal: unable to access 'https://alice:hunter2@example.com/x.git'"),
            "fatal: unable to access 'https://alice:[REDACTED]@example.com/x.git'"
        );
        // key/value forms and bearer tokens.
        let kv = redact_secrets("API_KEY=abcdef123456");
        assert!(kv.contains("[REDACTED]") && !kv.contains("abcdef123456"), "{kv}");
        let bearer = redact_secrets("Authorization: Bearer abcdefghijklmnop");
        assert!(bearer.contains("[REDACTED]") && !bearer.contains("abcdefghijklmnop"), "{bearer}");
        // Well-known token prefixes.
        let tok = redact_secrets("using ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789");
        assert!(tok.contains("[REDACTED]") && !tok.contains("ghp_"), "{tok}");
        // A URI value never nests brackets.
        let nested = redact_secrets("token=https://u:p@h/x");
        assert!(!nested.contains("[REDACTED]]"), "{nested}");
    }

    #[test]
    fn redaction_is_applied_to_the_text_the_model_sees() {
        let mut stdout = String::new();
        for i in 0..30 {
            stdout.push_str(&format!("   Compiling dep{i} v1.0\n"));
        }
        stdout.push_str("error: failed to fetch https://bob:s3cret@git.example.com/r.git\n");
        let out = compress("cargo build", &stdout, "", 101, &cfg()).expect("must compact");
        assert!(out.text.contains("bob:[REDACTED]@git.example.com"), "{}", out.text);
        assert!(!out.text.contains("s3cret"), "{}", out.text);
    }
}