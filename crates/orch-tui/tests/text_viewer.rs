//! The full-text viewer (design D3, D10.3–D10.6, D11), driven through the
//! single B1 hook with a `TestBackend` frame, a fake clock and the fake copy
//! facade. No host clipboard, terminal or process is touched.
//!
//! The harness draws the dashboard and then, when the dashboard itself did
//! not draw the viewer (the B1 branch, before B0's overlay call is merged),
//! draws it into B0's documented overlay rectangle `Rect(x, y+6, W, H-7)`.
//! The same tests therefore run unchanged on the composed I1 branch.
//!
//! Vectors that scroll through the shared D2 helper are skipped, loudly, when
//! that helper is still the I0 inert seed: they prove B0+B1 composition and
//! are run against B0 in a disposable composition checkout.

use std::sync::Arc;

use orch_tui::App;
use orch_tui::backend::{
    CopyOutcome, CopyResult, CopyTarget, FakeBackend, TuiBackend,
    copy::{self, GUIDANCE_STY, GUIDANCE_TMUX},
};
use orch_tui::clock::TestClock;
use orch_tui::hit::{HitMap, TextAction, ZoneKind};
use orch_tui::navigation::{NavAction, ScrollMetrics, ScrollMode, ScrollState, navigate};
use orch_tui::state::EntityType;
use orch_tui::views::{self, Caps, RenderCtx};
use orch_tui::widgets::text_viewer::{
    Completeness, LocalNotice, ROW_INDEX_MAX_BYTES, Representation, TextViewer, ViewerDocument,
    ViewerSource,
};
use orch_tui::wizard::{DraftKey, WizardKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

const START_MS: i64 = 1_000_000;

struct V {
    app: App<FakeBackend, TestClock>,
    term: Terminal<TestBackend>,
    clock: TestClock,
}

fn caps() -> Caps {
    Caps {
        can_create_task: true,
        can_add_agent: true,
        can_create_goal: true,
        can_force_stop_agent: true,
        can_send_message: true,
    }
}

impl V {
    fn sized(width: u16, height: u16) -> V {
        let clock = TestClock::new(START_MS);
        let mut app = App::new(FakeBackend::new(), clock.clone());
        app.state.width = width;
        app.state.height = height;
        V {
            app,
            term: Terminal::new(TestBackend::new(width, height)).unwrap(),
            clock,
        }
    }

    fn open(&mut self, doc: ViewerDocument) -> &mut Self {
        self.app
            .state
            .register_viewer_origin(doc.source.clone())
            .unwrap();
        self.app.state.viewer = Some(TextViewer::new(doc));
        self.draw();
        self
    }

    /// Draw one frame and adopt its hit map, as the runner does.
    fn draw(&mut self) -> String {
        let app = &self.app;
        let mut hits: Option<HitMap> = None;
        self.term
            .draw(|frame| {
                let area = frame.area();
                let ctx = RenderCtx::new(&app.state, caps(), app.now(), area);
                views::render(frame, area, &ctx);
                let drawn = ctx
                    .hits
                    .borrow()
                    .zones()
                    .iter()
                    .any(|z| matches!(z.kind, ZoneKind::ViewerText));
                if !drawn && app.state.viewer.is_some() {
                    let overlay = Rect::new(area.x, area.y + 6, area.width, area.height - 7);
                    TextViewer::render(frame, overlay, &ctx);
                }
                hits = Some(ctx.hits.into_inner());
            })
            .unwrap();
        self.app.set_hits(hits.unwrap());
        self.text()
    }

    fn text(&self) -> String {
        let buffer = self.term.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            out.push_str(&self.line(y));
            out.push('\n');
        }
        out
    }

    fn line(&self, y: u16) -> String {
        let buffer = self.term.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    }

    fn viewer(&self) -> &TextViewer {
        self.app.state.viewer.as_ref().expect("viewer open")
    }

    fn body(&self) -> Rect {
        self.app
            .state
            .mouse
            .hits
            .zones()
            .iter()
            .find(|z| z.kind == ZoneKind::ViewerText)
            .map(|z| z.rect)
            .expect("viewer body zone")
    }

    fn footer(&self) -> Rect {
        self.app
            .state
            .mouse
            .hits
            .metrics
            .viewer_footer
            .as_ref()
            .expect("published viewer footer")
            .rect
    }

    fn footer_text(&self) -> String {
        let footer = self.footer();
        let line = self.line(footer.y);
        line.chars()
            .skip(usize::from(footer.x))
            .take(usize::from(footer.width))
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    fn key(&mut self, code: KeyCode) -> &mut Self {
        self.key_mod(code, KeyModifiers::NONE)
    }

    fn key_mod(&mut self, code: KeyCode, modifiers: KeyModifiers) -> &mut Self {
        self.app.handle_key(KeyEvent::new(code, modifiers));
        self.draw();
        self
    }

    fn mouse(&mut self, kind: MouseEventKind, x: u16, y: u16) {
        self.app.handle_mouse(MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        });
    }

    fn press(&mut self, x: u16, y: u16) {
        self.mouse(MouseEventKind::Down(MouseButton::Left), x, y);
    }

    fn release(&mut self, x: u16, y: u16) {
        self.mouse(MouseEventKind::Up(MouseButton::Left), x, y);
    }

    /// Press and release, then redraw like the runner.
    fn click(&mut self, x: u16, y: u16) -> &mut Self {
        self.press(x, y);
        self.release(x, y);
        self.draw();
        self
    }

    fn footer_click(&mut self, col: u16) -> &mut Self {
        let footer = self.footer();
        self.click(footer.x + col, footer.y)
    }

    fn requests(&self) -> &[(CopyTarget, String)] {
        &self.app.backend.copy_requests
    }

    fn origin(&self) -> orch_tui::state::CopyOrigin {
        let source = self.viewer().document().source.clone();
        self.app
            .state
            .live_copy_origin(&orch_tui::hit::TextSurfaceId::Viewer(source))
            .cloned()
            .expect("live viewer origin")
    }
}

fn doc(body: &str) -> ViewerDocument {
    ViewerDocument {
        source: ViewerSource::Message(7),
        title: "Message 7".into(),
        body: Arc::from(body),
        representation: Representation::Raw,
        raw_body: None,
        completeness: Completeness::Complete,
        original_bytes: body.len(),
        captured_at_ms: START_MS,
        initial_anchor: 0,
    }
}

