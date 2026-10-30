//! Concurrent execution of an auto-approved tool batch (#361).
//!
//! When one assistant response carries several tool calls and none of them
//! needs interactive approval, they run concurrently, capped by
//! `[agent] max_concurrent`. Result blocks are reassembled in the original
//! tool_use order (providers require results to match the request order);
//! per-tool bookkeeping runs sequentially after the join so no shared
//! mutable state crosses the parallel region. Batches with any
//! approval-gated tool, single-tool batches, and `max_concurrent = 1` all
//! take the existing sequential path untouched.

use super::tool_loop::{
    build_tool_result_content, extract_path_for_recent_buffer, strip_ansi_output,
};
use super::types::{ProgressCallback, ProgressEvent};
use crate::brain::provider::ContentBlock;
use crate::brain::tools::r#trait::ToolExecutionContext;
use futures::StreamExt;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Outcome of one concurrently executed tool, carried back for in-order
/// assembly and bookkeeping.
struct ToolOutcome {
    tool_id: String,
    tool_name: String,
    tool_input: Value,
    success: bool,
    content: String,
    images: Vec<(String, String)>,
    /// Tool never ran (unknown name / invalid args): a model miss, bucketed
    /// as discovery_miss instead of a tool failure.
    pre_execution_miss: bool,
    /// True when the execution returned Err (vs a failed ToolResult).
    exec_error: bool,
}

/// What the batch produced, in original order.
pub(crate) struct ParallelBatchOutcome {
    pub results: Vec<ContentBlock>,
    pub descriptions: Vec<String>,
    pub outputs: Vec<(bool, String)>,
    /// Count of successful tool runs (drives the phantom-detection counters).
    pub successes: usize,
    /// The cancel token fired mid-batch; later tools did not run.
    pub cancelled: bool,
}

impl super::AgentService {
    /// True when every tool in the batch is auto-approved under the current
    /// flags, so the whole batch can run concurrently. Any approval-gated
    /// tool keeps the batch on the sequential path (interactive prompts
    /// cannot be parallelized sensibly), and so does a batch in which two
    /// calls write one path (#593).
    pub(crate) fn batch_is_parallel_eligible(
        &self,
        tool_uses: &[(String, String, Value)],
        tool_context: &ToolExecutionContext,
        has_override_approval: bool,
    ) -> bool {
        if tool_uses.len() < 2 || self.max_concurrent < 2 {
            return false;
        }
        let all_auto_approved = tool_uses.iter().all(|(_, name, input)| {
            match self.tool_registry.get(name) {
                Some(tool) => {
                    let needs_approval = tool.requires_approval_for_input(input)
                        && (!self.auto_approve_tools || has_override_approval)
                        && !tool_context.auto_approve;
                    !needs_approval
                }
                // Unknown tool: the sequential path has dedicated handling.
                None => false,
            }
        });
        if !all_auto_approved {
            return false;
        }
        // Two calls that write one path must not overlap (#593). Each writer
        // is a whole-file read-modify-write: run concurrently they both read
        // the same original, and the later write replaces the file outright,
        // silently dropping the earlier edit while both report success. The
        // per-path lock is advisory — a contended write waits briefly and
        // then proceeds anyway — so it cannot order two writers within one
        // turn; this gate can, and the ordering costs nothing here. Refusing
        // parallelism sends the batch down the sequential path, where each
        // call reads its predecessor's output and both edits land.
        !self.batch_writes_one_path(tool_uses, tool_context)
    }

    /// True when two calls in the batch resolve to the same write target.
    ///
    /// Targets come from [`crate::brain::tools::Tool::write_target`], so a
    /// tool that addresses its file through a key other than `path` is
    /// covered by its own implementation rather than by a name table here.
    /// A reader has no write target and so never collides with a writer.
    fn batch_writes_one_path(
        &self,
        tool_uses: &[(String, String, Value)],
        tool_context: &ToolExecutionContext,
    ) -> bool {
        let working_directory = tool_context.working_dir();
        let mut seen: Vec<std::path::PathBuf> = Vec::new();
        for (_, name, input) in tool_uses {
            let Some(tool) = self.tool_registry.get(name) else {
                continue;
            };
            let Some(target) = tool.write_target(input, &working_directory) else {
                continue;
            };
            // Canonicalise where the file exists, so two spellings of one
            // file compare equal; a not-yet-created path keeps the resolved
            // form, which is the same resolution the writing tool applies.
            let target = std::fs::canonicalize(&target).unwrap_or(target);
            if seen.contains(&target) {
                return true;
            }
            seen.push(target);
        }
        false
    }

