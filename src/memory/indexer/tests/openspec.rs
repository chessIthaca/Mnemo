// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

use super::*;

#[tokio::test]
async fn memory_search_finds_a_requirement_by_its_scenario_text() {
    let dir = tempdir().unwrap();
    let spec_dir = dir.path().join(".coding/openspec/specs/dispatch-routing");
    std::fs::create_dir_all(&spec_dir).unwrap();
    std::fs::write(
        spec_dir.join("spec.md"),
        "# Dispatch routing\n\n## Purpose\nHow dispatch chooses a worker.\n\n## Requirements\n\n### Requirement: Model changes are training labels\nThe system SHALL append a reroute row for every model change.\n\n#### Scenario: Routing off\n- **WHEN** the quixotic routing switch is off\n- **THEN** one reroute row is appended\n",
    )
    .unwrap();
    // Metadata is Mnemo-only and must not become a separate memory row.
    std::fs::write(
        spec_dir.join(".mnemo.yaml"),
        "paths: ['src/runtime/**/*.rs']\n",
    )
    .unwrap();
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

    let id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        b"openspec:dispatch-routing/Model changes are training labels",
    )
    .to_string();
    let memory = store.get_memory(&id).await.unwrap().expect("spec indexed");
    assert_eq!(memory.record_class, MemoryClass::Derived);
    assert_eq!(memory.record_type, MemoryRecordType::Spec);
    assert_eq!(
        memory.title,
        "SPEC: dispatch-routing/Model changes are training labels"
    );
    assert!(memory.content.contains("Scenario: Routing off"));
    assert!(memory
        .content
        .contains("path .coding/openspec/specs/dispatch-routing/spec.md"));

    let hits = store
        .recall("quixotic", &MemoryFilter::new())
        .await
        .unwrap();
    assert!(
        hits.iter().any(|hit| hit.memory.id == id),
        "scenario-only vocabulary must retrieve the requirement: {hits:?}"
    );
}
