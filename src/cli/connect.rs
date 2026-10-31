//! `opencrabs connect` — remote access to this machine's A2A gateway in
//! one command: managed cloudflared quick tunnel + one-time pairing code.
//!
//! The CLI never sees a session token: it mints a code, starts the tunnel,
//! and prints the exact curl the remote side runs against `POST /a2a/pair`.
//! Sub-actions (`--status`, `--stop`, `--list-sessions`, `--revoke`) manage
//! the tunnel and paired sessions without starting anything.

use anyhow::{Context, Result};

use crate::config::Config;
use crate::remote::pairing::PairingStore;
use crate::remote::scopes::{SCOPE_READ, SCOPE_SEND};
use crate::remote::tunnel::{self, TunnelSpec};

/// Parse the `--scopes` flag: "read", "send", "read,send" (order-free).
fn parse_scopes(raw: &str) -> Result<Vec<String>> {
    let mut scopes = Vec::new();
    for part in raw.split(',') {
        match part.trim().to_ascii_lowercase().as_str() {
            "read" | "a2a:read" => push_unique(&mut scopes, SCOPE_READ),
            "send" | "a2a:send" => push_unique(&mut scopes, SCOPE_SEND),
            other => anyhow::bail!("unknown scope {other:?} (expected: read, send)"),
        }
    }
    if scopes.is_empty() {
        anyhow::bail!("--scopes needs at least one of: read, send");
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
    label: &str,
    status: bool,
    stop: bool,
    list_sessions: bool,
    revoke: Option<&str>,
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
                println!(
                    "Target:  127.0.0.1:{} (pid {}, up {}m{}s)",
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
    if !config.a2a.enabled {
        anyhow::bail!(
            "the A2A gateway is disabled ([a2a].enabled = false); a tunnel to a closed door helps nobody. Enable it first."
        );
    }
    if config.a2a.api_key.as_deref().unwrap_or("").is_empty() {
        anyhow::bail!(
            "refusing to open a tunnel to a KEYLESS gateway: set [a2a].api_key first. \
             Without it, anyone who learns the tunnel URL gets the agent's full tool \
             surface (the #1473 open-gate posture), and route announcement stays suppressed."
        );
    }
    let port = port.unwrap_or(config.a2a.port);
    let scope_list = parse_scopes(scopes)?;

    // A running tunnel for THIS port is reused — `connect` means "make me
    // reachable and give me a code", not "start a second tunnel". A tunnel
    // bound to a different port is a conflict the caller must resolve.
    let existing = tunnel::status(&state_dir)?;
    if let Some(st) = &existing
        && st.port != port
    {
        anyhow::bail!(
            "a tunnel for port {} is already running ({}); use --port {}, or --stop first",
            st.port,
            st.url,
            st.port
        );
    }

    // Mint BEFORE the tunnel so the code's 5-minute TTL covers the
    // (up to 60 s) URL wait, not just the moment it's printed.
    let code = store.mint_code(&scope_list).context("mint pairing code")?;

    let state = match existing {
        Some(st) => {
            println!("Reusing running tunnel (pid {}): {}", st.pid, st.url);
            st
        }
        None => {
            println!("Starting cloudflared quick tunnel → 127.0.0.1:{port} …");
            match tunnel::start(&TunnelSpec::new(port)) {
                Ok(st) => st,
                Err(e) => {
                    eprintln!("Tunnel failed: {e:#}");
                    eprintln!("Log: {}", state_dir.join("tunnel.log").display());
                    eprintln!("The minted pairing code was NOT consumed; it expires in 5 minutes.");
                    return Err(e);
                }
            }
        }
    };

    let url = state.url.trim_end_matches('/');
    println!();
    println!("✔ Tunnel up (pid {})", state.pid);
    println!("  URL:   {url}  →  127.0.0.1:{port}");
    println!();
    println!("Pairing code (single use, 5 min):  {code}");
    println!("Scopes granted: {}", scope_list.join(" "));
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

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
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
    fn parse_scopes_rejects_unknown_and_empty() {
        assert!(parse_scopes("admin").is_err());
        assert!(parse_scopes("").is_err());
        assert!(parse_scopes("read,delete-everything").is_err());
    }

    #[test]
    fn truncate_marks_cut_strings() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
    }
}
