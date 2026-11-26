//! Vectors of the spec's "Cursors of the eight families other than Task" that
//! the inline arm can show without a table, a leaf or a database: C7, P-B1 (its
//! inline half), I-T1 to I-T4, Q7', P-S2, P-X2 and P-X3. Everything is synthetic.
use super::layout::Kind;
use super::token::{INLINE_SKEW_MS, INLINE_TTL_MS};
use super::*;
use orch_core::store::{Cursor, Filter, PageRequest, Sort, StoreError};
use orch_domain::GoalStatus;
use std::cmp::Ordering;

const PROJECT: &str = "project-a";
const GENERATION: &str = "generation-1";
const NOW: i64 = 1_800_000_000_000;

fn cursors() -> NonTaskCursors {
    NonTaskCursors::new(GENERATION)
}

fn runs(task: Option<&str>) -> Filter {
    Filter::Runs {
        task: task.map(str::to_owned),
        agent: None,
    }
}
fn goals() -> Filter {
    Filter::Goals { status: None }
}
fn messages() -> Filter {
    Filter::Messages { recipient: None }
}
fn pending() -> Filter {
    Filter::PendingMessages {
        recipient: "agent".into(),
        now: "2026-01-01T00:00:00Z".into(),
    }
}

/// One row of the spec's capacity table.
struct Row {
    label: &'static str,
    filter: Filter,
    sort: Sort,
    capacity: usize,
}

fn row(label: &'static str, filter: Filter, sort: Sort, capacity: usize) -> Row {
    Row {
        label,
        filter,
        sort,
        capacity,
    }
}

/// The table of the spec, row by row, plus every sort `id` and the families
/// that the table names together.
fn table() -> Vec<Row> {
    vec![
        row("agents id", Filter::Agents, Sort::Id, 230),
        row("teams id", Filter::Teams, Sort::Id, 230),
        row("runs id", runs(None), Sort::Id, 230),
        row("runs of a task id", runs(Some("t")), Sort::Id, 230),
        row("messages id", messages(), Sort::Id, 230),
        row("pending messages id", pending(), Sort::Id, 230),
        row("context id", Filter::Context, Sort::Id, 230),
        row("config id", Filter::Config, Sort::Id, 230),
        row("templates id", Filter::Templates, Sort::Id, 230),
        row("goals id", goals(), Sort::Id, 230),
        row("context default", Filter::Context, Sort::Default, 230),
        row("config default", Filter::Config, Sort::Default, 230),
        row("templates default", Filter::Templates, Sort::Default, 230),
        row("agents default", Filter::Agents, Sort::Default, 222),
        row("agents name", Filter::Agents, Sort::Name, 221),
        row("teams default", Filter::Teams, Sort::Default, 229),
        row("teams name", Filter::Teams, Sort::Name, 229),
        row("runs default", runs(None), Sort::Default, 229),
        row(
            "runs of a task default",
            runs(Some("t")),
            Sort::Default,
            222,
        ),
        row(
            "runs of a task attempt",
            runs(Some("t")),
            Sort::Attempt,
            222,
        ),
        row("goals default", goals(), Sort::Default, 205),
        row("goals created_at", goals(), Sort::CreatedAt, 221),
        row("messages default", messages(), Sort::Default, 221),
        row("pending messages default", pending(), Sort::Default, 221),
    ]
}

/// `bytes` bytes of text. With `multibyte`, two-byte characters, and one ASCII
/// byte when the count is odd, so that the characters are fewer than the bytes.
fn text(bytes: usize, multibyte: bool) -> String {
    if multibyte {
        format!("{}{}", "é".repeat(bytes / 2), "x".repeat(bytes % 2))
    } else {
        "x".repeat(bytes)
    }
}

#[derive(Clone, Copy, Debug)]
enum Shape {
    /// The identity takes the bytes; a text before it is three bytes.
    LongIdentity,
    /// The text before the identity takes the bytes; the identity is one byte.
    LongLeading,
}

