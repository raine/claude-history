use crate::agent::refs::MessageRange;
use crate::agent::sanitize::sanitize_agent_text;
use crate::agent::transcript::{
    AgentMessage, AgentMessagePart, AgentTranscript, agent_part_search_text, truncate_chars,
};
use crate::agent::visibility::ContentVisibility;
use crate::search::literal::Literal;
use crate::search::query::ParsedQuery;
use crate::text_match::{contains_cjk, contains_prefix_match, normalize_for_search};
use chrono::{DateTime, Local};
use std::cmp::Ordering;

const MAX_PREVIEW_CHARS: usize = 160;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentHitSource {
    Dialogue,
    Tool,
    Thinking,
    /// Text a producer attached to the conversation. Named separately so a hit
    /// on an annotation does not read as something said in the conversation.
    Annotation,
}

pub type AgentHitRenderOptions = ContentVisibility;

#[derive(Clone, Debug, PartialEq)]
pub struct AgentSearchHit {
    pub conversation_ref: Option<String>,
    pub score: f64,
    pub source: AgentHitSource,
    pub preview: String,
    pub focus_range: MessageRange,
    pub read_range: MessageRange,
    pub render_options: AgentHitRenderOptions,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AgentRetrievalOptions {
    pub read_context_radius: usize,
    pub limit: usize,
}

impl Default for AgentRetrievalOptions {
    fn default() -> Self {
        Self {
            read_context_radius: 1,
            limit: 10,
        }
    }
}

#[derive(Clone, Copy)]
pub struct AgentTranscriptSearchTarget<'a> {
    pub transcript: &'a AgentTranscript,
    pub conversation_ref: Option<&'a str>,
    pub timestamp: Option<DateTime<Local>>,
}

#[derive(Clone, Debug)]
struct Segment {
    message_ordinal: usize,
    source: AgentHitSource,
    render_options: AgentHitRenderOptions,
    text: String,
    normalized: String,
}

#[derive(Clone, Copy, Debug)]
struct MatchRange {
    start: usize,
    end: usize,
    term_index: usize,
    source: AgentHitSource,
    render_options: AgentHitRenderOptions,
}

#[derive(Clone, Debug)]
struct Candidate {
    hit: AgentSearchHit,
    timestamp: Option<DateTime<Local>>,
    message_ordinal: usize,
    first_offset: usize,
}

#[cfg(test)]
pub fn retrieve_agent_hits(
    transcript: &AgentTranscript,
    query: &str,
    options: AgentRetrievalOptions,
) -> Vec<AgentSearchHit> {
    retrieve_agent_hits_for_target(
        AgentTranscriptSearchTarget {
            transcript,
            conversation_ref: None,
            timestamp: None,
        },
        query,
        options,
    )
}

pub fn retrieve_agent_hits_for_target(
    target: AgentTranscriptSearchTarget<'_>,
    query: &str,
    options: AgentRetrievalOptions,
) -> Vec<AgentSearchHit> {
    retrieve_agent_hit_candidates(target, query, options)
        .into_iter()
        .map(|candidate| candidate.hit)
        .collect()
}

