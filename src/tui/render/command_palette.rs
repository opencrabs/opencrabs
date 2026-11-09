//! The Seeker card in the terminal.
//!
//! Same engine as the window (`ui_core::palette`), so the rows, their order and
//! the no-match line are decided once for both surfaces (#2458). This file is
//! the terminal's half: a centered card, the window behind it dimmed for real
//! rather than hidden, and one row carrying the marker.
//!
//! Dimming is done on the [`Buffer`] because ratatui has no layer opacity: the
//! cells outside the card keep their content and gain [`Modifier::DIM`], so the
//! frame still reads as the app you were in, just out of focus.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::tui::render::chrome;
use crate::tui::render::theme::{self, Role};
use crate::ui_core::palette::{Channel, MAX_ROWS, PaletteState, channel_hint, pick_prompt, roast};

/// How many rows the card can print for a given outer area: the borders, the
/// field and the footer take the rest, and every hint line takes one more
/// (#2462, #2711).
fn page_height(area: Rect, hint_lines: usize) -> usize {
    let card = card_area(area);
    let chrome = 4 + u16::try_from(hint_lines).unwrap_or(0);
    usize::from(card.height.saturating_sub(chrome)).min(MAX_ROWS)
}

/// Cut the setup hint to the card's inner width, with an ellipsis. The strings
/// name every step the setup takes, so they are longer than a narrow terminal's
/// card, and ratatui would clip the tail without saying so (#2462).
fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The card: 70% of the width, the height the rows need, centered.
pub fn card_area(area: Rect) -> Rect {
    let width = (area.width * 7 / 10).clamp(30, 90);
    let height = (area.height * 6 / 10).clamp(8, 24);
    chrome::centered(area, width, height)
}

/// Dim everything outside `keep`, in place. The card paints after this, so its
/// own cells are never dimmed.
pub fn dim_region(buf: &mut Buffer, keep: Rect) {
    let area = buf.area;
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if keep.contains(Position::new(x, y)) {
                continue;
            }
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.modifier.insert(Modifier::DIM);
            }
        }
    }
}

