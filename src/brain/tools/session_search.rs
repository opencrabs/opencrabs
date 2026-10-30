//! Session Search Tool
//!
//! Searches chat session message history with a direct case-insensitive
//! SQL LIKE query against the `messages` table. Always up to date and
//! exhaustive (no indexing/truncation), so the agent can find content in
//! its own active session including thousands of messages back.

use super::error::Result;
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
#[cfg(feature = "telegram")]
use crate::channels::telegram::TelegramState;
use crate::db::Pool;
use async_trait::async_trait;
use serde_json::Value;
#[cfg(feature = "telegram")]
use std::sync::Arc;

/// Tool for listing and searching session message history via direct DB search.
pub struct SessionSearchTool {
    pool: Pool,
    /// Live channel state, wired only by the interactive registration
    /// (`with_telegram`); the core daemon/cron registration keeps `None`
    /// and rows omit turn info rather than guess.
    #[cfg(feature = "telegram")]
    telegram: Option<Arc<TelegramState>>,
}

impl SessionSearchTool {
    pub fn new(pool: Pool) -> Self {
        Self {
            pool,
            #[cfg(feature = "telegram")]
            telegram: None,
        }
    }

    /// Attach live channel state so discovery rows report turn activity.
    /// Registered by the interactive path once the bot connects; the registry
    /// insert replaces the stateless core instance by name.
    #[cfg(feature = "telegram")]
    pub fn with_telegram(pool: Pool, telegram: Arc<TelegramState>) -> Self {
        Self {
            pool,
            telegram: Some(telegram),
        }
    }

    /// `"running"` while a turn is in flight for the session, `"idle"` when
    /// it is waiting; `None` when no channel state is wired (#1203). A
    /// running session drains a queued push at its next tool-loop boundary;
    /// an idle one starts a fresh turn for it.
    #[cfg(feature = "telegram")]
    pub(crate) fn turn_state(&self, session_id: uuid::Uuid) -> Option<&'static str> {
        self.telegram.as_ref().map(|tg| {
            if tg.is_turn_active(session_id) {
                "running"
            } else {
                "idle"
            }
        })
    }

    /// Stateless counterpart so call sites stay cfg-free: no channel state,
    /// no turn info.
    #[cfg(not(feature = "telegram"))]
    fn turn_state(&self, _session_id: uuid::Uuid) -> Option<&'static str> {
        None
    }
}

#[async_trait]
impl Tool for SessionSearchTool {
    fn name(&self) -> &str {
        "session_search"
    }

