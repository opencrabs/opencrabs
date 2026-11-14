//! Desktop settings modal wiring and secret-handling contract (#2878).
//!
//! GPUI has no headless window harness here. Pure setting behaviour lives in
//! `gui/settings_view_test.rs`; these tests pin its window integration, while
//! `config_remove_key_test.rs` exercises the real file mutation.

use crate::ui_core::help_keys;
use crate::ui_core::palette::{PaletteAction, PaletteItem, PaletteKind, PaletteState};

const GUI_COMMANDS: &str = include_str!("../gui/commands.rs");
const GUI_COMMAND_DISPATCH: &str = include_str!("../gui/dashboard_commands.rs");
const GUI_COMPOSER_VIEW: &str = include_str!("../gui/composer_view.rs");
const GUI_EDITOR: &str = include_str!("../gui/editor.rs");
const GUI_FINDER: &str = include_str!("../gui/dashboard_finder.rs");
const GUI_MAIN: &str = include_str!("../gui/main.rs");
const GUI_SETTINGS: &str = include_str!("../gui/dashboard_settings.rs");
const GUI_KEYS: &str = include_str!("../gui/dashboard_keys.rs");
const GUI_NAV: &str = include_str!("../gui/dashboard_nav.rs");
const SETTINGS_VIEW: &str = include_str!("../gui/settings_view.rs");
const SETTINGS_MODAL: &str = include_str!("../gui/settings_modal.rs");

#[test]
fn ctrl_comma_is_listed_as_the_window_settings_chord() {
    let row = help_keys::gui_rows()
        .find(|row| row.gui == Some("ctrl-comma"))
        .expect("the help screen lists ctrl+, for settings");
    assert!(row.description.to_lowercase().contains("settings"));
    assert!(
        row.description.contains("/settings"),
        "the GUI /help screen must name the slash command as well as ctrl+,"
    );
}

#[test]
fn settings_slash_command_opens_the_existing_modal() {
    for needle in [
        "\"/settings\" => GuiAction::OpenSettings",
        "commands::support_gui(&invocation.name)",
        "commands::lookup_gui(&invocation.name)",
    ] {
        assert!(
            GUI_COMMANDS.contains(needle),
            "missing command wiring: {needle}"
        );
    }
    for (source, needle) in [
        (
            GUI_COMMAND_DISPATCH,
            "commands::GuiAction::OpenSettings => {",
        ),
        (GUI_COMMAND_DISPATCH, "self.toggle_settings(cx);"),
        (GUI_FINDER, "slash::gui_commands()"),
        (GUI_COMPOSER_VIEW, "slash::lookup_gui(name)"),
    ] {
        assert!(source.contains(needle), "missing window wiring: {needle}");
    }
    assert!(
        GUI_EDITOR.contains("gui_completions(&self.content)"),
        "the composer must autocomplete /settings"
    );
}

#[test]
fn seeker_offers_an_executable_settings_command_before_the_help_row() {
    for query in ["settings", "/settings", "preferences"] {
        let mut items: Vec<PaletteItem> = crate::ui_core::commands::gui_commands()
            .filter(|command| !crate::ui_core::palette::is_surface_command(command.name))
            .map(|command| {
                PaletteItem::new(
                    command.name,
                    command.description,
                    PaletteKind::Command,
                    PaletteAction::RunSlash(command.name),
                )
                .with_keywords(crate::ui_core::palette::keywords_for(command.name))
            })
            .collect();
        items.extend(crate::ui_core::palette::surface_items());
        items.extend(help_keys::seeker_items(help_keys::HelpClient::Gui));

        let mut state = PaletteState::new(items);
        state.insert_text(query);
        let visible = state.visible();
        let first = visible
            .first()
            .unwrap_or_else(|| panic!("{query} has no settings result"));
        assert_eq!(first.item.title, "/settings", "{query} ranked help first");
        assert_eq!(
            first.item.action,
            PaletteAction::RunSlash("/settings"),
            "{query} must open settings, not the help overlay"
        );
    }
}

