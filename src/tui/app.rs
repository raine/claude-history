use crate::config::KeyBindings;
use crate::history::{
    Conversation, Workspace, format_short_name_from_path, process_conversation_file,
};
use crate::search;
#[cfg(test)]
use crate::semantic::types::{SemanticExplanation, SemanticScoreBreakdown};
#[cfg(test)]
use crate::tui::semantic_worker::{SemanticSearchMessage, SemanticWorkerCommand};
#[cfg(test)]
use crate::tui::ui;
use crate::tui::viewer::ToolDisplayMode;
#[cfg(test)]
use crate::tui::viewer::ToolOutputId;
#[cfg(test)]
use chrono::Local;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

mod dialog_state;
mod input_controller;
mod list_preview;
mod list_state;
mod search_state;
mod types;
mod view_state;
#[allow(unused_imports)]
pub use list_preview::PreviewView;
#[allow(unused_imports)]
use search_state::{
    IndexScope, SearchCommand, SearchResponse, SemanticCorpus, SemanticSearchState,
    spawn_search_worker,
};
#[allow(unused_imports)]
pub use types::{
    Action, AppMode, DialogMode, LIST_LINES_PER_ITEM, ListSearchMode, Readiness, ReconcileState,
    SemanticProgress, SemanticResultMetadata, TuiSearchOptions, ViewFamily, ViewSearchMode,
    ViewState, list_lines_per_item,
};

/// Cards materialized for the first frame. Bounded so the first frame costs
/// the same whatever the corpus size; the full list follows once the index
/// is open.
const SNAPSHOT_ROWS: usize = 500;

/// Card that is not a generation doc, e.g. a session found by UUID.
const NO_DOC: u32 = u32::MAX;

/// App state
pub struct App {
    /// Rows of the list. In generation mode (`snapshot` set) these are the
    /// cards materialized so far, without text, in the order first needed;
    /// otherwise every loaded conversation, shared with the search worker.
    conversations: Arc<Vec<Conversation>>,
    /// Generation doc of each row in generation mode, or `NO_DOC`
    row_docs: Vec<u32>,
    /// Sessions deleted from this TUI, hidden until a generation without
    /// them is shown, by `key_hash`
    removed: HashSet<u64>,
    /// Titles set from this TUI, applied to cards until a generation holds
    /// them, by path
    renamed: HashMap<PathBuf, Option<String>>,
    /// Set when the session files changed and the store should reconcile
    reconcile_requested: bool,
    /// Full conversations for semantic search in generation mode, loaded
    /// on first use
    semantic_corpus: SemanticCorpus,
    /// Shared semantic conversation snapshot for background workers
    semantic_conversations_snapshot: Arc<Vec<Arc<Conversation>>>,
    /// Version of the semantic corpus snapshot
    semantic_corpus_version: u64,
    /// Version of the semantic scope snapshot
    semantic_scope_version: u64,
    /// Last corpus version sent to the semantic worker
    semantic_sent_corpus_version: u64,
    /// Last scope signature sent to the semantic worker
    semantic_sent_scope_signature: Option<(u64, Arc<Vec<usize>>)>,
    /// Indices into conversations, sorted by current score
    filtered: Vec<usize>,
    /// Currently selected index into filtered (None if no results)
    selected: Option<usize>,
    /// Session path to restore after replacing and re-filtering the corpus
    selection_anchor: Option<PathBuf>,
    /// Current search query
    query: String,
    /// Cursor position in query (character index, not byte)
    cursor_pos: usize,
    /// Which parts of the list are usable
    readiness: Readiness,
    /// Generation whose cards are the rows, in generation mode
    snapshot: Option<crate::store::Snapshot>,
    /// Generation index lexical queries run against, when there is one
    search_index: Option<Arc<crate::store::SearchIndex>>,
    /// Row of each card of the snapshot, once materialized
    doc_rows: Vec<Option<usize>>,
    /// Rows whose evidence was computed for display, found or not
    evidence_checked: HashSet<usize>,
    /// Current dialog overlay (confirm, menu)
    dialog_mode: DialogMode,
    /// Main app mode (list or view)
    app_mode: AppMode,
    /// Views left by opening a related conversation from the viewer, most
    /// recent last; leaving the viewer returns to them first
    view_stack: Vec<ViewState>,
    /// Conversations related to the one in the viewer
    view_family: ViewFamily,
    /// Selected search result as rendered in the list's preview pane
    list_preview: list_preview::ListPreviewState,
    /// Status message with timestamp for auto-clear
    status_message: Option<(String, std::time::Instant)>,
    /// Progress of the reconcile pass shown as indexing
    indexing: Option<crate::store::progress::Indexing>,
    /// Persistent view setting: tool display mode
    tool_display: ToolDisplayMode,
    /// Persistent view setting: whether to show thinking blocks
    show_thinking: bool,
    /// Persistent view setting: whether to show timing information
    show_timing: bool,
    /// Whether the app is running in single file mode (direct input, no list)
    single_file_mode: bool,
    /// Configurable keybindings
    keys: KeyBindings,
    /// Whether the delete key may delete conversations (`[safety] disable_delete`)
    delete_enabled: bool,
    /// Whether workspace filter is active (only show current project's conversations)
    workspace_filter: bool,
    /// Whether automated and child conversations are listed
    all_kinds: bool,
    /// Whether lexical search matches only the text the person typed
    user_only: bool,
    /// The directory the project scope is anchored to
    workspace: Option<Workspace>,
    /// Exact project names hidden from list-mode display
    excluded_projects: HashSet<String>,
    /// Working-directory prefixes hidden from list-mode display
    excluded_paths: Vec<String>,
    /// Channel to send commands to the background search worker
    search_tx: mpsc::Sender<SearchCommand>,
    /// Channel to receive results from the background search worker
    search_rx: mpsc::Receiver<SearchResponse>,
    /// Monotonic generation counter for search requests
    search_generation: u64,
    /// Whether a search is currently in-flight on the worker thread
    search_in_flight: bool,
    /// Whether `filtered` is the first page of an index search still being
    /// completed
    search_page_shown: bool,
    /// Current list search mode
    list_search_mode: ListSearchMode,
    /// Semantic TUI state
    semantic_search: SemanticSearchState,
    /// Cached lexical evidence produced outside the render path
    lexical_evidence: HashMap<usize, search::LexicalEvidence>,
}

