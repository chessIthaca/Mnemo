// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The Claude Code hook modes (`notices`, `gate-agent`, `hint`) and the
//! background `wait` command.
//!
//! A hook must never break the user's session: every failure here ends in "no
//! output", and no mode starts Mnemo2. Without a run file naming the caller's
//! project the hooks return before making any network call.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::instance::{
    call, find_instance, find_instance_for_hook, registered_root, root_body, Env, Instance,
    HOOK_TIMEOUT,
};

/// Extra read time on top of the wait's own timeout, so the server's "no events"
/// reply wins the race against our socket timeout.
const WAIT_SLACK: Duration = Duration::from_secs(30);

/// `wait` could not find (or keep talking to) Mnemo2. The lead must not start
/// another waiter: that would loop forever while Mnemo2 is down.
pub const NOT_REACHABLE: &str = "Mnemo2 is not reachable for this project. Do not restart the waiter; call team_status once Mnemo2 is running again.";
/// The server released this lead while it was waiting.
pub const DETACHED: &str = "The Mnemo lead was detached (another session closed or Mnemo2 restarted). Call team_status before restarting the waiter.";
/// Mnemo2 runs, but the project is not open in it.
pub const NO_PROJECT: &str = "Mnemo2 has no project open for this directory. Do not restart the waiter; call team_status once the project is open in Mnemo2.";

/// Which `notices` hook is running.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoticesMode {
    /// PostToolUse: hand over (and clear) the queued notices.
    Drain,
    /// SessionStart: summary plus anything already queued.
    SessionStart,
    /// Stop: ask whether workers are running with nobody waiting.
    StopCheck,
}

#[derive(Deserialize, Default)]
struct NoticesReply {
    #[serde(default)]
    attached: bool,
    #[serde(default)]
    notices: Vec<String>,
    #[serde(default)]
    active_workers: u32,
    #[serde(default)]
    waiter_alive: bool,
    #[serde(default)]
    summary: String,
}

/// The directory a hook reports: stdin `cwd`, else `CLAUDE_PROJECT_DIR`, else
/// the process cwd.
fn hook_cwd(env: &Env, input: &Value) -> PathBuf {
    input
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)
        .or_else(|| env.project_dir.clone())
        .unwrap_or_else(|| env.process_cwd.clone())
}

fn parse_input(text: &str) -> Value {
    serde_json::from_str(text.trim_start_matches('\u{feff}')).unwrap_or(Value::Null)
}

/// True when the hook fired for a tool call made inside a subagent: Claude Code
/// adds a non-empty `agent_id` then. Those calls must not consume the lead's
/// notices or trigger its stop check.
fn from_subagent(input: &Value) -> bool {
    input
        .get("agent_id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.trim().is_empty())
}

/// The Claude Code session the hook fired for, when its input names one
/// (`session_id`). The server records it for the staking lead, so that
/// session's own hooks — and no other session's — can re-attach it after an
/// idle release.
fn hook_session(input: &Value) -> Option<&str> {
    input
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|session| !session.is_empty())
}

/// The helper path for the waiter command lines. A path that could break out
/// of the quotes falls back to the bare command name.
fn quoted_helper(env: &Env) -> String {
    let helper = env.helper.to_string_lossy();
    let usable = !helper.is_empty() && !helper.chars().any(|c| c == '"' || c.is_control());
    format!("\"{}\"", if usable { helper.as_ref() } else { "mnemo2-mcp" })
}

/// The exact waiter command lines for the Claude Code session this hook fired
/// for — Bash and PowerShell, named with `--session <id>`, so staking the
/// lead lets that session's own hooks (and no other session's) re-attach it
/// after an idle release. `None` when the input names no session: then a
/// started waiter stays anonymous, as before.
fn waiter_commands(env: &Env, input: &Value) -> Option<String> {
    let session = hook_session(input)?;
    let helper = quoted_helper(env);
    Some(format!(
        "Bash: {helper} wait --session {session} \u{2014} PowerShell: & {helper} wait --session {session}"
    ))
}

fn notice_lines(notices: &[String]) -> String {
    let lines: Vec<String> = notices.iter().map(|notice| format!("- {notice}")).collect();
    format!("Mnemo notices:\n{}", lines.join("\n"))
}

fn hook_output(event: &str, context: &str) -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": event,
            "additionalContext": context,
        }
    })
    .to_string()
}