/// A tuple of `layout` whose texts total `total` bytes.
fn tuple(layout: &Layout, total: usize, shape: Shape, multibyte: bool) -> Tuple {
    let leading = layout
        .parts()
        .iter()
        .filter(|p| p.kind == Kind::Text)
        .count()
        - 1;
    let (lead, id) = match (leading, shape) {
        (0, _) => (0, total),
        (_, Shape::LongIdentity) => (3, total - 3),
        (_, Shape::LongLeading) => (total - 1, 1),
    };
    let mut values = Vec::new();
    let mut texts = 0;
    for part in layout.parts() {
        values.push(match part.kind {
            Kind::Text if texts < leading => {
                texts += 1;
                Component::Text(text(lead, multibyte))
            }
            Kind::Text => Component::Text(text(id, multibyte)),
            Kind::Rank => Component::Int(1),
            Kind::Presence => Component::Int(0),
            Kind::Position => Component::Int(7),
            Kind::Attempt => Component::Int(3),
        });
    }
    Tuple::new(layout, values).unwrap()
}

fn issue(row: &Row, anchor: &Tuple, now: i64) -> Option<Cursor> {
    cursors()
        .issue_inline(PROJECT, &row.filter, row.sort, anchor, now)
        .unwrap()
}

fn resume(row: &Row, cursor: &Cursor, now: i64) -> Result<Tuple, StoreError> {
    cursors()
        .resume_inline(cursor, PROJECT, &row.filter, row.sort, now)
        .map_err(|error| match error {
            crate::sqlite::SqliteError::Store(error) => error,
            other => panic!("not a store error: {other}"),
        })
}

fn layout(row: &Row) -> Layout {
    Layout::of(&row.filter, row.sort).unwrap()
}

/// A small tuple of the row's layout: `i` decides every value.
fn small(layout: &Layout, i: usize) -> Tuple {
    let mut values = Vec::new();
    let last = layout.parts().len() - 1;
    for (index, part) in layout.parts().iter().enumerate() {
        values.push(match part.kind {
            Kind::Text if index == last => Component::Text(format!("id-{i:03}")),
            // Shared prefixes, so that the next component decides.
            Kind::Text => {
                if index > 0 && i % 4 == 3 && layout.parts()[index - 1].kind == Kind::Presence {
                    Component::Text(String::new())
                } else {
                    Component::Text(format!("2026-01-0{}", i % 3))
                }
            }
            Kind::Rank => Component::Int((i % 4) as i64),
            Kind::Presence => Component::Int(i64::from(i % 4 == 3)),
            Kind::Position => Component::Int((i % 5) as i64),
            Kind::Attempt => Component::Int((i % 3) as i64),
        });
    }
    Tuple::new(layout, values).unwrap()
}

/// A walk of `rows` by pages of `limit` through inline tokens, the model of
/// what the key-taking leaves will do: a page is the rows greater than the
/// decoded tuple. With `delete`, every page's last row is deleted right after
/// the token is issued. Every token must be inline.
fn walk(row: &Row, rows: &[Tuple], limit: usize, delete: bool) -> Vec<Tuple> {
    let layout = layout(row);
    let c = cursors();
    let mut data = rows.to_vec();
    let mut seen = Vec::new();
    let mut cursor: Option<Cursor> = None;
    loop {
        let after = cursor.as_ref().map(|cursor| {
            c.resume_inline(cursor, PROJECT, &row.filter, row.sort, NOW)
                .unwrap()
        });
        let rest: Vec<Tuple> = data
            .iter()
            .filter(|t| {
                after
                    .as_ref()
                    .is_none_or(|a| layout.compare(t, a) == Ordering::Greater)
            })
            .cloned()
            .collect();
        let page: Vec<Tuple> = rest.iter().take(limit).cloned().collect();
        seen.extend(page.clone());
        if rest.len() <= limit {
            return seen;
        }
        let anchor = page.last().unwrap();
        cursor = Some(
            c.issue_inline(PROJECT, &row.filter, row.sort, anchor, NOW)
                .unwrap()
                .expect("an anchor that fits needs no table entry"),
        );
        if delete {
            data.retain(|t| t != anchor);
        }
    }
}

fn sorted(layout: &Layout, mut rows: Vec<Tuple>) -> Vec<Tuple> {
    rows.sort_by(|a, b| layout.compare(a, b));
    rows
}

