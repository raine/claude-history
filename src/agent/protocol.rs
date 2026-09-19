use crate::agent::diagnostic::AgentError;
use crate::agent::diagnostic::{AgentWarning, format_warning_records};
use crate::agent::records::{Cut, Response};
use crate::agent::refs::ResolvedConversation;
use crate::agent::sanitize::sanitize_agent_text;
use crate::agent::transcript::{
    AgentMessage, AgentMessagePart, AgentMessageRole, AgentTranscript, MAX_AGENT_SEGMENT_CHARS,
    bounded_tool_summary,
};
use crate::agent::visibility::ContentVisibility;
use crate::error::Result;
use crate::history::MessageRange;
use serde_json::Value;
use std::collections::BTreeSet;
use std::str::FromStr;

const OUTLINE_SHORT_MESSAGE_LIMIT: usize = 20;
const OUTLINE_SEGMENT_SIZE: usize = 10;
const SNIPPET_LIMIT: usize = 80;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MessageLineRange {
    pub start: usize,
    pub end: usize,
}

impl FromStr for MessageLineRange {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        let (start, end) = value
            .split_once("..")
            .ok_or_else(|| "line range must use START..END".to_string())?;
        let start = start
            .parse::<usize>()
            .map_err(|_| "line range must contain positive integers".to_string())?;
        let end = end
            .parse::<usize>()
            .map_err(|_| "line range must contain positive integers".to_string())?;
        if start == 0 || end == 0 {
            return Err("line range starts at line 1".to_string());
        }
        if start > end {
            return Err("line range start must not exceed its end".to_string());
        }
        Ok(Self { start, end })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadSlice {
    Lines(MessageLineRange),
    Match { query: String, context: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolOptions {
    pub budget: Option<usize>,
    pub visibility: ContentVisibility,
}

#[derive(Clone, Debug)]
pub struct ReadRequest<'a> {
    pub resolved: &'a ResolvedConversation,
    pub transcript: &'a AgentTranscript,
    pub range: Option<MessageRange>,
}

#[derive(Clone, Debug)]
pub struct ProtocolFocus {
    pub conversation_full_ref: Option<String>,
    pub range: MessageRange,
}

#[derive(Clone, Debug)]
struct RenderedMessage<'a> {
    conversation: &'a ResolvedConversation,
    transcript: &'a AgentTranscript,
    message: &'a AgentMessage,
    body: String,
    slice: Option<String>,
}

#[cfg(test)]
pub fn format_read(
    requests: &[ReadRequest<'_>],
    focus: Option<ProtocolFocus>,
    slice: Option<&ReadSlice>,
    options: ProtocolOptions,
) -> Result<String> {
    format_read_with_warnings(requests, focus, slice, options, &[])
}

pub fn format_read_with_warnings(
    requests: &[ReadRequest<'_>],
    focus: Option<ProtocolFocus>,
    slice: Option<&ReadSlice>,
    options: ProtocolOptions,
    warnings: &[AgentWarning],
) -> Result<String> {
    let mut messages = Vec::new();
    for request in requests {
        let selected = selected_messages(request.transcript, request.range)?;
        for message in selected {
            if let Some(rendered) =
                render_message(request.resolved, request.transcript, message, options)
            {
                messages.push(apply_read_slice(rendered, slice)?);
            }
        }
    }

    let protected = focused_indices(&messages, focus.as_ref());
    let mut selected = select_for_budget(&messages, focus, options.budget);
    let mut cut = cut_marker(messages.len(), &selected);
    let (warning_count, warning_records) = format_warning_records(warnings);
    let render = |messages: &[RenderedMessage<'_>], selected: &[usize], cut: &str| {
        let mut output = String::new();
        let warning_suffix = if warning_records.is_empty() {
            String::new()
        } else {
            format!(
                " warnings={warning_count} warning-records={}",
                warning_records.len()
            )
        };
        output.push_str(&format!(
            "protocol agent-read cut={} chars={} policy={} omit={}{}\n",
            escape_atom(cut),
            budget_atom(options.budget),
            options.visibility.atom(),
            omitted_message_ranges(messages, selected),
            warning_suffix
        ));
        let mut last_ref = None;
        render_selected_messages(&mut output, messages, selected, &mut last_ref);
        for record in continuation_records(messages, selected) {
            output.push_str(&record);
        }
        for record in &warning_records {
            if options
                .budget
                .is_none_or(|budget| output.chars().count() + record.chars().count() <= budget)
            {
                output.push_str(record);
            }
        }
        output
    };

    let mut output = render(&messages, &selected, &cut);
    if let Some(budget) = options.budget
        && output.chars().count() > budget
    {
        cut = if selected.len() == messages.len() {
            "body".to_string()
        } else {
            cut
        };
        if !protected.is_empty() {
            selected.retain(|index| protected.contains(index));
            cut = cut_marker(messages.len(), &selected);
        }
        trim_selected_message_bodies(&mut messages, &selected, budget, &render, &cut);
        output = render(&messages, &selected, &cut);
        while output.chars().count() > budget && !selected.is_empty() {
            let removable = selected
                .iter()
                .rposition(|index| !protected.contains(index));
            let Some(removable) = removable else {
                break;
            };
            selected.remove(removable);
            cut = cut_marker(messages.len(), &selected);
            trim_selected_message_bodies(&mut messages, &selected, budget, &render, &cut);
            output = render(&messages, &selected, &cut);
        }
        if output.chars().count() > budget {
            output = output.chars().take(budget).collect();
        }
    }
    Ok(output)
}

#[cfg(test)]
pub fn format_outline(
    resolved: &ResolvedConversation,
    transcript: &AgentTranscript,
    options: ProtocolOptions,
) -> String {
    format_outline_with_warnings(resolved, transcript, options, &[])
}

pub fn format_outline_with_warnings(
    resolved: &ResolvedConversation,
    transcript: &AgentTranscript,
    options: ProtocolOptions,
    warnings: &[AgentWarning],
) -> String {
    let visible: Vec<_> = transcript
        .messages
        .iter()
        .filter_map(|message| render_message(resolved, transcript, message, options))
        .collect();
    let (warning_count, warning_records) = format_warning_records(warnings);
    let warning_suffix = if warning_records.is_empty() {
        String::new()
    } else {
        format!(
            " warnings={warning_count} warning-records={}",
            warning_records.len()
        )
    };
    let conversation = conversation_record(resolved);
    let short = visible.len() <= OUTLINE_SHORT_MESSAGE_LIMIT;
    // Units are messages when the outline is short, segments otherwise; the
    // number of messages a cut omits follows from the units kept.
    let omitted_from = |cut: &Cut| {
        let visible_index = if short {
            cut.kept_units
        } else {
            cut.kept_units.saturating_mul(OUTLINE_SEGMENT_SIZE)
        };
        (visible_index, visible.len().saturating_sub(visible_index))
    };
    let header = |cut: Option<&Cut>| match cut {
        None => format!(
            "protocol agent-outline cut=none chars={} policy={}{warning_suffix}\n{conversation}",
            budget_atom(options.budget),
            options.visibility.atom(),
        ),
        Some(cut) => format!(
            "protocol agent-outline cut=tail chars={} policy={} omitted-records={} warnings={warning_count} warnings-emitted=0\n{conversation}",
            budget_atom(options.budget),
            options.visibility.atom(),
            omitted_from(cut).1,
        ),
    };
    let cut_footer = |cut: &Cut| {
        let (visible_index, omitted) = omitted_from(cut);
        if omitted == 0 {
            return String::new();
        }
        let start = visible[visible_index].message.ordinal;
        let end = visible
            .last()
            .expect("omitted outline has a message")
            .message
            .ordinal;
        format!(
            "continue read ref={}:m{start}..m{end}\n",
            resolved.reference.canonical()
        )
    };
    let fallback = || {
        format!(
            "protocol agent-outline cut=tail chars={} policy={} omitted-records={}\n",
            budget_atom(options.budget),
            options.visibility.atom(),
            visible.len()
        )
    };

    let units = if short {
        visible
            .iter()
            .map(|rendered| {
                format!(
                    "m{} role={} chars={} anchor={} | {}\n",
                    rendered.message.ordinal,
                    role_atom(rendered.message.role),
                    rendered.body.chars().count(),
                    transcript.message_anchor(resolved, rendered.message),
                    snippet(&rendered.body)
                )
            })
            .collect()
    } else {
        visible
            .chunks(OUTLINE_SEGMENT_SIZE)
            .map(|chunk| {
                let first = chunk.first().expect("chunk is non-empty");
                let last = chunk.last().expect("chunk is non-empty");
                let count: usize = chunk
                    .iter()
                    .map(|message| message.body.chars().count())
                    .sum();
                format!(
                    "seg m{}..m{} chars={} anchors={}..{} | {} / {}\n",
                    first.message.ordinal,
                    last.message.ordinal,
                    count,
                    transcript.message_anchor(resolved, first.message),
                    transcript.message_anchor(resolved, last.message),
                    snippet(&first.body),
                    snippet(&last.body)
                )
            })
            .collect()
    };

    Response {
        budget: options.budget,
        header: &header,
        units,
        whole_trailer: warning_records.concat(),
        cut_footer: &cut_footer,
        fallback: &fallback,
    }
    .render()
}

fn conversation_record(resolved: &ResolvedConversation) -> String {
    format!(
        "conversation project={} uuid={} ref={}\n",
        escape_atom(&resolved.key.project_id()),
        escape_atom(&resolved.reference.uuid()),
        escape_atom(&resolved.reference.canonical())
    )
}

pub fn escape_atom(value: &str) -> String {
    let mut escaped = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'.'
            | b'_'
            | b'~'
            | b':'
            | b'/'
            | b'+'
            | b'-' => escaped.push(byte as char),
            _ => escaped.push_str(&format!("%{byte:02X}")),
        }
    }
    escaped
}

fn selected_messages(
    transcript: &AgentTranscript,
    range: Option<MessageRange>,
) -> Result<Vec<&AgentMessage>> {
    let Some(range) = range else {
        return Ok(transcript.messages.iter().collect());
    };
    let max = transcript.messages.len();
    if range.end > max {
        return Err(AgentError::out_of_range(
            None,
            format!(
                "message range m{}..m{} exceeds transcript length m{}",
                range.start, range.end, max
            ),
        )
        .into());
    }
    Ok(transcript
        .messages
        .iter()
        .filter(|message| range.start <= message.ordinal && message.ordinal <= range.end)
        .collect())
}

fn render_message<'a>(
    conversation: &'a ResolvedConversation,
    transcript: &'a AgentTranscript,
    message: &'a AgentMessage,
    options: ProtocolOptions,
) -> Option<RenderedMessage<'a>> {
    let visibility = options.visibility;
    if !visibility.message_is_visible(message) {
        return None;
    }
    let mut parts = Vec::new();
    for part in &message.parts {
        if !visibility.part_is_visible(part) {
            continue;
        }
        match part {
            AgentMessagePart::Text { text, .. } => parts.push(sanitize_agent_text(text)),
            AgentMessagePart::ToolUse { name, input, .. } => {
                parts.push(sanitize_agent_text(&bounded_tool_summary(
                    name,
                    input,
                    MAX_AGENT_SEGMENT_CHARS,
                )));
            }
            AgentMessagePart::ToolResult {
                content: Some(content),
                ..
            } => {
                parts.push(sanitize_agent_text(&tool_result_text(content)));
            }
            AgentMessagePart::Thinking { thinking, .. } => {
                parts.push(format!("thinking: {}", sanitize_agent_text(thinking)));
            }
            AgentMessagePart::ToolResult { content: None, .. } => {}
        }
    }
    let body = parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!body.is_empty()).then_some(RenderedMessage {
        conversation,
        transcript,
        message,
        body,
        slice: None,
    })
}

