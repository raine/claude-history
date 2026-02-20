mod claude;
mod cli;
mod config;
mod debug;
mod debug_log;
mod display;
mod error;
mod history;
mod markdown;
mod pager;
mod providers;
mod syntax;
mod tool_format;
mod tui;

use clap::Parser;
use cli::Args;
use error::{AppError, Result};
use history::LoaderMessage;
use providers::Provider;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

fn main() {
    if let Err(e) = run() {
        match e {
            AppError::SelectionCancelled => {
                // User cancelled, exit silently
                std::process::exit(0);
            }
            _ => {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
    }
}

/// Helper function to resolve a boolean setting by merging CLI flags and config values.
///
/// Priority: enable_flag > disable_flag > config_value > default_value
fn resolve_bool_setting(
    enable_flag: bool,
    disable_flag: bool,
    config_value: Option<bool>,
    default_value: bool,
) -> bool {
    if enable_flag {
        true
    } else if disable_flag {
        false
    } else {
        config_value.unwrap_or(default_value)
    }
}

fn run() -> Result<()> {
    let args = Args::parse();
    let config = config::load_config()?;

    // Merge CLI arguments with config file settings. CLI takes precedence.
    let display_config = config.display.unwrap_or_default();

    // Extract resume config
    let resume_config = config.resume.unwrap_or_default();
    let default_args = resume_config.default_args.as_deref().unwrap_or(&[]);

    // Use positive names internally for clarity
    let show_tools = resolve_bool_setting(
        args.show_tools,
        args.no_tools,
        display_config.no_tools.map(|b| !b),
        false, // Default: hide tools
    );
    // Map CLI flag to ToolDisplayMode
    // --show-tools → Full, --no-tools → Hidden, default → Truncated
    let tool_display = if args.show_tools {
        tui::ToolDisplayMode::Full
    } else if args.no_tools {
        tui::ToolDisplayMode::Hidden
    } else {
        match display_config.no_tools {
            Some(true) => tui::ToolDisplayMode::Hidden,
            Some(false) => tui::ToolDisplayMode::Full,
            None => tui::ToolDisplayMode::Truncated,
        }
    };
    let show_last = resolve_bool_setting(args.last, args.first, display_config.last, false);
    let use_relative_time = resolve_bool_setting(
        args.relative_time,
        args.absolute_time,
        display_config.relative_time,
        false,
    );
    let show_thinking = resolve_bool_setting(
        args.show_thinking,
        args.hide_thinking,
        display_config.show_thinking,
        false,
    );
    let plain_mode = resolve_bool_setting(args.plain, false, display_config.plain, false);
    let use_pager = resolve_bool_setting(
        args.pager,
        args.no_pager,
        display_config.pager,
        std::io::stdout().is_terminal(),
    );
    let use_global = args.global || config.global.unwrap_or(false);

    // Build provider registry
    let providers: Vec<Box<dyn Provider>> = vec![
        Box::new(providers::claude::ClaudeProvider::new()),
        Box::new(providers::cursor::CursorProvider::new()),
    ];

    // Handle --render flag: render a JSONL file in ledger format and exit
    if let Some(ref render_path) = args.render {
        let display_options = display::DisplayOptions {
            no_tools: !show_tools,
            show_thinking,
            debug_level: args.debug,
            use_pager,
            no_color: args.no_color,
        };
        return display::render_to_terminal(render_path, &display_options);
    }

    // Handle direct file input mode
    if let Some(ref input_file) = args.input_file {
        if !input_file.exists() {
            return Err(AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("File not found: {}", input_file.display()),
            )));
        }
        if !input_file.is_file() {
            return Err(AppError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Not a file: {}", input_file.display()),
            )));
        }
        tui::run_single_file(
            input_file.clone(),
            use_relative_time,
            tool_display,
            show_thinking,
            &providers,
        )?;
        return Ok(());
    }

    // Handle --show-dir flag (Claude-specific, print directory and exit)
    if args.show_dir {
        if let Ok(current_dir) = std::env::current_dir() {
            if let Ok(projects_dir) = history::get_claude_projects_dir(&current_dir) {
                println!("{}", projects_dir.display());
            }
        }
        return Ok(());
    }

    // Determine how to load conversations based on mode
    let (conversations, selected_path) = if use_global {
        // Global mode - merge streaming loaders from all providers
        let receivers: Vec<_> = providers
            .iter()
            .map(|p| p.load_conversations_streaming(show_last, args.debug))
            .collect();
        let rx = merge_streaming_loaders(receivers);

        match tui::run_with_loader(rx, use_relative_time, tool_display, show_thinking, &providers)?
        {
            (tui::Action::Select(path), convs) => (convs, path),
            (tui::Action::Resume(path), convs) => {
                resume_conversation(&convs, &path, &providers, default_args)?;
                return Ok(());
            }
            (tui::Action::Quit, _) => return Err(AppError::SelectionCancelled),
            (tui::Action::Delete(_), _) => unreachable!("Delete is handled internally"),
        }
    } else {
        // Local mode - load from all providers for current directory
        let current_dir = std::env::current_dir().map_err(|e| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Failed to get current directory: {}", e),
            ))
        })?;

        let mut conversations = Vec::new();

        for provider in &providers {
            match provider.load_conversations(show_last, args.debug) {
                Ok(mut convs) => {
                    // For non-Claude providers, filter to current directory
                    if provider.kind() != history::ProviderKind::Claude {
                        convs.retain(|c| {
                            c.project_path
                                .as_ref()
                                .is_some_and(|p| p == &current_dir)
                        });
                    }
                    conversations.extend(convs);
                }
                Err(_) => {} // Silently skip providers that fail in local mode
            }
        }

        // Sort merged conversations by timestamp (newest first) and re-index
        conversations.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        for (idx, conv) in conversations.iter_mut().enumerate() {
            conv.index = idx;
        }

        if conversations.is_empty() {
            return Err(AppError::NoHistoryFound("selected scope".to_string()));
        }

        match tui::run(
            conversations.clone(),
            use_relative_time,
            tool_display,
            show_thinking,
            &providers,
        )? {
            tui::Action::Select(path) => (conversations, path),
            tui::Action::Resume(path) => {
                resume_conversation(&conversations, &path, &providers, default_args)?;
                return Ok(());
            }
            tui::Action::Quit => return Err(AppError::SelectionCancelled),
            tui::Action::Delete(_) => unreachable!("Delete is handled internally"),
        }
    };

    if args.show_path {
        println!("{}", selected_path.display());
        return Ok(());
    }

    if args.show_id {
        let conv = conversations.iter().find(|c| c.path == selected_path);
        let id = conv
            .map(|c| c.id.as_str())
            .or_else(|| {
                selected_path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
            })
            .ok_or_else(|| {
                AppError::ClaudeExecutionError(
                    "Conversation filename is not valid Unicode".to_string(),
                )
            })?;
        println!("{}", id);
        return Ok(());
    }

    if args.resume {
        resume_conversation(&conversations, &selected_path, &providers, default_args)?;
        return Ok(());
    }

    // Log parse errors to debug log if debug mode is enabled
    if args.debug.is_some()
        && let Some(conv) = conversations.iter().find(|c| c.path == selected_path)
    {
        if let Err(e) = debug_log::log_parse_errors(conv) {
            debug::warn(
                args.debug,
                &format!("Failed to write parse errors to log: {}", e),
            );
        } else if !conv.parse_errors.is_empty() {
            debug::info(
                args.debug,
                &format!(
                    "Logged {} parse error(s) to ~/.local/state/claude-history/debug.log",
                    conv.parse_errors.len()
                ),
            );
        }
    }

    // Display the selected conversation
    let display_options = display::DisplayOptions {
        no_tools: !show_tools,
        show_thinking,
        debug_level: args.debug,
        use_pager,
        no_color: false, // Regular display uses the colored crate which handles this automatically
    };

    if plain_mode {
        display::display_conversation_plain(&selected_path, &display_options)?;
    } else {
        display::display_conversation(&selected_path, &display_options)?;
    }

    Ok(())
}

