//! Phantom-tool-call detection.
//!
//! Catches assistant text that narrates actions ("Let me check…", "I'll
//! update…", "Pushed.") without emitting any actual tool calls. Two
//! detectors:
//!
//! * `has_phantom_tool_intent_no_tools` — relaxed gate, used when the
//!   iteration already produced zero tool uses. Bare intent phrases or
//!   short past-tense terminal claims are sufficient.
//! * `has_phantom_tool_intent` — strict gate for the general path; needs
//!   either standalone strong signals (multi-step plans, completion
//!   claims, gerund drops) or an intent phrase + file-path corroboration.
//!
//! All language-dependent data (intent phrases, action verbs, regex
//! patterns) lives in `phantom_lang/` TOML files, loaded at compile time.
//! Language detection is automatic via character-set heuristics.

use regex::Regex;
use std::sync::LazyLock;

use super::phantom_lang;

/// Relaxed phantom detection used when the caller already knows the
/// model emitted **zero tool_use blocks** this iteration. In that case
/// any bare intent phrase is phantom — no path or extension
/// corroboration required, because the tool count already proves
/// nothing happened.
///
/// Structured answers are exempt. Commit-log tables, code blocks, and
/// long bulleted lists inevitably contain intent-phrase substrings
/// (e.g. a commit message literally titled
/// `"fix(heal): phantom detector lets 'Let me check...' loops slide"`
/// — seen in logs 2026-04-17 03:38:37 — triggered this detector on
/// itself). A legitimate answer rendered as a table is NEVER a phantom,
/// even if its content happens to quote a phrase we watch for.
pub fn has_phantom_tool_intent_no_tools(text: &str) -> bool {
    // <<react:🔥>> and sibling inline directives prefix the narration and
    // broke every ^-anchored pattern (#464: "<<react:🔥>> Pushing the 3
    // commits now." sailed through). Strip them before any matching.
    let cleaned = strip_inline_directives(text);
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lang = phantom_lang::detect_language(trimmed);
    let too_short_for_phrases = trimmed.len() < 20;

    // Both ENDS, not just the lead. Every rule used to match against the lead
    // alone, which is the first 7 lines or up to the first list item. That held
    // while a turn opened with its intent ("Let me read the config."), and
    // stopped holding once reasoning began occupying the lead: the announcement
    // slid past the window and nothing was examined but deliberation. An
    // announcement sits at one end of a turn or the other, never buried
    // mid-paragraph, so the tail closes the gap without widening the middle
    // where discussing an action would false-positive (#783).
    for window in [prose_lead_in(trimmed), prose_tail(trimmed)] {
        if window.is_empty() {
            continue;
        }
        // Brief present-continuous work announcements ("Running checks now.",
        // "Checking the logs…") are phantom on their own — the model says it's
        // acting but emitted no tool call. At 19 bytes "Running checks now."
        // falls under the length floor below, so check this before it.
        if matches_work_announcement(window) {
            return true;
        }
        // Leading-imminence announcement ("... Now downloading the fonts.") —
        // the work-announcement regex needs a trailing marker and misses it.
        if matches_now_gerund(window) {
            return true;
        }
        // Sequenced plan announcement ("Setting up the plan, then mapping
        // every call site before touching anything.") — no imminence marker
        // at either end, so neither pattern above sees it (#1261).
        if matches_plan_announcement(window) {
            return true;
        }
        // Marker-less bare-participle announcement carrying its own object
        // pronoun ("Reading them, with mtimes so I know each postdates the
        // tree it claims to cover.") — no now / … / : and no then / before,
        // so all three arms above miss it (#1694).
        if matches_participle_object(window) {
            return true;
        }
        if too_short_for_phrases {
            continue;
        }
        let lower = window.to_lowercase();
        if has_intent_phrase(&lower) {
            return true;
        }
        // Past-tense completion claims stay gated to the detected language:
        // action_verbs are short single words with real cross-language
        // collision risk, unlike the multi-word intent phrases above.
        if has_past_tense_action_claim(&lower, &lang.action_verbs) {
            return true;
        }
    }
    false
}

/// The turn's closing prose, mirroring [`prose_lead_in`] from the other end.
///
/// Walks backwards over at most [`TAIL_LINES`] lines and stops at the first
/// structural one, so a table or code block at the end of an answer is not
/// mistaken for the model's parting announcement.
fn prose_tail(text: &str) -> &str {
    const TAIL_LINES: usize = 5;
    let lines: Vec<&str> = text.lines().collect();
    let first_kept = lines
        .iter()
        .enumerate()
        .rev()
        .take(TAIL_LINES)
        .take_while(|(_, line)| !is_structural_line(line))
        .map(|(idx, _)| idx)
        .last();
    let Some(first_kept) = first_kept else {
        return "";
    };
    let offset: usize = lines[..first_kept].iter().map(|l| l.len() + 1).sum();
    text[offset.min(text.len())..].trim()
}

/// Detects short past-tense completion claims like `"Pushed."`, `"Deployed."`,
/// `"Migration created."` — sentences that announce an action's done without
/// having executed any tool. Only used in the zero-tool-call path; loose
/// matching elsewhere would false-positive on conversational recaps.
pub(crate) fn has_past_tense_action_claim(lower: &str, action_verbs: &[String]) -> bool {
    for raw_sentence in lower.split(['.', '\n', '!']) {
        // Models type the curly apostrophe as often as the straight one, and
        // a contraction spelled `it’s` has to reach the guard spelled the way
        // the list below holds it. Normalise before the word split.
        let normalized = raw_sentence.replace('\u{2019}', "'");
        let s = normalized.trim();
        if s.is_empty() || s.len() > 80 {
            continue;
        }
        let words: Vec<&str> = s
            .split_whitespace()
            .take(4)
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
            .collect();
        for verb in action_verbs {
            if let Some(pos) = words.iter().position(|w| w == verb) {
                // Passive/copular description ("the config is loaded from
                // disk") is prose about state, not a completion claim —
                // only an ACTIVE claim ("Loaded.", "Both loaded properly")
                // counts. Guard on the auxiliary right before the verb.
                //
                // Contractions are auxiliaries too, and leaving them out made
                // ordinary prose read as a completed action: "everything i'm
                // tagged in" matched `tagged`, the phantom gate replaced a
                // real answer with its canned note, and the reply never
                // reached the chat (#2110). The word trim above only strips
                // the ENDS of a word, so an internal apostrophe survives and
                // the literal contractions are what the list needs; the
                // stripped spellings are listed as well because a model drops
                // the apostrophe often enough to matter. Negated copulas are
                // the same shape ("it isn't loaded yet") and are listed for
                // the same reason.
                //
                // The perfect auxiliaries (`i've`, `we've`, `you've`,
                // `they've`) are deliberately absent: they precede an ACTIVE
                // claim ("I've pushed it"), which is exactly what this guard
                // exists to catch.
                const AUX: &[&str] = &[
                    "is", "are", "was", "were", "been", "be", "being", "gets", "get", "got", "i'm",
                    "im", "am", "we're", "you're", "they're", "it's", "youre", "theyre", "its",
                    "he's", "she's", "hes", "shes", "that's", "there's", "what's", "thats",
                    "theres", "whats", "isn't", "aren't", "wasn't", "weren't", "isnt", "arent",
                    "wasnt", "werent", "está", "están", "foi", "é", "est", "sont",
                ];
                let passive = pos > 0 && AUX.contains(&words[pos - 1]);
                if !passive {
                    return true;
                }
            }
        }
    }
    false
}

/// Does the text contain any investigative/intent phrases?
/// Used by the phantom tool-call detector to identify when the model is
/// narrating an action it should be executing via tools.
pub fn has_investigative_intent(text: &str) -> bool {
    let lower = text.to_lowercase();
    lang_intent_match_any(&lower)
}

/// Forward-looking intent detector for the post-success path.
///
/// Behaves like `has_phantom_tool_intent_no_tools` but DROPS the
/// past-tense completion-claim branch. Used as the eligibility gate
/// for phantom self-heal AFTER a turn has already produced at least
/// one successful tool call: at that point past-tense summaries
/// (`Pushed.`, `Committed.`, `On main.`) are legitimate completion
/// acks and must not re-fire the detector — that's the whole reason
/// the post-success exemption exists. But FORWARD-looking intent
/// (`Let me dig into …`, `I'll check the …`, `Let me read the …`,
/// `need to update the …`) signals more tool calls promised and
/// dropped, which IS phantom regardless of how many tools already
/// ran this turn.
///
/// Logs 2026-06-03: a turn that ran one `git branch --show-current`
/// tool call then emitted "Good, on main. Let me dig into the delete
/// invitation endpoint, the email send path, and the invite flow to
/// find the bugs." silently ended without ever dispatching the three
/// promised investigations because the original exemption gate
/// (`phantom_eligible = tools_completed == 0`) disabled phantom
/// detection entirely for the post-tool-call portion of the turn.
///
/// Uses `prose_lead_in` so structural content (tables, code blocks,
/// bullet lists) past the lead-in doesn't contribute matches —
/// matches the host detector's own filter and keeps commit-message
/// tables from re-triggering the original false positive that
/// `e843f405` fixed.
pub fn has_forward_intent_post_success(text: &str) -> bool {
    let cleaned = strip_inline_directives(text);
    let trimmed = cleaned.trim();
    if trimmed.len() < 20 {
        return false;
    }
    // A reply that ends by asking the user something has handed the turn back:
    // the agent is waiting for the answer, so 'I'll ask what you do' or 'I'll
    // walk you through it' is conversation, not a promised action (#2083). An
    // explicit work announcement in the same reply still counts.
    if prose_tail(trimmed).contains('?')
        && !matches_work_announcement(trimmed)
        && !matches_now_gerund(trimmed)
    {
        return false;
    }
    // Both ends, for the same reason the zero-tool detector reads both: a
    // promise made AFTER the tool results ("...no errors from our changes.
    // Let me run tests and check the diff.") sits in the tail, and a
    // lead-only window never saw it, so the turn closed on an undelivered
    // promise and was logged as a pure completion acknowledgement.
    for window in [prose_lead_in(trimmed), prose_tail(trimmed)] {
        if window.is_empty() {
            continue;
        }
        let lower = window.to_lowercase();
        // A present-continuous work announcement ("Pushing the 3 commits
        // now.") is inherently FORWARD-looking: it can never be a completion
        // ack, no matter how many tools already ran this turn. The gate used
        // to check intent phrases only, so announcements after a successful
        // tool call closed turns as fake completions (#464).
        // `matches_now_gerund` adds the leading-imminence form ("... Now
        // downloading the fonts.") the trailing-marker regex misses.
        if has_intent_phrase(&lower)
            || matches_work_announcement(window)
            || matches_now_gerund(window)
        {
            return true;
        }
    }
    false
}

