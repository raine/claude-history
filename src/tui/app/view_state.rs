use super::{App, AppMode, DialogMode, ViewFamily, ViewSearchMode, ViewState};
use crate::tui::ui;
use crate::tui::viewer::{MessageRange, RenderOptions, RenderedLine, ToolOutputId};
use ratatui::prelude::*;
use std::collections::BTreeSet;
use std::path::PathBuf;

impl App {
    pub fn enter_view_mode(&mut self, content_width: usize) {
        let Some(selected) = self.selected else {
            return;
        };
        let Some(&conv_idx) = self.filtered.get(selected) else {
            return;
        };
        let path = self.conversations[conv_idx].path.clone();
        if let Some(state) = self.view_of(path, content_width) {
            self.view_stack.clear();
            self.app_mode = AppMode::View(state);
            self.refresh_view_family();
        }
    }

    /// Leaves the viewer for the view it was opened from, else the list.
    pub(super) fn leave_view(&mut self) {
        match self.view_stack.pop() {
            Some(state) => {
                self.app_mode = AppMode::View(state);
                self.refresh_view_family();
            }
            None => self.exit_view_mode(),
        }
    }

    /// Finds the parent and children of the conversation in the viewer,
    /// materializing their cards.
    pub(super) fn refresh_view_family(&mut self) {
        let AppMode::View(state) = &self.app_mode else {
            self.view_family = ViewFamily::default();
            return;
        };
        let path = state.conversation_path.clone();
        let Some(row) = self.row_of_path(&path) else {
            self.view_family = ViewFamily::default();
            return;
        };
        let conversation = self.conversations[row].clone();
        let conversation = &conversation;
        let docs = self.snapshot.as_ref().map(|snapshot| {
            (
                snapshot.parent_of(conversation),
                snapshot.children_of(conversation),
            )
        });
        self.view_family = match docs {
            Some((parent, children)) => {
                let parent = parent.filter(|&doc| !self.is_removed(doc));
                let children: Vec<u32> = children
                    .into_iter()
                    .filter(|&doc| !self.is_removed(doc))
                    .collect();
                ViewFamily {
                    parent: parent.map(|doc| self.card_row(doc)),
                    children: children.into_iter().map(|doc| self.card_row(doc)).collect(),
                }
            }
            None => {
                let parent = self
                    .conversations
                    .iter()
                    .position(|candidate| crate::history::is_parent_of(candidate, conversation));
                let mut children: Vec<usize> = (0..self.conversations.len())
                    .filter(|&child| {
                        crate::history::is_parent_of(conversation, &self.conversations[child])
                    })
                    .collect();
                children.sort_by_key(|&child| self.conversations[child].timestamp);
                ViewFamily { parent, children }
            }
        };
    }

    /// Opens the parent of the conversation in the viewer, coming back here
    /// when it is left.
    pub(super) fn open_view_parent(&mut self) {
        match self.view_family.parent {
            Some(row) => self.open_related(row),
            None => {
                let has_parent = self
                    .viewed_conversation()
                    .is_some_and(|conversation| conversation.parent.is_some());
                self.set_status_message(if has_parent {
                    "Parent session not found".to_string()
                } else {
                    "This session has no parent".to_string()
                });
            }
        }
    }

    /// Lists the subagents and forks of the conversation in the viewer.
    pub(super) fn open_children_dialog(&mut self) {
        if self.view_family.children.is_empty() {
            self.set_status_message("No subagents or forks in this session".to_string());
        } else {
            self.dialog_mode = DialogMode::Children { selected: 0 };
        }
    }

