mod common;

use common::Harness;
use orch_domain::{Agent, AgentStatus, RemoteRef};
use orch_tui::backend::{FakeBackend, make_agent, make_team};
use orch_tui::messages::{MsgType, PendingMessage};
use orch_tui::state::{DetailPanel, Focus, ViewId};
use orch_tui::theme::Token;
use ratatui::crossterm::event::KeyCode;

fn agents() -> Vec<Agent> {
    let mut agents = vec![
        make_agent("z", "Zulu", AgentStatus::Error),
        make_agent("b", "Beta", AgentStatus::Idle),
        make_agent("a2", "Alpha", AgentStatus::Idle),
        make_agent("a1", "Alpha", AgentStatus::Idle),
    ];
    agents[0].adapter = "shell".into();
    agents[1].adapter = "claude".into();
    agents[2].adapter = "claude".into();
    agents[3].adapter = "claude".into();
    agents[1].config.model = Some("z-model".into());
    agents[2].config.model = Some("a-model".into());
    agents[3].config.model = Some("a-model".into());
    agents[0].remote = Some(RemoteRef {
        alias: "remote-z".into(),
        online: true,
    });
    agents[2].remote = Some(RemoteRef {
        alias: "remote-a".into(),
        online: false,
    });
    agents[3].remote = Some(RemoteRef {
        alias: "remote-a".into(),
        online: true,
    });
    agents
}

fn harness() -> Harness {
    let mut h = Harness::with(FakeBackend::new().with_agents(agents()));
    h.app.state.view = ViewId::Agents;
    h
}

fn ids(h: &Harness) -> Vec<&str> {
    h.app
        .state
        .sorted_agents()
        .into_iter()
        .map(|i| h.app.state.agents[i].id.as_str())
        .collect()
}

#[test]
fn the_default_is_status_ascending_with_source_order_ties() {
    // Idle before Error; the three Idle agents keep their source order.
    assert_eq!(ids(&harness()), ["b", "a2", "a1", "z"]);
}

#[test]
fn every_field_sorts_in_both_directions_with_source_order_ties() {
    // `o` presses from the default: Status, Name, Adapter/Model, Task, Caps,
    // Where, Runs, Updated. No agent has a task, caps, runs or a timestamp,
    // so those four are all-blank: every agent ties, in source order, both
    // ways.
    let blank = || vec!["z", "b", "a2", "a1"];
    let cases = [
        (0, vec!["b", "a2", "a1", "z"], vec!["z", "b", "a2", "a1"]),
        (1, vec!["a2", "a1", "b", "z"], vec!["z", "b", "a2", "a1"]),
        (2, vec!["a2", "a1", "b", "z"], vec!["z", "b", "a2", "a1"]),
        (3, blank(), blank()),
        (4, blank(), blank()),
        (5, vec!["b", "a2", "a1", "z"], vec!["z", "a2", "a1", "b"]),
        (6, blank(), blank()),
        (7, blank(), blank()),
    ];
    for (cycles, asc, desc) in cases {
        let mut h = harness();
        for _ in 0..cycles {
            h.key('o');
        }
        assert_eq!(ids(&h), asc, "field {cycles} ascending");
        h.key('O');
        assert_eq!(ids(&h), desc, "field {cycles} descending");
    }
}

#[test]
fn reset_restores_the_default_and_keeps_the_selected_agent() {
    let mut h = harness();
    h.app.state.agent_selected = 1; // a2 in the default order.
    h.key('o');
    assert_eq!(ids(&h), ["a2", "a1", "b", "z"]);
    assert_eq!(h.app.state.selected_agent().unwrap().id, "a2");
    h.key('O');
    assert_eq!(ids(&h), ["z", "b", "a2", "a1"]);
    assert_eq!(h.app.state.selected_agent().unwrap().id, "a2");
    h.key('0');
    assert_eq!(ids(&h), ["b", "a2", "a1", "z"]);
    assert_eq!(h.app.state.selected_agent().unwrap().id, "a2");
}

#[test]
fn cycle_wraps_to_status_without_losing_direction() {
    use orch_tui::state::AgentSortField;
    let mut h = harness();
    h.key('O');
    for _ in 0..8 {
        h.key('o');
    }
    assert_eq!(h.app.state.agent_sort.field, AgentSortField::Status);
    assert!(h.app.state.agent_sort.descending);
    assert_eq!(ids(&h), ["z", "b", "a2", "a1"]);
}

