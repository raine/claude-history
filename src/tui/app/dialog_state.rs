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

    /// Open the annotate prompt for the conversation being viewed.
    ///
    /// The focused message supplies the line through two hops: `focused_message`
    /// indexes `message_ranges`, whose `entry_index` counts parsed entries after
    /// filtering rather than file lines, so the matching entry carries the line.
    /// With no focused message the annotation attaches to the session.
    pub(super) fn start_annotate(&mut self) {
        let line = match &self.app_mode {
            AppMode::View(state) => state
                .focused_message
                .and_then(|index| state.message_ranges.get(index))
                .and_then(|range| {
                    let entries = state.parsed_entries.as_ref()?;
                    entries
                        .iter()
                        .find(|entry| entry.entry_index == range.entry_index)
                        .map(|entry| entry.jsonl_line)
                }),
            _ => return,
        };
        self.dialog_mode = DialogMode::Annotate {
            input: String::new(),
            cursor: 0,
            line,
            replacing: None,
        };
    }

    /// Open the annotate prompt on the selected note, pre-filled with its text.
    pub(super) fn start_edit_annotation(&mut self) {
        let AppMode::View(state) = &self.app_mode else {
            return;
        };
        let Some(id) = state.focused_annotation.clone() else {
            return;
        };
        let Some(annotation) = state
            .annotations
            .session
            .iter()
            .chain(state.annotations.positioned.iter())
            .find(|annotation| annotation.id == id)
        else {
            return;
        };
        let input = annotation.text.clone();
        let cursor = input.chars().count();
        let line = annotation.anchor_line();
        self.dialog_mode = DialogMode::Annotate {
            input,
            cursor,
            line,
            replacing: Some(id),
        };
    }

    /// Remove the selected note.
    /// Copies the selected note's text to the clipboard.
    pub(super) fn copy_focused_annotation(&mut self) {
        let AppMode::View(state) = &self.app_mode else {
            return;
        };
        let Some(id) = state.focused_annotation.as_deref() else {
            return;
        };
        let Some(text) = annotation_text(&state.annotations, id) else {
            return;
        };
        let message = match crate::tui::export::copy_to_system_clipboard(&text) {
            Ok(crate::tui::export::ClipboardDestination::System) => {
                "Note copied to clipboard".to_string()
            }
            Ok(crate::tui::export::ClipboardDestination::Terminal) => {
                "Note sent to terminal clipboard".to_string()
            }
            Err(e) => e,
        };
        self.status_message = Some((message, std::time::Instant::now()));
    }

    pub(super) fn delete_focused_annotation(&mut self, viewport_height: usize) {
        let AppMode::View(state) = &self.app_mode else {
            return;
        };
        let Some(id) = state.focused_annotation.clone() else {
            return;
        };
        let path = state.conversation_path.clone();
        // The annotator holding the note is carried on the note itself, so the
        // delete reaches the store it came from rather than the one writes go
        // to.
        let annotator = state
            .annotations
            .session
            .iter()
            .chain(state.annotations.positioned.iter())
            .find(|annotation| annotation.id == id)
            .map(|annotation| annotation.annotator.clone())
            .unwrap_or_default();
        if self.annotators().delete(&path, &id, &annotator).is_err() {
            return;
        }
        let annotations = self.annotators().read_one(&path);
        if let AppMode::View(state) = &mut self.app_mode {
            state.annotations = annotations;
            state.focused_annotation = None;
        }
        self.refresh_annotation_count(&path);
        self.refresh_conversation_annotations(&path);
        self.re_render_view(viewport_height);
    }

    pub(super) fn submit_annotate(&mut self, viewport_height: usize) {
        let (text, line, replacing) = match &self.dialog_mode {
            DialogMode::Annotate {
                input,
                line,
                replacing,
                ..
            } => (input.trim().to_string(), *line, replacing.clone()),
            _ => return,
        };
        self.dialog_mode = DialogMode::None;
        if text.is_empty() {
            return;
        }
        let AppMode::View(state) = &self.app_mode else {
            return;
        };
        let path = state.conversation_path.clone();

        let replaced_annotator = replacing.as_ref().and_then(|id| {
            state
                .annotations
                .session
                .iter()
                .chain(state.annotations.positioned.iter())
                .find(|annotation| annotation.id == *id)
                .map(|annotation| annotation.annotator.clone())
        });

        // The replacement is written before the original is removed, so a
        // failure part-way leaves the note present rather than lost.
        let annotation = crate::annotations::Annotation {
            id: crate::annotations::generated_id(),
            targets: line
                .map(crate::annotations::TargetSpan::single)
                .into_iter()
                .collect(),
            kind: "note".to_string(),
            text,
            annotator: String::new(),
        };
        if self.annotators().write(&path, &annotation).is_err() {
            return;
        }
        if let Some(id) = replacing {
            let _ = self
                .annotators()
                .delete(&path, &id, &replaced_annotator.unwrap_or_default());
        }
        // Re-read rather than appending in memory, so the viewer renders what
        // the annotators hold rather than the record this process sent them.
        let annotations = self.annotators().read_one(&path);
        if let AppMode::View(state) = &mut self.app_mode {
            state.annotations = annotations;
            state.focused_annotation = None;
        }
        self.refresh_annotation_count(&path);
        self.refresh_conversation_annotations(&path);
        // Without this the written annotation sits in state unseen until the
        // next redraw is triggered by something else.
        self.re_render_view(viewport_height);
    }

    pub(super) fn handle_annotate_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        viewport_height: usize,
    ) -> Option<Action> {
        match code {
            KeyCode::Esc => {
                self.dialog_mode = DialogMode::None;
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.dialog_mode = DialogMode::None;
            }
            KeyCode::Enter => self.submit_annotate(viewport_height),
            KeyCode::Left => {
                if let DialogMode::Annotate { cursor, .. } = &mut self.dialog_mode {
                    *cursor = cursor.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if let DialogMode::Annotate { input, cursor, .. } = &mut self.dialog_mode {
                    *cursor = (*cursor + 1).min(input.chars().count());
                }
            }
            KeyCode::Backspace => {
                if let DialogMode::Annotate { input, cursor, .. } = &mut self.dialog_mode
                    && *cursor > 0
                    && let Some((byte_pos, _)) = input.char_indices().nth(*cursor - 1)
                {
                    input.remove(byte_pos);
                    *cursor -= 1;
                }
            }
            KeyCode::Char(character) => {
                if let DialogMode::Annotate { input, cursor, .. } = &mut self.dialog_mode {
                    let byte_pos = input
                        .char_indices()
                        .nth(*cursor)
                        .map(|(pos, _)| pos)
                        .unwrap_or(input.len());
                    input.insert(byte_pos, character);
                    *cursor += 1;
                }
            }
            _ => {}
        }
        None
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

/// The text of the note with this id, from either the session-level or the
/// positioned notes of a conversation.
fn annotation_text(
    annotations: &crate::annotations::ConversationAnnotations,
    id: &str,
) -> Option<String> {
    annotations
        .session
        .iter()
        .chain(annotations.positioned.iter())
        .find(|annotation| annotation.id == id)
        .map(|annotation| annotation.text.clone())
}

#[cfg(test)]
mod annotation_copy_tests {
    use super::annotation_text;
    use crate::annotations::{Annotation, ConversationAnnotations, TargetSpan};

    fn note(id: &str, targets: Vec<TargetSpan>, text: &str) -> Annotation {
        Annotation {
            id: id.to_string(),
            targets,
            kind: "note".to_string(),
            text: text.to_string(),
            annotator: "file".to_string(),
        }
    }

    #[test]
    fn the_selected_note_resolves_to_its_own_text_only() {
        let annotations = ConversationAnnotations::from_flat(vec![
            note("s1", Vec::new(), "session-level remark"),
            note("p1", vec![TargetSpan::single(4)], "the line four remark"),
        ]);

        assert_eq!(
            annotation_text(&annotations, "p1").as_deref(),
            Some("the line four remark")
        );
        assert_eq!(
            annotation_text(&annotations, "s1").as_deref(),
            Some("session-level remark")
        );
        assert_eq!(annotation_text(&annotations, "missing"), None);
    }
}