/// `notices --drain | --session-start | --stop-check`: the JSON to print, if any.
pub fn notices(env: &Env, mode: NoticesMode, stdin_text: &str) -> Option<String> {
    let input = parse_input(stdin_text);
    let stop_hook_active = input
        .get("stop_hook_active")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if mode == NoticesMode::StopCheck && stop_hook_active {
        return None;
    }
    // A subagent's tool calls are not the lead's: leave its notices queued.
    if mode != NoticesMode::SessionStart && from_subagent(&input) {
        return None;
    }
    let instance = find_instance_for_hook(env, &hook_cwd(env, &input))?;
    let wire_mode = if mode == NoticesMode::StopCheck {
        "peek"
    } else {
        "drain"
    };
    // Name the session when Claude Code does: the server re-attaches an
    // idle-released lead only for the session that staked it.
    let mut body = json!({ "root": instance.root, "mode": wire_mode });
    if mode == NoticesMode::SessionStart {
        // Only a real SessionStart pays for the knowledge map (plan 373dec00
        // step 6); a PostToolUse drain carries the same "drain" mode.
        body["session_start"] = json!(true);
    }
    if let Some(session) = hook_session(&input) {
        body["session"] = json!(session);
    }
    let reply = call(&instance, "/lead/notices", &body, HOOK_TIMEOUT)?;
    let reply: NoticesReply = serde_json::from_value(reply).ok()?;
    match mode {
        NoticesMode::Drain => {
            if reply.notices.is_empty() {
                return None;
            }
            let mut context = notice_lines(&reply.notices);
            // Workers are running and no waiter: the session may have
            // compacted away the waiter command it was taught — repeat it, so
            // a waiter named for this session keeps the claim re-attachable.
            if reply.attached && reply.active_workers > 0 && !reply.waiter_alive {
                if let Some(commands) = waiter_commands(env, &input) {
                    context.push_str(&format!(
                        "\n\n{} Mnemo worker(s) are still running and no waiter is active. \
                         Start this as a background command so you are woken when one reports. {commands}",
                        reply.active_workers,
                    ));
                }
            }
            Some(hook_output("PostToolUse", &context))
        }
        // The server fills the summary whenever the root is its open project:
        // SessionStart can run before MCP `initialize` attaches the lead.
        NoticesMode::SessionStart => {
            let mut context = reply.summary.trim().to_string();
            if !reply.notices.is_empty() {
                if !context.is_empty() {
                    context.push_str("\n\n");
                }
                context.push_str(&notice_lines(&reply.notices));
            }
            // Teach the session its own waiter command up front: the MCP
            // instructions and the skill cannot name the session, and an
            // unnamed claim cannot come back through its hooks after an
            // idle release.
            if let Some(commands) = waiter_commands(env, &input) {
                if !context.is_empty() {
                    context.push_str("\n\n");
                }
                context.push_str(
                    "Whenever you start a Mnemo waiter, use this command (it names your Claude Code session, so your own hooks can re-attach you after an idle release). ",
                );
                context.push_str(&commands);
            }
            if context.is_empty() {
                return None;
            }
            Some(hook_output("SessionStart", &context))
        }
        NoticesMode::StopCheck => {
            if !reply.attached || reply.active_workers == 0 || reply.waiter_alive {
                return None;
            }
            // Name this session in the command (the Stop hook knows it; the
            // session itself does not): staking the lead with its session is
            // what lets its own hooks re-attach it after an idle release.
            let commands = waiter_commands(env, &input).unwrap_or_else(|| {
                let helper = quoted_helper(env);
                format!("Bash: {helper} wait \u{2014} PowerShell: & {helper} wait")
            });
            Some(
                json!({
                    "decision": "block",
                    "reason": format!(
                        "{} Mnemo worker(s) are still running and no waiter is active. \
                         Start this as a background command so you are woken when one reports. {commands}",
                        reply.active_workers,
                    ),
                })
                .to_string(),
            )
        }
    }
}