#[cfg(test)]
pub fn retrieve_agent_hits_for_targets(
    targets: &[AgentTranscriptSearchTarget<'_>],
    query: &str,
    options: AgentRetrievalOptions,
) -> Vec<AgentSearchHit> {
    let mut candidates = targets
        .iter()
        .flat_map(|target| {
            retrieve_agent_hit_candidates(
                AgentTranscriptSearchTarget {
                    transcript: target.transcript,
                    conversation_ref: target.conversation_ref,
                    timestamp: target.timestamp,
                },
                query,
                options,
            )
        })
        .collect::<Vec<_>>();
    let prefer_dialogue = ParsedQuery::parse(query).is_quoted_only();
    sort_candidates(&mut candidates, prefer_dialogue);
    candidates.truncate(options.limit);
    candidates
        .into_iter()
        .map(|candidate| candidate.hit)
        .collect()
}

fn retrieve_agent_hit_candidates(
    target: AgentTranscriptSearchTarget<'_>,
    query: &str,
    options: AgentRetrievalOptions,
) -> Vec<Candidate> {
    if options.limit == 0 || target.transcript.messages.is_empty() {
        return Vec::new();
    }

    let parsed = ParsedQuery::parse(query);
    if parsed.is_effectively_empty() {
        return Vec::new();
    }

    let segments = build_segments(target.transcript);
    if segments.is_empty() {
        return Vec::new();
    }

    let mut candidates = if parsed.is_quoted_only() {
        exact_candidates(&segments, target, &parsed, options)
    } else {
        lexical_candidates(&segments, target, &parsed, options)
    };
    sort_candidates(&mut candidates, parsed.is_quoted_only());
    candidates.truncate(options.limit);
    candidates
}

fn lexical_candidates(
    segments: &[Segment],
    target: AgentTranscriptSearchTarget<'_>,
    parsed: &ParsedQuery,
    options: AgentRetrievalOptions,
) -> Vec<Candidate> {
    let unquoted_terms = unquoted_terms(parsed.unquoted());
    let normalized_query = normalize_for_search(
        &unquoted_terms
            .iter()
            .copied()
            .filter(|term| !term.contains('_'))
            .collect::<Vec<_>>()
            .join(" "),
    );
    let query_words = normalized_query
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let identifier_literals = unquoted_terms
        .iter()
        .copied()
        .filter(|term| term.contains('_'))
        .map(|term| Literal::new(term.to_string()));
    let literal_filters = parsed
        .literals()
        .iter()
        .cloned()
        .chain(identifier_literals)
        .collect::<Vec<_>>();

    if query_words.is_empty() && literal_filters.is_empty() {
        return Vec::new();
    }
    if query_words.is_empty() {
        return exact_candidates_for_literals(segments, target, &literal_filters, options);
    }

    segments_by_message(segments)
        .into_iter()
        .filter_map(|message_segments| {
            let body = join_segment_text(&message_segments);
            let normalized = normalize_for_search(&body);
            if !message_matches_words(&normalized, &query_words) {
                return None;
            }
            if !literal_filters.iter().all(|literal| literal.matches(&body)) {
                return None;
            }
            let ranges = collect_match_ranges(&message_segments, &query_words, &literal_filters);
            if ranges.is_empty() {
                return None;
            }
            Some(candidate_from_ranges(CandidateInput {
                target,
                message_ordinal: message_segments[0].message_ordinal,
                message_count: target.transcript.messages.len(),
                options,
                body: &body,
                query_words: &query_words,
                literals: &literal_filters,
                ranges,
            }))
        })
        .collect()
}

fn exact_candidates(
    segments: &[Segment],
    target: AgentTranscriptSearchTarget<'_>,
    parsed: &ParsedQuery,
    options: AgentRetrievalOptions,
) -> Vec<Candidate> {
    exact_candidates_for_literals(segments, target, parsed.literals(), options)
}

fn exact_candidates_for_literals(
    segments: &[Segment],
    target: AgentTranscriptSearchTarget<'_>,
    literals: &[Literal],
    options: AgentRetrievalOptions,
) -> Vec<Candidate> {
    if literals.is_empty() {
        return Vec::new();
    }

    segments_by_message(segments)
        .into_iter()
        .filter_map(|message_segments| {
            let body = join_segment_text(&message_segments);
            if !literals.iter().all(|literal| literal.matches(&body)) {
                return None;
            }
            let ranges = collect_match_ranges(&message_segments, &[], literals);
            if ranges.is_empty() {
                return None;
            }
            Some(candidate_from_ranges(CandidateInput {
                target,
                message_ordinal: message_segments[0].message_ordinal,
                message_count: target.transcript.messages.len(),
                options,
                body: &body,
                query_words: &[],
                literals,
                ranges,
            }))
        })
        .collect()
}

struct CandidateInput<'a> {
    target: AgentTranscriptSearchTarget<'a>,
    message_ordinal: usize,
    message_count: usize,
    options: AgentRetrievalOptions,
    body: &'a str,
    query_words: &'a [String],
    literals: &'a [Literal],
    ranges: Vec<MatchRange>,
}

