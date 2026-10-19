//! Counted hit-map cases, numbered `h001..`.
//!
//! The map is recorded by the code that draws, so the thing worth testing is
//! that it agrees with what was drawn — not that it holds the values this file
//! also computes. Every case therefore reads the rendered buffer back and
//! compares against the zone, which is the only assertion that can catch an
//! off-by-one against the real renderer (design §5.2).

mod common;

use common::Harness;
use orch_domain::{AgentStatus, GoalStatus, TaskStatus};
use orch_tui::backend::{FakeBackend, make_agent, make_goal, make_task};
use orch_tui::hit::{ChromeId, FooterAction, PseudoRow, ScrollRegion, VerbId, ZoneId, ZoneKind};
use orch_tui::messages::{Message, MsgType};
use orch_tui::state::{EntityType, ViewId};
use orch_tui::theme::Token;

fn tasks(n: usize) -> Vec<orch_domain::Task> {
    (0..n)
        .map(|i| {
            make_task(
                &format!("tsk_{i}"),
                &format!("Task number {i}"),
                TaskStatus::Todo,
            )
        })
        .collect()
}

fn agents(n: usize) -> Vec<orch_domain::Agent> {
    (0..n)
        .map(|i| make_agent(&format!("agt_{i}"), &format!("agent{i}"), AgentStatus::Idle))
        .collect()
}

fn goals(n: usize) -> Vec<orch_domain::Goal> {
    (0..n)
        .map(|i| {
            make_goal(
                &format!("gol_{i}"),
                &format!("Goal number {i}"),
                GoalStatus::Active,
            )
        })
        .collect()
}

/// The entity name a zone's identity points at, or `None` for a zone that is
/// not an entity row.
fn entity_name(harness: &Harness, id: &ZoneId) -> Option<String> {
    let state = &harness.app.state;
    match id {
        ZoneId::Entity(EntityType::Task, task_id) => state
            .tasks
            .iter()
            .find(|t| &t.id == task_id)
            .map(|t| t.title.clone()),
        ZoneId::Entity(EntityType::Agent, agent_id) => state
            .agents
            .iter()
            .find(|a| &a.id == agent_id)
            .map(|a| a.name.clone()),
        ZoneId::Entity(EntityType::Goal, goal_id) => state
            .goals
            .iter()
            .find(|g| &g.id == goal_id)
            .map(|g| g.title.clone()),
        _ => None,
    }
}

/// **The sync invariant.** For every row zone in the frame, the text actually
/// painted on that row must name the entity the zone's identity points at.
///
/// This is the case that enforces "the hit map agrees with what was drawn". It
/// catches every off-by-one against the real renderer — the section-header
/// offset (`state.rs:239-241`) and the scroll skip (`views/tasks.rs:69`)
/// included — because it never recomputes a row position, it reads one.
#[track_caller]
fn assert_rows_agree_with_the_frame(harness: &mut Harness, expected_rows: usize) {
    harness.draw();
    let zones: Vec<_> = harness
        .hits()
        .zones()
        .iter()
        .filter(|z| {
            matches!(
                z.kind,
                ZoneKind::TaskRow { .. } | ZoneKind::AgentRow { .. } | ZoneKind::GoalRow { .. }
            )
        })
        .map(|z| (z.rect, z.id.clone()))
        .collect();

    assert_eq!(
        zones.len(),
        expected_rows,
        "expected {expected_rows} entity row zones, got {}:\n{}",
        zones.len(),
        harness.text()
    );

    for (rect, id) in zones {
        let name = entity_name(harness, &id).expect("a row zone names an entity");
        let line = harness.line(rect.y);
        assert!(
            line.contains(&name),
            "the zone at row {} identifies {name:?}, but that row reads {line:?}",
            rect.y
        );
    }
}

// ── The sync invariant, per view and across the matrix ─────────────────────

