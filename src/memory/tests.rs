use super::*;
use crate::memory::embedder::HashEmbedder;
use std::sync::atomic::{AtomicI64, Ordering};

fn make_store() -> MemoryStore {
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::new());
    let now_val = Arc::new(AtomicI64::new(1000));
    MemoryStore::open_in_memory_with_clock(embedder, move || {
        now_val.fetch_add(1, Ordering::SeqCst) + 1
    })
    .unwrap()
}

/// Deterministic anchor-word embedder for recall tests: each word maps to
/// a fixed unit axis (synonym pairs share one), so a query and a memory
/// can be vector-aligned while sharing ZERO tokens — constructing the
/// zero-keyword-hit case FTS cannot see. `model_id` is configurable so
/// tests can exercise the embedder-aware sim weight ("hash" vs a real
/// embedding-model id). Note the write path embeds CONTENT only
/// (mod.rs write), titles ride the FTS index + substring boost.
struct AnchorEmbedder {
    model_id: &'static str,
}

impl AnchorEmbedder {
    /// Word → unit axis. Synonym pairs ("pipeline"/"conduit",
    /// "transpiler"/"compiler") share an axis: aligned vectors, zero
    /// token overlap.
    fn axis_for(word: &str) -> usize {
        match word {
            "pipeline" | "conduit" => 0,
            "transpiler" | "compiler" => 1,
            "alpha" => 2,
            "unrelated" => 5,
            _ => 4,
        }
    }
}

#[async_trait::async_trait]
impl Embedder for AnchorEmbedder {
    async fn embed(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; crate::memory::embedder::EMBEDDING_DIM];
        for word in text.split_whitespace() {
            v[Self::axis_for(word)] += 1.0;
        }
        v
    }
    fn model_id(&self) -> &str {
        self.model_id
    }
}

/// Open an in-memory store with an [`AnchorEmbedder`] and the standard
/// test clock.
fn make_anchor_store(model_id: &'static str) -> MemoryStore {
    let embedder: Arc<dyn Embedder> = Arc::new(AnchorEmbedder { model_id });
    let now_val = Arc::new(AtomicI64::new(1000));
    MemoryStore::open_in_memory_with_clock(embedder, move || {
        now_val.fetch_add(1, Ordering::SeqCst) + 1
    })
    .unwrap()
}

#[test]
fn embedder_handle_recovers_from_a_poisoned_lock() {
    // The failure-triage kNN overlay reads the live embedder through this
    // getter on a failure-handling path — a poisoned lock (some other
    // holder panicked mid-swap) must yield a valid embedder, never panic
    // the turn. The guarded field is a plain `Arc` swap, so recovering the
    // guard from the poison error is sound.
    let store = make_store();
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = store.embedder.write().expect("write guard for the poison");
        panic!("poison the embedder lock");
    }));
    assert!(poisoned.is_err(), "the guard scope must have panicked");
    let handle = store.embedder_handle();
    assert_eq!(handle.model_id(), "hash");
}

#[tokio::test]
async fn recall_semantic_fallback_on_zero_keyword_hits() {
    // Regression (backlog 82b831dc, 2026-12-06): zero FTS keyword hits
    // used to return EMPTY — the embeddings never ran, which is exactly
    // the case they were bought for. The NoMatches arm now falls through
    // to the capped semantic scan: the vector-aligned memory surfaces and
    // ranks first even though the query shares no token with it.
    // Fixture vocabularies are fully disjoint (gotcha: any shared token
    // defeats the zero-keyword precondition).
    let store = make_anchor_store("anchor-test");
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "waterfall chart",
            "unrelated unrelated unrelated",
            1000,
        ))
        .await
        .unwrap();
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "conduit stages",
            "conduit conduit conduit",
            1000,
        ))
        .await
        .unwrap();

    // "pipeline" shares no token with either memory (conduit/unrelated/
    // waterfall/chart/stages) → FTS NoMatches → the semantic scan runs.
    let results = store
        .recall("pipeline", &MemoryFilter::new())
        .await
        .unwrap();
    assert!(
        !results.is_empty(),
        "zero-keyword query must fall through to the capped semantic scan"
    );
    assert_eq!(
        results[0].memory.title, "conduit stages",
        "the vector-aligned memory (cosine 1.0) must rank first"
    );
}

#[tokio::test]
async fn sim_weight_follows_embedder_model() {
    // The cosine weight is embedder-aware: under a real embedding model
    // (model_id != "hash" → 0.35) a strong semantic match (sim ≈ 0.95)
    // outweighs a bare keyword-phrase hit (0.3); under the hash model_id
    // (0.2) it does not. Same fixture under both embedders — only the
    // weight differs, so the first-place memory flips.
    //
    // Fixture (query "pipeline pipeline pipeline transpiler" → vector
    // 3·e0 + e1):
    // - M_sim: FTS-matches via its title token, content embeds as pure
    //   e0 ("conduit") → sim = 3/√10 ≈ 0.949, no substring boost (the
    //   full query phrase is not in its title).
    // - M_kw: title IS the full query phrase (substring boost 0.3),
    //   content embeds as pure e5 ("unrelated") → sim ≈ 0.
    async fn first_title(model_id: &'static str) -> String {
        let store = make_anchor_store(model_id);
        store
            .write(Memory::new(
                MemoryTier::Semantic,
                "pipeline pipeline pipeline transpiler",
                "unrelated unrelated unrelated",
                1000,
            ))
            .await
            .unwrap();
        store
            .write(Memory::new(
                MemoryTier::Semantic,
                "pipeline notes",
                "conduit conduit conduit",
                1000,
            ))
            .await
            .unwrap();
        let results = store
            .recall(
                "pipeline pipeline pipeline transpiler",
                &MemoryFilter::new(),
            )
            .await
            .unwrap();
        assert!(!results.is_empty());
        results[0].memory.title.clone()
    }

    // Real embedding model: sim 0.949 × 0.35 ≈ 0.332 > keyword 0.3.
    assert_eq!(
        first_title("anchor-test").await,
        "pipeline notes",
        "under a real embedder the strong semantic match must rank first"
    );
    // Hash model_id: sim 0.949 × 0.2 ≈ 0.19 < keyword 0.3.
    assert_eq!(
        first_title("hash").await,
        "pipeline pipeline pipeline transpiler",
        "under the hash embedder the keyword hit must keep rank"
    );
}

#[tokio::test]
async fn count_by_class_distinguishes_authored_and_derived() {
    let store = make_store();
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "authored fact",
            "c",
            1000,
        ))
        .await
        .unwrap();
    let mut derived = Memory::new(MemoryTier::Semantic, "PLAN: digest", "c", 1000);
    derived.record_class = MemoryClass::Derived;
    store.write(derived).await.unwrap();

    let (authored, derived) = store.count_by_class().await.unwrap();
    assert_eq!((authored, derived), (1, 1));
}

#[tokio::test]
async fn delete_derived_wipes_only_derived_rows() {
    // The rebuild's wipe step must NEVER touch authored rows — they are
    // sacred (a rebuild regenerates derived records from on-disk truth;
    // authored knowledge has no other home).
    let store = make_store();
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "authored fact",
            "the login route lives in routes.rs",
            1000,
        ))
        .await
        .unwrap();
    let mut derived = Memory::new(
        MemoryTier::Semantic,
        "PLAN: digest",
        "plan digest content",
        1000,
    );
    derived.record_class = MemoryClass::Derived;
    let derived_id = derived.id.clone();
    store.write(derived).await.unwrap();

    let deleted = store.delete_derived().await.unwrap();
    assert_eq!(deleted, 1);
    // Derived row is gone (even from a point lookup); authored row
    // survives and still recalls.
    assert!(store.get_memory(&derived_id).await.unwrap().is_none());
    let hits = store
        .recall("login route", &MemoryFilter::new())
        .await
        .unwrap();
    assert!(
        hits.iter().any(|sm| sm.memory.title == "authored fact"),
        "authored row must survive delete_derived"
    );
    let (authored, derived) = store.count_by_class().await.unwrap();
    assert_eq!((authored, derived), (1, 0));
}

#[tokio::test]
async fn get_memory_roundtrips_and_misses() {
    let store = make_store();
    let id = store
        .write(Memory::new(
            MemoryTier::Semantic,
            "BUG: crash",
            "root cause: unwrap on None",
            1000,
        ))
        .await
        .unwrap();
    let m = store.get_memory(&id).await.unwrap().expect("row exists");
    assert_eq!(m.title, "BUG: crash");
    assert_eq!(m.record_type, MemoryRecordType::Bug);
    assert!(store.get_memory("no-such-id").await.unwrap().is_none());
}

#[tokio::test]
async fn recall_honors_the_configured_per_query_cap() {
    // With no explicit filter limit, the store's [memory] config caps the
    // result set (Settings-tunable scale knob).
    let store = make_store();
    for i in 0..6 {
        store
            .write(Memory::new(
                MemoryTier::Semantic,
                format!("login fact {i}"),
                "login route detail",
                1000,
            ))
            .await
            .unwrap();
    }
    store.set_memory_search_config(MemorySearchConfig {
        per_query_cap: 3,
        ..MemorySearchConfig::default()
    });
    let hits = store.recall("login", &MemoryFilter::new()).await.unwrap();
    assert_eq!(hits.len(), 3, "capped by the configured per-query cap");
    // An explicit filter limit still wins over the knob.
    let hits = store
        .recall("login", &MemoryFilter::new().limit(5))
        .await
        .unwrap();
    assert_eq!(hits.len(), 5, "explicit limit wins over the cap");
}

#[tokio::test]
async fn recall_caps_derived_records_per_class_but_never_authored() {
    // The scale backstop: a large indexed corpus must not crowd authored
    // knowledge out of a result set — derived records are capped per
    // record type, authored records are exempt.
    let store = make_store();
    for i in 0..4 {
        let mut m = Memory::new(
            MemoryTier::Semantic,
            format!("PLAN: digest {i}"),
            "plan digest about storage",
            1000,
        );
        m.record_class = MemoryClass::Derived;
        store.write(m).await.unwrap();
    }
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "PLAN: authored storage plan",
            "authored storage plan knowledge",
            1000,
        ))
        .await
        .unwrap();
    store.set_memory_search_config(MemorySearchConfig {
        derived_per_class_cap: 2,
        ..MemorySearchConfig::default()
    });
    let hits = store.recall("storage", &MemoryFilter::new()).await.unwrap();
    let derived_hits = hits
        .iter()
        .filter(|sm| sm.memory.record_class == MemoryClass::Derived)
        .count();
    assert!(derived_hits <= 2, "derived capped at 2, got {derived_hits}");
    assert!(
        hits.iter()
            .any(|sm| sm.memory.title == "PLAN: authored storage plan"),
        "the authored record is never capped out"
    );
}

