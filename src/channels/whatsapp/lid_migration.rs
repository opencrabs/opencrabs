//! One-time reconciliation of WhatsApp sessions keyed by LID (#2178).
//!
//! WhatsApp addresses one person by two ids: a phone number (PN) and an opaque
//! privacy id (LID). #1533 fixed which of the pair a NEW message keys its
//! session on, but nothing reconciled the rows created before it, so an install
//! that ran WhatsApp across that change keeps two live sessions for one human:
//! the LID row stops receiving writes and its history sits unreachable while
//! the PN row starts fresh. The bot "forgets" a conversation that is still on
//! disk.
//!
//! The mapping between the two ids is authoritative and already persisted by
//! the Signal store (`wa_lid_pn_mapping`), so this needs no inference: for each
//! `(lid, pn)` pair, a live `[chat:wa-<lid>]` session is either renamed to the
//! PN key when that key is free, or folded into the existing PN session when it
//! is not.
//!
//! Idempotent by construction: once reconciled, no live session carries a LID
//! key from the mapping, so a second pass finds nothing to do.

use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};

use crate::db::Pool;

/// What one reconciliation pass changed. All zero is the common case on every
/// boot after the first, and it is what makes the pass safe to run on start.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LidMigrationReport {
    /// LID-keyed sessions whose key was rewritten to the PN key.
    pub(crate) renamed: usize,
    /// LID-keyed sessions folded into an existing PN session.
    pub(crate) merged: usize,
    /// Messages moved from a LID session into its PN twin.
    pub(crate) messages_moved: usize,
}

impl LidMigrationReport {
    /// True when the pass found nothing to reconcile.
    pub(crate) fn is_noop(&self) -> bool {
        self.renamed == 0 && self.merged == 0 && self.messages_moved == 0
    }
}

/// The stable channel key for a WhatsApp id, matching `session_resolve`'s shape.
fn chat_key(wa_id: &str) -> String {
    format!("[chat:wa-{wa_id}]")
}

/// The label `handle_message` generates for a session, kept in one place so the
/// rename can tell a generated title from one a human renamed (#121).
fn generated_title(wa_id: &str) -> String {
    format!("WhatsApp: {wa_id} {}", chat_key(wa_id))
}

/// Reconcile every live LID-keyed session named by `mappings` into its PN key.
///
/// `mappings` is `(lid, phone_number)` as the Signal store reports them. One
/// transaction for the whole pass: either every pair is reconciled or the
/// database is untouched, which matters because a half-applied pass would
/// leave a session whose messages moved but whose key did not.
pub(crate) async fn reconcile_lid_sessions(
    pool: &Pool,
    mappings: Vec<(String, String)>,
) -> Result<LidMigrationReport> {
    if mappings.is_empty() {
        return Ok(LidMigrationReport::default());
    }
    let conn = pool
        .get()
        .await
        .context("Failed to get a connection for LID reconciliation")?;
    conn.interact(move |conn| -> Result<LidMigrationReport> {
        let tx = conn.transaction()?;
        let mut report = LidMigrationReport::default();
        let now = chrono::Utc::now().timestamp();

        for (lid, pn) in mappings {
            let (lid, pn) = (lid.trim(), pn.trim());
            // A pair of one id is not a pair, and an empty side would key a
            // session to `wa-` and collide with every other empty one.
            if lid.is_empty() || pn.is_empty() || lid == pn {
                continue;
            }
            let lid_key = chat_key(lid);
            let pn_key = chat_key(pn);

            let lid_row: Option<(String, Option<String>, i64, f64)> = tx
                .query_row(
                    "SELECT id, title, token_count, total_cost FROM sessions \
                     WHERE channel_chat_key = ?1 AND archived_at IS NULL \
                     ORDER BY updated_at DESC LIMIT 1",
                    params![lid_key],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            let Some((lid_id, lid_title, lid_tokens, lid_cost)) = lid_row else {
                // The common case for the ~120 contacts that never produced a
                // LID-keyed session: nothing to reconcile.
                continue;
            };

            let pn_id: Option<String> = tx
                .query_row(
                    "SELECT id FROM sessions \
                     WHERE channel_chat_key = ?1 AND archived_at IS NULL \
                     ORDER BY updated_at DESC LIMIT 1",
                    params![pn_key],
                    |r| r.get(0),
                )
                .optional()?;

            match pn_id {
                // No PN session yet: the LID row becomes the PN row, history
                // intact. A title a human edited is left alone; only the
                // generated label is rewritten, so the visible name still
                // matches the key it now carries.
                None => {
                    let legacy = generated_title(lid);
                    let title = if lid_title.as_deref() == Some(legacy.as_str()) {
                        Some(generated_title(pn))
                    } else {
                        lid_title.clone()
                    };
                    tx.execute(
                        "UPDATE sessions SET channel_chat_key = ?1, title = ?2, updated_at = ?3 \
                         WHERE id = ?4",
                        params![pn_key, title, now, lid_id],
                    )?;
                    report.renamed += 1;
                }
                // Already the same row: nothing to do.
                Some(pn_id) if pn_id == lid_id => {}
                // Both live: move the transcript over, then retire the LID row.
                // `sequence` is per session, so the merged set is renumbered by
                // time afterwards; leaving both sequences in place would order
                // the older half of the conversation after the newer one.
                Some(pn_id) => {
                    let moved = tx.execute(
                        "UPDATE messages SET session_id = ?1 WHERE session_id = ?2",
                        params![pn_id, lid_id],
                    )?;
                    tx.execute(
                        "WITH ordered AS ( \
                             SELECT id, ROW_NUMBER() OVER (ORDER BY created_at, sequence, id) - 1 \
                                    AS new_seq \
                             FROM messages WHERE session_id = ?1 \
                         ) \
                         UPDATE messages \
                         SET sequence = (SELECT new_seq FROM ordered WHERE ordered.id = messages.id) \
                         WHERE session_id = ?1",
                        params![pn_id],
                    )?;
                    // The lifetime counters follow the transcript: `/usage`
                    // reads them, and dropping them would understate a
                    // session's real spend by everything the LID row held.
                    tx.execute(
                        "UPDATE sessions SET token_count = token_count + ?1, \
                                            total_cost = total_cost + ?2, updated_at = ?3 \
                         WHERE id = ?4",
                        params![lid_tokens, lid_cost, now, pn_id],
                    )?;
                    // Archived, never deleted: auxiliary rows keyed to this
                    // session (plan cards, background tasks) stay readable.
                    tx.execute(
                        "UPDATE sessions SET archived_at = ?1 WHERE id = ?2",
                        params![now, lid_id],
                    )?;
                    report.merged += 1;
                    report.messages_moved += moved;
                }
            }
        }

        tx.commit()?;
        Ok(report)
    })
    .await
    .map_err(crate::db::interact_err)?
}
