//! The window draws its panes from a tree on the chat screen (#2438, #2472).
//! The window has no headless harness, so these checks read source. Each rule
//! below was broken once: a split that drew only inside the themes preview, a
//! second pane with no header, a divider that could not resize, a caret that
//! could not move between panes, and a split that could not split again.

const GUI_MAIN: &str = include_str!("../gui/main.rs");

/// The chat's width planning and its screen arm, from where the pane width is
/// read to the next screen. The share and the measure are planned above the
/// match so the chrome under the conversation aligns to one width (#2588), so
/// the region starts at the pane width rather than at the match.
fn chat_arm() -> &'static str {
    GUI_MAIN
        .split("let pane_width = attachment_view::content_width(")
        .nth(1)
        .expect("the chat reads the pane width in render")
        .split("Screen::Usage => {")
        .next()
        .expect("the chat arm comes before the usage arm")
}

/// The chat arm that wraps the window-level plan, queue and composer (#2530):
/// the copies that stand down once a split gives every pane its own.
fn composer_arm() -> &'static str {
    GUI_MAIN
        .split("right = match self.screen {")
        .nth(1)
        .expect("the composer match is built in render")
        .split("Screen::Usage => right.child(footer),")
        .next()
        .expect("the chat arm comes before the footer-only arms")
}

/// The key handler, from its opening to the next method on the window.
fn on_key() -> &'static str {
    GUI_MAIN
        .split("fn on_key(")
        .nth(1)
        .expect("on_key is defined")
        .split("\n    fn ")
        .next()
        .expect("the handler has a body")
}

/// The walk that turns the pane tree into elements (#2472).
fn pane_element() -> &'static str {
    GUI_MAIN
        .split("fn pane_element(")
        .nth(1)
        .expect("pane_element is defined")
        .split("fn split_pick_overlay(")
        .next()
        .expect("the walk comes before the pick overlay")
}

/// The body that draws the chat screen's pane tree (#2530): where each pane's
/// own plan, queue and composer are built before the walk paints them.
fn panes_body() -> &'static str {
    GUI_MAIN
        .split("fn panes_body(")
        .nth(1)
        .expect("panes_body is defined")
        .split("impl Render for Dashboard {")
        .next()
        .expect("the body closes before the window's render")
}

/// The dimmed composer an unfocused pane shows (#2530).
fn pane_composer_dim() -> &'static str {
    GUI_MAIN
        .split("fn pane_composer_dim(")
        .nth(1)
        .expect("pane_composer_dim is defined")
        .split("/// One pane's header")
        .next()
        .expect("the dim helper comes before the header")
}

