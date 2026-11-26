//! `opencrabs session notify` — mechanical session notifications for
//! tooling (#23).
//!
//! The CLI is a SEPARATE PROCESS from the daemon that owns the in-memory
//! route table, so the verb posts over the profile's A2A gateway — the
//! daemon's HTTP surface — where the `session/notify` method hands the
//! message to `deliver_to_session`, the same route the agent's
//! session_notify tool uses (including #19 redirect-with-framing for
//! archived/replaced sessions and #1206 parking).
//!
//! Exit codes are the machine contract for tooling (oc-deploy fan-out, #24):
//!
//! | exit | meaning                                             |
//! |------|-----------------------------------------------------|
//! | 0    | delivered / redirected / parked — the message is safe |
//! | 2    | unknown or dead uuid — nothing sent, nothing created |
//! | 3    | refused: target mid-turn and `--interrupt` not set  |
//! | 4    | transport/config: a2a disabled, unreachable, bad response |
//!
//! SENDER LABEL (#23, owner amendment "Overridable"): the CLI lane has no
//! sender session, so the recipient's echo shows the carried label —
//! default "CLI tooling", overridable with `--sender`.

use crate::cli::args::OutputFormat;
use crate::config::Config;
use anyhow::Result;

pub const EXIT_OK: i32 = 0;
pub const EXIT_NO_ROUTE: i32 = 2;
pub const EXIT_REFUSED: i32 = 3;
pub const EXIT_TRANSPORT: i32 = 4;

/// Bounded retry budget for a notify's transport leg (#199).
///
/// A `req.send()` that ERRORS is ambiguous — the request may have reached the
/// daemon and queued the notify (only the RESPONSE was lost), or it may never
/// have arrived. The CLI cannot tell which, so it re-sends the SAME request,
/// caller-minted `notify_id` included, and the gateway's reserve-before-deliver
/// guard turns the ambiguous case into exactly-once delivery: the duplicate
/// reports the original outcome instead of delivering a second copy.
///
/// Only transport-class failures are retried: a `req.send()` Err, or a reply
/// that carries no JSON-RPC body at all (a gateway mid-reload, a proxy's
/// 502/504) — neither is an answer. A reply that unpacks into a JSON-RPC error
/// or result IS an answer, a decision, and re-sending a decision is pointless.
/// `pub(crate)` so the bounds are assertable in tests.
pub(crate) const MAX_ATTEMPTS: u32 = 3;
pub(crate) const RETRY_BACKOFF_MS: [u64; 2] = [500, 1000];

/// Classify a reqwest send failure into the #1959 candidate mechanisms so a
/// single journal line can tell them apart. reqwest's `Display` is the same
/// opaque `error sending request for url (...)` for every failure mode, and
/// that uniformity is exactly what left 16 post-bind losses undiagnosable.
/// The bottom of the source chain is usually hyper's io error ("Connection
/// refused (os error 111)"), so it rides along.
pub(crate) fn transport_cause(e: &reqwest::Error) -> String {
    let kind = if e.is_connect() {
        "connect: gateway refused or dropped the connection (listener not accepting)"
    } else if e.is_timeout() {
        "timeout: the send budget expired (accept backlog saturated or handler wedged)"
    } else if e.is_body() {
        "body: the request body failed to send"
    } else if e.is_request() {
        "request: failed after connect, before a usable response (server closed the connection mid-exchange)"
    } else {
        "unknown reqwest failure"
    };
    let mut out = kind.to_string();
    let mut chain: Vec<String> = Vec::new();
    let mut src = std::error::Error::source(e);
    while let Some(s) = src {
        chain.push(s.to_string());
        src = s.source();
    }
    if !chain.is_empty() {
        out.push_str(&format!("; cause chain: {}", chain.join(" -> ")));
    }
    out
}

