# TEST-MAP.md — WP-4 functional-parity acceptance map

This file cross-references every `it(...)` case in the TypeScript `orch-ts` application
test suite against the Rust test in `orch-engine` that covers it, or marks it `N/A` with a
reason. It is the acceptance artifact for WP-4 of the TS→Rust rewrite.

**Totals** (tallied directly from the two inventories below, not asserted): **419** TS
cases, of which **399** are mapped to a real Rust test and **20** are `N/A` (0 are
genuinely unmapped). The Rust side has **463** tests total across `cargo test -p
orch-engine -- --list`.

**Regenerating the inventories:**
- TS cases: for each file under `parity/orch-ts/test/{unit/application,integration}/*.ts`,
  walk the `describe`/`it` AST (or `vitest --reporter=verbose --run --list`) and record
  `(line, describe chain, it title)`.
- Rust tests: `cargo test -p orch-engine -- --list` from `crates/orch-engine`.

**The one rule that makes the counts not match 1:1:** a single Rust test commonly covers
several TS cases (the Rust port asserts multiple related facts — e.g. both halves of an
accept/reject pair — in one `#[test]`/`#[tokio::test]` function, named as a sentence). Where
that happens, the same Rust test name appears on consecutive rows and the `Note` column says
so.

## test/unit/application/agent-factory.test.ts — 12 cases

| TS case | Rust test | Note |
|---|---|---|
| `isMcpSkill` › returns true for colon-separated skills | `agent_factory::tests::mcp_skills_are_the_colon_separated_ones` | |
| `isMcpSkill` › returns false for library skills | `agent_factory::tests::mcp_skills_are_the_colon_separated_ones` | same test as row above |
| `templateToAgentInput` › resolves model for claude adapter | `agent_factory::tests::claude_gets_its_balanced_model_and_every_field_from_the_template` | |
| `templateToAgentInput` › resolves model for codex adapter | `agent_factory::tests::codex_gets_its_own_balanced_model` | |
| `templateToAgentInput` › rejects shell because shop templates do not define executable commands | `agent_factory::tests::shell_is_refused_because_a_template_carries_no_command` | |
| `templateToAgentInput` › resolves model for pi adapter | `agent_factory::tests::pi_gets_its_own_balanced_model` | |
| `templateToAgentInput` › filters MCP skills for pi adapter | `agent_factory::tests::every_other_adapter_drops_the_mcp_skills` | |
| `templateToAgentInput` › keeps all skills for claude adapter | `agent_factory::tests::claude_keeps_every_skill_including_the_mcp_ones` | |
| `templateToAgentInput` › filters MCP skills for non-claude adapters | `agent_factory::tests::every_other_adapter_drops_the_mcp_skills` | same test as two rows above |
| `templateToAgentInput` › filters MCP skills for codex adapter | `agent_factory::tests::every_other_adapter_drops_the_mcp_skills` | same test, codex is one of the looped adapters |
| `templateToAgentInput` › handles capable tier | `agent_factory::tests::the_capable_tier_resolves_to_the_capable_model` | |
| `templateToAgentInput` › handles unknown adapter gracefully | `agent_factory::tests::an_unknown_adapter_is_accepted_with_no_model` | |

## test/unit/application/agent-service.test.ts — 39 cases

| TS case | Rust test | Note |
|---|---|---|
| `AgentService > create` › creates an agent with defaults from config | `agent_service::tests::create_applies_the_configured_defaults` | |
| `AgentService > create` › throws on empty name | `agent_service::tests::an_empty_name_is_rejected_and_the_name_is_trimmed` | |
| `AgentService > create` › throws on duplicate name | `agent_service::tests::a_duplicate_name_is_rejected` | |
| `AgentService > create` › stores effort in config when provided | `agent_service::tests::create_stores_effort_when_provided_and_leaves_it_unset_otherwise` | |
| `AgentService > create` › leaves effort undefined when not provided | `agent_service::tests::create_stores_effort_when_provided_and_leaves_it_unset_otherwise` | same test, second half |
| `AgentService > create` › requires a non-empty command for shell agents | `agent_service::tests::a_shell_agent_without_a_command_is_rejected` | |
| `AgentService > create` › trims shell commands and defaults their approval policy to auto | `agent_service::tests::create_trims_the_shell_command`, `agent_service::tests::a_shell_agent_matches_the_reference_shape` | trimming and the auto-approval default are pinned by two separate Rust tests |
| `AgentService > get` › returns agent when found | `agent_service::tests::get_returns_the_agent_or_reports_not_found_with_exit_1` | |
| `AgentService > get` › throws AgentNotFoundError | `agent_service::tests::get_returns_the_agent_or_reports_not_found_with_exit_1` | same test, not-found half |
| `AgentService > remove` › removes idle agent | `agent_service::tests::remove_deletes_an_idle_agent` | |
| `AgentService > remove` › throws on running agent with active run | `agent_service::tests::remove_refuses_a_running_agent_that_has_a_live_run` | |
| `AgentService > remove` › resets stale running status and deletes | `agent_service::tests::remove_resets_a_stale_running_status_and_then_deletes` | |
| `AgentService > disable / enable` › disables an agent | `agent_service::tests::disable_and_enable_move_the_status` | |
| `AgentService > disable / enable` › enables a disabled agent | `agent_service::tests::disable_and_enable_move_the_status` | same test, enable half |
| `AgentService > update` › sets adapter via update | `agent_service::tests::update_sets_the_adapter` | |
| `AgentService > update` › throws when updating adapter to empty string | `agent_service::tests::update_rejects_a_blank_adapter` | |
| `AgentService > update` › clears model when set to empty string | `agent_service::tests::update_clears_the_model_when_set_to_an_empty_string` | |
| `AgentService > update` › sets effort via update | `agent_service::tests::update_sets_and_clears_the_effort` | |
| `AgentService > update` › clears effort when set to empty string | `agent_service::tests::update_sets_and_clears_the_effort` | same test, clears half |
| `AgentService > update` › requires a command when switching an agent to shell | `agent_service::tests::update_requires_a_command_when_switching_to_shell` | |
| `AgentService > update` › updates and trims a shell command | `agent_service::tests::update_trims_a_shell_command` | |
| `AgentService > update` › preserves an explicit policy when editing an existing shell agent | `agent_service::tests::update_preserves_an_explicit_policy_on_an_existing_shell_agent` | |
| `AgentService > update` › can switch to shell when a command is provided in the same update | `agent_service::tests::update_can_switch_to_shell_when_a_command_comes_in_the_same_call` | |
| `AgentService > findBestAgent` › returns null when no agents available | `agent_service::tests::find_best_agent_returns_none_when_no_agents_are_available` | |
| `AgentService > findBestAgent` › returns assigned agent if idle | `agent_service::tests::an_explicit_assignee_is_honoured_only_while_idle` | |
| `AgentService > findBestAgent` › returns null if assigned agent is not idle | `agent_service::tests::an_explicit_assignee_is_honoured_only_while_idle` | same test, not-idle half |
| `AgentService > findBestAgent` › matches by label via role | `agent_service::tests::a_label_matches_through_the_role` | |
| `AgentService > findBestAgent` › prefers skill match over role match | `agent_service::tests::a_skill_match_beats_a_role_match` | |
| `AgentService > findBestAgent` › scores multiple skill matches cumulatively | `agent_service::tests::skill_matches_accumulate` | |
| `AgentService > findBestAgent` › applies success rate bonus | `agent_service::tests::the_success_rate_bonus_breaks_an_otherwise_equal_tie` | |
| `AgentService > findBestAgent` › falls back to first idle agent | `agent_service::tests::only_idle_agents_are_candidates` | |
| `AgentService > findBestAgent` › excludes disabled agents from matching | `agent_service::tests::a_disabled_agent_is_excluded_even_with_a_perfect_skill_match` | |
| `AgentService > findBestAgent` › matches skills case-insensitively | `agent_service::tests::skills_match_case_insensitively` | |
| `AgentService > findBestAgent` › role substring matching can cause false positives (known issue) | `agent_service::tests::role_matching_is_a_substring_test_and_can_false_positive` | |
| `AgentService > findBestAgent` › combines skill + role + success rate scoring | `agent_service::tests::skill_role_and_success_rate_combine` | |
| `AgentService > findBestAgent` › returns null when all agents are disabled or running | `agent_service::tests::none_when_every_agent_is_disabled_or_running` | |
| `AgentService > findBestAgent` › ignores assignee if agent does not exist | `agent_service::tests::an_assignee_that_matches_no_agent_yields_none` | |
| `AgentService > findBestAgent` › handles agents with zero task history (no success bonus) | `agent_service::tests::zero_task_history_scores_no_success_bonus_and_does_not_divide_by_zero` | |
| `AgentService > findBestAgent` › does not match labels against agents without skills or role | `agent_service::tests::labels_do_not_match_an_agent_with_neither_skills_nor_a_role` | |

## test/unit/application/defensive-validation.test.ts — 42 cases

