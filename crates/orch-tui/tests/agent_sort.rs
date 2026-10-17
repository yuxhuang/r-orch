mod common;

use common::Harness;
use orch_domain::{Agent, AgentStatus, RemoteRef};
use orch_tui::backend::{FakeBackend, make_agent, make_team};
use orch_tui::state::{DetailPanel, ViewId};
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
fn original_preserves_status_then_source_order_without_timestamps() {
    assert_eq!(ids(&harness()), ["b", "a2", "a1", "z"]);
}

#[test]
fn every_field_sorts_in_both_directions_with_name_and_id_ties() {
    let cases = [
        (0, vec!["b", "a2", "a1", "z"], vec!["z", "a1", "a2", "b"]),
        (1, vec!["a1", "a2", "b", "z"], vec!["z", "b", "a1", "a2"]),
        (2, vec!["a1", "a2", "b", "z"], vec!["z", "a1", "a2", "b"]),
        (3, vec!["a1", "a2", "b", "z"], vec!["z", "b", "a1", "a2"]),
        (4, vec!["b", "a1", "a2", "z"], vec!["z", "a1", "a2", "b"]),
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
fn reset_restores_original_and_keeps_the_selected_agent() {
    let mut h = harness();
    h.app.state.agent_selected = 1; // a2 in original order.
    h.key('o');
    assert_eq!(ids(&h), ["a1", "a2", "b", "z"]);
    assert_eq!(h.app.state.selected_agent().unwrap().id, "a2");
    h.key('O');
    assert_eq!(ids(&h), ["z", "b", "a1", "a2"]);
    assert_eq!(h.app.state.selected_agent().unwrap().id, "a2");
    h.key('0');
    assert_eq!(ids(&h), ["b", "a2", "a1", "z"]);
    assert_eq!(h.app.state.selected_agent().unwrap().id, "a2");
}

#[test]
fn cycle_wraps_to_original_without_losing_direction() {
    let mut h = harness();
    h.key('O');
    for _ in 0..5 {
        h.key('o');
    }
    assert_eq!(ids(&h), ["z", "a1", "a2", "b"]);
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
    assert_eq!(ids(&h), ["a1", "a2", "b", "z"]);
}

#[test]
fn sort_chips_are_clickable_and_help_describes_the_keys() {
    let mut h = harness();
    h.draw().click_text("o sort");
    assert_eq!(ids(&h), ["a1", "a2", "b", "z"]);
    h.draw().click_text("O asc");
    assert_eq!(ids(&h), ["z", "b", "a1", "a2"]);
    h.draw().click_text("0 reset");
    assert_eq!(ids(&h), ["b", "a2", "a1", "z"]);
    h.key('?');
    let text = h.text();
    assert!(text.contains("o sort; O reverse; 0 original"), "{text}");
}

#[test]
fn adapter_sort_handles_missing_empty_and_trimmed_models() {
    let mut h = harness();
    h.app.state.agents[0].adapter = "claude".into();
    h.app.state.agents[1].config.model = Some("  ".into());
    h.app.state.agents[2].config.model = Some(" model ".into());
    h.app.state.agents[3].config.model = Some("model".into());
    h.key('o').key('o').key('o');
    assert_eq!(ids(&h), ["b", "z", "a1", "a2"]);
    h.key('O');
    assert_eq!(ids(&h), ["a1", "a2", "b", "z"]);
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
    h.key('o').key('o');
    assert_eq!(ids(&h), ["4", "3", "2", "1", "0"]);
    h.key('O');
    assert_eq!(ids(&h), ["0", "1", "2", "3", "4"]);
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
fn every_sort_chip_is_visible_and_clickable_at_minimum_width() {
    let mut h = Harness::sized(FakeBackend::new().with_agents(agents()), 40, 14);
    h.app.state.view = ViewId::Agents;
    for _ in 0..5 {
        let text = h.text();
        assert!(text.contains("0 reset"), "{text}");
        for needle in ["o sort", "O asc", "0 reset"] {
            let (x, y) = h.find_text(needle).unwrap();
            assert!(matches!(
                h.zone_at(x, y).unwrap().kind,
                orch_tui::hit::ZoneKind::Verb(_)
            ));
        }
        h.key('o');
    }
}
