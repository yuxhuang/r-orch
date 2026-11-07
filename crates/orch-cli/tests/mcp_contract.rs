//! CLI drift alarm for the MCP catalogue (design D20).
//!
//! `orch mcp` maps every project operation of the CLI to one tool, or to an
//! explicit exclusion (spec R3). The tool registry and `mcp_stdio`'s EXPECTED
//! lists could both stay green while the CLI grows an operation, a flag or a
//! changed default — so this file introspects clap's `Cli::command()` and
//! compares it with a checked-in, human-reviewed expectation. It is an alarm,
//! not a generator: when it fails, decide the MCP consequence (new tool,
//! argument, default or exclusion), update `DISPOSITION` and the expectation
//! together, and say why in the PR.
//!
//! Normalised form, one block per command path (aliases in `[..]`):
//!
//! ```text
//! LEAF task add            <- an operation (no subcommands)
//!   <title> required       <- positional, in position order
//!   --priority -p default=3
//!   --edit -e flag         <- boolean switch
//! GROUP task               <- a container with subcommands
//! ```
//!
//! Options are sorted, so reordering flags in the source does not fail the
//! test; adding, removing, renaming, re-defaulting, re-requiring or changing
//! the allowed values of one does. Global presentation flags and help/version
//! are classified once in [`GLOBALS`] instead of being repeated per leaf.

mod mcp_common;

use std::collections::{BTreeMap, BTreeSet};

use clap::{Arg, ArgAction, Command, CommandFactory};
use mcp_common::{DESTRUCTIVE, EXPECTED};
use orch_cli::tree::Cli;