/// Remove inline channel directives (`<<react:🔥>>`, `<<IMG:path>>`, …)
/// so anchored phantom patterns see the narration, not the marker (#464).
///
/// Shared with the announcement guard (#2129): a directive is an instruction
/// to the channel layer, never prose, so it must not reach a text-similarity
/// check either.
pub(crate) fn strip_inline_directives(text: &str) -> String {
    static DIRECTIVE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"<<[^<>\n]{1,120}>>").expect("directive regex"));
    DIRECTIVE_RE.replace_all(text, "").into_owned()
}

/// Language-agnostic phantom tell (#463): the text names a REAL registered
/// tool while the turn executed zero tool calls. Models hallucinate tool
/// usage by naming the tool ("loaded via load_brain_file"), in ANY language,
/// so this catches narration the phrase lists cannot.
///
/// A multi-word (underscore) name is unmistakable and counts wherever it
/// appears. A BARE name — `plan`, `bash`, `grep`, `ls` — is an ordinary word
/// and used to be skipped outright, which cost a turn its self-heal: asked to
/// use the plan feature, the model answered "Setting up the plan, …", called
/// nothing, and the one check written for exactly that claim had excluded the
/// name for being a single word (#1262). A bare name now counts when the text
/// puts it in a tool-use frame (see [`bare_name_in_tool_context`]), so a claim
/// of usage is caught while an instruction addressed to the user ("run it in
/// bash and plan accordingly") stays untouched.
pub fn mentions_registered_tool(text: &str, tool_names: &[String]) -> bool {
    let lower = text.to_lowercase();
    for name in tool_names {
        let bare = !name.contains('_');
        let mut from = 0;
        while let Some(rel) = lower[from..].find(name.as_str()) {
            let start = from + rel;
            let end = start + name.len();
            let before_ok = start == 0
                || !lower[..start]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_');
            let after_ok = end == lower.len()
                || !lower[end..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_');
            if before_ok && after_ok && (!bare || bare_name_in_tool_context(&lower, start, end)) {
                return true;
            }
            from = end;
        }
    }
    false
}

/// Does the text around a BARE tool name claim the model USED it (#1262)?
///
/// Naming `plan` proves nothing; being the object of the model's own action
/// does. Four frames count, and nothing else:
///
/// * a marker immediately before it, across at most one determiner —
///   "setting up the plan", "running bash", "usando a ferramenta"
/// * a tool noun immediately after it — "the plan tool"
/// * backticked — "`plan`" is a name, not a word
/// * slash-prefixed — "/plan" is an invocation
///
/// The one-determiner limit is what separates a claim from an instruction:
/// "run it in bash" puts `it in` between the marker and the name, so the name
/// is not what the marker acts on and the text is left alone. Markers,
/// determiners and tool nouns are read across `all_langs()`, never from a
/// detected language: a model narrating in French inside an English session
/// is the same phantom.
fn bare_name_in_tool_context(lower: &str, start: usize, end: usize) -> bool {
    let before = &lower[..start];
    // "`plan`" / "/plan" — the name is written as a name.
    if matches!(before.chars().next_back(), Some('`') | Some('/')) {
        return true;
    }

    let after = lower[end..].trim_start();
    let next_word: String = after.chars().take_while(|c| c.is_alphanumeric()).collect();
    let words: Vec<String> = before
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .rev()
        .take(3)
        .map(str::to_string)
        .collect();

    phantom_lang::all_langs().iter().any(|lang| {
        // "the plan tool"
        if !next_word.is_empty() && lang.tool_nouns.iter().any(|n| n == &next_word) {
            return true;
        }
        let is_marker = |single: Option<&String>, pair_tail: Option<&String>| -> bool {
            let Some(w) = single else { return false };
            if lang.tool_use_markers.iter().any(|m| m == w) {
                return true;
            }
            // Two-word markers ("setting up", "mise en place" reduced to its
            // two-word head) are stored whole, so rebuild the pair.
            pair_tail.is_some_and(|prev| {
                let pair = format!("{prev} {w}");
                lang.tool_use_markers.iter().any(|m| m == &pair)
            })
        };
        // Directly before the name...
        if is_marker(words.first(), words.get(1)) {
            return true;
        }
        // ...or one determiner before it.
        words
            .first()
            .is_some_and(|w| lang.tool_name_determiners.iter().any(|d| d == w))
            && is_marker(words.get(1), words.get(2))
    })
}

/// High-stakes side-effect categories whose truthful use REQUIRES a tool call:
/// a git push/tag, a release, a version bump, a CHANGELOG/file write, an
/// external post. Each inner slice is ONE category; matching any of its phrases
/// counts that category once. Phrases are completion-shaped ("pushed to origin",
/// "created and pushed"), NOT intent ("let me push") — intent is already caught
/// by the forward-looking detectors.
///
/// Unlike the prose/lead-in detectors this scans the FULL lowercased text, so a
/// claim buried in a markdown table cell (the exact shape that slipped a
/// fabricated "shipped" scoreboard past every other check, #680) is still seen.
const SIDE_EFFECT_CATEGORIES: &[&[&str]] = &[
    // Ship / release
    &[
        "shipped",
        "released",
        "release complete",
        "release is shipped",
        "release was completed",
        "release is done",
    ],
    // Git push
    &[
        "pushed to origin",
        "pushed to remote",
        "tag pushed",
        "pushed (",
        "push status",
        "git push",
    ],
    // Tag
    &[
        "tag created",
        "created and pushed",
        "tagged v",
        "tag `v",
        "created` and pushed",
    ],
    // Version bump
    &[
        "bumped to",
        "version bump",
        "version bumped",
        "cargo.toml bumped",
        "bumped cargo",
    ],
    // CHANGELOG / release-notes write
    &[
        "changelog updated",
        "changelog entry",
        "entry inserted",
        "new entry inserted",
    ],
    // External post / publish
    &[
        "posts appended",
        "posts appended to",
        "release posts",
        "published to",
        "posted to",
    ],
];

/// Number of DISTINCT high-stakes side-effect categories the text claims.
/// Counting distinct categories (not raw phrase hits) means one repeated word
/// can't inflate the score, but a release scoreboard (ship + push + tag + bump
/// + changelog + post) scores high.
pub fn count_unbacked_side_effect_claims(text: &str) -> usize {
    let lower = text.to_lowercase();
    SIDE_EFFECT_CATEGORIES
        .iter()
        .filter(|category| category.iter().any(|phrase| lower.contains(phrase)))
        .count()
}

/// Verification-by-construction phantom tell (#680): a turn that ran ZERO tool
/// calls yet claims 2+ distinct high-stakes side-effects is fabricating a
/// completion report — those actions (push, tag, release, version bump, file
/// write, external post) cannot happen without a tool call, so with none
/// executed the claims are false by construction. The caller gates this on
/// `tool_calls_completed_this_turn == 0`, so a real release turn (which DID run
/// git/bash tools) is never flagged. The 2+ threshold keeps a lone "I pushed it
/// earlier" from tripping while catching the multi-claim scoreboard shape.
pub fn claims_unbacked_side_effects(text: &str) -> bool {
    count_unbacked_side_effect_claims(text) >= 2
}

/// Image-generation hallucination tell (#747): a turn asserts it produced or
/// delivered an image/media result but carries no `<<IMG:>>`/`<<VID:>>` marker,
/// so nothing was actually sent. `generate_image` delivers via those markers,
/// so a "there it is / generated it / the edited image" about a visual result
/// with no marker is fabricating. The caller gates this on
/// `tool_calls_completed_this_turn == 0`, so a real generation turn (which ran
/// the tool and emitted a marker) is never flagged. A marker anywhere in the
/// text short-circuits to `false` — a real deliverable rode along.
///
/// Multilingual (like the text/tool-call phantom tells): the delivery phrases
/// and visual context words come from every `phantom_lang` TOML and are scanned
/// as a union, so a Spanish/French/Portuguese/Indonesian/Russian narration is
/// caught too, immune to language mis-detection.
pub fn claims_unbacked_media_result(text: &str) -> bool {
    if text.contains("<<IMG:") || text.contains("<<VID:") {
        return false;
    }
    let lower = text.to_lowercase();
    let claims_delivery = phantom_lang::all_langs()
        .iter()
        .flat_map(|l| l.media_delivery_phrases.iter())
        .any(|p| lower.contains(p.as_str()));
    let is_visual = phantom_lang::all_langs()
        .iter()
        .flat_map(|l| l.media_context_words.iter())
        .any(|w| lower.contains(w.as_str()));
    claims_delivery && is_visual
}

/// Count line-start intent phrases — `Let me <verb>`, `I'll <verb>`,
/// `Let's <verb>`, or `Now let me / Now I'll <verb>`. A high count in a
/// single iteration's text means the model is spinning in place: emitting
/// back-to-back narration instead of calling a tool.
///
/// Only line-starts (after optional whitespace / list bullet) count. Intent
/// phrases embedded mid-paragraph are normal prose, not narration spam.
pub fn count_intent_line_starts(text: &str) -> usize {
    let lang = phantom_lang::detect_language(text);
    if lang.line_start_re.is_empty() {
        return 0;
    }
    let re = Regex::new(&lang.line_start_re).unwrap_or_else(|_| {
        Regex::new(r"$^").unwrap() // never matches
    });
    re.find_iter(text).count()
}

/// Threshold above which a repeated intent line is treated as "model stuck in
/// a phantom loop".
pub const STUCK_INTENT_LOOP_THRESHOLD: usize = 3;

/// The highest number of times the SAME intent line-start appears (normalized:
/// trimmed, lowercased, whitespace collapsed). A genuine phantom loop repeats
/// the *same* line ("Let me check the file." over and over); a legitimate
/// multi-step plan ("check X… then Y… actually Z first") has many DISTINCT
/// intent lines, which must NOT be mistaken for a loop.
pub fn max_repeated_intent_line(text: &str) -> usize {
    let lang = phantom_lang::detect_language(text);
    if lang.line_start_re.is_empty() {
        return 0;
    }
    let Ok(re) = Regex::new(&lang.line_start_re) else {
        return 0;
    };
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut max = 0;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Only lines that START with an intent phrase count.
        if re.find(trimmed).map(|m| m.start() == 0).unwrap_or(false) {
            let norm = trimmed
                .to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let c = counts.entry(norm).or_insert(0);
            *c += 1;
            max = max.max(*c);
        }
    }
    max
}

/// Whether the text shows a genuine phantom loop: the SAME intent line repeated
/// `STUCK_INTENT_LOOP_THRESHOLD`+ times. Distinct intent lines (a varied plan)
/// are NOT a loop — that false positive used to kill legitimate planful replies.
pub fn is_stuck_in_intent_loop(text: &str) -> bool {
    max_repeated_intent_line(text) >= STUCK_INTENT_LOOP_THRESHOLD
}

