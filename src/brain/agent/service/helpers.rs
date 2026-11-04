use super::builder::AgentService;
use super::types::{MessageQueueCallback, ProgressCallback, ProgressEvent};
use crate::brain::provider::{ContentBlock, LLMRequest, LLMResponse, Message, StopReason};
use serde_json::Value;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Reminder appended to the system prompt for Xiaomi MiMo models. mimo
/// habitually narrates tool calls in prose ("Running checks now.") or emits
/// them as `<tool_call_list>{…}</tool_call_list>` text instead of using the
/// structured tool-call field, which leaves the turn doing nothing. The
/// phantom self-heal and the `<tool_call_list>` parser recover most of these
/// after the fact; this addresses the cause up front.
pub(crate) const MIMO_TOOL_CALL_HINT: &str = "## Tool calls — required format\n\
When you need to run a tool, emit it ONLY as a real structured tool call. Never \
write a tool call as text or JSON in your visible message or in your reasoning \
(e.g. `<tool_call>`, `<tool_call_list>`, or a raw `{\"tool_name\": …}` object) — \
text like that is NOT executed and the action silently does nothing. Do not \
announce that you are about to act (\"Running the tests now.\", \"Let me check \
the logs.\") and then stop: in the same turn, actually call the tool. Only write \
a plain-text reply once the work is genuinely done.";

/// True for Xiaomi MiMo models (`mimo-v2.5-pro`, `mimo-v2-flash`, …), which
/// need the structured-tool-call reminder above.
pub(crate) fn is_mimo_model(model: &str) -> bool {
    model.to_ascii_lowercase().contains("mimo")
}

/// The system nudge injected when a model produced a reasoning-only turn (#692
/// follow-up). CRITICAL: when `no_tools_yet` (no tool has executed this turn),
/// the model reasoned but never acted, so it most likely still needs to CALL a
/// tool — the nudge must ENCOURAGE the structured tool call, never suppress it.
/// The old text ("tool results above are sufficient — do not call more tools")
/// sabotaged exactly that and left the agent narrating with no way to act. Only
/// once a tool HAS run do we steer toward "write the answer from the results".
/// Should a completed iteration that produced no answer be nudged (#978)?
///
/// The trigger is an empty ANSWER and nothing else. It previously also demanded
/// 40+ characters of reasoning, which let the worst case escape: a reply with no
/// answer AND no reasoning failed the check, so it was never nudged and the turn
/// simply ended delivering nothing. That capped the counter at 1/5 in practice,
/// which in turn meant the retry budget was never exhausted and the fallback
/// chain was never walked. Going quieter must not be a way out of the guard.
///
/// CLI providers are excluded because they run their own loop internally, and
/// iteration 0 is excluded because the opening call has not yet had a chance to
/// act on anything.
pub(crate) fn should_nudge_empty_answer(
    iteration: usize,
    is_cli_provider: bool,
    iteration_text: &str,
) -> bool {
    iteration > 0 && !is_cli_provider && iteration_text.trim().is_empty()
}

pub(crate) fn empty_reasoning_nudge(no_tools_yet: bool, attempt: u32) -> &'static str {
    if no_tools_yet {
        match attempt {
            1 => {
                "[System: Your last turn produced only internal reasoning — no tool call and no \
                  reply. If you need to DO something (read a file, run a command, check git, fetch \
                  data), CALL the correct tool NOW through the structured tool-call API — do NOT \
                  describe the tool in text, that does nothing. If you already have everything you \
                  need, write the answer as plain text instead. Pick one and act on this turn.]"
            }
            2 => {
                "[System: Again only reasoning — no action. Decide NOW: either call the tool you \
                  need via the structured tool-call API (not text, not JSON in your message), or \
                  write the final answer as plain text. Do exactly one of them this turn.]"
            }
            _ => {
                "[System: Still no tool call and no reply after reasoning. Invoke the required \
                  tool through the structured API now, or write the answer. Another reasoning-only \
                  turn will switch the conversation to a fallback provider automatically.]"
            }
        }
    } else {
        match attempt {
            1 => {
                "[System: Your previous turn produced only internal reasoning and no visible \
                  reply. The tool results above are sufficient — write the answer now as plain \
                  text (tables, prose, or whatever the user asked for). Do not re-reason; only \
                  call another tool if you genuinely still need more data.]"
            }
            2 => {
                "[System: Second nudge — you again produced only reasoning. Output the answer as \
                  plain text on this turn using the tool results you already have.]"
            }
            3 => {
                "[System: Third nudge. Stop reasoning. Reply now in plain prose, one or two short \
                  paragraphs, from the results above. No <thinking>, no internal monologue.]"
            }
            4 => {
                "[System: Fourth nudge — final warning before fallback. Emit a visible text reply \
                  NOW. If you produce another reasoning-only turn the conversation will switch to \
                  a different provider automatically.]"
            }
            _ => {
                "[System: Fifth and last nudge. Reply in plain text on this turn or the system \
                  will hand the conversation to a fallback provider on the next turn.]"
            }
        }
    }
}

/// Assistant message for the empty-reasoning nudge (#692): a preserve_thinking
/// model produced a reasoning-only turn (reasoning_content, no visible answer),
/// and we are about to nudge it for the answer. The reasoning MUST be echoed
/// back — qwen3.8-max-preview keeps thinking always on and requires the COMPLETE
/// reasoning_content in history — so carry it as a leading `Thinking` block (the
/// encoder emits it as `reasoning_content`). Dropping it (an empty message) makes
/// the model re-reason from scratch on every nudge, i.e. the 200s runaway loop.
/// Returns `None` when there is no reasoning to preserve: the caller must then
/// append NOTHING. It previously returned an empty assistant message in that
/// case, which was harmless while only one nudge could ever fire, and actively
/// destructive once the escalation reached 5/5 (#979). Five `[empty assistant]
/// [nudge]` pairs accumulated on the context, and that same context was handed
/// to every fallback provider, so all of them answered a conversation full of
/// empty assistant turns and returned nothing.
/// Conversation to hand a fallback provider after the nudges failed (#979).
///
/// A fallback is a fresh attempt at what the user asked, not a continuation of
/// the dialogue that just failed. `pre_nudge_len` is where the conversation
/// stood before the first nudge, so everything after it is scaffolding: "you
/// reasoned without answering", repeated up to five times. Sending that made
/// every fallback answer a conversation full of failure notices and return
/// nothing.
///
/// Falls back to the full list when no boundary was recorded (no nudge ever
/// fired) or when it is out of range, so this can only ever trim scaffolding,
/// never lose real history.
pub(crate) fn fallback_messages(
    pre_nudge_len: Option<usize>,
    messages: &[Message],
) -> Vec<Message> {
    match pre_nudge_len {
        Some(n) if n <= messages.len() => messages[..n].to_vec(),
        _ => messages.to_vec(),
    }
}

pub(crate) fn assistant_reasoning_stub(reasoning: Option<&str>) -> Option<Message> {
    match reasoning {
        Some(r) if !r.trim().is_empty() => Some(Message {
            role: crate::brain::provider::Role::Assistant,
            content: vec![ContentBlock::Thinking {
                thinking: r.to_string(),
                signature: None,
            }],
        }),
        _ => None,
    }
}

/// Reduce a CLI text block to its `<<react:…>>` directive when clearing the
/// displayable text after an IntermediateText flush (#547). The displayable
/// text was already delivered as an intermediate message, so it is dropped to
/// avoid duplication — but a `<<react:…>>` marker is a DIRECTIVE, not display
/// text. Clearing it left a react-only CLI turn reaching delivery with empty
/// `response.content`, so delivery never took the react-only path and settled a
/// bare "Finished" flow block. Keeping the marker (rebuilt from the first
/// recognized emoji) lets delivery treat the turn as react-only, matching the
/// API providers. Returns the bare directive `<<react:EMOJI>>`, or "" when the
/// block carried no react directive.
pub(crate) fn retain_react_directive(text: &str) -> String {
    match crate::utils::extract_react_marker(text) {
        (_, Some(emoji)) => format!("<<react:{emoji}>>"),
        (_, None) => String::new(),
    }
}

/// Pick the stream-handshake timeout based on what kind of provider
/// we're calling. Pulled out as a free `pub(crate)` function so the
/// matrix is unit-testable without spinning up a real async stream.
///
/// Branches:
///   - CLI providers (claude-cli, opencode-cli, etc.): **10 min**.
///     Subprocess startup + auth refresh can be slow.
///   - Local HTTP (llama.cpp / MLX / Ollama / LM Studio): **90s**.
///     30s killed real Unsloth cold starts (2026-04-17 20:18: 35B
///     gguf loading KV cache succeeded at +38s); 90s still fails an
///     unrecoverable wedge in under 2 min via the retry chain.
///   - The built-in OpenCrabs endpoint (`inference.opencrabs.com`):
///     **120s**. It is a proxy in front of upstream models, so a cold
///     upstream can legitimately need longer than 60s to produce
///     response headers. A handshake that lands at 70s or 100s is slow,
///     not broken, and the 60s cloud budget was killing it and burning
///     a retry on a healthy request (#2232).
///   - Cloud HTTP (OpenAI, Anthropic, Zhipu, opencode.ai, NVIDIA NIM,
///     etc.): **60s**. Healthy cloud gateways return headers in <5s,
///     but routing proxies (dialagram, openrouter) can take 20-45s
///     when upstream is slow. Wedged servers are >120s, so 60s still
///     catches real hangs in under 3 min via the retry chain. Previous
///     30s killed legitimate requests to slower-but-healthy providers.
///
/// The endpoint class is a host check, not a provider-name check, so a
/// `[providers.custom.*]` section that reaches the same endpoint gets the
/// same budget.
pub(crate) fn handshake_timeout_for(cli_handles_tools: bool, base_url: Option<&str>) -> Duration {
    if cli_handles_tools {
        Duration::from_secs(600)
    } else if base_url.is_some_and(crate::brain::provider::factory::is_local_base_url) {
        Duration::from_secs(90)
    } else if crate::brain::provider::opencrabs_install::is_opencrabs_endpoint(base_url) {
        Duration::from_secs(120)
    } else {
        Duration::from_secs(60)
    }
}

