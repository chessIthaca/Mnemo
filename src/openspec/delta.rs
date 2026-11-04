// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! OpenSpec requirement deltas and archive-order application.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{Requirement, Spec};

/// A set of OpenSpec changes for one capability.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    /// Capability folder and specification identity.
    pub capability: String,
    /// New requirements.
    #[serde(default)]
    pub added: Vec<Requirement>,
    /// Complete replacements for existing requirements.
    #[serde(default)]
    pub modified: Vec<Requirement>,
    /// Requirements to remove, each with an archive rationale.
    #[serde(default)]
    pub removed: Vec<RemovedRequirement>,
    /// Requirement identity changes, applied before every other operation.
    #[serde(default)]
    pub renamed: Vec<RenamedRequirement>,
}

/// A REMOVED Requirements entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovedRequirement {
    /// Existing requirement name.
    #[serde(rename = "requirement", alias = "name")]
    pub requirement: String,
    /// Why the requirement is no longer part of the current state.
    pub reason: String,
}

/// A RENAMED Requirements entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenamedRequirement {
    /// Existing requirement name.
    pub from: String,
    /// New, unused requirement name.
    pub to: String,
}

/// Delta operation attached to a conflict, suitable for user-facing errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaOperation {
    Added,
    Modified,
    Removed,
    Renamed,
}

impl std::fmt::Display for DeltaOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Added => "ADDED",
            Self::Modified => "MODIFIED",
            Self::Removed => "REMOVED",
            Self::Renamed => "RENAMED",
        };
        f.write_str(name)
    }
}

/// A validation or merge conflict. Requirement conflicts always include the
/// affected requirement name so plan creation and plan close can act on it.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DeltaConflict {
    /// The delta was presented for a different specification.
    #[error("delta capability '{delta}' does not match spec capability '{spec}'")]
    CapabilityMismatch { delta: String, spec: String },
    /// MODIFIED, REMOVED, or RENAMED refers to an absent requirement.
    #[error("{operation} requirement '{requirement}' does not exist in capability '{capability}'")]
    MissingRequirement {
        operation: DeltaOperation,
        capability: String,
        requirement: String,
    },
    /// ADDED or a rename target would make requirement names non-unique.
    #[error("{operation} requirement '{requirement}' already exists in capability '{capability}'")]
    ExistingRequirement {
        operation: DeltaOperation,
        capability: String,
        requirement: String,
    },
    /// Normative prose must use OpenSpec's SHALL or MUST vocabulary.
    #[error("{operation} requirement '{requirement}' must contain SHALL or MUST text")]
    MissingNormativeText {
        operation: DeltaOperation,
        requirement: String,
    },
    /// Every requirement needs at least one acceptance scenario.
    #[error("{operation} requirement '{requirement}' must have at least one scenario")]
    MissingScenario {
        operation: DeltaOperation,
        requirement: String,
    },
    /// A scenario must have a non-empty WHEN bullet.
    #[error("{operation} requirement '{requirement}' scenario '{scenario}' is missing WHEN")]
    MissingWhen {
        operation: DeltaOperation,
        requirement: String,
        scenario: String,
    },
    /// A scenario must have a non-empty THEN bullet.
    #[error("{operation} requirement '{requirement}' scenario '{scenario}' is missing THEN")]
    MissingThen {
        operation: DeltaOperation,
        requirement: String,
        scenario: String,
    },
    /// REMOVED entries carry a reason in the archive.
    #[error("REMOVED requirement '{requirement}' must include a reason")]
    MissingRemovalReason { requirement: String },
}

impl Delta {
    /// Validate this delta against a specification without modifying it.
    ///
    /// Validation follows archive order, so later operations see earlier
    /// renames and removals. Pass `None` for a capability that has no spec yet.
    pub fn validate(&self, spec: Option<&Spec>) -> Result<(), DeltaConflict> {
        apply(spec, self).map(|_| ())
    }
}

