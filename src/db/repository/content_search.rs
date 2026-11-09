//! Session-level content search (#2716).
//!
//! Maps message-level content hits onto deduped session rows so the GUI rail
//! and the TUI session list can answer "which sessions mention X" with a
//! snippet, instead of dumping raw message hits. Reuses the existing
//! repository search (`search_by_content` plus the tokenized fallback); this
//! adds no new index. The grouping is a pure function so both surfaces share
//! one tested mapping.

use std::collections::HashMap;

use anyhow::Result;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::db::models::Message;
use crate::db::repository::{MessageRepository, SessionListOptions, SessionRepository};

/// Width of the snippet centered on the query, in characters.
const SNIPPET_CHARS: usize = 200;

/// One deduped session row produced by a content search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentSearchRow {
    pub session_id: Uuid,
    pub title: String,
    /// Snippet centered on the query, taken from the session's newest hit.
    pub snippet: String,
    /// How many of the returned message hits belonged to this session.
    pub match_count: usize,
    /// `created_at` of the newest matching hit. Rows sort by this, newest first.
    pub last_hit_at: DateTime<Utc>,
}

/// Group recency-ordered message hits into one row per session.
///
/// The first hit seen for a session supplies its title, snippet and recency;
/// every further hit only bumps `match_count`. Rows come back newest-first and
/// are truncated to `limit`. `titles` resolves session ids to display titles;
/// an unknown id falls back to "Untitled". `terms` are the snippet anchors:
/// the whole query first, then its individual words for tokenized matches.
pub fn map_hits_to_sessions(
    hits: &[Message],
    titles: &HashMap<Uuid, String>,
    terms: &[&str],
    limit: usize,
) -> Vec<ContentSearchRow> {
    let mut order: Vec<Uuid> = Vec::new();
    let mut rows: HashMap<Uuid, ContentSearchRow> = HashMap::new();
    for msg in hits {
        if let Some(row) = rows.get_mut(&msg.session_id) {
            row.match_count += 1;
            continue;
        }
        order.push(msg.session_id);
        rows.insert(
            msg.session_id,
            ContentSearchRow {
                session_id: msg.session_id,
                title: titles
                    .get(&msg.session_id)
                    .cloned()
                    .unwrap_or_else(|| "Untitled".to_string()),
                snippet: snippet(&msg.content, terms, SNIPPET_CHARS),
                match_count: 1,
                last_hit_at: msg.created_at,
            },
        );
    }
    let mut out: Vec<ContentSearchRow> = order
        .into_iter()
        .filter_map(|id| rows.remove(&id))
        .collect();
    // The repository returns hits newest-first, so first appearance already
    // orders sessions by recency; the stable sort makes that explicit and
    // keeps the mapping correct even for an unsorted input.
    out.sort_by_key(|a| std::cmp::Reverse(a.last_hit_at));
    out.truncate(limit);
    out
}

/// Run a content search across all sessions and return deduped session rows.
///
/// Calls the verbatim search first; when it finds nothing and the query has
/// more than one word, retries with the tokenized AND-match fallback (the
/// same semantics as the `session_search` tool, #1626). Titles are resolved
/// from the session list. `limit` bounds the returned rows; the message fetch
/// is widened so several sessions can surface even when one session holds
/// many hits.
pub async fn search_session_content(
    messages: &MessageRepository,
    sessions: &SessionRepository,
    query: &str,
    limit: usize,
) -> Result<Vec<ContentSearchRow>> {
    let trimmed = query.trim();
    if trimmed.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let fetch = limit.saturating_mul(5).clamp(1, 200);
    let mut hits = messages.search_by_content(None, trimmed, fetch).await?;
    let mut terms: Vec<&str> = vec![trimmed];
    if hits.is_empty() && trimmed.split_whitespace().count() >= 2 {
        hits = messages
            .search_by_content_tokenized(None, trimmed, fetch)
            .await?;
        terms.extend(trimmed.split_whitespace());
    }
    if hits.is_empty() {
        return Ok(Vec::new());
    }

    let all = sessions
        .list(SessionListOptions {
            include_archived: true,
            ..Default::default()
        })
        .await?;
    let titles: HashMap<Uuid, String> = all
        .into_iter()
        .map(|s| (s.id, s.title.unwrap_or_else(|| "Untitled".to_string())))
        .collect();

    Ok(map_hits_to_sessions(&hits, &titles, &terms, limit))
}

