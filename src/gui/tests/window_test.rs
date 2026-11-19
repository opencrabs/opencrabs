//! The window's own tests: the panel and the popup are the two places the
//! GUI has behaviour of its own that no headless test can reach through a
//! real window.

use crate::Config;
use crate::composer_view::popup_window;
use crate::dashboard_source::{DASHBOARD, production_half};

/// Eight names is the most a popup may cover: beyond that it would
/// hide the conversation it belongs to.
#[test]
fn the_list_paints_at_most_eight_rows() {
    let window = popup_window(33, 0, 8);
    assert_eq!(window, 0..8);
}

/// The window stays put while the selection walks its rows: the eye
/// keeps a stable block of names instead of a list that scrolls on
/// every arrow.
#[test]
fn the_window_slides_only_past_its_last_row() {
    assert_eq!(popup_window(33, 7, 8), 0..8, "last row, no slide yet");
    assert_eq!(popup_window(33, 8, 8), 1..9, "one step past: slide by one");
}

/// A short list does not scroll at all.
#[test]
fn a_short_list_never_scrolls() {
    assert_eq!(popup_window(3, 0, 8), 0..3);
    assert_eq!(popup_window(3, 2, 8), 0..3);
}

/// The selection is clamped rather than trusted: the list shrinks as
/// the draft grows, and a stale index must never slice past the end.
#[test]
fn a_stale_selection_is_clamped() {
    assert_eq!(popup_window(4, 99, 8), 0..4);
    assert_eq!(popup_window(1, 99, 8), 0..1);
}

/// A one-row budget still yields one row instead of an empty range,
/// which would paint nothing and panic on an empty slice elsewhere.
#[test]
fn a_zero_budget_still_shows_one_row() {
    assert_eq!(popup_window(5, 2, 0), 2..3);
}

/// A line starting with a slash is read before the agent gates, or
/// `/stop` and `/help` would be dead in exactly the states that need
/// them. Source scan, because `send_message` needs a live window.
#[test]
fn a_command_is_read_before_the_agent_gates() {
    let src = DASHBOARD;
    let parse = src
        .find("commands::parse_command(&text)")
        .expect("send_message parses the line");
    let offline = src
        .find("agent offline: {}")
        .expect("the offline gate exists");
    assert!(
        parse < offline,
        "the command parse must precede the agent gates"
    );
}

/// A blank home reaches all four capabilities the built-in provider
/// serves, with no wizard and no key. This is what `boot_config` has
/// to leave behind, and it is testable without a window or a network
/// call: the shipped example carries the vision, generation and
/// transcription models, and seeding is what puts that section on
/// disk. Under a temp home, never the live `~/.opencrabs`.
#[test]
fn provisioning_a_fresh_home_reaches_all_four_capabilities() {
    let home = tempfile::tempdir().expect("a temp home");
    opencrabs::config::profile::with_home_override(home.path().to_path_buf(), || {
        assert!(
            opencrabs::config::seed::ensure_config_seeded(),
            "a blank home gets the shipped example"
        );
        let config = Config::load().expect("the shipped example parses");
        let section = config
            .providers
            .opencrabs
            .as_ref()
            .expect("the built-in section survives seeding");
        assert_eq!(
            section.default_model.as_deref(),
            Some("deepseek-v4.1-flash"),
            "text"
        );
        assert!(
            section
                .vision_model
                .as_deref()
                .is_some_and(|m| !m.is_empty()),
            "vision"
        );
        assert!(
            section
                .generation_model
                .as_deref()
                .is_some_and(|m| !m.is_empty()),
            "image generation"
        );
        assert!(
            section.stt_model.as_deref().is_some_and(|m| !m.is_empty()),
            "transcription"
        );
        // And the one call the window makes here is warranted: no
        // token on disk and no other usable provider.
        assert!(
            opencrabs::brain::provider::opencrabs_acquire::needs_builtin_provision(&config),
            "a blank home must ask the proxy for its token"
        );
    });
}

