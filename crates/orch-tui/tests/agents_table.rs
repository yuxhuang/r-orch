//! The Agents list is a table like Goals and Tasks (goal `goal_DHCHB3O`,
//! requirements R1-R5).
//!
//! These cases are written FIRST and are expected to fail (to compile, while
//! `AgentSortField` has no `Task`/`Caps`/`Runs`/`Updated`, and
//! `AgentGroupBy` no `Status`/`Adapter`/`Caps`/`None` or `prev`). The names
//! used are the fixed contract names of plan.md "Approved deltas":
//!
//! * `AgentSortField { Status (default), Name, AdapterModel, Task, Caps,
//!   Location, Runs, Updated }`, `o` walks that order; the header labels are
//!   `STATUS NAME ADAPTER/MODEL TASK CAPS WHERE RUNS UPDATED`;
//! * `AgentGroupBy { Team (default), Node, Status, Adapter, Caps, None }`,
//!   `b` cycles forward, `B` backward; a `GROUP: <Mode>` chip sits at the
//!   right end of the header row and is clickable; there is no chip row above
//!   the header;
//! * sorting is stable, ties fall back to index (source) order in both
//!   directions, and blanks (no task, no caps, no runs, no timestamp) sort
//!   last in both directions;
//! * section rows (`◈ LABEL · n agents`) per mode, empty sections are not
//!   drawn, `None` draws none, Status sections run ACTIVE, IDLE, ERROR,
//!   OFFLINE, DISABLED, Adapter and Caps sections are alphabetical.
//!
//! Only the public API of `orch-tui` and the rendered frame are used, so the
//! cases survive any choice of internal seams. Section rows are recognised by
//! their shape (`label · N agent(s)`), not by glyph or by a private row enum.
//! Every case runs on the fake backend with a held clock and touches no home
//! directory, network or AI CLI.
//!
//! Left to the design (no case pins it, see the module-end notes): where the
//! UPDATED value comes from (D1), and how an agent with several capabilities
//! is placed under Caps grouping (D2).

mod common;

use common::Harness;
use orch_domain::{Agent, AgentStatus, RemoteRef, TaskStatus};
use orch_tui::backend::{FakeBackend, make_agent, make_task, make_team};
use orch_tui::hit::ZoneKind;
use orch_tui::state::{
    AgentGroupBy, // contract: plan.md Approved deltas (Status, Adapter, Caps, None, prev)
    AgentSort,
    AgentSortField, // contract: plan.md Approved deltas (Task, Caps, Runs, Updated)
    SortColumn,
    ViewId,
};
use unicode_width::UnicodeWidthStr;

// ── Contract tables ──────────────────────────────────────────────────────────

/// The sort fields in `o` order with their header labels.
const FIELDS: [(AgentSortField, &str); 8] = [
    (AgentSortField::Status, "STATUS"),
    (AgentSortField::Name, "NAME"),
    (AgentSortField::AdapterModel, "ADAPTER/MODEL"),
    (AgentSortField::Task, "TASK"), // contract: plan.md Approved deltas
    (AgentSortField::Caps, "CAPS"), // contract: plan.md Approved deltas
    (AgentSortField::Location, "WHERE"),
    (AgentSortField::Runs, "RUNS"), // contract: plan.md Approved deltas
    (AgentSortField::Updated, "UPDATED"), // contract: plan.md Approved deltas
];

/// The grouping modes in `b` order with the word the chip shows.
const MODES: [(AgentGroupBy, &str); 6] = [
    (AgentGroupBy::Team, "Team"),
    (AgentGroupBy::Node, "Node"),
    (AgentGroupBy::Status, "Status"), // contract: plan.md Approved deltas
    (AgentGroupBy::Adapter, "Adapter"), // contract: plan.md Approved deltas
    (AgentGroupBy::Caps, "Caps"),     // contract: plan.md Approved deltas
    (AgentGroupBy::None, "None"),     // contract: plan.md Approved deltas
];

/// Wide enough that every column and the group chip show.
const WIDE: u16 = 160;
const HIGH: u16 = 30;

fn default_sort() -> AgentSort {
    AgentSort {
        field: AgentSortField::Status,
        descending: false,
    }
}

// ── Frame helpers ────────────────────────────────────────────────────────────

/// The screen row of the agents table header: the first row naming both
/// `STATUS` and `NAME`.
#[track_caller]
fn header_y(h: &mut Harness) -> u16 {
    let text = h.text();
    text.lines()
        .position(|l| l.contains("STATUS") && l.contains("NAME"))
        .unwrap_or_else(|| panic!("no agents table header on screen:\n{text}")) as u16
}

#[track_caller]
fn header_line(h: &mut Harness) -> String {
    let y = header_y(h);
    h.line(y)
}

/// Display column of `needle` in `line` (cells, not bytes: rows carry `▸ ⠋ ★`).
fn col_of(line: &str, needle: &str) -> Option<u16> {
    line.find(needle)
        .map(|byte| UnicodeWidthStr::width(&line[..byte]) as u16)
}

/// Display column one past the end of `needle` in `line`.
fn end_col_of(line: &str, needle: &str) -> Option<u16> {
    col_of(line, needle).map(|c| c + UnicodeWidthStr::width(needle) as u16)
}

/// The cell of a header label, for clicking and zone lookups. `ADAPTER/MODEL`
/// may be cut by a narrow column, so its first seven characters stand for it.
#[track_caller]
fn label_cell(h: &mut Harness, label: &str) -> (u16, u16) {
    let key = if label == "ADAPTER/MODEL" {
        "ADAPTER"
    } else {
        label
    };
    let y = header_y(h);
    let line = h.line(y);
    let x =
        col_of(&line, key).unwrap_or_else(|| panic!("{label} is not in the header row {line:?}"));
    (x, y)
}

#[track_caller]
fn click_header(h: &mut Harness, label: &str) {
    let (x, y) = label_cell(h, label);
    h.click(x, y);
}

fn ids(h: &Harness) -> Vec<&str> {
    h.app
        .state
        .sorted_agents()
        .into_iter()
        .map(|i| h.app.state.agents[i].id.as_str())
        .collect()
}

#[track_caller]
fn selected_id(h: &Harness) -> String {
    h.app
        .state
        .selected_agent()
        .unwrap_or_else(|| panic!("no agent is selected"))
        .id
        .clone()
}

/// A section row is `label · N agent(s)`, possibly framed by rules and a
/// glyph (`─── ◈ ALPHA · 2 agents · ★ lead ───`).
fn section_of(line: &str) -> Option<(String, usize)> {
    let at = line.find(" · ")?;
    let rest = &line[at + " · ".len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || !rest[digits.len()..].starts_with(" agent") {
        return None;
    }
    let label = line[..at]
        .trim_start_matches(|c: char| "─ ◈◇".contains(c))
        .trim()
        .to_lowercase();
    Some((label, digits.parse().ok()?))
}

/// The fixture names every agent `Agx-NN`; a row is any line holding one.
fn agent_in(line: &str) -> Option<String> {
    let at = line.find("Agx-")?;
    Some(line[at..].chars().take(6).collect())
}

/// The frame as `(lowercase section label, agent names in row order)`. Agent
/// rows before any section row (grouping `None`) land under `""`. A section
/// header whose count is not the number of rows under it fails here.
#[track_caller]
fn grouped(text: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, usize, Vec<String>)> = Vec::new();
    for line in text.lines() {
        if let Some((label, count)) = section_of(line) {
            out.push((label, count, Vec::new()));
        } else if let Some(name) = agent_in(line) {
            if out.is_empty() {
                out.push((String::new(), usize::MAX, Vec::new()));
            }
            out.last_mut().unwrap().2.push(name);
        }
    }
    for (label, count, agents) in &out {
        assert!(
            *count == usize::MAX || *count == agents.len(),
            "section {label:?} says {count} agents but {} rows follow:\n{text}",
            agents.len()
        );
    }
    out.into_iter().map(|(l, _, a)| (l, a)).collect()
}

