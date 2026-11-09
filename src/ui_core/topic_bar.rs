//! The shape of the chat's topic bar (#2480), the way Telegram's forum bar is
//! built: the conversation's name sits on top in its own card, then a compact
//! row of chips starts with `All` and continues with the topics.
//!
//! The owner's report was three defects in one row: the session itself was
//! drawn as a chip (`vec![root]` led the strip), one chip stretched across the
//! whole vertical rail, and the open one was marked only by a background, so
//! nothing said which conversation you were reading.
//!
//! Everything here is pure: which entries the bar holds, how a chip lays out
//! its icon against its name on each edge, and how the open entry is marked.
//! The GUI owns the drawing; the tests below own the rules.

use uuid::Uuid;

/// How a chip lays out its icon against its name.
///
/// A horizontal bar has room to the side, so the icon leads the name. A
/// vertical rail is narrow, so the icon sits above the name and both centre,
/// which is the shape Telegram's forum list uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipShape {
    /// Icon above the name, centred: the vertical rail.
    IconAbove,
    /// Icon before the name, one row: the top and bottom bars.
    IconLeading,
}

/// How the open entry is marked.
///
/// Telegram marks the open topic with an accent underline under a horizontal
/// bar and with an accent bar on the leading edge down a rail. The mark is
/// always 2px so switching topics never changes a chip's height and the row
/// does not jump.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryMark {
    /// A 2px accent rule under the chip: the top and bottom bars.
    Underline,
    /// A 2px accent bar on the leading edge: the vertical rail.
    LeadingBar,
}

/// One entry of the bar, in the order it is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarEntry {
    /// The whole conversation, every topic at once. Always first.
    All,
    /// One topic of the open conversation.
    Topic(Uuid),
}

impl BarEntry {
    /// The session this entry opens.
    ///
    /// `All` opens the root conversation, which is the session every topic
    /// hangs off. A chip that did not open anything would be a label pretending
    /// to be a control.
    pub fn session(self, root: Uuid) -> Uuid {
        match self {
            BarEntry::All => root,
            BarEntry::Topic(id) => id,
        }
    }

    /// Whether this entry is the conversation currently on screen.
    ///
    /// `All` is current while the root session is open, which is the state the
    /// bar starts in and the one the name card above describes. A topic entry
    /// is current when that topic is open.
    pub fn is_open(self, root: Uuid, open: Uuid) -> bool {
        match self {
            BarEntry::All => open == root,
            BarEntry::Topic(id) => id == open,
        }
    }
}

/// The entries a topic bar holds: `All` first, then the topics in list order.
///
/// The root session is never an entry. It used to be (`vec![root]` led the
/// strip), which put the conversation's own name in the row of its topics and
/// made the bar read as if the parent were a sibling. The name belongs to the
/// card above, and `All` is the way back to it.
///
/// A topic list that already contains the root is filtered rather than trusted,
/// so a caller that hands the family's ids in unsorted cannot resurrect the old
/// shape.
pub fn bar_entries(root: Uuid, topics: &[Uuid]) -> Vec<BarEntry> {
    let mut out = Vec::with_capacity(topics.len() + 1);
    out.push(BarEntry::All);
    out.extend(
        topics
            .iter()
            .copied()
            .filter(|topic| *topic != root)
            .map(BarEntry::Topic),
    );
    out
}

/// How a chip lays its icon and name out on this edge.
pub fn chip_shape(vertical: bool) -> ChipShape {
    if vertical {
        ChipShape::IconAbove
    } else {
        ChipShape::IconLeading
    }
}

/// How the open entry is marked on this edge.
pub fn entry_mark(vertical: bool) -> EntryMark {
    if vertical {
        EntryMark::LeadingBar
    } else {
        EntryMark::Underline
    }
}

/// Whether chips size themselves to their content instead of filling the bar.
///
/// Always true, on both edges. This function exists because the vertical rail
/// shipped without it: the strip was a fixed-width column and the single chip
/// inside it grew to the column's width, which is the wall of colour the owner
/// screenshotted. A chip is a label, so it hugs its label on every edge.
pub fn chips_hug(_vertical: bool) -> bool {
    true
}

/// How many topic chips the bar draws before it folds the rest into one pill
/// (#2762). A forum with twenty topics would otherwise push every chip off the
/// edge of a narrow rail.
pub const BAR_TOPICS_SHOWN: usize = 6;

/// One topic as the bar sees it: the session it opens, and whether something is
/// happening in it right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopicChip {
    pub id: Uuid,
    /// A turn is in flight in this topic. The window never learns "unread" for
    /// a session it is not drawing, so activity is the honest signal it has.
    pub activity: bool,
}

/// The pill that stands for the chips a narrow bar could not draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Overflow {
    /// How many topics are folded away.
    pub hidden: usize,
    /// How many of those are busy right now, so the pill says when something
    /// is happening inside the part of the family the reader cannot see.
    pub activity: usize,
}

/// What the bar draws: the entries in order, and the overflow pill when the
/// family is wider than the bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BarPlan {
    pub entries: Vec<BarEntry>,
    pub overflow: Option<Overflow>,
}

