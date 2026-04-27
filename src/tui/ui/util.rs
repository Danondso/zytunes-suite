//! String/layout helpers shared across the `ui` submodules. Kept tight on
//! purpose: every function here measures or mutates display widths in
//! terminal cells, never touches `App` state, and is independently
//! unit-testable.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Display width of a string in terminal cells. Uses standard Unicode EAW
/// measurement (ambiguous-width chars counted as 1 cell) to match what
/// common terminals actually render, and more importantly what ratatui uses
/// internally when laying out spans and widgets. CJK Wide chars are 2 cells
/// under either measurement, so this still fixes the Japanese-overflow case.
#[inline]
pub(super) fn disp_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

#[inline]
pub(super) fn char_disp_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

pub(super) fn compute_scroll(selected: usize, visible: usize, total: usize) -> usize {
    if total <= visible {
        return 0;
    }
    if selected < visible / 2 {
        0
    } else if selected + visible / 2 >= total {
        total.saturating_sub(visible)
    } else {
        selected.saturating_sub(visible / 2)
    }
}

pub(super) fn truncate(s: &str, max: usize) -> String {
    if disp_width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    // Reserve columns for the ellipsis glyph when there's room. `…` is an
    // ambiguous-width char — 2 cells under CJK-wide measurement — so reserve
    // its actual display width, not a hard-coded 1.
    let ellipsis = "\u{2026}";
    let ellipsis_w = disp_width(ellipsis);
    let (budget, suffix) = if max > ellipsis_w {
        (max - ellipsis_w, ellipsis)
    } else {
        (max, "")
    };
    let mut used = 0usize;
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let w = char_disp_width(ch);
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push_str(suffix);
    out
}

/// Right-pad `s` with spaces so its rendered display width is exactly `width`.
/// If `s` is already wider than `width`, returns it unchanged (the caller is
/// expected to have truncated first).
pub(super) fn pad_right_to_width(s: &str, width: usize) -> String {
    let cur = disp_width(s);
    if cur >= width {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + (width - cur));
    out.push_str(s);
    for _ in 0..(width - cur) {
        out.push(' ');
    }
    out
}

/// Marquee-scroll a string that's wider than `width` display columns.
/// Scrolls through `text   text` seamlessly, advancing one character every
/// 4 frames with an initial pause. Respects grapheme display widths so wide
/// chars don't cause the visible window to drift.
pub(super) fn marquee(text: &str, width: usize, frame: usize) -> String {
    if disp_width(text) <= width || width == 0 {
        return truncate(text, width);
    }
    let chars: Vec<char> = text.chars().collect();
    let gap = 3;
    let cycle_len = chars.len() + gap;
    // Pause at the start for 12 frames before scrolling.
    let scroll_frame = frame.saturating_sub(12);
    let offset = (scroll_frame / 4) % cycle_len;
    let padded: Vec<char> = chars
        .iter()
        .chain(std::iter::repeat_n(&' ', gap))
        .chain(chars.iter())
        .copied()
        .collect();
    // Take chars from `offset` onward until we fill `width` display columns.
    let mut used = 0usize;
    let mut out = String::new();
    for &ch in &padded[offset..] {
        let w = char_disp_width(ch);
        if used + w > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    // Pad with spaces if the last char we couldn't fit left a half-column gap,
    // so the rendered width is stable across frames.
    while used < width {
        out.push(' ');
        used += 1;
    }
    out
}

/// Pad/center a string to exactly `w` chars.
pub(super) fn center_pad(s: &str, w: usize) -> String {
    let clipped = truncate_hard(s, w);
    let len = disp_width(&clipped);
    if len >= w {
        clipped
    } else {
        let left = (w - len) / 2;
        let right = w - len - left;
        format!("{}{}{}", " ".repeat(left), clipped, " ".repeat(right))
    }
}

/// Truncate to at most `max` display columns with no ellipsis. Used when
/// center/pad logic needs a hard cap on width.
pub(super) fn truncate_hard(s: &str, max: usize) -> String {
    let mut used = 0usize;
    let mut out = String::new();
    for ch in s.chars() {
        let cw = char_disp_width(ch);
        if used + cw > max {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn center_pad_cases() {
        assert_eq!(center_pad("hi", 6), "  hi  ");
        assert_eq!(center_pad("abc", 5), " abc ");
        assert_eq!(center_pad("ab", 5), " ab  "); // odd remainder: extra space on right
        assert_eq!(center_pad("toolong", 4), "tool"); // truncated
        assert_eq!(center_pad("exact", 5), "exact");
        assert_eq!(center_pad("", 4), "    ");
    }

    #[test]
    fn marquee_short_text_no_scroll() {
        assert_eq!(marquee("Hi", 10, 0), "Hi");
    }

    #[test]
    fn truncate_respects_cjk_display_width() {
        // Each CJK char is 2 display columns. 14 chars = 28 columns,
        // so at max=28 the string fits untouched; at max=27 it must be
        // truncated so rendered width is <= 27.
        let s = "自分は此処にいるへきてない"; // 13 CJK chars = 26 cols
        assert_eq!(s.width(), 26);
        assert_eq!(truncate(s, 26).width(), 26);
        assert!(truncate(s, 20).width() <= 20);
        assert!(truncate(s, 10).width() <= 10);
        // Ellipsis branch: result still fits budget.
        let out = truncate(s, 10);
        assert!(out.ends_with('\u{2026}'));
    }

    #[test]
    fn truncate_mixed_ascii_hiragana_budget() {
        // Mixes ASCII with Hiragana (Wide). Output width must stay <= max for
        // every budget so panel borders don't shift.
        let s = "(っ◔◡◔)っ ♥ Computer Class";
        for max in [10usize, 15, 19, 22] {
            let out = truncate(s, max);
            assert!(
                disp_width(&out) <= max,
                "max={max} produced width {} for {out:?}",
                disp_width(&out)
            );
        }
    }

    #[test]
    fn center_pad_cjk_exact_width() {
        // "世界" is 4 display cells (2 Wide CJK chars).
        let out = center_pad("世界", 8);
        assert_eq!(disp_width(&out), 8);
    }

    #[test]
    fn truncate_mixed_ascii_cjk() {
        let s = "Hello 世界!"; // H=1,e=1,l=1,l=1,o=1,space=1,世=2,界=2,!=1 = 11
        assert_eq!(s.width(), 11);
        assert_eq!(truncate(s, 11), s);
        // At width 8, must stop before or at 8 columns.
        assert!(truncate(s, 8).width() <= 8);
    }

    #[test]
    fn marquee_cjk_frame_width_is_stable() {
        // A long CJK string that must scroll. The rendered width at every
        // frame should be exactly `width` — wide chars can't straddle the
        // visible window without the function compensating.
        let text = "あいうえおかきくけこ"; // 10 chars × 2 cols = 20 cols
        let width = 7;
        for frame in 0..40 {
            let out = marquee(text, width, frame);
            assert_eq!(
                out.width(),
                width,
                "frame {frame} produced width {} for {out:?}",
                out.width()
            );
        }
    }

    #[test]
    fn marquee_pauses_then_scrolls() {
        let text = "Hello World";
        let width = 5;
        // During pause (first 12 frames), shows start of text.
        assert_eq!(marquee(text, width, 0), "Hello");
        assert_eq!(marquee(text, width, 11), "Hello");
        // After pause, starts scrolling (every 4 frames).
        assert_eq!(marquee(text, width, 16), "ello ");
    }
}
