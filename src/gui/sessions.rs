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
    AnyElement, AnyView, App, ClickEvent, Context, Div, Image, ImageFormat, InteractiveElement,
    ObjectFit, Render, ScrollHandle, SharedString, Stateful, Styled, Window, div, img,
    linear_color_stop, linear_gradient, prelude::*, rems, rgb,
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
    /// scrolling rail, and the slide strip already behaves this way.
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

    /// Pinned first, then everything else, each group keeping the ledger's
    /// order. TUI parity: `SessionService::session_favorites` feeds a stable
    /// `sort_by_key(|s| !pins.contains(&s.id))`, so a pin moves a chat to the
    /// top without disturbing the order inside either group.
    ///
    /// A topic sits directly under its parent when that parent is in view
    /// (#2303). A pinned topic stays a chip under its parent: the pin orders
    /// top-level rows only, it never ejects a topic to the top level (#2701).
    pub fn ordered(&self) -> impl Iterator<Item = &Session> {
        let visible: Vec<&Session> = self.visible().collect();
        let in_view = |id: uuid::Uuid| visible.iter().any(|s| s.id == id);
        let grouped = |s: &Session| self.parents.get(&s.id).is_some_and(|p| in_view(*p));
        let (mut pinned, rest): (Vec<&Session>, Vec<&Session>) = visible
            .iter()
            .copied()
            .filter(|s| !grouped(s))
            .partition(|s| self.is_favorite(s.id));
        pinned.extend(rest);
        let mut out: Vec<&Session> = Vec::with_capacity(visible.len());
        for top in pinned {
            out.push(top);
            // Topic pins are always visible beneath their parent. The arrow
            // keys walk this exact order, so no cursor can target a hidden row.
            out.extend(
                visible
                    .iter()
                    .copied()
                    .filter(|s| self.parents.get(&s.id) == Some(&top.id) && grouped(s)),
            );
        }
        out.into_iter()
    }

    /// Whether a row is a topic shown under its parent, so it is indented.
    pub fn is_closed(&self, id: uuid::Uuid) -> bool {
        self.closed.contains(&id)
    }

    pub fn is_indented(&self, id: uuid::Uuid) -> bool {
        // Indentation tracks chip-strip membership, not the pin: a pinned
        // topic is still a topic of its parent, so the favorite flag must not
        // drop its indent (#2670 regression). The pin keeps its role in
        // ordering only (pinned first), never in whether a row is grouped.
        self.parents
            .get(&id)
            .is_some_and(|p| self.visible().any(|s| s.id == *p))
    }

    /// Whether this session is pinned right now.
    pub fn is_favorite(&self, id: uuid::Uuid) -> bool {
        self.favorites.contains(&id)
    }

    pub fn is_topic_enabled(&self, id: uuid::Uuid) -> bool {
        self.topics.contains(&id)
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

const PIN_SVG: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/assets/pin.svg"));
const PIN_OUTLINE_SVG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/assets/pin-outline.svg"
));

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

