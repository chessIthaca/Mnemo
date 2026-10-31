// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Plan-scoped memories: detection at store time, settlement at plan close.
//!
//! A note that names a plan id ("Plan 64694f06 ...") or a branch ("branch
//! feature/x", "wt/...") is only true while that plan/branch lives. Filed as a
//! durable semantic fact it would be recalled long after the plan closed and
//! the branch is gone. So `memory_write` routes such notes to the Episodic
//! tier and tags them with the plan id(s); when the plan closes,
//! [`settle_plan_memories`] returns the tagged notes (for the closing report)
//! and retires them from normal recall.
//!
//! The tag lives in the existing `Memory.data` JSON (no schema change):
//! `{"plan_scoped": true, "plan_ids": ["64694f06"], "branch": false}`.
//! Retirement is a quarantine (recoverable via `include_quarantined`, never a
//! delete): quarantined rows are excluded from recall, listing and the primer.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::{json, Value};

use crate::error::Result;
use crate::memory::{Memory, MemoryFilter, MemoryStoreTrait, MemoryTier};

/// A plan-scoped note returned (and retired) by [`settle_plan_memories`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettledNote {
    pub id: String,
    pub title: String,
    pub content: String,
}

fn plan_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // "plan 64694f06", "Plan #64694f06", "plan_id: 64694f06", "plan id=...",
        // ".coding/plans/64694f06.md". The word "plan" must be adjacent.
        Regex::new(
            r#"(?i)(?:\bplan(?:[_\- ]?id)?\b[\s:=#"'`(\[]*(?:id\b[\s:=#"'`]*)?|\.coding/plans/)([0-9a-f]{8})\b"#,
        )
        .expect("plan id regex")
    })
}

fn branch_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // "branch feature/x", "branch: `fix/y`", or a bare "wt/..." worktree branch.
        Regex::new(
            r#"(?i)(?:\bbranch(?:es)?\b[\s:=]*[`'"]?[\w.\-]+/[\w./\-]+|(?:^|[\s`'"(])wt/\w[\w.\-/]*)"#,
        )
        .expect("branch regex")
    })
}

/// Plan ids (8 lowercase hex, deduped, in order of appearance) referenced in
/// `text`. A bare 8-hex token (a commit SHA) never matches: the word "plan"
/// (or `plan_id`, or a `.coding/plans/` path) must sit directly before it,
/// and the token must contain a digit.
pub fn plan_refs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for cap in plan_re().captures_iter(text) {
        let id = cap[1].to_ascii_lowercase();
        if id.bytes().any(|b| b.is_ascii_digit()) && !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// Whether `text` names a branch ("branch feature/x", "wt/...").
pub fn mentions_branch(text: &str) -> bool {
    branch_re().is_match(text)
}

/// The scope verdict for a note about to be stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanScope {
    pub tier: MemoryTier,
    pub plan_ids: Vec<String>,
    pub branch: bool,
}

impl PlanScope {
    /// True when the note references a plan or a branch.
    pub fn is_scoped(&self) -> bool {
        !self.plan_ids.is_empty() || self.branch
    }

    /// The `Memory.data` tag, or `Null` when the note is not plan-scoped.
    pub fn data(&self) -> Value {
        if self.is_scoped() {
            json!({"plan_scoped": true, "plan_ids": self.plan_ids, "branch": self.branch})
        } else {
            Value::Null
        }
    }
}

/// Decide the tier + tags for a note. Only a Semantic request is demoted (to
/// Episodic); other tiers keep their tier but still carry the plan tag so the
/// close-out can find them.
pub fn scoped_tier(requested: MemoryTier, title: &str, content: &str) -> PlanScope {
    let mut plan_ids = plan_refs(title);
    for id in plan_refs(content) {
        if !plan_ids.contains(&id) {
            plan_ids.push(id);
        }
    }
    let branch = mentions_branch(title) || mentions_branch(content);
    let scoped = !plan_ids.is_empty() || branch;
    let tier = if scoped && requested == MemoryTier::Semantic {
        MemoryTier::Episodic
    } else {
        requested
    };
    PlanScope { tier, plan_ids, branch }
}

fn tagged_with(m: &Memory, plan_id: &str) -> bool {
    m.data
        .get("plan_ids")
        .and_then(Value::as_array)
        .is_some_and(|ids| ids.iter().any(|v| v.as_str() == Some(plan_id)))
}

/// Find every live memory (any tier) tagged with `plan_id`, retire them from
/// recall (quarantine: recoverable, never a delete) and return them for the
/// closing report. Idempotent: already-retired notes are not returned again.
pub async fn settle_plan_memories(
    store: &dyn MemoryStoreTrait,
    plan_id: &str,
) -> Result<Vec<SettledNote>> {
    let plan_id = plan_id.trim().to_ascii_lowercase();
    let mut live = store
        .list_filtered(&MemoryFilter::new().include_working())
        .await?;
    live.retain(|m| tagged_with(m, &plan_id));
    live.sort_by_key(|m| m.created_at);
    let reason = format!("plan {plan_id} closed (plan-scoped note retired)");
    let mut out = Vec::with_capacity(live.len());
    for m in live {
        store.quarantine_memory(&m.id, &reason).await?;
        out.push(SettledNote { id: m.id, title: m.title, content: m.content });
    }
    Ok(out)
}

