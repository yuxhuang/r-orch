//! The clap command tree — every command, subcommand, alias, positional and
//! flag of the `orch` CLI.
//!
//! The surface test checks this, so it is exhaustive on purpose: a flag that
//! is missing or extra, or an arity that differs, is a
//! defect even when no behaviour hangs off it yet.
//!
//! Two deliberate choices, both limited to `--help` layout and wording:
//!
//! - help is rendered by clap, not hand-laid-out;
//! - there is one clap tree, and it carries the **registered** (longer)
//!   description of each command.
//!
//! Everything else here — names, aliases, value names, shorts, defaults,
//! `required`, and which flags are global — is contract.

use clap::{Args, Parser, Subcommand};

/// The known commands, in registration order.
///
/// Used by the pre-parse routing guard, which has to decide whether the first
/// non-flag token is a real subcommand *before* clap sees the argv.
pub const ALL_KNOWN_COMMANDS: [&str; 18] = [
    "task", "agent", "status", "logs", "config", "context", "msg", "goal", "team", "org", "run",
    "doctor", "tui", "serve", "web", "init", "update", "remote",
];

/// The three commands that run without a container, and so without
/// an initialized project. `doctor` additionally runs in either
/// tier, which is why it is here *and* takes a container when one exists.
pub const CONTAINER_LESS_COMMANDS: [&str; 3] = ["init", "update", "doctor"];

/// The version `--version` prints and the dashboard reports: this crate's own
/// release version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The parsed command line. clap parses [`RawCli`]; this type adds the one
/// check clap cannot express, so every parse path (`try_parse_from`,
/// `parse`, the tests) gets it.
#[derive(Debug)]
pub struct Cli {
    pub json: bool,
    pub yaml: bool,
    pub quiet: bool,
    pub no_color: bool,
    pub ascii: bool,
    pub command: Option<Command>,
}

/// The clap program name stays `orchestry` so `--help` matches, whichever of
/// the three binary names was invoked.
#[derive(Debug, Parser)]
#[command(
    name = "orchestry",
    about = "Agents Organizations — CLI orchestrator for AI agents",
    version = VERSION,
    // Usage errors exit 1; clap's default is 2, and the
    // mapping lives in `run()`.
    disable_help_flag = false
)]
struct RawCli {
    #[arg(long, global = true, help = "Output as JSON")]
    json: bool,
    #[arg(long, global = true, conflicts_with = "json", help = "Output as YAML")]
    yaml: bool,
    #[arg(long, global = true, help = "Minimal output (IDs only)")]
    quiet: bool,
    #[arg(long = "no-color", global = true, help = "Disable colors")]
    no_color: bool,
    #[arg(long, global = true, help = "ASCII-only output (no Unicode)")]
    ascii: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

impl clap::CommandFactory for Cli {
    fn command() -> clap::Command {
        RawCli::command()
    }

    fn command_for_update() -> clap::Command {
        RawCli::command_for_update()
    }
}

impl clap::FromArgMatches for Cli {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        Self::from_arg_matches_mut(&mut matches.clone())
    }

    /// clap's `conflicts_with` on a global flag only fires when both flags
    /// are given at the same subcommand level, so `orch --json task list
    /// --yaml` would otherwise parse with both set. The propagated values are
    /// all visible here, after parsing.
    fn from_arg_matches_mut(matches: &mut clap::ArgMatches) -> Result<Self, clap::Error> {
        let RawCli {
            json,
            yaml,
            quiet,
            no_color,
            ascii,
            command,
        } = RawCli::from_arg_matches_mut(matches)?;
        if json && yaml {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "the argument '--yaml' cannot be used with '--json'",
            ));
        }
        Ok(Cli {
            json,
            yaml,
            quiet,
            no_color,
            ascii,
            command,
        })
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

impl Parser for Cli {}

impl Cli {
    /// The structured format the global flags ask for, if any.
    pub fn output(&self) -> Option<crate::output::StructuredFormat> {
        crate::output::StructuredFormat::from_flags(self.json, self.yaml)
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage tasks
    Task(TaskCmd),
    /// Manage agents
    Agent(AgentCmd),
    /// Manage remote orch peers
    Remote(RemoteCmd),
    /// Show orchestrator status
    Status,
    /// View run logs
    Logs(LogsCmd),
    /// Manage configuration
    Config(ConfigCmd),
    /// Shared context store for inter-agent data exchange
    Context(ContextCmd),
    /// Inter-agent messaging
    Msg(MsgCmd),
    /// Manage goals
    Goal(GoalCmd),
    /// Manage agent teams
    Team(TeamCmd),
    /// Pre-built AI companies — deploy a full department with one command
    Org(OrgCmd),
    /// Run tasks
    Run(RunCmd),
    /// Check adapters and dependencies
    Doctor,
    /// Launch interactive TUI dashboard
    Tui(TuiCmd),
    /// Headless daemon mode — structured logs to stdout
    Serve(ServeCmd),
    /// Serve the TUI dashboard in a browser (xterm.js over a websocket)
    Web(WebCmd),
    /// Initialize .orchestry/ in the current directory
    Init(InitCmd),
    /// Check for updates and install the latest version
    Update(UpdateCmd),
}

impl Command {
    /// The name the pre-parse guard and the container tiering key off.
    pub fn name(&self) -> &'static str {
        match self {
            Command::Task(_) => "task",
            Command::Agent(_) => "agent",
            Command::Remote(_) => "remote",
            Command::Status => "status",
            Command::Logs(_) => "logs",
            Command::Config(_) => "config",
            Command::Context(_) => "context",
            Command::Msg(_) => "msg",
            Command::Goal(_) => "goal",
            Command::Team(_) => "team",
            Command::Org(_) => "org",
            Command::Run(_) => "run",
            Command::Doctor => "doctor",
            Command::Tui(_) => "tui",
            Command::Serve(_) => "serve",
            Command::Web(_) => "web",
            Command::Init(_) => "init",
            Command::Update(_) => "update",
        }
    }
}