    fn description(&self) -> &str {
        "Search or list chat session history using direct case-insensitive substring \
         search against the messages table. Always up-to-date and exhaustive. \
         Use 'list' to show all sessions with titles, dates, and message counts. \
         Use 'search' to find messages across sessions by substring query. \
         Use 'tail' to read the last N messages of one session (cheap on huge histories). \
         Use 'query' for machine-readable session discovery: JSON rows with full session ids, \
         titles, last-active timestamps and message counts - e.g. to find another session's id \
         to target with session_notify. \
         'session' can be a number (1 = most recent), a title keyword, or 'all' (default; \
         ignored by 'tail', which falls back to the newest session). \
         Each row also carries 'turn' - 'running' (a turn is in flight; a \
         queued push drains at its next tool-loop boundary), 'idle' (waiting; \
         a push starts a fresh turn) or null when channel state is unavailable."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["list", "search", "tail", "query"],
                    "description": "'list' to show sessions, 'search' to find messages, 'tail' to read the last N messages of a session, 'query' for machine-readable session discovery"
                },
                "query": {
                    "type": "string",
                    "description": "Natural-language query (required for 'search')"
                },
                "session": {
                    "type": "string",
                    "description": "Session to search: number (1=most recent), title keyword, or 'all' (default)"
                },
                "status": {
                    "type": "string",
                    "enum": ["active", "archived", "all"],
                    "description": "Session state filter for 'query' (default: active)"
                },
                "title_contains": {
                    "type": "string",
                    "description": "Case-insensitive title substring filter for 'query'"
                },
                "updated_since": {
                    "type": "string",
                    "description": "'query' only: sessions last active after this point. RFC3339 timestamp or Nd/Nh shorthand (e.g. 7d, 24h)"
                },
                "limit": {
                    "type": "integer",
                    "description": "'query' only: max rows returned (default: 50, max: 500)"
                },
                "n": {
                    "type": "integer",
                    "description": "Max results for 'search'/'tail' (default: 10)",
                    "default": 10
                }
            },
            "required": ["operation"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadFiles]
    }

    fn requires_approval(&self) -> bool {
        false
    }

    async fn execute(&self, input: Value, _context: &ToolExecutionContext) -> Result<ToolResult> {
        let operation = input
            .get("operation")
            .and_then(|v| v.as_str())
            .unwrap_or("list");

        match operation {
            "list" => self.list_sessions().await,
            "tail" => {
                let session_filter = input
                    .get("session")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                // Default 10, clamped to 100 so a stray huge n can't dump a
                // whole history into context.
                let n = input
                    .get("n")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10)
                    .clamp(1, 100) as usize;
                self.tail_session(session_filter.as_deref(), n).await
            }
            "search" => {
                // Trim first: a whitespace-only query is as empty as a
                // missing one, and passing it through only moves the same
                // failure one layer deeper.
                let query = input
                    .get("query")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|q| !q.is_empty())
                    .map(str::to_string);
                let Some(query) = query else {
                    // An empty search query is the #1 avoidable failure
                    // class in the feedback ledger (600+ occurrences):
                    // autoheal and cron loops call `search` with no query
                    // when what they actually want is "what are the recent
                    // sessions". Erroring just feeds the retry loop;
                    // handing back the list they meant — with a banner that
                    // teaches the right call — ends it.
                    let banner = "Empty 'query' on search — returning the recent-session list instead. For substring search, pass a non-empty query. For the last messages of a session, use operation='tail' with a session filter.";
                    let mut result = self.list_sessions().await?;
                    result.output = format!("{}\n\n{}", banner, result.output);
                    return Ok(result);
                };
                let session_filter = input
                    .get("session")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let n = input.get("n").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
                self.search_sessions(&query, session_filter.as_deref(), n)
                    .await
            }
            "query" => {
                let status = input
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("active");
                if !matches!(status, "active" | "archived" | "all") {
                    return Ok(ToolResult::error(
                        "Invalid status. Use 'active', 'archived', or 'all'.".to_string(),
                    ));
                }
                let title_contains = input
                    .get("title_contains")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty());
                let updated_since = match input.get("updated_since").and_then(|v| v.as_str()) {
                    Some(raw) => match parse_updated_since(raw) {
                        Ok(dt) => Some(dt),
                        Err(e) => return Ok(ToolResult::error(e)),
                    },
                    None => None,
                };
                // Default 50, clamped to 500 so one call can't flood context.
                let limit = input
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50)
                    .clamp(1, 500) as usize;
                self.query_sessions(status, title_contains, updated_since, limit)
                    .await
            }
            _ => Ok(ToolResult::error(format!(
                "Unknown operation '{}'. Use 'list', 'search', 'tail', or 'query'.",
                operation
            ))),
        }
    }
}

impl SessionSearchTool {
    async fn list_sessions(&self) -> Result<ToolResult> {
        use crate::db::repository::{MessageRepository, SessionListOptions, SessionRepository};

        let session_repo = SessionRepository::new(self.pool.clone());
        let message_repo = MessageRepository::new(self.pool.clone());

        let sessions = session_repo
            .list(SessionListOptions {
                include_archived: false,
                limit: None,
                offset: 0,
                query: None,
                include_subagents: false,
            })
            .await
            .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;

        if sessions.is_empty() {
            return Ok(ToolResult::success("No sessions found.".to_string()));
        }

        let mut output = String::from(
            "Sessions, newest first ([short-id] identifies each row; · running/idle = turn state):\n",
        );
        for (i, session) in sessions.iter().enumerate() {
            let count = message_repo.count_by_session(session.id).await.unwrap_or(0);
            let title = session.title.as_deref().unwrap_or("Untitled");
            let date = session.updated_at.format("%Y-%m-%d %H:%M").to_string();
            let turn = match self.turn_state(session.id) {
                Some(t) => format!(" · {t}"),
                None => String::new(),
            };
            output.push_str(&format!(
                "{}. [{}] \"{}\" — last active {}, {} messages{}, target `oc://session/{}`\n",
                i + 1,
                &session.id.to_string()[..8],
                title,
                date,
                count,
                turn,
                session.id
            ));
        }

        Ok(ToolResult::success(output))
    }

