//! Discord Agent
//!
//! Agent struct and startup logic. Mirrors the Telegram/WhatsApp agent pattern.

use super::DiscordState;
use super::handler;
use crate::brain::agent::AgentService;
use crate::config::Config;
use crate::db::ChannelMessageRepository;
use crate::services::{ServiceContext, SessionService};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use serenity::async_trait;
use serenity::model::application::Interaction;
use serenity::model::channel::Message;
use serenity::model::gateway::Ready;
use serenity::prelude::*;

/// Discord bot that forwards messages to the AgentService
pub struct DiscordAgent {
    agent_service: Arc<AgentService>,
    session_service: SessionService,
    shared_session_id: Arc<Mutex<Option<Uuid>>>,
    discord_state: Arc<DiscordState>,
    config_rx: tokio::sync::watch::Receiver<Config>,
    channel_msg_repo: ChannelMessageRepository,
}

impl DiscordAgent {
    pub fn new(
        agent_service: Arc<AgentService>,
        service_context: ServiceContext,
        shared_session_id: Arc<Mutex<Option<Uuid>>>,
        discord_state: Arc<DiscordState>,
        config_rx: tokio::sync::watch::Receiver<Config>,
        channel_msg_repo: ChannelMessageRepository,
    ) -> Self {
        Self {
            agent_service,
            session_service: SessionService::new(service_context),
            shared_session_id,
            discord_state,
            config_rx,
            channel_msg_repo,
        }
    }

    /// Start the bot as a background task. Returns a JoinHandle.
    pub fn start(self, token: String) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            // Validate token format - Discord tokens are typically ~70 chars
            if token.is_empty() || token.len() < 50 {
                tracing::debug!("Discord bot token not configured or invalid, skipping bot start");
                return;
            }

            let cfg = self.config_rx.borrow().clone();
            tracing::info!(
                "Starting Discord bot with {} allowed user(s), STT={}, TTS={}",
                cfg.channels.discord.allowed_users.len(),
                cfg.voice_config().stt_enabled,
                cfg.voice_config().tts_enabled,
            );

            let extra_sessions: Arc<Mutex<HashMap<u64, (Uuid, std::time::Instant)>>> =
                Arc::new(Mutex::new(HashMap::new()));

            let agent = self.agent_service;
            let session_svc = self.session_service;
            let shared_session = self.shared_session_id;
            let discord_state = self.discord_state;
            let config_rx = self.config_rx;
            let channel_msg_repo = self.channel_msg_repo;

            let intents = GatewayIntents::GUILD_MESSAGES
                | GatewayIntents::DIRECT_MESSAGES
                | GatewayIntents::MESSAGE_CONTENT;

            let make_handler = || Handler {
                agent: agent.clone(),
                session_svc: session_svc.clone(),
                extra_sessions: extra_sessions.clone(),
                shared_session: shared_session.clone(),
                discord_state: discord_state.clone(),
                config_rx: config_rx.clone(),
                channel_msg_repo: channel_msg_repo.clone(),
            };

            let mut client = match Client::builder(&token, intents)
                .event_handler(make_handler())
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("Discord: failed to create client: {}", e);
                    return;
                }
            };

            // Retry loop: if the gateway connection drops (network hiccup, Discord
            // server restart, etc.), wait and reconnect instead of dying silently.
            loop {
                tracing::info!("Discord: starting gateway connection");
                if let Err(e) = client.start().await {
                    tracing::error!("Discord: client error: {} — reconnecting in 5s", e);
                } else {
                    tracing::warn!("Discord: client exited unexpectedly — reconnecting in 5s");
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                client = match Client::builder(&token, intents)
                    .event_handler(make_handler())
                    .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("Discord: failed to rebuild client: {}", e);
                        return;
                    }
                };
            }
        })
    }
}

/// Serenity event handler — routes messages to the agent
struct Handler {
    agent: Arc<AgentService>,
    session_svc: SessionService,
    extra_sessions: Arc<Mutex<HashMap<u64, (Uuid, std::time::Instant)>>>,
    shared_session: Arc<Mutex<Option<Uuid>>>,
    discord_state: Arc<DiscordState>,
    config_rx: tokio::sync::watch::Receiver<Config>,
    channel_msg_repo: ChannelMessageRepository,
}