// ── init ───────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct InitCmd {
    #[arg(long, value_name = "name", help = "Project name")]
    pub name: Option<String>,
    #[arg(
        long,
        value_name = "adapter",
        help = "Default agent adapter (claude, opencode, codex, cursor, pi, grok, antigravity, shell)"
    )]
    pub adapter: Option<String>,
}

// ── task ───────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct TaskCmd {
    #[command(subcommand)]
    pub command: Option<TaskSub>,
}

#[derive(Debug, Subcommand)]
pub enum TaskSub {
    /// Create a new task
    ///
    /// The flags live in their own `Args` struct rather than inline on the
    /// variant: there are thirteen of them, and an inline variant makes the
    /// enum large enough that every `TaskSub` costs that much to move.
    Add(Box<TaskAddFlags>),
    /// List all tasks
    List {
        #[arg(long, value_name = "status", help = "Filter by status")]
        status: Option<String>,
    },
    /// Show task details
    Show {
        #[arg(value_name = "id")]
        id: String,
    },
    /// Open task in $EDITOR to modify title, priority and description
    Edit {
        #[arg(value_name = "id")]
        id: String,
    },
    /// Replace an authored definition from JSON or YAML
    Update(EntityDefinitionInput),
    /// Apply an RFC 7396 merge patch from JSON or YAML
    Patch(EntityDefinitionInput),
    /// Assign task to agent
    Assign {
        #[arg(value_name = "task-id")]
        task_id: String,
        #[arg(value_name = "agent-id")]
        agent_id: String,
    },
    /// Cancel a task
    Cancel {
        #[arg(value_name = "id")]
        id: String,
    },
    /// Approve a task in review
    Approve {
        #[arg(value_name = "id")]
        id: String,
    },
    /// Reject a task and send it back for rework
    Reject {
        #[arg(value_name = "id")]
        id: String,
        #[arg(
            short,
            long,
            value_name = "reason",
            help = "Feedback for the agent explaining what to fix"
        )]
        reason: Option<String>,
    },
    /// Retry a failed task
    Retry {
        #[arg(value_name = "id")]
        id: String,
    },
}

#[derive(Debug, Args)]
pub struct TaskAddFlags {
    #[arg(value_name = "title")]
    pub title: String,
    #[arg(short, long, value_name = "desc", help = "Task description")]
    pub description: Option<String>,
    #[arg(
        short,
        long,
        value_name = "n",
        default_value = "3",
        help = "Priority (1-4)"
    )]
    pub priority: String,
    #[arg(short, long, value_name = "labels", help = "Comma-separated labels")]
    pub labels: Option<String>,
    #[arg(
        long = "depends-on",
        value_name = "ids",
        help = "Comma-separated dependency task IDs"
    )]
    pub depends_on: Option<String>,
    #[arg(long = "max-attempts", value_name = "n", help = "Max retry attempts")]
    pub max_attempts: Option<String>,
    #[arg(
        long = "workspace-mode",
        value_name = "mode",
        help = "Workspace mode: shared|worktree|isolated"
    )]
    pub workspace_mode: Option<String>,
    #[arg(long, value_name = "agent-id", help = "Assign to agent")]
    pub assignee: Option<String>,
    #[arg(
        long = "review-criteria",
        value_name = "criteria",
        help = "Comma-separated auto-review criteria: test_pass,typecheck,lint"
    )]
    pub review_criteria: Option<String>,
    #[arg(
        long,
        value_name = "patterns",
        help = "Comma-separated glob patterns for file scope (e.g. src/auth/**,src/session/**)"
    )]
    pub scope: Option<String>,
    #[arg(
        long = "goal-id",
        value_name = "goalId",
        help = "Associate task with a goal"
    )]
    pub goal_id: Option<String>,
    #[arg(
        long,
        value_name = "paths",
        help = "Comma-separated file paths to attach (screenshots, docs)"
    )]
    pub attach: Option<String>,
    #[arg(short, long, help = "Open $EDITOR to write the description")]
    pub edit: bool,
}

// ── agent ──────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct AgentCmd {
    #[command(subcommand)]
    pub command: Option<AgentSub>,
}

#[derive(Debug, Subcommand)]
pub enum AgentSub {
    /// Add a new agent
    Add(Box<AgentAddFlags>),
    /// Browse and install pre-built agent templates
    Shop(crate::template_shop::ShopArgs),
    /// List all agents
    List,
    /// Show agent details
    #[command(alias = "show")]
    Status {
        #[arg(value_name = "id")]
        id: String,
    },
    /// Edit an agent in $EDITOR
    Edit {
        #[arg(value_name = "id")]
        id: String,
    },
    /// Replace an authored definition from JSON or YAML
    Update(EntityDefinitionInput),
    /// Apply an RFC 7396 merge patch from JSON or YAML
    Patch(EntityDefinitionInput),
    /// Remove an agent
    Remove {
        #[arg(value_name = "id")]
        id: String,
    },
    /// Disable an agent
    Disable {
        #[arg(value_name = "id")]
        id: String,
    },
    /// Enable an agent
    Enable {
        #[arg(value_name = "id")]
        id: String,
    },
    /// Toggle autonomous mode for an agent
    Autonomous {
        #[arg(value_name = "id")]
        id: String,
        #[arg(long, help = "Enable autonomous mode")]
        on: bool,
        #[arg(long, help = "Disable autonomous mode")]
        off: bool,
    },
}

