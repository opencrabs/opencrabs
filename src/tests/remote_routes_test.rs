use crate::remote::routes::*;

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