/// What the MCP server does with one CLI operation.
enum Disposition {
    /// Exposed as this tool.
    Tool(&'static str),
    /// Deliberately not exposed (spec R3); the reason is the design's.
    Excluded(&'static str),
}
use Disposition::{Excluded, Tool};

/// Every CLI leaf, by canonical (non-alias) path.
const DISPOSITION: &[(&str, Disposition)] = &[
    ("task add", Tool("task_add")),
    ("task list", Tool("task_list")),
    ("task show", Tool("task_show")),
    ("task edit", Excluded("editor-spawning")),
    ("task update", Tool("task_update")),
    ("task patch", Tool("task_patch")),
    ("task assign", Tool("task_assign")),
    ("task cancel", Tool("task_cancel")),
    ("task approve", Tool("task_approve")),
    ("task reject", Tool("task_reject")),
    ("task retry", Tool("task_retry")),
    ("agent add", Tool("agent_add")),
    (
        "agent shop",
        Excluded("interactive download/install workflow"),
    ),
    ("agent list", Tool("agent_list")),
    ("agent status", Tool("agent_show")),
    ("agent edit", Excluded("editor-spawning")),
    ("agent update", Tool("agent_update")),
    ("agent patch", Tool("agent_patch")),
    ("agent remove", Tool("agent_remove")),
    ("agent disable", Tool("agent_disable")),
    ("agent enable", Tool("agent_enable")),
    ("agent autonomous", Tool("agent_autonomous")),
    ("remote add", Excluded("peer configuration workflow")),
    ("remote remove", Excluded("peer configuration workflow")),
    ("remote list", Tool("remote_list")),
    ("remote status", Tool("remote_status")),
    ("remote listen", Excluded("opens a network listener")),
    ("status", Tool("status")),
    ("logs", Tool("logs")),
    ("config get", Tool("config_get")),
    ("config set", Tool("config_set")),
    ("config edit", Excluded("editor-spawning")),
    ("config update", Tool("config_update")),
    ("config patch", Tool("config_patch")),
    (
        "config global get",
        Excluded("user-global settings outside the project"),
    ),
    (
        "config global set",
        Excluded("user-global settings outside the project"),
    ),
    (
        "config global show",
        Excluded("user-global settings outside the project"),
    ),
    ("context set", Tool("context_set")),
    ("context get", Tool("context_get")),
    ("context list", Tool("context_list")),
    ("context delete", Tool("context_delete")),
    ("msg send", Tool("msg_send")),
    ("msg broadcast", Tool("msg_broadcast")),
    ("msg inbox", Tool("msg_inbox")),
    ("msg list", Tool("msg_list")),
    ("goal add", Tool("goal_add")),
    ("goal list", Tool("goal_list")),
    ("goal show", Tool("goal_show")),
    ("goal status", Tool("goal_status")),
    ("goal update", Tool("goal_update")),
    ("goal delete", Tool("goal_delete")),
    ("team create", Tool("team_create")),
    ("team list", Tool("team_list")),
    ("team show", Tool("team_show")),
    ("team join", Tool("team_join")),
    ("team leave", Tool("team_leave")),
    ("team add-task", Tool("team_add_task")),
    ("team set-lead", Tool("team_set_lead")),
    ("team disband", Tool("team_disband")),
    (
        "org list",
        Excluded("template/deployment subsystem deferred"),
    ),
    (
        "org template list",
        Excluded("template/deployment subsystem deferred"),
    ),
    (
        "org template show",
        Excluded("template/deployment subsystem deferred"),
    ),
    (
        "org template init",
        Excluded("template/deployment subsystem deferred"),
    ),
    (
        "org template edit",
        Excluded("template/deployment subsystem deferred"),
    ),
    (
        "org template update",
        Excluded("template/deployment subsystem deferred"),
    ),
    (
        "org template patch",
        Excluded("template/deployment subsystem deferred"),
    ),
    (
        "org template agent list",
        Excluded("template/deployment subsystem deferred"),
    ),
    (
        "org template agent add",
        Excluded("template/deployment subsystem deferred"),
    ),
    (
        "org template agent edit",
        Excluded("template/deployment subsystem deferred"),
    ),
    (
        "org template agent update",
        Excluded("template/deployment subsystem deferred"),
    ),
    (
        "org template agent patch",
        Excluded("template/deployment subsystem deferred"),
    ),
    (
        "org template agent delete",
        Excluded("template/deployment subsystem deferred"),
    ),
    (
        "org deploy",
        Excluded("template/deployment subsystem deferred"),
    ),
    ("run", Excluded("starts an orchestrator and child agents")),
    ("doctor", Tool("doctor")),
    ("tui", Excluded("interactive UI")),
    ("serve", Excluded("starts an orchestrator")),
    ("web", Excluded("network server")),
    ("mcp", Excluded("is the server itself")),
    (
        "init",
        Excluded("bootstraps a project; MCP requires an existing one"),
    ),
    ("update", Excluded("host software management")),
];

/// The global presentation flags (`global = true`) and nothing else. They are
/// ignored by the server transport, so no tool takes them (design D6).
const GLOBALS: &[&str] = &[
    "--ascii flag",
    "--json flag",
    "--no-color flag",
    "--quiet flag",
    "--yaml flag",
];

/// The checked-in CLI surface. Regenerate only after deciding the MCP
/// consequence of each difference; see the module docs.
const EXPECTED_SURFACE: &str = r"
GROUP task
LEAF task add
  <title> required
  --assignee
  --attach
  --depends-on
  --description -d
  --edit -e flag
  --goal-id
  --labels -l
  --max-attempts
  --priority -p default=3
  --review-criteria
  --scope
  --workspace-mode
LEAF task list
  --status
LEAF task show
  <id> required
LEAF task edit
  <id> required
LEAF task update
  <id> required
  --file -f required
  --input-format values=json|yaml
LEAF task patch
  <id> required
  --file -f required
  --input-format values=json|yaml
LEAF task assign
  <task_id> required
  <agent_id> required
LEAF task cancel
  <id> required
LEAF task approve
  <id> required
LEAF task reject
  <id> required
  --reason -r
LEAF task retry
  <id> required
GROUP agent
LEAF agent add
  <name> required
  --adapter required
  --approval-policy
  --command
  --edit -e flag
  --effort
  --max-turns
  --model
  --prompt-mode alias=mode
  --role
  --role-summary
  --skills
  --timeout
  --workspace-mode
LEAF agent shop
  --list flag
  --template
  --template-dir
LEAF agent list
LEAF agent status[show]
  <id> required
  --effective flag
LEAF agent edit
  <id> required
LEAF agent update
  <id> required
  --effort
  --file -f
  --input-format values=json|yaml
  --prompt-mode alias=mode
  --role-summary
LEAF agent patch
  <id> required
  --file -f required
  --input-format values=json|yaml
LEAF agent remove
  <id> required
LEAF agent disable
  <id> required
LEAF agent enable
  <id> required
LEAF agent autonomous
  <id> required
  --off flag
  --on flag
GROUP remote
LEAF remote add
  <alias> required
  <addr> required
LEAF remote remove
  <alias> required
LEAF remote list
LEAF remote status
LEAF remote listen
  <state> required values=on|off
  <addr>
LEAF status
LEAF logs
  <run_id>
  --agent
  --follow flag
  --since
  --task
GROUP config
LEAF config get
  <key> required
LEAF config set
  <key> required
  <value> required
LEAF config edit
LEAF config update
  --file -f required
  --input-format values=json|yaml
LEAF config patch
  --file -f required
  --input-format values=json|yaml
GROUP config global
LEAF config global get
  <key> required
LEAF config global set
  <key> required
  <value> required
LEAF config global show
GROUP context
LEAF context set
  <key> required
  <value> required
  --ttl
LEAF context get
  <key> required
LEAF context list
LEAF context delete
  <key> required
GROUP msg
LEAF msg send
  <to_agent_id> required
  <body> required
  --from
  --reply-to
  --subject -s
  --ttl
LEAF msg broadcast
  <body> required
  --from
  --subject -s
  --team
  --ttl
LEAF msg inbox
  <agent_id> required
LEAF msg list
  --agent
GROUP goal
LEAF goal add
  <title> required
  --assignee
  --description
LEAF goal list[ls]
  --status
LEAF goal show
  <id> required
LEAF goal status
  <id> required
  <status> required
  --force flag
LEAF goal update
  <id> required
  --assignee
  --description
  --title
LEAF goal delete[rm]
  <id> required
GROUP team
LEAF team create
  <name> required
  --description -d
  --lead required
  --members
  --no-auto-claim flag
LEAF team list
LEAF team show
  <id> required
LEAF team join
  <team_id> required
  <agent_id> required
LEAF team leave
  <team_id> required
  <agent_id> required
LEAF team add-task
  <team_id> required
  <task_id> required
LEAF team set-lead
  <team_id> required
  <agent_id> required
LEAF team disband
  <id> required
GROUP org
LEAF org list[ls]
GROUP org template
LEAF org template list
LEAF org template show
  <name>
  --resolved flag
  --template-dir
LEAF org template init
  <name> required
  --from
  --template-dir
LEAF org template edit
  <name>
  --template-dir
LEAF org template update
  <name>
  --file -f required
  --input-format values=json|yaml
  --template-dir
LEAF org template patch
  <name>
  --file -f required
  --input-format values=json|yaml
  --template-dir
GROUP org template agent
LEAF org template agent list
  <name>
  --template-dir
LEAF org template agent add
  <name>
  --file -f required
  --input-format values=json|yaml
  --template-dir
LEAF org template agent edit
  <name>
  <agent>
  --agent
  --template-dir
LEAF org template agent update
  <name>
  <agent>
  --agent
  --file -f required
  --input-format values=json|yaml
  --template-dir
LEAF org template agent patch
  <name>
  <agent>
  --agent
  --file -f required
  --input-format values=json|yaml
  --template-dir
LEAF org template agent delete
  <name>
  <agent>
  --agent
  --template-dir
LEAF org deploy
  <template>
  --adapter
  --goal
  --model
  --template
  --template-dir
LEAF run
  <task_id>
  --all flag
  --listen
  --peer-name
  --verbose flag
  --watch flag
LEAF doctor
LEAF tui
  --listen
  --observe flag
  --peer-name
LEAF serve
  --listen
  --log-file
  --log-format default=json
  --once flag
  --peer-name
  --tick-interval
  --verbose flag
LEAF web
  --allow-host multi
  --bind default=127.0.0.1:7681
  --no-auth flag
  --observe flag
  --password
LEAF mcp
  --agent
  --allow-destructive flag
  --project-root
  --read-only flag
LEAF init
  --adapter
  --name
  --prompt-mode alias=mode
LEAF update
  --check flag
";

// ── normalisation ─────────────────────────────────────────────────────────

fn segment(command: &Command) -> String {
    let mut text = command.get_name().to_owned();
    let aliases: Vec<&str> = command.get_all_aliases().collect();
    if !aliases.is_empty() {
        text.push_str(&format!("[{}]", aliases.join(",")));
    }
    if command.is_hide_set() {
        text.push_str("{hidden}");
    }
    text
}

fn is_switch(arg: &Arg) -> bool {
    matches!(
        arg.get_action(),
        ArgAction::SetTrue | ArgAction::SetFalse | ArgAction::Count
    )
}

fn describe(arg: &Arg) -> String {
    let mut parts = Vec::new();
    if arg.is_positional() {
        parts.push(format!("<{}>", arg.get_id()));
    } else {
        if let Some(long) = arg.get_long() {
            parts.push(format!("--{long}"));
        }
        if let Some(short) = arg.get_short() {
            parts.push(format!("-{short}"));
        }
    }
    if is_switch(arg) {
        parts.push("flag".to_owned());
    }
    if matches!(arg.get_action(), ArgAction::Append) {
        parts.push("multi".to_owned());
    }
    if arg.is_required_set() {
        parts.push("required".to_owned());
    }
    // A switch's implicit `false` default is not information.
    if !is_switch(arg) {
        let defaults: Vec<String> = arg
            .get_default_values()
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect();
        if !defaults.is_empty() {
            parts.push(format!("default={}", defaults.join(",")));
        }
        let values: Vec<String> = arg
            .get_possible_values()
            .iter()
            .filter(|value| !value.is_hide_set())
            .map(|value| value.get_name().to_owned())
            .collect();
        if !values.is_empty() {
            parts.push(format!("values={}", values.join("|")));
        }
    }
    if let Some(aliases) = arg.get_all_aliases() {
        parts.push(format!("alias={}", aliases.join(",")));
    }
    if arg.is_hide_set() {
        parts.push("hidden".to_owned());
    }
    parts.join(" ")
}

fn walk(command: &Command, path: &mut Vec<String>, out: &mut Vec<String>) {
    let children: Vec<&Command> = command
        .get_subcommands()
        .filter(|child| child.get_name() != "help")
        .collect();
    if !path.is_empty() {
        let kind = if children.is_empty() { "LEAF" } else { "GROUP" };
        out.push(format!("{kind} {}", path.join(" ")));
        if children.is_empty() {
            for positional in command.get_positionals() {
                out.push(format!("  {}", describe(positional)));
            }
            let mut options: Vec<String> = command
                .get_arguments()
                .filter(|arg| {
                    !arg.is_positional()
                        && !arg.is_global_set()
                        && !matches!(arg.get_id().as_str(), "help" | "version")
                })
                .map(describe)
                .collect();
            options.sort();
            out.extend(options.into_iter().map(|option| format!("  {option}")));
        }
    }
    for child in children {
        path.push(segment(child));
        walk(child, path, out);
        path.pop();
    }
}

/// The live CLI in the normalised form, plus its global flags.
fn live() -> (String, Vec<String>) {
    let mut cli = Cli::command();
    cli.build();
    let mut lines = Vec::new();
    walk(&cli, &mut Vec::new(), &mut lines);
    let mut globals: Vec<String> = cli
        .get_arguments()
        .filter(|arg| arg.is_global_set())
        .map(describe)
        .collect();
    globals.sort();
    (lines.join("\n"), globals)
}

/// Block header → its argument lines.
fn blocks(text: &str) -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        if line.starts_with(' ') {
            let header = current.as_ref().expect("argument line before any header");
            map.get_mut(header)
                .expect("header recorded")
                .push(line.to_owned());
        } else {
            assert!(
                map.insert(line.to_owned(), Vec::new()).is_none(),
                "duplicate block {line:?}"
            );
            current = Some(line.to_owned());
        }
    }
    map
}