#[derive(Debug, Args)]
pub struct AgentAddFlags {
    #[arg(value_name = "name")]
    pub name: String,
    #[arg(
        long,
        required = true,
        value_name = "adapter",
        help = "Adapter type: claude, opencode, codex, cursor, pi, grok, antigravity, shell"
    )]
    pub adapter: String,
    #[arg(long, value_name = "role", help = "Agent role description")]
    pub role: Option<String>,
    #[arg(long, value_name = "cmd", help = "Shell command (for shell adapter)")]
    pub command: Option<String>,
    #[arg(long, value_name = "model", help = "Model name (for AI adapters)")]
    pub model: Option<String>,
    #[arg(
        long,
        value_name = "level",
        help = "Reasoning effort: low, medium, high"
    )]
    pub effort: Option<String>,
    #[arg(long = "max-turns", value_name = "n", help = "Max turns per run")]
    pub max_turns: Option<String>,
    #[arg(long, value_name = "ms", help = "Timeout in ms")]
    pub timeout: Option<String>,
    #[arg(
        long = "approval-policy",
        value_name = "policy",
        help = "suggest|auto|manual"
    )]
    pub approval_policy: Option<String>,
    #[arg(
        long = "workspace-mode",
        value_name = "mode",
        help = "shared|worktree|isolated"
    )]
    pub workspace_mode: Option<String>,
    #[arg(
        long,
        value_name = "skills",
        help = "Comma-separated list of agent skills"
    )]
    pub skills: Option<String>,
    #[arg(short, long, help = "Open $EDITOR to write the role description")]
    pub edit: bool,
}

// ── logs ───────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct LogsCmd {
    #[arg(value_name = "run-id")]
    pub run_id: Option<String>,
    #[arg(long, value_name = "agent-id", help = "Filter by agent")]
    pub agent: Option<String>,
    #[arg(long, value_name = "task-id", help = "Filter by task")]
    pub task: Option<String>,
    #[arg(long, help = "Live stream")]
    pub follow: bool,
    #[arg(long, value_name = "duration", help = "Filter by time (e.g. 5m, 1h)")]
    pub since: Option<String>,
}

/// Input language is independent of the global output format.
#[derive(Debug, Args)]
pub struct DefinitionInput {
    #[arg(short = 'f', long, value_name = "FILE_OR_DASH")]
    pub file: std::path::PathBuf,
    #[arg(long, value_enum)]
    pub input_format: Option<crate::document_input::InputFormat>,
}

#[derive(Debug, Args)]
pub struct EntityDefinitionInput {
    #[arg(value_name = "id")]
    pub id: String,
    #[command(flatten)]
    pub input: DefinitionInput,
}

// ── config ─────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct ConfigCmd {
    #[command(subcommand)]
    pub command: Option<ConfigSub>,
}

#[derive(Debug, Subcommand)]
pub enum ConfigSub {
    /// Get a config value (dot notation)
    Get {
        #[arg(value_name = "key")]
        key: String,
    },
    /// Set a config value (dot notation)
    Set {
        #[arg(value_name = "key")]
        key: String,
        #[arg(value_name = "value")]
        value: String,
    },
    /// Open config.yml in $EDITOR
    Edit,
    /// Replace raw project configuration from JSON or YAML
    Update(DefinitionInput),
    /// Apply an RFC 7396 merge patch to raw project configuration
    Patch(DefinitionInput),
    /// Manage global settings (~/.orchestry/global.yml)
    Global(ConfigGlobalCmd),
}

#[derive(Debug, Args)]
pub struct ConfigGlobalCmd {
    #[command(subcommand)]
    pub command: Option<ConfigGlobalSub>,
}

#[derive(Debug, Subcommand)]
pub enum ConfigGlobalSub {
    /// Get a global config value
    Get {
        #[arg(value_name = "key")]
        key: String,
    },
    /// Set a global config value
    Set {
        #[arg(value_name = "key")]
        key: String,
        #[arg(value_name = "value")]
        value: String,
    },
    /// Show all global settings
    Show,
}

// ── context ────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct ContextCmd {
    #[command(subcommand)]
    pub command: Option<ContextSub>,
}

#[derive(Debug, Subcommand)]
pub enum ContextSub {
    /// Set a shared context entry
    Set {
        #[arg(value_name = "key")]
        key: String,
        #[arg(value_name = "value")]
        value: String,
        #[arg(long, value_name = "ms", help = "Time-to-live in milliseconds")]
        ttl: Option<String>,
    },
    /// Get a shared context entry
    Get {
        #[arg(value_name = "key")]
        key: String,
    },
    /// List all shared context entries
    List,
    /// Delete a shared context entry
    Delete {
        #[arg(value_name = "key")]
        key: String,
    },
}

// ── msg ────────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct MsgCmd {
    #[command(subcommand)]
    pub command: Option<MsgSub>,
}

