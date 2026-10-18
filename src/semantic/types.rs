use crate::agent::refs::MessageRange;
use crate::semantic::matrix::{EmbeddingKey, EmbeddingMatrix};
use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

pub const DEFAULT_CHUNK_TARGET_CHARS: usize = 1_600;
pub const DEFAULT_CHUNK_OVERLAP_CHARS: usize = 300;
pub const DEFAULT_CHUNK_CONTEXT_TURNS: usize = 1;
/// Passages handed to the embedder per call, between progress reports and
/// cancellation checks.
pub const DEFAULT_EMBEDDING_BATCH_SIZE: usize = 64;
/// Passages per model inference batch.
pub const MODEL_BATCH_SIZE: usize = 8;
pub const MAX_GLOBAL_INTERACTIVE_PASSAGE_EMBEDDINGS: usize = 0;
pub const MAX_WITHIN_INTERACTIVE_PASSAGE_EMBEDDINGS: usize = 32;
pub const CACHE_SCHEMA_VERSION: u32 = 8;
pub const MODEL_NAME: &str = "SnowflakeArcticEmbedXS";

#[derive(Clone, Debug, Default)]
pub struct SemanticCancellationToken {
    generation: Arc<AtomicU64>,
    expected_generation: u64,
}

impl SemanticCancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn child(&self) -> Self {
        Self {
            generation: self.generation.clone(),
            expected_generation: self.generation.load(Ordering::Relaxed),
        }
    }

    pub fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.generation.load(Ordering::Relaxed) != self.expected_generation
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChunkConfig {
    pub target_chars: usize,
    pub overlap_chars: usize,
    pub context_turns: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            target_chars: DEFAULT_CHUNK_TARGET_CHARS,
            overlap_chars: DEFAULT_CHUNK_OVERLAP_CHARS,
            context_turns: DEFAULT_CHUNK_CONTEXT_TURNS,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum SemanticChunkSource {
    #[default]
    VisibleDialogue,
    AgentRoute,
    AgentTool,
    AgentThinking,
    AgentSubagentDialogue,
    AgentSubagentTool,
    AgentSubagentThinking,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticChunk {
    pub conversation_index: usize,
    pub source: SemanticChunkSource,
    pub session: String,
    pub chunk_index: usize,
    pub text: String,
    pub message_range: MessageRange,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EmbeddedChunk {
    pub conversation_index: usize,
    pub source: SemanticChunkSource,
    pub session: String,
    pub chunk_index: usize,
    pub text: String,
    pub message_range: MessageRange,
    pub embedding: Embedding,
}

/// A passage embedding, either computed in this process or a row of the
/// mapped embedding matrix.
#[derive(Clone)]
pub enum Embedding {
    Owned(Arc<[f32]>),
    Mapped(Arc<EmbeddingMatrix>, u32),
}

impl std::ops::Deref for Embedding {
    type Target = [f32];

    fn deref(&self) -> &[f32] {
        match self {
            Self::Owned(values) => values,
            Self::Mapped(matrix, row) => matrix.vector(*row),
        }
    }
}

impl From<Vec<f32>> for Embedding {
    fn from(values: Vec<f32>) -> Self {
        Self::Owned(values.into())
    }
}

impl PartialEq for Embedding {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl PartialEq<Vec<f32>> for Embedding {
    fn eq(&self, other: &Vec<f32>) -> bool {
        **self == **other
    }
}

impl std::fmt::Debug for Embedding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        (**self).fmt(f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SemanticScoreBreakdown {
    pub hybrid: f32,
    pub semantic: f32,
    pub lexical: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticQuality {
    Strong,
    Good,
    Fair,
    Weak,
}

impl SemanticQuality {
    pub fn label(self) -> &'static str {
        match self {
            Self::Strong => "strong",
            Self::Good => "good",
            Self::Fair => "fair",
            Self::Weak => "weak",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticRationaleKind {
    SemanticOnly,
    LexicalBoosted,
    WeakMatch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticChunkIdentity {
    pub conversation_index: usize,
    pub source: SemanticChunkSource,
    pub session: String,
    pub chunk_index: usize,
    pub message_range: MessageRange,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticExplanation {
    pub quality: SemanticQuality,
    pub quality_label: &'static str,
    pub matched_terms: Vec<String>,
    pub evidence_preview: String,
    pub rationale_kind: SemanticRationaleKind,
    pub chunk: SemanticChunkIdentity,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticHit {
    pub conversation_index: usize,
    pub session: String,
    pub chunk_index: usize,
    pub semantic_score: f32,
    pub lexical_score: f32,
    pub hybrid_score: f32,
    pub score_breakdown: SemanticScoreBreakdown,
    pub explanation: SemanticExplanation,
    pub snippet: String,
    pub message_range: MessageRange,
}

impl SemanticHit {
    pub fn new(score_breakdown: SemanticScoreBreakdown, explanation: SemanticExplanation) -> Self {
        let chunk = &explanation.chunk;
        Self {
            conversation_index: chunk.conversation_index,
            session: chunk.session.clone(),
            chunk_index: chunk.chunk_index,
            semantic_score: score_breakdown.semantic,
            lexical_score: score_breakdown.lexical,
            hybrid_score: score_breakdown.hybrid,
            snippet: explanation.evidence_preview.clone(),
            message_range: chunk.message_range,
            score_breakdown,
            explanation,
        }
    }
}

pub struct EmbeddingCache {
    pub chunk_target_chars: usize,
    pub chunk_overlap_chars: usize,
    pub chunk_context_turns: usize,
    pub access_counter: u64,
    pub entries: HashMap<EmbeddingKey, CachedChunk>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CachedChunk {
    pub embedding: Embedding,
    pub last_used: u64,
    pub protected: bool,
}
