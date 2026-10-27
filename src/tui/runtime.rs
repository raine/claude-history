use super::app::{Action, App, AppMode, DialogMode, ListSearchMode, TuiSearchOptions};
use super::terminal_out::{StderrRedirect, TerminalOut};
use super::ui;
use crate::config::{KeyBindings, ResumeConfig};
use crate::debug_log;
use crate::error::{AppError, Result};
use crate::history::Conversation;
use crate::time_filter::TimeFilter;
use crate::tui::viewer::ToolDisplayMode;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
    MouseEvent, MouseEventKind,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::prelude::*;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<TerminalOut>>,
    /// Dropped after the terminal is restored.
    _stderr: StderrRedirect,
}

impl TerminalGuard {
    fn new() -> Result<Self> {
        terminal::enable_raw_mode().map_err(|e| AppError::Io(io::Error::other(e)))?;
        let stderr = StderrRedirect::new();

        if let Err(e) = crossterm::execute!(TerminalOut, EnterAlternateScreen, EnableMouseCapture) {
            let _ = terminal::disable_raw_mode();
            return Err(AppError::Io(io::Error::other(e)));
        }

        let backend = CrosstermBackend::new(TerminalOut);
        let terminal = match Terminal::new(backend) {
            Ok(t) => t,
            Err(e) => {
                let _ = terminal::disable_raw_mode();
                let _ = crossterm::execute!(TerminalOut, DisableMouseCapture, LeaveAlternateScreen);
                return Err(AppError::Io(io::Error::other(e)));
            }
        };

        Ok(Self {
            terminal,
            _stderr: stderr,
        })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            self.terminal.backend_mut(),
            DisableMouseCapture,
            LeaveAlternateScreen
        );
    }
}

const NAME_WIDTH: usize = 9;

fn calculate_content_width(frame_width: usize, show_timing: bool) -> usize {
    let timing_width = if show_timing {
        crate::tui::viewer::TIMESTAMP_WIDTH
    } else {
        0
    };
    frame_width.saturating_sub(NAME_WIDTH + 3 + crate::tui::viewer::GUTTER_WIDTH + timing_width)
}

struct FrameState {
    frame_area: Rect,
    viewport_height: usize,
    content_width: usize,
}

enum EventLoopResult<T> {
    Continue,
    Break,
    Return(T),
}

/// Everything the list event loop waits on, merged into one channel so the
/// loop can block until there is work.
enum RuntimeEvent {
    Input(Event),
    InputError(io::Error),
    /// A reconcile process exited, with this status if it could be waited
    /// on.
    Reconciled(Option<std::process::ExitStatus>),
    /// The list's preview worker has a reply to draw.
    PreviewReady,
}

/// Most queued events handled before drawing again, so a long backlog still
/// shows progress.
const MAX_EVENTS_PER_FRAME: usize = 64;

/// Whether handling events queued after `event` needs the frame it changes
/// drawn first: its handler reads the layout of the frame that was drawn.
fn needs_fresh_frame(event: &RuntimeEvent) -> bool {
    matches!(
        event,
        RuntimeEvent::Input(Event::Resize(..))
            | RuntimeEvent::Input(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(_),
                ..
            }))
    )
}

/// How long the input thread waits for terminal input before checking
/// whether it should stop.
const INPUT_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Forwards terminal input to the runtime channel. Dropping it stops and
/// joins the thread, so no input is consumed after the TUI exits.
struct InputReader {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl InputReader {
    fn spawn(tx: Sender<RuntimeEvent>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let handle = std::thread::Builder::new()
            .name("tui-input".into())
            .spawn(move || {
                while !thread_stop.load(Ordering::Relaxed) {
                    let event = match event::poll(INPUT_POLL_INTERVAL) {
                        Ok(false) => continue,
                        Ok(true) => event::read().map(RuntimeEvent::Input),
                        Err(err) => Err(err),
                    };
                    let event = event.unwrap_or_else(RuntimeEvent::InputError);
                    let failed = matches!(event, RuntimeEvent::InputError(_));
                    if tx.send(event).is_err() || failed {
                        return;
                    }
                }
            })
            .ok();
        Self { stop, handle }
    }
}

impl Drop for InputReader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn read_event(wait: Duration) -> Result<Option<Event>> {
    if !event::poll(wait).map_err(|e| AppError::Io(io::Error::other(e)))? {
        return Ok(None);
    }