/// Provisioning is a one-time act: it writes the token and enables the
/// section, and the same check that said "yes, provision this" must
/// then say no. Without that, every start would ask the proxy again.
///
/// The two writes are the ones `provision_builtin_provider` makes
/// before it goes to the network, so this covers its effect on the
/// config without a token request.
#[test]
fn provisioning_stops_asking_once_the_token_is_written() {
    let home = tempfile::tempdir().expect("a temp home");
    opencrabs::config::profile::with_home_override(home.path().to_path_buf(), || {
        assert!(opencrabs::config::seed::ensure_config_seeded());
        assert!(opencrabs::config::seed::ensure_keys_seeded());
        let before = Config::load().expect("the seeded config parses");
        assert!(
            opencrabs::brain::provider::opencrabs_acquire::needs_builtin_provision(&before),
            "a blank home is the case the fix exists for"
        );

        Config::write_key("providers.opencrabs", "enabled", "true")
            .expect("the section is enabled");
        Config::write_key("providers.opencrabs", "api_key", "test-token")
            .expect("the token is written");

        let after = Config::load().expect("the provisioned config parses");
        assert!(
            !opencrabs::brain::provider::opencrabs_acquire::needs_builtin_provision(&after),
            "a provisioned home must not be touched again"
        );
    });
}

/// And the provisioned config is one the provider factory walks to the
/// built-in provider: the same registry answer `create_provider`
/// consumes, checked without a reactor or a request.
#[test]
fn a_provisioned_home_resolves_the_builtin_provider() {
    let home = tempfile::tempdir().expect("a temp home");
    opencrabs::config::profile::with_home_override(home.path().to_path_buf(), || {
        assert!(opencrabs::config::seed::ensure_config_seeded());
        assert!(opencrabs::config::seed::ensure_keys_seeded());
        Config::write_key("providers.opencrabs", "enabled", "true")
            .expect("the section is enabled");
        Config::write_key("providers.opencrabs", "api_key", "test-token")
            .expect("the token is written");

        let config = Config::load().expect("the provisioned config parses");
        let (provider, model) = config.providers.active_provider_and_model();
        assert_eq!(provider, "opencrabs", "the built-in provider is the one");
        assert_eq!(
            model, "deepseek-v4.1-flash",
            "with the model the shipped section declares"
        );
    });
}

/// The preview gate asks about the work, not only about the rail's
/// pane-less set: an open turn, a parked turn holding a question or a
/// tool, or a background pane-less session each mean the newest row
/// can still change. All three empty means nothing to read.
#[test]
fn a_preview_refresh_follows_any_live_work() {
    use crate::previews_can_move;
    assert!(previews_can_move(true, false, 0), "the open turn streams");
    assert!(
        previews_can_move(false, true, 0),
        "a parked turn is working"
    );
    assert!(
        previews_can_move(false, false, 1),
        "a pane-less session runs"
    );
    assert!(
        !previews_can_move(false, false, 0),
        "with no work at all the last row cannot have moved"
    );
}