#[test]
fn the_capacity_table_of_the_spec_is_what_the_layouts_derive() {
    for row in table() {
        assert_eq!(
            layout(&row).inline_capacity(),
            row.capacity,
            "{}",
            row.label
        );
    }
}

/// C7 and P-B1, the inline half: at the capacity the token is exactly 512
/// bytes of text, one byte more is not inline (and would not fit), and both
/// walk exactly after the anchor is deleted, with no character split.
#[test]
fn a_tuple_at_the_capacity_is_inline_and_one_byte_more_is_not() {
    for row in table() {
        let layout = layout(&row);
        for shape in [Shape::LongIdentity, Shape::LongLeading] {
            for multibyte in [false, true] {
                let at = tuple(&layout, row.capacity, shape, multibyte);
                let over = tuple(&layout, row.capacity + 1, shape, multibyte);
                let label = format!("{} {shape:?} multibyte={multibyte}", row.label);
                assert_eq!(layout.arm(&at), Arm::Inline, "{label}");
                assert_eq!(layout.arm(&over), Arm::Retained, "{label}");
                if multibyte {
                    let chars: usize = at
                        .components()
                        .iter()
                        .map(|c| match c {
                            Component::Text(t) => t.chars().count(),
                            Component::Int(_) => 0,
                        })
                        .sum();
                    assert!(chars < row.capacity, "{label}: characters, not bytes");
                }
                let cursor = issue(&row, &at, NOW).unwrap_or_else(|| panic!("{label}: inline"));
                assert_eq!(cursor.as_str().len(), 512, "{label}: the whole token");
                assert_eq!(resume(&row, &cursor, NOW).unwrap(), at, "{label}");
                assert!(issue(&row, &over, NOW).is_none(), "{label}: one byte more");
                // The encoder confirms the bound: one byte more is 514 bytes of text.
                let forced = cursors()
                    .encode(
                        PROJECT,
                        &row.filter,
                        row.sort,
                        &layout,
                        &over,
                        NOW + INLINE_TTL_MS,
                    )
                    .unwrap();
                assert_eq!(forced.len(), 257, "{label}");
                assert!(Cursor::new(super::token::hex(&forced)).is_err(), "{label}");

                // A walk whose anchors are at the capacity, deleted after issue.
                let before = tuple(&layout, 4, Shape::LongIdentity, false);
                let after = tuple(&layout, row.capacity, Shape::LongIdentity, !multibyte);
                let rows = sorted(&layout, vec![before, at, after]);
                assert_eq!(walk(&row, &rows, 1, true), rows, "{label}");
            }
        }
    }
}

#[test]
fn a_walk_is_exact_in_every_family_and_sort_whatever_is_deleted() {
    for row in table() {
        let layout = layout(&row);
        // Ties on every leading component and descending components included.
        let rows = sorted(&layout, (0..23).map(|i| small(&layout, i)).collect());
        for limit in [1, 2, 3, 7, 22, 23, 100] {
            for delete in [false, true] {
                assert_eq!(
                    walk(&row, &rows, limit, delete),
                    rows,
                    "{} limit {limit} delete {delete}",
                    row.label
                );
            }
        }
    }
}

