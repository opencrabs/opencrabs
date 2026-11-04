//! Tests for `BrainLoader::build_core_brain()`.
//!
//! Verifies the lean-injection model: USER.md is baked into every request;
//! AGENTS.md is injected last (owns the brain-file routing model);
//! other files are listed only as an index so the agent can retrieve them on demand via `load_brain_file`.

use crate::brain::prompt_builder::*;

use tempfile::TempDir;

// ── helpers ───────────────────────────────────────────────────────────────────

fn loader(dir: &TempDir) -> BrainLoader {
    BrainLoader::new(dir.path().to_path_buf())
}

fn write(dir: &TempDir, name: &str, content: &str) {
    std::fs::write(dir.path().join(name), content).unwrap();
}

// ── commands & skills awareness index ─────────────────────────────────────────

#[test]
fn core_brain_lists_user_commands_and_skills() {
    let dir = TempDir::new().unwrap();
    // A user-defined slash command in commands.toml (the runtime-added case).
    write(
        &dir,
        "commands.toml",
        "[[commands]]\nname = \"/deploy\"\ndescription = \"Build and ship to prod\"\nprompt = \"deploy now\"\n",
    );
    let brain = loader(&dir).build_core_brain(None);

    // In lazy-tools mode this index is the only way the agent learns a
    // runtime-added command/skill exists — it must be injected inline.
    assert!(
        brain.contains("Available Commands & Skills"),
        "commands/skills index header missing"
    );
    assert!(
        brain.contains("/deploy") && brain.contains("Build and ship to prod"),
        "user command + description must be listed: {brain}"
    );
    // The agent must run a command via the slash_command tool — say so.
    assert!(
        brain.contains("slash_command"),
        "index must tell the agent how to run a command"
    );
    // Built-in skills are always available (embedded) and must be surfaced too,
    // since their description is what the LLM reads to decide when to invoke.
    assert!(brain.contains("Skills —"), "skills section missing");
}

// ── core files are injected ───────────────────────────────────────────────────

#[test]
fn test_core_brain_contains_preamble() {
    let dir = TempDir::new().unwrap();
    let brain = loader(&dir).build_core_brain(None);
    assert!(
        brain.contains("You are OpenCrabs"),
        "preamble must always be present"
    );
}

#[test]
fn test_core_brain_forbids_tests_writing_the_live_config() {
    // The rule that closed #1399 lives in the always-loaded preamble, not
    // only in an on-demand brain file: an agent writing a test must see it
    // before it reaches for the wizard's save without a home override.
    let dir = TempDir::new().unwrap();
    let brain = loader(&dir).build_core_brain(None);
    assert!(
        brain.contains("TESTS NEVER WRITE THE LIVE CONFIG OR KEYS"),
        "the live-config test rule must be in the preamble"
    );
    assert!(
        brain.contains("with_home_override") && brain.contains("#1399"),
        "the rule must name the override the test must use"
    );
}

#[test]
fn test_core_brain_keeps_secrets_out_of_the_session() {
    // #2094: an endpoint that needs a key is called from a script that reads
    // the key itself, so the secret never enters the conversation.
    let dir = TempDir::new().unwrap();
    let brain = loader(&dir).build_core_brain(None);
    assert!(
        brain.contains("SECRETS AND KEYED CALLS"),
        "the keyed-call rule must be in the preamble"
    );
    assert!(
        brain.contains("Python script") && brain.contains("keys.toml"),
        "the rule must name the script and where the key is read from"
    );
}

#[test]
fn preamble_states_table_rendering_mechanics() {
    // #689: telling the agent to "use tables" is not enough — a weaker model
    // collapses them onto one line and Telegram renders raw pipes. The preamble
    // must spell out the mechanics: own line, header, `|---|` separator, each
    // row on its own line, no inline label. Sentinel so this guidance can't be
    // silently dropped in a future edit.
    let dir = TempDir::new().unwrap();
    let brain = loader(&dir).build_core_brain(None);
    assert!(
        brain.contains("WRITE TABLES SO THEY RENDER"),
        "preamble must state the table-rendering mechanics"
    );
    assert!(
        brain.contains("separator row") && brain.contains("own line"),
        "table mechanics must mention the separator row and own-line rows"
    );
}

