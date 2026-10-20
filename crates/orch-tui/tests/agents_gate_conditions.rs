//! Gate conditions for the Agents table (goal `goal_DHCHB3O`, PR #43), from
//! `adjudication-v3.md`. Three groups, one file because `agents_table.rs` is
//! already 2.5k lines and these cases need their own clock and fixtures:
//!
//! * **I-1 (Ruling 4)** - the Agent Overview `caps`, `node`, `updated` and
//!   `live` rows (`views/detail.rs`) had no test; removing all four left the
//!   suite green. These cases pass on the frozen build and fail without the
//!   rows.
//! * **F1 (Ruling 1)** - timestamp validity at the new Agents observation
//!   boundary (UPDATED evidence and the Overview live start). A timestamp is
//!   valid iff it is `YYYY-MM-DDTHH:MM:SS[.f{1,9}]Z`, a real calendar
//!   date/time, and `0 <= epoch ms <= now + 24h`; an invalid one is skipped,
//!   never clamped. Written against the ruling, so the `f1_*` rejection cases
//!   FAIL on the frozen build and pass on the fix. The valid-time cases
//!   (epoch zero, leap day, tolerance) pass on both.
//! * **Ruling 2** - UPDATED is strictly the lowest-priority column: at every
//!   width 40..=200, UPDATED present means every other optional column is
//!   present. The `c2_*` cases fail on the frozen build at 58, 59, 71, 72, 82
//!   and 83.
//!
//! Everything goes through the rendered frame and the public API, so the
//! cases survive any choice of validator name or seam. The fake backend and a
//! held clock are used; no home directory, network or AI CLI is touched.

mod common;

use common::Harness;
use orch_domain::{Agent, AgentStatus, RunningEntry, Task, TaskStatus};
use orch_tui::backend::{FakeBackend, make_agent, make_task};
use orch_tui::state::{AgentSort, AgentSortField, ViewId};
use ratatui::crossterm::event::KeyCode;
use unicode_width::UnicodeWidthStr;

// ── Time helpers ─────────────────────────────────────────────────────────────

/// Days from 1970-01-01 to `y-m-d` (proleptic Gregorian, Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// The epoch ms of a UTC wall-clock time.
fn ms_of(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> i64 {
    (days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s) * 1000
}

/// An RFC 3339 timestamp for `ms`, in the form this repo writes
/// (`YYYY-MM-DDTHH:MM:SS.mmmZ`).
fn stamp(ms: i64) -> String {
    let (y, m, d) = civil_from_days(ms.div_euclid(86_400_000));
    let t = ms.rem_euclid(86_400_000);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        t / 3_600_000,
        t / 60_000 % 60,
        t / 1000 % 60,
        t % 1000
    )
}

/// The harness clock's start, used by the I-1 and sweep cases.
const BASE: i64 = 1_000_000;

/// The F1 cases run at the real date the security measurements were taken on
/// (2026-10-19 12:00 UTC), so each literal from the report is rejected for
/// the reason it names and not merely because it lies after a 1970 clock.
fn f1_now() -> i64 {
    ms_of(2026, 10, 19, 12, 0, 0)
}

const H: i64 = 3_600_000;

// ── Fixtures and frame helpers ───────────────────────────────────────────────

const WIDE: u16 = 160;
const HIGH: u16 = 32;

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

fn assigned(id: &str, agent: &str, updated: &str) -> Task {
    let mut t = make_task(id, id, TaskStatus::Todo);
    t.assignee = Some(agent.into());
    t.updated_at = updated.into();
    t
}

fn backend(agents: Vec<Agent>, runs: Vec<(String, RunningEntry)>, tasks: Vec<Task>) -> FakeBackend {
    let mut b = FakeBackend::new().with_agents(agents).with_tasks(tasks);
    for (id, entry) in runs {
        b.state.running.insert(id, entry);
    }
    b
}

fn agents_view(b: FakeBackend, now_ms: i64, width: u16, height: u16) -> Harness {
    let mut h = Harness::sized(b, width, height);
    h.clock.set_ms(now_ms);
    // The app samples its sort/validation clock with the data (start-up and
    // every refresh); mirror a start-up at `now_ms` so the fixture's data and
    // that clock agree, as they do in the running app.
    h.app.state.sort_clock_ms = now_ms;
    h.app.state.view = ViewId::Agents;
    h
}