/// `gate-agent`: the PreToolUse JSON when the agent call needs the user's OK.
pub fn gate_agent(env: &Env, stdin_text: &str) -> Option<String> {
    let input = parse_input(stdin_text);
    let tool_name = input.get("tool_name").and_then(Value::as_str).unwrap_or("");
    let subagent_type = input
        .pointer("/tool_input/subagent_type")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| match tool_name {
            // An agent call without a type runs the general-purpose agent.
            "" | "Agent" | "Task" => "general-purpose".to_string(),
            other => other.to_string(),
        });
    let instance = find_instance_for_hook(env, &hook_cwd(env, &input))?;
    let reply = call(
        &instance,
        "/lead/gate-agent",
        &json!({ "root": instance.root, "subagent_type": subagent_type }),
        HOOK_TIMEOUT,
    )?;
    if reply.get("decision").and_then(Value::as_str) != Some("ask") {
        return None;
    }
    let reason = reply
        .get("reason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty())
        .unwrap_or("Mnemo's workers should do this work; Claude Code's own agents need your OK.");
    Some(
        json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "ask",
                "permissionDecisionReason": reason,
            }
        })
        .to_string(),
    )
}

// ---- hint --pre-search -----------------------------------------------------

/// The names Claude Code lists the plugin's Mnemo tools under (the skills use
/// the short ones). A hint names the full one so `ToolSearch` can load it.
const MCP_TOOL_PREFIX: &str = "mcp__plugin_mnemo_mnemo__";

/// Where Mnemo keeps the project's knowledge records.
const KNOWLEDGE_DIR: &str = ".coding/knowledge";

/// The typed titles Mnemo writes on knowledge records: a search for one of
/// them asks about project history, which `memory_search` answers better.
const KNOWLEDGE_PREFIXES: [&str; 6] = ["SPEC:", "DECISION:", "BUG:", "HOW:", "PLAN:", "REVIEW:"];

/// The declaration keywords that turn a name into a symbol search (`fn X`).
const DECLARATION_KEYWORDS: [&str; 14] = [
    "fn", "struct", "enum", "trait", "impl", "type", "mod", "const", "static", "def", "class",
    "func", "function", "interface",
];

/// Where a hinted search came from: the hint names the call it rides on, and a
/// shell search must not be told that a Grep tool call still runs.
#[derive(Clone, Copy)]
enum Source {
    /// A Grep or Glob tool call.
    Tool,
    /// A `grep`, `rg`, `git grep`, `findstr` or `Select-String` in a shell.
    Shell,
}

/// The hint a search earns: which Mnemo tool answers it better, and the call to
/// make.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Hint {
    /// A code identifier: the code graph and the content index know it.
    Code(String),
    /// A project-knowledge lookup: the memory store knows it, with the query
    /// to ask when the pattern names one.
    Memory(Option<String>),
}

impl Hint {
    /// The `additionalContext` text. It never says the search was wrong: the
    /// call always runs, and the hint only names the better tool and how to
    /// load it.
    fn context(&self, source: Source) -> String {
        // The search it rides on always runs; name it the way the caller made
        // it, so a shell search is told its own command still runs.
        let still = match source {
            Source::Tool => "Grep still runs.",
            Source::Shell => "The shell command still runs.",
        };
        match self {
            Hint::Code(query) => format!(
                "Mnemo answers this from its code graph: graph_search {{\"query\": \"{query}\"}} \
                 (load with ToolSearch select:{MCP_TOOL_PREFIX}graph_search) or \
                 search {{\"pattern\": \"{query}\"}} (select:{MCP_TOOL_PREFIX}search). \
                 {still}"
            ),
            Hint::Memory(Some(query)) => format!(
                "Mnemo keeps this project's decisions, bugs and how-tos: memory_search \
                 {{\"query\": \"{query}\"}} (load with ToolSearch \
                 select:{MCP_TOOL_PREFIX}memory_search). {still}"
            ),
            Hint::Memory(None) => format!(
                "Mnemo keeps this project's decisions, bugs and how-tos in {KNOWLEDGE_DIR}/: \
                 memory_search (load with ToolSearch select:{MCP_TOOL_PREFIX}memory_search) \
                 recalls them. {still}"
            ),
        }
    }
}

/// True when `text` asks for a typed knowledge record (`DECISION: …`).
fn is_knowledge(text: &str) -> bool {
    let upper = text.to_uppercase();
    KNOWLEDGE_PREFIXES.iter().any(|prefix| upper.starts_with(prefix))
}

/// True when `text` points into Mnemo's knowledge folder.
fn is_knowledge_path(text: &str) -> bool {
    text.replace('\\', "/").to_lowercase().contains(KNOWLEDGE_DIR)
}