    event::read()
        .map(Some)
        .map_err(|e| AppError::Io(io::Error::other(e)))
}

fn prepare_frame(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<TerminalOut>>,
) -> FrameState {
    let frame_area = terminal.get_frame().area();
    let viewport_height = frame_area.height.saturating_sub(3) as usize;
    let content_width = calculate_content_width(frame_area.width as usize, app.show_timing());

    app.check_view_resize(content_width, viewport_height);
    if matches!(app.app_mode(), AppMode::List) {
        let layout = ui::list_layout_rects(frame_area, app);
        if let Some(preview) = layout.preview {
            app.fill_list_preview(
                ui::list_preview_content_width(preview, app.show_timing()),
                super::preview_worker::SAME_FRAME_BUDGET,
            );
        }
        let lines_per_item = super::app::list_lines_per_item(app.list_search_mode(), app.query());
        app.fill_visible_evidence(layout.list.height as usize / lines_per_item.max(1));
    }
    let viewport_height = match app.app_mode() {
        AppMode::View(state) => {
            ui::view_layout_rects(frame_area, app, state).content.height as usize
        }
        AppMode::List => viewport_height,
    };

    FrameState {
        frame_area,
        viewport_height,
        content_width,
    }
}

fn draw_frame(app: &App, terminal: &mut Terminal<CrosstermBackend<TerminalOut>>) -> Result<()> {
    terminal.draw(|frame| ui::render(frame, app))?;
    Ok(())
}

fn handle_event<F>(
    app: &mut App,
    frame_state: &FrameState,
    ev: Event,
    allow_list_click_enter: bool,
    mut on_action: F,
) -> EventLoopResult<Option<Action>>
where
    F: FnMut(&mut App, Action) -> EventLoopResult<Option<Action>>,
{
    let key = match ev {
        Event::Key(k) if k.kind == KeyEventKind::Press => k,
        Event::Mouse(m) => {
            match m.kind {
                MouseEventKind::ScrollDown => {
                    app.scroll_mouse(3, frame_state.viewport_height);
                }
                MouseEventKind::ScrollUp => {
                    app.scroll_mouse(-3, frame_state.viewport_height);
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if app.handle_view_click(
                        m.row,
                        frame_state.frame_area,
                        frame_state.viewport_height,
                    ) {
                        return EventLoopResult::Break;
                    }
                    if allow_list_click_enter
                        && app.handle_list_click(m.column, m.row, frame_state.frame_area)
                    {
                        app.enter_view_mode(frame_state.content_width);
                        return EventLoopResult::Break;
                    }
                }
                MouseEventKind::Moved => {
                    app.handle_view_mouse_move(m.row, frame_state.frame_area);
                    if allow_list_click_enter {
                        app.handle_list_mouse_move(m.column, m.row, frame_state.frame_area);
                    }
                }
                _ => {}
            }
            return EventLoopResult::Continue;
        }
        _ => return EventLoopResult::Continue,
    };

    if allow_list_click_enter
        && matches!(app.app_mode(), AppMode::List)
        && *app.dialog_mode() == DialogMode::None
        && key.code == KeyCode::Enter
        && app.readiness().list
        && app.selected().is_some()
    {
        app.enter_view_mode(frame_state.content_width);
        return EventLoopResult::Break;
    }

    let Some(action) = app.handle_key(key.code, key.modifiers, frame_state.viewport_height) else {
        return EventLoopResult::Continue;
    };
    on_action(app, action)
}

/// The store the list is read from: generations under `root`, kept up to
/// date by reconcile processes.
struct Store {
    root: PathBuf,
    show_last: bool,
    time_filter: TimeFilter,
    /// Name of the generation shown.
    shown: Option<String>,
    /// A reconcile process is running.
    running: bool,
    /// Another pass was asked for while one was running.
    again: bool,
    /// When to retry a pass that found the store locked by another process.
    retry_at: Option<Instant>,
}

/// How often `CURRENT` is checked while a reconcile pass may publish.
const GENERATION_POLL: Duration = Duration::from_millis(200);
/// How long to wait before retrying a pass that found the store locked.
const BUSY_RETRY: Duration = Duration::from_secs(1);

impl Store {
    fn new(show_last: bool, time_filter: TimeFilter) -> Option<Self> {
        Some(Self {
            root: crate::store::default_root()?,
            show_last,
            time_filter,
            shown: None,
            running: false,
            again: false,
            retry_at: None,
        })
    }

