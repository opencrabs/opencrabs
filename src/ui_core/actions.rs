//! Shared Seeker action definitions.
//!
//! The catalog owns presentation and identity. Each frontend still executes
//! the picked `PaletteAction` through its own state and focus rules.

use super::palette::{PaletteAction, PaletteItem, PaletteKind};
use super::split::Layout;

/// One action row, independent of a terminal or window implementation.
pub struct ActionSpec {
    pub title: &'static str,
    pub subtitle: &'static str,
    pub keywords: &'static str,
    pub kind: PaletteKind,
    pub action: PaletteAction,
}

impl ActionSpec {
    pub fn item(&self) -> PaletteItem {
        PaletteItem::new(self.title, self.subtitle, self.kind, self.action.clone())
            .with_keywords(self.keywords)
    }
}

/// Every standalone action offered by both Seeker frontends.
pub const ACTIONS: &[ActionSpec] = &[
    ActionSpec {
        title: "create topic",
        subtitle: "pick a conversation, name the topic, done",
        keywords: "create topic new topic make topic start a topic",
        kind: PaletteKind::NewTopic,
        action: PaletteAction::NewTopic,
    },
    ActionSpec {
        title: "favorite any session",
        subtitle: "pick a conversation, pin it to the top",
        keywords: "favorite favourite pin star session bookmark top",
        kind: PaletteKind::Favorite,
        action: PaletteAction::Favorite,
    },
    ActionSpec {
        title: "open sidebar",
        subtitle: "show the conversation rail",
        keywords: "open show reveal expand sidebar side bar sessions rail conversation list drawer",
        kind: PaletteKind::Action,
        action: PaletteAction::OpenSidebar,
    },
    ActionSpec {
        title: "close sidebar",
        subtitle: "hide the conversation rail",
        keywords: "close hide collapse dismiss sidebar side bar sessions rail conversation list drawer",
        kind: PaletteKind::Action,
        action: PaletteAction::CloseSidebar,
    },
    // Both surfaces own panes, so both print the split rows (#2712). The
    // words are the ones the key table and the docs already use: `|` lays the
    // panes out side by side, `_` stacks them. The subtitles say the shape out
    // loud so neither row can be read the wrong way round.
    ActionSpec {
        title: "split | horizontal",
        subtitle: "two panes side by side, pick the session for the new one",
        keywords: "split vertical side beside column side-by-side divider top bottom stack",
        kind: PaletteKind::Action,
        action: PaletteAction::BeginSplit(Layout::Side),
    },
    ActionSpec {
        title: "split _ vertical",
        subtitle: "two panes stacked, pick the session for the new one",
        keywords: "split horizontal stack stacked row above below one-over divider side beside",
        kind: PaletteKind::Action,
        action: PaletteAction::BeginSplit(Layout::Stack),
    },
];

pub fn items() -> Vec<PaletteItem> {
    ACTIONS.iter().map(ActionSpec::item).collect()
}

/// The one row that needs a window (#2713).
///
/// Grouping sessions into topics is a window modal, so a terminal row would
/// be a dead end. Both surfaces print `items()`; only the window adds this.
pub const GUI_ACTIONS: &[ActionSpec] = &[ActionSpec {
    title: "group topics",
    subtitle: "attach sessions as topics under the picked one",
    keywords: "group topics attach sessions parent nest under existing",
    kind: PaletteKind::Action,
    action: PaletteAction::GroupTopics,
}];