| TS case | Rust test | Note |
|---|---|---|
| `Fix #1: TaskService priority validation [1-4] > create()` › accepts priority 1 | `task_service::tests::priority_must_be_between_1_and_4` | |
| `Fix #1: TaskService priority validation [1-4] > create()` › accepts priority 4 | `task_service::tests::priority_must_be_between_1_and_4` | same test |
| `Fix #1: TaskService priority validation [1-4] > create()` › rejects priority 0 | `task_service::tests::priority_must_be_between_1_and_4` | same test |
| `Fix #1: TaskService priority validation [1-4] > create()` › rejects priority 5 | `task_service::tests::priority_must_be_between_1_and_4` | same test |
| `Fix #1: TaskService priority validation [1-4] > create()` › rejects negative priority | `task_service::tests::priority_must_be_between_1_and_4` | same test, covers -1 |
| `Fix #1: TaskService priority validation [1-4] > create()` › rejects fractional priority | *(N/A)* | N/A — unrepresentable: the Rust parameter is typed (`i64`), so the input cannot be constructed. The range check is covered by `task_service::tests::priority_must_be_between_1_and_4`; the representational edge is pinned by `task_service::tests::a_non_integral_priority_is_unrepresentable`. |
| `Fix #1: TaskService priority validation [1-4] > create()` › rejects NaN priority | *(N/A)* | N/A — unrepresentable (`i64`). See `task_service::tests::a_non_integral_priority_is_unrepresentable`. |
| `Fix #1: TaskService priority validation [1-4] > create()` › rejects Infinity priority | *(N/A)* | N/A — unrepresentable (`i64`). `a_non_integral_priority_is_unrepresentable` exercises the nearest constructible case (`i64::MAX`/`MIN`). |
| `Fix #1: TaskService priority validation [1-4] > update()` › accepts valid priority update to 1 | `task_service::tests::update_rejects_an_empty_title_and_an_out_of_range_priority` | |
| `Fix #1: TaskService priority validation [1-4] > update()` › accepts valid priority update to 4 | `task_service::tests::update_rejects_an_empty_title_and_an_out_of_range_priority` | same test |
| `Fix #1: TaskService priority validation [1-4] > update()` › rejects priority 0 in update | `task_service::tests::update_rejects_an_empty_title_and_an_out_of_range_priority` | same test |
| `Fix #1: TaskService priority validation [1-4] > update()` › rejects priority 5 in update | `task_service::tests::update_rejects_an_empty_title_and_an_out_of_range_priority` | same test |
| `Fix #1: TaskService priority validation [1-4] > update()` › rejects fractional priority in update | *(N/A)* | N/A — unrepresentable (`i64`). Range check covered by `task_service::tests::update_rejects_an_empty_title_and_an_out_of_range_priority`. |
| `Fix #2: Orchestrator retry_queue bounds and dedup` › retry_queue dedup: same task_id is not added twice | `orchestrator::tests::endurance::the_retry_queue_is_deduplicated_by_task_id` | |
| `Fix #2: Orchestrator retry_queue bounds and dedup` › retry_queue drops oldest when at capacity (100) | `orchestrator::tests::endurance::the_retry_queue_is_capped_at_a_hundred_entries_dropping_the_oldest` | |
| `Fix #2: Orchestrator retry_queue bounds and dedup` › maxRetryQueueSize is 100 in Orchestrator class | `orchestrator::tests::defensive_validation::the_retry_queue_bound_is_a_hundred` | asserts the real `MAX_RETRY_QUEUE_SIZE` constant rather than grepping source text |
| `Fix #3: ContextStore TTL bounds validation` › accepts valid TTL (1ms) | *(N/A)* | N/A here — `ContextStore` is `orch-infra`'s. |
| `Fix #3: ContextStore TTL bounds validation` › accepts TTL at max bound (30 days) | *(N/A)* | N/A here — `ContextStore` is `orch-infra`'s. |
| `Fix #3: ContextStore TTL bounds validation` › accepts no TTL (undefined) | *(N/A)* | N/A here — `ContextStore` is `orch-infra`'s. |
| `Fix #3: ContextStore TTL bounds validation` › rejects TTL = 0 | *(N/A)* | N/A here — `ContextStore` is `orch-infra`'s. |
| `Fix #3: ContextStore TTL bounds validation` › rejects negative TTL | *(N/A)* | N/A here — `ContextStore` is `orch-infra`'s. |
| `Fix #3: ContextStore TTL bounds validation` › rejects TTL exceeding 30 days | *(N/A)* | N/A here — `ContextStore` is `orch-infra`'s. |
| `Fix #3: ContextStore TTL bounds validation` › rejects NaN TTL | *(N/A)* | N/A here — `ContextStore` is `orch-infra`'s. |
| `Fix #3: ContextStore TTL bounds validation` › rejects Infinity TTL | *(N/A)* | N/A here — `ContextStore` is `orch-infra`'s. |
| `Fix #3: ContextStore TTL bounds validation` › rejects -Infinity TTL | *(N/A)* | N/A here — `ContextStore` is `orch-infra`'s. |
| `Fix #4: AgentService optional chaining on task.labels` › handles task with undefined labels (no crash) | `agent_service::tests::an_empty_or_absent_label_list_scores_the_idle_bonus_only` | |
| `Fix #4: AgentService optional chaining on task.labels` › handles task with null labels (no crash) | `agent_service::tests::an_empty_or_absent_label_list_scores_the_idle_bonus_only` | same test, `None` case |
| `Fix #4: AgentService optional chaining on task.labels` › handles task with empty labels array | `agent_service::tests::an_empty_or_absent_label_list_scores_the_idle_bonus_only` | same test, `Some(&[])` case |
| `Fix #4: AgentService optional chaining on task.labels` › still scores correctly when labels are provided | `agent_service::tests::an_empty_or_absent_label_list_scores_the_idle_bonus_only` | same test, final assertion |
| `Fix #5: isValidISOTimestamp validation` › accepts valid ISO timestamp | `orchestrator::tests::defensive_validation::the_canonical_iso_form_is_accepted` | |
| `Fix #5: isValidISOTimestamp validation` › accepts another valid ISO timestamp | `orchestrator::tests::defensive_validation::the_canonical_iso_form_is_accepted` | same test |
| `Fix #5: isValidISOTimestamp validation` › rejects non-string (number) | *(N/A)* | N/A — unrepresentable: the Rust parameter is `&str`. `orchestrator::tests::defensive_validation::a_non_string_timestamp_is_unrepresentable` documents this and pins the nearest constructible degenerate input. |
| `Fix #5: isValidISOTimestamp validation` › rejects non-string (undefined) | *(N/A)* | N/A — unrepresentable (`&str`). See `a_non_string_timestamp_is_unrepresentable`. |
| `Fix #5: isValidISOTimestamp validation` › rejects non-string (null) | *(N/A)* | N/A — unrepresentable (`&str`). See `a_non_string_timestamp_is_unrepresentable`. |
| `Fix #5: isValidISOTimestamp validation` › rejects empty string | `orchestrator::tests::defensive_validation::anything_that_is_not_the_canonical_form_is_rejected` | |
| `Fix #5: isValidISOTimestamp validation` › rejects malformed date string | `orchestrator::tests::defensive_validation::anything_that_is_not_the_canonical_form_is_rejected` | same test, "not a date" case |
| `Fix #5: isValidISOTimestamp validation` › rejects non-ISO date format (locale) | `orchestrator::tests::defensive_validation::anything_that_is_not_the_canonical_form_is_rejected` | same test, locale case |
| `Fix #5: isValidISOTimestamp validation` › rejects ISO without milliseconds if toISOString adds them | `orchestrator::tests::defensive_validation::anything_that_is_not_the_canonical_form_is_rejected` | same test, `"2025-01-01T00:00:00Z"` case |
| `Fix #5: isValidISOTimestamp validation` › rejects Invalid Date string | `orchestrator::tests::defensive_validation::anything_that_is_not_the_canonical_form_is_rejected` | same test, `"Invalid Date"` case |
| `Fix #5: isValidISOTimestamp validation` › fallback logic: invalid timestamp replaced with current ISO | `orchestrator::tests::defensive_validation::an_unusable_event_timestamp_is_replaced_with_the_clocks_own` | |
| `Fix #5: isValidISOTimestamp validation` › fallback logic: undefined timestamp replaced with current ISO | *(N/A)* | N/A — unrepresentable: `AgentEvent::timestamp` is a `String`, not `Option<String>`. `an_unusable_event_timestamp_is_replaced_with_the_clocks_own` covers the nearest constructible degenerate case (empty string). |
| `Fix #5: isValidISOTimestamp validation` › function exists in orchestrator.ts source | `orchestrator::tests::defensive_validation::the_canonical_iso_form_is_accepted` | the predicate (`is_valid_iso_timestamp`) is exercised directly by real assertions here, not grepped from source text |

## test/unit/application/error-kind-propagation.test.ts — 9 cases

| TS case | Rust test | Note |
|---|---|---|
| `errorKind propagation (commit 593cee9)` › agent:error event carries errorKind from adapter error event | `orchestrator::tests::error_kind_propagation::the_agent_error_event_carries_the_adapters_classification` | |
| `errorKind propagation (commit 593cee9)` › agent:error event omits errorKind when not present on adapter event | `orchestrator::tests::error_kind_propagation::the_agent_error_event_omits_the_kind_when_the_adapter_did_not_supply_one` | |
| `errorKind propagation (commit 593cee9)` › _handleRunFailure persists last_error with truncated message, kind, and timestamp | `orchestrator::tests::error_kind_propagation::the_failure_handler_persists_a_truncated_message_a_kind_and_a_timestamp` | |
| `errorKind propagation (commit 593cee9)` › _handleRunFailure uses classifyAdapterError when errorKind is not provided | `orchestrator::tests::error_kind_propagation::the_failure_handler_classifies_the_message_when_no_kind_is_supplied` | |
| `errorKind propagation (commit 593cee9)` › _handleRunFailure clears current_task on agent | `orchestrator::tests::error_kind_propagation::the_failure_handler_clears_current_task` | |
| `errorKind propagation (commit 593cee9)` › dispatch code clears last_error (line 882 verification) | `orchestrator::tests::error_kind_propagation::a_successful_dispatch_clears_a_stale_last_error` | |
| `errorKind propagation (commit 593cee9)` › reconcile PID crash calls _handleRunFailure with 3 args (backward compat) | `orchestrator::tests::error_kind_propagation::a_reconcile_pid_crash_records_the_crash_message_and_a_derived_kind` | Rust asserts the resulting record, not call arity — arity is compile-time checked in Rust, so there is nothing dynamic left to assert |
| `errorKind propagation (commit 593cee9)` › reconcile stall calls _handleRunFailure with 3 args (backward compat) | `orchestrator::tests::error_kind_propagation::a_reconcile_stall_records_the_stall_message` | same reasoning as the row above |
| `errorKind propagation (commit 593cee9)` › AgentLastError interface has correct shape and is exported from index | `orchestrator::tests::error_kind_propagation::the_last_error_record_has_the_contract_shape` | covers the field-shape/round-trip half for real; the "exported from index.ts" half is N/A — the Rust library surface is PARITY.md §13.1, not a JS barrel file |

## test/unit/application/event-bus.test.ts — 20 cases

| TS case | Rust test | Note |
|---|---|---|
| `EventBus > on / emit` › delivers events to matching handler | `events::tests::delivers_events_to_matching_handler` | |
| `EventBus > on / emit` › does not deliver events to non-matching handlers | `events::tests::does_not_deliver_events_to_non_matching_handlers` | |
| `EventBus > on / emit` › supports multiple handlers for same event type | `events::tests::supports_multiple_handlers_for_the_same_event_type` | |
| `EventBus > on / emit` › returns an unsubscribe function | `events::tests::returns_an_unsubscribe_handle` | |
| `EventBus > off` › removes a specific handler | `events::tests::off_removes_a_specific_handler` | |
| `EventBus > once` › handler fires only once then auto-unsubscribes | `events::tests::once_fires_only_once_then_auto_unsubscribes` | |
| `EventBus > once` › returns unsubscribe that prevents even the first call | `events::tests::once_can_be_unsubscribed_before_the_first_call` | |
| `EventBus > onAny` › receives all event types | `events::tests::on_any_receives_all_event_types` | |
| `EventBus > onAny` › unsubscribe removes from all event types | `events::tests::on_any_unsubscribe_removes_from_all_event_types` | |
| `EventBus > error handling` › does not break event chain when handler throws | `events::tests::a_panicking_handler_does_not_break_the_event_chain` | Rust catches a panic via `catch_unwind`, the closest equivalent to a caught exception |
| `EventBus > task:orphaned event` › delivers task:orphaned event to subscribers | `events::tests::delivers_task_orphaned_to_typed_and_wildcard_subscribers` | |
| `EventBus > task:orphaned event` › task:orphaned is included in onAny subscription | `events::tests::delivers_task_orphaned_to_typed_and_wildcard_subscribers` | same test, covers both subscriber kinds |
| `EventBus > clear` › removes all handlers | `events::tests::clear_removes_all_handlers` | |
| `EventBus > maxListeners` › defaults to 10 | `events::tests::max_listeners_defaults_to_10` | |
| `EventBus > maxListeners` › can be changed via setMaxListeners | `events::tests::max_listeners_can_be_changed` | |
| `EventBus > maxListeners` › warns when listener count exceeds maxListeners | `events::tests::warns_when_the_listener_count_exceeds_max_listeners` | |
| `EventBus > maxListeners` › warns only once per event type | `events::tests::warns_only_once_per_event_type` | |
| `EventBus > maxListeners` › does not warn when maxListeners is 0 (unlimited) | `events::tests::does_not_warn_when_max_listeners_is_zero` | |
| `EventBus > maxListeners` › listenerCount returns current count | `events::tests::listener_count_returns_the_current_count` | |
| `EventBus > maxListeners` › clear resets warned types so warnings fire again | `events::tests::clear_resets_warned_types_so_warnings_fire_again` | |

## test/unit/application/goal-service-autonomous.test.ts — 36 cases