struct AppParts {
    conversations: Arc<Vec<Conversation>>,
    semantic_conversations_snapshot: Arc<Vec<Arc<Conversation>>>,
    filtered: Vec<usize>,
    selected: Option<usize>,
    readiness: Readiness,
    app_mode: AppMode,
    tool_display: ToolDisplayMode,
    show_thinking: bool,
    single_file_mode: bool,
    keys: KeyBindings,
    workspace_filter: bool,
    all_kinds: bool,
    workspace: Option<Workspace>,
    excluded_projects: HashSet<String>,
    excluded_paths: Vec<String>,
    search_tx: mpsc::Sender<SearchCommand>,
    search_rx: mpsc::Receiver<SearchResponse>,
    list_search_mode: ListSearchMode,
    semantic_search: SemanticSearchState,
}

impl App {
    fn from_parts(parts: AppParts) -> Self {
        Self {
            semantic_conversations_snapshot: parts.semantic_conversations_snapshot,
            semantic_corpus_version: 1,
            semantic_scope_version: 0,
            semantic_sent_corpus_version: 0,
            semantic_sent_scope_signature: None,
            conversations: parts.conversations,
            row_docs: Vec::new(),
            removed: HashSet::new(),
            renamed: HashMap::new(),
            reconcile_requested: false,
            semantic_corpus: SemanticCorpus::default(),
            filtered: parts.filtered,
            selected: parts.selected,
            selection_anchor: None,
            query: String::new(),
            cursor_pos: 0,
            readiness: parts.readiness,
            snapshot: None,
            search_index: None,
            doc_rows: Vec::new(),
            evidence_checked: HashSet::new(),
            dialog_mode: DialogMode::None,
            app_mode: parts.app_mode,
            view_stack: Vec::new(),
            view_family: ViewFamily::default(),
            list_preview: Default::default(),
            status_message: None,
            indexing: None,
            tool_display: parts.tool_display,
            show_thinking: parts.show_thinking,
            show_timing: false,
            single_file_mode: parts.single_file_mode,
            keys: parts.keys,
            delete_enabled: true,
            workspace_filter: parts.workspace_filter,
            all_kinds: parts.all_kinds,
            user_only: false,
            workspace: parts.workspace,
            excluded_projects: parts.excluded_projects,
            excluded_paths: parts.excluded_paths,
            search_tx: parts.search_tx,
            search_rx: parts.search_rx,
            search_generation: 0,
            search_in_flight: false,
            search_page_shown: false,
            list_search_mode: parts.list_search_mode,
            semantic_search: parts.semantic_search,
            lexical_evidence: HashMap::new(),
        }
    }