#[test]
fn sort_keeps_team_sections_in_place_even_when_descending() {
    let mut h = harness();
    h.app.state.teams = vec![
        make_team("t2", "Zulu team", "a1", &["a1"]),
        make_team("t1", "Alpha team", "b", &["b", "z"]),
    ];
    h.key('o').key('O');
    assert_eq!(ids(&h), ["z", "b", "a1", "a2"]);
}

#[test]
fn empty_and_single_agent_lists_accept_all_sort_actions() {
    for agents in [vec![], vec![make_agent("only", "only", AgentStatus::Idle)]] {
        let count = agents.len();
        let mut h = Harness::with(FakeBackend::new().with_agents(agents));
        h.app.state.view = ViewId::Agents;
        for _ in 0..5 {
            h.key('o').key('O');
            assert_eq!(ids(&h).len(), count);
        }
        h.key('0');
        assert_eq!(h.app.state.agent_selected, 0);
    }
}

#[test]
fn sorting_preserves_the_add_agent_row_selection() {
    let mut h = harness();
    h.app.state.agent_selected = h.app.state.agents.len();
    h.key('o').key('O').key('0');
    assert_eq!(h.app.state.agent_selected, h.app.state.agents.len());
    assert!(h.app.state.selected_agent().is_none());
}

#[test]
fn sorting_works_with_detail_open_and_keeps_its_agent() {
    let mut h = harness();
    h.press(KeyCode::Enter).key('o');
    assert_eq!(h.app.state.detail, DetailPanel::Entity);
    assert_eq!(h.app.state.selected_agent().unwrap().id, "b");
    assert_eq!(ids(&h), ["a2", "a1", "b", "z"]);
}

#[test]
fn sort_headers_are_clickable_and_help_describes_the_keys() {
    let mut h = harness();
    h.draw().click_text("NAME");
    assert_eq!(ids(&h), ["a2", "a1", "b", "z"]);
    h.draw().click_text("NAME");
    assert_eq!(ids(&h), ["z", "b", "a2", "a1"]);
    h.key('0');
    assert_eq!(ids(&h), ["b", "a2", "a1", "z"]);
    assert!(h.text().contains("STATUS↑"));
    h.key('?');
    let text = h.text();
    assert!(
        text.contains("o sort; O reverse; 0 default order"),
        "{text}"
    );
}

#[test]
fn adapter_sort_handles_missing_empty_and_trimmed_models() {
    let mut h = harness();
    h.app.state.agents[0].adapter = "claude".into();
    h.app.state.agents[1].config.model = Some("  ".into());
    h.app.state.agents[2].config.model = Some(" model ".into());
    h.app.state.agents[3].config.model = Some("model".into());
    h.key('o').key('o');
    // No model and a blank model are the same empty model: z and b tie, in
    // source order; a trimmed model ties with the bare one.
    assert_eq!(ids(&h), ["z", "b", "a2", "a1"]);
    h.key('O');
    assert_eq!(ids(&h), ["a2", "a1", "z", "b"]);
}

#[test]
fn status_sort_covers_every_status_rank() {
    let agents = [
        AgentStatus::Offline,
        AgentStatus::Disabled,
        AgentStatus::Error,
        AgentStatus::Idle,
        AgentStatus::Running,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, status)| make_agent(&i.to_string(), "same", status))
    .collect();
    let mut h = Harness::with(FakeBackend::new().with_agents(agents));
    h.app.state.view = ViewId::Agents;
    // ACTIVE, IDLE, ERROR, OFFLINE, DISABLED.
    assert_eq!(ids(&h), ["4", "3", "2", "0", "1"]);
    h.key('O');
    assert_eq!(ids(&h), ["1", "0", "2", "3", "4"]);
}

#[test]
fn sort_keys_do_not_change_other_views_or_command_input() {
    let mut h = harness();
    h.app.state.view = ViewId::Tasks;
    h.key('o').key('O').key('0');
    assert_eq!(ids(&h), ["b", "a2", "a1", "z"]);
    h.app.state.view = ViewId::Agents;
    h.key('/').type_str("oO0");
    assert_eq!(ids(&h), ["b", "a2", "a1", "z"]);
}

