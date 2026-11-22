//! Mermaid diagram rendering for Telegram rich messages (#1044).
//!
//! Model output frequently carries ```mermaid fences. They are embedded via
//! `sendRichMessage`: the PRIMARY path is the markdown input mode plus the
//! Bot API 10.2 `media` field (`tg://photo?id=` references), which keeps any
//! tables in the message native; the HTML input mode (`<img>`) is the
//! fallback for servers without the `media` field. A broken image URL makes
//! the whole send fail with `RICH_MESSAGE_PHOTO_NO_MEDIA_FOUND`, so before
//! delivery each fence is pre-validated against the renderer (mermaid.ink):
//! the render's PNG bytes are fetched by us and uploaded via multipart
//! (`attach://`), so Telegram never fetches a third-party URL. A request
//! ladder keeps the render inside Telegram's photo box (natural size first,
//! then a proportional 1200px width clamp); anything else degrades to a
//! legible failure block (the renderer's error note plus the original
//! source) instead of killing the message. Pre-validation never panics or
//! hangs; failure paths yield [`MermaidResult::Failed`].

use super::ast::Block;
pub use super::ast::MermaidResult;
use crate::channels::telegram::markdown::escape_html;
use futures::FutureExt;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Base URL of the mermaid.ink image renderer. The diagram source is
/// base64url-appended. NOTE: this sends the diagram text to a third party.
const MERMAID_INK_BASE: &str = "https://mermaid.ink/img/";

/// Dedicated vector endpoint for a diagram source (#189 Leg 4). Same base64url
/// payload as [`ink_url`], over `https://mermaid.ink/svg/` — the endpoint
/// serves a real `image/svg+xml`. Build-only URL: the browser does the fetch,
/// this host never touches mermaid.ink for it.
pub(crate) const MERMAID_INK_SVG_BASE: &str = "https://mermaid.ink/svg/";

/// Query parameters appended to every mermaid.ink render request.
///
/// Natural-size PNG request: no width/scale overrides, so mermaid.ink
/// returns the diagram at its intrinsic Chromium/ELK render size (the
/// 44-node stress case: 1611×3727). The previous hi-res override
/// (`width=1600&scale=2`) doubled that to 3200×7404 — 10604 combined px,
/// past Telegram's photo box — and Telegram refused the photo. The render
/// size is a property of the request we send, not of the diagram.
const MERMAID_INK_PARAMS: &str = "?type=png";

/// Ladder rung 2: same renderer, layout clamped to a proportional 1200px
/// width. `width` scales the render proportionally (measured: the stress
/// case goes 1611×3727 → 1200×2776), so a single clamp rung covers any
/// realistic aspect ratio without a local redraw.
const MERMAID_INK_CLAMP_PARAMS: &str = "?type=png&width=1200";

/// Telegram rejects photos whose width+height exceeds this combined budget
/// (measured live, #1238: 3200×7404 refused, 1611×3727 accepted).
const PHOTO_MAX_TOTAL_DIMS: f32 = 9_600.0;

/// Total budget for one renderer fetch — connect, headers AND body (#189).
/// The body is the expensive leg: live renders measured 12 KB–140 KB
/// (2026-09-12), so this must cover a slow stream rather than only a slow
/// handshake. A stalled download degrades to a legible failure block.
pub(crate) const PREVALIDATE_TIMEOUT_SECS: u64 = 30;

/// Connect-phase budget, split out of the total (#189). A renderer that will
/// not accept a connection within this window is down, and spending the whole
/// body budget waiting for a handshake wastes the ladder. Before the split a
/// single 10s total let a slow body masquerade as an unreachable host.
pub(crate) const PREVALIDATE_CONNECT_TIMEOUT_SECS: u64 = 5;

/// One bounded re-fetch after a transient body failure (#189). By then the
/// response has already passed the `2xx + image/*` check, so the render
/// exists server-side and the retry is a cheap re-read of bytes we know are
/// there — one network wobble should not cost the picture.
const BODY_RETRY_DELAY_MS: u64 = 300;

/// Cap on how much of the renderer's error body we surface, so a huge HTML
/// error page can't blow up the message.
const ERROR_NOTE_MAX_CHARS: usize = 400;

/// One media reference embedded via the markdown `media` field (#1044).
/// `id` matches the `tg://photo?id=<id>` reference in the markdown text.
///
/// Exactly one of the payload sources is set:
/// - `url`: the mermaid.ink renderer image URL Telegram fetches server-side
///   (the legacy, network-dependent path).
/// - `bytes`: the pre-validated mermaid.ink PNG bytes, uploaded to Telegram
///   via multipart as `attach://<id>` — the active delivery mode; Telegram
///   never touches a third-party URL.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MediaEntry {
    pub(crate) id: String,
    pub(crate) url: Option<String>,
    pub(crate) bytes: Option<Vec<u8>>,
    /// Which Telegram media type this entry carries (#1918). One message's
    /// `media` array is all `photo` OR all `document` per family, and the
    /// request builder stamps the literal from this field instead of
    /// hardcoding `"photo"`.
    pub(crate) kind: MediaKind,
    /// The file's name for a `document` entry, sent as the multipart part
    /// filename so Telegram shows it as the document's title. `None` for
    /// `photo` entries, whose filename is fixed by the upload path.
    pub(crate) name: Option<String>,
}

