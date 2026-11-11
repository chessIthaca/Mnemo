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
    let mut memory = Memory::new(MemoryTier::Semantic, "HOW: f", "content", 1000);
    memory.record_class = MemoryClass::Derived;
    store.write(memory).await.unwrap();
    store.primer_records(8).await.unwrap();

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
async fn stale_record_ranks_below_fresh_of_equal_relevance() {
    // Backlog 4100f5e3 pillar B: closed-session digests dominated the
    // injected recall block because they ride surface similarity + keyword
    // + tier long after their strength term decayed to ~0 (the peek paths
    // never bump last_accessed_at, so it stays frozen at write time). The
    // staleness penalty sinks a record not accessed within the grace
    // window, so a fresh record of equal relevance outranks it.
    let embedder: Arc<dyn Embedder> = Arc::new(AnchorEmbedder { model_id: "anchor-real" });
    let now_val = Arc::new(AtomicI64::new(1000));
    let clock = now_val.clone();
    let store = MemoryStore::open_in_memory_with_clock(embedder, move || {
        clock.load(Ordering::SeqCst)
    })
    .unwrap();
    const DAY: i64 = 86_400;

    // The stale record: fully query-aligned (sim 1.0 under the anchor
    // embedder), 60 days unaccessed — the surface-similarity dominator
    // the old ranking rewarded. Strength is pinned on both records so the
    // arithmetic is exact.
    let mut stale = Memory::new(
        MemoryTier::Semantic,
        "stale review digest",
        "pipeline pipeline",
        1000,
    );
    stale.strength = 0.5;
    store.write(stale).await.unwrap();

    // 60 days later: a fresh record, lower similarity (axis-mixed content,
    // sim ~0.447) but equal keyword/tier/class and a live strength term.
    now_val.store(1000 + 60 * DAY, Ordering::SeqCst);
    let mut fresh = Memory::new(
        MemoryTier::Semantic,
        "fresh pipeline note",
        "pipeline unrelated unrelated",
        1000 + 60 * DAY,
    );
    fresh.strength = 0.5;
    store.write(fresh).await.unwrap();

    let results = store
        .recall_peek("pipeline", &MemoryFilter::new())
        .await
        .unwrap();
    assert_eq!(results.len(), 2, "both records are recall candidates");
    assert_eq!(
        results[0].memory.title, "fresh pipeline note",
        "the fresh record must outrank the stale one of equal relevance"
    );
}

#[tokio::test]
async fn an_unconfirmed_row_ranks_below_an_equal_confirmed_one() {
    // Plan 373dec00 step 4: confirmation is a +0.05 BONUS, never a penalty.
    // Two rows of identical text and strength differ only in `data.evidence`,
    // so the bonus alone decides the order — and the unconfirmed row keeps
    // the score it always had (it is not pushed down).
    let store = make_store();
    let mut confirmed = Memory::new(
        MemoryTier::Semantic,
        "clamp fact",
        "the clamp guards the pipeline",
        1000,
    );
    confirmed.data = serde_json::json!({ "evidence": "tests/clamp.rs::guards" });
    store.write(confirmed).await.unwrap();
    let unconfirmed = Memory::new(
        MemoryTier::Semantic,
        "clamp fact",
        "the clamp guards the pipeline",
        1000,
    );
    store.write(unconfirmed).await.unwrap();

    let results = store
        .recall_peek("clamp pipeline", &MemoryFilter::new())
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert!(
        results[0].memory.is_confirmed(),
        "the confirmed row takes the top slot: {:?}",
        results.iter().map(|r| &r.memory.title).collect::<Vec<_>>()
    );
    assert!(
        results[1].memory.is_unconfirmed(),
        "the other row is the unconfirmed one"
    );
    // Equal text: the delta is exactly the bonus, not a downrank of the peer.
    // Tiebreaker-sized (0.005): the eval showed a 0.05 bonus reordered
    // non-ties and cost hit@5 (0.400 -> 0.325) — most BUG digests and
    // extracted facts count as confirmed at once, so a big bonus acts as a
    // class re-weighting instead of a tiebreak.
    let delta = results[0].score - results[1].score;
    assert!(
        (delta - 0.005).abs() < 1e-9,
        "the confirmed row gains exactly 0.005, got {delta}"
    );
}

#[test]
fn a_bug_with_a_regression_test_counts_as_confirmed() {
    let mut bug = Memory::new(
        MemoryTier::Semantic,
        "BUG: os error 5 on the rebuild",
        "root cause and fix in the file",
        1000,
    );
    bug.record_type = MemoryRecordType::Bug;
    // Nothing in the text names a regression test: still unconfirmed.
    assert!(!bug.is_confirmed());
    assert!(bug.is_unconfirmed());
    // The indexer's finding (`data.regression_test`) confirms it.
    bug.data = serde_json::json!({ "regression_test": true });
    assert!(bug.is_confirmed());
    assert!(!bug.is_unconfirmed());
    // So does the record's own text naming one.
    let mut named = Memory::new(
        MemoryTier::Semantic,
        "BUG: os error 5 on the rebuild",
        "regression test: rebuild_keeps_the_lock",
        1000,
    );
    named.record_type = MemoryRecordType::Bug;
    assert!(named.is_confirmed());
    // The same text on a non-BUG record is not a confirmation.
    let mut spec = Memory::new(
        MemoryTier::Semantic,
        "SPEC: the rebuild",
        "regression test: rebuild_keeps_the_lock",
        1000,
    );
    spec.record_type = MemoryRecordType::Spec;
    assert!(!spec.is_confirmed());
}

#[test]
fn a_digest_row_is_never_marked_unconfirmed() {
    // PLAN/REVIEW digests point at a file rather than claiming something of
    // their own, and episodic rows are history: none of them is ever marked,
    // with or without evidence.
    for record_type in [MemoryRecordType::Plan, MemoryRecordType::Review] {
        let mut digest = Memory::new(
            MemoryTier::Semantic,
            "PLAN: some plan",
            "the plan's gist",
            1000,
        );
        digest.record_type = record_type;
        assert!(!digest.is_claim(), "{record_type} is not a claim");
        assert!(!digest.is_unconfirmed(), "{record_type} is never marked");
    }
    for tier in [MemoryTier::Episodic, MemoryTier::Working] {
        let mut row = Memory::new(tier, "Session summary", "worked on the clamp", 1000);
        row.record_type = MemoryRecordType::None;
        assert!(!row.is_unconfirmed(), "{tier} is never marked");
    }
}

