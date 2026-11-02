//! The model picker's rows.
//!
//! The catalogue is the one the terminal's own picker reads
//! ([`load_default_models`], sourced from the embedded `config.toml.example`),
//! so the window cannot offer a model the terminal has never heard of, or miss
//! one the terminal ships. Nothing here touches the network: `/models` opens
//! instantly, and a switch is the only act that can go out.
//!
//! The row list is pure, which is what makes it testable without a window.

use opencrabs::tui::onboarding::PROVIDERS;
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