/// The Telegram media type a [`MediaEntry`] carries (#1918). The wire value
/// is produced by [`MediaKind::as_str`], the single home of the literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum MediaKind {
    /// A photo: the mermaid diagrams this module rendered before #1918.
    #[default]
    Photo,
    /// A generic file: the local-file family's documents (#1918).
    Document,
}

impl MediaKind {
    /// The Telegram `type` literal for this media kind.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            MediaKind::Photo => "photo",
            MediaKind::Document => "document",
        }
    }
}

/// A located ```mermaid fence in the source markdown. `start` is the byte
/// offset of the opening fence line's first byte; `end` is the byte offset
/// just past the closing fence line (including its terminator). Replacing
/// `text[start..end]` swaps the fence without touching the rest.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MermaidFence {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) source: String,
}

/// Encode `input` as base64url (RFC 4648 §5, no padding), the alphabet
/// mermaid.ink requires. Standard base64 (`+`, `/`) returns 404 there.
pub(crate) fn base64url(input: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input.as_bytes())
}

/// Whether a fence body (the text between opening and closing ``` lines)
/// starts like a mermaid diagram. Used to classify *untagged* fences, whose
/// info string is empty: models frequently emit diagrams as ```graph TD ...
/// without the `mermaid` tag. The first non-blank, non-comment (`%%`) line
/// must start with a known diagram opener; `graph` additionally requires a
/// direction word (`TD`/`TB`/`BT`/`LR`/`RL`) so DOT-style `graph G {` and
/// similar foreign notations are not misclassified.
pub(crate) fn looks_like_mermaid_source(source: &str) -> bool {
    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        let mut words = line.split_whitespace();
        let head = words.next().unwrap_or("").to_ascii_lowercase();
        return match head.as_str() {
            "graph" => words.next().is_some_and(|d| {
                matches!(
                    d.to_ascii_lowercase().as_str(),
                    "td" | "tb" | "bt" | "lr" | "rl"
                )
            }),
            "flowchart" | "sequencediagram" | "classdiagram" | "classdiagram-v2"
            | "statediagram" | "statediagram-v2" | "erdiagram" | "journey" | "gantt" | "pie"
            | "quadrantchart" | "requirementdiagram" | "gitgraph" | "mindmap" | "timeline"
            | "zenuml" | "sankey-beta" | "xychart-beta" | "block-beta" | "packet-beta"
            | "architecture-beta" => true,
            _ => false,
        };
    }
    false
}

/// Whether `text` contains a fence that should render as a mermaid diagram:
/// either tagged ```mermaid, or untagged with mermaid-shaped content
/// ([`looks_like_mermaid_source`]). A fast line-scan used to gate the richer
/// (async) render path.
pub(crate) fn has_mermaid_fence(text: &str) -> bool {
    let mut in_fence = false;
    let mut tagged_mermaid = false;
    // Whether the OPENING fence carried no info string. Content
    // classification applies to bare fences only, so the opening tag is what
    // decides it — the closing line is bare almost every time and says
    // nothing about the block.
    let mut untagged = false;
    let mut body_start = 0usize;
    let mut pos = 0usize;
    for line in text.split_inclusive('\n') {
        let line_end = pos + line.len();
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("```") {
            // Tolerant pairing (14:23Z bug class): an info-carrying fence
            // line is ALWAYS an opener — it implicitly closes any open
            // block. Only a BARE fence line closes cleanly. A stray bare
            // fence before ```mermaid used to pair with the tagged opener,
            // desyncing the machine so the real diagram never located and
            // raw fences shipped.
            let info = rest.trim();
            if in_fence {
                let body = text[body_start..pos].trim_end_matches('\n');
                if tagged_mermaid || (untagged && looks_like_mermaid_source(body)) {
                    return true;
                }
            }
            in_fence = true;
            tagged_mermaid = info.eq_ignore_ascii_case("mermaid");
            untagged = info.is_empty();
            body_start = line_end;
        }
        pos = line_end;
    }
    false
}

