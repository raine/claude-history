use crate::error::{AppError, Result};
use crate::semantic::evidence::{
    evidence_preview, matched_terms_prepared, preview_is_identity, query_terms,
};
use crate::semantic::types::{
    EmbeddedChunk, SemanticChunkIdentity, SemanticExplanation, SemanticHit, SemanticQuality,
    SemanticRationaleKind, SemanticScoreBreakdown,
};
use crate::text_match::normalize_for_search;
use rayon::prelude::*;
use std::cmp::Ordering;
use std::collections::HashMap;
/// Query-independent derivations of a chunk, computed once (at residency or
/// on the fly) so that ranking never re-normalizes text per query.
///
/// `preview` is `None` when `evidence_preview` would return the text itself,
/// which is the common case for already-normalized semantic turns.
#[derive(Clone, Debug)]
pub struct PreparedText {
    lower: Box<str>,
    normalized: Box<str>,
    preview: Option<Box<str>>,
    norm: f32,
}

/// Query-side work hoisted out of the per-chunk loop.
impl PreparedText {
    pub fn of(chunk: &EmbeddedChunk) -> Self {
        let text = chunk.text.as_str();
        Self {
            lower: text.to_lowercase().into_boxed_str(),
            normalized: normalize_for_search(text).into_boxed_str(),
            preview: (!preview_is_identity(text)).then(|| evidence_preview(text).into_boxed_str()),
            norm: chunk.embedding.iter().map(|y| y * y).sum::<f32>().sqrt(),
        }
    }
}
struct QueryContext<'q> {
    embedding: &'q [f32],
    norm: f32,
    words_lower: Vec<String>,
    terms: Vec<String>,
}

impl<'q> QueryContext<'q> {
    fn new(query: &'q str, embedding: &'q [f32]) -> Self {
        Self {
            embedding,
            norm: embedding.iter().map(|x| x * x).sum::<f32>().sqrt(),
            words_lower: query
                .split_whitespace()
                .map(|word| word.to_lowercase())
                .collect(),
            terms: query_terms(query),
        }
    }
    fn score(&self, chunk: &EmbeddedChunk, prepared: &PreparedText) -> SemanticHit {
        let semantic_score =
            cosine_prepared(self.embedding, self.norm, &chunk.embedding, prepared.norm);
        let lexical_score = lexical_overlap_prepared(&self.words_lower, &prepared.lower);
        let score_breakdown = SemanticScoreBreakdown {
            hybrid: semantic_score + lexical_score,
            semantic: semantic_score,
            lexical: lexical_score,
        };
        let quality = quality_for_score(score_breakdown.hybrid);
        let explanation = SemanticExplanation {
            quality,
            quality_label: quality.label(),
            matched_terms: matched_terms_prepared(&self.terms, &prepared.normalized),
            evidence_preview: prepared
                .preview
                .as_deref()
                .unwrap_or(&chunk.text)
                .to_string(),
            rationale_kind: rationale_kind(score_breakdown),
            chunk: SemanticChunkIdentity {
                conversation_index: chunk.conversation_index,
                source: chunk.source,
                session: chunk.session.clone(),
                chunk_index: chunk.chunk_index,
                message_range: chunk.message_range,
            },
        };
        SemanticHit::new(score_breakdown, explanation)
    }
}

/// Ranks chunks that already carry their `PreparedText`. Scoring runs in
/// parallel (order-preserving) and the final sort is deterministic, so the
/// result is identical to the sequential one.
pub fn rank_prepared(
    query: &str,
    query_embedding: &[f32],
    chunks: &[(&EmbeddedChunk, &PreparedText)],
    cancellation: &crate::semantic::types::SemanticCancellationToken,
) -> Result<Vec<SemanticHit>> {
    let context = QueryContext::new(query, query_embedding);
    let mut hits = chunks
        .par_iter()
        .map(|(chunk, prepared)| {
            if cancellation.is_cancelled() {
                return Err(AppError::SemanticSearchCancelled);
            }
            Ok(context.score(chunk, prepared))
        })
        .collect::<Result<Vec<_>>>()?;
    hits.par_sort_by(compare_hits);
    Ok(hits)
}
/// Scores every chunk against the query and sorts. Takes the chunks by
/// reference and prepares their text on the fly; callers with a resident
/// index use `rank_prepared` and skip that.
pub fn rank_chunk_hits<'a>(
    query: &str,
    query_embedding: &[f32],
    chunks: impl IntoIterator<Item = &'a EmbeddedChunk>,
    cancellation: &crate::semantic::types::SemanticCancellationToken,
) -> Result<Vec<SemanticHit>> {
    let chunks: Vec<&EmbeddedChunk> = chunks.into_iter().collect();
    let prepared: Vec<PreparedText> = chunks
        .par_iter()
        .map(|chunk| PreparedText::of(chunk))
        .collect();
    let pairs: Vec<(&EmbeddedChunk, &PreparedText)> =
        chunks.iter().copied().zip(prepared.iter()).collect();
    rank_prepared(query, query_embedding, &pairs, cancellation)
}