/// Source with every run of whitespace folded to one space. A `rustfmt`
/// reflow then reads the same as the line it wrapped, so these checks pin
/// behaviour rather than column positions.
fn flat(src: &str) -> String {
    src.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn the_chat_screen_draws_the_panes() {
    assert!(
        chat_arm().contains("self.panes_body("),
        "the chat arm draws the tree through panes_body, not only the themes preview"
    );
    assert_eq!(
        GUI_MAIN.matches("self.panes_body(").count(),
        1,
        "only the chat arm draws the panes: the theme modal opens over it (#2619)"
    );
}

#[test]
fn a_split_widens_nothing_when_it_is_absent() {
    let chat = chat_arm();
    assert!(
        chat.contains(".and_then(|(tree, id)| tree.leaf_width_share(id))")
            && chat.contains(".unwrap_or(1.0);"),
        "the chat plans its rows against the share the tree gives it, and an untreed window gets all of it"
    );
    assert!(
        chat.contains("split_pane_width(pane_width, false, chat_share)"),
        "two panes share the content width through split_pane_width, which subtracts the gap and the handle"
    );
    assert!(
        GUI_MAIN.contains("if tree.leaves().len() < 2 {\n            return chat;"),
        "a tree of one leaf hands the chat back untouched, which is the unsplit window"
    );
}

#[test]
fn the_second_pane_is_composed_in_one_place() {
    let walk = pane_element();
    // Count definitions, not mentions: two other tests in this file quote the
    // signature as a literal, so a raw `matches` counted those too (#2646).
    assert_eq!(
        GUI_MAIN
            .lines()
            .filter(|line| line.starts_with("fn pane_element("))
            .count(),
        1,
        "one walk draws every pane, so a flat split and a nested one cannot drift"
    );
    assert!(
        flat(walk).contains("PaneTree::Leaf { session, .. } =>")
            && flat(walk).contains("PaneTree::Split { layout, children, .. } =>"),
        "the walk answers both shapes of node"
    );
    assert!(
        walk.contains("match ctx.states.get(session) {"),
        "a pane draws its own transcript from its own state, not the chat's"
    );
    assert!(
        walk.contains("let element = pane_element(child, ctx, path, child_width);"),
        "a split recurses, which is how a split can split again (#2472)"
    );
}

#[test]
fn both_panes_carry_a_header_and_a_close_button() {
    let walk = pane_element();
    // Only the second pane used to have a header, so one session read as a
    // named session and the other as a nameless strip (#2474).
    assert!(
        walk.contains("pane_header(") && walk.contains("format!(\"pane-close-{session}\")"),
        "every leaf builds its own header and its own close button"
    );
    assert!(
        GUI_MAIN.contains("fn pane_header(") && GUI_MAIN.contains("focused: bool,"),
        "one helper draws every header, so they cannot drift"
    );
    assert!(
        walk.contains("ctx.focus.is_none()") && walk.contains("ctx.focus == Some(*session)"),
        "the caret sits on the chat when no pane took it, and the accent follows it"
    );
}

#[test]
fn the_focused_pane_wears_a_band_under_its_header() {
    let header = GUI_MAIN
        .split("fn pane_header(")
        .nth(1)
        .expect("pane_header is defined")
        .split("\n}\n")
        .next()
        .expect("the helper has a body");
    // A one-pixel rule in the panel colour, plus a title that only shifted
    // hue, was invisible on both themes: Tab moved the caret and the window
    // looked unchanged (#2475).
    assert!(
        header.contains("let header_bg = if focused {"),
        "the focused header takes its own background"
    );
    assert!(
        header.contains(".bg(rgb(header_bg))"),
        "the header paints that background"
    );
    assert!(
        header.contains(".border_b_2()"),
        "the band under the header is thick enough to read"
    );
    assert!(
        header.contains("focus_ring_color(focused)"),
        "the unfocused header sits on the same contrast as the divider, and the live one on the accent"
    );
}

#[test]
fn choosing_a_session_repaints_before_the_transcript_loads() {
    // The pane itself is built by `open_pane_state`, which a split and a
    // restored layout both call, so the store and its repaint live there
    // (#2675). The ordering it has to keep is unchanged: paint the pane, then
    // load its transcript.
    let pane = GUI_MAIN
        .split("fn open_pane_state(")
        .nth(1)
        .expect("open_pane_state is defined");
    let stored = pane
        .find("self.pane_states.insert(")
        .expect("the picked pane is stored");
    let tail = pane
        .get(stored..)
        .expect("the pane is stored on a char boundary");
    let painted = tail
        .find("cx.notify();")
        .expect("storing the pane repaints the window");
    assert!(
        painted < tail.find("cx.spawn").unwrap_or(painted + 1),
        "the repaint happens before the transcript load is spawned"
    );
    let choose = GUI_MAIN
        .split("fn choose_split(")
        .nth(1)
        .expect("choose_split is defined");
    assert!(
        choose.contains(".is_some_and(|tree| tree.split(target, layout, id));"),
        "the pane the pick started from is the one that splits, which is what nests a split"
    );
}

#[test]
fn both_halves_take_their_share_of_the_width() {
    let walk = pane_element();
    // The chat used to be handed in bare, so it sized to its content and the
    // split read as a chat with a strip beside it (#2473). Each child of a
    // split now sits in a slot sized by its own share (#2472, #2476).
    assert!(
        walk.contains(".flex_basis(gpui::relative(child.share()))"),
        "each pane takes its own share of the row, so a drag can make one bigger"
    );
    assert!(
        walk.contains("format!(\"split-slot-{here:?}-{ix}\")"),
        "every share gets its own slot, so a drag addresses one box and not the window"
    );
    // The rule is what makes the boundary readable on a dark theme.
    assert!(
        walk.contains("split_divider(stacked, grab)"),
        "a visible rule sits between each pair, and the strip around it takes the drag"
    );
    assert!(
        GUI_MAIN.contains("const SPLIT_DIVIDER_PX: f32 = 2.0;"),
        "the rule has a named width, and one pixel of a panel tone was invisible"
    );
    assert!(
        GUI_MAIN.contains("fn split_rule_color() -> u32 {"),
        "the rule reads a colour that contrasts with the background"
    );
    assert!(
        !GUI_MAIN
            .split("fn split_divider(")
            .nth(1)
            .expect("split_divider is defined")
            .split("\n}\n")
            .next()
            .expect("the helper has a body")
            .contains("panel_hi"),
        "the rule is not drawn in a panel tone, which sits on the background"
    );
}

#[test]
fn a_stacked_pane_plans_against_the_whole_width() {
    let helper = GUI_MAIN
        .split("fn split_pane_width(")
        .nth(1)
        .expect("split_pane_width is defined")
        .split("\n}\n")
        .next()
        .expect("the helper has a body");
    assert!(
        helper.contains("if stacked {\n        content\n    }"),
        "stacked panes share the height, not the width, so a row gets all of it"
    );
    assert!(
        helper.contains("SPLIT_GAP_PX * 2.0") && helper.contains("SPLIT_HANDLE_PX"),
        "side by side the halves subtract the gap either side and the handle"
    );
}

/// #2476: each pane is sized by a share the drag moves, not by an even flex,
/// so resizing is possible at all and the default still splits 50/50.
#[test]
fn the_divider_resizes_both_panes() {
    let walk = pane_element();
    assert!(
        walk.contains(".on_children_prepainted(move |bounds, _window, _cx| {"),
        "the pane area is recorded at paint time, so the drag maps onto the real boxes"
    );
    assert!(
        walk.contains(".insert(key.clone(), [start_x, start_y, end_x, end_y]);"),
        "each split records its own box under its path, so a nested rule maps onto its pane"
    );
    assert!(
        GUI_MAIN.contains("self.split_drag = Some(DividerDrag {"),
        "pressing the rule starts a drag that remembers its path and its divider"
    );
    assert!(
        GUI_MAIN.contains("tree.drag_divider(&drag.path, drag.divider, travelled, span);"),
        "the move lands on that split's own pair, measured against its own span"
    );
    // The reset is answered before the borrow that starts the drag, so the
    // even pair can be written down for the next launch (#2675).
    assert!(
        GUI_MAIN.contains("tree.even_divider(path, divider)"),
        "a double click puts the pair back to even"
    );
    assert!(
        GUI_MAIN.contains("const SPLIT_HANDLE_PX: f32 = 12.0;"),
        "the rule has a grabbable strip, since a 2px target cannot be hit reliably"
    );
}

#[test]
fn alt_tab_rotates_the_panes_and_bare_tab_does_not() {
    let keys = on_key();
    // A split opens holding the caret, and the chord is the only way to move
    // it: bare Tab belongs to the emoji picker, the follow-up suggestion and
    // the rail (#2580, #2579).
    let rotate = keys
        .find("if let Some(rotation) = rotation_chord(ks) {")
        .expect("the window answers the rotation chord");
    let rail = keys
        .find("self.rail_focused = !self.rail_focused;")
        .expect("Tab still walks to the rail");
    assert!(
        rotate < rail,
        "the chord is read before the rail, so it works from the composer too"
    );
    assert!(
        !keys.contains("self.cycle_pane_focus(cx);"),
        "bare Tab no longer cycles the panes"
    );
    let arm = keys
        .split("if let Some(rotation) = rotation_chord(ks) {")
        .nth(1)
        .expect("the rotation arm has a body")
        .split("return;")
        .next()
        .expect("the arm returns once it has moved the caret");
    assert!(
        flat(arm)
            .contains("self.cycle_pane_focus(rotation == split::PaneRotation::Clockwise, cx);"),
        "the arm hands the direction to the caret instead of always going forward"
    );
    let helper = GUI_MAIN
        .split("fn rotation_chord(")
        .nth(1)
        .expect("the chord is read in one place")
        .split("\n}\n")
        .next()
        .expect("the helper has a body");
    assert!(
        flat(helper).contains("ks.modifiers.alt,")
            && flat(helper).contains("ks.modifiers.control,")
            && flat(helper).contains("ks.modifiers.platform,")
            && flat(helper).contains("ks.modifiers.shift,"),
        "alt is required and the other modifiers are handed over so they can be refused"
    );
    let cycle_fn = GUI_MAIN
        .split("fn cycle_pane_focus(")
        .nth(1)
        .expect("cycle_pane_focus is defined")
        .split("\n    fn ")
        .next()
        .expect("the caret fn has a body");
    // The slice starts after the name, so the parameters are checked as the
    // remainder rather than with the prefix this split just consumed.
    assert!(
        cycle_fn.starts_with("&mut self, clockwise: bool, cx: &mut Context<Self>) {"),
        "the caret knows which way to walk"
    );
    assert!(
        flat(cycle_fn).contains("split::rotated_index(at, leaves.len(), rotation)"),
        "the wraparound lives in the core, where it is tested without a window"
    );
    assert!(
        cycle_fn.contains("self.pane_focus = if Some(next) == self.transcript.session {"),
        "the chat's own leaf is the pane the caret reaches as None"
    );
}

#[test]
fn the_page_keys_scroll_the_pane_holding_the_caret() {
    let keys = flat(on_key());
    // A read-only transcript holds the caret by taking the page keys. Without
    // this the second pane could be focused and still not scroll (#2475).
    assert!(
        keys.contains(".filter(|id| self.pane_states.contains_key(id));"),
        "the page keys follow the caret into whichever pane holds it"
    );
    assert!(
        keys.contains("let (x, y, max) = match target.and_then(|id| self.pane_states.get(&id)) {"),
        "the caret's pane is the one the page keys move"
    );
    assert!(
        keys.contains("let offset = self.transcript_scroll.offset();"),
        "with no pane focused the page keys scroll the chat"
    );
}

#[test]
fn ctrl_x_closes_the_pane_holding_the_caret() {
    let keys = on_key();
    let arm = flat(
        keys.split("ctrl+x closes the pane holding the caret")
            .nth(1)
            .expect("the chord is handled")
            .split("return;")
            .next()
            .expect("the arm returns"),
    );
    assert!(
        arm.contains("&& ks.key == \"x\"")
            && arm.contains(".is_some_and(|tree| tree.leaves().len() > 1)")
            && arm.contains("self.close_focused_pane(cx);"),
        "ctrl+x closes the pane holding the caret, and only while there are panes to close"
    );
    let close = GUI_MAIN
        .split("fn close_pane(")
        .nth(1)
        .expect("close_pane is defined")
        .split("fn close_focused_pane(")
        .next()
        .expect("close_pane comes before its chord wrapper");
    assert!(
        close.contains("let Some(absorbed) = tree.close(session) else {"),
        "the closed pane's share goes to its neighbour through the tree"
    );
    assert!(
        close.contains("self.open_session(absorbed, cx);"),
        "closing the chat hands the window to the survivor rather than dropping it"
    );
    assert!(
        close.contains("self.pane_focus = None;"),
        "focus returns to the chat leaf, so the next chord lands somewhere known"
    );
}

/// #2471: the chord has to reach the reader in a shape the window can send.
/// The handler used to demand control and shift together, which no keystroke
/// on macOS satisfies for `|` because shift is folded into the glyph.
#[test]
fn the_chord_is_read_through_one_place() {
    assert!(
        GUI_MAIN.contains("fn split_chord(ks: &gpui::Keystroke)"),
        "the window reads the chord in one place, not inline at the call site"
    );
    assert!(
        GUI_MAIN.contains("if let Some(layout) = split_chord(ks)"),
        "opening a split goes through that reader"
    );
}

#[test]
fn each_pane_owns_composer_plan_and_queue() {
    let body = flat(panes_body());
    assert!(
        body.contains("chrome.insert(*session, col.into_any_element())"),
        "each leaf gets a chrome column of its own"
    );
    assert!(
        body.contains("agent_bridge::queue_snapshot(*session)"),
        "the queue is read from that pane's session, not the window's"
    );
    assert!(
        body.contains("pane.plan.refresh()"),
        "each pane's plan strip is re-read for its own session"
    );
    assert!(
        body.contains("pane_composer_dim(draft, *session, is_chat, cx.entity().downgrade())"),
        "an unfocused pane shows the draft it holds, dimmed"
    );
}

#[test]
fn the_pane_holding_the_keyboard_draws_the_real_composer() {
    let body = flat(panes_body());
    let composer = body
        .split("let composer: AnyElement = if focused {")
        .nth(1)
        .expect("the composer picks one pane to draw live");
    let composer = composer
        .split("col = col.child(composer);")
        .next()
        .expect("the composer is placed in the pane's column");
    assert!(
        composer.contains("cell.child(editor).into_any_element()"),
        "the focused pane draws the live editor entity"
    );
    assert!(
        composer.contains("pane_composer_dim(draft, *session, is_chat, cx.entity().downgrade())"),
        "every other pane shows the draft it holds, dimmed"
    );
    let live_at = composer
        .find("cell.child(editor)")
        .expect("the live composer");
    let dim_at = composer.find("pane_composer_dim(").expect("the dim copy");
    assert!(
        live_at < dim_at,
        "focus chooses the live composer; the dim one is the fallback"
    );
}

#[test]
fn the_window_composer_stands_down_once_the_window_is_split() {
    let arm = flat(composer_arm());
    let gated = arm
        .split("if !self.window_is_split() {")
        .nth(1)
        .expect("the window chrome is gated on the split");
    let gated = gated
        .split("if let Some(strip) = topic_bottom")
        .next()
        .expect("the gate closes before the topic strip");
    // The gate keeps the measure wrapper inside it (#2594): the unsplit
    // window still aligns the conversation to one width, and the split leaves
    // stand down. `flat` folds the reflow, so this pins the wrapper itself
    // rather than the column the editor landed on.
    assert!(
        gated.contains("central( self.editor.clone()"),
        "the window's composer sits inside the gate, still on the chat measure"
    );
    assert!(
        gated.contains(".editing_view(actions.as_ref())")
            && gated.contains(".or_else(|| self.plan.view())"),
        "so does the window's plan strip: the Editing approval card draws \
         first (#2755) and the running view falls back"
    );
    assert!(
        gated.contains("if let Some(plan_view) = card"),
        "and the strip that wins still lands inside the measure wrapper"
    );
}

#[test]
fn an_unfocused_composer_is_dimmed_and_hands_the_keyboard_back() {
    let dim = flat(pane_composer_dim());
    assert!(
        dim.contains(".opacity(0.55)"),
        "an unfocused composer is disabled, not hidden"
    );
    assert!(
        dim.contains("this.pane_focus = if is_chat { None } else { Some(session) }"),
        "clicking it moves the keyboard to that pane"
    );
}

#[test]
fn a_leaf_draws_its_chrome_above_its_own_footer() {
    let walk = flat(pane_element());
    let chrome_at = walk
        .find("if let Some(chrome) = ctx.chrome.remove(session)")
        .expect("the leaf takes its chrome");
    let footer_at = walk
        .find("node.child(pane_footer(")
        .expect("the leaf draws its footer");
    assert!(
        chrome_at < footer_at,
        "the composer and plan sit above the footer row"
    );
}

/// The drop listener that turns a Finder drag into composer chips (#2610).
fn drop_handler() -> &'static str {
    GUI_MAIN
        .split(".on_drop::<ExternalPaths>(")
        .nth(1)
        .expect("the window takes drops")
        .split(".on_mouse_up(")
        .next()
        .expect("the drop comes before the mouse-up handler")
}

