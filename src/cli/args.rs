//! CLI argument types and entry point.

use anyhow::Result;
use clap::{Parser, Subcommand};

use super::{commands, cron, migrate, ui};
use crate::config::Config;

/// OpenCrabs - High-Performance Terminal AI Orchestration Agent
#[derive(Parser, Debug)]
#[command(name = "opencrabs")]
#[command(version, about, long_about = None)]
pub struct Cli {
    /// Enable debug mode (creates log files in .opencrabs/logs/)
    #[arg(short, long, global = true)]
    pub debug: bool,

    /// Configuration file path
    #[arg(short, long, global = true)]
    pub config: Option<String>,

    /// Profile to use (default: "default", or OPENCRABS_PROFILE env)
    #[arg(short, long, global = true)]
    pub profile: Option<String>,

    /// Subcommand to execute
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Start interactive TUI mode (default)
    Chat {
        /// Session ID to resume
        #[arg(short, long)]
        session: Option<String>,

        /// Force onboarding wizard before chat
        #[arg(long)]
        onboard: bool,
    },

    /// Run the onboarding setup wizard
    Onboard,

    /// Run a single command non-interactively
    Run {
        /// The prompt to execute
        prompt: String,

        /// Auto-approve all tool executions (dangerous!)
        #[arg(long, alias = "yolo")]
        auto_approve: bool,

        /// Output format
        #[arg(short, long, default_value = "text")]
        format: OutputFormat,

        /// Suppress progress and stats: stdout carries only the answer
        /// (payload-only output for scripting and one-shot callers)
        #[arg(long)]
        quiet: bool,
    },

    /// Show system status: version, provider, channels, database, brain
    Status,

    /// Run diagnostics: check config, provider connectivity, channel health, tools, brain
    Doctor {
        /// Repair detected issues: stuck cron rows, stale plan markers,
        /// loose brain/log file permissions (#1114)
        #[arg(long)]
        fix: bool,
    },

    /// Initialize configuration
    Init {
        /// Force overwrite existing configuration
        #[arg(short, long)]
        force: bool,
    },

    /// Show configuration
    Config {
        /// Show full configuration including secrets
        #[arg(short, long)]
        show_secrets: bool,
    },

    /// Database operations
    Db {
        #[command(subcommand)]
        operation: DbCommands,
    },

    /// Log management operations
    Logs {
        #[command(subcommand)]
        operation: LogCommands,
    },

    /// Interactive CLI agent (no TUI) — multi-turn conversation in your terminal
    Agent {
        /// Single message mode (non-interactive)
        #[arg(short, long)]
        message: Option<String>,

        /// Session ID to resume
        #[arg(short, long)]
        session: Option<String>,

        /// Auto-approve all tool executions
        #[arg(long, alias = "yolo")]
        auto_approve: bool,

        /// Output format (only for single-message mode)
        #[arg(short, long, default_value = "text")]
        format: OutputFormat,
    },

    /// Serve ACP (Agent Client Protocol) over stdio for agent-aware editors
    /// (MonoCode, Zed). stdout carries JSON-RPC frames only — one process
    /// per editor thread, sessions persist in the normal store.
    Acp {
        /// Default model override for sessions on this server
        #[arg(long)]
        model: Option<String>,
    },

    /// Channel operations
    Channel {
        #[command(subcommand)]
        operation: ChannelCommands,
    },

    /// Memory operations
    Memory {
        #[command(subcommand)]
        operation: MemoryCommands,
    },

    /// Session management
    Session {
        #[command(subcommand)]
        operation: SessionCommands,
    },

    /// OS service management (launchd/systemd)
    Service {
        #[command(subcommand)]
        operation: ServiceCommands,
    },

    /// Run in headless daemon mode — no TUI, channel bots only (Telegram, Discord, Slack, WhatsApp)
    /// Used by the systemd/LaunchAgent service installed during onboarding
    Daemon,

