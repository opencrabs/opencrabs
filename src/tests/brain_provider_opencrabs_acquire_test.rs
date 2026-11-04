use crate::brain::provider::opencrabs_acquire::ensure_install_at;
use crate::brain::provider::opencrabs_install::{INSTALL_PATH, InstallError};
use crate::brain::provider::opencrabs_store::{self, StoredInstall};
use crate::config::profile::with_home_override;

fn in_temp_home(f: impl FnOnce()) {
    let dir = tempfile::tempdir().expect("tempdir");
    let opencrabs = dir.path().join(".opencrabs");
    std::fs::create_dir_all(&opencrabs).expect("create .opencrabs");
    with_home_override(opencrabs, f);
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

#[test]
fn fresh_home_requests_a_token_and_stores_it() {
    in_temp_home(|| {
        runtime().block_on(async {
            let mut server = mockito::Server::new_async().await;
            let mock = server
                .mock("POST", INSTALL_PATH)
                .with_status(200)
                .with_body(r#"{"token":"tok-new-123","label":"install-abc"}"#)
                .expect(1)
                .create_async()
                .await;

            let install = ensure_install_at(&server.url())
                .await
                .expect("token issued");

            mock.assert_async().await;
            assert_eq!(install.token, "tok-new-123");
            assert_eq!(install.install_id.len(), 32);
            assert_eq!(opencrabs_store::load(), Some(install));
        });
    });
}

#[test]
fn stored_install_is_reused_without_calling_the_proxy() {
    in_temp_home(|| {
        let stored = StoredInstall {
            install_id: "abcd1234abcd1234abcd1234abcd1234".to_string(),
            token: "tok-kept-456".to_string(),
        };
        opencrabs_store::save(&stored).expect("save");
        runtime().block_on(async {
            let mut server = mockito::Server::new_async().await;
            let mock = server
                .mock("POST", INSTALL_PATH)
                .expect(0)
                .create_async()
                .await;

            let install = ensure_install_at(&server.url())
                .await
                .expect("stored install");

            mock.assert_async().await;
            assert_eq!(install, stored);
        });
    });
}

#[test]
fn refused_issuance_stores_nothing() {
    in_temp_home(|| {
        runtime().block_on(async {
            let mut server = mockito::Server::new_async().await;
            server
                .mock("POST", INSTALL_PATH)
                .with_status(403)
                .expect(1)
                .create_async()
                .await;

            let err = ensure_install_at(&server.url())
                .await
                .expect_err("issuance is off");

            assert_eq!(err, InstallError::IssuanceOff);
            assert_eq!(opencrabs_store::load(), None);
        });
    });
}

/// The issue's second loss path: a `409` used to mint a new identity.
#[test]
fn a_409_reissues_for_the_same_id_instead_of_minting() {
    in_temp_home(|| {
        let id = "abcd1234abcd1234abcd1234abcd1234";
        opencrabs_store::write_install_id(id).expect("id file");
        runtime().block_on(async {
            let mut server = mockito::Server::new_async().await;
            let first = server
                .mock("POST", INSTALL_PATH)
                .match_body(mockito::Matcher::Json(serde_json::json!({
                    "install_id": id
                })))
                .with_status(409)
                .expect(1)
                .create_async()
                .await;
            let reissue = server
                .mock("POST", INSTALL_PATH)
                .match_body(mockito::Matcher::Json(serde_json::json!({
                    "install_id": id,
                    "reissue": true
                })))
                .with_status(200)
                .with_body(r#"{"token":"tok-reissued","label":"install-abc"}"#)
                .expect(1)
                .create_async()
                .await;

            let install = ensure_install_at(&server.url())
                .await
                .expect("the lost token is reissued");

            first.assert_async().await;
            reissue.assert_async().await;
            assert_eq!(
                install.install_id, id,
                "a 409 is a lost token for a known identity, not a new one"
            );
            assert_eq!(install.token, "tok-reissued");
            assert_eq!(opencrabs_store::load(), Some(install));
        });
    });
}

/// The last resort stays, but only after the reissue is refused, and it mints
/// a different identity rather than retrying the dead one.
#[test]
fn a_new_id_is_minted_only_when_the_reissue_is_refused() {
    in_temp_home(|| {
        let id = "abcd1234abcd1234abcd1234abcd1234";
        opencrabs_store::write_install_id(id).expect("id file");
        runtime().block_on(async {
            let mut server = mockito::Server::new_async().await;
            // The proxy will not serve this identity and this machine cannot
            // recover its token, so both requests are refused.
            let refused = server
                .mock("POST", INSTALL_PATH)
                .match_body(mockito::Matcher::Json(serde_json::json!({
                    "install_id": id
                })))
                .with_status(409)
                .expect(1)
                .create_async()
                .await;
            let refused_reissue = server
                .mock("POST", INSTALL_PATH)
                .match_body(mockito::Matcher::Json(serde_json::json!({
                    "install_id": id,
                    "reissue": true
                })))
                .with_status(409)
                .expect(1)
                .create_async()
                .await;
            let issued = server
                .mock("POST", INSTALL_PATH)
                .with_status(200)
                .with_body(r#"{"token":"tok-after-reissue","label":"install-new"}"#)
                .expect(1)
                .create_async()
                .await;

            let install = ensure_install_at(&server.url())
                .await
                .expect("a new identity keeps the user working");

            refused.assert_async().await;
            refused_reissue.assert_async().await;
            issued.assert_async().await;
            assert_ne!(install.install_id, id, "the old identity is dead");
            assert_eq!(install.install_id.len(), 32);
            assert_eq!(opencrabs_store::load(), Some(install.clone()));
            assert_eq!(
                opencrabs_store::load_install_id(),
                Some(install.install_id),
                "the replacement identity is persisted too"
            );
        });
    });
}

/// The first loss path: a failed token write used to take the ID with it.
#[test]
fn the_id_is_on_disk_before_the_first_request() {
    in_temp_home(|| {
        let path = opencrabs_store::install_id_path();
        let seen: std::sync::Arc<std::sync::Mutex<Option<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let at_request = std::sync::Arc::clone(&seen);
        runtime().block_on(async {
            let mut server = mockito::Server::new_async().await;
            let mock = server
                .mock("POST", INSTALL_PATH)
                .with_body_from_request(move |_req| {
                    // Read the file while the request is in flight: whatever is
                    // here is what was on disk before the proxy was asked.
                    *at_request.lock().expect("lock") = std::fs::read_to_string(&path).ok();
                    br#"{"token":"tok-disk-first","label":"install-abc"}"#.to_vec()
                })
                .expect(1)
                .create_async()
                .await;

            let install = ensure_install_at(&server.url())
                .await
                .expect("token issued");
            mock.assert_async().await;

            let on_disk = seen.lock().expect("lock").clone();
            assert_eq!(
                on_disk.as_deref().map(str::trim),
                Some(install.install_id.as_str()),
                "the identity must reach disk before the first request"
            );
        });
    });
}

#[test]
fn proxy_server_error_is_named_not_hidden() {
    in_temp_home(|| {
        runtime().block_on(async {
            let mut server = mockito::Server::new_async().await;
            server
                .mock("POST", INSTALL_PATH)
                .with_status(502)
                .expect(1)
                .create_async()
                .await;

            let err = ensure_install_at(&server.url())
                .await
                .expect_err("proxy failed");

            assert_eq!(err, InstallError::ProxyFailed);
        });
    });
}

#[test]
fn unreachable_proxy_is_named() {
    in_temp_home(|| {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("addr").port()
        };
        runtime().block_on(async {
            let err = ensure_install_at(&format!("http://127.0.0.1:{port}"))
                .await
                .expect_err("nothing listens there");

            assert_eq!(err, InstallError::Unreachable);
            assert_eq!(opencrabs_store::load(), None);
        });
    });
}

/// The first loss path seen from the acquire side: a hand-edited `keys.toml`
/// no longer parses, so the token is unreadable and no token can be written
/// either. The identity is in its own file, so the request goes out with the
/// id already held instead of minting a new one, which is how one user used to
/// become two. The token is lost, and that is reported rather than hidden.
#[test]
fn a_corrupt_keys_file_keeps_the_identity_and_reports_storage_failed() {
    in_temp_home(|| {
        let id = "abcd1234abcd1234abcd1234abcd1234";
        opencrabs_store::write_install_id(id).expect("id file");
        std::fs::write(
            crate::config::keys_path(),
            "providers.opencrabs = [unclosed\n",
        )
        .expect("corrupt keys.toml");
        runtime().block_on(async {
            let mut server = mockito::Server::new_async().await;
            let mock = server
                .mock("POST", INSTALL_PATH)
                .match_body(mockito::Matcher::Json(serde_json::json!({
                    "install_id": id
                })))
                .with_status(200)
                .with_body(r#"{"token":"tok-after-corrupt","label":"install-abc"}"#)
                .expect(1)
                .create_async()
                .await;

            let err = ensure_install_at(&server.url())
                .await
                .expect_err("a corrupt keys.toml cannot take a token");

            // The one request carried the surviving id, so nothing was minted.
            mock.assert_async().await;
            assert_eq!(err, InstallError::StorageFailed);
            assert_eq!(
                opencrabs_store::load_install_id(),
                Some(id.to_string()),
                "a corrupt keys.toml must cost the token, never the identity"
            );
        });
    });
}

/// The worst combination: the token is gone, the proxy will not reissue, and
/// `keys.toml` cannot accept a token. Minting a replacement here would put a
/// second identity in the proxy's books for a token that can never be stored,
/// and the next launch would mint a third. The identity already on disk wins
/// (#2198).
#[test]
fn a_corrupt_keys_file_never_mints_a_replacement_identity() {
    in_temp_home(|| {
        let id = "abcd1234abcd1234abcd1234abcd1234";
        opencrabs_store::write_install_id(id).expect("id file");
        std::fs::write(
            crate::config::keys_path(),
            "providers.opencrabs = [unclosed\n",
        )
        .expect("corrupt keys.toml");
        runtime().block_on(async {
            let mut server = mockito::Server::new_async().await;
            let plain = server
                .mock("POST", INSTALL_PATH)
                .match_body(mockito::Matcher::Json(serde_json::json!({
                    "install_id": id
                })))
                .with_status(409)
                .expect(1)
                .create_async()
                .await;
            let reissue = server
                .mock("POST", INSTALL_PATH)
                .match_body(mockito::Matcher::Json(serde_json::json!({
                    "install_id": id,
                    "reissue": true
                })))
                .with_status(409)
                .expect(1)
                .create_async()
                .await;

            let err = ensure_install_at(&server.url())
                .await
                .expect_err("no token can be stored");

            // Exactly two requests, both for the original id: no third request
            // means no replacement identity was minted.
            plain.assert_async().await;
            reissue.assert_async().await;
            assert_eq!(err, InstallError::StorageFailed);
            assert_eq!(
                opencrabs_store::load_install_id(),
                Some(id.to_string()),
                "the surviving identity is kept instead of minting one that cannot hold a token"
            );
        });
    });
}

/// The second loss path: the token write fails. The identity was already on
/// disk before the request, so the next acquire asks with the same id.
#[test]
fn a_failed_token_save_keeps_the_identity_for_the_next_attempt() {
    in_temp_home(|| {
        let id = "abcd1234abcd1234abcd1234abcd1234";
        opencrabs_store::write_install_id(id).expect("id file");
        // A directory where keys.toml belongs makes every token write fail,
        // which is what a read-only or full home looks like to `save`.
        let keys = crate::config::keys_path();
        std::fs::create_dir_all(&keys).expect("block keys.toml");
        runtime().block_on(async {
            let mut server = mockito::Server::new_async().await;
            let first = server
                .mock("POST", INSTALL_PATH)
                .match_body(mockito::Matcher::Json(serde_json::json!({
                    "install_id": id
                })))
                .with_status(200)
                .with_body(r#"{"token":"tok-lost","label":"install-abc"}"#)
                .expect(1)
                .create_async()
                .await;

            let err = ensure_install_at(&server.url())
                .await
                .expect_err("the token cannot be stored");
            first.assert_async().await;
            assert_eq!(err, InstallError::StorageFailed);
            assert_eq!(
                opencrabs_store::load_install_id(),
                Some(id.to_string()),
                "the identity survives the token write failing"
            );

            // Next launch with keys.toml writable again: same id, no mint.
            std::fs::remove_dir_all(&keys).expect("unblock keys.toml");
            let second = server
                .mock("POST", INSTALL_PATH)
                .match_body(mockito::Matcher::Json(serde_json::json!({
                    "install_id": id
                })))
                .with_status(200)
                .with_body(r#"{"token":"tok-recovered","label":"install-abc"}"#)
                .expect(1)
                .create_async()
                .await;

            let install = ensure_install_at(&server.url())
                .await
                .expect("the next attempt succeeds");

            second.assert_async().await;
            assert_eq!(
                install.install_id, id,
                "the retry presents the same identity, not a new one"
            );
            assert_eq!(install.token, "tok-recovered");
        });
    });
}
