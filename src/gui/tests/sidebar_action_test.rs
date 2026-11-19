use crate::dashboard_rail::set_rail_open_state;
use crate::dashboard_source::DASHBOARD;
use crate::*;

/// The body of one function in one of the GUI files, up to the next method.
/// The source scans below pin decisions a unit test cannot reach without a
/// window, and they read the body so a mention elsewhere cannot satisfy
/// them (#2665).
fn fn_body(src: &'static str, name: &str) -> &'static str {
    item_body(src, &format!("fn {name}("))
}

/// The Dashboard's window render. Two impls in this file define `render`,
/// and only this signature is the one that holds the window (#2741).
fn dashboard_render(src: &'static str) -> &'static str {
    // Scoped to the window's own `Render` impl first: the rail is a view of
    // its own now (#2933) and its render carries the same signature, so a
    // bare split would read the rail's body instead of the window's.
    item_body(
        item_body(src, "impl Render for Dashboard"),
        "fn render(&mut self, window: &mut Window, cx: &mut Context<Self>)",
    )
}

/// The body of one item, from its own definition to its closing brace.
///
/// Two things this gets right that a plain `split` does not: only a real
/// definition counts, because the scans below quote the names they look
/// for inside their own strings, and the body ends at the brace that
/// closes it rather than at the next method, so a top level function in
/// another module file reads the same as a `Dashboard` method (#2741).
fn item_body(src: &'static str, needle: &str) -> &'static str {
    let at = src
        .match_indices(needle)
        .map(|(at, _)| at)
        .find(|&at| {
            let line = src[..at].rfind('\n').map(|n| n + 1).unwrap_or(0);
            let prefix = src[line..at].trim();
            prefix.is_empty() || prefix.starts_with("pub")
        })
        .unwrap_or_else(|| panic!("{needle} not found"));
    let line = src[..at].rfind('\n').map(|n| n + 1).unwrap_or(0);
    let indent = src[line..at]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .count();
    let close = format!("\n{}}}", " ".repeat(indent));
    let rest = &src[at..];
    rest.split(&close).next().unwrap_or(rest)
}

#[test]
fn sidebar_actions_gui_open_and_close_are_idempotent() {
    for initially_open in [false, true] {
        let mut open = initially_open;
        let mut focused = initially_open;
        let mut sessions = sessions::SessionsState {
            filter: String::from("needle"),
            filter_active: initially_open,
            ..Default::default()
        };

        set_rail_open_state(true, &mut open, &mut focused, &mut sessions);
        set_rail_open_state(true, &mut open, &mut focused, &mut sessions);
        assert!(open);
        // Opening arms the walk (#2665): the arrows have to move the cards
        // without a Tab first, which is what opening the sidebar means.
        assert!(focused);
        assert_eq!(sessions.filter_active, initially_open);

        set_rail_open_state(false, &mut open, &mut focused, &mut sessions);
        set_rail_open_state(false, &mut open, &mut focused, &mut sessions);
        assert!(!open);
        assert!(!focused);
        assert!(sessions.filter.is_empty());
        assert!(!sessions.filter_active);
    }
}

#[test]
fn opening_the_rail_arms_the_walk_on_the_open_conversation() {
    // #2665: opening the sidebar is asking to move with the arrows, so it
    // must not need a Tab first, and the walk starts on the conversation
    // on screen instead of nowhere.
    let mut open = false;
    let mut focused = false;
    let mut sessions = sessions::SessionsState::default();
    let on_screen = Uuid::new_v4();
    sessions.selected = Some(on_screen);

    set_rail_open_state(true, &mut open, &mut focused, &mut sessions);

    assert!(open, "the rail opens");
    assert!(
        focused,
        "opening arms the keys, the arrows need no Tab first"
    );
    assert_eq!(
        sessions.cursor,
        Some(on_screen),
        "the walk starts where the user is, not on an arbitrary row"
    );
}

#[test]
fn a_committed_row_hands_the_keyboard_to_the_composer() {
    // #2665: a pick ends the walk. The rail stays on screen but stops
    // owning keys, and the focus request is made after that, because
    // `open_overlays` counts the rail only while it is focused.
    let src = DASHBOARD;
    let body = fn_body(src, "open_session_handing_back");
    let (_before, after) = body
        .split_once("if hand_back {")
        .unwrap_or_else(|| panic!("the hand back branch"));
    assert!(
        after.contains("self.rail_focused = false"),
        "a committed row takes the keys off the rail"
    );
    assert!(
        after.contains("self.refocus_screen()"),
        "the focus request follows, so the derivation sees an unarmed rail"
    );
    assert!(
        after.find("self.rail_focused = false") < after.find("self.refocus_screen()"),
        "the rail has to let go before focus is derived"
    );
    // Every commit goes through the wrapper, so the click, Enter, the
    // palette, the find field and a topic tab all hand the keys back.
    assert!(
        fn_body(src, "open_session").contains("open_session_handing_back(id, true, cx)"),
        "a commit is the form that hands the keyboard back"
    );
}

#[test]
fn the_caret_receipt_names_who_took_the_keys() {
    // #2741: the reconciler can ask for the composer and still lose the
    // caret to the next press, and a silent `window.focus` cannot tell
    // those apart. The receipt records the state before the call and the
    // owner after it, so the next build answers it from the daily log.
    let src = DASHBOARD;
    let render = dashboard_render(src);
    assert!(
        render.contains("caret -> composer before={before} now={}"),
        "the composer branch leaves a receipt"
    );
    assert!(
        render.contains("caret -> root before={before} now={}"),
        "the root branch leaves the mirror receipt"
    );
    assert!(
        render.contains("keyboard owner -> {owner:?}"),
        "an owner transition names the owner it moved to"
    );
    assert!(
        render.contains("self.caret_owner_name(window, cx)"),
        "the receipt names the element that holds the keys after the call"
    );
    let helper = fn_body(src, "caret_owner_name");
    for name in ["\"composer\"", "\"root\"", "\"field\"", "\"none\""] {
        assert!(helper.contains(name), "the helper names {name}");
    }
    assert!(
        helper.contains("window.focused(cx)"),
        "the name comes from the window's live focus, not a guess"
    );
}

#[test]
fn the_arrow_walk_previews_without_taking_the_keys() {
    // #2665: the walk opens each row it stands on, so handing the composer
    // the caret there would eat the next arrow. #2819: the row lands in the
    // pane holding the caret, still without the hand back.
    let src = DASHBOARD;
    let keys = fn_body(src, "on_key");
    assert!(
        keys.contains("open_picked_in_focused_pane(id, false, cx)"),
        "the arrows preview the row and keep the rail armed"
    );
}

#[test]
fn a_created_session_and_a_new_split_leave_the_caret_in_the_composer() {
    // #2665: every one of these was clicked from the rail, so each has to
    // take the keys off it before asking for focus again.
    let src = DASHBOARD;
    for name in ["new_session", "choose_split"] {
        let body = fn_body(src, name);
        let (_before, after) = body
            .split_once("self.rail_focused = false")
            .unwrap_or_else(|| panic!("{name} never lets the rail go"));
        assert!(
            after.contains("self.refocus_screen()"),
            "{name} asks for focus after the rail lets go"
        );
    }
}

#[test]
fn a_press_on_the_sidebar_never_takes_the_keyboard() {
    // #2798: the window root armed the rail from the press's own x
    // position, so every press in the column that did not commit a pick
    // (the empty space under the rows, the scrollbar, the header, a
    // filter chip) left the composer without its caret until
    // Esc. A press is not a walk: only a key arms the rail.
    let src = DASHBOARD;
    let render = dashboard_render(src);
    assert!(
        !render.contains("ev.position.x) <= this.rail_width"),
        "no pointer press decides who owns the keys"
    );
    // The window root's key handler is followed by the one press listener
    // it owns, and the drag handler comes after it: that slice is the
    // press, with nothing of the rows' listeners in it.
    let root_press = render
        .split(".on_key_down(cx.listener(Self::on_key))")
        .nth(1)
        .and_then(|rest| rest.split(".on_mouse_move(").next())
        .unwrap_or_else(|| panic!("the root still listens for a press"));
    assert!(
        root_press.contains("this.rail_hands_back();"),
        "a press anywhere hands the keys back to the composer"
    );
    assert!(
        !root_press.contains("rail_focused ="),
        "a press never decides who owns the keys"
    );
    assert!(
        root_press.contains("this.sessions.rail_menu.take()"),
        "the same press still dismisses the rail's menu"
    );
    let helper = fn_body(src, "rail_hands_back");
    assert!(
        helper.contains("self.rail_focused = false"),
        "the rail lets go of the keys"
    );
    assert!(
        helper.contains("self.refocus_screen()"),
        "the press asks for the caret once the rail lets go"
    );
    assert!(
        helper.find("self.rail_focused = false") < helper.find("self.refocus_screen()"),
        "the rail has to let go before focus is derived"
    );
    assert!(
        helper.contains("self.split_pick.is_some()"),
        "a split waiting for its pane keeps the keys its dialog answers"
    );
    // The walk still arms without a press, or the arrows and Tab would
    // have nothing left to open with.
    let open = fn_body(src, "set_rail_open_state");
    assert!(
        open.contains("*rail_focused = true"),
        "opening the rail still arms the walk"
    );
    // Leaving the find field hands the caret back, whichever way it exits.
    assert!(
        fn_body(src, "close_rail_filter").contains("self.refocus_screen()"),
        "closing the filter and the find field asks for the caret"
    );
}

#[test]
fn a_rail_affordance_that_stops_the_press_hands_the_keys_back() {
    // #2798: these three stop the press before it reaches the window
    // root, so the root cannot hand the caret back for them. Each one
    // does it itself, or a press on a rail affordance leaves the composer
    // waiting for an Esc.
    let src = DASHBOARD;
    // The rail asks the window by name now (#2933): each affordance has a
    // method of its own, and the hand back lives in it rather than in the
    // closure that used to be minted inside the window's render.
    for (method, why) in [
        ("rail_row_menu", "a row's right press"),
        ("rail_toggle_pin", "a row's pin"),
        ("rail_chip_menu", "a topic chip's right press"),
    ] {
        let body = fn_body(src, method);
        assert!(
            body.contains("self.rail_hands_back();"),
            "{why} hands the keys back before it opens the card"
        );
    }
    // A row press is not a pick yet, and the caret does not wait for the
    // release to find out what it was.
    let press = fn_body(src, "rail_row_press");
    assert!(
        press.contains("self.rail_hands_back();"),
        "a press on a row hands the keys back whether or not it commits"
    );
}

#[test]
fn an_attached_file_asks_for_the_caret_back() {
    // #2665: the platform picker holds the keyboard while it is open, so
    // attaching is the moment to ask for it back. Deferred to render
    // because only a frame holding a window can focus.
    let src = include_str!("../editor.rs");
    let body = src
        .split("fn pick_paths(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .unwrap_or_else(|| panic!("pick_paths"));
    assert!(
        body.contains("attach_paths(paths) > 0"),
        "only a real attach asks for it"
    );
    assert!(
        body.contains("editor.focus_pending = true"),
        "the caret goes back to the field the chips landed in"
    );
    // Cut at the impl's closing brace: `split("impl ")` also matches the
    // `-> impl IntoElement` return type and leaves a 76-byte stub.
    let render = src
        .split("impl Render for Editor")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("the render impl");
    assert!(
        render.contains("window.focus(&self.focus_handle)"),
        "the deferred request needs the window, so it lands in render"
    );
}

/// The rename dialog's icon picks render big enough to recognise
/// (#2789): picker cells at 4.0rem, the chosen badge at 3.2rem inside a
/// 4.0rem slot. The 512x512 sources stay crisp at 2x.
#[test]
fn rename_dialog_icons_render_big_and_crisp() {
    let src = DASHBOARD;
    // Scope to the window render: this test module sits above it in the
    // file, so a bare split would land in the test's own strings.
    let render = dashboard_render(src);
    let picker = render
        .split("topic-icon-{name}")
        .nth(1)
        .expect("picker cell id")
        .split(".collect()")
        .next()
        .expect("picker cell before collect");
    assert!(
        picker.contains("topic_icons::picture(bytes, 4.0)"),
        "a pick renders at 4.0rem, not a speck"
    );
    let overlay = fn_body(include_str!("../overlays.rs"), "topic_prompt_overlay");
    assert!(
        overlay.contains("topic_icons::picture(bytes, 3.2)"),
        "the chosen badge picture renders at 3.2rem"
    );
    assert!(
        overlay.contains(".size(rems(4.0))"),
        "the badge slot grows to 4.0rem around it"
    );
}

/// A rename keeps the session's icon (#2791): the dialog seeds the draft
/// from the stored value, and the submit path writes it back only when
/// the helper says the draft changed it.
#[test]
fn rename_seeds_and_persists_the_icon() {
    let src = DASHBOARD;
    let start = fn_body(src, "start_rename");
    assert!(
        start.contains("self.topic_draft_icon = self.sessions.topic_icons.get(&id).cloned()"),
        "the rename dialog opens on the session's own icon"
    );
    let submit = fn_body(src, "submit_topic");
    assert!(
        submit.contains("sessions::icon_write_needed("),
        "the rename branch asks the helper before writing"
    );
    assert!(
        submit.contains("sessions::set_topic_icon(db_path, target, draft_icon)"),
        "the rename branch persists the changed icon"
    );
}

/// The session image is the upload trigger (#2790): the badge in the
/// prompt carries a click that opens the file picker, and the add-image
/// cell keeps its own. Rename and creation share the overlay, so both
/// get the trigger.
#[test]
fn the_session_image_opens_the_picker() {
    let src = DASHBOARD;
    let overlay = fn_body(include_str!("../overlays.rs"), "topic_prompt_overlay");
    assert!(
        overlay.contains(".id(\"topic-prompt-image\")"),
        "the badge is a click target, not a static picture"
    );
    assert!(
        overlay.contains(".on_click(pick_image)"),
        "clicking the session image opens the file picker"
    );
    assert!(
        overlay.contains(".cursor_pointer()"),
        "the badge shows it can be clicked"
    );
    let render = dashboard_render(src);
    let badge_pick = render
        .split("let pick_image = cx.listener")
        .nth(1)
        .expect("the badge listener")
        .split("});")
        .next()
        .expect("listener body");
    assert!(
        badge_pick.contains("this.pick_icon_image(cx)"),
        "the badge opens the same picker the add-image cell opens"
    );
    assert!(
        render.contains(".id(\"topic-icon-add-image\")"),
        "the add-image cell stays in the picker row"
    );
}

/// The rail card draws the stored icon, not just custom images (#2790):
/// the same resolver the topic tab uses, so a pick in the rename dialog
/// reaches the card. The rail resolves it in the window, which owns the
/// decoded-image cache, and draws it in its own view (#2933), so the two
/// halves are read where they now live. Scoped to those regions: this test
/// module sits inside the same concat, and a bare split would land in the
/// test's own strings.
#[test]
fn rail_card_resolves_the_stored_icon() {
    let src = DASHBOARD;
    let wiring = src
        .split("pub(crate) fn rail_icon(")
        .nth(1)
        .expect("the rail's icon resolver")
        .split("/// The icon picker's")
        .next()
        .expect("the resolver ends at the next method");
    assert!(
        wiring.contains("topic_icons::tab_icon(stored, sessions::RAIL_MARK_REMS)"),
        "an embedded mark is recognised at the full rail-card media size"
    );
    assert!(
        wiring.contains("self.icon_images.get(stored)"),
        "a custom image comes from the view's decoded cache"
    );
    assert!(
        wiring.contains("topic_icons::IMG_PREFIX"),
        "an image that failed to decode paints nothing rather than its raw path"
    );
    let drawing = src
        .split("icon: row.flags.icon.as_ref()")
        .nth(1)
        .expect("the rail draws the resolved icon")
        .split("let chips = row")
        .next()
        .expect("the icon arm ends at the strip");
    assert!(
        drawing.contains("topic_icons::tab_icon(stored, sessions::RAIL_MARK_REMS)"),
        "an embedded mark renders at the full rail-card media size"
    );
    assert!(
        drawing.contains("topic_icons::cached_picture(")
            && drawing.contains("sessions::RAIL_MARK_REMS"),
        "a custom image renders from the window's handle at the same size"
    );
    assert!(
        drawing.contains("topic_icons::tab_glyph(stored, sessions::RAIL_MARK_REMS)"),
        "a legacy glyph keeps its text slot at the same size"
    );
}

#[test]
fn a_topic_switch_hands_the_caret_back() {
    // #2741: a click on a topic chip took the caret and never asked for it
    // back, on both of the paths that do not go through `open_session`:
    // the session already sits in another leaf, or the leaf is renamed.
    let src = DASHBOARD;
    let body = fn_body(src, "open_topic_in_pane");
    assert_eq!(
        body.matches("self.request_composer_caret(cx)").count(),
        2,
        "both branches ask the composer for the caret"
    );
    assert_eq!(
        body.matches("self.rail_focused = false").count(),
        2,
        "a chip in the rail also lets the rail go, the way a row does"
    );
    let caret = fn_body(src, "request_composer_caret");
    assert!(
        caret.contains("editor.focus_pending = true"),
        "the field claims the caret from its own render, after the press"
    );
    assert!(
        caret.contains("self.refocus_screen()"),
        "the derived owner stays the one decision point"
    );
}

#[test]
fn a_pick_lands_in_the_pane_that_holds_the_caret() {
    // #2819: every commit door went through `open_session`, which is the
    // window door and only knows the chat leaf. With a second pane holding
    // the caret the wrong pane changed, and the pane the reader was
    // looking at never moved.
    let src = DASHBOARD;
    let helper = fn_body(src, "open_picked_in_focused_pane");
    assert!(
        helper.contains("self.pane_focus"),
        "the door asks which pane holds the caret"
    );
    assert!(
        helper.contains("tree.contains(*focus)"),
        "a cached caret is only trusted while the tree still holds it"
    );
    let rename = ["tree.rename_leaf(caret, ", "id)"].concat();
    assert!(
        helper.contains(rename.as_str()),
        "the leaf holding the caret is the one that moves"
    );
    let follows = ["self.pane_focus = Some(", "id);"].concat();
    assert!(
        helper.contains(follows.as_str()),
        "the caret follows the conversation into the renamed leaf"
    );
    assert!(
        helper.contains("self.persist_layout()"),
        "the tree the user now sees is the one written down"
    );
    // The rail's two commits and its arrow preview all take this door.
    let keys = fn_body(src, "on_key");
    let commit = ["open_picked_in_focused_pane(id, ", "true, cx)"].concat();
    assert!(
        keys.contains(commit.as_str()),
        "a committed rail row moves the focused pane"
    );
    let preview = ["open_picked_in_focused_pane(id, ", "false, cx)"].concat();
    assert!(
        keys.contains(preview.as_str()),
        "the walk previews in the focused pane without the hand back"
    );
    // The row's click handler moved with the rail (#2933): the view asks
    // the window by name, so the commit is pinned on the method it calls.
    let row_pick = fn_body(src, "rail_row_pick");
    assert!(
        row_pick.contains(commit.as_str()),
        "the row click moves the focused pane"
    );
    let pick = fn_body(src, "pick_or_open");
    assert!(
        pick.contains(commit.as_str()),
        "the palette and the filter land in the focused pane"
    );
}

#[test]
fn a_duplicate_pick_warns_instead_of_swapping() {
    // #2819: picking a conversation that already sits in a pane used to
    // drag it into the chat leaf and close the pane it came from, so one
    // pick could take a split apart for nothing.
    let src = DASHBOARD;
    let helper = fn_body(src, "open_picked_in_focused_pane");
    let (_before, after) = helper
        .split_once("if duplicate {")
        .unwrap_or_else(|| panic!("the duplicate branch"));
    let branch = after
        .split_once("let Some(caret) = caret else {")
        .map(|(branch, _)| branch)
        .unwrap_or_else(|| panic!("the branch ends where the chat leaf path begins"));
    assert!(
        branch.contains("self.warn_pane_duplicate(id, cx)"),
        "a duplicate says so instead of moving anything"
    );
    assert!(
        branch.contains("return false;"),
        "the pick reports that it did not land instead of renaming a leaf"
    );
    assert!(
        !branch.contains(["rename_leaf(caret, ", "id)"].concat().as_str()),
        "no leaf moves on the duplicate path"
    );
    let warn = fn_body(src, "warn_pane_duplicate");
    assert!(
        warn.contains("self.pane_warning = Some(title)"),
        "the card carries the conversation it named"
    );
    let overlays = fn_body(src, "open_overlays");
    assert!(
        overlays.contains("self.pane_warning.is_some()"),
        "the card owns the keyboard while it is up"
    );
    assert!(
        src.contains("is already open in another pane"),
        "the card says why the pick did nothing"
    );
    let keys = fn_body(src, "on_key");
    assert!(
        keys.contains("self.pane_warning = None"),
        "Enter or Esc closes the card"
    );
    let render = dashboard_render(src);
    assert!(
        render.contains("warning_overlay(&body, dismiss)"),
        "the card is the window's existing warning shape"
    );
}

#[test]
fn a_topic_pick_drafts_in_the_pane_holding_the_caret() {
    // #2819: both topic-pick arms opened through `open_session`, which
    // moves the chat leaf, so asking for a new topic on a conversation
    // picked from the palette changed the wrong pane. The prompt resolves
    // its parent from the pane holding the caret, so an arm that carried on
    // after a refused pick would draft under the wrong conversation.
    let src = DASHBOARD;
    let body = fn_body(src, "run_finder_pick");
    let draft = body
        .split_once("TopicPick::Draft => {")
        .map(|(_, rest)| rest)
        .unwrap_or_else(|| panic!("the draft arm"))
        .split_once("TopicPick::EnableThenDraft => {")
        .map(|(arm, _)| arm)
        .unwrap_or_else(|| panic!("the draft arm ends at the next arm"));
    let enable = body
        .split_once("TopicPick::EnableThenDraft => {")
        .map(|(_, rest)| rest)
        .unwrap_or_else(|| panic!("the enable arm"))
        .split_once("PaletteAction::Favorite =>")
        .map(|(arm, _)| arm)
        .unwrap_or_else(|| panic!("the enable arm ends at the next palette row"));
    for (name, arm) in [("draft", draft), ("enable then draft", enable)] {
        assert!(
            arm.contains("if self.open_picked_in_focused_pane(id, true, cx)"),
            "the {name} arm opens in the pane holding the caret and only carries on when the pick landed"
        );
        assert!(
            !arm.contains("self.open_session(id, cx)"),
            "the {name} arm never moves the chat leaf"
        );
    }
    assert!(
        draft.contains("self.start_topic_draft(cx)"),
        "the draft arm opens the prompt"
    );
    assert!(
        enable.contains("this.start_topic_draft(cx)"),
        "the enable arm opens the prompt after the flag is written"
    );
    // The rail row's own "New topic" item walked the same chat leaf door,
    // so the prompt landed on the wrong conversation there too.
    let menu = fn_body(src, "rail_menu_popover_for");
    let new_topic = menu
        .split_once("MenuKind::NewTopic => {")
        .map(|(_, rest)| rest)
        .unwrap_or_else(|| panic!("the row menu's new topic item"))
        .split_once("MenuKind::GroupTopics =>")
        .map(|(item, _)| item)
        .unwrap_or_else(|| panic!("the new topic item ends at the next entry"));
    assert!(
        new_topic.contains("if this.open_picked_in_focused_pane(id, true, cx)"),
        "the row menu opens the conversation in the pane holding the caret"
    );
    assert!(
        !new_topic.contains("this.open_session(id, cx)"),
        "the row menu never moves the chat leaf"
    );
}

#[test]
fn the_chat_leaf_still_takes_the_pick_when_no_pane_has_the_caret() {
    // #2819 keeps the single pane window exactly as it was: with no other
    // leaf holding the caret, the chat leaf is the one that moves, and it
    // keeps whatever hand back the door asked for.
    let src = DASHBOARD;
    let helper = fn_body(src, "open_picked_in_focused_pane");
    let fallback = ["open_session_handing_back(id, ", "hand_back, cx)"].concat();
    assert!(
        helper.contains(fallback.as_str()),
        "the chat leaf path keeps the hand back it was given"
    );
    assert!(
        helper.contains("let Some(caret) = caret else {"),
        "the fallback is the no other pane case, not a guess"
    );
}

#[test]
fn a_committed_row_leaves_the_keys_in_the_chat_leaf() {
    // #2741: the chat leaf adopts the row's conversation, so a split that
    // kept the caret in a neighbouring pane typed into a conversation the
    // click did not open.
    let src = DASHBOARD;
    let body = fn_body(src, "open_session_handing_back");
    let after = body
        .split_once("self.rail_focused = false")
        .map(|(_, rest)| rest)
        .expect("a committed row lets the rail go");
    assert!(
        after.contains("self.pane_focus = None"),
        "the chat leaf is the pane that took the conversation"
    );
    assert!(
        after.contains("self.request_composer_caret(cx)"),
        "the caret follows the conversation"
    );
    assert!(
        after.contains("self.refocus_screen()"),
        "the owner is derived again after the hand back"
    );
}

#[test]
fn the_caret_is_repaired_from_the_window_not_the_owner() {
    // #2741: the owner does not change when a press lands on chrome, so a
    // reconciler that only acts on transitions never fires. The repair
    // reads the window instead.
    let src = DASHBOARD;
    let render = dashboard_render(src);
    assert!(
        render.contains("caret -> composer repaired now={}"),
        "a repaired caret leaves a receipt"
    );
    assert!(
        render.contains("KeyboardOwner::Composer"),
        "only the composer repairs, an overlay owns the keys on purpose"
    );
    assert!(
        render.contains(".is_none_or(|handle| handle == self.focus)"),
        "the root holding the keys is the drift this repairs"
    );
}

#[test]
fn an_overlay_never_leaves_the_caret_in_the_composer() {
    // #2767: the repair ran one way only. With the topic prompt open the
    // owner was the prompt, the composer kept the caret, so the name typed
    // into the field also landed in the message field and Enter submitted
    // the message instead of creating the topic.
    let src = DASHBOARD;
    let render = dashboard_render(src);
    assert!(
        render.contains("caret -> root repaired now={}"),
        "the other direction leaves a receipt too"
    );
    assert!(
        render.contains("let leaked = composer.read(cx).focus_handle.is_focused(window)"),
        "the drift is the composer holding keys it no longer owns"
    );
    assert!(
        render.contains("an overlay or a screen took the keys while the composer"),
        "the repair is a branch with two arms, never one"
    );
}

#[test]
fn a_send_is_refused_while_an_overlay_owns_the_keys() {
    // #2767: the second half. Even with the caret repaired, a send must
    // not reach the agent while a prompt is waiting for its own Enter.
    let src = DASHBOARD;
    let body = fn_body(src, "send_from_pane");
    assert!(
        body.contains("send refused"),
        "a refused send leaves a receipt naming the owner"
    );
    assert!(
        body.contains("self.open_overlays()"),
        "the refusal asks the one owner decision"
    );
}

#[test]
fn sidebar_actions_existing_toggle_uses_the_same_transition() {
    // The toggle moved to the rail module in the split (#2822). Read that
    // file alone: the whole dashboard also contains this test's own needle,
    // and `rsplit` picks the last occurrence, which would be the string
    // rather than the function.
    let source = include_str!("../dashboard_rail.rs");
    let body = source
        .split("fn toggle_rail")
        .nth(1)
        .and_then(|tail| tail.split("fn set_rail_open").next())
        .expect("toggle body");
    assert!(body.contains("self.set_rail_open(open, cx)"));
    assert!(!body.contains("self.rail_open = !self.rail_open"));
}

/// A drag-close takes the same transition as the toggle (#2845). The drag
/// used to write the flag directly, so a hidden filter, the open chip panel
/// and the keyboard focus all survived a rail that was no longer drawn, and
/// reopening with ctrl+s showed the stale filter.
#[test]
fn a_drag_close_takes_the_same_transition_as_the_toggle() {
    let src = DASHBOARD;
    // Assembled at run time: this module is part of the text it scans, and
    // a contiguous literal here would match itself.
    let direct = ["this.rail_open", "=", "false"].join(" ");
    let transition = concat!("this.set_rail_open(", "false, cx);");
    assert!(
        !src.contains(&direct),
        "a close path writes the rail flag directly instead of the transition"
    );
    assert!(
        src.contains(transition),
        "the drag close goes through set_rail_open"
    );
}