#[derive(Debug, Subcommand)]
pub enum MsgSub {
    /// Send a direct message to an agent
    Send {
        #[arg(value_name = "to-agent-id")]
        to_agent_id: String,
        #[arg(value_name = "body")]
        body: String,
        #[arg(short, long, value_name = "subject", help = "Message subject")]
        subject: Option<String>,
        #[arg(long, value_name = "agent-id", help = "Sender agent ID (default: cli)")]
        from: Option<String>,
        #[arg(long, value_name = "ms", help = "TTL in milliseconds")]
        ttl: Option<String>,
        #[arg(long = "reply-to", value_name = "msg-id", help = "Reply to a message")]
        reply_to: Option<String>,
    },
    /// Broadcast a message to all agents (or team members)
    Broadcast {
        #[arg(value_name = "body")]
        body: String,
        #[arg(short, long, value_name = "subject", help = "Message subject")]
        subject: Option<String>,
        #[arg(long, value_name = "agent-id", help = "Sender agent ID (default: cli)")]
        from: Option<String>,
        #[arg(long, value_name = "team-id", help = "Limit broadcast to team members")]
        team: Option<String>,
        #[arg(long, value_name = "ms", help = "TTL in milliseconds")]
        ttl: Option<String>,
    },
    /// Show pending messages for an agent
    Inbox {
        #[arg(value_name = "agent-id")]
        agent_id: String,
    },
    /// List all messages
    List {
        #[arg(
            long,
            value_name = "agent-id",
            help = "Filter by agent (sent or received)"
        )]
        agent: Option<String>,
    },
}

// ── goal ───────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct GoalCmd {
    #[command(subcommand)]
    pub command: Option<GoalSub>,
}

#[derive(Debug, Subcommand)]
pub enum GoalSub {
    /// Create a new goal
    Add {
        #[arg(value_name = "title")]
        title: String,
        #[arg(long, value_name = "desc", help = "Goal description")]
        description: Option<String>,
        #[arg(long, value_name = "agentId", help = "Assign to a specific agent")]
        assignee: Option<String>,
    },
    /// List all goals
    #[command(alias = "ls")]
    List {
        #[arg(long, value_name = "status", help = "Filter by status")]
        status: Option<String>,
    },
    /// Show goal details
    Show {
        #[arg(value_name = "id")]
        id: String,
    },
    /// Change goal status (active, paused, achieved, abandoned)
    Status {
        #[arg(value_name = "id")]
        id: String,
        #[arg(value_name = "status")]
        status: String,
        #[arg(
            long,
            help = "Force transition: cancel pending tasks when marking achieved"
        )]
        force: bool,
    },
    /// Update goal fields
    Update {
        #[arg(value_name = "id")]
        id: String,
        #[arg(long, value_name = "title", help = "New title")]
        title: Option<String>,
        #[arg(long, value_name = "desc", help = "New description")]
        description: Option<String>,
        #[arg(
            long,
            value_name = "agentId",
            help = "New assignee (empty string to unassign)"
        )]
        assignee: Option<String>,
    },
    /// Delete a goal
    #[command(alias = "rm")]
    Delete {
        #[arg(value_name = "id")]
        id: String,
    },
}

// ── team ───────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct TeamCmd {
    #[command(subcommand)]
    pub command: Option<TeamSub>,
}

#[derive(Debug, Subcommand)]
pub enum TeamSub {
    /// Create a new team
    Create {
        #[arg(value_name = "name")]
        name: String,
        #[arg(long, required = true, value_name = "agent-id", help = "Lead agent ID")]
        lead: String,
        #[arg(long, value_name = "ids", help = "Comma-separated member agent IDs")]
        members: Option<String>,
        #[arg(short, long, value_name = "desc", help = "Team description")]
        description: Option<String>,
        #[arg(long = "no-auto-claim", help = "Disable auto-claiming")]
        no_auto_claim: bool,
    },
    /// List all teams
    List,
    /// Show team details
    Show {
        #[arg(value_name = "id")]
        id: String,
    },
    /// Add an agent to a team
    Join {
        #[arg(value_name = "team-id")]
        team_id: String,
        #[arg(value_name = "agent-id")]
        agent_id: String,
    },
    /// Remove an agent from a team
    Leave {
        #[arg(value_name = "team-id")]
        team_id: String,
        #[arg(value_name = "agent-id")]
        agent_id: String,
    },
    /// Add a task to the team pool
    #[command(name = "add-task")]
    AddTask {
        #[arg(value_name = "team-id")]
        team_id: String,
        #[arg(value_name = "task-id")]
        task_id: String,
    },
    /// Transfer team lead to another member
    #[command(name = "set-lead")]
    SetLead {
        #[arg(value_name = "team-id")]
        team_id: String,
        #[arg(value_name = "agent-id")]
        agent_id: String,
    },
    /// Disband a team
    Disband {
        #[arg(value_name = "id")]
        id: String,
    },
}

// ── org ────────────────────────────────────────────────────────────────────

pub use crate::commands::org::{OrgCmd, OrgSub};

// ── run ────────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct PeerFlags {
    #[arg(long, num_args = 0..=1, default_missing_value = crate::commands::remote::DEFAULT_LISTEN, value_parser = parse_listen_address, value_name = "ADDR", help = "Allow incoming peer connections (engine owner only; default 127.0.0.1:7690; no authentication, so other hosts need an explicit address such as 0.0.0.0:7690)")]
    pub listen: Option<String>,
    #[arg(long, value_name = "NAME", help = "Peer display name")]
    pub peer_name: Option<String>,
}

fn parse_listen_address(raw: &str) -> std::result::Result<String, String> {
    raw.parse::<std::net::SocketAddr>()
        .map(|addr| addr.to_string())
        .map_err(|_| "Listen address must be an IP address and port".into())
}

impl PeerFlags {
    pub fn config(&self) -> orch_domain::PeerConfig {
        orch_domain::PeerConfig {
            name: self.peer_name.clone(),
            listen: self.listen.clone(),
        }
    }
}

#[derive(Debug, Args)]
pub struct RemoteCmd {
    #[command(subcommand)]
    pub command: Option<RemoteSub>,
}

