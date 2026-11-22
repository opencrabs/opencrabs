//! GitHub Copilot OAuth Device Flow & Token Management
//!
//! Implements the OAuth device flow for authenticating with GitHub Copilot subscriptions.
//! The flow:
//! 1. Request a device code from GitHub
//! 2. User visits github.com/login/device and enters the code
//! 3. Poll for OAuth access token (long-lived `gho_*` token)
//! 4. Exchange OAuth token for short-lived Copilot token (~30min)
//! 5. Use Copilot token for API calls to api.githubcopilot.com
//! 6. Refresh Copilot token in the background before it expires

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Well-known Copilot OAuth client ID (used by all official Copilot integrations).
pub const COPILOT_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";

/// Copilot chat completions endpoint.
pub const COPILOT_CHAT_URL: &str = "https://api.githubcopilot.com/chat/completions";

/// Copilot models endpoint.
pub const COPILOT_MODELS_URL: &str = "https://api.githubcopilot.com/models";

/// Copilot internal token exchange endpoint.
pub(crate) const COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";

/// GitHub device code request endpoint.
pub(crate) const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";

/// GitHub OAuth token polling endpoint.
pub(crate) const OAUTH_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";

/// Response from the device code request (step 1).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DeviceFlowResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// Response from the Copilot token exchange (step 4).
#[derive(Debug, serde::Deserialize)]
pub(crate) struct CopilotTokenResponse {
    pub(crate) token: String,
    pub(crate) expires_at: u64,
}

/// OAuth polling response — either success or a known error state.
#[derive(Debug, serde::Deserialize)]
pub(crate) struct OAuthPollResponse {
    #[serde(default)]
    pub(crate) access_token: Option<String>,
    #[serde(default)]
    pub(crate) error: Option<String>,
}

/// Standard headers sent with Copilot API requests.
fn copilot_headers() -> Vec<(&'static str, &'static str)> {
    vec![
        ("copilot-integration-id", "vscode-chat"),
        ("editor-version", "vscode/1.99.0"),
        ("editor-plugin-version", "copilot-chat/0.26.0"),
        ("user-agent", "GitHubCopilotChat/0.26.0"),
    ]
}

// ─── Device Flow (used during onboarding) ────────────────────────────────────

/// Start the OAuth device flow. Returns device code + user code for display.
pub async fn start_device_flow() -> anyhow::Result<DeviceFlowResponse> {
    let client = crate::utils::http::shared();
    let resp = client
        .post(DEVICE_CODE_URL)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .json(&serde_json::json!({
            "client_id": COPILOT_CLIENT_ID,
            "scope": "read:user"
        }))
        .send()
        .await?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Device flow request failed: {}", body);
    }

    Ok(resp.json::<DeviceFlowResponse>().await?)
}

/// Poll until the user authorizes the device. Returns the OAuth access token.
/// Blocks (with sleeps) until authorized, denied, or expired.
pub async fn poll_for_oauth_token(device_code: &str, interval: u64) -> anyhow::Result<String> {
    let client = crate::utils::http::shared();
    let mut poll_interval = Duration::from_secs(interval.max(5));

    loop {
        tokio::time::sleep(poll_interval).await;

        let resp = client
            .post(OAUTH_TOKEN_URL)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .json(&serde_json::json!({
                "client_id": COPILOT_CLIENT_ID,
                "device_code": device_code,
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code"
            }))
            .send()
            .await?;

        let poll: OAuthPollResponse = resp.json().await?;

        if let Some(token) = poll.access_token
            && !token.is_empty()
        {
            return Ok(token);
        }

        match poll.error.as_deref() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                poll_interval += Duration::from_secs(5);
                continue;
            }
            Some("expired_token") => anyhow::bail!("Device code expired. Please try again."),
            Some("access_denied") => anyhow::bail!("Authorization denied by user."),
            Some(other) => anyhow::bail!("OAuth error: {}", other),
            None => continue,
        }
    }
}

// ─── Token Manager (runtime, for the provider) ──────────────────────────────

/// Manages the short-lived Copilot API token, refreshing it automatically.
/// The OAuth token is long-lived; the Copilot token expires every ~30 minutes.
pub struct CopilotTokenManager {
    /// Long-lived OAuth token (`gho_*`).
    oauth_token: String,
    /// Short-lived Copilot API token (rotated every ~25 min).
    copilot_token: Arc<RwLock<String>>,
    /// When the current Copilot token expires.
    expires_at: Arc<RwLock<Instant>>,
}

