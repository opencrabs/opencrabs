//! CLI subcommands — run, init, config, db, keyring, logs, status, doctor, and config loading.

use anyhow::{Context, Result};
use std::sync::Arc;

use crate::brain::BrainLoader;
use crate::brain::prompt_builder::RuntimeInfo;

use super::args::{
    ChannelCommands, DbCommands, LogCommands, MemoryCommands, OutputFormat, ProfileCommands,
    ServiceCommands, SessionCommands,
};

/// Show system status
pub(crate) async fn cmd_status(config: &crate::config::Config) -> Result<()> {
    use crate::db::Database;

    let version = env!("CARGO_PKG_VERSION");
    println!("🦀 OpenCrabs v{version}\n");

    // Provider
    match crate::brain::provider::create_provider(config).await {
        Ok(provider) => {
            println!(
                "  Provider:  {} ({})",
                provider.name(),
                provider.default_model()
            );
        }
        Err(_) => println!("  Provider:  not configured"),
    }

    // Brain
    let brain_path = BrainLoader::resolve_path();
    let brain_files: Vec<&str> = [
        "persona.md",
        "system.md",
        "USER.md",
        "MEMORY.md",
        "AGENTS.md",
        "TOOLS.md",
        "SOUL.md",
    ]
    .iter()
    .filter(|f| brain_path.join(f).exists())
    .copied()
    .collect();
    if brain_files.is_empty() {
        println!("  Brain:     no files found at {}", brain_path.display());
    } else {
        println!(
            "  Brain:     {} files ({})",
            brain_files.len(),
            brain_files.join(", ")
        );
    }

    // Database
    let db_path = &config.database.path;
    if db_path.exists() {
        let size = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
        let size_str = if size > 1_048_576 {
            format!("{:.1} MB", size as f64 / 1_048_576.0)
        } else {
            format!("{:.0} KB", size as f64 / 1024.0)
        };

        match Database::connect(db_path).await {
            Ok(db) => {
                let counts = async {
                    let conn = db.pool().get().await.ok()?;
                    conn.interact(|c| {
                        let sessions: i64 =
                            c.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
                        let messages: i64 =
                            c.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
                        Ok::<_, rusqlite::Error>((sessions, messages))
                    })
                    .await
                    .ok()?
                    .ok()
                }
                .await;
                if let Some((sessions, messages)) = counts {
                    println!(
                        "  Database:  {} — {} sessions, {} messages ({})",
                        db_path.display(),
                        sessions,
                        messages,
                        size_str
                    );
                } else {
                    println!("  Database:  {} ({})", db_path.display(), size_str);
                }
            }
            Err(_) => println!("  Database:  {} ({})", db_path.display(), size_str),
        }
    } else {
        println!("  Database:  not initialized");
    }

    // Channels
    let mut channels = Vec::new();
    if config.channels.telegram.enabled {
        channels.push("Telegram");
    }
    if config.channels.discord.enabled {
        channels.push("Discord");
    }
    if config.channels.slack.enabled {
        channels.push("Slack");
    }
    if config.channels.whatsapp.enabled {
        channels.push("WhatsApp");
    }
    if config.channels.trello.enabled {
        channels.push("Trello");
    }
    if channels.is_empty() {
        println!("  Channels:  none enabled");
    } else {
        println!("  Channels:  {}", channels.join(", "));
    }

    // A2A
    if config.a2a.enabled {
        println!(
            "  A2A:       enabled ({}:{})",
            config.a2a.bind, config.a2a.port
        );
    }

    // Dynamic tools
    let tools_path = BrainLoader::resolve_path().join("tools.toml");
    if tools_path.exists()
        && let Ok(contents) = std::fs::read_to_string(&tools_path)
    {
        let count = contents.matches("[[tools]]").count();
        if count > 0 {
            println!("  Tools:     {} dynamic tool(s) in tools.toml", count);
        }
    }

    // Cron
    let cron_path = BrainLoader::resolve_path().join("cron.toml");
    if cron_path.exists()
        && let Ok(contents) = std::fs::read_to_string(&cron_path)
    {
        let count = contents.matches("[[jobs]]").count();
        if count > 0 {
            println!("  Cron:      {} job(s)", count);
        }
    }

    // Logs
    let log_dir = dirs::config_dir()
        .map(|d| d.join("opencrabs").join("logs"))
        .unwrap_or_default();
    if log_dir.exists() {
        let log_count = std::fs::read_dir(&log_dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path()
                            .extension()
                            .map(|ext| ext == "log")
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0);
        if log_count > 0 {
            println!(
                "  Logs:      {} file(s) in {}",
                log_count,
                log_dir.display()
            );
        }
    }

    println!();
    Ok(())
}

/// Run diagnostics
pub(crate) async fn cmd_doctor(config: &crate::config::Config, fix: bool) -> Result<()> {
    use crate::db::Database;

    let version = env!("CARGO_PKG_VERSION");
    println!("🦀 OpenCrabs Doctor v{version}\n");

    let mut pass = 0u32;
    let mut fail = 0u32;
    let mut warn = 0u32;

    // 1. Config file
    let config_path = crate::config::Config::system_config_path();
    if let Some(ref p) = config_path
        && p.exists()
    {
        println!("  ✅ Config file: {}", p.display());
        pass += 1;
    } else {
        println!("  ❌ Config file: not found (run `opencrabs init` or `opencrabs onboard`)");
        fail += 1;
    }

    // 2. Keys file
    let keys_path = crate::config::keys_path();
    if keys_path.exists() {
        println!("  ✅ Keys file: {}", keys_path.display());
        pass += 1;
    } else {
        println!("  ⚠️  Keys file: not found (API keys stored in config.toml or env vars)");
        warn += 1;
    }

    // 3. Provider
    match crate::brain::provider::create_provider(config).await {
        Ok(provider) => {
            println!(
                "  ✅ Provider: {} (model: {})",
                provider.name(),
                provider.default_model()
            );
            pass += 1;
        }
        Err(e) => {
            println!("  ❌ Provider: {}", e);
            fail += 1;
        }
    }

    // 4. Database
    let db_path = &config.database.path;
    if db_path.exists() {
        match Database::connect(db_path).await {
            Ok(db) => {
                // #1779: the pre-migration guard refuses on a damaged image, and
                // `.ok()` threw that refusal away, so doctor printed
                // "✅ Database" over a corrupt file. That is precisely the
                // blindness the rpi5 operator hit while looking for a signal, so
                // the refusal is reported here instead of discarded.
                //
                // Note what this does NOT fix: `doctor` still returns Ok and
                // still exits 0 with failures counted, so a script cannot gate on
                // it. That is a separate defect about the whole command, not
                // about the database check.
                match db.run_migrations().await {
                    Ok(()) => {
                        println!("  ✅ Database: {}", db_path.display());
                        pass += 1;
                        // #1779 defect 4: doctor reported the image as healthy and
                        // nothing about whether a copy of it existed to restore.
                        // The snapshot line makes the recovery asset visible on the
                        // one command an operator reaches for mid-incident.
                        let snapshot_dir = crate::db::migration_snapshot::snapshot_dir();
                        let newest = crate::db::migration_snapshot::newest_snapshot(&snapshot_dir);
                        if crate::db::db_integrity_failed_now() {
                            println!(
                                "  ⚠️  Database integrity: check FAILED after migrations; \
                                 newest snapshot: {}",
                                crate::db::migration_snapshot::newest_snapshot_note(
                                    newest.as_deref()
                                )
                            );
                            warn += 1;
                        } else {
                            println!(
                                "  📸 Database snapshot: {}",
                                crate::db::migration_snapshot::newest_snapshot_note(
                                    newest.as_deref()
                                )
                            );
                        }
                        if fix {
                            apply_fixes(config, db.pool()).await;
                        }
                    }
                    Err(e) => {
                        println!("  ❌ Database: {e:#}");
                        fail += 1;
                    }
                }
            }
            Err(e) => {
                println!("  ❌ Database: failed to connect — {}", e);
                fail += 1;
            }
        }
    } else {
        println!(
            "  ❌ Database: not found at {} (run `opencrabs db init`)",
            db_path.display()
        );
        fail += 1;
    }

    // 5. Brain files
    let brain_path = BrainLoader::resolve_path();
    if brain_path.exists() {
        let brain_files: Vec<&str> = ["persona.md", "system.md", "USER.md", "MEMORY.md"]
            .iter()
            .filter(|f| brain_path.join(f).exists())
            .copied()
            .collect();
        if brain_files.is_empty() {
            println!(
                "  ⚠️  Brain: directory exists but no brain files found at {}",
                brain_path.display()
            );
            warn += 1;
        } else {
            println!(
                "  ✅ Brain: {} files ({})",
                brain_files.len(),
                brain_files.join(", ")
            );
            pass += 1;
        }
    } else {
        println!(
            "  ⚠️  Brain: directory not found at {}",
            brain_path.display()
        );
        warn += 1;
    }

    // 6. Channels
    println!();
    println!("  Channels:");

    // Telegram
    if config.channels.telegram.enabled {
        if config
            .channels
            .telegram
            .token
            .as_ref()
            .is_some_and(|t| !t.is_empty())
        {
            println!("    ✅ Telegram: enabled, token set");
            pass += 1;
        } else {
            println!("    ❌ Telegram: enabled but no bot token");
            fail += 1;
        }
    } else {
        println!("    ⬚  Telegram: disabled");
    }

    // Discord
    if config.channels.discord.enabled {
        if config
            .channels
            .discord
            .token
            .as_ref()
            .is_some_and(|t| !t.is_empty())
        {
            println!("    ✅ Discord: enabled, token set");
            pass += 1;
        } else {
            println!("    ❌ Discord: enabled but no bot token");
            fail += 1;
        }
    } else {
        println!("    ⬚  Discord: disabled");
    }

    // Slack
    if config.channels.slack.enabled {
        let has_bot = config
            .channels
            .slack
            .token
            .as_ref()
            .is_some_and(|t| !t.is_empty());
        let has_app = config
            .channels
            .slack
            .app_token
            .as_ref()
            .is_some_and(|t| !t.is_empty());
        if has_bot && has_app {
            println!("    ✅ Slack: enabled, bot + app tokens set");
            pass += 1;
        } else {
            let missing: Vec<&str> = [
                (!has_bot).then_some("bot token"),
                (!has_app).then_some("app token"),
            ]
            .into_iter()
            .flatten()
            .collect();
            println!("    ❌ Slack: enabled but missing {}", missing.join(", "));
            fail += 1;
        }
    } else {
        println!("    ⬚  Slack: disabled");
    }

    // WhatsApp
    if config.channels.whatsapp.enabled {
        println!("    ✅ WhatsApp: enabled (pairs at runtime)");
        pass += 1;
    } else {
        println!("    ⬚  WhatsApp: disabled");
    }

    // Trello
    if config.channels.trello.enabled {
        let has_token = config
            .channels
            .trello
            .token
            .as_ref()
            .is_some_and(|t| !t.is_empty());
        let has_app_token = config
            .channels
            .trello
            .app_token
            .as_ref()
            .is_some_and(|t| !t.is_empty());
        if has_token && has_app_token {
            println!("    ✅ Trello: enabled, token + app token set");
            pass += 1;
        } else {
            println!("    ❌ Trello: enabled but missing credentials");
            fail += 1;
        }
    } else {
        println!("    ⬚  Trello: disabled");
    }

    // 7. Memory embeddings (#1067). Counted into the summary, not just printed:
    // a store where every document is waiting on an embedding is a broken
    // install that reported all-green because search fell back to FTS and kept
    // answering.
    println!();
    println!("  Memory:");
    for line in crate::memory::doctor_lines() {
        let marker = if line.contains("MISSING") || line.contains("FAILING") {
            fail += 1;
            "❌"
        } else if line.contains("awaiting embedding") && !line.contains(", 0 awaiting") {
            warn += 1;
            "⚠️ "
        } else {
            "  "
        };
        println!("    {marker} {line}");
    }

    // 8. Config keys (#1725). Reads config.toml through the compiled struct
    // with serde_ignored — any key the struct discards is reported. A user
    // who follows the Telegram alert's "Run /doctor" advice is no longer told
    // everything is fine when the file has stale or typoed keys.
    println!();
    println!("  Config:");
    {
        let config_path = crate::config::opencrabs_home().join("config.toml");
        if config_path.exists() {
            match std::fs::read_to_string(&config_path) {
                Ok(raw) => match crate::config::sections::ignored_key_paths(&raw) {
                    Ok(paths) if paths.is_empty() => {
                        println!("    ✅ All keys recognized");
                        pass += 1;
                    }
                    Ok(paths) => {
                        println!(
                            "    ❌ {} unrecognized key(s): {}",
                            paths.len(),
                            paths.join(", ")
                        );
                        fail += 1;
                    }
                    Err(e) => {
                        println!("    ❌ Could not parse config.toml: {e}");
                        fail += 1;
                    }
                },
                Err(e) => {
                    println!("    ❌ Could not read config.toml: {e}");
                    fail += 1;
                }
            }
        } else {
            println!("    ⬚  No config.toml found (using defaults)");
        }
    }

    // 9. CLI tools in PATH
    println!();
    println!("  CLI tools:");
    for (name, desc) in [
        ("claude", "Claude CLI provider"),
        ("opencode", "OpenCode CLI provider"),
        ("codex", "Codex CLI provider"),
        ("docker", "container runtime"),
        ("ffmpeg", "media processing"),
        ("gh", "GitHub CLI"),
    ] {
        if which::which(name).is_ok() {
            println!("    ✅ {name}: found ({desc})");
            pass += 1;
        } else {
            println!("    ⬚  {name}: not found ({desc})");
        }
    }

    // 8. Dynamic tools
    let tools_path = brain_path.join("tools.toml");
    if tools_path.exists()
        && let Ok(contents) = std::fs::read_to_string(&tools_path)
    {
        let count = contents.matches("[[tools]]").count();
        if count > 0 {
            println!();
            println!("  ✅ Dynamic tools: {} defined in tools.toml", count);
            pass += 1;
        }
    }

    // Summary
    println!();
    if fail == 0 {
        println!("  ✅ All checks passed ({pass} ok, {warn} warnings)");
    } else {
        println!("  {fail} issue(s), {pass} ok, {warn} warning(s)");
    }
    println!();

    Ok(())
}