#[tokio::test]
async fn staleness_grace_follows_the_configured_decay_tau() {
    // Review 084978b3 LOW-2: the staleness grace was hardcoded at 7 days
    // while the adjacent strength-decay tau is live-configurable — a 30-day
    // half-life still saw penalties begin at 7 days. The grace must track
    // the configured tau. Identical text on both records isolates the
    // strength + penalty terms: the 20-day-old record (strength 1.0,
    // decayed term 0.154 under tau=30) beats the fresh pinned 0.35
    // (term 0.105) — but only while its grace window exempts it; under the
    // pre-fix constant grace the 0.141 penalty sinks it.
    let embedder: Arc<dyn Embedder> = Arc::new(AnchorEmbedder { model_id: "anchor-real" });
    let now_val = Arc::new(AtomicI64::new(1000));
    let clock = now_val.clone();
    let store = MemoryStore::open_in_memory_with_clock(embedder, move || {
        clock.load(Ordering::SeqCst)
    })
    .unwrap();
    const DAY: i64 = 86_400;

    let mut old = Memory::new(
        MemoryTier::Semantic,
        "old pipeline digest",
        "pipeline pipeline",
        1000,
    );
    old.strength = 1.0;
    store.write(old).await.unwrap();

    now_val.store(1000 + 20 * DAY, Ordering::SeqCst);
    let mut fresh = Memory::new(
        MemoryTier::Semantic,
        "fresh pipeline note",
        "pipeline pipeline",
        1000 + 20 * DAY,
    );
    fresh.strength = 0.35;
    store.write(fresh).await.unwrap();

    // Configured tau = 30 days: the 20-day-old record is inside its grace
    // window — no penalty — and keeps the top slot.
    store.set_memory_search_config(MemorySearchConfig {
        decay_half_life_days: 30.0,
        ..MemorySearchConfig::default()
    });
    let results = store
        .recall_peek("pipeline", &MemoryFilter::new())
        .await
        .unwrap();
    assert_eq!(results.len(), 2, "both records are recall candidates");
    assert_eq!(
        results[0].memory.title, "old pipeline digest",
        "a record within the configured tau's grace must not be de-ranked"
    );

    // Default 7-day tau: the same record is deep in the ramp and sinks.
    store.set_memory_search_config(MemorySearchConfig::default());
    let results = store
        .recall_peek("pipeline", &MemoryFilter::new())
        .await
        .unwrap();
    assert_eq!(
        results[0].memory.title, "fresh pipeline note",
        "under the default tau the aged record sinks again"
    );
}

#[tokio::test]
async fn old_typed_knowledge_beats_a_fresh_unrelated_fact() {
    // Plan 373dec00 step 11 (the eval's title control): age was worth up to
    // 0.55 in the score — 0.3 of decayed strength plus 0.25 of staleness —
    // while a perfect semantic match was worth 0.35, so a durable record
    // nobody had searched for lost to an unrelated row touched today.
    // SPEC / DECISION / BUG / HOW are timeless: they keep their write-time
    // strength and take no staleness penalty.
    let embedder: Arc<dyn Embedder> = Arc::new(AnchorEmbedder { model_id: "anchor-real" });
    let now_val = Arc::new(AtomicI64::new(1000));
    let clock = now_val.clone();
    let store = MemoryStore::open_in_memory_with_clock(embedder, move || {
        clock.load(Ordering::SeqCst)
    })
    .unwrap();
    const DAY: i64 = 86_400;

    // The durable record: its title IS the query, last accessed 90 days ago
    // (the peek paths never bump it). Strength pinned at 1.0 so the
    // arithmetic is exact — unexempted, the decayed strength term is ~0 and
    // the staleness ramp takes the full 0.25.
    let mut spec = Memory::new(
        MemoryTier::Semantic,
        "SPEC: pipeline stages",
        "pipeline pipeline",
        1000,
    );
    spec.strength = 1.0;
    store.write(spec).await.unwrap();

    // 90 days later, a fresh untyped fact sharing the query's word: same
    // keyword boost and tier weight, only partial similarity (sim ~0.707).
    now_val.store(1000 + 90 * DAY, Ordering::SeqCst);
    let mut fresh = Memory::new(
        MemoryTier::Semantic,
        "unrelated note",
        "unrelated pipeline",
        1000 + 90 * DAY,
    );
    fresh.strength = 1.0;
    store.write(fresh).await.unwrap();

    let results = store
        .recall_peek("pipeline", &MemoryFilter::new())
        .await
        .unwrap();
    assert_eq!(results.len(), 2, "both records are recall candidates");
    assert_eq!(
        results[0].memory.title, "SPEC: pipeline stages",
        "the 90-day-old SPEC record must outrank the fresh untyped fact"
    );
}

#[tokio::test]
async fn old_knowledge_keeps_its_relevance_order() {
    // The timeless exemption changes what age is worth, not relevance: two
    // 90-day-old DECISION records still rank by how well they answer the
    // query, the aligned one first.
    let embedder: Arc<dyn Embedder> = Arc::new(AnchorEmbedder { model_id: "anchor-real" });
    let now_val = Arc::new(AtomicI64::new(1000));
    let clock = now_val.clone();
    let store = MemoryStore::open_in_memory_with_clock(embedder, move || {
        clock.load(Ordering::SeqCst)
    })
    .unwrap();
    const DAY: i64 = 86_400;

    let mut matching = Memory::new(
        MemoryTier::Semantic,
        "DECISION: pipeline stages",
        "pipeline pipeline",
        1000,
    );
    matching.strength = 1.0;
    store.write(matching).await.unwrap();

    // The sibling answers less well: its content mixes axes (sim ~0.577).
    let mut sibling = Memory::new(
        MemoryTier::Semantic,
        "DECISION: pipeline archive",
        "pipeline unrelated unrelated",
        1000,
    );
    sibling.strength = 1.0;
    store.write(sibling).await.unwrap();

    // Both are 90 days unaccessed when the recall runs.
    now_val.store(1000 + 90 * DAY, Ordering::SeqCst);

    let results = store
        .recall_peek("pipeline", &MemoryFilter::new())
        .await
        .unwrap();
    assert_eq!(results.len(), 2, "both records are recall candidates");
    assert_eq!(
        results[0].memory.title, "DECISION: pipeline stages",
        "among timeless records the better answer still ranks first"
    );
}

#[tokio::test]
async fn old_episodic_digests_still_age() {
    // A session digest is a snapshot of a moment, not durable knowledge: the
    // timeless exemption must not leak to the episodic tier, so an aged digest
    // still sinks below a fresh one of equal relevance.
    let embedder: Arc<dyn Embedder> = Arc::new(AnchorEmbedder { model_id: "anchor-real" });
    let now_val = Arc::new(AtomicI64::new(1000));
    let clock = now_val.clone();
    let store = MemoryStore::open_in_memory_with_clock(embedder, move || {
        clock.load(Ordering::SeqCst)
    })
    .unwrap();
    const DAY: i64 = 86_400;

    let mut old = Memory::new(
        MemoryTier::Episodic,
        "old session digest",
        "pipeline pipeline",
        1000,
    );
    old.strength = 1.0;
    store.write(old).await.unwrap();

    now_val.store(1000 + 90 * DAY, Ordering::SeqCst);
    let mut fresh = Memory::new(
        MemoryTier::Episodic,
        "fresh session digest",
        "pipeline pipeline",
        1000 + 90 * DAY,
    );
    fresh.strength = 1.0;
    store.write(fresh).await.unwrap();

    let results = store
        .recall_peek("pipeline", &MemoryFilter::new())
        .await
        .unwrap();
    assert_eq!(results.len(), 2, "both digests are recall candidates");
    assert_eq!(
        results[0].memory.title, "fresh session digest",
        "an aged episodic digest still sinks below a fresh one of equal relevance"
    );
}

