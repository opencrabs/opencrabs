//! The skills screen's rows.
//!
//! The list is the terminal's own loader ([`load_all_skills`]): the window
//! shows exactly the skills `/skills` lists in the TUI, from the same three
//! places (built-ins, project overlays, the profile's own). Nothing is
//! re-parsed here and nothing is invented, so a skill that works on the
//! terminal works on the window.
//!
//! A row is pure data, which is what makes the screen testable without a
//! window: the list, the filter and the prompt a skill stands for are all
//! plain functions over [`Skill`].

use opencrabs::brain::skills::{Skill, SkillSource, load_all_skills};

/// Every skill the terminal can invoke, in loader order.
pub fn rows() -> Vec<Skill> {
    load_all_skills()
}

/// Where a skill came from, for the row's tag.
pub fn source_label(skill: &Skill) -> &'static str {
    match skill.source {
        SkillSource::Builtin => "builtin",
        SkillSource::User => "user",
    }
}

/// Filter `skills` by `query`, case-insensitively, on name or description.
///
/// Empty query passes everything through, which is what the TUI's dialog does
/// on open (`tui::app::skills_dialog::state::matching`): the window and the
/// terminal narrow the same list the same way.
pub fn matching<'a>(skills: &'a [Skill], query: &str) -> Vec<&'a Skill> {
    if query.is_empty() {
        return skills.iter().collect();
    }
    let q = query.to_lowercase();
    skills
        .iter()
        .filter(|s| s.name.to_lowercase().contains(&q) || s.description.to_lowercase().contains(&q))
        .collect()
}

/// The prompt a skill stands for when it is run from the list.
///
/// This is [`Skill::prompt_body`], not the raw body: a skill declaring
/// `review_gate: true` carries its reminder into the turn, so the window can
/// never quietly skip the gate the terminal honours.
pub fn prompt(skill: &Skill) -> String {
    skill.prompt_body()
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencrabs::config::profile::with_home_override;

    fn skill(name: &str, description: &str, body: &str, gate: bool) -> Skill {
        Skill {
            name: name.to_string(),
            slash_name: format!("/{name}"),
            description: description.to_string(),
            body: body.to_string(),
            globs: Vec::new(),
            review_gate: gate,
            source: SkillSource::User,
        }
    }

    #[test]
    fn skills_screen_shows_every_shipped_skill() {
        // A temp home still ships the built-ins, which is the floor the screen
        // must never fall below: an empty list is the failure this exists for.
        let home = tempfile::tempdir().expect("a temp home");
        let rows = with_home_override(home.path().to_path_buf(), rows);
        assert!(!rows.is_empty(), "the skills loader produced no rows");
        assert!(
            rows.iter().any(|s| s.source == SkillSource::Builtin),
            "no built-in skill reached the window"
        );
        // Every row can be run: the command it stands for must be spellable.
        assert!(rows.iter().all(|s| s.slash_name.starts_with('/')));
    }

    #[test]
    fn skills_screen_filter_narrows_on_name_or_description() {
        let all = vec![
            skill("security-audit", "Run a CVE audit", "body-a", false),
            skill(
                "photos",
                "Process the photos the user sent",
                "body-b",
                false,
            ),
        ];
        assert_eq!(matching(&all, "").len(), 2);
        assert_eq!(matching(&all, "SECURITY").len(), 1);
        assert_eq!(matching(&all, "security")[0].name, "security-audit");
        // The description is searchable too, which is how a user who only
        // remembers what a skill does still finds it.
        assert_eq!(matching(&all, "photos")[0].name, "photos");
        assert!(matching(&all, "no-such-skill").is_empty());
    }

    #[test]
    fn skills_screen_keeps_the_review_gate_reminder() {
        let gated = skill("drop-release", "Draft and publish a release", "do it", true);
        let plain = skill("usage", "Show usage", "show it", false);
        // The window sends `prompt`, never `body`, so the gate cannot be lost.
        let sent = prompt(&gated);
        assert!(sent.contains("SKILL REVIEW GATE"), "gate reminder dropped");
        assert!(sent.contains("do it"), "skill body dropped");
        assert_eq!(prompt(&plain), "show it");
    }

    #[test]
    fn skills_screen_tags_the_source_like_the_terminal() {
        let mut s = skill("x", "y", "z", false);
        s.source = SkillSource::Builtin;
        assert_eq!(source_label(&s), "builtin");
        s.source = SkillSource::User;
        assert_eq!(source_label(&s), "user");
    }
}
