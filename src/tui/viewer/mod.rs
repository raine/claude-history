//! Conversation viewer rendering for TUI display.
//!
//! This module renders conversation JSONL files to `Vec<RenderedLine>` for display
//! in the TUI viewer. It produces styled spans that ratatui can render directly,
//! without using ANSI escape codes.

use crate::claude::LogEntry;
use std::collections::BTreeSet;
use std::path::Path;

use crate::tui::theme::{self, Theme};

mod commands;
mod entry;

pub(crate) use commands::process_command_message;
mod ledger;
mod markdown;
mod output;
mod style;
mod summary;
mod timing;
mod tools;

pub use output::{LineStyle, RenderedLine};

use entry::render_entry;
use summary::{
    PendingToolSummary, flush_tool_summary, tool_only_assistant_summary,
    user_entry_is_only_tool_results,
};
use tools::make_tool_summary_output_id;

/// Width of the focus gutter indicator (▌ + space)
pub const GUTTER_WIDTH: usize = 2;

const NAME_WIDTH: usize = 9;
/// Width of timestamp prefix when timing is enabled (space + HH:MM + space)
const TIMESTAMP_WIDTH: usize = 7;

/// Get the current theme (cached after first detection)
fn th() -> &'static Theme {
    theme::detect_theme()
}

/// Maximum body lines shown in truncated tool call mode
const TRUNCATED_BODY_LINES: usize = 3;
/// Maximum result lines shown in truncated tool result mode
const TRUNCATED_RESULT_LINES: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ToolOutputId(pub String);

/// Controls how tool calls and results are displayed
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToolDisplayMode {
    #[default]
    Hidden,
    Truncated,
    Full,
}

impl ToolDisplayMode {
    /// Cycle to the next mode: Summary → Truncated → Full → Summary
    pub fn next(self) -> Self {
        match self {
            Self::Hidden => Self::Truncated,
            Self::Truncated => Self::Full,
            Self::Full => Self::Hidden,
        }
    }

    pub fn is_summary(self) -> bool {
        matches!(self, Self::Hidden)
    }

    /// Whether full or truncated tool details should be rendered
    pub fn shows_details(self) -> bool {
        !matches!(self, Self::Hidden)
    }

    /// Whether tools should be included in exported text
    pub fn is_visible(self) -> bool {
        self.shows_details()
    }

    /// Fixed-width label for the status bar (3 chars each)
    pub fn status_label(self) -> &'static str {
        match self {
            Self::Hidden => "sum",
            Self::Truncated => "trn",
            Self::Full => "all",
        }
    }
}

/// Options for rendering a conversation
pub struct RenderOptions {
    pub tool_display: ToolDisplayMode,
    pub show_thinking: bool,
    pub show_timing: bool,
    pub content_width: usize,
    pub expanded_tool_outputs: BTreeSet<ToolOutputId>,
    pub show_annotations: bool,
    pub annotations: crate::annotations::ConversationAnnotations,
    /// Label per annotator key, printed in the name column. A key absent here
    /// prints with its first letter capitalised.
    pub annotator_labels: std::collections::HashMap<String, String>,
    /// Id of the annotation currently selected, styled apart from the rest.
    pub focused_annotation: Option<String>,
}

/// Tracks the line range of a single message (User or Assistant entry) in the rendered output
#[derive(Clone, Debug)]
pub struct MessageRange {
    /// Index of the JSONL entry (line number in the file, 0-based, counting only parsed entries)
    pub entry_index: usize,
    /// Start line in rendered output (inclusive)
    pub start_line: usize,
    /// End line in rendered output (exclusive, excludes trailing blank)
    pub end_line: usize,
}

/// The lines one annotation occupies, and the annotation it came from.
///
/// Held separately from `MessageRange` because an annotation carries no entry,
/// and `MessageRange::entry_index` is binary-searched when restoring scroll
/// position: a synthetic or duplicated index there corrupts that search.
#[derive(Clone, Debug)]
pub struct AnnotationRange {
    pub id: String,
    pub start_line: usize,
    pub end_line: usize,
}

/// Result of rendering a conversation
pub struct RenderedConversation {
    pub lines: Vec<RenderedLine>,
    pub messages: Vec<MessageRange>,
    pub annotations: Vec<AnnotationRange>,
}

/// Format an ISO 8601 timestamp to HH:MM local time
fn format_timestamp(iso_timestamp: &str) -> Option<String> {
    use chrono::{DateTime, Local};
    // Parse RFC 3339 timestamp (handles timezone offsets) and convert to local time
    DateTime::parse_from_rfc3339(iso_timestamp)
        .ok()
        .map(|dt| dt.with_timezone(&Local).format("%H:%M").to_string())
}

