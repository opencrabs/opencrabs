//! Seeker: one ranker for the terminal and the window.
//!
//! The owner asked for Spotlight scoped to the app: one chord, a card in the
//! middle, the caret in the field, and everything in this app findable by the
//! word the user already has in their head. That word is often not the name of
//! the thing: `voice` has to find `/whisper`, `upgrade` has to find `/evolve`,
//! `rsi` has to find `/mission-control`.
//!
//! None of that may live in a frontend. Two frontends ranking the same query
//! differently is the drift this module exists to prevent, so the scoring, the
//! keyword table, the ordering and the state all live here, with no terminal
//! and no UI toolkit in sight (#2458).

use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;

/// How long the field's caret keeps blinking after the last keystroke. Past
/// this the field goes quiet and the caret hides, the way Spotlight's does
/// once typing stops; the next key brings it back (#2531).
pub const CARET_IDLE_AFTER: Duration = Duration::from_millis(1600);

/// Whether the caret is live at `now`, given the last key at `last`.
///
/// The rule lives here, shared, so the terminal and the window cannot pick
/// different idle windows and so the behaviour has a test that runs without
/// a UI (#2531).
pub fn caret_blinks(last: Instant, now: Instant, idle_after: Duration) -> bool {
    now.saturating_duration_since(last) < idle_after
}

/// Byte offset where the word before `offset` begins. The walk skips
/// separators and then the word, matching the terminal and GUI composers.
/// Grapheme boundaries keep the walk from splitting a multibyte character.
pub fn previous_word_start(content: &str, offset: usize) -> usize {
    let clamped = offset.min(content.len());
    let graphemes: Vec<(usize, &str)> = content.grapheme_indices(true).collect();
    let mut index = graphemes
        .iter()
        .position(|(start, _)| *start >= clamped)
        .unwrap_or(graphemes.len());
    while index > 0 && !is_word_grapheme(graphemes[index - 1].1) {
        index -= 1;
    }
    while index > 0 && is_word_grapheme(graphemes[index - 1].1) {
        index -= 1;
    }
    graphemes.get(index).map(|(start, _)| *start).unwrap_or(0)
}

/// Byte offset the word after `offset` ends on, past the word and the
/// separators that follow it, matching the terminal composer.
pub fn next_word_end(content: &str, offset: usize) -> usize {
    let clamped = offset.min(content.len());
    let graphemes: Vec<(usize, &str)> = content.grapheme_indices(true).collect();
    let mut index = graphemes
        .iter()
        .position(|(start, _)| *start >= clamped)
        .unwrap_or(graphemes.len());
    while index < graphemes.len() && is_word_grapheme(graphemes[index].1) {
        index += 1;
    }
    while index < graphemes.len() && !is_word_grapheme(graphemes[index].1) {
        index += 1;
    }
    graphemes
        .get(index)
        .map(|(start, _)| *start)
        .unwrap_or(content.len())
}

/// Letters and digits are word content; whitespace and punctuation separate
/// words. A grapheme counts as a word when any of its characters is a letter.
fn is_word_grapheme(grapheme: &str) -> bool {
    grapheme.chars().any(|ch| ch.is_alphanumeric())
}

/// Byte index of a character offset, clamped to the end. The Seeker caret is
/// counted in chars so it can never land inside a UTF-8 codepoint.
fn char_to_byte(content: &str, offset: usize) -> usize {
    content
        .char_indices()
        .nth(offset)
        .map(|(index, _)| index)
        .unwrap_or(content.len())
}

/// Character index of a byte offset produced by a word walk or a shaped-text
/// mouse hit. Public so the GUI can translate GPUI's byte index without doing
/// UTF-8 arithmetic of its own.
pub fn byte_to_char(content: &str, offset: usize) -> usize {
    content
        .char_indices()
        .take_while(|(index, _)| *index < offset)
        .count()
}

/// What a row is, which decides how it is labelled and how ties break.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum PaletteKind {
    /// A slash command the frontend runs.
    Command,
    /// A screen or panel the frontend can show.
    Surface,
    /// A conversation from the rail.
    Session,
    /// A provider model from the catalogue.
    Model,
    /// A skill from `skills/`.
    Skill,
    /// A command from the user's own `commands.toml`.
    UserCommand,
    /// A messaging channel the agent can be reached on: the row starts its
    /// setup instead of describing it (#2462).
    Channel,
    /// A documentation page, opened in the default browser.
    Docs,
    /// The miss row: the query matched nothing, so the card offers to start
    /// a fresh session with the typed text as the request (#2482).
    Ask,
    /// The create-topic row: picking it re-arms the same card as a session
    /// picker, and the conversation picked there receives the topic (#2482).
    NewTopic,
    /// The favorite row: same picker, but the pick pins the conversation to
    /// the top of the rail instead of naming a topic (#2487).
    Favorite,
}

impl PaletteKind {
    /// The word printed down the right of the row.
    pub fn label(self) -> &'static str {
        match self {
            PaletteKind::Command => "command",
            PaletteKind::Surface => "surface",
            PaletteKind::Session => "session",
            PaletteKind::Model => "model",
            PaletteKind::Skill => "skill",
            PaletteKind::UserCommand => "yours",
            PaletteKind::Channel => "channel",
            PaletteKind::Docs => "docs",
            PaletteKind::Ask => "ask",
            PaletteKind::NewTopic => "new topic",
            PaletteKind::Favorite => "favorite",
        }
    }

    /// Tie-break rank: an action beats a destination, a destination beats one
    /// more conversation. Lower wins, and it only decides equal scores.
    fn rank(self) -> u8 {
        match self {
            PaletteKind::Command => 0,
            PaletteKind::Surface => 1,
            PaletteKind::Skill => 2,
            // A setup row is an action, so it sits with the surfaces: for the
            // query `telegram` it is printed above the docs page that carries
            // the same name (#2462).
            PaletteKind::Channel => 1,
            PaletteKind::Docs => 3,
            PaletteKind::UserCommand => 4,
            PaletteKind::Model => 5,
            PaletteKind::Session => 6,
            // The miss row only ever prints alone, so its rank is a formality:
            // dead last keeps it out of the way should that ever change.
            PaletteKind::Ask => 7,
            // An action, so it sits with the destinations, above chats.
            PaletteKind::NewTopic => 1,
            PaletteKind::Favorite => 1,
        }
    }

    /// The rank the IDLE card orders by: with nothing typed every score is
    /// equal, so this decides the whole list. Destinations lead (the screen
    /// the word names), the two action rows follow, then commands, docs and
    /// finally chats, because an idle launcher is a menu, not a command
    /// dump (#2491).
    fn idle_rank(self) -> u8 {
        match self {
            PaletteKind::Surface => 0,
            // Actions sit with the destinations, under the screens.
            PaletteKind::NewTopic | PaletteKind::Favorite => 1,
            PaletteKind::Command | PaletteKind::UserCommand | PaletteKind::Channel => 2,
            PaletteKind::Docs => 3,
            PaletteKind::Session => 4,
            PaletteKind::Skill | PaletteKind::Model => 5,
            PaletteKind::Ask => 7,
        }
    }
}

/// A screen or panel a palette row can open. Named here so neither frontend
/// can offer a destination the other cannot reach.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Surface {
    Usage,
    Files,
    Themes,
    Models,
    Sessions,
    Help,
    Skills,
    Profiles,
    MissionControl,
}

impl Surface {
    /// The row title: the bare command, which is also what the query is
    /// matched against. A screen and the command that opens it print one
    /// name, and it is the name the user already types (#2491).
    pub fn title(self) -> &'static str {
        match self {
            Surface::Usage => "/usage",
            Surface::Files => "session files",
            Surface::Themes => "/theme",
            Surface::Models => "/models",
            Surface::Sessions => "/sessions",
            Surface::Help => "/help",
            Surface::Skills => "/skills",
            Surface::Profiles => "/profiles",
            Surface::MissionControl => "/mission-control",
        }
    }

    /// The muted line under the title: what opening the row actually shows.
    pub fn hint(self) -> &'static str {
        match self {
            Surface::Usage => "tokens, cost and context for this session",
            Surface::Files => "everything sent and received in this conversation",
            Surface::Themes => "pick the window's colours",
            Surface::Models => "every model the provider catalogue offers",
            Surface::Sessions => "the conversation rail",
            Surface::Help => "every key this client answers",
            Surface::Skills => "the saved workflows in skills/",
            Surface::Profiles => "switch the account the agent runs as",
            Surface::MissionControl => "jobs, runs and what they cost",
        }
    }

    /// The slash command that reaches this destination in the terminal.
    ///
    /// The session-files browser has no slash command: it is a view the
    /// terminal opens from its own key, so it answers `None` and the frontend
    /// opens the view itself rather than inventing a command that would fail.
    pub fn command(self) -> Option<&'static str> {
        match self {
            Surface::Usage => Some("/usage"),
            Surface::Files => None,
            Surface::Themes => Some("/theme"),
            Surface::Models => Some("/models"),
            Surface::Sessions => Some("/sessions"),
            Surface::Help => Some("/help"),
            Surface::Skills => Some("/skills"),
            Surface::Profiles => Some("/profiles"),
            Surface::MissionControl => Some("/mission-control"),
        }
    }

    /// Words that should find this destination without naming it.
    pub fn keywords(self) -> &'static str {
        match self {
            Surface::Usage => "cost tokens spend budget context ctx",
            Surface::Files => "attachments uploads downloads artifacts",
            Surface::Themes => "colour color dark light palette appearance",
            Surface::Models => "provider llm sonnet opus haiku switch",
            Surface::Sessions => "chats conversations history rail sidebar",
            Surface::Help => "keys shortcuts commands what can i press",
            Surface::Skills => "workflows recipes macros",
            Surface::Profiles => "accounts workspaces tenants switch",
            Surface::MissionControl => "rsi mc control jobs analytics dashboard",
        }
    }
}

/// Every destination the card offers, in a fixed order so the terminal and the
/// window cannot print different lists for the same empty query (#2458).
pub const SURFACES: &[Surface] = &[
    Surface::Sessions,
    Surface::Usage,
    Surface::Themes,
    Surface::Models,
    Surface::Help,
    Surface::Skills,
    Surface::Profiles,
    Surface::MissionControl,
];