fn human_doc(human: &str, raw: &str) -> ViewerDocument {
    ViewerDocument {
        body: Arc::from(human),
        representation: Representation::Human,
        raw_body: Some(Arc::from(raw)),
        original_bytes: raw.len(),
        ..doc(human)
    }
}

/// Whether the shared D2 helper is real (B0) or the I0 inert seed.
fn shared_navigation() -> bool {
    let mut state = ScrollState {
        selected: None,
        offset: 0,
    };
    navigate(
        &mut state,
        NavAction::Down,
        ScrollMetrics {
            items: 0,
            display_rows: 10,
            viewport_rows: 2,
            selection_rows: &[],
            mode: ScrollMode::Text,
        },
    );
    state.offset == 1
}

macro_rules! needs_shared_navigation {
    () => {
        if !shared_navigation() {
            eprintln!("SKIPPED: requires B0's shared D2 navigate (I0 inert seed on this branch)");
            return;
        }
    };
}

fn shift() -> KeyModifiers {
    KeyModifiers::SHIFT
}

fn ctrl() -> KeyModifiers {
    KeyModifiers::CONTROL
}

// ── D10.3 geometry and O3 provenance ────────────────────────────────────

#[test]
fn tv01_geometry_matches_the_d10_oracle_at_minimum_and_default_sizes() {
    for (w, h, body_rows, footer_row) in [(40, 14, 4, 12), (80, 24, 14, 22), (120, 40, 30, 38)] {
        let mut v = V::sized(w, h);
        v.open(doc("hello"));
        let body = v.body();
        assert_eq!((body.y, body.height), (8, body_rows), "{w}x{h} body");
        assert_eq!((body.x, body.width), (1, w - 2), "{w}x{h} margins");
        assert_eq!(v.footer().y, footer_row, "{w}x{h} footer");
        // Rail and feedback never take body rows.
        v.key(KeyCode::F(6));
        assert_eq!(v.body().height, body_rows, "{w}x{h} body with rail");
    }
}

#[test]
fn tv02_provenance_and_reopen_guidance_are_always_visible() {
    let mut v = V::sized(40, 14);
    v.open(human_doc("decoded", "{\"text\":\"decoded\"}"));
    let row0 = v.line(6);
    let row1 = v.line(7);
    assert!(row0.contains("Snapshot 00:16:40Z Human 7B"), "{row0}");
    assert!(row1.contains("Esc closes; reopen for latest"), "{row1}");
    assert!(
        row1.contains("r:H/R"),
        "the toggle fits beside the guidance: {row1}"
    );
    // Still there with the rail open and with feedback showing.
    v.key(KeyCode::F(6));
    assert!(v.line(6).contains("Snapshot") && v.line(7).contains("reopen for latest"));
    v.key(KeyCode::Enter);
    assert!(v.line(6).contains("Snapshot") && v.line(7).contains("reopen for latest"));
    assert_eq!(v.footer_text(), copy::SUMMARY_PENDING);
}

#[test]
fn tv03_maximum_admitted_size_fits_the_38_cell_provenance_row() {
    let body = "x".repeat(1_048_576);
    let mut v = V::sized(40, 14);
    v.open(doc(&body));
    let row = v.line(6);
    assert!(
        row.contains("Snapshot 00:16:40Z Raw 1048576B"),
        "the byte count is never elided: {row}"
    );
}

// ── V1: complete text ───────────────────────────────────────────────────

fn fifty_kib() -> String {
    let mut body = String::new();
    let mut line = 0;
    while body.len() < 49_500 {
        body.push_str(&format!(
            "line {line:05} — ünïcödé 中文 🦀 padding padding\n"
        ));
        line += 1;
    }
    body.push_str("SUFFIX-MARKER-AFTER-49000");
    assert!(body.find("SUFFIX").unwrap() > 49_000);
    body
}

#[test]
fn v1_the_whole_body_is_reachable_and_ctrl_y_copies_every_byte() {
    let body = fifty_kib();
    let mut v = V::sized(80, 24);
    v.open(doc(&body));
    assert!(!v.text().contains("SUFFIX-MARKER"));
    v.key_mod(KeyCode::End, ctrl());
    assert!(
        v.text().contains("SUFFIX-MARKER-AFTER-49000"),
        "Ctrl+End reveals the end"
    );
    v.key_mod(KeyCode::Char('y'), ctrl());
    assert_eq!(v.requests(), &[(CopyTarget::Auto, body.clone())]);
}

#[test]
fn v1_end_scrolls_to_the_suffix_through_the_shared_helper() {
    needs_shared_navigation!();
    let body = fifty_kib();
    let mut v = V::sized(80, 24);
    v.open(doc(&body));
    v.key(KeyCode::End);
    assert!(v.text().contains("SUFFIX-MARKER-AFTER-49000"));
    v.key(KeyCode::Home);
    assert!(v.text().contains("line 00000"));
}

// ── V2: Unicode, tabs, soft wraps and exact selection ───────────────────

#[test]
fn v2_reverse_drag_across_a_soft_wrap_selects_exact_source_bytes() {
    let body = format!("A\t中é👩\u{200d}💻{}\n  tail \n", "x".repeat(40));
    let mut v = V::sized(40, 14);
    v.open(doc(&body));
    let b = v.body();
    // Row 2 is "  tail " (row 0 soft-wraps into row 1). Press on its `i`.
    assert!(v.line(b.y + 2).contains("  tail"), "{}", v.text());
    v.press(b.x + 4, b.y + 2);
    v.mouse(MouseEventKind::Drag(MouseButton::Left), b.x + 9, b.y + 1);
    // The tab spans columns 1..4, so column 4 is the first cell of 中.
    v.mouse(MouseEventKind::Drag(MouseButton::Left), b.x + 4, b.y);
    v.release(b.x + 4, b.y);
    v.draw();
    let start = body.find('中').unwrap();
    let end = body.find("tail").unwrap() + "tai".len();
    assert_eq!(v.viewer().selected_text(), Some(&body[start..end]));

    v.key(KeyCode::Char('y'));
    assert_eq!(
        v.requests().last().unwrap(),
        &(CopyTarget::Auto, body[start..end].to_string())
    );
    v.key_mod(KeyCode::Char('y'), ctrl());
    assert_eq!(
        v.requests().last().unwrap(),
        &(CopyTarget::Auto, body.clone()),
        "Ctrl+Y copies the whole body, tab, newlines and outer spaces included"
    );
}

