//! Get this install's caller token for the built-in OpenCrabs provider
//! (#2047, #2198).
//!
//! A stored install is reused as-is, so a second launch never calls the proxy.
//! Without one, the identity is settled before anything is sent: the stored ID
//! is reused when it survived, a fresh one is minted exactly once when it did
//! not, and either way the ID reaches disk before the first request. A lost
//! response or a failed token write can then cost the token, never the
//! identity.
//!
//! A `409` means the proxy already holds a token for this ID, which from here
//! is what a lost token looks like. That is a reissue for the same ID, not a
//! new identity. Only when the proxy refuses the reissue too does this mint a
//! new ID, and it names both IDs in the log, so a split stays traceable instead
//! of silent.

use std::time::Duration;

use super::opencrabs_install::{
    INSTALL_PATH, InstallError, IssuedToken, OPENCRABS_BASE_URL, install_request_body,
    new_install_id, parse_install_response, reissue_request_body,
};
use super::opencrabs_store::{self, StoredInstall};
use crate::config::Config;

/// Longest wait for the proxy to answer one install request.
const TIMEOUT: Duration = Duration::from_secs(15);

/// Return this install's stored token, or request and store a new one.
pub async fn ensure_install() -> Result<StoredInstall, InstallError> {
    ensure_install_at(OPENCRABS_BASE_URL).await
}

/// [`ensure_install`] against an explicit proxy base URL, so tests can point
/// it at a local server.
pub async fn ensure_install_at(base_url: &str) -> Result<StoredInstall, InstallError> {
    if let Some(install) = opencrabs_store::load() {
        return Ok(install);
    }

    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| {
            tracing::error!("Could not build the HTTP client for the built-in provider: {e}");
            InstallError::Unreachable
        })?;
    let url = format!("{}{}", base_url.trim_end_matches('/'), INSTALL_PATH);

    // One identity per acquire, on disk before the first request. Every path
    // below uses this ID until the proxy proves it will not serve it.
    let install_id = match opencrabs_store::load_install_id() {
        Some(id) => id,
        None => mint_and_persist_id()?,
    };

    match post_install(&client, &url, &install_id, false).await {
        Ok(issued) => return store_install(install_id, issued),
        Err(InstallError::AlreadyIssued) => {}
        Err(other) => return Err(other),
    }

    // The proxy knows this ID and this machine does not hold its token, so ask
    // for the token to be reissued against the same ID. The matching proxy
    // change is tracked separately; a proxy without it answers 409 again.
    match post_install(&client, &url, &install_id, true).await {
        Ok(issued) => return store_install(install_id, issued),
        Err(InstallError::AlreadyIssued) => {}
        Err(other) => return Err(other),
    }

    // Last resort: this machine cannot recover the token and the proxy will not
    // reissue for the ID. A new identity keeps the user working, so it is
    // minted here, but never quietly: both IDs are named so a split in the
    // proxy's numbers can be traced back to this line.
    //
    // Only when a token can actually be stored. A `keys.toml` that does not
    // parse refuses every token write, so a fresh identity would be issued and
    // then thrown away, and the next launch would mint yet another one. That
    // is the split this module exists to prevent, so the identity already on
    // disk is kept and the failure is reported (#2198).
    if !opencrabs_store::token_store_is_writable() {
        tracing::error!(
            "The built-in provider would not reissue a token for install {install_id}, and \
             keys.toml cannot accept one, so no new identity is minted. Repair keys.toml and \
             retry: the install ID is kept."
        );
        return Err(InstallError::StorageFailed);
    }
    let fresh = mint_and_persist_id()?;
    tracing::warn!(
        "The built-in provider would not reissue a token for install {install_id}; \
         minting a new install identity {fresh}"
    );
    let issued = post_install(&client, &url, &fresh, false).await?;
    store_install(fresh, issued)
}

/// Mint one identity and put it on disk before the proxy can be asked about it.
///
/// The only place this module generates an ID. A failure to persist it stops
/// the acquire instead of requesting a token for an ID that would be lost on
/// restart, which is how one user used to become two.
fn mint_and_persist_id() -> Result<String, InstallError> {
    let install_id = new_install_id();
    opencrabs_store::write_install_id(&install_id).map_err(|e| {
        tracing::error!("Could not write the install ID before requesting a token: {e}");
        InstallError::StorageFailed
    })?;
    Ok(install_id)
}

