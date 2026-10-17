/// Sentinel value stored in api_key_input when a key was loaded from config.
/// The actual key is never held in memory — this just signals "key exists".
pub use crate::tui::provider_selector::EXISTING_KEY_SENTINEL;

/// STT provider selection in onboarding voice screen.
/// Replaces magic numbers with named variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SttProvider {
    #[default]
    Off,
    Groq,
    Local,
    OpenAiCompatible,
    Voicebox,
}

impl SttProvider {
    /// All variants in radio-button order
    pub const ALL: [SttProvider; 5] = [
        SttProvider::Off,
        SttProvider::Groq,
        SttProvider::Local,
        SttProvider::OpenAiCompatible,
        SttProvider::Voicebox,
    ];

    /// Variants available when local STT is compiled in
    pub fn available(is_local_available: bool) -> &'static [SttProvider] {
        if is_local_available {
            &Self::ALL
        } else {
            &[
                Self::Off,
                Self::Groq,
                Self::OpenAiCompatible,
                Self::Voicebox,
            ]
        }
    }

    /// Radio button label shown in the TUI
    pub fn label(&self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Groq => "API (Groq Whisper)",
            Self::Local => "Local (Whisper — runs on device)",
            Self::OpenAiCompatible => "OpenAI-compatible (custom endpoint)",
            Self::Voicebox => "Voicebox",
        }
    }

    /// Cycle to the next variant (wraps around)
    pub fn next(self, available: &[SttProvider]) -> Self {
        available
            .iter()
            .cycle()
            .skip_while(|&&v| v != self)
            .nth(1)
            .copied()
            .unwrap_or(Self::Off)
    }

    /// Cycle to the previous variant (wraps around)
    pub fn prev(self, available: &[SttProvider]) -> Self {
        available
            .iter()
            .rev()
            .cycle()
            .skip_while(|&&v| v != self)
            .nth(1)
            .copied()
            .unwrap_or(Self::Off)
    }
}

/// TTS provider selection in onboarding voice screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TtsProvider {
    #[default]
    Off,
    OpenAi,
    Local,
    OpenAiCompatible,
    Voicebox,
}

impl TtsProvider {
    /// All variants in radio-button order
    pub const ALL: [TtsProvider; 5] = [
        TtsProvider::Off,
        TtsProvider::OpenAi,
        TtsProvider::Local,
        TtsProvider::OpenAiCompatible,
        TtsProvider::Voicebox,
    ];

    /// Variants available when local TTS is compiled in
    pub fn available(is_local_available: bool) -> &'static [TtsProvider] {
        if is_local_available {
            &Self::ALL
        } else {
            &[
                Self::Off,
                TtsProvider::OpenAi,
                TtsProvider::OpenAiCompatible,
                TtsProvider::Voicebox,
            ]
        }
    }

    /// Radio button label shown in the TUI
    pub fn label(&self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::OpenAi => "API (OpenAI TTS — uses OpenAI key)",
            Self::Local => "Local (Piper — runs on device, free)",
            Self::OpenAiCompatible => "OpenAI-compatible (custom endpoint)",
            Self::Voicebox => "Voicebox",
        }
    }

    /// Cycle to the next variant (wraps around)
    pub fn next(self, available: &[TtsProvider]) -> Self {
        available
            .iter()
            .cycle()
            .skip_while(|&&v| v != self)
            .nth(1)
            .copied()
            .unwrap_or(Self::Off)
    }

    /// Cycle to the previous variant (wraps around)
    pub fn prev(self, available: &[TtsProvider]) -> Self {
        available
            .iter()
            .rev()
            .cycle()
            .skip_while(|&&v| v != self)
            .nth(1)
            .copied()
            .unwrap_or(Self::Off)
    }
}