/// Human-readable differences between two surfaces; empty means identical.
fn drift(expected: &str, actual: &str) -> Vec<String> {
    let (expected, actual) = (blocks(expected), blocks(actual));
    let mut report = Vec::new();
    for header in expected.keys().filter(|key| !actual.contains_key(*key)) {
        report.push(format!("removed or renamed: {header}"));
    }
    for header in actual.keys().filter(|key| !expected.contains_key(*key)) {
        report.push(format!("new: {header}"));
    }
    for (header, want) in &expected {
        let Some(got) = actual.get(header) else {
            continue;
        };
        let (want_set, got_set): (BTreeSet<_>, BTreeSet<_>) =
            (want.iter().collect(), got.iter().collect());
        for line in want_set.difference(&got_set) {
            report.push(format!("{header}: expected{line}"));
        }
        for line in got_set.difference(&want_set) {
            report.push(format!("{header}: found   {line}"));
        }
        if want_set == got_set && want != got {
            report.push(format!("{header}: positional order changed"));
        }
    }
    report
}

fn leaf_paths(surface: &str) -> BTreeSet<String> {
    blocks(surface)
        .keys()
        .filter_map(|header| header.strip_prefix("LEAF "))
        .map(canonical)
        .collect()
}

/// `agent status[show]` → `agent status`.
fn canonical(path: &str) -> String {
    path.split(' ')
        .map(|segment| segment.split(['[', '{']).next().unwrap_or(segment))
        .collect::<Vec<_>>()
        .join(" ")
}

