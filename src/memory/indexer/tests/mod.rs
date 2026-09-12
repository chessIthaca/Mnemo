// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

use super::*;
use crate::memory::{Embedder, HashEmbedder, MemoryFilter, MemoryRecordType, MemoryStore};
use std::sync::Arc;
use std::sync::Mutex;
use tempfile::tempdir;

fn make_store() -> Arc<MemoryStore> {
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::new());
    Arc::new(MemoryStore::open_in_memory(embedder).unwrap())
}

fn plans_dir(root: &Path) -> std::path::PathBuf {
    root.join(".coding/plans")
}

fn backlog_path(root: &Path) -> std::path::PathBuf {
    root.join(".coding/backlog.jsonl")
}

fn write_plan(root: &Path, name: &str, text: &str) {
    let dir = plans_dir(root);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{name}.md")), text).unwrap();
}

fn write_review(root: &Path, name: &str, text: &str) {
    let dir = root.join(".coding/reviews");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{name}.md")), text).unwrap();
}

fn write_backlog(root: &Path, json: &str) {
    let dir = root.join(".coding");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("backlog.jsonl"), json).unwrap();
}

fn noop_progress(_: usize, _: usize) {}

const PLAN_A: &str = "# Plan: Add feature X\n\n## Goal\nShip the X feature safely.\n\n## Kind\nimplementation\n\n## Context\ncommit fa9dcdd landed the base\n\n## Steps\n- [x] **One** — done\n- [ ] **Two** — todo\n";

const REVIEW_A: &str = "# Review: Storage layer refactor\n\n**Date:** 2026-08-01\n\n## Findings\n\n### High — connection leak\n\ndetails\n\n### Low — naming\n\ndetails\n";

/// jsonl backlog: one JSON item per line. Ids are UUID strings now.
const BACKLOG: &str = "{\"id\":\"b-0001\",\"text\":\"Fix the login crash\\nmore detail\",\"images\":[],\"status\":\"pending\",\"created_at\":1,\"note\":null}\n{\"id\":\"b-0002\",\"text\":\"Done thing\",\"images\":[],\"status\":\"done\",\"created_at\":2,\"note\":null}\n";

#[tokio::test]
async fn indexes_plans_reviews_and_pending_backlog_items() {
    let dir = tempdir().unwrap();
    write_plan(dir.path(), "p1", PLAN_A);
    write_review(dir.path(), "r1", REVIEW_A);
    write_backlog(dir.path(), BACKLOG);
    let store = make_store();
    let report = index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    assert_eq!(report.indexed, 3);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.removed, 0);
    assert!(report.errors.is_empty(), "{:?}", report.errors);

    // Plan: deterministic id, derived class, typed record, budgeted
    // digest with the pointer + the plan-referenced commit.
    let plan_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, b"plan:p1").to_string();
    let m = store
        .get_memory(&plan_id)
        .await
        .unwrap()
        .expect("plan indexed");
    assert_eq!(m.record_class, MemoryClass::Derived);
    assert_eq!(m.record_type, MemoryRecordType::Plan);
    assert_eq!(m.title, "PLAN: Add feature X");
    assert!(
        m.content.contains("Ship the X feature safely."),
        "{}",
        m.content
    );
    assert!(m.content.contains("1/2 steps"), "{}", m.content);
    assert!(
        m.content.contains("path .coding/plans/p1.md"),
        "{}",
        m.content
    );
    assert!(m.content.contains("commit fa9dcdd"), "{}", m.content);

    // Review: verdict-less legacy report falls back to a findings count.
    let review_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, b"review:r1").to_string();
    let m = store
        .get_memory(&review_id)
        .await
        .unwrap()
        .expect("review indexed");
    assert_eq!(m.record_type, MemoryRecordType::Review);
    assert_eq!(m.title, "REVIEW: r1");
    assert!(
        m.content.contains("Storage layer refactor"),
        "{}",
        m.content
    );
    assert!(m.content.contains("2 finding sections"), "{}", m.content);
    assert!(
        m.content.contains("path .coding/reviews/r1.md"),
        "{}",
        m.content
    );

    // Only the PENDING backlog item was indexed.
    let pending_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, b"backlog:b-0001").to_string();
    let m = store
        .get_memory(&pending_id)
        .await
        .unwrap()
        .expect("pending indexed");
    assert!(m.content.contains("Fix the login crash"), "{}", m.content);
    assert!(m.content.contains("status pending"), "{}", m.content);
    let done_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, b"backlog:b-0002").to_string();
    assert!(store.get_memory(&done_id).await.unwrap().is_none());
}