fn header_line(h: &mut Harness) -> String {
    let text = h.text();
    text.lines()
        .find(|l| l.contains("STATUS") && l.contains("NAME"))
        .unwrap_or_else(|| panic!("no agents table header on screen:\n{text}"))
        .to_string()
}

fn col_of(line: &str, needle: &str) -> Option<u16> {
    line.find(needle)
        .map(|byte| UnicodeWidthStr::width(&line[..byte]) as u16)
}

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

/// The UPDATED cell of the row naming `agent`, trimmed (the eight cells that
/// start at the header label, plus the gap before it).
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

/// Agent names in on-screen order after sorting by UPDATED. The group mode is
/// `None` so no section moves a row.
fn updated_order(h: &mut Harness, descending: bool) -> Vec<String> {
    h.app
        .state
        .set_agent_group_by(orch_tui::state::AgentGroupBy::None);
    h.app.state.set_agent_sort(AgentSort {
        field: AgentSortField::Updated,
        descending,
    });
    let text = h.text();
    text.lines()
        .filter_map(|l| {
            let at = l.find("Agx-")?;
            Some(l[at..].chars().take(6).collect::<String>())
        })
        .collect()
}

/// The Agent Overview as whitespace-normalised lines, with the first agent
/// selected and the detail open (`Enter`). Rows are matched by their label
/// and value, never by column position.
fn overview(mut h: Harness) -> Vec<String> {
    h.press(KeyCode::Enter);
    let text = h.text();
    let lines: Vec<String> = text
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    assert!(
        lines.iter().any(|l| l.starts_with("status ")),
        "the Agent Overview is not on screen:\n{text}"
    );
    lines
}

/// The value of the Overview row `label` (`updated`, `live`, ...), if any.
fn row(lines: &[String], label: &str) -> Option<String> {
    lines
        .iter()
        .find_map(|l| l.strip_prefix(&format!("{label} ")).map(String::from))
}

/// One Idle agent `Agx-00`, plus whatever `edit` does to it.
fn agent(edit: impl FnOnce(&mut Agent)) -> Agent {
    let mut a = make_agent("a0", "Agx-00", AgentStatus::Running);
    edit(&mut a);
    a
}

fn overview_of(
    agents: Vec<Agent>,
    runs: Vec<(String, RunningEntry)>,
    tasks: Vec<Task>,
) -> Vec<String> {
    overview(agents_view(backend(agents, runs, tasks), BASE, 140, 50))
}

fn secs_ago(secs: i64) -> String {
    stamp(BASE - secs * 1000)
}

// ═════════════════════════════════════════════════════════════════════════════
// I-1 - the Agent Overview rows (views/detail.rs: caps, node, updated, live)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn i1_the_overview_caps_row_lists_every_capability_in_stored_order() {
    let lines = overview_of(
        vec![agent(|a| {
            a.capabilities = vec!["linux".into(), "gpu".into(), "arm".into()]
        })],
        vec![],
        vec![],
    );
    assert_eq!(
        row(&lines, "caps")
            .as_deref()
            .map(|v| v.split(" node").next().unwrap()),
        Some("linux,gpu,arm")
    );
}

#[test]
fn i1_the_overview_caps_row_is_a_dash_for_an_agent_without_capabilities() {
    let lines = overview_of(vec![agent(|a| a.capabilities = vec![])], vec![], vec![]);
    assert_eq!(
        row(&lines, "caps")
            .as_deref()
            .map(|v| v.split(" node").next().unwrap()),
        Some("—")
    );
}

#[test]
fn i1_the_overview_node_row_says_local_for_a_local_agent() {
    let lines = overview_of(vec![agent(|_| {})], vec![], vec![]);
    assert!(
        lines.iter().any(|l| l.contains(" node local")),
        "{lines:#?}"
    );
}

#[test]
fn i1_the_overview_node_row_names_an_online_remote_node() {
    let lines = overview_of(
        vec![agent(|a| {
            a.remote = Some(orch_domain::RemoteRef {
                alias: "wa".into(),
                online: true,
            })
        })],
        vec![],
        vec![],
    );
    assert!(
        lines.iter().any(|l| l.contains(" node wa online")),
        "{lines:#?}"
    );
}