    /// Serve dropped files to a TUI running on another machine.
    ///
    /// Run this on the machine you drag files FROM, then connect with
    /// `ssh -R 127.0.0.1:8765:localhost:8765 <you>@<host>`. Dropping a file into the
    /// remote TUI then pulls it across the SSH connection you already made.
    DropAgent {
        /// Port to listen on. Must match the `-R` forward.
        #[arg(long, default_value_t = crate::utils::drop_transfer::DEFAULT_DROP_PORT)]
        port: u16,

        /// Directory to serve from. Repeatable. Defaults to Desktop,
        /// Downloads, Pictures, Documents and Movies.
        #[arg(long = "root")]
        roots: Vec<std::path::PathBuf>,
    },

    /// Manage profiles — isolated OpenCrabs instances with their own config, DB, and memory
    Profile {
        #[command(subcommand)]
        operation: ProfileCommands,
    },

    /// Manage scheduled cron jobs
    Cron {
        #[command(subcommand)]
        operation: CronCommands,
    },

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Print version and exit
    Version,

    /// Print the database schema migration count (used by evolve for compatibility checks)
    PrintMigrationCount,

    /// Check for and install the latest OpenCrabs release
    Evolve {
        /// Only check for updates without installing
        #[arg(long)]
        check_only: bool,
    },