/// Center a single-line snippet on the earliest query-term occurrence.
fn snippet(body: &str, terms: &[&str], max_len: usize) -> String {
    let lower = body.to_lowercase();
    let best = terms
        .iter()
        .filter_map(|t| lower.find(&t.to_lowercase()))
        .min()
        .unwrap_or(0);
    // Center the window on the term: half the budget as lead context. The
    // term then sits mid-window with room to spare, and the tail clamps to
    // the body's end without ever cutting the term itself.
    let half = max_len / 2;
    let start = body.floor_char_boundary(best.saturating_sub(half));
    let end = body.ceil_char_boundary((start + max_len).min(body.len()));
    let mut out = String::new();
    if start > 0 {
        out.push_str("...");
    }
    out.push_str(body[start..end].trim());
    if end < body.len() {
        out.push_str("...");
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{map_hits_to_sessions, search_session_content};
    use crate::db::Database;
    use crate::db::models::{Message, Session};
    use crate::db::repository::{MessageRepository, SessionRepository};
    use chrono::Utc;
    use std::collections::HashMap;
    use uuid::Uuid;

    /// A user hit whose recency is `secs_ago` seconds in the past, so tests
    /// can pin newest-first ordering without sleeping.
    fn hit(sid: Uuid, content: &str, secs_ago: i64) -> Message {
        let mut m = Message::new(sid, "user".to_string(), content.to_string(), 1);
        m.created_at = Utc::now() - chrono::Duration::seconds(secs_ago);
        m
    }

    #[test]
    fn content_search_map_dedupes_multiple_hits_in_one_session() {
        let sid = Uuid::new_v4();
        let hits = vec![
            hit(sid, "newest mention of rust", 1),
            hit(sid, "another rust line", 5),
            hit(sid, "older rust note", 9),
        ];
        let mut titles = HashMap::new();
        titles.insert(sid, "My session".to_string());
        let rows = map_hits_to_sessions(&hits, &titles, &["rust"], 10);
        assert_eq!(rows.len(), 1, "three hits collapse to one session row");
        assert_eq!(rows[0].session_id, sid);
        assert_eq!(rows[0].title, "My session");
        assert_eq!(rows[0].match_count, 3);
        assert!(rows[0].snippet.contains("rust"));
        assert_eq!(
            rows[0].last_hit_at, hits[0].created_at,
            "newest hit sets recency"
        );
    }

    #[test]
    fn content_search_map_spans_sessions_newest_first() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();
        // Deliberately unsorted input: the mapping must sort by recency.
        let hits = vec![
            hit(b, "rust in b", 5),
            hit(a, "rust in a", 1),
            hit(c, "rust in c", 9),
        ];
        let rows = map_hits_to_sessions(&hits, &HashMap::new(), &["rust"], 10);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].session_id, a);
        assert_eq!(rows[1].session_id, b);
        assert_eq!(rows[2].session_id, c);
        assert!(
            rows.iter().all(|r| r.title == "Untitled"),
            "unknown ids fall back"
        );
    }

    #[test]
    fn content_search_map_respects_limit() {
        let hits: Vec<Message> = (0..5).map(|i| hit(Uuid::new_v4(), "rust", i)).collect();
        let rows = map_hits_to_sessions(&hits, &HashMap::new(), &["rust"], 2);
        assert_eq!(rows.len(), 2, "limit bounds the row count");
    }

    #[test]
    fn content_search_map_snippet_centers_on_query() {
        let sid = Uuid::new_v4();
        // Both fillers outrun the snippet window on their own side, so the
        // tail is genuinely cut and owes an ellipsis. With a short tail the
        // window ran to the end of the body and the test proved nothing.
        let body = format!("{} target word here {}", "x".repeat(200), "y".repeat(200));
        let hits = vec![hit(sid, &body, 1)];
        let rows = map_hits_to_sessions(&hits, &HashMap::new(), &["target"], 10);
        assert!(rows[0].snippet.contains("target"));
        assert!(
            rows[0].snippet.starts_with("..."),
            "trimmed lead gets an ellipsis"
        );
        assert!(
            rows[0].snippet.ends_with("..."),
            "trimmed tail gets an ellipsis"
        );
    }

    #[test]
    fn content_search_map_empty_hits_yield_no_rows() {
        let rows = map_hits_to_sessions(&[], &HashMap::new(), &["rust"], 10);
        assert!(rows.is_empty());
    }

    async fn setup() -> (Database, SessionRepository, MessageRepository) {
        let db = Database::connect_in_memory().await.expect("in-memory db");
        db.run_migrations().await.expect("migrations");
        let sessions = SessionRepository::new(db.pool().clone());
        let messages = MessageRepository::new(db.pool().clone());
        (db, sessions, messages)
    }

    async fn session(sessions: &SessionRepository, title: &str) -> Uuid {
        let s = Session::new(Some(title.to_string()), None, None);
        let id = s.id;
        sessions.create(&s).await.expect("create session");
        id
    }

    async fn say(messages: &MessageRepository, sid: Uuid, content: &str, seq: i32) {
        let m = Message::new(sid, "user".to_string(), content.to_string(), seq);
        messages.create(&m).await.expect("create message");
    }

    #[tokio::test]
    async fn content_search_db_dedupes_and_spans_sessions() {
        let (_db, sessions, messages) = setup().await;
        let a = session(&sessions, "Alpha").await;
        let b = session(&sessions, "Beta").await;
        say(&messages, a, "rust language rocks", 1).await;
        say(&messages, a, "more rust here", 2).await;
        say(&messages, b, "rust in beta", 1).await;
        say(&messages, b, "nothing relevant", 2).await;

        let rows = search_session_content(&messages, &sessions, "rust", 10)
            .await
            .expect("search");
        assert_eq!(rows.len(), 2, "one row per session, not per hit");
        let ra = rows.iter().find(|r| r.session_id == a).expect("alpha row");
        let rb = rows.iter().find(|r| r.session_id == b).expect("beta row");
        assert_eq!(ra.title, "Alpha");
        assert_eq!(ra.match_count, 2);
        assert_eq!(rb.match_count, 1);
        assert!(ra.snippet.to_lowercase().contains("rust"));
    }

    #[tokio::test]
    async fn content_search_db_tokenized_fallback_on_scattered_words() {
        let (_db, sessions, messages) = setup().await;
        let a = session(&sessions, "Scattered").await;
        // Both words present, never contiguous: verbatim LIKE finds nothing,
        // so the tokenized AND-match fallback must surface the session.
        say(&messages, a, "the quick brown fox jumps", 1).await;
        let rows = search_session_content(&messages, &sessions, "quick fox", 10)
            .await
            .expect("search");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id, a);
        assert!(rows[0].snippet.contains("quick"));
    }

    #[tokio::test]
    async fn content_search_db_empty_query_returns_empty() {
        let (_db, sessions, messages) = setup().await;
        let rows = search_session_content(&messages, &sessions, "   ", 10)
            .await
            .expect("search");
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn content_search_db_no_match_returns_empty() {
        let (_db, sessions, messages) = setup().await;
        let a = session(&sessions, "A").await;
        say(&messages, a, "hello world", 1).await;
        let rows = search_session_content(&messages, &sessions, "zzznotpresent", 10)
            .await
            .expect("search");
        assert!(rows.is_empty());
    }
}
