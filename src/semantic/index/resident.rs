//! Resident embeddings, one entry per conversation.
//!
//! A conversation is resident once its chunks were resolved against the cache
//! (and embedded when the budget allowed). Conversations whose chunks are all
//! present carry their signature and are skipped by later refreshes; partial
//! ones (some chunks still uncached after a bounded refresh) stay rankable
//! with what they have and are planned again next time, so a later refresh
//! can complete them.
//!
//! Ranking borrows the resident chunks together with their query-independent
//! text derivations; nothing is copied per query, and the chunk count is kept
//! up to date on insert so `indexed_chunk_count` is O(1).

use super::signature::{ConversationKey, ConversationSignature, candidate_key};
use super::{SemanticIndexCandidate, SemanticIndexRequest};
use crate::error::{AppError, Result};
use crate::search::literal::Literal;
use crate::semantic::chunk::build_chunks_with_sources;
use crate::semantic::rank::PreparedText;
use crate::semantic::types::{
    ChunkConfig, EmbeddedChunk, SemanticCancellationToken, SemanticChunk,
};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};

struct ResidentChunk {
    embedded: EmbeddedChunk,
    prepared: PreparedText,
}

struct ResidentConversation {
    /// `Some` when every chunk is present; `None` for a partial conversation.
    signature: Option<ConversationSignature>,
    chunks: Vec<ResidentChunk>,
}

#[derive(Default)]
pub(super) struct ResidentIndex {
    conversations: HashMap<ConversationKey, ResidentConversation>,
    chunk_count: usize,
}

impl ResidentIndex {
    pub(super) fn chunk_count(&self) -> usize {
        self.chunk_count
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.conversations.len()
    }

    pub(super) fn clear(&mut self) {
        self.conversations.clear();
        self.chunk_count = 0;
    }

    /// Drops conversations that left the corpus.
    pub(super) fn retain_corpus(&mut self, corpus: &[SemanticIndexCandidate]) {
        if self.conversations.is_empty() {
            return;
        }
        let live: HashSet<ConversationKey> = corpus.iter().map(candidate_key).collect();
        let mut dropped = 0;
        self.conversations.retain(|key, resident| {
            let keep = live.contains(key);
            if !keep {
                dropped += resident.chunks.len();
            }
            keep
        });
        self.chunk_count -= dropped;
    }

    /// True when the conversation is fully resident in this exact version.
    pub(super) fn is_current(&self, candidate: &SemanticIndexCandidate) -> bool {
        self.conversations
            .get(&candidate_key(candidate))
            .and_then(|resident| resident.signature.as_ref())
            .is_some_and(|signature| signature.matches(candidate))
    }

    /// Makes the planned conversations resident from the chunks that came back
    /// from the cache/embedder. A conversation whose chunk count matches the
    /// plan is complete and keeps its signature; the others become partial.
    /// Text derivations are computed in parallel across all of them.
    pub(super) fn absorb(&mut self, pending: PendingRefresh, embedded: Vec<EmbeddedChunk>) {
        let PendingRefresh {
            keys,
            signatures,
            expected,
            ..
        } = pending;
        let slot_of: HashMap<ConversationKey, usize> = keys
            .iter()
            .enumerate()
            .map(|(slot, key)| (*key, slot))
            .collect();
        let mut groups: Vec<Vec<EmbeddedChunk>> = keys.iter().map(|_| Vec::new()).collect();
        for chunk in embedded {
            if let Some(&slot) = slot_of.get(&(chunk.conversation_index, chunk.source)) {
                groups[slot].push(chunk);
            }
        }
        let prepared: Vec<(
            ConversationKey,
            Option<ConversationSignature>,
            Vec<ResidentChunk>,
        )> = keys
            .into_par_iter()
            .zip(signatures)
            .zip(expected)
            .zip(groups)
            .map(|(((key, signature), expected), mut chunks)| {
                chunks.sort_by_key(|chunk| chunk.chunk_index);
                let complete = chunks.len() == expected;
                let chunks = chunks
                    .into_iter()
                    .map(|embedded| ResidentChunk {
                        prepared: PreparedText::of(&embedded),
                        embedded,
                    })
                    .collect();
                (key, complete.then_some(signature), chunks)
            })
            .collect();
        for (key, signature, chunks) in prepared {
            self.chunk_count += chunks.len();
            if let Some(previous) = self
                .conversations
                .insert(key, ResidentConversation { signature, chunks })
            {
                self.chunk_count -= previous.chunks.len();
            }
        }
    }

