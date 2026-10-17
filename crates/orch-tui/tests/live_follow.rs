//! Live feeds follow at the end and retain their viewport when browsing.
mod common;
use common::Harness;
use orch_domain::TaskStatus;
use orch_tui::backend::{FakeBackend, make_task};
use orch_tui::messages::{Message, MsgType, PendingMessage};
use orch_tui::navigation::ScrollState;
use orch_tui::state::{DetailPanel, Focus, TaskSection, ViewId};
use orch_tui::theme::Token;
use ratatui::crossterm::event::KeyCode;

fn feed(focus: Focus, view: ViewId) -> Harness {
    let mut h = Harness::with(FakeBackend::new().with_tasks(vec![make_task(
        "task",
        "Fixture",
        TaskStatus::InProgress,
    )]));
    h.app.state.messages.clear();
    h.app.state.view = view;
    if matches!(focus, Focus::TaskDetail(_)) {
        h.app.state.detail = DetailPanel::Entity;
    }
    for i in 0..80 {
        append(&mut h, &format!("entry-{i:03}"));
    }
    h.app.state.set_focus(focus);
    h.draw();
    h
}

fn append(h: &mut Harness, text: &str) {
    h.app.state.add_message(
        PendingMessage {
            text: text.into(),
            color: Token::White,
            msg_type: MsgType::Info,
            agent_id: None,
            task_id: Some("task".into()),
            detail: None,
            ts_ms: None,
        },
        1_000_000,
        0,
    );
}

fn focuses() -> [(Focus, ViewId); 3] {
    [
        (Focus::Activity, ViewId::Tasks),
        (Focus::MainList, ViewId::Logs),
        (Focus::TaskDetail(TaskSection::Activity), ViewId::Tasks),
    ]
}

fn following(h: &Harness) -> bool {
    match h.app.state.focus {
        Focus::Activity => h.app.state.activity_follow_tail,
        Focus::MainList => h.app.state.log_selected_index < 0,
        focus => h
            .app
            .state
            .task_activity_following(&h.app.state.pane_key(focus)),
    }
}

fn body(h: &Harness) -> ratatui::layout::Rect {
    h.hits()
        .pane_metrics(&h.app.state.pane_key(h.app.state.focus))
        .unwrap()
        .body
}

fn visible(h: &mut Harness) -> Vec<String> {
    h.draw();
    let r = body(h);
    (r.y..r.bottom()).map(|y| h.line(y)).collect()
}

#[test]
fn appended_entries_advance_each_following_feed_and_pause_on_page_up() {
    for (focus, view) in focuses() {
        let mut h = feed(focus, view);
        append(&mut h, "newest-live");
        h.assert_text("newest-live");
        h.press(KeyCode::PageUp);
        assert!(!following(&h), "{focus:?}");
        let before = visible(&mut h);
        append(&mut h, "newest-paused");
        assert_eq!(visible(&mut h), before, "{focus:?}");
        h.assert_text("paused");
        h.press(KeyCode::End);
        h.assert_text("newest-paused");
        h.assert_text("following");
    }
}

#[test]
fn wheel_reaching_the_last_entry_resumes_each_feed() {
    for (focus, view) in focuses() {
        let mut h = feed(focus, view);
        h.press(KeyCode::Up);
        assert!(!following(&h));
        h.draw();
        let r = body(&h);
        h.wheel_down(r.x, r.y);
        assert!(following(&h), "{focus:?}");
        append(&mut h, "wheel-followed");
        h.assert_text("wheel-followed");
    }
}

#[test]
fn clicking_last_entry_resumes_but_clicking_earlier_entry_keeps_the_window() {
    for (focus, view) in focuses() {
        let mut h = feed(focus, view);
        let before = visible(&mut h);
        h.click_text("entry-078");
        assert!(!following(&h), "{focus:?}");
        let after = visible(&mut h);
        // Only the selection marker may change, never the first visible entry.
        assert_eq!(before[0].get(2..), after[0].get(2..), "{focus:?}");
        h.clock.advance_ms(1_000);
        h.click_text("entry-079");
        assert!(following(&h), "{focus:?}");
        append(&mut h, "click-followed");
        h.assert_text("click-followed");
    }
}

#[test]
fn focusing_a_feed_whose_viewport_is_at_bottom_resumes_following() {
    for (focus, view) in focuses() {
        let mut h = feed(focus, view);
        h.press(KeyCode::Up); // pauses, with the bottom entry still on screen
        assert!(!following(&h));
        let away = if focus == Focus::MainList {
            Focus::Activity
        } else {
            Focus::MainList
        };
        h.app.state.set_focus(away);
        h.app.state.set_focus(focus);
        assert!(following(&h), "{focus:?}");
        append(&mut h, "focus-followed");
        h.assert_text("focus-followed");
    }
}

