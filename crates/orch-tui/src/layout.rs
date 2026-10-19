//! Region sizing.
//!
//! The "three consecutive `+`" and "two consecutive `-`" clamp tests exist because this math is fragile.

use crate::state::ViewId;

/// Header(5) + gap(1) + SectionLabel(1) + gap(1) + CommandBar(1).
pub const FIXED_ROWS: u16 = 9;

/// Resize step for `+` / `-`.
pub const SPLIT_STEP: i32 = 3;

/// Minimum terminal size.
///
/// Without a guard the layout produces negative heights below roughly
/// `FIXED_ROWS` plus a usable split. Below this we render a single centred message instead.
pub const MIN_WIDTH: u16 = 40;
pub const MIN_HEIGHT: u16 = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Regions {
    pub content_h: u16,
    pub main_h: u16,
    pub feed_h: u16,
    pub rule_w: u16,
}

pub fn content_height(height: u16) -> u16 {
    height.saturating_sub(FIXED_ROWS).max(4)
}

pub fn rule_width(width: u16) -> u16 {
    width.saturating_sub(2).max(10)
}

/// `maxSplitOffset = contentH - 4 - 3` — the list keeps 3 rows, the detail 4.
pub fn max_split_offset(content_h: u16) -> i32 {
    content_h as i32 - 4 - 3
}

/// The full region computation.
pub fn regions(
    width: u16,
    height: u16,
    view: ViewId,
    list_item_count: usize,
    split_offset: i32,
    maximized: bool,
) -> Regions {
    let content_h = content_height(height);
    let rule_w = rule_width(width);

    // The list never exceeds 50 % of the content height by default.
    let half = (content_h as f64 * 0.5).ceil() as i32;
    let min_list_h = ((list_item_count as i32) + 1).min(half);
    let base_main_h = if view == ViewId::Logs {
        content_h as i32
    } else {
        min_list_h.min(content_h as i32 - 4).max(2)
    };

    let (main_h, feed_h) = if view == ViewId::Logs {
        (content_h as i32, 0)
    } else if maximized {
        (0, (content_h as i32).max(1))
    } else {
        let main = (base_main_h + split_offset)
            .min(content_h as i32 - 4)
            .max(3);
        (main, (content_h as i32 - main).max(1))
    };

    Regions {
        content_h,
        main_h: main_h.max(0) as u16,
        feed_h: feed_h.max(0) as u16,
        rule_w,
    }
}

/// Re-clamp the split offset to ±the maximum, as every content-height change does.
pub fn clamp_split_offset(offset: i32, content_h: u16) -> i32 {
    let max = max_split_offset(content_h).max(0);
    offset.clamp(-max, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_height_floors_at_four() {
        assert_eq!(content_height(24), 15);
        assert_eq!(content_height(9), 4);
        assert_eq!(content_height(0), 4);
    }

    #[test]
    fn logs_view_takes_the_whole_content_area() {
        let r = regions(100, 30, ViewId::Logs, 0, 0, false);
        assert_eq!(r.content_h, 21);
        assert_eq!(r.main_h, 21);
        assert_eq!(r.feed_h, 0);
    }

    #[test]
    fn list_never_exceeds_half_the_content_height() {
        // 100 items would want 101 rows; half of 21 is 11 (ceil).
        let r = regions(100, 30, ViewId::Tasks, 100, 0, false);
        assert_eq!(r.main_h, 11);
        assert_eq!(r.feed_h, 10);
    }

    #[test]
    fn maximizing_gives_every_row_to_the_detail_panel() {
        let r = regions(100, 30, ViewId::Tasks, 5, 0, true);
        assert_eq!(r.main_h, 0);
        assert_eq!(r.feed_h, 21);
    }

    #[test]
    fn split_offset_grows_and_shrinks_the_list() {
        let base = regions(100, 30, ViewId::Tasks, 5, 0, false).main_h;
        let grown = regions(100, 30, ViewId::Tasks, 5, SPLIT_STEP, false).main_h;
        let shrunk = regions(100, 30, ViewId::Tasks, 5, -SPLIT_STEP, false).main_h;
        assert!(grown > base);
        assert!(shrunk < base || shrunk == 3);
    }

    #[test]
    fn three_consecutive_grows_and_two_shrinks_stay_renderable() {
        let content_h = content_height(30);
        let mut offset = 0;
        for _ in 0..3 {
            offset = clamp_split_offset(offset - SPLIT_STEP, content_h);
            let r = regions(100, 30, ViewId::Tasks, 5, offset, false);
            assert!(r.main_h >= 3 && r.feed_h >= 1);
        }
        let mut offset = 0;
        for _ in 0..2 {
            offset = clamp_split_offset(offset + SPLIT_STEP, content_h);
            let r = regions(100, 30, ViewId::Tasks, 5, offset, false);
            assert!(r.main_h >= 3 && r.feed_h >= 1);
        }
    }

    #[test]
    fn split_offset_is_clamped_both_ways() {
        let content_h = content_height(30);
        assert_eq!(max_split_offset(content_h), 14);
        assert_eq!(clamp_split_offset(100, content_h), 14);
        assert_eq!(clamp_split_offset(-100, content_h), -14);
    }

    #[test]
    fn n10_small_height_split_clamp_has_nonnegative_magnitude() {
        for height in [13, 14, 15, 16, 24] {
            let content_h = content_height(height);
            let magnitude = max_split_offset(content_h).max(0);
            for offset in [i32::MIN, -1, 0, 1, i32::MAX] {
                assert_eq!(
                    clamp_split_offset(offset, content_h),
                    offset.clamp(-magnitude, magnitude)
                );
            }
        }
        let regions = regions(40, 14, ViewId::Tasks, 7, 0, false);
        assert_eq!((regions.main_h, regions.feed_h), (3, 2));
    }

    #[test]
    fn tiny_terminals_still_produce_non_negative_regions() {
        for height in 0..20u16 {
            for width in [0u16, 10, 40, 200] {
                let r = regions(width, height, ViewId::Tasks, 3, 0, false);
                assert!(r.main_h >= 3, "{width}x{height}");
                assert!(r.feed_h >= 1, "{width}x{height}");
                assert!(r.rule_w >= 10);
            }
        }
    }
}
