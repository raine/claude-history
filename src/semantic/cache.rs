use crate::error::{AppError, Result};
use crate::semantic::embed::SemanticEmbedder;
use crate::semantic::types::{
    CACHE_SCHEMA_VERSION, CachedChunk, ChunkConfig, DEFAULT_EMBEDDING_BATCH_SIZE, EmbeddedChunk,
    EmbeddingCache, MODEL_NAME, SemanticChunk,
};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

pub(crate) const MAX_CACHE_ENTRIES: usize = 50_000;

#[cfg(test)]
pub fn embed_chunks_with_progress_and_save(
    embedder: &mut dyn SemanticEmbedder,
    chunks: Vec<SemanticChunk>,
    cache: &mut EmbeddingCache,
    cancellation: &crate::semantic::types::SemanticCancellationToken,
    progress: impl FnMut(usize, usize),
    save: impl FnMut(&EmbeddingCache),
) -> Result<Vec<EmbeddedChunk>> {
    embed_chunks_with_budget_and_save(embedder, chunks, cache, cancellation, None, progress, save)
}

pub fn embed_chunks_with_budget_and_save(
    embedder: &mut dyn SemanticEmbedder,
    chunks: Vec<SemanticChunk>,
    cache: &mut EmbeddingCache,
    cancellation: &crate::semantic::types::SemanticCancellationToken,
    max_new_embeddings: Option<usize>,
    mut progress: impl FnMut(usize, usize),
    mut save: impl FnMut(&EmbeddingCache),
) -> Result<Vec<EmbeddedChunk>> {
    const SAVE_INTERVAL: usize = 256;

    if cancellation.is_cancelled() {
        return Err(AppError::SemanticSearchCancelled);
    }

    let access = cache.access_counter.saturating_add(1);
    cache.access_counter = access;
    let mut embedded = Vec::with_capacity(chunks.len());
    let mut misses = Vec::<Vec<SemanticChunk>>::new();
    let mut miss_indices = HashMap::<String, usize>::new();

    for chunk in chunks {
        if cancellation.is_cancelled() {
            return Err(AppError::SemanticSearchCancelled);
        }
        let key = embedding_cache_key(&chunk.text);
        if let Some(entry) = cache.entries.get_mut(&key) {
            entry.last_used = access;
            embedded.push(embedded_chunk(&chunk, entry.embedding.clone()));
        } else if let Some(index) = miss_indices.get(&key).copied() {
            misses[index].push(chunk);
        } else {
            miss_indices.insert(key, misses.len());
            misses.push(vec![chunk]);
        }
    }

    let total_misses = max_new_embeddings.map_or(misses.len(), |limit| misses.len().min(limit));
    misses.truncate(total_misses);
    let mut completed = 0;
    let mut last_saved = 0;
    for batch in misses.chunks(DEFAULT_EMBEDDING_BATCH_SIZE) {
        if cancellation.is_cancelled() {
            save_pending_cache(cache, completed, &mut last_saved, &mut save);
            return Err(AppError::SemanticSearchCancelled);
        }
        let texts = batch
            .iter()
            .map(|chunks| chunks[0].text.clone())
            .collect::<Vec<_>>();
        let embeddings = match embedder.embed_passages(&texts) {
            Ok(embeddings) => embeddings,
            Err(error) => {
                save_pending_cache(cache, completed, &mut last_saved, &mut save);
                return Err(error);
            }
        };
        if let Err(error) = validate_passage_embeddings(&texts, &embeddings) {
            save_pending_cache(cache, completed, &mut last_saved, &mut save);
            return Err(error);
        }

        for (chunks, embedding) in batch.iter().zip(embeddings) {
            let key = embedding_cache_key(&chunks[0].text);
            cache.entries.insert(
                key,
                CachedChunk {
                    embedding: embedding.clone(),
                    last_used: access,
                    protected: false,
                },
            );
            for chunk in chunks {
                embedded.push(embedded_chunk(chunk, embedding.clone()));
            }
        }
        completed += batch.len();
        if completed == total_misses || completed - last_saved >= SAVE_INTERVAL {
            prune_cache(cache);
            save(cache);
            last_saved = completed;
        }
        progress(completed, total_misses);
    }

    Ok(embedded)
}