    /// Migrate config and brain files from another AI agent tool
    ///
    /// Scans the system for instances of the source tool, shows an interactive
    /// picker if multiple are found, then spawns an agent to handle the migration.
    Migrate {
        /// Source tool to migrate from (openclaw, hermes)
        source: MigrationSource,

        /// Preview what would be migrated without making changes
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum LogCommands {
    /// Show log file location and status
    Status,
    /// View recent log entries (requires debug mode)
    View {
        /// Number of lines to show (default: 50)
        #[arg(short, long, default_value = "50")]
        lines: usize,
    },
    /// Clean up old log files
    Clean {
        /// Maximum age in days (default: 7)
        #[arg(short = 'a', long, default_value = "7")]
        days: u64,
    },
    /// Open log directory in file manager
    Open,
}

#[derive(Subcommand, Debug)]
pub enum DbCommands {
    /// Initialize database
    Init,
    /// Show database statistics
    Stats,
    /// Clear all sessions and messages from database
    Clear {
        /// Skip confirmation prompt (use with caution)
        #[arg(short, long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum CronCommands {
    /// Add a new cron job
    Add {
        /// Job name
        #[arg(long)]
        name: String,

        /// Cron expression (5-field: min hour dom mon dow)
        #[arg(long)]
        cron: String,

        /// Timezone (default: UTC)
        #[arg(long, default_value = "UTC")]
        tz: String,

        /// Prompt / instructions for the agent (optional if --trigger-cmd is supplied). When
        /// --trigger-cmd fires, {output}, {stdout}, {stderr} and {exit_code} in the prompt are
        /// substituted with the trigger's result; a prompt with no placeholder is passed through
        /// unchanged.
        #[arg(long, alias = "message", required_unless_present = "trigger_cmd")]
        prompt: Option<String>,

        /// Override provider (e.g. anthropic, openai)
        #[arg(long)]
        provider: Option<String>,

        /// Override model (e.g. claude-sonnet-4-20250514)
        #[arg(long)]
        model: Option<String>,

        /// Thinking mode: off, on, budget
        #[arg(long, default_value = "off")]
        thinking: String,

        /// Auto-approve tool executions
        #[arg(long, default_value = "true")]
        auto_approve: bool,

        /// Delivery destination: passive channel output (e.g. 'oc://telegram/...', 'telegram:123456') or active session turn injection ('oc://session/<uuid>')
        #[arg(long, alias = "deliver")]
        deliver_to: Option<String>,

        /// Optional pre-flight shell command to run before agent turn. When it fires, the job's
        /// prompt has {output}, {stdout}, {stderr} and {exit_code} substituted with the
        /// command's result; every fire logs the payload size as output_bytes.
        #[arg(long)]
        trigger_cmd: Option<String>,

        /// Trigger condition: non_empty (default), exit_non_zero, always
        #[arg(long)]
        trigger_on: Option<String>,

        /// Set active goal in destination session (requires deliver_to pointing to oc://session/<uuid> or session:<uuid>)
        #[arg(long)]
        set_goal: bool,

        /// Goal formatting template (interpolates {output}, {stdout}, {stderr}, {exit_code})
        #[arg(long)]
        goal_template: Option<String>,
    },

    /// List all cron jobs
    List,

    /// Remove a cron job by ID or name
    Remove {
        /// Job ID or name
        id: String,
    },

    /// Enable a cron job
    Enable {
        /// Job ID or name
        id: String,
    },

    /// Disable a cron job (pause without deleting)
    Disable {
        /// Job ID or name
        id: String,
    },

    /// Trigger a cron job immediately (runs on next scheduler tick)
    Test {
        /// Job ID or name
        id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum ChannelCommands {
    /// List configured channels and their status
    List,
    /// Run health checks on all enabled channels
    Doctor,
    /// Log the Telegram userbot in (MTProto user session; QR by default)
    #[command(name = "userbot-login")]
    UserbotLogin {
        /// Use the phone-code flow instead of QR
        #[arg(long)]
        code: bool,
    },
    /// Download a document through the Telegram userbot (up to 2 GB)
    #[command(name = "userbot-download")]
    UserbotDownload {
        /// Peer: @username or numeric chat/channel id
        peer: String,
        /// Message id, or "latest" for the most recent document
        #[arg(default_value = "latest")]
        message: String,
        /// Output directory (defaults to the current directory)
        #[arg(short, long)]
        outdir: Option<String>,
        /// List recent document messages instead of downloading
        #[arg(long)]
        list: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum MemoryCommands {
    /// List memory files in the brain directory
    List,
    /// Show a specific memory file
    Get {
        /// Memory file name (e.g. "MEMORY.md" or just "MEMORY")
        name: String,
    },
    /// Show memory statistics
    Stats,
    /// Prune cold beliefs and archive cold MEMORY.md sections (dry run by
    /// default; --apply executes, receipts land in memory/archive/ + backups)
    Prune {
        /// Execute the prune (default: dry run, nothing is written)
        #[arg(long)]
        apply: bool,
        /// Cold threshold in days (default: 90)
        #[arg(long)]
        max_age_days: Option<i64>,
    },
}

#[derive(Subcommand, Debug)]
pub enum SessionCommands {
    /// List all sessions
    List {
        /// Include archived sessions
        #[arg(short, long)]
        all: bool,
    },
    /// Show session details
    Get {
        /// Session ID
        id: String,
    },
    /// Set provider/model for sessions, non-interactively (#465)
    SetModel {
        /// The pair as provider/model — the FIRST slash splits, so model
        /// names with slashes work: openrouter/tencent/hy3:free
        pair: String,
        /// Target session: full ID or unambiguous prefix
        id: Option<String>,
        /// Match sessions by title substring (case-insensitive)
        #[arg(long)]
        name: Option<String>,
        /// Apply to ALL non-archived sessions
        #[arg(long)]
        all: bool,
    },
    /// Send a notification to a session (#23)
    Notify {
        /// Target session UUID or unambiguous prefix (#1340 resolver)
        id: String,
        /// Message text (send mode)
        #[arg(long)]
        text: Option<String>,
        /// Optional title header
        #[arg(long)]
        title: Option<String>,
        /// Sender label shown to the recipient (default: "CLI tooling")
        #[arg(long)]
        sender: Option<String>,
        /// Deliver even if the session is mid-turn (#13 failsafe) — deprecated alias for --mode turn-end
        #[arg(long)]
        interrupt: bool,
        /// Delivery mode: now (default) | turn-end | quiet
        #[arg(long)]
        mode: Option<String>,
        /// quiet mode: idle window before delivery, seconds (default 60)
        #[arg(long)]
        quiet_for_secs: Option<u64>,
        /// quiet mode: starvation cap, seconds (default 1800)
        #[arg(long)]
        max_delay_secs: Option<u64>,
        /// Verify end-to-end: watch the receiving machinery (~10s) for a wake instead of reporting the route alone
        #[arg(long)]
        confirm: bool,
        /// Poll a notification receipt by id instead of sending (uses <text> as the id)
        #[arg(long)]
        status: bool,
        /// CLI output format
        #[arg(short, long, default_value = "text")]
        format: OutputFormat,
    },
}

#[derive(Subcommand, Debug)]
pub enum ProfileCommands {
    /// Create a new profile
    Create {
        /// Profile name (alphanumeric, hyphens, underscores)
        name: String,

        /// Optional description
        #[arg(short, long)]
        description: Option<String>,
    },
    /// List all profiles
    List,
    /// Delete a profile and all its data
    Delete {
        /// Profile name to delete
        name: String,

        /// Skip confirmation prompt
        #[arg(short, long)]
        force: bool,
    },
    /// Export a profile as a tar.gz archive
    Export {
        /// Profile name to export
        name: String,

        /// Output file path (default: <name>.tar.gz)
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Import a profile from a tar.gz archive
    Import {
        /// Path to the archive file
        path: String,
    },
    /// Migrate config and brain files from one profile to another (no DB/sessions)
    Migrate {
        /// Source profile name
        from: String,

        /// Destination profile name
        to: String,

        /// Overwrite existing files in the destination
        #[arg(short, long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ServiceCommands {
    /// Install as OS service (launchd on macOS, systemd on Linux)
    Install,
    /// Start the service
    Start,
    /// Stop the service
    Stop,
    /// Restart the service
    Restart,
    /// Show service status
    Status,
    /// Uninstall the service
    Uninstall,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
    Markdown,
}

/// Source tool for migration
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum MigrationSource {
    /// Migrate from OpenClaw (TypeScript AI agent, ~/.openclaw/)
    Openclaw,
    /// Migrate from Hermes (NousResearch Python AI agent, ~/.hermes/)
    Hermes,
}

/// Main CLI entry point
pub async fn run() -> Result<()> {
    let cli = Cli::parse();

    // Set active profile BEFORE anything touches opencrabs_home(). main()
    // already applied it before logging init (#983); skip when the lock is
    // set so profile startups don't emit a spurious "already set" warning.
    if !crate::config::profile::is_profile_set() {
        crate::config::profile::set_active_profile(cli.profile.clone())
            .unwrap_or_else(|e| tracing::warn!("Profile already set: {}", e));
    }

    // Track profile usage
    if let Some(ref name) = cli.profile
        && let Ok(mut registry) = crate::config::profile::ProfileRegistry::load()
    {
        registry.touch(name);
        let _ = registry.save();
    }

    // Set up logging level based on debug flag
    if cli.debug {
        tracing::info!("Debug mode enabled");
    }

    // Load configuration
    let config = commands::load_config(cli.config.as_deref()).await?;
    // Seed the in-memory mirror so Config::current() readers never touch disk.
    Config::set_current(config.clone());

    // Apply the config-level debug_logs toggle on top of the --debug flag now
    // that the profile is resolved and config is loaded (#678). Lets a
    // non-technical user (or the agent, on request) enable file logging by
    // editing config.toml; the ConfigWatcher re-applies live changes.
    crate::logging::apply_debug_logs(cli.debug, config.agent.debug_logs);

    // Auto-generate config.toml if API keys exist in env but no config file yet.
    // This prevents the onboarding wizard from triggering when .env is already set up.
    let config_path = Config::system_config_path();
    if let Some(ref path) = config_path
        && !path.exists()
        && config.has_any_api_key()
    {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        // Seed the documented example, then enable what there are keys for
        // (#1437). This used to serialise the whole in-memory config over the
        // path instead, and because TOML cannot represent a None, that wrote a
        // file holding only the handful of providers that happened to be set:
        // no sections for the rest, no comments, nothing to hand-edit later.
        crate::config::seed::ensure_config_seeded();
        let enabled = crate::config::seed::enable_providers_with_keys(&config);
        if enabled.is_empty() {
            tracing::warn!(
                "Auto-generating config.toml found keys but enabled no provider section"
            );
        } else {
            tracing::info!(
                "Auto-generated config.toml, enabled: {}",
                enabled.join(", ")
            );
        }
    }

    // Seed the active profile's brain templates before any command runs
    // (#1382): wizard abort, daemon-only, docker, channel-first, or `init`
    // must all leave the user with a working personality on first open.
    // Compiled-in templates, offline-safe, idempotent, never overwrites.
    crate::config::profile::ensure_brain_seeded();

    match cli.command {
        None | Some(Commands::Chat { .. }) => {
            // Default: Interactive TUI mode
            let (session, force_onboard) = match &cli.command {
                Some(Commands::Chat { session, onboard }) => (session.clone(), *onboard),
                _ => (None, false),
            };
            ui::cmd_chat(&config, session, force_onboard).await
        }
        Some(Commands::Onboard) => {
            // Launch TUI with onboarding wizard (skip splash)
            ui::cmd_chat(&config, None, true).await
        }
        Some(Commands::Status) => commands::cmd_status(&config).await,
        Some(Commands::Doctor { fix }) => commands::cmd_doctor(&config, fix).await,
        Some(Commands::Init { force }) => commands::cmd_init(&config, force).await,
        Some(Commands::Config { show_secrets }) => {
            commands::cmd_config(&config, show_secrets).await
        }
        Some(Commands::Db { operation }) => commands::cmd_db(&config, operation).await,
        Some(Commands::Logs { operation }) => commands::cmd_logs(operation).await,
        Some(Commands::Run {
            prompt,
            auto_approve,
            format,
            quiet,
        }) => commands::cmd_run(&config, prompt, auto_approve, format, None, quiet).await,
        Some(Commands::Agent {
            message,
            session,
            auto_approve,
            format,
        }) => {
            if let Some(msg) = message {
                // Single message mode — same as `run`, plus #1368 resume:
                // `agent --session <prefix|uuid>` continues that session.
                // The interactive surface keeps its progress output: quiet
                // is a `run`-only flag for machine callers.
                commands::cmd_run(&config, msg, auto_approve, format, session, false).await
            } else {
                // Interactive CLI agent (no TUI), resumable the same way.
                commands::cmd_agent_interactive(&config, auto_approve, session).await
            }
        }
        Some(Commands::Acp { model }) => commands::cmd_acp(&config, model).await,
        Some(Commands::Channel { operation }) => commands::cmd_channel(&config, operation).await,
        Some(Commands::Memory { operation }) => commands::cmd_memory(operation).await,
        Some(Commands::Session { operation }) => commands::cmd_session(&config, operation).await,
        Some(Commands::Service { operation }) => commands::cmd_service(operation).await,
        Some(Commands::Daemon) => ui::cmd_daemon(&config).await,
        Some(Commands::DropAgent { port, roots }) => crate::utils::drop_agent::serve(port, roots),
        Some(Commands::Profile { operation }) => commands::cmd_profile(operation).await,
        Some(Commands::Cron { operation }) => cron::cmd_cron(&config, operation).await,
        Some(Commands::Completions { shell }) => {
            use clap::CommandFactory;
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                "opencrabs",
                &mut std::io::stdout(),
            );
            Ok(())
        }
        Some(Commands::Version) => {
            println!("opencrabs {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(Commands::PrintMigrationCount) => {
            println!("{}", crate::db::Database::MIGRATION_COUNT);
            Ok(())
        }
        Some(Commands::Evolve { check_only }) => commands::cmd_evolve(&config, check_only).await,
        Some(Commands::Migrate { source, dry_run }) => migrate::cmd_migrate(source, dry_run).await,
    }
}
