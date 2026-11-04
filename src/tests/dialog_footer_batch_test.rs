//! Render tests for the #1775 dialog-footer migration batch: every surface
//! that replaced a hand-rolled bracket-span hint with the shared
//! `hints::footer_line` / `hints::render_footer` helpers.
//!
//! Two families:
//! 1. Widget-level: render `hints::render_footer` into a TestBackend buffer
//!    and assert the canonical `key: verb` spans are present and styled
//!    (bold accent keys, dim verbs).
//! 2. Layout-level: /help collapses to a single column under 100 cols and
//!    still draws the footer row.

use ratatui::{Terminal, backend::TestBackend};

use crate::tui::app::dialog_keys::{self, DialogScope};
use crate::tui::render::hints;

/// The usage dashboard footer: previously centered with no colons, now the
/// canonical left-aligned `key: verb` row from the catalog.
#[test]
fn usage_dashboard_footer_is_canonical_key_colon_verb() {
    let backend = TestBackend::new(80, 3);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let area = f.area();
            hints::render_footer(
                f,
                ratatui::layout::Rect::new(area.x, area.y + 1, area.width, 1),
                dialog_keys::dialog_keys(DialogScope::UsageDashboard),
            );
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let row: String = (0..buf.area.width)
        .map(|x| buf[(x, 1)].symbol().to_string())
        .collect();
    assert!(
        row.starts_with("Tab: switch"),
        "usage footer must start with the canonical first binding, got: {row:?}"
    );
    assert!(
        row.contains("Enter: details"),
        "missing Enter binding: {row:?}"
    );
    assert!(
        row.contains("T/W/M/A: period"),
        "missing period binding: {row:?}"
    );
    assert!(row.contains("Esc: close"), "missing Esc binding: {row:?}");
    assert!(
        !row.contains(" navigate  "),
        "old bracket-style double-space verbs must be gone: {row:?}"
    );

    // Key span styling: bold. Find the 'T' of "Tab" at x=0 and check the
    // modifier survived into the cell.
    let cell = &buf[(0, 1)];
    assert!(
        cell.style()
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD),
        "footer keys must be bold, got {:?}",
        cell.style()
    );
}

/// The inline tool-approval hint renders from the ToolApproval table, which
/// advertises the `a` approve-once hotkey (#1775) alongside Enter/D/V/Esc.
#[test]
fn tool_approval_footer_line_advertises_approve_hotkey() {
    let line = hints::footer_line("  ", dialog_keys::dialog_keys(DialogScope::ToolApproval));
    let text: String = line
        .spans
        .iter()
        .map(|s| s.content.clone())
        .collect::<String>();
    for needle in [
        "a: approve",
        "Enter: confirm",
        "D/r/Esc: deny",
        "V: details",
    ] {
        assert!(
            text.contains(needle),
            "ToolApproval footer missing {needle:?}: {text:?}"
        );
    }
    assert!(text.starts_with("  "), "indent must be preserved: {text:?}");
}

/// The approve-policy menu footer: plain navigate/confirm/cancel, no `a`
/// (the menu has no single-key approve — that is the inline approval only).
#[test]
fn approve_policy_menu_footer_has_no_single_key_approve() {
    let line = hints::footer_line(
        "  ",
        dialog_keys::dialog_keys(DialogScope::ApprovePolicyMenu),
    );
    let text: String = line
        .spans
        .iter()
        .map(|s| s.content.clone())
        .collect::<String>();
    assert!(text.contains("Enter: confirm"), "missing Enter: {text:?}");
    assert!(text.contains("Esc: cancel"), "missing Esc: {text:?}");
    assert!(
        !text.contains("a: approve"),
        "menu must not advertise `a`: {text:?}"
    );
}

