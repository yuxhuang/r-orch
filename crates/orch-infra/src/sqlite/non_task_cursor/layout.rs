//! The typed ordering tuple of a family and sort, its size and its arm.
use orch_core::store::{Filter, Sort};
use orch_domain::GoalStatus;
use std::cmp::Ordering;

/// A tuple of at most this many bytes is held whole by the adapter (retained);
/// one of more is held by its digests (by lookup).
pub(in crate::sqlite) const RETAINED_MAX: usize = 65_536;
/// The bytes of an integer component, and of an inline token's header: a
/// version, the sort tag, the expiry (8) and the binding digest (16).
pub(super) const INT_BYTES: usize = 8;
pub(super) const HEADER: usize = 1 + 1 + 8 + 16;
/// A `Cursor` is at most 512 bytes of text and a token is its hex.
pub(super) const MAX_RAW: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sqlite) enum Dir {
    Asc,
    Desc,
}

/// What a component is. The integers have the range the stored column has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sqlite) enum Kind {
    /// A goal's status as its index in `GoalStatus::ALL`.
    Rank,
    /// A goal's `updated_at`: 0 when present, 1 when absent. The text that
    /// follows is empty when it is absent, so the two forms do not collide.
    Presence,
    /// A row's position, never negative.
    Position,
    /// A run's attempt, at most `u32::MAX`.
    Attempt,
    Text,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sqlite) struct Part {
    pub(in crate::sqlite) kind: Kind,
    pub(in crate::sqlite) dir: Dir,
}

const fn up(kind: Kind) -> Part {
    Part {
        kind,
        dir: Dir::Asc,
    }
}
const fn down(kind: Kind) -> Part {
    Part {
        kind,
        dir: Dir::Desc,
    }
}

const ID: [Part; 1] = [up(Kind::Text)];
const AGENT: [Part; 2] = [up(Kind::Position), up(Kind::Text)];
const AGENT_NAME: [Part; 3] = [up(Kind::Text), up(Kind::Position), up(Kind::Text)];
const TEXT_ID: [Part; 2] = [up(Kind::Text), up(Kind::Text)];
const RUN: [Part; 2] = [down(Kind::Text), up(Kind::Text)];
const RUN_OF_TASK: [Part; 2] = [down(Kind::Attempt), up(Kind::Text)];
const TEXT_POSITION_ID: [Part; 3] = [up(Kind::Text), up(Kind::Position), up(Kind::Text)];
const GOAL: [Part; 5] = [
    up(Kind::Rank),
    up(Kind::Presence),
    down(Kind::Text),
    up(Kind::Position),
    up(Kind::Text),
];

/// The components of the ordering of one list, in the order the rows are
/// compared, ending with the identity (a text, compared as bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sqlite) struct Layout {
    parts: &'static [Part],
}

/// Which arm a tuple takes by its size (spec "Three kinds of cursor").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sqlite) enum Arm {
    /// The texts fit the token.
    Inline,
    /// Not inline, *S* at most 65,536.
    Retained,
    /// *S* of 65,537 or more.
    Lookup,
}

