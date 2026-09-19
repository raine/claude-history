//! Non-interactive transcript output: the ledger the TUI viewer draws, written
//! to stdout (or a pager) with ANSI colour, and a plain `Speaker: text` form
//! for piping. Neither walks `LogEntry`s: the ledger is the viewer's
//! `RenderedLine`s, the plain form is `turns::Turn`s.

use crate::error::Result;
use crate::pager;
use crate::tool_format;
use crate::tui::viewer::{NAME_WIDTH, SEPARATOR_WIDTH};
use crate::tui::{RenderOptions, ToolDisplayMode, render_conversation};
use crate::turns::{self, Part, Speaker, Turn, Visibility};
use colored::{Colorize, CustomColor};
use crossterm::terminal;
use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::Path;

/// Configuration options for displaying conversations
#[derive(Debug, Clone, Default)]
pub struct DisplayOptions {
    /// Hide tool calls and results
    pub no_tools: bool,
    /// Show thinking/reasoning blocks
    pub show_thinking: bool,
    /// Use a pager for output (less/more)
    pub use_pager: bool,
    /// Disable colored output
    pub no_color: bool,
}

impl DisplayOptions {
    fn visibility(&self) -> Visibility {
        Visibility {
            tools: !self.no_tools,
            thinking: self.show_thinking,
        }
    }
}

/// Default content width for plain text output
const PLAIN_CONTENT_WIDTH: usize = 80;

/// Get the terminal width, defaulting to 80 if unavailable
fn get_terminal_width() -> usize {
    terminal::size().map(|(w, _)| w as usize).unwrap_or(80)
}

/// Runs `emit` against stdout, or a pager's stdin when requested, and waits
/// for the pager to exit. Write errors (pager quit) end the output quietly.
fn with_output(use_pager: bool, emit: impl FnOnce(&mut dyn Write) -> io::Result<()>) {
    let mut pager_child = if use_pager {
        pager::spawn_pager().ok()
    } else {
        None
    };

    let mut stdout_handle = io::stdout().lock();
    let writer: &mut dyn Write = if let Some(ref mut child) = pager_child {
        child.stdin.as_mut().unwrap()
    } else {
        &mut stdout_handle
    };
    let _ = emit(writer);

    drop(stdout_handle);
    if let Some(mut child) = pager_child {
        let _ = child.wait();
    }
}

/// Display a conversation in the viewer's ledger format.
pub fn display_conversation(file_path: &Path, options: &DisplayOptions) -> Result<()> {
    let terminal_width = get_terminal_width();
    let render_options = RenderOptions {
        tool_display: if options.no_tools {
            ToolDisplayMode::Hidden
        } else {
            ToolDisplayMode::Full
        },
        show_thinking: options.show_thinking,
        show_timing: false, // Non-TUI render doesn't support timing toggle
        content_width: terminal_width.saturating_sub(NAME_WIDTH + SEPARATOR_WIDTH),
        expanded_tool_outputs: BTreeSet::new(),
    };
    let rendered = render_conversation(file_path, &render_options)?;

    with_output(options.use_pager, |writer| {
        for line in &rendered.lines {
            for (text, style) in &line.spans {
                if options.no_color {
                    write!(writer, "{text}")?;
                    continue;
                }
                let mut styled = text.as_str().normal();
                if let Some((r, g, b)) = style.fg {
                    styled = styled.custom_color(CustomColor { r, g, b });
                }
                if style.bold {
                    styled = styled.bold();
                }
                if style.dimmed {
                    styled = styled.dimmed();
                }
                if style.italic {
                    styled = styled.italic();
                }
                write!(writer, "{styled}")?;
            }
            writeln!(writer)?;
        }
        Ok(())
    });
    Ok(())
}

