//! [`LangConfig`], the embedded language tables, and the language
//! registry the rest of the crate resolves its packs from, parsed once on
//! first use. A TOML syntax error fails the build via `include_str!`.
//!
//! [`Lang`] is the one definition of which languages ship and in what
//! detection-priority order. Every per-language table family resolves from
//! it through an exhaustive match, so a language added to [`Lang::ALL`] does
//! not compile until both packs exist. `utils::prompt_analyzer` used to carry
//! its own copy of the list with its own detection function, kept in step by
//! hand (#2226).
//!
//! Test builds carry a traversal counter ([`scan_counters`]) so the phantom
//! detectors can assert their work shape: the scan cost must scale with the
//! number of packs, never with the length of the narration. A wall-clock
//! assertion cannot do that job here, because the CI runner is the same Mac
//! that hosts interactive sessions and any millisecond budget flakes under
//! that contention. Counting pack traversals is deterministic and catches the
//! change that actually matters, an input-proportional scan.

use serde::Deserialize;
use std::sync::LazyLock;

/// Language-specific phantom detection configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct LangConfig {
    #[serde(default)]
    pub intent_phrases: Vec<String>,
    #[serde(default)]
    pub action_verbs: Vec<String>,
    #[serde(default)]
    pub line_start_re: String,
    /// Matches a brief present-continuous work announcement that ends the
    /// turn with no tool call — e.g. "Running checks now.", "Checking the
    /// logs…". These fall under the no-tools detector's length floor and
    /// aren't "I'll / let me / I'm going to" phrases, so they need their own
    /// pattern. Anchored and ending in an imminence marker (now / … / :) to
    /// avoid matching ordinary sentences that merely open with a gerund.
    #[serde(default)]
    pub work_announcement_re: String,
    #[serde(default)]
    pub completion_claims: Vec<String>,
    /// Phrases presenting a named command as ALREADY RUN, e.g. "ran",
    /// "checked with", "output above". Required by the uncalled-command check
    /// (#789): the command itself is language-neutral, but the framing that
    /// turns a proposal into a claim is not, and an English-only list let a
    /// fabrication in any other language through untouched.
    #[serde(default)]
    pub executed_framings: Vec<String>,
    /// Generic first-person intent, e.g. `let me <verb>` / `I'll <verb>`,
    /// with the verb captured in group 1 rather than enumerated. The phrase
    /// list is an allowlist of specific verb pairings, so every verb nobody
    /// thought to add slips through: `let me execute the full flow now` ended
    /// a turn with no tool call because `let me execute` was not among the
    /// 701 entries. Verbs that read as speech rather than action
    /// (`let me know`) are filtered by `intent_verb_exclusions`.
    #[serde(default)]
    pub generic_intent_re: String,
    /// Verbs that follow the construction above without promising work:
    /// `let me know` addresses the user, `I'll be happy to` is not an action.
    #[serde(default)]
    pub intent_verb_exclusions: Vec<String>,
    /// Gerund-led plan announcement with NO imminence marker, sequenced
    /// with then / before — "Setting up the plan, then mapping every call
    /// site before touching anything." `work_announcement_re` enumerates
    /// execution verbs and demands a trailing now / … / :, `gerund_re`
    /// demands a leading "Now"; a turn that announces the PREPARATION for
    /// work satisfied neither and was delivered as if it were an answer
    /// (#1261). Consumed by the zero-tool gate only.
    #[serde(default)]
    pub plan_announcement_re: String,
    /// Bare-participle announcement carrying its OWN object pronoun, with no
    /// imminence marker and no sequencing: "Reading them, with mtimes so I
    /// know each postdates the tree it claims to cover." Every existing arm
    /// needs something this clause does not have — `work_announcement_re` and
    /// `gerund_re` a now / … / :, `plan_announcement_re` a then / before /
    /// after — so the marker-less form shipped as a finished answer while
    /// announcing unfinished work (#1694). Group 1 MUST capture the rest of
    /// the clause; the announcement/statement split reads it.
    ///
    /// The construction is language-specific in more than vocabulary: the
    /// clitic sits after and separate (en, ru), enclitic behind a hyphen (pt),
    /// fused with an accent shift (es), PROCLITIC before the verb (fr), or as
    /// a fused suffix (id). Empty for a language whose split cannot be
    /// discriminated safely — every consumer guards `is_empty()` first, so an
    /// absent arm is inert, not broken.
    #[serde(default)]
    pub participle_object_re: String,
    /// Closed-class words that may follow the pronoun in an ANNOUNCEMENT
    /// (prepositions, conjunctions, numerals, ordinals, modals, fixed
    /// adverbials): "Reading them **against** the log", "Siguiéndolos **paso**
    /// a paso". A tail beginning with anything else is a STATEMENT whose
    /// participle is the subject — "Getting them **took** a while",
    /// "Leyéndolos **reveló** el fallo" — and must not fire.
    ///
    /// This is deliberately a closed class. The split used to be specified as
    /// a copula list, which is both open in the general case and insufficient:
    /// the statements above carry no copula at all, and French "En les
    /// lisant, on comprend vite" likewise has none, so no copula list can
    /// separate them. Verb predicates are an open class and enumerating them
    /// never converges (#1122); the word class AFTER the pronoun does.
    #[serde(default)]
    pub announcement_tail_words: Vec<String>,
    /// Verbs and prepositions that make a following BARE tool name the
    /// object of an action the model attributes to itself: "setting up the
    /// plan", "running bash". A multi-word name proves tool usage on its
    /// own; `plan` / `bash` / `grep` / `ls` are ordinary words and prove it
    /// only in this frame (#1262).
    #[serde(default)]
    pub tool_use_markers: Vec<String>,
    /// Nouns that name the preceding word as a tool ("the plan tool").
    #[serde(default)]
    pub tool_nouns: Vec<String>,
    /// Determiners allowed between a marker and the name. At most one:
    /// anything else in between means the name is not the marker's object,
    /// which is what keeps "run it in bash" (an instruction to the user)
    /// out. Empty for languages without articles.
    #[serde(default)]
    pub tool_name_determiners: Vec<String>,
    #[serde(default)]
    pub gerund_re: String,
    #[serde(default)]
    pub trailing_colon_re: String,
    #[serde(default)]
    pub now_imperative_re: String,
    #[serde(default)]
    pub numbered_steps_re: String,
    #[serde(default)]
    pub past_tense_standalone_re: String,
    #[serde(default)]
    pub path_re: String,
    #[serde(default)]
    pub ext_re: String,
    #[serde(default)]
    pub backtick_code_re: String,
    /// Assertions that a visual/media result was produced or delivered
    /// ("there it is", "generated it", "the edited image", ...). Paired with
    /// `media_context_words` to catch image-generation hallucination in any
    /// language (#747) — a claim of a delivered image with no `<<IMG:>>` marker.
    #[serde(default)]
    pub media_delivery_phrases: Vec<String>,
    /// Words marking a claimed result as visual/image work ("image", "photo",
    /// "background", "brightness", ...). Paired with `media_delivery_phrases`.
    #[serde(default)]
    pub media_context_words: Vec<String>,
    /// Claims that a FILE or DOCUMENT was sent ("file sent", "attached the
    /// file", "sent the report"). Paired with `file_context_words` to catch a
    /// delivery claim in a turn that never invoked a document-sending tool
    /// (#825). Document sending is newer than image generation, so the media
    /// checks (#747) did not cover it.
    #[serde(default)]
    pub file_delivery_phrases: Vec<String>,
    /// Words marking the claimed deliverable as a file rather than a message
    /// ("file", "document", "report", ".md"). Paired with
    /// `file_delivery_phrases`.
    #[serde(default)]
    pub file_context_words: Vec<String>,
}

