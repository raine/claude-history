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
mod syntax;
mod tool_format;
mod tui;

use clap::Parser;
use cli::Args;
use error::{AppError, Result};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

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
        )?;
        return Ok(());
    }

    let use_global = resolve_bool_setting(args.global, false, config.global, false);

    // Determine how to load conversations based on mode
    let (conversations, selected_path) = if use_global {
        // Global Search (-g) - use streaming loader for instant startup
        let rx = history::load_all_conversations_streaming(show_last, args.debug);

        match tui::run_with_loader(rx, use_relative_time, tool_display, show_thinking)? {
            (tui::Action::Select(path), convs) => (convs, path),
            (tui::Action::Resume(path), convs) => {
                let conv = convs.iter().find(|c| c.path == path);
                let project_path = conv.and_then(|c| c.project_path.as_ref());
                resume_with_claude(&path, project_path, default_args)?;
                return Ok(());
            }
            (tui::Action::Quit, _) => return Err(AppError::SelectionCancelled),
            (tui::Action::Delete(_), _) => unreachable!("Delete is handled internally"),
        }
    } else {
        // Current Directory mode - synchronous loading is fast enough
        let current_dir = std::env::current_dir().map_err(|e| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Failed to get current directory: {}", e),
            ))
        })?;

        let projects_dir = history::get_claude_projects_dir(&current_dir)?;

        // If --show-dir flag is set, print directory and exit
        if args.show_dir {
            println!("{}", projects_dir.display());
            return Ok(());
        }

        if !projects_dir.exists() {
            return Err(AppError::ProjectsDirNotFound(
                projects_dir.display().to_string(),
            ));
        }

        let conversations = history::load_conversations(&projects_dir, show_last, args.debug)?;

        if conversations.is_empty() {
            return Err(AppError::NoHistoryFound("selected scope".to_string()));
        }

        match tui::run(
            conversations.clone(),
            use_relative_time,
            tool_display,
            show_thinking,
        )? {
            tui::Action::Select(path) => (conversations, path),
            tui::Action::Resume(path) => {
                let conv = conversations.iter().find(|c| c.path == path);
                let project_path = conv.and_then(|c| c.project_path.as_ref());
                resume_with_claude(&path, project_path, default_args)?;
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
        let conversation_id = selected_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| {
                AppError::ClaudeExecutionError(
                    "Conversation filename is not valid Unicode".to_string(),
                )
            })?;
        println!("{}", conversation_id);
        return Ok(());
    }

    if args.resume {
        // Find the selected conversation to get its project_path
        let conv = conversations.iter().find(|c| c.path == selected_path);
        debug::debug(
            args.debug,
            &format!("Selected path: {}", selected_path.display()),
        );
        debug::debug(
            args.debug,
            &format!("Found conversation: {}", conv.is_some()),
        );
        if let Some(c) = conv {
            debug::debug(args.debug, &format!("project_path: {:?}", c.project_path));
            if let Some(p) = &c.project_path {
                debug::debug(args.debug, &format!("project_path exists: {}", p.exists()));
            }
        }
        let project_path = conv.and_then(|c| c.project_path.as_ref());
        resume_with_claude(&selected_path, project_path, default_args)?;
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

fn resume_with_claude(
    selected_path: &Path,
    project_path: Option<&PathBuf>,
    default_args: &[String],
) -> Result<()> {
    let conversation_id = selected_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| {
            AppError::ClaudeExecutionError("Conversation filename is not valid Unicode".to_string())
        })?
        .to_owned();

    // Require a valid project directory to resume
    let project_dir = match project_path {
        Some(path) if path.exists() && path.is_dir() => path,
        Some(path) => {
            return Err(AppError::ClaudeExecutionError(format!(
                "Project directory no longer exists: {}",
                path.display()
            )));
        }
        None => {
            return Err(AppError::ClaudeExecutionError(
                "Cannot determine project directory for this conversation".to_string(),
            ));
        }
    };

    let mut command = Command::new("claude");
    command.args(["--resume", &conversation_id]);
    command.args(default_args);
    command.current_dir(project_dir);

    run_claude_command(command)
}

#[cfg(unix)]
fn run_claude_command(mut command: Command) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let err = command.exec();
    Err(AppError::ClaudeExecutionError(err.to_string()))
}

#[cfg(not(unix))]
fn run_claude_command(mut command: Command) -> Result<()> {
    let status = command
        .status()
        .map_err(|e| AppError::ClaudeExecutionError(e.to_string()))?;

    if !status.success() {
        return Err(AppError::ClaudeExecutionError(format!(
            "claude CLI exited with status {}",
            status
        )));
    }

    Ok(())
}
