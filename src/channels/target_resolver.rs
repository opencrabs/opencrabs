//! The `oc://` target URL scheme and its single resolver (#148).
//!
//! EVERY tool that targets a session or a channel accepts these URLs and
//! funnels through [`resolve_target`] here — this module is the ONLY place
//! that may parse or construct `oc://` strings (the opaqueness wall, D11;
//! enforced by `src/tests/oc_target_opaqueness_test.rs`). Consumers hand
//! the raw string over and get a [`ResolvedTarget`] back, or an error that
//! names the candidates — never a guess.
//!
//! Grammar:
//!
//! ```text
//! oc://session/<uuid | 8-char-prefix>
//! oc://telegram/<chat_id>[/<thread>]     thread=1 => General topic (session-scoping
//!                                        key, #1220) — resolves, delivers threadless;
//!                                        thread=0 is a PARSE ERROR, never coerced.
//! oc://discord/<channel_id>
//! oc://slack/<channel_id>
//! oc://whatsapp/<phone | jid>
//! oc://here                              ambient — resolves against the origin stamp
//! ```
//!
//! The legacy `telegram:<chat>[:<thread>]` deliver_to grammar stays accepted
//! and distinct (no `://`), so existing cron job rows keep working untouched.
//! Non-numeric segments are percent-encoded per RFC 3986 (D11); numeric ids
//! stay bare digits.

use anyhow::{Result, anyhow, bail};
use uuid::Uuid;

use super::GENERAL_TOPIC_ID;
use crate::brain::tools::OriginTarget;

/// Where a resolved target's delivery should go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetDestination {
    /// Deliver into the session that owns the URL — the delivery surface
    /// re-derives its current binding from the session id at send time.
    Session(Uuid),
    /// Deliver to this concrete channel target (chat id + optional topic).
    /// Cron bakes this form at create time; the owning session id rides
    /// alongside for liveness revalidation (fork #17).
    Channel {
        channel: &'static str,
        chat_id: String,
        thread: Option<i32>,
        session: Option<Uuid>,
    },
}

/// The result of resolving an `oc://` target URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTarget {
    /// The session the URL designates, when the resolver could pin one.
    pub session: Option<Uuid>,
    /// Concrete delivery destination.
    pub destination: TargetDestination,
}

impl ResolvedTarget {
    /// The legacy deliver_to string for this target (`telegram:<chat>[:<t>]`
    /// …) — the concrete form baked into cron job rows. General topics bake
    /// WITHOUT `:1` (#1319): the scoping key never becomes a wire address.
    pub fn deliver_to(&self) -> String {
        match &self.destination {
            TargetDestination::Session(u) => format!("session:{u}"),
            TargetDestination::Channel {
                channel,
                chat_id,
                thread,
                ..
            } => match thread {
                Some(t) if *t != GENERAL_TOPIC_ID => format!("{channel}:{chat_id}:{t}"),
                _ => format!("{channel}:{chat_id}"),
            },
        }
    }
}

/// Percent-encode a non-numeric URL path segment (D11): everything outside
/// RFC 3986 unreserved set becomes `%XX`. Numeric segments pass through
/// bare — callers must not feed digits here.
pub fn encode_segment(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for b in seg.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Decode a percent-encoded segment. Invalid escapes are an error — the
/// resolver never guesses at mangled input.
pub fn decode_segment(seg: &str) -> Result<String> {
    let bytes = seg.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .ok_or_else(|| anyhow!("invalid percent-encoding in '{seg}'"))?;
            let s = std::str::from_utf8(hex)
                .map_err(|_| anyhow!("invalid percent-encoding in '{seg}'"))?;
            let byte = u8::from_str_radix(s, 16)
                .map_err(|_| anyhow!("invalid percent-encoding in '{seg}'"))?;
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| anyhow!("segment '{seg}' is not valid UTF-8"))
}

/// Split an `oc://` URL into `(authority, decoded-segments)`. Pure syntax —
/// no channel semantics. This is the ONLY grammar parser for `oc://`.
fn parse_url(url: &str) -> Result<(&str, Vec<String>)> {
    let rest = url
        .strip_prefix("oc://")
        .ok_or_else(|| anyhow!("not an oc:// url: '{url}'"))?;
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a, p),
        None => (rest, ""),
    };
    if authority.is_empty() {
        bail!("empty authority in '{url}'");
    }
    let segments: Vec<String> = if path.is_empty() {
        Vec::new()
    } else {
        path.split('/')
            .map(decode_segment)
            .collect::<Result<Vec<_>>>()?
    };
    Ok((authority, segments))
}