/// Locate every mermaid fence in `text`, returning byte ranges and the
/// diagram source between the fences. Consistent with [`has_mermaid_fence`]:
/// a fence qualifies when its info string trims to `mermaid`
/// (case-insensitive), or is empty and the body starts like a diagram
/// ([`looks_like_mermaid_source`]); either way it is closed by the next
/// bare ``` line.
pub(crate) fn find_mermaid_fences(text: &str) -> Vec<MermaidFence> {
    let mut fences = Vec::new();
    let mut in_fence = false;
    let mut is_mermaid = false;
    // See `has_mermaid_fence`: classification keys off the OPENING info
    // string, never the closing line's.
    let mut untagged = false;
    let mut block_start = 0usize;
    let mut source_start = 0usize;
    let mut pos = 0usize;

    for line in text.split_inclusive('\n') {
        let line_end = pos + line.len();
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("```") {
            // Tolerant pairing — see `has_mermaid_fence`: info-carrying
            // lines always open (implicitly closing the previous block, at
            // THIS line's start so the opener is never swallowed); only a
            // bare fence closes, and its own line is part of the range.
            let info = rest.trim();
            if in_fence {
                let source = &text[source_start..pos];
                if is_mermaid || (untagged && looks_like_mermaid_source(source)) {
                    fences.push(MermaidFence {
                        start: block_start,
                        end: if info.is_empty() { line_end } else { pos },
                        source: source.to_string(),
                    });
                }
            }
            block_start = pos;
            source_start = line_end;
            is_mermaid = info.eq_ignore_ascii_case("mermaid");
            untagged = info.is_empty();
            in_fence = true;
        }
        pos = line_end;
    }
    fences
}

/// Whether `text` should be routed through the mermaid render path:
/// rich messages are enabled, the `mermaid_render` flag is on, and the text
/// actually contains a mermaid fence. Requires `rich_messages` because the
/// image can only be embedded via `sendRichMessage`.
pub(crate) fn should_render_mermaid(text: &str) -> bool {
    let tg = &crate::config::Config::current().channels.telegram;
    tg.rich_messages && tg.mermaid_render && has_mermaid_fence(text)
}

/// Full mermaid.ink embed/resolve URL for a diagram source: b64url
/// payload plus the natural-size PNG parameters ([`MERMAID_INK_PARAMS`]).
pub(crate) fn ink_url(source: &str) -> String {
    ink_url_params(source, MERMAID_INK_PARAMS)
}

/// The dedicated vector endpoint for a diagram source (#189 Leg 4).
/// Same base64url payload as [`ink_url`], over `https://mermaid.ink/svg/` —
/// the endpoint serves a real `image/svg+xml`. Build-only URL: the browser
/// does the fetch, the server never touches mermaid.ink for this.
pub(crate) fn ink_url_svg(source: &str) -> String {
    format!("{}{}", MERMAID_INK_SVG_BASE, base64url(source))
}

/// Same URL at an explicit parameter set — the ladder's clamp rung.
fn ink_url_params(source: &str, params: &str) -> String {
    format!("{}{}{}", MERMAID_INK_BASE, base64url(source), params)
}

/// Standard mobile viewport width and single-screen reading box dimensions.
/// Diagrams exceeding these dimensions or hitting width clamp rungs are considered capped/scaled (>= 1x).
pub(crate) const STANDARD_VIEWPORT_MAX_WIDTH: u32 = 1200;
pub(crate) const STANDARD_VIEWPORT_MAX_HEIGHT: u32 = 1800;

/// Whether a diagram's dimensions represent a capped or scaled-down render (>= 1x).
/// When width exceeds standard reading width (1200px), height exceeds comfortable
/// vertical reading bounds (1800px), or aspect ratio is extreme, Telegram mobile
/// scales the image down substantially, rendering small text illegible.
pub(crate) fn is_diagram_capped(w: u32, h: u32) -> bool {
    w > STANDARD_VIEWPORT_MAX_WIDTH
        || h > STANDARD_VIEWPORT_MAX_HEIGHT
        || (w > 0 && h > 0 && ((w as f32 / h as f32) > 2.5 || (h as f32 / w as f32) > 3.0))
}

/// Whether a render fits Telegram's photo box (width + height budget).
/// Pure f32 arithmetic with no renderer deps — kept out of the
/// feature-gated local-render module so every build can dimension-check
/// remote PNGs.
pub(crate) fn photo_fits(w: u32, h: u32) -> bool {
    (w as f32) + (h as f32) <= PHOTO_MAX_TOTAL_DIMS
}

/// Parse a PNG's IHDR header for its (width, height). Returns `None` for
/// non-PNG bodies and buffers too short to carry the header. Split out so
/// the oversize ladder is unit-testable without a network call.
pub(crate) fn png_dims(png: &[u8]) -> Option<(u32, u32)> {
    const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if png.len() < 24 || png[..8] != PNG_SIG {
        return None;
    }
    if png[12..16] != *b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    Some((w, h))
}

// ── Render cache (#37) ──
//
// The regen-nudge preflight resolves the same fence the delivery path
// resolves moments later, and mermaid.ink renders are deterministic:
// identical source, identical outcome. Caching the fresh outcome spares the
// renderer the duplicate round-trip — and spares the delivery path a second
// chance to hit a transient wobble on an already-proven source. Only
// deterministic outcomes are stored: rendered image bytes and parse errors.
// Transient `Failed` outcomes are never pinned, because the same source may
// render fine seconds later.

/// Freshness window for cached render outcomes (#37).
const RENDER_CACHE_TTL_SECS: u64 = 600;