#[test]
fn i1_the_overview_node_row_names_an_offline_remote_node() {
    let lines = overview_of(
        vec![agent(|a| {
            a.remote = Some(orch_domain::RemoteRef {
                alias: "wb".into(),
                online: false,
            })
        })],
        vec![],
        vec![],
    );
    assert!(
        lines.iter().any(|l| l.contains(" node wb offline")),
        "{lines:#?}"
    );
}

#[test]
fn i1_the_overview_updated_row_for_run_evidence_says_r_and_names_the_source() {
    let runs = vec![running("r0", "a0", &secs_ago(900), &secs_ago(60))];
    let lines = overview_of(vec![agent(|_| {})], runs, vec![]);
    assert_eq!(
        row(&lines, "updated").as_deref(),
        Some("R 1m · newest run start or event")
    );
}

#[test]
fn i1_the_overview_updated_row_for_a_task_only_agent_says_t_and_that_it_may_be_administrative() {
    let tasks = vec![assigned("t0", "a0", &secs_ago(45))];
    let lines = overview_of(vec![agent(|a| a.status = AgentStatus::Idle)], vec![], tasks);
    assert_eq!(
        row(&lines, "updated").as_deref(),
        Some(
            "T 45s · assigned task record updated; may be an administrative change, not proof the agent ran"
        )
    );
}

#[test]
fn i1_the_overview_updated_row_with_no_evidence_is_unknown() {
    let lines = overview_of(
        vec![agent(|a| a.status = AgentStatus::Idle)],
        vec![],
        vec![],
    );
    assert_eq!(
        row(&lines, "updated").as_deref(),
        Some("unknown — no live run or assigned task")
    );
}

#[test]
fn i1_an_agent_without_a_running_entry_has_no_live_row() {
    let tasks = vec![assigned("t0", "a0", &secs_ago(45))];
    let lines = overview_of(vec![agent(|a| a.status = AgentStatus::Idle)], vec![], tasks);
    assert_eq!(row(&lines, "live"), None, "{lines:#?}");
}

#[test]
fn i1_the_overview_live_row_shows_the_time_since_a_valid_start() {
    let runs = vec![running("r0", "a0", &secs_ago(100), &secs_ago(5))];
    let lines = overview_of(vec![agent(|_| {})], runs, vec![]);
    assert_eq!(row(&lines, "live").as_deref(), Some("1:40 elapsed"));
}

#[test]
fn i1_the_overview_live_row_uses_the_newest_valid_start_among_several_runs() {
    // The older run is listed second in the map order on purpose: ids sort
    // `r0 < r1` but the newest start belongs to `r0`.
    let runs = vec![
        running("r0", "a0", &secs_ago(30), &secs_ago(5)),
        running("r1", "a0", &secs_ago(900), &secs_ago(5)),
    ];
    let lines = overview_of(vec![agent(|_| {})], runs, vec![]);
    assert_eq!(row(&lines, "live").as_deref(), Some("30s elapsed"));
}

#[test]
fn i1_a_malformed_start_is_skipped_and_the_newest_valid_one_is_used() {
    let runs = vec![
        running("r0", "a0", "not-a-date", &secs_ago(5)),
        running("r1", "a0", &secs_ago(100), &secs_ago(5)),
    ];
    let lines = overview_of(vec![agent(|_| {})], runs, vec![]);
    assert_eq!(row(&lines, "live").as_deref(), Some("1:40 elapsed"));
}

#[test]
fn i1_when_every_start_is_malformed_the_live_row_says_unknown() {
    let runs = vec![
        running("r0", "a0", "not-a-date", &secs_ago(5)),
        running("r1", "a0", "", &secs_ago(5)),
    ];
    let lines = overview_of(vec![agent(|_| {})], runs, vec![]);
    assert_eq!(row(&lines, "live").as_deref(), Some("unknown"));
}

#[test]
fn i1_a_start_a_little_in_the_future_clamps_the_live_row_to_zero() {
    let runs = vec![running("r0", "a0", &stamp(BASE + 60_000), &secs_ago(5))];
    let lines = overview_of(vec![agent(|_| {})], runs, vec![]);
    assert_eq!(row(&lines, "live").as_deref(), Some("0s elapsed"));
}

