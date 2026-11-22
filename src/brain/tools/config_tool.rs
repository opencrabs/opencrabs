//! Config Management Tool
//!
//! Lets the agent read/write config.toml and commands.toml at runtime.

use super::error::Result;
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde_json::Value;

pub struct ConfigTool;

#[async_trait]
impl Tool for ConfigTool {
    fn name(&self) -> &str {
        "config_manager"
    }

    fn description(&self) -> &str {
        "Read or write OpenCrabs configuration (config.toml) and user commands (commands.toml). \
         Use this to change settings like approval policy, view current config, \
         add/remove user slash commands, change working directory, or trigger a config reload. \
         write_config is validated against the compiled config schema: a section that exists in \
         config.toml (like [memory]) is accepted, while a path the schema would silently drop \
         on load is hard-denied."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": [
                        "read_config",
                        "write_config",
                        "read_commands",
                        "add_command",
                        "remove_command",
                        "reload",
                        "set_working_directory"
                    ],
                    "description": "The operation to perform"
                },
                "section": {
                    "type": "string",
                    "description": "Config section for read_config/write_config (e.g. 'agent', 'voice', 'logging'). Omit to read the full config. write_config accepts any section the compiled Config struct knows — e.g. 'memory', even though read_config only renders the first level; unknown paths are hard-denied."
                },
                "key": {
                    "type": "string",
                    "description": "Config key within the section for write_config (e.g. 'approval_policy')"
                },
                "value": {
                    "type": "string",
                    "description": "Value to write for write_config (e.g. 'auto-always')"
                },
                "command_name": {
                    "type": "string",
                    "description": "Slash command name for add_command/remove_command (e.g. '/deploy')"
                },
                "command_description": {
                    "type": "string",
                    "description": "Description for add_command"
                },
                "command_prompt": {
                    "type": "string",
                    "description": "Prompt text for add_command"
                },
                "command_action": {
                    "type": "string",
                    "description": "Action type for add_command: 'prompt' or 'system' (default: 'prompt')"
                },
                "path": {
                    "type": "string",
                    "description": "Absolute directory path for set_working_directory (e.g. '/home/user/project')"
                }
            },
            "required": ["operation"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WriteFiles]
    }

    fn requires_approval(&self) -> bool {
        // Writes need approval, reads are safe — but we mark true since
        // the tool *can* write. The agent description guides appropriate use.
        true
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let operation = input
            .get("operation")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match operation {
            "read_config" => self.read_config(&input),
            "write_config" => self.write_config(&input),
            "read_commands" => self.read_commands(),
            "add_command" => self.add_command(&input),
            "remove_command" => self.remove_command(&input),
            "reload" => self.reload_config(),
            "set_working_directory" => self.set_working_directory(&input, context).await,
            _ => Ok(ToolResult::error(format!(
                "Unknown operation: '{}'. Valid: read_config, write_config, \
                 read_commands, add_command, remove_command, reload, set_working_directory",
                operation
            ))),
        }
    }
}

/// Top-level sections `read_config` can render.
pub(crate) use crate::config::sections::{known_sections, resolve_section};