/// Most cached render outcomes held; the oldest entry is evicted when the
/// cap is reached (#37).
const RENDER_CACHE_CAP: usize = 64;

/// Cached render outcomes keyed by diagram source: insertion time plus the
/// outcome itself (#37).
static RENDER_CACHE: LazyLock<Mutex<HashMap<String, (Instant, MermaidResult)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// A fresh cached render outcome for `source`, if any (#37). Expired
/// entries are swept on every lookup so an idle cache cannot bloat.
/// #100: the key is normalized — trailing newlines stripped — because the
/// raw-markdown fence finder keeps the fence body's trailing newline while
/// the parser's `Block::Code.text` drops it; without this the same diagram
/// got two keys one byte apart and each delivery surface re-paid the render.
pub(crate) fn cache_get(source: &str) -> Option<MermaidResult> {
    let source = source.trim_end_matches('\n');
    let mut cache = RENDER_CACHE.lock().ok()?;
    let now = Instant::now();
    cache.retain(|_, (at, _)| now.duration_since(*at) < Duration::from_secs(RENDER_CACHE_TTL_SECS));
    cache.get(source).map(|(_, outcome)| outcome.clone())
}

/// Store a deterministic render outcome for `source` (#37). Only
/// [`MermaidResult::ImageBytes`] and [`MermaidResult::ParseError`] are
/// stored; transient failures are skipped so a retryable outage is never
/// pinned as a stuck failure block. Evicts the oldest entry at the cap.
pub(crate) fn cache_put(source: &str, outcome: &MermaidResult) {
    let source = source.trim_end_matches('\n'); // #100: shared normalized key
    if !matches!(
        outcome,
        MermaidResult::ImageBytes(_) | MermaidResult::ParseError(_)
    ) {
        return;
    }
    let Ok(mut cache) = RENDER_CACHE.lock() else {
        return;
    };
    if cache.len() >= RENDER_CACHE_CAP && !cache.contains_key(source) {
        let oldest = cache
            .iter()
            .min_by_key(|(_, (at, _))| *at)
            .map(|(k, _)| k.clone());
        if let Some(key) = oldest {
            cache.remove(&key);
        }
    }
    cache.insert(source.to_string(), (Instant::now(), outcome.clone()));
}

/// Hand the render outcome to the caller, caching it first when it is
/// deterministic (#37).
fn finish(source: &str, outcome: MermaidResult) -> MermaidResult {
    cache_put(source, &outcome);
    outcome
}

/// Record a render failure and return it (#189). Before this helper every
/// non-image exit returned [`MermaidResult::Failed`] with ZERO log output, so
/// a dropped body left no trace in the daemon log and incidents were argued
/// from absence — the same defect class #64 fixed for non-200 responses, where
/// the body leg was simply missed. `stage` names the ladder rung, and the
/// `timed_out` split is what distinguishes a stalled download from a refused
/// connection; both used to print the identical user-facing string.
fn fail(
    stage: &str,
    source: &str,
    note: impl Into<String>,
    err: Option<&reqwest::Error>,
) -> MermaidResult {
    let note = note.into();
    let detail = err.map(|e| e.to_string()).unwrap_or_default();
    tracing::warn!(
        stage,
        source_len = source.len(),
        timed_out = err.map(|e| e.is_timeout()).unwrap_or(false),
        error = %detail,
        note = %note,
        "mermaid render failed"
    );
    MermaidResult::Failed(note)
}

/// Read a render body in full, retrying ONCE on a transient failure (#189).
/// The caller only reaches this with a response that already passed the
/// `2xx + image/*` check, so the render exists server-side: one mid-stream
/// drop is worth exactly one cheap re-fetch before we degrade to a failure
/// block. `stage` labels the rung (`body` / `body-clamp`) so the retry rung is
/// distinguishable in the log while the user-facing note stays clean.
async fn read_body_with_retry(
    client: &reqwest::Client,
    url: &str,
    first: reqwest::Response,
    source: &str,
    stage: &str,
) -> Result<Vec<u8>, MermaidResult> {
    match first.bytes().await {
        Ok(b) => return Ok(b.to_vec()),
        Err(e) => {
            tracing::warn!(
                stage,
                source_len = source.len(),
                timed_out = e.is_timeout(),
                error = %e,
                "mermaid render body read failed; retrying once"
            );
        }
    }
    tokio::time::sleep(Duration::from_millis(BODY_RETRY_DELAY_MS)).await;
    let retry = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            return Err(fail(
                &format!("{stage}-retry"),
                source,
                "diagram renderer unreachable",
                Some(&e),
            ));
        }
    };
    let status = retry.status().as_u16();
    let content_type = retry
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !is_image_response(status, &content_type) {
        let body = retry.text().await.unwrap_or_default();
        return Err(fail(
            &format!("{stage}-retry"),
            source,
            error_note(status, &body),
            None,
        ));
    }
    match retry.bytes().await {
        Ok(b) => Ok(b.to_vec()),
        Err(e) => Err(fail(
            &format!("{stage}-retry"),
            source,
            "diagram renderer dropped the image",
            Some(&e),
        )),
    }
}