/// Provider definitions
pub const PROVIDERS: &[ProviderInfo] = &[
    ProviderInfo {
        id: "anthropic",
        name: "Anthropic",
        models: &[], // Fetched from API
        key_label: "Setup Token",
        help_lines: &[
            "Claude Max / Code: run 'claude setup-token'",
            "Or paste API key from console.anthropic.com",
        ],
    },
    ProviderInfo {
        id: "openai",
        name: "OpenAI",
        models: &[],
        key_label: "API Key",
        help_lines: &["Get key from platform.openai.com"],
    },
    ProviderInfo {
        id: "github",
        name: "GitHub Copilot",
        models: &[],
        key_label: "OAuth",
        help_lines: &["Sign in with GitHub to use your Copilot subscription"],
    },
    ProviderInfo {
        id: "gemini",
        name: "Google Gemini",
        models: &[],
        key_label: "API Key",
        help_lines: &["Get key from aistudio.google.com"],
    },
    ProviderInfo {
        id: "openrouter",
        name: "OpenRouter",
        models: &[],
        key_label: "API Key",
        help_lines: &["Get key from openrouter.ai/keys"],
    },
    ProviderInfo {
        id: "minimax",
        name: "Minimax",
        models: &[], // Loaded from config.toml at runtime
        key_label: "API Key",
        help_lines: &["Get key from platform.minimax.io"],
    },
    ProviderInfo {
        id: "zai",
        name: "z.ai",
        models: &[], // Fetched from API
        key_label: "API Key",
        help_lines: &["Get key from open.bigmodel.cn"],
    },
    ProviderInfo {
        id: "moonshot",
        name: "Moonshot AI",
        models: &[], // Fetched from API
        key_label: "API Key",
        help_lines: &[
            "API plan: key from platform.moonshot.ai",
            "Coding plan: key from kimi.com (token subscription)",
        ],
    },
    ProviderInfo {
        id: "claude-cli",
        name: "Claude CLI",
        models: &["sonnet", "opus", "haiku"],
        key_label: "",
        help_lines: &[
            "Uses local 'claude' CLI subprocess — no API key needed",
            "Requires: npm install -g @anthropic-ai/claude-code",
        ],
    },
    ProviderInfo {
        id: "opencode-cli",
        name: "OpenCode CLI",
        models: &[],
        key_label: "",
        help_lines: &[
            "Uses local 'opencode' CLI subprocess — free models, no API key needed",
            "Requires: curl -fsSL https://opencode.ai/install | bash",
        ],
    },
    ProviderInfo {
        id: "codex-cli",
        name: "Codex CLI",
        models: &[
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.3-codex",
            "gpt-5.3-codex-spark",
            "gpt-5.2",
        ],
        key_label: "",
        help_lines: &[
            "Uses local 'codex' CLI subprocess — ChatGPT/Codex auth, no API key needed",
            "Requires: npm install -g @openai/codex (then run 'codex' once to sign in)",
        ],
    },
    ProviderInfo {
        id: "command-code-cli",
        name: "Command Code CLI",
        models: &[
            "taste-1",
            "deepseek/deepseek-v4-flash",
            "moonshotai/Kimi-K2.7-Code",
            "zai-org/GLM-5.2",
            "xiaomi/mimo-v2.5",
            "MiniMaxAI/MiniMax-M2.7",
            "Qwen/Qwen3.6-Max-Preview",
        ],
        key_label: "",
        help_lines: &[
            "Uses local 'cmd' CLI subprocess — Command Code account, no API key needed",
            "Requires: npm install -g command-code (then run 'cmd login')",
        ],
    },
    ProviderInfo {
        id: "codex",
        name: "Codex",
        models: &[
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.3-codex",
            "gpt-5.3-codex-spark",
            "gpt-5.2",
            "gpt-4o",
            "o3",
            "o4-mini",
        ],
        key_label: "OAuth",
        help_lines: &[
            "Authenticate with OpenAI Codex subscription via device-code flow",
            "No CLI needed — sign in through OpenCrabs onboarding",
        ],
    },
    ProviderInfo {
        id: "opencode",
        name: "OpenCode",
        models: &[], // Fetched from opencode.ai API
        key_label: "API Key",
        help_lines: &[
            "Go and Zen plans — opencode.ai",
            "Get key from opencode.ai/settings",
        ],
    },
    ProviderInfo {
        id: "qwen",
        name: "Qwen",
        models: &[
            "qwen3.6-plus",
            "qwen3-max",
            "qwen3-coder-plus",
            "qwen3.5-plus",
            "qwen-max",
            "qwen-plus",
            "qwen-flash",
        ],
        key_label: "API Key",
        help_lines: &[
            "DashScope OpenAI-compatible API (Alibaba Cloud Model Studio)",
            "Get key from bailian.console.aliyun.com or qwen.ai/apiplatform",
        ],
    },
    ProviderInfo {
        id: "ollama",
        name: "Ollama",
        models: &[], // Fetched from /api/tags
        key_label: "API Key",
        help_lines: &[
            "Local: runs on localhost:11434 — no key needed",
            "Cloud: set base_url + optional API key",
            "Requires: ollama serve (local) or cloud endpoint",
        ],
    },
    // Xiaomi MiMo — OpenAI-compatible, keyed. Get an API key at
    // platform.xiaomimimo.com.
    ProviderInfo {
        id: "xiaomi",
        name: "Xiaomi",
        models: &[], // Fetched live from /v1/models (chat models only)
        key_label: "API Key",
        help_lines: &[
            "Get an API key at platform.xiaomimimo.com",
            "OpenAI-compatible — MiMo v2.5 models",
        ],
    },
    // Built-in OpenCrabs — no key is typed. Onboarding requests the install token.
    ProviderInfo {
        id: "opencrabs",
        name: "OpenCrabs",
        models: &["cb/deepseek-v4.1-flash"],
        key_label: "",
        help_lines: &[
            "Built in: no account or key needed",
            "Runs on the OpenCrabs inference service",
        ],
    },
    ProviderInfo {
        id: "", // dynamic — custom providers use runtime names
        name: "Custom OpenAI-Compatible",
        models: &[],
        key_label: "API Key",
        help_lines: &["Enter your own API endpoint"],
    },
];