    pub(super) fn handle_children_key(&mut self, code: crossterm::event::KeyCode) {
        use crossterm::event::KeyCode;
        let DialogMode::Children { selected } = self.dialog_mode else {
            return;
        };
        let count = self.view_family.children.len();
        match code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.dialog_mode = DialogMode::Children {
                    selected: (selected + 1).min(count.saturating_sub(1)),
                };
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.dialog_mode = DialogMode::Children {
                    selected: selected.saturating_sub(1),
                };
            }
            KeyCode::Enter => {
                self.dialog_mode = DialogMode::None;
                if let Some(&row) = self.view_family.children.get(selected) {
                    self.open_related(row);
                }
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('s') => {
                self.dialog_mode = DialogMode::None;
            }
            _ => {}
        }
    }

    /// Opens conversation `row` in the viewer, keeping the current view to
    /// return to.
    fn open_related(&mut self, row: usize) {
        let AppMode::View(current) = &self.app_mode else {
            return;
        };
        let width = current.content_width;
        let path = self.conversations[row].path.clone();
        if let Some(state) = self.view_of(path, width) {
            let AppMode::View(previous) =
                std::mem::replace(&mut self.app_mode, AppMode::View(state))
            else {
                unreachable!("the viewer was open");
            };
            self.view_stack.push(previous);
            self.refresh_view_family();
        }
    }

    /// The conversation in the viewer, when it has a row.
    pub(crate) fn viewed_conversation(&self) -> Option<&crate::history::Conversation> {
        let AppMode::View(state) = &self.app_mode else {
            return None;
        };
        self.conversations
            .iter()
            .find(|conversation| conversation.path == state.conversation_path)
    }

    pub fn view_family(&self) -> &ViewFamily {
        &self.view_family
    }

    /// A fresh view of the conversation at `path`, or `None` with the
    /// reason in the status line when it cannot be opened.
    fn view_of(&mut self, path: PathBuf, content_width: usize) -> Option<ViewState> {
        use crate::tui::viewer::{open_conversation, render_conversation};

        let options = RenderOptions {
            tool_display: self.tool_display,
            show_thinking: self.show_thinking,
            show_timing: self.show_timing,
            content_width,
            expanded_tool_outputs: BTreeSet::new(),
        };

        match open_conversation(&path) {
            Ok(transcript) => {
                let rendered = render_conversation(&transcript, &options);
                let total_lines = rendered.lines.len();
                let first_msg = if rendered.messages.is_empty() {
                    None
                } else {
                    Some(0)
                };
                Some(ViewState {
                    conversation_path: path,
                    transcript: Some(transcript),
                    scroll_offset: 0,
                    rendered_lines: rendered.lines,
                    total_lines,
                    tool_display: self.tool_display,
                    show_thinking: self.show_thinking,
                    show_timing: self.show_timing,
                    content_width,
                    search_mode: ViewSearchMode::Off,
                    search_query: String::new(),
                    search_matches: Vec::new(),
                    current_match: 0,
                    message_ranges: rendered.messages,
                    focused_message: first_msg,
                    message_nav_active: false,
                    expanded_tool_outputs: BTreeSet::new(),
                    hovered_tool_output: None,
                    text_selection: None,
                })
            }
            Err(e) => {
                self.status_message =
                    Some((format!("Failed to open: {}", e), std::time::Instant::now()));
                None
            }
        }
    }

    pub fn exit_view_mode(&mut self) {
        self.app_mode = AppMode::List;
        self.view_stack.clear();
        self.view_family = ViewFamily::default();
    }

    pub(super) fn start_view_search(&mut self) {
        if let AppMode::View(ref mut state) = self.app_mode {
            state.search_mode = ViewSearchMode::Typing;
            state.search_query.clear();
            state.search_matches.clear();
            state.current_match = 0;
        }
    }

    pub(super) fn clear_view_search(&mut self) {
        if let AppMode::View(ref mut state) = self.app_mode {
            state.search_mode = ViewSearchMode::Off;
            state.search_query.clear();
            state.search_matches.clear();
        }
    }

    pub(super) fn clear_view_search_query(&mut self) -> bool {
        if let AppMode::View(ref mut state) = self.app_mode
            && !state.search_query.is_empty()
        {
            state.search_query.clear();
            self.update_search_results();
            return true;
        }
        false
    }

    pub(super) fn delete_view_search_word_backwards(&mut self) {
        if let AppMode::View(ref mut state) = self.app_mode {
            let trimmed = state.search_query.trim_end();
            if let Some(last_space) = trimmed.rfind(|c: char| c.is_whitespace()) {
                state.search_query.truncate(last_space + 1);
            } else {
                state.search_query.clear();
            }
        }
        self.update_search_results();
    }

    pub(super) fn push_view_search_char(&mut self, c: char) {
        if let AppMode::View(ref mut state) = self.app_mode {
            state.search_query.push(c);
        }
        self.update_search_results();
    }

    pub(super) fn backspace_view_search(&mut self) {
        if let AppMode::View(ref mut state) = self.app_mode {
            state.search_query.pop();
        }
        self.update_search_results();
    }

    pub(super) fn commit_view_search(&mut self) {
        let mut no_matches = None;
        if let AppMode::View(ref mut state) = self.app_mode {
            if !state.search_matches.is_empty() {
                state.search_mode = ViewSearchMode::Active;
            } else {
                state.search_mode = ViewSearchMode::Off;
                if !state.search_query.trim().is_empty() {
                    no_matches = Some(format!("No matches for /{}", state.search_query));
                }
            }
        }
        if let Some(message) = no_matches {
            self.set_status_message(message);
        }
    }

    pub(super) fn update_search_results(&mut self) {
        if let AppMode::View(ref mut state) = self.app_mode {
            let query = ui::ViewSearchQuery::parse(&state.search_query);
            if query.is_empty() {
                state.search_matches.clear();
                return;
            }

            state.search_matches = state
                .rendered_lines
                .iter()
                .enumerate()
                .filter(|(_, line)| line_matches_query(line, &query))
                .map(|(i, _)| i)
                .collect();

            if !state.search_matches.is_empty() {
                state.current_match = 0;
                let match_line = state.search_matches[0];
                state.scroll_offset = match_line;
                Self::focus_message_at_line(state, match_line);
            }
        }
    }

    pub(super) fn next_search_match(&mut self, viewport_height: usize) {
        if let AppMode::View(ref mut state) = self.app_mode {
            if state.search_matches.is_empty() {
                return;
            }
            state.current_match = (state.current_match + 1) % state.search_matches.len();
            let match_line = state.search_matches[state.current_match];
            if match_line < state.scroll_offset
                || match_line >= state.scroll_offset + viewport_height
            {
                state.scroll_offset = match_line;
            }
            Self::focus_message_at_line(state, match_line);
        }
    }

    pub(super) fn prev_search_match(&mut self, viewport_height: usize) {
        if let AppMode::View(ref mut state) = self.app_mode {
            if state.search_matches.is_empty() {
                return;
            }
            state.current_match = if state.current_match == 0 {
                state.search_matches.len() - 1
            } else {
                state.current_match - 1
            };
            let match_line = state.search_matches[state.current_match];
            if match_line < state.scroll_offset
                || match_line >= state.scroll_offset + viewport_height
            {
                state.scroll_offset = match_line;
            }
            Self::focus_message_at_line(state, match_line);
        }
    }

    pub(super) fn toggle_view_tools(&mut self, viewport_height: usize) {
        if let AppMode::View(ref mut state) = self.app_mode {
            state.tool_display = state.tool_display.next();
            self.tool_display = state.tool_display;
            self.re_render_view(viewport_height);
        }
    }

    pub(super) fn toggle_view_thinking(&mut self, viewport_height: usize) {
        if let AppMode::View(ref mut state) = self.app_mode {
            state.show_thinking = !state.show_thinking;
            self.show_thinking = state.show_thinking;
            self.re_render_view(viewport_height);
        }
    }

    pub(super) fn toggle_view_timing(&mut self, viewport_height: usize) {
        if let AppMode::View(ref mut state) = self.app_mode {
            state.show_timing = !state.show_timing;
            self.show_timing = state.show_timing;
            self.re_render_view(viewport_height);
        }
    }

    pub(super) fn re_render_view(&mut self, viewport_height: usize) {
        use crate::tui::viewer::{open_conversation, render_conversation};

        if let AppMode::View(ref mut state) = self.app_mode {
            let options = RenderOptions {
                tool_display: state.tool_display,
                show_thinking: state.show_thinking,
                show_timing: state.show_timing,
                content_width: state.content_width,
                expanded_tool_outputs: state.expanded_tool_outputs.clone(),
            };

            let anchor = capture_anchor(
                &state.message_ranges,
                state.scroll_offset,
                state.focused_message,
                state.message_nav_active,
            );
            let old_scroll = state.scroll_offset;

            let transcript = match state.transcript.clone() {
                Some(transcript) => transcript,
                None => match open_conversation(&state.conversation_path) {
                    Ok(transcript) => {
                        state.transcript = Some(transcript.clone());
                        transcript
                    }
                    Err(_) => return,
                },
            };
            let rendered = render_conversation(&transcript, &options);
            state.total_lines = rendered.lines.len();
            state.rendered_lines = rendered.lines;
            state.text_selection = None;
            state.message_ranges = rendered.messages;

            let max_scroll = state.total_lines.saturating_sub(viewport_height);

            let resolved_idx = anchor
                .and_then(|a| find_message_idx_or_prev(&state.message_ranges, a.entry_index))
                .or_else(|| (!state.message_ranges.is_empty()).then_some(0));
            state.focused_message = resolved_idx;

            state.scroll_offset = match (anchor, resolved_idx) {
                (Some(a), Some(idx)) => {
                    let new_msg = &state.message_ranges[idx];
                    let rel = if new_msg.entry_index == a.entry_index {
                        a.relative_row
                    } else {
                        a.relative_row.min(0)
                    };
                    let raw = new_msg.start_line as isize - rel;
                    raw.clamp(0, max_scroll as isize) as usize
                }
                _ => old_scroll.min(max_scroll),
            };

            if state.search_mode == ViewSearchMode::Active && !state.search_query.is_empty() {
                let query = ui::ViewSearchQuery::parse(&state.search_query);
                state.search_matches = state
                    .rendered_lines
                    .iter()
                    .enumerate()
                    .filter(|(_, line)| line_matches_query(line, &query))
                    .map(|(i, _)| i)
                    .collect();

                if state.search_matches.is_empty() {
                    state.current_match = 0;
                } else {
                    state.current_match = state.current_match.min(state.search_matches.len() - 1);
                }
            }
        }
    }

    pub(super) fn focus_next_message(&mut self, viewport_height: usize) {
        if let AppMode::View(ref mut state) = self.app_mode {
            if state.message_ranges.is_empty() {
                return;
            }
            if !state.message_nav_active {
                state.message_nav_active = true;
                Self::sync_focus_to_scroll(state, viewport_height);
            }
            let next = match state.focused_message {
                Some(i) if i + 1 < state.message_ranges.len() => i + 1,
                Some(i) => i,
                None => 0,
            };
            state.focused_message = Some(next);
            Self::ensure_message_visible(state, viewport_height);
        }
    }

    pub(super) fn focus_prev_message(&mut self, viewport_height: usize) {
        if let AppMode::View(ref mut state) = self.app_mode {
            if state.message_ranges.is_empty() {
                return;
            }
            if !state.message_nav_active {
                state.message_nav_active = true;
                Self::sync_focus_to_scroll(state, viewport_height);
            }
            let prev = match state.focused_message {
                Some(i) if i > 0 => i - 1,
                Some(i) => i,
                None => 0,
            };
            state.focused_message = Some(prev);
            Self::ensure_message_visible(state, viewport_height);
        }
    }

    fn focus_message_at_line(state: &mut ViewState, line_idx: usize) {
        let found = state
            .message_ranges
            .iter()
            .position(|m| line_idx >= m.start_line && line_idx < m.end_line);
        if let Some(idx) = found {
            state.message_nav_active = true;
            state.focused_message = Some(idx);
        }
    }

    pub(super) fn sync_focus_after_scroll(&mut self, viewport_height: usize) {
        if let AppMode::View(ref mut state) = self.app_mode
            && state.message_nav_active
        {
            Self::sync_focus_to_scroll(state, viewport_height);
        }
    }

    pub fn scroll_view(&mut self, delta: isize, viewport_height: usize) {
        if let AppMode::View(ref mut state) = self.app_mode {
            if state.search_mode == ViewSearchMode::Typing {
                return;
            }
            let max_scroll = state.total_lines.saturating_sub(viewport_height);
            let new_offset = if delta >= 0 {
                state
                    .scroll_offset
                    .saturating_add(delta as usize)
                    .min(max_scroll)
            } else {
                state.scroll_offset.saturating_sub((-delta) as usize)
            };
            state.scroll_offset = new_offset;
            self.sync_focus_after_scroll(viewport_height);
        }
    }

    pub fn scroll_mouse(&mut self, delta: isize, viewport_height: usize) {
        if self.dialog_mode != DialogMode::None {
            return;
        }

        match self.app_mode {
            AppMode::List => self.scroll_list(delta.signum()),
            AppMode::View(_) => self.scroll_view(delta, viewport_height),
        }
    }

    fn view_line_at_row(&self, row: u16, frame_area: Rect) -> Option<usize> {
        let AppMode::View(state) = &self.app_mode else {
            return None;
        };
        if self.dialog_mode != DialogMode::None {
            return None;
        }
        let layout = ui::view_layout_rects(frame_area, self, state);
        if row < layout.content.y || row >= layout.content.y.saturating_add(layout.content.height) {
            return None;
        }
        Some(state.scroll_offset + (row - layout.content.y) as usize)
    }

    fn message_idx_at_line(ranges: &[MessageRange], line_idx: usize) -> Option<usize> {
        let idx = ranges.partition_point(|m| m.end_line <= line_idx);
        ranges
            .get(idx)
            .is_some_and(|m| line_idx >= m.start_line && line_idx < m.end_line)
            .then_some(idx)
    }

    fn view_tool_output_at_line(&self, line_idx: usize) -> Option<ToolOutputId> {
        let AppMode::View(state) = &self.app_mode else {
            return None;
        };
        state.rendered_lines.get(line_idx).and_then(|line| {
            if line.clickable {
                line.tool_output_id.clone()
            } else {
                None
            }
        })
    }

    pub fn handle_view_mouse_move(&mut self, row: u16, frame_area: Rect) -> bool {
        let next = self
            .view_line_at_row(row, frame_area)
            .and_then(|line_idx| self.view_tool_output_at_line(line_idx));
        let AppMode::View(state) = &mut self.app_mode else {
            return false;
        };
        if state.hovered_tool_output == next {
            return false;
        }
        state.hovered_tool_output = next;
        true
    }

    pub fn handle_view_click(
        &mut self,
        row: u16,
        frame_area: Rect,
        viewport_height: usize,
    ) -> bool {
        let Some(line_idx) = self.view_line_at_row(row, frame_area) else {
            return false;
        };
        let tool_output = self.view_tool_output_at_line(line_idx);
        let message_idx = if let AppMode::View(state) = &self.app_mode {
            Self::message_idx_at_line(&state.message_ranges, line_idx)
        } else {
            None
        };
        if tool_output.is_none() && message_idx.is_none() {
            return false;
        }

        let AppMode::View(state) = &mut self.app_mode else {
            return false;
        };
        let mut changed = false;
        if let Some(idx) = message_idx
            && (!state.message_nav_active || state.focused_message != Some(idx))
        {
            state.message_nav_active = true;
            state.focused_message = Some(idx);
            changed = true;
        }
        if let Some(id) = tool_output {
            if state.expanded_tool_outputs.contains(&id) {
                state.expanded_tool_outputs.remove(&id);
            } else {
                state.expanded_tool_outputs.insert(id.clone());
            }
            state.hovered_tool_output = Some(id);
            changed = true;
        }
        if changed {
            self.re_render_view(viewport_height);
        }
        changed
    }

    fn sync_focus_to_scroll(state: &mut ViewState, viewport_height: usize) {
        if state.message_ranges.is_empty() {
            return;
        }
        let viewport_start = state.scroll_offset;
        let viewport_end = viewport_start + viewport_height;
        let found = state
            .message_ranges
            .iter()
            .position(|m| m.end_line > viewport_start && m.start_line < viewport_end);
        if let Some(idx) = found {
            state.focused_message = Some(idx);
        }
    }

    fn ensure_message_visible(state: &mut ViewState, viewport_height: usize) {
        if let Some(idx) = state.focused_message
            && let Some(msg) = state.message_ranges.get(idx)
        {
            let max_scroll = state.total_lines.saturating_sub(viewport_height);
            if msg.start_line < state.scroll_offset
                || msg.start_line >= state.scroll_offset + viewport_height
            {
                state.scroll_offset = msg.start_line.min(max_scroll);
            }
        }
    }

    pub(super) fn copy_focused_message(&mut self, viewport_height: usize) {
        if let AppMode::View(ref mut state) = self.app_mode
            && !state.message_nav_active
        {
            state.message_nav_active = true;
            Self::sync_focus_to_scroll(state, viewport_height);
        }

        let (transcript, entry_index) = if let AppMode::View(ref state) = self.app_mode {
            if let Some(idx) = state.focused_message
                && let Some(transcript) = state.transcript.clone()
            {
                if let Some(msg) = state.message_ranges.get(idx) {
                    (transcript, msg.entry_index)
                } else {
                    return;
                }
            } else {
                return;
            }
        } else {
            return;
        };

        let options = if let AppMode::View(ref state) = self.app_mode {
            crate::tui::export::ExportOptions {
                show_tools: state.tool_display.is_visible(),
                show_thinking: state.show_thinking,
            }
        } else {
            return;
        };

        match crate::tui::export::extract_message_text(&transcript, entry_index, options) {
            Ok(text) if text.is_empty() => {
                self.status_message = Some((
                    "No text content in this message".to_string(),
                    std::time::Instant::now(),
                ));
            }
            Ok(text) => match crate::tui::export::copy_to_system_clipboard(&text) {
                Ok(crate::tui::export::ClipboardDestination::System) => {
                    self.status_message = Some((
                        "Message copied to clipboard".to_string(),
                        std::time::Instant::now(),
                    ));
                }
                Ok(crate::tui::export::ClipboardDestination::Terminal) => {
                    self.status_message = Some((
                        "Message sent to terminal clipboard".to_string(),
                        std::time::Instant::now(),
                    ));
                }
                Err(e) => {
                    self.status_message = Some((e, std::time::Instant::now()));
                }
            },
            Err(e) => {
                self.status_message = Some((e, std::time::Instant::now()));
            }
        }
    }

    pub fn check_view_resize(&mut self, new_content_width: usize, viewport_height: usize) {
        if let AppMode::View(ref mut state) = self.app_mode
            && state.content_width != new_content_width
        {
            state.content_width = new_content_width;
            self.re_render_view(viewport_height);
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ScrollAnchor {
    entry_index: usize,
    relative_row: isize,
}

fn capture_anchor(
    ranges: &[MessageRange],
    scroll_offset: usize,
    focused: Option<usize>,
    nav_active: bool,
) -> Option<ScrollAnchor> {
    if ranges.is_empty() {
        return None;
    }

    let msg = if nav_active {
        focused.and_then(|i| ranges.get(i))
    } else {
        None
    }
    .unwrap_or_else(|| {
        let i = ranges.partition_point(|m| m.start_line < scroll_offset);
        ranges.get(i).unwrap_or_else(|| ranges.last().unwrap())
    });

    Some(ScrollAnchor {
        entry_index: msg.entry_index,
        relative_row: msg.start_line as isize - scroll_offset as isize,
    })
}

fn find_message_idx_or_prev(ranges: &[MessageRange], entry_index: usize) -> Option<usize> {
    if ranges.is_empty() {
        return None;
    }
    match ranges.binary_search_by_key(&entry_index, |m| m.entry_index) {
        Ok(idx) => Some(idx),
        Err(0) => Some(0),
        Err(idx) => Some(idx - 1),
    }
}

/// Whether `line` has a match for `query`, by the list's matching rules.
pub fn line_matches_query(line: &RenderedLine, query: &ui::ViewSearchQuery) -> bool {
    let full_text: String = line.spans.iter().map(|(text, _)| text.as_str()).collect();
    query.has_match(&full_text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_search_matches_lines_like_the_list() {
        let matches = |line: &str, query: &str| {
            let line = RenderedLine::new(vec![(line.to_string(), Default::default())]);
            line_matches_query(&line, &ui::ViewSearchQuery::parse(query))
        };
        assert!(matches("the hidden needle_token appears", "needle token"));
        assert!(matches("late mention of İstanbul", "istanbul"));
        assert!(matches("tag v1.1.1", "1.1"));
        assert!(matches("tag v1.2.3", "1.2.3"));
        assert!(matches("encode H264", "264"));
        assert!(!matches("the AppConfig type", "onfig"));
        assert!(!matches("encode H264", "64"));
        assert!(matches("needle only", "needle token"));
        assert!(!matches("the fired plan", "ired"));
        assert!(matches("exact audio_generation", "\"audio_generation\""));
        assert!(!matches("audio generation", "\"audio_generation\""));
    }
}
