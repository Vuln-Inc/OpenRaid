//! Mouse selection uses a frozen rendered pane, never neighboring panes or borders.
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::Modifier,
    text::Span,
};

#[derive(Default)]
pub(super) struct Selection {
    drag: Option<Drag>,
}

struct Drag {
    buffer: Buffer,
    area: Rect,
    anchor: Position,
    end: Position,
    moved: bool,
}

impl Selection {
    pub fn start(&mut self, buffer: &Buffer, area: Rect, point: Position) {
        self.cancel();
        if area.contains(point) && area.width > 0 && area.height > 0 {
            self.drag = Some(Drag {
                buffer: buffer.clone(),
                area,
                anchor: point,
                end: point,
                moved: false,
            });
        }
    }

    pub fn update(&mut self, point: Position) {
        if let Some(drag) = &mut self.drag {
            drag.end = Position::new(
                point.x.clamp(drag.area.x, drag.area.right() - 1),
                point.y.clamp(drag.area.y, drag.area.bottom() - 1),
            );
            drag.moved |= drag.end != drag.anchor;
        }
    }

    pub fn finish(&mut self, point: Position) -> Option<String> {
        self.update(point);
        let drag = self.drag.take()?;
        drag.moved.then(|| drag.text())
    }

    pub fn cancel(&mut self) {
        self.drag = None;
    }

    pub fn render(&self, buffer: &mut Buffer) {
        let Some(drag) = &self.drag else {
            return;
        };
        if buffer.area != drag.buffer.area {
            return;
        }
        let palette = crate::theme::current_palette();
        for y in drag.area.y..drag.area.bottom() {
            for x in drag.area.x..drag.area.right() {
                buffer[(x, y)] = drag.buffer[(x, y)].clone();
                if drag.moved && drag.contains(x, y) {
                    buffer[(x, y)]
                        .set_bg(palette.selection)
                        .set_fg(palette.selection_text)
                        .set_style(Modifier::BOLD);
                }
            }
        }
    }
}

impl Drag {
    fn ordered(&self) -> (Position, Position) {
        if (self.anchor.y, self.anchor.x) <= (self.end.y, self.end.x) {
            (self.anchor, self.end)
        } else {
            (self.end, self.anchor)
        }
    }

    fn contains(&self, x: u16, y: u16) -> bool {
        let (start, end) = self.ordered();
        (y, x) >= (start.y, start.x) && (y, x) <= (end.y, end.x)
    }

    fn text(&self) -> String {
        let (start, end) = self.ordered();
        let mut lines = Vec::new();
        for y in start.y..=end.y {
            let mut line = String::new();
            let mut x = self.area.x;
            while x < self.area.right() {
                let symbol = self.buffer[(x, y)].symbol();
                let width = Span::raw(symbol).width().max(1) as u16;
                // A wide grapheme is selected even when the pointer touches its trailing cell.
                if (x..x.saturating_add(width).min(self.area.right()))
                    .any(|column| self.contains(column, y))
                {
                    line.push_str(symbol);
                }
                x = x.saturating_add(width);
            }
            lines.push(line.trim_end().to_owned());
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer() -> Buffer {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 12, 4));
        buffer.set_string(1, 1, "hello", ratatui::style::Style::default());
        buffer.set_string(1, 2, "world", ratatui::style::Style::default());
        buffer
    }

    #[test]
    fn reverse_drag_matches_forward_and_omits_padding() {
        let mut selection = Selection::default();
        let area = Rect::new(1, 1, 8, 2);
        selection.start(&buffer(), area, Position::new(2, 1));
        assert_eq!(
            selection.finish(Position::new(3, 2)).as_deref(),
            Some("ello\nwor")
        );
        selection.start(&buffer(), area, Position::new(3, 2));
        assert_eq!(
            selection.finish(Position::new(2, 1)).as_deref(),
            Some("ello\nwor")
        );
    }

    #[test]
    fn click_does_not_copy_and_outside_drag_is_clamped() {
        let mut selection = Selection::default();
        let area = Rect::new(1, 1, 8, 2);
        selection.start(&buffer(), area, Position::new(1, 1));
        assert!(selection.finish(Position::new(1, 1)).is_none());
        selection.start(&buffer(), area, Position::new(1, 1));
        assert_eq!(
            selection.finish(Position::new(11, 3)).as_deref(),
            Some("hello\nworld")
        );
    }

    #[test]
    fn wide_and_combining_graphemes_copy_once() {
        let mut buffer = buffer();
        buffer.set_string(1, 1, "界e\u{301}z", ratatui::style::Style::default());
        let mut selection = Selection::default();
        selection.start(&buffer, Rect::new(1, 1, 8, 2), Position::new(2, 1));
        assert_eq!(
            selection.finish(Position::new(3, 1)).as_deref(),
            Some("界e\u{301}")
        );
    }

    #[test]
    fn drag_freezes_pane_and_cancel_removes_highlight() {
        let original = buffer();
        let mut selection = Selection::default();
        selection.start(&original, Rect::new(1, 1, 8, 2), Position::new(1, 1));
        selection.update(Position::new(4, 1));
        let mut changed = original.clone();
        changed.set_string(1, 1, "other", ratatui::style::Style::default());
        selection.render(&mut changed);
        assert_eq!(changed[(1, 1)].symbol(), "h");
        selection.cancel();
        assert!(selection.finish(Position::new(4, 1)).is_none());
    }
}
