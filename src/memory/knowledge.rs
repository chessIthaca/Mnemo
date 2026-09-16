// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Knowledge records — typed semantic facts as markdown files on disk.
//!
//! A knowledge record is one file under `.coding/knowledge/<type>/`:
//!
//! ```text
//! .coding/knowledge/
//!   spec/2026-08-23-branch-strategy.md
//!   decision/2026-08-23-typed-records-as-files.md
//!   bug/2026-08-19-os-error-5-directory-read.md
//!   how/release-checklist.md
//! ```
//!
//! Format: optional TOML front matter between `+++` fences, then an
//! unbounded markdown body:
//!
//! ```text
//! +++
//! title = "Typed records as files"
//! status = "live"                      # live | superseded
//! supersedes = "2026-08-23-merge-bundles"
//! created = "2026-08-23"
//! +++
//!
//! Body — the complete record of truth. Wiki-links ([[decision/other]],
//! [[plan/<id>]], [[review/<stem>]], repo-relative file paths) reference
//! related records instead of copying information into the memory.
//! ```
//!
//! Design invariants (mirroring the derived-record indexer):
//!
//! - **Files are the truth; the DB is an index.** Every record parses to a
//!   stable source key (`knowledge:<type>/<slug>`) whose UUIDv5 becomes the
//!   derived memory id — two worktrees that merged the same file converge on
//!   the same memory row with no data movement.
//! - **Graceful degradation.** A missing or unparseable front-matter block
//!   never fails the parse: the title falls back to the first heading (or
//!   first non-empty line) and `status` defaults to live. One bad file must
//!   never abort an index run.
//! - **Pointer-first.** The file body is unbounded; the derived digest built
//!   from it is budgeted and always carries the file path. `PLAN:` and
//!   `REVIEW:` records have their own file homes (`.coding/plans/`,
//!   `.coding/reviews/`) and are NOT knowledge files.
//! - **Monotonic dates.** New records are stamped `max(wall clock, newest
//!   on-disk artifact date)` (the knowledge dirs plus the sibling
//!   `.coding/reviews/`), so a stale or rolled-back OS clock cannot
//!   back-date exports (the 2026-09-01 incident, backlog d7b2e722).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::memory::types::MemoryRecordType;

/// The knowledge root under `.coding/` (`.coding/knowledge/`).
pub const KNOWLEDGE_DIR_NAME: &str = "knowledge";

/// Maximum chars of the title part of a generated slug (the date prefix and
/// joining hyphen are extra). Keeps filenames a label, not a paragraph.
const SLUG_TITLE_MAX_CHARS: usize = 48;

/// The publication status carried in a knowledge file's front matter.
///
/// `Superseded` mirrors the memory store's supersede-not-delete history: the
/// file stays on disk (readable, diffable history), but the derived memory is
/// excluded from recall by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KnowledgeStatus {
    /// The record is current knowledge (the default).
    Live,
    /// The record was replaced by a successor (see `supersedes` on the
    /// successor's front matter); it is history, not live knowledge.
    Superseded,
}

impl KnowledgeStatus {
    /// Parse a status from its front-matter string form. Unknown values
    /// degrade to [`KnowledgeStatus::Live`] — an unparseable file must never
    /// lose its knowledge.
    pub fn from_str_or_live(s: &str) -> Self {
        match s.trim() {
            "superseded" => Self::Superseded,
            _ => Self::Live,
        }
    }
}

/// The `superseded_by` sentinel for a knowledge record whose front matter
/// says `status = "superseded"` but whose successor file is absent (deleted,
/// or still on another branch). A non-NULL `superseded_by` is what excludes
/// the derived row from recall/listing — the sentinel keeps such history out
/// of live knowledge without inventing a fake successor id.
pub const SUPERSEDED_SENTINEL: &str = "superseded";

/// What a `[[wiki-link]]` points at. The kind is derived from the target's
/// shape so links stay human-typed (no explicit kind syntax).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KnowledgeLinkKind {
    /// Another knowledge record: `spec/<slug>`, `decision/<slug>`,
    /// `bug/<slug>`, or `how/<slug>`.
    Knowledge,
    /// A plan file: `plan/<id>`.
    Plan,
    /// A review report: `review/<stem>`.
    Review,
    /// A repo-relative file path (e.g. `src/memory/mod.rs`) — source code or
    /// any other tracked file.
    File,
    /// Anything else — kept verbatim so unknown shapes survive a round-trip
    /// instead of being dropped.
    Other,
}

/// One `[[target]]` wiki-link extracted from a knowledge file body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeLink {
    /// The link target, verbatim (trimmed).
    pub target: String,
    /// The classified kind.
    pub kind: KnowledgeLinkKind,
}

/// A parsed knowledge record — one file under `.coding/knowledge/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeRecord {
    /// The record type, derived from the subdirectory (`spec/` → `Spec`, …).
    pub record_type: MemoryRecordType,
    /// The file stem — the record's stable, human-readable identity
    /// (`2026-08-23-typed-records-as-files`).
    pub slug: String,
    /// The record path relative to the knowledge root
    /// (`decision/2026-08-23-typed-records-as-files.md`).
    pub rel_path: String,
    /// The bare title from front matter (no typed prefix — the digest adds
    /// `SPEC:` etc.). Falls back to the first heading or content line.
    pub title: String,
    /// Publication status from front matter (default live).
    pub status: KnowledgeStatus,
    /// The slug of the record this one replaces, when set on the successor.
    /// History flows successor → predecessor; the indexer flips it into
    /// `superseded_by` on the predecessor's derived memory.
    pub supersedes: Option<String>,
    /// The creation date from front matter (`YYYY-MM-DD`), when present.
    pub created: Option<String>,
    /// The markdown body after the front matter (unbounded, verbatim).
    pub body: String,
    /// Wiki-links extracted from the body.
    pub links: Vec<KnowledgeLink>,
}

impl KnowledgeRecord {
    /// The stable source key for this record — the same shape as the
    /// indexer's other source families (`plan:<stem>`, `review:<stem>`).
    /// The derived memory id is the UUIDv5 of this key, so two worktrees
    /// holding the same file derive the same memory row.
    pub fn source_key(&self) -> String {
        format!(
            "{}:{}/{}",
            KNOWLEDGE_DIR_NAME,
            self.record_type.as_str(),
            self.slug
        )
    }

    /// The digest title — the typed prefix plus the bare title
    /// (e.g. `DECISION: typed records as files`). Records are constructed
    /// only for knowledge types, so the prefix is always present; the
    /// fallback keeps the method total regardless.
    pub fn typed_title(&self) -> String {
        format!(
            "{} {}",
            title_prefix(self.record_type).unwrap_or("RECORD:"),
            self.title
        )
    }

    /// The pointer line every derived digest carries — the file is the truth,
    /// the digest must never lose the path to it.
    pub fn pointer_line(&self) -> String {
        format!(
            "path .coding/{}/{}/{}.md",
            KNOWLEDGE_DIR_NAME,
            self.record_type.as_str(),
            self.slug
        )
    }
}

/// The typed-title prefix for a knowledge record type
/// (`Spec` → `"SPEC:"`). `None` for types without a knowledge home
/// (`Plan`, `Review`, `None`).
pub fn title_prefix(record_type: MemoryRecordType) -> Option<&'static str> {
    match record_type {
        MemoryRecordType::Spec => Some("SPEC:"),
        MemoryRecordType::Decision => Some("DECISION:"),
        MemoryRecordType::Bug => Some("BUG:"),
        MemoryRecordType::How => Some("HOW:"),
        MemoryRecordType::Plan | MemoryRecordType::Review | MemoryRecordType::None => None,
    }
}

/// Map a knowledge subdirectory name to its record type. Only the four
/// knowledge types map (`spec`/`decision`/`bug`/`how`); `plan`, `review`,
/// and anything else return `None` — those records have their own file homes.
pub fn record_type_for_dir(dir: &str) -> Option<MemoryRecordType> {
    match MemoryRecordType::from_str(dir) {
        Some(t) if dir_for_record_type(t).is_some() => Some(t),
        _ => None,
    }
}

/// Map a record type to its knowledge subdirectory name. `None` for types
/// without a knowledge home (`Plan`, `Review`, `None`).
pub fn dir_for_record_type(record_type: MemoryRecordType) -> Option<&'static str> {
    match record_type {
        MemoryRecordType::Spec
        | MemoryRecordType::Decision
        | MemoryRecordType::Bug
        | MemoryRecordType::How => Some(record_type.as_str()),
        MemoryRecordType::Plan | MemoryRecordType::Review | MemoryRecordType::None => None,
    }
}