/// Whether a renderer status is transient enough to be worth exactly one
/// re-send (#1741): server errors plus the two 4xx classes
/// [`classify_render_failure`] already treats as transient. Any other 4xx
/// is a deterministic parse rejection of this exact source and is returned
/// untouched for classification. Kept next to `classify_render_failure` so
/// the two transient splits stay mirrorable.
pub(crate) fn is_transient_status(status: u16) -> bool {
    status >= 500 || status == 408 || status == 429
}

/// Legible note for a transport-level send failure, matching the split the
/// connect stage has always logged (timeout vs unreachable).
pub(crate) fn transport_note(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "diagram renderer timed out"
    } else {
        "diagram renderer unreachable"
    }
}

/// Send the render request with ONE bounded retry (#1741). The body stage
/// has had this since #189; the connect stage did not, so a single stalled
/// handshake degraded a valid diagram to the failure block: 3 of 4 renders
/// in one 6-minute window failed at `connect` with `timed_out=true` while a
/// manual GET seconds later returned HTTP 200. The GET is idempotent and
/// the render deterministic, so one re-send costs an RTT and recovers the
/// intermittent-stall pattern. A deterministic 4xx (other than 408/429) is
/// NOT retried: the renderer rejected this exact source and will reject it
/// again. `stage` labels the rung (`connect` / `connect-clamp`); the retry
/// rung logs and fails as `{stage}-retry` so the ladder stays legible.
/// Takes the full URL so tests can point it at a local mock server without
/// exposing [`MERMAID_INK_BASE`].
pub(crate) async fn send_with_retry(
    client: &reqwest::Client,
    url: &str,
    source: &str,
    stage: &str,
) -> Result<reqwest::Response, MermaidResult> {
    let retry_stage = format!("{stage}-retry");
    match client.get(url).send().await {
        Ok(r) if !is_transient_status(r.status().as_u16()) => return Ok(r),
        Ok(r) => {
            tracing::warn!(
                stage,
                retry_stage = %retry_stage,
                status = r.status().as_u16(),
                source_len = source.len(),
                "mermaid render request failed transiently; retrying once"
            );
        }
        Err(e) => {
            tracing::warn!(
                stage,
                retry_stage = %retry_stage,
                timed_out = e.is_timeout(),
                source_len = source.len(),
                error = %e,
                "mermaid render request failed transiently; retrying once"
            );
        }
    }
    tokio::time::sleep(Duration::from_millis(BODY_RETRY_DELAY_MS)).await;
    match client.get(url).send().await {
        Ok(r) => Ok(r),
        Err(e) => Err(fail(&retry_stage, source, transport_note(&e), Some(&e))),
    }
}