impl ConfigTool {
    fn read_config(&self, input: &Value) -> Result<ToolResult> {
        let config = match crate::config::Config::load() {
            Ok(c) => c,
            Err(e) => return Ok(ToolResult::error(format!("Failed to load config: {}", e))),
        };

        // Resolve `providers.stt` / `telegram` to the section that owns them
        // before dispatch (#889); `None` keeps the original string so the error
        // below names what was actually asked for.
        let requested = input.get("section").and_then(|v| v.as_str());
        let resolved = requested.and_then(resolve_section);
        // Unresolved requests keep the original string so the error below
        // names what was actually asked for — and legacy section names that
        // predate the struct registry (e.g. `voice`, migrated into
        // `providers`) still render their dedicated read views.
        let section = resolved.or_else(|| requested.map(str::to_string));

        let output = match section.as_deref() {
            Some("agent") => format_toml(&config.agent),
            Some("voice") => {
                let vc = config.voice_config();
                format!(
                    "stt_enabled = {}\nstt_mode = {:?}\ntts_enabled = {}\ntts_mode = {:?}\ntts_voice = {:?}\nlocal_tts_voice = {:?}",
                    vc.stt_enabled,
                    vc.stt_mode,
                    vc.tts_enabled,
                    vc.tts_mode,
                    vc.tts_voice,
                    vc.local_tts_voice
                )
            }
            Some("logging") => format_toml(&config.logging),
            Some("debug") => format_toml(&config.debug),
            Some("channels") => format_toml(&config.channels),
            Some("provider_registry") => format_toml(&config.provider_registry),
            Some("database") => format_toml(&config.database),
            Some("providers") => format_toml(&config.providers),
            // Remaining struct sections (#86): the resolver resolves them fine,
            // but the render match only carried the first eight arms, so every
            // other known section fell into the catch-all error arm — the
            // error named the section unknown while listing it as valid.
            Some("a2a") => format_toml(&config.a2a),
            Some("brain") => format_toml(&config.brain),
            Some("browser") => format_toml(&config.browser),
            Some("cron") => format_toml(&config.cron),
            Some("daemon") => format_toml(&config.daemon),
            Some("doctor") => format_toml(&config.doctor),
            Some("image") => format_toml(&config.image),
            Some("memory") => format_toml(&config.memory),
            Some("tui") => format_toml(&config.tui),
            Some(other) => {
                return Ok(ToolResult::error(format!(
                    "Unknown config section: '{}'. Valid: {}. Nested paths and child names \
                     resolve to their parent (e.g. 'providers.stt' or 'stt' -> providers, \
                     'telegram' -> channels).",
                    other,
                    known_sections().join(", ")
                )));
            }
            None => {
                // Full config — skip api_keys for safety
                format_toml(&config)
            }
        };

        Ok(ToolResult::success(output))
    }

    fn write_config(&self, input: &Value) -> Result<ToolResult> {
        let section = match input.get("section").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => {
                return Ok(ToolResult::error(
                    "'section' is required for write_config".into(),
                ));
            }
        };
        let key = match input.get("key").and_then(|v| v.as_str()) {
            Some(k) => k,
            None => {
                return Ok(ToolResult::error(
                    "'key' is required for write_config".into(),
                ));
            }
        };
        let value = match input.get("value").and_then(|v| v.as_str()) {
            Some(v) => v,
            None => {
                return Ok(ToolResult::error(
                    "'value' is required for write_config".into(),
                ));
            }
        };

