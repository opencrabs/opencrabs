//! Following a paired crab to a new address without a new code.
//!
//! `opencrabs connect` opens a cloudflared quick tunnel, and a quick tunnel's
//! `*.trycloudflare.com` hostname is random: a reboot or a tunnel crash hands
//! the crab a new one. The pairing token survives on both sides (30 days on the
//! server, in `instances.json` here), so the credential is still good; only
//! the stored URL died. This module finds the crab again and moves the URL.
//!
//! Two ways in, one check:
//!
//! - [`recover`]: the probe of a paired crab failed at its stored URL, so its
//!   stored routes (what the crab itself announced on `/a2a/routes` the last
//!   time it answered) are tried in order.
//! - [`repoint_to`]: the owner pasted a new address into the badge's card.
//!
//! Either way a candidate is accepted only by [`judge`]: `/a2a/routes` at the
//! candidate must answer our token and announce the `host_id` we already hold.
//! Anything else leaves the store untouched.
//!
//! Why that check stands in for the owner's yes. Pairing refuses a known
//! `host_id` at a new URL ([`super::instance_store::HostIdCollision`]) and asks
//! the owner, because there the claim comes from a stranger holding nothing
//! but a fresh one-time code. Here the candidate must accept the credential
//! the owner's own pairing minted on that crab (a 401/403 refuses), and must
//! announce the id we keyed the row on. The candidate URL itself came either
//! from the crab's own authenticated announcement or from the owner's hands.
//! What this does not prove: an endpoint that ignores auth and already knows
//! the host id would pass. Host ids are announced only to read-scoped token
//! holders, so such an endpoint must have been told it by one. The token is
//! presented to every candidate, which is why the automatic path only ever
//! tries addresses the crab announced, never guesses.

use std::time::Duration;

use anyhow::Result;

use super::instance_store::{
    InstanceStore, KnownInstance, StoredRoute, is_derived_host_id, routes_from_announcement,
};

/// How long one candidate gets to answer. A LAN address from another network
/// is a dead address, and the sweep must move on to the next one.
pub const ASK_TIMEOUT: Duration = Duration::from_secs(4);

/// What one address said when asked who it is, with our token.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// 2xx: the id it announced (none from a keyless or older crab) and its
    /// routes.
    Announced {
        host_id: Option<String>,
        routes: Vec<StoredRoute>,
    },
    /// 401 or 403: the token is not good there.
    TokenRefused { status: u16 },
    /// Did not answer, answered another status, or sent something unreadable.
    Failed(String),
}

/// Why a candidate was not accepted. Every variant leaves the store as it was.
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    /// No credential for that host id in the store.
    NotPaired,
    Unreachable(String),
    TokenRefused {
        status: u16,
    },
    /// The address answered but announced no host id, so there is nothing to
    /// match: a keyless or older crab, or a row keyed by its URL.
    NoHostId,
    /// The address is another crab. Never accepted, whatever else it says.
    OtherHost {
        announced: String,
    },
    /// The row's token changed between the check and the write (a re-pairing
    /// landed meanwhile), so the check no longer speaks for it.
    Superseded,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::NotPaired => write!(f, "this crab is not paired here"),
            Refusal::Unreachable(why) => write!(f, "{why}"),
            Refusal::TokenRefused { status } => write!(
                f,
                "that address refused this crab's token (HTTP {status}); it is another crab or the pairing was revoked"
            ),
            Refusal::NoHostId => write!(
                f,
                "that address did not say which crab it is, so the move cannot be confirmed"
            ),
            Refusal::OtherHost { announced } => {
                write!(f, "that address is another crab (host id {announced})")
            }
            Refusal::Superseded => write!(
                f,
                "the pairing changed while checking; try again with the new one"
            ),
        }
    }
}

impl std::error::Error for Refusal {}

