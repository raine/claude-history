use crate::agent::diagnostic::{AgentWarning, format_warning_records};
use crate::agent::records::{Cut, Response};
use crate::agent::refs::{AgentConversationKey, ResolvedConversation};
use crate::agent::retrieval::{
    AgentHitRenderOptions, AgentHitSource, AgentRetrievalOptions, AgentSearchHit as RetrievalHit,
    AgentTranscriptSearchTarget, format_evidence_preview, read_range_for_focus,
    retrieve_agent_hits_for_target,
};
use crate::agent::sanitize::sanitize_agent_text;
use crate::agent::transcript::AgentTranscript;
use crate::error::{AppError, Result};
use crate::history::Conversation;
use crate::history::MessageRange;
use crate::search::mode::SearchMode;
use crate::search::query::ParsedQuery;
use crate::semantic::types::{SemanticChunkSource, SemanticHit, SemanticScoreBreakdown};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::HashMap;

const SHORTLIST_MIN: usize = 50;
const SHORTLIST_FACTOR: usize = 5;
const SHORTLIST_MAX: usize = 500;
const MODALITY_CANDIDATE_MIN: usize = 50;
const MODALITY_CANDIDATE_FACTOR: usize = 8;
const MODALITY_CANDIDATE_MAX: usize = 1_000;
const RRF_K: f64 = 60.0;
const AGENT_SEARCH_TITLE_CHARS: usize = 240;
const AGENT_SEARCH_HIT_CHARS: usize = 500;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentSearchScope {
    Global,
    Local,
}

