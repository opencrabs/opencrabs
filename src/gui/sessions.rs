//! The session rail: every real session from the ledger, most recent first,
//! click to select. Phase 0's lessons are baked in: the fetch runs on a
//! dedicated 4 MB thread (gpui's dispatch-queue workers have tiny stacks and
//! the crash receipt lives in `main.rs`), the DB opens through
//! `Database::connect`, the same no-migration read-only door the usage
//! loader uses, and no string is ever sliced by byte index (#2271): the
//! title truncation walks chars.
//!
//! The split between `row` (pure data) and the click attach (the view's
//! listener) exists because `Context::listener` needs `&mut cx`: the view
//! builds each row's handler in its own loop turn, then hands the finished
//! rows to `rail`.

use crate::{icon, palette, platform_icons};
use std::collections::{HashMap, HashSet};

use gpui::{
    AnyElement, App, ClickEvent, Div, Image, ImageFormat, InteractiveElement, ObjectFit,
    ScrollHandle, SharedString, Stateful, Styled, TextRun, Window, div, img, linear_color_stop,
    linear_gradient, prelude::*, rems, rgb,
};
use std::sync::Arc;

use opencrabs::config::gui_theme;
use opencrabs::db::Database;
use opencrabs::db::models::Session;
use opencrabs::db::repository::session::{
    SessionKind, SessionListOptions, SessionRepository, classify_session_title,
};
use opencrabs::db::repository::session_favorites::SessionFavoritesRepository;
use opencrabs::db::repository::{ContentSearchRow, MessageRepository};
use opencrabs::ui_core::rail;

// The menu primitives moved to `menu.rs` (#2613) so the transcript can draw
// its own context menu without depending on the session rail. Re-exported
// here because every rail call site still says `sessions::menu_*`.
pub use crate::menu::{MenuEntry, MenuKind, menu_item, menu_popover};

/// The rail's live state, owned by the window's view.
#[derive(Default)]
pub struct SessionsState {
    /// Most recent first, straight from `SessionRepository::list`.
    pub items: Vec<Session>,
    /// The session a click chose; the transcript (task 3) reads it.
    pub selected: Option<uuid::Uuid>,
    /// The row the keyboard is on. Distinct from `selected`: that is the
    /// chat whose transcript is open, this is the row an arrow is pointing
    /// at, the same split the TUI keeps between its list index and the open
    /// chat (#2351).
    pub cursor: Option<uuid::Uuid>,
    /// The row whose context menu is open. Right-click asked for it; a
    /// left click anywhere, an esc, or a pick closes it. The menu anchors
    /// to the row, not the pointer: the row is the stable thing inside a
    /// scrolling rail, and the slide already behaves this way.
    pub rail_menu: Option<uuid::Uuid>,
    /// Set when the fetch failed; the rail says why instead of lying empty.
    pub error: Option<SharedString>,
    /// ctrl+f's query. A non-empty query narrows the rail to the chats whose
    /// title (or id prefix) matches; the ledger is untouched, so clearing the
    /// query shows everything back exactly as it was.
    pub filter: String,
    /// Whether the field is taking keystrokes. The bar is only drawn while it
    /// is, so an unsearched rail looks the way it always did.
    pub filter_active: bool,
    /// Pinned sessions, straight from the `session_favorites` sidecar the TUI
    /// writes (`SessionService::favorite_session`). Same table, same ids, so
    /// pinning in one surface shows up in the other on its next fetch. A set,
    /// because every rail row asks "is this one pinned?" once per frame and
    /// the sidecar is keyed by session id: the same shape it is stored in.
    pub favorites: HashSet<uuid::Uuid>,
    /// Sessions with topics turned on (#2302). Kept beside the rows for the
    /// same reason as favorites: the row model stays as it is.
    pub topics: HashSet<uuid::Uuid>,
    /// The icon each topic carries, shown before its name (#2414).
    pub topic_icons: HashMap<uuid::Uuid, String>,
    /// The live name each Telegram topic carries, resolved once per refresh
    /// pass in ONE query (#2490).
    ///
    /// Keyed by session id, so a row looks its title up with no allocation and
    /// no database call inside the paint. Rows that are not topic-bound
    /// Telegram sessions simply have no entry and keep their stored name.
    pub topic_titles: HashMap<uuid::Uuid, String>,
    /// Topic id to its parent session id (#2303). A topic is grouped under
    /// its parent in the rail; the row model itself is unchanged.
    pub parents: HashMap<uuid::Uuid, uuid::Uuid>,
    /// Closed sessions (#2305): drawn dimmed and refuse new turns.
    pub closed: HashSet<uuid::Uuid>,
    /// One already-formatted preview line per session, for the card's third
    /// line. Filled by one batched read on a rail refresh, never inside a
    /// paint: the rail repaints on a shared clock, so a per-row query would
    /// be a query per card per frame. Formatted once here so the paint only
    /// clones a `SharedString`.
    pub previews: HashMap<uuid::Uuid, SharedString>,
    /// The rail filter menu (#2719): a plan-state chip plus whether the
    /// automation sessions (cron + RSI) show. It lives on the state so it
    /// survives every repaint, and resets to its default when the rail closes.
    pub rail_filter: RailFilter,
    /// Whether the filter chip row is open (#2787). The funnel button toggles
    /// it; the chips themselves are unchanged. Closed by default, and it
    /// resets with the filter when the rail closes, so a reopened rail never
    /// shows a stale panel.
    pub rail_filter_open: bool,
    /// Plan checklist progress `(done, total)` for every session, snapshotted
    /// once per paint before `visible()` filters. The plan chip decides each
    /// row from this map, so the filter never reads a plan file inside the
    /// row loop and never chases the rows it is about to hide (#2719).
    pub plan_progress: HashMap<uuid::Uuid, (usize, usize)>,
    /// ctrl+f's deep hits (#2720): session id to the snippet centered on the
    /// query, from the debounced content search. A row whose title misses the
    /// query still shows when its messages match, and the snippet stands in
    /// for the preview so the reader sees why. Empty whenever the field is
    /// empty or closed, so an unsearched rail is untouched.
    pub content_matches: HashMap<uuid::Uuid, String>,
}

/// One topic chip waiting for its card (#2853): the id the click opens, the
/// label it prints, the ledger stamp it sorts by, and whether work is running
/// in it right now. Built where the chips are built so the strip can be
/// ordered by the same facts the chip itself paints.
pub struct RailChip {
    pub id: uuid::Uuid,
    pub title: String,
    pub at: chrono::DateTime<chrono::Utc>,
    pub busy: bool,
}

/// The instant one chip sorts by (#2853). A topic with work in flight is
/// pinned to `now`, the paint's one clock, so the chips the reader can see
/// working lead the strip and two of them tie instead of reshuffling on every
/// frame. Everything else sorts by its own stamp, the clock the card's title
/// folds over (#2815), so the strip and the stamp can never disagree.
pub fn chip_order_key(
    chip: &RailChip,
    now: chrono::DateTime<chrono::Utc>,
) -> std::cmp::Reverse<chrono::DateTime<chrono::Utc>> {
    std::cmp::Reverse(if chip.busy { now } else { chip.at })
}

impl SessionsState {
    /// The rows the rail shows right now, in ledger order. Three gates: the
    /// ctrl+f query (`matches` on the title, or a deep content hit), and the
    /// filter menu (`rail_filter_allows`). The menu reads the per-paint plan
    /// snapshot, never the index, so a row hidden by the plan chip still had
    /// its progress resolved before the filter ran (#2719). A content hit
    /// keeps a row visible even when its title misses the query (#2720).
    pub fn visible(&self) -> impl Iterator<Item = &Session> {
        self.items.iter().filter(|s| {
            (matches(s, &self.filter) || self.content_matches.contains_key(&s.id))
                && rail_filter_allows(
                    &self.rail_filter,
                    s.title.as_deref().unwrap_or(""),
                    self.plan_progress.get(&s.id).copied(),
                )
        })
    }

    /// The instant a card sorts by (#2815): the same grouped stamp it paints,
    /// so the rail can never order rows by a different clock than it prints.
    /// Every card's grouped stamp in one pass (#2817): the sort and the card
    /// both ask this per row, and answering it per row scanned the whole rail
    /// each time.
    pub fn grouped_activities(&self) -> HashMap<uuid::Uuid, chrono::DateTime<chrono::Utc>> {
        let mut stamps: HashMap<uuid::Uuid, chrono::DateTime<chrono::Utc>> =
            self.items.iter().map(|s| (s.id, s.updated_at)).collect();
        for s in &self.items {
            if let Some(parent) = self.parents.get(&s.id)
                && let Some(at) = stamps.get_mut(parent)
            {
                *at = (*at).max(s.updated_at);
            }
        }
        stamps
    }

    /// Every card's grouped spend in one pass (#2817), for the same reason.
    pub fn grouped_costs(&self) -> HashMap<uuid::Uuid, f64> {
        let mut costs: HashMap<uuid::Uuid, f64> =
            self.items.iter().map(|s| (s.id, s.total_cost)).collect();
        for s in &self.items {
            if let Some(parent) = self.parents.get(&s.id)
                && let Some(total) = costs.get_mut(parent)
            {
                *total += s.total_cost;
            }
        }
        costs
    }

    /// The rows that indent this frame (#2817): a topic whose parent is in
    /// view. Resolved once per frame instead of scanning the visible set per
    /// row.
    pub fn indented_ids(&self) -> HashSet<uuid::Uuid> {
        let visible: HashSet<uuid::Uuid> = self.visible().map(|s| s.id).collect();
        // The parent must be in view, not the topic itself: a topic whose chat
        // the filter hid has no card to sit under, exactly as before (#2817).
        self.parents
            .iter()
            .filter(|(_, parent)| visible.contains(parent))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Pinned first, then everything else, each group newest activity first.
    /// The key is the same grouped stamp the card paints (#2815): the parent's
    /// own row folded together with its topics, because a message is written
    /// to the session it was addressed to and the ledger's order therefore
    /// reads the parent alone. `sort_by_key` is stable, so equal stamps keep
    /// the ledger's order.
    ///
    /// A topic sits directly under its parent when that parent is in view
    /// (#2303). A pinned topic stays a chip under its parent: the pin orders
    /// top-level rows only, it never ejects a topic to the top level (#2701).
    pub fn ordered(&self) -> impl Iterator<Item = &Session> {
        let visible: Vec<&Session> = self.visible().collect();
        // Membership once, not once per row (#2817): `in_view` used to scan
        // the whole visible set for every session, and the topic pass scanned
        // it again for every parent, which is what made a 200-row rail cost
        // ~700k item visits per call.
        let visible_ids: HashSet<uuid::Uuid> = visible.iter().map(|s| s.id).collect();
        // The sort key, grouped over each card's topics, computed once per
        // session. `sort_by_key` re-evaluates its key on every comparison and
        // `grouped_activity` scans every item, so the sort alone was
        // O(n^2 log n). Folded over `items`, not `visible`, so the order reads
        // the same stamp the card prints.
        let activity = self.grouped_activities();
        let grouped = |s: &Session| {
            self.parents
                .get(&s.id)
                .is_some_and(|p| visible_ids.contains(p))
        };
        let (mut pinned, mut rest): (Vec<&Session>, Vec<&Session>) = visible
            .iter()
            .copied()
            .filter(|s| !grouped(s))
            .partition(|s| self.is_favorite(s.id));
        let key =
            |s: &&Session| std::cmp::Reverse(activity.get(&s.id).copied().unwrap_or(s.updated_at));
        pinned.sort_by_cached_key(key);
        rest.sort_by_cached_key(key);
        pinned.extend(rest);
        // Parent -> children in one pass, in the ledger's own order, instead
        // of a scan of the whole rail per parent.
        let mut children: HashMap<uuid::Uuid, Vec<&Session>> = HashMap::new();
        for s in &visible {
            if grouped(s)
                && let Some(parent) = self.parents.get(&s.id)
            {
                children.entry(*parent).or_default().push(*s);
            }
        }
        let mut out: Vec<&Session> = Vec::with_capacity(visible.len());
        for top in pinned {
            out.push(top);
            // Topic pins are always visible beneath their parent. The arrow
            // keys walk this exact order, so no cursor can target a hidden row.
            if let Some(kids) = children.get(&top.id) {
                out.extend(kids.iter().copied());
            }
        }
        out.into_iter()
    }

    pub fn is_closed(&self, id: uuid::Uuid) -> bool {
        self.closed.contains(&id)
    }

    /// Whether this session is pinned right now.
    pub fn is_favorite(&self, id: uuid::Uuid) -> bool {
        self.favorites.contains(&id)
    }

    pub fn is_topic_enabled(&self, id: uuid::Uuid) -> bool {
        self.topics.contains(&id)
    }

    /// Whether `root` still has at least one live topic under it (#2863).
    ///
    /// The flag alone is not the answer. A session can carry `topics_enabled`
    /// with every topic archived or folded, and the strip then drew a bar
    /// whose only chip was `All`. `items` holds live rows only, so a root with
    /// no child in it has no family to draw.
    pub fn has_live_topics(&self, root: uuid::Uuid) -> bool {
        self.items
            .iter()
            .any(|s| self.parents.get(&s.id) == Some(&root))
    }

    /// Move the cursor one row down (positive delta) or up (negative),
    /// walking the same pinned-first order the rail paints and clamping at
    /// both ends. An unarmed cursor starts at the first row either way, so
    /// the first press always lands on a real row.
    pub fn cursor_step(&self, current: Option<uuid::Uuid>, delta: isize) -> Option<uuid::Uuid> {
        let ids: Vec<uuid::Uuid> = self.ordered().map(|s| s.id).collect();
        let last = ids.len().checked_sub(1)?;
        let at = current
            .and_then(|c| ids.iter().position(|&id| id == c))
            .map(|p| p as isize)
            .unwrap_or(if delta >= 0 { -1 } else { 0 });
        let next = (at + delta).clamp(0, last as isize) as usize;
        Some(ids[next])
    }

    /// The row's index in the painted order, for the scroll handle's
    /// scroll_to_item. None when the row is not shown right now.
    pub fn cursor_index(&self, id: uuid::Uuid) -> Option<usize> {
        self.ordered().position(|s| s.id == id)
    }

    /// Whether the cursor points at a row the rail still shows. A deleted,
    /// filtered-out or refetched-away row must never stay armed.
    pub fn cursor_visible(&self) -> bool {
        self.cursor
            .is_some_and(|c| self.visible().any(|s| s.id == c))
    }

    /// What a delete chord acts on: the row the keyboard is pointed at, or
    /// the chat that is open when nothing is armed. Resolving the target is
    /// all this does. Nothing erases here: the caller asks the dialog, and
    /// only its confirm button reaches the delete bridge (#2352).
    pub fn delete_target(&self) -> Option<uuid::Uuid> {
        self.rail_target()
    }

    /// The row a rail chord acts on: the cursor row while the keyboard is
    /// on one, else the chat that is open. Chords share this resolution
    /// instead of each spelling it out (#2352, #2353).
    pub fn rail_target(&self) -> Option<uuid::Uuid> {
        self.cursor.or(self.selected)
    }

    /// What the pin chord acts on, resolved the same way the delete chord
    /// resolves its target (#2353).
    pub fn pin_target(&self) -> Option<uuid::Uuid> {
        self.rail_target()
    }

    /// Which conversation owns a parent card's activity/preview line (#2804).
    /// A selected child takes its own parent's line. Cursor is deliberately not
    /// an input, and a selection owned by another card leaves this parent alone.
    pub fn progress_id(
        &self,
        parent: uuid::Uuid,
        children: impl IntoIterator<Item = uuid::Uuid>,
    ) -> uuid::Uuid {
        self.selected
            .filter(|selected| children.into_iter().any(|child| child == *selected))
            .unwrap_or(parent)
    }

    /// What Enter in the find field opens: the armed row while it still
    /// matches the query, else the top match. Resolving it here keeps the
    /// rule out of the window's key handler, so it is tested without one
    /// (#2454).
    pub fn find_target(&self) -> Option<uuid::Uuid> {
        self.cursor
            .filter(|_| self.cursor_visible())
            .or_else(|| self.cursor_step(None, 1))
    }

    /// Dismiss the context menu. Deliberately the only thing it touches:
    /// a dismissal is an eraser, never an action, so no favorite flips
    /// and no delete arms on the way out (#2354).
    pub fn close_rail_menu(&mut self) {
        self.rail_menu = None;
    }

    /// How many of the ledger's sessions match the query.
    pub fn matched(&self) -> usize {
        self.visible().count()
    }
}

/// Case-insensitive title match, plus an id prefix so a pasted id finds its
/// row. An empty or whitespace query matches everything: an unsearched rail
/// and a cleared field take the same path.
pub fn matches(s: &Session, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    if let Some(title) = s.title.as_deref()
        && title.to_lowercase().contains(&q)
    {
        return true;
    }
    s.id.to_string().starts_with(&q)
}

/// One session the ctrl+f field found, and why (#2720). A title-only match
/// carries no snippet (the row's own preview stands); a content match carries
/// the snippet centered on the query so the reader sees the hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindMatch {
    pub session_id: uuid::Uuid,
    pub snippet: Option<String>,
}