#[test]
fn v2_clicking_a_wide_glyph_s_trailing_cell_takes_the_whole_glyph() {
    let mut v = V::sized(40, 14);
    v.open(doc("中文ab"));
    let b = v.body();
    // Press the trailing cell of 中 and drag right onto `a`.
    v.press(b.x + 1, b.y);
    v.mouse(MouseEventKind::Drag(MouseButton::Left), b.x + 4, b.y);
    v.release(b.x + 4, b.y);
    assert_eq!(v.viewer().selected_text(), Some("中文a"));
}

#[test]
fn v2_a_click_without_movement_selects_nothing() {
    let mut v = V::sized(40, 14);
    v.open(doc("abc"));
    let b = v.body();
    v.click(b.x + 1, b.y);
    assert_eq!(v.viewer().selection(), None);
    v.key(KeyCode::Char('y'));
    assert!(
        v.requests().is_empty(),
        "y with no selection makes no request"
    );
    assert_eq!(v.viewer().notice(), Some(LocalNotice::NoSelection));
    assert_eq!(v.footer_text(), copy::SUMMARY_NO_SELECTION);
}

// ── C2: hostile controls are drawn, never executed ──────────────────────

#[test]
fn c2_terminal_controls_render_as_placeholders_and_copy_exactly() {
    let body = "\u{1b}]52;c;ATTACK\u{7}\u{1b}\\ ok\u{9c};semi\u{202e}evil\nnext";
    let mut v = V::sized(80, 24);
    v.open(doc(body));
    let frame = v.text();
    for raw in ['\u{1b}', '\u{7}', '\u{9c}', '\u{202e}'] {
        assert!(!frame.contains(raw), "{raw:?} reached the frame");
    }
    assert!(
        frame.contains('\u{241b}'),
        "ESC is drawn as a control picture"
    );
    assert!(frame.contains("<9C>") && frame.contains("<202E>"));
    assert!(v.line(6).contains("controls shown"));
    v.key_mod(KeyCode::Char('y'), ctrl());
    assert_eq!(v.requests(), &[(CopyTarget::Auto, body.to_string())]);
}

#[test]
fn c2_a_hostile_title_cannot_reach_the_terminal() {
    let mut document = doc("body");
    document.title = "evil\u{1b}[2J\u{7}title".into();
    let mut v = V::sized(80, 24);
    v.open(document);
    assert!(!v.text().contains('\u{1b}'));
}

// ── V3: frozen snapshot, close never quits ──────────────────────────────

#[test]
fn v3_the_snapshot_survives_eviction_and_q_or_esc_close_without_quitting() {
    let mut v = V::sized(80, 24);
    v.app.state.messages.push(
        orch_tui::messages::Message::new(
            7,
            0,
            "live",
            orch_tui::theme::Token::White,
            orch_tui::messages::MsgType::Output,
        )
        .with_detail(Some("frozen body".into())),
    );
    let document = orch_tui::messages::message_document(&v.app.state.messages[0], 0, START_MS);
    v.open(document);
    v.app.state.messages.clear();
    assert!(v.draw().contains("frozen body"));
    let origin = v.origin();
    v.key(KeyCode::Char('q'));
    assert!(v.app.state.viewer.is_none());
    assert!(
        !v.app.state.should_exit,
        "q closes the viewer, never the app"
    );
    assert!(
        !v.app.state.copy_origin_is_live(&origin),
        "closing retires the origin"
    );

    v.open(doc("again"));
    v.key(KeyCode::Esc);
    assert!(v.app.state.viewer.is_none());
    assert!(!v.app.state.should_exit);
}

#[test]
fn v3_row1_esc_closes_is_a_clickable_close_control() {
    let mut v = V::sized(40, 14);
    v.open(doc("text"));
    v.click(1 + 4, 7);
    assert!(
        v.app.state.viewer.is_none(),
        "clicking `Esc closes` closes the viewer"
    );
    assert!(!v.app.state.should_exit);
}

#[test]
fn v3_background_input_is_swallowed_while_the_viewer_is_open() {
    let mut v = V::sized(80, 24);
    v.open(doc("text"));
    for code in [
        KeyCode::Char('d'),
        KeyCode::Char('n'),
        KeyCode::Tab,
        KeyCode::Char('/'),
    ] {
        v.key(code);
    }
    v.app.handle_paste("pasted");
    assert!(v.app.backend.calls.is_empty(), "no background action ran");
    assert!(v.app.state.viewer.is_some());
    assert_eq!(v.app.state.input_mode, orch_tui::state::InputMode::None);
    // A click on the header row behind the overlay is swallowed too.
    v.click(5, 1);
    assert!(v.app.state.viewer.is_some());
}

// ── V4: drag, resize and capture loss ───────────────────────────────────

#[test]
fn v4_resize_or_capture_loss_cancels_a_drag_but_keeps_the_range() {
    let mut v = V::sized(80, 24);
    v.open(doc("abcdefghij"));
    let b = v.body();
    v.press(b.x, b.y);
    v.mouse(MouseEventKind::Drag(MouseButton::Left), b.x + 3, b.y);
    assert!(v.viewer().is_dragging());
    v.app.dispatch_ui_event(&orch_tui::keys::UiEvent::Terminal(
        ratatui::crossterm::event::Event::Resize(60, 20),
    ));
    assert!(!v.viewer().is_dragging());
    assert_eq!(v.viewer().selected_text(), Some("abcd"));

    v.draw();
    let b = v.body();
    v.press(b.x + 4, b.y);
    v.mouse(MouseEventKind::Drag(MouseButton::Left), b.x + 6, b.y);
    v.app.set_mouse_enabled(false);
    v.app.tick();
    assert!(!v.viewer().is_dragging(), "lost capture cancels the drag");
    assert_eq!(v.viewer().selected_text(), Some("efg"));
}

#[test]
fn v4_a_stale_frame_never_extends_or_creates_a_selection() {
    let mut v = V::sized(80, 24);
    v.open(doc("abcdefghij"));
    let b = v.body();
    v.press(b.x, b.y);
    // A frame that no longer publishes the viewer body (e.g. a stale/replaced
    // hit map) must not let a drag extend or create a selection.
    v.app.set_hits(orch_tui::hit::HitMap::default());
    v.mouse(MouseEventKind::Drag(MouseButton::Left), b.x + 5, b.y);
    v.release(b.x + 5, b.y);
    assert_eq!(v.viewer().selection(), None);
}

