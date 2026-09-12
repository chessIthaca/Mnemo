// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Safety rules — regex-based auto-approve for tool calls.
//!
//! When the user clicks "Mark Safe" on an approval prompt, a regex rule is
//! saved to `.coding/safety.toml`. On subsequent tool calls, if the call's
//! signature matches a rule, the call is auto-approved (the approval prompt
//! is skipped entirely).
//!
//! A rule has a `tool` name and a `pattern` (regex). The pattern is matched
//! against the tool call's **signature** — a string of the form
//! `"<tool>:<key_argument>"` where `<key_argument>` is the most identifying
//! argument (e.g. `path` for file tools, `command` for `shell`).
//!
//! The file is mtime-checked: [`SafetyRules::is_safe`] re-reads the file from
//! disk if its mtime has advanced, so edits in the Safety tab take effect
//! without restarting.

pub mod cmd_class;

pub use cmd_class::classify;

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::tool::agent::git::resolve_git_subcommand;

/// A header comment prepended to every generated `safety.toml`.
const FILE_HEADER: &str = "\
# Safety rules — auto-approve matching tool calls without prompting.
#
# Each rule has a `tool` name and a `pattern`. For `literal` rules (the
# default when `kind` is omitted), `pattern` is a regex matched against the
# tool call's signature (\"<tool>:<key_argument>\"). For `command_class` rules
# (shell-only), `pattern` is a normalized safety class: the command is
# classified into its primary operation (ignoring cosmetic output filtering
# like Select-String patterns and redirections), and any call with the same
# class is auto-approved. Chained (`&&`, `;`) or unknown commands classify to
# nothing and always prompt.
#
# Examples:
#   [[rule]]
#   tool = \"file_write\"
#   pattern = \"^file_write:src/.*\\.rs$\"   # all Rust file writes (literal)
#
#   [[rule]]
#   tool = \"shell\"
#   kind = \"command_class\"
#   pattern = \"cargo test\"                # any `cargo test ...` variant
";

/// The kind of a safety rule — how its `pattern` is interpreted.
///
/// - `Literal` (the default, historical behavior): `pattern` is a regex
///   matched against the tool call's full signature (`"<tool>:<key_argument>"`).
/// - `CommandClass` (shell-only): `pattern` is a normalized safety class (e.g.
///   `"cargo test"`). A shell call is auto-approved when [`classify`]ing its
///   command yields the same class — so cosmetic output filtering
///   (`Select-String`, `2>&1`) is ignored, but command chaining (`&&`, `;`)
///   with an unknown segment is not (it classifies to `None` → no match).
///
/// Serialized as `kind = "literal"` / `kind = "command_class"`. A missing
/// `kind` field (old `safety.toml` files) deserializes as `Literal`, so
/// existing rules keep working unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    Literal,
    CommandClass,
}

impl Default for RuleKind {
    fn default() -> Self {
        Self::Literal
    }
}

impl RuleKind {
    /// Whether this kind is the default `Literal` (used to skip serializing
    /// `kind` for literal rules, keeping the TOML one-line `tool`+`pattern`).
    fn is_literal(&self) -> bool {
        matches!(self, Self::Literal)
    }
}

/// A single safety rule: a tool name + a pattern + how to interpret it.
///
/// For `Literal` rules, `pattern` is a regex matched against the tool call's
/// signature (`"<tool>:<key_argument>"`); `regex` is its compiled form. For
/// `CommandClass` rules, `pattern` is a normalized safety class and `regex`
/// is unused (the matcher compares the classified class to `pattern`).
#[derive(Debug, Clone)]
pub struct Rule {
    /// The tool name this rule applies to (e.g. `"file_write"`).
    pub tool: String,
    /// The regex pattern (source text) for `Literal` rules, or the normalized
    /// safety class for `CommandClass` rules.
    pub pattern: String,
    /// How `pattern` is interpreted.
    pub kind: RuleKind,
    /// The compiled regex (only used by `Literal` rules).
    regex: Regex,
}

impl Rule {
    /// Create a `Literal` rule from a tool name + regex pattern string.
    ///
    /// Returns an error if the pattern is not valid regex.
    pub fn new(tool: impl Into<String>, pattern: impl Into<String>) -> Result<Self> {
        let tool = tool.into();
        let pattern = pattern.into();
        let regex = Regex::new(&pattern)
            .map_err(|e| Error::Safety(format!("invalid regex '{pattern}': {e}")))?;
        Ok(Self {
            tool,
            pattern,
            kind: RuleKind::Literal,
            regex,
        })
    }