#[test]
fn sorting_reveals_the_preserved_selection_in_the_shorter_agent_body() {
    let agents = (0..30)
        .map(|i| {
            make_agent(
                &format!("id{i:02}"),
                &format!("Agent{i:02}"),
                AgentStatus::Idle,
            )
        })
        .collect();
    let mut h = Harness::sized(FakeBackend::new().with_agents(agents), 80, 24);
    h.app.state.view = ViewId::Agents;
    h.draw().key('o').key('O').draw();
    assert_eq!(h.app.state.selected_agent().unwrap().id, "id00");
    let screen = h.text();
    assert!(screen.contains("Agent00"), "{screen}");
}

#[test]
fn every_sort_is_named_at_minimum_width() {
    // At 40 columns only STATUS, NAME and RUNS have a column: the other
    // fields are named, with their direction, by a clickable footer hint.
    let mut h = Harness::sized(FakeBackend::new().with_agents(agents()), 40, 14);
    h.app.state.view = ViewId::Agents;
    for _ in 0..8 {
        for _ in 0..2 {
            let text = h.text();
            let sort = h.app.state.agent_sort;
            let arrow = if sort.descending { '↓' } else { '↑' };
            let header = text
                .lines()
                .find(|l| l.contains("STATUS") && l.contains("NAME"))
                .unwrap();
            if !header.contains(arrow) {
                let hint = format!("o {}{arrow}", sort.field.label());
                let Some((x, y)) = h.find_text(&hint) else {
                    panic!("{hint:?} missing:\n{text}");
                };
                assert!(
                    matches!(
                        h.zone_at(x, y).unwrap().kind,
                        orch_tui::hit::ZoneKind::FooterHint(_)
                    ),
                    "{hint:?} is not clickable:\n{text}"
                );
            }
            h.key('O');
        }
        h.key('o');
    }
}

#[test]
fn sort_and_follow_keys_keep_independent_state_in_agents_view() {
    let mut h = harness();
    h.app.state.messages.clear();
    for i in 0..80 {
        h.app.state.add_message(
            PendingMessage {
                text: format!("entry-{i:03}"),
                color: Token::White,
                msg_type: MsgType::Info,
                agent_id: None,
                task_id: None,
                detail: None,
                ts_ms: None,
            },
            1_000_000,
            0,
        );
    }
    h.app.state.set_focus(Focus::Activity);
    h.draw().press(KeyCode::Up);
    assert!(!h.app.state.activity_follow_tail);
    h.key('o').key('O');
    assert_eq!(ids(&h), ["z", "b", "a2", "a1"]);
    assert!(!h.app.state.activity_follow_tail);
    let sort = h.app.state.agent_sort;
    h.press(KeyCode::End);
    assert!(h.app.state.activity_follow_tail);
    assert_eq!(h.app.state.agent_sort, sort);
    h.press(KeyCode::Up).key('0');
    assert_eq!(ids(&h), ["b", "a2", "a1", "z"]);
    assert!(!h.app.state.activity_follow_tail);
    h.press(KeyCode::End);
    assert!(h.app.state.activity_follow_tail);
    h.app.state.set_focus(Focus::MainList);
    h.key('G');
    assert_eq!(h.app.state.view, ViewId::Goals);
}

#[test]
fn name_and_location_sort_ignore_case_then_keep_source_order() {
    use orch_tui::state::{AgentSort, AgentSortField};
    for field in [AgentSortField::Name, AgentSortField::Location] {
        let agents = [
            ("b", "bob"),
            ("z", "Zed"),
            ("a2", "alice"),
            ("a1", "alice"),
            ("upper", "ALICE"),
        ]
        .into_iter()
        .map(|(id, name)| {
            let mut agent = make_agent(id, name, AgentStatus::Idle);
            agent.remote = Some(RemoteRef {
                alias: name.into(),
                online: true,
            });
            agent
        })
        .collect();
        let mut h = Harness::with(FakeBackend::new().with_agents(agents));
        h.app.state.view = ViewId::Agents;
        h.app.state.set_agent_sort(AgentSort {
            field,
            descending: false,
        });
        assert_eq!(ids(&h), ["a2", "a1", "upper", "b", "z"], "{field:?}");
        h.app.state.set_agent_sort(AgentSort {
            field,
            descending: true,
        });
        assert_eq!(ids(&h), ["z", "b", "a2", "a1", "upper"], "{field:?}");
    }
}

