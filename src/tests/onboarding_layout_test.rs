//! Full-screen onboarding layout (#1973): the wizard is a screen, not a
//! centered dialog, and the provider step's key and model fields stay on
//! screen on small terminals.

use crate::tui::onboarding::WizardMode;
use crate::tui::onboarding::{AuthField, OnboardingStep, OnboardingWizard, PROVIDERS};
use crate::tui::onboarding_layout::{
    BandPadding, MAX_CONTENT_WIDTH, NodeState, TimelineFit, band_padding, content_width,
    node_state, scroll_offset, timeline_fit, wrap_line, wrap_lines,
};
use crate::tui::onboarding_render::render_onboarding;
use crate::tui::render::theme::{self, Role};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

fn screen_text(wizard: &OnboardingWizard, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| render_onboarding(f, wizard)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..h {
        for x in 0..w {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

/// OpenAI on the provider step with the API key field focused and a long
/// model catalogue, the shape that pushed the key off a small screen.
fn provider_step_wizard() -> OnboardingWizard {
    let mut w = OnboardingWizard::new();
    w.step = OnboardingStep::ProviderAuth;
    w.ps.selected_provider = PROVIDERS
        .iter()
        .position(|p| p.name == "OpenAI")
        .expect("OpenAI provider");
    w.ps.api_key_input = "sk-test".to_string();
    w.ps.models.clear();
    w.ps.config_models = (0..40).map(|i| format!("gpt-model-{i}")).collect();
    w.auth_field = AuthField::ApiKey;
    w.error_message = None;
    w.resume_notice = None;
    w
}

#[test]
fn provider_step_key_and_model_fit_small_terminals() {
    let wizard = provider_step_wizard();
    for (w, h) in [(80, 24), (60, 20), (200, 60)] {
        let text = screen_text(&wizard, w, h);
        assert!(
            text.contains("API Key:"),
            "{w}x{h}: key field off screen\n{text}"
        );
        assert!(
            text.contains("Model:"),
            "{w}x{h}: model field off screen\n{text}"
        );
    }
}

#[test]
fn wizard_is_a_screen_not_a_boxed_dialog() {
    let wizard = provider_step_wizard();
    let text = screen_text(&wizard, 120, 40);
    // The old dialog drew a four-cornered box; the screen has none.
    for corner in ['┌', '┐', '└', '┘', '╭', '╮', '╰', '╯'] {
        assert!(!text.contains(corner), "found box corner {corner}\n{text}");
    }
    assert!(text.contains("OpenCrabs Setup"));
}

#[test]
fn error_stays_visible_in_the_footer() {
    let mut wizard = provider_step_wizard();
    wizard.error_message = Some("API key is required".to_string());
    let text = screen_text(&wizard, 60, 16);
    assert!(text.contains("API key is required"), "{text}");
}

#[test]
fn provider_selector_footer_keeps_search_and_count_visible_during_fetch() {
    let mut wizard = provider_step_wizard();
    wizard.auth_field = AuthField::Model;
    wizard.ps.models_fetching = true;

    let text = screen_text(&wizard, 120, 40);
    assert!(text.contains("type to filter"), "{text}");
    assert!(text.contains("40 models"), "{text}");
    assert!(text.contains("fetching live models"), "{text}");
}

#[test]
fn provider_selector_footer_shows_filtered_total_and_fetch_error() {
    let mut wizard = provider_step_wizard();
    wizard.quick_jump = true;
    wizard.ps.model_filter = "gpt-model-1".to_string();
    wizard.model_fetch_lifecycle.error = Some("provider timed out".to_string());

    let text = screen_text(&wizard, 120, 40);
    assert!(text.contains("11 of 40 models"), "{text}");
    assert!(text.contains("filter: gpt-model-1"), "{text}");
    assert!(
        text.contains("model fetch failed: provider timed out"),
        "{text}"
    );
    assert!(!text.contains("Step 3 of"), "{text}");
}

#[test]
fn first_run_step_one_footer_says_quit() {
    let mut wizard = OnboardingWizard::new();
    wizard.is_first_time = true;
    wizard.resume_notice = None;
    let text = screen_text(&wizard, 100, 30);
    assert!(text.contains("[Esc] Quit"), "{text}");
}

#[test]
fn content_width_caps_and_keeps_a_gutter() {
    assert_eq!(content_width(80), 76);
    assert_eq!(content_width(300), MAX_CONTENT_WIDTH);
    assert_eq!(content_width(10), 10);
}

#[test]
fn wrap_line_respects_width_and_keeps_text_and_style() {
    let red = Style::default().fg(Color::Red);
    let line = Line::from(vec![
        Span::styled("  Label: ", red),
        Span::raw("alpha beta gamma delta epsilon zeta eta theta"),
    ]);
    let rows = wrap_line(line, 16);
    assert!(rows.len() > 1);
    for r in &rows {
        let w: usize = r.spans.iter().map(|s| s.content.width()).sum();
        assert!(w <= 16, "row too wide: {w}");
    }
    assert_eq!(rows[0].spans[0].style, red);
    let joined: String = rows
        .iter()
        .flat_map(|r| r.spans.iter().map(|s| s.content.to_string()))
        .collect::<Vec<_>>()
        .join(" ");
    for word in ["alpha", "epsilon", "theta", "Label:"] {
        assert!(joined.contains(word), "lost {word}: {joined}");
    }
}

#[test]
fn wrap_lines_maps_each_line_to_its_first_row() {
    let lines = vec![
        Line::from("short"),
        Line::from("this one is long enough to wrap twice over"),
        Line::from("last"),
    ];
    let (rows, starts) = wrap_lines(lines, 12);
    assert_eq!(starts[0], 0);
    assert_eq!(starts[1], 1);
    assert!(starts[2] > 2);
    assert_eq!(rows.len(), starts[2] + 1);
}

#[test]
fn scroll_offset_follows_focus_and_clamps() {
    // Everything fits: never scroll.
    assert_eq!(scroll_offset(10, 12, 20, 0), 0);
    // Focus below the fold: scroll to it with two rows of context.
    assert_eq!(scroll_offset(30, 50, 20, 0), 28);
    // Never past the end.
    assert_eq!(scroll_offset(49, 50, 20, 0), 30);
    // Page Down adds on top, still clamped.
    assert_eq!(scroll_offset(0, 50, 20, 100), 30);
}

#[test]
fn mode_select_step_counts_match_the_progress_counter() {
    let mut wizard = OnboardingWizard::new();
    wizard.resume_notice = None;
    let text = screen_text(&wizard, 100, 30);
    let quick = format!("Sensible defaults, {} steps", OnboardingStep::quick_total());
    let full = format!("Full control, all {} steps", OnboardingStep::total());
    assert!(text.contains(&quick), "missing {quick:?}\n{text}");
    assert!(text.contains(&full), "missing {full:?}\n{text}");
}

#[test]
fn rerun_step_one_footer_says_exit() {
    // Esc on step 1 of an /onboard re-run closes the wizard back to chat,
    // so the hint must not promise a previous step.
    let mut wizard = OnboardingWizard::new();
    wizard.is_first_time = false;
    wizard.resume_notice = None;
    let text = screen_text(&wizard, 100, 30);
    assert!(text.contains("[Esc] Exit"), "{text}");
    assert!(!text.contains("[Esc] Back"), "{text}");
}

fn screen_rows(
    wizard: &OnboardingWizard,
    w: u16,
    h: u16,
) -> (Vec<String>, ratatui::buffer::Buffer) {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| render_onboarding(f, wizard)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let rows = (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect();
    (rows, buf)
}

#[test]
fn band_padding_shrinks_with_the_terminal() {
    assert_eq!(band_padding(40), BandPadding { outer: 1, inner: 1 });
    assert_eq!(band_padding(24), BandPadding { outer: 1, inner: 0 });
    assert_eq!(band_padding(16), BandPadding { outer: 0, inner: 0 });
}

#[test]
fn header_and_footer_keep_off_the_screen_edges() {
    // #1975: the title and the key hints sat on the first and last rows.
    let wizard = provider_step_wizard();
    for (w, h) in [(120, 40), (80, 24)] {
        let (rows, _) = screen_rows(&wizard, w, h);
        assert!(
            rows[0].trim().is_empty(),
            "{w}x{h}: row 0 not blank: {:?}",
            rows[0]
        );
        let last = &rows[h as usize - 1];
        assert!(
            last.trim().is_empty(),
            "{w}x{h}: last row not blank: {last:?}"
        );
        assert!(
            rows[1].contains("Brain Fuel"),
            "{w}x{h}: title not on row 1"
        );
    }
}

#[test]
fn header_and_footer_are_tinted_bands() {
    let wizard = provider_step_wizard();
    let (_, buf) = screen_rows(&wizard, 120, 40);
    let tint = theme::role(Role::SurfacePanel);
    // Padding rows carry the tint across the full width, not just under text.
    assert_eq!(buf[(0, 0)].bg, tint, "header band");
    assert_eq!(buf[(119, 0)].bg, tint, "header band right edge");
    assert_eq!(buf[(0, 39)].bg, tint, "footer band");
    // The content area stays untinted.
    assert_ne!(buf[(0, 20)].bg, tint, "content area");
}

// ── left-side step timeline (#1979) ─────────────────────────────────────

#[test]
fn flow_steps_agree_with_totals_and_numbering() {
    let quick = OnboardingStep::flow_steps(WizardMode::QuickStart);
    let full = OnboardingStep::flow_steps(WizardMode::Advanced);
    assert_eq!(quick.len(), OnboardingStep::quick_total());
    assert_eq!(full.len(), OnboardingStep::total());
    for (i, s) in quick.iter().enumerate() {
        assert_eq!(s.flow_number(WizardMode::QuickStart), i + 1, "{s:?}");
    }
    for (i, s) in full.iter().enumerate() {
        assert_eq!(s.flow_number(WizardMode::Advanced), i + 1, "{s:?}");
    }
}

#[test]
fn node_states_split_around_the_current_step() {
    assert_eq!(node_state(0, 2), NodeState::Done);
    assert_eq!(node_state(1, 2), NodeState::Current);
    assert_eq!(node_state(2, 2), NodeState::Upcoming);
    // Past the last step (Complete): everything is done.
    assert_eq!(node_state(5, 7), NodeState::Done);
}

#[test]
fn timeline_fit_drops_connectors_before_hiding() {
    // 6 steps: heading 3 (brand, counter, blank) + 6 nodes + 5 connectors.
    assert_eq!(timeline_fit(6, 14), TimelineFit::Spacious);
    assert_eq!(timeline_fit(6, 13), TimelineFit::Compact);
    assert_eq!(timeline_fit(6, 9), TimelineFit::Compact);
    assert_eq!(timeline_fit(6, 8), TimelineFit::Hidden);
}

fn mode_select_wizard() -> OnboardingWizard {
    let mut w = OnboardingWizard::new();
    w.step = OnboardingStep::ModeSelect;
    w.mode = WizardMode::QuickStart;
    w.quick_jump = false;
    w.resume_notice = None;
    w.error_message = None;
    w
}

#[test]
fn wide_screen_shows_the_timeline_instead_of_header_dots() {
    let wizard = mode_select_wizard();
    let text = screen_text(&wizard, 160, 45);
    assert!(text.contains("Step 1 of 6"), "{text}");
    for title in ["Home Base", "Brain Fuel", "Make It Yours"] {
        assert!(text.contains(title), "timeline misses {title}\n{text}");
    }
    assert!(
        !text.contains("1/6"),
        "header still carries the counter\n{text}"
    );
    assert!(text.contains("◉"), "no current-step node\n{text}");
}

#[test]
fn advanced_flow_timeline_lists_all_nine_steps() {
    let mut wizard = mode_select_wizard();
    wizard.mode = WizardMode::Advanced;
    let text = screen_text(&wizard, 160, 45);
    assert!(text.contains("Step 1 of 9"), "{text}");
    for title in ["Chat Me Anywhere", "Voice Superpowers", "Image Handling"] {
        assert!(text.contains(title), "timeline misses {title}\n{text}");
    }
}

#[test]
fn narrow_screen_keeps_the_header_dots() {
    let wizard = mode_select_wizard();
    let text = screen_text(&wizard, 80, 24);
    assert!(text.contains("1/6"), "{text}");
    assert!(!text.contains("Step 1 of"), "{text}");
}

#[test]
fn deep_link_gets_no_timeline() {
    let mut wizard = provider_step_wizard();
    wizard.quick_jump = true;
    let text = screen_text(&wizard, 160, 45);
    assert!(!text.contains("Step 3 of"), "{text}");
    assert!(text.contains("API Key:"), "{text}");
}

// ── header layout (#1980) ───────────────────────────────────────────────

#[test]
fn wide_header_is_the_step_and_the_timeline_carries_the_brand() {
    let wizard = mode_select_wizard();
    let (rows, _) = screen_rows(&wizard, 160, 45);
    let rule = rows
        .iter()
        .position(|r| r.trim_start().starts_with('─'))
        .expect("header rule");
    let header = rows[..rule].join("\n");
    assert!(header.contains("Pick Your Vibe"), "{header}");
    assert!(
        !header.contains("OpenCrabs Setup"),
        "brand still in header\n{header}"
    );
    let body = rows[rule..].join("\n");
    let brand = body.find("OpenCrabs Setup").expect("brand in timeline");
    let counter = body.find("Step 1 of 6").expect("step counter");
    assert!(brand < counter, "brand must sit above the counter\n{body}");
}

#[test]
fn narrow_header_is_one_title_line_with_dots_below() {
    let wizard = mode_select_wizard();
    let (rows, _) = screen_rows(&wizard, 80, 40);
    let title = rows
        .iter()
        .position(|r| r.contains("OpenCrabs Setup: Pick Your Vibe"))
        .expect("one-line title");
    let dots = rows.iter().position(|r| r.contains("1/6")).expect("dots");
    assert!(dots > title, "dots must follow the title line");
    assert!(!rows[title].contains("1/6"), "dots share the title line");
}

#[test]
fn tall_header_spaces_its_lines_short_header_packs_them() {
    let wizard = mode_select_wizard();
    let (tall, _) = screen_rows(&wizard, 160, 45);
    let t = tall
        .iter()
        .position(|r| r.contains("Pick Your Vibe"))
        .unwrap();
    assert!(
        tall[t + 1].trim().is_empty(),
        "no blank row under the title"
    );
    assert!(
        tall[t + 2].contains("your call"),
        "subtitle not after the gap"
    );

    let (short, _) = screen_rows(&wizard, 160, 26);
    let t = short
        .iter()
        .position(|r| r.contains("Pick Your Vibe"))
        .unwrap();
    assert!(
        short[t + 1].contains("your call"),
        "short header should pack"
    );
}
