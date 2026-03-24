use crate::config::KeyBindings;
use crate::debug_log;
use crate::error::{AppError, Result};
use crate::history::{
    Conversation, LoaderMessage, format_short_name_from_path, process_conversation_file,
};
use crate::tui::search::{self, SearchableConversation};
use crate::tui::ui;
use crate::tui::viewer::{MessageRange, ToolDisplayMode};
use chrono::Local;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::prelude::*;
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

/// Result of running the TUI
pub enum Action {
    Select(PathBuf),
    Delete(PathBuf),
    Resume(PathBuf),
    ForkResume(PathBuf),
    ResumeHere(PathBuf),
    Quit,
}

/// Dialog overlay mode (for confirmations, menus)
#[derive(Clone, Debug, PartialEq)]
pub enum DialogMode {
    /// No dialog shown
    None,
    /// Confirming deletion of the selected conversation
    ConfirmDelete,
    /// Export menu (save to file)
    ExportMenu { selected: usize },
    /// Yank menu (copy to clipboard)
    YankMenu { selected: usize },
    /// Help overlay showing keyboard shortcuts
    Help,
}

/// Export format options for menus
const EXPORT_OPTIONS: [&str; 4] = [
    "Ledger (formatted)",
    "Plain text",
    "Markdown",
    "JSONL (raw)",
];

/// Main application mode
#[derive(Clone, Debug)]
pub enum AppMode {
    /// List mode - browsing conversations
    List,
    /// View mode - reading a conversation
    View(ViewState),
}

/// State for the conversation viewer
#[derive(Clone, Debug)]
pub struct ViewState {
    /// Path to the conversation file (stable identity)
    pub conversation_path: PathBuf,
    /// Current scroll position (line offset)
    pub scroll_offset: usize,
    /// Pre-rendered conversation lines
    pub rendered_lines: Vec<RenderedLine>,
    /// Total content height in lines
    pub total_lines: usize,
    /// Tool display mode (hidden/truncated/full)
    pub tool_display: ToolDisplayMode,
    /// Whether to show thinking blocks
    pub show_thinking: bool,
    /// Whether to show timing information (timestamps + durations)
    pub show_timing: bool,
    /// Content width used for rendering (for resize detection)
    pub content_width: usize,
    /// Search mode state
    pub search_mode: ViewSearchMode,
    /// Current search query
    pub search_query: String,
    /// Line indices with matches
    pub search_matches: Vec<usize>,
    /// Current match index
    pub current_match: usize,
    /// Message boundary ranges from rendering
    pub message_ranges: Vec<MessageRange>,
    /// Currently focused message index
    pub focused_message: Option<usize>,
    /// Whether message navigation mode is active (shows gutter indicator)
    pub message_nav_active: bool,
}

/// Search mode within view
#[derive(Clone, Debug, PartialEq, Default)]
pub enum ViewSearchMode {
    #[default]
    Off,
    /// Typing search query
    Typing,
    /// Search active, navigating results
    Active,
}

/// A single rendered line with its spans
#[derive(Clone, Debug)]
pub struct RenderedLine {
    pub spans: Vec<(String, LineStyle)>,
}

/// Style information for a span
#[derive(Clone, Debug, Default)]
pub struct LineStyle {
    pub fg: Option<(u8, u8, u8)>,
    pub bold: bool,
    pub dimmed: bool,
    pub italic: bool,
}

/// Loading state for the TUI
#[derive(Clone, Debug)]
pub enum LoadingState {
    /// Still loading conversations
    Loading { loaded: usize },
    /// All conversations loaded and ready
    Ready,
}

/// Command sent to the background search worker
enum SearchCommand {
    /// Update the dataset the worker searches over
    UpdateData {
        conversations: Arc<Vec<Conversation>>,
        searchable: Arc<Vec<SearchableConversation>>,
    },
    /// Run a search query
    Search {
        query: String,
        generation: u64,
        workspace_filter: bool,
        project_dir_name: Option<String>,
    },
}

/// Result returned from the background search worker
struct SearchResponse {
    filtered: Vec<usize>,
    generation: u64,
}

/// Spawn the background search worker thread.
/// Returns (sender for commands, receiver for results).
fn spawn_search_worker() -> (mpsc::Sender<SearchCommand>, mpsc::Receiver<SearchResponse>) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<SearchCommand>();
    let (res_tx, res_rx) = mpsc::channel::<SearchResponse>();

    std::thread::Builder::new()
        .name("search-worker".into())
        .spawn(move || {
            let mut conversations: Arc<Vec<Conversation>> = Arc::new(Vec::new());
            let mut searchable: Arc<Vec<SearchableConversation>> = Arc::new(Vec::new());

            while let Ok(cmd) = cmd_rx.recv() {
                match cmd {
                    SearchCommand::UpdateData {
                        conversations: c,
                        searchable: s,
                    } => {
                        conversations = c;
                        searchable = s;
                    }
                    SearchCommand::Search {
                        mut query,
                        mut generation,
                        mut workspace_filter,
                        mut project_dir_name,
                    } => {
                        // Drain pending commands: apply all data updates,
                        // keep only the latest search request
                        while let Ok(pending) = cmd_rx.try_recv() {
                            match pending {
                                SearchCommand::UpdateData {
                                    conversations: c,
                                    searchable: s,
                                } => {
                                    conversations = c;
                                    searchable = s;
                                }
                                SearchCommand::Search {
                                    query: q,
                                    generation: g,
                                    workspace_filter: wf,
                                    project_dir_name: pdn,
                                } => {
                                    query = q;
                                    generation = g;
                                    workspace_filter = wf;
                                    project_dir_name = pdn;
                                }
                            }
                        }

                        let now = chrono::Local::now();
                        let mut filtered = search::search(&conversations, &searchable, &query, now);

                        if workspace_filter && let Some(ref dir_name) = project_dir_name {
                            filtered.retain(|&idx| {
                                conversations[idx]
                                    .path
                                    .parent()
                                    .and_then(|p| p.file_name())
                                    .is_some_and(|name| {
                                        crate::history::path::is_same_project(
                                            &name.to_string_lossy(),
                                            dir_name,
                                        )
                                    })
                            });
                        }

                        let _ = res_tx.send(SearchResponse {
                            filtered,
                            generation,
                        });
                    }
                }
            }
        })
        .expect("failed to spawn search worker thread");

    (cmd_tx, res_rx)
}

/// App state
pub struct App {
    /// All loaded conversations
    conversations: Vec<Conversation>,
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
    /// Channel to send commands to the background search worker
    search_tx: mpsc::Sender<SearchCommand>,
    /// Channel to receive results from the background search worker
    search_rx: mpsc::Receiver<SearchResponse>,
    /// Monotonic generation counter for search requests
    search_generation: u64,
    /// Whether a search is currently in-flight on the worker thread
    search_in_flight: bool,
}