#[test]
fn v4_scrolling_during_a_drag_extends_onto_newly_visible_text() {
    needs_shared_navigation!();
    let body: String = (0..60).map(|i| format!("row{i:02}\n")).collect();
    let mut v = V::sized(80, 24);
    v.open(doc(&body));
    let b = v.body();
    v.press(b.x, b.y);
    v.key(KeyCode::PageDown);
    let b = v.body();
    v.mouse(MouseEventKind::Drag(MouseButton::Left), b.x + 4, b.y);
    v.release(b.x + 4, b.y);
    let selected = v.viewer().selected_text().unwrap().to_string();
    assert!(
        selected.starts_with("row00\n") && selected.ends_with("row14"),
        "{selected:?}"
    );
}

// ── N3/N8: text paging and wheel ────────────────────────────────────────

#[test]
fn n3_text_paging_follows_the_d2_oracle() {
    needs_shared_navigation!();
    // 23 visual rows in a 10-row body: 80x17 gives H-7-3 = 7… so size it.
    let body: String = (0..22).map(|i| format!("r{i}\n")).collect::<String>() + "r22";
    let mut v = V::sized(80, 20);
    v.open(doc(&body));
    assert_eq!(v.body().height, 10);
    let mut tops = Vec::new();
    for code in [
        KeyCode::PageDown,
        KeyCode::PageDown,
        KeyCode::PageUp,
        KeyCode::End,
        KeyCode::Home,
    ] {
        v.key(code);
        tops.push(v.viewer().top_row());
    }
    assert_eq!(tops, vec![10, 13, 3, 13, 0]);
}

#[test]
fn n8_three_wheel_notches_move_nine_rows_and_a_fourth_is_dropped() {
    needs_shared_navigation!();
    let body: String = (0..100).map(|i| format!("r{i}\n")).collect();
    let mut v = V::sized(80, 24);
    v.open(doc(&body));
    let b = v.body();
    for _ in 0..4 {
        v.mouse(MouseEventKind::ScrollDown, b.x + 1, b.y + 1);
    }
    assert_eq!(v.viewer().top_row(), 9);
    v.draw();
    v.mouse(MouseEventKind::ScrollUp, b.x + 1, b.y + 1);
    assert_eq!(v.viewer().top_row(), 6);
}

// ── Anchor and reflow ───────────────────────────────────────────────────

#[test]
fn the_initial_anchor_row_opens_at_the_top() {
    let body: String = (0..100).map(|i| format!("row{i:02}\n")).collect();
    let anchor = body.find("row42").unwrap() + 2;
    let mut v = V::sized(80, 24);
    let mut document = doc(&body);
    document.initial_anchor = anchor;
    v.open(document);
    let b = v.body();
    assert!(v.line(b.y).contains("row42"), "{}", v.text());
    assert_eq!(v.viewer().caret(), anchor);
}

#[test]
fn reflow_on_resize_keeps_the_top_source_byte_on_top() {
    // Enough paragraphs that the anchor row is never clamped by the end of
    // the document at 120x40, so the anchor really opens on top.
    let body: String = (0..100)
        .map(|i| format!("paragraph {i:02} {}\n", "word ".repeat(12)))
        .collect();
    let anchor = body.find("paragraph 17").unwrap();
    let mut v = V::sized(120, 40);
    let mut document = doc(&body);
    document.initial_anchor = anchor;
    v.open(document);
    assert!(v.line(v.body().y).contains("paragraph 17"));
    let top = v.viewer().top_byte();
    v.app.state.width = 40;
    v.app.state.height = 14;
    v.term.backend_mut().resize(40, 14);
    v.draw();
    assert!(v.line(v.body().y).contains("paragraph 17"), "{}", v.text());
    assert_eq!(
        v.viewer().top_byte(),
        top,
        "the row holding the anchor byte stays on top"
    );
}

// ── V5: Human/Raw ───────────────────────────────────────────────────────

#[test]
fn v5_switching_representation_clears_the_range_and_copies_the_labelled_bytes() {
    let human = "Run this:\n  cargo test -p orch-tui \"quoted\"\n";
    let raw = serde_json::to_string(&serde_json::json!({
        "type": "item.completed",
        "item": {"type": "agent_message", "text": human},
    }))
    .unwrap();
    let mut v = V::sized(80, 24);
    v.open(human_doc(human, &raw));
    // Select the command line with the keyboard and copy it in Human.
    let start = human.find("cargo").unwrap();
    v.app.state.viewer.as_mut().unwrap();
    v.key(KeyCode::Down).key(KeyCode::Right); // caret placement is scroll-free here
    let mut v = V::sized(80, 24);
    let mut document = human_doc(human, &raw);
    document.initial_anchor = start;
    v.open(document);
    for _ in 0.."cargo test -p orch-tui \"quoted\"".len() {
        v.key_mod(KeyCode::Right, shift());
    }
    v.key(KeyCode::Char('y'));
    assert_eq!(
        v.requests().last().unwrap().1,
        "cargo test -p orch-tui \"quoted\"",
        "the decoded command copies exactly, quotes unescaped"
    );

    v.key(KeyCode::Char('r'));
    assert_eq!(v.viewer().representation(), Representation::Raw);
    assert_eq!(v.viewer().selection(), None, "switching clears the range");
    assert_eq!(v.viewer().top_byte(), 0);
    assert!(
        v.footer_text().starts_with("Selection cleared"),
        "{}",
        v.footer_text()
    );
    assert!(v.line(6).contains(" Raw "));
    v.key_mod(KeyCode::Char('y'), ctrl());
    assert_eq!(
        v.requests().last().unwrap(),
        &(CopyTarget::Auto, raw.clone())
    );

    v.key(KeyCode::Char('r'));
    v.key_mod(KeyCode::Char('y'), ctrl());
    assert_eq!(
        v.requests().last().unwrap(),
        &(CopyTarget::Auto, human.to_string())
    );
}

#[test]
fn v5_a_raw_only_document_has_no_toggle() {
    let mut v = V::sized(80, 24);
    v.open(doc("{\"type\":\"mystery\"}"));
    assert!(!v.line(7).contains("r:H/R"));
    v.key(KeyCode::Char('r'));
    assert_eq!(v.viewer().representation(), Representation::Raw);
    assert_eq!(v.viewer().notice(), None);
}