#[tokio::test]
async fn index_state_helpers_roundtrip_remove_clear_and_last_built() {
    let store = make_store();
    // Empty at first.
    assert!(store.index_state_get("plan:a").await.unwrap().is_none());
    assert!(store.index_state_last_built().await.unwrap().is_none());
    assert!(store.index_state_keys().await.unwrap().is_empty());

    // Upsert two keys; read them back.
    store
        .index_state_upsert("plan:a", "hash1", "mem-1")
        .await
        .unwrap();
    store
        .index_state_upsert("review:r1", "hash2", "mem-2")
        .await
        .unwrap();
    let (hash, mem) = store.index_state_get("plan:a").await.unwrap().unwrap();
    assert_eq!((hash.as_str(), mem.as_str()), ("hash1", "mem-1"));
    // Upsert is idempotent by key (refreshes hash + memory id).
    store
        .index_state_upsert("plan:a", "hash1b", "mem-1")
        .await
        .unwrap();
    let (hash, _) = store.index_state_get("plan:a").await.unwrap().unwrap();
    assert_eq!(hash, "hash1b");

    // Keys + last_built: the deterministic clock increments per call, so
    // the second upsert carries the later timestamp.
    let mut keys = store.index_state_keys().await.unwrap();
    keys.sort();
    assert_eq!(keys, ["plan:a", "review:r1"]);
    assert!(store.index_state_last_built().await.unwrap().is_some());

    // Remove one key; clear wipes the rest.
    store.index_state_remove("review:r1").await.unwrap();
    assert!(store.index_state_get("review:r1").await.unwrap().is_none());
    assert_eq!(store.index_state_keys().await.unwrap(), ["plan:a"]);
    store.index_state_clear().await.unwrap();
    assert!(store.index_state_keys().await.unwrap().is_empty());
    assert!(store.index_state_last_built().await.unwrap().is_none());
}

#[tokio::test]
async fn set_derived_metadata_touches_only_derived_rows() {
    // The knowledge-indexer reconciliation write is class-scoped: an
    // authored row is refused (false + untouched); a derived row takes
    // both fields and clears back to None.
    let store = make_store();
    let authored_id = store
        .write(Memory::new(
            MemoryTier::Semantic,
            "authored fact",
            "c",
            1000,
        ))
        .await
        .unwrap();
    let mut derived = Memory::new(MemoryTier::Semantic, "SPEC: digest", "c", 1000);
    derived.record_class = MemoryClass::Derived;
    let derived_id = derived.id.clone();
    store.write(derived).await.unwrap();

    // Authored row: refused and unchanged.
    let touched = store
        .set_derived_metadata(&authored_id, Some("x"), &serde_json::json!({"k": 1}))
        .await
        .unwrap();
    assert!(!touched, "authored rows are out of bounds");
    let m = store.get_memory(&authored_id).await.unwrap().unwrap();
    assert_eq!(m.superseded_by, None);

    // Derived row: both fields land; clearing back to None works.
    let touched = store
        .set_derived_metadata(
            &derived_id,
            Some("succ"),
            &serde_json::json!({"kind": "knowledge"}),
        )
        .await
        .unwrap();
    assert!(touched);
    let m = store.get_memory(&derived_id).await.unwrap().unwrap();
    assert_eq!(m.superseded_by.as_deref(), Some("succ"));
    assert_eq!(m.data["kind"], "knowledge");
    assert!(store
        .set_derived_metadata(&derived_id, None, &serde_json::Value::Null)
        .await
        .unwrap());
    let m = store.get_memory(&derived_id).await.unwrap().unwrap();
    assert_eq!(m.superseded_by, None);
}

#[tokio::test]
async fn access_log_records_reads_and_writes() {
    let store = make_store();
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "auth fact",
            "the login route is defined in routes.rs",
            1000,
        ))
        .await
        .unwrap();
    let hits = store
        .recall(
            "login route",
            &MemoryFilter::new().tier(MemoryTier::Semantic),
        )
        .await
        .unwrap()
        .len();

    let log = store.access_log();
    assert_eq!(log.len(), 2, "one write + one read entry");
    // Newest-first: the recall (later) is at index 0.
    assert_eq!(log[0].op, "read");
    assert_eq!(log[0].detail, "login route");
    assert_eq!(log[0].tier.as_deref(), Some("semantic"));
    assert_eq!(log[0].hits, Some(hits));
    assert!(log[0].at >= 1000, "timestamp from the store clock");
    assert_eq!(log[1].op, "write");
    assert_eq!(log[1].detail, "auth fact");
    assert_eq!(log[1].tier.as_deref(), Some("semantic"));
    assert_eq!(log[1].hits, None, "writes carry no hit count");
}

#[tokio::test]
async fn access_log_caps_at_100_evicting_oldest() {
    let store = make_store();
    for i in 0..105 {
        store
            .write(Memory::new(
                MemoryTier::Working,
                format!("t{i:03}"),
                "c",
                1000,
            ))
            .await
            .unwrap();
    }
    let log = store.access_log();
    assert_eq!(log.len(), 100, "capped at MAX_ACCESS_LOG_ENTRIES");
    // Newest-first: the oldest survivor is write #5, the newest #104.
    assert_eq!(log.last().unwrap().detail, "t005");
    assert_eq!(log[0].detail, "t104");
}

#[tokio::test]
async fn access_log_records_session_primer_reads() {
    let store = make_store();
    store
        .write(Memory::new(MemoryTier::Semantic, "f", "content", 1000))
        .await
        .unwrap();
    store
        .strongest(&[MemoryTier::Semantic, MemoryTier::Procedural], 8)
        .await
        .unwrap();

    let log = store.access_log();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].op, "read");
    assert_eq!(log[0].detail, "session primer");
    assert_eq!(log[0].tier, None, "the primer reads across tiers");
    assert_eq!(log[0].hits, Some(1));
}

#[tokio::test]
async fn access_log_detail_is_char_capped() {
    let store = make_store();
    // 130 multibyte chars — a byte-slice cap would split the final char.
    let title: String = "é".repeat(130);
    store
        .write(Memory::new(MemoryTier::Semantic, title, "c", 1000))
        .await
        .unwrap();
    let log = store.access_log();
    assert_eq!(log[0].detail.chars().count(), 120);
    assert!(
        log[0].detail.chars().all(|c| c == 'é'),
        "truncation landed on a char boundary"
    );
}

#[tokio::test]
async fn write_and_recall_working() {
    // Working memories are excluded from recall by default (they are
    // consolidation input, not recall output). Opt in via include_working
    // to recall raw tool events.
    let store = make_store();
    let mem = Memory::new(
        MemoryTier::Working,
        "read routes.rs",
        "the routes file defines /login and /logout",
        1000,
    );
    let id = store.write(mem).await.unwrap();
    assert!(!id.is_empty());

    // Default filter excludes working → empty.
    let results = store
        .recall("login route", &MemoryFilter::new())
        .await
        .unwrap();
    assert!(results.is_empty(), "working excluded by default");

    // Opting in recalls the working memory.
    let results = store
        .recall("login route", &MemoryFilter::new().include_working())
        .await
        .unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].memory.title, "read routes.rs");
}

#[tokio::test]
async fn recall_excludes_working_by_default() {
    // A working + a semantic memory sharing a keyword. The default
    // filter (exclude_working = true) must return only the semantic one.
    let store = make_store();
    store
        .write(Memory::new(
            MemoryTier::Working,
            "tool: search",
            "the login route is defined in routes.rs",
            1000,
        ))
        .await
        .unwrap();
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "auth fact",
            "the login route is defined in routes.rs",
            1000,
        ))
        .await
        .unwrap();

    let results = store
        .recall("login route", &MemoryFilter::new())
        .await
        .unwrap();
    assert_eq!(results.len(), 1, "only the semantic memory returns");
    assert_eq!(results[0].memory.tier, MemoryTier::Semantic);
}

#[tokio::test]
async fn recall_filters_by_tier() {
    let store = make_store();
    let working = Memory::new(MemoryTier::Working, "w", "login route content", 1000);
    let semantic = Memory::new(MemoryTier::Semantic, "s", "login route fact", 1000);
    store.write(working).await.unwrap();
    store.write(semantic).await.unwrap();

    let only_working = store
        .recall(
            "login route",
            &MemoryFilter::new().tier(MemoryTier::Working),
        )
        .await
        .unwrap();
    assert!(only_working
        .iter()
        .all(|m| m.memory.tier == MemoryTier::Working));

    let only_semantic = store
        .recall(
            "login route",
            &MemoryFilter::new().tier(MemoryTier::Semantic),
        )
        .await
        .unwrap();
    assert!(only_semantic
        .iter()
        .all(|m| m.memory.tier == MemoryTier::Semantic));
}

#[tokio::test]
async fn recall_applies_limit() {
    let store = make_store();
    for i in 0..10 {
        store
            .write(Memory::new(
                MemoryTier::Semantic,
                format!("mem {i}"),
                "shared content about routes",
                1000,
            ))
            .await
            .unwrap();
    }
    let results = store
        .recall("routes", &MemoryFilter::new().limit(3))
        .await
        .unwrap();
    assert_eq!(results.len(), 3);
}

#[tokio::test]
async fn access_bumps_count() {
    let store = make_store();
    let id = store
        .write(Memory::new(
            MemoryTier::Working,
            "t",
            "some content about auth",
            1000,
        ))
        .await
        .unwrap();
    store.access(&id).await.unwrap();
    store.access(&id).await.unwrap();

    let list = store.list_by_tier(MemoryTier::Working).await.unwrap();
    let m = list.iter().find(|m| m.id == id).unwrap();
    assert_eq!(m.access_count, 2);
}