#[test]
fn the_modal_owns_the_keyboard_and_paints_above_the_window() {
    for needle in [
        "fn toggle_settings",
        "fn close_settings",
        "fn settings_modal_key",
        "settings_view::pane_index(",
        "settings_view::section_rows(active, setting_count, cred_count)",
    ] {
        assert!(
            GUI_SETTINGS.contains(needle),
            "missing settings behaviour: {needle}"
        );
    }
    assert!(GUI_NAV.contains("return vec![OverlayId::Settings];"));
    for needle in [
        "if self.settings_open {",
        "self.settings_modal_key(ks, cx);",
    ] {
        assert!(GUI_KEYS.contains(needle), "missing key ownership: {needle}");
    }
    assert!(GUI_KEYS.contains("ev.keystroke.modifiers.control && ev.keystroke.key == \",\""));
    assert!(SETTINGS_MODAL.contains(".track_scroll(scroll)"));
    let modal = GUI_MAIN
        .find("root = root.child(self.settings_modal(cx));")
        .expect("settings modal is attached");
    let panes = GUI_MAIN.find(".child(right)").expect("panes are attached");
    assert!(panes < modal, "settings paints above the panes");
    assert!(SETTINGS_MODAL.contains(".id(\"settings-modal-scrim\")"));
    assert!(SETTINGS_MODAL.contains(".occlude()"));
}

#[test]
fn the_rail_paints_one_section_and_switches_on_click() {
    for needle in [
        "pub enum SettingsSection",
        "pub const ALL: [SettingsSection; 6]",
        "pub fn section_rows(",
        "pub fn pane_index(",
    ] {
        assert!(
            SETTINGS_VIEW.contains(needle),
            "missing section model: {needle}"
        );
    }
    for needle in [
        "pub fn rail_item(",
        "rail: Vec<gpui::AnyElement>",
        ".id(\"settings-modal-rail\")",
    ] {
        assert!(
            SETTINGS_MODAL.contains(needle),
            "missing rail shell: {needle}"
        );
    }
    for needle in [
        "self.settings_section = settings_view::SettingsSection::General;",
        "for section in settings_view::SettingsSection::ALL {",
        "this.settings_select_section(section, cx);",
        "\"tab\" => {",
        "let rows = settings_view::section_rows(active, setting_count, cred_count);",
    ] {
        assert!(
            GUI_SETTINGS.contains(needle),
            "missing rail wiring: {needle}"
        );
    }
    assert!(
        GUI_MAIN.contains("settings_section: settings_view::SettingsSection,"),
        "the dashboard must remember the active section"
    );
    // The old full-table paint is gone: no header walk over every section.
    assert!(
        !GUI_SETTINGS.contains("body.push(settings_modal::section_header"),
        "the pane paints one section, not a stack of headers"
    );
}

#[test]
fn a_failed_config_load_paints_no_invented_settings() {
    for needle in [
        "self.settings_values = Vec::new();",
        "self.settings_values.len() + self.settings_creds.len()",
        "let setting_count = self.settings_values.len();",
        "if self.settings_row_count() == 0 {",
    ] {
        assert!(
            GUI_SETTINGS.contains(needle),
            "missing empty-config guard: {needle}"
        );
    }
    assert!(
        !GUI_SETTINGS.contains("unwrap_or(settings_view::RowValue::Bool(false))"),
        "a missing value must not become a fallback setting row"
    );
}

#[test]
fn writes_use_the_config_apis_and_credentials_stay_masked() {
    for needle in [
        "Config::write_key(spec.section, spec.key",
        "Config::write_key_string(spec.section, spec.key",
        "Config::remove_key(spec.section, spec.key",
        "Config::write_keys_key(&cred.section, \"api_key\"",
        "format!(\"saved {}.api_key\", cred.section)",
        "cx.read_from_clipboard()",
        "buffer.chars().count()",
        "\"\\u{2022}\".repeat(len.max(1))",
    ] {
        assert!(
            GUI_SETTINGS.contains(needle),
            "missing write or mask: {needle}"
        );
    }
    assert!(
        SETTINGS_VIEW.contains("pub struct CredSpec"),
        "credential rows carry only section, label and configured state"
    );
    assert!(
        !SETTINGS_VIEW.contains("api_key: cfg"),
        "credential rows must not retain the secret"
    );
}

#[test]
fn startup_only_settings_are_labelled() {
    for key in ["lazy_tools", "vector_enabled"] {
        let at = SETTINGS_VIEW
            .find(&format!("key: \"{key}\""))
            .unwrap_or_else(|| panic!("{key} has a settings row"));
        let row = SETTINGS_VIEW
            .get(at..)
            .and_then(|tail| tail.split("SettingSpec").next())
            .expect("setting row body");
        assert!(row.contains("restart: true"), "{key} needs a restart label");
    }
}