#[test]
fn h001_task_rows_agree_with_the_frame_across_scroll_and_grouping() {
    for (len, scroll, grouped) in [
        (3usize, 0usize, false),
        (3, 0, true),
        (40, 0, false),
        (40, 5, false),
        (40, 5, true),
        (40, 37, false),
    ] {
        let mut harness = Harness::with(FakeBackend::new().with_tasks(tasks(len)));
        harness.app.state.view = ViewId::Tasks;
        harness.app.state.group_by_goal = grouped;
        // Without this the list is capped (`TASK_LIST_LIMIT`) and a scroll
        // past the cap has nothing to show — the collapsed list is covered by
        // the `len = 3` cases.
        harness.app.state.show_all_tasks = true;
        harness.app.state.task_scroll = scroll;
        harness.draw();

        // However many rows the renderer found room for.
        let drawn = harness
            .hits()
            .zones()
            .iter()
            .filter(|z| matches!(z.kind, ZoneKind::TaskRow { .. }))
            .count();
        assert!(drawn > 0, "len={len} scroll={scroll} drew no task rows");
        assert_rows_agree_with_the_frame(&mut harness, drawn);
    }
}

#[test]
fn h002_agent_rows_agree_with_the_frame() {
    let mut harness = Harness::with(FakeBackend::new().with_agents(agents(12)));
    harness.app.state.view = ViewId::Agents;
    harness.app.state.agent_scroll = 3;
    harness.draw();
    let drawn = harness
        .hits()
        .zones()
        .iter()
        .filter(|z| matches!(z.kind, ZoneKind::AgentRow { .. }))
        .count();
    assert_rows_agree_with_the_frame(&mut harness, drawn);
}

#[test]
fn h003_goal_rows_agree_with_the_frame() {
    let mut harness = Harness::with(FakeBackend::new().with_goals(goals(9)));
    harness.app.state.view = ViewId::Goals;
    harness.app.state.goal_scroll = 2;
    harness.draw();
    let drawn = harness
        .hits()
        .zones()
        .iter()
        .filter(|z| matches!(z.kind, ZoneKind::GoalRow { .. }))
        .count();
    assert_rows_agree_with_the_frame(&mut harness, drawn);
}

#[test]
fn h004_a_scrolled_list_maps_the_first_row_to_the_scrolled_index() {
    let mut harness = Harness::with(FakeBackend::new().with_tasks(tasks(40)));
    harness.app.state.view = ViewId::Tasks;
    harness.app.state.task_scroll = 5;
    harness.draw();

    let first = harness
        .hits()
        .zones()
        .iter()
        .find(|z| matches!(z.kind, ZoneKind::TaskRow { .. }))
        .expect("a task row");
    assert_eq!(
        first.kind,
        ZoneKind::TaskRow { selection: 5 },
        "with task_scroll = 5 the top row must select 5, not 0"
    );
    assert!(harness.line(first.rect.y).contains("Task number 5"));
}

#[test]
fn h005_a_section_header_registers_no_zone_and_the_row_below_it_does() {
    let mut tasks = tasks(2);
    tasks[0].goal_id = Some("gol_0".into());
    tasks[1].goal_id = Some("gol_0".into());
    let backend = FakeBackend::new().with_tasks(tasks).with_goals(goals(1));
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Tasks;
    harness.app.state.group_by_goal = true;
    harness.draw();

    let header_y = (0..harness.app.state.height)
        .find(|y| harness.line(*y).contains("GOAL NUMBER 0"))
        .expect("the grouped section header renders");
    assert!(
        matches!(
            harness.zone_at(4, header_y).map(|z| &z.kind),
            Some(ZoneKind::Pane(orch_tui::state::Focus::MainList))
        ),
        "a section header consumes no selection index, so it must not be clickable; \
         found {:?}",
        harness.zone_at(4, header_y).map(|z| z.kind.clone())
    );
    assert!(
        matches!(
            harness.zone_at(4, header_y + 1).map(|z| &z.kind),
            Some(ZoneKind::TaskRow { .. })
        ),
        "the row below the header is the task"
    );
}