/// Load configuration from file or defaults
pub(crate) async fn load_config(config_path: Option<&str>) -> Result<crate::config::Config> {
    use crate::config::Config;

    let config = if let Some(path) = config_path {
        tracing::info!("Loading configuration from custom path: {}", path);
        Config::load_from_path(path)?
    } else {
        tracing::debug!("Loading default configuration");
        // Schema migration is a startup step, not a side effect of reading
        // config (#912). This is the one place every CLI entry point passes
        // through, so it stays exactly as often as it used to run in practice.
        Config::migrate_config_files();
        Config::load()?
    };

    // Validate configuration
    config.validate()?;

    Ok(config)
}

/// Initialize configuration file
pub(crate) async fn cmd_init(_config: &crate::config::Config, force: bool) -> Result<()> {
    use crate::config::Config;

    println!("🦀 OpenCrabs Configuration Initialization\n");

    let config_path = Config::system_config_path()
        .unwrap_or_else(|| crate::config::opencrabs_home().join("config.toml"));

    // Check if config already exists
    if config_path.exists() && !force {
        anyhow::bail!(
            "Configuration file already exists at: {}\nUse --force to overwrite",
            config_path.display()
        );
    }

    // Write the shipped example, not the in-memory default (#2093). `init` is
    // the command that owns creating this file, and what it leaves behind has
    // to be the documented one: a section per provider, disabled, with its
    // options inline (#2079). `Config::default().save()` wrote the handful of
    // sections that happened to be set and dropped every comment with them.
    crate::config::seed::write_config_example(&config_path)?;

    println!("✅ Configuration initialized at: {}", config_path.display());
    println!("\n📝 Next steps:");
    println!("   1. Edit the config file to add your API keys");
    println!("   2. Set ANTHROPIC_API_KEY environment variable");
    println!("   3. Run 'opencrabs' or 'opencrabs chat' to start");

    Ok(())
}

/// Show configuration
pub(crate) async fn cmd_config(config: &crate::config::Config, show_secrets: bool) -> Result<()> {
    println!("🦀 OpenCrabs Configuration\n");

    if show_secrets {
        println!("{:#?}", config);
    } else {
        println!("Database: {}", config.database.path.display());
        println!("Log level: {}", config.logging.level);
        println!("\nProviders:");

        if let Some(ref anthropic) = config.providers.anthropic {
            println!(
                "  - anthropic: {}",
                anthropic
                    .default_model
                    .as_ref()
                    .unwrap_or(&"claude-3-5-sonnet-20240620".to_string())
            );
            println!(
                "    API Key: {}",
                if anthropic.api_key.is_some() {
                    "[SET]"
                } else {
                    "[NOT SET]"
                }
            );
        }

        if let Some(ref openai) = config.providers.openai {
            println!(
                "  - openai: {}",
                openai
                    .default_model
                    .as_ref()
                    .unwrap_or(&"gpt-4".to_string())
            );
            println!(
                "    API Key: {}",
                if openai.api_key.is_some() {
                    "[SET]"
                } else {
                    "[NOT SET]"
                }
            );
        }

        println!("\n💡 Use --show-secrets to display API keys");
    }

    Ok(())
}

/// Database operations
pub(crate) async fn cmd_db(config: &crate::config::Config, operation: DbCommands) -> Result<()> {
    use crate::db::Database;

    match operation {
        DbCommands::Init => {
            println!("🗄️  Initializing database...");
            let db = Database::connect(&config.database.path).await?;
            db.run_migrations().await?;
            println!(
                "✅ Database initialized at: {}",
                config.database.path.display()
            );
            Ok(())
        }
        DbCommands::Stats => {
            println!("📊 Database Statistics\n");
            let db = Database::connect(&config.database.path).await?;

            let (session_count, message_count, file_count) = db
                .pool()
                .get()
                .await
                .context("Failed to get connection")?
                .interact(|conn| {
                    let sessions: i64 =
                        conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
                    let messages: i64 =
                        conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
                    let files: i64 =
                        conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
                    Ok::<_, rusqlite::Error>((sessions, messages, files))
                })
                .await
                .map_err(crate::db::interact_err)?
                .context("Failed to query stats")?;

            println!("Sessions: {}", session_count);
            println!("Messages: {}", message_count);
            println!("Files: {}", file_count);

            Ok(())
        }
        DbCommands::Clear { force } => {
            let db = Database::connect(&config.database.path).await?;

            let (session_count, message_count, file_count) = db
                .pool()
                .get()
                .await
                .context("Failed to get connection")?
                .interact(|conn| {
                    let sessions: i64 =
                        conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
                    let messages: i64 =
                        conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
                    let files: i64 =
                        conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
                    Ok::<_, rusqlite::Error>((sessions, messages, files))
                })
                .await
                .map_err(crate::db::interact_err)?
                .context("Failed to query counts")?;

            if session_count == 0 && message_count == 0 && file_count == 0 {
                println!("✨ Database is already empty");
                return Ok(());
            }

            println!("⚠️  WARNING: This will permanently delete ALL data:\n");
            println!("   • {} sessions", session_count);
            println!("   • {} messages", message_count);
            println!("   • {} files", file_count);
            println!();

            // Confirmation prompt
            if !force {
                use std::io::{self, Write};
                print!("Type 'yes' to confirm deletion: ");
                io::stdout().flush()?;

                let mut input = String::new();
                io::stdin().read_line(&mut input)?;

                if input.trim().to_lowercase() != "yes" {
                    println!("❌ Cancelled - no data was deleted");
                    return Ok(());
                }
            }

            // Clear all tables
            println!("\n🗑️  Clearing database...");

            // Delete in correct order to respect foreign key constraints
            db.pool()
                .get()
                .await
                .context("Failed to get connection")?
                .interact(|conn| {
                    conn.execute("DELETE FROM messages", [])?;
                    conn.execute("DELETE FROM files", [])?;
                    conn.execute("DELETE FROM sessions", [])?;
                    Ok::<_, rusqlite::Error>(())
                })
                .await
                .map_err(crate::db::interact_err)?
                .context("Failed to clear database")?;

            println!(
                "✅ Successfully cleared {} sessions, {} messages, and {} files",
                session_count, message_count, file_count
            );

            Ok(())
        }
    }
}