    /// Create a `CommandClass` rule for `shell` with the given safety class.
    ///
    /// The `pattern` is the normalized class string (e.g. `"cargo test"`).
    /// No regex is compiled (class rules match by string equality, not regex).
    pub fn new_class(tool: impl Into<String>, class: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            pattern: class.into(),
            kind: RuleKind::CommandClass,
            // A class rule never uses the regex; compile a trivial always-fail
            // pattern so the field is valid (it's never consulted).
            regex: Regex::new(r"^\b\B").expect("trivial regex must compile"),
        }
    }

    /// Whether this rule matches the given signature. For `Literal` rules this
    /// is a regex test; for `CommandClass` rules it's a class comparison
    /// (handled by the caller via [`classify`], since the rule needs the raw
    /// command, not the signature).
    pub fn matches(&self, signature: &str) -> bool {
        self.regex.is_match(signature)
    }
}

/// The on-disk TOML shape: an array of `[[rule]]` tables.
#[derive(Debug, Default, Serialize, Deserialize)]
struct SafetyFile {
    #[serde(default)]
    rule: Vec<RuleEntry>,
}

/// A single `[[rule]]` entry in the TOML file.
#[derive(Debug, Serialize, Deserialize)]
struct RuleEntry {
    tool: String,
    pattern: String,
    /// How `pattern` is interpreted. Defaults to `Literal` when absent (so old
    /// `safety.toml` files without `kind` keep working). Only serialized when
    /// not the default, so literal rules stay one-line `tool`+`pattern`.
    #[serde(default, skip_serializing_if = "RuleKind::is_literal")]
    kind: RuleKind,
}

/// File-backed, mtime-checked safety rules.
///
/// Caches the parsed rules + the file's last-seen mtime. [`is_safe`] re-reads
/// from disk if the mtime has advanced, so edits in the Safety tab take effect
/// without restarting. Invalid regex patterns in the file are skipped
/// (best-effort) — they don't prevent the valid rules from working.
///
/// The mutable cache lives behind a `Mutex` so the struct can be shared behind
/// an `Arc` (the agent loop holds `Option<Arc<SafetyRules>>` and calls
/// `is_safe` through a shared reference). This mirrors the codebase's
/// `ConstitutionSource` pattern.
///
/// [`is_safe`]: SafetyRules::is_safe
pub struct SafetyRules {
    path: PathBuf,
    inner: Mutex<Inner>,
}

/// The mutable cache: parsed rules + the file's last-seen mtime.
#[derive(Default)]
struct Inner {
    cached: Vec<Rule>,
    mtime: Option<SystemTime>,
}