/// Plan overlay: the footer is a suffix of the full PlanOverlay table in
/// every state, so no state can advertise a key its handler rejects.
#[test]
fn plan_overlay_states_are_table_suffixes() {
    let keys = dialog_keys::dialog_keys(DialogScope::PlanOverlay);
    let esc_at = keys
        .iter()
        .position(|k| k.label == "Esc")
        .expect("Esc in table");
    let d_at = keys
        .iter()
        .position(|k| k.label == "d")
        .expect("d in table");

    // NoPlan: Esc-only suffix.
    assert_eq!(keys[esc_at..].len(), 1);
    // Active / PreInitEditing: from `d` onward, and that must be a real
    // suffix (starts after index 0, covers through the end).
    assert!(d_at > 0);
    assert!(d_at < esc_at, "d must come before Esc");
    // Full table must lead with `a` (PostInitEditing).
    assert_eq!(keys[0].label, "a");
}

/// The log viewer footer keeps the full filter vocabulary (e/w/i/d levels,
/// g/G jumps, day paging) that the popup handler consumes.
#[test]
fn log_viewer_footer_carries_filter_vocabulary() {
    let line = hints::footer_line("", dialog_keys::dialog_keys(DialogScope::McLogViewer));
    let text: String = line
        .spans
        .iter()
        .map(|s| s.content.clone())
        .collect::<String>();
    for needle in ["/: search", "g/G", "1-5/e/w/i/d", "[ ]", "Esc"] {
        assert!(
            text.contains(needle),
            "log viewer footer missing {needle:?}: {text:?}"
        );
    }
}

/// SSH/sudo password prompts share the SshPassword table: Enter submit,
/// Esc cancel — nothing else, since typing goes to the input.
#[test]
fn ssh_password_footer_is_minimal() {
    let keys = dialog_keys::dialog_keys(DialogScope::SshPassword);
    // Enter + Esc are real events; the middle entry is the display-only
    // "type: password" pseudo-key with empty events.
    assert_eq!(keys.len(), 3);
    assert_eq!(keys[0].label, "Enter");
    assert_eq!(keys[1].label, "type");
    assert!(
        keys[1].events.is_empty(),
        "display-only pseudo-key must replay nothing"
    );
    assert_eq!(keys[2].label, "Esc");
}

/// The usage dashboard footer follows the state (#2027): Enter is advertised
/// only while the Cache Efficiency card is focused, because that is the only
/// card with prices behind it, and with the breakdown open Esc is the whole
/// keymap.
#[test]
fn usage_dashboard_footer_follows_the_focused_card() {
    use crate::ui_core::usage::input::CACHE_CARD;
    use crate::usage::cards;
    use crate::usage::dashboard::DashboardState;

    let footer_row = |state: &DashboardState| {
        let backend = TestBackend::new(80, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                let row = ratatui::layout::Rect::new(area.x, area.y + 1, area.width, 1);
                cards::render_footer(f, row, state);
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..buf.area.width)
            .map(|x| buf[(x, 1)].symbol().to_string())
            .collect::<String>()
    };

    let on_grid = DashboardState {
        focused_card: 0,
        ..Default::default()
    };
    let row = footer_row(&on_grid);
    assert!(
        row.starts_with("Tab: switch"),
        "canonical first binding: {row:?}"
    );
    assert!(
        !row.contains("Enter"),
        "Enter does nothing off the cache card, so it must not be advertised: {row:?}"
    );
    assert!(row.contains("T/W/M/A: period"), "{row:?}");

    let on_cache = DashboardState {
        focused_card: CACHE_CARD,
        ..Default::default()
    };
    let row = footer_row(&on_cache);
    assert!(
        row.contains("Enter: details"),
        "Enter opens the prices: {row:?}"
    );

    let open = DashboardState {
        focused_card: CACHE_CARD,
        cache_detail_open: true,
        ..Default::default()
    };
    let row = footer_row(&open);
    assert_eq!(
        row.trim_end(),
        "Esc: close",
        "the open breakdown has exactly one key: {row:?}"
    );
}