#[tokio::test]
async fn batch_access_bumps_all_in_one_query() {
    // recall() now uses batch_access instead of a per-result loop. Verify
    // that a single batch_access call bumps the access_count of every id
    // (and only those ids) by exactly 1.
    let store = make_store();
    let mut ids = Vec::new();
    for i in 0..5 {
        let id = store
            .write(Memory::new(
                MemoryTier::Semantic,
                format!("mem {i}"),
                "content about routes",
                1000,
            ))
            .await
            .unwrap();
        ids.push(id);
    }
    // A sixth memory NOT in the batch — its count must stay 0.
    let outsider = store
        .write(Memory::new(
            MemoryTier::Semantic,
            "outsider",
            "content about routes",
            1000,
        ))
        .await
        .unwrap();

    store.batch_access(&ids).await.unwrap();

    let list = store.list_by_tier(MemoryTier::Semantic).await.unwrap();
    for id in &ids {
        let m = list.iter().find(|m| &m.id == id).unwrap();
        assert_eq!(m.access_count, 1, "batched id {id} should be bumped once");
    }
    let m = list.iter().find(|m| m.id == outsider).unwrap();
    assert_eq!(m.access_count, 0, "outsider should not be bumped");
}

#[tokio::test]
async fn batch_access_empty_is_noop() {
    let store = make_store();
    // Must not error on an empty slice.
    store.batch_access(&[]).await.unwrap();
}

#[tokio::test]
async fn recall_peek_does_not_bump_access() {
    // Regression for the self-reinforcement loop (2026-08-20): passive
    // recall paths (auto-recall, debug preview) must NOT bump access —
    // otherwise a memory that once wins keeps refreshing its own recency
    // and never decays. The explicit recall() path keeps the bump.
    let store = make_store();
    let id = store
        .write(Memory::new(
            MemoryTier::Semantic,
            "route fact".to_string(),
            "the login route handles authentication",
            1000,
        ))
        .await
        .unwrap();

    let before = store.list_by_tier(MemoryTier::Semantic).await.unwrap();
    let before = before.iter().find(|m| m.id == id).unwrap();
    assert_eq!(before.access_count, 0);
    let before_accessed = before.last_accessed_at;

    // Peek: the hit is returned, but access state is untouched.
    let results = store
        .recall_peek("login route", &MemoryFilter::new())
        .await
        .unwrap();
    assert!(!results.is_empty(), "peek still returns the hit");
    let after = store.list_by_tier(MemoryTier::Semantic).await.unwrap();
    let after = after.iter().find(|m| m.id == id).unwrap();
    assert_eq!(after.access_count, 0, "peek must not bump access_count");
    assert_eq!(
        after.last_accessed_at, before_accessed,
        "peek must not refresh last_accessed_at"
    );

    // Explicit recall: still bumps (deliberate lookups reinforce).
    let _ = store
        .recall("login route", &MemoryFilter::new())
        .await
        .unwrap();
    let bumped = store.list_by_tier(MemoryTier::Semantic).await.unwrap();
    let bumped = bumped.iter().find(|m| m.id == id).unwrap();
    assert_eq!(bumped.access_count, 1, "explicit recall bumps access");
}

#[tokio::test]
async fn recall_bumps_access_via_batch() {
    // End-to-end: recall should still bump access counts (now via the
    // batched path). After recalling 3 memories, each should have
    // access_count == 1.
    let store = make_store();
    let mut ids = Vec::new();
    for i in 0..3 {
        let id = store
            .write(Memory::new(
                MemoryTier::Semantic,
                format!("route fact {i}"),
                "the login route handles authentication",
                1000,
            ))
            .await
            .unwrap();
        ids.push(id);
    }
    let results = store
        .recall("login route", &MemoryFilter::new())
        .await
        .unwrap();
    assert_eq!(results.len(), 3);
    let list = store.list_by_tier(MemoryTier::Semantic).await.unwrap();
    for id in &ids {
        let m = list.iter().find(|m| &m.id == id).unwrap();
        assert_eq!(
            m.access_count, 1,
            "recall should bump access_count via batch"
        );
    }
}

#[tokio::test]
async fn delete_working_for_session_removes_only_matching_events() {
    let store = make_store();
    // Two sessions, each with working events.
    store
        .record_tool_event(
            Some("sess-a"),
            "file_read",
            serde_json::json!({"path": "a.rs"}),
            "a",
            None,
        )
        .await
        .unwrap();
    store
        .record_tool_event(
            Some("sess-a"),
            "file_write",
            serde_json::json!({"path": "a.rs"}),
            "written",
            None,
        )
        .await
        .unwrap();
    store
        .record_tool_event(
            Some("sess-b"),
            "file_read",
            serde_json::json!({"path": "b.rs"}),
            "b",
            None,
        )
        .await
        .unwrap();
    // A semantic memory tagged with sess-a — must NOT be deleted.
    let mut sem = Memory::new(MemoryTier::Semantic, "fact", "a fact", 1000);
    sem.source_session_ids = vec!["sess-a".to_string()];
    store.write(sem).await.unwrap();

    let deleted = store.delete_working_for_session("sess-a").await.unwrap();
    assert_eq!(deleted, 2, "should delete the two sess-a working events");

    let working = store.list_by_tier(MemoryTier::Working).await.unwrap();
    // Only sess-b's working event remains.
    assert_eq!(working.len(), 1);
    assert_eq!(working[0].source_session_ids, vec!["sess-b".to_string()]);

    // The semantic memory tagged with sess-a survives (cleanup is
    // working-tier only).
    let semantic = store.list_by_tier(MemoryTier::Semantic).await.unwrap();
    assert_eq!(semantic.len(), 1);
    assert_eq!(semantic[0].source_session_ids, vec!["sess-a".to_string()]);
}

#[tokio::test]
async fn delete_working_for_session_no_match_is_zero() {
    let store = make_store();
    store
        .record_tool_event(
            Some("sess-a"),
            "file_read",
            serde_json::json!({"path": "a.rs"}),
            "a",
            None,
        )
        .await
        .unwrap();
    let deleted = store
        .delete_working_for_session("nonexistent")
        .await
        .unwrap();
    assert_eq!(deleted, 0);
}

#[tokio::test]
async fn stored_model_fingerprints_reflect_writes() {
    // After writing memories, the stored fingerprints must include the
    // embedder's model_id + dim (so a startup mismatch is detectable).
    let store = make_store();
    store
        .write(Memory::new(MemoryTier::Semantic, "fact", "jwt auth", 1000))
        .await
        .unwrap();
    let fps = store.stored_model_fingerprints().await.unwrap();
    // The in-memory test store uses HashEmbedder → model_id "hash", dim 768.
    assert!(
        fps.iter().any(|(m, d)| m == "hash" && *d == 768),
        "expected a hash/768 fingerprint, got {fps:?}"
    );
}

#[tokio::test]
async fn reembed_all_updates_fingerprint_and_count() {
    // Re-embedding all memories with a new embedder must update every row's
    // vector + fingerprint, and return the count re-embedded.
    use crate::memory::embedder::HashEmbedder;
    let store = make_store();
    store
        .write(Memory::new(MemoryTier::Semantic, "a", "content one", 1000))
        .await
        .unwrap();
    store
        .write(Memory::new(MemoryTier::Semantic, "b", "content two", 1000))
        .await
        .unwrap();
    // Re-embed with a fresh HashEmbedder (same model_id "hash" — the point
    // is to verify the count + that the fingerprint is present after).
    let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
    let count = store.reembed_all(embedder, None).await.unwrap();
    assert_eq!(count, 2, "both memories should be re-embedded");
    let fps = store.stored_model_fingerprints().await.unwrap();
    assert!(
        fps.iter().any(|(m, d)| m == "hash" && *d == 768),
        "fingerprint should be hash/768 after reembed, got {fps:?}"
    );
}

#[tokio::test]
async fn reembed_all_progress_is_monotonic_and_ends_complete() {
    // The progress callback must fire once per row with (done, total),
    // monotonically increasing and ending at (n, n) — the Settings
    // maintenance rebuild drives its progress bar from this signal.
    use crate::memory::embedder::HashEmbedder;
    let store = make_store();
    for i in 0..3 {
        store
            .write(Memory::new(
                MemoryTier::Semantic,
                format!("t{i}"),
                format!("content {i}"),
                1000,
            ))
            .await
            .unwrap();
    }
    let seen: Arc<std::sync::Mutex<Vec<(usize, usize)>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = seen.clone();
    let progress = move |done, total| sink.lock().unwrap().push((done, total));
    let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
    let count = store.reembed_all(embedder, Some(&progress)).await.unwrap();
    assert_eq!(count, 3);
    let seen = seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![(1, 3), (2, 3), (3, 3)],
        "one (done, total) per row, monotonic, ending complete"
    );
}

#[tokio::test]
async fn writes_all_four_tiers() {
    let store = make_store();
    for tier in [
        MemoryTier::Working,
        MemoryTier::Episodic,
        MemoryTier::Semantic,
        MemoryTier::Procedural,
    ] {
        let id = store
            .write(Memory::new(tier, "title", "content about jwt auth", 1000))
            .await
            .unwrap();
        assert!(!id.is_empty());
        let list = store.list_by_tier(tier).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].tier, tier);
    }
}

#[tokio::test]
async fn session_lifecycle() {
    let store = make_store();
    let session = store.start_session("my-project").await.unwrap();
    assert_eq!(session.project, "my-project");
    store.end_session(&session.id).await.unwrap();
}

#[tokio::test]
async fn record_tool_event_stores_working() {
    let store = make_store();
    let id = store
        .record_tool_event(
            Some("sess-1"),
            "file_read",
            serde_json::json!({"path": "src/main.rs"}),
            "fn main() {}",
            None,
        )
        .await
        .unwrap();
    let list = store.list_by_tier(MemoryTier::Working).await.unwrap();
    let m = list.iter().find(|m| m.id == id).unwrap();
    assert_eq!(m.title, "tool: file_read");
    assert_eq!(m.source_session_ids, vec!["sess-1".to_string()]);
}

#[tokio::test]
async fn recall_ranks_by_relevance() {
    let store = make_store();
    // A highly relevant memory and an irrelevant one.
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "auth fact",
            "this project uses jose for JWT authentication",
            1000,
        ))
        .await
        .unwrap();
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "unrelated",
            "the weather is sunny today",
            1000,
        ))
        .await
        .unwrap();

    let results = store
        .recall("jwt authentication", &MemoryFilter::new())
        .await
        .unwrap();
    assert!(!results.is_empty());
    // The auth fact should rank higher than the weather.
    assert_eq!(results[0].memory.title, "auth fact");
}

