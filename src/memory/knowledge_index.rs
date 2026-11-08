// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! A small generated map of the knowledge corpus (plan 373dec00, step 6).
//!
//! `.coding/knowledge/INDEX.md` is a *generated* file: one section per area,
//! each line a typed title plus the record's path relative to the knowledge
//! root. It is never a record itself — the corpus scan only reads
//! `<type>/*.md` — and nothing here opens the memory store.
//!
//! Two consumers:
//!
//! - a human or an agent browsing `.coding/knowledge/INDEX.md` by area, and
//! - the external lead's SessionStart context, which gets [`knowledge_map`] —
//!   the same areas with their counts, in under [`MAP_MAX_BYTES`].
//!
//! Areas come from the record's citations using the check pass's rule
//! ([`health::CITATION_PATTERN`]): `src-tauri/src/<x>/…` is `src-tauri/<x>`,
//! a file directly under `src-tauri/src` is `src-tauri`, `mnemo2/src/<x>/…` is
//! `mnemo2/<x>`, and every other path keeps its first two segments (or its
//! first when it has fewer). A record that cites no repo path is `general`;
//! the smallest areas fold into `general` too, so the file stays at ten
//! sections no matter how many areas the corpus grows.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::memory::health;
use crate::memory::knowledge::{self, KnowledgeRecord, KnowledgeStatus};

/// The generated index's file name, inside the knowledge dir.
pub const INDEX_FILE_NAME: &str = "INDEX.md";

/// The area for a record that cites no repo path — and the bucket every area
/// beyond the largest [`AREA_COUNT`] folds into.
pub const GENERAL_AREA: &str = "general";

/// How many lines a section lists before its `+N more` line.
const SECTION_CAP: usize = 15;

/// How many named areas the index keeps; the rest fold into [`GENERAL_AREA`].
const AREA_COUNT: usize = 9;

/// The SessionStart map's byte budget (plan 373dec00 step 6).
pub const MAP_MAX_BYTES: usize = 1_500;

/// The map's opening words (the area list follows on the same line).
const MAP_LEAD: &str = "Project knowledge";

/// The sentence that tells a lead how to read the list.
const MAP_TAIL: &str =
    "memory_search finds them; .coding/knowledge/INDEX.md lists them by area.";

/// Render the whole index from parsed knowledge records.
///
/// Pure: the caller has already read the corpus (this is what makes the
/// "rewrite only when the bytes differ" check cheap to test). Superseded
/// records are history and never listed. Sections are ordered by record count
/// (descending, ties by name) with [`GENERAL_AREA`] last, and each section
/// lists its newest [`SECTION_CAP`] records (by `created`, then path).
pub fn render_index(records: &[KnowledgeRecord]) -> String {
    let citation_re =
        Regex::new(health::CITATION_PATTERN).expect("the citation pattern is a fixed regex");
    let url_re = Regex::new(health::URL_PATTERN).expect("the URL pattern is a fixed regex");

    let mut by_area: BTreeMap<String, Vec<&KnowledgeRecord>> = BTreeMap::new();
    for record in records {
        if record.status == KnowledgeStatus::Superseded {
            continue;
        }
        by_area
            .entry(record_area(record, &citation_re, &url_re))
            .or_default()
            .push(record);
    }

    // The nine largest named areas keep their own section; everything else —
    // records that cite nothing, and the smallest areas — is `general`.
    let mut general = by_area.remove(GENERAL_AREA).unwrap_or_default();
    let mut named: Vec<(String, Vec<&KnowledgeRecord>)> = by_area.into_iter().collect();
    named.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    while named.len() > AREA_COUNT {
        let (_, folded) = named.pop().expect("named is longer than AREA_COUNT");
        general.extend(folded);
    }

    let mut out = String::from("# Project knowledge index\n\n");
    out.push_str("Generated from the knowledge files. Do not edit. Search with memory_search.\n");
    for (area, list) in named {
        out.push('\n');
        out.push_str(&render_section(&area, list));
    }
    if !general.is_empty() {
        out.push('\n');
        out.push_str(&render_section(GENERAL_AREA, general));
    }
    out
}

