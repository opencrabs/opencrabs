//! Tests for auto-title post-processing (`clean_auto_title`).
//!
//! The LLM returns a raw title string. These tests verify the cleanup:
//! trim whitespace, strip surrounding quotes, cap at 60 characters.

use crate::brain::agent::service::AgentService;

mod clean_auto_title {
    use super::*;

    #[test]
    fn basic_title_unchanged() {
        assert_eq!(
            AgentService::clean_auto_title("Rust Refactoring"),
            "Rust Refactoring"
        );
    }

    #[test]
    fn trims_whitespace() {
        assert_eq!(
            AgentService::clean_auto_title("  Hashline Debug  "),
            "Hashline Debug"
        );
    }

    #[test]
    fn strips_double_quotes() {
        assert_eq!(
            AgentService::clean_auto_title("\"Auto Title Fix\""),
            "Auto Title Fix"
        );
    }

    #[test]
    fn strips_single_quotes() {
        assert_eq!(
            AgentService::clean_auto_title("'Session Config'"),
            "Session Config"
        );
    }

    #[test]
    fn caps_at_60_chars() {
        let long = "a".repeat(80);
        let result = AgentService::clean_auto_title(&long);
        assert_eq!(result.len(), 60);
    }

    #[test]
    fn caps_at_60_chars_cyrillic() {
        // Cyrillic is 2 bytes per char — byte slicing at 60 panics mid-char
        let cyrillic = "А".repeat(40); // 80 bytes, 40 chars
        let long = format!("{cyrillic}Extra Text To Push Past Sixty Chars Here");
        let result = AgentService::clean_auto_title(&long);
        assert!(
            result.chars().count() <= 60,
            "should cap at 60 chars, got {}",
            result.chars().count()
        );
        // Must not panic and must be valid UTF-8
        assert!(!result.is_empty());
    }

    #[test]
    fn caps_at_60_chars_emoji() {
        // Emoji can be 4 bytes per char
        let emoji = "🦀".repeat(30); // 120 bytes, 30 chars
        let long = format!("{emoji}Extra text to push past sixty characters boundary here ok");
        let result = AgentService::clean_auto_title(&long);
        assert!(result.chars().count() <= 60);
    }

    #[test]
    fn exactly_60_chars_unchanged() {
        let exact = "b".repeat(60);
        assert_eq!(AgentService::clean_auto_title(&exact), exact);
    }

    #[test]
    fn empty_string() {
        assert_eq!(AgentService::clean_auto_title(""), "");
    }

    #[test]
    fn only_whitespace() {
        assert_eq!(AgentService::clean_auto_title("   "), "");
    }

    #[test]
    fn only_quotes() {
        assert_eq!(AgentService::clean_auto_title("\"\""), "");
    }

    #[test]
    fn quotes_and_whitespace() {
        assert_eq!(
            AgentService::clean_auto_title("  \"Clean Title\"  "),
            "Clean Title"
        );
    }

    #[test]
    fn trailing_punctuation_kept() {
        // The LLM prompt says no punctuation but we don't enforce it in code
        assert_eq!(AgentService::clean_auto_title("Fix Bug."), "Fix Bug.");
    }

    /// #2743: the prompt asks for three words, but a model can ignore it, so
    /// a six-word answer must be stored as at most three words.
    #[test]
    fn caps_six_word_answer_at_three_words() {
        let result = AgentService::clean_auto_title("Investigate the failing build on staging now");
        assert!(
            result.split_whitespace().count() <= 3,
            "expected at most 3 words, got {result:?}"
        );
        assert!(!result.is_empty());
    }

    /// #2743: filler goes first so the title keeps its meaning. This is the
    /// issue's own example verbatim.
    #[test]
    fn drops_filler_words_before_content() {
        assert_eq!(
            AgentService::clean_auto_title("Análise do espaço em disco e lixo"),
            "Análise espaço disco"
        );
    }

    #[test]
    fn three_word_title_unchanged() {
        assert_eq!(
            AgentService::clean_auto_title("Análise espaço disco"),
            "Análise espaço disco"
        );
    }

    /// #2743: the word cap is multibyte safe (regression for #884). Three
    /// accented Portuguese words survive intact, never split mid-char.
    #[test]
    fn three_word_cap_is_multibyte_safe() {
        let result = AgentService::clean_auto_title("Análise espaço configuração lixo servidor");
        assert_eq!(result, "Análise espaço configuração");
        assert_eq!(result.chars().count(), 27);
    }

    /// Issue #115: the `[chat:ID]` suffix is the stable session identifier.
    /// It is not a title word and must survive the cap untouched.
    #[test]
    fn keeps_chat_suffix_when_capping_words() {
        let result = AgentService::clean_auto_title(
            "Investigate the failing build on staging now [chat:7711740248]",
        );
        assert!(
            result.ends_with("[chat:7711740248]"),
            "suffix must survive the cap, got {result:?}"
        );
        let body = result.trim_end_matches("[chat:7711740248]").trim();
        assert!(
            body.split_whitespace().count() <= 3,
            "title body should be at most 3 words, got {body:?}"
        );
    }