fn expected(sections: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
    sections
        .iter()
        .map(|(l, a)| (l.to_string(), a.iter().map(|n| n.to_string()).collect()))
        .collect()
}

// ── Fixtures ─────────────────────────────────────────────────────────────────

/// Agents `i0..` whose ids AND names run against index order (`id9, id8, ...`,
/// `n9, n8, ...`), so an order that falls back to name or id instead of index
/// order shows. All Idle, no task, no caps, local, no runs, until `edit`.
fn trap_agents(n: usize, edit: impl Fn(usize, &mut Agent)) -> Vec<Agent> {
    (0..n)
        .map(|i| {
            let mut a = make_agent(
                &format!("id{}", 9 - i),
                &format!("n{}", 9 - i),
                AgentStatus::Idle,
            );
            edit(i, &mut a);
            a
        })
        .collect()
}

fn remote(alias: &str, online: bool) -> Option<RemoteRef> {
    Some(RemoteRef {
        alias: alias.into(),
        online,
    })
}

fn agents_view(backend: FakeBackend, width: u16, height: u16) -> Harness {
    let mut h = Harness::sized(backend, width, height);
    h.app.state.view = ViewId::Agents;
    h
}

/// Six agents covering every status but one, two adapters... Names are
/// `Agx-00..05`; indexes equal the number in the name.
///
/// | agent  | status   | adapter | caps    | node             | team  |
/// |--------|----------|---------|---------|------------------|-------|
/// | Agx-00 | Idle     | claude  | web     | local            | Alpha (lead) |
/// | Agx-01 | Running  | codex   | macos   | wa online        | Alpha |
/// | Agx-02 | Error    | shell   | -       | wb offline       | -     |
/// | Agx-03 | Offline  | claude  | macos   | local            | -     |
/// | Agx-04 | Disabled | codex   | linux   | wa online        | Beta (lead) |
/// | Agx-05 | Running  | claude  | web     | local            | -     |
///
/// Offline and Disabled never share a section here (their relative STATUS
/// sort rank is a design question, see the notes at the end of the file).
fn world_agents() -> Vec<Agent> {
    let rows: [(AgentStatus, &str, &[&str], Option<RemoteRef>); 6] = [
        (AgentStatus::Idle, "claude", &["web"], None),
        (
            AgentStatus::Running,
            "codex",
            &["macos"],
            remote("wa", true),
        ),
        (AgentStatus::Error, "shell", &[], remote("wb", false)),
        (AgentStatus::Offline, "claude", &["macos"], None),
        (
            AgentStatus::Disabled,
            "codex",
            &["linux"],
            remote("wa", true),
        ),
        (AgentStatus::Running, "claude", &["web"], None),
    ];
    rows.into_iter()
        .enumerate()
        .map(|(i, (status, adapter, caps, remote))| {
            let mut a = make_agent(&format!("a{i}"), &format!("Agx-{i:02}"), status);
            a.adapter = adapter.into();
            a.capabilities = caps.iter().map(|c| c.to_string()).collect();
            a.remote = remote;
            a
        })
        .collect()
}

fn world_backend() -> FakeBackend {
    FakeBackend::new()
        .with_agents(world_agents())
        .with_teams(vec![
            make_team("t_alpha", "Alpha", "a0", &["a0", "a1"]),
            make_team("t_beta", "Beta", "a4", &["a4"]),
        ])
}

fn world(width: u16) -> Harness {
    agents_view(world_backend(), width, HIGH)
}

/// The sections each mode draws over [`world_agents`], agents in NAME order
/// (the order a name-ascending sort gives inside every section).
fn world_sections(mode: AgentGroupBy) -> Vec<(&'static str, Vec<&'static str>)> {
    match mode {
        AgentGroupBy::Team => vec![
            ("alpha", vec!["Agx-00", "Agx-01"]),
            ("beta", vec!["Agx-04"]),
            ("unassigned", vec!["Agx-02", "Agx-03", "Agx-05"]),
        ],
        AgentGroupBy::Node => vec![
            ("local", vec!["Agx-00", "Agx-03", "Agx-05"]),
            ("wa ● online", vec!["Agx-01", "Agx-04"]),
            ("wb ○ offline", vec!["Agx-02"]),
        ],
        AgentGroupBy::Status => vec![
            ("active", vec!["Agx-01", "Agx-05"]),
            ("idle", vec!["Agx-00"]),
            ("error", vec!["Agx-02"]),
            ("offline", vec!["Agx-03"]),
            ("disabled", vec!["Agx-04"]),
        ],
        AgentGroupBy::Adapter => vec![
            ("claude", vec!["Agx-00", "Agx-03", "Agx-05"]),
            ("codex", vec!["Agx-01", "Agx-04"]),
            ("shell", vec!["Agx-02"]),
        ],
        AgentGroupBy::Caps => vec![
            ("linux", vec!["Agx-04"]),
            ("macos", vec!["Agx-01", "Agx-03"]),
            ("web", vec!["Agx-00", "Agx-05"]),
            ("no caps", vec!["Agx-02"]),
        ],
        AgentGroupBy::None => vec![(
            "",
            vec!["Agx-00", "Agx-01", "Agx-02", "Agx-03", "Agx-04", "Agx-05"],
        )],
    }
}

fn set_mode(h: &mut Harness, mode: AgentGroupBy) {
    h.app.state.set_agent_group_by(mode);
}

fn set_sort(h: &mut Harness, field: AgentSortField, descending: bool) {
    h.app.state.set_agent_sort(AgentSort { field, descending });
}

// ═════════════════════════════════════════════════════════════════════════════
// R1 - one header row, every column sortable, arrows, no chip row
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn r1_default_sort_is_status_ascending_not_original_order() {
    assert_eq!(AgentSort::default(), default_sort());
    // Statuses scrambled against index order; ties keep index order.
    let statuses = [
        AgentStatus::Error,
        AgentStatus::Idle,
        AgentStatus::Running,
        AgentStatus::Offline,
        AgentStatus::Idle,
        AgentStatus::Running,
    ];
    let agents = trap_agents(6, |i, a| a.status = statuses[i]);
    let h = agents_view(FakeBackend::new().with_agents(agents), WIDE, HIGH);
    // Running (i2, i5), Idle (i1, i4), Error (i0), Offline (i3).
    assert_eq!(ids(&h), ["id7", "id4", "id8", "id5", "id9", "id6"]);
}