#[test]
fn v5_human_and_raw_pin_at_most_two_mebibytes_plus_one_bounded_index() {
    let human = "h\n".repeat(512 * 1024);
    let raw = "r".repeat(1024 * 1024);
    let mut v = V::sized(80, 24);
    v.open(human_doc(&human, &raw));
    let viewer = v.viewer();
    assert!(viewer.index_bytes() <= ROW_INDEX_MAX_BYTES);
    assert_eq!(
        viewer.pinned_bytes(),
        human.len() + raw.len() + viewer.index_bytes()
    );
    assert!(viewer.pinned_bytes() <= 2 * 1024 * 1024 + ROW_INDEX_MAX_BYTES);
    // The switch drops the Human index before the Raw one is built.
    v.app.state.viewer.as_mut().unwrap();
    v.app
        .handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
    assert_eq!(
        v.viewer().index_bytes(),
        0,
        "only the active representation is indexed"
    );
    v.draw();
    assert!(v.viewer().index_bytes() > 0);
}

// ── V6: frozen entity snapshot ──────────────────────────────────────────

#[test]
fn v6_a_field_snapshot_never_refreshes_and_copies_its_frozen_bytes() {
    let mut v = V::sized(80, 24);
    v.open(ViewerDocument {
        source: ViewerSource::Entity {
            kind: EntityType::Goal,
            id: "g1".into(),
            field: "progress".into(),
        },
        title: "Goal · progress".into(),
        representation: Representation::Field,
        ..doc("progress at capture")
    });
    v.app.state.goal_progress_report = Some("changed underneath".into());
    let frame = v.draw();
    assert!(frame.contains("progress at capture") && !frame.contains("changed underneath"));
    assert!(v.line(6).contains("Snapshot") && v.line(6).contains("Field"));
    v.key_mod(KeyCode::Char('y'), ctrl());
    assert_eq!(
        v.requests(),
        &[(CopyTarget::Auto, "progress at capture".to_string())]
    );
}

// ── V7: keyboard and mouse select the same bytes ────────────────────────

#[test]
fn v7_shift_arrows_select_the_same_passage_as_a_mouse_drag() {
    let body = "pre 中é👩\u{200d}💻 post";
    let mut mouse = V::sized(80, 24);
    mouse.open(doc(body));
    let b = mouse.body();
    mouse.press(b.x + 4, b.y);
    // Cells: 中 4-5, é 6, 👩‍💻 7-8. Drag ends on the emoji's trailing cell,
    // which selects the whole grapheme (pointer cell is inclusive).
    mouse.mouse(MouseEventKind::Drag(MouseButton::Left), b.x + 8, b.y);
    mouse.release(b.x + 8, b.y);
    let by_mouse = mouse.viewer().selected_text().map(str::to_string);

    let mut keys = V::sized(80, 24);
    keys.open(doc(body));
    for _ in 0..4 {
        keys.key(KeyCode::Right);
    }
    for _ in 0..3 {
        keys.key_mod(KeyCode::Right, shift());
    }
    assert_eq!(keys.viewer().selected_text().map(str::to_string), by_mouse);
    assert_eq!(by_mouse.as_deref(), Some("中é👩\u{200d}💻"));

    // Copy selection through the F6 rail, and through Ctrl+Alt+Y.
    keys.key(KeyCode::F(6))
        .key(KeyCode::Right)
        .key(KeyCode::Enter);
    keys.key_mod(KeyCode::Char('y'), ctrl() | KeyModifiers::ALT);
    assert_eq!(
        keys.requests(),
        &[
            (CopyTarget::Auto, "中é👩\u{200d}💻".to_string()),
            (CopyTarget::Auto, "中é👩\u{200d}💻".to_string()),
        ]
    );
    assert!(
        keys.app.backend.calls.is_empty(),
        "no backend action leaked"
    );
}

#[test]
fn v7_vertical_and_row_edge_extension_and_document_ends() {
    let body = "abc\ndefg\nhi";
    let mut v = V::sized(80, 24);
    v.open(doc(body));
    v.key(KeyCode::Right);
    v.key_mod(KeyCode::Down, shift());
    assert_eq!(v.viewer().selected_text(), Some("bc\nd"));
    v.key_mod(KeyCode::End, shift());
    assert_eq!(v.viewer().selected_text(), Some("bc\ndefg"));
    v.key_mod(KeyCode::End, ctrl() | shift());
    assert_eq!(v.viewer().selected_text(), Some("bc\ndefg\nhi"));
    v.key_mod(KeyCode::Home, ctrl());
    assert_eq!((v.viewer().selection(), v.viewer().caret()), (None, 0));
}

// ── C6: the F6 rail, keyboard and mouse ─────────────────────────────────

#[test]
fn c6_f6_end_enter_copies_the_selection_or_all_on_this_host() {
    let mut v = V::sized(40, 14);
    v.open(doc("whole body"));
    v.key(KeyCode::F(6)).key(KeyCode::End).key(KeyCode::Enter);
    assert_eq!(
        v.requests(),
        &[(CopyTarget::Native, "whole body".to_string())]
    );
    assert_eq!(v.viewer().rail(), None, "Enter collapses the rail");

    v.key_mod(KeyCode::Right, shift())
        .key_mod(KeyCode::Right, shift());
    v.key(KeyCode::F(6)).key(KeyCode::End).key(KeyCode::Enter);
    assert_eq!(v.requests()[1], (CopyTarget::Native, "wh".to_string()));
    assert_eq!(
        v.viewer().selected_text(),
        Some("wh"),
        "the range survives the copy"
    );
}

#[test]
fn c6_the_rail_cycles_with_arrows_tab_home_and_end_and_esc_or_f6_cancel() {
    let mut v = V::sized(40, 14);
    v.open(doc("text"));
    v.key(KeyCode::F(6));
    let mut seen = vec![v.viewer().rail().unwrap()];
    for code in [KeyCode::Right, KeyCode::Tab, KeyCode::Right, KeyCode::Right] {
        v.key(code);
        seen.push(v.viewer().rail().unwrap());
    }
    assert_eq!(
        seen,
        vec![
            TextAction::CopyAll,
            TextAction::CopySelection,
            TextAction::CopyTerminal,
            TextAction::CopyNative,
            TextAction::CopyAll
        ]
    );
    v.key(KeyCode::Left);
    assert_eq!(
        v.viewer().rail(),
        Some(TextAction::CopyNative),
        "Left wraps"
    );
    v.key(KeyCode::BackTab).key(KeyCode::Home);
    assert_eq!(v.viewer().rail(), Some(TextAction::CopyAll));
    v.key(KeyCode::Esc);
    assert_eq!(v.viewer().rail(), None);
    assert!(
        v.app.state.viewer.is_some(),
        "Esc on the rail does not close the viewer"
    );
    v.key(KeyCode::F(6)).key(KeyCode::F(6));
    assert_eq!(v.viewer().rail(), None);
    assert!(
        v.requests().is_empty(),
        "no navigation or cancel copies anything"
    );
}

