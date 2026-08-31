use super::{Action, App, AppMode, DialogMode};
use crossterm::event::{KeyCode, KeyModifiers};

const EXPORT_OPTIONS: [&str; 4] = [
    "Ledger (formatted)",
    "Plain text",
    "Markdown",
    "JSONL (raw)",
];

impl App {
    pub(super) fn handle_confirm_key(&mut self, code: KeyCode) -> Option<Action> {
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.dialog_mode = DialogMode::None;
                self.get_selected_path().map(Action::Delete)
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.dialog_mode = DialogMode::None;
                None
            }
            _ => None,
        }
    }

    pub(super) fn handle_menu_key(&mut self, code: KeyCode) -> Option<Action> {
        let (selected, is_yank) = match &mut self.dialog_mode {
            DialogMode::ExportMenu { selected } => (selected, false),
            DialogMode::YankMenu { selected } => (selected, true),
            _ => return None,
        };

        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                *selected = selected.saturating_sub(1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                *selected = (*selected + 1).min(EXPORT_OPTIONS.len() - 1);
                None
            }
            KeyCode::Char('1') => {
                self.perform_export(0, is_yank);
                self.dialog_mode = DialogMode::None;
                None
            }
            KeyCode::Char('2') => {
                self.perform_export(1, is_yank);
                self.dialog_mode = DialogMode::None;
                None
            }
            KeyCode::Char('3') => {
                self.perform_export(2, is_yank);
                self.dialog_mode = DialogMode::None;
                None
            }
            KeyCode::Char('4') => {
                self.perform_export(3, is_yank);
                self.dialog_mode = DialogMode::None;
                None
            }
            KeyCode::Enter => {
                let sel = *selected;
                self.perform_export(sel, is_yank);
                self.dialog_mode = DialogMode::None;
                None
            }
            KeyCode::Esc => {
                self.dialog_mode = DialogMode::None;
                None
            }
            _ => None,
        }
    }

    /// Open the subagent picker for the currently viewed session, if it has
    /// sidecar `agent-*.jsonl` transcripts. No-op (with a status hint) when
    /// not in view mode or the session has no subagents.
    pub(super) fn open_subagent_picker(&mut self) {
        let path = match &self.app_mode {
            AppMode::View(state) => state.conversation_path.clone(),
            _ => return,
        };
        let entries = crate::history::discover_subagents(&path);
        if entries.is_empty() {
            self.status_message = Some((
                "No subagents for this session".to_string(),
                std::time::Instant::now(),
            ));
            return;
        }
        self.dialog_mode = DialogMode::SubagentPicker {
            entries,
            selected: 0,
        };
    }

    pub(super) fn handle_subagent_picker_key(&mut self, code: KeyCode) -> Option<Action> {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let DialogMode::SubagentPicker { selected, .. } = &mut self.dialog_mode {
                    *selected = selected.saturating_sub(1);
                }
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let DialogMode::SubagentPicker { entries, selected } = &mut self.dialog_mode {
                    *selected = (*selected + 1).min(entries.len().saturating_sub(1));
                }
                None
            }
            KeyCode::Enter => {
                let path = match &self.dialog_mode {
                    DialogMode::SubagentPicker { entries, selected } => {
                        entries.get(*selected).map(|e| e.path.clone())
                    }
                    _ => None,
                };
                self.dialog_mode = DialogMode::None;
                if let Some(path) = path {
                    let content_width = match &self.app_mode {
                        AppMode::View(state) => state.content_width,
                        _ => return None,
                    };
                    self.open_conversation_path(path, content_width);
                }
                None
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.dialog_mode = DialogMode::None;
                None
            }
            _ => None,
        }
    }

    pub(super) fn handle_help_key(
        &mut self,
        code: KeyCode,
        viewport_height: usize,
    ) -> Option<Action> {
        let DialogMode::Help { scroll } = &mut self.dialog_mode else {
            return None;
        };

        match code {
            KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Esc => {
                self.dialog_mode = DialogMode::None;
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                *scroll = scroll.saturating_add(1);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                *scroll = scroll.saturating_sub(1);
                None
            }
            KeyCode::PageDown | KeyCode::Char('d') => {
                *scroll = scroll.saturating_add(viewport_height.max(1));
                None
            }
            KeyCode::PageUp | KeyCode::Char('u') => {
                *scroll = scroll.saturating_sub(viewport_height.max(1));
                None
            }
            KeyCode::Home | KeyCode::Char('g') => {
                *scroll = 0;
                None
            }
            _ => None,
        }
    }

    pub(super) fn start_rename(&mut self) {
        let Some(idx) = self.get_selected_conversation_index() else {
            return;
        };
        let input = self.conversations[idx]
            .custom_title
            .clone()
            .unwrap_or_default();
        let cursor = input.chars().count();
        self.dialog_mode = DialogMode::Rename { input, cursor };
    }

    pub(super) fn handle_rename_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Option<Action> {
        match code {
            KeyCode::Esc => {
                self.dialog_mode = DialogMode::None;
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.dialog_mode = DialogMode::None;
            }
            KeyCode::Enter => self.submit_rename(),
            KeyCode::Left => {
                if let DialogMode::Rename { cursor, .. } = &mut self.dialog_mode {
                    *cursor = cursor.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if let DialogMode::Rename { input, cursor } = &mut self.dialog_mode {
                    *cursor = (*cursor + 1).min(input.chars().count());
                }
            }
            KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                if let DialogMode::Rename { input, cursor } = &mut self.dialog_mode {
                    input.clear();
                    *cursor = 0;
                }
            }
            KeyCode::Home | KeyCode::Char('a') if modifiers.contains(KeyModifiers::CONTROL) => {
                if let DialogMode::Rename { cursor, .. } = &mut self.dialog_mode {
                    *cursor = 0;
                }
            }
            KeyCode::End | KeyCode::Char('e') if modifiers.contains(KeyModifiers::CONTROL) => {
                if let DialogMode::Rename { input, cursor } = &mut self.dialog_mode {
                    *cursor = input.chars().count();
                }
            }
            KeyCode::Backspace => {
                if let DialogMode::Rename { input, cursor } = &mut self.dialog_mode
                    && *cursor > 0
                    && let Some((byte_pos, _)) = input.char_indices().nth(*cursor - 1)
                {
                    input.remove(byte_pos);
                    *cursor -= 1;
                }
            }
            KeyCode::Delete => {
                if let DialogMode::Rename { input, cursor } = &mut self.dialog_mode
                    && *cursor < input.chars().count()
                    && let Some((byte_pos, _)) = input.char_indices().nth(*cursor)
                {
                    input.remove(byte_pos);
                }
            }
            KeyCode::Char(ch) if modifiers.is_empty() || modifiers == KeyModifiers::SHIFT => {
                if let DialogMode::Rename { input, cursor } = &mut self.dialog_mode {
                    let byte_pos = input
                        .char_indices()
                        .nth(*cursor)
                        .map(|(i, _)| i)
                        .unwrap_or(input.len());
                    input.insert(byte_pos, ch);
                    *cursor += 1;
                }
            }
            _ => {}
        }
        None
    }

    pub(super) fn submit_rename(&mut self) {
        let title = match &self.dialog_mode {
            DialogMode::Rename { input, .. } => input.trim().to_string(),
            _ => return,
        };
        let Some(idx) = self.get_selected_conversation_index() else {
            self.dialog_mode = DialogMode::None;
            return;
        };
        let path = self.conversations[idx].path.clone();

        let source = self.conversations[idx].source;
        let rename = match source {
            crate::history::Source::Claude => crate::history::append_session_rename(&path, &title),
            crate::history::Source::Pi => crate::history::pi::append_session_rename(&path, &title),
            crate::history::Source::Omp => {
                crate::history::pi::append_omp_session_rename(&path, &title)
            }
        };
        match rename
            .and_then(|_| crate::history::process_conversation_file(path.clone(), None, None))
        {
            Ok(Some(mut conv)) => {
                conv.index = idx;
                conv.project_name = self.conversations[idx].project_name.clone();
                conv.project_path = self.conversations[idx].project_path.clone();
                self.conversations[idx] = conv;
                self.dialog_mode = DialogMode::None;
                self.status_message =
                    Some(("Session renamed".to_string(), std::time::Instant::now()));
                self.refresh_search_data();
                self.update_filter();
                if let Some(new_selected) = self
                    .filtered
                    .iter()
                    .position(|&i| self.conversations[i].path == path)
                {
                    self.selected = Some(new_selected);
                }
            }
            Ok(None) => {
                self.status_message = Some((
                    "Failed to rename: conversation became empty".to_string(),
                    std::time::Instant::now(),
                ));
            }
            Err(e) => {
                self.status_message = Some((
                    format!("Failed to rename: {}", e),
                    std::time::Instant::now(),
                ));
            }
        }
    }

    pub(super) fn perform_export(&mut self, option: usize, to_clipboard: bool) {
        let (path, options) = match &self.app_mode {
            AppMode::View(state) => (
                state.conversation_path.clone(),
                crate::tui::export::ExportOptions {
                    show_tools: state.tool_display.is_visible(),
                    show_thinking: state.show_thinking,
                },
            ),
            _ => return,
        };

        let format = match crate::tui::export::ExportFormat::from_index(option) {
            Some(f) => f,
            None => return,
        };

        let result = if to_clipboard {
            crate::tui::export::export_to_clipboard(&path, format, options)
        } else {
            crate::tui::export::export_to_file(&path, format, options)
        };

        self.status_message = Some((result.message, std::time::Instant::now()));
    }
}
