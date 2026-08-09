use crate::history::Conversation;
use crate::search::literal::Literal;
use crate::search::query::ParsedQuery;
use crate::text_match::normalize_for_search;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LexicalEvidence {
    pub context_ranges: Vec<(usize, usize)>,
}

#[derive(Clone, Copy, Debug)]
struct TermHit {
    start: usize,
    end: usize,
    term_idx: usize,
}

#[derive(Clone, Debug)]
struct HitCluster {
    start: usize,
    end: usize,
    unique_terms: u64,
    missing_terms: u64,
    adjacent_pairs: u32,
    last_hit_end: usize,
    last_term_idx: usize,
}

impl HitCluster {
    fn span(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    fn unique_count(&self) -> u32 {
        self.unique_terms.count_ones()
    }

    fn missing_count(&self) -> u32 {
        self.missing_terms.count_ones()
    }
}

#[derive(Clone)]
enum EvidenceSpec {
    Normalized(String),
    Literal(Literal),
}

impl EvidenceSpec {
    fn key(&self) -> (&str, bool) {
        match self {
            Self::Normalized(term) => (term.as_str(), false),
            Self::Literal(literal) => (literal.text(), true),
        }
    }

    fn has_match(&self, text: &str) -> bool {
        match self {
            Self::Normalized(term) => find_first_normalized_match(text, term).is_some(),
            Self::Literal(literal) => literal.matches(text),
        }
    }