    fn semantic_snapshot(conversations: &[Conversation]) -> Arc<Vec<Arc<Conversation>>> {
        Arc::new(conversations.iter().cloned().map(Arc::new).collect())
    }

    fn list_search_mode_from_options(search_options: TuiSearchOptions) -> ListSearchMode {
        search_options.default_mode
    }

    fn semantic_search_state(available: bool) -> SemanticSearchState {
        SemanticSearchState {
            available,
            ..Default::default()
        }
    }

    /// Create a new app with all conversations pre-loaded
    #[allow(dead_code)]
    pub fn new(
        conversations: Vec<Conversation>,
        tool_display: ToolDisplayMode,
        show_thinking: bool,
        keys: KeyBindings,
        exclude_projects: Vec<String>,
    ) -> Self {
        Self::new_with_options(
            conversations,
            tool_display,
            show_thinking,
            keys,
            exclude_projects,
            TuiSearchOptions::default(),
        )
    }

    #[allow(dead_code)]
    pub fn new_with_options(
        conversations: Vec<Conversation>,
        tool_display: ToolDisplayMode,
        show_thinking: bool,
        keys: KeyBindings,
        exclude_projects: Vec<String>,
        search_options: TuiSearchOptions,
    ) -> Self {
        let excluded_projects = exclude_projects.into_iter().collect();
        let filtered = list_state::filter_conversation_indices(
            0..conversations.len(),
            &conversations,
            &excluded_projects,
            &[],
            search_options.all_kinds,
            None,
        );
        let selected = if filtered.is_empty() { None } else { Some(0) };
        let (search_tx, search_rx) = spawn_search_worker();
        let semantic_conversations_snapshot = Self::semantic_snapshot(&conversations);
        let conversations = Arc::new(conversations);
        let _ = search_tx.send(SearchCommand::UpdateData {
            conversations: conversations.clone(),
        });

        Self::from_parts(AppParts {
            semantic_conversations_snapshot,
            conversations,
            filtered,
            selected,
            readiness: Readiness::ready(),
            app_mode: AppMode::List,
            tool_display,
            show_thinking,
            single_file_mode: false,
            keys,
            workspace_filter: false,
            all_kinds: search_options.all_kinds,
            workspace: None,
            excluded_projects,
            excluded_paths: Vec::new(),
            search_tx,
            search_rx,
            list_search_mode: Self::list_search_mode_from_options(search_options),
            semantic_search: Self::semantic_search_state(true),
        })
    }

    /// Create a new app in loading state
    #[allow(clippy::too_many_arguments)]
    pub fn new_loading_with_options(
        tool_display: ToolDisplayMode,
        show_thinking: bool,
        keys: KeyBindings,
        workspace_filter: bool,
        workspace: Option<Workspace>,
        exclude_projects: Vec<String>,
        exclude_paths: Vec<String>,
        search_options: TuiSearchOptions,
    ) -> Self {
        let (search_tx, search_rx) = spawn_search_worker();

        Self::from_parts(AppParts {
            semantic_conversations_snapshot: Arc::default(),
            conversations: Arc::default(),
            filtered: Vec::new(),
            selected: None,
            readiness: Readiness::default(),
            app_mode: AppMode::List,
            tool_display,
            show_thinking,
            single_file_mode: false,
            keys,
            workspace_filter,
            all_kinds: search_options.all_kinds,
            workspace,
            excluded_projects: exclude_projects.into_iter().collect(),
            excluded_paths: exclude_paths,
            search_tx,
            search_rx,
            list_search_mode: Self::list_search_mode_from_options(search_options),
            semantic_search: Self::semantic_search_state(true),
        })
    }