/// Merge title matches with content-search rows into one deduped list (#2720).
/// Title matches keep the ledger's order and come first; content rows add the
/// sessions the title missed, newest-first, each with its snippet. A session
/// found both ways appears once, and the content snippet wins so a non-obvious
/// title still shows why it matched. Pure: no database, no window, so the
/// merge and its dedup are testable on their own.
pub fn merge_find_results(
    title_matches: Vec<uuid::Uuid>,
    content: &[ContentSearchRow],
) -> Vec<FindMatch> {
    let snippets: HashMap<uuid::Uuid, &str> = content
        .iter()
        .map(|row| (row.session_id, row.snippet.as_str()))
        .collect();
    let mut seen: HashSet<uuid::Uuid> = HashSet::new();
    let mut out: Vec<FindMatch> = Vec::new();
    for id in title_matches {
        if seen.insert(id) {
            out.push(FindMatch {
                session_id: id,
                snippet: snippets.get(&id).map(|s| s.to_string()),
            });
        }
    }
    for row in content {
        if seen.insert(row.session_id) {
            out.push(FindMatch {
                session_id: row.session_id,
                snippet: Some(row.snippet.clone()),
            });
        }
    }
    out
}

/// Which plan-state chip the rail filter menu has selected (#2719). The
/// progress counts come from the same plan index the card bar reads, so a
/// row's chip and its bar never disagree about the same plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlanFilter {
    /// Every session, plan or not.
    #[default]
    All,
    /// An approved checklist with work left: done < total.
    Active,
    /// An approved checklist that is complete: done == total, total > 0.
    Done,
    /// No approved checklist at all (no plan, or a draft with no tasks).
    NoPlan,
}

/// The rail's filter menu state (#2719): a plan-state chip plus whether the
/// automation sessions (cron + RSI) show. The default hides automation per
/// A1 and leaves the plan chip on All, so an untouched rail reads the way it
/// always did minus the background noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RailFilter {
    pub plan: PlanFilter,
    pub show_automation: bool,
}

impl Default for RailFilter {
    fn default() -> Self {
        Self {
            plan: PlanFilter::All,
            show_automation: false,
        }
    }
}

/// Whether the rail filter deviates from its default (#2787). The funnel
/// button wears the accent while this is true, so a filter that hides rows
/// is never invisible behind a closed panel.
pub fn filter_is_active(filter: &RailFilter) -> bool {
    filter.plan != PlanFilter::All || filter.show_automation
}

/// Whether one row passes the rail filter menu (#2719). Pure: the title's
/// kind comes from the core classifier and the plan progress from the
/// per-paint snapshot, so the same inputs always give the same answer and
/// the rule is testable without a window. Automation (cron + RSI) is hidden
/// unless the toggle is on; the plan chip then narrows what remains. A
/// subagent is not automation for this toggle: it stays visible.
pub fn rail_filter_allows(filter: &RailFilter, title: &str, plan: Option<(usize, usize)>) -> bool {
    if !filter.show_automation
        && matches!(
            classify_session_title(title),
            SessionKind::Cron | SessionKind::Rsi
        )
    {
        return false;
    }
    match filter.plan {
        PlanFilter::All => true,
        PlanFilter::Active => plan.is_some_and(|(done, total)| done < total),
        PlanFilter::Done => plan.is_some_and(|(done, total)| total > 0 && done == total),
        PlanFilter::NoPlan => plan.is_none(),
    }
}

/// How far left a rail row must be slid before its action strip opens, in
/// px. Below this a click with an unsteady hand stays a click, and a slide
/// that comes up short snaps back to where the row was.
pub const SLIDE_REVEAL_PX: f32 = 36.0;

/// The slide's whole rule, as arithmetic, so it can be tested without a
/// window: a drag that started at `from_x` and is now at `to_x` has travelled
/// far enough left when the travel is at least `SLIDE_REVEAL_PX`. Only
/// leftward travel counts; a drag to the right never opens anything.
pub fn slide_reveals(from_x: f32, to_x: f32) -> bool {
    to_x - from_x <= -SLIDE_REVEAL_PX
}

/// ctrl+f's field, drawn under the rail's header while it holds the keyboard.
/// The match count rides on the right so a query that found nothing says so
/// before Enter is pressed on it.
pub fn filter_bar(state: &SessionsState) -> Stateful<Div> {
    let empty = state.filter.trim().is_empty();
    let body = if state.filter.is_empty() {
        "find a chat\u{2026}".to_string()
    } else {
        state.filter.clone()
    };
    let count = if empty {
        String::new()
    } else {
        format!("{}/{}", state.matched(), state.items.len())
    };
    div()
        .id("session-filter")
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_1()
        .px_2()
        .py_1()
        .rounded_sm()
        .bg(rgb(palette::current().panel_hi))
        .border_1()
        .border_color(rgb(palette::current().focus))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(rgb(if empty {
                    palette::current().text_faint
                } else {
                    palette::current().text
                }))
                .child(SharedString::from(body)),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_hint))
                .child(SharedString::from(count)),
        )
}

/// The hard cap so a long history cannot make the rail unbounded.
const RAIL_LIMIT: usize = 200;

/// Fetch the real session list on a thread with a real stack, same shape as
/// `load_view` in `main.rs`: tokio current-thread runtime on a 4 MB thread,
/// result over a oneshot. `Database::connect` runs pragmas only, never
/// migrations, so a read cannot race the daemon that owns the schema.
pub async fn load_sessions(db_path: std::path::PathBuf) -> Result<Vec<Session>, String> {
    let (tx, rx) = futures::channel::oneshot::channel::<Result<Vec<Session>, String>>();
    let spawned = std::thread::Builder::new()
        .name("sessions-fetch".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt
                    .block_on(async {
                        let db = Database::connect(&db_path).await?;
                        let repo = SessionRepository::new(db.pool().clone());
                        repo.list(SessionListOptions {
                            limit: Some(RAIL_LIMIT),
                            ..Default::default()
                        })
                        .await
                    })
                    .map_err(|e| format!("{e:#}")),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("sessions fetch thread dropped its result".into()),
        },
        Err(e) => Err(format!("sessions fetch thread failed to spawn: {e}")),
    }
}

/// The pinned set, read through the same 4 MB no-migration door as
/// [`load_sessions`]. Returns ids: the rail already holds the rows, this only
/// says which of them go on top.
pub async fn load_favorites(db_path: std::path::PathBuf) -> Result<Vec<uuid::Uuid>, String> {
    let (tx, rx) = futures::channel::oneshot::channel::<Result<Vec<uuid::Uuid>, String>>();
    let spawned = std::thread::Builder::new()
        .name("favorites-fetch".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt.block_on(async {
                    let db = Database::connect(&db_path)
                        .await
                        .map_err(|e| format!("db connect: {e}"))?;
                    let repo = SessionFavoritesRepository::new(db.pool().clone());
                    let ids = repo
                        .all()
                        .await
                        .map_err(|e| format!("favorites read: {e}"))?;
                    // The sidecar stores ids as text; a malformed one is a
                    // foreign key that got past SQLite, so it is skipped
                    // rather than failing the whole rail.
                    // Both arms are `String`-errored, so the block's own
                    // error type is stated rather than inferred: a
                    // generic `?` target leaves it ambiguous.
                    Ok::<Vec<uuid::Uuid>, String>(
                        ids.iter()
                            .filter_map(|s| uuid::Uuid::parse_str(s).ok())
                            .collect(),
                    )
                }),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("favorites fetch thread dropped its result".into()),
        },
        Err(e) => Err(format!("favorites fetch thread failed to spawn: {e}")),
    }
}

/// The preview line for every session in `ids`, through the same 4 MB
/// no-migration door as [`load_sessions`]. One batched query per pass, so a
/// rail of 200 cards costs one round trip rather than 200.
pub async fn load_previews(
    db_path: std::path::PathBuf,
    ids: Vec<uuid::Uuid>,
) -> Result<HashMap<uuid::Uuid, SharedString>, String> {
    let (tx, rx) =
        futures::channel::oneshot::channel::<Result<HashMap<uuid::Uuid, SharedString>, String>>();
    let spawned = std::thread::Builder::new()
        .name("session-previews-fetch".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt
                    .block_on(async {
                        let db = Database::connect(&db_path).await?;
                        let repo = MessageRepository::new(db.pool().clone());
                        repo.last_messages_for(&ids).await
                    })
                    .map(|previews| {
                        previews
                            .into_iter()
                            .map(|(id, p)| (id, preview_line(&p.role, &p.text)))
                            .collect()
                    })
                    .map_err(|e| format!("{e:#}")),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("previews fetch thread dropped its result".into()),
        },
        Err(e) => Err(format!("previews fetch thread failed to spawn: {e}")),
    }
}

/// ctrl+f's deep read (#2720): which sessions mention `query` in their
/// messages, with a snippet, through the same 4 MB no-migration door as
/// [`load_previews`]. Spans every session (the core search includes archived),
/// off the paint path, so a rail repaint never waits on the database. The row
/// count is bounded so a common word cannot flood the rail.
pub async fn load_content_matches(
    db_path: std::path::PathBuf,
    query: String,
) -> Result<Vec<ContentSearchRow>, String> {
    let (tx, rx) = futures::channel::oneshot::channel::<Result<Vec<ContentSearchRow>, String>>();
    let spawned = std::thread::Builder::new()
        .name("session-content-find".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt
                    .block_on(async {
                        let db = Database::connect(&db_path).await?;
                        let messages = MessageRepository::new(db.pool().clone());
                        let sessions = SessionRepository::new(db.pool().clone());
                        opencrabs::db::repository::search_session_content(
                            &messages, &sessions, &query, 50,
                        )
                        .await
                    })
                    .map_err(|e| format!("{e:#}")),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("content find thread dropped its result".into()),
        },
        Err(e) => Err(format!("content find thread failed to spawn: {e}")),
    }
}

/// What the topic info panel shows (#2304). Read once per open, never edited.
#[derive(Clone)]
pub struct TopicInfo {
    pub id: uuid::Uuid,
    pub parent_title: Option<String>,
    pub created_at: i64,
    pub messages: i64,
}

/// Read one session's info for the panel. A session that is not a topic
/// reports no parent.
pub async fn load_topic_info(
    db_path: std::path::PathBuf,
    id: uuid::Uuid,
) -> Result<Option<TopicInfo>, String> {
    let (tx, rx) = futures::channel::oneshot::channel::<Result<Option<TopicInfo>, String>>();
    let spawned = std::thread::Builder::new()
        .name("topic-info-fetch".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt
                    .block_on(async {
                        let db = Database::connect(&db_path).await?;
                        let repo = SessionRepository::new(db.pool().clone());
                        let Some((parent, created_at, messages)) = repo.topic_info(id).await?
                        else {
                            return Ok(None);
                        };
                        let parent_title = match parent {
                            Some(p) => repo.find_by_id(p).await?.and_then(|s| s.title),
                            None => None,
                        };
                        Ok::<_, anyhow::Error>(Some(TopicInfo {
                            id,
                            parent_title,
                            created_at,
                            messages,
                        }))
                    })
                    .map_err(|e| format!("{e:#}")),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("topic info thread dropped its result".into()),
        },
        Err(e) => Err(format!("topic info thread failed to spawn: {e}")),
    }
}

/// Rename a session or topic. The caller re-reads the rail afterwards.
pub async fn rename_session(
    db_path: std::path::PathBuf,
    id: uuid::Uuid,
    title: String,
) -> Result<(), String> {
    let (tx, rx) = futures::channel::oneshot::channel::<Result<(), String>>();
    let spawned = std::thread::Builder::new()
        .name("rename-write".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt
                    .block_on(async {
                        let db = Database::connect(&db_path).await?;
                        let repo = SessionRepository::new(db.pool().clone());
                        repo.rename(id, &title).await
                    })
                    .map_err(|e| format!("{e:#}")),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("rename thread dropped its result".into()),
        },
        Err(e) => Err(format!("rename thread failed to spawn: {e}")),
    }
}

/// Every closed session. A closed topic is dimmed and refuses new turns (#2305).
pub async fn load_closed(db_path: std::path::PathBuf) -> Result<Vec<uuid::Uuid>, String> {
    let (tx, rx) = futures::channel::oneshot::channel::<Result<Vec<uuid::Uuid>, String>>();
    let spawned = std::thread::Builder::new()
        .name("closed-fetch".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt
                    .block_on(async {
                        let db = Database::connect(&db_path).await?;
                        let repo = SessionRepository::new(db.pool().clone());
                        repo.closed_ids().await
                    })
                    .map_err(|e| format!("{e:#}")),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("closed fetch thread dropped its result".into()),
        },
        Err(e) => Err(format!("closed fetch thread failed to spawn: {e}")),
    }
}

/// Close or reopen one topic. The caller re-reads the closed set afterwards.
pub async fn set_closed(
    db_path: std::path::PathBuf,
    id: uuid::Uuid,
    closed: bool,
) -> Result<(), String> {
    let (tx, rx) = futures::channel::oneshot::channel::<Result<(), String>>();
    let spawned = std::thread::Builder::new()
        .name("closed-write".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt
                    .block_on(async {
                        let db = Database::connect(&db_path).await?;
                        let repo = SessionRepository::new(db.pool().clone());
                        repo.set_closed(id, closed).await
                    })
                    .map_err(|e| format!("{e:#}")),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("closed write thread dropped its result".into()),
        },
        Err(e) => Err(format!("closed write thread failed to spawn: {e}")),
    }
}

/// Every (topic, parent) pair. Read separately from the rows, like favorites,
/// so the grouping does not change the row model (#2303).
pub async fn load_topic_parents(
    db_path: std::path::PathBuf,
) -> Result<Vec<(uuid::Uuid, uuid::Uuid)>, String> {
    let (tx, rx) =
        futures::channel::oneshot::channel::<Result<Vec<(uuid::Uuid, uuid::Uuid)>, String>>();
    let spawned = std::thread::Builder::new()
        .name("topic-parents-fetch".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt
                    .block_on(async {
                        let db = Database::connect(&db_path).await?;
                        let repo = SessionRepository::new(db.pool().clone());
                        repo.topic_parents().await
                    })
                    .map_err(|e| format!("{e:#}")),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("topic parents fetch thread dropped its result".into()),
        },
        Err(e) => Err(format!("topic parents fetch thread failed to spawn: {e}")),
    }
}

/// Create a topic under a session and return its id. The database refuses a
/// parent that is already a topic; that refusal comes back as the error.
pub async fn create_topic(
    db_path: std::path::PathBuf,
    parent: uuid::Uuid,
    title: String,
) -> Result<uuid::Uuid, String> {
    let (tx, rx) = futures::channel::oneshot::channel::<Result<uuid::Uuid, String>>();
    let spawned = std::thread::Builder::new()
        .name("topic-create".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt
                    .block_on(async {
                        let db = Database::connect(&db_path).await?;
                        let repo = SessionRepository::new(db.pool().clone());
                        repo.create_topic(parent, &title).await
                    })
                    .map_err(|e| format!("{e:#}")),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("topic create thread dropped its result".into()),
        },
        Err(e) => Err(format!("topic create thread failed to spawn: {e}")),
    }
}

/// Candidate sessions for the Group topics picker: live, non-subagent rows
/// other than the target, each as (id, title, date). Topic status is filtered
/// by the caller from the rail's own parent map, so this stays a plain list
/// read on the same dedicated-stack thread the other rail loaders use.
pub async fn load_group_candidates(
    db_path: std::path::PathBuf,
    target: uuid::Uuid,
) -> Result<Vec<(uuid::Uuid, String, String)>, String> {
    let (tx, rx) =
        futures::channel::oneshot::channel::<Result<Vec<(uuid::Uuid, String, String)>, String>>();
    let spawned = std::thread::Builder::new()
        .name("group-candidates".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt
                    .block_on(async {
                        let db = Database::connect(&db_path).await?;
                        let repo = SessionRepository::new(db.pool().clone());
                        let rows = repo.list(SessionListOptions::default()).await?;
                        Ok::<_, anyhow::Error>(
                            rows.into_iter()
                                .filter(|s| s.id != target)
                                .map(|s| {
                                    let title = s.title.unwrap_or_else(|| "(untitled)".to_string());
                                    let date = s.created_at.format("%m-%d %H:%M").to_string();
                                    (s.id, title, date)
                                })
                                .collect(),
                        )
                    })
                    .map_err(|e| format!("{e:#}")),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("group candidates thread dropped its result".into()),
        },
        Err(e) => Err(format!("group candidates thread failed to spawn: {e}")),
    }
}

/// Attach one existing session as a topic of `parent`. A guard refusal comes
/// back as its plain sentence (the typed error's Display), so the picker can
/// show why instead of a raw constraint error.
pub async fn attach_topic(
    db_path: std::path::PathBuf,
    child: uuid::Uuid,
    parent: uuid::Uuid,
) -> Result<(), String> {
    let (tx, rx) = futures::channel::oneshot::channel::<Result<(), String>>();
    let spawned = std::thread::Builder::new()
        .name("topic-attach".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt
                    .block_on(async {
                        let db = Database::connect(&db_path).await?;
                        let repo = SessionRepository::new(db.pool().clone());
                        repo.attach_topic(child, parent).await
                    })
                    .map_err(|e| format!("{e:#}")),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("topic attach thread dropped its result".into()),
        },
        Err(e) => Err(format!("topic attach thread failed to spawn: {e}")),
    }
}