/// The rotation, from its signature to the next method on the window.
fn cycle_pane_focus() -> &'static str {
    GUI_MAIN
        .split("fn cycle_pane_focus(")
        .nth(1)
        .expect("the rotation is defined")
        .split("\n    fn ")
        .next()
        .expect("the rotation has a body")
}

#[test]
fn a_dropped_file_belongs_to_the_focused_pane() {
    let drop = flat(drop_handler());
    assert!(
        drop.contains("this.composer_entity()"),
        "the chip goes to the composer the keyboard owns, not the window's"
    );
    assert!(
        !drop.contains("this.editor.update"),
        "the window's editor is the chat leaf's field, which a split may not draw"
    );
}

#[test]
fn the_keyboard_follows_the_caret_between_panes() {
    let cycle = flat(cycle_pane_focus());
    assert!(
        cycle.contains("self.refocus_screen();"),
        "rotating the caret moves the keyboard with it, or the next chord lands in the old field"
    );
    let composer_target = flat(
        GUI_MAIN
            .split("FocusTarget::Composer => {")
            .nth(1)
            .expect("the window aims the composer")
            .split("FocusTarget::Root")
            .next()
            .expect("the root arm follows the composer arm"),
    );
    assert!(
        composer_target.contains("self.composer_entity()"),
        "the composer target resolves the focused pane's own field"
    );
}

