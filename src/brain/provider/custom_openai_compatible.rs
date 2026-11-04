//! Custom OpenAI-Compatible Provider Implementation
//!
//! Implements the Provider trait for any OpenAI-compatible API, including:
//! - Official OpenAI (GPT-4, GPT-3.5, etc.)
//! - OpenRouter (100+ models)
//! - Minimax
//! - Local LLMs via LM Studio, Ollama, LocalAI
//! - Any endpoint that speaks the OpenAI chat completions protocol

use super::error::{ProviderError, Result};
use super::rate_limiter::RateLimiter;
use super::responses_routing::{is_dialect_rejection_error, responses_url};
use super::r#trait::{Provider, ProviderStream};
use super::types::*;
use crate::brain::tokenizer::{count_message_tokens, count_tokens};
use crate::config::WireDialect;
use async_trait::async_trait;
use futures::stream::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
const DEFAULT_OPENAI_API_URL: &str = "https://api.openai.com/v1/chat/completions";
// Total request ceiling for NON-STREAMING calls: `complete()`, `/models`, and
// anything else that buffers a whole body. reqwest's `.timeout()` covers the
// response body read, so the same value is also a hard wall-clock ceiling on
// an SSE stream, which is exactly how #1687 killed healthy streams. With
// `timeout_secs` unset this constant IS the ceiling every stream inherits
// (`factory.rs` calls `with_timeout` only when the key is present and > 0),
// and it sat at 60s from #217 until #1635 moved it to 300s, which relocated
// the wall instead of taking it off the stream path. Streams are served by
// `build_stream_http_client` instead, which carries no total
// ceiling; inter-chunk silence stays the app-level guard in
// `brain/agent/service/helpers.rs` (`stream_idle_timeout_secs`). Matches the
// Anthropic and Gemini providers.
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
// TCP keepalive: OS-level probes detect silent connection drops without
// waiting for the request timeout. Critical for streaming.
const DEFAULT_TCP_KEEPALIVE: Duration = Duration::from_secs(15);

fn build_http_client(timeout: Duration) -> Client {
    Client::builder()
        .timeout(timeout)
        .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
        .pool_idle_timeout(DEFAULT_POOL_IDLE_TIMEOUT)
        .pool_max_idle_per_host(2)
        .tcp_keepalive(DEFAULT_TCP_KEEPALIVE)
        .build()
        .expect("Failed to create HTTP client")
}

/// The streaming client: handshake bound by `connect_timeout`, pool and TCP
/// keepalive preserved, and deliberately NO total request timeout.
///
/// reqwest's `.timeout()` is a wall-clock ceiling on the whole exchange
/// INCLUDING the response body read, so on an SSE stream it fires no matter how
/// healthily chunks are arriving (#1687). A dead stream is the app-level
/// inter-chunk guard's job (`brain/agent/service/helpers.rs`,
/// `stream_idle_timeout_secs`), and a handshake that never lands is still
/// bounded by `connect_timeout`.
fn build_stream_http_client() -> Client {
    Client::builder()
        .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
        .pool_idle_timeout(DEFAULT_POOL_IDLE_TIMEOUT)
        .pool_max_idle_per_host(2)
        .tcp_keepalive(DEFAULT_TCP_KEEPALIVE)
        .build()
        .expect("Failed to create streaming HTTP client")
}

/// Open/close tag pairs to strip from streaming/non-streaming content.
/// Covers DeepSeek-style `<think>` and Kimi-style `<!-- reasoning -->` blocks.
/// The generic `<!--` entry catches ALL HTML comments (tools-v2, lens, /tools-v2,
/// and any future hallucinated markers) so they never reach the TUI during streaming.
/// Each entry in STRIP_CLOSE_TAGS is a list of accepted close tags (first match wins).
/// MiniMax closes `<!-- reasoning -->` with `</think>` instead of `<!-- /reasoning -->`.
/// Order matters: more specific patterns must come before the generic `<!--` catch-all.
/// NOTE: Only reasoning/markup blocks belong here — NOT XML tool-call tags.
/// Tool-call XML (`<tool_call>`, `<tool_use>`, `<result>`, etc.) must NOT be
/// stripped during streaming because the model may MENTION these tags in prose
/// (e.g. "strip `<result>` tags"). Stripping them here eats the rest of the
/// response when no closing tag arrives in the same chunk. Tool-call XML is
/// handled post-response in tool_loop.rs where the full text is available.
const STRIP_OPEN_TAGS: &[&str] = &["<think>", "<mm:think>", "<!-- reasoning -->", "<!--"];
const STRIP_CLOSE_TAGS: &[&[&str]] = &[
    &["</think>"],
    &["</mm:think>"], // MiniMax/mimo namespaced think tag — leaked to chat before this entry existed
    &["<!-- /reasoning -->", "</think>", "-->"], // Kimi uses <!-- /reasoning -->, MiniMax uses </think>, --> catches split-chunk close tags
    &["-->"],
];

/// Filter reasoning/markup blocks from a streaming chunk.
///
/// Tracks state across chunks via `inside_think`. Returns the portion of `text`
/// that is outside any stripped block. Handles tags split across chunk boundaries.
/// Maximum bytes to consume inside a `<think>...</think>` / `<!-- ... -->`
/// block before we emit a one-time warning that the close tag seems delayed.
///
/// The old value (400 bytes) was chosen when DeepSeek-style `<think>` blocks
/// were short. Qwen3 / Kimi / DeepSeek-R1 with `enable_thinking=true` routinely
/// emit 5-20 KB of reasoning before closing — 400 bytes blew past on every
/// turn, flipped `inside_think=false`, and leaked the rest of the reasoning
/// (including the orphan `</think>` tag) straight to the display. Raised to
/// 200 KB to fit real-world reasoning chains comfortably.
///
/// NOTE: when this cap is exceeded we no longer flip `inside_think=false`.
/// The original behaviour was designed for hallucinated open tags (e.g.
/// `<!-- tools-v2:` with no closer) — but flipping out of think mode in the
/// common long-reasoning case is strictly worse than holding until the close
/// actually arrives. We log once per block and keep suppressing.
const THINK_BLOCK_MAX_BYTES: usize = 200_000;

/// Longest open-tag-prefix we may have to hold back between chunks so
/// an open tag split across chunk boundaries still gets detected.
/// `"<!-- reasoning -->"` is the longest entry, but we only need to
/// carry up to `len(open_tag) - 1` bytes of it to bridge a split.
const MAX_OPEN_TAG_CARRY: usize = 17;

/// Returns `(display_text, reasoning_text)`.
///
/// `display_text` is the portion of the input that falls OUTSIDE every
/// stripped block — what the user should see.
///
/// `reasoning_text` is the portion that fell inside a `<think>`,
/// `<mm:think>`, or `<!-- reasoning -->` block. Generic HTML-comment blocks
/// (the catch-all `<!--` entry, the last in STRIP_OPEN_TAGS, covering
/// echoed `<!-- tools-v2: ... -->` markers etc.) are NOT treated as
/// reasoning — they go to neither output and are discarded.
///
/// Capturing reasoning lets the caller emit it as
/// `ContentDelta::ReasoningDelta` so models like Qwen that emit their
/// thinking inline inside message content (rather than via the OpenAI
/// `reasoning_content` field) still surface to the TUI as live
/// "Thinking..." content and get persisted as `details` on the final
/// assistant `DisplayMessage` — matching the UX of providers that emit
/// reasoning via the structured field natively.
/// Short, user-facing reason for a retry notice. Keeps the TUI "⏳ Retry
/// N/M — …" line concise and non-technical instead of dumping a raw
/// reqwest/HTTP error.
fn retry_reason(err: &super::error::ProviderError) -> String {
    use super::error::ProviderError;
    match err {
        // Surface the REAL cause (DNS failure, connection refused, TLS, …)
        // instead of a flat "connection error" that tells the user nothing.
        ProviderError::HttpError(e) => super::error::describe_reqwest_error(e),
        ProviderError::Timeout(_) => "timed out".to_string(),
        ProviderError::RateLimitExceeded(_) => {
            // #952: a HARD monthly/billing quota will not lift on retry —
            // say so instead of the generic throttle label.
            if err.is_quota_exhausted() {
                "quota exhausted".to_string()
            } else {
                "rate limited".to_string()
            }
        }
        ProviderError::ApiError { status, .. } if *status == 429 => {
            if err.is_quota_exhausted() {
                "quota exhausted".to_string()
            } else {
                "rate limited".to_string()
            }
        }
        ProviderError::ApiError { status, .. } if *status >= 500 => {
            format!("server error {status}")
        }
        ProviderError::ApiError { status, .. } => format!("HTTP {status}"),
        _ => "transient error".to_string(),
    }
}

pub(crate) fn filter_think_tags(
    text: &str,
    inside_think: &mut bool,
    active_close_tag: &mut usize,
    bytes_consumed: &mut usize,
    carry: &mut String,
) -> (String, String) {
    // Prepend any carry-over from the previous chunk so a split open
    // tag (e.g. prior chunk ended with `<!`, current chunk starts with
    // `-- tools-v2:`) can match as one unit.
    let mut owned: String;
    let input_ref: &str = if carry.is_empty() {
        text
    } else {
        owned = std::mem::take(carry);
        owned.push_str(text);
        owned.as_str()
    };
    let mut result = String::new();
    let mut reasoning = String::new();
    let mut remaining = input_ref;

    // STRIP_OPEN_TAGS indices 0 (`<think>`), 1 (`<mm:think>`), and 2
    // (`<!-- reasoning -->`) carry real reasoning content. Index 3 (`<!--`
    // generic) catches hallucinated tool markers and random HTML comments —
    // those must stay discarded.
    let is_reasoning_block = |idx: usize| idx < 3;

    loop {
        if *inside_think {
            // Safety valve: if we've consumed an unusually large amount of
            // content without finding the closing tag, log ONCE per block so
            // operators can spot a genuinely hallucinated open tag — but keep
            // `inside_think=true` and stay in suppress mode. Previously we
            // flipped out of think mode here, which leaked the rest of long
            // Qwen reasoning chains straight to the display.
            *bytes_consumed += remaining.len();
            if *bytes_consumed > THINK_BLOCK_MAX_BYTES {
                tracing::warn!(
                    "⚠️ Think-tag filter consumed {} bytes without close tag \
                     (tag_idx={}) — still waiting for close, continuing to suppress",
                    *bytes_consumed,
                    *active_close_tag,
                );
                // Reset the counter so we don't re-log every chunk; stay in
                // suppress mode until the actual close tag arrives. No
                // reasoning routing for generic `<!--` comments.
                if is_reasoning_block(*active_close_tag) {
                    reasoning.push_str(remaining);
                }
                *bytes_consumed = 0;
                break;
            }

            // Find the earliest matching close tag among the candidates for this block.
            let close_candidates = STRIP_CLOSE_TAGS[*active_close_tag];
            let earliest_close = close_candidates
                .iter()
                .filter_map(|close| remaining.find(close).map(|pos| (pos, *close)))
                .min_by_key(|(pos, _)| *pos);

            if let Some((end, close)) = earliest_close {
                if is_reasoning_block(*active_close_tag) {
                    reasoning.push_str(&remaining[..end]);
                }
                remaining = &remaining[end + close.len()..];
                *inside_think = false;
                *bytes_consumed = 0;
            } else {
                // No close in this chunk. Capture the whole remaining
                // slice as reasoning (if this IS a reasoning block) so
                // the caller can stream it live; the close tag will
                // arrive in a future chunk.
                if is_reasoning_block(*active_close_tag) {
                    reasoning.push_str(remaining);
                }
                break;
            }
        } else {
            // Find the earliest open tag
            let mut earliest: Option<(usize, usize)> = None; // (position, tag_index)
            for (i, open) in STRIP_OPEN_TAGS.iter().enumerate() {
                if let Some(pos) = remaining.find(open)
                    && earliest.is_none_or(|(best, _)| pos < best)
                {
                    earliest = Some((pos, i));
                }
            }

            if let Some((pos, tag_idx)) = earliest {
                result.push_str(&remaining[..pos]);
                remaining = &remaining[pos + STRIP_OPEN_TAGS[tag_idx].len()..];
                *inside_think = true;
                *active_close_tag = tag_idx;
                *bytes_consumed = 0;
            } else {
                // No open tag found. Check for orphan close tags — some
                // models (Mimo, Qwen) occasionally skip the opening think
                // tag entirely. When a close tag appears without a preceding
                // open, treat everything before it as reasoning content.
                // Skip the bare `-->` candidate (too generic for prose:
                // arrows, ASCII art) and empty strings — but by VALUE, not by
                // index, since `-->` also appears inside the index-1 list. The
                // specific reasoning closers (`</think>`, `<!-- /reasoning -->`)
                // still trigger orphan routing.
                let mut orphan: Option<(usize, &str)> = None;
                for close_candidates in STRIP_CLOSE_TAGS.iter() {
                    for close in close_candidates.iter() {
                        if *close == "-->" || close.is_empty() {
                            continue;
                        }
                        if let Some(pos) = remaining.find(close)
                            && orphan.is_none_or(|(best, _)| pos < best)
                        {
                            orphan = Some((pos, close));
                        }
                    }
                }
                if let Some((pos, close)) = orphan {
                    reasoning.push_str(&remaining[..pos]);
                    remaining = &remaining[pos + close.len()..];
                    continue;
                }

                // Before emitting `remaining` as final output, check if its
                // tail could be the leading prefix of an open tag that gets
                // completed by the next chunk. If so, move those bytes to
                // the carry instead of emitting them.
                let tail_keep = open_tag_prefix_len(remaining);
                if tail_keep > 0 {
                    let split_at = remaining.len() - tail_keep;
                    result.push_str(&remaining[..split_at]);
                    carry.push_str(&remaining[split_at..]);
                } else {
                    result.push_str(remaining);
                }
                break;
            }
        }
    }

    (result, reasoning)
}

/// Length of the longest suffix of `s` that's a strict prefix of any
/// marker in `markers`. Used by the streaming tool-call partitioner to
/// decide how many trailing bytes to hold back as carry when a marker
/// could be split across chunk boundaries — the exact problem local
/// llama.cpp/MLX backends create by streaming 1-3 bytes per SSE event.
///
/// Returns 0 when no suffix is a marker prefix, so the caller can emit
/// the whole working string unchanged. Respects UTF-8 char boundaries so
/// the carry always sits on a safe split point.
pub(crate) fn tool_marker_prefix_len(s: &str, markers: &[&str]) -> usize {
    let max_marker_len = markers.iter().map(|m| m.len()).max().unwrap_or(0);
    if max_marker_len <= 1 {
        return 0;
    }
    let start = s.len().saturating_sub(max_marker_len - 1);
    // Walk forward from the earliest viable start — the first match we
    // find is the longest suffix.
    for i in start..s.len() {
        if !s.is_char_boundary(i) {
            continue;
        }
        let suffix = &s[i..];
        if suffix.is_empty() {
            continue;
        }
        if markers
            .iter()
            .any(|m| m.len() > suffix.len() && m.starts_with(suffix))
        {
            return suffix.len();
        }
    }
    0
}

/// Return how many trailing bytes of `s` look like the beginning of any
/// STRIP_OPEN_TAGS entry (but not a full match). These are withheld as
/// carry so the open-tag detector can see the full tag on the next chunk.
fn open_tag_prefix_len(s: &str) -> usize {
    // Walk character boundaries from the tail so every suffix we test is
    // guaranteed to be a valid `&str`. Longest-first: return the largest
    // tail that's a proper prefix of any STRIP_OPEN_TAGS entry.
    let tail_starts = s
        .char_indices()
        .map(|(i, _)| i)
        .filter(|i| s.len() - i <= MAX_OPEN_TAG_CARRY);
    for start in tail_starts {
        let suffix = &s[start..];
        for open in STRIP_OPEN_TAGS {
            if open.len() > suffix.len() && open.starts_with(suffix) {
                return suffix.len();
            }
        }
    }
    0
}

/// Strip complete reasoning/markup blocks from non-streaming content.
pub(crate) fn strip_think_blocks(text: &str) -> String {
    let mut result = text.to_string();
    for (open, close_candidates) in STRIP_OPEN_TAGS.iter().zip(STRIP_CLOSE_TAGS.iter()) {
        while let Some(start) = result.find(open) {
            // Find the earliest close tag among the candidates.
            let earliest_close = close_candidates
                .iter()
                .filter_map(|close| result[start..].find(close).map(|end| (end, *close)))
                .min_by_key(|(end, _)| *end);

            if let Some((end, close)) = earliest_close {
                result = format!(
                    "{}{}",
                    &result[..start],
                    &result[start + end + close.len()..]
                );
            } else {
                // Unclosed tag — strip from open tag to end
                result = result[..start].to_string();
                break;
            }
        }
    }
    // Handle orphan close tags — close tag without a preceding open.
    // Some models (Mimo, Qwen) skip the opening think tag entirely,
    // so we never enter the while-loop above. Strip everything before
    // the orphan close tag as leaked reasoning. Skip the bare `-->`
    // candidate (too generic for prose) and empty strings by VALUE, not by
    // index, since `-->` also lives inside the index-1 list.
    for close_candidates in STRIP_CLOSE_TAGS.iter() {
        for close in close_candidates.iter() {
            if *close == "-->" || close.is_empty() {
                continue;
            }
            if let Some(pos) = result.find(close) {
                result = result[pos + close.len()..].to_string();
                break;
            }
        }
    }

    result.trim().to_string()
}

/// Classification for the bare top-level array of OpenAI tool-call envelopes
/// shape, e.g. `[{"id":"call_1","type":"function","function":{"name":...}}]`.
/// Some Qwen-thinking models emit this as plain content text in parallel with
/// the structured `delta.tool_calls` field (seen 2026-05-16 in logs), so the
/// model's reply ends up showing raw JSON instead of just the prose intro.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum BareToolArrayMatch {
    /// Confirmed: `[` + `{` + `"id"` + `:` + `"call_` are all present in order.
    Full,
    /// Still on the recognition path — more content could complete the match.
    Prefix,
    /// Diverged: this is not a tool-call array.
    None,
}

/// Walk a string against the rigid envelope-opener pattern
/// `[ws*{ws*"id"ws*:ws*"call_`, allowing arbitrary JSON-style whitespace
/// between structural tokens. Returns whether the input is a Full match,
/// a viable Prefix (continue buffering), or a definitive None.
///
/// The `"id":"call_` anchor is chosen because OpenAI-style tool-call IDs
/// almost universally use the `call_` prefix; using it as the recognition
/// anchor keeps false positives on legitimate JSON content (e.g. a code
/// block showing `[{"id":1,"type":"x"}]`) to near zero.
pub(crate) fn classify_bare_tool_array(s: &str) -> BareToolArrayMatch {
    // Tries to consume `literal` from the start of `t` after trimming leading
    // whitespace. Returns `Some(rest)` on full consumption, or `None` on
    // short-circuit; the `state` out-param captures whether we hit a viable
    // Prefix (more bytes could complete the match) or definite None.
    fn step<'a>(t: &'a str, literal: &str, state: &mut BareToolArrayMatch) -> Option<&'a str> {
        let t = t.trim_start();
        if t.is_empty() {
            *state = BareToolArrayMatch::Prefix;
            return None;
        }
        if t.len() < literal.len() {
            *state = if literal.starts_with(t) {
                BareToolArrayMatch::Prefix
            } else {
                BareToolArrayMatch::None
            };
            return None;
        }
        if let Some(rest) = t.strip_prefix(literal) {
            Some(rest)
        } else {
            *state = BareToolArrayMatch::None;
            None
        }
    }

    let t = s.trim_start();
    if t.is_empty() {
        return BareToolArrayMatch::Prefix;
    }
    let mut state = BareToolArrayMatch::None;
    let Some(t) = step(t, "[", &mut state) else {
        return state;
    };
    let Some(t) = step(t, "{", &mut state) else {
        return state;
    };
    let Some(t) = step(t, "\"id\"", &mut state) else {
        return state;
    };
    let Some(t) = step(t, ":", &mut state) else {
        return state;
    };
    let Some(_) = step(t, "\"call_", &mut state) else {
        return state;
    };
    BareToolArrayMatch::Full
}

/// Extract tool_call blocks emitted as text content.
///
/// Local GGUF/MLX backends serving reasoning models will often put tool
/// calls into `message.content` instead of the structured `tool_calls`
/// field when the runtime isn't in the right template mode. Unsloth's
/// parser (`studio/backend/routes/inference.py`) solves this by scanning
/// the raw text for several formats and emitting structured tool calls
/// anyway. We handle four:
///
///   1. `<tool_call>{"name":"x","arguments":{...}}</tool_call>` — Qwen XML
///   2. `<function=x><parameter=k>v</parameter></function>` — Qwen v2 XML
///   3. `tool_call:{"id":"...","type":"function","function":{...}}` —
///      bare OpenAI-envelope prefix (no tags) that Qwen3 leaks when the
///      template isn't fully in reasoning mode but the model still knows
///      it should be calling a tool
///   4. `{"tool_calls":[{...}, ...]}` — OpenAI multi-call envelope
///      hallucinated as content text
///
/// Closing tags are treated as optional — models skip them constantly.
/// JSON parsing uses balanced-brace counting with a string/escape guard
/// so `{` or `}` inside argument strings doesn't confuse the extractor.
///
/// Returns `(tool_calls, cleaned_text)` where `cleaned_text` is the input
/// with every matched block removed. If nothing matches, the text is
/// returned unchanged — safe to call on any content.
/// Find the closing tag `</name>` in `hay`, tolerating DeepSeek DSML
/// tokenizer corruption where the closer arrives as `</｜｜DSML｜｜name>` or
/// `</｜DSML｜name>` (#398, observed live with deepseek-v4-flash). Returns
/// (offset, matched_len).
fn find_closer_tolerant(hay: &str, name: &str) -> Option<(usize, usize)> {
    let candidates = [
        format!("</{name}>"),
        format!("</\u{ff5c}\u{ff5c}DSML\u{ff5c}\u{ff5c}{name}>"),
        format!("</\u{ff5c}DSML\u{ff5c}{name}>"),
    ];
    candidates
        .iter()
        .filter_map(|c| hay.find(c.as_str()).map(|p| (p, c.len())))
        .min_by_key(|(p, _)| *p)
}