#[test]
fn the_order_is_the_one_the_leaves_compare_by() {
    let goal = goals();
    let g = Layout::of(&goal, Sort::Default).unwrap();
    let t = |rank: i64, presence: i64, updated: &str, position: i64, id: &str| {
        Tuple::new(
            &g,
            vec![
                Component::Int(rank),
                Component::Int(presence),
                Component::Text(updated.into()),
                Component::Int(position),
                Component::Text(id.into()),
            ],
        )
        .unwrap()
    };
    // Status rank, then present before absent (an empty text included), then
    // the newest time first, then position, then the identity's bytes.
    let expected = vec![
        t(0, 0, "2026-02", 9, "z"),
        t(0, 0, "2026-01", 1, "a"),
        t(0, 0, "", 1, "a"),
        t(0, 1, "", 1, "a"),
        t(0, 1, "", 2, "a"),
        t(0, 1, "", 2, "b"),
        t(1, 0, "2026-03", 0, "a"),
    ];
    let mut shuffled = expected.clone();
    shuffled.reverse();
    assert_eq!(sorted(&g, shuffled), expected);

    // Runs: the newest start first, or the latest attempt first, then the id.
    let started = Layout::of(&runs(None), Sort::Default).unwrap();
    let r = |s: &str, id: &str| {
        Tuple::new(
            &started,
            vec![Component::Text(s.into()), Component::Text(id.into())],
        )
        .unwrap()
    };
    let expected = vec![r("b", "x"), r("a", "x"), r("a", "y")];
    assert_eq!(
        sorted(&started, expected.iter().rev().cloned().collect()),
        expected
    );
    let attempts = Layout::of(&runs(Some("t")), Sort::Attempt).unwrap();
    let a = |n: i64, id: &str| {
        Tuple::new(
            &attempts,
            vec![Component::Int(n), Component::Text(id.into())],
        )
        .unwrap()
    };
    let expected = vec![a(3, "x"), a(2, "x"), a(2, "y")];
    assert_eq!(
        sorted(&attempts, expected.iter().rev().cloned().collect()),
        expected
    );
    // Texts are bytes, not characters or collation.
    let teams = Layout::of(&Filter::Teams, Sort::Name).unwrap();
    let n = |name: &str| {
        Tuple::new(
            &teams,
            vec![Component::Text(name.into()), Component::Text("i".into())],
        )
        .unwrap()
    };
    assert_eq!(teams.compare(&n("Z"), &n("a")), Ordering::Less);
    assert_eq!(teams.compare(&n("é"), &n("f")), Ordering::Greater);
}

#[test]
fn the_families_offer_the_sorts_the_leaves_accept_and_no_others() {
    let none = |filter: Filter, sort: Sort| assert!(Layout::of(&filter, sort).is_none());
    none(
        Filter::Tasks {
            status: None,
            goal: None,
        },
        Sort::Id,
    );
    none(Filter::Agents, Sort::CreatedAt);
    none(Filter::Agents, Sort::Attempt);
    none(Filter::Teams, Sort::Position);
    none(runs(None), Sort::Name);
    none(runs(None), Sort::Attempt);
    none(messages(), Sort::Name);
    none(messages(), Sort::CreatedAt);
    none(Filter::Context, Sort::CreatedAt);
    none(goals(), Sort::Name);
    none(goals(), Sort::UpdatedAt);
    none(goals(), Sort::Attempt);
    // A run list that names a task orders by attempt, with the default or by name.
    assert_eq!(
        Layout::of(&runs(Some("t")), Sort::Default),
        Layout::of(&runs(Some("t")), Sort::Attempt)
    );
    let error = cursors()
        .issue_inline(
            PROJECT,
            &Filter::Agents,
            Sort::Attempt,
            &small(&Layout::of(&Filter::Agents, Sort::Default).unwrap(), 0),
            NOW,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        crate::sqlite::SqliteError::Unrepresentable(_)
    ));
}