    /// Machine-readable session discovery.
    ///
    /// Returns JSON rows `{id, title, last_active, messages, turn}` with FULL
    /// UUIDs so other tools (`session_notify`) can be targeted without
    /// parsing the human-oriented `list` output. Rows are ordered by last
    /// activity, newest first (repo-level ORDER BY). `turn` is `"running"`
    /// while a turn is in flight, `"idle"` when waiting, `null` when no live
    /// channel state is wired.
    async fn query_sessions(
        &self,
        status: &str,
        title_contains: Option<&str>,
        updated_since: Option<chrono::DateTime<chrono::Utc>>,
        limit: usize,
    ) -> Result<ToolResult> {
        use crate::db::repository::{MessageRepository, SessionListOptions, SessionRepository};

        let session_repo = SessionRepository::new(self.pool.clone());
        let message_repo = MessageRepository::new(self.pool.clone());

        // Repo sorts by updated_at DESC already; archived/all need the wider
        // SQL net, then a Rust-side filter narrows to archived-only rows.
        let include_archived = status != "active";
        let sessions = session_repo
            .list(SessionListOptions {
                include_archived,
                limit: None,
                offset: 0,
                query: title_contains.map(str::to_string),
                include_subagents: false,
            })
            .await
            .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;

        let mut selected: Vec<_> = sessions
            .into_iter()
            .filter(|s| status != "archived" || s.archived_at.is_some())
            .filter(|s| updated_since.is_none_or(|since| s.updated_at > since))
            .collect();

        if selected.is_empty() {
            return Ok(ToolResult::success("[]".to_string()));
        }
        selected.truncate(limit);

        let mut rows = Vec::with_capacity(selected.len());
        for s in &selected {
            let count = message_repo.count_by_session(s.id).await.unwrap_or(0);
            rows.push(serde_json::json!({
                "id": s.id.to_string(),
                "title": s.title.as_deref().unwrap_or("Untitled"),
                "last_active": s
                    .updated_at
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                "messages": count,
                "turn": self.turn_state(s.id),
            }));
        }

        let json = serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".to_string());
        Ok(ToolResult::success(json))
    }

    async fn search_sessions(
        &self,
        query: &str,
        session_filter: Option<&str>,
        n: usize,
    ) -> Result<ToolResult> {
        use crate::db::repository::{MessageRepository, SessionListOptions, SessionRepository};

        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(ToolResult::error("Query cannot be empty.".to_string()));
        }

        let session_repo = SessionRepository::new(self.pool.clone());
        let message_repo = MessageRepository::new(self.pool.clone());

        // Load all sessions (most-recent-first) to resolve the filter and to
        // map session ids -> titles for output formatting.
        let all_sessions = session_repo
            .list(SessionListOptions {
                include_archived: true,
                limit: None,
                offset: 0,
                query: None,
                include_subagents: false,
            })
            .await
            .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;

        let target_sessions: Vec<_> = match session_filter {
            None | Some("all") => all_sessions.clone(),
            Some(filter) => {
                if let Ok(idx) = filter.parse::<usize>() {
                    all_sessions
                        .get(idx.saturating_sub(1))
                        .cloned()
                        .into_iter()
                        .collect()
                } else {
                    let lower = filter.to_lowercase();
                    all_sessions
                        .iter()
                        .filter(|s| {
                            s.title
                                .as_deref()
                                .unwrap_or("")
                                .to_lowercase()
                                .contains(&lower)
                        })
                        .cloned()
                        .collect()
                }
            }
        };

        if target_sessions.is_empty() {
            return Ok(ToolResult::success(
                "No matching sessions found.".to_string(),
            ));
        }