#[test]
fn h006_log_rows_identify_the_message_drawn_on_them_under_a_filter() {
    let mut harness = Harness::with(FakeBackend::new());
    harness.app.state.view = ViewId::Logs;
    harness.app.state.messages = (0..40)
        .map(|i| {
            let (token, msg_type) = if i % 2 == 0 {
                (Token::White, MsgType::Output)
            } else {
                (Token::Red, MsgType::Error)
            };
            Message::new(i as u64 + 1, 0, &format!("msg-{i:02}"), token, msg_type)
        })
        .collect();
    harness.app.state.log_type_filter = [MsgType::Error].into_iter().collect();
    harness.draw();

    let zones: Vec<_> = harness
        .hits()
        .zones()
        .iter()
        .filter_map(|z| match (&z.kind, &z.id) {
            (ZoneKind::LogRow { filtered_index }, ZoneId::LogMessage(id)) => {
                Some((z.rect.y, *filtered_index, *id))
            }
            _ => None,
        })
        .collect();
    assert!(!zones.is_empty(), "no log rows:\n{}", harness.text());

    let filtered = harness.app.state.filtered_messages();
    for (y, filtered_index, message_id) in zones {
        // The filtered index the action will use must resolve to the message
        // the identity names, and that message's text must be on that row.
        let message_index = filtered[filtered_index];
        let message = &harness.app.state.messages[message_index];
        assert_eq!(
            message.id, message_id,
            "row {y}: index and identity disagree"
        );
        assert!(
            harness.line(y).contains(&message.text),
            "row {y} should read {:?} but reads {:?}",
            message.text,
            harness.line(y)
        );
    }
}

// ── Pseudo-rows ────────────────────────────────────────────────────────────

#[test]
fn h007_the_add_pseudo_rows_are_registered_in_each_view() {
    for (view, needle, pseudo) in [
        (ViewId::Tasks, "+ add task...", PseudoRow::AddTask),
        (ViewId::Agents, "+ add agent...", PseudoRow::AddAgent),
        (ViewId::Goals, "+ add goal...", PseudoRow::AddGoal),
    ] {
        let backend = FakeBackend::new()
            .with_tasks(tasks(2))
            .with_agents(agents(2))
            .with_goals(goals(2));
        let mut harness = Harness::with(backend);
        harness.app.state.view = view;
        harness.draw();

        let y = (0..harness.app.state.height)
            .find(|y| harness.line(*y).contains(needle))
            .unwrap_or_else(|| panic!("{needle} not drawn in {view:?}:\n{}", harness.text()));
        assert_eq!(
            harness.zone_at(6, y).map(|z| z.kind.clone()),
            Some(ZoneKind::Pseudo(pseudo)),
            "{needle} must be clickable"
        );
    }
}

// ── Tab pills ──────────────────────────────────────────────────────────────

#[test]
fn h008_tab_pill_zones_land_on_their_labels_whatever_the_brand_width() {
    // A pill's start column depends on the project name's length, which is
    // why the zone is recorded by the code that draws it rather than
    // recomputed (design §3.2).
    for name in ["p", "a-project-with-a-rather-long-name"] {
        let mut harness = Harness::new();
        harness.app.state.project_name = name.to_string();
        harness.draw();

        for view in ViewId::ALL {
            let zone = harness
                .hits()
                .zones()
                .iter()
                .find(|z| z.id == ZoneId::Tab(view))
                .unwrap_or_else(|| panic!("no pill zone for {view:?} with name {name:?}"));
            let line = harness.line(zone.rect.y);
            let cells: String = line
                .chars()
                .skip(zone.rect.x as usize)
                .take(zone.rect.width as usize)
                .collect();
            let label = view.label();
            assert!(
                cells.to_lowercase().contains(&label.to_lowercase()),
                "the {view:?} pill zone covers {cells:?}, which does not contain {label:?} \
                 (project name {name:?})\n{}",
                harness.text()
            );
        }
    }
}

// ── Clipping ───────────────────────────────────────────────────────────────

#[test]
fn h009_no_zone_extends_past_the_frame_at_any_width() {
    // §3.2.1: widths are computed from `area.width` and then clipped by
    // ratatui, so a zone built from a computed x-range could cover cells that
    // were never painted.
    for width in [40u16, 60, 80, 100, 140] {
        let backend = FakeBackend::new()
            .with_tasks(tasks(6))
            .with_agents(agents(3))
            .with_goals(goals(3));
        let mut harness = Harness::sized(backend, width, 30);
        for view in ViewId::ALL {
            harness.app.state.view = view;
            harness.draw();
            for zone in harness.hits().zones() {
                assert!(
                    zone.rect.right() <= width,
                    "at width {width} in {view:?}, zone {:?} ends at column {} — \
                     past the frame, so it would be invisibly clickable",
                    zone.kind,
                    zone.rect.right()
                );
                assert!(zone.rect.width > 0, "a zero-width zone was recorded");
                assert!(
                    zone.rect.bottom() <= harness.app.state.height,
                    "zone {:?} extends below the frame",
                    zone.kind
                );
            }
        }
    }
}