#[test]
fn test_soul_md_is_injected_in_core() {
    let dir = TempDir::new().unwrap();
    write(&dir, "SOUL.md", "Be helpful and precise.");
    let brain = loader(&dir).build_core_brain(None);
    assert!(
        brain.contains("Be helpful and precise."),
        "SOUL.md must be injected in core brain"
    );
}

#[test]
fn test_user_md_is_injected_in_core() {
    let dir = TempDir::new().unwrap();
    write(&dir, "USER.md", "Name: Alice\nRole: Engineer");
    let brain = loader(&dir).build_core_brain(None);
    assert!(
        brain.contains("Name: Alice"),
        "USER.md must be injected in core brain"
    );
}

// ── contextual files are NOT injected inline ──────────────────────────────────

/// The whole point of the optimisation: contextual files must NOT be baked into
/// every request. The agent should retrieve them via `load_brain_file` only when
/// the request actually needs them.
#[test]
fn test_identity_md_not_injected_in_core_brain() {
    let dir = TempDir::new().unwrap();
    let brain = loader(&dir).build_core_brain(None);
    assert!(!brain.contains("I am OpenCrabs the crab."),);
}

#[test]
fn test_memory_md_not_injected_in_core_brain() {
    let dir = TempDir::new().unwrap();
    write(&dir, "MEMORY.md", "SECRET_PROJECT_NOTES: do not leak.");
    let brain = loader(&dir).build_core_brain(None);
    assert!(
        !brain.contains("SECRET_PROJECT_NOTES"),
        "MEMORY.md content must NOT be injected inline — only listed in the memory index"
    );
}

#[test]
fn test_agents_md_injected_in_core_brain() {
    // AGENTS.md is always-loaded (it owns the enforced hard rules) — its content
    // MUST be injected inline so the safety/permission gates are in context every
    // turn and survive compaction, not merely listed in the on-demand index.
    let dir = TempDir::new().unwrap();
    write(&dir, "AGENTS.md", "WORKSPACE_RULE: always commit");
    let brain = loader(&dir).build_core_brain(None);
    assert!(
        brain.contains("WORKSPACE_RULE"),
        "AGENTS.md content MUST be injected inline (always-loaded hard rules)"
    );
}

#[test]
fn test_tools_md_not_injected_in_core_brain() {
    // TOOLS.md moved back to contextual on 2026-05-03 (eb58c63f) to slim
    // the always-on prompt. Content must NOT be inline; only listed in the
    // index so the agent retrieves it via `load_brain_file` when relevant.
    let dir = TempDir::new().unwrap();
    write(&dir, "TOOLS.md", "TOOL_NOTE: use cargo test");
    let brain = loader(&dir).build_core_brain(None);
    assert!(
        !brain.contains("TOOL_NOTE"),
        "TOOLS.md content must NOT be injected inline (it is contextual)"
    );
    assert!(
        brain.contains("TOOLS.md"),
        "TOOLS.md must still be listed in the context-file index"
    );
}

#[test]
fn test_security_md_not_injected_in_core_brain() {
    let dir = TempDir::new().unwrap();
    write(&dir, "SECURITY.md", "POLICY: never expose keys");
    let brain = loader(&dir).build_core_brain(None);
    assert!(
        !brain.contains("POLICY: never expose keys"),
        "SECURITY.md content must NOT be injected inline"
    );
}

#[test]
fn test_code_md_not_injected_in_core_brain() {
    let dir = TempDir::new().unwrap();
    write(&dir, "CODE.md", "RULE: use snake_case");
    let brain = loader(&dir).build_core_brain(None);
    assert!(
        !brain.contains("RULE: use snake_case"),
        "CODE.md content must NOT be injected inline"
    );
}

// ── memory index is present when contextual files exist ──────────────────────

/// The memory index tells the agent WHAT is available to retrieve. Without it,
/// the agent would not know to call `load_brain_file`.
#[test]
fn test_memory_index_present_when_contextual_files_exist() {
    let dir = TempDir::new().unwrap();
    write(&dir, "MEMORY.md", "some notes");
    let brain = loader(&dir).build_core_brain(None);
    assert!(
        brain.contains("Available Context Files"),
        "memory index section must appear when contextual files exist"
    );
    assert!(
        brain.contains("load_brain_file"),
        "memory index must mention the load_brain_file tool"
    );
}

