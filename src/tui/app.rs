use crate::config::KeyBindings;
use crate::history::{Conversation, format_short_name_from_path, process_conversation_file};
use crate::search::{self, SearchableConversation};
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
mod list_state;
mod search_state;
mod types;
mod view_state;
#[allow(unused_imports)]
use search_state::{SearchCommand, SearchResponse, SemanticSearchState, spawn_search_worker};
#[allow(unused_imports)]
pub use types::{
    Action, AppMode, DialogMode, LIST_LINES_PER_ITEM, ListSearchMode, LoadingState,
    SemanticProgress, SemanticResultMetadata, TuiSearchOptions, ViewSearchMode, ViewState,
    list_lines_per_item,
};

/// App state
pub struct App {
    /// All loaded conversations
    conversations: Vec<Conversation>,
    /// Shared search data snapshot for background workers
    conversations_snapshot: Arc<Vec<Conversation>>,
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
    /// Precomputed search data
    searchable: Vec<SearchableConversation>,
    /// Indices into conversations, sorted by current score
    filtered: Vec<usize>,
    /// Currently selected index into filtered (None if no results)
    selected: Option<usize>,
    /// Current search query
    query: String,
    /// Cursor position in query (character index, not byte)
    cursor_pos: usize,
    /// Loading state
    loading_state: LoadingState,
    /// Current dialog overlay (confirm, menu)
    dialog_mode: DialogMode,
    /// Main app mode (list or view)
    app_mode: AppMode,
    /// Status message with timestamp for auto-clear
    status_message: Option<(String, std::time::Instant)>,
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
    /// Whether workspace filter is active (only show current project's conversations)
    workspace_filter: bool,
    /// The encoded project directory name for the current workspace (for filtering)
    current_project_dir_name: Option<String>,
    /// Exact project names hidden from list-mode display
    excluded_projects: HashSet<String>,
    /// Channel to send commands to the background search worker
    search_tx: mpsc::Sender<SearchCommand>,
    /// Channel to receive results from the background search worker
    search_rx: mpsc::Receiver<SearchResponse>,
    /// Monotonic generation counter for search requests
    search_generation: u64,
    /// Whether a search is currently in-flight on the worker thread
    search_in_flight: bool,
    /// Current list search mode
    list_search_mode: ListSearchMode,
    /// Semantic TUI state
    semantic_search: SemanticSearchState,
    /// Cached lexical evidence produced outside the render path
    lexical_evidence: HashMap<usize, search::LexicalEvidence>,
    /// Root the annotation sidecars sit under, absent when no root resolves.
    annotations_root: Option<PathBuf>,
    /// Note count per sidecar path, read once at startup and refreshed for one
    /// conversation after a write. The list row states a count per row, and a
    /// per-row read would open one file per visible row on every frame.
    annotation_counts: HashMap<PathBuf, usize>,
    /// Whether note text has been appended to the corpus's lexical fields.
    /// The append is not idempotent, so a second pass over the same
    /// conversations would double every note's text and double its scoring
    /// weight.
    annotations_enriched: bool,
}

struct AppParts {
    conversations: Vec<Conversation>,
    conversations_snapshot: Arc<Vec<Conversation>>,
    semantic_conversations_snapshot: Arc<Vec<Arc<Conversation>>>,
    searchable: Vec<SearchableConversation>,
    filtered: Vec<usize>,
    selected: Option<usize>,
    loading_state: LoadingState,
    app_mode: AppMode,
    tool_display: ToolDisplayMode,
    show_thinking: bool,
    single_file_mode: bool,
    keys: KeyBindings,
    workspace_filter: bool,
    current_project_dir_name: Option<String>,
    excluded_projects: HashSet<String>,
    search_tx: mpsc::Sender<SearchCommand>,
    search_rx: mpsc::Receiver<SearchResponse>,
    list_search_mode: ListSearchMode,
    semantic_search: SemanticSearchState,
}