/// What to ask `memory_search` for: the words after a typed prefix, when they
/// are words and not a path or a glob.
fn knowledge_query(pattern: &str) -> Option<String> {
    let mut rest = pattern.trim();
    if is_knowledge(rest) {
        rest = rest.split_once(':').map(|(_, after)| after).unwrap_or("").trim();
    }
    let usable = !rest.is_empty()
        && !is_knowledge_path(rest)
        && !rest.chars().any(|c| {
            matches!(
                c,
                '/' | '\\' | '*' | '?' | '"' | '^' | '$' | '|' | '(' | ')' | '[' | ']'
            )
        });
    usable.then(|| rest.to_string())
}

/// True when a bare token looks like a code identifier rather than a word:
/// snake_case (`attach_lead`) or CamelCase (`TeamManager`, `HTTPClient`). A
/// single lower-case word ("cache") stays with Grep.
fn is_identifier(token: &str) -> bool {
    if token.chars().filter(|c| c.is_alphanumeric()).count() < 2 {
        return false;
    }
    token.contains('_')
        || token
            .chars()
            .zip(token.chars().skip(1))
            .any(|(lower, upper)| lower.is_lowercase() && upper.is_uppercase())
}

/// The name to look up for a single-token pattern: `Type::member` asks for
/// `member`, and a bare word only counts when it looks like an identifier.
fn single_name(token: &str) -> Option<String> {
    if let Some((_, member)) = token.rsplit_once("::") {
        return (!member.is_empty()).then(|| member.to_string());
    }
    is_identifier(token).then(|| token.to_string())
}

/// True when `word` is a declaration keyword (`fn X`, `struct X`).
fn is_declaration_keyword(word: &str) -> bool {
    DECLARATION_KEYWORDS.contains(&word.to_ascii_lowercase().as_str())
}

/// The hint one search earns, from the pattern it looks for and the path it
/// searches: `None` when the pattern is ordinary text, a regex, a path or a
/// file glob — exactly what Grep and Glob are for.
fn classify(pattern: &str, path: &str) -> Option<Hint> {
    let pattern = pattern.trim();
    // A typed title (`DECISION: …`) or a search inside the knowledge folder
    // asks about project history, whether the pattern or the path says so.
    if is_knowledge(pattern) || is_knowledge_path(pattern) || is_knowledge_path(path) {
        return Some(Hint::Memory(knowledge_query(pattern)));
    }
    // `X()` and `X(` name a call: the parentheses are not part of the name.
    let text = pattern.trim_end_matches("()").trim_end_matches('(');
    // Regexes and paths cannot name a symbol, and Mnemo's search takes plain
    // patterns, not regular expressions.
    if !text
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | ' '))
    {
        return None;
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    match words.as_slice() {
        [word] => Some(Hint::Code(single_name(word)?)),
        [keyword, word] if is_declaration_keyword(keyword) && is_identifier(word) => {
            Some(Hint::Code((*word).to_string()))
        }
        // Plain prose ("how does eviction work") stays with Grep.
        _ => None,
    }
}

/// The hint one tool call earns and where the search came from; `None` for
/// every other tool, and for a shell command that is not a confident file
/// search.
///
/// Grep and Glob carry the pattern and the path in their own input. A Bash or
/// PowerShell call is read out of its command line instead: a `grep`, `rg`,
/// `git grep`, `findstr` or `Select-String` that searches files earns the same
/// hint, and one that only filters another command's output stays silent.
fn search_hint(input: &Value) -> Option<(Hint, Source)> {
    let tool_input = input.get("tool_input").filter(|value| value.is_object())?;
    match input.get("tool_name").and_then(Value::as_str)? {
        "Grep" | "Glob" => {
            let pattern = tool_input.get("pattern").and_then(Value::as_str).unwrap_or("");
            let path = tool_input.get("path").and_then(Value::as_str).unwrap_or("");
            classify(pattern, path).map(|hint| (hint, Source::Tool))
        }
        "Bash" | "PowerShell" => {
            let command = tool_input.get("command").and_then(Value::as_str)?;
            let (pattern, path) = shell_search(command)?;
            classify(&pattern, path.as_deref().unwrap_or("")).map(|hint| (hint, Source::Shell))
        }
        _ => None,
    }
}

// ---- shell searches --------------------------------------------------------

/// One command in a shell command line: its words with the quoting removed, and
/// whether a single pipe (`|`) fed it — a command fed by a pipe is an output
/// filter, not a file search.
struct ShellCommand {
    words: Vec<String>,
    piped: bool,
}

