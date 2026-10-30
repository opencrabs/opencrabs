//! Arabic pack and Arabic script detection (#2222).
//!
//! The pack existed in neither half of the problem: there was no `ar.toml`, and
//! `detect_lang` had no Arabic branch, so an Arabic message fell through to
//! English and the English packs found nothing in it. A turn that narrated its
//! work in Arabic and called no tool was ruled a clean completion.
//!
//! The script branch is not optional for the pack to work. The multi-word
//! intent phrases are scanned across every language, so they fire once the
//! pack exists; the short single-word `action_verbs` are gated to the DETECTED
//! language, which means an Arabic pack without an Arabic detection branch
//! would carry that list and never reach it.

use crate::brain::agent::service::phantom::{
    has_phantom_tool_intent, has_phantom_tool_intent_no_tools,
};
use crate::brain::agent::service::phantom_lang::{Lang, detect_lang, detect_language};
use crate::utils::prompt_analyzer::detect_language as analyzer_detect_language;

/// Criterion 1: an Arabic sentence resolves to Arabic, not to the English
/// fallback. Without this every other test in this file would pass on a
/// mis-detected message only by luck of the cross-language scan.
#[test]
fn an_arabic_sentence_resolves_to_the_arabic_pack() {
    for sentence in [
        "سأفحص الملف الآن",
        "دعني أراجع التغييرات في الفرع",
        "يجب أن أصلح الخطأ في الكود",
        "سوف ننشر التغييرات بعد الاختبارات",
    ] {
        assert_eq!(
            detect_lang(sentence),
            Lang::Ar,
            "{sentence:?} must resolve to Arabic"
        );
        assert!(
            std::ptr::eq(detect_language(sentence), Lang::Ar.pack()),
            "{sentence:?} must resolve to the Arabic pack"
        );
    }
}

/// The other table family resolves from the same id, which is the invariant
/// #2226 established: one definition of the language set, two exhaustive
/// matches. A language registered in one family and not the other is what that
/// issue was.
#[test]
fn the_arabic_id_resolves_in_both_table_families() {
    assert!(std::ptr::eq(
        analyzer_detect_language("سأفحص الملف الآن"),
        &*crate::utils::prompt_analyzer::LANG_AR
    ));
    assert!(std::ptr::eq(
        Lang::Ar.pack(),
        &*crate::brain::agent::service::phantom_lang::config::LANG_AR
    ));
}

/// Criterion 2: fabrication claims in Arabic are caught. Three shapes are
/// pinned separately, because they travel three different paths and only the
/// first two are reachable in a turn that called no tool:
///
/// - a future intent ("I will publish") goes through `intent_phrases`, which
///   is scanned across every pack;
/// - a perfective verb ("I checked") goes through `action_verbs`, which is
///   gated to the detected language and therefore depends on the script
///   branch landing in this commit;
/// - a passive completion ("has been verified") goes through
///   `completion_claims`, which only the strict gate reads, so it is pinned
///   against that gate rather than the zero-tool one.
#[test]
fn an_arabic_intent_announcement_is_caught_without_tools() {
    for claim in [
        "سأنشر التغييرات الآن.",
        "سأفحص الملف الآن.",
        "دعني أراجع التغييرات في الفرع.",
    ] {
        assert!(
            has_phantom_tool_intent_no_tools(claim),
            "{claim:?} promises work that no tool call performed"
        );
    }
}

#[test]
fn an_arabic_past_tense_claim_is_caught_without_tools() {
    // The action_verbs list is gated to the detected language, so this passing
    // is also the receipt that the Arabic script branch reaches the pack.
    for claim in [
        "فحصت الملف وأصلحت الخطأ.",
        "شغلت الاختبارات ومرت كلها بنجاح.",
        "نشرت التغييرات إلى الفرع الرئيسي.",
    ] {
        assert!(
            has_phantom_tool_intent_no_tools(claim),
            "{claim:?} claims work that no tool call performed"
        );
    }
}

#[test]
fn an_arabic_completion_claim_is_caught_by_the_strict_gate() {
    // The passive "tamma" family lives in completion_claims, which the
    // zero-tool gate does not read: that list is checked when a real tool call
    // shares the response with the claim. The text is long enough to clear the
    // strict gate's length floor, which is the condition it runs under.
    let claim = "تم التحقق من التغييرات وكل شيء جاهز. النتيجة مطابقة لما هو متوقع \
                 في السجل، ولم يتبق شيء.";
    assert!(
        has_phantom_tool_intent(claim),
        "the passive completion family must reach the strict gate"
    );
}

/// The expanded pack is the risk: a phrase that is too short or too generic
/// turns an ordinary Arabic answer into a phantom. These are statements about
/// state and location, the shape a real answer takes.
#[test]
fn ordinary_arabic_prose_stays_clean() {
    for statement in [
        "الملف موجود في المجلد الرئيسي.",
        "الإعدادات تأتي من ملف التكوين في المسار الافتراضي.",
        "هذه الدالة تستقبل معاملين وتعيد قيمة واحدة.",
        "المشروع مكتوب بلغة رست ويستخدم مكتبة tokio.",
        "الاختبارات موجودة في مجلد src/tests.",
    ] {
        assert!(
            !has_phantom_tool_intent_no_tools(statement),
            "{statement:?} is a plain statement, not an announcement"
        );
    }
}

/// A wrong branch would be worse than none: Arabic text must not be routed to
/// Russian just because both are non-Latin scripts.
#[test]
fn arabic_and_cyrillic_do_not_steal_each_other() {
    assert_eq!(detect_lang("Давайте проверю логи"), Lang::Ru);
    assert_eq!(detect_lang("سأفحص الملف الآن"), Lang::Ar);
    // Latin text with no Arabic stays where it was.
    assert_eq!(detect_lang("hello world"), Lang::En);
}