pub struct ProviderInfo {
    /// Canonical provider id matching `KNOWN_PROVIDERS` (e.g. "anthropic", "claude-cli")
    /// Empty string for "Custom OpenAI-Compatible" (last entry) which is dynamic.
    pub id: &'static str,
    pub name: &'static str,
    pub models: &'static [&'static str],
    pub key_label: &'static str,
    pub help_lines: &'static [&'static str],
}

/// Channel definitions for the unified Channels step.
/// Index mapping: 0=Telegram, 1=Discord, 2=WhatsApp, 3=Slack, 4=Trello
pub const CHANNEL_NAMES: &[(&str, &str)] = &[
    ("Telegram", "Bot token (via @BotFather)"),
    ("Discord", "Bot token (via Developer Portal)"),
    ("WhatsApp", "QR code pairing"),
    ("Slack", "Socket Mode (bot + app tokens)"),
    ("Trello", "API Key + Token from trello.com/power-ups/admin"),
];

/// Template files to seed in the workspace
pub const TEMPLATE_FILES: &[(&str, &str)] = &[
    (
        "SOUL.md",
        include_str!("../../docs/reference/templates/SOUL.md"),
    ),
    (
        "USER.md",
        include_str!("../../docs/reference/templates/USER.md"),
    ),
    (
        "AGENTS.md",
        include_str!("../../docs/reference/templates/AGENTS.md"),
    ),
    (
        "TOOLS.md",
        include_str!("../../docs/reference/templates/TOOLS.md"),
    ),
    (
        "MEMORY.md",
        include_str!("../../docs/reference/templates/MEMORY.md"),
    ),
    (
        "CODE.md",
        include_str!("../../docs/reference/templates/CODE.md"),
    ),
    (
        "SECURITY.md",
        include_str!("../../docs/reference/templates/SECURITY.md"),
    ),
    (
        "BOOT.md",
        include_str!("../../docs/reference/templates/BOOT.md"),
    ),
    (
        "HEARTBEAT.md",
        include_str!("../../docs/reference/templates/HEARTBEAT.md"),
    ),
];

/// Current step in the onboarding wizard
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardingStep {
    ModeSelect,
    Workspace,
    ProviderAuth,
    Channels,
    TelegramSetup,
    DiscordSetup,
    WhatsAppSetup,
    SlackSetup,
    TrelloSetup,
    VoiceSetup,
    ImageSetup,
    Daemon,
    HealthCheck,
    BrainSetup,
    Complete,
}

impl OnboardingStep {
    /// Step number (1-based)
    pub fn number(&self) -> usize {
        match self {
            Self::ModeSelect => 1,
            Self::Workspace => 2,
            Self::ProviderAuth => 3,
            Self::Channels => 4,
            Self::TelegramSetup => 4, // sub-step of Channels
            Self::DiscordSetup => 4,  // sub-step of Channels
            Self::WhatsAppSetup => 4, // sub-step of Channels
            Self::SlackSetup => 4,    // sub-step of Channels
            Self::TrelloSetup => 4,   // sub-step of Channels
            Self::VoiceSetup => 5,
            Self::ImageSetup => 6,
            Self::Daemon => 7,
            Self::HealthCheck => 8,
            Self::BrainSetup => 9,
            Self::Complete => 10,
        }
    }

