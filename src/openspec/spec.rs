// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Parser and renderer for OpenSpec `spec.md` files.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A parsed OpenSpec current-state specification.
///
/// The original document is retained so parsing and rendering a well-formed
/// file is byte-stable, including unknown sections and line endings. Applying
/// a delta switches to the canonical renderer; unknown `##` sections are still
/// copied verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    capability: String,
    purpose: String,
    requirements: Vec<Requirement>,
    extra_sections: Vec<String>,
    original: Option<String>,
}

/// One OpenSpec requirement and its acceptance scenarios.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    /// Requirement title (the JSON delta field is named `requirement`).
    #[serde(rename = "requirement", alias = "name")]
    pub name: String,
    /// Normative prose, which must contain SHALL or MUST when used in a delta.
    #[serde(default)]
    pub text: String,
    /// Acceptance scenarios belonging to the requirement.
    #[serde(default)]
    pub scenarios: Vec<Scenario>,
    /// Original Markdown block, retained for lossless rendering.
    #[serde(skip)]
    pub(crate) original: Option<String>,
}

/// One OpenSpec scenario. `AND` bullets are retained in their own list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scenario {
    /// Scenario title.
    pub name: String,
    /// Values of the scenario's WHEN bullets.
    #[serde(default)]
    pub when: Vec<String>,
    /// Values of the scenario's THEN bullets.
    #[serde(default)]
    pub then: Vec<String>,
    /// Values of additional AND bullets.
    #[serde(default)]
    pub and: Vec<String>,
}

/// Structural errors in an OpenSpec `spec.md` document.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SpecError {
    /// The level-one capability heading is absent or empty.
    #[error("spec is missing a non-empty '# <Capability>' heading")]
    MissingCapability,
    /// The required Purpose section is absent.
    #[error("spec '{capability}' is missing its '## Purpose' section")]
    MissingPurpose { capability: String },
    /// The required Requirements section is absent.
    #[error("spec '{capability}' is missing its '## Requirements' section")]
    MissingRequirements { capability: String },
    /// A required level-two section occurs more than once.
    #[error("spec '{capability}' has duplicate '## {section}' sections")]
    DuplicateSection { capability: String, section: String },
    /// A requirement heading has no name.
    #[error("spec '{capability}' has an empty requirement name")]
    EmptyRequirementName { capability: String },
    /// Requirement names are the identity used by deltas and must be unique.
    #[error("spec '{capability}' has duplicate requirement '{requirement}'")]
    DuplicateRequirement {
        capability: String,
        requirement: String,
    },
}

impl Spec {
    /// Parse an OpenSpec `spec.md` document.
    pub fn parse(text: &str) -> Result<Self, SpecError> {
        let lines = markdown_lines(text);
        let title = lines
            .iter()
            .find(|line| line.text.starts_with("# ") && !line.text.starts_with("## "))
            .ok_or(SpecError::MissingCapability)?;
        let capability = title.text[2..].trim().to_string();
        if capability.is_empty() {
            return Err(SpecError::MissingCapability);
        }

        let purpose = unique_section(&lines, &capability, "Purpose")?;
        let requirements = unique_section(&lines, &capability, "Requirements")?;
        let purpose = purpose.ok_or_else(|| SpecError::MissingPurpose {
            capability: capability.clone(),
        })?;
        let requirements = requirements.ok_or_else(|| SpecError::MissingRequirements {
            capability: capability.clone(),
        })?;
        if purpose.start > requirements.start {
            return Err(SpecError::MissingRequirements { capability });
        }

        let purpose_end = next_level_two(&lines, purpose.line_index + 1)
            .map(|line| line.start)
            .unwrap_or(text.len());
        let purpose_text = text[purpose.body_start..purpose_end].trim().to_string();

        let requirements_end = next_level_two(&lines, requirements.line_index + 1)
            .map(|line| line.start)
            .unwrap_or(text.len());
        let mut parsed_requirements = Vec::new();
        let mut names = HashSet::new();
        for (index, line) in lines.iter().enumerate() {
            if line.start < requirements.body_start || line.start >= requirements_end {
                continue;
            }
            let Some(name) = line.text.strip_prefix("### Requirement: ") else {
                continue;
            };
            let name = name.trim();
            if name.is_empty() {
                return Err(SpecError::EmptyRequirementName { capability });
            }
            if !names.insert(name.to_string()) {
                return Err(SpecError::DuplicateRequirement {
                    capability,
                    requirement: name.to_string(),
                });
            }
            let end = lines[index + 1..]
                .iter()
                .find(|next| {
                    next.start < requirements_end
                        && (next.text.starts_with("### ") || next.text.starts_with("## "))
                })
                .map(|next| next.start)
                .unwrap_or(requirements_end);
            let original = text[line.start..end].to_string();
            parsed_requirements.push(Requirement::parse_block(name, &original));
        }

        let mut extra_sections = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if !line.text.starts_with("## ") || line.text.starts_with("### ") {
                continue;
            }
            if line.start == purpose.start || line.start == requirements.start {
                continue;
            }
            let end = lines[index + 1..]
                .iter()
                .find(|next| next.text.starts_with("## ") && !next.text.starts_with("### "))
                .map(|next| next.start)
                .unwrap_or(text.len());
            extra_sections.push(text[line.start..end].to_string());
        }

