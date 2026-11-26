//! The cursors of the eight families other than Task (spec "Cursors of the
//! eight families other than Task", ADR 0024, plan "Non-Task cursors" steps 2 and 4).
//!
//! Inactive: nothing here is wired into a leaf or into `Storage`. The typed
//! ordering and inline format remain the step 2 interface; step 4 adds bounded
//! retained/lookup entries, monotonic age and an unfiltered identity scan.
//! `issue` selects the arm; `resume` verifies the original tuple. The caller
//! supplies the resuming connection and a same-view validated tuple hydrator.
//! Step 5 owns its wiring into the native leaves and the public page path.
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
//!   held token uses version 2 (retained) or 3 (lookup), sort, binding (16),
//!   nonce (8), handle (8): 34 raw bytes, with no expiry field, as ADR 0024
//!   specifies. The inline header remains 26 bytes. [`NonTaskCursors::resume`]
//!   and [`NonTaskCursors::resume_inline`] refuse a token of more than 512
//!   characters before decoding it, because a deserialized `Cursor` is not bounded
//!   by its constructor; the page that calls them still validates the request.
//! - [`INLINE_TTL_MS`] and [`INLINE_SKEW_MS`] are the owner's inline clock rule.
mod held;
mod layout;
mod token;

#[cfg(test)]
mod tests;

#[allow(
    unused_imports,
    reason = "the frozen interface stays private until step 5 wires public pages"
)]
pub(in crate::sqlite) use layout::{Arm, Component, Layout, Tuple};
#[allow(
    unused_imports,
    reason = "the frozen interface stays private until step 5 wires public pages"
)]
pub(in crate::sqlite) use token::NonTaskCursors;