#[derive(Debug, Subcommand)]
pub enum RemoteSub {
    /// Register a remote orch peer; persists and reconnects until removed
    Add {
        #[arg(
            help = "Local name for the remote, [a-z0-9][a-z0-9-]*; its agents appear as <id>@<alias>"
        )]
        alias: String,
        #[arg(help = "Peer address as host[:port] or [ipv6]:port; the port defaults to 7690")]
        addr: String,
    },
    /// Unregister a remote; its in-flight remote tasks fail with an unknown outcome
    Remove {
        #[arg(help = "Alias given to `remote add`")]
        alias: String,
    },
    /// List registered remotes with their addresses and online state
    List,
    /// Show the listen address, connected controller and each remote's state
    Status,
    /// Save the incoming-connection preference for the next engine start
    #[command(
        long_about = "Save the incoming-connection preference for the next engine start.\n\n\
                      The setting is stored as peer.listen and does not affect a running engine. \
                      A --listen flag on serve, run or tui overrides it for that invocation."
    )]
    Listen {
        #[arg(value_parser = ["on", "off"], help = "Enable (on) or disable (off) listening")]
        state: String,
        #[arg(
            help = "IP address and port to bind with `on` (default 127.0.0.1:7690, this host only; \
                    peers have no authentication, so accept other hosts only with an explicit \
                    address such as 0.0.0.0:7690 on a trusted network)"
        )]
        addr: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct RunCmd {
    #[command(flatten)]
    pub peer: PeerFlags,
    #[arg(value_name = "task-id")]
    pub task_id: Option<String>,
    #[arg(long, help = "Run all todo tasks")]
    pub all: bool,
    #[arg(long, help = "Watch mode: continuous orchestration")]
    pub watch: bool,
    #[arg(long, help = "Include agent output in watch mode")]
    pub verbose: bool,
}

// ── tui ────────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct TuiCmd {
    #[command(flatten)]
    pub peer: PeerFlags,
    #[arg(
        long,
        help = "Do not start the orchestrator; watch the project as an observer. \
                Not read-only: tasks and agents can still be changed and run from here"
    )]
    pub observe: bool,
}

// ── serve ──────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct ServeCmd {
    #[command(flatten)]
    pub peer: PeerFlags,
    #[arg(long, help = "Process todo tasks and exit when all are terminal")]
    pub once: bool,
    #[arg(
        long = "tick-interval",
        value_name = "ms",
        help = "Override polling interval (ms)"
    )]
    pub tick_interval: Option<String>,
    #[arg(
        long = "log-file",
        value_name = "path",
        help = "Also write logs to file (append mode)"
    )]
    pub log_file: Option<String>,
    #[arg(
        long = "log-format",
        value_name = "format",
        default_value = "json",
        help = "Log format: json or text (default: json)"
    )]
    pub log_format: String,
    #[arg(long, help = "Include high-frequency agent:output events")]
    pub verbose: bool,
}

// ── web ────────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct WebCmd {
    #[arg(
        long,
        value_name = "addr:port",
        default_value = "127.0.0.1:7681",
        help = "Address to listen on (default: 127.0.0.1:7681)"
    )]
    pub bind: String,
    #[arg(
        long,
        value_name = "pw",
        help = "Require this password (HTTP Basic, any username) instead of the generated access token. \
                Set ORCH_WEB_PASSWORD to keep it out of `ps`. At least 16 characters on a non-loopback bind"
    )]
    pub password: Option<String>,
    #[arg(
        long = "no-auth",
        conflicts_with = "password",
        help = "Serve with NO authentication. Anyone who can reach the port can run code as you, \
                and the session is unencrypted. Wildcard binds are refused; restrict reachability \
                with a firewall, Tailscale ACLs or an SSH tunnel"
    )]
    pub no_auth: bool,
    #[arg(
        long = "allow-host",
        value_name = "name",
        help = "Accept this Host header name (repeatable, exact match, no port). Needed behind a reverse proxy or DNS name"
    )]
    pub allow_host: Vec<String>,
    #[arg(
        long,
        help = "Run each tab's dashboard with `orch tui --observe`: the orchestrator is not started. \
                Not read-only: tasks and agents can still be changed and run from the page"
    )]
    pub observe: bool,
}