impl SafetyRules {
    /// Create a safety-rules store backed by the given file path.
    ///
    /// Reads the file immediately so the first `is_safe` call doesn't pay the
    /// read cost. A missing file is not an error — the rules start empty.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let (cached, mtime) = load(&path)?;
        Ok(Self {
            path,
            inner: Mutex::new(Inner { cached, mtime }),
        })
    }

    /// Re-read the file if its mtime has advanced since the last read.
    ///
    /// Returns `true` if anything changed. Errors during stat/read are
    /// swallowed (the previous cache is kept) so a transient FS failure can't
    /// break the agent loop.
    ///
    /// # Why not `spawn_blocking`? (review L3)
    ///
    /// `mtime_of` issues a `std::fs::metadata` stat — a microseconds-fast
    /// syscall on an OS-cached inode. Wrapping it in `spawn_blocking` would
    /// add task-allocation + scheduling + context-switch overhead that may
    /// *exceed* the stat itself (a well-known anti-pattern for trivially-fast
    /// blocking ops). This is called once per tool call via `is_safe`, and each
    /// tool call already does file I/O or shell execution (milliseconds to
    /// seconds) — one stat is noise against that. Accepted as a documented
    /// trade-off, not an oversight.
    pub fn reload_if_changed(&self) -> bool {
        let mut inner = self.inner.lock().expect("safety_rules lock poisoned");
        let mtime = mtime_of(&self.path);
        if mtime == inner.mtime {
            return false;
        }
        match load(&self.path) {
            Ok((cached, new_mtime)) => {
                inner.cached = cached;
                inner.mtime = new_mtime;
                true
            }
            Err(_) => {
                // Keep the previous cache on error, but update the mtime so
                // we don't retry every call.
                inner.mtime = mtime;
                false
            }
        }
    }

    /// The cached rules (not re-read from disk).
    ///
    /// Call [`reload_if_changed`](Self::reload_if_changed) first if you need
    /// the latest on-disk rules.
    pub fn rules(&self) -> Vec<Rule> {
        self.inner
            .lock()
            .expect("safety_rules lock poisoned")
            .cached
            .clone()
    }

    /// Whether a tool call is auto-approved by any rule.
    ///
    /// Re-reads the file first if its mtime changed. For each rule whose `tool`
    /// matches: `Literal` rules test their regex against the call's signature;
    /// `CommandClass` rules (shell-only) classify the command and compare the
    /// class to the rule's pattern (so cosmetic output filtering is ignored but
    /// chained/unknown commands are not).
    pub fn is_safe(&self, tool: &str, args: &Value) -> bool {
        self.reload_if_changed();
        let sig = Self::signature(tool, args);
        let inner = self.inner.lock().expect("safety_rules lock poisoned");
        inner.cached.iter().any(|r| {
            if r.tool != tool {
                return false;
            }
            match r.kind {
                RuleKind::Literal => r.matches(&sig),
                // CommandClass rules apply only to shell: classify the command
                // and compare the class to the rule's pattern. A command that
                // can't be classified (None) never matches — it falls through
                // to a prompt.
                RuleKind::CommandClass if tool == "shell" => {
                    let command = key_argument(tool, args);
                    classify(&command) == Some(r.pattern.clone())
                }
                RuleKind::CommandClass => false,
            }
        })
    }

    /// Add a rule for the given tool call + write it to disk.
    ///
    /// Generates a literal-match pattern from the call's signature (escaped +
    /// anchored). If an identical rule already exists, this is a no-op.
    /// Returns the pattern string that was saved.
    pub fn add_rule(&self, tool: &str, args: &Value) -> Result<String> {
        let sig = Self::signature(tool, args);
        let pattern = format!("^{}$", regex::escape(&sig));
        let mut inner = self.inner.lock().expect("safety_rules lock poisoned");
        // Skip if an identical rule already exists.
        if inner
            .cached
            .iter()
            .any(|r| r.tool == tool && r.pattern == pattern)
        {
            return Ok(pattern);
        }
        let rule = Rule::new(tool.to_string(), pattern.clone())?;
        inner.cached.push(rule);
        drop(inner);
        self.persist()?;
        Ok(pattern)
    }

    /// Add a *broad* rule that auto-approves **any** call of the given tool,
    /// regardless of its arguments. The pattern is `^<escaped_tool>:` (the
    /// signature prefix), so it matches every signature for that tool.
    ///
    /// This is the "Allow for project" action: instead of flipping the global
    /// safety mode (which is unpersisted and can desync), it adds an additive,
    /// persistent rule scoped to the whole tool. If an identical broad rule
    /// already exists, this is a no-op. Returns the pattern string that was
    /// saved.
    pub fn add_rule_broad(&self, tool: &str) -> Result<String> {
        let pattern = format!("^{}:", regex::escape(tool));
        let mut inner = self.inner.lock().expect("safety_rules lock poisoned");
        // Skip if an identical rule already exists.
        if inner
            .cached
            .iter()
            .any(|r| r.tool == tool && r.pattern == pattern)
        {
            return Ok(pattern);
        }
        let rule = Rule::new(tool.to_string(), pattern.clone())?;
        inner.cached.push(rule);
        drop(inner);
        self.persist()?;
        Ok(pattern)
    }

    /// Add a `CommandClass` rule for a shell command (the "Mark Safe (same
    /// operation)" action).
    ///
    /// Classifies the command into a normalized safety class (e.g. `cargo test`)
    /// and saves a rule that auto-approves any shell call with the same class —
    /// ignoring cosmetic output filtering (`Select-String`, `2>&1`) but still
    /// prompting for chained (`&&`, `;`) or unknown commands (those classify
    /// to `None`). Only applies to `shell`; other tools return an error.
    ///
    /// Returns the class string that was saved, or an error if the command
    /// can't be classified (so the UI can inform the user / fall back). If an
    /// identical class rule already exists, this is a no-op.
    pub fn add_rule_class(&self, tool: &str, args: &Value) -> Result<String> {
        if tool != "shell" {
            return Err(Error::Safety(
                "command-class rules only apply to the shell tool".to_string(),
            ));
        }
        let command = key_argument(tool, args);
        let class = classify(&command).ok_or_else(|| {
            Error::Safety(
                "command cannot be classified as safe (chained, unknown, or unparseable)"
                    .to_string(),
            )
        })?;
        let mut inner = self.inner.lock().expect("safety_rules lock poisoned");
        // Skip if an identical class rule already exists.
        if inner
            .cached
            .iter()
            .any(|r| r.tool == tool && r.kind == RuleKind::CommandClass && r.pattern == class)
        {
            return Ok(class);
        }
        let rule = Rule::new_class(tool.to_string(), class.clone());
        inner.cached.push(rule);
        drop(inner);
        self.persist()?;
        Ok(class)
    }

    /// The raw TOML text of the file (or an empty string if no file).
    pub fn read_raw(&self) -> String {
        read_optional(&self.path).unwrap_or_default()
    }

    /// Write raw TOML text to the file + reload the cache.
    ///
    /// The text is parsed first to validate the TOML structure; invalid TOML
    /// returns an error and the file is not written. Invalid regex patterns
    /// within the rules are skipped (best-effort) — the file is still written
    /// so the user can fix the pattern in the editor.
    pub fn write_raw(&self, text: &str) -> Result<()> {
        // Validate by parsing (errors on bad TOML; skips bad regex).
        let parsed = parse(text)?;
        // Write to disk.
        std::fs::write(&self.path, text)?;
        // Update the cache + mtime.
        let mut inner = self.inner.lock().expect("safety_rules lock poisoned");
        inner.cached = parsed;
        inner.mtime = mtime_of(&self.path);
        Ok(())
    }

    /// Compute a tool call's signature: `"<tool>:<key_argument>"`.
    ///
    /// The key argument is the most identifying argument for the tool:
    /// - file tools (`file_read`, `file_write`, `file_edit`, `file_append`,
    ///   `convert_line_endings`): `path`
    /// - `shell`: `command`
    /// - `git`: the resolved `subcommand` (either field, or a branch/stash
    ///   action name)
    /// - `search`: `pattern`
    /// - other tools: empty (signature is just `"<tool>:"`)
    pub fn signature(tool: &str, args: &Value) -> String {
        format!("{}:{}", tool, key_argument(tool, args))
    }

    /// Write the cached rules to the file as TOML + update the mtime.
    fn persist(&self) -> Result<()> {
        let inner = self.inner.lock().expect("safety_rules lock poisoned");
        let file = SafetyFile {
            rule: inner
                .cached
                .iter()
                .map(|r| RuleEntry {
                    tool: r.tool.clone(),
                    pattern: r.pattern.clone(),
                    kind: r.kind,
                })
                .collect(),
        };
        drop(inner);
        let body = toml::to_string(&file)?;
        let text = format!("{FILE_HEADER}\n{body}");
        std::fs::write(&self.path, text)?;
        let mut inner = self.inner.lock().expect("safety_rules lock poisoned");
        inner.mtime = mtime_of(&self.path);
        Ok(())
    }
}