// ═════════════════════════════════════════════════════════════════════════════
// F1 - timestamp validity at the observation boundary (Ruling 1)
// ═════════════════════════════════════════════════════════════════════════════

/// Three agents in source order: `Agx-00` genuine run 60 s ago, `Agx-01`
/// the one under test, `Agx-02` genuine run 300 s ago.
fn trio(under_test: (Vec<(String, RunningEntry)>, Vec<Task>), now: i64) -> Harness {
    let agents = vec![
        make_agent("a0", "Agx-00", AgentStatus::Idle),
        make_agent("a1", "Agx-01", AgentStatus::Idle),
        make_agent("a2", "Agx-02", AgentStatus::Idle),
    ];
    let (mut runs, tasks) = under_test;
    runs.push(running(
        "g0",
        "a0",
        &stamp(now - 120_000),
        &stamp(now - 60_000),
    ));
    runs.push(running(
        "g2",
        "a2",
        &stamp(now - 400_000),
        &stamp(now - 300_000),
    ));
    agents_view(backend(agents, runs, tasks), now, WIDE, HIGH)
}

/// `Agx-01` is blank and last in both directions; the genuine two keep their
/// order (oldest first ascending).
#[track_caller]
fn assert_blank_and_last(h: &mut Harness) {
    assert_eq!(updated_cell(h, "Agx-01"), "—");
    assert_eq!(updated_order(h, false), ["Agx-02", "Agx-00", "Agx-01"]);
    assert_eq!(updated_order(h, true), ["Agx-00", "Agx-02", "Agx-01"]);
}

/// Every literal below is the security report's measured case unless noted;
/// each module is one rejected form, tested at the same six places.
macro_rules! rejected_timestamp {
    ($($case:ident => $bad:expr,)+) => {$(
        mod $case {
            use super::*;

            fn bad() -> String {
                $bad
            }

            #[test]
            fn a_run_with_this_in_both_fields_has_a_blank_updated_cell_that_sorts_last_both_ways() {
                let now = f1_now();
                let runs = vec![running("r1", "a1", &bad(), &bad())];
                assert_blank_and_last(&mut trio((runs, vec![]), now));
            }

            #[test]
            fn an_assigned_task_with_this_update_time_alone_is_blank_and_sorts_last_both_ways() {
                let now = f1_now();
                let tasks = vec![assigned("t1", "a1", &bad())];
                assert_blank_and_last(&mut trio((vec![], tasks), now));
            }

            #[test]
            fn a_run_with_this_in_both_fields_gives_unknown_overview_updated_and_live() {
                let now = f1_now();
                let agents = vec![make_agent("a0", "Agx-00", AgentStatus::Running)];
                let runs = vec![running("r0", "a0", &bad(), &bad())];
                let h = agents_view(backend(agents, runs, vec![]), now, 140, 50);
                let lines = overview(h);
                let updated = row(&lines, "updated").expect("an updated row");
                assert!(updated.starts_with("unknown"), "updated: {updated:?}");
                assert_eq!(row(&lines, "live").as_deref(), Some("unknown"));
            }

            #[test]
            fn a_run_with_this_start_and_a_valid_event_reads_r_at_the_event_time() {
                let now = f1_now();
                let runs = vec![running("r1", "a1", &bad(), &stamp(now - 600_000))];
                let mut h = trio((runs, vec![]), now);
                assert_eq!(updated_cell(&mut h, "Agx-01"), "R 10m");
            }

            #[test]
            fn a_run_with_this_in_both_fields_falls_back_to_the_valid_assigned_task_time() {
                let now = f1_now();
                let runs = vec![running("r1", "a1", &bad(), &bad())];
                let tasks = vec![assigned("t1", "a1", &stamp(now - 200_000))];
                let mut h = trio((runs, tasks), now);
                assert_eq!(updated_cell(&mut h, "Agx-01"), "T 3m");
            }

            #[test]
            fn this_start_beside_a_valid_start_leaves_the_live_row_at_the_valid_one() {
                let now = f1_now();
                let agents = vec![make_agent("a0", "Agx-00", AgentStatus::Running)];
                let runs = vec![
                    running("r0", "a0", &bad(), &stamp(now - 5000)),
                    running("r1", "a0", &stamp(now - 100_000), &stamp(now - 5000)),
                ];
                let h = agents_view(backend(agents, runs, vec![]), now, 140, 50);
                assert_eq!(row(&overview(h), "live").as_deref(), Some("1:40 elapsed"));
            }
        }
    )+};
}