/// Store an issued token beside the identity it belongs to.
fn store_install(install_id: String, issued: IssuedToken) -> Result<StoredInstall, InstallError> {
    let install = StoredInstall {
        install_id,
        token: issued.token,
    };
    opencrabs_store::save(&install).map_err(|e| {
        tracing::error!("Could not save the built-in provider token: {e}");
        InstallError::StorageFailed
    })?;
    Ok(install)
}

/// One request to the install route, read through the pure parser.
async fn post_install(
    client: &reqwest::Client,
    url: &str,
    install_id: &str,
    reissue: bool,
) -> Result<IssuedToken, InstallError> {
    let body = if reissue {
        reissue_request_body(install_id)
    } else {
        install_request_body(install_id)
    };
    let response = client.post(url).json(&body).send().await.map_err(|e| {
        tracing::warn!("Built-in provider install request failed: {e}");
        InstallError::Unreachable
    })?;
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    parse_install_response(status, &body)
}

/// Whether this install still needs the built-in provider provisioned (#2151).
///
/// True only when the section exists, it holds no token, and no other provider
/// can serve a request. That last condition is what keeps this from hijacking
/// a configured install: a user with a working provider is never touched,
/// whatever their `[providers.opencrabs]` section happens to say.
///
/// The section ships disabled in the documented example, so `enabled` is
/// deliberately not part of the test. A fresh home has it disabled and a
/// token-less section is exactly the install that cannot answer a request.
pub fn needs_builtin_provision(config: &Config) -> bool {
    let Some(section) = config.providers.opencrabs.as_ref() else {
        return false;
    };
    if section.api_key.as_ref().is_some_and(|key| !key.is_empty()) {
        return false;
    }
    // Ask the registry the factory walks, with the built-in section switched
    // off: whatever it answers is a provider this install can actually use,
    // and such an install must not be touched.
    //
    // Switching the section off instead of reading
    // `active_provider_and_model()` directly is what makes the empty-token
    // case work. That walk only asks whether `api_key` is `Some`, and an empty
    // string is `Some`, so an `api_key = ""` section reports "opencrabs" for an
    // install that has no usable provider at all (`try_create_opencrabs`
    // filters empty tokens out). This is the exact shape of the reported
    // deployment, so trusting the unmodified answer would skip the one case
    // the fix exists for.
    let mut without_builtin = config.clone();
    if let Some(section) = without_builtin.providers.opencrabs.as_mut() {
        section.enabled = false;
    }
    let (provider, _) = without_builtin.providers.active_provider_and_model();
    provider == "none"
}

/// Put an issued token into the in-memory config (#2151).
///
/// `ensure_install` writes `keys.toml`, but every caller here already holds a
/// `Config` it loaded before that write, and `try_create_opencrabs` reads the
/// token from the config. Without this the file would carry a token the
/// factory still cannot see, and the command would fail exactly as before.
pub fn apply_install(config: &mut Config, token: &str) {
    if let Some(section) = config.providers.opencrabs.as_mut() {
        section.enabled = true;
        section.api_key = Some(token.to_string());
    }
}

/// Zero-setup for the entry points that never open the TUI (#2151).
///
/// The TUI provisions on first run (`App::provision_first_run`): it enables the
/// section, asks the proxy for this install's token and rebuilds onto it. A
/// machine that only ever runs `opencrabs run`, `daemon` or the cron scheduler
/// had no such path, so it started with an empty key and reported "No provider
/// configured" on a fresh home, with no way out short of a TTY.
///
/// This is the provider half of that provisioning. It returns whether a token
/// was acquired, and failure is never fatal: the caller's own "no provider"
/// error is more useful than a new one here, and a proxy that is down must not
/// stop a user whose other provider is configured.
///
/// One `POST /v1/install` on first launch only, because `ensure_install`
/// returns the stored install without a request.
pub async fn provision_builtin_provider(config: &mut Config) -> bool {
    if !needs_builtin_provision(config) {
        return false;
    }

    tracing::info!("First run: requesting the built-in provider token");
    if let Err(e) = Config::write_key("providers.opencrabs", "enabled", "true") {
        // Not fatal. The in-memory section is set below either way, and the
        // token write is the part a later launch depends on.
        tracing::warn!("Could not enable the built-in provider in config.toml: {e}");
    }

    let install = match ensure_install().await {
        Ok(install) => install,
        Err(e) => {
            tracing::warn!("Built-in provider token request failed: {e:?}");
            return false;
        }
    };

    apply_install(config, &install.token);

    // The install is set up now, so a later TUI launch goes straight to chat
    // instead of running first-run provisioning a second time.
    crate::tui::onboarding::state::OnboardingState::mark_completed();
    true
}
