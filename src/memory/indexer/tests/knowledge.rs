// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

use super::*;

// ── Knowledge corpus ────────────────────────────────────────────────────

fn write_knowledge(root: &Path, rel: &str, text: &str) {
    let path = root.join(".coding/knowledge").join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn knowledge_id(key: &str) -> String {
    Uuid::new_v5(&Uuid::NAMESPACE_URL, key.as_bytes()).to_string()
}

const KNOWLEDGE_DECISION: &str = "+++\ntitle = \"Typed records as files\"\ncreated = \"2026-08-23\"\n+++\n\nFiles are the truth. See [[spec/2026-08-23-branch-strategy]].\n";

#[tokio::test]
async fn knowledge_files_index_as_derived_typed_records() {
    let dir = tempdir().unwrap();
    write_knowledge(
        dir.path(),
        "decision/2026-08-23-typed-records.md",
        KNOWLEDGE_DECISION,
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

    let id = knowledge_id("knowledge:decision/2026-08-23-typed-records");
    let m = store
        .get_memory(&id)
        .await
        .unwrap()
        .expect("knowledge indexed");
    assert_eq!(m.record_class, MemoryClass::Derived);
    assert_eq!(m.record_type, MemoryRecordType::Decision);
    assert_eq!(m.title, "DECISION: Typed records as files");
    assert!(m.content.contains("Files are the truth."), "{}", m.content);
    assert!(
        m.content
            .contains("path .coding/knowledge/decision/2026-08-23-typed-records.md"),
        "{}",
        m.content
    );
    assert!(
        m.content.chars().count() <= 300,
        "DECISION digest fits its budget"
    );
    // The links data JSON rides along for the UI/backlink surface.
    assert_eq!(m.data["kind"], "knowledge");
    assert_eq!(m.data["rel_path"], "decision/2026-08-23-typed-records.md");
    assert_eq!(
        m.data["links"][0]["target"],
        "spec/2026-08-23-branch-strategy"
    );
    assert_eq!(m.superseded_by, None);

    // Second run: nothing changed → skip.
    let second = index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    assert_eq!(second.indexed, 0);
    assert_eq!(second.skipped, 1);
}

#[tokio::test]
async fn knowledge_supersede_chain_marks_predecessor() {
    let dir = tempdir().unwrap();
    let pred = "+++\ntitle = \"Old decision\"\nstatus = \"superseded\"\ncreated = \"2026-08-20\"\n+++\n\nThe old way.\n";
    let succ = "+++\ntitle = \"New decision\"\nsupersedes = \"2026-08-20-old-decision\"\ncreated = \"2026-08-23\"\n+++\n\nThe new way.\n";
    write_knowledge(dir.path(), "decision/2026-08-20-old-decision.md", pred);
    write_knowledge(dir.path(), "decision/2026-08-23-new-decision.md", succ);
    let store = make_store();
    index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();

    let pred_id = knowledge_id("knowledge:decision/2026-08-20-old-decision");
    let succ_id = knowledge_id("knowledge:decision/2026-08-23-new-decision");
    let m = store.get_memory(&pred_id).await.unwrap().unwrap();
    assert_eq!(m.superseded_by.as_deref(), Some(succ_id.as_str()));
    // The predecessor leaves recall; history stays reachable.
    let hits = store.recall("old way", &MemoryFilter::new()).await.unwrap();
    assert!(
        hits.iter().all(|sm| sm.memory.id != pred_id),
        "superseded predecessor excluded from recall"
    );
    let history = store
        .recall("old way", &MemoryFilter::new().include_superseded())
        .await
        .unwrap();
    assert!(
        history.iter().any(|sm| sm.memory.id == pred_id),
        "history reachable via include_superseded"
    );
    // The successor is live.
    let m = store.get_memory(&succ_id).await.unwrap().unwrap();
    assert_eq!(m.superseded_by, None);
}

#[tokio::test]
async fn superseded_without_successor_gets_the_sentinel() {
    let dir = tempdir().unwrap();
    let dead =
        "+++\ntitle = \"Dead spec\"\nstatus = \"superseded\"\n+++\n\nObsolete knowledge.\n";
    write_knowledge(dir.path(), "spec/2026-08-19-dead-spec.md", dead);
    let store = make_store();
    index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    let id = knowledge_id("knowledge:spec/2026-08-19-dead-spec");
    let m = store.get_memory(&id).await.unwrap().unwrap();
    assert_eq!(
        m.superseded_by.as_deref(),
        Some(crate::memory::knowledge::SUPERSEDED_SENTINEL)
    );
}

#[tokio::test]
async fn successor_added_later_supersedes_unchanged_predecessor() {
    // The git-merge convergence case: run 1 indexes a live predecessor;
    // run 2 adds the successor FILE (the predecessor is byte-identical →
    // skipped by the content-hash check). The reconciliation pass must
    // still flip the predecessor's superseded_by — this is exactly what
    // happens in the second worktree after a git merge.
    let dir = tempdir().unwrap();
    let pred_live =
        "+++\ntitle = \"Old decision\"\ncreated = \"2026-08-20\"\n+++\n\nThe old way.\n";
    write_knowledge(dir.path(), "decision/2026-08-20-old-decision.md", pred_live);
    let store = make_store();
    index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    let pred_id = knowledge_id("knowledge:decision/2026-08-20-old-decision");
    assert_eq!(
        store
            .get_memory(&pred_id)
            .await
            .unwrap()
            .unwrap()
            .superseded_by,
        None
    );

    let succ = "+++\ntitle = \"New decision\"\nsupersedes = \"2026-08-20-old-decision\"\ncreated = \"2026-08-23\"\n+++\n\nThe new way.\n";
    write_knowledge(dir.path(), "decision/2026-08-23-new-decision.md", succ);
    let second = index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    assert_eq!(second.indexed, 1, "only the successor (re)indexed");
    assert_eq!(second.skipped, 1, "the predecessor skipped on content hash");
    let succ_id = knowledge_id("knowledge:decision/2026-08-23-new-decision");
    let m = store.get_memory(&pred_id).await.unwrap().unwrap();
    assert_eq!(
        m.superseded_by.as_deref(),
        Some(succ_id.as_str()),
        "reconciliation flipped the unchanged predecessor"
    );
}

#[tokio::test]
async fn removed_knowledge_file_drops_its_row() {
    let dir = tempdir().unwrap();
    write_knowledge(
        dir.path(),
        "how/release-checklist.md",
        "# Release checklist\n\nRun the matrix.\n",
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
    let id = knowledge_id("knowledge:how/release-checklist");
    assert!(store.get_memory(&id).await.unwrap().is_some());

    std::fs::remove_file(
        dir.path()
            .join(".coding/knowledge/how/release-checklist.md"),
    )
    .unwrap();
    let second = index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    assert_eq!(second.removed, 1);
    assert!(store.get_memory(&id).await.unwrap().is_none());
    assert!(store
        .index_state_get("knowledge:how/release-checklist")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn reindex_knowledge_files_updates_just_the_given_files() {
    // The file-backed writer's post-write path: a targeted reindex must
    // (re)write exactly the touched files and leave everything else —
    // including the plans/reviews/backlog families and untouched
    // knowledge files — alone.
    let dir = tempdir().unwrap();
    write_knowledge(
        dir.path(),
        "decision/2026-08-20-old.md",
        "+++\ntitle = \"Old\"\n+++\n\nOld body.\n",
    );
    write_knowledge(
        dir.path(),
        "decision/2026-08-23-new.md",
        "+++\ntitle = \"New\"\n+++\n\nNew body.\n",
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

    // The file changes on disk (simulating a KnowledgeStore write).
    std::fs::write(
        dir.path()
            .join(".coding/knowledge/decision/2026-08-23-new.md"),
        "+++\ntitle = \"Newer\"\n+++\n\nUpdated body.\n",
    )
    .unwrap();
    let report = reindex_knowledge_files(
        store.as_ref(),
        &dir.path().join(".coding/knowledge"),
        &["decision/2026-08-23-new.md".to_string()],
    )
    .await
    .unwrap();
    assert_eq!(report.indexed, 1, "the changed file re-indexes");
    assert_eq!(report.skipped, 0);
    assert!(report.errors.is_empty(), "{:?}", report.errors);

    // The updated row carries the new title/content; the untouched
    // knowledge file's row is unchanged.
    let id = knowledge_id("knowledge:decision/2026-08-23-new");
    let m = store.get_memory(&id).await.unwrap().unwrap();
    assert_eq!(m.title, "DECISION: Newer");
    assert!(m.content.contains("Updated body."), "{}", m.content);
    let old_id = knowledge_id("knowledge:decision/2026-08-20-old");
    let old = store.get_memory(&old_id).await.unwrap().unwrap();
    assert!(old.content.contains("Old body."), "{}", old.content);

    // An idempotent second run skips (the state table now matches).
    let second = reindex_knowledge_files(
        store.as_ref(),
        &dir.path().join(".coding/knowledge"),
        &["decision/2026-08-23-new.md".to_string()],
    )
    .await
    .unwrap();
    assert_eq!(second.indexed, 0);
    assert_eq!(second.skipped, 1);
}

#[tokio::test]
async fn reindex_knowledge_files_resolves_supersedes_against_the_corpus() {
    // F1 regression: memory_update on a SUCCESSOR knowledge file
    // reindexes just the touched rel. `supersedes` must resolve against
    // the on-disk corpus (the predecessor is NOT in the reindexed
    // subset) — before the fix this errored "supersedes '<slug>' not
    // found" and left the successor row's digest stale until the next
    // startup content-hash reconciliation.
    let dir = tempdir().unwrap();
    write_knowledge(
        dir.path(),
        "decision/2026-08-20-old.md",
        "+++\ntitle = \"Old\"\n+++\n\nOld body.\n",
    );
    write_knowledge(
        dir.path(),
        "decision/2026-08-23-new.md",
        "+++\ntitle = \"New\"\nsupersedes = \"2026-08-20-old\"\n+++\n\nNew body.\n",
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

    // The successor file changes on disk (the memory_update write path).
    std::fs::write(
        dir.path()
            .join(".coding/knowledge/decision/2026-08-23-new.md"),
        "+++\ntitle = \"New\"\nsupersedes = \"2026-08-20-old\"\n+++\n\nUpdated body.\n",
    )
    .unwrap();
    let report = reindex_knowledge_files(
        store.as_ref(),
        &dir.path().join(".coding/knowledge"),
        &["decision/2026-08-23-new.md".to_string()],
    )
    .await
    .unwrap();

    // The F1 symptom: the targeted reindex errored because the
    // predecessor was not part of the reindexed subset.
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.indexed, 1, "the changed successor re-indexes");
    assert_eq!(
        report.skipped, 1,
        "the unchanged predecessor rides along as context"
    );

    // The successor row carries the fresh digest.
    let id = knowledge_id("knowledge:decision/2026-08-23-new");
    let m = store.get_memory(&id).await.unwrap().unwrap();
    assert!(m.content.contains("Updated body."), "{}", m.content);

    // The predecessor row keeps its supersede metadata — its content is
    // unchanged, so only the reconciliation re-asserts the pointer.
    let pred_id = knowledge_id("knowledge:decision/2026-08-20-old");
    let pred = store.get_memory(&pred_id).await.unwrap().unwrap();
    assert_eq!(
        pred.superseded_by.as_deref(),
        Some(id.as_str()),
        "predecessor still points at its successor"
    );
    assert!(pred.content.contains("Old body."), "{}", pred.content);
}

#[tokio::test]
async fn resolve_link_targets_records_and_files() {
    // The [[wiki-link]] resolver: knowledge/plan/review targets resolve
    // via deterministic ids to their rows; file-ish targets resolve to
    // the path; missing records degrade to path-only entries.
    let dir = tempdir().unwrap();
    write_knowledge(
        dir.path(),
        "decision/2026-08-23-typed-records.md",
        "+++\ntitle = \"Typed records as files\"\n+++\n\nFiles are the truth.\n",
    );
    write_plan(dir.path(), "p1", PLAN_A);
    let store = make_store();
    index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();

    // Knowledge target → row + title + snippet.
    let r = resolve_link(store.as_ref(), "decision/2026-08-23-typed-records")
        .await
        .unwrap()
        .expect("resolves");
    assert_eq!(r.kind, "knowledge");
    assert!(
        r.memory_id.is_some(),
        "the row id resolves deterministically"
    );
    assert_eq!(r.title.as_deref(), Some("DECISION: Typed records as files"));
    assert!(r.snippet.is_some());

    // Plan target → the plan row.
    let r = resolve_link(store.as_ref(), "plan/p1")
        .await
        .unwrap()
        .expect("resolves");
    assert_eq!(r.kind, "plan");
    assert_eq!(r.rel_path.as_deref(), Some(".coding/plans/p1.md"));

    // File-ish target → path-only, no row.
    let r = resolve_link(store.as_ref(), "src/main.rs")
        .await
        .unwrap()
        .expect("resolves");
    assert_eq!(r.kind, "file");
    assert_eq!(r.rel_path.as_deref(), Some("src/main.rs"));
    assert!(r.memory_id.is_none());

    // Empty target → None.
    assert!(resolve_link(store.as_ref(), "  ").await.unwrap().is_none());
}

#[tokio::test]
async fn corpus_is_stale_detects_changes_and_removals() {
    let dir = tempdir().unwrap();
    write_knowledge(
        dir.path(),
        "decision/2026-08-23-typed-records.md",
        "+++\ntitle = \"Typed records\"\n+++\n\nFiles are the truth.\n",
    );
    write_plan(dir.path(), "p1", PLAN_A);
    let store = make_store();
    index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();

    // Fresh index → not stale.
    assert!(
        !corpus_is_stale(
            store.as_ref(),
            &plans_dir(dir.path()),
            &backlog_path(dir.path())
        )
        .await
        .unwrap(),
        "a just-indexed corpus is not stale"
    );

    // A changed knowledge file → stale.
    std::fs::write(
        dir.path()
            .join(".coding/knowledge/decision/2026-08-23-typed-records.md"),
        "+++\ntitle = \"Typed records\"\n+++\n\nChanged body.\n",
    )
    .unwrap();
    assert!(
        corpus_is_stale(
            store.as_ref(),
            &plans_dir(dir.path()),
            &backlog_path(dir.path())
        )
        .await
        .unwrap(),
        "a changed file makes the corpus stale"
    );

    // Re-index → fresh again; a REMOVED file → stale.
    index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    std::fs::remove_file(
        dir.path()
            .join(".coding/knowledge/decision/2026-08-23-typed-records.md"),
    )
    .unwrap();
    assert!(
        corpus_is_stale(
            store.as_ref(),
            &plans_dir(dir.path()),
            &backlog_path(dir.path())
        )
        .await
        .unwrap(),
        "a removed file makes the corpus stale"
    );
}

#[tokio::test]
async fn corpus_is_stale_when_state_table_is_empty() {
    // A fresh store (no derived index state at all) is stale — the
    // startup reconcile must rebuild from the files (delete-to-rebuild
    // recovery: the DB was deleted, the files are the truth).
    let dir = tempdir().unwrap();
    write_knowledge(
        dir.path(),
        "how/release-checklist.md",
        "# Release checklist\n\nRun the matrix.\n",
    );
    let store = make_store();
    assert!(
        corpus_is_stale(
            store.as_ref(),
            &plans_dir(dir.path()),
            &backlog_path(dir.path())
        )
        .await
        .unwrap(),
        "no state table → stale (rebuild needed)"
    );
}

#[tokio::test]
async fn deleted_memory_db_rebuilds_everything_from_files() {
    // The delete-to-rebuild contract: `.coding/memory.db` is a gitignored
    // CACHE of the truth files. Deleting it and reopening the store must
    // rebuild the whole derived index (knowledge + plans + reviews +
    // backlog) from the on-disk corpus — nothing is lost.
    let dir = tempdir().unwrap();
    write_knowledge(
        dir.path(),
        "decision/2026-08-23-typed-records.md",
        "+++\ntitle = \"Typed records\"\n+++\n\nFiles are the truth.\n",
    );
    write_plan(dir.path(), "p1", PLAN_A);
    write_review(dir.path(), "r1", REVIEW_A);
    write_backlog(dir.path(), BACKLOG);

    // Phase 1: index into a file-backed store (simulating the project's
    // .coding/memory.db), then close it.
    let db_path = dir.path().join(".coding/memory.db");
    let store = {
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        Arc::new(crate::memory::MemoryStore::open(&db_path, embedder).unwrap())
    };
    index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    assert_eq!(
        store.count_by_class().await.unwrap().1,
        4,
        "plan + review + backlog + knowledge indexed"
    );
    drop(store);

    // Phase 2: the user deletes the DB (the documented rebuild path).
    std::fs::remove_file(&db_path).unwrap();

    // Phase 3: a fresh open + the startup reconcile rebuilds from files.
    let reopened = {
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        Arc::new(crate::memory::MemoryStore::open(&db_path, embedder).unwrap())
    };
    assert!(
        corpus_is_stale(
            reopened.as_ref(),
            &plans_dir(dir.path()),
            &backlog_path(dir.path())
        )
        .await
        .unwrap(),
        "a deleted DB is stale — the files are the truth"
    );
    let report = index_derived(
        reopened.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    assert_eq!(report.indexed, 4, "everything rebuilt from files");
    assert!(report.errors.is_empty(), "{:?}", report.errors);

    // The knowledge row is back, derived from the file (deterministic id).
    let id = knowledge_id("knowledge:decision/2026-08-23-typed-records");
    let m = reopened
        .get_memory(&id)
        .await
        .unwrap()
        .expect("knowledge row rebuilt");
    assert_eq!(m.record_class, MemoryClass::Derived);
    assert!(m.content.contains("Files are the truth."), "{}", m.content);
}

#[tokio::test]
async fn backlinks_for_finds_referencing_memories() {
    // A record that links to another record's slug shows up in the
    // reverse lookup; a record with no links never matches.
    let dir = tempdir().unwrap();
    write_knowledge(
        dir.path(),
        "spec/2026-08-23-branch-strategy.md",
        "+++\ntitle = \"Branch strategy\"\n+++\n\nWorking branches. [[decision/2026-08-23-merge-bundles]].\n",
    );
    write_knowledge(
        dir.path(),
        "decision/2026-08-23-merge-bundles.md",
        "+++\ntitle = \"Merge bundles\"\n+++\n\nBundles at merge time.\n",
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

    // The spec links the decision → the decision's backlinks include the
    // spec's row.
    let rows = backlinks_for(store.as_ref(), "decision/2026-08-23-merge-bundles")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "SPEC: Branch strategy");

    // No links → empty.
    let rows = backlinks_for(
        store.as_ref(),
        "decision/2026-08-23-merge-bundles.md", // .md variant matches too
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert!(backlinks_for(store.as_ref(), "how/nothing")
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn migrate_authored_typed_rows_writes_files_and_drops_rows() {
    // The one-time migration: authored typed rows (SPEC/DECISION/BUG/HOW
    // in distilled tiers) become knowledge FILES; the rows are deleted so
    // the derived index rebuilds them from the files. Rows without a
    // knowledge home (PLAN/REVIEW/untyped/working) stay put.
    let dir = tempdir().unwrap();
    let knowledge = Arc::new(crate::memory::KnowledgeStore::with_clock(
        dir.path().join(".coding/knowledge"),
        || 1_769_904_000, // 2026-02-01 UTC
    ));
    let store = make_store();
    // Migratable: typed + semantic/procedural + live.
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "DECISION: storage engine",
            "we use sqlite for storage",
            1_769_904_000,
        ))
        .await
        .unwrap();
    store
        .write(Memory::new(
            MemoryTier::Procedural,
            "HOW: run the test suite",
            "cargo test, then the matrix",
            1_769_904_000,
        ))
        .await
        .unwrap();
    // Staying: PLAN (no knowledge home), untyped, working-tier typed.
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "PLAN: some plan",
            "plan digest",
            1_769_904_000,
        ))
        .await
        .unwrap();
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "plain fact",
            "untyped stays",
            1_769_904_000,
        ))
        .await
        .unwrap();
    store
        .write(Memory::new(
            MemoryTier::Working,
            "BUG: raw event",
            "working tier stays",
            1_769_904_000,
        ))
        .await
        .unwrap();

    let report = migrate_authored_typed_rows(store.as_ref(), &knowledge)
        .await
        .unwrap();
    assert_eq!(report.migrated, 2, "both typed distilled rows migrated");
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    // The one-time guard marker is latched (plan step 7).
    assert!(
        dir.path()
            .join(".coding/knowledge/.migration-authored-v1")
            .exists(),
        "migration completes by writing the guard marker"
    );

    // Files exist with the deterministic slugs.
    let decision = std::fs::read_to_string(
        dir.path()
            .join(".coding/knowledge/decision/2026-02-01-storage-engine.md"),
    )
    .unwrap();
    assert!(decision.contains("we use sqlite for storage"), "{decision}");
    let how = std::fs::read_to_string(
        dir.path()
            .join(".coding/knowledge/how/2026-02-01-run-the-test-suite.md"),
    )
    .unwrap();
    assert!(how.contains("cargo test"), "{how}");

    // The rows are gone; the staying rows remain.
    let all = store
        .list_filtered(&MemoryFilter::new().include_working())
        .await
        .unwrap();
    assert_eq!(all.len(), 3, "PLAN + untyped + working stay");
    assert!(
        all.iter().all(|m| m.record_class == MemoryClass::Authored),
        "nothing derived was touched"
    );
    // The migrated knowledge is recallable as DERIVED rows after a
    // reindex (the startup reconcile does this next).
    index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    let id = knowledge_id("knowledge:decision/2026-02-01-storage-engine");
    let m = store
        .get_memory(&id)
        .await
        .unwrap()
        .expect("derived row from file");
    assert_eq!(m.record_class, MemoryClass::Derived);
    assert_eq!(m.title, "DECISION: storage engine");

    // Idempotent: a second run migrates nothing — now LATCHED by the
    // guard marker, even for rows that appeared after the first pass
    // (a future path re-inserting authored typed rows must clear the
    // marker or address the knowledge-file truth itself).
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "SPEC: late arrival",
            "this row appears after the migration ran",
            1_769_904_000,
        ))
        .await
        .unwrap();
    let report = migrate_authored_typed_rows(store.as_ref(), &knowledge)
        .await
        .unwrap();
    assert_eq!(report.migrated, 0, "latched: the guard skips the scan");
    let late = store
        .list_filtered(&MemoryFilter::new().record_type(MemoryRecordType::Spec))
        .await
        .unwrap();
    assert_eq!(late.len(), 1, "the late row stays (not silently migrated)");
}

#[tokio::test]
async fn lowered_config_budget_truncates_derived_digest() {
    // The digest budgets are now config-driven (wired to the store's
    // MemorySearchConfig): a lowered decision_budget must truncate the
    // derived digest to ≤ that length, while the pointer line survives.
    // (When the pointer itself exceeds the budget, budgeted_digest
    // returns just the pointer — pointer-first by construction.)
    let dir = tempdir().unwrap();
    let long_body = "x".repeat(2000);
    write_knowledge(
        dir.path(),
        "decision/2026-09-05-tiny-budget.md",
        &format!("+++\ntitle = \"Tiny\"\n+++\n\n{long_body}\n"),
    );
    let store = make_store();
    store.set_memory_search_config(crate::memory::MemorySearchConfig {
        decision_budget: 100,
        ..crate::memory::MemorySearchConfig::default()
    });
    index_derived(
        store.as_ref(),
        &plans_dir(dir.path()),
        &backlog_path(dir.path()),
        &noop_progress,
    )
    .await
    .unwrap();
    let id = knowledge_id("knowledge:decision/2026-09-05-tiny-budget");
    let m = store.get_memory(&id).await.unwrap().unwrap();
    assert!(
        m.content.chars().count() <= 100,
        "lowered budget truncates the digest: {} chars",
        m.content.chars().count()
    );
    assert!(
        !m.content.contains(&"x".repeat(100)),
        "the 2000-char body was truncated away"
    );
    assert!(
        m.content
            .contains("path .coding/knowledge/decision/2026-09-05-tiny-budget.md"),
        "pointer line survives truncation: {}",
        m.content
    );
}