| TS case | Rust test | Note |
|---|---|---|
| `GoalService autonomous mode side effects > create` › calls enableAutonomous when goal is created with assignee | `goal_service::tests::create_with_an_assignee_enables_autonomous` | |
| `GoalService autonomous mode side effects > create` › does not call enableAutonomous when goal is created without assignee | `goal_service::tests::create_without_an_assignee_leaves_autonomous_alone` | |
| `GoalService autonomous mode side effects > delete` › calls maybeDisableAutonomous when deleting goal with assignee | `goal_service::tests::delete_disables_autonomous_for_the_former_assignee` | |
| `GoalService autonomous mode side effects > delete` › does not call setAutonomous when deleted goal has no assignee | `goal_service::tests::delete_without_an_assignee_leaves_autonomous_alone` | |
| `GoalService autonomous mode side effects > updateStatus` › calls maybeDisableAutonomous when status transitions to achieved | `goal_service::tests::achieved_disables_autonomous` | |
| `GoalService autonomous mode side effects > updateStatus` › calls maybeDisableAutonomous when status transitions to abandoned | `goal_service::tests::abandoned_disables_autonomous` | |
| `GoalService autonomous mode side effects > updateStatus` › calls maybeDisableAutonomous when transitioning to paused | `goal_service::tests::paused_disables_autonomous` | |
| `GoalService autonomous mode side effects > updateStatus` › does not call setAutonomous on terminal status if goal has no assignee | `goal_service::tests::terminal_without_an_assignee_leaves_autonomous_alone` | |
| `GoalService autonomous mode side effects > update assignee change` › enables new assignee and maybe-disables old when assignee changes | `goal_service::tests::changing_the_assignee_enables_the_new_and_disables_the_old` | |
| `GoalService autonomous mode side effects > update assignee change` › enables new assignee only when old was undefined | `goal_service::tests::setting_an_assignee_where_there_was_none_only_enables` | |
| `GoalService autonomous mode side effects > update assignee change` › maybe-disables old assignee only when new assignee is empty string (cleared) | `goal_service::tests::clearing_the_assignee_only_disables_the_old` | |
| `GoalService autonomous mode side effects > update assignee change` › does not call setAutonomous when assignee is unchanged | `goal_service::tests::updating_other_fields_leaves_autonomous_alone` | |
| `GoalService autonomous mode side effects > no agentService (undefined)` › create with assignee is graceful no-op | `goal_service::tests::create_without_an_agent_service_is_a_no_op` | |
| `GoalService autonomous mode side effects > no agentService (undefined)` › delete is graceful no-op | `goal_service::tests::delete_without_an_agent_service_is_a_no_op` | |
| `GoalService autonomous mode side effects > no agentService (undefined)` › updateStatus to achieved is graceful no-op | `goal_service::tests::update_status_without_an_agent_service_is_a_no_op` | |
| `GoalService autonomous mode side effects > agentService.setAutonomous throws` › create: error is caught silently | `goal_service::tests::create_swallows_a_set_autonomous_failure` | |
| `GoalService autonomous mode side effects > agentService.setAutonomous throws` › delete: error is caught silently | `goal_service::tests::delete_swallows_a_set_autonomous_failure` | |
| `GoalService autonomous mode side effects > agentService.setAutonomous throws` › updateStatus: error is caught silently | `goal_service::tests::update_status_swallows_a_set_autonomous_failure` | |
| `GoalService autonomous mode side effects > achieved blocks when child tasks are pending` › throws GoalHasPendingTasksError when todo tasks exist for the goal | `goal_service::tests::achieved_blocks_on_a_todo_child_task` | |
| `GoalService autonomous mode side effects > achieved blocks when child tasks are pending` › throws when in_progress tasks exist for the goal | `goal_service::tests::achieved_blocks_on_an_in_progress_child_task` | |
| `GoalService autonomous mode side effects > achieved blocks when child tasks are pending` › throws when review tasks exist for the goal | `goal_service::tests::achieved_blocks_on_a_review_child_task` | |
| `GoalService autonomous mode side effects > achieved blocks when child tasks are pending` › allows achieved when all child tasks are in terminal states | `goal_service::tests::achieved_allows_terminal_child_tasks` | |
| `GoalService autonomous mode side effects > achieved blocks when child tasks are pending` › allows achieved when no child tasks exist | `goal_service::tests::achieved_allows_a_goal_with_no_child_tasks` | |
| `GoalService autonomous mode side effects > achieved blocks when child tasks are pending` › force: cancels todo/retrying/review tasks and allows achieved | `goal_service::tests::force_cancels_every_cancellable_child_task` | |
| `GoalService autonomous mode side effects > achieved blocks when child tasks are pending` › force: still blocks if in_progress tasks exist (live processes) | `goal_service::tests::force_still_blocks_on_in_progress_child_tasks` | |
| `GoalService autonomous mode side effects > achieved blocks when child tasks are pending` › does not check tasks for non-achieved transitions | `goal_service::tests::paused_does_not_run_the_achieved_guard` | |
| `GoalService autonomous mode side effects > achieved blocks when child tasks are pending` › does not check tasks for abandoned transition | `goal_service::tests::abandoned_does_not_run_the_achieved_guard` | |
| `GoalService autonomous mode side effects > achieved blocks when child tasks are pending` › error message includes task IDs and statuses | `goal_service::tests::the_pending_task_error_names_every_task_and_status` | |
| `GoalService autonomous mode side effects > achieved blocks when child tasks are pending` › excludes autonomous [auto] tasks from pending check | `goal_service::tests::the_achieved_guard_ignores_autonomous_tasks` | |
| `GoalService autonomous mode side effects > achieved blocks when child tasks are pending` › still blocks on non-autonomous in_progress tasks | `goal_service::tests::the_achieved_guard_still_blocks_on_a_user_task` | |
| `GoalService autonomous mode side effects > achieved blocks when child tasks are pending` › paused → achieved is allowed and calls maybeDisableAutonomous | `goal_service::tests::paused_to_achieved_is_allowed_and_disables_autonomous` | |
| `GoalService autonomous mode side effects > achieved blocks when child tasks are pending` › paused → achieved with force cancels pending non-auto tasks | `goal_service::tests::paused_to_achieved_with_force_cancels_pending_tasks` | |
| `GoalService autonomous mode side effects > achieved blocks when child tasks are pending` › skips validation when taskService is not provided | `goal_service::tests::the_achieved_guard_is_skipped_without_a_task_service` | |
| `GoalService autonomous mode side effects > multiple active goals — disable only when last removed` › does NOT disable autonomous when agent still has another active goal after deletion | `goal_service::tests::delete_keeps_autonomous_while_another_active_goal_remains` | |
| `GoalService autonomous mode side effects > multiple active goals — disable only when last removed` › disables autonomous when last active goal is deleted | `goal_service::tests::delete_of_the_last_active_goal_disables_autonomous` | |
| `GoalService autonomous mode side effects > multiple active goals — disable only when last removed` › does NOT disable when agent still has active goal after terminal status change | `goal_service::tests::terminal_keeps_autonomous_while_another_active_goal_remains` | |

## test/unit/application/message-service.test.ts — 15 cases

| TS case | Rust test | Note |
|---|---|---|
| `MessageService > send direct message` › creates message with correct fields | `message_service::tests::direct_send_creates_a_message_with_the_right_fields` | |
| `MessageService > send direct message` › throws if recipient not found | `message_service::tests::direct_send_rejects_an_unknown_recipient` | |
| `MessageService > send direct message` › throws if body is empty | `message_service::tests::a_whitespace_only_body_is_rejected` | |
| `MessageService > send broadcast` › creates one message per non-sender agent | `message_service::tests::broadcast_creates_one_message_per_non_sender_enabled_agent` | |
| `MessageService > send broadcast` › with team_id filters to team members | `message_service::tests::broadcast_with_a_team_id_restricts_to_team_members` | |
| `MessageService > send lead channel` › resolves to team lead | `message_service::tests::lead_channel_resolves_to_the_team_lead` | |
| `MessageService > send lead channel` › throws without team_id | `message_service::tests::lead_channel_requires_a_team_id` | |
| `MessageService > send lead channel` › throws if team not found | `message_service::tests::lead_channel_rejects_an_unknown_team` | |
| `MessageService > drainMailbox` › returns pending messages and marks them delivered | `message_service::tests::drain_mailbox_returns_pending_messages_and_marks_them_delivered` | |
| `MessageService > drainMailbox` › emits message:delivered events | `message_service::tests::drain_mailbox_emits_message_delivered` | |
| `MessageService > listForAgent` › returns messages sent to and from agent | `message_service::tests::list_for_agent_returns_messages_sent_to_and_from_the_agent` | |
| `MessageService > purgeExpired` › delegates to store | `message_service::tests::purge_expired_delegates_to_the_store` | |
| `MessageService > send emits message:sent event` › emits message:sent for direct message | `message_service::tests::direct_send_emits_message_sent` | |
| `MessageService > TTL validation` › throws if TTL is zero | `message_service::tests::a_zero_ttl_is_rejected` | |
| `MessageService > TTL validation` › throws if TTL exceeds max | `message_service::tests::a_ttl_above_the_maximum_is_rejected` | |

## test/unit/application/orchestrator-auto-review-approve.test.ts — 6 cases

| TS case | Rust test | Note |
|---|---|---|
| `autoApprove + review_criteria interaction` › transitions to done when review_criteria pass and autoApprove is set | `orchestrator::tests::auto_review_approve::passing_criteria_plus_auto_approve_reaches_done` | |
| `autoApprove + review_criteria interaction` › transitions to done when review_criteria FAIL but autoApprove is set | `orchestrator::tests::auto_review_approve::failing_criteria_are_overridden_by_auto_approve` | |
| `autoApprove + review_criteria interaction` › emits warning when criteria fail but autoApprove forces done | `orchestrator::tests::auto_review_approve::an_overridden_failure_is_reported_as_an_orchestrator_error` | |
| `autoApprove + review_criteria interaction` › stays in review when criteria fail and autoApprove is NOT set | `orchestrator::tests::auto_review_approve::failing_criteria_without_auto_approve_stay_in_review` | |
| `autoApprove + review_criteria interaction` › saves review_results on task even when autoApprove overrides | `orchestrator::tests::auto_review_approve::the_review_results_are_recorded_even_when_auto_approve_overrides_them` | |
| `autoApprove + review_criteria interaction` › transitions review → done directly when autoApprove is set and no review_criteria | `orchestrator::tests::auto_review_approve::auto_approve_with_no_criteria_promotes_directly_and_runs_nothing` | |

## test/unit/application/orchestrator-bugfixes-2026-03.test.ts — 8 cases

| TS case | Rust test | Note |
|---|---|---|
| `P2: Proof detection — tool_call file path extraction` › extracts file paths from Claude tool_use Write events into proof.files_changed | `orchestrator::tests::bugfixes_2026_03::write_shaped_tool_calls_become_proof_files_changed` | |
| `P2: Proof detection — tool_call file path extraction` › emits agent:file_changed events for tool_call-derived file paths | `orchestrator::tests::bugfixes_2026_03::a_tool_call_derived_path_is_also_announced_on_the_bus` | |
| `P2: Proof detection — tool_call file path extraction` › does not extract file paths from non-write tool_call events | `orchestrator::tests::bugfixes_2026_03::a_read_or_bash_tool_call_contributes_no_files` | |
| `P3: Orphaned preparing runs — cleanup at startup` › finalizes orphaned preparing runs as cancelled during startup | `orchestrator::tests::bugfixes_2026_03::startup_finalizes_an_orphaned_preparing_run_as_cancelled` | |
| `P3: Orphaned preparing runs — cleanup at startup` › does not finalize runs that are actively running (in state.running) | `orchestrator::tests::bugfixes_2026_03::startup_leaves_a_preparing_run_that_belongs_to_a_live_entry_alone` | |
| `P3: Orphaned preparing runs — cleanup at startup` › handles multiple orphaned preparing runs in parallel | `orchestrator::tests::bugfixes_2026_03::startup_finalizes_every_orphaned_preparing_run` | |
| `Bonus: Per-agent stall_timeout_ms in reconcile` › uses per-agent stall timeout instead of global when agent has custom value | `orchestrator::tests::bugfixes_2026_03::a_generous_per_agent_stall_timeout_overrides_the_global_one` | |
| `Bonus: Per-agent stall_timeout_ms in reconcile` › falls back to global stall timeout when agent has no custom value | `orchestrator::tests::bugfixes_2026_03::an_agent_without_its_own_stall_timeout_falls_back_to_the_global_one` | |

