//! Protocol framing tests for the ACP server mode (#1540, #1539).
//!
//! Landed under `src/tests/` per the contribution rule that test modules
//! never live inline: the items exercised here (`parse_line`, `prompt_text`,
//! `tool_kind`, `permission_outcome`, `ClientMessage`) are all `pub` in
//! `crate::acp::protocol`, so no test-only surface has to leak into the
//! production module.

use crate::acp::protocol::{
    AcpMode, ClientMessage, EXTENSION_METHODS, SESSION_COMPACT, SESSION_COMPACT_LEGACY,
    SESSION_SET_MODE, SESSION_SET_MODEL, SESSION_SET_MODEL_LEGACY, SESSION_STEER_LEGACY,
    initialize_result, modes_payload, parse_line, permission_outcome, prompt_text, replay_updates,
    session_response, tool_kind,
};
use crate::db::models::Message;
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

#[test]
fn parses_request() {
    let msg =
        parse_line(r#"{"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":"/tmp"}}"#)
            .unwrap();
    match msg {
        ClientMessage::Request { id, method, params } => {
            assert_eq!(id, json!(1));
            assert_eq!(method, "session/new");
            assert_eq!(params["cwd"], json!("/tmp"));
        }
        other => panic!("expected request, got {other:?}"),
    }
}

#[test]
fn parses_notification() {
    let msg = parse_line(r#"{"jsonrpc":"2.0","method":"session/cancel","params":{}}"#).unwrap();
    assert!(matches!(msg, ClientMessage::Notification { .. }));
}

#[test]
fn parses_response_result_and_error() {
    let ok = parse_line(r#"{"jsonrpc":"2.0","id":7,"result":{"x":1}}"#).unwrap();
    match ok {
        ClientMessage::Response { id, result } => {
            assert_eq!(id, 7);
            assert_eq!(result.unwrap()["x"], json!(1));
        }
        other => panic!("expected response, got {other:?}"),
    }
    let err =
        parse_line(r#"{"jsonrpc":"2.0","id":8,"error":{"code":-32601,"message":"no"}}"#).unwrap();
    match err {
        ClientMessage::Response { id, result } => {
            assert_eq!(id, 8);
            let e = result.unwrap_err();
            assert_eq!(e.code, -32601);
        }
        other => panic!("expected response, got {other:?}"),
    }
}

#[test]
fn rejects_garbage() {
    assert!(parse_line("not json").is_err());
    assert!(parse_line(r#"{"jsonrpc":"2.0"}"#).is_err());
}

#[test]
fn extracts_prompt_text_blocks() {
    let params = json!({
        "prompt": [
            { "type": "text", "text": "hello" },
            { "type": "resource_link", "uri": "file:///tmp/a.png" },
            { "type": "image", "data": "..." }
        ]
    });
    let text = prompt_text(&params).unwrap();
    assert!(text.starts_with("hello"));
    assert!(text.contains("file:///tmp/a.png"));
}

#[test]
fn permission_outcome_maps_option_ids() {
    assert_eq!(
        permission_outcome(&json!({"outcome":{"outcome":"selected","optionId":"allow-once"}})),
        (true, false)
    );
    assert_eq!(
        permission_outcome(&json!({"outcome":{"outcome":"selected","optionId":"allow-always"}})),
        (true, true)
    );
    assert_eq!(
        permission_outcome(&json!({"outcome":{"outcome":"selected","optionId":"reject-once"}})),
        (false, false)
    );
    assert_eq!(
        permission_outcome(&json!({"outcome":{"outcome":"cancelled"}})),
        (false, false)
    );
}

#[test]
fn tool_kind_covers_loop_tools() {
    assert_eq!(tool_kind("bash"), "execute");
    assert_eq!(tool_kind("read_file"), "read");
    assert_eq!(tool_kind("edit_file"), "edit");
    assert_eq!(tool_kind("grep"), "search");
    assert_eq!(tool_kind("http_request"), "fetch");
    assert_eq!(tool_kind("spawn_agent"), "other");
}

#[test]
fn initialize_result_is_honest_about_load_replay() {
    let caps = initialize_result()["agentCapabilities"].clone();
    // session/load binds an existing session AND replays the stored
    // transcript before answering; the advertised capability must not claim
    // otherwise.
    assert_eq!(caps["loadSession"], json!(true));
    assert_eq!(caps["promptCapabilities"]["text"], json!(true));
}

#[test]
fn set_mode_is_accepted_alongside_set_model() {
    // The contract Moe's PR body promised is spelled `session/set_mode`;
    // both spellings dispatch to the same handler.
    assert_eq!(SESSION_SET_MODE, "session/set_mode");
    assert_eq!(SESSION_SET_MODEL, "_opencrabs/set_model");
}

#[test]
fn custom_methods_live_behind_the_extension_prefix() {
    // v1 allows custom requests "as long as their name starts with an
    // underscore" and reserves every other name for future protocol versions
    // (#1815 F3). Official methods stay unprefixed: `session/set_mode` is one
    // of the twelve session methods in the schema and must not be renamed.
    for name in EXTENSION_METHODS {
        assert!(
            name.starts_with('_'),
            "{name} sits in the reserved namespace"
        );
    }
    assert_eq!(SESSION_SET_MODEL, "_opencrabs/set_model");
    assert_eq!(SESSION_COMPACT, "_opencrabs/compact");
    assert!(!SESSION_SET_MODE.starts_with('_'));
}

#[test]
fn legacy_spellings_are_retained_but_not_advertised() {
    // Dropping these breaks MonoCode and every adapter already in the field,
    // so they stay accepted for one release (#1815 F3). They must not appear
    // in the advertisement, or a fresh client is pointed at a name we are
    // retiring.
    assert_eq!(SESSION_SET_MODEL_LEGACY, "session/set_model");
    assert_eq!(SESSION_COMPACT_LEGACY, "session/compact");
    assert_eq!(SESSION_STEER_LEGACY, "session/steer");
    for legacy in [
        SESSION_SET_MODEL_LEGACY,
        SESSION_COMPACT_LEGACY,
        SESSION_STEER_LEGACY,
    ] {
        assert!(
            !EXTENSION_METHODS.contains(&legacy),
            "{legacy} advertised as canonical"
        );
    }
}

#[test]
fn initialize_advertises_extensions_under_meta() {
    // `_meta` is the sanctioned place for this; a root-level field on a spec
    // type is a MUST NOT (#1815 F3, and the same rule F4 trips on `models`).
    let result = initialize_result();
    let advertised = result["_meta"]["extensions"]
        .as_array()
        .expect("initialize result must advertise its extensions");
    let names: Vec<&str> = advertised
        .iter()
        .map(|v| v.as_str().expect("extension name is a string"))
        .collect();
    for method in EXTENSION_METHODS {
        assert!(names.contains(method), "{method} not advertised");
    }
}

#[test]
fn mode_parse_round_trips_advertised_ids() {
    for id in [
        "supervised",
        "auto-accept-edits",
        "auto",
        "full-access",
        "plan",
    ] {
        let mode = AcpMode::parse(id).expect("advertised id parses");
        assert_eq!(mode.id(), id);
    }
    assert!(AcpMode::parse("yolo").is_none());
}

#[test]
fn modes_payload_names_current() {
    let payload = modes_payload(AcpMode::Plan);
    assert_eq!(payload["currentModeId"], json!("plan"));
    let ids: Vec<&str> = payload["availableModes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    assert_eq!(
        ids,
        vec![
            "supervised",
            "auto-accept-edits",
            "auto",
            "full-access",
            "plan"
        ]
    );
}

#[test]
fn replay_updates_maps_reasoning_blocked_and_text_segments() {
    let msg = Message {
        forward: None,
        id: Uuid::new_v4(),
        session_id: Uuid::new_v4(),
        role: "assistant".into(),
        content: "<!-- reasoning -->\nthinking hard\n<!-- /reasoning -->\n\
                  <!-- phantom_blocked=1 -->\nphantom narration\n<!-- /phantom_blocked=1 -->\n\
                  the visible answer"
            .into(),
        sequence: 1,
        created_at: Utc::now(),
        token_count: None,
        cost: None,
        input_tokens: None,
        cache_creation_tokens: None,
        cache_read_tokens: None,
        thinking: None,
        duration_secs: None,
    };
    let updates = replay_updates(&[msg]);
    let (kinds, texts): (Vec<&str>, Vec<&str>) = updates
        .iter()
        .map(|u| {
            (
                u["sessionUpdate"].as_str().unwrap(),
                u["content"]["text"].as_str().unwrap(),
            )
        })
        .unzip();
    assert_eq!(
        kinds,
        vec![
            "agent_thought_chunk",
            "agent_thought_chunk",
            "agent_message_chunk",
        ]
    );
    assert_eq!(texts[0], "thinking hard");
    assert!(texts[1].contains("Blocked narration"));
    assert!(texts[1].contains("phantom narration"));
    assert_eq!(texts[2], "the visible answer");
}

#[test]
fn initialize_advertises_auth_methods() {
    let v = initialize_result();
    assert!(
        v.get("authMethods").and_then(|a| a.as_array()).is_some(),
        "authMethods is optional per the ACP v1 schema (required is only protocolVersion); we emit it explicitly so clients and registry validators that expect the field present do not depend on the schema default"
    );
}

#[test]
fn new_session_result_carries_the_session_id() {
    let result = session_response(
        "acp-sess-1",
        json!({ "availableModels": [], "currentModelId": "" }),
        json!({ "availableModes": [] }),
        json!([]),
        true,
    );
    // NewSessionResponse requires sessionId: it is the only place a client
    // learns the id of a session it asked us to create.
    assert_eq!(result["sessionId"], json!("acp-sess-1"));
    assert!(result["configOptions"].is_array());
}

#[test]
fn load_session_result_does_not_echo_the_session_id() {
    // LoadSessionResponse defines modes, configOptions and _meta and nothing
    // else. The client supplied the id in the request, so echoing it added a
    // root field to a type the spec owns (#1815 F4).
    let result = session_response(
        "acp-sess-1",
        json!({ "availableModels": [], "currentModelId": "" }),
        json!({ "availableModes": [] }),
        json!([]),
        false,
    );
    assert!(
        result.get("sessionId").is_none(),
        "load echoed a sessionId the client already has"
    );
    assert!(result["configOptions"].is_array());
}