/// Build the slug for a new record: `<YYYY-MM-DD>-<kebab-title>`. ASCII
/// letters lowercase, everything else collapses to single hyphens; the title
/// part caps at [`SLUG_TITLE_MAX_CHARS`] chars (char-boundary safe); an empty
/// result (a title with no ASCII letters) falls back to `record`.
pub fn slug_for(title: &str, date: &str) -> String {
    let mut part = String::with_capacity(SLUG_TITLE_MAX_CHARS + 8);
    let mut last_hyphen = true; // suppress leading + collapsed hyphens
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            part.push(ch.to_ascii_lowercase());
            last_hyphen = false;
        } else if !last_hyphen {
            part.push('-');
            last_hyphen = true;
        }
    }
    while part.ends_with('-') {
        part.pop();
    }
    let part: String = part.chars().take(SLUG_TITLE_MAX_CHARS).collect();
    let part = part.trim_end_matches('-');
    if part.is_empty() {
        format!("{date}-record")
    } else {
        format!("{date}-{part}")
    }
}

/// The leading `YYYY-MM-DD` of a slug, when it looks like a date — the
/// fallback record creation date when front matter omits `created`.
pub fn date_from_slug(slug: &str) -> Option<&str> {
    let head = slug.get(0..10)?;
    let bytes = head.as_bytes();
    let shaped = bytes.len() == 10
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit);
    if shaped && slug.len() > 10 && slug.as_bytes()[10] == b'-' {
        Some(head)
    } else {
        None
    }
}

/// Parse one knowledge file. `rel_path` is the path under the knowledge root
/// (`decision/<slug>.md`); the record type comes from its first component.
///
/// Returns `None` when the path is not a knowledge shape (unknown type dir or
/// non-`.md` extension). Never fails on content — front matter that is
/// missing or unparseable degrades to defaults (see the module docs).
pub fn parse_file(rel_path: &str, contents: &str) -> Option<KnowledgeRecord> {
    let (dir, file) = rel_path.split_once('/')?;
    let record_type = record_type_for_dir(dir)?;
    let slug = file.strip_suffix(".md")?.to_string();

    let (front, body) = split_front_matter(contents);
    // Unparseable TOML degrades to defaults — one bad block must never fail
    // the whole record (mirrors the indexer's per-file error containment).
    let front = front
        .as_deref()
        .and_then(|text| toml::from_str::<FrontMatter>(text).ok());
    let title = front
        .as_ref()
        .and_then(|f| {
            let t = f.title.as_deref()?.trim();
            (!t.is_empty()).then(|| t.to_string())
        })
        .unwrap_or_else(|| title_from_body(&body, &slug));
    let status = front
        .as_ref()
        .and_then(|f| f.status.as_deref())
        .map(KnowledgeStatus::from_str_or_live)
        .unwrap_or(KnowledgeStatus::Live);
    let supersedes = front.as_ref().and_then(|f| {
        let s = f.supersedes.as_deref()?.trim();
        (!s.is_empty()).then(|| s.to_string())
    });
    let created = front
        .as_ref()
        .and_then(|f| {
            let c = f.created.as_deref()?.trim();
            (!c.is_empty()).then(|| c.to_string())
        })
        .or_else(|| date_from_slug(&slug).map(str::to_string));

    Some(KnowledgeRecord {
        record_type,
        links: extract_links(&body),
        rel_path: rel_path.to_string(),
        slug,
        title,
        status,
        supersedes,
        created,
        body,
    })
}

/// The front-matter fields a knowledge file may carry. Every field optional;
/// unknown keys are ignored (serde's default) so the format can grow without
/// breaking older parses.
#[derive(Debug, Default, Deserialize)]
struct FrontMatter {
    title: Option<String>,
    status: Option<String>,
    supersedes: Option<String>,
    created: Option<String>,
}

/// Split a file into (optional front-matter TOML text, body). Front matter is
/// a leading `+++` line closed by the NEXT `+++` line (CRLF-tolerant). A file
/// without a leading fence — or with an unclosed one — is all body.
fn split_front_matter(contents: &str) -> (Option<String>, String) {
    let lines: Vec<&str> = contents.lines().collect();
    if lines.first().copied().map(str::trim_end) != Some("+++") {
        return (None, contents.trim().to_string());
    }
    // The closing fence: the first later line that trims to `+++`.
    let Some(close) = lines[1..]
        .iter()
        .position(|l| l.trim_end() == "+++")
        .map(|i| i + 1)
    else {
        // Unclosed fence: treat the whole file as body (graceful degradation).
        return (None, contents.trim().to_string());
    };
    let toml_text = lines[1..close].join("\n");
    let body = lines[close + 1..].join("\n").trim().to_string();
    (Some(toml_text), body)
}

/// The degraded title: the first `#` heading, else the first non-empty
/// non-heading line, else the slug.
fn title_from_body(body: &str, slug: &str) -> String {
    body.lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("# "))
        .map(|h| h.trim().to_string())
        .filter(|t| !t.is_empty())
        .or_else(|| {
            body.lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_string)
        })
        .unwrap_or_else(|| slug.to_string())
}

/// Extract `[[target]]` wiki-links from a text, in order of appearance.
/// Empty and whitespace-only targets are skipped; a lone `[[` without a
/// closing `]]` ends the scan. Links are extracted everywhere in the body —
/// including code blocks — by design (a link in an example is still a link).
pub fn extract_links(text: &str) -> Vec<KnowledgeLink> {
    let mut links = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else { break };
        let target = after[..end].trim();
        if !target.is_empty() {
            links.push(classify_link(target));
        }
        rest = &after[end + 2..];
    }
    links
}

/// Classify a link target by its shape (see [`KnowledgeLinkKind`]).
fn classify_link(target: &str) -> KnowledgeLink {
    let kind = if target.starts_with("plan/") {
        KnowledgeLinkKind::Plan
    } else if target.starts_with("review/") {
        KnowledgeLinkKind::Review
    } else if target.starts_with("spec/")
        || target.starts_with("decision/")
        || target.starts_with("bug/")
        || target.starts_with("how/")
    {
        KnowledgeLinkKind::Knowledge
    } else if target.contains('/') || target.contains('.') {
        KnowledgeLinkKind::File
    } else {
        KnowledgeLinkKind::Other
    };
    KnowledgeLink {
        target: target.to_string(),
        kind,
    }
}

/// Scan one directory for the newest strict `YYYY-MM-DD-<slug>` stem and
/// fold it into `best` when it passes [`valid_floor_date`]. Unreadable dirs
/// and non-matching names contribute nothing.
fn scan_dir_max_date(dir: &std::path::Path, ceiling: &str, best: &mut Option<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for stem in entries.flatten().filter_map(|e| {
        e.path()
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
    }) {
        if let Some(date) = date_from_slug(&stem) {
            if valid_floor_date(date, ceiling) && best.as_deref().map_or(true, |b| date > b) {
                *best = Some(date.to_string());
            }
        }
    }
}

/// A floor candidate is plausible when its calendar components are valid
/// (month 1-12 and day within that month's length, leap-checked — rejects
/// typo'd stems like `2026-13-99` and `2026-02-31`) and it does not exceed
/// the slack ceiling (rejects far-future junk that would pin every export).
fn valid_floor_date(date: &str, ceiling: &str) -> bool {
    let year: u32 = date.get(0..4).and_then(|y| y.parse().ok()).unwrap_or(0);
    let month: u32 = date.get(5..7).and_then(|m| m.parse().ok()).unwrap_or(0);
    let day: u32 = date.get(8..10).and_then(|d| d.parse().ok()).unwrap_or(0);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        // Month out of range (0 or 13+) — parse guaranteed the shape, the
        // value can still be junk.
        _ => 0,
    };
    (1..=max_day).contains(&day) && date <= ceiling
}

// ── File-backed writer ─────────────────────────────────────────────────────

/// Apply a targeted, unambiguous repair to a record body (backlog 488248ce):
/// the in-tool way to fix stray text in a knowledge record — the file tools
/// refuse `.coding/knowledge/**` by design, and a full-body rewrite
/// ([`KnowledgeStore::update`]) is transcription-risky for long records.
///
/// Refuses an empty `find`, a `find` that is absent, a `find` that occurs
/// more than once (the repair must be unambiguous — widen the substring), and
/// a replacement that would leave the body empty. Returns the repaired body.
pub fn replace_unique(body: &str, find: &str, replace_with: &str) -> Result<String> {
    if find.trim().is_empty() {
        return Err(Error::Memory(
            "replace: the find text is empty — pass the exact text to replace".into(),
        ));
    }
    match body.matches(find).count() {
        0 => Err(Error::Memory(format!(
            "replace: the find text {find:?} is not present in the record body — nothing changed"
        ))),
        1 => {
            let repaired = body.replacen(find, replace_with, 1);
            if repaired.trim().is_empty() {
                return Err(Error::Memory(
                    "replace: refused — the replacement would empty the record body".into(),
                ));
            }
            Ok(repaired)
        }
        n => Err(Error::Memory(format!(
            "replace: the find text occurs {n} times — widen it to a unique substring"
        ))),
    }
}