#[test]
fn a_pane_wears_a_ring_and_the_unfocused_ones_are_drawn_back() {
    let walk = flat(pane_element());
    assert!(
        walk.contains("border_color(rgb(focus_ring_color(focused)))"),
        "the leaf that owns the keyboard wears the accent ring"
    );
    assert!(
        walk.contains("UNFOCUSED_PANE_OPACITY"),
        "a pane that does not own the keyboard reads as disabled"
    );
    assert!(
        walk.contains("this.refocus_screen();"),
        "clicking a pane takes the keyboard, not only the dim composer"
    );
}

#[test]
fn keystrokes_are_not_written_to_the_log() {
    // The owner removed per-keystroke logging entirely (#2693): the daily log
    // and the cargo-run terminal are not a keystroke recorder. Targeted key
    // diagnostics are added ad hoc when a key is actually being debugged, so
    // this pins the flood line itself, not all logging in the handler.
    assert!(
        !flat(on_key()).contains("key {:?} screen="),
        "the window must not write one log line per keystroke (#2693)"
    );
}

#[test]
fn a_pane_send_stays_in_its_own_leaf() {
    let send = flat(
        GUI_MAIN
            .split("fn send_from_pane(")
            .nth(1)
            .expect("the pane send is defined")
            .split("\n    fn ")
            .next()
            .expect("the send has a body"),
    );
    assert!(
        !send.contains("open_session("),
        "a send must not adopt the window's chat: that closes a leaf and the pane vanishes (#2614)"
    );
    assert!(
        send.contains("pane.busy = true"),
        "the panel that was typed in is the one that runs the turn"
    );
    // The call is reflowed across lines by rustfmt, so the argument list is
    // matched as text rather than as one unbroken line.
    assert!(
        send.contains("spawn_turn(") && send.contains("bridge.service.clone(), session,"),
        "the turn runs against the pane's session, not the window's"
    );
}

#[test]
fn a_pane_turn_settles_its_own_leaf() {
    let poll = flat(
        GUI_MAIN
            .split("fn poll(")
            .nth(1)
            .expect("the poll loop is defined")
            .split("\n    fn ")
            .next()
            .expect("the poll loop has a body"),
    );
    assert!(
        poll.contains("self.settle_pane(session, tokens_per_second, cx);"),
        "a finished pane turn settles that pane, so the window's conversation never moves"
    );
    assert!(
        poll.contains("self.fold_pane_progress("),
        "a pane's stream paints into its own card"
    );
    let walk = flat(pane_element());
    assert!(
        walk.contains("&pane.live,"),
        "the leaf draws its own live turn instead of an empty one"
    );
}