impl Layout {
    /// The ordering of `sort` over the list `filter` names, or `None` when the
    /// family does not offer that sort (or is Task, whose cursor is not this
    /// one). The pairs are those the three leaves accept.
    pub(in crate::sqlite) fn of(filter: &Filter, sort: Sort) -> Option<Self> {
        let parts: &'static [Part] = match (filter, sort) {
            (Filter::Tasks { .. }, _) => return None,
            (_, Sort::Id) => &ID,
            (Filter::Agents, Sort::Default) => &AGENT,
            (Filter::Agents, Sort::Name) => &AGENT_NAME,
            (Filter::Teams, Sort::Default | Sort::Name) => &TEXT_ID,
            (Filter::Runs { task: Some(_), .. }, Sort::Default | Sort::Attempt) => &RUN_OF_TASK,
            (Filter::Runs { task: None, .. }, Sort::Default) => &RUN,
            (Filter::Messages { .. } | Filter::PendingMessages { .. }, Sort::Default) => {
                &TEXT_POSITION_ID
            }
            (Filter::Context | Filter::Config | Filter::Templates, Sort::Default) => &ID,
            (Filter::Goals { .. }, Sort::Default) => &GOAL,
            (Filter::Goals { .. }, Sort::CreatedAt) => &TEXT_POSITION_ID,
            _ => return None,
        };
        Some(Self { parts })
    }

    /// Whether `values` has this layout's shape and ranges: one component of
    /// the right kind for each part, an integer within its kind's range, and an
    /// absent goal time (presence 1) with an empty text. A [`Tuple`] does not
    /// remember its layout, so every entry that takes one asks again.
    pub(in crate::sqlite) fn admits(&self, values: &[Component]) -> bool {
        if values.len() != self.parts.len() {
            return false;
        }
        // After presence 1 the next text is the absent goal time: empty.
        let mut absent = false;
        for (part, value) in self.parts.iter().zip(values) {
            let ok = match (part.kind, value) {
                (Kind::Text, Component::Text(text)) => {
                    let ok = !absent || text.is_empty();
                    absent = false;
                    ok
                }
                (Kind::Rank, Component::Int(v)) => {
                    usize::try_from(*v).is_ok_and(|v| v < GoalStatus::ALL.len())
                }
                (Kind::Presence, Component::Int(v)) => {
                    absent = *v == 1;
                    matches!(v, 0 | 1)
                }
                (Kind::Position, Component::Int(v)) => *v >= 0,
                (Kind::Attempt, Component::Int(v)) => u32::try_from(*v).is_ok(),
                _ => false,
            };
            if !ok {
                return false;
            }
        }
        true
    }

    pub(in crate::sqlite) fn parts(&self) -> &'static [Part] {
        self.parts
    }

    /// Text bytes that an inline token carries for this layout, the identity
    /// included: 230 less 8 for each integer and 1 for each text before the
    /// identity (its length byte).
    pub(in crate::sqlite) fn inline_capacity(&self) -> usize {
        let integers = self.parts.iter().filter(|p| p.kind != Kind::Text).count();
        let lengths = self.parts.len() - integers - 1;
        MAX_RAW - HEADER - INT_BYTES * integers - lengths
    }

    pub(in crate::sqlite) fn arm(&self, tuple: &Tuple) -> Arm {
        if tuple.text_bytes() <= self.inline_capacity() {
            Arm::Inline
        } else if tuple.size() <= RETAINED_MAX {
            Arm::Retained
        } else {
            Arm::Lookup
        }
    }

    /// The family's total order on two tuples of this layout: component by
    /// component, integers by value, texts by their bytes, a descending
    /// component reversed.
    pub(in crate::sqlite) fn compare(&self, a: &Tuple, b: &Tuple) -> Ordering {
        self.parts
            .iter()
            .zip(a.0.iter().zip(&b.0))
            .map(|(part, (x, y))| {
                let order = match (x, y) {
                    (Component::Int(x), Component::Int(y)) => x.cmp(y),
                    (Component::Text(x), Component::Text(y)) => x.as_bytes().cmp(y.as_bytes()),
                    // A tuple is built against its layout, so the kinds agree.
                    _ => Ordering::Equal,
                };
                match part.dir {
                    Dir::Asc => order,
                    Dir::Desc => order.reverse(),
                }
            })
            .find(|order| order.is_ne())
            .unwrap_or(Ordering::Equal)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::sqlite) enum Component {
    Int(i64),
    Text(String),
}

/// The original ordering tuple of a row: one component for each part of its
/// layout. Valid by construction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::sqlite) struct Tuple(Vec<Component>);

impl Tuple {
    /// `None` unless `values` has the layout's shape and ranges (see
    /// [`Layout::admits`]).
    pub(in crate::sqlite) fn new(layout: &Layout, values: Vec<Component>) -> Option<Self> {
        layout.admits(&values).then_some(Self(values))
    }

    pub(in crate::sqlite) fn components(&self) -> &[Component] {
        &self.0
    }

    /// The identity: the last component.
    pub(in crate::sqlite) fn identity(&self) -> &str {
        match self.0.last() {
            Some(Component::Text(id)) => id,
            _ => "",
        }
    }

    /// The UTF-8 bytes of every text, the identity included.
    pub(in crate::sqlite) fn text_bytes(&self) -> usize {
        self.0
            .iter()
            .map(|c| match c {
                Component::Text(text) => text.len(),
                Component::Int(_) => 0,
            })
            .sum()
    }

    /// The spec's *S*: the texts' UTF-8 bytes plus 8 for each integer. A goal's
    /// presence integer counts 8 however `updated_at` stands.
    pub(in crate::sqlite) fn size(&self) -> usize {
        self.0
            .iter()
            .map(|c| match c {
                Component::Text(text) => text.len(),
                Component::Int(_) => INT_BYTES,
            })
            .sum()
    }
}
