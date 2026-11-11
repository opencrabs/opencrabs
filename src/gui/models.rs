//! The model picker's rows.
//!
//! The catalogue is the one the terminal's own picker reads
//! ([`load_default_models`], sourced from the embedded `config.toml.example`),
//! so the window cannot offer a model the terminal has never heard of, or miss
//! one the terminal ships. `/models` opens on the saved fallback immediately
//! and replaces each provider's section after its live fetch succeeds.
//!
//! The row list and panel model are pure, which makes the picker testable
//! without a window.

use opencrabs::tui::onboarding_render::visible_window;
use opencrabs::ui_core::model_selector::{
    CUSTOM_PROVIDER_IDX, ProviderSelectorState, single_model_choice, sort_newest_first,
};

/// One selectable pair.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Row {
    /// The provider id a switch names, e.g. `anthropic`.
    pub provider: String,
    /// The provider's display name, so a row can say who answers it.
    pub provider_name: String,
    /// The model id the provider answers to.
    pub model: String,
    /// Whether this is the pair the service is answering with right now.
    pub current: bool,
}

impl Row {
    /// The `provider/model` spelling a switch takes.
    pub fn pair(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }
}

/// One painted line in the model panel. Headers are not selectable; a model
/// keeps its index in the filtered list so keyboard and mouse select the same
/// pair.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PanelRow {
    Header {
        provider: String,
        name: String,
        count: usize,
    },
    Model {
        row: Row,
        index: usize,
        selected: bool,
    },
}

/// Everything the `/models` renderer paints. Keeping the search label and
/// group headers in this model makes their coverage a render test, rather
/// than a test of an unused formatting helper.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Panel {
    pub search: String,
    pub rows: Vec<PanelRow>,
    pub matched: usize,
    pub total: usize,
    pub above: usize,
    pub below: usize,
}

/// Case-insensitive live filtering, across the model id, provider id and
/// provider display name. Order stays exactly as the catalogue or live fetch
/// supplied it.
pub fn filtered_rows(rows: &[Row], query: &str) -> Vec<Row> {
    let needle = query.to_lowercase();
    if needle.is_empty() {
        return rows.to_vec();
    }
    rows.iter()
        .filter(|row| {
            row.pair().to_lowercase().contains(&needle)
                || row.provider_name.to_lowercase().contains(&needle)
        })
        .cloned()
        .collect()
}

/// Provider rows the models screen keeps in view. The picker is the screen's
/// main content, so this stays shorter than the wizard's 14-row window.
pub const PROVIDERS_VISIBLE: usize = 8;

/// One row of the models screen's provider list.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProviderRow {
    /// The provider id a click selects.
    pub provider: String,
    /// The display name, so the row never shows an opaque id alone.
    pub name: String,
    /// How many models this provider contributes to the list in view.
    pub count: usize,
    /// Index of the provider's first model in the slice this was built from.
    /// Passed the filtered rows, a click lands on the first visible model.
    pub first: usize,
}

/// Group rows into the provider list, in catalogue order. Rows arrive grouped
/// by provider, so one pass is enough, and `first` is resolved against the
/// slice it was given rather than the unfiltered catalogue.
pub fn providers(rows: &[Row]) -> Vec<ProviderRow> {
    let mut out: Vec<ProviderRow> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        match out.last_mut() {
            Some(last) if last.provider == row.provider => last.count += 1,
            _ => out.push(ProviderRow {
                provider: row.provider.clone(),
                name: row.provider_name.clone(),
                count: 1,
                first: index,
            }),
        }
    }
    out
}

/// The pair a provider row's click switches to without asking again.
///
/// A provider with one model in view has no model row worth picking, so the
/// click has to be the switch itself. Before this, the tile answered a hand
/// with a cursor move and the picker stayed open on a list with one row in it
/// (#2782). `rows` is the slice the provider list was built from, so the
/// count comes from the rows actually painted rather than a number carried
/// alongside them.
///
/// Returns `None` when there is nothing to switch to: more than one model in
/// view, or the single model is the pair already answering.
pub fn single_model_pair(rows: &[Row], row: &ProviderRow) -> Option<String> {
    // The rows actually painted for this provider, so the one-model rule is
    // the shared `single_model_choice` and never a second count kept in step
    // by hand (#2782).
    let offered: Vec<String> = rows
        .iter()
        .filter(|candidate| candidate.provider == row.provider)
        .map(Row::pair)
        .collect();
    let pair = single_model_choice(&offered)?.to_string();
    let model = rows.iter().find(|candidate| candidate.pair() == pair)?;
    (!model.current).then_some(pair)
}