pub fn rank_chunks<'a>(
    query: &str,
    query_embedding: &[f32],
    chunks: impl IntoIterator<Item = &'a EmbeddedChunk>,
    cancellation: &crate::semantic::types::SemanticCancellationToken,
) -> Result<Vec<SemanticHit>> {
    let hits = rank_chunk_hits(query, query_embedding, chunks, cancellation)?;
    Ok(rank_conversation_hits(&hits))
}

/// Best hit of each conversation, sorted — cloning only the winners.
pub fn rank_conversation_hits(hits: &[SemanticHit]) -> Vec<SemanticHit> {
    let mut best_index: HashMap<usize, usize> = HashMap::new();
    for (index, hit) in hits.iter().enumerate() {
        let replace = best_index
            .get(&hit.conversation_index)
            .is_none_or(|&existing| compare_hits(hit, &hits[existing]).is_lt());
        if replace {
            best_index.insert(hit.conversation_index, index);
        }
    }
    let mut winners: Vec<SemanticHit> = best_index
        .into_values()
        .map(|index| hits[index].clone())
        .collect();
    winners.sort_by(compare_hits);
    winners
}

fn cosine_prepared(query: &[f32], query_norm: f32, chunk: &[f32], chunk_norm: f32) -> f32 {
    if query_norm == 0.0 || chunk_norm == 0.0 {
        return 0.0;
    }
    let dot: f32 = query.iter().zip(chunk).map(|(x, y)| x * y).sum();
    dot / (query_norm * chunk_norm)
}

fn lexical_overlap_prepared(query_words_lower: &[String], text_lower: &str) -> f32 {
    if query_words_lower.is_empty() {
        return 0.0;
    }
    let matches = query_words_lower
        .iter()
        .filter(|word| text_lower.contains(word.as_str()))
        .count();
    0.2 * matches as f32 / query_words_lower.len() as f32
}
fn compare_hits(a: &SemanticHit, b: &SemanticHit) -> Ordering {
    b.score_breakdown
        .hybrid
        .total_cmp(&a.score_breakdown.hybrid)
        .then_with(|| {
            b.score_breakdown
                .semantic
                .total_cmp(&a.score_breakdown.semantic)
        })
        .then_with(|| {
            b.score_breakdown
                .lexical
                .total_cmp(&a.score_breakdown.lexical)
        })
        .then_with(|| a.conversation_index.cmp(&b.conversation_index))
        .then_with(|| a.session.cmp(&b.session))
        .then_with(|| a.chunk_index.cmp(&b.chunk_index))
}

fn quality_for_score(hybrid_score: f32) -> SemanticQuality {
    if hybrid_score >= 0.85 {
        SemanticQuality::Strong
    } else if hybrid_score >= 0.65 {
        SemanticQuality::Good
    } else if hybrid_score >= 0.35 {
        SemanticQuality::Fair
    } else {
        SemanticQuality::Weak
    }
}