/// Display a conversation in plain text format (no ledger formatting)
pub fn display_conversation_plain(file_path: &Path, options: &DisplayOptions) -> Result<()> {
    let turns = turns::project_file(file_path, options.visibility())?;
    with_output(options.use_pager, |writer| {
        for turn in &turns {
            write_plain_turn(writer, turn)?;
        }
        Ok(())
    });
    Ok(())
}

/// `Speaker: text` lines; subagent turns are indented and tagged with the
/// agent id.
fn write_plain_turn(writer: &mut dyn Write, turn: &Turn) -> io::Result<()> {
    let (indent, speaker) = match (&turn.subagent, &turn.speaker) {
        (Some(id), Speaker::User | Speaker::Metadata { .. }) => ("  ", format!("[{id}] User")),
        (Some(id), Speaker::Assistant { .. }) => ("  ", format!("[{id}] Agent")),
        (None, Speaker::User | Speaker::Metadata { .. }) => ("", "You".to_owned()),
        (None, Speaker::Assistant { name }) => {
            ("", name.clone().unwrap_or_else(|| "Claude".to_owned()))
        }
    };
    let body_indent = format!("{indent}  ");

    for part in &turn.parts {
        match part {
            // Pi/OMP metadata reads as a labelled user line, as in exports.
            Part::Text(text) => match &turn.speaker {
                Speaker::Metadata { label } => {
                    writeln!(writer, "{indent}{speaker}: [{label}] {text}")?
                }
                _ => writeln!(writer, "{indent}{speaker}: {text}")?,
            },
            Part::Thinking(text) => writeln!(writer, "{indent}Thinking: {text}")?,
            Part::ToolCall { name, input } => {
                let formatted = tool_format::format_tool_call(name, input, PLAIN_CONTENT_WIDTH);
                writeln!(writer, "{indent}{speaker}: {}", formatted.header)?;
                if let Some(body) = formatted.body {
                    for line in body.lines() {
                        writeln!(writer, "{body_indent}{line}")?;
                    }
                }
            }
            Part::ToolResult(content) => {
                writeln!(writer, "{body_indent}Tool: <Result>")?;
                for line in turns::tool_result_text(content.as_ref()).lines() {
                    writeln!(writer, "{body_indent}{line}")?;
                }
            }
        }
    }
    writeln!(writer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plain(turn: Turn) -> String {
        let mut out = Vec::new();
        write_plain_turn(&mut out, &turn).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn turn(speaker: Speaker, subagent: Option<&str>, parts: Vec<Part>) -> Turn {
        Turn {
            entry_index: 0,
            speaker,
            subagent: subagent.map(str::to_owned),
            timestamp: None,
            parts,
        }
    }

    #[test]
    fn plain_turns_use_speaker_prefixes() {
        assert_eq!(
            plain(turn(Speaker::User, None, vec![Part::Text("hi".into())])),
            "You: hi\n\n"
        );
        assert_eq!(
            plain(turn(
                Speaker::Assistant {
                    name: Some("Pi".into())
                },
                None,
                vec![Part::Text("yo".into()), Part::Thinking("why".into())]
            )),
            "Pi: yo\nThinking: why\n\n"
        );
    }

    #[test]
    fn plain_metadata_matches_the_export_form() {
        assert_eq!(
            plain(turn(
                Speaker::Metadata {
                    label: "Compaction".into()
                },
                None,
                vec![Part::Text("summary".into())]
            )),
            "You: [Compaction] summary\n\n"
        );
    }

    #[test]
    fn plain_subagent_turns_are_indented_and_tagged() {
        let out = plain(turn(
            Speaker::Assistant { name: None },
            Some("agent12"),
            vec![
                Part::ToolCall {
                    name: "Bash".into(),
                    input: json!({"command": "ls"}),
                },
                Part::ToolResult(Some(json!("file.txt"))),
            ],
        ));
        assert!(out.starts_with("  [agent12] Agent: "), "{out:?}");
        assert!(
            out.contains("    Tool: <Result>\n    file.txt\n"),
            "{out:?}"
        );
    }
}