/// The `here` token and its URL form — both resolve against the ambient
/// origin stamp. A cron turn (headless) has no origin and must refuse.
pub const HERE_TOKEN: &str = "here";

/// Is this string an `oc://` URL or the `here` token? Cheap syntax check so
/// consumers can decide whether to route through the resolver without
/// parsing the contents themselves (opaqueness wall — they still may not).
pub fn is_target_url(s: &str) -> bool {
    s.starts_with("oc://") || s == HERE_TOKEN
}

/// Extract session target identifier string from a target URL or legacy deliver_to string.
/// Returns the raw UUID or prefix string when the target designates a session (e.g. `oc://session/<id>` or `session:<id>`).
pub fn extract_session_target(target: &str) -> Option<&str> {
    if let Some(rest) = target.strip_prefix("oc://session/") {
        rest.split('/').next()
    } else {
        target.strip_prefix("session:")
    }
}

/// Does this target designate a session (e.g. `oc://session/<id>` or `session:<id>`)?
pub fn is_session_target(target: &str) -> bool {
    extract_session_target(target).is_some()
}

/// What the resolver needs from the world. Implemented by the wiring context
/// (ChannelManager + session lookup) so tests can drive resolution without
/// a live daemon.
#[async_trait::async_trait]
pub trait TargetResolution: Send + Sync {
    /// The session bound to `(channel, chat_id, thread)` — reverse
    /// ownership map. `None` when no session currently owns it.
    async fn session_for_channel(
        &self,
        channel: &str,
        chat_id: &str,
        thread: Option<i32>,
    ) -> Option<Uuid>;

    /// The channel target a session is currently bound to (forward map) —
    /// used to confirm a resolved session still owns a live channel.
    async fn binding_for_session(&self, session: Uuid) -> Option<OriginTarget>;

    /// The session bound to an `oc://telegram/<chat>` URL with NO thread:
    /// `Ok(None)` when the chat simply has no binding, `Err(list)` when the
    /// chat is a forum with MULTIPLE topic sessions — the caller must pick
    /// a topic; bare-chat resolution would guess (D10/D9 multi-topic rule).
    async fn telegram_chat_topics(&self, chat_id: i64) -> Result<Option<Vec<i32>>>;
}

