//! browser_act — Batched browser actions (multi_act) in ONE call.
//!
//! Accepts an ordered `actions` array (`click` / `fill` / `press` /
//! `select` / `upload` / `wait`) and executes it against a STABLE view: every
//! selector is resolved up front in a single JS pass BEFORE anything
//! executes — a stale reference rejects the whole call instead of
//! half-running a sequence. Execution aborts on first failure and
//! reports the completed prefix. One screenshot at the end (the whole
//! point of batching: fewer round-trips, one page-change capture).

use super::manager::{BrowserManager, split_frame_selector};
use crate::brain::tools::error::Result;
use crate::brain::tools::r#trait::{
    Tool, ToolCapability, ToolExecutionContext, ToolHints, ToolResult,
};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

/// Hard cap on actions per call (#1188: "max-10 cap enforced").
pub(crate) const MAX_ACTIONS: usize = 10;
/// Per-`wait` cap — a single action must not eat the whole budget.
pub(crate) const MAX_WAIT_MS: u64 = 10_000;
/// Overall budget for the whole batch.
pub(crate) const BATCH_TIMEOUT_SECS: u64 = 120;

/// One parsed, validated action.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Act {
    Click {
        selector: String,
    },
    Fill {
        selector: String,
        text: String,
    },
    Press {
        key: String,
    },
    Select {
        selector: String,
        value: String,
    },
    /// Attach local files to a file input (#2112).
    ///
    /// Only the paths cross the tool call; the bytes go straight from disk to
    /// the browser, so a 10 MB video costs one short string instead of a
    /// base64 blob in the context window.
    Upload {
        selector: String,
        files: Vec<String>,
    },
    Wait {
        ms: u64,
    },
}

impl Act {
    /// The selector this action targets, if any (drives pre-flight).
    pub(crate) fn selector(&self) -> Option<&str> {
        match self {
            Act::Click { selector }
            | Act::Fill { selector, .. }
            | Act::Select { selector, .. }
            | Act::Upload { selector, .. } => Some(selector),
            Act::Press { .. } | Act::Wait { .. } => None,
        }
    }

    /// Short human label for reporting ("click #submit", "wait 500ms").
    pub(crate) fn label(&self) -> String {
        match self {
            Act::Click { selector } => format!("click {selector}"),
            Act::Fill { selector, .. } => format!("fill {selector}"),
            Act::Press { key } => format!("press {key}"),
            Act::Select { selector, .. } => format!("select {selector}"),
            Act::Upload { selector, files } => format!("upload {selector} ({} files)", files.len()),
            Act::Wait { ms } => format!("wait {ms}ms"),
        }
    }
}

/// Parse + validate the `actions` array. Pure — every rejection reason
/// is testable without a browser.
pub(crate) fn parse_actions(input: &Value) -> std::result::Result<Vec<Act>, String> {
    let raw = match input["actions"].as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return Err("'actions' must be a non-empty array".into()),
    };
    if raw.len() > MAX_ACTIONS {
        return Err(format!(
            "Too many actions: {} (cap is {MAX_ACTIONS}). Split into multiple browser_act calls.",
            raw.len()
        ));
    }
    let mut out = Vec::with_capacity(raw.len());
    for (i, a) in raw.iter().enumerate() {
        let kind = a["type"].as_str().unwrap_or("");
        let act = match kind {
            "click" => Act::Click {
                selector: req_str(a, "selector", i, "click")?,
            },
            "fill" => Act::Fill {
                selector: req_str(a, "selector", i, "fill")?,
                text: req_str(a, "text", i, "fill")?,
            },
            "press" => Act::Press {
                key: req_str(a, "key", i, "press")?,
            },
            "select" => Act::Select {
                selector: req_str(a, "selector", i, "select")?,
                value: match a["value"].as_str() {
                    Some(v) => v.to_string(),
                    None => String::new(),
                },
            },
            "upload" => Act::Upload {
                selector: req_str(a, "selector", i, "upload")?,
                files: req_upload_paths(a, i)?,
            },
            "wait" => {
                let ms = a["ms"].as_u64().unwrap_or(0);
                if ms == 0 {
                    return Err(format!(
                        "action {}: 'wait' requires a positive \"ms\" (1-{MAX_WAIT_MS})",
                        i + 1
                    ));
                }
                if ms > MAX_WAIT_MS {
                    return Err(format!(
                        "action {}: wait of {ms}ms exceeds the per-action cap of {MAX_WAIT_MS}ms — \
                         split long waits into multiple actions",
                        i + 1
                    ));
                }
                Act::Wait { ms }
            }
            other => {
                return Err(format!(
                    "action {}: unknown type '{other}' (expected click/fill/press/select/wait/upload)",
                    i + 1
                ));
            }
        };
        out.push(act);
    }
    Ok(out)
}