/// One `## <area> (<count>)` section: newest [`SECTION_CAP`] lines, then
/// `+N more` when the section is longer.
fn render_section(area: &str, mut list: Vec<&KnowledgeRecord>) -> String {
    list.sort_by(|a, b| {
        b.created
            .as_deref()
            .unwrap_or("")
            .cmp(a.created.as_deref().unwrap_or(""))
            .then_with(|| a.rel_path.cmp(&b.rel_path))
    });
    let mut out = format!("## {area} ({})\n", list.len());
    let shown = list.len().min(SECTION_CAP);
    for record in &list[..shown] {
        out.push_str("- ");
        out.push_str(&record.typed_title());
        out.push_str(" - ");
        out.push_str(&record.rel_path);
        out.push('\n');
    }
    if list.len() > shown {
        out.push_str(&format!("- +{} more\n", list.len() - shown));
    }
    out
}

/// Regenerate `<knowledge_dir>/INDEX.md` from the corpus on disk.
///
/// Writes through a temp file plus rename, and only when the rendered bytes
/// differ from what is already there — an unchanged corpus touches nothing,
/// so the pass is safe to run on every knowledge write and every project open.
/// Returns whether the file was written.
pub fn regenerate(knowledge_dir: &Path) -> std::io::Result<bool> {
    let records: Vec<KnowledgeRecord> = health::read_corpus(knowledge_dir)
        .into_iter()
        .map(|(_, record)| record)
        .collect();
    let rendered = render_index(&records);
    let path = knowledge_dir.join(INDEX_FILE_NAME);
    if let Ok(current) = std::fs::read_to_string(&path) {
        if current == rendered {
            return Ok(false);
        }
    }
    std::fs::create_dir_all(knowledge_dir)?;
    let temp = knowledge_dir.join(format!("{INDEX_FILE_NAME}.tmp"));
    std::fs::write(&temp, rendered.as_bytes())?;
    std::fs::rename(&temp, &path)?;
    Ok(true)
}

/// [`regenerate`] on the blocking pool, for the indexer's and the check pass's
/// async callers. A failure is one line on stderr and never fails the caller:
/// the index is a convenience, the records are the truth.
pub async fn regenerate_blocking(knowledge_dir: PathBuf) {
    let dir = knowledge_dir.clone();
    let result = tokio::task::spawn_blocking(move || regenerate(&dir)).await;
    match result {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => eprintln!(
            "memory: cannot regenerate {}: {error}",
            knowledge_dir.join(INDEX_FILE_NAME).display()
        ),
        Err(error) => eprintln!("memory: the knowledge index task failed: {error}"),
    }
}

/// The SessionStart map for one project: `None` when INDEX.md is missing or
/// carries no sections.
pub fn knowledge_map_at(project_root: &Path) -> Option<String> {
    let path = project_root
        .join(".coding")
        .join(knowledge::KNOWLEDGE_DIR_NAME)
        .join(INDEX_FILE_NAME);
    let text = std::fs::read_to_string(path).ok()?;
    knowledge_map(&text)
}

/// The one-line knowledge map an external lead gets at session start: the
/// INDEX.md sections and their counts, under [`MAP_MAX_BYTES`] bytes.
///
/// The smallest areas are dropped until it fits; `None` when the file has no
/// `## ` sections (a missing corpus, or a file from another tool).
pub fn knowledge_map(index_md: &str) -> Option<String> {
    let mut areas: Vec<(String, usize)> = Vec::new();
    for line in index_md.lines() {
        let Some(rest) = line.strip_prefix("## ") else {
            continue;
        };
        let Some((name, count)) = rest.rsplit_once(" (") else {
            continue;
        };
        let Some(count) = count.strip_suffix(')').and_then(|n| n.parse::<usize>().ok()) else {
            continue;
        };
        let name = name.trim();
        if !name.is_empty() {
            areas.push((name.to_string(), count));
        }
    }
    if areas.is_empty() {
        return None;
    }
    // The total is the whole corpus, even when an area is dropped below.
    let total: usize = areas.iter().map(|(_, count)| count).sum();
    loop {
        let text = map_text(total, &areas);
        if text.len() <= MAP_MAX_BYTES || areas.len() <= 1 {
            return Some(text);
        }
        // Drop the smallest area (the later one on a tie, keeping file order).
        let smallest = areas
            .iter()
            .enumerate()
            .min_by_key(|(index, (_, count))| (*count, std::cmp::Reverse(*index)))
            .map(|(index, _)| index)
            .expect("areas is not empty");
        areas.remove(smallest);
    }
}