/// Highest match count of `regex_of(lang)` across ALL supported languages.
/// Scanning the union (not just `detect_language`'s pick) makes phantom
/// detection immune to language mis-detection — e.g. one accented word
/// ("Activité") flipping an English monologue to French and silencing the
/// English patterns (the Kimi K3 case). The per-language regexes carry
/// language-distinctive tokens, so the union is collision-free.
fn max_lang_regex_count(
    lower: &str,
    regex_of: impl Fn(&phantom_lang::LangConfig) -> &str,
) -> usize {
    phantom_lang::all_langs()
        .iter()
        .filter_map(|lang| {
            let re = regex_of(lang);
            (!re.is_empty())
                .then(|| Regex::new(re).ok().map(|r| r.find_iter(lower).count()))
                .flatten()
        })
        .max()
        .unwrap_or(0)
}

/// Whether `regex_of(lang)` matches `text` in ANY supported language.
fn any_lang_regex_match(text: &str, regex_of: impl Fn(&phantom_lang::LangConfig) -> &str) -> bool {
    phantom_lang::all_langs().iter().any(|lang| {
        let re = regex_of(lang);
        !re.is_empty() && Regex::new(re).map(|r| r.is_match(text)).unwrap_or(false)
    })
}

pub fn has_phantom_tool_intent(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.len() < 40 {
        return false;
    }
    let lower = trimmed.to_lowercase();

    // ── Strong signals (standalone — no corroboration needed) ─────────
    // All signals scan EVERY language (not just detect_language's pick), so a
    // mis-detected language can't disable them (#602 follow-up: one accented
    // word mis-routed an English monologue to French and slipped the detector).

    // 2+ imperative "Now <verb>" / "Let me <verb>" at line start = multi-step plan
    if max_lang_regex_count(&lower, |l| &l.now_imperative_re) >= 2 {
        return true;
    }

    // 2+ numbered steps with action verbs = narrated plan
    if max_lang_regex_count(&lower, |l| &l.numbered_steps_re) >= 2 {
        return true;
    }

    // 2+ past-tense standalone sentences = phantom completion narration
    if max_lang_regex_count(&lower, |l| &l.past_tense_standalone_re) >= 2 {
        return true;
    }

    // ── Completion claims (standalone) ────────────────────────────────
    if phantom_lang::all_langs()
        .iter()
        .any(|l| lang_completion_match(&lower, &l.completion_claims))
    {
        return true;
    }

    // ── Now + gerund status-then-action drops (standalone) ─────────────
    if any_lang_regex_match(trimmed, |l| &l.gerund_re) {
        return true;
    }

    // ── Trailing-colon intent ─────────────────────────────────────────
    if any_lang_regex_match(trimmed, |l| &l.trailing_colon_re) {
        return true;
    }

    // ── Weak signals (need corroboration) ─────────────────────────────
    // `lang_intent_match_any` already scans all languages.
    if lang_intent_match_any(&lower) {
        // Corroborate with file paths, extensions, or backtick code refs.
        // These are language-agnostic, so any language's regex works.
        let path = any_lang_regex_match(trimmed, |l| &l.path_re)
            || any_lang_regex_match(trimmed, |l| &l.ext_re)
            || any_lang_regex_match(trimmed, |l| &l.backtick_code_re);
        if path {
            return true;
        }
    }

    false
}

// ── Language-agnostic helpers ──────────────────────────────────────────

/// Check if `lower` contains any phrase from the list (case-insensitive).
fn lang_intent_match(lower: &str, phrases: &[String]) -> bool {
    phrases.iter().any(|p| lower.contains(p.as_str()))
}

/// Check if `lower` matches an intent phrase in ANY supported language.
///
/// `detect_language` only routes Cyrillic and accented-Latin text
/// reliably, so accent-free non-English narration (e.g.
/// `"Voy a usar write_file…"`) falls through to English and would slip
/// past a detected-language-only check. Intent phrases are multi-word and
/// carry language-distinctive tokens, so the cross-language union is
/// collision-free — a Spanish phrase can't match English prose and vice
/// versa. 2026-06-12.
/// Does `lower` carry a first-person intent whose VERB was never enumerated?
///
/// [`lang_intent_match_any`] is an allowlist: it holds `let me run` and
/// `let me check` but not `let me execute`, so a turn that ended on "Let me
/// execute the full flow now" with no tool call was ruled a clean completion.
/// Enumerating verbs cannot converge, so this matches the construction and
/// reads the verb, rejecting only the ones that promise speech instead of
/// work (`let me know`).
fn matches_generic_intent(lower: &str) -> bool {
    phantom_lang::all_langs().iter().any(|lang| {
        if lang.generic_intent_re.is_empty() {
            return false;
        }
        let Ok(re) = Regex::new(&lang.generic_intent_re) else {
            return false;
        };
        re.captures_iter(lower).any(|caps| {
            caps.get(1).is_some_and(|verb| {
                let verb = verb.as_str();
                !lang
                    .intent_verb_exclusions
                    .iter()
                    .any(|excluded| excluded == verb)
            })
        })
    })
}

/// Intent in any supported language, whether or not its verb was enumerated.
fn has_intent_phrase(lower: &str) -> bool {
    lang_intent_match_any(lower) || matches_generic_intent(lower)
}

fn lang_intent_match_any(lower: &str) -> bool {
    phantom_lang::all_langs()
        .iter()
        .any(|lang| lang_intent_match(lower, &lang.intent_phrases))
}

/// Does `lead` OPEN with a present-continuous work announcement in ANY
/// supported language ("Running checks now.", "Verificando ahora…")? Scanned
/// across all languages like the intent phrases. Each regex is anchored to the
/// message start and requires the announcement's imminence marker (now / ahora
/// / agora / maintenant / сейчас / … / trailing :) at a sentence boundary, so
/// the model leading with the announcement and then continuing ("Running fmt,
/// clippy, tests now. Then fetching…") still matches, while an ordinary
/// sentence that merely opens with a gerund ("Reading the file is
/// straightforward.") or uses "now" as an adverb ("Running it now takes a
/// minute.") does not.
pub(crate) fn matches_work_announcement(lead: &str) -> bool {
    let lead = strip_inline_directives(lead);
    let lead = lead.trim();
    phantom_lang::all_langs().iter().any(|lang| {
        !lang.work_announcement_re.is_empty()
            && Regex::new(&lang.work_announcement_re)
                .map(|re| announcement_matches_anywhere(&re, lead))
                .unwrap_or(false)
    })
}

/// Candidate clause starts inside `lead`: position 0, every position that
/// begins a sentence or follows a clause introducer, and (in the caller) the
/// short ", " lead-in. Shared by the boolean arms and by
/// `matches_participle_object`, which needs the same tolerance but must see
/// the CAPTURE rather than just a match.
///
/// Sentence enders, plus the clause introducers that carry an announcement
/// just as often (#1192). "That's ten issues — fetching all specs fresh…:"
/// placed both gerunds after an em dash, so neither was ever offered to the
/// ^-anchored regex and a zero-tool turn shipped as a finished answer. The
/// comma-window fallback in the callers was the earlier acknowledgement that
/// a lead clause can precede the announcement; it only ever covered ", "
/// inside 48 bytes.
///
/// Purely additive: every start the old scan produced is still produced, so
/// nothing that matched before stops matching.
fn announcement_clause_starts(lead: &str) -> Vec<usize> {
    let mut starts: Vec<usize> = vec![0];
    let mut after_ender = false;
    for (idx, ch) in lead.char_indices() {
        if after_ender && !ch.is_whitespace() {
            starts.push(idx);
            after_ender = false;
        }
        if matches!(ch, '.' | '!' | '?' | '\n' | '…' | '—' | '–' | ':' | ';') {
            after_ender = true;
        }
    }
    starts
}

/// Every slice of `lead` an anchored announcement regex should be offered:
/// each clause start from `announcement_clause_starts`, plus the short ", "
/// lead-in window ("Internet's back, pushing now.", #464). Order is per start,
/// full suffix first. Purely additive — the same slices, in the same order,
/// that the scan inside `announcement_matches_anywhere` used to visit.
fn announcement_candidate_slices(lead: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for &start in &announcement_clause_starts(lead) {
        let suffix = &lead[start..];
        out.push(suffix);
        // Short lead clause before the announcement ("internet's back, ").
        let window_end = suffix
            .char_indices()
            .take_while(|(i, _)| *i <= 48)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        if let Some(comma) = suffix[..window_end].find(", ") {
            out.push(&suffix[comma + 2..]);
        }
    }
    out
}

/// Run an anchored announcement regex against every sentence start in the
/// lead, tolerating a short lead clause before the gerund. The live escapes
/// (#464) were all anchor evasions: "Internet's back, pushing now." (clause
/// prefix), "Apologies Adolfo, you're right. Pushing now." (sentence
/// prefix). Suffix slices keep the terminal imminence markers intact.
fn announcement_matches_anywhere(re: &Regex, lead: &str) -> bool {
    announcement_candidate_slices(lead)
        .iter()
        .any(|slice| re.is_match(slice))
}

/// Does `text` contain a "Now &lt;gerund&gt;" work announcement at a sentence
/// start in ANY supported language (`gerund_re`)? This catches the
/// leading-imminence form ("Issue #22 filed. Now downloading the fonts.") that
/// `matches_work_announcement` misses — that regex requires a TRAILING imminence
/// marker (now / … / :), so an announcement that leads with "Now" and ends on a
/// plain period slips it. Scanned across all languages like the other tells.
pub(crate) fn matches_now_gerund(text: &str) -> bool {
    let text = strip_inline_directives(text);
    phantom_lang::all_langs().iter().any(|lang| {
        !lang.gerund_re.is_empty()
            && Regex::new(&lang.gerund_re)
                .map(|re| re.is_match(&text))
                .unwrap_or(false)
    })
}

/// Does `text` announce the PREPARATION for work, sequenced rather than
/// imminent, in ANY supported language (`plan_announcement_re`)?
///
/// "Setting up the plan, then mapping every call site before touching
/// anything." is a promise of steps, and in a turn that emitted zero tool
/// calls it is a promise already broken. It escaped every existing tell on
/// two counts: `work_announcement_re` enumerates execution verbs and never
/// carried the preparation ones (mapping, planning, drafting, listing), and
/// both it and `gerund_re` key on an imminence marker (trailing now / … / :,
/// or a leading "Now") that a sequenced clause does not carry.
///
/// Zero-tool path only. After a tool has run, "Updating the config fixed it,
/// then the build passed" is the same shape as a legitimate recap, and the
/// ambiguity that the zero-tool count resolves is no longer there.
pub(crate) fn matches_plan_announcement(text: &str) -> bool {
    let text = strip_inline_directives(text);
    let text = text.trim();
    phantom_lang::all_langs().iter().any(|lang| {
        !lang.plan_announcement_re.is_empty()
            && Regex::new(&lang.plan_announcement_re)
                .map(|re| re.is_match(text))
                .unwrap_or(false)
    })
}