/// Extract the most identifying argument value from a tool call.
fn key_argument(tool: &str, args: &Value) -> String {
    let key = match tool {
        "file_read" | "file_write" | "file_edit" | "file_append" | "convert_line_endings" => "path",
        "shell" => "command",
        // The git subcommand is resolved forgivingly (either field, or a
        // branch/stash action name), so a safety rule saved from a swapped
        // call still matches the canonical subcommand.
        "git" => return resolve_git_subcommand(args).unwrap_or_default(),
        "search" | "search_read" => "pattern",
        _ => return String::new(),
    };
    args.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Load + parse the file, returning the rules + the file's mtime.
fn load(path: &Path) -> Result<(Vec<Rule>, Option<SystemTime>)> {
    let text = read_optional(path)?;
    let rules = parse(&text)?;
    let mtime = mtime_of(path);
    Ok((rules, mtime))
}

/// Parse TOML text into rules, skipping entries with invalid regex (for
/// `Literal` rules). `CommandClass` rules don't compile a regex, so they're
/// always kept (their pattern is a plain class string).
fn parse(text: &str) -> Result<Vec<Rule>> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let file: SafetyFile = toml::from_str(text)?;
    let mut rules = Vec::with_capacity(file.rule.len());
    for entry in file.rule {
        match entry.kind {
            RuleKind::CommandClass => rules.push(Rule::new_class(entry.tool, entry.pattern)),
            RuleKind::Literal => match Rule::new(entry.tool, entry.pattern) {
                Ok(r) => rules.push(r),
                Err(e) => {
                    // Skip invalid regex rules — don't fail the whole load.
                    eprintln!("safety_rules: skipping invalid rule: {e}");
                }
            },
        }
    }
    Ok(rules)
}