fn req_str(a: &Value, field: &str, i: usize, kind: &str) -> std::result::Result<String, String> {
    match a[field].as_str() {
        Some(s) if !s.is_empty() => Ok(s.to_string()),
        _ => Err(format!(
            "action {}: '{kind}' requires a non-empty \"{field}\"",
            i + 1
        )),
    }
}

/// Validate an `upload` action's `files` array (#2112).
///
/// Every path is checked BEFORE any CDP call, so a bad batch has zero side
/// effects. `DOM.setFileInputFiles` accepts anything the browser process can
/// read, so a prompt-injected page asking for `~/.ssh/id_rsa` must be refused
/// here rather than silently attached: the path must be absolute, exist, and
/// be a regular file.
fn req_upload_paths(a: &Value, i: usize) -> std::result::Result<Vec<String>, String> {
    let raw = match a["files"].as_array() {
        Some(f) if !f.is_empty() => f,
        Some(_) => {
            return Err(format!(
                "action {}: 'upload' requires a non-empty \"files\" array",
                i + 1
            ));
        }
        None => {
            return Err(format!(
                "action {}: 'upload' requires a \"files\" array of absolute paths",
                i + 1
            ));
        }
    };
    let mut paths = Vec::with_capacity(raw.len());
    for entry in raw {
        let Some(raw_path) = entry.as_str().filter(|s| !s.trim().is_empty()) else {
            return Err(format!(
                "action {}: 'upload' \"files\" entries must be non-empty strings",
                i + 1
            ));
        };
        let path = std::path::PathBuf::from(raw_path);
        if !path.is_absolute() {
            return Err(format!(
                "action {}: upload path '{raw_path}' is not absolute: the browser \
                 process cannot resolve a path relative to the agent's working directory",
                i + 1
            ));
        }
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => {
                return Err(format!(
                    "action {}: upload path '{raw_path}' is not a regular file",
                    i + 1
                ));
            }
            Err(e) => {
                return Err(format!(
                    "action {}: upload path '{raw_path}' does not exist ({e})",
                    i + 1
                ));
            }
        }
        paths.push(path.to_string_lossy().into_owned());
    }
    Ok(paths)
}

/// Completed-prefix failure report: "actions 1-2 OK; action 3 failed: …".
/// Pure — pinned by tests.
pub(crate) fn format_prefix_report(done: usize, failed: &Act, reason: &str) -> String {
    let label = failed.label();
    if done == 0 {
        format!("action 1 ({label}) failed: {reason}. No actions executed.")
    } else {
        format!(
            "actions 1-{done} OK; action {} ({label}) failed: {reason}. Later actions were NOT run — \
             the page state reflects the completed prefix only; re-inventory with browser_find \
             before retrying the remainder.",
            done + 1
        )
    }
}

/// Build the one-pass pre-flight JS: resolves EVERY selector (all three
/// shapes: CSS, `text=…`, `xpath=…`) and reports presence + visibility.
/// Injected as a JSON array so quoting is airtight; pure fn — shape is
/// pinned by tests.
pub(crate) fn build_pre_flight_js(selectors: &[String]) -> String {
    let arr = serde_json::to_string(selectors).unwrap_or_else(|_| "[]".into());
    // Pre-flight has to see exactly what execution will see: css and
    // text resolve across open shadow roots, xpath stays main-document
    // (the XPath spec has no shadow boundary), matching exec_click.
    super::shadow::with_deep_helpers(&format!(
        r#"
  var sels = {arr};
  var results = [];
  for (var i = 0; i < sels.length; i++) {{
    var s = sels[i];
    var el = null;
    try {{
      if (s.indexOf("text=") === 0) {{
        var needle = s.slice(5).toLowerCase();
        var nodes = __ocWalk();
        for (var w = 0; w < nodes.length; w++) {{
          var node = nodes[w];
          var t = (node.innerText || node.textContent || "").toLowerCase();
          if (t.indexOf(needle) !== -1) {{
            var r = node.getBoundingClientRect();
            if (r.width > 0 && r.height > 0) {{ el = node; break; }}
          }}
        }}
      }} else if (s.indexOf("xpath=") === 0) {{
        var it = document.evaluate(s.slice(6), document, null,
          XPathResult.FIRST_ORDERED_NODE_TYPE, null);
        var n = it.singleNodeValue;
        if (n) {{
          var r2 = n.getBoundingClientRect();
          if (r2.width > 0 && r2.height > 0) el = n;
        }}
      }} else {{
        var q = __ocQueryOne(s);
        if (q) {{
          var r3 = q.getBoundingClientRect();
          if (r3.width > 0 && r3.height > 0) el = q;
        }}
      }}
    }} catch (e) {{
      results.push({{i: i, ok: false, reason: "invalid selector: " + e.message}});
      continue;
    }}
    results.push({{i: i, ok: !!el, reason: el ? "" : "not found or not visible"}});
  }}
  return results;"#
    ))
}

