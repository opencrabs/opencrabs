//! Agent Card generation for `.well-known/agent.json`.
//!
//! Builds an `AgentCard` from the running OpenCrabs configuration,
//! exposing available skills and capabilities to other A2A agents.
//!
//! Besides A2A itself the card announces the GUI backend protocol
//! ([`crate::backend::hello::PROTOCOL`]) as a second interface on the same
//! endpoint, so a window can tell a crab that serves it from one that needs
//! an update before it asks for anything.

use crate::a2a::types::*;
use crate::backend::hello::{PROTOCOL, PROTOCOL_VERSION};
use crate::brain::tools::registry::ToolRegistry;

/// Build the Agent Card for this OpenCrabs instance.
///
/// Skills are generated dynamically based on available tools in the registry.
pub fn build_agent_card(host: &str, port: u16, tool_registry: Option<&ToolRegistry>) -> AgentCard {
    let base_url = format!("http://{}:{}", host, port);

    let mut skills = vec![AgentSkill {
        id: "code-analysis".to_string(),
        name: "Code Analysis & Refactoring".to_string(),
        description: Some(
            "Analyze source code, identify issues, and suggest improvements.".to_string(),
        ),
        tags: vec![
            "code".to_string(),
            "analysis".to_string(),
            "refactoring".to_string(),
        ],
        examples: vec!["Analyze this Rust module for performance issues.".to_string()],
        input_modes: vec!["text/plain".to_string(), "application/json".to_string()],
        output_modes: vec!["text/plain".to_string(), "application/json".to_string()],
    }];

    // Research skill requires search tools
    let has_search = tool_registry
        .map(|r| r.has_tool("web_search") || r.has_tool("memory_search"))
        .unwrap_or(true);
    if has_search {
        skills.push(AgentSkill {
            id: "research".to_string(),
            name: "Deep Research".to_string(),
            description: Some(
                "Perform multi-source research using web search, memory search, and document analysis. Runs in read-only mode."
                    .to_string(),
            ),
            tags: vec![
                "research".to_string(),
                "analysis".to_string(),
                "synthesis".to_string(),
            ],
            examples: vec![
                "Research the latest developments in AI agent security.".to_string(),
            ],
            input_modes: vec!["text/plain".to_string()],
            output_modes: vec!["text/plain".to_string(), "application/json".to_string()],
        });
    }

    skills.push(AgentSkill {
        id: "debate".to_string(),
        name: "Multi-Agent Debate".to_string(),
        description: Some(
            "Participate in structured multi-round debates with other A2A agents using knowledge-enriched context."
                .to_string(),
        ),
        tags: vec![
            "debate".to_string(),
            "council".to_string(),
            "multi-agent".to_string(),
        ],
        examples: vec![
            "Debate the pros and cons of microservices vs monoliths.".to_string(),
        ],
        input_modes: vec!["text/plain".to_string(), "application/json".to_string()],
        output_modes: vec!["text/plain".to_string(), "application/json".to_string()],
    });

    AgentCard {
        name: format!(
            "OpenCrabs '{}' (v{})",
            crate::config::profile::current_profile_name(),
            crate::VERSION
        ),
        description: Some(
            "High-performance AI orchestration agent with A2A protocol support. \
             Part of the Bee Colony multi-agent system."
                .to_string(),
        ),
        version: Some(crate::VERSION.to_string()),
        documentation_url: Some("https://github.com/opencrabs/opencrabs".to_string()),
        icon_url: None,
        supported_interfaces: vec![
            SupportedInterface {
                url: format!("{}/a2a/v1", base_url),
                protocol_binding: "JSONRPC".to_string(),
                protocol_version: Some("1.0".to_string()),
            },
            SupportedInterface {
                url: format!("{}/a2a/v1", base_url),
                protocol_binding: PROTOCOL.to_string(),
                protocol_version: Some(PROTOCOL_VERSION.to_string()),
            },
        ],
        provider: Some(AgentProvider {
            organization: "OpenCrabs Contributors".to_string(),
            url: Some("https://github.com/opencrabs/opencrabs".to_string()),
        }),
        capabilities: Some(AgentCapabilities {
            streaming: true,
            push_notifications: false,
            state_transition_history: true,
        }),
        skills,
        default_input_modes: vec!["text/plain".to_string(), "application/json".to_string()],
        default_output_modes: vec!["text/plain".to_string(), "application/json".to_string()],
    }
}

/// Whether `card` announces the GUI backend protocol this build speaks.
///
/// A card from a crab older than the protocol has no such interface; a card
/// from a newer generation names another one. Either way the window shows
/// that the crab needs an update instead of a half working panel.
pub fn speaks_gui_backend(card: &AgentCard) -> bool {
    card.supported_interfaces
        .iter()
        .any(|interface| interface.protocol_binding == PROTOCOL)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/a2a/agent_card.json");

    /// The card as JSON with the two fields that change per build and per
    /// profile pinned, so the fixture holds still.
    fn pinned(card: &AgentCard) -> serde_json::Value {
        let mut json = serde_json::to_value(card).expect("encode the card");
        json["name"] = "<name>".into();
        json["version"] = "<version>".into();
        json
    }

    #[test]
    fn the_card_matches_its_fixture() {
        let card = build_agent_card("127.0.0.1", 18790, None);
        let fixture: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture is JSON");
        assert_eq!(pinned(&card), fixture);
    }

    #[test]
    fn the_card_announces_the_gui_backend_protocol() {
        let card = build_agent_card("127.0.0.1", 18790, None);
        assert!(speaks_gui_backend(&card));
        let interface = card
            .supported_interfaces
            .iter()
            .find(|interface| interface.protocol_binding == PROTOCOL)
            .expect("the gui-backend interface");
        assert_eq!(interface.url, "http://127.0.0.1:18790/a2a/v1");
    }

    #[test]
    fn an_older_card_without_the_protocol_does_not_speak_it() {
        // N-1: a crab built before the GUI backend existed.
        let mut json: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture is JSON");
        json["supportedInterfaces"]
            .as_array_mut()
            .expect("interfaces")
            .retain(|interface| interface["protocolBinding"] != PROTOCOL);
        let card: AgentCard = serde_json::from_value(json).expect("an N-1 card decodes");
        assert!(!speaks_gui_backend(&card));
    }

    #[test]
    fn a_newer_card_with_unknown_fields_decodes() {
        // N+1: fields this build has never seen, at the top, in an interface
        // and in the capabilities, plus a later protocol generation.
        let mut json: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture is JSON");
        json["signatures"] = serde_json::json!([{ "alg": "none" }]);
        json["capabilities"]["extensions"] = serde_json::json!([]);
        json["supportedInterfaces"]
            .as_array_mut()
            .expect("interfaces")
            .push(serde_json::json!({
                "url": "http://127.0.0.1:18790/a2a/v1",
                "protocolBinding": "gui-backend/2",
                "protocolVersion": "2",
                "tenant": "x",
            }));
        let card: AgentCard = serde_json::from_value(json).expect("an N+1 card decodes");
        assert!(speaks_gui_backend(&card));
        assert_eq!(card.supported_interfaces.len(), 3);
    }

    #[test]
    fn a_card_of_another_generation_only_does_not_speak_this_one() {
        let mut card = build_agent_card("127.0.0.1", 18790, None);
        for interface in &mut card.supported_interfaces {
            if interface.protocol_binding == PROTOCOL {
                interface.protocol_binding = "gui-backend/2".to_string();
            }
        }
        assert!(!speaks_gui_backend(&card));
    }
}