    fn ranges(&self, text: &str) -> Vec<(usize, usize)> {
        match self {
            Self::Normalized(term) => find_normalized_term_ranges(text, term),
            Self::Literal(literal) => literal.match_ranges(text),
        }
    }
}

pub fn build_lexical_evidence(
    conversation: &Conversation,
    parsed: &ParsedQuery,
) -> Option<LexicalEvidence> {
    let specs = evidence_specs(parsed);
    if specs.is_empty() {
        return None;
    }

    let ranges = select_context_ranges(&conversation.full_text, &conversation.preview, &specs)?;
    Some(LexicalEvidence {
        context_ranges: ranges,
    })
}

fn evidence_specs(parsed: &ParsedQuery) -> Vec<EvidenceSpec> {
    let unquoted = parsed.unquoted();
    let unquoted_terms = unquoted.split_whitespace().collect::<Vec<_>>();
    let identifier_literals = unquoted_terms
        .iter()
        .copied()
        .filter(|term| term.contains('_'))
        .map(|term| EvidenceSpec::Literal(Literal::new(term.to_string())));
    let normalized_query = unquoted_terms
        .iter()
        .copied()
        .filter(|term| !term.contains('_'))
        .collect::<Vec<_>>()
        .join(" ");
    let normalized = normalize_for_search(&normalized_query)
        .split_whitespace()
        .map(|term| EvidenceSpec::Normalized(term.to_string()))
        .collect::<Vec<_>>();

    dedupe_specs(
        &normalized
            .into_iter()
            .chain(identifier_literals)
            .chain(parsed.literals().iter().cloned().map(EvidenceSpec::Literal))
            .collect::<Vec<_>>(),
    )
}

fn dedupe_specs(specs: &[EvidenceSpec]) -> Vec<EvidenceSpec> {
    let mut deduped = Vec::new();
    for spec in specs {
        let (text, is_literal) = spec.key();
        if !deduped.iter().any(|existing: &EvidenceSpec| {
            let (existing_text, existing_is_literal) = existing.key();
            existing_is_literal == is_literal && existing_text.eq_ignore_ascii_case(text)
        }) {
            deduped.push(spec.clone());
            if deduped.len() == 64 {
                break;
            }
        }
    }
    deduped
}

fn select_context_ranges(
    full_text: &str,
    preview: &str,
    specs: &[EvidenceSpec],
) -> Option<Vec<(usize, usize)>> {
    let mut missing_mask: u64 = 0;
    let mut missing_count = 0u32;
    for (i, spec) in specs.iter().enumerate() {
        if !spec.has_match(preview) {
            missing_mask |= 1 << i;
            missing_count += 1;
        }
    }

    let all_hits = find_all_spec_hits(full_text, specs);
    if all_hits.is_empty() {
        return None;
    }

    if missing_count == 0 {
        let preview_hit_count = find_all_spec_hits(preview, specs).len();
        if all_hits.len() <= preview_hit_count {
            return None;
        }
    }

    let hit_spans: Vec<(usize, usize, usize)> = all_hits
        .iter()
        .map(|hit| (hit.start, hit.end, hit.term_idx))
        .collect();
    select_hidden_context_ranges(full_text, &hit_spans, missing_mask, missing_count)
}

pub(crate) fn select_hidden_context_ranges(
    full_text: &str,
    hits: &[(usize, usize, usize)],
    missing_mask: u64,
    missing_count: u32,
) -> Option<Vec<(usize, usize)>> {
    let merge_gap_bytes: usize = 50;
    let max_cluster_span_bytes: usize = 200;
    let max_clusters: usize = 3;

    if hits.is_empty() {
        return None;
    }

    let mut clusters: Vec<HitCluster> = Vec::new();
    for &(start, end, term_idx) in hits {
        let term_bit: u64 = 1u64 << term_idx;
        let is_missing = (missing_mask & term_bit) != 0;
        let mut extended = false;

        if let Some(last) = clusters.last_mut() {
            let close_enough = start <= last.end.saturating_add(merge_gap_bytes);
            let new_end = last.end.max(end);
            let new_span = new_end.saturating_sub(last.start);
            if close_enough && new_span <= max_cluster_span_bytes {
                if term_idx != last.last_term_idx && start >= last.last_hit_end {
                    let gap = &full_text[last.last_hit_end..start];
                    if !gap.is_empty() && gap.chars().all(|c| !c.is_alphanumeric()) {
                        last.adjacent_pairs += 1;
                    }
                }

                last.end = new_end;
                last.unique_terms |= term_bit;
                if is_missing {
                    last.missing_terms |= term_bit;
                }
                last.last_hit_end = end;
                last.last_term_idx = term_idx;
                extended = true;
            }
        }

        if !extended {
            clusters.push(HitCluster {
                start,
                end,
                unique_terms: term_bit,
                missing_terms: if is_missing { term_bit } else { 0 },
                adjacent_pairs: 0,
                last_hit_end: end,
                last_term_idx: term_idx,
            });
        }
    }

    if missing_count > 0 {
        clusters.retain(|cluster| cluster.missing_count() > 0);
    }
    if clusters.is_empty() {
        return None;
    }

    clusters.sort_unstable_by(|a, b| {
        b.missing_count()
            .cmp(&a.missing_count())
            .then_with(|| b.adjacent_pairs.cmp(&a.adjacent_pairs))
            .then_with(|| b.unique_count().cmp(&a.unique_count()))
            .then_with(|| a.span().cmp(&b.span()))
            .then_with(|| a.start.cmp(&b.start))
    });

    let mut selected: Vec<HitCluster> = Vec::new();
    let mut covered_missing: u64 = 0;

    for cluster in &clusters {
        if selected.len() >= max_clusters {
            break;
        }
        let new_missing = cluster.missing_terms & !covered_missing;
        if new_missing != 0 {
            covered_missing |= cluster.missing_terms;
            selected.push(cluster.clone());
        }
    }

    for cluster in &clusters {
        if selected.len() >= max_clusters {
            break;
        }
        if !selected
            .iter()
            .any(|selected| selected.start == cluster.start && selected.end == cluster.end)
        {
            selected.push(cluster.clone());
        }
    }

    selected.sort_unstable_by_key(|cluster| cluster.start);
    Some(
        selected
            .into_iter()
            .map(|cluster| (cluster.start, cluster.end))
            .collect(),
    )
}

fn find_all_spec_hits(text: &str, specs: &[EvidenceSpec]) -> Vec<TermHit> {
    let mut all_hits = Vec::new();
    for (term_idx, spec) in specs.iter().enumerate() {
        all_hits.extend(spec.ranges(text).into_iter().map(|(start, end)| TermHit {
            start,
            end,
            term_idx,
        }));
    }
    all_hits.sort_unstable_by_key(|hit| hit.start);
    all_hits
}

fn find_first_normalized_match(text: &str, term: &str) -> Option<(usize, usize)> {
    find_normalized_term_ranges(text, term).into_iter().next()
}

fn find_normalized_term_ranges(text: &str, term: &str) -> Vec<(usize, usize)> {
    let term_chars: Vec<char> = term.chars().collect();
    if term_chars.is_empty() {
        return Vec::new();
    }

    let query_starts_alnum = term_chars[0].is_alphanumeric();
    let mut ranges = Vec::new();
    let mut prev_is_alnum = false;
    let mut iter = text.char_indices().peekable();

    while let Some(&(byte_start, ch)) = iter.peek() {
        let norm_ch = normalize_evidence_char(ch);
        let is_alnum = ch.is_alphanumeric();
        let valid_start = !query_starts_alnum || !prev_is_alnum;

        if valid_start && norm_ch == term_chars[0] {
            let mut lookahead = iter.clone();
            lookahead.next();
            let mut matched = true;
            let mut end_byte = byte_start + ch.len_utf8();

            for &query_char in term_chars.iter().skip(1) {
                if let Some(&(_, next_ch)) = lookahead.peek() {
                    let next_norm = normalize_evidence_char(next_ch);
                    end_byte += next_ch.len_utf8();
                    lookahead.next();
                    if next_norm != query_char {
                        matched = false;
                        break;
                    }
                } else {
                    matched = false;
                    break;
                }
            }

            if matched {
                ranges.push((byte_start, end_byte));
                for _ in 0..term_chars.len().saturating_sub(1) {
                    iter.next();
                }
                prev_is_alnum = term_chars.last().is_some_and(|c| c.is_alphanumeric());
                iter.next();
                continue;
            }
        }

        prev_is_alnum = is_alnum;
        iter.next();
    }

    ranges
}

fn normalize_evidence_char(ch: char) -> char {
    if ch == '_' || ch == '-' || ch == '/' {
        ' '
    } else {
        ch.to_lowercase().next().unwrap_or(ch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, TimeZone};
    use std::path::PathBuf;

    fn conversation(preview: &str, full_text: &str) -> Conversation {
        Conversation {
            path: PathBuf::from("/tmp/session.jsonl"),
            index: 0,
            timestamp: Local.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
            preview: preview.to_string(),
            preview_first: preview.to_string(),
            preview_last: preview.to_string(),
            full_text: full_text.to_string(),
            agent_search_text: String::new(),
            semantic_turns: vec![full_text.to_string()],
            semantic_turn_ranges: vec![crate::agent::refs::MessageRange::single(1)],
            search_text_lower: normalize_for_search(full_text),
            project_name: None,
            project_path: None,
            cwd: None,
            message_count: 1,
            parse_errors: Vec::new(),
            summary: None,
            custom_title: None,
            model: None,
            total_tokens: 0,
            duration_minutes: None,
            source_label: None,
        }
    }

    #[test]
    fn unquoted_body_match_produces_context_range() {
        let parsed = ParsedQuery::parse("deepgram");
        let conversation = conversation(
            "unrelated preview",
            "unrelated preview followed by hidden deepgram evidence",
        );

        let evidence = build_lexical_evidence(&conversation, &parsed).unwrap();

        assert_eq!(evidence.context_ranges.len(), 1);
        let (start, end) = evidence.context_ranges[0];
        assert!(conversation.full_text[start..end].contains("deepgram"));
    }

    #[test]
    fn quoted_literal_preserves_exact_punctuation() {
        let parsed = ParsedQuery::parse("\"audio_generation\"");
        let conversation = conversation(
            "audio generation normalized preview",
            "audio generation normalized preview and exact audio_generation evidence",
        );

        let evidence = build_lexical_evidence(&conversation, &parsed).unwrap();

        let (start, end) = evidence.context_ranges[0];
        assert_eq!(&conversation.full_text[start..end], "audio_generation");
    }

    #[test]
    fn unquoted_identifier_evidence_preserves_exact_punctuation() {
        let parsed = ParsedQuery::parse("api_key");
        let conversation = conversation(
            "api key normalized preview",
            "api key normalized preview and exact api_key evidence",
        );

        let evidence = build_lexical_evidence(&conversation, &parsed).unwrap();

        let snippets = evidence
            .context_ranges
            .iter()
            .map(|(start, end)| &conversation.full_text[*start..*end])
            .collect::<Vec<_>>();
        assert!(snippets.iter().any(|snippet| snippet.contains("api_key")));
    }

    #[test]
    fn mixed_query_can_surface_unquoted_and_literal_ranges() {
        let parsed = ParsedQuery::parse("hidden_unquoted \"exact_literal\"");
        let full_text = format!("hidden_unquoted {} exact_literal", "x ".repeat(120));
        let conversation = conversation("visible preview", &full_text);

        let evidence = build_lexical_evidence(&conversation, &parsed).unwrap();
        let snippets = evidence
            .context_ranges
            .iter()
            .map(|(start, end)| &conversation.full_text[*start..*end])
            .collect::<Vec<_>>();

        assert!(
            snippets
                .iter()
                .any(|snippet| snippet.contains("hidden_unquoted"))
        );
        assert!(
            snippets
                .iter()
                .any(|snippet| snippet.contains("exact_literal"))
        );
    }
}
