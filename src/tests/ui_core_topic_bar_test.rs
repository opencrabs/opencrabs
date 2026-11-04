//! The topic bar's shape rules (#2480): `All` first, the session never a chip,
//! the icon above the name on a rail and beside it on a bar, and the open entry
//! marked on the edge it sits on.

use crate::ui_core::topic_bar::{
    BarEntry, ChipShape, EntryMark, bar_entries, chip_shape, chips_hug, entry_mark,
};
use uuid::Uuid;

fn ids(n: u128) -> Vec<Uuid> {
    (1..=n).map(Uuid::from_u128).collect()
}

#[test]
fn all_always_leads_and_the_root_is_never_a_chip() {
    let root = Uuid::from_u128(100);
    let topics = ids(3);
    let entries = bar_entries(root, &topics);
    assert_eq!(entries.first(), Some(&BarEntry::All));
    assert_eq!(
        entries[1..],
        [
            BarEntry::Topic(topics[0]),
            BarEntry::Topic(topics[1]),
            BarEntry::Topic(topics[2])
        ]
    );
    // The reported bug: the root session was the first chip, so the
    // conversation's own name sat in the row of its topics.
    assert!(
        !entries.contains(&BarEntry::Topic(root)),
        "the root session must not be an entry"
    );
}

#[test]
fn a_root_that_leaks_into_the_topic_list_is_filtered_out() {
    // A caller that hands in the whole family (parent included) must not
    // resurrect the old shape.
    let root = Uuid::from_u128(7);
    let mut topics = ids(2);
    topics.push(root);
    let entries = bar_entries(root, &topics);
    assert_eq!(
        entries,
        vec![
            BarEntry::All,
            BarEntry::Topic(topics[0]),
            BarEntry::Topic(topics[1])
        ]
    );
}

#[test]
fn all_opens_the_root_and_a_topic_opens_itself() {
    let root = Uuid::from_u128(100);
    let topic = Uuid::from_u128(5);
    assert_eq!(BarEntry::All.session(root), root);
    assert_eq!(BarEntry::Topic(topic).session(root), topic);
}

#[test]
fn all_is_current_while_the_root_is_open_and_a_topic_while_it_is() {
    let root = Uuid::from_u128(100);
    let topic = Uuid::from_u128(5);
    // Reading the parent conversation: the way back to it is the current entry.
    assert!(BarEntry::All.is_open(root, root));
    assert!(!BarEntry::All.is_open(root, topic));
    assert!(BarEntry::Topic(topic).is_open(root, topic));
    assert!(!BarEntry::Topic(topic).is_open(root, root));
    assert!(!BarEntry::Topic(Uuid::from_u128(6)).is_open(root, topic));
}

#[test]
fn a_conversation_with_no_topics_still_has_all() {
    let root = Uuid::from_u128(100);
    assert_eq!(bar_entries(root, &[]), vec![BarEntry::All]);
}

#[test]
fn a_rail_stacks_the_icon_above_the_name_and_a_bar_leads_with_it() {
    assert_eq!(chip_shape(true), ChipShape::IconAbove);
    assert_eq!(chip_shape(false), ChipShape::IconLeading);
}

#[test]
fn a_rail_marks_the_open_chip_on_its_leading_edge() {
    assert_eq!(entry_mark(true), EntryMark::LeadingBar);
    assert_eq!(entry_mark(false), EntryMark::Underline);
}

#[test]
fn chips_hug_their_content_on_every_edge() {
    // The rail shipped without this and one chip filled the whole column.
    assert!(chips_hug(true));
    assert!(chips_hug(false));
}
