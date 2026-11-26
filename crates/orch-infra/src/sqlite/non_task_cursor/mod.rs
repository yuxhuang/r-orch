//! The inline cursor of the eight families other than Task (spec "Cursors of the
//! eight families other than Task", ADR 0024, plan "Non-Task cursors" step 2).
//!
//! Inactive: nothing here is wired into a leaf or into `Storage`. It holds the
//! part of the contract that needs no state of the adapter's: the typed ordering
//! of each family and sort ([`Layout`], [`Tuple`]), the binding of a token to its
//! project, generation, family and filter, and the inline token with its
//! expiry. Steps 3 (key-taking leaves) and 4 (the table of retained entries and
//! entries by lookup) build on the interface below.
//!
//! The interface that step 4 relies on, and that a change here must keep:
//!
//! - [`Layout::of`] says which (filter, sort) pairs a family offers and in what
//!   order its rows stand: the components of the ordering tuple, each an integer
//!   of a named kind or a text, ascending or descending, ending in the identity.
//!   [`Layout::compare`] is the one definition of "after the anchor" for the
//!   leaves and for the comparison of a lookup entry with the row it finds.
//! - [`Tuple`] is the original ordering tuple, built from the row's original
//!   values (the goal's absent `updated_at` is presence 1 and an empty text).
//!   [`Tuple::size`] is the spec's *S* in UTF-8 bytes, [`Layout::arm`] the arm that
//!   *S* selects, and [`Layout::inline_capacity`] the capacity table. A tuple
//!   does not remember its layout: [`Layout::admits`] says whether values have
//!   the shape and ranges of one, and [`NonTaskCursors::issue_inline`] refuses a
//!   tuple that its request's layout does not admit.
//! - [`NonTaskCursors::binding`] is the 16-byte digest that every kind of token
//!   carries; [`NonTaskCursors::sort_tag`] is the byte that says the sort. A
//!   held token (retained or by lookup) is the same header, with
//!   `RETAINED` or `LOOKUP` as its version (2 and 3, reserved), followed by the
//!   adapter's nonce and a handle. This module refuses those versions until step 4
//!   reads them. [`NonTaskCursors::resume_inline`] refuses a token of more than 512
//!   characters before decoding it, because a deserialized `Cursor` is not bounded
//!   by its constructor; the page that calls it still validates the request.
//! - [`INLINE_TTL_MS`] and [`INLINE_SKEW_MS`] are the owner's inline clock rule.
mod layout;
mod token;

#[cfg(test)]
mod tests;

#[allow(
    unused_imports,
    reason = "the frozen interface of step 4, which has no caller until the table lands"
)]
pub(in crate::sqlite) use layout::{Arm, Component, Layout, Tuple};
#[allow(
    unused_imports,
    reason = "the frozen interface of step 4, which has no caller until the table lands"
)]
pub(in crate::sqlite) use token::NonTaskCursors;
