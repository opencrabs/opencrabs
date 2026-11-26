//! Dynamic tool types and executor.
//!
//! `DynamicToolDef` is the TOML-serializable definition.
//! `DynamicTool` wraps a definition and implements the `Tool` trait.

use crate::brain::tools::error::Result;
use crate::brain::tools::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Executor type for a dynamic tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorType {
    Http,
    Shell,
}

/// What to do with a parameter value when it lands in one of the
/// edge-case shapes (`null`, empty array, empty string). Configured
/// per-param in `tools.toml` so the same call can hand off cleanly to
/// servers that disagree on what "absent" means (issue #95).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CoerceAction {
    /// Pass the value through as-is. Default; preserves the
    /// pre-coercion behaviour for every existing tools.toml entry.
    #[default]
    Keep,
    /// Drop the key entirely. For HTTP this means it does not appear
    /// in the JSON body. For shell, the `{{#name}}…{{/name}}` block
    /// that wraps the parameter is collapsed away.
    Omit,
    /// Replace the value with an explicit JSON `null`. Useful when the
    /// downstream server expects `null` rather than an empty array.
    Null,
    /// Reject the call before it leaves the tool. Returns an error to
    /// the agent so it can adjust.
    Error,
}

/// Parameter definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamDef {
    pub name: String,
    #[serde(rename = "type", default = "default_string_type")]
    pub param_type: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default)]
    pub default: Option<Value>,
    /// What to do when the resolved value is an empty container
    /// (empty array, empty object, empty string). Default `Keep`.
    #[serde(default)]
    pub coerce_empty_to: CoerceAction,
    /// What to do when the resolved value is JSON `null`. Default
    /// `Keep`.
    #[serde(default)]
    pub coerce_null_to: CoerceAction,
}

/// A single dynamic tool definition as parsed from tools.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicToolDef {
    pub name: String,
    pub description: String,
    pub executor: ExecutorType,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub requires_approval: bool,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub params: Vec<ParamDef>,
}

fn default_true() -> bool {
    true
}
fn default_timeout() -> u64 {
    30
}
fn default_string_type() -> String {
    "string".to_string()
}

/// Shell quoting context a `{{param}}` placeholder lands in, tracked while
/// scanning a shell command template so each value is escaped correctly (#523).
#[derive(Clone, Copy, PartialEq)]
enum ShellQuoteCtx {
    None,
    Single,
    Double,
}

/// Escape a string value for the shell quoting context it is substituted into
/// (#523). Single-quoted and unquoted spans use the exact previous single-quote
/// escaping (`'` → `'\''`) so working templates are byte-for-byte unchanged; a
/// double-quoted span backslash-escapes the characters the shell still expands
/// there (`\`, `` ` ``, `$`, `"`). `!` is intentionally not escaped: history
/// expansion is off in the non-interactive `sh -c` these commands run under.
fn escape_for_shell_ctx(s: &str, ctx: ShellQuoteCtx) -> String {
    match ctx {
        ShellQuoteCtx::Single | ShellQuoteCtx::None => s.replace('\'', "'\\''"),
        ShellQuoteCtx::Double => s
            .replace('\\', "\\\\")
            .replace('`', "\\`")
            .replace('$', "\\$")
            .replace('"', "\\\""),
    }
}

