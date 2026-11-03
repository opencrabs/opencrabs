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