rejected_timestamp! {
    // The security report's measured literals (gate-security.md F1).
    year_9999 => "9999-12-31T23:59:59.999Z".to_string(),
    one_second_before_the_epoch => "1969-12-31T23:59:59Z".to_string(),
    impossible_date_and_time_of_day => "2026-02-31T99:99:99Z".to_string(),
    wrong_separators_and_trailing_garbage => "2026-10-19T00x00x00garbage".to_string(),
    valid_form_with_a_2000_byte_suffix => format!("2026-10-19T00:00:00Z{}", "x".repeat(2000)),
    // Within the domain, so only the grammar/calendar checks can reject them
    // (a domain-only validator would let these through as "recent").
    hour_24 => "2026-10-19T24:00:00Z".to_string(),
    feb_29_in_a_non_leap_year => "2026-02-29T00:00:00Z".to_string(),
    day_31_in_a_30_day_month => "2026-04-31T00:00:00Z".to_string(),
    second_60 => "2026-10-19T00:00:60Z".to_string(),
}

#[test]
fn f1_an_offset_or_zoneless_timestamp_is_rejected_by_the_ruled_grammar() {
    // Ruling 1 grammar: `Z` only, no offsets. (If a producer in `crates/`
    // turns out to write an offset form, the fix report says so and this case
    // is amended with it.)
    let now = f1_now();
    for bad in ["2026-10-19T11:00:00+00:00", "2026-10-19T11:00:00"] {
        let runs = vec![running("r1", "a1", bad, bad)];
        let mut h = trio((runs, vec![]), now);
        assert_eq!(updated_cell(&mut h, "Agx-01"), "—", "{bad}");
    }
}

#[test]
fn f1_a_year_9999_event_does_not_outrank_genuine_evidence_in_the_descending_sort() {
    // The attack in the report: a fabricated far-future observation ranked
    // first on UPDATED↓ and showed `R now`.
    let now = f1_now();
    let runs = vec![running(
        "r1",
        "a1",
        "9999-12-31T23:59:59.999Z",
        "9999-12-31T23:59:59.999Z",
    )];
    let mut h = trio((runs, vec![]), now);
    assert_ne!(updated_cell(&mut h, "Agx-01"), "R now");
    assert_eq!(
        updated_order(&mut h, true).first().map(String::as_str),
        Some("Agx-00")
    );
}

#[test]
fn f1_epoch_zero_stays_a_valid_very_old_time_not_a_blank() {
    // Binding: epoch zero is known. It is the oldest observation, so it sorts
    // first ascending and is not blank.
    let now = f1_now();
    let epoch = "1970-01-01T00:00:00.000Z";
    let runs = vec![running("r1", "a1", epoch, epoch)];
    let mut h = trio((runs, vec![]), now);
    let cell = updated_cell(&mut h, "Agx-01");
    assert!(cell.starts_with("R ") && cell != "—", "{cell:?}");
    assert_eq!(updated_order(&mut h, false), ["Agx-01", "Agx-02", "Agx-00"]);
    assert_eq!(updated_order(&mut h, true), ["Agx-00", "Agx-02", "Agx-01"]);
}

#[test]
fn f1_a_leap_day_a_whole_second_and_a_nine_digit_fraction_are_valid() {
    let now = f1_now();
    let accepted = [
        "2024-02-29T00:00:00Z",
        "2026-10-19T11:58:00Z",
        "2026-10-19T11:58:00.123456789Z",
        "2026-10-19T11:58:00.5Z",
    ];
    for good in accepted {
        let runs = vec![running("r1", "a1", good, good)];
        let mut h = trio((runs, vec![]), now);
        let cell = updated_cell(&mut h, "Agx-01");
        assert!(cell.starts_with("R ") && cell != "—", "{good}: {cell:?}");
    }
}

#[test]
fn f1_a_start_one_hour_ahead_is_within_the_skew_window_and_reads_now() {
    let now = f1_now();
    let runs = vec![running("r1", "a1", &stamp(now + H), &stamp(now + H))];
    let mut h = trio((runs, vec![]), now);
    assert_eq!(updated_cell(&mut h, "Agx-01"), "R now");
}

