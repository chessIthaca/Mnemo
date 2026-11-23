// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! A Claude Code agent's transcript as Mission Control's worker messages.
//!
//! The window shows a worker's `Transcript` (user and assistant text, tool
//! calls with their input, tool results). A worker that runs as a Claude Code
//! agent has none of its own: its log is the `agent-<id>.jsonl` file Claude
//! Code writes. This module converts that file into the same message shape,
//! read-only, so the snapshot can serve it in place of the worker's thread.
//!
//! Records of type `user` and `assistant` are read; their content blocks map
//! to text, tool calls (`tool_use` with its input) and tool results. Thinking
//! blocks, other record types and malformed lines are skipped. A tool result
//! longer than [`RESULT_CAP`] characters is cut, so one huge output cannot
//! swamp the window. The file is found only through
//! [`find_transcript`], which confines it to `~/.claude/projects`.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use mnemo::headless::Transcript;
use mnemo::provider::{Message, ToolCall};
use serde_json::Value;

use super::agent_watch::{find_transcript, AgentWatch};
use super::record::WorkerRecord;

/// The longest tool result kept, in characters.
const RESULT_CAP: usize = 4000;

/// How much of the end of a long transcript is converted.
const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;

/// What a conversion was made from: the file's length and modification time.
type Stamp = (u64, u128);

type Converted = HashMap<PathBuf, (Stamp, Arc<Transcript>)>;

/// The last conversion per transcript file, so an unchanged file is neither
/// re-read nor given a new `Arc` (the snapshot's revision follows the `Arc`).
fn cache() -> &'static Mutex<Converted> {
    static CACHE: OnceLock<Mutex<Converted>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn clip(text: &str) -> String {
    if text.chars().count() <= RESULT_CAP {
        return text.to_string();
    }
    let kept: String = text.chars().take(RESULT_CAP).collect();
    format!("{kept}\n... [truncated]")
}

/// The text of a tool result's `content`: a string, or text blocks.
fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Convert Claude Code JSONL into worker messages.
pub(super) fn convert(jsonl: &str) -> Vec<Message> {
    let mut messages = Vec::new();
    // Tool names by call id, so a result can say which tool produced it.
    let mut names: HashMap<String, String> = HashMap::new();
    for line in jsonl.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let kind = record.get("type").and_then(Value::as_str);
        let Some(content) = record.get("message").and_then(|m| m.get("content")) else {
            continue;
        };
        match (kind, content) {
            (Some("user"), Value::String(text)) if !text.is_empty() => {
                messages.push(Message::user_text(text.clone()));
            }
            (Some("assistant"), Value::String(text)) if !text.is_empty() => {
                messages.push(Message::assistant_text(text.clone()));
            }
            (Some("user"), Value::Array(blocks)) => {
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                messages.push(Message::user_text(text.to_string()));
                            }
                        }
                        Some("tool_result") => {
                            let id = block
                                .get("tool_use_id")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            let name = names.get(id).cloned().unwrap_or_default();
                            let text = clip(&result_text(block.get("content")));
                            let mut message = Message::tool_result(id, name, text);
                            message.tool_is_error =
                                block.get("is_error").and_then(Value::as_bool) == Some(true);
                            messages.push(message);
                        }
                        _ => {}
                    }
                }
            }
            (Some("assistant"), Value::Array(blocks)) => {
                let mut text = String::new();
                let mut calls = Vec::new();
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(part) = block.get("text").and_then(Value::as_str) {
                                if !text.is_empty() {
                                    text.push_str("\n\n");
                                }
                                text.push_str(part);
                            }
                        }
                        Some("tool_use") => {
                            let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
                            let name =
                                block.get("name").and_then(Value::as_str).unwrap_or_default();
                            let input = block.get("input").cloned().unwrap_or(Value::Null);
                            names.insert(id.to_string(), name.to_string());
                            calls.push(ToolCall::new(id, name, input.to_string()));
                        }
                        _ => {}
                    }
                }
                if !text.is_empty() || !calls.is_empty() {
                    messages.push(Message::assistant(text, calls));
                }
            }
            _ => {}
        }
    }
    messages
}

