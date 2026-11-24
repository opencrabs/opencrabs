//! Channel Search Tool
//!
//! Searches passively captured channel messages (Telegram groups, etc.).
//! Provides list_chats, recent, and search operations.

use super::error::Result;
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use crate::db::ChannelMessageRepository;
use async_trait::async_trait;
use chrono::DateTime;
use serde_json::Value;

/// Is this thread id the General-topic SESSION-SCOPING sentinel (#1220)?
fn is_general_topic(thread: i32) -> bool {
    thread == crate::channels::GENERAL_TOPIC_ID
}

/// Which slice of a chat's message history an operation reads (#718/#719).
/// ONE parameter carries all three states a topic-bound reader needs. Before
/// this, `thread_id` was free-form and omitting it meant "no filter at all",
/// so a session bound to a forum topic silently read every OTHER topic too.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TopicScope {
    /// This chat's General topic. Rows persist with `thread_id IS NULL`
    /// (General messages carry no `message_thread_id`), so this resolves to
    /// the `*_general` repository readers — never to a literal `'1'`
    /// comparison, which would match only the legacy sentinel rows instead.
    General,
    /// One real forum topic, keyed exactly as persisted.
    Thread(String),
    /// No thread filter: every topic of the chat, interleaved. The pre-#718
    /// behaviour, now an explicit opt-in.
    All,
}

impl TopicScope {
    /// Human label for the output header (D4). A wide read must be visible in
    /// the transcript, so the header states the scope actually used —
    /// including when it was inherited from the session binding rather than
    /// passed.
    fn label(&self, inherited: bool) -> String {
        let base = match self {
            Self::General => "General".to_string(),
            Self::Thread(t) => format!("topic {t}"),
            Self::All => "all topics".to_string(),
        };
        if inherited {
            format!("{base} inherited")
        } else {
            base
        }
    }
}

/// Tool for listing and searching channel message history.
pub struct ChannelSearchTool {
    repo: ChannelMessageRepository,
}

impl ChannelSearchTool {
    pub fn new(repo: ChannelMessageRepository) -> Self {
        Self { repo }
    }
}

