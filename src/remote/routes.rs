//! Route announcement: what addresses this host is reachable at.
//!
//! Server-side vocabulary shared with the A2A HTTP surface
//! (`src/a2a/routes_http.rs`) and, in Phase 2b, the client route store.
//! Two layers, deliberately split:
//!
//! - [`collect_interfaces`]: thin `getifaddrs(3)` FFI — the only unsafe here.
//! - [`select_advertised_routes`]: pure selection over an interface table —
//!   RFC1918 on physical NICs → `lan`, CGNAT on a tailscale utun → `tailnet`,
//!   a live quick-tunnel URL → `tunnel`. Pure means fake-table tests, no
//!   machine dependencies.
//!
//! Privacy invariant (enforced by the HTTP layer, stated here): routes are
//! announced ONLY to authenticated callers when an api_key is configured —
//! `/a2a/health` is public and the tunnel exposes it to the open internet,
//! and LAN topology is not for anonymous eyes.

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::path::Path;

use anyhow::{Context, Result};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};

/// One usable IPv4 address on one interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Iface {
    pub name: String,
    pub ipv4: Ipv4Addr,
}

/// What kind of reachability a route offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RouteKind {
    Lan,
    Tailnet,
    Tunnel,
}

/// One announced way to reach this host's A2A gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnouncedRoute {
    pub url: String,
    pub kind: RouteKind,
}