/// Every session with topics turned on. Read separately from the rows, the
/// same way favorites are, so the row model does not carry the flag.
pub async fn load_topics(db_path: std::path::PathBuf) -> Result<Vec<uuid::Uuid>, String> {
    let (tx, rx) = futures::channel::oneshot::channel::<Result<Vec<uuid::Uuid>, String>>();
    let spawned = std::thread::Builder::new()
        .name("topics-fetch".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt
                    .block_on(async {
                        let db = Database::connect(&db_path).await?;
                        let repo = SessionRepository::new(db.pool().clone());
                        repo.topic_enabled_ids().await
                    })
                    .map_err(|e| format!("{e:#}")),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("topics fetch thread dropped its result".into()),
        },
        Err(e) => Err(format!("topics fetch thread failed to spawn: {e}")),
    }
}

/// Every topic's own icon. Read with the topic state, the same way the flag is.
pub async fn load_topic_icons(
    db_path: std::path::PathBuf,
) -> Result<Vec<(uuid::Uuid, String)>, String> {
    let (tx, rx) =
        futures::channel::oneshot::channel::<Result<Vec<(uuid::Uuid, String)>, String>>();
    let spawned = std::thread::Builder::new()
        .name("topic-icons-fetch".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt
                    .block_on(async {
                        let db = Database::connect(&db_path).await?;
                        let repo = SessionRepository::new(db.pool().clone());
                        repo.topic_icons().await
                    })
                    .map_err(|e| format!("{e:#}")),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            if tx.send(result).is_err() {
                crate::logging::error("topic icons result dropped: the window closed first");
            }
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("topic icons fetch thread dropped its result".into()),
        },
        Err(e) => Err(format!("topic icons fetch thread failed to spawn: {e}")),
    }
}

/// The live name of every Telegram topic session in `items`, in ONE query
/// (#2490).
///
/// The row a rail draws is a snapshot: a session keeps the name it was created
/// with. Telegram is the truth, and a rename lands in `channel_messages`
/// (`#143`). Resolving per refresh means the rail follows a rename on its next
/// pass with no restart, and batching the pairs into one query means a list of
/// two hundred rows still costs one lookup per pass, not two hundred.
pub async fn load_telegram_topic_titles(
    db_path: std::path::PathBuf,
    items: &[Session],
) -> Result<HashMap<uuid::Uuid, String>, String> {
    use opencrabs::db::repository::channel_message::ChannelMessageRepository;
    use opencrabs::ui_core::rail;

    let mut wanted: Vec<(uuid::Uuid, String, String)> = Vec::new();
    for session in items {
        let Some(title) = session.title.as_deref() else {
            continue;
        };
        let Some((chat_id, thread_id)) = rail::topic_thread(title) else {
            continue;
        };
        wanted.push((session.id, chat_id.to_string(), thread_id.to_string()));
    }
    if wanted.is_empty() {
        return Ok(HashMap::new());
    }

    let (tx, rx) =
        futures::channel::oneshot::channel::<Result<HashMap<uuid::Uuid, String>, String>>();
    let spawned = std::thread::Builder::new()
        .name("topic-titles-fetch".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt
                    .block_on(async {
                        let db = Database::connect(&db_path).await?;
                        let repo = ChannelMessageRepository::new(db.pool().clone());
                        let pairs: Vec<(String, String)> = wanted
                            .iter()
                            .map(|(_, chat, thread)| (chat.clone(), thread.clone()))
                            .collect();
                        let names = repo.topic_names("telegram", &pairs).await?;
                        let mut out = HashMap::with_capacity(wanted.len());
                        for (id, chat, thread) in wanted {
                            if let Some(name) = names.get(&(chat, thread)) {
                                out.insert(id, name.clone());
                            }
                        }
                        Ok(out)
                    })
                    .map_err(|e: anyhow::Error| format!("{e:#}")),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            if tx.send(result).is_err() {
                crate::logging::error("topic titles result dropped: the window closed first");
            }
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("topic titles fetch thread dropped its result".into()),
        },
        Err(e) => Err(format!("topic titles fetch thread failed to spawn: {e}")),
    }
}

/// Whether a rename has to write the icon (#2791). The draft is seeded from
/// the stored value when the dialog opens, so only a real change writes: a
/// None draft keeps whatever is stored and never overwrites it with the
/// default, and an equal draft skips the write entirely.
pub fn icon_write_needed(stored: Option<&str>, draft: Option<&str>) -> bool {
    match draft {
        None => false,
        Some(draft) => stored != Some(draft),
    }
}

/// Give one topic an icon, or clear it with None. The caller re-reads afterwards.
pub async fn set_topic_icon(
    db_path: std::path::PathBuf,
    id: uuid::Uuid,
    icon: Option<String>,
) -> Result<(), String> {
    let (tx, rx) = futures::channel::oneshot::channel::<Result<(), String>>();
    let spawned = std::thread::Builder::new()
        .name("topic-icon-write".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt
                    .block_on(async {
                        let db = Database::connect(&db_path).await?;
                        let repo = SessionRepository::new(db.pool().clone());
                        repo.set_topic_icon(id, icon.as_deref()).await
                    })
                    .map_err(|e| format!("{e:#}")),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            if tx.send(result).is_err() {
                crate::logging::error("topic icon result dropped: the window closed first");
            }
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("topic icon write thread dropped its result".into()),
        },
        Err(e) => Err(format!("topic icon write thread failed to spawn: {e}")),
    }
}

/// Turn topics on or off for one session. The caller re-reads afterwards.
pub async fn set_topics(
    db_path: std::path::PathBuf,
    id: uuid::Uuid,
    enabled: bool,
) -> Result<(), String> {
    let (tx, rx) = futures::channel::oneshot::channel::<Result<(), String>>();
    let spawned = std::thread::Builder::new()
        .name("topics-write".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt
                    .block_on(async {
                        let db = Database::connect(&db_path).await?;
                        let repo = SessionRepository::new(db.pool().clone());
                        repo.set_topics_enabled(id, enabled).await
                    })
                    .map_err(|e| format!("{e:#}")),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("topics write thread dropped its result".into()),
        },
        Err(e) => Err(format!("topics write thread failed to spawn: {e}")),
    }
}

/// Fold every topic of `id` back into it and switch topics off (#2863).
///
/// The whole fold is one repository transaction, so this either reports how
/// many topics came home or reports nothing changed. The caller refreshes the
/// rail and the topic store afterwards rather than patching its own copy: a
/// topic that moved is a session row that changed, and guessing at that in
/// memory is how two surfaces end up disagreeing.
pub async fn merge_topics(db_path: std::path::PathBuf, id: uuid::Uuid) -> Result<usize, String> {
    let (tx, rx) = futures::channel::oneshot::channel::<Result<usize, String>>();
    let spawned = std::thread::Builder::new()
        .name("topics-fold".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt
                    .block_on(async {
                        let db = Database::connect(&db_path).await?;
                        let repo = SessionRepository::new(db.pool().clone());
                        repo.merge_topics_into_parent(id).await
                    })
                    .map_err(|e| format!("{e:#}")),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("topics fold thread dropped its result".into()),
        },
        Err(e) => Err(format!("topics fold thread failed to spawn: {e}")),
    }
}

/// Pin or unpin one session in the shared sidecar table. The caller refetches
/// afterwards instead of patching its own list, so the rail can never show a
/// pin the ledger does not have.
pub async fn set_favorite(
    db_path: std::path::PathBuf,
    id: uuid::Uuid,
    pinned: bool,
) -> Result<(), String> {
    let (tx, rx) = futures::channel::oneshot::channel::<Result<(), String>>();
    let spawned = std::thread::Builder::new()
        .name("favorite-write".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt
                    .block_on(async {
                        let db = Database::connect(&db_path).await?;
                        let repo = SessionFavoritesRepository::new(db.pool().clone());
                        let sid = id.to_string();
                        if pinned {
                            repo.add(&sid).await
                        } else {
                            repo.remove(&sid).await
                        }
                    })
                    .map_err(|e| format!("{e:#}")),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("favorite write thread dropped its result".into()),
        },
        Err(e) => Err(format!("favorite write thread failed to spawn: {e}")),
    }
}

/// Char-safe truncation: walk chars, never slice bytes (#2271's whole class).
fn fit(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The label a topic chip prints, cut to the chip's character budget. The strip
/// measures this same string when it places the chips (#2809), so the width and
/// the paint can never disagree about what a chip says.
pub fn chip_label(title: &str) -> String {
    fit(title, 14)
}

/// The card's preview line: who spoke, then the flattened text cut to fit.
///
/// The prefix names the speaker the way the reader thinks of them, not the
/// way the table stores the role. A role that is neither `user` nor
/// `assistant` keeps its own name rather than pretending to be one of them.
pub fn preview_line(role: &str, text: &str) -> SharedString {
    // The wording lives in `ui_core`: the TUI session row draws the same
    // line for the same session (#2701).
    SharedString::from(opencrabs::ui_core::session_preview::preview_line(
        role, text,
    ))
}

/// The pin mark a row carries (#2788): a quiet Telegram-style pushpin,
/// filled once pinned, outline before first use. Without the outline a
/// first-time user saw no mark, no button and no hint, so pinning looked
/// like it did not exist (#2353). The colour rides along: muted when the
/// pin is on, faint while the outline is only an invitation. Never the
/// accent: the pin is a quiet mark, not a highlight.
pub fn pin_mark(favorited: bool) -> (bool, u32) {
    if favorited {
        (true, palette::current().text_muted)
    } else {
        (false, palette::current().text_faint)
    }
}

/// Full rail cards use one media size for stored pictures and platform marks.
pub const RAIL_MARK_REMS: f32 = 1.75;
const RAIL_CHANNEL_BADGE_REMS: f32 = 0.75;
const RAIL_CHANNEL_GLYPH_REMS: f32 = 0.45;

/// The card's quiet stamp, Telegram style: a session touched today shows its
/// clock alone, and anything older collapses to its calendar day. A rail only
/// ever lists recent work, so printing the year, or a date that is obviously
/// today, is noise.
fn rail_stamp(
    updated_at: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Local>,
) -> String {
    let local = updated_at.with_timezone(&chrono::Local);
    if local.date_naive() == now.date_naive() {
        local.format("%H:%M").to_string()
    } else {
        local.format("%m-%d").to_string()
    }
}

/// Cost at rail scale: cents are noise beside a name, so a card prints whole
/// dollars and shortens four-digit sums to one decimal of thousands. Only a
/// sub-dollar session keeps its cents, where they are the whole story.
fn rail_cost(cost: f64) -> String {
    if cost >= 1_000.0 {
        format!("${:.1}k", cost / 1_000.0)
    } else if cost >= 1.0 {
        format!("${:.0}", cost)
    } else if cost > 0.0 {
        format!("${:.2}", cost)
    } else {
        "$0".to_string()
    }
}

const PIN_SVG: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/assets/pin.svg"));
const PIN_OUTLINE_SVG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/assets/pin-outline.svg"
));

/// The pin's square, in rems (#2803). The chip strip reserves exactly this
/// band at the right end of its first row, so a chip never lands under the pin
/// (#2809).
pub const PIN_REMS: f32 = 1.4;

/// The row's pushpin (#2788), drawn from an embedded vector so the tilted
/// silhouette stays crisp on retina, which a font glyph cannot promise.
pub fn pin_icon(favorited: bool, size: f32) -> impl IntoElement {
    let (filled, colour) = pin_mark(favorited);
    let bytes = if filled { PIN_SVG } else { PIN_OUTLINE_SVG };
    let svg = String::from_utf8_lossy(bytes)
        .replace("#8a8a8a", &format!("#{colour:06x}"))
        .into_bytes();
    img(Arc::new(Image::from_bytes(ImageFormat::Svg, svg)))
        .w(rems(size))
        .h(rems(size))
        .flex_none()
        .object_fit(ObjectFit::Contain)
}

/// The visible pin is the pin/unpin control (#2803). The view attaches the
/// existing favorite action; this renderer owns only the hit target and hover.
pub fn pin_toggle(id: uuid::Uuid, favorited: bool) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("session-pin-toggle-{id}")))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(rems(PIN_REMS))
        .rounded_sm()
        .cursor_pointer()
        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
        .child(pin_icon(favorited, 0.9))
}

/// The menu's contents, resolved from the row's pin and topic state. Pure, so
/// the labels are pinned by test and the render only draws what this returns
/// (#2354).
pub fn rail_menu_entries(
    favorited: bool,
    topics_enabled: bool,
    is_topic: bool,
) -> Vec<MenuEntry> {
    let mut entries = vec![MenuEntry {
        kind: MenuKind::Pin,
        label: if favorited { "Unpin" } else { "Pin" },
    }];
    // Rename sits next to Pin: the rail row is where a conversation is
    // managed, and the prompt used to be reachable only from the
    // conversation body's own menu, which left a sidebar row with no way to
    // take a new name (#2668).
    entries.push(MenuEntry {
        kind: MenuKind::Rename,
        label: "Rename",
    });
    // A project belongs to the parent session. Topics inside that session
    // share its assignment and never get an independent project action.
    if !is_topic {
        entries.push(MenuEntry {
            kind: MenuKind::AssignProject,
            label: "Assign project...",
        });
    }
    // Topics are opt-in per session (#2302): the item is offered only while
    // they are off, so it cannot be flipped twice by accident. Once they are
    // on, the same slot offers a new topic under the row (#2397), and the
    // way back out sits next to it (#2863). Turning them off folds whatever
    // lives in the topics into this session, so the entry asks first.
    if topics_enabled {
        entries.push(MenuEntry {
            kind: MenuKind::NewTopic,
            label: "New topic",
        });
        entries.push(MenuEntry {
            kind: MenuKind::DisableTopics,
            label: "Disable topics...",
        });
    } else {
        entries.push(MenuEntry {
            kind: MenuKind::EnableTopics,
            label: "Enable topics",
        });
    }
    // Group existing conversations under this row as topics, without moving
    // their transcripts. The attach guards refuse a depth-2 target with a
    // readable sentence rather than a constraint error.
    entries.push(MenuEntry {
        kind: MenuKind::GroupTopics,
        label: "Group topics...",
    });
    // Split | and Split _ start a split from this row (#2438).
    entries.push(MenuEntry {
        kind: MenuKind::SplitSide,
        label: "Split |",
    });
    entries.push(MenuEntry {
        kind: MenuKind::SplitStack,
        label: "Split _",
    });
    entries.push(MenuEntry {
        kind: MenuKind::Delete,
        label: "Delete",
    });
    entries
}

/// What a rail row shows about its session right now. Grouped so the row
/// keeps a short argument list as the rail gains states.
///
/// Not Clone: built elements are moved, not copied. The row takes the flags by
/// value, so nothing needs a clone.
#[derive(Default)]
pub struct RowFlags {
    pub selected: bool,
    pub cursored: bool,
    /// The view-wired pin/unpin control (#2803), shared by full and compact rows.
    pub pin_toggle: Option<Stateful<Div>>,
    pub indented: bool,
    pub closed: bool,
    /// The spinner glyph for a row that has work in flight (#2636), or `None`.
    /// The rail supplies the frame from its one shared clock; this module only
    /// draws the cell, so it never owns a timer.
    pub badge: Option<&'static str>,
    /// The already-formatted preview line, or `None` when the session has no
    /// readable user/assistant message yet. It paints as the card's message
    /// line: under the title when there are no topics, under the chip strip
    /// when there are. Timestamp and cost always ride the title line, so a
    /// card without a plan or a running task is at most three lines.
    pub preview: Option<SharedString>,
    /// What the turn running in this session is doing right now (#2718): the
    /// tool call in flight, else the thinking line. It takes the preview's
    /// place while it lasts, so a card never grows a line for it, and the
    /// persisted preview comes back untouched once the turn settles.
    pub activity: Option<SharedString>,
    /// The session's approved plan progress as `(done, total)`, or `None`
    /// when there is no checklist to draw. Paints as a slim bar line under
    /// the preview, reusing the composer strip's own bar so the same plan
    /// reads identically in both places.
    pub plan: Option<(usize, usize)>,
    /// Whether the plan bar is in the lit half of its settle flash, in which
    /// case it paints orange instead of its resting teal. The caller owns the
    /// clock; this module only picks the colour.
    pub flash: bool,
    /// The detached commands this session is still running, already shaped by
    /// the terminal's own formatter (`⏳ cargo test 32s`), or `None` when
    /// nothing runs. Paints as the card's last line, under the plan bar, so a
    /// row says "work is still going here" without being opened.
    pub running: Option<SharedString>,
    /// The spend the card prints, already rolled up over the topics it hides
    /// as chips (#2807). The row never reads `s.total_cost` itself, so a
    /// grouped card cannot print a number that leaves its children out.
    pub cost: f64,
    /// The instant the card's stamp prints (#2815), already rolled up over the
    /// topics it hides as chips the way `cost` is. A topic's message lands on
    /// the topic's own ledger row, so the parent's `updated_at` stays at the
    /// last message addressed to the parent itself; the view folds the two
    /// together so a card whose topics ran minutes ago does not print an old
    /// date and sit below quieter cards.
    pub stamp_at: chrono::DateTime<chrono::Utc>,
    /// The session's stored icon, already built by the view (#2790): an
    /// embedded mark, a custom image or a legacy glyph. It leads the optional
    /// channel mark rather than replacing it (#2800). The module never reads
    /// files: the view owns the image cache and hands over the finished
    /// element.
    pub icon: Option<AnyElement>,
}

