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

    /// The file line the focused message sits on.
    ///
    /// The line arrives through two hops: `focused_message` indexes
    /// `message_ranges`, whose `entry_index` counts parsed entries after
    /// filtering rather than file lines, so the matching entry carries the line.
    /// No focused message yields no line.
    pub(super) fn focused_message_line(&self) -> Option<usize> {
        let AppMode::View(state) = &self.app_mode else {
            return None;
        };
        state
            .focused_message
            .and_then(|index| state.message_ranges.get(index))
            .and_then(|range| {
                let entries = state.parsed_entries.as_ref()?;
                entries
                    .iter()
                    .find(|entry| entry.entry_index == range.entry_index)
                    .map(|entry| entry.jsonl_line)
            })
    }

    /// The file lines of the entries the view currently renders, ascending and
    /// without repeats.
    ///
    /// The set is drawn from `message_ranges`, which holds one range per
    /// rendered block, rather than from every parsed entry. A summary record,
    /// and an entry the filters hide, carries a line and draws no row, so
    /// stepping onto it would move the note in the file while the screen held
    /// still.
    pub(super) fn rendered_entry_lines(&self) -> Vec<usize> {
        let AppMode::View(state) = &self.app_mode else {
            return Vec::new();
        };
        let Some(entries) = state.parsed_entries.as_ref() else {
            return Vec::new();
        };
        let mut lines: Vec<usize> = state
            .message_ranges
            .iter()
            .filter_map(|range| {
                entries
                    .iter()
                    .find(|entry| entry.entry_index == range.entry_index)
            })
            .map(|entry| entry.jsonl_line)
            .collect();
        lines.sort_unstable();
        lines.dedup();
        lines
    }

    /// Put the selected note under a move.
    ///
    /// A note already on a line starts there. A session note carries no line
    /// and stays as it is, so `m` on one leaves the viewer where it was.
    pub(super) fn start_annotation_move(&mut self) {
        let AppMode::View(state) = &self.app_mode else {
            return;
        };
        let Some(id) = state.focused_annotation.clone() else {
            return;
        };
        let Some(annotation) = find_annotation(&state.annotations, &id) else {
            return;
        };
        let Some(line) = annotation.anchor_line() else {
            return;
        };
        let original = annotation.targets.clone();
        let scroll_offset = state.scroll_offset;
        if let AppMode::View(state) = &mut self.app_mode {
            state.moving_annotation = Some(crate::tui::app::types::AnnotationMove {
                id,
                original,
                line,
                scroll_offset,
            });
        }
    }

    /// Step the note under a move to the neighbouring entry line.
    ///
    /// The note's targets move with it, so the transcript shows it at the new
    /// line as the key is pressed. The step stops at the first and last entry
    /// line. Nothing is written until the move is saved.
    pub(super) fn move_selected_annotation(&mut self, forward: bool, viewport_height: usize) {
        let lines = self.rendered_entry_lines();
        let AppMode::View(state) = &self.app_mode else {
            return;
        };
        let Some(moving) = state.moving_annotation.clone() else {
            return;
        };
        let next = match lines.iter().position(|candidate| *candidate == moving.line) {
            Some(index) if forward => lines.get(index + 1).copied(),
            Some(index) => index
                .checked_sub(1)
                .and_then(|prior| lines.get(prior).copied()),
            None if forward => lines
                .iter()
                .find(|candidate| **candidate > moving.line)
                .copied(),
            None => lines
                .iter()
                .rev()
                .find(|candidate| **candidate < moving.line)
                .copied(),
        };
        let Some(target) = next else {
            return;
        };
        if let AppMode::View(state) = &mut self.app_mode {
            retarget_annotation(&mut state.annotations, &moving.id, target);
            if let Some(moving) = state.moving_annotation.as_mut() {
                moving.line = target;
            }
        }
        self.re_render_view(viewport_height);
        self.scroll_annotation_into_view(&moving.id, viewport_height);
    }

    /// Write the note at the line the move reached and leave move mode.
    pub(super) fn commit_annotation_move(&mut self, viewport_height: usize) {
        let AppMode::View(state) = &self.app_mode else {
            return;
        };
        let Some(moving) = state.moving_annotation.clone() else {
            return;
        };
        let path = state.conversation_path.clone();
        let Some(annotation) = find_annotation(&state.annotations, &moving.id) else {
            return;
        };
        let replacement = crate::annotations::Annotation {
            id: crate::annotations::generated_id(),
            targets: shifted_targets(&moving.original, moving.line),
            kind: annotation.kind.clone(),
            text: annotation.text.clone(),
            annotator: String::new(),
            origin: annotation.origin.clone(),
            created: annotation.created.clone(),
            modified: Some(crate::annotations::now_rfc3339()),
        };
        let holder = annotation.annotator.clone();
        // The replacement is written before the original is removed, so a
        // failure part-way leaves the note present rather than lost.
        // A move is a modification, so it writes back to the annotator holding
        // the note rather than to the configured write target. `write_to`
        // governs new notes.
        let stored = match self.annotators().write_to_annotator(
            &holder,
            &path,
            &replacement,
            Some(&moving.id),
        ) {
            Ok(stored) => stored,
            Err(error) => {
                // The note returns to where the move started and the row names
                // the refusal, so a failed write reads as a refusal rather than
                // as a key that did nothing.
                self.cancel_annotation_move(viewport_height);
                self.status_message = Some((
                    format!("Note not moved: {error}"),
                    std::time::Instant::now(),
                ));
                return;
            }
        };
        // An annotator acting on `replaces` returns the id it kept and holds
        // one record. A differing id means a second record was stored, so the
        // superseded one is removed.
        if stored != moving.id {
            let _ = self.annotators().delete(&path, &moving.id, &holder);
        }
        let annotations = self.annotators().read_one(&path);
        if let AppMode::View(state) = &mut self.app_mode {
            state.annotations = annotations;
            state.moving_annotation = None;
            state.focused_annotation = None;
        }
        self.refresh_annotation_count(&path);
        self.refresh_conversation_annotations(&path);
        self.re_render_view(viewport_height);
    }

    /// Put the note back where the move started and leave move mode.
    pub(super) fn cancel_annotation_move(&mut self, viewport_height: usize) {
        let AppMode::View(state) = &mut self.app_mode else {
            return;
        };
        let Some(moving) = state.moving_annotation.take() else {
            return;
        };
        restore_annotation_targets(&mut state.annotations, &moving.id, moving.original);
        self.re_render_view(viewport_height);
        // The viewport returns after the re-render, which is what settles the
        // line count the offset is clamped against.
        if let AppMode::View(state) = &mut self.app_mode {
            let max_scroll = state.total_lines.saturating_sub(viewport_height);
            state.scroll_offset = moving.scroll_offset.min(max_scroll);
        }
    }

    /// Scroll the rendered block for one note into view.
    fn scroll_annotation_into_view(&mut self, id: &str, viewport_height: usize) {
        let AppMode::View(state) = &mut self.app_mode else {
            return;
        };
        let Some(range) = state
            .annotation_ranges
            .iter()
            .find(|range| range.id == id)
            .cloned()
        else {
            return;
        };
        let max_scroll = state.total_lines.saturating_sub(viewport_height);
        if range.start_line < state.scroll_offset
            || range.start_line >= state.scroll_offset + viewport_height
        {
            state.scroll_offset = range.start_line.min(max_scroll);
        }
    }

    /// Open the annotate prompt for the conversation being viewed.
    ///
    /// The prompt opens on the focused message's line. With no focused message
    /// the annotation attaches to the session.
    pub(super) fn start_annotate(&mut self) {
        if !matches!(self.app_mode, AppMode::View(_)) {
            return;
        }
        let line = self.focused_message_line();
        self.dialog_mode = DialogMode::Annotate {
            input: String::new(),
            cursor: 0,
            line,
            anchor: line,
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
        // A note already on a line returns to that line; a session note returns
        // to the focused message, so Tab has a target in both directions.
        let anchor = line.or_else(|| self.focused_message_line());
        self.dialog_mode = DialogMode::Annotate {
            input,
            cursor,
            line,
            anchor,
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
        let Some(annotation) = find_annotation(&state.annotations, id) else {
            return;
        };
        let text = annotation_yank_text(annotation, &state.conversation_path);
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

        let replaced = replacing.as_ref().and_then(|id| {
            state
                .annotations
                .session
                .iter()
                .chain(state.annotations.positioned.iter())
                .find(|annotation| annotation.id == *id)
        });
        let replaced_annotator = replaced.map(|annotation| annotation.annotator.clone());
        // An edit carries the original's creation stamp forward, so the pair
        // bounds the note's life rather than restarting at every edit. A note
        // written before stamps existed carries none, and the edit stamps both.
        let stamp = crate::annotations::now_rfc3339();
        let created = replaced
            .and_then(|annotation| annotation.created.clone())
            .unwrap_or_else(|| stamp.clone());

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
            origin: None,
            created: Some(created),
            modified: Some(stamp),
        };
        // An edit writes back to the annotator holding the note; a new note
        // goes to the configured write target.
        let written = match &replaced_annotator {
            Some(holder) => self.annotators().write_to_annotator(
                holder,
                &path,
                &annotation,
                replacing.as_deref(),
            ),
            None => self.annotators().write(&path, &annotation, None),
        };
        let stored = match written {
            Ok(stored) => stored,
            Err(error) => {
                self.status_message = Some((
                    format!("Note not saved: {error}"),
                    std::time::Instant::now(),
                ));
                return;
            }
        };
        // An annotator acting on `replaces` returns the id it kept and holds
        // one record. A differing id means a second record was stored, so the
        // superseded one is removed.
        if let Some(id) = replacing
            && stored != id
        {
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
            // Alt+Enter and Shift+Enter insert a newline; plain Enter saves.
            // Terminals that report neither modifier reach the save arm, which
            // is the behaviour the prompt had before newlines were accepted.
            KeyCode::Enter if modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) => {
                if let DialogMode::Annotate { input, cursor, .. } = &mut self.dialog_mode {
                    let byte_pos = input
                        .char_indices()
                        .nth(*cursor)
                        .map(|(pos, _)| pos)
                        .unwrap_or(input.len());
                    input.insert(byte_pos, '\n');
                    *cursor += 1;
                }
            }
            KeyCode::Enter => self.submit_annotate(viewport_height),
            // Tab moves the target between the anchored line and the session.
            // With no anchor there is one target and the key does nothing.
            KeyCode::Tab | KeyCode::BackTab => {
                if let DialogMode::Annotate { line, anchor, .. } = &mut self.dialog_mode
                    && anchor.is_some()
                {
                    *line = match line {
                        Some(_) => None,
                        None => *anchor,
                    };
                }
            }
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

/// The note with this id, from either the session-level or the positioned
/// notes of a conversation.
/// Point one note at `line`, replacing whatever it targeted.
///
/// The change is in memory only, so a move shows in the transcript before it
/// is written and leaves the store alone until it is saved.
fn retarget_annotation(
    annotations: &mut crate::annotations::ConversationAnnotations,
    id: &str,
    line: usize,
) {
    for annotation in annotations
        .session
        .iter_mut()
        .chain(annotations.positioned.iter_mut())
    {
        if annotation.id == id {
            annotation.targets = shifted_targets(&annotation.targets, line);
        }
    }
}

/// The targets a note holds once its first line sits at `line`.
///
/// Every span moves by the same distance, so a note covering three lines still
/// covers three after a move, and a note covering one still covers one. A note
/// with no target gains the single line, which is the degenerate case.
fn shifted_targets(
    targets: &[crate::annotations::TargetSpan],
    line: usize,
) -> Vec<crate::annotations::TargetSpan> {
    let Some(first) = targets.iter().map(|span| span.start).min() else {
        return vec![crate::annotations::TargetSpan::single(line)];
    };
    targets
        .iter()
        .map(|span| crate::annotations::TargetSpan {
            // Distances are held in signed space so a move upward does not wrap
            // a span's start below the first line of the file.
            start: (span.start + line).saturating_sub(first),
            end: (span.end + line).saturating_sub(first),
        })
        .collect()
}

/// Put one note's targets back to `original`, the state a cancelled move
/// returns the viewer to.
fn restore_annotation_targets(
    annotations: &mut crate::annotations::ConversationAnnotations,
    id: &str,
    original: Vec<crate::annotations::TargetSpan>,
) {
    for annotation in annotations
        .session
        .iter_mut()
        .chain(annotations.positioned.iter_mut())
    {
        if annotation.id == id {
            annotation.targets = original.clone();
        }
    }
}

fn find_annotation<'a>(
    annotations: &'a crate::annotations::ConversationAnnotations,
    id: &str,
) -> Option<&'a crate::annotations::Annotation> {
    annotations
        .session
        .iter()
        .chain(annotations.positioned.iter())
        .find(|annotation| annotation.id == id)
}

