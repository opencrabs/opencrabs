//! The model picker's rows.
//!
//! The catalogue is the one the terminal's own picker reads
//! ([`load_default_models`], sourced from the embedded `config.toml.example`),
//! so the window cannot offer a model the terminal has never heard of, or miss
//! one the terminal ships. `/models` opens on that catalogue immediately and
//! replaces the current provider's section after its live fetch succeeds.
//!
//! The row list and panel model are pure, which makes the picker testable
//! without a window.

use opencrabs::tui::onboarding::PROVIDERS;
use opencrabs::tui::onboarding_render::visible_window;
use opencrabs::tui::provider_selector::load_default_models;

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

/// Every model in the shared catalogue, in provider order, with the live pair
/// marked.
///
/// The dynamic "custom" entry carries an empty id and has no catalogue, so it is
/// skipped: a switch has nothing to name there until the user configures it.
pub fn rows(current: Option<(&str, &str)>) -> Vec<Row> {
    let mut out = Vec::new();
    for info in PROVIDERS.iter().filter(|info| !info.id.is_empty()) {
        for model in load_default_models(info.id) {
            out.push(Row {
                provider: info.id.to_string(),
                provider_name: info.name.to_string(),
                current: current.is_some_and(|(p, m)| p == info.id && m == model),
                model,
            });
        }
    }
    out
}

/// Replace one provider's static section with a successful live fetch. Other
/// providers keep their rows and order; the selected highlight is resolved
/// again by the caller because indices may move.
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
    let live = fetched.iter().map(|model| Row {
        provider: provider_id.to_string(),
        provider_name: provider_name.to_string(),
        current: current
            .is_some_and(|(provider, active)| provider == provider_id && active == model),
        model: model.clone(),
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
    fn the_catalogue_is_not_empty() {
        // A picker with no rows is the failure this test exists for.
        let rows = rows(None);
        assert!(!rows.is_empty(), "the shared catalogue produced no rows");
        assert!(
            rows.len() > 20,
            "suspiciously small catalogue: {}",
            rows.len()
        );
    }

    #[test]
    fn the_live_pair_is_marked_once() {
        let all = rows(None);
        let first = all.first().expect("a row");
        let live = rows(Some((&first.provider, &first.model)));
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
        assert_eq!(rows[0].model, "live-new");
        assert!(rows[0].current);
        assert_eq!(rows[1].model, "live-old");
        assert_eq!(rows[2].pair(), "two/untouched");
    }

    #[test]
    fn a_pair_round_trips_through_find() {
        let all = rows(None);
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