#[tokio::test]
async fn recall_ranks_semantic_above_working_when_both_match() {
    // A working + a semantic memory with IDENTICAL content + title. Both
    // match the query keyword and have the same embedding/sim, so the only
    // thing separating them is the tier weight (semantic 0.3 vs working 0).
    // The semantic one must rank first. Uses include_working() so both are
    // candidates (working is excluded by default).
    let store = make_store();
    store
        .write(Memory::new(
            MemoryTier::Working,
            "auth fact",
            "this project uses jose for JWT authentication",
            1000,
        ))
        .await
        .unwrap();
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "auth fact",
            "this project uses jose for JWT authentication",
            1000,
        ))
        .await
        .unwrap();

    let results = store
        .recall("jwt authentication", &MemoryFilter::new().include_working())
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0].memory.tier,
        MemoryTier::Semantic,
        "semantic must outrank working on identical content"
    );
    assert_eq!(results[1].memory.tier, MemoryTier::Working);
}

#[tokio::test]
async fn recall_decays_strength_so_old_memories_sink() {
    // Two memories with identical content + title, written at different
    // times. The older one (last_accessed_at far in the past) should rank
    // LOWER than the recent one, because strength decays by time-since-
    // last-access (Ebbinghaus curve, 7-day tau). Without decay both would
    // tie at the same frozen write-time strength.
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::new());
    let now_val = Arc::new(AtomicI64::new(1_000_000));
    let now_clone = Arc::clone(&now_val);
    let store =
        MemoryStore::open_in_memory_with_clock(embedder, move || now_clone.load(Ordering::SeqCst))
            .unwrap();

    // Write the "old" memory, then advance the clock by 30 days.
    let thirty_days: i64 = 30 * 24 * 60 * 60;
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "stale fact",
            "the project uses postgres for storage",
            now_val.load(Ordering::SeqCst),
        ))
        .await
        .unwrap();
    now_val.store(1_000_000 + thirty_days, Ordering::SeqCst);

    // Write the "recent" memory with identical content (same keyword +
    // embedding score, but last_accessed_at is now).
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "fresh fact",
            "the project uses postgres for storage",
            now_val.load(Ordering::SeqCst),
        ))
        .await
        .unwrap();

    let results = store
        .recall("postgres storage", &MemoryFilter::new())
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    // The recent memory (decayed strength ≈ 1.0) must outrank the stale
    // one (decayed strength ≈ 0.5^4 ≈ 0.06 after 30 days).
    assert_eq!(results[0].memory.title, "fresh fact");
    assert_eq!(results[1].memory.title, "stale fact");
}

#[tokio::test]
async fn strongest_returns_top_by_decayed_strength() {
    // Two semantic memories written at different times. The more-recently-
    // accessed one (smaller time-since-last-access) has higher decayed
    // strength and must rank first. Uses a fixed clock so decay is
    // deterministic.
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::new());
    let now_val = Arc::new(AtomicI64::new(1_000_000));
    let now_clone = Arc::clone(&now_val);
    let store =
        MemoryStore::open_in_memory_with_clock(embedder, move || now_clone.load(Ordering::SeqCst))
            .unwrap();

    // "old" memory: written + last-accessed at t=1_000_000.
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "old fact",
            "the project uses postgres for storage",
            1_000_000,
        ))
        .await
        .unwrap();
    // Advance the clock by 30 days, then write the "fresh" memory.
    let thirty_days: i64 = 30 * 24 * 60 * 60;
    now_val.store(1_000_000 + thirty_days, Ordering::SeqCst);
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "fresh fact",
            "the project uses postgres for storage",
            1_000_000 + thirty_days,
        ))
        .await
        .unwrap();

    let results = store
        .strongest(&[MemoryTier::Semantic, MemoryTier::Procedural], 8)
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    // Fresh (decayed strength ≈ 1.0) outranks old (≈ 0.06 after 30 days).
    assert_eq!(results[0].title, "fresh fact");
    assert_eq!(results[1].title, "old fact");
}

#[tokio::test]
async fn strongest_excludes_superseded_memories() {
    // The session primer (strongest) is the standing context of every new
    // session — a superseded record must never surface there at full
    // strength (the same invariant recall/list_filtered honor; the primer
    // path was the gap). Supersede the STRONGEST semantic memory and
    // assert it leaves strongest() output entirely.
    let store = make_store();
    let mut old = Memory::new(
        MemoryTier::Semantic,
        "DECISION: storage engine",
        "we use postgres for storage",
        1000,
    );
    old.strength = 5.0; // the strongest memory in the store
    let old_id = store.write(old).await.unwrap();

    // Before superseding: the row surfaces.
    let before = store.strongest(&[MemoryTier::Semantic], 8).await.unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].id, old_id);

    // Supersede the strongest row with its successor.
    store
        .supersede_memory(
            &old_id,
            Memory::new(
                MemoryTier::Semantic,
                "DECISION: storage engine",
                "we use sqlite for storage",
                1001,
            ),
        )
        .await
        .unwrap();

    // The superseded row leaves the primer entirely — only the live
    // successor remains.
    let after = store.strongest(&[MemoryTier::Semantic], 8).await.unwrap();
    assert_eq!(after.len(), 1, "superseded excluded from the primer");
    assert_ne!(after[0].id, old_id);
    assert!(after.iter().all(|m| m.superseded_by.is_none()));
}

#[tokio::test]
async fn strongest_respects_tier_filter_and_excludes_working() {
    // A working + a semantic + a procedural memory. strongest over
    // [Semantic, Procedural] must return only the distilled tiers (never
    // working, even if passed). A procedural memory is included.
    let store = make_store();
    store
        .write(Memory::new(
            MemoryTier::Working,
            "working event",
            "raw tool output snapshot",
            1000,
        ))
        .await
        .unwrap();
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "semantic fact",
            "a distilled project fact",
            1000,
        ))
        .await
        .unwrap();
    store
        .write(Memory::new(
            MemoryTier::Procedural,
            "procedural workflow",
            "a learned recurring workflow",
            1000,
        ))
        .await
        .unwrap();

    // Even if Working is passed, it must be excluded.
    let results = store
        .strongest(
            &[
                MemoryTier::Semantic,
                MemoryTier::Procedural,
                MemoryTier::Working,
            ],
            8,
        )
        .await
        .unwrap();
    assert_eq!(results.len(), 2, "working must be excluded");
    assert!(results
        .iter()
        .all(|m| m.tier == MemoryTier::Semantic || m.tier == MemoryTier::Procedural));

    // An empty tier slice returns empty.
    let empty = store.strongest(&[], 8).await.unwrap();
    assert!(empty.is_empty());

    // A limit of 0 returns empty.
    let zero = store.strongest(&[MemoryTier::Semantic], 0).await.unwrap();
    assert!(zero.is_empty());
}

#[tokio::test]
async fn recall_fts_prefilters_candidates() {
    // Insert 100 memories: 5 share a distinctive keyword ("sesame"), the
    // other 95 are filler with no keyword overlap. A recall for "sesame"
    // should return only the 5 matching memories (the FTS pre-filter
    // excludes the 95 irrelevant ones before cosine ranking).
    let store = make_store();
    for i in 0..5 {
        store
            .write(Memory::new(
                MemoryTier::Semantic,
                format!("sesame fact {i}"),
                "the sesame seed grants access to the vault",
                1000,
            ))
            .await
            .unwrap();
    }
    for i in 0..95 {
        store
            .write(Memory::new(
                MemoryTier::Semantic,
                format!("filler {i}"),
                "completely unrelated filler content about the weather",
                1000,
            ))
            .await
            .unwrap();
    }

    let results = store.recall("sesame", &MemoryFilter::new()).await.unwrap();
    // Only the 5 sesame memories should be returned.
    assert_eq!(
        results.len(),
        5,
        "FTS pre-filter should exclude the 95 filler memories"
    );
    for sm in &results {
        assert!(
            sm.memory.title.contains("sesame") || sm.memory.content.contains("sesame"),
            "non-matching memory leaked through FTS pre-filter: {}",
            sm.memory.title
        );
    }
}

#[tokio::test]
async fn recall_fts_respects_tier_filter() {
    // The FTS pre-filter path must honour the tier filter: a recall scoped
    // to Semantic should not return Working-tier memories even if they
    // match the keyword.
    let store = make_store();
    store
        .write(Memory::new(
            MemoryTier::Working,
            "working sesame",
            "sesame seed in working memory",
            1000,
        ))
        .await
        .unwrap();
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "semantic sesame",
            "sesame seed in semantic memory",
            1000,
        ))
        .await
        .unwrap();

    let results = store
        .recall("sesame", &MemoryFilter::new().tier(MemoryTier::Semantic))
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].memory.tier, MemoryTier::Semantic);
}

#[tokio::test]
async fn recall_zero_keyword_hits_still_returns_semantic_candidates() {
    // When the query shares no keyword with any memory, FTS returns no
    // candidates — and recall falls through to the capped semantic scan
    // (plan cb1914b9, merged 4e3f987): zero keyword overlap is the recall
    // case embeddings exist for, so the query vector still ranks the
    // 200 most-recent by cosine instead of returning empty. The
    // HashEmbedder is deterministic, so a query with overlapping tokens
    // still ranks the closest memory first.
    let store = make_store();
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "alpha",
            "alpha beta gamma delta epsilon zeta",
            1000,
        ))
        .await
        .unwrap();
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "omega",
            "one two three four five six",
            1000,
        ))
        .await
        .unwrap();

    // "zeta" appears only in the first memory → FTS matches it directly.
    let results = store.recall("zeta", &MemoryFilter::new()).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].memory.title, "alpha");

    // "kappa" appears in neither memory → FTS returns no matches → the
    // capped semantic scan still surfaces both rows (not empty); ranking
    // order under the hash embedder is not asserted (hash-cosine is a
    // weak tiebreaker by design).
    let results = store.recall("kappa", &MemoryFilter::new()).await.unwrap();
    assert_eq!(
        results.len(),
        2,
        "zero-keyword hit → capped semantic scan, not empty"
    );
    let mut titles: Vec<_> = results.iter().map(|sm| sm.memory.title.clone()).collect();
    titles.sort();
    assert_eq!(titles, vec!["alpha", "omega"]);
}

