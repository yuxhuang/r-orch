//! liquidjs with `strictVariables: false` renders any missing variable or
//! property, at any depth, as empty. `liquid` instead fails the render. This
//! module wraps the globals in a tree whose lookups never fail: a missing key,
//! a missing array index, or a property of a missing value all yield a nil
//! that can itself be indexed (and stays nil).
//!
//! Residual divergence, both inside `liquid`'s own lookup where this layer
//! cannot intervene: (1) a *scalar* or *array* asked for an unknown named
//! property (`task.title.foo`, `agents.foo`); (2) any unknown property of a
//! `{% for %}` loop variable, because loop variables are copied out as plain
//! values. Documented variables never hit either: every key is always present.

// `liquid`'s `ValueView`/`ObjectView` traits name `KStringCow` in their
// signatures, and `kstring` 2.0.3+ deprecates the whole crate. Implementing
// the traits means spelling the type, so the warning cannot be fixed here;
// it goes away when `liquid` moves off `kstring`.
#![allow(deprecated)]

use std::sync::LazyLock;

use liquid::model::{
    ArrayView, DisplayCow, KStringCow, ObjectView, ScalarCow, State, Value, ValueView,
};

static NIL_VALUE: Value = Value::Nil;
static NIL: Node = Node::Nil;
static NIL_OBJECT: LazyLock<Object> = LazyLock::new(|| Object {
    entries: Vec::new(),
    plain: Value::Nil,
    size: Value::Nil,
});

#[derive(Debug)]
pub(super) enum Node {
    Nil,
    Scalar(Value),
    Array(Array),
    Object(Object),
}

#[derive(Debug)]
pub(super) struct Array {
    items: Vec<Node>,
    plain: Value,
}

#[derive(Debug)]
pub(super) struct Object {
    entries: Vec<(String, Node)>,
    plain: Value,
    /// What `hash.size` evaluates to, kept here so `get` can lend it out.
    size: Value,
}

impl Node {
    pub(super) fn from_value(value: &Value) -> Node {
        match value {
            Value::Nil => Node::Nil,
            Value::Array(items) => Node::Array(Array {
                items: items.iter().map(Node::from_value).collect(),
                plain: value.clone(),
            }),
            Value::Object(map) => Node::Object(Object::from_entries(
                map.iter()
                    .map(|(k, v)| (k.to_string(), Node::from_value(v)))
                    .collect(),
                value.clone(),
            )),
            other => Node::Scalar(other.clone()),
        }
    }

    fn base(&self) -> &Value {
        match self {
            Node::Nil => &NIL_VALUE,
            Node::Scalar(v) => v,
            Node::Array(a) => &a.plain,
            Node::Object(o) => &o.plain,
        }
    }
}

impl Object {
    fn from_entries(entries: Vec<(String, Node)>, plain: Value) -> Object {
        let size = Value::scalar(entries.len() as i64);
        Object {
            entries,
            plain,
            size,
        }
    }

    /// The root scope: an object whose every lookup succeeds.
    pub(super) fn root(globals: &liquid::Object) -> Object {
        let plain = Value::Object(globals.clone());
        let entries = globals
            .iter()
            .map(|(k, v)| (k.to_string(), Node::from_value(v)))
            .collect();
        Object::from_entries(entries, plain)
    }
}

impl ValueView for Node {
    fn as_debug(&self) -> &dyn std::fmt::Debug {
        self
    }
    fn render(&self) -> DisplayCow<'_> {
        self.base().render()
    }
    fn source(&self) -> DisplayCow<'_> {
        self.base().source()
    }
    fn type_name(&self) -> &'static str {
        self.base().type_name()
    }
    fn query_state(&self, state: State) -> bool {
        self.base().query_state(state)
    }
    fn to_kstr(&self) -> KStringCow<'_> {
        self.base().to_kstr()
    }
    fn to_value(&self) -> Value {
        self.base().clone()
    }
    fn as_scalar(&self) -> Option<ScalarCow<'_>> {
        self.base().as_scalar()
    }
    fn as_array(&self) -> Option<&dyn ArrayView> {
        match self {
            Node::Array(a) => Some(a),
            _ => None,
        }
    }
    fn as_object(&self) -> Option<&dyn ObjectView> {
        match self {
            Node::Object(o) => Some(o),
            Node::Nil => Some(&*NIL_OBJECT),
            _ => None,
        }
    }
    fn as_state(&self) -> Option<State> {
        self.base().as_state()
    }
    fn is_nil(&self) -> bool {
        self.base().is_nil()
    }
}