/// Front matter rendered back to TOML for file rewrites. Only the fields
/// that apply are emitted, in a stable order (title, supersedes, created,
/// then status — the status default `live` is omitted since the parser
/// defaults to it).
fn render_front_matter(
    title: &str,
    status: KnowledgeStatus,
    supersedes: Option<&str>,
    created: Option<&str>,
) -> String {
    fn escape(s: &str) -> String {
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', " ")
    }
    let mut out = String::from("+++\n");
    out.push_str(&format!("title = \"{}\"\n", escape(title)));
    if let Some(s) = supersedes {
        out.push_str(&format!("supersedes = \"{}\"\n", escape(s)));
    }
    if let Some(c) = created {
        out.push_str(&format!("created = \"{}\"\n", escape(c)));
    }
    if status == KnowledgeStatus::Superseded {
        out.push_str("status = \"superseded\"\n");
    }
    out.push_str("+++\n");
    out
}

/// The file-backed writer for typed semantic records — the reverse of
/// [`parse_file`]. The memory tools (`memory_write` / `memory_supersede` /
/// `memory_update` / `memory_delete`) drive it when they touch a knowledge
/// record; the finished-plan captures route through it too.
///
/// Files are the truth: a write creates
/// `.coding/knowledge/<type>/<date>-<slug>.md` with front matter + an
/// unbounded body, and the derived memory row comes from the indexer. The
/// writer never touches the DB — the caller re-indexes the changed file(s)
/// afterwards (see
/// [`reindex_knowledge_files`](crate::memory::indexer::reindex_knowledge_files)).
pub struct KnowledgeStore {
    /// `.coding/knowledge/` — resolved from the project root.
    knowledge_dir: PathBuf,
    /// Clock for the `YYYY-MM-DD` created prefix (tests pin it).
    now: Box<dyn Fn() -> i64 + Send + Sync>,
    /// Serializes the read-modify-write of a supersede (two files change in
    /// one operation).
    lock: Mutex<()>,
}

