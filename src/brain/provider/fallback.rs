//! Fallback Provider
//!
//! Wraps a primary provider with an ordered list of fallbacks.
//! When a provider returns a rate-limit (or other retryable) error, the
//! next provider in the chain is tried. After a successful fallback the
//! chosen provider becomes **sticky** — subsequent calls skip the dead
//! primary entirely until the process exits, so a single 429 doesn't
//! cost 60s of retries on every following turn.
//!
//! A 429 that DECLARES a wait is handled differently, and deliberately so: the
//! provider is parked for exactly the window it named (see
//! [`super::cooldown::CooldownRegistry`]) and the chain routes around it for
//! that long. A fallback that covers a parked provider does NOT become sticky,
//! because nothing failed. The parked provider re-enters on its own when its
//! window lapses, and the session keeps the model it had.

use super::cooldown::CooldownRegistry;
use super::error::{ProviderError, Result};
use super::r#trait::{Provider, ProviderStream};
use super::types::{LLMRequest, LLMResponse};
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// Description of a swap that just occurred — consumed once by the
/// caller (typically the agent service) so it can surface a UI alert.
#[derive(Debug, Clone)]
pub struct SwapEvent {
    pub from_name: String,
    pub from_model: String,
    pub to_name: String,
    pub to_model: String,
    pub reason: String,
}

/// A provider that tries a chain of providers in order on failure.
///
/// `active` indexes into the chain: 0 = primary, 1..=fallbacks.len() = the
/// (n-1)-th fallback. After a successful swap, `active` advances and stays
/// there for the rest of the process — there is no automatic recovery
/// back to the original primary.
pub struct FallbackProvider {
    primary: Arc<dyn Provider>,
    fallbacks: Vec<Arc<dyn Provider>>,
    active: AtomicUsize,
    pending_swap: Mutex<Option<SwapEvent>>,
    /// Providers parked by a 429 that declared its own wait. Owned by this
    /// chain, never a process-wide static, so a reload or a test gets a fresh
    /// one.
    cooldowns: CooldownRegistry,
}

impl FallbackProvider {
    pub fn new(primary: Arc<dyn Provider>, fallbacks: Vec<Arc<dyn Provider>>) -> Self {
        Self {
            primary,
            fallbacks,
            active: AtomicUsize::new(0),
            pending_swap: Mutex::new(None),
            cooldowns: CooldownRegistry::default(),
        }
    }

    /// The provider at chain index `idx`: 0 is the primary, `n` is
    /// `fallbacks[n - 1]`.
    fn provider_at(&self, idx: usize) -> Arc<dyn Provider> {
        if idx == 0 {
            self.primary.clone()
        } else {
            self.fallbacks[idx - 1].clone()
        }
    }

    /// The name of the provider at chain index `idx`.
    fn name_at(&self, idx: usize) -> &str {
        if idx == 0 {
            self.primary.name()
        } else {
            self.fallbacks[idx - 1].name()
        }
    }

    /// Chain order from `start_idx`, with parked entries moved to the back so a
    /// provider in a declared cooldown is used only if everything else failed.
    ///
    /// Recovery is automatic: once a window lapses the entry is no longer
    /// cooling and returns to its natural position. Nothing has to clear it.
    fn candidate_order(&self, start_idx: usize) -> Vec<usize> {
        let total = 1 + self.fallbacks.len();
        let (cool, warm): (Vec<usize>, Vec<usize>) =
            (start_idx..total).partition(|&i| self.cooldowns.is_cooling(self.name_at(i)));
        warm.into_iter().chain(cool).collect()
    }

    /// Park a provider that named its own wait. An error that declares no
    /// duration parks nothing, so a plain 429 or 500 walks the chain exactly as
    /// it did before.
    ///
    /// Returns true when a park happened. A parked provider is away, not dead:
    /// the walk must not promote anyone to sticky on its account, or the chain
    /// would never come back to it.
    fn park_if_declared(&self, entry: &dyn Provider, err: &ProviderError) -> bool {
        match err.declared_cooldown() {
            Some(wait) => {
                self.cooldowns.park(entry.name(), Instant::now() + wait);
                true
            }
            None => false,
        }
    }

