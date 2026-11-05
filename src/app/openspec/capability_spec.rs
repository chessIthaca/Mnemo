// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Read the current OpenSpec specification for one capability.

use std::io::ErrorKind;
use std::path::Path;

/// Read `<openspec_dir>/specs/<capability>/spec.md` without following a linked
/// capability folder or spec file. A missing folder or file is not an error.
pub fn read_capability_spec(
    openspec_dir: &Path,
    capability: &str,
) -> Result<Option<String>, String> {
    if !valid_capability_name(capability) {
        return Err(format!(
            "invalid capability name '{capability}': expected kebab-case ASCII letters, digits, and hyphens"
        ));
    }

    let folder = openspec_dir.join("specs").join(capability);
    match std::fs::symlink_metadata(&folder) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(format!(
                "refusing to read linked capability folder '{}'",
                folder.display()
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "could not inspect capability folder '{}': {error}",
                folder.display()
            ));
        }
    }

    let spec_path = folder.join("spec.md");
    match std::fs::symlink_metadata(&spec_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(format!(
                "refusing to read linked capability spec '{}'",
                spec_path.display()
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "could not inspect capability spec '{}': {error}",
                spec_path.display()
            ));
        }
    }

    match std::fs::read_to_string(&spec_path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "could not read capability spec '{}': {error}",
            spec_path.display()
        )),
    }
}

fn valid_capability_name(capability: &str) -> bool {
    !capability.is_empty()
        && capability.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_current_spec_and_returns_none_when_it_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("specs/tier-ladder");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("spec.md"), "# Tier ladder\n\nCurrent text.\n").unwrap();

        let text = read_capability_spec(temp.path(), "tier-ladder")
            .unwrap()
            .unwrap();
        assert_eq!(text, "# Tier ladder\n\nCurrent text.\n");
        assert_eq!(
            read_capability_spec(temp.path(), "unknown-capability").unwrap(),
            None
        );
        std::fs::remove_file(folder.join("spec.md")).unwrap();
        assert_eq!(
            read_capability_spec(temp.path(), "tier-ladder").unwrap(),
            None
        );
    }

    #[test]
    fn refuses_names_that_are_not_kebab_case() {
        let temp = tempfile::tempdir().unwrap();
        for capability in [
            "",
            "../x",
            "/absolute",
            "Tier-ladder",
            "tier_ladder",
            "tier--ladder",
            "-tier",
            "tier-",
            "tier/ladder",
            "tier\\ladder",
        ] {
            let error = read_capability_spec(temp.path(), capability).unwrap_err();
            assert!(
                error.contains("invalid capability name"),
                "{capability:?}: {error}"
            );
        }
        assert!(read_capability_spec(temp.path(), "tier-2")
            .unwrap()
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_linked_capability_folders_and_spec_files() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let specs = temp.path().join("specs");
        std::fs::create_dir_all(&specs).unwrap();
        let target = temp.path().join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("spec.md"), "# Target\n").unwrap();
        symlink(&target, specs.join("linked-folder")).unwrap();
        let error = read_capability_spec(temp.path(), "linked-folder").unwrap_err();
        assert!(error.contains("linked capability folder"), "{error}");

        let folder = specs.join("linked-spec");
        std::fs::create_dir_all(&folder).unwrap();
        symlink(target.join("spec.md"), folder.join("spec.md")).unwrap();
        let error = read_capability_spec(temp.path(), "linked-spec").unwrap_err();
        assert!(error.contains("linked capability spec"), "{error}");
    }
}