/// Pre-validate a single mermaid diagram against the renderer. On HTTP 200
/// with an `image/*` content type it DOWNLOADS the rendered PNG and returns
/// [`MermaidResult::ImageBytes`] — Telegram never fetches a URL from us
/// (its own URL fetcher proved the unreliable link: `400 failed to get
/// HTTP URL content` on live probes while the same URL fetched fine from
/// this host). Every other outcome (non-200, non-image, timeout, transport
/// error, client build failure, dropped body) yields
/// [`MermaidResult::Failed`] with a legible note. Never panics, never hangs
/// past the timeout.
///
/// Delivery is remote-only (#1238, owner directive: the in-process
/// renderer's visual quality is not acceptable in production, and the owner
/// later ordered it removed from the tree entirely): the request ladder
/// walks natural size → proportional width clamp, both served by
/// mermaid.ink; if every rung fails or still busts Telegram's photo box,
/// the fence degrades to a legible failure block. No local renderer exists.
pub(crate) async fn resolve(source: &str) -> MermaidResult {
    // #37: reuse a fresh render — the regen preflight and the delivery path
    // resolve the same source, and the renderer's outcome is deterministic.
    if let Some(cached) = cache_get(source) {
        return cached;
    }

    let url = ink_url(source);

    // Every retry and body read below shares this client, and the ceilings are
    // client-level settings, so the slot builds it once.
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    let client = crate::utils::http::cached_client(&CLIENT, |b| {
        b.connect_timeout(std::time::Duration::from_secs(
            PREVALIDATE_CONNECT_TIMEOUT_SECS,
        ))
        .timeout(std::time::Duration::from_secs(PREVALIDATE_TIMEOUT_SECS))
    });

    let resp = match send_with_retry(client, &url, source, "connect").await {
        Ok(r) => r,
        Err(outcome) => return outcome,
    };

    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if is_image_response(status, &content_type) {
        // Bytes delivery (hybrid round 2): download the PNG here and hand
        // Telegram the bytes via multipart (`attach://`). Telegram's own
        // server-side URL fetcher is the unreliable hop — it 400s with
        // "failed to get HTTP URL content" on URLs this host fetches fine —
        // so Telegram never sees a URL at all.
        let body = match read_body_with_retry(client, &url, resp, source, "body").await {
            Ok(b) => b,
            Err(outcome) => return outcome,
        };
        // Dimension ladder (#1238): natural size first; if it busts
        // Telegram's photo box, re-request the SAME diagram with a
        // proportional 1200px width clamp — still mermaid.ink, still
        // Chromium/ELK quality. Delivery never redraws locally.
        match png_dims(&body) {
            Some((w, h)) if !photo_fits(w, h) => {
                tracing::warn!(
                    w,
                    h,
                    "natural render exceeds the photo box; retrying at the width clamp"
                );
                let clamp_url = ink_url_params(source, MERMAID_INK_CLAMP_PARAMS);
                let cresp = match send_with_retry(client, &clamp_url, source, "connect-clamp").await
                {
                    Ok(r) => r,
                    Err(outcome) => return outcome,
                };
                let cstatus = cresp.status().as_u16();
                let ctype = cresp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                if !is_image_response(cstatus, &ctype) {
                    let cbody = cresp.text().await.unwrap_or_default();
                    return fail("clamp-response", source, error_note(cstatus, &cbody), None);
                }
                let cbytes =
                    match read_body_with_retry(client, &clamp_url, cresp, source, "body-clamp")
                        .await
                    {
                        Ok(b) => b,
                        Err(outcome) => return outcome,
                    };
                if let Some((cw, ch)) = png_dims(&cbytes).filter(|&(cw, ch)| !photo_fits(cw, ch)) {
                    return fail(
                        "clamp-dims",
                        source,
                        format!(
                            "rendered diagram exceeds the photo box even at the width clamp: {cw}x{ch} px"
                        ),
                        None,
                    );
                }
                tracing::info!(
                    bytes = cbytes.len(),
                    "mermaid.ink clamp render ok; delivering bytes"
                );
                return finish(source, MermaidResult::ImageBytes(cbytes.to_vec()));
            }
            _ => {}
        }
        tracing::info!(
            bytes = body.len(),
            "mermaid.ink render ok; delivering bytes"
        );
        return finish(source, MermaidResult::ImageBytes(body.to_vec()));
    }

    // Not a usable image: surface the renderer's own error text (mermaid.ink
    // returns a plain-text parse error) so the failure block is legible.
    let body = resp.text().await.unwrap_or_default();
    // Failure telemetry (leshchenko1979/opencrabs#64): the success leg logs
    // `render ok`; without a matching warn here a non-200 is invisible in
    // the daemon log and incidents get argued from absence.
    tracing::warn!(
        status,
        note = %error_note(status, &body),
        "mermaid.ink render failed"
    );
    finish(source, classify_render_failure(status, &body))
}

/// Pre-render every mermaid fence in `text` and collect the PARSE errors
/// (#37). Called from the tool loop before the reply is final: each fence
/// is resolved through [`resolve`] — which populates the render cache, so
/// the delivery path reuses every outcome instead of re-asking the
/// renderer — and only deterministic parse rejections are reported.
/// Transient failures stay silent here; the delivery path degrades them
/// to legible failure blocks as before. An empty `Vec` means every fence
/// rendered.
pub(crate) async fn preflight_parse_errors(text: &str) -> Vec<String> {
    let mut errors = Vec::new();
    for fence in find_mermaid_fences(text) {
        if let MermaidResult::ParseError(note) = resolve(&fence.source).await {
            errors.push(note);
        }
    }
    errors
}

/// Classify a non-image renderer response (#37): HTTP 4xx — except the
/// transient 408/429 — is a deterministic PARSE rejection of this exact
/// source; mermaid.ink answers it with plain-text error text naming the
/// offending construct, which the model can act on (regen nudge). Anything
/// else (server errors, odd non-image responses) is transient/infra and
/// keeps the plain note. Pure, so the branching is unit-testable without a
/// network call.
pub(crate) fn classify_render_failure(status: u16, body: &str) -> MermaidResult {
    if (400..500).contains(&status) && !matches!(status, 408 | 429) {
        MermaidResult::ParseError(error_note(status, body))
    } else {
        MermaidResult::Failed(error_note(status, body))
    }
}

/// Whether an HTTP response represents a usable rendered image. Split out so
/// the accept/reject branching is unit-testable without a network call.
pub(crate) fn is_image_response(status: u16, content_type: &str) -> bool {
    (200..300).contains(&status) && content_type.to_lowercase().starts_with("image/")
}

/// Build a short, legible failure note from the renderer's response body.
pub(crate) fn error_note(status: u16, body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return format!("diagram renderer returned HTTP {status}");
    }
    trimmed.chars().take(ERROR_NOTE_MAX_CHARS).collect()
}

