use crate::tool_format;

use super::ledger::{
    LedgerRow, NameCol, push_row, render_continuation_dimmed, render_ledger_block_plain_dimmed,
    render_truncation_indicator,
};
use super::markdown::render_markdown_to_lines;
use super::timing::TimingSlot;
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ToolOutputKind {
    ToolCall,
    ToolResult,
}

impl ToolOutputKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::ToolCall => "call",
            Self::ToolResult => "result",
        }
    }
}
pub(super) fn make_tool_output_id(
    entry_index: usize,
    parent_id: Option<&str>,
    block_index: usize,
    kind: ToolOutputKind,
    raw_id: Option<&str>,
) -> ToolOutputId {
    let parent = parent_id.unwrap_or("top");
    let raw = raw_id.unwrap_or("none");
    ToolOutputId(format!(
        "entry:{entry_index}:parent:{parent}:block:{block_index}:kind:{}:id:{raw}",
        kind.as_str()
    ))
}

pub(super) fn make_tool_summary_output_id(
    entry_index: usize,
    parent_id: Option<&str>,
) -> ToolOutputId {
    let parent = parent_id.unwrap_or("top");
    ToolOutputId(format!("entry:{entry_index}:parent:{parent}:kind:summary"))
}
pub(super) use crate::turns::{
    tool_result_json as format_tool_result_content, tool_result_prose as extract_tool_result_text,
};

/// Pick the display text for a tool result: prefer extracted text content,
/// fall back to a JSON pretty-print for objects, null, or text-less arrays.
pub(super) fn tool_result_display_text(content: Option<&serde_json::Value>) -> String {
    extract_tool_result_text(content).unwrap_or_else(|| format_tool_result_content(content))
}

/// Descriptor for one tool-call rendering pass shared by entry and
/// summary-expansion render paths.
pub(super) struct ToolCallRenderSpec<'a> {
    pub name: &'a str,
    pub input: &'a serde_json::Value,
    pub label: &'a str,
    pub label_color: (u8, u8, u8),
    pub dimmed: bool,
    pub content_width: usize,
    pub timing: TimingSlot<'a>,
    pub tool_display: ToolDisplayMode,
    pub tool_output_id: &'a ToolOutputId,
    pub expanded: bool,
}

/// Descriptor for one tool-result rendering pass shared by entry and
/// summary-expansion render paths.
pub(super) struct ToolResultRenderSpec<'a> {
    pub text: &'a str,
    pub content_width: usize,
    pub timing: TimingSlot<'a>,
    pub tool_display: ToolDisplayMode,
    pub tool_output_id: &'a ToolOutputId,
    pub expanded: bool,
}
/// Render a formatted tool call with proper styling
pub(super) fn render_tool_call(lines: &mut Vec<RenderedLine>, spec: &ToolCallRenderSpec<'_>) {
    let ToolCallRenderSpec {
        name,
        input,
        label,
        label_color,
        dimmed,
        content_width,
        timing,
        tool_display,
        tool_output_id,
        expanded,
    } = *spec;
    let formatted = tool_format::format_tool_call(name, input, content_width);

    let header_content = vec![(
        formatted.header.clone(),
        LineStyle {
            fg: Some(th().tool_text),
            dimmed,
            ..Default::default()
        },
    )];
    push_row(
        lines,
        LedgerRow {
            timing,
            name: NameCol::Label {
                text: label,
                color: label_color,
                bold: false,
                dimmed,
            },
            separator_dimmed: dimmed,
            tool_output_id: Some(tool_output_id),
            clickable: false,
        },
        header_content,
    );

    // Render the body if present, with empty line separator
    if let Some(body) = formatted.body {
        let body_timing = timing.continuation();

        // Empty line between header and body
        push_row(
            lines,
            LedgerRow {
                timing: body_timing,
                name: NameCol::BlankPlain,
                separator_dimmed: dimmed,
                tool_output_id: Some(tool_output_id),
                clickable: false,
            },
            Vec::new(),
        );

        if tool_display == ToolDisplayMode::Truncated && !expanded {
            let body_lines: Vec<&str> = body.lines().collect();
            let total = body_lines.len();
            if total > TRUNCATED_BODY_LINES {
                let truncated = body_lines[..TRUNCATED_BODY_LINES].join("\n");
                render_tool_body(
                    lines,
                    &truncated,
                    dimmed,
                    body_timing,
                    Some(tool_output_id),
                    true,
                );
                render_truncation_indicator(
                    lines,
                    total - TRUNCATED_BODY_LINES,
                    dimmed,
                    body_timing,
                    Some(tool_output_id),
                );
            } else {
                render_tool_body(lines, &body, dimmed, body_timing, None, false);
            }
        } else {
            let id = (tool_display == ToolDisplayMode::Truncated).then_some(tool_output_id);
            render_tool_body(lines, &body, dimmed, body_timing, id, id.is_some());
        }
    }
}

