//! Codex OAuth Provider — direct OpenAI API with device-code auth
//!
//! Implements the Codex CLI's OAuth device-code flow natively, so users
//! can authenticate through OpenCrabs without needing the `codex` CLI
//! installed. Tokens are stored in `~/.opencrabs/auth/codex.json` and
//! auto-refreshed before expiry.
//!
//! Auth flow:
//! 1. Request device code from `https://auth.openai.com/api/accounts/deviceauth/usercode`
//! 2. User visits verification URL and enters code
//! 3. Poll for tokens at `https://auth.openai.com/api/accounts/deviceauth/token`
//! 4. Store `access_token`, `refresh_token`, `account_id`, `id_token`
//! 5. Use `access_token` as Bearer for OpenAI API calls
//! 6. Auto-refresh before expiry using `refresh_token`
//!
//! API calls go to the standard OpenAI chat completions endpoint
//! (`https://api.openai.com/v1/chat/completions`) using the OAuth
//! access token — same models available as Codex CLI (GPT-5.5, etc.).

use super::custom_openai_compatible::{OpenAIProvider, TokenFn};
use super::error::{ProviderError, Result};
use super::r#trait::{Provider, ProviderStream};
use super::types::*;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Codex OAuth client ID (public client, no secret needed).
/// This is the same client ID used by the Codex CLI and other
/// third-party integrations (OpenCode, term-llm, etc.).
pub const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Device code request endpoint.
pub(crate) const DEVICE_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";

/// Device token polling endpoint.
pub(crate) const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";

/// OAuth token endpoint (for refresh + revoke).
pub(crate) const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

/// OpenAI chat completions endpoint.
pub(crate) const OPENAI_CHAT_URL: &str = "https://api.openai.com/v1/chat/completions";

/// OAuth scopes requested.
const SCOPES: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";

/// Path to the token storage file.
fn token_path() -> std::path::PathBuf {
    crate::config::opencrabs_home()
        .join("auth")
        .join("codex.json")
}

// ─── Token Storage ───────────────────────────────────────────────────────────

/// Stored OAuth tokens for Codex.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexTokens {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    /// Unix timestamp when the access token expires.
    #[serde(default)]
    pub expires_at: u64,
}

impl CodexTokens {
    /// Load tokens from disk.
    pub fn load() -> Option<Self> {
        let path = token_path();
        if !path.exists() {
            return None;
        }
        let content = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Save tokens to disk.
    pub fn save(&self) -> std::io::Result<()> {
        let path = token_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        // chmod 600 on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)?.permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(&path, perms)?;
        }
        Ok(())
    }

    /// Check if the access token is still valid (with 2-minute buffer).
    pub fn is_valid(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.expires_at > now + 120
    }
}

// ─── Device Flow (used during onboarding) ────────────────────────────────────

/// Response from the device code request.
/// OpenAI's actual field names differ from the OAuth 2.0 device flow spec.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceFlowResponse {
    /// OpenAI calls this `device_auth_id` (not `device_code`)
    #[serde(alias = "device_code")]
    pub device_auth_id: String,
    pub user_code: String,
    /// OpenAI doesn't return this — we hardcode the verification URL
    #[serde(default)]
    pub verification_uri: Option<String>,
    /// OpenAI returns `expires_at` (ISO datetime), not `expires_in`
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub expires_in: u64,
    /// OpenAI returns interval as a string ("5"), not a number
    #[serde(default, deserialize_with = "deserialize_string_or_u64")]
    pub interval: u64,
}

fn deserialize_string_or_u64<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Number(n) => n.as_u64().ok_or_else(|| D::Error::custom("expected u64")),
        serde_json::Value::String(s) => s
            .parse::<u64>()
            .map_err(|e| D::Error::custom(e.to_string())),
        _ => Err(D::Error::custom("expected string or number")),
    }
}

/// Intermediate response from deviceauth/token — NOT final tokens.
/// OpenAI's device flow returns a PKCE authorization code, which must
/// then be exchanged at /oauth/token with the code_verifier.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceAuthCodeResponse {
    pub authorization_code: String,
    pub code_challenge: String,
    pub code_verifier: String,
}

/// Final response from /oauth/token after PKCE exchange.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub expires_in: u64,
}

/// Start the OAuth device flow. Returns device code + user code for display.
pub async fn start_device_flow() -> anyhow::Result<DeviceFlowResponse> {
    let client = crate::utils::http::shared();
    let resp = client
        .post(DEVICE_CODE_URL)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .json(&serde_json::json!({
            "client_id": CODEX_CLIENT_ID,
            "scope": SCOPES,
        }))
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Device flow request failed ({}): {}", status, body);
    }

    Ok(resp.json::<DeviceFlowResponse>().await?)
}

