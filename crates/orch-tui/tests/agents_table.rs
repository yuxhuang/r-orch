//! The Agents list is a table like Goals and Tasks (goal `goal_DHCHB3O`,
//! requirements R1-R5), aligned to the lead's adjudication v2
//! (`adjudication-v2.md`, plan.md "Lead rulings v2").
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
//!   `b` cycles forward, `B` backward; there is no chip row above the header;
//! * the `GROUP: <Mode>` chip sits at the right end of the header row and is
//!   clickable, but only from table width 108 (`GROUP_CHIP_FROM`): it is
//!   chrome, budgeted after every column. Below that the Agents footer
//!   carries the mode as a clickable first hint, `b <Mode>`, at every width;
//! * sorting is stable, ties fall back to index (source) order in both
//!   directions, and blanks (no task, no caps, no runs, no timestamp) sort
//!   last in both directions;
//! * UPDATED is `R <age>` (newest valid `started_at`/`last_event_at` of the
//!   agent's running entries), else `T <age>` (newest valid `updated_at` of
//!   the tasks assigned to it), else `—`; the sort key is the timestamp
//!   alone, oldest first when ascending;
//! * section rows (`◈ LABEL · n agents`) per mode, empty sections are not
//!   drawn, `None` draws none, Status sections and the STATUS sort rank both
//!   run ACTIVE, IDLE, ERROR, OFFLINE, DISABLED, Adapter and Caps sections
//!   are alphabetical; Caps places an agent once, under its alphabetically
//!   first capability, labelled with the plain capability name.
//!
//! Only the public API of `orch-tui` and the rendered frame are used, so the
//! cases survive any choice of internal seams. Section rows are recognised by
//! their shape (`label · N agent(s)`), not by glyph or by a private row enum.
//! Every case runs on the fake backend with a held clock and touches no home
//! directory, network or AI CLI.
//!
//! Changed from the first T0 draft, by ruling (each test names its ruling in
//! a `Ruling:` line): chip visibility (Issue 2), UPDATED semantics (Issue 1),
//! header arrow on a cut label, Status rank, RUNS statistics on active rows,
//! all-capability keyword matching, Caps primary placement.

mod common;

use common::Harness;
use orch_domain::{Agent, AgentStatus, RemoteRef, RunningEntry, Task, TaskStatus};
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

/// The first table width with the `GROUP: <Mode>` chip (adjudication v2,
/// Issue 2): the chip is chrome and is budgeted after every column, so it
/// needs 15 cells (gap + 14) to be spare; that first happens at 108.
const GROUP_CHIP_FROM: u16 = 108;

/// Widths that carry the chip, from its first width to a very wide one.
const CHIP_WIDTHS: [u16; 3] = [GROUP_CHIP_FROM, 120, WIDE];

/// The instant [`Harness`] starts its clock at.
const BASE: i64 = 1_000_000;

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