/// Paint the card over the frame. The caller renders the app first, then this,
/// so the dim has something to dim.
pub(crate) fn render_command_palette(
    f: &mut Frame,
    state: &PaletteState,
    area: Rect,
    configured: &[Channel],
) {
    let card = card_area(area);
    dim_region(f.buffer_mut(), card);
    f.render_widget(Clear, card);

    let accent = theme::role(Role::Accent);
    let muted = theme::role(Role::GrayDim);
    let text = theme::role(Role::TextPrimary);

    // The setup line for a channel the query names but the config does not
    // carry: printed under the field, in the warning colour, because "you can
    // have this" is news and the control legend is not (#2462).
    let setup = channel_hint(state.query(), configured);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(" seeker ", Style::default().fg(accent)),
        Span::styled(state.query().to_string(), Style::default().fg(text)),
        // A caret so an empty field still looks like it is listening.
        Span::styled("▏", Style::default().fg(accent)),
    ]));
    // The setup line stays first because it is rarer and louder. Under it,
    // the armed pick names its job: without that sentence the card is an
    // ordinary list of conversations that never says what the pick serves
    // (#2462, #2711).
    let mut hint_lines = 0;
    if let Some(hint) = setup {
        lines.push(Line::from(Span::styled(
            fit(hint, usize::from(card.width.saturating_sub(2))),
            Style::default().fg(theme::role(Role::Warning)),
        )));
        hint_lines += 1;
    }
    if let Some(sentence) = state.pick().map(pick_prompt) {
        lines.push(Line::from(Span::styled(
            fit(sentence, usize::from(card.width.saturating_sub(2))),
            Style::default().fg(muted),
        )));
        hint_lines += 1;
    }
    if hint_lines == 0 {
        lines.push(Line::from(""));
    }

    let rows = state.visible();
    if rows.is_empty() && !state.is_empty() {
        lines.push(Line::from(Span::styled(
            roast(state.query()),
            Style::default().fg(theme::role(Role::Warning)),
        )));
    } else {
        let page = page_height(area, hint_lines).max(1);
        let armed = state.cursor().unwrap_or(0);
        let scroll = state
            .scroll()
            .min(armed.saturating_sub(page.saturating_sub(1)));
        for (ix, row) in rows.iter().enumerate().skip(scroll).take(page) {
            let marker = if ix == armed { "> " } else { "  " };
            let title_style = if ix == armed {
                Style::default()
                    .fg(theme::role(Role::TextPrimary))
                    .bg(theme::role(Role::SelectionBg))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(text)
            };
            let mut spans = vec![
                Span::styled(marker, Style::default().fg(accent)),
                Span::styled(row.item.title.clone(), title_style),
            ];
            if !row.item.subtitle.is_empty() {
                spans.push(Span::styled(
                    format!("  {}", row.item.subtitle),
                    Style::default().fg(muted),
                ));
            }
            spans.push(Span::styled(
                format!("  {}", row.item.kind.label()),
                Style::default().fg(muted),
            ));
            lines.push(Line::from(spans));
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::role(Role::BorderModal)))
        .title(Line::from(Span::styled(
            " seeker ",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        )))
        .title_bottom(Line::from(Span::styled(
            " enter run · ↑↓ move · esc close ",
            Style::default().fg(muted),
        )));

    f.render_widget(Paragraph::new(lines).block(block), card);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brain::agent::service::AgentService;
    use crate::db::Database;
    use crate::db::models::Session;
    use crate::db::repository::ContentSearchRow;
    use crate::services::ServiceContext;
    use crate::tests::agent_service_mocks::MockProvider;
    use crate::tui::app::state::App;
    use crate::tui::events::TuiEvent;
    use crate::tui::pane::PaneManager;
    use crate::ui_core::palette::{PaletteAction, PaletteItem, PaletteKind, Surface};
    use std::sync::Arc;

    fn item(title: &str) -> PaletteItem {
        PaletteItem::new(
            title,
            "",
            PaletteKind::Command,
            PaletteAction::OpenSurface(Surface::Help),
        )
    }

    /// The window behind the card is dimmed, and the card's own cells are not:
    /// dimming the whole frame would make the card unreadable, and dimming
    /// nothing would leave the card floating with no focus cue.
    #[test]
    fn the_region_outside_the_card_is_dimmed_and_the_card_is_not() {
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        let card = card_area(area);
        dim_region(&mut buf, card);

        let outside = buf.cell((0, 0)).expect("a cell outside the card");
        assert!(outside.modifier.contains(Modifier::DIM));
        let inside = buf
            .cell((card.x + 1, card.y + 1))
            .expect("a cell inside the card");
        assert!(!inside.modifier.contains(Modifier::DIM));
    }

    /// A tiny terminal still gets a card that fits, and a card that is never
    /// wider than the frame it sits in.
    #[test]
    fn the_card_fits_every_terminal() {
        for (w, h) in [(20u16, 8u16), (80, 24), (200, 60)] {
            let area = Rect::new(0, 0, w, h);
            let card = card_area(area);
            assert!(card.width <= area.width, "{w}x{h} card is too wide");
            assert!(card.height <= area.height, "{w}x{h} card is too tall");
            assert!(card.width > 0 && card.height > 0);
        }
    }

    /// An armed pick prints its own sentence under the field, so the terminal
    /// names the job the same way the window does (#2711).
    #[test]
    fn an_armed_pick_prints_its_sentence_on_the_card() {
        use crate::ui_core::palette::{PickFor, pick_prompt};
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        for purpose in [PickFor::NewTopic, PickFor::Favorite, PickFor::GroupTopics] {
            let state = PaletteState::picking(purpose, vec![item("invoices")]);
            let mut terminal = Terminal::new(TestBackend::new(90, 30)).unwrap();
            terminal
                .draw(|f| {
                    let area = f.area();
                    render_command_palette(f, &state, area, &[]);
                })
                .unwrap();
            let printed: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            let sentence = pick_prompt(purpose);
            // The card trims to its own width, so the test reads the head of
            // the sentence, which is the part that names the job.
            let head: String = sentence.chars().take(24).collect();
            assert!(
                printed.contains(&head),
                "{purpose:?} prints {head:?} on the card"
            );
        }
    }

    /// The page the card can print is bounded by the rows it has, so a long
    /// list scrolls instead of painting past the border.
    #[test]
    fn the_page_never_exceeds_the_card_or_the_row_cap() {
        let area = Rect::new(0, 0, 100, 40);
        assert!(page_height(area, 0) <= MAX_ROWS);
        assert!(page_height(area, 1) <= MAX_ROWS);
        assert!(page_height(area, 2) <= MAX_ROWS);
        // Every hint line costs the list one row, never a row it did not have.
        assert!(page_height(area, 1) <= page_height(area, 0));
        assert!(page_height(area, 2) <= page_height(area, 1));
        let state = PaletteState::new(
            (0..MAX_ROWS + 30)
                .map(|ix| item(&format!("r{ix}")))
                .collect(),
        );
        assert_eq!(state.visible().len(), MAX_ROWS);
    }

    // ── Seeker deep search (#2725) ───────────────────────────────────────

    async fn seeker_app() -> App {
        let db = Database::connect_in_memory().await.unwrap();
        db.run_migrations().await.unwrap();
        let context = ServiceContext::new(db.pool().clone());
        let provider: Arc<dyn crate::brain::provider::Provider> = Arc::new(MockProvider);
        let service = Arc::new(AgentService::new_for_test(provider, context.clone()).await);
        #[cfg(feature = "whatsapp")]
        let mut app = App::new(
            service,
            context,
            Arc::new(crate::channels::whatsapp::WhatsAppState::new()),
        );
        #[cfg(not(feature = "whatsapp"))]
        let mut app = App::new(service, context);
        app.pane_manager = PaneManager::new();
        app
    }

    fn seeker_session(title: &str) -> Session {
        let now = chrono::Utc::now();
        Session {
            id: uuid::Uuid::new_v4(),
            title: Some(title.to_string()),
            model: None,
            provider_name: None,
            created_at: now,
            updated_at: now,
            archived_at: None,
            token_count: 0,
            total_cost: 0.0,
            working_directory: None,
            auto_title_attempted: false,
            project_id: None,
            channel_chat_key: None,
        }
    }

    fn seeker_content_row(sid: uuid::Uuid, snippet: &str) -> ContentSearchRow {
        ContentSearchRow {
            session_id: sid,
            title: "Untitled".to_string(),
            snippet: snippet.to_string(),
            match_count: 1,
            last_hit_at: chrono::Utc::now(),
        }
    }

    /// Open the card and type the query, the same shape a real open plus
    /// typing takes. The catalog itself rebuilds through the scheduler's
    /// refresh, never by hand, so the tests walk the production seam.
    fn open_seeker(app: &mut App, query: &str) {
        app.command_palette = Some(PaletteState::new(Vec::new()));
        let state = app.command_palette.as_mut().unwrap();
        for c in query.chars() {
            state.type_char(c);
        }
    }

    /// A keyword that lives only in a message body surfaces the session in
    /// the Seeker, and the row carries the matching snippet as its subtitle.
    #[tokio::test]
    async fn seeker_content_only_hit_surfaces_with_snippet() {
        let mut app = seeker_app().await;
        let s = seeker_session("Roadmap");
        app.sessions = vec![s.clone()];
        app.seeker_find.insert(
            "rust".to_string(),
            vec![seeker_content_row(s.id, "we use rust here")],
        );
        open_seeker(&mut app, "rust");
        // A cached query rebuilds at once, no read needed.
        app.schedule_seeker_content_search();

        let rows = app.command_palette.as_ref().unwrap().visible();
        let row = rows
            .iter()
            .find(|r| r.item.action == PaletteAction::OpenSession(s.id.as_u128()))
            .expect("content-only hit must surface in the Seeker");
        assert_eq!(row.item.title, "Roadmap");
        assert!(
            row.item.subtitle.contains("rust"),
            "snippet missing: {}",
            row.item.subtitle
        );
    }

    /// A title match keeps its date subtitle and no injected keyword when
    /// the deep read found no message content: the old behavior is unchanged.
    #[tokio::test]
    async fn seeker_title_match_keeps_the_date_subtitle() {
        let mut app = seeker_app().await;
        let s = seeker_session("Rust roadmap");
        app.sessions = vec![s.clone()];
        // The read already ran and found nothing: an empty cache entry. The
        // cache-hit path rebuilds the real catalog, so the title match stands
        // on its own with no snippet and no injected keyword.
        app.seeker_find.insert("rust".to_string(), Vec::new());
        open_seeker(&mut app, "rust");
        app.schedule_seeker_content_search();

        let rows = app.command_palette.as_ref().unwrap().visible();
        let row = rows
            .iter()
            .find(|r| r.item.action == PaletteAction::OpenSession(s.id.as_u128()))
            .expect("title match must surface");
        assert_eq!(
            row.item.subtitle,
            s.updated_at.format("%Y-%m-%d %H:%M").to_string()
        );
        assert!(row.item.keywords.is_empty());
    }

    /// An older read that finishes last must not land in the cache; the
    /// current generation's read does.
    #[tokio::test]
    async fn seeker_stale_generation_result_is_ignored() {
        use std::sync::atomic::Ordering::SeqCst;
        let mut app = seeker_app().await;
        let s = seeker_session("Roadmap");
        app.sessions = vec![s.clone()];
        app.seeker_find_gen.store(5, SeqCst);

        app.handle_event(TuiEvent::SeekerContentSearchResult {
            generation: 3,
            query: "rust".to_string(),
            rows: vec![seeker_content_row(s.id, "stale rust hit")],
        })
        .await
        .unwrap();
        assert!(
            app.seeker_find.is_empty(),
            "stale generation landed in the cache"
        );

        app.handle_event(TuiEvent::SeekerContentSearchResult {
            generation: 5,
            query: "rust".to_string(),
            rows: vec![seeker_content_row(s.id, "fresh rust hit")],
        })
        .await
        .unwrap();
        assert_eq!(app.seeker_find["rust"].len(), 1);
        assert_eq!(app.seeker_find["rust"][0].snippet, "fresh rust hit");
    }

    /// The full rail: a persisted message body the title never mentions,
    /// typed into the open card, surfaces through the real debounced read.
    #[tokio::test]
    async fn seeker_end_to_end_reads_message_content() {
        use crate::db::models::Message;
        use crate::db::repository::{MessageRepository, SessionRepository};

        let mut app = seeker_app().await;
        let sessions = SessionRepository::new(app.session_service.pool());
        let messages = MessageRepository::new(app.session_service.pool());
        let s = seeker_session("Roadmap");
        sessions.create(&s).await.unwrap();
        let m = Message::new(s.id, "user".to_string(), "we use rust here".to_string(), 1);
        messages.create(&m).await.unwrap();
        app.sessions = vec![s.clone()];

        open_seeker(&mut app, "rust");
        app.schedule_seeker_content_search();
        // The read debounces 250ms then lands on the event rail; wait for it.
        let event = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            app.event_handler_mut().next(),
        )
        .await
        .expect("seeker search timed out")
        .expect("event channel closed");
        app.handle_event(event).await.unwrap();

        assert_eq!(app.seeker_find["rust"].len(), 1);
        let rows = app.command_palette.as_ref().unwrap().visible();
        let row = rows
            .iter()
            .find(|r| r.item.action == PaletteAction::OpenSession(s.id.as_u128()))
            .expect("content-only hit must surface after the read");
        assert!(
            row.item.subtitle.contains("rust"),
            "snippet missing: {}",
            row.item.subtitle
        );
    }
}