/// Poll until the user authorizes the device. Returns an intermediate PKCE authorization code.
/// This does NOT return final tokens — you must call `exchange_device_code_for_tokens` next.
pub async fn poll_for_device_code(
    device_auth_id: &str,
    user_code: &str,
    interval: u64,
) -> anyhow::Result<DeviceAuthCodeResponse> {
    let client = crate::utils::http::shared();
    let poll_interval = Duration::from_secs(interval.max(5));
    let max_wait = Duration::from_secs(15 * 60);
    let start = Instant::now();

    loop {
        tokio::time::sleep(poll_interval).await;

        if start.elapsed() >= max_wait {
            anyhow::bail!("Device auth timed out after 15 minutes");
        }

        let resp = client
            .post(DEVICE_TOKEN_URL)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .json(&serde_json::json!({
                "device_auth_id": device_auth_id,
                "user_code": user_code,
            }))
            .send()
            .await?;

        let status = resp.status();

        // Success — returns { authorization_code, code_challenge, code_verifier }
        if status.is_success() {
            return Ok(resp.json::<DeviceAuthCodeResponse>().await?);
        }

        // 403/404 = still waiting for user to authorize
        if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::NOT_FOUND {
            continue;
        }

        // Other error
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "Device auth failed ({}): {}",
            status,
            &body[..body.len().min(200)]
        );
    }
}

/// Exchange the device authorization code for final tokens via PKCE at /oauth/token.
/// This is step 2 of the Codex device flow.
pub async fn exchange_device_code_for_tokens(
    device_code: &DeviceAuthCodeResponse,
) -> anyhow::Result<TokenResponse> {
    let client = crate::utils::http::shared();
    let redirect_uri = "https://auth.openai.com/deviceauth/callback";

    let resp = client
        .post(OAUTH_TOKEN_URL)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .json(&serde_json::json!({
            "client_id": CODEX_CLIENT_ID,
            "grant_type": "authorization_code",
            "code": device_code.authorization_code,
            "code_verifier": device_code.code_verifier,
            "redirect_uri": redirect_uri,
        }))
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "PKCE token exchange failed ({}): {}",
            status,
            &body[..body.len().min(300)]
        );
    }

    #[derive(Deserialize)]
    struct RawTokenResponse {
        id_token: String,
        access_token: String,
        refresh_token: String,
    }

    let raw: RawTokenResponse = resp.json().await?;

    // Decode id_token JWT to extract account_id and expiry
    let (account_id, expires_in) = decode_jwt_claims(&raw.id_token);

    Ok(TokenResponse {
        access_token: raw.access_token,
        refresh_token: raw.refresh_token,
        id_token: Some(raw.id_token),
        account_id,
        expires_in,
    })
}

/// Extract account_id and expires_in from a JWT id_token (minimal decode, no verification).
fn decode_jwt_claims(id_token: &str) -> (Option<String>, u64) {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() < 2 {
        return (None, 864_000);
    }

    let claims_bytes = match URL_SAFE_NO_PAD.decode(parts[1]) {
        Ok(b) => b,
        Err(_) => return (None, 864_000),
    };
    let claims: serde_json::Value = match serde_json::from_slice(&claims_bytes) {
        Ok(v) => v,
        Err(_) => return (None, 864_000),
    };

    let account_id = claims.get("https://api.openai.com/profile").and_then(|p| {
        p.get("account_id")
            .and_then(|v| v.as_str())
            .map(String::from)
    });

    let expires_in = claims
        .get("exp")
        .and_then(|v| v.as_u64())
        .map(|exp| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            exp.saturating_sub(now)
        })
        .unwrap_or(864_000);

    (account_id, expires_in)
}

/// Exchange tokens for an OpenAI API key (optional — gives longer-lived access).
/// This is the `urn:ietf:params:oauth:grant-type:token-exchange` flow.
pub async fn exchange_for_api_key(id_token: &str) -> anyhow::Result<String> {
    let client = crate::utils::http::shared();
    let resp = client
        .post(OAUTH_TOKEN_URL)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .json(&serde_json::json!({
            "client_id": CODEX_CLIENT_ID,
            "grant_type": "urn:ietf:params:oauth:grant-type:token-exchange",
            "requested_token_type": "openai-api-key",
            "subject_token": id_token,
            "subject_token_type": "urn:ietf:params:oauth:token-type:id_token",
        }))
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("API key exchange failed ({}): {}", status, body);
    }

    #[derive(Deserialize)]
    struct ApiKeyResponse {
        access_token: String,
    }

    let resp: ApiKeyResponse = resp.json().await?;
    Ok(resp.access_token)
}