#[test]
fn r1_header_names_all_eight_columns_in_order() {
    let mut h = world(WIDE);
    let line = header_line(&mut h);
    let mut at = 0;
    for (_, label) in FIELDS {
        let key = if label == "ADAPTER/MODEL" {
            "ADAPTER"
        } else {
            label
        };
        let c = col_of(&line, key).unwrap_or_else(|| panic!("{label} missing from {line:?}"));
        assert!(c >= at, "{label} is out of order in {line:?}");
        at = c;
    }
}

#[test]
fn r1_the_active_column_carries_an_up_arrow_by_default() {
    let mut h = world(WIDE);
    let line = header_line(&mut h);
    assert!(line.contains("STATUS↑"), "{line:?}");
    assert_eq!(
        line.matches('↑').count() + line.matches('↓').count(),
        1,
        "{line:?}"
    );
}

#[test]
fn r1_o_moves_the_arrow_across_every_column_in_contract_order_and_wraps() {
    let mut h = world(WIDE);
    for step in 1..=FIELDS.len() {
        h.key('o');
        let (field, label) = FIELDS[step % FIELDS.len()];
        assert_eq!(h.app.state.agent_sort.field, field, "after {step} x o");
        let line = header_line(&mut h);
        let key = if label == "ADAPTER/MODEL" {
            "ADAPTER/MODEL"
        } else {
            label
        };
        assert!(
            line.contains(&format!("{key}↑")),
            "after {step} x o: {line:?}"
        );
        assert_eq!(
            line.matches('↑').count() + line.matches('↓').count(),
            1,
            "{line:?}"
        );
    }
    assert_eq!(h.app.state.agent_sort, default_sort());
}

#[test]
fn r1_capital_o_flips_the_arrow_down_and_back() {
    let mut h = world(WIDE);
    h.key('O');
    assert!(header_line(&mut h).contains("STATUS↓"));
    h.key('O');
    assert!(header_line(&mut h).contains("STATUS↑"));
}

#[test]
fn r1_zero_resets_any_sort_to_status_ascending() {
    let mut h = world(WIDE);
    h.key('o').key('o').key('o').key('O');
    assert_ne!(h.app.state.agent_sort, default_sort());
    h.key('0');
    assert_eq!(h.app.state.agent_sort, default_sort());
    assert!(header_line(&mut h).contains("STATUS↑"));
}

#[test]
fn r1_zero_resets_the_sort_but_not_the_grouping() {
    let mut h = world(WIDE);
    h.key('b').key('b').key('o').key('O');
    h.key('0');
    assert_eq!(h.app.state.agent_group_by, AgentGroupBy::Status);
}

#[test]
fn r1_clicking_any_header_sorts_by_it_ascending_then_flips() {
    for (field, label) in FIELDS {
        let mut h = world(WIDE);
        click_header(&mut h, label);
        let first = h.app.state.agent_sort;
        // The default column starts ascending, so a first click flips it.
        let expect_desc = field == AgentSortField::Status;
        assert_eq!(
            first,
            AgentSort {
                field,
                descending: expect_desc
            },
            "first click on {label}"
        );
        click_header(&mut h, label);
        assert_eq!(
            h.app.state.agent_sort,
            AgentSort {
                field,
                descending: !expect_desc
            },
            "second click on {label}"
        );
    }
}

#[test]
fn r1_every_sortable_header_cell_is_a_sort_header_zone() {
    let mut h = world(WIDE);
    for (field, label) in FIELDS {
        let (x, y) = label_cell(&mut h, label);
        assert_eq!(
            h.zone_at(x, y).map(|z| z.kind.clone()),
            Some(ZoneKind::SortHeader(SortColumn::Agent(field))),
            "{label}"
        );
    }
}

#[test]
fn r1_there_is_no_chip_row_above_the_header() {
    let mut h = world(WIDE);
    let text = h.text();
    for old in [
        "o sort",
        "O asc",
        "O desc",
        "0 reset",
        "b group:",
        "sort:Original",
    ] {
        assert!(!text.contains(old), "{old:?} is still drawn:\n{text}");
    }
}

#[test]
fn r1_the_header_sits_on_the_same_row_as_the_tasks_table_header() {
    // Same pane geometry; the Agents list used to start one row lower
    // because of its chip row.
    let mut agents = world(WIDE);
    let mut tasks = Harness::sized(
        FakeBackend::new().with_tasks(vec![make_task("t1", "one", TaskStatus::Todo)]),
        WIDE,
        HIGH,
    );
    tasks.app.state.view = ViewId::Tasks;
    let tasks_y = {
        let text = tasks.text();
        text.lines()
            .position(|l| l.contains("STATUS") && l.contains("TITLE"))
            .expect("tasks header") as u16
    };
    assert_eq!(header_y(&mut agents), tasks_y);
}

#[test]
fn r1_the_first_list_row_is_directly_under_the_header() {
    let mut h = world(WIDE);
    set_mode(&mut h, AgentGroupBy::None);
    let y = header_y(&mut h);
    assert!(
        h.line(y + 1).contains("Agx-"),
        "row under the header: {:?}",
        h.line(y + 1)
    );
}

#[test]
fn r1_the_row_freed_by_the_chip_row_shows_one_more_agent() {
    // Same pane height, grouping None (no section row): the Agents list must
    // show as many rows as the Tasks list does for the same count.
    fn visible(prefix: &str, h: &mut Harness) -> usize {
        h.text().lines().filter(|l| l.contains(prefix)).count()
    }
    let agents: Vec<Agent> = (0..60)
        .map(|i| make_agent(&format!("a{i}"), &format!("Row-{i:02}"), AgentStatus::Idle))
        .collect();
    let tasks = (0..60)
        .map(|i| make_task(&format!("t{i}"), &format!("Row-{i:02}"), TaskStatus::Todo))
        .collect();
    let mut a = agents_view(FakeBackend::new().with_agents(agents), 100, 20);
    set_mode(&mut a, AgentGroupBy::None);
    let mut t = Harness::sized(FakeBackend::new().with_tasks(tasks), 100, 20);
    t.app.state.view = ViewId::Tasks;
    assert_eq!(visible("Row-", &mut a), visible("Row-", &mut t));
}

// ── R1: per-field ordering (stable, ties by index, blanks last) ─────────────

/// Check one field both ways over `agents`/`tasks`, in the default Team
/// grouping (no team exists, so one section).
#[track_caller]
fn check_order(
    field: AgentSortField,
    agents: Vec<Agent>,
    tasks: Vec<orch_domain::Task>,
    asc: &[&str],
    desc: &[&str],
) {
    let mut h = agents_view(
        FakeBackend::new().with_agents(agents).with_tasks(tasks),
        WIDE,
        HIGH,
    );
    set_sort(&mut h, field, false);
    assert_eq!(ids(&h), asc, "{field:?} ascending");
    set_sort(&mut h, field, true);
    assert_eq!(ids(&h), desc, "{field:?} descending");
}

