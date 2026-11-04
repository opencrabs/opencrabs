//! Network half of the WhatsApp live tool block (#2023).
//!
//! One living message per background-completion turn. It opens before the
//! agent runs so the clock covers the thinking, re-renders on a tick, and
//! settles once with the turn's outcome. Progress arrives on a channel and a
//! single task owns every send and edit, the same ordering rule `stream.rs`
//! follows.
//!
//! Failure degrades, never drops. A rejected edit (the 15-minute window
//! closed, or the server refused it) stops the running updates instead of
//! posting a new block on every tick. The settled state still lands once.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use wacore_binary::jid::Jid;
use whatsapp_rust::client::Client;

use uuid::Uuid;

use super::WhatsAppState;
use super::flow_render::{
    FlushAction, Phase, Step, WindowPlan, fold, render, take_trailing_answer,
};
use super::followups::send_suggestions;
use super::rate_limit::GateOutcome;
use crate::brain::agent::{ProgressCallback, ProgressEvent};

/// How often the running block re-renders, so the clock moves.
const TICK: Duration = Duration::from_secs(5);

/// Everything the block needs that is not the channel itself.
pub(crate) struct FlowConfig {
    pub client: Arc<Client>,
    pub jid: Jid,
    pub state: Arc<WhatsAppState>,
    pub rate_limit: crate::config::WaRateLimitConfig,
    pub is_owner: bool,
    /// Whether suggestions may use native buttons (`interactive_buttons`).
    pub interactive_buttons: bool,
    /// Answers posted before the suggestions, so the caller can keep its
    /// final body from repeating them (#2030).
    pub posted: Arc<tokio::sync::Mutex<Vec<String>>>,
}

enum FlowMsg {
    Progress(ProgressEvent),
    /// The turn halted on `suggest_options` with these choices.
    Options {
        session_id: Uuid,
        options: Vec<String>,
    },
    Settle(Phase),
}

/// Handle the turn writes into. Cloneable through its callback.
pub(crate) struct FlowSender {
    tx: mpsc::UnboundedSender<FlowMsg>,
}

impl FlowSender {
    /// Progress callback for `send_push_turn`.
    pub(crate) fn progress_callback(&self) -> ProgressCallback {
        let tx = self.tx.clone();
        Arc::new(move |session_id, event| {
            let msg = match event {
                ProgressEvent::SuggestedOptions(options) if !options.is_empty() => {
                    FlowMsg::Options {
                        session_id,
                        options: options.into_iter().map(|item| item.label).collect(),
                    }
                }
                event => FlowMsg::Progress(event),
            };
            if let Err(e) = tx.send(msg) {
                tracing::warn!(
                    target: "whatsapp",
                    error = %e,
                    "flow event arrived after the block settled; not shown"
                );
            }
        })
    }

    /// Settle the block with the turn's outcome. Call once, after the turn.
    pub(crate) fn settle(&self, phase: Phase) {
        if let Err(e) = self.tx.send(FlowMsg::Settle(phase)) {
            tracing::warn!(
                target: "whatsapp",
                error = %e,
                "flow settle arrived after the block task exited"
            );
        }
    }
}

/// Start the task that owns the living block for one turn.
pub(crate) fn spawn(config: FlowConfig) -> (FlowSender, JoinHandle<()>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<FlowMsg>();
    let handle = tokio::spawn(async move {
        let mut block = Block::new(config);
        block.flush(Phase::Running).await;
        let mut tick = tokio::time::interval(TICK);
        // The first tick fires at once; the flush above already covered it.
        tick.tick().await;
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(FlowMsg::Progress(event)) => {
                        block.apply(&event);
                    }
                    Some(FlowMsg::Options { session_id, options }) => {
                        block.halt_on_options(session_id, options).await;
                    }
                    Some(FlowMsg::Settle(phase)) => {
                        block.flush(phase).await;
                        break;
                    }
                    None => {
                        // Every sender dropped without a settle: the turn
                        // ended some other way. Show it as failed, not running.
                        tracing::warn!(
                            target: "whatsapp",
                            "flow block closed without settling; marking it failed"
                        );
                        block.flush(Phase::Failed).await;
                        break;
                    }
                },
                _ = tick.tick() => {
                    block.flush(Phase::Running).await;
                }
            }
        }
    });
    (FlowSender { tx }, handle)
}

/// The block's state, owned by the single task.
struct Block {
    config: FlowConfig,
    steps: Vec<Step>,
    started: Instant,
    /// Server id of the posted block, once it exists.
    message_id: Option<String>,
    /// When the block was posted, so the edit window can be checked locally.
    posted_at: Option<Instant>,
    /// Edit-window decisions: what the next flush does, and whether editing
    /// is over for this turn (#2150).
    plan: WindowPlan,
}