#[test]
fn test_memory_index_lists_existing_files_only() {
    let dir = TempDir::new().unwrap();
    // MEMORY.md does NOT exist
    let brain = loader(&dir).build_core_brain(None);
    // The dynamic "Available Context Files" index must not OFFER MEMORY.md as
    // loadable when it's absent. Match the index-entry format (`- **MEMORY.md**`)
    // rather than the bare name, since the preamble's brain-file ownership map
    // legitimately references file names regardless of which exist on disk.
    assert!(
        !brain.contains("- **MEMORY.md**"),
        "index must NOT list MEMORY.md as loadable (does not exist)"
    );
}

#[test]
fn test_memory_index_absent_when_no_contextual_files_exist() {
    let dir = TempDir::new().unwrap();
    // Only SOUL.md and USER.md — no contextual files
    write(&dir, "SOUL.md", "I am a crab.");
    write(&dir, "USER.md", "Name: Alice");
    let brain = loader(&dir).build_core_brain(None);
    assert!(
        !brain.contains("Available Context Files"),
        "memory index must be absent when no contextual files exist"
    );
}

// ── brain directory path is exposed ──────────────────────────────────────────

/// Regression guard: when TOOLS.md was always-on it carried the literal
/// `~/.opencrabs/` path inside its body, anchoring the agent to where brain
/// files live. After 2026-05-03 (eb58c63f) TOOLS.md became contextual and that
/// anchor disappeared, leaving the agent grep'ing the codebase to find paths.
/// The index header MUST now expose the brain dir path explicitly.
#[test]
fn test_memory_index_exposes_brain_directory_path() {
    let dir = TempDir::new().unwrap();
    write(&dir, "MEMORY.md", "notes");
    let brain = loader(&dir).build_core_brain(None);
    let dir_str = dir.path().display().to_string();
    assert!(
        brain.contains(&format!("Brain directory: {}/", dir_str))
            || brain.contains(&format!("(in {}/)", dir_str)),
        "context-file index must show the brain directory path so the agent \
         knows where the files live; brain was:\n{}",
        brain
    );
}

#[test]
fn test_memory_index_lists_user_created_md_files() {
    let dir = TempDir::new().unwrap();
    write(&dir, "MEMORY.md", "notes");
    write(&dir, "custom.md", "some user-created notes");
    let brain = loader(&dir).build_core_brain(None);
    assert!(
        brain.contains("custom.md"),
        "user-created .md files must be discoverable in the index"
    );
    assert!(
        !brain.contains("user notes about agentverse"),
        "user-created file CONTENT must NOT be inlined — only the name"
    );
}

// ── on-demand guidance is present ────────────────────────────────────────────

#[test]
fn test_load_guidance_tells_agent_when_to_retrieve() {
    let dir = TempDir::new().unwrap();
    write(&dir, "MEMORY.md", "notes");
    let brain = loader(&dir).build_core_brain(None);
    // Agent must know WHEN to call load_brain_file
    assert!(
        brain.contains("Load proactively when"),
        "brain must contain guidance on when to load contextual files"
    );
}

// ── runtime info ─────────────────────────────────────────────────────────────

#[test]
fn test_runtime_info_included_in_core_brain() {
    let dir = TempDir::new().unwrap();
    let info = RuntimeInfo {
        model: Some("claude-sonnet-4-6".to_string()),
        provider: Some("anthropic".to_string()),
        working_directory: Some("/home/user/project".to_string()),
    };
    let brain = loader(&dir).build_core_brain(Some(&info));
    assert!(brain.contains("claude-sonnet-4-6"));
    assert!(brain.contains("anthropic"));
    assert!(brain.contains("/home/user/project"));
}

// ── SOUL.md position ─────────────────────────────────────────────────────────

