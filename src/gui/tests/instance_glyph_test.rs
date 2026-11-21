//! The badge's mark: the owner's choice, the fallback, and the old store.
//!
//! Three rules are pinned here, and they are the ones a later change will want
//! to undo:
//!
//! 1. A store written before the field existed still loads. It is the same
//!    file the window reads at startup, so a missing `glyph` that failed to
//!    parse would take the whole strip down on upgrade.
//! 2. An empty mark means "derive it", never "draw nothing". A 32 px circle
//!    with no letter in it is a hole the user cannot tell from a broken crab.
//! 3. Two characters is the cap, and the cap is applied on the way in. The
//!    badge's text size is fixed, so a third character is not a longer mark,
//!    it is an overflow.

use crate::instances::*;
use opencrabs::remote::instance_store::{InstanceFile, KnownInstance};

fn known(id: &str, label: &str, glyph: Option<&str>) -> KnownInstance {
    KnownInstance {
        host_id: id.to_string(),
        label: label.to_string(),
        url: format!("http://{id}:7878"),
        token: "ocpair_test".to_string(),
        added_at: 100,
        last_ok: 0,
        glyph: glyph.map(str::to_string),
    }
}

fn entry(glyph: Option<&str>) -> InstanceEntry {
    InstanceEntry {
        known: known("h1", "lenovo-g780", glyph),
        health: Health::Unknown,
    }
}

#[test]
fn a_store_written_before_the_field_existed_still_loads() {
    // Exactly the bytes on disk today: no `glyph` key at all.
    let json = br#"{
        "version": 1,
        "instances": [{
            "host_id": "h1",
            "label": "lenovo",
            "url": "http://100.125.20.50:18790",
            "token": "ocpair_abc",
            "added_at": 1700000000,
            "last_ok": 1700000100
        }]
    }"#;
    let file: InstanceFile = serde_json::from_slice(json).expect("an old store parses");
    assert_eq!(file.instances.len(), 1);
    assert_eq!(file.instances[0].glyph, None, "absent means derive it");

    let set = Instances::from_file(&file);
    assert_eq!(set.entries()[0].glyph(), "L", "the initial still applies");
}

#[test]
fn a_chosen_mark_beats_the_initial() {
    assert_eq!(entry(None).glyph(), "L");
    assert_eq!(entry(Some("9")).glyph(), "9");
    // Case is the owner's: a lowercase mark is a choice, not a typo.
    assert_eq!(entry(Some("i7")).glyph(), "i7");
}

#[test]
fn a_blank_mark_falls_back_instead_of_drawing_nothing() {
    for blank in ["", "   ", "\t\n"] {
        assert_eq!(entry(Some(blank)).glyph(), "L", "for {blank:?}");
    }
}

#[test]
fn the_mark_is_capped_at_two_characters() {
    assert_eq!(normalise_glyph("lenovo"), Some("le".to_string()));
    assert_eq!(normalise_glyph(" L "), Some("L".to_string()));
    // A pasted word with spaces inside keeps no spaces: the badge is a circle.
    assert_eq!(normalise_glyph("a b c"), Some("ab".to_string()));
    assert_eq!(normalise_glyph("  "), None);
    assert_eq!(
        normalise_glyph("🇵🇹"),
        None,
        "no alphanumeric to cap cleanly"
    );
}

#[test]
fn a_mark_round_trips_through_the_store_file() {
    let dir = std::env::temp_dir().join(format!("opencrabs-glyph-store-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let store = opencrabs::remote::instance_store::InstanceStore::new(dir.join("instances.json"));

    store.upsert(known("h1", "lenovo", Some("L7"))).unwrap();
    let back = store.find("h1").unwrap().expect("the record is there");
    assert_eq!(back.glyph.as_deref(), Some("L7"));

    // Clearing it writes no key at all, so the file stays as it was before the
    // owner ever set one.
    let mut cleared = back;
    cleared.glyph = None;
    store.upsert(cleared).unwrap();
    let bytes = std::fs::read_to_string(dir.join("instances.json")).expect("read back");
    assert!(
        !bytes.contains("glyph"),
        "a cleared mark must not leave a key: {bytes}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
