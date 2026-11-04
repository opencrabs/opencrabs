//! Coverage floor for every shipped language pack (#2225).
//!
//! English carried 462 `intent_phrases` and 51 `completion_claims` while the
//! other five packs carried between 96 and 164, and between 16 and 26. The
//! gap was not cosmetic: an intent the pack never lists is an intent the
//! detector never sees, so a turn that announced work in Spanish, French,
//! Indonesian, Russian or Portuguese and called no tool was flagged only when
//! the wording happened to be one of the few entries the pack had.
//!
//! The floors below are per key and per pack. They sit under the counts the
//! packs carry today, so ordinary edits have room, and far above the counts
//! before this change, so a trim back to the old coverage fails here instead
//! of shipping as a silent detection gap.
//!
//! The same file guards two properties that are cheap to state and expensive
//! to lose: every non-empty regex field in a pack compiles, and no pack entry
//! carries a replacement character. Both have already cost real coverage.
//! A lookahead in a pack regex makes `Regex::new` return Err, which every
//! consumer swallows with `unwrap_or(false)`, so the arm goes dead with no
//! build error and no warning (#2224). A corrupted byte in `ru.toml` left
//! `"всё обновлено"` spelled with two U+FFFD characters, an entry that could
//! never match anything.

use crate::brain::agent::service::phantom::has_phantom_tool_intent_no_tools;
use crate::brain::agent::service::phantom_lang::{Lang, all_langs};

/// Minimum `intent_phrases` entries per pack. English had 462, the other five
/// had 96 to 164 before #2225; every pack now carries at least 436.
const INTENT_FLOOR: usize = 400;

/// Minimum `completion_claims` entries per pack. English had 51, the other
/// five had 16 to 26 before #2225; every pack now carries at least 54.
const COMPLETION_FLOOR: usize = 45;

#[test]
fn every_pack_carries_comparable_intent_coverage() {
    for lang in Lang::ALL {
        let pack = lang.pack();
        assert!(
            pack.intent_phrases.len() >= INTENT_FLOOR,
            "{} carries {} intent phrases, floor is {}",
            lang.code(),
            pack.intent_phrases.len(),
            INTENT_FLOOR
        );
    }
}

#[test]
fn every_pack_carries_comparable_completion_coverage() {
    for lang in Lang::ALL {
        let pack = lang.pack();
        assert!(
            pack.completion_claims.len() >= COMPLETION_FLOOR,
            "{} carries {} completion claims, floor is {}",
            lang.code(),
            pack.completion_claims.len(),
            COMPLETION_FLOOR
        );
    }
}

#[test]
fn every_pack_regex_compiles() {
    for lang in Lang::ALL {
        let pack = lang.pack();
        let fields: [(&str, &str); 8] = [
            ("line_start_re", &pack.line_start_re),
            ("work_announcement_re", &pack.work_announcement_re),
            ("plan_announcement_re", &pack.plan_announcement_re),
            ("gerund_re", &pack.gerund_re),
            ("trailing_colon_re", &pack.trailing_colon_re),
            ("now_imperative_re", &pack.now_imperative_re),
            ("numbered_steps_re", &pack.numbered_steps_re),
            ("generic_intent_re", &pack.generic_intent_re),
        ];
        for (name, pattern) in fields {
            if pattern.is_empty() {
                continue;
            }
            assert!(
                regex::Regex::new(pattern).is_ok(),
                "{}: {} does not compile, so its detector arm is silently dead",
                lang.code(),
                name
            );
        }
    }
}

#[test]
fn no_pack_entry_carries_a_replacement_character() {
    for lang in Lang::ALL {
        let pack = lang.pack();
        let entries = pack
            .intent_phrases
            .iter()
            .chain(pack.completion_claims.iter())
            .chain(pack.action_verbs.iter())
            .chain(pack.executed_framings.iter());
        for entry in entries {
            assert!(
                !entry.contains('\u{fffd}'),
                "{}: {:?} carries a replacement character and can never match",
                lang.code(),
                entry
            );
        }
    }
}

#[test]
fn the_pack_list_is_the_one_under_test() {
    // A language added to the registry is covered by every loop above without
    // a second edit here. This pins the count so the floors cannot quietly
    // apply to fewer packs than the binary ships.
    assert_eq!(all_langs().len(), Lang::ALL.len());
    assert_eq!(Lang::ALL.len(), 8);
}

/// One real announcement per pack, in the shape a speaker of that language
/// writes it, must be caught by the zero-tool detector. A floor test alone
/// would pass on a list of junk, so this pins that the entries do their job.
#[test]
fn a_real_announcement_is_caught_in_every_pack() {
    let samples = [
        ("en", "I'll update the config now."),
        ("es", "Voy a actualizar el archivo de configuración."),
        ("fr", "Je vais mettre à jour le fichier de configuration."),
        ("id", "Saya akan memperbarui file konfigurasi."),
        ("ru", "Я обновлю файл конфигурации."),
        ("pt", "Vou atualizar o ficheiro de configuração."),
        ("ar", "سأحدث ملف الإعدادات الآن."),
        ("zh", "我会更新配置文件。"),
    ];
    for (code, sentence) in samples {
        assert!(
            has_phantom_tool_intent_no_tools(sentence),
            "{code}: {sentence:?} must be caught as an announcement"
        );
    }
}

/// The other half: ordinary prose in the same languages must stay clean. The
/// expanded lists are the risk here, because a phrase that is too short or too
/// generic turns a normal answer into a phantom.
#[test]
fn ordinary_prose_stays_clean_in_every_pack() {
    let samples = [
        ("en", "The config file lives in the home directory."),
        (
            "es",
            "El archivo de configuración está en el directorio home.",
        ),
        (
            "fr",
            "Le fichier de configuration se trouve dans le répertoire personnel.",
        ),
        ("id", "File konfigurasi ada di direktori home."),
        ("ru", "Файл конфигурации находится в домашнем каталоге."),
        ("pt", "O ficheiro de configuração está na pasta home."),
        ("ar", "ملف الإعدادات موجود في المجلد الرئيسي."),
        ("zh", "配置文件位于用户主目录下。"),
    ];
    for (code, sentence) in samples {
        assert!(
            !has_phantom_tool_intent_no_tools(sentence),
            "{code}: {sentence:?} is a plain statement, not an announcement"
        );
    }
}