/// AGENTS.md must appear as the LAST section in the system prompt — it owns
/// the brain-file ownership/routing model and the model needs it closest to
/// its generation point to navigate brain files after compaction.
#[test]
fn test_agents_md_is_last_section() {
    let dir = TempDir::new().unwrap();
    write(&dir, "SOUL.md", "PERSONALITY_MARKER_UNIQUE_12345");
    write(&dir, "AGENTS.md", "AGENTS_MARKER_UNIQUE_67890");
    write(&dir, "USER.md", "Name: Alice");
    let brain = loader(&dir).build_core_brain(None);
    let agents_pos = brain
        .find("AGENTS_MARKER_UNIQUE_67890")
        .expect("AGENTS.md must be in prompt");
    let soul_pos = brain
        .find("PERSONALITY_MARKER_UNIQUE_12345")
        .expect("SOUL.md must be in prompt");
    assert!(
        agents_pos > soul_pos,
        "AGENTS.md must come AFTER SOUL.md (AGENTS owns the brain-file routing model)"
    );
    // AGENTS.md should be the last brain file in the prompt
    let after_agents = &brain[agents_pos..];
    let trimmed = after_agents.trim();
    assert!(
        trimmed.ends_with("AGENTS_MARKER_UNIQUE_67890") || trimmed.ends_with("---"),
        "AGENTS.md should be the last section in the system prompt, got trailing: {:?}",
        &trimmed[trimmed.len().saturating_sub(100)..]
    );
}

// ── skills section (issue #151) ───────────────────────────────────────────────

/// Skills descriptions must be injected into the system prompt so the LLM
/// can recommend or auto-dispatch them. Previously the `description` field
/// Skills are no longer injected into the system prompt.
/// TOOLS.md has a pointer to `/skills` for on-demand discovery.
#[test]
fn test_skills_section_absent_from_core_brain() {
    let dir = TempDir::new().unwrap();
    let brain = loader(&dir).build_core_brain(None);
    assert!(
        !brain.contains("--- Available Skills ---"),
        "skills section must NOT appear in core brain (moved to on-demand via TOOLS.md pointer)"
    );
}

#[test]
fn test_skills_section_absent_from_full_brain() {
    let dir = TempDir::new().unwrap();
    let brain = loader(&dir).build_system_brain(None);
    assert!(
        !brain.contains("--- Available Skills ---"),
        "skills section must NOT appear in full brain (moved to on-demand via TOOLS.md pointer)"
    );
}

// ── full brain still works (backwards compat) ─────────────────────────────────

#[test]
fn test_full_brain_injects_bounded_files_but_never_memory() {
    let dir = TempDir::new().unwrap();
    write(&dir, "SOUL.md", "core soul");
    write(&dir, "USER.md", "Name: Alice");
    write(&dir, "SECURITY.md", "no exfiltration");
    write(&dir, "MEMORY.md", "long term memory content");
    let brain = loader(&dir).build_system_brain(None);
    assert!(
        brain.contains("Name: Alice"),
        "full brain must include USER.md"
    );
    assert!(
        brain.contains("no exfiltration"),
        "full brain must include SECURITY.md"
    );
    // MEMORY.md is the one brain file with no bound on its size: append-only
    // and growing for as long as the agent is used. Inlining it meant a mature
    // workspace carried the whole file (~99 KB, roughly 25k tokens) in every
    // full-mode system prompt regardless of relevance, while every other
    // surface already treated it as contextual (#995). It now reaches all
    // surfaces the same way: per-turn recall plus the on-demand tools.
    assert!(
        !brain.contains("long term memory content"),
        "full brain must NOT inline MEMORY.md, it is unbounded and contextual"
    );
}

// ── core vs full comparison ───────────────────────────────────────────────────

/// Demonstrates the token saving: the core brain must be strictly smaller
/// than the full brain when contextual files are populated.
#[test]
fn test_core_brain_is_smaller_than_full_brain_when_contextual_files_exist() {
    let dir = TempDir::new().unwrap();
    write(&dir, "SOUL.md", "I am a crab.");
    write(&dir, "USER.md", "Name: Alice\n".repeat(100).as_str()); // 1 200 chars
    write(&dir, "MEMORY.md", "project notes\n".repeat(200).as_str()); // 2 800 chars
    write(&dir, "AGENTS.md", "workspace rules\n".repeat(50).as_str());
    // SECURITY.md and BOOT.md carry the difference now that MEMORY.md is
    // contextual on every surface (#995). Without them full and core inline
    // the same set and the comparison stops exercising anything.
    write(
        &dir,
        "SECURITY.md",
        "no exfiltration\n".repeat(100).as_str(),
    );
    write(&dir, "BOOT.md", "startup steps\n".repeat(100).as_str());

    let core_len = loader(&dir).build_core_brain(None).len();
    let full_len = loader(&dir).build_system_brain(None).len();

    assert!(
        core_len < full_len,
        "core brain ({} bytes) must be smaller than full brain ({} bytes)",
        core_len,
        full_len
    );
}

