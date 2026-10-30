//! Tests for the remote pairing store: single-use codes, TTL, lockout,
//! hashed-at-rest tokens, revocation, permissions, sweep, corruption.

use crate::remote::pairing::{
    CODE_ALPHABET, CODE_LEN, MAX_FAIL_STREAK, PairingStore, TOKEN_PREFIX, VerifyError, sha_b64,
};
use crate::remote::scopes::{SCOPE_READ, SCOPE_SEND};
use std::os::unix::fs::PermissionsExt;

fn store() -> (tempfile::TempDir, PairingStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("remote").join("pairing.json");
    (dir, PairingStore::new(path))
}

fn full_scopes() -> Vec<String> {
    vec![SCOPE_READ.to_string(), SCOPE_SEND.to_string()]
}

fn read_raw(store: &PairingStore) -> serde_json::Value {
    let bytes = std::fs::read(store.path()).expect("store file exists");
    serde_json::from_slice(&bytes).expect("valid json")
}

fn write_raw(store: &PairingStore, raw: &serde_json::Value) {
    std::fs::write(store.path(), serde_json::to_vec_pretty(raw).unwrap()).unwrap();
}

#[test]
fn mint_returns_8_char_alphabet_codes_unique() {
    let (_d, st) = store();
    let mut seen = std::collections::HashSet::new();
    for _ in 0..50 {
        let c = st.mint_code(&full_scopes()).expect("mint");
        assert_eq!(c.len(), CODE_LEN);
        assert!(
            c.bytes().all(|b| CODE_ALPHABET.contains(&b)),
            "bad char in {c}"
        );
        assert!(seen.insert(c), "duplicate code minted");
    }
}

#[test]
fn mint_validates_scopes() {
    let (_d, st) = store();
    assert!(st.mint_code(&[]).is_err());
    assert!(st.mint_code(&["a2a:admin".to_string()]).is_err());
    assert!(st.mint_code(&[SCOPE_READ.to_string()]).is_ok());
}

#[test]
fn store_never_contains_plaintext_secret() {
    let (_d, st) = store();
    let code = st.mint_code(&full_scopes()).unwrap();
    let (token, _s) = st.verify_code(&code, "phone").expect("verify");
    let raw = std::fs::read_to_string(st.path()).unwrap();
    assert!(!raw.contains(&code), "plaintext code at rest");
    assert!(!raw.contains(&token), "plaintext token at rest");
    assert!(raw.contains(&sha_b64(&token)), "token hash missing");
}

#[test]
fn verify_success_issues_scoped_session() {
    let (_d, st) = store();
    let read_only = vec![SCOPE_READ.to_string()];
    let code = st.mint_code(&read_only).unwrap();
    let (token, session) = st.verify_code(&code, "monitor").expect("verify");
    assert!(token.starts_with(TOKEN_PREFIX));
    assert_eq!(session.scopes, read_only);
    assert_eq!(session.label, "monitor");
    assert_eq!(session.id.len(), 12);
    assert!(session.revoked_at.is_none());
    assert!(
        st.verify_token(&token).is_some(),
        "issued token must verify"
    );
}

#[test]
fn verify_is_single_use_and_trims() {
    let (_d, st) = store();
    let code = st.mint_code(&full_scopes()).unwrap();
    let padded = format!("  {code}  ");
    st.verify_code(&padded, "a")
        .expect("first verify (trimmed)");
    assert!(matches!(
        st.verify_code(&code, "b"),
        Err(VerifyError::NoMatch)
    ));
}

#[test]
fn expired_code_rejected() {
    let (_d, st) = store();
    let code = st.mint_code(&full_scopes()).unwrap();
    let mut raw = read_raw(&st);
    raw["codes"][0]["expires_at"] = serde_json::json!(1u64);
    write_raw(&st, &raw);
    assert!(matches!(
        st.verify_code(&code, "x"),
        Err(VerifyError::NoMatch)
    ));
}

#[test]
fn lockout_trips_after_max_fail_streak() {
    let (_d, st) = store();
    let code = st.mint_code(&full_scopes()).unwrap();
    for i in 0..(MAX_FAIL_STREAK - 1) {
        let wrong = format!("WRONG{i:04}");
        assert!(matches!(
            st.verify_code(&wrong, "x"),
            Err(VerifyError::NoMatch)
        ));
    }
    // the MAX_FAIL_STREAK-th failure trips the lock but still reads NoMatch
    assert!(matches!(
        st.verify_code("WRONGFINAL", "x"),
        Err(VerifyError::NoMatch)
    ));
    // the CORRECT code during lockout is refused as Locked, not consumed
    assert!(matches!(
        st.verify_code(&code, "x"),
        Err(VerifyError::Locked { .. })
    ));
}