#[derive(Debug)]
pub struct RenderableEntry {
    pub entry_index: usize,
    /// Physical JSONL line this entry came from, 1-based, blank lines counted
    /// before being skipped. Kept because annotations name lines, and
    /// `entry_index` counts parsed entries after filtering rather than lines.
    pub jsonl_line: usize,
    entry: LogEntry,
}

pub fn parse_conversation_file(file_path: &Path) -> std::io::Result<Vec<RenderableEntry>> {
    let normalized = crate::history::normalized_log_entries(file_path)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(normalized
        .into_iter()
        .enumerate()
        .filter_map(|(entry_index, (jsonl_line, entry))| {
            (!matches!(entry, LogEntry::FileHistorySnapshot { .. })).then_some(RenderableEntry {
                entry_index,
                jsonl_line,
                entry,
            })
        })
        .collect())
}

pub fn render_parsed_conversation(
    entries: &[RenderableEntry],
    options: &RenderOptions,
) -> RenderedConversation {
    let mut lines = Vec::new();
    let mut messages = Vec::new();
    let mut annotation_ranges = Vec::new();
    let mut pending_tool_summary: Option<PendingToolSummary> = None;

    // Session-level annotations name no line, so they carry no position in the
    // file and print ahead of the conversation.
    if options.show_annotations {
        for annotation in &options.annotations.session {
            push_annotation_lines(
                &mut lines,
                &mut annotation_ranges,
                annotation,
                options.content_width,
                options.show_timing,
                options.focused_annotation.as_deref() == Some(annotation.id.as_str()),
                &options.annotator_labels,
            );
        }
    }

    let mut next_annotation = 0;
    for (parsed_idx, parsed) in entries.iter().enumerate() {
        if options.tool_display.is_summary()
            && try_extend_or_start_pending_summary(
                &mut lines,
                &mut messages,
                &mut pending_tool_summary,
                entries,
                parsed_idx,
                options,
            )
        {
            continue;
        }

        flush_tool_summary(
            &mut lines,
            &mut messages,
            &mut pending_tool_summary,
            entries,
            options,
        );

        render_entry_with_range(&mut lines, &mut messages, parsed, options);

        // Annotations print after the entry holding the line they name, so a
        // note follows what it describes. An annotation naming a line that
        // produced no entry -- a snapshot record, a line absorbed into a tool
        // summary -- prints after the next entry rendered rather than being
        // dropped.
        if options.show_annotations {
            while let Some(annotation) = options.annotations.positioned.get(next_annotation) {
                let Some(anchor) = annotation.anchor_line() else {
                    next_annotation += 1;
                    continue;
                };
                if anchor > parsed.jsonl_line {
                    break;
                }
                push_annotation_lines(
                    &mut lines,
                    &mut annotation_ranges,
                    annotation,
                    options.content_width,
                    options.show_timing,
                    options.focused_annotation.as_deref() == Some(annotation.id.as_str()),
                    &options.annotator_labels,
                );
                next_annotation += 1;
            }
        }
    }

    flush_tool_summary(
        &mut lines,
        &mut messages,
        &mut pending_tool_summary,
        entries,
        options,
    );

    // Annotations naming a line past the last entry still belong to the
    // conversation, so they print at its end rather than vanishing.
    if options.show_annotations {
        for annotation in options.annotations.positioned.iter().skip(next_annotation) {
            push_annotation_lines(
                &mut lines,
                &mut annotation_ranges,
                annotation,
                options.content_width,
                options.show_timing,
                options.focused_annotation.as_deref() == Some(annotation.id.as_str()),
                &options.annotator_labels,
            );
        }
    }

    postprocess_blank_lines(&mut lines, &mut messages);

    RenderedConversation {
        lines,
        messages,
        annotations: annotation_ranges,
    }
}

/// Render one annotation as its own lines, labelled with its kind and the lines
/// it names.
///
/// The label carries the target because a filtered view -- tools, thinking or
/// subagents hidden -- prints an annotation between its visible neighbours
/// rather than among the entries it sat with. Stating the line keeps that gap
/// visible instead of silently approximate.
/// The name column for one annotation: the annotator's configured label, else
/// its key with the first letter capitalised, truncated to the column width so
/// every row's text starts in the same place.
fn annotator_label(key: &str, labels: &std::collections::HashMap<String, String>) -> String {
    let full = match labels.get(key) {
        Some(label) => label.clone(),
        None => {
            let mut chars = key.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => "Note".to_string(),
            }
        }
    };
    full.chars().take(NAME_WIDTH).collect()
}

