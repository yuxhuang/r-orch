//! Text editing primitives shared by the command bar and the wizards.

pub mod cursor;
pub mod input;

pub use cursor::{Cursor, display_width, grapheme_length, grapheme_segments, text_display_width};
pub use input::TextInput;