pub(crate) fn extract_text_tool_calls(text: &str) -> (Vec<(String, serde_json::Value)>, String) {
    // Cheap pre-check so non-matching content pays nothing.
    let has_claude_style = KNOWN_TOOL_NAMES
        .iter()
        .any(|t| text.contains(&format!("<{}>", t)));
    // Bare top-level array of OpenAI envelopes — no `"tool_calls"` wrapper.
    // Cheap signal: `"id":"call_` is the OpenAI-envelope-with-call-prefix
    // signature; combined with a top-level `[` this is the leaked-array shape.
    let has_bare_array_signal = text.contains("\"id\":\"call_") || text.contains("\"id\": \"call_");
    // Dict-by-call-id signal: 2026-05-24 regression with qwen-3.7-max-preview
    // where the model emits tool calls as a top-level object keyed by call
    // ID instead of using the structured `tool_calls` API or the bare array
    // shape. Format: `{"call_<24hex>": {"name":"...", "arguments":{...}}}`.
    // Often appears inside a markdown ```json fence, so Telegram renders
    // the whole thing as a code block and the call never dispatches.
    let has_dict_by_id_signal =
        text.contains("\"call_") && (text.contains("\"name\"") || text.contains("\"function\""));
    // Bare-args signal: 2026-05-27 qwen-3.7-max-thinking regression on the
    // dialagram provider. The model emits orphaned tool arguments as bare
    // JSON objects in delta.content with NO call_id wrapper, NO `name`
    // field, NO XML tag — just the raw arguments object. Shape:
    //   {"command": "cd ~/srv/rs/opencrabs && grep ..."}
    // The bash tool's `command` key is unique enough to anchor on (other
    // tools take `path`, `query`, `pattern`, …; only bash uses `command`).
    // Without this signal the early-return below ships the JSON straight
    // to visible TUI content and the call never dispatches.
    let has_bare_command_args = text.contains("{\"command\":")
        || text.contains("{ \"command\":")
        || text.contains("{\"command\" :");
    // Anthropic `<invoke name="X">` signal — used standalone, inside
    // `<function_calls>`, or inside the Qwen-namespaced `<qwen:tool_call>`
    // wrapper (2026-05-27 leak: model emitted the inner Anthropic shape
    // inside qwen:tool_call and we had no parser for either layer).
    // Anchoring on `<invoke name=` is strict enough to skip prose like
    // `the <invoke> tag` while catching every real shape.
    let has_invoke_signal = text.contains("<invoke name=") || text.contains("invoke name=\"");
    // Element-style signal: 2026-07-06 deepseek-v4-flash (opencode-go)
    // regression (#398). The model emits the whole call as sibling XML
    // elements — a bare `<function>` wrapper, the tool in `<name>`, and
    // every parameter as its own child element:
    //   <function>
    //   <name>read_file</name>
    //   <path>/root/.opencrabs/MEMORY.md</path>
    //   </function>
    // None of the other shapes anchor on this (no `=name`, no JSON, no
    // `<invoke`), so it leaked verbatim to Telegram and the call never
    // dispatched.
    let has_element_style = text.contains("<function>") && text.contains("<name>");
    // Bare `{"name": "<tool>", "arguments": {...}}` signal — 2026-06-02
    // qwen-3.7-max-thinking via dialagram. The model emits the same
    // tool call TWICE: once as a proper structured `tool_calls` delta
    // (which dispatches) and once as JSON-stringified text in
    // `delta.content` (which leaks to Telegram as raw JSON). Full
    // extraction lives in `bare_tool_call_extractor` — this module is
    // already 5000+ lines, no point cramming another 100-line pattern
    // inline.
    let has_bare_name_args = super::bare_tool_call_extractor::has_bare_name_args_signal(text);

    // Corrupted-tool-call signal: 2026-07-03 mimo-v2.5 (opencode-go)
    // tokenizer bug. When the user converses in Bengali, the model
    // sometimes emits a tool call where the tool name and JSON params
    // are correct but the separator between them becomes non-ASCII
    // garbage (Bengali script like "ব্যক"). Shape:
    // `tool_search ব্যক{"query":"..."}`. The fix is model-agnostic —
    // any model producing this pattern is handled. Without this pass
    // the call falls through as plain text and never dispatches.
    let has_corrupted_tool_signal = KNOWN_TOOL_NAMES.iter().any(|&tool| {
        text.find(tool).is_some_and(|pos| {
            let after = &text[pos + tool.len()..];
            after.find('{').is_some_and(|brace_pos| {
                let gap = &after[..brace_pos];
                // Gap must contain non-ASCII bytes (the corruption) and
                // NO ASCII letters (so prose like "tool_search for {reason}"
                // is never swept in).
                gap.bytes().any(|b| b >= 0x80) && !gap.bytes().any(|b| b.is_ascii_alphabetic())
            })
        })
    });

    if !text.contains("<tool_call>")
        && !text.contains("<tool_call_list>")
        && !text.contains("<function=")
        && !text.contains("tool_call:")
        && !text.contains("\"tool_calls\"")
        && !text.contains("\"tool_call\"")
        && !has_claude_style
        && !has_bare_array_signal
        && !has_dict_by_id_signal
        && !has_bare_command_args
        && !has_invoke_signal
        && !has_bare_name_args
        && !has_corrupted_tool_signal
        && !has_element_style
    {
        return (Vec::new(), text.to_string());
    }

    // Collect byte ranges of every matched block; strip them at the end.
    let mut tool_calls: Vec<(String, serde_json::Value)> = Vec::new();
    let mut strip_ranges: Vec<(usize, usize)> = Vec::new();

    // Pass 0.5 — element-style `<function><name>X</name><param>v</param>`
    // (#398, deepseek-v4-flash). Gated on KNOWN_TOOL_NAMES so prose that
    // merely discusses a `<function>` tag is never swallowed. The closing
    // `</function>` is optional (models drop closers constantly); the
    // block then ends at the next `<function>` or end of text.
    if has_element_style {
        let mut search = 0usize;
        while let Some(rel) = text[search..].find("<function>") {
            let start = search + rel;
            let after = start + "<function>".len();
            // `<name>` must follow with only whitespace between.
            let name = text[after..]
                .find("<name>")
                .filter(|&nrel| text[after..after + nrel].trim().is_empty())
                .and_then(|nrel| {
                    let nstart = after + nrel + "<name>".len();
                    text[nstart..].find("</name>").map(|nend| {
                        (
                            text[nstart..nstart + nend].trim(),
                            nstart + nend + "</name>".len(),
                        )
                    })
                });
            let Some((tool_name, mut cursor)) = name else {
                search = after;
                continue;
            };
            if !KNOWN_TOOL_NAMES.contains(&tool_name) {
                search = after;
                continue;
            }
            let body_close = find_closer_tolerant(&text[cursor..], "function");
            let body_end = body_close
                .map(|(r, _)| cursor + r)
                .or_else(|| text[cursor..].find("<function>").map(|r| cursor + r))
                .unwrap_or(text.len());
            let mut args = serde_json::Map::new();
            while cursor < body_end {
                let Some(lt) = text[cursor..body_end].find('<') else {
                    break;
                };
                let tag_start = cursor + lt + 1;
                let Some(gt) = text[tag_start..body_end].find('>') else {
                    break;
                };
                let raw_tag = text[tag_start..tag_start + gt].trim();
                // Two element forms (#398):
                //   <path>v</path>                        — bare child key
                //   <parameter name="command" ...>v</parameter>
                //     — attributed form (second deepseek-v4-flash shape);
                //       the real key lives in the name="..." attribute and
                //       the closing tag is </parameter>.
                let (key, close): (String, String) =
                    if let Some(rest) = raw_tag.strip_prefix("parameter ") {
                        let Some(name) = rest
                            .split("name=\"")
                            .nth(1)
                            .and_then(|r| r.split('"').next())
                            .filter(|n| {
                                !n.is_empty()
                                    && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                            })
                        else {
                            cursor = tag_start + gt + 1;
                            continue;
                        };
                        (name.to_string(), "</parameter>".to_string())
                    } else if !raw_tag.is_empty()
                        && !raw_tag.starts_with('/')
                        && raw_tag
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        (raw_tag.to_string(), format!("</{raw_tag}>"))
                    } else {
                        cursor = tag_start + gt + 1;
                        continue;
                    };
                let key = key.as_str();
                let val_start = tag_start + gt + 1;
                // `close` names the tag whose closer we expect ("parameter"
                // for the attributed form, the key itself for bare keys);
                // the finder tolerates DSML-corrupted closers.
                let close_name = close
                    .trim_start_matches("</")
                    .trim_end_matches('>')
                    .to_string();
                let Some((vrel, vlen)) =
                    find_closer_tolerant(&text[val_start..body_end], &close_name)
                else {
                    cursor = val_start;
                    continue;
                };
                let raw_val = text[val_start..val_start + vrel].trim();
                // Numbers/bools/objects keep their JSON type so integer
                // params validate; everything else stays a string.
                let value = serde_json::from_str::<serde_json::Value>(raw_val)
                    .ok()
                    .filter(|v| !v.is_string())
                    .unwrap_or_else(|| serde_json::Value::String(raw_val.to_string()));
                args.insert(key.to_string(), value);
                cursor = val_start + vrel + vlen;
            }
            let mut strip_end = match body_close {
                Some((r, len)) if cursor + r == body_end => body_end + len,
                _ if text[body_end..].starts_with("</function>") => body_end + "</function>".len(),
                _ => body_end,
            };
            // Swallow trailing DSML protocol garbage: bare corrupted closers
            // like </｜｜DSML｜｜invoke> / </｜｜DSML｜｜tool_calls> that follow
            // the block are tokenizer noise, never user content.
            loop {
                let rest = &text[strip_end..];
                let trimmed = rest.trim_start();
                let ws = rest.len() - trimmed.len();
                if (trimmed.starts_with("</\u{ff5c}\u{ff5c}DSML\u{ff5c}\u{ff5c}")
                    || trimmed.starts_with("</\u{ff5c}DSML\u{ff5c}"))
                    && let Some(gtp) = trimmed.find('>')
                {
                    strip_end += ws + gtp + 1;
                } else {
                    break;
                }
            }
            tracing::info!(
                "extract_text_tool_calls: element-style <function><name> leak parsed as '{}' \
                 with {} arg(s) (#398)",
                tool_name,
                args.len()
            );
            tool_calls.push((tool_name.to_string(), serde_json::Value::Object(args)));
            strip_ranges.push((start, strip_end));
            search = strip_end;
        }
    }

    // Pass 1 — Claude-style `<TOOLNAME><PARAM>val</PARAM></TOOLNAME>`.
    // Seen in logs 2026-04-17 14:27 where unsloth Qwen emitted a
    // <bash><command>curl ...</command></bash> block instead of calling
    // the structured `tool_calls` API. Run BEFORE the tag-based scan so
    // `<bash>...</bash>` doesn't get mistaken for a prose <bash> mention
    // elsewhere.
    if has_claude_style {
        for (start, end, name, input) in extract_claude_style_tool_calls(text) {
            tool_calls.push((name, input));
            strip_ranges.push((start, end));
        }
    }

    // Pass 1.5 — bare top-level array of OpenAI envelopes (no `tool_calls`
    // wrapper). Seen 2026-05-16 with `qwen-3.6-max-preview-thinking`: the
    // model double-emits, putting the entire `[{"id":"call_...","type":
    // "function","function":{"name":..., "arguments":{...}}}]` array into
    // `delta.content` AND firing the real `delta.tool_calls` chunk. The
    // structured path drives the actual tool call; without this pass the
    // text-content copy survives to the TUI as raw JSON.
    //
    // Locate every `"id":"call_` anchor, walk back to the nearest `[`
    // within a short window, run balanced extraction, parse as a JSON
    // array of envelopes. Anchor distance is bounded so prose like
    // "the field `\"id\":\"call_1\"` is set" doesn't sweep in unrelated
    // brackets earlier in the response.
    if has_bare_array_signal {
        let anchors = ["\"id\":\"call_", "\"id\": \"call_"];
        let mut search_from = 0;
        loop {
            let next = anchors
                .iter()
                .filter_map(|a| text[search_from..].find(a).map(|p| (search_from + p, *a)))
                .min_by_key(|(p, _)| *p);
            let Some((anchor_pos, anchor_lit)) = next else {
                break;
            };

            // Walk back up to 64 bytes to find the wrapping `[`.
            let window_start = anchor_pos.saturating_sub(64);
            let bracket_pos = text[window_start..anchor_pos]
                .rfind('[')
                .map(|r| window_start + r);
            let Some(arr_pos) = bracket_pos else {
                search_from = anchor_pos + anchor_lit.len();
                continue;
            };

            // Skip if already covered by an earlier strip range.
            if strip_ranges
                .iter()
                .any(|(s, e)| *s <= arr_pos && arr_pos < *e)
            {
                search_from = anchor_pos + anchor_lit.len();
                continue;
            }

            match extract_balanced_json(&text[arr_pos..]) {
                Some(consumed) => {
                    let arr_slice = &text[arr_pos..arr_pos + consumed];
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(arr_slice)
                        && let Some(items) = v.as_array()
                    {
                        let mut found_any = false;
                        for item in items {
                            if let Some(call) = parse_tool_call_value(item) {
                                tool_calls.push(call);
                                found_any = true;
                            }
                        }
                        if found_any {
                            strip_ranges.push((arr_pos, arr_pos + consumed));
                            search_from = arr_pos + consumed;
                            continue;
                        }
                    }
                    search_from = anchor_pos + anchor_lit.len();
                }
                None => {
                    search_from = anchor_pos + anchor_lit.len();
                }
            }
        }
    }

    // Pass 1.6 — `<tool_call_list>…</tool_call_list>` wrapper. Seen 2026-06-12
    // with mimo-v2.5-pro: instead of the structured `tool_calls` field the
    // model leaks the call into `content` as
    // `<tool_call_list>{"tool_call_id":"…","tool_name":"…","tool_input":{…}}</tool_call_list>`
    // (one object, several, or a JSON array inside). None of the other
    // signals match `<tool_call_list>` — `"tool_call_id"` doesn't contain the
    // literal `"tool_call"` — so without this pass the whole block ships to
    // visible content and the call never dispatches.
    if text.contains("<tool_call_list>") {
        let mut seen_blocks: Vec<(usize, usize)> = Vec::new();
        for (start, end, name, input) in extract_tool_call_list_calls(text) {
            tool_calls.push((name, input));
            if !seen_blocks.contains(&(start, end)) {
                seen_blocks.push((start, end));
                strip_ranges.push((start, end));
            }
        }
    }

    // Pass 1.6 — top-level object keyed by call ID. 2026-05-24 qwen-3.7-max-
    // preview regression where the model emits tool calls as:
    //   {"call_<24hex>": {"name":"bash", "arguments":{"command":"..."}}}
    // sometimes one per object, sometimes multiple keys in one object,
    // often wrapped in a ```json fence. The structured tool_calls path
    // never fires for these, so they leak into delta.content and Telegram
    // renders them as code blocks.
    //
    // Strategy: anchor on `"call_` preceded by `{` (i.e. the very first
    // key of a JSON object). Walk back to the `{`, balance-extract the
    // object, parse it, and for every top-level key matching `^call_`
    // run `parse_tool_call_value` on the value. Skip anchors inside
    // already-stripped ranges.
    if has_dict_by_id_signal {
        let mut search_from = 0;
        while let Some(rel) = text[search_from..].find("\"call_") {
            let anchor_pos = search_from + rel;
            // Require the call_ key to be the FIRST key of an object so
            // we don't sweep up prose like `the "call_123" field`. Walk
            // backwards past whitespace and quotes to find `{`.
            let mut back = anchor_pos;
            while back > 0 {
                let b = text.as_bytes()[back - 1];
                if b.is_ascii_whitespace() || b == b'\n' || b == b'\r' {
                    back -= 1;
                    continue;
                }
                break;
            }
            if back == 0 || text.as_bytes()[back - 1] != b'{' {
                search_from = anchor_pos + "\"call_".len();
                continue;
            }
            let obj_start = back - 1;
            // Skip if already covered by an earlier strip range.
            if strip_ranges
                .iter()
                .any(|(s, e)| *s <= obj_start && obj_start < *e)
            {
                search_from = anchor_pos + "\"call_".len();
                continue;
            }
            // Allow a small window of leading whitespace inside the
            // markdown ```json fence; the fence itself isn't part of
            // the JSON so we balance-extract starting at `{`.
            let Some(consumed) = extract_balanced_json(&text[obj_start..]) else {
                search_from = anchor_pos + "\"call_".len();
                continue;
            };
            let obj_slice = &text[obj_start..obj_start + consumed];
            let Ok(v) = serde_json::from_str::<serde_json::Value>(obj_slice) else {
                search_from = anchor_pos + "\"call_".len();
                continue;
            };
            let Some(obj) = v.as_object() else {
                search_from = anchor_pos + "\"call_".len();
                continue;
            };
            let mut found_any = false;
            for (key, val) in obj {
                if !key.starts_with("call_") {
                    continue;
                }
                if let Some(call) = parse_tool_call_value(val) {
                    tool_calls.push(call);
                    found_any = true;
                }
            }
            if found_any {
                strip_ranges.push((obj_start, obj_start + consumed));
                search_from = obj_start + consumed;
            } else {
                search_from = anchor_pos + "\"call_".len();
            }
        }
    }

    let mut i: usize = 0;
    let bytes = text.as_bytes();

    while i < bytes.len() {
        let tc_at = text[i..].find("<tool_call>").map(|r| i + r);
        let fn_at = text[i..].find("<function=").map(|r| i + r);
        let bare_at = text[i..].find("tool_call:").map(|r| i + r);
        let arr_at = text[i..].find("\"tool_calls\"").map(|r| i + r);
        // Singular `{"tool_call": {...}}` envelope. Logs 2026-04-17 03:07
        // confirmed Qwen3 emits this shape (often with the JSON colons
        // missing between key and value — "tool_call" { ... }) when the
        // template isn't fully in reasoning mode.
        let sing_at = {
            let candidate = text[i..].find("\"tool_call\"").map(|r| i + r);
            // Skip matches that are actually the plural `"tool_calls"` —
            // those are handled by arr_at with different extraction logic.
            match candidate {
                Some(p)
                    if text.as_bytes().get(p + "\"tool_call\"".len() - 1).copied()
                        != Some(b'"') =>
                {
                    None
                }
                Some(p) if text[p..].starts_with("\"tool_calls\"") => None,
                other => other,
            }
        };
        let next = [tc_at, fn_at, bare_at, arr_at, sing_at]
            .into_iter()
            .flatten()
            .min();
        let Some(start) = next else { break };

        if bare_at == Some(start) {
            // Guard: "tool_call:" must be a bare marker, not a substring inside a
            // larger identifier like "set_tool_call:true" or "my_tool_call:fn".
            // Require the preceding byte (if any) to be a word boundary.
            if start > 0 {
                let prev = text.as_bytes()[start - 1];
                let is_boundary = prev.is_ascii_whitespace()
                    || matches!(
                        prev,
                        b',' | b';' | b':' | b'[' | b'(' | b'{' | b'\n' | b'\r'
                    );
                if !is_boundary {
                    i = start + "tool_call:".len();
                    continue;
                }
            }
            let body_start = start + "tool_call:".len();
            let brace_rel = text[body_start..]
                .char_indices()
                .find(|(_, c)| !c.is_whitespace())
                .map(|(idx, _)| idx);
            let brace_abs = match brace_rel {
                Some(rel) if text.as_bytes().get(body_start + rel) == Some(&b'{') => {
                    body_start + rel
                }
                _ => {
                    // Not a JSON envelope — advance past the marker.
                    i = body_start;
                    continue;
                }
            };
            match extract_balanced_json(&text[brace_abs..]) {
                Some(consumed) => {
                    let json_slice = &text[brace_abs..brace_abs + consumed];
                    if let Some(call) = parse_qwen_tool_json(json_slice) {
                        tool_calls.push(call);
                        strip_ranges.push((start, brace_abs + consumed));
                        i = brace_abs + consumed;
                        continue;
                    }
                    // Unrecognized JSON shape — skip the bare marker but not
                    // the JSON (it may still be prose with legitimate JSON).
                    i = body_start;
                }
                None => {
                    i = body_start;
                }
            }
            continue;
        } else if arr_at == Some(start) {
            // `{"tool_calls":[...]}` envelope hallucinated as content text.
            // Find the wrapping `{`; it should be within a short window of
            // the `"tool_calls"` key (typically `{"tool_calls":` or
            // `{ "tool_calls":`). If it's further away, this is probably
            // prose like `the \"tool_calls\" field` — bail.
            let wrapper = text[..start].rfind('{');
            let wrapper_start = match wrapper {
                Some(br) if start - br <= 4 => br,
                _ => {
                    i = start + "\"tool_calls\"".len();
                    continue;
                }
            };
            match extract_balanced_json(&text[wrapper_start..]) {
                Some(consumed) => {
                    let env_slice = &text[wrapper_start..wrapper_start + consumed];
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(env_slice)
                        && let Some(arr) = v.get("tool_calls").and_then(|a| a.as_array())
                    {
                        let mut found_any = false;
                        for item in arr {
                            if let Some(call) = parse_tool_call_value(item) {
                                tool_calls.push(call);
                                found_any = true;
                            }
                        }
                        if found_any {
                            strip_ranges.push((wrapper_start, wrapper_start + consumed));
                            i = wrapper_start + consumed;
                            continue;
                        }
                    }
                    i = start + "\"tool_calls\"".len();
                }
                None => {
                    i = start + "\"tool_calls\"".len();
                }
            }
            continue;
        } else if sing_at == Some(start) {
            // `{"tool_call": {...}}` singular envelope. Wrapper `{` within
            // ~4 bytes of the key. Parse tolerantly — the model sometimes
            // drops the colon between key and value (`"tool_call" {`) so
            // strict serde_json fails. Fall back to recovering name /
            // arguments from the inner object via regex.
            let wrapper = text[..start].rfind('{');
            let wrapper_start = match wrapper {
                Some(br) if start - br <= 4 => br,
                _ => {
                    i = start + "\"tool_call\"".len();
                    continue;
                }
            };
            match extract_balanced_json_tolerant(&text[wrapper_start..]) {
                Some(consumed) => {
                    let env_slice = &text[wrapper_start..wrapper_start + consumed];
                    let recovered = recover_tool_call_from_malformed_json(env_slice);
                    if let Some(call) = recovered {
                        tool_calls.push(call);
                        strip_ranges.push((wrapper_start, wrapper_start + consumed));
                        i = wrapper_start + consumed;
                        continue;
                    }
                    i = start + "\"tool_call\"".len();
                }
                None => {
                    i = start + "\"tool_call\"".len();
                }
            }
            continue;
        } else if tc_at == Some(start) {
            // <tool_call>{json}</tool_call>? — closing tag optional
            let body_start = start + "<tool_call>".len();
            // Skip whitespace to the opening brace.
            let brace_rel = text[body_start..]
                .char_indices()
                .find(|(_, c)| !c.is_whitespace())
                .map(|(idx, _)| idx);
            let brace_abs = match brace_rel {
                Some(rel) if text.as_bytes().get(body_start + rel) == Some(&b'{') => {
                    body_start + rel
                }
                _ => {
                    // Not a JSON tool_call — advance past the tag and keep scanning.
                    i = body_start;
                    continue;
                }
            };
            match extract_balanced_json(&text[brace_abs..]) {
                Some(consumed) => {
                    let json_slice = &text[brace_abs..brace_abs + consumed];
                    if let Some(call) = parse_qwen_tool_json(json_slice) {
                        tool_calls.push(call);
                    }
                    // Include the optional `</tool_call>` (and any whitespace
                    // between `}` and it) in the strip range.
                    let mut end = brace_abs + consumed;
                    let after = &text[end..];
                    let ws_len = after.len() - after.trim_start().len();
                    if after.trim_start().starts_with("</tool_call>") {
                        end += ws_len + "</tool_call>".len();
                    }
                    strip_ranges.push((start, end));
                    i = end;
                }
                None => {
                    // Unbalanced JSON — likely a prose mention like
                    // "strip the <tool_call> tag". Don't strip; move on.
                    i = body_start;
                }
            }
        } else {
            // <function=name>...</function>? — parameters optional
            let tag_start = start;
            // Find the `>` closing the opening tag.
            let after = &text[tag_start..];
            let open_end = match after.find('>') {
                Some(r) => tag_start + r + 1,
                None => {
                    i = tag_start + "<function=".len();
                    continue;
                }
            };
            let name = text[tag_start + "<function=".len()..open_end - 1].trim();
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                i = open_end;
                continue;
            }
            // Body ends at first of: </tool_call>, next <function=, or </function>.
            // Unsloth avoids </function> as boundary because code params can contain
            // it literally — but we keep it as a last resort since our param set
            // doesn't embed Python code that closes the tag name.
            let tail = &text[open_end..];
            let candidates = [
                tail.find("</tool_call>").map(|r| (r, "</tool_call>".len())),
                tail.find("<function=").map(|r| (r, 0usize)),
                tail.find("</function>").map(|r| (r, "</function>".len())),
            ];
            let pick = candidates.iter().filter_map(|o| *o).min_by_key(|(r, _)| *r);
            let (body_rel, close_len) = match pick {
                Some(p) => p,
                None => {
                    // No boundary — take until end of string (open-ended).
                    (tail.len(), 0)
                }
            };
            let body = &tail[..body_rel];
            let input = parse_function_params(body);
            tool_calls.push((name.to_string(), input));
            let end = open_end + body_rel + close_len;
            strip_ranges.push((start, end));
            i = end;
        }
    }

    // Pass 1.7 — orphaned bash args. 2026-05-27 qwen-3.7-max-thinking on
    // dialagram emits multiple bare `{"command": "..."}` objects in
    // delta.content with no wrapper. Runs LAST so it can check the
    // accumulated `strip_ranges` and skip any `"command"` anchor that's
    // already inside a wrapper the other passes captured — otherwise a
    // nested `{"arguments":{"command":"..."}}` would synthesize a
    // duplicate.
    //
    // Anchor on `"command"` preceded by `{` (with optional whitespace),
    // balance-extract the object, verify keys are a strict subset of
    // bash's schema (`command`, `working_dir`, `timeout_secs`) — that
    // rules out prose like `{"command": "show example", "note": "x"}` —
    // and synthesize a bash tool call.
    if has_bare_command_args {
        let mut search_from = 0;
        while let Some(rel) = text[search_from..].find("\"command\"") {
            let anchor_pos = search_from + rel;
            // Walk back past whitespace to find `{`.
            let mut back = anchor_pos;
            while back > 0 {
                let b = text.as_bytes()[back - 1];
                if b.is_ascii_whitespace() || b == b'\n' || b == b'\r' {
                    back -= 1;
                    continue;
                }
                break;
            }
            if back == 0 || text.as_bytes()[back - 1] != b'{' {
                search_from = anchor_pos + "\"command\"".len();
                continue;
            }
            let obj_start = back - 1;
            // Already inside a wrapper another pass captured — skip.
            if strip_ranges
                .iter()
                .any(|(s, e)| *s <= obj_start && obj_start < *e)
            {
                search_from = anchor_pos + "\"command\"".len();
                continue;
            }
            // Nested-as-value guard: if the `{` is preceded (past
            // whitespace) by `:`, this object is the value of an outer
            // key (e.g. `"arguments": {"command": ...}` or
            // `"function": {...}`). Some outer envelope already had its
            // chance via the other passes; if it wasn't a recognised
            // shape we deliberately don't try to recover it as bare bash
            // — the existing test `openai_nested_function_name_without_
            // wrapper` locks that policy in.
            let mut prev = obj_start;
            while prev > 0 {
                let b = text.as_bytes()[prev - 1];
                if b.is_ascii_whitespace() || b == b'\n' || b == b'\r' {
                    prev -= 1;
                    continue;
                }
                break;
            }
            if prev > 0 && text.as_bytes()[prev - 1] == b':' {
                // Distinguish JSON-key colon (preceded by `"`) from
                // English-prose colon (preceded by a letter/digit). Only
                // JSON nesting should suppress synthesis — prose like
                // `"Running build: {\"command\":\"cargo\"} done."` is
                // still a legit bare-args leak.
                let mut k = prev - 1;
                while k > 0 {
                    let b = text.as_bytes()[k - 1];
                    if b.is_ascii_whitespace() {
                        k -= 1;
                        continue;
                    }
                    break;
                }
                if k > 0 && text.as_bytes()[k - 1] == b'"' {
                    search_from = anchor_pos + "\"command\"".len();
                    continue;
                }
            }
            let Some(consumed) = extract_balanced_json(&text[obj_start..]) else {
                search_from = anchor_pos + "\"command\"".len();
                continue;
            };
            let obj_slice = &text[obj_start..obj_start + consumed];
            let Ok(v) = serde_json::from_str::<serde_json::Value>(obj_slice) else {
                search_from = anchor_pos + "\"command\"".len();
                continue;
            };
            let Some(obj) = v.as_object() else {
                search_from = anchor_pos + "\"command\"".len();
                continue;
            };
            let known = ["command", "working_dir", "timeout_secs"];
            let all_known = obj.keys().all(|k| known.contains(&k.as_str()));
            let has_command_string = obj.get("command").and_then(|c| c.as_str()).is_some();
            if !all_known || !has_command_string {
                search_from = anchor_pos + "\"command\"".len();
                continue;
            }
            tool_calls.push(("bash".to_string(), v));
            strip_ranges.push((obj_start, obj_start + consumed));
            search_from = obj_start + consumed;
        }
    }

    // Pass 1.8 — Anthropic-style `<invoke name="X">` blocks, optionally
    // wrapped in `<qwen:tool_call>` or `<function_calls>`. 2026-05-27
    // qwen-3.7-max-thinking on Telegram emitted:
    //
    //   <qwen:tool_call>
    //   invoke name="read_file">       (model dropped the leading `<`)
    //   <parameter name="path">/tmp/x</parameter>
    //   <parameter name="start_line">1888</parameter>
    //   </invoke>
    //   <parameter name="read_file">   (model wrote `parameter` not `invoke`)
    //   <parameter name="path">/tmp/y</parameter>
    //   </invoke>
    //   </qwen:tool_call>
    //
    // The wrapper is not in our XML signal table and the inner
    // `<invoke name>` shape is Anthropic-original (tool name as attribute,
    // not as tag). Without this pass the whole block leaks to TUI / Telegram
    // and no calls dispatch.
    //
    // Strategy: scan for `invoke name="X"` anywhere. The leading `<` is
    // optional — qwen-thinking regularly drops it. Tool name X must be in
    // KNOWN_TOOL_NAMES to skip prose `<invoke>` mentions. Collect each
    // `<parameter name="Y">value</parameter>` inside the matching `</invoke>`,
    // parameter values coerced to int/bool when they look like one. Strip
    // ranges include any surrounding `<qwen:tool_call>` / `<function_calls>`
    // wrapper so it doesn't render as an empty tag.
    if has_invoke_signal {
        let invoke_calls = extract_invoke_style_tool_calls(text, &strip_ranges);
        for (s, e, name, args) in invoke_calls {
            tool_calls.push((name, args));
            strip_ranges.push((s, e));
        }
        // After collecting invoke ranges, opportunistically extend each to
        // swallow a surrounding `<qwen:tool_call>` / `<function_calls>` wrapper
        // when it would otherwise leave a lone empty tag pair behind.
        widen_strip_to_known_wrappers(text, &mut strip_ranges);
    }

    // Pass 1.9 — bare `{"name": "<tool>", "arguments": {...}}` objects
    // emitted inline in delta.content. Logic in
    // `bare_tool_call_extractor` — kept out of this file. MUST run
    // after every marker-based pass so anchors that fall inside a
    // `<tool_call>{...}</tool_call>` wrapper (already claimed) are
    // correctly skipped instead of stripping the inner JSON and
    // leaving an empty wrapper pair behind.
    if has_bare_name_args {
        for m in super::bare_tool_call_extractor::extract_bare_name_args_calls(
            text,
            &strip_ranges,
            &tool_calls,
        ) {
            if !m.already_in_existing {
                tool_calls.push((m.name, m.args));
            }
            strip_ranges.push((m.strip_start, m.strip_end));
        }
    }

    // Pass 1.10 — corrupted separator recovery. 2026-07-03 mimo-v2.5
    // (opencode-go): when conversing in Bengali, the model emits valid
    // tool name and valid JSON params but corrupts the separator into
    // non-ASCII garbage (Bengali script). Shape:
    // `tool_search ব্যক{"query":"..."}`. Model-agnostic — handles any
    // model producing this pattern. Must run after all marker-based
    // passes so tool names inside `<tool_call>` wrappers are already
    // claimed.
    if has_corrupted_tool_signal {
        for (start, end, name, args) in extract_corrupted_tool_calls(text, &strip_ranges) {
            tool_calls.push((name, args));
            strip_ranges.push((start, end));
        }
    }

    if strip_ranges.is_empty() {
        return (tool_calls, text.to_string());
    }

    // Sort by start so the rebuild loop sees ranges in order. The Claude-style,
    // bare-array, and main-loop passes can each add ranges out of order with
    // respect to each other.
    strip_ranges.sort_by_key(|(s, _)| *s);

    // Rebuild text with blocks removed, keeping everything outside them.
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for (s, e) in strip_ranges {
        if s >= cursor {
            if s > cursor {
                out.push_str(&text[cursor..s]);
            }
            cursor = e;
        } else if e > cursor {
            // Overlapping ranges (shouldn't happen with our passes, but be
            // defensive): extend cursor without copying duplicate text.
            cursor = e;
        }
    }
    if cursor < text.len() {
        out.push_str(&text[cursor..]);
    }
    (tool_calls, out.trim().to_string())
}