/// Merge multiple streaming loader receivers into a single receiver.
/// Each provider streams independently; batches are forwarded immediately.
/// Done is only sent when ALL providers have finished.
/// Fatal errors from individual providers are downgraded to ProjectError
/// so the app continues with other providers.
fn merge_streaming_loaders(receivers: Vec<Receiver<LoaderMessage>>) -> Receiver<LoaderMessage> {
    let (tx, rx) = mpsc::channel();
    let remaining = Arc::new(AtomicUsize::new(receivers.len()));

    for receiver in receivers {
        let tx = tx.clone();
        let remaining = remaining.clone();
        std::thread::spawn(move || {
            for msg in receiver {
                match msg {
                    LoaderMessage::Done => {
                        // Only send Done when all providers have finished
                        if remaining.fetch_sub(1, Ordering::SeqCst) == 1 {
                            let _ = tx.send(LoaderMessage::Done);
                        }
                    }
                    LoaderMessage::Fatal(_) => {
                        // Downgrade: one provider failing shouldn't kill the app
                        let _ = tx.send(LoaderMessage::ProjectError);
                        if remaining.fetch_sub(1, Ordering::SeqCst) == 1 {
                            let _ = tx.send(LoaderMessage::Done);
                        }
                    }
                    other => {
                        let _ = tx.send(other);
                    }
                }
            }
        });
    }

    rx
}

/// Resume a conversation through the appropriate provider
fn resume_conversation(
    conversations: &[history::Conversation],
    path: &std::path::Path,
    providers: &[Box<dyn Provider>],
    default_args: &[String],
) -> Result<()> {
    let conv = conversations
        .iter()
        .find(|c| c.path == path)
        .ok_or_else(|| {
            AppError::ClaudeExecutionError("Conversation not found for resume".to_string())
        })?;

    let provider = providers
        .iter()
        .find(|p| p.kind() == conv.provider)
        .ok_or_else(|| {
            AppError::ClaudeExecutionError(format!(
                "No provider found for {:?} conversation",
                conv.provider
            ))
        })?;

    provider.resume(conv, default_args)
}