/// One POST to the A2A gateway, retried `MAX_ATTEMPTS` times on transport
/// errors. Shared by the send and status verbs (one copy, no drift): both dial
/// the same endpoint with the same client, timeout and auth, and differ only in
/// the method they call and how they read the outcome back.
///
/// `Ok(result)` = the JSON-RPC result object. `Err(msg)` = the caller's journal
/// detail for a transport failure.
///
/// `pub(crate)` so the retry leg is testable in-process (#199) — the
/// behavioral test drives a real HTTP server through it.
pub(crate) async fn post_jsonrpc(
    url: &str,
    api_key: Option<&str>,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut last_error = format!("cannot reach the A2A gateway at {url}");
    for attempt in 0..MAX_ATTEMPTS {
        if attempt > 0 {
            let backoff = RETRY_BACKOFF_MS[attempt as usize - 1];
            tracing::debug!(attempt, backoff_ms = backoff, "retrying A2A transport");
            tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
        }
        let mut req = crate::utils::http::shared()
            .post(url)
            .timeout(std::time::Duration::from_secs(10))
            .json(body);
        if let Some(key) = api_key {
            req = req.bearer_auth(key);
        }
        let resp = match req.send().await {
            Ok(resp) => resp,
            Err(e) => {
                // #1959: the bare `{e}` Display is the same opaque string for
                // every failure mode; classify so the journal can discriminate
                // the mechanism, and number the attempt so a mid-exchange drop
                // (last attempt dying) reads differently from a dead listener
                // (every attempt failing).
                last_error = format!(
                    "cannot reach the A2A gateway at {url}: {} (attempt {}/{})",
                    transport_cause(&e),
                    attempt + 1,
                    MAX_ATTEMPTS
                );
                continue;
            }
        };
        let status = resp.status();
        let rpc = match resp.json::<crate::a2a::types::JsonRpcResponse>().await {
            Ok(rpc) => rpc,
            // A response that carries no JSON-RPC body is NOT a decision: the
            // usual cause is a gateway mid-reload or a proxy answering 502/504
            // — transient by nature, and the very case this leg exists for. So
            // it retries like a transport error instead of failing the notify
            // on the first attempt. A JSON-RPC `error` below IS a decision and
            // is never retried.
            Err(e) => {
                // #1959: same attempt numbering as the send branch: the
                // mechanism differs by which attempt died and how.
                last_error = format!(
                    "gateway at {url} returned HTTP {status} without a JSON-RPC body: {e} \
                     (attempt {}/{})",
                    attempt + 1,
                    MAX_ATTEMPTS
                );
                continue;
            }
        };
        if let Some(err) = rpc.error {
            return Err(format!("gateway error {}: {}", err.code, err.message));
        }
        return match rpc.result {
            Some(result) => Ok(result),
            None => Err("gateway response carried neither result nor error".into()),
        };
    }
    Err(last_error)
}

/// The gateway endpoint and auth for this profile's config — one derivation,
/// used by both verbs. A bind of `0.0.0.0`/`::` is a LISTENING address, not a
/// connectable one: same-box callers always dial loopback.
fn gateway_endpoint(config: &Config) -> (String, Option<String>) {
    let host = match config.a2a.bind.as_str() {
        "0.0.0.0" | "::" | "[::]" => "127.0.0.1",
        other => other,
    };
    (
        format!("http://{}:{}/a2a/v1", host, config.a2a.port),
        config.a2a.api_key.clone(),
    )
}