#[test]
fn r1_sort_by_status_ranks_running_idle_error_and_keeps_index_order_in_ties() {
    let statuses = [
        AgentStatus::Error,
        AgentStatus::Running,
        AgentStatus::Idle,
        AgentStatus::Running,
        AgentStatus::Idle,
        AgentStatus::Error,
    ];
    let agents = trap_agents(6, |i, a| a.status = statuses[i]);
    // i1 i3 Running | i2 i4 Idle | i0 i5 Error
    check_order(
        AgentSortField::Status,
        agents,
        vec![],
        &["id8", "id6", "id7", "id5", "id9", "id4"],
        &["id9", "id4", "id7", "id5", "id8", "id6"],
    );
}

#[test]
fn r1_sort_by_name_ignores_case_and_keeps_index_order_for_equal_names() {
    let names = ["bob", "Zed", "alice", "alice", "carl"];
    let agents = trap_agents(5, |i, a| a.name = names[i].into());
    // alice(i2,i3) bob(i0) carl(i4) Zed(i1)
    check_order(
        AgentSortField::Name,
        agents,
        vec![],
        &["id7", "id6", "id9", "id5", "id8"],
        &["id8", "id5", "id9", "id7", "id6"],
    );
}

#[test]
fn r1_sort_by_adapter_model_orders_adapter_then_model_with_index_ties() {
    let spec = [
        ("shell", "m-s"),
        ("claude", "m-b"),
        ("claude", "m-a"),
        ("claude", "m-b"),
        ("codex", "m-c"),
    ];
    let agents = trap_agents(5, |i, a| {
        a.adapter = spec[i].0.into();
        a.config.model = Some(spec[i].1.into());
    });
    // claude/m-a(i2) claude/m-b(i1,i3) codex(i4) shell(i0)
    check_order(
        AgentSortField::AdapterModel,
        agents,
        vec![],
        &["id7", "id8", "id6", "id5", "id9"],
        &["id9", "id5", "id8", "id6", "id7"],
    );
}

#[test]
fn r1_sort_by_task_orders_by_title_and_puts_agents_without_one_last_both_ways() {
    // Task ids run against title order, so sorting by task id would show.
    let titles = [
        Some(("t_y", "build")),
        None,
        Some(("t_z", "audit")),
        None,
        Some(("t_x", "deploy")),
    ];
    let agents = trap_agents(5, |i, a| {
        if let Some((task_id, _)) = titles[i] {
            a.status = AgentStatus::Running;
            a.current_task = Some(task_id.into());
        }
    });
    let tasks = titles
        .iter()
        .flatten()
        .map(|(id, title)| make_task(id, title, TaskStatus::InProgress))
        .collect();
    // audit(i2) build(i0) deploy(i4), then the blanks i1 i3 in index order.
    // Status is not a tie-breaker here: the blanks are Idle, the rest Running,
    // and the sort field decides first.
    check_order(
        AgentSortField::Task,
        agents,
        tasks,
        &["id7", "id9", "id5", "id8", "id6"],
        &["id5", "id9", "id7", "id8", "id6"],
    );
}

#[test]
fn r1_sort_by_caps_orders_alphabetically_and_puts_agents_without_caps_last_both_ways() {
    let caps: [&[&str]; 5] = [&["macos"], &[], &["linux"], &[], &["windows"]];
    let agents = trap_agents(5, |i, a| {
        a.capabilities = caps[i].iter().map(|c| c.to_string()).collect()
    });
    // linux(i2) macos(i0) windows(i4), then blanks i1 i3.
    check_order(
        AgentSortField::Caps,
        agents,
        vec![],
        &["id7", "id9", "id5", "id8", "id6"],
        &["id5", "id9", "id7", "id8", "id6"],
    );
}

#[test]
fn r1_sort_by_where_puts_local_first_then_aliases_with_index_ties() {
    let place: [Option<&str>; 5] = [None, Some("wb"), Some("wa"), None, Some("wb")];
    let agents = trap_agents(5, |i, a| a.remote = place[i].and_then(|p| remote(p, true)));
    // local(i0,i3) wa(i2) wb(i1,i4)
    check_order(
        AgentSortField::Location,
        agents,
        vec![],
        &["id9", "id6", "id7", "id8", "id5"],
        &["id8", "id5", "id7", "id9", "id6"],
    );
}

#[test]
fn r1_sort_by_runs_is_numeric_and_puts_agents_with_no_runs_last_both_ways() {
    // 10 sorts after 5 numerically but before 2 as text.
    let runs = [2u64, 10, 0, 5, 0];
    let agents = trap_agents(5, |i, a| {
        a.stats.total_runs = runs[i];
        a.stats.tasks_completed = runs[i];
    });
    // 2(i0) 5(i3) 10(i1), then blanks i2 i4.
    check_order(
        AgentSortField::Runs,
        agents,
        vec![],
        &["id9", "id6", "id8", "id7", "id5"],
        &["id8", "id6", "id9", "id7", "id5"],
    );
}

#[test]
fn r1_sort_by_updated_with_no_timestamps_keeps_index_order_both_ways() {
    // Whatever the source of UPDATED (design decision D1), an agent with no
    // timestamp is blank, and all-blank is all-tied.
    let agents = trap_agents(5, |_, _| {});
    check_order(
        AgentSortField::Updated,
        agents,
        vec![],
        &["id9", "id8", "id7", "id6", "id5"],
        &["id9", "id8", "id7", "id6", "id5"],
    );
}