#[test]
fn g_and_page_down_resume_each_feed_and_up_moves_off_the_last_entry() {
    for (focus, view) in focuses() {
        let mut h = feed(focus, view);
        h.press(KeyCode::Up);
        let selected = match focus {
            Focus::Activity => h.app.state.activity_last_position,
            Focus::MainList => h.app.state.log_selected_index as usize,
            _ => h
                .app
                .state
                .pane_scroll(&h.app.state.pane_key(focus))
                .selected
                .unwrap(),
        };
        assert_eq!(selected, 78, "{focus:?}");
        h.press(KeyCode::PageDown);
        assert!(following(&h));
        h.press(KeyCode::Home).key('G');
        assert!(following(&h));
        assert_eq!(h.app.state.view, view);
    }
}

#[test]
fn following_survives_resize_and_paused_feeds_retain_surviving_ids_after_rotation() {
    for (focus, view) in focuses() {
        let mut h = feed(focus, view);
        h.terminal.backend_mut().resize(80, 24);
        h.terminal
            .resize(ratatui::layout::Rect::new(0, 0, 80, 24))
            .unwrap();
        h.app.state.width = 80;
        h.app.state.height = 24;
        append(&mut h, "resized-live");
        h.assert_text("resized-live");
        for i in 0..orch_tui::messages::MAX_MESSAGES {
            append(&mut h, &format!("fill-{i}"));
        }
        h.draw().press(KeyCode::PageUp);
        let before = visible(&mut h);
        // The next flush evicts the oldest entry at the real retention cap.
        append(&mut h, "rotated-paused");
        assert_eq!(h.app.state.messages.len(), orch_tui::messages::MAX_MESSAGES);
        assert_eq!(visible(&mut h), before, "{focus:?}");
        h.press(KeyCode::End);
        h.assert_text("rotated-paused");
        // Complete truncation must still allow the next live entry.
        h.app.state.messages.clear();
        append(&mut h, "after-truncation");
        h.assert_text("after-truncation");
    }
}

#[test]
fn task_activity_keeps_paused_message_identity_when_history_is_prepended() {
    let mut h = feed(Focus::TaskDetail(TaskSection::Activity), ViewId::Tasks);
    h.press(KeyCode::PageUp);
    let before = visible(&mut h);
    h.app.state.prepend_messages(
        vec![PendingMessage {
            text: "old history".into(),
            color: Token::White,
            msg_type: MsgType::Info,
            agent_id: None,
            task_id: Some("task".into()),
            detail: None,
            ts_ms: Some(0),
        }],
        1_000_000,
    );
    assert_eq!(visible(&mut h), before);
}

#[test]
fn focusing_bottom_selection_without_a_published_frame_resumes() {
    for (focus, view) in focuses() {
        let mut h = feed(focus, view);
        h.app.state.mouse.hits.metrics.panes.clear();
        let key = h.app.state.pane_key(focus);
        let last = h.app.state.messages.len() - 1;
        match focus {
            Focus::Activity => {
                h.app.state.activity_follow_tail = false;
                h.app.state.activity_selected = Some(h.app.state.messages[last].id);
            }
            Focus::MainList => h.app.state.log_selected_index = last as i64,
            _ => h.app.state.set_pane_scroll(
                key,
                ScrollState {
                    selected: Some(last),
                    offset: 0,
                },
            ),
        }
        h.app.state.focus = Focus::TextViewer;
        h.app.state.set_focus(focus);
        assert!(following(&h));
    }
}

#[test]
fn task_activity_follows_only_its_own_messages_and_viewers_remain_snapshots() {
    let mut h = feed(Focus::TaskDetail(TaskSection::Activity), ViewId::Tasks);
    h.press(KeyCode::Enter);
    let source = h
        .app
        .state
        .viewer
        .as_ref()
        .unwrap()
        .document()
        .source
        .clone();
    let before = h.app.state.viewer.as_ref().unwrap().document().body.clone();
    append(&mut h, "later entry");
    assert_eq!(h.app.state.viewer.as_ref().unwrap().document().body, before);
    assert_eq!(
        source,
        orch_tui::widgets::text_viewer::ViewerSource::Message(80)
    );
    h.press(KeyCode::Esc);
    h.app.state.messages.push(Message::new(
        999,
        1_000_000,
        "other task",
        Token::White,
        MsgType::Info,
    ));
    assert!(!visible(&mut h).join("\n").contains("other task"));
}

#[test]
fn paging_to_the_bottom_viewport_resumes_before_the_selection_reaches_last() {
    for (focus, view) in focuses() {
        let mut h = feed(focus, view);
        h.press(KeyCode::Home).draw();
        let height = body(&h).height as usize;
        for _ in 0..(80 - height).div_ceil(height) {
            h.press(KeyCode::PageDown);
        }
        assert!(
            following(&h),
            "{focus:?}: reaching the last page must resume"
        );
        append(&mut h, "paged-followed");
        h.assert_text("paged-followed");
    }
}

#[test]
fn bottom_log_row_can_still_expand_while_following() {
    let mut h = feed(Focus::MainList, ViewId::Logs);
    h.click_text("entry-079");
    assert!(following(&h));
    h.press(KeyCode::Enter);
    assert_eq!(
        &*h.app.state.viewer.as_ref().unwrap().document().body,
        "entry-079"
    );
}