/// Apply a delta in OpenSpec archive order: RENAMED, REMOVED, MODIFIED, ADDED.
///
/// The input is cloned and never mutated. A missing specification (`None`)
/// creates a new capability; any non-ADDED entry then fails as a missing
/// requirement conflict.
pub fn apply(spec: Option<&Spec>, delta: &Delta) -> Result<Spec, DeltaConflict> {
    let mut result = match spec {
        Some(spec) => {
            if capability_key(spec.capability()) != capability_key(&delta.capability) {
                return Err(DeltaConflict::CapabilityMismatch {
                    delta: delta.capability.clone(),
                    spec: spec.capability().to_string(),
                });
            }
            spec.clone()
        }
        None => Spec::new(delta.capability.clone()),
    };

    for rename in &delta.renamed {
        require_exists(&result, DeltaOperation::Renamed, &rename.from)?;
        if result.requirement(&rename.to).is_some() {
            return Err(DeltaConflict::ExistingRequirement {
                operation: DeltaOperation::Renamed,
                capability: delta.capability.clone(),
                requirement: rename.to.clone(),
            });
        }
        result.rename_requirement(&rename.from, &rename.to);
    }
    for removed in &delta.removed {
        require_exists(&result, DeltaOperation::Removed, &removed.requirement)?;
        if removed.reason.trim().is_empty() {
            return Err(DeltaConflict::MissingRemovalReason {
                requirement: removed.requirement.clone(),
            });
        }
        result.remove_requirement(&removed.requirement);
    }
    for requirement in &delta.modified {
        require_exists(&result, DeltaOperation::Modified, &requirement.name)?;
        validate_requirement(DeltaOperation::Modified, requirement)?;
        result.replace_requirement(&requirement.name, requirement.clone());
    }
    for requirement in &delta.added {
        if result.requirement(&requirement.name).is_some() {
            return Err(DeltaConflict::ExistingRequirement {
                operation: DeltaOperation::Added,
                capability: delta.capability.clone(),
                requirement: requirement.name.clone(),
            });
        }
        validate_requirement(DeltaOperation::Added, requirement)?;
        result.add_requirement(requirement.clone());
    }
    Ok(result)
}

fn capability_key(value: &str) -> String {
    value
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join("-")
}

fn require_exists(
    spec: &Spec,
    operation: DeltaOperation,
    requirement: &str,
) -> Result<(), DeltaConflict> {
    if spec.requirement(requirement).is_some() {
        Ok(())
    } else {
        Err(DeltaConflict::MissingRequirement {
            operation,
            capability: spec.capability().to_string(),
            requirement: requirement.to_string(),
        })
    }
}