## test/unit/application/orchestrator-dispatch-worktree.test.ts — 7 cases

| TS case | Rust test | Note |
|---|---|---|
| `dispatchTask — worktree stale-overwrite fix` › saves in_progress status (not stale todo) when worktree branch is present | `orchestrator::tests::dispatch_worktree::the_branch_save_carries_in_progress_not_the_stale_todo` | |
| `dispatchTask — worktree stale-overwrite fix` › sets proof.branch to the worktree branch name | `orchestrator::tests::dispatch_worktree::the_branch_name_lands_on_the_proof` | |
| `dispatchTask — worktree stale-overwrite fix` › sets workspace to the worktree path | `orchestrator::tests::dispatch_worktree::the_worktree_path_lands_on_the_task` | |
| `dispatchTask — worktree stale-overwrite fix` › saves correct attempts counter (1) after first dispatch | `orchestrator::tests::dispatch_worktree::the_branch_save_carries_the_incremented_attempt_count` | |
| `dispatchTask — worktree stale-overwrite fix` › does not save proof.branch for non-worktree tasks (no branch from workspace) | `orchestrator::tests::dispatch_worktree::a_non_worktree_dispatch_writes_no_branch_at_all` | |
| `dispatchTask — worktree stale-overwrite fix` › preserves existing proof.files_changed when saving branch | `orchestrator::tests::dispatch_worktree::an_existing_files_changed_list_survives_the_branch_save` | |
| `dispatchTask — graceful null on freshTask re-read` › does not throw if taskStore.get returns null for fresh re-read after dispatch | `orchestrator::tests::dispatch_worktree::a_task_deleted_mid_dispatch_does_not_break_the_tick` | |

## test/unit/application/orchestrator-endurance.test.ts — 17 cases

| TS case | Rust test | Note |
|---|---|---|
| `Orchestrator Endurance > Memory bounds` › state.running and state.claimed stay bounded after 1000 task dispatches and completions | `orchestrator::tests::endurance::a_thousand_stale_running_entries_are_all_cleaned_in_one_reconcile` | the `claimed`-bound half is additionally covered by `orchestrator::tests::endurance::startup_recovery_clears_every_claim` |
| `Orchestrator Endurance > Retry queue saturation` › caps retry_queue at 100 entries, dropping oldest when full | `orchestrator::tests::endurance::the_retry_queue_is_capped_at_a_hundred_entries_dropping_the_oldest` | |
| `Orchestrator Endurance > Retry queue saturation` › deduplicates retry entries by task_id | `orchestrator::tests::endurance::the_retry_queue_is_deduplicated_by_task_id` | |
| `Orchestrator Endurance > Stats accumulation` › total_tokens stays finite after 500 increments | `orchestrator::tests::endurance::five_hundred_token_increments_stay_exact` | |
| `Orchestrator Endurance > Stats accumulation` › stats remain correct with zero-token completions | `orchestrator::tests::endurance::zero_token_completions_leave_the_totals_at_zero` | |
| `Orchestrator Endurance > Event bus listener leak` › subscribe/unsubscribe 1000 times leaves listenerCount at 0 | `events::tests::a_thousand_subscribe_unsubscribe_cycles_leave_no_listeners` | cross-module: an `EventBus` property, ported into the `events` test module rather than duplicated under `orchestrator::tests::endurance` |
| `Orchestrator Endurance > Event bus listener leak` › wildcard subscribe/unsubscribe 1000 times does not leak | `events::tests::a_thousand_wildcard_subscribe_unsubscribe_cycles_do_not_leak` | cross-module, same reason |
| `Orchestrator Endurance > Event bus listener leak` › once() auto-unsubscribes and does not accumulate | `events::tests::once_does_not_accumulate_listeners` | cross-module, same reason |
| `Orchestrator Endurance > State save debounce under pressure` › 100 saveStateLazy calls within 100ms result in only 1 actual save | `orchestrator::tests::endurance::a_hundred_lazy_saves_inside_one_window_write_once` | |
| `Orchestrator Endurance > State save debounce under pressure` › flushStateLazy forces immediate write | `orchestrator::tests::endurance::a_flush_writes_immediately_and_cancels_the_pending_timer` | |
| `Orchestrator Endurance > Concurrent tick safety` › 10 parallel withStateLock calls serialize without data corruption | `orchestrator::tests::mutex::there_are_no_lost_updates_under_a_read_modify_write_race` | cross-module: concurrency safety of the state lock is tested against the real orchestrator lock in the dedicated `mutex` suite |
| `Orchestrator Endurance > Concurrent tick safety` › mutex does not deadlock on 100 rapid lock acquisitions | `orchestrator::tests::mutex::high_contention_does_not_deadlock` | cross-module, same reason |
| `Orchestrator Endurance > JSONL file growth` › appendJsonl 10000 records and readJsonlTail returns correct last N | *(N/A)* | N/A here — `fs_utils` is `orch-infra`'s; covered by its own suite. |
| `Orchestrator Endurance > JSONL file growth` › readJsonlTail handles file with exactly 1 record | *(N/A)* | N/A here — `fs_utils` is `orch-infra`'s; covered by its own suite. |
| `Orchestrator Endurance > Stale process cleanup` › reconcile cleans up running entry with dead PID in 1 tick | `orchestrator::tests::endurance::one_reconcile_is_enough_to_clean_up_a_dead_pid` | |
| `Orchestrator Endurance > Graceful shutdown under load` › stop() cleans up 5 running tasks, releases lock, clears timers | `orchestrator::tests::endurance::stop_winds_down_five_running_agents_and_releases_the_lock` | |
| `Orchestrator Endurance > Graceful shutdown under load` › stop() is idempotent — second call is a no-op | `orchestrator::tests::endurance::a_second_stop_writes_nothing` | |

## test/unit/application/orchestrator-force-review.test.ts — 3 cases

| TS case | Rust test | Note |
|---|---|---|
| `forceTaskToReview clears agent.current_task` › clears current_task when merge conflict triggers forceTaskToReview | `orchestrator::tests::force_review::a_merge_conflict_clears_current_task` | |
| `forceTaskToReview clears agent.current_task` › clears current_task when merge throws an error | `orchestrator::tests::force_review::a_merge_that_errors_outright_clears_current_task` | |
| `forceTaskToReview clears agent.current_task` › task status is review after forceTaskToReview | `orchestrator::tests::force_review::the_task_lands_in_review_with_the_conflict_named_in_the_summary` | |

## test/unit/application/orchestrator-mutex.test.ts — 4 cases

| TS case | Rust test | Note |
|---|---|---|
| `Orchestrator state mutex` › serializes concurrent mutations | `orchestrator::tests::mutex::critical_sections_serialize_regardless_of_their_duration` | |
| `Orchestrator state mutex` › releases lock even if fn throws | `orchestrator::tests::mutex::the_lock_is_released_even_when_the_critical_section_fails` | |
| `Orchestrator state mutex` › prevents read-after-write race on shared state | `orchestrator::tests::mutex::there_are_no_lost_updates_under_a_read_modify_write_race` | |
| `Orchestrator state mutex` › handles high contention without deadlock | `orchestrator::tests::mutex::high_contention_does_not_deadlock` | |

## test/unit/application/orchestrator-parallel-race.test.ts — 4 cases

| TS case | Rust test | Note |
|---|---|---|
| `Issue #8: parallel run race condition — activeCollectors guard` › reconcile skips PID check for tasks with active collector | `orchestrator::tests::parallel_race::reconcile_skips_the_pid_check_for_a_task_with_an_active_collector` | |
| `Issue #8: parallel run race condition — activeCollectors guard` › reconcile correctly detects crash when collector is NOT active | `orchestrator::tests::parallel_race::reconcile_does_detect_the_crash_when_no_collector_is_active` | |
| `Issue #8: parallel run race condition — activeCollectors guard` › activeCollectors is cleaned up after collectEvents completes | `orchestrator::tests::parallel_race::the_collector_registration_is_released_once_collection_finishes` | |
| `Issue #8: parallel run race condition — activeCollectors guard` › collectEvents finalizes run when entry was already cleaned by reconcile | `orchestrator::tests::parallel_race::a_stream_error_finalizes_the_run_even_when_the_entry_is_already_gone` | |

## test/unit/application/orchestrator-perf-opts.test.ts — 12 cases

| TS case | Rust test | Note |
|---|---|---|
| `claimed Set operations` › DEFAULT_STATE.claimed is a Set | `orchestrator::tests::perf_opts::the_default_states_claimed_is_an_empty_set` | |
| `claimed Set operations` › claimed.add() adds an ID | `orchestrator::tests::perf_opts::claimed_supports_insert_remove_and_membership` | |
| `claimed Set operations` › claimed.delete() removes an ID | `orchestrator::tests::perf_opts::claimed_supports_insert_remove_and_membership` | same test |
| `claimed Set operations` › claimed.has() is O(1) lookup (Set semantics) | `orchestrator::tests::perf_opts::claimed_supports_insert_remove_and_membership` | same test; Rust asserts `IndexSet` membership/no-dup semantics rather than measuring algorithmic complexity |
| `claimed Set operations` › claimed.delete() on non-existent ID is a no-op (no error) | `orchestrator::tests::perf_opts::claimed_supports_insert_remove_and_membership` | same test |
| `claimed Set serialization round-trip` › Array.from(claimed) serializes Set to array for JSON | `orchestrator::tests::perf_opts::claimed_serializes_as_an_array_and_round_trips` | |
| `claimed Set serialization round-trip` › new Set(array) deserializes array back to Set | `orchestrator::tests::perf_opts::claimed_serializes_as_an_array_and_round_trips` | same test |
| `claimed Set serialization round-trip` › round-trip: claimed Set → array → Set preserves all IDs | `orchestrator::tests::perf_opts::claimed_serializes_as_an_array_and_round_trips` | same test |
| `claimed Set serialization round-trip` › null claimed deserializes to empty Set (corrupt state recovery) | `orchestrator::tests::perf_opts::a_corrupt_claimed_recovers_as_an_empty_set` | |
| `claimed Set serialization round-trip` › non-array claimed deserializes to empty Set (corrupt state recovery) | `orchestrator::tests::perf_opts::a_corrupt_claimed_recovers_as_an_empty_set` | same test — loops over `null`, `"not-an-array"`, an object, and a number |
| `reconcile parallel task reads via Promise.all` › taskStore.get is called for each running entry (parallel pre-fetch) | `orchestrator::tests::perf_opts::reconcile_reads_every_running_task` | |
| `reconcile parallel task reads via Promise.all` › Promise.all issues all reads before processing any result | `orchestrator::tests::perf_opts::reconcile_issues_all_of_its_pre_fetches_before_processing_any_of_them` | |

## test/unit/application/orchestrator-resilience.test.ts — 15 cases

