// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The one-time offer to run Mnemo without permission prompts.
//!
//! By default Claude Code asks before every Mnemo tool call and before the
//! background waiter, which stalls a lead that works on its own. The first time
//! Claude actually uses Mnemo, `serve` appends a short note to that tool result
//! asking Claude to put the question to the user; if the user agrees, Claude
//! adds three `permissions.allow` rules with its own, visible file edit. This
//! module never writes Claude's settings: it only reads them (to stay quiet
//! when Mnemo is already allowed) and leaves a marker in the Mnemo config dir
//! so the offer is made once per machine.
//!
//! Nothing here may fail or delay a tool call: every I/O problem just skips the
//! offer, with a short note on stderr.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::instance::Env;

/// The rule that allows every tool of the plugin's MCP server.
const SERVER_RULE: &str = "mcp__plugin_mnemo_mnemo__*";
/// A `permissions.allow` entry starting like this already covers Mnemo's tools.
const RULE_PREFIX: &str = "mcp__plugin_mnemo_mnemo";
/// The marker `<config>/claude-lead/permissions-offered`.
const MARKER_DIR: &str = "claude-lead";
const MARKER_FILE: &str = "permissions-offered";

/// What the offer has decided so far.
enum State {
    /// No tool result with a content array has gone by yet.
    Unchecked,
    /// The offer is due; this is the text to append to the next fitting result.
    Pending(String),
    /// Offered, or not to be offered by this process: never look again.
    Closed,
}

/// The offer of one `serve` process.
pub struct Offer {
    /// `<config>/claude-lead/permissions-offered`.
    marker: PathBuf,
    /// Claude Code's user config dir; None when it cannot be found.
    claude_dir: Option<PathBuf>,
    /// This executable, which the waiter rules name.
    helper: PathBuf,
    state: Mutex<State>,
}