impl App {
    /// Create a new app with all conversations pre-loaded
    #[allow(dead_code)]
    pub fn new(
        conversations: Vec<Conversation>,
        tool_display: ToolDisplayMode,
        show_thinking: bool,
        keys: KeyBindings,
    ) -> Self {
        let searchable = search::precompute_search_text(&conversations);
        let filtered: Vec<usize> = (0..conversations.len()).collect();
        let selected = if filtered.is_empty() { None } else { Some(0) };
        let (search_tx, search_rx) = spawn_search_worker();

        // Send initial data to the worker
        let _ = search_tx.send(SearchCommand::UpdateData {
            conversations: Arc::new(conversations.clone()),
            searchable: Arc::new(searchable.clone()),
        });

        Self {
            conversations,
            searchable,
            filtered,
            selected,
            query: String::new(),
            cursor_pos: 0,
            loading_state: LoadingState::Ready,
            dialog_mode: DialogMode::None,
            app_mode: AppMode::List,
            status_message: None,
            tool_display,
            show_thinking,
            show_timing: false,
            single_file_mode: false,
            keys,
            workspace_filter: false,
            current_project_dir_name: None,
            search_tx,
            search_rx,
            search_generation: 0,
            search_in_flight: false,
        }
    }

    /// Create a new app in loading state
    pub fn new_loading(
        tool_display: ToolDisplayMode,
        show_thinking: bool,
        keys: KeyBindings,
        workspace_filter: bool,
        current_project_dir_name: Option<String>,
    ) -> Self {
        let (search_tx, search_rx) = spawn_search_worker();

        Self {
            conversations: Vec::new(),
            searchable: Vec::new(),
            filtered: Vec::new(),
            selected: None,
            query: String::new(),
            cursor_pos: 0,
            loading_state: LoadingState::Loading { loaded: 0 },
            dialog_mode: DialogMode::None,
            app_mode: AppMode::List,
            status_message: None,
            tool_display,
            show_thinking,
            show_timing: false,
            single_file_mode: false,
            keys,
            workspace_filter,
            current_project_dir_name,
            search_tx,
            search_rx,
            search_generation: 0,
            search_in_flight: false,
        }
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

        if let Ok(Some(mut conv)) = process_conversation_file(path.clone(), false, modified, None) {
            // Set project_name the same way as the loader does
            let project_path = conv.cwd.clone().unwrap_or_else(|| path.clone());
            conv.project_name = Some(format_short_name_from_path(&project_path));

            conversations.push(conv);
            filtered.push(0);
            selected = Some(0);
        }

        Self {
            conversations,
            searchable: Vec::new(),
            filtered,
            selected,
            query: String::new(),
            cursor_pos: 0,
            loading_state: LoadingState::Ready,
            dialog_mode: DialogMode::None,
            app_mode: AppMode::View(ViewState {
                conversation_path: path,
                scroll_offset: 0,
                rendered_lines: Vec::new(),
                total_lines: 0,
                tool_display,
                show_thinking,
                show_timing: false,
                content_width: 0,
                search_mode: ViewSearchMode::Off,
                search_query: String::new(),
                search_matches: Vec::new(),
                current_match: 0,
                message_ranges: Vec::new(),
                focused_message: None,
                message_nav_active: false,
            }),
            status_message: None,
            tool_display,
            show_thinking,
            show_timing: false,
            single_file_mode: true,
            keys,
            workspace_filter: false,
            current_project_dir_name: None,
            search_tx,
            search_rx,
            search_generation: 0,
            search_in_flight: false,
        }
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

        // Update filtered so items appear in the list during loading
        // (Items shown in arrival order initially, will be re-sorted in finish_loading)
        // Apply workspace filter during loading too
        for idx in start_idx..end_idx {
            if self.workspace_filter
                && let Some(ref project_dir_name) = self.current_project_dir_name
                && self.conversations[idx]
                    .path
                    .parent()
                    .and_then(|p| p.file_name())
                    .is_none_or(|name| {
                        !crate::history::path::is_same_project(
                            &name.to_string_lossy(),
                            project_dir_name,
                        )
                    })
            {
                continue;
            }
            self.filtered.push(idx);
        }

        // Select first item if nothing selected yet
        if self.selected.is_none() && !self.filtered.is_empty() {
            self.selected = Some(0);
        }

        // Update loading count
        self.loading_state = LoadingState::Loading {
            loaded: self.conversations.len(),
        };
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

        // Now precompute search text (only once, at the end)
        self.searchable = search::precompute_search_text(&self.conversations);

        // Send data snapshot to the background search worker
        let _ = self.search_tx.send(SearchCommand::UpdateData {
            conversations: Arc::new(self.conversations.clone()),
            searchable: Arc::new(self.searchable.clone()),
        });

        self.loading_state = LoadingState::Ready;

        // Apply filter (handles both query and workspace filter)
        if self.query.is_empty() && !self.workspace_filter {
            // No query and no workspace filter - show all
            self.filtered = (0..self.conversations.len()).collect();
            self.selected = if self.filtered.is_empty() {
                None
            } else {
                Some(0)
            };
        } else {
            // Has query or workspace filter active - apply full filter
            self.update_filter();
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

    /// Update filtered results based on current query
    fn update_filter(&mut self) {
        let query = self.query.trim().to_string();

        // UUID search: find session by UUID across all projects
        if search::is_uuid(&query)
            && let Some(idx) = self.find_or_load_uuid(&query)
        {
            self.filtered = vec![idx];
            self.selected = Some(0);
            return;
        }

        let now = Local::now();
        let mut filtered = search::search(&self.conversations, &self.searchable, &self.query, now);

        // Apply workspace filter if active
        // Matches conversations from the same project, including workmux worktrees
        if self.workspace_filter
            && let Some(ref project_dir_name) = self.current_project_dir_name
        {
            filtered.retain(|&idx| {
                self.conversations[idx]
                    .path
                    .parent()
                    .and_then(|p| p.file_name())
                    .is_some_and(|name| {
                        crate::history::path::is_same_project(
                            &name.to_string_lossy(),
                            project_dir_name,
                        )
                    })
            });
        }

        self.filtered = filtered;
        self.selected = if self.filtered.is_empty() {
            None
        } else {
            Some(0)
        };
    }

    /// Dispatch a search to the background worker.
    /// UUID queries are handled synchronously (rare, needs to modify data).
    fn dispatch_search(&mut self) {
        let query = self.query.trim().to_string();

        // UUID search: synchronous (rare, needs to modify conversations)
        if search::is_uuid(&query) {
            if let Some(idx) = self.find_or_load_uuid(&query) {
                self.filtered = vec![idx];
                self.selected = Some(0);
            }
            return;
        }

        self.search_generation += 1;
        self.search_in_flight = true;
        let _ = self.search_tx.send(SearchCommand::Search {
            query,
            generation: self.search_generation,
            workspace_filter: self.workspace_filter,
            project_dir_name: self.current_project_dir_name.clone(),
        });
    }

    /// Check for completed search results from the background worker.
    /// Returns true if results were applied.
    pub fn receive_search_results(&mut self) -> bool {
        let mut applied = false;
        while let Ok(response) = self.search_rx.try_recv() {
            // Only apply the result if it matches the latest generation
            if response.generation == self.search_generation {
                self.filtered = response.filtered;
                self.selected = if self.filtered.is_empty() {
                    None
                } else {
                    Some(0)
                };
                self.search_in_flight = false;
                applied = true;
            }
        }
        applied
    }

    /// Find a conversation by UUID in loaded conversations, or load it from disk.
    fn find_or_load_uuid(&mut self, uuid: &str) -> Option<usize> {
        // Check already-loaded conversations
        let uuid_jsonl = format!("{}.jsonl", uuid);
        for (idx, conv) in self.conversations.iter().enumerate() {
            if conv
                .path
                .file_name()
                .is_some_and(|f| f.to_string_lossy() == uuid_jsonl)
            {
                return Some(idx);
            }
        }

        // Try to find and load from filesystem
        let path = crate::history::find_jsonl_by_uuid(uuid).ok()??;
        let modified = path.metadata().ok().and_then(|m| m.modified().ok());
        let mut conv =
            crate::history::process_conversation_file(path, false, modified, None).ok()??;

        // Inject project metadata (process_conversation_file doesn't set these)
        let fallback_path = conv
            .path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| crate::history::path::decode_project_dir_name_to_path(&n.to_string_lossy()))
            .unwrap_or_default();
        let project_path = conv.cwd.clone().unwrap_or(fallback_path);
        conv.project_name = Some(format_short_name_from_path(&project_path));
        conv.project_path = Some(project_path);

        let idx = self.conversations.len();
        self.conversations.push(conv);

        // Rebuild search index to include the new conversation
        self.searchable = search::precompute_search_text(&self.conversations);

        // Update the worker's data snapshot
        let _ = self.search_tx.send(SearchCommand::UpdateData {
            conversations: Arc::new(self.conversations.clone()),
            searchable: Arc::new(self.searchable.clone()),
        });

        Some(idx)
    }

    /// Move selection up
    fn select_prev(&mut self) {
        if let Some(selected) = self.selected
            && selected > 0
        {
            self.selected = Some(selected - 1);
        }
    }

    /// Move selection down
    fn select_next(&mut self) {
        if let Some(selected) = self.selected
            && selected + 1 < self.filtered.len()
        {
            self.selected = Some(selected + 1);
        }
    }

    /// Move selection to first item
    fn select_first(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = Some(0);
        }
    }