#[test]
fn c6_copy_selection_with_nothing_selected_makes_zero_requests() {
    let mut v = V::sized(40, 14);
    v.open(doc("text"));
    v.key(KeyCode::F(6)).key(KeyCode::Right).key(KeyCode::Enter);
    assert!(v.requests().is_empty());
    assert_eq!(v.footer_text(), copy::SUMMARY_NO_SELECTION);
}

#[test]
fn c6_mouse_chip_then_navigate_and_click_copy_on_this_host() {
    let mut v = V::sized(40, 14);
    v.open(doc("by mouse"));
    v.footer_click(2);
    assert_eq!(v.viewer().rail(), Some(TextAction::CopyAll));
    v.clock.advance_ms(400);
    for _ in 0..3 {
        v.footer_click(33);
    }
    assert_eq!(v.viewer().rail(), Some(TextAction::CopyNative));
    assert!(v.requests().is_empty());
    v.footer_click(5);
    assert_eq!(
        v.requests(),
        &[(CopyTarget::Native, "by mouse".to_string())]
    );
}

#[test]
fn c6_terminal_and_native_routes_carry_selection_or_all_scope_labels() {
    let mut v = V::sized(40, 14);
    v.open(doc("abc"));
    v.key(KeyCode::F(6)).key(KeyCode::End);
    assert_eq!(v.footer_text(), " < Copy on this host [all]       > x");
    v.key(KeyCode::Esc);
    v.key_mod(KeyCode::Right, shift());
    v.key(KeyCode::F(6)).key(KeyCode::Left).key(KeyCode::Left);
    assert_eq!(v.footer_text(), " < Copy via terminal [selection] > x");
    v.key(KeyCode::Enter);
    assert_eq!(v.requests(), &[(CopyTarget::Terminal, "a".to_string())]);
}

// ── C7: fixed slot geometry and D10.6 footer identity ───────────────────

#[test]
fn c7_every_rail_label_sits_in_the_fixed_29_cell_slot() {
    for selected in [false, true] {
        for steps in 0..4 {
            let mut v = V::sized(40, 14);
            v.open(doc("abc"));
            if selected {
                v.key_mod(KeyCode::Right, shift());
            }
            v.key(KeyCode::F(6));
            for _ in 0..steps {
                v.key(KeyCode::Right);
            }
            let footer = v.footer();
            let line: Vec<String> = (0..37)
                .map(|c| {
                    v.term.backend().buffer()[(footer.x + c, footer.y)]
                        .symbol()
                        .to_string()
                })
                .collect();
            assert_eq!(line[1], "<");
            assert_eq!(line[33], ">");
            assert_eq!(line[35], "x");
            let label = v.viewer().rail_label(v.viewer().rail().unwrap());
            assert!(label.len() <= 29, "{label}");
            let slot: String = line[3..32].concat();
            assert_eq!(slot.trim_end(), label, "left-aligned in [3,32)");
            // Padding and separators are inert.
            for col in [0u16, 2, 32, 34, 36] {
                let zone = v.app.state.mouse.hits.resolve(footer.x + col, footer.y);
                assert!(
                    !matches!(zone.map(|z| &z.kind), Some(ZoneKind::TextAction(_))),
                    "col {col} must be inert"
                );
            }
            if label.len() < 29 {
                let zone = v
                    .app
                    .state
                    .mouse
                    .hits
                    .resolve(footer.x + 3 + label.len() as u16, footer.y);
                assert!(!matches!(
                    zone.map(|z| &z.kind),
                    Some(ZoneKind::TextAction(_))
                ));
            }
        }
    }
}

#[test]
fn c7_rapid_next_clicks_advance_twice_and_previous_returns_with_zero_requests() {
    let mut v = V::sized(40, 14);
    v.open(doc("abc"));
    v.key(KeyCode::F(6)); // settled: keyboard arms no guard
    v.footer_click(33).footer_click(33);
    assert_eq!(v.viewer().rail(), Some(TextAction::CopyTerminal));
    v.footer_click(1).footer_click(1);
    assert_eq!(v.viewer().rail(), Some(TextAction::CopyAll));
    assert!(v.requests().is_empty());
}

#[test]
fn c7_navigation_without_a_redraw_fails_closed() {
    let mut v = V::sized(40, 14);
    v.open(doc("abc"));
    v.key(KeyCode::F(6));
    let footer = v.footer();
    v.press(footer.x + 33, footer.y);
    v.release(footer.x + 33, footer.y);
    v.press(footer.x + 33, footer.y);
    v.release(footer.x + 33, footer.y);
    assert_eq!(
        v.viewer().rail(),
        Some(TextAction::CopySelection),
        "the second click had no fresh frame"
    );
}

#[test]
fn c7_a_chip_double_click_opens_the_rail_and_copies_nothing() {
    let mut v = V::sized(40, 14);
    v.open(doc("abc"));
    v.footer_click(5);
    v.clock.advance_ms(1);
    v.footer_click(5);
    assert_eq!(v.viewer().rail(), Some(TextAction::CopyAll));
    assert!(v.requests().is_empty());
    // At the deadline a fresh click acts again.
    v.clock.advance_ms(399);
    v.footer_click(5);
    assert_eq!(v.requests().len(), 1);
}

#[test]
fn c7_a_label_double_click_makes_exactly_one_request_including_immediate_errors() {
    for fail in [false, true] {
        let mut v = V::sized(40, 14);
        v.open(doc("abc"));
        v.key(KeyCode::F(6)).key(KeyCode::End);
        if fail {
            v.app.backend.copy_fail_next = Some("Copy busy".into());
        }
        v.footer_click(5);
        v.clock.advance_ms(1);
        v.footer_click(5);
        v.clock.advance_ms(398);
        v.footer_click(14);
        assert_eq!(v.requests().len(), 1, "fail={fail}");
        let origin = v.origin();
        assert!(
            v.app.state.copy_footer_visible(&origin),
            "the duplicate clicks did not dismiss the new footer (fail={fail})"
        );
        if fail {
            assert_eq!(v.footer_text(), copy::SUMMARY_BUSY);
        } else {
            assert_eq!(v.footer_text(), copy::SUMMARY_PENDING);
        }
    }
}