| TS case | Rust test | Note |
|---|---|---|
| `Orchestrator resilience > Fix 1: consecutiveTickFailures counter` › resets counter to 0 on successful tick | `orchestrator::tests::resilience::a_successful_tick_leaves_the_failure_counter_at_zero` | |
| `Orchestrator resilience > Fix 1: consecutiveTickFailures counter` › increments counter on tick failure | `orchestrator::tests::resilience::a_failing_tick_increments_the_failure_counter` | |
| `Orchestrator resilience > Fix 1: consecutiveTickFailures counter` › emits orchestrator:error with fatal:true after 5 consecutive failures | `orchestrator::tests::resilience::five_consecutive_failures_are_fatal_and_shut_the_orchestrator_down` | |
| `Orchestrator resilience > Fix 1: consecutiveTickFailures counter` › emits orchestrator:shutdown after 5 consecutive failures | `orchestrator::tests::resilience::five_consecutive_failures_are_fatal_and_shut_the_orchestrator_down` | same test |
| `Orchestrator resilience > Fix 1: consecutiveTickFailures counter` › calls stop() after 5 consecutive failures | `orchestrator::tests::resilience::five_consecutive_failures_are_fatal_and_shut_the_orchestrator_down` | same test; also asserts `is_owner()` false and `release_count() == 1` |
| `Orchestrator resilience > Fix 2: Agent stats update emits orchestrator:error` › emits orchestrator:error when agentService.updateStats fails in _handleRunSuccess | `orchestrator::tests::resilience::a_failing_agent_stats_update_is_reported_and_does_not_abort_completion` | |
| `Orchestrator resilience > Fix 3: SIGINT/SIGTERM signal handlers` › registerSignalHandlers installs SIGINT and SIGTERM listeners | `orchestrator::tests::resilience::the_watch_loop_installs_both_signal_arms` | Rust uses `tokio::select!` arms (`SignalArms`), not registered OS signal handlers — this asserts both arms are installed instead of `signalHandlers.length === 2` |
| `Orchestrator resilience > Fix 3: SIGINT/SIGTERM signal handlers` › removeSignalHandlers clears signal listeners on stop() | `orchestrator::tests::resilience::a_disabled_signal_arm_never_fires` | Rust uses `select!` arms — a "disabled" arm stays permanently pending rather than removed, the behavioral equivalent asserted here |
| `Orchestrator resilience > Fix 3: SIGINT/SIGTERM signal handlers` › SIGINT handler emits orchestrator:shutdown event | `orchestrator::tests::resilience::sigint_reports_the_reason_and_stops` | Rust dispatches through `handle_signal("SIGINT")` rather than an OS-installed callback; the observable contract (shutdown event + reason) matches |
| `Orchestrator resilience > Fix 3: SIGINT/SIGTERM signal handlers` › SIGTERM handler emits orchestrator:shutdown and calls stop() | `orchestrator::tests::resilience::sigterm_reports_the_reason_and_stops` | same `select!`-arms substitution as above |
| `Orchestrator resilience > Fix 4: structured error events instead of console.error` › dispatch failure emits orchestrator:error | `orchestrator::tests::resilience::a_dispatch_failure_is_reported_as_a_non_fatal_orchestrator_error` | |
| `Orchestrator resilience > Fix 4: structured error events instead of console.error` › adapter execution error emits orchestrator:error | `orchestrator::tests::resilience::an_adapter_whose_stream_throws_fails_the_run_rather_than_the_tick` | |
| `Orchestrator resilience > Fix 4: structured error events instead of console.error` › debounced state save error emits orchestrator:error | `orchestrator::tests::resilience::a_failing_debounced_state_save_is_reported` | |
| `Orchestrator resilience > Event types in events.ts and event-bus.ts` › EventBus.onAny subscribes to orchestrator:error | `orchestrator::tests::resilience::a_wildcard_subscriber_sees_orchestrator_error_and_shutdown` | asserts real wildcard-subscription behaviour directly, not a source-text grep |
| `Orchestrator resilience > Event types in events.ts and event-bus.ts` › EventBus.onAny subscribes to orchestrator:shutdown | `orchestrator::tests::resilience::a_wildcard_subscriber_sees_orchestrator_error_and_shutdown` | same test |

## test/unit/application/orchestrator-retry-race.test.ts — 5 cases

| TS case | Rust test | Note |
|---|---|---|
| `Retry dispatch race condition guards` › dispatchTask silently skips a task that is already done | `orchestrator::tests::retry_race::dispatch_silently_skips_a_task_that_is_already_done` | |
| `Retry dispatch race condition guards` › retry queue skips tasks that succeeded while waiting in queue | `orchestrator::tests::retry_race::the_retry_queue_skips_a_task_that_succeeded_while_it_waited` | |
| `Retry dispatch race condition guards` › _handleRunFailure skips if running entry was already cleaned up by success handler | `orchestrator::tests::retry_race::the_failure_handler_returns_early_when_the_entry_is_already_gone` | |
| `Retry dispatch race condition guards` › dispatchTask skips a task in cancelled status | `orchestrator::tests::retry_race::dispatch_silently_skips_a_cancelled_task` | |
| `Retry dispatch race condition guards` › dispatchTask skips a task in failed status | `orchestrator::tests::retry_race::dispatch_silently_skips_a_failed_task` | |

## test/unit/application/orchestrator-skills.test.ts — 13 cases

| TS case | Rust test | Note |
|---|---|---|
| `Orchestrator skill injection > 1. Library skill injection into systemPrompt` › appends skill block to systemPrompt when agent has a library skill | `orchestrator::tests::skills::a_library_skill_is_appended_to_the_system_prompt` | |
| `Orchestrator skill injection > 1. Library skill injection into systemPrompt` › skillLoader.loadSkills is called with the agent skill list | `orchestrator::tests::skills::the_loader_is_called_with_the_agents_skill_list_in_order` | |
| `Orchestrator skill injection > 1. Library skill injection into systemPrompt` › skill content is separated from the original systemPrompt by a blank line | `orchestrator::tests::skills::the_skill_block_is_separated_from_the_base_prompt_by_a_blank_line` | |
| `Orchestrator skill injection > 2. MCP skills are skipped` › does not append any skill block when agent only has MCP skills | `orchestrator::tests::skills::an_mcp_only_skill_list_appends_nothing` | |
| `Orchestrator skill injection > 2. MCP skills are skipped` › skillLoader.loadSkills still receives MCP skills but returns empty string | `orchestrator::tests::skills::the_loader_still_receives_the_mcp_names_and_resolves_nothing` | |
| `Orchestrator skill injection > 3. Mixed library and MCP skills` › appends only library skill content when agent has both library and MCP skills | `orchestrator::tests::skills::a_mixed_list_contributes_only_its_library_skills` | |
| `Orchestrator skill injection > 3. Mixed library and MCP skills` › passes the full mixed skill list to loadSkills (loader decides what to skip) | `orchestrator::tests::skills::a_mixed_list_is_passed_to_the_loader_whole` | |
| `Orchestrator skill injection > 4. No skills — systemPrompt unchanged` › does not modify systemPrompt when agent has an empty skills array | `orchestrator::tests::skills::an_empty_skill_list_leaves_the_system_prompt_untouched_and_calls_nothing` | |
| `Orchestrator skill injection > 4. No skills — systemPrompt unchanged` › does not modify systemPrompt when agent has no skills field (undefined) | `orchestrator::tests::skills::an_absent_skills_field_leaves_the_system_prompt_untouched` | |
| `Orchestrator skill injection > 4. No skills — systemPrompt unchanged` › does not call loadSkills when skillLoader is not provided in deps | `orchestrator::tests::skills::no_skill_loader_at_all_means_no_skill_lookup` | |
| `Orchestrator skill injection > 5. Legacy template mode` › appends skill block to combined prompt when config.prompt.template is set | `orchestrator::tests::skills::the_legacy_combined_prompt_gets_the_skill_block_instead` | |
| `Orchestrator skill injection > 5. Legacy template mode` › legacy combined prompt retains original content before the skill block | `orchestrator::tests::skills::the_legacy_prompt_keeps_its_own_content_before_the_block` | |
| `Orchestrator skill injection > 5. Legacy template mode` › does not append skills to legacy prompt when all skills are MCP | `orchestrator::tests::skills::an_all_mcp_list_appends_nothing_to_the_legacy_prompt_either` | |

## test/unit/application/orchestrator.test.ts — 48 cases