fn push_annotation_lines(
    lines: &mut Vec<RenderedLine>,
    ranges: &mut Vec<AnnotationRange>,
    annotation: &crate::annotations::Annotation,
    content_width: usize,
    timing_enabled: bool,
    focused: bool,
    labels: &std::collections::HashMap<String, String>,
) {
    let label = annotator_label(&annotation.annotator, labels);
    let start_line = lines.len();
    use ledger::{LedgerRow, NameCol, push_row};
    use timing::TimingSlot;

    let color = th().annotation;
    let timing = || {
        if timing_enabled {
            TimingSlot::Pad
        } else {
            TimingSlot::Disabled
        }
    };
    // A focused note drops the italic and takes bold, so the selection is
    // marked by weight rather than by a colour outside the theme's palette.
    let text_style = LineStyle {
        fg: Some(color),
        italic: !focused,
        bold: focused,
        ..LineStyle::default()
    };
    let trailer_style = LineStyle {
        fg: Some(color),
        dimmed: true,
        ..LineStyle::default()
    };

    // The trailer names the lines targeted and the producer's kind. The target
    // is stated because a filtered view -- tools, thinking or subagents hidden
    // -- prints an annotation between its visible neighbours rather than among
    // the entries it sat with, and stating it keeps that gap visible instead of
    // silently approximate.
    let target = match annotation.targets.as_slice() {
        [] => "session".to_string(),
        targets => targets
            .iter()
            .map(|span| {
                if span.start == span.end {
                    span.start.to_string()
                } else {
                    format!("{}..{}", span.start, span.end)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
    };
    let mut trailer = if annotation.kind.trim().is_empty() {
        format!("  @{target}")
    } else {
        format!("  @{target} · {}", annotation.kind)
    };
    // An origin names the file the note summarises when that file is not this
    // conversation, so a recap of an agent's work leads back to the agent.
    if let Some(origin) = &annotation.origin {
        trailer.push_str(&format!(" · {}", origin.short()));
    }

    let width = content_width.max(20);
    let mut wrapped = Vec::new();
    for text_line in annotation.text.lines() {
        for piece in textwrap::wrap(text_line, width) {
            wrapped.push(piece.to_string());
        }
    }
    if wrapped.is_empty() {
        wrapped.push(String::new());
    }
    let last = wrapped.len() - 1;

    for (index, piece) in wrapped.into_iter().enumerate() {
        let name = if index == 0 {
            NameCol::Label {
                text: &label,
                color,
                bold: true,
                dimmed: false,
            }
        } else {
            NameCol::BlankColoredDim { color }
        };
        let mut content = vec![(piece, text_style.clone())];
        // The trailer follows the body's final line, so a wrapped annotation
        // does not push its target off the first row.
        if index == last {
            content.push((trailer.clone(), trailer_style.clone()));
        }
        push_row(
            lines,
            LedgerRow {
                timing: timing(),
                name,
                separator_dimmed: true,
                tool_output_id: None,
                clickable: false,
            },
            content,
        );
    }

    // A blank row after the block, matching the separation between message
    // blocks. Consecutive blanks are collapsed later, so a run of annotations
    // does not open a gap per annotation.
    lines.push(RenderedLine::new(Vec::new()));

    ranges.push(AnnotationRange {
        id: annotation.id.clone(),
        start_line,
        // The trailing blank is excluded, so a click on the gap between blocks
        // selects nothing rather than the block above it.
        end_line: lines.len().saturating_sub(1),
    });
}

/// Handle a parsed entry while in summary tool-display mode.
///
/// Returns `true` when the entry was absorbed into (or started) a pending
/// summary group and should be skipped by the normal render path.
fn try_extend_or_start_pending_summary(
    lines: &mut Vec<RenderedLine>,
    messages: &mut Vec<MessageRange>,
    pending: &mut Option<PendingToolSummary>,
    entries: &[RenderableEntry],
    parsed_idx: usize,
    options: &RenderOptions,
) -> bool {
    let parsed = &entries[parsed_idx];
    let entry_index = parsed.entry_index;
    let entry = &parsed.entry;

    if let Some((parent_id, agent, timestamp, summary)) =
        tool_only_assistant_summary(entry, options)
    {
        match pending {
            Some(p) if p.parent_id.as_deref() == parent_id && p.agent.as_deref() == agent => {
                p.last_parsed_idx = parsed_idx;
                p.summary.merge(summary);
            }
            _ => {
                flush_tool_summary(lines, messages, pending, entries, options);
                *pending = Some(PendingToolSummary {
                    id: make_tool_summary_output_id(entry_index, parent_id),
                    first_entry_index: entry_index,
                    first_parsed_idx: parsed_idx,
                    last_parsed_idx: parsed_idx,
                    parent_id: parent_id.map(str::to_string),
                    agent: agent.map(str::to_string),
                    timestamp: timestamp.map(str::to_string),
                    summary,
                });
            }
        }
        return true;
    }

    if user_entry_is_only_tool_results(entry, options) {
        if let Some(p) = pending {
            p.last_parsed_idx = parsed_idx;
        }
        return true;
    }

    false
}

/// Render one parsed entry and, if it produced a navigable message,
/// append a `MessageRange` that excludes any trailing blank line.
fn render_entry_with_range(
    lines: &mut Vec<RenderedLine>,
    messages: &mut Vec<MessageRange>,
    parsed: &RenderableEntry,
    options: &RenderOptions,
) {
    let entry_index = parsed.entry_index;
    let entry = &parsed.entry;
    let is_message = matches!(entry, LogEntry::User { .. } | LogEntry::Assistant { .. })
        || matches!(entry, LogEntry::Progress { data, .. }
            if options.show_thinking && crate::claude::parse_agent_progress(data).is_some());

    let start_line = lines.len();
    render_entry(lines, entry_index, entry, options);
    let end_line = lines.len();

    if !is_message {
        return;
    }
    if let Some(range) =
        message_range_excluding_trailing_blank(lines, start_line, end_line, entry_index)
    {
        messages.push(range);
    }
}

/// If the rendered slice produced any non-blank lines, return a
/// `MessageRange` whose `end_line` excludes a trailing blank.
fn message_range_excluding_trailing_blank(
    lines: &[RenderedLine],
    start_line: usize,
    end_line: usize,
    entry_index: usize,
) -> Option<MessageRange> {
    if end_line <= start_line {
        return None;
    }
    let effective_end = if lines.get(end_line - 1).is_some_and(|l| l.spans.is_empty()) {
        end_line - 1
    } else {
        end_line
    };
    if effective_end <= start_line {
        return None;
    }
    Some(MessageRange {
        entry_index,
        start_line,
        end_line: effective_end,
    })
}

/// Collapse consecutive blank rendered lines and remap message ranges so
/// they continue to point at their original visible content.
///
/// Multiple render helpers each push a trailing blank line, which can
/// produce adjacent blanks when a tool result emits empty output. The
/// dedup pass removes any blank line whose immediate predecessor is also
/// blank, and the remap pass shifts every range start/end onto the new
/// line indices, clamping ranges that ended on a removed blank.
fn postprocess_blank_lines(lines: &mut Vec<RenderedLine>, messages: &mut Vec<MessageRange>) {
    let mut removed = vec![false; lines.len()];
    let mut i = 1;
    while i < lines.len() {
        if lines[i].spans.is_empty() && lines[i - 1].spans.is_empty() {
            removed[i] = true;
        }
        i += 1;
    }

    // Build index mapping: old line index -> new line index. Removed
    // entries get the index they would collapse onto; they are never
    // dereferenced for surviving ranges because the remap below walks
    // backward off any removed terminator first.
    let mut new_index = Vec::with_capacity(lines.len());
    let mut offset = 0usize;
    for (idx, &is_removed) in removed.iter().enumerate() {
        if is_removed {
            new_index.push(idx - offset);
            offset += 1;
        } else {
            new_index.push(idx - offset);
        }
    }
    let total_after = lines.len() - offset;

    // Compact in place.
    let mut write = 0;
    for (read, &is_removed) in removed.iter().enumerate() {
        if !is_removed {
            if write != read {
                lines.swap(write, read);
            }
            write += 1;
        }
    }
    lines.truncate(total_after);

    for msg in messages.iter_mut() {
        msg.start_line = new_index[msg.start_line];
        if msg.end_line > 0 && msg.end_line <= new_index.len() {
            // end_line is exclusive — find the new index of the last
            // non-removed line before it and add 1.
            let mut last = msg.end_line - 1;
            while last > msg.start_line && removed[last] {
                last -= 1;
            }
            msg.end_line = new_index[last] + 1;
        } else if msg.end_line == new_index.len() {
            msg.end_line = total_after;
        }
        msg.end_line = msg.end_line.min(total_after);
        msg.start_line = msg.start_line.min(msg.end_line);
    }

    messages.retain(|m| m.start_line < m.end_line);
}

/// Render a conversation file to lines for display in the TUI viewer
pub fn render_conversation(
    file_path: &Path,
    options: &RenderOptions,
) -> std::io::Result<RenderedConversation> {
    let entries = parse_conversation_file(file_path)?;
    Ok(render_parsed_conversation(&entries, options))
}

#[cfg(test)]
mod tests;