#[tokio::test]
async fn old_untyped_facts_still_age() {
    // An untyped fact is a note about a moment, not a typed durable record: it
    // keeps the old aging behaviour — no exemption for merely being factual.
    let embedder: Arc<dyn Embedder> = Arc::new(AnchorEmbedder { model_id: "anchor-real" });
    let now_val = Arc::new(AtomicI64::new(1000));
    let clock = now_val.clone();
    let store = MemoryStore::open_in_memory_with_clock(embedder, move || {
        clock.load(Ordering::SeqCst)
    })
    .unwrap();
    const DAY: i64 = 86_400;

    let mut old = Memory::new(
        MemoryTier::Semantic,
        "old pipeline digest",
        "pipeline pipeline",
        1000,
    );
    old.strength = 1.0;
    store.write(old).await.unwrap();

    now_val.store(1000 + 90 * DAY, Ordering::SeqCst);
    let mut fresh = Memory::new(
        MemoryTier::Semantic,
        "fresh pipeline digest",
        "pipeline pipeline",
        1000 + 90 * DAY,
    );
    fresh.strength = 1.0;
    store.write(fresh).await.unwrap();

    let results = store
        .recall_peek("pipeline", &MemoryFilter::new())
        .await
        .unwrap();
    assert_eq!(results.len(), 2, "both facts are recall candidates");
    assert_eq!(
        results[0].memory.title, "fresh pipeline digest",
        "an aged untyped fact still sinks below a fresh one of equal relevance"
    );
}

#[tokio::test]
async fn turn_boundary_auto_consolidates_previous_turns_working_rows() {
    // Backlog 4100f5e3 pillar C: a gap between a session's tool events
    // marks a turn boundary — the previous turn's working rows are
    // compressed into one episodic summary and deleted automatically,
    // without the manual consolidate tool or the stale-session cleanup.
    // The live turn's row stays.
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::new());
    let now_val = Arc::new(AtomicI64::new(1000));
    let clock = now_val.clone();
    let store = MemoryStore::open_in_memory_with_clock(embedder, move || {
        clock.load(Ordering::SeqCst)
    })
    .unwrap();

    // Turn one: two tool events a second apart.
    store
        .record_tool_event(
            Some("sess-t"),
            "file_read",
            serde_json::json!({"path": "a.rs"}),
            "a",
            None,
        )
        .await
        .unwrap();
    now_val.store(1001, Ordering::SeqCst);
    store
        .record_tool_event(
            Some("sess-t"),
            "file_write",
            serde_json::json!({"path": "b.rs"}),
            "b",
            None,
        )
        .await
        .unwrap();

    // 400 seconds later: the new tool event crosses the 300s turn gap.
    now_val.store(1401, Ordering::SeqCst);
    store
        .record_tool_event(
            Some("sess-t"),
            "file_read",
            serde_json::json!({"path": "c.rs"}),
            "c",
            None,
        )
        .await
        .unwrap();

    // The two pre-gap rows are gone from working; the live row remains.
    let working = store.list_by_tier(MemoryTier::Working).await.unwrap();
    assert_eq!(working.len(), 1, "only the live turn's row remains in working");
    assert_eq!(working[0].source_session_ids, vec!["sess-t".to_string()]);

    // The previous turn was compressed into one episodic summary.
    let episodic = store.list_by_tier(MemoryTier::Episodic).await.unwrap();
    assert_eq!(episodic.len(), 1, "the previous turn left an episodic summary");
    assert_eq!(episodic[0].title, "session sess-t summary");
    assert_eq!(episodic[0].source_session_ids, vec!["sess-t".to_string()]);
}

#[tokio::test]
async fn synthetic_summaries_do_not_compete_in_recall() {
    // Plan 373dec00 step 2: the turn-boundary compressor's synthetic rows
    // keep their job (bounding the working tier) but stop competing in
    // ranked recall — they used to crowd real records out of the top 5.
    let store = make_store();
    let synthetic_id = store
        .write(Memory::new(
            MemoryTier::Episodic,
            "session sess-t summary",
            "Session involved 2 tool calls across tools: file_read, file_write. \
             Files modified: routes.rs. The login route lives in routes.rs.",
            1000,
        ))
        .await
        .unwrap();
    let real_id = store
        .write(Memory::new(
            MemoryTier::Semantic,
            "login route location",
            "The login route lives in routes.rs; the trap is its middleware ordering.",
            1000,
        ))
        .await
        .unwrap();

    let hits = store
        .recall_peek("login route routes.rs", &MemoryFilter::new())
        .await
        .unwrap();
    assert!(
        hits.iter().any(|h| h.memory.id == real_id),
        "the real record still ranks: {hits:?}"
    );
    assert!(
        hits.iter().all(|h| h.memory.id != synthetic_id),
        "the synthetic summary must not compete in recall: {hits:?}"
    );

    // A query that names the episodic tier is still a search, not a browse:
    // the tier filter narrows the candidates, it does not resurrect noise.
    let hits = store
        .recall_peek(
            "login route routes.rs",
            &MemoryFilter::new().tier(MemoryTier::Episodic),
        )
        .await
        .unwrap();
    assert!(
        hits.iter().all(|h| h.memory.id != synthetic_id),
        "an explicit tier on a query does not resurrect synthetic summaries: {hits:?}"
    );

    // The bumping recall path shares the same SQL: same result.
    let hits = store
        .recall("login route routes.rs", &MemoryFilter::new())
        .await
        .unwrap();
    assert!(hits.iter().all(|h| h.memory.id != synthetic_id));
}