/// Run a single command non-interactively
pub(crate) async fn cmd_run(
    config: &crate::config::Config,
    prompt: String,
    auto_approve: bool,
    format: OutputFormat,
    session_id: Option<String>,
    quiet: bool,
) -> Result<()> {
    use crate::{
        brain::{agent::AgentService, tools::registry::ToolRegistry},
        db::Database,
        services::{ServiceContext, SessionService},
    };

    tracing::info!("Running non-interactive command: {}", prompt);

    // Initialize database
    let db = Database::connect(&config.database.path).await?;
    db.run_migrations().await?;

    // Select provider based on configuration using factory
    let provider = crate::brain::provider::create_provider(config).await?;

    // Create tool registry (Arc-wrapped early so SpawnAgentTool can reference it).
    // Core + runtime tools come from the shared tool_setup helpers so this
    // headless path never drifts from the TUI/daemon tool set. `true` =
    // headless (#129): session_notify/suggest_options stay unregistered.
    let tool_registry = Arc::new(ToolRegistry::new());
    let subagent_manager =
        crate::cli::tool_setup::register_core_agent_tools(&tool_registry, &db, config, true);

    // Build dynamic system brain from workspace files
    let brain_path = BrainLoader::resolve_path();
    let brain_loader = BrainLoader::new(brain_path.clone());
    let runtime_info = RuntimeInfo {
        model: Some(provider.default_model().to_string()),
        provider: Some(provider.name().to_string()),
        // Collapse $HOME → ~ for the same reason as the TUI runtime
        // info builder: don't leak username, save tokens, keep cache
        // keys stable across machines.
        working_directory: Some(crate::brain::tools::error::collapse_home(
            &crate::utils::cwd::launch_cwd(),
        )),
    };
    // Feedback/performance digest stays out of the LLM context — it's a
    // maintenance warning in ~/.opencrabs/rsi/digest.md, not conversation input.
    let mut system_brain = brain_loader.build_system_brain(Some(&runtime_info));
    if config.agent.lazy_tools {
        // Headless one-shot (#129): the roster drops the interactive-only
        // pair, matching the registry gate below.
        system_brain.push_str(&crate::brain::tools::catalog::tool_access_prompt(true));
    }
    // Headless-only preamble (#129): the one-shot run's final message is the
    // ONLY thing the caller sees — inject the self-containedness law.
    system_brain.push_str(crate::cli::tool_setup::HEADLESS_PREAMBLE);

    // Create service context and agent service
    let service_context = ServiceContext::new(db.pool().clone());
    let agent_service = AgentService::new(provider.clone(), service_context.clone(), config)
        .await
        .with_tool_registry(tool_registry.clone())
        .with_system_brain(system_brain)
        // --auto-approve executes tools without an approval prompt. Single-shot
        // `run` has no TTY to prompt on, so without the flag only non-approval
        // tools run and approval-gated ones are denied by the loop.
        .with_auto_approve_tools(auto_approve)
        // #129: backstop flag — interactive-only tools hard-error in the
        // tool loop even if a future regression re-registers them.
        .with_headless(true)
        .with_subagent_manager(subagent_manager);

    // Create or resume session (#1368): `agent --session <prefix|uuid>`
    // continues an existing conversation instead of starting fresh. The id
    // is echoed in every output format below so scripts can close the loop
    // without scraping `session list`.
    let session_service = SessionService::new(service_context);
    let session = crate::cli::session_resolve::resolve_or_create_session(
        &session_service,
        session_id.as_deref(),
        "CLI Run",
    )
    .await?;
    let short_id = &session.id.to_string()[..8];

    // Send through the full tool loop so headless runs actually execute tools
    // (#492). Plain send_message() is a single completion with no tool
    // execution — run/agent must invoke tools like every other surface.
    // --quiet: stdout is the payload — no processing line, no progress
    // narration, no stats footer (machine callers parse stdout directly).
    if !quiet {
        println!("🤔 Processing...\n");
    }
    let progress = if quiet {
        None
    } else {
        Some(crate::cli::headless_callbacks::cli_progress_callback())
    };
    let response = agent_service
        .send_message_with_tools_and_callback(
            session.id, prompt, None, None,
            None, // no stdin prompt for single-shot run; auto_approve_tools gates it
            progress, "cli", None, None,
        )
        .await?;

    // Format and display output
    match format {
        OutputFormat::Text => {
            println!("{}", response.content);
            if !quiet {
                println!();
                println!(
                    "📊 Tokens: {}",
                    response.usage.input_tokens + response.usage.output_tokens
                );
                println!("💰 Cost: ${:.6}", response.cost);
                println!("🆔 Session: {short_id} (resume: opencrabs agent --session {short_id})");
            }
        }
        OutputFormat::Json => {
            let output = serde_json::json!({
                "content": response.content,
                "usage": {
                    "input_tokens": response.usage.input_tokens,
                    "output_tokens": response.usage.output_tokens,
                },
                "cost": response.cost,
                "model": response.model,
                "session_id": session.id.to_string(),
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        OutputFormat::Markdown => {
            println!("# Response\n");
            println!("{}\n", response.content);
            if !quiet {
                println!("---");
                println!(
                    "**Tokens:** {}",
                    response.usage.input_tokens + response.usage.output_tokens
                );
                println!("**Cost:** ${:.6}", response.cost);
                println!("**Session:** {short_id}");
            }
        }
    }

    Ok(())
}

/// Serve ACP (Agent Client Protocol) over stdio for agent-aware editors
/// (MonoCode, Zed). Same bootstrap as `run` — db, provider, tools, brain —
/// but stdout is reserved for JSON-RPC frames, so nothing here may print.
/// Approvals are NOT auto-approved: they round-trip to the client as
/// `session/request_permission`, which the editor's runtime mode answers.
pub(crate) async fn cmd_acp(config: &crate::config::Config, model: Option<String>) -> Result<()> {
    use crate::{
        brain::{agent::AgentService, tools::registry::ToolRegistry},
        db::Database,
        services::{ServiceContext, SessionService},
    };

    tracing::info!("Starting ACP server (stdio)");

    let db = Database::connect(&config.database.path).await?;
    db.run_migrations().await?;

    let provider = crate::brain::provider::create_provider(config).await?;

    // ACP is a LIVE user surface (the phone app, Zed): interactive registry
    // so `suggest_options` exists and its chips render client-side. See
    // `register_acp_agent_tools` for the full ruling, including why
    // `session_notify` is dropped.
    let tool_registry = Arc::new(ToolRegistry::new());
    let subagent_manager =
        crate::cli::tool_setup::register_acp_agent_tools(&tool_registry, &db, config);

    let brain_path = BrainLoader::resolve_path();
    let brain_loader = BrainLoader::new(brain_path.clone());
    let runtime_info = RuntimeInfo {
        model: Some(provider.default_model().to_string()),
        provider: Some(provider.name().to_string()),
        working_directory: Some(crate::brain::tools::error::collapse_home(
            &crate::utils::cwd::launch_cwd(),
        )),
    };
    let mut system_brain = brain_loader.build_system_brain(Some(&runtime_info));
    if config.agent.lazy_tools {
        // Interactive roster (headless = false) to match the registry above —
        // the headless roster omits `suggest_options` and would un-advertise
        // a tool this surface just registered.
        system_brain.push_str(&crate::brain::tools::catalog::tool_access_prompt(false));
    }

    crate::cli::tool_setup::register_runtime_tools(&tool_registry, config);

    // `session/steer` lands in this map; the tool loop drains it between
    // iterations via the message-queue callback.
    let steer = crate::acp::new_steer_map();
    let steer_for_queue = steer.clone();
    let queue_callback: crate::brain::agent::MessageQueueCallback = Arc::new(move |session_id| {
        let steer = steer_for_queue.clone();
        Box::pin(async move {
            steer
                .lock()
                .await
                .get_mut(&session_id)
                .and_then(std::collections::VecDeque::pop_front)
        })
    });

    let service_context = ServiceContext::new(db.pool().clone());
    let agent_service = AgentService::new(provider.clone(), service_context.clone(), config)
        .await
        .with_tool_registry(tool_registry.clone())
        .with_system_brain(system_brain)
        // Explicit (not the default): a future default flip to headless must
        // not silently strip the FOLLOW-UP SUGGESTIONS paragraph or hard-error
        // suggest_options on this interactive surface. Mirrors cmd_run's
        // explicit `.with_headless(true)`.
        .with_headless(false)
        .with_subagent_manager(subagent_manager)
        .with_message_queue_callback(Some(queue_callback));

    let session_service = SessionService::new(service_context.clone());
    let message_service = crate::services::MessageService::new(service_context);
    let server = crate::acp::AcpServer::new(
        Arc::new(agent_service),
        session_service,
        message_service,
        model,
        steer,
        Arc::new(config.clone()),
    );
    server.run().await
}

/// Log management commands
pub(crate) async fn cmd_logs(operation: LogCommands) -> Result<()> {
    use crate::logging;
    use std::io::{BufRead, BufReader};

    // Resolve the SAME directory the writer uses (~/.opencrabs/logs or the
    // DEBUG_LOGS_LOCATION override). The old code looked under the process's
    // CWD, so a daemon (whose working dir isn't home) always saw an empty
    // directory and reported "Log files: 0" (#190 secondary).
    let log_dir = logging::log_dir();

    match operation {
        LogCommands::Status => {
            println!("📊 OpenCrabs Logging Status\n");
            println!("Log directory: {}", log_dir.display());

            if log_dir.exists() {
                // Count log files and total size
                let mut file_count = 0;
                let mut total_size = 0u64;
                let mut newest_file: Option<std::path::PathBuf> = None;
                let mut newest_time = std::time::UNIX_EPOCH;

                for entry in std::fs::read_dir(&log_dir)? {
                    let entry = entry?;
                    let path = entry.path();
                    if entry.file_name().to_str().is_some_and(logging::is_log_file) {
                        file_count += 1;
                        if let Ok(metadata) = entry.metadata() {
                            total_size += metadata.len();
                            if let Ok(modified) = metadata.modified()
                                && modified > newest_time
                            {
                                newest_time = modified;
                                newest_file = Some(path);
                            }
                        }
                    }
                }

                println!("Status: ✅ Active");
                println!("Log files: {}", file_count);
                println!(
                    "Total size: {:.2} MB",
                    total_size as f64 / (1024.0 * 1024.0)
                );

                if let Some(newest) = newest_file {
                    println!("Latest log: {}", newest.display());
                }

                println!("\n💡 To enable debug logging, run with -d flag:");
                println!("   opencrabs -d");
            } else {
                println!("Status: ❌ No logs found");
                println!("\n💡 To enable debug logging, run with -d flag:");
                println!("   opencrabs -d");
                println!("\nThis will create log files in:");
                println!("   {}", log_dir.display());
            }

            Ok(())
        }

        LogCommands::View { lines } => {
            if let Some(log_path) = logging::get_log_path() {
                println!(
                    "📜 Viewing last {} lines of: {}\n",
                    lines,
                    log_path.display()
                );

                let file = std::fs::File::open(&log_path)?;
                let reader = BufReader::new(file);

                // Collect all lines then show last N
                let all_lines: Vec<String> = reader.lines().map_while(Result::ok).collect();
                let start = all_lines.len().saturating_sub(lines);

                for line in &all_lines[start..] {
                    println!("{}", line);
                }

                if all_lines.is_empty() {
                    println!("(empty log file)");
                }
            } else {
                println!("❌ No log files found.\n");
                println!("💡 Run OpenCrabs with -d flag to enable debug logging:");
                println!("   opencrabs -d");
            }

            Ok(())
        }

        LogCommands::Clean { days } => {
            println!("🧹 Cleaning up log files older than {} days...\n", days);

            match logging::cleanup_old_logs(days) {
                Ok(removed) => {
                    if removed > 0 {
                        println!("✅ Removed {} old log file(s)", removed);
                    } else {
                        println!("✅ No old log files to remove");
                    }
                }
                Err(e) => {
                    println!("❌ Error cleaning logs: {}", e);
                }
            }

            Ok(())
        }

        LogCommands::Open => {
            if !log_dir.exists() {
                println!("❌ Log directory does not exist: {}", log_dir.display());
                println!("\n💡 Run OpenCrabs with -d flag to enable debug logging:");
                println!("   opencrabs -d");
                return Ok(());
            }

            println!("📂 Opening log directory: {}", log_dir.display());

            #[cfg(target_os = "macos")]
            {
                std::process::Command::new("open")
                    .arg(&log_dir)
                    .spawn()
                    .context("Failed to open directory")?;
            }

            #[cfg(target_os = "linux")]
            {
                std::process::Command::new("xdg-open")
                    .arg(&log_dir)
                    .spawn()
                    .context("Failed to open directory")?;
            }

            #[cfg(target_os = "windows")]
            {
                std::process::Command::new("explorer")
                    .arg(&log_dir)
                    .spawn()
                    .context("Failed to open directory")?;
            }

            Ok(())
        }
    }
}

/// Interactive CLI agent — multi-turn conversation without TUI
pub(crate) async fn cmd_agent_interactive(
    config: &crate::config::Config,
    auto_approve: bool,
    session_id: Option<String>,
) -> Result<()> {
    use crate::{
        brain::{agent::AgentService, tools::registry::ToolRegistry},
        db::Database,
        services::{ServiceContext, SessionService},
    };
    use std::io::{self, Write};

    let db = Database::connect(&config.database.path).await?;
    db.run_migrations().await?;

    let provider = crate::brain::provider::create_provider(config).await?;

    // Core tools come from the shared tool_setup helper so this REPL path never
    // drifts from the TUI/daemon tool set. `false` = interactive (#129): the
    // REPL user sees mid-task output, so session_notify stays when [agent]
    // session_notify_enabled is on (#1840); suggest_options always stays.
    // Runtime tools (dynamic/browser) are added after the system brain is
    // built, below.
    let tool_registry = Arc::new(ToolRegistry::new());
    let subagent_manager =
        crate::cli::tool_setup::register_core_agent_tools(&tool_registry, &db, config, false);

    let brain_path = BrainLoader::resolve_path();
    let brain_loader = BrainLoader::new(brain_path);
    let runtime_info = RuntimeInfo {
        model: Some(provider.default_model().to_string()),
        provider: Some(provider.name().to_string()),
        working_directory: Some(crate::brain::tools::error::collapse_home(
            &crate::utils::cwd::launch_cwd(),
        )),
    };
    // Feedback/performance digest stays out of the LLM context — it's a
    // maintenance warning in ~/.opencrabs/rsi/digest.md, not conversation input.
    let mut system_brain = brain_loader.build_system_brain(Some(&runtime_info));
    if config.agent.lazy_tools {
        system_brain.push_str(&crate::brain::tools::catalog::tool_access_prompt(false));
    }

    // Headless-safe runtime tools (dynamic tools.toml tools, tool_manage, browser).
    crate::cli::tool_setup::register_runtime_tools(&tool_registry, config);

    let service_context = ServiceContext::new(db.pool().clone());
    // Background-task resume (#731): a detached long command that finishes pushes
    // its completion here; the select loop below picks it up and resumes the
    // session proactively, matching the TUI. The callback just forwards to the
    // channel — the loop (which owns the agent) runs the resume turn.
    let (bg_tx, mut bg_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::brain::agent::service::QueuedUserMessage>();
    let enqueue_cb: crate::brain::agent::service::MessageEnqueueCallback =
        std::sync::Arc::new(move |_session_id, msg| {
            let _ = bg_tx.send(msg);
        });
    let agent_service = AgentService::new(provider.clone(), service_context.clone(), config)
        .await
        .with_tool_registry(tool_registry.clone())
        .with_system_brain(system_brain)
        .with_message_enqueue_callback(Some(enqueue_cb))
        // --auto-approve runs tools without prompting; otherwise each
        // approval-gated tool prompts on stdin via stdin_approval_callback.
        .with_auto_approve_tools(auto_approve)
        .with_subagent_manager(subagent_manager);

    let session_service = SessionService::new(service_context);
    // Create or resume (#1368): `agent --session <prefix|uuid>` continues an
    // existing conversation instead of starting a fresh REPL.
    let session = crate::cli::session_resolve::resolve_or_create_session(
        &session_service,
        session_id.as_deref(),
        "CLI Agent",
    )
    .await?;

    let short_id = &session.id.to_string()[..8];
    println!(
        "🦀 OpenCrabs Agent — {} ({})",
        provider.name(),
        provider.default_model()
    );
    println!("Session: {short_id} — resume later with: opencrabs agent --session {short_id}");
    println!("Type /exit or Ctrl+D to quit\n");

    // Run one turn (a typed line or a background-task completion) and print its
    // result. Shared by both loop arms so they stay identical.
    async fn run_cli_turn(
        agent: &AgentService,
        session_id: uuid::Uuid,
        prompt: String,
        auto_approve: bool,
    ) {
        // Without --auto-approve, each approval-gated tool prompts on stdin.
        let approval = if auto_approve {
            None
        } else {
            Some(crate::cli::headless_callbacks::stdin_approval_callback())
        };
        match agent
            .send_message_with_tools_and_callback(
                session_id,
                prompt,
                None,
                None,
                approval,
                Some(crate::cli::headless_callbacks::cli_progress_callback()),
                "cli",
                None,
                None,
            )
            .await
        {
            Ok(response) => {
                println!("\n{}\n", response.content);
                println!(
                    "  tokens: {} | cost: ${:.6}\n",
                    response.usage.input_tokens + response.usage.output_tokens,
                    response.cost
                );
            }
            Err(e) => {
                eprintln!("\n  error: {e}\n");
            }
        }
    }

    // A single in-flight stdin read, kept alive across background interrupts so
    // a resume never spawns a second concurrent reader (which would race the
    // user's next line). `select!` waits on either that read or a finished
    // background task (#731).
    let mut pending_read: Option<tokio::task::JoinHandle<Option<String>>> = None;
    loop {
        if pending_read.is_none() {
            print!("❯ ");
            io::stdout().flush()?;
            pending_read = Some(tokio::task::spawn_blocking(|| {
                let mut input = String::new();
                match io::stdin().read_line(&mut input) {
                    Ok(0) => None, // EOF (Ctrl+D)
                    Ok(_) => Some(input),
                    Err(_) => None,
                }
            }));
        }
        let mut read = pending_read.take().expect("pending_read set above");

        tokio::select! {
            joined = &mut read => {
                let Some(line) = joined.ok().flatten() else {
                    println!();
                    break;
                };
                let input = line.trim().to_string();
                if input.is_empty() {
                    continue;
                }
                if input == "/exit" || input == "/quit" || input == "/q" {
                    break;
                }
                run_cli_turn(&agent_service, session.id, input, auto_approve).await;
            }
            Some(msg) = bg_rx.recv() => {
                // A detached command finished — resume proactively. Put the
                // in-flight stdin read back so the user's pending line survives.
                pending_read = Some(read);
                println!("\n⚙️  background task finished — resuming\n");
                run_cli_turn(&agent_service, session.id, msg.context_text, auto_approve).await;
            }
        }
    }

    Ok(())
}

/// Channel operations
pub(crate) async fn cmd_channel(
    config: &crate::config::Config,
    operation: ChannelCommands,
) -> Result<()> {
    match operation {
        ChannelCommands::List => {
            println!("🦀 Configured Channels\n");

            let channels: Vec<(&str, bool, &str)> = vec![
                (
                    "Telegram",
                    config.channels.telegram.enabled,
                    if config
                        .channels
                        .telegram
                        .token
                        .as_ref()
                        .is_some_and(|t| !t.is_empty())
                    {
                        "token set"
                    } else {
                        "no token"
                    },
                ),
                (
                    "Discord",
                    config.channels.discord.enabled,
                    if config
                        .channels
                        .discord
                        .token
                        .as_ref()
                        .is_some_and(|t| !t.is_empty())
                    {
                        "token set"
                    } else {
                        "no token"
                    },
                ),
                (
                    "Slack",
                    config.channels.slack.enabled,
                    if config
                        .channels
                        .slack
                        .token
                        .as_ref()
                        .is_some_and(|t| !t.is_empty())
                        && config
                            .channels
                            .slack
                            .app_token
                            .as_ref()
                            .is_some_and(|t| !t.is_empty())
                    {
                        "bot + app tokens set"
                    } else {
                        "missing tokens"
                    },
                ),
                (
                    "WhatsApp",
                    config.channels.whatsapp.enabled,
                    "pairs at runtime",
                ),
                (
                    "Trello",
                    config.channels.trello.enabled,
                    if config
                        .channels
                        .trello
                        .token
                        .as_ref()
                        .is_some_and(|t| !t.is_empty())
                    {
                        "token set"
                    } else {
                        "no token"
                    },
                ),
            ];

            for (name, enabled, detail) in channels {
                let status = if enabled { "✅" } else { "⬚ " };
                let state = if enabled { "enabled" } else { "disabled" };
                println!("  {status} {name:<12} {state:<10} ({detail})");
            }
            println!();
            Ok(())
        }
        ChannelCommands::Doctor => {
            println!("🦀 Channel Health Check\n");
            cmd_doctor_channels(config);
            println!();
            Ok(())
        }
        #[cfg(feature = "telegram-userbot")]
        ChannelCommands::UserbotLogin { code } => {
            crate::channels::telegram::userbot::login::cmd_userbot_login(config, code).await
        }
        #[cfg(not(feature = "telegram-userbot"))]
        ChannelCommands::UserbotLogin { .. } => {
            anyhow::bail!(
                "this build lacks the telegram-userbot feature — rebuild with --features telegram-userbot"
            );
        }
        #[cfg(feature = "telegram-userbot")]
        ChannelCommands::UserbotDownload {
            peer,
            message,
            outdir,
            list,
        } => {
            crate::channels::telegram::userbot::download::cmd_userbot_download(
                config,
                &peer,
                &message,
                outdir.as_deref(),
                list,
            )
            .await
        }
        #[cfg(not(feature = "telegram-userbot"))]
        ChannelCommands::UserbotDownload { .. } => {
            anyhow::bail!(
                "this build lacks the telegram-userbot feature — rebuild with --features telegram-userbot"
            );
        }
    }
}

/// Shared channel diagnostics
/// #1114: run every repair sweep and print the audit trail. Invoked only
/// via explicit `--fix`; the startup sweep has its own gate ([doctor] auto_fix).
async fn apply_fixes(config: &crate::config::Config, pool: &crate::db::Pool) {
    println!("\n  🔧 Repairs (--fix):");
    let roots = marker_roots(pool).await;
    match crate::cli::doctor_fix::run_all(pool, &roots, &crate::config::opencrabs_home()).await {
        Ok(reports) if reports.is_empty() => println!("    ✅ Nothing to repair"),
        Ok(reports) => {
            for r in &reports {
                println!("    ✅ [{}] {}", r.action, r.detail);
            }
            println!("    {} repair(s) applied", reports.len());
        }
        Err(e) => println!("    ❌ Repair sweep failed: {e:#}"),
    }
    let _ = config;
}

/// Every directory that can hold `.opencrabs_plan_*.preinit` markers: the
/// profile-default session dir, the legacy flat dir, and each project-bound
/// session dir (`<projects_dir>/<slug>/session/`).
pub(crate) async fn marker_roots(pool: &crate::db::Pool) -> Vec<std::path::PathBuf> {
    let mut roots = vec![
        crate::config::opencrabs_home().join("session"),
        crate::config::opencrabs_home()
            .join("agents")
            .join("session"),
    ];
    let Ok(conn) = pool.get().await else {
        return roots;
    };
    let names = conn
        .interact(|conn| {
            let Ok(mut stmt) = conn.prepare(
                "SELECT DISTINCT p.name FROM projects p JOIN sessions s ON s.project_id = p.id",
            ) else {
                return Vec::new();
            };
            stmt.query_map([], |r| r.get::<_, String>(0))
                .map(|rows| rows.flatten().collect())
                .unwrap_or_default()
        })
        .await
        .unwrap_or_default();
    for name in names {
        roots.push(
            crate::services::ProjectService::projects_dir()
                .join(crate::services::file::slugify_project_name(&name))
                .join("session"),
        );
    }
    roots
}

fn cmd_doctor_channels(config: &crate::config::Config) {
    if config.channels.telegram.enabled {
        if config
            .channels
            .telegram
            .token
            .as_ref()
            .is_some_and(|t| !t.is_empty())
        {
            println!("  ✅ Telegram: enabled, token set");
        } else {
            println!("  ❌ Telegram: enabled but no bot token");
        }
    } else {
        println!("  ⬚  Telegram: disabled");
    }

    if config.channels.discord.enabled {
        if config
            .channels
            .discord
            .token
            .as_ref()
            .is_some_and(|t| !t.is_empty())
        {
            println!("  ✅ Discord: enabled, token set");
        } else {
            println!("  ❌ Discord: enabled but no bot token");
        }
    } else {
        println!("  ⬚  Discord: disabled");
    }

    if config.channels.slack.enabled {
        let has_bot = config
            .channels
            .slack
            .token
            .as_ref()
            .is_some_and(|t| !t.is_empty());
        let has_app = config
            .channels
            .slack
            .app_token
            .as_ref()
            .is_some_and(|t| !t.is_empty());
        if has_bot && has_app {
            println!("  ✅ Slack: enabled, bot + app tokens set");
        } else {
            let missing: Vec<&str> = [
                (!has_bot).then_some("bot token"),
                (!has_app).then_some("app token"),
            ]
            .into_iter()
            .flatten()
            .collect();
            println!("  ❌ Slack: enabled but missing {}", missing.join(", "));
        }
    } else {
        println!("  ⬚  Slack: disabled");
    }

    if config.channels.whatsapp.enabled {
        println!("  ✅ WhatsApp: enabled (pairs at runtime)");
    } else {
        println!("  ⬚  WhatsApp: disabled");
    }

    if config.channels.trello.enabled {
        let has_token = config
            .channels
            .trello
            .token
            .as_ref()
            .is_some_and(|t| !t.is_empty());
        let has_app_token = config
            .channels
            .trello
            .app_token
            .as_ref()
            .is_some_and(|t| !t.is_empty());
        if has_token && has_app_token {
            println!("  ✅ Trello: enabled, token + app token set");
        } else {
            println!("  ❌ Trello: enabled but missing credentials");
        }
    } else {
        println!("  ⬚  Trello: disabled");
    }
}

/// Memory operations
pub(crate) async fn cmd_memory(operation: MemoryCommands) -> Result<()> {
    let brain_path = BrainLoader::resolve_path();

    match operation {
        MemoryCommands::List => {
            println!("🦀 Memory Files\n");

            let brain_files = [
                "MEMORY.md",
                "USER.md",
                "AGENTS.md",
                "TOOLS.md",
                "SOUL.md",
                "persona.md",
                "system.md",
            ];

            println!("  Brain files:");
            for name in &brain_files {
                let path = brain_path.join(name);
                if path.exists() {
                    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    println!("    ✅ {name:<16} ({size} bytes)");
                }
            }

            let memory_dir = brain_path.join("memory");
            if memory_dir.exists() {
                let mut count = 0;
                let mut total_size = 0u64;
                if let Ok(entries) = std::fs::read_dir(&memory_dir) {
                    for entry in entries.flatten() {
                        if entry
                            .path()
                            .extension()
                            .is_some_and(|e| e == "md" || e == "txt")
                        {
                            count += 1;
                            total_size += entry.metadata().map(|m| m.len()).unwrap_or(0);
                        }
                    }
                }
                if count > 0 {
                    println!(
                        "\n  Memory directory: {count} file(s), {:.1} KB",
                        total_size as f64 / 1024.0
                    );
                    println!("    {}", memory_dir.display());
                }
            }

            println!();
            Ok(())
        }
        MemoryCommands::Get { name } => {
            let name = if name.ends_with(".md") {
                name
            } else {
                format!("{name}.md")
            };

            let path = brain_path.join(&name);
            let path = if path.exists() {
                path
            } else {
                let alt = brain_path.join("memory").join(&name);
                if alt.exists() {
                    alt
                } else {
                    anyhow::bail!("Memory file not found: {name}");
                }
            };

            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            println!("{content}");
            Ok(())
        }
        MemoryCommands::Stats => {
            println!("🦀 Memory Statistics\n");

            let brain_files: Vec<_> = ["MEMORY.md", "USER.md", "AGENTS.md", "TOOLS.md", "SOUL.md"]
                .iter()
                .filter(|f| brain_path.join(f).exists())
                .collect();

            let brain_size: u64 = brain_files
                .iter()
                .map(|f| {
                    std::fs::metadata(brain_path.join(f))
                        .map(|m| m.len())
                        .unwrap_or(0)
                })
                .sum();
            println!(
                "  Brain files:    {} ({:.1} KB)",
                brain_files.len(),
                brain_size as f64 / 1024.0
            );

            let memory_dir = brain_path.join("memory");
            if memory_dir.exists() {
                let (count, size) = std::fs::read_dir(&memory_dir)
                    .map(|rd| {
                        rd.flatten().fold((0u32, 0u64), |(c, s), e| {
                            (c + 1, s + e.metadata().map(|m| m.len()).unwrap_or(0))
                        })
                    })
                    .unwrap_or((0, 0));
                println!("  Memory entries: {count} ({:.1} KB)", size as f64 / 1024.0);
            }

            println!("  Brain path:     {}", brain_path.display());
            println!();
            Ok(())
        }
        MemoryCommands::Prune {
            apply,
            max_age_days,
        } => {
            use crate::brain::tools::epistemic;

            let days = max_age_days.unwrap_or(90);
            let mode = if apply { "APPLY" } else { "DRY RUN" };
            println!("🦀 Memory Prune ({mode}, cold = 0 hits and unseen for {days}+ days)\n");

            // Cold beliefs: same yardstick delete_cold_beliefs will apply.
            let cold_beliefs = epistemic::list_cold_beliefs(days);
            println!("  Cold beliefs: {}", cold_beliefs.len());
            for belief in &cold_beliefs {
                let last_seen = belief.last_used.unwrap_or(belief.source.recorded_at);
                let age = (chrono::Utc::now() - last_seen).num_days();
                println!(
                    "    - {} [{}], {} hits, last seen {}d ago",
                    belief.key,
                    belief.confidence.label(),
                    belief.hits,
                    age
                );
            }

            // Cold sections: what archive_cold_sections would retire.
            let cold_sections = epistemic::list_cold_sections(days);
            println!("\n  Cold MEMORY.md sections: {}", cold_sections.len());
            for section in &cold_sections {
                println!(
                    "    - {} ({} bytes) -> memory/archive/",
                    section.heading.trim(),
                    section.rendered.len()
                );
            }

            if !apply {
                if cold_beliefs.is_empty() && cold_sections.is_empty() {
                    println!("\n  Nothing cold enough to prune.");
                } else {
                    println!(
                        "\n  Dry run: nothing written. Re-run with --apply to archive sections and delete cold beliefs."
                    );
                }
                println!();
                return Ok(());
            }

            // Apply: archive first (sections take their beliefs with them),
            // then sweep whatever cold beliefs remain.
            let report = epistemic::archive_cold_sections(days)
                .map_err(|e| anyhow::anyhow!("memory prune: {e}"))?;
            let deleted = epistemic::delete_cold_beliefs(days);

            if report.skipped {
                println!("\n  No sections were cold enough to archive.");
            } else {
                let archive_target = report
                    .archive_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "memory/archive/".to_string());
                println!(
                    "\n  Archived {} section(s) to {}",
                    report.archived.len(),
                    archive_target
                );
                if let Some(backup) = &report.backup {
                    println!("  Backup: {}", backup.display());
                }
            }
            println!("  Deleted {} cold belief(s)", deleted.len());
            for key in &deleted {
                println!("    - {key}");
            }
            println!();
            Ok(())
        }
    }
}

/// Session management
pub(crate) async fn cmd_session(
    config: &crate::config::Config,
    operation: SessionCommands,
) -> Result<()> {
    use crate::{
        db::Database,
        services::{ServiceContext, SessionService},
    };

    let db = Database::connect(&config.database.path).await?;
    db.run_migrations().await?;
    let service_context = ServiceContext::new(db.pool().clone());
    let session_svc = SessionService::new(service_context);

    match operation {
        SessionCommands::List { all } => {
            use crate::db::repository::SessionListOptions;

            println!("🦀 Sessions\n");
            let options = SessionListOptions {
                include_archived: all,
                ..Default::default()
            };
            let sessions = session_svc.list_sessions(options).await?;

            if sessions.is_empty() {
                println!("  No sessions found");
            } else {
                for s in &sessions {
                    let title = s.title.as_deref().unwrap_or("untitled");
                    let provider = s.provider_name.as_deref().unwrap_or("-");
                    let model = s.model.as_deref().unwrap_or("-");
                    let tokens = s.token_count;
                    let archived = if s.archived_at.is_some() {
                        " [archived]"
                    } else {
                        ""
                    };
                    println!(
                        "  {} {:<30} {}/{} ({}tok){archived}",
                        &s.id.to_string()[..8],
                        title,
                        provider,
                        model,
                        tokens,
                    );
                }
                println!("\n  {} session(s)", sessions.len());
            }
            println!();
            Ok(())
        }
        SessionCommands::SetModel {
            pair,
            id,
            name,
            all,
        } => {
            use crate::cli::session_set_model::{parse_pair, resolve_targets};
            use crate::db::repository::SessionListOptions;

            let (provider, model) = parse_pair(&pair).map_err(anyhow::Error::msg)?;
            if !crate::brain::provider::factory::is_known_provider_name(config, &provider) {
                anyhow::bail!(
                    "unknown provider '{provider}' — it must be a configured provider \
                     section (e.g. openrouter, minimax, or a [providers.custom.<name>] key)"
                );
            }

            let sessions = session_svc
                .list_sessions(SessionListOptions {
                    include_archived: true,
                    ..Default::default()
                })
                .await?;
            let targets = resolve_targets(&sessions, id.as_deref(), name.as_deref(), all)
                .map_err(anyhow::Error::msg)?;

            let mut updated = 0usize;
            for sid in &targets {
                let Some(mut session) = session_svc.get_session(*sid).await? else {
                    println!("  ⚠ session {sid} disappeared mid-run — skipped");
                    continue;
                };
                session.provider_name = Some(provider.clone());
                session.model = Some(model.clone());
                session_svc.update_session(&session).await?;
                updated += 1;
                println!(
                    "  ✓ {} {:<30} → {}/{}",
                    &sid.to_string()[..8],
                    session.title.as_deref().unwrap_or("untitled"),
                    provider,
                    model,
                );
            }
            println!(
                "\n🦀 {updated} session(s) set to {provider}/{model}. A running instance \
                 applies it on each session's next message."
            );
            Ok(())
        }
        SessionCommands::Get { id } => {
            let uuid =
                crate::cli::session_resolve::resolve_session_id_with_service(&session_svc, &id)
                    .await?;
            match session_svc.get_session(uuid).await? {
                Some(s) => {
                    println!("🦀 Session {}\n", s.id);
                    // Labels padded to the longest one so the values line up;
                    // the token figure is a lifetime accumulator, not the size
                    // of the context window, so it says so (#2180).
                    println!(
                        "  Title:           {}",
                        s.title.as_deref().unwrap_or("untitled")
                    );
                    println!(
                        "  Provider:        {}",
                        s.provider_name.as_deref().unwrap_or("-")
                    );
                    println!("  Model:           {}", s.model.as_deref().unwrap_or("-"));
                    println!("  Lifetime tokens: {}", s.token_count);
                    println!("  Cost:            ${:.6}", s.total_cost);
                    println!("  Archived:        {}", s.archived_at.is_some());
                    println!("  Created:         {}", s.created_at);
                    println!("  Updated:         {}", s.updated_at);
                    println!();
                }
                None => println!("Session not found: {id}"),
            }
            Ok(())
        }
        SessionCommands::Notify {
            id,
            text,
            title,
            sender,
            interrupt,
            mode,
            quiet_for_secs,
            max_delay_secs,
            confirm,
            status,
            format,
        } => {
            let uuid =
                crate::cli::session_resolve::resolve_session_id_with_service(&session_svc, &id)
                    .await?;
            crate::cli::session_notify::run(
                config,
                &uuid.to_string(),
                text.as_deref(),
                title.as_deref(),
                sender.as_deref(),
                interrupt,
                mode.as_deref(),
                quiet_for_secs,
                max_delay_secs,
                confirm,
                status,
                format,
            )
            .await?;
            Ok(())
        }
    }
}

/// Resolve profile-specific service identifiers.
/// Default profile → `com.opencrabs.daemon` / `opencrabs`
/// Named profile → `com.opencrabs.daemon.hermes` / `opencrabs-hermes`
fn service_identifiers() -> (String, String, String) {
    let profile = crate::config::profile::active_profile();
    let suffix = match profile {
        Some(name) if name != "default" => format!(".{name}"),
        _ => String::new(),
    };
    let systemd_suffix = match profile {
        Some(name) if name != "default" => format!("-{name}"),
        _ => String::new(),
    };
    let plist_name = format!("com.opencrabs.daemon{suffix}");
    let systemd_name = format!("opencrabs{systemd_suffix}");
    let log_suffix = if suffix.is_empty() {
        String::new()
    } else {
        suffix.clone()
    };
    (plist_name, systemd_name, log_suffix)
}

/// Build the daemon arguments, including `-p <profile>` when a named profile is active.
fn daemon_args() -> Vec<String> {
    let mut args = Vec::new();
    if let Some(name) = crate::config::profile::active_profile()
        && name != "default"
    {
        args.push("-p".to_string());
        args.push(name.to_string());
    }
    args.push("daemon".to_string());
    args
}

/// Build a systemd unit file. A root-installed *system* unit runs the daemon
/// as the invoking user (`run_as`, taken from `SUDO_USER`) so it reads that
/// user's `~/.opencrabs` rather than root's, and targets `multi-user.target`
/// so it starts on boot. A *user* unit omits `User=`/`HOME=` and targets
/// `default.target`. Pure (no OS calls) so it is unit-testable on any platform.
// Only the Linux install path calls this; off-Linux it lives solely for tests.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn build_systemd_unit(
    profile_label: &str,
    exec_args: &str,
    run_as: Option<&str>,
    system: bool,
) -> String {
    let identity = match run_as {
        Some(u) => format!("User={u}\nGroup={u}\nEnvironment=HOME=/home/{u}\n"),
        None => String::new(),
    };
    let wanted_by = if system {
        "multi-user.target"
    } else {
        "default.target"
    };
    // OOMPolicy=continue: this daemon is the parent of every tool call and cron
    // child, and systemd's default policy (`stop`) terminates the whole unit when
    // any single child is OOM-killed — turning one tool call's memory spike into a
    // full daemon restart that takes every agent lane down with it. `continue`
    // logs the kill and leaves the daemon running.
    format!(
        "[Unit]\n\
         Description=OpenCrabs Daemon [{profile_label}]\n\
         After=network.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         {identity}ExecStart={exec_args}\n\
         Restart=always\n\
         RestartSec=5\n\
         OOMPolicy=continue\n\
         \n\
         [Install]\n\
         WantedBy={wanted_by}\n"
    )
}

/// Linux: true when running as root (euid 0). We then install a *system* unit
/// (`/etc/systemd/system`, plain `systemctl`) instead of a user unit, because a
/// user unit needs a running per-user systemd + D-Bus session that a headless
/// VPS SSH session lacks ("Failed to connect to bus").
#[cfg(target_os = "linux")]
pub(crate) fn running_as_root() -> bool {
    // SAFETY: geteuid() always succeeds and has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

/// Core probe logic with explicit paths for testability.
/// Returns `true` for system scope, `false` for user scope.
#[cfg(target_os = "linux")]
pub(crate) fn unit_scope_probe(
    systemd_name: &str,
    system_dir: &std::path::Path,
    user_dir: Option<&std::path::Path>,
) -> bool {
    if system_dir.join(format!("{systemd_name}.service")).exists() {
        return true;
    }
    if let Some(dir) = user_dir
        && dir.join(format!("{systemd_name}.service")).exists()
    {
        return false;
    }
    // Neither exists — fall back to the old heuristic (covers fresh install).
    running_as_root()
}

/// Probe which systemd scope a unit was installed in by checking the unit file
/// paths. Falls back to `running_as_root()` when neither file exists (e.g. the
/// unit has not been installed yet — `install` still decides scope on its own).
#[cfg(target_os = "linux")]
fn unit_scope(systemd_name: &str) -> bool {
    let system_dir = std::path::PathBuf::from("/etc/systemd/system");
    let user_dir = invoking_user_home().map(|h| h.join(".config/systemd/user"));
    unit_scope_probe(systemd_name, &system_dir, user_dir.as_deref())
}

/// Home of the user who actually owns the units. Under `sudo`,
/// `dirs::home_dir()` resolves to root's home (`/root`), not the invoking
/// user's — so a user-scope unit in `~/.config/systemd/user` would be missed
/// and we'd wrongly try system scope. Prefer `SUDO_USER`'s home, matching the
/// `/home/<user>` convention `install` already writes into the unit's
/// `Environment=HOME=`.
#[cfg(target_os = "linux")]
fn invoking_user_home() -> Option<std::path::PathBuf> {
    user_home_from(std::env::var("SUDO_USER").ok().as_deref(), dirs::home_dir())
}

/// Pure resolution for [`invoking_user_home`]: prefer `SUDO_USER`'s standard
/// home (`/home/<user>`), otherwise the `fallback`. Split out so it is testable
/// without touching the environment or filesystem.
#[cfg(target_os = "linux")]
pub(crate) fn user_home_from(
    sudo_user: Option<&str>,
    fallback: Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    match sudo_user.filter(|u| !u.is_empty() && *u != "root") {
        Some(user) => Some(std::path::PathBuf::from(format!("/home/{user}"))),
        None => fallback,
    }
}

/// Run `systemctl` (system scope, or `--user`) and FAIL on a non-zero exit.
/// `.status()` alone only catches a spawn failure, so "Failed to connect to
/// bus" (exit 1) used to slip through and the caller printed a misleading ✅.
#[cfg(target_os = "linux")]
fn run_systemctl(system: bool, args: &[&str]) -> Result<()> {
    let mut cmd = std::process::Command::new("systemctl");
    if !system {
        cmd.arg("--user");
    }
    let out = cmd
        .args(args)
        .output()
        .context("failed to spawn systemctl")?;
    if out.status.success() {
        return Ok(());
    }
    let scope = if system {
        "systemctl"
    } else {
        "systemctl --user"
    };
    anyhow::bail!(
        "{scope} {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
}

/// Run `launchctl <flags> <plist>` and FAIL on a non-zero exit, except for its
/// benign no-ops (loading an already-loaded agent, unloading one that isn't
/// loaded). Like the Linux path, `.status()` alone hid real failures behind a
/// green ✅.
#[cfg(target_os = "macos")]
fn run_launchctl(flags: &[&str], plist: &std::path::Path) -> Result<()> {
    let out = std::process::Command::new("launchctl")
        .args(flags)
        .arg(plist)
        .output()
        .context("failed to spawn launchctl")?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lc = stderr.to_lowercase();
    if lc.contains("already loaded") || lc.contains("not find") || lc.contains("no such") {
        return Ok(());
    }
    anyhow::bail!(
        "launchctl {} {} failed: {}",
        flags.join(" "),
        plist.display(),
        stderr.trim()
    );
}

/// Guidance shown when a `--user` systemctl op fails for lack of a user bus.
#[cfg(target_os = "linux")]
fn user_service_hint() -> String {
    "A user service needs a running per-user systemd instance, which a headless \
     SSH session usually lacks. Either enable lingering so it runs without an \
     active login:\n    sudo loginctl enable-linger $(whoami)\nthen retry, or \
     install a system-wide service instead:\n    sudo opencrabs service install"
        .to_string()
}

/// OS service management
#[allow(unused_variables)]
pub(crate) async fn cmd_service(operation: ServiceCommands) -> Result<()> {
    let binary = std::env::current_exe().context("Could not determine binary path")?;
    let binary_str = binary.display().to_string();
    let (plist_name, systemd_name, log_suffix) = service_identifiers();
    let args = daemon_args();
    let profile_label = crate::config::profile::active_profile().unwrap_or("default");

    match operation {
        ServiceCommands::Install => {
            #[cfg(target_os = "macos")]
            {
                let plist_path = dirs::home_dir()
                    .context("No home dir")?
                    .join("Library/LaunchAgents")
                    .join(format!("{plist_name}.plist"));

                let args_xml: String =
                    std::iter::once(format!("        <string>{binary_str}</string>"))
                        .chain(args.iter().map(|a| format!("        <string>{a}</string>")))
                        .collect::<Vec<_>>()
                        .join("\n");

                // Daemon stdout/stderr go to the same private log directory as
                // every other log, not /tmp (OC-05). /tmp is world-readable, and
                // daemon stderr carries URLs, tracing events, and chat-adjacent
                // text. The directory is created 0700 so a second local user
                // cannot list or read it.
                let log_dir = crate::logging::log_dir();
                if let Err(e) = std::fs::create_dir_all(&log_dir) {
                    tracing::warn!(
                        "service install: could not create log dir {}: {e}",
                        log_dir.display()
                    );
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ =
                        std::fs::set_permissions(&log_dir, std::fs::Permissions::from_mode(0o700));
                }
                let out_log = log_dir.join(format!("daemon{log_suffix}.out.log"));
                let err_log = log_dir.join(format!("daemon{log_suffix}.err.log"));
                let out_log_str = out_log.to_string_lossy();
                let err_log_str = err_log.to_string_lossy();

                let plist = format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{plist_name}</string>
    <key>ProgramArguments</key>
    <array>
{args_xml}
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{out_log_str}</string>
    <key>StandardErrorPath</key>
    <string>{err_log_str}</string>
</dict>
</plist>"#
                );

                if let Some(parent) = plist_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&plist_path, plist)?;
                println!(
                    "✅ Installed LaunchAgent [{profile_label}]: {}",
                    plist_path.display()
                );
                println!("   Run: opencrabs service start");
            }

            #[cfg(target_os = "linux")]
            {
                let system = running_as_root();
                let unit_path = if system {
                    std::path::PathBuf::from(format!("/etc/systemd/system/{systemd_name}.service"))
                } else {
                    dirs::home_dir()
                        .context("No home dir")?
                        .join(format!(".config/systemd/user/{systemd_name}.service"))
                };

                let exec_args = std::iter::once(binary_str.clone())
                    .chain(args.iter().cloned())
                    .collect::<Vec<_>>()
                    .join(" ");

                // System unit installed via sudo: run the daemon as the
                // invoking user so it reads that user's ~/.opencrabs, not root's.
                let run_as = if system {
                    std::env::var("SUDO_USER")
                        .ok()
                        .filter(|u| !u.is_empty() && u != "root")
                } else {
                    None
                };
                let unit = build_systemd_unit(profile_label, &exec_args, run_as.as_deref(), system);

                if let Some(parent) = unit_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&unit_path, unit)
                    .with_context(|| format!("writing unit {}", unit_path.display()))?;

                if let Err(e) = run_systemctl(system, &["daemon-reload"]) {
                    eprintln!("❌ {e}");
                    if !system {
                        eprintln!("\n{}", user_service_hint());
                    }
                    return Err(e);
                }
                println!(
                    "✅ Installed systemd {} unit [{profile_label}]: {}",
                    if system { "system" } else { "user" },
                    unit_path.display()
                );
                println!(
                    "   Run: {}opencrabs service start",
                    if system { "sudo " } else { "" }
                );
            }

            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            return Err(anyhow::anyhow!(
                "Service install not supported on this platform"
            ));

            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Ok(())
        }
        ServiceCommands::Start => {
            #[cfg(target_os = "macos")]
            {
                let plist = dirs::home_dir()
                    .context("No home dir")?
                    .join(format!("Library/LaunchAgents/{plist_name}.plist"));
                if let Err(e) = run_launchctl(&["load", "-w"], &plist) {
                    eprintln!("❌ {e}");
                    return Err(e);
                }
                println!("✅ Started OpenCrabs daemon [{profile_label}]");
            }

            #[cfg(target_os = "linux")]
            {
                let system = unit_scope(&systemd_name);
                // `enable --now` so a system unit also survives reboot; a user
                // unit just starts (boot-persistence there needs lingering).
                let res = if system {
                    run_systemctl(true, &["enable", "--now", systemd_name.as_str()])
                } else {
                    run_systemctl(false, &["start", systemd_name.as_str()])
                };
                if let Err(e) = res {
                    eprintln!("❌ {e}");
                    if !system {
                        eprintln!("\n{}", user_service_hint());
                    }
                    return Err(e);
                }
                println!("✅ Started OpenCrabs daemon [{profile_label}]");
            }

            Ok(())
        }
        ServiceCommands::Stop => {
            #[cfg(target_os = "macos")]
            {
                let plist = dirs::home_dir()
                    .context("No home dir")?
                    .join(format!("Library/LaunchAgents/{plist_name}.plist"));
                if let Err(e) = run_launchctl(&["unload"], &plist) {
                    eprintln!("❌ {e}");
                    return Err(e);
                }
                println!("✅ Stopped OpenCrabs daemon [{profile_label}]");
            }

            #[cfg(target_os = "linux")]
            {
                let system = unit_scope(&systemd_name);
                if let Err(e) = run_systemctl(system, &["stop", systemd_name.as_str()]) {
                    eprintln!("❌ {e}");
                    if !system {
                        eprintln!("\n{}", user_service_hint());
                    }
                    return Err(e);
                }
                println!("✅ Stopped OpenCrabs daemon [{profile_label}]");
            }

            Ok(())
        }
        ServiceCommands::Restart => {
            #[cfg(target_os = "macos")]
            {
                let plist = dirs::home_dir()
                    .context("No home dir")?
                    .join(format!("Library/LaunchAgents/{plist_name}.plist"));
                let _ = run_launchctl(&["unload"], &plist);
                if let Err(e) = run_launchctl(&["load", "-w"], &plist) {
                    eprintln!("❌ {e}");
                    return Err(e);
                }
                println!("✅ Restarted OpenCrabs daemon [{profile_label}]");
            }
            #[cfg(target_os = "linux")]
            {
                let system = unit_scope(&systemd_name);
                if let Err(e) = run_systemctl(system, &["restart", systemd_name.as_str()]) {
                    eprintln!("❌ {e}");
                    if !system {
                        eprintln!("\n{}", user_service_hint());
                    }
                    return Err(e);
                }
                println!("✅ Restarted OpenCrabs daemon [{profile_label}]");
            }
            Ok(())
        }
        ServiceCommands::Status => {
            #[cfg(target_os = "macos")]
            {
                let output = std::process::Command::new("launchctl")
                    .args(["list", &plist_name])
                    .output()?;
                if output.status.success() {
                    println!("✅ OpenCrabs daemon [{profile_label}] is running");
                    println!("{}", String::from_utf8_lossy(&output.stdout));
                } else {
                    println!("⬚  OpenCrabs daemon [{profile_label}] is not running");
                }
            }

            #[cfg(target_os = "linux")]
            {
                let system = unit_scope(&systemd_name);
                let mut cmd = std::process::Command::new("systemctl");
                if !system {
                    cmd.arg("--user");
                }
                let output = cmd.args(["status", systemd_name.as_str()]).output()?;
                // `systemctl status` exits non-zero when the unit is inactive;
                // that is informational, not an error, so just show the output.
                print!("{}", String::from_utf8_lossy(&output.stdout));
                if output.stdout.is_empty() {
                    eprint!("{}", String::from_utf8_lossy(&output.stderr));
                }
            }

            Ok(())
        }
        ServiceCommands::Uninstall => {
            #[cfg(target_os = "macos")]
            {
                let plist = dirs::home_dir()
                    .context("No home dir")?
                    .join(format!("Library/LaunchAgents/{plist_name}.plist"));
                let _ = std::process::Command::new("launchctl")
                    .args(["unload"])
                    .arg(&plist)
                    .status();
                if plist.exists() {
                    std::fs::remove_file(&plist)?;
                    println!(
                        "✅ Removed LaunchAgent [{profile_label}]: {}",
                        plist.display()
                    );
                } else {
                    println!("⬚  LaunchAgent [{profile_label}] not found");
                }
            }
            #[cfg(target_os = "linux")]
            {
                let system = unit_scope(&systemd_name);
                // Best-effort stop/disable: failing here (already stopped, no
                // bus) must not block removing the unit file.
                let _ = run_systemctl(system, &["stop", systemd_name.as_str()]);
                let _ = run_systemctl(system, &["disable", systemd_name.as_str()]);
                let unit_path = if system {
                    std::path::PathBuf::from(format!("/etc/systemd/system/{systemd_name}.service"))
                } else {
                    dirs::home_dir()
                        .context("No home dir")?
                        .join(format!(".config/systemd/user/{systemd_name}.service"))
                };
                if unit_path.exists() {
                    std::fs::remove_file(&unit_path)?;
                    let _ = run_systemctl(system, &["daemon-reload"]);
                    println!(
                        "✅ Removed systemd unit [{profile_label}]: {}",
                        unit_path.display()
                    );
                } else {
                    println!("⬚  Systemd unit [{profile_label}] not found");
                }
            }

            Ok(())
        }
    }
}

/// Profile management
pub(crate) async fn cmd_profile(operation: ProfileCommands) -> Result<()> {
    use crate::config::profile;

    match operation {
        ProfileCommands::Create { name, description } => {
            let path = profile::create_profile(&name, description.as_deref())?;
            println!("✅ Created profile '{name}'");
            println!("   Path: {}", path.display());
            println!("\n   Usage: opencrabs -p {name}");
            Ok(())
        }
        ProfileCommands::List => {
            let profiles = profile::list_profiles()?;
            let active = profile::active_profile().unwrap_or("default");

            println!("Profiles:\n");
            for p in &profiles {
                let marker = if p.name == active { " ←" } else { "" };
                let desc = p
                    .description
                    .as_deref()
                    .map(|d| format!(" — {d}"))
                    .unwrap_or_default();
                println!("  {}{}{}", p.name, desc, marker);
            }
            println!("\n  {} profile(s) total", profiles.len());
            Ok(())
        }
        ProfileCommands::Delete { name, force } => {
            if !force {
                println!("⚠️  This will permanently delete profile '{name}' and ALL its data.");
                println!("   Re-run with --force to confirm.");
                return Ok(());
            }
            profile::delete_profile(&name)?;
            println!("✅ Deleted profile '{name}'");
            Ok(())
        }
        ProfileCommands::Export { name, output } => {
            let output_path = output
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from(format!("{name}.tar.gz")));
            profile::export_profile(&name, &output_path)?;
            println!("✅ Exported profile '{name}' to {}", output_path.display());
            Ok(())
        }
        ProfileCommands::Import { path } => {
            let name = profile::import_profile(std::path::Path::new(&path))?;
            println!("✅ Imported profile '{name}'");
            println!("\n   Usage: opencrabs -p {name}");
            Ok(())
        }
        ProfileCommands::Migrate { from, to, force } => {
            let migrated = profile::migrate_profile(&from, &to, force)?;
            if migrated.is_empty() {
                println!("⚠️  No files migrated from '{from}' to '{to}'.");
                println!("   All files already exist in '{to}'. Use --force to overwrite.");
            } else {
                println!(
                    "✅ Migrated {} files from '{from}' to '{to}':\n",
                    migrated.len()
                );
                for file in &migrated {
                    println!("   {file}");
                }
                println!("\n   Switch to the new profile: opencrabs -p {to}");
                println!("   Then customize identity, brain files, keys, etc.");
            }
            Ok(())
        }
    }
}

