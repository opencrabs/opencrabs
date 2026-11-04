use chrono::Local;
use crossterm::event::{KeyCode, KeyEvent};

use super::types::*;
use super::wizard::OnboardingWizard;

impl OnboardingWizard {
    pub(super) fn handle_brain_setup_key(&mut self, event: KeyEvent) -> WizardAction {
        // While generating: Esc or Enter cancel and skip to Complete.
        // All other keys are ignored so the user can't corrupt input mid-stream.
        if self.brain_generating {
            if matches!(event.code, KeyCode::Esc | KeyCode::Enter) {
                self.brain_generating = false;
                self.step = OnboardingStep::Complete;
                return WizardAction::Complete;
            }
            return WizardAction::None;
        }

        // If already generated or errored, Enter advances
        if self.brain_generated || self.brain_error.is_some() {
            if event.code == KeyCode::Enter {
                self.next_step();
                return WizardAction::Complete;
            }
            return WizardAction::None;
        }

        match event.code {
            KeyCode::Esc => {
                // Esc always skips
                self.step = OnboardingStep::Complete;
                return WizardAction::Complete;
            }
            KeyCode::Tab => {
                self.brain_field = match self.brain_field {
                    BrainField::AboutMe => BrainField::AboutAgent,
                    BrainField::AboutAgent => BrainField::AboutMe,
                };
            }
            KeyCode::BackTab => {
                self.brain_field = match self.brain_field {
                    BrainField::AboutMe => BrainField::AboutAgent,
                    BrainField::AboutAgent => BrainField::AboutMe,
                };
            }
            KeyCode::Enter => {
                if self.brain_field == BrainField::AboutAgent {
                    if self.about_me.is_empty() && self.about_opencrabs.is_empty() {
                        // Nothing to work with — skip straight to Complete
                        self.step = OnboardingStep::Complete;
                        return WizardAction::Complete;
                    }
                    // If inputs unchanged from loaded values, skip without regenerating
                    if !self.brain_inputs_changed() && !self.original_about_me.is_empty() {
                        self.step = OnboardingStep::Complete;
                        return WizardAction::Complete;
                    }
                    // Inputs changed or new — normalize into valid markdown and trigger
                    // generation. `normalize_brain_inputs` wraps raw prose in a minimal
                    // markdown skeleton so the model sees consistent structure even if
                    // the user pasted plain text. Preview is implicit: the formatted
                    // values live in `formatted_about_me`/`formatted_about_agent` and
                    // the brain render shows them alongside the raw input.
                    self.normalize_brain_inputs();
                    self.preview_shown = true;
                    return WizardAction::GenerateBrain;
                }
                // Enter on AboutMe moves to AboutAgent
                self.brain_field = BrainField::AboutAgent;
            }
            KeyCode::Char(c) => {
                self.mark_brain_field_edited();
                self.active_brain_field_mut().push(c);
            }
            KeyCode::Backspace => {
                if !self.is_brain_field_edited() && !self.active_brain_field().is_empty() {
                    // First destructive action on untouched template — wipe it
                    self.active_brain_field_mut().clear();
                    self.mark_brain_field_edited();
                } else {
                    self.active_brain_field_mut().pop();
                }
            }
            KeyCode::Delete
                if !self.is_brain_field_edited() && !self.active_brain_field().is_empty() =>
            {
                self.active_brain_field_mut().clear();
                self.mark_brain_field_edited();
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                // Arrow navigation signals intent to edit — mark as touched
                self.mark_brain_field_edited();
            }
            _ => {}
        }
        WizardAction::None
    }

    /// Get reference to the currently focused brain text area
    fn active_brain_field(&self) -> &str {
        match self.brain_field {
            BrainField::AboutMe => &self.about_me,
            BrainField::AboutAgent => &self.about_opencrabs,
        }
    }

    /// Get mutable reference to the currently focused brain text area
    fn active_brain_field_mut(&mut self) -> &mut String {
        match self.brain_field {
            BrainField::AboutMe => &mut self.about_me,
            BrainField::AboutAgent => &mut self.about_opencrabs,
        }
    }

    /// Whether the current field has been explicitly edited (arrow/char input)
    fn is_brain_field_edited(&self) -> bool {
        match self.brain_field {
            BrainField::AboutMe => self.brain_me_edited,
            BrainField::AboutAgent => self.brain_agent_edited,
        }
    }

    /// Mark the current field as touched by user editing
    fn mark_brain_field_edited(&mut self) {
        match self.brain_field {
            BrainField::AboutMe => self.brain_me_edited = true,
            BrainField::AboutAgent => self.brain_agent_edited = true,
        }
    }

    /// Whether brain inputs have been modified since loading from file.
    /// Public so the window's wizard asks the same question the terminal does
    /// before it decides between generating and skipping the step.
    pub fn brain_inputs_changed(&self) -> bool {
        self.about_me != self.original_about_me
            || self.about_opencrabs != self.original_about_opencrabs
    }

