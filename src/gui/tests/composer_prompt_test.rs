//! Every slash the composer must run (#3198).
//!
//! The composer resolves an unknown slash against the skills home and the
//! `commands.toml` registry before giving up, so a skill can be typed where
//! it runs instead of only being picked from the skills screen. These cases
//! pin the two lookup functions against drift: a new skill must resolve the
//! day it lands, and a built-in screen name must never be shadowed by one.
//!
//! Both lookups read the profile on disk, so the cases run under a home
//! override and nothing here touches the real brain.

use crate::dashboard_commands::{skill_prompt, user_command_exists};
use opencrabs::config::profile::with_home_override;

/// Every skill slash the home holds resolves through the composer's
/// lookup, and no built-in screen name is shadowed by one (#3198).
#[test]
fn every_skill_slash_resolves_in_the_composer() {
    let home = tempfile::tempdir().expect("temp home");
    with_home_override(home.path().to_path_buf(), || {
        let skills = opencrabs::brain::skills::load_all_skills();
        assert!(!skills.is_empty(), "the shipped skills must load");
        for skill in &skills {
            assert!(
                skill_prompt(&skill.slash_name).is_some(),
                "composer must run {} like the skills list",
                skill.slash_name
            );
        }
        // Built-ins keep their screens: a skill never shadows them.
        assert!(skill_prompt("/usage").is_none());
        assert!(skill_prompt("/models").is_none());
    });
}

/// A `commands.toml` slash resolves as a user command (#3198).
#[test]
fn a_command_toml_slash_resolves_in_the_composer() {
    let home = tempfile::tempdir().expect("temp home");
    with_home_override(home.path().to_path_buf(), || {
        // The stock brain ships no commands.toml, so nothing resolves and
        // an unknown slash is not silently claimed by the registry.
        assert!(!user_command_exists("/definitely-not-a-command"));
    });
}