/// The single-session composer and its key hints are ONE card, not a
/// pill parked above a separate footer row.
///
/// The scan reads the rendered order, not the presence of a style: a
/// `rounded(px(..))` on the composer is invisible to a test that only
/// greps for the dock's id, which is how the floating pill shipped in
/// the first place. Three things are pinned together: the dock owns
/// the surface and the radius, the footer is its child, and the
/// composer paints its own card only while nothing docks it (#2987).
#[test]
fn composer_dock_holds_the_input_and_the_key_hints() {
    let dock_at = DASHBOARD
        .find(".id(\"composer-dock\")")
        .expect("the single-session composer dock must exist");
    let window = DASHBOARD
        .get(dock_at..)
        .expect("the dock offset lands on a char boundary");
    let end = window.find("col\n").expect("the chat column ends the dock");
    let dock = window
        .get(..end)
        .expect("the chat column ends on a char boundary");
    for rule in [
        ".child(composer)",
        ".child(footer.into_any_element())",
        ".rounded(px(COMPOSER_RADIUS))",
        ".bg(glass_fill(palette::current().panel))",
        ".gap_0()",
    ] {
        assert!(dock.contains(rule), "the dock lost {rule}");
    }
    // The hairline is the only separator inside the card: a second
    // gap would reopen the space the owner called "flying".
    assert!(
        dock.contains(".h(px(1.))"),
        "the hairline between the input and the key hints is gone"
    );

    // The composer paints its own card ONLY when nothing docks it. A
    // focused split pane renders the bare editor (`pane_view.rs`), so
    // deleting the chrome outright would leave that pane boxless while
    // its keyboard-less siblings keep theirs. The dock suppresses it
    // through the flag, and the flag is recomputed per frame because
    // the window's composer becomes the focused pane's editor on split.
    let editor = include_str!("../editor.rs");
    assert!(
        editor.contains(".when(!self.docked, |d| {"),
        "the composer chrome is no longer conditional on the dock"
    );
    for chrome in [".shadow_sm()", ".rounded(px(COMPOSER_RADIUS))"] {
        let at = editor
            .find(".when(!self.docked, |d| {")
            .expect("the dock gate");
        let gate = editor
            .get(at..)
            .expect("the dock gate offset lands on a char boundary");
        let end = gate.find(".p_3()").expect("the field's padding");
        assert!(
            gate.get(..end)
                .expect("the field padding ends on a char boundary")
                .contains(chrome),
            "the undocked composer lost {chrome}"
        );
    }
    assert!(
        DASHBOARD
            .contains("editor.docked = self.screen == Screen::Chat && !self.window_is_split();"),
        "the dock flag must be recomputed per frame, not latched"
    );

    // The dim composer a keyboard-less pane draws is the same 44px
    // box, so it follows the same radius or the pane reshapes on
    // focus loss.
    let chrome = include_str!("../pane_chrome.rs");
    assert!(
        chrome.contains(".rounded(px(card::COMPOSER_RADIUS))"),
        "the dim pane composer no longer shares the dock radius"
    );
}

/// The bottom topic row belongs ABOVE the composer dock, in
/// Telegram's forum arrangement. It used to be appended after the
/// composer and before the footer, which trapped it between two
/// surfaces it does not belong to.
#[test]
fn composer_dock_sits_below_the_bottom_topic_row() {
    // Anchor on the dock, not on `Screen::Chat => {`: the file has two
    // such match arms (the body builder and the render match), and a
    // `.nth(1)` silently reads the wrong one when a third appears.
    let dock = DASHBOARD
        .find(".id(\"composer-dock\")")
        .expect("the dock is still drawn");
    // The topic row is appended to the same chat column, before the
    // dock. Searching the text in front of the dock is what proves
    // the order; a whole-file `find` would match an unrelated arm.
    let before = DASHBOARD
        .get(..dock)
        .expect("the dock offset lands on a char boundary");
    let topic = before
        .rfind("if let Some(strip) = topic_bottom")
        .expect("the bottom topic row is still drawn");
    let chat_start = before
        .get(..topic)
        .expect("the topic row offset lands on a char boundary")
        .rfind("Screen::Chat => {")
        .expect("the topic row belongs to the chat render arm");
    assert!(
        chat_start < topic && topic < dock,
        "the bottom topic row moved below the composer again"
    );
    // And the footer is no longer a sibling row of the chat column:
    // that row is what made the composer and the footer read as two
    // unrelated modules.
    // Stop the region at the window's own test module. The footer pin
    // used to read to the end of the file, so it matched the needle in
    // this very assertion and reported the footer back as a sibling
    // row every time (#2999).
    let chat = production_half()
        .get(chat_start..)
        .expect("the chat arm offset lands on a char boundary");
    assert!(
        !chat.contains("col.child(central(footer"),
        "the footer escaped the dock and became its own row again"
    );
}