/// Does `text` announce work with a bare participle carrying its OWN object
/// pronoun, with no imminence marker and no sequencing?
///
/// "Reading them, with mtimes so I know each postdates the tree it claims to
/// cover." — the #1694 incident, delivered as a finished answer by a turn that
/// called nothing. Every existing arm needs a token this clause does not carry:
/// `work_announcement_re` and `gerund_re` a trailing now / … / :,
/// `plan_announcement_re` a then / before / after.
///
/// The regex alone cannot make the call, because the same construction is also
/// an ordinary subject: "Reading them is straightforward." and "Getting them
/// took a while." Only the second lacks a copula, so NO copula list separates
/// the two — what separates them is the word class the pronoun is followed by.
/// An announcement is followed by nothing, by a clause introducer, or by a
/// closed-class modifier; a statement is followed by the finite verb its
/// participle subject governs. Hence `announcement_tail_words`, a closed class
/// (prepositions, conjunctions, numerals, ordinals, modals, fixed adverbials)
/// rather than a verb list, which is the open class that never converges
/// (#1122).
///
/// Zero-tool path only, for the reason `matches_plan_announcement` gives: after
/// a real call the shape is a legitimate recap (#1506/#1172).
pub(crate) fn matches_participle_object(text: &str) -> bool {
    let text = strip_inline_directives(text);
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    phantom_lang::all_langs().iter().any(|lang| {
        !lang.participle_object_re.is_empty()
            && Regex::new(&lang.participle_object_re)
                .map(|re| {
                    announcement_candidate_slices(text).iter().any(|slice| {
                        re.captures(slice).is_some_and(|caps| {
                            tail_reads_as_announcement(
                                caps.get(1).map_or("", |m| m.as_str()),
                                &lang.announcement_tail_words,
                            )
                        })
                    })
                })
                .unwrap_or(false)
    })
}

/// Whether the clause tail after the pronoun reads as an announcement.
///
/// Empty (the announcement is the last thing said), or opening on a clause
/// introducer, or headed by a closed-class word. Everything else is the
/// predicate of a participial subject.
fn tail_reads_as_announcement(tail: &str, closed: &[String]) -> bool {
    let tail = tail.trim_start();
    if tail.is_empty() {
        return true;
    }
    if matches!(tail.chars().next(), Some(',' | ':' | ';' | '…' | '(')) {
        return true;
    }
    // Accented letters are word characters here, so a plain `split` on
    // non-letters would chop "difícil" and hand the predicate test a fragment.
    let head: String = tail
        .chars()
        .take_while(|c| c.is_alphabetic() || c.is_ascii_digit() || *c == '\'')
        .collect();
    if head.is_empty() {
        return true;
    }
    closed.iter().any(|w| w == &head.to_lowercase())
}

/// Check if `lower` contains any completion claim.
fn lang_completion_match(lower: &str, claims: &[String]) -> bool {
    claims.iter().any(|c| lower.contains(c.as_str()))
}

/// Slice of the text before the first code fence, markdown table row,
/// or list-item line — the "narration" portion.
/// Whether `line` is markup rather than prose: a fence, table row, bullet, or
/// numbered item. Shared so both ends of a turn agree on where prose stops.
fn is_structural_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("```")
        || (t.starts_with('|') && t.contains('|'))
        || t.starts_with("- ")
        || t.starts_with("* ")
        || t.starts_with("• ")
        || (t.chars().next().is_some_and(|c| c.is_ascii_digit()) && t.contains(". "))
}

fn prose_lead_in(text: &str) -> &str {
    let mut byte_offset: usize = 0;
    for (idx, line) in text.lines().enumerate() {
        if is_structural_line(line) {
            return text[..byte_offset].trim_end();
        }
        if idx >= 6 {
            break;
        }
        byte_offset += line.len() + 1;
    }
    text
}

/// Does the user message contain an analysis / data-interpretation verb?
///
/// Used to detect "the user asked me to AUDIT something" vs. "the user
/// asked me to COMMIT something" so the runtime can react when a turn
/// ends with `finish_reason: stop` and ZERO text after successful tool
/// calls. For side-effect tasks (commit / push / edit / deploy), the
/// tool call IS the deliverable — empty-text completion is fine. For
/// analysis tasks, the tool fetched data the user expected the model
/// to interpret — empty-text completion is a regression we shipped via
/// the `FINISHING A TURN` directive in commit e843f405.
///
/// Matches at a word boundary so prose like "you describe this
/// pattern" does NOT trip on "describe" inside another sentence. Only
/// the leading-imperative / question form counts.
///
/// Coverage is intentionally English-only for now. Spanish / Portuguese
/// / French / Russian variants follow the same shape; this MVP catches
/// the common case and can be expanded as patterns emerge in logs.
pub fn is_analysis_intent(text: &str) -> bool {
    let lower = text.to_lowercase();
    // Strip the channel prefix if present so `[Channel: Telegram ...]\n<msg>`
    // matches on `<msg>` content, not on the bracketed wrapper.
    let body = lower.rsplit('\n').next().unwrap_or(&lower);
    // Look at the first ~200 chars only — the verb is in the request,
    // not buried in a long quote.
    let head: String = body.chars().take(200).collect();
    // Phrase patterns to match. Each entry is matched as a contained
    // substring on the head — short verbs need leading whitespace or
    // start-of-string to avoid matching inside another word
    // ("examine" should not trigger on "exam"; "audit" must not
    // trigger on "auditorium" in a quoted URL).
    let leading_word = |w: &str| -> bool {
        // Match at start or after whitespace/punct, followed by space.
        // Cheap manual scan rather than a regex — keeps this hot path
        // allocation-free for the common no-match case.
        let needle = format!(" {w} ");
        if head.starts_with(&format!("{w} ")) {
            return true;
        }
        head.contains(&needle)
    };
    const ANALYSIS_VERBS: &[&str] = &[
        "audit",
        "review",
        "compare",
        "explain",
        "summarise",
        "summarize",
        "check",
        "describe",
        "analyse",
        "analyze",
        "find",
        "look up",
        "look at",
        "what does",
        "how does",
        "why does",
        "what is",
        "what are",
        "tell me",
        "show me",
        "investigate",
        "diagnose",
    ];
    // "report" deliberately omitted — too noun-ambiguous. "the report
    // says X" and "your report failed" would false-positive the
    // analysis-nudge while no analysis was requested. `report on X`
    // is rare enough that users who want it can rephrase as "explain
    // X" or "summarise X" without losing precision.
    ANALYSIS_VERBS.iter().any(|v| leading_word(v))
}

/// Bare terminal completion tokens. A response that is ONLY one of these
/// (after stripping directives, emoji, and punctuation) is a content-free
/// completion ack: fine as a TRUE terminal after real tool work, but a phantom
/// when the turn ran zero tools and the user asked for a deliverable.
const BARE_COMPLETION_PHRASES: &[&str] = &[
    "done",
    "all done",
    "ready",
    "all ready",
    "ready to go",
    "good to go",
    "finished",
    "all finished",
    "complete",
    "completed",
    "task complete",
    "task completed",
    "all set",
    "all good",
    "taken care of",
    "handled",
    "sorted",
    "there you go",
    "here you go",
];

/// Is `text` nothing but a bare completion word/phrase — no deliverable, no
/// substantive body?
///
/// Strips inline directives, then requires the whole remaining message
/// (lowercased, emoji/punctuation flattened to spaces, whitespace collapsed)
/// to equal one of `BARE_COMPLETION_PHRASES`. A code fence disqualifies it
/// outright (that carries the deliverable), as does any real length — a genuine
/// confirmation with specifics ("Committed as 7256f6…") is longer than any bare
/// ack. #680 follow-up: a lone "Done." is 5 bytes and slips every other
/// detector's 20/40-byte floor, so it needs an explicit, floor-free check.
pub fn is_bare_completion_only(text: &str) -> bool {
    let cleaned = strip_inline_directives(text);
    if cleaned.contains("```") {
        return false;
    }
    let trimmed = cleaned.trim();
    if trimmed.chars().count() > 48 {
        return false;
    }
    let normalized: String = trimmed
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    BARE_COMPLETION_PHRASES.contains(&normalized.as_str())
}

/// Did the user ask the model to PRODUCE an artifact — build/create/write/
/// generate code, a script, a component, a document?
///
/// Mirror of `is_analysis_intent`, used to gate the bare-completion phantom
/// check: a delivery request answered by a content-free "Done." with zero tool
/// calls produced nothing. Interrogative openers ("did you build it?") are
/// questions ABOUT work, not requests to produce it, so they never count — that
/// guard keeps a legitimate cross-turn ack from being flagged. English-only for
/// now, like `is_analysis_intent`.
pub fn is_delivery_intent(text: &str) -> bool {
    let lower = text.to_lowercase();
    // Drop a leading channel/context bracket ("[channel: telegram …]") so the
    // request itself is what we scan, not the wrapper.
    let body = lower.trim_start();
    let body = if body.starts_with('[') {
        body.split_once(']')
            .map(|x| x.1)
            .unwrap_or(body)
            .trim_start()
    } else {
        body
    };
    const QUESTION_OPENERS: &[&str] = &[
        "did ", "do ", "does ", "is ", "are ", "was ", "were ", "have ", "has ", "had ", "can ",
        "could ", "will ", "would ", "should ", "what ", "where ", "when ", "why ", "how ",
        "which ", "who ",
    ];
    if QUESTION_OPENERS.iter().any(|q| body.starts_with(q)) {
        return false;
    }
    // Build requests span multiple lines; scan the whole request but cap the
    // window so a long pasted blob can't dominate this hot path.
    let head: String = body.chars().take(1000).collect();
    let leading_word = |w: &str| -> bool {
        let needle = format!(" {w} ");
        head.starts_with(&format!("{w} ")) || head.contains(&needle)
    };
    const DELIVERY_VERBS: &[&str] = &[
        "create",
        "build",
        "generate",
        "write",
        "implement",
        "produce",
        "make",
        "draft",
        "compose",
        "design",
        "provide",
    ];
    DELIVERY_VERBS.iter().any(|v| leading_word(v))
}

/// Heuristic: does `text` look like it was truncated mid-sentence?
pub fn looks_truncated_mid_sentence(text: &str) -> bool {
    let trimmed = text.trim_end();
    if trimmed.chars().count() < 40 {
        return false;
    }
    // A response that ends INSIDE an unclosed code fence (odd count of
    // fence lines) is truncated regardless of its last character — no
    // complete markdown reply leaves a fence open (#36).
    let fence_lines = trimmed
        .lines()
        .filter(|l| l.trim_start().starts_with("```"))
        .count();
    if fence_lines % 2 == 1 {
        return true;
    }
    if trimmed.ends_with("```") {
        return false;
    }
    if trimmed.ends_with('|') {
        return false;
    }
    if ends_with_url(trimmed) {
        return false;
    }
    let last = match trimmed.chars().next_back() {
        Some(c) => c,
        None => return false,
    };
    if last.is_alphanumeric() {
        return true;
    }
    // Inline-code parity is a LAST-CHAR-AGNOSTIC structural signal: no
    // complete markdown reply leaves an inline code span open, so odd
    // backticks outside fences means the stream died mid-span whichever
    // char it died on. Old code only consulted parity when the last char
    // WAS the backtick (#36 shape); the 2026-09-26 incident (#1753) died
    // one char deeper on an opening quote — `... gated on `content.`
    // contains("` with a provider EndTurn lie — and shipped unmarked to
    // Telegram and TUI alike. This branch subsumes the #36 case.
    if backticks_outside_fences(trimmed) % 2 == 1 {
        return true;
    }
    matches!(
        last,
        ',' | ';' | ':' | '-' | '(' | '[' | '{' | '<' | '/' | '\\' | '&' | '@' | '#' | '—' | '–'
    )
}