fn validate_passage_embeddings(texts: &[String], embeddings: &[Vec<f32>]) -> Result<()> {
    if embeddings.len() != texts.len() {
        return Err(AppError::ConfigError(format!(
            "semantic embedder returned {} passage embeddings for {} passages",
            embeddings.len(),
            texts.len()
        )));
    }
    let Some(dimensions) = embeddings.first().map(Vec::len) else {
        return Ok(());
    };
    if dimensions == 0
        || embeddings.iter().any(|embedding| {
            embedding.len() != dimensions || embedding.iter().any(|value| !value.is_finite())
        })
    {
        return Err(AppError::ConfigError(
            "semantic embedder returned empty, inconsistent, or non-finite passage embeddings"
                .to_string(),
        ));
    }
    Ok(())
}

fn save_pending_cache(
    cache: &mut EmbeddingCache,
    completed: usize,
    last_saved: &mut usize,
    save: &mut impl FnMut(&EmbeddingCache),
) {
    if completed > *last_saved {
        prune_cache(cache);
        save(cache);
        *last_saved = completed;
    }
}

fn prune_cache(cache: &mut EmbeddingCache) {
    prune_cache_to_limit(cache, MAX_CACHE_ENTRIES);
}

fn prune_cache_to_limit(cache: &mut EmbeddingCache, max_entries: usize) {
    let excess = cache.entries.len().saturating_sub(max_entries);
    if excess == 0 {
        return;
    }
    let mut oldest = cache
        .entries
        .iter()
        .map(|(key, entry)| (entry.protected, entry.last_used, key.clone()))
        .collect::<Vec<_>>();
    oldest.sort_unstable();
    for (_, _, key) in oldest.into_iter().take(excess) {
        cache.entries.remove(&key);
    }
}

fn embedded_chunk(chunk: &SemanticChunk, embedding: Vec<f32>) -> EmbeddedChunk {
    EmbeddedChunk {
        conversation_index: chunk.conversation_index,
        source: chunk.source,
        session: chunk.session.clone(),
        chunk_index: chunk.chunk_index,
        text: chunk.text.clone(),
        message_range: chunk.message_range,
        embedding,
    }
}

pub fn cached_chunks(
    chunks: Vec<SemanticChunk>,
    cache: &EmbeddingCache,
    cancellation: &crate::semantic::types::SemanticCancellationToken,
) -> Result<(Vec<EmbeddedChunk>, usize)> {
    let mut embedded = Vec::with_capacity(chunks.len());
    let mut misses = HashSet::new();

    for chunk in chunks {
        if cancellation.is_cancelled() {
            return Err(AppError::SemanticSearchCancelled);
        }
        let key = embedding_cache_key(&chunk.text);
        if let Some(entry) = cache.entries.get(&key) {
            embedded.push(embedded_chunk(&chunk, entry.embedding.clone()));
        } else {
            misses.insert(key);
        }
    }

    Ok((embedded, misses.len()))
}

pub fn cache_miss_count(chunks: &[SemanticChunk], cache: &EmbeddingCache) -> usize {
    chunks
        .iter()
        .map(|chunk| embedding_cache_key(&chunk.text))
        .filter(|key| !cache.entries.contains_key(key))
        .collect::<HashSet<_>>()
        .len()
}