/// One topic chip: a child conversation painted as a compact pill on its
/// parent's card instead of its own row (Telegram topic style). `live` is the
/// spinner glyph while the child streams; `ring` paints a dot while it awaits
/// the user. `plan` is the child's approved checklist as `(done, total)`: while
/// it runs the chip itself fills left-to-right like a progress bar, the done
/// share tinted teal over the chip's base. The caller attaches the open and
/// context-menu handlers, so this stays pure data plus paint.
pub fn topic_chip(
    id: uuid::Uuid,
    title: &str,
    live: Option<&'static str>,
    ring: bool,
    selected: bool,
    plan: Option<(usize, usize)>,
) -> Stateful<Div> {
    let pal = palette::current();
    // The focused chip sits one visible step away from the panel in both
    // themes (#2792): track, not the old selection wash, plus an accent ring
    // below. Hover can never equal this fill.
    let base = if selected { pal.track } else { pal.panel };
    let chip = div()
        .id(SharedString::from(format!("topic-chip-{id}")))
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap_0p5()
        .flex_none()
        .px_2()
        .py_0p5()
        .rounded_md()
        .cursor_pointer()
        .text_xs()
        .text_color(rgb(pal.text));
    // A running plan turns the chip into its own progress bar: a hard-stop
    // gradient fills the done share with teal mixed into the base, the rest
    // stays the base. No plan (or an empty one) keeps the flat chip colour.
    let chip = match plan {
        Some((done, total)) if total > 0 => {
            let frac = (done as f32 / total as f32).clamp(0.0, 1.0);
            let fill = gui_theme::mix(base, pal.teal, 45, 100);
            chip.bg(linear_gradient(
                90.0,
                linear_color_stop(rgb(fill), frac),
                linear_color_stop(rgb(base), frac),
            ))
        }
        _ => chip.bg(rgb(base)),
    };
    let mut chip = chip.hover(|d| d.bg(rgb(pal.panel_hi)));
    // The ring is the "you are here" mark on the focused chip (#2792): a
    // 1px accent outline over the track fill, never a colour field.
    if selected {
        chip = chip.border_1().border_color(rgb(pal.accent));
    }
    if let Some(glyph) = live {
        chip = chip.child(
            div()
                .flex_none()
                .text_color(rgb(palette::current().teal))
                .child(SharedString::from(glyph)),
        );
    } else if ring {
        chip = chip.child(
            div()
                .flex_none()
                .text_color(rgb(palette::current().accent))
                .child("\u{25CF}"),
        );
    }
    chip.child(
        div()
            .truncate()
            .child(SharedString::from(chip_label(title))),
    )
}

/// The overflow pill: "+N" expands a parent's hidden chips, the collapse
/// label folds them back. Same shape as a topic chip, no title of its own.
pub fn count_chip(id: uuid::Uuid, label: SharedString) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("topic-count-{id}")))
        .flex()
        .flex_row()
        .items_center()
        .flex_none()
        .px_2()
        .py_0p5()
        .rounded_md()
        .cursor_pointer()
        .text_xs()
        .text_color(rgb(palette::current().text_muted))
        .bg(rgb(palette::current().panel))
        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
        .child(label)
}

/// The pixel width of a string at the chip's type size, measured against the
/// live text system. Falls back to a per-character estimate only if the shaper
/// refuses the run, which keeps a chip placeable rather than collapsing it.
fn text_width(window: &Window, text: &str) -> f32 {
    let style = window.text_style();
    let size = window.rem_size() * 0.75;
    let run = TextRun {
        len: text.len(),
        font: style.font(),
        color: style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_text(
            SharedString::from(text.to_string()),
            size,
            &[run],
            None,
            None,
        )
        .ok()
        .and_then(|lines| lines.into_iter().next())
        .map(|line| f32::from(line.width()))
        .unwrap_or_else(|| text.chars().count() as f32 * f32::from(size) * 0.7)
}

/// The width one topic chip takes on the strip, measured from the label it
/// prints. The strip places the chips itself so the pin can hold the right end
/// of the first row alone (#2809), and a hand-placed chip needs its own width.
pub fn chip_width(window: &Window, title: &str, glyph: bool, selected: bool) -> f32 {
    let rem = f32::from(window.rem_size());
    chip_width_at(rem, text_width(window, &chip_label(title)), glyph, selected)
}

/// A chip's width from its measured label: the pill's padding on both sides,
/// the live glyph slot when it has one, the accent ring when it is focused, and
/// a hair of slack so a rounded glyph never reaches the pin.
fn chip_width_at(rem: f32, text: f32, glyph: bool, selected: bool) -> f32 {
    let mut width = rem + text + rem * 0.1;
    if glyph {
        width += rem * 0.125 + rem * 0.75;
    }
    if selected {
        width += 2.0;
    }
    width
}

/// The width the overflow pill takes: the chip padding, its short label, and
/// the same slack a topic chip keeps.
pub fn count_chip_width(window: &Window, label: &str) -> f32 {
    let rem = f32::from(window.rem_size());
    rem + text_width(window, label) + rem * 0.1
}

/// The strip's two pixel budgets (#2809). The strip sits in a card's body, and
/// the row spends 0.5rem of padding on each side, 1.75rem on the media slot and
/// 0.25rem between them, so the body is 3rem narrower than the rail. The first
/// row gives up the pin's band as well, because the pin floats at its right
/// end; every row below runs the whole body width.
pub fn strip_budgets(rem: f32, rail: f32) -> (f32, f32) {
    let body = rail - rem * 3.0;
    // A chip stops one gap short of the pin, plus the same slack it keeps
    // everywhere else.
    let first = body - rem * (PIN_REMS + 0.25) - rem * 0.1;
    (first, body - rem * 0.1)
}

/// Split chips into rows against a pixel budget, the way a wrapping flex row
/// would, but with a narrower first row. Counts per row, in order. A chip wider
/// than its whole budget still takes a row of its own rather than vanishing.
pub fn pack_rows(widths: &[f32], gap: f32, first: f32, rest: f32) -> Vec<usize> {
    let mut rows = Vec::new();
    let mut count = 0usize;
    let mut used = 0.0f32;
    let mut budget = first;
    for width in widths {
        let needed = if count == 0 {
            *width
        } else {
            used + gap + *width
        };
        if count > 0 && needed > budget {
            rows.push(count);
            count = 0;
            used = *width;
            budget = rest;
        } else {
            used = needed;
        }
        count += 1;
    }
    if count > 0 {
        rows.push(count);
    }
    rows
}

/// The chip strip under a parent card's title. The caller packs the chips into
/// rows, so the first row can stop short of the pin while the rows below take
/// the full card width (#2809). A row never holds one chip per line: the chips
/// ride inline under the title, the way Telegram stacks its topics. The parent
/// id keys the strip so two parents never share an element id.
pub fn topic_chips(id: uuid::Uuid, rows: Vec<Vec<Stateful<Div>>>) -> Stateful<Div> {
    let mut strip = div()
        .id(SharedString::from(format!("topic-chips-{id}")))
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .min_w_0();
    for chips in rows {
        let mut line = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .w_full()
            .min_w_0();
        for chip in chips {
            line = line.child(chip);
        }
        strip = strip.child(line);
    }
    strip
}

/// Whether a rail row paints the left accent bar. The bar marks the open
/// conversation, if and only if the row is selected: the keyboard cursor
/// never draws it and gets no replacement cue. The owner overruled the
/// two-state design from #2351 (#2717). Both flags come in so the contract
/// stays testable across all four state combinations.
fn accent_bar_visible(selected: bool, cursored: bool) -> bool {
    matches!((selected, cursored), (true, _))
}

pub fn row(s: &Session, flags: RowFlags, chips: Option<Stateful<Div>>) -> Stateful<Div> {
    let RowFlags {
        selected,
        cursored,
        pin_toggle,
        indented,
        closed,
        badge,
        preview,
        activity,
        plan,
        flash,
        running,
        cost,
        stamp_at,
        icon,
    } = flags;
    // A channel card says the chat once: the platform prints as its mark, not
    // as a word, and the topic segment belongs to the chip under the card, not
    // to the card itself (#2766). A row indented under a card is a topic, so it
    // prints its own topic name and no second mark.
    let stored = s.title.as_deref();
    let label = match stored {
        Some(title) if indented => rail::chip_name(Some(title), None),
        Some(title) => rail::chat_name(Some(title)),
        None => "(untitled)".to_string(),
    };
    // One media slot, never two side-by-side icons (#2800). A stored picture
    // carries a small accent channel badge at its top right. Without a picture,
    // a supported channel keeps only its standalone mark; GUI-only sessions
    // get no invented avatar.
    let platform = rail::platform_of(stored.filter(|_| !indented));
    let picture = icon.map(|icon| {
        let badge = platform
            .and_then(|platform| {
                platform_icons::platform_icon_with_colour(
                    platform,
                    RAIL_CHANNEL_GLYPH_REMS,
                    palette::current().accent,
                )
            })
            .map(|mark| {
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(rems(RAIL_CHANNEL_BADGE_REMS))
                    .rounded_full()
                    .border_1()
                    .border_color(rgb(palette::current().accent))
                    .bg(rgb(palette::current().panel))
                    .child(mark)
            });
        div()
            .relative()
            .flex_none()
            .size(rems(RAIL_MARK_REMS))
            .child(icon)
            .when_some(badge, |d, badge| d.child(badge))
            .into_any_element()
    });
    let marks = picture.or_else(|| {
        platform.and_then(|platform| platform_icons::platform_icon(platform, RAIL_MARK_REMS))
    });
    let title_text = div()
        // A closed topic reads dimmed: still there, no longer taking work.
        .text_color(rgb(if closed {
            palette::current().text_faint
        } else {
            palette::current().text
        }))
        .truncate()
        // The name gives way, the trailing cluster never moves (#2802). A
        // text child sized to its own content overflowed the row on a wide
        // rail instead of shrinking, so the cluster it shared the line with
        // was painted over the name. `flex_1` plus `min_w_0` lets the name
        // truncate inside the space the cluster leaves.
        .flex_1()
        .min_w_0()
        .child(SharedString::from(fit(&label, 42)));
    let title_text = if indented {
        title_text.text_xs()
    } else {
        title_text.text_sm().font_weight(gpui::FontWeight::BOLD)
    };
    // The stamp rides the title line, one size down from the name (#2802).
    // It belongs to the trailing cluster, not to the name's line box: that
    // way it shares the card's right edge with the pin, and the name yields
    // by truncating instead of the date painting over it.
    let stamp = (!indented).then(|| {
        div()
            .flex_none()
            .text_xs()
            .text_color(rgb(palette::current().text_muted))
            .child(SharedString::from(format!(
                "{} · {}",
                rail_stamp(stamp_at, chrono::Local::now()),
                rail_cost(cost)
            )))
    });
    // The pin rides the card's chip line, the way Telegram puts it beside the
    // topic cards (#2803, #2809). With topics attached it floats over the
    // strip's first line at the card's right edge; a card with no topics keeps
    // the pin on its name line so it never invents an empty row.
    let (title_pin, footer_pin) = if chips.is_some() {
        (None, pin_toggle)
    } else {
        (pin_toggle, None)
    };
    let title = div()
        .flex()
        .flex_row()
        .items_center()
        // One tight gap on each side of the name (#2800): the media sits 8px
        // from the first letter, and the trailing cluster the same distance
        // from the name. A single gap is enough now that the name yields by
        // truncating instead of overflowing the row; doubling it put the
        // picture too far from the title.
        .gap_2()
        .min_w_0()
        // The name line fills the body so the trailing cluster's `ml_auto`
        // has free space to push against. Without this it shrinks to its text
        // and the glyph parks right after the name (#2636 follow-up).
        .w_full()
        .when_some(marks, |d, marks| d.child(marks))
        .child(title_text)
        // The stamp, the pin and the activity glyph keep the right edge of
        // the name line. `ml_auto` carries them there in one group, and
        // `flex_none` holds their width while a long title truncates.
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .flex_none()
                .ml_auto()
                // The day-or-clock stamp leads the cluster, so it shares the
                // card's right edge with the pin instead of drifting (#2802).
                .when_some(stamp, |d, stamp| d.child(stamp))
                // Without topics the pin keeps this line; with topics it moves
                // down beside the chips (#2803).
                .when_some(title_pin, |d, pin| d.child(pin))
                // The activity glyph closes the cluster at the right edge
                // (#2636).
                .when_some(badge, |d, glyph| d.child(badge_cell(glyph))),
        );
    // The pin rides the strip's first line at the card's right edge, the way
    // Telegram pins a row (#2809). It floats over the strip instead of taking
    // a column beside it: a column charged the pin's width to every row (a
    // 28-topic card lost about 30px on all eleven) while a line of its own
    // pushed the whole strip down. The strip packs its own rows, so the first
    // one stops short of the pin and the rows below run edge to edge.
    let footer = match (chips, footer_pin) {
        (Some(strip), Some(pin)) => Some(
            div()
                .relative()
                .w_full()
                .min_w_0()
                .child(div().w_full().min_w_0().child(strip))
                .child(div().absolute().top_0().right_0().child(pin)),
        ),
        (Some(strip), None) => Some(div().w_full().child(strip)),
        (None, _) => None,
    };
    // A grouped topic is a compact pin beneath its session. Its title stays
    // visible, while the repeated date and cost metadata belongs to full rows.
    // Fill the remaining row width: `justify_between` alone left this
    // content-sized, so the cluster's `ml_auto` had nothing to consume and the
    // activity glyph never reached the right edge (#2636).
    let body = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .flex_1()
        .min_w_0()
        .child(title)
        // Topic chips ride inline under the title, on the parent card itself
        // (Telegram topic style). They never push a child out to its own row.
        .when_some(footer, |d, c| d.child(c))
        .when(!indented, |d| {
            // A turn in flight says what it is doing, in the preview's own
            // slot (#2718): the card would otherwise sit on the last
            // persisted message while a new conversation runs its first tool
            // calls, which reads as nothing happening at all. The line takes
            // the accent colour while it is live, so the card never claims
            // the preview is streaming, and it hands the slot back the
            // moment the turn settles.
            let live_line = activity.is_some();
            let preview = activity.or(preview);
            // The message line is pure conversation context: it paints only
            // when a readable user/assistant message exists. The ledger
            // stamp lives on the title line, so a quiet card simply stops
            // after its title (and chips, when topics are attached).
            d.when_some(preview, |d, line| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(rgb(if live_line {
                            palette::current().accent
                        } else {
                            palette::current().text_muted
                        }))
                        .truncate()
                        .child(line),
                )
            })
            // An approved plan reads as a slim progress bar, never as raw
            // plan JSON in the message line. Same ten cells as the composer
            // strip, so one plan has one look wherever it is drawn. A task
            // that just settled lifts the bar to the accent orange for a few
            // seconds, then it falls back to its resting teal.
            .when_some(plan, |d, (done, total)| {
                d.child(plan_progress_cell(done, total, flash))
            })
            // Detached commands close the card (#762): a session whose turn
            // ended while a build is still running says so here, under the
            // bar, instead of reading as idle for the whole build.
            .when_some(running, |d, line| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette::current().warn))
                        .truncate()
                        .child(line),
                )
            })
        });
    div()
        .id(SharedString::from(s.id.to_string()))
        .w_full()
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_1()
        .w_full()
        .p_2()
        .when(indented, |d| d.pl_4().pr_2().py_1())
        .cursor_pointer()
        // Every card paints the same soft panel, focused or not (#2792):
        // state reads from the small marks, never a big colour field. The
        // accent bar below is the only "you are here" cue, Telegram style.
        .bg(rgb(palette::current().panel))
        // The accent bar marks the open conversation, and only it: the
        // keyboard cursor gets no bar and no replacement cue. Selection
        // alone decides (#2717, overruling #2351's two-state design).
        .when(accent_bar_visible(selected, cursored), |d| {
            d.border_l_2().border_color(rgb(palette::current().accent))
        })
        // The hover tint is for rows that are not selected: on the open one
        // it would paint over the state and hide which conversation is open.
        .when(!selected, |d| d.hover(|d| d.bg(rgb(palette::current().bg))))
        .child(body)
}

/// One glyph cell at the right edge of a row's name line (#2636).
///
/// The cell carries `ml_auto`, but an auto margin only pushes when its
/// container is wider than its content. That is why `row` gives `body`
/// `flex_1().min_w_0()` and `body` gives `title` `w_full()`: without both, the
/// margin has zero free space and the glyph parks next to the name. This
/// function is deliberately unaware of layout, so the fill lives on the
/// containers rather than here.
///
/// Cyan, because that is the colour the running-work surfaces already use: the
/// same hue as the task strip, so "busy" reads the same wherever it is drawn.
fn badge_cell(glyph: &str) -> Div {
    div()
        .flex_none()
        .pl_1()
        .text_xs()
        .text_color(rgb(palette::current().teal))
        .child(SharedString::from(glyph.to_string()))
}

