use crate::history::model::Transcript;
use crate::search::query::ParsedQuery;
use crate::semantic::types::{SemanticExplanation, SemanticScoreBreakdown};
use crate::tui::viewer::{MessageRange, RenderedLine, ToolDisplayMode, ToolOutputId};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

/// Result of running the TUI
pub enum Action {
    Select(PathBuf),
    Delete(PathBuf),
    Resume(PathBuf),
    ForkResume(PathBuf),
    Refresh,
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
    Help { scroll: usize },
    /// Semantic result debug details
    SemanticDebug,
    /// Rename the selected conversation
    Rename { input: String, cursor: usize },
    /// Subagents and forks of the conversation in the viewer, by index
    /// into `ViewFamily::children`
    Children { selected: usize },
}

/// The conversations related to the one in the viewer, as rows of the app's
/// conversations.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ViewFamily {
    /// The conversation it was started or branched from.
    pub parent: Option<usize>,
    /// Its subagents and forks, oldest first.
    pub children: Vec<usize>,
}

/// Main application mode
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
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
    /// The conversation open in the view, once decoded.
    pub transcript: Option<Arc<Transcript>>,
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
    /// Tool outputs expanded independently from global tool display mode
    pub expanded_tool_outputs: BTreeSet<ToolOutputId>,
    /// Tool output currently under the mouse cursor
    pub hovered_tool_output: Option<ToolOutputId>,
    pub text_selection: Option<crate::tui::selection::TextSelection>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TuiSearchOptions {
    pub default_mode: ListSearchMode,
    /// Whether the list starts out showing automated and child
    /// conversations.
    pub all_kinds: bool,
}

impl Default for TuiSearchOptions {
    fn default() -> Self {
        Self {
            default_mode: ListSearchMode::Lexical,
            all_kinds: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ListSearchMode {
    #[default]
    Lexical,
    Semantic,
}

impl ViewState {
    pub(super) fn initial(
        conversation_path: PathBuf,
        tool_display: ToolDisplayMode,
        show_thinking: bool,
        show_timing: bool,
        content_width: usize,
    ) -> Self {
        Self {
            conversation_path,
            transcript: None,
            scroll_offset: 0,
            rendered_lines: Vec::new(),
            total_lines: 0,
            tool_display,
            show_thinking,
            show_timing,
            content_width,
            search_mode: ViewSearchMode::Off,
            search_query: String::new(),
            search_matches: Vec::new(),
            current_match: 0,
            message_ranges: Vec::new(),
            focused_message: None,
            message_nav_active: false,
            expanded_tool_outputs: BTreeSet::new(),
            hovered_tool_output: None,
            text_selection: None,
        }
    }
}

impl ListSearchMode {
    pub fn label(self) -> &'static str {
        match self {
            ListSearchMode::Lexical => "lex",
            ListSearchMode::Semantic => "sem",
        }
    }
}

pub const LIST_LINES_PER_ITEM: usize = 3;

pub fn list_lines_per_item(_mode: ListSearchMode, query: &str) -> usize {
    let parsed = ParsedQuery::parse(query);
    if !parsed.literals().is_empty() {
        4
    } else {
        LIST_LINES_PER_ITEM
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum SemanticProgress {
    #[default]
    Idle,
    /// Loading the full conversations semantic search ranks.
    LoadingSessions,
    InitializingModel,
    CacheReady,
    Embedding {
        completed: usize,
        total: usize,
    },
    Ranking,
    Complete,
    EmptyCorpus,
    Failed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticResultMetadata {
    pub score_breakdown: SemanticScoreBreakdown,
    pub explanation: SemanticExplanation,
}

/// Which parts of the list are usable. Each part becomes ready on its own.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Readiness {
    /// Rows are shown newest first and can be acted on.
    pub list: bool,
    /// Lexical queries run, against the generation index or the loaded
    /// conversations.
    pub lexical: bool,
    /// Whether the store is being reconciled with the session files.
    pub reconcile: ReconcileState,
    /// The semantic worker can rank its corpus.
    pub semantic: bool,
}

impl Readiness {
    pub fn ready() -> Self {
        Self {
            list: true,
            lexical: true,
            reconcile: ReconcileState::Done,
            semantic: true,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum ReconcileState {
    /// A reconcile pass may publish a newer generation.
    #[default]
    Running,
    Done,
}