    /// Get the currently-active provider (primary or a sticky fallback).
    fn active_provider(&self) -> Arc<dyn Provider> {
        let idx = self.active.load(Ordering::Acquire);
        if idx == 0 {
            self.primary.clone()
        } else {
            self.fallbacks[idx - 1].clone()
        }
    }

    /// Promote a fallback to active. Records a swap event for the caller
    /// to surface in the UI.
    ///
    /// `from_model` is the model the failing request actually carried and
    /// `to_model` the one the succeeding request was actually sent with, after
    /// any remap. Both are required rather than derived from
    /// `Provider::default_model()`: a session pinned to a non-default model
    /// (a `/models` pick) still ran on its own model, and a fallback that
    /// supports that model is not remapped to its default either. Announcing
    /// the defaults named two models that were never used, contradicting the
    /// footer, which reads the session's resolved pair (#918).
    fn promote(&self, new_idx: usize, reason: &str, from_model: &str, to_model: &str) {
        let old_idx = self.active.swap(new_idx, Ordering::AcqRel);
        if old_idx == new_idx {
            return;
        }
        let from = if old_idx == 0 {
            &self.primary
        } else {
            &self.fallbacks[old_idx - 1]
        };
        let to = if new_idx == 0 {
            &self.primary
        } else {
            &self.fallbacks[new_idx - 1]
        };
        let event = SwapEvent {
            from_name: from.name().to_string(),
            from_model: from_model.to_string(),
            to_name: to.name().to_string(),
            to_model: to_model.to_string(),
            reason: reason.to_string(),
        };
        tracing::warn!(
            "Sticky fallback: '{}/{}' → '{}/{}' (reason: {})",
            event.from_name,
            event.from_model,
            event.to_name,
            event.to_model,
            event.reason
        );
        if let Ok(mut slot) = self.pending_swap.lock() {
            *slot = Some(event);
        }
    }

    /// The model `provider` will actually run for `requested`: its own
    /// default when it does not carry the requested one. An empty
    /// `supported_models()` means "unknown", so the request passes through.
    fn model_for(provider: &dyn Provider, requested: &str) -> String {
        let supported = provider.supported_models();
        if !supported.is_empty() && !supported.iter().any(|m| m == requested) {
            provider.default_model().to_string()
        } else {
            requested.to_string()
        }
    }

    /// Build a request for the ACTIVE provider, remapping the model only when
    /// it does not carry it. The active provider runs the model the session
    /// picked; this is a validity guard, not a policy (#1374 keeps it that
    /// way: a swap never invents a model).
    fn remap_request_for_fallback(fb: &dyn Provider, request: &LLMRequest) -> LLMRequest {
        let mut fb_request = request.clone();
        let new_model = Self::model_for(fb, &fb_request.model);
        if new_model != fb_request.model {
            tracing::info!(
                "Fallback '{}': model '{}' not supported — remapping to '{}'",
                fb.name(),
                fb_request.model,
                new_model
            );
            fb_request.model = new_model;
        }
        fb_request
    }

    /// Decide whether an error justifies trying the next provider in the
    /// chain. Beyond transient errors (rate-limit, 5xx, timeout), we also
    /// fall through on model/parameter mismatches — the fallback provider
    /// may support the request after model remapping. Auth errors (401/403)
    /// also trigger fallback: for OAuth providers like Qwen, a 401 after a
    /// failed refresh means the token is dead and the provider is unusable
    /// until re-authenticated.
    ///
    /// Delegates to the shared policy in `provider::error` (#1247) so this
    /// chain, the tool loop's walk and compaction cannot disagree about what
    /// is fatal. Notably it now falls through on hard quota / 402 billing
    /// errors, which this function used to treat as terminal because it gated
    /// on `is_retryable` alone.
    fn should_try_next(err: &ProviderError) -> bool {
        super::error::should_try_next_provider(err)
    }