/// The menu's contents, resolved from the row's pin state. Pure, so the
/// labels are pinned by test and the render only draws what this returns
/// (#2354).
pub fn rail_menu_entries(favorited: bool, topics_enabled: bool) -> Vec<MenuEntry> {
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
    // Topics are opt-in per session (#2302): the item is offered only while
    // they are off, so it cannot be flipped twice by accident. Once they are
    // on, the same slot offers a new topic under the row (#2397).
    if topics_enabled {
        entries.push(MenuEntry {
            kind: MenuKind::NewTopic,
            label: "New topic",
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
/// Not Clone: the custom icon is a built element, and elements are moved,
/// not copied. The row takes the flags by value, so nothing needs a clone.
#[derive(Default)]
pub struct RowFlags {
    pub selected: bool,
    pub cursored: bool,
    pub favorited: bool,
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
    /// The session's stored icon, already built by the view (#2790): an
    /// embedded mark, a custom image or a legacy glyph. When set it wins
    /// over the platform mark in the leading slot. The module never reads
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
    chip.child(div().truncate().child(SharedString::from(fit(title, 14))))
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

/// The chip strip under a parent card's title: the visible topic chips on one
/// wrapping line. The parent id keys the strip so two parents never share an
/// element id.
pub fn topic_chips(id: uuid::Uuid, chips: Vec<Stateful<Div>>) -> Stateful<Div> {
    let mut strip = div()
        .id(SharedString::from(format!("topic-chips-{id}")))
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap_1()
        .w_full();
    for chip in chips {
        strip = strip.child(chip);
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

pub fn row(
    s: &Session,
    flags: RowFlags,
    action: Option<Stateful<Div>>,
    chips: Option<Stateful<Div>>,
) -> Stateful<Div> {
    let RowFlags {
        selected,
        cursored,
        favorited,
        indented,
        closed,
        badge,
        preview,
        activity,
        plan,
        flash,
        running,
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
    // The stored icon wins over the platform mark (#2790): the owner picked
    // it for this session, so it leads the row. Without one, a channel card
    // still prints its platform; a row without a pick has no mark.
    let mark = icon.or_else(|| {
        rail::platform_of(stored.filter(|_| !indented))
            .and_then(|platform| platform_icons::platform_icon(platform, 1.0))
    });
    let title_text = div()
        // A closed topic reads dimmed: still there, no longer taking work.
        .text_color(rgb(if closed {
            palette::current().text_faint
        } else {
            palette::current().text
        }))
        .truncate()
        .child(SharedString::from(fit(&label, 42)));
    let title_text = if indented {
        title_text.text_xs()
    } else {
        title_text.text_sm()
    };
    let title = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .min_w_0()
        // The name line fills the body so the trailing cluster's `ml_auto`
        // has free space to push against. Without this it shrinks to its text
        // and the glyph parks right after the name (#2636 follow-up).
        .w_full()
        .when_some(mark, |d, mark| d.child(mark))
        .child(title_text)
        // Timestamp, cost, the pin and the activity glyph share ONE
        // right-aligned cluster: a single auto margin carries the whole
        // group to the edge. Two auto margins (stamp and badge each their
        // own) would split the free space and park the stamp mid-row on a
        // streaming session.
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .flex_none()
                .ml_auto()
                .when(!indented, |d| {
                    // Telegram's quiet column (#2788): the date/cost stamp
                    // above, the pushpin below, both right-aligned. The
                    // message line below stays pure conversation context.
                    d.child(
                        div()
                            .flex()
                            .flex_col()
                            .items_end()
                            .flex_none()
                            .pl_1()
                            .gap_0p5()
                            .child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(rgb(palette::current().text_muted))
                                    .child(SharedString::from(format!(
                                        "{} · ${:.2}",
                                        s.updated_at
                                            .with_timezone(&chrono::Local)
                                            .format("%m-%d %H:%M"),
                                        s.total_cost
                                    ))),
                            )
                            .child(pin_icon(favorited, 0.9)),
                    )
                })
                // A grouped topic row keeps its compact pin inline, without
                // the ledger metadata repeated on every child (#2788).
                .when(indented, |d| d.child(pin_icon(favorited, 0.9)))
                // The activity glyph closes the cluster at the right edge
                // (#2636). `flex_none` keeps it its width when a long title
                // truncates.
                .when_some(badge, |d, glyph| d.child(badge_cell(glyph))),
        );
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
        .when_some(chips, |d, c| d.child(c))
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
    let mut row = div()
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
        .child(body);
    if let Some(action) = action {
        row = row.child(action);
    }
    row
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

/// One icon in a row's slide-out strip. Icons only, so the strip stays small
/// next to the title: the title keeps its room, and the glyph carries the
/// action (#2384).
/// The hover label of a slide icon: a small card naming the action (#2392).
struct IconTip(SharedString);

impl Render for IconTip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_sm()
            .border_1()
            .border_color(rgb(palette::current().panel_hi))
            .bg(rgb(palette::current().panel))
            .text_xs()
            .text_color(rgb(palette::current().text))
            .child(self.0.clone())
    }
}

fn action_icon(id: &'static str, icon: AnyElement, label: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .tooltip(move |_window, cx| -> AnyView {
            cx.new(|_| IconTip(SharedString::from(label))).into()
        })
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(rems(1.6))
        .rounded_sm()
        .cursor_pointer()
        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
        .child(icon)
}

/// A text glyph shaped for the slide-out strip, so the vector icons and the
/// glyph icons share one cell style.
fn glyph_icon(glyph: &'static str, color: u32) -> AnyElement {
    div()
        .text_sm()
        .text_color(rgb(color))
        .child(SharedString::from(glyph))
        .into_any_element()
}

/// The pin icon of a row's slide-out strip (#2788): the same quiet pushpin
/// the row carries, filled and muted once pinned, outline and faint before.
/// The tooltip names the action the click performs, matching the rail menu.
pub fn pin_button(favorited: bool) -> Stateful<Div> {
    let (id, label) = if favorited {
        ("session-unpin", "Unpin")
    } else {
        ("session-pin", "Pin")
    };
    action_icon(id, pin_icon(favorited, 1.0).into_any_element(), label)
}

/// The topic icon of a row's slide-out strip. With topics off it turns them
/// on; with topics on it opens a new topic under the session (#2302, #2303).
pub fn topic_button(topics_on: bool) -> Stateful<Div> {
    let color = if topics_on {
        palette::current().accent
    } else {
        palette::current().text_muted
    };
    action_icon("session-topic", glyph_icon("\u{2442}", color), "Topic")
}

/// The delete icon of a row's slide-out strip. It never deletes on its own:
/// the view routes every delete path through one confirmation dialog, so a
/// mis-swipe cannot cost a conversation.
pub fn delete_button() -> Stateful<Div> {
    action_icon(
        "session-delete",
        glyph_icon("\u{2715}", palette::current().red),
        "Delete",
    )
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
        filter_is_active, fit, icon_write_needed, load_sessions, merge_find_results, palette,
        pin_mark, preview_line, rail_filter_allows, rail_menu_entries,
    };
    use opencrabs::db::models::Session;
    use opencrabs::db::repository::ContentSearchRow;
    use opencrabs::db::repository::PREVIEW_CHARS;

    /// #2701: a topic sits directly under its parent as a chip. Pinning it
    /// does not eject it to a top-level row: the pin orders top-level rows
    /// only, and the topic stays grouped (and indented) under its parent.
    #[test]
    fn a_pinned_topic_stays_a_chip_under_its_parent() {
        let a = Session::new(Some("A".into()), None, None);
        let b = Session::new(Some("B".into()), None, None);
        let a1 = Session::new(Some("A topic".into()), None, None);
        let c = Session::new(Some("C".into()), None, None);
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
        assert!(state.is_indented(a1_id));
        assert!(!state.is_indented(a_id));
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
            state.is_indented(a1_id),
            "a pinned topic is still a topic of its parent: the pin never drops the indent"
        );
    }

    /// #2701: topics paint as chips on the parent card, so they always stay in
    /// the ordered list and the keyboard can reach them. The row-era fold is
    /// gone from the GUI: a `topics_collapsed` flag the terminal writes must
    /// never blank the chip strip (the CrabsDev card showed no chips at all).
    #[test]
    fn topics_always_stay_listed_under_their_parent() {
        let a = Session::new(Some("A".into()), None, None);
        let a1 = Session::new(Some("A topic".into()), None, None);
        let b = Session::new(Some("B".into()), None, None);
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

    /// TUI parity: pins go on top through a stable partition, so a pin moves
    /// that one chat and leaves every other row where the ledger put it.
    /// Sorting by title or id instead would reshuffle the rail under the
    /// cursor, which is why the rule is "pinned first, ledger order inside
    /// each group" and not "sort".
    #[test]
    fn favorites_pin_to_the_top_and_leave_the_rest_in_ledger_order() {
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
    /// row snaps back and its strip stays shut.
    #[test]
    fn a_short_slide_snaps_back() {
        assert!(!super::slide_reveals(500.0, 500.0));
        assert!(!super::slide_reveals(
            500.0,
            500.0 - super::SLIDE_REVEAL_PX + 1.0
        ));
    }

    /// Past the threshold the strip is earned, and the threshold itself
    /// counts: the rule is "at least this far", not "further than".
    #[test]
    fn a_slide_past_the_threshold_opens_the_strip() {
        assert!(super::slide_reveals(500.0, 500.0 - super::SLIDE_REVEAL_PX));
        assert!(super::slide_reveals(
            500.0,
            500.0 - super::SLIDE_REVEAL_PX - 40.0
        ));
    }

    /// Rightward travel is not a slide, however far it goes.
    #[test]
    fn a_rightward_slide_never_opens_the_strip() {
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
        assert_eq!(second, uuid::Uuid::from_u128(1));

        let third = state.cursor_step(Some(second), 1).expect("rows exist");
        assert_eq!(third, uuid::Uuid::from_u128(2));

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
        let first = uuid::Uuid::from_u128(1);
        let last = uuid::Uuid::from_u128(2);

        assert_eq!(state.cursor_step(Some(first), -1), Some(first));
        assert_eq!(state.cursor_step(Some(last), 1), Some(last));
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

    /// The pin contract (#2788): one pushpin per row, in the trailing
    /// cluster and never before the name. A full row stacks the stamp above
    /// the pin, an indented topic keeps it inline, and the star glyphs are
    /// gone from the module.
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
            .split(".when_some(chips,")
            .next()
            .expect("title line before the chip strip");
        assert!(
            !title_line.contains("= pin_mark(favorited)"),
            "the title line never paints the pin before the name"
        );
        let name_line = title_line
            .split(".ml_auto()")
            .next()
            .expect("name line before the trailing cluster");
        assert!(
            !name_line.contains("pin_icon") && !name_line.contains("topic_pin_mark"),
            "no pin of any kind paints before the name, indented rows included"
        );
        let cluster = title_line
            .split(".ml_auto()")
            .nth(1)
            .expect("trailing cluster");
        assert!(
            cluster.contains("pin_icon(favorited, 0.9)"),
            "the pin rides the one right-aligned cluster"
        );
        let stamp = cluster.find("%m-%d %H:%M").expect("date/cost stamp");
        let pin = cluster
            .find("pin_icon(favorited, 0.9)")
            .expect("pushpin under the stamp");
        assert!(
            stamp < pin,
            "the stamp paints above the pin on a full row, Telegram style"
        );
        assert!(
            cluster.contains(".flex_col()") && cluster.contains(".items_end()"),
            "a full row stacks the stamp above the pin, Telegram style"
        );
        assert!(
            !source.contains('\u{2605}') && !source.contains('\u{2606}'),
            "the star glyphs are gone: the pushpin replaces them"
        );
        let button = source
            .split("pub fn pin_button(")
            .nth(1)
            .expect("pin button")
            .split("pub fn topic_button(")
            .next()
            .expect("button before the topic button");
        assert!(
            button.contains("pin_icon(favorited, 1.0)"),
            "the strip speaks the same pin language as the row"
        );
        assert!(
            button.contains("\"Unpin\"") && button.contains("\"Pin\""),
            "the tooltip names the action, matching the rail menu"
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

    /// The menu offers the row actions (#2354): pin (unpin on a pinned row)
    /// and delete, delete never renamed. Topics off adds Enable topics; topics
    /// on adds New topic in the same slot (#2397).
    #[test]
    fn rail_menu_offers_pin_and_delete() {
        let off = rail_menu_entries(false, false);
        assert_eq!(
            off.len(),
            7,
            "pin, rename, enable topics, group topics, two splits and delete"
        );
        assert_eq!(off[0].label, "Pin");
        assert!(matches!(off[0].kind, MenuKind::Pin));
        assert_eq!(off[1].label, "Rename");
        assert!(matches!(off[1].kind, MenuKind::Rename));
        assert_eq!(off[3].label, "Group topics...");
        assert!(matches!(off[3].kind, MenuKind::GroupTopics));
        assert_eq!(off[6].label, "Delete");
        assert!(matches!(off[6].kind, MenuKind::Delete));

        let on = rail_menu_entries(false, true);
        assert_eq!(
            on.len(),
            7,
            "pin, rename, new topic, group topics, two splits and delete"
        );
        assert_eq!(on[2].label, "New topic");
        assert!(matches!(on[2].kind, MenuKind::NewTopic));
        assert_eq!(on[6].label, "Delete");

        let pinned = rail_menu_entries(true, true);
        assert_eq!(pinned[0].label, "Unpin", "a pinned row offers the way out");
        assert!(matches!(pinned[0].kind, MenuKind::Pin));
        assert_eq!(pinned[6].label, "Delete");
    }

    /// A rail row can take a new name from its own menu (#2668). The prompt
    /// already existed; only the entry that reaches it was missing.
    #[test]
    fn rail_menu_offers_rename_on_every_row() {
        for (favorited, topics) in [(false, false), (false, true), (true, true)] {
            let menu = rail_menu_entries(favorited, topics);
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
        let menu = rail_menu_entries(false, true);
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
        let off = rail_menu_entries(false, false);
        assert_eq!(off.len(), 7);
        assert_eq!(off[2].label, "Enable topics");
        assert!(matches!(off[2].kind, MenuKind::EnableTopics));

        let on = rail_menu_entries(false, true);
        assert!(
            on.iter().all(|e| e.label != "Enable topics"),
            "a session with topics on cannot be flipped twice"
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
            row_source.contains(".when_some(chips,"),
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
            strip_source.contains(".flex_wrap()"),
            "chips wrap on one line"
        );
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
            chip_source.contains("fit(title, 14)"),
            "chip title truncates"
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
            !main_source.contains("let holds_focus"),
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
        let action_source = source
            .split("fn action_icon(")
            .nth(1)
            .expect("action icon renderer")
            .split("fn glyph_icon(")
            .next()
            .expect("action source before the glyph helper");
        assert!(
            action_source.contains(".hover(|d| d.bg(rgb(palette::current().panel_hi)))"),
            "the slide-out icons hover at panel_hi too"
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

    /// Card contract: line one is the title with timestamp and cost aligned
    /// right on the SAME line; the message context is its own line below
    /// (under the chip strip when topics are attached). No date fallback
    /// masquerading as a message. An approved plan may add one slim bar line
    /// after the message; nothing else grows the card.
    #[test]
    fn card_meta_rides_title_line_and_preview_is_own_line() {
        let source = include_str!("sessions.rs");
        let row_source = source
            .split("pub fn row(")
            .nth(1)
            .expect("session row renderer")
            .split("pub fn on_click(")
            .next()
            .expect("row source before click handler");
        let title_line = row_source
            .split(".when_some(chips,")
            .next()
            .expect("title line before the chip strip");
        assert!(
            title_line.contains("%m-%d %H:%M"),
            "the timestamp paints on the title line"
        );
        assert!(
            title_line.contains("s.total_cost"),
            "the cost paints on the title line"
        );
        assert!(
            title_line.contains(".ml_auto()"),
            "timestamp and cost align right"
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
        assert!(
            main_source.contains(
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