fn candidate_from_ranges(input: CandidateInput<'_>) -> Candidate {
    let first_start = input
        .ranges
        .iter()
        .map(|range| range.start)
        .min()
        .unwrap_or(0);
    let last_end = input
        .ranges
        .iter()
        .map(|range| range.end)
        .max()
        .unwrap_or(first_start);
    let mut render_options = AgentHitRenderOptions::default();
    for range in &input.ranges {
        render_options.merge(range.render_options);
    }
    let source = input
        .ranges
        .iter()
        .min_by_key(|range| source_rank(range.source))
        .map(|range| range.source)
        .unwrap_or(AgentHitSource::Dialogue);
    let focus_range = MessageRange::single(input.message_ordinal);
    let read_range = read_range_for_focus(
        focus_range,
        input.message_count,
        input.options.read_context_radius,
    );
    let unique_terms = input
        .ranges
        .iter()
        .map(|range| range.term_index)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let adjacency = adjacency_score(input.body, input.query_words);
    let span = last_end.saturating_sub(first_start).max(1);
    let score = (unique_terms as f64 * 4.0)
        + (input.literals.len() as f64 * 3.0)
        + (adjacency as f64 * 2.0)
        + (1.0 / span as f64);

    Candidate {
        hit: AgentSearchHit {
            conversation_ref: input.target.conversation_ref.map(str::to_string),
            score,
            source,
            preview: preview(input.body, first_start, last_end),
            focus_range,
            read_range,
            render_options,
        },
        timestamp: input.target.timestamp,
        message_ordinal: input.message_ordinal,
        first_offset: first_start,
    }
}

fn build_segments(transcript: &AgentTranscript) -> Vec<Segment> {
    transcript
        .messages
        .iter()
        .flat_map(message_segments)
        .collect()
}

fn message_segments(message: &AgentMessage) -> Vec<Segment> {
    let visibility = ContentVisibility::SEARCH;
    if !visibility.message_is_visible(message) {
        return Vec::new();
    }
    message
        .parts
        .iter()
        .filter(|part| visibility.part_is_visible(part))
        .filter_map(|part| segment_for_part(message, part))
        .collect()
}

fn segment_for_part(message: &AgentMessage, part: &AgentMessagePart) -> Option<Segment> {
    let (source, render_options) = match part {
        AgentMessagePart::Text { .. } => (
            AgentHitSource::Dialogue,
            AgentHitRenderOptions {
                subagents: message.parent_tool_use_id.is_some(),
                ..AgentHitRenderOptions::default()
            },
        ),
        AgentMessagePart::ToolUse { .. } => (
            AgentHitSource::Tool,
            AgentHitRenderOptions {
                tools: true,
                subagents: message.parent_tool_use_id.is_some(),
                ..AgentHitRenderOptions::default()
            },
        ),
        AgentMessagePart::ToolResult { .. } => (
            AgentHitSource::Tool,
            AgentHitRenderOptions {
                tool_results: true,
                subagents: message.parent_tool_use_id.is_some(),
                ..AgentHitRenderOptions::default()
            },
        ),
        AgentMessagePart::Thinking { .. } => (
            AgentHitSource::Thinking,
            AgentHitRenderOptions {
                thinking: true,
                subagents: message.parent_tool_use_id.is_some(),
                ..AgentHitRenderOptions::default()
            },
        ),
    };
    let text = agent_part_search_text(part)?;
    Some(Segment {
        message_ordinal: message.ordinal,
        source,
        render_options,
        normalized: normalize_for_search(&text),
        text,
    })
}

fn segments_by_message(segments: &[Segment]) -> Vec<Vec<&Segment>> {
    let mut groups = Vec::new();
    for segment in segments {
        if groups
            .last()
            .and_then(|group: &Vec<&Segment>| group.first().copied())
            .is_some_and(|first| first.message_ordinal == segment.message_ordinal)
        {
            groups
                .last_mut()
                .expect("group exists after last check")
                .push(segment);
        } else {
            groups.push(vec![segment]);
        }
    }
    groups
}

