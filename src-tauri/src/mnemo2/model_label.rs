// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Compact model labels shared by Mnemo2's external surfaces.

/// Return the short, conversational name for a model specification.
///
/// Provider prefixes are discarded, the model name ends at its first dash,
/// and flash models carry an `-f` suffix.
pub fn short_model_name(model: &str) -> String {
    let name = model.rsplit('/').next().unwrap_or_default();
    let short = name.split('-').next().unwrap_or_default().to_lowercase();
    if name.to_lowercase().contains("flash") {
        format!("{short}-f")
    } else {
        short
    }
}

#[cfg(test)]
mod tests {
    use super::short_model_name;

    #[test]
    fn short_names_follow_the_external_surface_rule() {
        for (model, expected) in [
            ("Kimi/kimi-k3.1-pro", "kimi"),
            ("Ollama Cloud/glm-5.3", "glm"),
            ("Ollama Cloud/deepseek-v4.1-flash:1017", "deepseek-f"),
            ("Ollama Cloud/DeepSeek-V4.1-FLASH:1017", "deepseek-f"),
            ("Ollama Cloud/GLM", "glm"),
            ("", ""),
        ] {
            assert_eq!(short_model_name(model), expected, "model: {model}");
        }
    }
}