/// A global search with its settings already resolved: `mode` is the
/// [`effective_agent_mode`] for `query`, so nothing below re-derives either.
#[derive(Clone, Debug)]
pub struct AgentSearchRequest {
    pub query: ParsedQuery,
    pub mode: SearchMode,
    pub top: usize,
    pub flat: bool,
    pub hits_per_conversation: usize,
    pub retrieval_hits_per_conversation: Option<usize>,
    pub all_hits: bool,
    pub budget: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct AgentWithinRequest {
    pub query: ParsedQuery,
    pub mode: SearchMode,
    pub top: usize,
    pub budget: Option<usize>,
}

#[derive(Clone)]
pub struct AgentConversationInput<'a> {
    pub conversation: &'a Conversation,
    pub resolved: ResolvedConversation,
    pub original_index: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AgentSearchStats {
    pub shortlisted: usize,
    pub transcripts_loaded: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentSearchOutput {
    pub protocol: AgentProtocolKind,
    pub target: Option<AgentConversationMetadata>,
    pub query: String,
    pub mode: SearchMode,
    pub hits: Vec<AgentOutputHit>,
    pub groups: Vec<AgentConversationGroup>,
    pub flat: bool,
    pub budget: Option<usize>,
    pub stats: AgentSearchStats,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentConversationMetadata {
    pub project_id: String,
    pub conversation_uuid: String,
    pub conversation_ref: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentConversationGroup {
    pub conversation_ref: String,
    pub project_id: String,
    pub conversation_uuid: String,
    pub session: String,
    pub title: String,
    pub score: f64,
    pub total_hits: usize,
    pub hits: Vec<AgentOutputHit>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentProtocolKind {
    Search,
    Within,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentOutputHit {
    pub conversation_ref: String,
    pub project_id: String,
    pub conversation_uuid: String,
    pub session: String,
    pub anchors: Vec<String>,
    pub title: String,
    pub score: f64,
    pub evidence_score: f64,
    pub semantic_score_breakdown: Option<SemanticScoreBreakdown>,
    pub source: AgentHitKind,
    pub evidence_source: AgentHitSource,
    pub render_options: AgentHitRenderOptions,
    pub preview: String,
    pub focus_range: MessageRange,
    pub read_range: MessageRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentHitKind {
    Exact,
    Lexical,
    Semantic,
    Hybrid,
}

#[derive(Clone, Debug)]
struct RankedHit {
    hit: AgentOutputHit,
    lexical_rank: Option<usize>,
    semantic_rank: Option<usize>,
    exact: bool,
}

pub fn attach_transcript_metadata(
    output: &mut AgentSearchOutput,
    resolved: &ResolvedConversation,
    transcript: &AgentTranscript,
) {
    let reference = resolved.reference.canonical();
    if output.protocol == AgentProtocolKind::Within {
        output.target = Some(AgentConversationMetadata {
            project_id: resolved.key.project_id(),
            conversation_uuid: resolved.reference.uuid(),
            conversation_ref: reference.clone(),
        });
    }
    for hit in output
        .hits
        .iter_mut()
        .chain(
            output
                .groups
                .iter_mut()
                .flat_map(|group| group.hits.iter_mut()),
        )
        .filter(|hit| hit.conversation_ref == reference)
    {
        hit.project_id = resolved.key.project_id();
        hit.conversation_uuid = resolved.reference.uuid();
        hit.session = resolved.key.session_filename.clone();
        hit.anchors = anchors_for_range(transcript, resolved, hit.focus_range);
    }
    for group in output
        .groups
        .iter_mut()
        .filter(|group| group.conversation_ref == reference)
    {
        group.project_id = resolved.key.project_id();
        group.conversation_uuid = resolved.reference.uuid();
        group.session = resolved.key.session_filename.clone();
    }
}

/// The mode a query actually runs in: a quoted-only query is always exact,
/// otherwise the mode resolved from CLI and config applies.
pub fn effective_agent_mode(query: &ParsedQuery, resolved: SearchMode) -> SearchMode {
    if query.is_quoted_only() {
        SearchMode::Exact
    } else {
        resolved
    }
}

#[cfg(test)]
pub fn format_agent_output(output: &AgentSearchOutput) -> String {
    format_agent_output_with_warnings(output, &[])
}

pub fn format_agent_output_with_warnings(
    output: &AgentSearchOutput,
    warnings: &[AgentWarning],
) -> String {
    let protocol = match output.protocol {
        AgentProtocolKind::Search => "agent-search",
        AgentProtocolKind::Within => "agent-within",
    };
    let hits = output_hits(output);
    let (warning_count, warning_records) = format_warning_records(warnings);
    let warning_suffix = if warning_records.is_empty() {
        String::new()
    } else {
        format!(
            " warnings={warning_count} warning-records={}",
            warning_records.len()
        )
    };
    let grouped = output.protocol == AgentProtocolKind::Search && !output.flat;
    let groups_atom = if grouped {
        format!(" groups={}", output.groups.len())
    } else {
        String::new()
    };
    let header = |cut: Option<&Cut>| {
        let (cut_atom, omitted) = match cut {
            None => ("none", String::new()),
            Some(cut) => ("tail", format!(" omitted-lines={}", cut.omitted_lines)),
        };
        format!(
            "protocol {protocol} mode={} cut={cut_atom} chars={} policy=per-hit{groups_atom} hits={}{warning_suffix}{omitted}\n",
            mode_atom(output.mode),
            budget_atom(output.budget),
            hits.len(),
        )
    };
    let recovery = if let Some(target) = &output.target {
        format!(
            "continue within ref={} action=narrow-query-or-increase-budget\n",
            crate::agent::protocol::escape_atom(&target.conversation_ref)
        )
    } else {
        "continue search action=narrow-scope-or-increase-budget\n".to_string()
    };
    let cut_footer = |_: &Cut| recovery.clone();
    let mut units = Vec::new();
    units.push(format!(
        "query text={} hits={}\n",
        crate::agent::protocol::escape_atom(&output.query),
        hits.len()
    ));
    if let Some(target) = &output.target {
        units.push(format!(
            "conversation project={} uuid={} ref={}\n",
            crate::agent::protocol::escape_atom(&target.project_id),
            crate::agent::protocol::escape_atom(&target.conversation_uuid),
            crate::agent::protocol::escape_atom(&target.conversation_ref)
        ));
    }
    if grouped {
        units.push(format!("groups count={}\n", output.groups.len()));
        for (index, group) in output.groups.iter().enumerate() {
            units.push(format!(
                "conversation rank={} project={} uuid={} ref={} score={:.6}{} hits={} total={} | {}\n",
                index + 1,
                crate::agent::protocol::escape_atom(&group.project_id),
                crate::agent::protocol::escape_atom(&group.conversation_uuid),
                crate::agent::protocol::escape_atom(&group.conversation_ref),
                group.score,
                score_breakdown_atoms(group.hits.first().and_then(|hit| hit.semantic_score_breakdown)),
                group.hits.len(),
                group.total_hits,
                protocol_snippet(&group.title, AGENT_SEARCH_TITLE_CHARS)
            ));
            units.extend(group.hits.iter().map(hit_unit));
        }
    } else {
        for hit in &hits {
            units.push(format!(
                "title project={} uuid={} ref={} | {}\n",
                crate::agent::protocol::escape_atom(&hit.project_id),
                crate::agent::protocol::escape_atom(&hit.conversation_uuid),
                crate::agent::protocol::escape_atom(&hit.conversation_ref),
                protocol_snippet(&hit.title, AGENT_SEARCH_TITLE_CHARS)
            ));
            units.push(hit_unit(hit));
        }
    }
    units.extend(warning_records.iter().cloned());
    let all_cut = Cut {
        kept_units: 0,
        omitted_units: units.len(),
        omitted_lines: units.iter().map(|unit| unit.lines().count()).sum(),
    };
    let fallback = || header(Some(&all_cut)) + &recovery;

    Response {
        budget: output.budget,
        header: &header,
        units,
        whole_trailer: String::new(),
        cut_footer: &cut_footer,
        fallback: &fallback,
    }
    .render()
}

fn budget_atom(budget: Option<usize>) -> String {
    budget
        .map(|value| value.to_string())
        .unwrap_or_else(|| "none".to_string())
}

fn output_hits(output: &AgentSearchOutput) -> Vec<&AgentOutputHit> {
    if output.protocol == AgentProtocolKind::Search && !output.flat && !output.groups.is_empty() {
        output
            .groups
            .iter()
            .flat_map(|group| group.hits.iter())
            .collect()
    } else {
        output.hits.iter().collect()
    }
}

fn score_breakdown_atoms(breakdown: Option<SemanticScoreBreakdown>) -> String {
    breakdown.map_or_else(String::new, |score| {
        format!(
            " semantic={:.6} lexical={:.6}",
            score.semantic, score.lexical
        )
    })
}

/// A hit and its read recipe: one unit, never split by truncation.
fn hit_unit(hit: &AgentOutputHit) -> String {
    let mut rendered = String::new();
    rendered.push_str(&format!(
        "hit project={} uuid={} ref={} anchors={} source={} score={:.6}{} focus=m{}..m{} | {}\n",
        crate::agent::protocol::escape_atom(&hit.project_id),
        crate::agent::protocol::escape_atom(&hit.conversation_uuid),
        crate::agent::protocol::escape_atom(&hit.conversation_ref),
        hit.anchors.join(","),
        output_source_atom(hit),
        hit.score,
        score_breakdown_atoms(hit.semantic_score_breakdown),
        hit.focus_range.start,
        hit.focus_range.end,
        protocol_snippet(&hit.preview, AGENT_SEARCH_HIT_CHARS)
    ));
    rendered.push_str(&format!(
        "read ref={}:m{}..m{} focus=m{}..m{}{}\n",
        crate::agent::protocol::escape_atom(&hit.conversation_ref),
        hit.read_range.start,
        hit.read_range.end,
        hit.focus_range.start,
        hit.focus_range.end,
        render_option_atoms(hit.render_options)
    ));
    rendered
}

fn protocol_snippet(text: &str, limit: usize) -> String {
    let sanitized = sanitize_agent_text(text);
    let normalized = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= limit {
        normalized
    } else {
        let mut snippet = normalized
            .chars()
            .take(limit.saturating_sub(3))
            .collect::<String>();
        snippet.push_str("...");
        snippet
    }
}

pub fn run_within_search(
    request: &AgentWithinRequest,
    conversation: &Conversation,
    resolved: &ResolvedConversation,
    transcript: &AgentTranscript,
    semantic_hits: &[SemanticHit],
) -> AgentSearchOutput {
    let hits = match request.mode {
        SearchMode::Lexical | SearchMode::Exact => {
            return run_within_lexical_search(request, conversation, resolved, transcript);
        }
        SearchMode::Semantic => semantic_output_hits(
            semantic_hits,
            request.top,
            &[AgentConversationInput {
                conversation,
                resolved: resolved.clone(),
                original_index: 0,
            }],
        ),
        SearchMode::Hybrid => {
            let candidate_depth = modality_candidate_depth_for_hits(request.top);
            hybrid_hits(
                retrieval_hits(
                    &request.query,
                    candidate_depth,
                    conversation,
                    resolved,
                    transcript,
                    SearchMode::Lexical,
                ),
                semantic_output_hits(
                    semantic_hits,
                    candidate_depth,
                    &[AgentConversationInput {
                        conversation,
                        resolved: resolved.clone(),
                        original_index: 0,
                    }],
                ),
                request.top,
            )
        }
    };
    within_output(request, hits, resolved, transcript)
}

/// Lexical (or exact) evidence for a within request regardless of its mode.
/// The hybrid path falls back to this when semantic search is unavailable,
/// so the output keeps reporting `request.mode`.
pub fn run_within_lexical_search(
    request: &AgentWithinRequest,
    conversation: &Conversation,
    resolved: &ResolvedConversation,
    transcript: &AgentTranscript,
) -> AgentSearchOutput {
    let retrieval_mode = lexical_retrieval_mode(request.mode);
    let hits = retrieval_hits(
        &retrieval_query(&request.query, retrieval_mode),
        request.top,
        conversation,
        resolved,
        transcript,
        retrieval_mode,
    );
    within_output(request, hits, resolved, transcript)
}

fn within_output(
    request: &AgentWithinRequest,
    hits: Vec<AgentOutputHit>,
    resolved: &ResolvedConversation,
    transcript: &AgentTranscript,
) -> AgentSearchOutput {
    let mut output = AgentSearchOutput {
        protocol: AgentProtocolKind::Within,
        target: None,
        query: request.query.raw().to_string(),
        mode: request.mode,
        hits,
        groups: Vec::new(),
        flat: true,
        budget: request.budget,
        stats: AgentSearchStats {
            shortlisted: 1,
            transcripts_loaded: 1,
        },
    };
    attach_transcript_metadata(&mut output, resolved, transcript);
    output
}

#[cfg(test)]
pub fn run_global_lexical_search(
    request: &AgentSearchRequest,
    conversations: &[Conversation],
    keys: &[AgentConversationKey],
    ranked_indices: &[usize],
    load_transcript: impl Fn(&AgentConversationKey) -> Result<AgentTranscript>,
) -> Result<AgentSearchOutput> {
    run_global_lexical_search_reporting(
        request,
        conversations,
        keys,
        ranked_indices,
        load_transcript,
        |_, _| {},
    )
}

pub fn run_global_lexical_search_reporting(
    request: &AgentSearchRequest,
    conversations: &[Conversation],
    keys: &[AgentConversationKey],
    ranked_indices: &[usize],
    load_transcript: impl Fn(&AgentConversationKey) -> Result<AgentTranscript>,
    mut report_error: impl FnMut(&AgentConversationKey, &AppError),
) -> Result<AgentSearchOutput> {
    let retrieval_mode = lexical_retrieval_mode(request.mode);
    let retrieval_query = retrieval_query(&request.query, retrieval_mode);
    let limit = shortlist_limit(request.top).min(ranked_indices.len());
    let resolved_by_path = crate::agent::refs::resolved_conversations_for_keys(keys)
        .into_iter()
        .map(|resolved| (resolved.key.path.clone(), resolved))
        .collect::<HashMap<_, _>>();
    let mut hits = Vec::new();
    let mut transcripts_loaded = 0;

    for index in ranked_indices.iter().take(limit).copied() {
        let Some(conversation) = conversations.get(index) else {
            continue;
        };
        let Some(resolved) = resolved_by_path.get(&conversation.path) else {
            continue;
        };
        let transcript = match load_transcript(&resolved.key) {
            Ok(transcript) => transcript,
            Err(error) => {
                report_error(&resolved.key, &error);
                continue;
            }
        };
        transcripts_loaded += 1;
        hits.extend(retrieval_hits(
            &retrieval_query,
            request
                .retrieval_hits_per_conversation
                .unwrap_or_else(|| lexical_per_conversation_candidate_depth(request)),
            conversation,
            resolved,
            &transcript,
            retrieval_mode,
        ));
        if request.retrieval_hits_per_conversation.is_some() && hits.len() >= request.top {
            break;
        }
        if !request.flat {
            let conversation_count = hits
                .iter()
                .map(|hit| hit.conversation_ref.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len();
            if conversation_count >= request.top {
                break;
            }
        }
    }

    sort_output_hits(&mut hits);
    let (hits, groups) = finalize_global_hits(hits, request);

    Ok(AgentSearchOutput {
        protocol: AgentProtocolKind::Search,
        target: None,
        query: request.query.raw().to_string(),
        mode: request.mode,
        hits,
        groups,
        flat: request.flat,
        budget: request.budget,
        stats: AgentSearchStats {
            shortlisted: limit,
            transcripts_loaded,
        },
    })
}

pub fn run_global_semantic_search(
    request: &AgentSearchRequest,
    inputs: &[AgentConversationInput<'_>],
    semantic_hits: &[SemanticHit],
) -> AgentSearchOutput {
    let semantic_order = semantic_conversation_order(semantic_hits, inputs);
    let mut hits = semantic_output_hit_candidates(semantic_hits, inputs);
    sort_output_hits(&mut hits);
    deduplicate_hits_by_identity(&mut hits);
    apply_semantic_conversation_order(&mut hits, &semantic_order);
    let (hits, groups) = finalize_global_hits(hits, request);
    AgentSearchOutput {
        protocol: AgentProtocolKind::Search,
        target: None,
        query: request.query.raw().to_string(),
        mode: request.mode,
        hits,
        groups,
        flat: request.flat,
        budget: request.budget,
        stats: AgentSearchStats {
            shortlisted: inputs.len(),
            transcripts_loaded: 0,
        },
    }
}

pub fn run_global_hybrid_search(
    request: &AgentSearchRequest,
    lexical: AgentSearchOutput,
    semantic_hits: &[SemanticHit],
    inputs: &[AgentConversationInput<'_>],
) -> AgentSearchOutput {
    let candidate_depth = modality_candidate_depth(request);
    let semantic_order = semantic_conversation_order(semantic_hits, inputs);
    let semantic_conversations = semantic_order
        .iter()
        .take(candidate_depth)
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let mut semantic = semantic_output_hit_candidates(semantic_hits, inputs);
    semantic.retain(|hit| semantic_conversations.contains(&hit.conversation_ref));
    sort_output_hits(&mut semantic);
    deduplicate_hits_by_identity(&mut semantic);
    let hits =
        hybrid_hits_with_semantic_order(lexical.hits, semantic, &semantic_order, candidate_depth);
    let (hits, groups) = finalize_global_hits(hits, request);
    AgentSearchOutput {
        protocol: AgentProtocolKind::Search,
        target: None,
        query: request.query.raw().to_string(),
        mode: SearchMode::Hybrid,
        hits,
        groups,
        flat: request.flat,
        budget: request.budget,
        stats: lexical.stats,
    }
}

pub fn scoped_conversation_inputs(
    conversations: &[Conversation],
    scope: AgentSearchScope,
    current_project_dir_name: Option<&str>,
) -> Result<Vec<usize>> {
    let mut indices = Vec::new();
    for (index, conversation) in conversations.iter().enumerate() {
        if scope == AgentSearchScope::Local {
            let Some(project) = current_project_dir_name else {
                return Err(AppError::ConfigError(
                    "local agent search requires a current project".to_string(),
                ));
            };
            let matches = conversation
                .path
                .parent()
                .and_then(|p| p.file_name())
                .is_some_and(|name| {
                    crate::history::is_same_project(&name.to_string_lossy(), project)
                });
            if !matches {
                continue;
            }
        }
        indices.push(index);
    }
    Ok(indices)
}

pub fn shortlist_limit(top: usize) -> usize {
    top.saturating_mul(SHORTLIST_FACTOR)
        .clamp(SHORTLIST_MIN, SHORTLIST_MAX)
}

pub fn modality_candidate_depth(request: &AgentSearchRequest) -> usize {
    let requested_hits = if request.flat {
        request.top
    } else {
        request
            .top
            .saturating_mul(request.hits_per_conversation.max(1))
    };
    modality_candidate_depth_for_hits(requested_hits)
}

fn modality_candidate_depth_for_hits(requested_hits: usize) -> usize {
    requested_hits
        .saturating_mul(MODALITY_CANDIDATE_FACTOR)
        .clamp(MODALITY_CANDIDATE_MIN, MODALITY_CANDIDATE_MAX)
}

fn lexical_per_conversation_candidate_depth(request: &AgentSearchRequest) -> usize {
    if request.flat {
        request.top
    } else {
        request.hits_per_conversation.saturating_mul(4)
    }
    .max(1)
}

/// How transcript retrieval runs for a search mode: exact stays exact, every
/// other mode retrieves lexically.
fn lexical_retrieval_mode(mode: SearchMode) -> SearchMode {
    match mode {
        SearchMode::Exact => SearchMode::Exact,
        _ => SearchMode::Lexical,
    }
}

/// The query retrieval matches: exact mode treats an unquoted query as one
/// phrase; quoted-only queries and lexical retrieval use the query as parsed.
fn retrieval_query(query: &ParsedQuery, retrieval_mode: SearchMode) -> Cow<'_, ParsedQuery> {
    if retrieval_mode == SearchMode::Exact && !query.is_quoted_only() {
        Cow::Owned(ParsedQuery::exact_phrase(query.raw()))
    } else {
        Cow::Borrowed(query)
    }
}

fn retrieval_hits(
    query: &ParsedQuery,
    limit: usize,
    conversation: &Conversation,
    resolved: &ResolvedConversation,
    transcript: &AgentTranscript,
    mode: SearchMode,
) -> Vec<AgentOutputHit> {
    retrieve_agent_hits_for_target(
        AgentTranscriptSearchTarget {
            transcript,
            conversation_ref: Some(&resolved.reference.canonical()),
            timestamp: Some(conversation.timestamp),
        },
        query,
        AgentRetrievalOptions {
            limit,
            ..AgentRetrievalOptions::default()
        },
    )
    .into_iter()
    .map(|hit| retrieval_output_hit(hit, conversation, resolved, transcript, mode))
    .collect()
}

fn retrieval_output_hit(
    hit: RetrievalHit,
    conversation: &Conversation,
    resolved: &ResolvedConversation,
    transcript: &AgentTranscript,
    mode: SearchMode,
) -> AgentOutputHit {
    AgentOutputHit {
        conversation_ref: resolved.reference.canonical(),
        project_id: resolved.key.project_id(),
        conversation_uuid: resolved.reference.uuid(),
        session: resolved.key.session_filename.clone(),
        anchors: anchors_for_range(transcript, resolved, hit.focus_range),
        title: title_for_conversation(conversation),
        score: hit.score,
        evidence_score: hit.score,
        semantic_score_breakdown: None,
        source: if mode == SearchMode::Exact {
            AgentHitKind::Exact
        } else {
            AgentHitKind::Lexical
        },
        evidence_source: hit.source,
        render_options: hit.render_options,
        preview: hit.preview,
        focus_range: hit.focus_range,
        read_range: hit.read_range,
    }
}

fn anchors_for_range(
    transcript: &AgentTranscript,
    resolved: &ResolvedConversation,
    range: MessageRange,
) -> Vec<String> {
    transcript
        .messages
        .iter()
        .filter(|message| range.start <= message.ordinal && message.ordinal <= range.end)
        .map(|message| transcript.message_anchor(resolved, message))
        .collect()
}

fn semantic_output_hits(
    hits: &[SemanticHit],
    limit: usize,
    inputs: &[AgentConversationInput<'_>],
) -> Vec<AgentOutputHit> {
    let mut output = semantic_output_hit_candidates(hits, inputs);
    sort_output_hits(&mut output);
    deduplicate_hits_by_identity(&mut output);
    output.truncate(limit);
    output
}

fn semantic_output_hit_candidates(
    hits: &[SemanticHit],
    inputs: &[AgentConversationInput<'_>],
) -> Vec<AgentOutputHit> {
    hits.iter()
        .filter(|hit| hit.explanation.chunk.source != SemanticChunkSource::AgentRoute)
        .filter_map(|hit| {
            let input = inputs
                .iter()
                .find(|input| input.original_index == hit.conversation_index)?;
            Some(AgentOutputHit {
                conversation_ref: input.resolved.reference.canonical(),
                project_id: input.resolved.key.project_id(),
                conversation_uuid: input.resolved.reference.uuid(),
                session: input.resolved.key.session_filename.clone(),
                anchors: Vec::new(),
                title: title_for_conversation(input.conversation),
                score: semantic_score(hit.score_breakdown),
                evidence_score: semantic_score(hit.score_breakdown),
                semantic_score_breakdown: Some(hit.score_breakdown),
                source: AgentHitKind::Semantic,
                evidence_source: semantic_evidence_source(hit.explanation.chunk.source),
                render_options: semantic_render_options(hit.explanation.chunk.source),
                preview: format_evidence_preview(&hit.snippet),
                focus_range: hit.message_range,
                read_range: read_range_for_focus(
                    hit.message_range,
                    input.conversation.message_count.max(hit.message_range.end),
                    1,
                ),
            })
        })
        .collect()
}

fn semantic_conversation_order(
    hits: &[SemanticHit],
    inputs: &[AgentConversationInput<'_>],
) -> Vec<String> {
    let input_refs = inputs
        .iter()
        .map(|input| (input.original_index, input.resolved.reference.canonical()))
        .collect::<HashMap<_, _>>();
    let mut seen = std::collections::HashSet::new();
    hits.iter()
        .filter_map(|hit| input_refs.get(&hit.conversation_index))
        .filter(|reference| seen.insert((*reference).clone()))
        .cloned()
        .collect()
}

fn apply_semantic_conversation_order(hits: &mut [AgentOutputHit], order: &[String]) {
    let ranks = order
        .iter()
        .enumerate()
        .map(|(index, reference)| (reference.as_str(), index + 1))
        .collect::<HashMap<_, _>>();
    for hit in hits {
        if let Some(rank) = ranks.get(hit.conversation_ref.as_str()) {
            hit.score = rrf_score(None, Some(*rank));
        }
    }
}

fn semantic_evidence_source(source: SemanticChunkSource) -> AgentHitSource {
    match source {
        SemanticChunkSource::VisibleDialogue
        | SemanticChunkSource::AgentRoute
        | SemanticChunkSource::AgentSubagentDialogue => AgentHitSource::Dialogue,
        SemanticChunkSource::AgentTool | SemanticChunkSource::AgentSubagentTool => {
            AgentHitSource::Tool
        }
        SemanticChunkSource::AgentThinking | SemanticChunkSource::AgentSubagentThinking => {
            AgentHitSource::Thinking
        }
    }
}

fn semantic_render_options(source: SemanticChunkSource) -> AgentHitRenderOptions {
    AgentHitRenderOptions {
        tools: matches!(
            source,
            SemanticChunkSource::AgentTool | SemanticChunkSource::AgentSubagentTool
        ),
        tool_results: matches!(
            source,
            SemanticChunkSource::AgentTool | SemanticChunkSource::AgentSubagentTool
        ),
        thinking: matches!(
            source,
            SemanticChunkSource::AgentThinking | SemanticChunkSource::AgentSubagentThinking
        ),
        subagents: matches!(
            source,
            SemanticChunkSource::AgentSubagentDialogue
                | SemanticChunkSource::AgentSubagentTool
                | SemanticChunkSource::AgentSubagentThinking
        ),
    }
}

fn finalize_global_hits(
    mut hits: Vec<AgentOutputHit>,
    request: &AgentSearchRequest,
) -> (Vec<AgentOutputHit>, Vec<AgentConversationGroup>) {
    if request.flat {
        sort_output_hits(&mut hits);
        deduplicate_hits_by_identity(&mut hits);
        hits.truncate(request.top);
        return (hits, Vec::new());
    }

    let groups = build_conversation_groups(
        hits,
        request.top,
        request.hits_per_conversation,
        request.all_hits,
    );
    let output_hits = flatten_groups(&groups, request.top);
    (output_hits, groups)
}

fn deduplicate_hits_by_identity(hits: &mut Vec<AgentOutputHit>) {
    let mut unique = Vec::<AgentOutputHit>::with_capacity(hits.len());
    for hit in hits.drain(..) {
        if let Some(existing) = unique
            .iter_mut()
            .find(|existing| same_evidence_identity(existing, &hit))
        {
            merge_duplicate_hit(existing, &hit);
        } else {
            unique.push(hit);
        }
    }
    *hits = unique;
}

fn build_conversation_groups(
    hits: Vec<AgentOutputHit>,
    top: usize,
    hits_per_conversation: usize,
    all_hits: bool,
) -> Vec<AgentConversationGroup> {
    let mut by_ref = Vec::<AgentConversationGroup>::new();
    for hit in hits {
        if let Some(group) = by_ref
            .iter_mut()
            .find(|group| group.conversation_ref == hit.conversation_ref)
        {
            group.total_hits += 1;
            push_group_hit(group, hit, hits_per_conversation, all_hits);
        } else {
            let mut group = AgentConversationGroup {
                conversation_ref: hit.conversation_ref.clone(),
                project_id: hit.project_id.clone(),
                conversation_uuid: hit.conversation_uuid.clone(),
                session: hit.session.clone(),
                title: hit.title.clone(),
                score: hit.score,
                total_hits: 1,
                hits: Vec::new(),
            };
            push_group_hit(&mut group, hit, hits_per_conversation, all_hits);
            by_ref.push(group);
        }
    }
    for group in &mut by_ref {
        sort_group_hits(&mut group.hits);
        group.hits.truncate(hits_per_conversation);
        group.score = group
            .hits
            .first()
            .map(|hit| hit.score)
            .unwrap_or(group.score);
    }
    by_ref.retain(|group| !group.hits.is_empty());
    by_ref.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.conversation_ref.cmp(&b.conversation_ref))
    });
    by_ref.truncate(top);
    by_ref
}

fn push_group_hit(
    group: &mut AgentConversationGroup,
    hit: AgentOutputHit,
    hits_per_conversation: usize,
    all_hits: bool,
) {
    if !all_hits
        && let Some(existing) = group
            .hits
            .iter_mut()
            .find(|existing| same_evidence_identity(existing, &hit))
    {
        merge_duplicate_hit(existing, &hit);
        return;
    }
    group.hits.push(hit);
    sort_group_hits(&mut group.hits);
    group.hits.truncate(hits_per_conversation);
}

fn same_evidence_identity(existing: &AgentOutputHit, candidate: &AgentOutputHit) -> bool {
    existing.conversation_ref == candidate.conversation_ref
        && existing.focus_range == candidate.focus_range
        && existing.evidence_source == candidate.evidence_source
}

/// Keep all components from one contributing chunk, selected by semantic ranker score.
fn merge_score_breakdown(
    existing: &mut Option<SemanticScoreBreakdown>,
    candidate: Option<SemanticScoreBreakdown>,
) {
    let Some(candidate) = candidate else {
        return;
    };
    if existing.is_none_or(|current| {
        candidate
            .hybrid
            .total_cmp(&current.hybrid)
            .then_with(|| candidate.semantic.total_cmp(&current.semantic))
            .then_with(|| candidate.lexical.total_cmp(&current.lexical))
            .is_gt()
    }) {
        *existing = Some(candidate);
    }
}

fn merge_duplicate_hit(existing: &mut AgentOutputHit, candidate: &AgentOutputHit) {
    existing.render_options.merge(candidate.render_options);
    existing.read_range = existing.read_range.union(&candidate.read_range);
    existing.score = existing.score.max(candidate.score);
    existing.evidence_score = existing.evidence_score.max(candidate.evidence_score);
    merge_score_breakdown(
        &mut existing.semantic_score_breakdown,
        candidate.semantic_score_breakdown,
    );
}

fn sort_group_hits(hits: &mut [AgentOutputHit]) {
    hits.sort_by(|a, b| {
        score_bucket(b.score)
            .cmp(&score_bucket(a.score))
            .then_with(|| {
                evidence_source_rank(a.evidence_source)
                    .cmp(&evidence_source_rank(b.evidence_source))
            })
            .then_with(|| {
                b.evidence_score
                    .partial_cmp(&a.evidence_score)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal))
            .then_with(|| a.focus_range.start.cmp(&b.focus_range.start))
            .then_with(|| source_rank(a.source).cmp(&source_rank(b.source)))
    });
}

fn score_bucket(score: f64) -> i64 {
    (score * 10.0).floor() as i64
}

fn evidence_source_rank(source: AgentHitSource) -> u8 {
    match source {
        AgentHitSource::Dialogue => 0,
        AgentHitSource::Tool => 1,
        AgentHitSource::Thinking => 2,
    }
}

fn flatten_groups(groups: &[AgentConversationGroup], limit: usize) -> Vec<AgentOutputHit> {
    let mut hits = groups
        .iter()
        .flat_map(|group| group.hits.iter().cloned())
        .collect::<Vec<_>>();
    sort_output_hits(&mut hits);
    hits.truncate(limit);
    hits
}

fn hybrid_hits(
    lexical_hits: Vec<AgentOutputHit>,
    semantic_hits: Vec<AgentOutputHit>,
    limit: usize,
) -> Vec<AgentOutputHit> {
    let semantic_order = semantic_hits
        .iter()
        .map(|hit| hit.conversation_ref.clone())
        .collect::<Vec<_>>();
    let mut hits =
        hybrid_hits_with_semantic_order(lexical_hits, semantic_hits, &semantic_order, limit);
    hits.truncate(limit);
    hits
}

fn hybrid_hits_with_semantic_order(
    lexical_hits: Vec<AgentOutputHit>,
    semantic_hits: Vec<AgentOutputHit>,
    semantic_order: &[String],
    _limit: usize,
) -> Vec<AgentOutputHit> {
    let mut conversation_ranks = std::collections::HashMap::<String, ConversationRanks>::new();
    let mut seen = std::collections::HashSet::new();
    for hit in &lexical_hits {
        if seen.insert(hit.conversation_ref.clone()) {
            let rank = seen.len();
            conversation_ranks
                .entry(hit.conversation_ref.clone())
                .or_default()
                .lexical = Some(rank);
        }
    }
    seen.clear();
    for reference in semantic_order {
        if seen.insert(reference.clone()) {
            let rank = seen.len();
            conversation_ranks
                .entry(reference.clone())
                .or_default()
                .semantic = Some(rank);
        }
    }

    let mut ranked = Vec::<RankedHit>::new();
    for (rank, hit) in lexical_hits.into_iter().enumerate() {
        ranked.push(RankedHit {
            exact: hit.source == AgentHitKind::Exact,
            hit,
            lexical_rank: Some(rank + 1),
            semantic_rank: None,
        });
    }
    for (rank, hit) in semantic_hits.into_iter().enumerate() {
        if let Some(existing) = ranked.iter_mut().find(|existing| {
            existing.hit.conversation_ref == hit.conversation_ref
                && existing.hit.focus_range == hit.focus_range
        }) {
            existing.semantic_rank = Some(rank + 1);
            existing.hit.source = AgentHitKind::Hybrid;
            merge_score_breakdown(
                &mut existing.hit.semantic_score_breakdown,
                hit.semantic_score_breakdown,
            );
            existing.hit.render_options.merge(hit.render_options);
            existing.hit.read_range = existing.hit.read_range.union(&hit.read_range);
        } else {
            ranked.push(RankedHit {
                hit,
                lexical_rank: None,
                semantic_rank: Some(rank + 1),
                exact: false,
            });
        }
    }
    for ranked_hit in &mut ranked {
        let ranks = conversation_ranks
            .get(&ranked_hit.hit.conversation_ref)
            .copied()
            .unwrap_or_default();
        ranked_hit.hit.score = rrf_score(ranks.lexical, ranks.semantic);
    }
    ranked.sort_by(|a, b| {
        b.hit
            .score
            .partial_cmp(&a.hit.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| source_priority(a).cmp(&source_priority(b)))
            .then_with(|| {
                b.hit
                    .evidence_score
                    .partial_cmp(&a.hit.evidence_score)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| a.hit.conversation_ref.cmp(&b.hit.conversation_ref))
            .then_with(|| a.hit.focus_range.start.cmp(&b.hit.focus_range.start))
    });
    ranked.into_iter().map(|ranked| ranked.hit).collect()
}

#[derive(Clone, Copy, Default)]
struct ConversationRanks {
    lexical: Option<usize>,
    semantic: Option<usize>,
}

fn source_priority(hit: &RankedHit) -> u8 {
    if hit.exact {
        0
    } else if hit.lexical_rank.is_some() {
        1
    } else {
        2
    }
}

fn rrf_score(lexical_rank: Option<usize>, semantic_rank: Option<usize>) -> f64 {
    lexical_rank.map_or(0.0, |rank| 1.0 / (RRF_K + rank as f64))
        + semantic_rank.map_or(0.0, |rank| 1.0 / (RRF_K + rank as f64))
}

fn semantic_score(score: SemanticScoreBreakdown) -> f64 {
    score.hybrid as f64
}

fn sort_output_hits(hits: &mut [AgentOutputHit]) {
    hits.sort_by(|a, b| {
        score_bucket(b.score)
            .cmp(&score_bucket(a.score))
            .then_with(|| {
                evidence_source_rank(a.evidence_source)
                    .cmp(&evidence_source_rank(b.evidence_source))
            })
            .then_with(|| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal))
            .then_with(|| {
                b.evidence_score
                    .partial_cmp(&a.evidence_score)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| source_rank(a.source).cmp(&source_rank(b.source)))
            .then_with(|| a.conversation_ref.cmp(&b.conversation_ref))
            .then_with(|| a.focus_range.start.cmp(&b.focus_range.start))
    });
}

fn source_rank(source: AgentHitKind) -> u8 {
    match source {
        AgentHitKind::Exact => 0,
        AgentHitKind::Lexical => 1,
        AgentHitKind::Hybrid => 2,
        AgentHitKind::Semantic => 3,
    }
}

fn title_for_conversation(conversation: &Conversation) -> String {
    conversation
        .custom_title
        .as_deref()
        .or(conversation.summary.as_deref())
        .unwrap_or(&conversation.preview)
        .to_string()
}

fn mode_atom(mode: SearchMode) -> &'static str {
    match mode {
        SearchMode::Lexical => "lexical",
        SearchMode::Semantic => "semantic",
        SearchMode::Exact => "exact",
        SearchMode::Hybrid => "hybrid",
    }
}

fn output_source_atom(hit: &AgentOutputHit) -> &'static str {
    match hit.evidence_source {
        AgentHitSource::Dialogue => hit_source_atom(hit.source),
        AgentHitSource::Tool => "tool",
        AgentHitSource::Thinking => "thinking",
    }
}

fn hit_source_atom(source: AgentHitKind) -> &'static str {
    match source {
        AgentHitKind::Exact => "exact",
        AgentHitKind::Lexical => "lexical",
        AgentHitKind::Semantic => "semantic",
        AgentHitKind::Hybrid => "hybrid",
    }
}

fn render_option_atoms(options: AgentHitRenderOptions) -> String {
    format!(
        " tools={} tool-results={} thinking={} subagents={}",
        options.tools, options.tool_results, options.thinking, options.subagents
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::refs::AgentConversationKey;
    use crate::agent::test_support::text_message;
    use crate::agent::transcript::{AgentMessage, AgentMessageRole, AgentTranscript};
    use crate::semantic::types::{
        SemanticChunkIdentity, SemanticExplanation, SemanticQuality, SemanticRationaleKind,
    };
    use chrono::Local;
    use std::path::PathBuf;

    fn message(ordinal: usize, role: AgentMessageRole, text: &str) -> AgentMessage {
        text_message(ordinal, role, text)
    }

    const TEST_UUID: &str = "12345678-1234-4234-9234-123456789abc";

    fn transcript(messages: Vec<AgentMessage>) -> AgentTranscript {
        crate::agent::test_support::transcript(messages, "session.jsonl")
    }

    fn conversation(path: &str, title: &str) -> Conversation {
        Conversation {
            source: crate::history::Source::Claude,
            session_id: String::new(),
            path: PathBuf::from(path),
            index: 0,
            timestamp: Local::now(),
            preview: title.to_string(),
            preview_first: title.to_string(),
            preview_last: title.to_string(),
            full_text: title.to_string(),
            agent_search_text: String::new(),
            semantic_route_text: String::new(),
            semantic_turns: vec![title.to_string()],
            semantic_turn_ranges: vec![MessageRange::single(1)],
            search_text_lower: title.to_string(),
            dialogue_text_lower: title.to_string(),
            project_name: Some("project-a".to_string()),
            project_path: None,
            cwd: None,
            message_count: 1,
            parse_errors: vec![],
            summary: None,
            custom_title: Some(title.to_string()),
            model: None,
            total_tokens: 0,
            duration_minutes: None,
        }
    }

    fn resolved(path: &str) -> ResolvedConversation {
        let key = AgentConversationKey::new("project-a", path, PathBuf::from(path));
        ResolvedConversation {
            reference: key.conversation_ref(),
            key,
        }
    }

    fn request(query: &str, mode: Option<SearchMode>) -> AgentWithinRequest {
        let query = ParsedQuery::parse(query);
        let mode = effective_agent_mode(&query, mode.unwrap_or_default());
        AgentWithinRequest {
            query,
            mode,
            top: 10,
            budget: None,
        }
    }

    fn global_request(query: &str, mode: SearchMode, top: usize, flat: bool) -> AgentSearchRequest {
        let query = ParsedQuery::parse(query);
        let mode = effective_agent_mode(&query, mode);
        AgentSearchRequest {
            query,
            mode,
            top,
            flat,
            hits_per_conversation: 2,
            retrieval_hits_per_conversation: None,
            all_hits: false,
            budget: None,
        }
    }

    fn semantic_hit(index: usize, range: MessageRange, text: &str, score: f32) -> SemanticHit {
        semantic_hit_with_source(
            index,
            range,
            text,
            score,
            SemanticChunkSource::VisibleDialogue,
        )
    }

    fn semantic_hit_with_source(
        index: usize,
        range: MessageRange,
        text: &str,
        score: f32,
        source: SemanticChunkSource,
    ) -> SemanticHit {
        SemanticHit::new(
            SemanticScoreBreakdown {
                hybrid: score,
                semantic: score,
                lexical: 0.0,
            },
            SemanticExplanation {
                quality: SemanticQuality::Good,
                quality_label: "good",
                matched_terms: vec![],
                evidence_preview: text.to_string(),
                rationale_kind: SemanticRationaleKind::SemanticOnly,
                chunk: SemanticChunkIdentity {
                    conversation_index: index,
                    source,
                    session: "session".to_string(),
                    chunk_index: range.start,
                    message_range: range,
                },
            },
        )
    }

    fn test_uuid(_conv: &str) -> String {
        TEST_UUID.to_string()
    }

    fn lexical_dialogue_hit(
        conv: &str,
        title: &str,
        score: f64,
        preview: &str,
        focus_range: MessageRange,
        read_range: MessageRange,
    ) -> AgentOutputHit {
        AgentOutputHit {
            conversation_ref: conv.to_string(),
            project_id: "pr_test".to_string(),
            conversation_uuid: test_uuid(conv),
            session: "session.jsonl".to_string(),
            anchors: vec!["ma_0000000000000000".to_string()],
            title: title.to_string(),
            score,
            evidence_score: score,
            semantic_score_breakdown: None,
            source: AgentHitKind::Lexical,
            evidence_source: AgentHitSource::Dialogue,
            render_options: AgentHitRenderOptions::default(),
            preview: preview.to_string(),
            focus_range,
            read_range,
        }
    }

    fn lexical_tool_hit(
        conv: &str,
        title: &str,
        score: f64,
        preview: &str,
        focus_range: MessageRange,
        read_range: MessageRange,
    ) -> AgentOutputHit {
        AgentOutputHit {
            conversation_ref: conv.to_string(),
            project_id: "pr_test".to_string(),
            conversation_uuid: test_uuid(conv),
            session: "session.jsonl".to_string(),
            anchors: vec!["ma_0000000000000000".to_string()],
            title: title.to_string(),
            score,
            evidence_score: score,
            semantic_score_breakdown: None,
            source: AgentHitKind::Lexical,
            evidence_source: AgentHitSource::Tool,
            render_options: AgentHitRenderOptions::default(),
            preview: preview.to_string(),
            focus_range,
            read_range,
        }
    }

    fn semantic_dialogue_hit(
        conv: &str,
        title: &str,
        score: f64,
        preview: &str,
        focus_range: MessageRange,
        read_range: MessageRange,
    ) -> AgentOutputHit {
        AgentOutputHit {
            conversation_ref: conv.to_string(),
            project_id: "pr_test".to_string(),
            conversation_uuid: test_uuid(conv),
            session: "session.jsonl".to_string(),
            anchors: vec!["ma_0000000000000000".to_string()],
            title: title.to_string(),
            score,
            evidence_score: score,
            semantic_score_breakdown: Some(SemanticScoreBreakdown {
                hybrid: score as f32,
                semantic: score as f32,
                lexical: 0.0,
            }),
            source: AgentHitKind::Semantic,
            evidence_source: AgentHitSource::Dialogue,
            render_options: AgentHitRenderOptions::default(),
            preview: preview.to_string(),
            focus_range,
            read_range,
        }
    }

    #[test]
    fn semantic_breakdown_survives_rrf_in_every_search_layout() {
        let conv = conversation("a.jsonl", "cache warming");
        let resolved = resolved("a.jsonl");
        let inputs = [AgentConversationInput {
            conversation: &conv,
            resolved: resolved.clone(),
            original_index: 0,
        }];
        let transcript = transcript(vec![message(1, AgentMessageRole::User, "cache warming")]);
        for breakdown in [
            SemanticScoreBreakdown {
                hybrid: 0.969,
                semantic: 0.769,
                lexical: 0.2,
            },
            SemanticScoreBreakdown {
                hybrid: 0.1,
                semantic: 0.1,
                lexical: 0.0,
            },
        ] {
            let mut semantic = semantic_hit(0, MessageRange::single(1), "cache warming", 0.0);
            semantic.score_breakdown = breakdown;
            let semantic = [semantic];
            for mode in [SearchMode::Semantic, SearchMode::Hybrid] {
                for flat in [false, true] {
                    let global = global_request("cache", mode, 10, flat);
                    let output = if mode == SearchMode::Semantic {
                        run_global_semantic_search(&global, &inputs, &semantic)
                    } else {
                        let lexical = run_within_search(
                            &request("cache", Some(SearchMode::Lexical)),
                            &conv,
                            &resolved,
                            &transcript,
                            &[],
                        );
                        run_global_hybrid_search(&global, lexical, &semantic, &inputs)
                    };
                    let expected_score =
                        rrf_score((mode == SearchMode::Hybrid).then_some(1), Some(1));
                    assert_eq!(output_hits(&output)[0].score, expected_score);
                    assert_eq!(
                        output_hits(&output)[0].semantic_score_breakdown,
                        Some(breakdown)
                    );
                    let rendered = format_agent_output(&output);
                    for record in rendered.lines().filter(|line| {
                        line.starts_with("hit ") || line.starts_with("conversation rank=")
                    }) {
                        assert!(record.contains(&score_breakdown_atoms(Some(breakdown))));
                        assert!(!record.contains(" hybrid="));
                    }
                }
                let output = run_within_search(
                    &request("cache", Some(mode)),
                    &conv,
                    &resolved,
                    &transcript,
                    &semantic,
                );
                assert_eq!(output.hits[0].semantic_score_breakdown, Some(breakdown));
                assert!(!format_agent_output(&output).contains(" hybrid="));
                assert!(
                    format_agent_output(&output).contains(&score_breakdown_atoms(Some(breakdown)))
                );
            }
        }
    }

    #[test]
    fn score_breakdown_merge_preserves_whole_tuple_and_missing_values() {
        let a = SemanticScoreBreakdown {
            hybrid: 0.7,
            semantic: 0.5,
            lexical: 0.2,
        };
        let b = SemanticScoreBreakdown {
            hybrid: 0.75,
            semantic: 0.75,
            lexical: 0.0,
        };
        for (first, second) in [(a, b), (b, a)] {
            let mut hit = semantic_dialogue_hit(
                "ch_a",
                "title",
                0.7,
                "preview",
                MessageRange::single(1),
                MessageRange::single(1),
            );
            hit.semantic_score_breakdown = Some(first);
            let mut candidate = hit.clone();
            candidate.semantic_score_breakdown = Some(second);
            merge_duplicate_hit(&mut hit, &candidate);
            assert_eq!(hit.semantic_score_breakdown, Some(b));
            candidate.semantic_score_breakdown = None;
            candidate.evidence_score = 20.0;
            merge_duplicate_hit(&mut hit, &candidate);
            assert_eq!(hit.semantic_score_breakdown, Some(b));
            assert_eq!(hit.evidence_score, 20.0);
        }
        assert_eq!(score_breakdown_atoms(None), "");
        assert_eq!(
            score_breakdown_atoms(Some(SemanticScoreBreakdown {
                hybrid: 0.0,
                semantic: -0.2,
                lexical: 0.2,
            })),
            " semantic=-0.200000 lexical=0.200000"
        );
        assert_eq!(
            score_breakdown_atoms(Some(SemanticScoreBreakdown {
                hybrid: 1.2,
                semantic: 1.0,
                lexical: 0.2,
            })),
            " semantic=1.000000 lexical=0.200000"
        );
    }

    #[test]
    fn conversation_breakdown_uses_first_retained_hit() {
        let mut lexical = lexical_dialogue_hit(
            "ch_a",
            "title",
            0.02,
            "lexical",
            MessageRange::single(1),
            MessageRange::single(1),
        );
        lexical.evidence_score = 10.0;
        let mut semantic = semantic_dialogue_hit(
            "ch_a",
            "title",
            0.9,
            "semantic",
            MessageRange::single(2),
            MessageRange::single(2),
        );
        semantic.score = 0.02;
        for all_hits in [false, true] {
            for limit in [1, 2] {
                let groups = build_conversation_groups(
                    vec![lexical.clone(), semantic.clone()],
                    1,
                    limit,
                    all_hits,
                );
                let output = AgentSearchOutput {
                    protocol: AgentProtocolKind::Search,
                    target: None,
                    query: "cache".into(),
                    mode: SearchMode::Hybrid,
                    hits: vec![],
                    groups,
                    flat: false,
                    budget: None,
                    stats: AgentSearchStats::default(),
                };
                let rendered = format_agent_output(&output);
                let conversation = rendered
                    .lines()
                    .find(|line| line.starts_with("conversation rank="))
                    .unwrap();
                assert!(!conversation.contains("semantic="));
                assert_eq!(rendered.contains("semantic="), limit == 2);
            }
        }
    }

    #[test]
    fn quoted_query_forces_exact_mode() {
        assert_eq!(
            effective_agent_mode(
                &ParsedQuery::parse("\"literal needle\""),
                SearchMode::Semantic
            ),
            SearchMode::Exact
        );
        assert_eq!(
            effective_agent_mode(&ParsedQuery::parse("literal needle"), SearchMode::Semantic),
            SearchMode::Semantic
        );
    }

    #[test]
    fn plain_query_hits_stay_lexical_when_preview_is_a_quoted_string() {
        let conv = conversation(&format!("{TEST_UUID}.jsonl"), "quoted title");
        let resolved = resolved(&format!("{TEST_UUID}.jsonl"));
        let transcript = transcript(vec![
            message(1, AgentMessageRole::User, "\"quoted preview only\""),
            message(2, AgentMessageRole::Assistant, "quoted preview, unquoted"),
        ]);

        let output = run_within_search(
            &request("quoted preview", Some(SearchMode::Lexical)),
            &conv,
            &resolved,
            &transcript,
            &[],
        );

        assert_eq!(output.hits.len(), 2);
        assert!(
            output
                .hits
                .iter()
                .all(|hit| hit.source == AgentHitKind::Lexical),
            "{:?}",
            output.hits.iter().map(|hit| hit.source).collect::<Vec<_>>()
        );
        assert!(output.hits.iter().any(|hit| hit.preview.starts_with('"')));
        assert!(!format_agent_output(&output).contains("source=exact"));
    }

    #[test]
    fn within_lexical_fallback_keeps_hybrid_mode_with_lexical_hits() {
        let conv = conversation(&format!("{TEST_UUID}.jsonl"), "title");
        let resolved = resolved(&format!("{TEST_UUID}.jsonl"));
        let transcript = transcript(vec![
            message(1, AgentMessageRole::User, "cache warming answer"),
            message(2, AgentMessageRole::Assistant, "unrelated"),
        ]);

        let output = run_within_lexical_search(
            &request("cache warming", Some(SearchMode::Hybrid)),
            &conv,
            &resolved,
            &transcript,
        );

        assert_eq!(output.mode, SearchMode::Hybrid);
        assert_eq!(output.hits.len(), 1);
        assert_eq!(output.hits[0].source, AgentHitKind::Lexical);
        assert!(format_agent_output(&output).starts_with("protocol agent-within mode=hybrid "));
    }

    #[test]
    fn exact_mode_hits_are_exact_for_an_unquoted_query() {
        let conv = conversation(&format!("{TEST_UUID}.jsonl"), "title");
        let resolved = resolved(&format!("{TEST_UUID}.jsonl"));
        let transcript = transcript(vec![
            message(1, AgentMessageRole::User, "cache warming answer"),
            message(2, AgentMessageRole::Assistant, "warming the cache"),
        ]);

        let output = run_within_search(
            &request("cache warming", Some(SearchMode::Exact)),
            &conv,
            &resolved,
            &transcript,
            &[],
        );

        assert_eq!(output.query, "cache warming");
        assert_eq!(output.hits.len(), 1);
        assert_eq!(output.hits[0].source, AgentHitKind::Exact);
        assert_eq!(output.hits[0].focus_range, MessageRange::single(1));
    }

    #[test]
    fn zero_matches_emit_protocol_and_query_only() {
        let output = AgentSearchOutput {
            protocol: AgentProtocolKind::Search,
            target: None,
            query: "missing".to_string(),
            mode: SearchMode::Lexical,
            hits: vec![],
            groups: vec![],
            flat: false,
            budget: None,
            stats: AgentSearchStats::default(),
        };

        assert_eq!(
            format_agent_output(&output),
            "protocol agent-search mode=lexical cut=none chars=none policy=per-hit groups=0 hits=0\nquery text=missing hits=0\ngroups count=0\n"
        );
    }

    #[test]
    fn within_without_hits_still_emits_identity() {
        let conv = conversation(&format!("{TEST_UUID}.jsonl"), "title");
        let resolved = resolved(&format!("{TEST_UUID}.jsonl"));
        let transcript = transcript(vec![message(1, AgentMessageRole::User, "haystack")]);

        let output = run_within_search(
            &request("missing", Some(SearchMode::Lexical)),
            &conv,
            &resolved,
            &transcript,
            &[],
        );
        let rendered = format_agent_output(&output);

        assert!(rendered.contains("conversation project=pr_"));
        assert!(rendered.contains(&format!("uuid={TEST_UUID} ref=ch_")));
        assert!(rendered.contains(&format!("ref={}", resolved.reference.canonical())));
    }

    #[test]
    fn within_lexical_formats_title_hit_and_read_lines() {
        let conv = conversation(&format!("{TEST_UUID}.jsonl"), "cache title");
        let resolved = resolved(&format!("{TEST_UUID}.jsonl"));
        let transcript = transcript(vec![
            message(1, AgentMessageRole::User, "question"),
            message(2, AgentMessageRole::Assistant, "cache warming answer"),
        ]);

        let output = run_within_search(
            &request("cache warming", None),
            &conv,
            &resolved,
            &transcript,
            &[],
        );
        let rendered = format_agent_output(&output);

        assert!(rendered.starts_with(
            "protocol agent-within mode=lexical cut=none chars=none policy=per-hit hits=1\n"
        ));
        assert!(rendered.contains("title project=pr_"));
        assert!(rendered.contains(&format!("uuid={TEST_UUID} ref=ch_")));
        assert!(rendered.contains(" | cache title"));
        assert!(rendered.contains("hit project=pr_"));
        assert!(rendered.contains(&format!("uuid={TEST_UUID} ref=ch_")));
        assert!(rendered.contains(" | cache warming answer"));
        assert!(rendered.contains("read ref=ch_"));
        assert!(rendered.contains("focus=m2..m2"));
    }

    #[test]
    fn invalid_session_filename_emits_uuid_none() {
        let conv = conversation("session.jsonl", "cache title");
        let resolved = resolved("session.jsonl");
        let transcript = transcript(vec![
            message(1, AgentMessageRole::User, "question"),
            message(2, AgentMessageRole::Assistant, "cache warming answer"),
        ]);

        let output = run_within_search(
            &request("cache warming", None),
            &conv,
            &resolved,
            &transcript,
            &[],
        );
        let rendered = format_agent_output(&output);

        assert!(rendered.contains("title project=pr_"));
        assert!(rendered.contains("uuid=none ref=ch_"));
        assert!(rendered.contains("hit project=pr_"));
    }

    #[test]
    fn within_semantic_returns_message_level_hits_with_context_recipes() {
        let mut conv = conversation(&format!("{TEST_UUID}.jsonl"), "semantic title");
        conv.message_count = 4;
        let resolved = resolved(&format!("{TEST_UUID}.jsonl"));
        let transcript = transcript(vec![message(1, AgentMessageRole::User, "placeholder")]);
        let output = run_within_search(
            &request("semantic", Some(SearchMode::Semantic)),
            &conv,
            &resolved,
            &transcript,
            &[
                semantic_hit(0, MessageRange::single(1), "first", 0.8),
                semantic_hit(0, MessageRange::single(3), "third", 0.7),
            ],
        );

        assert_eq!(output.hits.len(), 2);
        assert_eq!(output.hits[0].focus_range, MessageRange::single(1));
        assert_eq!(output.hits[0].read_range, MessageRange { start: 1, end: 2 });
        assert_eq!(output.hits[1].focus_range, MessageRange::single(3));
        assert_eq!(output.hits[1].read_range, MessageRange { start: 2, end: 4 });
    }

    #[test]
    fn semantic_visible_multi_turn_range_does_not_enable_subagents() {
        let conv = conversation("session.jsonl", "semantic title");
        let resolved = resolved("session.jsonl");
        let hits = semantic_output_hits(
            &[semantic_hit(
                0,
                MessageRange { start: 1, end: 1 },
                "first",
                0.8,
            )],
            1,
            &[AgentConversationInput {
                conversation: &conv,
                resolved,
                original_index: 0,
            }],
        );

        assert!(!hits[0].render_options.subagents);
    }

    #[test]
    fn semantic_progress_source_enables_subagents_for_mixed_range() {
        let conv = conversation("session.jsonl", "semantic title");
        let resolved = resolved("session.jsonl");
        let hits = semantic_output_hits(
            &[semantic_hit_with_source(
                0,
                MessageRange { start: 2, end: 4 },
                "subagent",
                0.8,
                SemanticChunkSource::AgentSubagentDialogue,
            )],
            1,
            &[AgentConversationInput {
                conversation: &conv,
                resolved,
                original_index: 0,
            }],
        );

        assert!(hits[0].render_options.subagents);
    }

    #[test]
    fn semantic_tool_and_thinking_sources_emit_matching_read_policy() {
        let tool = semantic_render_options(SemanticChunkSource::AgentTool);
        assert!(tool.tools);
        assert!(tool.tool_results);
        assert!(!tool.thinking);
        assert!(!tool.subagents);
        assert_eq!(
            semantic_evidence_source(SemanticChunkSource::AgentTool),
            AgentHitSource::Tool
        );

        let thinking = semantic_render_options(SemanticChunkSource::AgentSubagentThinking);
        assert!(!thinking.tools);
        assert!(!thinking.tool_results);
        assert!(thinking.thinking);
        assert!(thinking.subagents);
        assert_eq!(
            semantic_evidence_source(SemanticChunkSource::AgentSubagentThinking),
            AgentHitSource::Thinking
        );
    }

    #[test]
    fn semantic_hits_use_shared_evidence_format_without_changing_recipes() {
        let mut conv = conversation("session.jsonl", "semantic title");
        conv.message_count = 5;
        let resolved = resolved("session.jsonl");
        let raw = format!("semantic\n\u{1b}[31m{} tail", "🙂 ".repeat(200));

        let hits = semantic_output_hits(
            &[semantic_hit_with_source(
                0,
                MessageRange::single(3),
                &raw,
                0.8,
                SemanticChunkSource::AgentSubagentTool,
            )],
            1,
            &[AgentConversationInput {
                conversation: &conv,
                resolved,
                original_index: 0,
            }],
        );

        assert_eq!(hits[0].preview, format_evidence_preview(&raw));
        assert_eq!(hits[0].preview.chars().count(), 160);
        assert_eq!(hits[0].evidence_source, AgentHitSource::Tool);
        assert_eq!(hits[0].focus_range, MessageRange::single(3));
        assert_eq!(hits[0].read_range, MessageRange { start: 2, end: 4 });
        assert!(hits[0].render_options.tools);
        assert!(hits[0].render_options.tool_results);
        assert!(hits[0].render_options.subagents);
    }

    #[test]
    fn hybrid_dedupes_same_focus_and_prefers_lexical_preview() {
        let lexical = vec![lexical_dialogue_hit(
            "ch_123456789abc",
            "title",
            10.0,
            "lexical preview",
            MessageRange::single(2),
            MessageRange { start: 1, end: 3 },
        )];
        let semantic = vec![semantic_dialogue_hit(
            "ch_123456789abc",
            "title",
            0.9,
            "semantic preview",
            MessageRange::single(2),
            MessageRange::single(2),
        )];

        let hits = hybrid_hits(lexical, semantic, 10);

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source, AgentHitKind::Hybrid);
        assert_eq!(hits[0].preview, "lexical preview");
        assert_eq!(hits[0].semantic_score_breakdown.unwrap().semantic, 0.9);
        assert_eq!(hits[0].read_range, MessageRange { start: 1, end: 3 });
    }

    #[test]
    fn hybrid_fuses_different_evidence_ranges_by_conversation() {
        let lexical = vec![
            lexical_tool_hit(
                "ch_aaaaaaaaaaaa",
                "lexical only",
                10.0,
                "lexical only preview",
                MessageRange::single(5),
                MessageRange::single(5),
            ),
            lexical_tool_hit(
                "ch_bbbbbbbbbbbb",
                "reinforced",
                9.0,
                "tool preview",
                MessageRange::single(7),
                MessageRange::single(7),
            ),
        ];
        let semantic = vec![semantic_dialogue_hit(
            "ch_bbbbbbbbbbbb",
            "reinforced",
            0.9,
            "dialogue preview",
            MessageRange::single(2),
            MessageRange::single(2),
        )];

        let hits = hybrid_hits(lexical, semantic, 10);

        assert_eq!(hits[0].conversation_ref, "ch_bbbbbbbbbbbb");
        assert_eq!(hits[0].focus_range, MessageRange::single(7));
        assert!(
            hits.iter()
                .any(|hit| hit.conversation_ref == "ch_bbbbbbbbbbbb"
                    && hit.focus_range == MessageRange::single(2))
        );
    }

    #[test]
    fn hybrid_preserves_tool_render_options() {
        let lexical = vec![AgentOutputHit {
            render_options: AgentHitRenderOptions {
                tool_results: true,
                ..AgentHitRenderOptions::default()
            },
            ..lexical_tool_hit(
                "ch_123456789abc",
                "title",
                10.0,
                "tool preview",
                MessageRange::single(2),
                MessageRange { start: 1, end: 3 },
            )
        }];
        let semantic = vec![semantic_dialogue_hit(
            "ch_123456789abc",
            "title",
            0.9,
            "semantic preview",
            MessageRange::single(2),
            MessageRange::single(2),
        )];

        let rendered = format_agent_output(&AgentSearchOutput {
            protocol: AgentProtocolKind::Within,
            target: None,
            query: "needle".to_string(),
            mode: SearchMode::Hybrid,
            hits: hybrid_hits(lexical, semantic, 10),
            groups: vec![],
            flat: true,
            budget: None,
            stats: AgentSearchStats::default(),
        });

        assert!(rendered.contains("hit project=pr_test uuid=12345678-1234-4234-9234-123456789abc ref=ch_123456789abc anchors=ma_0000000000000000 source=tool"));
        assert!(
            rendered.contains("read ref=ch_123456789abc:m1..m3 focus=m2..m2 tools=false tool-results=true thinking=false subagents=false")
        );
    }

    #[test]
    fn grouped_search_caps_hits_per_conversation_and_prefers_dialogue_bucket() {
        let group = build_conversation_groups(
            vec![
                lexical_tool_hit(
                    "ch_a",
                    "title a",
                    10.02,
                    "tool evidence",
                    MessageRange::single(2),
                    MessageRange::single(2),
                ),
                lexical_dialogue_hit(
                    "ch_a",
                    "title a",
                    10.01,
                    "dialogue evidence",
                    MessageRange::single(1),
                    MessageRange::single(1),
                ),
                lexical_dialogue_hit(
                    "ch_a",
                    "title a",
                    9.0,
                    "lower evidence",
                    MessageRange::single(3),
                    MessageRange::single(3),
                ),
            ],
            10,
            2,
            false,
        )
        .pop()
        .unwrap();

        assert_eq!(group.total_hits, 3);
        assert_eq!(group.hits.len(), 2);
        assert_eq!(group.hits[0].preview, "dialogue evidence");
        assert_eq!(group.hits[1].preview, "tool evidence");
    }

    #[test]
    fn grouped_search_keeps_higher_bucket_tool_before_dialogue() {
        let group = build_conversation_groups(
            vec![
                lexical_tool_hit(
                    "ch_a",
                    "title a",
                    10.9,
                    "tool evidence",
                    MessageRange::single(2),
                    MessageRange::single(2),
                ),
                lexical_dialogue_hit(
                    "ch_a",
                    "title a",
                    10.1,
                    "dialogue evidence",
                    MessageRange::single(1),
                    MessageRange::single(1),
                ),
            ],
            10,
            2,
            false,
        )
        .pop()
        .unwrap();

        assert_eq!(group.hits[0].preview, "tool evidence");
    }

    #[test]
    fn grouped_search_preserves_duplicate_previews_at_distinct_messages() {
        let hit = |focus| AgentOutputHit {
            conversation_ref: "ch_a".to_string(),
            project_id: "pr_test".to_string(),
            conversation_uuid: "uuid-a".to_string(),
            session: "session.jsonl".to_string(),
            anchors: vec!["ma_0000000000000000".to_string()],
            title: "title a".to_string(),
            score: 10.0,
            evidence_score: 10.0,
            semantic_score_breakdown: None,
            source: AgentHitKind::Lexical,
            evidence_source: AgentHitSource::Tool,
            render_options: AgentHitRenderOptions {
                tool_results: true,
                ..AgentHitRenderOptions::default()
            },
            preview: "The file /tmp/a has been updated successfully.".to_string(),
            focus_range: MessageRange::single(focus),
            read_range: MessageRange::single(focus),
        };

        let groups = build_conversation_groups(vec![hit(1), hit(2)], 10, 10, false);

        assert_eq!(groups[0].hits.len(), 2);
        assert_eq!(groups[0].hits[0].focus_range, MessageRange::single(1));
        assert_eq!(groups[0].hits[1].focus_range, MessageRange::single(2));
    }

    #[test]
    fn grouped_search_suppresses_same_source_position() {
        let hit = lexical_dialogue_hit(
            "ch_a",
            "title a",
            10.0,
            "same evidence",
            MessageRange::single(1),
            MessageRange::single(1),
        );

        let deduped = build_conversation_groups(vec![hit.clone(), hit.clone()], 10, 10, false);
        let all = build_conversation_groups(vec![hit.clone(), hit], 10, 10, true);

        assert_eq!(deduped[0].hits.len(), 1);
        assert_eq!(all[0].hits.len(), 2);
    }

    #[test]
    fn global_grouped_output_uses_pipe_snippets() {
        let output = AgentSearchOutput {
            protocol: AgentProtocolKind::Search,
            target: None,
            query: "cache warming".to_string(),
            mode: SearchMode::Lexical,
            hits: vec![],
            groups: vec![AgentConversationGroup {
                conversation_ref: "ch_1234abcd5678".to_string(),
                project_id: "pr_test".to_string(),
                conversation_uuid: "12345678-1234-4234-9234-123456789abc".to_string(),
                session: "session.jsonl".to_string(),
                title: "cache session".to_string(),
                score: 12.5,
                total_hits: 3,
                hits: vec![lexical_dialogue_hit(
                    "ch_1234abcd5678",
                    "cache session",
                    12.5,
                    "cache warming answer",
                    MessageRange::single(2),
                    MessageRange { start: 1, end: 3 },
                )],
            }],
            flat: false,
            budget: None,
            stats: AgentSearchStats::default(),
        };

        let rendered = format_agent_output(&output);

        assert!(rendered.starts_with("protocol agent-search mode=lexical cut=none chars=none policy=per-hit groups=1 hits=1\n"));
        assert!(rendered.contains("conversation rank=1 project=pr_test uuid=12345678-1234-4234-9234-123456789abc ref=ch_1234abcd5678 score=12.500000"));
        assert!(rendered.contains("hit project=pr_test uuid=12345678-1234-4234-9234-123456789abc ref=ch_1234abcd5678 anchors=ma_0000000000000000 source=lexical"));
        assert!(rendered.contains("read ref=ch_1234abcd5678:m1..m3 focus=m2..m2 tools=false tool-results=false thinking=false subagents=false\n"));
        assert!(!rendered.contains("semantic="));
        assert!(!rendered.contains("hybrid="));
        assert!(!rendered.contains("lexical="));
        assert!(!rendered.contains("preview="));
        assert!(!rendered.contains("title ref=ch_1234abcd5678 text="));
    }

    #[test]
    fn grouped_search_ranks_groups_by_best_retained_display_hit() {
        let groups = build_conversation_groups(
            vec![
                lexical_tool_hit(
                    "ch_a",
                    "title a",
                    10.09,
                    "best tool evidence",
                    MessageRange::single(2),
                    MessageRange::single(2),
                ),
                lexical_dialogue_hit(
                    "ch_a",
                    "title a",
                    10.01,
                    "display dialogue evidence",
                    MessageRange::single(1),
                    MessageRange::single(1),
                ),
                lexical_dialogue_hit(
                    "ch_b",
                    "title b",
                    10.05,
                    "other dialogue evidence",
                    MessageRange::single(1),
                    MessageRange::single(1),
                ),
            ],
            2,
            2,
            false,
        );

        assert_eq!(groups[0].conversation_ref, "ch_b");
        assert_eq!(groups[0].score, 10.05);
        assert_eq!(groups[1].hits[0].preview, "display dialogue evidence");
    }

    #[test]
    fn global_flat_output_uses_output_hits_not_group_order() {
        let first = lexical_dialogue_hit(
            "ch_a",
            "title a",
            12.0,
            "first flat hit",
            MessageRange::single(1),
            MessageRange::single(1),
        );
        let second = lexical_dialogue_hit(
            "ch_b",
            "title b",
            11.0,
            "second flat hit",
            MessageRange::single(1),
            MessageRange::single(1),
        );
        let output = AgentSearchOutput {
            protocol: AgentProtocolKind::Search,
            target: None,
            query: "cache warming".to_string(),
            mode: SearchMode::Lexical,
            hits: vec![first.clone()],
            groups: vec![AgentConversationGroup {
                conversation_ref: "ch_b".to_string(),
                project_id: "pr_test".to_string(),
                conversation_uuid: "uuid-b".to_string(),
                session: "session.jsonl".to_string(),
                title: "title b".to_string(),
                score: 11.0,
                total_hits: 1,
                hits: vec![second],
            }],
            flat: true,
            budget: None,
            stats: AgentSearchStats::default(),
        };

        let rendered = format_agent_output(&output);

        assert!(rendered.starts_with(
            "protocol agent-search mode=lexical cut=none chars=none policy=per-hit hits=1\n"
        ));
        assert!(!rendered.contains("conversation rank="));
        assert!(rendered.contains(
            "title project=pr_test uuid=12345678-1234-4234-9234-123456789abc ref=ch_a | title a\n"
        ));
        assert!(rendered.contains("first flat hit"));
        assert!(!rendered.contains("second flat hit"));
    }

    #[test]
    fn search_output_has_a_hard_character_budget_and_atomic_recipes() {
        let hits = (1..=20)
            .map(|ordinal| {
                lexical_dialogue_hit(
                    "ch_1234abcd5678",
                    "title",
                    20.0 - ordinal as f64,
                    &format!("hit {ordinal} {}", "x".repeat(200)),
                    MessageRange::single(ordinal),
                    MessageRange::single(ordinal),
                )
            })
            .collect::<Vec<_>>();
        let output = AgentSearchOutput {
            protocol: AgentProtocolKind::Within,
            target: None,
            query: "needle".to_string(),
            mode: SearchMode::Lexical,
            hits,
            groups: vec![],
            flat: true,
            budget: Some(500),
            stats: AgentSearchStats::default(),
        };

        let rendered = format_agent_output(&output);

        assert!(rendered.chars().count() <= 500);
        assert!(
            rendered.starts_with(
                "protocol agent-within mode=lexical cut=tail chars=500 policy=per-hit"
            )
        );
        assert!(rendered.contains("omitted-lines="));
        assert_eq!(
            rendered
                .lines()
                .filter(|line| line.starts_with("hit "))
                .count(),
            rendered
                .lines()
                .filter(|line| line.starts_with("read "))
                .count()
        );
    }

    #[test]
    fn search_sanitizes_previews_and_declares_recipe_visibility() {
        let output = AgentSearchOutput {
            protocol: AgentProtocolKind::Within,
            target: None,
            query: "needle".to_string(),
            mode: SearchMode::Lexical,
            hits: vec![AgentOutputHit {
                render_options: AgentHitRenderOptions {
                    tool_results: true,
                    ..AgentHitRenderOptions::default()
                },
                ..lexical_tool_hit(
                    "ch_1234abcd5678",
                    "safe\u{1b}[31mtitle",
                    1.0,
                    "tool\u{1b}]0;title\u{7} result",
                    MessageRange::single(1),
                    MessageRange::single(1),
                )
            }],
            groups: vec![],
            flat: true,
            budget: None,
            stats: AgentSearchStats::default(),
        };

        let rendered = format_agent_output(&output);

        assert!(!rendered.contains('\u{1b}'));
        assert!(rendered.contains("| safetitle"));
        assert!(rendered.contains("| tool result"));
        assert!(rendered.contains("tools=false tool-results=true thinking=false subagents=false"));
    }

    #[test]
    fn semantic_routes_rank_conversations_without_becoming_evidence() {
        let conv_a = conversation("a.jsonl", "title a");
        let conv_b = conversation("b.jsonl", "title b");
        let input_a = AgentConversationInput {
            conversation: &conv_a,
            resolved: resolved("a.jsonl"),
            original_index: 0,
        };
        let input_b = AgentConversationInput {
            conversation: &conv_b,
            resolved: resolved("b.jsonl"),
            original_index: 1,
        };
        let request = AgentSearchRequest {
            query: ParsedQuery::parse("semantic"),
            mode: SearchMode::Semantic,
            top: 2,
            flat: false,
            hits_per_conversation: 1,
            retrieval_hits_per_conversation: None,
            all_hits: false,
            budget: None,
        };
        let hits = vec![
            semantic_hit_with_source(
                1,
                MessageRange::single(1),
                "synthetic route",
                1.0,
                SemanticChunkSource::AgentRoute,
            ),
            semantic_hit(0, MessageRange::single(2), "evidence a", 0.9),
            semantic_hit(1, MessageRange::single(3), "evidence b", 0.8),
        ];

        let output = run_global_semantic_search(&request, &[input_a, input_b], &hits);

        assert_eq!(
            output.groups[0].conversation_ref,
            resolved("b.jsonl").reference.canonical()
        );
        assert_eq!(output.groups[0].hits[0].preview, "evidence b");
        assert_eq!(
            output.groups[0].hits[0]
                .semantic_score_breakdown
                .unwrap()
                .semantic,
            0.8
        );
        assert!(
            format_agent_output(&output)
                .lines()
                .find(|line| line.starts_with("conversation rank=1 "))
                .unwrap()
                .contains("semantic=0.800000")
        );
        assert!(
            output
                .groups
                .iter()
                .flat_map(|group| &group.hits)
                .all(|hit| hit.preview != "synthetic route")
        );
    }

    #[test]
    fn grouped_semantic_search_collects_until_top_conversations() {
        let conv_a = conversation("a.jsonl", "title a");
        let conv_b = conversation("b.jsonl", "title b");
        let input_a = AgentConversationInput {
            conversation: &conv_a,
            resolved: resolved("a.jsonl"),
            original_index: 0,
        };
        let input_b = AgentConversationInput {
            conversation: &conv_b,
            resolved: resolved("b.jsonl"),
            original_index: 1,
        };
        let request = AgentSearchRequest {
            query: ParsedQuery::parse("semantic"),
            mode: SearchMode::Semantic,
            top: 2,
            flat: false,
            hits_per_conversation: 2,
            retrieval_hits_per_conversation: None,
            all_hits: false,
            budget: None,
        };
        let mut hits = (1..=20)
            .map(|index| semantic_hit(0, MessageRange::single(index), "first", 1.0))
            .collect::<Vec<_>>();
        hits.push(semantic_hit(1, MessageRange::single(1), "second", 0.1));
        let expected = vec![
            input_a.resolved.reference.canonical(),
            input_b.resolved.reference.canonical(),
        ];

        let output = run_global_semantic_search(&request, &[input_a, input_b], &hits);

        assert_eq!(
            output
                .groups
                .iter()
                .map(|group| group.conversation_ref.clone())
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn duplicate_uuid_search_records_keep_project_identity() {
        let filename = format!("{TEST_UUID}.jsonl");
        let conversations = vec![
            conversation(&format!("project-a/{filename}"), "needle a"),
            conversation(&format!("project-b/{filename}"), "needle b"),
        ];
        let keys = vec![
            AgentConversationKey::new(
                "project-a",
                &filename,
                PathBuf::from(format!("project-a/{filename}")),
            ),
            AgentConversationKey::new(
                "project-b",
                &filename,
                PathBuf::from(format!("project-b/{filename}")),
            ),
        ];
        let request = global_request("needle", SearchMode::Lexical, 2, false);

        let output = run_global_lexical_search(&request, &conversations, &keys, &[0, 1], |_| {
            Ok(transcript(vec![message(
                1,
                AgentMessageRole::User,
                "needle evidence",
            )]))
        })
        .unwrap();

        assert_eq!(output.groups.len(), 2);
        assert_eq!(
            output.groups[0].conversation_uuid,
            output.groups[1].conversation_uuid
        );
        assert_ne!(output.groups[0].project_id, output.groups[1].project_id);
        assert_ne!(
            output.groups[0].conversation_ref,
            output.groups[1].conversation_ref
        );
    }

    #[test]
    fn global_lexical_loads_only_bounded_shortlist_for_evidence() {
        let conversations = (0..60)
            .map(|index| conversation(&format!("session-{index}.jsonl"), "needle title"))
            .collect::<Vec<_>>();
        let keys = conversations
            .iter()
            .map(|conversation| {
                AgentConversationKey::new(
                    "project-a",
                    conversation.path.file_name().unwrap().to_string_lossy(),
                    conversation.path.clone(),
                )
            })
            .collect::<Vec<_>>();
        let ranked = (0..60).collect::<Vec<_>>();
        let request = global_request("needle", SearchMode::Lexical, 3, false);

        let output = run_global_lexical_search(&request, &conversations, &keys, &ranked, |_| {
            Ok(transcript(vec![message(
                1,
                AgentMessageRole::User,
                "needle evidence",
            )]))
        })
        .unwrap();

        assert_eq!(output.hits.len(), 3);
        assert_eq!(output.stats.shortlisted, 50);
        assert_eq!(output.stats.transcripts_loaded, 3);
    }

    #[test]
    fn flat_top_counts_ranked_message_hits_and_keeps_same_conversation_hits() {
        let conversations = vec![conversation("session.jsonl", "needle title")];
        let keys = vec![AgentConversationKey::new(
            "project-a",
            "session.jsonl",
            PathBuf::from("session.jsonl"),
        )];
        let request = global_request("needle", SearchMode::Lexical, 2, true);

        let output = run_global_lexical_search(&request, &conversations, &keys, &[0], |_| {
            Ok(transcript(vec![
                message(1, AgentMessageRole::User, "needle one"),
                message(2, AgentMessageRole::User, "needle two"),
                message(3, AgentMessageRole::User, "needle three"),
            ]))
        })
        .unwrap();

        assert_eq!(output.hits.len(), 2);
        assert!(output.groups.is_empty());
        assert_eq!(
            output.hits[0].conversation_ref,
            output.hits[1].conversation_ref
        );
        assert_ne!(output.hits[0].focus_range, output.hits[1].focus_range);
    }

    #[test]
    fn hybrid_lexical_candidates_bound_evidence_per_conversation() {
        let conversations = vec![
            conversation("a.jsonl", "needle a"),
            conversation("b.jsonl", "needle b"),
            conversation("c.jsonl", "needle c"),
        ];
        let keys = conversations
            .iter()
            .map(|conversation| {
                AgentConversationKey::new(
                    "project-a",
                    conversation.path.file_name().unwrap().to_string_lossy(),
                    conversation.path.clone(),
                )
            })
            .collect::<Vec<_>>();
        let mut request = global_request("needle", SearchMode::Lexical, 2, true);
        request.retrieval_hits_per_conversation = Some(1);

        let output = run_global_lexical_search(&request, &conversations, &keys, &[0, 1, 2], |_| {
            Ok(transcript(vec![
                message(1, AgentMessageRole::User, "needle one"),
                message(2, AgentMessageRole::User, "needle two"),
            ]))
        })
        .unwrap();

        assert_eq!(output.hits.len(), 2);
        assert_eq!(output.stats.transcripts_loaded, 2);
        assert_ne!(
            output.hits[0].conversation_ref,
            output.hits[1].conversation_ref
        );
    }

    #[test]
    fn grouped_top_counts_conversations_and_keeps_per_conversation_hits() {
        let conversations = vec![
            conversation("a.jsonl", "needle a"),
            conversation("b.jsonl", "needle b"),
        ];
        let keys = conversations
            .iter()
            .map(|conversation| {
                AgentConversationKey::new(
                    "project-a",
                    conversation.path.file_name().unwrap().to_string_lossy(),
                    conversation.path.clone(),
                )
            })
            .collect::<Vec<_>>();
        let request = global_request("needle", SearchMode::Lexical, 1, false);

        let output = run_global_lexical_search(&request, &conversations, &keys, &[0, 1], |_| {
            Ok(transcript(vec![
                message(1, AgentMessageRole::User, "needle one"),
                message(2, AgentMessageRole::User, "needle two"),
            ]))
        })
        .unwrap();

        assert_eq!(output.groups.len(), 1);
        assert_eq!(output.groups[0].hits.len(), 2);
        assert_eq!(output.stats.transcripts_loaded, 1);
    }

    #[test]
    fn hybrid_keeps_semantic_only_candidate_outside_lexical_candidates() {
        let conv_a = conversation("a.jsonl", "title a");
        let conv_b = conversation("b.jsonl", "title b");
        let inputs = vec![
            AgentConversationInput {
                conversation: &conv_a,
                resolved: resolved("a.jsonl"),
                original_index: 0,
            },
            AgentConversationInput {
                conversation: &conv_b,
                resolved: resolved("b.jsonl"),
                original_index: 1,
            },
        ];
        let lexical_hit = lexical_dialogue_hit(
            &inputs[0].resolved.reference.canonical(),
            "title a",
            10.0,
            "literal candidate",
            MessageRange::single(1),
            MessageRange::single(1),
        );
        let lexical = AgentSearchOutput {
            protocol: AgentProtocolKind::Search,
            target: None,
            query: "concept".to_string(),
            mode: SearchMode::Lexical,
            hits: vec![lexical_hit],
            groups: Vec::new(),
            flat: true,
            budget: None,
            stats: AgentSearchStats::default(),
        };
        let request = global_request("concept", SearchMode::Hybrid, 2, false);

        let output = run_global_hybrid_search(
            &request,
            lexical,
            &[semantic_hit(
                1,
                MessageRange::single(4),
                "conceptual candidate",
                0.9,
            )],
            &inputs,
        );

        assert_eq!(output.groups.len(), 2);
        assert!(
            output
                .groups
                .iter()
                .any(|group| group.conversation_ref == inputs[1].resolved.reference.canonical())
        );
    }

    #[test]
    fn hybrid_fuses_identity_before_ordering_distinct_hits() {
        let lexical = vec![
            lexical_dialogue_hit(
                "ch_a",
                "title",
                10.0,
                "same preview",
                MessageRange::single(1),
                MessageRange::single(1),
            ),
            lexical_dialogue_hit(
                "ch_a",
                "title",
                9.0,
                "same preview",
                MessageRange::single(2),
                MessageRange::single(2),
            ),
        ];
        let semantic = vec![semantic_dialogue_hit(
            "ch_a",
            "title",
            0.9,
            "semantic preview",
            MessageRange::single(1),
            MessageRange::single(1),
        )];

        let hits = hybrid_hits(lexical, semantic, 10);

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].source, AgentHitKind::Hybrid);
        assert_eq!(hits[0].focus_range, MessageRange::single(1));
        assert_eq!(hits[1].focus_range, MessageRange::single(2));
    }

    #[test]
    fn flat_unicode_budget_preserves_visibility_recipes() {
        let mut request = global_request("needle", SearchMode::Lexical, 2, true);
        request.budget = Some(900);
        let candidate_hits = (1..=2)
            .map(|ordinal| AgentOutputHit {
                semantic_score_breakdown: Some(SemanticScoreBreakdown {
                    hybrid: 0.969,
                    semantic: 0.769,
                    lexical: 0.2,
                }),
                source: AgentHitKind::Hybrid,
                render_options: AgentHitRenderOptions {
                    tool_results: true,
                    ..AgentHitRenderOptions::default()
                },
                ..lexical_tool_hit(
                    "ch_a",
                    "unicode title",
                    2.0 - ordinal as f64,
                    &"🙂".repeat(300),
                    MessageRange::single(ordinal),
                    MessageRange::single(ordinal),
                )
            })
            .collect();
        let (hits, groups) = finalize_global_hits(candidate_hits, &request);
        let rendered = format_agent_output(&AgentSearchOutput {
            protocol: AgentProtocolKind::Search,
            target: None,
            query: request.query.raw().to_string(),
            mode: SearchMode::Lexical,
            hits,
            groups,
            flat: true,
            budget: request.budget,
            stats: AgentSearchStats::default(),
        });

        assert!(rendered.chars().count() <= 900);
        assert!(rendered.contains("cut=tail"));
        assert!(rendered.contains("tool-results=true"));
        assert!(rendered.contains("semantic=0.769000 lexical=0.200000"));
        assert_eq!(
            rendered
                .lines()
                .filter(|line| line.starts_with("hit "))
                .count(),
            rendered
                .lines()
                .filter(|line| line.starts_with("read "))
                .count()
        );
    }
}
