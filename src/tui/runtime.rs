use super::app::{Action, App, AppMode, DialogMode, TuiSearchOptions};
use super::ui;
use crate::ccs::CcsInfo;
use crate::config::KeyBindings;
use crate::debug_log;
use crate::error::{AppError, Result};
use crate::history::{Conversation, LoaderMessage};
use crate::tui::viewer::ToolDisplayMode;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
    MouseEventKind,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::prelude::*;
use std::io::{self, Stderr};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stderr>>,
}

impl TerminalGuard {
    fn new() -> Result<Self> {
        terminal::enable_raw_mode().map_err(|e| AppError::Io(io::Error::other(e)))?;

        let mut stderr = io::stderr();
        if let Err(e) = crossterm::execute!(stderr, EnterAlternateScreen, EnableMouseCapture) {
            let _ = terminal::disable_raw_mode();
            return Err(AppError::Io(io::Error::other(e)));
        }

        let backend = CrosstermBackend::new(stderr);
        let terminal = match Terminal::new(backend) {
            Ok(t) => t,
            Err(e) => {
                let _ = terminal::disable_raw_mode();
                let _ =
                    crossterm::execute!(io::stderr(), DisableMouseCapture, LeaveAlternateScreen);
                return Err(AppError::Io(io::Error::other(e)));
            }
        };

        Ok(Self { terminal })
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
const MAX_EVENT_BATCH: usize = 256;

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

fn prepare_frame(app: &mut App, terminal: &mut Terminal<CrosstermBackend<Stderr>>) -> FrameState {
    let frame_area = terminal.get_frame().area();
    let viewport_height = frame_area.height.saturating_sub(3) as usize;
    let content_width = (frame_area.width as usize)
        .saturating_sub(NAME_WIDTH + 3 + crate::tui::viewer::GUTTER_WIDTH);

    app.check_view_resize(content_width, viewport_height);
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

fn draw_frame(app: &App, terminal: &mut Terminal<CrosstermBackend<Stderr>>) -> Result<()> {
    terminal.draw(|frame| ui::render(frame, app))?;
    Ok(())
}

fn handle_events<F>(
    app: &mut App,
    frame_state: &FrameState,
    poll_timeout: Duration,
    allow_list_click_enter: bool,
    mut on_action: F,
) -> Result<EventLoopResult<Option<Action>>>
where
    F: FnMut(&mut App, Action) -> EventLoopResult<Option<Action>>,
{
    let events = drain_events(poll_timeout)?;
    for ev in events {
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
                            return Ok(EventLoopResult::Break);
                        }
                        if allow_list_click_enter
                            && app.handle_list_click(m.row, frame_state.frame_area)
                        {
                            app.enter_view_mode(frame_state.content_width);
                            return Ok(EventLoopResult::Break);
                        }
                    }
                    MouseEventKind::Moved => {
                        app.handle_view_mouse_move(m.row, frame_state.frame_area);
                    }
                    _ => {}
                }
                continue;
            }
            _ => continue,
        };

        if allow_list_click_enter
            && matches!(app.app_mode(), AppMode::List)
            && *app.dialog_mode() == DialogMode::None
            && key.code == KeyCode::Enter
            && !app.is_loading()
            && app.selected().is_some()
        {
            app.enter_view_mode(frame_state.content_width);
            return Ok(EventLoopResult::Break);
        }

        if let Some(action) = app.handle_key(key.code, key.modifiers, frame_state.viewport_height) {
            match on_action(app, action) {
                EventLoopResult::Continue => {}
                EventLoopResult::Break => return Ok(EventLoopResult::Break),
                EventLoopResult::Return(action) => return Ok(EventLoopResult::Return(action)),
            }
        }
    }
    Ok(EventLoopResult::Continue)
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub fn run_with_loader(
    rx: Receiver<LoaderMessage>,
    tool_display: ToolDisplayMode,
    show_thinking: bool,
    keys: KeyBindings,
    workspace_filter: bool,
    current_project_dir_name: Option<String>,
    exclude_projects: Vec<String>,
    search_options: TuiSearchOptions,
    ccs_info: Option<CcsInfo>,
    override_profile: Option<String>,
) -> Result<(Action, Vec<Conversation>)> {
    let mut guard = TerminalGuard::new()?;
    let mut app = App::new_loading_with_options(
        tool_display,
        show_thinking,
        keys,
        workspace_filter,
        current_project_dir_name,
        exclude_projects,
        search_options,
        ccs_info,
        override_profile,
    );

    loop {
        loop {
            match rx.try_recv() {
                Ok(LoaderMessage::Fatal(err)) => {
                    drop(guard);
                    return Err(err);
                }
                Ok(LoaderMessage::ProjectError) => {}
                Ok(LoaderMessage::Batch(convs)) => {
                    app.append_conversations(convs);
                }
                Ok(LoaderMessage::Done) => {
                    app.finish_loading();
                    if app.conversations().is_empty() {
                        drop(guard);
                        return Err(AppError::NoHistoryFound("selected scope".to_string()));
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
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

        let frame_state = prepare_frame(&mut app, &mut guard.terminal);
        app.receive_search_results();
        draw_frame(&app, &mut guard.terminal)?;

        let poll_timeout = if app.is_loading() {
            Duration::from_millis(50)
        } else if app.has_search_work_in_flight() {
            Duration::from_millis(8)
        } else if let Some(remaining) = app.status_message_remaining() {
            remaining
        } else {
            Duration::from_secs(3600)
        };

        let event_result = handle_events(
            &mut app,
            &frame_state,
            poll_timeout,
            true,
            |app, action| match action {
                Action::Delete(ref path) => {
                    let uuid = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                    match crate::history::delete_session_by_uuid(uuid) {
                        Ok(_) => {
                            app.remove_selected_from_list();
                            app.exit_view_mode();
                            EventLoopResult::Continue
                        }
                        Err(e) => {
                            let _ = debug_log::log_debug(&format!(
                                "Failed to delete session {}: {}",
                                uuid, e
                            ));
                            EventLoopResult::Continue
                        }
                    }
                }
                _ => EventLoopResult::Return(Some(action)),
            },
        )?;

        match event_result {
            EventLoopResult::Continue => {}
            EventLoopResult::Break => continue,
            EventLoopResult::Return(Some(action)) => return Ok((action, app.into_conversations())),
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

        let event_result = handle_events(
            &mut app,
            &frame_state,
            Duration::from_secs(3600),
            false,
            |_, action| match action {
                Action::Quit => EventLoopResult::Return(Some(Action::Quit)),
                _ => EventLoopResult::Continue,
            },
        )?;

        match event_result {
            EventLoopResult::Continue => {}
            EventLoopResult::Break => continue,
            EventLoopResult::Return(Some(Action::Quit)) => return Ok(()),
            EventLoopResult::Return(None) => {}
            EventLoopResult::Return(Some(_)) => {}
        }
    }
}