/// Tool names we recognise when the model emits Claude-style native XML
/// invocations — `<TOOLNAME><PARAM>value</PARAM></TOOLNAME>`. Keeping the
/// set explicit rather than matching any `<\w+>` pair avoids false
/// positives on prose that mentions tags (e.g. `<html>`, `<body>`,
/// `<script>`).
pub(crate) const KNOWN_TOOL_NAMES: &[&str] = &[
    "bash",
    "ls",
    "glob",
    "grep",
    "read_file",
    "write_file",
    "edit_file",
    "patch_file",
    "web_search",
    "web_fetch",
    "web_request",
    "web_scrape",
    "http_request",
    "tool_search",
    "plan",
    "task_manager",
    "cron_manage",
    "goal_manage",
    "memory_search",
    "session_search",
    "lsp",
    "agent",
    "slack_send",
    "telegram_send",
    "discord_send",
    "trello_send",
    "whatsapp_send",
    // Registered builtins that were missing from this gate (#398 follow-up):
    // a leaked call naming any of them was invisible to every text-leak
    // pass that anchors on this list (observed live with load_brain_file).
    "load_brain_file",
    "write_opencrabs_file",
    "hashline_edit",
    "task",
    "session_context",
    "http_request",
    "config_manager",
    "slash_command",
    "rename_session",
    "analyze_image",
    "analyze_video",
    "generate_image",
    "generate_document",
    "parse_document",
    "pdf_to_images",
    "code_exec",
    "notebook_edit",
    "exa_search",
    "brave_search",
    "channel_search",
    "a2a_send",
    "profile_list",
    "mission_control_report",
    "feedback_record",
    "feedback_analyze",
    "self_improve",
    "evolve",
    "tool_manage",
    "spawn_agent",
    "wait_agent",
    "send_input",
    "close_agent",
    "resume_agent",
    "cowork_connect",
    "provider_vision",
];

/// Extract `<TOOLNAME><PARAM>value</PARAM>…</TOOLNAME>` invocations. The
/// outer tag name must be one of `KNOWN_TOOL_NAMES`; the body must
/// contain at least one `<param>value</param>` pair with matching
/// Recover tool calls whose separator between the tool name and JSON
/// params was corrupted into non-ASCII garbage. 2026-07-03 mimo-v2.5
/// (opencode-go) tokenizer bug: when the user converses in Bengali,
/// the model sometimes emits `tool_search ব্যক{"query":"..."}` where
/// the tool name and JSON are correct but the gap between them is
/// Bengali script instead of a proper separator. Model-agnostic —
/// any model producing this pattern is handled.
///
/// Detection criteria (all must hold):
///   1. A `KNOWN_TOOL_NAMES` entry appears in the text.
///   2. Immediately after it, there is a gap containing non-ASCII bytes
///      AND no ASCII letters (so prose like "tool_search for {x}" is
///      never swept in).
///   3. The gap is followed by a `{` that starts valid balanced JSON
///      parsing to an object (the tool params).
///
/// Returns `(start_byte, end_byte, tool_name, args_json)` per match.
fn extract_corrupted_tool_calls(
    text: &str,
    existing_strip_ranges: &[(usize, usize)],
) -> Vec<(usize, usize, String, serde_json::Value)> {
    let mut results = Vec::new();
    for &tool in KNOWN_TOOL_NAMES {
        let mut search_from = 0;
        while let Some(rel) = text[search_from..].find(tool) {
            let abs = search_from + rel;

            // Skip if already claimed by an earlier pass.
            if existing_strip_ranges
                .iter()
                .any(|(s, e)| *s <= abs && abs < *e)
            {
                search_from = abs + tool.len();
                continue;
            }

            let after_tool = abs + tool.len();
            let rest = &text[after_tool..];

            // Find the next `{` after the tool name.
            let Some(brace_rel) = rest.find('{') else {
                search_from = after_tool;
                continue;
            };

            let gap = &rest[..brace_rel];
            // Gap must contain non-ASCII bytes (the corruption) and no
            // ASCII letters (so prose mentions are never swept in).
            let has_non_ascii = gap.bytes().any(|b| b >= 0x80);
            let has_ascii_alpha = gap.bytes().any(|b| b.is_ascii_alphabetic());
            if !has_non_ascii || has_ascii_alpha {
                search_from = after_tool;
                continue;
            }

            // Try to extract balanced JSON from the `{`.
            let brace_abs = after_tool + brace_rel;
            let Some(consumed) = extract_balanced_json(&text[brace_abs..]) else {
                search_from = brace_abs;
                continue;
            };
            let json_str = &text[brace_abs..brace_abs + consumed];
            let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) else {
                search_from = brace_abs + consumed;
                continue;
            };
            if !v.is_object() {
                search_from = brace_abs + consumed;
                continue;
            }

            let end = brace_abs + consumed;
            results.push((abs, end, tool.to_string(), v));
            search_from = end;
        }
    }
    results
}

/// open/close tag. Returns `(start, end, name, args)` byte ranges so
/// the caller can add them to `strip_ranges`.
fn extract_claude_style_tool_calls(text: &str) -> Vec<(usize, usize, String, serde_json::Value)> {
    let mut results = Vec::new();
    let mut cursor = 0;

    while cursor < text.len() {
        // Find the next `<TOOLNAME>` from our allowlist.
        let mut best: Option<(usize, &'static str)> = None;
        for &tool in KNOWN_TOOL_NAMES {
            let needle_owned = format!("<{}>", tool);
            if let Some(rel) = text[cursor..].find(&needle_owned) {
                let abs = cursor + rel;
                if best.is_none_or(|(b, _)| abs < b) {
                    best = Some((abs, tool));
                }
            }
        }
        let Some((start, tool_name)) = best else {
            break;
        };
        let open_tag_len = tool_name.len() + 2; // `<` + name + `>`
        let body_start = start + open_tag_len;

        let close_tag = format!("</{}>", tool_name);
        let Some(close_rel) = text[body_start..].find(&close_tag) else {
            // No close — advance past the open and keep scanning.
            cursor = body_start;
            continue;
        };
        let close_abs = body_start + close_rel;
        let body = &text[body_start..close_abs];

        let params = parse_xml_param_pairs(body);
        if params.is_empty() {
            cursor = close_abs + close_tag.len();
            continue;
        }

        let mut map = serde_json::Map::new();
        for (k, v) in params {
            map.insert(k, serde_json::Value::String(v));
        }
        let end = close_abs + close_tag.len();
        results.push((
            start,
            end,
            tool_name.to_string(),
            serde_json::Value::Object(map),
        ));
        cursor = end;
    }
    results
}

/// Extract `<name>value</name>` pairs from a Claude-style tool body.
/// Only accepts alphanumeric+underscore tag names so arbitrary nested
/// XML doesn't accidentally register as a parameter.
fn parse_xml_param_pairs(body: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut cursor = 0;
    while cursor < body.len() {
        // Find next `<` followed by an identifier.
        let Some(lt_rel) = body[cursor..].find('<') else {
            break;
        };
        let lt_abs = cursor + lt_rel;
        let after_lt = &body[lt_abs + 1..];
        // Identifier must be lowercase letters/digits/underscore.
        let name_len = after_lt
            .bytes()
            .take_while(|&b| b.is_ascii_alphanumeric() || b == b'_')
            .count();
        if name_len == 0 || after_lt.as_bytes().get(name_len) != Some(&b'>') {
            cursor = lt_abs + 1;
            continue;
        }
        let name = &after_lt[..name_len];
        let body_start = lt_abs + 1 + name_len + 1; // past `>`
        let close = format!("</{}>", name);
        let Some(close_rel) = body[body_start..].find(&close) else {
            break;
        };
        let value = body[body_start..body_start + close_rel].trim().to_string();
        pairs.push((name.to_string(), value));
        cursor = body_start + close_rel + close.len();
    }
    pairs
}

/// Extract Anthropic-style `<invoke name="X">…</invoke>` blocks. The tool
/// name is read from the `name="…"` attribute (or `name='…'`), parameters
/// from each nested `<parameter name="Y">value</parameter>`. The leading
/// `<` on `invoke` is OPTIONAL — qwen-thinking regularly drops it.
///
/// Tolerates the 2026-05-27 qwen-thinking malformation where the model
/// wrote `<parameter name="TOOL">` instead of `<invoke name="TOOL">`:
/// if a `<parameter>` block at the top level has its `name` matching a
/// KNOWN_TOOL_NAMES entry, treat it as an invoke. This loses the
/// parameter-name semantics for that one call (we can't recover what
/// the model meant) but at least dispatches a call instead of silently
/// dropping it.
///
/// Returns `(start_byte, end_byte, tool_name, args_json)` per match.
fn extract_invoke_style_tool_calls(
    text: &str,
    existing_strip_ranges: &[(usize, usize)],
) -> Vec<(usize, usize, String, serde_json::Value)> {
    let mut results: Vec<(usize, usize, String, serde_json::Value)> = Vec::new();
    let mut cursor: usize = 0;
    while cursor < text.len() {
        // Find next anchor — either the canonical `invoke name=` OR the
        // qwen-thinking malformation `<parameter name="KNOWN_TOOL"` at the
        // top level. Whichever appears first wins.
        let invoke_at = text[cursor..]
            .find("invoke name=")
            .map(|r| (cursor + r, "invoke name=".len()));
        let param_at = text[cursor..]
            .find("<parameter name=")
            .map(|r| (cursor + r, "<parameter name=".len()));
        let (anchor, anchor_len) = match (invoke_at, param_at) {
            (Some(i), Some(p)) => {
                if i.0 <= p.0 {
                    i
                } else {
                    p
                }
            }
            (Some(i), None) => i,
            (None, Some(p)) => p,
            (None, None) => break,
        };
        // Skip if already covered by another pass.
        if existing_strip_ranges
            .iter()
            .any(|(s, e)| *s <= anchor && anchor < *e)
        {
            cursor = anchor + anchor_len;
            continue;
        }
        // Find quote char (' or "), then name end.
        let after_eq = anchor + anchor_len;
        let bytes = text.as_bytes();
        if after_eq >= bytes.len() {
            break;
        }
        let quote = bytes[after_eq];
        if quote != b'"' && quote != b'\'' {
            cursor = after_eq;
            continue;
        }
        let name_start = after_eq + 1;
        let name_end_rel = match text[name_start..].find(quote as char) {
            Some(r) => r,
            None => {
                cursor = name_start;
                continue;
            }
        };
        let name = text[name_start..name_start + name_end_rel].to_string();
        // Tool name must match a KNOWN_TOOL_NAMES entry — strict guard against
        // prose like `<invoke name="something">` or legitimate
        // `<parameter name="some_arg">` blocks (those are nested params, not
        // tool invocations, and are consumed by parse_invoke_parameters).
        if !KNOWN_TOOL_NAMES.iter().any(|&n| n == name) {
            cursor = name_start + name_end_rel;
            continue;
        }
        // Find the matching `</invoke>`. If absent, take everything up to
        // the wrapper close (`</qwen:tool_call>`, `</function_calls>`) or
        // end of text — best-effort recovery.
        let body_search_start = name_start + name_end_rel;
        let close = text[body_search_start..]
            .find("</invoke>")
            .map(|r| (body_search_start + r, "</invoke>".len()));
        let (body_end, close_len) = close.unwrap_or_else(|| {
            // Wrapper-close fallback: stop at the next known wrapper close.
            let qwen_close = text[body_search_start..].find("</qwen:tool_call>");
            let func_close = text[body_search_start..].find("</function_calls>");
            let stop_rel = [qwen_close, func_close]
                .into_iter()
                .flatten()
                .min()
                .unwrap_or(text.len() - body_search_start);
            (body_search_start + stop_rel, 0)
        });

        let body = &text[body_search_start..body_end];
        let params = parse_invoke_parameters(body);
        let mut obj = serde_json::Map::new();
        for (k, v) in params {
            obj.insert(k, coerce_xml_param_value(&v));
        }
        // Find the original start of this invoke block. Walk back from
        // `anchor` past whitespace; if the preceding char is `<`, include it.
        let mut block_start = anchor;
        if block_start > 0 && text.as_bytes()[block_start - 1] == b'<' {
            block_start -= 1;
        }
        let block_end = body_end + close_len;
        results.push((block_start, block_end, name, serde_json::Value::Object(obj)));
        cursor = block_end;
    }
    results
}

/// Parse `<parameter name="K">V</parameter>` pairs from an invoke body.
/// Tolerates the qwen-thinking malformation where some inner blocks are
/// themselves `<parameter name="KNOWN_TOOL">…</invoke>` — those are skipped
/// here (they get re-scanned by the outer `invoke name=` search loop on
/// the next iteration).
fn parse_invoke_parameters(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut cursor = 0;
    while cursor < body.len() {
        let rel = match body[cursor..].find("<parameter name=") {
            Some(r) => r,
            None => break,
        };
        let abs = cursor + rel;
        let after = abs + "<parameter name=".len();
        let bytes = body.as_bytes();
        if after >= bytes.len() {
            break;
        }
        let quote = bytes[after];
        if quote != b'"' && quote != b'\'' {
            cursor = after;
            continue;
        }
        let name_start = after + 1;
        let name_end_rel = match body[name_start..].find(quote as char) {
            Some(r) => r,
            None => break,
        };
        let key = body[name_start..name_start + name_end_rel].to_string();
        // After the closing quote, expect `>`. Skip any leading whitespace
        // before the value.
        let after_name = name_start + name_end_rel + 1;
        let value_start = match body[after_name..].find('>') {
            Some(r) => after_name + r + 1,
            None => break,
        };
        let close = "</parameter>";
        let close_rel = match body[value_start..].find(close) {
            Some(r) => r,
            None => break,
        };
        let value = body[value_start..value_start + close_rel]
            .trim()
            .to_string();
        // Skip the qwen malformation: `<parameter name="read_file">` at top
        // level is the model writing `parameter` where it meant `invoke`.
        // Caller's extractor re-scans for `invoke name=` so we just don't
        // record it as a parameter.
        if !KNOWN_TOOL_NAMES.iter().any(|&n| n == key) {
            out.push((key, value));
        }
        cursor = value_start + close_rel + close.len();
    }
    out
}

/// Coerce an XML parameter string value into the most likely JSON type.
/// Numbers (`1888`), booleans (`true`/`false`/`yes`/`no`), and everything
/// else as string. Keeps strings as strings for path-like or
/// command-like values.
fn coerce_xml_param_value(raw: &str) -> serde_json::Value {
    let trimmed = raw.trim();
    if let Ok(n) = trimmed.parse::<i64>() {
        return serde_json::Value::from(n);
    }
    if let Ok(n) = trimmed.parse::<f64>() {
        // Reject NaN / inf to keep the JSON valid.
        if n.is_finite() {
            return serde_json::json!(n);
        }
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "true" | "yes" => return serde_json::Value::Bool(true),
        "false" | "no" => return serde_json::Value::Bool(false),
        _ => {}
    }
    serde_json::Value::String(trimmed.to_string())
}

/// After Pass 1.8 records every `<invoke>` block as a strip range, scan
/// the whole text for `<qwen:tool_call>…</qwen:tool_call>` or
/// `<function_calls>…</function_calls>` wrappers that CONTAIN at least
/// one already-stripped invoke. Add the wrapper's outer bounds as a new
/// strip range so the wrapper doesn't render as an empty tag pair.
///
/// Scanning the whole text (instead of per-range backward/forward) is
/// necessary because multiple invokes commonly share one wrapper — a
/// per-range widening leaves the inter-invoke whitespace AND the
/// outer wrapper visible.
fn widen_strip_to_known_wrappers(text: &str, strip_ranges: &mut Vec<(usize, usize)>) {
    const WRAPPERS: &[(&str, &str)] = &[
        ("<qwen:tool_call>", "</qwen:tool_call>"),
        ("<function_calls>", "</function_calls>"),
    ];
    let mut additions: Vec<(usize, usize)> = Vec::new();
    for (open, close) in WRAPPERS {
        let mut search_from = 0;
        while let Some(rel) = text[search_from..].find(open) {
            let open_pos = search_from + rel;
            let after_open = open_pos + open.len();
            let close_rel = match text[after_open..].find(close) {
                Some(r) => r,
                None => break,
            };
            let close_end = after_open + close_rel + close.len();
            // Does this wrapper contain any already-stripped invoke?
            let contains_invoke = strip_ranges
                .iter()
                .any(|(s, _)| open_pos <= *s && *s < close_end);
            if contains_invoke {
                additions.push((open_pos, close_end));
            }
            search_from = close_end;
        }
    }
    strip_ranges.extend(additions);
}

/// Consume a balanced JSON object starting at `s[0] == '{'`. Returns the byte
/// length through the matching closing `}`, or `None` if unbalanced. Tracks
/// string + escape state so braces inside argument strings don't confuse the
/// depth counter.
pub(crate) fn extract_balanced_json(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let (open, close) = match bytes.first()? {
        b'{' => (b'{', b'}'),
        b'[' => (b'[', b']'),
        _ => return None,
    };
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
        if b == b'"' {
            in_string = true;
        } else if b == open {
            depth += 1;
        } else if b == close {
            depth -= 1;
            if depth == 0 {
                return Some(idx + 1);
            }
        }
    }
    None
}

/// Parse the JSON inside a `<tool_call>{...}</tool_call>` block or a bare
/// `tool_call:{...}` envelope. Accepts every shape we've seen in the wild:
/// Qwen's canonical `{"name":"x","arguments":{...}}`, MiniMax's
/// `tool_name`/`args`, the OpenAI envelope
/// `{"id":"...","type":"function","function":{"name":"...","arguments":{...}}}`,
/// and stringified arguments (`"arguments": "{\"k\":1}"`).
fn parse_qwen_tool_json(json: &str) -> Option<(String, serde_json::Value)> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    parse_tool_call_value(&v)
}

/// Extract calls wrapped in `<tool_call_list>…</tool_call_list>` (mimo-v2.5-pro
/// leaks them into `content`). The block holds one or more tool-call JSON
/// objects — a single object, newline-separated objects, or a JSON array —
/// using the `{"tool_name":"…","tool_input":{…}}` schema (also accepts the
/// `name`/`arguments` shapes via `parse_tool_call_value`). The closing tag is
/// optional. Returns `(block_start, block_end, name, input)` per parsed call;
/// callers strip `block_start..block_end` from the visible text.
fn extract_tool_call_list_calls(text: &str) -> Vec<(usize, usize, String, serde_json::Value)> {
    const OPEN: &str = "<tool_call_list>";
    const CLOSE: &str = "</tool_call_list>";
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(rel) = text[i..].find(OPEN) {
        let block_start = i + rel;
        let inner_start = block_start + OPEN.len();
        // End at the closing tag if present, else the next opener, else EOT.
        let (inner_end, block_end) = match text[inner_start..].find(CLOSE) {
            Some(r) => (inner_start + r, inner_start + r + CLOSE.len()),
            None => {
                let next = text[inner_start..]
                    .find(OPEN)
                    .map(|r| inner_start + r)
                    .unwrap_or(text.len());
                (next, next)
            }
        };
        let inner = &text[inner_start..inner_end];
        // Walk the inner text for balanced JSON objects/arrays and parse each.
        let mut j = 0;
        while j < inner.len() {
            let b = inner.as_bytes()[j];
            if (b == b'{' || b == b'[')
                && let Some(consumed) = extract_balanced_json(&inner[j..])
            {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&inner[j..j + consumed]) {
                    match &v {
                        serde_json::Value::Array(items) => {
                            for item in items {
                                if let Some(call) = parse_tool_call_value(item) {
                                    out.push((block_start, block_end, call.0, call.1));
                                }
                            }
                        }
                        _ => {
                            if let Some(call) = parse_tool_call_value(&v) {
                                out.push((block_start, block_end, call.0, call.1));
                            }
                        }
                    }
                }
                j += consumed.max(1);
                continue;
            }
            j += 1;
        }
        i = block_end.max(inner_start);
    }
    out
}

/// Value-level parser shared by the JSON-string path and the
/// `{"tool_calls":[{...}, ...]}` multi-envelope path.
fn parse_tool_call_value(v: &serde_json::Value) -> Option<(String, serde_json::Value)> {
    // Name can live at the top level OR nested under `function` (OpenAI shape).
    let name = v
        .get("name")
        .and_then(|n| n.as_str())
        .or_else(|| v.get("tool_name").and_then(|n| n.as_str()))
        .or_else(|| {
            v.get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
        })
        // Fallback: `function` itself might be a string (legacy OpenAI
        // function-call format: `{"function": "bash", ...}`).
        .or_else(|| {
            v.get("function")
                .and_then(|f| if f.is_string() { f.as_str() } else { None })
        })?
        .to_string();
    if name.is_empty() {
        return None;
    }
    // Arguments follow the same pattern — may be nested under `function`
    // for the OpenAI envelope shape.
    let args_val = v
        .get("arguments")
        .or_else(|| v.get("args"))
        .or_else(|| v.get("input"))
        .or_else(|| v.get("tool_input"))
        .or_else(|| v.get("parameters"))
        .or_else(|| v.get("function").and_then(|f| f.get("arguments")))
        .or_else(|| v.get("function").and_then(|f| f.get("parameters")));
    let input = match args_val {
        Some(serde_json::Value::String(s)) => {
            serde_json::from_str(s).unwrap_or(serde_json::json!({}))
        }
        Some(other) => other.clone(),
        None => serde_json::json!({}),
    };
    Some((name, input))
}

/// Balanced-brace walker that tolerates tokens where the model dropped
/// structural characters (colons between key and value). Consumes as much
/// as needed to find the matching closing `}`. Unlike `extract_balanced_json`
/// this one is called only when we know the content is a malformed envelope
/// emitted by a local model — the caller then runs `recover_tool_call_from_malformed_json`
/// to extract name + arguments via regex rather than serde_json.
fn extract_balanced_json_tolerant(s: &str) -> Option<usize> {
    // Behaviour is identical to the strict version — the tolerance lives in
    // the subsequent parser, not in brace matching.
    extract_balanced_json(s)
}

/// Recover a tool call from a `{"tool_call": {"name": "...", "arguments": {...}}}`
/// envelope even when the model mangled the JSON. Seen in the wild:
///
///   `{"tool_call" {"name" "bash", "arguments" {"command" "..."}}}`
///
/// (note the missing colons after the keys). Strict serde_json refuses these,
/// so we walk via regex: find `"name"` followed by a string literal, find
/// `"command"` / `"arguments"` similarly, and stitch the result into a clean
/// `(name, input)` tuple.
fn recover_tool_call_from_malformed_json(env: &str) -> Option<(String, serde_json::Value)> {
    // First try strict parsing — cheap path for well-formed envelopes.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(env) {
        let inner = v
            .get("tool_call")
            .or_else(|| v.get("function"))
            .cloned()
            .unwrap_or(v);
        if let Some(call) = parse_tool_call_value(&inner) {
            return Some(call);
        }
    }

    // Fallback: regex extract name + common arg fields. Keep the regex
    // simple — we only handle the primitive value types local models
    // commonly emit (strings, numbers, booleans). Nested object arguments
    // need the strict path above.
    use regex::Regex;
    use std::sync::LazyLock;
    static NAME_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#""name"\s*:?\s*"([^"]+)""#).unwrap());
    // Capture every `"key" <optional-colon> <value>` pair inside the
    // arguments block. Values can be strings, numbers, or booleans.
    static ARG_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#""([a-zA-Z_][a-zA-Z0-9_]*)"\s*:?\s*("([^"\\]|\\.)*"|true|false|-?\d+(\.\d+)?)"#,
        )
        .unwrap()
    });
    let name_cap = NAME_RE.captures(env)?;
    let name = name_cap.get(1)?.as_str().to_string();
    if name.is_empty() {
        return None;
    }
    // Skip the `"name"` match itself when collecting args.
    let name_end = name_cap.get(0)?.end();
    let args_region = &env[name_end..];
    let mut map = serde_json::Map::new();
    for cap in ARG_RE.captures_iter(args_region) {
        let key = cap.get(1).map(|m| m.as_str().to_string());
        let raw = cap.get(2).map(|m| m.as_str().to_string());
        if let (Some(k), Some(r)) = (key, raw) {
            // Reserved keys we don't want to treat as arguments.
            if matches!(
                k.as_str(),
                "name" | "tool_call" | "type" | "id" | "function"
            ) {
                continue;
            }
            let val = if let Some(stripped) = r.strip_prefix('"').and_then(|s| s.strip_suffix('"'))
            {
                serde_json::Value::String(stripped.replace("\\\"", "\""))
            } else if r == "true" {
                serde_json::Value::Bool(true)
            } else if r == "false" {
                serde_json::Value::Bool(false)
            } else if let Ok(n) = r.parse::<i64>() {
                serde_json::Value::Number(n.into())
            } else if let Ok(f) = r.parse::<f64>()
                && let Some(n) = serde_json::Number::from_f64(f)
            {
                serde_json::Value::Number(n)
            } else {
                continue;
            };
            map.insert(k, val);
        }
    }
    Some((name, serde_json::Value::Object(map)))
}

/// Parse `<parameter=key>value</parameter>` pairs out of a function body.
/// When the body contains none, returns an empty object — the caller may
/// still have a valid tool call that just takes no arguments.
fn parse_function_params(body: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    let mut i = 0usize;
    while let Some(rel) = body[i..].find("<parameter=") {
        let tag_start = i + rel;
        let after = &body[tag_start..];
        let Some(gt) = after.find('>') else { break };
        let key = body[tag_start + "<parameter=".len()..tag_start + gt].trim();
        if key.is_empty() {
            i = tag_start + gt + 1;
            continue;
        }
        let val_start = tag_start + gt + 1;
        let tail = &body[val_start..];
        // Value ends at next `<parameter=` or `</parameter>` (whichever comes first).
        let end_at_param = tail.find("</parameter>");
        let end_at_next = tail.find("<parameter=");
        let end = match (end_at_param, end_at_next) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let (val, next_i) = match end {
            Some(rel) => {
                // If we ended on </parameter>, skip past it; if on the next
                // opening tag, leave cursor on it.
                let skip = if end_at_param == Some(rel) {
                    rel + "</parameter>".len()
                } else {
                    rel
                };
                (tail[..rel].trim().to_string(), val_start + skip)
            }
            None => (tail.trim().to_string(), body.len()),
        };
        map.insert(key.to_string(), serde_json::Value::String(val));
        i = next_i;
    }
    serde_json::Value::Object(map)
}

/// Dynamic token provider — called on every request to get the current bearer token.
/// Used by Copilot provider where the token rotates every ~30 minutes.
pub type TokenFn = Arc<dyn Fn() -> String + Send + Sync>;

/// Optional request-body transform hook — called just before each outgoing
/// chat-completions request is serialized. Lets a provider mutate the JSON
/// body to match a vendor-specific dialect (e.g. qwen-cli's content-array
/// shape + metadata fields) without polluting the generic OpenAI path.
pub type BodyTransformFn = Arc<dyn Fn(serde_json::Value) -> serde_json::Value + Send + Sync>;

/// Optional dynamic base-URL provider. When set, `send_url()` will call
/// this on every request instead of using the stored `base_url`. Used by
/// qwen where `resource_url` can change mid-session after a token refresh.
pub type BaseUrlFn = Arc<dyn Fn() -> String + Send + Sync>;

/// Optional async auth-refresh hook called after a 401/403 response. The
/// hook is expected to refresh the bearer token (so the next `token_fn()`
/// call returns a new one) and return `Ok(())` on success. The provider
/// then retries the failed request exactly once.
pub type AuthRefreshFn = Arc<
    dyn Fn() -> std::pin::Pin<
            Box<dyn std::future::Future<Output = std::result::Result<(), String>> + Send>,
        > + Send
        + Sync,
>;

pub type AuthInvalidateFn = Arc<dyn Fn() + Send + Sync>;