    /// Resident chunks of `scope`, in scope order, borrowed with their
    /// prepared text.
    pub(super) fn scoped<'a>(
        &'a self,
        scope: &[SemanticIndexCandidate],
        cancellation: &SemanticCancellationToken,
    ) -> Result<Vec<(&'a EmbeddedChunk, &'a PreparedText)>> {
        let mut chunks = Vec::new();
        for candidate in scope {
            if cancellation.is_cancelled() {
                return Err(AppError::SemanticSearchCancelled);
            }
            if let Some(resident) = self.conversations.get(&candidate_key(candidate)) {
                chunks.extend(
                    resident
                        .chunks
                        .iter()
                        .map(|chunk| (&chunk.embedded, &chunk.prepared)),
                );
            }
        }
        Ok(chunks)
    }
}

/// Keeps only the chunks whose text satisfies every literal filter.
pub(super) fn passes_literals(chunk: &EmbeddedChunk, literals: &[Literal]) -> bool {
    literals.iter().all(|literal| literal.matches(&chunk.text))
}

// ---------------------------------------------------------------------------
// Refresh planning
// ---------------------------------------------------------------------------

/// Conversations that are missing, partial or stale: their keys, signatures,
/// expected chunk counts, and all their chunks flattened in corpus order.
pub(super) struct PendingRefresh {
    keys: Vec<ConversationKey>,
    signatures: Vec<ConversationSignature>,
    expected: Vec<usize>,
    pub(super) chunks: Vec<SemanticChunk>,
}

impl PendingRefresh {
    pub(super) fn has_chunks(&self) -> bool {
        !self.chunks.is_empty()
    }
    /// Splits the flattened chunks off so they can be embedded while the plan
    /// (keys, signatures, expected counts) stays available for `absorb`.
    pub(super) fn take_chunks(mut self) -> (PendingRefresh, Vec<SemanticChunk>) {
        let chunks = std::mem::take(&mut self.chunks);
        (self, chunks)
    }
}

pub(super) fn plan_refresh(
    resident: &ResidentIndex,
    request: &SemanticIndexRequest<'_>,
    chunk_config: ChunkConfig,
    cancellation: &SemanticCancellationToken,
) -> Result<PendingRefresh> {
    let mut stale: Vec<&SemanticIndexCandidate> = Vec::new();
    for candidate in request.full_corpus {
        if cancellation.is_cancelled() {
            return Err(AppError::SemanticSearchCancelled);
        }
        if !resident.is_current(candidate) {
            stale.push(candidate);
        }
    }
    let keys: Vec<ConversationKey> = stale
        .iter()
        .map(|candidate| candidate_key(candidate))
        .collect();
    let chunks = build_chunks_with_sources(
        stale.iter().map(|candidate| {
            (
                candidate.index,
                candidate.source,
                candidate.conversation.as_ref(),
            )
        }),
        chunk_config,
    );
    let mut expected = vec![0usize; keys.len()];
    let mut cursor = 0;
    for chunk in &chunks {
        let key = (chunk.conversation_index, chunk.source);
        while cursor < keys.len() && keys[cursor] != key {
            cursor += 1;
        }
        if cursor == keys.len() {
            break;
        }
        expected[cursor] += 1;
    }
    Ok(PendingRefresh {
        signatures: stale
            .iter()
            .map(|candidate| ConversationSignature::of(candidate))
            .collect(),
        keys,
        expected,
        chunks,
    })
}

// ---------------------------------------------------------------------------
// Chunk building
// ---------------------------------------------------------------------------

pub(super) fn candidate_chunks(
    candidates: &[SemanticIndexCandidate],
    chunk_config: ChunkConfig,
) -> Vec<SemanticChunk> {
    build_chunks_with_sources(
        candidates.iter().map(|candidate| {
            (
                candidate.index,
                candidate.source,
                candidate.conversation.as_ref(),
            )
        }),
        chunk_config,
    )
}

/// Does any conversation of the corpus produce at least one chunk? Stops at
/// the first one that does, instead of chunking the whole corpus.
pub(super) fn corpus_has_chunks(
    request: &SemanticIndexRequest<'_>,
    chunk_config: ChunkConfig,
) -> bool {
    request.full_corpus.iter().any(|candidate| {
        !candidate_chunks(std::slice::from_ref(candidate), chunk_config).is_empty()
    })
}

#[cfg(test)]
pub(super) fn full_corpus_chunks(
    request: &SemanticIndexRequest<'_>,
    chunk_config: ChunkConfig,
) -> Vec<SemanticChunk> {
    candidate_chunks(request.full_corpus, chunk_config)
}

#[cfg(test)]
pub(super) fn semantic_chunks(
    request: &SemanticIndexRequest<'_>,
    chunk_config: ChunkConfig,
) -> Vec<SemanticChunk> {
    candidate_chunks(request.scope, chunk_config)
}