#[test]
fn lockout_expires() {
    let (_d, st) = store();
    let code = st.mint_code(&full_scopes()).unwrap();
    for _ in 0..MAX_FAIL_STREAK {
        let _ = st.verify_code("NOPE1234", "x");
    }
    assert!(matches!(
        st.verify_code(&code, "x"),
        Err(VerifyError::Locked { .. })
    ));
    let mut raw = read_raw(&st);
    raw["locked_until"] = serde_json::json!(1u64);
    write_raw(&st, &raw);
    assert!(
        st.verify_code(&code, "x").is_ok(),
        "lock expiry must restore pairing"
    );
}

#[test]
fn token_checks() {
    let (_d, st) = store();
    let code = st.mint_code(&full_scopes()).unwrap();
    let (token, _) = st.verify_code(&code, "t").unwrap();
    assert!(st.verify_token(&token).is_some());
    assert!(
        st.verify_token(&format!("{token}X")).is_none(),
        "tampered token"
    );
    assert!(
        st.verify_token("sk-live-notours").is_none(),
        "foreign token"
    );
    assert!(st.verify_token("").is_none());
}

#[test]
fn expired_session_token_rejected() {
    let (_d, st) = store();
    let code = st.mint_code(&full_scopes()).unwrap();
    let (token, _) = st.verify_code(&code, "t").unwrap();
    let mut raw = read_raw(&st);
    raw["sessions"][0]["expires_at"] = serde_json::json!(1u64);
    write_raw(&st, &raw);
    assert!(st.verify_token(&token).is_none());
}

#[test]
fn revoke_by_id_and_label() {
    let (_d, st) = store();
    let c1 = st.mint_code(&full_scopes()).unwrap();
    let c2 = st.mint_code(&full_scopes()).unwrap();
    let (t1, s1) = st.verify_code(&c1, "phone").unwrap();
    let (t2, _s2) = st.verify_code(&c2, "laptop").unwrap();

    assert_eq!(st.revoke(&s1.id).unwrap(), 1);
    assert!(st.verify_token(&t1).is_none(), "revoked by id");
    assert!(st.verify_token(&t2).is_some(), "other session untouched");

    assert_eq!(st.revoke("laptop").unwrap(), 1);
    assert!(st.verify_token(&t2).is_none(), "revoked by label");
    assert_eq!(st.revoke("ghost").unwrap(), 0);
}

#[test]
fn list_sessions_is_safe_and_accurate() {
    let (_d, st) = store();
    let code = st.mint_code(&full_scopes()).unwrap();
    let (token, _) = st.verify_code(&code, "viewme").unwrap();
    let views = st.list_sessions().unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].label, "viewme");
    assert!(views[0].active);
    let raw = serde_json::to_string(&views).unwrap();
    assert!(!raw.contains(&token), "views must not leak tokens");
    st.revoke("viewme").unwrap();
    assert!(!st.list_sessions().unwrap()[0].active);
}

#[test]
fn state_file_is_owner_only() {
    let (_d, st) = store();
    st.mint_code(&full_scopes()).unwrap();
    let mode = std::fs::metadata(st.path()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn sweep_drops_expired_codes_on_next_write() {
    let (_d, st) = store();
    let _old = st.mint_code(&full_scopes()).unwrap();
    let mut raw = read_raw(&st);
    raw["codes"][0]["expires_at"] = serde_json::json!(1u64);
    write_raw(&st, &raw);
    st.mint_code(&full_scopes()).unwrap(); // load → sweep → save
    let raw2 = read_raw(&st);
    assert_eq!(
        raw2["codes"].as_array().unwrap().len(),
        1,
        "expired code not swept"
    );
}

#[test]
fn labels_are_sanitized() {
    let (_d, st) = store();
    let code = st.mint_code(&full_scopes()).unwrap();
    let (_t, s) = st.verify_code(&code, "  \u{7}evil\u{0}name  ").unwrap();
    assert_eq!(s.label, "evilname");
    let c2 = st.mint_code(&full_scopes()).unwrap();
    let (_t2, s2) = st.verify_code(&c2, "   ").unwrap();
    assert_eq!(s2.label, "unnamed");
}

#[test]
fn corrupt_store_is_an_error_not_a_wipe() {
    let (_d, st) = store();
    st.mint_code(&full_scopes()).unwrap();
    std::fs::write(st.path(), b"{ not json").unwrap();
    assert!(matches!(
        st.verify_code("ANYTHING", "x"),
        Err(VerifyError::Storage(_))
    ));
    assert!(st.mint_code(&full_scopes()).is_err());
    assert!(
        std::fs::read_to_string(st.path())
            .unwrap()
            .contains("not json"),
        "corrupt file must survive"
    );
}
