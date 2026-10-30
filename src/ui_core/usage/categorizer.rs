//! Session auto-categorizer
//!
//! Runs on startup (or periodically) to classify uncategorized sessions.
//! Uses the LLM to batch-classify session titles into activity categories,
//! then persists the result to `sessions.category`.

use crate::db::{Pool, interact_err};
use anyhow::{Context, Result};

/// Valid categories the LLM should pick from.
pub const CATEGORIES: &[&str] = &[
    "Development",
    "Bug Fixes",
    "Features",
    "Refactoring",
    "Testing",
    "Documentation",
    "CI/Deploy",
    "Config",
    "Research",
    "DevOps",
    "Automation",
];

/// A session pending categorization.
#[derive(Debug, Clone)]
pub struct UncategorizedSession {
    pub id: String,
    pub title: String,
}

/// Fetch sessions that have no category set yet.
pub async fn fetch_uncategorized(pool: &Pool, limit: usize) -> Result<Vec<UncategorizedSession>> {
    let conn = pool.get().await.context("pool")?;
    conn.interact(move |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, COALESCE(title, '') FROM sessions \
             WHERE category IS NULL AND title IS NOT NULL AND title != '' \
             AND title != 'recovered' AND title != 'New Chat' \
             ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |row| {
            Ok(UncategorizedSession {
                id: row.get(0)?,
                title: row.get(1)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
    })
    .await
    .map_err(interact_err)?
    .context("Failed to fetch uncategorized sessions")
}

/// Build the prompt for the LLM to classify session titles.
pub fn build_classification_prompt(sessions: &[UncategorizedSession]) -> String {
    let categories = CATEGORIES.join(", ");
    let mut prompt = format!(
        "Classify each session title into exactly ONE category from this list:\n\
         [{categories}]\n\n\
         Respond with ONLY lines in the format: ID|CATEGORY\n\
         No explanations, no extra text.\n\n"
    );
    for s in sessions {
        prompt.push_str(&format!("{}|{}\n", s.id, s.title));
    }
    prompt
}

/// Parse the LLM response into (session_id, category) pairs.
pub fn parse_classification_response(response: &str) -> Vec<(String, String)> {
    let valid: std::collections::HashSet<&str> = CATEGORIES.iter().copied().collect();
    response
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(2, '|').collect();
            if parts.len() == 2 {
                let id = parts[0].trim().to_string();
                let cat = parts[1].trim().to_string();
                // Only accept valid categories
                if valid.contains(cat.as_str()) && !id.is_empty() {
                    return Some((id, cat));
                }
            }
            None
        })
        .collect()
}

/// Persist categories back to the database.
pub async fn save_categories(pool: &Pool, categories: &[(String, String)]) -> Result<usize> {
    if categories.is_empty() {
        return Ok(0);
    }
    let cats = categories.to_vec();
    let conn = pool.get().await.context("pool")?;
    conn.interact(move |conn| {
        let mut count = 0usize;
        let mut stmt =
            conn.prepare("UPDATE sessions SET category = ?1 WHERE id = ?2 AND category IS NULL")?;
        for (id, cat) in &cats {
            count += stmt.execute(rusqlite::params![cat, id])?;
        }
        Ok::<_, rusqlite::Error>(count)
    })
    .await
    .map_err(interact_err)?
    .context("Failed to save categories")
}

/// Categorize sessions that the heuristic can handle (no LLM needed).
/// Returns count of sessions categorized.
pub async fn categorize_with_heuristic(pool: &Pool) -> Result<usize> {
    let uncategorized = fetch_uncategorized(pool, 500).await?;
    if uncategorized.is_empty() {
        return Ok(0);
    }

    let pairs: Vec<(String, String)> = uncategorized
        .iter()
        .map(|s| {
            let cat = super::data::classify_activity(&s.title).to_string();
            (s.id.clone(), cat)
        })
        .collect();

    save_categories(pool, &pairs).await
}
