//! Whitespace-preserving display layout for the multiline composer.
//!
//! Unlike prose wrapping, editor layout must retain every source character and
//! expose source offsets so cursor and motion math match what is painted.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualLine {
    pub text: String,
    pub logical_row: usize,
    /// Inclusive source character offset within `logical_row`.
    pub start: usize,
    /// Exclusive source character offset within `logical_row`.
    pub end: usize,
}

pub fn layout(lines: &[String], width: usize) -> Vec<VisualLine> {
    let width = width.max(1);
    let mut out = Vec::new();
    for (logical_row, line) in lines.iter().enumerate() {
        let chars = line.chars().collect::<Vec<_>>();
        if chars.is_empty() {
            out.push(VisualLine {
                text: String::new(),
                logical_row,
                start: 0,
                end: 0,
            });
            continue;
        }
        let mut start = 0;
        let mut used = 0;
        for (index, ch) in chars.iter().enumerate() {
            let char_width = UnicodeWidthChar::width(*ch).unwrap_or(0);
            if used + char_width > width && used > 0 {
                out.push(VisualLine {
                    text: chars[start..index].iter().collect(),
                    logical_row,
                    start,
                    end: index,
                });
                start = index;
                used = 0;
            }
            used += char_width;
        }
        out.push(VisualLine {
            text: chars[start..].iter().collect(),
            logical_row,
            start,
            end: chars.len(),
        });
    }
    if out.is_empty() {
        out.push(VisualLine {
            text: String::new(),
            logical_row: 0,
            start: 0,
            end: 0,
        });
    }
    out
}

/// Convert a logical insertion position to a visual row and terminal cell.
pub fn cursor(visual: &[VisualLine], logical_row: usize, logical_col: usize) -> (usize, usize) {
    let mut fallback = None;
    for (index, line) in visual.iter().enumerate() {
        if line.logical_row != logical_row {
            continue;
        }
        fallback = Some(index);
        if logical_col >= line.start && logical_col < line.end {
            let cell = UnicodeWidthStr::width(
                line.text
                    .chars()
                    .take(logical_col - line.start)
                    .collect::<String>()
                    .as_str(),
            );
            return (index, cell);
        }
    }
    let index = fallback.unwrap_or(0);
    let line = &visual[index];
    (index, UnicodeWidthStr::width(line.text.as_str()))
}

/// Logical character offset nearest to `cell` on a visual line, clamped to its end.
pub fn offset_at_cell(line: &VisualLine, cell: usize) -> usize {
    let mut used = 0;
    for (offset, ch) in line.text.chars().enumerate() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + width > cell {
            return line.start + offset;
        }
        used += width;
    }
    line.end
}

#[cfg(test)]
mod tests {
    use super::{cursor, layout, offset_at_cell};

    #[test]
    fn preserves_spaces_and_source_offsets_across_wraps() {
        let rows = layout(&["hello world".into()], 5);
        assert_eq!(
            rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>(),
            ["hello", " worl", "d"]
        );
        assert_eq!((rows[1].start, rows[1].end), (5, 10));
        assert_eq!(cursor(&rows, 0, 6), (1, 1));
    }

    #[test]
    fn cursor_and_motion_use_terminal_cells_for_wide_text() {
        let rows = layout(&["a界bc".into()], 3);
        assert_eq!(
            rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>(),
            ["a界", "bc"]
        );
        assert_eq!(cursor(&rows, 0, 2), (1, 0));
        assert_eq!(offset_at_cell(&rows[1], 1), 3);
    }
}