impl App {
    fn from_parts(parts: AppParts) -> Self {
        let annotations_root = crate::config::load_config()
            .ok()
            .and_then(|config| crate::config::annotations_root(&config));
        Self {
            conversations_snapshot: parts.conversations_snapshot,
            semantic_conversations_snapshot: parts.semantic_conversations_snapshot,
            semantic_corpus_version: 1,
            semantic_scope_version: 0,
            semantic_sent_corpus_version: 0,
            semantic_sent_scope_signature: None,
            conversations: parts.conversations,
            searchable: parts.searchable,
            filtered: parts.filtered,
            selected: parts.selected,
            query: String::new(),
            cursor_pos: 0,
            loading_state: parts.loading_state,
            dialog_mode: DialogMode::None,
            app_mode: parts.app_mode,
            status_message: None,
            tool_display: parts.tool_display,
            show_thinking: parts.show_thinking,
            show_timing: false,
            single_file_mode: parts.single_file_mode,
            keys: parts.keys,
            workspace_filter: parts.workspace_filter,
            current_project_dir_name: parts.current_project_dir_name,
            excluded_projects: parts.excluded_projects,
            search_tx: parts.search_tx,
            search_rx: parts.search_rx,
            search_generation: 0,
            search_in_flight: false,
            list_search_mode: parts.list_search_mode,
            semantic_search: parts.semantic_search,
            lexical_evidence: HashMap::new(),
            annotations_root: annotations_root.clone(),
            annotation_counts: annotations_root
                .as_deref()
                .map(crate::annotations::sidecar_counts)
                .unwrap_or_default(),
            annotations_enriched: false,
        }
    }

    /// The number of notes attached to one conversation.
    pub fn annotation_count(&self, conversation: &std::path::Path) -> usize {
        let Some(root) = self.annotations_root.as_deref() else {
            return 0;
        };
        crate::annotations::sidecar_path(root, conversation)
            .and_then(|sidecar| self.annotation_counts.get(&sidecar).copied())
            .unwrap_or(0)
    }

    /// Places a note count for one conversation without touching the store.
    #[cfg(test)]
    pub fn set_annotation_count_for_test(
        &mut self,
        root: PathBuf,
        conversation: &std::path::Path,
        count: usize,
    ) {
        if let Some(sidecar) = crate::annotations::sidecar_path(&root, conversation) {
            self.annotation_counts.insert(sidecar, count);
        }
        self.annotations_root = Some(root);
    }

    /// Points the annotation reads at one root and drops the enrichment guard,
    /// so a test corpus is read instead of the configured store.
    #[cfg(test)]
    pub fn set_annotations_root_for_test(&mut self, root: PathBuf) {
        self.annotation_counts = crate::annotations::sidecar_counts(&root);
        self.annotations_root = Some(root);
        self.annotations_enriched = false;
    }

    /// Re-reads one conversation's sidecar after a note is written or deleted,
    /// so the list row states the count the store holds rather than the count
    /// read at startup.
    pub(super) fn refresh_annotation_count(&mut self, conversation: &std::path::Path) {
        let Some(root) = self.annotations_root.clone() else {
            return;
        };
        let Some(sidecar) = crate::annotations::sidecar_path(&root, conversation) else {
            return;
        };
        let count = crate::annotations::for_conversation(conversation).len();
        if count == 0 {
            self.annotation_counts.remove(&sidecar);
        } else {
            self.annotation_counts.insert(sidecar, count);
        }
    }

    fn conversation_snapshot(conversations: &[Conversation]) -> Arc<Vec<Conversation>> {
        Arc::new(conversations.to_vec())
    }

    fn semantic_snapshot(conversations: &[Conversation]) -> Arc<Vec<Arc<Conversation>>> {
        Arc::new(conversations.iter().cloned().map(Arc::new).collect())
    }

    fn send_initial_search_data(
        search_tx: &mpsc::Sender<SearchCommand>,
        conversations: Arc<Vec<Conversation>>,
        searchable: &[SearchableConversation],
    ) {
        let _ = search_tx.send(SearchCommand::UpdateData {
            conversations,
            searchable: Arc::new(searchable.to_vec()),
        });
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
        let searchable = search::precompute_search_text(&conversations);
        let excluded_projects = exclude_projects.into_iter().collect();
        let filtered = list_state::filter_conversation_indices(
            0..conversations.len(),
            &conversations,
            &excluded_projects,
            false,
            None,
        );
        let selected = if filtered.is_empty() { None } else { Some(0) };
        let (search_tx, search_rx) = spawn_search_worker();
        let conversations_snapshot = Self::conversation_snapshot(&conversations);

        Self::send_initial_search_data(&search_tx, conversations_snapshot.clone(), &searchable);

        Self::from_parts(AppParts {
            conversations_snapshot,
            semantic_conversations_snapshot: Self::semantic_snapshot(&conversations),
            conversations,
            searchable,
            filtered,
            selected,
            loading_state: LoadingState::Ready,
            app_mode: AppMode::List,
            tool_display,
            show_thinking,
            single_file_mode: false,
            keys,
            workspace_filter: false,
            current_project_dir_name: None,
            excluded_projects,
            search_tx,
            search_rx,
            list_search_mode: Self::list_search_mode_from_options(search_options),
            semantic_search: Self::semantic_search_state(true),
        })
    }