    /// A title made only of filler must not be emptied by the trim.
    #[test]
    fn all_filler_title_keeps_words() {
        let result = AgentService::clean_auto_title("the of and to in on");
        assert!(!result.is_empty(), "must not return an empty title");
        assert!(result.split_whitespace().count() <= 3);
    }
}

mod is_default_channel_title {
    use super::*;

    #[test]
    fn telegram_dm_title() {
        assert!(AgentService::is_default_channel_title(
            "Telegram: DM John (12345) [chat:67890]"
        ));
    }

    #[test]
    fn telegram_group_title() {
        // Telegram groups have no clear default pattern marker, so they're NOT default
        // to prevent auto-title from firing on every message
        assert!(!AgentService::is_default_channel_title(
            "Telegram: My Group [chat:12345]"
        ));
    }

    #[test]
    fn telegram_auto_titled_not_default() {
        // After auto-title runs, the title should NOT be considered default
        assert!(!AgentService::is_default_channel_title(
            "Telegram: Fix Bug Report [chat:67890]"
        ));
    }

    #[test]
    fn discord_title() {
        assert!(AgentService::is_default_channel_title("Discord: #general"));
    }

    #[test]
    fn slack_title() {
        assert!(AgentService::is_default_channel_title("Slack: #random"));
    }

    #[test]
    fn whatsapp_title() {
        // WhatsApp has no clear default pattern marker, so it's NOT default
        // to prevent auto-title from firing on every message
        assert!(!AgentService::is_default_channel_title(
            "WhatsApp: John Doe"
        ));
    }

    #[test]
    fn trello_title() {
        // Trello has no clear default pattern marker, so it's NOT default
        // to prevent auto-title from firing on every message
        assert!(!AgentService::is_default_channel_title("Trello: My Board"));
    }

    #[test]
    fn custom_title_not_default() {
        assert!(!AgentService::is_default_channel_title(
            "Rust Refactoring Session"
        ));
    }

    #[test]
    fn empty_title_not_default() {
        assert!(!AgentService::is_default_channel_title(""));
    }

    #[test]
    fn partial_prefix_not_default() {
        assert!(!AgentService::is_default_channel_title("Telegram"));
        assert!(!AgentService::is_default_channel_title("TelegramBot: test"));
    }

    #[test]
    fn new_chat_is_default() {
        assert!(AgentService::is_default_channel_title("New Chat"));
    }
}

mod extract_channel_prefix {
    use super::*;

    #[test]
    fn telegram_prefix() {
        assert_eq!(
            AgentService::extract_channel_prefix("Telegram: DM John"),
            "Telegram: "
        );
    }

    #[test]
    fn discord_prefix() {
        assert_eq!(
            AgentService::extract_channel_prefix("Discord: #general"),
            "Discord: "
        );
    }

    #[test]
    fn slack_prefix() {
        assert_eq!(
            AgentService::extract_channel_prefix("Slack: #random"),
            "Slack: "
        );
    }

    #[test]
    fn whatsapp_prefix() {
        assert_eq!(
            AgentService::extract_channel_prefix("WhatsApp: John"),
            "WhatsApp: "
        );
    }

    #[test]
    fn trello_prefix() {
        assert_eq!(
            AgentService::extract_channel_prefix("Trello: Board"),
            "Trello: "
        );
    }

    #[test]
    fn no_prefix() {
        assert_eq!(AgentService::extract_channel_prefix("New Chat"), "");
        assert_eq!(AgentService::extract_channel_prefix("Custom Title"), "");
        assert_eq!(AgentService::extract_channel_prefix(""), "");
    }
}

mod strip_channel_preamble {
    use super::*;

    #[test]
    fn plain_message_unchanged() {
        assert_eq!(
            AgentService::strip_channel_preamble("Fix the login bug"),
            "Fix the login bug"
        );
    }

    #[test]
    fn strips_single_channel_block() {
        let input = "[Channel: Telegram (chat_id: -100123) — auto-delivery]\n\nFix the login bug";
        assert_eq!(
            AgentService::strip_channel_preamble(input),
            "Fix the login bug"
        );
    }

    #[test]
    fn strips_multiple_preamble_blocks() {
        let input = "[Channel: Telegram (chat_id: -100123) — auto-delivery]\n\n\
                      [Reaction directive: use <<react:EMOJI>>]\n\n\
                      [Recent group history (5 messages):\n[13:57] User: hello\n— end history —]\n\n\
                      Fix the login bug";
        assert_eq!(
            AgentService::strip_channel_preamble(input),
            "Fix the login bug"
        );
    }