/// True when a destination already prints this command as its row title, so
/// the card lists one row per destination instead of the same name twice
/// (#2491).
pub fn is_surface_command(name: &str) -> bool {
    SURFACES
        .iter()
        .any(|surface| surface.command() == Some(name))
}

/// The destination rows, built here rather than in each frontend, so a screen
/// one client can open is never offered by the other.
pub fn surface_items() -> Vec<PaletteItem> {
    SURFACES
        .iter()
        .map(|surface| {
            PaletteItem::new(
                surface.title(),
                surface.hint(),
                PaletteKind::Surface,
                PaletteAction::OpenSurface(*surface),
            )
            .with_keywords(surface.keywords())
        })
        .collect()
}

/// A messaging channel the agent can be reached on, and the shape of its setup.
///
/// The steps are the ones the `channels` skill documents, kept short enough to
/// read in a card row: what the channel needs before it can answer a message.
/// Nothing here reads the config or the network: the frontend knows which
/// channels are live and asks for the hint (#2462).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Channel {
    Telegram,
    Discord,
    Slack,
    WhatsApp,
    Trello,
}

impl Channel {
    /// Fixed order, so both frontends print the same five rows.
    pub const ALL: &'static [Channel] = &[
        Channel::Telegram,
        Channel::Discord,
        Channel::Slack,
        Channel::WhatsApp,
        Channel::Trello,
    ];

    /// The name the user types, and the word the query is matched on.
    pub fn name(self) -> &'static str {
        match self {
            Channel::Telegram => "Telegram",
            Channel::Discord => "Discord",
            Channel::Slack => "Slack",
            Channel::WhatsApp => "WhatsApp",
            Channel::Trello => "Trello",
        }
    }

    /// The credential the channel needs, in the order the setup asks for it.
    pub fn steps(self) -> &'static str {
        match self {
            Channel::Telegram => "bot token, enabled = true, restart",
            Channel::Discord => "bot token, channel ids, restart",
            Channel::Slack => "bot token, app token, restart",
            Channel::WhatsApp => "scan the QR, then enabled = true",
            Channel::Trello => "api key, member token, board",
        }
    }

    /// The page that documents this setup, for the docs row beside it.
    pub fn doc_path(self) -> &'static str {
        match self {
            Channel::Telegram => "/channels/telegram.html",
            Channel::Discord => "/channels/discord.html",
            Channel::Slack => "/channels/slack.html",
            Channel::WhatsApp => "/channels/whatsapp.html",
            Channel::Trello => "/channels/trello.html",
        }
    }

    /// Words that should find this channel without naming it.
    pub fn keywords(self) -> &'static str {
        match self {
            Channel::Telegram => "telegram tg bot botfather chat group message",
            Channel::Discord => "discord guild server chat bot",
            Channel::Slack => "slack workspace socket mode bot",
            Channel::WhatsApp => "whatsapp wa phone number qr pair",
            Channel::Trello => "trello board card list kanban",
        }
    }

    /// The line printed while the channel is not configured: what it takes,
    /// counted, so the card answers "how hard is this" before the user commits.
    /// True to the skill, and short enough for the terminal card (#2462).
    pub fn setup_hint(self) -> &'static str {
        match self {
            Channel::Telegram => {
                "Telegram is not set up: 3 steps, bot token from @BotFather, enabled = true, restart"
            }
            Channel::Discord => "Discord is not set up: 3 steps, bot token, channel ids, restart",
            Channel::Slack => "Slack is not set up: 3 steps, bot token, app token, restart",
            Channel::WhatsApp => "WhatsApp is not set up: 2 steps, scan the QR, enabled = true",
            Channel::Trello => "Trello is not set up: 3 steps, api key, member token, board",
        }
    }

    /// The channel a query names, if it names one. Matched on the name, or on a
    /// word long enough to be a real stem, so `telegram` and `tele` both find
    /// it while `telegraph` and a two letter prefix do not.
    fn named_in(query: &str) -> Option<Channel> {
        let query = query.to_lowercase();
        Channel::ALL.iter().copied().find(|channel| {
            let name = channel.name().to_lowercase();
            query.contains(&name)
                || query
                    .split_whitespace()
                    .any(|word| word.len() >= 4 && name.starts_with(word))
        })
    }
}

/// The setup rows: one per channel, titled with the channel's own name so the
/// same word that finds the docs page also finds the row that starts the setup.
pub fn channel_items() -> Vec<PaletteItem> {
    Channel::ALL
        .iter()
        .map(|channel| {
            PaletteItem::new(
                channel.name(),
                format!("set up: {}", channel.steps()),
                PaletteKind::Channel,
                PaletteAction::SetupChannel(*channel),
            )
            .with_keywords(channel.keywords())
        })
        .collect()
}

/// The hint printed under the field when the query names a channel that is not
/// live yet. `configured` is what the frontend read from the config, so a
/// channel that already answers messages gets no hint.
pub fn channel_hint(query: &str, configured: &[Channel]) -> Option<&'static str> {
    let channel = Channel::named_in(query)?;
    if configured.contains(&channel) {
        return None;
    }
    Some(channel.setup_hint())
}

/// The create-topic row both frontends list. One definition, so the window
/// and the terminal offer the same words and land in the same picker (#2482).
pub fn new_topic_item() -> PaletteItem {
    PaletteItem::new(
        "create topic",
        "pick a conversation, name the topic, done",
        PaletteKind::NewTopic,
        PaletteAction::NewTopic,
    )
    .with_keywords("create topic new topic make topic start a topic")
}

/// The favorite row both frontends list: same picker as create topic, but
/// the pick pins the conversation to the top of the rail (#2487). One
/// definition, so the window and the terminal offer the same words.
pub fn favorite_item() -> PaletteItem {
    PaletteItem::new(
        "favorite any session",
        "pick a conversation, pin it to the top",
        PaletteKind::Favorite,
        PaletteAction::Favorite,
    )
    .with_keywords("favorite favourite pin star session bookmark top")
}

/// What picking a row does. The frontend applies it; this module only names it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PaletteAction {
    RunSlash(&'static str),
    OpenSurface(Surface),
    /// A session id, held as `u128` so this module needs no uuid dependency.
    /// The frontend converts it back with `Uuid::from_u128`.
    OpenSession(u128),
    SwitchModel {
        provider: String,
        model: String,
    },
    RunSkill(String),
    RunUserCommand(String),
    /// Start the guided setup for a messaging channel. The frontend sends the
    /// instruction, so the work happens in the conversation the user is
    /// already in and the config write goes through the normal gate (#2462).
    SetupChannel(Channel),
    /// A documentation page, held as the path the sitemap publishes. The
    /// frontends compose the URL with `ui_core::docs::url`, so the host lives
    /// in one place.
    OpenDocs(&'static str),
    /// The miss row's action: start a fresh session and send the typed text
    /// as its first request (#2482). The frontend funnels it through its own
    /// submit seam, so no second session-creation path exists.
    Ask(String),
    /// Arm the card as a session picker for a new topic: the same card, its
    /// field empty, listing only conversations (#2482).
    NewTopic,
    /// The conversation picked in the picker: the frontend opens its own
    /// topic dialog under it, the same one its key or menu item already
    /// walks (#2482).
    NewTopicOn(u128),
    /// Arm the card as a session picker for a favorite: same card, same
    /// empty field (#2487).
    Favorite,
    /// The conversation picked in the favorite picker: the frontend runs
    /// its own pin toggle on it, the same one its rail button walks (#2487).
    FavoriteToggle(u128),
}

/// Why the card is armed as a session picker. The purpose decides what a
/// picked conversation does (#2486, #2487).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PickFor {
    /// The pick names the conversation for the topic dialog.
    NewTopic,
    /// The pick pins the conversation to the top of the rail.
    Favorite,
}

/// One pickable row, already flat: the frontend paints it and nothing else.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PaletteItem {
    /// The line the eye reads and the field matches first.
    pub title: String,
    /// The muted line under the title: the command's own description, the
    /// provider for a model, the directory for a session.
    pub subtitle: String,
    pub kind: PaletteKind,
    /// Words that should find this row without being in its title. Weighted
    /// below the title, so a real name match always wins.
    pub keywords: String,
    pub action: PaletteAction,
}

impl PaletteItem {
    pub fn new(
        title: impl Into<String>,
        subtitle: impl Into<String>,
        kind: PaletteKind,
        action: PaletteAction,
    ) -> Self {
        Self {
            title: title.into(),
            subtitle: subtitle.into(),
            kind,
            keywords: String::new(),
            action,
        }
    }

    /// Add the words that should find this row by meaning rather than by name.
    pub fn with_keywords(mut self, keywords: &str) -> Self {
        self.keywords = keywords.to_string();
        self
    }
}

/// Domain words mapped onto the command that does the work. The left side is
/// what a user types when they do not know the command's name.
const KEYWORDS: &[(&str, &str)] = &[
    (
        "terminal voice local voice mic microphone dictation speech transcribe audio stt tts record",
        "/whisper",
    ),
    ("update upgrade latest version release", "/evolve"),
    (
        "rsi mc control analytics inbox dashboard",
        "/mission-control",
    ),
    ("commands keys shortcuts help what can you do", "/help"),
    ("new session new conversation fresh start topic", "/new"),
    (
        "channels telegram discord slack whatsapp trello connect setup bot",
        "/channels",
    ),
    ("cost tokens spend budget context ctx", "/usage"),
    (
        "compact trim context window summarise summarize",
        "/compact",
    ),
    ("model provider llm sonnet opus haiku", "/models"),
    ("colour color dark light palette", "/theme"),
    ("chat conversation history resume reopen", "/sessions"),
    ("skill workflow recipe", "/skills"),
    ("profile account workspace switch", "/profiles"),
];