/// The connect budget for **one HTTP send** — connect + TLS + response headers.
///
/// Each HTTP provider applies this at its own `.send()` call
/// (`custom_openai_compatible.rs`, `anthropic.rs`, `gemini.rs`). It must NOT be
/// applied by the caller: `stream_complete` wraps `provider.stream(request)`,
/// the whole subtree, so a wall there is shared by the provider-internal
/// rate-limit wait, the in-place retry backoff and the fallback chain walk
/// (#680/#682). A timeout raised by this function means one send failed — the
/// interval it was sized for — so a log line stops reporting our own backoff as
/// a connect failure.
///
/// `cli_handles_tools` providers spawn a subprocess instead of sending HTTP, so
/// there is no send to bound: `None`, and such a provider bounds its own
/// subprocess instead.
pub(crate) fn send_handshake_timeout_for(
    cli_handles_tools: bool,
    base_url: Option<&str>,
) -> Option<Duration> {
    if cli_handles_tools {
        None
    } else {
        Some(handshake_timeout_for(cli_handles_tools, base_url))
    }
}

/// The **caller-level** handshake wall, for subprocess providers only.
///
/// `stream_complete` applies this around `provider.stream(request)`. That
/// subtree is the wrong place for an HTTP budget (#680/#682): it also holds the
/// provider-internal rate-limit wait, the retry backoff and the fallback chain
/// walk, all of which stretch on a loaded host, so the wall fired while no
/// single hop had failed. An HTTP provider now applies its own budget at each
/// `.send()` instead, and returns `None` here.
///
/// A CLI provider spawns a subprocess, so its budget is a **process-startup**
/// one and this level is the only one that can carry it — nothing inside those
/// providers bounds startup on its own.
pub(crate) fn cli_startup_timeout_for(
    cli_handles_tools: bool,
    base_url: Option<&str>,
) -> Option<Duration> {
    if cli_handles_tools {
        Some(handshake_timeout_for(cli_handles_tools, base_url))
    } else {
        None
    }
}

/// Sleep for `dur`, aborting early when the cancel token fires (#1148).
///
/// Returns `false` only when cancellation won — the caller should stop
/// whatever retry/backoff sequence it is inside of. With no token this is a
/// plain sleep and always returns `true`. Shared by every stream-retry
/// backoff site so `/stop` never rides out an exponential backoff chain.
pub(crate) async fn cancellable_backoff(token: Option<&CancellationToken>, dur: Duration) -> bool {
    let Some(token) = token else {
        tokio::time::sleep(dur).await;
        return true;
    };
    tokio::select! {
        _ = tokio::time::sleep(dur) => true,
        _ = token.cancelled() => false,
    }
}

/// #1984: drop-time cleanup of the claude mirror rows a stream leaves behind.
///
/// Each `task_started` event mirrors a row into the background manager so
/// surfaces show the CLI's backgrounded work (#1776 seam 2), and the CLI's
/// own `task_notification` removes it. When that notification never arrives
/// (the task was stopped, the CLI died mid-run) the row lived forever and
/// every later turn's typing tick pinged on it: the stuck indicator #1984
/// reports. The marker set is exactly the rows this process mirrored in, so
/// draining it when the stream (and with it the CLI process) ends closes
/// every row this process could still own. A genuine survivor is unaffected:
/// its late completion wakes the session through the synthetic survival
/// delivery, which keys on the notification, not on the row.
struct ClaudeMirrorExitGuard<'a> {
    service: &'a AgentService,
    session_id: Uuid,
}

impl Drop for ClaudeMirrorExitGuard<'_> {
    fn drop(&mut self) {
        let Some(mgr) = self.service.background_manager() else {
            return;
        };
        // No `expect` inside Drop: a poisoned lock means another turn died
        // mid-marker-update, and panicking from a destructor would abort the
        // process over a cosmetic row. Skipping silently is safe because the
        // next turn's entry clear still removes the markers.
        if let Ok(mut tasks) = self.service.claude_turn_tasks.lock() {
            super::background_tasks::clear_claude_mirrors_for_session(
                &mut tasks,
                &mgr,
                self.session_id,
            );
        }
    }
}

impl AgentService {
    /// Token count for the serialized schemas of ALL registered tools — the
    /// upper-bound tool overhead. Used as the cl100k baseline and as the
    /// calibration guard's `expected` ceiling, where over-counting (vs the
    /// lazy-filtered set actually sent) only makes the guard more lenient.
    /// The per-request filtering lives in `tool_schemas_for_session`.
    pub(super) fn actual_tool_schema_tokens(&self) -> usize {
        crate::brain::tokenizer::count_tokens(
            &serde_json::to_string(&self.tool_registry.get_tool_definitions()).unwrap_or_default(),
        )
    }

    /// The tool schemas to attach to a request for `session_id`. In lazy-tools
    /// mode this is the CORE set + `tool_search` + whatever EXTENDED tools the
    /// session has activated via `tool_search`; otherwise it's every
    /// registered tool (the historical behaviour). Single source of truth so
    /// the ~9 request-build sites in the tool loop stay in lock-step.
    pub(super) fn tool_schemas_for_session(
        &self,
        session_id: uuid::Uuid,
    ) -> Vec<crate::brain::provider::Tool> {
        // #2134: a guest turn is scoped before the lazy split, so a stranger is
        // never shown a schema the registry would refuse.
        let guest_scope = self.is_guest_session(session_id);
        let mut defs = if self.lazy_tools {
            let active = self.tool_registry.active_tools(session_id);
            self.tool_registry
                .get_tool_definitions_scoped(&active, guest_scope)
        } else if guest_scope {
            self.tool_registry.get_tool_definitions_for_guest()
        } else {
            self.tool_registry.get_tool_definitions()
        };
        // #1706: append recorded-success-rate warnings to the descriptions
        // the model routes on, so a dead tool's schema can no longer outrank
        // reality. Cache-backed, never blocks the request path.
        let health = self.tool_health_annotations();
        if !health.is_empty() {
            for def in defs.iter_mut() {
                if let Some(warning) = health.get(&def.name) {
                    def.description.push_str(warning);
                }
            }
        }
        defs
    }