| TS case | Rust test | Note |
|---|---|---|
| `Orchestrator > constructor` › creates an orchestrator in non-owner mode | `orchestrator::tests::core::a_new_orchestrator_is_not_the_lock_owner` | |
| `Orchestrator > requireOwnership` › runTask throws LockConflictError if lock is held by another process | `orchestrator::tests::core::run_task_reports_a_lock_conflict_with_the_holders_pid` | |
| `Orchestrator > requireOwnership` › runAll throws LockConflictError if lock is held by another process | `orchestrator::tests::core::run_all_reports_a_lock_conflict_with_the_holders_pid` | |
| `Orchestrator > requireOwnership` › runTask acquires and releases lock even on error | `orchestrator::tests::core::run_task_releases_the_lock_even_when_the_task_does_not_exist` | |
| `Orchestrator > requireOwnership` › runTask does not reactive-dispatch other todo tasks after the requested task completes | `orchestrator::tests::core::run_task_does_not_reactive_dispatch_the_other_ready_tasks` | |
| `Orchestrator > requireOwnership` › cancelTask auto-acquires lock when not owned | `orchestrator::tests::core::cancel_task_takes_a_temporary_lock_when_it_does_not_own_one` | |
| `Orchestrator > requireOwnership` › forceStopAgent auto-acquires lock when not owned | `orchestrator::tests::core::force_stop_agent_takes_a_temporary_lock_when_it_does_not_own_one` | |
| `Orchestrator > startWatch + stop` › acquires lock and starts tick loop | `orchestrator::tests::core::start_watch_takes_the_lock_and_records_the_holder` | |
| `Orchestrator > startWatch + stop` › stop releases lock and clears state | `orchestrator::tests::core::stop_releases_the_lock_and_clears_the_holder` | |
| `Orchestrator > startWatch + stop` › stop is idempotent (double stop does not throw) | `orchestrator::tests::core::stop_is_idempotent` | |
| `Orchestrator > startWatch + stop` › stop kills running agents and marks tasks for retry | `orchestrator::tests::core::stop_kills_running_agents_and_leaves_their_tasks_retryable` | |
| `Orchestrator > tick (reconcile + dispatch)` › emits orchestrator:tick event with running and queued counts | `orchestrator::tests::core::a_tick_emits_the_running_and_queued_counts` | |
| `Orchestrator > tick (reconcile + dispatch)` › reconcile detects orphaned in_progress tasks and marks them failed | `orchestrator::tests::core::reconcile_fails_orphaned_in_progress_tasks_and_says_so` | |
| `Orchestrator > tick (reconcile + dispatch)` › reconcile fixes stale agent statuses | `orchestrator::tests::core::reconcile_resets_an_agent_stuck_in_running` | |
| `Orchestrator > tick (reconcile + dispatch)` › reconcile detects dead PID — running entry cleaned up on terminal task | `orchestrator::tests::core::reconcile_drops_a_running_entry_whose_task_is_already_terminal` | verified against the TS body: a `failed` task with dead PID — matches exactly |
| `Orchestrator > tick (reconcile + dispatch)` › reconcile emits stall_detected and kills stalled process | `orchestrator::tests::core::reconcile_drops_a_stale_entry_for_a_done_task_without_checking_the_clock` | TS title is misleading — the TS body builds a `done` task with `isAlive=true` and only asserts the stale entry is cleaned up; it never asserts a stall event. This Rust test faithfully ports what the TS body does, not its title. |
| `Orchestrator > tick (reconcile + dispatch)` › reconcile handles dead PID via _handleRunFailure without deadlock | `orchestrator::tests::core::reconcile_handles_a_dead_pid_through_the_failure_path_without_deadlocking`, `orchestrator::tests::core::start_watch_cancels_a_dead_pids_task_rather_than_retrying_it` | the TS body asserts two things in one test (no deadlock, and a restart cancels rather than retries the dead-PID task); the Rust port splits these into two focused tests, both cited |
| `Orchestrator > tick (reconcile + dispatch)` › handleRunFailure updates task status and enqueues retry (called directly) | `orchestrator::tests::core::the_failure_handler_updates_the_task_and_enqueues_a_retry` | |
| `Orchestrator > tick (reconcile + dispatch)` › reconcile processes retry queue when due | `orchestrator::tests::core::reconcile_drains_a_retry_that_has_come_due` | |
| `Orchestrator > dispatchAll` › dispatches todo tasks to idle agents | `orchestrator::tests::core::dispatch_sends_a_todo_task_to_an_idle_agent` | |
| `Orchestrator > dispatchAll` › does not dispatch when max_concurrent_agents reached | `orchestrator::tests::core::dispatch_stops_at_the_concurrency_cap` | |
| `Orchestrator > dispatchAll` › does not dispatch blocked tasks | `orchestrator::tests::core::dispatch_skips_a_task_blocked_on_an_unfinished_dependency` | |
| `Orchestrator > dispatchAll` › emits scope overlap warning for overlapping tasks | `orchestrator::tests::core::dispatch_reports_and_skips_a_scope_overlap_with_running_work` | |
| `Orchestrator > dispatchAll` › does not dispatch task with scope overlapping a running task | `orchestrator::tests::core::dispatch_reports_and_skips_a_scope_overlap_with_running_work` | same test as the row above |
| `Orchestrator > dispatchAll` › dispatches tasks without scope normally even when other tasks have scope | `orchestrator::tests::core::a_task_without_a_scope_dispatches_alongside_scoped_work` | |
| `Orchestrator > dispatchAll` › blocked candidate does not count as peer for subsequent candidates | `orchestrator::tests::core::a_blocked_candidate_is_not_itself_an_obstacle_to_later_candidates` | |
| `Orchestrator > dispatch ordering (priority + goals)` › dispatches higher-priority tasks first (P1 before P3) | `orchestrator::tests::core::a_lower_priority_number_goes_first` | |
| `Orchestrator > dispatch ordering (priority + goals)` › dispatches goal-linked tasks before unlinked at same priority | `orchestrator::tests::core::a_goal_linked_task_beats_an_unlinked_one_at_the_same_priority` | |
| `Orchestrator > dispatch ordering (priority + goals)` › priority trumps goal-linking (P1 no goal beats P2 with goal) | `orchestrator::tests::core::priority_outranks_goal_linkage` | |
| `Orchestrator > dispatch ordering (priority + goals)` › recency breaks ties within same priority and goal status | `orchestrator::tests::core::recency_breaks_a_priority_and_goal_tie` | |
| `Orchestrator > cancelTask` › kills running agent and marks task cancelled | `orchestrator::tests::core::cancel_task_kills_the_agent_and_marks_the_task_cancelled` | |
| `Orchestrator > cancelTask` › calls abort() on AbortController before deleting it | `orchestrator::tests::core::cancel_task_cancels_the_run_token_before_dropping_it` | |
| `Orchestrator > cancelTask` › clears retry queue entries for cancelled task | `orchestrator::tests::core::cancel_task_clears_the_tasks_retry_queue_entry` | |
| `Orchestrator > forceStopAgent` › kills all running tasks for the agent | `orchestrator::tests::core::force_stop_agent_kills_every_run_the_agent_owns` | |
| `Orchestrator > collectEvents` › processes adapter events and updates state | `orchestrator::tests::core::collecting_events_logs_them_and_forwards_them_to_the_bus` | |
| `Orchestrator > consecutive tick failures` › emits orchestrator:shutdown after maxConsecutiveTickFailures | `orchestrator::tests::resilience::five_consecutive_failures_are_fatal_and_shut_the_orchestrator_down` | cross-module: this exact scenario was consolidated into the dedicated `resilience` suite rather than duplicated in `core` |
| `Orchestrator > state recovery` › loads persisted state on startWatch | `orchestrator::tests::core::start_watch_preserves_the_persisted_stats` | |
| `Orchestrator > state recovery` › cleans up stale running entries for terminal tasks on reconcile | `orchestrator::tests::core::reconcile_cleans_up_a_stale_entry_for_a_done_task` | |
| `Orchestrator > BUG#1 + BUG#2: current_task cleared and total_runtime_ms accumulated` › _handleRunSuccess clears agent.current_task after completion | `orchestrator::tests::core::the_success_handler_clears_current_task_and_accumulates_runtime` | |
| `Orchestrator > BUG#1 + BUG#2: current_task cleared and total_runtime_ms accumulated` › _handleRunFailure clears agent.current_task after failure | `orchestrator::tests::core::the_failure_handler_clears_current_task_and_accumulates_runtime` | |
| `Orchestrator > BUG#1 + BUG#2: current_task cleared and total_runtime_ms accumulated` › _handleRunSuccess accumulates total_runtime_ms in agent stats | `orchestrator::tests::core::the_success_handler_clears_current_task_and_accumulates_runtime` | same test as two rows above |
| `Orchestrator > BUG#1 + BUG#2: current_task cleared and total_runtime_ms accumulated` › _handleRunFailure accumulates total_runtime_ms in agent stats | `orchestrator::tests::core::the_failure_handler_clears_current_task_and_accumulates_runtime` | same test as its sibling row above |
| `Orchestrator > BUG#1 + BUG#2: current_task cleared and total_runtime_ms accumulated` › total_runtime_ms is non-zero after success run (Speedster badge scenario) | `orchestrator::tests::core::a_successful_run_always_records_some_runtime` | |
| `Orchestrator > immediate dispatch on task:created` › task:created triggers immediateDispatch after debounce | `orchestrator::tests::core::task_created_triggers_a_dispatch_after_the_debounce` | |
| `Orchestrator > immediate dispatch on task:created` › debounce batches multiple task:created events into one dispatch | `orchestrator::tests::core::several_task_created_events_coalesce_into_one_dispatch` | |
| `Orchestrator > immediate dispatch on task:created` › stop clears task:created subscription and debounce timer | `orchestrator::tests::core::stop_clears_the_pending_immediate_dispatch` | |
| `Orchestrator > immediate dispatch on task:created` › does not dispatch if shuttingDown | `orchestrator::tests::core::nothing_is_dispatched_once_shutdown_has_begun` | |
| `Orchestrator > immediate dispatch on task:created` › does not run immediateDispatch while tick is in progress | `orchestrator::tests::core::an_immediate_dispatch_defers_while_a_tick_is_in_flight_then_runs` | |

## test/unit/application/review-runner.test.ts — 13 cases

| TS case | Rust test | Note |
|---|---|---|
| `ReviewRunner > runAll` › should run all criteria and return results | `review_runner::tests::runs_all_criteria_and_returns_results_in_staged_order` | |
| `ReviewRunner > runAll` › should sort criteria: typecheck → lint → test_pass | `review_runner::tests::sorts_criteria_typecheck_then_lint_then_test_pass` | |
| `ReviewRunner > runAll` › should stop on first failure in fail-fast mode (default) | `review_runner::tests::stops_at_the_first_failure_in_fail_fast_mode` | |
| `ReviewRunner > runAll` › should run all criteria when fail_fast is false | `review_runner::tests::runs_every_criterion_when_fail_fast_is_off` | |
| `ReviewRunner > runAll` › should mark failed criteria correctly | `review_runner::tests::marks_each_criterion_by_its_own_outcome` | |
| `ReviewRunner > runAll` › should pass cwd and timeout to execFile | `review_runner::tests::passes_the_cwd_and_timeout_through_to_the_child` | |
| `ReviewRunner > runAll` › should truncate output to 2000 chars | `review_runner::tests::truncates_the_output_to_2000_chars` | |
| `ReviewRunner > allPassed` › should return true when all results passed | `review_runner::tests::all_passed_is_true_only_for_a_non_empty_all_passing_set` | |
| `ReviewRunner > allPassed` › should return false when any result failed | `review_runner::tests::all_passed_is_true_only_for_a_non_empty_all_passing_set` | same test |
| `ReviewRunner > allPassed` › should return false for empty results | `review_runner::tests::all_passed_is_true_only_for_a_non_empty_all_passing_set` | same test, empty-slice case |
| `ReviewRunner > formatReport` › should format passing results with checkmarks | `review_runner::tests::format_report_renders_a_passing_result` | |
| `ReviewRunner > formatReport` › should format failing results with X marks | `review_runner::tests::format_report_renders_a_failing_result` | |
| `ReviewRunner > formatReport` › should format mixed results | `review_runner::tests::format_report_joins_mixed_results_with_a_blank_line` | |

## test/unit/application/run-service.test.ts — 7 cases

| TS case | Rust test | Note |
|---|---|---|
| `RunService.getLastFailedRunContext` › should return null when no failed runs exist | `run_service::tests::returns_none_when_no_failed_runs_exist` | |
| `RunService.getLastFailedRunContext` › should return null when no runs exist for the task | `run_service::tests::returns_none_when_no_runs_exist_for_the_task` | |
| `RunService.getLastFailedRunContext` › should return error and output from the most recent failed run | `run_service::tests::returns_the_error_and_output_of_the_most_recent_failed_run` | |
| `RunService.getLastFailedRunContext` › should return "Unknown error" when run has no error field | `run_service::tests::reports_unknown_error_when_the_run_has_no_error_field` | |
| `RunService.getLastFailedRunContext` › should return all output lines without truncation | `run_service::tests::the_output_is_capped_at_the_last_fifty_events` | TS title is misleading — the TS body seeds 100 events and asserts the output is the last 50 ("to prevent OOM"), which is exactly what this Rust test pins |
| `RunService.getLastFailedRunContext` › should handle missing events file gracefully | `run_service::tests::a_failing_events_read_degrades_to_empty_output` | |
| `RunService.getLastFailedRunContext` › should only include agent_output and error events in output | `run_service::tests::only_agent_output_and_error_events_reach_the_output` | |

## test/unit/application/task-service-assignee.test.ts — 11 cases

| TS case | Rust test | Note |
|---|---|---|
| `Issue #7: assignee name→ID resolution > TaskService.create — resolveAssignee` › resolves agent name to ID at creation time | `task_service::tests::an_assignee_resolves_by_id_or_by_name` | |
| `Issue #7: assignee name→ID resolution > TaskService.create — resolveAssignee` › accepts valid agent ID as-is | `task_service::tests::an_assignee_resolves_by_id_or_by_name` | same test, by-id half |
| `Issue #7: assignee name→ID resolution > TaskService.create — resolveAssignee` › throws error for unknown agent name | `task_service::tests::an_unresolvable_assignee_reports_the_right_message_for_its_shape` | |
| `Issue #7: assignee name→ID resolution > TaskService.create — resolveAssignee` › throws error for unknown agt_ ID | `task_service::tests::an_unresolvable_assignee_reports_the_right_message_for_its_shape` | same test, id-shaped half |
| `Issue #7: assignee name→ID resolution > TaskService.create — resolveAssignee` › works without agentStore (backward compatibility) | *(N/A)* | N/A — Rust's `TaskService::new` takes a mandatory `Arc<dyn AgentStore>` (not `Option`), so there is no backward-compatible no-store mode to port; assignee resolution always validates against the store. |
| `Issue #7: assignee name→ID resolution > TaskService.create — resolveAssignee` › handles undefined assignee | `task_service::tests::an_absent_assignee_stays_absent` | |
| `Issue #7: assignee name→ID resolution > TaskService.assign — resolveAssignee` › resolves agent name to ID on reassignment | `task_service::tests::assign_resolves_a_name_to_an_id_but_emits_the_raw_input` | |
| `Issue #7: assignee name→ID resolution > TaskService.assign — resolveAssignee` › passes through valid agent ID | `task_service::tests::assign_sets_the_assignee_and_emits` | |
| `Issue #7: assignee name→ID resolution > AgentService.findBestAgent — name fallback` › matches by agent name when assignee is a name | `agent_service::tests::an_assignee_can_be_an_agent_name` | |
| `Issue #7: assignee name→ID resolution > AgentService.findBestAgent — name fallback` › matches by agent ID when assignee is an ID | `agent_service::tests::an_explicit_assignee_is_honoured_only_while_idle` | |
| `Issue #7: assignee name→ID resolution > AgentService.findBestAgent — name fallback` › returns null when assigned agent is busy | `agent_service::tests::an_explicit_assignee_is_honoured_only_while_idle` | same test, busy-agent half |