/// Build the grouped, windowed panel the GUI renders. The window arithmetic is
/// the terminal picker's own, including its selection centring and end clamp.
pub fn panel(rows: &[Row], query: &str, selected: usize, max_models: usize) -> Panel {
    let filtered = filtered_rows(rows, query);
    let matched = filtered.len();
    let selected = selected.min(matched.saturating_sub(1));
    let (start, end) = visible_window(matched, selected, max_models);
    let mut painted = Vec::new();
    let mut previous_provider: Option<&str> = None;

    for (index, row) in filtered.iter().enumerate().take(end).skip(start) {
        if previous_provider != Some(row.provider.as_str()) {
            let count = filtered
                .iter()
                .filter(|candidate| candidate.provider == row.provider)
                .count();
            painted.push(PanelRow::Header {
                provider: row.provider.clone(),
                name: row.provider_name.clone(),
                count,
            });
            previous_provider = Some(row.provider.as_str());
        }
        painted.push(PanelRow::Model {
            row: row.clone(),
            index,
            selected: index == selected,
        });
    }

    Panel {
        search: if query.is_empty() {
            "/ type to filter…".to_string()
        } else {
            format!("/ {query}")
        },
        rows: painted,
        matched,
        total: rows.len(),
        above: start,
        below: matched.saturating_sub(end),
    }
}

/// Build the shared catalogue in provider order, substituting the wizard's
/// live provider state for its saved counterpart and marking the current pair.
/// Saved custom instances use their runtime name and models; the empty "new
/// custom" sentinel is skipped. Search text never removes entries here: it is
/// applied only by `filtered_rows`, so counts and discovery keep the full set.
pub fn rows_for_picker(
    current: Option<(&str, &str)>,
    edited: Option<&ProviderSelectorState>,
) -> Vec<Row> {
    let mut catalogue = ProviderSelectorState::default();
    catalogue.load_custom_names();
    let order = edited.unwrap_or(&catalogue).provider_display_order();
    let mut out = Vec::new();
    for index in order {
        let selector =
            if let Some(edited) = edited.filter(|selector| selector.selected_provider == index) {
                edited.clone()
            } else {
                if index == CUSTOM_PROVIDER_IDX {
                    continue;
                }
                let mut saved = catalogue.clone();
                saved.selected_provider = index;
                saved.detect_existing_key();
                saved.reload_config_models();
                saved
            };
        if index == CUSTOM_PROVIDER_IDX && selector.custom_name.trim().is_empty() {
            continue;
        }
        out.extend(provider_rows(&selector, current));
    }
    out
}

/// Build one provider's rows from an immutable selector snapshot. Keeping this
/// separate makes edited-provider behavior testable without reading a live HOME.
fn provider_rows(selector: &ProviderSelectorState, current: Option<(&str, &str)>) -> Vec<Row> {
    let (provider, provider_name) = if selector.selected_provider >= CUSTOM_PROVIDER_IDX {
        (selector.custom_name.clone(), selector.custom_name.clone())
    } else {
        (
            selector.current_provider().id.to_string(),
            selector.current_provider().name.to_string(),
        )
    };
    selector
        .all_model_names()
        .into_iter()
        .map(|model| Row {
            current: current
                .is_some_and(|(active, active_model)| active == provider && active_model == model),
            model: model.to_string(),
            provider: provider.clone(),
            provider_name: provider_name.clone(),
        })
        .collect()
}

/// Resolve a typed id which is absent from every fetched catalogue. A fully
/// qualified pair keeps its provider; a bare id uses the active provider.
pub fn manual_pair(query: &str, active_provider: &str) -> Option<String> {
    let query = query.trim();
    if query.is_empty() || query.chars().any(char::is_whitespace) {
        return None;
    }
    if query.starts_with('/') || query.ends_with('/') {
        return None;
    }
    if query.contains('/') {
        Some(query.to_string())
    } else if active_provider.is_empty() {
        None
    } else {
        Some(format!("{active_provider}/{query}"))
    }
}