pub struct BrowserActTool {
    manager: Arc<BrowserManager>,
}

impl BrowserActTool {
    pub fn new(manager: Arc<BrowserManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for BrowserActTool {
    fn name(&self) -> &str {
        "browser_act"
    }

    fn description(&self) -> &str {
        "Execute a batch of browser actions in ONE call against the current page. \
         Actions run in order: click/fill/press/select/upload/wait (max 10). Every selector is \
         resolved BEFORE anything executes — one stale reference rejects the whole call \
         with zero side effects. On mid-sequence failure the completed prefix is reported \
         and the rest is skipped. Takes ONE screenshot at the end. Use this instead of \
         chained browser_click/browser_type calls whenever the sequence is known: one \
         round-trip, one stable view, one screenshot."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "actions": {
                    "type": "array",
                    "maxItems": 10,
                    "description": "Ordered actions. Each object has a \"type\" plus its fields: \
                        {\"type\":\"click\",\"selector\":\"…\"} (CSS, text=Label, xpath=//…, or f2:N frame index), \
                        {\"type\":\"fill\",\"selector\":\"…\",\"text\":\"…\"} (replaces the value, framework-safe), \
                        {\"type\":\"press\",\"key\":\"Enter\"}, \
                        {\"type\":\"select\",\"selector\":\"…\",\"value\":\"option value\"}, \
                        {\"type\":\"upload\",\"selector\":\"input[type=file]\",\"files\":[\"/abs/path/a.mp4\"]} \
                        (absolute paths to existing regular files; the bytes never enter the \
                        conversation), \
                        {\"type\":\"wait\",\"ms\":500} (max 10000).",
                }
            },
            "required": ["actions"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Network]
    }

    fn hints(&self) -> ToolHints {
        ToolHints {
            read_only: false,
            destructive: true,
            idempotent: false,
            open_world: true,
        }
    }

    fn requires_approval(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let main_page = match self
            .manager
            .get_or_create_session_page(context.session_id)
            .await
        {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(format!("Browser error: {e}"))),
        };

        // Overall batch budget (#1188): pre-flight + execution must fit
        // inside BATCH_TIMEOUT_SECS. Wait actions are individually capped
        // at MAX_WAIT_MS, so the sum is bounded, but a hung CDP call or a
        // pathological sequence still gets cut off here.
        tokio::time::timeout(
            std::time::Duration::from_secs(BATCH_TIMEOUT_SECS),
            self.run_batch(&input, context, &main_page),
        )
        .await
        .unwrap_or_else(|_| {
            Ok(ToolResult::error(format!(
                "Batch exceeded the overall {BATCH_TIMEOUT_SECS}s budget (pre-flight + execution). \
                 Reduce waits or split into multiple browser_act calls."
            )))
        })
    }
} // impl Tool for BrowserActTool