macro_rules! delegate_to_plain {
    ($ty:ty, $($extra:tt)*) => {
        impl ValueView for $ty {
            fn as_debug(&self) -> &dyn std::fmt::Debug {
                self
            }
            fn render(&self) -> DisplayCow<'_> {
                self.plain.render()
            }
            fn source(&self) -> DisplayCow<'_> {
                self.plain.source()
            }
            fn type_name(&self) -> &'static str {
                self.plain.type_name()
            }
            fn query_state(&self, state: State) -> bool {
                self.plain.query_state(state)
            }
            fn to_kstr(&self) -> KStringCow<'_> {
                self.plain.to_kstr()
            }
            fn to_value(&self) -> Value {
                self.plain.clone()
            }
            fn as_scalar(&self) -> Option<ScalarCow<'_>> {
                None
            }
            fn is_nil(&self) -> bool {
                self.plain.is_nil()
            }
            $($extra)*
        }
    };
}
delegate_to_plain!(
    Array,
    fn as_array(&self) -> Option<&dyn ArrayView> {
        Some(self)
    }
);
delegate_to_plain!(
    Object,
    fn as_object(&self) -> Option<&dyn ObjectView> {
        Some(self)
    }
);

impl ArrayView for Array {
    fn as_value(&self) -> &dyn ValueView {
        self
    }
    fn size(&self) -> i64 {
        self.items.len() as i64
    }
    fn values<'k>(&'k self) -> Box<dyn Iterator<Item = &'k dyn ValueView> + 'k> {
        Box::new(self.items.iter().map(|n| n as &dyn ValueView))
    }
    fn contains_key(&self, index: i64) -> bool {
        self.position(index).is_some()
    }
    fn get(&self, index: i64) -> Option<&dyn ValueView> {
        Some(self.position(index).map_or(&NIL, |i| &self.items[i]) as &dyn ValueView)
    }
}

impl Array {
    fn position(&self, index: i64) -> Option<usize> {
        let len = self.items.len() as i64;
        let i = if index < 0 { len + index } else { index };
        (0..len).contains(&i).then_some(i as usize)
    }
}

impl ObjectView for Object {
    fn as_value(&self) -> &dyn ValueView {
        self
    }
    fn size(&self) -> i64 {
        self.entries.len() as i64
    }
    fn keys<'k>(&'k self) -> Box<dyn Iterator<Item = KStringCow<'k>> + 'k> {
        Box::new(self.entries.iter().map(|(k, _)| KStringCow::from_ref(k)))
    }
    fn values<'k>(&'k self) -> Box<dyn Iterator<Item = &'k dyn ValueView> + 'k> {
        Box::new(self.entries.iter().map(|(_, n)| n as &dyn ValueView))
    }
    fn iter<'k>(&'k self) -> Box<dyn Iterator<Item = (KStringCow<'k>, &'k dyn ValueView)> + 'k> {
        Box::new(
            self.entries
                .iter()
                .map(|(k, n)| (KStringCow::from_ref(k.as_str()), n as &dyn ValueView)),
        )
    }
    /// Always true: liquid's scope frames consult this before `get`, and a
    /// missing variable must reach `get` to become nil rather than an error.
    fn contains_key(&self, _index: &str) -> bool {
        true
    }
    fn get<'s>(&'s self, index: &str) -> Option<&'s dyn ValueView> {
        match self.entries.iter().find(|(k, _)| k == index) {
            Some((_, node)) => Some(node),
            None if index == "size" => Some(&self.size),
            None => Some(&NIL),
        }
    }
}