#[tokio::test]
async fn recall_fts_special_characters_do_not_error() {
    // A query containing FTS5 special characters (':', '*', '(', '"')
    // must not raise a syntax error — tokens are quoted. It should either
    // match via the literal token or fall through to the capped semantic
    // scan (FTS available but no matches) — never a syntax error.
    let store = make_store();
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "special",
            "content with a colon: here and an asterisk *",
            1000,
        ))
        .await
        .unwrap();
    // These must not panic / error.
    let _ = store
        .recall("colon: asterisk*", &MemoryFilter::new())
        .await
        .unwrap();
    let _ = store
        .recall("(parenthesized) query", &MemoryFilter::new())
        .await
        .unwrap();
    let _ = store
        .recall("\"quoted\"", &MemoryFilter::new())
        .await
        .unwrap();
}

#[test]
fn contains_ascii_ci_is_case_insensitive_and_allocation_free() {
    // P2: the allocation-free ASCII case-insensitive contains used in the
    // recall scoring loop (replaces per-memory to_lowercase()).
    assert!(MemoryStore::contains_ascii_ci("Hello World", "world"));
    assert!(MemoryStore::contains_ascii_ci("Hello World", "HELLO"));
    assert!(MemoryStore::contains_ascii_ci("Hello World", "o w"));
    assert!(!MemoryStore::contains_ascii_ci("Hello World", "xyz"));
    // Empty needle always matches.
    assert!(MemoryStore::contains_ascii_ci("anything", ""));
    // Needle longer than haystack.
    assert!(!MemoryStore::contains_ascii_ci("hi", "hello"));
}

#[tokio::test]
async fn recall_no_fts_match_returns_empty_no_full_scan() {
    // Plan cb1914b9 (merged 4e3f987): when FTS is available but returns no
    // matches, recall falls back to the SAME capped semantic scan used when
    // FTS is unavailable (the 200 most-recent rows ranked by cosine) —
    // zero-keyword recall is what the embeddings exist for, so it must not
    // return empty. Verified by writing memories that share no keyword
    // with the query.
    let store = make_store();
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "alpha",
            "apple banana cherry",
            1000,
        ))
        .await
        .unwrap();
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "beta",
            "dog elephant fox",
            1000,
        ))
        .await
        .unwrap();
    // "zebra" matches neither → the capped semantic scan still returns
    // both rows (order under the hash embedder is not asserted).
    let results = store.recall("zebra", &MemoryFilter::new()).await.unwrap();
    assert_eq!(
        results.len(),
        2,
        "no FTS match → capped semantic scan returns the rows, not empty"
    );
}

#[tokio::test]
async fn load_recent_locked_caps_and_orders_by_recency() {
    // N2: the capped full-scan fallback (used when FTS5 is unavailable)
    // loads at most `limit` memories, most-recent first. FTS is always
    // available in the test env, so exercise the function directly.
    let store = make_store();
    // Write 5 memories; bump access times so the recency order is
    // deterministic (write order is also created_at order, but
    // last_accessed_at drives the ORDER BY).
    for i in 0..5u32 {
        let mut m = Memory::new(
            MemoryTier::Semantic,
            &format!("mem-{i}"),
            "shared keyword",
            1000,
        );
        m.last_accessed_at = 1000 + i as i64; // ascending recency
        store.write(m).await.unwrap();
    }
    let conn = store.read_conn().lock().expect("read conn lock poisoned");
    // Cap at 3 — only the 3 most-recent are loaded. include_working()
    // here because all memories are Semantic (nothing to exclude) and we
    // want to exercise the plain (no tier, no exclusion) SQL shape.
    let recent =
        MemoryStore::load_recent_locked(&conn, &MemoryFilter::new().include_working(), 3).unwrap();
    assert_eq!(recent.len(), 3, "the fallback is capped at `limit`");
    // Most-recent first (last_accessed_at DESC): mem-4, mem-3, mem-2.
    assert_eq!(recent[0].title, "mem-4");
    assert_eq!(recent[1].title, "mem-3");
    assert_eq!(recent[2].title, "mem-2");
    // A cap larger than the store returns all (still most-recent first).
    let all = MemoryStore::load_recent_locked(&conn, &MemoryFilter::new().include_working(), 200)
        .unwrap();
    assert_eq!(all.len(), 5);
    assert_eq!(all[0].title, "mem-4");
    // Tier filter is respected.
    let semantic = MemoryStore::load_recent_locked(
        &conn,
        &MemoryFilter::new().tier(MemoryTier::Semantic),
        200,
    )
    .unwrap();
    assert_eq!(semantic.len(), 5);
    let working =
        MemoryStore::load_recent_locked(&conn, &MemoryFilter::new().tier(MemoryTier::Working), 200)
            .unwrap();
    assert_eq!(working.len(), 0, "no Working-tier memories");
}

// ── Phase 1: typed records + hygiene ops ─────────────────────────────

#[tokio::test]
async fn write_derives_record_type_from_title_prefix() {
    let store = make_store();
    // A typed prefix is classified on write...
    let id = store
        .write(Memory::new(
            MemoryTier::Semantic,
            "BUG: crash on open",
            "the app crashes when opened",
            1000,
        ))
        .await
        .unwrap();
    let list = store.list_by_tier(MemoryTier::Semantic).await.unwrap();
    let m = list.iter().find(|m| m.id == id).unwrap();
    assert_eq!(m.record_type, MemoryRecordType::Bug);

    // ...and the prefix WINS over a caller-set record_type.
    let mut wrong = Memory::new(
        MemoryTier::Semantic,
        "DECISION: use sqlite",
        "single-file storage",
        1000,
    );
    wrong.record_type = MemoryRecordType::Spec; // caller error — title wins
    let id = store.write(wrong).await.unwrap();
    let list = store.list_by_tier(MemoryTier::Semantic).await.unwrap();
    let m = list.iter().find(|m| m.id == id).unwrap();
    assert_eq!(m.record_type, MemoryRecordType::Decision);

    // A plain title classifies as None even when the caller set one.
    let mut plain = Memory::new(MemoryTier::Semantic, "plain fact", "c", 1000);
    plain.record_type = MemoryRecordType::How;
    let id = store.write(plain).await.unwrap();
    let list = store.list_by_tier(MemoryTier::Semantic).await.unwrap();
    let m = list.iter().find(|m| m.id == id).unwrap();
    assert_eq!(m.record_type, MemoryRecordType::None);
}

#[tokio::test]
async fn update_memory_roundtrips_and_keeps_fts_in_sync() {
    let store = make_store();
    let id = store
        .write(Memory::new(
            MemoryTier::Semantic,
            "plain title",
            "the alpha keyword fact",
            1000,
        ))
        .await
        .unwrap();

    // Content update: FTS must re-index — the NEW keyword recalls the
    // row, the OLD one no longer does.
    let updated = store
        .update_memory(&id, None, Some("the beta keyword fact"))
        .await
        .unwrap();
    assert!(updated);
    let hits = store
        .recall("beta", &MemoryFilter::new().tier(MemoryTier::Semantic))
        .await
        .unwrap();
    assert_eq!(hits.len(), 1, "FTS re-indexed the new content");
    // The old content left the FTS index. recall("alpha") itself is NOT
    // the right probe anymore: since plan cb1914b9 (merged 4e3f987) a
    // zero-keyword recall falls through to the capped semantic scan, so
    // the row legitimately resurfaces even though FTS dropped it. Probe
    // the FTS index directly instead.
    {
        let conn = store.read_conn().lock().expect("read conn lock poisoned");
        let probe = MemoryStore::fts_candidates_locked(
            &conn,
            "alpha",
            &MemoryFilter::new().tier(MemoryTier::Semantic),
            50,
        );
        assert!(
            matches!(probe, FtsResult::NoMatches),
            "the old content left the FTS index"
        );
    }

    // Title update: re-derives record_type from the new prefix and
    // leaves the content untouched.
    let updated = store
        .update_memory(&id, Some("BUG: beta broke"), None)
        .await
        .unwrap();
    assert!(updated);
    let list = store.list_by_tier(MemoryTier::Semantic).await.unwrap();
    let m = list.iter().find(|m| m.id == id).unwrap();
    assert_eq!(m.title, "BUG: beta broke");
    assert_eq!(m.content, "the beta keyword fact", "untouched field kept");
    assert_eq!(
        m.record_type,
        MemoryRecordType::Bug,
        "a title change re-derives record_type"
    );

    // A no-field update is an existence check, not a no-op UPDATE.
    assert!(store.update_memory(&id, None, None).await.unwrap());

    // Unknown id → false, no row created.
    let updated = store
        .update_memory("no-such-id", Some("t"), Some("c"))
        .await
        .unwrap();
    assert!(!updated);
    assert!(!store.update_memory("no-such-id", None, None).await.unwrap());
}

#[tokio::test]
async fn supersede_excludes_old_from_recall_but_keeps_history() {
    let store = make_store();
    let old_id = store
        .write(Memory::new(
            MemoryTier::Semantic,
            "DECISION: storage engine",
            "we use postgres for storage",
            1000,
        ))
        .await
        .unwrap();

    let new_id = store
        .supersede_memory(
            &old_id,
            Memory::new(
                MemoryTier::Semantic,
                "DECISION: storage engine",
                "we use sqlite for storage",
                1001,
            ),
        )
        .await
        .unwrap();
    assert_ne!(new_id, old_id);

    // Default recall: the superseded row is gone, the successor shows.
    let results = store.recall("storage", &MemoryFilter::new()).await.unwrap();
    assert_eq!(results.len(), 1, "superseded excluded by default");
    assert_eq!(results[0].memory.id, new_id);
    assert_eq!(results[0].memory.superseded_by, None);

    // include_superseded resurfaces the old row, still pointing at its
    // successor (the [superseded] annotation keys off this field).
    let results = store
        .recall("storage", &MemoryFilter::new().include_superseded())
        .await
        .unwrap();
    assert_eq!(results.len(), 2, "history resurfaces when opted in");
    let old = results.iter().find(|sm| sm.memory.id == old_id).unwrap();
    assert_eq!(old.memory.superseded_by.as_deref(), Some(new_id.as_str()));
}

