use crate::history::{Conversation, ParseError};
use chrono::Local;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

/// Get the debug log file path (~/.local/state/claude-history/debug.log)
fn get_debug_log_path() -> Option<PathBuf> {
    let home = home::home_dir()?;
    Some(
        home.join(".local")
            .join("state")
            .join("claude-history")
            .join("debug.log"),
    )
}

/// Log parse errors for a conversation to the debug log file.
///
/// Only writes to the log if there are parse errors. The log is appended to,
/// so errors accumulate over time for debugging.
pub fn log_parse_errors(conversation: &Conversation) -> std::io::Result<()> {
    if conversation.parse_errors.is_empty() {
        return Ok(());
    }

    let log_path = match get_debug_log_path() {
        Some(p) => p,
        None => return Ok(()),
    };

    // Create directory if needed
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");

    writeln!(file, "=== Parse Errors: {} ===", timestamp)?;
    writeln!(file, "File: {}", conversation.path.display())?;
    writeln!(file, "Errors: {}", conversation.parse_errors.len())?;
    writeln!(file)?;

    for error in &conversation.parse_errors {
        write_parse_error(&mut file, error)?;
    }

    writeln!(file, "---")?;
    writeln!(file)?;

    Ok(())
}

/// Write a single parse error with context to the log file
fn write_parse_error(file: &mut fs::File, error: &ParseError) -> std::io::Result<()> {
    writeln!(file, "Line {}: {}", error.line_number, error.error_message)?;
    writeln!(file)?;

    // Context lines before the error
    let ctx_start = error.line_number.saturating_sub(error.context_before.len());
    for (i, ctx) in error.context_before.iter().enumerate() {
        writeln!(file, "  {:>4} | {}", ctx_start + i, truncate_line(ctx, 200))?;
    }

    // The failing line (marked with >)
    writeln!(
        file,
        "> {:>4} | {}",
        error.line_number,
        truncate_line(&error.line_content, 200)
    )?;

    // Context lines after the error
    for (i, ctx) in error.context_after.iter().enumerate() {
        writeln!(
            file,
            "  {:>4} | {}",
            error.line_number + 1 + i,
            truncate_line(ctx, 200)
        )?;
    }

    writeln!(file)?;
    Ok(())
}

/// Truncate a line to a maximum number of characters for readable logs.
/// Uses char-aware truncation to avoid panicking on multi-byte UTF-8.
fn truncate_line(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect::<String>() + "..."
    }
}

/// Log a debug message to the debug log file.
pub fn log_debug(message: &str) -> std::io::Result<()> {
    let log_path = match get_debug_log_path() {
        Some(p) => p,
        None => return Ok(()),
    };

    // Create directory if needed
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    writeln!(file, "[{}] {}", timestamp, message)?;

    Ok(())
}

/// Log the selected conversation path to the debug log file.
#[allow(dead_code)]
pub fn log_selected_path(path: &std::path::Path) -> std::io::Result<()> {
    let log_path = match get_debug_log_path() {
        Some(p) => p,
        None => return Ok(()),
    };

    // Create directory if needed
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");

    writeln!(file, "[{}] Selected: {}", timestamp, path.display())?;

    Ok(())
}

/// Log a display-time parse error to the debug log file.
pub fn log_display_error(
    file_path: &std::path::Path,
    line_number: usize,
    error: &str,
    line_content: &str,
) -> std::io::Result<()> {
    let log_path = match get_debug_log_path() {
        Some(p) => p,
        None => return Ok(()),
    };

    // Create directory if needed
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");

    writeln!(file, "=== Display Parse Error: {} ===", timestamp)?;
    writeln!(file, "File: {}", file_path.display())?;
    writeln!(file, "Line {}: {}", line_number, error)?;
    writeln!(file, "Content: {}", truncate_line(line_content, 200))?;
    writeln!(file, "---")?;
    writeln!(file)?;

    Ok(())
}