/// Push the word built so far onto `words` and clear it. `started` marks that a
/// word was begun, so a quoted empty word (`rg ""`) still counts.
fn end_word(word: &mut String, words: &mut Vec<String>, started: &mut bool) {
    if *started || !word.is_empty() {
        words.push(std::mem::take(word));
    }
    *started = false;
}

/// Split a shell command line into its commands, with the quoting and the
/// escapes taken out of the words. A command a single `|` fed is marked `piped`;
/// `||` starts one on the failure of the last, so it is not a pipe.
///
/// This reads a command line, it does not understand one: whatever it cannot
/// split confidently leaves words the caller then declines to read.
fn shell_commands(command: &str) -> Vec<ShellCommand> {
    let mut commands = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut piped = false;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' => {
                started = true;
                for inner in chars.by_ref() {
                    if inner == c {
                        break;
                    }
                    word.push(inner);
                }
            }
            // A backslash only escapes whitespace, quotes and itself, so a
            // Windows path keeps its separators (`src\*.rs`).
            '\\' => {
                started = true;
                match chars.peek() {
                    Some(next) if next.is_whitespace() || matches!(next, '\'' | '"' | '\\') => {
                        word.push(*next);
                        chars.next();
                    }
                    _ => word.push('\\'),
                }
            }
            c if c.is_whitespace() => end_word(&mut word, &mut words, &mut started),
            '|' => {
                end_word(&mut word, &mut words, &mut started);
                let single = chars.peek() != Some(&'|');
                if !single {
                    chars.next();
                }
                commands.push(ShellCommand { words: std::mem::take(&mut words), piped });
                piped = single;
            }
            ';' | '\n' | '&' => {
                end_word(&mut word, &mut words, &mut started);
                if c == '&' && chars.peek() == Some(&'&') {
                    chars.next();
                }
                commands.push(ShellCommand { words: std::mem::take(&mut words), piped });
                piped = false;
            }
            other => {
                started = true;
                word.push(other);
            }
        }
    }
    end_word(&mut word, &mut words, &mut started);
    if !words.is_empty() {
        commands.push(ShellCommand { words, piped });
    }
    commands
}

/// The search a shell command line makes, when it makes a confident file
/// search: the pattern it looks for and, when given, the path it searches.
/// Anything else — an output filter, a command this parser cannot read — is
/// `None`, so the hint stays silent rather than guessing.
fn shell_search(command: &str) -> Option<(String, Option<String>)> {
    shell_commands(command)
        .into_iter()
        .filter(|command| !command.piped)
        .find_map(|command| command_search(&command.words))
}

/// The search one shell command runs, when it is a `grep`, `rg`, `git grep`,
/// `findstr` or `Select-String` that reads files.
fn command_search(words: &[String]) -> Option<(String, Option<String>)> {
    let (name, args) = words.split_first()?;
    match command_name(name).as_str() {
        "grep" | "egrep" | "fgrep" => grep_search(args, GrepKind::Grep),
        "rg" => grep_search(args, GrepKind::Ripgrep),
        "findstr" => findstr_search(args),
        "select-string" | "sls" => select_string_search(args),
        "git" => match args.split_first() {
            Some((sub, rest)) if command_name(sub) == "grep" => {
                grep_search(rest, GrepKind::GitGrep)
            }
            _ => None,
        },
        _ => None,
    }
}

/// The command a word names: its base name in lower case, without a Windows
/// `.exe` suffix, so `/usr/bin/grep` and `grep.exe` both read as `grep`.
fn command_name(word: &str) -> String {
    let base = word.rsplit(['/', '\\']).next().unwrap_or(word).to_ascii_lowercase();
    base.strip_suffix(".exe").unwrap_or(base.as_str()).to_string()
}

/// Which grep flavour a command runs: they differ in when they search files
/// rather than the text piped into them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GrepKind {
    /// `grep` reads stdin unless it recurses or is given a file.
    Grep,
    /// `rg` searches files by default.
    Ripgrep,
    /// `git grep` always searches the working tree or the index.
    GitGrep,
}

/// The grep options that take the next word as their value, so that value is
/// never mistaken for the pattern (`-g X`, `-t rust`, `-A 3`).
const GREP_VALUE_FLAGS: [&str; 26] = [
    "-m",
    "-A",
    "-B",
    "-C",
    "-d",
    "-g",
    "-t",
    "-T",
    "-j",
    "-M",
    "--max-count",
    "--after-context",
    "--before-context",
    "--context",
    "--include",
    "--exclude",
    "--exclude-dir",
    "--glob",
    "--type",
    "--type-not",
    "--max-depth",
    "--threads",
    "--color",
    "--colour",
    "--colors",
    "--encoding",
];