    /// Truncate file content to first N chars for preview in the wizard
    pub fn truncate_preview(content: &str, max_chars: usize) -> String {
        let trimmed = content.trim();
        if trimmed.len() <= max_chars {
            trimmed.to_string()
        } else {
            let truncated = &trimmed[..trimmed.floor_char_boundary(max_chars)];
            format!("{}...", truncated.trim_end())
        }
    }

    /// Normalize the two free-form user inputs into valid markdown before
    /// feeding them to the AI. If the user pasted raw prose, wrap it in a
    /// minimal markdown skeleton so the model sees consistent structure.
    /// Called right before generation kicks off.
    pub fn normalize_brain_inputs(&mut self) {
        self.formatted_about_me = auto_format_markdown(&self.about_me, "About Me");
        self.formatted_about_agent = auto_format_markdown(&self.about_opencrabs, "About The Agent");
    }

    /// Build the prompt sent to the AI to generate personalized brain files.
    /// Uses existing workspace files if available, falls back to static templates.
    pub fn build_brain_prompt(&self) -> String {
        let today = Local::now().format("%Y-%m-%d").to_string();
        let workspace = std::path::Path::new(&self.workspace_path);

        // Read current brain files from workspace, fall back to static templates
        let soul_template_static = include_str!("../../docs/reference/templates/SOUL.md");
        let user_template_static = include_str!("../../docs/reference/templates/USER.md");
        let agents_template_static = include_str!("../../docs/reference/templates/AGENTS.md");
        let tools_template_static = include_str!("../../docs/reference/templates/TOOLS.md");
        let memory_template_static = include_str!("../../docs/reference/templates/MEMORY.md");

        let soul_template = std::fs::read_to_string(workspace.join("SOUL.md"))
            .unwrap_or_else(|_| soul_template_static.to_string());
        let user_template = std::fs::read_to_string(workspace.join("USER.md"))
            .unwrap_or_else(|_| user_template_static.to_string());
        let agents_template = std::fs::read_to_string(workspace.join("AGENTS.md"))
            .unwrap_or_else(|_| agents_template_static.to_string());
        let tools_template = std::fs::read_to_string(workspace.join("TOOLS.md"))
            .unwrap_or_else(|_| tools_template_static.to_string());
        let memory_template = std::fs::read_to_string(workspace.join("MEMORY.md"))
            .unwrap_or_else(|_| memory_template_static.to_string());

        format!(
            r#"You are setting up a personal AI agent's brain — its entire workspace of markdown files that define who it is, who its human is, and how it operates.

The user dumped two blocks of info. One about themselves (name, role, links, projects, whatever they shared). One about how they want their agent to be (personality, vibe, behavior). Use EVERYTHING they gave you to personalize ALL six template files below.

=== ABOUT THE USER ===
{about_me}

=== ABOUT THE AGENT ===
{about_opencrabs}

=== TODAY'S DATE ===
{date}

Below are the 5 template files. Replace ALL <placeholder> tags and HTML comments with real values based on what the user provided. Keep the exact markdown structure. Fill what you can from the user's info, leave sensible defaults for anything not provided. Don't invent facts — if the user didn't mention something, use a reasonable placeholder like "TBD" or remove that line.

===TEMPLATE: SOUL.md===
{soul}

===TEMPLATE: USER.md===
{user}

===TEMPLATE: AGENTS.md===
{agents}

===TEMPLATE: TOOLS.md===
{tools}

===TEMPLATE: MEMORY.md===
{memory}

CRITICAL OUTPUT RULES:
1. Start your response IMMEDIATELY with ---SOUL--- (no preamble, no "Here are", no commentary)
2. Use EXACTLY these delimiters on their own line: ---SOUL--- ---USER--- ---AGENTS--- ---TOOLS--- ---MEMORY---
3. After the last section (MEMORY content), STOP. No closing remarks, no "Let me know", no summary, no notes.
4. Do NOT wrap output in markdown code fences (no ```). Raw content only.
5. Each section is the complete file content — valid markdown, ready to save as-is.

---SOUL---
(generated SOUL.md content)


---USER---
(generated USER.md content)
---AGENTS---
(generated AGENTS.md content)
---TOOLS---
(generated TOOLS.md content)
---MEMORY---
(generated MEMORY.md content)"#,
            // Prefer the normalized/auto-formatted markdown if generation
            // went through `normalize_brain_inputs`; fall back to the raw
            // input otherwise so callers that skip normalization still work.
            about_me = if !self.formatted_about_me.is_empty() {
                self.formatted_about_me.as_str()
            } else if self.about_me.is_empty() {
                "Not provided"
            } else {
                self.about_me.as_str()
            },
            about_opencrabs = if !self.formatted_about_agent.is_empty() {
                self.formatted_about_agent.as_str()
            } else if self.about_opencrabs.is_empty() {
                "Not provided"
            } else {
                self.about_opencrabs.as_str()
            },
            date = today,
            soul = soul_template,
            user = user_template,
            agents = agents_template,
            tools = tools_template,
            memory = memory_template,
        )
    }

    /// Store the generated brain content from the AI response.
    ///
    /// The response is parsed leniently: the strict `---NAME---` delimiters
    /// are tried first, and if any are missing we fall back to loose matching
    /// that also accepts common markdown-header variants the model sometimes
    /// emits instead (e.g. `## SOUL.md`, `### IDENTITY`, `**USER**`). As long
    /// as we can recover at least SOUL + USER we count the
    /// generation as a success and fill in whatever else we find.
    pub fn apply_generated_brain(&mut self, response: &str) {
        let parsed = parse_brain_sections(response);

        // Need at least SOUL, USER to consider it a success
        if parsed[0].is_none() || parsed[1].is_none() || parsed[2].is_none() {
            self.brain_error = Some("Couldn't parse AI response — using defaults".to_string());
            self.brain_generating = false;
            return;
        }

        self.generated_soul = parsed[0].clone();
        self.generated_user = parsed[1].clone();
        self.generated_agents = parsed[2].clone();
        self.generated_tools = parsed[3].clone();
        self.generated_memory = parsed[4].clone();

        self.brain_generated = true;
        self.brain_generating = false;
    }
}

/// Parse an AI response into six optional brain sections
/// (SOUL, USER, AGENTS, TOOLS, MEMORY) in that order.
/// Accepts both the strict `---NAME---` delimiters and a variety of
/// header-style fallbacks so a model that forgets the exact format
/// can still be recovered.
pub fn parse_brain_sections(response: &str) -> [Option<String>; 5] {
    const NAMES: [&str; 5] = ["SOUL", "USER", "AGENTS", "TOOLS", "MEMORY"];

    // Each entry: (section_index, byte position of header start, header length)
    let mut hits: Vec<(usize, usize, usize)> = Vec::new();

    for (i, name) in NAMES.iter().enumerate() {
        if let Some((pos, len)) = find_section_header(response, name) {
            hits.push((i, pos, len));
        }
    }

    hits.sort_by_key(|(_, pos, _)| *pos);

    let mut out: [Option<String>; 5] = Default::default();
    for (idx, &(section, pos, len)) in hits.iter().enumerate() {
        let start = pos + len;
        let end = if idx + 1 < hits.len() {
            hits[idx + 1].1
        } else {
            response.len()
        };
        if start > end || start > response.len() {
            continue;
        }
        let content = response[start..end.min(response.len())].trim();
        if !content.is_empty() {
            out[section] = Some(content.to_string());
        }
    }

    out
}

/// Find the first header for `name` in `response`. Returns the byte offset of
/// the header start and its length so the caller can slice content after it.
/// Tries strict `---NAME---`, then common fallbacks the model might emit.
fn find_section_header(response: &str, name: &str) -> Option<(usize, usize)> {
    // 1. Strict delimiter: ---NAME---
    let strict = format!("---{}---", name);
    if let Some(pos) = response.find(&strict) {
        return Some((pos, strict.len()));
    }

    // 2. Line-oriented header fallbacks. Scan line-by-line so we can match
    //    headers that include `.md`, surrounding markdown syntax, or varying
    //    case without false-positives in body text.
    let mut byte_offset = 0usize;
    for line in response.split_inclusive('\n') {
        let trimmed = line.trim();
        if header_line_matches(trimmed, name) {
            // Consume the whole line (including trailing newline) as the header.
            return Some((byte_offset, line.len()));
        }
        byte_offset += line.len();
    }

    None
}

/// Return true if a trimmed line looks like a section header for `name`.
/// Accepts: `## SOUL`, `## SOUL.md`, `### SOUL.md`, `**SOUL**`, `SOUL.md`,
/// `---SOUL---`, `# SOUL`, etc. Case-insensitive on the name.
fn header_line_matches(line: &str, name: &str) -> bool {
    // Strip common markdown decoration characters from both ends, then compare.
    let stripped = line
        .trim_matches(|c: char| {
            c == '#'
                || c == '*'
                || c == '-'
                || c == '='
                || c == '_'
                || c == ':'
                || c.is_whitespace()
        })
        .to_ascii_uppercase();

    let name_upper = name.to_ascii_uppercase();
    stripped == name_upper || stripped == format!("{}.MD", name_upper)
}

/// Detect if text looks like markdown
fn looks_like_markdown(text: &str) -> bool {
    let t = text.trim();
    t.contains('#')
        || t.contains("```")
        || t.contains("- ")
        || t.contains("* ")
        || t.contains("##")
        || t.contains("[")
        || t.contains("![]")
        || t.contains("__")
        || t.contains("**")
        || t.starts_with("> ")
        || t.contains("|")
}

/// Auto-wrap plain text in markdown template
fn auto_format_markdown(input: &str, section_title: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() || looks_like_markdown(trimmed) {
        return trimmed.to_string();
    }
    format!(
        "# {}\n\n{}\n\n## Preferences\n\n- \n\n## Boundaries\n\n- ",
        section_title, trimmed
    )
}