/// OpenAI provider for GPT models
#[derive(Clone)]
pub struct OpenAIProvider {
    api_key: String,
    base_url: String,
    client: Client,
    /// Client for `stream()`: no total wall-clock ceiling, so a healthy long
    /// stream is never cut (#1687).
    stream_client: Client,
    custom_default_model: Option<String>,
    name: String,
    /// When set, swap to this model for requests containing images.
    vision_model: Option<String>,
    /// Extra headers injected into every request (e.g. GitHub Copilot API versioning).
    pub(crate) extra_headers: Vec<(String, String)>,
    /// Configured context window size (overrides model-name heuristics).
    configured_context_window: Option<u32>,
    /// Models advertised by this provider (e.g. live-fetched from `/v1/models`
    /// at /models save time). When non-empty, overrides the hardcoded GPT
    /// fallback in `supported_models()`. Without this, custom routers like
    /// dialagram (which expose qwen, kimi, etc.) hit the supported-model
    /// mismatch path in helpers.rs and either log a noisy no-op remap or
    /// route to a wrong model when default_model differs from the selection.
    configured_models: Vec<String>,
    /// Optional dynamic token provider (overrides api_key when set).
    token_fn: Option<TokenFn>,
    /// Proactive rate limiter — shared via Arc so all clones throttle together.
    /// Used for OpenRouter `:free` models (~3s between requests).
    rate_limiter: Option<Arc<RateLimiter>>,
    /// Optional body-transform hook applied to the serialized request body
    /// right before it is sent. Used by providers (e.g. qwen) that need a
    /// vendor-specific dialect on top of the standard OpenAI shape.
    body_transform: Option<BodyTransformFn>,
    /// Optional dynamic base-URL provider. When set, each outgoing request
    /// uses the value returned by this callback instead of `base_url`.
    base_url_fn: Option<BaseUrlFn>,
    /// Optional async auth-refresh hook. When set, a 401/403 response
    /// triggers a single retry after calling this hook.
    auth_refresh_fn: Option<AuthRefreshFn>,
    /// Optional auth-invalidate hook. Called when the refresh hook succeeds
    /// but the retried request STILL gets 401/403 — meaning the refreshed
    /// token is also dead. The callback clears credentials so re-auth is
    /// triggered on next startup.
    auth_invalidate_fn: Option<AuthInvalidateFn>,
    /// When set, overrides the automatic retry config selection in
    /// `retry_config()`. Used by `RotatingQwenProvider` to disable
    /// retry-on-rate-limit for sub-providers (rotation handles 429).
    retry_config_override: Option<crate::utils::retry::RetryConfig>,
    /// OpenRouter response caching — when true, adds `X-OpenRouter-Cache: true`.
    cache_enabled: bool,
    /// OpenRouter cache TTL in seconds (1-86400, default 300).
    cache_ttl: Option<u32>,
    /// Which route this provider speaks (#2059). `Chat` (the default here, and
    /// for every OpenAI-compatible endpoint) posts to
    /// `{base}/chat/completions`; `Responses` posts to `{base}/responses`;
    /// `Auto` starts on Chat and only moves to Responses when the endpoint
    /// refuses the model there. `configure_openai_compatible` resolves the
    /// configured `wire_dialect` into this field.
    wire_dialect: WireDialect,
    /// Kimi reasoning setting (e.g. `max`, `on`, `off`). Applied to each
    /// request when the active model accepts it; see `kimi_reasoning`.
    reasoning_setting: Option<String>,
    /// Configured `enable_thinking`. Only consumed on DashScope targets, where
    /// `qwen_reasoning` decides per family whether the switch is the knob the
    /// model reads. Local servers take thinking through `chat_template_kwargs`
    /// instead and never reach this field.
    enable_thinking_setting: Option<bool>,
    /// Buffer of `(attempt, max, reason)` retry notices recorded during the
    /// most recent stream/complete call. Drained by the agent service via
    /// `take_retry_notices()` to surface "⏳ Retry N/M" to the user. Shared
    /// via Arc so clones of this provider (and the FallbackProvider that
    /// wraps it) read the same buffer.
    retry_notices: Arc<std::sync::Mutex<Vec<(u32, u32, String)>>>,
    /// Configured HTTP client request timeout.
    request_timeout: Option<Duration>,
    /// Configured inter-chunk streaming inactivity timeout.
    stream_idle_timeout: Option<Duration>,
    /// Resolved thinking-loop guard ceiling in seconds (#1690). `Some(0)`
    /// disables the guard for this provider, so this is a plain `u64` in
    /// seconds rather than a `Duration`.
    thinking_loop_timeout: Option<u64>,
}

/// Pick the retry config for one request from the facts that decide it.
///
/// Split out of the provider's `retry_config` method so the selection rules
/// can be pinned without widening that method's visibility: the decision is a
/// pure function of the override, the provider name, whether the host is
/// OpenRouter, and the model id.
pub(crate) fn select_retry_config(
    override_cfg: Option<&crate::utils::retry::RetryConfig>,
    provider_name: &str,
    is_openrouter: bool,
    model: &str,
) -> crate::utils::retry::RetryConfig {
    if let Some(ovr) = override_cfg {
        return ovr.clone();
    }
    // Qwen, OpenRouter, and :free models: retry on 429 with backoff.
    // OpenRouter upstream providers often have tight per-minute windows that
    // reopen within seconds, so bailing to fallback on the first 429 is
    // wasteful when a 4-attempt backoff would get through.
    if provider_name == "qwen" || is_openrouter || model.ends_with(":free") {
        crate::utils::retry::RetryConfig::qwen_cli_match()
    } else {
        crate::utils::retry::RetryConfig::default()
    }
}

impl OpenAIProvider {
    /// Returns true if this provider targets OpenRouter (detected by base_url).
    fn is_openrouter(&self) -> bool {
        self.base_url.to_lowercase().contains("openrouter")
    }

    /// Pick a retry config tuned for this (provider, model) pair.
    ///
    /// - Qwen OAuth matches qwen-cli's DEFAULT_RETRY_OPTIONS (retry 429s
    ///   in-place, Retry-After honored) because its shared upstream window
    ///   closes briefly after 2–3 tool calls then reopens within seconds —
    ///   falling back on the first 429 drops the session into the fallback
    ///   chain unnecessarily.
    /// - OpenRouter `:free` models get the same treatment: the 20 req/min
    ///   window is shared across the key and reopens quickly, and the
    ///   proactive rate_limiter already paces requests to ~15/min, so any
    ///   429 that does leak through is almost certainly a transient window
    ///   flip rather than a true quota exhaustion. Retrying in-place keeps
    ///   the user on the free tier instead of silently burning paid credits
    ///   on the fallback chain.
    /// - All other providers keep the default (bail-to-fallback on 429).
    fn retry_config(&self, model: &str) -> crate::utils::retry::RetryConfig {
        select_retry_config(
            self.retry_config_override.as_ref(),
            &self.name,
            self.is_openrouter(),
            model,
        )
    }

    /// Create a new OpenAI provider with official API
    pub fn new(api_key: String) -> Self {
        let client = build_http_client(DEFAULT_TIMEOUT);
        let stream_client = build_stream_http_client();

        Self {
            api_key,
            base_url: DEFAULT_OPENAI_API_URL.to_string(),
            client,
            stream_client,
            custom_default_model: None,
            name: "openai".to_string(),
            wire_dialect: WireDialect::Chat,
            vision_model: None,
            extra_headers: vec![],
            configured_context_window: None,
            configured_models: Vec::new(),
            token_fn: None,
            rate_limiter: None,
            body_transform: None,
            base_url_fn: None,
            auth_refresh_fn: None,
            auth_invalidate_fn: None,
            retry_config_override: None,
            cache_enabled: false,
            cache_ttl: None,
            reasoning_setting: None,
            enable_thinking_setting: None,
            retry_notices: Arc::new(std::sync::Mutex::new(Vec::new())),
            request_timeout: None,
            stream_idle_timeout: None,
            thinking_loop_timeout: None,
        }
    }

    /// Create provider for local LLM (LM Studio, Ollama, etc.)
    pub fn local(base_url: String) -> Self {
        let client = build_http_client(DEFAULT_TIMEOUT);
        let stream_client = build_stream_http_client();

        Self {
            api_key: "not-needed".to_string(),
            wire_dialect: WireDialect::Chat,
            base_url,
            client,
            stream_client,
            custom_default_model: None,
            name: "openai-compatible".to_string(),
            vision_model: None,
            extra_headers: vec![],
            configured_context_window: None,
            configured_models: Vec::new(),
            token_fn: None,
            rate_limiter: None,
            body_transform: None,
            base_url_fn: None,
            auth_refresh_fn: None,
            auth_invalidate_fn: None,
            retry_config_override: None,
            cache_enabled: false,
            cache_ttl: None,
            reasoning_setting: None,
            enable_thinking_setting: None,
            retry_notices: Arc::new(std::sync::Mutex::new(Vec::new())),
            request_timeout: None,
            stream_idle_timeout: None,
            thinking_loop_timeout: None,
        }
    }

    /// Create with custom base URL
    pub fn with_base_url(api_key: String, base_url: String) -> Self {
        let client = build_http_client(DEFAULT_TIMEOUT);
        let stream_client = build_stream_http_client();

        Self {
            api_key,
            base_url,
            client,
            wire_dialect: WireDialect::Chat,
            stream_client,
            custom_default_model: None,
            name: "openai-compatible".to_string(),
            vision_model: None,
            extra_headers: vec![],
            configured_context_window: None,
            configured_models: Vec::new(),
            token_fn: None,
            rate_limiter: None,
            body_transform: None,
            base_url_fn: None,
            auth_refresh_fn: None,
            auth_invalidate_fn: None,
            retry_config_override: None,
            cache_enabled: false,
            cache_ttl: None,
            reasoning_setting: None,
            enable_thinking_setting: None,
            retry_notices: Arc::new(std::sync::Mutex::new(Vec::new())),
            request_timeout: None,
            stream_idle_timeout: None,
            thinking_loop_timeout: None,
        }
    }

