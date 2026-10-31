// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Tests for the one-time "run Mnemo without prompts" offer. Every path (Claude
//! Code's config dir, the Mnemo config dir holding the marker, the project) is
//! a temp dir; nothing reads the real `~/.claude`.

use super::*;
use crate::testutil::Sandbox;

const WINDOWS_HELPER: &str = r"C:\Users\Me\AppData\Local\Mnemo\bin\mnemo2-mcp.exe";

/// Claude Code's user config dir inside the sandbox.
fn claude_dir_of(sandbox: &Sandbox) -> PathBuf {
    sandbox.dir.path().join("claude-home")
}

fn offer(sandbox: &Sandbox) -> Offer {
    Offer::new(&sandbox.env(), Some(claude_dir_of(sandbox)))
}

fn marker(sandbox: &Sandbox) -> PathBuf {
    sandbox
        .config_dir
        .join("claude-lead")
        .join("permissions-offered")
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn allow_settings(rules: &[&str]) -> String {
    json!({ "permissions": { "allow": rules } }).to_string()
}

/// A tool result as Mnemo2 would answer it.
fn result_line(blocks: &[&str], is_error: bool) -> String {
    let content: Vec<Value> = blocks
        .iter()
        .map(|text| json!({ "type": "text", "text": text }))
        .collect();
    json!({ "jsonrpc": "2.0", "id": 7, "result": { "content": content, "isError": is_error } })
        .to_string()
}

fn content_of(line: &str) -> Vec<Value> {
    let message: Value = serde_json::from_str(line).expect("a JSON line");
    message["result"]["content"]
        .as_array()
        .expect("a content array")
        .clone()
}

fn note_of(line: &str) -> Option<String> {
    content_of(line)
        .iter()
        .filter_map(|block| block["text"].as_str())
        .find(|text| text.starts_with("[Mnemo setup, shown once]"))
        .map(str::to_string)
}

/// The JSON array of rules inside the note.
fn rules_in(note: &str) -> Vec<String> {
    let start = note
        .find("[\"mcp__plugin_mnemo_mnemo__*\"")
        .expect("the rules array");
    let mut values = serde_json::Deserializer::from_str(&note[start..]).into_iter::<Vec<String>>();
    values
        .next()
        .expect("a value")
        .expect("a JSON array of strings")
}

// ---- offering once ----------------------------------------------------------

#[test]
fn the_first_tool_result_gets_the_note_and_the_marker_and_nothing_follows() {
    let sandbox = Sandbox::new();
    let first = offer(&sandbox);
    let reply = result_line(&["done"], false);

    let offered = first.apply(reply.clone(), &sandbox.project);
    assert!(note_of(&offered).is_some(), "{offered}");
    let stamp = std::fs::read_to_string(marker(&sandbox)).expect("the marker is written");
    let stamp = stamp.trim();
    assert_eq!(stamp.len(), "2026-01-01T00:00:00Z".len(), "{stamp}");
    assert!(stamp.starts_with("20") && stamp.ends_with('Z'), "{stamp}");

    // Never twice in the same process ...
    let again = result_line(&["next"], false);
    assert_eq!(first.apply(again.clone(), &sandbox.project), again);
    // ... nor in a new one, now that the marker is there.
    let later = offer(&sandbox);
    assert_eq!(later.apply(again.clone(), &sandbox.project), again);
}

#[test]
fn the_note_is_one_more_block_and_the_rest_of_the_reply_is_untouched() {
    let sandbox = Sandbox::new();
    let reply = result_line(&["first", "second"], false);
    let offered = offer(&sandbox).apply(reply.clone(), &sandbox.project);

    let original: Value = serde_json::from_str(&reply).unwrap();
    let mut changed: Value = serde_json::from_str(&offered).unwrap();
    let blocks = changed["result"]["content"].as_array_mut().unwrap();
    assert_eq!(blocks.len(), 3);
    let note = blocks.pop().unwrap();
    assert_eq!(note["type"], "text");
    assert!(note["text"]
        .as_str()
        .unwrap()
        .starts_with("[Mnemo setup, shown once]"));
    assert_eq!(changed, original, "everything else is as it was");
}

#[test]
fn a_failed_tool_call_does_not_carry_or_use_up_the_offer() {
    let sandbox = Sandbox::new();
    let offer = offer(&sandbox);
    let failed = result_line(&["boom"], true);
    assert_eq!(offer.apply(failed.clone(), &sandbox.project), failed);
    // The next call that worked gets it.
    let worked = offer.apply(result_line(&["ok"], false), &sandbox.project);
    assert!(worked.contains("[Mnemo setup, shown once]"), "{worked}");
}

#[test]
fn the_note_carries_the_three_rules_as_a_json_array_and_names_the_settings_file() {
    let sandbox = Sandbox::new();
    let mut env = sandbox.env();
    env.helper = PathBuf::from(WINDOWS_HELPER);
    let offered = Offer::new(&env, Some(claude_dir_of(&sandbox)))
        .apply(result_line(&["done"], false), &sandbox.project);

    let note = note_of(&offered).expect("offered");
    assert_eq!(
        rules_in(&note),
        vec![
            "mcp__plugin_mnemo_mnemo__*".to_string(),
            format!("Bash(\"{WINDOWS_HELPER}\" wait *)"),
            format!("PowerShell(& \"{WINDOWS_HELPER}\" wait *)"),
        ]
    );
    // The backslashes and quotes are escaped the way the settings file needs.
    assert!(note.contains(&format!(
        "Bash(\\\"{}\\\" wait *)",
        WINDOWS_HELPER.replace('\\', "\\\\")
    )));
    let settings = claude_dir_of(&sandbox).join("settings.json");
    assert!(
        note.contains(&format!("array in {} (create the file", settings.display())),
        "{note}"
    );
    assert!(note.contains("show the user the entries before writing"));
    assert!(note.contains("If they decline, do not ask again"));
    assert!(note.contains("which still ask."));
    // The reply as a whole is still one JSON line.
    assert!(!offered.contains('\n'));
}

#[test]
fn a_reply_that_cannot_carry_the_note_does_not_use_the_offer_up() {
    let sandbox = Sandbox::new();
    let offer = offer(&sandbox);
    let unfit = [
        // A JSON-RPC error.
        json!({ "jsonrpc": "2.0", "id": 7, "error": { "code": -32000, "message": "no" } })
            .to_string(),
        // A result without content, and one whose content is not an array.
        json!({ "jsonrpc": "2.0", "id": 7, "result": {} }).to_string(),
        json!({ "jsonrpc": "2.0", "id": 7, "result": { "content": "text" } }).to_string(),
        // Not an object result, not JSON at all.
        json!({ "jsonrpc": "2.0", "id": 7, "result": null }).to_string(),
        "not json".to_string(),
    ];
    for reply in unfit {
        assert_eq!(offer.apply(reply.clone(), &sandbox.project), reply);
        assert!(!marker(&sandbox).exists(), "{reply}");
    }
    // The next result that fits still gets it.
    let offered = offer.apply(result_line(&["done"], false), &sandbox.project);
    assert!(note_of(&offered).is_some());
    assert!(marker(&sandbox).exists());
}

// ---- when Mnemo is already allowed -------------------------------------------

#[test]
fn settings_that_already_allow_mnemo_silence_the_offer() {
    // (file, rule) pairs: every place Claude Code reads, and the server-wide
    // rule as well as a narrower one.
    let cases: [(&str, &str); 4] = [
        ("user/settings.json", "mcp__plugin_mnemo_mnemo__*"),
        (
            "user/settings.local.json",
            "mcp__plugin_mnemo_mnemo__team_status",
        ),
        ("project/settings.json", "mcp__plugin_mnemo_mnemo__*"),
        ("project/settings.local.json", "mcp__plugin_mnemo_mnemo"),
    ];
    for (file, rule) in cases {
        let sandbox = Sandbox::new();
        let path = match file.split_once('/').unwrap() {
            ("user", name) => claude_dir_of(&sandbox).join(name),
            (_, name) => sandbox.project.join(".claude").join(name),
        };
        write(&path, &allow_settings(&["Read", rule]));

        let reply = result_line(&["done"], false);
        assert_eq!(
            offer(&sandbox).apply(reply.clone(), &sandbox.project),
            reply,
            "{file}"
        );
        assert!(
            !marker(&sandbox).exists(),
            "{file}: nothing was offered, so no marker"
        );
    }
}

#[test]
fn the_projects_settings_count_only_for_that_project() {
    let sandbox = Sandbox::new();
    write(
        &sandbox.project.join(".claude").join("settings.local.json"),
        &allow_settings(&["mcp__plugin_mnemo_mnemo__*"]),
    );
    let reply = result_line(&["done"], false);
    let elsewhere = offer(&sandbox).apply(reply.clone(), &sandbox.elsewhere);
    assert!(
        note_of(&elsewhere).is_some(),
        "another project's rules do not apply"
    );
}

#[test]
fn look_alike_settings_do_not_count_as_allowing_mnemo() {
    let sandbox = Sandbox::new();
    let settings = json!({
        "permissions": {
            "allow": ["Bash(git status)", "mcp__plugin_other_other__*", "mcp__mnemo__*", 7, null],
            "deny": ["mcp__plugin_mnemo_mnemo__*"],
            "ask": ["mcp__plugin_mnemo_mnemo__team_merge"],
        },
        "allow": ["mcp__plugin_mnemo_mnemo__*"],
    });
    write(
        &claude_dir_of(&sandbox).join("settings.json"),
        &settings.to_string(),
    );
    let offered = offer(&sandbox).apply(result_line(&["done"], false), &sandbox.project);
    assert!(note_of(&offered).is_some());
}

#[test]
fn unreadable_or_invalid_settings_are_ignored() {
    let sandbox = Sandbox::new();
    write(&claude_dir_of(&sandbox).join("settings.json"), "{ nope");
    write(&claude_dir_of(&sandbox).join("settings.local.json"), "");
    // A directory where a file should be cannot be read either.
    std::fs::create_dir_all(sandbox.project.join(".claude").join("settings.json")).unwrap();
    write(
        &sandbox.project.join(".claude").join("settings.local.json"),
        "[1, 2",
    );
    let offered = offer(&sandbox).apply(result_line(&["done"], false), &sandbox.project);
    assert!(note_of(&offered).is_some());
}

#[test]
fn a_byte_order_mark_in_front_of_the_settings_is_fine() {
    let sandbox = Sandbox::new();
    write(
        &claude_dir_of(&sandbox).join("settings.json"),
        &format!(
            "\u{feff}{}",
            allow_settings(&["mcp__plugin_mnemo_mnemo__*"])
        ),
    );
    let reply = result_line(&["done"], false);
    assert_eq!(
        offer(&sandbox).apply(reply.clone(), &sandbox.project),
        reply
    );
}

// ---- never failing the call ---------------------------------------------------

#[test]
fn without_a_known_claude_dir_there_is_no_offer() {
    let sandbox = Sandbox::new();
    let reply = result_line(&["done"], false);
    let offer = Offer::new(&sandbox.env(), None);
    assert_eq!(offer.apply(reply.clone(), &sandbox.project), reply);
    assert!(!marker(&sandbox).exists());
}

#[test]
fn a_helper_path_that_cannot_be_quoted_in_a_rule_is_not_offered() {
    for helper in [
        "C:\\bad\"dir\\mnemo2-mcp.exe",
        "/opt/$HOME/mnemo2-mcp",
        "/opt/a*b/mnemo2-mcp",
        "",
    ] {
        let sandbox = Sandbox::new();
        let mut env = sandbox.env();
        env.helper = PathBuf::from(helper);
        let reply = result_line(&["done"], false);
        let offer = Offer::new(&env, Some(claude_dir_of(&sandbox)));
        assert_eq!(
            offer.apply(reply.clone(), &sandbox.project),
            reply,
            "{helper:?}"
        );
        assert!(!marker(&sandbox).exists());
    }
}

#[test]
fn a_marker_that_cannot_be_written_skips_the_offer_without_failing_or_retrying() {
    let sandbox = Sandbox::new();
    // A file where the marker's directory should be.
    write(&sandbox.config_dir.join("claude-lead"), "in the way");
    let offer = offer(&sandbox);
    let reply = result_line(&["done"], false);
    assert_eq!(offer.apply(reply.clone(), &sandbox.project), reply);
    // Closed for the rest of the process, even once the obstacle is gone.
    std::fs::remove_file(sandbox.config_dir.join("claude-lead")).unwrap();
    assert_eq!(offer.apply(reply.clone(), &sandbox.project), reply);
    assert!(!marker(&sandbox).exists());
}

#[test]
fn the_marker_is_claimed_by_one_process_only() {
    let sandbox = Sandbox::new();
    let first = offer(&sandbox);
    let second = offer(&sandbox);
    assert!(first.claim_marker().unwrap());
    assert!(!second.claim_marker().unwrap(), "the marker exists already");
    // A process that lost the race for the marker stays quiet.
    let reply = result_line(&["done"], false);
    let raced = Offer {
        state: Mutex::new(State::Pending("note".to_string())),
        ..offer(&sandbox)
    };
    assert_eq!(raced.apply(reply.clone(), &sandbox.project), reply);
}

#[test]
fn concurrent_tool_results_share_one_offer() {
    let sandbox = Sandbox::new();
    let offer = offer(&sandbox);
    let offered: usize = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    note_of(&offer.apply(result_line(&["done"], false), &sandbox.project)).is_some()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| usize::from(handle.join().unwrap()))
            .sum()
    });
    assert_eq!(offered, 1);
}