impl CopilotTokenManager {
    /// Create a new token manager. Does NOT fetch the initial token —
    /// call `refresh()` or `ensure_token()` before first use.
    pub fn new(oauth_token: String) -> Self {
        Self {
            oauth_token,
            copilot_token: Arc::new(RwLock::new(String::new())),
            expires_at: Arc::new(RwLock::new(Instant::now())),
        }
    }

    /// Get the current Copilot token. If expired or empty, refreshes first.
    pub async fn ensure_token(&self) -> anyhow::Result<String> {
        {
            let token = self.copilot_token.read().unwrap();
            let expires = self.expires_at.read().unwrap();
            // Refresh if token is empty or expires within 2 minutes
            if !token.is_empty() && *expires > Instant::now() + Duration::from_secs(120) {
                return Ok(token.clone());
            }
        }
        self.refresh().await?;
        Ok(self.copilot_token.read().unwrap().clone())
    }

    /// Get the current cached token without refreshing (sync, for headers callback).
    pub fn get_cached_token(&self) -> String {
        self.copilot_token.read().unwrap().clone()
    }

    /// Exchange the OAuth token for a fresh Copilot API token.
    pub async fn refresh(&self) -> anyhow::Result<()> {
        let client = crate::utils::http::shared();
        let mut req = client
            .get(COPILOT_TOKEN_URL)
            .header("authorization", format!("token {}", self.oauth_token))
            .header("accept", "application/json");

        for (k, v) in copilot_headers() {
            req = req.header(k, v);
        }

        let resp = req.send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Copilot token refresh failed ({}): {}",
                status,
                &body[..body.floor_char_boundary(300)]
            );
        }

        let token_resp: CopilotTokenResponse = resp.json().await?;

        // expires_at is a unix timestamp — convert to Instant
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let ttl = token_resp.expires_at.saturating_sub(now_unix);

        {
            let mut token = self.copilot_token.write().unwrap();
            *token = token_resp.token;
        }
        {
            let mut expires = self.expires_at.write().unwrap();
            *expires = Instant::now() + Duration::from_secs(ttl);
        }

        tracing::debug!("Copilot token refreshed, TTL {}s", ttl);
        Ok(())
    }

    /// Spawn a background task that refreshes the token immediately, then on a timer.
    pub fn start_background_refresh(self: Arc<Self>) {
        tokio::spawn(async move {
            // Immediate first refresh so the token is ready for the first API call
            if let Err(e) = self.refresh().await {
                tracing::warn!("Copilot initial token refresh failed: {}", e);
            }

            loop {
                // Sleep until 2 minutes before expiry (min 60s between refreshes)
                let sleep_secs = {
                    let expires = self.expires_at.read().unwrap();
                    let remaining = expires.saturating_duration_since(Instant::now());
                    remaining.as_secs().saturating_sub(120).max(60)
                };

                tokio::time::sleep(Duration::from_secs(sleep_secs)).await;

                if let Err(e) = self.refresh().await {
                    tracing::warn!("Copilot token background refresh failed: {}", e);
                    // Retry in 30 seconds on failure
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
            }
        });
    }
}

/// Extra headers required for Copilot API requests (for OpenAIProvider).
pub fn copilot_extra_headers() -> Vec<(String, String)> {
    copilot_headers()
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Fetch available models from the Copilot API using an OAuth token.
/// Used during onboarding to populate the model list.
pub async fn fetch_copilot_models(oauth_token: &str) -> anyhow::Result<Vec<String>> {
    // First exchange OAuth token for a Copilot token
    let manager = CopilotTokenManager::new(oauth_token.to_string());
    let copilot_token = manager.ensure_token().await?;

    let client = crate::utils::http::shared();
    let mut req = client
        .get(COPILOT_MODELS_URL)
        .header("authorization", format!("Bearer {}", copilot_token));

    for (k, v) in copilot_headers() {
        req = req.header(k, v);
    }

    let resp = req.send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("Failed to fetch Copilot models: {}", resp.status());
    }

    #[derive(serde::Deserialize)]
    struct ModelEntry {
        id: String,
        #[serde(default)]
        created: i64,
    }
    #[derive(serde::Deserialize)]
    struct ModelsResponse {
        data: Vec<ModelEntry>,
    }

    let body: ModelsResponse = resp.json().await?;
    let mut entries = body.data;
    // Sort newest first (by created timestamp descending)
    entries.sort_by_key(|e| std::cmp::Reverse(e.created));
    Ok(entries.into_iter().map(|m| m.id).collect())
}