impl DynamicToolDef {
    pub fn input_schema(&self) -> Value {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();
        for param in &self.params {
            let mut prop = serde_json::Map::new();
            prop.insert("type".into(), Value::String(param.param_type.clone()));
            if !param.description.is_empty() {
                prop.insert(
                    "description".into(),
                    Value::String(param.description.clone()),
                );
            }
            if let Some(ref default) = param.default {
                prop.insert("default".into(), default.clone());
            }
            properties.insert(param.name.clone(), Value::Object(prop));
            if param.required {
                required.push(Value::String(param.name.clone()));
            }
        }
        serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required
        })
    }

    /// Render `template` by substituting `{{name}}` placeholders with
    /// the matching value from `params`. Also expands mustache-style
    /// conditional sections `{{#name}}…{{/name}}`: when `name` is
    /// present in `params` the section's body is rendered (with its
    /// inner `{{name}}` substituted); when it's absent the entire
    /// section, including its enclosing tags, is dropped. This lets
    /// `coerce_empty_to = "omit"` cleanly remove a CLI flag like
    /// `{{#ids}}--ids {{ids}}{{/ids}}` instead of leaving a dangling
    /// `--ids ` with no value.
    pub fn render_template(template: &str, params: &Value) -> String {
        let obj = params.as_object();

        // Section pass first, then the standard `{{name}}` substitution.
        let mut result = Self::resolve_sections(template, obj);
        if let Some(obj) = obj {
            for (key, value) in obj {
                let placeholder = format!("{{{{{}}}}}", key);
                let replacement = match value {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                result = result.replace(&placeholder, &replacement);
            }
        }
        result
    }

    /// Resolve `{{#name}}...{{/name}}` conditional sections against the params
    /// (body kept when `name` is present), leaving `{{name}}` placeholders for
    /// a later substitution pass. Shared by [`render_template`] and
    /// [`render_shell_command`]. Single-pass left-to-right so nested tags
    /// collapse predictably; malformed tags pass through untouched so the
    /// error is at least visible.
    fn resolve_sections(template: &str, obj: Option<&serde_json::Map<String, Value>>) -> String {
        let mut after_sections = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(open_at) = rest.find("{{#") {
            after_sections.push_str(&rest[..open_at]);
            let after_open = &rest[open_at + 3..];
            let Some(name_end) = after_open.find("}}") else {
                after_sections.push_str(&rest[open_at..]);
                rest = "";
                break;
            };
            let name = &after_open[..name_end];
            let body_start = name_end + 2;
            let close_tag = format!("{{{{/{}}}}}", name);
            let after_name = &after_open[body_start..];
            let Some(close_at) = after_name.find(&close_tag) else {
                after_sections.push_str(&rest[open_at..]);
                rest = "";
                break;
            };
            let body = &after_name[..close_at];
            let present = obj.is_some_and(|o| o.contains_key(name));
            if present {
                after_sections.push_str(body);
            }
            rest = &after_name[close_at + close_tag.len()..];
        }
        after_sections.push_str(rest);
        after_sections
    }

    /// Render a SHELL command template, escaping each string parameter for the
    /// quoting context it lands in (#523). The generic single-quote escaper
    /// (`shell_escape_params`) corrupted values placed in a DOUBLE-quoted
    /// argument like `-p "{{prompt}}"`: `$`, backtick and `\` stayed
    /// shell-interpreted (the `command_code` 44% failure). Here we scan the
    /// template tracking whether each `{{name}}` sits in a single-quoted,
    /// double-quoted, or unquoted span and escape accordingly: single-quoted
    /// and unquoted spans use `'` → `'\''` (the exact previous behavior, so
    /// every working template is byte-for-byte unchanged), while a
    /// double-quoted span backslash-escapes `\`, backtick, `$`, and `"`.
    ///
    /// Non-string values pass through via `to_string()` (numbers/bools are
    /// shell-safe). Unknown placeholders are emitted verbatim, as before.
    pub fn render_shell_command(template: &str, params: &Value) -> String {
        let resolved = Self::resolve_sections(template, params.as_object());
        let obj = params.as_object();
        let mut out = String::with_capacity(resolved.len() + 16);
        let mut quote = ShellQuoteCtx::None;
        let mut idx = 0usize;
        while idx < resolved.len() {
            let tail = &resolved[idx..];
            // Placeholder?
            if let Some(rest) = tail.strip_prefix("{{")
                && !rest.starts_with(['#', '/'])
                && let Some(end) = rest.find("}}")
            {
                let name = &rest[..end];
                let advance = 2 + end + 2;
                if let Some(value) = obj.and_then(|o| o.get(name)) {
                    match value {
                        Value::String(s) => out.push_str(&escape_for_shell_ctx(s, quote)),
                        other => out.push_str(&other.to_string()),
                    }
                } else {
                    // Unknown placeholder: emit verbatim (render_template does
                    // the same by leaving unmatched keys in place).
                    out.push_str(&resolved[idx..idx + advance]);
                }
                idx += advance;
                continue;
            }
            // Literal char: track quote state, honoring backslash escapes in a
            // double-quoted span so an author's `\"` does not close the quote.
            let c = tail.chars().next().expect("non-empty tail");
            let clen = c.len_utf8();
            match quote {
                ShellQuoteCtx::None => match c {
                    '\'' => quote = ShellQuoteCtx::Single,
                    '"' => quote = ShellQuoteCtx::Double,
                    _ => {}
                },
                ShellQuoteCtx::Single => {
                    if c == '\'' {
                        quote = ShellQuoteCtx::None;
                    }
                }
                ShellQuoteCtx::Double => {
                    if c == '\\' {
                        out.push(c);
                        idx += clen;
                        if let Some(n) = resolved[idx..].chars().next() {
                            out.push(n);
                            idx += n.len_utf8();
                        }
                        continue;
                    } else if c == '"' {
                        quote = ShellQuoteCtx::None;
                    }
                }
            }
            out.push(c);
            idx += clen;
        }
        out
    }

    /// Escape string values in params for use in single-quoted shell
    /// arguments. Replaces each `'` with `'\''` (end single-quote,
    /// escaped single-quote, resume single-quote) — the standard POSIX
    /// shell idiom for embedding a single quote inside a single-quoted
    /// string.
    ///
    /// Non-string values (numbers, booleans, arrays, objects) pass
    /// through unchanged — they are already safe for shell usage since
    /// `render_template` converts them with `to_string()`.
    pub fn shell_escape_params(params: &Value) -> Value {
        match params {
            Value::Object(map) => {
                let mut out = serde_json::Map::new();
                for (k, v) in map {
                    out.insert(k.clone(), Self::shell_escape_params(v));
                }
                Value::Object(out)
            }
            Value::String(s) => Value::String(s.replace('\'', "'\\''")),
            other => other.clone(),
        }
    }
}