    /// Move selection to last item
    fn select_last(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = Some(self.filtered.len() - 1);
        }
    }

    /// Move selection up by a page
    fn select_page_up(&mut self) {
        if let Some(selected) = self.selected {
            self.selected = Some(selected.saturating_sub(10));
        }
    }

    /// Move selection down by a page
    fn select_page_down(&mut self) {
        if let Some(selected) = self.selected {
            let new_selected = (selected + 10).min(self.filtered.len().saturating_sub(1));
            self.selected = Some(new_selected);
        }
    }

    /// Move selection down by half a page (vim-style Ctrl-D)
    fn select_half_page_down(&mut self, viewport_height: usize) {
        if let Some(selected) = self.selected {
            let half_page = viewport_height / 2;
            let new_selected = (selected + half_page).min(self.filtered.len().saturating_sub(1));
            self.selected = Some(new_selected);
        }
    }

    /// Get the currently selected conversation path
    fn get_selected_path(&self) -> Option<PathBuf> {
        self.selected
            .and_then(|sel| self.filtered.get(sel))
            .map(|&idx| self.conversations[idx].path.clone())
    }

    // Getters for UI access
    pub fn filtered(&self) -> &[usize] {
        &self.filtered
    }

    pub fn conversations(&self) -> &[Conversation] {
        &self.conversations
    }

    /// Count of conversations in the current scope (project-filtered or all).
    pub fn scoped_count(&self) -> usize {
        if self.workspace_filter
            && let Some(ref project_dir_name) = self.current_project_dir_name
        {
            return self
                .conversations
                .iter()
                .filter(|c| {
                    c.path
                        .parent()
                        .and_then(|p| p.file_name())
                        .is_some_and(|name| {
                            crate::history::path::is_same_project(
                                &name.to_string_lossy(),
                                project_dir_name,
                            )
                        })
                })
                .count();
        }
        self.conversations.len()
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

    pub fn has_project_context(&self) -> bool {
        self.current_project_dir_name.is_some()
    }

    /// Toggle between global and workspace-only view
    fn toggle_workspace_filter(&mut self) {
        // Only toggle if we have a workspace context
        if self.current_project_dir_name.is_some() {
            self.workspace_filter = !self.workspace_filter;
            self.update_filter();
        }
    }

    /// Move cursor left by one character
    fn cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos -= 1;
        }
    }

    /// Move cursor right by one character
    fn cursor_right(&mut self) {
        let len = self.query.chars().count();
        if self.cursor_pos < len {
            self.cursor_pos += 1;
        }
    }

    /// Move cursor to the beginning of the line (Ctrl+A)
    fn cursor_home(&mut self) {
        self.cursor_pos = 0;
    }

    /// Move cursor to the end of the line (Ctrl+E)
    fn cursor_end(&mut self) {
        self.cursor_pos = self.query.chars().count();
    }

    /// Move cursor one word to the left (Ctrl+Left / Alt+B)
    fn cursor_word_left(&mut self) {
        let chars: Vec<char> = self.query.chars().collect();
        let mut pos = self.cursor_pos.min(chars.len());
        // Skip separators to the left
        while pos > 0 && search::is_word_separator(chars[pos - 1]) {
            pos -= 1;
        }
        // Skip non-separators (the word)
        while pos > 0 && !search::is_word_separator(chars[pos - 1]) {
            pos -= 1;
        }
        self.cursor_pos = pos;
    }

    /// Move cursor one word to the right (Ctrl+Right / Alt+F)
    fn cursor_word_right(&mut self) {
        let chars: Vec<char> = self.query.chars().collect();
        let len = chars.len();
        let mut pos = self.cursor_pos.min(len);
        // Skip non-separators (the word)
        while pos < len && !search::is_word_separator(chars[pos]) {
            pos += 1;
        }
        // Skip separators
        while pos < len && search::is_word_separator(chars[pos]) {
            pos += 1;
        }
        self.cursor_pos = pos;
    }

    /// Delete from cursor to end of line (Ctrl+K). Returns true if modified.
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

    /// Delete from beginning of line to cursor (Ctrl+U). Returns true if modified.
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

    /// Delete the word before the cursor (Ctrl+W behavior).
    /// Returns true if the query was modified.
    fn delete_word_backwards(&mut self) -> bool {
        let chars: Vec<char> = self.query.chars().collect();
        let cursor = self.cursor_pos.min(chars.len());
        if cursor == 0 {
            return false;
        }

        let mut new_pos = cursor;

        // First, consume any separators to the left of cursor
        while new_pos > 0 && search::is_word_separator(chars[new_pos - 1]) {
            new_pos -= 1;
        }

        // Then, consume non-separators (the actual word)
        while new_pos > 0 && !search::is_word_separator(chars[new_pos - 1]) {
            new_pos -= 1;
        }

        if new_pos == cursor {
            return false;
        }

        // Convert char indices to byte indices for safe string manipulation
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

    /// Remove the currently selected conversation from the UI list.
    /// This should only be called after the file has been successfully deleted from disk.
    /// Handles index management for conversations, searchable, and filtered vectors.
    pub fn remove_selected_from_list(&mut self) {
        let Some(selected) = self.selected else {
            return;
        };
        let Some(&conv_idx) = self.filtered.get(selected) else {
            return;
        };

        // Remove from conversations
        self.conversations.remove(conv_idx);

        // Remove from searchable and update indices
        // Note: searchable is not ordered by index due to parallel collection,
        // so we can't use positional removal - must find by index value
        self.searchable.retain_mut(|s| {
            if s.index == conv_idx {
                false // Remove this entry
            } else {
                if s.index > conv_idx {
                    s.index -= 1; // Adjust index for removed item
                }
                true
            }
        });

        // Update filtered: remove the deleted index and decrement all indices > conv_idx
        self.filtered.retain(|&idx| idx != conv_idx);
        for idx in &mut self.filtered {
            if *idx > conv_idx {
                *idx -= 1;
            }
        }

        // Update selection: stay at same position if possible, or move to last item
        if self.filtered.is_empty() {
            self.selected = None;
        } else if selected >= self.filtered.len() {
            self.selected = Some(self.filtered.len() - 1);
        }
        // else: selected stays the same (now pointing to next item)

        // Sync updated data to the background search worker and bump generation
        // to discard any in-flight results computed against stale data
        let _ = self.search_tx.send(SearchCommand::UpdateData {
            conversations: Arc::new(self.conversations.clone()),
            searchable: Arc::new(self.searchable.clone()),
        });
        self.search_generation += 1;
    }

    /// Handle a key event during confirmation mode
    fn handle_confirm_key(&mut self, code: KeyCode) -> Option<Action> {
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

    /// Handle a key event during export/yank menu mode
    fn handle_menu_key(&mut self, code: KeyCode) -> Option<Action> {
        let (selected, is_yank) = match &mut self.dialog_mode {
            DialogMode::ExportMenu { selected } => (selected, false),
            DialogMode::YankMenu { selected } => (selected, true),
            _ => return None,
        };

        match code {
            // Navigate up
            KeyCode::Up | KeyCode::Char('k') => {
                *selected = selected.saturating_sub(1);
                None
            }
            // Navigate down
            KeyCode::Down | KeyCode::Char('j') => {
                *selected = (*selected + 1).min(EXPORT_OPTIONS.len() - 1);
                None
            }
            // Number keys for direct selection
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
            // Enter to select current option
            KeyCode::Enter => {
                let sel = *selected;
                self.perform_export(sel, is_yank);
                self.dialog_mode = DialogMode::None;
                None
            }
            // Escape to cancel
            KeyCode::Esc => {
                self.dialog_mode = DialogMode::None;
                None
            }
            _ => None,
        }
    }

    /// Handle a key event during help overlay mode
    fn handle_help_key(&mut self, code: KeyCode) -> Option<Action> {
        match code {
            KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Esc => {
                self.dialog_mode = DialogMode::None;
                None
            }
            _ => None,
        }
    }

    /// Perform export or yank operation
    fn perform_export(&mut self, option: usize, to_clipboard: bool) {
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

    /// Handle a key event, returns Some(Action) if the app should exit
    /// viewport_height is the visible content area height for view mode scrolling
    pub fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        viewport_height: usize,
    ) -> Option<Action> {
        // Handle dialogs first
        match self.dialog_mode {
            DialogMode::ConfirmDelete => return self.handle_confirm_key(code),
            DialogMode::ExportMenu { .. } | DialogMode::YankMenu { .. } => {
                return self.handle_menu_key(code);
            }
            DialogMode::Help => return self.handle_help_key(code),
            DialogMode::None => {}
        }

        // Delegate based on app mode
        match &self.app_mode {
            AppMode::View(_) => self.handle_view_key(code, modifiers, viewport_height),
            AppMode::List => self.handle_list_key(code, modifiers, viewport_height),
        }
    }

    /// Handle key events in view mode
    fn handle_view_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        viewport_height: usize,
    ) -> Option<Action> {
        // First check if we're in search typing mode
        if let AppMode::View(ref state) = self.app_mode
            && state.search_mode == ViewSearchMode::Typing
        {
            return self.handle_search_typing_key(code, modifiers);
        }

        // Check configurable keybindings before the match block
        if self.keys.delete.matches(code, modifiers) {
            if !self.single_file_mode {
                self.dialog_mode = DialogMode::ConfirmDelete;
            }
            return None;
        }
        if self.keys.resume.matches(code, modifiers) {
            return if self.single_file_mode {
                None
            } else {
                self.get_selected_path().map(Action::Resume)
            };
        }
        if self.keys.fork.matches(code, modifiers) {
            return if self.single_file_mode {
                None
            } else {
                self.get_selected_path().map(Action::ForkResume)
            };
        }
        if self.keys.resume_here.matches(code, modifiers) {
            return if self.single_file_mode {
                None
            } else {
                self.get_selected_path().map(Action::ResumeHere)
            };
        }

        let state = match &mut self.app_mode {
            AppMode::View(s) => s,
            _ => return None,
        };

        let max_scroll = state.total_lines.saturating_sub(viewport_height);

        match code {
            // Exit view mode (or clear search if active)
            KeyCode::Esc => {
                // Exit message nav mode first
                if let AppMode::View(ref mut state) = self.app_mode
                    && state.message_nav_active
                {
                    state.message_nav_active = false;
                    return None;
                }
                // If search is active, clear it first before exiting view
                if let AppMode::View(ref mut state) = self.app_mode
                    && state.search_mode == ViewSearchMode::Active
                {
                    state.search_mode = ViewSearchMode::Off;
                    state.search_matches.clear();
                    state.search_query.clear();
                    return None;
                }
                // In single file mode, Esc quits the app
                if self.single_file_mode {
                    return Some(Action::Quit);
                }
                self.app_mode = AppMode::List;
                None
            }

            KeyCode::Char('q') => {
                // In single file mode, q quits the app
                if self.single_file_mode {
                    return Some(Action::Quit);
                }
                self.app_mode = AppMode::List;
                None
            }

            // Scroll down one line
            KeyCode::Down | KeyCode::Char('j') => {
                state.scroll_offset = (state.scroll_offset + 1).min(max_scroll);
                self.sync_focus_after_scroll(viewport_height);
                None
            }

            // Scroll up one line
            KeyCode::Up | KeyCode::Char('k') => {
                state.scroll_offset = state.scroll_offset.saturating_sub(1);
                self.sync_focus_after_scroll(viewport_height);
                None
            }

            // Jump to next message
            KeyCode::Char('J') | KeyCode::Char(']') => {
                self.focus_next_message(viewport_height);
                None
            }

            // Jump to previous message
            KeyCode::Char('K') | KeyCode::Char('[') => {
                self.focus_prev_message(viewport_height);
                None
            }

            // Scroll down half page
            KeyCode::Char('d') if !modifiers.contains(KeyModifiers::CONTROL) => {
                state.scroll_offset = (state.scroll_offset + viewport_height / 2).min(max_scroll);
                self.sync_focus_after_scroll(viewport_height);
                None
            }

            // Scroll up half page
            KeyCode::Char('u') if !modifiers.contains(KeyModifiers::CONTROL) => {
                let half_page = viewport_height / 2;
                state.scroll_offset = state.scroll_offset.saturating_sub(half_page);
                self.sync_focus_after_scroll(viewport_height);
                None
            }

            // Page down
            KeyCode::PageDown => {
                state.scroll_offset = (state.scroll_offset + viewport_height).min(max_scroll);
                self.sync_focus_after_scroll(viewport_height);
                None
            }

            // Page up
            KeyCode::PageUp => {
                state.scroll_offset = state.scroll_offset.saturating_sub(viewport_height);
                self.sync_focus_after_scroll(viewport_height);
                None
            }

            // Jump to top
            KeyCode::Char('g') | KeyCode::Home => {
                state.scroll_offset = 0;
                self.sync_focus_after_scroll(viewport_height);
                None
            }

            // Jump to bottom
            KeyCode::Char('G') | KeyCode::End => {
                state.scroll_offset = max_scroll;
                self.sync_focus_after_scroll(viewport_height);
                None
            }

            // Start search
            KeyCode::Char('/') => {
                self.start_view_search();
                None
            }

            // Next match
            KeyCode::Char('n') if !modifiers.contains(KeyModifiers::CONTROL) => {
                if let AppMode::View(ref state) = self.app_mode
                    && state.search_mode == ViewSearchMode::Active
                {
                    self.next_search_match(viewport_height);
                }
                None
            }

            // Previous match
            KeyCode::Char('N') => {
                if let AppMode::View(ref state) = self.app_mode
                    && state.search_mode == ViewSearchMode::Active
                {
                    self.prev_search_match(viewport_height);
                }
                None
            }

            // Toggle tools
            KeyCode::Char('t') => {
                self.toggle_view_tools(viewport_height);
                None
            }

            // Toggle thinking
            KeyCode::Char('T') => {
                self.toggle_view_thinking(viewport_height);
                None
            }

            // Toggle timing (timestamps + durations)
            KeyCode::Char('i') => {
                self.toggle_view_timing(viewport_height);
                None
            }

            // Show path
            KeyCode::Char('p') => {
                if let AppMode::View(ref state) = self.app_mode {
                    self.status_message = Some((
                        state.conversation_path.display().to_string(),
                        std::time::Instant::now(),
                    ));
                }
                None
            }

            // Copy path to clipboard
            KeyCode::Char('Y') => {
                if let AppMode::View(ref state) = self.app_mode {
                    let path_str = state.conversation_path.display().to_string();
                    match crate::tui::export::copy_to_system_clipboard(&path_str) {
                        Ok(()) => {
                            self.status_message = Some((
                                "Path copied to clipboard".to_string(),
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

            // Copy session ID to clipboard
            KeyCode::Char('I') => {
                if let AppMode::View(ref state) = self.app_mode
                    && let Some(id) = state.conversation_path.file_stem().and_then(|s| s.to_str())
                {
                    match crate::tui::export::copy_to_system_clipboard(id) {
                        Ok(()) => {
                            self.status_message = Some((
                                "Session ID copied to clipboard".to_string(),
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

            // Open export menu (save to file)
            KeyCode::Char('e') => {
                self.dialog_mode = DialogMode::ExportMenu { selected: 0 };
                None
            }

            // Yank: copy message if in nav mode, otherwise open yank menu
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

            // Open help overlay
            KeyCode::Char('?') => {
                self.dialog_mode = DialogMode::Help;
                None
            }

            // Ctrl+D - half page down (vim-style, same as 'd')
            KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
                state.scroll_offset = (state.scroll_offset + viewport_height / 2).min(max_scroll);
                self.sync_focus_after_scroll(viewport_height);
                None
            }

            // Ctrl+U - half page up (vim-style, same as 'u')
            KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                let half_page = viewport_height / 2;
                state.scroll_offset = state.scroll_offset.saturating_sub(half_page);
                self.sync_focus_after_scroll(viewport_height);
                None
            }

            // Ctrl+C - quit the app
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => Some(Action::Quit),

            _ => None,
        }
    }

    /// Handle key events while typing a search query
    fn handle_search_typing_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Option<Action> {
        match code {
            // Ctrl+C: cancel search
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                if let AppMode::View(ref mut state) = self.app_mode {
                    state.search_mode = ViewSearchMode::Off;
                    state.search_query.clear();
                    state.search_matches.clear();
                }
                None
            }
            // Ctrl+U: clear entire query
            KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                if let AppMode::View(ref mut state) = self.app_mode
                    && !state.search_query.is_empty()
                {
                    state.search_query.clear();
                    self.update_search_results();
                }
                None
            }
            // Ctrl+W: delete last word
            KeyCode::Char('w') if modifiers.contains(KeyModifiers::CONTROL) => {
                if let AppMode::View(ref mut state) = self.app_mode {
                    let trimmed = state.search_query.trim_end();
                    if let Some(last_space) = trimmed.rfind(|c: char| c.is_whitespace()) {
                        state.search_query.truncate(last_space + 1);
                    } else {
                        state.search_query.clear();
                    }
                }
                self.update_search_results();
                None
            }
            KeyCode::Char(c) => {
                if let AppMode::View(ref mut state) = self.app_mode {
                    state.search_query.push(c);
                }
                self.update_search_results();
                None
            }
            KeyCode::Backspace => {
                if let AppMode::View(ref mut state) = self.app_mode {
                    state.search_query.pop();
                }
                self.update_search_results();
                None
            }
            KeyCode::Enter => {
                if let AppMode::View(ref mut state) = self.app_mode {
                    if !state.search_matches.is_empty() {
                        state.search_mode = ViewSearchMode::Active;
                    } else {
                        state.search_mode = ViewSearchMode::Off;
                    }
                }
                None
            }
            KeyCode::Esc => {
                if let AppMode::View(ref mut state) = self.app_mode {
                    state.search_mode = ViewSearchMode::Off;
                    state.search_query.clear();
                    state.search_matches.clear();
                }
                None
            }
            _ => None,
        }
    }

    /// Handle key events in list mode
    fn handle_list_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        viewport_height: usize,
    ) -> Option<Action> {
        // During loading, allow navigation and typing but not Enter selection
        if self.is_loading() {
            return match code {
                KeyCode::Esc => Some(Action::Quit),
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                    Some(Action::Quit)
                }
                // Ctrl+Left: move cursor one word left
                KeyCode::Left if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.cursor_word_left();
                    None
                }
                // Ctrl+Right: move cursor one word right
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
                // Ctrl+A: cursor to beginning of line
                KeyCode::Char('a') if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.cursor_home();
                    None
                }
                // Ctrl+E: cursor to end of line
                KeyCode::Char('e') if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.cursor_end();
                    None
                }
                // Ctrl+B: cursor left (emacs-style)
                KeyCode::Char('b') if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.cursor_left();
                    None
                }
                // Ctrl+F: cursor right (emacs-style)
                KeyCode::Char('f') if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.cursor_right();
                    None
                }
                // Alt+B: move cursor one word left
                KeyCode::Char('b') if modifiers.contains(KeyModifiers::ALT) => {
                    self.cursor_word_left();
                    None
                }
                // Alt+F: move cursor one word right
                KeyCode::Char('f') if modifiers.contains(KeyModifiers::ALT) => {
                    self.cursor_word_right();
                    None
                }
                // Ctrl+K: kill from cursor to end of line
                KeyCode::Char('k') if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.kill_to_end();
                    None
                }
                // Ctrl+U: kill from beginning of line to cursor
                KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.kill_to_start();
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
                KeyCode::Char('w') if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.delete_word_backwards();
                    None
                }
                // Tab: toggle workspace/global filter
                KeyCode::Tab => {
                    self.toggle_workspace_filter();
                    None
                }
                // Open help overlay
                KeyCode::Char('?') => {
                    self.dialog_mode = DialogMode::Help;
                    None
                }
                // Allow typing during loading - query is buffered for when loading finishes
                KeyCode::Char(c) => {
                    // Insert at cursor position
                    let byte_pos = self
                        .query
                        .char_indices()
                        .nth(self.cursor_pos)
                        .map(|(i, _)| i)
                        .unwrap_or(self.query.len());
                    self.query.insert(byte_pos, c);
                    self.cursor_pos += 1;
                    None
                }
                KeyCode::Backspace => {
                    if self.cursor_pos > 0
                        && let Some((byte_pos, _)) =
                            self.query.char_indices().nth(self.cursor_pos - 1)
                    {
                        self.query.remove(byte_pos);
                        self.cursor_pos -= 1;
                    }
                    None
                }
                KeyCode::Delete => {
                    let len = self.query.chars().count();
                    if self.cursor_pos < len
                        && let Some((byte_pos, _)) = self.query.char_indices().nth(self.cursor_pos)
                    {
                        self.query.remove(byte_pos);
                    }
                    None
                }
                _ => None,
            };
        }

        // Check configurable keybindings before the match block
        if self.keys.delete.matches(code, modifiers) {
            if self.get_selected_path().is_some() {
                self.dialog_mode = DialogMode::ConfirmDelete;
            }
            return None;
        }
        if self.keys.resume.matches(code, modifiers) {
            return self.get_selected_path().map(Action::Resume);
        }
        if self.keys.fork.matches(code, modifiers) {
            return self.get_selected_path().map(Action::ForkResume);
        }
        if self.keys.resume_here.matches(code, modifiers) {
            return self.get_selected_path().map(Action::ResumeHere);
        }

        // Normal handling when ready
        match code {
            KeyCode::Esc => Some(Action::Quit),
            // Enter now triggers view mode entry (handled in run loop)
            KeyCode::Enter => None,
            // Ctrl+Left: move cursor one word left
            KeyCode::Left if modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor_word_left();
                None
            }
            // Ctrl+Right: move cursor one word right
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
            KeyCode::Home => {
                self.select_first();
                None
            }
            KeyCode::End => {
                self.select_last();
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
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
            // Ctrl+A: cursor to beginning of line
            KeyCode::Char('a') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor_home();
                None
            }
            // Ctrl+E: cursor to end of line
            KeyCode::Char('e') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor_end();
                None
            }
            // Ctrl+B: cursor left (emacs-style)
            KeyCode::Char('b') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor_left();
                None
            }
            // Ctrl+F: cursor right (emacs-style)
            KeyCode::Char('f') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor_right();
                None
            }
            // Alt+B: move cursor one word left
            KeyCode::Char('b') if modifiers.contains(KeyModifiers::ALT) => {
                self.cursor_word_left();
                None
            }
            // Alt+F: move cursor one word right
            KeyCode::Char('f') if modifiers.contains(KeyModifiers::ALT) => {
                self.cursor_word_right();
                None
            }
            // Ctrl+K: kill from cursor to end of line
            KeyCode::Char('k') if modifiers.contains(KeyModifiers::CONTROL) => {
                if self.kill_to_end() {
                    self.dispatch_search();
                }
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
            // Ctrl+D - half page down (vim-style)
            KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_half_page_down(viewport_height);
                None
            }
            // Ctrl+U - kill from beginning of line to cursor (emacs-style)
            KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                if self.kill_to_start() {
                    self.dispatch_search();
                }
                None
            }
            // Ctrl+O - select and exit (for scripting, --show-path)
            KeyCode::Char('o') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.get_selected_path().map(Action::Select)
            }
            KeyCode::Char('w') if modifiers.contains(KeyModifiers::CONTROL) => {
                if self.delete_word_backwards() {
                    self.dispatch_search();
                }
                None
            }
            // Tab: toggle workspace/global filter
            KeyCode::Tab => {
                self.toggle_workspace_filter();
                None
            }
            // Open help overlay
            KeyCode::Char('?') => {
                self.dialog_mode = DialogMode::Help;
                None
            }
            KeyCode::Char(c) => {
                // Insert at cursor position
                let byte_pos = self
                    .query
                    .char_indices()
                    .nth(self.cursor_pos)
                    .map(|(i, _)| i)
                    .unwrap_or(self.query.len());
                self.query.insert(byte_pos, c);
                self.cursor_pos += 1;
                self.dispatch_search();
                None
            }
            KeyCode::Backspace => {
                let mut changed = false;
                if self.cursor_pos > 0
                    && let Some((byte_pos, _)) = self.query.char_indices().nth(self.cursor_pos - 1)
                {
                    self.query.remove(byte_pos);
                    self.cursor_pos -= 1;
                    changed = true;
                }
                if changed {
                    self.dispatch_search();
                }
                None
            }
            KeyCode::Delete => {
                let mut changed = false;
                let len = self.query.chars().count();
                if self.cursor_pos < len
                    && let Some((byte_pos, _)) = self.query.char_indices().nth(self.cursor_pos)
                {
                    self.query.remove(byte_pos);
                    changed = true;
                }
                if changed {
                    self.dispatch_search();
                }
                None
            }
            _ => None,
        }
    }

    /// Enter view mode for the currently selected conversation
    pub fn enter_view_mode(&mut self, content_width: usize) {
        use crate::tui::viewer::{RenderOptions, render_conversation};

        let Some(selected) = self.selected else {
            return;
        };
        let Some(&conv_idx) = self.filtered.get(selected) else {
            return;
        };
        let path = self.conversations[conv_idx].path.clone();

        let options = RenderOptions {
            tool_display: self.tool_display,
            show_thinking: self.show_thinking,
            show_timing: self.show_timing,
            content_width,
        };

        match render_conversation(&path, &options) {
            Ok(rendered) => {
                let total_lines = rendered.lines.len();
                let first_msg = if rendered.messages.is_empty() {
                    None
                } else {
                    Some(0)
                };
                self.app_mode = AppMode::View(ViewState {
                    conversation_path: path,
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
                });
            }
            Err(e) => {
                self.status_message =
                    Some((format!("Failed to open: {}", e), std::time::Instant::now()));
            }
        }
    }

    /// Exit view mode and return to list
    pub fn exit_view_mode(&mut self) {
        self.app_mode = AppMode::List;
    }

    /// Start search mode in view
    fn start_view_search(&mut self) {
        if let AppMode::View(ref mut state) = self.app_mode {
            state.search_mode = ViewSearchMode::Typing;
            state.search_query.clear();
            state.search_matches.clear();
            state.current_match = 0;
        }
    }

    /// Update search results based on current query
    fn update_search_results(&mut self) {
        if let AppMode::View(ref mut state) = self.app_mode {
            let query_lower = state.search_query.to_lowercase();
            if query_lower.is_empty() {
                state.search_matches.clear();
                return;
            }

            state.search_matches = state
                .rendered_lines
                .iter()
                .enumerate()
                .filter(|(_, line)| line_matches_query(line, &query_lower))
                .map(|(i, _)| i)
                .collect();

            // Jump to first match if any
            if !state.search_matches.is_empty() {
                state.current_match = 0;
                let match_line = state.search_matches[0];
                state.scroll_offset = match_line;
                Self::focus_message_at_line(state, match_line);
            }
        }
    }

    /// Go to next search match
    fn next_search_match(&mut self, viewport_height: usize) {
        if let AppMode::View(ref mut state) = self.app_mode {
            if state.search_matches.is_empty() {
                return;
            }
            state.current_match = (state.current_match + 1) % state.search_matches.len();
            let match_line = state.search_matches[state.current_match];
            // Scroll to show match in viewport
            if match_line < state.scroll_offset
                || match_line >= state.scroll_offset + viewport_height
            {
                state.scroll_offset = match_line;
            }
            Self::focus_message_at_line(state, match_line);
        }
    }

    /// Go to previous search match
    fn prev_search_match(&mut self, viewport_height: usize) {
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

    /// Cycle tool display mode in view mode
    fn toggle_view_tools(&mut self, viewport_height: usize) {
        if let AppMode::View(ref mut state) = self.app_mode {
            state.tool_display = state.tool_display.next();
            self.tool_display = state.tool_display; // Persist at app level
            self.re_render_view(viewport_height);
        }
    }

    /// Toggle thinking visibility in view mode
    fn toggle_view_thinking(&mut self, viewport_height: usize) {
        if let AppMode::View(ref mut state) = self.app_mode {
            state.show_thinking = !state.show_thinking;
            self.show_thinking = state.show_thinking; // Persist at app level
            self.re_render_view(viewport_height);
        }
    }

    /// Toggle timing visibility in view mode (timestamps + durations)
    fn toggle_view_timing(&mut self, viewport_height: usize) {
        if let AppMode::View(ref mut state) = self.app_mode {
            state.show_timing = !state.show_timing;
            self.show_timing = state.show_timing; // Persist at app level
            self.re_render_view(viewport_height);
        }
    }

    /// Re-render the view with current toggle settings
    fn re_render_view(&mut self, viewport_height: usize) {
        use crate::tui::viewer::{RenderOptions, render_conversation};

        if let AppMode::View(ref mut state) = self.app_mode {
            let options = RenderOptions {
                tool_display: state.tool_display,
                show_thinking: state.show_thinking,
                show_timing: state.show_timing,
                content_width: state.content_width,
            };

            if let Ok(rendered) = render_conversation(&state.conversation_path, &options) {
                let old_scroll = state.scroll_offset;
                // Preserve focus across re-render by saving the entry_index
                let old_entry_index = state
                    .focused_message
                    .and_then(|i| state.message_ranges.get(i))
                    .map(|m| m.entry_index);

                state.total_lines = rendered.lines.len();
                state.rendered_lines = rendered.lines;
                state.message_ranges = rendered.messages;

                // Restore focused message by entry_index
                state.focused_message = old_entry_index.and_then(|old_idx| {
                    state
                        .message_ranges
                        .iter()
                        .position(|m| m.entry_index == old_idx)
                });
                // If old focus not found, default to first message
                if state.focused_message.is_none() && !state.message_ranges.is_empty() {
                    state.focused_message = Some(0);
                }

                // Clamp scroll offset to new content bounds
                let max_scroll = state.total_lines.saturating_sub(viewport_height);
                state.scroll_offset = old_scroll.min(max_scroll);

                // Recompute search matches for new content
                if state.search_mode == ViewSearchMode::Active && !state.search_query.is_empty() {
                    let query_lower = state.search_query.to_lowercase();
                    state.search_matches = state
                        .rendered_lines
                        .iter()
                        .enumerate()
                        .filter(|(_, line)| line_matches_query(line, &query_lower))
                        .map(|(i, _)| i)
                        .collect();

                    // Clamp current_match to valid range
                    if state.search_matches.is_empty() {
                        state.current_match = 0;
                    } else {
                        state.current_match =
                            state.current_match.min(state.search_matches.len() - 1);
                    }
                }
            }
        }
    }

    /// Jump to the next message (activates message nav mode)
    fn focus_next_message(&mut self, viewport_height: usize) {
        if let AppMode::View(ref mut state) = self.app_mode {
            if state.message_ranges.is_empty() {
                return;
            }
            // On first activation, sync focus to current scroll position
            if !state.message_nav_active {
                state.message_nav_active = true;
                Self::sync_focus_to_scroll(state, viewport_height);
            }
            let next = match state.focused_message {
                Some(i) if i + 1 < state.message_ranges.len() => i + 1,
                Some(i) => i, // already at last
                None => 0,
            };
            state.focused_message = Some(next);
            Self::ensure_message_visible(state, viewport_height);
        }
    }

    /// Jump to the previous message (activates message nav mode)
    fn focus_prev_message(&mut self, viewport_height: usize) {
        if let AppMode::View(ref mut state) = self.app_mode {
            if state.message_ranges.is_empty() {
                return;
            }
            // On first activation, sync focus to current scroll position
            if !state.message_nav_active {
                state.message_nav_active = true;
                Self::sync_focus_to_scroll(state, viewport_height);
            }
            let prev = match state.focused_message {
                Some(i) if i > 0 => i - 1,
                Some(i) => i, // already at first
                None => 0,
            };
            state.focused_message = Some(prev);
            Self::ensure_message_visible(state, viewport_height);
        }
    }

    /// Focus the message containing the given line index, activating nav mode
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

    /// Sync focus after a scroll operation (only when message nav is active)
    fn sync_focus_after_scroll(&mut self, viewport_height: usize) {
        if let AppMode::View(ref mut state) = self.app_mode
            && state.message_nav_active
        {
            Self::sync_focus_to_scroll(state, viewport_height);
        }
    }

    /// Sync focused message to the current scroll position
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

    /// Scroll viewport to make the focused message visible
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

    /// Copy the currently focused message to clipboard
    fn copy_focused_message(&mut self, viewport_height: usize) {
        // Activate nav mode and sync focus if needed
        if let AppMode::View(ref mut state) = self.app_mode
            && !state.message_nav_active
        {
            state.message_nav_active = true;
            Self::sync_focus_to_scroll(state, viewport_height);
        }

        let (path, entry_index) = if let AppMode::View(ref state) = self.app_mode {
            if let Some(idx) = state.focused_message {
                if let Some(msg) = state.message_ranges.get(idx) {
                    (state.conversation_path.clone(), msg.entry_index)
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

        match crate::tui::export::extract_message_text(&path, entry_index, options) {
            Ok(text) if text.is_empty() => {
                self.status_message = Some((
                    "No text content in this message".to_string(),
                    std::time::Instant::now(),
                ));
            }
            Ok(text) => match crate::tui::export::copy_to_system_clipboard(&text) {
                Ok(()) => {
                    self.status_message = Some((
                        "Message copied to clipboard".to_string(),
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

    /// Check if view needs re-render due to width change
    pub fn check_view_resize(&mut self, new_content_width: usize, viewport_height: usize) {
        if let AppMode::View(ref mut state) = self.app_mode
            && state.content_width != new_content_width
        {
            state.content_width = new_content_width;
            self.re_render_view(viewport_height);
        }
    }
}

/// RAII guard to ensure terminal is restored on exit
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

/// Check if a rendered line matches the search query by concatenating all span texts.
/// This allows multi-word queries to match across span boundaries.
pub fn line_matches_query(line: &RenderedLine, query_lower: &str) -> bool {
    let full_text: String = line.spans.iter().map(|(text, _)| text.as_str()).collect();
    full_text.to_lowercase().contains(query_lower)
}

impl TerminalGuard {
    fn new() -> Result<Self> {
        terminal::enable_raw_mode().map_err(|e| AppError::Io(io::Error::other(e)))?;

        let mut stdout = io::stdout();
        if let Err(e) = crossterm::execute!(stdout, EnterAlternateScreen) {
            let _ = terminal::disable_raw_mode();
            return Err(AppError::Io(io::Error::other(e)));
        }

        let backend = CrosstermBackend::new(stdout);
        let terminal = match Terminal::new(backend) {
            Ok(t) => t,
            Err(e) => {
                let _ = terminal::disable_raw_mode();
                let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen);
                return Err(AppError::Io(io::Error::other(e)));
            }
        };

        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = crossterm::execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
    }
}

/// Name column width for ledger-style display
const NAME_WIDTH: usize = 9;

/// Maximum events to drain in a single batch to avoid starving redraws
const MAX_EVENT_BATCH: usize = 256;

/// Read all immediately available events after an initial blocking wait.
///
/// When pasting text, crossterm delivers each character as a separate KeyEvent.
/// Without batching, each character triggers a full redraw before reading the next,
/// making paste visibly slow. This function drains all ready events so the caller
/// can process them all before a single redraw.
fn drain_events(wait: Duration) -> Result<Vec<Event>> {
    if !event::poll(wait).map_err(|e| AppError::Io(io::Error::other(e)))? {
        return Ok(Vec::new());
    }

    let mut events = vec![event::read().map_err(|e| AppError::Io(io::Error::other(e)))?];

    while events.len() < MAX_EVENT_BATCH
        && event::poll(Duration::ZERO).map_err(|e| AppError::Io(io::Error::other(e)))?
    {
        events.push(event::read().map_err(|e| AppError::Io(io::Error::other(e)))?);
    }

    Ok(events)
}

/// Run the TUI with background loading
/// Returns the action and the final list of conversations
pub fn run_with_loader(
    rx: Receiver<LoaderMessage>,
    tool_display: ToolDisplayMode,
    show_thinking: bool,
    keys: KeyBindings,
    workspace_filter: bool,
    current_project_dir_name: Option<String>,
) -> Result<(Action, Vec<Conversation>)> {
    // Set up panic hook to restore terminal
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = terminal::disable_raw_mode();
        let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(panic_info);
    }));

    let mut guard = TerminalGuard::new()?;
    let mut app = App::new_loading(
        tool_display,
        show_thinking,
        keys,
        workspace_filter,
        current_project_dir_name,
    );

    loop {
        // Process all pending loader messages (non-blocking)
        loop {
            match rx.try_recv() {
                Ok(LoaderMessage::Fatal(err)) => {
                    // Fatal error - restore terminal and return error
                    drop(guard);
                    return Err(err);
                }
                Ok(LoaderMessage::ProjectError) => {
                    // Logged by loader, continue
                }
                Ok(LoaderMessage::Batch(convs)) => {
                    app.append_conversations(convs);
                }
                Ok(LoaderMessage::Done) => {
                    app.finish_loading();
                    // Check for empty conversations
                    if app.conversations().is_empty() {
                        drop(guard);
                        return Err(AppError::NoHistoryFound("selected scope".to_string()));
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // Loader finished unexpectedly
                    if app.is_loading() {
                        app.finish_loading();
                        if app.conversations().is_empty() {
                            drop(guard);
                            return Err(AppError::NoHistoryFound("selected scope".to_string()));
                        }
                    }
                    break;
                }
            }
        }

        let frame_area = guard.terminal.get_frame().area();
        let viewport_height = frame_area.height.saturating_sub(3) as usize;
        let content_width = (frame_area.width as usize)
            .saturating_sub(NAME_WIDTH + 3 + crate::tui::viewer::GUTTER_WIDTH);

        // Check for resize in view mode
        app.check_view_resize(content_width, viewport_height);

        // Pick up any completed search results from the background worker
        app.receive_search_results();

        // Render current state
        guard.terminal.draw(|frame| ui::render(frame, &app))?;

        // Use short poll timeout while loading or search is in-flight,
        // otherwise block until input arrives (or until status message expires)
        let poll_timeout = if app.is_loading() {
            Duration::from_millis(50)
        } else if app.search_in_flight {
            // Poll frequently so search results appear quickly (within ~8ms)
            Duration::from_millis(8)
        } else if let Some(remaining) = app.status_message_remaining() {
            remaining
        } else {
            Duration::from_secs(3600)
        };

        // Drain all currently queued events and process them, then redraw.
        // drain_events coalesces events that arrive during rendering (e.g. paste),
        // while always returning to the outer loop for a redraw after each batch.
        let events = drain_events(poll_timeout)?;
        for ev in events {
            let Event::Key(key) = ev else { continue };
            if key.kind != KeyEventKind::Press {
                continue;
            }

            // Check for Enter in list mode - enter view mode (but not during dialogs)
            if matches!(app.app_mode(), AppMode::List)
                && *app.dialog_mode() == DialogMode::None
                && key.code == KeyCode::Enter
                && !app.is_loading()
                && app.selected().is_some()
            {
                app.enter_view_mode(content_width);
                break; // mode transition: redraw before processing more events
            }

            if let Some(action) = app.handle_key(key.code, key.modifiers, viewport_height) {
                match action {
                    Action::Delete(ref path) => {
                        // Extract UUID from filename and delete session
                        // (removes .jsonl + session dir with tool-results/subagents)
                        let uuid = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                        match crate::history::delete_session_by_uuid(uuid) {
                            Ok(_) => {
                                // Only remove from list if deletion succeeded
                                app.remove_selected_from_list();
                                // If in view mode, return to list
                                app.exit_view_mode();
                            }
                            Err(e) => {
                                let _ = debug_log::log_debug(&format!(
                                    "Failed to delete session {}: {}",
                                    uuid, e
                                ));
                                // Keep item in list since file still exists
                            }
                        }
                        // Continue the loop (don't exit TUI)
                    }
                    _ => return Ok((action, app.into_conversations())),
                }
            }
        }
    }
}

/// Run the TUI for a single file (direct input mode)
pub fn run_single_file(
    path: PathBuf,
    tool_display: ToolDisplayMode,
    show_thinking: bool,
    keys: KeyBindings,
) -> Result<()> {
    // Set up panic hook to restore terminal
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = terminal::disable_raw_mode();
        let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(panic_info);
    }));

    let mut guard = TerminalGuard::new()?;
    let mut app = App::new_single_file(path, tool_display, show_thinking, keys);

    loop {
        let frame_area = guard.terminal.get_frame().area();
        let viewport_height = frame_area.height.saturating_sub(3) as usize;
        let content_width = (frame_area.width as usize)
            .saturating_sub(NAME_WIDTH + 3 + crate::tui::viewer::GUTTER_WIDTH);

        // Check for resize in view mode (this triggers initial render too)
        app.check_view_resize(content_width, viewport_height);

        guard.terminal.draw(|frame| ui::render(frame, &app))?;

        let events = drain_events(Duration::from_secs(3600))?;
        for ev in events {
            let Event::Key(key) = ev else { continue };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if let Some(Action::Quit) = app.handle_key(key.code, key.modifiers, viewport_height) {
                return Ok(());
            }
        }
    }
}