fn rationale_kind(score_breakdown: SemanticScoreBreakdown) -> SemanticRationaleKind {
    if quality_for_score(score_breakdown.hybrid) == SemanticQuality::Weak {
        SemanticRationaleKind::WeakMatch
    } else if score_breakdown.lexical > 0.0 {
        SemanticRationaleKind::LexicalBoosted
    } else {
        SemanticRationaleKind::SemanticOnly
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::types::SemanticCancellationToken;

    fn embedded(
        session: &str,
        conversation_index: usize,
        chunk_index: usize,
        text: &str,
        embedding: Vec<f32>,
    ) -> EmbeddedChunk {
        EmbeddedChunk {
            conversation_index,
            source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
            session: session.to_string(),
            chunk_index,
            key: format!("{session}:{chunk_index}"),
            text: text.to_string(),
            message_range: crate::agent::refs::MessageRange::single(chunk_index + 1),
            embedding,
        }
    }

    #[test]
    fn ranking_keeps_best_chunk_per_session() {
        let chunks = vec![
            embedded("session-a", 0, 0, "rust cache", vec![1.0, 0.0]),
            embedded("session-a", 0, 1, "unrelated", vec![0.0, 1.0]),
            embedded("session-b", 1, 0, "rust", vec![0.5, 0.5]),
        ];

        let hits = rank_chunks(
            "rust cache",
            &[1.0, 0.0],
            &chunks,
            &SemanticCancellationToken::new(),
        )
        .unwrap();

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].session, "session-a");
        assert_eq!(hits[0].snippet, "rust cache");
        assert!(hits[0].semantic_score > hits[1].semantic_score);
        assert_eq!(hits[0].lexical_score, 0.2);
    }

    #[test]
    fn ranking_preserves_message_range() {
        let chunks = vec![embedded(
            "session-a",
            0,
            3,
            "range evidence",
            vec![1.0, 0.0],
        )];

        let hits = rank_chunks(
            "range",
            &[1.0, 0.0],
            &chunks,
            &SemanticCancellationToken::new(),
        )
        .unwrap();

        assert_eq!(
            hits[0].message_range,
            crate::agent::refs::MessageRange::single(4)
        );
        assert_eq!(
            hits[0].explanation.chunk.message_range,
            crate::agent::refs::MessageRange::single(4)
        );
    }

    #[test]
    fn ranking_uses_explicit_query_embedding() {
        let chunks = vec![
            embedded("session-a", 0, 0, "same words", vec![0.0, 1.0]),
            embedded("session-b", 1, 0, "same words", vec![1.0, 0.0]),
        ];

        let hits = rank_chunks(
            "same words",
            &[0.0, 1.0],
            &chunks,
            &SemanticCancellationToken::new(),
        )
        .unwrap();

        assert_eq!(hits[0].session, "session-a");
        assert_eq!(hits[0].semantic_score, 1.0);
    }

    #[test]
    fn empty_query_has_no_lexical_boost() {
        let chunks = vec![
            embedded("session-a", 0, 0, "same words", vec![0.0, 1.0]),
            embedded("session-b", 1, 0, "same words", vec![1.0, 0.0]),
        ];

        let hits = rank_chunks(
            "   ",
            &[0.0, 1.0],
            &chunks,
            &SemanticCancellationToken::new(),
        )
        .unwrap();

        assert_eq!(hits[0].session, "session-a");
        assert!(hits.iter().all(|hit| hit.lexical_score == 0.0));
    }

    #[test]
    fn semantic_only_match_records_no_lexical_terms() {
        let chunks = vec![embedded(
            "session-a",
            0,
            0,
            "vector-only evidence",
            vec![1.0, 0.0],
        )];

        let hits = rank_chunks(
            "unrelated",
            &[1.0, 0.0],
            &chunks,
            &SemanticCancellationToken::new(),
        )
        .unwrap();
        let explanation = &hits[0].explanation;

        assert_eq!(hits[0].lexical_score, 0.0);
        assert_eq!(
            explanation.rationale_kind,
            SemanticRationaleKind::SemanticOnly
        );
        assert!(explanation.matched_terms.is_empty());
    }

    #[test]
    fn lexical_overlap_contributes_to_hybrid_ranking() {
        let chunks = vec![
            embedded("session-a", 0, 0, "unrelated", vec![1.0, 0.0]),
            embedded("session-b", 1, 0, "rust cache", vec![1.0, 0.0]),
        ];

        let hits = rank_chunks(
            "rust cache",
            &[1.0, 0.0],
            &chunks,
            &SemanticCancellationToken::new(),
        )
        .unwrap();

        assert_eq!(hits[0].session, "session-b");
        assert!(hits[0].lexical_score > hits[1].lexical_score);
        assert!(hits[0].hybrid_score > hits[1].hybrid_score);
        assert_eq!(
            hits[0].explanation.rationale_kind,
            SemanticRationaleKind::LexicalBoosted
        );
    }

    #[test]
    fn ranking_keeps_copied_sessions_separate() {
        let chunks = vec![
            embedded("session", 0, 0, "same words", vec![1.0, 0.0]),
            embedded("session", 1, 0, "same words", vec![1.0, 0.0]),
        ];

        let hits = rank_chunks(
            "same words",
            &[1.0, 0.0],
            &chunks,
            &SemanticCancellationToken::new(),
        )
        .unwrap();

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].conversation_index, 0);
        assert_eq!(hits[1].conversation_index, 1);
    }

    #[test]
    fn ranking_uses_stable_tiebreaks_for_sessions_and_chunks() {
        let chunks = vec![
            embedded("session-b", 1, 0, "same words", vec![1.0, 0.0]),
            embedded("session-a", 0, 1, "same words", vec![1.0, 0.0]),
            embedded("session-a", 0, 0, "same words", vec![1.0, 0.0]),
        ];

        let hits = rank_chunks(
            "same words",
            &[1.0, 0.0],
            &chunks,
            &SemanticCancellationToken::new(),
        )
        .unwrap();

        assert_eq!(hits[0].conversation_index, 0);
        assert_eq!(hits[0].chunk_index, 0);
        assert_eq!(hits[1].conversation_index, 1);
    }

    #[test]
    fn score_breakdown_mirrors_compatibility_fields() {
        let chunks = vec![embedded("session-a", 0, 0, "rust cache", vec![1.0, 0.0])];

        let hits = rank_chunks(
            "rust cache",
            &[1.0, 0.0],
            &chunks,
            &SemanticCancellationToken::new(),
        )
        .unwrap();
        let hit = &hits[0];

        assert_eq!(hit.score_breakdown.hybrid, hit.hybrid_score);
        assert_eq!(hit.score_breakdown.semantic, hit.semantic_score);
        assert_eq!(hit.score_breakdown.lexical, hit.lexical_score);
        assert_eq!(hit.snippet, hit.explanation.evidence_preview);
    }

    #[test]
    fn explanation_records_matched_terms_in_query_order() {
        let chunks = vec![embedded(
            "session-a",
            0,
            0,
            "The audio_generation cache uses Rust code",
            vec![1.0, 0.0],
        )];

        let hits = rank_chunks(
            "rust audio-generation audio",
            &[1.0, 0.0],
            &chunks,
            &SemanticCancellationToken::new(),
        )
        .unwrap();

        assert_eq!(
            hits[0].explanation.matched_terms,
            vec![
                "rust".to_string(),
                "audio".to_string(),
                "generation".to_string()
            ]
        );
    }

    #[test]
    fn explanation_records_cjk_matched_terms_in_query_order() {
        let chunks = vec![embedded(
            "session-a",
            0,
            0,
            "日本語の検索と意味検索について",
            vec![1.0, 0.0],
        )];

        let hits = rank_chunks(
            "検索 日本語",
            &[1.0, 0.0],
            &chunks,
            &SemanticCancellationToken::new(),
        )
        .unwrap();

        assert_eq!(
            hits[0].explanation.matched_terms,
            vec!["検索".to_string(), "日本語".to_string()]
        );
    }

    #[test]
    fn explanation_assigns_quality_and_rationale_deterministically() {
        assert_eq!(quality_for_score(0.85), SemanticQuality::Strong);
        assert_eq!(quality_for_score(0.65), SemanticQuality::Good);
        assert_eq!(quality_for_score(0.35), SemanticQuality::Fair);
        assert_eq!(quality_for_score(0.349), SemanticQuality::Weak);
        let chunks = vec![embedded("session-a", 0, 0, "semantic text", vec![1.0, 0.0])];
        let hits = rank_chunks(
            "semantic",
            &[1.0, 0.0],
            &chunks,
            &SemanticCancellationToken::new(),
        )
        .unwrap();

        assert_eq!(hits[0].explanation.quality_label, "strong");
        assert_eq!(SemanticQuality::Good.label(), "good");
        assert_eq!(SemanticQuality::Fair.label(), "fair");
        assert_eq!(SemanticQuality::Weak.label(), "weak");
        assert_eq!(
            rationale_kind(SemanticScoreBreakdown {
                hybrid: 0.2,
                semantic: 0.2,
                lexical: 0.2,
            }),
            SemanticRationaleKind::WeakMatch
        );
        assert_eq!(
            rationale_kind(SemanticScoreBreakdown {
                hybrid: 0.7,
                semantic: 0.5,
                lexical: 0.2,
            }),
            SemanticRationaleKind::LexicalBoosted
        );
        assert_eq!(
            rationale_kind(SemanticScoreBreakdown {
                hybrid: 0.7,
                semantic: 0.7,
                lexical: 0.0,
            }),
            SemanticRationaleKind::SemanticOnly
        );
    }

    #[test]
    fn explanation_uses_sanitized_evidence_preview() {
        let chunks = vec![embedded(
            "session-a",
            0,
            0,
            "alpha\n<system-reminder>hidden</system-reminder>\tVec<T> x < y",
            vec![1.0, 0.0],
        )];

        let hits = rank_chunks(
            "alpha",
            &[1.0, 0.0],
            &chunks,
            &SemanticCancellationToken::new(),
        )
        .unwrap();

        assert_eq!(hits[0].explanation.evidence_preview, "alpha Vec<T> x < y");
        assert_eq!(hits[0].snippet, "alpha Vec<T> x < y");
    }
}