// ── tests ─────────────────────────────────────────────────────────────────

#[test]
fn the_cli_surface_matches_the_checked_in_expectation() {
    let (actual, _) = live();

    let report = drift(EXPECTED_SURFACE, &actual);

    assert!(
        report.is_empty(),
        "the CLI changed; decide its MCP consequence, then update DISPOSITION and \
         EXPECTED_SURFACE together (design D20):\n{}",
        report.join("\n")
    );
}

#[test]
fn only_the_five_presentation_flags_are_global() {
    let (_, globals) = live();

    assert_eq!(globals, GLOBALS);
}

#[test]
fn every_cli_operation_is_mapped_to_a_tool_or_explicitly_excluded() {
    let (actual, _) = live();
    let live_leaves = leaf_paths(&actual);
    let classified: BTreeSet<String> = DISPOSITION
        .iter()
        .map(|(path, _)| (*path).to_owned())
        .collect();

    let unclassified: Vec<&String> = live_leaves.difference(&classified).collect();
    let stale: Vec<&String> = classified.difference(&live_leaves).collect();

    assert!(
        unclassified.is_empty() && stale.is_empty(),
        "CLI operations with no disposition: {unclassified:?}; dispositions for operations \
         that no longer exist: {stale:?}"
    );
}

#[test]
fn the_mapped_tools_are_exactly_the_fifty_catalogue_names_each_used_once() {
    let mut mapped: Vec<&str> = DISPOSITION
        .iter()
        .filter_map(|(_, disposition)| match disposition {
            Tool(name) => Some(*name),
            Excluded(_) => None,
        })
        .collect();
    mapped.sort();
    let mut catalogue: Vec<&str> = EXPECTED.iter().chain(DESTRUCTIVE).copied().collect();
    catalogue.sort();

    assert_eq!(mapped, catalogue);
}