    fn open(&self) -> Option<crate::store::Snapshot> {
        crate::store::Snapshot::open(&self.root, self.show_last, self.time_filter)
    }

    /// Shows the generation `CURRENT` names if it is not the one shown.
    /// Returns whether it did.
    fn show_current(&mut self, app: &mut App) -> bool {
        let current = crate::store::Generation::current_name(&self.root);
        if current.is_none() || current == self.shown {
            return false;
        }
        let Some(cards) = self.open() else {
            return false;
        };
        self.shown = Some(cards.generation_name().to_owned());
        if app.generation_len().is_none() {
            app.show_snapshot(cards);
            app.open_search_index();
            true
        } else {
            app.swap_generation(cards)
        }
    }

    /// Starts a reconcile pass, or another one after the running one.
    /// `Reconciled` is sent when it exits.
    fn reconcile(&mut self, tx: &Sender<RuntimeEvent>) {
        self.retry_at = None;
        if self.running {
            self.again = true;
            return;
        }
        let mut child = match spawn_reconciler() {
            Ok(child) => child,
            Err(err) => {
                let _ = debug_log::log_debug(&format!("Failed to start reconcile: {err}"));
                return;
            }
        };
        let tx = tx.clone();
        let spawned = std::thread::Builder::new()
            .name("reconcile-wait".into())
            .spawn(move || {
                let status = child.wait().ok();
                let _ = tx.send(RuntimeEvent::Reconciled(status));
            });
        self.running = spawned.is_ok();
    }

    /// Records that the running pass exited with `status`, and starts the
    /// next one when one was asked for or the store was locked. Returns why
    /// the pass failed, if it did.
    fn reconciled(
        &mut self,
        status: Option<std::process::ExitStatus>,
        tx: &Sender<RuntimeEvent>,
    ) -> Option<String> {
        self.running = false;
        let mut failure = None;
        match status {
            Some(status) if status.code() == Some(RECONCILE_BUSY) => {
                self.retry_at = Some(Instant::now() + BUSY_RETRY);
            }
            Some(status) if !status.success() => {
                let _ = debug_log::log_debug(&format!("Reconcile failed: {status}"));
                failure = Some(match crate::store::progress::read(&self.root) {
                    Some(crate::store::progress::Status::Failed(message)) => message,
                    _ => status.to_string(),
                });
            }
            _ => {}
        }
        if std::mem::take(&mut self.again) {
            self.reconcile(tx);
        }
        failure
    }

    /// Progress of the pass indexing the store, by this process or another,
    /// while one may run.
    fn progress(&self) -> Option<crate::store::progress::Indexing> {
        if !self.pending() {
            return None;
        }
        match crate::store::progress::read(&self.root)? {
            crate::store::progress::Status::Indexing(progress) => Some(progress),
            crate::store::progress::Status::Failed(_) => None,
        }
    }

    /// Shows whether the store is being reconciled and how far indexing got.
    fn show_progress(&self, app: &mut App) {
        app.set_reconciling(self.pending());
        app.set_index_progress(self.progress(), self.pending());
    }

    /// Whether a pass is running or due, so `CURRENT` may change.
    fn pending(&self) -> bool {
        self.running || self.retry_at.is_some()
    }