    /// Final error when every chain entry failed: log the ledger and attach
    /// the chain-exhaustion summary to the last error so the user sees what
    /// was tried instead of one provider's raw failure (#1007).
    fn exhausted(
        last_err: Option<ProviderError>,
        primary_name: &str,
        tried: &[String],
    ) -> ProviderError {
        let err = last_err.unwrap_or_else(|| {
            ProviderError::Internal("FallbackProvider: all providers exhausted".into())
        });
        let summary = super::error::chain_exhausted_summary(
            primary_name,
            &super::error::short_error_reason(&err),
            tried,
        );
        tracing::error!("Fallback chain exhausted: {summary}");
        super::error::with_chain_summary(err, summary)
    }
}

#[async_trait]
impl Provider for FallbackProvider {
    async fn complete(&self, request: LLMRequest) -> Result<LLMResponse> {
        let start_idx = self.active.load(Ordering::Acquire);
        let mut last_err: Option<ProviderError> = None;
        let mut tried: Vec<String> = Vec::new();
        // Set when any entry parked itself for a declared window. A park is not
        // a death: nothing is promoted on its account, so the parked provider
        // returns when its window lapses.
        let mut parked = false;

        // Chain order from the sticky pointer, parked entries last. Always
        // remap: after a restart the sticky index resets to 0 but the request
        // may still carry a model from a previously-active provider
        // (e.g. "openrouter/elephant-alpha" sent to Qwen).
        for idx in self.candidate_order(start_idx) {
            let entry = self.provider_at(idx);
            let entry_request = if idx == start_idx {
                Self::remap_request_for_fallback(entry.as_ref(), &request)
            } else {
                substitute_request(entry.as_ref(), &request)
            };
            let to_model = entry_request.model.clone();
            match entry.complete(entry_request).await {
                Ok(resp) => {
                    // Promote only after an ACTUAL failure. A fallback that
                    // merely covered a parked provider must not become sticky,
                    // or the session would never return to the primary once its
                    // window lapses.
                    if !parked && let Some(err) = last_err.as_ref() {
                        self.promote(
                            idx,
                            super::error::user_facing_reason(err).as_str(),
                            &request.model,
                            &to_model,
                        );
                    }
                    return Ok(resp);
                }
                Err(e) if idx == start_idx && !Self::should_try_next(&e) => return Err(e),
                Err(e) => {
                    if idx == start_idx {
                        tracing::warn!(
                            "Chain entry {} failed: {}, trying next in chain",
                            self.provenance_label(),
                            e
                        );
                        tried.push(self.provenance_label());
                    } else {
                        tracing::warn!(
                            "Chain entry fallback #{} '{}' failed: {}",
                            idx,
                            entry.name(),
                            e
                        );
                        tried.push(entry.name().to_string());
                    }
                    if self.park_if_declared(entry.as_ref(), &e) {
                        parked = true;
                    }
                    last_err = Some(e);
                }
            }
        }

        Err(Self::exhausted(last_err, self.primary.name(), &tried))
    }

    async fn stream(&self, request: LLMRequest) -> Result<ProviderStream> {
        let start_idx = self.active.load(Ordering::Acquire);
        let mut last_err: Option<ProviderError> = None;
        let mut tried: Vec<String> = Vec::new();
        let mut parked = false;

        // Chain order from the sticky pointer, parked entries last.
        // Always remap — see complete() comment.
        for idx in self.candidate_order(start_idx) {
            let entry = self.provider_at(idx);
            let entry_request = if idx == start_idx {
                Self::remap_request_for_fallback(entry.as_ref(), &request)
            } else {
                substitute_request(entry.as_ref(), &request)
            };
            let to_model = entry_request.model.clone();
            match entry.stream(entry_request).await {
                Ok(stream) => {
                    // Promote only after an ACTUAL failure, see complete().
                    if !parked && let Some(err) = last_err.as_ref() {
                        self.promote(
                            idx,
                            super::error::user_facing_reason(err).as_str(),
                            &request.model,
                            &to_model,
                        );
                    }
                    return Ok(stream);
                }
                Err(e) if idx == start_idx && !Self::should_try_next(&e) => return Err(e),
                Err(e) => {
                    if idx == start_idx {
                        tracing::warn!(
                            "Chain entry {} stream failed: {}, trying next in chain",
                            self.provenance_label(),
                            e
                        );
                        tried.push(self.provenance_label());
                    } else {
                        tracing::warn!(
                            "Chain entry fallback #{} '{}' stream failed: {}",
                            idx,
                            entry.name(),
                            e
                        );
                        tried.push(entry.name().to_string());
                    }
                    if self.park_if_declared(entry.as_ref(), &e) {
                        parked = true;
                    }
                    last_err = Some(e);
                }
            }
        }

        Err(Self::exhausted(last_err, self.primary.name(), &tried))
    }

