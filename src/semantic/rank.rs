use crate::error::{AppError, Result};
use crate::semantic::evidence::{evidence_preview, matched_terms};
use crate::semantic::types::{
    EmbeddedChunk, SemanticChunkIdentity, SemanticExplanation, SemanticHit, SemanticQuality,
    SemanticRationaleKind, SemanticScoreBreakdown,
};
use rayon::prelude::*;
use std::cmp::Ordering;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct PreparedText {
    lower: Box<str>,
    norm: f32,
}

impl PreparedText {
    pub fn new(chunk: &EmbeddedChunk) -> Self {
        Self {
            lower: chunk.text.to_lowercase().into_boxed_str(),
            norm: vector_norm(&chunk.embedding),
        }
    }
}

#[derive(Clone, Copy)]
pub struct PreparedChunk<'a> {
    pub chunk: &'a EmbeddedChunk,
    pub prepared: &'a PreparedText,
}

pub struct RankedHits {
    pub conversations: Vec<SemanticHit>,
    pub chunks: Vec<SemanticHit>,
}

#[derive(Clone, Copy)]
struct ScoredChunk<'a> {
    chunk: &'a EmbeddedChunk,
    score: SemanticScoreBreakdown,
}

struct QueryContext<'a> {
    query: &'a str,
    embedding: &'a [f32],
    norm: f32,
    words_lower: Vec<String>,
}

impl<'a> QueryContext<'a> {
    fn new(query: &'a str, embedding: &'a [f32]) -> Self {
        Self {
            query,
            embedding,
            norm: vector_norm(embedding),
            words_lower: query
                .split_whitespace()
                .map(|word| word.to_lowercase())
                .collect(),
        }
    }

    fn score<'c>(&self, input: PreparedChunk<'c>) -> ScoredChunk<'c> {
        let semantic = cosine_prepared(
            self.embedding,
            self.norm,
            &input.chunk.embedding,
            input.prepared.norm,
        );
        let lexical = lexical_overlap_prepared(&self.words_lower, &input.prepared.lower);
        ScoredChunk {
            chunk: input.chunk,
            score: SemanticScoreBreakdown {
                hybrid: semantic + lexical,
                semantic,
                lexical,
            },
        }
    }

    fn materialize(&self, scored: ScoredChunk<'_>) -> SemanticHit {
        let quality = quality_for_score(scored.score.hybrid);
        let chunk = scored.chunk;
        SemanticHit::new(
            scored.score,
            SemanticExplanation {
                quality,
                quality_label: quality.label(),
                matched_terms: matched_terms(self.query, &chunk.text),
                evidence_preview: evidence_preview(&chunk.text),
                rationale_kind: rationale_kind(scored.score),
                chunk: SemanticChunkIdentity {
                    conversation_index: chunk.conversation_index,
                    source: chunk.source,
                    session: chunk.session.clone(),
                    chunk_index: chunk.chunk_index,
                    message_range: chunk.message_range,
                },
            },
        )
    }
}

pub fn rank_prepared(
    query: &str,
    query_embedding: &[f32],
    chunks: &[PreparedChunk<'_>],
    include_chunk_hits: bool,
    cancellation: &crate::semantic::types::SemanticCancellationToken,
) -> Result<RankedHits> {
    if query_embedding.is_empty() || query_embedding.iter().any(|value| !value.is_finite()) {
        return Err(AppError::ConfigError(
            "semantic query embedding is empty or contains non-finite values".to_string(),
        ));
    }
    let context = QueryContext::new(query, query_embedding);
    let mut scored = chunks
        .par_iter()
        .map(|input| {
            if cancellation.is_cancelled() {
                return Err(AppError::SemanticSearchCancelled);
            }
            if input.chunk.embedding.len() != query_embedding.len()
                || input.chunk.embedding.iter().any(|value| !value.is_finite())
            {
                return Err(AppError::ConfigError(format!(
                    "semantic passage embedding for {}:{} is invalid: expected {} finite dimensions, got {}",
                    input.chunk.session,
                    input.chunk.chunk_index,
                    query_embedding.len(),
                    input.chunk.embedding.len()
                )));
            }
            Ok(context.score(*input))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut best = HashMap::<usize, ScoredChunk<'_>>::new();
    for candidate in &scored {
        best.entry(candidate.chunk.conversation_index)
            .and_modify(|current| {
                if compare_scored(candidate, current).is_lt() {
                    *current = *candidate;
                }
            })
            .or_insert(*candidate);
    }
    let mut conversation_scores = best.into_values().collect::<Vec<_>>();
    conversation_scores.sort_by(compare_scored);
    let conversations = conversation_scores
        .into_par_iter()
        .map(|candidate| {
            if cancellation.is_cancelled() {
                return Err(AppError::SemanticSearchCancelled);
            }
            Ok(context.materialize(candidate))
        })
        .collect::<Result<Vec<_>>>()?;

    let chunks = if include_chunk_hits {
        scored.par_sort_by(compare_scored);
        scored
            .into_par_iter()
            .map(|candidate| {
                if cancellation.is_cancelled() {
                    return Err(AppError::SemanticSearchCancelled);
                }
                Ok(context.materialize(candidate))
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        Vec::new()
    };

    Ok(RankedHits {
        conversations,
        chunks,
    })
}

pub fn rank_chunks(
    query: &str,
    query_embedding: &[f32],
    chunks: &[EmbeddedChunk],
    cancellation: &crate::semantic::types::SemanticCancellationToken,
) -> Result<Vec<SemanticHit>> {
    let prepared = chunks.iter().map(PreparedText::new).collect::<Vec<_>>();
    let inputs = chunks
        .iter()
        .zip(&prepared)
        .map(|(chunk, prepared)| PreparedChunk { chunk, prepared })
        .collect::<Vec<_>>();
    Ok(rank_prepared(query, query_embedding, &inputs, false, cancellation)?.conversations)
}

fn compare_scored(a: &ScoredChunk<'_>, b: &ScoredChunk<'_>) -> Ordering {
    b.score
        .hybrid
        .total_cmp(&a.score.hybrid)
        .then_with(|| b.score.semantic.total_cmp(&a.score.semantic))
        .then_with(|| b.score.lexical.total_cmp(&a.score.lexical))
        .then_with(|| a.chunk.conversation_index.cmp(&b.chunk.conversation_index))
        .then_with(|| a.chunk.session.cmp(&b.chunk.session))
        .then_with(|| a.chunk.chunk_index.cmp(&b.chunk.chunk_index))
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

fn vector_norm(values: &[f32]) -> f32 {
    values.iter().map(|value| value * value).sum::<f32>().sqrt()
}

fn cosine_prepared(a: &[f32], norm_a: f32, b: &[f32], norm_b: f32) -> f32 {
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    let dot = a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
    dot / (norm_a * norm_b)
}

fn lexical_overlap_prepared(query_words: &[String], text_lower: &str) -> f32 {
    if query_words.is_empty() {
        return 0.0;
    }
    let matches = query_words
        .iter()
        .filter(|word| text_lower.contains(word.as_str()))
        .count();
    0.2 * matches as f32 / query_words.len() as f32
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
            text: text.to_string(),
            message_range: crate::history::MessageRange::single(chunk_index + 1),
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
            crate::history::MessageRange::single(4)
        );
        assert_eq!(
            hits[0].explanation.chunk.message_range,
            crate::history::MessageRange::single(4)
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