    #[test]
    fn strips_the_channel_noun_history_block() {
        // Discord and Slack frame the block as "channel history"; only the
        // "group" wording was listed, so their whole window reached the title
        // prompt (#1618, #1619, #1620).
        let input = "[Recent channel history (2 messages) — prior context from various senders,                      NOT the person you are replying to now:\n[13:57] Adi: hello\n\
                     --- end history ---]\n\nFix the login bug";
        assert_eq!(
            AgentService::strip_channel_preamble(input),
            "Fix the login bug"
        );
    }

    #[test]
    fn handles_nested_brackets_in_history() {
        // Group history lines contain [HH:MM] timestamps inside the outer block
        let input =
            "[Recent group history:\n[13:57] Alice: hi\n[14:02] Bob: hey\n— end —]\n\nDeploy now";
        assert_eq!(AgentService::strip_channel_preamble(input), "Deploy now");
    }

    #[test]
    fn preserves_user_bracket_text_after_preamble() {
        let input = "[Channel: Telegram]\n\n[BUG] Fix the crash";
        assert_eq!(
            AgentService::strip_channel_preamble(input),
            "[BUG] Fix the crash"
        );
    }

    #[test]
    fn unmatched_bracket_stops_stripping() {
        let input = "[unclosed block\nFix the bug";
        assert_eq!(
            AgentService::strip_channel_preamble(input),
            "[unclosed block\nFix the bug"
        );
    }

    #[test]
    fn empty_string() {
        assert_eq!(AgentService::strip_channel_preamble(""), "");
    }

    #[test]
    fn only_preamble_returns_empty() {
        let input = "[Channel: Telegram]\n\n[Reaction directive: stuff]";
        assert_eq!(AgentService::strip_channel_preamble(input), "");
    }

    #[test]
    fn trims_leading_whitespace_after_strip() {
        let input = "[Channel: Telegram]\n\n\n   Fix the bug";
        assert_eq!(AgentService::strip_channel_preamble(input), "Fix the bug");
    }
}

/// #2882: a title was generated once, from the first message, and the flag
/// guarding that call never re-armed. These pin the drift test that decides
/// when a stored title has stopped describing the conversation, using the
/// reporter's own case: a 619-message WhatsApp session still titled
/// "PayrollTSJ v5 September" while its November turns were harvest workbooks.
mod title_drift {
    use super::*;

    fn turns(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|text| text.to_string()).collect()
    }

    #[test]
    fn a_title_naming_none_of_the_recent_work_has_drifted() {
        let recent = turns(&[
            "harvest workbook for November, three sheets to reconcile before the Friday deadline",
            "the poster job needs a darker crop on the left side and a tighter bleed margin",
            "send the finished files to the client on Thursday morning once the numbers agree",
        ]);
        assert!(
            AgentService::title_drifted_from(Some("PayrollTSJ v5 September"), &recent),
            "a months-old title naming none of the recent work is drift"
        );
    }

    #[test]
    fn a_title_sharing_one_distinctive_word_is_left_alone() {
        let recent = turns(&[
            "harvest workbook for November, three sheets to reconcile before the Friday deadline",
            "the poster job needs a darker crop on the left side and a tighter bleed margin",
            "send the finished harvest files to the client on Thursday morning once the numbers agree",
        ]);
        assert!(
            !AgentService::title_drifted_from(Some("Harvest workbook"), &recent),
            "one shared distinctive word means the title still fits"
        );
    }

    /// The 200-character floor is what let the fixture above pass vacuously
    /// once. Pin it so a short fixture can never masquerade as a real check.
    #[test]
    fn two_substantial_turns_are_not_enough() {
        let recent = turns(&[
            "harvest workbook for November, three sheets to reconcile before the Friday deadline",
            "send the finished files to the client on Thursday morning once the numbers agree",
        ]);
        assert!(
            !AgentService::title_drifted_from(Some("PayrollTSJ v5 September"), &recent),
            "two turns are below the three-turn floor even when they are long"
        );
    }

    #[test]
    fn three_short_turns_are_not_enough() {
        let recent = turns(&["ok thanks", "yes please", "sounds good"]);
        assert!(
            !AgentService::title_drifted_from(Some("PayrollTSJ v5 September"), &recent),
            "three turns under twenty characters carry no topic signal"
        );
    }

    #[test]
    fn a_one_word_follow_up_never_re_titles() {
        let recent = turns(&["ok", "thanks", "yes"]);
        assert!(
            !AgentService::title_drifted_from(Some("PayrollTSJ v5 September"), &recent),
            "short turns carry no topic signal"
        );
    }

    #[test]
    fn a_missing_title_is_not_drift() {
        assert!(!AgentService::title_drifted_from(None, &turns(&[])));
    }
}
