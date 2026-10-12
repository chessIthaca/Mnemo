// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The shipped-skill catalog + project seeding — kernel-owned (job 2b
//! decision 5): the project bootstrap (a kernel concern) seeds
//! `.coding/skills/*.toml` without importing the app's skill engine, and the
//! app's skill module reads the catalog back through this module (app→kernel,
//! the legal direction). The catalog is data, not engine logic: the skill
//! library load path and the skill tools stay app-side.

use std::path::Path;

/// The skills shipped with the app, compiled into the binary as
/// `(name, toml_text)` pairs via `include_str!`. [`crate::project::Project::init`]
/// seeds each into a project's `.coding/skills/` directory (write-if-missing),
/// so every new project — and an existing one whose skill files were deleted —
/// starts with the same baseline skill set as the main installation, without
/// depending on the app's source tree being present at runtime.
///
/// The baseline is `merge_to_main` (landing a working branch) plus
/// `create_skill` (plan-free skill authoring — its prompt is written to read
/// correctly OUTSIDE the app repo too, so a seeded copy is never misleading).
/// The skills that deliberately do NOT ship are declared in the app's
/// `REPO_LOCAL_SKILLS` list, and the guard test
/// `every_repo_skill_is_shipped_or_declared_repo_local` makes every future
/// repo skill pick a side instead of silently never shipping.
///
/// Adding a shipped skill takes TWO edits: drop the TOML into the repo's own
/// `.coding/skills/<name>.toml` AND add one entry here (the file is embedded
/// at compile time; nothing is read from disk at runtime).
pub const SHIPPED_SKILLS: &[(&str, &str)] = &[
    (
        "merge_to_main",
        include_str!("../../.coding/skills/merge_to_main.toml"),
    ),
    (
        "create_skill",
        include_str!("../../.coding/skills/create_skill.toml"),
    ),
];

/// Seed the [`SHIPPED_SKILLS`] into `skills_dir` (a project's
/// `.coding/skills/`): the directory is created when missing and each shipped
/// skill is written as `<name>.toml` — but ONLY when that file does not
/// already exist, so a user-modified or project-specific skill is never
/// overwritten. Re-seeding a populated directory is a no-op; deleting a skill
/// file (or the whole directory) makes the next project init self-heal it.
///
/// Returns the underlying I/O error when the directory cannot be created or a
/// first write fails.
pub fn seed_skills(skills_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(skills_dir)?;
    for (name, text) in SHIPPED_SKILLS {
        let path = skills_dir.join(format!("{name}.toml"));
        if path.exists() {
            continue;
        }
        std::fs::write(&path, text)?;
    }
    Ok(())
}