#[tokio::test]
async fn an_explicit_episodic_browse_still_lists_them() {
    // Plan 373dec00 step 2: an explicit episodic-tier browse with no query is
    // the one deliberate way back to a synthetic summary (the startup sweep
    // reads them through it). The default browse hides them.
    let store = make_store();
    let synthetic_id = store
        .write(Memory::new(
            MemoryTier::Episodic,
            "session sess-t summary",
            "Session involved 2 tool calls across tools: file_read.",
            1000,
        ))
        .await
        .unwrap();

    let browse = store
        .list_filtered(&MemoryFilter::new().tier(MemoryTier::Episodic))
        .await
        .unwrap();
    assert!(
        browse.iter().any(|m| m.id == synthetic_id),
        "an explicit episodic browse lists synthetic summaries: {browse:?}"
    );

    let default_browse = store.list_filtered(&MemoryFilter::new()).await.unwrap();
    assert!(
        default_browse.iter().all(|m| m.id != synthetic_id),
        "the default browse hides them: {default_browse:?}"
    );
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
async fn the_primer_prefers_searched_typed_records() {
    // The primer ranks by `access_count` — how often an explicit search
    // returned the row — then recency. Typed titles are required: untyped
    // rows are never primer material (plan 373dec00 step 6).
    let store = make_store();
    let mut first = Memory::new(
        MemoryTier::Semantic,
        "HOW: postgres for storage",
        "the project uses postgres for storage",
        1000,
    );
    first.record_class = MemoryClass::Derived;
    store.write(first).await.unwrap();
    let mut second = Memory::new(
        MemoryTier::Semantic,
        "DECISION: sqlite for storage",
        "we use sqlite for storage",
        2000,
    );
    second.record_class = MemoryClass::Derived;
    store.write(second).await.unwrap();

    // One explicit search for the older record: only it gets counted.
    store
        .recall("postgres", &MemoryFilter::new())
        .await
        .unwrap();

    let primer = store.primer_records(8).await.unwrap();
    assert_eq!(
        primer
            .iter()
            .map(|memory| memory.title.as_str())
            .collect::<Vec<_>>(),
        vec!["HOW: postgres for storage", "DECISION: sqlite for storage"],
        "the searched record leads despite being older"
    );
}

#[tokio::test]
async fn primer_records_excludes_superseded_memories() {
    // The primer is the standing context of every new session — a superseded
    // record must never surface there (the same invariant recall and
    // list_filtered honor; the primer path was the gap before plan 373dec00
    // step 6).
    let store = make_store();
    let mut old = Memory::new(
        MemoryTier::Semantic,
        "DECISION: storage engine",
        "we use postgres for storage",
        1000,
    );
    old.record_class = MemoryClass::Derived;
    let old_id = store.write(old).await.unwrap();

    // Before superseding: the row surfaces.
    let before = store.primer_records(8).await.unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].id, old_id);

    // Supersede the row with its successor.
    let mut successor = Memory::new(
        MemoryTier::Semantic,
        "DECISION: storage engine",
        "we use sqlite for storage",
        1001,
    );
    successor.record_class = MemoryClass::Derived;
    store
        .supersede_memory(&old_id, successor)
        .await
        .unwrap();

    // The superseded row leaves the primer entirely — only the live
    // successor remains.
    let after = store.primer_records(8).await.unwrap();
    assert_eq!(after.len(), 1, "superseded excluded from the primer");
    assert_ne!(after[0].id, old_id);
    assert!(after.iter().all(|m| m.superseded_by.is_none()));
}

#[tokio::test]
async fn primer_records_excludes_working_and_untyped_rows() {
    // A working row is never primer material, and neither is an untyped row
    // (the primer wants typed knowledge, not free-text facts). The typed
    // procedural record is the only one that surfaces.
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
            "a distilled but untyped fact",
            1000,
        ))
        .await
        .unwrap();
    let mut procedural = Memory::new(
        MemoryTier::Procedural,
        "HOW: release checklist",
        "a learned recurring workflow",
        1000,
    );
    procedural.record_class = MemoryClass::Derived;
    store.write(procedural).await.unwrap();

    let results = store.primer_records(8).await.unwrap();
    assert_eq!(results.len(), 1, "only the typed procedural row");
    assert_eq!(results[0].title, "HOW: release checklist");

    // A limit of 0 returns empty.
    let zero = store.primer_records(0).await.unwrap();
    assert!(zero.is_empty());
}

#[tokio::test]
async fn the_primer_excludes_bug_plan_and_untyped_rows() {
    // The primer is standing context: BUG and PLAN digests are history and
    // bookkeeping, not durable knowledge (plan 373dec00 step 6). SPEC,
    // DECISION and HOW survive.
    let store = make_store();
    for (title, tier) in [
        ("SPEC: the contract", MemoryTier::Semantic),
        ("DECISION: the choice", MemoryTier::Semantic),
        ("HOW: the procedure", MemoryTier::Procedural),
        ("BUG: an old defect", MemoryTier::Semantic),
        ("PLAN: an old plan", MemoryTier::Semantic),
        ("REVIEW: an old review", MemoryTier::Semantic),
        ("untyped fact", MemoryTier::Semantic),
    ] {
        // The primer only takes file-backed (derived) rows, so the typed ones
        // are marked derived here; the type filter is what this test probes.
        let mut memory = Memory::new(tier, title, "content", 1000);
        memory.record_class = MemoryClass::Derived;
        store.write(memory).await.unwrap();
    }

    let mut primer: Vec<String> = store
        .primer_records(8)
        .await
        .unwrap()
        .into_iter()
        .map(|memory| memory.title)
        .collect();
    // The rows share one timestamp here, so this test is about exclusion, not
    // order (the ordering contract has its own test above).
    primer.sort();
    assert_eq!(
        primer,
        vec![
            "DECISION: the choice",
            "HOW: the procedure",
            "SPEC: the contract"
        ],
        "only the typed knowledge rows survive"
    );
}

#[tokio::test]
async fn the_primer_skips_file_less_stale_and_status_first_rows() {
    // Standing context must never lead a session with the wrong thing (plan
    // 373dec00 step 6 review): a typed authored row with no knowledge file is
    // hearsay, a row the check pass stamped `moved_paths` is stale, and a
    // legacy git-state digest opens with landing status. Only the clean,
    // file-backed row is primer material.
    let store = make_store();

    // Authored (no file behind it).
    store
        .write(Memory::new(
            MemoryTier::Semantic,
            "DECISION: file-less",
            "no path backs this row",
            1000,
        ))
        .await
        .unwrap();

    // Derived, but the check pass found the cited path gone.
    let mut stale = Memory::new(
        MemoryTier::Semantic,
        "SPEC: stale",
        "cites src/gone.rs",
        1001,
    );
    stale.record_class = MemoryClass::Derived;
    stale.data = serde_json::json!({
        "moved_paths": [{ "from": "src/gone.rs", "to": null }]
    });
    store.write(stale).await.unwrap();

    // Derived, but a legacy landing-status digest.
    let mut status_first = Memory::new(
        MemoryTier::Semantic,
        "HOW: landed",
        "MERGED into main at e37d07a on 2027-01-24; branch wt/x deleted",
        1002,
    );
    status_first.record_class = MemoryClass::Derived;
    store.write(status_first).await.unwrap();

    // The one that belongs.
    let mut clean = Memory::new(
        MemoryTier::Semantic,
        "HOW: clean",
        "a durable, file-backed procedure",
        1003,
    );
    clean.record_class = MemoryClass::Derived;
    store.write(clean).await.unwrap();

    let primer: Vec<String> = store
        .primer_records(8)
        .await
        .unwrap()
        .into_iter()
        .map(|memory| memory.title)
        .collect();
    assert_eq!(primer, vec!["HOW: clean"]);
}