fn validate_requirement(
    operation: DeltaOperation,
    requirement: &Requirement,
) -> Result<(), DeltaConflict> {
    let normative = requirement
        .text
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| matches!(word, "SHALL" | "MUST"));
    if !normative {
        return Err(DeltaConflict::MissingNormativeText {
            operation,
            requirement: requirement.name.clone(),
        });
    }
    if requirement.scenarios.is_empty() {
        return Err(DeltaConflict::MissingScenario {
            operation,
            requirement: requirement.name.clone(),
        });
    }
    for scenario in &requirement.scenarios {
        if !scenario.when.iter().any(|value| !value.trim().is_empty()) {
            return Err(DeltaConflict::MissingWhen {
                operation,
                requirement: requirement.name.clone(),
                scenario: scenario.name.clone(),
            });
        }
        if !scenario.then.iter().any(|value| !value.trim().is_empty()) {
            return Err(DeltaConflict::MissingThen {
                operation,
                requirement: requirement.name.clone(),
                scenario: scenario.name.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openspec::Scenario;

    fn requirement(name: &str) -> Requirement {
        Requirement::new(
            name,
            "The system SHALL preserve the contract.",
            vec![Scenario::new(
                "Contract exercised",
                vec!["the contract is exercised".to_string()],
                vec!["the behavior is observable".to_string()],
                vec![],
            )],
        )
    }

    fn base() -> Spec {
        let mut spec = Spec::new("dispatch-routing");
        for name in ["Old name", "Remove me", "Modify me", "Stay"] {
            spec.add_requirement(requirement(name));
        }
        spec
    }

    fn delta() -> Delta {
        Delta {
            capability: "dispatch-routing".to_string(),
            ..Delta::default()
        }
    }

    #[test]
    fn every_delta_kind_applies_in_archive_order() {
        let original = base();
        let mut change = delta();
        change.renamed.push(RenamedRequirement {
            from: "Old name".to_string(),
            to: "New name".to_string(),
        });
        change.removed.push(RemovedRequirement {
            requirement: "Remove me".to_string(),
            reason: "The behavior was retired.".to_string(),
        });
        change.modified.push(Requirement::new(
            "Modify me",
            "The system MUST use the new behavior.",
            requirement("ignored").scenarios,
        ));
        change.added.push(requirement("Added requirement"));
        let result = apply(Some(&original), &change).unwrap();
        let names: Vec<_> = result
            .requirements()
            .iter()
            .map(|req| req.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["New name", "Modify me", "Stay", "Added requirement"]
        );
        assert!(result
            .requirement("Modify me")
            .unwrap()
            .text
            .contains("MUST"));
        assert!(original.requirement("Old name").is_some());
        assert!(original.requirement("Remove me").is_some());
        assert!(result.render().contains("### Requirement: New name"));
    }

    #[test]
    fn later_operations_see_renames_and_removals() {
        let original = base();
        let mut change = delta();
        change.renamed.push(RenamedRequirement {
            from: "Old name".to_string(),
            to: "Modify me next".to_string(),
        });
        change.modified.push(Requirement::new(
            "Modify me next",
            "The system MUST use the renamed identity.",
            requirement("ignored").scenarios,
        ));
        change.removed.push(RemovedRequirement {
            requirement: "Remove me".to_string(),
            reason: "Replaced by an updated requirement.".to_string(),
        });
        change.added.push(requirement("Remove me"));
        let result = apply(Some(&original), &change).unwrap();
        assert!(result.requirement("Modify me next").is_some());
        assert!(result.requirement("Old name").is_none());
        assert!(result.requirement("Remove me").is_some());
    }

    #[test]
    fn missing_capability_is_created_from_added_only() {
        let mut change = delta();
        change.added.push(requirement("First requirement"));
        let result = apply(None, &change).unwrap();
        assert_eq!(result.capability(), "dispatch-routing");
        assert_eq!(result.requirements().len(), 1);
        assert!(result.render().contains("# dispatch-routing"));
    }

    #[test]
    fn human_heading_matches_folder_identity_and_unknown_sections_survive() {
        let original = Spec::parse(
            "# Dispatch routing\n\n## Purpose\nRouting behavior.\n\n## Requirements\n\n### Requirement: Stay\nThe system SHALL stay.\n\n#### Scenario: Used\n- **WHEN** it is used\n- **THEN** it stays\n\n## Notes\nRetained *verbatim*.\n",
        )
        .unwrap();
        let mut change = delta();
        change.modified.push(Requirement::new(
            "Stay",
            "The system MUST stay.",
            requirement("ignored").scenarios,
        ));
        let rendered = apply(Some(&original), &change).unwrap().render();
        assert!(rendered.contains("The system MUST stay."));
        assert!(rendered.contains("## Notes\nRetained *verbatim*.\n"));
    }

    #[test]
    fn absent_names_are_conflicts_for_each_existing_requirement_operation() {
        for (operation, change) in [
            (DeltaOperation::Modified, {
                let mut d = delta();
                d.modified.push(requirement("Absent"));
                d
            }),
            (DeltaOperation::Removed, {
                let mut d = delta();
                d.removed.push(RemovedRequirement {
                    requirement: "Absent".to_string(),
                    reason: "Gone.".to_string(),
                });
                d
            }),
            (DeltaOperation::Renamed, {
                let mut d = delta();
                d.renamed.push(RenamedRequirement {
                    from: "Absent".to_string(),
                    to: "New".to_string(),
                });
                d
            }),
        ] {
            let error = apply(Some(&base()), &change).unwrap_err();
            assert_eq!(
                error,
                DeltaConflict::MissingRequirement {
                    operation,
                    capability: "dispatch-routing".to_string(),
                    requirement: "Absent".to_string(),
                }
            );
            assert!(error.to_string().contains("Absent"));
        }
    }

    #[test]
    fn added_and_rename_collisions_name_the_requirement() {
        let mut added = delta();
        added.added.push(requirement("Stay"));
        assert_eq!(
            apply(Some(&base()), &added),
            Err(DeltaConflict::ExistingRequirement {
                operation: DeltaOperation::Added,
                capability: "dispatch-routing".to_string(),
                requirement: "Stay".to_string(),
            })
        );
        let mut renamed = delta();
        renamed.renamed.push(RenamedRequirement {
            from: "Old name".to_string(),
            to: "Stay".to_string(),
        });
        assert_eq!(
            apply(Some(&base()), &renamed),
            Err(DeltaConflict::ExistingRequirement {
                operation: DeltaOperation::Renamed,
                capability: "dispatch-routing".to_string(),
                requirement: "Stay".to_string(),
            })
        );
    }

    #[test]
    fn requirement_validation_covers_normative_text_and_scenarios() {
        let cases = [
            (
                Requirement::new("Bad text", "The system records events.", vec![]),
                DeltaConflict::MissingNormativeText {
                    operation: DeltaOperation::Added,
                    requirement: "Bad text".to_string(),
                },
            ),
            (
                Requirement::new("No scenarios", "The system SHALL work.", vec![]),
                DeltaConflict::MissingScenario {
                    operation: DeltaOperation::Added,
                    requirement: "No scenarios".to_string(),
                },
            ),
            (
                Requirement::new(
                    "No when",
                    "The system SHALL work.",
                    vec![Scenario::new(
                        "Incomplete",
                        vec![],
                        vec!["it works".to_string()],
                        vec![],
                    )],
                ),
                DeltaConflict::MissingWhen {
                    operation: DeltaOperation::Added,
                    requirement: "No when".to_string(),
                    scenario: "Incomplete".to_string(),
                },
            ),
            (
                Requirement::new(
                    "No then",
                    "The system SHALL work.",
                    vec![Scenario::new(
                        "Incomplete",
                        vec!["it is used".to_string()],
                        vec!["  ".to_string()],
                        vec![],
                    )],
                ),
                DeltaConflict::MissingThen {
                    operation: DeltaOperation::Added,
                    requirement: "No then".to_string(),
                    scenario: "Incomplete".to_string(),
                },
            ),
        ];
        for (invalid, expected) in cases {
            let mut change = delta();
            change.added.push(invalid);
            assert_eq!(change.validate(Some(&base())), Err(expected));
        }
    }

    #[test]
    fn removal_reason_and_capability_are_validated() {
        let mut removed = delta();
        removed.removed.push(RemovedRequirement {
            requirement: "Stay".to_string(),
            reason: "  ".to_string(),
        });
        assert_eq!(
            removed.validate(Some(&base())),
            Err(DeltaConflict::MissingRemovalReason {
                requirement: "Stay".to_string()
            })
        );
        let wrong = Delta {
            capability: "other-capability".to_string(),
            ..Delta::default()
        };
        assert_eq!(
            wrong.validate(Some(&base())),
            Err(DeltaConflict::CapabilityMismatch {
                delta: "other-capability".to_string(),
                spec: "dispatch-routing".to_string(),
            })
        );
    }
}