/// The mtime of a path, or `None` if it doesn't exist / can't be stat'd.
fn mtime_of(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Read a file, returning an empty string if it doesn't exist.
fn read_optional(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    Ok(std::fs::read_to_string(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    // --- signature extraction ------------------------------------------------

    #[test]
    fn signature_file_tools_use_path() {
        assert_eq!(
            SafetyRules::signature("file_write", &json!({"path": "src/foo.rs"})),
            "file_write:src/foo.rs"
        );
        assert_eq!(
            SafetyRules::signature("file_edit", &json!({"path": "a.txt"})),
            "file_edit:a.txt"
        );
        assert_eq!(
            SafetyRules::signature("file_read", &json!({"path": "b.rs"})),
            "file_read:b.rs"
        );
        assert_eq!(
            SafetyRules::signature("file_append", &json!({"path": "c.md"})),
            "file_append:c.md"
        );
        assert_eq!(
            SafetyRules::signature("convert_line_endings", &json!({"path": "d.txt"})),
            "convert_line_endings:d.txt"
        );
    }

    #[test]
    fn signature_shell_uses_command() {
        assert_eq!(
            SafetyRules::signature("shell", &json!({"command": "git status"})),
            "shell:git status"
        );
    }

    #[test]
    fn signature_git_uses_subcommand() {
        assert_eq!(
            SafetyRules::signature("git", &json!({"subcommand": "commit"})),
            "git:commit"
        );
    }

    #[test]
    fn signature_git_resolves_action_field() {
        // The subcommand may arrive in the `action` field, or as a
        // branch/stash action name — the signature normalizes to the
        // canonical subcommand so saved rules match swapped calls.
        assert_eq!(
            SafetyRules::signature("git", &json!({"action": "commit", "message": "x"})),
            "git:commit"
        );
        assert_eq!(
            SafetyRules::signature("git", &json!({"subcommand": "delete"})),
            "git:branch"
        );
        assert_eq!(
            SafetyRules::signature("git", &json!({"action": "pop"})),
            "git:stash"
        );
        // Nothing recognizable → empty key (signature is just "git:").
        assert_eq!(SafetyRules::signature("git", &json!({})), "git:");
    }

    #[test]
    fn signature_search_uses_pattern() {
        assert_eq!(
            SafetyRules::signature("search", &json!({"pattern": "TODO"})),
            "search:TODO"
        );
        // search_read (the search+read combo) uses the same `pattern` key arg.
        assert_eq!(
            SafetyRules::signature("search_read", &json!({"pattern": "TODO"})),
            "search_read:TODO"
        );
    }

    #[test]
    fn signature_unknown_tool_empty_key() {
        assert_eq!(
            SafetyRules::signature("memory_write", &json!({"title": "x"})),
            "memory_write:"
        );
    }

    #[test]
    fn signature_missing_key_arg() {
        // The tool is known but the key arg is absent — empty key.
        assert_eq!(
            SafetyRules::signature("file_write", &json!({})),
            "file_write:"
        );
    }

    // --- is_safe --------------------------------------------------------------

    #[test]
    fn is_safe_false_with_no_rules() {
        let dir = tempdir().unwrap();
        let sr = SafetyRules::new(dir.path().join("safety.toml")).unwrap();
        assert!(!sr.is_safe("file_write", &json!({"path": "a.rs"})));
    }

    #[test]
    fn is_safe_true_after_add_rule() {
        let dir = tempdir().unwrap();
        let sr = SafetyRules::new(dir.path().join("safety.toml")).unwrap();
        sr.add_rule("file_write", &json!({"path": "src/foo.rs"}))
            .unwrap();
        assert!(sr.is_safe("file_write", &json!({"path": "src/foo.rs"})));
    }

    #[test]
    fn is_safe_false_for_different_path() {
        let dir = tempdir().unwrap();
        let sr = SafetyRules::new(dir.path().join("safety.toml")).unwrap();
        sr.add_rule("file_write", &json!({"path": "src/foo.rs"}))
            .unwrap();
        // A different path should not match the literal rule.
        assert!(!sr.is_safe("file_write", &json!({"path": "src/bar.rs"})));
    }

    #[test]
    fn is_safe_false_for_different_tool() {
        let dir = tempdir().unwrap();
        let sr = SafetyRules::new(dir.path().join("safety.toml")).unwrap();
        sr.add_rule("file_write", &json!({"path": "x.rs"})).unwrap();
        // Same path but different tool — must not match.
        assert!(!sr.is_safe("file_edit", &json!({"path": "x.rs"})));
    }

    #[test]
    fn is_safe_with_regex_pattern_from_file() {
        // A hand-written rule with a wildcard pattern should match multiple paths.
        // Use TOML literal strings (single quotes) so backslashes in the regex
        // are not treated as TOML escape sequences.
        let dir = tempdir().unwrap();
        let path = dir.path().join("safety.toml");
        std::fs::write(
            &path,
            r#"
[[rule]]
tool = "file_write"
pattern = '^file_write:src/.*\.rs$'
"#,
        )
        .unwrap();
        let sr = SafetyRules::new(&path).unwrap();
        assert!(sr.is_safe("file_write", &json!({"path": "src/foo.rs"})));
        assert!(sr.is_safe("file_write", &json!({"path": "src/bar.rs"})));
        // Non-rust file doesn't match.
        assert!(!sr.is_safe("file_write", &json!({"path": "src/foo.txt"})));
    }

    // --- add_rule -------------------------------------------------------------

    #[test]
    fn add_rule_returns_escaped_pattern() {
        let dir = tempdir().unwrap();
        let sr = SafetyRules::new(dir.path().join("safety.toml")).unwrap();
        let pattern = sr
            .add_rule("file_write", &json!({"path": "src/foo.rs"}))
            .unwrap();
        // The pattern is an anchored, escaped literal of the signature.
        assert_eq!(pattern, r"^file_write:src/foo\.rs$");
    }

    #[test]
    fn add_rule_persists_to_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("safety.toml");
        let sr = SafetyRules::new(&path).unwrap();
        sr.add_rule("shell", &json!({"command": "git status"}))
            .unwrap();
        // The file should exist and contain the rule.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("tool = \"shell\""));
        assert!(raw.contains(r"git status"));
    }

    #[test]
    fn add_rule_duplicate_is_noop() {
        let dir = tempdir().unwrap();
        let sr = SafetyRules::new(dir.path().join("safety.toml")).unwrap();
        sr.add_rule("file_write", &json!({"path": "a.rs"})).unwrap();
        sr.add_rule("file_write", &json!({"path": "a.rs"})).unwrap();
        // Only one rule should be stored.
        assert_eq!(sr.rules().len(), 1);
    }

    #[test]
    fn add_rule_multiple_distinct() {
        let dir = tempdir().unwrap();
        let sr = SafetyRules::new(dir.path().join("safety.toml")).unwrap();
        sr.add_rule("file_write", &json!({"path": "a.rs"})).unwrap();
        sr.add_rule("file_write", &json!({"path": "b.rs"})).unwrap();
        sr.add_rule("shell", &json!({"command": "ls"})).unwrap();
        assert_eq!(sr.rules().len(), 3);
    }

    // --- add_rule_broad -------------------------------------------------------

    #[test]
    fn add_rule_broad_returns_prefix_pattern() {
        let dir = tempdir().unwrap();
        let sr = SafetyRules::new(dir.path().join("safety.toml")).unwrap();
        let pattern = sr.add_rule_broad("file_edit").unwrap();
        // The broad pattern matches any signature for the tool: ^file_edit:
        assert_eq!(pattern, r"^file_edit:");
    }

    #[test]
    fn add_rule_broad_matches_multiple_paths() {
        let dir = tempdir().unwrap();
        let sr = SafetyRules::new(dir.path().join("safety.toml")).unwrap();
        sr.add_rule_broad("file_edit").unwrap();
        // The broad rule should auto-approve any file_edit call, regardless of
        // the path argument.
        assert!(sr.is_safe("file_edit", &json!({"path": "src/foo.rs"})));
        assert!(sr.is_safe("file_edit", &json!({"path": "other/bar.txt"})));
        assert!(sr.is_safe("file_edit", &json!({"path": "deeply/nested/path.md"})));
    }

    #[test]
    fn add_rule_broad_does_not_match_other_tools() {
        let dir = tempdir().unwrap();
        let sr = SafetyRules::new(dir.path().join("safety.toml")).unwrap();
        sr.add_rule_broad("file_edit").unwrap();
        // A broad file_edit rule must not auto-approve file_write or shell.
        assert!(!sr.is_safe("file_write", &json!({"path": "src/foo.rs"})));
        assert!(!sr.is_safe("shell", &json!({"command": "ls"})));
    }

    #[test]
    fn add_rule_broad_duplicate_is_noop() {
        let dir = tempdir().unwrap();
        let sr = SafetyRules::new(dir.path().join("safety.toml")).unwrap();
        sr.add_rule_broad("file_edit").unwrap();
        sr.add_rule_broad("file_edit").unwrap();
        assert_eq!(sr.rules().len(), 1);
    }

    #[test]
    fn add_rule_broad_persists_to_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("safety.toml");
        let sr = SafetyRules::new(&path).unwrap();
        sr.add_rule_broad("file_edit").unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("tool = \"file_edit\""));
        assert!(raw.contains(r"^file_edit:"));
    }

    #[test]
    fn add_rule_broad_and_narrow_coexist() {
        // A broad rule and a narrow rule for the same tool are distinct rules
        // (different patterns) and both are stored.
        let dir = tempdir().unwrap();
        let sr = SafetyRules::new(dir.path().join("safety.toml")).unwrap();
        sr.add_rule("file_edit", &json!({"path": "src/foo.rs"}))
            .unwrap();
        sr.add_rule_broad("file_edit").unwrap();
        assert_eq!(sr.rules().len(), 2);
    }

    // --- reload_if_changed ----------------------------------------------------

    #[test]
    fn reload_picks_up_external_edits() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("safety.toml");
        let sr = SafetyRules::new(&path).unwrap();
        assert!(sr.rules().is_empty());

        // Externally write a rule file.
        std::fs::write(
            &path,
            r#"
[[rule]]
tool = "shell"
pattern = "^shell:.*"
"#,
        )
        .unwrap();
        // Sleep so the mtime resolution can't collapse the two writes.
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // is_safe re-reads on mtime change — the new rule should be active.
        assert!(sr.is_safe("shell", &json!({"command": "anything"})));
        assert_eq!(sr.rules().len(), 1);
    }

    #[test]
    fn reload_no_change_is_noop() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("safety.toml");
        let sr = SafetyRules::new(&path).unwrap();
        assert!(!sr.reload_if_changed());
    }

    // --- read_raw / write_raw -------------------------------------------------

    #[test]
    fn read_raw_returns_file_contents() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("safety.toml");
        std::fs::write(
            &path,
            "# comment\n[[rule]]\ntool = \"x\"\npattern = \"y\"\n",
        )
        .unwrap();
        let sr = SafetyRules::new(&path).unwrap();
        let raw = sr.read_raw();
        assert!(raw.contains("# comment"));
        assert!(raw.contains("[[rule]]"));
    }

    #[test]
    fn read_raw_empty_when_no_file() {
        let dir = tempdir().unwrap();
        let sr = SafetyRules::new(dir.path().join("safety.toml")).unwrap();
        assert_eq!(sr.read_raw(), "");
    }

    #[test]
    fn write_raw_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("safety.toml");
        let sr = SafetyRules::new(&path).unwrap();
        let text = r#"