// ── Grouping ────────────────────────────────────────────────────────────────

/// [`agents`] with one link state per alias, as the loader writes it, plus a
/// team that By node must ignore.
fn node_harness() -> Harness {
    let mut h = harness();
    // z is on remote-z, offline; a1/a2 are on remote-a, online; b is local.
    h.app.state.agents[0].remote.as_mut().unwrap().online = false;
    h.app.state.agents[2].remote.as_mut().unwrap().online = true;
    h.app.state.teams = vec![make_team("t1", "Alpha team", "z", &["z"])];
    h
}

#[test]
fn group_by_node_sections() {
    use orch_tui::state::{AgentGroupBy, AgentRow};
    let mut h = node_harness();
    h.key('o'); // Name ascending.
    // By team: the team section leads.
    assert_eq!(ids(&h), ["z", "a2", "a1", "b"]);

    h.key('b');
    assert_eq!(h.app.state.agent_group_by, AgentGroupBy::Node);
    // Local first, then the aliases in alias order; the sort applies inside
    // each section, and the team no longer groups anything.
    assert_eq!(ids(&h), ["b", "a2", "a1", "z"]);
    let rows = h.app.state.agent_rows(true);
    let node = |alias: Option<&str>, online: bool, count: usize| AgentRow::NodeSection {
        alias: alias.map(str::to_string),
        online,
        count,
    };
    assert_eq!(rows[0], node(None, true, 1));
    assert!(matches!(rows[1], AgentRow::Agent { selection: 0, .. }));
    assert_eq!(rows[2], node(Some("remote-a"), true, 2));
    assert!(matches!(rows[3], AgentRow::Agent { selection: 1, .. }));
    assert!(matches!(rows[4], AgentRow::Agent { selection: 2, .. }));
    assert_eq!(rows[5], node(Some("remote-z"), false, 1));
    assert!(matches!(rows[6], AgentRow::Agent { selection: 3, .. }));
    assert!(matches!(rows[7], AgentRow::AddAgent { selection: 4 }));
    assert_eq!(rows.len(), 8);
    assert_eq!(h.app.state.agent_section_rows(), 3);

    // Descending never moves a section (and equal statuses keep source
    // order both ways).
    h.key('0');
    assert_eq!(ids(&h), ["b", "a2", "a1", "z"]);
    h.key('O');
    assert_eq!(ids(&h), ["b", "a2", "a1", "z"]);

    // The headers on screen, each node marked online or offline.
    let text = h.draw().text();
    for needle in [
        "◈ LOCAL · 1 agent ",
        "◈ remote-a ● online · 2 agents",
        "◈ remote-z ○ offline · 1 agent ",
    ] {
        assert!(text.contains(needle), "{needle:?} missing:\n{text}");
    }
    assert!(!text.contains("ALPHA TEAM"), "{text}");
    let local = text.find("LOCAL").unwrap();
    let a = text.find("remote-a ●").unwrap();
    let z = text.find("remote-z ○").unwrap();
    assert!(local < a && a < z, "{text}");
}