/// P-B2 and D3-A reduced to their arithmetic: the arm that a tuple's size
/// selects (the entries and the resumes of those arms are step 4's).
#[test]
fn the_size_of_a_tuple_selects_the_arm_by_bytes_with_the_presence_integer_counted() {
    let id = Layout::of(&Filter::Teams, Sort::Id).unwrap();
    let at = |n, multibyte| tuple(&id, n, Shape::LongIdentity, multibyte);
    assert_eq!(id.arm(&at(230, false)), Arm::Inline);
    assert_eq!(id.arm(&at(231, false)), Arm::Retained);
    assert_eq!(id.arm(&at(65_536, false)), Arm::Retained);
    assert_eq!(id.arm(&at(65_537, false)), Arm::Lookup);
    // D2-T: 32,768 two-byte characters, then one ASCII byte more (32,769 characters).
    let wide = at(65_536, true);
    assert_eq!(wide.size(), 65_536);
    assert_eq!(id.arm(&wide), Arm::Retained);
    let mut longer = text(65_536, true);
    longer.push('x');
    let longer = Tuple::new(&id, vec![Component::Text(longer)]).unwrap();
    assert_eq!(longer.size(), 65_537);
    assert_eq!(id.arm(&longer), Arm::Lookup);
    // P-B2: an agent's default order adds 8 for the position.
    let agent = Layout::of(&Filter::Agents, Sort::Default).unwrap();
    let by_id = |n| tuple(&agent, n, Shape::LongIdentity, false);
    assert_eq!(by_id(65_528).size(), 65_536);
    assert_eq!(agent.arm(&by_id(65_528)), Arm::Retained);
    assert_eq!(agent.arm(&by_id(65_529)), Arm::Lookup);
    // D3-A: a goal's absent `updated_at` counts 0 bytes and the presence 8, as a
    // present empty one does: 24 for three integers, 65,512 for the identity.
    let goal = Layout::of(&goals(), Sort::Default).unwrap();
    let form = |presence, id: usize| {
        Tuple::new(
            &goal,
            vec![
                Component::Int(0),
                Component::Int(presence),
                Component::Text(String::new()),
                Component::Int(0),
                Component::Text(text(id, false)),
            ],
        )
        .unwrap()
    };
    for presence in [0, 1] {
        assert_eq!(form(presence, 65_512).size(), 65_536);
        assert_eq!(goal.arm(&form(presence, 65_512)), Arm::Retained);
        assert_eq!(goal.arm(&form(presence, 65_513)), Arm::Lookup);
    }
}

#[test]
fn a_tuple_is_built_only_in_the_shape_and_range_of_its_layout() {
    let goal = Layout::of(&goals(), Sort::Default).unwrap();
    let v = |rank: i64, presence: i64, updated: &str, position: i64| {
        Tuple::new(
            &goal,
            vec![
                Component::Int(rank),
                Component::Int(presence),
                Component::Text(updated.into()),
                Component::Int(position),
                Component::Text("g".into()),
            ],
        )
    };
    assert!(v(3, 1, "", 0).is_some());
    assert!(v(4, 0, "t", 0).is_none(), "a rank that no status has");
    assert!(v(-1, 0, "t", 0).is_none());
    assert!(v(0, 2, "t", 0).is_none(), "a presence other than 0 and 1");
    assert!(v(0, 1, "t", 0).is_none(), "an absent time has no text");
    assert!(v(0, 0, "t", -1).is_none(), "a negative position");
    assert!(Tuple::new(&goal, vec![Component::Text("g".into())]).is_none());
    let run = Layout::of(&runs(Some("t")), Sort::Default).unwrap();
    let attempt = |n: i64| Tuple::new(&run, vec![Component::Int(n), Component::Text("r".into())]);
    assert!(attempt(i64::from(u32::MAX)).is_some());
    assert!(attempt(i64::from(u32::MAX) + 1).is_none());
    assert!(attempt(-1).is_none());
    assert!(Tuple::new(&run, vec![Component::Text("x".into()), Component::Int(1)]).is_none());
    assert_eq!(GoalStatus::ALL.len(), 4, "rank 3 is the highest");
}

fn teams_row() -> Row {
    row("teams name", Filter::Teams, Sort::Name, 229)
}

fn teams_token() -> (Row, Tuple, Cursor) {
    let row = teams_row();
    let anchor = small(&layout(&row), 4);
    let cursor = issue(&row, &anchor, NOW).unwrap();
    (row, anchor, cursor)
}

/// I-T1: equality passes, one millisecond later does not.
#[test]
fn an_inline_token_resumes_at_its_expiry_and_not_a_millisecond_after() {
    let (row, anchor, cursor) = teams_token();
    let expires = NOW + INLINE_TTL_MS;
    assert_eq!(resume(&row, &cursor, expires).unwrap(), anchor);
    assert!(matches!(
        resume(&row, &cursor, expires + 1),
        Err(StoreError::InvalidCursor)
    ));
}

/// I-T2: a clock set back by 5 minutes right after issue passes; 5 minutes and
/// a millisecond does not. A step forward within the day changes nothing.
#[test]
fn a_clock_set_back_five_minutes_resumes_and_a_millisecond_more_does_not() {
    let (row, anchor, cursor) = teams_token();
    assert_eq!(resume(&row, &cursor, NOW - INLINE_SKEW_MS).unwrap(), anchor);
    assert!(matches!(
        resume(&row, &cursor, NOW - INLINE_SKEW_MS - 1),
        Err(StoreError::InvalidCursor)
    ));
    assert_eq!(resume(&row, &cursor, NOW + 60 * 60 * 1000).unwrap(), anchor);
}