#[test]
fn c7_rail_x_then_an_immediate_click_does_nothing() {
    let mut v = V::sized(40, 14);
    v.open(doc("abc"));
    v.key(KeyCode::F(6));
    v.footer_click(35);
    assert_eq!(v.viewer().rail(), None);
    v.clock.advance_ms(1);
    v.footer_click(35).footer_click(2);
    assert_eq!(
        v.viewer().rail(),
        None,
        "the chip click inside the guard is dropped"
    );
    assert!(v.requests().is_empty());
}

#[test]
fn c7_pending_x_hides_presentation_and_its_other_cells_are_inert() {
    let mut v = V::sized(40, 14);
    v.open(doc("abc"));
    v.key(KeyCode::F(6)).key(KeyCode::Enter);
    assert_eq!(v.footer_text(), copy::SUMMARY_PENDING);
    let origin = v.origin();
    for col in [0u16, 5, 14, 16] {
        v.footer_click(col);
    }
    assert!(
        v.app.state.copy_footer_visible(&origin),
        "non-x pending cells are inert"
    );
    v.footer_click(18);
    assert!(!v.app.state.copy_footer_visible(&origin));
    v.clock.advance_ms(1);
    v.footer_click(35);
    assert_eq!(v.viewer().rail(), None);
    // The association survived: the completion reappears.
    let ticket = v.app.state.copy_feedback.pending_count();
    assert_eq!(ticket, 1);
    complete_all(&mut v, CopyOutcome::NativeWritten, None);
    assert_eq!(v.footer_text(), copy::SUMMARY_NATIVE_OK);
}

#[test]
fn c7_completed_feedback_click_dismisses_and_double_clicks_do_nothing_more() {
    let mut v = V::sized(40, 14);
    v.open(doc("abc"));
    v.key_mod(KeyCode::Char('y'), ctrl());
    complete_all(&mut v, CopyOutcome::NativeWritten, None);
    assert_eq!(v.footer_text(), copy::SUMMARY_NATIVE_OK);
    v.footer_click(14);
    v.clock.advance_ms(1);
    v.footer_click(14);
    v.footer_click(22);
    assert!(
        v.footer_text().starts_with("Copy\u{2026} (F6)"),
        "{}",
        v.footer_text()
    );
    assert_eq!(v.viewer().rail(), None);
    assert_eq!(v.requests().len(), 1);
    let origin = v.origin();
    assert!(
        v.app.state.feedback_for(&origin).is_some(),
        "F1 can still read it"
    );
}

#[test]
fn c7_a_press_inside_the_guard_released_after_it_does_nothing() {
    let mut v = V::sized(40, 14);
    v.open(doc("abc"));
    v.footer_click(2); // chip -> rail, guard until t+400
    let footer = v.footer();
    v.clock.advance_ms(399);
    v.press(footer.x + 5, footer.y);
    v.clock.advance_ms(2);
    v.release(footer.x + 5, footer.y);
    v.draw();
    assert!(v.requests().is_empty(), "dropped input is not replayed");
    assert_eq!(v.viewer().rail(), Some(TextAction::CopyAll));
}

#[test]
fn c7_keyboard_is_unaffected_by_the_mouse_guard() {
    let mut v = V::sized(40, 14);
    v.open(doc("abc"));
    v.footer_click(2);
    v.key(KeyCode::End).key(KeyCode::Enter);
    assert_eq!(v.requests(), &[(CopyTarget::Native, "abc".to_string())]);
}

// ── C8: surface-specific feedback and the one Tick drain ────────────────

fn complete_all(v: &mut V, outcome: CopyOutcome, guidance: Option<&str>) {
    let tickets: Vec<_> = (1..=8)
        .map(orch_tui::backend::CopyTicket)
        .filter(|t| {
            v.app.backend.complete_copy(CopyResult {
                ticket: *t,
                outcome: outcome.clone(),
                guidance: guidance.map(str::to_string),
            })
        })
        .collect();
    assert!(!tickets.is_empty(), "nothing was pending");
    v.app.tick();
    v.draw();
}

fn editor_key() -> DraftKey {
    DraftKey {
        kind: WizardKind::EditTask,
        target_id: Some("task-a".into()),
        setting: None,
        template_key: None,
    }
}

#[test]
fn c8_viewer_and_editor_results_keep_their_own_footer_in_either_order() {
    for reverse in [false, true] {
        let mut v = V::sized(40, 14);
        let editor = v
            .app
            .state
            .register_editor_origin(editor_key(), "description".into())
            .unwrap();
        let e_ticket = v
            .app
            .backend
            .copy_text_via("editor bytes", CopyTarget::Native);
        v.app.state.register_copy_request(
            editor.clone(),
            CopyTarget::Native,
            e_ticket.clone(),
            START_MS,
        );
        v.open(doc("viewer bytes"));
        v.key(KeyCode::F(6))
            .key(KeyCode::Right)
            .key(KeyCode::Right)
            .key(KeyCode::Enter);
        let e = e_ticket.unwrap();
        let viewer_ticket = orch_tui::backend::CopyTicket(e.0 + 1);
        let e_result = CopyResult {
            ticket: e,
            outcome: CopyOutcome::Failed("Copy on this host failed: denied".into()),
            guidance: None,
        };
        let v_result = CopyResult {
            ticket: viewer_ticket,
            outcome: CopyOutcome::TerminalRequested,
            guidance: Some(GUIDANCE_TMUX.into()),
        };
        let order = if reverse {
            [v_result, e_result]
        } else {
            [e_result, v_result]
        };
        for result in order {
            assert!(v.app.backend.complete_copy(result));
        }
        v.app.tick();
        v.draw();
        assert_eq!(v.footer_text(), copy::SUMMARY_TMUX, "reverse={reverse}");
        let feedback = v.app.state.feedback_for(&editor).expect("editor slot");
        assert!(matches!(feedback.outcome, CopyOutcome::Failed(_)));
        assert_eq!(v.app.state.copy_feedback.pending_count(), 0, "both drained");
        assert_eq!(
            v.requests(),
            &[
                (CopyTarget::Native, "editor bytes".to_string()),
                (CopyTarget::Terminal, "viewer bytes".to_string()),
            ]
        );
        // The editor slot survives the viewer closing.
        v.key(KeyCode::Esc);
        assert!(v.app.state.feedback_for(&editor).is_some());
    }
}