#[test]
fn group_by_node_with_only_local_agents_has_one_local_section() {
    use orch_tui::state::AgentRow;
    let agents = vec![
        make_agent("x", "x", AgentStatus::Idle),
        make_agent("y", "y", AgentStatus::Running),
    ];
    let mut h = Harness::with(FakeBackend::new().with_agents(agents));
    h.app.state.view = ViewId::Agents;
    h.key('b');
    let rows = h.app.state.agent_rows(false);
    assert_eq!(
        rows[0],
        AgentRow::NodeSection {
            alias: None,
            online: true,
            count: 2
        }
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(h.app.state.agent_section_rows(), 1);
}

#[test]
fn group_by_node_orders_remote_sections_by_alias_not_by_agent() {
    let mut agents = Vec::new();
    for (id, alias) in [("1", "w2"), ("2", "a-peer"), ("3", "w10"), ("4", "w2")] {
        let mut agent = make_agent(id, id, AgentStatus::Idle);
        agent.remote = Some(RemoteRef {
            alias: alias.into(),
            online: true,
        });
        agents.push(agent);
    }
    let mut h = Harness::with(FakeBackend::new().with_agents(agents));
    h.app.state.view = ViewId::Agents;
    h.key('b');
    assert_eq!(ids(&h), ["2", "3", "1", "4"]);
    assert_eq!(h.app.state.agent_section_rows(), 3);
}

#[test]
fn group_by_node_mixed_link_state_is_offline() {
    use orch_tui::state::AgentRow;
    // The loader stamps one link state per alias; if a snapshot ever mixes
    // them, the header must not claim the node is reachable.
    let agents = [("up", true), ("down", false)]
        .into_iter()
        .map(|(id, online)| {
            let mut agent = make_agent(id, id, AgentStatus::Idle);
            agent.remote = Some(RemoteRef {
                alias: "w1".into(),
                online,
            });
            agent
        })
        .collect();
    let mut h = Harness::with(FakeBackend::new().with_agents(agents));
    h.app.state.view = ViewId::Agents;
    h.key('b');
    let rows = h.app.state.agent_rows(false);
    assert_eq!(
        rows[0],
        AgentRow::NodeSection {
            alias: Some("w1".into()),
            online: false,
            count: 2
        }
    );
    let text = h.draw().text();
    assert!(text.contains("◈ w1 ○ offline · 2 agents"), "{text}");
}

#[test]
fn group_by_team_keeps_two_teams_of_one_name_apart() {
    use orch_tui::state::AgentRow;
    // Members of two same-named teams, interleaved in source order.
    let agents = ["a", "b", "c", "d"]
        .into_iter()
        .map(|id| make_agent(id, id, AgentStatus::Idle))
        .collect();
    let mut h = Harness::with(FakeBackend::new().with_agents(agents));
    h.app.state.view = ViewId::Agents;
    h.app.state.teams = vec![
        make_team("t1", "Core", "a", &["a", "c"]),
        make_team("t2", "Core", "b", &["b", "d"]),
    ];
    assert_eq!(ids(&h), ["a", "c", "b", "d"]);
    let headers: Vec<(String, usize)> = h
        .app
        .state
        .agent_rows(false)
        .into_iter()
        .filter_map(|row| match row {
            AgentRow::TeamSection { team_id, count, .. } => Some((team_id, count)),
            _ => None,
        })
        .collect();
    assert_eq!(headers, [("t1".to_string(), 2), ("t2".to_string(), 2)]);
}

#[test]
fn group_by_node_preserves_selection() {
    let mut h = node_harness();
    h.app.state.agent_selected = 2; // a2 in team order: z, b, a2, a1.
    assert_eq!(h.app.state.selected_agent().unwrap().id, "a2");
    h.key('o').key('O'); // Name descending.
    assert_eq!(h.app.state.selected_agent().unwrap().id, "a2");
    assert_eq!(ids(&h), ["z", "b", "a2", "a1"]);
    assert_eq!(h.app.state.agent_selected, 2);
    h.key('b');
    assert_eq!(ids(&h), ["b", "a2", "a1", "z"]);
    assert_eq!(h.app.state.agent_selected, 1);
    assert_eq!(h.app.state.selected_agent().unwrap().id, "a2");
    h.key('B');
    assert_eq!(ids(&h), ["z", "b", "a2", "a1"]);
    assert_eq!(h.app.state.agent_selected, 2);
    assert_eq!(h.app.state.selected_agent().unwrap().id, "a2");

    // The add-agent row stays selected across both directions.
    h.app.state.agent_selected = h.app.state.agents.len();
    h.key('b');
    assert!(h.app.state.selected_agent().is_none());
    assert_eq!(h.app.state.agent_selected, h.app.state.agents.len());
    h.key('B');
    assert_eq!(h.app.state.agent_selected, h.app.state.agents.len());
}

#[test]
fn group_by_node_reveals_the_preserved_selection() {
    // 30 local agents and one remote: By node moves the remote agent from
    // the top (its team leads By team) to the bottom, below the fold.
    let mut agents: Vec<Agent> = (0..30)
        .map(|i| {
            make_agent(
                &format!("id{i:02}"),
                &format!("Agent{i:02}"),
                AgentStatus::Idle,
            )
        })
        .collect();
    let mut far = make_agent("far", "Faraway", AgentStatus::Idle);
    far.remote = Some(RemoteRef {
        alias: "w1".into(),
        online: true,
    });
    agents.push(far);
    let mut h = Harness::sized(FakeBackend::new().with_agents(agents), 80, 24);
    h.app.state.view = ViewId::Agents;
    h.app.state.teams = vec![make_team("t1", "Away", "far", &["far"])];
    h.draw();
    assert_eq!(h.app.state.selected_agent().unwrap().id, "far");
    h.key('b').draw();
    assert_eq!(h.app.state.selected_agent().unwrap().id, "far");
    let screen = h.text();
    assert!(screen.contains("Faraway"), "{screen}");
    assert!(screen.contains("◈ w1 ● online"), "{screen}");
}

#[test]
fn group_by_keys_chip_and_help() {
    use orch_tui::state::AgentGroupBy;
    let mut h = Harness::sized(FakeBackend::new().with_agents(agents()), 120, 30);
    h.app.state.view = ViewId::Agents;
    assert_eq!(h.app.state.agent_group_by, AgentGroupBy::Team);
    h.key('b');
    assert_eq!(h.app.state.agent_group_by, AgentGroupBy::Node);
    h.key('B');
    assert_eq!(h.app.state.agent_group_by, AgentGroupBy::Team);
    h.key('B');
    assert_eq!(h.app.state.agent_group_by, AgentGroupBy::None);
    h.key('b');

    // `0` resets the sort, not the grouping, and `b` leaves the sort alone.
    h.key('b').key('o').key('O');
    let sort = h.app.state.agent_sort;
    h.key('b').key('b');
    assert_eq!(h.app.state.agent_sort, sort);
    h.key('0');
    assert_eq!(h.app.state.agent_group_by, AgentGroupBy::Adapter);

    // The header chip and the footer hint name the mode; the chip cycles it.
    let text = h.draw().text();
    assert!(text.contains("GROUP: Adapter"), "{text}");
    assert!(text.contains("b Adapter"), "{text}");
    h.click_text("GROUP: Adapter");
    assert_eq!(h.app.state.agent_group_by, AgentGroupBy::Caps);
    let text = h.draw().text();
    assert!(text.contains("GROUP: Caps"), "{text}");

    // Help names the keys.
    h.key('?');
    let text = h.text();
    assert!(
        text.contains("b/B group by team/node/status/adapter"),
        "{text}"
    );
    h.key('x');

    // Inert outside the Agents view and while typing a command.
    h.app.state.view = ViewId::Tasks;
    h.key('b').key('B');
    assert_eq!(h.app.state.agent_group_by, AgentGroupBy::Caps);
    h.app.state.view = ViewId::Agents;
    h.key('/').type_str("bB");
    assert_eq!(h.app.state.agent_group_by, AgentGroupBy::Caps);
}

#[test]
fn group_mode_hint_is_visible_and_clickable_at_minimum_width() {
    use orch_tui::state::AgentGroupBy;
    // At 40 columns there is no header chip; the footer leads with the mode
    // and cycles it through all six, whatever the sort.
    let mut h = Harness::sized(FakeBackend::new().with_agents(agents()), 40, 14);
    h.app.state.view = ViewId::Agents;
    let mut seen = Vec::new();
    for _ in 0..6 {
        let mode = h.app.state.agent_group_by;
        seen.push(mode);
        let text = h.text();
        assert!(!text.contains("GROUP:"), "{text}");
        let hint = format!("b {}", mode.label());
        let Some((x, y)) = h.find_text(&hint) else {
            panic!("{hint:?} missing:\n{text}");
        };
        assert!(
            matches!(
                h.zone_at(x, y).unwrap().kind,
                orch_tui::hit::ZoneKind::FooterHint(_)
            ),
            "{hint:?} is not clickable:\n{text}"
        );
        h.key('o').key('O');
        h.click_text(&hint);
    }
    assert_eq!(
        seen,
        [
            AgentGroupBy::Team,
            AgentGroupBy::Node,
            AgentGroupBy::Status,
            AgentGroupBy::Adapter,
            AgentGroupBy::Caps,
            AgentGroupBy::None,
        ]
    );
    assert_eq!(h.app.state.agent_group_by, AgentGroupBy::Team);
}