/// Embedded TOML content (compile-time validated).
const EN_TOML: &str = include_str!("en.toml");
const RU_TOML: &str = include_str!("ru.toml");
const AR_TOML: &str = include_str!("ar.toml");
const ZH_TOML: &str = include_str!("zh.toml");
const ES_TOML: &str = include_str!("es.toml");
const PT_TOML: &str = include_str!("pt.toml");
const FR_TOML: &str = include_str!("fr.toml");
const ID_TOML: &str = include_str!("id.toml");

pub(crate) static LANG_EN: LazyLock<LangConfig> =
    LazyLock::new(|| toml::from_str(EN_TOML).expect("BUG: en.toml failed to parse at runtime"));
pub(crate) static LANG_RU: LazyLock<LangConfig> =
    LazyLock::new(|| toml::from_str(RU_TOML).expect("BUG: ru.toml failed to parse at runtime"));
pub(crate) static LANG_AR: LazyLock<LangConfig> =
    LazyLock::new(|| toml::from_str(AR_TOML).expect("BUG: ar.toml failed to parse at runtime"));
pub(crate) static LANG_ZH: LazyLock<LangConfig> =
    LazyLock::new(|| toml::from_str(ZH_TOML).expect("BUG: zh.toml failed to parse at runtime"));
pub(crate) static LANG_ES: LazyLock<LangConfig> =
    LazyLock::new(|| toml::from_str(ES_TOML).expect("BUG: es.toml failed to parse at runtime"));
pub(crate) static LANG_PT: LazyLock<LangConfig> =
    LazyLock::new(|| toml::from_str(PT_TOML).expect("BUG: pt.toml failed to parse at runtime"));
pub(crate) static LANG_FR: LazyLock<LangConfig> =
    LazyLock::new(|| toml::from_str(FR_TOML).expect("BUG: fr.toml failed to parse at runtime"));
pub(crate) static LANG_ID: LazyLock<LangConfig> =
    LazyLock::new(|| toml::from_str(ID_TOML).expect("BUG: id.toml failed to parse at runtime"));