// ─── Token Manager (runtime, for the provider) ──────────────────────────────

/// Manages the Codex OAuth access token, refreshing it automatically.
pub struct CodexTokenManager {
    /// Current tokens (access + refresh).
    tokens: Arc<RwLock<Option<CodexTokens>>>,
    /// When the current access token expires.
    expires_at: Arc<RwLock<Instant>>,
}

impl Default for CodexTokenManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexTokenManager {
    /// Create a new token manager, loading tokens from disk.
    pub fn new() -> Self {
        let tokens = CodexTokens::load();
        let expires = if let Some(ref t) = tokens {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let remaining = t.expires_at.saturating_sub(now);
            Instant::now() + Duration::from_secs(remaining)
        } else {
            Instant::now()
        };

        Self {
            tokens: Arc::new(RwLock::new(tokens)),
            expires_at: Arc::new(RwLock::new(expires)),
        }
    }

    /// Get the current access token. Refreshes if expired.
    pub async fn ensure_token(&self) -> anyhow::Result<String> {
        {
            let tokens = self.tokens.read().unwrap();
            let expires = self.expires_at.read().unwrap();
            if let Some(ref t) = *tokens
                && *expires > Instant::now() + Duration::from_secs(120)
            {
                return Ok(t.access_token.clone());
            }
        }
        self.refresh().await?;
        let tokens = self.tokens.read().unwrap();
        tokens
            .as_ref()
            .map(|t| t.access_token.clone())
            .ok_or_else(|| anyhow::anyhow!("No tokens available after refresh"))
    }

    /// Get the current cached token without refreshing (sync).
    pub fn get_cached_token(&self) -> Option<String> {
        let tokens = self.tokens.read().unwrap();
        let expires = self.expires_at.read().unwrap();
        if let Some(ref t) = *tokens
            && *expires > Instant::now()
        {
            return Some(t.access_token.clone());
        }
        None
    }

    /// Get the account_id for the ChatGPT-Account-Id header.
    pub fn get_account_id(&self) -> Option<String> {
        self.tokens
            .read()
            .unwrap()
            .as_ref()
            .and_then(|t| t.account_id.clone())
    }

    /// Refresh the access token using the refresh_token.
    pub async fn refresh(&self) -> anyhow::Result<()> {
        let refresh_token = {
            let tokens = self.tokens.read().unwrap();
            tokens
                .as_ref()
                .map(|t| t.refresh_token.clone())
                .ok_or_else(|| anyhow::anyhow!("No refresh token available"))?
        };

        let client = crate::utils::http::shared();
        let resp = client
            .post(OAUTH_TOKEN_URL)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .json(&serde_json::json!({
                "client_id": CODEX_CLIENT_ID,
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
            }))
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Token refresh failed ({}): {}",
                status,
                &body[..body.len().min(300)]
            );
        }

        let token_resp: TokenResponse = resp.json().await?;
        let expires_at_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + token_resp.expires_in;

        let new_tokens = CodexTokens {
            access_token: token_resp.access_token,
            refresh_token: token_resp.refresh_token,
            id_token: token_resp.id_token,
            account_id: token_resp.account_id,
            expires_at: expires_at_unix,
        };

        // Save to disk
        if let Err(e) = new_tokens.save() {
            tracing::warn!("Failed to save refreshed Codex tokens: {}", e);
        }

        // Update in-memory state
        {
            let mut tokens = self.tokens.write().unwrap();
            *tokens = Some(new_tokens);
        }
        {
            let mut expires = self.expires_at.write().unwrap();
            *expires = Instant::now() + Duration::from_secs(token_resp.expires_in);
        }

        tracing::debug!("Codex token refreshed, TTL {}s", token_resp.expires_in);
        Ok(())
    }

    /// Spawn a background task that refreshes the token on a timer.
    pub fn start_background_refresh(self: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                // Sleep until 2 minutes before expiry (min 60s between refreshes)
                let sleep_secs = {
                    let expires = self.expires_at.read().unwrap();
                    let remaining = expires.saturating_duration_since(Instant::now());
                    remaining.as_secs().saturating_sub(120).max(60)
                };

                tokio::time::sleep(Duration::from_secs(sleep_secs)).await;

                if let Err(e) = self.refresh().await {
                    tracing::warn!("Codex token background refresh failed: {}", e);
                    // Retry in 30 seconds on failure
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
            }
        });
    }
}