#[test]
fn h010_a_frame_below_the_minimum_size_records_nothing() {
    let mut harness = Harness::sized(FakeBackend::new().with_tasks(tasks(3)), 30, 10);
    harness.draw();
    assert!(
        harness.hits().zones().is_empty(),
        "the too-small guard frame has no controls, so nothing may be clickable"
    );
    assert!(harness.zone_at(5, 5).is_none());
}

// ── Scroll regions ─────────────────────────────────────────────────────────

#[test]
fn h011_the_list_answers_the_wheel_while_rows_still_win_a_click() {
    let mut harness = Harness::with(FakeBackend::new().with_tasks(tasks(4)));
    harness.app.state.view = ViewId::Tasks;
    harness.draw();

    let row = harness
        .hits()
        .zones()
        .iter()
        .find(|z| matches!(z.kind, ZoneKind::TaskRow { .. }))
        .expect("a task row")
        .rect;
    assert!(
        matches!(
            harness.zone_at(4, row.y).map(|z| &z.kind),
            Some(ZoneKind::TaskRow { .. })
        ),
        "a click on a row takes the row"
    );
    assert_eq!(
        harness.hits().resolve_scroll(4, row.y),
        Some(ScrollRegion::MainList),
        "the same cell still answers the wheel"
    );
}

// ── Invalidation ───────────────────────────────────────────────────────────

#[test]
fn h012_a_keystroke_invalidates_the_map() {
    let mut harness = Harness::with(FakeBackend::new().with_tasks(tasks(3)));
    harness.app.state.view = ViewId::Tasks;
    harness.draw();
    assert!(harness.hits().is_valid());
    let row_y = harness
        .hits()
        .zones()
        .iter()
        .find(|z| matches!(z.kind, ZoneKind::TaskRow { .. }))
        .expect("a task row")
        .rect
        .y;
    assert!(harness.zone_at(4, row_y).is_some());

    harness.press(ratatui::crossterm::event::KeyCode::Down);
    assert!(
        !harness.hits().is_valid(),
        "a key can move what is on screen, and a click from the same input \
         burst must not resolve against the pre-key frame (§3.2.3)"
    );
    assert!(
        harness.zone_at(4, row_y).is_none(),
        "an invalid map must answer nothing"
    );
}

// ── Verbs, footer hints and overlays ───────────────────────────────────────

/// Find the zone whose rect covers the first cell of `needle` in the frame.
///
/// The column is the **display width** of the text before the match, not a
/// byte offset: these lines carry `↑↓` and `←→`, so the two differ, and a
/// byte offset would land on a neighbouring hint.
#[track_caller]
fn zone_on_text(harness: &mut Harness, needle: &str) -> Option<ZoneKind> {
    let text = harness.text();
    for (y, line) in text.lines().enumerate() {
        if let Some(byte_offset) = line.find(needle) {
            let column = unicode_width::UnicodeWidthStr::width(&line[..byte_offset]) as u16;
            return harness.zone_at(column, y as u16).map(|z| z.kind.clone());
        }
    }
    None
}

#[test]
fn h013_the_picker_footer_verbs_are_each_addressable() {
    let backend = FakeBackend::new().with_agents(agents(3));
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Logs;
    harness.app.state.show_agent_picker = true;
    harness.draw();

    for (label, verb) in [
        ("Space toggle", VerbId::PickerToggle),
        ("a all", VerbId::PickerAll),
        ("Enter confirm", VerbId::PickerConfirm),
        ("Esc cancel", VerbId::PickerCancel),
    ] {
        assert_eq!(
            zone_on_text(&mut harness, label),
            Some(ZoneKind::Verb(verb)),
            "{label} must be clickable, or a mouse-only user cannot confirm a picker"
        );
    }
}