#[test]
fn f1_a_start_one_hour_ahead_sorts_newest_descending() {
    let now = f1_now();
    let runs = vec![running("r1", "a1", &stamp(now + H), &stamp(now + H))];
    let mut h = trio((runs, vec![]), now);
    assert_eq!(updated_order(&mut h, true), ["Agx-01", "Agx-00", "Agx-02"]);
}

#[test]
fn f1_a_start_exactly_24_hours_ahead_is_still_valid() {
    let now = f1_now();
    let at = stamp(now + 24 * H);
    let runs = vec![running("r1", "a1", &at, &at)];
    let mut h = trio((runs, vec![]), now);
    assert_eq!(updated_cell(&mut h, "Agx-01"), "R now");
}

#[test]
fn f1_a_start_one_second_past_24_hours_ahead_is_rejected() {
    let now = f1_now();
    let at = stamp(now + 24 * H + 1000);
    let runs = vec![running("r1", "a1", &at, &at)];
    assert_blank_and_last(&mut trio((runs, vec![]), now));
}

#[test]
fn f1_a_start_25_hours_ahead_is_rejected() {
    let now = f1_now();
    let at = stamp(now + 25 * H);
    let runs = vec![running("r1", "a1", &at, &at)];
    assert_blank_and_last(&mut trio((runs, vec![]), now));
}

#[test]
fn f1_a_task_update_25_hours_ahead_is_rejected_and_a_task_one_hour_ahead_reads_t_now() {
    let now = f1_now();
    let tasks = vec![assigned("t1", "a1", &stamp(now + 25 * H))];
    assert_blank_and_last(&mut trio((vec![], tasks), now));
    let tasks = vec![assigned("t1", "a1", &stamp(now + H))];
    let mut h = trio((vec![], tasks), now);
    assert_eq!(updated_cell(&mut h, "Agx-01"), "T now");
}

#[test]
fn f1_a_live_start_one_hour_ahead_clamps_to_zero_but_25_hours_ahead_is_unknown() {
    let now = f1_now();
    let agents = vec![make_agent("a0", "Agx-00", AgentStatus::Running)];
    let near = vec![running("r0", "a0", &stamp(now + H), &stamp(now))];
    let h = agents_view(backend(agents.clone(), near, vec![]), now, 140, 50);
    assert_eq!(row(&overview(h), "live").as_deref(), Some("0s elapsed"));
    let far = vec![running("r0", "a0", &stamp(now + 25 * H), &stamp(now))];
    let h = agents_view(backend(agents, far, vec![]), now, 140, 50);
    assert_eq!(row(&overview(h), "live").as_deref(), Some("unknown"));
}

#[test]
fn f1_a_rejected_start_never_pulls_the_live_row_to_zero_beside_a_valid_one() {
    // The unvalidated build shows `0s elapsed` here: the year-9999 start is
    // the "newest" and is clamped. The valid start is 100 s old.
    let now = f1_now();
    let agents = vec![make_agent("a0", "Agx-00", AgentStatus::Running)];
    let runs = vec![
        running("r0", "a0", "9999-12-31T23:59:59.999Z", &stamp(now)),
        running("r1", "a0", &stamp(now - 100_000), &stamp(now)),
    ];
    let h = agents_view(backend(agents, runs, vec![]), now, 140, 50);
    assert_eq!(row(&overview(h), "live").as_deref(), Some("1:40 elapsed"));
}

// ═════════════════════════════════════════════════════════════════════════════
// Ruling 2 - UPDATED is strictly the lowest-priority column
// ═════════════════════════════════════════════════════════════════════════════

/// One running agent whose every cell has a distinctive word, UPDATED `T 5m`.
fn sweep_backend() -> FakeBackend {
    let mut a = make_agent("a1", "Walter", AgentStatus::Running);
    a.current_task = Some("t1".into());
    a.capabilities = vec!["capx".into()];
    a.remote = Some(orch_domain::RemoteRef {
        alias: "wnode".into(),
        online: true,
    });
    a.config.model = Some("opus-x".into());
    a.stats.total_runs = 4;
    a.stats.tasks_completed = 3;
    let mut t = make_task("t1", "Taskmark", TaskStatus::InProgress);
    t.assignee = Some("a1".into());
    t.updated_at = stamp(BASE - 300_000);
    FakeBackend::new().with_agents(vec![a]).with_tasks(vec![t])
}