fn apply_read_slice<'a>(
    mut rendered: RenderedMessage<'a>,
    slice: Option<&ReadSlice>,
) -> Result<RenderedMessage<'a>> {
    let Some(slice) = slice else {
        return Ok(rendered);
    };
    let lines = rendered.body.split('\n').collect::<Vec<_>>();
    match slice {
        ReadSlice::Lines(range) => {
            if range.end > lines.len() {
                return Err(AgentError::out_of_range(
                    None,
                    format!(
                        "content line range {}..{} exceeds message m{} length {}",
                        range.start,
                        range.end,
                        rendered.message.ordinal,
                        lines.len()
                    ),
                )
                .into());
            }
            rendered.body = numbered_lines(&lines, range.start..=range.end, &[]);
            rendered.slice = Some(format!("lines={}..{}", range.start, range.end));
        }
        ReadSlice::Match { query, context } => {
            let query_lower = query.to_lowercase();
            let matches = lines
                .iter()
                .enumerate()
                .filter_map(|(index, line)| {
                    line.to_lowercase()
                        .contains(&query_lower)
                        .then_some(index + 1)
                })
                .collect::<Vec<_>>();
            let mut selected = BTreeSet::new();
            for line in &matches {
                let start = line.saturating_sub(*context).max(1);
                let end = line.saturating_add(*context).min(lines.len());
                selected.extend(start..=end);
            }
            rendered.body = numbered_lines(&lines, selected.iter().copied(), &matches);
            rendered.slice = Some(format!(
                "match={} context={} hits={}",
                escape_atom(query),
                context,
                matches.len()
            ));
        }
    }
    Ok(rendered)
}