/// Lines printed when nothing matches. Short, PG, aimed at the query or at the
/// crab, never at the user, and under 80 characters each.
const ROASTS: &[&str] = &[
    "Nothing. Not even the crab could pretend that matched.",
    "Zero hits. Bold query, terrible result.",
    "The index looked, sighed, and found nothing.",
    "No match. Try fewer letters and more hope.",
    "That query found dust. Impressive dust, but dust.",
    "Nothing here answers to that. The crab checked twice.",
    "Empty. The search equivalent of a fridge at 3am.",
    "Nope. Even fuzzy matching has standards.",
];

/// The chord that opens the card, as plain booleans so neither frontend has
/// to translate its own key type into the other's. `alt+space` is the one
/// every platform can send and every finger can reach; `cmd-k` stays as the
/// window's alias, and on macOS the command key arrives as `platform`
/// (#2458, #2491). A terminal names space `space`, the window names it the
/// same way, so both spellings are accepted here.
pub fn opens(alt: bool, platform: bool, key: &str) -> bool {
    (alt && (key == " " || key.eq_ignore_ascii_case("space")))
        || (platform && key.eq_ignore_ascii_case("k"))
}

/// The chord that opens the card in a terminal. Alt arrives there as the ESC
/// prefix, which crossterm reports as the ALT modifier on the space event, so
/// `alt+space` reaches this gate intact; the ctrl byte-collision hack of
/// #2463 is retired because the chord no longer needs it (#2491).
pub fn opens_in_terminal(alt: bool, key: &str) -> bool {
    alt && (key == " " || key.eq_ignore_ascii_case("space"))
}

/// One editing chord, translated before the GUI touches its state. Keeping
/// the reading here makes the key behavior testable without a window and
/// keeps platform chords out of the query text (#2534).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekerEdit {
    Move {
        delta: isize,
        word: bool,
        select: bool,
    },
    Home {
        select: bool,
    },
    End {
        select: bool,
    },
    Backspace {
        word: bool,
    },
    Delete {
        word: bool,
    },
    SelectAll,
    Copy,
    Cut,
    Paste,
}

/// Translate the Seeker's editing keys. Command is the primary modifier on
/// macOS; Control does the same clipboard work elsewhere. Alt or Control
/// walks words, Command+Left/Right reaches the line edges, and Shift extends
/// the selection instead of collapsing it.
pub fn editing_chord(
    key: &str,
    control: bool,
    platform: bool,
    alt: bool,
    shift: bool,
) -> Option<SeekerEdit> {
    let primary = control || platform;
    if primary && !alt {
        match key {
            "a" => return Some(SeekerEdit::SelectAll),
            "c" => return Some(SeekerEdit::Copy),
            "x" => return Some(SeekerEdit::Cut),
            "v" => return Some(SeekerEdit::Paste),
            _ => {}
        }
    }
    match key {
        "left" | "right" => {
            let delta = if key == "left" { -1 } else { 1 };
            if platform && !control && !alt {
                Some(if delta < 0 {
                    SeekerEdit::Home { select: shift }
                } else {
                    SeekerEdit::End { select: shift }
                })
            } else {
                Some(SeekerEdit::Move {
                    delta,
                    word: alt || control,
                    select: shift,
                })
            }
        }
        "home" => Some(SeekerEdit::Home { select: shift }),
        "end" => Some(SeekerEdit::End { select: shift }),
        "backspace" => Some(SeekerEdit::Backspace {
            word: alt || control,
        }),
        "delete" => Some(SeekerEdit::Delete {
            word: alt || control,
        }),
        _ => None,
    }
}

/// The line to print when the query matched nothing. Chosen by a hash of the
/// query, so the same query always gets the same line and the card does not
/// flicker while the user keeps typing.
pub fn roast(query: &str) -> &'static str {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in query.trim().to_lowercase().as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    ROASTS[(hash % ROASTS.len() as u64) as usize]
}

/// The keywords for one command, or empty when it has none.
pub fn keywords_for(command: &str) -> &'static str {
    KEYWORDS
        .iter()
        .find(|(_, name)| *name == command)
        .map(|(words, _)| *words)
        .unwrap_or("")
}

/// Score one candidate against a query, or `None` when it does not match.
///
/// Matching is a case-insensitive subsequence over characters, never bytes, so
/// a multi-byte title cannot be sliced apart. The score rewards what a reader
/// would call a better match: a prefix beats a word start, a word start beats a
/// run in the middle, and a spread-out match still counts but scores low, which
/// is what keeps `gst` finding `git status` without ranking it above an exact
/// hit.
pub fn score(query: &str, item: &PaletteItem) -> Option<u16> {
    let query = query.trim();
    if query.is_empty() {
        return Some(0);
    }
    // A phrase is matched one word at a time, and every word has to land
    // somewhere: "terminal voice" asks two things at once, so a row that only
    // knows the word "voice" is not the answer. Flattening the phrase into a
    // single subsequence (the old rule) demanded both words appear letter for
    // letter inside one field, which no keyword list can satisfy, so the query
    // that names the microphone matched nothing at all (#2461).
    //
    // Each word must START a word in the field, then may be abbreviated to a
    // subsequence ("trbl" still reaches "troubleshooting"). Without that
    // anchor a sentence like "i want to setup a browser" matched by reading
    // stray letters out of every keyword string it passed.
    let mut total: u16 = 0;
    for token in query.split_whitespace() {
        let title = match_from(token, &item.title, 10);
        let words = match_from(token, &item.keywords, 6);
        let best = match (title, words) {
            (None, None) => return None,
            (Some(t), None) => t,
            (None, Some(w)) => w,
            (Some(t), Some(w)) => t.max(w),
        };
        total = total.saturating_add(best);
    }
    Some(total)
}

/// Score one text field. The first token char has to START a word: that is
/// what turns a phrase into separate questions instead of one long blur of
/// stray letters read out of any keyword string (#2461). When no word starts
/// with the token, a full contiguous run still counts, ranked lowest: "ção"
/// lives mid-word in "Configuração" and is exactly what a user types when
/// the start of the word is not the part they remember.
fn match_from(query: &str, text: &str, weight: u16) -> Option<u16> {
    if text.is_empty() {
        return None;
    }
    let needle: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect();
    if needle.is_empty() {
        return Some(0);
    }
    let hay: Vec<char> = text.chars().flat_map(|c| c.to_lowercase()).collect();
    // The mid-word last resort: a whole run of the token, in order, ranked
    // lowest. Single chars get it too: the old substring matcher found "a"
    // mid-word in "beta" and the count test pins that, so the anchor never
    // narrows a query below what the card answered before (#2482).
    let contiguous: Option<usize> = hay
        .windows(needle.len())
        .position(|w| w == needle.as_slice());
    let mut score: i32 = 0;
    let mut hi = 0;
    let mut last_hit: Option<usize> = None;
    for (ni, want) in needle.iter().enumerate() {
        let found = if ni == 0 {
            // The first char must START a word: scan every occurrence. When
            // no word-start hit exists at all, the contiguous run is the
            // honest fallback, accepted mid-word and scored low by the normal
            // scoring below. The fallback is taken OUTSIDE the hunt: feeding
            // its position back into a hunt that had just rejected that same
            // spot looped forever on "ção" in "Configuração" (the fallback
            // always sits behind the scan cursor by definition, since the
            // cursor only advanced past rejected word starts).
            let mut from = 0usize;
            let mut word_start_hit = None;
            while let Some(p) = hay[from..].iter().position(|c| c == want) {
                let pos = p + from;
                let starts_word = pos == 0 || !hay[pos - 1].is_alphanumeric();
                if starts_word {
                    word_start_hit = Some(pos);
                    break;
                }
                from = pos + 1;
            }
            match word_start_hit {
                Some(p) => p,
                None => contiguous?,
            }
        } else {
            // Later chars have no anchoring job: first hit at or after the
            // previous one wins.
            hay[hi..].iter().position(|c| c == want)? + hi
        };
        let at_word_start = found == 0 || !hay[found - 1].is_alphanumeric();
        let consecutive = last_hit.is_some_and(|last| last + 1 == found);
        score += if at_word_start {
            8
        } else if consecutive {
            4
        } else {
            1
        };
        // A gap costs, but never enough to reject a real match.
        if let Some(last) = last_hit {
            score -= ((found - last - 1) as i32).min(6);
        }
        if ni == 0 && found == 0 {
            score += 10;
        }
        last_hit = Some(found);
        hi = found + 1;
    }
    // A whole-title prefix is the best answer there is.
    let prefix: Vec<char> = hay.iter().take(needle.len()).copied().collect();
    if prefix == needle {
        score += 12;
    }
    let scaled = (score.max(0) as u16).saturating_mul(weight);
    Some(scaled)
}

/// One ranked row, ready to paint.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PaletteRow {
    pub item: PaletteItem,
    pub score: u16,
}

/// The card's whole state: the query, the armed row, the scroll offset and the
/// items the frontend handed over when it opened.
#[derive(Clone, Debug)]
pub struct PaletteState {
    query: String,
    /// Insertion point as a character offset into `query`, never a byte
    /// offset, so editing cannot split a UTF-8 codepoint (#2534).
    caret: usize,
    /// Where a shift or mouse selection began. `None` means no selection;
    /// the live end is always `caret` (#2534).
    selection_anchor: Option<usize>,
    /// Index into `visible()`, not into `items`: the list is re-ranked on every
    /// keystroke, so a raw index would arm a different row than the one the
    /// user is looking at.
    cursor: Option<usize>,
    scroll: usize,
    items: Vec<PaletteItem>,
    /// Armed by the create-topic or favorite row: the card becomes a session
    /// picker, its field empty, conversations only (#2482, #2486, #2487).
    pick: Option<PickFor>,
    /// A kind chip pressed on the card: only rows of that kind print
    /// (#2491). None means no chip, the default.
    kind_filter: Option<PaletteKind>,
    /// When the card opened: the idle suggestions hold for
    /// `IDLE_REVEAL_DELAY` after this, so the first paint stays clean (#2491).
    opened_at: Instant,
}

/// How many rows the card prints. A list longer than this is scrolled, not
/// rendered: the card is a launcher, not a catalogue.
pub const MAX_ROWS: usize = 60;