    /// Stream a request and accumulate into an LLMResponse.
    ///
    /// Sends text deltas to the progress callback as `StreamingChunk` events
    /// so the TUI can display them in real-time. Returns the full response
    /// once the stream completes, ready for tool extraction.
    ///
    /// `override_cb` takes precedence over the service-level `self.progress_callback`
    /// so per-call callbacks (e.g. Telegram) receive real-time streaming chunks.
    ///
    /// `queue_cb` + `queued_out`: CLI providers only. When a queued user message
    /// is consumed mid-stream at a tool boundary, it is written to `queued_out`
    /// so the caller can inject it into context after the stream ends.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn stream_complete(
        &self,
        session_id: Uuid,
        request: LLMRequest,
        cancel_token: Option<&CancellationToken>,
        override_cb: Option<&ProgressCallback>,
        queue_cb: Option<&MessageQueueCallback>,
        queued_out: Option<&tokio::sync::Mutex<Option<super::types::QueuedUserMessage>>>,
        suppress_callback: bool,
    ) -> std::result::Result<(LLMResponse, Option<String>), crate::brain::provider::ProviderError>
    {
        use crate::brain::provider::{ContentDelta, StreamEvent, TokenUsage};
        use futures::StreamExt;

        // suppress_callback=true skips all progress events (used during compaction
        // to prevent the compaction LLM response from leaking as visible TUI text).
        let effective_cb: Option<&ProgressCallback> = if suppress_callback {
            None
        } else {
            override_cb.or(self.progress_callback.as_ref())
        };

        let provider = self.provider_for_session(session_id);

        // Invariant: never send a {provider, model} pair the user didn't
        // configure. If the request's model isn't in this provider's
        // supported list, remap to the provider's own default. This
        // catches every path that might end up with a mismatched pair —
        // cancelled fallback that bypassed its Drop guard, stale
        // session_providers entry, upstream bug — so the HTTP call
        // always goes out with a valid pair. 2026-04-18 18:14:
        // zhipu was sent `model=qwen3.6-plus` (a custom provider's model)
        // because the session's provider got stuck on a fallback
        // after a cancelled turn. That exact case is now prevented
        // at the stream site regardless of the cached state.
        let mut request = request;
        let supported = provider.supported_models();
        if !supported.is_empty() && !supported.iter().any(|m| m == &request.model) {
            let remapped = provider.default_model().to_string();
            tracing::warn!(
                "stream_complete: provider '{}' does not support model '{}' — remapping to '{}' (never send a pair the user never configured)",
                provider.name(),
                request.model,
                remapped,
            );
            request.model = remapped;
        }
        let request_model = request.model.clone();

        // Bound the stream handshake. For HTTP providers the budget is applied
        // INSIDE each provider at its own `.send()` (#680/#682): wrapping
        // `provider.stream(request)` here would charge our own rate-limit wait,
        // retry backoff and fallback walk to a clock sized for connect + TLS +
        // response headers — the wall then fired while no single hop had failed,
        // and the failure was reported as a handshake timeout. Only a CLI
        // provider — a subprocess, not an HTTP send — keeps a wall at this
        // level, bounding process startup + auth refresh.
        let handshake_timeout =
            cli_startup_timeout_for(provider.cli_handles_tools(), provider.base_url());
        // /stop must win over the pre-first-token window too (#1148): the
        // call below contains the provider-internal rate-limit retries and
        // the fallback chain walk, none of which observe the token. Racing
        // the whole subtree drops all of it instantly on cancel instead of
        // riding out minutes of backoff.
        let handshake = async {
            match handshake_timeout {
                Some(wall) => tokio::time::timeout(wall, provider.stream(request)).await,
                None => Ok(provider.stream(request).await),
            }
        };
        let handshake_result = if let Some(token) = cancel_token {
            tokio::select! {
                res = handshake => res,
                _ = token.cancelled() => {
                    tracing::info!("🛑 stream handshake aborted — cancelled while connecting");
                    return Err(crate::brain::provider::ProviderError::Internal(
                        "cancelled by user".to_string(),
                    ));
                }
            }
        } else {
            handshake.await
        };
        let mut stream = match handshake_result {
            Ok(Ok(s)) => {
                // Per-call provenance (#969): which chain entry actually
                // served this call, with the session id so an incident
                // can tell "fallback advanced" apart from "different
                // session". Read AFTER the await: a sticky promote
                // happens inside provider.stream(), so the label must
                // reflect the entry that won the handshake.
                tracing::info!(
                    "Streaming call served: session={} {} model='{}'",
                    session_id,
                    provider.provenance_label(),
                    // The model that RAN, not the one asked for (#1254): a
                    // chain entry without the requested model remapped to its
                    // own default inside the call above.
                    provider.served_model(&request_model),
                );
                s
            }
            Ok(Err(e)) => {
                crate::config::health::record_failure(provider.name(), &e.to_string());
                return Err(e);
            }
            Err(_elapsed) => {
                // Reachable only when a wall exists at this level — the CLI
                // class. An HTTP provider's send budget is enforced inside the
                // provider, where the error is already a `Timeout`/`HttpError`.
                let secs = handshake_timeout.map_or(0, |wall| wall.as_secs());
                tracing::warn!(
                    "⏱️ stream handshake timeout after {}s ({}); retry chain will fire",
                    secs,
                    provider.base_url().unwrap_or("<no-base-url>"),
                );
                crate::config::health::record_failure(
                    provider.name(),
                    &format!("handshake timeout after {}s", secs),
                );
                return Err(crate::brain::provider::ProviderError::Timeout(secs));
            }
        };

        // Accumulate state from stream events
        let mut id = String::new();
        let mut model = String::new();
        let mut stop_reason: Option<StopReason> = None;
        let mut input_tokens = 0u32;
        let mut output_tokens = 0u32;
        let mut cache_creation_tokens = 0u32;
        let mut cache_read_tokens = 0u32;
        let mut billing_cache_creation = 0u32;
        let mut billing_cache_read = 0u32;
        // Provider-reported dollars: the LAST report wins — OpenRouter and
        // friends put the authoritative total on the final usage chunk, and
        // MiniMax-style double deltas start at zero (#1707).
        let mut cost_usd: Option<f64> = None;

        // --- Active-streaming-time accumulator ---
        // Tracks the wall-clock time spent actually receiving output
        // deltas (text / reasoning / thinking / tool-use JSON), with
        // gaps longer than IDLE_GAP_SECS treated as idle (tool exec,
        // network round-trip between message blocks, throttling).
        // Used as the tok/s denominator instead of total turn wall-
        // clock so the rate reflects the model's actual sustained
        // generation speed, not the average-including-tool-exec rate.
        //
        // Invariant matches the TUI's StreamingTpsTracker: a window
        // starts at the first delta event and extends to the most
        // recent delta event. A delta arriving more than 1s after the
        // previous one closes the current window and opens a new one.
        // The wall-clock measured for each window is
        // last_in_window - first_in_window, so a single isolated
        // delta contributes 0 seconds (correct — one event has no
        // measurable duration on its own).
        const IDLE_GAP_SECS: f64 = 1.0;
        let mut active_secs: f64 = 0.0;
        let mut window_start: Option<std::time::Instant> = None;
        let mut last_delta_at: Option<std::time::Instant> = None;
        let note_delta = |now: std::time::Instant,
                          active_secs: &mut f64,
                          window_start: &mut Option<std::time::Instant>,
                          last_delta_at: &mut Option<std::time::Instant>| {
            match (*window_start, *last_delta_at) {
                (None, _) => {
                    *window_start = Some(now);
                }
                (Some(start), Some(last)) => {
                    if (now - last).as_secs_f64() > IDLE_GAP_SECS {
                        *active_secs += (last - start).as_secs_f64();
                        *window_start = Some(now);
                    }
                }
                (Some(_), None) => {
                    *window_start = Some(now);
                }
            }
            *last_delta_at = Some(now);
        };

        // --- Text repetition detection ---
        // Some providers (e.g. MiniMax) loop the same content indefinitely without
        // sending a stop signal. We keep a sliding window of recent text chunks and
        // detect when a long enough substring repeats, indicating a stuck loop.
        let mut total_text_len: usize = 0;
        let mut text_window = String::new(); // rolling window of recent text
        const REPEAT_WINDOW: usize = 2048; // bytes to keep in window
        const REPEAT_MIN_MATCH: usize = 200; // minimum repeated substring to trigger

        // Track partial content blocks by index
        // Text blocks: accumulate text deltas
        // ToolUse blocks: accumulate JSON deltas
        struct BlockState {
            block: ContentBlock,
            json_buf: String, // for tool use JSON accumulation
        }
        let mut block_states: Vec<BlockState> = Vec::new();
        let mut reasoning_buf = String::new();
        let mut reasoning_window = String::new(); // rolling window for reasoning repetition
        const REASONING_REPEAT_WINDOW: usize = 8192; // reasoning can legitimately be longer
        const REASONING_REPEAT_MIN_MATCH: usize = 300; // min substring to detect reasoning loops
        let is_cli = provider.cli_handles_tools();
        // #1984: the CLI process ends with this stream, on every path
        // (clean, errored, unwound). Its mirror rows must end with it, or
        // one lost notification holds a row alive forever and every later
        // typing tick pings on it (see ClaudeMirrorExitGuard).
        let _claude_mirror_exit = if is_cli {
            Some(ClaudeMirrorExitGuard {
                service: self,
                session_id,
            })
        } else {
            None
        };
        // CLI: track unflushed text so we can emit IntermediateText at tool
        // boundaries, giving the TUI real-time text→tools→text interleaving
        // during streaming instead of one massive wall after stream ends.
        let mut cli_unflushed_text = String::new();

        // Maximum idle time between SSE events before treating as a dropped
        // connection. NVIDIA/Kimi and some other providers occasionally hang
        // silently without sending [DONE] — this timeout lets the retry logic
        // in tool_loop.rs recover instead of blocking the TUI forever.
        //
        // Three-way split:
        //
        //   CLI: 1h. Subprocess runs tools internally (cargo build, gh, etc.).
        //
        //   Local HTTP server (llama.cpp / Unsloth / LM Studio / Ollama / MLX):
        //   1h. Hardware + model size varies too much to pick a smaller
        //   number confidently — 4B on an M3 Ultra emits in seconds, but a
        //   70B gguf on an older Mac or a LAN box running MoE 100B+ can
        //   spend tens of minutes on prefill for a full-context prompt. The
        //   handshake timeout (90s) above still catches genuinely dead
        //   servers fast; once headers arrive the server is clearly alive
        //   and just working — don't cut it off on a guess. The user can
        //   always Esc if a turn truly runs away.
        //
        //   Remote HTTP: 20s default (or custom stream_idle_timeout_secs).
        //   Cross-continent latency plus LLM warmup is covered during handshake;
        //   once streaming, inter-chunk silence exceeding 20s indicates a frozen
        //   stream proxy or dropped TCP connection.
        let is_local = provider
            .base_url()
            .map(crate::brain::provider::factory::is_local_base_url)
            .unwrap_or(false);
        let stream_idle_timeout = if let Some(dur) = provider.stream_idle_timeout() {
            dur
        } else if is_cli || is_local {
            std::time::Duration::from_secs(3600)
        } else {
            std::time::Duration::from_secs(20)
        };

        // --- Thinking-loop guard (#890, scoped and disarming per #1690) ---
        // If the model streams for `thinking_loop_timeout_secs` without
        // emitting a single tool call, the guard fires. Two things changed
        // here:
        //
        // 1. SCOPE. The ceiling resolves [providers.<name>] → [agent], asked
        //    of the provider that is actually streaming, so a long-reasoning
        //    model no longer inherits the global number and a phantom-prone
        //    one can be tightened without touching every other provider.
        //    `0` disables the guard for that provider (CLI providers force 0:
        //    they run tools internally, so a tool-less stream is normal).
        //
        // 2. DISARM ON DELIVERY. The guard used to `return Err` unconditionally,
        //    which threw away whatever the model had already written and made
        //    the tool loop replay the whole turn. Now it only kills a stream
        //    that delivered NOTHING — the genuine "thinking forever, saying
        //    nothing" signature. A stream that is delivering tokens but emitting
        //    no tool calls stands the clock down and finishes its answer:
        //    silence is `stream_idle_timeout`'s job, narrated tool intent is the
        //    post-success phantom detector's job (#1672), and a runaway
        //    repetition is caught by the windows below.
        let thinking_loop_timeout_secs = if is_cli {
            0
        } else {
            provider.thinking_loop_timeout().unwrap_or_else(|| {
                crate::config::Config::current()
                    .agent
                    .thinking_loop_timeout_secs
            })
        };
        let thinking_loop_deadline = if thinking_loop_timeout_secs > 0 {
            Some(
                tokio::time::Instant::now()
                    + std::time::Duration::from_secs(thinking_loop_timeout_secs),
            )
        } else {
            None
        };
        // Separate from `thinking_loop_deadline` because the select future above
        // borrows the deadline; this flag is what the arm is allowed to clear.
        let mut thinking_loop_armed = thinking_loop_deadline.is_some();
        let mut has_tool_call = false;

        loop {
            // Race stream.next() against cancellation token and idle timeout.
            // This ensures /stop takes effect immediately even mid-chunk.
            let next = tokio::select! {
                biased;
                _ = async {
                    if let Some(token) = cancel_token {
                        token.cancelled().await;
                    } else {
                        // No cancel token — never resolves
                        std::future::pending::<()>().await;
                    }
                } => {
                    tracing::info!("Stream cancelled by user");
                    break;
                }
                _ = async {
                    match thinking_loop_deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending::<()>().await,
                    }
                }, if !has_tool_call && thinking_loop_armed => {
                    if last_delta_at.is_some() {
                        // #1690: the clock expired on a stream that IS delivering.
                        // Discarding it would throw away the answer the user waited
                        // for and make the tool loop replay the whole turn, so stand
                        // the guard down and keep consuming. Silence is still
                        // bounded by `stream_idle_timeout` below.
                        tracing::warn!(
                            "🧠 Thinking-loop guard (#890/#1690): {}s elapsed with zero tool calls, \
                             but the stream has delivered content — standing the clock down \
                             instead of discarding the answer.",
                            thinking_loop_timeout_secs
                        );
                        thinking_loop_armed = false;
                    } else {
                        tracing::warn!(
                            "🧠 Thinking-loop timeout (#890): {}s elapsed with zero tool calls and \
                             nothing delivered. Killing stream for phantom enforcement retry.",
                            thinking_loop_timeout_secs
                        );
                        return Err(crate::brain::provider::ProviderError::ThinkingLoopTimeout(
                            thinking_loop_timeout_secs,
                        ));
                    }
                    // Disarmed on a delivering stream. `biased` above means the
                    // stream branch was never polled for this iteration, so
                    // re-polling without a value loses no chunk.
                    continue;
                }
                result = tokio::time::timeout(stream_idle_timeout, stream.next()) => {
                    match result {
                        Ok(Some(item)) => item,
                        Ok(None) => break, // Stream ended normally
                        Err(_elapsed) => {
                            tracing::warn!(
                                "⏱️ Stream idle timeout after {}s — no event received from provider. \
                                 Treating as dropped stream (stop_reason=None → will retry).",
                                stream_idle_timeout.as_secs()
                            );
                            break; // stop_reason stays None → triggers retry in tool_loop
                        }
                    }
                }
            };

            let event = match next {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("Stream error: {}", e);
                    return Err(e);
                }
            };

            match event {
                StreamEvent::MessageStart { message } => {
                    id = message.id;
                    model = message.model;
                    input_tokens = message.usage.input_tokens;
                }
                StreamEvent::ContentBlockStart {
                    index,
                    content_block,
                } => {
                    // Ensure block_states has enough capacity
                    while block_states.len() <= index {
                        block_states.push(BlockState {
                            block: ContentBlock::Text {
                                text: String::new(),
                            },
                            json_buf: String::new(),
                        });
                    }
                    // Separate thinking blocks from different rounds with a blank line
                    if matches!(content_block, ContentBlock::Thinking { .. })
                        && !reasoning_buf.is_empty()
                    {
                        reasoning_buf.push_str("\n\n");
                        // Also emit separator to TUI so streaming display stays in sync
                        if let Some(cb) = effective_cb {
                            cb(
                                session_id,
                                ProgressEvent::ReasoningChunk {
                                    text: "\n\n".to_string(),
                                },
                            );
                        }
                    }
                    // #890: flag a tool call BEFORE moving content_block
                    // into block_states, then assign.
                    if matches!(content_block, ContentBlock::ToolUse { .. }) {
                        has_tool_call = true;
                    }
                    block_states[index] = BlockState {
                        block: content_block,
                        json_buf: String::new(),
                    };
                }
                StreamEvent::ContentBlockDelta { index, delta } => {
                    if index < block_states.len() {
                        // Every output-bearing delta — text, reasoning,
                        // thinking, tool-use JSON — extends the active
                        // streaming window. Done once up-front so the
                        // four match arms below don't each need to
                        // remember to call note_delta.
                        note_delta(
                            std::time::Instant::now(),
                            &mut active_secs,
                            &mut window_start,
                            &mut last_delta_at,
                        );
                        match delta {
                            ContentDelta::TextDelta { text } => {
                                // Forward to TUI / per-call callback for real-time display
                                if let Some(cb) = effective_cb {
                                    cb(
                                        session_id,
                                        ProgressEvent::StreamingChunk { text: text.clone() },
                                    );
                                }
                                // CLI: track unflushed text for tool-boundary flushing
                                if is_cli {
                                    cli_unflushed_text.push_str(&text);
                                }
                                // Accumulate into block
                                if let ContentBlock::Text { text: ref mut t } =
                                    block_states[index].block
                                {
                                    t.push_str(&text);
                                }

                                // --- Repetition & size detection ---
                                total_text_len += text.len();
                                text_window.push_str(&text);
                                if text_window.len() > REPEAT_WINDOW {
                                    let mut drain = text_window.len() - REPEAT_WINDOW;
                                    // Advance to a valid char boundary
                                    while !text_window.is_char_boundary(drain)
                                        && drain < text_window.len()
                                    {
                                        drain += 1;
                                    }
                                    text_window.drain(..drain);
                                }

                                // Check for repeated substring in window, with
                                // fenced code excluded. Repeated literal blocks
                                // are NORMAL in a correct technical answer — two
                                // SQL variants differing by a table name, a
                                // before/after diff, a pair of migrations — and
                                // byte-identical runs there look exactly like a
                                // loop. One such answer was terminated at 3322
                                // bytes and reached the user torn in half (#788).
                                // Prose repetition, which is the real loop signal,
                                // is still fully detected.
                                if detect_text_repetition(
                                    &strip_fenced_code(&text_window),
                                    REPEAT_MIN_MATCH,
                                ) {
                                    tracing::warn!(
                                        "🔁 Repetition detected in streaming response after {} bytes. \
                                         Provider appears to be looping. Terminating stream.",
                                        total_text_len,
                                    );
                                    stop_reason = Some(StopReason::EndTurn);
                                    break;
                                }
                            }
                            ContentDelta::InputJsonDelta { partial_json } => {
                                block_states[index].json_buf.push_str(&partial_json);
                            }
                            ContentDelta::ReasoningDelta { text } => {
                                if let Some(cb) = effective_cb {
                                    cb(
                                        session_id,
                                        ProgressEvent::ReasoningChunk { text: text.clone() },
                                    );
                                }
                                // Always accumulate for DB persistence
                                reasoning_buf.push_str(&text);
                                reasoning_window.push_str(&text);
                                if reasoning_window.len() > REASONING_REPEAT_WINDOW {
                                    let mut drain =
                                        reasoning_window.len() - REASONING_REPEAT_WINDOW;
                                    while !reasoning_window.is_char_boundary(drain)
                                        && drain < reasoning_window.len()
                                    {
                                        drain += 1;
                                    }
                                    reasoning_window.drain(..drain);
                                }
                                if detect_text_repetition(
                                    &reasoning_window,
                                    REASONING_REPEAT_MIN_MATCH,
                                ) {
                                    tracing::warn!(
                                        "🔁 Repetition detected in reasoning after {} bytes. \
                                         Model appears to be looping in its thinking. \
                                         Terminating stream.",
                                        reasoning_buf.len(),
                                    );
                                    stop_reason = Some(StopReason::EndTurn);
                                    break;
                                }
                                // A `!!!!!` style run (GLM-5.3-Flash degeneration,
                                // #1351): end the stream the same way, so the
                                // empty-answer nudge path takes over.
                                if let Some((ch, len)) = super::reasoning_run::degenerate_run(
                                    &reasoning_window,
                                    super::reasoning_run::MIN_RUN,
                                ) {
                                    tracing::warn!(
                                        "🔁 Same-character run in reasoning after {} bytes \
                                         ({len} x {ch:?}). Model output has degenerated. \
                                         Terminating stream.",
                                        reasoning_buf.len(),
                                    );
                                    stop_reason = Some(StopReason::EndTurn);
                                    break;
                                }
                            }
                            ContentDelta::ThinkingDelta { thinking } => {
                                // Anthropic native thinking_delta — same as reasoning
                                if let Some(cb) = effective_cb {
                                    cb(
                                        session_id,
                                        ProgressEvent::ReasoningChunk {
                                            text: thinking.clone(),
                                        },
                                    );
                                }
                                reasoning_buf.push_str(&thinking);
                                reasoning_window.push_str(&thinking);
                                if reasoning_window.len() > REASONING_REPEAT_WINDOW {
                                    let mut drain =
                                        reasoning_window.len() - REASONING_REPEAT_WINDOW;
                                    while !reasoning_window.is_char_boundary(drain)
                                        && drain < reasoning_window.len()
                                    {
                                        drain += 1;
                                    }
                                    reasoning_window.drain(..drain);
                                }
                                if detect_text_repetition(
                                    &reasoning_window,
                                    REASONING_REPEAT_MIN_MATCH,
                                ) {
                                    tracing::warn!(
                                        "🔁 Repetition detected in thinking after {} bytes. \
                                         Model appears to be looping in its thinking. \
                                         Terminating stream.",
                                        reasoning_buf.len(),
                                    );
                                    stop_reason = Some(StopReason::EndTurn);
                                    break;
                                }
                                if let Some((ch, len)) = super::reasoning_run::degenerate_run(
                                    &reasoning_window,
                                    super::reasoning_run::MIN_RUN,
                                ) {
                                    tracing::warn!(
                                        "🔁 Same-character run in thinking after {} bytes \
                                         ({len} x {ch:?}). Model output has degenerated. \
                                         Terminating stream.",
                                        reasoning_buf.len(),
                                    );
                                    stop_reason = Some(StopReason::EndTurn);
                                    break;
                                }
                            }
                        }
                    }
                }
                StreamEvent::ContentBlockStop { index } => {
                    if index < block_states.len() {
                        // Finalize tool use blocks: parse accumulated JSON with
                        // partial-repair fallback so a truncated stream surfaces
                        // partial intent instead of {}.
                        {
                            let state = &mut block_states[index];
                            if let ContentBlock::ToolUse { ref mut input, .. } = state.block
                                && !state.json_buf.is_empty()
                            {
                                *input = crate::brain::provider::json_repair::parse_or_repair(
                                    &state.json_buf,
                                );
                            }
                        }
                        // CLI: flush accumulated text as IntermediateText before
                        // emitting tool events, so TUI shows text→tools sequentially
                        // during streaming instead of one wall after stream ends.
                        // Also clear the text from prior text blocks so the final
                        // response.content only contains text emitted AFTER the
                        // last flush (preventing complete_response from
                        // overwriting the last intermediate msg with duplicate text).
                        let is_tool =
                            matches!(block_states[index].block, ContentBlock::ToolUse { .. });
                        if is_cli
                            && is_tool
                            && !cli_unflushed_text.is_empty()
                            && let Some(cb) = effective_cb
                        {
                            cb(
                                session_id,
                                ProgressEvent::IntermediateText {
                                    text: cli_unflushed_text.clone(),
                                    // None lets the TUI pull from its
                                    // accumulated streaming_reasoning
                                    reasoning: None,
                                },
                            );
                            cli_unflushed_text.clear();
                            for bs in block_states.iter_mut() {
                                if let ContentBlock::Text { text: ref mut t } = bs.block {
                                    t.clear();
                                }
                            }
                        }
                        // Emit ToolStarted + ToolCompleted with fully parsed input
                        // so the TUI shows real tool context (command, file path, etc.)
                        //
                        // CLI-ONLY: for CLI providers (claude-cli, qwen-cli, opencode),
                        // the CLI runs tools itself and the stream-close is the ONLY
                        // signal we get — so we synthesize the lifecycle here.
                        //
                        // For non-CLI providers (OpenAI-compatible, Anthropic, etc.)
                        // tool_loop owns the full lifecycle: it fires ToolStarted
                        // before invoking the tool and ToolCompleted with the real
                        // output. Firing here too would DOUBLE every event, bloat
                        // the tool-call count in the TUI, and leave a phantom
                        // "Processing: <tool>" indicator because the premature
                        // fake completion races the real one. See the 6-in-85µs
                        // duplication observed in logs.
                        if is_cli {
                            let state = &mut block_states[index];
                            if let ContentBlock::ToolUse {
                                ref id,
                                ref name,
                                ref input,
                                ..
                            } = state.block
                                && let Some(cb) = effective_cb
                            {
                                let emit_name = name.to_lowercase();
                                cb(
                                    session_id,
                                    ProgressEvent::ToolStarted {
                                        tool_name: emit_name.clone(),
                                        tool_input: input.clone(),
                                        tool_call_id: id.clone(),
                                    },
                                );
                                cb(
                                    session_id,
                                    ProgressEvent::ToolCompleted {
                                        tool_name: emit_name,
                                        tool_input: input.clone(),
                                        success: true,
                                        summary: String::new(),
                                        tool_call_id: id.clone(),
                                    },
                                );

                                // CLI only: check if user queued a message during
                                // tool execution. Consume it and break the stream
                                // so tool_loop can inject it into context.
                                if let Some(qcb) = queue_cb
                                    && let Some(queued) = qcb(session_id).await
                                {
                                    tracing::info!(
                                        "Queued user message at CLI tool boundary — storing for tool_loop"
                                    );
                                    // Only store — don't emit QueuedUserMessage here.
                                    // tool_loop emits it AFTER CLI interleaving so it
                                    // appears in the correct position (after all tools).
                                    if let Some(buf) = queued_out {
                                        *buf.lock().await = Some(queued);
                                    }
                                    stop_reason = Some(StopReason::EndTurn);
                                    break;
                                }
                            }
                        }
                    }
                }
                StreamEvent::MessageDelta { delta, usage } => {
                    // Only update stop_reason if the delta carries one — deferred
                    // usage chunks send a second MessageDelta with stop_reason=None
                    // that must not overwrite the real stop_reason.
                    if delta.stop_reason.is_some() {
                        stop_reason = delta.stop_reason;
                    }
                    // Take the largest values — MiniMax sends two deltas:
                    // first (0,0), then the real usage. Other providers
                    // may only send one. Using max() handles both cases.
                    if usage.input_tokens > input_tokens {
                        input_tokens = usage.input_tokens;
                    }
                    if usage.output_tokens > output_tokens {
                        output_tokens = usage.output_tokens;
                    }
                    // Per-call cache tokens (context window proxy)
                    if usage.cache_creation_tokens > cache_creation_tokens {
                        cache_creation_tokens = usage.cache_creation_tokens;
                    }
                    if usage.cache_read_tokens > cache_read_tokens {
                        cache_read_tokens = usage.cache_read_tokens;
                    }
                    // Billing cache tokens (cumulative across CLI rounds)
                    if usage.billing_cache_creation > billing_cache_creation {
                        billing_cache_creation = usage.billing_cache_creation;
                    }
                    if usage.billing_cache_read > billing_cache_read {
                        billing_cache_read = usage.billing_cache_read;
                    }
                    if usage.cost_usd.is_some() {
                        cost_usd = usage.cost_usd;
                    }
                }
                StreamEvent::MessageStop => break,
                StreamEvent::Ping => {
                    // CLI providers (opencode, claude-cli, qwen-cli) emit Ping
                    // at step_finish / tool_result boundaries. Use them as flush
                    // points so the TUI shows each step's tool calls + thinking
                    // live, instead of batching everything into one giant group
                    // at end-of-stream. Skip if there's nothing pending — empty
                    // flushes inflate the message count and waste vertical space.
                    if is_cli
                        && !cli_unflushed_text.is_empty()
                        && let Some(cb) = effective_cb
                    {
                        cb(
                            session_id,
                            ProgressEvent::IntermediateText {
                                text: std::mem::take(&mut cli_unflushed_text),
                                reasoning: None,
                            },
                        );
                        // Clear DISPLAYABLE text from prior text blocks so the
                        // final response.content only contains text emitted
                        // AFTER this flush — prevents complete_response from
                        // overwriting the last intermediate msg with duplicate
                        // text. BUT preserve a `<<react:…>>` DIRECTIVE: it is not
                        // displayable text, and clearing it meant a react-only
                        // CLI turn reached delivery with empty content, so the
                        // react-only path never ran and a bare "Finished" flow
                        // block was left behind (#547). Keeping the marker lets
                        // delivery treat the turn as react-only (react + block
                        // cleanup), matching the API providers.
                        for bs in block_states.iter_mut() {
                            if let ContentBlock::Text { text: ref mut t } = bs.block {
                                *t = retain_react_directive(t);
                            }
                        }
                    }
                }
                // #1776 seams 2+3: mirror the CLI's background-task lifecycle
                // into the session's background manager (surfaces show a
                // backgrounded claude task as in-flight work), and deliver
                // post-exit survivor notifications synthetically: a
                // notification for a task started THIS turn is claude's to
                // see natively (silent); one for an earlier turn's task is a
                // survivor — the spawning process is gone — so it rides the
                // ONE gated route (fork #19, interrupt=true per fork #13).
                // Cosmetic mirror / missing id = skip silently, never fail a
                // turn over it.
                StreamEvent::BackgroundTask {
                    subtype,
                    task_id,
                    status,
                    description,
                    ..
                } => {
                    let Some(id) = task_id.as_deref() else {
                        continue;
                    };
                    let label = super::background_tasks::claude_task_label(id);
                    match (subtype.as_str(), status.as_deref()) {
                        ("task_started", _) => {
                            self.claude_turn_tasks
                                .lock()
                                .expect("claude turn-task lock")
                                .insert((session_id, id.to_string()));
                            if let Some(mgr) = self.background_manager() {
                                mgr.mirror_started(session_id, &label);
                            }
                        }
                        ("task_notification", st) => {
                            // #1984: any status a notification carries is
                            // terminal. Claude also reports stopped, cancelled
                            // and killed, and the old arm keyed the row removal
                            // on completed|failed only, so every other status
                            // left the mirror row alive forever, which is the
                            // stuck typing indicator this issue reports. Clear
                            // first, unconditionally; the synthetic wake stays
                            // limited to the two statuses that carry a result
                            // worth delivering.
                            if let Some(mgr) = self.background_manager() {
                                mgr.mirror_finished(session_id, &label);
                            }
                            if matches!(st, Some("completed") | Some("failed"))
                                && super::background_tasks::claude_needs_survival_delivery(
                                    &self
                                        .claude_turn_tasks
                                        .lock()
                                        .expect("claude turn-task lock"),
                                    session_id,
                                    id,
                                )
                            {
                                let msg = super::background_tasks::claude_completion_message(
                                    id,
                                    st.unwrap_or("unknown"),
                                    description.as_deref(),
                                );
                                match super::session_routes::deliver_to_session(
                                    session_id, msg, true,
                                ) {
                                    super::session_routes::Delivery::NoRoute => {
                                        tracing::warn!(
                                            target: "background_task",
                                            "claude task '{label}' completion for session \
                                             {session_id} had nowhere to go"
                                        );
                                    }
                                    _ => {
                                        tracing::info!(
                                            target: "background_task",
                                            "delivered post-exit claude task '{label}' \
                                             completion for session {session_id}"
                                        );
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                StreamEvent::Error { error } => {
                    crate::config::health::record_failure(provider.name(), &error);
                    return Err(crate::brain::provider::ProviderError::StreamError(error));
                }
            }
        }

        // CLI: flush any trailing text after the last tool
        if is_cli
            && !cli_unflushed_text.is_empty()
            && let Some(cb) = effective_cb
        {
            cb(
                session_id,
                ProgressEvent::IntermediateText {
                    text: cli_unflushed_text,
                    reasoning: None,
                },
            );
        }

        // Detect premature stream termination — if we accumulated blocks
        // but never got a stop_reason, the connection MAY have dropped
        // before [DONE]/MessageStop. BUT not every provider honours the
        // `[DONE]` protocol; some (observed: dialagram + qwen-3.7-max-
        // thinking, 2026-05-30) simply close the TCP connection at end
        // of response without emitting `finish_reason` or `[DONE]`.
        //
        // Treating those as failures triggered the user-visible
        // pathology: a complete response rendered in the chat, then 3
        // pointless retries (~minutes each) regenerating the same
        // content, then a fallback-provider switch — all while the
        // "is responding..." indicator climbed past 8 minutes.
        //
        // Discriminator: if the response carries a tool_use block, we
        // MUST retry — incomplete tool calls produce broken state. For
        // text-only responses, check whether the accumulated text
        // looks structurally complete (ends with terminal punctuation,
        // closing fence, etc.) and synthesise stop_reason = EndTurn
        // when it does. Only bail to retry when the content truly
        // looks mid-sentence.
        if stop_reason.is_none() && !block_states.is_empty() {
            let has_tool_use = block_states
                .iter()
                .any(|bs| matches!(&bs.block, ContentBlock::ToolUse { .. }));
            let text: String = block_states
                .iter()
                .filter_map(|bs| match &bs.block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            if !has_tool_use && crate::utils::text_complete::text_looks_complete(&text) {
                tracing::info!(
                    "Stream ended without [DONE] but text looks complete \
                     ({} blocks, {} output tokens, last 40 chars: {:?}) — \
                     synthesising EndTurn instead of retrying",
                    block_states.len(),
                    output_tokens,
                    text.chars()
                        .rev()
                        .take(40)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect::<String>(),
                );
                stop_reason = Some(StopReason::EndTurn);
            } else {
                let msg = format!(
                    "Stream ended without [DONE]: {} content blocks, {} output tokens — connection likely dropped",
                    block_states.len(),
                    output_tokens,
                );
                tracing::warn!("⚠️ {}", msg);
                return Err(crate::brain::provider::ProviderError::StreamError(msg));
            }
        }

        // Self-heal: detect truncated responses disguised as complete.
        // Some providers (notably Qwen) occasionally send finish_reason="stop"
        // with a usage chunk after only a handful of tokens, producing a response
        // like "Let me check the current state:" — clearly mid-thought.  The
        // premature-termination guard above can't catch this because stop_reason
        // IS set (from the finish_reason chunk).  Detect it by checking: very low
        // output tokens + text that looks like an incomplete preamble (ends with
        // `:` or `...`).  Returning StreamError lets retry/rotation/fallback
        // re-issue the request instead of accepting garbage.
        if stop_reason == Some(StopReason::EndTurn) && output_tokens > 0 && output_tokens < 100 {
            let has_tool_use = block_states
                .iter()
                .any(|bs| matches!(&bs.block, ContentBlock::ToolUse { .. }));
            if !has_tool_use {
                let text: String = block_states
                    .iter()
                    .filter_map(|bs| match &bs.block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                let trimmed = text.trim();
                if trimmed.ends_with(':') || trimmed.ends_with("...") {
                    // Heuristic to reduce false positives: if the text contains
                    // multiple sentences (period, exclamation) it's more likely
                    // a legitimate short instruction than a truncated preamble.
                    // Real truncations are single preamble sentences like
                    // "Let me check the current state:" — no prior punctuation.
                    let before_last = crate::utils::string::prefix_before_last_char(trimmed);
                    let has_prior_sentence = before_last.contains('.') || before_last.contains('!');
                    if has_prior_sentence {
                        tracing::debug!(
                            "Self-heal: skipping truncation check — text contains \
                             prior sentences (likely deliberate short response)"
                        );
                    } else {
                        let preview = if trimmed.len() > 80 {
                            crate::utils::string::truncate_str_tail(trimmed, 80)
                        } else {
                            trimmed
                        };
                        let msg = format!(
                            "Self-heal: provider sent stop after only {} output tokens: \
                             response appears truncated: \"{}\"",
                            output_tokens, preview,
                        );
                        tracing::warn!("⚠️ {}", msg);
                        if let Some(cb) = effective_cb {
                            cb(
                                session_id,
                                ProgressEvent::SelfHealingAlert {
                                    message: msg.clone(),
                                },
                            );
                        }
                        return Err(crate::brain::provider::ProviderError::StreamError(msg));
                    }
                }
            }
        }

        // Build final content blocks from accumulated state
        // Filter out empty text blocks — Anthropic rejects "text content blocks must be non-empty"
        let content_blocks: Vec<ContentBlock> = block_states
            .into_iter()
            .map(|s| s.block)
            .filter(|b| !matches!(b, ContentBlock::Text { text } if text.is_empty()))
            .collect();

        // Track provider health. (last-good config snapshotting moved to the
        // config watcher, which fires when config actually changes and parses
        // cleanly — an LLM call succeeding says nothing about config validity,
        // and the once-per-process gate left the snapshot months stale.)
        crate::config::health::record_success(provider.name());

        let reasoning = if reasoning_buf.is_empty() {
            None
        } else {
            Some(reasoning_buf)
        };

        // Finalize the active-streaming window: close the currently-
        // open window using (last_delta - window_start). Same shape as
        // the TUI's StreamingTpsTracker::finalize — window time is
        // always measured between first and last delta, never extended
        // to "now", so a long idle tail after the last token doesn't
        // dilute the rate. Returns None when the stream never
        // delivered a delta (active_secs == 0), so the channel footer
        // shows no tok/s instead of a fake "0 tok/s" or NaN.
        let final_active_secs = match (window_start, last_delta_at) {
            (Some(start), Some(last)) => active_secs + (last - start).as_secs_f64().max(0.0),
            _ => active_secs,
        };
        let streaming_active_secs = if final_active_secs > 0.0 {
            Some(final_active_secs)
        } else {
            None
        };

        // Streaming leak flag (fork #66, ex-upstream opencrabs/opencrabs#1260):
        // text was already emitted to display as deltas, so there is no
        // retro-strip here — the flag lets the tool loop attempt one
        // corrective retry and otherwise fail clean instead of accepting
        // the residue as a final answer.
        let tool_text_leak =
            crate::brain::provider::json_repair::content_has_unrecovered_tool_text(&content_blocks);

        Ok((
            LLMResponse {
                id,
                // OpenAI-compatible gateways may return a backend-local model
                // ID in stream chunks (for example, without the provider
                // prefix). Keep the requested ID for the persisted session
                // pair, just like the non-streaming response path does.
                model: if request_model.is_empty() {
                    model
                } else {
                    request_model
                },
                content: content_blocks,
                stop_reason,
                usage: TokenUsage {
                    input_tokens,
                    output_tokens,
                    cache_creation_tokens,
                    cache_read_tokens,
                    billing_cache_creation,
                    billing_cache_read,
                    cost_usd,
                    ..Default::default()
                },
                streaming_active_secs,
                tool_text_leak,
            },
            reasoning,
        ))
    }

    /// Build a user Message from text, converting any `<<IMG:path>>` markers into
    /// a plain-text `[image attached: path]` hint.
    ///
    /// Images are deliberately NOT inlined as multimodal `image_url` content
    /// blocks. The agent views an attached image on demand via the
    /// `analyze_image` tool, which uses the provider's configured `vision_model`,
    /// or Gemini from onboarding — that's the intended, working design.
    ///
    /// Inlining the raw base64 image here (the previous behaviour) attached an
    /// `image_url` content block that rode along to EVERY provider, including
    /// text-only fallbacks like zhipu/glm. Those reject it with
    /// `400 messages.content.type is invalid, allowed values: ['text']`, which
    /// broke the fallback chain whenever an image message hit a text-only
    /// provider (Telegram, 2026-06-12). Keeping the user message text-only means
    /// any provider can handle it and vision stays in `analyze_image`.
    pub(crate) fn build_user_message(text: &str) -> Message {
        let mut clean_text = text.to_string();
        while let Some(start) = clean_text.find("<<IMG:") {
            let Some(end) = clean_text[start..].find(">>") else {
                break; // malformed marker
            };
            let marker_end = start + end + 2;
            let img_path = clean_text[start + 6..start + end].to_string();
            let hint = format!("[image attached: {img_path}]");
            clean_text = format!(
                "{}{}{}",
                &clean_text[..start],
                hint,
                &clean_text[marker_end..]
            );
        }
        Message::user(clean_text.trim().to_string())
    }

    /// Compact tool description for DB persistence (mirrors TUI's format_tool_description).
    /// Paths / commands collapse `$HOME` → `~` so channel displays don't expose
    /// the user's full home path and don't waste the truncation budget on
    /// a constant prefix.
    pub(super) fn format_tool_summary(tool_name: &str, tool_input: &Value) -> String {
        use crate::utils::string::tilde_home;
        let raw = match tool_name {
            "bash" => {
                let cmd = tool_input
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                // Drop the leading `cd` and flatten newlines (#790, #791).
                let label = crate::utils::command_label::command_label(cmd);
                let label = if label.is_empty() { "?" } else { &label };
                format!("bash: {}", tilde_home(label))
            }
            "read_file" | "read" => {
                let path = tool_input
                    .get("path")
                    .or_else(|| tool_input.get("file_path"))
                    .or_else(|| tool_input.get("filePath"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                format!("Read {}", tilde_home(path))
            }
            "write_file" | "write" => {
                let path = tool_input
                    .get("path")
                    .or_else(|| tool_input.get("file_path"))
                    .or_else(|| tool_input.get("filePath"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                format!("Write {}", tilde_home(path))
            }
            "edit_file" | "edit" => {
                let path = tool_input
                    .get("path")
                    .or_else(|| tool_input.get("file_path"))
                    .or_else(|| tool_input.get("filePath"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                format!("Edit {}", tilde_home(path))
            }
            "ls" => {
                let path = tool_input
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".");
                format!("ls {}", tilde_home(path))
            }
            "glob" => {
                let p = tool_input
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                format!("Glob {}", p)
            }
            "grep" => {
                let p = tool_input
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let path = tool_input
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if path.is_empty() {
                    format!("Grep '{}'", p)
                } else {
                    format!("Grep '{}' in {}", p, tilde_home(path))
                }
            }
            "web_search" | "exa_search" | "brave_search" => {
                let q = tool_input
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                format!("Search: {}", q)
            }
            "plan" => {
                let op = tool_input
                    .get("operation")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                format!("Plan: {}", op)
            }
            "task_manager" => {
                let op = tool_input
                    .get("operation")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                format!("Task: {}", op)
            }
            "memory_search" => {
                let q = tool_input
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                format!("Memory: {}", q)
            }
            other => other.to_string(),
        };
        // Redact inline secrets (Bearer tokens, api_key=, URL passwords)
        // before this summary is persisted to the DB and rendered on
        // channels. Without this a `curl -H "Authorization: Bearer …"`
        // command leaked the key into the session history and channel
        // tool bubbles (2026-06-07). The agent still runs the real command;
        // only the persisted/displayed summary is redacted.
        crate::utils::sanitize::redact_command(&raw)
    }

    /// Normalize hallucinated tool names from providers.
    ///
    /// Some models (e.g. MiniMax) send tool names like `"Plan: complete_task"`
    /// instead of `tool="plan"` with `operation="complete_task"` in the input.
    /// This recovers the intended call so it doesn't fail with "Tool not found".
    pub(crate) fn normalize_tool_call(
        name: String,
        mut input: serde_json::Value,
    ) -> (String, serde_json::Value) {
        // "Plan: <op>" or "plan: <op>" → tool="plan", inject operation into input
        if let Some(op) = name
            .strip_prefix("Plan: ")
            .or_else(|| name.strip_prefix("plan: "))
            .or_else(|| name.strip_prefix("Plan:"))
            .or_else(|| name.strip_prefix("plan:"))
        {
            let op = op.trim().replace(' ', "_");
            if !op.is_empty() {
                if let Some(obj) = input.as_object_mut() {
                    obj.entry("operation")
                        .or_insert_with(|| serde_json::Value::String(op));
                }
                tracing::info!(
                    "[TOOL_NORM] Normalized '{}' → tool='plan', input={:?}",
                    name,
                    input
                );
                return ("plan".to_string(), input);
            }
        }

        // Generic fallback: if name contains ": " and isn't a registered tool,
        // try the part before ": " as the tool name (lowercased)
        if name.contains(": ") {
            let parts: Vec<&str> = name.splitn(2, ": ").collect();
            if parts.len() == 2 {
                let candidate = parts[0].to_lowercase().replace(' ', "_");
                let suffix = parts[1].trim().replace(' ', "_");
                if !suffix.is_empty() {
                    if let Some(obj) = input.as_object_mut() {
                        obj.entry("operation")
                            .or_insert_with(|| serde_json::Value::String(suffix));
                    }
                    tracing::info!(
                        "[TOOL_NORM] Normalized '{}' → tool='{}', input={:?}",
                        name,
                        candidate,
                        input
                    );
                    return (candidate, input);
                }
            }
        }

        // Claude Code tool name mapping (capitalized → OpenCrabs lowercase)
        // The cc-max-proxy returns Claude Code tool names which differ from ours.
        let mapped = match name.as_str() {
            "Bash" => Some("bash"),
            "Read" => Some("read_file"),
            "Write" => Some("write_file"),
            "Edit" => Some("edit_file"),
            "Glob" => Some("glob"),
            "Grep" => Some("grep"),
            "WebSearch" => Some("web_search"),
            "WebFetch" => Some("http_request"),
            "NotebookEdit" => Some("notebook_edit"),
            _ => None,
        };
        if let Some(canonical) = mapped {
            tracing::info!(
                "[TOOL_NORM] Mapped Claude Code tool '{}' → '{}'",
                name,
                canonical
            );
            return (canonical.to_string(), input);
        }

        // Final fallback: lowercase the name (catches simple case mismatches)
        let lowered = name.to_lowercase();
        if lowered != name {
            tracing::info!("[TOOL_NORM] Lowercased tool '{}' → '{}'", name, lowered);
            return (lowered, input);
        }

        (name, input)
    }

    /// Strip XML tool-call blocks from text so raw XML
    /// doesn't get persisted to DB or shown to the user.
    /// Catches `<tool_call>`, `<tool_code>`, `<StartToolCall>`, `<minimax:tool_call>`,
    /// `<tool_use>`, `<result>`, and any `<parameter>` blocks providers hallucinate.
    /// Check if text contains actual XML tool-call blocks (not just mentions).
    /// Requires BOTH opening AND closing tags to exist so that prose mentions
    /// like `` `<tool_use>` `` don't trigger false positives.
    pub(crate) fn has_xml_tool_block(text: &str) -> bool {
        (text.contains("<tool_call>") && text.contains("</tool_call>"))
            || (text.contains("<tool_code>") && text.contains("</tool_code>"))
            || (text.contains("<StartToolCall>") && text.contains("</StartToolCall>"))
            || (text.contains("<minimax:tool_call>") && text.contains("</minimax:tool_call>"))
            || (text.contains("<invoke") && text.contains("</invoke>"))
            || (text.contains("<tool_use>") && text.contains("</tool_use>"))
    }

    /// Parse XML tool-call blocks into (name, input) pairs.
    /// Handles multiple formats MiniMax uses:
    ///   <tool_call>{"tool_name":"bash","args":{"command":"..."}}</tool_call>
    ///   <tool_call>{"name":"bash","arguments":{"command":"..."}}</tool_call>
    ///   <tool_use>{"name":"bash","input":{"command":"..."}}</tool_use>
    pub(crate) fn parse_xml_tool_calls(text: &str) -> Vec<(String, serde_json::Value)> {
        use regex::Regex;
        use std::sync::LazyLock;

        static XML_BLOCK_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r#"(?s)<(?:tool_call|tool_code|tool_use|minimax:tool_call|StartToolCall)>(.*?)</(?:tool_call|tool_code|tool_use|minimax:tool_call|StartToolCall)>"#).unwrap()
        });

        let mut results = Vec::new();
        for cap in XML_BLOCK_RE.captures_iter(text) {
            let inner = cap[1].trim();
            // Try parsing as JSON
            if let Ok(obj) = serde_json::from_str::<serde_json::Value>(inner) {
                // Extract tool name from various field names
                let name = obj
                    .get("tool_name")
                    .or_else(|| obj.get("name"))
                    .or_else(|| obj.get("function"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                if name.is_empty() {
                    continue;
                }

                // Extract input/arguments from various field names
                let input = obj
                    .get("args")
                    .or_else(|| obj.get("arguments"))
                    .or_else(|| obj.get("input"))
                    .or_else(|| obj.get("parameters"))
                    .cloned()
                    .unwrap_or(serde_json::json!({}));

                tracing::info!(
                    "[XML_TOOL_PARSE] Recovered tool call: name={}, input_keys={:?}",
                    name,
                    input.as_object().map(|o| o.keys().collect::<Vec<_>>())
                );
                results.push((name, input));
            }
        }
        results
    }

    pub(crate) fn strip_xml_tool_calls(text: &str) -> String {
        use regex::Regex;
        use std::sync::LazyLock;

        // Match only properly closed XML tool-call blocks.
        // NO |$ fallback — unclosed tags (prose mentions) must NOT match.
        static TOOL_CALL_BLOCK_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r#"(?s)(<tool_call>.*?</tool_call>|<tool_code>.*?</tool_code>|<StartToolCall>.*?</StartToolCall>|<minimax:tool_call>.*?</minimax:tool_call>|<qwen:tool_call>.*?</qwen:tool_call>|<function_calls>.*?</function_calls>|<invoke\b.*?</invoke>|<param(?:eter)?\b[^>]*>.*?</param(?:eter)?>|<tool_use>.*?</tool_use>|<tool_result>.*?</tool_result>|<result>.*?</result>)"#).unwrap()
        });

        let result = TOOL_CALL_BLOCK_RE.replace_all(text, "");

        // Orphan close-tag stripping. Models routinely emit standalone
        // `</tool_result>`, `</tool_call>`, `</invoke>`, `</function_calls>`,
        // `</qwen:tool_call>` lines without a matching opener — either
        // because the opener was already stripped by an earlier pass or
        // the model never produced one. The matched-pair regex above
        // doesn't catch these and they leak straight to the TUI.
        // 2026-05-28 user report: `</tool_result>` rendered visibly
        // between paragraphs of normal prose.
        static ORPHAN_CLOSE_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r#"(?im)^\s*</(?:tool_result|tool_call|tool_code|tool_use|invoke|function_calls|qwen:tool_call|minimax:tool_call|StartToolCall|param(?:eter)?|result)>\s*$"#).unwrap()
        });
        let result = ORPHAN_CLOSE_RE.replace_all(&result, "");

        // Also strip inline orphan close tags that appear mid-line (rare
        // but happens when the model wraps a paragraph with only the
        // closing tag at the end). Keep tight wording so we don't eat
        // prose like "we fixed the </tool_result> bug" — require the
        // close tag to be the last thing on its line OR surrounded by
        // whitespace at line edges.
        static INLINE_ORPHAN_CLOSE_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r#"(?im)\s*</(?:tool_result|tool_call|tool_code|tool_use|invoke|function_calls|qwen:tool_call|minimax:tool_call)>\s*$"#).unwrap()
        });
        let result = INLINE_ORPHAN_CLOSE_RE.replace_all(&result, "");

        result.trim().to_string()
    }

    /// Strip ALL HTML comments from text.
    ///
    /// LLMs echo or hallucinate various HTML comment markers from context:
    /// `<!-- tools-v2: ... -->`, `<!-- lens -->`, `<!-- /tools-v2>`, etc.
    ///
    /// `<!-- tools-v2: [JSON] -->` is special-cased because its JSON payload
    /// embeds arbitrary tool output — including rustc's ` --> src/foo.rs:10`
    /// span arrows, React/JSX `{/* --> */}` fragments, markdown arrow
    /// glyphs, etc. A naive non-greedy `<!--.*?-->` terminates at the FIRST
    /// inner `-->` and leaks the rest of the JSON array as visible text in
    /// the TUI (screenshot 2026-04-17 16:58 — `cargo check` output bled
    /// into chat). For the v2 marker we parse the balanced JSON array and
    /// then consume the trailing ` -->`. Generic comments keep the regex.
    pub(crate) fn strip_html_comments(text: &str) -> String {
        use regex::Regex;
        use std::sync::LazyLock;

        let mut stripped = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(start) = rest.find("<!-- tools-v2:") {
            stripped.push_str(&rest[..start]);
            let after_prefix = &rest[start + "<!-- tools-v2:".len()..];
            let array_start = match after_prefix.find('[') {
                Some(i) => i,
                None => {
                    // Malformed — drop the opener and stop scanning v2.
                    rest = after_prefix;
                    break;
                }
            };
            let scan = &after_prefix[array_start..];
            let Some(array_end_rel) = find_balanced_json_end(scan) else {
                // Array never closes: drop the whole tail (well-formed
                // writers always close; an unclosed marker would dump raw
                // JSON otherwise).
                rest = "";
                break;
            };
            let tail = &scan[array_end_rel..];
            let tail_trim_len = tail.len() - tail.trim_start().len();
            let post = &tail[tail_trim_len..];
            if let Some(stripped_end) = post.strip_prefix("-->") {
                rest = stripped_end;
            } else {
                // No closing ` -->` — conservatively skip just the array so
                // downstream regex can still catch a stray `-->` later.
                rest = tail;
            }
        }
        stripped.push_str(rest);

        // Generic HTML comments (reasoning, lens, etc.) — safe with non-greedy
        // since none of those embed tool output.
        static HTML_COMMENT_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"(?s)<!--.*?-->"#).unwrap());
        let result = HTML_COMMENT_RE.replace_all(&stripped, "");

        // Collapse any runs of 3+ newlines left by stripping
        let collapsed = result.lines().collect::<Vec<_>>().join("\n");
        let trimmed = collapsed.trim().to_string();
        static MULTI_BLANK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{3,}").unwrap());
        MULTI_BLANK.replace_all(&trimmed, "\n\n").to_string()
    }

    /// Strip the `[CONTEXT COMPACTION — …]` banner prefix from a stored
    /// message. The marker exists in DB so `messages_from_last_compaction`
    /// can find the load point, but once that scan has run we don't want
    /// the model to see the literal banner — chatty models (notably
    /// qwen-3.7-max-preview-thinking on long Telegram sessions, 2026-05-18)
    /// imitate the format and emit their own `[[CONTEXT COMPACTION` +
    /// structured summary as their visible response.
    ///
    /// Removes the banner LINE plus the blank line that follows it
    /// (`…\n\n`). The summary body that follows stays intact so the model
    /// still has the task snapshot it needs to continue. No-op when the
    /// message doesn't start with the marker.
    pub(crate) fn strip_compaction_banner(content: &mut String) {
        if !content.starts_with("[CONTEXT COMPACTION") {
            return;
        }
        if let Some(idx) = content.find("\n\n") {
            *content = content[idx + 2..].to_string();
        }
    }
}

/// Walk a JSON array starting at `s[0] == '['` and return the byte offset
/// one-past the matching `]`. Tracks string + escape state so braces,
/// brackets, quotes or `-->` arrows embedded in string values don't fool
/// the depth counter. Returns `None` if the array never closes.
fn find_balanced_json_end(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'[') {
        return None;
    }
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    for (idx, &b) in bytes.iter().enumerate() {
        if escape {
            escape = false;
            continue;
        }
        if in_string {
            match b {
                b'\\' => escape = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'[' | b'{' => depth += 1,
            b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(idx + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Detect repetition in a streaming text window.
///
/// Returns `true` if a substring of `min_match` bytes from the second half
/// of `window` also appears in the first half, indicating the provider is
/// looping the same content.
pub fn detect_text_repetition(window: &str, min_match: usize) -> bool {
    if min_match == 0 || window.len() < min_match * 2 {
        return false;
    }
    // Find a valid char boundary at or after the midpoint
    let mut half = window.len() / 2;
    while !window.is_char_boundary(half) && half < window.len() {
        half += 1;
    }
    let second_half = &window[half..];
    let mut check_len = min_match.min(second_half.len());
    // Ensure check_len lands on a char boundary within second_half
    while !second_half.is_char_boundary(check_len) && check_len < second_half.len() {
        check_len += 1;
    }
    if let Some(needle) = second_half.get(..check_len) {
        window[..half].contains(needle)
    } else {
        false
    }
}

/// #705: did the turn START on the session's OWN saved provider?
///
/// A session with no saved provider counts as matched — it legitimately
/// captures its first pair. Otherwise the resolved (active) provider must equal
/// the saved one. `false` means the turn ran on the wrong provider (a #704
/// restore gap): an involuntary remap whose {provider, model} pair must NOT be
/// persisted over the session's saved choice.
pub fn provider_matches_session(saved_provider: Option<&str>, active_provider: &str) -> bool {
    saved_provider.is_none_or(|saved| saved == active_provider)
}

/// Blank out fenced code blocks so repetition detection sees only prose.
///
/// Fences are replaced by their newlines rather than removed, so the window
/// keeps its shape and an unclosed fence (the normal case mid-stream) masks to
/// the end. A model looping inside a code block still loops in the prose around
/// it; a correct answer quoting two similar queries does not.
pub fn strip_fenced_code(window: &str) -> String {
    let mut out = String::with_capacity(window.len());
    let mut in_fence = false;
    for line in window.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            out.push('\n');
            continue;
        }
        if in_fence {
            // Preserve the line break only, so offsets stay roughly aligned.
            if line.ends_with('\n') {
                out.push('\n');
            }
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Cap on normalized length for loop-match text (#957).
const LOOP_MATCH_MAX_CHARS: usize = 400;

/// Normalize text for loop detection (#957).
///
/// Lowercase, drop digits and punctuation/symbols, collapse whitespace, cap
/// at [`LOOP_MATCH_MAX_CHARS`]. Letters from any script survive (Cyrillic
/// included), so a counter-incremented repeat like "Отправляю 1
/// подтверждение" collides with "Отправляю 2 подтверждение" while genuinely
/// different commands stay apart.
///
/// Shared by both loop-guard layers: the bash near-match in the tool loop
/// and the cross-turn announcement ring buffer.
pub fn normalize_loop_text(text: &str) -> String {
    let mut buf = String::with_capacity(text.len().min(LOOP_MATCH_MAX_CHARS * 4));
    for ch in text.chars() {
        if ch.is_alphabetic() {
            for lc in ch.to_lowercase() {
                buf.push(lc);
            }
        } else if ch.is_whitespace() {
            buf.push(' ');
        }
        // digits, punctuation, symbols: dropped
    }
    let collapsed = buf.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(LOOP_MATCH_MAX_CHARS).collect()
}

/// Normalize one STRING tool argument for the near-match signature (#1397).
///
/// Same rules as [`normalize_loop_text`] with one difference: a digit is
/// dropped only when it is a LONE token. A digit that sits inside a longer
/// alphanumeric run survives. The distinction is counter versus identifier:
/// counter loops step through `1`, `2`, `3` (`"attempt 1 of 6"`, the Luna
/// echo loop), while the numbers a shell command carries are identifiers
/// (`gh pr merge 1394`, `sed -n '490,570p'`, `git show b3b615ee`, port
/// `:8080`). [`normalize_loop_text`] erased both, so paging through one
/// file or merging PRs one by one looked like a loop, and the guard nudged
/// then broke the turn on legitimate work (#1397, four trips on
/// 2026-09-05, a PR merge and a conflict resolution dropped).
///
/// A counter that reaches two digits stops collapsing here. By then the
/// guard has already had nine near-identical iterations to fire on, and the
/// cross-turn announcement ring (which stays on [`normalize_loop_text`])
/// catches the narration that accompanies such loops.
pub fn normalize_loop_arg(text: &str) -> String {
    let mut buf = String::with_capacity(text.len().min(LOOP_MATCH_MAX_CHARS * 4));
    let mut token = String::new();
    let flush = |token: &mut String, buf: &mut String| {
        if token.chars().count() > 1 || token.chars().all(|c| !c.is_ascii_digit()) {
            buf.push_str(token);
        }
        token.clear();
    };
    for ch in text.chars() {
        if ch.is_alphabetic() {
            for lc in ch.to_lowercase() {
                token.push(lc);
            }
        } else if ch.is_ascii_digit() {
            token.push(ch);
        } else {
            flush(&mut token, &mut buf);
            if ch.is_whitespace() {
                buf.push(' ');
            } else {
                // Punctuation and symbols split tokens but are not kept, so
                // `'490,570p'` and `490,570p` still collide.
                buf.push(' ');
            }
        }
    }
    flush(&mut token, &mut buf);
    let collapsed = buf.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(LOOP_MATCH_MAX_CHARS).collect()
}

/// Whether an argument key represents a pagination or continuation token (#199).
///
/// Pagination arguments (offsets, page numbers/tokens, cursors, markers)
/// advance legitimate multi-step iteration loops over large datasets. When passed
/// as string values (e.g. `"page": "2"`, `"cursor": "cur-2"`, `"offset": "50"`),
/// running them through [`normalize_loop_arg`] would strip lone digits and punctuation,
/// collapsing distinct pages into identical signatures and causing false-positive
/// loop detection.
pub fn is_pagination_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    matches!(
        k.as_str(),
        "offset"
            | "page"
            | "page_token"
            | "next_page_token"
            | "cursor"
            | "next_cursor"
            | "starting_after"
            | "start_line"
            | "token"
            | "pagination_token"
            | "after"
            | "before"
            | "skip"
            | "marker"
            | "continuation_token"
            | "page_number"
            | "page_idx"
            | "page_index"
    ) || k.ends_with("_token")
        || k.ends_with("_cursor")
        || k.ends_with("_offset")
        || k.ends_with("_page")
}

/// Normalized near-match signature for one tool call (#961).
///
/// Tool name + ':' + one normalized part per argument field. STRING values
/// go through [`normalize_loop_arg`]: punctuation and whitespace stripped,
/// lone digits dropped so counter-incremented repeats (`"attempt 1 of 6"`
/// vs `2`) still collapse to the SAME signature, digits inside longer
/// tokens kept so identifiers (`gh pr merge 1394` vs `1390`, `sed -n
/// '490,570p'` vs `'495,580p'`) stay apart (#1397). NUMERIC and BOOLEAN
/// values are kept EXACT as `key=value` parts: they are parameters, not
/// counters — `task_order: 1` vs `2`, `start_line: 100` vs `150`,
/// `timeout_secs: 30` vs `60` are genuinely different calls, and
/// digit-stripping made the guard flag that legitimate work as a loop
/// (#82: a plan checklist progression was nudged, then broken,
/// 2026-09-02). Pagination arguments (page, cursor, offset, continuation tokens)
/// are preserved intact (#199) so legitimate paging loops do not collapse.
/// Parts are sorted so argument insertion order never changes the signature.
pub fn normalized_call_signature(name: &str, args: &Value) -> String {
    let mut sig = String::from(name);
    sig.push(':');
    match args {
        Value::Object(map) => {
            let mut parts: Vec<String> = map
                .iter()
                .map(|(key, value)| match value {
                    Value::Number(n) => format!("{key}={n}"),
                    Value::Bool(b) => format!("{key}={b}"),
                    Value::String(text) => {
                        if is_pagination_key(key) {
                            format!("{key}={}", text.trim())
                        } else {
                            format!("{key}={}", normalize_loop_arg(text))
                        }
                    }
                    other => {
                        if is_pagination_key(key) {
                            format!("{key}={}", other.to_string().trim())
                        } else {
                            format!("{key}={}", normalize_loop_arg(&other.to_string()))
                        }
                    }
                })
                .collect();
            parts.sort();
            sig.push_str(&parts.join(" "));
        }
        other => sig.push_str(&normalize_loop_arg(&other.to_string())),
    }
    sig
}
