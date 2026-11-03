//! Selection coordinates refer to rendered text, excluding the navigation gutter.
use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::viewer::RenderedLine;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPoint {
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Debug)]
pub struct TextSelection {
    pub anchor: TextPoint,
    pub focus: TextPoint,
    pub dragging: bool,
    pub active: bool,
}

impl TextSelection {
    pub fn new(point: TextPoint) -> Self {
        Self {
            anchor: point,
            focus: point,
            dragging: true,
            active: false,
        }
    }

    pub fn update(&mut self, point: TextPoint) {
        self.active |= point != self.anchor;
        self.focus = point;
    }

    pub fn columns(&self, line: usize) -> Range<usize> {
        if !self.active {
            return 0..0;
        }
        let (start, end) = if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        };
        if line < start.line || line > end.line {
            return 0..0;
        }
        let start_column = if line == start.line { start.column } else { 0 };
        let end_column = if line == end.line {
            end.column.saturating_add(1)
        } else {
            usize::MAX
        };
        start_column..end_column
    }

    pub fn text(&self, lines: &[RenderedLine]) -> String {
        if !self.active {
            return String::new();
        }
        let start = self.anchor.line.min(self.focus.line);
        let end = self.anchor.line.max(self.focus.line);
        lines
            .iter()
            .enumerate()
            .skip(start)
            .take(end - start + 1)
            .map(|(i, line)| {
                let text: String = line.spans.iter().map(|(text, _)| text.as_str()).collect();
                selected_graphemes(&text, self.columns(i))
                    .map(|(_, text)| text)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Include whole graphemes even when an endpoint hits a wide cell's second column.
pub fn selected_graphemes(
    text: &str,
    range: Range<usize>,
) -> impl Iterator<Item = (Range<usize>, &str)> {
    let mut column = 0;
    text.graphemes(true).filter_map(move |text| {
        let columns = column..column + text.width();
        column = columns.end;
        (columns.start < range.end && columns.end > range.start).then_some((columns, text))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str) -> RenderedLine {
        RenderedLine::new(vec![(text.into(), Default::default())])
    }

    #[test]
    fn forward_and_reverse_multiline_selection() {
        let lines = vec![line("first line"), line(""), line("last line")];
        let a = TextPoint { line: 0, column: 6 };
        let b = TextPoint { line: 2, column: 3 };
        for (anchor, focus) in [(a, b), (b, a)] {
            let mut selection = TextSelection::new(anchor);
            assert_eq!(selection.text(&lines), "");
            selection.update(focus);
            assert_eq!(selection.text(&lines), "line\n\nlast");
        }
    }

    #[test]
    fn selection_preserves_wide_combining_and_emoji_graphemes() {
        let text = "a界e\u{301}👩‍💻z";
        assert_eq!(
            selected_graphemes(text, 2..3)
                .map(|(_, s)| s)
                .collect::<String>(),
            "界"
        );
        assert_eq!(
            selected_graphemes(text, 3..4)
                .map(|(_, s)| s)
                .collect::<String>(),
            "e\u{301}"
        );
        assert_eq!(
            selected_graphemes(text, 5..6)
                .map(|(_, s)| s)
                .collect::<String>(),
            "👩‍💻"
        );
    }
}