/// Check for and install the latest OpenCrabs release
pub(crate) async fn cmd_evolve(config: &crate::config::Config, check_only: bool) -> Result<()> {
    let current_version = crate::VERSION;
    println!("🔄 OpenCrabs v{current_version} — checking for updates...\n");

    if check_only {
        match crate::brain::tools::evolve::check_for_update().await {
            Some(latest) => {
                println!("✅ Update available: v{current_version} → v{latest}");
                println!("   Run `opencrabs evolve` to install.");
            }
            None => {
                println!("✅ Already on the latest version (v{current_version}).");
            }
        }
        return Ok(());
    }

    // Read the current database user_version for migration compatibility check
    let current_user_version: Option<i64> = {
        let db_path = &config.database.path;
        if db_path.exists() {
            match crate::db::Database::connect(db_path).await {
                Ok(db) => match db.get_user_version().await {
                    Ok(ver) => Some(ver),
                    Err(e) => {
                        tracing::warn!("evolve: could not read user_version: {e}");
                        None
                    }
                },
                Err(e) => {
                    tracing::warn!("evolve: could not open database: {e}");
                    None
                }
            }
        } else {
            None
        }
    };

    // Full evolve — instantiate the tool and execute
    use crate::brain::tools::evolve::EvolveTool;
    use crate::brain::tools::{Tool, ToolExecutionContext};
    use serde_json::json;

    let tool = EvolveTool::new(None);
    let input = json!({
        "check_only": false,
        "current_user_version": current_user_version
    });
    let mut context = ToolExecutionContext::new(uuid::Uuid::new_v4())
        .with_working_directory(std::env::current_dir().unwrap_or_default())
        .with_auto_approve(true);
    context.timeout_secs = 300;
    // #1802: every surface that builds a context stamps the kill switch.
    context.session_notify_enabled = crate::config::Config::current()
        .agent
        .session_notify_enabled;

    let result = tool.execute(input, &context).await?;
    println!("{}", result.output);

    Ok(())
}
