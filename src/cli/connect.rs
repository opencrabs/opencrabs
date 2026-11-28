//! `opencrabs connect` — remote access to this machine's A2A gateway in
//! one command: managed cloudflared quick tunnel + one-time pairing code.
//!
//! The CLI never sees a session token: it mints a code, starts the tunnel,
//! and prints the exact curl the remote side runs against `POST /a2a/pair`.
//! Sub-actions (`--status`, `--stop`, `--list-sessions`, `--revoke`) manage
//! the tunnel and paired sessions without starting anything.

use anyhow::{Context, Result};

use crate::config::Config;
use crate::remote::pairing::{GUEST_TTL_SECS, PairingStore, TOKEN_TTL_SECS, parse_ttl};
use crate::remote::routes::{
    AnnouncedRoute, collect_interfaces, reachable_through_bind, select_advertised_routes,
};
use crate::remote::scopes::{SCOPE_MANAGE, SCOPE_READ, SCOPE_SEND};
use crate::remote::tunnel::{self, TunnelSpec};

/// Parse the `--scopes` flag: "read", "send", "manage" in any combination
/// (order-free). `manage` is elevated: it is never in the default, so a code
/// carries it only when the operator types it here. Without it a paired GUI
/// cannot create a session on the remote crab.
fn parse_scopes(raw: &str) -> Result<Vec<String>> {
    let mut scopes = Vec::new();
    for part in raw.split(',') {
        match part.trim().to_ascii_lowercase().as_str() {
            "read" | "a2a:read" => push_unique(&mut scopes, SCOPE_READ),
            "send" | "a2a:send" => push_unique(&mut scopes, SCOPE_SEND),
            "manage" | "a2a:manage" => push_unique(&mut scopes, SCOPE_MANAGE),
            other => anyhow::bail!("unknown scope {other:?} (expected: read, send, manage)"),
        }
    }
    if scopes.is_empty() {
        anyhow::bail!("--scopes needs at least one of: read, send, manage");
    }
    Ok(scopes)
}

