//! Query normalization for Seeker.
//!
//! This is deliberately small and deterministic. It removes conversational
//! filler, folds Latin diacritics, and handles common plural forms without
//! importing the brain ranker's broader stemming rules.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// Words that usually carry no lookup intent in a natural question. Action
/// verbs are absent on purpose: `open` and `close` can select different rows.
const FILLER: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "can", "could", "do", "does", "for", "from",
    "get", "give", "got", "how", "i", "in", "is", "it", "let", "me", "my", "of", "on", "or",
    "please", "should", "some", "tell", "that", "the", "their", "there", "this", "to", "want",
    "was", "way", "what", "when", "where", "which", "will", "with", "would", "you", "your",
];

/// A parsed query. Each outer entry is one required intent group, and each
/// inner entry is an acceptable alternative, such as `open/close`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedQuery {
    original: String,
    groups: Vec<Vec<String>>,
}

impl NormalizedQuery {
    pub fn parse(input: &str) -> Self {
        let mut groups = token_groups(input);
        let has_meaningful = groups.iter().flatten().any(|token| !is_filler(token));
        if has_meaningful {
            groups = groups
                .into_iter()
                .filter_map(|group| {
                    let kept: Vec<String> = group
                        .into_iter()
                        .filter(|token| !is_filler(token))
                        .collect();
                    (!kept.is_empty()).then_some(kept)
                })
                .collect();
        }
        Self {
            original: input.to_string(),
            groups,
        }
    }

    /// The exact user text. Ask Crab uses this, never the normalized tokens.
    pub fn original(&self) -> &str {
        &self.original
    }

    pub fn groups(&self) -> &[Vec<String>] {
        &self.groups
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }
}

/// Normalize a field into independent words. Fields retain filler words so a
/// query made entirely of filler can still fall back to the original intent.
pub fn field_tokens(text: &str) -> Vec<String> {
    token_groups(text).into_iter().flatten().collect()
}

/// Lowercase, fold diacritics, and conservatively normalize a plural.
pub fn normalize_token(word: &str) -> String {
    let folded = fold_diacritics(&word.to_lowercase());
    stem_plural(&folded)
}

fn is_filler(token: &str) -> bool {
    FILLER.contains(&token)
}

/// Split words on punctuation and whitespace. An interior slash joins an OR
/// group, while the leading slash in `/theme` is only command punctuation.
fn token_groups(text: &str) -> Vec<Vec<String>> {
    let text = fold_diacritics(&text.to_lowercase());
    let mut groups = Vec::new();
    let mut alternatives = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch.is_alphanumeric() {
            current.push(ch);
            continue;
        }
        let alternative = ch == '/'
            && !current.is_empty()
            && chars.peek().is_some_and(|next| next.is_alphanumeric());
        push_word(&mut current, &mut alternatives);
        if !alternative {
            push_group(&mut alternatives, &mut groups);
        }
    }
    push_word(&mut current, &mut alternatives);
    push_group(&mut alternatives, &mut groups);
    groups
}

fn push_word(current: &mut String, alternatives: &mut Vec<String>) {
    if current.is_empty() {
        return;
    }
    let token = normalize_token(current);
    current.clear();
    if !alternatives.contains(&token) {
        alternatives.push(token);
    }
}

fn push_group(alternatives: &mut Vec<String>, groups: &mut Vec<Vec<String>>) {
    if !alternatives.is_empty() {
        groups.push(std::mem::take(alternatives));
    }
}

/// Only strip a plural when a useful stem remains. In particular, do not
/// strip broad `es` endings: the brain ranker turns `themes` into `them`.
fn stem_plural(word: &str) -> String {
    if word.len() > 5
        && let Some(stem) = word.strip_suffix("ies")
    {
        return format!("{stem}y");
    }
    if word.len() > 3
        && !word.ends_with("ss")
        && !word.ends_with("us")
        && !word.ends_with("is")
        && let Some(stem) = word.strip_suffix('s')
    {
        return stem.to_string();
    }
    word.to_string()
}

/// Fold marks only when their base is ASCII. This handles Latin input without
/// merging distinct letters in scripts whose marks carry separate meaning.
fn fold_diacritics(text: &str) -> String {
    let mut kept = Vec::with_capacity(text.len());
    let mut base_was_ascii = false;
    for ch in text.nfd() {
        if is_combining_mark(ch) {
            if !base_was_ascii {
                kept.push(ch);
            }
        } else {
            base_was_ascii = ch.is_ascii_alphanumeric();
            kept.push(ch);
        }
    }
    kept.into_iter().nfc().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups(input: &str) -> Vec<Vec<String>> {
        NormalizedQuery::parse(input).groups
    }

    #[test]
    fn natural_language_keeps_intent_and_original_text() {
        let query = NormalizedQuery::parse("How do I change theme?");
        assert_eq!(query.original(), "How do I change theme?");
        assert_eq!(
            query.groups(),
            &[vec![String::from("change")], vec![String::from("theme")]]
        );
    }

    #[test]
    fn interior_slashes_are_alternatives_not_extra_requirements() {
        assert_eq!(
            groups("open/close theme/appearance"),
            vec![
                vec![String::from("open"), String::from("close")],
                vec![String::from("theme"), String::from("appearance")],
            ]
        );
        assert_eq!(groups("/theme"), vec![vec![String::from("theme")]]);
        assert_eq!(
            groups("/mission-control"),
            vec![vec![String::from("mission")], vec![String::from("control")]]
        );
    }

    #[test]
    fn quotes_and_punctuation_do_not_become_query_terms() {
        assert_eq!(
            groups("\"change themes!\""),
            vec![vec![String::from("change")], vec![String::from("theme")]]
        );
        let punctuation = NormalizedQuery::parse("  ???  ");
        assert!(punctuation.is_empty());
        assert_eq!(punctuation.original(), "  ???  ");
    }

    #[test]
    fn latin_diacritics_fold_without_flattening_other_scripts() {
        assert_eq!(field_tokens("Configuração"), vec!["configuracao"]);
        assert_eq!(
            groups("configuracao"),
            vec![vec![String::from("configuracao")]]
        );
        assert_ne!(normalize_token("й"), normalize_token("и"));
    }

    #[test]
    fn common_plurals_normalize_on_queries_and_fields() {
        for (singular, plural) in [
            ("theme", "themes"),
            ("command", "commands"),
            ("sidebar", "sidebars"),
            ("appearance", "appearances"),
            ("query", "queries"),
            ("cafe", "cafés"),
        ] {
            assert_eq!(normalize_token(plural), singular);
            assert_eq!(field_tokens(plural), vec![singular]);
        }
        for unchanged in ["this", "status", "class"] {
            assert_eq!(normalize_token(unchanged), unchanged);
        }
    }

    #[test]
    fn filler_filtering_falls_back_when_it_would_erase_the_query() {
        let query = NormalizedQuery::parse("what can you do");
        assert_eq!(
            query.groups(),
            &[
                vec![String::from("what")],
                vec![String::from("can")],
                vec![String::from("you")],
                vec![String::from("do")],
            ]
        );
        assert_eq!(
            field_tokens("what can you do"),
            vec!["what", "can", "you", "do"]
        );
    }
}
