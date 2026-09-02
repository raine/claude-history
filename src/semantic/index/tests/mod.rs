//! Fixtures shared by the index tests: a deterministic embedder, corpus
//! builders and refresh helpers. Each test group lives in its own file.

use super::resident::{full_corpus_chunks, semantic_chunks};
use super::signature::semantic_index_signature;
use super::*;
use crate::semantic::cache::{cache_miss_count, empty_embedding_cache};
use crate::semantic::test_fixtures::{SemanticConversationFixture, beta_hit_metadata};
use crate::semantic::types::CachedChunk;

mod corpus;
mod rank;
mod refresh;

struct FakeEmbedder {
    passage_calls: usize,
    query_calls: usize,
    embedded_passages: Vec<Vec<String>>,
    query_embedding: Option<Vec<f32>>,
}

impl FakeEmbedder {
    fn new() -> Self {
        Self {
            passage_calls: 0,
            query_calls: 0,
            embedded_passages: Vec::new(),
            query_embedding: Some(vec![1.0, 0.0]),
        }
    }
}

impl SemanticEmbedder for FakeEmbedder {
    fn embed_passages(&mut self, passages: &[String]) -> Result<Vec<Vec<f32>>> {
        self.passage_calls += 1;
        self.embedded_passages.push(passages.to_vec());
        Ok(passages
            .iter()
            .map(|passage| match passage.as_str() {
                "visible alpha" => vec![1.0, 0.0],
                "visible beta" => vec![0.0, 1.0],
                _ => vec![0.5, 0.5],
            })
            .collect())
    }

    fn embed_query(&mut self, query: &str) -> Result<Option<Vec<f32>>> {
        self.query_calls += 1;
        Ok(if query.contains("beta") {
            Some(vec![0.0, 1.0])
        } else {
            self.query_embedding.clone()
        })
    }
}

fn conversation(path: &str, semantic_turns: Vec<&str>) -> Conversation {
    SemanticConversationFixture::new(path, semantic_turns).build()
}

fn request(
    query: &str,
    conversations: Vec<Conversation>,
    candidate_indices: Vec<usize>,
) -> (String, Vec<SemanticIndexCandidate>) {
    let candidates = candidate_indices
        .into_iter()
        .map(|index| SemanticIndexCandidate {
            index,
            source: SemanticChunkSource::VisibleDialogue,
            conversation: Arc::new(conversations[index].clone()),
        })
        .collect();
    (query.to_string(), candidates)
}

fn index_request<'a>(
    query: &'a str,
    candidates: &'a [SemanticIndexCandidate],
) -> SemanticIndexRequest<'a> {
    SemanticIndexRequest {
        query,
        literal_filters: &[],
        full_corpus: candidates,
        scope: candidates,
        corpus_version: 1,
        prewarm: false,
    }
}

fn cache_passage(cache: &mut EmbeddingCache, _key: String, text: String, embedding: Vec<f32>) {
    cache.entries.insert(
        crate::semantic::cache::embedding_cache_key(&text),
        CachedChunk {
            embedding,
            last_used: 0,
            protected: false,
        },
    );
}

fn candidates_from(conversations: &[Conversation]) -> Vec<SemanticIndexCandidate> {
    conversations
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, conversation)| SemanticIndexCandidate {
            index,
            source: SemanticChunkSource::VisibleDialogue,
            conversation: Arc::new(conversation),
        })
        .collect()
}

fn cache_request_passages(cache: &mut EmbeddingCache, request: &SemanticIndexRequest<'_>) {
    for chunk in full_corpus_chunks(request, ChunkConfig::default()) {
        let embedding = match chunk.text.as_str() {
            "visible beta" => vec![0.0, 1.0],
            "visible alpha" => vec![1.0, 0.0],
            _ => vec![0.5, 0.5],
        };
        cache_passage(cache, chunk.key, chunk.text, embedding);
    }
}

fn prepare_indexed_state(
    request: &SemanticIndexRequest<'_>,
    chunk_config: ChunkConfig,
) -> (SemanticIndexState, FakeEmbedder) {
    let mut cache = empty_embedding_cache(chunk_config);
    cache_request_passages(&mut cache, request);
    (
        SemanticIndexState::with_cache(chunk_config, cache),
        FakeEmbedder::new(),
    )
}

fn prepare_empty_state(chunk_config: ChunkConfig) -> (SemanticIndexState, FakeEmbedder) {
    (
        SemanticIndexState::with_cache(chunk_config, empty_embedding_cache(chunk_config)),
        FakeEmbedder::new(),
    )
}

fn run_refresh(
    state: &mut SemanticIndexState,
    request: &SemanticIndexRequest<'_>,
    embedder: &mut FakeEmbedder,
) -> Result<SemanticIndexResponse> {
    state.refresh_or_prewarm(
        request,
        embedder,
        &SemanticCancellationToken::new(),
        |_| {},
        |_| {},
    )
}

fn run_refresh_with_observers(
    state: &mut SemanticIndexState,
    request: &SemanticIndexRequest<'_>,
    embedder: &mut FakeEmbedder,
    progress: impl FnMut(SemanticIndexProgress),
    save_cache: impl FnMut(&EmbeddingCache),
) -> Result<SemanticIndexResponse> {
    state.refresh_or_prewarm(
        request,
        embedder,
        &SemanticCancellationToken::new(),
        progress,
        save_cache,
    )
}

fn run_refresh_passages(
    state: &mut SemanticIndexState,
    request: &SemanticIndexRequest<'_>,
    embedder: &mut FakeEmbedder,
) -> Result<SemanticIndexResponse> {
    state.refresh_passages(
        request,
        embedder,
        &SemanticCancellationToken::new(),
        |_| {},
        |_| {},
    )
}

fn assert_hit_indices(response: &SemanticIndexResponse, expected: &[usize]) {
    assert_eq!(
        response
            .hits
            .iter()
            .map(|hit| hit.conversation_index)
            .collect::<Vec<_>>(),
        expected
    );
}