        // Scope the SQL to the resolved session ids when a filter was given.
        // For "all" / unfiltered, pass None so the query scans every session.
        let scope_ids: Option<Vec<uuid::Uuid>> = match session_filter {
            None | Some("all") => None,
            Some(_) => Some(target_sessions.iter().map(|s| s.id).collect()),
        };

        let messages = message_repo
            .search_by_content(scope_ids.as_deref(), trimmed, n)
            .await
            .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;

        let title_map: std::collections::HashMap<uuid::Uuid, String> = all_sessions
            .iter()
            .map(|s| {
                (
                    s.id,
                    s.title.clone().unwrap_or_else(|| "Untitled".to_string()),
                )
            })
            .collect();

        let render = |msg: &crate::db::models::Message, terms: &[&str]| -> String {
            let title = title_map
                .get(&msg.session_id)
                .map(String::as_str)
                .unwrap_or("Untitled");
            let date = msg.created_at.format("%Y-%m-%d %H:%M").to_string();
            let role = if msg.role == "user" {
                "user"
            } else {
                "assistant"
            };
            let snippet = extract_snippet(&msg.content, terms, 280);
            format!("**{}** [{} • {}]\n   {}\n\n", title, role, date, snippet)
        };

        // #1626: zero contiguous-verbatim hits do not mean the content is
        // absent — a natural-language query scatters its words across a
        // sentence. Before reporting nothing, retry multi-word queries with
        // a tokenized AND-match (every word present somewhere) and label the
        // rows so the weaker semantics stay explicit.
        if messages.is_empty() {
            let no_match = || {
                ToolResult::success(format!(
                    "No messages found matching '{}' in the selected session(s).",
                    trimmed
                ))
            };
            if trimmed.split_whitespace().count() < 2 {
                // Single token: verbatim and tokenized are the same query;
                // retrying would only duplicate the work.
                return Ok(no_match());
            }
            let tokenized = message_repo
                .search_by_content_tokenized(scope_ids.as_deref(), trimmed, n)
                .await
                .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;
            if tokenized.is_empty() {
                return Ok(no_match());
            }
            let tokens: Vec<&str> = trimmed.split_whitespace().collect();
            let mut terms: Vec<&str> = vec![trimmed];
            terms.extend(tokens.iter().copied());
            let mut output = format!(
                "~ token match — no contiguous match for '{}'; every row below \
                 contains all of its words, ranked by word hits then recency.\n\n",
                trimmed
            );
            for msg in &tokenized {
                output.push_str(&render(msg, &terms));
            }
            return Ok(ToolResult::success(output));
        }

        let mut output = String::new();
        for msg in &messages {
            output.push_str(&render(msg, std::slice::from_ref(&trimmed)));
        }