/// Count backticks on lines outside fenced code blocks. Fence delimiter
/// lines themselves don't count — their backticks delimit blocks, they
/// are not inline code. Used for the inline-code parity check in
/// [`looks_truncated_mid_sentence`] (#36, generalized for #1753: last-char
/// agnostic, a dangling span is the signal, not the char it ends on).
fn backticks_outside_fences(text: &str) -> usize {
    let mut in_fence = false;
    let mut count = 0;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            count += line.bytes().filter(|&b| b == b'`').count();
        }
    }
    count
}

/// Detect whether `text` ends with a URL.
fn ends_with_url(text: &str) -> bool {
    let trimmed = text.trim_end();
    // `rfind` returns the first byte of the separator, so `i + 1` can land
    // inside it when the separator is multi-byte (U+202F, U+00A0, U+2009,
    // U+3000). Snapping forward keeps the slice on a char boundary; slicing
    // at `i + 1` panicked with "not a char boundary" before #2220.
    let boundary = trimmed
        .rfind(|c: char| c.is_whitespace() || matches!(c, '(' | '[' | '{' | '<' | '"' | '\''))
        .map(|i| trimmed.ceil_char_boundary(i + 1))
        .unwrap_or(0);
    let tail = &trimmed[boundary..];
    tail.contains("://")
}

/// Whether `text` presents literal command output that no tool produced this
/// turn.
///
/// Verb lists cannot catch this one. After a genuine grep, "Grepped and found
/// 34 hits" is an honest recap, and the sentence is identical when the grep
/// never ran — what differs is whether the CONTENT came from a tool. So this
/// checks content, not wording, and holds regardless of how many calls the
/// turn made.
///
/// The trigger is quoted evidence: grep-style `149:pub struct Tui {` lines and
/// `=== marker ===` blocks, the shapes a model uses when presenting tool
/// output. Prose and paraphrase carry neither, so ordinary answers are never
/// examined. At least [`MIN_EVIDENCE_LINES`] are required, so a passing mention
/// of a line number is not enough, and it only fires when a MAJORITY are absent
/// from every tool result — a partially-quoted real output stays clean.
///
/// This is the case where one trivial call bought immunity for a whole turn:
/// self-heal forced a tool, the model ran an `echo`, then reported the output
/// of two greps it never made (#785).
pub fn claims_unbacked_evidence(text: &str, tool_outputs: &[String]) -> bool {
    /// Below this, a numbered line is more likely a citation than a dump.
    const MIN_EVIDENCE_LINES: usize = 3;

    let evidence: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| is_evidence_line(l))
        .collect();
    if evidence.len() < MIN_EVIDENCE_LINES {
        return false;
    }
    let backed = evidence
        .iter()
        .filter(|line| tool_outputs.iter().any(|out| out.contains(**line)))
        .count();
    backed * 2 < evidence.len()
}

/// A line shaped like quoted tool output rather than prose.
///
/// Covers the three shapes fabrications have actually taken, not just the
/// first one seen: grep output, aligned key-value blocks, and column rows.
/// Built too narrowly the first time, it matched only grep lines and missed
/// two later fabrications that used the other two (#789).
fn is_evidence_line(line: &str) -> bool {
    // `149:pub struct Tui {` — grep -n, and the `wc -l` / `ls` numeric forms.
    let numbered = line.split_once(':').is_some_and(|(n, rest)| {
        !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) && !rest.trim().is_empty()
    });
    // `=== LINE COUNT ===`, the framing a model adds around a dump.
    let marker = line.len() > 6 && line.starts_with("===") && line.ends_with("===");
    // `Title  : Non-owner can no longer …` — an aligned key-value block. The
    // padding before the colon is what distinguishes it from ordinary prose
    // that happens to contain one.
    let aligned_kv = line.split_once(':').is_some_and(|(k, v)| {
        let key = k.trim_end();
        !key.is_empty()
            && k.len() > key.len()
            && key.len() <= 24
            && !key.contains(' ')
            && !v.trim().is_empty()
    });
    // `#776  TUI: pasted multi-line text …  bug, tui` — a column row: an id
    // token followed by run-together spacing.
    let column_row = line.starts_with('#')
        && line.contains("  ")
        && line[1..].chars().take_while(|c| c.is_ascii_digit()).count() >= 2;
    numbered || marker || aligned_kv || column_row
}

/// Commands the text claims to have ALREADY RUN that no tool call actually
/// contained this turn.
///
/// Every other detector here matches how a fabrication looks — its verbs, its
/// layout, where in the turn it sits — and a model can always look different.
/// This one does not infer: the loop knows exactly what it executed, so a
/// claim naming a specific command is checkable against fact.
///
/// Two conditions, both required, so a PROPOSED command stays untouched:
/// the command appears in backticks, and the surrounding sentence frames it as
/// already executed. "I could run `gh issue list`" is a suggestion; "Ran `gh
/// issue list` for real this turn" is a claim, and if no tool input contains
/// it, it is a false one (#789).
pub fn claims_uncalled_commands(text: &str, executed_inputs: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    // Framings are scanned across EVERY language, like the intent phrases, not
    // taken from the detected one. The command is language-neutral but the
    // claim around it is not, and detection is a character-set heuristic that
    // reads accented Latin as the wrong language — Portuguese and Spanish
    // claims slipped through when this trusted it. Multi-word framings carry
    // little cross-language collision risk, which is why the intent phrases
    // are scanned the same way.
    for sentence in sentences_outside_code_spans(text) {
        if !frames_as_executed_any(&sentence.to_lowercase()) {
            continue;
        }
        for cmd in backticked_commands(sentence) {
            // Match on the distinctive head (program + first argument) rather
            // than the whole span: a real call may add flags, redirects or a
            // pipeline the prose omits, and that is not a fabrication.
            let head: String = cmd.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
            if head.is_empty() {
                continue;
            }
            if !executed_inputs.iter().any(|inp| inp.contains(&head)) {
                out.push(cmd);
            }
        }
    }
    out
}

/// Facts a text-only iteration asserts that can be checked against the
/// conversation: git object ids and result tallies.
///
/// The post-success exemption lets a wrap-up through once the turn has real
/// work behind it, on the reasoning that `Pushed.` is an acknowledgement
/// rather than a claim. That holds for an ack and fails for a report: a
/// 4,664-character iteration full of invented counts and commit shas was
/// exempted as a "pure completion acknowledgement" because 24 tools had
/// succeeded earlier in the same turn (#1423).
///
/// Length cannot separate the two. Across 128 exempted iterations over three
/// days the median is 968 characters and p90 is 2,563, so most exempted text
/// is legitimately long, and bounding the exemption by size would re-break
/// the regression it was added for. What separates a report from a
/// fabrication is not its size but whether the facts in it exist.
///
/// Same principle as `claims_uncalled_commands` and `claims_unbacked_evidence`:
/// read fact, not wording (#1073). Deliberately narrow, because the shapes
/// those two recognize are exactly the ones a report-shaped fabrication
/// avoids: `claims_unbacked_evidence` needs three lines shaped like a grep
/// dump, an `===` frame, padded key-value or a column row, and a markdown
/// table with prose between its rows matches none of them.
pub fn asserted_facts(text: &str) -> Vec<String> {
    /// Bound the nudge and the scan; a pathological iteration states a
    /// handful of facts, not hundreds.
    const MAX_FACTS: usize = 20;
    let mut out = asserted_shas(text);
    out.extend(asserted_tallies(text));
    out.extend(asserted_diagnostics(text));
    out.sort();
    out.dedup();
    out.truncate(MAX_FACTS);
    out
}

/// Which of the asserted facts appear nowhere in the conversation's evidence.
pub fn unbacked_facts(facts: &[String], known: &str) -> Vec<String> {
    facts
        .iter()
        .filter(|fact| !fact_is_backed(fact, known))
        .cloned()
        .collect()
}

/// Which fact checks an iteration that ALSO carried tool calls failed (#1693).
///
/// Every detector in the phantom block sits behind `if tool_uses.is_empty()`, so
/// a fabrication glued to a legitimate tool call was never evaluated by any of
/// them — the gate at `tool_loop.rs:4823` skipped the whole 1341-line block on
/// the strength of one real call. The checks here are content-vs-evidence: they
/// compare what the text asserts against what the turn actually produced, and
/// that comparison does not depend on the iteration having zero tool calls.
///
/// The shape-based tells (`work_announcement_re`, `gerund_re`,
/// `plan_announcement_re`) deliberately stay zero-tool-only. After a real call
/// "Running fmt, then clippy" is a legitimate recap of work in progress, which
/// is exactly what the post-success exemption exists to protect (#1506, #1172).
#[derive(Debug)]
pub(crate) struct MixedFactViolation {
    /// Branch names for the WARN and telemetry, in detection order.
    pub(crate) branches: Vec<&'static str>,
    /// Commands named as already run that no call contained (#789).
    pub(crate) uncalled_commands: Vec<String>,
    /// Shas, tallies and diagnostic codes absent from the evidence (#1423, #1693).
    pub(crate) unbacked_facts: Vec<String>,
    /// Claims about this session's own record that the record contradicts
    /// (#2122): a command denied but present in the calls, or an identifier
    /// reported as produced with nothing in the evidence to show for it.
    pub(crate) unbacked_actions: Vec<String>,
    /// Whether the text is a #1506 structured completion report. Carried on the
    /// verdict so the call site can honour the directive without holding the
    /// iteration text alive until the tool results land.
    pub(crate) structured_report: bool,
}

/// Run the fact-based checks over an iteration that carried tool calls.
///
/// `executed_inputs` must include this iteration's OWN calls: the text "Running
/// `gh pr list` now" accompanied by a `gh pr list` call is the call doing the
/// thing, and scoring it against prior inputs alone would flag the work it is
/// about to do. `tool_outputs` and `evidence` are deliberately the PRIOR
/// iterations only — the in-flight calls have produced nothing yet, so they
/// cannot vouch for a claim stated in the past tense. That asymmetry is the
/// whole point: a gate claimed to have already died is checkable against what
/// ran before, not against what is about to run.
///
/// Returns `None` when nothing is asserted that the evidence cannot support.
///
/// Test-only: the live loop always calls `_with_history`, since it has the
/// session's earlier calls at hand. Kept as the plain four-argument form so a
/// test can state "no history" without spelling an empty slice.
#[cfg(test)]
pub(crate) fn mixed_iteration_facts(
    text: &str,
    executed_inputs: &[String],
    tool_outputs: &[String],
    evidence: &str,
) -> Option<MixedFactViolation> {
    mixed_iteration_facts_with_history(text, executed_inputs, tool_outputs, evidence, &[])
}

