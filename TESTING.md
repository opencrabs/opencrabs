# Testing Guide

Comprehensive test coverage for OpenCrabs. All tests run with:

```bash
cargo test --all-features
```

## Fixture Constants Convention

**Fixture expectations derive from the source constant; they never restate its value.**

```rust
// BAD - silently wrong after any cap change
let options = build_n_options(5);
assert!(!result.success); // "over cap" (cap was 4)

// GOOD - survives every future cap change
let options = build_n_options(MAX_OPTIONS + 1);
assert!(!result.success);
```

- Boundary tests use `CONST - 1` / `CONST` / `CONST + 1`, never literal numbers.
- If a test genuinely pins a literal contract (e.g. a wire-format value fixed by an external spec), prefix a comment naming that external contract.

Born from two real incidents (#646, #1178): hard-coded fixture constants broke on legitimate contract changes and each cost a debug cycle whose only finding was "the test pinned a constant."

## Quick Reference

| Category | Tests | Location |
|----------|------:|----------|
| Tests — A2A Agent Card | 2 | `src/tests/a2a_agent_card_test.rs` |
| Tests — A2A Context Continuity | 4 | `src/tests/a2a_context_continuity_test.rs` |
| Tests — A2A Debate | 8 | `src/tests/a2a_debate_test.rs` |
| Tests — A2A Gate Auth | 8 | `src/tests/a2a_gate_auth_test.rs` |
| Tests — A2A Handler Tasks | 1 | `src/tests/a2a_handler_tasks_test.rs` |
| Tests — A2A Handler | 2 | `src/tests/a2a_handler_test.rs` |
| Tests — A2A Notify Handler | 10 | `src/tests/a2a_notify_handler_test.rs` |
| Tests — A2A Server | 2 | `src/tests/a2a_server_test.rs` |
| Tests — A2A Session Notify | 4 | `src/tests/a2a_session_notify_test.rs` |
| Tests — A2A Types | 6 | `src/tests/a2a_types_test.rs` |
| Tests — Active Skill Tracking | 6 | `src/tests/active_skill_tracking_test.rs` |
| Tests — Agent Approval Policies | 7 | `src/tests/agent_approval_policies_test.rs` |
| Tests — Agent Basic | 12 | `src/tests/agent_basic_test.rs` |
| Tests — Agent Context Tracking | 8 | `src/tests/agent_context_tracking_test.rs` |
| Tests — Agent Helpers React Directive | 4 | `src/tests/agent_helpers_react_directive_test.rs` |
| Tests — Agent Model Selection | 6 | `src/tests/agent_model_selection_test.rs` |
| Tests — Agent Parallel Sessions | 5 | `src/tests/agent_parallel_sessions_test.rs` |
| Tests — Agent Streaming Usage | 5 | `src/tests/agent_streaming_usage_test.rs` |
| Tests — Agent Tool Normalization | 10 | `src/tests/agent_tool_normalization_test.rs` |
| Tests — Altgr Input | 8 | `src/tests/altgr_input_test.rs` |
| Tests — Analysis Intent Nudge | 11 | `src/tests/analysis_intent_nudge_test.rs` |
| Tests — Analytics Db | 15 | `src/tests/analytics_db_test.rs` |
| Tests — Analytics Emitters | 6 | `src/tests/analytics_emitters_test.rs` |
| Tests — Analytics Queries | 9 | `src/tests/analytics_queries_test.rs` |
| Tests — Analytics Render | 16 | `src/tests/analytics_render_test.rs` |
| Tests — Analyze Video Fallback | 9 | `src/tests/analyze_video_fallback_test.rs` |
| Tests — Approval Policy Resolution | 7 | `src/tests/approval_policy_resolution_test.rs` |
| Tests — Auto Title E2e | 1 | `src/tests/auto_title_e2e_test.rs` |
| Tests — Auto Title | 39 | `src/tests/auto_title_test.rs` |
| Tests — Background Indicator | 9 | `src/tests/background_indicator_test.rs` |
| Tests — Background Session | 14 | `src/tests/background_session_test.rs` |
| Tests — Background Task Persistence | 5 | `src/tests/background_task_persistence_test.rs` |
| Tests — Background Task Route | 4 | `src/tests/background_task_route_test.rs` |
| Tests — Background Tasks | 5 | `src/tests/background_tasks_test.rs` |
| Tests — Baseline Merge | 14 | `src/tests/baseline_merge_test.rs` |
| Tests — Bash Blocklist | 6 | `src/tests/bash_blocklist_test.rs` |
| Tests — Bash Failure Classification | 10 | `src/tests/bash_failure_classification_test.rs` |
| Tests — Bash Feedback Enrichment | 16 | `src/tests/bash_feedback_enrichment_test.rs` |
| Tests — Bash Inline | 8 | `src/tests/bash_inline_test.rs` |
| Tests — Bash Interactive Reject | 37 | `src/tests/bash_interactive_reject_test.rs` |
| Tests — Bash Posix Quote | 9 | `src/tests/bash_posix_quote_test.rs` |
| Tests — Bash Retry Loop | 11 | `src/tests/bash_retry_loop_test.rs` |
| Tests — Bash Ssh Detection | 10 | `src/tests/bash_ssh_detection_test.rs` |
| Tests — Bash Toml Blocklist | 11 | `src/tests/bash_toml_blocklist_test.rs` |
| Tests — Bg Push Echo | 30 | `src/tests/bg_push_echo_test.rs` |
| Tests — Bg Resume | 3 | `src/tests/bg_resume_test.rs` |
| Tests — Boot Report | 5 | `src/tests/boot_report_test.rs` |
| Tests — Bot Owner Placeholder Seed | 3 | `src/tests/bot_owner_placeholder_seed_test.rs` |
| Tests — Brain Agent Context | 12 | `src/tests/brain_agent_context_test.rs` |
| Tests — Brain Agent Service Phantom Lang | 11 | `src/tests/brain_agent_service_phantom_lang_test.rs` |
| Tests — Brain Agent Service Phantom | 22 | `src/tests/brain_agent_service_phantom_test.rs` |
| Tests — Brain Agent Service Work Status | 17 | `src/tests/brain_agent_service_work_status_test.rs` |
| Tests — Brain Commands | 6 | `src/tests/brain_commands_test.rs` |
| Tests — Brain File Generic Guard | 4 | `src/tests/brain_file_generic_guard_test.rs` |
| Tests — Brain File Safety | 37 | `src/tests/brain_file_safety_test.rs` |
| Tests — Brain Filter Strip Empty Sections | 15 | `src/tests/brain_filter_strip_empty_sections_test.rs` |
| Tests — Brain Hints | 8 | `src/tests/brain_hints_test.rs` |
| Tests — Brain Live Rebuild | 5 | `src/tests/brain_live_rebuild_test.rs` |
| Tests — Brain Project Overlay | 3 | `src/tests/brain_project_overlay_test.rs` |
| Tests — Brain Prompt Builder | 37 | `src/tests/brain_prompt_builder_test.rs` |
| Tests — Brain Provider Anthropic | 7 | `src/tests/brain_provider_anthropic_test.rs` |
| Tests — Brain Provider Codex Oauth | 6 | `src/tests/brain_provider_codex_oauth_test.rs` |
| Tests — Brain Provider Copilot | 8 | `src/tests/brain_provider_copilot_test.rs` |
| Tests — Brain Provider Error | 4 | `src/tests/brain_provider_error_test.rs` |
| Tests — Brain Provider Factory | 4 | `src/tests/brain_provider_factory_test.rs` |
| Tests — Brain Provider Identity | 14 | `src/tests/brain_provider_identity_test.rs` |
| Tests — Brain Provider Json Repair | 20 | `src/tests/brain_provider_json_repair_test.rs` |
| Tests — Brain Provider Qwen | 13 | `src/tests/brain_provider_qwen_test.rs` |
| Tests — Brain Provider Response Id | 5 | `src/tests/brain_provider_response_id_test.rs` |
| Tests — Brain Provider Trait | 2 | `src/tests/brain_provider_trait_test.rs` |
| Tests — Brain Provider Types | 3 | `src/tests/brain_provider_types_test.rs` |
| Tests — Brain Sections | 10 | `src/tests/brain_sections_test.rs` |
| Tests — Brain Seed Entrypoint | 3 | `src/tests/brain_seed_entrypoint_test.rs` |
| Tests — Brain Self Update | 1 | `src/tests/brain_self_update_test.rs` |
| Tests — Brain Templates | 10 | `src/tests/brain_templates_test.rs` |
| Tests — Brain Tokenizer | 8 | `src/tests/brain_tokenizer_test.rs` |
| Tests — Brain Tools A2A Send | 18 | `src/tests/brain_tools_a2a_send_test.rs` |
| Tests — Brain Tools Bash | 24 | `src/tests/brain_tools_bash_test.rs` |
| Tests — Brain Tools Brave Search | 12 | `src/tests/brain_tools_brave_search_test.rs` |
| Tests — Brain Tools Browser Manager | 12 | `src/tests/brain_tools_browser_manager_test.rs` |
| Tests — Brain Tools Config Tool | 7 | `src/tests/brain_tools_config_tool_test.rs` |
| Tests — Brain Tools Doc Parser | 10 | `src/tests/brain_tools_doc_parser_test.rs` |
| Tests — Brain Tools Dynamic Loader | 6 | `src/tests/brain_tools_dynamic_loader_test.rs` |
| Tests — Brain Tools Dynamic Tool | 25 | `src/tests/brain_tools_dynamic_tool_test.rs` |
| Tests — Brain Tools Edit Notfound | 5 | `src/tests/brain_tools_edit_notfound_test.rs` |
| Tests — Brain Tools Error | 7 | `src/tests/brain_tools_error_test.rs` |
| Tests — Brain Tools Exa Search | 18 | `src/tests/brain_tools_exa_search_test.rs` |
| Tests — Brain Tools Fuzzy | 11 | `src/tests/brain_tools_fuzzy_test.rs` |
| Tests — Brain Tools Hashline Hash | 10 | `src/tests/brain_tools_hashline_hash_test.rs` |
| Tests — Brain Tools Hashline Types | 15 | `src/tests/brain_tools_hashline_types_test.rs` |
| Tests — Brain Tools Load Brain File | 15 | `src/tests/brain_tools_load_brain_file_tests.rs` |
| Tests — Brain Tools Memory Search | 2 | `src/tests/brain_tools_memory_search_test.rs` |
| Tests — Brain Tools Profile List | 6 | `src/tests/brain_tools_profile_list_test.rs` |
| Tests — Brain Tools Read | 6 | `src/tests/brain_tools_read_test.rs` |
| Tests — Brain Tools Registry | 12 | `src/tests/brain_tools_registry_test.rs` |
| Tests — Brain Tools Slash Command | 8 | `src/tests/brain_tools_slash_command_test.rs` |
| Tests — Brain Tools Subagent Reconcile | 12 | `src/tests/brain_tools_subagent_reconcile_test.rs` |
| Tests — Brain Tools Tool Manage | 11 | `src/tests/brain_tools_tool_manage_test.rs` |
| Tests — Brain Tools Trait | 3 | `src/tests/brain_tools_trait_test.rs` |
| Tests — Brain Tools Whatsapp Send | 27 | `src/tests/brain_tools_whatsapp_send_test.rs` |
| Tests — Brain Tools Write Opencrabs File | 20 | `src/tests/brain_tools_write_opencrabs_file_tests.rs` |
| Tests — Brain Tools Write | 5 | `src/tests/brain_tools_write_test.rs` |
| Tests — Brain Verify Inline | 15 | `src/tests/brain_verify_inline_test.rs` |
| Tests — Browser Act | 8 | `src/tests/browser_act_test.rs` |
| Tests — Browser Cdp Endpoint | 4 | `src/tests/browser_cdp_endpoint_test.rs` |
| Tests — Browser Close | 6 | `src/tests/browser_close_test.rs` |
| Tests — Browser Default | 12 | `src/tests/browser_default_test.rs` |
| Tests — Browser Drop | 2 | `src/tests/browser_drop_test.rs` |
| Tests — Browser E2e | 4 | `src/tests/browser_e2e_test.rs` |
| Tests — Browser Eval Cap | 5 | `src/tests/browser_eval_cap_test.rs` |
| Tests — Browser Events | 6 | `src/tests/browser_events_test.rs` |
| Tests — Browser Find | 9 | `src/tests/browser_find_test.rs` |
| Tests — Browser Health | 4 | `src/tests/browser_health_test.rs` |
| Tests — Browser Inventory | 10 | `src/tests/browser_inventory_test.rs` |
| Tests — Browser Locks | 5 | `src/tests/browser_locks_test.rs` |
| Tests — Browser Oopif | 3 | `src/tests/browser_oopif_test.rs` |
| Tests — Browser Profile Wait | 4 | `src/tests/browser_profile_wait_test.rs` |
| Tests — Browser Screenshot Surface | 2 | `src/tests/browser_screenshot_surface_test.rs` |
| Tests — Browser Session | 4 | `src/tests/browser_session_test.rs` |
| Tests — Browser Stealth | 6 | `src/tests/browser_stealth_test.rs` |
| Tests — Build User Message Image | 3 | `src/tests/build_user_message_image_test.rs` |
| Tests — Bundled Plans | 20 | `src/tests/bundled_plans_test.rs` |
| Tests — Cancel Persists Completion | 8 | `src/tests/cancel_persists_completion_test.rs` |
| Tests — Cancel Restore Query | 2 | `src/tests/cancel_restore_query_test.rs` |
| Tests — Candle Whisper | 6 | `src/tests/candle_whisper_test.rs` |
| Tests — Channel Action | 4 | `src/tests/channel_action_test.rs` |
| Tests — Channel Approval Owner Gate | 2 | `src/tests/channel_approval_owner_gate_test.rs` |
| Tests — Channel Command Media Marker | 3 | `src/tests/channel_command_media_marker_test.rs` |
| Tests — Channel Command Owner Gate | 5 | `src/tests/channel_command_owner_gate_test.rs` |
| Tests — Channel Commands | 21 | `src/tests/channel_commands_test.rs` |
| Tests — Channel Deny By Default | 4 | `src/tests/channel_deny_by_default_test.rs` |
| Tests — Channel Factory Subagent | 2 | `src/tests/channel_factory_subagent_test.rs` |
| Tests — Channel Restart Target | 6 | `src/tests/channel_restart_target_test.rs` |
| Tests — Channel Route Expectation | 5 | `src/tests/channel_route_expectation_test.rs` |
| Tests — Channel Search | 32 | `src/tests/channel_search_test.rs` |
| Tests — Channel Session Resolve | 10 | `src/tests/channel_session_resolve_test.rs` |
| Tests — Channel User Command Owner Gate | 7 | `src/tests/channel_user_command_owner_gate_test.rs` |
| Tests — Channels Telegram Cowork | 11 | `src/tests/channels_telegram_cowork_test.rs` |
| Tests — Channels Telegram Session Resolve | 8 | `src/tests/channels_telegram_session_resolve_test.rs` |
| Tests — Channels | 5 | `src/tests/channels_tests.rs` |
| Tests — Channels Voice Service | 10 | `src/tests/channels_voice_service_test.rs` |
| Tests — Chat Expand Anchor | 14 | `src/tests/chat_expand_anchor_test.rs` |
| Tests — Chat Fold Deliverable | 6 | `src/tests/chat_fold_deliverable_test.rs` |
| Tests — Chunk Hash Cache | 3 | `src/tests/chunk_hash_cache_test.rs` |
| Tests — Claude CLI Model | 7 | `src/tests/claude_cli_model_test.rs` |
| Tests — CLI Agent Session Resume | 4 | `src/tests/cli_agent_session_resume_test.rs` |
| Tests — CLI Arg Too Long | 2 | `src/tests/cli_arg_too_long_test.rs` |
| Tests — CLI Context Window | 5 | `src/tests/cli_context_window_test.rs` |
| Tests — CLI Headless Tools | 4 | `src/tests/cli_headless_tools_test.rs` |
| Tests — CLI Session Id Prefix | 9 | `src/tests/cli_session_id_prefix_test.rs` |
| Tests — CLI Session Set Model | 6 | `src/tests/cli_session_set_model_test.rs` |
| Tests — Cli Silent Failure | 5 | `src/tests/cli_silent_failure_test.rs` |
| Tests — CLI Supported Models | 14 | `src/tests/cli_supported_models_test.rs` |
| Tests — CLI | 28 | `src/tests/cli_test.rs` |
| Tests — CLIck To Expand | 7 | `src/tests/click_to_expand_test.rs` |
| Tests — CLIpboard Image Paste | 2 | `src/tests/clipboard_image_paste_test.rs` |
| Tests — Codex CLI | 10 | `src/tests/codex_cli_test.rs` |
| Tests — Collapse Build Output | 9 | `src/tests/collapse_build_output_test.rs` |
| Tests — Collapse Home | 8 | `src/tests/collapse_home_test.rs` |
| Tests — Command Code CLI | 6 | `src/tests/command_code_cli_test.rs` |
| Tests — Command Handle Strip | 10 | `src/tests/command_handle_strip_test.rs` |
| Tests — Command Label | 12 | `src/tests/command_label_test.rs` |
| Tests — Command Rich Table | 5 | `src/tests/command_rich_table_test.rs` |
| Tests — Compaction Background | 10 | `src/tests/compaction_background_test.rs` |
| Tests — Compaction Fallback Chain | 19 | `src/tests/compaction_fallback_chain_test.rs` |
| Tests — Compaction Prompts | 16 | `src/tests/compaction_prompts_test.rs` |
| Tests — Compaction Signal | 16 | `src/tests/compaction_signal_test.rs` |
| Tests — Compaction Single Continuation Path | 2 | `src/tests/compaction_single_continuation_path_test.rs` |
| Tests — Compaction | 28 | `src/tests/compaction_test.rs` |
| Tests — Compaction Truncation Marker | 5 | `src/tests/compaction_truncation_marker_test.rs` |
| Tests — Confidential Read Deny | 5 | `src/tests/confidential_read_deny_test.rs` |
| Tests — Config Alias Merge | 17 | `src/tests/config_alias_merge_test.rs` |
| Tests — Config Dotted Caps | 6 | `src/tests/config_dotted_caps_test.rs` |
| Tests — Config Guard | 5 | `src/tests/config_guard_test.rs` |
| Tests — Config Last Good Recovery | 3 | `src/tests/config_last_good_recovery_test.rs` |
| Tests — Config Live Home Guard | 5 | `src/tests/config_live_home_guard_test.rs` |
| Tests — Config Load Status Isolation | 4 | `src/tests/config_load_status_isolation_test.rs` |
| Tests — Config Memory External | 7 | `src/tests/config_memory_external_test.rs` |
| Tests — Config Owner Seed Migration | 7 | `src/tests/config_owner_seed_migration_test.rs` |
| Tests — Config Phantom Xiaomi Default | 2 | `src/tests/config_phantom_xiaomi_default_test.rs` |
| Tests — Config Provider Registry | 3 | `src/tests/config_provider_registry_test.rs` |
| Tests — Config Reload Reason | 7 | `src/tests/config_reload_reason_test.rs` |
| Tests — Config Repair | 7 | `src/tests/config_repair_test.rs` |
| Tests — Config Secrets | 5 | `src/tests/config_secrets_test.rs` |
| Tests — Config Section Resolve | 9 | `src/tests/config_section_resolve_test.rs` |
| Tests — Config Seed On Create | 4 | `src/tests/config_seed_on_create_test.rs` |
| Tests — Config Types Loader | 25 | `src/tests/config_types_loader_test.rs` |
| Tests — Config Update | 4 | `src/tests/config_update_test.rs` |
| Tests — Config Voice Migration | 5 | `src/tests/config_voice_migration_test.rs` |
| Tests — Config Watcher | 5 | `src/tests/config_watcher_test.rs` |
| Tests — Config Write Path | 7 | `src/tests/config_write_path_test.rs` |
| Tests — Config Write Types | 5 | `src/tests/config_write_types_test.rs` |
| Tests — Content Vectors Heal | 3 | `src/tests/content_vectors_heal_test.rs` |
| Tests — Context Provider Anchor | 6 | `src/tests/context_provider_anchor_test.rs` |
| Tests — Context Store Concurrent Save | 3 | `src/tests/context_store_concurrent_save_test.rs` |
| Tests — Context Window | 14 | `src/tests/context_window_test.rs` |
| Tests — Core Tool Names | 3 | `src/tests/core_tool_names_test.rs` |
| Tests — Corrupted Tool Call | 7 | `src/tests/corrupted_tool_call_test.rs` |
| Tests — Cowork Connect | 2 | `src/tests/cowork_connect_test.rs` |
| Tests — Cron Dedup Repair | 3 | `src/tests/cron_dedup_repair_test.rs` |
| Tests — Cron Dedup Scan Schedule | 3 | `src/tests/cron_dedup_scan_schedule_test.rs` |
| Tests — Cron Deliver To Thread | 7 | `src/tests/cron_deliver_to_thread_test.rs` |
| Tests — Cron Profile Isolation | 6 | `src/tests/cron_profile_isolation_test.rs` |
| Tests — Cron Schedule Util | 12 | `src/tests/cron_schedule_util_test.rs` |
| Tests — Cron Scheduler Dedup Job | 1 | `src/tests/cron_scheduler_dedup_job_test.rs` |
| Tests — Cron Scheduler Lock | 4 | `src/tests/cron_scheduler_lock_test.rs` |
| Tests — Cron Send Scope | 6 | `src/tests/cron_send_scope_test.rs` |
| Tests — Cron Session Target | 7 | `src/tests/cron_session_target_test.rs` |
| Tests — Cron | 74 | `src/tests/cron_test.rs` |
| Tests — Cron Tool Registry | 2 | `src/tests/cron_tool_registry_test.rs` |
| Tests — Cross Provider Model Leak Guard | 6 | `src/tests/cross_provider_model_leak_guard_test.rs` |
| Tests — Custom Model Paste | 5 | `src/tests/custom_model_paste_test.rs` |
| Tests — Custom Provider Cache Autoenable | 10 | `src/tests/custom_provider_cache_autoenable_test.rs` |
| Tests — Custom Provider Key Fetch | 3 | `src/tests/custom_provider_key_fetch_test.rs` |
| Tests — Custom Provider Live Fetch Regression | 5 | `src/tests/custom_provider_live_fetch_regression_test.rs` |
| Tests — Custom Provider No Models | 3 | `src/tests/custom_provider_no_models_test.rs` |
| Tests — Custom Provider Rename Keys Toml | 11 | `src/tests/custom_provider_rename_keys_toml_test.rs` |
| Tests — Custom Provider Section Resolver | 2 | `src/tests/custom_provider_section_resolver_test.rs` |
| Tests — Custom Provider Stream Empty Finish | 1 | `src/tests/custom_provider_stream_empty_finish_test.rs` |
| Tests — Custom Provider | 31 | `src/tests/custom_provider_test.rs` |
| Tests — Daemon Health | 10 | `src/tests/daemon_health_test.rs` |
| Tests — Daily Backup Rotation | 3 | `src/tests/daily_backup_rotation_test.rs` |
| Tests — DB Database | 7 | `src/tests/db_database_test.rs` |
| Tests — DB Migration 33 Heal | 4 | `src/tests/db_migration_33_heal_test.rs` |
| Tests — DB Models | 5 | `src/tests/db_models_test.rs` |
| Tests — DB Pending Origin Heal | 5 | `src/tests/db_pending_origin_heal_test.rs` |
| Tests — DB Repository Channel Message | 1 | `src/tests/db_repository_channel_message_test.rs` |
| Tests — DB Repository File | 2 | `src/tests/db_repository_file_test.rs` |
| Tests — DB Repository Message | 5 | `src/tests/db_repository_message_test.rs` |
| Tests — DB Repository Project | 5 | `src/tests/db_repository_project_test.rs` |
| Tests — DB Repository Session | 2 | `src/tests/db_repository_session_test.rs` |
| Tests — DB Retry | 8 | `src/tests/db_retry_test.rs` |
| Tests — DB Thread ID Stamp Heal | 3 | `src/tests/db_thread_id_stamp_heal_test.rs` |
| Tests — Deepseek Reasoning | 13 | `src/tests/deepseek_reasoning_test.rs` |
| Tests — Directive Discovery | 13 | `src/tests/directive_discovery_test.rs` |
| Tests — Discord Handler | 2 | `src/tests/discord_handler_test.rs` |
| Tests — Discord Tool Group | 4 | `src/tests/discord_tool_group_test.rs` |
| Tests — Doc Gen Docx | 7 | `src/tests/doc_gen_docx_test.rs` |
| Tests — Doc Gen Pdf | 14 | `src/tests/doc_gen_pdf_test.rs` |
| Tests — Doc Gen Pptx | 5 | `src/tests/doc_gen_pptx_test.rs` |
| Tests — Doc Gen Xlsx | 6 | `src/tests/doc_gen_xlsx_test.rs` |
| Tests — Doc Parser Page Range | 5 | `src/tests/doc_parser_page_range_test.rs` |
| Tests — Doctor Fix | 4 | `src/tests/doctor_fix_test.rs` |
| Tests — Drop Landing | 6 | `src/tests/drop_landing_test.rs` |
| Tests — Drop Transfer | 15 | `src/tests/drop_transfer_test.rs` |
| Tests — Duplicate Submit | 8 | `src/tests/duplicate_submit_test.rs` |
| Tests — Dynamic Tool Coerce | 13 | `src/tests/dynamic_tool_coerce_test.rs` |
| Tests — Dynamic Tool Parse Error | 12 | `src/tests/dynamic_tool_parse_error_test.rs` |
| Tests — Edit Retry | 5 | `src/tests/edit_retry_test.rs` |
| Tests — Empty Answer Nudge | 8 | `src/tests/empty_answer_nudge_test.rs` |
| Tests — Empty Reasoning Stub | 9 | `src/tests/empty_reasoning_stub_test.rs` |
| Tests — Epistemic Inline | 9 | `src/tests/epistemic_inline_test.rs` |
| Tests — Epistemic Plan Start | 6 | `src/tests/epistemic_plan_start_test.rs` |
| Tests — Eval Baseline | 6 | `src/tests/eval_baseline_test.rs` |
| Tests — Eval Before After | 5 | `src/tests/eval_before_after_test.rs` |
| Tests — Eval Compaction | 5 | `src/tests/eval_compaction_test.rs` |
| Tests — Eval Live Resolver | 4 | `src/tests/eval_live_resolver_test.rs` |
| Tests — Eval Manifest | 3 | `src/tests/eval_manifest_test.rs` |
| Tests — Eval Panel | 8 | `src/tests/eval_panel_test.rs` |
| Tests — Eval Produce | 4 | `src/tests/eval_produce_test.rs` |
| Tests — Eval Recall | 7 | `src/tests/eval_recall_test.rs` |
| Tests — Eval Replay Provider | 5 | `src/tests/eval_replay_provider_test.rs` |
| Tests — Eval Runner | 9 | `src/tests/eval_runner_test.rs` |
| Tests — Eval Scorer | 4 | `src/tests/eval_scorer_test.rs` |
| Tests — Eval Self Awareness | 12 | `src/tests/eval_self_awareness_test.rs` |
| Tests — Evolve Diagnose | 7 | `src/tests/evolve_diagnose_test.rs` |
| Tests — Evolve Systemd Restart | 14 | `src/tests/evolve_systemd_restart_test.rs` |
| Tests — Evolve | 23 | `src/tests/evolve_test.rs` |
| Tests — Evolve Verify | 8 | `src/tests/evolve_verify_test.rs` |
| Tests — Exa Search | 4 | `src/tests/exa_search_test.rs` |
| Tests — Execution Blocklist Floor | 4 | `src/tests/execution_blocklist_floor_test.rs` |
| Tests — External Scope | 3 | `src/tests/external_scope_test.rs` |
| Tests — Fallback Chain Hot Reload | 3 | `src/tests/fallback_chain_hot_reload_test.rs` |
| Tests — Fallback Cli Tool Ownership | 6 | `src/tests/fallback_cli_tool_ownership_test.rs` |
| Tests — Fallback Provenance | 4 | `src/tests/fallback_provenance_test.rs` |
| Tests — Fallback Streak | 7 | `src/tests/fallback_streak_test.rs` |
| Tests — Fallback Suggestion | 5 | `src/tests/fallback_suggestion_test.rs` |
| Tests — Fallback Swap Model Report | 4 | `src/tests/fallback_swap_model_report_test.rs` |
| Tests — Fallback Vision | 64 | `src/tests/fallback_vision_test.rs` |
| Tests — Feedback Policy | 7 | `src/tests/feedback_policy_test.rs` |
| Tests — File Extract | 38 | `src/tests/file_extract_test.rs` |
| Tests — File Versions | 8 | `src/tests/file_versions_test.rs` |
| Tests — Flock Retry | 5 | `src/tests/flock_retry_test.rs` |
| Tests — Flow Progress Key | 7 | `src/tests/flow_progress_key_test.rs` |
| Tests — Force Default | 4 | `src/tests/force_default_test.rs` |
| Tests — Format User Error | 12 | `src/tests/format_user_error_test.rs` |
| Tests — Gemini Fetch | 3 | `src/tests/gemini_fetch_test.rs` |
| Tests — Gemini Schema Sanitize | 10 | `src/tests/gemini_schema_sanitize_test.rs` |
| Tests — Generate Image Backend | 5 | `src/tests/generate_image_backend_test.rs` |
| Tests — Generate Image Filename | 5 | `src/tests/generate_image_filename_test.rs` |
| Tests — Git Branch | 8 | `src/tests/git_branch_test.rs` |
| Tests — Github Provider | 38 | `src/tests/github_provider_test.rs` |
| Tests — GLM Reasoning | 13 | `src/tests/glm_reasoning_test.rs` |
| Tests — Glob Tool | 3 | `src/tests/glob_tool_test.rs` |
| Tests — Goal Command | 14 | `src/tests/goal_command_test.rs` |
| Tests — Goal Judge | 12 | `src/tests/goal_judge_test.rs` |
| Tests — Goal Manage | 6 | `src/tests/goal_manage_test.rs` |
| Tests — Governor Gates | 10 | `src/tests/governor_gates_test.rs` |
| Tests — Governor Internals | 5 | `src/tests/governor_internals_test.rs` |
| Tests — Handshake Timeout | 4 | `src/tests/handshake_timeout_test.rs` |
| Tests — Hashline | 32 | `src/tests/hashline_test.rs` |
| Tests — Headless Tool Surface | 7 | `src/tests/headless_tool_surface_test.rs` |
| Tests — Html Comment Strip | 14 | `src/tests/html_comment_strip_test.rs` |
| Tests — HTTP Request | 5 | `src/tests/http_request_test.rs` |
| Tests — Image Util | 9 | `src/tests/image_util_test.rs` |
| Tests — Incident Log Dedup | 10 | `src/tests/incident_log_dedup_test.rs` |
| Tests — Install Homebrew | 10 | `src/tests/install_homebrew_test.rs` |
| Tests — Instance Lock | 6 | `src/tests/instance_lock_test.rs` |
| Tests — Kimi Plan | 6 | `src/tests/kimi_plan_test.rs` |
| Tests — Kimi Reasoning Map | 7 | `src/tests/kimi_reasoning_map_test.rs` |
| Tests — Kimi Reasoning | 14 | `src/tests/kimi_reasoning_test.rs` |
| Tests — Lazy Tools | 8 | `src/tests/lazy_tools_test.rs` |
| Tests — Legacy Doc Support | 6 | `src/tests/legacy_doc_support_test.rs` |
| Tests — Local Provider Gate | 6 | `src/tests/local_provider_gate_test.rs` |
| Tests — Logger Lock Recovery | 3 | `src/tests/logger_lock_recovery_test.rs` |
| Tests — Logger Mutex Contention | 3 | `src/tests/logger_mutex_contention_test.rs` |
| Tests — Logging Log Files | 5 | `src/tests/logging_log_files_test.rs` |
| Tests — Logging Logger | 5 | `src/tests/logging_logger_test.rs` |
| Tests — Long Command | 5 | `src/tests/long_command_test.rs` |
| Tests — Loop Break | 10 | `src/tests/loop_break_test.rs` |
| Tests — Loop Guard | 32 | `src/tests/loop_guard_test.rs` |
| Tests — Markdown Render | 10 | `src/tests/markdown_render_test.rs` |
| Tests — Memory Backfill Sweep | 6 | `src/tests/memory_backfill_sweep_test.rs` |
| Tests — Memory Chunk Vector | 13 | `src/tests/memory_chunk_vector_test.rs` |
| Tests — Memory Chunker | 8 | `src/tests/memory_chunker_test.rs` |
| Tests — Memory Collection Routing | 4 | `src/tests/memory_collection_routing_test.rs` |
| Tests — Memory Db | 5 | `src/tests/memory_db_test.rs` |
| Tests — Memory Embedding Gate | 5 | `src/tests/memory_embedding_gate_test.rs` |
| Tests — Memory Embedding Key | 7 | `src/tests/memory_embedding_key_test.rs` |
| Tests — Memory External Sweep | 4 | `src/tests/memory_external_sweep_test.rs` |
| Tests — Memory External | 4 | `src/tests/memory_external_test.rs` |
| Tests — Memory Health Report | 9 | `src/tests/memory_health_report_test.rs` |
| Tests — Memory Local Engine | 5 | `src/tests/memory_local_engine_test.rs` |
| Tests — Memory Recall Eval | 5 | `src/tests/memory_recall_eval_test.rs` |
| Tests — Memory Recall Multilingual | 6 | `src/tests/memory_recall_multilingual_test.rs` |
| Tests — Memory Recall | 11 | `src/tests/memory_recall_test.rs` |
| Tests — Memory Search Code Graph | 5 | `src/tests/memory_search_code_graph_test.rs` |
| Tests — Memory Search Parity | 15 | `src/tests/memory_search_parity_test.rs` |
| Tests — Memory Search Resolve Path | 5 | `src/tests/memory_search_resolve_path_test.rs` |
| Tests — Memory Search Rrf | 2 | `src/tests/memory_search_rrf_test.rs` |
| Tests — Memory Search Scope | 3 | `src/tests/memory_search_scope_test.rs` |
| Tests — Memory Search | 3 | `src/tests/memory_search_test.rs` |
| Tests — Memory Store Profile | 3 | `src/tests/memory_store_profile_test.rs` |
| Tests — Memory Store | 6 | `src/tests/memory_store_test.rs` |
| Tests — Memory Symbol Extractor | 5 | `src/tests/memory_symbol_extractor_test.rs` |
| Tests — Memory Symbol Extractor Tree | 11 | `src/tests/memory_symbol_extractor_tree_test.rs` |
| Tests — Menu Auto Solo | 6 | `src/tests/menu_auto_solo_test.rs` |
| Tests — Menu Refresh | 5 | `src/tests/menu_refresh_test.rs` |
| Tests — Merge Provider Keys | 12 | `src/tests/merge_provider_keys_test.rs` |
| Tests — Message Split Markup | 7 | `src/tests/message_split_markup_test.rs` |
| Tests — Mimo Tool Call Hint | 3 | `src/tests/mimo_tool_call_hint_test.rs` |
| Tests — Mission Control Activity Malformed | 9 | `src/tests/mission_control_activity_malformed_test.rs` |
| Tests — Mission Control Activity Service | 8 | `src/tests/mission_control_activity_service_test.rs` |
| Tests — Mission Control Command | 2 | `src/tests/mission_control_command_test.rs` |
| Tests — Mission Control Dedup Detail | 5 | `src/tests/mission_control_dedup_detail_test.rs` |
| Tests — Mission Control Inbox Service | 6 | `src/tests/mission_control_inbox_service_test.rs` |
| Tests — Mission Control Input | 23 | `src/tests/mission_control_input_test.rs` |
| Tests — Mission Control Layout | 7 | `src/tests/mission_control_layout_test.rs` |
| Tests — Mission Control Report | 2 | `src/tests/mission_control_report_test.rs` |
| Tests — Mission Control Schedule Service | 5 | `src/tests/mission_control_schedule_service_test.rs` |
| Tests — Mission Control Skill Inbox | 8 | `src/tests/mission_control_skill_inbox_test.rs` |
| Tests — Model Display Label | 7 | `src/tests/model_display_label_test.rs` |
| Tests — Model Fetch | 11 | `src/tests/model_fetch_test.rs` |
| Tests — Model Match | 9 | `src/tests/model_match_test.rs` |
| Tests — Model Menu | 5 | `src/tests/model_menu_test.rs` |
| Tests — Model Order | 9 | `src/tests/model_order_test.rs` |
| Tests — Model Refresh | 4 | `src/tests/model_refresh_test.rs` |
| Tests — Models Picker Dedup | 7 | `src/tests/models_picker_dedup_test.rs` |
| Tests — Mouse Frag Gate | 18 | `src/tests/mouse_frag_gate_test.rs` |
| Tests — Nesting Gate | 4 | `src/tests/nesting_gate_test.rs` |
| Tests — New Session Pane Binding | 3 | `src/tests/new_session_pane_binding_test.rs` |
| Tests — No Direct Terminal Writes | 2 | `src/tests/no_direct_terminal_writes_test.rs` |
| Tests — Nonstream Compat | 5 | `src/tests/nonstream_compat_test.rs` |
| Tests — Notify Receipts | 4 | `src/tests/notify_receipts_test.rs` |
| Tests — Nudge Text | 9 | `src/tests/nudge_text_test.rs` |
| Tests — OC Consumer Migration | 7 | `src/tests/oc_consumer_migration_test.rs` |
| Tests — OC Opaqueness Wall | 2 | `src/tests/oc_opaqueness_wall_test.rs` |
| Tests — OC Reverse Maps | 5 | `src/tests/oc_reverse_maps_test.rs` |
| Tests — OC Send Scope | 8 | `src/tests/oc_send_scope_test.rs` |
| Tests — Onboard Channel | 13 | `src/tests/onboard_channel_test.rs` |
| Tests — Onboarding Brain | 23 | `src/tests/onboarding_brain_test.rs` |
| Tests — Onboarding Channel Deep Link | 7 | `src/tests/onboarding_channel_deep_link_test.rs` |
| Tests — Onboarding Completion State | 8 | `src/tests/onboarding_completion_state_test.rs` |
| Tests — Onboarding Custom Model Input | 9 | `src/tests/onboarding_custom_model_input_test.rs` |
| Tests — Onboarding Custom Model Pick | 6 | `src/tests/onboarding_custom_model_pick_test.rs` |
| Tests — Onboarding Field Nav | 53 | `src/tests/onboarding_field_nav_test.rs` |
| Tests — Onboarding Key Field | 15 | `src/tests/onboarding_key_field_test.rs` |
| Tests — Onboarding Keys | 4 | `src/tests/onboarding_keys_test.rs` |
| Tests — Onboarding Navigation | 26 | `src/tests/onboarding_navigation_test.rs` |
| Tests — Onboarding No Silent Commit | 8 | `src/tests/onboarding_no_silent_commit_test.rs` |
| Tests — Onboarding Step Save | 8 | `src/tests/onboarding_step_save_test.rs` |
| Tests — Onboarding Tts API | 16 | `src/tests/onboarding_tts_api_test.rs` |
| Tests — Onboarding Types | 17 | `src/tests/onboarding_types_test.rs` |
| Tests — Onboarding User Scroll | 8 | `src/tests/onboarding_user_scroll_test.rs` |
| Tests — Onboarding Visible Window | 9 | `src/tests/onboarding_visible_window_test.rs` |
| Tests — Onboarding Voice Key Edit | 6 | `src/tests/onboarding_voice_key_edit_test.rs` |
| Tests — Onboarding Voice Preserve | 4 | `src/tests/onboarding_voice_preserve_test.rs` |
| Tests — Onboarding Voice Seed | 3 | `src/tests/onboarding_voice_seed_test.rs` |
| Tests — Onboarding Welcome | 9 | `src/tests/onboarding_welcome_test.rs` |
| Tests — Onboarding Wizard | 68 | `src/tests/onboarding_wizard_test.rs` |
| Tests — Openai Provider | 20 | `src/tests/openai_provider_test.rs` |
| Tests — Opencode Provider | 21 | `src/tests/opencode_provider_test.rs` |
| Tests — Orphan Close Tag Strip | 9 | `src/tests/orphan_close_tag_strip_test.rs` |
| Tests — Orphan Think Close Tag | 13 | `src/tests/orphan_think_close_tag_test.rs` |
| Tests — Owner Plus Normalization | 5 | `src/tests/owner_plus_normalization_test.rs` |
| Tests — Owner Resolve | 6 | `src/tests/owner_resolve_test.rs` |
| Tests — Parallel Tools | 3 | `src/tests/parallel_tools_test.rs` |
| Tests — Path Lock | 7 | `src/tests/path_lock_test.rs` |
| Tests — Pdf Page Range Parser | 25 | `src/tests/pdf_page_range_parser_test.rs` |
| Tests — Pdf Smart Routing | 5 | `src/tests/pdf_smart_routing_test.rs` |
| Tests — Pdf To Images | 4 | `src/tests/pdf_to_images_test.rs` |
| Tests — Pdf Vision | 3 | `src/tests/pdf_vision_test.rs` |
| Tests — Pending Request Age | 4 | `src/tests/pending_request_age_test.rs` |
| Tests — Pending Request Thread | 3 | `src/tests/pending_request_thread_test.rs` |
| Tests — Pending Resume No Reinsert | 3 | `src/tests/pending_resume_no_reinsert_test.rs` |
| Tests — Phantom Allowlist | 7 | `src/tests/phantom_allowlist_test.rs` |
| Tests — Phantom Bare Completion | 5 | `src/tests/phantom_bare_completion_test.rs` |
| Tests — Phantom Bare Tool Name | 6 | `src/tests/phantom_bare_tool_name_test.rs` |
| Tests — Phantom Clause Boundary | 5 | `src/tests/phantom_clause_boundary_test.rs` |
| Tests — Phantom Cleanup Intent | 7 | `src/tests/phantom_cleanup_intent_test.rs` |
| Tests — Phantom Compound Gerund | 5 | `src/tests/phantom_compound_gerund_test.rs` |
| Tests — Phantom DB Persistence | 2 | `src/tests/phantom_db_persistence_test.rs` |
| Tests — Phantom Deferment | 11 | `src/tests/phantom_deferment_test.rs` |
| Tests — Phantom Dotted Command | 6 | `src/tests/phantom_dotted_command_test.rs` |
| Tests — Phantom Fenced Command | 9 | `src/tests/phantom_fenced_command_test.rs` |
| Tests — Phantom Generic Intent | 5 | `src/tests/phantom_generic_intent_test.rs` |
| Tests — Phantom Going To | 3 | `src/tests/phantom_going_to_test.rs` |
| Tests — Phantom Issue Action | 16 | `src/tests/phantom_issue_action_test.rs` |
| Tests — Phantom Null Effect | 10 | `src/tests/phantom_null_effect_test.rs` |
| Tests — Phantom Oven Claim | 9 | `src/tests/phantom_oven_claim_test.rs` |
| Tests — Phantom Persist Budget | 8 | `src/tests/phantom_persist_budget_test.rs` |
| Tests — Phantom Plan Announcement | 6 | `src/tests/phantom_plan_announcement_test.rs` |
| Tests — Phantom Playback Claim | 7 | `src/tests/phantom_playback_claim_test.rs` |
| Tests — Phantom Post Success Exemption | 11 | `src/tests/phantom_post_success_exemption_test.rs` |
| Tests — Phantom Pronoun Drop | 8 | `src/tests/phantom_pronoun_drop_test.rs` |
| Tests — Phantom Side Effect | 12 | `src/tests/phantom_side_effect_test.rs` |
| Tests — Phantom Structured Report | 14 | `src/tests/phantom_structured_report_test.rs` |
| Tests — Phantom Trigger Gap | 3 | `src/tests/phantom_trigger_gap_test.rs` |
| Tests — Phantom Unbacked Evidence | 11 | `src/tests/phantom_unbacked_evidence_test.rs` |
| Tests — Phantom Unbacked Facts | 16 | `src/tests/phantom_unbacked_facts_test.rs` |
| Tests — Phantom Uncalled Command | 12 | `src/tests/phantom_uncalled_command_test.rs` |
| Tests — Phantom Unsent File | 12 | `src/tests/phantom_unsent_file_test.rs` |
| Tests — Phantom Work Announcement | 14 | `src/tests/phantom_work_announcement_test.rs` |
| Tests — Picker Limits | 10 | `src/tests/picker_limits_test.rs` |
| Tests — Plan Approval Gate | 10 | `src/tests/plan_approval_gate_test.rs` |
| Tests — Plan Card Line Breaks | 12 | `src/tests/plan_card_line_breaks_test.rs` |
| Tests — Plan Card Lock | 3 | `src/tests/plan_card_lock_test.rs` |
| Tests — Plan Card Persist | 7 | `src/tests/plan_card_persist_test.rs` |
| Tests — Plan Card Rate Limit | 5 | `src/tests/plan_card_rate_limit_test.rs` |
| Tests — Plan Completed Persist | 7 | `src/tests/plan_completed_persist_test.rs` |
| Tests — Plan Document | 19 | `src/tests/plan_document_test.rs` |
| Tests — Plan Files | 16 | `src/tests/plan_files_test.rs` |
| Tests — Plan Flow Keyboard Gate | 6 | `src/tests/plan_flow_keyboard_gate_test.rs` |
| Tests — Plan Gate | 7 | `src/tests/plan_gate_test.rs` |
| Tests — Plan Mode Command | 14 | `src/tests/plan_mode_command_test.rs` |
| Tests — Plan Mode Provider | 15 | `src/tests/plan_mode_provider_test.rs` |
| Tests — Plan Reminder | 6 | `src/tests/plan_reminder_test.rs` |
| Tests — Plan Stale Marker | 3 | `src/tests/plan_stale_marker_test.rs` |
| Tests — Plan Status Glyph | 3 | `src/tests/plan_status_glyph_test.rs` |
| Tests — Plan Template Nudge | 5 | `src/tests/plan_template_nudge_test.rs` |
| Tests — Plan Title Echo | 9 | `src/tests/plan_title_echo_test.rs` |
| Tests — Plan Tool Contract | 18 | `src/tests/plan_tool_contract_test.rs` |
| Tests — Plan Tool Description | 8 | `src/tests/plan_tool_description_test.rs` |
| Tests — Plan Tool Inline | 9 | `src/tests/plan_tool_inline_test.rs` |
| Tests — Plan Tool | 54 | `src/tests/plan_tool_test.rs` |
| Tests — Plan Vacuous Pass | 7 | `src/tests/plan_vacuous_pass_test.rs` |
| Tests — Plan Window | 21 | `src/tests/plan_window_test.rs` |
| Tests — Post Evolve | 5 | `src/tests/post_evolve_test.rs` |
| Tests — Preamble Large Files | 4 | `src/tests/preamble_large_files_test.rs` |
| Tests — Pressure Warning | 11 | `src/tests/pressure_warning_test.rs` |
| Tests — Pricing Fallback | 9 | `src/tests/pricing_fallback_test.rs` |
| Tests — Profile Addressing | 9 | `src/tests/profile_addressing_test.rs` |
| Tests — Profile Pid Lock | 3 | `src/tests/profile_pid_lock_test.rs` |
| Tests — Profile Preempt | 4 | `src/tests/profile_preempt_test.rs` |
| Tests — Profile | 64 | `src/tests/profile_test.rs` |
| Tests — Profiles Dialog | 56 | `src/tests/profiles_dialog_test.rs` |
| Tests — Progress Callback Fanout | 4 | `src/tests/progress_callback_fanout_test.rs` |
| Tests — Project File Archive | 3 | `src/tests/project_file_archive_test.rs` |
| Tests — Project File Slug | 4 | `src/tests/project_file_slug_test.rs` |
| Tests — Project Runner | 8 | `src/tests/project_runner_test.rs` |
| Tests — Project Skills | 5 | `src/tests/project_skills_test.rs` |
| Tests — Project | 25 | `src/tests/project_test.rs` |
| Tests — Prompt Analyzer | 29 | `src/tests/prompt_analyzer_test.rs` |
| Tests — Prompt Cache Split | 7 | `src/tests/prompt_cache_split_test.rs` |
| Tests — Prompt Cache Stability | 3 | `src/tests/prompt_cache_stability_test.rs` |
| Tests — Prompt Compiled Features | 16 | `src/tests/prompt_compiled_features_test.rs` |
| Tests — Prompt Inline Edit Directive | 5 | `src/tests/prompt_inline_edit_directive_test.rs` |
| Tests — Prompt Known Paths | 9 | `src/tests/prompt_known_paths_test.rs` |
| Tests — Provider By Name Restore | 5 | `src/tests/provider_by_name_restore_test.rs` |
| Tests — Provider Config Regression | 32 | `src/tests/provider_config_regression_test.rs` |
| Tests — Provider Context Window Override | 2 | `src/tests/provider_context_window_override_test.rs` |
| Tests — Provider Error Proxy | 27 | `src/tests/provider_error_proxy_test.rs` |
| Tests — Provider Factory Regression | 31 | `src/tests/provider_factory_regression_test.rs` |
| Tests — Provider Matches Session | 3 | `src/tests/provider_matches_session_test.rs` |
| Tests — Provider Models Isolation | 5 | `src/tests/provider_models_isolation_test.rs` |
| Tests — Provider Name For Session | 3 | `src/tests/provider_name_for_session_test.rs` |
| Tests — Provider Never Ignored | 5 | `src/tests/provider_never_ignored_test.rs` |
| Tests — Provider Picker Setup Hint | 4 | `src/tests/provider_picker_setup_hint_test.rs` |
| Tests — Provider Registry | 8 | `src/tests/provider_registry_test.rs` |
| Tests — Provider Retry Consolidation | 9 | `src/tests/provider_retry_consolidation_test.rs` |
| Tests — Provider Spec | 8 | `src/tests/provider_spec_test.rs` |
| Tests — Provider Sync | 8 | `src/tests/provider_sync_test.rs` |
| Tests — Qr Render | 10 | `src/tests/qr_render_test.rs` |
| Tests — Question Common | 10 | `src/tests/question_common_test.rs` |
| Tests — Queued Message Join | 5 | `src/tests/queued_message_join_test.rs` |
| Tests — Queued Message | 15 | `src/tests/queued_message_test.rs` |
| Tests — Quiet Delivery | 6 | `src/tests/quiet_delivery_test.rs` |
| Tests — Quota Classification | 13 | `src/tests/quota_classification_test.rs` |
| Tests — Qwen Detect | 18 | `src/tests/qwen_detect_test.rs` |
| Tests — Qwen Preserve Thinking | 7 | `src/tests/qwen_preserve_thinking_test.rs` |
| Tests — Qwen Reasoning | 19 | `src/tests/qwen_reasoning_test.rs` |
| Tests — Qwen Tool Extractor | 72 | `src/tests/qwen_tool_extractor_test.rs` |
| Tests — Qwen Tool Marker Strip | 7 | `src/tests/qwen_tool_marker_strip_test.rs` |
| Tests — Ralph Loop Config | 5 | `src/tests/ralph_loop_config_test.rs` |
| Tests — Ralph Receipt Binding | 25 | `src/tests/ralph_receipt_binding_test.rs` |
| Tests — Ralph Verification Gate | 28 | `src/tests/ralph_verification_gate_test.rs` |
| Tests — Rate Limit Reporting | 7 | `src/tests/rate_limit_reporting_test.rs` |
| Tests — Rate Limiter | 8 | `src/tests/rate_limiter_test.rs` |
| Tests — React Marker | 43 | `src/tests/react_marker_test.rs` |
| Tests — Read Empty File | 2 | `src/tests/read_empty_file_test.rs` |
| Tests — Read Media Redirect | 3 | `src/tests/read_media_redirect_test.rs` |
| Tests — Read Output Budget | 4 | `src/tests/read_output_budget_test.rs` |
| Tests — Read Resume Offset | 1 | `src/tests/read_resume_offset_test.rs` |
| Tests — Read State | 1 | `src/tests/read_state_test.rs` |
| Tests — Reasoning Lines | 7 | `src/tests/reasoning_lines_test.rs` |
| Tests — Reasoning Run | 6 | `src/tests/reasoning_run_test.rs` |
| Tests — Reasoning Split | 13 | `src/tests/reasoning_split_test.rs` |
| Tests — Recent Paths | 17 | `src/tests/recent_paths_test.rs` |
| Tests — Redact Scope | 3 | `src/tests/redact_scope_test.rs` |
| Tests — Rename Session | 7 | `src/tests/rename_session_test.rs` |
| Tests — Repetition Error Message | 5 | `src/tests/repetition_error_message_test.rs` |
| Tests — Repetition Fenced Code | 5 | `src/tests/repetition_fenced_code_test.rs` |
| Tests — Repetition | 5 | `src/tests/repetition_test.rs` |
| Tests — Respond To Group Persist | 3 | `src/tests/respond_to_group_persist_test.rs` |
| Tests — Restart Recovery | 6 | `src/tests/restart_recovery_test.rs` |
| Tests — Restart Replay Context | 1 | `src/tests/restart_replay_context_test.rs` |
| Tests — Retry Notice Drain | 4 | `src/tests/retry_notice_drain_test.rs` |
| Tests — RSI Brain Dedup | 31 | `src/tests/rsi_brain_dedup_test.rs` |
| Tests — RSI Command Patterns | 5 | `src/tests/rsi_command_patterns_test.rs` |
| Tests — RSI Disposition | 12 | `src/tests/rsi_disposition_test.rs` |
| Tests — Rsi Enabled Gate | 5 | `src/tests/rsi_enabled_gate_test.rs` |
| Tests — RSI Fallback Wrap | 4 | `src/tests/rsi_fallback_wrap_test.rs` |
| Tests — RSI Git History | 12 | `src/tests/rsi_git_history_test.rs` |
| Tests — RSI Notification Redaction | 5 | `src/tests/rsi_notification_redaction_test.rs` |
| Tests — RSI Opportunity Hash | 7 | `src/tests/rsi_opportunity_hash_test.rs` |
| Tests — RSI Prompt Propose | 5 | `src/tests/rsi_prompt_propose_test.rs` |
| Tests — RSI Prompt Triage | 3 | `src/tests/rsi_prompt_triage_test.rs` |
| Tests — RSI Proposals | 19 | `src/tests/rsi_proposals_test.rs` |
| Tests — RSI Provider Resolution | 11 | `src/tests/rsi_provider_resolution_test.rs` |
| Tests — RSI Pruned | 23 | `src/tests/rsi_pruned_test.rs` |
| Tests — RSI Rule Budget | 13 | `src/tests/rsi_rule_budget_test.rs` |
| Tests — RSI Self Improve Dedup | 2 | `src/tests/rsi_self_improve_dedup_test.rs` |
| Tests — RSI Session Pin | 5 | `src/tests/rsi_session_pin_test.rs` |
| Tests — RSI Skill Proposals | 9 | `src/tests/rsi_skill_proposals_test.rs` |
| Tests — RSI Skill Sequences | 6 | `src/tests/rsi_skill_sequences_test.rs` |
| Tests — RSI Stale Cycle Wire | 8 | `src/tests/rsi_stale_cycle_wire_test.rs` |
| Tests — RSI Stale Ledger | 16 | `src/tests/rsi_stale_ledger_test.rs` |
| Tests — RSI Stale Regression | 12 | `src/tests/rsi_stale_regression_test.rs` |
| Tests — RSI Stale Scan | 27 | `src/tests/rsi_stale_scan_test.rs` |
| Tests — RSI Staleness | 6 | `src/tests/rsi_staleness_test.rs` |
| Tests — RSI Subsystem | 23 | `src/tests/rsi_subsystem_test.rs` |
| Tests — RSI Sync Cap Bail | 9 | `src/tests/rsi_sync_cap_bail_test.rs` |
| Tests — RSI Sync | 20 | `src/tests/rsi_sync_test.rs` |
| Tests — RSI Sync Tracked | 9 | `src/tests/rsi_sync_tracked_test.rs` |
| Tests — RSI | 91 | `src/tests/rsi_test.rs` |
| Tests — Rtk Autodownload | 4 | `src/tests/rtk_autodownload_test.rs` |
| Tests — Rtk Rewrite | 15 | `src/tests/rtk_rewrite_test.rs` |
| Tests — Rtk Sysadmin Supported | 6 | `src/tests/rtk_sysadmin_supported_test.rs` |
| Tests — Rtk Tracker | 5 | `src/tests/rtk_tracker_test.rs` |
| Tests — Runtime Info Home Anchor | 7 | `src/tests/runtime_info_home_anchor_test.rs` |
| Tests — Sanitize Code Edit Block | 10 | `src/tests/sanitize_code_edit_block_test.rs` |
| Tests — Sanitize Quoted Secret | 9 | `src/tests/sanitize_quoted_secret_test.rs` |
| Tests — Sanitize Reasoning Leak | 7 | `src/tests/sanitize_reasoning_leak_test.rs` |
| Tests — Sanitize Redaction | 31 | `src/tests/sanitize_redaction_test.rs` |
| Tests — Scope All Recency | 5 | `src/tests/scope_all_recency_test.rs` |
| Tests — Secret Hygiene Oc05 | 4 | `src/tests/secret_hygiene_oc05_test.rs` |
| Tests — Seen Skills | 11 | `src/tests/seen_skills_test.rs` |
| Tests — Self Healing | 88 | `src/tests/self_healing_test.rs` |
| Tests — Self Improve Failure Log Guard | 3 | `src/tests/self_improve_failure_log_guard_test.rs` |
| Tests — Self Improve Guard | 6 | `src/tests/self_improve_guard_test.rs` |
| Tests — Self Update Path | 6 | `src/tests/self_update_path_test.rs` |
| Tests — Services Context | 2 | `src/tests/services_context_test.rs` |
| Tests — Services File | 11 | `src/tests/services_file_test.rs` |
| Tests — Services Message | 10 | `src/tests/services_message_test.rs` |
| Tests — Services Project | 7 | `src/tests/services_project_test.rs` |
| Tests — Services Session | 10 | `src/tests/services_session_test.rs` |
| Tests — Session Chat Id Lookup | 8 | `src/tests/session_chat_id_lookup_test.rs` |
| Tests — Session Cwd Restore | 4 | `src/tests/session_cwd_restore_test.rs` |
| Tests — Session Enqueue Callback | 2 | `src/tests/session_enqueue_callback_test.rs` |
| Tests — Session List Shows Last Activity | 4 | `src/tests/session_list_shows_last_activity_test.rs` |
| Tests — Session Notify | 20 | `src/tests/session_notify_test.rs` |
| Tests — Session Project Autolink | 11 | `src/tests/session_project_autolink_test.rs` |
| Tests — Session Provider Restore | 3 | `src/tests/session_provider_restore_test.rs` |
| Tests — Session Provider Wrap | 9 | `src/tests/session_provider_wrap_test.rs` |
| Tests — Session Search Query | 5 | `src/tests/session_search_query_test.rs` |
| Tests — Session Search Tail | 5 | `src/tests/session_search_tail_test.rs` |
| Tests — Session Working Dir Isolation | 3 | `src/tests/session_working_dir_isolation_test.rs` |
| Tests — Session Working Dir | 19 | `src/tests/session_working_dir_test.rs` |
| Tests — Shell Scan | 7 | `src/tests/shell_scan_test.rs` |
| Tests — Shutdown Recovery Row | 7 | `src/tests/shutdown_recovery_row_test.rs` |
| Tests — Skill Slash Dispatch | 8 | `src/tests/skill_slash_dispatch_test.rs` |
| Tests — Skills Dialog | 18 | `src/tests/skills_dialog_test.rs` |
| Tests — Skills | 18 | `src/tests/skills_test.rs` |
| Tests — Slack Blocks | 7 | `src/tests/slack_blocks_test.rs` |
| Tests — Slack Final Body | 8 | `src/tests/slack_final_body_test.rs` |
| Tests — Slack Fmt | 21 | `src/tests/slack_fmt_test.rs` |
| Tests — Slack Handler | 2 | `src/tests/slack_handler_test.rs` |
| Tests — Slack Narration Fold | 12 | `src/tests/slack_narration_fold_test.rs` |
| Tests — Slack Reactions | 2 | `src/tests/slack_reactions_test.rs` |
| Tests — Slack Send Content Type | 2 | `src/tests/slack_send_content_type_test.rs` |
| Tests — Slack Structure | 10 | `src/tests/slack_structure_test.rs` |
| Tests — Slack Tool Group | 5 | `src/tests/slack_tool_group_test.rs` |
| Tests — Slash Autocomplete Dimensions | 18 | `src/tests/slash_autocomplete_dimensions_test.rs` |
| Tests — Slash Command Resolution | 4 | `src/tests/slash_command_resolution_test.rs` |
| Tests — Slash Models Target | 12 | `src/tests/slash_models_target_test.rs` |
| Tests — Split Pane | 21 | `src/tests/split_pane_test.rs` |
| Tests — Stale Topic Eviction | 3 | `src/tests/stale_topic_eviction_test.rs` |
| Tests — Start Gate Allowed User | 7 | `src/tests/start_gate_allowed_user_test.rs` |
| Tests — Startup Checks | 10 | `src/tests/startup_checks_test.rs` |
| Tests — Stop Intent | 11 | `src/tests/stop_intent_test.rs` |
| Tests — Stored Key | 6 | `src/tests/stored_key_test.rs` |
| Tests — Stream Cancel | 6 | `src/tests/stream_cancel_test.rs` |
| Tests — Stream Loop | 19 | `src/tests/stream_loop_test.rs` |
| Tests — Streaming Active Secs | 2 | `src/tests/streaming_active_secs_test.rs` |
| Tests — Streaming Tok Per Sec Guard | 10 | `src/tests/streaming_tok_per_sec_guard_test.rs` |
| Tests — Streaming Tps Accumulator | 12 | `src/tests/streaming_tps_accumulator_test.rs` |
| Tests — Stt Fallback Chain | 10 | `src/tests/stt_fallback_chain_test.rs` |
| Tests — Subagent Brain | 8 | `src/tests/subagent_brain_test.rs` |
| Tests — Subagent Compaction Preamble | 7 | `src/tests/subagent_compaction_preamble_test.rs` |
| Tests — Subagent Natural Completion | 3 | `src/tests/subagent_natural_completion_test.rs` |
| Tests — Subagent Notify | 15 | `src/tests/subagent_notify_test.rs` |
| Tests — Subagent Provider Pair | 5 | `src/tests/subagent_provider_pair_test.rs` |
| Tests — Subagent Push Result | 11 | `src/tests/subagent_push_result_test.rs` |
| Tests — Subagent Session Ttl | 10 | `src/tests/subagent_session_ttl_test.rs` |
| Tests — Subagent | 82 | `src/tests/subagent_test.rs` |
| Tests — Subagent Tool Description | 8 | `src/tests/subagent_tool_description_test.rs` |
| Tests — Subagent Worktree | 8 | `src/tests/subagent_worktree_test.rs` |
| Tests — Suggest Options | 10 | `src/tests/suggest_options_test.rs` |
| Tests — System Continuation | 6 | `src/tests/system_continuation_test.rs` |
| Tests — Systemd Unit | 3 | `src/tests/systemd_unit_test.rs` |
| Tests — Tasks List | 5 | `src/tests/tasks_list_test.rs` |
| Tests — Telegram Acl | 12 | `src/tests/telegram_acl_test.rs` |
| Tests — Telegram Attachment Tmp Name | 9 | `src/tests/telegram_attachment_tmp_name_test.rs` |
| Tests — Telegram Atx Heading Agreement | 6 | `src/tests/telegram_atx_heading_agreement_test.rs` |
| Tests — Telegram Bg Ack Fold | 7 | `src/tests/telegram_bg_ack_fold_test.rs` |
| Tests — Telegram Bg Resume Gate | 6 | `src/tests/telegram_bg_resume_gate_test.rs` |
| Tests — Telegram Callback Session Topic | 5 | `src/tests/telegram_callback_session_topic_test.rs` |
| Tests — Telegram Cancel Token No Drop | 2 | `src/tests/telegram_cancel_token_no_drop_test.rs` |
| Tests — Telegram Caption | 3 | `src/tests/telegram_caption_test.rs` |
| Tests — Telegram Command Sanitize | 12 | `src/tests/telegram_command_sanitize_test.rs` |
| Tests — Telegram Dedup Approval | 6 | `src/tests/telegram_dedup_approval_test.rs` |
| Tests — Telegram Details Fallback Render | 6 | `src/tests/telegram_details_fallback_render_test.rs` |
| Tests — Telegram Ephemeral | 9 | `src/tests/telegram_ephemeral_test.rs` |
| Tests — Telegram Flow Chrome | 50 | `src/tests/telegram_flow_chrome_test.rs` |
| Tests — Telegram Folded Reclaim Suppression | 6 | `src/tests/telegram_folded_reclaim_suppression_test.rs` |
| Tests — Telegram Followup Pick | 25 | `src/tests/telegram_followup_pick_test.rs` |
| Tests — Telegram General Topic Delivery | 11 | `src/tests/telegram_general_topic_delivery_test.rs` |
| Tests — Telegram Group History Capture | 5 | `src/tests/telegram_group_history_capture_test.rs` |
| Tests — Telegram Group Migration | 6 | `src/tests/telegram_group_migration_test.rs` |
| Tests — Telegram Group Name | 12 | `src/tests/telegram_group_name_test.rs` |
| Tests — Telegram Group Sender Label | 3 | `src/tests/telegram_group_sender_label_test.rs` |
| Tests — Telegram Handler | 17 | `src/tests/telegram_handler_test.rs` |
| Tests — Telegram Impersonation | 7 | `src/tests/telegram_impersonation_test.rs` |
| Tests — Telegram Join Detection | 13 | `src/tests/telegram_join_detection_test.rs` |
| Tests — Telegram Last Intermediate Footer | 7 | `src/tests/telegram_last_intermediate_footer_test.rs` |
| Tests — Telegram Long Rate Limit | 3 | `src/tests/telegram_long_rate_limit_test.rs` |
| Tests — Telegram Md To Html | 8 | `src/tests/telegram_md_to_html_test.rs` |
| Tests — Telegram Mentions Other Bot | 6 | `src/tests/telegram_mentions_other_bot_test.rs` |
| Tests — Telegram Menu Scope | 6 | `src/tests/telegram_menu_scope_test.rs` |
| Tests — Telegram Mermaid | 59 | `src/tests/telegram_mermaid_test.rs` |
| Tests — Telegram Model Callback Data | 3 | `src/tests/telegram_model_callback_data_test.rs` |
| Tests — Telegram Newest Msg Id | 7 | `src/tests/telegram_newest_msg_id_test.rs` |
| Tests — Telegram Options Reclaim | 20 | `src/tests/telegram_options_reclaim_test.rs` |
| Tests — Telegram Outbound Dedup | 3 | `src/tests/telegram_outbound_dedup_test.rs` |
| Tests — Telegram Outbox Record | 4 | `src/tests/telegram_outbox_record_test.rs` |
| Tests — Telegram Photo Batching | 8 | `src/tests/telegram_photo_batching_test.rs` |
| Tests — Telegram Photo Thread Scope | 5 | `src/tests/telegram_photo_thread_scope_test.rs` |
| Tests — Telegram Plan Card Interrupt Restick | 2 | `src/tests/telegram_plan_card_interrupt_restick_test.rs` |
| Tests — Telegram Plan Finalize | 4 | `src/tests/telegram_plan_finalize_test.rs` |
| Tests — Telegram Plan Render | 9 | `src/tests/telegram_plan_render_test.rs` |
| Tests — Telegram Pre Tool Rolling | 1 | `src/tests/telegram_pre_tool_rolling_test.rs` |
| Tests — Telegram Queued Origin | 4 | `src/tests/telegram_queued_origin_test.rs` |
| Tests — Telegram Quote Reply | 19 | `src/tests/telegram_quote_reply_test.rs` |
| Tests — Telegram Raw Update Parse | 2 | `src/tests/telegram_raw_update_parse_test.rs` |
| Tests — Telegram React Delivery | 5 | `src/tests/telegram_react_delivery_test.rs` |
| Tests — Telegram Reaction Map | 5 | `src/tests/telegram_reaction_map_test.rs` |
| Tests — Telegram Reaction Prompt | 11 | `src/tests/telegram_reaction_prompt_test.rs` |
| Tests — Telegram Reaction Queue | 7 | `src/tests/telegram_reaction_queue_test.rs` |
| Tests — Telegram Reaction Routing | 7 | `src/tests/telegram_reaction_routing_test.rs` |
| Tests — Telegram Reflow Collapsed Table | 9 | `src/tests/telegram_reflow_collapsed_table_test.rs` |
| Tests — Telegram Reply Context Recovery | 8 | `src/tests/telegram_reply_context_recovery_test.rs` |
| Tests — Telegram Resume Labels | 13 | `src/tests/telegram_resume_labels_test.rs` |
| Tests — Telegram Resume | 65 | `src/tests/telegram_resume_test.rs` |
| Tests — Telegram Retain History | 8 | `src/tests/telegram_retain_history_test.rs` |
| Tests — Telegram Rich Api | 7 | `src/tests/telegram_rich_api_test.rs` |
| Tests — Telegram Rich Decode Official | 13 | `src/tests/telegram_rich_decode_official_test.rs` |
| Tests — Telegram Rich Decode | 21 | `src/tests/telegram_rich_decode_test.rs` |
| Tests — Telegram Rich Edit Dedup | 5 | `src/tests/telegram_rich_edit_dedup_test.rs` |
| Tests — Telegram Rich Json | 12 | `src/tests/telegram_rich_json_test.rs` |
| Tests — Telegram Rich Parse | 30 | `src/tests/telegram_rich_parse_test.rs` |
| Tests — Telegram Rich | 11 | `src/tests/telegram_rich_test.rs` |
| Tests — Telegram Rich Transport Retry | 8 | `src/tests/telegram_rich_transport_retry_test.rs` |
| Tests — Telegram Rich Wrap P | 9 | `src/tests/telegram_rich_wrap_p_test.rs` |
| Tests — Telegram Send Caption | 9 | `src/tests/telegram_send_caption_test.rs` |
| Tests — Telegram Send Input File | 5 | `src/tests/telegram_send_input_file_test.rs` |
| Tests — Telegram Send Origin Thread Echo | 7 | `src/tests/telegram_send_origin_thread_echo_test.rs` |
| Tests — Telegram Send Retry | 7 | `src/tests/telegram_send_retry_test.rs` |
| Tests — Telegram Send String Coercion | 3 | `src/tests/telegram_send_string_coercion_test.rs` |
| Tests — Telegram Send Thread Id Override | 10 | `src/tests/telegram_send_thread_id_override_test.rs` |
| Tests — Telegram Session Gate | 5 | `src/tests/telegram_session_gate_test.rs` |
| Tests — Telegram Session Resolve | 20 | `src/tests/telegram_session_resolve_test.rs` |
| Tests — Telegram Split Message | 11 | `src/tests/telegram_split_message_test.rs` |
| Tests — Telegram State | 7 | `src/tests/telegram_state_test.rs` |
| Tests — Telegram Status Message | 15 | `src/tests/telegram_status_message_test.rs` |
| Tests — Telegram Stream Loop Resume | 1 | `src/tests/telegram_stream_loop_resume_test.rs` |
| Tests — Telegram Suggest Merge | 19 | `src/tests/telegram_suggest_merge_test.rs` |
| Tests — Telegram System Chrome Reclaim | 7 | `src/tests/telegram_system_chrome_reclaim_test.rs` |
| Tests — Telegram Table Blank Line | 6 | `src/tests/telegram_table_blank_line_test.rs` |
| Tests — Telegram Table Render | 2 | `src/tests/telegram_table_render_test.rs` |
| Tests — Telegram Target Resolver | 9 | `src/tests/telegram_target_resolver_test.rs` |
| Tests — Telegram Telemetry | 3 | `src/tests/telegram_telemetry_test.rs` |
| Tests — Telegram Thread Id Lookup | 10 | `src/tests/telegram_thread_id_lookup_test.rs` |
| Tests — Telegram Token Redaction | 6 | `src/tests/telegram_token_redaction_test.rs` |
| Tests — Telegram Tool Group | 65 | `src/tests/telegram_tool_group_test.rs` |
| Tests — Telegram Topic Listing | 8 | `src/tests/telegram_topic_listing_test.rs` |
| Tests — Telegram Topic Routing | 3 | `src/tests/telegram_topic_routing_test.rs` |
| Tests — Telegram Userbot Boundary | 2 | `src/tests/telegram_userbot_boundary_test.rs` |
| Tests — Telegram Userbot Config | 6 | `src/tests/telegram_userbot_config_test.rs` |
| Tests — Telegram Userbot Reconcile | 4 | `src/tests/telegram_userbot_reconcile_test.rs` |
| Tests — Telegram Userbot Runner | 2 | `src/tests/telegram_userbot_runner_test.rs` |
| Tests — Telegram Userbot Session | 4 | `src/tests/telegram_userbot_session_test.rs` |
| Tests — Template Governance | 20 | `src/tests/template_governance_test.rs` |
| Tests — Text Complete | 21 | `src/tests/text_complete_test.rs` |
| Tests — Theme Catalog Converter | 11 | `src/tests/theme_catalog_converter_test.rs` |
| Tests — Theme Pack | 3 | `src/tests/theme_pack_test.rs` |
| Tests — Theme Persist Roundtrip | 4 | `src/tests/theme_persist_roundtrip_test.rs` |
| Tests — Thinking Loop Fallback | 5 | `src/tests/thinking_loop_fallback_test.rs` |
| Tests — Token Report Calibration | 9 | `src/tests/token_report_calibration_test.rs` |
| Tests — Token Tracking | 29 | `src/tests/token_tracking_test.rs` |
| Tests — Toml Hot Reload | 6 | `src/tests/toml_hot_reload_test.rs` |
| Tests — Toml Merge | 9 | `src/tests/toml_merge_test.rs` |
| Tests — Tool Arg Unescape | 10 | `src/tests/tool_arg_unescape_test.rs` |
| Tests — Tool Context Origin Target | 3 | `src/tests/tool_context_origin_target_test.rs` |
| Tests — Tool Description Redaction | 6 | `src/tests/tool_description_redaction_test.rs` |
| Tests — Tool Execution Repo | 8 | `src/tests/tool_execution_repo_test.rs` |
| Tests — Tool Execution Stats | 2 | `src/tests/tool_execution_stats_test.rs` |
| Tests — Tool Loop Helpers | 40 | `src/tests/tool_loop_helpers_test.rs` |
| Tests — Tool Name Heal | 11 | `src/tests/tool_name_heal_test.rs` |
| Tests — Tool Process Kill On Drop | 2 | `src/tests/tool_process_kill_on_drop_test.rs` |
| Tests — Tool Repeat | 13 | `src/tests/tool_repeat_test.rs` |
| Tests — Tool Search Activation | 5 | `src/tests/tool_search_activation_test.rs` |
| Tests — Tool Search Child Registry | 2 | `src/tests/tool_search_child_registry_test.rs` |
| Tests — Tools Md Regression | 9 | `src/tests/tools_md_regression_test.rs` |
| Tests — Tracing Session Id | 3 | `src/tests/tracing_session_id_test.rs` |
| Tests — Transport Ready | 7 | `src/tests/transport_ready_test.rs` |
| Tests — Truncation Join | 15 | `src/tests/truncation_join_test.rs` |
| Tests — Truncation | 13 | `src/tests/truncation_test.rs` |
| Tests — Tts Fallback Chain | 11 | `src/tests/tts_fallback_chain_test.rs` |
| Tests — TUI App State | 2 | `src/tests/tui_app_state_test.rs` |
| Tests — Tui Cancel Indicator | 4 | `src/tests/tui_cancel_indicator_test.rs` |
| Tests — TUI Compact Notice | 2 | `src/tests/tui_compact_notice_test.rs` |
| Tests — TUI Components Logo | 2 | `src/tests/tui_components_logo_test.rs` |
| Tests — TUI Drop Path | 4 | `src/tests/tui_drop_path_test.rs` |
| Tests — TUI Dropped Path | 10 | `src/tests/tui_dropped_path_test.rs` |
| Tests — TUI Error | 21 | `src/tests/tui_error_test.rs` |
| Tests — TUI Events | 4 | `src/tests/tui_events_test.rs` |
| Tests — TUI Highlight | 8 | `src/tests/tui_highlight_test.rs` |
| Tests — TUI Markdown | 9 | `src/tests/tui_markdown_test.rs` |
| Tests — TUI Notice Render | 4 | `src/tests/tui_notice_render_test.rs` |
| Tests — TUI Notice | 19 | `src/tests/tui_notice_test.rs` |
| Tests — TUI Palette | 4 | `src/tests/tui_palette_test.rs` |
| Tests — TUI Plan | 38 | `src/tests/tui_plan_tests_test.rs` |
| Tests — TUI Process Commands | 3 | `src/tests/tui_process_commands_test.rs` |
| Tests — TUI Remote Upload | 12 | `src/tests/tui_remote_upload_test.rs` |
| Tests — TUI Queue Indicator | 8 | `src/tests/tui_queue_indicator_test.rs` |
| Tests — TUI Reclaim After Panic | 9 | `src/tests/tui_reclaim_after_panic_test.rs` |
| Tests — TUI Render Clear | 4 | `src/tests/tui_render_clear_test.rs` |
| Tests — TUI Render Utils | 12 | `src/tests/tui_render_utils_test.rs` |
| Tests — TUI Session Switch State | 7 | `src/tests/tui_session_switch_state_test.rs` |
| Tests — TUI Theme Picker | 10 | `src/tests/tui_theme_picker_test.rs` |
| Tests — Tui Theme Presets | 15 | `src/tests/tui_theme_presets_test.rs` |
| Tests — TUI Tool Stack | 10 | `src/tests/tui_tool_stack_test.rs` |
| Tests — TUI Unconsumed Queue | 5 | `src/tests/tui_unconsumed_queue_test.rs` |
| Tests — TUI User Themes | 10 | `src/tests/tui_user_themes_test.rs` |
| Tests — Turn Duration | 6 | `src/tests/turn_duration_test.rs` |
| Tests — Turn Ranges | 40 | `src/tests/turn_ranges_test.rs` |
| Tests — Usage Activity Columns | 9 | `src/tests/usage_activity_columns_test.rs` |
| Tests — Usage Cache | 15 | `src/tests/usage_cache_test.rs` |
| Tests — Usage Categorizer | 4 | `src/tests/usage_categorizer_test.rs` |
| Tests — Usage Cosmetic Alias | 17 | `src/tests/usage_cosmetic_alias_test.rs` |
| Tests — Usage Dashboard | 6 | `src/tests/usage_dashboard_test.rs` |
| Tests — Usage Data | 7 | `src/tests/usage_data_test.rs` |
| Tests — Usage Grouping | 18 | `src/tests/usage_grouping_test.rs` |
| Tests — Usage Ledger Attribution | 3 | `src/tests/usage_ledger_attribution_test.rs` |
| Tests — Usage Ledger | 5 | `src/tests/usage_ledger_test.rs` |
| Tests — User Correction Metadata | 3 | `src/tests/user_correction_metadata_test.rs` |
| Tests — Utc Timestamp | 4 | `src/tests/utc_timestamp_test.rs` |
| Tests — Utils File Extract | 8 | `src/tests/utils_file_extract_test.rs` |
| Tests — Utils Install | 6 | `src/tests/utils_install_test.rs` |
| Tests — Utils Retry | 8 | `src/tests/utils_retry_test.rs` |
| Tests — Utils Sanitize | 49 | `src/tests/utils_sanitize_test.rs` |
| Tests — Utils String | 26 | `src/tests/utils_string_test.rs` |
| Tests — Variation Directive | 4 | `src/tests/variation_directive_test.rs` |
| Tests — Vba Modules | 10 | `src/tests/vba_modules_test.rs` |
| Tests — Voice Chain | 7 | `src/tests/voice_chain_test.rs` |
| Tests — Voice Config Derivation | 7 | `src/tests/voice_config_derivation_test.rs` |
| Tests — Voice Flag Flips | 4 | `src/tests/voice_flag_flips_test.rs` |
| Tests — Voice Local Tts | 9 | `src/tests/voice_local_tts_test.rs` |
| Tests — Voice Local Whisper | 25 | `src/tests/voice_local_whisper_test.rs` |
| Tests — Voice Onboarding | 67 | `src/tests/voice_onboarding_test.rs` |
| Tests — Voice Openai Compatible | 12 | `src/tests/voice_openai_compatible_test.rs` |
| Tests — Voice Service | 14 | `src/tests/voice_service_test.rs` |
| Tests — Voice Stt Dispatch | 21 | `src/tests/voice_stt_dispatch_test.rs` |
| Tests — Voice Text Cleanup | 6 | `src/tests/voice_text_cleanup_test.rs` |
| Tests — Voice Voicebox | 15 | `src/tests/voice_voicebox_test.rs` |
| Tests — Wait Agent Resolver | 12 | `src/tests/wait_agent_resolver_test.rs` |
| Tests — Web Browser Routing | 9 | `src/tests/web_browser_routing_test.rs` |
| Tests — Web Scrape Benchmark | 1 | `src/tests/web_scrape_benchmark_test.rs` |
| Tests — Web Scrape Clean | 7 | `src/tests/web_scrape_clean_test.rs` |
| Tests — Web Scrape Export | 5 | `src/tests/web_scrape_export_test.rs` |
| Tests — Web Scrape Extract | 5 | `src/tests/web_scrape_extract_test.rs` |
| Tests — Web Scrape Fetch | 4 | `src/tests/web_scrape_fetch_test.rs` |
| Tests — Web Scrape Markdown | 6 | `src/tests/web_scrape_markdown_test.rs` |
| Tests — Web Scrape Sitemap | 6 | `src/tests/web_scrape_sitemap_test.rs` |
| Tests — Web Scrape Ssrf | 14 | `src/tests/web_scrape_ssrf_test.rs` |
| Tests — Web Scrape Tool | 6 | `src/tests/web_scrape_tool_test.rs` |
| Tests — Web Search | 4 | `src/tests/web_search_test.rs` |
| Tests — Whatsapp Handler | 6 | `src/tests/whatsapp_handler_test.rs` |
| Tests — Whatsapp Owner Filter | 15 | `src/tests/whatsapp_owner_filter_test.rs` |
| Tests — Whatsapp Photo Batching | 11 | `src/tests/whatsapp_photo_batching_test.rs` |
| Tests — Whatsapp Qr Replay | 4 | `src/tests/whatsapp_qr_replay_test.rs` |
| Tests — Whatsapp Rate Limit | 10 | `src/tests/whatsapp_rate_limit_test.rs` |
| Tests — Whatsapp State | 8 | `src/tests/whatsapp_state_test.rs` |
| Tests — Whatsapp Store | 15 | `src/tests/whatsapp_store_test.rs` |
| Tests — Word Delete Keybinding | 7 | `src/tests/word_delete_keybinding_test.rs` |
| Tests — Work Status Output Full | 4 | `src/tests/work_status_output_full_test.rs` |
| Tests — Work Status Parent Binding | 4 | `src/tests/work_status_parent_binding_test.rs` |
| Tests — Write Opencrabs File Inline | 4 | `src/tests/write_opencrabs_file_inline_test.rs` |
| Tests — Write Partial View Guard | 4 | `src/tests/write_partial_view_guard_test.rs` |
| Tests — Xiaomi Config Default | 3 | `src/tests/xiaomi_config_default_test.rs` |
| Tests — Xiaomi Keyed Provider Regression | 4 | `src/tests/xiaomi_keyed_provider_regression_test.rs` |
| Tests — Xiaomi Onboarding | 5 | `src/tests/xiaomi_onboarding_test.rs` |
| Tests — Zhipu Endpoint | 4 | `src/tests/zhipu_endpoint_test.rs` |

---

## Feature-Gated Tests

Some tests only compile/run with specific feature flags:

| Feature | Tests |
|---------|-------|
| `local-stt` | Local whisper inline tests, candle whisper tests, STT dispatch local-mode tests, codec tests, availability cycling tests |
| `local-tts` | TTS voice cycling, Piper voice Up/Down |
| `code-graph` | `memory_symbol_extractor_test`, `memory_symbol_extractor_tree_test`, `memory_search_code_graph_test` (tree-sitter symbol extraction and structural search) |

All feature-gated tests use `#[cfg(feature = "...")]` and are automatically included when running with `--all-features`.

---

## Running Tests

```bash
# Run all tests (recommended)
cargo test --all-features

# Run a specific test module
cargo test --all-features -- voice_onboarding_test

# Run a single test
cargo test --all-features -- is_newer_major_bump

# Run with output (for debugging)
cargo test --all-features -- --nocapture

# Run only local-stt tests
cargo test --features local-stt -- local_whisper
```

---

## Profile Tests

Profile tests live in `src/tests/profile_test.rs` and cover multi-instance isolation:

| Area | What's tested |
|------|--------------|
| Name validation | Reserved names, length bounds, special characters |
| Token hashing | Determinism, uniqueness, fixed length, hex output |
| Registry (in-memory) | CRUD, serde roundtrip, touch timestamps |
| Path resolution | Base dir, env var override, default vs named profiles |
| Filesystem CRUD | Create/delete lifecycle, duplicate detection, registry sync |
| Export/Import | Roundtrip with config files, nested memory directories |
| Migration | Copy `.md`/`.toml` files, skip/force behavior, default source |
| Token locks | Acquire/release, stale PID cleanup, cross-profile conflict |
| Profile isolation | Separate directories, concurrent writes, default vs named |
| Concurrent writes | Tokio tasks creating 5 profiles simultaneously |

```bash
# Run profile tests only
cargo test --all-features -p opencrabs -- profile_test
```

**Note:** All filesystem-touching tests acquire a global `fs_lock()` mutex to prevent concurrent write corruption of `~/.opencrabs/profiles.toml`. The mutex uses `unwrap_or_else(|p| p.into_inner())` to recover from poison (a prior test panic won't cascade-fail every subsequent test). In-memory tests run in parallel without the lock. The `test_set_and_get_active_profile` test accounts for `OnceLock` semantics (can only be set once per process).

---

## Files Absent From the Quick Reference

These are registered in `src/tests/mod.rs` but contribute no tests to a
macOS `--all-features` run, so they carry no count in the table above:

| File | Reason |
|------|--------|
| `agent_service_mocks` | Shared mock providers and helpers, not a test module |
| `browser_default_linux_test` | Platform-gated; runs on Linux only |
| `browser_default_windows_test` | Platform-gated; runs on Windows only |
| `service_scope_test` | `#[cfg(target_os = "linux")]`, systemd unit-scope probe |
| `intermediate_text_strip_guard_test` | Placeholder stub, no tests yet |

---

## Phantom Detection Tests

The self-healing phantom detector prevents the agent from dropping requests mid-stream when it says it will investigate something but never calls tools.

### Coverage

Tests in `src/tests/self_healing_test.rs` verify detection of investigative intent phrases:

| Phrase Pattern | Examples |
|---------------|----------|
| `let me hunt/trace/track` | "let me hunt down the bug", "let me trace the request" |
| `let me look into/check into` | "let me look into that", "let me check into the logs" |
| `let me find out/dig into` | "let me find out why", "let me dig into the code" |
| `i'll hunt/trace/track` | "i'll hunt that down", "i'll trace the flow" |
| `i'll look into/check into` | "i'll look into it", "i'll check into the error" |
| `i'll find out/dig into` | "i'll find out what's wrong", "i'll dig into the issue" |

### Behavior

When the agent outputs one of these phrases with zero tool calls, the phantom detector:
1. Catches the mismatch between intent and action
2. Injects a correction forcing tool invocation
3. Prevents the response from ending with unexecuted promises

### Test Count

88 tests covering phrase detection, edge cases, and integration with the tool loop.
| Tests — A2A Gate Auth | 8 | `src/tests/a2a_gate_auth_test.rs` |
| Tests — Browser Default Linux | 4 | `src/tests/browser_default_linux_test.rs` |
| Tests — Browser Default Windows | 6 | `src/tests/browser_default_windows_test.rs` |
| Tests — Channel Approval Owner Gate | 2 | `src/tests/channel_approval_owner_gate_test.rs` |
| Tests — Channel Deny By Default | 4 | `src/tests/channel_deny_by_default_test.rs` |
| Tests — Confidential Read Deny | 5 | `src/tests/confidential_read_deny_test.rs` |
| Tests — Cron Session Target | 7 | `src/tests/cron_session_target_test.rs` |
| Tests — Custom Provider Stream Empty Finish | 1 | `src/tests/custom_provider_stream_empty_finish_test.rs` |
| Tests — DB Thread ID Stamp Heal | 3 | `src/tests/db_thread_id_stamp_heal_test.rs` |
| Tests — Evolve Verify | 8 | `src/tests/evolve_verify_test.rs` |
| Tests — Execution Blocklist Floor | 4 | `src/tests/execution_blocklist_floor_test.rs` |
| Tests — Headless Tool Surface | 7 | `src/tests/headless_tool_surface_test.rs` |
| Tests — Secret Hygiene Oc05 | 4 | `src/tests/secret_hygiene_oc05_test.rs` |
| Tests — Service Scope | 11 | `src/tests/service_scope_test.rs` |
| Tests — Subagent Brain | 8 | `src/tests/subagent_brain_test.rs` |
| Tests — Tool Context Origin Target | 3 | `src/tests/tool_context_origin_target_test.rs` |
| Tests — Work Status Output Full | 4 | `src/tests/work_status_output_full_test.rs` |
| Tests — Stale Topic Eviction | 3 | `src/tests/stale_topic_eviction_test.rs` |
| Tests — OC Consumer Migration | 7 | `src/tests/oc_consumer_migration_test.rs` |
| Tests — OC Opaqueness Wall | 2 | `src/tests/oc_opaqueness_wall_test.rs` |
| Tests — OC Reverse Maps | 5 | `src/tests/oc_reverse_maps_test.rs` |
| Tests — OC Send Scope | 8 | `src/tests/oc_send_scope_test.rs` |