impl BrowserActTool {
    async fn run_batch(
        &self,
        input: &Value,
        context: &ToolExecutionContext,
        main_page: &chromiumoxide::Page,
    ) -> Result<ToolResult> {
        let acts = match parse_actions(input) {
            Ok(a) => a,
            Err(e) => return Ok(ToolResult::error(e)),
        };

        // Resolve every action's page up front: frame-prefixed selectors
        // (`f2:14`) route to their OOPIF page, everything else to main.
        // A stale frame label rejects the whole call (consistent with
        // click/type routing).
        use std::collections::HashMap;
        let mut frame_pages: HashMap<String, chromiumoxide::Page> = HashMap::new();
        for act in &acts {
            let Some(sel) = act.selector() else { continue };
            let Some((label, _)) = split_frame_selector(sel) else {
                continue;
            };
            if let std::collections::hash_map::Entry::Vacant(slot) =
                frame_pages.entry(label.clone())
            {
                match self
                    .manager
                    .oopif_page_by_label(context.session_id, &label)
                    .await
                {
                    Ok(Some(p)) => {
                        slot.insert(p);
                    }
                    Ok(None) => {
                        return Ok(ToolResult::error(format!(
                            "Frame '{label}' not found. The frame may have navigated away — \
                             re-run `browser_find` to get a fresh inventory."
                        )));
                    }
                    Err(e) => {
                        return Ok(ToolResult::error(format!("Browser error: {e}")));
                    }
                }
            }
        }

        // ---- Pre-flight: one JS pass per page resolving ALL selectors ----
        // Group selector-having actions by their page (main or frame),
        // run the resolver, map failures back to action indices.
        let mut page_selectors: Vec<(Option<String>, Vec<usize>)> = Vec::new(); // (frame label, action idx)
        for (idx, act) in acts.iter().enumerate() {
            if let Some(sel) = act.selector() {
                let label = split_frame_selector(sel).map(|(l, _)| l);
                match page_selectors.iter_mut().find(|(l, _)| *l == label) {
                    Some((_, v)) => v.push(idx),
                    None => page_selectors.push((label, vec![idx])),
                }
            }
        }
        for (label, idxs) in &page_selectors {
            let page = match label {
                Some(l) => &frame_pages[l],
                None => main_page,
            };
            let selectors: Vec<String> = idxs
                .iter()
                .map(|&i| acts[i].selector().unwrap().to_string())
                .collect();
            let js = build_pre_flight_js(&selectors);
            let raw = match page.evaluate(js.as_str()).await {
                Ok(r) => r.value().cloned().unwrap_or(Value::Null),
                Err(e) => return Ok(ToolResult::error(format!("Pre-flight check failed: {e}"))),
            };
            if let Some(arr) = raw.as_array() {
                for entry in arr {
                    let local_i = entry["i"].as_u64().unwrap_or(0) as usize;
                    let ok = entry["ok"].as_bool().unwrap_or(false);
                    if !ok && local_i < idxs.len() {
                        let act_idx = idxs[local_i];
                        let reason = entry["reason"].as_str().unwrap_or("unresolved");
                        let sel = acts[act_idx].selector().unwrap_or("?");
                        return Ok(ToolResult::error(format!(
                            "Pre-flight rejected: action {} targets '{sel}' — {reason}. \
                             NOTHING executed. Re-run `browser_find` for a fresh inventory.",
                            act_idx + 1
                        )));
                    }
                }
            }
        }

        // ---- Execute in order; abort on first failure ----
        let mut done = 0usize;
        for act in &acts {
            let page_ref = match act.selector().and_then(split_frame_selector) {
                Some((label, _)) => &frame_pages[&label],
                None => main_page,
            };
            let outcome: std::result::Result<(), String> = match act {
                Act::Click { selector } => self.exec_click(page_ref, selector).await,
                Act::Fill { selector, text } => self.exec_fill(page_ref, selector, text).await,
                Act::Press { key } => page_ref
                    .press_key(key.as_str())
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
                Act::Select { selector, value } => {
                    self.exec_select(page_ref, selector, value).await
                }
                Act::Upload { selector, files } => {
                    self.exec_upload(page_ref, selector, files).await
                }
                Act::Wait { ms } => {
                    tokio::time::sleep(std::time::Duration::from_millis(*ms)).await;
                    Ok(())
                }
            };
            match outcome {
                Ok(()) => done += 1,
                Err(e) => {
                    let report = format_prefix_report(done, act, &e);
                    self.manager
                        .reset_identical_screenshot_count(context.session_id)
                        .await;
                    let mut tr = ToolResult::error(super::events::append_line(
                        report,
                        self.manager.drain_recent_events(),
                    ));
                    self.manager
                        .attach_screenshot(context.session_id, &mut tr)
                        .await;
                    return Ok(tr);
                }
            }
        }

        // ---- Success: one screenshot, event line, summary ----
        self.manager
            .reset_identical_screenshot_count(context.session_id)
            .await;
        let summary = acts
            .iter()
            .map(|a| a.label())
            .collect::<Vec<_>>()
            .join(", ");
        // #2112: the attached paths are echoed in full. An upload is the one
        // action whose effect leaves this machine, so the result has to say
        // exactly which files went out, not just how many.
        let uploads: Vec<String> = acts
            .iter()
            .filter_map(|a| match a {
                Act::Upload { selector, files } => {
                    Some(format!("upload {selector}: {}", files.join(", ")))
                }
                _ => None,
            })
            .collect();
        let detail = if uploads.is_empty() {
            String::new()
        } else {
            format!("\nAttached files: {}", uploads.join("; "))
        };
        let mut result = ToolResult::success(super::events::append_line(
            format!(
                "Executed {} action{} (one call): {summary}{detail}",
                acts.len(),
                if acts.len() == 1 { "" } else { "s" }
            ),
            self.manager.drain_recent_events(),
        ));
        self.manager
            .attach_screenshot(context.session_id, &mut result)
            .await;
        Ok(result)
    }
}