/// The window-only rows, built the same way as the shared ones.
pub fn gui_items() -> Vec<PaletteItem> {
    GUI_ACTIONS.iter().map(ActionSpec::item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_core::palette::PaletteState;

    fn row(action: PaletteAction) -> PaletteItem {
        items()
            .into_iter()
            .find(|item| item.action == action)
            .expect("catalog action")
    }

    /// Both surfaces own panes, so both print the split rows; only the group
    /// modal is window-only (#2712, #2713).
    #[test]
    fn the_split_rows_print_everywhere_and_the_group_row_prints_in_the_window() {
        let gui = gui_items();
        let shared = items();
        assert_eq!(gui.len(), 1, "group topics is the only window-only row");

        let group = gui
            .iter()
            .find(|it| it.action == PaletteAction::GroupTopics)
            .expect("the group topics row prints in the window");
        assert_eq!(group.title, "group topics");
        assert_eq!(
            group.subtitle,
            "attach sessions as topics under the picked one"
        );

        for (layout, title, shape) in [
            (Layout::Side, "split | horizontal", "side by side"),
            (Layout::Stack, "split _ vertical", "stacked"),
        ] {
            let row = shared
                .iter()
                .find(|it| it.action == PaletteAction::BeginSplit(layout))
                .unwrap_or_else(|| panic!("{title} prints in both surfaces"));
            assert_eq!(row.title, title);
            assert!(
                row.subtitle.contains(shape),
                "{title} names the shape the panes take, got {:?}",
                row.subtitle
            );
        }

        assert!(
            !shared
                .iter()
                .any(|it| it.action == PaletteAction::GroupTopics),
            "the terminal has no group modal, so it gets no row"
        );
    }

    /// The rows answer to the word the user types, so `split` and `group`
    /// find them without a keyword crutch (#2712, #2713).
    #[test]
    fn the_split_and_group_rows_answer_to_their_own_words() {
        for query in ["split", "split vertical", "split horizontal"] {
            let mut state = PaletteState::new(items());
            for c in query.chars() {
                state.type_char(c);
            }
            let top = state.selected().map(|r| r.item.action);
            assert!(
                matches!(top, Some(PaletteAction::BeginSplit(_))),
                "{query} finds a split row, got {top:?}"
            );
        }

        let mut state = PaletteState::new(gui_items());
        for c in "group topics".chars() {
            state.type_char(c);
        }
        assert_eq!(
            state.selected().map(|r| r.item.action),
            Some(PaletteAction::GroupTopics),
            "group topics finds its row"
        );
    }

    #[test]
    fn sidebar_actions_keep_the_existing_picker_definitions() {
        let topic = row(PaletteAction::NewTopic);
        assert_eq!(topic.title, "create topic");
        assert_eq!(topic.subtitle, "pick a conversation, name the topic, done");
        assert_eq!(topic.kind, PaletteKind::NewTopic);
        assert_eq!(
            topic.keywords,
            "create topic new topic make topic start a topic"
        );

        let favorite = row(PaletteAction::Favorite);
        assert_eq!(favorite.title, "favorite any session");
        assert_eq!(favorite.subtitle, "pick a conversation, pin it to the top");
        assert_eq!(favorite.kind, PaletteKind::Favorite);
        assert_eq!(
            favorite.keywords,
            "favorite favourite pin star session bookmark top"
        );
    }

    #[test]
    fn sidebar_actions_are_explicit_and_findable() {
        let open = row(PaletteAction::OpenSidebar);
        assert_eq!(open.title, "open sidebar");
        assert_eq!(open.subtitle, "show the conversation rail");
        assert_eq!(open.kind, PaletteKind::Action);
        let close = row(PaletteAction::CloseSidebar);
        assert_eq!(close.title, "close sidebar");
        assert_eq!(close.subtitle, "hide the conversation rail");
        assert_eq!(close.kind, PaletteKind::Action);

        for (query, expected) in [
            ("open sidebar", PaletteAction::OpenSidebar),
            ("show sidebar", PaletteAction::OpenSidebar),
            ("close sidebar", PaletteAction::CloseSidebar),
            ("hide sidebar", PaletteAction::CloseSidebar),
        ] {
            let mut state = PaletteState::new(items());
            state.insert_text(query);
            assert_eq!(
                state.selected().map(|row| row.item.action),
                Some(expected),
                "{query}"
            );
        }
    }

    #[test]
    fn sidebar_actions_frontends_consume_the_same_catalog() {
        for source in [
            include_str!("../gui/main.rs"),
            include_str!("../tui/app/state.rs"),
        ] {
            assert!(source.contains("actions::items()"));
            assert!(!source.contains("new_topic_item()"));
            assert!(!source.contains("favorite_item()"));
        }
    }
}
