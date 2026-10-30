//! Character-set language detection: which of the packs a message reads as.

use super::config::{Lang, LangConfig};

/// Which language `text` reads as, from character-set heuristics over its
/// first 500 characters.
///
/// The script branches live here next to the Cyrillic test, not in the
/// registry. Callers that need the table rather than the id use
/// [`detect_language`], and callers that scan every language use
/// `config::all_langs`.
///
/// The script branches matter more than routing: the short single-word
/// `action_verbs` are gated to the detected language, so a language whose
/// script has no branch here can list those verbs and never fire them.
pub fn detect_lang(text: &str) -> Lang {
    let mut cyrillic = 0u32;
    let mut arabic = 0u32;
    let mut han = 0u32;
    let mut kana = 0u32;
    let mut hangul = 0u32;
    let mut latin_accent = 0u32;
    let mut total_alpha = 0u32;

    for ch in text.chars().take(500) {
        if ch.is_alphabetic() {
            total_alpha += 1;
            if ('\u{0400}'..='\u{04FF}').contains(&ch) {
                cyrillic += 1;
            } else if is_arabic(ch) {
                arabic += 1;
            } else if is_han(ch) {
                han += 1;
            } else if is_kana(ch) {
                kana += 1;
            } else if is_hangul(ch) {
                hangul += 1;
            } else if ('\u{00C0}'..='\u{024F}').contains(&ch) {
                latin_accent += 1;
            }
        }
    }

    if total_alpha == 0 {
        return Lang::En;
    }

    // Cyrillic > 20% of alpha chars → Russian
    if cyrillic * 5 > total_alpha {
        return Lang::Ru;
    }

    // Arabic > 20% of alpha chars → Arabic. Same threshold as Cyrillic, and
    // the same reason for a script branch: the single-word `action_verbs` are
    // gated to the detected language, so without this branch the Arabic pack
    // would carry them and never reach them.
    if arabic * 5 > total_alpha {
        return Lang::Ar;
    }

    // Han characters without kana or hangul → Chinese.
    //
    // The kana and hangul tests are what keep Japanese and Korean out: both
    // write with Han characters too, and this build ships no pack for either,
    // so routing them here would apply Chinese phrases to Japanese text and
    // call it detection. A Japanese sentence carries kana by definition, and a
    // Korean one carries hangul, so requiring both to be absent separates the
    // three scripts without a language model.
    if han * 5 > total_alpha && kana == 0 && hangul == 0 {
        return Lang::Zh;
    }

    // For Latin-accent text, distinguish Spanish/Portuguese/French
    // by looking for language-specific characters
    if latin_accent > 0 {
        // Portuguese-specific: ã, õ, ç
        if text.contains('ã')
            || text.contains('õ')
            || text.contains('ç')
            || text.contains('Ã')
            || text.contains('Õ')
            || text.contains('Ç')
        {
            return Lang::Pt;
        }
        // Spanish-specific: ñ, ¿, ¡
        if text.contains('ñ') || text.contains('Ñ') || text.contains('¿') || text.contains('¡')
        {
            return Lang::Es;
        }
        // If we have significant accented Latin but no PT/ES markers,
        // check for French patterns (à, â, ç, é, è, ê, ë, î, ï, ô, ù, û, ü, ÿ)
        // French is the fallback for accented Latin since it's the most
        // common accented-Latin language after Spanish/Portuguese
        if text.contains('à')
            || text.contains('â')
            || text.contains('é')
            || text.contains('è')
            || text.contains('ê')
            || text.contains('ë')
            || text.contains('î')
            || text.contains('ï')
            || text.contains('ô')
            || text.contains('û')
            || text.contains('ù')
            || text.contains('ü')
            || text.contains('ÿ')
        {
            return Lang::Fr;
        }
    }

    Lang::En
}

/// Whether `ch` is an Arabic-script letter: the main block, the supplement,
/// and Extended-A. The presentation forms are deliberately absent, because
/// text reaching the agent is logical-order Arabic, not the shaped glyphs a
/// renderer emits.
fn is_arabic(ch: char) -> bool {
    matches!(ch,
        '\u{0600}'..='\u{06FF}' | '\u{0750}'..='\u{077F}' | '\u{08A0}'..='\u{08FF}')
}

/// Whether `ch` is a Han ideograph: the main unified block, Extension A, and
/// the compatibility ideographs. Kana and hangul are tested separately, because
/// Japanese and Korean mix ideographs with their own syllabaries.
fn is_han(ch: char) -> bool {
    matches!(ch,
        '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' | '\u{F900}'..='\u{FAFF}')
}

/// Whether `ch` is a kana syllable: hiragana or katakana.
fn is_kana(ch: char) -> bool {
    matches!(ch, '\u{3040}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}')
}

/// Whether `ch` is a hangul syllable or jamo.
fn is_hangul(ch: char) -> bool {
    matches!(ch,
        '\u{AC00}'..='\u{D7AF}' | '\u{1100}'..='\u{11FF}' | '\u{3130}'..='\u{318F}')
}

/// The pack for `text`, i.e. [`detect_lang`] resolved through the registry.
pub fn detect_language(text: &str) -> &'static LangConfig {
    detect_lang(text).pack()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_keeps_its_character_set_routing() {
        assert_eq!(detect_lang("Давайте проверю логи"), Lang::Ru);
        assert_eq!(detect_lang("سأفحص الملف وأصلح الخطأ"), Lang::Ar);
        assert_eq!(detect_lang("ملف"), Lang::Ar);
        assert_eq!(detect_lang("我会检查配置文件"), Lang::Zh);
        assert_eq!(detect_lang("我會檢查配置文件"), Lang::Zh);
        // Japanese and Korean write with Han characters too, and this build
        // ships no pack for either, so they must not route to Chinese.
        assert_eq!(detect_lang("設定ファイルを確認します"), Lang::En);
        assert_eq!(detect_lang("설정 파일을 확인하겠습니다"), Lang::En);
        assert_eq!(detect_lang("planejamento com ã"), Lang::Pt);
        assert_eq!(detect_lang("¿cómo estás?"), Lang::Es);
        assert_eq!(detect_lang("résumé à la carte"), Lang::Fr);
        assert_eq!(detect_lang("hello world"), Lang::En);
        assert_eq!(detect_lang(""), Lang::En);
    }

    #[test]
    fn detect_language_resolves_through_the_registry() {
        assert!(std::ptr::eq(
            detect_language("Давайте проверю логи и исправлю ошибку"),
            Lang::Ru.pack()
        ));
        assert!(std::ptr::eq(
            detect_language("no accents here"),
            Lang::En.pack()
        ));
    }
}
