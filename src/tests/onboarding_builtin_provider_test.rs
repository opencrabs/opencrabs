use crate::brain::provider::opencrabs_store::{self, StoredInstall};
use crate::config::profile::with_home_override;
use crate::tui::onboarding::{
    AuthField, OnboardingStep, OnboardingWizard, PROVIDERS, WizardAction, WizardMode,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn in_temp_home(f: impl FnOnce()) {
    let dir = tempfile::tempdir().expect("tempdir");
    let opencrabs = dir.path().join(".opencrabs");
    std::fs::create_dir_all(&opencrabs).expect("create .opencrabs");
    with_home_override(opencrabs, f);
}

fn enter() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, KeyModifiers::empty())
}

fn provider_index(id: &str) -> usize {
    PROVIDERS
        .iter()
        .position(|p| p.id == id)
        .unwrap_or_else(|| panic!("{id} present in onboarding PROVIDERS"))
}

fn stored_install() -> StoredInstall {
    StoredInstall {
        install_id: "abcd1234abcd1234abcd1234abcd1234".to_string(),
        token: "tok-stored-123".to_string(),
    }
}

/// A wizard on the LLM step with the built-in provider selected and the
/// cursor on the model field, as after the user presses Enter on it.
fn builtin_llm_step(first_run: bool) -> OnboardingWizard {
    let mut wizard = OnboardingWizard::new();
    wizard.is_first_time = first_run;
    wizard.mode = WizardMode::Advanced;
    wizard.step = OnboardingStep::ProviderAuth;
    wizard.auth_field = AuthField::Model;
    wizard.ps.selected_provider = provider_index("opencrabs");
    wizard.ps.selected_model = 0;
    wizard
}

#[test]
fn advanced_first_run_preselects_the_built_in_provider_and_model() {
    in_temp_home(|| {
        let mut wizard = OnboardingWizard::new();
        wizard.is_first_time = true;
        wizard.step = OnboardingStep::Workspace;
        wizard.next_step();

        assert_eq!(wizard.step, OnboardingStep::ProviderAuth);
        assert_eq!(wizard.ps.selected_provider, provider_index("opencrabs"));
        assert_eq!(wizard.ps.selected_model_name(), "cb/deepseek-v4.1-flash");
        assert!(wizard.ps.is_keyless(), "the built-in key is never typed");
    });
}

#[test]
fn returning_user_keeps_their_provider() {
    in_temp_home(|| {
        let mut wizard = OnboardingWizard::new();
        wizard.is_first_time = false;
        wizard.step = OnboardingStep::Workspace;
        let before = wizard.ps.selected_provider;
        wizard.next_step();

        assert_eq!(wizard.ps.selected_provider, before);
        assert_ne!(PROVIDERS[wizard.ps.selected_provider].id, "opencrabs");
    });
}

#[test]
fn enter_on_the_model_without_a_token_asks_for_one_and_stays() {
    in_temp_home(|| {
        let mut wizard = builtin_llm_step(true);
        let action = wizard.handle_key(enter());

        assert!(matches!(action, WizardAction::AcquireBuiltinToken));
        assert_eq!(wizard.step, OnboardingStep::ProviderAuth);
    });
}

#[test]
fn enter_on_the_model_with_a_stored_token_advances() {
    in_temp_home(|| {
        opencrabs_store::save(&stored_install()).expect("save install");
        let mut wizard = builtin_llm_step(true);
        let action = wizard.handle_key(enter());

        assert!(!matches!(action, WizardAction::AcquireBuiltinToken));
        assert_eq!(wizard.step, OnboardingStep::Channels);
        assert!(wizard.error_message.is_none());
    });
}

#[test]
fn choosing_another_provider_does_not_ask_for_a_built_in_token() {
    in_temp_home(|| {
        let mut wizard = builtin_llm_step(true);
        wizard.ps.selected_provider = provider_index("anthropic");
        let action = wizard.handle_key(enter());

        assert!(!matches!(action, WizardAction::AcquireBuiltinToken));
        assert_eq!(wizard.ps.provider_id(), "anthropic");
    });
}

#[test]
fn quick_start_first_run_enter_on_mode_select_goes_to_the_built_in_path() {
    let mut wizard = OnboardingWizard::new();
    wizard.is_first_time = true;
    wizard.mode = WizardMode::QuickStart;
    wizard.step = OnboardingStep::ModeSelect;
    let action = wizard.handle_key(enter());

    assert!(matches!(action, WizardAction::QuickStartBuiltin));
    assert_eq!(
        wizard.step,
        OnboardingStep::ModeSelect,
        "the wizard does not walk its steps"
    );
}

#[test]
fn quick_start_on_a_returning_run_keeps_the_wizard_steps() {
    in_temp_home(|| {
        let mut wizard = OnboardingWizard::new();
        wizard.is_first_time = false;
        wizard.mode = WizardMode::QuickStart;
        wizard.step = OnboardingStep::ModeSelect;
        let action = wizard.handle_key(enter());

        assert!(!matches!(action, WizardAction::QuickStartBuiltin));
        assert_eq!(wizard.step, OnboardingStep::Workspace);
    });
}

#[test]
fn prepare_quick_start_selects_the_built_in_provider() {
    in_temp_home(|| {
        let mut wizard = OnboardingWizard::new();
        wizard.is_first_time = true;
        wizard
            .prepare_quick_start()
            .expect("files and selection prepared");

        assert_eq!(wizard.ps.selected_provider, provider_index("opencrabs"));
        assert_eq!(wizard.ps.selected_model_name(), "cb/deepseek-v4.1-flash");
    });
}