    /// Total number of steps (excluding Complete)
    pub fn total() -> usize {
        9
    }

    /// Position in the QuickStart sequence, which skips Channels/Voice/Image:
    /// ModeSelect → Workspace → ProviderAuth → Daemon → HealthCheck → BrainSetup.
    /// Used for the progress dots so Quick shows its own count (e.g. 4/6) instead
    /// of the full 1-9 numbering jumping when steps are skipped.
    pub fn quick_number(&self) -> usize {
        match self {
            Self::ModeSelect => 1,
            Self::Workspace => 2,
            Self::ProviderAuth => 3,
            Self::Daemon => 4,
            Self::HealthCheck => 5,
            Self::BrainSetup => 6,
            Self::Complete => 7,
            // Channels/Voice/Image aren't part of the Quick flow; if one is
            // somehow rendered under QuickStart, clamp to the nearest position.
            _ => 3,
        }
    }

    /// Total steps in the QuickStart flow (excluding Complete).
    pub fn quick_total() -> usize {
        6
    }

    /// The steps a flow walks through, in order, excluding Complete. The
    /// onboarding timeline draws exactly these, so they must line up with
    /// `number()` / `quick_number()` and the two totals above.
    pub fn flow_steps(mode: WizardMode) -> &'static [OnboardingStep] {
        const QUICK: [OnboardingStep; 6] = [
            OnboardingStep::ModeSelect,
            OnboardingStep::Workspace,
            OnboardingStep::ProviderAuth,
            OnboardingStep::Daemon,
            OnboardingStep::HealthCheck,
            OnboardingStep::BrainSetup,
        ];
        const ADVANCED: [OnboardingStep; 9] = [
            OnboardingStep::ModeSelect,
            OnboardingStep::Workspace,
            OnboardingStep::ProviderAuth,
            OnboardingStep::Channels,
            OnboardingStep::VoiceSetup,
            OnboardingStep::ImageSetup,
            OnboardingStep::Daemon,
            OnboardingStep::HealthCheck,
            OnboardingStep::BrainSetup,
        ];
        match mode {
            WizardMode::QuickStart => &QUICK,
            WizardMode::Advanced => &ADVANCED,
        }
    }

    /// 1-based position of this step in `mode`'s flow, the same numbering the
    /// progress counter uses (channel sub-steps count as Channels).
    pub fn flow_number(&self, mode: WizardMode) -> usize {
        match mode {
            WizardMode::QuickStart => self.quick_number(),
            WizardMode::Advanced => self.number(),
        }
    }

    /// Step title
    pub fn title(&self) -> &'static str {
        match self {
            Self::ModeSelect => "Pick Your Vibe",
            Self::Workspace => "Home Base",
            Self::ProviderAuth => "Brain Fuel",
            Self::Channels => "Chat Me Anywhere",
            Self::TelegramSetup => "Telegram Bot",
            Self::DiscordSetup => "Discord Bot",
            Self::WhatsAppSetup => "WhatsApp",
            Self::SlackSetup => "Slack Bot",
            Self::TrelloSetup => "Trello",
            Self::VoiceSetup => "Voice Superpowers",
            Self::ImageSetup => "Image Handling",
            Self::Daemon => "Always On",
            Self::HealthCheck => "Vibe Check",
            Self::BrainSetup => "Make It Yours",
            Self::Complete => "Let's Go!",
        }
    }

    /// Stable identifier for persisting progress. Deliberately not the title,
    /// which is prose and free to change without invalidating saved state.
    pub fn as_key(&self) -> &'static str {
        match self {
            Self::ModeSelect => "mode_select",
            Self::Workspace => "workspace",
            Self::ProviderAuth => "provider_auth",
            Self::Channels => "channels",
            Self::TelegramSetup => "telegram_setup",
            Self::DiscordSetup => "discord_setup",
            Self::WhatsAppSetup => "whatsapp_setup",
            Self::SlackSetup => "slack_setup",
            Self::TrelloSetup => "trello_setup",
            Self::VoiceSetup => "voice_setup",
            Self::ImageSetup => "image_setup",
            Self::Daemon => "daemon",
            Self::HealthCheck => "health_check",
            Self::BrainSetup => "brain_setup",
            Self::Complete => "complete",
        }
    }

    /// Parse a key written by a previous run. Unknown keys return `None` so a
    /// state file from a newer or older build restarts the wizard rather than
    /// resuming somewhere arbitrary.
    pub fn from_key(key: &str) -> Option<Self> {
        let all = [
            Self::ModeSelect,
            Self::Workspace,
            Self::ProviderAuth,
            Self::Channels,
            Self::TelegramSetup,
            Self::DiscordSetup,
            Self::WhatsAppSetup,
            Self::SlackSetup,
            Self::TrelloSetup,
            Self::VoiceSetup,
            Self::ImageSetup,
            Self::Daemon,
            Self::HealthCheck,
            Self::BrainSetup,
            Self::Complete,
        ];
        all.into_iter().find(|s| s.as_key() == key)
    }

    /// The main steps of a flow, in order. Channel sub-steps are left out:
    /// they are reached from Channels and are not separate stops.
    pub fn flow(mode: WizardMode) -> &'static [Self] {
        match mode {
            WizardMode::QuickStart => &[
                Self::ModeSelect,
                Self::Workspace,
                Self::ProviderAuth,
                Self::Daemon,
                Self::HealthCheck,
                Self::BrainSetup,
            ],
            WizardMode::Advanced => &[
                Self::ModeSelect,
                Self::Workspace,
                Self::ProviderAuth,
                Self::Channels,
                Self::VoiceSetup,
                Self::ImageSetup,
                Self::Daemon,
                Self::HealthCheck,
                Self::BrainSetup,
            ],
        }
    }

    /// Titles of the steps still ahead, so a resumed wizard can say what is
    /// left rather than dropping the user back in with no context (#919).
    /// A step not in the flow (a channel sub-step) counts from its parent.
    pub fn remaining_titles(&self, mode: WizardMode) -> Vec<&'static str> {
        let flow = Self::flow(mode);
        let anchor = match self {
            Self::TelegramSetup
            | Self::DiscordSetup
            | Self::WhatsAppSetup
            | Self::SlackSetup
            | Self::TrelloSetup => Self::Channels,
            other => *other,
        };
        let Some(pos) = flow.iter().position(|s| *s == anchor) else {
            return Vec::new();
        };
        flow[pos + 1..].iter().map(|s| s.title()).collect()
    }

    /// Step subtitle
    pub fn subtitle(&self) -> &'static str {
        match self {
            Self::ModeSelect => "Quick and easy or full control — your call",
            Self::Workspace => "Where my brain lives on disk",
            Self::ProviderAuth => "Pick your AI model and drop your key",
            Self::Channels => "Chat with me from your phone — Telegram, WhatsApp, whatever",
            Self::TelegramSetup => "Hook up your Telegram bot token",
            Self::DiscordSetup => "Hook up your Discord bot token",
            Self::WhatsAppSetup => "Scan the QR code with your phone",
            Self::SlackSetup => "Hook up your Slack bot and app tokens",
            Self::TrelloSetup => "Hook up your Trello API Key and Token",
            Self::VoiceSetup => "Talk to me, literally",
            Self::ImageSetup => "Vision and image generation via Google Gemini",
            Self::Daemon => "Keep me running in the background",
            Self::HealthCheck => "Making sure everything's wired up right",
            Self::BrainSetup => "Make me yours, drop some context so I actually get you",
            Self::Complete => "You're all set — let's build something cool",
        }
    }
}