/// Top-level tools.toml structure.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DynamicToolsConfig {
    #[serde(default)]
    pub tools: Vec<DynamicToolDef>,
}

/// Runtime tool wrapping a TOML definition.
pub struct DynamicTool {
    def: DynamicToolDef,
}

impl DynamicTool {
    pub fn new(def: DynamicToolDef) -> Self {
        Self { def }
    }

    pub(crate) fn extract_params(&self, input: &Value) -> Value {
        let mut out = serde_json::Map::new();
        let obj = input.as_object();
        for p in &self.def.params {
            let val = obj
                .and_then(|o| o.get(&p.name))
                .cloned()
                .or_else(|| p.default.clone());
            if let Some(v) = val {
                out.insert(p.name.clone(), v);
            }
        }
        Value::Object(out)
    }

    /// Apply per-parameter `coerce_empty_to` / `coerce_null_to` rules
    /// (issue #95) to the extracted params. Returns `Ok(params)` with
    /// the coerced map, or `Err(message)` when any param had its
    /// `Error` rule triggered. `Omit` removes the key; `Null` replaces
    /// the value with `Value::Null`; `Keep` is the no-op default.
    fn coerce_params(&self, params: Value) -> std::result::Result<Value, String> {
        let mut map = match params {
            Value::Object(m) => m,
            other => return Ok(other),
        };

        // Drop the action enum into a small Vec of decisions so we
        // mutate the map after walking the param defs.
        for p in &self.def.params {
            let Some(v) = map.get(&p.name) else { continue };

            let is_null = matches!(v, Value::Null);
            let is_empty = match v {
                Value::String(s) => s.is_empty(),
                Value::Array(a) => a.is_empty(),
                Value::Object(o) => o.is_empty(),
                _ => false,
            };

            let action = if is_null {
                p.coerce_null_to
            } else if is_empty {
                p.coerce_empty_to
            } else {
                CoerceAction::Keep
            };

            match action {
                CoerceAction::Keep => {}
                CoerceAction::Omit => {
                    map.remove(&p.name);
                }
                CoerceAction::Null => {
                    map.insert(p.name.clone(), Value::Null);
                }
                CoerceAction::Error => {
                    let shape = if is_null { "null" } else { "empty" };
                    return Err(format!(
                        "Parameter '{}' is {} and the tool config rejects this shape \
                         (coerce_{}_to = \"error\"). Adjust the call or change the rule.",
                        p.name,
                        shape,
                        if is_null { "null" } else { "empty" }
                    ));
                }
            }
        }

        Ok(Value::Object(map))
    }

