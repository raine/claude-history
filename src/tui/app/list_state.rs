use super::{App, AppMode};
use crate::history::actions::{self, Capabilities};
use crate::history::path::project_is_excluded;
use crate::history::{Conversation, Workspace};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

impl App {
    pub(super) fn filter_indices<I>(&self, indices: I) -> Vec<usize>
    where
        I: IntoIterator<Item = usize>,
    {
        filter_conversation_indices(
            indices,
            &self.conversations,
            &self.excluded_projects,
            &self.excluded_paths,
            self.all_kinds,
            self.scope_workspace(),
            self.pinned_only,
            &self.pinned,
        )
    }

    pub(super) fn apply_filtered(&mut self, filtered: Vec<usize>) {
        self.filtered = filtered;
        self.selected = self.selection_anchor.take().and_then(|path| {
            self.filtered
                .iter()
                .position(|&index| self.conversations[index].path == path)
        });
        if self.selected.is_none() && !self.filtered.is_empty() {
            self.selected = Some(0);
        }
    }

    pub(super) fn clear_selection_anchor(&mut self) {
        self.selection_anchor = None;
    }

    pub(super) fn select_prev(&mut self) {
        self.clear_selection_anchor();
        if let Some(selected) = self.selected
            && selected > 0
        {
            self.selected = Some(selected - 1);
        }
    }

    pub(super) fn select_next(&mut self) {
        self.clear_selection_anchor();
        if let Some(selected) = self.selected
            && selected + 1 < self.filtered.len()
        {
            self.selected = Some(selected + 1);
        }
    }

    pub(super) fn select_first(&mut self) {
        self.clear_selection_anchor();
        if !self.filtered.is_empty() {
            self.selected = Some(0);
        }
    }

    pub(super) fn select_last(&mut self) {
        self.clear_selection_anchor();
        if !self.filtered.is_empty() {
            self.selected = Some(self.filtered.len() - 1);
        }
    }

    pub(super) fn select_page_up(&mut self) {
        self.clear_selection_anchor();
        if let Some(selected) = self.selected {
            self.selected = Some(selected.saturating_sub(10));
        }
    }

    pub(super) fn select_page_down(&mut self) {
        self.clear_selection_anchor();
        if let Some(selected) = self.selected {
            let new_selected = (selected + 10).min(self.filtered.len().saturating_sub(1));
            self.selected = Some(new_selected);
        }
    }

    pub(super) fn select_half_page_down(&mut self, viewport_height: usize) {
        self.clear_selection_anchor();
        if let Some(selected) = self.selected {
            let half_page = viewport_height / 2;
            let new_selected = (selected + half_page).min(self.filtered.len().saturating_sub(1));
            self.selected = Some(new_selected);
        }
    }

    pub(super) fn scroll_list(&mut self, delta: isize) {
        self.clear_selection_anchor();
        let Some(selected) = self.selected else {
            return;
        };

        let max = self.filtered.len().saturating_sub(1);
        let new_selected = if delta >= 0 {
            selected.saturating_add(delta as usize).min(max)
        } else {
            selected.saturating_sub((-delta) as usize)
        };
        self.selected = Some(new_selected);
    }

    /// Toggles whether the selected conversation is pinned, persisting the
    /// change to the user's pins file. Works the same in the list and in
    /// the viewer, like `get_selected_path`.
    pub(super) fn toggle_pin_selected(&mut self) {
        let Some(path) = self.get_selected_path() else {
            return;
        };
        let now_pinned = !self.pinned.contains(&path);
        match crate::pins::set_pinned(&path, now_pinned) {
            Ok(()) => {
                if now_pinned {
                    self.pinned.insert(path);
                } else {
                    self.pinned.remove(&path);
                }
                let message = if now_pinned { "Pinned" } else { "Unpinned" };
                self.set_status_message(message.to_string());
            }
            Err(error) => {
                self.set_status_message(format!("Could not save pin: {error}"));
            }
        }
    }

    /// Path of the selected conversation: the one in the viewer while it
    /// is open, else the list's selection.
    pub(super) fn get_selected_path(&self) -> Option<PathBuf> {
        self.get_selected_conversation_index()
            .map(|idx| self.conversations[idx].path.clone())
    }

    pub(crate) fn get_selected_source(&self) -> Option<crate::history::model::SourceKind> {
        self.get_selected_conversation_index()
            .map(|index| self.conversations[index].source)
    }

    pub(crate) fn has_multiple_sources(&self) -> bool {
        if let Some(snapshot) = &self.snapshot {
            return snapshot.has_multiple_sources();
        }
        let sources = self
            .conversations
            .iter()
            .map(|conversation| conversation.source)
            .collect::<HashSet<_>>();
        sources.len() > 1
    }