/// Markdown section for the closing report. Empty string when there are no
/// notes.
pub fn render_settled_notes(notes: &[SettledNote]) -> String {
    if notes.is_empty() {
        return String::new();
    }
    let mut s = String::from("## Plan notes (retired at close)\n\n");
    for n in notes {
        s.push_str(&format!("### {}\n\n{}\n\n", n.title.trim(), n.content.trim()));
    }
    s.push_str("Re-store any durable fact above without plan/branch references.\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{HashEmbedder, MemoryStore};
    use std::sync::Arc;

    #[test]
    fn plan_refs_positive() {
        assert_eq!(plan_refs("Plan 64694f06, branch x"), vec!["64694f06"]);
        assert_eq!(plan_refs("see plan 64694F06."), vec!["64694f06"]);
        assert_eq!(plan_refs("plan_id: 64694f06"), vec!["64694f06"]);
        assert_eq!(plan_refs("plan id=64694f06"), vec!["64694f06"]);
        assert_eq!(
            plan_refs("Plan #64694f06 and plan 11112222 and plan 64694f06"),
            vec!["64694f06", "11112222"]
        );
        assert_eq!(plan_refs("read .coding/plans/64694f06.md"), vec!["64694f06"]);
    }

    #[test]
    fn plan_refs_negative() {
        assert!(plan_refs("fixed in commit 64694f06").is_empty());
        assert!(plan_refs("the plan is good, deadbeef").is_empty());
        assert!(plan_refs("plan 64694f0").is_empty());
        assert!(plan_refs("plan 64694f067").is_empty());
        assert!(plan_refs("plan facedcab").is_empty(), "no digit => not an id");
    }

    #[test]
    fn branch_detection() {
        assert!(mentions_branch("Plan x, branch feature/pacman-game"));
        assert!(mentions_branch("branch: `fix/login`"));
        assert!(mentions_branch("lives in wt/mnemo"));
        assert!(!mentions_branch("branch protection is enabled"));
        assert!(!mentions_branch("the main branch is clean"));
    }

    #[test]
    fn scoped_tier_routes() {
        let s = scoped_tier(MemoryTier::Semantic, "SPEC: x", "Plan 64694f06 exports Game");
        assert_eq!(s.tier, MemoryTier::Episodic);
        assert_eq!(s.plan_ids, vec!["64694f06"]);
        let b = scoped_tier(MemoryTier::Semantic, "SPEC: x", "on branch feature/x");
        assert_eq!(b.tier, MemoryTier::Episodic);
        assert!(b.plan_ids.is_empty() && b.is_scoped());
        let p = scoped_tier(MemoryTier::Semantic, "SPEC: x", "Game is a named export");
        assert_eq!(p.tier, MemoryTier::Semantic);
        assert_eq!(p.data(), Value::Null);
        let w = scoped_tier(MemoryTier::Working, "t", "plan 64694f06");
        assert_eq!(w.tier, MemoryTier::Working);
        assert_eq!(w.plan_ids, vec!["64694f06"]);
    }

    fn store() -> MemoryStore {
        MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap()
    }

    fn tagged(tier: MemoryTier, title: &str, content: &str, plan: &str) -> Memory {
        let mut m = Memory::new(tier, title, content, 100);
        m.data = json!({"plan_scoped": true, "plan_ids": [plan], "branch": false});
        m
    }

    #[tokio::test]
    async fn settle_returns_and_retires_tagged_notes_only() {
        let s = store();
        s.write(tagged(MemoryTier::Episodic, "SPEC: a", "alpha pacman note", "64694f06"))
            .await
            .unwrap();
        s.write(tagged(MemoryTier::Procedural, "HOW: b", "beta pacman note", "64694f06"))
            .await
            .unwrap();
        s.write(tagged(MemoryTier::Episodic, "SPEC: c", "gamma pacman note", "aaaa1111"))
            .await
            .unwrap();
        s.write(Memory::new(MemoryTier::Semantic, "SPEC: d", "delta pacman fact", 100))
            .await
            .unwrap();

        let notes = settle_plan_memories(&s, "64694F06").await.unwrap();
        assert_eq!(notes.len(), 2);
        assert!(notes.iter().any(|n| n.title == "SPEC: a"));
        assert!(notes.iter().any(|n| n.title == "HOW: b"));

        let live = s.list_filtered(&MemoryFilter::new()).await.unwrap();
        let titles: Vec<_> = live.iter().map(|m| m.title.as_str()).collect();
        assert!(!titles.contains(&"SPEC: a") && !titles.contains(&"HOW: b"));
        assert!(titles.contains(&"SPEC: c") && titles.contains(&"SPEC: d"));
        let hits = s.recall("pacman", &MemoryFilter::new()).await.unwrap();
        assert!(hits.iter().all(|h| h.memory.title != "SPEC: a" && h.memory.title != "HOW: b"));

        // Idempotent; other plan untouched.
        assert!(settle_plan_memories(&s, "64694f06").await.unwrap().is_empty());
        assert_eq!(settle_plan_memories(&s, "aaaa1111").await.unwrap().len(), 1);
    }

    #[test]
    fn render_format() {
        assert_eq!(render_settled_notes(&[]), "");
        let out = render_settled_notes(&[SettledNote {
            id: "1".into(),
            title: "SPEC: a".into(),
            content: "body".into(),
        }]);
        assert!(out.starts_with("## Plan notes (retired at close)\n"));
        assert!(out.contains("### SPEC: a\n\nbody\n"));
        assert!(out
            .trim_end()
            .ends_with("Re-store any durable fact above without plan/branch references."));
    }
}