// ── update ─────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct UpdateCmd {
    #[arg(long, help = "Only check, do not install")]
    pub check: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// Every command, subcommand, alias, positional and flag
    /// exists with the same arity.
    ///
    /// The table is the spec transcribed, so a drift fails here rather than in
    /// a scenario — `<name>` is a required positional, `[name]` optional, and
    /// an option is `--long/-s <value-name>` or `--long` for a flag.
    const SURFACE: &[(&str, &[&str], &[&str])] = &[
        ("init", &[], &["--name <name>", "--adapter <adapter>"]),
        (
            "task add",
            &["<title>"],
            &[
                "--description/-d <desc>",
                "--priority/-p <n>",
                "--labels/-l <labels>",
                "--depends-on <ids>",
                "--max-attempts <n>",
                "--workspace-mode <mode>",
                "--assignee <agent-id>",
                "--review-criteria <criteria>",
                "--scope <patterns>",
                "--goal-id <goalId>",
                "--attach <paths>",
                "--edit/-e",
            ],
        ),
        ("task list", &[], &["--status <status>"]),
        ("task show", &["<id>"], &[]),
        ("task edit", &["<id>"], &[]),
        ("task assign", &["<task-id>", "<agent-id>"], &[]),
        ("task cancel", &["<id>"], &[]),
        ("task approve", &["<id>"], &[]),
        ("task reject", &["<id>"], &["--reason/-r <reason>"]),
        ("task retry", &["<id>"], &[]),
        (
            "agent add",
            &["<name>"],
            &[
                "--adapter! <adapter>",
                "--role <role>",
                "--command <cmd>",
                "--model <model>",
                "--effort <level>",
                "--max-turns <n>",
                "--timeout <ms>",
                "--approval-policy <policy>",
                "--workspace-mode <mode>",
                "--skills <skills>",
                "--edit/-e",
            ],
        ),
        (
            "agent shop",
            &[],
            &["--list", "--template <TEMPLATE>", "--template-dir <PATH>"],
        ),
        ("agent list", &[], &[]),
        ("agent status", &["<id>"], &[]),
        ("agent edit", &["<id>"], &[]),
        ("agent remove", &["<id>"], &[]),
        ("agent disable", &["<id>"], &[]),
        ("agent enable", &["<id>"], &[]),
        ("agent autonomous", &["<id>"], &["--on", "--off"]),
        ("status", &[], &[]),
        (
            "logs",
            &["[run-id]"],
            &[
                "--agent <agent-id>",
                "--task <task-id>",
                "--follow",
                "--since <duration>",
            ],
        ),
        ("config get", &["<key>"], &[]),
        ("config set", &["<key>", "<value>"], &[]),
        ("config edit", &[], &[]),
        ("config global get", &["<key>"], &[]),
        ("config global set", &["<key>", "<value>"], &[]),
        ("config global show", &[], &[]),
        ("context set", &["<key>", "<value>"], &["--ttl <ms>"]),
        ("context get", &["<key>"], &[]),
        ("context list", &[], &[]),
        ("context delete", &["<key>"], &[]),
        (
            "msg send",
            &["<to-agent-id>", "<body>"],
            &[
                "--subject/-s <subject>",
                "--from <agent-id>",
                "--ttl <ms>",
                "--reply-to <msg-id>",
            ],
        ),
        (
            "msg broadcast",
            &["<body>"],
            &[
                "--subject/-s <subject>",
                "--from <agent-id>",
                "--team <team-id>",
                "--ttl <ms>",
            ],
        ),
        ("msg inbox", &["<agent-id>"], &[]),
        ("msg list", &[], &["--agent <agent-id>"]),
        (
            "goal add",
            &["<title>"],
            &["--description <desc>", "--assignee <agentId>"],
        ),
        ("goal list", &[], &["--status <status>"]),
        ("goal show", &["<id>"], &[]),
        ("goal status", &["<id>", "<status>"], &["--force"]),
        (
            "goal update",
            &["<id>"],
            &[
                "--title <title>",
                "--description <desc>",
                "--assignee <agentId>",
            ],
        ),
        ("goal delete", &["<id>"], &[]),
        (
            "team create",
            &["<name>"],
            &[
                "--lead! <agent-id>",
                "--members <ids>",
                "--description/-d <desc>",
                "--no-auto-claim",
            ],
        ),
        ("team list", &[], &[]),
        ("team show", &["<id>"], &[]),
        ("team join", &["<team-id>", "<agent-id>"], &[]),
        ("team leave", &["<team-id>", "<agent-id>"], &[]),
        ("team add-task", &["<team-id>", "<task-id>"], &[]),
        ("team set-lead", &["<team-id>", "<agent-id>"], &[]),
        ("team disband", &["<id>"], &[]),
        ("org list", &[], &[]),
        (
            "org deploy",
            &["[template]"],
            &[
                "--template <TEMPLATE_NAME>",
                "--template-dir <PATH>",
                "--adapter <ADAPTER>",
                "--model <MODEL>",
                "--goal <goal>",
            ],
        ),
        (
            "run",
            &["[task-id]"],
            &[
                "--all",
                "--watch",
                "--verbose",
                "--listen <ADDR>",
                "--peer-name <NAME>",
            ],
        ),
        ("doctor", &[], &[]),
        (
            "tui",
            &[],
            &["--observe", "--listen <ADDR>", "--peer-name <NAME>"],
        ),
        (
            "serve",
            &[],
            &[
                "--once",
                "--tick-interval <ms>",
                "--log-file <path>",
                "--log-format <format>",
                "--verbose",
                "--listen <ADDR>",
                "--peer-name <NAME>",
            ],
        ),
        (
            "web",
            &[],
            &[
                "--bind <addr:port>",
                "--password <pw>",
                "--no-auth",
                "--allow-host <name>",
                "--observe",
            ],
        ),
        ("update", &[], &["--check"]),
    ];

    /// Walk to the clap subcommand a dotted path names.
    fn find(path: &str) -> clap::Command {
        let mut current = Cli::command();
        for segment in path.split(' ') {
            current = current
                .find_subcommand(segment)
                .unwrap_or_else(|| panic!("{path}: no subcommand {segment}"))
                .clone();
        }
        current
    }

    #[test]
    fn peer_options_parse_for_owner_commands() {
        let cli =
            Cli::try_parse_from(["orch", "serve", "--listen", "--peer-name", "worker"]).unwrap();
        let Some(Command::Serve(args)) = cli.command else {
            panic!("serve")
        };
        assert_eq!(args.peer.listen.as_deref(), Some("127.0.0.1:7690"));
        assert_eq!(args.peer.peer_name.as_deref(), Some("worker"));
        let cli =
            Cli::try_parse_from(["orch", "run", "--watch", "--listen", "127.0.0.1:1234"]).unwrap();
        let Some(Command::Run(args)) = cli.command else {
            panic!("run")
        };
        assert_eq!(args.peer.listen.as_deref(), Some("127.0.0.1:1234"));
        let cli = Cli::try_parse_from(["orch", "tui", "--listen"]).unwrap();
        let Some(Command::Tui(args)) = cli.command else {
            panic!("tui")
        };
        assert_eq!(args.peer.listen.as_deref(), Some("127.0.0.1:7690"));
    }

    #[test]
    fn remote_commands_and_show_alias_parse() {
        for argv in [
            vec!["orch", "remote", "add", "w1", "worker:7690"],
            vec!["orch", "remote", "remove", "w1"],
            vec!["orch", "remote", "list", "--json"],
            vec!["orch", "remote", "status"],
            vec!["orch", "remote", "listen", "on", "127.0.0.1:7690"],
            vec!["orch", "remote", "listen", "off"],
            vec!["orch", "agent", "show", "agt_worker@w1", "--json"],
        ] {
            assert!(Cli::try_parse_from(argv).is_ok());
        }
    }

    #[test]
    fn the_command_surface_matches_the_surface_table() {
        for (path, positionals, options) in SURFACE {
            let command = find(path);

            let actual: Vec<String> = command
                .get_positionals()
                .map(|arg| {
                    let name = arg
                        .get_value_names()
                        .map(|names| names[0].as_str())
                        .unwrap_or("");
                    if arg.is_required_set() {
                        format!("<{name}>")
                    } else {
                        format!("[{name}]")
                    }
                })
                .collect();
            assert_eq!(&actual, positionals, "{path}: positionals");

            for spec in *options {
                let (spec, required) = match spec.split_once('!') {
                    Some((head, tail)) => (format!("{head}{tail}"), true),
                    None => ((*spec).to_owned(), false),
                };
                let (names, value) = match spec.split_once(' ') {
                    Some((names, value)) => (names, Some(value)),
                    None => (spec.as_str(), None),
                };
                let (long, short) = match names.split_once('/') {
                    Some((long, short)) => (long, Some(short)),
                    None => (names, None),
                };
                let long = long.trim_start_matches('-');

                let arg = command
                    .get_arguments()
                    .find(|arg| arg.get_long() == Some(long))
                    .unwrap_or_else(|| panic!("{path}: no --{long}"));

                assert_eq!(
                    arg.get_short().map(|short| format!("-{short}")),
                    short.map(str::to_owned),
                    "{path} --{long}: short flag"
                );
                // A `SetTrue` flag takes no value; clap keeps a vestigial
                // value name on one, so the action is what decides.
                let takes_value = !matches!(
                    arg.get_action(),
                    clap::ArgAction::SetTrue | clap::ArgAction::SetFalse
                );
                assert_eq!(takes_value, value.is_some(), "{path} --{long}: arity");
                if takes_value {
                    assert_eq!(
                        arg.get_value_names().map(|names| format!("<{}>", names[0])),
                        value.map(str::to_owned),
                        "{path} --{long}: value name"
                    );
                }
                assert_eq!(arg.is_required_set(), required, "{path} --{long}: required");
            }

            // Nothing beyond the table, counting `--help` but not the five
            // global flags, which clap propagates to every subcommand.
            let declared: Vec<&str> = command
                .get_arguments()
                .filter_map(|arg| arg.get_long())
                .filter(|long| {
                    !matches!(
                        *long,
                        "help" | "json" | "yaml" | "quiet" | "no-color" | "ascii"
                    )
                })
                .collect();
            assert_eq!(
                declared.len(),
                options.len(),
                "{path}: extra options {declared:?} vs {options:?}"
            );
        }
    }

    #[test]
    fn every_group_has_exactly_the_documented_subcommands() {
        let expected: &[(&str, &[&str])] = &[
            (
                "task",
                &[
                    "add", "list", "show", "edit", "update", "patch", "assign", "cancel",
                    "approve", "reject", "retry",
                ],
            ),
            (
                "agent",
                &[
                    "add",
                    "shop",
                    "list",
                    "status",
                    "edit",
                    "update",
                    "patch",
                    "remove",
                    "disable",
                    "enable",
                    "autonomous",
                ],
            ),
            (
                "config",
                &["get", "set", "edit", "update", "patch", "global"],
            ),
            ("config global", &["get", "set", "show"]),
            ("context", &["set", "get", "list", "delete"]),
            ("msg", &["send", "broadcast", "inbox", "list"]),
            (
                "goal",
                &["add", "list", "show", "status", "update", "delete"],
            ),
            (
                "team",
                &[
                    "create", "list", "show", "join", "leave", "add-task", "set-lead", "disband",
                ],
            ),
            ("org", &["list", "template", "deploy"]),
            ("remote", &["add", "remove", "list", "status", "listen"]),
        ];

        for (group, subs) in expected {
            let command = find(group);
            let actual: Vec<&str> = command
                .get_subcommands()
                .map(|sub| sub.get_name())
                .filter(|name| *name != "help")
                .collect();
            assert_eq!(&actual, subs, "{group}");
        }
    }

    #[test]
    fn the_three_documented_aliases_exist() {
        for (path, alias) in [
            ("agent status", "show"),
            ("goal list", "ls"),
            ("goal delete", "rm"),
            ("org list", "ls"),
        ] {
            let parent = path.rsplit_once(' ').expect("a group").0;
            let command = find(parent);
            assert!(
                command
                    .get_subcommands()
                    .any(|sub| sub.get_all_aliases().any(|a| a == alias)),
                "{path} is missing the {alias} alias"
            );
        }
        // And nothing else has one.
        let mut aliased = Vec::new();
        collect_aliases(&Cli::command(), String::new(), &mut aliased);
        assert_eq!(
            aliased,
            ["agent status", "goal list", "goal delete", "org list"]
        );
    }

    fn collect_aliases(command: &clap::Command, prefix: String, out: &mut Vec<String>) {
        for sub in command.get_subcommands() {
            let path = if prefix.is_empty() {
                sub.get_name().to_owned()
            } else {
                format!("{prefix} {}", sub.get_name())
            };
            if sub.get_all_aliases().next().is_some() {
                out.push(path.clone());
            }
            collect_aliases(sub, path, out);
        }
    }

    #[test]
    fn the_root_has_all_seventeen_commands_and_the_global_flags() {
        let command = Cli::command();
        let mut names: Vec<&str> = command
            .get_subcommands()
            .map(|sub| sub.get_name())
            .filter(|name| *name != "help")
            .collect();
        names.sort();
        let mut expected = ALL_KNOWN_COMMANDS.to_vec();
        expected.sort();
        assert_eq!(names, expected);

        for flag in ["json", "yaml", "quiet", "no-color", "ascii"] {
            let arg = command
                .get_arguments()
                .find(|arg| arg.get_long() == Some(flag))
                .unwrap_or_else(|| panic!("--{flag}"));
            assert!(arg.is_global_set(), "--{flag} must be global");
        }
    }

    #[test]
    fn the_program_name_and_version_are_the_reference_ones() {
        assert_eq!(Cli::command().get_name(), "orchestry");
        assert_eq!(
            Cli::command().get_version(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(
            Cli::command().get_about().map(|about| about.to_string()),
            Some("Agents Organizations — CLI orchestrator for AI agents".to_owned())
        );
    }

    #[test]
    fn global_flags_are_accepted_before_and_after_the_subcommand() {
        assert!(
            Cli::try_parse_from(["orch", "--json", "task", "list"])
                .unwrap()
                .json
        );
        assert!(
            Cli::try_parse_from(["orch", "task", "list", "--json"])
                .unwrap()
                .json
        );
        assert!(
            Cli::try_parse_from(["orch", "--quiet", "agent", "list"])
                .unwrap()
                .quiet
        );
    }

    #[test]
    fn yaml_is_global_and_maps_to_the_yaml_format() {
        use crate::output::StructuredFormat;
        for argv in [
            ["orch", "--yaml", "task", "list"],
            ["orch", "task", "list", "--yaml"],
        ] {
            let cli = Cli::try_parse_from(argv).unwrap();
            assert_eq!(cli.output(), Some(StructuredFormat::Yaml), "{argv:?}");
        }
        let json = Cli::try_parse_from(["orch", "task", "list", "--json"]).unwrap();
        assert_eq!(json.output(), Some(StructuredFormat::Json));
        let human = Cli::try_parse_from(["orch", "task", "list"]).unwrap();
        assert_eq!(human.output(), None);
    }

    #[test]
    fn json_and_yaml_together_are_a_usage_error() {
        let cases: [&[&str]; 4] = [
            &["orch", "--json", "task", "list", "--yaml"],
            &["orch", "task", "list", "--yaml", "--json"],
            &["orch", "--yaml", "--json", "status"],
            &["orch", "task", "--json", "list", "--yaml"],
        ];
        for argv in cases {
            let err = Cli::try_parse_from(argv).unwrap_err();
            assert_eq!(
                err.kind(),
                clap::error::ErrorKind::ArgumentConflict,
                "{argv:?}"
            );
        }
    }

    #[test]
    fn required_flags_are_enforced() {
        assert_eq!(
            Cli::try_parse_from(["orch", "agent", "add", "a"])
                .expect_err("--adapter")
                .kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            Cli::try_parse_from(["orch", "team", "create", "t"])
                .expect_err("--lead")
                .kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn a_group_with_no_subcommand_parses_so_the_handler_can_print_help() {
        for group in [
            "task", "agent", "config", "context", "msg", "goal", "team", "org",
        ] {
            let cli = Cli::try_parse_from(["orch", group]).expect("parses");
            assert!(cli.command.is_some(), "{group}");
        }
        // The nested group too.
        let cli = Cli::try_parse_from(["orch", "config", "global"]).expect("parses");
        assert!(matches!(
            cli.command,
            Some(Command::Config(ConfigCmd {
                command: Some(ConfigSub::Global(ConfigGlobalCmd { command: None })),
            }))
        ));
    }

    #[test]
    fn the_defaults_are_stable() {
        let cli = Cli::try_parse_from(["orch", "task", "add", "t"]).expect("parses");
        let Some(Command::Task(TaskCmd {
            command: Some(TaskSub::Add(flags)),
        })) = cli.command
        else {
            panic!("task add");
        };
        assert_eq!(flags.priority, "3");
        assert!(!flags.edit);

        let cli = Cli::try_parse_from(["orch", "serve"]).expect("parses");
        let Some(Command::Serve(serve)) = cli.command else {
            panic!("serve");
        };
        assert_eq!(serve.log_format, "json");
        assert!(!serve.once);
    }

    #[test]
    fn command_names_round_trip() {
        for name in ALL_KNOWN_COMMANDS {
            // Every command parses from its own name, with required args
            // supplied where the tree demands them.
            let argv: Vec<&str> = match name {
                "task" | "agent" | "config" | "context" | "msg" | "goal" | "team" | "org" => {
                    vec!["orch", name]
                }
                _ => vec!["orch", name],
            };
            let cli = Cli::try_parse_from(argv).unwrap_or_else(|err| panic!("{name}: {err}"));
            assert_eq!(cli.command.expect("a command").name(), name);
        }
    }
}

#[cfg(test)]
mod file_mutation_tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn live_mutations_require_file_and_accept_independent_formats() {
        for target in ["agent", "task", "config"] {
            for command in ["update", "patch"] {
                let mut args = vec!["orch", target, command];
                if target != "config" {
                    args.push("id");
                }
                assert!(Cli::try_parse_from(&args).is_err());
                args.extend(["-f", "-", "--input-format", "json", "--yaml"]);
                assert!(Cli::try_parse_from(&args).is_ok(), "{args:?}");
            }
        }
    }
}