impl KnowledgeStore {
    /// Create a store rooted at the knowledge dir, with the wall clock.
    pub fn new(knowledge_dir: impl Into<PathBuf>) -> Self {
        Self {
            knowledge_dir: knowledge_dir.into(),
            now: Box::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0)
            }),
            lock: Mutex::new(()),
        }
    }

    /// The knowledge root directory (`.coding/knowledge/`).
    pub fn dir(&self) -> &Path {
        &self.knowledge_dir
    }

    /// Create a store with a fixed clock (deterministic tests).
    pub fn with_clock(
        knowledge_dir: impl Into<PathBuf>,
        now: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        Self {
            knowledge_dir: knowledge_dir.into(),
            now: Box::new(now),
            lock: Mutex::new(()),
        }
    }

    /// The front-matter title for a record: the record type's typed prefix
    /// stripped (e.g. `"DECISION: x"` → `"x"`). The derived digest's
    /// [`KnowledgeRecord::typed_title`] adds the prefix back, so a prefixed
    /// title written verbatim would double-prefix ("DECISION: DECISION: x")
    /// and pollute the slug. Titles without the record's own prefix pass
    /// through unchanged (a different record's prefix is left alone — the
    /// file's directory, not the title, is the record's type).
    fn bare_title(title: &str, record_type: MemoryRecordType) -> String {
        match title_prefix(record_type) {
            Some(prefix) => match title.strip_prefix(prefix) {
                Some(rest) => rest.trim_start().to_string(),
                None => title.to_string(),
            },
            None => title.to_string(),
        }
    }

    /// The date stamped on NEW records: `max(wall clock, artifact floor)`.
    ///
    /// The OS wall clock is not always right — on a stale/pinned clock every
    /// export gets back-dated (the 2026-09-01 incident, backlog d7b2e722 /
    /// review LOW 2). The artifact floor (newest on-disk date prefix across
    /// the knowledge dirs and the sibling reviews dir) acts as a monotonic
    /// high-water mark: a new record can never be dated BEFORE the newest
    /// thing already on disk, so a frozen or rolled-back clock cannot
    /// back-date exports. When the clock is ahead of every artifact (the
    /// normal case) it wins and behavior is unchanged.
    fn stamped_date(&self) -> String {
        let wall = fmt_date((self.now)());
        match self.artifact_date_floor() {
            Some(floor) if floor > wall => floor,
            _ => wall,
        }
    }

    /// Newest strict `YYYY-MM-DD` filename prefix across the knowledge type
    /// dirs and the sibling `.coding/reviews/` dir (both sides of the
    /// `.coding` side-car carry dated artifacts). Candidates beyond a slack
    /// ceiling (~13 months past the wall clock) are ignored — a typo'd or
    /// hostile far-future filename must not pin every future export.
    fn artifact_date_floor(&self) -> Option<String> {
        let ceiling = fmt_date((self.now)() + 400 * 86_400);
        let mut best: Option<String> = None;
        for t in [
            MemoryRecordType::Spec,
            MemoryRecordType::Decision,
            MemoryRecordType::Bug,
            MemoryRecordType::How,
        ] {
            if let Some(dir) = dir_for_record_type(t) {
                scan_dir_max_date(&self.knowledge_dir.join(dir), &ceiling, &mut best);
            }
        }
        // The reviews dir is the knowledge dir's sibling under `.coding/`
        // (`.coding/knowledge` ↔ `.coding/reviews`); a missing dir simply
        // contributes nothing.
        if let Some(parent) = self.knowledge_dir.parent() {
            scan_dir_max_date(&parent.join("reviews"), &ceiling, &mut best);
        }
        best
    }

    /// Write a new live knowledge record: `<type>/<date>-<slug>.md`.
    /// `record_type` must be one of the four knowledge types (SPEC /
    /// DECISION / BUG / HOW) — anything else errors. Returns the rel path
    /// (with `.md`), e.g. `decision/2026-08-23-typed-records.md`. The body
    /// is stored verbatim; the front matter carries the title, `created`,
    /// and `live` status. A slug collision (same date + title already on
    /// disk) suffixes `-2`, `-3`, … so a rewrite never clobbers history.
    pub fn write(&self, record_type: MemoryRecordType, title: &str, body: &str) -> Result<String> {
        let dir = dir_for_record_type(record_type).ok_or_else(|| {
            Error::Memory(format!(
                "record type '{record_type}' has no knowledge home (spec/decision/bug/how only)"
            ))
        })?;
        // The front-matter title is BARE (no typed prefix) — `typed_title()`
        // adds the prefix back on the digest side (see `bare_title`).
        let title = Self::bare_title(title, record_type);
        let _guard = self
            .lock
            .lock()
            .map_err(|_| Error::Memory("knowledge lock poisoned".into()))?;
        let date = self.stamped_date();
        let base = slug_for(&title, &date);
        for n in 1u32.. {
            let slug = if n == 1 {
                base.clone()
            } else {
                format!("{base}-{n}")
            };
            let rel = format!("{dir}/{slug}.md");
            if !self.knowledge_dir.join(&rel).exists() {
                let path = self.knowledge_dir.join(&rel);
                std::fs::create_dir_all(path.parent().unwrap())?;
                let file = format!(
                    "{}\n{}\n",
                    render_front_matter(&title, KnowledgeStatus::Live, None, Some(&date)),
                    body
                );
                std::fs::write(path, file)?;
                return Ok(rel);
            }
        }
        unreachable!("the loop always returns or the filesystem is exhausted")
    }

    /// Idempotent write keyed by an explicit slug: create (or overwrite)
    /// `dir/<slug>.md`. Unlike [`Self::write`] this is stable across calls
    /// with the same slug — used by the finish auto-capture, where the plan
    /// id is the record's identity (a re-finish must address the same file,
    /// never create a `-2` duplicate).
    pub fn write_at(
        &self,
        record_type: MemoryRecordType,
        slug: &str,
        title: &str,
        body: &str,
    ) -> Result<String> {
        let dir = dir_for_record_type(record_type).ok_or_else(|| {
            Error::Memory(format!(
                "record type '{record_type}' has no knowledge home (spec/decision/bug/how only)"
            ))
        })?;
        // The front-matter title is BARE (no typed prefix) — see `bare_title`.
        let title = Self::bare_title(title, record_type);
        let _guard = self
            .lock
            .lock()
            .map_err(|_| Error::Memory("knowledge lock poisoned".into()))?;
        let rel = format!("{dir}/{slug}.md");
        let path = self.knowledge_dir.join(&rel);
        std::fs::create_dir_all(path.parent().unwrap())?;
        let created = date_from_slug(slug)
            .map(str::to_string)
            .or_else(|| Some(self.stamped_date()));
        let file = format!(
            "{}\n{}\n",
            render_front_matter(&title, KnowledgeStatus::Live, None, created.as_deref()),
            body
        );
        std::fs::write(path, file)?;
        Ok(rel)
    }

    /// Update a record's file in place (title and/or body). The slug (the
    /// record's identity) is preserved even when the title changes — the
    /// front-matter title moves, the file name does not. Returns the rel
    /// path (unchanged).
    ///
    /// Digest-shape guard (backlog 63f882c9): a body that is strictly
    /// shorter than the current one AND ends in a knowledge-path pointer
    /// line is a row digest, not a replacement body — it is REFUSED (the
    /// file is the truth; the derived row re-indexes from it). See
    /// [`Self::looks_like_row_digest`].
    pub fn update(
        &self,
        rel_path: &str,
        new_title: Option<&str>,
        new_body: Option<&str>,
    ) -> Result<String> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| Error::Memory("knowledge lock poisoned".into()))?;
        let path = self.knowledge_dir.join(rel_path);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::Memory(format!("knowledge update: {}: {e}", rel_path)))?;
        let record = parse_file(rel_path, &text).ok_or_else(|| {
            Error::Memory(format!(
                "knowledge update: {}: not a knowledge record",
                rel_path
            ))
        })?;
        // Digest-shape guard (backlog 63f882c9): a row digest — condensed,
        // pointer-first — must never replace the file's body: the detail
        // the pointer points at would be destroyed (2027-01-09 incident:
        // a full spec collapsed to a one-paragraph digest).
        if let Some(body) = new_body {
            if Self::looks_like_row_digest(body, &record.body) {
                return Err(Error::Memory(format!(
                    "refused: the new content looks like a row digest (shorter than the file \
                     body + a knowledge-path pointer tail) — .coding/knowledge/{rel_path} is \
                     the truth and carries the full detail; pass the full corrected body to \
                     replace it, or append a dated amendment paragraph — the derived row \
                     re-indexes from the file either way"
                )));
            }
        }
        // A caller-supplied title is stored BARE (no typed prefix) — the
        // record's own type is authoritative (see `bare_title`).
        let title = match new_title {
            Some(t) => Self::bare_title(t, record.record_type),
            None => record.title.clone(),
        };
        let body = new_body.unwrap_or(&record.body);
        let file = format!(
            "{}\n{}\n",
            render_front_matter(
                &title,
                record.status,
                record.supersedes.as_deref(),
                record.created.as_deref()
            ),
            body
        );
        std::fs::write(path, file)?;
        Ok(rel_path.to_string())
    }

    /// Repair stray text in a record body in place (backlog 488248ce): the
    /// unique-substring rule of [`replace_unique`] is enforced, then the file
    /// is rewritten with its front matter re-rendered exactly as
    /// [`Self::update`] does. Returns the number of replacements (always 1).
    ///
    /// Body-only — `find` is matched against the record body, never the
    /// machine-rendered front matter. Refuses unknown rel paths, non-records
    /// and superseded records (history is never edited, as in [`Self::amend`]).
    ///
    /// Deliberately not behind [`Self::looks_like_row_digest`], the guard
    /// [`Self::update`] consults: that guard stops a condensed row digest from
    /// REPLACING a body, while a unique-substring replacement cannot do so
    /// *silently* — the caller must spell out the exact text it removes (a
    /// paraphrase matches nothing) — and a repair that SHRINKS a record
    /// (collapsing a doubled amendment heading, the 2027-01-14 incident) would
    /// otherwise be refused whenever the body ends in a `.coding/knowledge/…md`
    /// pointer tail.
    pub fn replace_in_body(&self, rel_path: &str, find: &str, replace_with: &str) -> Result<u64> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| Error::Memory("knowledge lock poisoned".into()))?;
        let path = self.knowledge_dir.join(rel_path);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::Memory(format!("knowledge replace: {}: {e}", rel_path)))?;
        let record = parse_file(rel_path, &text).ok_or_else(|| {
            Error::Memory(format!(
                "knowledge replace: {}: not a knowledge record",
                rel_path
            ))
        })?;
        if record.status == KnowledgeStatus::Superseded {
            return Err(Error::Memory(format!(
                "knowledge replace: {}: record is superseded — history is never edited",
                rel_path
            )));
        }
        let body = replace_unique(&record.body, find, replace_with)?;
        let file = format!(
            "{}\n{}\n",
            render_front_matter(
                &record.title,
                record.status,
                record.supersedes.as_deref(),
                record.created.as_deref()
            ),
            body.trim_end()
        );
        std::fs::write(&path, file)
            .map_err(|e| Error::Memory(format!("knowledge replace: {}: {e}", rel_path)))?;
        Ok(1)
    }

    /// One stripping step for [`Self::strip_self_headings`]: the paragraph
    /// with one leading amendment heading removed (every line kept), or
    /// `None` when it does not open with a stamp-shaped heading.
    fn leading_self_heading(paragraph: &str) -> Option<&str> {
        let line = paragraph.lines().next()?;
        let bytes = line.as_bytes();
        let word_len = if bytes.len() >= 7 && bytes[..7].eq_ignore_ascii_case(b"amended") {
            7
        } else if bytes.len() >= 9 && bytes[..9].eq_ignore_ascii_case(b"amendment") {
            9
        } else {
            return None;
        };
        // Word boundary — "amendedness"/"amendmental" are not the stamp word.
        match line[word_len..].chars().next() {
            Some(' ' | '\t' | '(' | ':') => {}
            _ => return None,
        }
        // The head ends at the heading's colon — but a colon INSIDE a
        // parenthesized attribution group ("(review L1: the message was
        // wrong)") does not close it, and a heading may carry no colon at all
        // (the text follows on the next line). So try every colon on the line,
        // then the whole line itself, and strip at the first candidate whose
        // head reads as a stamp. Since the line starts at offset 0, an index
        // into it is also an index into the paragraph.
        let candidates = line
            .match_indices(':')
            .map(|(i, _)| i)
            .filter(|i| *i >= word_len)
            .chain(std::iter::once(line.len()));
        for colon in candidates {
            let head = line[word_len..colon].trim();
            if head.is_empty() || head.chars().count() > 120 || !Self::stamp_shaped(head) {
                continue;
            }
            let remainder = if colon == line.len() {
                // The whole line was the heading: the remainder starts on the
                // next line, past its newline.
                let rest = &paragraph[line.len()..];
                rest.strip_prefix('\n').unwrap_or(rest)
            } else {
                &paragraph[colon + 1..]
            };
            return Some(remainder);
        }
        None
    }

    /// Whether `head` — the text between the `amended`/`amendment` word and the
    /// heading's colon — reads as a self-supplied stamp rather than prose.
    /// Stamp shapes: a parenthesized attribution group ("(plan 987fef4c)"), or
    /// a leading four-digit year with a date-shaped tail ("2027-01-14",
    /// "2027-01-14 (plan 987fef4c, backlog 56168c38)").
    fn stamp_shaped(head: &str) -> bool {
        if head.starts_with('(') {
            return head.ends_with(')');
        }
        if head.len() < 4 || !head.as_bytes()[..4].iter().all(u8::is_ascii_digit) {
            return false;
        }
        // Anything between the year and a closing attribution group may only be
        // the rest of the date (the group's own content is free-form).
        let date_char =
            |b: u8| b.is_ascii_digit() || matches!(b, b'-' | b'/' | b'.' | b':' | b',' | b' ');
        let tail = head[4..].trim();
        match tail.find('(') {
            Some(open) => tail[open..].ends_with(')') && tail[..open].bytes().all(date_char),
            None => tail.bytes().all(date_char),
        }
    }

    /// Strip a leading self-supplied amendment heading from `paragraph` and
    /// return the text plus the number of headings removed (backlog 488248ce).
    ///
    /// [`Self::amend`] prefixes its own dated `Amended <date>: ` heading, so a
    /// caller-supplied one would double it — and its date could contradict the
    /// stamp the tool wrote. A heading is recognized only on the FIRST LINE,
    /// and only in stamp shapes: `amended`/`amendment` (any case, at a word
    /// boundary) followed by a head of at most 120 chars that is either a
    /// parenthesized group (`Amendment (plan 987fef4c): …`) or a leading
    /// four-digit year with a date-shaped tail (`2027-01-14: …`,
    /// `2027-01-14 (plan 987fef4c, backlog 56168c38): …`). The head ends at
    /// the first colon yielding such a shape — a colon inside the group
    /// (`(review L1: the message was wrong)`) cannot end it — and a first line
    /// that is all heading with no colon at all is stripped likewise, its text
    /// taken from the following line. Stripping repeats until the text no
    /// longer opens with a heading, so `Amended A: Amended B: text` collapses
    /// to `text`; every other line is preserved. Prose that merely opens with
    /// the word is returned verbatim, and a heading with no text after it
    /// strips to the empty string — the caller's emptiness check reports that
    /// as nothing to amend.
    pub fn strip_self_headings(paragraph: &str) -> (String, usize) {
        let mut rest = paragraph.trim();
        let mut stripped = 0;
        while let Some(remainder) = Self::leading_self_heading(rest) {
            rest = remainder.trim_start();
            stripped += 1;
        }
        (rest.to_string(), stripped)
    }

    /// Append a dated amendment paragraph to a record's body (plan be16ea36
    /// step 7): the sanctioned way to ADD information to a knowledge record
    /// without replacing it — `memory_amend`'s backing. Append-only: the
    /// existing body is preserved verbatim, so the row-digest guard can
    /// never fire (the body only grows). Refuses unknown rel paths (not a
    /// knowledge record) and superseded records (history is never edited —
    /// amend the successor instead). Returns the rel path (unchanged).
    pub fn amend(&self, rel_path: &str, paragraph: &str) -> Result<String> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| Error::Memory("knowledge lock poisoned".into()))?;
        let path = self.knowledge_dir.join(rel_path);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::Memory(format!("knowledge amend: {}: {e}", rel_path)))?;
        let record = parse_file(rel_path, &text).ok_or_else(|| {
            Error::Memory(format!(
                "knowledge amend: {}: not a knowledge record",
                rel_path
            ))
        })?;
        if record.status == KnowledgeStatus::Superseded {
            return Err(Error::Memory(format!(
                "refused: {rel_path} is superseded (history is never edited) — \
                 amend its successor instead"
            )));
        }
        let paragraph = paragraph.trim();
        if paragraph.is_empty() {
            return Err(Error::Memory(
                "nothing to amend: the paragraph is empty".into(),
            ));
        }
        // A caller-supplied amendment heading is stripped (backlog 488248ce):
        // the stamp below is then the only heading, and its date is
        // authoritative — a caller's own date can never contradict the line
        // it heads.
        let (paragraph, _) = Self::strip_self_headings(paragraph);
        if paragraph.is_empty() {
            return Err(Error::Memory(
                "nothing to amend: the paragraph is only an 'Amended …' heading — pass the \
                 amendment text itself (the tool stamps the heading)"
                    .into(),
            ));
        }
        let body = format!(
            "{}\n\nAmended {}: {}",
            record.body.trim_end(),
            self.stamped_date(),
            paragraph
        );
        let file = format!(
            "{}\n{}\n",
            render_front_matter(
                &record.title,
                record.status,
                record.supersedes.as_deref(),
                record.created.as_deref()
            ),
            body
        );
        std::fs::write(path, file)?;
        Ok(rel_path.to_string())
    }

    /// True when `candidate` carries the row-digest signature: strictly
    /// shorter than the file's current body AND its last non-empty line is
    /// a knowledge-path pointer (contains `.coding/knowledge/`, ends with
    /// `.md`). Such content is a condensed pointer AT the file — writing
    /// it over the body destroys the detail it points at. False positives
    /// are safe (a refusal, never data loss); legitimate updates — full
    /// bodies, or short ones without a pointer tail — pass through.
    fn looks_like_row_digest(candidate: &str, current_body: &str) -> bool {
        if candidate.trim().len() >= current_body.trim().len() {
            return false;
        }
        let Some(last) = candidate.lines().rev().find(|l| !l.trim().is_empty()) else {
            return false;
        };
        let last = last.trim();
        last.contains(".coding/knowledge/") && last.ends_with(".md")
    }

    /// Supersede a record: write the successor file (same type dir, new
    /// date-slug, front matter `supersedes = "<old slug>"`) and flip the old
    /// file's status to `superseded`. Supersede-not-delete: the old file
    /// stays on disk as history. Returns the successor's rel path.
    pub fn supersede(&self, old_rel: &str, new_title: &str, new_body: &str) -> Result<String> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| Error::Memory("knowledge lock poisoned".into()))?;
        // Read the old record to learn its slug (the successor's supersedes).
        let old_path = self.knowledge_dir.join(old_rel);
        let old_text = std::fs::read_to_string(&old_path)
            .map_err(|e| Error::Memory(format!("knowledge supersede: {}: {e}", old_rel)))?;
        let old = parse_file(old_rel, &old_text).ok_or_else(|| {
            Error::Memory(format!(
                "knowledge supersede: {}: not a knowledge record",
                old_rel
            ))
        })?;
        let dir = dir_for_record_type(old.record_type).ok_or_else(|| {
            Error::Memory(format!(
                "record type '{}' has no knowledge home",
                old.record_type
            ))
        })?;
        // The successor's front-matter title is BARE (no typed prefix) — the
        // record's own type is authoritative (see `bare_title`).
        let new_title = Self::bare_title(new_title, old.record_type);
        let date = self.stamped_date();
        let base = slug_for(&new_title, &date);
        let new_rel = {
            let mut new_rel = String::new();
            for n in 1u32.. {
                let slug = if n == 1 {
                    base.clone()
                } else {
                    format!("{base}-{n}")
                };
                let candidate = format!("{dir}/{slug}.md");
                if !self.knowledge_dir.join(&candidate).exists() {
                    new_rel = candidate;
                    break;
                }
            }
            new_rel
        };
        // Successor file.
        let successor_path = self.knowledge_dir.join(&new_rel);
        std::fs::create_dir_all(successor_path.parent().unwrap())?;
        let file = format!(
            "{}\n{}\n",
            render_front_matter(
                &new_title,
                KnowledgeStatus::Live,
                Some(&old.slug),
                Some(&date)
            ),
            new_body
        );
        std::fs::write(successor_path, file)?;
        // Flip the old file (preserve its title/created; mark superseded).
        let flipped = format!(
            "{}\n{}\n",
            render_front_matter(
                &old.title,
                KnowledgeStatus::Superseded,
                old.supersedes.as_deref(),
                old.created.as_deref()
            ),
            old.body
        );
        std::fs::write(&old_path, flipped)?;
        Ok(new_rel)
    }

    /// Hard-delete a record's file (junk cleanup — the truth itself dies).
    pub fn delete(&self, rel_path: &str) -> Result<String> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| Error::Memory("knowledge lock poisoned".into()))?;
        let path = self.knowledge_dir.join(rel_path);
        std::fs::remove_file(&path)
            .map_err(|e| Error::Memory(format!("knowledge delete: {}: {e}", rel_path)))?;
        Ok(rel_path.to_string())
    }

    /// The rel path of a record by type+slug — `Some` when the file exists
    /// (the deterministic file a successor's `supersedes` references).
    pub fn resolve(&self, record_type: MemoryRecordType, slug: &str) -> Option<String> {
        let dir = dir_for_record_type(record_type)?;
        let rel = format!("{dir}/{slug}.md");
        self.knowledge_dir.join(&rel).is_file().then_some(rel)
    }
}