/// The grep options that put the pattern somewhere else (`-e PATTERN`,
/// `-f FILE`), so the words after them cannot be read as the pattern.
const GREP_PATTERN_FLAGS: [&str; 4] = ["-e", "-f", "--regexp", "--file"];

/// True when a short grep flag bundles `-r` or `-R` (`-rn`, `-rni`). Long
/// options are checked by name, because `--regexp` has an `r` too.
fn is_recursive_short_flag(arg: &str) -> bool {
    !arg.starts_with("--") && arg.chars().skip(1).any(|c| matches!(c, 'r' | 'R'))
}

/// The search one grep command runs: its pattern and, when given, its first
/// path. `None` when the pattern cannot be read confidently, or when grep
/// reads stdin rather than files.
fn grep_search(args: &[String], kind: GrepKind) -> Option<(String, Option<String>)> {
    let mut pattern: Option<String> = None;
    let mut paths: Vec<String> = Vec::new();
    let mut recursive = false;
    let mut options_done = false;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        index += 1;
        if !options_done && arg == "--" {
            options_done = true;
            continue;
        }
        if !options_done && arg.starts_with('-') && arg.len() > 1 {
            let name = arg.split('=').next().unwrap_or(arg);
            // `-r` replaces in rg but recurses in grep, and `-e`/`-f` move the
            // pattern out of reach: none of that can be read confidently.
            if GREP_PATTERN_FLAGS.contains(&name)
                || (kind == GrepKind::Ripgrep && (arg == "-r" || arg.starts_with("--replace")))
            {
                return None;
            }
            recursive |= arg.starts_with("--recursive")
                || arg.starts_with("--dereference-recursive")
                || is_recursive_short_flag(arg);
            let lower = name.to_ascii_lowercase();
            if !arg.contains('=') && GREP_VALUE_FLAGS.contains(&lower.as_str()) {
                index += 1; // the flag's value is not the pattern
            }
            continue;
        }
        if pattern.is_none() {
            pattern = Some(arg.clone());
        } else {
            paths.push(arg.clone());
        }
    }
    // grep reads what is piped into it unless it recurses or is given a file;
    // rg and git grep search files on their own.
    let searches_files = kind != GrepKind::Grep || recursive || !paths.is_empty();
    if !searches_files {
        return None;
    }
    Some((pattern?, paths.first().cloned()))
}

/// The `findstr` search: it reads stdin unless it is given files, and its
/// `/C`, `/F` and `/G` switches keep the pattern or the file list inside the
/// switch.
fn findstr_search(args: &[String]) -> Option<(String, Option<String>)> {
    let mut pattern: Option<String> = None;
    let mut paths: Vec<String> = Vec::new();
    for arg in args {
        if let Some(switch) = arg.strip_prefix('/') {
            if matches!(switch.chars().next(), Some('C' | 'F' | 'G')) {
                return None;
            }
            continue;
        }
        if pattern.is_none() {
            pattern = Some(arg.clone());
        } else {
            paths.push(arg.clone());
        }
    }
    Some((pattern?, Some(paths.first()?.clone())))
}

/// The `Select-String` parameters that take the word after them.
const SELECT_STRING_VALUE_PARAMS: [&str; 8] = [
    "pattern",
    "path",
    "literalpath",
    "include",
    "exclude",
    "context",
    "encoding",
    "inputobject",
];

/// The `Select-String` parameters that are switches of their own.
const SELECT_STRING_SWITCHES: [&str; 7] = [
    "casesensitive",
    "notmatch",
    "simplematch",
    "allmatches",
    "list",
    "quiet",
    "raw",
];