/// I-T3: the approved early expiry of a forward step past the expiry.
#[test]
fn a_clock_set_forward_past_the_expiry_expires_the_token() {
    let (row, _, cursor) = teams_token();
    for step in [INLINE_TTL_MS + 1, 48 * 60 * 60 * 1000] {
        assert!(matches!(
            resume(&row, &cursor, NOW + step),
            Err(StoreError::InvalidCursor)
        ));
    }
}

/// I-T4: an expiry that a forger writes is accepted up to `now + 24 h + 5 min`.
#[test]
fn a_forged_expiry_is_accepted_up_to_a_day_and_five_minutes_ahead() {
    let (row, anchor, _) = teams_token();
    let layout = layout(&row);
    let forged = |expires: i64| {
        let raw = cursors()
            .encode(PROJECT, &row.filter, row.sort, &layout, &anchor, expires)
            .unwrap();
        Cursor::new(super::token::hex(&raw)).unwrap()
    };
    let limit = NOW + INLINE_TTL_MS + INLINE_SKEW_MS;
    assert_eq!(resume(&row, &forged(limit), NOW).unwrap(), anchor);
    assert!(matches!(
        resume(&row, &forged(limit + 1), NOW),
        Err(StoreError::InvalidCursor)
    ));
    // The extremes of the claim are arithmetic in a wider type, not a wrap.
    for expires in [i64::MAX, i64::MIN] {
        assert!(matches!(
            resume(&row, &forged(expires), NOW),
            Err(StoreError::InvalidCursor)
        ));
    }
}

/// Q7': a wall clock 30 days away, either way, in every family.
#[test]
fn a_wall_clock_thirty_days_away_refuses_in_both_directions() {
    let month = 30 * 24 * 60 * 60 * 1000;
    for row in table() {
        let anchor = small(&layout(&row), 2);
        let cursor = issue(&row, &anchor, NOW).unwrap();
        for now in [NOW - month, NOW + month] {
            assert!(
                matches!(resume(&row, &cursor, now), Err(StoreError::InvalidCursor)),
                "{}",
                row.label
            );
        }
    }
}

#[test]
fn issuing_at_the_ends_of_the_clock_saturates_and_resumes() {
    let row = teams_row();
    let anchor = small(&layout(&row), 1);
    let cursor = issue(&row, &anchor, i64::MAX).unwrap();
    assert_eq!(resume(&row, &cursor, i64::MAX).unwrap(), anchor);
    let cursor = issue(&row, &anchor, i64::MIN).unwrap();
    assert_eq!(
        resume(&row, &cursor, i64::MIN + INLINE_TTL_MS).unwrap(),
        anchor
    );
}

/// P-S2: the same token across a reopen of the adapter.
#[test]
fn an_inline_token_survives_a_reopen_with_the_same_generation_and_not_with_another() {
    let (row, anchor, cursor) = teams_token();
    let reopened = NonTaskCursors::new(GENERATION);
    assert_eq!(
        reopened
            .resume_inline(&cursor, PROJECT, &row.filter, row.sort, NOW)
            .unwrap(),
        anchor
    );
    let restored = NonTaskCursors::new("generation-2");
    assert!(matches!(
        restored.resume_inline(&cursor, PROJECT, &row.filter, row.sort, NOW),
        Err(crate::sqlite::SqliteError::Store(StoreError::InvalidCursor))
    ));
}