// ── build_system_brain regression tests (recovered from #248) ────────────────

#[test]
fn test_build_prompt_no_files() {
    let dir = TempDir::new().unwrap();
    let loader = BrainLoader::new(dir.path().to_path_buf());
    let prompt = loader.build_system_brain(None);

    // Should contain brain preamble even with no brain files
    assert!(prompt.contains("You are OpenCrabs"));
    assert!(prompt.contains("CRITICAL RULE"));
}

#[test]
fn test_build_prompt_with_soul() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("SOUL.md"), "I am a helpful crab.").unwrap();

    let loader = BrainLoader::new(dir.path().to_path_buf());
    let prompt = loader.build_system_brain(None);

    assert!(prompt.contains("You are OpenCrabs"));
    assert!(prompt.contains("I am a helpful crab."));
    assert!(prompt.contains("SOUL.md"));
}

#[test]
fn test_build_prompt_with_runtime_info() {
    let dir = TempDir::new().unwrap();
    let loader = BrainLoader::new(dir.path().to_path_buf());
    let info = RuntimeInfo {
        model: Some("claude-sonnet-4-20250514".to_string()),
        provider: Some("anthropic".to_string()),
        working_directory: Some("/home/user/project".to_string()),
    };
    let prompt = loader.build_system_brain(Some(&info));

    assert!(prompt.contains("claude-sonnet-4-20250514"));
    assert!(prompt.contains("anthropic"));
    assert!(prompt.contains("/home/user/project"));
}

#[test]
fn test_skips_empty_files() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("SOUL.md"), "  \n  ").unwrap();

    let loader = BrainLoader::new(dir.path().to_path_buf());
    let prompt = loader.build_system_brain(None);

    // Should NOT contain SOUL.md section header for empty content
    // (the filename may appear in BRAIN_PREAMBLE tool docs, so check for the section format)
    assert!(!prompt.contains("--- SOUL.md ("));
}

#[test]
fn test_override_runtime_model_provider_rewrites_only_runtime_info() {
    let brain = "--- MEMORY.md ---\nModel: keep-me\n\n--- Runtime Info ---\nModel: Qwen-Ambassador/Qwen3.7-Plus\nProvider: modelscope\nWorking directory: ~/x\n\n--- SOUL.md ---\nProvider: also-keep\n";
    let out = override_runtime_model_provider(brain, "claude-fable-5", "claude_cli");
    assert!(out.contains("Model: claude-fable-5\n"));
    assert!(out.contains("Provider: claude_cli\n"));
    assert!(!out.contains("Qwen3.7-Plus"));
    assert!(!out.contains("modelscope"));
    // Lines outside the Runtime Info section are untouched
    assert!(out.contains("Model: keep-me"));
    assert!(out.contains("Provider: also-keep"));
}

#[test]
fn test_override_runtime_model_provider_noop_without_section() {
    let brain = "--- SOUL.md ---\nModel: untouched\n";
    let out = override_runtime_model_provider(brain, "m", "p");
    assert_eq!(out, brain);
}

#[test]
fn test_override_runtime_working_directory_rewrites_only_runtime_info() {
    // #703: the working-dir line is per-session and must be patchable without
    // touching a `Working directory:` string elsewhere in the brain.
    let brain = "--- NOTES.md ---\nWorking directory: ~/keep-me\n\n--- Runtime Info ---\nModel: m\nProvider: p\nWorking directory: ~/srv/animation/ff7_remotion\nHome: ~\n\n--- SOUL.md ---\n";
    let out = override_runtime_working_directory(brain, "~/srv/rs/opencrabs");
    assert!(out.contains("Working directory: ~/srv/rs/opencrabs\n"));
    assert!(!out.contains("ff7_remotion"));
    // Lines outside the Runtime Info section are untouched.
    assert!(out.contains("Working directory: ~/keep-me"));
    // Sibling Runtime Info lines are untouched.
    assert!(out.contains("Model: m\n"));
    assert!(out.contains("Provider: p\n"));
    assert!(out.contains("Home: ~\n"));
}