        Ok(ToolResult::success(output))
    }

    /// Read the last `n` messages of one session, oldest-first.
    ///
    /// Filter resolution mirrors `search_sessions` (number = position in the
    /// most-recent-first list, else title keyword). With no filter, the
    /// newest session is used. 'all' is rejected: tail needs one session.
    async fn tail_session(&self, session_filter: Option<&str>, n: usize) -> Result<ToolResult> {
        use crate::db::repository::{MessageRepository, SessionListOptions, SessionRepository};

        if session_filter == Some("all") {
            return Ok(ToolResult::error(
                "'tail' needs a single session. Pass a session number (see 'list'), \
                 a title keyword, or no 'session' to tail the newest one."
                    .to_string(),
            ));
        }

        let session_repo = SessionRepository::new(self.pool.clone());
        let message_repo = MessageRepository::new(self.pool.clone());

        let all_sessions = session_repo
            .list(SessionListOptions {
                include_archived: true,
                limit: None,
                offset: 0,
                query: None,
                include_subagents: false,
            })
            .await
            .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;

        if all_sessions.is_empty() {
            return Ok(ToolResult::success("No sessions found.".to_string()));
        }

        let target = match session_filter {
            None => all_sessions.first().cloned(),
            Some(filter) => {
                // Raw session id (spec: "session id or newest default").
                if let Ok(id) = filter.parse::<uuid::Uuid>() {
                    all_sessions.iter().find(|s| s.id == id).cloned()
                } else if let Ok(idx) = filter.parse::<usize>() {
                    all_sessions.get(idx.saturating_sub(1)).cloned()
                } else {
                    let lower = filter.to_lowercase();
                    all_sessions
                        .iter()
                        .find(|s| {
                            s.title
                                .as_deref()
                                .unwrap_or("")
                                .to_lowercase()
                                .contains(&lower)
                        })
                        .cloned()
                }
            }
        };

        let Some(session) = target else {
            return Ok(ToolResult::success(format!(
                "No matching session found for '{}'. Use 'list' to see sessions.",
                session_filter.unwrap_or("newest")
            )));
        };

        let messages = message_repo
            .find_recent_by_session(session.id, n)
            .await
            .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;

        let title = session.title.as_deref().unwrap_or("Untitled");
        if messages.is_empty() {
            return Ok(ToolResult::success(format!(
                "Session \"{}\" has no messages.",
                title
            )));
        }

        let mut output = format!(
            "Last {} of {} messages in \"{}\":\n\n",
            messages.len(),
            message_repo.count_by_session(session.id).await.unwrap_or(0),
            title
        );
        for msg in &messages {
            let date = msg.created_at.format("%Y-%m-%d %H:%M").to_string();
            let role = if msg.role == "user" {
                "user"
            } else {
                "assistant"
            };
            let body: String = msg.content.split_whitespace().collect::<Vec<_>>().join(" ");
            let body = if body.len() > 500 {
                let mut end = 500.min(body.len());
                while !body.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}...", &body[..end])
            } else {
                body
            };
            output.push_str(&format!("[{}] {}: {}\n\n", date, role, body));
        }

        Ok(ToolResult::success(output))
    }
}

fn extract_snippet(body: &str, terms: &[&str], max_len: usize) -> String {
    let body_lower = body.to_lowercase();

    // Center on the whole query when it is present (terms[0]); otherwise on
    // the earliest single-term occurrence so tokenized rows (#1626) point
    // at a matching word instead of the body's start.
    let best_pos = terms
        .first()
        .and_then(|q| body_lower.find(&q.to_lowercase()))
        .or_else(|| {
            terms
                .iter()
                .skip(1)
                .filter_map(|t| body_lower.find(&t.to_lowercase()))
                .min()
        })
        .unwrap_or(0);

    let start = best_pos.saturating_sub(50);
    let end = (start + max_len).min(body.len());
    let start = body.floor_char_boundary(start);
    let end = body.ceil_char_boundary(end);

    let mut snippet = String::new();
    if start > 0 {
        snippet.push_str("...");
    }
    snippet.push_str(body[start..end].trim());
    if end < body.len() {
        snippet.push_str("...");
    }

    // Collapse runs of whitespace so multi-line content stays readable in the
    // single-line snippet output.
    snippet.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse an `updated_since` spec: RFC3339 timestamp, or `Nd`/`Nh` shorthand
/// meaning N days/hours back from now.
pub(crate) fn parse_updated_since(
    spec: &str,
) -> std::result::Result<chrono::DateTime<chrono::Utc>, String> {
    let spec = spec.trim();
    let invalid = || {
        format!(
            "Invalid updated_since '{}': use RFC3339 (2026-08-25T00:00:00Z) or Nd/Nh shorthand (7d, 24h)",
            spec
        )
    };
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(spec) {
        return Ok(dt.with_timezone(&chrono::Utc));
    }
    if spec.len() < 2 {
        return Err(invalid());
    }
    // The unit is the final character, which may be multi-byte. Slicing at
    // `len - 1` would land inside it and panic.
    let Some((last, _)) = spec.char_indices().next_back() else {
        return Err(invalid());
    };
    let (num, unit) = spec.split_at(last);
    let Ok(n) = num.parse::<i64>() else {
        return Err(invalid());
    };
    let duration = match (unit, n) {
        ("h", n) if n > 0 => chrono::Duration::hours(n),
        ("d", n) if n > 0 => chrono::Duration::days(n),
        _ => return Err(invalid()),
    };
    Ok(chrono::Utc::now() - duration)
}