impl BrowserActTool {
    /// Click honoring the three selector shapes. CSS goes through
    /// find_element (CDP-level trusted click); text=/xpath= use the
    /// in-page evaluator — same split as browser_click.
    async fn exec_click(
        &self,
        page: &chromiumoxide::Page,
        selector: &str,
    ) -> std::result::Result<(), String> {
        let selector = selector.to_string(); // own for the JS builds below
        if let Some(text) = selector.strip_prefix("text=") {
            let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
            // Composed-tree walk, matching `browser_find` mode=text and
            // browser_act's pre-flight: a `text=` target inside an open
            // shadow root must click here, or pre-flight accepts a
            // selector that execution then misses.
            let js = super::shadow::with_deep_helpers(&format!(
                r#"
                    const needle = "{escaped}".toLowerCase();
                    for (const node of __ocWalk()) {{
                        const t = (node.innerText || node.textContent || "").toLowerCase();
                        if (!t.includes(needle)) continue;
                        const r = node.getBoundingClientRect();
                        if (r.width === 0 || r.height === 0) continue;
                        node.scrollIntoView({{block: "center"}});
                        node.click();
                        return "ok";
                    }}
                    return "not_found";
                "#
            ));
            let r = page
                .evaluate(js.as_str())
                .await
                .map_err(|e| format!("text-click eval failed: {e}"))?;
            let v = r.value().and_then(|v| v.as_str().map(String::from));
            if v.as_deref() == Some("ok") {
                let _ = page
                    .wait_for_network_almost_idle_with_timeout(std::time::Duration::from_secs(3))
                    .await;
                Ok(())
            } else {
                Err(format!(
                    "no visible element matched text '{text}' (present during pre-flight; \
                     the view changed)"
                ))
            }
        } else if let Some(xpath) = selector.strip_prefix("xpath=") {
            let escaped = xpath.replace('\\', "\\\\").replace('"', "\\\"");
            let js = format!(
                r#"
                (() => {{
                    const it = document.evaluate("{escaped}", document, null,
                        XPathResult.FIRST_ORDERED_NODE_TYPE, null);
                    const node = it.singleNodeValue;
                    if (!node) return "not_found";
                    const r = node.getBoundingClientRect();
                    if (r.width === 0 || r.height === 0) return "not_visible";
                    node.scrollIntoView({{block: "center"}});
                    node.click();
                    return "ok";
                }})()
                "#
            );
            let r = page
                .evaluate(js.as_str())
                .await
                .map_err(|e| format!("xpath-click eval failed: {e}"))?;
            let v = r.value().and_then(|v| v.as_str().map(String::from));
            if v.as_deref() == Some("ok") {
                let _ = page
                    .wait_for_network_almost_idle_with_timeout(std::time::Duration::from_secs(3))
                    .await;
                Ok(())
            } else {
                Err(format!("xpath '{xpath}' no longer resolves"))
            }
        } else {
            // Plain CSS — CDP-level click like browser_click.
            let element = super::manager::resolve_element(page, &selector)
                .await
                .map_err(|e| format!("element '{selector}' not found: {e}"))?;
            element
                .click()
                .await
                .map_err(|e| format!("click failed: {e}"))?;
            let _ = page
                .wait_for_network_almost_idle_with_timeout(std::time::Duration::from_secs(3))
                .await;
            Ok(())
        }
    }