#[test]
fn every_exclusion_gives_a_reason() {
    let blank: Vec<&str> = DISPOSITION
        .iter()
        .filter_map(|(path, disposition)| match disposition {
            Excluded(reason) if reason.trim().is_empty() => Some(*path),
            _ => None,
        })
        .collect();

    assert!(blank.is_empty(), "exclusions without a reason: {blank:?}");
}

/// A small fixed surface for testing the comparator itself, so these tests
/// stay valid whatever the live CLI looks like.
const SAMPLE: &str = "GROUP task
LEAF task add
  <title> required
  --priority -p default=3
LEAF task show
  <id> required
LEAF goal list[ls]
  --status
";

#[test]
fn drift_reports_a_changed_default() {
    let edited = SAMPLE.replace("default=3", "default=4");

    let report = drift(SAMPLE, &edited);

    assert_eq!(
        report,
        [
            "LEAF task add: expected  --priority -p default=3",
            "LEAF task add: found     --priority -p default=4",
        ]
    );
}

#[test]
fn drift_reports_a_new_operation() {
    let edited = format!("{SAMPLE}LEAF task retry\n  <id> required\n");

    assert_eq!(drift(SAMPLE, &edited), ["new: LEAF task retry"]);
}

#[test]
fn drift_reports_a_removed_or_renamed_operation() {
    let edited = SAMPLE.replace("LEAF task show", "LEAF task view");

    let report = drift(SAMPLE, &edited);

    assert_eq!(
        report,
        ["removed or renamed: LEAF task show", "new: LEAF task view"]
    );
}

#[test]
fn drift_reports_an_option_added_to_one_operation_that_others_share() {
    // `--status` already exists under `goal list`; a surface-wide line-set
    // diff would call this unchanged.
    let edited = SAMPLE.replace("LEAF task show\n", "LEAF task show\n  --status\n");

    let report = drift(SAMPLE, &edited);

    assert_eq!(report, ["LEAF task show: found     --status"]);
}

#[test]
fn drift_reports_a_changed_requirement() {
    let edited = SAMPLE.replace("  <id> required", "  <id>");

    let report = drift(SAMPLE, &edited);

    assert_eq!(
        report,
        [
            "LEAF task show: expected  <id> required",
            "LEAF task show: found     <id>",
        ]
    );
}

#[test]
fn drift_reports_reordered_positionals() {
    let before = "LEAF x\n  <a> required\n  <b> required\n";
    let after = "LEAF x\n  <b> required\n  <a> required\n";

    assert_eq!(drift(before, after), ["LEAF x: positional order changed"]);
}

#[test]
fn identical_surfaces_have_no_drift() {
    assert!(drift(SAMPLE, SAMPLE).is_empty());
}
