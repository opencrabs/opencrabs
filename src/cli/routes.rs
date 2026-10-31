//! `opencrabs routes` — inspect and manage the client-side route table:
//! the per-host map of ways to reach remote A2A gateways (LAN, tailnet,
//! tunnel), raced and learned by `a2a_send`. Thin shell over
//! `crate::remote::{route_client, route_store}`.

use anyhow::Result;

use super::args::RoutesCommands;
use crate::remote::now_secs;
use crate::remote::route_client;
use crate::remote::route_store::{RouteSource, RouteStore};
use crate::remote::routes::RouteKind;

fn parse_kind(raw: &str) -> Result<RouteKind> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "lan" => Ok(RouteKind::Lan),
        "tailnet" => Ok(RouteKind::Tailnet),
        "tunnel" => Ok(RouteKind::Tunnel),
        other => anyhow::bail!("unknown route kind {other:?} (expected: lan, tailnet, tunnel)"),
    }
}

pub async fn cmd_routes(operation: RoutesCommands) -> Result<()> {
    let store = RouteStore::new(RouteStore::default_path());
    match operation {
        RoutesCommands::List => {
            let file = store.load()?;
            if file.hosts.is_empty() {
                println!(
                    "Route table is empty. Seed with: opencrabs routes add <host> <url>, or learn from a remote: opencrabs routes learn <url> --api-key <key>"
                );
                return Ok(());
            }
            let now = now_secs();
            for (id, host) in &file.hosts {
                println!(
                    "host {id}  \"{}\"  (seen {}s ago)",
                    host.label,
                    now.saturating_sub(host.seen_at)
                );
                println!(
                    "  URL                                         KIND     SOURCE     EWMA(ms)  FAILS STALE"
                );
                for r in crate::remote::route_store::order(&host.routes, now) {
                    println!(
                        "  {:<44} {:<9} {:<11} {:<10} {:<5} {}",
                        r.url,
                        kind_str(r.kind),
                        source_str(r.source),
                        if r.ewma_ms > 0.0 {
                            format!("{:.1}", r.ewma_ms)
                        } else {
                            "-".into()
                        },
                        r.failures,
                        if r.stale(now) { "yes" } else { "no" }
                    );
                }
                println!();
            }
        }
        RoutesCommands::Add { host_id, url, kind } => {
            let kind = parse_kind(&kind)?;
            store.upsert_configured(&host_id, &url, &url, kind, now_secs())?;
            println!(
                "Added configured route {url} ({}) for host {host_id}.",
                kind_str(kind)
            );
        }
        RoutesCommands::Race { host_id } => {
            let (winner, outcomes) = route_client::race_host(&store, &host_id).await?;
            for o in &outcomes {
                println!(
                    "  {} {:<44} {:.1}ms",
                    if o.ok { "✔" } else { "✘" },
                    o.url,
                    o.latency_ms
                );
            }
            match winner {
                Some(w) => println!("Winner: {w}"),
                None => println!("No healthy route for host {host_id}."),
            }
        }
        RoutesCommands::Learn { url, api_key } => {
            match route_client::harvest(&store, &url, api_key.as_deref()).await? {
                Some((host_id, n)) => {
                    println!("Learned {n} route(s) from {url} → host {host_id}.");
                    if let Some(host) = store.host(&host_id)? {
                        for r in &host.routes {
                            println!("  {} ({})", r.url, kind_str(r.kind));
                        }
                    }
                }
                None => println!(
                    "Nothing learned: {url} announced no routes (keyless remotes suppress announcement; is the api_key right?)."
                ),
            }
        }
        RoutesCommands::Clear { host_id } => {
            let n = store.clear(host_id.as_deref())?;
            match host_id {
                Some(id) => println!("Removed host {id} ({n} host(s))."),
                None => println!("Cleared route table ({n} host(s))."),
            }
        }
    }
    Ok(())
}

fn kind_str(k: RouteKind) -> &'static str {
    match k {
        RouteKind::Lan => "lan",
        RouteKind::Tailnet => "tailnet",
        RouteKind::Tunnel => "tunnel",
    }
}

fn source_str(s: RouteSource) -> &'static str {
    match s {
        RouteSource::Configured => "configured",
        RouteSource::Learned => "learned",
    }
}