/// Backlog f2d2809b (user request 2027-01-07): backlog.jsonl lines now
/// carry an optional `deferred` field — the indexer's minimal
/// `BacklogEntry` parser must tolerate it (unknown fields are ignored)
/// and still index the pending item.
#[tokio::test]
async fn backlog_lines_with_deferred_field_still_index() {
    let dir = tempdir().unwrap();
    write_backlog(
        dir.path(),
        "{\"id\":\"b-d1\",\"text\":\"Parked item\\nbody\",\"images\":[],\"status\":\"pending\",\"created_at\":1,\"note\":null,\"deferred\":true}\n",
    );
    let store = make_store();
    let report = index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    assert_eq!(report.indexed, 1);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let id = Uuid::new_v5(&Uuid::NAMESPACE_URL, b"backlog:b-d1").to_string();
    let m = store
        .get_memory(&id)
        .await
        .unwrap()
        .expect("deferred item indexed");
    assert_eq!(m.record_type, MemoryRecordType::Plan);
    assert!(m.content.contains("Parked item"), "{}", m.content);
}

#[tokio::test]
async fn soft_deleted_backlog_item_is_not_indexed() {
    // A soft-deleted item (deleted_at set) is invisible work — even with
    // status "pending" it must not be indexed as a live PLAN record.
    let dir = tempdir().unwrap();
    let deleted = r#"{"id":"b-0003","text":"Deleted work","images":[],"status":"pending","created_at":3,"note":null,"deleted_at":999}"#;
    write_backlog(dir.path(), &format!("{BACKLOG}{deleted}\n"));
    let store = make_store();
    let report = index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    // Only the live pending item (b-0001) is indexed.
    assert_eq!(report.indexed, 1);
    let deleted_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, b"backlog:b-0003").to_string();
    assert!(store.get_memory(&deleted_id).await.unwrap().is_none());
}

#[tokio::test]
async fn second_run_skips_unchanged_sources() {
    let dir = tempdir().unwrap();
    write_plan(dir.path(), "p1", PLAN_A);
    write_review(dir.path(), "r1", REVIEW_A);
    write_backlog(dir.path(), BACKLOG);
    let store = make_store();
    index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    let second = index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    assert_eq!(second.indexed, 0, "unchanged sources must skip");
    assert_eq!(second.skipped, 3);
    assert_eq!(second.removed, 0);
}

#[tokio::test]
async fn changed_plan_reindexes_in_place() {
    let dir = tempdir().unwrap();
    write_plan(dir.path(), "p1", PLAN_A);
    write_review(dir.path(), "r1", REVIEW_A);
    let store = make_store();
    index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    let plan_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, b"plan:p1").to_string();

    // Change the goal → the SAME memory id is updated in place.
    let updated = PLAN_A.replace("Ship the X feature safely.", "Ship the Y feature boldly.");
    write_plan(dir.path(), "p1", &updated);
    let second = index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    assert_eq!(second.indexed, 1);
    assert_eq!(second.skipped, 1);
    let m = store
        .get_memory(&plan_id)
        .await
        .unwrap()
        .expect("plan survives");
    assert!(
        m.content.contains("Ship the Y feature boldly."),
        "{}",
        m.content
    );
    assert!(!m.content.contains("safely"), "{}", m.content);
}

#[tokio::test]
async fn deleted_source_removes_record_and_state() {
    let dir = tempdir().unwrap();
    write_plan(dir.path(), "p1", PLAN_A);
    write_review(dir.path(), "r1", REVIEW_A);
    let store = make_store();
    index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    let review_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, b"review:r1").to_string();
    assert!(store.get_memory(&review_id).await.unwrap().is_some());

    std::fs::remove_file(dir.path().join(".coding/reviews/r1.md")).unwrap();
    let second = index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    assert_eq!(second.removed, 1);
    assert!(store.get_memory(&review_id).await.unwrap().is_none());
    assert!(store.index_state_get("review:r1").await.unwrap().is_none());
}