pub fn embedding_cache_key(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

pub fn cache_contains_text(cache: &EmbeddingCache, text: &str) -> bool {
    cache.entries.contains_key(&embedding_cache_key(text))
}

pub fn read_embedding_cache(config: ChunkConfig) -> EmbeddingCache {
    let Some(path) = embedding_cache_path() else {
        return empty_embedding_cache(config);
    };
    read_embedding_cache_from_path(&path, config)
}

fn read_embedding_cache_from_path(path: &Path, config: ChunkConfig) -> EmbeddingCache {
    let Ok(data) = std::fs::read(path) else {
        return empty_embedding_cache(config);
    };
    let Ok(mut cache) = bincode::deserialize::<EmbeddingCache>(&data) else {
        return empty_embedding_cache(config);
    };
    if cache_matches_config(&cache, config) {
        if cache.entries.len() > MAX_CACHE_ENTRIES {
            prune_cache(&mut cache);
            write_embedding_cache_to_path(&cache, path);
        }
        cache
    } else {
        empty_embedding_cache(config)
    }
}

pub fn write_embedding_cache(cache: &EmbeddingCache) {
    let Some(path) = embedding_cache_path() else {
        return;
    };
    write_embedding_cache_to_path(cache, &path);
}

pub fn embedding_cache_file_path() -> Option<PathBuf> {
    embedding_cache_path()
}

pub fn model_cache_dir() -> PathBuf {
    semantic_cache_dir_with_fallback().join("fastembed")
}

pub fn clear_semantic_cache_files() -> std::io::Result<bool> {
    let Some(path) = semantic_cache_dir() else {
        return Ok(false);
    };
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_dir_all(path)?;
    Ok(true)
}

fn write_embedding_cache_to_path(cache: &EmbeddingCache, path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(data) = bincode::serialize(cache) else {
        return;
    };
    let Ok(mut tmp) = tempfile::NamedTempFile::new_in(parent) else {
        return;
    };
    if tmp.write_all(&data).is_err() {
        return;
    }
    let _ = tmp.persist(path);
}

pub fn empty_embedding_cache(config: ChunkConfig) -> EmbeddingCache {
    EmbeddingCache {
        schema_version: CACHE_SCHEMA_VERSION,
        model: MODEL_NAME.to_string(),
        chunk_target_chars: config.target_chars,
        chunk_overlap_chars: config.overlap_chars,
        chunk_context_turns: config.context_turns,
        access_counter: 0,
        entries: HashMap::new(),
    }
}

fn cache_matches_config(cache: &EmbeddingCache, config: ChunkConfig) -> bool {
    cache.schema_version == CACHE_SCHEMA_VERSION
        && cache.model == MODEL_NAME
        && cache.chunk_target_chars == config.target_chars
        && cache.chunk_overlap_chars == config.overlap_chars
        && cache.chunk_context_turns == config.context_turns
}

fn embedding_cache_path() -> Option<PathBuf> {
    semantic_cache_dir().map(|path| path.join("embeddings-v1.bin"))
}

fn semantic_cache_dir() -> Option<PathBuf> {
    home::home_dir().map(semantic_cache_dir_in)
}

fn semantic_cache_dir_with_fallback() -> PathBuf {
    semantic_cache_dir_in(home::home_dir().unwrap_or_else(|| PathBuf::from(".")))
}

fn semantic_cache_dir_in(home: PathBuf) -> PathBuf {
    home.join(".cache").join("claude-history").join("semantic")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::types::SemanticCancellationToken;

    struct FakeEmbedder {
        calls: usize,
    }

    impl SemanticEmbedder for FakeEmbedder {
        fn embed_passages(&mut self, passages: &[String]) -> Result<Vec<Vec<f32>>> {
            self.calls += 1;
            Ok(passages
                .iter()
                .map(|text| vec![text.len() as f32, 1.0])
                .collect())
        }

        fn embed_query(&mut self, _query: &str) -> Result<Option<Vec<f32>>> {
            Ok(Some(vec![1.0, 0.0]))
        }
    }

    struct CancellingEmbedder {
        cancellation: SemanticCancellationToken,
    }

    impl SemanticEmbedder for CancellingEmbedder {
        fn embed_passages(&mut self, passages: &[String]) -> Result<Vec<Vec<f32>>> {
            self.cancellation.cancel();
            Ok(passages.iter().map(|_| vec![1.0, 0.0]).collect())
        }

        fn embed_query(&mut self, _query: &str) -> Result<Option<Vec<f32>>> {
            Ok(Some(vec![1.0, 0.0]))
        }
    }

    fn chunk(key: &str, text: &str) -> SemanticChunk {
        let chunk_index = key
            .rsplit_once(':')
            .and_then(|(_, index)| index.parse::<usize>().ok())
            .unwrap_or(0);
        SemanticChunk {
            conversation_index: 0,
            source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
            session: "session".to_string(),
            chunk_index,
            text: text.to_string(),
            message_range: crate::history::MessageRange::single(1),
        }
    }

    fn cached() -> CachedChunk {
        CachedChunk {
            embedding: vec![0.5, 0.5],
            last_used: 0,
            protected: false,
        }
    }

    fn cache_text(cache: &mut EmbeddingCache, text: &str) {
        cache.entries.insert(embedding_cache_key(text), cached());
    }

    #[test]
    fn passage_embedding_validation_rejects_invalid_output() {
        let texts = vec!["alpha".to_string(), "beta".to_string()];

        assert!(validate_passage_embeddings(&texts, &[vec![1.0, 0.0]]).is_err());
        assert!(validate_passage_embeddings(&texts, &[vec![1.0], vec![1.0, 0.0]]).is_err());
        assert!(validate_passage_embeddings(&texts, &[vec![f32::NAN], vec![1.0]]).is_err());
    }

    #[test]
    fn cache_pruning_keeps_most_recent_entries() {
        let mut cache = empty_embedding_cache(ChunkConfig::default());
        for (text, last_used) in [("oldest", 1), ("middle", 2), ("newest", 3)] {
            cache.entries.insert(
                embedding_cache_key(text),
                CachedChunk {
                    embedding: vec![last_used as f32],
                    last_used,
                    protected: false,
                },
            );
        }

        prune_cache_to_limit(&mut cache, 2);

        assert_eq!(cache.entries.len(), 2);
        assert!(!cache_contains_text(&cache, "oldest"));
        assert!(cache_contains_text(&cache, "middle"));
        assert!(cache_contains_text(&cache, "newest"));
    }

    #[test]
    fn cache_pruning_preserves_protected_core_entries() {
        let mut cache = empty_embedding_cache(ChunkConfig::default());
        cache_text(&mut cache, "adaptive newest");
        cache
            .entries
            .get_mut(&embedding_cache_key("adaptive newest"))
            .unwrap()
            .last_used = 10;
        cache_text(&mut cache, "core oldest");
        let core = cache
            .entries
            .get_mut(&embedding_cache_key("core oldest"))
            .unwrap();
        core.last_used = 1;
        core.protected = true;

        prune_cache_to_limit(&mut cache, 1);

        assert!(cache_contains_text(&cache, "core oldest"));
        assert!(!cache_contains_text(&cache, "adaptive newest"));
    }

    #[test]
    fn embed_chunks_respects_new_embedding_budget() {
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        cache_text(&mut cache, "cached text");
        let chunks = std::iter::once(chunk("session:0", "cached text"))
            .chain((1..=40).map(|index| {
                chunk(
                    &format!("session:{index}"),
                    &format!("missing text {index}"),
                )
            }))
            .collect::<Vec<_>>();
        let mut embedder = FakeEmbedder { calls: 0 };

        let embedded = embed_chunks_with_budget_and_save(
            &mut embedder,
            chunks,
            &mut cache,
            &SemanticCancellationToken::new(),
            Some(32),
            |_, _| {},
            |_| {},
        )
        .expect("embedding succeeds");

        assert_eq!(embedder.calls, 1);
        assert_eq!(embedded.len(), 33);
        assert_eq!(cache.entries.len(), 33);
        assert!(cache_contains_text(&cache, "missing text 32"));
        assert!(!cache_contains_text(&cache, "missing text 33"));
    }

    #[test]
    fn embed_chunks_reuses_matching_cache_entry() {
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        cache_text(&mut cache, "cached text");
        let mut embedder = FakeEmbedder { calls: 0 };

        let embedded = embed_chunks_with_progress_and_save(
            &mut embedder,
            vec![chunk("session:0", "cached text")],
            &mut cache,
            &SemanticCancellationToken::new(),
            |_, _| {},
            |_| {},
        )
        .expect("embedding succeeds");

        assert_eq!(embedder.calls, 0);
        assert_eq!(embedded[0].embedding, vec![0.5, 0.5]);
        assert_eq!(embedded[0].chunk_index, 0);
    }

    #[test]
    fn embed_chunks_embeds_cache_misses() {
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        let mut embedder = FakeEmbedder { calls: 0 };

        let embedded = embed_chunks_with_progress_and_save(
            &mut embedder,
            vec![chunk("session:0", "new text")],
            &mut cache,
            &SemanticCancellationToken::new(),
            |_, _| {},
            |_| {},
        )
        .expect("embedding succeeds");

        assert_eq!(embedder.calls, 1);
        assert_eq!(embedded[0].embedding, vec![8.0, 1.0]);
        assert_eq!(embedded[0].chunk_index, 0);
        assert!(cache.entries.contains_key(&embedding_cache_key("new text")));
    }

    #[test]
    fn embed_chunks_reuses_content_across_chunk_keys() {
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        cache_text(&mut cache, "shared text");
        let mut embedder = FakeEmbedder { calls: 0 };

        let embedded = embed_chunks_with_progress_and_save(
            &mut embedder,
            vec![
                chunk("first-session:0", "shared text"),
                chunk("second-session:4", "shared text"),
            ],
            &mut cache,
            &SemanticCancellationToken::new(),
            |_, _| {},
            |_| {},
        )
        .expect("embedding succeeds");

        assert_eq!(embedder.calls, 0);
        assert_eq!(embedded.len(), 2);
        assert_eq!(cache.entries.len(), 1);
    }

    #[test]
    fn embed_chunks_embeds_duplicate_misses_once() {
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        let mut embedder = FakeEmbedder { calls: 0 };
        let mut progress = Vec::new();

        let embedded = embed_chunks_with_progress_and_save(
            &mut embedder,
            vec![
                chunk("first-session:0", "duplicate text"),
                chunk("second-session:0", "duplicate text"),
            ],
            &mut cache,
            &SemanticCancellationToken::new(),
            |completed, total| progress.push((completed, total)),
            |_| {},
        )
        .expect("embedding succeeds");

        assert_eq!(embedder.calls, 1);
        assert_eq!(embedded.len(), 2);
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(progress, vec![(1, 1)]);
    }

    #[test]
    fn embed_chunks_checkpoints_large_runs_and_saves_the_final_batch() {
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        let mut embedder = FakeEmbedder { calls: 0 };
        let chunks = (0..300)
            .map(|index| chunk(&format!("session:{index}"), &format!("text {index}")))
            .collect();
        let mut saved_entry_counts = Vec::new();

        embed_chunks_with_progress_and_save(
            &mut embedder,
            chunks,
            &mut cache,
            &SemanticCancellationToken::new(),
            |_, _| {},
            |cache| saved_entry_counts.push(cache.entries.len()),
        )
        .expect("embedding succeeds");

        assert_eq!(embedder.calls, 10);
        assert_eq!(saved_entry_counts, vec![256, 300]);
    }

    #[test]
    fn embed_chunks_saves_completed_work_when_cancelled() {
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        let cancellation = SemanticCancellationToken::new();
        let mut embedder = CancellingEmbedder {
            cancellation: cancellation.clone(),
        };
        let chunks = (0..33)
            .map(|index| chunk(&format!("session:{index}"), &format!("text {index}")))
            .collect();
        let mut saved_entry_counts = Vec::new();

        let result = embed_chunks_with_progress_and_save(
            &mut embedder,
            chunks,
            &mut cache,
            &cancellation,
            |_, _| {},
            |cache| saved_entry_counts.push(cache.entries.len()),
        );

        assert!(matches!(result, Err(AppError::SemanticSearchCancelled)));
        assert_eq!(saved_entry_counts, vec![32]);
    }

    #[test]
    fn changed_text_creates_a_distinct_cache_entry() {
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        cache_text(&mut cache, "old text");
        let mut embedder = FakeEmbedder { calls: 0 };

        let embedded = embed_chunks_with_progress_and_save(
            &mut embedder,
            vec![chunk("session:0", "new text")],
            &mut cache,
            &SemanticCancellationToken::new(),
            |_, _| {},
            |_| {},
        )
        .expect("embedding succeeds");

        let restored = cache
            .entries
            .get(&embedding_cache_key("new text"))
            .expect("cache entry");
        assert_eq!(embedder.calls, 1);
        assert_eq!(embedded[0].embedding, vec![8.0, 1.0]);
        assert_eq!(restored.embedding, vec![8.0, 1.0]);
        assert!(cache_contains_text(&cache, "old text"));
    }

    #[test]
    fn cache_config_mismatch_invalidates_cache() {
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        cache.chunk_target_chars += 1;

        assert!(!cache_matches_config(&cache, config));
    }

    #[test]
    fn wrong_schema_cache_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cache.bin");
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        cache.schema_version = CACHE_SCHEMA_VERSION - 1;
        cache_text(&mut cache, "stale");
        write_embedding_cache_to_path(&cache, &path);

        let restored = read_embedding_cache_from_path(&path, config);

        assert_eq!(restored.schema_version, CACHE_SCHEMA_VERSION);
        assert!(restored.entries.is_empty());
    }

    #[test]
    fn previous_cache_layout_is_ignored() {
        #[derive(serde::Serialize)]
        struct PreviousCache {
            schema_version: u32,
            model: String,
            chunk_target_chars: usize,
            chunk_overlap_chars: usize,
            chunk_context_turns: usize,
            entries: HashMap<String, PreviousEntry>,
        }

        #[derive(serde::Serialize)]
        struct PreviousEntry {
            file_size: u64,
            mtime_secs: u64,
            mtime_nsecs: u32,
            text: String,
            embedding: Vec<f32>,
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cache.bin");
        let config = ChunkConfig::default();
        let previous = PreviousCache {
            schema_version: CACHE_SCHEMA_VERSION - 1,
            model: MODEL_NAME.to_string(),
            chunk_target_chars: config.target_chars,
            chunk_overlap_chars: config.overlap_chars,
            chunk_context_turns: config.context_turns,
            entries: HashMap::from([(
                "session:0".to_string(),
                PreviousEntry {
                    file_size: 10,
                    mtime_secs: 20,
                    mtime_nsecs: 30,
                    text: "cached text".to_string(),
                    embedding: vec![0.5, 0.5],
                },
            )]),
        };
        std::fs::write(&path, bincode::serialize(&previous).unwrap()).unwrap();

        let restored = read_embedding_cache_from_path(&path, config);

        assert_eq!(restored.schema_version, CACHE_SCHEMA_VERSION);
        assert!(restored.entries.is_empty());
    }

    #[test]
    fn corrupt_cache_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cache.bin");
        let config = ChunkConfig::default();
        std::fs::write(&path, b"not bincode").expect("write corrupt cache");

        let restored = read_embedding_cache_from_path(&path, config);

        assert_eq!(restored.schema_version, CACHE_SCHEMA_VERSION);
        assert!(restored.entries.is_empty());
    }

    #[test]
    fn mismatched_config_cache_is_ignored_when_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cache.bin");
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        cache.chunk_overlap_chars += 1;
        cache_text(&mut cache, "stale");
        write_embedding_cache_to_path(&cache, &path);

        let restored = read_embedding_cache_from_path(&path, config);

        assert_eq!(restored.chunk_overlap_chars, config.overlap_chars);
        assert!(restored.entries.is_empty());
    }

    #[test]
    fn cache_round_trips_when_config_matches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cache.bin");
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        cache_text(&mut cache, "cached text");

        write_embedding_cache_to_path(&cache, &path);
        let restored = read_embedding_cache_from_path(&path, config);

        assert!(cache_contains_text(&restored, "cached text"));
    }

    #[test]
    fn cache_miss_count_matches_cached_chunks() {
        let config = ChunkConfig::default();
        let mut cache = empty_embedding_cache(config);
        cache_text(&mut cache, "cached text");
        cache_text(&mut cache, "old text");
        let chunks = vec![
            chunk("session:0", "cached text"),
            chunk("session:1", "new text"),
            chunk("session:2", "uncached text"),
        ];

        let (_, cached_chunk_misses) =
            cached_chunks(chunks.clone(), &cache, &SemanticCancellationToken::new()).unwrap();

        assert_eq!(cache_miss_count(&chunks, &cache), cached_chunk_misses);
        assert_eq!(cache_miss_count(&chunks, &cache), 2);
    }

    #[test]
    fn semantic_cache_paths_preserve_existing_locations() {
        let home = PathBuf::from("/home/example");
        let cache_dir = semantic_cache_dir_in(home);

        assert_eq!(
            cache_dir.join("embeddings-v1.bin"),
            PathBuf::from("/home/example/.cache/claude-history/semantic/embeddings-v1.bin")
        );
        assert_eq!(
            cache_dir.join("fastembed"),
            PathBuf::from("/home/example/.cache/claude-history/semantic/fastembed")
        );
    }
}
