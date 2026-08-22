use crate::error::{AppError, Result};
use crate::history::Conversation;
use crate::search::literal::Literal;
use crate::semantic::cache::{
    cache_miss_count, embed_chunks_with_budget_and_save, read_embedding_cache,
};
use crate::semantic::embed::SemanticEmbedder;
use crate::semantic::rank::{rank_conversation_hits, rank_prepared};
use crate::semantic::types::{
    ChunkConfig, EmbeddingCache, SemanticCancellationToken, SemanticChunkSource, SemanticHit,
};
use rayon::prelude::*;
use std::sync::Arc;
mod resident;
mod signature;
#[cfg(test)]
mod tests;

use resident::{ResidentIndex, corpus_has_chunks, passes_literals, plan_refresh};
use signature::SemanticIndexSignature;

#[derive(Clone)]
pub struct SemanticIndexCandidate {
    pub index: usize,
    pub source: SemanticChunkSource,
    pub conversation: Arc<Conversation>,
}

pub struct SemanticIndexRequest<'a> {
    pub query: &'a str,
    pub literal_filters: &'a [Literal],
    pub full_corpus: &'a [SemanticIndexCandidate],
    pub scope: &'a [SemanticIndexCandidate],
    pub corpus_version: u64,
    pub prewarm: bool,
}

pub struct SemanticIndexResponse {
    pub hits: Vec<SemanticHit>,
    pub chunk_hits: Vec<SemanticHit>,
    pub indexed_chunk_count: usize,
    pub missing_chunk_count: usize,
    pub query_embedding_returned: bool,
    pub progress: SemanticIndexProgress,
    pub prewarm: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticIndexProgress {
    Embedding { completed: usize, total: usize },
    CacheReady,
    Ranking,
    Complete,
    EmptyCorpus,
}
/// Persistent semantic index: resident embeddings per conversation plus the
/// on-disk embedding cache they were built from.
///
/// Refreshing is incremental per conversation (see `resident`); ranking
/// borrows the resident chunks and never copies them.
pub struct SemanticIndexState {
    signature: Option<SemanticIndexSignature>,
    resident: ResidentIndex,
    pub cache: EmbeddingCache,
    pub chunk_config: ChunkConfig,
}

impl SemanticIndexState {
    pub fn new() -> Self {
        Self::with_chunk_config(ChunkConfig::default())
    }