    fn supports_streaming(&self) -> bool {
        self.primary.supports_streaming()
    }

    fn supports_tools(&self) -> bool {
        self.primary.supports_tools()
    }

    fn supports_vision(&self) -> bool {
        self.primary.supports_vision()
    }

    /// Reports the **active** entry, not the primary (#1100).
    ///
    /// This flag decides who executes tool calls. An agentic CLI provider
    /// runs the tools itself inside its own subprocess, so the tool loop
    /// must render its `tool_use` blocks without executing them. Answering
    /// for the primary meant a chain that started on an API provider and
    /// failed over to `claude-cli` kept reporting `false`: the CLI ran each
    /// command and OpenCrabs ran it a second time. Non-idempotent calls
    /// duplicated (two commits, two `gh issue create`s, a `sed -i` applied
    /// twice into code that no longer compiled).
    ///
    /// The mirror case is just as broken: a chain whose primary is a CLI,
    /// after failing over to an API provider, kept reporting `true` and no
    /// one executed the tools at all.
    fn cli_handles_tools(&self) -> bool {
        self.active_provider().cli_handles_tools()
    }

    /// Reports the **active** entry for the same reason as
    /// [`Self::cli_handles_tools`]: whoever owns the conversation history
    /// owns compaction. Compacting on OpenCrabs' side for a CLI that
    /// persists its own session (or skipping compaction for an API provider
    /// that does not) both corrupt the turn.
    fn cli_manages_context(&self) -> bool {
        self.active_provider().cli_manages_context()
    }

    fn name(&self) -> &str {
        // Persistence and config-display name stays as the originally-configured
        // primary, even after a sticky swap. Use `active_subprovider_name()` for
        // the live indicator.
        self.primary.name()
    }

    fn is_fallback_chain(&self) -> bool {
        true
    }

    fn base_url(&self) -> Option<&str> {
        // Forward the primary's base_url so features that identify specific
        // proxies by URL (e.g. dialagram gaslighting strip) keep working even
        // when the provider is wrapped in a fallback chain.
        self.primary.base_url()
    }

    fn default_model(&self) -> &str {
        self.primary.default_model()
    }

    fn supported_models(&self) -> Vec<String> {
        self.primary.supported_models()
    }

    async fn fetch_models(&self) -> Vec<String> {
        self.primary.fetch_models().await
    }

    fn context_window(&self, model: &str) -> Option<u32> {
        self.primary.context_window(model)
    }

    fn configured_context_window(&self) -> Option<u32> {
        self.primary.configured_context_window()
    }

    fn request_timeout(&self) -> Option<std::time::Duration> {
        self.active_provider().request_timeout()
    }

    fn stream_idle_timeout(&self) -> Option<std::time::Duration> {
        self.active_provider().stream_idle_timeout()
    }

    fn thinking_loop_timeout(&self) -> Option<u64> {
        // Follows the ACTIVE provider, not the primary: the guard clocks the
        // stream that is actually running, and a fallback that is phantom-prone
        // needs its own ceiling (#1690).
        self.active_provider().thinking_loop_timeout()
    }

    fn calculate_cost(&self, model: &str, input_tokens: u32, output_tokens: u32) -> f64 {
        self.primary
            .calculate_cost(model, input_tokens, output_tokens)
    }