fn push_unique(v: &mut Vec<String>, s: &str) {
    if !v.iter().any(|x| x == s) {
        v.push(s.to_string());
    }
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_connect(
    config: &Config,
    port: Option<u16>,
    scopes: &str,
    ttl: Option<&str>,
    label: &str,
    status: bool,
    stop: bool,
    list_sessions: bool,
    revoke: Option<&str>,
    no_tunnel: bool,
) -> Result<()> {
    let state_dir = tunnel::default_state_dir();
    let store = PairingStore::new(PairingStore::default_path());

    // ── management sub-actions (mutually independent, run in order) ──────
    let mut acted = false;

    if stop {
        acted = true;
        if tunnel::stop(&state_dir)? {
            println!("Tunnel stopped.");
        } else {
            println!("No tunnel was running.");
        }
    }

    if status {
        acted = true;
        match tunnel::status(&state_dir)? {
            Some(st) => {
                let age = crate::remote::now_secs().saturating_sub(st.started_at);
                println!("Tunnel:  {}", st.url);
                // The recorded host, not a literal: printing `127.0.0.1` here
                // claimed loopback for a gateway bound to a tailnet address,
                // which is how a 502 stayed unexplained (2026-11-21).
                println!(
                    "Target:  {}:{} (pid {}, up {}m{}s)",
                    st.origin_host,
                    st.port,
                    st.pid,
                    age / 60,
                    age % 60
                );
                println!("Binary:  {}", st.binary);
                println!("Log:     {}", state_dir.join("tunnel.log").display());
            }
            None => println!("No tunnel running. Start one with: opencrabs connect"),
        }
    }

    if list_sessions {
        acted = true;
        let sessions = store.list_sessions()?;
        if sessions.is_empty() {
            println!("No paired sessions.");
        } else {
            println!(
                "{:<14} {:<24} {:<22} {:<8}",
                "ID", "LABEL", "SCOPES", "ACTIVE"
            );
            for s in sessions {
                println!(
                    "{:<14} {:<24} {:<22} {:<8}",
                    s.id,
                    truncate(&s.label, 24),
                    s.scopes.join(" "),
                    if s.active { "yes" } else { "no" }
                );
            }
        }
    }

    if let Some(target) = revoke {
        acted = true;
        let n = store.revoke(target)?;
        if n > 0 {
            println!("Revoked {n} session(s) matching {target:?}.");
        } else {
            println!("No active session matches {target:?}.");
        }
    }

    if acted {
        return Ok(());
    }

    // ── main flow: mint code + start tunnel ──────────────────────────────
    // The same gate the window's share card applies: both doors into a
    // public tunnel go through `tunnel::open`, and this early call only makes
    // the refusal land before a code is minted.
    tunnel::gateway_gate(&config.a2a)?;
    let port = port.unwrap_or(config.a2a.port);
    let scope_list = parse_scopes(scopes)?;
    // Session lifetime for whatever this code redeems into. Absent means the
    // 30-day default the CLI has always issued; `--ttl 1h` is the guest case
    // (#2873). Resolved before the tunnel so a bad duration fails fast.
    let session_ttl = match ttl {
        Some(raw) => parse_ttl(raw)?,
        None => TOKEN_TTL_SECS,
    };

    // Direct addresses first: a lan or tailnet address the gateway is bound
    // to survives a restart, a quick tunnel URL does not. Pairing at a fixed
    // address is what saves the client from pasting a new URL every time
    // cloudflared comes back up with another hostname.
    let direct = direct_routes(&collect_interfaces(), &config.a2a.bind, port);

    if no_tunnel {
        if direct.is_empty() {
            anyhow::bail!(
                "--no-tunnel needs an address another machine can reach, and the gateway \
                 is bound to '{}'. Bind [a2a].bind to a LAN or tailnet address (or 0.0.0.0), \
                 or drop --no-tunnel to open a quick tunnel.",
                config.a2a.bind
            );
        }
        let code = store
            .mint_code_with_ttl(&scope_list, session_ttl)
            .context("mint pairing code")?;
        println!("Pair at a fixed address (no tunnel started):");
        print_direct(&direct);
        println!();
        print_code(&code, &scope_list, ttl, session_ttl);
        return Ok(());
    }

    // A running tunnel for THIS port is reused: `connect` means "make me
    // reachable and give me a code", not "start a second tunnel". A tunnel
    // bound to a different port is a conflict the caller must resolve, and
    // checking here makes it fail before a code is minted.
    let existing = tunnel::reusable(&state_dir, port)?;

    // Mint BEFORE the tunnel so the code's 5-minute TTL covers the
    // (up to 60 s) URL wait, not just the moment it's printed.
    let code = store
        .mint_code_with_ttl(&scope_list, session_ttl)
        .context("mint pairing code")?;

    let spec = TunnelSpec::on(port, &config.a2a.bind);
    if existing.is_none() {
        println!("Starting cloudflared quick tunnel → {} …", spec.origin());
    }
    // The one tunnel path, shared with the window's share card.
    let state = match tunnel::open(&config.a2a, &spec) {
        Ok(tunnel::Opened::Reused(st)) => {
            println!("Reusing running tunnel (pid {}): {}", st.pid, st.url);
            st
        }
        Ok(tunnel::Opened::Started(st)) => st,
        Err(e) => {
            eprintln!("Tunnel failed: {e:#}");
            eprintln!("Log: {}", state_dir.join("tunnel.log").display());
            eprintln!("The minted pairing code was NOT consumed; it expires in 5 minutes.");
            return Err(e);
        }
    };

    let url = state.url.trim_end_matches('/');
    println!();
    if !direct.is_empty() {
        println!("Pair at a fixed address when the other machine can reach it (preferred):");
        print_direct(&direct);
        println!();
    }
    println!("✔ Tunnel up (pid {})", state.pid);
    // The recorded host, like `--status`: the arrow says where cloudflared
    // dials, and a literal here named loopback for a tunnel pointed at a
    // tailnet address (2026-11-21).
    println!("  URL:   {url}  →  {}:{port}", state.origin_host);
    println!("  (a quick tunnel gets a new hostname every time it restarts)");
    println!();
    print_code(&code, &scope_list, ttl, session_ttl);
    println!();
    println!("From the remote machine, pair with:");
    println!(
        "  curl -s {url}/a2a/pair -H 'content-type: application/json' \\\n       -d '{{\"code\":\"{code}\",\"label\":\"{label}\"}}'"
    );
    println!();
    println!("The response carries {{\"token\":\"ocpair_…\"}}; call the gateway with:");
    println!(
        "  curl -s {url}/a2a/v1 -H 'authorization: Bearer <token>' \\\n       -H 'content-type: application/json' \\\n       -d '{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tasks/get\",\"params\":{{\"id\":\"<task-id>\"}}}}'"
    );
    println!();
    println!(
        "Manage:  opencrabs connect --status | --stop | --list-sessions | --revoke <id|label>"
    );
    println!("Log:     {}", state_dir.join("tunnel.log").display());
    Ok(())
}

/// The lan and tailnet routes a gateway bound to `bind` on `port` answers
/// at, most local first. Empty for a loopback bind.
fn direct_routes(
    ifaces: &[crate::remote::routes::Iface],
    bind: &str,
    port: u16,
) -> Vec<AnnouncedRoute> {
    select_advertised_routes(&reachable_through_bind(ifaces, bind), port, None)
}

fn print_direct(routes: &[AnnouncedRoute]) {
    for r in routes {
        let kind = serde_json::to_value(r.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        println!("  {:<8} {}", kind, r.url);
    }
}

fn print_code(code: &str, scope_list: &[String], ttl: Option<&str>, session_ttl: u64) {
    println!("Pairing code (single use, 5 min):  {code}");
    println!("Scopes granted: {}", scope_list.join(" "));
    match ttl {
        Some(_) => println!(
            "Token lifetime: {} (revocable with --revoke)",
            human_ttl(session_ttl)
        ),
        None => println!(
            "Token lifetime: {} (revocable with --revoke; --ttl {} for a guest)",
            human_ttl(session_ttl),
            human_ttl(GUEST_TTL_SECS)
        ),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

/// Print form for a resolved session lifetime: whole days, hours or minutes
/// when it divides evenly, seconds otherwise.
fn human_ttl(secs: u64) -> String {
    for (unit, size) in [("d", 86_400u64), ("h", 3_600), ("m", 60)] {
        if secs.is_multiple_of(size) {
            return format!("{}{unit}", secs / size);
        }
    }
    format!("{secs}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_scopes_accepts_short_and_full_names() {
        assert_eq!(parse_scopes("read").unwrap(), vec![SCOPE_READ]);
        assert_eq!(parse_scopes("send").unwrap(), vec![SCOPE_SEND]);
        let both = parse_scopes("read,send").unwrap();
        assert_eq!(both.len(), 2);
        assert!(both.contains(&SCOPE_READ.to_string()));
        assert!(both.contains(&SCOPE_SEND.to_string()));
        // order-free, dedup, whitespace, full names, case-insensitive
        assert_eq!(parse_scopes(" SEND , a2a:read ").unwrap().len(), 2);
        assert_eq!(parse_scopes("read,read").unwrap(), vec![SCOPE_READ]);
    }

    #[test]
    fn parse_scopes_names_every_mintable_scope() {
        // Every scope the pairing store accepts must be nameable here, or
        // an op gated on it is unreachable over a paired link.
        for scope in crate::remote::scopes::ALL_SCOPES {
            let short = scope.trim_start_matches("a2a:");
            assert_eq!(parse_scopes(short).unwrap(), vec![scope.to_string()]);
            assert_eq!(parse_scopes(scope).unwrap(), vec![scope.to_string()]);
        }
    }

    #[test]
    fn parse_scopes_default_stays_unelevated() {
        let default = parse_scopes("read,send").unwrap();
        for elevated in crate::remote::scopes::ELEVATED_SCOPES {
            assert!(!default.iter().any(|s| s == elevated));
        }
    }

    #[test]
    fn parse_scopes_rejects_unknown_and_empty() {
        assert!(parse_scopes("admin").is_err());
        assert!(parse_scopes("").is_err());
        assert!(parse_scopes("read,delete-everything").is_err());
    }

    #[test]
    fn direct_routes_are_empty_for_a_loopback_gateway() {
        use crate::remote::routes::Iface;
        let machine = vec![
            Iface {
                name: "eth0".into(),
                ipv4: std::net::Ipv4Addr::new(192, 168, 1, 185),
            },
            Iface {
                name: "tailscale0".into(),
                ipv4: std::net::Ipv4Addr::new(100, 118, 255, 80),
            },
        ];
        assert!(direct_routes(&machine, "127.0.0.1", 18799).is_empty());
        let tailnet: Vec<String> = direct_routes(&machine, "100.118.255.80", 18799)
            .into_iter()
            .map(|r| r.url)
            .collect();
        assert_eq!(tailnet, vec!["http://100.118.255.80:18799"]);
    }

    #[test]
    fn truncate_marks_cut_strings() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn human_ttl_prints_whole_units() {
        assert_eq!(human_ttl(TOKEN_TTL_SECS), "30d");
        assert_eq!(human_ttl(GUEST_TTL_SECS), "1h");
        assert_eq!(human_ttl(1_800), "30m");
        assert_eq!(human_ttl(900), "15m");
        assert_eq!(human_ttl(45), "45s");
    }
}