/// P-X2, and C3 for the inline kind: another sort, project, generation, family
/// or filter is refused from the token alone.
#[test]
fn a_token_is_refused_for_another_sort_project_generation_family_or_filter() {
    for row in table() {
        let layout = layout(&row);
        let anchor = small(&layout, 3);
        let cursor = issue(&row, &anchor, NOW).unwrap();
        assert_eq!(resume(&row, &cursor, NOW).unwrap(), anchor, "{}", row.label);
        let c = cursors();
        let refused = |c: &NonTaskCursors, project: &str, filter: &Filter, sort: Sort| {
            matches!(
                c.resume_inline(&cursor, project, filter, sort, NOW),
                Err(crate::sqlite::SqliteError::Store(StoreError::InvalidCursor))
            )
        };
        assert!(
            refused(&c, "project-b", &row.filter, row.sort),
            "{}",
            row.label
        );
        assert!(
            refused(&NonTaskCursors::new("g2"), PROJECT, &row.filter, row.sort),
            "{}",
            row.label
        );
        for other in [
            Sort::Default,
            Sort::Id,
            Sort::Position,
            Sort::Name,
            Sort::Priority,
            Sort::CreatedAt,
            Sort::UpdatedAt,
            Sort::Attempt,
        ] {
            if other != row.sort {
                assert!(
                    refused(&c, PROJECT, &row.filter, other),
                    "{} {other:?}",
                    row.label
                );
            }
        }
        for filter in [
            Filter::Agents,
            Filter::Teams,
            runs(None),
            runs(Some("t")),
            messages(),
            pending(),
            Filter::Context,
            Filter::Config,
            Filter::Templates,
            goals(),
            Filter::Tasks {
                status: None,
                goal: None,
            },
        ] {
            if filter != row.filter {
                assert!(
                    refused(&c, PROJECT, &filter, row.sort),
                    "{} {filter:?}",
                    row.label
                );
            }
        }
    }
}

#[test]
fn the_filters_values_are_bound() {
    let c = cursors();
    let binding = |filter: Filter| c.binding(PROJECT, &filter).unwrap();
    let all = [
        goals(),
        Filter::Goals {
            status: Some(GoalStatus::Active),
        },
        Filter::Goals {
            status: Some(GoalStatus::Achieved),
        },
        runs(None),
        runs(Some("ab")),
        Filter::Runs {
            task: Some("a".into()),
            agent: Some("b".into()),
        },
        Filter::Runs {
            task: None,
            agent: Some("ab".into()),
        },
        Filter::Runs {
            task: Some(String::new()),
            agent: None,
        },
        messages(),
        // The same text in the same position of another family.
        Filter::Runs {
            task: Some("agent".into()),
            agent: None,
        },
        Filter::Messages {
            recipient: Some("agent".into()),
        },
        Filter::Messages {
            recipient: Some(String::new()),
        },
        pending(),
        Filter::PendingMessages {
            recipient: "agent".into(),
            now: "2026-01-01T00:00:01Z".into(),
        },
        Filter::PendingMessages {
            recipient: "agen".into(),
            now: "t2026-01-01T00:00:00Z".into(),
        },
        Filter::Agents,
        Filter::Teams,
        Filter::Context,
        Filter::Config,
        Filter::Templates,
    ];
    let digests: Vec<_> = all.iter().cloned().map(binding).collect();
    for (i, a) in digests.iter().enumerate() {
        for b in &digests[i + 1..] {
            assert_ne!(a, b);
        }
    }
    // The project and the generation are length-prefixed: no boundary moves.
    let a = NonTaskCursors::new("bc")
        .binding("a", &Filter::Teams)
        .unwrap();
    let b = NonTaskCursors::new("c")
        .binding("ab", &Filter::Teams)
        .unwrap();
    assert_ne!(a, b);
    // A Task filter has no non-Task binding.
    assert!(
        c.binding(
            PROJECT,
            &Filter::Tasks {
                status: None,
                goal: None
            }
        )
        .is_err()
    );
}

/// P-X3: the token does not know the page size or the count.
#[test]
fn a_token_resumes_with_another_limit_and_with_count() {
    let (row, anchor, cursor) = teams_token();
    for (limit, with_count) in [(1, false), (1, true), (100, false), (1000, true)] {
        let request = PageRequest {
            limit,
            cursor: Some(cursor.clone()),
            sort: row.sort,
            with_count,
        };
        let resumed = cursors()
            .resume_inline(
                request.cursor.as_ref().unwrap(),
                PROJECT,
                &row.filter,
                request.sort,
                NOW,
            )
            .unwrap();
        assert_eq!(resumed, anchor, "limit {limit} count {with_count}");
    }
    // Tokens issued in the middle of walks of different page sizes are the
    // same token for the same anchor, and the walks agree.
    let layout = layout(&row);
    let rows = sorted(&layout, (0..11).map(|i| small(&layout, i)).collect());
    let reference = walk(&row, &rows, 1, false);
    for limit in [2, 3, 5, 11] {
        assert_eq!(walk(&row, &rows, limit, true), reference, "limit {limit}");
    }
}