/// Pure: given a pre-validation outcome, the fence's position, and its
/// source, produce the markdown replacement and (for a valid image) the
/// media entry. Split out so the replacement shape is unit-testable without
/// a network call. `index` is the fence's ordinal in the message.
pub(crate) fn replacement_for(
    outcome: &MermaidResult,
    index: usize,
    source: &str,
) -> (String, Option<MediaEntry>) {
    match outcome {
        MermaidResult::Image(url) => {
            let id = format!("diag{index}");
            // URL path does not carry byte dimensions locally; always provide vector link for URL-based delivery
            let md = format!("![diagram](tg://photo?id={id}){}", svg_link_md(source));
            (
                md,
                Some(MediaEntry {
                    id,
                    url: Some(url.clone()),
                    bytes: None,
                    kind: MediaKind::Photo,
                    name: None,
                }),
            )
        }
        MermaidResult::ImageBytes(bytes) => {
            let id = format!("diag{index}");
            let capped = png_dims(bytes)
                .map(|(w, h)| is_diagram_capped(w, h))
                .unwrap_or(false);
            let md = if capped {
                format!("![diagram](tg://photo?id={id}){}", svg_link_md(source))
            } else {
                format!("![diagram](tg://photo?id={id})")
            };
            (
                md,
                Some(MediaEntry {
                    id,
                    url: None,
                    bytes: Some(bytes.clone()),
                    kind: MediaKind::Photo,
                    name: None,
                }),
            )
        }
        // #189: a transient failure keeps the svg hatch — the render may well
        // exist server-side; a deterministic parse rejection does not, because
        // the renderer produced no diagram to link to.
        MermaidResult::Failed(err) => (markdown_failure_block_with_link(err, source), None),
        MermaidResult::ParseError(err) => (markdown_failure_block(err, source), None),
    }
}

/// Resolve a single fence to a render outcome. Remote-only delivery
/// (#1044 bytes path, #1238 ladder): mermaid.ink renders (ELK layout,
/// Chromium text rendering, full mermaid 11 diagram coverage) and the PNG
/// bytes ride to Telegram via multipart (`attach://`) — Telegram never
/// fetches a URL. On total failure the prevalidation note degrades to the
/// legible failure block; the in-process renderer is never invoked in
/// delivery.
async fn resolve_fence(source: &str) -> MermaidResult {
    resolve(source).await
}

/// Resolve every mermaid fence in `text` for the markdown+media path: valid
/// diagrams become `![diagram](tg://photo?id=diagN)` references with a
/// matching [`MediaEntry`], broken ones become legible markdown failure
/// blocks. Non-fence text is untouched (byte-identical). Boxed because the
/// resolver is async and because it spans an await boundary.
pub(crate) fn resolve_markdown_media(text: &str) -> BoxFuture<'static, (String, Vec<MediaEntry>)> {
    let text = text.to_string();
    async move {
        let fences = find_mermaid_fences(&text);
        if fences.is_empty() {
            return (text, Vec::new());
        }
        let mut result = text.clone();
        let mut media = Vec::new();
        // Replace from last to first so earlier byte offsets stay valid.
        for (i, fence) in fences.iter().enumerate().rev() {
            let outcome = resolve_fence(&fence.source).await;
            let (replacement, entry) = replacement_for(&outcome, i, &fence.source);
            if let Some(e) = entry {
                media.push(e);
            }
            result.replace_range(fence.start..fence.end, &replacement);
        }
        // Media was pushed in reverse fence order; restore fence order.
        media.reverse();
        (result, media)
    }
    .boxed()
}

/// Recursively replace every `Code{lang:"mermaid"}` block with a
/// `Mermaid{source, result}` block by pre-validating each fence. Handles
/// top-level fences and fences nested inside quotes, list items, and details.
/// Boxed because the walk is recursive and an async fn cannot recurse
/// without indirection (E0733). Used by the HTML fallback path.
pub(crate) fn resolve_blocks(blocks: Vec<Block>) -> BoxFuture<'static, Vec<Block>> {
    async move {
        let mut out = Vec::with_capacity(blocks.len());
        for block in blocks {
            out.push(resolve_block(block).await);
        }
        out
    }
    .boxed()
}

fn resolve_block(block: Block) -> BoxFuture<'static, Block> {
    async move {
        match block {
            Block::Code { lang, text }
                if lang.as_deref().is_some_and(is_mermaid_lang)
                    || (lang.is_none() && looks_like_mermaid_source(&text)) =>
            {
                let result = resolve_fence(&text).await;
                Block::Mermaid {
                    source: text,
                    result,
                }
            }
            Block::Quote(inner) => Block::Quote(resolve_blocks(inner).await),
            Block::List(mut list) => {
                for item in &mut list.items {
                    item.children = resolve_blocks(std::mem::take(&mut item.children)).await;
                }
                Block::List(list)
            }
            Block::Details {
                summary,
                blocks,
                open,
            } => Block::Details {
                summary,
                blocks: resolve_blocks(blocks).await,
                open,
            },
            other => other,
        }
    }
    .boxed()
}

fn is_mermaid_lang(lang: &str) -> bool {
    lang.trim().eq_ignore_ascii_case("mermaid")
}