/// The plan's checklist as one slim line on the card: the same ten-cell bar
/// the composer strip draws, plus the done/total count. The bar math lives in
/// `plan_strip`, so a row and the strip can never disagree about one plan.
fn plan_progress_cell(done: usize, total: usize, flash: bool) -> Div {
    let (_, bar) = crate::plan_strip::progress_bar(done, total);
    // The theme's own accent is the orange: it stays legible on both the dark
    // and the paper palette, where a hardcoded orange would wash out.
    let bar_colour = if flash {
        palette::current().accent
    } else {
        palette::current().teal
    };
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .text_xs()
        .child(
            div()
                .flex_none()
                .text_color(rgb(bar_colour))
                .child(SharedString::from(bar)),
        )
        .child(
            div()
                .flex_none()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from(format!("{done}/{total}"))),
        )
}

/// The click behaviour for a row, typed the way `cx.listener` produces it.
pub fn on_click(
    row: Stateful<Div>,
    handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    row.on_click(handler)
}

const RAIL_FILTER_SVG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/assets/rail-filter.svg"
));

/// The rail's funnel button (#2787). An embedded vector stays crisp at any
/// size and takes the palette colour the same way the orientation icon does.
pub fn filter_icon(size: f32, colour: u32) -> impl IntoElement {
    let svg = String::from_utf8_lossy(RAIL_FILTER_SVG)
        .replace("#8a8a8a", &format!("#{colour:06x}"))
        .into_bytes();
    img(Arc::new(Image::from_bytes(ImageFormat::Svg, svg)))
        .w(rems(size))
        .h(rems(size))
        .flex_none()
        .object_fit(ObjectFit::Contain)
}

/// One chip of the rail filter menu (#2719). Pure styling: the view attaches
/// the click that flips the filter. The selected chip wears the accent so the
/// active rule reads at a glance; the rest stay quiet.
pub fn filter_chip(id: &'static str, label: &'static str, active: bool) -> Stateful<Div> {
    let shown = palette::current();
    let (bg, fg) = if active {
        (shown.accent, shown.panel)
    } else {
        (shown.panel_hi, shown.text_muted)
    };
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .px_2()
        .py_0p5()
        .rounded_sm()
        .text_xs()
        .bg(rgb(bg))
        .text_color(rgb(fg))
        .cursor_pointer()
        .hover(|d| d.bg(rgb(shown.panel_hi)))
        .child(SharedString::from(label))
}

/// The rail column: header and error slot from the state, then the caller's
/// prebuilt rows (each already carrying its click handler). Stateful, because
/// the scroll container needs an element id.
pub fn rail(
    state: &SessionsState,
    // Not `Stateful<Div>`: a row that waits on a plan is wrapped in the ring
    // frame (#2636), so the slot has to accept a wrapper as well as a bare row.
    rows: Vec<AnyElement>,
    width_rem: f32,
    scroll: &ScrollHandle,
    footer: Option<AnyElement>,
    // The filter menu (#2719), built by the view with its click handlers and
    // pinned to the top-right of the rail. None until the view supplies it.
    filter_control: Option<AnyElement>,
) -> Stateful<Div> {
    // No rail padding or inter-row gap: selection and hover paint edge to edge.
    let mut col = div()
        .id("session-rail")
        .flex()
        .flex_col()
        .overflow_y_scroll()
        .track_scroll(scroll)
        .flex_none()
        .w(rems(width_rem))
        .h_full()
        .bg(rgb(palette::current().panel))
        .py_2()
        .child(brand_header());

    // The filter menu rides top-right, just under the brand (#2719). The
    // element carries its own gaps and clicks; the rail only seats it.
    if let Some(control) = filter_control {
        col = col.child(div().px_2().pb_1().flex().justify_end().child(control));
    }

    if state.filter_active {
        col = col.child(div().px_2().pb_1().child(filter_bar(state)));
    }

    if let Some(err) = &state.error {
        col = col.child(
            div()
                .px_2()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(err.clone()),
        );
        return col;
    }

    if rows.is_empty() && !state.filter.trim().is_empty() {
        col = col.child(
            div()
                .px_2()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child("no chat matches"),
        );
        return col;
    }

    for row in rows {
        col = col.child(row);
    }
    // The channel chip (#2452) sits at the end of the rail: with a short
    // list it pins to the bottom edge, with a full list it waits at the
    // end of the scroll like any other row.
    if let Some(footer) = footer {
        col = col.child(div().w_full().mt_auto().px_2().pb_2().child(footer));
    }
    col
}

/// A compact brand header instead of a second session counter.
fn brand_header() -> Div {
    div()
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_3()
        .py_3()
        .border_b_1()
        .border_color(rgb(palette::current().panel_hi))
        .child(icon::banner_logo())
        .child(
            div()
                .text_sm()
                .text_color(rgb(palette::current().text_title))
                .child("OpenCrabs"),
        )
}

#[cfg(test)]
mod tests {
    use super::{
        Database, FindMatch, MenuKind, PlanFilter, RailFilter, SessionsState, accent_bar_visible,
        chip_width_at, filter_is_active, fit, icon_write_needed, load_sessions, merge_find_results,
        pack_rows, palette, pin_mark, preview_line, rail_cost, rail_filter_allows,
        rail_menu_entries, rail_stamp, strip_budgets,
    };
    use opencrabs::db::models::Session;
    use opencrabs::db::repository::ContentSearchRow;
    use opencrabs::db::repository::PREVIEW_CHARS;

    /// #2701: a topic sits directly under its parent as a chip. Pinning it
    /// does not eject it to a top-level row: the pin orders top-level rows
    /// only, and the topic stays grouped (and indented) under its parent.
    #[test]
    fn a_pinned_topic_stays_a_chip_under_its_parent() {
        let a = stamped(30, "A");
        let b = stamped(20, "B");
        let a1 = stamped(25, "A topic");
        let c = stamped(10, "C");
        let (a_id, b_id, a1_id, c_id) = (a.id, b.id, a1.id, c.id);
        let mut state = SessionsState {
            items: vec![a, b, a1, c],
            ..Default::default()
        };
        state.parents.insert(a1_id, a_id);

        let order: Vec<_> = state.ordered().map(|s| s.id).collect();
        assert_eq!(
            order,
            vec![a_id, a1_id, b_id, c_id],
            "the topic follows its parent"
        );
        assert!(state.indented_ids().contains(&a1_id));
        assert!(!state.indented_ids().contains(&a_id));
        assert_eq!(
            state.cursor_step(Some(a_id), 1),
            Some(a1_id),
            "the keyboard lands on the same visible topic pin the rail paints"
        );

        state.favorites.insert(a1_id);
        let order: Vec<_> = state.ordered().map(|s| s.id).collect();
        assert_eq!(
            order,
            vec![a_id, a1_id, b_id, c_id],
            "a pinned topic stays under its parent instead of ejecting to the top"
        );
        assert_eq!(order[0], a_id, "the parent, not the pinned topic, leads");
        assert!(
            state.indented_ids().contains(&a1_id),
            "a pinned topic is still a topic of its parent: the pin never drops the indent"
        );
    }

    /// #2701: topics paint as chips on the parent card, so they always stay in
    /// the ordered list and the keyboard can reach them. The row-era fold is
    /// gone from the GUI: a `topics_collapsed` flag the terminal writes must
    /// never blank the chip strip (the CrabsDev card showed no chips at all).
    #[test]
    fn topics_always_stay_listed_under_their_parent() {
        let a = stamped(30, "A");
        let a1 = stamped(20, "A topic");
        let b = stamped(10, "B");
        let (a_id, a1_id, b_id) = (a.id, a1.id, b.id);
        let mut state = SessionsState {
            items: vec![a, a1, b],
            ..Default::default()
        };
        state.parents.insert(a1_id, a_id);

        let ordered: Vec<_> = state.ordered().map(|s| s.id).collect();
        assert_eq!(
            ordered,
            vec![a_id, a1_id, b_id],
            "a topic always stays directly under its parent"
        );
        assert_eq!(
            state.cursor_step(Some(a_id), 1),
            Some(a1_id),
            "the arrow steps into the topic, never over it"
        );
    }

    /// The first row is narrower because the pin floats at its right end, and
    /// every row after it takes the whole width (#2809). The counts are what
    /// the strip slices the chips by, so a wrong one drops or repeats a chip.
    #[test]
    fn pack_rows_gives_the_pin_its_band_on_the_first_row_only() {
        let widths = [100.0; 7];
        // With the pin's band spent, the first row stops at two chips.
        assert_eq!(pack_rows(&widths, 4.0, 250.0, 400.0), vec![2, 3, 2]);
        // The same chips pack three to a row when the first row is as wide as
        // the rest: the band is the only difference the pin makes.
        assert_eq!(pack_rows(&widths, 4.0, 400.0, 400.0), vec![3, 3, 1]);
        // A chip wider than its whole budget still takes a row, never nothing.
        assert_eq!(pack_rows(&[500.0, 100.0], 4.0, 200.0, 200.0), vec![1, 1]);
        assert_eq!(pack_rows(&[], 4.0, 200.0, 200.0), Vec::<usize>::new());
    }

    /// A chip is its label plus padding; the live glyph and the focus ring are
    /// the only things that widen it (#2809). Measuring short is what put a chip
    /// under the pin, so every part of the box has to be counted.
    #[test]
    fn chip_width_counts_the_padding_the_glyph_and_the_ring() {
        let plain = chip_width_at(16.0, 40.0, false, false);
        assert!(
            plain > 16.0 + 40.0,
            "a chip carries padding on both sides: {plain}"
        );
        let glyph = chip_width_at(16.0, 40.0, true, false);
        assert!(glyph > plain, "a live glyph takes its own slot: {glyph}");
        let focused = chip_width_at(16.0, 40.0, false, true);
        assert!(
            (focused - plain - 2.0).abs() < 0.001,
            "the focus ring is a pixel a side: {focused}"
        );
    }

    /// The strip's width comes from the card, not from the chips: the row's
    /// padding, the media slot and the gap are spent before the chips see any
    /// space, and the first row spends the pin's band too (#2809).
    #[test]
    fn strip_budgets_leave_the_pin_its_band_on_the_first_row() {
        let (first, rest) = strip_budgets(16.0, 300.0);
        // 300 less 48 (row padding, media slot, gap) is the body.
        assert!(
            rest <= 252.0 && rest > 240.0,
            "rows below use the whole body: {rest}"
        );
        assert!(
            first < rest - 22.4,
            "the first row keeps the pin's square clear: {first} vs {rest}"
        );
        assert!(first > 0.0, "a normal rail still fits a chip: {first}");
    }

    /// #2271's whole class, seated: truncation walks chars, never bytes. Any
    /// return to byte-index slicing panics here on a multibyte boundary, and
    /// the sweep hits every max so no length dodges.
    #[test]
    fn fit_never_slices_bytes_on_multibyte_content() {
        let titles = [
            "🦀aé日🎉…b",
            "日本語のセッション",
            "emoji 🦀🦀🦀 tail",
            "plain ascii title",
            "",
            "café",
        ];
        for title in titles {
            let chars = title.chars().count();
            for max in 0..=chars + 2 {
                let out = fit(title, max);
                // Never wider than the budget, always valid to keep using
                // (a String already is; this documents the contract).
                assert!(
                    out.chars().count() <= max.max(1) || out == title,
                    "fit({title:?}, {max}) -> {out:?} overflows"
                );
                if chars > max {
                    assert!(
                        out.ends_with('…'),
                        "truncated fit({title:?}, {max}) lost the ellipsis"
                    );
                    assert!(
                        out.chars().count() <= max.max(1),
                        "fit({title:?}, {max}) -> {out:?} still overflows"
                    );
                } else {
                    assert_eq!(
                        out, title,
                        "untruncated fit({title:?}, {max}) changed the text"
                    );
                }
            }
        }
    }

    /// The 4 MB thread pattern, seated. The phase-0 crash: DB work running on
    /// a tiny stack (gpui's dispatch-queue workers) dies with SIGILL. If
    /// `load_sessions` ever regresses to doing its work on the calling
    /// thread, this 64 KB caller dies with it; with the pattern, the deep
    /// stack is the loader's business and a 64 KB caller passes.
    #[test]
    fn load_sessions_runs_on_a_tiny_caller_stack() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("rail-seat.db");
        let setup_path = db_path.clone();