    /// Create a new app in loading state
    pub fn new_loading_with_options(
        tool_display: ToolDisplayMode,
        show_thinking: bool,
        keys: KeyBindings,
        workspace_filter: bool,
        current_project_dir_name: Option<String>,
        exclude_projects: Vec<String>,
        search_options: TuiSearchOptions,
    ) -> Self {
        let conversations = Vec::new();
        let (search_tx, search_rx) = spawn_search_worker();

        Self::from_parts(AppParts {
            conversations_snapshot: Self::conversation_snapshot(&conversations),
            semantic_conversations_snapshot: Self::semantic_snapshot(&conversations),
            conversations,
            searchable: Vec::new(),
            filtered: Vec::new(),
            selected: None,
            loading_state: LoadingState::Loading { loaded: 0 },
            app_mode: AppMode::List,
            tool_display,
            show_thinking,
            single_file_mode: false,
            keys,
            workspace_filter,
            current_project_dir_name,
            excluded_projects: exclude_projects.into_iter().collect(),
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
            conversations_snapshot: Self::conversation_snapshot(&conversations),
            semantic_conversations_snapshot: Self::semantic_snapshot(&conversations),
            conversations,
            searchable: Vec::new(),
            filtered,
            selected,
            loading_state: LoadingState::Ready,
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
            current_project_dir_name: None,
            excluded_projects: HashSet::new(),
            search_tx,
            search_rx,
            list_search_mode: ListSearchMode::Lexical,
            semantic_search: Self::semantic_search_state(false),
        })
    }

    pub fn keys(&self) -> &KeyBindings {
        &self.keys
    }

    /// Append a batch of conversations during loading
    /// Note: Does NOT precompute search text - that's deferred to finish_loading
    pub fn append_conversations(&mut self, new_convs: Vec<Conversation>) {
        let start_idx = self.conversations.len();
        self.conversations.extend(new_convs);
        let end_idx = self.conversations.len();

        let new_filtered = self.filter_indices(start_idx..end_idx);
        self.filtered.extend(new_filtered);

        // Select first item if nothing selected yet
        if self.selected.is_none() && !self.filtered.is_empty() {
            self.selected = Some(0);
        }

        // Update loading count
        self.loading_state = LoadingState::Loading {
            loaded: self.conversations.len(),
        };
    }

    /// Appends every conversation's note text to its lexical search fields.
    ///
    /// A note lives outside the transcript, so lexical search over the parsed
    /// text alone never matches it and the conversation carrying it stays out
    /// of the results. The append also reaches `full_text`, which the evidence
    /// builder draws context from, so a row matched on a note shows that note's
    /// text as its context line.
    fn enrich_annotations(&mut self) {
        if self.annotations_enriched {
            return;
        }
        let Some(root) = self.annotations_root.clone() else {
            return;
        };
        let scoped = (0..self.conversations.len()).collect::<Vec<_>>();
        let _ = crate::annotations::enrich_scoped(
            &mut self.conversations,
            &scoped,
            &crate::annotations::FileAnnotator::new(root),
        );
        self.annotations_enriched = true;
    }

    /// Rebuilds one conversation's lexical fields after its notes change.
    ///
    /// The append in `enrich_annotations` cannot be undone in place, so the
    /// transcript is re-parsed and the current notes appended to the fresh
    /// text; without the re-parse a deleted note stays searchable and an edited
    /// one is searchable under both its texts.
    pub(super) fn refresh_conversation_annotations(&mut self, conversation: &std::path::Path) {
        let Some(index) = self
            .conversations
            .iter()
            .position(|conv| conv.path == conversation)
        else {
            return;
        };
        let modified = std::fs::metadata(conversation)
            .and_then(|meta| meta.modified())
            .ok();
        let Ok(Some(reparsed)) =
            process_conversation_file(conversation.to_path_buf(), modified, None)
        else {
            return;
        };
        let conv = &mut self.conversations[index];
        conv.full_text = reparsed.full_text;
        conv.search_text_lower = reparsed.search_text_lower;
        conv.agent_search_text = reparsed.agent_search_text;

        if let Some(root) = self.annotations_root.clone() {
            let _ = crate::annotations::enrich_scoped(
                &mut self.conversations,
                &[index],
                &crate::annotations::FileAnnotator::new(root),
            );
        }

        // The worker scores against its own snapshot, so the corpus it holds is
        // replaced before the next query runs against stale text.
        self.conversations_snapshot = Arc::new(self.conversations.clone());
        self.rebuild_semantic_conversations_snapshot();
        self.searchable = search::precompute_search_text(&self.conversations);
        let _ = self.search_tx.send(SearchCommand::UpdateData {
            conversations: self.conversations_snapshot.clone(),
            searchable: Arc::new(self.searchable.clone()),
        });
        self.invalidate_search_generation();
    }