#[test]
fn r1_every_field_leaves_the_set_of_agents_unchanged() {
    let mut h = world(WIDE);
    let mut all: Vec<String> = ids(&h).iter().map(|s| s.to_string()).collect();
    all.sort();
    for (field, _) in FIELDS {
        for descending in [false, true] {
            set_sort(&mut h, field, descending);
            let mut now: Vec<String> = ids(&h).iter().map(|s| s.to_string()).collect();
            now.sort();
            assert_eq!(now, all, "{field:?} descending={descending}");
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// R2 - sections stay, sort applies inside them, selection id is kept
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn r2_team_sections_show_name_count_and_lead_and_the_unassigned_rest() {
    let mut h = world(WIDE);
    set_sort(&mut h, AgentSortField::Name, false);
    let text = h.text();
    assert_eq!(
        grouped(&text),
        expected(&[
            ("alpha", &["Agx-00", "Agx-01"]),
            ("beta", &["Agx-04"]),
            ("unassigned", &["Agx-02", "Agx-03", "Agx-05"]),
        ])
    );
    // The lead is named on its team's header.
    let alpha = text.lines().find(|l| l.contains("ALPHA")).unwrap();
    assert!(alpha.contains("★ Agx-00"), "{alpha:?}");
    assert!(alpha.contains("2 agents"), "{alpha:?}");
}

#[test]
fn r2_node_sections_mark_local_online_and_offline_nodes() {
    let mut h = world(WIDE);
    set_mode(&mut h, AgentGroupBy::Node);
    let text = h.text();
    for needle in ["LOCAL", "wa ● online", "wb ○ offline"] {
        assert!(text.contains(needle), "{needle:?} missing:\n{text}");
    }
}

#[test]
fn r2_the_sort_applies_inside_every_section_and_descending_never_moves_a_section() {
    for (mode, _) in MODES {
        let mut h = world(WIDE);
        set_mode(&mut h, mode);
        set_sort(&mut h, AgentSortField::Name, false);
        let asc = world_sections(mode);
        assert_eq!(
            grouped(&h.text()),
            expected(
                &asc.iter()
                    .map(|(l, a)| (*l, a.as_slice()))
                    .collect::<Vec<_>>()
            ),
            "{mode:?} ascending"
        );
        set_sort(&mut h, AgentSortField::Name, true);
        let desc: Vec<(&str, Vec<&str>)> = asc
            .iter()
            .map(|(l, a)| (*l, a.iter().rev().copied().collect()))
            .collect();
        assert_eq!(
            grouped(&h.text()),
            expected(
                &desc
                    .iter()
                    .map(|(l, a)| (*l, a.as_slice()))
                    .collect::<Vec<_>>()
            ),
            "{mode:?} descending"
        );
    }
}

/// Every action that reorders the list. Each must leave the selected agent
/// selected (by id), wherever it lands.
fn reorder_steps() -> Vec<(String, Box<dyn Fn(&mut Harness)>)> {
    let mut steps: Vec<(String, Box<dyn Fn(&mut Harness)>)> = Vec::new();
    for key in ['o', 'O', '0', 'b', 'B'] {
        for n in 1..=3 {
            steps.push((
                format!("{n} x {key}"),
                Box::new(move |h: &mut Harness| {
                    for _ in 0..n {
                        h.key(key);
                    }
                }),
            ));
        }
    }
    for (_, label) in FIELDS {
        steps.push((
            format!("click header {label}"),
            Box::new(move |h: &mut Harness| click_header(h, label)),
        ));
    }
    steps
}

#[test]
fn r2_the_selected_agent_survives_every_sort_and_group_key_and_header_click() {
    for start in 0..6 {
        let mut h = world(WIDE);
        h.app.state.agent_selected = start;
        let id = selected_id(&h);
        for (name, step) in reorder_steps() {
            step(&mut h);
            h.draw();
            assert_eq!(selected_id(&h), id, "start {start}, after {name}");
            assert_eq!(
                ids(&h)[h.app.state.agent_selected],
                id,
                "start {start}, cursor index after {name}"
            );
        }
    }
}

#[test]
fn r2_the_selected_agent_survives_clicking_the_group_chip_all_the_way_round() {
    let mut h = world(WIDE);
    h.app.state.agent_selected = 3;
    let id = selected_id(&h);
    for round in 0..MODES.len() {
        h.click_text("GROUP: ");
        assert_eq!(selected_id(&h), id, "after chip click {round}");
    }
}

#[test]
fn r2_the_add_agent_row_stays_selected_through_sorts_and_groupings() {
    let mut h = world(WIDE);
    let add = h.app.state.agents.len();
    h.app.state.agent_selected = add;
    for key in ['o', 'O', 'b', 'b', 'b', 'b', 'b', 'b', 'B', '0'] {
        h.key(key);
        assert_eq!(h.app.state.agent_selected, add, "after {key}");
        assert!(h.app.state.selected_agent().is_none(), "after {key}");
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// R3 - grouping modes
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn r3_the_default_grouping_is_team() {
    assert_eq!(AgentGroupBy::default(), AgentGroupBy::Team);
    assert_eq!(world(WIDE).app.state.agent_group_by, AgentGroupBy::Team);
}

#[test]
fn r3_every_mode_draws_its_sections_with_the_default_status_sort() {
    // Default sort Status asc: inside a section, Running before Idle before
    // Error, and index order among equals.
    let by_status: [(AgentGroupBy, Vec<(&str, Vec<&str>)>); 5] = [
        (
            AgentGroupBy::Team,
            vec![
                ("alpha", vec!["Agx-01", "Agx-00"]),
                ("beta", vec!["Agx-04"]),
                ("unassigned", vec!["Agx-05", "Agx-02", "Agx-03"]),
            ],
        ),
        (
            AgentGroupBy::Node,
            vec![
                ("local", vec!["Agx-05", "Agx-00", "Agx-03"]),
                ("wa ● online", vec!["Agx-01", "Agx-04"]),
                ("wb ○ offline", vec!["Agx-02"]),
            ],
        ),
        (
            AgentGroupBy::Status,
            world_sections(AgentGroupBy::Status).into_iter().collect(),
        ),
        (
            AgentGroupBy::Adapter,
            vec![
                ("claude", vec!["Agx-05", "Agx-00", "Agx-03"]),
                ("codex", vec!["Agx-01", "Agx-04"]),
                ("shell", vec!["Agx-02"]),
            ],
        ),
        (
            AgentGroupBy::Caps,
            vec![
                ("linux", vec!["Agx-04"]),
                ("macos", vec!["Agx-01", "Agx-03"]),
                ("web", vec!["Agx-05", "Agx-00"]),
                ("no caps", vec!["Agx-02"]),
            ],
        ),
    ];
    for (mode, sections) in by_status {
        let mut h = world(WIDE);
        set_mode(&mut h, mode);
        assert_eq!(
            grouped(&h.text()),
            expected(
                &sections
                    .iter()
                    .map(|(l, a)| (*l, a.as_slice()))
                    .collect::<Vec<_>>()
            ),
            "{mode:?}"
        );
    }
}

#[test]
fn r3_status_sections_run_active_idle_error_offline_disabled() {
    let mut h = world(WIDE);
    set_mode(&mut h, AgentGroupBy::Status);
    let labels: Vec<String> = grouped(&h.text()).into_iter().map(|(l, _)| l).collect();
    assert_eq!(labels, ["active", "idle", "error", "offline", "disabled"]);
}

#[test]
fn r3_status_sections_stay_in_that_order_whatever_the_sort() {
    for (field, _) in FIELDS {
        for descending in [false, true] {
            let mut h = world(WIDE);
            set_mode(&mut h, AgentGroupBy::Status);
            set_sort(&mut h, field, descending);
            let labels: Vec<String> = grouped(&h.text()).into_iter().map(|(l, _)| l).collect();
            assert_eq!(
                labels,
                ["active", "idle", "error", "offline", "disabled"],
                "{field:?} descending={descending}"
            );
        }
    }
}

#[test]
fn r3_empty_sections_are_not_drawn() {
    let agents = vec![
        make_agent("i", "Agx-00", AgentStatus::Idle),
        make_agent("o", "Agx-01", AgentStatus::Offline),
        make_agent("i2", "Agx-02", AgentStatus::Idle),
    ];
    let mut h = agents_view(FakeBackend::new().with_agents(agents), WIDE, HIGH);
    set_mode(&mut h, AgentGroupBy::Status);
    assert_eq!(
        grouped(&h.text()),
        expected(&[("idle", &["Agx-00", "Agx-02"]), ("offline", &["Agx-01"])])
    );
}

#[test]
fn r3_status_sections_follow_a_status_change_on_the_next_frame() {
    let mut h = world(WIDE);
    set_mode(&mut h, AgentGroupBy::Status);
    for a in &mut h.app.state.agents {
        a.status = AgentStatus::Error;
    }
    assert_eq!(
        grouped(&h.text())
            .into_iter()
            .map(|(l, _)| l)
            .collect::<Vec<_>>(),
        ["error"]
    );
}

#[test]
fn r3_adapter_sections_are_alphabetical_not_first_seen() {
    let adapters = ["shell", "codex", "claude", "shell"];
    let agents: Vec<Agent> = adapters
        .iter()
        .enumerate()
        .map(|(i, ad)| {
            let mut a = make_agent(&format!("a{i}"), &format!("Agx-{i:02}"), AgentStatus::Idle);
            a.adapter = (*ad).into();
            a
        })
        .collect();
    let mut h = agents_view(FakeBackend::new().with_agents(agents), WIDE, HIGH);
    set_mode(&mut h, AgentGroupBy::Adapter);
    assert_eq!(
        grouped(&h.text()),
        expected(&[
            ("claude", &["Agx-02"]),
            ("codex", &["Agx-01"]),
            ("shell", &["Agx-00", "Agx-03"]),
        ])
    );
}

#[test]
fn r3_caps_sections_are_alphabetical_with_no_caps_last() {
    let mut h = world(WIDE);
    set_mode(&mut h, AgentGroupBy::Caps);
    let labels: Vec<String> = grouped(&h.text()).into_iter().map(|(l, _)| l).collect();
    assert_eq!(labels, ["linux", "macos", "web", "no caps"]);
}

#[test]
fn r3_an_agent_with_several_caps_is_still_listed_and_the_counts_match_the_rows() {
    // Design decision D2 (first cap only, or once per cap) is not pinned: the
    // agent must be reachable, and every header count must equal its rows.
    let mut agents = world_agents();
    agents[2].capabilities = vec!["web".into(), "linux".into(), "macos".into()];
    let mut h = agents_view(FakeBackend::new().with_agents(agents), WIDE, HIGH);
    set_mode(&mut h, AgentGroupBy::Caps);
    let sections = grouped(&h.text()); // asserts count == rows per section
    assert!(
        sections
            .iter()
            .any(|(_, a)| a.iter().any(|n| n == "Agx-02")),
        "Agx-02 is in no section: {sections:?}"
    );
    // Whatever the placement, only caps the agent has (or "no caps" for
    // those with none) can label a section holding it.
    for (label, members) in &sections {
        if members.iter().any(|n| n == "Agx-02") {
            assert!(
                ["web", "linux", "macos"].contains(&label.as_str()),
                "{label}"
            );
        }
    }
}

#[test]
fn r3_none_draws_a_flat_table_with_no_section_rows() {
    let mut h = world(WIDE);
    set_mode(&mut h, AgentGroupBy::None);
    set_sort(&mut h, AgentSortField::Name, false);
    let text = h.text();
    assert_eq!(
        grouped(&text),
        expected(&[(
            "",
            &["Agx-00", "Agx-01", "Agx-02", "Agx-03", "Agx-04", "Agx-05"]
        )])
    );
    for old in ["UNASSIGNED", "ALPHA", "LOCAL", "agents ·", "agent ·"] {
        assert!(
            !text.contains(old),
            "{old:?} still drawn under None:\n{text}"
        );
    }
}

#[test]
fn r3_none_orders_the_whole_list_by_the_active_sort_across_former_sections() {
    let mut h = world(WIDE);
    set_mode(&mut h, AgentGroupBy::None);
    set_sort(&mut h, AgentSortField::Name, true);
    assert_eq!(ids(&h), ["a5", "a4", "a3", "a2", "a1", "a0"]);
    set_sort(&mut h, AgentSortField::AdapterModel, false);
    // claude x3 (a0, a3, a5), codex x2 (a1, a4), shell (a2): index ties.
    assert_eq!(ids(&h), ["a0", "a3", "a5", "a1", "a4", "a2"]);
}

#[test]
fn r3_section_headers_count_agents_in_the_singular_and_plural() {
    let mut h = world(WIDE);
    set_mode(&mut h, AgentGroupBy::Status);
    let text = h.text();
    assert!(text.contains("2 agents"), "{text}");
    assert!(text.contains("1 agent "), "{text}");
}

#[test]
fn r3_one_agent_list_and_empty_list_survive_every_mode_sort_and_width() {
    for agents in [
        vec![],
        vec![make_agent("only", "Agx-00", AgentStatus::Error)],
    ] {
        let count = agents.len();
        for width in [60, 100, 200] {
            let mut h = agents_view(FakeBackend::new().with_agents(agents.clone()), width, HIGH);
            for (mode, _) in MODES {
                set_mode(&mut h, mode);
                for (field, _) in FIELDS {
                    for descending in [false, true] {
                        set_sort(&mut h, field, descending);
                        h.draw();
                        assert_eq!(ids(&h).len(), count);
                    }
                }
            }
            h.key('0');
            assert_eq!(h.app.state.agent_selected, 0);
            // The header row is still drawn with no agents.
            header_y(&mut h);
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// R4 - switching: b / B, the header chip
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn r4_b_walks_the_modes_in_contract_order_and_wraps() {
    let mut h = world(WIDE);
    for step in 1..=MODES.len() {
        h.key('b');
        assert_eq!(
            h.app.state.agent_group_by,
            MODES[step % MODES.len()].0,
            "after {step} x b"
        );
    }
    assert_eq!(h.app.state.agent_group_by, AgentGroupBy::Team);
}

#[test]
fn r4_capital_b_walks_the_modes_backwards_and_wraps() {
    let mut h = world(WIDE);
    for step in 1..=MODES.len() {
        h.key('B');
        assert_eq!(
            h.app.state.agent_group_by,
            MODES[(MODES.len() - step) % MODES.len()].0,
            "after {step} x B"
        );
    }
    assert_eq!(h.app.state.agent_group_by, AgentGroupBy::Team);
}

#[test]
fn r4_next_and_prev_are_inverse_over_all_modes() {
    for (mode, _) in MODES {
        assert_eq!(mode.next().prev(), mode);
        assert_eq!(mode.prev().next(), mode); // contract: prev() added
    }
    let mut seen = vec![AgentGroupBy::Team];
    let mut m = AgentGroupBy::Team.next();
    while m != AgentGroupBy::Team {
        seen.push(m);
        m = m.next();
    }
    assert_eq!(seen, MODES.map(|(m, _)| m));
}

#[test]
fn r4_the_mode_labels_are_the_contract_words() {
    for (mode, word) in MODES {
        assert_eq!(mode.label(), word);
    }
}

#[test]
fn r4_b_and_capital_b_leave_the_sort_alone() {
    let mut h = world(WIDE);
    h.key('o').key('o').key('O');
    let sort = h.app.state.agent_sort;
    for _ in 0..7 {
        h.key('b');
    }
    h.key('B');
    assert_eq!(h.app.state.agent_sort, sort);
}

#[test]
fn r4_b_is_inert_outside_the_agents_view_and_while_typing_a_command() {
    let mut h = world(WIDE);
    h.app.state.view = ViewId::Tasks;
    h.key('b').key('B');
    assert_eq!(h.app.state.agent_group_by, AgentGroupBy::Team);
    h.app.state.view = ViewId::Agents;
    h.key('/').type_str("bB");
    assert_eq!(h.app.state.agent_group_by, AgentGroupBy::Team);
}

#[test]
fn r4_the_header_row_ends_with_a_group_chip_naming_the_mode() {
    let mut h = world(WIDE);
    for (mode, word) in MODES {
        set_mode(&mut h, mode);
        let line = header_line(&mut h);
        let chip = format!("GROUP: {word}");
        assert!(
            line.trim_end().ends_with(&chip),
            "mode {word}: header row {line:?} does not end with {chip:?}"
        );
    }
}

#[test]
fn r4_the_group_chip_is_drawn_once_and_only_on_the_header_row() {
    let mut h = world(WIDE);
    let text = h.text();
    assert_eq!(text.matches("GROUP: ").count(), 1, "{text}");
    let y = header_y(&mut h);
    assert!(h.line(y).contains("GROUP: Team"));
}

#[test]
fn r4_the_group_chip_is_separated_from_the_last_column_label() {
    let mut h = world(WIDE);
    let line = header_line(&mut h);
    let chip = col_of(&line, "GROUP: ").unwrap() as usize;
    let before: String = line
        .chars()
        .scan(0usize, |w, c| {
            let start = *w;
            *w += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            Some((start, c))
        })
        .take_while(|(w, _)| *w < chip)
        .map(|(_, c)| c)
        .collect();
    assert!(before.ends_with(' '), "chip glued to a label: {line:?}");
}

#[test]
fn r4_clicking_the_group_chip_cycles_to_the_next_mode() {
    let mut h = world(WIDE);
    for step in 1..=MODES.len() {
        h.click_text("GROUP: ");
        assert_eq!(
            h.app.state.agent_group_by,
            MODES[step % MODES.len()].0,
            "after {step} chip clicks"
        );
    }
}

#[test]
fn r4_the_group_chip_is_a_hit_zone_of_its_own_not_a_sort_header_or_a_row() {
    let mut h = world(WIDE);
    let (x, y) = h.find_text("GROUP: Team").expect("chip on screen");
    let zone = h.zone_at(x, y).expect("the chip has a hit zone").clone();
    assert!(
        !matches!(
            zone.kind,
            ZoneKind::SortHeader(_) | ZoneKind::AgentRow { .. } | ZoneKind::Scroll(_)
        ),
        "chip zone is {:?}",
        zone.kind
    );
    // And no sort header shares its zone id: pressing it can never pair with
    // a column.
    for (field, label) in FIELDS {
        let (lx, ly) = label_cell(&mut h, label);
        assert_ne!(
            h.zone_at(lx, ly).map(|z| z.id.clone()),
            Some(zone.id.clone()),
            "{field:?}"
        );
    }
}

#[test]
fn r4_the_group_chip_covers_its_whole_text() {
    let mut h = world(WIDE);
    let (x, y) = h.find_text("GROUP: Team").unwrap();
    let first = h.zone_at(x, y).map(|z| z.id.clone());
    let last = h
        .zone_at(x + "GROUP: Team".len() as u16 - 1, y)
        .map(|z| z.id.clone());
    assert!(first.is_some());
    assert_eq!(first, last);
}

#[test]
fn r4_clicking_the_chip_does_not_sort() {
    let mut h = world(WIDE);
    h.key('o');
    let sort = h.app.state.agent_sort;
    h.click_text("GROUP: ");
    assert_eq!(h.app.state.agent_sort, sort);
}

#[test]
fn r4_the_chip_survives_a_hostile_mode_switch_storm() {
    let mut h = world(WIDE);
    for _ in 0..50 {
        h.key('b').key('o').key('B').key('B').key('O');
        header_line(&mut h);
    }
    assert_eq!(h.text().matches("GROUP: ").count(), 1);
}

// ═════════════════════════════════════════════════════════════════════════════
// R5 - narrow widths: drop order, alignment, the freed row
// ═════════════════════════════════════════════════════════════════════════════

/// One running agent whose every cell has a distinctive first word, not a
/// team lead (a lead's name is also on its team header).
fn alignment_backend() -> FakeBackend {
    let mut a = make_agent("a1", "Walter", AgentStatus::Running);
    a.current_task = Some("t1".into());
    a.capabilities = vec!["capx".into()];
    a.remote = remote("wnode", true);
    a.config.model = Some("opus-x".into());
    a.stats.total_runs = 4;
    a.stats.tasks_completed = 3;
    let mut t = make_task("t1", "Taskmark", TaskStatus::InProgress);
    t.assignee = Some("a1".into());
    FakeBackend::new().with_agents(vec![a]).with_tasks(vec![t])
}

/// The header label, the first characters of the cell under it.
const LEFT_COLUMNS: [(&str, &str); 5] = [
    ("NAME", "Walter"),
    ("ADAPTER", "claude/o"),
    ("TASK", "Taskmark"),
    ("CAPS", "capx"),
    ("WHERE", "wnode"),
];

#[test]
fn r5_header_labels_and_row_cells_stay_aligned_at_every_width_60_to_200() {
    for width in 60..=200u16 {
        let mut h = agents_view(alignment_backend(), width, HIGH);
        let text = h.text();
        let hy = header_y(&mut h);
        let header = h.line(hy);
        let row = text
            .lines()
            .find(|l| l.contains("Walter"))
            .unwrap_or_else(|| panic!("{width}: no agent row:\n{text}"))
            .to_string();

        // The name is never dropped, and the status column leads.
        assert!(header.contains("NAME"), "{width}: {header:?}");
        // The chip reads `<glyph> ACTIVE`, so the word starts two cells in.
        assert_eq!(
            col_of(&header, "STATUS").map(|c| c + 2),
            col_of(&row, "ACTIVE"),
            "{width}: STATUS vs chip\n{header}\n{row}"
        );

        // A column is shown in the header exactly when its cell is shown in
        // the row, and the two start in the same cell.
        for (label, cell) in LEFT_COLUMNS {
            assert_eq!(
                col_of(&header, label),
                col_of(&row, cell),
                "{width}: {label} vs {cell}\n{header}\n{row}"
            );
        }

        // RUNS is right-aligned: label and value end in the same cell.
        assert_eq!(
            col_of(&header, "RUNS").is_some(),
            col_of(&row, "✓").is_some(),
            "{width}: RUNS shown on one side only\n{header}\n{row}"
        );
        if col_of(&header, "RUNS").is_some() {
            assert_eq!(
                end_col_of(&header, "RUNS"),
                end_col_of(&row, "✓"),
                "{width}: RUNS vs value\n{header}\n{row}"
            );
        }
    }
}

#[test]
fn r5_every_sortable_header_cell_hits_its_own_column_at_every_width() {
    for width in 60..=200u16 {
        let mut h = agents_view(alignment_backend(), width, HIGH);
        let hy = header_y(&mut h);
        let header = h.line(hy);
        for (field, label) in FIELDS {
            let key = if label == "ADAPTER/MODEL" {
                "ADAPTER"
            } else {
                label
            };
            let Some(x) = col_of(&header, key) else {
                continue;
            };
            assert_eq!(
                h.zone_at(x, hy).map(|z| z.kind.clone()),
                Some(ZoneKind::SortHeader(SortColumn::Agent(field))),
                "{width}: {label} in {header:?}"
            );
        }
    }
}

#[test]
fn r5_the_name_is_never_dropped_and_todays_columns_keep_their_availability() {
    // Today's fit (the existing `the_documented_column_widths_hold` table):
    // columns are granted RUNS, TASK, WHERE, ADAPTER/MODEL, CAPS in that
    // order while they fit, so a narrower width can show a column a wider
    // one does not (60 has ADAPTER/MODEL but no WHERE). UPDATED is new and
    // must not take a column that was there.
    // (width, adapter/model, task, caps, where, runs)
    let table = [
        (60u16, true, true, false, false, true),
        (70, false, true, false, true, true),
        (80, true, true, false, true, true),
        (90, true, true, true, true, true),
        (100, true, true, true, true, true),
        (104, true, true, true, true, true),
        (106, true, true, true, true, true),
        (120, true, true, true, true, true),
        (160, true, true, true, true, true),
    ];
    for (width, adapter, task, caps, place, runs) in table {
        let mut h = agents_view(alignment_backend(), width, HIGH);
        let header = header_line(&mut h);
        assert!(header.contains("NAME"), "{width}: {header:?}");
        for (label, shown) in [
            ("ADAPTER", adapter),
            ("TASK", task),
            ("CAPS", caps),
            ("WHERE", place),
            ("RUNS", runs),
        ] {
            assert_eq!(
                header.contains(label),
                shown,
                "{width}: {label} in {header:?}"
            );
        }
    }
}

#[test]
fn r5_at_120_columns_every_existing_column_and_updated_and_the_chip_show() {
    let mut h = agents_view(alignment_backend(), 120, HIGH);
    let header = header_line(&mut h);
    for label in [
        "STATUS",
        "NAME",
        "ADAPTER",
        "TASK",
        "CAPS",
        "WHERE",
        "RUNS",
        "GROUP: Team",
    ] {
        assert!(header.contains(label), "{label} missing at 120: {header:?}");
    }
}

#[test]
fn r5_at_160_columns_all_eight_labels_and_the_chip_show() {
    let mut h = agents_view(alignment_backend(), 160, HIGH);
    let header = header_line(&mut h);
    for (_, label) in FIELDS {
        let key = if label == "ADAPTER/MODEL" {
            "ADAPTER/MODEL"
        } else {
            label
        };
        assert!(header.contains(key), "{label} missing at 160: {header:?}");
    }
    assert!(header.trim_end().ends_with("GROUP: Team"), "{header:?}");
}

#[test]
fn r5_at_80_columns_only_caps_and_the_new_columns_are_dropped() {
    // Today (before this change) 80 columns drop CAPS alone. UPDATED is new
    // and may not displace a column that was there.
    let mut h = agents_view(alignment_backend(), 80, HIGH);
    let header = header_line(&mut h);
    for label in ["STATUS", "NAME", "ADAPTER", "TASK", "WHERE", "RUNS"] {
        assert!(header.contains(label), "{label} missing at 80: {header:?}");
    }
    assert!(!header.contains("CAPS"), "{header:?}");
}

#[test]
fn r5_the_group_chip_is_visible_at_80_and_above() {
    for width in 80..=200u16 {
        let mut h = agents_view(alignment_backend(), width, HIGH);
        let header = header_line(&mut h);
        assert!(
            header.trim_end().ends_with("GROUP: Team"),
            "{width}: {header:?}"
        );
    }
}

#[test]
fn r5_the_sweep_holds_in_every_mode_and_with_a_descending_sort() {
    // Section rows must not shift the table: rows keep the same columns under
    // every grouping, in both directions.
    for (mode, _) in MODES {
        for descending in [false, true] {
            for width in [60u16, 80, 100, 120, 160, 200] {
                let mut h = agents_view(alignment_backend(), width, HIGH);
                set_mode(&mut h, mode);
                set_sort(&mut h, AgentSortField::Name, descending);
                let text = h.text();
                let hy = header_y(&mut h);
                let header = h.line(hy);
                let row = text.lines().find(|l| l.contains("Walter")).unwrap();
                for (label, cell) in LEFT_COLUMNS {
                    assert_eq!(
                        col_of(&header, label),
                        col_of(row, cell),
                        "{mode:?} desc={descending} w={width}: {label}\n{header}\n{row}"
                    );
                }
            }
        }
    }
}

#[test]
fn r5_a_long_unicode_name_cannot_push_the_columns_out_of_line() {
    let mut a = make_agent(
        "a1",
        "漢字漢字漢字漢字漢字漢字漢字漢字漢字漢字漢字",
        AgentStatus::Idle,
    );
    a.capabilities = vec!["capx".into()];
    a.remote = remote("wnode", true);
    let b = make_agent("a2", "Walter", AgentStatus::Idle);
    let mut backend_agents = vec![a, b];
    backend_agents[1].capabilities = vec!["capx".into()];
    backend_agents[1].remote = remote("wnode", true);
    for width in [80u16, 100, 120, 160] {
        let mut h = agents_view(
            FakeBackend::new().with_agents(backend_agents.clone()),
            width,
            HIGH,
        );
        let text = h.text();
        let hy = header_y(&mut h);
        let header = h.line(hy);
        let rows: Vec<&str> = text.lines().filter(|l| l.contains("wnode")).collect();
        assert_eq!(rows.len(), 2, "{width}:\n{text}");
        for row in rows {
            assert_eq!(
                col_of(&header, "WHERE"),
                col_of(row, "wnode"),
                "{width}:\n{header}\n{row}"
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// R6 (cheap early guard for the new section kinds; the full pass is the
// security review's)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn r6_control_and_bidi_characters_in_adapter_and_caps_never_reach_the_frame() {
    let mut a = make_agent("a1", "Agx-00", AgentStatus::Idle);
    a.adapter = "cl\u{1b}[31maude\u{202e}".into();
    a.capabilities = vec!["x\u{1b}[2Jy\u{200b}z\u{202e}".into()];
    for mode in [
        AgentGroupBy::Adapter,
        AgentGroupBy::Caps,
        AgentGroupBy::Status,
    ] {
        let mut h = agents_view(FakeBackend::new().with_agents(vec![a.clone()]), WIDE, HIGH);
        set_mode(&mut h, mode);
        let text = h.text();
        for bad in ['\u{1b}', '\u{202e}', '\u{200b}', '\u{7}'] {
            assert!(
                !text.contains(bad),
                "{mode:?}: {bad:?} reached the frame:\n{text}"
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Notes for the design (not asserted):
//
// * Section order for Status is ACTIVE, IDLE, ERROR, OFFLINE, DISABLED, but
//   today's `agent_status_order` ranks Disabled (3) before Offline (4), and
//   the STATUS column sort uses it. No case here puts an Offline and a
//   Disabled agent in one section under a Status sort, so the design may
//   choose either; it should choose one and say so.
// * UPDATED (D1): no case sets a timestamp; the only case is "all blank".
//   Once the data source is fixed, add: newest-first/oldest-first, blanks
//   last in both directions, relative-age text in the row.
// * RUNS is sorted by `stats.total_runs` here. If the design sorts a running
//   agent by elapsed time or by completion ratio, case
//   `r1_sort_by_runs_is_numeric...` must be revisited with the design.
// ═════════════════════════════════════════════════════════════════════════════