    /// Run the batch concurrently (capped at `max_concurrent`), preserving
    /// result order. Mirrors the sequential non-approval branch: same
    /// progress events, feedback recording, dashboard records, recent-path
    /// capture, and result/output shapes.
    pub(crate) async fn execute_tools_parallel(
        &self,
        session_id: Uuid,
        tool_uses: Vec<(String, String, Value)>,
        tool_context: &ToolExecutionContext,
        cancel_token: Option<&CancellationToken>,
        progress_callback: Option<&ProgressCallback>,
        assistant_msg_id: Uuid,
    ) -> ParallelBatchOutcome {
        let mut approved_context = tool_context.clone();
        approved_context.auto_approve = true;
        let total = tool_uses.len();
        tracing::info!(
            "[TOOL_EXEC] ⚡ Executing {total} tools concurrently (max_concurrent={})",
            self.max_concurrent
        );

        let mut stream = futures::stream::iter(tool_uses.into_iter().map(
            |(tool_id, tool_name, tool_input)| {
                let ctx = approved_context.clone();
                let cb = progress_callback.cloned();
                async move {
                    if let Some(ref cb) = cb {
                        cb(
                            session_id,
                            ProgressEvent::ToolStarted {
                                tool_name: tool_name.clone(),
                                tool_input: tool_input.clone(),
                                tool_call_id: tool_id.clone(),
                            },
                        );
                    }
                    let exec = self
                        .tool_registry
                        .execute(&tool_name, tool_input.clone(), &ctx)
                        .await;
                    let outcome = match exec {
                        Ok(result) => ToolOutcome {
                            tool_id,
                            tool_name,
                            tool_input,
                            success: result.success,
                            content: build_tool_result_content(
                                result.success,
                                result.error,
                                &result.output,
                            ),
                            images: result.images,
                            pre_execution_miss: false,
                            exec_error: false,
                        },
                        Err(e) => ToolOutcome {
                            tool_id,
                            tool_name,
                            tool_input,
                            success: false,
                            content: format!("Tool execution error: {}", e),
                            images: Vec::new(),
                            pre_execution_miss: e.is_pre_execution_miss(),
                            exec_error: true,
                        },
                    };
                    // Completion event fires as each tool finishes (real-time
                    // display), even though bookkeeping waits for the join.
                    if let Some(ref cb) = cb {
                        let summary: String = strip_ansi_output(&outcome.content)
                            .chars()
                            .take(2000)
                            .collect();
                        cb(
                            session_id,
                            ProgressEvent::ToolCompleted {
                                tool_name: outcome.tool_name.clone(),
                                tool_input: outcome.tool_input.clone(),
                                success: outcome.success,
                                summary,
                                tool_call_id: outcome.tool_id.clone(),
                            },
                        );
                    }
                    outcome
                }
            },
        ))
        // `buffered` polls up to N futures concurrently while yielding
        // outcomes in ORIGINAL order — exactly the ordering contract
        // providers need for tool_result blocks.
        .buffered(self.max_concurrent);

        let mut outcomes: Vec<ToolOutcome> = Vec::with_capacity(total);
        let mut cancelled = false;
        loop {
            let next = tokio::select! {
                biased;
                _ = async {
                    match cancel_token {
                        Some(t) => t.cancelled().await,
                        None => std::future::pending().await,
                    }
                } => {
                    tracing::warn!(
                        "🛑 Parallel tool batch cancelled after {}/{total} outcomes",
                        outcomes.len()
                    );
                    cancelled = true;
                    break;
                }
                n = stream.next() => n,
            };
            match next {
                Some(outcome) => outcomes.push(outcome),
                None => break,
            }
        }
        drop(stream); // aborts any still-running futures on cancellation

        // ── In-order bookkeeping and assembly (sequential, single-threaded) ──
        let mut out = ParallelBatchOutcome {
            results: Vec::new(),
            descriptions: Vec::new(),
            outputs: Vec::new(),
            successes: 0,
            cancelled,
        };
        for o in outcomes {
            out.descriptions
                .push(Self::format_tool_summary(&o.tool_name, &o.tool_input));
            if o.success {
                tracing::info!(
                    "[TOOL_EXEC] ✅ Tool '{}' executed successfully, output_len={}",
                    o.tool_name,
                    o.content.len()
                );
                out.successes += 1;
                if let Some(p) = extract_path_for_recent_buffer(
                    &o.tool_name,
                    &o.tool_input,
                    &approved_context.working_directory,
                ) {
                    self.record_recent_path(&approved_context.working_directory, &p);
                }
            } else {
                tracing::error!(
                    "[TOOL_EXEC] ❌ Tool '{}' failed: {}",
                    o.tool_name,
                    o.content.chars().take(200).collect::<String>()
                );
            }
            if o.pre_execution_miss {
                self.record_tool_discovery_miss(
                    session_id,
                    &o.tool_name,
                    Some(&o.tool_input),
                    Some(&o.content),
                );
            } else {
                self.record_tool_feedback(
                    session_id,
                    &o.tool_name,
                    Some(&o.tool_input),
                    o.success,
                    if o.success { None } else { Some(&o.content) },
                );
            }
            // Record tool execution for usage dashboard.
            // #687: skip pre-execution misses (unknown tool / bad args)
            // so garbage names don't pollute stats.
            if !o.pre_execution_miss
                && let Some(pool) = crate::db::global_pool()
            {
                let tool_repo = crate::db::repository::ToolExecutionRepository::new(pool.clone());
                let exec_id = Uuid::new_v4().to_string();
                let mid = assistant_msg_id.to_string();
                let sid = session_id.to_string();
                let tname = o.tool_name.clone();
                let prov = self.provider_name_for_session(session_id);
                let mdl = Some(self.provider_model_for_session(session_id));
                let status = if o.exec_error {
                    "error"
                } else if o.success {
                    "success"
                } else {
                    "error"
                };
                tokio::spawn(async move {
                    if let Err(e) = tool_repo
                        .record(
                            &exec_id,
                            &mid,
                            &sid,
                            &tname,
                            status,
                            Some(&prov),
                            mdl.as_deref(),
                            None,
                        )
                        .await
                    {
                        tracing::error!("[TOOL_EXEC] Failed to record tool execution: {}", e);
                    }
                });

                // Audit READ row (#1705), parallel twin of the sequential
                // site: same flag gate + read-class filter BEFORE any repo
                // work, so an off flag costs a bool check per call.
                if self.audit_recording
                    && let Some(kind) =
                        crate::db::repository::turn_retrieval::retrieval_kind(&o.tool_name)
                {
                    let repo = crate::db::repository::TurnRetrievalRepository::new(pool.clone());
                    let rid = Uuid::new_v4().to_string();
                    let sid = session_id.to_string();
                    let mid = assistant_msg_id.to_string();
                    let tname = o.tool_name.clone();
                    let target = crate::db::repository::turn_retrieval::audit_target(
                        &o.tool_name,
                        &o.tool_input,
                    )
                    .unwrap_or_else(|| "unknown".to_string());
                    let hash = {
                        use sha2::{Digest, Sha256};
                        let mut h = Sha256::new();
                        h.update(o.content.as_bytes());
                        format!("{:x}", h.finalize())
                    };
                    let preview: String = o.content.chars().take(128).collect();
                    tokio::spawn(async move {
                        if let Err(e) = repo
                            .record_retrieval(
                                &rid,
                                &sid,
                                &mid,
                                &tname,
                                kind,
                                &target,
                                Some(&hash),
                                Some(&preview),
                            )
                            .await
                        {
                            tracing::error!("[AUDIT] Failed to record turn retrieval: {e}");
                        }
                    });
                }
            }
            let output_summary: String = strip_ansi_output(&o.content).chars().take(2000).collect();
            out.outputs.push((o.success, output_summary));
            out.results.push(ContentBlock::ToolResult {
                tool_use_id: o.tool_id,
                content: o.content,
                is_error: Some(!o.success),
            });
            for (media_type, data) in o.images {
                out.results.push(ContentBlock::Image {
                    source: crate::brain::provider::ImageSource::Base64 { media_type, data },
                });
            }
        }
        out
    }
}
