//! What a re-point may and may not do, pinned against a real HTTP exchange.
//!
//! The fake crab below is a bare TCP listener that answers `/a2a/routes` with
//! a fixed announcement when the right bearer is presented and 401 otherwise,
//! which is the shape the gateway's `require_bearer` gives. Every store lives
//! in a temp dir: a test never writes the live home.

use super::*;
use crate::remote::instance_client::block_on;

const TOKEN: &str = "ocpair_paired";

fn store_in(dir: &tempfile::TempDir) -> InstanceStore {
    InstanceStore::new(dir.path().join("instances.json"))
}

fn lan_route(url: &str) -> StoredRoute {
    StoredRoute {
        kind: "lan".to_string(),
        url: url.to_string(),
    }
}

fn paired(url: &str, routes: Vec<StoredRoute>) -> KnownInstance {
    KnownInstance {
        host_id: "crab-i7".to_string(),
        label: "server i7".to_string(),
        url: url.to_string(),
        token: TOKEN.to_string(),
        added_at: 11,
        last_ok: 22,
        glyph: Some("S".to_string()),
        routes,
    }
}

/// A crab at a fresh local port that knows `token` and announces `host_id`
/// with `routes`. Returns its base URL.
async fn fake_crab(token: &'static str, host_id: &'static str, routes: Vec<StoredRoute>) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a local port");
    let addr = listener.local_addr().expect("local addr");
    let body = serde_json::json!({ "host_id": host_id, "routes": routes }).to_string();
    let expected = format!("authorization: bearer {}", token.to_ascii_lowercase());
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let body = body.clone();
            let expected = expected.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut chunk = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(chunk.get(..n).unwrap_or_default()),
                    }
                }
                let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
                let (status, body) = if head.contains(&expected) {
                    ("200 OK", body)
                } else {
                    ("401 Unauthorized", "{}".to_string())
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(reply.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

/// An address nothing listens on: the port was bound and released.
async fn dead_address() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a local port");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);
    format!("http://{addr}")
}

#[test]
fn judge_accepts_only_the_same_host_answering_the_token() {
    let known = paired("https://old.trycloudflare.com", Vec::new());
    let routes = vec![lan_route("http://192.168.1.7:8080")];
    assert_eq!(
        judge(
            &known,
            Answer::Announced {
                host_id: Some("crab-i7".to_string()),
                routes: routes.clone(),
            }
        ),
        Ok(routes.clone())
    );
    assert_eq!(
        judge(
            &known,
            Answer::Announced {
                host_id: Some("crab-other".to_string()),
                routes: routes.clone(),
            }
        ),
        Err(Refusal::OtherHost {
            announced: "crab-other".to_string()
        })
    );
    assert_eq!(
        judge(&known, Answer::TokenRefused { status: 401 }),
        Err(Refusal::TokenRefused { status: 401 })
    );
    assert_eq!(
        judge(
            &known,
            Answer::Announced {
                host_id: None,
                routes: routes.clone(),
            }
        ),
        Err(Refusal::NoHostId)
    );
    // A row keyed by its URL never had an announced identity; even an
    // announcement that spells that key does not confirm a move.
    let mut derived = known.clone();
    derived.host_id = "url:old.trycloudflare.com".to_string();
    assert_eq!(
        judge(
            &derived,
            Answer::Announced {
                host_id: Some("url:old.trycloudflare.com".to_string()),
                routes,
            }
        ),
        Err(Refusal::NoHostId)
    );
}

#[test]
fn a_repoint_to_the_same_host_rewrites_only_the_url() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store_in(&dir);
    block_on(async {
        let routes = vec![lan_route("http://192.168.1.7:8080")];
        let new_url = fake_crab(TOKEN, "crab-i7", routes.clone()).await;
        let before = paired("https://old.trycloudflare.com", routes);
        store.upsert(before.clone())?;

        let moved = repoint_to(&store, "crab-i7", &format!("{new_url}/")).await?;

        let mut expected = before;
        expected.url = new_url;
        assert_eq!(moved, expected, "only the url moved");
        assert_eq!(store.find("crab-i7")?, Some(expected));
        Ok(())
    })
    .expect("the move goes through");
}

#[test]
fn a_repoint_to_another_host_is_refused_and_changes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store_in(&dir);
    block_on(async {
        let impostor = fake_crab(TOKEN, "crab-other", Vec::new()).await;
        let before = paired("https://old.trycloudflare.com", Vec::new());
        store.upsert(before.clone())?;

        let err = repoint_to(&store, "crab-i7", &impostor)
            .await
            .expect_err("another host id is never accepted");
        assert!(matches!(
            err.downcast_ref::<Refusal>(),
            Some(Refusal::OtherHost { .. })
        ));
        assert_eq!(store.find("crab-i7")?, Some(before), "the row is untouched");
        Ok(())
    })
    .expect("the test ran");
}

#[test]
fn a_repoint_whose_token_is_refused_changes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store_in(&dir);
    block_on(async {
        // Announces the right id, but only to a token we do not hold.
        let stranger = fake_crab("ocpair_someone_else", "crab-i7", Vec::new()).await;
        let before = paired("https://old.trycloudflare.com", Vec::new());
        store.upsert(before.clone())?;

        let err = repoint_to(&store, "crab-i7", &stranger)
            .await
            .expect_err("a refused token is never accepted");
        assert!(matches!(
            err.downcast_ref::<Refusal>(),
            Some(Refusal::TokenRefused { status: 401 })
        ));
        assert_eq!(store.find("crab-i7")?, Some(before), "the row is untouched");
        Ok(())
    })
    .expect("the test ran");
}

#[test]
fn recover_skips_dead_and_foreign_routes_and_takes_the_first_good_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store_in(&dir);
    block_on(async {
        let gone = dead_address().await;
        let unreachable = dead_address().await;
        let foreign = fake_crab(TOKEN, "crab-other", Vec::new()).await;
        // The good route announces the new tunnel, which is how a client that
        // still reaches the crab on its LAN learns where the tunnel went.
        let tunnel = StoredRoute {
            kind: "tunnel".to_string(),
            url: "https://new.trycloudflare.com".to_string(),
        };
        let good = fake_crab(TOKEN, "crab-i7", vec![lan_route("x"), tunnel.clone()]).await;
        let stored_routes = vec![
            lan_route(&gone),
            lan_route(&unreachable),
            lan_route(&foreign),
            lan_route(&good),
        ];
        store.upsert(paired(&gone, stored_routes))?;

        let moved = recover(&store, "crab-i7")
            .await?
            .expect("the good route is found");
        assert_eq!(moved.url, good);
        assert_eq!(moved.token, TOKEN);
        assert_eq!(moved.routes, vec![lan_route("x"), tunnel]);
        Ok(())
    })
    .expect("the test ran");
}

#[test]
fn recover_with_no_good_route_leaves_the_row_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store_in(&dir);
    block_on(async {
        let gone = dead_address().await;
        let foreign = fake_crab(TOKEN, "crab-other", Vec::new()).await;
        let before = paired(&gone, vec![lan_route(&foreign)]);
        store.upsert(before.clone())?;

        assert_eq!(recover(&store, "crab-i7").await?, None);
        assert_eq!(store.find("crab-i7")?, Some(before));
        Ok(())
    })
    .expect("the test ran");
}