    fn force_next_fallback(&self, reason: &str, current_model: &str) -> bool {
        let current = self.active.load(Ordering::Acquire);
        let next = current + 1;
        let total = 1 + self.fallbacks.len(); // primary + fallbacks
        if next >= total {
            tracing::warn!(
                "force_next_fallback: no more fallbacks (current={}, total={})",
                current,
                total,
            );
            return false;
        }
        // No request in flight, so the model the next provider will receive is
        // not yet decided. Its default is what an unremapped request lands on.
        let to_model = self.fallbacks[next - 1].default_model().to_string();
        self.promote(next, reason, current_model, &to_model);
        tracing::info!(
            "force_next_fallback: promoted index {} → {} (reason: {})",
            current,
            next,
            reason,
        );
        true
    }

    fn take_swap_event(&self) -> Option<SwapEvent> {
        self.pending_swap.lock().ok().and_then(|mut s| s.take())
    }

    fn take_retry_notices(&self) -> Vec<(u32, u32, String)> {
        // Aggregate retries from the primary and every fallback that was
        // tried this turn, in chain order, so the user sees the full
        // resilience sequence ("⏳ Retry 2/4 — dialagram …" then the
        // fallback's own retries).
        let mut out = self.primary.take_retry_notices();
        for fb in &self.fallbacks {
            out.extend(fb.take_retry_notices());
        }
        out
    }

    fn active_subprovider_name(&self) -> Option<String> {
        let idx = self.active.load(Ordering::Acquire);
        if idx == 0 {
            None
        } else {
            Some(self.fallbacks[idx - 1].name().to_string())
        }
    }

    fn active_subprovider_model(&self) -> Option<String> {
        let idx = self.active.load(Ordering::Acquire);
        if idx == 0 {
            None
        } else {
            Some(self.fallbacks[idx - 1].default_model().to_string())
        }
    }

    fn provenance_label(&self) -> String {
        let idx = self.active.load(Ordering::Acquire);
        if idx == 0 {
            format!("primary '{}'", self.primary.name())
        } else {
            format!("fallback #{} '{}'", idx, self.fallbacks[idx - 1].name())
        }
    }

    /// Mirrors `remap_request_for_fallback`: the primary always runs the
    /// model as requested (nothing remaps it), a fallback runs its own
    /// default when it does not carry that model.
    fn served_model(&self, requested: &str) -> String {
        let idx = self.active.load(Ordering::Acquire);
        if idx == 0 {
            requested.to_string()
        } else {
            Self::model_for(self.fallbacks[idx - 1].as_ref(), requested)
        }
    }
}

/// The model a substitute runs when the chain moves to it after a failure
/// (#1374): its own configured `default_model`, not the model the failed
/// request carried.
///
/// `models[]` is capability (what the endpoint can serve); `default_model` is
/// intent (what this provider is configured to run). Carrying the previous
/// model whenever the substitute happened to list it re-issued a byte-identical
/// request to the same host three times in a row, and the models actually
/// configured for those entries were never tried. A chain exists to try
/// something different. An empty default degrades to the requested model, so
/// a provider that publishes no default is never sent an empty model id.
///
/// Shared by [`FallbackProvider`] and compaction so the two paths cannot
/// disagree about what a substitute runs.
pub(crate) fn substitute_model(fb: &dyn Provider, requested: &str) -> String {
    let default = fb.default_model().trim();
    if default.is_empty() {
        requested.to_string()
    } else {
        default.to_string()
    }
}

/// `request` rebuilt for the substitute provider `fb`: same payload, the
/// model from [`substitute_model`].
pub(crate) fn substitute_request(fb: &dyn Provider, request: &LLMRequest) -> LLMRequest {
    let mut fb_request = request.clone();
    let new_model = substitute_model(fb, &fb_request.model);
    if new_model != fb_request.model {
        tracing::info!(
            "Fallback '{}': running its configured model '{}' instead of the carried '{}'",
            fb.name(),
            new_model,
            fb_request.model
        );
        fb_request.model = new_model;
    }
    fb_request
}
