use super::{App, AppMode, DialogMode};
use crate::tui::{
    selection::{TextPoint, TextSelection},
    ui,
};
use ratatui::layout::{Position, Rect};

impl App {
    pub fn clear_text_selection(&mut self) {
        if let AppMode::View(state) = &mut self.app_mode {
            state.text_selection = None;
        }
    }

    fn text_point(&self, column: u16, row: u16, frame: Rect) -> Option<TextPoint> {
        let AppMode::View(state) = &self.app_mode else {
            return None;
        };
        if self.dialog_mode != DialogMode::None || state.rendered_lines.is_empty() {
            return None;
        }
        let area = ui::view_layout_rects(frame, self, state).content;
        if area.is_empty() {
            return None;
        }
        let gutter = if state.message_nav_active {
            crate::tui::viewer::GUTTER_WIDTH
        } else {
            0
        };
        Some(TextPoint {
            line: (state.scroll_offset
                + row.clamp(area.y, area.bottom() - 1).saturating_sub(area.y) as usize)
                .min(state.rendered_lines.len() - 1),
            column: (column
                .clamp(area.x, area.right() - 1)
                .saturating_sub(area.x) as usize)
                .saturating_sub(gutter),
        })
    }

    pub fn begin_text_selection(&mut self, column: u16, row: u16, frame: Rect) -> bool {
        let AppMode::View(state) = &self.app_mode else {
            return false;
        };
        let area = ui::view_layout_rects(frame, self, state).content;
        let point = area
            .contains(Position::new(column, row))
            .then(|| self.text_point(column, row, frame))
            .flatten();
        if let AppMode::View(state) = &mut self.app_mode {
            state.text_selection = point.map(TextSelection::new);
        }
        point.is_some()
    }

    pub fn drag_text_selection(&mut self, column: u16, row: u16, frame: Rect) {
        if self.dialog_mode != DialogMode::None {
            return;
        }
        let AppMode::View(state) = &self.app_mode else {
            return;
        };
        if !state.text_selection.as_ref().is_some_and(|s| s.dragging) {
            return;
        }
        let area = ui::view_layout_rects(frame, self, state).content;
        if row < area.y {
            self.scroll_mouse(-1, area.height as usize);
        } else if row >= area.bottom() {
            self.scroll_mouse(1, area.height as usize);
        }
        if let Some(point) = self.text_point(column, row, frame)
            && let AppMode::View(state) = &mut self.app_mode
            && let Some(selection) = &mut state.text_selection
        {
            selection.update(point);
        }
    }

    pub fn finish_text_selection(
        &mut self,
        column: u16,
        row: u16,
        frame: Rect,
        viewport_height: usize,
    ) {
        let Some(point) = self.text_point(column, row, frame) else {
            return;
        };
        let AppMode::View(state) = &mut self.app_mode else {
            return;
        };
        let Some(selection) = &mut state.text_selection else {
            return;
        };
        if !selection.dragging {
            return;
        }
        selection.update(point);
        selection.dragging = false;
        if !selection.active {
            state.text_selection = None;
            self.handle_view_click(row, frame, viewport_height);
        }
    }

    pub(super) fn copy_text_selection(&mut self) -> bool {
        let AppMode::View(state) = &self.app_mode else {
            return false;
        };
        let Some(selection) = state.text_selection.as_ref().filter(|s| s.active) else {
            return false;
        };
        let text = selection.text(&state.rendered_lines);
        let message = if text.is_empty() {
            "No text selected".to_string()
        } else {
            match crate::tui::export::copy_to_system_clipboard(&text) {
                Ok(crate::tui::export::ClipboardDestination::System) => {
                    "Selection copied to clipboard".to_string()
                }
                Ok(crate::tui::export::ClipboardDestination::Terminal) => {
                    "Selection sent to terminal clipboard".to_string()
                }
                Err(error) => error,
            }
        };
        self.status_message = Some((message, std::time::Instant::now()));
        true
    }
}