    /// Framework-safe fill — the native-setter dance from browser_type.
    async fn exec_fill(
        &self,
        page: &chromiumoxide::Page,
        selector: &str,
        text: &str,
    ) -> std::result::Result<(), String> {
        let sel_js = serde_json::to_string(selector).unwrap_or_else(|_| "null".into());
        let val_js = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
        let js = super::shadow::with_deep_helpers(&format!(
            r#"
  var sel = {sel_js};
  var val = {val_js};
  var el = sel ? __ocQueryOne(sel) : document.activeElement;
  if (!el) return "no_element";
  var editable = el.isContentEditable;
  if (el.value === undefined && !editable) return "not_input";
  try {{ el.focus(); }} catch (e) {{}}
  if (editable) {{
    el.textContent = val;
    el.dispatchEvent(new InputEvent("input", {{bubbles:true}}));
    el.dispatchEvent(new Event("change", {{bubbles:true}}));
    return "ok";
  }}
  var proto = el.tagName === "TEXTAREA"
    ? window.HTMLTextAreaElement.prototype
    : window.HTMLInputElement.prototype;
  var desc = Object.getOwnPropertyDescriptor(proto, "value");
  if (desc && desc.set) {{ desc.set.call(el, val); }} else {{ el.value = val; }}
  el.dispatchEvent(new Event("input", {{bubbles:true}}));
  el.dispatchEvent(new Event("change", {{bubbles:true}}));
  return "ok";"#
        ));
        let r = page
            .evaluate(js.as_str())
            .await
            .map_err(|e| format!("fill eval failed: {e}"))?;
        let v = r.value().and_then(|v| v.as_str().map(String::from));
        match v.as_deref() {
            Some("ok") => Ok(()),
            Some("no_element") => Err(format!(
                "fill target '{selector}' disappeared (was present during pre-flight)"
            )),
            Some("not_input") => Err(format!("fill target '{selector}' is not an input")),
            _ => Err(format!("fill failed on '{selector}'")),
        }
    }

    /// Fresh `<select>` handling: set value + dispatch change.
    async fn exec_select(
        &self,
        page: &chromiumoxide::Page,
        selector: &str,
        value: &str,
    ) -> std::result::Result<(), String> {
        let sel_js = serde_json::to_string(selector).unwrap_or_else(|_| "null".into());
        let val_js = serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into());
        let js = super::shadow::with_deep_helpers(&format!(
            r#"
  var el = __ocQueryOne({sel_js});
  if (!el) return "no_element";
  if (el.tagName !== "SELECT") return "not_select";
  var val = {val_js};
  var opt = Array.prototype.find.call(el.options, function(o) {{
    return o.value === val || o.text === val;
  }});
  if (!opt) return "no_option";
  el.value = opt.value;
  el.dispatchEvent(new Event("input", {{bubbles:true}}));
  el.dispatchEvent(new Event("change", {{bubbles:true}}));
  return "ok";"#
        ));
        let r = page
            .evaluate(js.as_str())
            .await
            .map_err(|e| format!("select eval failed: {e}"))?;
        let v = r.value().and_then(|v| v.as_str().map(String::from));
        match v.as_deref() {
            Some("ok") => Ok(()),
            Some("no_element") => Err(format!(
                "select target '{selector}' disappeared (was present during pre-flight)"
            )),
            Some("not_select") => Err(format!("select target '{selector}' is not a <select>")),
            Some("no_option") => Err(format!("no option matching '{value}' in '{selector}'")),
            _ => Err(format!("select failed on '{selector}'")),
        }
    }

    /// Attach local files to a file input via CDP (#2112).
    ///
    /// `DOM.setFileInputFiles` sets the input's file list as if the user had
    /// picked them, so the page's own `change` handler fires and every upload
    /// flow that exists for a human works here too. The paths were validated
    /// at parse time; the bytes never pass through this process.
    async fn exec_upload(
        &self,
        page: &chromiumoxide::Page,
        selector: &str,
        files: &[String],
    ) -> std::result::Result<(), String> {
        let element = super::manager::resolve_element(page, selector)
            .await
            .map_err(|e| format!("file input '{selector}' not found: {e}"))?;
        let params = chromiumoxide::cdp::browser_protocol::dom::SetFileInputFilesParams::builder()
            .files(files.to_vec())
            .backend_node_id(element.backend_node_id)
            .build()
            .map_err(|e| format!("cannot build the upload request: {e}"))?;
        page.execute(params)
            .await
            .map_err(|e| format!("upload failed on '{selector}': {e}"))?;
        Ok(())
    }
}