/// Enumerate up+running, non-loopback IPv4 interfaces via `getifaddrs(3)`.
/// Returns an empty vec on FFI failure (logged) — announcement is
/// best-effort and must never take the gateway down.
pub fn collect_interfaces() -> Vec<Iface> {
    let mut out: Vec<Iface> = Vec::new();
    unsafe {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut head) != 0 {
            tracing::warn!("getifaddrs failed: {}", std::io::Error::last_os_error());
            return out;
        }
        let mut cur = head;
        while !cur.is_null() {
            let ifa = &*cur;
            let usable = !ifa.ifa_addr.is_null()
                && (*ifa.ifa_addr).sa_family as i32 == libc::AF_INET
                && (ifa.ifa_flags & libc::IFF_LOOPBACK as u32) == 0
                && (ifa.ifa_flags & libc::IFF_UP as u32) != 0;
            if usable {
                let name = std::ffi::CStr::from_ptr(ifa.ifa_name)
                    .to_string_lossy()
                    .to_string();
                let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                let ipv4 = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                if !out.iter().any(|i| i.ipv4 == ipv4) {
                    out.push(Iface { name, ipv4 });
                }
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(head);
    }
    out
}

fn is_rfc1918(ip: &Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 10 || (a == 192 && b == 168) || (a == 172 && (16..=31).contains(&b))
}

/// Tailscale uses the CGNAT range 100.64.0.0/10 for tailnet addresses.
fn is_tailnet_ip(ip: &Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 100 && (64..=127).contains(&b)
}

/// Physical NIC name patterns (macOS en*, Linux eth*/wlan*/wl*). Bridge,
/// docker, vmnet, utun and friends are NOT physical LAN presence.
fn is_physical_lan_name(name: &str) -> bool {
    name.starts_with("en")
        || name.starts_with("eth")
        || name.starts_with("wlan")
        || name.starts_with("wl")
}

fn is_tailnet_iface(name: &str) -> bool {
    name.starts_with("utun") || name.starts_with("tailscale")
}

/// Pure selection: which interfaces become announced routes. Dedup by URL;
/// `lan` before `tailnet` before `tunnel` (fastest/most-local first).
pub fn select_advertised_routes(
    ifaces: &[Iface],
    port: u16,
    tunnel_url: Option<&str>,
) -> Vec<AnnouncedRoute> {
    let mut routes: Vec<AnnouncedRoute> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut push = |url: String, kind: RouteKind| {
        if seen.insert(url.clone()) {
            routes.push(AnnouncedRoute { url, kind });
        }
    };

    for i in ifaces {
        if is_physical_lan_name(&i.name) && is_rfc1918(&i.ipv4) {
            push(format!("http://{}:{}", i.ipv4, port), RouteKind::Lan);
        }
    }
    for i in ifaces {
        if is_tailnet_iface(&i.name) && is_tailnet_ip(&i.ipv4) {
            push(format!("http://{}:{}", i.ipv4, port), RouteKind::Tailnet);
        }
    }
    if let Some(u) = tunnel_url {
        push(u.trim_end_matches('/').to_string(), RouteKind::Tunnel);
    }
    routes
}

/// Stable per-machine identifier so clients can correlate sightings of the
/// same host across changing addresses. Created once under
/// `<state_dir>/host_id`; not a secret (it identifies, it does not
/// authenticate), but stored 0600 like the rest of state/remote.
pub fn host_id(state_dir: &Path) -> Result<String> {
    let path = state_dir.join("host_id");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    let id = format!("h-{}", URL_SAFE_NO_PAD.encode(bytes));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(&path, format!("{id}\n"))
        .with_context(|| format!("write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(name: &str, ip: &str) -> Iface {
        Iface {
            name: name.to_string(),
            ipv4: ip.parse().unwrap(),
        }
    }

    #[test]
    fn lan_routes_come_from_rfc1918_on_physical_nics() {
        let ifaces = vec![
            iface("en0", "192.168.1.5"),
            iface("en1", "10.0.0.9"),
            iface("eth0", "172.16.4.2"),
            iface("wlan0", "192.168.0.7"),
        ];
        let routes = select_advertised_routes(&ifaces, 18991, None);
        assert_eq!(routes.len(), 4);
        assert!(routes.iter().all(|r| r.kind == RouteKind::Lan));
        assert!(routes.iter().any(|r| r.url == "http://192.168.1.5:18991"));
        assert!(routes.iter().any(|r| r.url == "http://172.16.4.2:18991"));
    }

    #[test]
    fn non_rfc1918_public_and_link_local_are_skipped() {
        let ifaces = vec![
            iface("en0", "93.184.216.34"), // public
            iface("en1", "169.254.3.2"),   // link-local
            iface("en2", "172.32.0.1"),    // just outside 172.16/12
        ];
        assert!(select_advertised_routes(&ifaces, 1, None).is_empty());
    }

    #[test]
    fn bridge_docker_vm_interfaces_are_not_lan() {
        let ifaces = vec![
            iface("bridge0", "192.168.5.5"),
            iface("br-0123abcd", "172.17.0.1"),
            iface("docker0", "172.18.0.1"),
            iface("vmnet8", "192.168.137.1"),
        ];
        assert!(select_advertised_routes(&ifaces, 1, None).is_empty());
    }

    #[test]
    fn tailnet_routes_need_utun_and_cgnat_together() {
        let ifaces = vec![
            iface("utun4", "100.106.89.86"), // tailscale ✓
            iface("utun2", "10.0.0.3"),      // utun but not CGNAT → skipped
            iface("en5", "100.64.1.1"), // CGNAT but physical name → lan? no: 100.64 is not RFC1918 → skipped entirely
        ];
        let routes = select_advertised_routes(&ifaces, 18991, None);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].kind, RouteKind::Tailnet);
        assert_eq!(routes[0].url, "http://100.106.89.86:18991");
    }

    #[test]
    fn tunnel_url_is_announced_and_deduped() {
        let routes = select_advertised_routes(
            &[iface("en0", "192.168.1.5")],
            1,
            Some("https://some-words.trycloudflare.com/"),
        );
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[1].kind, RouteKind::Tunnel);
        assert_eq!(routes[1].url, "https://some-words.trycloudflare.com");
        // same URL twice → once
        let r2 = select_advertised_routes(&[], 1, Some("https://x.trycloudflare.com"));
        assert_eq!(r2.len(), 1);
    }

    #[test]
    fn ordering_is_local_first() {
        let routes = select_advertised_routes(
            &[iface("utun4", "100.64.0.1"), iface("en0", "192.168.1.2")],
            7,
            Some("https://t.trycloudflare.com"),
        );
        let kinds: Vec<_> = routes.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            vec![RouteKind::Lan, RouteKind::Tailnet, RouteKind::Tunnel]
        );
    }

    #[test]
    fn host_id_is_created_once_and_stable() {
        let dir = tempfile::tempdir().unwrap();
        let a = host_id(dir.path()).unwrap();
        let b = host_id(dir.path()).unwrap();
        assert_eq!(a, b, "host_id must be stable across calls");
        assert!(a.starts_with("h-") && a.len() > 10);
    }

    #[test]
    fn collect_interfaces_smoke() {
        // Real-machine smoke: must not panic; loopback must never appear.
        let ifaces = collect_interfaces();
        assert!(ifaces.iter().all(|i| !i.name.starts_with("lo")));
        assert!(ifaces.iter().all(|i| !i.ipv4.is_loopback()));
    }
}