        // Build the schema on the test's own (default) stack first: the seat
        // is about the fetch's stack discipline, not about migrations.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let db = Database::connect(&setup_path)
                .await
                .expect("connect temp db");
            db.run_migrations().await.expect("migrate temp db");
        });
        drop(rt);

        let handle = std::thread::Builder::new()
            .name("tiny-caller-seat".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("runtime");
                rt.block_on(load_sessions(db_path))
            })
            .expect("spawn tiny caller");
        let result = handle.join().expect("tiny caller must survive");
        let sessions = result.unwrap_or_else(|e| panic!("fetch against a fresh db failed: {e}"));
        assert!(sessions.is_empty(), "fresh db must list zero sessions");
    }

    /// One row of the ledger, built in memory: the ordering rule is about
    /// what the rail does with rows, never about how they were fetched.
    fn session(id: u128, title: &str) -> Session {
        let stamp = chrono::DateTime::from_timestamp(1_700_000_000 + id as i64, 0)
            .expect("a fixed stamp is valid");
        Session {
            id: uuid::Uuid::from_u128(id),
            title: Some(title.to_string()),
            model: None,
            provider_name: None,
            created_at: stamp,
            updated_at: stamp,
            archived_at: None,
            token_count: 0,
            total_cost: 0.0,
            working_directory: None,
            auto_title_attempted: false,
            project_id: None,
            channel_chat_key: None,
        }
    }

    /// A session stamped at a fixed instant, so an ordering test never depends
    /// on how fast the fixture loop ran. `Session::new` calls `Utc::now()`, and
    /// two rows created microseconds apart would then sort by creation speed
    /// instead of by the stamp the test set (#2815).
    fn stamped(secs: i64, title: &str) -> Session {
        let mut s = Session::new(Some(title.to_string()), None, None);
        s.updated_at = chrono::DateTime::from_timestamp(1_700_000_000 + secs, 0)
            .expect("a fixed stamp is valid");
        s
    }

    /// TUI parity on the pin: pins go on top through a stable partition, so a
    /// pin moves that one chat and leaves every other row in activity order.
    /// Sorting by title or id instead would reshuffle the rail under the
    /// cursor, which is why the key is the stamp, never a display field.
    #[test]
    fn favorites_pin_to_the_top_and_the_rest_follow_activity() {
        let mut state = SessionsState {
            items: vec![
                session(3, "newest"),
                session(2, "pinned one"),
                session(1, "oldest"),
            ],
            ..Default::default()
        };
        state.favorites.insert(uuid::Uuid::from_u128(2));

        let order: Vec<&str> = state
            .ordered()
            .map(|s| s.title.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(order, ["pinned one", "newest", "oldest"]);
    }

    /// A pin never resurrects a row the find field hid: the query narrows
    /// first, the pins only sort what survives it.
    #[test]
    fn a_pin_does_not_escape_the_rail_filter() {
        let mut state = SessionsState {
            items: vec![session(3, "newest"), session(2, "pinned one")],
            ..Default::default()
        };
        state.favorites.insert(uuid::Uuid::from_u128(2));
        state.filter = "newest".to_string();

        let order: Vec<&str> = state
            .ordered()
            .map(|s| s.title.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(order, ["newest"]);
        assert_eq!(state.matched(), 1);
    }

    /// A slide that comes up short is a click with an unsteady hand: the
    /// row snaps back and its menu stays shut.
    #[test]
    fn a_short_slide_snaps_back() {
        assert!(!super::slide_reveals(500.0, 500.0));
        assert!(!super::slide_reveals(
            500.0,
            500.0 - super::SLIDE_REVEAL_PX + 1.0
        ));
    }

    /// Past the threshold the menu is earned, and the threshold itself
    /// counts: the rule is "at least this far", not "further than".
    #[test]
    fn a_slide_past_the_threshold_opens_the_menu() {
        assert!(super::slide_reveals(500.0, 500.0 - super::SLIDE_REVEAL_PX));
        assert!(super::slide_reveals(
            500.0,
            500.0 - super::SLIDE_REVEAL_PX - 40.0
        ));
    }

    /// Rightward travel is not a slide, however far it goes.
    #[test]
    fn a_rightward_slide_never_opens_the_menu() {
        assert!(!super::slide_reveals(500.0, 520.0));
        assert!(!super::slide_reveals(500.0, 900.0));
    }

    /// The pin set is the sidecar's set: membership answers per row, and a
    /// session that was never pinned is not in it.
    #[test]
    fn the_pinned_set_answers_membership_by_id() {
        let mut state = SessionsState::default();
        state.favorites.insert(uuid::Uuid::from_u128(7));

        assert!(state.is_favorite(uuid::Uuid::from_u128(7)));
        assert!(!state.is_favorite(uuid::Uuid::from_u128(8)));
    }

    /// #2351's main path: Down walks the painted order, pinned rows first,
    /// and an unarmed cursor starts at the top so the first press lands on
    /// a real row.
    #[test]
    fn cursor_moves_through_the_ordered_rows() {
        let mut state = SessionsState {
            items: vec![session(1, "a"), session(2, "b"), session(3, "c")],
            ..Default::default()
        };
        // Pinning the last ledger row must not confuse the walk: the rail
        // paints it first, so the first Down lands on it.
        state.favorites.insert(uuid::Uuid::from_u128(3));

        let first = state.cursor_step(None, 1).expect("rows exist");
        assert_eq!(
            first,
            uuid::Uuid::from_u128(3),
            "pinned row is painted first"
        );

        let second = state.cursor_step(Some(first), 1).expect("rows exist");
        assert_eq!(
            second,
            uuid::Uuid::from_u128(2),
            "the rail paints newest activity first, so 2 follows the pinned 3"
        );

        let third = state.cursor_step(Some(second), 1).expect("rows exist");
        assert_eq!(third, uuid::Uuid::from_u128(1));

        // Up walks the same order backwards from an unarmed start.
        let up_first = state.cursor_step(None, -1).expect("rows exist");
        assert_eq!(up_first, uuid::Uuid::from_u128(3));
        assert_eq!(
            state.cursor_step(Some(up_first), -1),
            Some(uuid::Uuid::from_u128(3)),
            "up from the top row clamps, it does not wrap"
        );
    }

    /// Both ends stop, they never wrap into the opposite end: a wrap reads
    /// as a jump to a row the user cannot see.
    #[test]
    fn cursor_clamps_at_both_ends() {
        let state = SessionsState {
            items: vec![session(1, "a"), session(2, "b")],
            ..Default::default()
        };
        // The ends of the painted order, not of the ledger: the rail sorts
        // newest activity first, so the last row inserted is the first painted.
        let ids: Vec<uuid::Uuid> = state.ordered().map(|s| s.id).collect();
        let (first, last) = (ids[0], ids[ids.len() - 1]);
        assert_ne!(first, last, "the fixture paints two distinct rows");

        assert_eq!(state.cursor_step(Some(first), -1), Some(first));
        assert_eq!(state.cursor_step(Some(last), 1), Some(last));
    }

    /// A grouped card prints what it stands for (#2807): its own ledger row
    /// plus every topic grouped under it, so the money spent inside the chips
    /// shows on the card instead of vanishing from the rail.
    #[test]
    fn a_grouped_card_adds_its_topics_cost() {
        let parent = uuid::Uuid::from_u128(1);
        let mut parent_row = session(1, "OCDevCore");
        parent_row.total_cost = 100.0;
        let mut first = session(2, "topic one");
        first.total_cost = 50.0;
        let mut second = session(3, "topic two");
        second.total_cost = 25.5;
        let mut stray = session(4, "another chat");
        stray.total_cost = 900.0;
        let mut state = SessionsState {
            items: vec![parent_row, first, second, stray],
            ..Default::default()
        };
        state.parents.insert(uuid::Uuid::from_u128(2), parent);
        state.parents.insert(uuid::Uuid::from_u128(3), parent);

        let costs = state.grouped_costs();
        assert_eq!(
            costs.get(&parent).copied(),
            Some(175.5),
            "the card adds both topics and ignores a chat it does not own"
        );
        assert_eq!(
            costs.get(&uuid::Uuid::from_u128(9)).copied(),
            None,
            "a card with no topics prints only its own spend"
        );
    }

    /// A grouped card is stamped by the newest thing under it (#2815): a
    /// message lands on the session it was addressed to, so a topic that spoke
    /// a minute ago never touches the parent's row and the card printed a date
    /// from before its own chips ran.
    #[test]
    fn grouped_activity_takes_the_newest_stamp_over_the_topics() {
        let parent_row = session(1, "OCDevCore");
        let busy = session(5, "busy topic");
        let quiet = session(2, "quiet topic");
        let (parent_id, busy_stamp) = (parent_row.id, busy.updated_at);
        let mut state = SessionsState {
            items: vec![parent_row, busy, quiet],
            ..Default::default()
        };
        state.parents.insert(uuid::Uuid::from_u128(5), parent_id);
        state.parents.insert(uuid::Uuid::from_u128(2), parent_id);

        assert_eq!(
            state.grouped_activities().get(&parent_id).copied(),
            Some(busy_stamp),
            "the newest topic stamp is what the card prints, not the parent's own row"
        );

        // A card whose topics are all older than its own last message keeps its
        // own stamp: grouping can only ever move a card forward in time. The
        // parent therefore has to be the newer row here: `session(2)` outranks
        // `session(1)` on the fixture's stamp, so the topic below is genuinely
        // quieter only once the card itself spoke last.
        let mut spoke_last = session(1, "OCDevCore");
        spoke_last.updated_at =
            chrono::DateTime::from_timestamp(1_700_000_010, 0).expect("a fixed stamp is valid");
        let parent_stamp = spoke_last.updated_at;
        let mut quiet_only = SessionsState {
            items: vec![spoke_last, session(2, "quiet topic")],
            ..Default::default()
        };
        quiet_only
            .parents
            .insert(uuid::Uuid::from_u128(2), uuid::Uuid::from_u128(1));
        assert_eq!(
            quiet_only
                .grouped_activities()
                .get(&uuid::Uuid::from_u128(1))
                .copied(),
            Some(parent_stamp),
            "a quieter topic never pulls the card back in time"
        );
    }

    /// The rail's slot follows the same grouped stamp (#2815): a card whose
    /// topics are running right now outranks a card that spoke more recently on
    /// its own row, which is what "last activity" means once topics count.
    #[test]
    fn ordered_puts_a_card_with_a_busy_topic_first() {
        // Ledger order (newest own row first) is the chat, then the card, then
        // the card's topic. Grouped, the card's key is the topic's stamp, so it
        // leads, and the topic still follows it.
        let chat = session(30, "chat");
        let card = session(10, "OCDevCore");
        let topic = session(40, "busy topic");
        let (chat_id, card_id, topic_id) = (chat.id, card.id, topic.id);
        let mut state = SessionsState {
            items: vec![chat, card, topic],
            ..Default::default()
        };
        state.parents.insert(topic_id, card_id);

        let order: Vec<_> = state.ordered().map(|s| s.id).collect();
        assert_eq!(
            order,
            vec![card_id, topic_id, chat_id],
            "the card leads on its topic's stamp, and the topic stays under it"
        );
    }

    /// A card's chips follow the same clock as its title (#2853): the topics
    /// with work in flight lead, then the rest by their own stamp. Before this
    /// the strip rode whatever order the last ledger load returned, and that
    /// load only runs at turn boundaries, so a topic that had just started
    /// working kept the slot it held minutes earlier while its chip already
    /// showed the spinner.
    #[test]
    fn chips_with_work_in_flight_lead_the_strip() {
        let stamp = |secs: i64| {
            chrono::DateTime::from_timestamp(1_700_000_000 + secs, 0).expect("a fixed stamp")
        };
        let now = stamp(100);
        // The ledger load order at the moment the owner looked: UX had spoken
        // last, two idle topics sat above the topic that had just started.
        let mut chips = vec![
            RailChip {
                id: uuid::Uuid::from_u128(1),
                title: "UX".to_string(),
                at: stamp(90),
                busy: true,
            },
            RailChip {
                id: uuid::Uuid::from_u128(2),
                title: "Themes".to_string(),
                at: stamp(80),
                busy: false,
            },
            RailChip {
                id: uuid::Uuid::from_u128(3),
                title: "Meme".to_string(),
                at: stamp(70),
                busy: false,
            },
            RailChip {
                id: uuid::Uuid::from_u128(4),
                title: "MultiPane".to_string(),
                at: stamp(60),
                busy: true,
            },
        ];

        chips.sort_by_key(|c| chip_order_key(c, now));

        let order: Vec<&str> = chips.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(
            order,
            vec!["UX", "MultiPane", "Themes", "Meme"],
            "the two working topics lead, and the idle ones keep their stamp order"
        );
    }

    /// Two topics working at once tie on the paint's one clock, so the strip
    /// keeps their existing order instead of reshuffling them every frame, and
    /// idle chips still sort newest first (#2853).
    #[test]
    fn chips_that_tie_keep_their_place() {
        let stamp = |secs: i64| {
            chrono::DateTime::from_timestamp(1_700_000_000 + secs, 0).expect("a fixed stamp")
        };
        let now = stamp(100);
        let mut chips = vec![
            RailChip {
                id: uuid::Uuid::from_u128(1),
                title: "older idle".to_string(),
                at: stamp(10),
                busy: false,
            },
            RailChip {
                id: uuid::Uuid::from_u128(2),
                title: "newer idle".to_string(),
                at: stamp(50),
                busy: false,
            },
            RailChip {
                id: uuid::Uuid::from_u128(3),
                title: "first busy".to_string(),
                at: stamp(1),
                busy: true,
            },
            RailChip {
                id: uuid::Uuid::from_u128(4),
                title: "second busy".to_string(),
                at: stamp(2),
                busy: true,
            },
        ];

        chips.sort_by_key(|c| chip_order_key(c, now));

        let order: Vec<&str> = chips.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(
            order,
            vec!["first busy", "second busy", "newer idle", "older idle"],
            "busy chips tie on now, the rest sort by stamp"
        );
    }

    /// The strip really orders what it builds (#2853). Structural because the
    /// ordering is one call in the window's paint loop: deleting it compiles,
    /// reads fine, and quietly puts the chips back on the ledger load order.
    /// The busy flag must come from the same badge that paints the spinner, or
    /// the sort and the marker would disagree about which topics are working.
    #[test]
    fn the_strip_orders_the_chips_it_builds() {
        let source = include_str!("main.rs");
        let block = source
            .split("let mut children_by_parent")
            .nth(1)
            .and_then(|rest| rest.split("// Pinned rows first").next())
            .expect("the chip set build");
        assert!(
            block.contains("kids.sort_by_key(|chip| sessions::chip_order_key(chip, chips_now))"),
            "the chips must be ordered by the activity key, not left on the load order"
        );
        assert!(
            block.contains("busy: activity.get(&s.id).is_some_and(|st| st.badge.is_some())"),
            "the busy flag is the badge the chip paints, so the order matches the marker"
        );
        assert!(
            block.contains("at: s.updated_at"),
            "an idle chip sorts by the same stamp the card's title folds over"
        );
    }

    /// The card paints the grouped stamp and the card builder folds the topics
    /// in (#2815). Structural because swapping the field back to the row's own
    /// `updated_at` would compile and read perfectly well in review.
    #[test]
    fn the_card_paints_the_grouped_stamp_not_its_own_row() {
        let source = include_str!("sessions.rs");
        let row_source = source
            .split("pub fn row(")
            .nth(1)
            .expect("session row renderer")
            .split("pub fn on_click(")
            .next()
            .expect("row source before click handler");
        assert!(
            row_source.contains("rail_stamp(stamp_at"),
            "the stamp reads the grouped flag"
        );
        assert!(
            !row_source.contains("rail_stamp(s.updated_at"),
            "the card's own ledger row never stamps it"
        );
        let wired = include_str!("main.rs")
            .split("stamp_at:")
            .nth(1)
            .expect("the card builder wires stamp_at");
        assert!(
            wired.trim_start().starts_with("grouped_stamps.get(&id)"),
            "the builder folds the topics into the stamp it hands the row"
        );
    }

    /// A cursor on a row that is gone (deleted, filtered out) disarms
    /// instead of arming a ghost; a cursor on a surviving row stays put.
    #[test]
    fn cursor_survives_a_row_disappearing() {
        let mut state = SessionsState {
            items: vec![session(1, "a"), session(2, "b")],
            ..Default::default()
        };
        let first = uuid::Uuid::from_u128(1);
        let second = uuid::Uuid::from_u128(2);

        state.cursor = Some(second);
        state.items = vec![session(1, "a")];
        assert!(
            !state.cursor_visible(),
            "the cursor points at a removed row"
        );

        state.cursor = Some(first);
        state.items = vec![session(1, "a"), session(3, "c")];
        assert!(
            state.cursor_visible(),
            "a refetch that keeps the row keeps the cursor"
        );
    }

    /// A rename writes the icon only on a real change (#2791): a None draft
    /// keeps the stored icon, an equal draft skips, a different one writes.
    #[test]
    fn icon_write_needed_only_on_a_real_change() {
        // A None draft never overwrites the stored icon with the default.
        assert!(!icon_write_needed(Some("topic:ops"), None));
        assert!(!icon_write_needed(None, None));
        // The seeded draft equals the stored value: no write.
        assert!(!icon_write_needed(Some("topic:ops"), Some("topic:ops")));
        assert!(!icon_write_needed(
            Some("img:crab.png"),
            Some("img:crab.png")
        ));
        // A change writes, including the first icon on a bare session.
        assert!(icon_write_needed(Some("topic:ops"), Some("topic:dev")));
        assert!(icon_write_needed(None, Some("topic:ops")));
        assert!(icon_write_needed(Some("topic:ops"), Some("img:crab.png")));
    }

    #[test]
    fn pin_mark_is_filled_only_when_favorited() {
        let (filled, colour) = pin_mark(true);
        assert!(filled, "a pinned row carries the filled pushpin");
        assert_eq!(
            colour,
            palette::current().text_muted,
            "the pin stays a quiet mark at the date text's visual weight"
        );
        assert_ne!(
            colour,
            palette::current().accent,
            "the pin never borrows the accent (#2788)"
        );
        assert_ne!(
            pin_mark(false).0,
            filled,
            "an unpinned row must never look pinned"
        );
    }

    #[test]
    fn hover_mark_is_the_outline() {
        // Before the first pin there was no mark, no button and no hint, so
        // the row itself has to carry the invitation: the outline pushpin in
        // the faint colour, which turns filled the moment it is used (#2353).
        let (filled, colour) = pin_mark(false);
        assert!(!filled, "an unpinned row shows the outline pushpin");
        assert_eq!(
            colour,
            palette::current().text_faint,
            "the invitation stays quieter than the pin"
        );
    }

    /// The pin contract (#2788, #2803): one pushpin per row, and never before
    /// the name. A card with topics carries it on the chip line, the way
    /// Telegram does; a card with none keeps it on the name line. The star
    /// glyphs are gone from the module.
    #[test]
    fn the_pin_rides_the_trailing_cluster_not_the_title() {
        let source = include_str!("sessions.rs");
        let row_source = source
            .split("pub fn row(")
            .nth(1)
            .expect("session row renderer")
            .split("pub fn on_click(")
            .next()
            .expect("row source before click handler");
        let title_line = row_source
            .split(".when_some(footer,")
            .next()
            .expect("title line before the chip strip");
        assert!(
            !title_line.contains("= pin_mark("),
            "the title line never paints the pin before the name"
        );
        // Ahead of the name the row only opens its container. The pin is bound
        // above as `title_pin`/`footer_pin`, so what this checks is the paint
        // call, never the binding that decides which line carries it.
        let before_name = title_line
            .split(".child(title_text)")
            .next()
            .expect("row source before the name");
        assert!(
            !before_name.contains(".child(pin_toggle)")
                && !before_name.contains(".child(topic_pin_mark)"),
            "the row paints nothing before the name, pin included"
        );
        let name_line = title_line
            .split(".child(title_text)")
            .nth(1)
            .expect("name line after the title text")
            .split(".ml_auto()")
            .next()
            .expect("name line before the trailing cluster");
        assert!(
            !name_line.contains("pin_toggle")
                && !name_line.contains("pin_icon")
                && !name_line.contains("topic_pin_mark"),
            "no pin of any kind paints before the name, indented rows included"
        );
        // The cluster is one div: it opens its own `.child(` with the layout
        // it shares, and `ml_auto` is the last modifier that carries it to the
        // right edge. Slice from that opening, not from the push, or the
        // layout above `ml_auto` is outside the text this reads.
        let cluster_at = title_line
            .find(".ml_auto()")
            .expect("the cluster is pushed right");
        let cluster_open = title_line[..cluster_at]
            .rfind(".child(")
            .expect("the cluster opens on its own child");
        let cluster_end = title_line
            .find("let footer")
            .expect("the title expression ends before the chip strip");
        let cluster = &title_line[cluster_open..cluster_end];
        assert!(
            cluster.contains(".when_some(title_pin,"),
            "a card with no topics keeps the wired pin on its name line"
        );
        assert!(
            !cluster.contains("%m-%d %H:%M"),
            "the cluster never formats a date: the stamp arrives already formatted"
        );
        assert!(
            !cluster.contains("when_some(footer_pin,"),
            "the chip line owns the pin whenever topics are attached"
        );
        assert!(
            cluster.contains(".flex_row()") && cluster.contains(".gap_2()"),
            "the stamp, pin and unread badge share one horizontal cluster"
        );
        assert!(
            !cluster.contains(".flex_col()") && !cluster.contains(".items_end()"),
            "no vertical stamp or second metadata row remains"
        );
        assert!(
            !source.contains('\u{2605}') && !source.contains('\u{2606}'),
            "the star glyphs are gone: the pushpin replaces them"
        );
        // Read the module before its own tests: this assertion names the three
        // functions it forbids, so scanning the whole file finds those names in
        // this very string and fails on itself.
        let production = source
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("the module before its tests");
        assert!(
            !production.contains("pub fn pin_button(")
                && !production.contains("pub fn topic_button(")
                && !production.contains("pub fn delete_button("),
            "the slide-out strip stays removed; right-click and slide open the menu"
        );
    }

    #[test]
    fn the_pin_chord_follows_the_cursor_row() {
        let mut state = SessionsState::default();
        let open_chat = uuid::Uuid::from_u128(7);
        let cursored = uuid::Uuid::from_u128(8);

        state.selected = Some(open_chat);
        assert_eq!(
            state.pin_target(),
            Some(open_chat),
            "with nothing armed the chord pins the open chat"
        );

        state.cursor = Some(cursored);
        assert_eq!(
            state.pin_target(),
            Some(cursored),
            "the row the keyboard is on wins over the open chat"
        );
    }

    #[test]
    fn topic_cursor_alone_does_not_switch_activity() {
        let parent = uuid::Uuid::from_u128(11);
        let child = uuid::Uuid::from_u128(12);
        let sibling = uuid::Uuid::from_u128(13);
        let another_parent = uuid::Uuid::from_u128(14);
        let mut state = SessionsState {
            selected: Some(child),
            ..Default::default()
        };
        assert_eq!(
            state.progress_id(parent, [child, sibling]),
            child,
            "a selected child owns its parent's activity and preview"
        );

        state.selected = Some(parent);
        state.cursor = Some(child);
        assert_eq!(
            state.progress_id(parent, [child, sibling]),
            parent,
            "a cursor on the child cannot steal the selected parent's line"
        );

        state.selected = Some(another_parent);
        assert_eq!(
            state.progress_id(parent, [child, sibling]),
            parent,
            "a selection owned by another parent leaves this card alone"
        );

        state.selected = None;
        assert_eq!(
            state.progress_id(parent, [child, sibling]),
            parent,
            "cursor alone never switches the line"
        );
    }

    /// The menu offers the row actions (#2354): pin (unpin on a pinned row)
    /// and delete, delete never renamed. Topics off adds Enable topics; topics
    /// on adds New topic in the same slot (#2397).
    #[test]
    fn rail_menu_offers_pin_and_delete() {
        let off = rail_menu_entries(false, false, false);
        assert_eq!(
            off.len(),
            8,
            "pin, rename, assign project, enable topics, group topics, two splits and delete"
        );
        assert_eq!(off[0].label, "Pin");
        assert!(matches!(off[0].kind, MenuKind::Pin));
        assert_eq!(off[1].label, "Rename");
        assert!(matches!(off[1].kind, MenuKind::Rename));
        assert_eq!(off[2].label, "Assign project...");
        assert!(matches!(off[2].kind, MenuKind::AssignProject));
        assert_eq!(off[4].label, "Group topics...");
        assert!(matches!(off[4].kind, MenuKind::GroupTopics));
        assert_eq!(off[7].label, "Delete");
        assert!(matches!(off[7].kind, MenuKind::Delete));

        let on = rail_menu_entries(false, true, false);
        assert_eq!(
            on.len(),
            9,
            "pin, rename, assign project, new topic, disable topics, group topics, \
             two splits and delete"
        );
        assert_eq!(on[3].label, "New topic");
        assert!(matches!(on[3].kind, MenuKind::NewTopic));
        assert_eq!(on[4].label, "Disable topics...");
        assert!(matches!(on[4].kind, MenuKind::DisableTopics));
        assert_eq!(on[8].label, "Delete");

        let pinned = rail_menu_entries(true, true, false);
        assert_eq!(pinned[0].label, "Unpin", "a pinned row offers the way out");
        assert!(matches!(pinned[0].kind, MenuKind::Pin));
        assert_eq!(pinned[8].label, "Delete");
    }

    /// A rail row can take a new name from its own menu (#2668). The prompt
    /// already existed; only the entry that reaches it was missing.
    #[test]
    fn rail_menu_offers_rename_on_every_row() {
        for (favorited, topics) in [(false, false), (false, true), (true, true)] {
            let menu = rail_menu_entries(favorited, topics, false);
            let rename = menu
                .iter()
                .find(|e| matches!(e.kind, MenuKind::Rename))
                .expect("every row offers a rename");
            assert_eq!(rename.label, "Rename", "the label never varies");
        }
    }

    /// Split | and Split _ sit before Delete on every row, so a split starts
    /// from the row that was clicked (#2438).
    #[test]
    fn rail_menu_offers_both_splits_before_delete() {
        let menu = rail_menu_entries(false, true, false);
        let labels: Vec<&str> = menu.iter().map(|e| e.label).collect();
        let split = labels
            .iter()
            .position(|l| *l == "Split |")
            .expect("Split |");
        assert_eq!(labels[split + 1], "Split _");
        assert_eq!(labels.last().copied(), Some("Delete"));
        assert!(matches!(menu[split].kind, MenuKind::SplitSide));
        assert!(matches!(menu[split + 1].kind, MenuKind::SplitStack));
    }

    /// Enable topics is offered only on a session with topics off, between
    /// pin and delete (#2302).
    #[test]
    fn rail_menu_offers_enable_topics_only_while_topics_are_off() {
        let off = rail_menu_entries(false, false, false);
        assert_eq!(off.len(), 8);
        assert_eq!(off[3].label, "Enable topics");
        assert!(matches!(off[3].kind, MenuKind::EnableTopics));

        let on = rail_menu_entries(false, true, false);
        assert!(
            on.iter().all(|e| e.label != "Enable topics"),
            "a session with topics on cannot be flipped twice"
        );
    }

    /// Turning topics off is the only way back out, and it is offered exactly
    /// while they are on, next to New topic (#2863). It asks before writing,
    /// because the topics are folded into the session and cannot be split out
    /// again.
    #[test]
    fn rail_menu_offers_disable_topics_only_while_topics_are_on() {
        let on = rail_menu_entries(false, true, false);
        let disable = on
            .iter()
            .position(|e| matches!(e.kind, MenuKind::DisableTopics))
            .expect("topics on offers the way back out");
        assert_eq!(on[disable].label, "Disable topics...");
        assert_eq!(
            on[disable - 1].label,
            "New topic",
            "the two topic actions sit together"
        );

        let off = rail_menu_entries(false, false, false);
        assert!(
            off.iter()
                .all(|e| !matches!(e.kind, MenuKind::DisableTopics)),
            "a session with topics off has nothing to disable"
        );
    }

    /// The strip is drawn for a family, not for a flag (#2863). A session
    /// whose topics were all archived or folded still carries
    /// `topics_enabled`, and painting a bar whose only chip is `All` reads as
    /// a broken topic feature.
    #[test]
    fn a_family_with_no_live_topic_has_nothing_to_draw() {
        let mut state = SessionsState::default();
        let root = uuid::Uuid::from_u128(1);
        let topic = uuid::Uuid::from_u128(2);
        state.items = vec![session(1, "root")];
        state.topics.insert(root);
        assert!(
            !state.has_live_topics(root),
            "the flag is on but no topic is live, so there is no family"
        );

        state.items.push(session(2, "topic"));
        state.parents.insert(topic, root);
        assert!(
            state.has_live_topics(root),
            "one live child is a family again"
        );
    }

    /// Dismissing the menu writes nothing: no favorite flips, no delete
    /// arms, no cursor or selection moves. A close is an eraser (#2354).
    #[test]
    fn rail_menu_closes_without_writing() {
        let mut state = SessionsState::default();
        let pinned = uuid::Uuid::from_u128(2);
        state.items = vec![session(1, "a"), session(2, "pinned")];
        state.favorites.insert(pinned);
        state.cursor = Some(uuid::Uuid::from_u128(1));
        state.selected = Some(uuid::Uuid::from_u128(1));
        state.rail_menu = Some(pinned);

        state.close_rail_menu();

        assert!(state.rail_menu.is_none(), "the menu closed");
        assert!(
            state.is_favorite(pinned),
            "a dismissal must not unpin the row"
        );
        assert_eq!(
            state.cursor,
            Some(uuid::Uuid::from_u128(1)),
            "a dismissal must not move the cursor"
        );
        assert_eq!(
            state.selected,
            Some(uuid::Uuid::from_u128(1)),
            "a dismissal must not change the open chat"
        );
    }

    /// #2454: Enter in the find field opens what the field found. The armed
    /// row wins while it still matches; a row the query filtered out can
    /// never be opened behind the field.
    #[test]
    fn rail_find_enter_opens_the_armed_row() {
        let state = SessionsState {
            items: vec![session(1, "alpha"), session(2, "beta"), session(3, "gamma")],
            filter: "ga".to_string(),
            filter_active: true,
            cursor: Some(uuid::Uuid::from_u128(3)),
            ..Default::default()
        };
        assert_eq!(state.find_target(), Some(uuid::Uuid::from_u128(3)));
    }

    #[test]
    fn rail_find_enter_falls_back_to_the_top_match() {
        let mut state = SessionsState {
            items: vec![session(1, "alpha"), session(2, "beta"), session(3, "gamma")],
            filter: "ga".to_string(),
            filter_active: true,
            // Armed on a row the query no longer shows: the top match is the
            // only honest target.
            cursor: Some(uuid::Uuid::from_u128(2)),
            ..Default::default()
        };
        assert_eq!(state.find_target(), Some(uuid::Uuid::from_u128(3)));

        // Nothing matches: Enter opens nothing rather than the open chat.
        state.filter = "zzz".to_string();
        assert_eq!(state.find_target(), None);
    }

    /// #2540: selection and hover are full-width row tiles, not inset cards.
    /// The rail keeps its control insets while the rows themselves stay flush.
    #[test]
    fn session_rows_are_square_and_edge_to_edge() {
        let source = include_str!("sessions.rs");
        let row_source = source
            .split("pub fn row(")
            .nth(1)
            .expect("session row renderer")
            .split("pub fn on_click(")
            .next()
            .expect("row source before click handler");
        assert!(row_source.contains(".w_full()"));
        assert!(row_source.contains(".p_2()"), "text keeps its inset");
        assert!(!row_source.contains(".rounded_sm()"));

        let rail_source = source
            .split("pub fn rail(")
            .nth(1)
            .expect("session rail renderer")
            .split("#[cfg(test)]")
            .next()
            .expect("rail source before tests");
        assert!(rail_source.contains(".py_2()"));
        assert!(rail_source.contains(".px_2()"), "controls keep their inset");
        assert!(!rail_source.contains(".p_2()"));
        assert!(!rail_source.contains(".gap_1()"));
    }

    /// The bar is the open conversation's mark, never the cursor's: all
    /// four flag combinations, selection alone decides (#2717).
    #[test]
    fn the_accent_bar_follows_selection_never_the_cursor() {
        assert!(
            accent_bar_visible(true, true),
            "selected and cursored keeps the bar"
        );
        assert!(
            accent_bar_visible(true, false),
            "selected alone keeps the bar"
        );
        assert!(
            !accent_bar_visible(false, true),
            "a cursored row that is not the open session gets no bar"
        );
        assert!(!accent_bar_visible(false, false), "a quiet row gets no bar");
    }

    /// Structural pin: the row paints the bar through the selection-only
    /// decision, and the old cursor-gated paint is gone (#2717).
    #[test]
    fn the_row_bar_is_gated_by_selection_not_the_cursor() {
        let source = include_str!("sessions.rs");
        let row_source = source
            .split("pub fn row(")
            .nth(1)
            .expect("session row renderer")
            .split("pub fn on_click(")
            .next()
            .expect("row source before click handler");
        assert!(
            row_source.contains(".when(accent_bar_visible(selected, cursored),"),
            "the bar paints through the selection-only decision"
        );
        assert!(
            !row_source.contains(".when(cursored,"),
            "the cursor never gates a paint on the row"
        );
    }

    /// Telegram-style topics: a parent card mounts its children as an inline
    /// wrapping chip strip, and the paint loop skips those children as rows.
    /// Structural, the same source-split style as the #2636 badge pins.
    #[test]
    fn topic_children_paint_as_inline_chips_not_rows() {
        let source = include_str!("sessions.rs");
        let row_source = source
            .split("pub fn row(")
            .nth(1)
            .expect("session row renderer")
            .split("pub fn on_click(")
            .next()
            .expect("row source before click handler");
        assert!(
            row_source.contains(".when_some(footer,"),
            "the parent card mounts the chip strip inline under its title"
        );

        let strip_source = source
            .split("pub fn topic_chips(")
            .nth(1)
            .expect("chip strip renderer")
            .split("pub fn row(")
            .next()
            .expect("strip source before the row renderer");
        assert!(
            !strip_source.contains(".flex_wrap()"),
            "the strip packs its own rows, so the first one can stop short of the pin (#2809)"
        );
        assert!(strip_source.contains(".flex_col()"));
        assert!(strip_source.contains(".flex_row()"));

        let chip_source = source
            .split("pub fn topic_chip(")
            .nth(1)
            .expect("topic chip renderer")
            .split("pub fn count_chip(")
            .next()
            .expect("chip source before the overflow pill");
        assert!(chip_source.contains(".text_xs()"), "a chip is compact");
        assert!(
            chip_source.contains("chip_label(title)"),
            "chip title truncates to the chip budget"
        );
        assert!(
            chip_source.contains("linear_gradient("),
            "a running plan fills the chip like a progress bar (#2701)"
        );
        assert!(
            !row_source.contains(".when(holds_focus,"),
            "the focus wash is gone: every card paints the same panel (#2792)"
        );

        // The paint loop drops a chip child from the row stream, so it never
        // paints as its own row while its parent is visible.
        let main_source = include_str!("main.rs");
        assert!(
            main_source.contains("if chip_children.contains(&id)"),
            "the rail skips a child that paints as a chip"
        );

        // A pinned topic stays a chip: the chip set must never filter out
        // favorites, or they eject to top-level rows and scatter (#2701).
        let chip_block = main_source
            .split("let chip_children:")
            .nth(1)
            .expect("chip set resolver")
            .split("let mut children_by_parent")
            .next()
            .expect("chip set before the parent map");
        assert!(
            !chip_block.contains("is_favorite"),
            "the chip set never ejects a favorite: a pinned topic stays a chip"
        );
        assert!(
            !chip_block.contains("is_collapsed"),
            "the chip set never hides a folded parent's topics: chips always paint"
        );
        assert!(
            !crate::dashboard_source::DASHBOARD.contains("let holds_focus"),
            "the parent focus computation is removed, not masked (#2792)"
        );
    }

    /// Honest focus states (#2792): three clearly separate levels. Every
    /// card paints panel; hover is panel_hi on the chips and icons; the
    /// focused chip is track plus a 1px accent ring. No card background is
    /// painted from focus or selection.
    #[test]
    fn focus_states_are_small_marks_not_colour_fields() {
        let source = include_str!("sessions.rs");
        let row_source = source
            .split("pub fn row(")
            .nth(1)
            .expect("session row renderer")
            .split("pub fn on_click(")
            .next()
            .expect("row source before click handler");
        assert!(
            row_source.contains(".bg(rgb(palette::current().panel))"),
            "every card paints the same soft panel"
        );
        assert!(
            !row_source.contains("palette::current().focus")
                && !row_source.contains("palette::current().selection"),
            "no card background comes from the focus or selection wash"
        );
        let chip_source = source
            .split("pub fn topic_chip(")
            .nth(1)
            .expect("topic chip renderer")
            .split("pub fn count_chip(")
            .next()
            .expect("chip source before the overflow pill");
        assert!(
            chip_source.contains("if selected { pal.track } else { pal.panel }"),
            "the focused chip fills with track, one step off the panel"
        );
        assert!(
            chip_source.contains(".border_1().border_color(rgb(pal.accent))"),
            "the focused chip carries a 1px accent ring"
        );
        assert!(
            chip_source.contains(".hover(|d| d.bg(rgb(pal.panel_hi)))"),
            "chip hover is panel_hi, never the focused fill"
        );
        let count_source = source
            .split("pub fn count_chip(")
            .nth(1)
            .expect("overflow pill renderer")
            .split("pub fn topic_chips(")
            .next()
            .expect("pill source before the strip");
        assert!(
            count_source.contains(".hover(|d| d.bg(rgb(palette::current().panel_hi)))"),
            "the overflow pill hovers at panel_hi too"
        );
        let filter_source = source
            .split("fn filter_chip(")
            .nth(1)
            .expect("filter chip renderer")
            .split("pub fn rail(")
            .next()
            .expect("filter source before the rail column");
        assert!(
            filter_source.contains(".hover(|d| d.bg(rgb(shown.panel_hi)))"),
            "the filter chips hover at panel_hi too"
        );
    }

    /// Card contract: the stamp (clock today, calendar day before) rides the
    /// title line one size down from the name, inside the same right-aligned
    /// cluster as the pin, and the name yields by truncating instead of the
    /// date painting over it (#2802). The message context is a line below,
    /// under the chip strip when topics are attached.
    #[test]
    fn card_stamp_shares_the_title_line_without_overlapping_the_name() {
        let source = include_str!("sessions.rs");
        let row_source = source
            .split("pub fn row(")
            .nth(1)
            .expect("session row renderer")
            .split("pub fn on_click(")
            .next()
            .expect("row source before click handler");
        let title_line = row_source
            .split(".when_some(footer,")
            .next()
            .expect("title line before the chip strip");
        assert!(
            title_line.contains("rail_stamp(stamp_at"),
            "the stamp reads the shared day-or-clock formatter on the grouped stamp"
        );
        assert!(
            title_line.contains("rail_cost(cost)"),
            "the cost reads the shared compact formatter"
        );
        assert!(
            title_line.contains(".text_xs()"),
            "the stamp is one size down from the bold name"
        );
        assert!(
            !title_line.contains(".justify_end()"),
            "the stamp no longer owns a full-width line of its own"
        );
        assert!(
            !title_line.contains("%m-%d %H:%M"),
            "the stamp is formatted once, never re-formatted at paint time"
        );
        let cluster = title_line
            .split(".ml_auto()")
            .nth(1)
            .expect("the right-aligned cluster");
        assert!(
            cluster.contains(".when_some(stamp,"),
            "the stamp rides the trailing cluster, so it shares the card's right edge"
        );
        let name = title_line
            .split(".ml_auto()")
            .next()
            .expect("the name before the cluster");
        assert!(
            name.contains(".flex_1()") && name.contains(".min_w_0()"),
            "the name truncates, which is what stops the stamp painting over it"
        );
        assert!(
            row_source.contains(".when_some(preview,"),
            "the message line paints only when a preview exists"
        );
        assert!(
            !row_source.contains("None => SharedString::from(format!("),
            "no date/cost fallback on the message line"
        );
    }

    /// A card prints the clock for a session touched today and the calendar
    /// day for anything older: a rail lists recent work only, so a year and a
    /// date that is obviously today are noise.
    #[test]
    fn rail_stamp_shows_the_clock_today_and_the_day_before() {
        use chrono::TimeZone as _;
        let now = chrono::Local
            .with_ymd_and_hms(2026, 11, 11, 15, 5, 0)
            .unwrap();
        let today = chrono::Local
            .with_ymd_and_hms(2026, 11, 11, 9, 5, 0)
            .unwrap();
        assert_eq!(rail_stamp(today.with_timezone(&chrono::Utc), now), "09:05");
        let yesterday = chrono::Local
            .with_ymd_and_hms(2026, 11, 10, 7, 5, 0)
            .unwrap();
        assert_eq!(
            rail_stamp(yesterday.with_timezone(&chrono::Utc), now),
            "11-10"
        );
        let older = chrono::Local.with_ymd_and_hms(2026, 9, 1, 1, 0, 0).unwrap();
        assert_eq!(rail_stamp(older.with_timezone(&chrono::Utc), now), "09-01");
    }

    /// Cost is printed at rail scale: whole dollars, thousands shortened to
    /// one decimal of thousands, and cents kept only where they are the whole
    /// number.
    #[test]
    fn rail_cost_drops_cents_and_shortens_thousands() {
        assert_eq!(rail_cost(0.0), "$0");
        assert_eq!(rail_cost(2101.59), "$2.1k");
        assert_eq!(rail_cost(1000.0), "$1.0k");
        assert_eq!(rail_cost(107.51), "$108");
        assert_eq!(rail_cost(28.61), "$29");
        assert_eq!(rail_cost(0.45), "$0.45");
    }

    /// An approved plan paints as the strip's own progress bar on a slim line
    /// of its own, never as plan JSON in the message line. The bar math is
    /// reused from plan_strip so the card and the composer agree.
    #[test]
    fn card_plan_paints_the_shared_progress_bar() {
        let source = include_str!("sessions.rs");
        let row_source = source
            .split("pub fn row(")
            .nth(1)
            .expect("session row renderer")
            .split("pub fn on_click(")
            .next()
            .expect("row source before click handler");
        assert!(
            row_source.contains(".when_some(plan,"),
            "the plan bar paints only when an approved checklist exists"
        );
        assert!(
            row_source.contains("plan_progress_cell(done, total, flash)"),
            "the bar line delegates to the shared cell, flash state and all"
        );
        let cell_source = source
            .split("fn plan_progress_cell(")
            .nth(1)
            .expect("plan progress cell")
            .split("pub fn on_click(")
            .next()
            .expect("cell source before the click handler");
        assert!(
            cell_source.contains("crate::plan_strip::progress_bar(done, total)"),
            "the bar reuses the composer strip's math, not a second copy"
        );
        assert!(
            cell_source.contains("{done}/{total}"),
            "the done/total count rides the bar"
        );
    }

    /// The preview names the speaker the way the reader thinks of them and
    /// cuts long text on a character boundary with an ellipsis.
    #[test]
    fn preview_line_prefixes_sender_and_truncates() {
        assert_eq!(preview_line("user", "hello").to_string(), "You: hello");
        assert_eq!(preview_line("assistant", "hi").to_string(), "Crabs: hi");
        assert_eq!(preview_line("tool", "x").to_string(), "tool: x");

        let long = "x".repeat(PREVIEW_CHARS + 10);
        let line = preview_line("user", &long).to_string();
        assert!(line.ends_with('\u{2026}'), "long text gets an ellipsis");
        let body = line.strip_prefix("You: ").expect("sender prefix");
        assert_eq!(body.chars().count(), PREVIEW_CHARS);
    }

    /// A1: an untouched rail hides the cron and RSI noise but keeps humans
    /// and subagents. The default chip is All, so the plan gate lets every
    /// remaining row through (#2719).
    #[test]
    fn rail_filter_default_hides_cron_and_rsi() {
        let f = RailFilter::default();
        assert!(!f.show_automation, "automation starts hidden per A1");
        assert_eq!(f.plan, PlanFilter::All);
        assert!(rail_filter_allows(&f, "Fix the login bug", None));
        assert!(rail_filter_allows(&f, "subagent: search", None));
        assert!(!rail_filter_allows(&f, "daily recap [cron-job:7]", None));
        assert!(!rail_filter_allows(&f, "RSI autonomous cycle", None));
    }

    /// The toggle is the only thing that brings cron and RSI back; a renamed
    /// cron title still classifies as cron because the marker survives.
    #[test]
    fn rail_filter_toggle_shows_automation() {
        let f = RailFilter {
            show_automation: true,
            ..Default::default()
        };
        assert!(rail_filter_allows(&f, "daily recap [cron-job:7]", None));
        assert!(rail_filter_allows(&f, "RSI autonomous cycle", None));
        assert!(rail_filter_allows(&f, "renamed recap [cron-job:9]", None));
        assert!(rail_filter_allows(&f, "Fix the login bug", None));
    }

    /// The Active chip wants an approved checklist with work left. A finished
    /// checklist and a session with no plan both fall out.
    #[test]
    fn rail_filter_plan_active_needs_unfinished_checklist() {
        let f = RailFilter {
            plan: PlanFilter::Active,
            ..Default::default()
        };
        assert!(rail_filter_allows(&f, "chat", Some((2, 8))));
        assert!(rail_filter_allows(&f, "chat", Some((0, 1))));
        assert!(!rail_filter_allows(&f, "chat", Some((8, 8))));
        assert!(!rail_filter_allows(&f, "chat", None));
    }

    /// The Done chip wants a complete, non-empty checklist. An empty (0, 0)
    /// snapshot is not a finished plan, and neither is a session with none.
    #[test]
    fn rail_filter_plan_done_needs_complete_checklist() {
        let f = RailFilter {
            plan: PlanFilter::Done,
            ..Default::default()
        };
        assert!(rail_filter_allows(&f, "chat", Some((8, 8))));
        assert!(rail_filter_allows(&f, "chat", Some((1, 1))));
        assert!(!rail_filter_allows(&f, "chat", Some((2, 8))));
        assert!(!rail_filter_allows(&f, "chat", Some((0, 0))));
        assert!(!rail_filter_allows(&f, "chat", None));
    }

    /// The No Plan chip is the complement: only rows with no approved
    /// checklist show, whatever the counts on the others.
    #[test]
    fn rail_filter_plan_no_plan_hides_checklists() {
        let f = RailFilter {
            plan: PlanFilter::NoPlan,
            ..Default::default()
        };
        assert!(rail_filter_allows(&f, "chat", None));
        assert!(!rail_filter_allows(&f, "chat", Some((2, 8))));
        assert!(!rail_filter_allows(&f, "chat", Some((8, 8))));
    }

    /// The two gates compose: a cron row stays hidden under a plan chip even
    /// when its progress would pass, until the automation toggle is on.
    #[test]
    fn rail_filter_plan_chip_still_hides_automation() {
        let mut f = RailFilter {
            plan: PlanFilter::Active,
            ..Default::default()
        };
        assert!(!rail_filter_allows(&f, "recap [cron-job:1]", Some((1, 4))));
        f.show_automation = true;
        assert!(rail_filter_allows(&f, "recap [cron-job:1]", Some((1, 4))));
    }

    /// `visible()` wires both gates: the ctrl+f query and the filter menu.
    /// The plan snapshot decides the plan chip without a per-row file read,
    /// and the menu state lives on the struct so a repaint keeps it.
    /// The funnel's accent rule (#2787): only a filter that deviates from
    /// the default lights the button, so a closed panel never hides rows in
    /// silence.
    #[test]
    fn filter_is_active_only_when_the_rule_deviates() {
        assert!(!filter_is_active(&RailFilter::default()));
        assert!(filter_is_active(&RailFilter {
            plan: PlanFilter::Active,
            ..Default::default()
        }));
        assert!(filter_is_active(&RailFilter {
            plan: PlanFilter::Done,
            ..Default::default()
        }));
        assert!(filter_is_active(&RailFilter {
            plan: PlanFilter::NoPlan,
            ..Default::default()
        }));
        assert!(filter_is_active(&RailFilter {
            show_automation: true,
            ..Default::default()
        }));
    }

    /// The funnel contract (#2787): the chip row paints only while the panel
    /// is open, the funnel click toggles it, the accent follows the filter,
    /// and both Escape and a rail close shut the panel. The find query is a
    /// separate surface and stays untouched by the panel's Escape.
    #[test]
    fn rail_filter_panel_opens_from_the_funnel_only() {
        let main_source = include_str!("main.rs");
        let control = main_source
            .split("let rail_filter = self.sessions.rail_filter;")
            .nth(1)
            .expect("filter control block")
            .split("let rail = sessions::rail(")
            .next()
            .expect("control before the rail call");
        assert!(
            control.contains("if self.sessions.rail_filter_open {"),
            "the chip row paints only while the panel is open"
        );
        assert!(
            control.contains("sessions::filter_is_active(&rail_filter)"),
            "the funnel wears the accent while a non-default filter hides rows"
        );
        assert!(
            control.contains("this.sessions.rail_filter_open = !this.sessions.rail_filter_open"),
            "the funnel click toggles the panel"
        );
        let state_source = include_str!("sessions.rs");
        assert!(
            state_source.contains("pub rail_filter_open: bool"),
            "the panel state lives on the rail state, so it survives repaints"
        );
        let reset = main_source
            .split("fn set_rail_open_state(")
            .nth(1)
            .expect("rail open state")
            .split("#[cfg(test)]")
            .next()
            .expect("reset body");
        assert!(
            reset.contains("sessions.rail_filter_open = false;"),
            "closing the rail shuts the panel"
        );
        // Escape is decided in the dashboard's key handler, which the split
        // gave its own module (#2822).
        let keys_source = include_str!("dashboard_keys.rs");
        assert!(
            keys_source.contains(
                "if self.sessions.rail_filter_open && ev.keystroke.key.as_str() == \"escape\""
            ),
            "Escape closes the open panel before the find field sees it"
        );
    }

    #[test]
    fn rail_filter_visible_wires_query_and_menu() {
        let human = Session::new(Some("Fix login".into()), None, None);
        let cron = Session::new(Some("recap [cron-job:1]".into()), None, None);
        let planned = Session::new(Some("Big refactor".into()), None, None);
        let (hid, pid) = (human.id, planned.id);
        let mut state = SessionsState {
            items: vec![human, cron, planned],
            ..Default::default()
        };
        state.plan_progress.insert(pid, (8, 8));

        // Default: cron hidden, the two humans show.
        let ids: Vec<_> = state.visible().map(|s| s.id).collect();
        assert_eq!(ids, vec![hid, pid]);

        // The state persists across calls (a repaint does not reset it).
        assert_eq!(state.visible().count(), 2);

        // Done chip narrows to the finished plan; the query then narrows more.
        state.rail_filter.plan = PlanFilter::Done;
        let ids: Vec<_> = state.visible().map(|s| s.id).collect();
        assert_eq!(ids, vec![pid]);
        state.filter = "login".into();
        assert_eq!(state.visible().count(), 0, "query and menu both gate");

        // Clearing the query leaves the menu in place; toggling automation
        // brings cron back under the All chip.
        state.filter.clear();
        state.rail_filter = RailFilter {
            show_automation: true,
            ..Default::default()
        };
        assert_eq!(state.visible().count(), 3);
    }

    /// One content-search row, built in memory: the merge rule is about what
    /// the rail does with hits, never about how they were fetched (#2720).
    fn content_row(id: u128, snippet: &str) -> ContentSearchRow {
        ContentSearchRow {
            session_id: uuid::Uuid::from_u128(id),
            title: format!("t{id}"),
            snippet: snippet.to_string(),
            match_count: 1,
            last_hit_at: chrono::DateTime::from_timestamp(1_700_000_000 + id as i64, 0)
                .expect("a fixed stamp is valid"),
        }
    }

    /// A session found by both title and content appears once, and the
    /// content snippet wins so the reader sees why it matched (#2720).
    #[test]
    fn rail_find_merge_dedups_a_session_found_both_ways() {
        let merged = merge_find_results(
            vec![uuid::Uuid::from_u128(1)],
            &[content_row(1, "the hit line")],
        );
        assert_eq!(
            merged,
            vec![FindMatch {
                session_id: uuid::Uuid::from_u128(1),
                snippet: Some("the hit line".to_string()),
            }]
        );
    }

    /// Title matches keep the ledger order and lead; content-only sessions
    /// follow in the order the search returned them (newest first).
    #[test]
    fn rail_find_merge_keeps_title_order_then_content() {
        let merged = merge_find_results(
            vec![uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(2)],
            &[content_row(3, "c"), content_row(4, "d")],
        );
        let ids: Vec<_> = merged.iter().map(|m| m.session_id).collect();
        assert_eq!(
            ids,
            vec![
                uuid::Uuid::from_u128(1),
                uuid::Uuid::from_u128(2),
                uuid::Uuid::from_u128(3),
                uuid::Uuid::from_u128(4),
            ]
        );
        assert!(merged[0].snippet.is_none(), "title-only has no snippet");
        assert_eq!(merged[2].snippet.as_deref(), Some("c"));
    }

    /// A title match with no content hit keeps its own preview (no snippet);
    /// a content-only hit carries its snippet.
    #[test]
    fn rail_find_merge_snippet_only_on_content_hits() {
        let merged = merge_find_results(
            vec![uuid::Uuid::from_u128(1)],
            &[content_row(2, "deep hit")],
        );
        assert_eq!(merged[0].snippet, None);
        assert_eq!(merged[1].snippet.as_deref(), Some("deep hit"));
        assert_eq!(merged[1].session_id, uuid::Uuid::from_u128(2));
    }

    /// Empty inputs merge to nothing, so a cleared field shows no deep hits.
    #[test]
    fn rail_find_merge_empty_inputs() {
        assert!(merge_find_results(Vec::new(), &[]).is_empty());
    }

    /// A repeated title id is collapsed too: the ledger can hand the same id
    /// twice and the rail must not draw it twice.
    #[test]
    fn rail_find_merge_dedups_repeated_title_ids() {
        let merged = merge_find_results(
            vec![uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(1)],
            &[],
        );
        assert_eq!(merged.len(), 1);
    }

    /// `visible()` keeps a content-only match on screen even when its title
    /// misses the query, and the snippet map is what holds it there (#2720).
    #[test]
    fn rail_find_visible_includes_content_only_matches() {
        let by_title = Session::new(Some("needle chat".into()), None, None);
        let by_content = Session::new(Some("unrelated name".into()), None, None);
        let miss = Session::new(Some("nothing here".into()), None, None);
        let (tid, cid, mid) = (by_title.id, by_content.id, miss.id);
        let mut state = SessionsState {
            items: vec![by_title, by_content, miss],
            filter: "needle".to_string(),
            filter_active: true,
            ..Default::default()
        };
        // Title alone finds only the first row.
        let ids: Vec<_> = state.visible().map(|s| s.id).collect();
        assert_eq!(ids, vec![tid]);

        // A deep hit on the second row brings it in; the third still misses.
        state
            .content_matches
            .insert(cid, "a needle in the messages".into());
        let ids: Vec<_> = state.visible().map(|s| s.id).collect();
        assert_eq!(ids, vec![tid, cid]);
        assert!(!ids.contains(&mid));

        // Clearing the deep hits drops the content-only row back out.
        state.content_matches.clear();
        let ids: Vec<_> = state.visible().map(|s| s.id).collect();
        assert_eq!(ids, vec![tid]);
    }
}