#[test]
fn c8_sty_and_both_mux_summaries_fit_the_minimum_footer() {
    for (guidance, summary) in [
        (GUIDANCE_STY, copy::SUMMARY_STY),
        (copy::GUIDANCE_STY_TMUX, copy::SUMMARY_STY_TMUX),
    ] {
        let mut v = V::sized(40, 14);
        v.open(doc("abc"));
        v.key_mod(KeyCode::Char('y'), ctrl());
        complete_all(&mut v, CopyOutcome::TerminalRequested, Some(guidance));
        assert_eq!(v.footer_text(), summary);
        let origin = v.origin();
        assert_eq!(
            v.app
                .state
                .feedback_for(&origin)
                .unwrap()
                .guidance
                .as_deref(),
            Some(guidance),
            "the full fixed guidance is retained for F1"
        );
    }
}

#[test]
fn c8_two_completed_but_undrained_tickets_keep_the_third_busy_until_tick() {
    let mut v = V::sized(40, 14);
    v.open(doc("abc"));
    v.key_mod(KeyCode::Char('y'), ctrl());
    v.key_mod(KeyCode::Char('y'), ctrl());
    for ticket in [1, 2] {
        assert!(v.app.backend.complete_copy(CopyResult {
            ticket: orch_tui::backend::CopyTicket(ticket),
            outcome: CopyOutcome::NativeWritten,
            guidance: None,
        }));
    }
    v.key_mod(KeyCode::Char('y'), ctrl());
    assert_eq!(v.footer_text(), copy::SUMMARY_BUSY);
    v.app.tick();
    v.draw();
    v.key_mod(KeyCode::Char('y'), ctrl());
    assert_eq!(
        v.footer_text(),
        copy::SUMMARY_PENDING,
        "capacity freed after the drain"
    );
    assert_eq!(v.requests().len(), 4);
}

#[test]
fn c8_a_late_result_after_close_drains_without_relabelling_a_reopened_viewer() {
    let mut v = V::sized(40, 14);
    v.open(doc("abc"));
    v.key_mod(KeyCode::Char('y'), ctrl());
    v.key(KeyCode::Esc);
    v.open(doc("abc"));
    let origin = v.origin();
    complete_all(&mut v, CopyOutcome::NativeWritten, None);
    assert_eq!(v.app.state.copy_feedback.pending_count(), 0, "drained");
    assert!(
        v.app.state.feedback_for(&origin).is_none(),
        "new generation untouched"
    );
    assert!(v.footer_text().starts_with("Copy\u{2026} (F6)"));
}

#[test]
fn c8_tick_drains_completions_with_no_viewer_open() {
    let mut v = V::sized(40, 14);
    let field = v
        .app
        .state
        .register_field_origin(ViewerSource::Message(3))
        .unwrap();
    let ticket = v.app.backend.copy_text_via("field", CopyTarget::Auto);
    v.app
        .state
        .register_copy_request(field.clone(), CopyTarget::Auto, ticket.clone(), START_MS);
    assert!(v.app.backend.complete_copy(CopyResult {
        ticket: ticket.unwrap(),
        outcome: CopyOutcome::NativeWritten,
        guidance: None,
    }));
    v.app.tick();
    assert_eq!(
        v.app.state.feedback_for(&field).map(|f| f.outcome.clone()),
        Some(CopyOutcome::NativeWritten)
    );
    assert!(
        v.app.backend.poll_copy_results().is_empty(),
        "exactly one drain"
    );
}

// ── D10.5: F1 is B0's; viewer state survives help ───────────────────────

#[test]
fn f1_is_declined_and_rail_selection_and_range_survive_help() {
    let mut v = V::sized(80, 24);
    v.open(doc("abcdef"));
    v.key_mod(KeyCode::Right, shift())
        .key_mod(KeyCode::Right, shift());
    v.key(KeyCode::F(6)).key(KeyCode::End);
    let before = (
        v.viewer().rail(),
        v.viewer().selection(),
        v.viewer().top_byte(),
    );
    let origin = v.origin();
    v.key(KeyCode::F(1));
    assert!(v.app.state.show_help, "F1 reached the help owner");
    assert_eq!(
        (
            v.viewer().rail(),
            v.viewer().selection(),
            v.viewer().top_byte()
        ),
        before,
        "F1 leaves the rail alone"
    );
    // F6 would collapse the rail if the viewer consumed it; while help is
    // shown the viewer declines it. (I0's legacy help dismisses itself on a
    // non-help key; B0's contextual help keeps it. Either way the viewer
    // state must be untouched.)
    v.key(KeyCode::F(6));
    assert_eq!(
        (
            v.viewer().rail(),
            v.viewer().selection(),
            v.viewer().top_byte()
        ),
        before,
        "help-owned input never reaches the viewer"
    );
    if v.app.state.show_help {
        v.key(KeyCode::Esc);
    }
    assert!(!v.app.state.show_help);
    assert!(
        v.app.state.viewer.is_some(),
        "closing help does not close the viewer"
    );
    assert_eq!(
        (
            v.viewer().rail(),
            v.viewer().selection(),
            v.viewer().top_byte()
        ),
        before
    );
    assert_eq!(v.origin(), origin, "same generation after help");
    assert!(v.requests().is_empty());
}

// ── R2: unavailable bodies ──────────────────────────────────────────────

#[test]
fn r2_an_unavailable_body_says_why_and_never_copies() {
    let mut v = V::sized(80, 24);
    v.open(ViewerDocument {
        body: Arc::from(""),
        completeness: Completeness::Unavailable {
            reason: "Body unavailable: exceeds 1 MiB".into(),
        },
        original_bytes: 1_048_577,
        ..doc("")
    });
    let frame = v.text();
    assert!(frame.contains("Body unavailable: exceeds 1 MiB"));
    assert!(frame.contains("1048577 bytes received"));
    assert!(v.line(6).contains("unavailable") && !v.line(6).contains("0B"));
    v.key_mod(KeyCode::Char('y'), ctrl());
    v.key(KeyCode::F(6)).key(KeyCode::End).key(KeyCode::Enter);
    assert!(v.requests().is_empty());
    assert_eq!(v.viewer().notice(), Some(LocalNotice::Unavailable));
}