#[tokio::test]
async fn legacy_plan_without_structured_sections_still_indexes() {
    let dir = tempdir().unwrap();
    write_plan(
        dir.path(),
        "legacy",
        "# Old Plan\n\nsome free text about the work\n",
    );
    let store = make_store();
    let report = index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    assert_eq!(report.indexed, 1);
    let id = Uuid::new_v5(&Uuid::NAMESPACE_URL, b"plan:legacy").to_string();
    let m = store
        .get_memory(&id)
        .await
        .unwrap()
        .expect("legacy plan indexed");
    assert_eq!(m.title, "PLAN: Old Plan");
    assert!(
        m.content.contains("some free text about the work"),
        "{}",
        m.content
    );
    assert!(
        m.content.contains("path .coding/plans/legacy.md"),
        "{}",
        m.content
    );
}

#[tokio::test]
async fn digests_always_fit_their_typed_budget() {
    let dir = tempdir().unwrap();
    let long_goal = "g".repeat(2000);
    let plan = format!("# Plan: Huge\n\n## Goal\n{long_goal}\n\n## Steps\n- [ ] x\n");
    write_plan(dir.path(), "huge", &plan);
    let long_heading = "h".repeat(2000);
    write_review(
        dir.path(),
        "big",
        &format!("# Review: {long_heading}\n\nbody\n"),
    );
    let store = make_store();
    index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();

    let plan_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, b"plan:huge").to_string();
    let m = store.get_memory(&plan_id).await.unwrap().unwrap();
    assert!(
        m.content.chars().count() <= 400,
        "PLAN digest fits its budget: {} chars",
        m.content.chars().count()
    );
    assert!(
        m.content.contains("path .coding/plans/huge.md"),
        "{}",
        m.content
    );

    let review_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, b"review:big").to_string();
    let m = store.get_memory(&review_id).await.unwrap().unwrap();
    assert!(
        m.content.chars().count() <= 500,
        "REVIEW digest fits its budget: {} chars",
        m.content.chars().count()
    );
    assert!(
        m.content.contains("path .coding/reviews/big.md"),
        "{}",
        m.content
    );
}

#[tokio::test]
async fn progress_ticks_to_total() {
    let dir = tempdir().unwrap();
    write_plan(dir.path(), "p1", PLAN_A);
    write_review(dir.path(), "r1", REVIEW_A);
    write_backlog(dir.path(), BACKLOG);
    let store = make_store();
    let ticks = Mutex::new(Vec::new());
    let progress = |done: usize, total: usize| ticks.lock().unwrap().push((done, total));
    index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &progress,
    )
    .await
    .unwrap();
    let ticks = ticks.lock().unwrap();
    assert_eq!(ticks.len(), 3);
    assert_eq!(ticks.last(), Some(&(3, 3)));
}

#[tokio::test]
async fn rebuild_derived_replaces_derived_and_preserves_authored() {
    // The rebuild wipes + re-scans derived records; authored memories are
    // sacred and must survive untouched (they have no other home).
    let dir = tempdir().unwrap();
    write_plan(dir.path(), "p1", PLAN_A);
    let store = make_store();
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "authored fact",
            "handwritten knowledge",
            1000,
        ))
        .await
        .unwrap();
    index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();

    let report = rebuild_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    // The state table was cleared, so the plan re-indexes from scratch.
    assert_eq!(report.indexed, 1);
    assert_eq!(report.skipped, 0);
    let (authored, derived) = store.count_by_class().await.unwrap();
    assert_eq!((authored, derived), (1, 1));
    let hits = store
        .recall("handwritten", &crate::memory::MemoryFilter::new())
        .await
        .unwrap();
    assert!(
        hits.iter().any(|sm| sm.memory.title == "authored fact"),
        "authored memory must survive the rebuild"
    );
}

mod knowledge;