        Ok(Self {
            capability,
            purpose: purpose_text,
            requirements: parsed_requirements,
            extra_sections,
            original: Some(text.to_string()),
        })
    }

    /// Create an empty specification for a previously unknown capability.
    pub fn new(capability: impl Into<String>) -> Self {
        Self {
            capability: capability.into(),
            purpose: String::new(),
            requirements: Vec::new(),
            extra_sections: Vec::new(),
            original: None,
        }
    }

    /// Capability name from the level-one heading.
    pub fn capability(&self) -> &str {
        &self.capability
    }

    /// Purpose prose without the `## Purpose` heading.
    pub fn purpose(&self) -> &str {
        &self.purpose
    }

    /// Requirements in document order.
    pub fn requirements(&self) -> &[Requirement] {
        &self.requirements
    }

    /// Find a requirement by its exact, case-sensitive name.
    pub fn requirement(&self, name: &str) -> Option<&Requirement> {
        self.requirements.iter().find(|req| req.name == name)
    }

    /// Render the specification as OpenSpec Markdown.
    ///
    /// A specification that was not changed by a delta renders byte-identical
    /// to its input. Reassembly from parts normalizes the spacing around each
    /// block instead: a requirement's retained Markdown carries the blank line
    /// that separated it from the next heading, so the trailing whitespace is
    /// trimmed and exactly one newline re-added — otherwise every merge would
    /// add another blank line before each preserved requirement.
    pub fn render(&self) -> String {
        if let Some(original) = &self.original {
            return original.clone();
        }
        let mut out = format!("# {}\n\n## Purpose\n", self.capability);
        if !self.purpose.is_empty() {
            out.push_str(self.purpose.trim());
            out.push('\n');
        }
        out.push_str("\n## Requirements\n");
        for requirement in &self.requirements {
            out.push('\n');
            out.push_str(requirement.render().trim_end());
            out.push('\n');
        }
        for section in &self.extra_sections {
            if !out.ends_with("\n\n") {
                out.push('\n');
            }
            out.push_str(section.trim_end());
            out.push('\n');
        }
        out
    }

    pub(crate) fn rename_requirement(&mut self, from: &str, to: &str) {
        if let Some(requirement) = self.requirements.iter_mut().find(|req| req.name == from) {
            requirement.rename(to);
        }
        self.original = None;
    }

    pub(crate) fn remove_requirement(&mut self, name: &str) {
        self.requirements.retain(|req| req.name != name);
        self.original = None;
    }

    pub(crate) fn replace_requirement(&mut self, name: &str, replacement: Requirement) {
        if let Some(requirement) = self.requirements.iter_mut().find(|req| req.name == name) {
            *requirement = replacement;
        }
        self.original = None;
    }

    pub(crate) fn add_requirement(&mut self, requirement: Requirement) {
        self.requirements.push(requirement);
        self.original = None;
    }
}

impl Requirement {
    /// Construct a requirement for an ADDED or MODIFIED delta.
    pub fn new(name: impl Into<String>, text: impl Into<String>, scenarios: Vec<Scenario>) -> Self {
        Self {
            name: name.into(),
            text: text.into(),
            scenarios,
            original: None,
        }
    }

    fn parse_block(name: &str, original: &str) -> Self {
        let lines = markdown_lines(original);
        let first_scenario = lines
            .iter()
            .position(|line| line.text.starts_with("#### Scenario: "));
        let text_end = first_scenario
            .map(|index| lines[index].start)
            .unwrap_or(original.len());
        let body_start = lines.first().map(|line| line.end).unwrap_or(0);
        let text = original[body_start..text_end].trim().to_string();
        let mut scenarios = Vec::new();
        if let Some(first) = first_scenario {
            for (relative, line) in lines[first..].iter().enumerate() {
                let Some(scenario_name) = line.text.strip_prefix("#### Scenario: ") else {
                    continue;
                };
                let index = first + relative;
                let end = lines[index + 1..]
                    .iter()
                    .find(|next| next.text.starts_with("#### "))
                    .map(|next| next.start)
                    .unwrap_or(original.len());
                scenarios.push(Scenario::parse_block(
                    scenario_name.trim(),
                    &original[line.end..end],
                ));
            }
        }
        Self {
            name: name.to_string(),
            text,
            scenarios,
            original: Some(original.to_string()),
        }
    }