    pub fn with_chunk_config(chunk_config: ChunkConfig) -> Self {
        Self {
            signature: None,
            resident: ResidentIndex::default(),
            cache: read_embedding_cache(chunk_config),
            chunk_config,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_cache(chunk_config: ChunkConfig, cache: EmbeddingCache) -> Self {
        Self {
            signature: None,
            resident: ResidentIndex::default(),
            cache,
            chunk_config,
        }
    }

    /// Number of chunks currently resident (and therefore rankable). O(1).
    pub fn indexed_chunk_count(&self) -> usize {
        self.resident.chunk_count()
    }

    #[cfg(test)]
    pub fn resident_conversation_count(&self) -> usize {
        self.resident.len()
    }

    pub fn has_chunks(
        &self,
        request: &SemanticIndexRequest<'_>,
        cancellation: &SemanticCancellationToken,
    ) -> Result<bool> {
        if cancellation.is_cancelled() {
            return Err(AppError::SemanticSearchCancelled);
        }
        if self.signature_matches(request) {
            return Ok(self.indexed_chunk_count() > 0);
        }
        Ok(corpus_has_chunks(request, self.chunk_config))
    }

    pub fn clear_empty(
        &mut self,
        request: &SemanticIndexRequest<'_>,
        cancellation: &SemanticCancellationToken,
    ) -> Result<()> {
        if cancellation.is_cancelled() {
            return Err(AppError::SemanticSearchCancelled);
        }
        self.signature = Some(SemanticIndexSignature::of(request, self.chunk_config));
        self.resident.clear();
        Ok(())
    }

    pub fn refresh_passages(
        &mut self,
        request: &SemanticIndexRequest<'_>,
        embedder: &mut dyn SemanticEmbedder,
        cancellation: &SemanticCancellationToken,
        progress: impl FnMut(SemanticIndexProgress),
        save_cache: impl FnMut(&EmbeddingCache),
    ) -> Result<SemanticIndexResponse> {
        self.refresh_passages_with_budget(
            request,
            embedder,
            cancellation,
            None,
            progress,
            save_cache,
        )
    }

    /// Brings the corpus to resident state, embedding at most
    /// `max_new_embeddings` missing passages.
    ///
    /// Conversations already resident in this exact version are skipped. The
    /// others are re-chunked; their chunks are resolved against the cache and
    /// the misses embedded within the budget. A conversation whose chunks are
    /// all present becomes complete and keeps its signature; one left with
    /// uncached chunks stays rankable with what it has and is planned again
    /// on the next refresh. The corpus signature is recorded only when nothing
    /// is missing.
    fn refresh_passages_with_budget(
        &mut self,
        request: &SemanticIndexRequest<'_>,
        embedder: &mut dyn SemanticEmbedder,
        cancellation: &SemanticCancellationToken,
        max_new_embeddings: Option<usize>,
        mut progress: impl FnMut(SemanticIndexProgress),
        save_cache: impl FnMut(&EmbeddingCache),
    ) -> Result<SemanticIndexResponse> {
        if cancellation.is_cancelled() {
            return Err(AppError::SemanticSearchCancelled);
        }
        if self.signature_matches(request) {
            progress(SemanticIndexProgress::CacheReady);
            return Ok(self.refresh_response(request, 0));
        }
        let next_signature = SemanticIndexSignature::of(request, self.chunk_config);
        self.resident.retain_corpus(request.full_corpus);
        let pending = plan_refresh(&self.resident, request, self.chunk_config, cancellation)?;

        if !pending.has_chunks() && self.resident.chunk_count() == 0 {
            self.signature = Some(next_signature);
            self.resident.clear();
            return Ok(self.refresh_response(request, 0));
        }

        let miss_count = cache_miss_count(&pending.chunks, &self.cache);
        let embedding_count = max_new_embeddings.map_or(miss_count, |limit| miss_count.min(limit));
        let missing_chunk_count = miss_count.saturating_sub(embedding_count);
        progress(if embedding_count > 0 {
            SemanticIndexProgress::Embedding {
                completed: 0,
                total: embedding_count,
            }
        } else {
            SemanticIndexProgress::CacheReady
        });

        let (pending, chunks) = pending.take_chunks();
        let embedded = embed_chunks_with_budget_and_save(
            embedder,
            chunks,
            &mut self.cache,
            cancellation,
            max_new_embeddings,
            |completed, total| progress(SemanticIndexProgress::Embedding { completed, total }),
            save_cache,
        )?;
        self.resident.absorb(pending, embedded);
        self.signature = (embedding_count == miss_count).then_some(next_signature);
        Ok(self.refresh_response(request, missing_chunk_count))
    }

    pub fn rank_refreshed(
        &self,
        request: &SemanticIndexRequest<'_>,
        embedder: &mut dyn SemanticEmbedder,
        cancellation: &SemanticCancellationToken,
        mut progress: impl FnMut(SemanticIndexProgress),
    ) -> Result<SemanticIndexResponse> {
        if cancellation.is_cancelled() {
            return Err(AppError::SemanticSearchCancelled);
        }
        let scoped = self.resident.scoped(request.scope, cancellation)?;
        if scoped.is_empty() || request.prewarm {
            return Ok(self.response(
                Vec::new(),
                Vec::new(),
                true,
                if scoped.is_empty() {
                    SemanticIndexProgress::EmptyCorpus
                } else {
                    SemanticIndexProgress::CacheReady
                },
                request.prewarm,
            ));
        }

        progress(SemanticIndexProgress::Ranking);
        let Some(query_embedding) = embedder.embed_query(request.query)? else {
            return Ok(self.response(
                Vec::new(),
                Vec::new(),
                false,
                SemanticIndexProgress::EmptyCorpus,
                request.prewarm,
            ));
        };

        let literals = request.literal_filters;
        let candidates: Vec<_> = if literals.is_empty() {
            scoped
        } else {
            scoped
                .into_par_iter()
                .filter(|(chunk, _)| passes_literals(chunk, literals))
                .collect()
        };
        let chunk_hits = rank_prepared(request.query, &query_embedding, &candidates, cancellation)?;
        let hits = rank_conversation_hits(&chunk_hits);
        Ok(self.response(
            hits,
            chunk_hits,
            true,
            SemanticIndexProgress::Complete,
            request.prewarm,
        ))
    }

    pub fn refresh_or_prewarm(
        &mut self,
        request: &SemanticIndexRequest<'_>,
        embedder: &mut dyn SemanticEmbedder,
        cancellation: &SemanticCancellationToken,
        progress: impl FnMut(SemanticIndexProgress),
        save_cache: impl FnMut(&EmbeddingCache),
    ) -> Result<SemanticIndexResponse> {
        self.refresh_or_prewarm_with_budget(
            request,
            embedder,
            cancellation,
            None,
            progress,
            save_cache,
        )
    }

    pub fn refresh_or_prewarm_with_budget(
        &mut self,
        request: &SemanticIndexRequest<'_>,
        embedder: &mut dyn SemanticEmbedder,
        cancellation: &SemanticCancellationToken,
        max_new_embeddings: Option<usize>,
        mut progress: impl FnMut(SemanticIndexProgress),
        save_cache: impl FnMut(&EmbeddingCache),
    ) -> Result<SemanticIndexResponse> {
        let response = self.refresh_passages_with_budget(
            request,
            embedder,
            cancellation,
            max_new_embeddings,
            &mut progress,
            save_cache,
        )?;
        if response.progress == SemanticIndexProgress::EmptyCorpus || request.prewarm {
            return Ok(response);
        }
        let missing_chunk_count = response.missing_chunk_count;
        let mut response = self.rank_refreshed(request, embedder, cancellation, progress)?;
        response.missing_chunk_count = missing_chunk_count;
        Ok(response)
    }

    fn signature_matches(&self, request: &SemanticIndexRequest<'_>) -> bool {
        self.signature
            .as_ref()
            .is_some_and(|signature| signature.matches(request, self.chunk_config))
    }

    fn refresh_response(
        &self,
        request: &SemanticIndexRequest<'_>,
        missing_chunk_count: usize,
    ) -> SemanticIndexResponse {
        let progress = if self.indexed_chunk_count() == 0 {
            SemanticIndexProgress::EmptyCorpus
        } else {
            SemanticIndexProgress::CacheReady
        };
        let mut response = self.response(Vec::new(), Vec::new(), true, progress, request.prewarm);
        response.missing_chunk_count = missing_chunk_count;
        response
    }

    fn response(
        &self,
        hits: Vec<SemanticHit>,
        chunk_hits: Vec<SemanticHit>,
        query_embedding_returned: bool,
        progress: SemanticIndexProgress,
        prewarm: bool,
    ) -> SemanticIndexResponse {
        SemanticIndexResponse {
            hits,
            chunk_hits,
            indexed_chunk_count: self.indexed_chunk_count(),
            missing_chunk_count: 0,
            query_embedding_returned,
            progress,
            prewarm,
        }
    }
}

impl Default for SemanticIndexState {
    fn default() -> Self {
        Self::new()
    }
}