/// Render tool body with diff-aware coloring
fn render_tool_body(
    lines: &mut Vec<RenderedLine>,
    text: &str,
    dimmed: bool,
    timing: TimingSlot<'_>,
    tool_output_id: Option<&ToolOutputId>,
    clickable: bool,
) {
    for line in text.lines() {
        let style = if line.starts_with("+ ") {
            LineStyle {
                fg: Some(th().diff_add),
                dimmed,
                ..Default::default()
            }
        } else if line.starts_with("- ") {
            LineStyle {
                fg: Some(th().diff_remove),
                dimmed,
                ..Default::default()
            }
        } else {
            LineStyle {
                dimmed: true,
                ..Default::default()
            }
        };
        push_row(
            lines,
            LedgerRow {
                timing,
                name: NameCol::BlankPlain,
                separator_dimmed: dimmed,
                tool_output_id,
                clickable,
            },
            vec![(line.to_string(), style)],
        );
    }
}

/// Render tool result with arrow indicator and markdown
pub(super) fn render_tool_result(lines: &mut Vec<RenderedLine>, spec: &ToolResultRenderSpec<'_>) {
    let ToolResultRenderSpec {
        text,
        content_width,
        timing,
        tool_display,
        tool_output_id,
        expanded,
    } = *spec;
    // Fence plain text tool results to prevent markdown misinterpretation.
    // If the result already contains fenced code blocks, assume it's intentional markdown.
    let text = if text.contains("```") {
        text.to_string()
    } else {
        format!("```text\n{}\n```", text)
    };
    // Render markdown
    let styled_lines = render_markdown_to_lines(&text, content_width);

    let total = styled_lines.len();
    let limit = if tool_display == ToolDisplayMode::Truncated
        && !expanded
        && total > TRUNCATED_RESULT_LINES
    {
        TRUNCATED_RESULT_LINES
    } else {
        total
    };

    let continuation = timing.continuation();
    for (i, styled_line) in styled_lines.iter().take(limit).enumerate() {
        let row_timing = if i == 0 { timing } else { continuation };
        let name_col = if i == 0 {
            NameCol::Label {
                text: "↳ Result",
                color: th().tool_text,
                bold: false,
                dimmed: false,
            }
        } else {
            NameCol::BlankPlain
        };
        let content: Vec<_> = styled_line
            .spans
            .iter()
            .map(|(t, s)| (t.clone(), s.clone()))
            .collect();
        let clickable = tool_display == ToolDisplayMode::Truncated && (expanded || limit < total);
        let id = clickable.then_some(tool_output_id);
        push_row(
            lines,
            LedgerRow {
                timing: row_timing,
                name: name_col,
                separator_dimmed: false,
                tool_output_id: id,
                clickable,
            },
            content,
        );
    }

    if limit < total {
        render_truncation_indicator(
            lines,
            total - limit,
            false,
            continuation,
            Some(tool_output_id),
        );
    }
}

/// Render the dimmed body of a subagent tool result.
///
/// In truncated tool-display mode this emits at most `TRUNCATED_RESULT_LINES`
/// of the result followed by a clickable "(N more lines...)" indicator;
/// otherwise it renders the full result as a continuation block. Used by
/// both the user-message subagent branch and the agent-progress user
/// branch.
pub(super) fn render_dimmed_tool_result_body(
    lines: &mut Vec<RenderedLine>,
    options: &RenderOptions,
    output_id: &ToolOutputId,
    expanded: bool,
    content_str: &str,
    timing: TimingSlot<'_>,
) {
    let truncated_mode = options.tool_display == ToolDisplayMode::Truncated;
    if truncated_mode && !expanded {
        let content_lines: Vec<&str> = content_str.lines().collect();
        let total = content_lines.len();
        if total > TRUNCATED_RESULT_LINES {
            let truncated = content_lines[..TRUNCATED_RESULT_LINES].join("\n");
            render_continuation_dimmed(lines, &truncated, timing, Some(output_id));
            render_truncation_indicator(
                lines,
                total - TRUNCATED_RESULT_LINES,
                true,
                timing,
                Some(output_id),
            );
        } else {
            render_continuation_dimmed(lines, content_str, timing, None);
        }
    } else {
        let id = truncated_mode.then_some(output_id);
        render_continuation_dimmed(lines, content_str, timing, id);
    }
}

/// Render the "  ↳ Tool │ <Result>" header that introduces a dimmed
/// subagent tool result block.
pub(super) fn render_subagent_tool_result_header(
    lines: &mut Vec<RenderedLine>,
    timing: TimingSlot<'_>,
) {
    render_ledger_block_plain_dimmed(lines, "  ↳ Tool", th().accent_dim, "<Result>", timing);
}