impl Offer {
    /// `claude_dir` is Claude Code's user config dir (see [`claude_dir`]); the
    /// marker lives in the Mnemo config dir of `env`.
    pub fn new(env: &Env, claude_dir: Option<PathBuf>) -> Offer {
        Offer {
            marker: env.config_dir.join(MARKER_DIR).join(MARKER_FILE),
            claude_dir,
            helper: env.helper.clone(),
            state: Mutex::new(State::Unchecked),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// `reply` is the line about to be written for a `tools/call` that Mnemo2
    /// answered; `project_root` is the matched instance's root. Returns the
    /// line to write: `reply` itself, or `reply` with the note appended as one
    /// more text block when the offer is due and the reply can carry it (a
    /// JSON-RPC result with a `content` array that is not a tool error; an
    /// error does not use it up).
    pub fn apply(&self, reply: String, project_root: &Path) -> String {
        let mut state = self.lock();
        if matches!(*state, State::Closed) {
            return reply;
        }
        let Ok(mut message) = serde_json::from_str::<Value>(&reply) else {
            return reply;
        };
        // A failed tool call is a poor moment to ask; wait for one that worked.
        if message.pointer("/result/isError").and_then(Value::as_bool) == Some(true) {
            return reply;
        }
        let Some(content) = message
            .pointer_mut("/result/content")
            .and_then(Value::as_array_mut)
        else {
            return reply;
        };
        if matches!(*state, State::Unchecked) {
            *state = match self.prepare(project_root) {
                Ok(Some(text)) => State::Pending(text),
                Ok(None) => State::Closed,
                Err(note) => {
                    eprintln!("mnemo2-mcp: not offering the permission rules: {note}");
                    State::Closed
                }
            };
        }
        let State::Pending(text) = &*state else {
            return reply;
        };
        content.push(json!({ "type": "text", "text": text }));
        let line = message.to_string();
        // Whatever happens next, this process offers at most once.
        *state = State::Closed;
        drop(state);
        match self.claim_marker() {
            Ok(true) => line,
            // Another process made the offer first.
            Ok(false) => reply,
            Err(err) => {
                eprintln!("mnemo2-mcp: not offering the permission rules: {err}");
                reply
            }
        }
    }

    /// The text to offer, or None when there is nothing to offer: the offer was
    /// made before, or Claude's settings already allow Mnemo. Err is a note
    /// for stderr, for when the offer cannot be worded.
    fn prepare(&self, project_root: &Path) -> Result<Option<String>, String> {
        if self.marker.exists() {
            return Ok(None);
        }
        let claude_dir = self
            .claude_dir
            .as_deref()
            .ok_or("Claude Code's config directory is not known")?;
        if already_allowed(claude_dir, project_root) {
            return Ok(None);
        }
        let helper = rule_path(&self.helper)
            .ok_or_else(|| format!("{} cannot be quoted in a rule", self.helper.display()))?;
        Ok(Some(offer_text(
            &claude_dir.join("settings.json"),
            &rules(&helper),
        )))
    }

    /// Create the marker. `Ok(false)` when it already exists (another process
    /// got there first). Creating it is atomic, so two sessions that start
    /// together offer once between them.
    fn claim_marker(&self) -> std::io::Result<bool> {
        if let Some(dir) = self.marker.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = match std::fs::File::options()
            .write(true)
            .create_new(true)
            .open(&self.marker)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
            Err(err) => return Err(err),
        };
        // The marker counts by existing; the time is for whoever looks at it.
        let _ = writeln!(file, "{}", iso_utc(now_secs()));
        Ok(true)
    }
}

/// The command prefix as the rules write it (quoted), or None when the path
/// cannot be quoted safely: a quote or a character a shell expands inside
/// double quotes, control characters, or a `*`, which a rule would read as a
/// wildcard. `lead_instructions` falls back to the bare command name for the
/// same paths, so no rule could match what the lead runs.
fn rule_path(helper: &Path) -> Option<String> {
    let text = helper.to_string_lossy();
    let text = text.trim();
    let usable = !text.is_empty()
        && !text
            .chars()
            .any(|c| matches!(c, '"' | '$' | '`' | '*') || c.is_control());
    usable.then(|| text.to_string())
}

/// The three rules for `helper`, in the order the note lists them: all Mnemo
/// tools, then the waiter under Bash and under PowerShell. The quotes inside
/// the last two are part of the rule.
fn rules(helper: &str) -> [String; 3] {
    [
        SERVER_RULE.to_string(),
        format!("Bash(\"{helper}\" wait *)"),
        format!("PowerShell(& \"{helper}\" wait *)"),
    ]
}

/// The note appended to the tool result. The rules go in as a JSON array, so
/// quotes and backslashes in the helper path arrive escaped the way they must
/// appear in the settings file.
fn offer_text(settings: &Path, rules: &[String; 3]) -> String {
    let rules_json = serde_json::to_string(rules).unwrap_or_default();
    format!(
        "[Mnemo setup, shown once] Claude Code asks before every Mnemo tool call and before the \
         background waiter, so a lead working on its own stalls on prompts. Ask the user now \
         whether Mnemo may run without these prompts. If they agree, add exactly these three \
         entries to the \"permissions\" \u{2192} \"allow\" array in {settings} (create the file \
         or keys if missing, keep every other setting unchanged, show the user the entries \
         before writing): {rules_json}. Rules: \"{SERVER_RULE}\" (all Mnemo tools), the Bash \
         waiter rule and the PowerShell waiter rule. If they decline, do not ask again; the \
         plugin README explains how to add them later. These rules do not cover Claude Code's \
         own agents, which still ask.",
        settings = settings.display(),
    )
}

// ---- Claude's settings -----------------------------------------------------

/// Claude Code's user config dir: `CLAUDE_CONFIG_DIR` when set, else
/// `<home>/.claude`. None when neither gives an absolute path (the settings
/// file could not be named to the user).
pub fn claude_dir(config_dir_var: Option<OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    let dir = match config_dir_var.filter(|value| !value.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home?.join(".claude"),
    };
    dir.is_absolute().then_some(dir)
}

/// [`claude_dir`] for this process.
pub fn claude_dir_from_process() -> Option<PathBuf> {
    claude_dir(std::env::var_os("CLAUDE_CONFIG_DIR"), home_from_process())
}

/// The home directory the way Claude Code finds it (`USERPROFILE` on Windows,
/// `HOME` elsewhere), else the parent of Mnemo's own `~/.mnemo`.
fn home_from_process() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            mnemo::config::global_config_dir()
                .parent()
                .map(Path::to_path_buf)
        })
}

/// The settings files that can allow Mnemo, widest first: the user's two in
/// `claude_dir`, then the project's two.
fn settings_files(claude_dir: &Path, project_root: &Path) -> [PathBuf; 4] {
    [
        claude_dir.join("settings.json"),
        claude_dir.join("settings.local.json"),
        project_root.join(".claude").join("settings.json"),
        project_root.join(".claude").join("settings.local.json"),
    ]
}

/// True when any of the settings files has a `permissions.allow` entry for
/// Mnemo's server. A file that is missing, unreadable or not valid JSON counts
/// as having no rules.
fn already_allowed(claude_dir: &Path, project_root: &Path) -> bool {
    settings_files(claude_dir, project_root)
        .iter()
        .any(|path| allows_mnemo(path))
}

fn allows_mnemo(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let text = String::from_utf8_lossy(&bytes);
    // Windows editors like to put a byte-order mark in front.
    let Ok(settings) = serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) else {
        return false;
    };
    settings
        .pointer("/permissions/allow")
        .and_then(Value::as_array)
        .is_some_and(|rules| {
            rules
                .iter()
                .filter_map(Value::as_str)
                .any(|rule| rule.trim_start().starts_with(RULE_PREFIX))
        })
}

// ---- the marker's timestamp ------------------------------------------------

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// `secs` since the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ` (UTC).
fn iso_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rest = secs % 86_400;
    // Days since 1970-01-01 to a calendar date (Howard Hinnant's civil_from_days).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