#[tokio::test]
async fn supersede_errors_on_missing_or_already_superseded() {
    let store = make_store();
    // Missing id → error naming it.
    let err = store
        .supersede_memory(
            "no-such-id",
            Memory::new(MemoryTier::Semantic, "t", "c", 1000),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("no memory with id 'no-such-id'"),
        "{err}"
    );

    // Already superseded → error naming the EXISTING superseder, and
    // the chain stays intact (the failed attempt inserted nothing).
    let old_id = store
        .write(Memory::new(MemoryTier::Semantic, "old", "c", 1000))
        .await
        .unwrap();
    let first = store
        .supersede_memory(&old_id, Memory::new(MemoryTier::Semantic, "v2", "c", 1001))
        .await
        .unwrap();
    let err = store
        .supersede_memory(&old_id, Memory::new(MemoryTier::Semantic, "v3", "c", 1002))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("already superseded"), "{msg}");
    assert!(
        msg.contains(&first),
        "the error names the existing superseder: {msg}"
    );
    // Rollback check: "v3" was never inserted (the transaction failed
    // before the insert), so only two rows exist.
    let all = store.list_by_tier(MemoryTier::Semantic).await.unwrap();
    assert_eq!(all.len(), 2, "the failed supersede inserted nothing");
    assert!(all.iter().all(|m| m.title != "v3"));
}

#[tokio::test]
async fn delete_memory_removes_from_recall_and_list() {
    let store = make_store();
    let id = store
        .write(Memory::new(
            MemoryTier::Semantic,
            "ephemeral",
            "the vanishing keyword",
            1000,
        ))
        .await
        .unwrap();

    let deleted = store.delete_memory(&id).await.unwrap();
    assert!(deleted);

    // Gone from listing...
    assert!(store
        .list_by_tier(MemoryTier::Semantic)
        .await
        .unwrap()
        .is_empty());
    // ...and from recall (the FTS delete trigger cleared the index).
    let results = store
        .recall("vanishing", &MemoryFilter::new())
        .await
        .unwrap();
    assert!(results.is_empty());

    // Deleting again reports false.
    let deleted = store.delete_memory(&id).await.unwrap();
    assert!(!deleted);
}

#[tokio::test]
async fn list_filtered_filters_by_tier_type_and_supersession() {
    let store = make_store();
    // Seed: a bug + a decision + a how + one superseded bug + working noise.
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "BUG: crash",
            "bug content",
            1000,
        ))
        .await
        .unwrap();
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "DECISION: db",
            "decision content",
            1001,
        ))
        .await
        .unwrap();
    store
        .write(Memory::new(
            MemoryTier::Procedural,
            "HOW: release",
            "how content",
            1002,
        ))
        .await
        .unwrap();
    let stale = store
        .write(Memory::new(
            MemoryTier::Semantic,
            "BUG: stale",
            "stale bug",
            999,
        ))
        .await
        .unwrap();
    store
        .supersede_memory(
            &stale,
            Memory::new(MemoryTier::Semantic, "BUG: fresh", "fresh bug", 1003),
        )
        .await
        .unwrap();
    store
        .write(Memory::new(MemoryTier::Working, "tool: x", "noise", 1004))
        .await
        .unwrap();

    // Default: all live, non-working memories, newest first
    // (created_at DESC). The superseded (999) and working (1004) rows
    // are excluded.
    let all = store.list_filtered(&MemoryFilter::new()).await.unwrap();
    let titles: Vec<&str> = all.iter().map(|m| m.title.as_str()).collect();
    assert_eq!(
        titles,
        vec!["BUG: fresh", "HOW: release", "DECISION: db", "BUG: crash"]
    );

    // record_type filter (bugs only, superseded still excluded).
    let bugs = store
        .list_filtered(&MemoryFilter::new().record_type(MemoryRecordType::Bug))
        .await
        .unwrap();
    let titles: Vec<&str> = bugs.iter().map(|m| m.title.as_str()).collect();
    assert_eq!(titles, vec!["BUG: fresh", "BUG: crash"]);

    // include_superseded resurfaces history.
    let bugs = store
        .list_filtered(
            &MemoryFilter::new()
                .record_type(MemoryRecordType::Bug)
                .include_superseded(),
        )
        .await
        .unwrap();
    assert_eq!(bugs.len(), 3);
    assert!(bugs.iter().any(|m| m.id == stale));

    // An explicit tier wins (and includes that tier regardless of the
    // working-exclusion default).
    let procedural = store
        .list_filtered(&MemoryFilter::new().tier(MemoryTier::Procedural))
        .await
        .unwrap();
    assert_eq!(procedural.len(), 1);
    assert_eq!(procedural[0].title, "HOW: release");
    let working = store
        .list_filtered(&MemoryFilter::new().tier(MemoryTier::Working))
        .await
        .unwrap();
    assert_eq!(working.len(), 1, "explicit working tier lists it");

    // limit bounds the result (newest first).
    let one = store
        .list_filtered(&MemoryFilter::new().limit(1))
        .await
        .unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].title, "BUG: fresh");
}

#[tokio::test]
async fn recall_filters_by_record_type() {
    let store = make_store();
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "BUG: login crash",
            "the login flow crashes on submit",
            1000,
        ))
        .await
        .unwrap();
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "login fact",
            "the login flow uses oauth tokens",
            1000,
        ))
        .await
        .unwrap();

    // Scoped to Bug records: only the typed memory matches.
    let bugs = store
        .recall(
            "login",
            &MemoryFilter::new().record_type(MemoryRecordType::Bug),
        )
        .await
        .unwrap();
    assert_eq!(bugs.len(), 1);
    assert_eq!(bugs[0].memory.title, "BUG: login crash");

    // Unscoped: both keyword matches return.
    let all = store.recall("login", &MemoryFilter::new()).await.unwrap();
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn recall_ranks_authored_above_derived_on_ties() {
    // Two memories with identical title/content/strength, one authored,
    // one indexer-derived. With everything else equal, the authored
    // record must rank first: derived records are pointers to on-disk
    // truth, authored records are the knowledge itself.
    let store = make_store();
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "same fact",
            "identical content about the login route",
            1000,
        ))
        .await
        .unwrap();
    let mut derived = Memory::new(
        MemoryTier::Semantic,
        "same fact",
        "identical content about the login route",
        1000,
    );
    derived.record_class = MemoryClass::Derived;
    store.write(derived).await.unwrap();

    let results = store
        .recall("login route", &MemoryFilter::new())
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0].memory.record_class,
        MemoryClass::Authored,
        "authored outranks derived on a tie"
    );
    assert_eq!(results[1].memory.record_class, MemoryClass::Derived);
}

#[tokio::test]
async fn superseded_strongest_match_stays_hidden_by_default() {
    // Exclusion — not ranking — hides superseded rows: even the
    // STRONGEST matching memory never surfaces by default once
    // superseded. include_superseded resurfaces it at full strength (no
    // score penalty — exclusion is the whole mechanism), with
    // superseded_by set (the source of the tool's [superseded]
    // annotation, pinned by recall_annotates_superseded_entries).
    let store = make_store();
    let mut old = Memory::new(
        MemoryTier::Semantic,
        "DECISION: cache layer",
        "the cache layer uses redis",
        1000,
    );
    old.strength = 5.0; // the strongest match in the store
    let old_id = store.write(old).await.unwrap();
    store
        .supersede_memory(
            &old_id,
            Memory::new(
                MemoryTier::Semantic,
                "DECISION: cache layer",
                "the cache layer uses memcached",
                1001,
            ),
        )
        .await
        .unwrap();

    // Default: only the (weaker) successor shows.
    let results = store
        .recall("cache layer", &MemoryFilter::new())
        .await
        .unwrap();
    assert_eq!(results.len(), 1, "the strongest match stays hidden");
    assert_ne!(results[0].memory.id, old_id);

    // Opted in: the superseded row resurfaces and still ranks first.
    let results = store
        .recall("cache layer", &MemoryFilter::new().include_superseded())
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].memory.id, old_id, "full strength, no penalty");
    assert!(results[0].memory.superseded_by.is_some());
}

// ── request_stats + aggregation ────────────────────────────────────────

/// A helper that records a stats row with sensible defaults.
async fn record(
    store: &MemoryStore,
    session_id: Option<&str>,
    model: &str,
    endpoint: Option<&str>,
    p: u32,
    c: u32,
    r: u32,
    cached: u32,
    ttft: Option<u32>,
    gen: Option<u32>,
    at: i64,
) {
    let stats = RequestStats {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: session_id.map(|s| s.to_string()),
        model: model.to_string(),
        endpoint: endpoint.map(|s| s.to_string()),
        prompt_tokens: p,
        completion_tokens: c,
        reasoning_tokens: r,
        cached_tokens: Some(cached),
        ttft_ms: ttft,
        generation_ms: gen,
        created_at: at,
        outcome: None,
        purpose: None,
    };
    store.record_request_stats(&stats).await.unwrap();
}

#[tokio::test]
async fn savings_events_round_trip_and_negative_expansions() {
    // Backlog e4a50d22: the per-event ledger persists kind, before/after
    // counts, the saved delta (negative for re-expansions), and the
    // measured-vs-estimated flag; the rows reader filters by session and
    // orders by creation time.
    let store = MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap();
    let now = 1_700_000_000;
    store
        .record_savings_event(&SavingsEvent {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: Some("s1".into()),
            kind: "skeleton".into(),
            detail: Some("src/agent/turn.rs".into()),
            tokens_before: 12_000,
            tokens_after: 900,
            tokens_saved: 11_100,
            measured: false,
            created_at: now,
        })
        .await
        .unwrap();
    store
        .record_savings_event(&SavingsEvent {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: Some("s1".into()),
            kind: "archive_expand".into(),
            detail: Some("arch-1".into()),
            tokens_before: 0,
            tokens_after: 4_000,
            tokens_saved: -4_000,
            measured: false,
            created_at: now + 1,
        })
        .await
        .unwrap();
    store
        .record_savings_event(&SavingsEvent {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: Some("s2".into()),
            kind: "compression".into(),
            detail: Some("cargo test".into()),
            tokens_before: 8_000,
            tokens_after: 1_200,
            tokens_saved: 6_800,
            measured: true,
            created_at: now + 2,
        })
        .await
        .unwrap();
    let rows = store.savings_events_rows(Some("s1")).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].kind, "skeleton");
    assert_eq!(rows[0].detail.as_deref(), Some("src/agent/turn.rs"));
    assert_eq!(rows[0].tokens_before, 12_000);
    assert_eq!(rows[0].tokens_after, 900);
    assert_eq!(rows[0].tokens_saved, 11_100);
    assert!(!rows[0].measured);
    assert_eq!(rows[1].kind, "archive_expand");
    assert_eq!(rows[1].tokens_saved, -4_000);
    // The unfiltered read spans sessions (the dashboard's raw view).
    let all = store.savings_events_rows(None).await.unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[2].kind, "compression");
    assert!(all[2].measured);
    // Another session sees only its own rows.
    let s2 = store.savings_events_rows(Some("s2")).await.unwrap();
    assert_eq!(s2.len(), 1);
}

