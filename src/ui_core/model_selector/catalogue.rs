/// Load default models from embedded config.toml.example for a provider.
pub fn load_default_models(provider_id: &str) -> Vec<String> {
    let config_content = include_str!("../../../config.toml.example");
    let mut models = Vec::new();

    if let Ok(config) = config_content.parse::<toml::Value>()
        && let Some(providers) = config.get("providers")
    {
        // Map provider id to config.toml.example section key
        // (config uses underscore: "claude_cli", but provider id uses hyphen: "claude-cli")
        let section_key = match provider_id {
            "claude-cli" => "claude_cli",
            "opencode-cli" => "opencode_cli",
            "codex-cli" => "codex_cli",
            "command-code-cli" => "command_code_cli",
            "codex" => "codex", // Codex OAuth
            "" => "custom",     // empty id = custom providers
            other => other,
        };

        if section_key == "custom" {
            // Custom providers: merge models from all custom sections
            if let Some(custom) = providers.get("custom")
                && let Some(custom_table) = custom.as_table()
            {
                for (_name, entry) in custom_table {
                    if let Some(models_arr) = entry.get("models").and_then(|m| m.as_array()) {
                        for model in models_arr {
                            if let Some(model_str) = model.as_str()
                                && !models.contains(&model_str.to_string())
                            {
                                models.push(model_str.to_string());
                            }
                        }
                    }
                }
            }
        } else if let Some(section) = providers.get(section_key)
            && let Some(models_arr) = section.get("models").and_then(|m| m.as_array())
        {
            for model in models_arr {
                if let Some(model_str) = model.as_str() {
                    models.push(model_str.to_string());
                }
            }
        }
    }

    tracing::debug!(
        "Loaded {} default models from config.toml.example for provider '{}'",
        models.len(),
        provider_id
    );
    models
}