/// Wizard mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardMode {
    QuickStart,
    Advanced,
}

impl WizardMode {
    /// Stable identifier for persisting which flow the user chose.
    pub fn as_key(&self) -> &'static str {
        match self {
            Self::QuickStart => "quick",
            Self::Advanced => "advanced",
        }
    }
}

/// Health check status for individual checks
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    Pending,
    Running,
    Pass,
    Fail(String),
}

/// Which field is being actively edited in ProviderAuth step
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthField {
    Provider,
    ApiKey,
    Model,
    CustomName,
    CustomBaseUrl,
    CustomApiKey,
    CustomModel,
    CustomContextWindow,
    ZhipuEndpointType,
    XiaomiEndpointType,
    MoonshotEndpointType,
    /// OpenCode: Go or Zen plan, before the API key (#2040).
    OpenCodeEndpointType,
    MoonshotPlan,
}

/// Which field is focused in DiscordSetup step
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordField {
    BotToken,
    ChannelID,
    AllowedList,
    RespondTo,
}

/// Which field is focused in SlackSetup step
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlackField {
    BotToken,
    AppToken,
    ChannelID,
    AllowedList,
    RespondTo,
}

/// Which field is focused in TelegramSetup step
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelegramField {
    BotToken,
    UserID,
    /// Rich text experience toggle: `channels.telegram.rich_messages`.
    /// Client-side gate (#418): rich messages only render on current
    /// Telegram apps, so the user self-checks instead of us detecting it.
    RichText,
    RespondTo,
}