        match crate::config::Config::write_key(section, key, value) {
            Ok(written) => Ok(ToolResult::success(format!(
                "Set [{section}].{key} = {written} in config.toml"
            ))),
            Err(e) => Ok(ToolResult::error(format!("Failed to write config: {}", e))),
        }
    }

    fn read_commands(&self) -> Result<ToolResult> {
        let brain_path = crate::brain::BrainLoader::resolve_path();
        let loader = crate::brain::CommandLoader::from_brain_path(&brain_path);
        let commands = loader.load();

        if commands.is_empty() {
            return Ok(ToolResult::success(
                "No user-defined commands. Use add_command to create one.".into(),
            ));
        }

        let mut output = format!("{} user command(s):\n\n", commands.len());
        for cmd in &commands {
            output.push_str(&format!(
                "  {} — {} (action: {})\n    prompt: {}\n\n",
                cmd.name, cmd.description, cmd.action, cmd.prompt
            ));
        }
        Ok(ToolResult::success(output))
    }

    fn add_command(&self, input: &Value) -> Result<ToolResult> {
        let name = match input.get("command_name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => {
                return Ok(ToolResult::error(
                    "'command_name' is required for add_command".into(),
                ));
            }
        };
        let description = input
            .get("command_description")
            .and_then(|v| v.as_str())
            .unwrap_or("User command");
        let prompt = match input.get("command_prompt").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => {
                return Ok(ToolResult::error(
                    "'command_prompt' is required for add_command".into(),
                ));
            }
        };
        let action = input
            .get("command_action")
            .and_then(|v| v.as_str())
            .unwrap_or("prompt");

        // Ensure name starts with /
        let name = if name.starts_with('/') {
            name.to_string()
        } else {
            format!("/{}", name)
        };

        let brain_path = crate::brain::BrainLoader::resolve_path();
        let loader = crate::brain::CommandLoader::from_brain_path(&brain_path);

        let cmd = crate::brain::commands::UserCommand {
            name: name.clone(),
            description: description.to_string(),
            action: action.to_string(),
            prompt: prompt.to_string(),
        };

        match loader.add_command(cmd) {
            Ok(()) => Ok(ToolResult::success(format!(
                "Added command {name} to commands.toml"
            ))),
            Err(e) => Ok(ToolResult::error(format!("Failed to add command: {}", e))),
        }
    }

    fn remove_command(&self, input: &Value) -> Result<ToolResult> {
        let name = match input.get("command_name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => {
                return Ok(ToolResult::error(
                    "'command_name' is required for remove_command".into(),
                ));
            }
        };

        let brain_path = crate::brain::BrainLoader::resolve_path();
        let loader = crate::brain::CommandLoader::from_brain_path(&brain_path);

        match loader.remove_command(name) {
            Ok(true) => Ok(ToolResult::success(format!(
                "Removed command {name} from commands.toml"
            ))),
            Ok(false) => Ok(ToolResult::success(format!(
                "Command {name} not found in commands.toml"
            ))),
            Err(e) => Ok(ToolResult::error(format!(
                "Failed to remove command: {}",
                e
            ))),
        }
    }

    fn reload_config(&self) -> Result<ToolResult> {
        match crate::config::Config::reload() {
            Ok(_) => Ok(ToolResult::success(
                "Configuration reloaded from disk.".into(),
            )),
            Err(e) => Ok(ToolResult::error(format!("Failed to reload config: {}", e))),
        }
    }

    /// Async because it writes the session row: the old shape reached for
    /// `block_in_place`, which panics on the current-thread runtime the
    /// window's turns run on (#2975).
    async fn set_working_directory(
        &self,
        input: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        let path_str = match input.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => {
                return Ok(ToolResult::error(
                    "'path' is required for set_working_directory".into(),
                ));
            }
        };

        let path = std::path::PathBuf::from(path_str);

        // Validate the path exists and is a directory
        if !path.exists() {
            return Ok(ToolResult::error(format!(
                "Path does not exist: {}",
                path_str
            )));
        }
        if !path.is_dir() {
            return Ok(ToolResult::error(format!(
                "Path is not a directory: {}",
                path_str
            )));
        }

        // Canonicalize to resolve symlinks and relative components
        let canonical = match path.canonicalize() {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(format!("Failed to resolve path: {}", e))),
        };

        // Update runtime working directory via shared handle
        if let Some(ref shared_wd) = context.shared_working_directory {
            *shared_wd.write().expect("working_directory lock poisoned") = canonical.clone();
        }

        // Persist to session DB — that's the source of truth for per-session WD.
        if let Some(ref svc_ctx) = context.service_context {
            let session_svc = crate::services::SessionService::new(svc_ctx.clone());
            let sid = context.session_id;
            let dir_str = canonical.to_string_lossy().to_string();
            let _ = session_svc
                .update_session_working_directory(sid, Some(dir_str))
                .await;
        }

        Ok(ToolResult::success(format!(
            "Working directory changed to: {}",
            canonical.display()
        )))
    }
}

/// Serialise any serde type to pretty TOML, falling back to Debug.
fn format_toml<T: serde::Serialize>(value: &T) -> String {
    toml::to_string_pretty(value).unwrap_or_else(|_| format!("{:?}", "serialization error"))
}