/// Rows one Seeker page shows (#2520): the results card clips at 480px,
/// which fits about seven rows. Arrows, the wheel and PageUp/PageDown all
/// pass it as the move_by page, and the GUI prints exactly this slice of
/// the filtered list, so keys, wheel and render walk the same window.
pub const SEEKER_PAGE: usize = 7;

/// The idle suggestions wait this long after the card opens, so the first
/// paint is the field and the placeholder alone. Apple shows its shortcuts
/// only after a beat; we do the same (#2491).
pub const IDLE_REVEAL_DELAY: Duration = Duration::from_secs(3);

impl PaletteState {
    pub fn new(items: Vec<PaletteItem>) -> Self {
        Self {
            query: String::new(),
            caret: 0,
            selection_anchor: None,
            cursor: None,
            scroll: 0,
            items,
            pick: None,
            kind_filter: None,
            opened_at: Instant::now(),
        }
    }

    /// The card re-armed as a session picker: the field starts empty and
    /// every conversation prints until typing narrows the list (#2482).
    pub fn picking(purpose: PickFor, items: Vec<PaletteItem>) -> Self {
        Self {
            query: String::new(),
            caret: 0,
            selection_anchor: None,
            cursor: None,
            scroll: 0,
            items,
            pick: Some(purpose),
            kind_filter: None,
            opened_at: Instant::now(),
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// The insertion point as a character offset into [`PaletteState::query`].
    pub fn caret(&self) -> usize {
        self.caret
    }

    /// Move by whole characters and clamp at both ends. With a selection, an
    /// ordinary arrow collapses to its near edge, as the composer does.
    /// Movement alone never re-ranks the result list (#2534).
    pub fn move_caret(&mut self, delta: isize) {
        if let Some((start, end)) = self.selection() {
            if delta != 0 {
                self.caret = if delta < 0 { start } else { end };
            }
            self.collapse();
            return;
        }
        let len = self.query.chars().count();
        self.caret = if delta < 0 {
            self.caret.saturating_sub(delta.unsigned_abs())
        } else {
            self.caret.saturating_add(delta.unsigned_abs()).min(len)
        };
        self.collapse();
    }

    /// Move to an absolute character offset and collapse any selection.
    /// Home, End and mouse clicks use the same clamped landing rule.
    pub fn move_caret_to(&mut self, offset: usize) {
        self.caret = offset.min(self.query.chars().count());
        self.collapse();
    }

    /// Move by the same word boundaries as the composer. The engine stores
    /// chars, while the shared word walk speaks bytes, so convert at the seam.
    pub fn move_caret_word(&mut self, delta: isize) {
        let byte_caret = char_to_byte(&self.query, self.caret);
        let target = if delta < 0 {
            previous_word_start(&self.query, byte_caret)
        } else if delta > 0 {
            next_word_end(&self.query, byte_caret)
        } else {
            byte_caret
        };
        self.caret = byte_to_char(&self.query, target);
        self.collapse();
    }

    /// Extend a selection to an absolute character offset. The first call
    /// anchors at the current caret; later calls keep that anchor, including
    /// when the direction reverses (#2534).
    pub fn select_to(&mut self, offset: usize) {
        let len = self.query.chars().count();
        let anchor = *self.selection_anchor.get_or_insert(self.caret);
        self.caret = offset.min(len);
        if self.caret == anchor {
            self.selection_anchor = None;
        }
    }

    /// Extend the selection by one composer word boundary. The original
    /// anchor survives repeated presses and direction reversals (#2534).
    pub fn select_word_to(&mut self, delta: isize) {
        let byte_caret = char_to_byte(&self.query, self.caret);
        let target = if delta < 0 {
            previous_word_start(&self.query, byte_caret)
        } else if delta > 0 {
            next_word_end(&self.query, byte_caret)
        } else {
            byte_caret
        };
        self.select_to(byte_to_char(&self.query, target));
    }

    /// Select the whole query, ready for copy, cut, or replacement typing.
    pub fn select_all(&mut self) {
        self.selection_anchor = Some(0);
        self.caret = self.query.chars().count();
    }

    /// Drop the selection without moving the caret or touching the ranking.
    pub fn collapse(&mut self) {
        self.selection_anchor = None;
    }

    /// Selected character offsets in ascending order. An empty selection is
    /// no selection, so editing falls back to the caret.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        (anchor != self.caret).then_some((anchor.min(self.caret), anchor.max(self.caret)))
    }

    /// The selected text without changing anything, for copy.
    pub fn selected_text(&self) -> Option<&str> {
        let (start, end) = self.selection()?;
        let start = char_to_byte(&self.query, start);
        let end = char_to_byte(&self.query, end);
        self.query.get(start..end)
    }

    /// Text slices before the caret or selection, inside the selection, and
    /// after it. With no selection the middle slice is empty and the split is
    /// the caret, which lets a frontend paint all three without UTF-8 math.
    pub fn render_spans(&self) -> (&str, &str, &str) {
        let (start, end) = self.selection().unwrap_or((self.caret, self.caret));
        let start = char_to_byte(&self.query, start);
        let end = char_to_byte(&self.query, end);
        (
            self.query.get(..start).unwrap_or(""),
            self.query.get(start..end).unwrap_or(""),
            self.query.get(end..).unwrap_or(""),
        )
    }

    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Whether a session-pick purpose is armed: the chips hide while it is,
    /// because a narrowed conversation list has no kinds to filter (#2491).
    pub fn pick(&self) -> Option<PickFor> {
        self.pick
    }

    /// The kind chip the card is narrowed to, if any (#2491).
    pub fn kind_filter(&self) -> Option<PaletteKind> {
        self.kind_filter
    }

    /// Press or release a kind chip. The list reshapes under the cursor, so
    /// the cursor disarms and the best row of the narrowed list becomes the
    /// one Enter acts on.
    pub fn set_kind_filter(&mut self, kind: Option<PaletteKind>) {
        self.kind_filter = kind;
        self.cursor = None;
        self.scroll = 0;
    }

    /// Arm a row by its index in [`PaletteState::visible`], clamped to the
    /// list. A click uses this, so the row it landed on is the row Enter and
    /// the arrows then work from.
    pub fn arm(&mut self, row: usize) {
        let len = self.visible().len();
        if len == 0 {
            self.cursor = None;
            return;
        }
        self.cursor = Some(row.min(len - 1));
    }

    /// Replace a character range and put the caret after the replacement.
    /// Every real edit funnels through here, so every edit also disarms the
    /// old result row and returns the list to its first page (#2534).
    fn replace_range(&mut self, start: usize, end: usize, replacement: &str) {
        let len = self.query.chars().count();
        let start = start.min(len);
        let end = end.min(len).max(start);
        let byte_start = char_to_byte(&self.query, start);
        let byte_end = char_to_byte(&self.query, end);
        self.query.replace_range(byte_start..byte_end, replacement);
        self.caret = start + replacement.chars().count();
        self.selection_anchor = None;
        self.cursor = None;
        self.scroll = 0;
    }

    /// Insert text at the caret, replacing any selection. Paste uses the same
    /// path as typing, so both frontends inherit identical editing (#2534).
    pub fn insert_text(&mut self, text: &str) {
        let (start, end) = self.selection().unwrap_or((self.caret, self.caret));
        if text.is_empty() && start == end {
            return;
        }
        self.replace_range(start, end, text);
    }

    /// A typed character inserts at the caret rather than always appending.
    pub fn type_char(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        let mut encoded = [0; 4];
        let text = c.encode_utf8(&mut encoded);
        self.insert_text(text);
    }

    /// Remove the selection, if one exists. Returns without touching the
    /// ranking when there is nothing to delete.
    pub fn delete_selection(&mut self) {
        if let Some((start, end)) = self.selection() {
            self.replace_range(start, end, "");
        }
    }

    /// Remove and return the selected text, for cut. Copy uses
    /// [`PaletteState::selected_text`] and leaves the query alone.
    pub fn take_selected_text(&mut self) -> Option<String> {
        let taken = self.selected_text().map(str::to_owned);
        if taken.is_some() {
            self.delete_selection();
        }
        taken
    }

    pub fn backspace(&mut self) {
        if self.selection().is_some() {
            self.delete_selection();
        } else if self.caret > 0 {
            self.replace_range(self.caret - 1, self.caret, "");
        }
    }

    /// Delete the character after the caret, or the whole selection.
    pub fn delete_forward(&mut self) {
        if self.selection().is_some() {
            self.delete_selection();
        } else if self.caret < self.query.chars().count() {
            self.replace_range(self.caret, self.caret + 1, "");
        }
    }

    /// Delete back to the previous composer word boundary.
    pub fn delete_word_back(&mut self) {
        if self.selection().is_some() {
            self.delete_selection();
            return;
        }
        let byte_caret = char_to_byte(&self.query, self.caret);
        let start = byte_to_char(&self.query, previous_word_start(&self.query, byte_caret));
        if start < self.caret {
            self.replace_range(start, self.caret, "");
        }
    }

    /// Delete forward through the next composer word boundary.
    pub fn delete_word_forward(&mut self) {
        if self.selection().is_some() {
            self.delete_selection();
            return;
        }
        let byte_caret = char_to_byte(&self.query, self.caret);
        let end = byte_to_char(&self.query, next_word_end(&self.query, byte_caret));
        if end > self.caret {
            self.replace_range(self.caret, end, "");
        }
    }

    /// Clear the query but keep the card open. Esc closes the card, so this is
    /// the key that clears the field without leaving.
    pub fn clear(&mut self) {
        self.query.clear();
        self.caret = 0;
        self.selection_anchor = None;
        self.cursor = None;
        self.scroll = 0;
    }

    /// Move the highlight. The first press arms the top row, so the arrows work
    /// from the field without a click first. `visible` is bounded, so the
    /// cursor can never point past the last printed row.
    pub fn move_by(&mut self, delta: i32, page: usize) {
        let len = self.visible().len();
        if len == 0 {
            self.cursor = None;
            return;
        }
        let current = self.cursor;
        let next = match current {
            None => {
                if delta < 0 {
                    len - 1
                } else {
                    0
                }
            }
            Some(ix) => {
                let moved = ix as i32 + delta;
                moved.clamp(0, len as i32 - 1) as usize
            }
        };
        self.cursor = Some(next);
        self.scroll = keep_in_view(self.scroll, next, page);
    }

    /// The rows the card prints, best first. Deterministic: score, then kind,
    /// then title, so two frontends cannot disagree about the order. With
    /// nothing typed the kind step is the idle rank instead: destinations
    /// lead, commands follow, chats last (#2491).
    /// Whether the idle suggestions may print yet: the card has been open
    /// for `IDLE_REVEAL_DELAY` with nothing typed and no picker armed. The
    /// frontends gate the idle block on this; the ranker stays clock-free so
    /// its tests stay deterministic (#2491).
    pub fn idle_ready(&self, now: Instant) -> bool {
        self.query.trim().is_empty()
            && self.pick.is_none()
            && now.duration_since(self.opened_at) >= IDLE_REVEAL_DELAY
    }

    pub fn visible(&self) -> Vec<PaletteRow> {
        let mut rows: Vec<PaletteRow> = self
            .items
            .iter()
            .filter_map(|item| {
                // Picker mode lists conversations only: the field starts
                // empty, so the whole rail prints until typing narrows it
                // (#2482).
                if self.pick.is_some() && item.kind != PaletteKind::Session {
                    return None;
                }
                // A pressed chip drops every row of another kind (#2491).
                if let Some(wanted) = self.kind_filter
                    && item.kind != wanted
                {
                    return None;
                }
                let mut item = item.clone();
                // A pick names the conversation for the armed purpose: the
                // topic dialog or the pin toggle, never opening the chat
                // (#2482, #2486, #2487).
                if let Some(purpose) = self.pick
                    && let PaletteAction::OpenSession(id) = item.action
                {
                    item.action = match purpose {
                        PickFor::NewTopic => PaletteAction::NewTopicOn(id),
                        PickFor::Favorite => PaletteAction::FavoriteToggle(id),
                    };
                }
                score(&self.query, &item).map(|score| PaletteRow { item, score })
            })
            .collect();
        // Which kind rank decides: the typed card ranks actions first, the
        // idle card leads with destinations (#2491).
        let idle = self.query.trim().is_empty();
        let kind_rank = |k: PaletteKind| if idle { k.idle_rank() } else { k.rank() };
        rows.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then(kind_rank(a.item.kind).cmp(&kind_rank(b.item.kind)))
                .then(
                    a.item
                        .title
                        .to_lowercase()
                        .cmp(&b.item.title.to_lowercase()),
                )
        });
        // The idle card trades the brain and compact commands for the
        // surfaces that already carry them (usage, mission control); the
        // reveal stays the full default list, curated, so nothing the old
        // card showed goes missing (#2491). Every state shares one cap.
        if idle && self.pick.is_none() && self.kind_filter.is_none() {
            rows.retain(|row| {
                !(row.item.kind == PaletteKind::Command
                    && matches!(row.item.title.as_str(), "/brain" | "/compact"))
            });
        }
        rows.truncate(MAX_ROWS);
        // A real query that matched nothing still deserves an answer: one row
        // that starts a fresh session carrying the typed text (#2482). The
        // roast rides along as the subtitle, so the miss keeps its sense of
        // humour while doing something useful.
        if rows.is_empty()
            && !self.query.trim().is_empty()
            && self.pick.is_none()
            && self.kind_filter.is_none()
        {
            let query = self.query.clone();
            rows.push(PaletteRow {
                item: PaletteItem {
                    title: query.clone(),
                    subtitle: roast(&query).to_string(),
                    kind: PaletteKind::Ask,
                    keywords: String::new(),
                    action: PaletteAction::Ask(query),
                },
                score: 0,
            });
        }
        rows
    }

    /// The row Enter acts on: the armed one, or the best match when the user
    /// has not stepped into the list yet.
    pub fn selected(&self) -> Option<PaletteRow> {
        let rows = self.visible();
        match self.cursor {
            Some(ix) => rows.get(ix).cloned(),
            None => rows.first().cloned(),
        }
    }
}