// ---- where Claude Code keeps its settings --------------------------------------

#[test]
fn claude_config_dir_wins_over_the_home_dir() {
    let dir = tempfile::tempdir().unwrap();
    let custom = dir.path().join("custom");
    let home = dir.path().join("home");
    assert_eq!(
        claude_dir(Some(custom.clone().into_os_string()), Some(home.clone())),
        Some(custom)
    );
    assert_eq!(
        claude_dir(None, Some(home.clone())),
        Some(home.join(".claude"))
    );
    // An empty variable counts as unset.
    assert_eq!(
        claude_dir(Some(OsString::new()), Some(home.clone())),
        Some(home.join(".claude"))
    );
}

#[test]
fn a_claude_dir_that_is_not_absolute_or_not_known_is_none() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(claude_dir(None, None), None);
    assert_eq!(
        claude_dir(
            Some(OsString::from("relative")),
            Some(dir.path().to_path_buf())
        ),
        None
    );
    assert_eq!(claude_dir(None, Some(PathBuf::from("home"))), None);
}

#[test]
fn the_four_settings_files_are_the_users_two_and_the_projects_two() {
    let files = settings_files(Path::new("/c"), Path::new("/p"));
    let names: Vec<PathBuf> = files.to_vec();
    assert_eq!(
        names,
        vec![
            Path::new("/c").join("settings.json"),
            Path::new("/c").join("settings.local.json"),
            Path::new("/p").join(".claude").join("settings.json"),
            Path::new("/p").join(".claude").join("settings.local.json"),
        ]
    );
}

#[test]
fn the_marker_time_is_iso_utc() {
    assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
    assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00Z");
    assert_eq!(iso_utc(1_700_000_000), "2023-11-14T22:13:20Z");
    assert_eq!(iso_utc(4_102_444_799), "2099-12-31T23:59:59Z");
}