## test/unit/application/task-service-attachments.test.ts — 19 cases

| TS case | Rust test | Note |
|---|---|---|
| `Task domain — attachments field` › Task interface has optional attachments?: string[] | `task_service::tests::an_absent_or_empty_attachment_list_leaves_the_field_unset` | TS asserts a type-system shape (`attachments?: string[]`); Rust enforces the same contract at compile time via `Option<Vec<String>>` and this test pins the resulting runtime default |
| `Task domain — attachments field` › Task can be created with attachments array | `task_service::tests::create_copies_attachments_and_stores_their_basenames` | exercised through real task creation with attachments rather than a bare struct literal |
| `Paths — attachments` › attachmentsDir returns .orchestry/attachments | `task_service::tests::attachment_path_is_under_the_attachments_directory_and_rejects_traversal` | |
| `Paths — attachments` › taskAttachmentsDir returns attachments/{id} | `task_service::tests::attachment_path_is_under_the_attachments_directory_and_rejects_traversal` | same test |
| `Paths — attachments` › taskAttachmentsDir is under attachmentsDir | `task_service::tests::attachment_path_is_under_the_attachments_directory_and_rejects_traversal` | same test, containment assertion |
| `Paths — attachments` › taskAttachmentsDir throws on invalid id with path traversal | `task_service::tests::attachment_path_is_under_the_attachments_directory_and_rejects_traversal` | same test, traversal-rejection assertion |
| `TaskService — attachments: undefined` › creates task without attachments field when input.attachments is undefined | `task_service::tests::an_absent_or_empty_attachment_list_leaves_the_field_unset` | |
| `TaskService — attachments: undefined` › creates task without attachments field when no paths provided | `task_service::tests::without_paths_attachments_are_ignored` | |
| `TaskService — attachments: empty array` › empty attachments [] does not set task.attachments | `task_service::tests::an_absent_or_empty_attachment_list_leaves_the_field_unset` | same test, empty-array half |
| `TaskService — attachments: copy files` › copies attachment files and stores basenames | `task_service::tests::create_copies_attachments_and_stores_their_basenames` | |
| `TaskService — attachments: copy files` › copies multiple files and stores all basenames | `task_service::tests::create_copies_attachments_and_stores_their_basenames` | same test, two files |
| `TaskService — attachments: copy files` › creates attachments directory under .orchestry/attachments/{id}/ | `task_service::tests::create_copies_attachments_and_stores_their_basenames` | same test, directory assertion |
| `TaskService — attachments: copy files` › throws InvalidArgumentsError for nonexistent file | `task_service::tests::a_missing_attachment_is_reported_and_nothing_is_copied` | |
| `TaskService — attachments: copy files` › error message contains the missing file path | `task_service::tests::a_missing_attachment_is_reported_and_nothing_is_copied` | same test |
| `TaskService — attachments: copy files` › does not copy any file if one is missing (validation before copy) | `task_service::tests::a_missing_attachment_is_reported_and_nothing_is_copied` | same test, no-partial-copy assertion |
| `TaskService — delete removes attachments directory` › removes attachments directory on delete | `task_service::tests::delete_removes_the_attachments_directory_and_tolerates_its_absence` | |
| `TaskService — delete removes attachments directory` › delete succeeds even when attachments directory does not exist | `task_service::tests::delete_removes_the_attachments_directory_and_tolerates_its_absence` | same test, absent-directory half |
| `TaskService — getAttachmentPath` › returns correct path for attachment file | `task_service::tests::attachment_path_is_under_the_attachments_directory_and_rejects_traversal` | reuse — asserts `attachment_path(id, name) == task_dir.join(name)` |
| `TaskService — getAttachmentPath` › throws when paths not configured | `task_service::tests::attachment_path_requires_paths_to_be_configured` | |

## test/unit/application/task-service.test.ts — 26 cases

| TS case | Rust test | Note |
|---|---|---|
| `TaskService > create` › creates a task with defaults | `task_service::tests::create_applies_the_documented_defaults` | |
| `TaskService > create` › emits task:created event | `task_service::tests::create_persists_and_emits_task_created` | |
| `TaskService > create` › throws on empty title | `task_service::tests::an_empty_title_is_invalid_arguments` | |
| `TaskService > create` › trims title and description | `task_service::tests::create_trims_the_title_and_description` | |
| `TaskService > create > depends_on validation` › throws InvalidArgumentsError for unknown single task ID | `task_service::tests::unknown_dependencies_are_listed_in_the_error` | |
| `TaskService > create > depends_on validation` › error message contains the unknown task ID | `task_service::tests::unknown_dependencies_are_listed_in_the_error` | same test |
| `TaskService > create > depends_on validation` › throws with all unknown IDs listed in error message | `task_service::tests::unknown_dependencies_are_listed_in_the_error` | same test, multi-missing-id case |
| `TaskService > create > depends_on validation` › accepts valid existing task IDs | `task_service::tests::unknown_dependencies_are_listed_in_the_error` | same test, valid-dependency case |
| `TaskService > create > depends_on validation` › accepts empty depends_on array without validation | `task_service::tests::unknown_dependencies_are_listed_in_the_error` | same test, empty-array case |
| `TaskService > create > depends_on validation` › accepts undefined depends_on without validation | `task_service::tests::unknown_dependencies_are_listed_in_the_error` | same test, absent-field case |
| `TaskService > create > depends_on validation` › rejects when only some IDs are missing | `task_service::tests::unknown_dependencies_are_listed_in_the_error` | same test, partially-missing case |
| `TaskService > create > depends_on validation` › accepts multiple valid task IDs | `task_service::tests::unknown_dependencies_are_listed_in_the_error` | same test, multi-valid case |
| `TaskService > get` › returns task when found | `task_service::tests::get_returns_the_task_when_present` | |
| `TaskService > get` › throws TaskNotFoundError when not found | `task_service::tests::get_reports_task_not_found_with_exit_1` | |
| `TaskService > updateStatus` › transitions valid status and emits event | `task_service::tests::update_status_transitions_and_emits` | |
| `TaskService > updateStatus` › throws InvalidTransitionError for invalid transition | `task_service::tests::update_status_refuses_a_transition_outside_the_table` | |
| `TaskService > assign` › sets assignee and emits event | `task_service::tests::assign_sets_the_assignee_and_emits` | |
| `TaskService > cancel` › cancels a todo task | `task_service::tests::cancel_moves_a_todo_task_to_cancelled` | |
| `TaskService > cancel` › throws on terminal task | `task_service::tests::cancel_refuses_a_terminal_task` | |
| `TaskService > cancel` › cancels an in_progress task (process cleanup is orchestrator responsibility) | `task_service::tests::cancel_also_works_on_an_in_progress_task` | |
| `TaskService > retry` › resets failed task to todo with attempts=0 | `task_service::tests::retry_resets_a_failed_task_to_todo_with_no_attempts` | |
| `TaskService > retry` › resets cancelled task to todo | `task_service::tests::retry_also_resets_a_cancelled_task` | |
| `TaskService > retry` › throws on non-failed/cancelled task | `task_service::tests::retry_refuses_anything_but_failed_or_cancelled` | |
| `TaskService > delete` › deletes a non-running task | `task_service::tests::delete_removes_a_non_running_task` | |
| `TaskService > delete` › throws on running task | `task_service::tests::delete_refuses_a_running_task` | |
| `TaskService > incrementAttempts` › increments attempts by 1 | `task_service::tests::increment_attempts_adds_one` | |

## test/unit/application/team-service.test.ts — 23 cases

| TS case | Rust test | Note |
|---|---|---|
| `TeamService > create` › creates team with valid input | `team_service::tests::create_builds_the_team_with_the_lead_first` | |
| `TeamService > create` › throws if lead agent not found | `team_service::tests::create_rejects_an_unknown_lead_agent` | |
| `TeamService > create` › throws if name already exists | `team_service::tests::create_rejects_a_duplicate_name` | |
| `TeamService > create` › throws if name is empty | `team_service::tests::create_rejects_an_empty_name` | |
| `TeamService > create` › emits team:created event | `team_service::tests::create_emits_team_created` | |
| `TeamService > join` › adds member | `team_service::tests::join_adds_a_member` | |
| `TeamService > join` › throws if agent already member | `team_service::tests::join_rejects_an_existing_member` | |
| `TeamService > join` › emits team:member_joined event | `team_service::tests::join_emits_team_member_joined` | |
| `TeamService > leave` › removes member | `team_service::tests::leave_removes_a_member` | |
| `TeamService > leave` › throws if trying to remove lead | `team_service::tests::leave_rejects_the_lead` | |
| `TeamService > leave` › emits team:member_left event | `team_service::tests::leave_emits_team_member_left` | |
| `TeamService > addTask` › adds task to pool | `team_service::tests::add_task_appends_to_the_pool` | |
| `TeamService > addTask` › throws if task not found | `team_service::tests::add_task_rejects_an_unknown_task` | |
| `TeamService > addTask` › emits team:task_added event | `team_service::tests::add_task_emits_team_task_added` | |
| `TeamService > addTask` › does not duplicate task in pool | `team_service::tests::add_task_does_not_duplicate_a_pooled_task` | |
| `TeamService > setLead` › transfers lead role | `team_service::tests::set_lead_transfers_the_role` | |
| `TeamService > setLead` › throws if agent is not a member | `team_service::tests::set_lead_rejects_a_non_member` | |
| `TeamService > disband` › sets status to disbanded | `team_service::tests::disband_sets_the_status_to_disbanded` | |
| `TeamService > disband` › emits team:disbanded event | `team_service::tests::disband_emits_team_disbanded` | |
| `TeamService > findTeamForAgent` › returns the team for an agent | `team_service::tests::find_team_for_agent_returns_the_team` | |
| `TeamService > findTeamForAgent` › returns null if agent is not in any team | `team_service::tests::find_team_for_agent_returns_none_when_unaffiliated` | |
| `TeamService > findTeamForAgent` › ignores disbanded teams | `team_service::tests::find_team_for_agent_ignores_disbanded_teams` | |
| `TeamService > get` › throws TeamNotFoundError for non-existent team | `team_service::tests::get_reports_team_not_found_with_exit_1` | |

## test/integration/new-adapters.e2e.test.ts — 2 cases

| TS case | Rust test | Note |
|---|---|---|
| `new adapters — e2e through Orchestrator` › drives a Grok task todo → in_progress → review → done | `grok_drives_a_task_from_todo_to_done` | integration test in `crates/orch-engine/tests/adapter_e2e.rs` |
| `new adapters — e2e through Orchestrator` › drives an Antigravity task todo → in_progress → review → done | `antigravity_drives_a_task_from_todo_to_done` | integration test in `crates/orch-engine/tests/adapter_e2e.rs` |

## test/integration/pi-adapter.e2e.test.ts — 2 cases

| TS case | Rust test | Note |
|---|---|---|
| `Pi adapter — end-to-end through Orchestrator` › drives a task todo → in_progress → review → done via real PiAdapter + mocked spawn | `pi_drives_a_task_from_todo_to_done` | integration test in `crates/orch-engine/tests/adapter_e2e.rs` |
| `Pi adapter — end-to-end through Orchestrator` › keeps a rate-limited Pi task out of done and records the run as failed | `a_rate_limited_pi_run_fails_and_never_reaches_done` | integration test in `crates/orch-engine/tests/adapter_e2e.rs` |