/// Which field is focused in WhatsAppSetup step
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhatsAppField {
    Connection,
    PhoneAllowlist,
}

/// Which field is focused in TrelloSetup step
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrelloField {
    ApiKey,
    ApiToken,
    BoardId,
    AllowedUsers,
}

/// Channel test connection status
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelTestStatus {
    Idle,
    Testing,
    Success,
    Failed(String),
}

/// Which field is focused in VoiceSetup step
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceField {
    SttModeSelect,
    GroqApiKey,
    LocalModelSelect,
    // OpenAI-compatible STT
    SttOpenaiCompatSelect,
    SttOpenaiCompatUrl,
    SttOpenaiCompatModel,
    SttOpenaiCompatKey,
    // Voicebox STT
    SttVoiceboxSelect,
    SttVoiceboxUrl,
    // TTS mode
    TtsModeSelect,
    TtsLocalVoiceSelect,
    // Built-in OpenAI TTS
    TtsApiVoiceSelect,
    TtsApiKey,
    // OpenAI-compatible TTS
    TtsOpenaiCompatSelect,
    TtsOpenaiCompatUrl,
    TtsOpenaiCompatModel,
    TtsOpenaiCompatVoice,
    TtsOpenaiCompatKey,
    // Voicebox TTS
    TtsVoiceboxSelect,
    TtsVoiceboxUrl,
    TtsVoiceboxProfileId,
    TtsVoiceboxEngine,
    // Navigation
    Continue,
}

/// Which field is focused in ImageSetup step
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageField {
    VisionToggle,
    GenerationToggle,
    /// Free-text override for the image-generation model name.
    /// Only navigable when `image_generation_enabled` is true; empty
    /// input keeps the seeded `gemini-3.1-flash-image-preview` default.
    GenerationModel,
    ApiKey,
}

/// GitHub Copilot device flow status
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitHubDeviceFlowStatus {
    /// Not started
    Idle,
    /// Waiting for user to enter code at github.com/login/device
    WaitingForUser,
    /// User authorized, token obtained
    Complete,
    /// Flow failed
    Failed(String),
}

/// Codex OAuth device flow status
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CodexDeviceFlowStatus {
    /// Not started
    #[default]
    Idle,
    /// Waiting for user to enter code at verification URL
    WaitingForUser,
    /// User authorized, tokens saved
    Complete,
    /// Flow failed
    Failed(String),
}

/// Which text area is focused in BrainSetup step
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrainField {
    AboutMe,
    AboutAgent,
}

/// What the app should do after handling a wizard key event
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardAction {
    /// Nothing special
    None,
    /// User cancelled the wizard (Esc from step 1)
    Cancel,
    /// User left a first-run wizard (Esc from step 1). There is no configured
    /// provider to land on, so the app quits; the next launch reopens setup.
    Quit,
    /// Wizard completed successfully
    Complete,
    /// Trigger async AI generation of brain files
    GenerateBrain,
    /// Request this install's built-in OpenCrabs token, then advance the
    /// LLM step. The app runs the request (#2047).
    AcquireBuiltinToken,
    /// Quick start on a first run: prepare the files, request the built-in
    /// token, then finish straight into chat (#2048).
    QuickStartBuiltin,
    /// Trigger async model list fetch from provider API
    FetchModels,
    /// Trigger async WhatsApp QR code pairing
    WhatsAppConnect,
    /// Trigger async Telegram test message
    TestTelegram,
    /// Trigger async Discord test message
    TestDiscord,
    /// Trigger async Slack test message
    TestSlack,
    /// Trigger async WhatsApp test message
    TestWhatsApp,
    /// Trigger async Trello connection test
    TestTrello,
    /// Trigger async whisper model download
    DownloadWhisperModel,
    /// Trigger async Piper voice model download
    DownloadPiperVoice,
    /// Trigger GitHub Copilot OAuth device flow
    GitHubDeviceFlow,
    /// Trigger Codex OAuth device flow
    CodexDeviceFlow,
    /// Quick-jump step completed — save config and close wizard
    QuickJumpDone,
}