/// The `Select-String` search: it reads the pipeline unless it is given a path,
/// so the pattern is the `-Pattern` value (or the first positional word) and
/// the path is its `-Path` value (or the next positional word).
fn select_string_search(args: &[String]) -> Option<(String, Option<String>)> {
    let mut pattern: Option<String> = None;
    let mut paths: Vec<String> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        index += 1;
        // A positional word: the pattern first, then the paths.
        let Some(parameter) = arg.strip_prefix('-') else {
            if pattern.is_none() {
                pattern = Some(arg.clone());
            } else {
                paths.push(arg.clone());
            }
            continue;
        };
        // `-Pattern Foo` and `-Pattern:Foo` spell the same parameter.
        let (name, inline) = match parameter.split_once(':') {
            Some((name, value)) => (name.to_ascii_lowercase(), Some(value.to_string())),
            None => (parameter.to_ascii_lowercase(), None),
        };
        if SELECT_STRING_SWITCHES.contains(&name.as_str()) {
            continue;
        }
        if !SELECT_STRING_VALUE_PARAMS.contains(&name.as_str()) {
            // An unknown parameter may take the next word with it, so nothing
            // after it can be read confidently.
            return None;
        }
        let value = match inline {
            Some(value) => value,
            None => {
                let value = args.get(index)?.clone();
                index += 1;
                value
            }
        };
        match name.as_str() {
            "pattern" => pattern = Some(value),
            "path" | "literalpath" => paths.push(value),
            _ => {}
        }
    }
    Some((pattern?, Some(paths.first()?.clone())))
}

/// `hint --pre-search`: the PreToolUse context that points a Grep, Glob or
/// shell file search at the Mnemo tool that answers it better, when there is
/// one.
///
/// Silent outside a registered project, for ordinary text, regexes and paths,
/// for a shell command that only filters another command's output, and for
/// every other tool. It never blocks and never sets a permission decision: the
/// search it rides on always runs.
pub fn hint(env: &Env, stdin_text: &str) -> Option<String> {
    let input = parse_input(stdin_text);
    let (found, source) = search_hint(&input)?;
    // Registration is the whole check: no `/health` probe, so a hint costs one
    // file read and never delays the search it rides on.
    registered_root(env, &hook_cwd(env, &input)).ok().flatten()?;
    Some(hook_output("PreToolUse", &found.context(source)))
}

/// The instance `wait` should listen to: the process cwd first, then
/// `CLAUDE_PROJECT_DIR`.
fn wait_instance(env: &Env) -> Option<Instance> {
    find_instance(env, &env.process_cwd).or_else(|| {
        env.project_dir
            .as_ref()
            .and_then(|dir| find_instance(env, dir))
    })
}

/// The notices in a `/lead/wait` reply.
fn reply_notices(reply: &Value) -> Vec<String> {
    reply
        .get("notices")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|notice| notice.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// What `wait` prints for a `/lead/wait` reply. Notices win; otherwise the
/// server's `status` decides, and a reply without one reads as before.
fn describe_wait_reply(reply: &Value, timeout_s: u64) -> String {
    let notices = reply_notices(reply);
    if !notices.is_empty() {
        return notices.join("\n");
    }
    match reply.get("status").and_then(Value::as_str) {
        Some("detached") => DETACHED.to_string(),
        Some("no_project") => NO_PROJECT.to_string(),
        Some("timeout") => format!("no events after {timeout_s} s"),
        _ => "no events".to_string(),
    }
}

/// `wait [--timeout <s>] [--session <id>]`: block until a notice is queued;
/// the text to print.
///
/// When no instance is found, or it stops answering, this keeps looking every
/// `wait_poll` for up to `wait_reach_timeout` (the instance may be restarting)
/// and then says plainly that Mnemo2 is unreachable, instead of exiting at
/// once and having the lead start another waiter, again and again.
///
/// `--session` names the Claude Code session the Stop hook suggested this
/// command for (the session itself cannot know it): staking the lead with
/// that name is what lets the session's own hooks re-attach it after an
/// idle release.
pub fn wait(env: &Env, timeout_s: u64, session: Option<&str>) -> String {
    let read_timeout = Duration::from_secs(timeout_s).saturating_add(WAIT_SLACK);
    let session = session.map(str::trim).filter(|session| !session.is_empty());
    let mut unreachable_since: Option<Instant> = None;
    loop {
        let reply = wait_instance(env).and_then(|instance| {
            let mut body = root_body(&instance);
            body["timeout_s"] = json!(timeout_s);
            if let Some(session) = session {
                body["session"] = json!(session);
            }
            call(&instance, "/lead/wait", &body, read_timeout)
        });
        if let Some(reply) = reply {
            return describe_wait_reply(&reply, timeout_s);
        }
        let since = *unreachable_since.get_or_insert_with(Instant::now);
        if since.elapsed() >= env.wait_reach_timeout {
            return NOT_REACHABLE.to_string();
        }
        std::thread::sleep(env.wait_poll);
    }
}
#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