#[tokio::test]
async fn note_recalled_counts_without_touching_last_accessed() {
    // The auto-recall counter (plan 373dec00 step 6) is a separate measure
    // from the explicit-read counters: the peek path stays read-only, and the
    // turn loop's `note_recalled` bumps only `recall_count`.
    let store = make_store();
    let id = store
        .write(Memory::new(
            MemoryTier::Semantic,
            "SPEC: storage",
            "we use sqlite",
            1000,
        ))
        .await
        .unwrap();
    let other = store
        .write(Memory::new(
            MemoryTier::Semantic,
            "SPEC: other",
            "another fact",
            1001,
        ))
        .await
        .unwrap();
    let before = store.get_memory(&id).await.unwrap().unwrap();
    let other_before = store.get_memory(&other).await.unwrap().unwrap();

    // One batch covers both ids (and tolerates an id that does not exist); a
    // second recall counts only the first row again.
    store
        .note_recalled(&[id.clone(), other.clone(), "does-not-exist".to_string()])
        .await
        .unwrap();
    store.note_recalled(std::slice::from_ref(&id)).await.unwrap();
    let after = store.get_memory(&id).await.unwrap().unwrap();
    let other_after = store.get_memory(&other).await.unwrap().unwrap();
    assert_eq!(
        after.access_count, before.access_count,
        "an explicit-read counter is not touched"
    );
    assert_eq!(
        after.last_accessed_at, before.last_accessed_at,
        "recency is not touched"
    );
    assert_eq!(
        other_after.access_count, other_before.access_count,
        "the other row's explicit-read counter is untouched"
    );
    assert_eq!(
        store.top_recalled(10).await.unwrap(),
        vec![("SPEC: storage".to_string(), 2), ("SPEC: other".to_string(), 1)]
    );
}