    /// Create a new app for viewing a single file directly
    pub fn new_single_file(
        path: PathBuf,
        tool_display: ToolDisplayMode,
        show_thinking: bool,
        keys: KeyBindings,
    ) -> Self {
        let (search_tx, search_rx) = spawn_search_worker();

        // Parse using the same parser as the main list
        let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();

        let mut conversations = Vec::new();
        let mut filtered = Vec::new();
        let mut selected = None;

        if let Ok(Some(mut conv)) = process_conversation_file(path.clone(), modified, None) {
            // Set project_name the same way as the loader does
            let project_path = conv.cwd.clone().unwrap_or_else(|| path.clone());
            conv.project_name = Some(format_short_name_from_path(&project_path));

            conversations.push(conv);
            filtered.push(0);
            selected = Some(0);
        }

        Self::from_parts(AppParts {
            semantic_conversations_snapshot: Self::semantic_snapshot(&conversations),
            conversations: Arc::new(conversations),
            filtered,
            selected,
            readiness: Readiness::ready(),
            app_mode: AppMode::View(ViewState::initial(
                path,
                tool_display,
                show_thinking,
                false,
                0,
            )),
            tool_display,
            show_thinking,
            single_file_mode: true,
            keys,
            workspace_filter: false,
            all_kinds: false,
            workspace: None,
            excluded_projects: HashSet::new(),
            excluded_paths: Vec::new(),
            search_tx,
            search_rx,
            list_search_mode: ListSearchMode::Lexical,
            semantic_search: Self::semantic_search_state(false),
        })
    }

    pub fn keys(&self) -> &KeyBindings {
        &self.keys
    }

    pub fn delete_enabled(&self) -> bool {
        self.delete_enabled
    }

    pub fn set_delete_enabled(&mut self, enabled: bool) {
        self.delete_enabled = enabled;
    }

    /// Shows the cards of `snapshot`: rows come from its cards from now on
    /// (generation mode). Only the newest cards are materialized for the
    /// first frame; `open_search_index` lists the rest.
    pub fn show_snapshot(&mut self, snapshot: crate::store::Snapshot) {
        self.reset_rows(snapshot);
        self.show_cards(SNAPSHOT_ROWS);
        self.readiness.list = true;
        if self.list_search_mode == ListSearchMode::Semantic {
            self.ensure_semantic_corpus();
        }
    }

    /// Shows `conversations`, loaded in full, as the rows, keeping the
    /// query and the selected session.
    pub fn show_conversations(&mut self, mut conversations: Vec<Conversation>) {
        let mut paths = HashSet::new();
        conversations.retain(|conversation| {
            let path = conversation
                .path
                .canonicalize()
                .unwrap_or_else(|_| conversation.path.clone());
            paths.insert(path)
        });
        conversations.sort_by_key(|conversation| std::cmp::Reverse(conversation.timestamp));
        for (index, conversation) in conversations.iter_mut().enumerate() {
            conversation.index = index;
        }

        if self.selection_anchor.is_none() {
            self.selection_anchor = self.get_selected_path();
        }
        self.snapshot = None;
        self.row_docs.clear();
        self.lexical_evidence.clear();
        self.conversations = Arc::new(conversations);
        self.filtered.clear();
        self.selected = None;
        self.readiness = Readiness::ready();
        self.refresh_view_family();
        // The index holds other conversations.
        self.drop_search_index();
        self.refresh_search_data();

        if self.query.trim().is_empty() {
            self.update_filter();
            self.prewarm_semantic_cache();
        } else {
            // Keep the list populated until the worker answers the query.
            let anchor = self.selection_anchor.clone();
            self.apply_filtered(self.filter_indices(0..self.conversations.len()));
            self.selection_anchor = anchor;
            self.dispatch_search();
        }
    }