    async fn execute_http(&self, params: &Value) -> Result<ToolResult> {
        let url = match &self.def.url {
            Some(u) => DynamicToolDef::render_template(u, params),
            None => return Ok(ToolResult::error("HTTP tool missing 'url' field".into())),
        };
        let method = self.def.method.as_deref().unwrap_or("GET").to_uppercase();
        let client = crate::utils::http::shared();
        let mut req = match method.as_str() {
            "POST" => client.post(&url),
            "PUT" => client.put(&url),
            "PATCH" => client.patch(&url),
            "DELETE" => client.delete(&url),
            _ => client.get(&url),
        };
        for (k, v) in &self.def.headers {
            let rendered = DynamicToolDef::render_template(v, params);
            req = req.header(k.as_str(), rendered);
        }
        let timeout = std::time::Duration::from_secs(self.def.timeout_secs);
        match req.timeout(timeout).send().await {
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                if status.is_success() {
                    Ok(ToolResult::success(body))
                } else {
                    Ok(ToolResult::error(format!(
                        "HTTP {} {}: {}",
                        status.as_u16(),
                        status.canonical_reason().unwrap_or(""),
                        body
                    )))
                }
            }
            Err(e) => Ok(ToolResult::error(format!("HTTP request failed: {e}"))),
        }
    }

    async fn execute_shell(
        &self,
        params: &Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult> {
        // Write all parameters to a temporary JSON file and expose the
        // path via OPENCRABS_PARAMS environment variable. This lets tool
        // commands read structured data (JSON arrays, multiline strings,
        // nested objects) without shell quoting or heredoc fragility.
        //
        // Backward compatibility: if the command template contains
        // {{param}} placeholders, we still expand them (with shell
        // escaping) so existing tools.toml entries continue to work.
        let params_file = match tempfile::NamedTempFile::new() {
            Ok(f) => f,
            Err(e) => {
                return Ok(ToolResult::error(format!(
                    "Failed to create params temp file: {e}"
                )));
            }
        };
        let params_path = params_file.path().to_string_lossy().to_string();
        if let Err(e) = std::fs::write(
            &params_path,
            serde_json::to_string(params).unwrap_or_default(),
        ) {
            return Ok(ToolResult::error(format!(
                "Failed to write params JSON: {e}"
            )));
        }

        // Escape string parameters for the quoting context each `{{param}}`
        // lands in (#523): single-quoted / unquoted keep the previous
        // `'message={{message}}'` behavior, double-quoted args like
        // `-p "{{prompt}}"` now escape `$`, backtick and `\` too.
        let cmd = match &self.def.command {
            Some(c) => DynamicToolDef::render_shell_command(c, params),
            None => {
                return Ok(ToolResult::error(
                    "Shell tool missing 'command' field".into(),
                ));
            }
        };
        // The bash hard blocklist applies to dynamic shell tools too (OC-06):
        // a `sh -c` executor ran with no floor, so a dynamic tool (or a
        // `{{param}}` expanded into one) could run what bash refuses. Checked
        // on the rendered command, after parameter substitution.
        if let Some(reason) = super::super::bash::assert_command_allowed(&cmd) {
            return Ok(ToolResult::error(format!(
                "dynamic shell tool refused: {reason}. This is on the command blocklist (OC-06)."
            )));
        }
        // Detach stdin from the parent TTY so mouse-capture bytes don't
        // leak into captured stdout (same TUI-bleed issue as bash.rs).
        // Platform shell pair (`cmd /C` on Windows) — a hardcoded `sh -c`
        // here made every dynamic shell tool fail to spawn on Windows.
        use crate::utils::shell::PushShellCommand;
        let (shell, shell_arg) = crate::utils::shell::shell_pair();
        let output = tokio::process::Command::new(shell)
            .kill_on_drop(true)
            .push_shell_command(shell_arg, &cmd)
            .env("OPENCRABS_PARAMS", &params_path)
            .current_dir(context.working_dir())
            .stdin(std::process::Stdio::null())
            .output()
            .await;

        // Clean up the params file after execution (NamedTempFile drops
        // automatically but we can be explicit).
        drop(params_file);

        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                if out.status.success() {
                    let mut result = stdout;
                    if !stderr.is_empty() {
                        result.push_str("\n[stderr] ");
                        result.push_str(&stderr);
                    }
                    Ok(ToolResult::success(result))
                } else {
                    Ok(ToolResult::error(format!(
                        "Exit code {}: {}{}",
                        out.status.code().unwrap_or(-1),
                        stdout,
                        if stderr.is_empty() {
                            String::new()
                        } else {
                            format!("\n[stderr] {stderr}")
                        }
                    )))
                }
            }
            Err(e) => Ok(ToolResult::error(format!("Failed to spawn shell: {e}"))),
        }
    }
}

#[async_trait]
impl Tool for DynamicTool {
    fn name(&self) -> &str {
        &self.def.name
    }
    fn description(&self) -> &str {
        &self.def.description
    }
    fn input_schema(&self) -> Value {
        self.def.input_schema()
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        match self.def.executor {
            ExecutorType::Http => vec![ToolCapability::Network],
            ExecutorType::Shell => vec![ToolCapability::ExecuteShell],
        }
    }
    fn requires_approval(&self) -> bool {
        self.def.requires_approval
    }
    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let raw_params = self.extract_params(&input);
        let params = match self.coerce_params(raw_params) {
            Ok(p) => p,
            Err(msg) => return Ok(ToolResult::error(msg)),
        };
        tracing::info!(
            "Executing dynamic tool '{}' ({:?})",
            self.def.name,
            self.def.executor
        );
        match self.def.executor {
            ExecutorType::Http => self.execute_http(&params).await,
            ExecutorType::Shell => self.execute_shell(&params, context).await,
        }
    }
}
