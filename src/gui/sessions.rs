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

use gpui::{
    App, ClickEvent, Div, InteractiveElement, SharedString, Stateful, Styled, Window, div,
    prelude::*, rems, rgb,
};

use opencrabs::db::Database;
use opencrabs::db::models::Session;
use opencrabs::db::repository::session::{SessionListOptions, SessionRepository};

use crate::theme::{
    BG, FOCUS, PANEL, PANEL_HI, SELECTION, TEXT, TEXT_FAINT, TEXT_HINT, TEXT_MUTED, TEXT_TITLE,
};

/// The rail's live state, owned by the window's view.
#[derive(Default)]
pub struct SessionsState {
    /// Most recent first, straight from `SessionRepository::list`.
    pub items: Vec<Session>,
    /// The session a click chose; the transcript (task 3) reads it.
    pub selected: Option<uuid::Uuid>,
    /// Set when the fetch failed; the rail says why instead of lying empty.
    pub error: Option<SharedString>,
    /// Cmd+F's query. A non-empty query narrows the rail to the chats whose
    /// title (or id prefix) matches; the ledger is untouched, so clearing the
    /// query shows everything back exactly as it was.
    pub filter: String,
    /// Whether the field is taking keystrokes. The bar is only drawn while it
    /// is, so an unsearched rail looks the way it always did.
    pub filter_active: bool,
}

impl SessionsState {
    /// The rows the rail shows right now, in ledger order.
    pub fn visible(&self) -> impl Iterator<Item = &Session> {
        self.items.iter().filter(|s| matches(s, &self.filter))
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

/// Cmd+F's field, drawn under the rail's header while it holds the keyboard.
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
        .bg(rgb(PANEL_HI))
        .border_1()
        .border_color(rgb(FOCUS))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(rgb(if empty { TEXT_FAINT } else { TEXT }))
                .child(SharedString::from(body)),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(TEXT_HINT))
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

/// Char-safe truncation: walk chars, never slice bytes (#2271's whole class).
fn fit(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// One session row: title and cost on the left, the caller's action on the
/// right. Pure data plus one slot for behaviour, so the view owns every
/// listener it puts on screen.
pub fn row(s: &Session, selected: bool, action: Option<Stateful<Div>>) -> Stateful<Div> {
    let body = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .text_sm()
                .text_color(rgb(TEXT))
                .child(SharedString::from(fit(
                    s.title.as_deref().unwrap_or("(untitled)"),
                    42,
                ))),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(TEXT_MUTED))
                .child(SharedString::from(format!(
                    "{} · ${:.2}",
                    s.updated_at
                        .with_timezone(&chrono::Local)
                        .format("%m-%d %H:%M"),
                    s.total_cost
                ))),
        );
    let mut row = div()
        .id(SharedString::from(s.id.to_string()))
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_1()
        .p_2()
        .rounded_sm()
        .cursor_pointer()
        .when(selected, |d| d.bg(rgb(SELECTION)))
        .hover(|d| d.bg(rgb(BG)))
        .child(body);
    if let Some(action) = action {
        row = row.child(action);
    }
    row
}

/// The click behaviour for a row, typed the way `cx.listener` produces it.
pub fn on_click(
    row: Stateful<Div>,
    handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    row.on_click(handler)
}

/// The per-row delete affordance. A small ✕ that the view wires to
/// `SessionService::delete_session` (soft delete: the row is archived, so
/// usage history survives). Dim until the row is hovered in the caller's
/// layout, so a rail of sessions does not read as a rail of delete buttons.
pub fn delete_marker() -> Stateful<Div> {
    div()
        .id("session-delete")
        .text_xs()
        .text_color(rgb(TEXT_FAINT))
        .cursor_pointer()
        .child(SharedString::from("\u{2715}"))
}

/// The rail column: header and error slot from the state, then the caller's
/// prebuilt rows (each already carrying its click handler). Stateful, because
/// the scroll container needs an element id.
pub fn rail(state: &SessionsState, rows: Vec<Stateful<Div>>, width_rem: f32) -> Stateful<Div> {
    let mut col = div()
        .id("session-rail")
        .flex()
        .flex_col()
        .overflow_y_scroll()
        .flex_none()
        .w(rems(width_rem))
        .h_full()
        .bg(rgb(PANEL))
        .p_2()
        .gap_1();

    col = col.child(
        div()
            .text_xs()
            .text_color(rgb(TEXT_TITLE))
            .child(SharedString::from(format!(
                "SESSIONS · {}",
                state.items.len()
            ))),
    );

    if state.filter_active {
        col = col.child(filter_bar(state));
    }

    if let Some(err) = &state.error {
        col = col.child(
            div()
                .text_xs()
                .text_color(rgb(TEXT_MUTED))
                .child(err.clone()),
        );
        return col;
    }

    if rows.is_empty() && !state.filter.trim().is_empty() {
        col = col.child(
            div()
                .text_xs()
                .text_color(rgb(TEXT_MUTED))
                .child("no chat matches"),
        );
        return col;
    }

    for row in rows {
        col = col.child(row);
    }
    col
}

#[cfg(test)]
mod tests {
    use super::{Database, fit, load_sessions};

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
}