    /// Starts the pass retried after a busy lock once it is due.
    fn retry_due(&mut self, tx: &Sender<RuntimeEvent>) {
        if self.retry_at.is_some_and(|at| at <= Instant::now()) {
            self.reconcile(tx);
        }
    }
}

/// Exit status of a reconcile process that found the store locked by
/// another process (`EX_TEMPFAIL`).
const RECONCILE_BUSY: i32 = 75;

/// Starts `claude-history --reconcile` in its own session, so it keeps
/// running when the TUI exits or replaces itself with an agent.
fn spawn_reconciler() -> io::Result<std::process::Child> {
    use std::process::{Command, Stdio};
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--reconcile")
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    if crate::bench::enabled() {
        command
            .env("CLAUDE_HISTORY_BENCH", "1")
            .stderr(Stdio::inherit());
    } else {
        command.stderr(Stdio::null());
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe and touches no Rust state.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    command.spawn()
}

/// Entry point of the reconcile process: brings the store up to date with
/// the session files at low priority (see `store::reconcile`).
pub fn reconcile() -> Result<()> {
    #[cfg(unix)]
    // SAFETY: lowers this process's own scheduling priority.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
    let Some(root) = crate::store::default_root() else {
        return Ok(());
    };
    crate::bench::mark("reconcile_start");
    let sources = crate::history::sources::Registry::from_env();
    let outcome = match crate::store::reconcile::reconcile(&root, &sources) {
        Ok(outcome) => outcome,
        Err(err) => {
            crate::store::progress::record_failure(&root, &err.to_string());
            return Err(err.into());
        }
    };
    crate::bench::mark("reconcile_done");
    if outcome.publish == crate::store::generation::Publish::Busy {
        std::process::exit(RECONCILE_BUSY);
    }
    if crate::bench::enabled() {
        eprintln!(
            "bench reconcile_bytes_written {}B\nbench reconcile_parsed {}\nbench reconcile_published {}",
            outcome.written.bytes, outcome.parsed, outcome.published
        );
    }
    crate::bench::mark_peak_rss_as("reconcile_peak_rss");
    let _ = debug_log::log_debug(&format!("Reconcile: {outcome:?}"));
    Ok(())
}

/// Loads every conversation inside `time_filter` with the full loader, for
/// runs without a store.
fn load_in_memory(show_last: bool, time_filter: TimeFilter) -> Result<Vec<Conversation>> {
    let mut conversations = crate::store::load_in_memory(show_last)?;
    if time_filter.is_active() {
        conversations.retain(|conversation| time_filter.matches(conversation.timestamp));
    }
    Ok(conversations)
}

/// Runs the list TUI and returns the action that ended it, with the
/// conversation it applies to.
#[allow(clippy::too_many_arguments)]
pub fn run_with_loader(
    show_last: bool,
    time_filter: TimeFilter,
    tool_display: ToolDisplayMode,
    show_thinking: bool,
    keys: KeyBindings,
    delete_enabled: bool,
    workspace_filter: bool,
    workspace: Option<crate::history::Workspace>,
    exclude_projects: Vec<String>,
    exclude_paths: Vec<String>,
    search_options: TuiSearchOptions,
    resume_config: &ResumeConfig,
) -> Result<(Action, Option<Conversation>)> {
    let mut store = Store::new(show_last, time_filter);
    let mut guard = TerminalGuard::new()?;
    let (events_tx, events_rx) = mpsc::channel();
    let _input = InputReader::spawn(events_tx.clone());
    let mut app = App::new_loading_with_options(
        tool_display,
        show_thinking,
        keys,
        workspace_filter,
        workspace,
        exclude_projects,
        exclude_paths,
        search_options,
    );
    app.set_delete_enabled(delete_enabled);
    let no_history = || AppError::NoHistoryFound("selected scope".to_string());
    match &mut store {
        Some(store) => {
            if let Some(cards) = store.open() {
                store.shown = Some(cards.generation_name().to_owned());
                app.show_snapshot(cards);
            }
        }
        None => {
            let conversations = load_in_memory(show_last, time_filter)?;
            if conversations.is_empty() {
                return Err(no_history());
            }
            app.show_conversations(conversations);
        }
    }
    prepare_frame(&mut app, &mut guard.terminal);
    draw_frame(&app, &mut guard.terminal)?;
    // After the first draw, which detected the theme the worker renders with.
    let preview_tx = events_tx.clone();
    app.start_list_preview_worker(Box::new(move || {
        let _ = preview_tx.send(RuntimeEvent::PreviewReady);
    }));
    let mut reported_first_rows = report_first_rows(&app);
    if app.open_search_index() {
        crate::bench::mark("index_open");
    }
    if let Some(store) = &mut store {
        store.reconcile(&events_tx);
        app.set_reconciling(store.pending());
    }
    // Generation size before a refresh, while one runs.
    let mut refreshing: Option<usize> = None;
    // The first pass has not finished; an empty list may still fill.
    let mut starting = true;

    let mut frame_state = prepare_frame(&mut app, &mut guard.terminal);
    // Event taken from the queue to handle before drawing again
    let mut queued: Option<RuntimeEvent> = None;
    let mut handled_since_draw = 0;
    loop {
        if queued.is_none() {
            frame_state = prepare_frame(&mut app, &mut guard.terminal);
            draw_frame(&app, &mut guard.terminal)?;
            handled_since_draw = 0;
            if !reported_first_rows {
                reported_first_rows = report_first_rows(&app);
            }
            if app.receive_search_results() {
                frame_state = prepare_frame(&mut app, &mut guard.terminal);
                draw_frame(&app, &mut guard.terminal)?;
            }
        }
        if let Some(store) = &mut store {
            if app.take_reconcile_request() {
                store.reconcile(&events_tx);
            }
            store.retry_due(&events_tx);
            store.show_current(&mut app);
            store.show_progress(&mut app);
        }

        // Search results arrive on the workers' own channels, so poll for
        // them while a search runs, and poll `CURRENT` while a reconcile
        // pass may publish; otherwise sleep until an event arrives or the
        // status message expires.
        let wait = [
            app.has_search_work_in_flight()
                .then_some(Duration::from_millis(8)),
            app.semantic_worker_busy()
                .then_some(Duration::from_millis(250)),
            store
                .as_ref()
                .is_some_and(Store::pending)
                .then_some(GENERATION_POLL),
            app.status_message_remaining(),
            app.list_preview_notice_in(),
        ]
        .into_iter()
        .flatten()
        .min();
        let received = match (queued.take(), wait) {
            (Some(event), _) => Ok(event),
            (None, Some(wait)) => events_rx.recv_timeout(wait),
            (None, None) => events_rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let event = match received {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => unreachable!("events_tx is held by the loop"),
        };
        // Handle what queued up behind this event before drawing, so a held
        // key or a mouse sweep costs a frame per batch, not per event.
        handled_since_draw += 1;
        if handled_since_draw < MAX_EVENTS_PER_FRAME && !needs_fresh_frame(&event) {
            queued = events_rx.try_recv().ok();
        }

        let ev = match event {
            RuntimeEvent::Input(ev) => ev,
            RuntimeEvent::InputError(err) => return Err(AppError::Io(err)),
            RuntimeEvent::PreviewReady => continue,
            RuntimeEvent::Reconciled(status) => {
                let Some(store) = &mut store else {
                    continue;
                };
                let failure = store.reconciled(status, &events_tx);
                store.show_current(&mut app);
                if let Some(failure) = &failure {
                    app.index_failed(failure);
                }
                store.show_progress(&mut app);
                if store.pending() {
                    continue;
                }
                if let Some(failure) = failure {
                    refreshing = None;
                    if std::mem::take(&mut starting) && app.generation_is_empty() {
                        return Err(AppError::Io(io::Error::other(format!(
                            "indexing sessions failed: {failure}"
                        ))));
                    }
                }
                if let Some(old_count) = refreshing.take() {
                    let new_count = app.generation_len().unwrap_or(0);
                    app.set_status_message(format!(
                        "Refreshed sessions: {old_count} -> {new_count}"
                    ));
                }
                if std::mem::take(&mut starting) && app.generation_is_empty() {
                    return Err(no_history());
                }
                continue;
            }
        };

        let event_result = handle_event(
            &mut app,
            &frame_state,
            ev,
            true,
            |app, action| match action {
                // The delete key never opens the confirmation while deleting is
                // disabled; this guards the deletion itself as well.
                Action::Delete(_) if !app.delete_enabled() => EventLoopResult::Continue,
                Action::Delete(ref path) => {
                    let source = app
                        .get_selected_source()
                        .unwrap_or(crate::history::model::SourceKind::Claude);
                    let adapter =
                        crate::history::sources::Registry::from_env().for_path(source, path);
                    if !adapter.capabilities().delete {
                        app.set_status_message(format!(
                            "Deleting {} sessions is not supported",
                            source.list_label()
                        ));
                        return EventLoopResult::Continue;
                    }
                    let result = adapter.delete(path, &resume_config.launcher(source));
                    match result {
                        Ok(()) => {
                            app.remove_selected_from_list();
                            app.exit_view_mode();
                            EventLoopResult::Continue
                        }
                        Err(e) => {
                            let _ = debug_log::log_debug(&format!(
                                "Failed to delete session {}: {}",
                                path.display(),
                                e
                            ));
                            app.set_status_message(format!("Failed to delete: {e}"));
                            EventLoopResult::Continue
                        }
                    }
                }
                Action::Resume(_) | Action::ForkResume(_) => {
                    // Stays in the list when the agent cannot start.
                    let source = app
                        .get_selected_source()
                        .unwrap_or(crate::history::model::SourceKind::Claude);
                    match resume_config.launcher(source).check(source) {
                        Ok(()) => EventLoopResult::Return(Some(action)),
                        Err(e) => {
                            app.set_status_message(e.to_string());
                            EventLoopResult::Continue
                        }
                    }
                }
                _ => EventLoopResult::Return(Some(action)),
            },
        );

        match event_result {
            EventLoopResult::Continue | EventLoopResult::Break => {}
            EventLoopResult::Return(Some(Action::Refresh)) => match &mut store {
                Some(store) => {
                    if refreshing.is_none() {
                        refreshing = Some(app.generation_len().unwrap_or(0));
                        store.reconcile(&events_tx);
                        app.set_reconciling(store.pending());
                        app.set_status_message("Refreshing sessions...".to_string());
                    }
                }
                None => {
                    let old_count = app.conversations().len();
                    match load_in_memory(show_last, time_filter) {
                        Ok(conversations) => {
                            let new_count = conversations.len();
                            app.show_conversations(conversations);
                            app.set_status_message(format!(
                                "Refreshed sessions: {old_count} -> {new_count}"
                            ));
                        }
                        Err(err) => app.set_status_message(format!("Refresh failed: {err}")),
                    }
                }
            },
            EventLoopResult::Return(Some(action)) => {
                let conversation = match &action {
                    Action::Select(path) | Action::Resume(path) | Action::ForkResume(path) => {
                        app.conversation_at(path)
                    }
                    _ => None,
                };
                return Ok((action, conversation));
            }
            EventLoopResult::Return(None) => {}
        }
    }
}

pub fn run_single_file(
    path: PathBuf,
    tool_display: ToolDisplayMode,
    show_thinking: bool,
    keys: KeyBindings,
) -> Result<()> {
    let mut guard = TerminalGuard::new()?;
    let mut app = App::new_single_file(path, tool_display, show_thinking, keys);

    loop {
        let frame_state = prepare_frame(&mut app, &mut guard.terminal);
        draw_frame(&app, &mut guard.terminal)?;

        let Some(ev) = read_event(Duration::from_secs(3600))? else {
            continue;
        };
        let event_result = handle_event(
            &mut app,
            &frame_state,
            ev,
            false,
            |_, action| match action {
                Action::Quit => EventLoopResult::Return(Some(Action::Quit)),
                _ => EventLoopResult::Continue,
            },
        );

        match event_result {
            EventLoopResult::Continue => {}
            EventLoopResult::Break => continue,
            EventLoopResult::Return(Some(Action::Quit)) => return Ok(()),
            EventLoopResult::Return(None) => {}
            EventLoopResult::Return(Some(_)) => {}
        }
    }
}

/// Marks `first_rows` once the drawn list shows the newest conversations:
/// snapshot cards, or the sorted corpus after loading. Returns whether it did.
fn report_first_rows(app: &App) -> bool {
    let ready = app.readiness().list;
    if ready {
        crate::bench::mark("first_rows");
    }
    ready
}

/// Runs the list startup path against an offscreen terminal and prints timing
/// marks: first populated frame, the first page and complete ranking of
/// `query`, the reconcile pass, and the swap to the generation it published.
///
/// The query runs as soon as the generation's index is open, or after the
/// first pass when there was no generation. When
/// `CLAUDE_HISTORY_BENCH_QUERIES` names a file, each of its lines is then
/// run the same way and the latency percentiles are printed.
#[allow(clippy::too_many_arguments)]
pub fn bench_startup(
    show_last: bool,
    time_filter: TimeFilter,
    keys: KeyBindings,
    workspace_filter: bool,
    workspace: Option<crate::history::Workspace>,
    exclude_projects: Vec<String>,
    exclude_paths: Vec<String>,
    query: &str,
) -> Result<()> {
    use crate::bench;
    use ratatui::backend::TestBackend;

    const WIDTH: u16 = 120;
    const HEIGHT: u16 = 40;

    type Screen = Terminal<TestBackend>;
    fn draw(terminal: &mut Screen, app: &App) -> Result<()> {
        terminal
            .draw(|frame| ui::render(frame, app))
            .map(|_| ())
            .map_err(|e| AppError::Io(io::Error::other(e)))
    }
    /// Runs `query` the way typing it does and returns the times until its
    /// first page and its complete ranking are drawn.
    fn run_query(
        terminal: &mut Screen,
        app: &mut App,
        query: &str,
        marks: bool,
    ) -> Result<(Duration, Duration)> {
        let started = Instant::now();
        app.replace_query(query);
        let mut page = None;
        loop {
            let applied = app.receive_search_results();
            if applied || !app.has_search_work_in_flight() {
                draw(terminal, app)?;
                if page.is_none() {
                    page = Some(started.elapsed());
                    if marks {
                        bench::mark("search_ready");
                    }
                }
            }
            if !app.has_search_work_in_flight() {
                if marks {
                    bench::mark("search_complete");
                }
                let page = page.unwrap_or_else(|| started.elapsed());
                return Ok((page, started.elapsed()));
            }
            if !applied {
                std::thread::sleep(Duration::from_micros(200));
            }
        }
    }
    fn run_bench_queries(terminal: &mut Screen, app: &mut App) -> Result<()> {
        let Some(path) = std::env::var_os("CLAUDE_HISTORY_BENCH_QUERIES") else {
            return Ok(());
        };
        let queries = std::fs::read_to_string(path).map_err(AppError::Io)?;
        let (mut pages, mut completes) = (Vec::new(), Vec::new());
        for query in queries.lines().filter(|line| !line.trim().is_empty()) {
            let (page, complete) = run_query(terminal, app, query, false)?;
            pages.push(page.as_secs_f64() * 1e3);
            completes.push(complete.as_secs_f64() * 1e3);
        }
        for (label, times) in [
            ("query_page", &mut pages),
            ("query_complete", &mut completes),
        ] {
            if times.is_empty() {
                continue;
            }
            times.sort_by(f64::total_cmp);
            let at = |p: f64| times[((times.len() - 1) as f64 * p).round() as usize];
            eprintln!(
                "bench {label} n={} p50 {:.1}ms p95 {:.1}ms max {:.1}ms",
                times.len(),
                at(0.5),
                at(0.95),
                at(1.0)
            );
        }
        Ok(())
    }

    /// With `CLAUDE_HISTORY_BENCH_SEMANTIC=<query>`, switches to semantic
    /// mode the way ctrl+t does, runs the query, and reports the time until
    /// its results are drawn, the peak footprint, and the top results.
    fn run_semantic_bench(terminal: &mut Screen, app: &mut App) -> Result<()> {
        let Some(query) = std::env::var("CLAUDE_HISTORY_BENCH_SEMANTIC")
            .ok()
            .filter(|query| !query.trim().is_empty())
        else {
            return Ok(());
        };
        let wait = |app: &mut App| {
            while app.has_search_work_in_flight() {
                if !app.receive_search_results() {
                    std::thread::sleep(Duration::from_micros(200));
                }
            }
        };
        app.replace_query("");
        wait(app);
        let started = Instant::now();
        app.set_list_search_mode(ListSearchMode::Semantic);
        app.replace_query(&query);
        wait(app);
        draw(terminal, app)?;
        eprintln!(
            "bench semantic_first_results {:.1}ms",
            started.elapsed().as_secs_f64() * 1e3
        );
        bench::mark_peak_rss_as("semantic_peak_rss");
        if let Some(error) = app.semantic_error() {
            eprintln!("bench semantic_error {error}");
        }
        eprintln!("bench semantic_results {}", app.filtered().len());
        for &row in app.filtered().iter().take(10) {
            eprintln!(
                "bench semantic_top {}",
                app.conversations()[row].path.display()
            );
        }
        if app.semantic_indexing().is_some() {
            while app.semantic_indexing().is_some() {
                if !app.receive_search_results() {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            wait(app);
            draw(terminal, app)?;
            eprintln!(
                "bench semantic_indexed {:.1}ms",
                started.elapsed().as_secs_f64() * 1e3
            );
            bench::mark_peak_rss_as("semantic_indexed_peak_rss");
            eprintln!("bench semantic_indexed_results {}", app.filtered().len());
            for &row in app.filtered().iter().take(10) {
                eprintln!(
                    "bench semantic_indexed_top {}",
                    app.conversations()[row].path.display()
                );
            }
        }
        app.set_list_search_mode(ListSearchMode::Lexical);
        app.replace_query("");
        wait(app);
        Ok(())
    }

    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT))
        .map_err(|e| AppError::Io(io::Error::other(e)))?;

    let Some(mut store) = Store::new(show_last, time_filter) else {
        return Err(AppError::Io(io::Error::other("no cache directory")));
    };
    let mut app = App::new_loading_with_options(
        ToolDisplayMode::Summary,
        false,
        keys,
        workspace_filter,
        workspace,
        exclude_projects,
        exclude_paths,
        TuiSearchOptions::default(),
    );
    if let Some(cards) = store.open() {
        eprintln!("bench generation {}", cards.generation_name());
        store.shown = Some(cards.generation_name().to_owned());
        app.show_snapshot(cards);
    }
    draw(&mut terminal, &app)?;
    bench::mark("first_frame");
    let first_rows = report_first_rows(&app);

    let mut searched = false;
    if app.open_search_index() {
        bench::mark("index_open");
        bench::mark_peak_rss_as("index_open_peak_rss");
        run_query(&mut terminal, &mut app, query, true)?;
        eprintln!("bench results {}", app.filtered().len());
        run_bench_queries(&mut terminal, &mut app)?;
        bench::mark_peak_rss_as("generation_peak_rss");
        run_semantic_bench(&mut terminal, &mut app)?;
        searched = true;
    }

    let status = spawn_reconciler()
        .and_then(|mut child| child.wait())
        .map_err(AppError::Io)?;
    bench::mark("reconcile_exited");
    eprintln!("bench reconcile_status {status}");
    if store.show_current(&mut app) {
        bench::mark("generation_shown");
        while app.has_search_work_in_flight() {
            if !app.receive_search_results() {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        draw(&mut terminal, &app)?;
        if !first_rows {
            report_first_rows(&app);
        }
        if searched {
            let (page, complete) = run_query(&mut terminal, &mut app, query, false)?;
            eprintln!(
                "bench swapped_query page {:.1}ms complete {:.1}ms results {}",
                page.as_secs_f64() * 1e3,
                complete.as_secs_f64() * 1e3,
                app.filtered().len()
            );
        }
    }
    if !searched {
        run_query(&mut terminal, &mut app, query, true)?;
        eprintln!("bench results {}", app.filtered().len());
        run_bench_queries(&mut terminal, &mut app)?;
    }
    eprintln!("bench conversations {}", app.generation_len().unwrap_or(0));
    bench::mark_peak_rss();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_width_keeps_timing_off_behavior() {
        assert_eq!(calculate_content_width(60, false), 46);
    }

    #[test]
    fn content_width_reserves_timestamp_prefix_when_timing_is_on() {
        assert_eq!(calculate_content_width(60, true), 32);
    }
}