/// Buffer column of `needle` on screen row `y`, counted in cells. A wide
/// glyph fills two cells and its hidden second cell flattens to a space in
/// [`Harness::line`], so [`col_of`] over-counts a row holding wide glyphs.
/// This walks the cells themselves; a cell's symbol may be one grapheme or
/// empty, never several cells.
fn cell_col_of(h: &Harness, y: u16, needle: &str) -> Option<u16> {
    let buffer = h.terminal.backend().buffer();
    let mut text = String::new();
    let mut starts = Vec::new();
    for x in 0..buffer.area.width {
        starts.push((text.len(), x));
        text.push_str(buffer[(x, y)].symbol());
    }
    let byte = text.find(needle)?;
    starts
        .iter()
        .rev()
        .find(|(start, _)| *start <= byte)
        .map(|(_, x)| *x)
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

// ── Time and UPDATED helpers ─────────────────────────────────────────────────

/// An RFC 3339 timestamp on day one of 1970, `ms` after midnight.
fn iso(ms: i64) -> String {
    assert!((0..86_400_000).contains(&ms), "{ms} is not on day one");
    format!(
        "1970-01-01T{:02}:{:02}:{:02}.{:03}Z",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// The timestamp `secs` seconds before the harness clock's start.
fn ago(secs: i64) -> String {
    iso(BASE - secs * 1000)
}

fn running(run: &str, agent: &str, started: &str, event: &str) -> (String, RunningEntry) {
    (
        run.to_string(),
        RunningEntry {
            run_id: run.into(),
            agent_id: agent.into(),
            task_id: format!("task_{run}"),
            pid: 1,
            started_at: started.into(),
            last_event_at: event.into(),
            remote: None,
        },
    )
}

/// A task assigned to `agent`, last updated at `updated`.
fn assigned(id: &str, agent: &str, status: TaskStatus, updated: &str) -> Task {
    let mut t = make_task(id, id, status);
    t.assignee = Some(agent.into());
    t.updated_at = updated.into();
    t
}

fn updated_backend(
    agents: Vec<Agent>,
    runs: Vec<(String, RunningEntry)>,
    tasks: Vec<Task>,
) -> FakeBackend {
    let mut backend = FakeBackend::new().with_agents(agents).with_tasks(tasks);
    for (id, entry) in runs {
        backend.state.running.insert(id, entry);
    }
    backend
}

/// The characters of `line` from display column `from`, `cells` wide.
fn cells_at(line: &str, from: u16, cells: u16) -> String {
    let mut col = 0u16;
    let mut out = String::new();
    for ch in line.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
        if col >= from && col + w <= from + cells {
            out.push(ch);
        }
        col += w;
    }
    out
}

/// The frame's last row: the footer hint bar.
fn footer(h: &mut Harness) -> String {
    let text = h.text();
    text.lines()
        .last()
        .expect("a frame has rows")
        .trim_end()
        .to_string()
}

/// The UPDATED cell of the row naming `agent`, trimmed. The UPDATED column is
/// the last data column; whatever its alignment, the cell lies within the
/// eight cells that start at its header label (one cell earlier is the gap).
#[track_caller]
fn updated_cell(h: &mut Harness, agent: &str) -> String {
    let text = h.text();
    let header = header_line(h);
    let x = col_of(&header, "UPDATED")
        .unwrap_or_else(|| panic!("no UPDATED column in {header:?}\n{text}"));
    let row = text
        .lines()
        .find(|l| l.contains(agent))
        .unwrap_or_else(|| panic!("no row for {agent}:\n{text}"));
    cells_at(row, x.saturating_sub(1), 10).trim().to_string()
}

/// Open the Agents keyword prompt (`^K`).
fn open_filter(h: &mut Harness) {
    h.press_mod(
        ratatui::crossterm::event::KeyCode::Char('k'),
        ratatui::crossterm::event::KeyModifiers::CONTROL,
    );
}

/// Replace the keyword in the open prompt.
fn set_filter(h: &mut Harness, keyword: &str) {
    h.app.state.agent_keyword.reset("");
    h.paste(keyword);
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
/// Offline and Disabled never share a section here; their relative STATUS
/// sort rank has its own test,
/// `r1_sort_by_status_puts_offline_before_disabled_in_the_ranking`.
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

/// Terminal height for [`world`]. Status mode draws the most list rows over
/// [`world_agents`]: the header, five section rules and six agents (12). The
/// list pane is capped at half the content height (`layout::regions`), which
/// is 11 rows at [`HIGH`], so the last agent would scroll out of view and the
/// screen-read oracles would miss it. 32 rows give the list 12.
const WORLD_HIGH: u16 = 32;

fn world(width: u16) -> Harness {
    agents_view(world_backend(), width, WORLD_HIGH)
}

/// Section labels with the agent names under each, in row order.
type Sections = Vec<(&'static str, Vec<&'static str>)>;

/// The sections each mode draws over [`world_agents`], agents in NAME order
/// (the order a name-ascending sort gives inside every section).
fn world_sections(mode: AgentGroupBy) -> Sections {
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
fn r1_sort_by_status_puts_offline_before_disabled_in_the_ranking() {
    // Ruling: adjudication v2 "Status order" - the STATUS sort rank is
    // ACTIVE, IDLE, ERROR, OFFLINE, DISABLED, the same as the Status
    // sections (today's `agent_status_order` ranks Disabled before Offline).
    // All five statuses, scrambled against index order, in one section.
    let statuses = [
        AgentStatus::Disabled,
        AgentStatus::Offline,
        AgentStatus::Idle,
        AgentStatus::Error,
        AgentStatus::Running,
    ];
    let agents = trap_agents(5, |i, a| a.status = statuses[i]);
    // Running(i4) Idle(i2) Error(i3) Offline(i1) Disabled(i0)
    check_order(
        AgentSortField::Status,
        agents,
        vec![],
        &["id5", "id7", "id6", "id8", "id9"],
        &["id9", "id8", "id6", "id7", "id5"],
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
fn r1_runs_cell_shows_completed_over_total_for_an_active_agent_too() {
    // Ruling: adjudication v2 "RT-D4 RUNS" - the lifetime statistic is shown
    // for every row, so the sorted quantity is always the visible one. An
    // active agent used to show its elapsed time here instead.
    let cells = [(1u64, 1u64), (10, 5), (100, 80), (0, 0)];
    let agents: Vec<Agent> = cells
        .iter()
        .enumerate()
        .map(|(i, (total, done))| {
            let mut a = make_agent(
                &format!("a{i}"),
                &format!("Agx-{i:02}"),
                AgentStatus::Running,
            );
            a.stats.total_runs = *total;
            a.stats.tasks_completed = *done;
            a
        })
        .collect();
    let runs = (0..cells.len())
        .map(|i| running(&format!("r{i}"), &format!("a{i}"), &ago(120), &ago(30)))
        .collect();
    let mut h = agents_view(updated_backend(agents, runs, vec![]), WIDE, HIGH);
    let text = h.text();
    for (name, want) in [
        ("Agx-00", "1/1 ✓"),
        ("Agx-01", "5/10 ✓"),
        ("Agx-02", "80/100 ✓"),
    ] {
        let row = text.lines().find(|l| l.contains(name)).unwrap();
        assert!(row.contains(want), "{name}: {row:?}");
    }
    // Elapsed time (the live run began 2 minutes ago) is not in the cell.
    for name in ["Agx-00", "Agx-01", "Agx-02", "Agx-03"] {
        let row = text.lines().find(|l| l.contains(name)).unwrap();
        assert!(!row.contains("2:00"), "{name} shows elapsed: {row:?}");
    }
    // A zero-total row has nothing to show, even while active.
    let hy = header_y(&mut h);
    let header = h.line(hy);
    let row = text.lines().find(|l| l.contains("Agx-03")).unwrap();
    assert_eq!(
        cells_at(row, end_col_of(&header, "RUNS").unwrap() - 9, 9).trim(),
        "—",
        "{row:?}"
    );
}

#[test]
fn r1_sort_by_runs_ranks_active_agents_by_lifetime_total_not_elapsed() {
    // Ruling: adjudication v2 "RT-D4 RUNS", "RUNS sort key = stats.total_runs".
    // The longest-running live run has the fewest lifetime runs, so an
    // elapsed-time sort would reverse this order.
    let totals = [1u64, 10, 100, 0];
    let agents = trap_agents(4, |i, a| {
        a.status = AgentStatus::Running;
        a.stats.total_runs = totals[i];
        a.stats.tasks_completed = totals[i] / 2;
    });
    let started = [900, 600, 60, 30];
    let runs = (0..4)
        .map(|i| {
            running(
                &format!("r{i}"),
                &format!("id{}", 9 - i),
                &ago(started[i]),
                &ago(started[i] / 2),
            )
        })
        .collect();
    let mut h = agents_view(updated_backend(agents, runs, vec![]), WIDE, HIGH);
    set_sort(&mut h, AgentSortField::Runs, false);
    // 1(i0) 10(i1) 100(i2), then the zero-total i3.
    assert_eq!(ids(&h), ["id9", "id8", "id7", "id6"], "ascending");
    set_sort(&mut h, AgentSortField::Runs, true);
    assert_eq!(ids(&h), ["id7", "id8", "id9", "id6"], "descending");
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

// ── R1: UPDATED (adjudication v2, Issue 1 = A1) ──────────────────────────────
//
// `R <age>` from the newest valid `started_at` / `last_event_at` of the agent's
// running entries, else `T <age>` from the newest valid `updated_at` of the
// tasks assigned to it, else `—`. Sorting by the timestamp alone: ascending is
// oldest first, descending newest first, `—` last both ways, index-order ties.
// Agents are `trap_agents(..)`: id9 id8 id7 ... in source order i0 i1 i2 ...

fn updated_order(h: &mut Harness, descending: bool) -> Vec<String> {
    set_sort(h, AgentSortField::Updated, descending);
    ids(h).into_iter().map(String::from).collect()
}

#[test]
fn r1_sort_by_updated_orders_agents_by_the_newest_run_timestamp_oldest_first() {
    // Ruling: Issue 1 A1 - `R` is the max over BOTH fields of EVERY running
    // entry of the agent.
    //   i0: two entries; the newer one (event 60s ago) is inserted second
    //   i1: started 10s ago, last event 500s ago (started is the newer field)
    //   i2: no evidence
    //   i3: started 400s ago, last event 300s ago
    // newest per agent: i0 60s, i1 10s, i3 300s => oldest first i3 i0 i1.
    let agents = trap_agents(4, |_, _| {});
    let runs = vec![
        running("r0a", "id9", &ago(900), &ago(800)),
        running("r1", "id8", &ago(10), &ago(500)),
        running("r3", "id6", &ago(400), &ago(300)),
        running("r0b", "id9", &ago(70), &ago(60)),
    ];
    let mut h = agents_view(updated_backend(agents, runs, vec![]), WIDE, HIGH);
    assert_eq!(updated_order(&mut h, false), ["id6", "id9", "id8", "id7"]);
    assert_eq!(updated_order(&mut h, true), ["id8", "id9", "id6", "id7"]);
}

#[test]
fn r1_updated_cells_show_r_with_the_age_of_the_newest_run_timestamp_and_a_dash_for_none() {
    // Ruling: Issue 1 A1 - cell text `R <age>` / `—`, same fixture as above.
    let agents = trap_agents(4, |i, a| a.name = format!("Agx-0{i}"));
    let runs = vec![
        running("r0a", "id9", &ago(900), &ago(800)),
        running("r1", "id8", &ago(10), &ago(500)),
        running("r3", "id6", &ago(400), &ago(300)),
        running("r0b", "id9", &ago(70), &ago(60)),
    ];
    let mut h = agents_view(updated_backend(agents, runs, vec![]), WIDE, HIGH);
    assert_eq!(updated_cell(&mut h, "Agx-00"), "R 1m");
    assert_eq!(updated_cell(&mut h, "Agx-01"), "R 10s");
    assert_eq!(updated_cell(&mut h, "Agx-03"), "R 5m");
    assert_eq!(updated_cell(&mut h, "Agx-02"), "—");
}

#[test]
fn r1_updated_falls_back_to_the_newest_assigned_task_update_and_marks_it_t() {
    // Ruling: Issue 1 A1 - agents with no run evidence use the newest
    // `updated_at` among the tasks currently assigned to them, whatever the
    // task's status (a Done task still counts).
    //   i0: Done task 30s ago
    //   i1: two tasks, 500s then 200s ago (the older is listed first)
    //   i2: nothing
    //   i3: Todo task 120s ago
    let agents = trap_agents(4, |i, a| a.name = format!("Agx-0{i}"));
    let tasks = vec![
        assigned("t0", "id9", TaskStatus::Done, &ago(30)),
        assigned("t1a", "id8", TaskStatus::Done, &ago(500)),
        assigned("t1b", "id8", TaskStatus::Todo, &ago(200)),
        assigned("t3", "id6", TaskStatus::Todo, &ago(120)),
    ];
    let mut h = agents_view(updated_backend(agents, vec![], tasks), WIDE, HIGH);
    assert_eq!(updated_cell(&mut h, "Agx-00"), "T 30s");
    assert_eq!(updated_cell(&mut h, "Agx-01"), "T 3m");
    assert_eq!(updated_cell(&mut h, "Agx-03"), "T 2m");
    assert_eq!(updated_cell(&mut h, "Agx-02"), "—");
    // oldest first: i1 (200s) i3 (120s) i0 (30s), then the blank i2.
    assert_eq!(updated_order(&mut h, false), ["id8", "id6", "id9", "id7"]);
    assert_eq!(updated_order(&mut h, true), ["id9", "id6", "id8", "id7"]);
}

#[test]
fn r1_an_agent_that_never_ran_but_was_assigned_a_task_shows_t_not_r() {
    // Ruling: Issue 1 A1 - an administrative assignment bumps the task's
    // `updated_at`; with no run it must read `T`, never `R`, and the agent
    // stays Idle with no statistics.
    let mut ran = make_agent("ran", "Agx-00", AgentStatus::Running);
    ran.stats.total_runs = 1;
    let idle = make_agent("idle", "Agx-01", AgentStatus::Idle);
    let tasks = vec![assigned("t1", "idle", TaskStatus::Todo, &ago(45))];
    let runs = vec![running("r0", "ran", &ago(90), &ago(80))];
    let mut h = agents_view(updated_backend(vec![ran, idle], runs, tasks), WIDE, HIGH);
    assert_eq!(updated_cell(&mut h, "Agx-01"), "T 45s");
    assert_eq!(updated_cell(&mut h, "Agx-00"), "R 1m");
}

#[test]
fn r1_run_evidence_takes_precedence_over_a_newer_assigned_task_update() {
    // Ruling: Issue 1 A1 - `R` wins even when the task was touched later.
    //   x: run event 600s ago, assigned task updated 30s ago
    //   y: only an assigned task, 120s ago
    // x's key is 600s (R), so ascending puts x first; if the newer task time
    // leaked into x's key (30s), y would come first.
    let x = make_agent("x", "Agx-00", AgentStatus::Idle);
    let y = make_agent("y", "Agx-01", AgentStatus::Idle);
    let tasks = vec![
        assigned("tx", "x", TaskStatus::Todo, &ago(30)),
        assigned("ty", "y", TaskStatus::Todo, &ago(120)),
    ];
    let runs = vec![running("rx", "x", &ago(700), &ago(600))];
    let mut h = agents_view(updated_backend(vec![x, y], runs, tasks), WIDE, HIGH);
    assert_eq!(updated_cell(&mut h, "Agx-00"), "R 10m");
    assert_eq!(updated_cell(&mut h, "Agx-01"), "T 2m");
    assert_eq!(updated_order(&mut h, false), ["x", "y"]);
    assert_eq!(updated_order(&mut h, true), ["y", "x"]);
}

#[test]
fn r1_invalid_timestamps_are_skipped_not_read_as_epoch_or_now() {
    // Ruling: Issue 1 A1 - invalid timestamps are skipped; a valid epoch zero
    // is a real (very old) time, not a blank.
    //   i0: started is garbage, event 60s ago      -> R 1m
    //   i1: both run times garbage, task 200s ago  -> T 3m (falls through)
    //   i2: both run times garbage, no task        -> blank
    //   i3: only a task with a garbage updated_at  -> blank
    //   i4: event at the epoch                     -> R 16m, the oldest
    let agents = trap_agents(5, |i, a| a.name = format!("Agx-0{i}"));
    let runs = vec![
        running("r0", "id9", "not-a-date", &ago(60)),
        running("r1", "id8", "garbage", "also garbage"),
        running("r2", "id7", "", "yesterday"),
        running(
            "r4",
            "id5",
            "1970-01-01T00:00:00.000Z",
            "1970-01-01T00:00:00.000Z",
        ),
    ];
    let tasks = vec![
        assigned("t1", "id8", TaskStatus::Todo, &ago(200)),
        assigned("t3", "id6", TaskStatus::Todo, "never"),
    ];
    let mut h = agents_view(updated_backend(agents, runs, tasks), WIDE, HIGH);
    assert_eq!(updated_cell(&mut h, "Agx-00"), "R 1m");
    assert_eq!(updated_cell(&mut h, "Agx-01"), "T 3m");
    assert_eq!(updated_cell(&mut h, "Agx-02"), "—");
    assert_eq!(updated_cell(&mut h, "Agx-03"), "—");
    assert_eq!(updated_cell(&mut h, "Agx-04"), "R 16m");
    // epoch(i4) < 200s(i1) < 60s(i0), then blanks i2 i3 in index order.
    assert_eq!(
        updated_order(&mut h, false),
        ["id5", "id8", "id9", "id7", "id6"]
    );
    assert_eq!(
        updated_order(&mut h, true),
        ["id9", "id8", "id5", "id7", "id6"]
    );
}

#[test]
fn r1_updated_with_equal_timestamps_keeps_index_order_both_ways() {
    // Ruling: contract "ties fall back to index order"; the source (R or T)
    // does not break a tie.
    let agents = trap_agents(3, |_, _| {});
    let runs = vec![running("r2", "id7", &ago(100), &ago(100))];
    let tasks = vec![
        assigned("t0", "id9", TaskStatus::Todo, &ago(100)),
        assigned("t1", "id8", TaskStatus::Todo, &ago(100)),
    ];
    let mut h = agents_view(updated_backend(agents, runs, tasks), WIDE, HIGH);
    assert_eq!(updated_order(&mut h, false), ["id9", "id8", "id7"]);
    assert_eq!(updated_order(&mut h, true), ["id9", "id8", "id7"]);
}

#[test]
fn r1_the_updated_marker_survives_a_cell_too_narrow_for_the_age() {
    // Ruling: Issue 1 A1 - "the marker survives truncation". The UPDATED cell
    // is 8 cells; 100000 days reads `100000d`, which with `R `/`T ` is 9.
    let a = make_agent("a", "Agx-00", AgentStatus::Idle);
    let b = make_agent("b", "Agx-01", AgentStatus::Idle);
    let epoch = "1970-01-01T00:00:00.000Z";
    let runs = vec![running("r0", "a", epoch, epoch)];
    let tasks = vec![assigned("t1", "b", TaskStatus::Todo, epoch)];
    let mut h = agents_view(updated_backend(vec![a, b], runs, tasks), WIDE, HIGH);
    // Jump the clock; `advance` would also tick, which replays the spinner
    // animation once per 80 ms of the jump.
    h.clock.set_ms(BASE + 100_000 * 86_400_000);
    let r = updated_cell(&mut h, "Agx-00");
    let t = updated_cell(&mut h, "Agx-01");
    assert!(r.starts_with("R "), "{r:?}");
    assert!(t.starts_with("T "), "{t:?}");
    assert!(
        r.chars().count() > 2 && t.chars().count() > 2,
        "{r:?} {t:?}"
    );
    for cell in [&r, &t] {
        assert!(UnicodeWidthStr::width(cell.as_str()) <= 8, "{cell:?}");
    }
}

#[test]
fn r5_the_updated_header_and_cell_share_their_column_at_every_width_60_to_200() {
    // The row's `T 5m` lies inside the eight cells of its header label, and
    // the header and the cell are present together.
    for width in 60..=200u16 {
        let mut h = agents_view(alignment_backend(), width, HIGH);
        let text = h.text();
        let header = header_line(&mut h);
        let row = text.lines().find(|l| l.contains("Walter")).unwrap();
        // Not vacuous: the column exists from 93 (see the priority test).
        assert!(
            width < 93 || col_of(&header, "UPDATED").is_some(),
            "{width}: no UPDATED column\n{header}"
        );
        assert_eq!(
            col_of(&header, "UPDATED").is_some(),
            col_of(row, "T 5m").is_some(),
            "{width}: UPDATED shown on one side only\n{header}\n{row}"
        );
        if let (Some(label), Some(cell)) = (col_of(&header, "UPDATED"), col_of(row, "T 5m")) {
            assert!(
                (label..label + 8).contains(&cell) && cell + 4 <= label + 8,
                "{width}: cell at {cell}, label at {label}\n{header}\n{row}"
            );
        }
    }
}

#[test]
fn r1_the_footer_explains_r_and_t_while_the_updated_sort_is_active() {
    // Ruling: Issue 1 A1 - legend `R run · T task` in the footer while the
    // Updated sort is active, also when the column is hidden (80).
    for width in [80u16, WIDE] {
        let mut h = world(width);
        set_sort(&mut h, AgentSortField::Updated, false);
        let bar = footer(&mut h);
        assert!(bar.contains("R run · T task"), "{width}: {bar:?}");
        set_sort(&mut h, AgentSortField::Updated, true);
        let bar = footer(&mut h);
        assert!(bar.contains("R run · T task"), "{width} desc: {bar:?}");
    }
}

#[test]
fn r1_the_footer_drops_the_r_and_t_legend_for_any_other_sort() {
    for (field, _) in FIELDS {
        if field == AgentSortField::Updated {
            continue;
        }
        let mut h = world(WIDE);
        set_sort(&mut h, field, false);
        let bar = footer(&mut h);
        assert!(!bar.contains("R run · T task"), "{field:?}: {bar:?}");
    }
}

// ── R1: the active arrow survives a narrow column ────────────────────────────

#[test]
fn r1_the_sort_arrow_stays_visible_when_the_adapter_model_label_is_cut() {
    // Ruling: adjudication v2 "Header arrow on a narrow active column" - at
    // 60 and 80 MODEL is 10 cells, narrower than `ADAPTER/MODEL↑`; the label
    // is cut, never the arrow.
    for width in [60u16, 80] {
        for (descending, arrow) in [(false, '↑'), (true, '↓')] {
            let mut h = agents_view(alignment_backend(), width, HIGH);
            set_sort(&mut h, AgentSortField::AdapterModel, descending);
            let header = header_line(&mut h);
            let x = col_of(&header, "ADAPTER")
                .unwrap_or_else(|| panic!("{width}: no ADAPTER label in {header:?}"));
            let cell = cells_at(&header, x, 10);
            assert!(cell.contains(arrow), "{width}: {cell:?} in {header:?}");
            assert_eq!(
                header.matches('↑').count() + header.matches('↓').count(),
                1,
                "{width}: {header:?}"
            );
        }
    }
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

/// A named action on the harness.
type Step = (String, Box<dyn Fn(&mut Harness)>);

/// Every action that reorders the list. Each must leave the selected agent
/// selected (by id), wherever it lands.
fn reorder_steps() -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
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
    // Ruling: Issue 2 - the chip exists from 108 (GROUP_CHIP_FROM) up.
    for width in CHIP_WIDTHS {
        let mut h = world(width);
        h.app.state.agent_selected = 3;
        let id = selected_id(&h);
        for round in 0..MODES.len() {
            h.click_text("GROUP: ");
            assert_eq!(selected_id(&h), id, "{width}: after chip click {round}");
        }
    }
}

#[test]
fn r2_the_selected_agent_survives_clicking_the_footer_mode_hint_all_the_way_round() {
    // Ruling: Issue 2 - below 108 the footer's `b <Mode>` hint is the click
    // target (there is no chip at 80).
    let mut h = world(80);
    h.app.state.agent_selected = 3;
    let id = selected_id(&h);
    for (round, (_, word)) in MODES.iter().enumerate() {
        h.click_text(&format!("b {word}"));
        assert_eq!(selected_id(&h), id, "after hint click {round}");
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
    let by_status: [(AgentGroupBy, Sections); 5] = [
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
fn r3_an_agent_with_several_caps_is_listed_once_under_its_alphabetically_first_cap() {
    // Ruling: adjudication v2 "RT-D3 Caps sections" (design D2) - one row per
    // agent, under the alphabetically-first capability, labelled with the
    // plain capability name; `no caps` only for an agent with none.
    let mut agents = world_agents();
    agents[2].capabilities = vec!["web".into(), "linux".into(), "macos".into()];
    let mut h = agents_view(FakeBackend::new().with_agents(agents), WIDE, HIGH);
    set_mode(&mut h, AgentGroupBy::Caps);
    let text = h.text();
    let sections = grouped(&text); // asserts count == rows per section
    assert_eq!(
        sections,
        expected(&[
            // Default sort: Error (Agx-02) ranks before Disabled (Agx-04).
            ("linux", &["Agx-02", "Agx-04"]),
            ("macos", &["Agx-01", "Agx-03"]),
            ("web", &["Agx-05", "Agx-00"]),
        ])
    );
    assert_eq!(text.matches("Agx-02").count(), 1, "one row only:\n{text}");
    assert!(!text.contains("primary"), "label is the plain cap:\n{text}");
}

#[test]
fn r3_the_caps_cell_lists_every_capability_of_the_agent() {
    // Ruling: "CAPS cell and Overview list all caps" - placement is by the
    // first cap, the cell is not.
    let mut a = make_agent("a1", "Agx-00", AgentStatus::Idle);
    a.capabilities = vec!["linux".into(), "gpu".into()];
    let mut h = agents_view(FakeBackend::new().with_agents(vec![a]), WIDE, HIGH);
    let text = h.text();
    let row = text.lines().find(|l| l.contains("Agx-00")).unwrap();
    assert!(row.contains("gpu") && row.contains("linux"), "{row:?}");
}

/// Three agents whose only capability words are `gpu` and `linux`: alpha and
/// beta have both, gamma only `linux`. No other searchable field (id, name,
/// status, adapter, model, node, team) contains either word.
fn caps_trio() -> FakeBackend {
    let caps: [&[&str]; 3] = [&["gpu", "linux"], &["gpu", "linux"], &["linux"]];
    let agents = caps
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut a = make_agent(&format!("a{i}"), &format!("Agx-{i:02}"), AgentStatus::Idle);
            a.capabilities = c.iter().map(|s| s.to_string()).collect();
            a
        })
        .collect();
    FakeBackend::new().with_agents(agents)
}

#[test]
fn r3_the_keyword_filter_matches_secondary_capabilities() {
    // Ruling: adjudication v2 "RT-D3" - the keyword matches EVERY capability,
    // so gamma (primary `linux`) and alpha/beta (primary `gpu`, secondary
    // `linux`) all answer to `linux`. At 80 the CAPS column is hidden, so the
    // filter and the section labels are the only way to see the capability.
    let mut h = agents_view(caps_trio(), 80, HIGH);
    set_mode(&mut h, AgentGroupBy::Caps);
    assert_eq!(
        grouped(&h.text()),
        expected(&[("gpu", &["Agx-00", "Agx-01"]), ("linux", &["Agx-02"])]),
        "unfiltered"
    );
    open_filter(&mut h);
    for (keyword, want) in [
        (
            "linux",
            expected(&[("gpu", &["Agx-00", "Agx-01"]), ("linux", &["Agx-02"])]),
        ),
        ("GPU", expected(&[("gpu", &["Agx-00", "Agx-01"])])),
        ("no-match", expected(&[])),
    ] {
        set_filter(&mut h, keyword);
        assert_eq!(grouped(&h.text()), want, "keyword {keyword:?}");
    }
}

#[test]
fn r3_a_secondary_capability_keyword_leaves_the_matching_agent_selectable_once() {
    let mut h = agents_view(caps_trio(), 80, HIGH);
    open_filter(&mut h);
    set_filter(&mut h, "linux");
    let mut found: Vec<&str> = ids(&h);
    found.sort();
    assert_eq!(found, ["a0", "a1", "a2"]);
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
    // Ruling: Issue 2 - chip tests run at 108 (the first chip width), 120, 160.
    for width in CHIP_WIDTHS {
        let mut h = world(width);
        for (mode, word) in MODES {
            set_mode(&mut h, mode);
            let line = header_line(&mut h);
            let chip = format!("GROUP: {word}");
            assert!(
                line.trim_end().ends_with(&chip),
                "{width}, mode {word}: header row {line:?} does not end with {chip:?}"
            );
        }
    }
}

#[test]
fn r4_the_group_chip_is_drawn_once_and_only_on_the_header_row() {
    for width in CHIP_WIDTHS {
        let mut h = world(width);
        let text = h.text();
        assert_eq!(text.matches("GROUP: ").count(), 1, "{width}:\n{text}");
        let y = header_y(&mut h);
        assert!(h.line(y).contains("GROUP: Team"), "{width}");
    }
}

#[test]
fn r4_the_group_chip_is_separated_from_the_last_column_label() {
    for width in CHIP_WIDTHS {
        let mut h = world(width);
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
        assert!(
            before.ends_with(' '),
            "{width}: chip glued to a label: {line:?}"
        );
    }
}

#[test]
fn r4_the_group_chip_fills_the_last_14_cells_after_a_one_cell_gap() {
    // Ruling: Issue 2 / design D4 - 14 cells of chip + 1 unclickable gap at
    // the right edge; `GROUP: Adapter` (14) is the longest label and fits.
    for width in CHIP_WIDTHS {
        let mut h = world(width);
        for (mode, word) in MODES {
            set_mode(&mut h, mode);
            let line = header_line(&mut h);
            let chip = format!("GROUP: {word}");
            let end = end_col_of(&line, &chip).unwrap();
            assert_eq!(end, width, "{width}, {word}: chip ends at the edge");
            assert!(
                col_of(&line, &chip).unwrap() >= width - 14,
                "{width}, {word}: chip starts inside its 14 cells: {line:?}"
            );
        }
    }
}

#[test]
fn r4_the_chip_never_overlaps_the_last_sort_header_and_its_gap_is_not_clickable() {
    // Ruling: Issue 2 chip hit contract - "never overlapping the last
    // SortHeader zone; gap unclickable".
    for width in CHIP_WIDTHS {
        let mut h = world(width);
        let y = header_y(&mut h);
        let line = h.line(y);
        let chip_x = col_of(&line, "GROUP: ").unwrap();
        for x in chip_x..width {
            assert!(
                !matches!(
                    h.zone_at(x, y).map(|z| &z.kind),
                    Some(ZoneKind::SortHeader(_))
                ),
                "{width}: chip cell {x} is a sort header"
            );
        }
        let gap = h.zone_at(width - 15, y).map(|z| z.kind.clone());
        assert!(
            !matches!(gap, Some(ZoneKind::SortHeader(_)) | Some(ZoneKind::Verb(_))),
            "{width}: the gap cell is {gap:?}"
        );
        let updated = label_cell(&mut h, "UPDATED");
        assert_eq!(
            h.zone_at(updated.0, updated.1).map(|z| z.kind.clone()),
            Some(ZoneKind::SortHeader(SortColumn::Agent(
                AgentSortField::Updated
            ))),
            "{width}: UPDATED stays a sort header"
        );
    }
}

#[test]
fn r4_clicking_the_group_chip_cycles_to_the_next_mode() {
    for width in CHIP_WIDTHS {
        let mut h = world(width);
        for step in 1..=MODES.len() {
            h.click_text("GROUP: ");
            assert_eq!(
                h.app.state.agent_group_by,
                MODES[step % MODES.len()].0,
                "{width}: after {step} chip clicks"
            );
        }
    }
}

#[test]
fn r4_the_group_chip_is_a_hit_zone_of_its_own_not_a_sort_header_or_a_row() {
    for width in CHIP_WIDTHS {
        let mut h = world(width);
        let (x, y) = h.find_text("GROUP: Team").expect("chip on screen");
        let zone = h.zone_at(x, y).expect("the chip has a hit zone").clone();
        assert!(
            !matches!(
                zone.kind,
                ZoneKind::SortHeader(_) | ZoneKind::AgentRow { .. } | ZoneKind::Scroll(_)
            ),
            "{width}: chip zone is {:?}",
            zone.kind
        );
        // And no sort header shares its zone id: pressing it can never pair
        // with a column.
        for (field, label) in FIELDS {
            let (lx, ly) = label_cell(&mut h, label);
            assert_ne!(
                h.zone_at(lx, ly).map(|z| z.id.clone()),
                Some(zone.id.clone()),
                "{width}: {field:?}"
            );
        }
    }
}

#[test]
fn r4_the_group_chip_covers_its_whole_text() {
    for width in CHIP_WIDTHS {
        let mut h = world(width);
        let (x, y) = h.find_text("GROUP: Team").unwrap();
        let first = h.zone_at(x, y).map(|z| z.id.clone());
        let last = h
            .zone_at(x + "GROUP: Team".len() as u16 - 1, y)
            .map(|z| z.id.clone());
        assert!(first.is_some(), "{width}");
        assert_eq!(first, last, "{width}");
    }
}

#[test]
fn r4_clicking_the_chip_does_not_sort() {
    for width in CHIP_WIDTHS {
        let mut h = world(width);
        h.key('o');
        let sort = h.app.state.agent_sort;
        h.click_text("GROUP: ");
        assert_eq!(h.app.state.agent_sort, sort, "{width}");
    }
}

// ── R4: the mode without a chip - the footer's `b <Mode>` hint ───────────────
//
// Ruling: adjudication v2 Issue 2 - "the mode is always visible without the
// chip": the Agents footer shows `b <Mode>` as the first hint (after the
// mouse-off marker, which the default harness does not show), clickable, at
// every width.

#[test]
fn r4_the_footer_leads_with_the_b_mode_hint_at_every_width_and_in_every_mode() {
    for width in [60u16, 80, 107, GROUP_CHIP_FROM, 120, WIDE] {
        let mut h = world(width);
        for (mode, word) in MODES {
            set_mode(&mut h, mode);
            let bar = footer(&mut h);
            assert!(
                bar.trim_start().starts_with(&format!("b {word}")),
                "{width}, {word}: footer {bar:?}"
            );
        }
    }
}

#[test]
fn r4_clicking_the_footer_mode_hint_cycles_the_mode_where_there_is_no_chip() {
    for width in [60u16, 80, 107] {
        let mut h = world(width);
        for step in 1..=MODES.len() {
            let word = MODES[(step - 1) % MODES.len()].1;
            h.click_text(&format!("b {word}"));
            assert_eq!(
                h.app.state.agent_group_by,
                MODES[step % MODES.len()].0,
                "{width}: after {step} hint clicks"
            );
        }
    }
}

#[test]
fn r4_the_footer_mode_hint_is_a_hit_zone_that_is_neither_a_header_nor_a_row() {
    let mut h = world(80);
    let (x, y) = h.find_text("b Team").expect("hint on screen");
    let kind = h.zone_at(x, y).map(|z| z.kind.clone());
    assert!(
        matches!(
            kind,
            Some(ZoneKind::FooterHint(_)) | Some(ZoneKind::Verb(_))
        ),
        "footer hint zone is {kind:?}"
    );
}

#[test]
fn r4_clicking_the_footer_mode_hint_does_not_sort() {
    let mut h = world(80);
    h.key('o');
    let sort = h.app.state.agent_sort;
    h.click_text("b Team");
    assert_eq!(h.app.state.agent_sort, sort);
}

#[test]
fn r4_the_footer_names_a_sort_whose_column_is_hidden_at_this_width() {
    // Ruling: Issue 2 - "when the active sort column is hidden at the current
    // width the footer also shows `o <Field>`". CAPS is hidden at 80, so
    // sorting by it would otherwise be a silent mode.
    let mut h = world(80);
    set_sort(&mut h, AgentSortField::Caps, false);
    assert!(!header_line(&mut h).contains("CAPS"));
    let bar = footer(&mut h);
    assert!(bar.contains("o Caps"), "{bar:?}");
}

#[test]
fn r4_there_is_no_mini_chip_between_40_and_59_columns() {
    // Ruling: "Narrow 40-59. No `G: <Mode>` mini-chip; footer only."
    for width in [40u16, 50, 59] {
        let mut h = world(width);
        let text = h.text();
        assert!(
            !text.contains("GROUP:") && !text.contains("G: "),
            "{width}:\n{text}"
        );
        let bar = footer(&mut h);
        assert!(bar.trim_start().starts_with("b Team"), "{width}: {bar:?}");
    }
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
    // Five minutes before the held clock: the agent's UPDATED cell reads
    // `T 5m`.
    t.updated_at = ago(300);
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

/// Which of today's five optional columns a row `width` cells wide shows,
/// as `(model, task, caps, place, runs)` - the allocator of
/// `AgentColumns::for_width` at 16de51a, written out: lead 16 + name 10, then
/// the optional columns are granted in the order runs 9, task 12, where 12,
/// model 10, caps 10, each needing more than its width left (and costing one
/// more cell for its gap). Adjudication v2 keeps this exactly (R5 = "today's
/// column-drop order"); UPDATED and the chip are appended after it.
fn todays_columns(width: u16) -> (bool, bool, bool, bool, bool) {
    let mut avail = usize::from(width).saturating_sub(16 + 10);
    let mut take = |w: usize| {
        if avail > w {
            avail -= w + 1;
            true
        } else {
            false
        }
    };
    let runs = take(9);
    let task = take(12);
    let place = take(12);
    let model = take(10);
    let caps = take(10);
    (model, task, caps, place, runs)
}

#[test]
fn r5_todays_five_optional_columns_keep_their_availability_at_every_width_60_to_200() {
    // Ruling: adjudication v2 Issue 2 - "the column allocator stays today's
    // algorithm"; UPDATED and the chip may not take a cell from a column that
    // was there. The 9 pinned widths of the test above are points of this
    // sweep.
    for width in 60..=200u16 {
        let mut h = agents_view(alignment_backend(), width, HIGH);
        let header = header_line(&mut h);
        let (model, task, caps, place, runs) = todays_columns(width);
        for (label, shown) in [
            ("ADAPTER", model),
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
fn r5_updated_is_the_lowest_priority_column_dropped_first_and_present_from_93() {
    // Ruling: Issue 2 - UPDATED is appended after CAPS: at 93 (= the five old
    // columns at their minimums + 9) it first shows next to all of them.
    // 60/70/80/90 are today's layouts exactly, without it.
    for width in [60u16, 70, 80, 90] {
        let mut h = agents_view(alignment_backend(), width, HIGH);
        let header = header_line(&mut h);
        assert!(!header.contains("UPDATED"), "{width}: {header:?}");
    }
    for width in 93..=200u16 {
        let mut h = agents_view(alignment_backend(), width, HIGH);
        let header = header_line(&mut h);
        assert!(header.contains("UPDATED"), "{width}: {header:?}");
        for label in ["ADAPTER", "TASK", "CAPS", "WHERE", "RUNS"] {
            assert!(header.contains(label), "{width}: {label} in {header:?}");
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
        "UPDATED",
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
fn r5_at_80_columns_the_header_is_todays_layout_with_no_chip_and_no_updated() {
    // Ruling: Issue 2 - 80 = NAME 14, MODEL 10, TASK 15, WHERE 12, RUNS 9:
    // today's layout exactly. CAPS is dropped as before; UPDATED and the chip
    // do not appear and do not displace anything.
    let mut h = agents_view(alignment_backend(), 80, HIGH);
    let header = header_line(&mut h);
    for label in ["STATUS", "NAME", "ADAPTER", "TASK", "WHERE", "RUNS"] {
        assert!(header.contains(label), "{label} missing at 80: {header:?}");
    }
    for label in ["CAPS", "UPDATED", "GROUP"] {
        assert!(!header.contains(label), "{label} at 80: {header:?}");
    }
}

#[test]
fn r5_the_group_chip_is_visible_from_108_and_absent_below() {
    // Ruling: Issue 2 (replaces `..._visible_at_80_and_above`). The chip is
    // chrome budgeted after every column: it shows only when 15 cells are
    // spare, i.e. from table width GROUP_CHIP_FROM = 108.
    for width in 60..=200u16 {
        let mut h = agents_view(alignment_backend(), width, HIGH);
        let text = h.text();
        let header = header_line(&mut h);
        if width >= GROUP_CHIP_FROM {
            assert!(
                header.trim_end().ends_with("GROUP: Team"),
                "{width}: {header:?}"
            );
        } else {
            assert!(!text.contains("GROUP:"), "{width} has a chip:\n{text}");
        }
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
        let rows: Vec<u16> = text
            .lines()
            .enumerate()
            .filter(|(_, l)| l.contains("wnode"))
            .map(|(y, _)| y as u16)
            .collect();
        assert_eq!(rows.len(), 2, "{width}:\n{text}");
        let where_col = cell_col_of(&h, hy, "WHERE");
        assert!(where_col.is_some(), "{width}: no WHERE header:\n{header}");
        for y in rows {
            assert_eq!(
                where_col,
                cell_col_of(&h, y, "wnode"),
                "{width}:\n{header}\n{}",
                h.line(y)
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
// Notes (not asserted):
//
// * Settled by adjudication v2 and now pinned above: Status rank and Status
//   sections (Offline before Disabled), UPDATED (R/T/—), the chip from 108,
//   `b <Mode>` in the footer, RUNS statistics on active rows, Caps placement.
// * Not pinned (the rulings leave them to the build): the footer text of the
//   hidden-sort hint beyond `o <Field>` (direction glyph), the Agent Overview
//   `updated`/`live elapsed`/`node`/caps rows (no case reads the detail
//   pane), the exact 40-59 column layout, and RUNS ties between equal totals
//   with different completed counts (design D3 says the completed/total ratio
//   breaks the tie, plan.md says index order; no case has such a pair).
// * The `GROUP_CHIP_FROM = 108` and 80-column layouts follow the ruling's own
//   arithmetic; the QA gate's tmux captures at 80/120/140 are the acceptance
//   evidence for them, not these cases.
// ═════════════════════════════════════════════════════════════════════════════