/// Scroll the window so `row` is inside it, keeping the card's height as the
/// visible slice.
fn keep_in_view(scroll: usize, row: usize, page: usize) -> usize {
    let page = page.max(1);
    if row < scroll {
        row
    } else if row >= scroll + page {
        row + 1 - page
    } else {
        scroll
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn command(name: &'static str) -> PaletteItem {
        PaletteItem::new(
            name,
            "a command",
            PaletteKind::Command,
            PaletteAction::RunSlash(name),
        )
        .with_keywords(keywords_for(name))
    }

    fn item(title: &str, kind: PaletteKind) -> PaletteItem {
        PaletteItem::new(title, "", kind, PaletteAction::OpenSurface(Surface::Help))
    }

    /// The whole point of the keyword table: the user types the word for the
    /// job, not the name of the command (#2458).
    #[test]
    fn domain_words_find_the_command_that_does_the_job() {
        let items = vec![
            command("/whisper"),
            command("/evolve"),
            command("/mission-control"),
            command("/help"),
        ];
        let state = PaletteState::new(items);
        for (query, want) in [
            ("voice", "/whisper"),
            ("upgrade", "/evolve"),
            ("latest", "/evolve"),
            ("rsi", "/mission-control"),
            ("mc", "/mission-control"),
            ("commands", "/help"),
        ] {
            let mut state = PaletteState::new(state.items.clone());
            for c in query.chars() {
                state.type_char(c);
            }
            let top = state.visible().first().cloned().expect("a match");
            assert_eq!(
                top.item.action,
                PaletteAction::RunSlash(want),
                "{query} must find {want}"
            );
        }
    }

    /// A real name match beats a keyword match. Typing `/he` must not put
    /// `/whisper` on top because its keywords mention "help".
    #[test]
    fn a_name_match_outranks_a_keyword_match() {
        let state = PaletteState::new(vec![command("/help"), command("/whisper")]);
        let mut state = state;
        state.type_char('h');
        state.type_char('e');
        assert_eq!(
            state.selected().map(|row| row.item.action),
            Some(PaletteAction::RunSlash("/help"))
        );
    }

    /// Acronyms count, which is what makes the card faster than scrolling a
    /// list of names.
    #[test]
    fn initials_match_a_multi_word_title() {
        let target = item("git status", PaletteKind::Surface);
        assert!(score("gst", &target).is_some(), "gst must find git status");
        assert!(
            score("zzz", &target).is_none(),
            "a query with no letters in order must not match"
        );
        // Spread matches score below an exact prefix.
        let exact = item("theme", PaletteKind::Command);
        let spread = item("tiger hunt echo", PaletteKind::Command);
        assert!(
            score("the", &exact).unwrap() > score("the", &spread).unwrap(),
            "a prefix must outscore a spread match"
        );
    }

    /// Order is a contract between the two frontends. Idle, the card is a
    /// menu of destinations: surfaces lead, then the action rows, then
    /// commands, conversations last, whatever the titles spell (#2491).
    /// Typed, score wins first and the kind only breaks ties.
    #[test]
    fn idle_orders_destinations_first_and_typed_orders_score_first() {
        let state = PaletteState::new(vec![
            item("zebra", PaletteKind::Session),
            item("alpha", PaletteKind::Surface),
            item("beta", PaletteKind::Command),
            item("gamma", PaletteKind::Surface),
        ]);
        let titles: Vec<String> = state
            .visible()
            .into_iter()
            .map(|row| row.item.title)
            .collect();
        assert_eq!(
            titles,
            vec!["alpha", "gamma", "beta", "zebra"],
            "an idle card leads with destinations, not a command dump"
        );

        // Typed: two rows with the same title and score are split by the
        // typed kind rank, an action before a conversation.
        let mut state = PaletteState::new(vec![
            item("alpha", PaletteKind::Session),
            item("alpha", PaletteKind::Command),
        ]);
        state.type_char('a');
        let kinds: Vec<PaletteKind> = state
            .visible()
            .into_iter()
            .map(|row| row.item.kind)
            .collect();
        assert_eq!(kinds, vec![PaletteKind::Command, PaletteKind::Session]);
    }

    /// The idle suggestions hold for three seconds after the card opens,
    /// so the first paint is the field and the placeholder alone (#2491).
    #[test]
    fn the_idle_suggestions_wait_three_seconds_before_revealing() {
        let state = PaletteState::new(vec![item("alpha", PaletteKind::Surface)]);
        assert!(
            !state.idle_ready(Instant::now()),
            "a freshly opened card keeps its suggestions hidden"
        );
        assert!(
            state.idle_ready(Instant::now() + IDLE_REVEAL_DELAY + Duration::from_millis(1)),
            "three quiet seconds reveal the suggestions"
        );
    }

    /// The idle card trades the brain and compact commands for the surfaces
    /// that already carry them: usage and mission control lead instead
    /// (#2491).
    #[test]
    fn the_idle_card_swaps_brain_and_compact_for_their_surfaces() {
        let state = PaletteState::new(vec![
            command("/brain"),
            command("/compact"),
            command("/cd"),
            item("/usage", PaletteKind::Surface),
        ]);
        let titles: Vec<String> = state
            .visible()
            .into_iter()
            .map(|row| row.item.title)
            .collect();
        assert!(
            titles.contains(&"/usage".to_string()),
            "the usage surface stays on the idle card"
        );
        assert!(
            !titles.contains(&"/brain".to_string()),
            "brain is redundant with the usage surface"
        );
        assert!(
            !titles.contains(&"/compact".to_string()),
            "compact is redundant with mission control"
        );
        assert!(titles.contains(&"/cd".to_string()), "other commands stay");
    }

    /// The action rows sit between the surfaces and the commands on the idle
    /// card, so create topic and favorite are one glance away (#2491).
    #[test]
    fn the_idle_card_sits_action_rows_between_surfaces_and_commands() {
        let state = PaletteState::new(vec![
            item("/help", PaletteKind::Command),
            item("create topic", PaletteKind::NewTopic),
            item("Sessions", PaletteKind::Surface),
            item("favorite any session", PaletteKind::Favorite),
        ]);
        let titles: Vec<String> = state
            .visible()
            .into_iter()
            .map(|row| row.item.title)
            .collect();
        assert_eq!(
            titles,
            vec!["Sessions", "create topic", "favorite any session", "/help"]
        );
    }

    /// A kind chip narrows the card to that kind, pressing it again (None)
    /// gives the whole list back, and the cursor disarms either way so
    /// Enter acts on the narrowed list's best row (#2491).
    #[test]
    fn a_kind_chip_narrows_the_card_to_that_kind() {
        let mut state = PaletteState::new(vec![
            item("/help", PaletteKind::Command),
            item("Themes", PaletteKind::Surface),
            item("Seeker", PaletteKind::Docs),
        ]);
        state.arm(2);
        assert_eq!(state.cursor(), Some(2));

        state.set_kind_filter(Some(PaletteKind::Docs));
        let printed = state.visible();
        assert_eq!(printed.len(), 1, "the chip keeps only its own kind");
        assert_eq!(printed[0].item.kind, PaletteKind::Docs);
        assert_eq!(state.cursor(), None, "a chip press disarms the cursor");

        state.set_kind_filter(None);
        assert_eq!(state.visible().len(), 3, "releasing the chip restores all");
    }

    /// A miss while a chip is active shows an empty list, never the ask row:
    /// the chip owns what the query means, so offering a fresh session
    /// would answer a question nobody asked (#2491).
    #[test]
    fn the_ask_row_stays_quiet_while_a_chip_filters() {
        let mut state = PaletteState::new(vec![item("/help", PaletteKind::Command)]);
        state.set_kind_filter(Some(PaletteKind::Docs));
        for c in "zzz".chars() {
            state.type_char(c);
        }
        assert!(
            state.visible().is_empty(),
            "a filtered miss stays empty, no ask row"
        );
    }

    /// The cursor starts unarmed so Enter acts on the best match, and the
    /// first arrow press steps into the list. Up from nothing lands on the
    /// last row, the way a menu wraps at the ends.
    #[test]
    fn the_first_arrow_arms_the_list_and_the_cursor_clamps() {
        let mut state = PaletteState::new(vec![
            item("one", PaletteKind::Command),
            item("two", PaletteKind::Command),
        ]);
        assert_eq!(state.cursor(), None);
        state.move_by(-1, 10);
        assert_eq!(state.cursor(), Some(1), "up from nothing lands on the last");
        state.move_by(1, 10);
        assert_eq!(state.cursor(), Some(1), "the cursor stops at the last row");
        state.move_by(-5, 10);
        assert_eq!(state.cursor(), Some(0), "and at the first");

        // A new query re-arms the best row rather than keeping a stale index.
        state.type_char('t');
        assert_eq!(state.cursor(), None);
    }

    /// Scrolling keeps the armed row on screen, so the printed window is the
    /// one the user is stepping through.
    #[test]
    fn scrolling_follows_the_armed_row() {
        let mut state = PaletteState::new(
            (0..10)
                .map(|ix| item(&format!("row{ix}"), PaletteKind::Command))
                .collect(),
        );
        for _ in 0..7 {
            state.move_by(1, 3);
        }
        assert_eq!(state.cursor(), Some(6));
        assert_eq!(state.scroll(), 4, "rows 4..7 are the visible window");
    }

    /// Nothing matched: the ask row carries the query and the roast is its
    /// subtitle. The line is stable for a query, so it does not flicker as
    /// the user types, and it never names the user.
    #[test]
    fn no_match_keeps_a_stable_short_line() {
        let mut state = PaletteState::new(vec![command("/help")]);
        for c in "qwx".chars() {
            state.type_char(c);
        }
        let rows = state.visible();
        assert_eq!(rows.len(), 1, "the miss offers exactly the ask row");
        assert_eq!(
            state.selected().map(|row| row.item.kind),
            Some(PaletteKind::Ask)
        );
        assert_eq!(rows[0].item.subtitle, roast("qwx"));

        let line = roast("qwx");
        assert_eq!(line, roast("qwx"), "the same query gets the same line");
        assert_eq!(line, roast("QWX"), "case does not change it");
        assert!(line.chars().count() < 80, "the line must fit one row");
        assert!(line.is_ascii(), "the card prints plain punctuation only");
    }

    /// A query nothing matches offers a fresh session carrying it: exactly one
    /// row, the typed text on it, Enter sends it (#2482).
    #[test]
    fn seeker_no_match_offers_a_new_session_carrying_the_query() {
        let mut state = PaletteState::new(vec![command("/whisper")]);
        let query = "I want to set up a browser automation";
        for c in query.chars() {
            state.type_char(c);
        }
        let rows = state.visible();
        assert_eq!(rows.len(), 1, "a miss must offer exactly the ask row");
        assert_eq!(rows[0].item.kind, PaletteKind::Ask);
        assert_eq!(rows[0].item.title, query);
        assert_eq!(rows[0].item.action, PaletteAction::Ask(query.to_string()));
    }

    /// A query with a real match never grows the ask row.
    #[test]
    fn seeker_no_match_row_stays_hidden_when_something_matches() {
        let mut state = PaletteState::new(vec![command("/whisper")]);
        for c in "whisper".chars() {
            state.type_char(c);
        }
        let rows = state.visible();
        assert!(rows.iter().all(|row| row.item.kind != PaletteKind::Ask));
    }

    /// The favorite row answers to the words a user types (#2487).
    #[test]
    fn seeker_favorite_row_answers_to_the_words() {
        let row = favorite_item();
        assert_eq!(row.kind, PaletteKind::Favorite);
        assert_eq!(row.action, PaletteAction::Favorite);
        let mut state = PaletteState::new(vec![
            command("/new"),
            row,
            item("topics we made", PaletteKind::Session),
        ]);
        for c in "favorite".chars() {
            state.type_char(c);
        }
        assert_eq!(
            state.selected().map(|r| r.item.kind),
            Some(PaletteKind::Favorite),
            "the row prints on top"
        );
    }

    /// Picking the favorite row arms the same session picker, and the pick
    /// carries the pin toggle instead of opening the chat (#2487).
    #[test]
    fn seeker_favorite_pick_toggles_the_picked_session() {
        let sessions = vec![PaletteItem::new(
            "invoices",
            "2026-11-02 21:00",
            PaletteKind::Session,
            PaletteAction::OpenSession(2),
        )];
        let mut state = PaletteState::picking(PickFor::Favorite, sessions);
        for c in "inv".chars() {
            state.type_char(c);
        }
        assert_eq!(
            state.selected().map(|r| r.item.action),
            Some(PaletteAction::FavoriteToggle(2)),
            "the pick pins the session, it does not open the chat"
        );
    }

    /// The create-topic row answers to the words a user types, over anything
    /// else naming a session (#2482).
    #[test]
    fn seeker_create_topic_row_answers_to_the_words() {
        let row = new_topic_item();
        assert_eq!(row.kind, PaletteKind::NewTopic);
        assert_eq!(row.action, PaletteAction::NewTopic);
        let mut state = PaletteState::new(vec![
            command("/new"),
            row,
            item("topics we made", PaletteKind::Session),
        ]);
        for c in "create topic".chars() {
            state.type_char(c);
        }
        assert_eq!(
            state.selected().map(|r| r.item.kind),
            Some(PaletteKind::NewTopic),
            "the row prints on top"
        );
    }

    /// Picking the row re-arms the card as a picker: empty field, every
    /// conversation, typing narrows, and a pick names the session for the
    /// topic dialog instead of opening the chat. A miss in the picker stays
    /// silent; it must not offer a fresh session mid-pick (#2482).
    #[test]
    fn seeker_create_topic_pick_names_the_session_for_the_dialog() {
        let sessions = vec![
            PaletteItem::new(
                "crabsland ops",
                "2026-11-03 08:00",
                PaletteKind::Session,
                PaletteAction::OpenSession(1),
            ),
            PaletteItem::new(
                "invoices",
                "2026-11-02 21:00",
                PaletteKind::Session,
                PaletteAction::OpenSession(2),
            ),
        ];
        let mut state = PaletteState::picking(PickFor::NewTopic, sessions);
        assert_eq!(
            state.visible().len(),
            2,
            "an empty field prints every conversation"
        );
        for c in "inv".chars() {
            state.type_char(c);
        }
        assert_eq!(
            state.selected().map(|r| r.item.action),
            Some(PaletteAction::NewTopicOn(2)),
            "the pick names the session, it does not open the chat"
        );
        for c in "zzz".chars() {
            state.type_char(c);
        }
        assert!(
            state.visible().is_empty(),
            "a miss in the picker stays silent"
        );
    }

    /// Titles are matched character by character, so a multi-byte title is
    /// scored instead of slicing a character in half.
    #[test]
    fn multi_byte_titles_are_safe() {
        let target = item("Configuração do projeto", PaletteKind::Session);
        assert!(score("config", &target).is_some());
        assert!(score("ção", &target).is_some());
        assert!(score("zzz", &target).is_none());
    }

    /// The idle card opens on the curated menu: destinations lead, the two
    /// redundant command rows are gone (their screens carry them), chats and
    /// docs trail, and the print stays bounded by the one catalogue cap
    /// (#2491).
    #[test]
    fn an_empty_query_lists_the_curated_idle_menu() {
        let mut items: Vec<PaletteItem> = Vec::new();
        for name in ["/brain", "/compact", "/help", "/evolve"] {
            items.push(command(name));
        }
        items.push(item("usage", PaletteKind::Surface));
        items.push(item("sessions", PaletteKind::Surface));
        items.push(item("docs page", PaletteKind::Docs));
        items.push(item("chat one", PaletteKind::Session));
        let state = PaletteState::new(items);
        let titles: Vec<String> = state
            .visible()
            .into_iter()
            .map(|row| row.item.title)
            .collect();
        // Eight rows went in; brain and compact do not print on idle.
        assert_eq!(titles.len(), 6, "brain and compact are dropped on idle");
        assert!(!titles.contains(&"/brain".to_string()));
        assert!(!titles.contains(&"/compact".to_string()));
        // Destinations lead; the surface tie breaks by title.
        assert_eq!(titles[0], "sessions");
        assert_eq!(titles[1], "usage");
        // Then commands, docs, chats, in idle_rank order.
        assert_eq!(titles[2], "/evolve");
        assert_eq!(titles[3], "/help");
        assert_eq!(titles[4], "docs page");
        assert_eq!(titles[5], "chat one");
        assert_eq!(
            state.selected().map(|row| row.item.title.clone()),
            Some("sessions".to_string()),
            "Enter with nothing typed acts on the first destination"
        );
    }

    /// One chord table for both clients: alt+space everywhere, cmd-k in the
    /// window, and nothing else opens the card.
    #[test]
    fn only_the_two_named_chords_open_the_card() {
        assert!(opens(true, false, " "));
        assert!(opens(true, false, "space"));
        assert!(opens(false, true, "k"));
        assert!(!opens(false, false, " "));
        assert!(!opens(true, false, "k"));
        assert!(!opens(true, false, "f"));
        assert!(!opens(false, true, " "));
    }

    /// The terminal gets the bare chord: crossterm reports the ESC prefix of
    /// alt+space as the ALT modifier on the space event, so the gate sees the
    /// chord intact (#2491).
    #[test]
    fn the_terminal_chord_is_alt_space() {
        assert!(opens_in_terminal(true, " "));
        assert!(opens_in_terminal(true, "space"));
        assert!(!opens_in_terminal(false, " "));
        assert!(!opens_in_terminal(true, "f"));
    }

    /// Every destination prints the bare command that opens it, so no row
    /// carries a screen name the user never typed (#2491).
    #[test]
    fn every_surface_names_the_command_that_opens_it() {
        let items = surface_items();
        assert_eq!(items.len(), SURFACES.len());
        for surface in SURFACES {
            let command = surface.command().expect("every surface names a command");
            assert!(command.starts_with('/'), "{command}");
            assert_eq!(surface.title(), command);
        }
        for item in items {
            assert_eq!(item.kind, PaletteKind::Surface);
            assert!(item.title.starts_with('/'), "{}", item.title);
        }
    }

    /// A click arms the row it landed on, and Enter then runs that row. The
    /// window sends every click through this path so a click and a key press
    /// can never act on different rows (#2458).
    #[test]
    fn a_click_arms_the_row_it_landed_on() {
        let mut state = PaletteState::new((0..5).map(|_| command("/cmd")).collect());
        state.arm(3);
        assert_eq!(state.cursor(), Some(3));
        assert_eq!(
            state.selected().map(|row| row.item.title),
            Some("/cmd".to_string())
        );

        // A click below the last printed row still arms something real.
        state.arm(99);
        assert_eq!(state.cursor(), Some(state.visible().len() - 1));

        // Typing disarms, so Enter goes back to the best match.
        state.type_char('c');
        assert_eq!(state.cursor(), None);
    }

    /// The space is part of a phrase, not a hotkey: "mission control" has to
    /// find /mission-control, and typing it must not split the query.
    #[test]
    fn spaces_in_a_query_are_ignored_for_matching() {
        let mut state = PaletteState::new(vec![command("/mission-control")]);
        for c in "mission control".chars() {
            state.type_char(c);
        }
        assert_eq!(state.query(), "mission control");
        assert_eq!(
            state.selected().map(|row| row.item.action),
            Some(PaletteAction::RunSlash("/mission-control"))
        );
    }

    /// Every channel gets a row that starts its setup, titled with the name the
    /// user types, so the row and the docs page answer to the same word (#2462).
    #[test]
    fn every_channel_gets_one_setup_row() {
        let rows = channel_items();
        assert_eq!(rows.len(), Channel::ALL.len());
        for (row, channel) in rows.iter().zip(Channel::ALL) {
            assert_eq!(row.kind, PaletteKind::Channel);
            assert_eq!(row.title, channel.name());
            assert_eq!(row.action, PaletteAction::SetupChannel(*channel));
            assert!(
                row.subtitle.contains("set up") || row.subtitle.contains("scan"),
                "{} needs its steps, got {:?}",
                channel.name(),
                row.subtitle
            );
        }
    }

    /// The whole point: type the channel and the setup row is the first thing
    /// Enter acts on, above the docs page that shares its name.
    #[test]
    fn typing_a_channel_arms_its_setup_row() {
        let mut items = channel_items();
        items.extend(crate::ui_core::docs::items());
        let mut state = PaletteState::new(items);
        for c in "telegram".chars() {
            state.type_char(c);
        }
        assert_eq!(
            state.selected().map(|row| row.item.action),
            Some(PaletteAction::SetupChannel(Channel::Telegram))
        );
    }

    /// The phrases a person actually types on the way to a page, a command or
    /// a channel (#2461). A multi-word query used to be flattened into one
    /// subsequence, so "terminal voice" had to appear letter for letter inside
    /// a single field and matched nothing at all.
    #[test]
    fn a_phrase_arms_the_row_that_answers_it() {
        fn all_items() -> Vec<PaletteItem> {
            let mut items: Vec<PaletteItem> = crate::ui_core::commands::SLASH_COMMANDS
                .iter()
                .map(|cmd| {
                    PaletteItem::new(
                        cmd.name,
                        cmd.description,
                        PaletteKind::Command,
                        PaletteAction::RunSlash(cmd.name),
                    )
                    .with_keywords(keywords_for(cmd.name))
                })
                .collect();
            items.extend(surface_items());
            items.extend(crate::ui_core::docs::items());
            items.extend(channel_items());
            items
        }

        fn armed(query: &str) -> Option<PaletteAction> {
            let mut state = PaletteState::new(all_items());
            for c in query.chars() {
                state.type_char(c);
            }
            state.selected().map(|row| row.item.action)
        }

        fn titles(query: &str) -> Vec<String> {
            let mut state = PaletteState::new(all_items());
            for c in query.chars() {
                state.type_char(c);
            }
            state
                .visible()
                .into_iter()
                .map(|row| row.item.title)
                .collect()
        }

        assert_eq!(
            armed("terminal voice"),
            Some(PaletteAction::RunSlash("/whisper")),
            "the phrase that names the microphone has to arm it"
        );
        assert_eq!(
            armed("upgrade"),
            Some(PaletteAction::RunSlash("/evolve")),
            "an answer about upgrading is the updater's row"
        );
        assert_eq!(
            armed("daemon troubleshooting"),
            Some(PaletteAction::OpenDocs(
                "/getting-started/troubleshooting.html"
            )),
            "a daemon problem is a troubleshooting page"
        );
        assert_eq!(
            armed("automation every morning"),
            Some(PaletteAction::OpenDocs("/features/cron-jobs.html")),
            "a daily automation is the cron page"
        );
        let trello = titles("trello");
        assert!(
            trello.contains(&"Trello".to_string()),
            "the setup row has to be reachable: {trello:?}"
        );
    }

    /// The owner's command queries, as tests (#2482): the words a person
    /// types without knowing a command's name still have to arm exactly one
    /// row, and every channel name has to reach its setup row and its docs
    /// page at once, because "how do I connect telegram" is both a question
    /// about the channel and about the docs that explain it.
    #[test]
    fn seeker_phrase_reaches_the_commands_the_owner_named() {
        fn all_items() -> Vec<PaletteItem> {
            let mut items: Vec<PaletteItem> = crate::ui_core::commands::SLASH_COMMANDS
                .iter()
                .map(|cmd| {
                    PaletteItem::new(
                        cmd.name,
                        cmd.description,
                        PaletteKind::Command,
                        PaletteAction::RunSlash(cmd.name),
                    )
                    .with_keywords(keywords_for(cmd.name))
                })
                .collect();
            items.extend(surface_items());
            items.extend(crate::ui_core::docs::items());
            items.extend(channel_items());
            items
        }

        fn state_for(query: &str) -> PaletteState {
            let mut state = PaletteState::new(all_items());
            for c in query.chars() {
                state.type_char(c);
            }
            state
        }

        fn armed(query: &str) -> Option<PaletteAction> {
            state_for(query).selected().map(|row| row.item.action)
        }

        fn actions(query: &str) -> Vec<PaletteAction> {
            state_for(query)
                .visible()
                .into_iter()
                .map(|row| row.item.action)
                .collect()
        }

        assert_eq!(
            armed("local voice"),
            Some(PaletteAction::RunSlash("/whisper")),
            "the other way of naming the microphone lands on the same row"
        );
        assert_eq!(
            armed("update"),
            Some(PaletteAction::RunSlash("/evolve")),
            "update and upgrade are one intent: the updater"
        );
        for (name, channel) in [
            ("telegram", Channel::Telegram),
            ("discord", Channel::Discord),
            ("slack", Channel::Slack),
            ("whatsapp", Channel::WhatsApp),
        ] {
            let found = actions(name);
            assert!(
                found.contains(&PaletteAction::SetupChannel(channel)),
                "{name} has to reach its setup row: {found:?}"
            );
            assert!(
                found.contains(&PaletteAction::OpenDocs(channel.doc_path())),
                "{name} has to reach its docs page next to the setup row: {found:?}"
            );
        }
    }

    /// Every docs row opens the official site, page level (#2482): the Seeker
    /// carries the 32 pages and links out, so the assertion is the URL, not a
    /// local file. The daemon query is the one from the bug report screenshot,
    /// where the site's own search matched a section and the Seeker did not.
    #[test]
    fn seeker_docs_open_the_official_pages() {
        fn armed(query: &str) -> Option<PaletteAction> {
            let mut items: Vec<PaletteItem> = crate::ui_core::commands::SLASH_COMMANDS
                .iter()
                .map(|cmd| {
                    PaletteItem::new(
                        cmd.name,
                        cmd.description,
                        PaletteKind::Command,
                        PaletteAction::RunSlash(cmd.name),
                    )
                    .with_keywords(keywords_for(cmd.name))
                })
                .collect();
            items.extend(surface_items());
            items.extend(crate::ui_core::docs::items());
            items.extend(channel_items());
            let mut state = PaletteState::new(items);
            for c in query.chars() {
                state.type_char(c);
            }
            state.selected().map(|row| row.item.action)
        }

        assert_eq!(
            armed("daemon trouble"),
            Some(PaletteAction::OpenDocs(
                "/getting-started/troubleshooting.html"
            )),
            "the short form from the report has to land on troubleshooting"
        );
        assert_eq!(
            armed("automation"),
            Some(PaletteAction::OpenDocs("/features/cron-jobs.html")),
            "the single word automation is the cron page"
        );
        let official = |path: &str| format!("{}{}", crate::ui_core::docs::DOCS_BASE, path);
        for path in [
            "/getting-started/troubleshooting.html",
            "/features/cron-jobs.html",
            "/channels/telegram.html",
            "/channels/discord.html",
            "/channels/slack.html",
            "/channels/whatsapp.html",
        ] {
            assert_eq!(
                crate::ui_core::docs::url(path),
                official(path),
                "docs rows open the official site: {path}"
            );
        }
        for page in crate::ui_core::docs::pages() {
            assert_eq!(
                crate::ui_core::docs::url(page.path),
                official(page.path),
                "every carried page is an official URL: {}",
                page.title
            );
        }
    }

    /// The hint is for a channel that is not live: once it is configured the
    /// card goes quiet instead of nagging about work already done.
    #[test]
    fn the_hint_only_speaks_for_an_unconfigured_channel() {
        assert_eq!(
            channel_hint("telegram", &[]),
            Some(Channel::Telegram.setup_hint())
        );
        assert_eq!(
            channel_hint("how do i setup telegram bot", &[]),
            Some(Channel::Telegram.setup_hint())
        );
        assert_eq!(channel_hint("telegram", &[Channel::Telegram]), None);
        assert_eq!(channel_hint("sessions", &[]), None);
        assert_eq!(channel_hint("", &[]), None);
    }

    /// A short stem still names the channel, because people type `tele`; a
    /// word that merely contains part of a channel name does not fire, and
    /// neither does a two letter prefix.
    #[test]
    fn a_channel_is_matched_on_its_name_not_on_a_fragment() {
        assert_eq!(Channel::named_in("te"), None);
        assert_eq!(Channel::named_in("tele"), Some(Channel::Telegram));
        assert_eq!(Channel::named_in("telegraph"), None);
        assert_eq!(Channel::named_in("telegram"), Some(Channel::Telegram));
        assert_eq!(Channel::named_in("WHATSAPP"), Some(Channel::WhatsApp));
    }

    /// The caret blinks while typing and hides once the field is idle, which
    /// is the Spotlight behaviour the owner asked for (#2531).
    #[test]
    fn seeker_caret_blinks_then_hides_when_typing_stops() {
        let start = Instant::now();
        assert!(
            caret_blinks(start, start, CARET_IDLE_AFTER),
            "a fresh key shows it"
        );
        assert!(caret_blinks(
            start,
            start + CARET_IDLE_AFTER / 2,
            CARET_IDLE_AFTER
        ));
        assert!(
            !caret_blinks(start, start + CARET_IDLE_AFTER, CARET_IDLE_AFTER),
            "at the window's edge the field is idle and the caret is gone"
        );
        assert!(!caret_blinks(
            start,
            start + CARET_IDLE_AFTER * 10,
            CARET_IDLE_AFTER
        ));
    }

    /// A clock that reads earlier than the last key must not panic and must
    /// not resurrect the caret: `saturating_duration_since` yields zero, so
    /// the field counts as freshly typed and the caret shows.
    #[test]
    fn seeker_caret_survives_a_backwards_clock() {
        let now = Instant::now();
        assert!(caret_blinks(
            now,
            now - Duration::from_millis(5),
            CARET_IDLE_AFTER
        ));
    }

    fn caret_inserts_at_the_caret(state: &mut PaletteState) {
        state.insert_text("ac");
        state.move_caret(-1);
        state.type_char('b');
        assert_eq!(state.query(), "abc");
        assert_eq!(state.caret(), 2);

        state.insert_text("é");
        assert_eq!(state.query(), "abéc");
        assert_eq!(state.caret(), 3);
        state.move_caret(1);
        state.type_char('🦀');
        assert_eq!(state.query(), "abéc🦀");
        assert_eq!(state.caret(), 5);

        state.move_caret(-99);
        state.type_char('\n');
        assert_eq!(state.query(), "abéc🦀", "control input stays out");
    }

    #[test]
    fn palette_caret_inserts_at_the_caret() {
        caret_inserts_at_the_caret(&mut PaletteState::new(Vec::new()));
    }

    #[test]
    fn palette_caret_movement_clamps_and_collapses_selection() {
        let mut state = PaletteState::new(Vec::new());
        state.insert_text("a🦀b");
        assert_eq!(state.caret(), 3);
        state.move_caret(-1);
        assert_eq!(state.caret(), 2);
        state.move_caret(-99);
        assert_eq!(state.caret(), 0);
        state.move_caret(99);
        assert_eq!(state.caret(), 3);

        state.select_to(1);
        assert_eq!(state.selection(), Some((1, 3)));
        state.move_caret(-1);
        assert_eq!(state.caret(), 1, "left collapses to the selection start");
        assert_eq!(state.selection(), None);

        state.select_all();
        assert_eq!(state.selection(), Some((0, 3)));
        state.move_caret(1);
        assert_eq!(state.caret(), 3, "right collapses to the selection end");
        assert_eq!(state.selection(), None);
    }

    #[test]
    fn palette_caret_word_movement_matches_the_composer() {
        let mut state = PaletteState::new(Vec::new());
        state.insert_text("alpha beta gamma");
        state.move_caret_word(-1);
        assert_eq!(state.caret(), 11);
        state.move_caret_word(-1);
        assert_eq!(state.caret(), 6);
        state.move_caret_word(-1);
        assert_eq!(state.caret(), 0);
        state.move_caret_word(1);
        assert_eq!(state.caret(), 6);
        state.move_caret_word(1);
        assert_eq!(state.caret(), 11);
        state.move_caret_word(1);
        assert_eq!(state.caret(), 16);

        state.clear();
        state.insert_text("olá mundo");
        state.move_caret(-99);
        state.move_caret_word(1);
        assert_eq!(state.caret(), 4, "multibyte words still count in chars");
    }

    #[test]
    fn palette_caret_selection_peek_and_render_spans() {
        let mut state = PaletteState::new(Vec::new());
        state.insert_text("alpha beta");
        state.move_caret(-99);
        state.select_to(5);
        assert_eq!(state.selection(), Some((0, 5)));
        assert_eq!(state.selected_text(), Some("alpha"));
        assert_eq!(state.render_spans(), ("", "alpha", " beta"));

        state.collapse();
        assert_eq!(state.render_spans(), ("alpha", "", " beta"));
        state.move_caret(1);
        assert_eq!(state.render_spans(), ("alpha ", "", "beta"));
        state.select_all();
        assert_eq!(state.render_spans(), ("", "alpha beta", ""));
    }

    #[test]
    fn palette_caret_insert_replaces_and_take_cuts_selection() {
        let mut state = PaletteState::new(Vec::new());
        state.insert_text("alpha beta");
        state.select_all();
        state.insert_text("x");
        assert_eq!(state.query(), "x");
        assert_eq!(state.caret(), 1);
        assert_eq!(state.selection(), None);

        state.clear();
        state.insert_text("alpha beta");
        state.move_caret(-99);
        state.select_to(5);
        assert_eq!(state.take_selected_text().as_deref(), Some("alpha"));
        assert_eq!(state.query(), " beta");
        assert_eq!(state.caret(), 0);
        assert_eq!(state.selection(), None);
        assert_eq!(state.take_selected_text(), None);
    }

    #[test]
    fn palette_caret_delete_backward_forward_and_selection() {
        let mut state = PaletteState::new(Vec::new());
        state.insert_text("aé🦀b");
        state.move_caret(-1);
        state.backspace();
        assert_eq!(state.query(), "aéb");
        assert_eq!(state.caret(), 2);
        state.delete_forward();
        assert_eq!(state.query(), "aé");
        state.backspace();
        assert_eq!(state.query(), "a");
        state.move_caret(-1);
        state.delete_forward();
        assert_eq!(state.query(), "");
        state.backspace();
        state.delete_forward();

        state.insert_text("abcd");
        state.move_caret(-99);
        state.select_to(2);
        state.backspace();
        assert_eq!(state.query(), "cd");
        state.select_all();
        state.delete_forward();
        assert_eq!(state.query(), "");
        assert_eq!(state.caret(), 0);
    }

    #[test]
    fn palette_caret_delete_word_in_both_directions() {
        let mut state = PaletteState::new(Vec::new());
        state.insert_text("alpha beta gamma");
        state.delete_word_back();
        assert_eq!(state.query(), "alpha beta ");
        assert_eq!(state.caret(), 11);
        state.delete_word_back();
        assert_eq!(state.query(), "alpha ");
        assert_eq!(state.caret(), 6);

        state.clear();
        state.insert_text("alpha beta gamma");
        state.move_caret(-99);
        state.delete_word_forward();
        assert_eq!(state.query(), "beta gamma");
        state.delete_word_forward();
        assert_eq!(state.query(), "gamma");
        state.delete_word_forward();
        assert_eq!(state.query(), "");
        state.delete_word_forward();
    }

    #[test]
    fn palette_caret_only_mutations_reset_the_result_cursor() {
        let mut state = PaletteState::new(
            (0..12)
                .map(|index| item(&format!("row{index}"), PaletteKind::Command))
                .collect(),
        );
        for _ in 0..4 {
            state.move_by(1, 3);
        }
        let cursor = state.cursor();
        let scroll = state.scroll();
        let visible = state.visible();
        state.move_caret(1);
        state.select_to(0);
        assert_eq!(state.cursor(), cursor);
        assert_eq!(state.scroll(), scroll);
        assert_eq!(state.visible(), visible);

        state.type_char('x');
        assert_eq!(state.cursor(), None);
        assert_eq!(state.scroll(), 0);
        state.select_all();
        state.clear();
        assert_eq!(state.query(), "");
        assert_eq!(state.caret(), 0);
        assert_eq!(state.selection(), None);
    }
}