#[test]
fn h014_the_type_picker_presets_and_rows_are_addressable() {
    let mut harness = Harness::with(FakeBackend::new());
    harness.app.state.view = ViewId::Logs;
    harness.app.state.show_type_picker = true;
    harness.draw();

    for digit in ['1', '2', '3', '4', '5'] {
        let label = format!("{digit}=");
        assert_eq!(
            zone_on_text(&mut harness, &label),
            Some(ZoneKind::Verb(VerbId::PickerPreset(digit))),
            "preset {digit} must be clickable"
        );
    }
    // A row's checkbox toggles that row; its label moves the cursor there.
    let rows: Vec<_> = harness
        .hits()
        .zones()
        .iter()
        .filter_map(|z| match (&z.kind, &z.id) {
            (ZoneKind::TypePickerRow { index, part }, ZoneId::PickerItem(_, name)) => {
                Some((*index, *part, name.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(rows.len(), 16, "eight rows, each a checkbox and a label");
}

#[test]
fn h015_the_footer_hints_are_addressable_and_the_arrows_are_not() {
    // Wide enough that every conditional hint fits; the clipped case is h021.
    let backend = FakeBackend::new().with_tasks(tasks(2));
    let mut harness = Harness::sized(backend, 140, 30);
    harness.app.state.view = ViewId::Tasks;
    harness.draw();

    for (label, action) in [
        ("/ cmd", FooterAction::CommandMode),
        ("Q quit", FooterAction::Quit),
        ("? help", FooterAction::Help),
        ("Enter detail", FooterAction::Enter),
    ] {
        assert_eq!(
            zone_on_text(&mut harness, label),
            Some(ZoneKind::FooterHint(action)),
            "{label} must be clickable"
        );
    }
    // Two opposite meanings in one 2-cell span: not a button.
    assert_eq!(
        zone_on_text(&mut harness, "Tab panes; ←→ views"),
        None,
        "a hint whose direction would have to be guessed from the sub-cell \
         must not be clickable"
    );
}

#[test]
fn h016_the_help_overlay_swallows_inside_and_dismisses_outside() {
    let backend = FakeBackend::new().with_tasks(tasks(4));
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Tasks;
    harness.app.state.show_help = true;
    harness.draw();

    let title_y = (0..harness.app.state.height)
        .find(|y| harness.line(*y).contains("KEYBOARD"))
        .expect("the overlay renders its title");
    assert_eq!(
        harness
            .zone_at(harness.app.state.width / 2, title_y)
            .map(|z| z.kind.clone()),
        Some(ZoneKind::Chrome(ChromeId::HelpOverlayInside)),
        "a click on the box itself does nothing"
    );
    assert_eq!(
        harness.zone_at(0, title_y).map(|z| z.kind.clone()),
        Some(ZoneKind::Chrome(ChromeId::HelpOverlayOutside)),
        "the margin dismisses — the overlay holds no draft, so nothing is lost"
    );
}

#[test]
fn h017_each_undo_banner_row_identifies_its_own_deletion() {
    let backend = FakeBackend::new().with_tasks(tasks(3));
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Tasks;
    for id in ["tsk_0", "tsk_1", "tsk_2"] {
        harness.app.delete_task_by_id(id);
    }
    assert_eq!(harness.app.state.pending_deletions.len(), 3);
    harness.draw();

    let rows: Vec<_> = harness
        .hits()
        .zones()
        .iter()
        .filter_map(|z| match (&z.kind, &z.id) {
            (ZoneKind::UndoRow { index }, ZoneId::UndoRow(key)) => Some((*index, *key)),
            _ => None,
        })
        .collect();
    assert_eq!(rows.len(), 3, "one zone per pending deletion");
    for (index, key) in rows {
        assert_eq!(
            harness.app.state.pending_deletions[index].key, key,
            "banner row {index} must address deletion {index}, not the last one"
        );
    }
}

#[test]
fn h018_the_detail_resize_verbs_are_addressable_and_change_with_maximize() {
    let backend = FakeBackend::new().with_tasks(tasks(2));
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Tasks;
    harness.app.state.detail = orch_tui::state::DetailPanel::Entity;
    harness.draw();
    for verb in [VerbId::DetailGrow, VerbId::DetailShrink, VerbId::DetailMax] {
        assert!(
            harness
                .hits()
                .zones()
                .iter()
                .any(|z| z.kind == ZoneKind::Verb(verb)),
            "{verb:?} must be clickable while a detail panel is open"
        );
    }

    harness.app.state.detail_maximized = true;
    harness.draw();
    assert!(
        !harness
            .hits()
            .zones()
            .iter()
            .any(|z| z.kind == ZoneKind::Verb(VerbId::DetailMax)),
        "the maximized hint reads `+/- exit max`, which offers no `M`"
    );
}

#[test]
fn h019_a_wizard_option_identity_carries_its_step_and_value() {
    let mut harness = Harness::new();
    harness.app.state.view = ViewId::Agents;
    harness.key('n');
    // The new-agent wizard opens on the name step, which is a text field;
    // naming the agent advances it to the adapter select.
    harness.type_str("alpha");
    harness.press(ratatui::crossterm::event::KeyCode::Enter);
    harness.draw();

    let options: Vec<_> = harness
        .hits()
        .zones()
        .iter()
        .filter_map(|z| match &z.id {
            ZoneId::WizardOption { step_id, value, .. } => Some((step_id.clone(), value.clone())),
            _ => None,
        })
        .collect();
    assert!(
        !options.is_empty(),
        "the wizard should be on a select step:\n{}",
        harness.text()
    );
    let step = harness
        .app
        .state
        .wizard
        .as_ref()
        .unwrap()
        .step_id()
        .to_string();
    for (step_id, value) in &options {
        assert_eq!(step_id, &step, "every option names the step it belongs to");
        assert!(!value.is_empty(), "every option carries its own value");
    }
    // Distinct options must not share an identity, or a double click on one
    // would match a press on another.
    let mut seen = options.clone();
    seen.sort();
    seen.dedup();
    assert_eq!(
        seen.len(),
        options.len(),
        "option identities must be unique"
    );
}

#[test]
fn h020_the_logs_status_bar_verbs_and_agent_chips_are_addressable() {
    let backend = FakeBackend::new().with_agents(agents(3));
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Logs;
    harness.draw();

    for (label, verb) in [
        ("a filter", VerbId::OpenAgentPicker),
        ("f type", VerbId::OpenTypePicker),
        ("F cycle", VerbId::CycleLogTypes),
    ] {
        assert_eq!(
            zone_on_text(&mut harness, label),
            Some(ZoneKind::Verb(verb)),
            "{label} must be clickable"
        );
    }
    assert_eq!(
        zone_on_text(&mut harness, "LIVE"),
        Some(ZoneKind::Verb(VerbId::LogsTail)),
        "the LIVE chip returns to tail, which is what Esc does here"
    );
    let chips = harness
        .hits()
        .zones()
        .iter()
        .filter(|z| matches!(z.kind, ZoneKind::LogAgentChip { .. }))
        .count();
    assert_eq!(chips, 3, "one chip zone per agent");
}

#[test]
fn h021_a_footer_hint_the_frame_clips_is_only_clickable_where_it_was_painted() {
    let mut h = Harness::sized(FakeBackend::new().with_tasks(tasks(2)), 100, 30);
    h.draw();
    let footer = h.app.state.height - 1;
    for z in h.hits().zones().iter().filter(|z| z.rect.y == footer) {
        assert!(z.rect.right() <= 100);
        assert!(z.rect.width > 0);
        assert!(
            h.line(footer)
                .chars()
                .skip(z.rect.x as usize)
                .take(z.rect.width as usize)
                .any(|c| c != ' ')
        );
    }
    assert!(h.zone_at(100, footer).is_none());
}

// ── Column headers (R10, R16) ──────────────────────────────────────────────

fn task_ids(h: &Harness) -> Vec<String> {
    h.app
        .state
        .sorted_tasks()
        .into_iter()
        .map(|i| h.app.state.tasks[i].id.clone())
        .collect()
}

fn header_harness() -> Harness {
    let mut h = Harness::with(FakeBackend::new().with_tasks(vec![
        make_task("tsk_b", "Bravo", TaskStatus::Todo),
        make_task("tsk_c", "charlie", TaskStatus::InProgress),
        make_task("tsk_a", "Alpha", TaskStatus::Done),
    ]));
    h.app.state.view = ViewId::Tasks;
    h
}

#[test]
fn header_click_sorts() {
    let mut h = header_harness();
    assert_eq!(task_ids(&h), ["tsk_c", "tsk_b", "tsk_a"], "status order");

    // The label is a header zone for its column, drawn where it reads.
    let (x, y) = h.find_text("TITLE").expect("the TITLE header");
    assert_eq!(
        h.zone_at(x, y).map(|z| z.kind.clone()),
        Some(ZoneKind::SortHeader(orch_tui::state::SortColumn::Task(
            orch_tui::state::TaskSortField::Title
        )))
    );

    // An inactive header sorts ascending and marks itself; the active one
    // flips.
    h.click_text("TITLE");
    assert_eq!(task_ids(&h), ["tsk_a", "tsk_b", "tsk_c"]);
    h.assert_text("TITLE↑");
    h.click_text("TITLE");
    assert_eq!(task_ids(&h), ["tsk_c", "tsk_b", "tsk_a"]);
    h.assert_text("TITLE↓");

    // The header is chrome: clicking it never moves the cursor onto a row.
    assert_eq!(h.app.state.task_selected, 0);
    assert_eq!(h.app.state.selected_task().unwrap().id, "tsk_c");

    // Goals and Agents headers drive their own lists only.
    let mut g = Harness::with(FakeBackend::new().with_goals(vec![
        make_goal("g_z", "Zulu goal", GoalStatus::Active),
        make_goal("g_a", "Alpha goal", GoalStatus::Achieved),
    ]));
    g.app.state.view = ViewId::Goals;
    g.click_text("TITLE");
    let goals: Vec<_> = g
        .app
        .state
        .sorted_goals()
        .into_iter()
        .map(|i| g.app.state.goals[i].id.clone())
        .collect();
    assert_eq!(goals, ["g_a", "g_z"]);
    assert_eq!(g.app.state.task_sort, Default::default());

    let mut a = Harness::with(FakeBackend::new().with_agents(vec![
        make_agent("z", "Zed", AgentStatus::Idle),
        make_agent("b", "bob", AgentStatus::Running),
    ]));
    a.app.state.view = ViewId::Agents;
    a.click_text("NAME");
    let agents: Vec<_> = a
        .app
        .state
        .sorted_agents()
        .into_iter()
        .map(|i| a.app.state.agents[i].id.clone())
        .collect();
    assert_eq!(agents, ["b", "z"]);
    a.assert_text("NAME↑");
    a.assert_text("o sort:Name");
}

#[test]
fn inactive_header_starts_ascending() {
    let mut h = header_harness();
    // Status descending, then a click on another column: ascending, not
    // the inherited direction.
    h.key('O');
    assert!(h.app.state.task_sort.descending);
    h.click_text("TITLE");
    assert_eq!(task_ids(&h), ["tsk_a", "tsk_b", "tsk_c"]);
    assert!(!h.app.state.task_sort.descending);
    // And clicking the now-active column again flips it.
    h.click_text("TITLE");
    assert!(h.app.state.task_sort.descending);
}

#[test]
fn a_header_press_released_on_another_header_does_nothing() {
    let mut h = header_harness();
    h.draw();
    let (title_x, y) = h.find_text("TITLE").unwrap();
    let (pri_x, _) = h.find_text("PRI").unwrap();
    h.press_at(title_x, y).release_at(pri_x, y);
    assert_eq!(h.app.state.task_sort, Default::default());

    // A resize between press and release moves the columns; the release
    // lands on a different header and cancels the click.
    let (assignee_x, _) = h.find_text("ASSIGNEE").unwrap();
    h.press_at(assignee_x, y);
    h.app.state.width = 60;
    h.terminal.backend_mut().resize(60, 30);
    h.draw();
    h.release_at(assignee_x, y);
    assert_eq!(h.app.state.task_sort, Default::default());
}