/// `GET /a2a/routes` at `url` with `token`. Never an error: every outcome is
/// an [`Answer`] for [`judge`] to read.
pub async fn ask(url: &str, token: &str) -> Answer {
    let base = url.trim().trim_end_matches('/');
    let resp = match crate::utils::http::shared()
        .get(format!("{base}/a2a/routes"))
        .timeout(ASK_TIMEOUT)
        .bearer_auth(token)
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => return Answer::Failed(format!("{base} did not answer: {e}")),
    };
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Answer::TokenRefused { status };
    }
    if !resp.status().is_success() {
        return Answer::Failed(format!("{base} answered HTTP {status}"));
    }
    match resp.json::<serde_json::Value>().await {
        Ok(body) => {
            let (host_id, routes) = routes_from_announcement(&body);
            Answer::Announced {
                host_id,
                routes: StoredRoute::from_pairs(routes),
            }
        }
        Err(e) => Answer::Failed(format!("{base} sent an unreadable announcement: {e}")),
    }
}

/// The whole rule, pure: accept only the crab we hold, answering our token.
/// `Ok` carries the routes it announced, to be stored with the move.
pub fn judge(known: &KnownInstance, answer: Answer) -> Result<Vec<StoredRoute>, Refusal> {
    match answer {
        Answer::Failed(why) => Err(Refusal::Unreachable(why)),
        Answer::TokenRefused { status } => Err(Refusal::TokenRefused { status }),
        // A row keyed by its URL never had an announced identity to match.
        _ if is_derived_host_id(&known.host_id) => Err(Refusal::NoHostId),
        Answer::Announced { host_id: None, .. } => Err(Refusal::NoHostId),
        Answer::Announced {
            host_id: Some(id),
            routes,
        } if id == known.host_id => Ok(routes),
        Answer::Announced {
            host_id: Some(announced),
            ..
        } => Err(Refusal::OtherHost { announced }),
    }
}

/// Move `host_id` to `candidate` if [`judge`] accepts it, and keep the routes
/// it announced. Returns the row as it now reads.
pub async fn repoint_to(
    store: &InstanceStore,
    host_id: &str,
    candidate: &str,
) -> Result<KnownInstance> {
    let candidate = candidate.trim().trim_end_matches('/');
    anyhow::ensure!(!candidate.is_empty(), "the new address is empty");
    anyhow::ensure!(
        candidate.starts_with("http://") || candidate.starts_with("https://"),
        "the new address must start with http:// or https://"
    );
    let known = store.find(host_id)?.ok_or(Refusal::NotPaired)?;
    let routes = match judge(&known, ask(candidate, &known.token).await) {
        Ok(routes) => routes,
        Err(refusal) => {
            if let Refusal::OtherHost { announced } = &refusal {
                tracing::warn!(
                    host_id,
                    candidate,
                    announced = announced.as_str(),
                    "re-point refused: the address is another crab"
                );
            }
            return Err(refusal.into());
        }
    };
    if !store.move_url(host_id, &known.token, candidate)? {
        return Err(Refusal::Superseded.into());
    }
    store.record_routes(host_id, &routes)?;
    tracing::info!(
        host_id,
        from = known.url.as_str(),
        to = candidate,
        "paired crab re-pointed"
    );
    store
        .find(host_id)?
        .ok_or_else(|| anyhow::Error::new(Refusal::NotPaired))
}

/// The stored URL stopped answering: try the crab's stored routes in order
/// and move to the first one [`judge`] accepts. `Ok(None)` when none did.
pub async fn recover(store: &InstanceStore, host_id: &str) -> Result<Option<KnownInstance>> {
    let Some(known) = store.find(host_id)? else {
        return Ok(None);
    };
    let current = known.url.trim().trim_end_matches('/').to_ascii_lowercase();
    for route in &known.routes {
        if route.url.trim().trim_end_matches('/').to_ascii_lowercase() == current {
            continue;
        }
        match repoint_to(store, host_id, &route.url).await {
            Ok(moved) => return Ok(Some(moved)),
            Err(e) => tracing::debug!(
                host_id,
                candidate = route.url.as_str(),
                "re-point candidate not taken: {e:#}"
            ),
        }
    }
    Ok(None)
}

/// The crab answered at its stored URL: keep what it announces now, so the
/// next address change has fresh routes to follow. Returns whether the stored
/// routes changed.
pub async fn refresh_routes(store: &InstanceStore, host_id: &str) -> Result<bool> {
    let Some(known) = store.find(host_id)? else {
        return Ok(false);
    };
    match judge(&known, ask(&known.url, &known.token).await) {
        Ok(routes) => store.record_routes(host_id, &routes),
        Err(_) => Ok(false),
    }
}

#[cfg(test)]
#[path = "instance_repoint_test.rs"]
mod tests;