    /// Whether no card of the generation shown is inside the time filter.
    pub fn generation_is_empty(&self) -> bool {
        self.snapshot.as_ref().is_none_or(|snapshot| {
            snapshot
                .head(1, self.all_kinds, &[], None, |_| true)
                .is_empty()
        })
    }

    /// Makes `snapshot` the source of rows, dropping rows of the previous
    /// one.
    fn reset_rows(&mut self, snapshot: crate::store::Snapshot) {
        self.doc_rows = vec![None; snapshot.len()];
        self.conversations = Arc::default();
        self.row_docs.clear();
        self.lexical_evidence.clear();
        self.evidence_checked.clear();
        self.snapshot = Some(snapshot);
    }

    /// Lists the newest `limit` cards in scope.
    fn show_cards(&mut self, limit: usize) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let docs = snapshot.head(
            limit,
            self.all_kinds,
            &self.excluded_paths,
            self.scope_workspace(),
            |project| list_state::card_project_visible(project, &self.excluded_projects),
        );
        let docs: Vec<u32> = docs
            .into_iter()
            .filter(|&doc| !self.is_removed(doc))
            .collect();
        let filtered = docs.into_iter().map(|doc| self.card_row(doc)).collect();
        self.apply_filtered(filtered);
    }

    fn is_removed(&self, doc: u32) -> bool {
        !self.removed.is_empty()
            && self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| self.removed.contains(&snapshot.key_hash(doc)))
    }

    /// The workspace rows must belong to, while the project scope is on.
    pub(super) fn scope_workspace(&self) -> Option<&Workspace> {
        self.workspace.as_ref().filter(|_| self.workspace_filter)
    }

    /// Index in `conversations` of snapshot card `doc`, materializing it on
    /// first use.
    fn card_row(&mut self, doc: u32) -> usize {
        if let Some(row) = self.doc_rows[doc as usize] {
            return row;
        }
        let snapshot = self.snapshot.as_ref().expect("cards come from a snapshot");
        let mut card = snapshot.card(doc);
        if let Some(title) = self.renamed.get(&card.path) {
            card.custom_title = title.clone();
        }
        let row = self.conversations.len();
        card.index = row;
        Arc::make_mut(&mut self.conversations).push(card);
        self.row_docs.push(doc);
        self.doc_rows[doc as usize] = Some(row);
        row
    }

    /// Row of the card stored at `path`, if the generation has one.
    pub(super) fn row_of_path(&mut self, path: &std::path::Path) -> Option<usize> {
        if let Some(row) = self.conversations.iter().position(|c| c.path == path) {
            return Some(row);
        }
        let doc = self.snapshot.as_ref()?.find(path)?;
        Some(self.card_row(doc))
    }

    /// Opens the index of the snapshot being shown, so lexical queries run,
    /// and lists every card in scope. Returns whether an index was opened.
    pub fn open_search_index(&mut self) -> bool {
        let Some(snapshot) = &self.snapshot else {
            return false;
        };
        let opened = match crate::store::SearchIndex::open(snapshot.clone()) {
            Some(index) => {
                self.search_index = Some(Arc::new(index));
                self.send_index_scope();
                self.readiness.lexical = true;
                true
            }
            None => false,
        };
        if self.query.trim().is_empty() || !opened {
            if self.selection_anchor.is_none() {
                self.selection_anchor = self.get_selected_path();
            }
            self.apply_browse_filter();
        } else {
            self.dispatch_search();
        }
        opened
    }

    /// Shows `snapshot` if it is a newer generation than the one shown,
    /// keeping the query, scope and selected session. Returns whether it
    /// did.
    pub fn swap_generation(&mut self, snapshot: crate::store::Snapshot) -> bool {
        if self
            .snapshot
            .as_ref()
            .is_some_and(|shown| shown.generation_name() == snapshot.generation_name())
        {
            return false;
        }
        if self.selection_anchor.is_none() {
            self.selection_anchor = self.get_selected_path();
        }
        let viewed = match &self.app_mode {
            AppMode::View(state) => Some(state.conversation_path.clone()),
            AppMode::List => None,
        };
        // The new generation holds whatever this TUI changed, unless it was
        // published before the change.
        self.removed.retain(|key| snapshot.contains_key(*key));
        self.renamed.retain(|path, title| {
            snapshot
                .find(path)
                .is_none_or(|doc| snapshot.card(doc).custom_title != *title)
        });
        // Rows shown stay listed, as rows of the new generation, until the
        // query has run on it.
        let shown: Vec<u64> = match &self.snapshot {
            Some(old) => self
                .filtered
                .iter()
                .filter_map(|&row| self.row_docs.get(row).copied())
                .filter(|&doc| doc != NO_DOC)
                .map(|doc| old.key_hash(doc))
                .collect(),
            None => Vec::new(),
        };
        self.reset_rows(snapshot.clone());
        self.readiness.list = true;
        self.search_index = None;
        self.semantic_search.results.clear();
        if viewed.is_some() {
            self.refresh_view_family();
        }
        let docs: Vec<u32> = shown
            .iter()
            .filter_map(|key| snapshot.docs_by_key().get(key).copied())
            .filter(|&doc| !self.is_removed(doc))
            .collect();
        self.filtered = docs.into_iter().map(|doc| self.card_row(doc)).collect();
        self.selected = self
            .selection_anchor
            .as_ref()
            .and_then(|path| {
                self.filtered
                    .iter()
                    .position(|&row| self.conversations[row].path == *path)
            })
            .or((!self.filtered.is_empty()).then_some(0));
        self.invalidate_search_generation();
        self.semantic_sent_scope_signature = None;
        self.open_search_index();
        true
    }

    /// Number of cards in the generation shown, in generation mode.
    pub fn generation_len(&self) -> Option<usize> {
        self.snapshot.as_ref().map(crate::store::Snapshot::len)
    }

    /// Asks for a reconcile pass; see `take_reconcile_request`.
    pub(super) fn request_reconcile(&mut self) {
        self.reconcile_requested = self.snapshot.is_some();
    }

    /// Whether the session files changed since the last call, so the store
    /// should reconcile.
    pub fn take_reconcile_request(&mut self) -> bool {
        std::mem::take(&mut self.reconcile_requested)
    }

    /// Sends the index and the cards in scope to the search worker.
    pub(super) fn send_index_scope(&mut self) {
        let Some(index) = &self.search_index else {
            return;
        };
        let mut docs = index.cards().scope(
            self.all_kinds,
            &self.excluded_paths,
            self.scope_workspace(),
            |project| list_state::card_project_visible(project, &self.excluded_projects),
        );
        if !self.removed.is_empty() {
            for (doc, in_scope) in docs.iter_mut().enumerate() {
                *in_scope &= !self.is_removed(doc as u32);
            }
        }
        let _ = self
            .search_tx
            .send(SearchCommand::UseIndex(Some(IndexScope {
                index: index.clone(),
                docs: Arc::new(docs),
            })));
    }

    /// Rows of index cards `docs`, in order, without removed cards.
    pub(super) fn rows_of_docs(&mut self, docs: &[usize]) -> Vec<usize> {
        let docs: Vec<u32> = docs
            .iter()
            .map(|&doc| doc as u32)
            .filter(|&doc| !self.is_removed(doc))
            .collect();
        docs.into_iter().map(|doc| self.card_row(doc)).collect()
    }

    /// Computes lexical evidence for the rows shown on the current page
    /// that the search worker left without any, from their text frames.
    pub fn fill_visible_evidence(&mut self, items_per_page: usize) {
        let (Some(index), Some(_)) = (&self.search_index, &self.snapshot) else {
            return;
        };
        if self.list_search_mode != ListSearchMode::Lexical
            || self.search_in_flight
            || items_per_page == 0
        {
            return;
        }
        let parsed = search::query::ParsedQuery::parse(self.query.trim());
        if parsed.is_effectively_empty() {
            return;
        }
        let offset = self
            .selected
            .map_or(0, |sel| sel / items_per_page * items_per_page);
        let rows: Vec<usize> = self
            .filtered
            .iter()
            .skip(offset)
            .take(items_per_page)
            .copied()
            .filter(|row| {
                !self.lexical_evidence.contains_key(row) && !self.evidence_checked.contains(row)
            })
            .collect();
        for row in rows {
            self.evidence_checked.insert(row);
            let doc = self.row_docs.get(row).copied().unwrap_or(NO_DOC);
            if doc == NO_DOC {
                continue;
            }
            let evidence = if self.searches_user_only() {
                search::build_lexical_evidence(&index.own_text(doc), "", &parsed)
            } else {
                search::build_lexical_evidence(
                    &index.text(doc),
                    &self.conversations[row].preview,
                    &parsed,
                )
            };
            if let Some(evidence) = evidence {
                self.lexical_evidence.insert(row, evidence);
            }
        }
    }

    /// Stops searching the index; queries then run over the loaded
    /// conversations.
    fn drop_search_index(&mut self) {
        if self.search_index.take().is_some() {
            self.doc_rows.clear();
            let _ = self.search_tx.send(SearchCommand::UseIndex(None));
        }
    }

    /// The conversation of the row stored at `path`, for acting on it after
    /// the TUI exits.
    pub fn conversation_at(&self, path: &std::path::Path) -> Option<Conversation> {
        self.conversations.iter().find(|c| c.path == path).cloned()
    }

    pub fn set_status_message(&mut self, message: String) {
        self.status_message = Some((message, std::time::Instant::now()));
    }

    pub fn readiness(&self) -> &Readiness {
        &self.readiness
    }

    /// Whether there are no rows yet, so only typing works.
    pub fn is_loading(&self) -> bool {
        !self.readiness.list
    }

    /// Records whether a reconcile pass is running.
    pub fn set_reconciling(&mut self, running: bool) {
        self.readiness.reconcile = if running {
            ReconcileState::Running
        } else {
            ReconcileState::Done
        };
    }

    /// Records the progress the running reconcile pass reports, if any,
    /// and whether a pass still runs. The indicator stays up until no pass
    /// runs, and a pass that was shown then leaves a completion message.
    pub fn set_index_progress(
        &mut self,
        progress: Option<crate::store::progress::Indexing>,
        running: bool,
    ) {
        if progress.is_some() {
            self.indexing = progress;
        } else if !running && let Some(shown) = self.indexing.take() {
            self.set_status_message(format!("Indexed {} sessions", group_digits(shown.total)));
        }
    }

    /// Reports a reconcile pass that failed with `message`.
    pub fn index_failed(&mut self, message: &str) {
        self.indexing = None;
        self.set_status_message(format!("Indexing failed: {message}"));
    }

    /// Bottom bar text while a reconcile pass is shown as indexing.
    pub fn indexing_status_text(&self) -> Option<String> {
        let progress = self.indexing?;
        let mut text = if progress.done < progress.total {
            format!(
                "Indexing sessions\u{2026} {} / {}",
                group_digits(progress.done),
                group_digits(progress.total)
            )
        } else {
            format!(
                "Finishing index\u{2026} {} sessions",
                group_digits(progress.total)
            )
        };
        if !self.query.trim().is_empty() {
            text.push_str("  \u{b7}  results may be incomplete");
        }
        Some(text)
    }

    // Getters for UI access
    pub fn filtered(&self) -> &[usize] {
        &self.filtered
    }

    pub fn conversations(&self) -> &[Conversation] {
        &self.conversations
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn dialog_mode(&self) -> &DialogMode {
        &self.dialog_mode
    }

    #[cfg(test)]
    pub fn set_dialog_mode_for_test(&mut self, dialog_mode: DialogMode) {
        self.dialog_mode = dialog_mode;
    }

    pub fn app_mode(&self) -> &AppMode {
        &self.app_mode
    }

    pub fn status_message(&self) -> Option<&(String, std::time::Instant)> {
        self.status_message.as_ref()
    }

    /// Returns how long until the active status message expires, if any
    pub fn status_message_remaining(&self) -> Option<Duration> {
        const STATUS_TTL: Duration = Duration::from_secs(3);
        self.status_message
            .as_ref()
            .and_then(|(_, instant)| STATUS_TTL.checked_sub(instant.elapsed()))
    }

    pub fn cursor_pos(&self) -> usize {
        self.cursor_pos
    }

    pub fn is_single_file_mode(&self) -> bool {
        self.single_file_mode
    }

    pub(crate) fn show_timing(&self) -> bool {
        self.show_timing
    }

    pub fn workspace_filter(&self) -> bool {
        self.workspace_filter
    }

    pub fn all_kinds(&self) -> bool {
        self.all_kinds
    }

    pub fn list_search_mode(&self) -> ListSearchMode {
        self.list_search_mode
    }

    pub fn has_project_context(&self) -> bool {
        self.workspace.is_some()
    }

    /// The directory the project scope is anchored to, when there is one.
    pub fn workspace(&self) -> Option<&Workspace> {
        self.workspace.as_ref()
    }

    #[cfg(test)]
    pub fn semantic_search_available(&self) -> bool {
        self.semantic_search.available
    }

    pub fn semantic_toggle_available(&self) -> bool {
        self.semantic_search.available
            && !self
                .keys
                .rename
                .matches(KeyCode::Char('t'), KeyModifiers::CONTROL)
            && !self
                .keys
                .delete
                .matches(KeyCode::Char('t'), KeyModifiers::CONTROL)
            && !self
                .keys
                .resume
                .matches(KeyCode::Char('t'), KeyModifiers::CONTROL)
            && !self
                .keys
                .fork
                .matches(KeyCode::Char('t'), KeyModifiers::CONTROL)
            && !self
                .keys
                .refresh
                .matches(KeyCode::Char('t'), KeyModifiers::CONTROL)
    }

    /// Index into `filtered` of the list item drawn at `row`, if any.
    fn list_idx_at_row(&self, column: u16, row: u16, frame_area: Rect) -> Option<usize> {
        if !matches!(self.app_mode, AppMode::List)
            || self.dialog_mode != DialogMode::None
            || !self.readiness.list
        {
            return None;
        }

        let list = crate::tui::ui::list_layout_rects(frame_area, self).list;
        if !list.contains(Position::new(column, row)) {
            return None;
        }
        let list_y = list.y;

        let lines_per_item = list_lines_per_item(self.list_search_mode, &self.query);
        let items_per_page = (list.height as usize) / lines_per_item;
        if items_per_page == 0 {
            return None;
        }

        let offset = match self.selected {
            Some(sel) => (sel / items_per_page) * items_per_page,
            None => 0,
        };
        let relative_row = (row - list_y) as usize;
        let new_idx = offset + relative_row / lines_per_item;
        (new_idx < self.filtered.len()).then_some(new_idx)
    }

    /// Handle a left-click in list mode: select the conversation under the cursor.
    /// Returns true if the click landed on a list item — the caller is expected to
    /// then transition into view mode (matching the Enter-key behavior).
    pub fn handle_list_click(&mut self, column: u16, row: u16, frame_area: Rect) -> bool {
        let Some(idx) = self.list_idx_at_row(column, row, frame_area) else {
            return false;
        };
        self.clear_selection_anchor();
        self.selected = Some(idx);
        true
    }

    /// Handle mouse movement in list mode: select the conversation under the
    /// cursor so the highlight shows which one a click would open. Returns
    /// true if the selection changed.
    pub fn handle_list_mouse_move(&mut self, column: u16, row: u16, frame_area: Rect) -> bool {
        let Some(idx) = self.list_idx_at_row(column, row, frame_area) else {
            return false;
        };
        if self.selected == Some(idx) {
            return false;
        }
        self.clear_selection_anchor();
        self.selected = Some(idx);
        true
    }
}

/// `n` with its thousands separated by commas, e.g. "7,185".
fn group_digits(n: usize) -> String {
    let digits = n.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod semantic_test_helpers;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod interaction_tests;

#[cfg(test)]
mod snapshot_tests;