    /// Add extra headers to every request (e.g. API versioning).
    pub fn with_extra_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.extra_headers = headers;
        self
    }

    /// Set a configured context window size that overrides model-name heuristics.
    pub fn with_context_window(mut self, size: u32) -> Self {
        self.configured_context_window = Some(size);
        self
    }

    /// Set the wire dialect (#2059). `configure_openai_compatible` calls this
    /// with the value resolved from the provider section's `wire_dialect` and
    /// the provider's own base URL. Anything that builds a provider directly
    /// keeps the struct default, `Chat`.
    pub fn with_wire_dialect(mut self, dialect: WireDialect) -> Self {
        self.wire_dialect = dialect;
        self
    }

    /// The route this provider speaks (#2059), resolved once at construction.
    pub fn wire_dialect(&self) -> WireDialect {
        self.wire_dialect
    }

    /// Set the list of models this provider advertises. Populated from the
    /// custom provider config's `models = [...]` field, which the /models
    /// dialog writes after a live `/v1/models` fetch.
    pub fn with_models(mut self, models: Vec<String>) -> Self {
        self.configured_models = models;
        self
    }

    /// Set provider name (for logging)
    pub fn with_name(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }

    /// Set custom default model (useful for local LLMs with specific model names)
    pub fn with_default_model(mut self, model: String) -> Self {
        self.custom_default_model = Some(model);
        self
    }

    /// Set the Kimi reasoning setting (e.g. `max`, `on`, `off`). Applied per
    /// request only when the active model accepts it; see `kimi_reasoning`.
    pub fn with_reasoning(mut self, setting: String) -> Self {
        self.reasoning_setting = Some(setting);
        self
    }

    /// Set the configured `enable_thinking`. Consumed per request by
    /// `qwen_reasoning` on DashScope targets only, which decides whether this
    /// family reads the switch at all.
    pub fn with_enable_thinking(mut self, enable: bool) -> Self {
        self.enable_thinking_setting = Some(enable);
        self
    }

    /// Set a dynamic token provider (overrides static api_key in headers).
    /// Used by Copilot where the bearer token rotates every ~30 minutes.
    pub fn with_token_fn(mut self, f: TokenFn) -> Self {
        self.token_fn = Some(f);
        self
    }

    /// Set vision model — used by the `analyze_image` tool as a provider-native
    /// vision backend when Gemini vision isn't configured.
    pub fn with_vision_model(mut self, model: String) -> Self {
        self.vision_model = Some(model);
        self
    }

    /// Set a proactive rate limiter — enforces minimum interval between API
    /// calls to stay under provider rate limits (e.g. OpenRouter :free at 20/min).
    /// Takes an `Arc<RateLimiter>` so multiple provider instances share ONE limiter.
    pub fn with_rate_limiter(mut self, limiter: Arc<RateLimiter>) -> Self {
        self.rate_limiter = Some(limiter);
        self
    }

    /// Install a body-transform hook. The hook receives the fully-serialized
    /// JSON body just before it is sent and returns a (possibly modified)
    /// JSON value that will be serialized in its place. Used by providers
    /// that need a vendor-specific dialect on top of the OpenAI shape.
    pub fn with_body_transform(mut self, f: BodyTransformFn) -> Self {
        self.body_transform = Some(f);
        self
    }

    /// Install a dynamic base-URL provider. When set, every outgoing request
    /// resolves its URL via this callback instead of the stored `base_url`.
    /// Used by qwen where `resource_url` can change after a token refresh.
    pub fn with_base_url_fn(mut self, f: BaseUrlFn) -> Self {
        self.base_url_fn = Some(f);
        self
    }

    /// Install an async auth-refresh hook. On a 401/403 response the
    /// provider will call this once, wait for it to resolve, and then
    /// retry the failed request exactly once with the refreshed token.
    pub fn with_auth_refresh_fn(mut self, f: AuthRefreshFn) -> Self {
        self.auth_refresh_fn = Some(f);
        self
    }

    /// Install an auth-invalidate hook. Called when a refreshed token is
    /// *still* rejected (401/403 after retry), meaning the OAuth credentials
    /// are dead and must be cleared so re-auth is triggered.
    pub fn with_auth_invalidate_fn(mut self, f: AuthInvalidateFn) -> Self {
        self.auth_invalidate_fn = Some(f);
        self
    }

    /// Override the automatic retry config selection. Used inside
    /// `RotatingQwenProvider` to disable retry-on-rate-limit so 429s
    /// rotate immediately instead of burning ~45s in backoff per account.
    pub fn with_retry_config(mut self, config: crate::utils::retry::RetryConfig) -> Self {
        self.retry_config_override = Some(config);
        self
    }

    /// Enable OpenRouter response caching (`X-OpenRouter-Cache: true`).
    pub fn with_cache_enabled(mut self, enabled: bool) -> Self {
        self.cache_enabled = enabled;
        self
    }

    /// Set OpenRouter cache TTL in seconds (1-86400, default 300).
    pub fn with_cache_ttl(mut self, ttl: u32) -> Self {
        self.cache_ttl = Some(ttl);
        self
    }

    /// Set the NON-STREAMING request ceiling and rebuild that client.
    /// Streams are unaffected: they run on `stream_client`, which has no total
    /// timeout by construction (#1687).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.client = build_http_client(timeout);
        self.request_timeout = Some(timeout);
        self
    }

    /// Set inter-chunk streaming inactivity timeout.
    pub fn with_stream_idle_timeout(mut self, timeout: Duration) -> Self {
        self.stream_idle_timeout = Some(timeout);
        self
    }

    /// Set the resolved thinking-loop guard ceiling in seconds (#1690).
    ///
    /// Unlike the transport setters above, `0` is a value: it disables the
    /// guard for this provider. The factory resolves
    /// `[providers.<name>]` → `[agent]` and always calls this, so a provider
    /// built from config never falls back to the `[agent]` read in
    /// `brain/agent/service/helpers.rs`.
    pub fn with_thinking_loop_timeout(mut self, secs: u64) -> Self {
        self.thinking_loop_timeout = Some(secs);
        self
    }

    /// Serialize a request body to JSON, applying the optional body_transform
    /// hook. Returns a `serde_json::Value` ready to pass to `.json(&value)`.
    fn encode_body<T: Serialize>(&self, body: &T) -> Result<serde_json::Value> {
        let mut v = serde_json::to_value(body)?;
        if let Some(ref f) = self.body_transform {
            v = f(v);
        }
        Ok(v)
    }

    /// Resolve the URL to POST this request to. Uses `base_url_fn` when set
    /// (for providers whose endpoint can change mid-session), otherwise
    /// returns the stored `base_url`.
    fn send_url(&self) -> String {
        if let Some(ref f) = self.base_url_fn {
            let u = f();
            if !u.is_empty() {
                return u;
            }
        }
        self.base_url.clone()
    }

    /// The Responses endpoint for this provider, when its configured URL has
    /// a shape this client can translate. `None` means the endpoint was left
    /// unrecognised, so the caller must stay on Chat rather than guess a URL
    /// (#2059).
    fn responses_endpoint(&self) -> Option<String> {
        responses_url(&self.send_url())
    }

    /// True when the resolved dialect is an outright Responses route.
    fn dialect_is_responses(&self) -> bool {
        self.wire_dialect == WireDialect::Responses
    }

    /// Error raised when `wire_dialect=responses` was configured but the
    /// base URL has no shape this client knows how to translate.
    fn responses_endpoint_error(&self) -> ProviderError {
        ProviderError::InvalidRequest(format!(
            "wire_dialect=responses needs a recognisable chat completions base_url, got '{}'",
            self.send_url()
        ))
    }

    /// Non-streaming POST to the Responses endpoint (#2059).
    ///
    /// Mirrors the chat path's retry/notice behaviour so a 429 or 5xx on a
    /// native Responses route retries exactly like it does on Chat.
    async fn complete_responses(&self, request: LLMRequest) -> Result<LLMResponse> {
        let endpoint = self
            .responses_endpoint()
            .ok_or_else(|| self.responses_endpoint_error())?;
        let requested_model = request.model.clone();
        let session = request.session_id;
        let body = super::responses_request::build_responses_request(
            request,
            self.reasoning_setting.as_deref(),
        );
        let retry_config = self.retry_config(&requested_model);
        let notices = self.retry_notices.clone();
        let pname = self.name().to_string();
        let notice_model = requested_model.clone();
        let response = crate::utils::retry::retry_with_notify(
            || async {
                let encoded = self.encode_body(&body)?;
                let response = self
                    .send_bounded(
                        &self.client,
                        &endpoint,
                        self.headers_for(session)?,
                        &encoded,
                    )
                    .await?;
                if !response.status().is_success() {
                    return Err(self.handle_error(response).await);
                }
                Ok(response)
            },
            &retry_config,
            |attempt, max, err| {
                if let Ok(mut v) = notices.lock() {
                    v.push((
                        attempt,
                        max,
                        format!("{}/{} - {}", pname, notice_model, retry_reason(err)),
                    ));
                }
            },
        )
        .await?;

        tracing::info!(
            "Responses request: model={}, url={}",
            requested_model,
            endpoint
        );
        let value: serde_json::Value = response.json().await.map_err(|e| {
            ProviderError::StreamError(format!("Responses body decode failed: {e}"))
        })?;
        Ok(super::responses_response::parse_responses_json(
            value,
            &requested_model,
        )?)
    }

    /// Streaming POST to the Responses endpoint (#2059).
    ///
    /// The Responses API is a typed event stream, so framing keeps the
    /// `event:` name alongside the `data:` body and hands both to the pure
    /// adapter in [`super::responses_stream`].
    async fn stream_responses(&self, request: LLMRequest) -> Result<ProviderStream> {
        let endpoint = self
            .responses_endpoint()
            .ok_or_else(|| self.responses_endpoint_error())?;
        let requested_model = request.model.clone();
        let session = request.session_id;
        let mut body = super::responses_request::build_responses_request(
            request,
            self.reasoning_setting.as_deref(),
        );
        body.stream = Some(true);
        let retry_config = self.retry_config(&requested_model);
        let notices = self.retry_notices.clone();
        let pname = self.name().to_string();
        let notice_model = requested_model.clone();
        let response = crate::utils::retry::retry_with_notify(
            || async {
                let encoded = self.encode_body(&body)?;
                let response = self
                    .send_bounded(
                        &self.stream_client,
                        &endpoint,
                        self.headers_for(session)?,
                        &encoded,
                    )
                    .await?;
                if !response.status().is_success() {
                    return Err(self.handle_error(response).await);
                }
                Ok(response)
            },
            &retry_config,
            |attempt, max, err| {
                if let Ok(mut v) = notices.lock() {
                    v.push((
                        attempt,
                        max,
                        format!("{}/{} - {}", pname, notice_model, retry_reason(err)),
                    ));
                }
            },
        )
        .await?;
        tracing::info!(
            "Responses streaming request: model={}, url={}",
            requested_model,
            endpoint
        );

        let byte_stream = response.bytes_stream();
        let buffer = Arc::new(std::sync::Mutex::new(String::new()));
        let pending_event: Arc<std::sync::Mutex<Option<String>>> =
            Arc::new(std::sync::Mutex::new(None));
        let mut utf8_carry = Utf8Carry::new();
        let event_stream = byte_stream
            .map(
                move |chunk_result| -> Vec<std::result::Result<StreamEvent, ProviderError>> {
                    let chunk = match chunk_result {
                        Ok(c) => c,
                        Err(e) => return vec![Err(ProviderError::StreamError(e.to_string()))],
                    };
                    let raw = utf8_carry.push(&chunk);
                    let mut buf = buffer.lock().expect("responses SSE buffer lock poisoned");
                    buf.push_str(&raw);
                    let mut out: Vec<std::result::Result<StreamEvent, ProviderError>> = Vec::new();
                    let mut pending = pending_event
                        .lock()
                        .expect("responses SSE event lock poisoned");
                    while let Some(pos) = buf.find('\n') {
                        let line = buf[..pos].trim_end_matches('\r').to_string();
                        buf.drain(..=pos);
                        if let Some(rest) = line.strip_prefix("event:") {
                            *pending = Some(rest.trim().to_string());
                            continue;
                        }
                        let Some(json_str) = line.strip_prefix("data:") else {
                            continue;
                        };
                        let json_str = json_str.trim();
                        if json_str.is_empty() {
                            continue;
                        }
                        if json_str == "[DONE]" {
                            out.push(Ok(StreamEvent::MessageStop));
                            continue;
                        }
                        match serde_json::from_str::<serde_json::Value>(json_str) {
                            Ok(value) => {
                                // The frame's own `type` is the fallback when a
                                // gateway omits the `event:` line.
                                let name = pending.take().unwrap_or_else(|| {
                                    value
                                        .get("type")
                                        .and_then(|t| t.as_str())
                                        .unwrap_or("")
                                        .to_string()
                                });
                                out.extend(
                                    super::responses_stream::effects_for_event(&name, &value)
                                        .into_iter()
                                        .map(Ok),
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "[RESPONSES_STREAM] bad JSON frame: {} | {}",
                                    e,
                                    json_str.chars().take(300).collect::<String>()
                                );
                            }
                        }
                    }
                    if out.is_empty() {
                        vec![Ok(StreamEvent::Ping)]
                    } else {
                        out
                    }
                },
            )
            .flat_map(futures::stream::iter);
        Ok(Box::pin(event_stream))
    }

    /// POST `url` with `headers` and a pre-encoded `body`, bounding **one
    /// send's** connect + TLS + response headers (#680/#682).
    ///
    /// The budget is per ATTEMPT: a retry's backoff sleep belongs to the retry
    /// policy, never to this clock — so a send budget that expires means the
    /// send itself did not answer, not that we spent the interval on our own
    /// backoff. Bound at the send (rather than around
    /// `provider.stream(request)`, which also contains the rate-limit wait,
    /// the retry backoff and the fallback walk) is what makes the resulting
    /// `Timeout` mean what its name says.
    async fn send_bounded(
        &self,
        client: &Client,
        url: &str,
        headers: reqwest::header::HeaderMap,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response> {
        let budget = crate::brain::agent::service::helpers::send_handshake_timeout_for(
            false,
            Some(self.base_url.as_str()),
        );
        let send = client.post(url).headers(headers).json(body).send();
        let response = match budget {
            Some(budget) => match tokio::time::timeout(budget, send).await {
                Ok(res) => res,
                Err(_) => return Err(ProviderError::Timeout(budget.as_secs())),
            },
            None => send.await,
        }?;
        Ok(response)
    }

    /// Returns true if a ProviderError represents an auth failure that
    /// should trigger an auth-refresh retry (401/403).
    fn is_auth_error(err: &ProviderError) -> bool {
        matches!(
            err,
            ProviderError::InvalidApiKey
                | ProviderError::ApiError {
                    status: 401 | 403,
                    ..
                }
        )
    }

    /// Get the configured vision model name (if any).
    pub fn vision_model(&self) -> Option<&str> {
        self.vision_model.as_deref()
    }

    /// Build request headers for a call made outside any conversation (the
    /// model-catalogue fetch). Chat paths use [`Self::headers_for`] so the
    /// gateway sees the session the request belongs to.
    fn headers(&self) -> std::result::Result<reqwest::header::HeaderMap, ProviderError> {
        self.headers_for(None)
    }

    /// Build request headers for a request belonging to `session`.
    fn headers_for(
        &self,
        session: Option<uuid::Uuid>,
    ) -> std::result::Result<reqwest::header::HeaderMap, ProviderError> {
        let mut headers = reqwest::header::HeaderMap::new();

        // Resolve the bearer token: dynamic token_fn takes priority over static api_key
        let bearer_key = if let Some(ref f) = self.token_fn {
            let token = f();
            if token.is_empty() { None } else { Some(token) }
        } else if self.api_key != "not-needed" {
            Some(self.api_key.trim().to_string())
        } else {
            None
        };

        if let Some(key) = bearer_key {
            let header_value: reqwest::header::HeaderValue =
                format!("Bearer {}", key).parse().map_err(|_| {
                    tracing::error!(
                        "API key contains invalid characters (length={}). Check keys.toml.",
                        key.len()
                    );
                    ProviderError::InvalidApiKey
                })?;
            headers.insert(reqwest::header::AUTHORIZATION, header_value);
        }

        headers.insert(
            reqwest::header::CONTENT_TYPE,
            "application/json".parse().expect("valid content-type"),
        );
        // Explicit Accept — matches what the `openai` npm SDK sends. Without
        // this, reqwest falls back to `accept: */*` which is a visible
        // fingerprint difference from qwen-cli / most SDK clients and at
        // least one gateway (portal.qwen.ai DashScope) rate-limits on it.
        headers.insert(
            reqwest::header::ACCEPT,
            "application/json".parse().expect("valid accept"),
        );

        // OpenRouter-specific optimization headers
        if self.base_url.to_lowercase().contains("openrouter") {
            if let (Ok(k1), Ok(v1)) = (
                "HTTP-Referer".parse::<reqwest::header::HeaderName>(),
                "https://opencrabs.com".parse::<reqwest::header::HeaderValue>(),
            ) {
                headers.insert(k1, v1);
            }
            if let (Ok(k2), Ok(v2)) = (
                "X-Title".parse::<reqwest::header::HeaderName>(),
                "OpenCrabs".parse::<reqwest::header::HeaderValue>(),
            ) {
                headers.insert(k2, v2);
            }
            if let (Ok(k3), Ok(v3)) = (
                "X-OpenRouter-Category".parse::<reqwest::header::HeaderName>(),
                "cli-agent,personal-agent,programming-app".parse::<reqwest::header::HeaderValue>(),
            ) {
                headers.insert(k3, v3);
            }
            // OpenRouter response caching — zero cost for identical requests
            if self.cache_enabled {
                if let (Ok(k), Ok(v)) = (
                    "X-OpenRouter-Cache".parse::<reqwest::header::HeaderName>(),
                    "true".parse::<reqwest::header::HeaderValue>(),
                ) {
                    headers.insert(k, v);
                }
                if let Some(ttl) = self.cache_ttl
                    && let (Ok(k), Ok(v)) = (
                        "X-OpenRouter-Cache-TTL".parse::<reqwest::header::HeaderName>(),
                        ttl.to_string().parse::<reqwest::header::HeaderValue>(),
                    )
                {
                    headers.insert(k, v);
                }
            }
            tracing::debug!("OpenRouter optimization headers attached");
        }

        for (key, value) in &self.extra_headers {
            if let (Ok(k), Ok(v)) = (
                key.parse::<reqwest::header::HeaderName>(),
                value.parse::<reqwest::header::HeaderValue>(),
            ) {
                headers.insert(k, v);
            }
        }

        // Client identification, resolved per gateway; a no-op for any host
        // without an identity contract.
        for (key, value) in super::identity::headers_for(&self.base_url, session) {
            match (
                key.parse::<reqwest::header::HeaderName>(),
                value.parse::<reqwest::header::HeaderValue>(),
            ) {
                (Ok(k), Ok(v)) => {
                    headers.insert(k, v);
                }
                _ => tracing::warn!(
                    "Identity: dropping malformed header '{}' for {} — the gateway may \
                     reject this request as an unidentified client",
                    key,
                    self.base_url
                ),
            }
        }

        Ok(headers)
    }

    /// Convert our generic request to OpenAI-specific format
    pub(crate) fn to_openai_request(&self, request: LLMRequest) -> OpenAIRequest {
        // Captured before `request.messages` is consumed below, so the
        // reasoning-echo telemetry (#830) can name the model.
        let request_model_for_log = request.model.clone();
        let total_request_messages = request.messages.len();
        let mut total_thinking_blocks: usize = 0;
        let mut total_reasoning_chars: usize = 0;
        let mut messages_with_reasoning: usize = 0;
        let mut messages = Vec::new();

        // Debug: log system brain
        if let Some(ref system) = request.system {
            tracing::debug!("System brain present: {} chars", system.len());
        } else {
            tracing::warn!("NO SYSTEM BRAIN in request!");
        }

        // Add system message if present. For a caching (qwen / DashScope) target
        // with a runtime suffix, send a 2-part array [stable, suffix] so
        // qwen_body_transform can cache_control ONLY the stable brain part —
        // keeping the cached prefix byte-stable across sessions. Other providers
        // get the concatenated string, byte-identical to the pre-split prompt
        // (#658).
        if let Some(stable) = request.system {
            let suffix = request.system_suffix.filter(|x| !x.trim().is_empty());
            let content = match suffix {
                Some(suffix)
                    if crate::brain::provider::qwen::looks_like_qwen_target(
                        &self.base_url,
                        &request.model,
                    ) =>
                {
                    serde_json::json!([
                        { "type": "text", "text": stable },
                        { "type": "text", "text": suffix },
                    ])
                }
                Some(suffix) => serde_json::Value::String(format!("{stable}\n\n{suffix}")),
                None => serde_json::Value::String(stable),
            };
            messages.push(OpenAIMessage {
                role: "system".to_string(),
                content: Some(content),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            });
        }

        // Add conversation messages
        for msg in request.messages {
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => "system",
            };

            // Separate content blocks by type
            let mut text_parts = Vec::new();
            let mut image_parts: Vec<serde_json::Value> = Vec::new();
            let mut tool_uses = Vec::new();
            let mut tool_results = Vec::new();
            let mut thinking_parts: Vec<String> = Vec::new();

            for block in msg.content {
                match block {
                    ContentBlock::Text { text } => {
                        text_parts.push(text);
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        tool_uses.push((id, name, input));
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => {
                        tool_results.push((tool_use_id, content));
                    }
                    ContentBlock::Thinking { thinking, .. } => {
                        // Capture for providers that serialize it as
                        // `reasoning_content` on assistant tool_call
                        // messages (Moonshot / opencode.ai kimi). Others
                        // ignore the field entirely.
                        if !thinking.is_empty() {
                            thinking_parts.push(thinking);
                        }
                    }
                    ContentBlock::Image { source } => {
                        let url = match source {
                            ImageSource::Base64 { media_type, data } => {
                                format!("data:{};base64,{}", media_type, data)
                            }
                            ImageSource::Url { url } => url,
                        };
                        image_parts.push(serde_json::json!({
                            "type": "image_url",
                            "image_url": { "url": url }
                        }));
                    }
                }
            }

            // Build content value: array when images present, string otherwise
            let make_content =
                |texts: &[String], images: &[serde_json::Value]| -> Option<serde_json::Value> {
                    if !images.is_empty() {
                        let mut parts: Vec<serde_json::Value> = Vec::new();
                        if !texts.is_empty() {
                            parts.push(serde_json::json!({
                                "type": "text",
                                "text": texts.join("\n")
                            }));
                        }
                        parts.extend(images.iter().cloned());
                        Some(serde_json::Value::Array(parts))
                    } else if !texts.is_empty() {
                        Some(serde_json::Value::String(texts.join("\n")))
                    } else {
                        None
                    }
                };

            // Handle assistant messages with tool calls
            if !tool_uses.is_empty() {
                let openai_tool_calls = tool_uses
                    .into_iter()
                    .map(|(id, name, input)| OpenAIToolCall {
                        id,
                        r#type: "function".to_string(),
                        function: OpenAIFunctionCall {
                            name,
                            arguments: serde_json::to_string(&input).unwrap_or_default(),
                        },
                    })
                    .collect();

                let mut content_val = make_content(&text_parts, &image_parts);
                // A tool-call-only turn carries no text, and `make_content`
                // returns None for that, which drops the field from the body
                // entirely. DeepSeek's own harness replays an empty string
                // rather than null on these turns, noting that some gateways
                // reject null — and an absent field is a third state neither
                // of them describes. Send what the vendor sends.
                if content_val.is_none()
                    && super::deepseek_reasoning::serves_deepseek(&self.base_url, &request.model)
                {
                    content_val = Some(serde_json::Value::String(String::new()));
                }
                // Real reasoning flows in via ContentBlock::Thinking blocks
                // from tool_loop (live turns) and from_db_messages (resumed
                // turns via the DB `thinking` column). `needs_reasoning_content`
                // is a last-resort safety for historic DB rows that predate
                // the thinking column — Moonshot validates non-empty, so a
                // single space satisfies the shape check when we truly have
                // nothing to echo. Non-Moonshot providers see None.
                let reasoning_content = if !thinking_parts.is_empty() {
                    // Real reasoning — sent for EVERY provider, gate or not.
                    // Qwen's contract ("pass back the complete reasoning_content")
                    // is satisfied here, not by the fallback below.
                    Some(thinking_parts.join("\n"))
                } else if needs_reasoning_content_for(&self.base_url, &request.model) {
                    // Blank-chain fallback, Moonshot ONLY. Moonshot merely
                    // validates the field is present and non-empty, so a space
                    // satisfies the shape check.
                    //
                    // It must NOT reach qwen. Qwen requires the COMPLETE chain,
                    // and the `thinking` DB column is currently never written —
                    // so on qwen this fired for every historical assistant
                    // tool_call message, handing the model an empty thinking
                    // chain. Qwen then re-derived its reasoning and, since it
                    // cannot concatenate reasoning into `content`, spilled it
                    // into the visible answer: reasoning-only turns, leaked
                    // thinking, and the same paragraph repeating (#760).
                    // Omitting is strictly better than blanking.
                    Some(" ".to_string())
                } else {
                    None
                };

                // #830 & #245: accumulate thinking metrics across messages
                // to emit a single consolidated summary per payload build.
                total_thinking_blocks += thinking_parts.len();
                total_reasoning_chars += reasoning_content.as_deref().map(str::len).unwrap_or(0);
                if reasoning_content.is_some() {
                    messages_with_reasoning += 1;
                }

                messages.push(OpenAIMessage {
                    role: role.to_string(),
                    content: content_val,
                    tool_calls: Some(openai_tool_calls),
                    tool_call_id: None,
                    reasoning_content,
                });
            }
            // Handle tool result messages
            else if !tool_results.is_empty() {
                for (tool_use_id, content) in tool_results {
                    messages.push(OpenAIMessage {
                        role: "tool".to_string(),
                        content: Some(serde_json::Value::String(content)),
                        tool_calls: None,
                        tool_call_id: Some(tool_use_id),
                        reasoning_content: None,
                    });
                }
            }
            // Handle regular text messages (with optional images)
            else {
                let content_val = make_content(&text_parts, &image_parts)
                    .unwrap_or(serde_json::Value::String(String::new()));

                // Assistant-only: if we captured thinking from a
                // previously-streamed assistant turn without tool_calls,
                // echo it under reasoning_content for kimi/Moonshot.
                let reasoning_content = if role == "assistant" && !thinking_parts.is_empty() {
                    Some(thinking_parts.join("\n"))
                } else {
                    None
                };

                if role == "assistant" {
                    total_thinking_blocks += thinking_parts.len();
                    total_reasoning_chars +=
                        reasoning_content.as_deref().map(str::len).unwrap_or(0);
                    if reasoning_content.is_some() {
                        messages_with_reasoning += 1;
                    }
                }

                messages.push(OpenAIMessage {
                    role: role.to_string(),
                    content: Some(content_val),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content,
                });
            }
        }

        tracing::debug!(
            target: "reasoning_echo",
            model = %request_model_for_log,
            message_count = total_request_messages,
            thinking_blocks = total_thinking_blocks,
            reasoning_chars = total_reasoning_chars,
            echoed_messages = messages_with_reasoning,
            "payload build: reasoning_content summary"
        );

        // Convert tools to OpenAI format
        let tools: Option<Vec<OpenAITool>> = request.tools.map(|tools| {
            tools
                .iter()
                .map(|tool| OpenAITool {
                    r#type: "function".to_string(),
                    function: OpenAIFunction {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        parameters: tool.input_schema.clone(),
                    },
                })
                .collect()
        });

        // Newer OpenAI models (gpt-4.1-*, gpt-5-*, gpt-6-*, o1-*, o3-*) require
        // max_completion_tokens instead of max_tokens. Use the new field
        // for these models and fall back to max_tokens for everything else.
        let uses_completion_tokens = uses_max_completion_tokens(&request.model);
        let (max_tokens, max_completion_tokens) = if uses_completion_tokens {
            (None, request.max_tokens)
        } else {
            (request.max_tokens, None)
        };

        // Set tool_choice to "auto" when tools are present so the model
        // knows it is allowed to call them (MiniMax requires this explicitly).
        let tool_choice = tools
            .as_ref()
            .filter(|t| !t.is_empty())
            .map(|_| serde_json::json!("auto"));

        // Enable reasoning/thinking for OpenRouter and compatible endpoints.
        // Detection is intentionally broad — models that don't support the field ignore it.
        let base = self.base_url.to_lowercase();
        let include_reasoning = if base.contains("openrouter")
            || base.contains("openrouter.ai")
            || std::env::var("OPENCRABS_ENABLE_REASONING").is_ok()
        {
            Some(true)
        } else {
            None
        };

        // Reasoning controls from the provider's configured `reasoning_effort`.
        // For a Kimi family, use the family-specific resolver (effort for K3,
        // thinking on/off for K2.x; an invalid value is a no-op there). For ANY
        // OTHER custom-provider model, pass the configured value straight through
        // as the request's `reasoning_effort` field — otherwise it was silently
        // dropped and e.g. modelstudio/DashScope qwen never got `xhigh` (#691).
        let (mut reasoning_effort, mut thinking) = match self.reasoning_setting.as_deref() {
            None => (None, None),
            Some(s) if super::kimi_reasoning::is_kimi_model(&request.model) => {
                super::kimi_reasoning::resolve_fields(&request.model, s)
            }
            Some(s) => (Some(s.to_string()), None),
        };

        // DashScope thinking knobs. Each qwen family reads exactly one of
        // `reasoning_effort` / `enable_thinking`, so resolve which one applies
        // and drop the inert other rather than shipping both (#1034). Gated on
        // the host: a locally served qwen takes thinking through
        // `chat_template_kwargs` and must not receive DashScope fields.
        //
        // Both gates are load-bearing. Model Studio also serves glm and
        // deepseek: those are not qwen, read neither knob, and must keep the
        // `reasoning_effort` resolved above rather than have it overwritten
        // with the empty qwen resolution.
        let mut enable_thinking = None;
        if super::qwen::serves_qwen_remotely(&self.base_url, &request.model)
            && super::qwen_reasoning::family(&request.model).is_some()
        {
            let knobs = super::qwen_reasoning::resolve(
                &request.model,
                reasoning_effort.as_deref(),
                self.enable_thinking_setting,
            );
            reasoning_effort = knobs.reasoning_effort;
            enable_thinking = knobs.enable_thinking;
        }

        // DeepSeek's knobs, which are not DashScope's. It reads a top-level
        // `thinking: {"type": ...}` plus a `low | high | max` effort, and
        // ignores `enable_thinking` entirely. Until now a DeepSeek target fell
        // through to the pass-through arm above, so a configured `xhigh` (a
        // qwen rung DeepSeek has no equivalent for) rode to the wire while the
        // toggle it actually reads went unset.
        //
        // Gated on the model id OR the endpoint, because DeepSeek is not a
        // built-in provider here: it is only ever reached through a custom
        // OpenAI-compatible entry, and gateways re-serve these models under a
        // namespaced id (#1040). A local runtime is excluded, matching qwen.
        if super::deepseek_reasoning::serves_deepseek(&self.base_url, &request.model) {
            let knobs = super::deepseek_reasoning::resolve(
                reasoning_effort.as_deref(),
                self.enable_thinking_setting,
            );
            thinking = Some(serde_json::json!({
                "type": if knobs.enabled { "enabled" } else { "disabled" }
            }));
            reasoning_effort = knobs.effort.map(str::to_string);
        }

        // z.ai GLM-5.x: Preserved Thinking, so a tool loop stops re-deriving
        // the whole chain on every step (#1348). Omitted for every other
        // host and for GLM-4.x, which keep their current behaviour.
        if let Some(glm) = super::glm_reasoning::thinking_for(
            &self.base_url,
            &request.model,
            self.enable_thinking_setting,
        ) {
            if glm.off_ignored {
                tracing::warn!(
                    "enable_thinking = false is ignored by {}: GLM-5.3 and later cannot turn thinking off",
                    request.model
                );
            }
            thinking = Some(glm.to_json());
        }
        // GLM-5.3+ reads a three-rung ladder and turns anything else into
        // `max` without saying so (#1349). Map, and say so.
        if let Some(mapped) = super::glm_reasoning::effort_for(
            &self.base_url,
            &request.model,
            reasoning_effort.as_deref(),
        ) {
            if let Some(note) = mapped.note.as_deref() {
                tracing::warn!("{note}");
            }
            reasoning_effort = Some(mapped.effort.to_string());
        }

        // Carry reasoning across turns instead of re-deriving it each turn
        // (#1033). Sent on every DashScope request, ungated by family.
        let preserve_thinking =
            if super::qwen_reasoning::preserve_thinking_for(&self.base_url, &request.model) {
                Some(true)
            } else {
                None
            };

        // MiniMax M3: send reasoning_split=true so reasoning goes to
        // reasoning_content field instead of being embedded in content
        // as <think> tags. Prevents reasoning leak into visible output (#667).
        let is_minimax = self.base_url.to_lowercase().contains("minimax")
            || request.model.to_lowercase().contains("minimax")
            || request.model.to_lowercase().contains("mimo");
        let reasoning_split = if is_minimax { Some(true) } else { None };

        // z.ai buffers tool-call arguments unless asked to stream them, and a
        // buffered 40 KB write is minutes of SSE silence that the idle timer
        // reads as a dead connection (#1347).
        let tool_stream = super::glm_reasoning::tool_stream_for(&self.base_url, request.stream);

        OpenAIRequest {
            model: request.model,
            messages,
            temperature: request.temperature,
            max_tokens,
            max_completion_tokens,
            stream: Some(request.stream),
            stream_options: None,
            tools,
            tool_choice,
            include_reasoning,
            reasoning_effort,
            thinking,
            reasoning_split,
            preserve_thinking,
            enable_thinking,
            tool_stream,
        }
    }

    /// Convert OpenAI response to our generic format
    #[allow(clippy::wrong_self_convention)]
    fn from_openai_response(&self, response: OpenAIResponse, requested_model: &str) -> LLMResponse {
        let choice = response
            .choices
            .into_iter()
            .next()
            .unwrap_or_else(|| OpenAIChoice {
                message: OpenAIMessage {
                    role: "assistant".to_string(),
                    content: Some(serde_json::Value::String(String::new())),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
                finish_reason: Some("error".to_string()),
            });

        // Convert content to content blocks
        let mut content_blocks = Vec::new();

        // Add text content if present, stripping <think>...</think> reasoning blocks
        if let Some(ref content_val) = choice.message.content {
            let text = match content_val {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Array(parts) => {
                    // Extract text from content parts array
                    parts
                        .iter()
                        .filter_map(|p| {
                            if p.get("type")?.as_str()? == "text" {
                                p.get("text")?.as_str().map(String::from)
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
                _ => String::new(),
            };
            if !text.is_empty() {
                let clean = strip_think_blocks(&text);
                if !clean.is_empty() {
                    content_blocks.push(ContentBlock::Text { text: clean });
                }
            }
        }

        // Fallback: local GGUF/MLX backends (llama.cpp, MLX, LM Studio, Ollama)
        // commonly emit Qwen tool calls as `<tool_call>{...}</tool_call>` text
        // inside `content` instead of the structured `tool_calls` field — this
        // happens whenever the runtime isn't launched with the right jinja +
        // reasoning template. Scan the extracted Text blocks and promote any
        // tool-call tags to ContentBlock::ToolUse so they actually execute.
        let structured_tool_calls_present = choice
            .message
            .tool_calls
            .as_ref()
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        if !structured_tool_calls_present {
            let mut recovered: Vec<ContentBlock> = Vec::new();
            for block in content_blocks.iter_mut() {
                if let ContentBlock::Text { text } = block {
                    let (calls, cleaned) = extract_text_tool_calls(text);
                    if calls.is_empty() {
                        continue;
                    }
                    tracing::info!(
                        "Recovered {} tool call(s) from text content (local-model fallback)",
                        calls.len()
                    );
                    *text = cleaned;
                    for (idx, (name, input)) in calls.into_iter().enumerate() {
                        recovered.push(ContentBlock::ToolUse {
                            id: format!("call_text_{}", idx),
                            name,
                            input,
                        });
                    }
                }
            }
            // Drop any Text blocks we just emptied, then append the recovered
            // tool-use blocks in the order they appeared.
            content_blocks.retain(|b| match b {
                ContentBlock::Text { text } => !text.trim().is_empty(),
                _ => true,
            });
            content_blocks.extend(recovered);
        }

        // Convert tool_calls to ToolUse content blocks
        if let Some(tool_calls) = choice.message.tool_calls {
            tracing::debug!(
                "Converting {} tool calls from OpenAI response",
                tool_calls.len()
            );
            for tool_call in tool_calls {
                // Parse arguments JSON string with partial-repair fallback so
                // a truncated tool call surfaces partial intent instead of {}.
                let input = super::json_repair::parse_or_repair(&tool_call.function.arguments);

                tracing::debug!(
                    "Converted tool call: {} with id {}",
                    tool_call.function.name,
                    tool_call.id
                );

                content_blocks.push(ContentBlock::ToolUse {
                    id: tool_call.id,
                    name: tool_call.function.name,
                    input,
                });
            }
        }

        // Detect models that dump tool JSON as text instead of structured
        // calls. Tightened detector (fork #66, ex-upstream
        // opencrabs/opencrabs#1260): only a parseable call-shaped JSON
        // object outside code fences counts — prose about tools no longer
        // false-positives. On detection the raw JSON is STRIPPED from the
        // content and `tool_text_leak` is set on the response for the tool
        // loop's corrective retry. No ⚠️ warning block is appended: parser
        // artifacts must never ride as content (they used to be persisted
        // verbatim into compaction summaries / memory files).
        let has_structured_tools = content_blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
        let mut tool_text_leak = false;
        if !has_structured_tools {
            for block in content_blocks.iter_mut() {
                if let ContentBlock::Text { text } = block {
                    let (cleaned, stripped) = super::json_repair::strip_call_shaped_json(text);
                    if stripped {
                        tracing::warn!(
                            "Model returned tool call JSON as text — unrecoverable, \
                             stripped from content (likely weak function-calling support)"
                        );
                        *text = cleaned;
                        tool_text_leak = true;
                    }
                }
            }
            if tool_text_leak {
                // Drop text blocks emptied by the strip.
                content_blocks.retain(|b| match b {
                    ContentBlock::Text { text } => !text.trim().is_empty(),
                    _ => true,
                });
            }
        }

        // Map finish_reason to StopReason
        let stop_reason = choice
            .finish_reason
            .and_then(|reason| match reason.as_str() {
                "stop" => Some(StopReason::EndTurn),
                "length" => Some(StopReason::MaxTokens),
                "tool_calls" | "function_call" => Some(StopReason::ToolUse),
                _ => None,
            });

        LLMResponse {
            id: response.id,
            model: stable_model_id(response.model, requested_model),
            content: content_blocks,
            stop_reason,
            usage: TokenUsage {
                input_tokens: response.usage.net_input_tokens(),
                output_tokens: response.usage.completion_tokens.unwrap_or(0),
                cache_creation_tokens: response.usage.cache_creation_input_tokens.unwrap_or(0),
                cache_read_tokens: response.usage.effective_cache_read(),
                cache_read_unreported: response.usage.cache_read_absent(),
                cost_usd: response.usage.cost,
                ..Default::default()
            },
            // Non-streaming parse path — streaming responses go through helpers.rs.
            streaming_active_secs: None,
            tool_text_leak,
        }
    }

    /// Handle API error response
    async fn handle_error(&self, response: reqwest::Response) -> ProviderError {
        let status = response.status().as_u16();

        // Extract Retry-After header for rate limits
        let retry_after = response.headers().get("retry-after").and_then(|v| {
            v.to_str().ok().and_then(|s| {
                // Retry-After can be either seconds or HTTP date
                // Try parsing as seconds first
                s.parse::<u64>().ok()
            })
        });

        let base_url_snapshot = self.base_url.clone();
        // Read the body as raw text first so we can log AND still try both
        // the OpenAI shape and any non-OpenAI shapes (Unsloth Studio's
        // FastAPI pydantic 422, llama-server's plain `{error:{message}}`,
        // etc.) without losing the actual validation details.
        let body_text = response.text().await.unwrap_or_default();

        // Always log the raw body on any error response. Without this,
        // generic passthrough messages like opencode.ai's
        // "Provider returned error" left the operator with nothing to
        // diagnose — the real error from the upstream backend was
        // hidden inside the OpenAI-shaped envelope, but all we logged
        // was the envelope's outer message.
        tracing::warn!(
            "[HANDLE_ERROR] {} → {}: raw body (first 1500 chars): {}",
            self.name,
            status,
            body_text.chars().take(1500).collect::<String>(),
        );

        // Try to parse OpenAI's `{error: {message, type}}` shape.
        if let Ok(error_body) = serde_json::from_str::<OpenAIErrorResponse>(&body_text) {
            // Unwrap proxy passthrough envelopes (opencode.ai, OpenRouter,
            // etc.) so the real upstream error surfaces instead of a
            // generic "Provider returned error".
            let (inner_message, inner_type) = unwrap_proxy_error(&error_body.error);
            let message = if status == 429 {
                super::error::rate_limit_message(&inner_message, retry_after)
            } else {
                inner_message
            };

            return if status == 429 {
                tracing::warn!("[RATE_LIMIT] {} → {}: {}", self.name, status, message,);
                ProviderError::RateLimitExceeded(message)
            } else {
                ProviderError::ApiError {
                    status,
                    message,
                    error_type: inner_type.or(Some(String::new())),
                }
            };
        }

        // 422 bodies from FastAPI/pydantic servers (notably Unsloth Studio)
        // come back as `{detail: [{loc, msg, type}, ...]}`. The generic
        // "Unknown error" fallback lost the per-field details, so operators
        // had no way to see WHICH field the server rejected. The body is
        // already logged above; we just translate it here.

        if status == 422
            && let Ok(pydantic) = serde_json::from_str::<PydanticValidationError>(&body_text)
        {
            // Build a concise summary of the offending fields.
            let mut parts: Vec<String> = pydantic
                .detail
                .iter()
                .take(5)
                .map(|d| {
                    let field = d
                        .loc
                        .iter()
                        .map(|v| match v {
                            serde_json::Value::String(s) => s.clone(),
                            serde_json::Value::Number(n) => n.to_string(),
                            _ => "?".to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join(".");
                    format!("{}: {}", field, d.msg)
                })
                .collect();
            if pydantic.detail.len() > 5 {
                parts.push(format!("… +{} more", pydantic.detail.len() - 5));
            }
            let fields_summary = parts.join("; ");

            // Unsloth Studio's wrapper is NOT OpenAI-compatible (its
            // pydantic schema rejects `tools`, `tool_choice`, `role:
            // "tool"`, and the `chat_template_kwargs` object). Point the
            // operator at the fix rather than leaving them to decode
            // validation errors.
            let unsloth_hint = if is_unsloth_studio_url(&base_url_snapshot) {
                " — Unsloth Studio's API is NOT OpenAI-compat (rejects `tools`, `role:\"tool\"`, etc.). \
                 Find the internal llama-server port with `lsof -iTCP -sTCP:LISTEN | grep llama-server` \
                 and point base_url at that directly."
            } else {
                ""
            };

            return ProviderError::ApiError {
                status,
                message: format!(
                    "Schema validation failed ({} field(s)): {}{}",
                    pydantic.detail.len(),
                    fields_summary,
                    unsloth_hint,
                ),
                error_type: Some("validation_error".to_string()),
            };
        }

        // Fallback error — include the first chunk of the raw body so
        // operators aren't stuck with "Unknown error".
        let fallback_message = if body_text.is_empty() {
            "Unknown error".to_string()
        } else {
            format!(
                "HTTP {}: {}",
                status,
                body_text.chars().take(400).collect::<String>()
            )
        };
        if status == 429 {
            let message = if let Some(secs) = retry_after {
                format!("Rate limit exceeded (retry after {} seconds)", secs)
            } else {
                "Rate limit exceeded, please retry later".to_string()
            };
            ProviderError::RateLimitExceeded(message)
        } else {
            ProviderError::ApiError {
                status,
                message: fallback_message,
                error_type: None,
            }
        }
    }
}

/// Keep the model identifier OpenCrabs sent when a compatible gateway reports
/// a backend-local identifier in its response. The request ID is also the one
/// used for the provider/model session pair, so replacing it with the response
/// ID can make the next turn fail when the gateway strips a provider prefix.
pub(crate) fn stable_model_id(response_model: String, requested_model: &str) -> String {
    if requested_model.is_empty() {
        response_model
    } else {
        requested_model.to_string()
    }
}

#[async_trait]
impl Provider for OpenAIProvider {
    async fn complete(&self, request: LLMRequest) -> Result<LLMResponse> {
        use crate::utils::retry::retry;

        let model = request.model.clone();
        let message_count = request.messages.len();
        let retry_config = self.retry_config(&model);
        // Captured before `to_openai_request` consumes the request: the
        // OpenCode session header is keyed on it.
        let session = request.session_id;

        // #2059: an outright Responses route never builds a chat body.
        if self.dialect_is_responses() {
            return self.complete_responses(request).await;
        }
        // Auto keeps a copy for one fallback attempt when the chat endpoint
        // refuses the request and this base URL has a Responses route.
        let responses_fallback = (self.wire_dialect == WireDialect::Auto
            && self.responses_endpoint().is_some())
        .then(|| request.clone());

        let mut openai_request = self.to_openai_request(request);

        let tool_count = openai_request.tools.as_ref().map(|t| t.len()).unwrap_or(0);
        tracing::info!(
            "OpenAI API request: model={}, messages={}, max_tokens={:?}, max_completion_tokens={:?}, tools={}",
            model,
            message_count,
            openai_request.max_tokens,
            openai_request.max_completion_tokens,
            tool_count
        );
        if tool_count == 0 {
            tracing::warn!(
                "OpenAI request has NO tools - LLM won't know about file/bash operations!"
            );
        }

        // Proactive pacing: stay under provider rate limits (e.g. OpenRouter :free at 20/min)
        if let Some(ref limiter) = self.rate_limiter {
            let waited = limiter.wait().await;
            if !waited.is_zero() {
                tracing::debug!(
                    "Rate limiter: waited {:?} before request to {}",
                    waited,
                    self.base_url
                );
            }
        }

        // Retry the entire API call with exponential backoff. Each retry is
        // recorded into `retry_notices` exactly like the streaming path, so a
        // caller of the non-streaming request (compaction above all) can
        // surface "Retry N/M" instead of retrying in silence (#1520).
        let notices = self.retry_notices.clone();
        let pname = self.name().to_string();
        let result = crate::utils::retry::retry_with_notify(
            || async {
                tracing::debug!("Sending request to OpenAI API: {}", self.base_url);
                let body = self.encode_body(&openai_request)?;
                let response = self
                    .send_bounded(&self.client, &self.send_url(), self.headers_for(session)?, &body)
                    .await?;

                let status = response.status();
                tracing::debug!("OpenAI API response status: {}", status);

                if !status.is_success() {
                    return Err(self.handle_error(response).await);
                }

                // Extract OpenRouter cache status before consuming response
                let cache_status = response
                    .headers()
                    .get("x-openrouter-cache-status")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());

                // Provider request/trace ID (#1013) for incident correlation.
                // Extracted before consumption: headers vanish once the body
                // is read.
                let request_id = provider_request_id(response.headers());

                let body = response.text().await?;
                log_usage_keys(&body);
                let openai_response: OpenAIResponse = serde_json::from_str(&body)?;
                let model_for_response = model.clone();
                let llm_response = self.from_openai_response(openai_response, &model_for_response);

                // Log cache hit/miss for OpenRouter
                if let Some(ref status) = cache_status {
                    if status == "HIT" {
                        tracing::info!("OpenRouter cache HIT — zero tokens billed");
                    } else {
                        tracing::debug!("OpenRouter cache MISS");
                    }
                }

                tracing::info!(
                    "OpenAI API response: input_tokens={}, output_tokens={}, stop_reason={:?}, response_id={}{}",
                    llm_response.usage.input_tokens,
                    llm_response.usage.output_tokens,
                    llm_response.stop_reason,
                    llm_response.id,
                    request_id
                        .as_deref()
                        .map(|r| format!(", request_id={r}"))
                        .unwrap_or_default()
                );

                Ok(llm_response)
            },
            &retry_config,
            |attempt, max, err| {
                if let Ok(mut v) = notices.lock() {
                    v.push((
                        attempt,
                        max,
                        format!("{}/{}: {}", pname, model, retry_reason(err)),
                    ));
                }
            },
        )
        .await;

        // Resilient fallback: if the API rejected max_tokens / max_completion_tokens,
        // swap the fields and retry once.
        if let Err(ref e) = result {
            if is_token_field_mismatch(&e.to_string()) {
                tracing::warn!(
                    "Token field mismatch for model {}, retrying with swapped fields",
                    model
                );
                openai_request.swap_token_fields();
                return retry(
                    || async {
                        let body = self.encode_body(&openai_request)?;
                        let response = self
                            .send_bounded(
                                &self.client,
                                &self.send_url(),
                                self.headers_for(session)?,
                                &body,
                            )
                            .await?;
                        if !response.status().is_success() {
                            return Err(self.handle_error(response).await);
                        }
                        let openai_response: OpenAIResponse = response.json().await?;
                        let model_for_response = model.clone();
                        Ok(self.from_openai_response(openai_response, &model_for_response))
                    },
                    &retry_config,
                )
                .await;
            }

            // Auth-refresh fallback: on 401/403, call the refresh hook once
            // (if installed) and retry the request a single time with the
            // rotated token. Used by qwen where OAuth tokens expire mid-session.
            if Self::is_auth_error(e)
                && let Some(ref refresh) = self.auth_refresh_fn
            {
                tracing::warn!("{} auth error — refreshing and retrying", self.name);
                match refresh().await {
                    Ok(()) => {
                        // Retry once with the new token. If this STILL returns
                        // 401, do NOT invalidate — could be transient server
                        // propagation delay. Let the outer rotation/fallback
                        // handle it; the next request cycle will try again.
                        return retry(
                            || async {
                                let body = self.encode_body(&openai_request)?;
                                let response = self
                                    .send_bounded(
                                        &self.client,
                                        &self.send_url(),
                                        self.headers_for(session)?,
                                        &body,
                                    )
                                    .await?;
                                if !response.status().is_success() {
                                    return Err(self.handle_error(response).await);
                                }
                                let openai_response: OpenAIResponse = response.json().await?;
                                let model_for_response = model.clone();
                                Ok(self.from_openai_response(openai_response, &model_for_response))
                            },
                            &retry_config,
                        )
                        .await;
                    }
                    Err(msg) => {
                        tracing::error!("{} auth refresh failed: {}", self.name, msg);
                        // Only invalidate when the refresh_token itself is dead
                        // (HTTP 400 from the token endpoint). Other errors
                        // (network, WAF, transient 5xx) must NOT invalidate.
                        if msg.contains("HTTP 400")
                            && let Some(ref invalidate) = self.auth_invalidate_fn
                        {
                            tracing::warn!(
                                "{} refresh_token permanently dead — invalidating account",
                                self.name
                            );
                            invalidate();
                        }
                    }
                }
            }

            tracing::error!("OpenAI API request failed: {}", e);
        }

        // #2059 Auto: the endpoint refused the chat dialect, so try the
        // Responses route once before surfacing the failure.
        if let (Err(e), Some(fallback_request)) = (&result, responses_fallback)
            && is_dialect_rejection_error(e)
        {
            tracing::warn!(
                "{}: chat endpoint rejected the request ({}); retrying on the Responses API",
                self.name(),
                e
            );
            return self.complete_responses(fallback_request).await;
        }

        result
    }

    async fn stream(&self, request: LLMRequest) -> Result<ProviderStream> {
        use crate::utils::retry::retry;

        let model = request.model.clone();
        let message_count = request.messages.len();
        // Captured before `to_openai_request` consumes the request: the
        // OpenCode session header is keyed on it.
        let session = request.session_id;

        // Proactive pacing: stay under provider rate limits
        if let Some(ref limiter) = self.rate_limiter {
            let waited = limiter.wait().await;
            if !waited.is_zero() {
                tracing::debug!(
                    "Rate limiter: waited {:?} before streaming request to {}",
                    waited,
                    self.base_url
                );
            }
        }

        tracing::info!(
            "{} streaming request: model={}, messages={}, base_url={}",
            self.name(),
            model,
            message_count,
            self.base_url
        );

        // #2059: an outright Responses route never builds a chat body.
        if self.dialect_is_responses() {
            return self.stream_responses(request).await;
        }
        // Auto keeps a copy for one fallback attempt when the chat endpoint
        // refuses the request and this base URL has a Responses route.
        let responses_fallback = (self.wire_dialect == WireDialect::Auto
            && self.responses_endpoint().is_some())
        .then(|| request.clone());

        let mut openai_request = self.to_openai_request(request);
        openai_request.stream = Some(true);
        openai_request.stream_options = Some(StreamOptions {
            include_usage: true,
        });

        // The input-token estimate is deliberately NOT computed here (#681).
        // It runs after the send returns, just past the auth-refresh retries.

        let retry_config = self.retry_config(&model);

        // Retry the stream connection establishment. Each retry is recorded
        // into `retry_notices` so the agent service can surface "⏳ Retry
        // N/M" to the user — the resilience used to be completely invisible.
        let notices = self.retry_notices.clone();
        let pname = self.name().to_string();
        let mut response = crate::utils::retry::retry_with_notify(
            || async {
                let body = self.encode_body(&openai_request)?;
                let response = self
                    .send_bounded(
                        &self.stream_client,
                        &self.send_url(),
                        self.headers_for(session)?,
                        &body,
                    )
                    .await?;

                tracing::debug!("OpenAI response status: {}", response.status());

                if !response.status().is_success() {
                    return Err(self.handle_error(response).await);
                }

                Ok(response)
            },
            &retry_config,
            |attempt, max, err| {
                if let Ok(mut v) = notices.lock() {
                    // provider/model in the notice so users can tell WHICH
                    // pair is retrying when several share a model name (#952)
                    v.push((
                        attempt,
                        max,
                        format!("{}/{} — {}", pname, model, retry_reason(err)),
                    ));
                }
            },
        )
        .await;

        // Resilient fallback: if the API rejected max_tokens / max_completion_tokens,
        // swap the fields and retry once.
        if let Err(ref e) = response
            && is_token_field_mismatch(&e.to_string())
        {
            tracing::warn!(
                "Token field mismatch for model {} (stream), retrying with swapped fields",
                model
            );
            openai_request.swap_token_fields();
            response = retry(
                || async {
                    let body = self.encode_body(&openai_request)?;
                    let r = self
                        .send_bounded(
                            &self.stream_client,
                            &self.send_url(),
                            self.headers_for(session)?,
                            &body,
                        )
                        .await?;
                    if !r.status().is_success() {
                        return Err(self.handle_error(r).await);
                    }
                    Ok(r)
                },
                &retry_config,
            )
            .await;
        }

        // Auth-refresh fallback: on 401/403, call the refresh hook once and
        // retry a single time. Mirrors the non-streaming path.
        if let Err(ref e) = response
            && Self::is_auth_error(e)
            && let Some(ref refresh) = self.auth_refresh_fn
        {
            tracing::warn!("{} stream auth error — refreshing and retrying", self.name);
            match refresh().await {
                Ok(()) => {
                    // Retry once with the new token. If it STILL returns 401,
                    // do NOT invalidate — same rationale as non-streaming path.
                    response = retry(
                        || async {
                            let body = self.encode_body(&openai_request)?;
                            let r = self
                                .send_bounded(
                                    &self.stream_client,
                                    &self.send_url(),
                                    self.headers_for(session)?,
                                    &body,
                                )
                                .await?;
                            if !r.status().is_success() {
                                return Err(self.handle_error(r).await);
                            }
                            Ok(r)
                        },
                        &retry_config,
                    )
                    .await;
                }
                Err(msg) => {
                    tracing::error!("{} stream auth refresh failed: {}", self.name, msg);
                    // Only invalidate when the refresh_token itself is dead
                    // (HTTP 400 from the token endpoint).
                    if msg.contains("HTTP 400")
                        && let Some(ref invalidate) = self.auth_invalidate_fn
                    {
                        tracing::warn!(
                            "{} refresh_token permanently dead — invalidating account",
                            self.name
                        );
                        invalidate();
                    }
                }
            }
        }
        let response = match response {
            Ok(r) => r,
            Err(e) => {
                // #2059 Auto: the chat endpoint refused the stream, so try the
                // Responses route once before surfacing the failure.
                if is_dialect_rejection_error(&e)
                    && let Some(fallback_request) = responses_fallback
                {
                    tracing::warn!(
                        "{}: chat endpoint rejected the stream ({}); retrying on the Responses API",
                        self.name(),
                        e
                    );
                    return self.stream_responses(fallback_request).await;
                }
                return Err(e);
            }
        };
        let tools_count = openai_request.tools.as_ref().map(|t| t.len()).unwrap_or(0);

        // Input-token estimate, computed AFTER the send (#681). It has two
        // consumers, and both belong after the response is in hand: the debug
        // line below, and the fallback usage figure emitted when a provider
        // reports no usage of its own (`[STREAM_USAGE]`). Computing it before
        // the request put a full tiktoken pass over every message and tool
        // schema on the connect path, burning guest CPU in the window the send
        // budget exists to protect.
        // Each message: content tokens + serialized tool_calls tokens + 4 overhead per message.
        let message_tokens: usize = openai_request
            .messages
            .iter()
            .map(|m| {
                let content = m
                    .content
                    .as_ref()
                    .map(|v| {
                        let s = v.as_str().unwrap_or("");
                        count_message_tokens(s)
                    })
                    .unwrap_or(4);
                let tool_calls = m
                    .tool_calls
                    .as_ref()
                    .map(|tc| count_tokens(&serde_json::to_string(tc).unwrap_or_default()))
                    .unwrap_or(0);
                content + tool_calls
            })
            .sum();
        let tool_schema_tokens = openai_request
            .tools
            .as_ref()
            .map(|tools| count_tokens(&serde_json::to_string(tools).unwrap_or_default()))
            .unwrap_or(0);
        let total_input_tokens = message_tokens + tool_schema_tokens;
        // #1147: budget against the provider's actual window (configured
        // context_window, then model heuristics) instead of a hardcoded 200k,
        // which misreported a configured-1M provider as "36% of 200k".
        let context_window = self.context_window(&model).unwrap_or(200_000);
        let context_pct =
            (total_input_tokens as f32 / context_window as f32 * 100.0).round() as u32;
        tracing::debug!(
            "[OI] stream request: ~{} input tokens ({}% of {}-token window) — {} messages, {} tool schemas",
            total_input_tokens,
            context_pct,
            context_window,
            openai_request.messages.len(),
            tools_count
        );

        // Extract OpenRouter cache status before consuming the stream
        let cache_status = response
            .headers()
            .get("x-openrouter-cache-status")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        // Provider request/trace ID (#1013): headers vanish once the stream
        // is consumed, so extract alongside cache_status. Logged when present
        // so incident forensics can match provider-side records.
        if let Some(rid) = provider_request_id(response.headers()) {
            tracing::info!(
                "{} streaming response header request_id: {rid}",
                self.name()
            );
        }

        if let Some(ref status) = cache_status {
            if status == "HIT" {
                tracing::info!("OpenRouter cache HIT (stream) — zero tokens billed");
            } else {
                tracing::debug!("OpenRouter cache MISS (stream)");
            }
        }

        // Parse Server-Sent Events stream - return Vec to emit multiple events like Anthropic
        let byte_stream = response.bytes_stream();
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

        // Accumulated state for a single streamed tool call
        #[derive(Debug, Clone, Default)]
        struct ToolCallAccum {
            id: String,
            name: String,
            arguments: String,
        }

        /// State persisted across SSE chunks via Arc<Mutex<_>>
        struct StreamState {
            emitted_message_start: bool,
            emitted_content_start: bool,
            /// Matching ContentBlockStop for the text block at index 0 has been emitted
            emitted_content_stop: bool,
            /// True once we've received real content via `delta` field
            seen_delta_content: bool,
            /// Index -> accumulated tool call
            tool_calls: std::collections::HashMap<usize, ToolCallAccum>,
            /// True while inside a stripped block (think/reasoning/tools-v2)
            inside_think: bool,
            /// Index into STRIP_CLOSE_TAGS for the currently active block
            active_close_tag: usize,
            /// Bytes consumed while inside_think is true (no close tag found).
            /// If this exceeds the threshold, we abandon filtering and pass
            /// content through — the model likely hallucinated an open tag
            /// without a matching close (e.g. `<!-- tools-v2: ...` with no `-->`).
            think_bytes_consumed: usize,
            /// Bytes withheld from the previous chunk because they could be
            /// the start of a split open tag (e.g. chunk ended with `<!`,
            /// which could become `<!--`). Prepended to the next chunk so
            /// the open-tag finder can match across chunk boundaries.
            think_carry: String,
            /// Stashed stop_reason from finish_reason chunk, emitted with
            /// the final usage-only chunk (MiniMax/OpenAI include_usage flow).
            pending_stop_reason: Option<crate::brain::provider::types::StopReason>,
            /// Rolling buffer of the leading visible content. Used to catch
            /// a hallucinated `{"tool_calls":...}` JSON envelope that
            /// dialagram-style providers stream 1-3 chars at a time through
            /// `delta.content`. While `leak_probe` is a strict prefix of the
            /// known leak markers, content is buffered rather than emitted.
            /// Once the accumulator diverges from every marker, the buffer
            /// is flushed through the normal think-tag filter. If the full
            /// marker is matched, `leak_active` is set and ALL subsequent
            /// content deltas are dropped for the remainder of the turn.
            leak_probe: String,
            leak_active: bool,
            /// Rolling accumulator of every emitted TextDelta across the turn.
            /// Scanned at `finish_reason` for `<tool_call>` / `<function=`
            /// blocks that local GGUF/MLX backends sometimes drop into
            /// content instead of the structured `tool_calls` field. Kept
            /// separate from `leak_probe` because the leak filter rejects
            /// text before emission, whereas this tracks what the consumer
            /// actually received.
            response_text_accum: String,
            /// Content suppressed by the tool-call marker filter (probe
            /// + subsequent suppressed deltas). Never reaches the consumer
            /// as text — scanned at `finish_reason` by `extract_text_tool_calls`
            /// and promoted to real `ContentBlock::ToolUse` events. This is
            /// how local-Qwen `tool_call:{...}` / `<tool_call>…` leaks become
            /// indistinguishable from cloud providers' structured tool calls
            /// in the TUI: the raw JSON never flows to the display layer.
            tool_capture_buffer: String,
            /// Once a tool-call marker has been seen in the stream we flip
            /// this to `true` and silently route all subsequent content
            /// deltas into `tool_capture_buffer` until the turn ends.
            /// Without this the model's `<tool_call>...</tool_call>` or
            /// `tool_call:{...}` text flashes in the TUI exactly the way
            /// cloud providers' structured `tool_calls` never would.
            tool_block_active: bool,
            /// Trailing bytes carried across chunks so the marker detector
            /// can match tokens that span chunk boundaries. Local models
            /// stream 1-3 bytes per SSE event, so "tool_call:" arrives as
            /// "too" + "l_" + "call:" — no single chunk contains the full
            /// marker. Holding back a short suffix bridges the split,
            /// mirroring `think_carry`'s role for `<think>` tag detection.
            marker_carry: String,
            /// TEXT_ACCUM journal (#36): number of TextDelta events emitted
            /// to the consumer this turn. Paired with `text_chars` and
            /// `response_text_accum` so the terminal `[STREAM_RECONCILE]`
            /// line can reconcile billed usage against what actually
            /// arrived — the discriminator for silent upstream/relay
            /// truncation, which inferhub's metadata-only retention can
            /// never settle after the fact.
            text_delta_count: usize,
            /// Char count of emitted text, maintained incrementally (O(1)
            /// per delta instead of recounting the growing accumulator).
            text_chars: usize,
            /// Reasoning content emitted this turn (think-tag promotions +
            /// `reasoning_content` field), chars and delta count — needed
            /// to reconcile `usage.output - usage.reasoning ≈ visible`.
            reasoning_chars: usize,
            reasoning_delta_count: usize,
            /// True once a chunk carrying `finish_reason` has been
            /// processed. Distinguishes provider-declared stops from the
            /// harness's EndTurn default (#694): `stop_source=default` at
            /// a terminal path means the provider never said why the
            /// stream ended.
            saw_finish_reason: bool,
            /// True once a MessageDelta carrying the provider's own token
            /// counts has been emitted. The `[DONE]` fallback below then
            /// stops re-asserting its local estimate: the consumer
            /// reconciles usage with `max()`, and a gross estimate would
            /// otherwise outrank the netted real input on every cached
            /// call and put the prefix back into the bill (#1636).
            reported_usage: bool,
        }

        // Tool-call marker filtering runs for ALL OpenAI-compatible providers.
        // Some cloud/custom providers also leak tool calls as text content
        // (e.g. `<tool_call>{...}`) instead of using the structured `tool_calls` field.
        // The filter suppresses raw markers from the TUI and captures them
        // for proper extraction at finish_reason.

        let state = std::sync::Arc::new(std::sync::Mutex::new(StreamState {
            emitted_message_start: false,
            emitted_content_start: false,
            emitted_content_stop: false,
            seen_delta_content: false,
            tool_calls: std::collections::HashMap::new(),
            inside_think: false,
            active_close_tag: 0,
            think_bytes_consumed: 0,
            think_carry: String::new(),
            pending_stop_reason: None,
            leak_probe: String::new(),
            leak_active: false,
            response_text_accum: String::new(),
            tool_capture_buffer: String::new(),
            tool_block_active: false,
            marker_carry: String::new(),
            text_delta_count: 0,
            text_chars: 0,
            reasoning_chars: 0,
            reasoning_delta_count: 0,
            saw_finish_reason: false,
            reported_usage: false,
        }));

        // Incremental UTF-8 carry: SSE chunks can split a multi-byte char in
        // half; decoding each chunk in isolation turns the orphaned bytes
        // into U+FFFD (the "resum��" class of mojibake).
        let mut utf8_carry = Utf8Carry::new();
        let event_stream = byte_stream
            .map(move |chunk_result| -> Vec<std::result::Result<StreamEvent, ProviderError>> {
                match chunk_result {
                    Err(e) => vec![Err(ProviderError::StreamError(e.to_string()))],
                    Ok(chunk) => {
                        // GRANULAR LOG: Raw SSE chunk. This fires once PER chunk —
                        // a firehose that floods debug-mode log files on every
                        // streamed response. Keep it at trace, not debug, so it's
                        // opt-in for deep diagnostics only.
                        let raw_text = utf8_carry.push(&chunk);
                        tracing::trace!("[STREAM_RAW] SSE chunk: {}", raw_text.chars().take(500).collect::<String>());
                        if raw_text.contains("tool_calls") {
                            tracing::trace!("[STREAM_RAW] SSE chunk with tool_calls: {}", raw_text.chars().take(500).collect::<String>());
                        }

                        let mut buf = buffer.lock().expect("SSE buffer lock poisoned");
                        buf.push_str(&raw_text);

                        let mut events = Vec::new();
                        let mut st = state.lock().expect("SSE state lock");

                        // Process complete lines (terminated by \n)
                        while let Some(newline_pos) = buf.find('\n') {
                            let line = buf[..newline_pos].trim().to_string();
                            buf.drain(..=newline_pos);

                            if let Some(json_str) = line.strip_prefix("data: ") {
                                if json_str == "[DONE]" {
                                    // Single-final-delta guard (#1738): the
                                    // inline-usage and usage-only-chunk paths set
                                    // reported_usage when they emit the final
                                    // MessageDelta+MessageStop pair. Reaching [DONE]
                                    // after one of those means the stream was already
                                    // finalized — pushing a second pair re-rendered
                                    // the same text block twice in the TUI (2026-09-25
                                    // zai: 604-char block duplicated) and carried a
                                    // bogus EndTurn default over the real stop reason.
                                    // Tools are not at risk: any terminal finish_reason
                                    // flushes st.tool_calls before a usage path can set
                                    // the flag, so the fallback flush below would be
                                    // empty by construction in this case.
                                    if st.reported_usage {
                                        tracing::info!(
                                            "[STREAM_RECONCILE] final delta already emitted (reported_usage=true) — suppressing duplicate [DONE] finalize (#1738)"
                                        );
                                        continue;
                                    }
                                    // Close the text block first, if one is still open,
                                    // so helpers.rs can finalize it before tool events.
                                    if st.emitted_content_start && !st.emitted_content_stop {
                                        events.push(Ok(StreamEvent::ContentBlockStop { index: 0 }));
                                        st.emitted_content_stop = true;
                                    }
                                    // Flush any accumulated tool calls before DONE.
                                    // Emit a matching ContentBlockStop after every Start so
                                    // helpers.rs fires ToolStarted/ToolCompleted progress
                                    // events to the TUI (otherwise the tool cards stay
                                    // visually "stuck" forever).
                                    for (_idx, accum) in st.tool_calls.drain() {
                                        let input = super::json_repair::parse_or_repair(&accum.arguments);
                                        tracing::info!(
                                            "[TOOL_EMIT] Flushing tool on DONE: id={}, name={}, args={}",
                                            accum.id, accum.name, &accum.arguments.chars().take(200).collect::<String>()
                                        );
                                        let tool_index = _idx + 1; // Offset to avoid collision with text block at index 0
                                        events.push(Ok(StreamEvent::ContentBlockStart {
                                            index: tool_index,
                                            content_block: ContentBlock::ToolUse {
                                                id: accum.id,
                                                name: accum.name,
                                                input,
                                            },
                                        }));
                                        events.push(Ok(StreamEvent::ContentBlockStop { index: tool_index }));
                                    }
                                    // A stream that reached `[DONE]` is COMPLETE. Emit the final
                                    // MessageDelta with a stop_reason — defaulting to EndTurn when
                                    // no finish_reason chunk set one (#694). qwen3.8-max-preview
                                    // ends some text turns without a parseable finish_reason, so
                                    // stop_reason stayed None and the tool loop treated a finished
                                    // response as unfinished → retried the SAME turn forever ("kept
                                    // thinking", response already complete on screen, uncancellable).
                                    let stop_reason = st
                                        .pending_stop_reason
                                        .take()
                                        .unwrap_or(crate::brain::provider::types::StopReason::EndTurn);
                                    // Usage-vs-received reconciliation (#36): what the consumer
                                    // actually got (chars/deltas/tail) against what the stream
                                    // declared. `[DONE]` carries no usage, so output/reasoning
                                    // are 0 here — the inline/usage-only paths carry real counts.
                                    // Sanitize the tail for logging: mid-turn text ends in paragraph
                                    // breaks (`\n\n` before a tool call) and the log writer does NOT
                                    // escape embedded newlines — a raw tail splits the log line and
                                    // orphans every field after it (caught in post-swap smoke).
                                    let reconc_tail: String = extract_sanitized_tail(&st.response_text_accum, 60);
                                    let stop_source = if st.saw_finish_reason { "provider" } else { "default" };
                                    tracing::info!(
                                        "[STREAM_RECONCILE] text_chars={}, text_deltas={}, reasoning_chars={}, reasoning_deltas={}, saw_finish_reason={}, stop_reason={:?}, stop_source={}, usage_input={}, usage_output=0, usage_reasoning=0, text_tail={}",
                                        st.text_chars,
                                        st.text_delta_count,
                                        st.reasoning_chars,
                                        st.reasoning_delta_count,
                                        st.saw_finish_reason,
                                        stop_reason,
                                        stop_source,
                                        total_input_tokens,
                                        reconc_tail
                                    );
                                    // The estimate is a last resort, not a second
                                    // opinion: this arm only runs when the
                                    // provider never reported usage — the #1738
                                    // guard above suppresses the whole finalize
                                    // otherwise, so the local estimate is all we
                                    // have (#36, #1636).
                                    let fallback_input = total_input_tokens as u32;
                                    tracing::info!(
                                        "[STREAM_USAGE] Final usage (fallback on DONE): input={}, output=0, stop_reason={:?}, provider_already_reported={}",
                                        fallback_input, stop_reason, st.reported_usage
                                    );
                                    events.push(Ok(StreamEvent::MessageDelta {
                                        delta: crate::brain::provider::types::MessageDelta {
                                            stop_reason: Some(stop_reason),
                                            stop_sequence: None,
                                        },
                                        usage: crate::brain::provider::types::TokenUsage {
                                            input_tokens: fallback_input,
                                            output_tokens: 0, ..Default::default() },
                                    }));
                                    events.push(Ok(StreamEvent::MessageStop));
                                    continue;
                                }

                                // Check for z.ai/provider-specific inline errors (HTTP 200 with error in body)
                                if let Ok(raw) = serde_json::from_str::<serde_json::Value>(json_str)
                                    && let Some(status_msg) = raw.pointer("/base_resp/status_msg").and_then(|v| v.as_str())
                                {
                                    let status_code = raw.pointer("/base_resp/status_code").and_then(|v| v.as_u64()).unwrap_or(0);
                                    if status_code != 0 {
                                        tracing::error!("[STREAM_ERROR] Provider returned inline error: code={}, msg={}", status_code, status_msg);
                                        events.push(Err(ProviderError::ApiError {
                                            status: status_code as u16,
                                            message: status_msg.to_string(),
                                            error_type: Some("provider_error".to_string()),
                                        }));
                                        continue;
                                    }
                                }

                                // Check for generic `{error: {message, type, code}}` mid-stream.
                                // DashScope/qwen emits these when quota is hit mid-flight, and
                                // OpenAI uses the same shape for policy/tos violations.
                                if let Ok(raw) = serde_json::from_str::<serde_json::Value>(json_str)
                                    && let Some(err_obj) = raw.get("error").and_then(|v| v.as_object())
                                {
                                    let message = err_obj.get("message").and_then(|v| v.as_str()).unwrap_or("stream error").to_string();
                                    let err_type = err_obj.get("type").and_then(|v| v.as_str()).map(|s| s.to_string());
                                    // Status can arrive as `code` (OpenAI) or `http_code`
                                    // (qwen portal), and qwen serializes it as a string.
                                    // Parse both, preferring whichever is non-zero.
                                    let read_status = |key: &str| -> u16 {
                                        err_obj
                                            .get(key)
                                            .and_then(|v| {
                                                v.as_u64().map(|n| n as u16).or_else(|| {
                                                    v.as_str().and_then(|s| s.parse::<u16>().ok())
                                                })
                                            })
                                            .unwrap_or(0)
                                    };
                                    let code = {
                                        let c = read_status("code");
                                        if c != 0 { c } else { read_status("http_code") }
                                    };
                                    tracing::error!("[STREAM_ERROR] Inline SSE error: type={:?}, code={}, msg={}", err_type, code, message);
                                    // Capacity saturation (#2270): the proxy answered 200 and then
                                    // reported a full upstream pool inline ("out of quota or cooling
                                    // down ...; retry shortly"). It rotates upstream providers on
                                    // every attempt, so this is transient and must ride the in-place
                                    // retry ladder instead of walking the fallback chain with zero
                                    // attempts. Hard quota phrases stay excluded from that matcher
                                    // and keep falling back immediately.
                                    let is_capacity_saturated =
                                        crate::brain::provider::error::is_capacity_saturation_signal(
                                            &message,
                                            err_type.as_deref(),
                                        );
                                    // Map 429/quota-style messages to RateLimitExceeded so the
                                    // fallback chain kicks in immediately instead of retrying.
                                    let msg_lc = message.to_lowercase();
                                    let is_rate_limit = !is_capacity_saturated
                                        && (code == 429
                                            || err_type.as_deref() == Some("rate_limit_exceeded")
                                            || msg_lc.contains("rate limit")
                                            || msg_lc.contains("quota"));
                                    // Qwen portal (and Cloudflare-fronted upstreams in general)
                                    // emits 529 / `overloaded_error` / 503 when the shared
                                    // cluster is temporarily saturated. It's the exact case
                                    // tool_loop's StreamError retry-then-fallback path exists
                                    // for: retry 5x with backoff, then swap to a healthy
                                    // provider. Mapping to StreamError routes it there —
                                    // mapping to ApiError{500} sent it to the catch-all that
                                    // bubbled straight to the TUI.
                                    let is_overloaded = is_capacity_saturated
                                        || code == 529
                                        || code == 503
                                        || err_type.as_deref() == Some("overloaded_error")
                                        || msg_lc.contains("overloaded")
                                        || msg_lc.contains("server cluster")
                                        || msg_lc.contains("high load");
                                    let pe = if is_rate_limit {
                                        ProviderError::RateLimitExceeded(message)
                                    } else if is_overloaded {
                                        ProviderError::StreamError(format!(
                                            "upstream overloaded ({}): {}",
                                            code, message
                                        ))
                                    } else {
                                        ProviderError::ApiError {
                                            status: if code == 0 { 500 } else { code },
                                            message,
                                            error_type: err_type,
                                        }
                                    };
                                    events.push(Err(pe));
                                    continue;
                                }

                                log_usage_keys(json_str);
                                match serde_json::from_str::<OpenAIStreamChunk>(json_str) {
                                    Ok(chunk) => {
                                        // Emit MessageStart on first chunk with id
                                        if !st.emitted_message_start && !chunk.id.is_empty() {
                                            st.emitted_message_start = true;
                                            // Response id from the first SSE chunk (#1013):
                                            // correlate with provider-side logs. Logged
                                            // before chunk.id moves into StreamMessage.
                                            tracing::info!(
                                                "OpenAI-compat streaming response id (first chunk): {}",
                                                chunk.id
                                            );
                                            let model = chunk.model.clone().unwrap_or_default();
                                            // Response id for provider-side correlation (#1013):
                                            // logged before chunk.id is moved into StreamMessage.
                                            tracing::info!(
                                                "OpenAI-compat streaming response id: {} model='{model}'",
                                                chunk.id
                                            );
                                            events.push(Ok(StreamEvent::MessageStart {
                                                message: crate::brain::provider::types::StreamMessage {
                                                    id: chunk.id,
                                                    model,
                                                    role: Role::Assistant,
                                                    usage: crate::brain::provider::types::TokenUsage {
                                                        input_tokens: 0,
                                                        output_tokens: 0, ..Default::default() },
                                                },
                                            }));
                                        }

                                        // Get content from delta or message (MiniMax uses message).
                                        // IMPORTANT: Some providers (LM Studio, etc.) send the FULL
                                        // response in the final chunk's `message` field while `delta`
                                        // is absent. If we already received content via delta, we must
                                        // NOT fall back to `message` or we'll duplicate the entire text.
                                        let delta_content = chunk.choices.first()
                                            .and_then(|c| c.delta.as_ref())
                                            .and_then(|d| d.content.as_ref())
                                            .cloned();
                                        let content = if delta_content.is_some() {
                                            if delta_content.as_ref().is_some_and(|s| !s.is_empty()) {
                                                st.seen_delta_content = true;
                                            }
                                            delta_content
                                        } else if !st.seen_delta_content {
                                            // Only use message field if we've never seen delta content
                                            // (MiniMax always uses message, standard providers don't)
                                            chunk.choices.first()
                                                .and_then(|c| c.message.as_ref())
                                                .and_then(|d| d.content.as_ref())
                                                .cloned()
                                        } else {
                                            None
                                        };

                                        // Get streaming tool_calls from delta or message
                                        let tool_calls = chunk.choices.first()
                                            .and_then(|c| c.delta.as_ref().or(c.message.as_ref()))
                                            .and_then(|d| d.tool_calls.as_ref());

                                        // Accumulate tool calls across chunks
                                        // OpenAI streaming sends: chunk1={index,id,type,name,args:""}, chunk2..N={index,args:"<fragment>"}
                                        if let Some(tc_list) = tool_calls {
                                            for tc_item in tc_list {
                                                let idx = tc_item.index;
                                                let accum = st.tool_calls.entry(idx).or_default();

                                                // First chunk for this index carries id + name
                                                if let Some(ref id) = tc_item.id
                                                    && !id.is_empty() {
                                                        accum.id = id.clone();
                                                    }
                                                if let Some(ref func) = tc_item.function {
                                                    if let Some(ref name) = func.name
                                                        && !name.is_empty() {
                                                            accum.name = name.clone();
                                                        }
                                                    // Append argument fragment
                                                    if let Some(ref args) = func.arguments {
                                                        accum.arguments.push_str(args);
                                                    }
                                                }

                                                tracing::trace!(
                                                    "[TOOL_ACCUM] idx={}, id={}, name={}, args_len={}, args_tail={}",
                                                    idx, accum.id, accum.name, accum.arguments.len(),
                                                    extract_sanitized_tail(&accum.arguments, 60)
                                                );
                                            }
                                        }

                                        // Check finish_reason — emit accumulated tool calls when done
                                        let finish_reason_str = chunk.choices.first()
                                            .and_then(|c| c.finish_reason.as_ref())
                                            // Some routes (cbcn/glm-5.3-flash, cb/gpt-5.6-*,
                                            // cb/kimi-k3) emit an EMPTY finish_reason between
                                            // content chunks — that is a continuation marker,
                                            // not a terminal reason (#105). Treating it as
                                            // Some("") flushed partial tool calls mid-stream
                                            // and could end the turn early. Ignore empty
                                            // reasons: [DONE] / the usage-only chunk remain
                                            // the terminal authorities.
                                            .filter(|r| !r.is_empty());

                                        // Flush accumulated tool calls on any terminal finish_reason.
                                        // Some providers (MiniMax) send "stop" even with tool_calls.
                                        if finish_reason_str.is_some() && !st.tool_calls.is_empty() {
                                                // Close the text block first if one is still open so
                                                // helpers.rs finalizes it before the tool blocks.
                                                if st.emitted_content_start && !st.emitted_content_stop {
                                                    events.push(Ok(StreamEvent::ContentBlockStop { index: 0 }));
                                                    st.emitted_content_stop = true;
                                                }
                                                // Emit all accumulated tool calls. Each Start MUST be
                                                // followed by a matching Stop so helpers.rs can fire
                                                // ToolStarted/ToolCompleted progress events — otherwise
                                                // the TUI tool cards stay visually stuck forever.
                                                for (idx, accum) in st.tool_calls.drain() {
                                                    // Repair partial args (network drop / timeout)
                                                    // instead of dropping the whole tool call.
                                                    let input = super::json_repair::parse_or_repair(&accum.arguments);
                                                    tracing::info!(
                                                        "[TOOL_EMIT] Emitting tool call: idx={}, id={}, name={}, args_len={}",
                                                        idx, accum.id, accum.name, accum.arguments.len()
                                                    );
                                                    let tool_index = idx + 1; // Offset by 1 to avoid collision with text block at index 0
                                                    events.push(Ok(StreamEvent::ContentBlockStart {
                                                        index: tool_index,
                                                        content_block: ContentBlock::ToolUse {
                                                            id: accum.id,
                                                            name: accum.name,
                                                            input,
                                                        },
                                                    }));
                                                    events.push(Ok(StreamEvent::ContentBlockStop { index: tool_index }));
                                                }
                                            }

                                        // Emit text content, filtering <think>...</think> reasoning blocks
                                        if let Some(ref c) = content {
                                            // Defensive: qwen-3.6-plus-thinking (and other thinking
                                            // models) occasionally hallucinate OpenAI tool_call
                                            // envelopes as plain text content (e.g.
                                            // `{"tool_calls":[{"id"...`). The real tool calls come
                                            // through `delta.tool_calls` — any such text in
                                            // `delta.content` is pure noise. Drop it outright.
                                            //
                                            // Detection runs over the ROLLING accumulator because
                                            // dialagram-style providers stream 1-3 chars per delta
                                            // and the single-chunk prefix check would never fire.
                                            // Strategy: while the trimmed accumulator is still a
                                            // strict prefix of a known leak marker, buffer the
                                            // content. Once the full marker matches → drop the
                                            // buffer and flip leak_active for the rest of the turn.
                                            // If the accumulator diverges → flush the buffer
                                            // through the normal think-tag filter.
                                            const LEAK_MARKERS: &[&str] = &[
                                                "{\"tool_calls\"",
                                                "{ \"tool_calls\"",
                                            ];

                                            // Once we know the turn is leaking, drop everything.
                                            let drop_all = st.leak_active;
                                            let mut to_emit: Option<String> = None;
                                            if drop_all {
                                                if !c.is_empty() {
                                                    tracing::debug!(
                                                        "[STREAM_FILTER] Suppressing {} chars of content during active tool_calls leak",
                                                        c.len()
                                                    );
                                                }
                                            } else {
                                                st.leak_probe.push_str(c);
                                                let probe_trimmed = st.leak_probe.trim_start();
                                                let full_match = LEAK_MARKERS
                                                    .iter()
                                                    .any(|m| probe_trimmed.starts_with(m));
                                                // Bare top-level array of OpenAI envelopes
                                                // (`[{"id":"call_..."}]`) — seen 2026-05-16 with
                                                // qwen-3.6-max-preview-thinking double-emitting
                                                // alongside structured delta.tool_calls.
                                                let bare_array_state =
                                                    classify_bare_tool_array(probe_trimmed);
                                                if full_match
                                                    || bare_array_state == BareToolArrayMatch::Full
                                                {
                                                    tracing::warn!(
                                                        "[STREAM_FILTER] Dropping hallucinated tool-call JSON across accumulated content ({} chars buffered, bare_array={})",
                                                        st.leak_probe.len(),
                                                        bare_array_state == BareToolArrayMatch::Full
                                                    );
                                                    st.leak_active = true;
                                                    st.leak_probe.clear();
                                                } else {
                                                    // Is the trimmed accum still a viable prefix
                                                    // of some marker? If so, keep buffering.
                                                    // Also allow buffering when the accum is empty
                                                    // after trim (pure leading whitespace).
                                                    let still_prefix = probe_trimmed.is_empty()
                                                        || LEAK_MARKERS.iter().any(|m| {
                                                            m.starts_with(probe_trimmed)
                                                        })
                                                        || bare_array_state == BareToolArrayMatch::Prefix;
                                                    if still_prefix {
                                                        // Keep withholding — bounded by marker len.
                                                        // Safety cap so a legitimate response that
                                                        // happens to start with "{" or "[" doesn't
                                                        // get buffered forever. Bare-array prefixes
                                                        // can reach ~50 bytes with pretty-printed
                                                        // whitespace, so cap at 128.
                                                        if st.leak_probe.len() > 128 {
                                                            to_emit = Some(std::mem::take(&mut st.leak_probe));
                                                        }
                                                    } else {
                                                        // Diverged — flush the buffer as normal content.
                                                        to_emit = Some(std::mem::take(&mut st.leak_probe));
                                                    }
                                                }
                                            }

                                            if let Some(ref flushed) = to_emit {
                                                let (mut inside, mut close_idx, mut consumed) =
                                                    (st.inside_think, st.active_close_tag, st.think_bytes_consumed);
                                                let mut carry = std::mem::take(&mut st.think_carry);
                                                let (filtered, reasoning_from_think) = filter_think_tags(
                                                    flushed,
                                                    &mut inside,
                                                    &mut close_idx,
                                                    &mut consumed,
                                                    &mut carry,
                                                );
                                                st.inside_think = inside;
                                                st.active_close_tag = close_idx;
                                                st.think_bytes_consumed = consumed;
                                                st.think_carry = carry;

                                                // Content inside `<think>…</think>` is promoted to a
                                                // `ReasoningDelta` event so downstream treats it like
                                                // the OpenAI `reasoning_content` field — helpers.rs
                                                // accumulates it into `reasoning_buf`, emits a
                                                // `ReasoningChunk` progress event the TUI renders as
                                                // live "Thinking…" text, and lands it as `details` on
                                                // the final `DisplayMessage`. Without this, models
                                                // like Qwen that emit `<think>` inline would have
                                                // their thinking silently discarded.
                                                if !reasoning_from_think.is_empty() {
                                                    if !st.emitted_content_start {
                                                        st.emitted_content_start = true;
                                                        events.push(Ok(StreamEvent::ContentBlockStart {
                                                            index: 0,
                                                            content_block: ContentBlock::Text { text: String::new() },
                                                        }));
                                                    }
                                                    events.push(Ok(StreamEvent::ContentBlockDelta {
                                                        index: 0,
                                                        delta: ContentDelta::ReasoningDelta {
                                                            text: reasoning_from_think,
                                                        },
                                                    }));
                                                }

                                                // For local providers (llama.cpp/MLX/etc.) the model often
                                                // leaks tool calls as content text. Partition the filtered
                                                // text at the first tool-call marker: everything before
                                                // stays visible (legitimate intermediate text like
                                                // "I'll check git status. "), everything from the marker
                                                // onwards is silently captured and promoted to a
                                                // structured `ContentBlock::ToolUse` at `finish_reason`.
                                                // Once the capture flag flips, subsequent deltas never
                                                // reach the consumer as text — matching the UX of every
                                                // other provider which emits structured tool_calls.
                                                //
                                                // Cross-chunk handling: local models stream 1-3 bytes per
                                                // SSE event, so "tool_call:" arrives as "too" + "l_" +
                                                // "call:" and no individual chunk contains the full
                                                // marker. Before scanning, prepend `marker_carry` (bytes
                                                // held back from the previous chunk's trailing prefix).
                                                // After scanning, hold back any suffix that still looks
                                                // like the start of a marker so the next chunk can close
                                                // the match.
                                                // Only match actual tool-call invocation formats.
                                                // JSON field names like "tool_calls" are NOT markers
                                                // — they appear in source code and cause false positives
                                                // that bleed raw content into the TUI.
                                                // Qwen 3 / DeepSeek emit a SentencePiece-style token
                                                // family `<|tool▁calls_section_begin|>` /
                                                // `<|tool▁call_begin|>` ... `<|tool▁call_end|>` /
                                                // `<|tool▁calls_section_end|>` (the U+2581 "Lower
                                                // One Eighth Block" between "tool" and the rest is
                                                // the SP word-boundary char — visually it looks
                                                // like an underscore in most fonts, which is how
                                                // it leaked into Telegram/TUI as plausible-looking
                                                // text and stayed unnoticed). Qwen 3 also degrades
                                                // into emitting hundreds of `<|tool▁calls_section_end|>`
                                                // tokens when looping; without these markers the
                                                // stream filter passed them through as content
                                                // until the repetition guard killed the stream
                                                // (after ~7 KB had already shipped). Capture both
                                                // the U+2581 form (real model output) and the ASCII
                                                // `_` variant emitted by some quantizations.
                                                const TOOL_MARKERS: &[&str] = &[
                                                    "<tool_call>",
                                                    "<function=",
                                                    "<|tool\u{2581}calls_section_begin|>",
                                                    "<|tool\u{2581}call_begin|>",
                                                    "<|tool_calls_section_begin|>",
                                                    "<|tool_call_begin|>",
                                                ];
                                                let mut display_text: String = String::new();
                                                if st.tool_block_active {
                                                    // Already capturing — all further content is tool body.
                                                    st.tool_capture_buffer.push_str(&filtered);
                                                } else {
                                                    // Prepend carry so tokens split across chunk
                                                    // boundaries still match as one unit.
                                                    let mut working = std::mem::take(&mut st.marker_carry);
                                                    working.push_str(&filtered);

                                                    let first = TOOL_MARKERS
                                                        .iter()
                                                        .filter_map(|m| working.find(m).map(|p| (p, *m)))
                                                        .min_by_key(|(p, _)| *p);

                                                    if let Some((pos, marker)) = first {
                                                        let before: String = working[..pos].to_string();
                                                        let after = &working[pos..];
                                                        st.tool_capture_buffer.push_str(after);
                                                        st.tool_block_active = true;
                                                        display_text = before;
                                                        tracing::info!(
                                                            "[STREAM_FILTER] Tool-call marker {:?} detected — routing {} bytes to capture buffer",
                                                            marker, after.len()
                                                        );
                                                    } else {
                                                        // No full marker match. Hold back any trailing
                                                        // suffix that's a viable prefix of some marker
                                                        // so the next chunk can finish the match.
                                                        let tail_keep = tool_marker_prefix_len(
                                                            &working,
                                                            TOOL_MARKERS,
                                                        );
                                                        if tail_keep >= working.len() {
                                                            // Entire working string is a viable prefix
                                                            // — hold it all, emit nothing this tick.
                                                            st.marker_carry = working;
                                                        } else if tail_keep > 0 {
                                                            let split = working.len() - tail_keep;
                                                            display_text = working[..split].to_string();
                                                            st.marker_carry =
                                                                working[split..].to_string();
                                                        } else {
                                                            display_text = working;
                                                        }
                                                    }
                                                }

                                                if !display_text.is_empty() {
                                                    if !st.emitted_content_start {
                                                        st.emitted_content_start = true;
                                                        events.push(Ok(StreamEvent::ContentBlockStart {
                                                            index: 0,
                                                            content_block: ContentBlock::Text { text: String::new() },
                                                        }));
                                                    }

                                                    st.response_text_accum.push_str(&display_text);
                                                    st.text_delta_count += 1;
                                                    st.text_chars += display_text.chars().count();
                                                    tracing::trace!(
                                                        "[TEXT_ACCUM] text_len={}, text_delta_count={}, text_tail={}",
                                                        st.text_chars,
                                                        st.text_delta_count,
                                                        // Sanitized: raw tails carry embedded newlines
                                                        // that split log lines (see STREAM_RECONCILE note).
                                                        extract_sanitized_tail(&st.response_text_accum, 60)
                                                    );
                                                    events.push(Ok(StreamEvent::ContentBlockDelta {
                                                        index: 0,
                                                        delta: ContentDelta::TextDelta {
                                                            text: display_text,
                                                        },
                                                    }));
                                                } else if !st.emitted_content_start
                                                    && flushed.is_empty()
                                                    && !st.tool_block_active
                                                {
                                                    st.emitted_content_start = true;
                                                    events.push(Ok(StreamEvent::ContentBlockStart {
                                                        index: 0,
                                                        content_block: ContentBlock::Text { text: String::new() },
                                                    }));
                                                }
                                            }
                                        }

                                        // Extract reasoning_content (MiniMax/dialagram thinking process).
                                        // Providers like dialagram `qwen-3.6-plus-thinking` stream the entire
                                        // reasoning BEFORE any text content arrives. helpers.rs silently drops
                                        // ContentBlockDelta at an index with no matching ContentBlockStart, so
                                        // we must open the text block at index 0 first — even if nothing has
                                        // been written to it yet — so the ReasoningDelta is actually forwarded
                                        // to the TUI via the ReasoningChunk progress event.
                                        let reasoning = chunk.choices.first()
                                            .and_then(|c| c.delta.as_ref())
                                            .and_then(|d| d.reasoning_content.as_ref())
                                            .cloned();
                                        if let Some(rc) = reasoning && !rc.is_empty() {
                                            if !st.emitted_content_start {
                                                st.emitted_content_start = true;
                                                events.push(Ok(StreamEvent::ContentBlockStart {
                                                    index: 0,
                                                    content_block: ContentBlock::Text { text: String::new() },
                                                }));
                                            }
                                            st.reasoning_delta_count += 1;
                                            st.reasoning_chars += rc.chars().count();
                                            events.push(Ok(StreamEvent::ContentBlockDelta {
                                                index: 0,
                                                delta: ContentDelta::ReasoningDelta {
                                                    text: rc,
                                                },
                                            }));
                                        }

                                        // Emit MessageDelta when finish_reason is present.
                                        // Do NOT emit MessageStop here — providers that support
                                        // stream_options.include_usage (MiniMax, OpenAI) send a
                                        // final usage-only chunk AFTER this one. We handle
                                        // MessageStop on [DONE] or the usage-only chunk below.
                                        if let Some(reason) = finish_reason_str {
                                            // The provider declared why the stream ended —
                                            // distinguishes this stop from the harness's EndTurn
                                            // default at the terminal paths (#36, #694).
                                            st.saw_finish_reason = true;
                                            // If we were still withholding a leak-probe buffer
                                            // waiting to confirm/deny a tool_calls envelope and
                                            // the turn is ending without it ever matching, flush
                                            // the buffered content as legitimate text so short
                                            // responses that happen to begin with `{` aren't lost.
                                            if !st.leak_active && !st.leak_probe.is_empty() {
                                                let flushed = std::mem::take(&mut st.leak_probe);
                                                let (mut inside, mut close_idx, mut consumed) =
                                                    (st.inside_think, st.active_close_tag, st.think_bytes_consumed);
                                                let mut carry = std::mem::take(&mut st.think_carry);
                                                let (filtered, reasoning_from_think) = filter_think_tags(
                                                    &flushed,
                                                    &mut inside,
                                                    &mut close_idx,
                                                    &mut consumed,
                                                    &mut carry,
                                                );
                                                st.inside_think = inside;
                                                st.active_close_tag = close_idx;
                                                st.think_bytes_consumed = consumed;
                                                st.think_carry = carry;
                                                if !reasoning_from_think.is_empty() {
                                                    if !st.emitted_content_start {
                                                        st.emitted_content_start = true;
                                                        events.push(Ok(StreamEvent::ContentBlockStart {
                                                            index: 0,
                                                            content_block: ContentBlock::Text { text: String::new() },
                                                        }));
                                                    }
                                                    st.reasoning_delta_count += 1;
                                                    st.reasoning_chars += reasoning_from_think.chars().count();
                                                    events.push(Ok(StreamEvent::ContentBlockDelta {
                                                        index: 0,
                                                        delta: ContentDelta::ReasoningDelta {
                                                            text: reasoning_from_think,
                                                        },
                                                    }));
                                                }
                                                if !filtered.is_empty() {
                                                    if !st.emitted_content_start {
                                                        st.emitted_content_start = true;
                                                        events.push(Ok(StreamEvent::ContentBlockStart {
                                                            index: 0,
                                                            content_block: ContentBlock::Text { text: String::new() },
                                                        }));
                                                    }
                                                    st.response_text_accum.push_str(&filtered);
                                                    st.text_delta_count += 1;
                                                    st.text_chars += filtered.chars().count();
                                                    tracing::trace!(
                                                        "[TEXT_ACCUM] text_len={}, text_delta_count={}, text_tail={}",
                                                        st.text_chars,
                                                        st.text_delta_count,
                                                        // Sanitized: raw tails carry embedded newlines
                                                        // that split log lines (see STREAM_RECONCILE note).
                                                        extract_sanitized_tail(&st.response_text_accum, 60)
                                                    );
                                                    events.push(Ok(StreamEvent::ContentBlockDelta {
                                                        index: 0,
                                                        delta: ContentDelta::TextDelta { text: filtered },
                                                    }));
                                                }
                                            }
                                            // Fallback: scan accumulated text for tool-call blocks
                                            // a local GGUF/MLX backend dropped into content. Two
                                            // sources to check:
                                            //   - `tool_capture_buffer` — marker-suppressed content
                                            //     (primary source for local providers; display never
                                            //     saw it)
                                            //   - `response_text_accum` — what did reach display,
                                            //     as a safety net in case the partitioning missed a
                                            //     marker (e.g. split across chunk boundaries before
                                            //     the first marker match).
                                            // Only fires when the structured `tool_calls` path
                                            // produced nothing, so providers that already emit
                                            // proper tool_calls pay no cost beyond the substring check.
                                            let has_markers_in_accum = st.response_text_accum.contains("<tool_call>")
                                                || st.response_text_accum.contains("<function=")
                                                || st.response_text_accum.contains("tool_call:")
                                                || st.response_text_accum.contains("\"tool_calls\"")
                                                || st.response_text_accum.contains("\"id\":\"call_")
                                                || st.response_text_accum.contains("\"id\": \"call_");
                                            let has_capture = !st.tool_capture_buffer.is_empty();
                                            if st.tool_calls.is_empty() && (has_markers_in_accum || has_capture)
                                            {
                                                let mut combined = st.tool_capture_buffer.clone();
                                                if has_markers_in_accum {
                                                    // Any markers still left in the display accum
                                                    // get scanned too — prepend so positional order
                                                    // roughly matches the stream order.
                                                    let mut prefix = st.response_text_accum.clone();
                                                    prefix.push('\n');
                                                    prefix.push_str(&combined);
                                                    combined = prefix;
                                                }
                                                let (recovered, _cleaned) =
                                                    extract_text_tool_calls(&combined);
                                                if !recovered.is_empty() {
                                                    tracing::info!(
                                                        "Recovered {} streaming tool call(s) from text content (local-model fallback; capture_bytes={}, display_markers={})",
                                                        recovered.len(),
                                                        st.tool_capture_buffer.len(),
                                                        has_markers_in_accum,
                                                    );
                                                    // Close the text block first so helpers.rs
                                                    // finalizes it before the tool blocks arrive.
                                                    if st.emitted_content_start && !st.emitted_content_stop {
                                                        events.push(Ok(StreamEvent::ContentBlockStop { index: 0 }));
                                                        st.emitted_content_stop = true;
                                                    }
                                                    for (tc_idx, (name, input)) in recovered.into_iter().enumerate() {
                                                        let tool_index = tc_idx + 1;
                                                        events.push(Ok(StreamEvent::ContentBlockStart {
                                                            index: tool_index,
                                                            content_block: ContentBlock::ToolUse {
                                                                id: format!("call_text_{}", tc_idx),
                                                                name,
                                                                input,
                                                            },
                                                        }));
                                                        events.push(Ok(StreamEvent::ContentBlockStop { index: tool_index }));
                                                    }
                                                }
                                            }
                                            // Close the text block (no tool path above will have
                                            // handled this if there were no tool calls to flush).
                                            if st.emitted_content_start && !st.emitted_content_stop {
                                                events.push(Ok(StreamEvent::ContentBlockStop { index: 0 }));
                                                st.emitted_content_stop = true;
                                            }
                                            // `raw_input` stays GROSS: it is what the provider
                                            // reported and what the [STREAM_USAGE] receipt line
                                            // has always carried. `net_input` is what the ledger
                                            // gets, cached prefix removed (#1636).
                                            let (raw_input, raw_output, raw_cache_read, raw_cache_create, net_input, reported_cost) = if let Some(ref usage) = chunk.usage {
                                                (
                                                    usage.prompt_tokens.unwrap_or(0),
                                                    usage.completion_tokens.unwrap_or(0),
                                                    usage.effective_cache_read(),
                                                    usage.cache_creation_input_tokens.unwrap_or(0),
                                                    usage.net_input_tokens(),
                                                    usage.cost,
                                                )
                                            } else {
                                                (0, 0, 0, 0, 0, None)
                                            };
                                            let raw_reasoning = chunk
                                                .usage
                                                .as_ref()
                                                .map(|u| u.reasoning_tokens())
                                                .unwrap_or(0);

                                            let stop_reason = Some(match reason.as_str() {
                                                "stop" => crate::brain::provider::types::StopReason::EndTurn,
                                                "length" => crate::brain::provider::types::StopReason::MaxTokens,
                                                "tool_calls" | "function_call" => crate::brain::provider::types::StopReason::ToolUse,
                                                _ => crate::brain::provider::types::StopReason::EndTurn,
                                            });

                                            // If this chunk already carries real usage (some
                                            // providers inline it), emit immediately + stop.
                                            if raw_input > 0 || raw_output > 0 {
                                                let reconc_tail: String = extract_sanitized_tail(&st.response_text_accum, 60);
                                                let stop_source = if st.saw_finish_reason { "provider" } else { "default" };
                                                tracing::info!(
                                                    "[STREAM_RECONCILE] text_chars={}, text_deltas={}, reasoning_chars={}, reasoning_deltas={}, saw_finish_reason={}, stop_reason={:?}, stop_source={}, usage_input={}, usage_output={}, usage_reasoning={}, text_tail={}",
                                                    st.text_chars,
                                                    st.text_delta_count,
                                                    st.reasoning_chars,
                                                    st.reasoning_delta_count,
                                                    st.saw_finish_reason,
                                                    stop_reason,
                                                    stop_source,
                                                    raw_input,
                                                    raw_output,
                                                    raw_reasoning,
                                                    reconc_tail
                                                );
                                                tracing::info!(
                                                    "[STREAM_USAGE] Final usage (inline): input={}, output={}, cache_read={}, cache_create={}, input_net={}",
                                                    raw_input, raw_output, raw_cache_read, raw_cache_create, net_input
                                                );
                                                events.push(Ok(StreamEvent::MessageDelta {
                                                    delta: crate::brain::provider::types::MessageDelta {
                                                        stop_reason,
                                                        stop_sequence: None,
                                                    },
                                                    usage: crate::brain::provider::types::TokenUsage {
                                                        input_tokens: net_input,
                                                        output_tokens: raw_output,
                                                        reasoning_tokens: raw_reasoning,
                                                        cache_creation_tokens: raw_cache_create,
                                                        cache_read_tokens: raw_cache_read,
                                                        cost_usd: reported_cost,
                                                        ..Default::default()
                                                    },
                                                }));
                                                events.push(Ok(StreamEvent::MessageStop));
                                                st.reported_usage = true;
                                            } else {
                                                // Stash stop_reason — we'll emit the final MessageDelta
                                                // with real usage once the usage-only chunk arrives.
                                                st.pending_stop_reason = stop_reason;
                                            }
                                        }

                                        // Handle usage-only chunk: choices is empty, usage has
                                        // real token counts. MiniMax and OpenAI send this as
                                        // the final chunk when stream_options.include_usage=true.
                                        if chunk.choices.is_empty()
                                            && let Some(ref usage) = chunk.usage {
                                                // `input` stays GROSS for the guard and the
                                                // receipt line; `net_input` is what the ledger
                                                // gets, cached prefix removed (#1636).
                                                let input = usage.prompt_tokens.unwrap_or(0);
                                                let net_input = usage.net_input_tokens();
                                                let output = usage.completion_tokens.unwrap_or(0);
                                                let cache_read = usage.effective_cache_read();
                                                let cache_create = usage.cache_creation_input_tokens.unwrap_or(0);
                                                let reasoning = usage.reasoning_tokens();
                                                if input > 0 || output > 0 {
                                                    // The usage-only chunk is the provider's FINAL
                                                    // chunk — the turn is complete. Default to
                                                    // EndTurn when no finish_reason set a
                                                    // stop_reason, so a finished turn terminates
                                                    // instead of retrying forever
                                                    // (qwen3.8-max-preview, #694).
                                                    let final_stop_reason = st.pending_stop_reason.take().unwrap_or(
                                                        crate::brain::provider::types::StopReason::EndTurn,
                                                    );
                                                    let reconc_tail: String = extract_sanitized_tail(&st.response_text_accum, 60);
                                                    let stop_source = if st.saw_finish_reason { "provider" } else { "default" };
                                                    tracing::info!(
                                                        "[STREAM_RECONCILE] text_chars={}, text_deltas={}, reasoning_chars={}, reasoning_deltas={}, saw_finish_reason={}, stop_reason={:?}, stop_source={}, usage_input={}, usage_output={}, usage_reasoning={}, text_tail={}",
                                                        st.text_chars,
                                                        st.text_delta_count,
                                                        st.reasoning_chars,
                                                        st.reasoning_delta_count,
                                                        st.saw_finish_reason,
                                                        final_stop_reason,
                                                        stop_source,
                                                        input,
                                                        output,
                                                        reasoning,
                                                        reconc_tail
                                                    );
                                                    tracing::info!(
                                                        "[STREAM_USAGE] Final usage: input={}, output={}, cache_read={}, cache_create={}, reasoning={}, input_net={}",
                                                        input, output, cache_read, cache_create, reasoning, net_input
                                                    );
                                                    events.push(Ok(StreamEvent::MessageDelta {
                                                        delta: crate::brain::provider::types::MessageDelta {
                                                            stop_reason: Some(final_stop_reason),
                                                            stop_sequence: None,
                                                        },
                                                        usage: crate::brain::provider::types::TokenUsage {
                                                            input_tokens: net_input,
                                                            output_tokens: output,
                                                            reasoning_tokens: reasoning,
                                                            cache_creation_tokens: cache_create,
                                                            cache_read_tokens: cache_read,
                                                            cache_read_unreported: usage.cache_read_absent(),
                                                            cost_usd: usage.cost,
                                                            ..Default::default()
                                                        },
                                                    }));
                                                    events.push(Ok(StreamEvent::MessageStop));
                                                    st.reported_usage = true;
                                                }
                                        }
                                    }
                                    Err(e) => {
                                        let json_preview = json_str.chars().take(300).collect::<String>();
                                        tracing::warn!(
                                            "[STREAM_PARSE] Failed to parse chunk: {} | Raw: {}",
                                            e, json_preview
                                        );
                                    }
                                }
                            }
                        }

                        // ── Non-streaming fallback ──────────────────
                        // Some upstreams (OpenRouter Trinity, Venice) return a
                        // plain JSON blob instead of SSE. Detect and synthesize
                        // the same stream events the SSE parser would produce.
                        if events.is_empty()
                            && !st.emitted_message_start
                            && super::nonstream_compat::is_nonstream_response(&buf)
                            && let Some(synth) = super::nonstream_compat::synthesize_stream_events(&buf)
                        {
                            st.emitted_message_start = true;
                            st.emitted_content_start = true;
                            st.emitted_content_stop = true;
                            buf.clear();
                            events.extend(synth);
                        }

                        if events.is_empty() {
                            vec![Ok(StreamEvent::Ping)]
                        } else {
                            events
                        }
                    }
                }
            })
            .flat_map(futures::stream::iter);

        Ok(Box::pin(event_stream))
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn supports_vision(&self) -> bool {
        self.vision_model.is_some()
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn take_retry_notices(&self) -> Vec<(u32, u32, String)> {
        self.retry_notices
            .lock()
            .map(|mut v| std::mem::take(&mut *v))
            .unwrap_or_default()
    }

    fn base_url(&self) -> Option<&str> {
        Some(&self.base_url)
    }

    fn default_model(&self) -> &str {
        self.custom_default_model.as_deref().unwrap_or_else(|| {
            tracing::error!(
                "No default_model configured for provider '{}' — check config.toml",
                self.name
            );
            "MISSING_MODEL"
        })
    }

    fn supported_models(&self) -> Vec<String> {
        // Live-fetched / config-declared models win — they reflect what this
        // particular endpoint actually serves (e.g. dialagram routes qwen,
        // kimi, deepseek; bigmodel/zhipu serves glm-*; OpenRouter has its
        // own catalog). Without this override, every custom OpenAI-compatible
        // provider would advertise the OpenAI GPT list, causing helpers.rs:95
        // to either run a noisy no-op remap or mis-route to default_model.
        let mut models = if !self.configured_models.is_empty() {
            self.configured_models.clone()
        } else {
            vec![
                "gpt-4-turbo-preview".to_string(),
                "gpt-4".to_string(),
                "gpt-4-32k".to_string(),
                "gpt-3.5-turbo".to_string(),
                "gpt-3.5-turbo-16k".to_string(),
            ]
        };
        // The configured default is servable by construction even when the
        // catalog omits it — OpenRouter hides stealth/* models from the public
        // /models list (#1147). Without the union, every remap gate that
        // consults supported_models() (helpers.rs stream_complete,
        // fallback.rs, context.rs compaction, tool_loop.rs stale-pin guard
        // and feedback attribution, cron/scheduler.rs job validation) treats
        // the provider's own default as foreign and either "remaps" it to
        // itself with a WARN per turn or skips a validly configured cron job.
        let default = self.default_model();
        if default != "MISSING_MODEL" && !models.iter().any(|m| m == default) {
            models.push(default.to_string());
            models.sort();
        }
        models
    }

    async fn fetch_models(&self) -> Vec<String> {
        // Derive models URL from base_url (replace /chat/completions with /models)
        let models_url = self.base_url.replace("/chat/completions", "/models");

        #[derive(Deserialize)]
        struct ModelEntry {
            id: String,
        }
        #[derive(Deserialize)]
        struct ModelsResponse {
            data: Vec<ModelEntry>,
        }

        let headers = match self.headers() {
            Ok(h) => h,
            Err(_) => return self.supported_models(),
        };
        match self.client.get(&models_url).headers(headers).send().await {
            Ok(resp) if resp.status().is_success() => match resp.json::<ModelsResponse>().await {
                Ok(body) => {
                    let mut models: Vec<String> = body.data.into_iter().map(|m| m.id).collect();
                    models.sort();
                    if models.is_empty() {
                        return self.supported_models();
                    }
                    models
                }
                Err(_) => self.supported_models(),
            },
            _ => self.supported_models(),
        }
    }

    fn configured_context_window(&self) -> Option<u32> {
        self.configured_context_window
    }

    fn request_timeout(&self) -> Option<Duration> {
        self.request_timeout
    }

    fn stream_idle_timeout(&self) -> Option<Duration> {
        self.stream_idle_timeout
    }

    fn thinking_loop_timeout(&self) -> Option<u64> {
        self.thinking_loop_timeout
    }

    fn context_window(&self, model: &str) -> Option<u32> {
        // User-configured value takes priority over model-name heuristics
        if let Some(cw) = self.configured_context_window {
            return Some(cw);
        }
        // DeepSeek publishes a 1M window; without this an entry added with no
        // `context_window` fell through to the generic fallback and budgeted
        // against a fraction of the real capacity.
        if let Some(cw) = super::deepseek_reasoning::default_context_window(model) {
            return Some(cw);
        }
        let m = model.to_lowercase();
        // gpt-5 family
        if m.starts_with("gpt-5") {
            return Some(1_047_576); // 1M tokens
        }
        // gpt-6 family
        if m.starts_with("gpt-6") {
            return Some(1_047_576); // 1M tokens
        }
        // gpt-4.1 family
        if m.starts_with("gpt-4.1") {
            return Some(1_047_576); // 1M tokens
        }
        // o-series reasoning models
        if m.starts_with("o4") || m.starts_with("o3") {
            return Some(200_000);
        }
        if m.starts_with("o1") {
            return Some(200_000);
        }
        // gpt-4o family
        if m.starts_with("gpt-4o") {
            return Some(128_000);
        }
        match model {
            "gpt-4-turbo" | "gpt-4-turbo-preview" => Some(128_000),
            "gpt-4" => Some(8_192),
            "gpt-4-32k" => Some(32_768),
            "gpt-3.5-turbo" => Some(16_384),
            "gpt-3.5-turbo-16k" => Some(16_384),
            _ => None,
        }
    }

    fn calculate_cost(&self, model: &str, input_tokens: u32, output_tokens: u32) -> f64 {
        crate::usage::pricing::PricingConfig::load()
            .map(|cfg| cfg.calculate_cost(model, input_tokens, output_tokens))
            .unwrap_or(0.0)
    }
}

/// Returns true if this model requires `max_completion_tokens` instead of `max_tokens`.
/// Newer OpenAI models (gpt-4.1-*, gpt-5-*, gpt-6-*, o1-*, o3-*) reject `max_tokens`.
/// Qwen thinking models also need this — when `max_tokens` is sent, DashScope
/// treats it as a text-only cap and reasoning tokens eat from a separate (tiny)
/// default budget, causing the model to stop after a handful of output tokens.
pub(crate) fn uses_max_completion_tokens(model: &str) -> bool {
    let m = model.to_lowercase();
    m.starts_with("gpt-4.1")
        || m.starts_with("gpt-5")
        || m.starts_with("gpt-6")
        || m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
        || m.contains("thinking")
}

// ============================================================================
// OpenAI-compatible request/response types
// ============================================================================

#[derive(Debug, Clone, Serialize)]
pub(crate) struct OpenAIRequest {
    pub(crate) model: String,
    pub(crate) messages: Vec<OpenAIMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    /// Legacy token limit field — used by older OpenAI models (gpt-4o, gpt-3.5, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    /// New token limit field — required by newer OpenAI models (gpt-4.1-*, gpt-5-*, o1-*, o3-*)
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAITool>>,
    /// Tells the model whether/how to call tools. "auto" = model decides.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
    /// OpenRouter: request reasoning/thinking tokens in the response.
    #[serde(skip_serializing_if = "Option::is_none")]
    include_reasoning: Option<bool>,
    /// Kimi K3 reasoning effort (only `max` today). Set from the provider's
    /// reasoning setting when the model accepts it.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    /// Kimi K2.x thinking toggle: `{ "type": "enabled" | "disabled" }`.
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<serde_json::Value>,
    /// MiniMax M3: when true, reasoning goes to `reasoning_content` field
    /// instead of being embedded in `content` as `<think>` tags. Prevents
    /// reasoning from leaking into visible output (#667).
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_split: Option<bool>,
    /// DashScope: carry reasoning across turns instead of re-deriving it every
    /// turn. Sent on every Model Studio request (#1033).
    #[serde(skip_serializing_if = "Option::is_none")]
    preserve_thinking: Option<bool>,
    /// DashScope qwen hybrid families: the on/off thinking switch. Never sent
    /// to the tiered-effort family, which reads `reasoning_effort` instead
    /// (#1034); see `qwen_reasoning`.
    #[serde(skip_serializing_if = "Option::is_none")]
    enable_thinking: Option<bool>,
    /// z.ai: stream tool-call arguments as deltas instead of one batch at the
    /// end of generation (#1347); see `glm_reasoning`.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_stream: Option<bool>,
}

impl OpenAIRequest {
    /// Swap max_tokens ↔ max_completion_tokens for retry after a 400 error.
    fn swap_token_fields(&mut self) {
        let old_max = self.max_tokens.take();
        let old_completion = self.max_completion_tokens.take();
        self.max_tokens = old_completion;
        self.max_completion_tokens = old_max;
    }
}

/// Returns true if the error message indicates a max_tokens / max_completion_tokens mismatch.
///
/// Providers word this rejection differently and only some say "unsupported".
/// Scaleway answers `max_completion_tokens is limited to 16384 for <model>`,
/// which names the field but not the word, so the swap-and-retry above never
/// fired and the request failed outright (#1059). The phrasing is the only
/// thing that varies; the meaning is always "you sent the wrong token field".
///
/// Deliberately keyed on the field name plus a rejection signal rather than on
/// any single vendor's sentence. A provider-specific config switch would put
/// the burden on every user of every such endpoint to discover a setting for
/// something the response already states.
pub(crate) fn is_token_field_mismatch(msg: &str) -> bool {
    let m = msg.to_lowercase();
    if !(m.contains("max_tokens") || m.contains("max_completion_tokens")) {
        return false;
    }
    // "limited to" is a cap complaint, which providers emit both when the value
    // is too high AND when the wrong field was used. Swapping is safe either
    // way: the retry re-sends the same value under the other name, so a genuine
    // over-cap still fails and surfaces its own error rather than being masked.
    [
        "unsupported",
        "not supported",
        "limited to",
        "use max_completion_tokens",
        "instead",
    ]
    .iter()
    .any(|sig| m.contains(sig))
}

#[derive(Debug, Clone, Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OpenAIMessage {
    pub(crate) role: String,
    /// Either a plain string or an array of content parts (text + image_url).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) content: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_calls: Option<Vec<OpenAIToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_call_id: Option<String>,
    /// Thinking / reasoning text associated with an assistant message.
    /// Moonshot kimi (direct and via opencode.ai/zen/go) 400s on tool-call
    /// messages that omit this when thinking mode is on upstream —
    /// `"thinking is enabled but reasoning_content is missing in assistant
    /// tool call message at index N"`. OpenAI-native, Anthropic, and
    /// Zhipu ignore unknown fields, so echoing our stored Thinking blocks
    /// here is safe cross-provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OpenAIToolCall {
    pub(crate) id: String,
    pub(crate) r#type: String,
    pub(crate) function: OpenAIFunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OpenAIFunctionCall {
    pub(crate) name: String,
    pub(crate) arguments: String,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAITool {
    r#type: String,
    function: OpenAIFunction,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAIFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

/// Provider request/trace ID from response headers (#1013). DashScope-family
/// providers (ModelScope) send `x-request-id`; others vary. First present,
/// non-empty value wins. Logged when present, silent when absent.
pub(crate) fn provider_request_id(headers: &reqwest::header::HeaderMap) -> Option<String> {
    ["x-request-id", "request-id", "x-trace-id"]
        .into_iter()
        .find_map(|name| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAIResponse {
    id: String,
    model: String,
    choices: Vec<OpenAIChoice>,
    usage: OpenAIUsage,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAIChoice {
    message: OpenAIMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct OpenAIUsage {
    #[serde(rename = "prompt_tokens")]
    prompt_tokens: Option<u32>,
    #[serde(rename = "completion_tokens")]
    completion_tokens: Option<u32>,
    /// Anthropic-style cache tokens — passed through by OpenRouter for Anthropic models.
    #[serde(default)]
    cache_creation_input_tokens: Option<u32>,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
    /// OpenAI/DashScope-style cache hit reporting:
    /// `usage.prompt_tokens_details.cached_tokens`.
    #[serde(default)]
    prompt_tokens_details: Option<OpenAIPromptTokensDetails>,
    /// DeepSeek's own cache reporting: `usage.prompt_cache_hit_tokens`, the prompt
    /// tokens served from cache (#2136). Read next to the OpenAI field, so a
    /// DeepSeek call records its hits instead of zero.
    #[serde(default)]
    prompt_cache_hit_tokens: Option<u32>,
    /// Top-level `usage.cached_tokens`, used by some OpenAI-compatible providers
    /// (Moonshot, per third-party docs; not confirmed against a live response,
    /// #2140). Read only when no other cache field is present.
    #[serde(default)]
    cached_tokens: Option<u32>,
    /// OpenAI/DashScope-style reasoning token reporting:
    /// `usage.completion_tokens_details.reasoning_tokens`.
    #[serde(default)]
    completion_tokens_details: Option<OpenAICompletionTokensDetails>,
    /// Gateway-reported dollars for this call: OpenRouter's `usage.cost`
    /// (returned when the request opts into usage accounting), LiteLLM and
    /// similar proxies. Where present the ledger trusts it over the pricing
    /// table — it is what the provider actually charged (#1707).
    #[serde(default)]
    cost: Option<f64>,
}

impl OpenAIUsage {
    /// True when the response carried no cache-read field at all (#2139): the
    /// zero from `effective_cache_read` then means "not reported".
    pub(crate) fn cache_read_absent(&self) -> bool {
        self.cache_read_input_tokens.is_none()
            && self
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens)
                .is_none()
            && self.prompt_cache_hit_tokens.is_none()
            && self.cached_tokens.is_none()
    }

    /// Effective cache-read tokens, merging the Anthropic field and the
    /// OpenAI/DashScope `prompt_tokens_details.cached_tokens` field.
    pub(crate) fn effective_cache_read(&self) -> u32 {
        self.cache_read_input_tokens
            .or_else(|| {
                self.prompt_tokens_details
                    .as_ref()
                    .and_then(|d| d.cached_tokens)
            })
            .or(self.prompt_cache_hit_tokens)
            .or(self.cached_tokens)
            .unwrap_or(0)
    }

    /// Non-cached prompt tokens, the value [`TokenUsage::input_tokens`] is
    /// documented to hold.
    ///
    /// `prompt_tokens` on an OpenAI-compatible API is the GROSS prompt, cached
    /// prefix included, while the pricing table bills `cache_read_tokens`
    /// separately on top. Assigning the gross value charges every cached token
    /// twice, once at the full input rate and once at the cache-read rate, and
    /// counts it twice in the token total (#1636). Net it out here, at the
    /// provider boundary, so everything downstream sees the documented
    /// contract.
    ///
    /// Only the cache-READ prefix is subtracted. No receipt shows a compat
    /// provider folding `cache_creation_input_tokens` into `prompt_tokens`,
    /// and subtracting it on a guess would under-count instead.
    fn net_input_tokens(&self) -> u32 {
        self.prompt_tokens
            .unwrap_or(0)
            .saturating_sub(self.effective_cache_read())
    }

    /// Reasoning tokens from `completion_tokens_details.reasoning_tokens`.
    fn reasoning_tokens(&self) -> u32 {
        self.completion_tokens_details
            .as_ref()
            .and_then(|d| d.reasoning_tokens)
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
struct OpenAIPromptTokensDetails {
    #[serde(default)]
    cached_tokens: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct OpenAICompletionTokensDetails {
    #[serde(default)]
    reasoning_tokens: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAIStreamChunk {
    /// Optional: InferHub omits id on the final usage-only chunk.
    /// Without default, serde rejects the whole chunk (missing field
    /// id) and the real token counts never reach the ledger.
    #[serde(default)]
    id: String,
    model: Option<String>,
    /// Optional for the same reason: the usage-only chunk may omit
    /// choices entirely.
    #[serde(default)]
    choices: Vec<OpenAIStreamChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAIStreamChoice {
    delta: Option<OpenAIMessageDelta>,
    message: Option<OpenAIMessageDelta>,
    finish_reason: Option<String>,
}

/// Streaming tool call — fields are optional because OpenAI sends them
/// incrementally: first chunk has id/type/name, continuation chunks only
/// have index + argument fragments.
#[derive(Debug, Clone, Deserialize)]
struct StreamingToolCall {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<StreamingFunctionCall>,
}

#[derive(Debug, Clone, Deserialize)]
struct StreamingFunctionCall {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAIMessageDelta {
    content: Option<String>,
    #[serde(default, alias = "reasoning")]
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<StreamingToolCall>>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct OpenAIErrorResponse {
    pub(crate) error: OpenAIError,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct OpenAIError {
    pub(crate) message: String,
    #[serde(rename = "type")]
    pub(crate) error_type: Option<String>,
    /// Proxy-wrapped upstream errors. opencode.ai in particular hides the
    /// real backend error inside `error.metadata.raw` as a stringified
    /// JSON blob (`"{\"error\":{\"message\":\"...\",\"type\":\"...\"}}"`).
    /// Leaving it wrapped meant users only saw "Provider returned error".
    #[serde(default)]
    pub(crate) metadata: Option<OpenAIErrorMetadata>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct OpenAIErrorMetadata {
    #[serde(default)]
    pub(crate) raw: Option<String>,
    #[serde(default)]
    pub(crate) provider_name: Option<String>,
}

/// True when an OpenAI-compatible endpoint routes to Moonshot kimi
/// and therefore requires `reasoning_content` on assistant tool-call
/// messages. Matches both the direct Moonshot URL and opencode.ai's
/// zen/go proxy when the model name contains "kimi".
pub(crate) fn needs_reasoning_content_for(base_url: &str, model: &str) -> bool {
    let url = base_url.to_ascii_lowercase();
    let model = model.to_ascii_lowercase();
    url.contains("moonshot") || (url.contains("opencode.ai") && model.contains("kimi"))
}

/// True when the base_url/model pair round-trips reasoning as a separate
/// `reasoning_content` field and must never carry it inside `content`.
///
/// Covers Moonshot / opencode kimi (via [`needs_reasoning_content_for`]) and
/// the Qwen family — Model Studio / DashScope custom endpoints and the built-in
/// Qwen provider — whose `preserve_thinking` contract requires the full
/// reasoning returned as `reasoning_content`; concatenating it into `content`
/// is unsupported and makes the model leak chain-of-thought (#654).
pub(crate) fn preserves_thinking(base_url: Option<&str>, model: &str) -> bool {
    let url = base_url.unwrap_or("");
    needs_reasoning_content_for(url, model)
        || crate::brain::provider::qwen::looks_like_qwen_target(url, model)
}

/// Walk a proxy's nested error envelope to find the real upstream error.
/// Returns (message, error_type) after stripping the passthrough wrapper.
/// The `provider_name` (if present) is prepended so operators can tell
/// which backend — Moonshot AI, Alibaba, z.ai — actually failed.
pub(crate) fn unwrap_proxy_error(outer: &OpenAIError) -> (String, Option<String>) {
    let Some(ref metadata) = outer.metadata else {
        return (outer.message.clone(), outer.error_type.clone());
    };
    let Some(ref raw) = metadata.raw else {
        return (outer.message.clone(), outer.error_type.clone());
    };
    // opencode.ai stuffs a stringified JSON of the upstream error in `raw`.
    // Parse that to get the real message / type.
    let Ok(inner) = serde_json::from_str::<OpenAIErrorResponse>(raw) else {
        // Not JSON — just append the raw text so it's visible.
        let prefix = metadata
            .provider_name
            .as_deref()
            .map(|p| format!("[{}] ", p))
            .unwrap_or_default();
        return (
            format!("{}{}: {}", prefix, outer.message, raw),
            outer.error_type.clone(),
        );
    };
    let prefix = metadata
        .provider_name
        .as_deref()
        .map(|p| format!("[{}] ", p))
        .unwrap_or_default();
    // Prefer the inner (upstream) error — it has the actionable detail.
    (
        format!("{}{}", prefix, inner.error.message),
        inner
            .error
            .error_type
            .clone()
            .or_else(|| outer.error_type.clone()),
    )
}

/// Extracts up to `max_chars` characters from the tail of `text` and sanitizes
/// newlines (`\n` -> `\n`, `\r` -> `\r`) in a single pass without intermediate string allocations.
pub(crate) fn extract_sanitized_tail(text: &str, max_chars: usize) -> String {
    if text.is_empty() || max_chars == 0 {
        return String::new();
    }
    let byte_offset = match text.char_indices().rev().take(max_chars).last() {
        Some((idx, _)) => idx,
        None => 0,
    };
    let tail_slice = &text[byte_offset..];
    let mut out = String::with_capacity(tail_slice.len());
    for c in tail_slice.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

/// Pydantic / FastAPI default 422 body shape. Used by Unsloth Studio and
/// other Python-backend wrappers in front of inference engines.
#[derive(Debug, Clone, Deserialize)]
struct PydanticValidationError {
    detail: Vec<PydanticDetail>,
}

#[derive(Debug, Clone, Deserialize)]
struct PydanticDetail {
    loc: Vec<serde_json::Value>,
    msg: String,
    #[serde(rename = "type")]
    _kind: Option<String>,
}

/// Best-effort check: does this base URL belong to Unsloth Studio's FastAPI
/// wrapper rather than the raw llama-server the Studio spawns internally?
/// We only need this accurate enough to produce a helpful error hint — a
/// false positive just means the hint shows for a non-Unsloth 422, which
/// still contains the raw pydantic details so the operator can tell.
fn is_unsloth_studio_url(url: &str) -> bool {
    // Unsloth Studio picks a port the user configures via `-p`, not a
    // fixed one. The only reliable marker we have is: it's a localhost
    // URL that historically returns 422 with pydantic `detail` on
    // OpenAI-style bodies. If we wanted a positive signal we'd need to
    // probe the server — for now treat any local base_url as a candidate
    // since the hint is harmless elsewhere.
    let lower = url.to_ascii_lowercase();
    lower.contains("localhost") || lower.contains("127.0.0.1")
}

/// Incremental UTF-8 decoder for SSE byte chunks. Network chunks can split a
/// multi-byte character in half; decoding each chunk in isolation replaces
/// the orphaned bytes with U+FFFD (the "resum??"/"????" mojibake class).
/// This carries the incomplete trailing bytes to the next chunk instead.
pub(crate) struct Utf8Carry {
    pending: Vec<u8>,
}

impl Utf8Carry {
    pub(crate) fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    /// Feed raw bytes, get back the decodable prefix. Incomplete trailing
    /// bytes are held for the next call; genuinely invalid bytes become a
    /// single U+FFFD each (same visible result as `from_utf8_lossy` for
    /// bytes that are truly garbage, without punishing split characters).
    pub(crate) fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(s) => {
                    out.push_str(s);
                    self.pending.clear();
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    if valid > 0 {
                        // Bytes before valid_up_to are guaranteed valid UTF-8.
                        out.push_str(&String::from_utf8_lossy(&self.pending[..valid]));
                        self.pending.drain(..valid);
                    }
                    match e.error_len() {
                        // None: incomplete tail — carry it to the next chunk.
                        None => break,
                        // Some(n): n bytes are genuinely invalid — substitute.
                        Some(n) => {
                            out.push('\u{FFFD}');
                            self.pending.drain(..n);
                        }
                    }
                }
            }
        }
        out
    }
}

/// Debug-log the key names of a response's `usage` object, and of any object
/// inside it, so the cache-read field a provider actually sends can be read
/// from normal traffic (#2152). Never logs a value or any prompt content.
fn log_usage_keys(body: &str) {
    if !tracing::enabled!(tracing::Level::DEBUG) || !body.contains("\"usage\"") {
        return;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return;
    };
    let Some(usage) = value.get("usage").and_then(|u| u.as_object()) else {
        return;
    };
    let mut keys: Vec<String> = Vec::new();
    for (key, val) in usage {
        keys.push(key.clone());
        if let Some(inner) = val.as_object() {
            keys.extend(inner.keys().map(|sub| format!("{key}.{sub}")));
        }
    }
    tracing::debug!(usage_keys = ?keys, "provider usage object key names");
}