/// Same check, with the session's earlier calls available to the denial check.
///
/// The history is deliberately NOT merged into `executed_inputs`. Those two
/// questions look alike and are not: "you claimed this ran and it did not" is
/// about the current turn, and clearing it with a call from an hour ago would
/// hollow the check out. A denial is the opposite shape, a claim about the whole
/// session, so it is the one branch that reads history.
pub(crate) fn mixed_iteration_facts_with_history(
    text: &str,
    executed_inputs: &[String],
    tool_outputs: &[String],
    evidence: &str,
    session_inputs: &[String],
) -> Option<MixedFactViolation> {
    if text.trim().is_empty() {
        return None;
    }
    let uncalled_commands = claims_uncalled_commands(text, executed_inputs);
    let facts = unbacked_facts(&asserted_facts(text), evidence);
    let unbacked_evidence = claims_unbacked_evidence(text, tool_outputs);
    let denials = asserted_history_denials(text);
    let mut unbacked_actions = denials_contradicted(&denials, executed_inputs);
    for contradicted in denials_contradicted(&denials, session_inputs) {
        if !unbacked_actions.contains(&contradicted) {
            unbacked_actions.push(contradicted);
        }
    }
    for identifier in asserted_produced_identifiers(text) {
        if !identifier_is_backed(&identifier, evidence) {
            unbacked_actions.push(format!(
                "{identifier} (reported as produced, absent from the evidence)"
            ));
        }
    }

    let mut branches = Vec::new();
    if !uncalled_commands.is_empty() {
        branches.push("mixed_uncalled_commands");
    }
    if !facts.is_empty() {
        branches.push("mixed_unbacked_facts");
    }
    if !unbacked_actions.is_empty() {
        branches.push("mixed_unbacked_actions");
    }
    if unbacked_evidence {
        branches.push("mixed_unbacked_evidence");
    }
    if branches.is_empty() {
        return None;
    }
    Some(MixedFactViolation {
        branches,
        uncalled_commands,
        unbacked_facts: facts,
        unbacked_actions,
        structured_report: is_structured_report(text),
    })
}

/// Git object ids the text states.
///
/// A maximal run of hex characters, 7 to 40 long, standing alone. Three
/// filters keep this off ordinary words, each trading a small miss rate for a
/// large false-positive reduction:
///
/// * the run must not touch a letter, digit, underscore or hyphen, so a
///   uuid's segments and a dotted path are not read as shas
/// * it must contain a digit, so `defaced` is not a sha
/// * it must contain a letter, so a bare number like `7892345` is not one
///
/// A real 7-character id misses the digit test about 4% of the time and the
/// letter test about as often. Missing a few shas is acceptable; flagging an
/// English word would put the exemption back where it started.
fn asserted_shas(text: &str) -> Vec<String> {
    const MIN_SHA: usize = 7;
    const MAX_SHA: usize = 40;
    let chars: Vec<char> = text.chars().collect();
    let touches_word =
        |c: Option<&char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-');
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_hexdigit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i].is_ascii_hexdigit() {
            i += 1;
        }
        let run: String = chars[start..i].iter().collect();
        let standalone =
            (start == 0 || !touches_word(Some(&chars[start - 1]))) && !touches_word(chars.get(i));
        if run.len() >= MIN_SHA
            && run.len() <= MAX_SHA
            && standalone
            && run.chars().any(|c| c.is_ascii_digit())
            && run.chars().any(|c| c.is_ascii_alphabetic())
        {
            // Git prints lowercase; a report may not.
            out.push(run.to_ascii_lowercase());
        }
    }
    out
}

/// Result tallies the text states: `7,896 passed`, `0 failed`, `30 ignored`.
///
/// The fact keeps its keyword so the nudge can quote the claim, and
/// `fact_is_backed` reads the digits back out of it.
fn asserted_tallies(text: &str) -> Vec<String> {
    const KEYWORDS: [&str; 3] = ["passed", "failed", "ignored"];
    let lower = text.to_lowercase();
    let mut out = Vec::new();
    for kw in KEYWORDS {
        let mut search = 0;
        while let Some(rel) = lower[search..].find(kw) {
            let at = search + rel;
            search = at + kw.len();
            // Word-bounded, so the `passed` in `bypassed` is not a tally.
            if lower[at + kw.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
            {
                continue;
            }
            let before = lower[..at].trim_end();
            let digits: String = before
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit() || *c == ',')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let plain: String = digits.chars().filter(|c| *c != ',').collect();
            // A number has to actually precede the keyword.
            if plain.is_empty() || !before.ends_with(&digits) {
                continue;
            }
            out.push(format!("{digits} {kw}"));
        }
    }
    out
}