/// A language this build ships.
///
/// The single definition of the pack list. Adding a language means one
/// variant here and its TOML files, and the exhaustive matches refuse to
/// compile until both table families are wired: `phantom_lang`'s own
/// [`Lang::pack`] and `utils::prompt_analyzer`'s `analyzer_pack`. A language
/// registered in one family and not the other is what #2226 was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// English.
    En,
    /// Russian.
    Ru,
    /// Arabic.
    Ar,
    /// Chinese, Simplified and Traditional.
    Zh,
    /// Spanish.
    Es,
    /// Portuguese.
    Pt,
    /// French.
    Fr,
    /// Indonesian.
    Id,
}

impl Lang {
    /// Every shipped language, in detection-priority order.
    pub const ALL: [Lang; 8] = [
        Lang::En,
        Lang::Ru,
        Lang::Ar,
        Lang::Zh,
        Lang::Es,
        Lang::Pt,
        Lang::Fr,
        Lang::Id,
    ];

    /// The ISO 639-1 code, for logs and diagnostics.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ru => "ru",
            Lang::Ar => "ar",
            Lang::Zh => "zh",
            Lang::Es => "es",
            Lang::Pt => "pt",
            Lang::Fr => "fr",
            Lang::Id => "id",
        }
    }

    /// This language's phantom-detection pack.
    pub fn pack(self) -> &'static LangConfig {
        match self {
            Lang::En => &LANG_EN,
            Lang::Ru => &LANG_RU,
            Lang::Ar => &LANG_AR,
            Lang::Zh => &LANG_ZH,
            Lang::Es => &LANG_ES,
            Lang::Pt => &LANG_PT,
            Lang::Fr => &LANG_FR,
            Lang::Id => &LANG_ID,
        }
    }
}

/// Every loaded language config, in detection-priority order.
///
/// The phantom-intent detectors scan intent phrases across all languages
/// at once: `detect_lang` only routes Cyrillic, Arabic, CJK and
/// accented-Latin text reliably, so accent-free non-English narration (e.g.
/// `"Voy a usar write_file…"`, no ñ/¿) otherwise falls through to English
/// and slips past. Intent phrases are multi-word and carry
/// language-distinctive tokens, so cross-language scanning is
/// collision-free; the short single-word `action_verbs` are not, which is why
/// they stay gated to the detected language. 2026-06-12.
pub fn all_langs() -> [&'static LangConfig; 8] {
    #[cfg(test)]
    scan_counters::bump();
    Lang::ALL.map(Lang::pack)
}

/// Test-only instrumentation for the phantom scan work shape (#2245).
///
/// Every phantom detector reads its tables across all packs, so one scan of a
/// narration costs one traversal of this list per pack. The work is therefore
/// proportional to the pack count and must stay independent of how long the
/// narration is. `phantom_scan_shape_test` asserts exactly that by resetting
/// this counter, running the same detector over inputs of growing length, and
/// requiring an identical traversal count.
///
/// Thread-local rather than global: `cargo test` runs tests in parallel on the
/// same binary, and a shared counter would let one test observe another's
/// traversals. The whole module is behind `#[cfg(test)]`, so a release build
/// carries no counter and no atomic.
#[cfg(test)]
pub(crate) mod scan_counters {
    use std::cell::Cell;

    thread_local! {
        static PACK_TRAVERSALS: Cell<usize> = const { Cell::new(0) };
    }

    /// Zero the counter for the current test thread.
    pub(crate) fn reset() {
        PACK_TRAVERSALS.with(|count| count.set(0));
    }

    /// Pack traversals recorded on this thread since the last [`reset`].
    pub(crate) fn traversals() -> usize {
        PACK_TRAVERSALS.with(Cell::get)
    }

    /// Record one traversal of the pack list.
    pub(crate) fn bump() {
        PACK_TRAVERSALS.with(|count| count.set(count.get() + 1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_lang_resolves_to_its_own_pack() {
        let codes: Vec<&str> = Lang::ALL.iter().map(|lang| lang.code()).collect();
        let unique: std::collections::HashSet<&&str> = codes.iter().collect();
        assert_eq!(unique.len(), codes.len(), "two languages share a code");

        let packs: Vec<*const LangConfig> = Lang::ALL
            .iter()
            .map(|lang| lang.pack() as *const LangConfig)
            .collect();
        let unique_packs: std::collections::HashSet<*const LangConfig> =
            packs.iter().copied().collect();
        assert_eq!(
            unique_packs.len(),
            packs.len(),
            "two languages resolve to the same pack"
        );
    }

    #[test]
    fn all_langs_matches_the_registry_order() {
        let listed = all_langs();
        assert_eq!(listed.len(), Lang::ALL.len());
        for (lang, pack) in Lang::ALL.iter().zip(listed) {
            assert!(std::ptr::eq(pack, lang.pack()));
        }
    }
}