fn numbered_lines(
    lines: &[&str],
    selected: impl IntoIterator<Item = usize>,
    matches: &[usize],
) -> String {
    let selected = selected.into_iter().collect::<Vec<_>>();
    let mut output = Vec::new();
    let mut previous = None;
    for line_number in selected {
        if previous.is_some_and(|previous| line_number > previous + 1) {
            output.push("...".to_string());
        }
        let marker = if matches.contains(&line_number) {
            ">"
        } else {
            " "
        };
        output.push(format!("{marker}{line_number}: {}", lines[line_number - 1]));
        previous = Some(line_number);
    }
    output.join("\n")
}

fn trim_selected_message_bodies(
    messages: &mut [RenderedMessage<'_>],
    selected: &[usize],
    budget: usize,
    render: &impl Fn(&[RenderedMessage<'_>], &[usize], &str) -> String,
    cut: &str,
) {
    if selected.is_empty() {
        return;
    }
    let empty_bodies = selected
        .iter()
        .map(|index| (*index, std::mem::take(&mut messages[*index].body)))
        .collect::<Vec<_>>();
    let base_len = render(messages, selected, cut).chars().count();
    let available = budget.saturating_sub(base_len) / selected.len();
    for (index, body) in empty_bodies {
        messages[index].body = truncated_message_body(&messages[index], &body, available);
    }
}

fn truncated_message_body(_rendered: &RenderedMessage<'_>, body: &str, available: usize) -> String {
    if framed_body_len(body) <= available {
        return body.to_string();
    }
    let total_chars = body.chars().count();
    let hint = format!("[omitted chars={{start}}..{total_chars}; use --lines or --match]");
    let mut low = 0;
    let mut high = total_chars;
    let mut best = String::new();
    while low <= high {
        let keep = low + (high - low) / 2;
        let start = keep + 1;
        let marker = hint.replace("{start}", &start.to_string());
        let mut candidate = body.chars().take(keep).collect::<String>();
        if keep > 0 && !candidate.ends_with('\n') {
            candidate.push('\n');
        }
        candidate.push_str(&marker);
        if framed_body_len(&candidate) <= available {
            best = candidate;
            low = keep + 1;
        } else if keep == 0 {
            break;
        } else {
            high = keep - 1;
        }
    }
    if best.is_empty() {
        let compact = format!("[omit c1..{total_chars}]");
        if framed_body_len(&compact) <= available {
            return compact;
        }
    }
    best
}

fn continuation_records(messages: &[RenderedMessage<'_>], selected: &[usize]) -> Vec<String> {
    let selected = selected.iter().copied().collect::<BTreeSet<_>>();
    let mut groups: Vec<(&ResolvedConversation, Vec<usize>)> = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        if selected.contains(&index) {
            continue;
        }
        if let Some((_, ordinals)) = groups.iter_mut().find(|(conversation, _)| {
            conversation.reference.full_ref() == message.conversation.reference.full_ref()
        }) {
            ordinals.push(message.message.ordinal);
        } else {
            groups.push((message.conversation, vec![message.message.ordinal]));
        }
    }
    groups
        .into_iter()
        .map(|(conversation, ordinals)| {
            let refs = compact_ordinals(&conversation.reference.canonical(), &ordinals);
            format!("continue read refs={}\n", escape_atom(&refs.join(";")))
        })
        .collect()
}

fn compact_ordinals(reference: &str, ordinals: &[usize]) -> Vec<String> {
    let mut ranges = Vec::new();
    let mut start = ordinals[0];
    let mut end = start;
    for ordinal in ordinals.iter().copied().skip(1) {
        if ordinal == end + 1 {
            end = ordinal;
        } else {
            ranges.push(format_message_range(reference, start, end));
            start = ordinal;
            end = ordinal;
        }
    }
    ranges.push(format_message_range(reference, start, end));
    ranges
}

fn format_message_range(reference: &str, start: usize, end: usize) -> String {
    if start == end {
        format!("{reference}:m{start}")
    } else {
        format!("{reference}:m{start}..m{end}")
    }
}

fn omitted_message_ranges(messages: &[RenderedMessage<'_>], selected: &[usize]) -> String {
    let qualify = messages.first().is_some_and(|first| {
        messages.iter().any(|message| {
            message.conversation.reference.full_ref() != first.conversation.reference.full_ref()
        })
    });
    let selected = selected.iter().copied().collect::<BTreeSet<_>>();
    let mut ranges = Vec::new();
    let mut start = None;
    for index in 0..=messages.len() {
        let omitted = index < messages.len() && !selected.contains(&index);
        match (start, omitted) {
            (None, true) => start = Some(index),
            (Some(first), false) => {
                let last = index - 1;
                let first_message = &messages[first];
                let last_message = &messages[last];
                let prefix = qualify
                    .then(|| format!("{}:", first_message.conversation.reference.canonical()))
                    .unwrap_or_default();
                if first_message.conversation.reference.full_ref()
                    == last_message.conversation.reference.full_ref()
                {
                    ranges.push(if first == last {
                        format!("{prefix}m{}", first_message.message.ordinal)
                    } else {
                        format!(
                            "{prefix}m{}..m{}",
                            first_message.message.ordinal, last_message.message.ordinal
                        )
                    });
                } else {
                    for message in &messages[first..=last] {
                        ranges.push(format!(
                            "{}:m{}",
                            message.conversation.reference.canonical(),
                            message.message.ordinal
                        ));
                    }
                }
                start = None;
            }
            _ => {}
        }
    }
    if ranges.is_empty() {
        "none".to_string()
    } else {
        ranges.join(",")
    }
}

fn framed_body_len(body: &str) -> usize {
    body.split('\n').map(framed_line_len).sum()
}

fn framed_line_len(line: &str) -> usize {
    if line.is_empty() {
        2
    } else {
        line.chars().count() + 3
    }
}

fn focused_indices(
    messages: &[RenderedMessage<'_>],
    focus: Option<&ProtocolFocus>,
) -> BTreeSet<usize> {
    let Some(focus) = focus else {
        return BTreeSet::new();
    };
    messages
        .iter()
        .enumerate()
        .filter_map(|(index, rendered)| {
            let conversation_matches = focus
                .conversation_full_ref
                .as_deref()
                .is_none_or(|target| rendered.conversation.reference.full_ref() == target);
            (conversation_matches
                && focus.range.start <= rendered.message.ordinal
                && rendered.message.ordinal <= focus.range.end)
                .then_some(index)
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SelectionWork {
    serialized_messages: usize,
    candidate_checks: usize,
}

struct CompactReadLayout {
    message_chars: Vec<usize>,
    conversation_chars: Vec<usize>,
    conversation_refs: Vec<String>,
}

impl CompactReadLayout {
    fn new(messages: &[RenderedMessage<'_>]) -> Self {
        Self {
            message_chars: messages
                .iter()
                .map(|message| message_record(message).chars().count())
                .collect(),
            conversation_chars: messages
                .iter()
                .map(|message| conversation_record(message.conversation).chars().count())
                .collect(),
            conversation_refs: messages
                .iter()
                .map(|message| message.conversation.reference.full_ref())
                .collect(),
        }
    }

    fn rendered_chars(&self, selected: &BTreeSet<usize>, cut: &str, budget: usize) -> usize {
        let mut chars = format!("protocol agent-read cut={cut} budget-chars={budget}\n")
            .chars()
            .count();
        let mut last_ref = None;
        for index in selected {
            if last_ref != Some(self.conversation_refs[*index].as_str()) {
                chars += self.conversation_chars[*index];
                last_ref = Some(self.conversation_refs[*index].as_str());
            }
            chars += self.message_chars[*index];
        }
        chars
    }

    fn expansion_chars(&self, candidate: usize, neighbor: usize) -> usize {
        self.message_chars[candidate]
            + if self.conversation_refs[candidate] == self.conversation_refs[neighbor] {
                0
            } else {
                self.conversation_chars[candidate]
            }
    }
}

fn select_for_budget(
    messages: &[RenderedMessage<'_>],
    focus: Option<ProtocolFocus>,
    budget: Option<usize>,
) -> Vec<usize> {
    select_for_budget_with_work(messages, focus, budget).0
}

fn select_for_budget_with_work(
    messages: &[RenderedMessage<'_>],
    focus: Option<ProtocolFocus>,
    budget: Option<usize>,
) -> (Vec<usize>, SelectionWork) {
    let all = (0..messages.len()).collect::<BTreeSet<_>>();
    let Some(budget) = budget else {
        return (
            all.into_iter().collect(),
            SelectionWork {
                serialized_messages: messages.len(),
                candidate_checks: 0,
            },
        );
    };
    let layout = CompactReadLayout::new(messages);
    let mut work = SelectionWork {
        serialized_messages: messages.len(),
        candidate_checks: 0,
    };
    if layout.rendered_chars(&all, "none", budget) <= budget {
        return (all.into_iter().collect(), work);
    }

    let mut selected = BTreeSet::new();
    if let Some(focus) = focus {
        for (index, rendered) in messages.iter().enumerate() {
            let conversation_matches = focus
                .conversation_full_ref
                .as_deref()
                .is_none_or(|target| rendered.conversation.reference.full_ref() == target);
            if conversation_matches
                && focus.range.start <= rendered.message.ordinal
                && rendered.message.ordinal <= focus.range.end
            {
                selected.insert(index);
            }
        }
    }
    if selected.is_empty() && !messages.is_empty() {
        selected.insert(0);
    }

    let mut rendered_chars = layout.rendered_chars(&selected, "head+focus+tail", budget);
    loop {
        let mut changed = false;
        if let Some(first) = selected.first().copied()
            && first > 0
        {
            work.candidate_checks += 1;
            let candidate = first - 1;
            let added = layout.expansion_chars(candidate, first);
            if rendered_chars.saturating_add(added) <= budget {
                selected.insert(candidate);
                rendered_chars += added;
                changed = true;
            }
        }
        if let Some(last) = selected.last().copied()
            && last + 1 < messages.len()
        {
            work.candidate_checks += 1;
            let candidate = last + 1;
            let added = layout.expansion_chars(candidate, last);
            if rendered_chars.saturating_add(added) <= budget {
                selected.insert(candidate);
                rendered_chars += added;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    (selected.into_iter().collect(), work)
}

fn message_record(rendered: &RenderedMessage<'_>) -> String {
    let mut output = format!(
        "message m{} role={} line={}{} anchor={}\n",
        rendered.message.ordinal,
        role_atom(rendered.message.role),
        rendered.message.jsonl_line,
        rendered
            .slice
            .as_ref()
            .map(|slice| format!(" slice={slice}"))
            .unwrap_or_default(),
        rendered
            .transcript
            .message_anchor(rendered.conversation, rendered.message)
    );
    push_body(&mut output, &rendered.body);
    output
}

fn render_selected_messages(
    output: &mut String,
    messages: &[RenderedMessage<'_>],
    selected: &[usize],
    last_ref: &mut Option<String>,
) {
    for index in selected {
        let rendered = &messages[*index];
        let canonical = rendered.conversation.reference.canonical();
        if last_ref.as_deref() != Some(canonical.as_str()) {
            output.push_str(&conversation_record(rendered.conversation));
            *last_ref = Some(canonical);
        }
        output.push_str(&message_record(rendered));
    }
}

fn cut_marker(total: usize, selected: &[usize]) -> String {
    if selected.len() == total {
        return "none".to_string();
    }
    let Some(first) = selected.first().copied() else {
        return "tail".to_string();
    };
    let last = selected.last().copied().unwrap_or(first);
    let mut parts = Vec::new();
    if first > 0 {
        parts.push("head");
    }
    if selected.windows(2).any(|pair| pair[1] != pair[0] + 1) || (first > 0 && last + 1 < total) {
        parts.push("focus");
    }
    if last + 1 < total {
        parts.push("tail");
    }
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join("+")
    }
}

fn push_body(output: &mut String, body: &str) {
    for line in body.split('\n') {
        if line.is_empty() {
            output.push_str("|\n");
        } else {
            output.push_str("| ");
            output.push_str(line);
            output.push('\n');
        }
    }
}

fn role_atom(role: AgentMessageRole) -> &'static str {
    match role {
        AgentMessageRole::User => "user",
        AgentMessageRole::Assistant => "assistant",
    }
}

fn budget_atom(budget: Option<usize>) -> String {
    budget
        .map(|budget| budget.to_string())
        .unwrap_or_else(|| "none".to_string())
}

fn tool_result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(text) => Some(text.clone()),
                Value::Object(map) => map
                    .get("text")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => content.to_string(),
    }
}

fn snippet(body: &str) -> String {
    let normalized = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= SNIPPET_LIMIT {
        normalized
    } else {
        normalized.chars().take(SNIPPET_LIMIT).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::refs::{AgentConversationKey, AgentConversationRef};
    use crate::agent::test_support::{source, text_message};
    use serde_json::json;
    use std::path::PathBuf;

    fn resolved(filename: &str) -> ResolvedConversation {
        let key =
            AgentConversationKey::new("project with space", filename, PathBuf::from(filename));
        ResolvedConversation {
            reference: AgentConversationRef::from_parts("project with space", filename),
            key,
        }
    }

    fn transcript(messages: Vec<AgentMessage>) -> AgentTranscript {
        crate::agent::test_support::transcript(messages, "test.jsonl")
    }

    fn options() -> ProtocolOptions {
        ProtocolOptions {
            budget: Some(6000),
            visibility: ContentVisibility::default(),
        }
    }

    fn legacy_select_for_budget(
        messages: &[RenderedMessage<'_>],
        focus: Option<ProtocolFocus>,
        budget: usize,
    ) -> Vec<usize> {
        let render_chars = |selected: &BTreeSet<usize>, cut: &str| {
            let mut output = format!("protocol agent-read cut={cut} budget-chars={budget}\n");
            let mut last_ref = None;
            render_selected_messages(
                &mut output,
                messages,
                &selected.iter().copied().collect::<Vec<_>>(),
                &mut last_ref,
            );
            output.chars().count()
        };
        let all = (0..messages.len()).collect::<BTreeSet<_>>();
        if render_chars(&all, "none") <= budget {
            return all.into_iter().collect();
        }
        let mut selected = BTreeSet::new();
        if let Some(focus) = focus {
            for (index, rendered) in messages.iter().enumerate() {
                let conversation_matches = focus
                    .conversation_full_ref
                    .as_deref()
                    .is_none_or(|target| rendered.conversation.reference.full_ref() == target);
                if conversation_matches
                    && focus.range.start <= rendered.message.ordinal
                    && rendered.message.ordinal <= focus.range.end
                {
                    selected.insert(index);
                }
            }
        }
        if selected.is_empty() && !messages.is_empty() {
            selected.insert(0);
        }
        loop {
            let mut changed = false;
            if let Some(first) = selected.first().copied()
                && first > 0
            {
                selected.insert(first - 1);
                if render_chars(&selected, "head+focus+tail") <= budget {
                    changed = true;
                } else {
                    selected.remove(&(first - 1));
                }
            }
            if let Some(last) = selected.last().copied()
                && last + 1 < messages.len()
            {
                selected.insert(last + 1);
                if render_chars(&selected, "head+focus+tail") <= budget {
                    changed = true;
                } else {
                    selected.remove(&(last + 1));
                }
            }
            if !changed {
                break;
            }
        }
        selected.into_iter().collect()
    }

    #[test]
    fn compact_read_layout_matches_legacy_selection_with_linear_serialization() {
        let resolved = resolved("session.jsonl");
        let transcript = transcript(
            (1..=40)
                .map(|ordinal| {
                    text_message(
                        ordinal,
                        AgentMessageRole::User,
                        &format!("message {ordinal} {}", "🙂x".repeat(ordinal % 7 + 1)),
                    )
                })
                .collect(),
        );
        let messages = transcript
            .messages
            .iter()
            .filter_map(|message| render_message(&resolved, &transcript, message, options()))
            .collect::<Vec<_>>();

        for budget in (100..=4_000).step_by(37) {
            let focus = ProtocolFocus {
                conversation_full_ref: Some(resolved.reference.full_ref()),
                range: MessageRange::single(20),
            };
            let expected = legacy_select_for_budget(&messages, Some(focus.clone()), budget);
            let (actual, work) = select_for_budget_with_work(&messages, Some(focus), Some(budget));

            assert_eq!(actual, expected, "selection differs at budget {budget}");
            assert_eq!(work.serialized_messages, messages.len());
            assert!(work.candidate_checks <= messages.len().saturating_mul(2));
        }
    }

    #[test]
    fn compact_read_layout_accounts_for_conversation_boundaries() {
        let first = resolved("first.jsonl");
        let second = resolved("second.jsonl");
        let first_transcript = transcript(
            (1..=8)
                .map(|ordinal| text_message(ordinal, AgentMessageRole::User, "alpha message"))
                .collect(),
        );
        let second_transcript = transcript(
            (1..=8)
                .map(|ordinal| text_message(ordinal, AgentMessageRole::User, "beta message"))
                .collect(),
        );
        let messages = first_transcript
            .messages
            .iter()
            .filter_map(|message| render_message(&first, &first_transcript, message, options()))
            .chain(second_transcript.messages.iter().filter_map(|message| {
                render_message(&second, &second_transcript, message, options())
            }))
            .collect::<Vec<_>>();

        for budget in (100..=2_000).step_by(29) {
            let expected = legacy_select_for_budget(&messages, None, budget);
            let (actual, work) = select_for_budget_with_work(&messages, None, Some(budget));

            assert_eq!(actual, expected, "selection differs at budget {budget}");
            assert_eq!(work.serialized_messages, messages.len());
            assert!(work.candidate_checks <= messages.len().saturating_mul(2));
        }
    }

    #[test]
    fn escapes_header_atom_delimiters() {
        assert_eq!(escape_atom("a b%c=d|e\tf\ng"), "a%20b%25c%3Dd%7Ce%09f%0Ag");
    }

    #[test]
    fn read_defaults_hide_non_text_parts_and_frame_body_lines() {
        let resolved = resolved("session file.jsonl");
        let mut message = text_message(1, AgentMessageRole::Assistant, "hello\nprotocol fake");
        message.parts.push(AgentMessagePart::ToolUse {
            id: "toolu_1".to_string(),
            name: "Bash".to_string(),
            input: json!({"command": "pwd"}),
            source: source(AgentMessageRole::Assistant),
        });
        message.parts.push(AgentMessagePart::Thinking {
            thinking: "secret".to_string(),
            source: source(AgentMessageRole::Assistant),
        });
        let transcript = transcript(vec![message]);
        let output = format_read(
            &[ReadRequest {
                resolved: &resolved,
                transcript: &transcript,
                range: None,
            }],
            None,
            None,
            options(),
        )
        .unwrap();

        assert!(
            output.starts_with("protocol agent-read cut=none chars=6000 policy=text omit=none\n")
        );
        assert!(output.contains("project=pr_"));
        assert!(!output.contains("path="));
        assert!(output.contains("| hello\n| protocol fake\n"));
        assert!(!output.contains("pwd"));
        assert!(!output.contains("secret"));
        assert!(!output.contains("\x1b["));
    }

    #[test]
    fn read_preserves_focus_when_budget_truncates() {
        let resolved = resolved("session.jsonl");
        let transcript = transcript(
            (1..=7)
                .map(|index| {
                    text_message(
                        index,
                        AgentMessageRole::User,
                        &format!("message {index} with padding padding padding"),
                    )
                })
                .collect(),
        );
        let output = format_read(
            &[ReadRequest {
                resolved: &resolved,
                transcript: &transcript,
                range: Some(MessageRange { start: 1, end: 7 }),
            }],
            Some(ProtocolFocus {
                conversation_full_ref: None,
                range: MessageRange::single(4),
            }),
            None,
            ProtocolOptions {
                budget: Some(800),
                ..options()
            },
        )
        .unwrap();

        assert!(output.starts_with("protocol agent-read cut="));
        assert!(output.lines().next().unwrap().contains("chars=800"));
        assert!(output.lines().next().unwrap().contains("omit="));
        assert!(output.contains("message m4 role=user"));
        assert!(output.contains("message 4 with padding"));
    }

    #[test]
    fn no_budget_read_emits_full_output_with_no_cut() {
        let resolved = resolved("session.jsonl");
        let transcript = transcript(vec![
            text_message(1, AgentMessageRole::User, "one"),
            text_message(2, AgentMessageRole::Assistant, "two"),
        ]);
        let output = format_read(
            &[ReadRequest {
                resolved: &resolved,
                transcript: &transcript,
                range: None,
            }],
            None,
            None,
            ProtocolOptions {
                budget: None,
                ..options()
            },
        )
        .unwrap();

        assert!(
            output.starts_with("protocol agent-read cut=none chars=none policy=text omit=none\n")
        );
        assert!(output.contains("message m1 role=user"));
        assert!(output.contains("message m2 role=assistant"));
    }

    #[test]
    fn line_slice_returns_numbered_content_lines() {
        let resolved = resolved("session.jsonl");
        let transcript = transcript(vec![text_message(
            1,
            AgentMessageRole::Assistant,
            "one\ntwo\nthree\nfour\nfive",
        )]);
        let output = format_read(
            &[ReadRequest {
                resolved: &resolved,
                transcript: &transcript,
                range: Some(MessageRange::single(1)),
            }],
            None,
            Some(&ReadSlice::Lines(MessageLineRange { start: 2, end: 4 })),
            options(),
        )
        .unwrap();

        assert!(output.contains("line=1 slice=lines=2..4 anchor=ma_"));
        assert!(output.contains("|  2: two\n|  3: three\n|  4: four\n"));
        assert!(!output.contains("|  1: one"));
        assert!(!output.contains("|  5: five"));
    }

    #[test]
    fn match_slice_returns_merged_numbered_context_windows() {
        let resolved = resolved("session.jsonl");
        let transcript = transcript(vec![text_message(
            1,
            AgentMessageRole::Assistant,
            "zero\nNeedle first\ntwo\nthree\nfour\nneedle second\nsix",
        )]);
        let output = format_read(
            &[ReadRequest {
                resolved: &resolved,
                transcript: &transcript,
                range: Some(MessageRange::single(1)),
            }],
            None,
            Some(&ReadSlice::Match {
                query: "needle".to_string(),
                context: 1,
            }),
            options(),
        )
        .unwrap();

        assert!(output.contains("slice=match=needle context=1 hits=2 anchor=ma_"));
        assert!(output.contains("|  1: zero\n| >2: Needle first\n|  3: two\n| ...\n"));
        assert!(output.contains("|  5: four\n| >6: needle second\n|  7: six\n"));
        assert!(!output.contains("4: three"));
    }

    #[test]
    fn match_slice_reports_zero_hits_without_dumping_message() {
        let resolved = resolved("session.jsonl");
        let transcript = transcript(vec![text_message(1, AgentMessageRole::User, "haystack")]);
        let output = format_read(
            &[ReadRequest {
                resolved: &resolved,
                transcript: &transcript,
                range: Some(MessageRange::single(1)),
            }],
            None,
            Some(&ReadSlice::Match {
                query: "needle".to_string(),
                context: 3,
            }),
            options(),
        )
        .unwrap();

        assert!(output.contains("slice=match=needle context=3 hits=0 anchor=ma_"));
        assert!(!output.contains("haystack"));
    }

    #[test]
    fn line_slice_rejects_ranges_past_message_end() {
        let resolved = resolved("session.jsonl");
        let transcript = transcript(vec![text_message(1, AgentMessageRole::User, "one\ntwo")]);
        let error = format_read(
            &[ReadRequest {
                resolved: &resolved,
                transcript: &transcript,
                range: Some(MessageRange::single(1)),
            }],
            None,
            Some(&ReadSlice::Lines(MessageLineRange { start: 2, end: 3 })),
            options(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("exceeds message m1 length 2"));
    }

    #[test]
    fn sliced_message_truncates_content_within_budget() {
        let resolved = resolved("session.jsonl");
        let body = (1..=20)
            .map(|line| format!("needle line {line} with padding padding"))
            .collect::<Vec<_>>()
            .join("\n");
        let transcript = transcript(vec![text_message(1, AgentMessageRole::User, &body)]);
        let output = format_read(
            &[ReadRequest {
                resolved: &resolved,
                transcript: &transcript,
                range: Some(MessageRange::single(1)),
            }],
            None,
            Some(&ReadSlice::Match {
                query: "needle".to_string(),
                context: 0,
            }),
            ProtocolOptions {
                budget: Some(400),
                ..options()
            },
        )
        .unwrap();

        assert!(
            output.starts_with("protocol agent-read cut=body chars=400 policy=text omit=none\n")
        );
        assert!(output.contains("| >1: needle line 1"));
        assert!(output.chars().count() <= 400);
    }

    #[test]
    fn non_focused_oversized_read_respects_budget_with_header_only_cut() {
        let resolved = resolved("session.jsonl");
        let transcript = transcript(vec![text_message(
            1,
            AgentMessageRole::User,
            &"x".repeat(500),
        )]);
        let output = format_read(
            &[ReadRequest {
                resolved: &resolved,
                transcript: &transcript,
                range: None,
            }],
            None,
            None,
            ProtocolOptions {
                budget: Some(80),
                ..options()
            },
        )
        .unwrap();

        assert!(output.starts_with("protocol agent-read cut="));
        assert!(output.chars().count() <= 80);
    }

    #[test]
    fn focused_oversized_message_is_truncated_inside_the_message() {
        let resolved = resolved("session.jsonl");
        let transcript = transcript(vec![
            text_message(1, AgentMessageRole::User, "before"),
            text_message(2, AgentMessageRole::Assistant, &"界".repeat(500)),
            text_message(3, AgentMessageRole::User, "after"),
        ]);
        let output = format_read(
            &[ReadRequest {
                resolved: &resolved,
                transcript: &transcript,
                range: None,
            }],
            Some(ProtocolFocus {
                conversation_full_ref: None,
                range: MessageRange::single(2),
            }),
            None,
            ProtocolOptions {
                budget: Some(500),
                ..options()
            },
        )
        .unwrap();

        assert!(output.chars().count() <= 500);
        assert!(output.contains("message m2 role=assistant"));
        assert!(output.contains("omitted chars="));
        assert!(output.contains("use --lines or --match"));
    }

    #[test]
    fn multiple_focused_messages_share_the_hard_budget() {
        let resolved = resolved("session.jsonl");
        let transcript = transcript(
            (1..=4)
                .map(|ordinal| {
                    text_message(
                        ordinal,
                        AgentMessageRole::Assistant,
                        &format!("focus {ordinal} {}", "x".repeat(300)),
                    )
                })
                .collect(),
        );
        let output = format_read(
            &[ReadRequest {
                resolved: &resolved,
                transcript: &transcript,
                range: None,
            }],
            Some(ProtocolFocus {
                conversation_full_ref: None,
                range: MessageRange { start: 2, end: 3 },
            }),
            None,
            ProtocolOptions {
                budget: Some(420),
                ..options()
            },
        )
        .unwrap();

        assert!(output.chars().count() <= 420);
        assert!(output.contains("message m2 role=assistant"));
        assert!(output.contains("message m3 role=assistant"));
        assert!(output.contains("omit=m1,m4"));
    }

    #[test]
    fn read_sanitizes_visible_text_tools_results_and_thinking() {
        let resolved = resolved("session.jsonl");
        let mut message = text_message(1, AgentMessageRole::Assistant, "safe\u{1b}[31mred");
        message.parts.extend([
            AgentMessagePart::ToolUse {
                id: "toolu_1".to_string(),
                name: "Ba\u{1b}]0;title\u{7}sh".to_string(),
                input: json!({"command": "pwd"}),
                source: source(AgentMessageRole::Assistant),
            },
            AgentMessagePart::ToolResult {
                tool_use_id: "toolu_1".to_string(),
                content: Some(json!("ok\u{1b}[2Jdone")),
                source: source(AgentMessageRole::User),
            },
            AgentMessagePart::Thinking {
                thinking: "think\u{0}ing".to_string(),
                source: source(AgentMessageRole::Assistant),
            },
        ]);
        let transcript = transcript(vec![message]);
        let output = format_read(
            &[ReadRequest {
                resolved: &resolved,
                transcript: &transcript,
                range: None,
            }],
            None,
            None,
            ProtocolOptions {
                visibility: ContentVisibility {
                    tools: true,
                    tool_results: true,
                    thinking: true,
                    subagents: false,
                },
                ..options()
            },
        )
        .unwrap();

        assert!(output.contains("safered"));
        assert!(output.contains("tool Bash"));
        assert!(output.contains("okdone"));
        assert!(output.contains("thinking: thinking"));
        assert!(!output.contains('\u{1b}'));
        assert!(!output.contains('\u{0}'));
    }

    #[test]
    fn qualified_focus_only_matches_target_conversation() {
        let first = resolved("first.jsonl");
        let second = resolved("second.jsonl");
        let first_transcript = transcript(vec![
            text_message(1, AgentMessageRole::User, "first one padding padding"),
            text_message(2, AgentMessageRole::User, "first two padding padding"),
        ]);
        let second_transcript = transcript(vec![
            text_message(1, AgentMessageRole::User, "second one padding padding"),
            text_message(2, AgentMessageRole::User, "second two padding padding"),
        ]);
        let output = format_read(
            &[
                ReadRequest {
                    resolved: &first,
                    transcript: &first_transcript,
                    range: None,
                },
                ReadRequest {
                    resolved: &second,
                    transcript: &second_transcript,
                    range: None,
                },
            ],
            Some(ProtocolFocus {
                conversation_full_ref: Some(second.reference.full_ref()),
                range: MessageRange::single(2),
            }),
            None,
            ProtocolOptions {
                budget: Some(500),
                ..options()
            },
        )
        .unwrap();

        assert!(output.contains("second two padding padding"));
        assert!(!output.contains("first two padding padding"));
    }

    #[test]
    fn snippet_truncates_to_eighty_characters() {
        assert_eq!(snippet(&"a".repeat(100)).chars().count(), SNIPPET_LIMIT);
    }

    #[test]
    fn subagent_messages_are_hidden_by_default_and_visible_with_option() {
        let resolved = resolved("session.jsonl");
        let mut subagent = text_message(2, AgentMessageRole::Assistant, "subagent hidden text");
        subagent.parent_tool_use_id = Some("agent-abcdef".to_string());
        let transcript = transcript(vec![
            text_message(1, AgentMessageRole::User, "question"),
            subagent,
            text_message(3, AgentMessageRole::Assistant, "answer"),
        ]);

        let hidden = format_read(
            &[ReadRequest {
                resolved: &resolved,
                transcript: &transcript,
                range: None,
            }],
            None,
            None,
            options(),
        )
        .unwrap();
        let visible = format_read(
            &[ReadRequest {
                resolved: &resolved,
                transcript: &transcript,
                range: None,
            }],
            None,
            None,
            ProtocolOptions {
                visibility: ContentVisibility {
                    subagents: true,
                    ..ContentVisibility::default()
                },
                ..options()
            },
        )
        .unwrap();

        assert!(!hidden.contains("subagent hidden text"));
        assert!(visible.contains("message m2 role=assistant"));
        assert!(visible.contains("subagent hidden text"));
    }

    #[test]
    fn short_outline_emits_one_line_per_message() {
        let resolved = resolved("session.jsonl");
        let transcript = transcript(vec![
            text_message(1, AgentMessageRole::User, "one"),
            text_message(2, AgentMessageRole::Assistant, "two"),
        ]);
        let output = format_outline(&resolved, &transcript, options());

        assert!(output.contains("m1 role=user chars=3 anchor=ma_"));
        assert!(output.contains(" | one"));
        assert!(output.contains("m2 role=assistant chars=3 anchor=ma_"));
        assert!(output.contains(" | two"));
        assert!(!output.contains("seg "));
    }

    #[test]
    fn long_outline_emits_deterministic_segments() {
        let resolved = resolved("session.jsonl");
        let transcript = transcript(
            (1..=21)
                .map(|index| {
                    text_message(index, AgentMessageRole::User, &format!("message {index}"))
                })
                .collect(),
        );
        let output = format_outline(&resolved, &transcript, options());

        assert!(output.contains("seg m1..m10 chars=91 anchors=ma_"));
        assert!(output.contains(" | message 1 / message 10"));
        assert!(output.contains("seg m11..m20 chars=100 anchors=ma_"));
        assert!(output.contains(" | message 11 / message 20"));
        assert!(output.contains("seg m21..m21 chars=10 anchors=ma_"));
        assert!(output.contains(" | message 21 / message 21"));
        assert!(!output.contains("summary"));
    }
}