/// Compiler and linter diagnostic codes the text states as already obtained.
///
/// Neither existing extractor reaches a rustc code. `asserted_shas` needs a run
/// of at least seven hex characters and `E0603` is five; `asserted_tallies`
/// recognises only `passed` / `failed` / `ignored`, so `2× E0603` is not a tally
/// either. A turn can therefore assert a specific compiler error and quote its
/// message, and no fact check in this module has anything to compare against —
/// which is how "Gate died on 2× E0603 \"this item is private\"" reached the user
/// in a response whose own tool call was launching that gate (#1693).
///
/// The token is the check, not the wording around it. `E0603` is not English
/// prose, so unlike the shape-based tells it cannot be evaded by rephrasing,
/// and unlike a sha it is never an incidental substring of ordinary text. The
/// boundary rules mirror `asserted_shas`: a run that touches a word character,
/// `_`, `-` or `.` is part of something else (a path, a longer hash) and is not
/// a diagnostic.
fn asserted_diagnostics(text: &str) -> Vec<String> {
    /// A pathological iteration names a handful of diagnostics.
    const MAX_DIAGNOSTICS: usize = 8;
    /// rustc codes are `E` followed by exactly four digits.
    const CODE_LEN: usize = 5;
    let chars: Vec<char> = text.chars().collect();
    let touches_word = |c: Option<&char>| {
        c.is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i + CODE_LEN <= chars.len() {
        if chars[i] != 'E'
            || !chars[i + 1..i + CODE_LEN]
                .iter()
                .all(|c| c.is_ascii_digit())
        {
            i += 1;
            continue;
        }
        if !touches_word(i.checked_sub(1).map(|p| &chars[p]))
            && !touches_word(chars.get(i + CODE_LEN))
        {
            out.push(chars[i..i + CODE_LEN].iter().collect());
        }
        i += CODE_LEN;
    }
    out.truncate(MAX_DIAGNOSTICS);
    out
}

/// Commands or tools the text denies ever having run (#2122).
///
/// The shape a verb list cannot reach: "No `sleep` command was ever in my tool
/// calls" names no action the agent claims to have taken, it claims the ABSENCE
/// of one, and the evidence that settles it is the session's own call history.
/// A denial is decidable exactly, so it costs no false positives, unlike the
/// past-tense verb list that measured 10.2% wrong on legitimate answers
/// (`~/.opencrabs/projects/opencrabs/research/2122-measurement.md`).
///
/// Two families, both narrow on purpose:
///
/// * `no <token> command|call|invocation|tool`
/// * `never|didn't|did not <ran|run|called|call|invoked|invoke|executed|execute> <token>`
///
/// The token has to look like a command or tool name, and pronouns are dropped
/// because "I never ran it" names nothing checkable.
pub fn asserted_history_denials(text: &str) -> Vec<String> {
    /// A pathological iteration denies a handful of things, not dozens.
    const MAX_DENIALS: usize = 6;
    static DENIAL_RE: LazyLock<Regex> = LazyLock::new(|| {
        // One literal string, no line continuation: in a raw string a
        // trailing backslash is a backslash plus a newline, which the regex
        // engine reads as an escaped newline and which silently breaks the
        // alternation.
        Regex::new(concat!(
            r"(?i)(?:no\s+`?([a-z0-9_.-]{2,40})`?\s+(?:command|call|invocation|tool)\b)",
            r"|(?:(?:never|didn'?t|did\s+not)\s+",
            r"(?:ran|run|called|call|invoked|invoke|executed|execute)\s+",
            r"`?([a-z0-9_.-]{2,40})`?)",
        ))
        .expect("denial regex is valid")
    });
    /// Words that name nothing a history check could look up.
    const PRONOUNS: [&str; 10] = [
        "it", "that", "this", "anything", "nothing", "them", "those", "these", "one", "any",
    ];
    let mut out = Vec::new();
    for caps in DENIAL_RE.captures_iter(text) {
        let Some(token) = caps.get(1).or_else(|| caps.get(2)) else {
            continue;
        };
        let token = token
            .as_str()
            .to_lowercase()
            // A sentence-ending period is not part of the command name.
            .trim_end_matches(['.', '-', '_'])
            .to_string();
        if PRONOUNS.contains(&token.as_str()) {
            continue;
        }
        out.push(token);
    }
    out.sort();
    out.dedup();
    out.truncate(MAX_DENIALS);
    out
}

/// Which denials the session's own recorded commands contradict.
///
/// The match is on a command segment's first word, not a substring: a
/// `grep -rn "sleep" logs/` call mentions the word without being one, and
/// counting it as evidence would clear the very lie this exists to catch. The
/// segment walk skips the wrapper vocabulary the sleep gate skips, so `timeout
/// 30 sleep 5` and `cd /tmp && sleep 5` still count as a sleep.
pub fn denials_contradicted(denials: &[String], executed_inputs: &[String]) -> Vec<String> {
    // Tool inputs arrive as JSON, so the command has to be pulled out first: in
    // `{"command":"sleep 5"}` the first word is `{"command":"sleep`, which
    // matches nothing.
    let commands: Vec<String> = executed_inputs
        .iter()
        .map(|input| extract_command_field(input).unwrap_or_else(|| input.clone()))
        .collect();
    denials
        .iter()
        .filter(|denial| {
            commands
                .iter()
                .any(|command| command_invokes(command, denial))
        })
        .cloned()
        .collect()
}

/// Does `command` invoke `token` as the first word of any of its segments?
fn command_invokes(command: &str, token: &str) -> bool {
    crate::brain::tools::bash::split_command_segments(command)
        .iter()
        .any(|segment| {
            for raw in segment.split_whitespace() {
                let word = raw.trim_matches(|c: char| c == '`' || c == '"' || c == '\'');
                if word.eq_ignore_ascii_case(token) {
                    return true;
                }
                // Prefixes that carry no meaning of their own: a wrapper, a
                // flag, a bare duration, or an inline environment assignment.
                let skippable = matches!(
                    word,
                    "sudo"
                        | "command"
                        | "exec"
                        | "nohup"
                        | "env"
                        | "do"
                        | "then"
                        | "timeout"
                        | "xargs"
                ) || word.starts_with('-')
                    || word.contains('=')
                    || (!word.is_empty() && word.chars().all(|c| c.is_ascii_digit()));
                if !skippable {
                    break;
                }
            }
            false
        })
}

/// Tool inputs recorded in a session's persisted assistant rows (#2122).
///
/// A denial can be about an earlier turn: the 2026-10-18 lie ("no `sleep`
/// command was ever in my tool calls") sat thirty calls deep in the same
/// session, so turn scope alone would have missed it. `tool_executions` cannot
/// answer it either, since it stores no input column; the command text lives in
/// `messages.content` inside the v2 ledgers, under `i`.
///
/// `d` is the display description ("bash: sleep 30") and is used as a fallback
/// for rows written before `i` existed, so an older session is still checkable.
pub fn ledger_tool_inputs(contents: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for content in contents {
        let mut rest = content.as_str();
        while let Some(ledger) = crate::tui::app::ledger_scan::next_ledger(rest) {
            if ledger.is_v2
                && let Ok(entries) = serde_json::from_str::<Vec<serde_json::Value>>(ledger.body)
            {
                for entry in entries {
                    let input = entry.get("i").map(|v| v.to_string()).or_else(|| {
                        // `d` reads `bash: sleep 30`, so the tool-name prefix
                        // has to go or the first word is `bash:`.
                        entry.get("d").and_then(|v| v.as_str()).map(|d| {
                            d.split_once(": ")
                                .map(|(_, rest)| rest.to_string())
                                .unwrap_or_else(|| d.to_string())
                        })
                    });
                    if let Some(input) = input {
                        out.push(input);
                    }
                }
            }
            rest = &rest[ledger.end..];
        }
    }
    out
}

/// Issue and pull request numbers the text claims to have produced (#2122).
///
/// A number merely referenced is not a claim. "Check #2098" quotes the user and
/// answering it must not be punished, so a number is recorded only when a
/// production verb sits on the same line within a short window of it. The
/// distinction is the whole point: the defect is a false report of work, not
/// mentioning an identifier.
pub fn asserted_produced_identifiers(text: &str) -> Vec<String> {
    /// A pathological iteration reports a handful of filings.
    const MAX_IDENTIFIERS: usize = 8;
    /// How far the number may sit from the verb that produced it.
    const WINDOW: usize = 80;
    /// Verbs that assert the agent created or changed the object.
    const VERBS: [&str; 7] = [
        "filed",
        "opened",
        "closed",
        "created",
        "merged",
        "reopened",
        "commented on",
    ];
    let mut out = Vec::new();
    for line in text.lines() {
        let lower = line.to_lowercase();
        for verb in VERBS {
            let mut search = 0;
            while let Some(rel) = lower[search..].find(verb) {
                let after = search + rel + verb.len();
                search = after;
                let window_end = {
                    // `after + WINDOW` can land mid-character on non-ASCII text,
                    // and slicing there panics (#2124).
                    let mut end = (after + WINDOW).min(lower.len());
                    while end < lower.len() && !lower.is_char_boundary(end) {
                        end += 1;
                    }
                    end
                };
                if after >= window_end {
                    continue;
                }
                let window = &lower[after..window_end];
                let bytes = window.as_bytes();
                let mut i = 0;
                while i < bytes.len() {
                    if bytes[i] != b'#' {
                        i += 1;
                        continue;
                    }
                    let mut j = i + 1;
                    while j < bytes.len() && bytes[j].is_ascii_digit() {
                        j += 1;
                    }
                    // `#` alone, or `#` before a non-digit, is not an id.
                    if j > i + 1 {
                        out.push(window[i..j].to_string());
                        i = j;
                    } else {
                        i += 1;
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out.truncate(MAX_IDENTIFIERS);
    out
}

/// Whether the conversation record supports a produced identifier.
///
/// The tool output for a filing is a URL (`.../issues/2122`) or a confirmation
/// line (`Created issue #2122`), so the bare `#2122` match alone would miss the
/// common shape and flag true work. All four spellings are accepted.
fn identifier_is_backed(identifier: &str, known: &str) -> bool {
    let Some(digits) = identifier.strip_prefix('#') else {
        return known.contains(identifier);
    };
    known.contains(identifier)
        || known.contains(&format!("issues/{digits}"))
        || known.contains(&format!("pull/{digits}"))
        || known.contains(&format!("discussions/{digits}"))
}

/// Completed artifacts the text claims were produced (#482).
///
/// A compaction summary is obeyed as the continuation document, so an artifact
/// it reports as finished is acted on. Observed 2026-09-21 13:04:26: a
/// `FullWindow` summary asserted `PRE-JUDGED VERDICTS (the analytical work is
/// DONE ... transcribe them)`, quoted "the exact text, end of part3" and "the
/// exact first two lines of part1", and ordered the woken agent to transcribe
/// it — while all three `write_file` calls ran at 13:04:28, :39 and :48, AFTER
/// the summary landed. A summary cannot quote what does not exist.
///
/// This is the artifact-existence half of the same principle as
/// `asserted_facts` (#1423): what separates a report from a fabrication is
/// whether the thing it names exists. It is a separate extractor rather than a
/// fourth `asserted_tallies` keyword because the BACKING TEST differs — a tally
/// is checked on its digits, and the digits in `13 HOLDS · 13 WRONG-MODE · 4
/// DOES-NOT-HOLD` appear in any conversation that mentions thirteen of
/// anything, so the incident's own count line would pass as backed. These facts
/// are checked on their whole quoted span.
///
/// Deliberately narrow, in the posture of `asserted_shas`: a claim is recorded
/// only when a completion cue and the artifact sit on the SAME line, so prose
/// about files is not read as a claim, and a backticked path is skipped because
/// a quoted path is a reference rather than a production claim.
pub fn claimed_artifacts(text: &str) -> Vec<String> {
    /// Bound the nudge and the scan.
    const MAX_CLAIMS: usize = 10;
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if !completion_cue(&trimmed.to_lowercase()) {
            continue;
        }
        let Some(name) = artifact_name(trimmed) else {
            continue;
        };
        // A quoted path is being referenced, not produced.
        if trimmed.contains(&format!("`{name}`")) {
            continue;
        }
        out.push(name);
    }
    out.sort();
    out.dedup();
    out.truncate(MAX_CLAIMS);
    out
}

/// Whether a line frames its work as finished rather than proposed.
fn completion_cue(lower: &str) -> bool {
    const CUES: [&str; 10] = [
        "(exact",
        "exact text",
        "verbatim",
        "transcribe",
        "completed",
        "work is done",
        "has been written",
        "have been written",
        "were written",
        "was written",
    ];
    CUES.iter().any(|c| lower.contains(*c))
}

/// The artifact a completion cue names, if the line names one.
///
/// Two shapes qualify: a run ending in a known extension (`part1.md`), and a
/// bare stem carrying a digit and introduced by `of`/`in`/`into`/`from`
/// (`end of part3`) — which is how the incident's summary named files it had
/// never written. The LAST match on the line wins, because that is the artifact
/// the cue is attached to.
fn artifact_name(line: &str) -> Option<String> {
    const EXTS: [&str; 6] = [".md", ".json", ".txt", ".csv", ".tsv", ".log"];
    const INTROS: [&str; 4] = ["of ", "in ", "into ", "from "];
    let chars: Vec<char> = line.chars().collect();
    let is_name = |c: char| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-';
    let mut best = None;
    let mut i = 0;
    while i < chars.len() {
        if !is_name(chars[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_name(chars[i]) {
            i += 1;
        }
        let run: String = chars[start..i].iter().collect();
        let has_ext = EXTS.iter().any(|e| run.ends_with(*e)) && run.len() > 3;
        let prefix: String = chars[..start].iter().collect::<String>().to_lowercase();
        let bare_stem =
            run.chars().any(|c| c.is_ascii_digit()) && INTROS.iter().any(|p| prefix.ends_with(*p));
        if has_ext || bare_stem {
            best = Some(run);
        }
    }
    best
}

/// Whether a claimed artifact exists anywhere in the conversation's evidence.
///
/// The whole span is the needle, unlike a tally: `13` alone appears in any
/// conversation that mentions thirteen of anything, and the incident's count
/// line is exactly that shape.
///
/// Separators are stripped from both sides, so `part3` matches the `part3.md`
/// a write tool would have named and `out/part3.md` matches a bare `part3.md`.
pub fn claimed_artifact_unbacked(claim: &str, evidence: &str) -> bool {
    let strip = |s: &str| -> String {
        s.chars()
            .filter(|c| !c.is_whitespace() && *c != '/' && *c != '\\')
            .collect()
    };
    let needle = strip(claim);
    !needle.is_empty() && !strip(evidence).contains(&needle)
}

/// Whether one asserted fact exists in the conversation's evidence.
///
/// A tally is checked in both spellings: a report may quote `7,926` from a run
/// that printed `7926`, and neither spelling is a fabrication.
fn fact_is_backed(fact: &str, known: &str) -> bool {
    if let Some(plain) = digits_of(fact) {
        return known.contains(&plain) || known.contains(&group_thousands(&plain));
    }
    known.contains(fact)
}

/// The digits of a tally fact, or None for a sha.
fn digits_of(fact: &str) -> Option<String> {
    let (digits, _keyword) = fact.split_once(' ')?;
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit() || c == ',') {
        return None;
    }
    let plain: String = digits.chars().filter(|c| *c != ',').collect();
    (!plain.is_empty()).then_some(plain)
}

/// `7926` into `7,926`, the spelling a report quotes from a tallied run.
fn group_thousands(digits: &str) -> String {
    let chars: Vec<char> = digits.chars().collect();
    let mut out = String::new();
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*c);
    }
    out
}

/// Sentence fragments, split on the usual terminators but never inside a
/// backticked span (#1074).
///
/// Splitting first and looking for backticks second tore every dotted command
/// in half: `` `wc -l src/tui/mod.rs` `` became `` `wc -l src/tui/mod `` plus
/// `` rs` ``, neither fragment holding a closed pair, so the span was never
/// parsed and the allowlist never consulted. That is the majority shape of a
/// real inspection claim, because the fabricated fact is normally about a
/// specific file, a path or a host.
///
/// Backticks are paired over the whole text before splitting rather than
/// toggled statefully as the scan walks it. A stray unmatched backtick would
/// otherwise swallow the entire remainder into one pseudo-sentence, letting a
/// framing in one paragraph vouch for a command three paragraphs down.
fn sentences_outside_code_spans(text: &str) -> Vec<&str> {
    let spans = code_span_ranges(text);
    let mut out = Vec::new();
    let mut start = 0;
    for (offset, c) in text.char_indices() {
        if !matches!(c, '.' | '\n' | '!' | '?') {
            continue;
        }
        if spans
            .iter()
            .any(|(open, close)| offset > *open && offset < *close)
        {
            continue;
        }
        out.push(&text[start..offset]);
        start = offset + c.len_utf8();
    }
    out.push(&text[start..]);
    out
}

/// Byte offsets of each opening backtick and its matching closing one.
///
/// Unmatched trailing backticks are dropped: without a close there is no span,
/// so the delimiters after it keep splitting normally.
fn code_span_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut cursor = 0;
    while let Some(open_rel) = text[cursor..].find('`') {
        let open = cursor + open_rel;
        let Some(close_rel) = text[open + 1..].find('`') else {
            break;
        };
        let close = open + 1 + close_rel;
        ranges.push((open, close));
        cursor = close + 1;
    }
    ranges
}

/// Whether the sentence presents its command as done rather than proposed.
///
/// Scanned across every language's `executed_framings`, so a claim in
/// Portuguese or Russian is caught the same as one in English.
fn frames_as_executed_any(lower: &str) -> bool {
    phantom_lang::all_langs().iter().any(|lang| {
        lang.executed_framings
            .iter()
            .any(|m| lower.contains(m.as_str()))
    })
}

/// Programs a fabricated command claim is most likely to name.
///
/// Module-scoped so the structural fenced-block tell (#1194) checks the
/// same allowlist as the inline-backtick check, instead of drifting a
/// second copy. A gap here is a gap in both.
pub(crate) const KNOWN_PROGRAMS: &[&str] = &[
    // Version control and issue tracking.
    "gh",
    "git",
    // Build and package managers.
    "cargo",
    "npm",
    "pnpm",
    "yarn",
    "make",
    "pip",
    "pip3",
    "brew",
    "rustc",
    "rustup",
    "go",
    "node",
    "python",
    "python3",
    // Text and file inspection.
    "grep",
    "rg",
    "ls",
    "cat",
    "wc",
    "sed",
    "awk",
    "diff",
    "find",
    "head",
    "tail",
    "sort",
    "uniq",
    "jq",
    // Filesystem and process facts — the timestamp/size/uptime claims.
    "stat",
    "date",
    "du",
    "df",
    "ps",
    "lsof",
    "uname",
    "whoami",
    "which",
    "uptime",
    // Hashing and crypto.
    "shasum",
    "sha256sum",
    "md5sum",
    "openssl",
    // Network.
    "curl",
    "wget",
    "dig",
    "ping",
    "ssh",
    "scp",
    "rsync",
    // Services and infrastructure.
    "systemctl",
    "journalctl",
    "docker",
    "kubectl",
    "terraform",
    // Databases.
    "psql",
    "sqlite3",
];

/// Backticked spans that look like a shell command: a bare program name
/// followed by at least one argument. Prose in backticks (`mod.rs`, a symbol,
/// a file path) has no space and is skipped.
///
/// The allowlist is what bounds this check, so a gap in it is a gap in the
/// check. It shipped with `psql` but not `ps`, and with `sha256sum` but not
/// `stat` or `date` — the three commands that produce the facts a turn is most
/// tempted to invent. Fabricated build timestamps ("the binary was built at
/// 20:24:41") named `stat` in backticks, framed as executed, and passed
/// untouched because the program was unknown. Inspection commands belong here
/// at least as much as the mutating ones: nobody fabricates `git commit`, they
/// fabricate its output.
///
/// Deliberately absent: `env`, `file`, `test` and other program names that are
/// also ordinary English nouns. A backticked `env var` or `file path` inside a
/// sentence framed as executed would read as a command claim and flag an
/// honest recap, and a false self-heal costs a whole turn.
fn backticked_commands(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else { break };
        let span = after[..close].trim();
        rest = &after[close + 1..];
        let mut words = span.split_whitespace();
        if let Some(prog) = words.next()
            && words.next().is_some()
            && KNOWN_PROGRAMS.contains(&prog)
        {
            out.push(span.to_string());
        }
    }
    out
}

/// Did every tool call this turn do nothing (#825)?
///
/// The post-success exemption asks "did any tool succeed", when what actually
/// vouches for a claim is "did a tool DO anything". A model that cannot or
/// will not make a real call can satisfy the former indefinitely with `true`.
///
/// Observed: seven green tool calls, none of them the one being claimed. Two
/// `true` no-ops, two `echo`s narrating an intent (`echo "attempting
/// telegram_send"`), and the turn then asserted it had "tried telegram_send a
/// dozen times". The calls were the substitute for the work, and they bought
/// immunity from every check that keys off tool success.
///
/// Returns false for an empty turn: no calls at all is already handled by the
/// zero-call branch, and reporting it here would double-count.
pub fn all_calls_were_null_effect(tool_inputs: &[String]) -> bool {
    !tool_inputs.is_empty() && tool_inputs.iter().all(|input| is_null_effect_call(input))
}

/// Does this single call produce no state change and no observation?
///
/// Only shell commands can be null: a `read_file` or `grep` observes
/// something by definition, so anything without a `command` field counts as
/// real work.
fn is_null_effect_call(input_json: &str) -> bool {
    let Some(command) = extract_command_field(input_json) else {
        return false;
    };
    is_null_effect_command(&command)
}

/// `true`, `:`, or a bare `echo` of a literal.
///
/// Deliberately narrow. `echo` legitimately writes files and builds
/// here-documents, and its output is often consumed downstream, so anything
/// carrying a redirect, pipe, or chain is real work. What is null is `echo`
/// whose entire effect is printing a string nobody reads.
pub fn is_null_effect_command(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return true;
    }
    // A chain does something even if one link is trivial.
    if trimmed.contains("&&")
        || trimmed.contains("||")
        || trimmed.contains('|')
        || trimmed.contains('>')
        || trimmed.contains('<')
        || trimmed.contains(';')
        || trimmed.contains('`')
        || trimmed.contains("$(")
    {
        return false;
    }
    if trimmed == "true" || trimmed == ":" {
        return true;
    }
    trimmed.strip_prefix("echo ").is_some_and(|rest| {
        // `echo $VAR` reads state; a quoted or bare literal does not.
        !rest.contains('$')
    })
}

/// Pull `command` out of a tool-input JSON blob.
fn extract_command_field(input_json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(input_json).ok()?;
    value
        .get("command")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Tool-input fragments that mean a file was actually handed to a surface.
///
/// Checked as substrings of the raw input JSON so a new channel's send does
/// not silently fall outside the check.
///
/// Deliberately underscore-style action names and parameter keys only. A first
/// attempt included bare words like `attachment`, and the very turn this
/// detector exists for defeated it: its `tool_search` query was "send telegram
/// document file attachment", so SEARCHING for how to send a file counted as
/// having sent one. Prose never contains `send_document` or `document_url`.
const FILE_SEND_MARKERS: &[&str] = &[
    "send_document",
    "send_file",
    "upload_file",
    "document_url",
    "file_url",
    "media_url",
];

/// Claimed a file was delivered when nothing sent one (#825).
///
/// The image equivalent (#747) checks for an `<<IMG:>>` marker in the text,
/// because `generate_image` delivers inline. A document goes out through a
/// tool call instead, so the evidence lives in what the turn INVOKED, not in
/// what it wrote. Document sending arrived after those media checks, which is
/// why this shape was uncovered.
///
/// Observed: a turn ran `write_file`, four no-ops, and `tool_search`, never
/// invoked any send, and opened its reply with "File sent above." The file
/// existed on disk; nothing had carried it to the chat.
///
/// Multilingual by construction — phrases come from every `phantom_lang`
/// TOML scanned as a union, never via `detect_language`, so an accented
/// language cannot disable it.
pub fn claims_unsent_file(text: &str, tool_inputs: &[String]) -> bool {
    // Something really did ship a file: nothing to flag.
    if tool_inputs
        .iter()
        .any(|input| FILE_SEND_MARKERS.iter().any(|m| input.contains(m)))
    {
        return false;
    }
    let lower = text.to_lowercase();
    let claims_delivery = phantom_lang::all_langs()
        .iter()
        .flat_map(|l| l.file_delivery_phrases.iter())
        .any(|p| lower.contains(p.as_str()));
    if !claims_delivery {
        return false;
    }
    // The phrase must actually be about a file. "sent above" alone is said of
    // ordinary messages all the time.
    phantom_lang::all_langs()
        .iter()
        .flat_map(|l| l.file_context_words.iter())
        .any(|w| lower.contains(w.as_str()))
}

/// Structured completion report (#1506): the text carries real report
/// furniture — a markdown table (header + separator row + at least one data
/// row) or two or more `##` section headings.
///
/// Every detector above matches verbatim strings: intent phrases, quoted
/// commands, sha/tally tokens checked against tool output. A distilled turn
/// summary paraphrases by construction, so the genre structurally
/// misfires — four legitimate batch reports (1.8-2.0 KB, tables included)
/// were discarded on 2026-09-10 while every fact in them was real. The
/// owner's standing directive: reports with tables and structured data are
/// NEVER discarded. The mid-loop kill exempts this genre from the phrase
/// matchers; a fired fact check still kills it (#2039). The fired branches
/// land in the log for forensics.
///
/// Prose-shaped completions keep the full detector set: a short fabricated
/// "Done, shipped abc1234" matches nothing here and dies as before.
pub fn is_structured_report(text: &str) -> bool {
    let mut pipe_rows = 0usize;
    let mut separator_seen = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            continue;
        }
        pipe_rows += 1;
        let inner = trimmed.trim_start_matches('|').trim_end_matches('|');
        // A separator row is pipes and dashes only: `|---|---|`. A data row
        // carries alphanumerics, so `| a--b |` never counts as a separator.
        if inner.contains("---") && !inner.chars().any(|c| c.is_alphanumeric()) {
            separator_seen = true;
        }
    }
    // Header + separator + at least one data row.
    if separator_seen && pipe_rows >= 3 {
        return true;
    }
    // Two or more `##` headings: a structured document, not prose.
    text.lines()
        .filter(|line| line.trim_start().starts_with("## "))
        .count()
        >= 2
}