/// The clipboard text for a note: its text, then a locator naming the
/// annotator, the kind, the conversation and the lines targeted, and the
/// origin when the note carries one. A note pasted elsewhere then leads back
/// to the transcript it describes and to the file it summarised.
fn annotation_yank_text(
    annotation: &crate::annotations::Annotation,
    conversation: &std::path::Path,
) -> String {
    let target = match annotation.targets.as_slice() {
        [] => "session".to_string(),
        targets => targets
            .iter()
            .map(|span| {
                if span.start == span.end {
                    span.start.to_string()
                } else {
                    format!("{}..{}", span.start, span.end)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
    };
    let mut out = format!(
        "{}\n\n— {} · {} · {} @{target}",
        annotation.text.trim_end(),
        annotation.annotator,
        annotation.kind,
        conversation.display()
    );
    if let Some(origin) = &annotation.origin {
        out.push_str(&format!("\n  origin {}", origin.long()));
    }
    out
}

#[cfg(test)]
mod annotation_copy_tests {
    use super::{annotation_yank_text, find_annotation};
    use crate::annotations::{Annotation, AnnotationOrigin, ConversationAnnotations, TargetSpan};
    use std::path::{Path, PathBuf};

    fn note(id: &str, targets: Vec<TargetSpan>, text: &str) -> Annotation {
        Annotation {
            id: id.to_string(),
            targets,
            kind: "recap".to_string(),
            text: text.to_string(),
            annotator: "chsum".to_string(),
            origin: None,
            created: None,
            modified: None,
        }
    }

    #[test]
    fn the_selected_note_resolves_to_its_own_text_only() {
        let annotations = ConversationAnnotations::from_flat(vec![
            note("s1", Vec::new(), "session-level remark"),
            note("p1", vec![TargetSpan::single(4)], "the line four remark"),
        ]);

        assert_eq!(
            find_annotation(&annotations, "p1").map(|a| a.text.as_str()),
            Some("the line four remark")
        );
        assert_eq!(
            find_annotation(&annotations, "s1").map(|a| a.text.as_str()),
            Some("session-level remark")
        );
        assert!(find_annotation(&annotations, "missing").is_none());
    }

    #[test]
    fn a_yanked_note_carries_its_locator() {
        let annotation = note(
            "p1",
            vec![TargetSpan::single(443)],
            "Agent removed the wizard.\n",
        );

        let text = annotation_yank_text(&annotation, Path::new("/tmp/proj/session.jsonl"));

        assert_eq!(
            text,
            "Agent removed the wizard.\n\n— chsum · recap · /tmp/proj/session.jsonl @443"
        );
    }

    #[test]
    fn a_yanked_note_with_an_origin_names_the_file_it_summarised() {
        let mut annotation = note(
            "p1",
            vec![TargetSpan::single(443)],
            "Agent removed the wizard.",
        );
        annotation.origin = Some(AnnotationOrigin {
            path: PathBuf::from("/tmp/proj/subagents/agent-ac1cffaa.jsonl"),
            lines: "412..430".to_string(),
        });

        let text = annotation_yank_text(&annotation, Path::new("/tmp/proj/session.jsonl"));

        assert!(
            text.ends_with("\n  origin /tmp/proj/subagents/agent-ac1cffaa.jsonl @412..430"),
            "{text}"
        );
    }
}