/// The map's text for one area list.
fn map_text(total: usize, areas: &[(String, usize)]) -> String {
    let list = areas
        .iter()
        .map(|(name, count)| format!("{name} {count}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{MAP_LEAD} ({total} records): {list}. {MAP_TAIL}")
}

/// The area a cited repo path belongs to.
fn area_for_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    match parts.as_slice() {
        // `src-tauri/src/<x>/…` groups by module; a file directly under
        // `src-tauri/src` keeps the crate root as its area.
        ["src-tauri", "src", _] => "src-tauri".to_string(),
        ["src-tauri", "src", area, _, ..] => format!("src-tauri/{area}"),
        // Same shape for a repo-root `mnemo2/` tree (the plugin and its app).
        ["mnemo2", "src", area, _, ..] => format!("mnemo2/{area}"),
        _ if parts.len() >= 3 => format!("{}/{}", parts[0], parts[1]),
        [first, ..] => (*first).to_string(),
        [] => GENERAL_AREA.to_string(),
    }
}

/// The area one record belongs to: its most-cited area, ties by name; a record
/// that cites no repo path is [`GENERAL_AREA`].
fn record_area(record: &KnowledgeRecord, citation_re: &Regex, url_re: &Regex) -> String {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for path in health::extract_citations(citation_re, url_re, &record.body) {
        *counts.entry(area_for_path(&path)).or_default() += 1;
    }
    // BTreeMap iterates in name order, so a strictly-greater test keeps the
    // alphabetically first area on a tie.
    let mut best: Option<(String, usize)> = None;
    for (area, count) in counts {
        let wins = match &best {
            None => true,
            Some((_, best_count)) => count > *best_count,
        };
        if wins {
            best = Some((area, count));
        }
    }
    best.map(|(area, _)| area)
        .unwrap_or_else(|| GENERAL_AREA.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record with the fields the index reads. `created` defaults to a date
    /// derived from the slug so sorting is deterministic in tests.
    fn record(record_type: MemoryRecordType, slug: &str, title: &str, body: &str) -> KnowledgeRecord {
        KnowledgeRecord {
            record_type,
            slug: slug.to_string(),
            rel_path: format!("{}/{slug}.md", knowledge::dir_for_record_type(record_type).unwrap()),
            title: title.to_string(),
            status: KnowledgeStatus::Live,
            supersedes: None,
            created: Some("2026-01-01".to_string()),
            evidence: None,
            body: body.to_string(),
            links: Vec::new(),
        }
    }

    use crate::memory::MemoryRecordType;

    #[test]
    fn the_index_groups_by_area_and_stays_under_its_cap() {
        // Eleven named areas, 40 records each: the nine largest keep a
        // section, the rest fold into `general`, and every section is capped.
        let mut records = Vec::new();
        for area in 0..11 {
            for i in 0..40 {
                let body = format!("cites src/area{area}/file{i}.rs");
                records.push(record(
                    MemoryRecordType::How,
                    &format!("how-{area}-{i}"),
                    &format!("HOW: thing {area}-{i}"),
                    &body,
                ));
            }
        }
        let index = render_index(&records);
        let lines: Vec<&str> = index.lines().collect();
        assert!(lines.len() < 200, "index is {} lines", lines.len());
        let headings: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|line| line.starts_with("## "))
            .collect();
        assert_eq!(headings.len(), 10, "nine areas plus general: {headings:?}");
        assert!(
            headings.last().unwrap().starts_with("## general ("),
            "general is last and holds the folded areas: {headings:?}"
        );
        for heading in &headings {
            let section_start = lines
                .iter()
                .position(|line| line == heading)
                .expect("heading is a line");
            let section_len = lines[section_start + 1..]
                .iter()
                .take_while(|line| !line.starts_with("## "))
                .filter(|line| line.starts_with("- "))
                .count();
            assert!(section_len <= SECTION_CAP + 1, "{heading} lists {section_len}");
        }
        // Two areas folded: general holds 2 * 40 = 80 records.
        assert!(headings.last().unwrap().contains("(80)"), "{headings:?}");
        assert!(index.contains("- +65 more"), "a capped section says how many were cut");
        assert!(!index.contains("## src/area9"), "the smallest areas folded");
    }

    #[test]
    fn index_excludes_superseded_records() {
        let mut live = record(MemoryRecordType::Spec, "live", "SPEC: live", "cites src/agent/a.rs");
        let old = record(MemoryRecordType::Spec, "old", "SPEC: old", "cites src/agent/a.rs");
        live.created = Some("2026-02-01".to_string());
        let mut old = old;
        old.status = KnowledgeStatus::Superseded;
        let index = render_index(&[live, old]);
        assert!(index.contains("SPEC: live"));
        assert!(!index.contains("SPEC: old"), "history is not listed");
    }

    #[test]
    fn the_index_is_not_rewritten_when_unchanged() {
        let dir = temp_dir("unchanged");
        std::fs::create_dir_all(dir.join("spec")).unwrap();
        std::fs::write(
            dir.join("spec/2026-01-01-one.md"),
            "+++
title = \"SPEC: one\"
created = \"2026-01-01\"
+++

cites src/agent/one.rs
",
        )
        .unwrap();
        assert!(regenerate(&dir).unwrap(), "the first pass writes");
        let path = dir.join(INDEX_FILE_NAME);
        let stamp = std::fs::metadata(&path).unwrap().modified().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(!regenerate(&dir).unwrap(), "a settled pass writes nothing");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), stamp);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_map_drops_small_areas_to_fit_its_budget() {
        // Ten areas, the smallest one padded with a name long enough to blow
        // the budget: it is dropped and the rest survive.
        let mut index = String::from("# Project knowledge index\n");
        for i in 0..9 {
            index.push_str(&format!("## src/area{i} ({})\n", 100 + i));
        }
        let long = "x".repeat(MAP_MAX_BYTES);
        index.push_str(&format!("## {long} (1)\n"));
        let map = knowledge_map(&index).expect("a map");
        assert!(map.len() <= MAP_MAX_BYTES, "map is {} bytes", map.len());
        assert!(map.contains("src/area8 108"), "the big areas survive: {map}");
        assert!(map.contains("(937 records)"), "the total counts the dropped area: {map}");
    }

    #[test]
    fn session_start_context_stays_under_its_budget() {
        let index = "# Project knowledge index\n\n## src/agent (92)\n- SPEC: a - spec/a.md\n\n## general (550)\n";
        let map = knowledge_map(index).expect("a map");
        assert!(
            map.starts_with("Project knowledge (642 records): src/agent 92, general 550. "),
            "{map}"
        );
        assert!(map.contains("memory_search finds them; .coding/knowledge/INDEX.md lists them by area."));
        assert!(map.len() <= MAP_MAX_BYTES);
        assert!(knowledge_map("# Project knowledge index\n").is_none(), "no sections, no map");
    }

    #[test]
    fn areas_follow_the_citation_rule() {
        assert_eq!(area_for_path("src-tauri/src/mnemo2/team.rs"), "src-tauri/mnemo2");
        assert_eq!(area_for_path("src-tauri/src/main.rs"), "src-tauri");
        assert_eq!(area_for_path("mnemo2/src/plugin/lib.rs"), "mnemo2/plugin");
        assert_eq!(area_for_path("mnemo2/src/main.rs"), "mnemo2/src");
        assert_eq!(area_for_path("src/agent/turn.rs"), "src/agent");
        assert_eq!(area_for_path("tests/eval.rs"), "tests");
        assert_eq!(area_for_path("frontend/src/app.ts"), "frontend/src");
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mnemo-knowledge-index-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }
}
