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
    /// The row title, which is also what the query is matched against.
    pub fn title(self) -> &'static str {
        match self {
            Surface::Usage => "Usage ledger",
            Surface::Files => "Session files",
            Surface::Themes => "Themes",
            Surface::Models => "Models",
            Surface::Sessions => "Sessions",
            Surface::Help => "Key reference",
            Surface::Skills => "Skills",
            Surface::Profiles => "Profiles",
            Surface::MissionControl => "Mission Control",
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
    Surface::Files,
    Surface::Themes,
    Surface::Models,
    Surface::Help,
    Surface::Skills,
    Surface::Profiles,
    Surface::MissionControl,
];

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
    ("voice dictation speech transcribe microphone", "/whisper"),
    ("update upgrade latest version release", "/evolve"),
    (
        "rsi mc control analytics inbox dashboard",
        "/mission-control",
    ),
    ("commands keys shortcuts help what can you do", "/help"),
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
/// to translate its own key type into the other's. `ctrl-shift-f` is the one
/// every platform can send; `cmd-k` is the window's alias, and on macOS the
/// command key arrives as `platform` (#2458).
pub fn opens(control: bool, shift: bool, platform: bool, key: &str) -> bool {
    (control && shift && key.eq_ignore_ascii_case("f"))
        || (platform && key.eq_ignore_ascii_case("k"))
}

/// The chord that opens the card in a terminal, where the shift is not a
/// separate event: a terminal delivers `ctrl-shift-f` as the raw control byte
/// `0x06`, which is indistinguishable from `ctrl-f`, so asking for the shift
/// made the card impossible to open. The terminal arm therefore takes the bare
/// chord, and the shifted one still works wherever a terminal does report it.
/// Nothing else in the terminal answers `ctrl-f`, so the two cannot collide
/// (#2463).
pub fn opens_in_terminal(control: bool, key: &str) -> bool {
    control && key.eq_ignore_ascii_case("f")
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
    let title = match_score(query, &item.title);
    let words = match_weighted(query, &item.keywords, 6);
    match (title, words) {
        (None, None) => None,
        (Some(t), None) => Some(t),
        (None, Some(w)) => Some(w),
        (Some(t), Some(w)) => Some(t.max(w)),
    }
}

/// Score one text field, full weight.
fn match_score(query: &str, text: &str) -> Option<u16> {
    match_weighted(query, text, 10)
}

