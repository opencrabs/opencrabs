//! Feedback Analyze Tool
//!
//! Queries the feedback ledger and returns aggregated stats, trends, and
//! patterns for the agent to reason about self-improvement opportunities.

use super::error::Result;
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde_json::Value;

pub struct FeedbackAnalyzeTool;

#[async_trait]
impl Tool for FeedbackAnalyzeTool {
    fn name(&self) -> &str {
        "feedback_analyze"
    }

    fn description(&self) -> &str {
        "Analyze the feedback ledger to identify patterns, success rates, and \
         improvement opportunities. Returns aggregated stats on tool performance, \
         failure patterns, and trends. Use this to understand what's working well \
         and what needs improvement before proposing changes via self_improve."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "What to analyze: 'summary' (overall stats), 'tool_stats' (per-tool success rates), 'recent' (last N events), 'failures' (tool failures only), 'user_corrections' (every correction a user made), 'provider_errors' (every provider failure). The ledger records all six; the last two are not tool events and are not part of 'failures'.",
                    "enum": [
                        "summary",
                        "tool_stats",
                        "recent",
                        "failures",
                        "user_corrections",
                        "provider_errors"
                    ]
                },
                "tool": {
                    "type": "string",
                    "description": "Optional tool name. With 'tool_stats' the table narrows to that one tool; with 'failures' only that tool's failures are returned. Omit to see every tool.",
                    "default": ""
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results for 'recent', 'failures', 'user_corrections' and 'provider_errors'",
                    "default": 50
                }
            },
            "required": ["query"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![] // Read-only DB access
    }

    fn requires_approval(&self) -> bool {
        false
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("summary");
        let limit = input.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as u32;
        // Optional single-tool filter. Callers kept asking for one tool's stats
        // and the tool could only ever answer with every tool in one table, so
        // the request was retried as an invented query value instead (#2200).
        let tool_filter = input
            .get("tool")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|t| !t.is_empty());

        let Some(ref svc_ctx) = context.service_context else {
            return Ok(ToolResult::error(
                "No service context — feedback analysis requires a database connection".to_string(),
            ));
        };

        let repo = crate::db::repository::FeedbackLedgerRepository::new(svc_ctx.pool().clone());

        match query {
            "summary" => {
                let total = repo
                    .total_count()
                    .await
                    .map_err(|e| crate::brain::tools::ToolError::Execution(e.to_string()))?;
                let breakdown = repo
                    .summary()
                    .await
                    .map_err(|e| crate::brain::tools::ToolError::Execution(e.to_string()))?;

                if total == 0 {
                    return Ok(ToolResult::success(
                        "No feedback data yet. The feedback ledger is empty — \
                         observations accumulate as you use tools and interact with users. \
                         Use feedback_record to manually log patterns you notice."
                            .to_string(),
                    ));
                }

                let mut out = format!("Feedback Ledger Summary ({total} total events)\n\n");
                out.push_str("Event Type Breakdown:\n");
                for (event_type, count) in &breakdown {
                    let pct = (*count as f64 / total as f64) * 100.0;
                    out.push_str(&format!("  {event_type}: {count} ({pct:.1}%)\n"));
                }
                Ok(ToolResult::success(out))
            }

            "tool_stats" => {
                let mut stats = repo
                    .stats_by_dimension("tool_")
                    .await
                    .map_err(|e| crate::brain::tools::ToolError::Execution(e.to_string()))?;

                if let Some(tool) = tool_filter {
                    stats.retain(|s| s.dimension == tool);
                    if stats.is_empty() {
                        return Ok(ToolResult::success(format!(
                            "No execution data for tool '{tool}'. Check the spelling, or run \
                             'tool_stats' with no tool filter to list every tool the ledger knows."
                        )));
                    }
                }

                if stats.is_empty() {
                    return Ok(ToolResult::success(
                        "No tool execution data yet.".to_string(),
                    ));
                }

                let heading = match tool_filter {
                    Some(tool) => format!("Tool Performance Stats: {tool}\n\n"),
                    None => "Tool Performance Stats:\n\n".to_string(),
                };
                let mut out = heading;
                out.push_str(&format!(
                    "{:<20} {:>6} {:>6} {:>6} {:>8}\n",
                    "Tool", "Total", "OK", "Fail", "Rate"
                ));
                out.push_str(&"-".repeat(50));
                out.push('\n');
                for s in &stats {
                    out.push_str(&format!(
                        "{:<20} {:>6} {:>6} {:>6} {:>7.1}%\n",
                        s.dimension,
                        s.total_events,
                        s.successes,
                        s.failures,
                        s.success_rate * 100.0
                    ));
                }
                Ok(ToolResult::success(out))
            }

            "recent" => {
                let entries = repo
                    .recent(limit)
                    .await
                    .map_err(|e| crate::brain::tools::ToolError::Execution(e.to_string()))?;

                if entries.is_empty() {
                    return Ok(ToolResult::success("No recent feedback.".to_string()));
                }

                let mut out = format!("Recent Feedback ({} entries):\n\n", entries.len());
                for e in &entries {
                    out.push_str(&format!(
                        "[{}] {}/{} = {} {}\n",
                        e.created_at.format("%Y-%m-%d %H:%M"),
                        e.event_type,
                        e.dimension,
                        e.value,
                        e.metadata.as_deref().unwrap_or("")
                    ));
                }
                Ok(ToolResult::success(out))
            }

            "failures" => {
                // A filtered read cannot use `limit` as the SQL bound: the
                // newest `limit` failures may all belong to other tools, so
                // filtering afterwards would report "no failures" for a tool
                // that has plenty. Read a wider window, filter, then truncate.
                let fetch = match tool_filter {
                    // `limit.max(5_000)` as the ceiling, never a bare 5_000:
                    // clamp panics when the floor exceeds the ceiling, so a
                    // caller asking for more than 5,000 rows would abort.
                    Some(_) => limit.saturating_mul(20).clamp(limit, limit.max(5_000)),
                    None => limit,
                };
                let mut entries = repo
                    .by_event_type("tool_failure", fetch)
                    .await
                    .map_err(|e| crate::brain::tools::ToolError::Execution(e.to_string()))?;

                if let Some(tool) = tool_filter {
                    entries.retain(|e| e.dimension == tool);
                    entries.truncate(limit as usize);
                }

                if entries.is_empty() {
                    return Ok(ToolResult::success(match tool_filter {
                        Some(tool) => format!("No tool failures recorded for '{tool}'."),
                        None => "No tool failures recorded.".to_string(),
                    }));
                }

                let mut out = format!("Recent Failures ({} entries):\n\n", entries.len());
                for e in &entries {
                    out.push_str(&format!(
                        "[{}] {} — {}\n",
                        e.created_at.format("%Y-%m-%d %H:%M"),
                        e.dimension,
                        e.metadata.as_deref().unwrap_or("(no details)")
                    ));
                }
                Ok(ToolResult::success(out))
            }

            // The ledger records corrections and provider errors alongside tool
            // outcomes, but only the tool ones had a route out of this tool. A
            // caller asking for them was refused, which is a capability gap and
            // not misuse: the data was already in the table (#2200).
            "user_corrections" | "provider_errors" => {
                let (event_type, heading) = if query == "user_corrections" {
                    ("user_correction", "User Corrections")
                } else {
                    ("provider_error", "Provider Errors")
                };

                let entries = repo
                    .by_event_type(event_type, limit)
                    .await
                    .map_err(|e| crate::brain::tools::ToolError::Execution(e.to_string()))?;

                if entries.is_empty() {
                    return Ok(ToolResult::success(format!(
                        "No {event_type} entries recorded yet."
                    )));
                }

                let mut out = format!("{heading} ({} entries):\n\n", entries.len());
                for e in &entries {
                    out.push_str(&format!(
                        "[{}] {} = {} {}\n",
                        e.created_at.format("%Y-%m-%d %H:%M"),
                        e.dimension,
                        e.value,
                        e.metadata.as_deref().unwrap_or("")
                    ));
                }
                Ok(ToolResult::success(out))
            }

            other => Ok(ToolResult::error(format!(
                "Unknown query type: '{other}'. Use 'summary', 'tool_stats', 'recent', \
                 'failures', 'user_corrections', or 'provider_errors'. Add a \"tool\" field \
                 to narrow 'tool_stats' or 'failures' to one tool."
            ))),
        }
    }
}