/// Mark a picked pair current, inserting an arbitrary id beside its provider
/// when no catalogue row exists yet. Returns its visible unfiltered index.
pub fn ensure_pair(rows: &mut Vec<Row>, provider: &str, model: &str) -> usize {
    for row in rows.iter_mut() {
        row.current = row.provider == provider && row.model == model;
    }
    if let Some(index) = rows
        .iter()
        .position(|row| row.provider == provider && row.model == model)
    {
        return index;
    }
    let provider_name = rows
        .iter()
        .find(|row| row.provider == provider)
        .map(|row| row.provider_name.clone())
        .unwrap_or_else(|| provider.to_string());
    let row = Row {
        provider: provider.to_string(),
        provider_name,
        model: model.to_string(),
        current: true,
    };
    let index = rows
        .iter()
        .rposition(|candidate| candidate.provider == provider)
        .map_or(rows.len(), |index| index + 1);
    rows.insert(index, row);
    index
}

/// Replace one provider's fallback section with a successful live fetch. Other
/// providers keep their rows and order until their own responses arrive; the
/// selected highlight is resolved again because indices may move.
pub fn replace_provider(
    rows: &mut Vec<Row>,
    provider_id: &str,
    provider_name: &str,
    fetched: &[String],
    current: Option<(&str, &str)>,
) {
    if provider_id.is_empty() {
        return;
    }
    let insert_at = rows
        .iter()
        .position(|row| row.provider == provider_id)
        .unwrap_or(rows.len());
    rows.retain(|row| row.provider != provider_id);
    let mut models: Vec<&str> = fetched.iter().map(String::as_str).collect();
    sort_newest_first(&mut models);
    let live = models.iter().map(|model| Row {
        provider: provider_id.to_string(),
        provider_name: provider_name.to_string(),
        current: current
            .is_some_and(|(provider, active)| provider == provider_id && active == *model),
        model: (*model).to_string(),
    });
    rows.splice(insert_at..insert_at, live);
}