/// Compare two chat ids. Both sides are platform ids rendered as strings, so
/// a textual compare is normally right; the numeric fallback means a
/// formatting difference cannot silently drop the session's topic and fall
/// through to an unfiltered read — which is the whole defect class (#718).
fn same_chat(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (a.parse::<i64>(), b.parse::<i64>()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Parse an explicit `topic_scope` value. `None` means the caller asked for
/// the SESSION's own topic, which is resolved against the ambient binding
/// below. An unknown value is a hard error naming the vocabulary: silently
/// falling back to `all` is exactly the leak #718 is about.
fn parse_topic_scope(raw: &str) -> Result<Option<TopicScope>> {
    match raw {
        "session" => Ok(None),
        "all" => Ok(Some(TopicScope::All)),
        "general" => Ok(Some(TopicScope::General)),
        other => match other.strip_prefix("thread:") {
            Some(id) if !id.is_empty() => Ok(Some(TopicScope::Thread(id.to_string()))),
            _ => Err(super::error::ToolError::InvalidInput(format!(
                "unknown topic_scope '{other}'. Valid: 'session' (default), 'general', 'all', 'thread:<id>'"
            ))),
        },
    }
}

/// Resolve the effective topic scope for one operation (#718/#719). Returns
/// the scope plus whether it was INHERITED from the session binding rather
/// than passed explicitly — the header says which, so a wide read can never
/// hide.
async fn resolve_topic_scope(
    input: &Value,
    ctx: &ToolExecutionContext,
    requested_chat: Option<&str>,
) -> Result<(TopicScope, bool)> {
    // An explicit request always wins over inheritance.
    if let Some(raw) = input.get("topic_scope").and_then(|v| v.as_str()) {
        if let Some(scope) = parse_topic_scope(raw.trim())? {
            return Ok((scope, false));
        }
        // "session" — fall through and inherit from the ambient binding.
    } else if let Some(tid) = input.get("thread_id").and_then(|v| v.as_str()) {
        // Deprecated alias (D2): `thread_id:"5"` keeps its old meaning.
        if !tid.is_empty() {
            return Ok((TopicScope::Thread(tid.to_string()), false));
        }
    }

    let Some(o) = ctx.origin_target.as_deref() else {
        // No ambient binding (DM, cron, CLI, sub-agent, TUI): there is no
        // topic to inherit, so read the whole chat — the pre-#718 behaviour.
        return Ok((TopicScope::All, true));
    };
    let Some(requested) = requested_chat else {
        // No chat given (a cross-chat `search`): topic ids are per-chat, so
        // there is nothing to inherit.
        return Ok((TopicScope::All, true));
    };
    if !same_chat(requested, &o.chat_id) {
        // #718 explicitly defers this choice to the fix. Falling back to
        // explicit-All rather than refusing, because the session has no topic
        // context for a foreign chat, the tool legitimately supports
        // cross-chat reads, and the header makes the wide read visible.
        return Ok((TopicScope::All, true));
    }

    match o.thread {
        Some(t) if !is_general_topic(t) => Ok((TopicScope::Thread(t.to_string()), true)),
        // A known forum's General topic normalizes to this sentinel (#1220).
        Some(_) => Ok((TopicScope::General, true)),
        // The binding carries no thread. On a known forum that is still the
        // General topic — a legacy NULL-encoded binding, or a row written
        // before the sentinel existed. The forum evidence is the same
        // `telegram_chat_topics` lookup the target resolver uses. When the
        // world is not wired (cron, CLI, tests) or the chat is not a forum,
        // this is a genuine DM / non-forum group and reads unfiltered.
        None => {
            let is_forum = match (ctx.world.as_ref(), o.channel) {
                (Some(world), "telegram") => match o.chat_id.parse::<i64>() {
                    Ok(cid) => {
                        matches!(world.telegram_chat_topics(cid).await, Ok(Some(t)) if !t.is_empty())
                    }
                    Err(_) => false,
                },
                _ => false,
            };
            if is_forum {
                Ok((TopicScope::General, true))
            } else {
                Ok((TopicScope::All, true))
            }
        }
    }
}

#[async_trait]
impl Tool for ChannelSearchTool {
    fn name(&self) -> &str {
        "channel_search"
    }

    fn description(&self) -> &str {
        "Search or list channel message history captured from Telegram groups, Telegram userbot chats, Discord, Slack, etc. \
         Use 'list_chats' to see known groups/channels with message counts. \
         Use 'recent' to get the last N messages from a specific chat. \
         Use 'search' to find messages by content across chats. \
         On a forum, results are scoped to the calling session's own topic by default — \
         the output header names the scope actually used."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["list_chats", "recent", "search", "attachments"],
                    "description": "'list_chats' to see known chats, 'recent' for last N messages, 'search' to find by content, 'attachments' to list stored files"
                },
                "channel": {
                    "type": "string",
                    "enum": ["telegram", "telegram-userbot", "discord", "slack", "whatsapp"],
                    "description": "Filter by channel platform (omit for all)"
                },
                "chat_id": {
                    "type": "string",
                    "description": "Chat/channel/group ID (required for 'recent' and 'attachments', optional for 'search')"
                },
                "query": {
                    "type": "string",
                    "description": "Search text (required for 'search')"
                },
                "n": {
                    "type": "integer",
                    "description": "Max results to return (default: 20)",
                    "default": 20
                },
                "topic_scope": {
                    "type": "string",
                    "description": "Which messages to read (optional; default 'session'). 'session': the calling session's own forum topic (on a DM/cron/CLI surface with no topic binding this reads the whole chat); 'general': the chat's General topic; 'all': every topic interleaved — an explicit opt-in, since omitting this no longer means 'no filter'; 'thread:<id>': one specific forum topic."
                },
                "thread_id": {
                    "type": "string",
                    "description": "DEPRECATED — use topic_scope 'thread:<id>'. Filter by thread/topic ID (optional, for Telegram forum topics)"
                },
                "message_type": {
                    "type": "string",
                    "enum": ["text", "document", "photo", "video", "voice", "video_note", "animation"],
                    "description": "Filter by message type (optional, for 'recent' and 'search')"
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

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let operation = input
            .get("operation")
            .and_then(|v| v.as_str())
            .unwrap_or("list_chats");

        let channel = input.get("channel").and_then(|v| v.as_str());
        let n = input.get("n").and_then(|v| v.as_i64()).unwrap_or(20);

        match operation {
            "list_chats" => {
                let chats = self
                    .repo
                    .list_chats(channel)
                    .await
                    .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;

                if chats.is_empty() {
                    return Ok(ToolResult::success(
                        "No channel messages captured yet.".to_string(),
                    ));
                }

                let lines: Vec<String> = chats
                    .iter()
                    .map(|c| {
                        let name = c.channel_chat_name.as_deref().unwrap_or("unnamed");
                        let ts = DateTime::from_timestamp(c.last_message_at, 0)
                            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                            .unwrap_or_default();
                        // One id, in one place (#985). This used to print the
                        // chat id twice, the first time quoted directly after
                        // the name, where it read as a handle rather than the
                        // same number again. The oc:// form (#148) is the
                        // target-URL half of the dual-form listing: raw id
                        // for humans, URL for targeting tools.
                        format!(
                            "- [{}] {} (id={}, target `oc://{}/{}`), {} msgs, last: {}",
                            c.channel,
                            name,
                            c.channel_chat_id,
                            c.channel,
                            c.channel_chat_id,
                            c.message_count,
                            ts
                        )
                    })
                    .collect();

                Ok(ToolResult::success(format!(
                    "Known chats ({}):\n{}",
                    chats.len(),
                    lines.join("\n")
                )))
            }

            "recent" => {
                let chat_id = match input.get("chat_id").and_then(|v| v.as_str()) {
                    Some(id) if !id.is_empty() => id,
                    _ => {
                        return Ok(ToolResult::error(
                            "'chat_id' is required for 'recent' operation.".to_string(),
                        ));
                    }
                };

                let (scope, inherited) =
                    resolve_topic_scope(&input, context, Some(chat_id)).await?;

                let messages = match &scope {
                    TopicScope::General => {
                        self.repo
                            .recent_general(channel.unwrap_or("telegram"), chat_id, n, None)
                            .await
                    }
                    TopicScope::Thread(t) => {
                        self.repo
                            .recent(channel, chat_id, n, Some(t.as_str()), None)
                            .await
                    }
                    TopicScope::All => self.repo.recent(channel, chat_id, n, None, None).await,
                }
                .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;

                if messages.is_empty() {
                    return Ok(ToolResult::success(format!(
                        "No messages found in chat {chat_id}."
                    )));
                }

                let lines: Vec<String> = messages
                    .iter()
                    .rev() // oldest first for readability
                    .map(|m| {
                        let ts = m.created_at.format("%m-%d %H:%M");
                        let pmid = m
                            .platform_message_id
                            .as_deref()
                            .map(|id| format!(" [msgid:{}]", id))
                            .unwrap_or_default();
                        format!("[{}]{} {}: {}", ts, pmid, m.sender_name, m.content)
                    })
                    .collect();

                Ok(ToolResult::success(format!(
                    "Recent messages in {} ({}, {} messages):\n{}",
                    chat_id,
                    scope.label(inherited),
                    messages.len(),
                    lines.join("\n")
                )))
            }

            "search" => {
                let query = match input.get("query").and_then(|v| v.as_str()) {
                    Some(q) if !q.is_empty() => q,
                    _ => {
                        return Ok(ToolResult::error(
                            "'query' is required for 'search' operation.".to_string(),
                        ));
                    }
                };

                let chat_id = input.get("chat_id").and_then(|v| v.as_str());
                let (scope, inherited) = resolve_topic_scope(&input, context, chat_id).await?;
                if matches!(scope, TopicScope::General) && chat_id.is_none() {
                    return Ok(ToolResult::error(
                        "'topic_scope: general' requires 'chat_id' — General is per-chat."
                            .to_string(),
                    ));
                }

                let messages = match &scope {
                    TopicScope::General => {
                        self.repo
                            .search_general(
                                channel.unwrap_or("telegram"),
                                chat_id.unwrap_or_default(),
                                query,
                                n,
                            )
                            .await
                    }
                    TopicScope::Thread(t) => {
                        self.repo
                            .search(channel, chat_id, query, n, Some(t.as_str()))
                            .await
                    }
                    TopicScope::All => self.repo.search(channel, chat_id, query, n, None).await,
                }
                .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;

                if messages.is_empty() {
                    return Ok(ToolResult::success(format!(
                        "No messages matching \"{query}\"."
                    )));
                }

                let lines: Vec<String> = messages
                    .iter()
                    .map(|m| {
                        let ts = m.created_at.format("%m-%d %H:%M");
                        let chat = m.channel_chat_name.as_deref().unwrap_or(&m.channel_chat_id);
                        let thread = m
                            .thread_id
                            .as_deref()
                            .map(|t| format!(" [{}]", t))
                            .unwrap_or_default();
                        let topic = m
                            .topic_name
                            .as_deref()
                            .map(|t| format!(" ({})", t))
                            .unwrap_or_default();
                        let pmid = m
                            .platform_message_id
                            .as_deref()
                            .map(|id| format!(" [msgid:{}]", id))
                            .unwrap_or_default();
                        format!(
                            "[{}] [{}:{}]{}{}{}: {}: {}",
                            ts, m.channel, chat, thread, topic, pmid, m.sender_name, m.content
                        )
                    })
                    .collect();

                Ok(ToolResult::success(format!(
                    "Search results for \"{}\" ({}, {} messages):\n{}",
                    query,
                    scope.label(inherited),
                    messages.len(),
                    lines.join("\n")
                )))
            }

            "attachments" => {
                let chat_id = match input.get("chat_id").and_then(|v| v.as_str()) {
                    Some(id) if !id.is_empty() => id,
                    _ => {
                        return Ok(ToolResult::error(
                            "'chat_id' is required for 'attachments' operation.".to_string(),
                        ));
                    }
                };

                let (scope, inherited) =
                    resolve_topic_scope(&input, context, Some(chat_id)).await?;

                let messages = match &scope {
                    TopicScope::General => {
                        self.repo
                            .recent_general(channel.unwrap_or("telegram"), chat_id, n, None)
                            .await
                    }
                    TopicScope::Thread(t) => {
                        self.repo
                            .recent(channel, chat_id, n, Some(t.as_str()), None)
                            .await
                    }
                    TopicScope::All => self.repo.recent(channel, chat_id, n, None, None).await,
                }
                .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;
                // Filter to only attachment types
                let attachments: Vec<_> = messages
                    .iter()
                    .filter(|m| {
                        matches!(
                            m.message_type.as_str(),
                            "document" | "photo" | "video" | "voice" | "video_note" | "animation"
                        )
                    })
                    .collect();

                if attachments.is_empty() {
                    return Ok(ToolResult::success(format!(
                        "No attachments found in chat {chat_id}."
                    )));
                }

                let lines: Vec<String> = attachments
                    .iter()
                    .map(|m| {
                        let ts = m.created_at.format("%m-%d %H:%M");
                        let topic = m
                            .topic_name
                            .as_deref()
                            .map(|t| format!(" ({})", t))
                            .unwrap_or_default();
                        format!(
                            "[{}] [{}]{} {}: {}",
                            ts, m.message_type, topic, m.sender_name, m.content
                        )
                    })
                    .collect();

                Ok(ToolResult::success(format!(
                    "Attachments in {} ({}, {} messages):\n{}",
                    chat_id,
                    scope.label(inherited),
                    attachments.len(),
                    lines.join("\n")
                )))
            }

            unknown => Ok(ToolResult::error(format!(
                "Unknown operation '{unknown}'. Valid: list_chats, recent, search, attachments"
            ))),
        }
    }
}
