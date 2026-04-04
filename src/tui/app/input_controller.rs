use super::{Action, App, AppMode, DialogMode, ListSearchMode, ViewSearchMode, ViewState};
use crate::search;
use crossterm::event::{KeyCode, KeyModifiers};

impl App {
    fn cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos -= 1;
        }
    }

    fn cursor_right(&mut self) {
        let len = self.query.chars().count();
        if self.cursor_pos < len {
            self.cursor_pos += 1;
        }
    }

    fn cursor_home(&mut self) {
        self.cursor_pos = 0;
    }

    fn cursor_end(&mut self) {
        self.cursor_pos = self.query.chars().count();
    }

    fn cursor_word_left(&mut self) {
        let chars: Vec<char> = self.query.chars().collect();
        let mut pos = self.cursor_pos.min(chars.len());
        while pos > 0 && search::is_word_separator(chars[pos - 1]) {
            pos -= 1;
        }
        while pos > 0 && !search::is_word_separator(chars[pos - 1]) {
            pos -= 1;
        }
        self.cursor_pos = pos;
    }

    fn cursor_word_right(&mut self) {
        let chars: Vec<char> = self.query.chars().collect();
        let len = chars.len();
        let mut pos = self.cursor_pos.min(len);
        while pos < len && !search::is_word_separator(chars[pos]) {
            pos += 1;
        }
        while pos < len && search::is_word_separator(chars[pos]) {
            pos += 1;
        }
        self.cursor_pos = pos;
    }

    fn kill_to_end(&mut self) -> bool {
        let len = self.query.chars().count();
        if self.cursor_pos >= len {
            return false;
        }
        let byte_pos = self
            .query
            .char_indices()
            .nth(self.cursor_pos)
            .map(|(i, _)| i)
            .unwrap_or(self.query.len());
        self.query.truncate(byte_pos);
        true
    }

    fn kill_to_start(&mut self) -> bool {
        if self.cursor_pos == 0 {
            return false;
        }
        let byte_pos = self
            .query
            .char_indices()
            .nth(self.cursor_pos)
            .map(|(i, _)| i)
            .unwrap_or(self.query.len());
        self.query.replace_range(..byte_pos, "");
        self.cursor_pos = 0;
        true
    }

    fn delete_word_backwards(&mut self) -> bool {
        let chars: Vec<char> = self.query.chars().collect();
        let cursor = self.cursor_pos.min(chars.len());
        if cursor == 0 {
            return false;
        }

        let mut new_pos = cursor;
        while new_pos > 0 && search::is_word_separator(chars[new_pos - 1]) {
            new_pos -= 1;
        }
        while new_pos > 0 && !search::is_word_separator(chars[new_pos - 1]) {
            new_pos -= 1;
        }
        if new_pos == cursor {
            return false;
        }

        let start_byte = self
            .query
            .char_indices()
            .nth(new_pos)
            .map(|(i, _)| i)
            .unwrap_or(0);
        let end_byte = self
            .query
            .char_indices()
            .nth(cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.query.len());

        self.query.replace_range(start_byte..end_byte, "");
        self.cursor_pos = new_pos;
        true
    }

    pub fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        viewport_height: usize,
    ) -> Option<Action> {
        match self.dialog_mode {
            DialogMode::ConfirmDelete => return self.handle_confirm_key(code),
            DialogMode::ExportMenu { .. } | DialogMode::YankMenu { .. } => {
                return self.handle_menu_key(code);
            }
            DialogMode::Help { .. } => return self.handle_help_key(code, viewport_height),
            DialogMode::SemanticDebug => {
                self.dialog_mode = DialogMode::None;
                return None;
            }
            DialogMode::Rename { .. } => return self.handle_rename_key(code, modifiers),
            DialogMode::ProfilePicker { .. } => return self.handle_profile_picker_key(code),
            DialogMode::None => {}
        }

        if self.list_search_mode == ListSearchMode::Semantic
            && self.semantic_result_metadata_for_selection().is_some()
            && matches!(code, KeyCode::Char('s'))
            && modifiers.contains(KeyModifiers::CONTROL)
        {
            self.dialog_mode = DialogMode::SemanticDebug;
            return None;
        }

        match &self.app_mode {
            AppMode::View(_) => self.handle_view_key(code, modifiers, viewport_height),
            AppMode::List => self.handle_list_key(code, modifiers, viewport_height),
        }
    }

    fn handle_view_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        viewport_height: usize,
    ) -> Option<Action> {
        if let AppMode::View(ref state) = self.app_mode
            && state.search_mode == ViewSearchMode::Typing
        {
            return self.handle_search_typing_key(code, modifiers);
        }

        if self.keys.delete.matches(code, modifiers) {
            if !self.single_file_mode {
                self.dialog_mode = DialogMode::ConfirmDelete;
            }
            return None;
        }
        if self.keys.resume.matches(code, modifiers) {
            if !self.single_file_mode {
                return self.resolve_resume_action(false);
            }
            return None;
        }
        if self.keys.fork.matches(code, modifiers) {
            if !self.single_file_mode {
                return self.resolve_resume_action(true);
            }
            return None;
        }

        let state = match &mut self.app_mode {
            AppMode::View(s) => s,
            _ => return None,
        };

        let max_scroll = state.total_lines.saturating_sub(viewport_height);

        match code {
            KeyCode::Esc => {
                if let AppMode::View(ref mut state) = self.app_mode
                    && state.message_nav_active
                {
                    state.message_nav_active = false;
                    return None;
                }
                if let AppMode::View(ref state) = self.app_mode
                    && state.search_mode == ViewSearchMode::Active
                {
                    self.clear_view_search();
                    return None;
                }
                if self.single_file_mode {
                    return Some(Action::Quit);
                }
                self.app_mode = AppMode::List;
                None
            }
            KeyCode::Char('q') => {
                if self.single_file_mode {
                    return Some(Action::Quit);
                }
                self.app_mode = AppMode::List;
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.scroll_offset = (state.scroll_offset + 1).min(max_scroll);
                self.sync_focus_after_scroll(viewport_height);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                state.scroll_offset = state.scroll_offset.saturating_sub(1);
                self.sync_focus_after_scroll(viewport_height);
                None
            }
            KeyCode::Char('J') | KeyCode::Char(']') => {
                self.focus_next_message(viewport_height);
                None
            }
            KeyCode::Char('K') | KeyCode::Char('[') => {
                self.focus_prev_message(viewport_height);
                None
            }
            KeyCode::Char('d') if !modifiers.contains(KeyModifiers::CONTROL) => {
                state.scroll_offset = (state.scroll_offset + viewport_height / 2).min(max_scroll);
                self.sync_focus_after_scroll(viewport_height);
                None
            }
            KeyCode::Char('u') if !modifiers.contains(KeyModifiers::CONTROL) => {
                let half_page = viewport_height / 2;
                state.scroll_offset = state.scroll_offset.saturating_sub(half_page);
                self.sync_focus_after_scroll(viewport_height);
                None
            }
            KeyCode::PageDown => {
                state.scroll_offset = (state.scroll_offset + viewport_height).min(max_scroll);
                self.sync_focus_after_scroll(viewport_height);
                None
            }
            KeyCode::PageUp => {
                state.scroll_offset = state.scroll_offset.saturating_sub(viewport_height);
                self.sync_focus_after_scroll(viewport_height);
                None
            }
            KeyCode::Char('g') | KeyCode::Home => {
                state.scroll_offset = 0;
                self.sync_focus_after_scroll(viewport_height);
                None
            }
            KeyCode::Char('G') | KeyCode::End => {
                state.scroll_offset = max_scroll;
                self.sync_focus_after_scroll(viewport_height);
                None
            }
            KeyCode::Char('/') => {
                self.start_view_search();
                None
            }
            KeyCode::Char('n') if !modifiers.contains(KeyModifiers::CONTROL) => {
                if let AppMode::View(ref state) = self.app_mode
                    && state.search_mode == ViewSearchMode::Active
                {
                    self.next_search_match(viewport_height);
                }
                None
            }
            KeyCode::Char('N') => {
                if let AppMode::View(ref state) = self.app_mode
                    && state.search_mode == ViewSearchMode::Active
                {
                    self.prev_search_match(viewport_height);
                }
                None
            }
            KeyCode::Char('t') => {
                self.toggle_view_tools(viewport_height);
                None
            }
            KeyCode::Char('T') => {
                self.toggle_view_thinking(viewport_height);
                None
            }
            KeyCode::Char('i') => {
                self.toggle_view_timing(viewport_height);
                None
            }
            KeyCode::Char('p') => {
                if let AppMode::View(ref state) = self.app_mode {
                    self.status_message = Some((
                        state.conversation_path.display().to_string(),
                        std::time::Instant::now(),
                    ));
                }
                None
            }
            KeyCode::Char('Y') => {
                if let AppMode::View(ref state) = self.app_mode {
                    let path_str = state.conversation_path.display().to_string();
                    match crate::tui::export::copy_to_system_clipboard(&path_str) {
                        Ok(crate::tui::export::ClipboardDestination::System) => {
                            self.status_message = Some((
                                "Path copied to clipboard".to_string(),
                                std::time::Instant::now(),
                            ));
                        }
                        Ok(crate::tui::export::ClipboardDestination::Terminal) => {
                            self.status_message = Some((
                                "Path sent to terminal clipboard".to_string(),
                                std::time::Instant::now(),
                            ));
                        }
                        Err(e) => {
                            self.status_message = Some((e, std::time::Instant::now()));
                        }
                    }
                }
                None
            }
            KeyCode::Char('I') => {
                if let AppMode::View(ref state) = self.app_mode
                    && let Some(id) = state.conversation_path.file_stem().and_then(|s| s.to_str())
                {
                    match crate::tui::export::copy_to_system_clipboard(id) {
                        Ok(crate::tui::export::ClipboardDestination::System) => {
                            self.status_message = Some((
                                "Session ID copied to clipboard".to_string(),
                                std::time::Instant::now(),
                            ));
                        }
                        Ok(crate::tui::export::ClipboardDestination::Terminal) => {
                            self.status_message = Some((
                                "Session ID sent to terminal clipboard".to_string(),
                                std::time::Instant::now(),
                            ));
                        }
                        Err(e) => {
                            self.status_message = Some((e, std::time::Instant::now()));
                        }
                    }
                }
                None
            }
            KeyCode::Char('e') => {
                self.dialog_mode = DialogMode::ExportMenu { selected: 0 };
                None
            }
            KeyCode::Char('y') => {
                let nav_active = matches!(
                    self.app_mode,
                    AppMode::View(ViewState {
                        message_nav_active: true,
                        ..
                    })
                );
                if nav_active {
                    self.copy_focused_message(viewport_height);
                } else {
                    self.dialog_mode = DialogMode::YankMenu { selected: 0 };
                }
                None
            }
            KeyCode::Char('?') => {
                self.dialog_mode = DialogMode::Help { scroll: 0 };
                None
            }
            KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
                state.scroll_offset = (state.scroll_offset + viewport_height / 2).min(max_scroll);
                self.sync_focus_after_scroll(viewport_height);
                None
            }
            KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                let half_page = viewport_height / 2;
                state.scroll_offset = state.scroll_offset.saturating_sub(half_page);
                self.sync_focus_after_scroll(viewport_height);
                None
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
            _ => None,
        }
    }

    fn handle_search_typing_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Option<Action> {
        match code {
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.clear_view_search();
                None
            }
            KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.clear_view_search_query();
                None
            }
            KeyCode::Char('w') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.delete_view_search_word_backwards();
                None
            }
            KeyCode::Char(c) => {
                self.push_view_search_char(c);
                None
            }
            KeyCode::Backspace => {
                self.backspace_view_search();
                None
            }
            KeyCode::Enter => {
                self.commit_view_search();
                None
            }
            KeyCode::Esc => {
                self.clear_view_search();
                None
            }
            _ => None,
        }
    }

    fn handle_list_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        viewport_height: usize,
    ) -> Option<Action> {
        if self.is_loading() {
            return self.handle_common_list_key(code, modifiers, false);
        }

        if self.keys.delete.matches(code, modifiers) {
            if self.get_selected_path().is_some() {
                self.dialog_mode = DialogMode::ConfirmDelete;
            }
            return None;
        }
        if self.keys.resume.matches(code, modifiers) {
            return self.resolve_resume_action(false);
        }
        if self.keys.fork.matches(code, modifiers) {
            return self.resolve_resume_action(true);
        }

        match code {
            _ if self.keys.rename.matches(code, modifiers) => {
                if self.get_selected_path().is_some() {
                    self.start_rename();
                }
                None
            }
            KeyCode::Enter => None,
            KeyCode::Home => {
                self.select_first();
                None
            }
            KeyCode::End => {
                self.select_last();
                None
            }
            KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_half_page_down(viewport_height);
                None
            }
            KeyCode::Char('o') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.get_selected_path().map(Action::Select)
            }
            _ => self.handle_common_list_key(code, modifiers, true),
        }
    }

    fn handle_common_list_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        dispatch_search: bool,
    ) -> Option<Action> {
        match code {
            KeyCode::Esc => {
                if self.query.is_empty() {
                    Some(Action::Quit)
                } else {
                    self.query.clear();
                    self.cursor_pos = 0;
                    self.dispatch_search();
                    None
                }
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
            KeyCode::Char('t') if modifiers.contains(KeyModifiers::CONTROL) => {
                if self.semantic_toggle_available() {
                    self.toggle_list_search_mode();
                }
                None
            }
            KeyCode::Left if modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor_word_left();
                None
            }
            KeyCode::Right if modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor_word_right();
                None
            }
            KeyCode::Left => {
                self.cursor_left();
                None
            }
            KeyCode::Right => {
                self.cursor_right();
                None
            }
            KeyCode::Up => {
                self.select_prev();
                None
            }
            KeyCode::Down => {
                self.select_next();
                None
            }
            KeyCode::Char('n') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_next();
                None
            }
            KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_prev();
                None
            }
            KeyCode::PageUp => {
                self.select_page_up();
                None
            }
            KeyCode::PageDown => {
                self.select_page_down();
                None
            }
            KeyCode::Tab => {
                self.toggle_workspace_filter();
                None
            }
            KeyCode::Char('?') => {
                self.dialog_mode = DialogMode::Help { scroll: 0 };
                None
            }
            _ if self.handle_list_query_edit_key(code, modifiers, dispatch_search) => None,
            _ => None,
        }
    }

    fn handle_list_query_edit_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        dispatch_search: bool,
    ) -> bool {
        let changed = match code {
            KeyCode::Char('?') => return false,
            KeyCode::Char('a') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor_home();
                false
            }
            KeyCode::Char('e') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor_end();
                false
            }
            KeyCode::Char('b') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor_left();
                false
            }
            KeyCode::Char('f') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor_right();
                false
            }
            KeyCode::Char('b') if modifiers.contains(KeyModifiers::ALT) => {
                self.cursor_word_left();
                false
            }
            KeyCode::Char('f') if modifiers.contains(KeyModifiers::ALT) => {
                self.cursor_word_right();
                false
            }
            KeyCode::Char('k') if modifiers.contains(KeyModifiers::CONTROL) => self.kill_to_end(),
            KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => self.kill_to_start(),
            KeyCode::Char('w') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.delete_word_backwards()
            }
            KeyCode::Char(c) => {
                self.insert_query_char(c);
                true
            }
            KeyCode::Backspace => self.backspace_query(),
            KeyCode::Delete => self.delete_query_char(),
            _ => return false,
        };

        if changed && dispatch_search {
            self.dispatch_search();
        }

        true
    }

    fn insert_query_char(&mut self, c: char) {
        let byte_pos = self
            .query
            .char_indices()
            .nth(self.cursor_pos)
            .map(|(i, _)| i)
            .unwrap_or(self.query.len());
        self.query.insert(byte_pos, c);
        self.cursor_pos += 1;
    }

    fn backspace_query(&mut self) -> bool {
        if self.cursor_pos > 0
            && let Some((byte_pos, _)) = self.query.char_indices().nth(self.cursor_pos - 1)
        {
            self.query.remove(byte_pos);
            self.cursor_pos -= 1;
            return true;
        }
        false
    }

    fn delete_query_char(&mut self) -> bool {
        let len = self.query.chars().count();
        if self.cursor_pos < len
            && let Some((byte_pos, _)) = self.query.char_indices().nth(self.cursor_pos)
        {
            self.query.remove(byte_pos);
            return true;
        }
        false
    }
}