    /// Mark loading as complete: sort, precompute search, and transition to Ready
    pub fn finish_loading(&mut self) {
        // Sort all conversations by timestamp (newest first)
        self.conversations
            .sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

        // Reindex after sorting
        for (idx, conv) in self.conversations.iter_mut().enumerate() {
            conv.index = idx;
        }

        // Notes join the lexical fields before the snapshot and the precompute,
        // so the worker's corpus and the searchable text both carry them.
        self.enrich_annotations();

        self.conversations_snapshot = Arc::new(self.conversations.clone());
        self.rebuild_semantic_conversations_snapshot();

        // Now precompute search text (only once, at the end)
        self.searchable = search::precompute_search_text(&self.conversations);

        // Send data snapshot to the background search worker
        let _ = self.search_tx.send(SearchCommand::UpdateData {
            conversations: self.conversations_snapshot.clone(),
            searchable: Arc::new(self.searchable.clone()),
        });

        self.loading_state = LoadingState::Ready;

        self.invalidate_search_generation();

        // Apply filter (handles query, exclusions, and workspace filter)
        self.update_filter();
        if self.list_search_mode == ListSearchMode::Semantic && !self.query.trim().is_empty() {
            self.dispatch_search();
        } else {
            self.prewarm_semantic_cache();
        }
    }

    /// Consume the app and return its conversations
    pub fn into_conversations(self) -> Vec<Conversation> {
        self.conversations
    }

    pub fn loading_state(&self) -> &LoadingState {
        &self.loading_state
    }

    pub fn is_loading(&self) -> bool {
        matches!(self.loading_state, LoadingState::Loading { .. })
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

    pub fn workspace_filter(&self) -> bool {
        self.workspace_filter
    }

    pub fn list_search_mode(&self) -> ListSearchMode {
        self.list_search_mode
    }

    pub fn semantic_search_available(&self) -> bool {
        self.semantic_search.available
    }

    pub fn has_project_context(&self) -> bool {
        self.current_project_dir_name.is_some()
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
    }

    /// Handle a left-click in list mode: select the conversation under the cursor.
    /// Returns true if the click landed on a list item — the caller is expected to
    /// then transition into view mode (matching the Enter-key behavior).
    pub fn handle_list_click(&mut self, row: u16, frame_area: Rect) -> bool {
        if !matches!(self.app_mode, AppMode::List)
            || self.dialog_mode != DialogMode::None
            || self.is_loading()
        {
            return false;
        }

        // Mirror the layout in render_list_mode: outer 1px border, then split
        // [search bar (2), list (Min 1), bottom bar (1)] — or omit the bottom
        // bar when the inner area is < 4 lines tall.
        let inner_height = frame_area.height.saturating_sub(2);
        let list_y = frame_area.y.saturating_add(1).saturating_add(2);
        let list_height = if inner_height < 4 {
            inner_height.saturating_sub(2)
        } else {
            inner_height.saturating_sub(3)
        };

        if list_height == 0 || row < list_y || row >= list_y.saturating_add(list_height) {
            return false;
        }

        let lines_per_item = list_lines_per_item(self.list_search_mode, &self.query);
        let items_per_page = (list_height as usize) / lines_per_item;
        if items_per_page == 0 {
            return false;
        }

        let offset = match self.selected {
            Some(sel) => (sel / items_per_page) * items_per_page,
            None => 0,
        };
        let relative_row = (row - list_y) as usize;
        let relative_idx = relative_row / lines_per_item;
        let new_idx = offset + relative_idx;
        if new_idx < self.filtered.len() {
            self.selected = Some(new_idx);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod semantic_test_helpers;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod interaction_tests;