/// Read `outcome`/`detail` out of a gateway result object, with the shared
/// fallback wording.
fn result_outcome(result: &serde_json::Value) -> (String, String) {
    (
        result
            .get("outcome")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        result
            .get("detail")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run(
    config: &Config,
    id_raw: &str,
    text: Option<&str>,
    title: Option<&str>,
    sender: Option<&str>,
    interrupt: bool,
    mode: Option<&str>,
    quiet_for_secs: Option<u64>,
    max_delay_secs: Option<u64>,
    confirm: bool,
    status: bool,
    format: OutputFormat,
) -> Result<()> {
    // #1802 kill switch (owner order): session notify is an explicit
    // operator opt-in, default OFF. Machine tooling sets
    // [agent] session_notify_enabled = true. Cross-session notification
    // belongs to the channel send tools under explicit user request.
    if !config.agent.session_notify_enabled {
        return finish(
            format,
            id_raw,
            "disabled_by_config",
            EXIT_TRANSPORT,
            "session notify is disabled by config: set [agent] session_notify_enabled = \
             true to enable it (issue #1802)",
        );
    }
    // Status mode (fork #146): poll a notification receipt by id instead of
    // sending. Rides the same A2A surface (); the id
    // is passed as the positional arg,  is unused.
    if status {
        let Some(notify_id) = text else {
            return finish(
                format,
                id_raw,
                "usage_error",
                EXIT_TRANSPORT,
                "--status requires the notification id as the <ID> argument",
            );
        };
        return run_status(config, notify_id, id_raw, format).await;
    }

    // Local usage errors: the gateway would reject these with INVALID_PARAMS
    // anyway, but failing here keeps the journal honest about who noticed.
    let target: uuid::Uuid = match id_raw.parse() {
        Ok(id) => id,
        Err(_) => {
            return finish(
                format,
                id_raw,
                "no_route",
                EXIT_NO_ROUTE,
                &format!("'{id_raw}' is not a valid session UUID"),
            );
        }
    };
    let Some(text) = text else {
        return finish(
            format,
            &target.to_string(),
            "usage_error",
            EXIT_TRANSPORT,
            "--text is required for sending (or pass --status to poll a receipt id)",
        );
    };
    if text.trim().is_empty() {
        return finish(
            format,
            &target.to_string(),
            "transport_error",
            EXIT_TRANSPORT,
            "--text must not be empty",
        );
    }
    // Shared sender validation (fork #146): the SAME policy rules the A2A
    // handler and the agent tool run — one copy, no drift. Failing here
    // keeps the journal honest about who noticed.
    if let Some(raw) = sender {
        let label = raw.trim();
        if label.is_empty() {
            return finish(
                format,
                &target.to_string(),
                "transport_error",
                EXIT_TRANSPORT,
                "--sender must not be empty",
            );
        }
        if let Err(e) = crate::brain::agent::service::notify_policy::validate_sender_label(label) {
            return finish(
                format,
                &target.to_string(),
                "transport_error",
                EXIT_TRANSPORT,
                &format!("--sender: {e}"),
            );
        }
    }
    if !config.a2a.enabled {
        return finish(
            format,
            &target.to_string(),
            "transport_error",
            EXIT_TRANSPORT,
            "the [a2a] gateway is disabled in this profile's config — the daemon cannot be reached",
        );
    }

    let (url, api_key) = gateway_endpoint(config);

    // #199: the id is minted ONCE per invocation, before the transport leg, so
    // every retry of THIS notify carries the identical id and the gateway's
    // reserve-before-deliver guard can recognise it as a retry rather than a
    // second notify. Minting inside the retry loop would defeat the whole leg.
    let notify_id = uuid::Uuid::new_v4();
    let mut params = serde_json::json!({
        "session_id": target.to_string(),
        "message": text,
        "notify_id": notify_id.to_string(),
    });
    // Omit `interrupt` unless explicitly true (fork #158): the CLI flags default
    // `interrupt` to false, but unconditionally sending `interrupt: false`
    // conflicts with `delivery.mode: "turn-end"` in `notify_policy::resolve_mode`.
    if interrupt {
        params["interrupt"] = serde_json::json!(true);
    }
    if let Some(t) = title {
        params["title"] = serde_json::json!(t);
    }
    if let Some(s) = sender {
        params["sender"] = serde_json::json!(s.trim());
    }
    // Delivery policy (fork #146): the full v2 ontology rides to the
    // A2A method, which resolves it through the shared policy module.
    if interrupt || mode.is_some() || quiet_for_secs.is_some() || max_delay_secs.is_some() {
        let mut delivery = serde_json::json!({});
        if let Some(m) = mode {
            delivery["mode"] = serde_json::json!(m);
        }
        if let Some(q) = quiet_for_secs {
            delivery["quiet_for_secs"] = serde_json::json!(q);
        }
        if let Some(m) = max_delay_secs {
            delivery["max_delay_secs"] = serde_json::json!(m);
        }
        params["delivery"] = delivery;
    }
    if confirm {
        params["confirm"] = serde_json::json!(true);
    }
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "session/notify",
        "params": params,
    });

    let (outcome, exit_code, detail) = match post_jsonrpc(&url, api_key.as_deref(), &body).await {
        Err(detail) => ("transport_error".to_string(), EXIT_TRANSPORT, detail),
        Ok(result) => {
            let (outcome, detail) = result_outcome(&result);
            let code = match outcome.as_str() {
                "delivered" | "parked" | "deferred" => EXIT_OK,
                "no_route" => EXIT_NO_ROUTE,
                "refused_in_flight" => EXIT_REFUSED,
                _ => EXIT_TRANSPORT,
            };
            (outcome, code, detail)
        }
    };

    finish(format, &target.to_string(), &outcome, exit_code, &detail)
}