/// The entries a bar holds for a family of `chips`, capped at `limit` topics.
///
/// `open` is never folded away: a bar that hides the chip for the conversation
/// on screen stops saying where the reader is, which is the defect the mark was
/// added to fix. When the open topic falls past the cap it takes the last
/// visible slot and the topic it displaced joins the overflow.
///
/// `limit` is clamped to at least one, so a caller cannot ask for a bar with no
/// topics and no pill.
pub fn bar_plan(root: Uuid, open: Uuid, chips: &[TopicChip], limit: usize) -> BarPlan {
    let limit = limit.max(1);
    let topics: Vec<TopicChip> = chips
        .iter()
        .copied()
        .filter(|chip| chip.id != root)
        .collect();
    if topics.len() <= limit {
        let mut entries = Vec::with_capacity(topics.len() + 1);
        entries.push(BarEntry::All);
        entries.extend(topics.iter().map(|chip| BarEntry::Topic(chip.id)));
        return BarPlan {
            entries,
            overflow: None,
        };
    }
    let mut shown: Vec<TopicChip> = topics[..limit].to_vec();
    if let Some(pos) = topics.iter().position(|chip| chip.id == open)
        && pos >= limit
    {
        shown[limit - 1] = topics[pos];
    }
    let hidden: Vec<&TopicChip> = topics
        .iter()
        .filter(|chip| !shown.iter().any(|shown| shown.id == chip.id))
        .collect();
    let mut entries = Vec::with_capacity(shown.len() + 1);
    entries.push(BarEntry::All);
    entries.extend(shown.iter().map(|chip| BarEntry::Topic(chip.id)));
    BarPlan {
        entries,
        overflow: Some(Overflow {
            hidden: hidden.len(),
            activity: hidden.iter().filter(|chip| chip.activity).count(),
        }),
    }
}

/// The overflow pill's face: how much is folded away, and how much of it is
/// busy.
pub fn overflow_label(overflow: &Overflow) -> String {
    match overflow.activity {
        0 => format!("+{}", overflow.hidden),
        activity => format!("+{} \u{b7} {} live", overflow.hidden, activity),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BAR_TOPICS_SHOWN, BarEntry, BarPlan, Overflow, TopicChip, bar_entries, bar_plan,
        overflow_label,
    };
    use uuid::Uuid;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn chips(ids: &[u128], busy: &[u128]) -> Vec<TopicChip> {
        ids.iter()
            .map(|n| TopicChip {
                id: id(*n),
                activity: busy.contains(n),
            })
            .collect()
    }

    /// A family that fits keeps the shape the bar has always had: `All` first,
    /// then every topic, no pill.
    #[test]
    fn a_short_family_has_no_overflow() {
        let root = id(1);
        let plan: BarPlan = bar_plan(root, root, &chips(&[2, 3], &[]), BAR_TOPICS_SHOWN);
        assert_eq!(
            plan.entries,
            vec![
                BarEntry::All,
                BarEntry::Topic(id(2)),
                BarEntry::Topic(id(3))
            ]
        );
        assert_eq!(plan.overflow, None);
        assert_eq!(plan.entries, bar_entries(root, &[id(2), id(3)]));
    }

    /// The long family folds its tail, and the pill counts what it hid.
    #[test]
    fn a_long_family_folds_the_tail_and_counts_it() {
        let root = id(1);
        let plan = bar_plan(root, root, &chips(&[2, 3, 4, 5, 6, 7, 8, 9], &[8]), 6);
        assert_eq!(plan.entries.len(), 7);
        assert_eq!(
            plan.overflow,
            Some(Overflow {
                hidden: 2,
                activity: 1
            })
        );
        assert_eq!(overflow_label(&plan.overflow.unwrap()), "+2 \u{b7} 1 live");
    }

    /// The open topic is never the one folded away, even when it sits past the
    /// cap.
    #[test]
    fn the_open_topic_keeps_its_chip() {
        let root = id(1);
        let open = id(9);
        let plan = bar_plan(root, open, &chips(&[2, 3, 4, 5, 6, 7, 8, 9], &[]), 6);
        assert!(plan.entries.contains(&BarEntry::Topic(open)));
        assert_eq!(plan.entries.len(), 7);
        assert_eq!(plan.overflow.map(|o| o.hidden), Some(2));
        assert!(!plan.entries.contains(&BarEntry::Topic(id(7))));
    }

    /// No activity anywhere means no number after the count: `+2 live` would
    /// promise a running turn that is not there.
    #[test]
    fn a_quiet_overflow_says_only_how_many() {
        assert_eq!(
            overflow_label(&Overflow {
                hidden: 4,
                activity: 0
            }),
            "+4"
        );
    }

    /// A caller cannot ask for a bar with nothing on it.
    #[test]
    fn a_zero_limit_still_draws_one_topic() {
        let root = id(1);
        let plan = bar_plan(root, root, &chips(&[2, 3], &[]), 0);
        assert_eq!(plan.entries, vec![BarEntry::All, BarEntry::Topic(id(2))]);
        assert_eq!(plan.overflow.map(|o| o.hidden), Some(1));
    }
}