/// Format epoch seconds as a UTC calendar date (`YYYY-MM-DD`) — Howard
/// Hinnant's `civil_from_days`, exact for the full day range (the crate has
/// no chrono dependency; mirrors the memory tools' `format_epoch_date`).
fn fmt_date(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = "+++\ntitle = \"Typed records as files\"\nstatus = \"superseded\"\nsupersedes = \"2026-08-23-merge-bundles\"\ncreated = \"2026-08-23\"\n+++\n\nAdopted files as truth. See [[decision/2026-08-23-merge-bundles]],\n[[plan/d16b3c22-afa1-4ff3-afc1-83872e9b02c6]], [[review/2026-08-20-git-tab-dag-review]],\n[[spec/branch-strategy]] and [[src/memory/knowledge.rs]]. Plain [[word]] too.\n";

    #[test]
    fn parses_front_matter_links_and_status() {
        let rec = parse_file("decision/2026-08-23-typed-records-as-files.md", FULL)
            .expect("knowledge shape parses");
        assert_eq!(rec.record_type, MemoryRecordType::Decision);
        assert_eq!(rec.slug, "2026-08-23-typed-records-as-files");
        assert_eq!(rec.title, "Typed records as files");
        assert_eq!(rec.status, KnowledgeStatus::Superseded);
        assert_eq!(rec.supersedes.as_deref(), Some("2026-08-23-merge-bundles"));
        assert_eq!(rec.created.as_deref(), Some("2026-08-23"));
        assert_eq!(
            rec.source_key(),
            "knowledge:decision/2026-08-23-typed-records-as-files"
        );
        assert_eq!(rec.typed_title(), "DECISION: Typed records as files");
        assert_eq!(
            rec.pointer_line(),
            "path .coding/knowledge/decision/2026-08-23-typed-records-as-files.md"
        );

        let kinds: Vec<_> = rec
            .links
            .iter()
            .map(|l| (l.target.as_str(), l.kind))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (
                    "decision/2026-08-23-merge-bundles",
                    KnowledgeLinkKind::Knowledge
                ),
                (
                    "plan/d16b3c22-afa1-4ff3-afc1-83872e9b02c6",
                    KnowledgeLinkKind::Plan
                ),
                (
                    "review/2026-08-20-git-tab-dag-review",
                    KnowledgeLinkKind::Review
                ),
                ("spec/branch-strategy", KnowledgeLinkKind::Knowledge),
                ("src/memory/knowledge.rs", KnowledgeLinkKind::File),
                ("word", KnowledgeLinkKind::Other),
            ]
        );
    }

    #[test]
    fn degrades_without_front_matter() {
        let text = "# Branch strategy\n\nWorking branches merge into develop.\n";
        let rec = parse_file("spec/2026-08-23-branch-strategy.md", text).unwrap();
        assert_eq!(rec.title, "Branch strategy");
        assert_eq!(rec.status, KnowledgeStatus::Live);
        assert_eq!(rec.supersedes, None);
        // No front-matter created → the slug's date prefix is the fallback.
        assert_eq!(rec.created.as_deref(), Some("2026-08-23"));
        assert!(rec.body.contains("Working branches"));
    }

    #[test]
    fn degrades_on_bad_toml_and_unclosed_fence() {
        // A fence with invalid TOML: the parse still succeeds with defaults
        // and the body after the closing fence is kept.
        let bad = "+++\ntitle = missing-quotes\n+++\n\nBody survives.\n";
        let rec = parse_file("how/release-checklist.md", bad).unwrap();
        assert_eq!(rec.status, KnowledgeStatus::Live);
        assert!(rec.body.contains("Body survives."));
        // Title degraded from body (no heading → first content line).
        assert_eq!(rec.title, "Body survives.");

        // An unclosed fence is all body.
        let unclosed = "+++\ntitle = \"x\"\n\nNo closing fence.\n";
        let rec = parse_file("how/unclosed.md", unclosed).unwrap();
        assert!(rec.body.contains("No closing fence."));
    }

    #[test]
    fn empty_front_matter_fields_fall_back() {
        let text = "+++\ntitle = \"\"\nstatus = \"bogus\"\n+++\n\n# Heading title\n";
        let rec = parse_file("bug/2026-08-19-os-error-5.md", text).unwrap();
        assert_eq!(rec.title, "Heading title");
        assert_eq!(rec.status, KnowledgeStatus::Live);
        assert_eq!(rec.created.as_deref(), Some("2026-08-19"));
    }

    #[test]
    fn unknown_paths_return_none() {
        assert!(parse_file("notes/foo.md", "body").is_none());
        assert!(parse_file("plan/foo.md", "body").is_none());
        assert!(parse_file("review/foo.md", "body").is_none());
        assert!(parse_file("decision/foo.txt", "body").is_none());
        assert!(parse_file("foo.md", "body").is_none());
    }

    #[test]
    fn dir_type_roundtrip() {
        for dir in ["spec", "decision", "bug", "how"] {
            let t = record_type_for_dir(dir).unwrap();
            assert_eq!(dir_for_record_type(t), Some(dir));
        }
        assert!(record_type_for_dir("plan").is_none());
        assert!(record_type_for_dir("review").is_none());
        assert!(record_type_for_dir("notes").is_none());
        assert!(dir_for_record_type(MemoryRecordType::Plan).is_none());
        assert!(dir_for_record_type(MemoryRecordType::None).is_none());
        assert_eq!(title_prefix(MemoryRecordType::Spec), Some("SPEC:"));
        assert_eq!(title_prefix(MemoryRecordType::How), Some("HOW:"));
        assert_eq!(title_prefix(MemoryRecordType::Plan), None);
    }

    #[test]
    fn slug_generation() {
        assert_eq!(
            slug_for("Typed records as files!", "2026-08-23"),
            "2026-08-23-typed-records-as-files"
        );
        assert_eq!(
            slug_for("  OS error 5 -- misdirection  ", "2026-08-19"),
            "2026-08-19-os-error-5-misdirection"
        );
        // Long titles cap at the title-part limit, char-safe, no trailing
        // hyphen from the cut.
        let long = "w".repeat(200);
        assert_eq!(
            slug_for(&long, "2026-08-23"),
            format!("2026-08-23-{}", "w".repeat(SLUG_TITLE_MAX_CHARS))
        );
        let mixed = "ab\u{e9}cd -- ef"; // non-ASCII acts as a separator
        assert_eq!(slug_for(mixed, "2026-08-23"), "2026-08-23-ab-cd-ef");
        // No ASCII letters at all → the `record` fallback.
        assert_eq!(slug_for("日本語", "2026-08-23"), "2026-08-23-record");
    }

    #[test]
    fn date_from_slug_shape() {
        assert_eq!(date_from_slug("2026-08-23-foo"), Some("2026-08-23"));
        assert_eq!(date_from_slug("2026-08-23"), None); // bare date, no slug part
        assert_eq!(date_from_slug("release-checklist"), None);
        assert_eq!(date_from_slug("2026-8-23-foo"), None);
    }

    #[test]
    fn link_extraction_edges() {
        let links = extract_links("a [[one]] b [[]] c [[two ]] [[plan/x]]");
        let targets: Vec<_> = links.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(targets, vec!["one", "two", "plan/x"]);
        // Unclosed bracket ends the scan; nested brackets degrade gracefully.
        assert!(extract_links("[[unclosed").is_empty());
        assert_eq!(extract_links("[[[x]]]")[0].target, "[x");
        assert!(extract_links("plain text").is_empty());
    }

    // ── KnowledgeStore (file-backed writer) ──────────────────────────────

    const CLOCK: i64 = 1_769_904_000; // 2026-02-01 UTC

    fn make_store() -> (tempfile::TempDir, KnowledgeStore) {
        let dir = tempfile::tempdir().unwrap();
        let ks = KnowledgeStore::with_clock(dir.path().join(".coding/knowledge"), || CLOCK);
        (dir, ks)
    }

    fn read_file(rel: &str, dir: &tempfile::TempDir) -> String {
        std::fs::read_to_string(dir.path().join(rel)).unwrap()
    }

    #[test]
    fn write_creates_live_file_with_front_matter() {
        let (dir, ks) = make_store();
        let rel = ks
            .write(
                MemoryRecordType::Decision,
                "Typed records as files",
                "Body text.\nSecond line.",
            )
            .unwrap();
        assert_eq!(rel, "decision/2026-02-01-typed-records-as-files.md");
        let text = read_file(&format!(".coding/knowledge/{rel}"), &dir);
        assert!(
            text.starts_with(
                "+++\ntitle = \"Typed records as files\"\ncreated = \"2026-02-01\"\n+++\n"
            ),
            "{text}"
        );
        assert!(text.contains("Body text.\nSecond line.\n"));
        // Round-trips through the parser as a live record.
        let rec = parse_file(&rel, &text).unwrap();
        assert_eq!(rec.record_type, MemoryRecordType::Decision);
        assert_eq!(rec.status, KnowledgeStatus::Live);
        assert_eq!(rec.supersedes, None);
    }

    #[test]
    fn write_rejects_non_knowledge_types() {
        let (_dir, ks) = make_store();
        for t in [
            MemoryRecordType::Plan,
            MemoryRecordType::Review,
            MemoryRecordType::None,
        ] {
            assert!(ks.write(t, "x", "y").is_err());
        }
    }

    #[test]
    fn write_collisions_get_a_suffix() {
        let (_dir, ks) = make_store();
        let a = ks
            .write(MemoryRecordType::How, "run tests", "body")
            .unwrap();
        let b = ks
            .write(MemoryRecordType::How, "run tests", "body2")
            .unwrap();
        assert_eq!(a, "how/2026-02-01-run-tests.md");
        assert_eq!(b, "how/2026-02-01-run-tests-2.md");
    }

    #[test]
    fn write_dates_from_artifact_floor_when_clock_stale() {
        // Backlog d7b2e722 / review LOW 2: a stale OS wall clock (here
        // pinned to 2026-02-01, mirroring the 2026-09-01 incident) back-dated
        // every export. The stamped date must never fall below the newest
        // on-disk artifact date — a 2026-12-19 review file floors new
        // records at 2026-12-19 regardless of the clock.
        let dir = tempfile::tempdir().unwrap();
        let reviews = dir.path().join(".coding/reviews");
        std::fs::create_dir_all(&reviews).unwrap();
        std::fs::write(reviews.join("2026-12-19-levers-review.md"), "x").unwrap();
        let ks = KnowledgeStore::with_clock(dir.path().join(".coding/knowledge"), || CLOCK);
        let rel = ks
            .write(MemoryRecordType::Decision, "Stale clock record", "body")
            .unwrap();
        assert_eq!(rel, "decision/2026-12-19-stale-clock-record.md");
        let text = read_file(&format!(".coding/knowledge/{rel}"), &dir);
        assert!(
            text.contains("created = \"2026-12-19\""),
            "created must match the artifact floor, not the stale clock: {text}"
        );
    }

    #[test]
    fn clock_ahead_of_artifact_floor_wins() {
        // Normal case: the wall clock is ahead of every on-disk artifact —
        // the clock date wins and behavior is identical to the old
        // clock-only stamping.
        let dir = tempfile::tempdir().unwrap();
        let reviews = dir.path().join(".coding/reviews");
        std::fs::create_dir_all(&reviews).unwrap();
        std::fs::write(reviews.join("2026-12-19-levers-review.md"), "x").unwrap();
        // 1_799_107_200 = 2027-01-05 UTC (338 days after CLOCK).
        let ks = KnowledgeStore::with_clock(dir.path().join(".coding/knowledge"), || 1_799_107_200);
        let rel = ks
            .write(MemoryRecordType::Spec, "Clock ahead", "body")
            .unwrap();
        assert_eq!(rel, "spec/2027-01-05-clock-ahead.md");
    }

    #[test]
    fn supersede_successor_never_backdates_below_floor() {
        // The supersede successor is stamped through the same floored date:
        // a stale clock must not date the successor BEFORE the floor.
        let dir = tempfile::tempdir().unwrap();
        let reviews = dir.path().join(".coding/reviews");
        std::fs::create_dir_all(&reviews).unwrap();
        std::fs::write(reviews.join("2026-12-19-levers-review.md"), "x").unwrap();
        let ks = KnowledgeStore::with_clock(dir.path().join(".coding/knowledge"), || CLOCK);
        let old = ks
            .write(MemoryRecordType::How, "Old record", "body")
            .unwrap();
        let successor = ks.supersede(&old, "New record", "body").unwrap();
        assert!(
            successor.starts_with("how/2026-12-19-"),
            "successor must carry the artifact floor date, got {successor}"
        );
    }

    #[test]
    fn malformed_floor_candidates_are_ignored() {
        // Junk stems must not move the floor: impossible month, undated name,
        // and far-future dates (beyond the slack ceiling) all contribute
        // nothing — with no valid floor the stale clock date stands.
        let dir = tempfile::tempdir().unwrap();
        let reviews = dir.path().join(".coding/reviews");
        std::fs::create_dir_all(&reviews).unwrap();
        std::fs::write(reviews.join("2026-13-99-bad-month.md"), "x").unwrap();
        std::fs::write(reviews.join("nodate.md"), "x").unwrap();
        std::fs::write(reviews.join("2999-01-01-far-future.md"), "x").unwrap();
        std::fs::write(reviews.join("2026-02-31-bad-day.md"), "x").unwrap();
        std::fs::write(reviews.join("2026-04-31-bad-day.md"), "x").unwrap();
        let ks = KnowledgeStore::with_clock(dir.path().join(".coding/knowledge"), || CLOCK);
        let rel = ks
            .write(MemoryRecordType::Bug, "Junk floor", "body")
            .unwrap();
        assert_eq!(rel, "bug/2026-02-01-junk-floor.md");
    }

    #[test]
    fn update_preserves_identity_and_status() {
        let (dir, ks) = make_store();
        let rel = ks
            .write(MemoryRecordType::Bug, "os error 5", "old body")
            .unwrap();
        ks.update(&rel, Some("OS error 5 misdirection"), Some("new body"))
            .unwrap();
        let text = read_file(&format!(".coding/knowledge/{rel}"), &dir);
        let rec = parse_file(&rel, &text).unwrap();
        assert_eq!(rec.title, "OS error 5 misdirection");
        assert_eq!(rec.body, "new body");
        assert_eq!(rec.status, KnowledgeStatus::Live);
    }

    #[test]
    fn amend_appends_dated_paragraph_and_preserves_body() {
        let (dir, ks) = make_store();
        let rel = ks
            .write(
                MemoryRecordType::Decision,
                "storage engine",
                "the store is sqlite",
            )
            .unwrap();
        // Append-only: a SHORT paragraph into a LONGER body succeeds — the
        // row-digest guard can never fire because the body only grows.
        ks.amend(&rel, "with WAL mode").unwrap();
        let text = read_file(&format!(".coding/knowledge/{rel}"), &dir);
        let rec = parse_file(&rel, &text).unwrap();
        assert_eq!(rec.status, KnowledgeStatus::Live);
        assert_eq!(rec.title, "storage engine");
        assert!(
            rec.body
                .starts_with("the store is sqlite\n\nAmended 2026-02-01: with WAL mode"),
            "{}",
            rec.body
        );

        // Unknown rel path refused.
        assert!(ks.amend("decision/does-not-exist.md", "x").is_err());
        // Empty paragraph refused.
        assert!(ks.amend(&rel, "   ").is_err());

        // Superseded records are refused — history is never edited; the
        // successor itself is amendable.
        let succ = ks.supersede(&rel, "storage engine v2", "new body").unwrap();
        assert!(ks.amend(&rel, "x").is_err());
        ks.amend(&succ, "amend the successor").unwrap();
    }

    #[test]
    fn amend_strips_self_supplied_heading_and_stamps_its_own_date() {
        // Regression (backlog 488248ce / reviewer finding LOW 1 in
        // .coding/reviews/2026-09-15-imperative-steering-phrasing-review.md):
        // amend prefixes its own "Amended <date>: " stamp, so a paragraph that
        // already opens with a heading produced "Amended 2027-01-11: Amended
        // 2027-01-14: …" — a doubled heading whose dates disagree. The
        // supplied heading is stripped and the tool's stamp stays
        // authoritative: exactly ONE heading ever lands.
        let (dir, ks) = make_store();
        // Every stamped heading — "Amended" or "Amendment", any case.
        let headings = |body: &str| {
            let lower = body.to_lowercase();
            lower.matches("amended").count() + lower.matches("amendment").count()
        };
        let amended = |paragraph: &str| {
            let rel = ks
                .write(
                    MemoryRecordType::Decision,
                    "storage engine",
                    "the store is sqlite",
                )
                .unwrap();
            ks.amend(&rel, paragraph).unwrap();
            let text = read_file(&format!(".coding/knowledge/{rel}"), &dir);
            parse_file(&rel, &text).unwrap().body
        };

        // The reported shape: a date-headed paragraph.
        let body = amended("Amended 2027-01-14: the zero-file hint's wording changed");
        assert_eq!(headings(&body), 1, "{body}");
        assert!(
            body.contains("Amended 2026-02-01: the zero-file hint's wording changed"),
            "{body}"
        );

        // The parenthetical variant that hit the second record.
        let body = amended(
            "Amended 2027-01-14 (plan 987fef4c, backlog 56168c38 — notes): \
             the nudge tails are imperatives",
        );
        assert_eq!(headings(&body), 1, "{body}");
        assert!(
            body.contains("Amended 2026-02-01: the nudge tails are imperatives"),
            "{body}"
        );

        // Uppercase + review-attributed variant (a shape already on disk).
        let body = amended("AMENDED 2027-01-11 (review L1, plan 9441d776): x");
        assert_eq!(headings(&body), 1, "{body}");
        assert!(body.contains("Amended 2026-02-01: x"), "{body}");

        // "Amendment (…)" — the other named form.
        let body = amended("Amendment (plan 987fef4c): y");
        assert_eq!(headings(&body), 1, "{body}");
        assert!(body.contains("Amended 2026-02-01: y"), "{body}");

        // A colon inside the attribution group does not fool the strip (review
        // LOW 1).
        let body = amended(
            "Amended 2027-01-14 (review L1: the message was wrong): the wording changed",
        );
        assert_eq!(headings(&body), 1, "{body}");
        assert!(
            body.contains("Amended 2026-02-01: the wording changed"),
            "{body}"
        );

        // A doubled chain collapses to one heading in a single amend.
        let body = amended("Amended 2027-01-11: Amended 2027-01-14: z");
        assert_eq!(headings(&body), 1, "{body}");
        assert!(body.contains("Amended 2026-02-01: z"), "{body}");

        // Prose that merely OPENS with the word is not a heading — it must
        // survive verbatim (no content swallowed by a greedy strip).
        let prose = "Amended the readme and the changelog: both now mention the flag";
        let body = amended(prose);
        assert!(body.contains(prose), "prose must survive verbatim: {body}");
        let prose = "Landed outcome (2027-01-14): the clamp is in place";
        let body = amended(prose);
        assert!(body.contains(prose), "prose must survive verbatim: {body}");

        // A bare heading is not a paragraph — nothing left to amend.
        let rel = ks
            .write(MemoryRecordType::Decision, "bare heading", "body")
            .unwrap();
        assert!(ks.amend(&rel, "Amended 2027-01-14:").is_err());
    }

    #[test]
    fn strip_self_headings_matches_stamp_shapes_only() {
        let strip = KnowledgeStore::strip_self_headings;
        assert_eq!(strip("Amended 2027-01-14: x"), ("x".to_string(), 1));
        assert_eq!(
            strip("Amended 2027-01-14 (plan 987fef4c, backlog 56168c38 — notes): x"),
            ("x".to_string(), 1)
        );
        assert_eq!(
            strip("AMENDED 2027-01-11 (review L1, plan 9441d776): x"),
            ("x".to_string(), 1)
        );
        assert_eq!(strip("Amendment (plan 987fef4c): y"), ("y".to_string(), 1));
        assert_eq!(strip("amended 2026-02-01: y"), ("y".to_string(), 1));
        // A doubled chain collapses in one call.
        assert_eq!(
            strip("Amended 2027-01-11: Amended 2027-01-14: z"),
            ("z".to_string(), 2)
        );
        // Lines after the heading survive; a bare heading strips to nothing.
        assert_eq!(
            strip("Amended 2027-01-14: first\n\nsecond"),
            ("first\n\nsecond".to_string(), 1)
        );
        assert_eq!(
            strip("Amended 2027-01-14:\nSecond line."),
            ("Second line.".to_string(), 1)
        );
        assert_eq!(strip("Amended 2027-01-14:"), (String::new(), 1));
        // An over-long head is prose, not a stamp.
        let wide = format!("Amended 2027-01-14 ({}): x", "notes,".repeat(24));
        assert_eq!(strip(&wide), (wide.clone(), 0));
        // A colon INSIDE the attribution group does not end the head (review
        // LOW 1): the strip still fires, and the group is never emitted as the
        // paragraph's text.
        assert_eq!(
            strip("Amended 2027-01-14 (review L1: the message was wrong): the wording changed"),
            ("the wording changed".to_string(), 1)
        );
        assert_eq!(
            strip("Amendment (plan 987fef4c: the nudge fix): x"),
            ("x".to_string(), 1)
        );
        // A heading line with no colon at all: the whole first line is the
        // heading and the text follows on the next line.
        assert_eq!(
            strip("Amended 2027-01-14\n\nBody text"),
            ("Body text".to_string(), 1)
        );
        assert_eq!(
            strip("Amended 2027-01-14\nBody text"),
            ("Body text".to_string(), 1)
        );
        assert_eq!(
            strip("Amendment (plan 987fef4c)\nBody text"),
            ("Body text".to_string(), 1)
        );
        assert_eq!(strip("Amended 2027-01-14"), (String::new(), 1));
        // Not headings: prose that merely opens with the word, a longer word,
        // a year inside a sentence, an unrelated parenthetical, or a prose line
        // with interior colons — all must survive verbatim.
        for prose in [
            "Amended the readme and the changelog: both now mention the flag",
            "Landed outcome (2027-01-14): the clamp is in place",
            "Amendedness: not a stamp",
            "Amended 2027 was the year the store moved: really",
        ] {
            assert_eq!(strip(prose), (prose.to_string(), 0), "{prose}");
        }
    }

    #[test]
    fn replace_unique_requires_exactly_one_match() {
        let body = "alpha beta gamma";
        assert_eq!(
            replace_unique(body, "beta", "BETA").unwrap(),
            "alpha BETA gamma"
        );
        // Absent, ambiguous, blank and body-emptying finds are all refused.
        assert!(replace_unique(body, "delta", "x").is_err());
        assert!(replace_unique("a a", "a", "b").is_err());
        assert!(replace_unique(body, "   ", "x").is_err());
        assert!(replace_unique("only", "only", "").is_err());
        assert!(replace_unique(body, "alpha beta gamma", " ").is_err());
        // A deletion (empty replacement) is a legitimate repair: this is the
        // collapse the doubled-heading incident needed on disk.
        let stray = "x\n\nAmended 2027-01-11: Amended 2027-01-14: y";
        assert_eq!(
            replace_unique(stray, "Amended 2027-01-14: ", "").unwrap(),
            "x\n\nAmended 2027-01-11: y"
        );
    }

    #[test]
    fn replace_in_body_repairs_record_text_without_digest_guard_interference() {
        let (dir, ks) = make_store();
        let body = "the clamp lives here\n\nAmended 2026-01-01: Amended 2026-02-02: stale\n\n\
                    full detail: .coding/knowledge/spec/2026-02-01-clamp.md";
        let rel = ks.write(MemoryRecordType::Spec, "clamp", body).unwrap();
        let path = format!(".coding/knowledge/{rel}");
        let before = parse_file(&rel, &read_file(&path, &dir)).unwrap();

        // The doubled heading collapses onto the one the tool stamped first.
        assert_eq!(
            ks.replace_in_body(&rel, "Amended 2026-02-02: ", "")
                .unwrap(),
            1
        );
        let after = parse_file(&rel, &read_file(&path, &dir)).unwrap();
        assert!(
            after.body.contains("Amended 2026-01-01: stale"),
            "{}",
            after.body
        );
        assert!(!after.body.contains("2026-02-02"), "{}", after.body);
        assert!(after
            .body
            .ends_with(".coding/knowledge/spec/2026-02-01-clamp.md"));
        assert!(after.body.contains("the clamp lives here"));
        // Front matter survives intact: the repair is body-only.
        assert_eq!(after.title, before.title);
        assert_eq!(after.status, before.status);
        assert_eq!(after.created, before.created);
        assert_eq!(after.supersedes, before.supersedes);
        assert_eq!(after.record_type, before.record_type);

        // This repair SHRINKS the body while it still ends in a pointer tail —
        // exactly the shape update()'s row-digest guard refuses, which is why
        // the repair path deliberately does not consult it.
        assert!(ks
            .update(
                &rel,
                None,
                Some("see .coding/knowledge/spec/2026-02-01-clamp.md")
            )
            .is_err());

        // Ambiguous, unknown-path and superseded targets are refused.
        assert!(ks.replace_in_body(&rel, "e", "E").is_err());
        assert!(ks.replace_in_body("spec/nope.md", "a", "b").is_err());
        ks.supersede(&rel, "clamp v2", "see the successor").unwrap();
        assert!(ks.replace_in_body(&rel, "Amended", "X").is_err());
    }

    #[test]
    fn supersede_flips_old_and_writes_successor() {
        let (dir, ks) = make_store();
        let old_rel = ks
            .write(MemoryRecordType::Decision, "Old storage", "old body")
            .unwrap();
        let new_rel = ks.supersede(&old_rel, "New storage", "new body").unwrap();
        assert_eq!(new_rel, "decision/2026-02-01-new-storage.md");
        // The successor carries `supersedes` pointing at the old slug.
        let succ_text = read_file(&format!(".coding/knowledge/{new_rel}"), &dir);
        let succ = parse_file(&new_rel, &succ_text).unwrap();
        assert_eq!(succ.supersedes.as_deref(), Some("2026-02-01-old-storage"));
        assert_eq!(succ.status, KnowledgeStatus::Live);
        assert_eq!(succ.body, "new body");
        // The old file flipped to superseded, content preserved.
        let old_text = read_file(&format!(".coding/knowledge/{old_rel}"), &dir);
        let old_rec = parse_file(&old_rel, &old_text).unwrap();
        assert_eq!(old_rec.status, KnowledgeStatus::Superseded);
        assert_eq!(old_rec.body, "old body");
    }

    #[test]
    fn supersede_dates_the_successor_from_the_clock_not_the_predecessor() {
        // F12 verification: the successor slug + front-matter `created`
        // carry the WRITE-TIME clock, never the predecessor's date — guards
        // the (disproven 2026-09-17 via git evidence) claim that the tool
        // copies the old record's date into the new filename.
        let (dir, ks) = make_store();
        let old_rel = ks
            .write(MemoryRecordType::Decision, "Dated old", "old body")
            .unwrap();
        assert_eq!(old_rel, "decision/2026-02-01-dated-old.md");
        let later = KnowledgeStore::with_clock(
            dir.path().join(".coding/knowledge"),
            || CLOCK + 86_400, // one day later — 2026-02-02 UTC
        );
        let new_rel = later.supersede(&old_rel, "Dated new", "new body").unwrap();
        assert_eq!(new_rel, "decision/2026-02-02-dated-new.md");
        let text = read_file(&format!(".coding/knowledge/{new_rel}"), &dir);
        let succ = parse_file(&new_rel, &text).unwrap();
        assert_eq!(succ.created.as_deref(), Some("2026-02-02"));
    }

    #[test]
    fn supersede_unknown_file_errors() {
        let (_dir, ks) = make_store();
        assert!(ks.supersede("decision/nope.md", "t", "b").is_err());
    }

    #[test]
    fn delete_removes_the_file() {
        let (dir, ks) = make_store();
        let rel = ks
            .write(MemoryRecordType::How, "checklist", "body")
            .unwrap();
        ks.delete(&rel).unwrap();
        assert!(!dir.path().join(".coding/knowledge").join(&rel).exists());
        assert!(ks.delete(&rel).is_err());
    }

    #[test]
    fn resolve_finds_existing_files() {
        let (_dir, ks) = make_store();
        let rel = ks.write(MemoryRecordType::Bug, "login crash", "b").unwrap();
        assert_eq!(
            ks.resolve(MemoryRecordType::Bug, "2026-02-01-login-crash"),
            Some(rel)
        );
        assert_eq!(ks.resolve(MemoryRecordType::Bug, "missing"), None);
    }
}