/// Markdown for a diagram that could not be rendered: a bold warning line,
/// then a code fence holding the renderer's error note and the original
/// source so the reader can see (and fix) what failed.
pub(crate) fn markdown_failure_block(err: &str, source: &str) -> String {
    format!(
        "> ⚠️ **Mermaid diagram could not be rendered**\n\n```\n{err}\n\nSource:\n{source}\n```"
    )
}

/// Markdown for a TRANSIENT failure (#1741): the renderer stalled, 5xx'd,
/// or dropped the image, which says nothing about the diagram's syntax.
/// The shared old headline ("could not be rendered") reads as a parse
/// rejection and sent an agent rewriting a valid diagram three times in
/// one session (2026-09-25 daemon log) before anyone curled the renderer.
/// The deterministic [`MermaidResult::ParseError`] path keeps the old
/// headline via [`markdown_failure_block`] because there the diagram IS
/// the problem.
pub(crate) fn markdown_failure_block_transport(err: &str, source: &str) -> String {
    format!(
        "> ⚠️ **Renderer failure, not a syntax error: your diagram was NOT modified**\n\n```\n{err}\n\nSource:\n{source}\n```"
    )
}

/// #189: the markdown failure block plus a `[svg]` escape hatch, for a
/// TRANSIENT failure ([`MermaidResult::Failed`]) only. In that case the
/// response had already passed the `2xx + image/*` check before the body was
/// lost, so the render very likely exists server-side and the link is worth
/// offering. A [`MermaidResult::ParseError`] is a deterministic rejection of
/// this exact source — the renderer produced no diagram, so the link would be
/// dead and the plain block stays. The URL is build-only: the browser does the
/// fetching, this host never touches mermaid.ink for it.
pub(crate) fn markdown_failure_block_with_link(err: &str, source: &str) -> String {
    format!(
        "{}\n\n[svg]({})",
        markdown_failure_block_transport(err, source),
        ink_url_svg(source)
    )
}

/// HTML for a successfully rendered diagram: a bare `<img>` in a `<figure>`,
/// which the Telegram rich-HTML parser turns into a native photo block.
pub(crate) fn image_html(url: &str) -> String {
    format!("<figure><img src=\"{}\"/></figure>", escape_html(url))
}

/// HTML for a diagram that could not be rendered: a bold warning line, the
/// renderer's error note in a blockquote, and the original source in a code
/// block so the reader can see (and fix) what failed.
pub(crate) fn svg_link_html(source: &str) -> String {
    format!(
        "\n<a href=\"{}\">[svg]</a>",
        escape_html(&ink_url_svg(source))
    )
}

pub(crate) fn failure_html(err: &str, source: &str) -> String {
    format!(
        "<b>⚠️ Mermaid diagram could not be rendered</b>\n<blockquote>{}</blockquote>\n<pre><code>{}</code></pre>",
        escape_html(err),
        escape_html(source)
    )
}

/// HTML headline for a TRANSIENT failure (#1741): same split as
/// [`markdown_failure_block_transport`]. The renderer choked; the diagram
/// did not.
pub(crate) fn failure_html_transport(err: &str, source: &str) -> String {
    format!(
        "<b>⚠️ Renderer failure, not a syntax error: your diagram was NOT modified</b>\n<blockquote>{}</blockquote>\n<pre><code>{}</code></pre>",
        escape_html(err),
        escape_html(source)
    )
}

/// Generic svg escape-hatch link fragment for markdown contexts —
/// a small `[Open SVG vector]` link to the full-size vector render when the diagram
/// is capped or scaled down. Always padded with trailing newline so subsequent markdown
/// blocks (e.g. tables, headers) start on a fresh line and retain proper block spacing.
pub(crate) fn svg_link_md(source: &str) -> String {
    format!("\n[Open SVG vector]({})\n", ink_url_svg(source))
}

/// Canonical correction rules for Mermaid diagrams shared across the codebase.
pub fn unified_mermaid_rules() -> &'static str {
    "Correction rules:\n\
     1. Syntax & Notes: Fix the exact token or line reported by the renderer. For sequence diagrams, \
     'Note over A,B:' supports at most two participants spanning the range — do not list three or more comma-separated actors. \
     Do not use backticks or HTML tags in labels (only '<br/>' is allowed for line breaks).\n\
     2. Mobile Layout & Aspect Ratio: Always use top-down vertical layouts ('flowchart TD' or 'direction TB'). \
     Never use wide 'LR' layouts or unconstrained horizontal subgraphs that shrink illegibly on mobile screens."
}

/// Format a unified Mermaid syntax error message with context and diagnostic errors.
pub fn format_mermaid_error(context: &str, errors: &[String]) -> String {
    let quoted = errors.join("\n");
    let rules = unified_mermaid_rules();
    if context.is_empty() {
        format!(
            "Mermaid diagram syntax error.\n\n\
             Renderer diagnostic:\n{quoted}\n\n\
             {rules}"
        )
    } else {
        format!(
            "Mermaid diagram syntax error in {context}.\n\n\
             Renderer diagnostic:\n{quoted}\n\n\
             {rules}"
        )
    }
}