## test/integration/scope-index.e2e.test.ts — 1 case

| TS case | Rust test | Note |
|---|---|---|
| `scope scheduling — real runtime e2e` › serializes overlapping scopes while filling free slots with disjoint work | `overlapping_scopes_serialize_while_disjoint_work_fills_the_free_slots` | integration test in `crates/orch-engine/tests/scope_index_e2e.rs` |

## Beyond the TS suite

Rust tests with no TS counterpart — new coverage added during the port. Grouped by module,
with a one-line reason per group (not every individual test is enumerated where the group
reason is uniform).

- **`orchestrator::tests::autonomous`** (25 tests) — the entire autonomous-seeding engine
  (goal-driven and role-driven task seeding for idle autonomous agents, cooldowns, retry
  context, mailbox draining into the dispatch prompt). There is no corresponding TS test
  file in this inventory; this behaviour is tested only in Rust.
- **`container::tests`** (9 tests) — the `Engine`/`FullEngine` dependency-wiring tiers
  (`src/container.ts`'s Rust port has no direct TS unit-test file in this inventory; wiring
  is exercised indirectly through the TS integration suites instead).
- **`doctor_service::tests`** (6 tests) — diagnostics (`doctor`) checks: adapter probing,
  git/gitignore checks, toolchain row, contract ordering. No TS unit-test file for
  `doctor-service.ts` is in this inventory.
- **`lock::tests`** (2 tests) — the project-lock seam (`FileLock`/in-memory lock), unit
  tested directly in Rust; TS tests this only indirectly via `vi.mock('.../storage/lock.js')`
  inside the orchestrator suites.
- **`agent_service::tests`** (8 tests): `an_ai_agent_keeps_model_and_the_configured_approval_policy`,
  `an_empty_adapter_falls_back_to_the_configured_default`, `list_preserves_last_write_order`,
  `set_autonomous_persists_and_emits`, `shell_drops_model_and_effort_even_when_given`,
  `update_rejects_an_empty_name_and_a_duplicate`, `update_stats_assigns_only_the_given_counters`,
  `the_success_rate_bonus_rounds_like_math_round` — extra `create`/`update`/`list` edge
  cases and the rounding-table pin for the success-rate bonus; none has a matching `it(...)`
  title in `agent-service.test.ts`.
- **`task_service::tests`** (8 tests): `a_proof_bearing_task_round_trips_through_update`,
  `assign_rejects_an_unknown_agent`, `list_passes_the_filter_through_to_the_store`,
  `optional_inputs_land_on_the_record`, `reject_refuses_a_task_that_is_not_in_review`,
  `reject_sends_a_reviewed_task_back_to_todo_with_feedback`,
  `update_appends_attachments_rather_than_replacing_them`, `update_changes_the_given_fields_only`
  — `reject()` has no TS test in this inventory at all, and `update()`'s title/description/
  labels/attachments paths and `list()` are exercised only incidentally by other TS tests,
  never by a dedicated `it(...)`.
- **`goal_service::tests`** (15 tests): `create_trims_and_emits_goal_created`,
  `create_rejects_an_empty_title`, `delete_removes_the_goal_and_emits_goal_deleted`,
  `get_reports_goal_not_found_with_exit_1`, `an_invalid_transition_is_rejected_with_exit_2`,
  `a_status_change_is_persisted_and_announced`, `resuming_a_paused_goal_re_enables_autonomous`,
  `pausing_cancels_the_agents_dispatchable_autonomous_tasks`,
  `update_trims_supplied_fields_and_leaves_the_rest`, `update_rejects_an_empty_title`,
  `list_passes_the_filter_through_to_the_store`, `list_tasks_for_goal_is_empty_without_a_task_service`,
  `get_progress_report_reads_the_goal_progress_context_key`,
  `an_empty_assignee_string_toggles_nothing`, `goal_created_is_emitted_before_the_autonomous_toggle`
  — plain CRUD/validation/event-shape contract that a basic `goal-service.test.ts` would
  cover in TS but isn't in this inventory (only `goal-service-autonomous.test.ts` is), plus
  two autonomous branches (resume, pause-cancels-tasks) the TS autonomous suite never
  exercises.
- **`message_service::tests`** (4 tests): `the_cli_may_send_without_being_a_registered_agent`,
  `an_empty_subject_falls_back_and_both_caps_apply`,
  `a_broadcast_team_id_naming_no_team_degrades_to_every_agent`,
  `truncation_counts_utf16_units_and_never_splits_a_character` — the Rust source marks
  these explicitly as beyond the TS suite (CLI-sender edge case, subject fallback, broadcast
  degrade path, and UTF-16-vs-`char` truncation boundary).
- **`team_service::tests`** (7 tests): `create_skips_a_member_who_is_also_the_lead`,
  `create_rejects_an_unknown_member_before_saving`, `create_overlays_a_partial_config_on_the_defaults`,
  `join_rejects_an_unknown_agent_with_exit_2`, `leave_is_not_an_error_for_a_non_member`,
  `remove_task_drops_the_task_and_emits_nothing`, `set_lead_on_the_current_lead_keeps_the_role`
  — extra `create`/`join`/`leave`/`removeTask`/`setLead` edge cases with no matching TS `it(...)`.
- **`run_service::tests`** (4 tests): `create_records_a_preparing_run_and_emits_nothing`,
  `start_sets_the_pid_and_emits_agent_started`, `finish_stamps_the_finish_time_and_reports_success_per_status`,
  `start_and_finish_report_run_not_found_with_exit_1` — `run-service.test.ts` in this
  inventory only tests `getLastFailedRunContext`; it never exercises the `create`/`start`/
  `finish` lifecycle, which the Rust port tests directly.
- **`review_runner::tests`** (5 tests): `the_default_timeout_is_120_seconds_and_fail_fast_is_on`,
  `an_empty_criteria_list_runs_nothing`, `the_process_runner_reports_a_missing_binary_as_a_failure`,
  `the_process_runner_captures_output_and_the_exit_status`,
  `the_process_runner_fails_a_child_that_outruns_its_timeout` — default-options pin, the
  empty-criteria no-op, and three tests of the **real** `ProcessCommandRunner` (the TS
  suite only ever mocks `execFile`, so a real-process test has no TS analogue by
  construction).
- **`events::tests`** (8 tests): `a_subscriber_receives_events_in_order`,
  `every_subscriber_sees_every_event`, `emitting_without_subscribers_is_not_an_error`,
  `recv_resolves_when_an_event_arrives`, `recv_returns_none_once_the_bus_is_dropped`,
  `a_lagging_subscriber_keeps_going_rather_than_erroring_out`, `a_nested_emit_keeps_channel_order`,
  `a_clone_shares_the_registry` — the `tokio::sync::broadcast`-backed streaming layer
  (`subscribe`/`recv`/`try_drain`) and `Clone` semantics, both Rust-specific; the
  synchronous TS `EventBus` has no equivalent API surface.
- **`orchestrator::tests::core`** (3 tests): `a_failing_tick_emits_orchestrator_error_with_the_tick_context`,
  `a_retry_that_is_not_yet_due_stays_queued`, `reconcile_emits_stall_detected_and_kills_a_stalled_run`
  — a direct unit test of non-fatal tick-failure reporting (TS only tests the 5-failures-fatal
  path end-to-end), the not-yet-due half of the retry-due check (TS only exercises the due
  case), and the real stall-detection-and-kill path (the TS case whose title mentions
  `stall_detected` actually tests terminal-task cleanup, not a real stall — see the note on
  that row in `orchestrator.test.ts` above).
- **`orchestrator::tests::bugfixes_2026_03`** (3 tests): `the_write_tool_match_is_case_insensitive_and_covers_the_four_names`,
  `the_stall_check_is_strictly_greater_than_the_timeout`,
  `the_default_config_global_stall_timeout_is_ten_minutes` — boundary/edge-case coverage
  (case-insensitivity of the write-tool match, the strict `>` in the stall check, and the
  ten-minute default) with no dedicated TS `it(...)`.
- **`orchestrator::tests::resilience`** (3 tests): `a_successful_tick_resets_the_counter_before_the_breaker_trips`,
  `the_debounce_coalesces_many_lazy_saves_into_one_write`, `flushing_forces_the_pending_write_out_immediately`
  — a stronger recovery-before-the-breaker-trips variant, and two debounce-robustness tests
  that duplicate what `orchestrator-endurance.test.ts` already covers but are kept here
  alongside the Fix-4 debounced-save-error tests.
- **`orchestrator::tests::retry_race`** (1 test): `the_success_handler_returns_early_when_the_entry_is_already_gone`
  — the symmetric late-success guard; TS only tests the late-failure side.
- **`orchestrator::tests::parallel_race`** (1 test): `a_stream_error_with_a_live_entry_goes_through_the_failure_handler`
  — the symmetric case to the already-gone-entry test.
- **`orchestrator::tests::endurance`** (2 tests): `startup_recovery_clears_every_claim`,
  `the_success_handler_recomputes_the_token_total_rather_than_accumulating_it` — the
  `claimed`-set half of the "stay bounded" scenario as its own case, and a correctness
  property (recomputing rather than accumulating the token total) not explicitly asserted
  by any TS case in this file.
- **`orchestrator::tests::force_review`** (2 tests): `a_merge_error_prefixes_the_summary_and_skips_auto_approve`,
  `a_clean_merge_reports_success_and_cleans_the_worktree_up` — the merge-error-summary
  format and the happy-path merge; the 3 TS cases in this file only assert `current_task`
  clearing and review status.
- **`orchestrator::tests::mutex`** (1 test): `the_internal_phases_cannot_re_enter_the_state_lock`
  — a Rust-specific reentrancy guarantee with no TS analogue.
- **`orchestrator::tests::skills`** (3 tests): `several_library_skills_are_joined_in_the_agents_order`,
  `split_mode_renders_the_two_templates_separately`, `a_custom_split_template_pair_is_used_when_configured`
  — coverage of the Rust split-template (system/user) rendering architecture, which has no
  direct TS equivalent.
- **`orchestrator::tests::auto_review_approve`** (3 tests): `passing_criteria_without_auto_approve_still_reach_done`,
  `criteria_run_in_the_tasks_own_workspace`, `criteria_fall_back_to_the_project_root_when_the_task_has_no_workspace`
  — the all-pass-without-auto-approve path, and workspace routing for review-criteria
  commands.
- **`orchestrator::tests::defensive_validation`** (1 test): `a_usable_event_timestamp_is_kept_as_given`
  — the symmetric case to `an_unusable_event_timestamp_is_replaced_with_the_clocks_own`,
  pinning that a *valid* supplied timestamp survives `collect_events` unchanged; TS's
  Fix #5 only tests `isValidISOTimestamp` directly, never through the collection path.
- **`orchestrator::tests::error_kind_propagation`** (1 test): `the_last_error_event_kind_wins_over_the_thrown_errors_kind`
  — pins precedence between the adapter event's error kind and the thrown error's
  classification; no TS case in this file addresses which one wins.
- Integration e2e extras (2 tests): `a_grok_child_that_floods_stderr_still_completes_and_leaks_nothing_into_the_error`
  (`tests/adapter_e2e.rs`) and `a_task_with_no_scope_is_never_blocked_by_one_that_has_scope`
  (`tests/scope_index_e2e.rs`) — a stderr-flood robustness case and the no-scope-task
  baseline, both added during the port with no TS counterpart.

`orchestrator::tests::perf_opts` and `orchestrator::tests::dispatch_worktree` have no
extras — every Rust test in those two modules is cited by a TS case above.