#[test]
fn test_override_runtime_working_directory_noop_without_section() {
    let brain = "--- SOUL.md ---\nWorking directory: ~/untouched\n";
    let out = override_runtime_working_directory(brain, "~/other");
    assert_eq!(out, brain);
}

// ── Clarify-before-building instruction (#778) ───────────────────────────────
// The preamble had no instruction to gather context before implementing, so an
// ambiguous request got an invented interpretation and the user corrected an
// artifact instead of a premise.

#[test]
fn preamble_instructs_clarifying_before_implementation() {
    let brain = crate::brain::prompt_builder::BRAIN_PREAMBLE;
    assert!(
        brain.contains("CLARIFY BEFORE YOU BUILD"),
        "the clarify instruction must survive preamble edits"
    );
    assert!(
        !brain.contains("follow_up_question"),
        "the removed question tool must not linger in prompt text"
    );
}

#[test]
fn the_clarify_instruction_carries_its_own_brakes() {
    // An agent that interviews before every small task is unusable, so the
    // exemptions are load-bearing, not decoration. Losing them in a future
    // edit would be worse than not having the instruction at all.
    let brain = crate::brain::prompt_builder::BRAIN_PREAMBLE;
    let section = brain
        .split("CLARIFY BEFORE YOU BUILD")
        .nth(1)
        .expect("section present")
        .split("\n\n")
        .next()
        .expect("section body");

    assert!(
        section.contains("Do NOT interrogate"),
        "must keep the exemption for trivial requests"
    );
    assert!(
        section.contains("Never ask what you could answer yourself"),
        "must keep the discoverable-answer brake"
    );
    assert!(
        section.contains("state the assumption"),
        "must keep the do-not-block escape so questions never stall delivery"
    );
}

// ── #1176 G2: follow-up guidance must not restate numeric budgets ───────────

#[test]
fn followup_guidance_is_free_of_magic_numbers() {
    let preamble = crate::brain::prompt_builder::BRAIN_PREAMBLE;
    let line = preamble
        .lines()
        .find(|l| l.starts_with("FOLLOW-UP SUGGESTIONS"))
        .expect("follow-up guidance paragraph must exist in the preamble");
    for token in ["1-4", "2-4", "~60"] {
        assert!(
            !line.contains(token),
            "guidance restates '{token}'; budgets live in code, not prose (#1176)"
        );
    }
    assert!(
        line.contains("suggest_options"),
        "guidance must keep naming the tool"
    );
    assert!(
        line.contains(r#"style=\"primary\""#),
        "guidance must instruct primary recommendation styling"
    );
    assert!(
        line.contains("N. <FirstWord>"),
        "guidance must mention folded button format"
    );
}

// ── #129: headless brains must not advertise suggest_options ────────────────

#[test]
fn strip_followup_suggestions_removes_the_paragraph() {
    let preamble = crate::brain::prompt_builder::BRAIN_PREAMBLE;
    let stripped = crate::brain::prompt_builder::strip_followup_suggestions(preamble);
    assert!(
        !stripped.contains("FOLLOW-UP SUGGESTIONS"),
        "stripped brain must not contain the FOLLOW-UP SUGGESTIONS header"
    );
    assert!(
        !stripped.contains("suggest_options"),
        "stripped brain must not name suggest_options anywhere"
    );
    // Neighbouring paragraphs must survive untouched.
    assert!(
        stripped.contains("CLARIFY BEFORE YOU BUILD")
            && stripped.contains("WEB / GITHUB / BROWSER ROUTING"),
        "paragraphs adjacent to the stripped block must survive"
    );
    // The stock preamble itself still carries the paragraph (interactive).
    assert!(preamble.contains("FOLLOW-UP SUGGESTIONS"));
}

#[test]
fn strip_followup_suggestions_is_idempotent_on_custom_brains() {
    let without = "You are OpenCrabs.\n\nFOLLOW-UP SUGGESTIONS is mentioned in prose but the real paragraph header is absent.\n\nMore text.";
    let out = crate::brain::prompt_builder::strip_followup_suggestions(without);
    assert_eq!(
        out, without,
        "brains without the exact header are unchanged"
    );
}