    fn rename(&mut self, to: &str) {
        self.name = to.to_string();
        if let Some(original) = &self.original {
            let first_end = original.find('\n').map(|i| i + 1).unwrap_or(original.len());
            let (heading, body) = original.split_at(first_end);
            let ending = if heading.ends_with("\r\n") {
                "\r\n"
            } else if heading.ends_with('\n') {
                "\n"
            } else {
                ""
            };
            self.original = Some(format!("### Requirement: {to}{ending}{body}"));
        }
    }

    /// Render one requirement block in canonical OpenSpec form.
    pub fn render(&self) -> String {
        if let Some(original) = &self.original {
            return original.clone();
        }
        let mut out = format!("### Requirement: {}\n", self.name);
        if !self.text.trim().is_empty() {
            out.push_str(self.text.trim());
            out.push('\n');
        }
        for scenario in &self.scenarios {
            out.push('\n');
            out.push_str(&scenario.render());
        }
        out
    }
}

impl Scenario {
    /// Construct a scenario with WHEN, THEN, and optional AND bullets.
    pub fn new(
        name: impl Into<String>,
        when: Vec<String>,
        then: Vec<String>,
        and: Vec<String>,
    ) -> Self {
        Self {
            name: name.into(),
            when,
            then,
            and,
        }
    }

    fn parse_block(name: &str, body: &str) -> Self {
        let mut scenario = Self {
            name: name.to_string(),
            ..Self::default()
        };
        // 0 = WHEN, 1 = THEN, 2 = AND. Keeping only the kind and index
        // avoids holding a mutable borrow of one list while parsing the next.
        let mut current: Option<(u8, usize)> = None;
        for line in body.lines() {
            let trimmed = line.trim();
            let parsed = trimmed
                .strip_prefix("- **WHEN**")
                .map(|value| (0, value))
                .or_else(|| trimmed.strip_prefix("- **THEN**").map(|value| (1, value)))
                .or_else(|| trimmed.strip_prefix("- **AND**").map(|value| (2, value)));
            if let Some((kind, value)) = parsed {
                let target = match kind {
                    0 => &mut scenario.when,
                    1 => &mut scenario.then,
                    _ => &mut scenario.and,
                };
                target.push(value.trim().to_string());
                current = Some((kind, target.len() - 1));
            } else if let Some((kind, index)) = current {
                if !trimmed.is_empty() {
                    let target = match kind {
                        0 => &mut scenario.when,
                        1 => &mut scenario.then,
                        _ => &mut scenario.and,
                    };
                    target[index].push('\n');
                    target[index].push_str(trimmed);
                }
            }
        }
        scenario
    }

    /// Render one scenario in canonical OpenSpec form.
    ///
    /// WHEN values are all written as `WHEN`. The first THEN value is written
    /// as `THEN` and every further outcome — as well as each explicit `and`
    /// value — as `AND`, the form OpenSpec uses for extra outcomes. Parsing
    /// such a bullet stores it in `and`, so the round trip is stable.
    pub fn render(&self) -> String {
        let mut out = format!("#### Scenario: {}\n", self.name);
        for value in &self.when {
            out.push_str(&format!("- **WHEN** {}\n", value.trim()));
        }
        for (index, value) in self.then.iter().enumerate() {
            let label = if index == 0 { "THEN" } else { "AND" };
            out.push_str(&format!("- **{label}** {}\n", value.trim()));
        }
        for value in &self.and {
            out.push_str(&format!("- **AND** {}\n", value.trim()));
        }
        out
    }
}

#[derive(Debug)]
struct MarkdownLine<'a> {
    start: usize,
    end: usize,
    text: &'a str,
}

fn markdown_lines(text: &str) -> Vec<MarkdownLine<'_>> {
    let mut start = 0;
    let mut lines = Vec::new();
    for part in text.split_inclusive('\n') {
        let end = start + part.len();
        lines.push(MarkdownLine {
            start,
            end,
            text: part.trim_end_matches(['\r', '\n']),
        });
        start = end;
    }
    if start < text.len() {
        lines.push(MarkdownLine {
            start,
            end: text.len(),
            text: &text[start..],
        });
    }
    lines
}

#[derive(Debug)]
struct Section {
    start: usize,
    body_start: usize,
    line_index: usize,
}

fn unique_section(
    lines: &[MarkdownLine<'_>],
    capability: &str,
    name: &str,
) -> Result<Option<Section>, SpecError> {
    let heading = format!("## {name}");
    let mut found = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.text.trim_end() == heading);
    let first = found.next();
    if found.next().is_some() {
        return Err(SpecError::DuplicateSection {
            capability: capability.to_string(),
            section: name.to_string(),
        });
    }
    Ok(first.map(|(line_index, line)| Section {
        start: line.start,
        body_start: line.end,
        line_index,
    }))
}