fn raw(row: &Row, anchor: &Tuple) -> Vec<u8> {
    let cursor = issue(row, anchor, NOW).unwrap();
    cursor
        .as_str()
        .as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn refused_raw(row: &Row, bytes: &[u8]) -> bool {
    let Ok(cursor) = Cursor::new(super::token::hex(bytes)) else {
        return true;
    };
    matches!(resume(row, &cursor, NOW), Err(StoreError::InvalidCursor))
}

#[test]
fn a_malformed_token_is_refused_whatever_is_wrong_with_it() {
    let goal = row("goals default", goals(), Sort::Default, 205);
    let goal_layout = layout(&goal);
    let anchor = small(&goal_layout, 1);
    let good = raw(&goal, &anchor);
    assert!(!refused_raw(&goal, &good));

    // Version: 0, the reserved held versions and a Task token's are not read.
    for version in [0u8, 2, 3, 4, 255] {
        let mut bad = good.clone();
        bad[0] = version;
        assert!(refused_raw(&goal, &bad), "version {version}");
    }
    // Text that is not lowercase hex of whole bytes.
    let text = Cursor::new("ABCDEF".to_owned()).unwrap();
    assert!(resume(&goal, &text, NOW).is_err());
    let upper = Cursor::new(super::token::hex(&good).to_uppercase()).unwrap();
    assert!(resume(&goal, &upper, NOW).is_err());
    let odd = Cursor::new(format!("{}0", super::token::hex(&good))).unwrap();
    assert!(resume(&goal, &odd, NOW).is_err());
    let letters = Cursor::new("zz".repeat(40)).unwrap();
    assert!(resume(&goal, &letters, NOW).is_err());
    // Cut inside the header or the integers and the length byte, which are all
    // before the text that the token ends with.
    let fixed = 26 + 8 + 8 + 1 + 3 + 8;
    for len in 0..fixed {
        assert!(refused_raw(&goal, &good[..len]), "cut at {len}");
    }
    // A length byte that runs past the end.
    let mut bad = good.clone();
    bad[26 + 16] = 200;
    assert!(refused_raw(&goal, &bad));
    // Text that is not UTF-8, in the leading text and in the identity.
    let mut bad = good.clone();
    bad[26 + 17] = 0xff;
    assert!(refused_raw(&goal, &bad), "leading text");
    let mut bad = good.clone();
    *bad.last_mut().unwrap() = 0xff;
    assert!(refused_raw(&goal, &bad), "identity");
    // Values no row has: a rank, a presence, a position, a time for an absent one.
    let int = |at: usize, value: i64| {
        let mut bad = good.clone();
        bad[at..at + 8].copy_from_slice(&value.to_be_bytes());
        bad
    };
    assert!(refused_raw(&goal, &int(26, 4)), "rank");
    assert!(refused_raw(&goal, &int(26, -1)), "negative rank");
    assert!(refused_raw(&goal, &int(34, 2)), "presence");
    let position = 26 + 16 + 1 + "2026-01-01".len();
    assert!(refused_raw(&goal, &int(position, -1)), "position");
    // An absent time with text is not a form that `issue` writes.
    let mut absent = int(34, 1);
    absent[26 + 16 + 1] = b'x';
    assert!(refused_raw(&goal, &absent), "absent time with a text");
    // An id-only token has no fixed part: any valid UTF-8 is a position.
    let id = row("context id", Filter::Context, Sort::Id, 230);
    let anchor = Tuple::new(&layout(&id), vec![Component::Text("ctx".into())]).unwrap();
    assert!(refused_raw(&id, &raw(&id, &anchor)[..25]));
}
