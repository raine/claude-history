//! Identity of what is resident: which conversations, in which version.
//!
//! A signature never copies conversation text. It keeps the `Arc<Conversation>`
//! the candidate arrived with, so the common case — the worker handing the
//! same corpus object on every request — is settled by pointer equality in
//! O(1) per conversation. Only a reloaded corpus falls back to comparing the
//! semantic turns, by reference and without allocating.

use super::{SemanticIndexCandidate, SemanticIndexRequest};
use crate::history::Conversation;
use crate::semantic::types::{ChunkConfig, SemanticChunkSource};
use std::fmt;
use std::sync::Arc;

/// `(conversation index, chunk source)`: the resident-map key.
pub(super) type ConversationKey = (usize, SemanticChunkSource);

pub(super) fn candidate_key(candidate: &SemanticIndexCandidate) -> ConversationKey {
    (candidate.index, candidate.source)
}

#[derive(Clone)]
pub(super) struct ConversationSignature {
    index: usize,
    source: SemanticChunkSource,
    conversation: Arc<Conversation>,
}

impl ConversationSignature {
    pub(super) fn of(candidate: &SemanticIndexCandidate) -> Self {
        Self {
            index: candidate.index,
            source: candidate.source,
            conversation: Arc::clone(&candidate.conversation),
        }
    }

    pub(super) fn matches(&self, candidate: &SemanticIndexCandidate) -> bool {
        self.index == candidate.index
            && self.source == candidate.source
            && same_conversation(&self.conversation, &candidate.conversation)
    }
}

/// Pointer equality first; otherwise the cheap fields, then the turn text.
fn same_conversation(a: &Arc<Conversation>, b: &Arc<Conversation>) -> bool {
    Arc::ptr_eq(a, b)
        || (a.path == b.path
            && a.semantic_turn_ranges == b.semantic_turn_ranges
            && a.semantic_turns == b.semantic_turns)
}

impl PartialEq for ConversationSignature {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
            && self.source == other.source
            && same_conversation(&self.conversation, &other.conversation)
    }
}

impl fmt::Debug for ConversationSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConversationSignature")
            .field("index", &self.index)
            .field("source", &self.source)
            .field("path", &self.conversation.path)
            .field("turns", &self.conversation.semantic_turns.len())
            .finish()
    }
}

/// Snapshot of a whole request corpus, used as the "nothing changed" fast path.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct SemanticIndexSignature {
    corpus_version: u64,
    chunk_config: ChunkConfig,
    conversations: Vec<ConversationSignature>,
}

impl SemanticIndexSignature {
    pub(super) fn of(request: &SemanticIndexRequest<'_>, chunk_config: ChunkConfig) -> Self {
        Self {
            corpus_version: request.corpus_version,
            chunk_config,
            conversations: request
                .full_corpus
                .iter()
                .map(ConversationSignature::of)
                .collect(),
        }
    }

    pub(super) fn matches(
        &self,
        request: &SemanticIndexRequest<'_>,
        chunk_config: ChunkConfig,
    ) -> bool {
        self.corpus_version == request.corpus_version
            && self.chunk_config == chunk_config
            && self.conversations.len() == request.full_corpus.len()
            && self
                .conversations
                .iter()
                .zip(request.full_corpus)
                .all(|(stored, candidate)| stored.matches(candidate))
    }
}

#[cfg(test)]
pub(super) fn semantic_index_signature(
    request: &SemanticIndexRequest<'_>,
    chunk_config: ChunkConfig,
) -> SemanticIndexSignature {
    SemanticIndexSignature::of(request, chunk_config)
}