const OTHER_COLUMNS: [&str; 6] = ["NAME", "ADAPTER", "TASK", "CAPS", "WHERE", "RUNS"];

fn sweep_header(width: u16) -> (String, String) {
    let mut h = agents_view(sweep_backend(), BASE, width, 30);
    let text = h.text();
    let header = text
        .lines()
        .find(|l| l.contains("STATUS") && l.contains("NAME"))
        .unwrap_or_else(|| panic!("{width}: no header\n{text}"))
        .to_string();
    let row = text
        .lines()
        .find(|l| l.contains("Walter"))
        .unwrap_or_else(|| panic!("{width}: no row\n{text}"))
        .to_string();
    (header, row)
}

#[test]
fn c2_updated_present_means_every_other_optional_column_is_present_at_every_width_40_to_200() {
    let mut offenders = Vec::new();
    for width in 40..=200u16 {
        let (header, _) = sweep_header(width);
        let missing: Vec<&str> = OTHER_COLUMNS
            .into_iter()
            .filter(|label| !header.contains(label))
            .collect();
        if header.contains("UPDATED") && !missing.is_empty() {
            offenders.push(format!("{width}: UPDATED without {missing:?}"));
        }
    }
    assert!(offenders.is_empty(), "{offenders:#?}");
}

#[test]
fn c2_the_updated_header_and_its_cell_appear_together_at_every_width_40_to_200() {
    for width in 40..=200u16 {
        let (header, row) = sweep_header(width);
        assert_eq!(
            header.contains("UPDATED"),
            row.contains("T 5m"),
            "{width}: header {header:?} / row {row:?}"
        );
    }
}

#[test]
fn c2_from_93_columns_every_column_and_the_updated_header_show() {
    for width in 93..=200u16 {
        let (header, _) = sweep_header(width);
        for label in OTHER_COLUMNS.into_iter().chain(["UPDATED"]) {
            assert!(header.contains(label), "{width}: {label} in {header:?}");
        }
    }
}

#[test]
fn c2_updated_is_absent_at_the_widths_that_showed_it_without_every_other_column() {
    // The frozen build drew UPDATED at exactly these widths while a lower
    // priority column (MODEL, CAPS, WHERE) was still dropped.
    let mut offenders = Vec::new();
    for width in [58u16, 59, 71, 72, 82, 83] {
        let (header, row) = sweep_header(width);
        if header.contains("UPDATED") || row.contains("T 5m") {
            offenders.push(format!("{width}: {header:?}"));
        }
    }
    assert!(offenders.is_empty(), "{offenders:#?}");
}

#[test]
fn c2_the_widths_either_side_of_the_fix_keep_the_columns_they_had() {
    // Columns other than UPDATED are untouched by the ruling: the allocator
    // grants runs, task, where, model, caps in that order. Spot-check the
    // same widths so the fix cannot "pass" by dropping a column instead.
    for (width, labels) in [
        (58u16, &["NAME", "TASK", "RUNS"][..]),
        (71, &["NAME", "TASK", "WHERE", "RUNS"][..]),
        (82, &["NAME", "ADAPTER", "TASK", "WHERE", "RUNS"][..]),
    ] {
        let (header, _) = sweep_header(width);
        for label in labels {
            assert!(header.contains(label), "{width}: {label} in {header:?}");
        }
    }
}

#[test]
fn c2_the_updated_sort_still_works_when_its_column_is_hidden() {
    // Hiding the column must not hide the sort: the order still follows the
    // timestamps (the footer carries the sort name).
    let now = BASE;
    let agents = vec![
        make_agent("a0", "Agx-00", AgentStatus::Idle),
        make_agent("a1", "Agx-01", AgentStatus::Idle),
    ];
    let tasks = vec![
        assigned("t0", "a0", &stamp(now - 10_000)),
        assigned("t1", "a1", &stamp(now - 500_000)),
    ];
    let mut h = agents_view(backend(agents, vec![], tasks), now, 72, 30);
    assert_eq!(updated_order(&mut h, false), ["Agx-01", "Agx-00"]);
    assert_eq!(updated_order(&mut h, true), ["Agx-00", "Agx-01"]);
}