/// The row an argument names: the full pair first, then a bare model id.
///
/// Returns `None` when nothing matches, which the caller turns into the same
/// notice the terminal prints rather than a silent no-op.
pub fn find<'a>(rows: &'a [Row], arg: &str) -> Option<&'a Row> {
    let arg = arg.trim();
    if arg.is_empty() {
        return None;
    }
    rows.iter()
        .find(|row| row.pair().eq_ignore_ascii_case(arg))
        .or_else(|| rows.iter().find(|row| row.model.eq_ignore_ascii_case(arg)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(provider: &str, name: &str, model: &str) -> Row {
        Row {
            provider: provider.into(),
            provider_name: name.into(),
            model: model.into(),
            current: false,
        }
    }

    #[test]
    fn models_panel_render_includes_group_headers_and_search_input() {
        let rows = vec![
            row("one", "Provider One", "alpha"),
            row("one", "Provider One", "beta"),
            row("two", "Provider Two", "gamma"),
        ];
        let rendered = panel(&rows, "", 0, 20);
        assert_eq!(rendered.search, "/ type to filter…");
        assert_eq!(rendered.matched, 3);
        assert!(matches!(
            &rendered.rows[0],
            PanelRow::Header { provider, name, count }
                if provider == "one" && name == "Provider One" && *count == 2
        ));
        assert!(rendered.rows.iter().any(|line| matches!(
            line,
            PanelRow::Header { provider, name, count }
                if provider == "two" && name == "Provider Two" && *count == 1
        )));

        let searched = panel(&rows, "alp", 0, 20);
        assert_eq!(searched.search, "/ alp");
        assert_eq!(searched.matched, 1);
        assert!(matches!(
            &searched.rows[0],
            PanelRow::Header { provider, .. } if provider == "one"
        ));
    }

    #[test]
    fn models_panel_search_matches_provider_names_and_model_ids() {
        let rows = vec![
            row("one", "First Provider", "alpha"),
            row("two", "Second Provider", "beta"),
        ];
        assert_eq!(filtered_rows(&rows, "SECOND").len(), 1);
        assert_eq!(filtered_rows(&rows, "two/bet")[0].model, "beta");
        assert!(filtered_rows(&rows, "missing").is_empty());
        let empty = panel(&rows, "missing", 0, 20);
        assert!(empty.rows.is_empty());
        assert_eq!(empty.matched, 0);
        assert_eq!(empty.total, 2);
    }

    #[test]
    fn models_panel_scroll_indicators_follow_the_tui_window() {
        let rows: Vec<_> = (0..8)
            .map(|index| row("one", "Provider One", &format!("model-{index}")))
            .collect();
        let rendered = panel(&rows, "", 6, 3);
        assert_eq!((rendered.above, rendered.below), (5, 0));
        let models: Vec<_> = rendered
            .rows
            .iter()
            .filter_map(|line| match line {
                PanelRow::Model {
                    row,
                    index,
                    selected,
                } => Some((row.model.as_str(), *index, *selected)),
                PanelRow::Header { .. } => None,
            })
            .collect();
        assert_eq!(
            models,
            vec![
                ("model-5", 5, false),
                ("model-6", 6, true),
                ("model-7", 7, false)
            ]
        );
        assert!(matches!(
            &rendered.rows[0],
            PanelRow::Header { count: 8, .. }
        ));
        let first = panel(&rows, "", 0, 3);
        assert_eq!((first.above, first.below), (0, 5));
    }

    #[test]
    fn provider_list_groups_rows_and_points_at_each_first_model() {
        let rows = vec![
            row("one", "Provider One", "alpha"),
            row("one", "Provider One", "beta"),
            row("two", "Provider Two", "gamma"),
        ];
        assert_eq!(
            providers(&rows),
            vec![
                ProviderRow {
                    provider: "one".into(),
                    name: "Provider One".into(),
                    count: 2,
                    first: 0,
                },
                ProviderRow {
                    provider: "two".into(),
                    name: "Provider Two".into(),
                    count: 1,
                    first: 2,
                },
            ]
        );
        // A filter rebuilds the counts and the jump targets from the rows that
        // are actually painted, so a click can never land on a hidden row.
        let filtered = filtered_rows(&rows, "gam");
        let narrowed = providers(&filtered);
        assert_eq!(narrowed.len(), 1);
        assert_eq!((narrowed[0].count, narrowed[0].first), (1, 0));
        assert!(providers(&[]).is_empty());
    }

    #[test]
    fn a_one_model_provider_click_switches_and_a_bigger_one_does_not() {
        let mut rows = vec![
            row("solo", "Solo Provider", "only-model"),
            row("many", "Many Provider", "alpha"),
            row("many", "Many Provider", "beta"),
        ];
        let grouped = providers(&rows);
        // One model in view: the click is the switch, and it names the pair the
        // tile counts instead of leaving the picker open on a single row.
        assert_eq!(
            single_model_pair(&rows, &grouped[0]).as_deref(),
            Some("solo/only-model")
        );
        // A real choice is still a choice.
        assert_eq!(single_model_pair(&rows, &grouped[1]), None);

        // A filter rebuilds the counts from the rows that are painted, so a
        // narrowed provider switches to the model its tile says it has.
        let narrowed = filtered_rows(&rows, "beta");
        let narrowed_rows = providers(&narrowed);
        assert_eq!(
            single_model_pair(&narrowed, &narrowed_rows[0]).as_deref(),
            Some("many/beta")
        );

        // The pair already answering is not a switch: the click only moves the
        // cursor, so a live tile never closes the picker onto itself.
        rows[0].current = true;
        let grouped = providers(&rows);
        assert_eq!(single_model_pair(&rows, &grouped[0]), None);

        // An index with no painted row behind it is not a switch either.
        let empty: Vec<Row> = Vec::new();
        assert_eq!(
            single_model_pair(
                &empty,
                &ProviderRow {
                    provider: "ghost".into(),
                    name: "Ghost".into(),
                    count: 1,
                    first: 0,
                }
            ),
            None
        );
    }

    #[test]
    fn unmatched_model_ids_can_be_selected_without_a_catalogue_row() {
        assert_eq!(
            manual_pair("new-model-2027", "openai").as_deref(),
            Some("openai/new-model-2027")
        );
        assert_eq!(
            manual_pair("custom/tenant/model", "openai").as_deref(),
            Some("custom/tenant/model")
        );
        assert_eq!(manual_pair("   ", "openai"), None);
        assert_eq!(manual_pair("has spaces", "openai"), None);
        assert_eq!(manual_pair("new-model", ""), None);
    }

    #[test]
    fn the_catalogue_is_not_empty() {
        // A picker with no rows is the failure this test exists for.
        let rows = rows_for_picker(None, None);
        assert!(!rows.is_empty(), "the shared catalogue produced no rows");
        assert!(
            rows.len() > 20,
            "suspiciously small catalogue: {}",
            rows.len()
        );
    }

    #[test]
    fn the_live_pair_is_marked_once() {
        let all = rows_for_picker(None, None);
        let first = all.first().expect("a row");
        let live = rows_for_picker(Some((&first.provider, &first.model)), None);
        assert_eq!(live.iter().filter(|row| row.current).count(), 1);
        assert!(live[0].current);
    }

    #[test]
    fn fetch_models_live_rows_replace_only_their_provider() {
        let first = Row {
            provider: "one".into(),
            provider_name: "One".into(),
            model: "static".into(),
            current: false,
        };
        let other = Row {
            provider: "two".into(),
            provider_name: "Two".into(),
            model: "untouched".into(),
            current: false,
        };
        let mut rows = vec![first, other];
        replace_provider(
            &mut rows,
            "one",
            "One",
            &["live-new".into(), "live-old".into()],
            Some(("one", "live-new")),
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].model, "live-old");
        assert!(!rows[0].current);
        assert_eq!(rows[1].model, "live-new");
        assert!(rows[1].current);
        assert_eq!(rows[2].pair(), "two/untouched");
    }

    #[test]
    fn live_rows_use_shared_numeric_version_order() {
        let mut rows = Vec::new();
        replace_provider(
            &mut rows,
            "test",
            "Test",
            &["glm-5.2".into(), "glm-5.10".into(), "glm-5-turbo".into()],
            None,
        );
        let models: Vec<&str> = rows.iter().map(|row| row.model.as_str()).collect();
        assert_eq!(models, ["glm-5.10", "glm-5.2", "glm-5-turbo"]);
    }

    #[test]
    fn edited_provider_search_does_not_shrink_its_catalogue() {
        let edited = ProviderSelectorState {
            config_models: vec!["visible-model".into(), "other-model".into()],
            model_filter: "visible".into(),
            ..Default::default()
        };
        let provider = edited.provider_id().to_string();
        let rows = provider_rows(&edited, None);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.provider == provider));
        assert!(rows.iter().any(|row| row.model == "other-model"));
        let filtered = panel(&rows, &edited.model_filter, 0, 20);
        assert_eq!(filtered.matched, 1);
        assert_eq!(filtered.total, 2);
    }

    #[test]
    fn arbitrary_pair_is_inserted_once_and_marked_current() {
        let edited = ProviderSelectorState {
            config_models: vec!["known-model".into()],
            ..Default::default()
        };
        let provider = edited.provider_id().to_string();
        let mut rows = provider_rows(&edited, None);
        let original = rows.len();
        ensure_pair(&mut rows, &provider, "future-model");
        ensure_pair(&mut rows, &provider, "future-model");
        assert_eq!(rows.len(), original + 1);
        let matches: Vec<_> = rows
            .iter()
            .filter(|row| row.provider == provider && row.model == "future-model")
            .collect();
        assert_eq!(matches.len(), 1);
        assert!(matches[0].current);
        assert_eq!(rows.iter().filter(|row| row.current).count(), 1);
    }

    #[test]
    fn a_pair_round_trips_through_find() {
        let all = rows_for_picker(None, None);
        // The catalogue decides, not a fixed id: several providers ship no
        // static model list because the terminal fetches theirs live, so the
        // first row stands in for "a pair that is really on the screen".
        let target = all.first().expect("a row");
        let pair = target.pair();
        assert_eq!(find(&all, &pair).map(Row::pair), Some(pair.clone()));
        // Case is not a typing problem.
        assert_eq!(
            find(&all, &pair.to_uppercase()).map(Row::pair),
            Some(pair.clone())
        );
        // A bare model id resolves too, which is how `/models opus` works.
        assert!(find(&all, &target.model).is_some());
        assert!(find(&all, "no-such-model").is_none());
        assert!(find(&all, "   ").is_none());
    }
}