/// Resolve an `oc://` URL (or the `here` token) to a concrete target.
///
/// Resolution order: `here`/ambient origin → session authority (exact uuid
/// fast-path, then case-insensitive 8-char prefix with the 0/1/many law) →
/// channel authority via the reverse ownership map. Errors name candidates
/// or the reason — resolution never guesses.
pub async fn resolve_target(
    url: &str,
    origin: Option<&OriginTarget>,
    world: &(impl TargetResolution + ?Sized),
    sessions: &[crate::db::models::Session],
) -> Result<ResolvedTarget> {
    // `here` — ambient origin only. No origin (cron, CLI, sub-agent) is a
    // clean refusal: a headless surface has no "here".
    if url == HERE_TOKEN {
        return match origin {
            None => bail!(
                "'here' has no current channel on this surface — pass an explicit oc:// target"
            ),
            Some(o) => Ok(ResolvedTarget {
                session: world
                    .session_for_channel(o.channel, &o.chat_id, o.thread)
                    .await,
                destination: TargetDestination::Channel {
                    channel: o.channel,
                    chat_id: o.chat_id.to_string(),
                    thread: o.thread,
                    session: world
                        .session_for_channel(o.channel, &o.chat_id, o.thread)
                        .await,
                },
            }),
        };
    }

    let (authority, segments) = parse_url(url)?;

    match authority {
        "session" => {
            let id = segments
                .first()
                .ok_or_else(|| anyhow!("oc://session needs an id: '{url}'"))?;
            // Exact-uuid fast path, then prefix (0/1/many, never guess).
            let session = if let Ok(u) = Uuid::parse_str(id) {
                sessions
                    .iter()
                    .find(|s| s.id == u)
                    .map(|s| s.id)
                    .ok_or_else(|| anyhow!("no session with id '{id}'"))?
            } else {
                // Prefix law lives in one place (#1340): reuse it verbatim so
                // the ambiguity rules cannot drift from the CLI resolvers.
                crate::cli::session_resolve::resolve_one_by_prefix(sessions, id)
                    .map_err(|e| anyhow!("{e}"))?
            };
            // #1961: a `session:<id>` delivery must land IN that session, or
            // fail loudly. Baking this to the session's bound channel/thread
            // made delivery resolve "whoever owns the thread at that instant":
            // when a re-spawn bound a new uuid to the same thread, the wake
            // surfaced in an unidentified session and the lane's duty was
            // silently refused. The queue form is the exact-address one; the
            // scheduler's `session` arm already delivers through it, and a
            // session that cannot receive fails there with a visible verdict.
            // (Re-pointing via `binding_for_session` is deliberately NOT used
            // to carry lane identity: the downstream guard refuses a wake
            // acting as a lane it is not bound to, and that guard is correct.)
            Ok(ResolvedTarget {
                session: Some(session),
                destination: TargetDestination::Session(session),
            })
        }
        "telegram" => {
            let chat_id = segments
                .first()
                .ok_or_else(|| anyhow!("oc://telegram needs a chat id: '{url}'"))?;
            let _chat: i64 = chat_id
                .parse()
                .map_err(|_| anyhow!("telegram chat id must be numeric: '{chat_id}'"))?;
            let thread: Option<i32> = match segments.get(1) {
                None => None,
                Some(t) => {
                    let t: i32 = t
                        .parse()
                        .map_err(|_| anyhow!("telegram thread must be numeric: '{t}'"))?;
                    if t == 0 {
                        // D10: `0` means nothing in the Telegram API or this
                        // codebase — refuse, never coerce to General/bare.
                        bail!("thread id 0 is not a valid Telegram topic (General is 1)");
                    }
                    Some(t)
                }
            };
            let session = world.session_for_channel("telegram", chat_id, thread).await;
            // Bare-chat URL on a forum chat: ambiguous by design (D9/D10).
            if thread.is_none() && session.is_none() {
                bail_on_ambiguous_forum(world, chat_id).await?;
            }
            Ok(ResolvedTarget {
                session,
                destination: TargetDestination::Channel {
                    channel: "telegram",
                    chat_id: chat_id.to_string(),
                    thread,
                    session,
                },
            })
        }
        "discord" | "slack" | "whatsapp" => {
            let chat_id = segments
                .first()
                .ok_or_else(|| anyhow!("oc://{authority} needs a channel id: '{url}'"))?;
            if chat_id.is_empty() {
                bail!("empty channel id in '{url}'");
            }
            // WhatsApp: accept a bare phone number as sugar for the JID.
            let id = if authority == "whatsapp" && !chat_id.contains('@') {
                let digits = chat_id.trim_start_matches('+');
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    bail!("whatsapp target must be a phone number or a JID: '{chat_id}'");
                }
                format!("{digits}@s.whatsapp.net")
            } else {
                chat_id.clone()
            };
            let session = world.session_for_channel(authority, &id, None).await;
            Ok(ResolvedTarget {
                session,
                destination: TargetDestination::Channel {
                    channel: authority_static(authority)?,
                    chat_id: id,
                    thread: None,
                    session,
                },
            })
        }
        other => {
            bail!("unknown oc:// authority '{other}' (session|telegram|discord|slack|whatsapp)")
        }
    }
}

/// D9/D10: a bare-chat `oc://telegram/<chat>` URL on a forum with more than
/// one topic session is ambiguous by design — refuse and name the topic
/// targets instead of guessing one. A non-numeric or single-topic chat falls
/// through to the caller's normal resolution path.
async fn bail_on_ambiguous_forum(
    world: &(impl TargetResolution + ?Sized),
    chat_id: &str,
) -> Result<()> {
    let Ok(cid) = chat_id.parse::<i64>() else {
        return Ok(());
    };
    let Some(topics) = world.telegram_chat_topics(cid).await? else {
        return Ok(());
    };
    if topics.len() <= 1 {
        return Ok(());
    }
    bail!(
        "chat {chat_id} is a forum with multiple topic sessions — \
         pick a topic: {}",
        topics
            .iter()
            .map(|t| format!("oc://telegram/{chat_id}/{t}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

fn authority_static(a: &str) -> Result<&'static str> {
    Ok(match a {
        "discord" => "discord",
        "slack" => "slack",
        "whatsapp" => "whatsapp",
        other => bail!("unknown authority '{other}'"),
    })
}