/// Branches that check a claim against evidence: the facts it states, the
/// commands it names, the results it quotes. A fired fact check is proof of
/// fabrication, so a structured report may never exempt it (#2039).
pub fn is_fact_check_branch(branch: &str) -> bool {
    matches!(
        branch,
        "unbacked_facts" | "uncalled_commands" | "unbacked_evidence"
    )
}

/// Whether a structured report escapes the phantom kill for the branches that
/// fired. Phrase matchers false-positive on distilled report prose, so the
/// report exempts them. Fact checks never yield to the exemption (#2039).
pub fn structured_report_exempts(structured: bool, fired_branches: &[&str]) -> bool {
    structured && !fired_branches.iter().any(|b| is_fact_check_branch(b))
}

/// Whether a text-only iteration that follows an option-surface halt escapes
/// the phantom kill (#31). The halt already handed the turn back with the
/// options rendered, so the trailing text closes the turn: it is a sign-off,
/// never a promised action. Phrase matchers false-positive on exactly that
/// prose, and the self-heal retry they triggered posted a second answer to a
/// channel whose first answer was already delivered (2026-10-17, Discord:
/// "Research delivered, file written, nothing generated ..." was killed on
/// `intent_no_tools` after the turn had run 11 tools). Fact checks still kill
/// it, the same rule structured reports follow (#2039): a sign-off that
/// states a sha the conversation cannot back is still invented.
pub fn option_halt_exempts(fired_branches: &[&str]) -> bool {
    !fired_branches.iter().any(|b| is_fact_check_branch(b))
}
