//! #2110: a contraction is an auxiliary, so prose is not a completion claim.
//!
//! The zero-tool phantom gate splits a reply into sentences, takes the opening
//! words, and flags the sentence when one of them is an `action_verbs` entry
//! unless an auxiliary sits directly before it. The auxiliary list held no
//! contractions, so a joke like "everything I'm tagged in, anyway" matched
//! `tagged` and the gate replaced the whole answer with its canned note. The
//! user never saw the reply.
//!
//! The guard reads the word before the verb, and the word trim only strips the
//! ENDS of a word, so an internal apostrophe survives: the literal
//! contractions are what the list needs. Curly apostrophes are normalised
//! first because models emit both spellings.
//!
//! Fixtures are synthetic and carry no user identifiers.

use crate::brain::agent::service::phantom::{
    has_past_tense_action_claim, has_phantom_tool_intent_no_tools,
};
use crate::brain::agent::service::phantom_lang::detect_language;

/// The English verb table, read from the same TOML the detector uses at
/// runtime. A test built on a hand-written verb list would pass while the real
/// corpus had drifted away from it.
fn english_verbs() -> Vec<String> {
    detect_language("the config is loaded from disk")
        .action_verbs
        .clone()
}

/// Guard against a vacuous suite: every verb the regression leans on has to be
/// in the real corpus, or the assertions below prove nothing.
#[test]
fn the_verb_corpus_holds_every_word_this_regression_uses() {
    let verbs = english_verbs();
    for verb in [
        "tagged",
        "loaded",
        "installed",
        "synced",
        "pushed",
        "committed",
    ] {
        assert!(
            verbs.iter().any(|v| v == verb),
            "{verb} is missing from the English action_verbs table, so a test \
             asserting it is not a claim would pass without testing anything"
        );
    }
}

#[test]
fn a_contraction_before_a_listed_verb_is_prose_not_a_claim() {
    let verbs = english_verbs();
    for sentence in [
        // The exact shape that ate the answer in the log.
        "well, everything i'm tagged in, anyway",
        "it's loaded",
        "they're installed",
        "you're synced",
        "we're done here",
        "he's committed to the deadline",
        // Curly apostrophe, as models type it.
        "it\u{2019}s loaded",
        // Negated copula, same mechanism.
        "it isn't loaded yet",
        "they weren't installed",
    ] {
        assert!(
            !has_past_tense_action_claim(sentence, &verbs),
            "read as a completion claim, which replaces the answer: {sentence:?}"
        );
    }
}

#[test]
fn a_real_completion_claim_is_still_detected() {
    let verbs = english_verbs();
    for sentence in [
        "loaded",
        "pushed it",
        "committed as 7256f6",
        // The perfect auxiliaries are deliberately NOT in the list: "I've
        // pushed it" claims an action, so it has to keep firing.
        "i've pushed it",
        "we've deployed it",
    ] {
        assert!(
            has_past_tense_action_claim(sentence, &verbs),
            "a completion claim with no tool call went undetected: {sentence:?}"
        );
    }
}

#[test]
fn the_logged_joke_reply_survives_the_zero_tool_detector() {
    // Verbatim shape from the 2026-10-18 log line, padded to the detector's
    // length floor so the claim branch actually runs.
    let reply = "Loud and clear. I see the tag. Well, everything I'm tagged in, anyway. 🦀";
    assert!(
        !has_phantom_tool_intent_no_tools(reply),
        "the reply is a joke that claims no work; it must reach the chat"
    );
}

#[test]
fn ordinary_prose_with_contractions_is_not_replaced() {
    for reply in [
        "It's loaded from the workspace config at boot, nothing to do here.",
        "They're installed already and the plugin list is empty.",
    ] {
        assert!(
            !has_phantom_tool_intent_no_tools(reply),
            "ordinary prose read as a phantom claim: {reply:?}"
        );
    }
}

#[test]
fn a_claim_after_a_contraction_sentence_still_fires() {
    // Contractions must not disable the detector for the whole reply: a
    // passive sentence followed by a real claim is still a phantom.
    let reply = "It's loaded. Pushed the branch and closed the issue, all green.";
    assert!(
        has_phantom_tool_intent_no_tools(reply),
        "a real completion claim hid behind a passive sentence"
    );
}