#[async_trait]
impl EventHandler for Handler {
    async fn reaction_add(&self, ctx: Context, reaction: serenity::model::channel::Reaction) {
        let agent = self.agent.clone();
        let session_svc = self.session_svc.clone();
        let discord_state = self.discord_state.clone();
        let config_rx = self.config_rx.clone();
        tokio::spawn(async move {
            super::reactions::handle_reaction_add(
                &ctx,
                &reaction,
                agent,
                session_svc,
                discord_state,
                config_rx,
            )
            .await;
        });
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        tracing::info!(
            "Discord: connected as {} (id={})",
            ready.user.name,
            ready.user.id
        );
        self.discord_state
            .set_connected(ctx.http.clone(), None)
            .await;
        self.discord_state
            .set_bot_user_id(ready.user.id.get())
            .await;

        // Application commands (#1850): project `commands.toml` onto Discord's
        // slash-command list so the catalog the TUI completes and Telegram
        // menus is the same one this channel autocompletes. `ready` is the only
        // hook holding both an HTTP handle and the guild list, and the retry
        // loop above rebuilds the client (not the handler) after a gateway
        // drop, so a reconnect re-plans and the key comparison decides whether
        // anything is actually sent. A guild that was joined while the process
        // was down appears in `ready.guilds` on the next connect and moves the
        // key, so it gets the menu without a config write. A guild joined while
        // we are connected is covered the same way, on the next reconnect: the
        // watcher below only knows the guild list this `ready` reported.
        let guilds: Vec<serenity::model::id::GuildId> =
            ready.guilds.iter().map(|guild| guild.id).collect();
        let http = ctx.http.clone();
        let state = self.discord_state.clone();
        let mut config_rx = self.config_rx.clone();

        // First `ready` of the process: sync unconditionally, because the stored
        // key is `None`. A reconnect hands back that stored key instead, so a
        // gateway that flaps on a short retry loop does not re-PUT
        // the whole command tree to every guild on each pass; it costs one
        // `commands.toml` read and nothing else, unless the catalog or the guild
        // set actually moved.
        let stored = *state.commands_sig.lock().await;
        let key = super::commands::sync_commands(&http, &guilds, stored).await;
        *state.commands_sig.lock().await = key;
        let start_watcher = !*state.commands_watcher_started.lock().await;
        if start_watcher {
            *state.commands_watcher_started.lock().await = true;
            tokio::spawn(async move {
                // The ConfigWatcher re-publishes on any config write, which
                // covers `commands.toml` (see the same contract documented in
                // `telegram::menu_refresh`). Each publish re-plans and compares
                // the key, so an unrelated config edit costs a file read and no
                // API call. Guild membership is folded into that key, so a
                // publish after the bot joined a server re-sends to all of them
                // rather than only the ones listed here.
                loop {
                    if config_rx.changed().await.is_err() {
                        break;
                    }
                    let last = *state.commands_sig.lock().await;
                    let next = super::commands::sync_commands(&http, &guilds, last).await;
                    *state.commands_sig.lock().await = next;
                }
            });
        }
    }