#[tokio::test]
async fn savings_stats_aggregates_kinds_days_recent_and_cache() {
    // Backlog 652ae094: the Dashboard's read path — the ledger aggregated
    // per-kind (biggest saver first), per UTC day (oldest first), the bounded
    // recent window (newest first), the metered totals, and the prompt-cache
    // numbers pulled from request_stats.
    let store = MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap();

    // A fresh project has an empty ledger: zeros and empty vectors, never an
    // error — the Dashboard renders an honest empty state from this.
    let empty = store.savings_stats().await.unwrap();
    assert_eq!(empty.event_count, 0);
    assert_eq!(empty.saved_tokens_total, 0);
    assert!(empty.per_kind.is_empty());
    assert!(empty.per_day.is_empty());
    assert!(empty.recent.is_empty());
    assert_eq!(empty.cache.request_count, 0);
    assert_eq!(empty.cache.prompt_tokens, 0);
    assert_eq!(empty.cache.cached_not_null_requests, 0);

    // Two distinct UTC days (the day bucket is created_at / 86400).
    let day1 = 1_700_000_000_i64;
    let day2 = day1 + 86_400;
    let record = |kind: &str, before: i64, after: i64, at: i64| SavingsEvent {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: Some("s1".into()),
        kind: kind.into(),
        detail: None,
        tokens_before: before,
        tokens_after: after,
        tokens_saved: before - after,
        measured: false,
        created_at: at,
    };
    store
        .record_savings_event(&record("skeleton", 12_000, 900, day1))
        .await
        .unwrap();
    store
        .record_savings_event(&record("compression", 8_000, 1_200, day1 + 60))
        .await
        .unwrap();
    store
        .record_savings_event(&record("skeleton", 2_000, 400, day2))
        .await
        .unwrap();
    // A re-expansion re-adds archived content, so it subtracts from the total.
    store
        .record_savings_event(&record("archive_expand", 0, 4_000, day2 + 60))
        .await
        .unwrap();

    let stats = store.savings_stats().await.unwrap();
    assert_eq!(stats.event_count, 4);
    // 11_100 + 6_800 + 1_600 - 4_000.
    assert_eq!(stats.saved_tokens_total, 15_500);

    // Per-kind, biggest saver first; the negative kind sorts last.
    assert_eq!(stats.per_kind.len(), 3);
    assert_eq!(stats.per_kind[0].kind, "skeleton");
    assert_eq!(stats.per_kind[0].event_count, 2);
    assert_eq!(stats.per_kind[0].saved_tokens, 12_700);
    assert_eq!(stats.per_kind[1].kind, "compression");
    assert_eq!(stats.per_kind[1].saved_tokens, 6_800);
    assert_eq!(stats.per_kind[2].kind, "archive_expand");
    assert_eq!(stats.per_kind[2].saved_tokens, -4_000);

    // Per-day, oldest first, stamped as the epoch DAY.
    assert_eq!(stats.per_day.len(), 2);
    assert_eq!(stats.per_day[0].day, day1 / 86_400);
    assert_eq!(stats.per_day[0].event_count, 2);
    assert_eq!(stats.per_day[0].saved_tokens, 17_900);
    assert_eq!(stats.per_day[1].day, day2 / 86_400);
    assert_eq!(stats.per_day[1].saved_tokens, -2_400);

    // Recent events, newest first.
    assert_eq!(stats.recent.len(), 4);
    assert_eq!(stats.recent[0].created_at, day2 + 60);
    assert_eq!(stats.recent[3].created_at, day1);

    // Cache efficiency from request_stats, NULL-safe: a row that never reported
    // a cache figure must not count toward the hit-rate denominator, or a
    // provider that never reports caching would look like a 0% hit rate.
    let req = |prompt: u32, cached: Option<u32>| RequestStats {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: Some("s1".into()),
        model: "m".into(),
        endpoint: None,
        prompt_tokens: prompt,
        completion_tokens: 10,
        reasoning_tokens: 0,
        cached_tokens: cached,
        ttft_ms: None,
        generation_ms: None,
        created_at: day1,
        outcome: None,
        purpose: None,
    };
    store
        .record_request_stats(&req(1_000, Some(400)))
        .await
        .unwrap();
    store.record_request_stats(&req(2_000, None)).await.unwrap();

    let stats = store.savings_stats().await.unwrap();
    assert_eq!(stats.cache.request_count, 2);
    assert_eq!(stats.cache.prompt_tokens, 3_000);
    assert_eq!(stats.cache.cached_tokens, 400);
    assert_eq!(stats.cache.cached_not_null_requests, 1);
}

#[tokio::test]
async fn archive_round_trip_expand_returns_identical_content() {
    // Backlog e4a50d22 lever 3: the full original goes into the archive and
    // comes back byte-identical by id — the progressive-disclosure contract
    // (the context keeps a preview, the archive keeps the truth).
    let store = MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap();
    let content = format!("{}\nDISTINCTIVE_MARKER_TOKEN\nbig = 0\n", "x".repeat(50_000));
    let id = store
        .archive_tool_result(Some("s1"), "shell", Some("cargo test"), &content)
        .await
        .unwrap();
    let fetched = store.expand_tool_result(&id).await.unwrap().expect("row exists");
    assert_eq!(fetched.content, content, "expand returns the identical original");
    assert_eq!(fetched.char_count as usize, content.chars().count());
    assert_eq!(fetched.tool.as_deref(), Some("shell"));
    assert_eq!(fetched.detail.as_deref(), Some("cargo test"));
    assert_eq!(fetched.session_id.as_deref(), Some("s1"));
    // Unknown ids are None, not an error (a foreign project's DB, a pruned row).
    assert!(store.expand_tool_result("nope").await.unwrap().is_none());
}