#[tokio::test]
async fn note_recalled_does_not_bump_the_store_version() {
    // The agent's per-turn recall cache keys on `version()`; a recall counter
    // bump must not invalidate it (turn.rs's cache would re-recall every turn).
    let store = make_store();
    let id = store
        .write(Memory::new(MemoryTier::Semantic, "HOW: v", "c", 1000))
        .await
        .unwrap();
    let version = store.version();
    store.note_recalled(&[id]).await.unwrap();
    assert_eq!(store.version(), version);
    assert_eq!(store.top_recalled(10).await.unwrap().len(), 1);
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
            format!("mem-{i}"),
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

/// The token/timing numbers of one stats row [`record`] files — grouped so
/// the helper stays under clippy's argument budget: prompt/completion/
/// reasoning/cached tokens, TTFT ms, generation ms, epoch seconds.
struct StatsRow {
    p: u32,
    c: u32,
    r: u32,
    cached: u32,
    ttft: Option<u32>,
    gen: Option<u32>,
    at: i64,
}

/// A helper that records a stats row with sensible defaults.
async fn record(
    store: &MemoryStore,
    session_id: Option<&str>,
    model: &str,
    endpoint: Option<&str>,
    row: StatsRow,
) {
    let StatsRow { p, c, r, cached, ttft, gen, at } = row;
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
    // The raw row reader still returns a retired kind; only the reports skip it.
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
async fn spend_events_round_trip_by_plan() {
    // Backlog a25a5323: the budget layer's per-decision ledger persists the
    // plan/step/lane/model attribution and the token cost, and the reader
    // scopes to ONE plan (the per-plan cost report's source).
    let store = MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap();
    let now = 1_700_000_000;
    let row = |plan: &str, reason: &str, lane: Option<&str>, at: i64| SpendEvent {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: Some("s1".into()),
        turn_id: Some("turn-1".into()),
        agent_id: Some("agent-1".into()),
        plan_id: plan.into(),
        step_index: Some(2),
        lane: lane.map(|l| l.to_string()),
        model: "worker-model".into(),
        reason: reason.into(),
        tokens_in: 1_200,
        tokens_out: 300,
        cached_tokens: Some(800),
        detail: None,
        created_at: at,
    };
    store
        .record_spend_event(&row("plan-a", "route", Some("medium"), now))
        .await
        .unwrap();
    store
        .record_spend_event(&row("plan-a", "retry", Some("high"), now + 1))
        .await
        .unwrap();
    store
        .record_spend_event(&row("plan-b", "default", None, now + 2))
        .await
        .unwrap();
    // A harness-verify evidence row: no turn, lane or step — the NULL
    // columns must round-trip like every other row.
    store
        .record_spend_event(&SpendEvent {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: None,
            turn_id: None,
            agent_id: None,
            plan_id: "plan-a".into(),
            step_index: None,
            lane: None,
            model: "cargo test".into(),
            reason: "verify".into(),
            tokens_in: 0,
            tokens_out: 0,
            cached_tokens: None,
            detail: Some("[verify] `cargo test` — FAILED (exit 101)".into()),
            created_at: now + 3,
        })
        .await
        .unwrap();

    let a = store.spend_rows_for_plan("plan-a").await.unwrap();
    assert_eq!(a.len(), 3);
    assert_eq!(a[0].reason, "route");
    assert_eq!(a[0].lane.as_deref(), Some("medium"));
    assert_eq!(a[0].step_index, Some(2));
    assert_eq!(a[0].model, "worker-model");
    assert_eq!(a[0].tokens_in, 1_200);
    assert_eq!(a[0].tokens_out, 300);
    assert_eq!(a[0].cached_tokens, Some(800));
    assert_eq!(a[1].reason, "retry");
    assert_eq!(a[1].lane.as_deref(), Some("high"));
    // The verify-shaped row survives with its NULL columns.
    assert_eq!(a[2].reason, "verify");
    assert!(a[2].turn_id.is_none());
    assert!(a[2].lane.is_none());
    assert!(a[2].step_index.is_none());
    assert_eq!(
        a[2].detail.as_deref(),
        Some("[verify] `cargo test` — FAILED (exit 101)")
    );

    // The reader scopes to ONE plan, and an unknown plan reads empty —
    // never an error.
    let b = store.spend_rows_for_plan("plan-b").await.unwrap();
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].reason, "default");
    assert!(b[0].lane.is_none());
    assert!(store
        .spend_rows_for_plan("plan-none")
        .await
        .unwrap()
        .is_empty());
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
        .record_savings_event(&record("outline", 12_000, 900, day1))
        .await
        .unwrap();
    store
        .record_savings_event(&record("compression", 8_000, 1_200, day1 + 60))
        .await
        .unwrap();
    store
        .record_savings_event(&record("outline", 2_000, 400, day2))
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
    assert_eq!(stats.per_kind[0].kind, "outline");
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
        StatsRow {
            p: 1000,
            c: 200,
            r: 0,
            cached: 800,
            ttft: Some(420),
            gen: Some(2000),
            at: now,
        },
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
        StatsRow {
            p: 1000,
            c: 200,
            r: 50,
            cached: 800,
            ttft: Some(420),
            gen: Some(2000),
            at: 5000,
        },
    )
    .await;
    record(
        &store,
        Some(&session.id),
        "gpt-4o",
        None,
        StatsRow {
            p: 2000,
            c: 400,
            r: 100,
            cached: 0,
            ttft: Some(500),
            gen: Some(4000),
            at: 6000,
        },
    )
    .await;
    record(
        &store,
        Some(&session.id),
        "gpt-4o-mini",
        None,
        StatsRow {
            p: 500,
            c: 100,
            r: 0,
            cached: 0,
            ttft: None,
            gen: None,
            at: 7000,
        },
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
        StatsRow {
            p: 1000,
            c: 200,
            r: 0,
            cached: 0,
            ttft: Some(400),
            gen: Some(2000),
            at: 5000,
        },
    )
    .await;
    record(
        &store,
        Some(&session.id),
        "glm-5.3",
        Some("b200"),
        StatsRow {
            p: 2000,
            c: 400,
            r: 0,
            cached: 0,
            ttft: Some(600),
            gen: Some(4000),
            at: 6000,
        },
    )
    .await;
    record(
        &store,
        Some(&session.id),
        "glm-5.3",
        None,
        StatsRow {
            p: 500,
            c: 100,
            r: 0,
            cached: 0,
            ttft: None,
            gen: None,
            at: 7000,
        },
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
        StatsRow {
            p: 1000,
            c: 200,
            r: 0,
            cached: 100,
            ttft: Some(420),
            gen: Some(2000),
            at: 5000,
        },
    )
    .await;
    record(
        &store,
        Some(&s1.id),
        "gpt-4o",
        None,
        StatsRow {
            p: 2000,
            c: 400,
            r: 0,
            cached: 0,
            ttft: Some(500),
            gen: Some(4000),
            at: 5000,
        },
    )
    .await;
    // Day 2 (86400 + 5000s) — one request in s2.
    record(
        &store,
        Some(&s2.id),
        "gpt-4o-mini",
        None,
        StatsRow {
            p: 500,
            c: 100,
            r: 0,
            cached: 0,
            ttft: Some(300),
            gen: Some(1000),
            at: 86400 + 5000,
        },
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
        StatsRow {
            p: 1000,
            c: 200,
            r: 0,
            cached: 0,
            ttft: Some(400),
            gen: Some(2000),
            at: 5000,
        },
    )
    .await;
    record(
        &store,
        Some(&s2.id),
        "glm-5.3",
        Some("b200"),
        StatsRow {
            p: 2000,
            c: 400,
            r: 0,
            cached: 0,
            ttft: Some(600),
            gen: Some(4000),
            at: 86400 + 5000,
        },
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
        StatsRow {
            p: 100,
            c: 20,
            r: 0,
            cached: 0,
            ttft: None,
            gen: None,
            at: 100,
        },
    )
    .await;
    let s2 = store.start_session("proj").await.unwrap();
    record(
        &store,
        Some(&s2.id),
        "gpt-4o",
        None,
        StatsRow {
            p: 500,
            c: 80,
            r: 10,
            cached: 0,
            ttft: None,
            gen: None,
            at: 200,
        },
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

#[tokio::test]
async fn savings_stats_clamps_archive_and_compression_to_the_result_cap() {
    // A tool result was already capped at TOOL_RESULT_MAX_CHARS (100,000 chars
    // = 25,000 tokens) before any lever ran, so no row may claim more. The
    // clamp also telescopes a compressed-then-archived result into ONE saving
    // instead of crediting both steps.
    let store = MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap();
    let day = 1_700_000_000_i64;
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
        .record_savings_event(&record("compression", 130_000, 9_000, day))
        .await
        .unwrap();
    store
        .record_savings_event(&record("archive", 9_000, 500, day + 60))
        .await
        .unwrap();
    store
        .record_savings_event(&record("archive", 100_000, 500, day + 120))
        .await
        .unwrap();
    store
        .record_savings_event(&record("outline", 12_000, 900, day + 180))
        .await
        .unwrap();

    let stats = store.savings_stats().await.unwrap();

    // compression: 25,000 - 9,000. archive: (9,000 - 500) + (25,000 - 500).
    // outline is not an archive kind, so its raw saving stands.
    assert_eq!(stats.saved_tokens_total, 16_000 + 33_000 + 11_100);
    assert_eq!(stats.per_kind.len(), 3);
    assert_eq!(stats.per_kind[0].kind, "archive");
    assert_eq!(stats.per_kind[0].saved_tokens, 33_000);
    assert_eq!(stats.per_kind[1].kind, "compression");
    assert_eq!(stats.per_kind[1].saved_tokens, 16_000);
    assert_eq!(stats.per_kind[2].kind, "outline");
    assert_eq!(stats.per_kind[2].saved_tokens, 11_100);
    // The per-day series sums to the same honest total.
    let per_day: i64 = stats.per_day.iter().map(|day| day.saved_tokens).sum();
    assert_eq!(per_day, stats.saved_tokens_total);
    // `recent` keeps the raw rows: the audit view shows what was recorded.
    let compression = stats
        .recent
        .iter()
        .find(|event| event.kind == "compression")
        .expect("the compression row is recent");
    assert_eq!(compression.tokens_saved, 121_000);
}

#[tokio::test]
async fn savings_reports_skip_retired_kinds() {
    // The delta-read and signature-skeleton levers are retired: their rows
    // stay in the ledger (the raw reader keeps them), but both reports —
    // the Cost tab's total, lever list, day chart, recent list and dollars —
    // list only the levers that still run.
    let store = MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap();
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
        .record_savings_event(&record("delta_read", 5_000, 1_000, day1))
        .await
        .unwrap();
    store
        .record_savings_event(&record("skeleton", 12_000, 900, day1 + 60))
        .await
        .unwrap();
    store
        .record_savings_event(&record("compression", 8_000, 1_200, day1 + 120))
        .await
        .unwrap();
    store
        .record_savings_event(&record("outline", 3_000, 500, day2))
        .await
        .unwrap();

    let stats = store.savings_stats().await.unwrap();
    assert_eq!(stats.event_count, 2);
    // 6_800 + 2_500: the retired rows' savings (4_000 and 11_100) do not count.
    assert_eq!(stats.saved_tokens_total, 6_800 + 2_500);
    let kinds: Vec<&str> = stats.per_kind.iter().map(|row| row.kind.as_str()).collect();
    assert_eq!(kinds, vec!["compression", "outline"]);
    assert_eq!(stats.per_day.len(), 2);
    assert_eq!(stats.per_day[0].day, day1 / 86_400);
    assert_eq!(stats.per_day[0].event_count, 1);
    assert_eq!(stats.per_day[0].saved_tokens, 6_800);
    assert_eq!(stats.per_day[1].day, day2 / 86_400);
    assert_eq!(stats.per_day[1].saved_tokens, 2_500);
    assert_eq!(stats.recent.len(), 2);
    assert!(stats
        .recent
        .iter()
        .all(|row| !RETIRED_SAVINGS_KINDS.contains(&row.kind.as_str())));

    let valuation = store.savings_valuation().await.unwrap();
    let mut valued: Vec<&str> = valuation.rows.iter().map(|row| row.kind.as_str()).collect();
    valued.sort();
    valued.dedup();
    assert_eq!(valued, vec!["compression", "outline"]);
    assert_eq!(
        store.savings_events_rows(Some("s1")).await.unwrap().len(),
        4,
        "the ledger keeps the retired rows"
    );
}

#[tokio::test]
async fn savings_valuation_counts_later_requests_up_to_the_lifetime() {
    let store = MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap();
    let base = 1_700_000_000_i64;
    let event = |kind: &str, session: Option<&str>, before: i64, after: i64, at: i64| {
        SavingsEvent {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session.map(str::to_string),
            kind: kind.into(),
            detail: None,
            tokens_before: before,
            tokens_after: after,
            tokens_saved: before - after,
            measured: false,
            created_at: at,
        }
    };
    // An early event with 20 requests after it: the credit stops at the
    // 15-request lifetime.
    store
        .record_savings_event(&event("archive", Some("s1"), 25_000, 0, base))
        .await
        .unwrap();
    // A late event with only two requests after it.
    store
        .record_savings_event(&event("outline", Some("s1"), 12_000, 900, base + 180))
        .await
        .unwrap();
    // A row with no session cannot be timed against any request.
    store
        .record_savings_event(&event("compression", None, 8_000, 1_200, base))
        .await
        .unwrap();
    // A compaction checkpoint is bookkeeping, not a lever.
    store
        .record_savings_event(&event("compaction_checkpoint", Some("s1"), 0, 0, base))
        .await
        .unwrap();
    let request = |at: i64| RequestStats {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: Some("s1".into()),
        model: "m".into(),
        endpoint: None,
        prompt_tokens: 10,
        completion_tokens: 1,
        reasoning_tokens: 0,
        cached_tokens: None,
        ttft_ms: None,
        generation_ms: None,
        created_at: at,
        outcome: None,
        purpose: None,
    };
    for i in 0..20 {
        store
            .record_request_stats(&request(base + 10 * (i + 1)))
            .await
            .unwrap();
    }

    let valuation = store.savings_valuation().await.unwrap();

    assert_eq!(valuation.rows.len(), 3, "{:?}", valuation.rows);
    assert!(
        valuation
            .rows
            .iter()
            .all(|row| row.kind != "compaction_checkpoint"),
        "the compaction checkpoint is not a lever row"
    );
    let archive = valuation
        .rows
        .iter()
        .find(|row| row.kind == "archive")
        .expect("the archive row is valued");
    assert_eq!(archive.later_requests, SAVED_TOKEN_LIFETIME_REQUESTS);
    assert_eq!(archive.events, 1);
    assert_eq!(archive.saved_tokens, 25_000);
    let outline = valuation
        .rows
        .iter()
        .find(|row| row.kind == "outline")
        .expect("the outline row is valued");
    assert_eq!(outline.later_requests, 2);
    let untimed = valuation
        .rows
        .iter()
        .find(|row| row.kind == "compression")
        .expect("the compression row is valued");
    assert_eq!(untimed.later_requests, 0);
    assert_eq!(untimed.session_id, None);
}

#[test]
fn cache_lifetime_from_gaps_finds_the_first_cold_window() {
    // Five warm samples in [300, 600) and five cold ones in [600, 1200): the
    // first probe whose window averages below 0.5 is the lifetime.
    let mut samples: Vec<(i64, f64)> = vec![(400, 0.9); 5];
    samples.extend(vec![(700, 0.2); 5]);
    assert_eq!(cache_lifetime_from_gaps(&samples), Some(600));
}

#[test]
fn cache_lifetime_from_gaps_skips_thin_windows() {
    // Probe 300's window holds only 2 cold samples (under the 3-sample
    // minimum); 600 and 900 hold none; 1200's window [1200, 2400) holds the
    // four cold ones.
    let mut samples: Vec<(i64, f64)> = vec![(400, 0.0); 2];
    samples.extend(vec![(2000, 0.1); 4]);
    assert_eq!(cache_lifetime_from_gaps(&samples), Some(1200));
}

#[test]
fn cache_lifetime_from_gaps_is_none_when_the_cache_stays_warm() {
    // A cache that outlives an hour never decides; neither does no data.
    assert_eq!(cache_lifetime_from_gaps(&[(4000, 0.95); 10]), None);
    assert_eq!(cache_lifetime_from_gaps(&[]), None);
}

#[tokio::test]
async fn cache_lifetime_secs_measures_one_model_from_the_request_log() {
    let store = MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap();
    // The store's clock is real time, so anchor the rows 20,000 s back: every
    // one stays inside the 30-day measurement window.
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let base = now_secs - 20_000;
    let request =
        |session: &str, model: &str, at: i64, prompt: u32, cached: u32, purpose: Option<&str>| {
            RequestStats {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: Some(session.into()),
                model: model.into(),
                endpoint: None,
                prompt_tokens: prompt,
                completion_tokens: 1,
                reasoning_tokens: 0,
                cached_tokens: Some(cached),
                ttft_ms: None,
                generation_ms: None,
                created_at: at,
                outcome: None,
                purpose: purpose.map(str::to_string),
            }
        };
    // Rows 40 days back: outside the 30-day measurement window, and their
    // only decisive gaps would be the 700 s ones if that window were dropped.
    let old_base = now_secs - 40 * 86_400;
    let rows = [
        // Two requests 100 s apart: a gap under every probe.
        request("s1", "m", base, 30_000, 27_000, None),
        request("s1", "m", base + 100, 30_000, 27_000, None),
        // Three gaps of 700 s, each a 10% cache hit — probe 600's window.
        request("s1", "m", base + 800, 30_000, 3_000, None),
        // Compaction's own call, between two "m" requests: filtered out.
        request("s1", "m", base + 850, 30_000, 0, Some("summarize")),
        // Under the 20,000-token minimum: filtered out.
        request("s1", "m", base + 900, 5_000, 0, None),
        request("s1", "m", base + 1_500, 30_000, 3_000, None),
        request("s1", "m", base + 2_200, 30_000, 3_000, None),
        // Four "other" rows 700 s apart, each 97% cached. Mixed into "m",
        // their three warm gaps would lift probe 600's mean to 0.53 and flip
        // the answer to None — so "m" proves the model filter.
        request("s2", "other", base, 30_000, 29_000, None),
        request("s2", "other", base + 700, 30_000, 29_000, None),
        request("s2", "other", base + 1_400, 30_000, 29_000, None),
        request("s2", "other", base + 2_100, 30_000, 29_000, None),
        // One model per filter, each with FOUR cold rows 700 s apart and no
        // other request: dropped, each would decide Some(600) on its own, so
        // the None below proves that filter.
        request("s3", "sum", base, 30_000, 0, Some("summarize")),
        request("s3", "sum", base + 700, 30_000, 0, Some("summarize")),
        request("s3", "sum", base + 1_400, 30_000, 0, Some("summarize")),
        request("s3", "sum", base + 2_100, 30_000, 0, Some("summarize")),
        request("s4", "small", base, 5_000, 0, None),
        request("s4", "small", base + 700, 5_000, 0, None),
        request("s4", "small", base + 1_400, 5_000, 0, None),
        request("s4", "small", base + 2_100, 5_000, 0, None),
        request("s5", "old", old_base, 30_000, 0, None),
        request("s5", "old", old_base + 700, 30_000, 0, None),
        request("s5", "old", old_base + 1_400, 30_000, 0, None),
        request("s5", "old", old_base + 2_100, 30_000, 0, None),
    ];
    for row in rows {
        store.record_request_stats(&row).await.unwrap();
    }

    assert_eq!(store.cache_lifetime_secs("m").await.unwrap(), Some(600));
    assert_eq!(store.cache_lifetime_secs("unknown").await.unwrap(), None);
    assert_eq!(store.cache_lifetime_secs("sum").await.unwrap(), None);
    assert_eq!(store.cache_lifetime_secs("small").await.unwrap(), None);
    assert_eq!(store.cache_lifetime_secs("old").await.unwrap(), None);
}

#[tokio::test]
async fn savings_valuation_lists_session_model_usage() {
    let store = MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap();
    let request = |model: &str, prompt: u32, cached: Option<u32>| RequestStats {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: Some("s1".into()),
        model: model.into(),
        endpoint: None,
        prompt_tokens: prompt,
        completion_tokens: 10,
        reasoning_tokens: 0,
        cached_tokens: cached,
        ttft_ms: None,
        generation_ms: None,
        created_at: 1_700_000_000,
        outcome: None,
        purpose: None,
    };
    store
        .record_request_stats(&request("alpha", 1_000, Some(400)))
        .await
        .unwrap();
    store
        .record_request_stats(&request("alpha", 1_000, Some(100)))
        .await
        .unwrap();
    // A provider that never reports caching: NULL counts as zero.
    store
        .record_request_stats(&request("beta", 500, None))
        .await
        .unwrap();

    let valuation = store.savings_valuation().await.unwrap();

    assert_eq!(valuation.sessions.len(), 2, "{:?}", valuation.sessions);
    let alpha = valuation
        .sessions
        .iter()
        .find(|row| row.model == "alpha")
        .expect("alpha is listed");
    assert_eq!(alpha.session_id.as_deref(), Some("s1"));
    assert_eq!(alpha.prompt_tokens, 2_000);
    assert_eq!(alpha.cached_tokens, 500);
    assert_eq!(alpha.requests, 2);
    let beta = valuation
        .sessions
        .iter()
        .find(|row| row.model == "beta")
        .expect("beta is listed");
    assert_eq!(beta.prompt_tokens, 500);
    assert_eq!(beta.cached_tokens, 0);
    assert_eq!(beta.requests, 1);
}

#[tokio::test]
async fn prune_tool_result_archive_removes_old_rows_and_their_search_entries() {
    let store = MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap();
    let old = store
        .archive_tool_result(Some("s1"), "shell", None, "OLDROW distinctive marker")
        .await
        .unwrap();
    let fresh = store
        .archive_tool_result(Some("s1"), "shell", None, "NEWROW distinctive marker")
        .await
        .unwrap();
    // Archive rows are stamped "now" on write, so the test back-dates the old
    // one directly to put it past the retention window.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs() as i64;
    let cutoff = now - maintenance::ARCHIVE_RETENTION_SECS;
    {
        let conn = store.conn.lock().expect("conn lock poisoned");
        conn.execute(
            "UPDATE tool_result_archive SET created_at = ?1 WHERE id = ?2",
            rusqlite::params![cutoff - 1, old],
        )
        .unwrap();
    }

    assert_eq!(
        store.prune_tool_result_archive(cutoff, 100).await.unwrap(),
        1
    );
    assert!(store.expand_tool_result(&old).await.unwrap().is_none());
    let hits = store.search_archive("distinctive", 10).await.unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].id, fresh);
    // Nothing left to prune.
    assert_eq!(
        store.prune_tool_result_archive(cutoff, 100).await.unwrap(),
        0
    );

    // A batch of one deletes one row per call, oldest first.
    let mut old_ids = Vec::new();
    for index in 0..3 {
        let id = store
            .archive_tool_result(Some("s1"), "shell", None, &format!("OLD {index}"))
            .await
            .unwrap();
        old_ids.push(id);
    }
    {
        let conn = store.conn.lock().expect("conn lock poisoned");
        for (index, id) in old_ids.iter().enumerate() {
            conn.execute(
                "UPDATE tool_result_archive SET created_at = ?1 WHERE id = ?2",
                rusqlite::params![cutoff - 100 + index as i64, id],
            )
            .unwrap();
        }
    }
    for id in &old_ids {
        assert_eq!(store.prune_tool_result_archive(cutoff, 1).await.unwrap(), 1);
        assert!(store.expand_tool_result(id).await.unwrap().is_none());
    }
    assert_eq!(
        store.prune_tool_result_archive(cutoff, 1).await.unwrap(),
        0
    );
}

#[tokio::test]
async fn prune_old_archive_rows_clears_every_old_row() {
    let store = Arc::new(MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap());
    // More than one prune batch, so the loop has to go round twice.
    for index in 0..150 {
        store
            .archive_tool_result(Some("s1"), "shell", None, &format!("OLD {index}"))
            .await
            .unwrap();
    }
    let fresh = store
        .archive_tool_result(Some("s1"), "shell", None, "FRESH")
        .await
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs() as i64;
    {
        let conn = store.conn.lock().expect("conn lock poisoned");
        conn.execute(
            "UPDATE tool_result_archive SET created_at = ?1 WHERE id <> ?2",
            rusqlite::params![now - maintenance::ARCHIVE_RETENTION_SECS - 1, fresh],
        )
        .unwrap();
    }

    maintenance::prune_old_archive_rows(store.clone()).await;

    let conn = store.conn.lock().expect("conn lock poisoned");
    let remaining: i64 = conn
        .query_row("SELECT COUNT(*) FROM tool_result_archive", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(remaining, 1, "only the row inside the window survives");
}