impl Block {
    fn new(config: FlowConfig) -> Self {
        Self {
            config,
            steps: Vec::new(),
            started: Instant::now(),
            message_id: None,
            posted_at: None,
            plan: WindowPlan::default(),
        }
    }

    fn apply(&mut self, event: &ProgressEvent) {
        fold(&mut self.steps, event);
    }

    /// The turn halted on `suggest_options`. The answer it halted on leaves
    /// the block and posts as a message, then the choices follow it (#2030).
    async fn halt_on_options(&mut self, session_id: Uuid, options: Vec<String>) {
        if let Some(answer) = take_trailing_answer(&mut self.steps) {
            self.flush(Phase::Running).await;
            if self.gate_answer(&answer).await {
                let message = waproto::whatsapp::Message {
                    conversation: Some(answer.clone()),
                    ..Default::default()
                };
                if let Err(e) = self
                    .config
                    .client
                    .send_message(self.config.jid.clone(), message)
                    .await
                {
                    tracing::error!(
                        target: "whatsapp",
                        error = %e,
                        "failed to post the answer held before the suggestions"
                    );
                }
            }
            self.config.posted.lock().await.push(answer);
        }
        send_suggestions(
            self.config.client.clone(),
            self.config.jid.clone(),
            self.config.state.clone(),
            session_id,
            options,
            self.config.interactive_buttons,
        )
        .await;
    }

    /// The answer is real output, so it queues under the daily cap rather
    /// than dropping (#1407). Returns true when it may be sent now.
    async fn gate_answer(&self, answer: &str) -> bool {
        let outcome = self
            .config
            .state
            .rate_limiter
            .gate(
                &self.config.rate_limit,
                &self.config.jid.to_string(),
                answer,
                self.config.is_owner,
            )
            .await;
        matches!(outcome, GateOutcome::SendNow)
    }

    async fn flush(&mut self, phase: Phase) {
        let settled = phase != Phase::Running;
        let text = render(&self.steps, phase, self.started.elapsed());
        let action = self.plan.decide(&text, settled, self.edit_window_open());
        match action {
            // The turn's visible state did not change, or editing is over and
            // this is still a running tick (#2150).
            FlushAction::Skip => {}
            FlushAction::Edit => {
                if !settled && !self.gate().await {
                    return;
                }
                let Some(id) = self.message_id.clone() else {
                    // The plan only edits a message it posted, so a missing id
                    // here means the two drifted. Posting keeps the row alive.
                    self.post(text, settled).await;
                    return;
                };
                let message = waproto::whatsapp::Message {
                    conversation: Some(text.clone()),
                    ..Default::default()
                };
                match self
                    .config
                    .client
                    .edit_message(self.config.jid.clone(), id, message)
                    .await
                {
                    Ok(_) => self.plan.landed(&text),
                    Err(e) => {
                        tracing::warn!(
                            target: "whatsapp",
                            error = %e,
                            "flow block edit rejected; running updates stop for this turn"
                        );
                        self.plan.edit_refused();
                        if settled {
                            self.post(text, settled).await;
                        }
                    }
                }
            }
            FlushAction::Post => self.post(text, settled).await,
        }
    }

    /// Whether the posted block is still young enough to edit. A block with
    /// no recorded post time is treated as editable, so the server stays the
    /// judge rather than a missing timestamp silently freezing the block.
    fn edit_window_open(&self) -> bool {
        self.posted_at
            .map(|at| super::flow_render::within_edit_window(at.elapsed()))
            .unwrap_or(true)
    }

    /// Post a new block and track it as the one future edits target.
    async fn post(&mut self, text: String, settled: bool) {
        if !settled && !self.gate().await {
            return;
        }
        let message = waproto::whatsapp::Message {
            conversation: Some(text.clone()),
            ..Default::default()
        };
        match self
            .config
            .client
            .send_message(self.config.jid.clone(), message)
            .await
        {
            Ok(result) => {
                self.message_id = Some(result.message_id);
                self.posted_at = Some(Instant::now());
                self.plan.landed(&text);
            }
            Err(e) => {
                tracing::warn!(
                    target: "whatsapp",
                    error = %e,
                    "flow block post failed; the turn continues without a live block"
                );
                self.plan.post_refused();
            }
        }
    }

    /// Running updates are one stanza each, so they pace on the bucket and
    /// drop under a saturated daily cap (#1407). The settled state is never
    /// gated: it is the one update that must reach the user.
    async fn gate(&self) -> bool {
        let allowed = self
            .config
            .state
            .rate_limiter
            .gate_ephemeral(&self.config.rate_limit, self.config.is_owner)
            .await;
        if !allowed {
            tracing::debug!(
                target: "whatsapp",
                "daily cap reached; skipping a live tool block update"
            );
        }
        allowed
    }
}