/// Status mode (fork #146): POST the notify id to the A2A
/// `session/notify-status` method and render the receipt lifecycle.
/// Exit codes: 0 = injected (consumed by the machinery) or queued-but-live;
/// 2 = unknown id (not tracked — in-memory receipts die with the process);
/// 4 = transport.
async fn run_status(
    config: &Config,
    notify_id: &str,
    id_raw: &str,
    format: OutputFormat,
) -> Result<()> {
    if notify_id.parse::<uuid::Uuid>().is_err() {
        return finish(
            format,
            id_raw,
            "unknown_id",
            EXIT_NO_ROUTE,
            &format!("'{notify_id}' is not a valid notification UUID"),
        );
    }
    if !config.a2a.enabled {
        return finish(
            format,
            id_raw,
            "transport_error",
            EXIT_TRANSPORT,
            "the [a2a] gateway is disabled in this profile's config — the daemon cannot be reached",
        );
    }
    let (url, api_key) = gateway_endpoint(config);
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "session/notify-status",
        "params": { "notify_id": notify_id },
    });
    let (outcome, exit_code, detail) = match post_jsonrpc(&url, api_key.as_deref(), &body).await {
        Err(detail) => ("transport_error".to_string(), EXIT_TRANSPORT, detail),
        Ok(result) => {
            let (outcome, detail) = result_outcome(&result);
            let code = match outcome.as_str() {
                "injected" | "queued" => EXIT_OK,
                "unknown_id" => EXIT_NO_ROUTE,
                _ => EXIT_TRANSPORT,
            };
            (outcome, code, detail)
        }
    };
    finish(format, id_raw, &outcome, exit_code, &detail)
}

/// Journal + output + exit. One append-only journal line per invocation,
/// written BEFORE the process exits — the journal, not the exit code, is the
/// durable record (tool-logging law).
fn finish(
    format: OutputFormat,
    target: &str,
    outcome: &str,
    exit_code: i32,
    detail: &str,
) -> Result<()> {
    journal_line(target, outcome, exit_code, detail);
    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "ok": exit_code == EXIT_OK,
                    "target": target,
                    "outcome": outcome,
                    "detail": detail,
                    "exit": exit_code,
                }))?
            );
        }
        _ => {
            if exit_code == EXIT_OK {
                if outcome == "parked" || outcome == "deferred" {
                    println!("⚠️ {outcome}: {detail}");
                } else {
                    println!("✅ {outcome}: {detail}");
                }
            } else {
                eprintln!("❌ {outcome}: {detail} (exit {exit_code})");
            }
        }
    }
    std::process::exit(exit_code)
}

fn journal_line(target: &str, outcome: &str, exit_code: i32, detail: &str) {
    use std::io::Write;

    let path = crate::logging::log_dir().join("session-notify.journal");
    let ts = chrono::Utc::now().to_rfc3339();
    let caller = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
    // Truncate detail so one pathological message cannot bloat the line.
    let detail: String = detail.chars().take(500).collect();
    let line = format!(
        "{ts}\tcaller={caller}\ttarget={target}\toutcome={outcome}\texit={exit_code}\tdetail={detail}\n"
    );
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(line.as_bytes()));
}