    /// Whether the selected conversation allows the action `allowed`
    /// picks, saying why not in the status line when it does not.
    pub(super) fn selected_allows(
        &mut self,
        allowed: fn(&Capabilities) -> bool,
        doing: &str,
    ) -> bool {
        let Some(index) = self.get_selected_conversation_index() else {
            return false;
        };
        let conversation = &self.conversations[index];
        let (source, provenance) = (conversation.source, conversation.provenance);
        if allowed(&actions::capabilities_of(source, provenance)) {
            return true;
        }
        self.set_status_message(actions::refusal(source, provenance, doing));
        false
    }

    /// Whether the selected conversation can be resumed: a Claude sidecar
    /// resumes its session, which must still exist.
    pub(super) fn selected_resumable(&mut self) -> bool {
        if !self.selected_allows(|can| can.resume, "Resuming") {
            return false;
        }
        let Some(path) = self.get_selected_path() else {
            return false;
        };
        let parent = crate::history::sources::claude::parent_transcript(&path);
        if parent.is_some_and(|parent| !parent.exists()) {
            self.set_status_message("The session this came from no longer exists".to_string());
            return false;
        }
        true
    }

    /// Row of the selected conversation: the one in the viewer while it is
    /// open, else the list's selection.
    pub(super) fn get_selected_conversation_index(&self) -> Option<usize> {
        if let AppMode::View(state) = &self.app_mode
            && let Some(row) = self
                .conversations
                .iter()
                .position(|conversation| conversation.path == state.conversation_path)
        {
            return Some(row);
        }
        self.selected
            .and_then(|sel| self.filtered.get(sel))
            .copied()
    }

    pub(crate) fn remove_selected_from_list(&mut self) {
        let Some(conv_idx) = self.get_selected_conversation_index() else {
            return;
        };
        let selected = self.selected.unwrap_or(0);

        if let Some(snapshot) = &self.snapshot {
            // Rows stay put so row indices remain valid; the row is hidden
            // until a generation without it is shown.
            let doc = self
                .row_docs
                .get(conv_idx)
                .copied()
                .unwrap_or(super::NO_DOC);
            if doc != super::NO_DOC {
                self.removed.insert(snapshot.key_hash(doc));
            }
            self.filtered.retain(|&idx| idx != conv_idx);
            if self.filtered.is_empty() {
                self.selected = None;
            } else if selected >= self.filtered.len() {
                self.selected = Some(self.filtered.len() - 1);
            }
            self.send_index_scope();
            self.semantic_sent_scope_signature = None;
            self.request_reconcile();
            return;
        }

        Arc::make_mut(&mut self.conversations).remove(conv_idx);

        self.filtered.retain(|&idx| idx != conv_idx);
        for idx in &mut self.filtered {
            if *idx > conv_idx {
                *idx -= 1;
            }
        }

        if self.filtered.is_empty() {
            self.selected = None;
        } else if selected >= self.filtered.len() {
            self.selected = Some(self.filtered.len() - 1);
        }

        self.refresh_search_data();
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn filter_conversation_indices<I, C>(
    indices: I,
    conversations: &[C],
    excluded_projects: &HashSet<String>,
    excluded_paths: &[String],
    all_kinds: bool,
    workspace: Option<&Workspace>,
    pinned_only: bool,
    pinned: &HashSet<PathBuf>,
) -> Vec<usize>
where
    I: IntoIterator<Item = usize>,
    C: std::borrow::Borrow<Conversation>,
{
    indices
        .into_iter()
        .filter(|&idx| all_kinds || !conversations[idx].borrow().provenance.hidden_by_default())
        .filter(|&idx| {
            conversations[idx]
                .borrow()
                .project_name
                .as_deref()
                .is_none_or(|name| !project_is_excluded(name, excluded_projects))
        })
        .filter(|&idx| {
            !crate::history::path::cwd_is_excluded(
                conversations[idx].borrow().cwd.as_deref(),
                excluded_paths,
            )
        })
        .filter(|&idx| {
            workspace.is_none_or(|workspace| workspace.contains(conversations[idx].borrow()))
        })
        .filter(|&idx| !pinned_only || pinned.contains(&conversations[idx].borrow().path))
        .collect()
}

/// Card counterpart of the project filters of `filter_conversation_indices`:
/// whether `exclude_projects` leaves `project` visible.
pub(super) fn card_project_visible(
    project: &crate::store::ProjectView,
    excluded_projects: &HashSet<String>,
) -> bool {
    project
        .name
        .is_none_or(|name| !project_is_excluded(name, excluded_projects))
}