fn join_segment_text(segments: &[&Segment]) -> String {
    segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn unquoted_terms(unquoted: &str) -> Vec<&str> {
    unquoted.split_whitespace().collect()
}

fn message_matches_words(normalized: &str, words: &[String]) -> bool {
    words.iter().all(|word| {
        contains_prefix_match(normalized, word) || (contains_cjk(word) && normalized.contains(word))
    })
}

fn collect_match_ranges(
    segments: &[&Segment],
    query_words: &[String],
    literals: &[Literal],
) -> Vec<MatchRange> {
    let mut ranges = Vec::new();
    let mut body_offset = 0usize;
    for segment in segments {
        for (term_index, word) in query_words.iter().enumerate() {
            if normalized_word_matches(&segment.normalized, word) {
                ranges.push(MatchRange {
                    start: body_offset,
                    end: body_offset + segment.text.len(),
                    term_index,
                    source: segment.source,
                    render_options: segment.render_options,
                });
            }
        }
        let literal_offset = query_words.len();
        for (literal_index, literal) in literals.iter().enumerate() {
            for (start, end) in literal.match_ranges(&segment.text) {
                ranges.push(MatchRange {
                    start: body_offset + start,
                    end: body_offset + end,
                    term_index: literal_offset + literal_index,
                    source: segment.source,
                    render_options: segment.render_options,
                });
            }
        }
        body_offset += segment.text.len() + 1;
    }
    ranges.sort_unstable_by_key(|range| (range.start, range.end, range.term_index));
    ranges
}

fn normalized_word_matches(normalized_text: &str, word: &str) -> bool {
    contains_prefix_match(normalized_text, word)
        || (contains_cjk(word) && normalized_text.contains(word))
}

fn adjacency_score(body: &str, query_words: &[String]) -> usize {
    if query_words.len() < 2 {
        return 0;
    }
    let normalized = normalize_for_search(body);
    query_words
        .windows(2)
        .filter(|pair| normalized.contains(&format!("{} {}", pair[0], pair[1])))
        .count()
}

pub(crate) fn read_range_for_focus(
    focus: MessageRange,
    message_count: usize,
    radius: usize,
) -> MessageRange {
    MessageRange {
        start: focus.start.saturating_sub(radius).max(1),
        end: focus.end.saturating_add(radius).min(message_count),
    }
}

fn preview(body: &str, start: usize, end: usize) -> String {
    let safe_start = floor_char_boundary(body, start);
    let safe_end = ceil_char_boundary(body, end).max(safe_start);
    let prefix_start = body[..safe_start]
        .char_indices()
        .rev()
        .nth(40)
        .map(|(idx, _)| idx)
        .unwrap_or(0);
    let suffix_end = body[safe_end..]
        .char_indices()
        .nth(80)
        .map(|(idx, _)| safe_end + idx)
        .unwrap_or(body.len());
    format_evidence_preview(&body[prefix_start..suffix_end])
}

pub(crate) fn format_evidence_preview(text: &str) -> String {
    let sanitized = sanitize_agent_text(text);
    let normalized = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&normalized, MAX_PREVIEW_CHARS)
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn sort_candidates(candidates: &mut [Candidate], prefer_dialogue: bool) {
    candidates.sort_by(|a, b| {
        let source_order = if prefer_dialogue {
            source_rank(a.hit.source).cmp(&source_rank(b.hit.source))
        } else {
            Ordering::Equal
        };
        source_order
            .then_with(|| {
                b.hit
                    .score
                    .partial_cmp(&a.hit.score)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| b.timestamp.cmp(&a.timestamp))
            .then_with(|| a.hit.conversation_ref.cmp(&b.hit.conversation_ref))
            .then_with(|| a.message_ordinal.cmp(&b.message_ordinal))
            .then_with(|| source_rank(a.hit.source).cmp(&source_rank(b.hit.source)))
            .then_with(|| a.first_offset.cmp(&b.first_offset))
    });
}

fn source_rank(source: AgentHitSource) -> u8 {
    match source {
        AgentHitSource::Dialogue => 0,
        AgentHitSource::Tool => 1,
        AgentHitSource::Thinking => 2,
        AgentHitSource::Annotation => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::test_support::{source, text_message};
    use crate::agent::transcript::{AgentMessageRole, AgentTranscript};
    use chrono::{Duration, TimeZone};
    use serde_json::{Value, json};

    fn tool_result_message(ordinal: usize, content: Value) -> AgentMessage {
        AgentMessage {
            ordinal,
            role: AgentMessageRole::User,
            timestamp: None,
            jsonl_line: ordinal,
            assistant_message_id: None,
            parent_tool_use_id: None,
            parts: vec![AgentMessagePart::ToolResult {
                tool_use_id: "toolu_1".to_string(),
                content: Some(content),
                source: source(AgentMessageRole::User),
            }],
        }
    }

    fn transcript(messages: Vec<AgentMessage>) -> AgentTranscript {
        crate::agent::test_support::transcript(messages, "session.jsonl")
    }

    fn options() -> AgentRetrievalOptions {
        AgentRetrievalOptions::default()
    }

    #[test]
    fn evidence_preview_formatting_is_unicode_bounded_and_sanitized() {
        let text = format!("  alpha\n\u{1b}[31m{} tail  ", "🙂 ".repeat(200));

        let preview = format_evidence_preview(&text);

        assert_eq!(preview.chars().count(), MAX_PREVIEW_CHARS);
        assert!(preview.starts_with("alpha 🙂"));
        assert!(!preview.contains('\u{1b}'));
        assert!(!preview.contains('\n'));
    }

    #[test]
    fn exact_tool_result_query_returns_tool_hit_with_readable_focus() {
        let transcript = transcript(vec![
            text_message(1, AgentMessageRole::User, "question"),
            tool_result_message(2, json!("hidden_exact_tool_needle")),
        ]);

        let hits = retrieve_agent_hits(&transcript, "\"hidden_exact_tool_needle\"", options());

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source, AgentHitSource::Tool);
        assert!(hits[0].render_options.tool_results);
        assert_eq!(hits[0].focus_range, MessageRange::single(2));
        assert!(hits[0].read_range.contains(&hits[0].focus_range));
        assert!(hits[0].preview.contains("hidden_exact_tool_needle"));
    }

    #[test]
    fn lexical_terms_cluster_to_tight_message_focus() {
        let transcript = transcript(vec![
            text_message(1, AgentMessageRole::User, "alpha only"),
            text_message(2, AgentMessageRole::Assistant, "near alpha beta evidence"),
            text_message(3, AgentMessageRole::User, "beta only"),
        ]);

        let hits = retrieve_agent_hits(&transcript, "alpha beta", options());

        assert_eq!(hits[0].focus_range, MessageRange::single(2));
        assert!(hits[0].read_range.contains(&MessageRange::single(2)));
        assert_ne!(hits[0].focus_range, MessageRange { start: 1, end: 3 });
    }

    #[test]
    fn mixed_query_requires_unquoted_terms_and_literals_in_same_message() {
        let transcript = transcript(vec![
            text_message(1, AgentMessageRole::User, "alpha beta beta beta"),
            text_message(
                2,
                AgentMessageRole::Assistant,
                "alpha beta with exact_literal nearby",
            ),
        ]);

        let hits = retrieve_agent_hits(&transcript, "alpha beta \"exact_literal\"", options());

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].focus_range, MessageRange::single(2));
        assert!(hits[0].preview.contains("exact_literal"));
    }

    #[test]
    fn split_literal_and_lexical_terms_do_not_match_across_messages() {
        let transcript = transcript(vec![
            text_message(1, AgentMessageRole::User, "alpha beta"),
            text_message(2, AgentMessageRole::Assistant, "exact_literal"),
        ]);

        let hits = retrieve_agent_hits(&transcript, "alpha beta \"exact_literal\"", options());

        assert!(hits.is_empty());
    }

    #[test]
    fn empty_results_are_deterministic() {
        let transcript = transcript(vec![text_message(1, AgentMessageRole::User, "alpha")]);

        let first = retrieve_agent_hits(&transcript, "missing", options());
        let second = retrieve_agent_hits(&transcript, "missing", options());

        assert!(first.is_empty());
        assert_eq!(first, second);
    }

    #[test]
    fn exact_hits_across_targets_are_newest_first_with_tie_breakers() {
        let older = transcript(vec![text_message(1, AgentMessageRole::User, "needle")]);
        let newer = transcript(vec![text_message(1, AgentMessageRole::User, "needle")]);
        let tied_a = transcript(vec![text_message(1, AgentMessageRole::User, "needle")]);
        let tied_b = transcript(vec![text_message(1, AgentMessageRole::User, "needle")]);
        let now = Local.with_ymd_and_hms(2024, 1, 2, 0, 0, 0).unwrap();
        let tied_time = now - Duration::hours(2);

        let hits = retrieve_agent_hits_for_targets(
            &[
                AgentTranscriptSearchTarget {
                    transcript: &older,
                    conversation_ref: Some("ch_old"),
                    timestamp: Some(now - Duration::days(1)),
                },
                AgentTranscriptSearchTarget {
                    transcript: &newer,
                    conversation_ref: Some("ch_new"),
                    timestamp: Some(now),
                },
                AgentTranscriptSearchTarget {
                    transcript: &tied_b,
                    conversation_ref: Some("ch_b"),
                    timestamp: Some(tied_time),
                },
                AgentTranscriptSearchTarget {
                    transcript: &tied_a,
                    conversation_ref: Some("ch_a"),
                    timestamp: Some(tied_time),
                },
            ],
            "\"needle\"",
            AgentRetrievalOptions {
                limit: 4,
                ..options()
            },
        );

        assert_eq!(
            hits.iter()
                .map(|hit| hit.conversation_ref.as_deref().unwrap())
                .collect::<Vec<_>>(),
            vec!["ch_new", "ch_a", "ch_b", "ch_old"]
        );
    }

    #[test]
    fn direct_exact_matches_rank_above_newer_tool_results() {
        let direct = transcript(vec![text_message(
            1,
            AgentMessageRole::Assistant,
            "protocol versioning",
        )]);
        let tool = transcript(vec![tool_result_message(1, json!("protocol versioning"))]);
        let now = Local.with_ymd_and_hms(2024, 1, 2, 0, 0, 0).unwrap();

        let hits = retrieve_agent_hits_for_targets(
            &[
                AgentTranscriptSearchTarget {
                    transcript: &tool,
                    conversation_ref: Some("ch_tool"),
                    timestamp: Some(now),
                },
                AgentTranscriptSearchTarget {
                    transcript: &direct,
                    conversation_ref: Some("ch_direct"),
                    timestamp: Some(now - Duration::days(1)),
                },
            ],
            "\"protocol versioning\"",
            AgentRetrievalOptions {
                limit: 2,
                ..options()
            },
        );

        assert_eq!(hits[0].conversation_ref.as_deref(), Some("ch_direct"));
        assert_eq!(hits[0].source, AgentHitSource::Dialogue);
        assert_eq!(hits[1].conversation_ref.as_deref(), Some("ch_tool"));
        assert_eq!(hits[1].source, AgentHitSource::Tool);
    }

    #[test]
    fn exact_tool_result_search_uses_bounded_head_and_tail() {
        let long = format!(
            "HEAD{}TAIL",
            "x".repeat(crate::agent::transcript::MAX_AGENT_SEGMENT_CHARS * 2)
        );
        let transcript = transcript(vec![tool_result_message(1, json!(long))]);

        let head = retrieve_agent_hits(&transcript, "\"HEAD\"", options());
        let tail = retrieve_agent_hits(&transcript, "\"TAIL\"", options());
        let middle = retrieve_agent_hits(
            &transcript,
            &format!(
                "\"{}\"",
                "x".repeat(crate::agent::transcript::MAX_AGENT_SEGMENT_CHARS + 1)
            ),
            options(),
        );

        assert_eq!(head.len(), 1);
        assert_eq!(tail.len(), 1);
        assert!(middle.is_empty());
    }

    #[test]
    fn read_ranges_are_clamped_at_transcript_boundaries() {
        let transcript = transcript(vec![
            text_message(1, AgentMessageRole::User, "first needle"),
            text_message(2, AgentMessageRole::Assistant, "middle"),
            text_message(3, AgentMessageRole::User, "last target"),
        ]);

        let first = retrieve_agent_hits(&transcript, "needle", options());
        let last = retrieve_agent_hits(&transcript, "target", options());

        assert_eq!(first[0].read_range, MessageRange { start: 1, end: 2 });
        assert_eq!(last[0].read_range, MessageRange { start: 2, end: 3 });
    }
}
