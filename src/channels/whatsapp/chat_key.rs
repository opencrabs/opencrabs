//! One canonical `channel_chat_id` per WhatsApp conversation (#2928).
//!
//! `channel_messages` is keyed by `channel_chat_id`, and the context query that
//! feeds a turn reads it back. WhatsApp addresses the same human by two ids, a
//! phone number (PN, `@s.whatsapp.net`) and an opaque privacy id (LID, `@lid`),
//! and a stanza carries whichever one the server felt like using. Writing the
//! transport address straight into the row therefore gives one contact two
//! threads: the live capture in #2928 shows a PN thread holding 39 rows, all
//! outbound, and a LID thread holding the inbound turns. A reply recorded in
//! the PN thread is invisible to the turn that answers the next inbound
//! message, so the agent answers a question it can no longer see.
//!
//! The rule is the one `identity.rs` already applies to senders: prefer the PN
//! when the pair has one, fall back to the address the stanza used. Groups are
//! left alone, they are not identity pairs.
//!
//! The twin usually arrives with the stanza (`sender_alt`, see `identity.rs`),
//! but not always, so the persisted `wa_lid_pn_mapping` is mirrored in memory
//! as well: seeded once at connect and taught by every stanza that carries both
//! ids. That way a LID-only stanza still lands in the PN thread the contact
//! started.

use wacore_binary::jid::Jid;

use super::state::WhatsAppState;

/// The canonical `channel_chat_id` for a chat, given the PN twin if it is
/// known. A group keeps its own address: a group JID is not a LID/PN pair.
pub(crate) fn canonical_chat_id(chat: &Jid, is_group: bool, pn_user: Option<&str>) -> String {
    if is_group {
        return chat.to_string();
    }
    match pn_user.map(str::trim).filter(|p| !p.is_empty()) {
        Some(pn) => format!("{pn}@s.whatsapp.net"),
        None => chat.to_string(),
    }
}

/// The PN user part carried by the stanza itself, if any.
///
/// A PN-addressed chat is its own answer. A LID-addressed one is answered by
/// the alt identity the parser attached. `None` means the stanza did not say,
/// and the caller falls back to what the mapping store learned.
pub(crate) fn pn_from_stanza(chat: &Jid, alt_user: Option<&str>) -> Option<String> {
    if chat.server.is_pn_family() {
        return Some(chat.user.to_string()).filter(|p| !p.is_empty());
    }
    if chat.server.is_lid_family() {
        return alt_user
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_string);
    }
    None
}

impl WhatsAppState {
    /// Replace the mirror with what the Signal store reports at connect.
    pub(crate) async fn seed_lid_pn_pairs(&self, pairs: Vec<(String, String)>) {
        let mut map = self.lid_pn.lock().await;
        for (lid, pn) in pairs {
            if !lid.is_empty() && !pn.is_empty() {
                map.insert(lid, pn);
            }
        }
    }

    /// Record a learned pair so a later LID-only stanza resolves to the PN.
    pub(crate) async fn remember_lid_pn_pair(&self, lid: &str, pn: &str) {
        let lid = lid.trim();
        let pn = pn.trim();
        if lid.is_empty() || pn.is_empty() || lid == pn {
            return;
        }
        self.lid_pn
            .lock()
            .await
            .insert(lid.to_string(), pn.to_string());
    }

    /// The PN user part known for a LID, if any.
    pub(crate) async fn lid_pn_twin(&self, lid: &str) -> Option<String> {
        self.lid_pn.lock().await.get(lid.trim()).cloned()
    }

    /// The canonical `channel_chat_id` for a stanza's chat, learning the pair
    /// on the way when the stanza carries both ids.
    ///
    /// This is what every `channel_messages` row for WhatsApp is written under,
    /// inbound and outbound alike, so one conversation has one key.
    pub(crate) async fn canonical_chat_id(
        &self,
        chat: &Jid,
        is_group: bool,
        alt_user: Option<&str>,
    ) -> String {
        if is_group {
            return canonical_chat_id(chat, true, None);
        }
        let chat_user = chat.user.to_string();
        let alt = alt_user.map(str::trim).filter(|a| !a.is_empty());

        // Teach the mirror from whichever direction the stanza names both ids.
        match (chat.server.is_pn_family(), chat.server.is_lid_family()) {
            (true, _) => {
                if let Some(alt) = alt {
                    self.remember_lid_pn_pair(alt, &chat_user).await;
                }
            }
            (_, true) => {
                if let Some(alt) = alt {
                    self.remember_lid_pn_pair(&chat_user, alt).await;
                }
            }
            _ => {}
        }

        let pn = match pn_from_stanza(chat, alt) {
            Some(pn) => Some(pn),
            None => self.lid_pn_twin(&chat_user).await,
        };
        canonical_chat_id(chat, false, pn.as_deref())
    }
}
