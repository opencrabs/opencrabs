//! Tests for the instance strip's model (`crate::instances`).
//!
//! Pure data, no window: every rule the strip relies on (order, selection,
//! label cutting, health marks) is asserted here, which is why the model lives
//! in its own module instead of inside the view.

use crate::instances::*;
use opencrabs::remote::instance_store::{InstanceFile, KnownInstance};

fn known(id: &str, label: &str) -> KnownInstance {
    KnownInstance {
        host_id: id.to_string(),
        label: label.to_string(),
        url: format!("http://{id}:7878"),
        token: "ocpair_test".to_string(),
        added_at: 100,
        last_ok: 0,
    }
}

fn file(ids: &[(&str, &str)]) -> InstanceFile {
    InstanceFile {
        version: 1,
        instances: ids.iter().map(|(id, l)| known(id, l)).collect(),
    }
}

#[test]
fn from_file_keeps_order_and_starts_unknown() {
    let set = Instances::from_file(&file(&[("a:1", "alpha"), ("b:2", "beta")]));
    assert_eq!(set.len(), 2);
    assert_eq!(set.entries()[0].label(), "alpha");
    assert_eq!(set.entries()[1].label(), "beta");
    assert!(set.entries().iter().all(|e| e.health == Health::Unknown));
    assert_eq!(set.selected_host_id(), None);
}

#[test]
fn health_never_reorders() {
    let mut set = Instances::from_file(&file(&[("a:1", "alpha"), ("b:2", "beta")]));
    set.record_probe("b:2", Ok(12), 500);
    set.record_probe("a:1", Err(()), 500);
    assert_eq!(set.entries()[0].label(), "alpha");
    assert!(set.entries()[0].health.is_down());
    assert!(set.entries()[1].health.is_up());
}

#[test]
fn upsert_keeps_position_and_health() {
    let mut set = Instances::from_file(&file(&[("a:1", "alpha"), ("b:2", "beta")]));
    set.record_probe("a:1", Ok(3), 500);
    let mut renamed = known("a:1", "alpha nova");
    renamed.url = "http://10.0.0.5:7878".to_string();
    set.upsert(renamed);
    assert_eq!(set.len(), 2);
    assert_eq!(set.entries()[0].label(), "alpha nova");
    assert_eq!(set.entries()[0].url(), "http://10.0.0.5:7878");
    assert!(set.entries()[0].health.is_up());
}

#[test]
fn remove_clears_a_selection_that_pointed_at_it() {
    let mut set = Instances::from_file(&file(&[("a:1", "alpha"), ("b:2", "beta")]));
    assert!(set.select("b:2"));
    assert!(set.remove("b:2"));
    assert_eq!(set.selected_host_id(), None);
    assert!(!set.remove("b:2"));
    assert_eq!(set.len(), 1);
}

#[test]
fn probe_for_a_removed_instance_is_ignored() {
    let mut set = Instances::from_file(&file(&[("a:1", "alpha")]));
    set.remove("a:1");
    set.record_probe("a:1", Ok(5), 900);
    assert!(set.is_empty());
}

#[test]
fn select_step_wraps_both_ways() {
    let mut set = Instances::from_file(&file(&[("a:1", "alpha"), ("b:2", "beta")]));
    assert_eq!(set.select_step(1), Some("a:1"));
    assert_eq!(set.select_step(1), Some("b:2"));
    assert_eq!(set.select_step(1), Some("a:1"));
    assert_eq!(set.select_step(-1), Some("b:2"));
    let mut empty = Instances::default();
    assert_eq!(empty.select_step(1), None);
}

#[test]
fn select_refuses_an_unknown_id() {
    let mut set = Instances::from_file(&file(&[("a:1", "alpha")]));
    assert!(!set.select("zzz:9"));
    assert_eq!(set.selected_host_id(), None);
}

#[test]
fn strip_label_collapses_and_cuts_on_char_boundaries() {
    assert_eq!(strip_label("server i7"), "server i7");
    assert_eq!(strip_label("  server   i7  "), "server i7");
    assert_eq!(strip_label("portátil do norte"), "portátil …");
    // Multi-byte throughout: the cut must not split a code point.
    assert_eq!(strip_label("açãoçãoçãoção"), "açãoçãoç…");
    assert_eq!(strip_label(""), "");
}

#[test]
fn age_label_buckets() {
    assert_eq!(age_label(1_000, 0), "nunca");
    assert_eq!(age_label(1_000, 990), "agora");
    assert_eq!(age_label(1_000, 700), "5 min");
    assert_eq!(age_label(10_000, 1_000), "2 h");
    assert_eq!(age_label(200_000, 1_000), "2 d");
    // A clock that went backwards must not print a huge number.
    assert_eq!(age_label(500, 900), "agora");
}

#[test]
fn badge_title_names_the_state() {
    let mut set = Instances::from_file(&file(&[("a:1", "alpha")]));
    set.record_probe("a:1", Ok(7), 100);
    let title = badge_title(&set.entries()[0], 100);
    assert!(title.contains("alpha"));
    assert!(title.contains("http://a:1:7878"));
    assert!(title.contains("7 ms"));
    assert!(
        badge_title(
            &Instances::from_file(&file(&[("b:2", "b")])).entries()[0],
            5
        )
        .contains("por verificar")
    );
}