fn next_level_two<'a>(lines: &'a [MarkdownLine<'a>], after: usize) -> Option<&'a MarkdownLine<'a>> {
    lines[after..]
        .iter()
        .find(|line| line.text.starts_with("## ") && !line.text.starts_with("### "))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REALISTIC: &str = "# Dispatch routing\n\n## Purpose\nHow Mnemo2 picks a worker's tier at dispatch.\n\n## Requirements\n\n### Requirement: Model changes are training labels\nThe system SHALL append a reroute row for every user model change.\n\n#### Scenario: Routing off\n- **WHEN** routing mode is off and the user switches a worker's model\n- **THEN** one user reroute row is appended\n- **AND** team_status shows the destination tier\n\n### Requirement: Explicit routing\nThe lead MUST be able to select a tier.\n\n#### Scenario: Selected tier\n- **WHEN** the lead selects a tier\n- **THEN** dispatch uses that tier\n\n## Compatibility notes\nThis unknown section is retained *verbatim*.\n";

    #[test]
    fn realistic_spec_round_trips_byte_for_byte() {
        let spec = Spec::parse(REALISTIC).unwrap();
        assert_eq!(spec.capability(), "Dispatch routing");
        assert_eq!(
            spec.purpose(),
            "How Mnemo2 picks a worker's tier at dispatch."
        );
        assert_eq!(spec.requirements().len(), 2);
        assert_eq!(
            spec.requirements()[0].scenarios[0].and,
            ["team_status shows the destination tier"]
        );
        assert_eq!(spec.render(), REALISTIC);
    }

    #[test]
    fn crlf_and_unknown_sections_round_trip() {
        let text = REALISTIC.replace('\n', "\r\n");
        assert_eq!(Spec::parse(&text).unwrap().render(), text);
    }

    #[test]
    fn duplicate_requirement_is_rejected() {
        let text = REALISTIC.replace(
            "### Requirement: Explicit routing",
            "### Requirement: Model changes are training labels",
        );
        assert_eq!(
            Spec::parse(&text),
            Err(SpecError::DuplicateRequirement {
                capability: "Dispatch routing".to_string(),
                requirement: "Model changes are training labels".to_string(),
            })
        );
    }

    #[test]
    fn required_structure_is_checked() {
        assert_eq!(
            Spec::parse("## Purpose\nx\n"),
            Err(SpecError::MissingCapability)
        );
        assert!(matches!(
            Spec::parse("# Cap\n\n## Requirements\n"),
            Err(SpecError::MissingPurpose { .. })
        ));
        assert!(matches!(
            Spec::parse("# Cap\n\n## Purpose\nx\n"),
            Err(SpecError::MissingRequirements { .. })
        ));
        let duplicate = "# Cap\n\n## Purpose\nx\n## Purpose\ny\n## Requirements\n";
        assert!(matches!(
            Spec::parse(duplicate),
            Err(SpecError::DuplicateSection { .. })
        ));
        let empty = "# Cap\n\n## Purpose\nx\n## Requirements\n### Requirement: \n";
        assert_eq!(
            Spec::parse(empty),
            Err(SpecError::EmptyRequirementName {
                capability: "Cap".to_string()
            })
        );
    }

    #[test]
    fn new_spec_and_requirement_render_in_openspec_format() {
        let mut spec = Spec::new("dispatch-routing");
        spec.purpose = "Current routing behavior.".to_string();
        spec.add_requirement(Requirement::new(
            "Labels",
            "The system MUST record labels.",
            vec![Scenario::new(
                "Change",
                vec!["a model changes".to_string()],
                vec!["a label is stored".to_string()],
                vec![],
            )],
        ));
        assert_eq!(
            spec.render(),
            "# dispatch-routing\n\n## Purpose\nCurrent routing behavior.\n\n## Requirements\n\n### Requirement: Labels\nThe system MUST record labels.\n\n#### Scenario: Change\n- **WHEN** a model changes\n- **THEN** a label is stored\n"
        );
        assert_eq!(Spec::parse(&spec.render()).unwrap().render(), spec.render());
    }

    #[test]
    fn scenario_renders_second_outcome_as_and() {
        let scenario = Scenario::new(
            "S",
            vec!["w".to_string()],
            vec!["t1".to_string(), "t2".to_string()],
            vec!["a".to_string()],
        );
        assert_eq!(
            scenario.render(),
            "#### Scenario: S\n- **WHEN** w\n- **THEN** t1\n- **AND** t2\n- **AND** a\n"
        );
    }
}