    async fn message(&self, ctx: Context, msg: Message) {
        // Skip bot messages
        if msg.author.bot {
            return;
        }

        handler::handle_message(
            &ctx,
            &msg,
            self.agent.clone(),
            self.session_svc.clone(),
            self.shared_session.clone(),
            self.discord_state.clone(),
            self.config_rx.clone(),
            self.channel_msg_repo.clone(),
        )
        .await;
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        // Slash commands (#1850): rebuild the invocation as the text the user
        // would have typed and route it through the same display path a tapped
        // suggestion uses. Parity is structural: the agent receives `/check
        // args`, exactly what a hand-typed message produces, so there is one
        // command implementation and the arguments survive inside the text.
        //
        // Deliberately NOT the bare `route_interaction_turn`. Its own contract
        // is a single completion with no tool loop (see `interactions.rs`), so
        // `/check` picked from the menu would come back saying it cannot run
        // cargo. The two synthetic branches that stay on it are synthetic: a
        // modal fill and a select pick are steering prompts, an invoked command
        // is a request to do work.
        if let Interaction::Command(command) = &interaction {
            let mut invocation = format!("/{}", command.data.name);
            for option in &command.data.options {
                if option.name == super::commands::ARGS_OPTION
                    && let serenity::model::application::CommandDataOptionValue::String(value) =
                        &option.value
                    && !value.is_empty()
                {
                    invocation.push(' ');
                    invocation.push_str(value.as_str());
                } else if option.name != super::commands::ARGS_OPTION {
                    // One option is all this registers (see `commands.rs`), so
                    // anything else means the client sent a set we did not ask
                    // for. Worth a log line rather than silently swallowing the
                    // argument text.
                    tracing::warn!(
                        "Discord: unexpected option {:?} on command {:?}",
                        option.name,
                        command.data.name
                    );
                }
            }
            let user = command.user.id.get();
            let user_name = command.user.name.clone();
            let is_dm = command.guild_id.is_none();
            let channel_id = command.channel_id.get();

            // OC-02: Discord shows the command list to every member of the
            // guild, so an interaction is an entry point like any other message
            // and gets the same deny-by-default gate `handle_message` applies.
            // Roles count in guilds only, mirroring that path.
            let cfg = self.config_rx.borrow().clone();
            let dc = &cfg.channels.discord;
            let role_ids: Vec<u64> = if is_dm {
                Vec::new()
            } else {
                command
                    .member
                    .as_ref()
                    .map(|m| m.roles.iter().map(|r| r.get()).collect())
                    .unwrap_or_default()
            };
            let owner =
                crate::config::owner::is_owner(&dc.allowed_users, &dc.bot_owner, &user.to_string());
            let in_allowlist = dc
                .allowed_users
                .iter()
                .filter_map(|s| s.parse::<i64>().ok())
                .any(|u| u == user as i64);
            // A thread or forum post resolves its settings through its parent
            // (#384, #2014). The lookup runs only when the operator configured
            // something a parent can carry, so a default install costs nothing.
            let channel_str = channel_id.to_string();
            let needs_parent =
                !is_dm && (!dc.allowed_channels.is_empty() || !dc.channels.is_empty());
            let parent = if needs_parent {
                super::commands::parent_channel_id(&ctx.http, command.channel_id).await
            } else {
                None
            };
            let open_here = !is_dm && dc.channel_open(&channel_str, parent.as_deref());
            let admitted = super::commands::identity_admitted(
                dc.allowed_users.is_empty()
                    && dc.allowed_roles.is_empty()
                    && dc.bot_owner.is_empty(),
                owner,
                in_allowlist,
                !is_dm && super::commands::holds_allowed_role(&dc.allowed_roles, &role_ids),
                open_here,
            );

            // Channel scope, with the parent fallback: a thread or forum post
            // carries its own id, so allow-listing a forum admits its posts.
            let mut channel_ok = dc.allowed_channels.is_empty()
                || dc.allowed_channels.iter().any(|c| c == &channel_str);
            if !channel_ok && !is_dm {
                channel_ok = parent
                    .as_deref()
                    .is_some_and(|p| dc.allowed_channels.iter().any(|c| c == p));
            }
            // `respond_to` filters unsolicited messages; an invoked command is
            // solicited by definition, so only `dm_only` applies, and it means
            // the operator told this bot not to speak in guild channels. The
            // mode is the channel's own, with the parent and global fallbacks.
            let inside_dm_policy = is_dm
                || !matches!(
                    dc.respond_to_for(&channel_str, parent.as_deref()),
                    crate::config::RespondTo::DmOnly
                );

            // The owner's /cowork and /respond_to pass the channel scope and the
            // dm_only policy, so they can open or fix the channel they run in.
            // DMs never had a channel scope; the interaction path now says so too.
            let owner_scope = !is_dm
                && super::commands::owner_scope_text(&format!("/{}", command.data.name), owner);
            let channel_ok = is_dm
                || owner_scope
                || channel_ok
                || dc.has_channel_block(&channel_str, parent.as_deref());
            let policy_ok = owner_scope || inside_dm_policy;
            if !admitted || !channel_ok || !policy_ok {
                tracing::warn!(
                    "Discord: refused /{} from user {} (admitted={}, channel_scope={}, respond_to_policy={})",
                    command.data.name,
                    user,
                    admitted,
                    channel_ok,
                    policy_ok
                );
                let _ = command
                    .create_response(
                        &ctx.http,
                        serenity::builder::CreateInteractionResponse::Message(
                            serenity::builder::CreateInteractionResponseMessage::new()
                                .content(super::commands::refusal_reason(
                                    admitted,
                                    channel_ok,
                                    policy_ok,
                                    &channel_str,
                                ))
                                .ephemeral(true),
                        ),
                    )
                    .await;
                return;
            }

            // `/respond_to` from the menu is answered here, not routed to the
            // model as a prompt. It never writes Telegram's section (#2013).
            let cowork_name = if !is_dm && super::cowork::is_cowork_command(&invocation) {
                super::cowork::channel_name(&ctx.http, command.channel_id).await
            } else {
                None
            };
            let scope_reply = super::cowork::cowork_discord_channel(
                &invocation,
                owner,
                !is_dm,
                &channel_str,
                cowork_name.as_deref(),
                super::cowork::write_channel_open,
            )
            .or_else(|| {
                crate::channels::respond_to_scope::respond_to_discord_channel(
                    &invocation,
                    owner,
                    &channel_str,
                    &dc.respond_to_for(&channel_str, parent.as_deref()),
                    super::commands::write_channel_respond_to,
                )
            });
            if let Some(reply) = scope_reply {
                if let Err(e) = command
                    .create_response(
                        &ctx.http,
                        serenity::builder::CreateInteractionResponse::Message(
                            serenity::builder::CreateInteractionResponseMessage::new()
                                .content(reply)
                                .ephemeral(true),
                        ),
                    )
                    .await
                {
                    tracing::warn!("Discord: /{} reply refused: {e}", command.data.name);
                }
                return;
            }

            let idle = dc.session_idle_hours;
            // History keeps the invocation the way a typed message would:
            // `Sender: /cmd args` in a guild, bare in the owner's DM, the same
            // rule `handler.rs` uses. `context_text` is the invocation itself,
            // which is what the model sees when you type it.
            let history_line = if owner && is_dm {
                invocation.clone()
            } else {
                format!("{user_name}: {invocation}")
            };
            // #1888: Acknowledge serializes to callback type 6
            // (DEFERRED_UPDATE_MESSAGE), a component-only ack: an
            // application command acked with it is never marked resolved,
            // so Discord paints "didn't respond in time" over a call whose
            // answer actually landed in the channel. Type 5
            // (DEFERRED_CHANNEL_MESSAGE_WITH_SOURCE) is the deferred ack
            // for commands and enters the loading state the moment
            // Discord accepts it. The answer keeps flowing through the
            // normal channel delivery that route_followup_turn drives, so
            // the placeholder is deleted once the ack is accepted instead
            // of edited into the reply, and a refused ack is logged rather
            // than swallowed by a discard-binding like the old code did.
            let ack = command
                .create_response(
                    &ctx.http,
                    serenity::builder::CreateInteractionResponse::Defer(
                        serenity::builder::CreateInteractionResponseMessage::new().ephemeral(true),
                    ),
                )
                .await;
            if let Err(e) = &ack {
                tracing::warn!(
                    "Discord: deferred ack for /{} refused: {e}",
                    command.data.name
                );
            } else if let Err(e) = ctx
                .http
                .delete_original_interaction_response(&command.token)
                .await
            {
                // Cosmetic only: the placeholder is ephemeral, so at worst
                // the invoker's own spinner ages out while the real reply
                // still arrives in the channel.
                tracing::debug!(
                    "Discord: could not clear /{} loading placeholder: {e}",
                    command.data.name
                );
            }
            let agent = self.agent.clone();
            let session_svc = self.session_svc.clone();
            let discord_state = self.discord_state.clone();
            let ctx2 = ctx.clone();
            tokio::spawn(async move {
                super::interactions::route_followup_turn(
                    &ctx2,
                    agent,
                    session_svc,
                    discord_state,
                    is_dm,
                    user,
                    channel_id,
                    idle,
                    invocation,
                    history_line,
                )
                .await;
            });
            return;
        }

        // Modal submissions (#383): route the filled fields back as a turn.
        if let Interaction::Modal(modal) = &interaction {
            let custom_id = modal.data.custom_id.clone();
            if let Some(form_id) = custom_id.strip_prefix("formsub:") {
                let Some(spec) = self.discord_state.take_form(form_id).await else {
                    let _ack = modal
                        .create_response(
                            &ctx.http,
                            serenity::builder::CreateInteractionResponse::Acknowledge,
                        )
                        .await;
                    return;
                };
                // Collect input values in field order.
                use serenity::model::application::ActionRowComponent;
                let mut values: Vec<String> = Vec::new();
                for row in &modal.data.components {
                    for comp in &row.components {
                        if let ActionRowComponent::InputText(input) = comp {
                            values.push(input.value.clone().unwrap_or_default());
                        }
                    }
                }
                let filled: Vec<String> = spec
                    .fields
                    .iter()
                    .zip(values.iter())
                    .map(|((label, _), v)| format!("{label}: {v}"))
                    .collect();
                let user = modal.user.id.get();
                let user_name = modal
                    .user
                    .global_name
                    .clone()
                    .unwrap_or_else(|| modal.user.name.clone());
                let is_dm = modal.guild_id.is_none();
                let channel_id = modal.channel_id.get();
                let _ack = modal
                    .create_response(
                        &ctx.http,
                        serenity::builder::CreateInteractionResponse::Acknowledge,
                    )
                    .await;
                let agent = self.agent.clone();
                let session_svc = self.session_svc.clone();
                let idle = self.config_rx.borrow().channels.discord.session_idle_hours;
                let title = spec.title.clone();
                let ctx2 = ctx.clone();
                let dstate = self.discord_state.clone();
                tokio::spawn(async move {
                    super::interactions::route_interaction_turn(
                        &ctx2,
                        &dstate.governor,
                        agent,
                        session_svc,
                        is_dm,
                        user,
                        channel_id,
                        idle,
                        format!(
                            "[{user_name} submitted the \"{title}\" form]\n{}",
                            filled.join("\n")
                        ),
                        format!("[System: {user_name} submitted the \"{title}\" form]"),
                        dstate.clone(),
                    )
                    .await;
                });
                return;
            }
        }

        if let Some(comp) = interaction.message_component() {
            let custom_id = comp.data.custom_id.as_str();
            tracing::info!("Discord callback received: custom_id={}", custom_id);

            // Optional follow-up suggestion tapped (#598): inject the chosen
            // suggestion as the user's next message (a fresh turn). Options were
            // stashed under `followup:<id>:<idx>` via the TTL-bounded select map.
            if let Some(rest) = custom_id.strip_prefix(super::suggest_options::FOLLOWUP_PREFIX) {
                let ttl = self.config_rx.borrow().channels.discord.component_ttl_hours;
                let picked: Option<String> = if let Some((id, idx_str)) = rest.rsplit_once(':') {
                    match (
                        self.discord_state.take_select(id, ttl).await,
                        idx_str.parse::<usize>(),
                    ) {
                        (Some(opts), Ok(idx)) => opts.get(idx).cloned(),
                        _ => None,
                    }
                } else {
                    None
                };
                use serenity::builder::{
                    CreateInteractionResponse, CreateInteractionResponseMessage,
                };
                let Some(choice) = picked else {
                    let _e = comp
                        .create_response(
                            &ctx.http,
                            CreateInteractionResponse::UpdateMessage(
                                CreateInteractionResponseMessage::new()
                                    .content("⌛ These suggestions expired.")
                                    .components(Vec::new()),
                            ),
                        )
                        .await;
                    return;
                };
                let user = comp.user.id.get();
                let is_dm = comp.guild_id.is_none();
                let channel_id = comp.channel_id.get();
                let display_tag = comp
                    .user
                    .global_name
                    .clone()
                    .unwrap_or_else(|| comp.user.name.clone());
                // Ack by disabling the buttons and echoing the pick.
                let _e = comp
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::UpdateMessage(
                            CreateInteractionResponseMessage::new()
                                .content(format!("\u{25b6}\u{fe0f} {choice}"))
                                .components(Vec::new()),
                        ),
                    )
                    .await;
                let agent = self.agent.clone();
                let session_svc = self.session_svc.clone();
                let discord_state = self.discord_state.clone();
                let idle = self.config_rx.borrow().channels.discord.session_idle_hours;
                let ctx2 = ctx.clone();
                tokio::spawn(async move {
                    // #1852: tapped suggestions ride the tool-loop display
                    // path (live status, tools, approvals, chained buttons)
                    // instead of the bare single-call interaction route.
                    super::interactions::route_followup_turn(
                        &ctx2,
                        agent,
                        session_svc,
                        discord_state,
                        is_dm,
                        user,
                        channel_id,
                        idle,
                        choice,
                        display_tag,
                    )
                    .await;
                });
                return;
            }

            // Select menu pick (#382), with lazy TTL (#386).
            if let Some(sel_id) = custom_id.strip_prefix("sel:") {
                let ttl = self.config_rx.borrow().channels.discord.component_ttl_hours;
                let options = self.discord_state.take_select(sel_id, ttl).await;
                use serenity::model::application::ComponentInteractionDataKind;
                let picked: Option<String> = match (&comp.data.kind, options) {
                    (ComponentInteractionDataKind::StringSelect { values }, Some(opts)) => values
                        .first()
                        .and_then(|v| v.parse::<usize>().ok())
                        .and_then(|i| opts.get(i).cloned()),
                    (_, None) => None,
                    _ => None,
                };
                let Some(choice) = picked else {
                    // Expired or unknown: say so and strip the dead menu.
                    use serenity::builder::{
                        CreateInteractionResponse, CreateInteractionResponseMessage,
                    };
                    let _e = comp
                        .create_response(
                            &ctx.http,
                            CreateInteractionResponse::UpdateMessage(
                                CreateInteractionResponseMessage::new()
                                    .content("⌛ This menu expired.")
                                    .components(Vec::new()),
                            ),
                        )
                        .await;
                    return;
                };
                let user = comp.user.id.get();
                let user_name = comp
                    .user
                    .global_name
                    .clone()
                    .unwrap_or_else(|| comp.user.name.clone());
                let is_dm = comp.guild_id.is_none();
                let channel_id = comp.channel_id.get();
                // Ack by disabling the menu and showing the pick.
                use serenity::builder::{
                    CreateInteractionResponse, CreateInteractionResponseMessage,
                };
                let _e = comp
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::UpdateMessage(
                            CreateInteractionResponseMessage::new()
                                .content(format!("✅ {user_name} picked: {choice}"))
                                .components(Vec::new()),
                        ),
                    )
                    .await;
                let agent = self.agent.clone();
                let session_svc = self.session_svc.clone();
                let idle = self.config_rx.borrow().channels.discord.session_idle_hours;
                let ctx2 = ctx.clone();
                let dstate = self.discord_state.clone();
                tokio::spawn(async move {
                    super::interactions::route_interaction_turn(
                        &ctx2,
                        &dstate.governor,
                        agent,
                        session_svc,
                        is_dm,
                        user,
                        channel_id,
                        idle,
                        format!(
                            "[{user_name} picked \"{choice}\" from your select menu — \
                             continue accordingly]"
                        ),
                        format!("[System: {user_name} picked \"{choice}\"]"),
                        dstate.clone(),
                    )
                    .await;
                });
                return;
            }