/// Subsequence scoring over one field. `weight` scales the answer so keywords
/// can sit below a title match without a second scoring rule.
fn match_weighted(query: &str, text: &str, weight: u16) -> Option<u16> {
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
    let mut score: i32 = 0;
    let mut hi = 0;
    let mut last_hit: Option<usize> = None;
    for (ni, want) in needle.iter().enumerate() {
        let found = hay[hi..].iter().position(|c| c == want)? + hi;
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
    /// Index into `visible()`, not into `items`: the list is re-ranked on every
    /// keystroke, so a raw index would arm a different row than the one the
    /// user is looking at.
    cursor: Option<usize>,
    scroll: usize,
    items: Vec<PaletteItem>,
}

/// How many rows the card prints. A list longer than this is scrolled, not
/// rendered: the card is a launcher, not a catalogue.
pub const MAX_ROWS: usize = 60;

impl PaletteState {
    pub fn new(items: Vec<PaletteItem>) -> Self {
        Self {
            query: String::new(),
            cursor: None,
            scroll: 0,
            items,
        }
    }

    pub fn query(&self) -> &str {
        &self.query
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

    /// A typed character extends the query and disarms the cursor, so the
    /// best match is the one Enter acts on until an arrow steps in.
    pub fn type_char(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        self.query.push(c);
        self.cursor = None;
        self.scroll = 0;
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.cursor = None;
        self.scroll = 0;
    }

    /// Clear the query but keep the card open. Esc closes the card, so this is
    /// the key that clears the field without leaving.
    pub fn clear(&mut self) {
        self.query.clear();
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
    /// then title, so two frontends cannot disagree about the order.
    pub fn visible(&self) -> Vec<PaletteRow> {
        let mut rows: Vec<PaletteRow> = self
            .items
            .iter()
            .filter_map(|item| {
                score(&self.query, item).map(|score| PaletteRow {
                    item: item.clone(),
                    score,
                })
            })
            .collect();
        rows.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then(a.item.kind.rank().cmp(&b.item.kind.rank()))
                .then(
                    a.item
                        .title
                        .to_lowercase()
                        .cmp(&b.item.title.to_lowercase()),
                )
        });
        rows.truncate(MAX_ROWS);
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

    /// Order is a contract between the two frontends: score, then kind, then
    /// title. Two equal-scoring rows must not swap places between paints.
    #[test]
    fn equal_scores_order_by_kind_then_title() {
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
        assert_eq!(titles, vec!["beta", "alpha", "gamma", "zebra"]);
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

    /// Nothing matched: the card stays open with no rows and prints one roast.
    /// The line is stable for a query, so it does not flicker as the user
    /// types, and it never names the user.
    #[test]
    fn no_match_keeps_a_stable_short_line() {
        let mut state = PaletteState::new(vec![command("/help")]);
        for c in "qwx".chars() {
            state.type_char(c);
        }
        assert!(state.visible().is_empty());
        assert_eq!(state.selected(), None);

        let line = roast("qwx");
        assert_eq!(line, roast("qwx"), "the same query gets the same line");
        assert_eq!(line, roast("QWX"), "case does not change it");
        assert!(line.chars().count() < 80, "the line must fit one row");
        assert!(line.is_ascii(), "the card prints plain punctuation only");
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

    /// An empty query lists everything, bounded, so the card opens on a useful
    /// list instead of a blank panel.
    #[test]
    fn an_empty_query_lists_everything_up_to_the_cap() {
        let state = PaletteState::new(
            (0..MAX_ROWS + 20)
                .map(|ix| item(&format!("row{ix:03}"), PaletteKind::Command))
                .collect(),
        );
        assert_eq!(state.visible().len(), MAX_ROWS);
        assert_eq!(
            state.selected().map(|row| row.item.title),
            Some("row000".to_string()),
            "Enter with nothing typed acts on the first row"
        );
    }

    /// One chord table for both clients: ctrl-shift-f everywhere, cmd-k in the
    /// window, and nothing else opens the card.
    #[test]
    fn only_the_two_named_chords_open_the_card() {
        assert!(opens(true, true, false, "f"));
        assert!(opens(true, true, false, "F"));
        assert!(opens(false, false, true, "k"));
        assert!(!opens(true, false, false, "f"));
        assert!(!opens(true, true, false, "k"));
        assert!(!opens(false, false, false, "k"));
        assert!(!opens(true, false, true, "f"));
    }

    /// The terminal gets the bare chord, because that is all a terminal sends:
    /// `ctrl-shift-f` arrives as `ctrl-f` there (#2463).
    #[test]
    fn the_terminal_chord_opens_without_the_shift() {
        assert!(opens_in_terminal(true, "f"));
        assert!(opens_in_terminal(true, "F"));
        assert!(!opens_in_terminal(false, "f"));
        assert!(!opens_in_terminal(true, "k"));
    }

    /// Every surface the card can find names a title, and every one but the
    /// session-files view names the command the terminal runs for it. That
    /// exception is pinned: a surface that grows without a command fails here
    /// instead of landing on a row the terminal cannot open (#2458).
    #[test]
    fn every_surface_but_files_names_a_command() {
        let items = surface_items();
        assert_eq!(items.len(), SURFACES.len());
        let mut without = Vec::new();
        for surface in SURFACES {
            assert!(!surface.title().is_empty());
            match surface.command() {
                Some(command) => assert!(command.starts_with('/'), "{command}"),
                None => without.push(*surface),
            }
        }
        assert_eq!(without, vec![Surface::Files]);
        for item in items {
            assert_eq!(item.kind, PaletteKind::Surface);
            assert!(!item.title.is_empty());
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
}