[[rule]]
tool = "file_write"
pattern = "^file_write:src/.*$"
"#;
        sr.write_raw(text).unwrap();
        // The cache should reflect the written rules.
        assert_eq!(sr.rules().len(), 1);
        assert!(sr.is_safe("file_write", &json!({"path": "src/anything"})));
        // read_raw returns what we wrote.
        assert_eq!(sr.read_raw(), text);
    }

    #[test]
    fn write_raw_invalid_toml_errors() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("safety.toml");
        let sr = SafetyRules::new(&path).unwrap();
        let result = sr.write_raw("this is not [valid toml");
        assert!(result.is_err());
        // The file should not have been written.
        assert!(!path.exists());
    }

    #[test]
    fn write_raw_skips_invalid_regex() {
        // A rule with an invalid regex should not prevent the write — it's
        // skipped (best-effort) and the valid rules still work.
        let dir = tempdir().unwrap();
        let path = dir.path().join("safety.toml");
        let sr = SafetyRules::new(&path).unwrap();
        let text = r#"
[[rule]]
tool = "file_write"
pattern = "^file_write:src/.*$"

[[rule]]
tool = "shell"
pattern = "[invalid"
"#;
        sr.write_raw(text).unwrap();
        // Only the valid rule is in the cache.
        assert_eq!(sr.rules().len(), 1);
        assert!(sr.is_safe("file_write", &json!({"path": "src/x"})));
    }

    // --- missing file / edge cases --------------------------------------------

    #[test]
    fn missing_file_starts_empty() {
        let dir = tempdir().unwrap();
        let sr = SafetyRules::new(dir.path().join("safety.toml")).unwrap();
        assert!(sr.rules().is_empty());
    }

    #[test]
    fn empty_file_starts_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("safety.toml");
        std::fs::write(&path, "").unwrap();
        let sr = SafetyRules::new(&path).unwrap();
        assert!(sr.rules().is_empty());
    }

    #[test]
    fn invalid_regex_in_file_skipped() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("safety.toml");
        std::fs::write(
            &path,
            r#"
[[rule]]
tool = "file_write"
pattern = "^file_write:ok$"

[[rule]]
tool = "shell"
pattern = "[broken"
"#,
        )
        .unwrap();
        let sr = SafetyRules::new(&path).unwrap();
        // Only the valid rule loaded.
        assert_eq!(sr.rules().len(), 1);
        assert_eq!(sr.rules()[0].tool, "file_write");
    }

    #[test]
    fn rule_new_invalid_regex_errors() {
        let result = Rule::new("shell", "[invalid");
        assert!(result.is_err());
    }

    #[test]
    fn rule_matches_anchored_pattern() {
        let rule = Rule::new("shell", r"^shell:git .*$").unwrap();
        assert!(rule.matches("shell:git status"));
        assert!(rule.matches("shell:git commit"));
        assert!(!rule.matches("shell:ls"));
    }

    // --- command-class rules -------------------------------------------------

    #[test]
    fn class_rule_approves_same_operation_ignoring_filters() {
        // A class rule for "cargo test" approves the bare command AND any
        // variant with cosmetic output filtering (Select-String, 2>&1).
        let dir = tempdir().unwrap();
        let path = dir.path().join("safety.toml");
        std::fs::write(
            &path,
            r#"
[[rule]]
tool = "shell"
kind = "command_class"
pattern = "cargo test"
"#,
        )
        .unwrap();
        let sr = SafetyRules::new(&path).unwrap();
        // Bare command.
        assert!(sr.is_safe("shell", &json!({"command": "cargo test"})));
        // With output filtering — same class.
        assert!(sr.is_safe(
            "shell",
            &json!({"command": r#"cargo test 2>&1 | Select-String -Pattern "foo""#})
        ));
        assert!(sr.is_safe(
            "shell",
            &json!({"command": r#"cargo test 2>&1 | Select-String -Pattern "bar""#})
        ));
    }

    #[test]
    fn class_rule_rejects_chained_and_different_commands() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("safety.toml");
        std::fs::write(
            &path,
            r#"
[[rule]]
tool = "shell"
kind = "command_class"
pattern = "cargo test"
"#,
        )
        .unwrap();
        let sr = SafetyRules::new(&path).unwrap();
        // Chained destructive command → classifies to None → NOT approved.
        assert!(!sr.is_safe("shell", &json!({"command": "cargo test && rm -rf x"})));
        // Different command (different class).
        assert!(!sr.is_safe("shell", &json!({"command": "cargo build"})));
        // Unrecognized subcommand → None.
        assert!(!sr.is_safe("shell", &json!({"command": "cargo test123"})));
    }

    #[test]
    fn class_rule_and_literal_rule_coexist() {
        // A class rule for shell + a literal rule for file_write both work.
        let dir = tempdir().unwrap();
        let path = dir.path().join("safety.toml");
        std::fs::write(
            &path,
            r#"
[[rule]]
tool = "shell"
kind = "command_class"
pattern = "cargo test"

[[rule]]
tool = "file_write"
pattern = "^file_write:src/.*\\.rs$"
"#,
        )
        .unwrap();
        let sr = SafetyRules::new(&path).unwrap();
        assert!(sr.is_safe("shell", &json!({"command": "cargo test"})));
        assert!(sr.is_safe("file_write", &json!({"path": "src/foo.rs"})));
        assert!(!sr.is_safe("file_write", &json!({"path": "src/foo.txt"})));
    }

    #[test]
    fn add_rule_class_saves_and_dedupes() {
        let dir = tempdir().unwrap();
        let sr = SafetyRules::new(dir.path().join("safety.toml")).unwrap();
        let class = sr
            .add_rule_class(
                "shell",
                &json!({"command": r#"cargo test 2>&1 | Select-String x"#}),
            )
            .unwrap();
        assert_eq!(class, "cargo test");
        assert_eq!(sr.rules().len(), 1);
        // Adding the same class (via a different variant) is a no-op.
        let class2 = sr
            .add_rule_class("shell", &json!({"command": "cargo test"}))
            .unwrap();
        assert_eq!(class2, "cargo test");
        assert_eq!(
            sr.rules().len(),
            1,
            "duplicate class rule should be a no-op"
        );
        // The saved rule approves both variants.
        assert!(sr.is_safe("shell", &json!({"command": "cargo test"})));
        assert!(sr.is_safe(
            "shell",
            &json!({"command": r#"cargo test 2>&1 | Select-String y"#})
        ));
    }

    #[test]
    fn add_rule_class_errors_on_unclassifiable() {
        let dir = tempdir().unwrap();
        let sr = SafetyRules::new(dir.path().join("safety.toml")).unwrap();
        // Chained unknown command → can't classify → error.
        let result = sr.add_rule_class("shell", &json!({"command": "cargo test && rm x"}));
        assert!(result.is_err());
        assert_eq!(sr.rules().len(), 0, "no rule should be saved on error");
    }

    #[test]
    fn add_rule_class_errors_for_non_shell() {
        let dir = tempdir().unwrap();
        let sr = SafetyRules::new(dir.path().join("safety.toml")).unwrap();
        let result = sr.add_rule_class("file_write", &json!({"path": "x.rs"}));
        assert!(result.is_err());
    }

    #[test]
    fn class_rule_persists_kind_field() {
        // A class rule written to disk round-trips with its `kind` field.
        let dir = tempdir().unwrap();
        let path = dir.path().join("safety.toml");
        let sr = SafetyRules::new(&path).unwrap();
        sr.add_rule_class("shell", &json!({"command": "cargo test"}))
            .unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("kind = \"command_class\""));
        assert!(raw.contains("pattern = \"cargo test\""));
        // Reload from disk — the rule still works.
        let sr2 = SafetyRules::new(&path).unwrap();
        assert!(sr2.is_safe("shell", &json!({"command": "cargo test"})));
    }

    #[test]
    fn missing_kind_defaults_to_literal() {
        // An old safety.toml without `kind` fields still parses as Literal.
        let dir = tempdir().unwrap();
        let path = dir.path().join("safety.toml");
        std::fs::write(
            &path,
            r#"
[[rule]]
tool = "shell"
pattern = "^shell:git status$"
"#,
        )
        .unwrap();
        let sr = SafetyRules::new(&path).unwrap();
        assert_eq!(sr.rules().len(), 1);
        assert_eq!(sr.rules()[0].kind, RuleKind::Literal);
        assert!(sr.is_safe("shell", &json!({"command": "git status"})));
    }
}
