//! Shared Seeker action definitions.
//!
//! The catalog owns presentation and identity. Each frontend still executes
//! the picked `PaletteAction` through its own state and focus rules.

use super::palette::{PaletteAction, PaletteItem, PaletteKind};

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
];

pub fn items() -> Vec<PaletteItem> {
    ACTIONS.iter().map(ActionSpec::item).collect()
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