            // Form button (#383): open the modal, with lazy TTL (#386).
            if let Some(form_id) = custom_id.strip_prefix("form:") {
                let ttl = self.config_rx.borrow().channels.discord.component_ttl_hours;
                let Some(spec) = self.discord_state.get_form(form_id, ttl).await else {
                    use serenity::builder::{
                        CreateInteractionResponse, CreateInteractionResponseMessage,
                    };
                    let _e = comp
                        .create_response(
                            &ctx.http,
                            CreateInteractionResponse::UpdateMessage(
                                CreateInteractionResponseMessage::new()
                                    .content("⌛ This form expired.")
                                    .components(Vec::new()),
                            ),
                        )
                        .await;
                    return;
                };
                use serenity::builder::{
                    CreateActionRow, CreateInputText, CreateInteractionResponse, CreateModal,
                };
                use serenity::model::application::InputTextStyle;
                let rows: Vec<CreateActionRow> = spec
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, (label, multiline))| {
                        let style = if *multiline {
                            InputTextStyle::Paragraph
                        } else {
                            InputTextStyle::Short
                        };
                        let short_label: String = label.chars().take(45).collect();
                        CreateActionRow::InputText(CreateInputText::new(
                            style,
                            short_label,
                            format!("field:{i}"),
                        ))
                    })
                    .collect();
                let modal = CreateModal::new(format!("formsub:{form_id}"), spec.title.clone())
                    .components(rows);
                if let Err(e) = comp
                    .create_response(&ctx.http, CreateInteractionResponse::Modal(modal))
                    .await
                {
                    tracing::warn!("Discord: failed to open modal: {e}");
                }
                return;
            }

            // Tool-group Expand/Collapse toggle (#380): flip stored state
            // and update THIS message via the interaction response.
            if let Some(mid_str) = custom_id.strip_prefix("toolgroup:") {
                // Every path must resolve the interaction (#1949): a bare
                // `Acknowledge` or a skipped response leaves Discord
                // spinning its "didn't respond in time" toast.
                // `render_content` clamps to the 2000-char wire cap, but
                // if the API still refuses, answer with an ephemeral
                // fallback instead of leaving the click unresolved.
                use serenity::builder::{
                    CreateInteractionResponse, CreateInteractionResponseMessage,
                };
                let resp = match mid_str.parse::<u64>() {
                    Ok(mid) => match self.discord_state.toggle_tool_group(mid).await {
                        Some(group) => CreateInteractionResponse::UpdateMessage(
                            CreateInteractionResponseMessage::new()
                                .content(super::tool_group::render_content(&group))
                                .embeds(super::tool_group::render_embeds(&group))
                                .components(super::tool_group::render_components(&group, mid)),
                        ),
                        None => {
                            tracing::debug!("Discord: tool group {mid} aged out — toggle ignored");
                            CreateInteractionResponse::Message(
                                CreateInteractionResponseMessage::new()
                                    .ephemeral(true)
                                    .content("This status bubble is too old to expand."),
                            )
                        }
                    },
                    Err(_) => CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .ephemeral(true)
                            .content("This status bubble is too old to expand."),
                    ),
                };
                if let Err(e) = comp.create_response(&ctx.http, resp).await {
                    tracing::warn!("Discord: tool group toggle response failed: {e}");
                    let fallback = CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .ephemeral(true)
                            .content("Couldn't redraw the status bubble — try again shortly."),
                    );
                    if let Err(e2) = comp.create_response(&ctx.http, fallback).await {
                        tracing::warn!("Discord: tool group toggle fallback response failed: {e2}");
                    }
                }
                return;
            }

            // Tool card paging and Log (#1880). Paging edits this message in
            // place; Log sends the full list to the viewer as a Markdown file.
            if let Some(rest) = custom_id.strip_prefix("tgpage:") {
                use serenity::builder::{
                    CreateInteractionResponse, CreateInteractionResponseMessage,
                };
                let parsed = rest.split_once(':').and_then(|(mid, page)| {
                    Some((mid.parse::<u64>().ok()?, page.parse::<usize>().ok()?))
                });
                let resp = match parsed {
                    Some((mid, page)) => {
                        match self.discord_state.set_tool_group_page(mid, page).await {
                            Some(group) => CreateInteractionResponse::UpdateMessage(
                                CreateInteractionResponseMessage::new()
                                    .content(super::tool_group::render_content(&group))
                                    .embeds(super::tool_group::render_embeds(&group))
                                    .components(super::tool_group::render_components(&group, mid)),
                            ),
                            None => super::tool_group::stale_card_reply(),
                        }
                    }
                    None => super::tool_group::stale_card_reply(),
                };
                if let Err(e) = comp.create_response(&ctx.http, resp).await {
                    tracing::warn!("Discord: tool card page response failed: {e}");
                }
                return;
            }
            if let Some(mid_str) = custom_id.strip_prefix("tglog:") {
                use serenity::builder::{
                    CreateAttachment, CreateInteractionResponse, CreateInteractionResponseMessage,
                };
                let snapshot = match mid_str.parse::<u64>() {
                    Ok(mid) => self.discord_state.tool_group_snapshot(mid).await,
                    Err(_) => None,
                };
                let resp = match snapshot {
                    Some(group) => {
                        let log = super::tool_card::log_markdown(&group.entries);
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .ephemeral(true)
                                .add_file(CreateAttachment::bytes(
                                    log.into_bytes(),
                                    "tool-calls.md",
                                )),
                        )
                    }
                    None => super::tool_group::stale_card_reply(),
                };
                if let Err(e) = comp.create_response(&ctx.http, resp).await {
                    tracing::warn!("Discord: tool card log response failed: {e}");
                }
                return;
            }

            if let Some(provider_name) = custom_id.strip_prefix("provider:") {
                let resp = crate::channels::commands::models_for_provider(provider_name).await;

                // Agent-handled providers (OpenRouter 300+ models, custom)
                if resp.agent_handled {
                    let session_id = *self.shared_session.lock().await;
                    let display = crate::channels::commands::provider_display_name(provider_name);
                    let config = crate::config::Config::current();
                    if let Ok(new_provider) =
                        crate::brain::provider::factory::create_provider_by_name(
                            &config,
                            provider_name,
                        )
                        .await
                    {
                        match session_id {
                            Some(sid) => self.agent.swap_provider_for_session(
                                sid,
                                new_provider.clone(),
                                new_provider.default_model().to_string(),
                            ),
                            None => self.agent.swap_provider(new_provider),
                        }
                    }
                    if !resp.current_model.is_empty() {
                        let _ = crate::channels::commands::switch_model(
                            &self.agent,
                            &resp.current_model,
                            session_id,
                            Some(provider_name),
                        )
                        .await;
                    }
                    let _ = comp
                        .create_response(
                            &ctx.http,
                            serenity::builder::CreateInteractionResponse::Acknowledge,
                        )
                        .await;
                    if let Some(sid) = session_id {
                        let prompt = if resp.current_model.is_empty() {
                            format!(
                                "[System: User selected {} provider but no default model is set. \
                                 Ask them which model they want. Use config_manager tool to read \
                                 providers section, then set the default_model. Keep current provider \
                                 until a model is chosen.]",
                                display
                            )
                        } else {
                            format!(
                                "[System: User switched to {} provider with model {}. \
                                 Confirm the switch. Ask if they want a different model — \
                                 if so, use config_manager to update providers.{}.default_model \
                                 and confirm.]",
                                display,
                                resp.current_model,
                                if provider_name == "openrouter" {
                                    "openrouter"
                                } else {
                                    provider_name
                                }
                            )
                        };
                        let agent_clone = self.agent.clone();
                        let http = ctx.http.clone();
                        let channel_id = comp.channel_id;
                        let dstate = self.discord_state.clone();
                        tokio::spawn(async move {
                            match agent_clone.send_message(sid, prompt, None).await {
                                Ok(r) => {
                                    if let Err(e) = super::governor::say(
                                        &dstate.governor,
                                        channel_id,
                                        &http,
                                        super::governor::Surface::Send,
                                        &r.content,
                                    )
                                    .await
                                    {
                                        tracing::warn!(error = %e, "failed to send Discord agent message");
                                    }
                                }
                                Err(e) => tracing::error!("Agent follow-up failed: {}", e),
                            }
                        });
                    }
                    return;
                }

                if resp.models.is_empty() {
                    let _ = comp
                        .create_response(
                            &ctx.http,
                            serenity::builder::CreateInteractionResponse::Message(
                                serenity::builder::CreateInteractionResponseMessage::new()
                                    .content("No models available for this provider.")
                                    .ephemeral(true),
                            ),
                        )
                        .await;
                    return;
                }
                use serenity::builder::{
                    CreateActionRow, CreateButton, CreateInteractionResponse,
                    CreateInteractionResponseMessage,
                };
                use serenity::model::application::ButtonStyle;
                let rows: Vec<CreateActionRow> = resp
                    .models
                    .chunks(5)
                    .take(5)
                    .map(|chunk| {
                        CreateActionRow::Buttons(
                            chunk
                                .iter()
                                .map(|m| {
                                    let label = if *m == resp.current_model {
                                        format!("✓ {}", m)
                                    } else {
                                        m.clone()
                                    };
                                    let label = if label.len() > 80 {
                                        let mut end = 79;
                                        while !label.is_char_boundary(end) {
                                            end -= 1;
                                        }
                                        format!("{}…", &label[..end])
                                    } else {
                                        label
                                    };
                                    CreateButton::new(format!("model:{}:{}", resp.provider_name, m))
                                        .label(label)
                                        .style(ButtonStyle::Secondary)
                                })
                                .collect(),
                        )
                    })
                    .collect();
                let _ = comp
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .content(&resp.text)
                                .components(rows)
                                .ephemeral(true),
                        ),
                    )
                    .await;
                return;
            }

            // Model switch callback (format: model:<provider>:<model>)
            if let Some(rest) = custom_id.strip_prefix("model:") {
                let (provider_name, model_name) = if let Some((p, m)) = rest.split_once(':') {
                    (Some(p), m)
                } else {
                    (None, rest)
                };
                // Resolve session first so the provider swap pins to the
                // right per-session slot instead of leaking via the global.
                let session_id = *self.shared_session.lock().await;
                let mut provider_err: Option<String> = None;
                if let Some(pname) = provider_name {
                    match crate::config::Config::load() {
                        Ok(config) => {
                            match crate::brain::provider::factory::create_provider_by_name(
                                &config, pname,
                            )
                            .await
                            {
                                Ok(new_provider) => match session_id {
                                    Some(sid) => self.agent.swap_provider_for_session(
                                        sid,
                                        new_provider.clone(),
                                        new_provider.default_model().to_string(),
                                    ),
                                    None => self.agent.swap_provider(new_provider),
                                },
                                Err(e) => {
                                    provider_err = Some(format!(
                                        "Failed to create provider '{}': {}",
                                        pname, e
                                    ))
                                }
                            }
                        }
                        Err(e) => provider_err = Some(format!("Failed to load config: {}", e)),
                    }
                }
                let reply = if let Some(err) = provider_err {
                    format!("⚠️ {}", err)
                } else {
                    match crate::channels::commands::switch_model(
                        &self.agent,
                        model_name,
                        session_id,
                        provider_name,
                    )
                    .await
                    {
                        Ok(_) => format!("✅ Model switched to `{}`", model_name),
                        Err(e) => format!("⚠️ {}", e),
                    }
                };
                let _ = comp
                    .create_response(
                        &ctx.http,
                        serenity::builder::CreateInteractionResponse::Message(
                            serenity::builder::CreateInteractionResponseMessage::new()
                                .content(reply)
                                .ephemeral(true),
                        ),
                    )
                    .await;
                return;
            }

            // Session switch callback
            if let Some(session_id_str) = custom_id.strip_prefix("session:") {
                if let Ok(new_id) = session_id_str.parse::<Uuid>() {
                    let cfg = self.config_rx.borrow().clone();
                    let caller_id = comp.user.id.get();
                    let owner_id = cfg
                        .channels
                        .discord
                        .allowed_users
                        .first()
                        .and_then(|s| s.parse::<u64>().ok());
                    let is_owner = cfg.channels.discord.allowed_users.is_empty()
                        || owner_id == Some(caller_id);

                    if is_owner {
                        *self.shared_session.lock().await = Some(new_id);
                    } else {
                        self.extra_sessions
                            .lock()
                            .await
                            .insert(caller_id, (new_id, std::time::Instant::now()));
                    }
                    self.discord_state
                        .register_session_channel(new_id, comp.channel_id.get())
                        .await;
                    let display = match self.session_svc.get_session(new_id).await {
                        Ok(Some(s)) => s.title.unwrap_or_else(|| {
                            session_id_str[..8.min(session_id_str.len())].to_string()
                        }),
                        _ => session_id_str[..8.min(session_id_str.len())].to_string(),
                    };
                    let _ = comp
                        .create_response(
                            &ctx.http,
                            serenity::builder::CreateInteractionResponse::Message(
                                serenity::builder::CreateInteractionResponseMessage::new()
                                    .content(format!("✅ Switched to session `{}`", display))
                                    .ephemeral(true),
                            ),
                        )
                        .await;
                } else {
                    let _ = comp
                        .create_response(
                            &ctx.http,
                            serenity::builder::CreateInteractionResponse::Message(
                                serenity::builder::CreateInteractionResponseMessage::new()
                                    .content("Invalid session ID")
                                    .ephemeral(true),
                            ),
                        )
                        .await;
                }
                return;
            }

            let (approved, always, yolo, approval_id) =
                if let Some(id) = custom_id.strip_prefix("approve:") {
                    (true, false, false, id.to_string())
                } else if let Some(id) = custom_id.strip_prefix("always:") {
                    (true, true, false, id.to_string())
                } else if let Some(id) = custom_id.strip_prefix("yolo:") {
                    (true, true, true, id.to_string())
                } else if let Some(id) = custom_id.strip_prefix("deny:") {
                    (false, false, false, id.to_string())
                } else {
                    tracing::warn!("Discord: unknown interaction custom_id: {}", custom_id);
                    let _ = comp
                        .create_response(
                            &ctx.http,
                            serenity::builder::CreateInteractionResponse::Acknowledge,
                        )
                        .await;
                    return;
                };

            // OC-01: the approval keyboard sits in a channel where any member
            // can press it, so re-check that the tapper is the owner before
            // acting. Without this a non-owner tap runs the pending tool, and a
            // YOLO tap persists auto-always for the whole instance. The session
            // switch branch above already re-checks; the tool-approval buttons
            // did not. Uses the canonical owner resolver, so an empty allowlist
            // is unconfigured (deny), not "everyone is owner".
            {
                let cfg = self.config_rx.borrow().clone();
                let caller = comp.user.id.get().to_string();
                let is_owner = crate::config::owner::is_owner(
                    &cfg.channels.discord.allowed_users,
                    &cfg.channels.discord.bot_owner,
                    &caller,
                );
                if !is_owner {
                    tracing::warn!(
                        "Discord: non-owner {} tapped '{}' — refused (OC-01)",
                        caller,
                        custom_id
                    );
                    let _ = comp
                        .create_response(
                            &ctx.http,
                            serenity::builder::CreateInteractionResponse::Message(
                                serenity::builder::CreateInteractionResponseMessage::new()
                                    .content("⛔ Only the owner can approve tool calls.")
                                    .ephemeral(true),
                            ),
                        )
                        .await;
                    return;
                }
            }

            if yolo {
                crate::utils::persist_auto_always_policy();
            }

            let resolved = self
                .discord_state
                .resolve_pending_approval(&approval_id, approved, always)
                .await;
            tracing::info!(
                "Discord approval resolved: id={}, approved={}, always={}, found_pending={}",
                approval_id,
                approved,
                always,
                resolved
            );
            if !resolved {
                tracing::warn!(
                    "Discord: no pending approval for id={} — may have timed out or already resolved",
                    approval_id
                );
            }

            // Ack the interaction so Discord doesn't show "interaction failed"
            let _ = comp
                .create_response(
                    &ctx.http,
                    serenity::builder::CreateInteractionResponse::Acknowledge,
                )
                .await;
        }
    }
}