#[tokio::test]
async fn archive_search_finds_rows_by_keyword_with_a_snippet() {
    let store = MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap();
    let a = store
        .archive_tool_result(
            None,
            "shell",
            Some("cargo build"),
            "lots of noise\nUNIQUE_ALPHA error: broke\nmore noise",
        )
        .await
        .unwrap();
    store
        .archive_tool_result(None, "read_files", Some("src/b.rs"), "unrelated body UNIQUE_BETA here")
        .await
        .unwrap();
    let hits = store.search_archive("UNIQUE_ALPHA", 10).await.unwrap();
    assert_eq!(hits.len(), 1, "only the row carrying the term: {hits:?}");
    assert_eq!(hits[0].id, a);
    assert_eq!(hits[0].tool.as_deref(), Some("shell"));
    assert!(hits[0].snippet.contains("UNIQUE_ALPHA"), "{}", hits[0].snippet);
    // No match at all -> empty, never an error.
    assert!(store.search_archive("NOSUCHTERM_ZZZ", 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn archive_search_degrades_to_a_substring_scan_without_alnum_terms() {
    // A query with no alphanumeric terms cannot build an FTS5 MATCH
    // expression; the store must fall back to the LIKE scan and still find
    // the row rather than erroring.
    let store = MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap();
    store
        .archive_tool_result(None, "shell", Some("cmd"), "payload ### marker body")
        .await
        .unwrap();
    let hits = store.search_archive("###", 10).await.unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].snippet.contains("###"), "{}", hits[0].snippet);
}

#[tokio::test]
async fn request_stats_rows_separates_outcome_and_purpose_tags() {
    // R21: the row-level read separates error rows (outcome='error',
    // cached_tokens NULL) and compaction rows (purpose='summarize') from
    // normal successful rows — the extraction-separation pin (the
    // .coding/analysis aggregates filter on these tags).
    let store = MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap();
    let now = 1_700_000_000;
    // A normal successful row.
    record(
        &store,
        Some("s1"),
        "glm-5.3",
        Some("a100"),
        1000,
        200,
        0,
        800,
        Some(420),
        Some(2000),
        now,
    )
    .await;
    // An error row: our estimated prompt, cached NULL, outcome='error'.
    store
        .record_request_stats(&RequestStats {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: Some("s1".into()),
            model: "glm-5.3".into(),
            endpoint: Some("a100".into()),
            prompt_tokens: 300_000,
            completion_tokens: 0,
            reasoning_tokens: 0,
            cached_tokens: None,
            ttft_ms: None,
            generation_ms: None,
            created_at: now,
            outcome: Some("error".into()),
            purpose: None,
        })
        .await
        .unwrap();
    // A compaction row: purpose='summarize'.
    store
        .record_request_stats(&RequestStats {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: Some("s1".into()),
            model: "glm-5.3".into(),
            endpoint: Some("a100".into()),
            prompt_tokens: 250_000,
            completion_tokens: 500,
            reasoning_tokens: 0,
            cached_tokens: Some(0),
            ttft_ms: Some(900),
            generation_ms: Some(3000),
            created_at: now,
            outcome: None,
            purpose: Some("summarize".into()),
        })
        .await
        .unwrap();

    let rows = store.request_stats_rows("s1").await.unwrap();
    assert_eq!(rows.len(), 3);
    let errors: Vec<_> = rows
        .iter()
        .filter(|r| r.outcome.as_deref() == Some("error"))
        .collect();
    assert_eq!(errors.len(), 1);
    assert_eq!(
        errors[0].cached_tokens, None,
        "the error row's cached_tokens is NULL"
    );
    assert_eq!(errors[0].prompt_tokens, 300_000);
    let summarizes: Vec<_> = rows
        .iter()
        .filter(|r| r.purpose.as_deref() == Some("summarize"))
        .collect();
    assert_eq!(summarizes.len(), 1);
    assert_eq!(summarizes[0].prompt_tokens, 250_000);
    let normal: Vec<_> = rows
        .iter()
        .filter(|r| r.outcome.is_none() && r.purpose.is_none())
        .collect();
    assert_eq!(normal.len(), 1);
    assert_eq!(normal[0].cached_tokens, Some(800));
}

#[tokio::test]
async fn session_stats_aggregates_tokens_and_timing() {
    let store = make_store();
    // Start a session so session_stats has metadata to look up.
    let session = store.start_session("proj").await.unwrap();
    // Two requests for model A, one for model B. Mixed timing presence.
    record(
        &store,
        Some(&session.id),
        "gpt-4o",
        None,
        1000,
        200,
        50,
        800,
        Some(420),
        Some(2000),
        5000,
    )
    .await;
    record(
        &store,
        Some(&session.id),
        "gpt-4o",
        None,
        2000,
        400,
        100,
        0,
        Some(500),
        Some(4000),
        6000,
    )
    .await;
    record(
        &store,
        Some(&session.id),
        "gpt-4o-mini",
        None,
        500,
        100,
        0,
        0,
        None,
        None,
        7000,
    )
    .await;

    let stats = store.session_stats(&session.id).await.unwrap();
    assert_eq!(stats.session_id, session.id);
    assert_eq!(stats.request_count, 3);
    assert_eq!(stats.prompt_tokens, 3500);
    assert_eq!(stats.completion_tokens, 700);
    assert_eq!(stats.reasoning_tokens, 150);
    assert_eq!(stats.cached_tokens, 800);
    // TTFT total = 420 + 500 (the None request contributes 0).
    assert_eq!(stats.ttft_ms_total, 920);
    assert_eq!(stats.generation_ms_total, 6000);
    // Only 2 requests had timing (the gpt-4o-mini one had None).
    assert_eq!(stats.timed_requests, 2);
    // Per-model breakdown.
    assert_eq!(stats.per_model.len(), 2);
    let gpt4o = stats
        .per_model
        .iter()
        .find(|m| m.model == "gpt-4o")
        .unwrap();
    assert_eq!(gpt4o.prompt_tokens, 3000);
    assert_eq!(gpt4o.cached_tokens, 800);
    assert_eq!(gpt4o.request_count, 2);
    assert_eq!(gpt4o.timed_requests, 2);
    let mini = stats
        .per_model
        .iter()
        .find(|m| m.model == "gpt-4o-mini")
        .unwrap();
    assert_eq!(mini.request_count, 1);
    assert_eq!(mini.timed_requests, 0);
}

#[tokio::test]
async fn session_stats_splits_same_model_across_endpoints() {
    let store = make_store();
    let session = store.start_session("proj").await.unwrap();
    // Same model on two endpoints (the GLM5.3 A100 vs B200 case), plus a
    // pre-endpoint (NULL) row — three per_model rows, each with its own
    // sums. Totals still aggregate across all three.
    record(
        &store,
        Some(&session.id),
        "glm-5.3",
        Some("a100"),
        1000,
        200,
        0,
        0,
        Some(400),
        Some(2000),
        5000,
    )
    .await;
    record(
        &store,
        Some(&session.id),
        "glm-5.3",
        Some("b200"),
        2000,
        400,
        0,
        0,
        Some(600),
        Some(4000),
        6000,
    )
    .await;
    record(
        &store,
        Some(&session.id),
        "glm-5.3",
        None,
        500,
        100,
        0,
        0,
        None,
        None,
        7000,
    )
    .await;

    let stats = store.session_stats(&session.id).await.unwrap();
    // Totals aggregate across endpoints.
    assert_eq!(stats.request_count, 3);
    assert_eq!(stats.prompt_tokens, 3500);
    assert_eq!(stats.ttft_ms_total, 1000);
    // The breakdown splits by (model, endpoint): two named endpoints plus
    // the NULL group (pre-endpoint rows) — three rows for one model id.
    assert_eq!(stats.per_model.len(), 3);
    let a100 = stats
        .per_model
        .iter()
        .find(|m| m.endpoint.as_deref() == Some("a100"))
        .unwrap();
    assert_eq!(a100.model, "glm-5.3");
    assert_eq!(a100.prompt_tokens, 1000);
    assert_eq!(a100.timed_requests, 1);
    let b200 = stats
        .per_model
        .iter()
        .find(|m| m.endpoint.as_deref() == Some("b200"))
        .unwrap();
    assert_eq!(b200.prompt_tokens, 2000);
    assert_eq!(b200.ttft_ms_total, 600);
    let null_endpoint = stats
        .per_model
        .iter()
        .find(|m| m.endpoint.is_none())
        .unwrap();
    assert_eq!(null_endpoint.prompt_tokens, 500);
    assert_eq!(null_endpoint.timed_requests, 0);
}

#[tokio::test]
async fn project_stats_aggregates_across_sessions_and_days() {
    let store = make_store();
    let s1 = store.start_session("proj").await.unwrap();
    let s2 = store.start_session("proj").await.unwrap();
    // Day 1 (5000s) — two requests in s1.
    record(
        &store,
        Some(&s1.id),
        "gpt-4o",
        None,
        1000,
        200,
        0,
        100,
        Some(420),
        Some(2000),
        5000,
    )
    .await;
    record(
        &store,
        Some(&s1.id),
        "gpt-4o",
        None,
        2000,
        400,
        0,
        0,
        Some(500),
        Some(4000),
        5000,
    )
    .await;
    // Day 2 (86400 + 5000s) — one request in s2.
    record(
        &store,
        Some(&s2.id),
        "gpt-4o-mini",
        None,
        500,
        100,
        0,
        0,
        Some(300),
        Some(1000),
        86400 + 5000,
    )
    .await;

    let stats = store.project_stats().await.unwrap();
    assert_eq!(stats.session_count, 2);
    assert_eq!(stats.request_count, 3);
    assert_eq!(stats.prompt_tokens, 3500);
    assert_eq!(stats.cached_tokens, 100);
    assert_eq!(stats.ttft_ms_total, 1220);
    assert_eq!(stats.generation_ms_total, 7000);
    assert_eq!(stats.timed_requests, 3);
    // Per-model.
    assert_eq!(stats.per_model.len(), 2);
    // Per-day — two distinct days (86400s apart).
    assert_eq!(stats.per_day.len(), 2);
    let day1 = stats.per_day.iter().find(|d| d.day == 0).unwrap();
    assert_eq!(day1.request_count, 2);
    assert_eq!(day1.prompt_tokens, 3000);
    let day2 = stats.per_day.iter().find(|d| d.day == 86400).unwrap();
    assert_eq!(day2.request_count, 1);
    assert_eq!(day2.prompt_tokens, 500);
}

#[tokio::test]
async fn project_stats_splits_same_model_across_endpoints() {
    let store = make_store();
    let s1 = store.start_session("proj").await.unwrap();
    let s2 = store.start_session("proj").await.unwrap();
    // The same model id served by two endpoints across two sessions — the
    // project-wide breakdown must split them exactly like session_stats.
    record(
        &store,
        Some(&s1.id),
        "glm-5.3",
        Some("a100"),
        1000,
        200,
        0,
        0,
        Some(400),
        Some(2000),
        5000,
    )
    .await;
    record(
        &store,
        Some(&s2.id),
        "glm-5.3",
        Some("b200"),
        2000,
        400,
        0,
        0,
        Some(600),
        Some(4000),
        86400 + 5000,
    )
    .await;

    let stats = store.project_stats().await.unwrap();
    assert_eq!(stats.request_count, 2);
    assert_eq!(stats.per_model.len(), 2);
    let a100 = stats
        .per_model
        .iter()
        .find(|m| m.endpoint.as_deref() == Some("a100"))
        .unwrap();
    assert_eq!(a100.prompt_tokens, 1000);
    let b200 = stats
        .per_model
        .iter()
        .find(|m| m.endpoint.as_deref() == Some("b200"))
        .unwrap();
    assert_eq!(b200.prompt_tokens, 2000);
    assert_eq!(b200.request_count, 1);
}

#[tokio::test]
async fn session_list_orders_newest_first_with_token_totals() {
    let store = make_store();
    let s1 = store.start_session("proj").await.unwrap();
    // Force s2 to have a later created_at by recording some events first
    // (the clock advances on each store call that uses self.now()).
    record(
        &store,
        Some(&s1.id),
        "gpt-4o",
        None,
        100,
        20,
        0,
        0,
        None,
        None,
        100,
    )
    .await;
    let s2 = store.start_session("proj").await.unwrap();
    record(
        &store,
        Some(&s2.id),
        "gpt-4o",
        None,
        500,
        80,
        10,
        0,
        None,
        None,
        200,
    )
    .await;

    let list = store.session_list().await.unwrap();
    // Newest first → s2 (created later) before s1.
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].session_id, s2.id);
    assert_eq!(list[0].prompt_tokens, 500);
    assert_eq!(list[0].reasoning_tokens, 10);
    assert_eq!(list[1].session_id, s1.id);
    assert_eq!(list[1].prompt_tokens, 100);
}

#[tokio::test]
async fn session_stats_for_unknown_session_returns_zero_with_no_metadata() {
    // session_stats does a session-metadata lookup; an unknown id errors.
    let store = make_store();
    let result = store.session_stats("nonexistent").await;
    assert!(
        result.is_err(),
        "session_stats should error on an unknown session id"
    );
}

#[tokio::test]
async fn project_stats_empty_project_is_zero() {
    let store = make_store();
    let stats = store.project_stats().await.unwrap();
    assert_eq!(stats.session_count, 0);
    assert_eq!(stats.request_count, 0);
    assert!(stats.per_model.is_empty());
    assert!(stats.per_day.is_empty());
}

#[tokio::test]
async fn compaction_checkpoint_round_trips_through_the_archive() {
    // Backlog e4a50d22 lever 4: the region compaction is about to drop is
    // archived under the `compaction_checkpoint` tool label BEFORE the summary
    // replaces it, so the pointer handed to the model resolves back to the
    // identical text (and the row stays keyword-searchable like lever 3's).
    let store = MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap();
    let region = "#1 [User] DROPPED_ALPHA\n#2 [Assistant] DROPPED_BETA\n".to_string();
    let id = store
        .archive_tool_result(
            Some("s1"),
            "compaction_checkpoint",
            Some("pre-compaction context"),
            &region,
        )
        .await
        .unwrap();
    let fetched = store.expand_tool_result(&id).await.unwrap().expect("row exists");
    assert_eq!(fetched.content, region, "the checkpoint comes back byte-identical");
    assert_eq!(fetched.tool.as_deref(), Some("compaction_checkpoint"));
    assert_eq!(fetched.detail.as_deref(), Some("pre-compaction context"));
    let hits = store.search_archive("DROPPED_ALPHA", 10).await.unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].id, id);
}
