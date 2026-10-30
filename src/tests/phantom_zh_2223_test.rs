//! Chinese pack and CJK script detection (#2223).
//!
//! Chinese had no pack and no detection branch, so a Chinese turn fell through
//! to the English packs and a reply that narrated its work while calling no
//! tool was ruled a clean completion.
//!
//! Two things make Chinese different from the other packs, and both are pinned
//! here rather than only written in a comment:
//!
//! 1. **Both script variants ship.** Every phrase exists in Simplified and in
//!    Traditional, because a reply is written in whichever the user's locale
//!    uses and a pack carrying one would miss the other outright.
//! 2. **`action_verbs` is empty on purpose.** Its only consumer splits on
//!    whitespace and compares one word at a time, and Chinese text has no
//!    whitespace between words, so no single verb entry could ever match. The
//!    perfective claims live in `completion_claims`, which is a substring scan.
//!    The test below proves the claim is still caught, so the empty list is a
//!    deliberate split rather than a forgotten arm.
//!
//! The script branch also has to keep Japanese and Korean out. Both write with
//! Han characters, and this build ships no pack for either, so routing them to
//! Chinese would apply Chinese phrases to their text and call it detection.

use crate::brain::agent::service::phantom::{
    has_phantom_tool_intent, has_phantom_tool_intent_no_tools,
};
use crate::brain::agent::service::phantom_lang::{Lang, detect_lang, detect_language};
use crate::utils::prompt_analyzer::detect_language as analyzer_detect_language;

/// Criterion 1: a CJK sentence resolves to Chinese, not to the English
/// fallback, in both table families.
#[test]
fn a_chinese_sentence_resolves_to_the_chinese_pack() {
    for sentence in [
        "我会检查配置文件",
        "我會檢查配置文件",
        "让我先看一下日志",
        "現在運行測試",
    ] {
        assert_eq!(
            detect_lang(sentence),
            Lang::Zh,
            "{sentence:?} must resolve to Chinese"
        );
        assert!(
            std::ptr::eq(detect_language(sentence), Lang::Zh.pack()),
            "{sentence:?} must resolve to the Chinese pack"
        );
    }
}

/// The analyzer family resolves from the same id, which is the invariant the
/// registry refactor put in place: one definition of the language set, two
/// exhaustive matches.
#[test]
fn the_chinese_id_resolves_in_both_table_families() {
    assert!(std::ptr::eq(
        analyzer_detect_language("我会检查配置文件"),
        &*crate::utils::prompt_analyzer::LANG_ZH
    ));
    assert!(std::ptr::eq(
        Lang::Zh.pack(),
        &*crate::brain::agent::service::phantom_lang::config::LANG_ZH
    ));
}

/// Criterion 2: a Chinese fabrication claim is caught. The shapes travel
/// different arms, so each is pinned against the gate that reads it: a future
/// intent goes through `intent_phrases`, which the zero-tool gate scans across
/// every pack, while a completion claim goes through `completion_claims`, which
/// only the strict gate reads (it is the gate that runs when a real tool call
/// shares the response with the claim).
#[test]
fn a_chinese_intent_announcement_is_caught_without_tools() {
    for claim in [
        "我会检查配置文件并修复问题。",
        "我會檢查配置文件並修復問題。",
        "让我先看一下日志。",
        "讓我先看一下日誌。",
        "现在运行测试。",
        "現在運行測試。",
    ] {
        assert!(
            has_phantom_tool_intent_no_tools(claim),
            "{claim:?} promises work that no tool call performed"
        );
    }
}

#[test]
fn a_chinese_completion_claim_is_caught_by_the_strict_gate() {
    // Padded past the strict gate's length floor, which is the condition it
    // runs under. Both script variants are covered.
    for claim in [
        "已检查完成，一切就绪。配置文件已经更新，测试也全部通过，没有剩下任何问题需要处理。",
        "已檢查完成，一切就緒。配置文件已經更新，測試也全部通過，沒有剩下任何問題需要處理。",
        "运行测试完成，全部通过。构建成功，编译没有错误，日志里也没有异常输出需要排查。",
        "運行測試完成，全部通過。構建成功，編譯沒有錯誤，日誌裡也沒有異常輸出需要排查。",
    ] {
        assert!(
            has_phantom_tool_intent(claim),
            "{claim:?} claims work that no tool call performed"
        );
    }
}

/// The empty `action_verbs` list is a deliberate split, not a missing arm: the
/// perfective claims are carried by `completion_claims`, which is a substring
/// scan. If someone fills `action_verbs` later expecting it to work, this test
/// still passes and the comment in the pack still explains why it cannot.
#[test]
fn the_pack_declares_no_single_word_action_verbs() {
    assert!(
        Lang::Zh.pack().action_verbs.is_empty(),
        "action_verbs cannot match inside unspaced Chinese text; \
         keep the perfective claims in completion_claims"
    );
    assert!(
        !Lang::Zh.pack().completion_claims.is_empty(),
        "the perfective claims have to live somewhere"
    );
}

/// Both variants are present, which is what makes the pack usable for either
/// locale rather than for one of them.
#[test]
fn both_script_variants_are_carried() {
    let phrases = &Lang::Zh.pack().intent_phrases;
    for (simplified, traditional) in [
        ("我会检查", "我會檢查"),
        ("让我", "讓我"),
        ("运行", "運行"),
        ("测试", "測試"),
    ] {
        assert!(
            phrases.iter().any(|p| p.contains(simplified)),
            "the Simplified form {simplified:?} is missing"
        );
        assert!(
            phrases.iter().any(|p| p.contains(traditional)),
            "the Traditional form {traditional:?} is missing"
        );
    }
}

/// Japanese and Korean write with Han characters too. Neither has a pack in
/// this build, so neither may be routed to the Chinese one.
#[test]
fn japanese_and_korean_do_not_route_to_chinese() {
    assert_ne!(detect_lang("設定ファイルを確認します"), Lang::Zh);
    assert_ne!(detect_lang("설정 파일을 확인하겠습니다"), Lang::Zh);
    // And Chinese is not routed away from itself by a stray Latin word.
    assert_eq!(detect_lang("检查这个 README 文件"), Lang::Zh);
}

/// The expanded pack is the risk: a phrase that is too short or too generic
/// turns an ordinary Chinese answer into a phantom.
#[test]
fn ordinary_chinese_prose_stays_clean() {
    for statement in [
        "配置文件位于用户主目录下。",
        "這個函數接收兩個參數並返回一個值。",
        "测试文件放在 src/tests 目录里。",
        "项目使用 Rust 编写，依赖 tokio 运行时。",
        "这个错误通常表示端口已被占用。",
    ] {
        assert!(
            !has_phantom_tool_intent_no_tools(statement),
            "{statement:?} is a plain statement, not an announcement"
        );
    }
}