/// The text of the last assistant message in Claude Code JSONL that has any:
/// the agent's final answer when it ended. `None` when no assistant message
/// carries text.
pub(super) fn last_assistant_text(jsonl: &str) -> Option<String> {
    convert(jsonl)
        .into_iter()
        .rev()
        .filter(|message| message.role == mnemo::provider::Role::Assistant)
        .map(|message| message.content.as_text().trim().to_string())
        .find(|text| !text.is_empty())
}

/// [`last_assistant_text`] of a transcript file, read from its tail.
pub(super) fn last_assistant_text_of(path: &Path) -> Option<String> {
    let len = std::fs::metadata(path).ok()?.len();
    last_assistant_text(&read_tail(path, len)?)
}

/// Read the tail of a transcript file as text, dropping a cut first line.
fn read_tail(path: &Path, len: u64) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let start = len.saturating_sub(MAX_LOG_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_LOG_BYTES).read_to_end(&mut bytes).ok()?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0 {
        match text.find('\n') {
            Some(at) => {
                text.drain(..=at);
            }
            None => return Some(String::new()),
        }
    }
    Some(text)
}

/// A registered agent worker's transcript as worker messages: `None` for a
/// worker without a registered agent, a transcript not found yet, or one
/// that holds nothing to show.
pub(super) fn agent_transcript(
    watch: &AgentWatch,
    worker: &WorkerRecord,
) -> Option<Arc<Transcript>> {
    let agent_id = worker.claude_agent_id.as_deref()?;
    let found = find_transcript(watch, agent_id, worker.claude_agent_transcript.as_deref());
    let path = PathBuf::from(found.path?);
    let meta = std::fs::metadata(&path).ok()?;
    let stamp: Stamp = (
        meta.len(),
        meta.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis()),
    );
    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some((seen, transcript)) = cache.get(&path) {
        if *seen == stamp {
            return (!transcript.messages.is_empty()).then(|| Arc::clone(transcript));
        }
    }
    let messages = convert(&read_tail(&path, meta.len())?);
    let transcript = Arc::new(Transcript { messages });
    cache.insert(path, (stamp, Arc::clone(&transcript)));
    (!transcript.messages.is_empty()).then_some(transcript)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mnemo::provider::Role;

    const FIXTURE: &str = r#"{"type":"user","message":{"role":"user","content":"Build the thing"}}
not json at all
{"type":"summary","summary":"skipped"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"Reading the code."},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"a.rs"}}]}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"fn main() {}"}]}]}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","content":"boom","is_error":true}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Done."}]}}
"#;

    #[test]
    fn converts_claude_records_to_worker_messages() {
        let messages = convert(FIXTURE);
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[0].content.as_text(), "Build the thing");
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(messages[1].content.as_text(), "Reading the code.");
        assert_eq!(messages[1].tool_calls.len(), 1);
        assert_eq!(messages[1].tool_calls[0].name, "Read");
        assert_eq!(messages[1].tool_calls[0].arguments, r#"{"file_path":"a.rs"}"#);
        assert_eq!(messages[2].role, Role::Tool);
        assert_eq!(messages[2].tool_call_id.as_deref(), Some("t1"));
        assert_eq!(messages[2].name.as_deref(), Some("Read"));
        assert_eq!(messages[2].content.as_text(), "fn main() {}");
        assert!(!messages[2].tool_is_error);
        assert!(messages[3].tool_is_error);
        assert_eq!(messages[4].content.as_text(), "Done.");
    }

    #[test]
    fn the_last_assistant_text_is_the_final_answer() {
        assert_eq!(last_assistant_text(FIXTURE).as_deref(), Some("Done."));
        let only_tools = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t","name":"Bash","input":{}}]}}"#;
        assert_eq!(last_assistant_text(only_tools), None);
        assert_eq!(last_assistant_text(""), None);
    }

    #[test]
    fn long_tool_results_are_cut() {
        let big = "x".repeat(RESULT_CAP * 2);
        let line = format!(
            r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"t","content":"{big}"}}]}}}}"#
        );
        let messages = convert(&line);
        let text = messages[0].content.as_text();
        assert!(text.chars().count() < RESULT_CAP + 40);
        assert!(text.ends_with("[truncated]"));
    }

    #[test]
    fn a_short_file_is_read_whole() {
        let dir = std::env::temp_dir().join(format!("agentlog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.jsonl");
        std::fs::write(&path, FIXTURE).unwrap();
        let len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(read_tail(&path, len).unwrap(), FIXTURE);
        std::fs::remove_dir_all(&dir).ok();
    }
}