// ─── Provider Implementation ────────────────────────────────────────────────

/// Codex OAuth provider — uses device-code auth + direct OpenAI API calls.
#[derive(Clone)]
pub struct CodexOAuthProvider {
    token_manager: Arc<CodexTokenManager>,
    default_model: String,
    inner: Arc<OpenAIProvider>,
}

impl CodexOAuthProvider {
    /// Create a new Codex OAuth provider.
    /// Loads tokens from `~/.opencrabs/auth/codex.json`.
    pub fn new() -> Result<Self> {
        let token_manager = Arc::new(CodexTokenManager::new());

        // Verify tokens exist
        if token_manager.get_cached_token().is_none() {
            return Err(ProviderError::Internal(
                "Codex OAuth not authenticated — run /models to authenticate".to_string(),
            ));
        }

        // Start background refresh
        token_manager.clone().start_background_refresh();

        let default_model = "gpt-5.5".to_string();

        // Build a placeholder OpenAIProvider — we'll override the token via token_fn
        let mgr_clone = token_manager.clone();
        let token_fn: TokenFn = Arc::new(move || mgr_clone.get_cached_token().unwrap_or_default());

        let mgr_clone2 = token_manager.clone();
        let account_id = mgr_clone2.get_account_id();

        let mut builder = OpenAIProvider::with_base_url(
            "codex-oauth-managed".to_string(),
            OPENAI_CHAT_URL.to_string(),
        )
        .with_name("codex")
        .with_token_fn(token_fn)
        .with_default_model(default_model.clone());

        // Add ChatGPT-Account-Id header if available
        if let Some(ref aid) = account_id {
            builder =
                builder.with_extra_headers(vec![("ChatGPT-Account-Id".to_string(), aid.clone())]);
        }

        let inner = Arc::new(builder);

        Ok(Self {
            token_manager,
            default_model,
            inner,
        })
    }

    /// Override the default model.
    pub fn with_default_model(mut self, model: String) -> Self {
        self.default_model = model.clone();
        self.inner = Arc::new((*self.inner).clone().with_default_model(model));
        self
    }
}

#[async_trait]
impl Provider for CodexOAuthProvider {
    async fn complete(&self, request: LLMRequest) -> Result<LLMResponse> {
        // Ensure token is valid before making the request
        if self.token_manager.get_cached_token().is_none() {
            self.token_manager
                .ensure_token()
                .await
                .map_err(|e| ProviderError::Internal(format!("Token refresh failed: {}", e)))?;
        }
        self.inner.complete(request).await
    }

    async fn stream(&self, request: LLMRequest) -> Result<ProviderStream> {
        // Ensure token is valid before making the request
        if self.token_manager.get_cached_token().is_none() {
            self.token_manager
                .ensure_token()
                .await
                .map_err(|e| ProviderError::Internal(format!("Token refresh failed: {}", e)))?;
        }
        self.inner.stream(request).await
    }

    fn name(&self) -> &str {
        "codex"
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn supported_models(&self) -> Vec<String> {
        vec![
            "gpt-5.5".to_string(),
            "gpt-5.4".to_string(),
            "gpt-5.4-mini".to_string(),
            "gpt-5.3-codex".to_string(),
            "gpt-5.3-codex-spark".to_string(),
            "gpt-5.2".to_string(),
            "gpt-4o".to_string(),
            "gpt-4o-mini".to_string(),
            "o3".to_string(),
            "o3-mini".to_string(),
            "o4-mini".to_string(),
        ]
    }

    fn context_window(&self, model: &str) -> Option<u32> {
        // GPT-5 family: 400k context
        if model.starts_with("gpt-5") {
            Some(400_000)
        } else if model.starts_with("o3") || model.starts_with("o4") {
            Some(200_000)
        } else {
            Some(128_000)
        }
    }

    fn calculate_cost(&self, model: &str, input_tokens: u32, output_tokens: u32) -> f64 {
        crate::usage::pricing::PricingConfig::load()
            .map(|cfg| cfg.calculate_cost(model, input_tokens, output_tokens))
            .unwrap_or(0.0)
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn supports_vision(&self) -> bool {
        true
    }

    fn cli_handles_tools(&self) -> bool {
        false
    }

    fn cli_manages_context(&self) -> bool {
        false
    }
}
