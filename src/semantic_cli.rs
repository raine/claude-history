use crate::error::{AppError, Result};
use crate::history::Conversation;
use crate::search::literal::{build_literal_corpus, exact_fallback};
use crate::search::query::ParsedQuery;
use crate::semantic::filter::filter_embedded_chunks_by_literals;
use crate::semantic::types::SemanticCancellationToken;

pub fn run(query: &str, conversations: &[Conversation], top: usize, local: bool) -> Result<()> {
    use crate::semantic::cache::write_embedding_cache;
    use crate::semantic::fastembed::FastembedEmbedder;
    use crate::semantic::index::{SemanticIndexRequest, SemanticIndexState};
    use crate::semantic::output::format_hit;
    use crate::semantic::types::MODEL_NAME;

    let selected = select_conversations(conversations, local)?;
    if selected.is_empty() {
        eprintln!("{}", no_conversations_message(local));
        return Ok(());
    }

    let parsed = ParsedQuery::parse(query);
    if parsed.is_effectively_empty() {
        eprintln!("Query is empty.");
        return Ok(());
    }
    if parsed.is_quoted_only() {
        let results = exact_literal_indices(&selected, &parsed);
        for (rank, index) in results.iter().take(top).enumerate() {
            eprintln!("{}", format_exact_hit(rank + 1, selected[*index]));
        }
        if results.is_empty() {
            eprintln!("No semantic matches found.");
        }
        return Ok(());
    }

    let candidates = semantic_index_candidates(&selected);
    let request = SemanticIndexRequest {
        query: parsed.semantic_text(),
        literal_filters: parsed.literals(),
        full_corpus: &candidates,
        scope: &candidates,
        corpus_version: 1,
        prewarm: false,
    };
    let mut state = SemanticIndexState::new();
    let cancellation = SemanticCancellationToken::new();
    if !state.has_chunks(&request, &cancellation)? {
        state.clear_empty(&request, &cancellation)?;
        eprintln!("No visible dialogue text available for semantic search.");
        return Ok(());
    }

    let mut embedder = FastembedEmbedder::new()?;
    let (refresh, response) =
        refresh_and_rank_interactive(&request, &mut state, &mut embedder, write_embedding_cache)?;

    eprintln!(
        "Semantic search: searching {} cached chunk(s) from {} recent conversation(s) with fastembed {MODEL_NAME}",
        refresh.indexed_chunk_count,
        selected.len()
    );

    if !response.query_embedding_returned {
        eprintln!("No query embedding returned.");
        return Ok(());
    }

    for (rank, hit) in response.hits.iter().take(top).enumerate() {
        eprintln!("{}", format_hit(rank + 1, hit, &selected));
    }

    if response.hits.is_empty() {
        eprintln!("No semantic matches found.");
    }

    Ok(())
}

pub fn clear_cache() -> Result<()> {
    let cleared = crate::semantic::cache::clear_semantic_cache_files()?;
    if cleared {
        eprintln!("Semantic cache cleared.");
    } else {
        eprintln!("Semantic cache is already empty.");
    }
    Ok(())
}

pub fn generate_cache(conversations: &[Conversation], local: bool) -> Result<()> {
    use crate::semantic::cache::{MAX_CACHE_ENTRIES, embedding_cache_key, write_embedding_cache};
    use crate::semantic::chunk::build_chunks_with_sources;
    use crate::semantic::fastembed::FastembedEmbedder;
    use crate::semantic::index::{SemanticIndexProgress, SemanticIndexRequest, SemanticIndexState};
    use crate::semantic::types::ChunkConfig;

    let selected = select_conversations(conversations, local)?;
    if selected.is_empty() {
        eprintln!("{}", no_conversations_message(local));
        return Ok(());
    }

    let candidates = semantic_cache_candidates(&selected);
    let chunks = build_chunks_with_sources(
        candidates.iter().map(|candidate| {
            (
                candidate.index,
                candidate.source,
                candidate.conversation.as_ref(),
            )
        }),
        ChunkConfig::default(),
    );
    let chunk_count = chunks.len();
    let core_keys = chunks
        .iter()
        .map(|chunk| embedding_cache_key(&chunk.text))
        .collect::<std::collections::HashSet<_>>();
    let unique_chunk_count = core_keys.len();
    if unique_chunk_count > MAX_CACHE_ENTRIES {
        return Err(AppError::ConfigError(format!(
            "semantic core has {unique_chunk_count} unique passages, exceeding the {MAX_CACHE_ENTRIES}-entry cache capacity"
        )));
    }
    if chunk_count == 0 {
        eprintln!("No semantic dialogue or route text available for cache generation.");
        return Ok(());
    }

    let request = SemanticIndexRequest {
        query: "",
        literal_filters: &[],
        full_corpus: &candidates,
        scope: &candidates,
        corpus_version: 1,
        prewarm: true,
    };
    let mut state = SemanticIndexState::new();
    for (key, entry) in &mut state.cache.entries {
        entry.protected &= core_keys.contains(key);
    }
    eprintln!(
        "Semantic cache: checking {chunk_count} chunk(s) from {} recent conversation(s).",
        selected.len()
    );

    let mut embedder = FastembedEmbedder::new()?;
    let response = state.refresh_or_prewarm(
        &request,
        &mut embedder,
        &SemanticCancellationToken::new(),
        |progress| {
            if let SemanticIndexProgress::Embedding { completed, total } = progress
                && total > 0
                && completed > 0
            {
                eprintln!("Semantic cache: embedded {completed}/{total} changed chunk(s)");
            }
        },
        write_embedding_cache,
    )?;
    for key in &core_keys {
        if let Some(entry) = state.cache.entries.get_mut(key) {
            entry.protected = true;
        }
    }
    write_embedding_cache(&state.cache);

    eprintln!(
        "Semantic cache: cached {} chunk(s) from {} recent conversation(s).",
        response.indexed_chunk_count.min(chunk_count),
        selected.len()
    );

    Ok(())
}

pub fn debug_search(query: &str, conversations: &[Conversation], local: bool) -> Result<()> {
    use crate::semantic::cache::{
        cache_contains_text, cache_miss_count, cached_chunks, embedding_cache_file_path,
        read_embedding_cache,
    };
    use crate::semantic::chunk::build_chunks;
    use crate::semantic::embed::SemanticEmbedder;
    use crate::semantic::fastembed::FastembedEmbedder;
    use crate::semantic::output::{format_hit, truncate};
    use crate::semantic::rank::rank_chunks;
    use crate::semantic::types::{ChunkConfig, MODEL_NAME, SemanticCancellationToken};
    use std::collections::HashMap;

    let parsed = ParsedQuery::parse(query);
    let selected = select_conversations(conversations, local)?;
    eprint!("{}", format_parsed_query(&parsed));
    eprintln!();
    eprintln!(
        "Semantic debug: selected {} conversation(s).",
        selected.len()
    );
    eprintln!(
        "Semantic debug: model cache: {}",
        FastembedEmbedder::cache_dir().display()
    );
    match embedding_cache_file_path() {
        Some(path) => eprintln!("Semantic debug: embedding cache: {}", path.display()),
        None => eprintln!("Semantic debug: embedding cache: unavailable"),
    }

    if selected.is_empty() {
        eprintln!("{}", no_conversations_message(local));
        return Ok(());
    }

    if parsed.is_effectively_empty() {
        eprintln!("Semantic debug: query is empty.");
        return Ok(());
    }

    let turn_count = selected
        .iter()
        .map(|conversation| conversation.semantic_turns.len())
        .sum::<usize>();
    let chunk_config = ChunkConfig::default();
    let chunks = build_chunks(&selected, chunk_config);
    eprintln!(
        "Semantic debug: {} semantic turn(s), {} chunk(s), target={} overlap={} context_turns={}",
        turn_count,
        chunks.len(),
        chunk_config.target_chars,
        chunk_config.overlap_chars,
        chunk_config.context_turns
    );

    let mut chunk_counts = HashMap::<usize, usize>::new();
    for chunk in &chunks {
        *chunk_counts.entry(chunk.conversation_index).or_default() += 1;
    }
    let mut chunk_counts = chunk_counts.into_iter().collect::<Vec<_>>();
    chunk_counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (rank, (index, count)) in chunk_counts.iter().take(10).enumerate() {
        let conversation = selected[*index];
        let title = conversation
            .custom_title
            .as_deref()
            .or(conversation.summary.as_deref())
            .unwrap_or(&conversation.preview);
        eprintln!(
            "Semantic debug: chunk-heavy #{:2}: {} chunk(s) | {} | {}",
            rank + 1,
            count,
            conversation.path.display(),
            title
        );
    }

    let suspicious = chunks
        .iter()
        .filter(|chunk| {
            chunk.text.contains("```")
                || chunk.text.contains("<system-reminder>")
                || chunk.text.contains("<local-command-stdout>")
                || chunk.text.contains("<command-message>")
        })
        .take(5)
        .collect::<Vec<_>>();
    if suspicious.is_empty() {
        eprintln!(
            "Semantic debug: no sampled chunks contain code fences or command/system markers."
        );
    } else {
        for chunk in suspicious {
            eprintln!(
                "Semantic debug: suspicious chunk {}:{}: {}",
                chunk.session,
                chunk.chunk_index,
                truncate(&chunk.text, 220)
            );
        }
    }

    if parsed.is_quoted_only() {
        let results = exact_literal_indices(&selected, &parsed);
        eprintln!("Semantic debug: exact literal matches={}.", results.len());
        for (rank, index) in results.iter().take(20).enumerate() {
            eprintln!("{}", format_exact_hit(rank + 1, selected[*index]));
        }
        if results.is_empty() {
            eprintln!("Semantic debug: no semantic matches found.");
        }
        return Ok(());
    }

    if chunks.is_empty() {
        eprintln!("Semantic debug: no visible dialogue text available.");
        return Ok(());
    }

    let cache = read_embedding_cache(chunk_config);
    let missing = cache_miss_count(&chunks, &cache);
    let cached_count = chunks.len().saturating_sub(missing);
    eprintln!(
        "Semantic debug: cache entries={}, hits={}, misses={}",
        cache.entries.len(),
        cached_count,
        missing
    );
    for chunk in chunks
        .iter()
        .filter(|chunk| !cache_contains_text(&cache, &chunk.text))
        .take(5)
    {
        eprintln!(
            "Semantic debug: missing cache chunk {}:{}: {}",
            chunk.session,
            chunk.chunk_index,
            truncate(&chunk.text, 180)
        );
    }

    let (embedded_chunks, _) = cached_chunks(chunks, &cache, &SemanticCancellationToken::new())?;
    if embedded_chunks.is_empty() {
        eprintln!("Semantic debug: no cached chunks available for ranking.");
        return Ok(());
    }

    let mut embedder = FastembedEmbedder::new()?;
    let Some(query_embedding) = embedder.embed_query(parsed.semantic_text())? else {
        eprintln!("Semantic debug: no query embedding returned.");
        return Ok(());
    };
    let embedded_chunks = filter_embedded_chunks_by_literals(embedded_chunks, parsed.literals());
    let hits = rank_chunks(
        parsed.semantic_text(),
        &query_embedding,
        &embedded_chunks,
        &SemanticCancellationToken::new(),
    )?;
    eprintln!(
        "Semantic debug: ranked {} cached chunk(s) with fastembed {MODEL_NAME}.",
        embedded_chunks.len()
    );

    for (rank, hit) in hits.iter().take(20).enumerate() {
        eprintln!("{}", format_hit(rank + 1, hit, &selected));
    }
    if hits.is_empty() {
        eprintln!("Semantic debug: no semantic matches found.");
    }

    Ok(())
}

fn format_exact_hit(rank: usize, conversation: &Conversation) -> String {
    let project = conversation.project_name.as_deref().unwrap_or("(none)");
    let title = conversation
        .custom_title
        .as_deref()
        .or(conversation.summary.as_deref())
        .unwrap_or(&conversation.preview);
    let session = conversation
        .path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("?");
    format!("#{rank:2} exact | {project} | {session}\n     {title}\n")
}

fn exact_literal_indices(conversations: &[&Conversation], parsed: &ParsedQuery) -> Vec<usize> {
    let plain_conversations = conversations
        .iter()
        .map(|conversation| (*conversation).clone())
        .collect::<Vec<_>>();
    let corpus = build_literal_corpus(&plain_conversations);
    exact_fallback(&plain_conversations, &corpus, parsed.literals(), |_| true)
}

fn format_parsed_query(parsed: &ParsedQuery) -> String {
    let mut output = format!("intent: {:?}\n", parsed.unquoted());
    if parsed.literals().is_empty() {
        output.push_str("literals: none\n");
    } else {
        output.push_str("literals:\n");
        for literal in parsed.literals() {
            output.push_str(&format!(
                "  {:?} ({:?})\n",
                literal.text(),
                literal.case_mode()
            ));
        }
    }
    output
}

fn refresh_and_rank_interactive(
    request: &crate::semantic::index::SemanticIndexRequest<'_>,
    state: &mut crate::semantic::index::SemanticIndexState,
    embedder: &mut dyn crate::semantic::embed::SemanticEmbedder,
    mut save_cache: impl FnMut(&crate::semantic::types::EmbeddingCache),
) -> Result<(
    crate::semantic::index::SemanticIndexResponse,
    crate::semantic::index::SemanticIndexResponse,
)> {
    let cancellation = SemanticCancellationToken::new();
    let refresh = state.refresh_passages(request, embedder, &cancellation, |_| {}, |_| {})?;
    save_cache(&state.cache);
    let response = state.rank_refreshed(request, embedder, &cancellation, |_| {})?;
    Ok((refresh, response))
}

fn semantic_cache_candidates(
    selected: &[&Conversation],
) -> Vec<crate::semantic::index::SemanticIndexCandidate> {
    // One read for the whole corpus, which is what the cache covers. Without
    // the annotation candidates, a note's chunk embeds at the first query that
    // scopes it rather than during prewarm.
    cache_candidates_with(selected, &annotations_by_path(selected))
}

fn cache_candidates_with(
    selected: &[&Conversation],
    annotations: &std::collections::HashMap<
        std::path::PathBuf,
        crate::annotations::ConversationAnnotations,
    >,
) -> Vec<crate::semantic::index::SemanticIndexCandidate> {
    let mut candidates = semantic_index_candidates(selected);
    for (index, conversation) in selected.iter().enumerate() {
        if !conversation.semantic_turns.is_empty()
            && let Some(route) =
                crate::agent::service::agent_route_semantic_conversation(conversation)
        {
            candidates.push(crate::semantic::index::SemanticIndexCandidate {
                index,
                source: crate::semantic::types::SemanticChunkSource::AgentRoute,
                conversation: std::sync::Arc::new(route),
            });
        }
        if let Some(found) = annotations.get(&conversation.path)
            && let Some(annotated) =
                crate::agent::service::annotation_semantic_conversation(conversation, found)
        {
            candidates.push(crate::semantic::index::SemanticIndexCandidate {
                index,
                source: crate::semantic::types::SemanticChunkSource::Annotation,
                conversation: std::sync::Arc::new(annotated),
            });
        }
    }
    candidates
}

/// Annotations for the selected conversations, keyed by transcript path.
fn annotations_by_path(
    selected: &[&Conversation],
) -> std::collections::HashMap<std::path::PathBuf, crate::annotations::ConversationAnnotations> {
    let paths = selected
        .iter()
        .map(|conversation| conversation.path.as_path())
        .collect::<Vec<_>>();
    crate::annotations::AnnotatorSet::from_current_config()
        .read_all(&paths)
        .into_iter()
        .collect()
}

fn semantic_index_candidates(
    selected: &[&Conversation],
) -> Vec<crate::semantic::index::SemanticIndexCandidate> {
    selected
        .iter()
        .enumerate()
        .map(
            |(index, conversation)| crate::semantic::index::SemanticIndexCandidate {
                index,
                source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
                conversation: std::sync::Arc::new((*conversation).clone()),
            },
        )
        .collect()
}

fn select_conversations(conversations: &[Conversation], local: bool) -> Result<Vec<&Conversation>> {
    let current_project_dir_name = if local {
        let dir = std::env::current_dir().map_err(AppError::Io)?;
        Some(crate::history::convert_path_to_project_dir_name(&dir))
    } else {
        None
    };

    let mut selected = Vec::new();
    for conversation in conversations {
        if let Some(ref project) = current_project_dir_name {
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

        selected.push(conversation);
    }
    Ok(selected)
}

fn no_conversations_message(local: bool) -> &'static str {
    if local {
        "No conversations available for semantic search in the current workspace."
    } else {
        "No conversations available for semantic search."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::embed::SemanticEmbedder;
    use crate::semantic::test_fixtures::SemanticConversationFixture;
    use crate::semantic::types::{ChunkConfig, EmbeddingCache};
    use chrono::Local;

    struct FakeEmbedder {
        passage_calls: usize,
        query_calls: usize,
        query_embedding: Option<Vec<f32>>,
        query_error: bool,
    }

    impl FakeEmbedder {
        fn new(query_embedding: Option<Vec<f32>>) -> Self {
            Self {
                passage_calls: 0,
                query_calls: 0,
                query_embedding,
                query_error: false,
            }
        }
    }

    impl SemanticEmbedder for FakeEmbedder {
        fn embed_passages(&mut self, passages: &[String]) -> Result<Vec<Vec<f32>>> {
            self.passage_calls += 1;
            Ok(passages
                .iter()
                .map(|passage| match passage.as_str() {
                    "visible beta" => vec![0.0, 1.0],
                    _ => vec![1.0, 0.0],
                })
                .collect())
        }

        fn embed_query(&mut self, query: &str) -> Result<Option<Vec<f32>>> {
            self.query_calls += 1;
            if self.query_error {
                return Err(AppError::ConfigError("query failed".to_string()));
            }
            Ok(if query.contains("beta") {
                Some(vec![0.0, 1.0])
            } else {
                self.query_embedding.clone()
            })
        }
    }

    fn test_conversation(path: &str, title: &str, semantic_turns: Vec<String>) -> Conversation {
        SemanticConversationFixture::new(path, semantic_turns)
            .with_title(title)
            .with_project("project-a", "/projects/project-a")
            .with_cwd(None)
            .with_summary(None)
            .build()
    }

    fn interactive_refresh_context(
        query: &str,
        query_embedding: Option<Vec<f32>>,
        query_error: bool,
    ) -> (
        String,
        Vec<crate::semantic::index::SemanticIndexCandidate>,
        crate::semantic::index::SemanticIndexState,
        FakeEmbedder,
        Vec<usize>,
    ) {
        let conversations = vec![test_conversation(
            "/projects/project-a/session-1.jsonl",
            "one",
            vec!["visible alpha".to_string()],
        )];
        let selected = vec![&conversations[0]];
        let candidates = semantic_index_candidates(&selected);
        let cache = crate::semantic::cache::empty_embedding_cache(ChunkConfig::default());
        let state =
            crate::semantic::index::SemanticIndexState::with_cache(ChunkConfig::default(), cache);
        let mut embedder = FakeEmbedder::new(query_embedding);
        embedder.query_error = query_error;

        (query.to_string(), candidates, state, embedder, Vec::new())
    }

    fn interactive_refresh_request<'a>(
        query: &'a String,
        candidates: &'a [crate::semantic::index::SemanticIndexCandidate],
    ) -> crate::semantic::index::SemanticIndexRequest<'a> {
        crate::semantic::index::SemanticIndexRequest {
            query,
            literal_filters: &[],
            full_corpus: candidates,
            scope: candidates,
            corpus_version: 1,
            prewarm: false,
        }
    }

    #[test]
    fn interactive_refresh_writes_cache_once_before_query_ranking() {
        let (query, candidates, mut state, mut embedder, mut saved_entry_counts) =
            interactive_refresh_context("alpha", None, false);
        let request = interactive_refresh_request(&query, &candidates);

        let (refresh, response) = refresh_and_rank_interactive(
            &request,
            &mut state,
            &mut embedder,
            |cache: &EmbeddingCache| saved_entry_counts.push(cache.entries.len()),
        )
        .expect("interactive refresh and rank succeeds");

        assert_eq!(refresh.indexed_chunk_count, 1);
        assert_eq!(saved_entry_counts, vec![1]);
        assert!(!response.query_embedding_returned);
        assert_eq!(embedder.passage_calls, 1);
        assert_eq!(embedder.query_calls, 1);
    }

    #[test]
    fn interactive_refresh_writes_cache_before_query_error() {
        let (query, candidates, mut state, mut embedder, mut saved_entry_counts) =
            interactive_refresh_context("alpha", Some(vec![1.0, 0.0]), true);
        let request = interactive_refresh_request(&query, &candidates);

        let error = match refresh_and_rank_interactive(
            &request,
            &mut state,
            &mut embedder,
            |cache: &EmbeddingCache| saved_entry_counts.push(cache.entries.len()),
        ) {
            Ok(_) => panic!("query error should propagate"),
            Err(error) => error,
        };

        assert_eq!(saved_entry_counts, vec![1]);
        assert_eq!(embedder.passage_calls, 1);
        assert_eq!(embedder.query_calls, 1);
        assert!(error.to_string().contains("query failed"));
    }

    #[test]
    fn selection_includes_all_conversations() {
        let conversations = vec![
            test_conversation(
                "/projects/project-a/session-1.jsonl",
                "one",
                vec!["one".to_string()],
            ),
            test_conversation(
                "/projects/project-a/session-2.jsonl",
                "two",
                vec!["two".to_string()],
            ),
        ];

        let selected = select_conversations(&conversations, false).expect("select conversations");

        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].custom_title.as_deref(), Some("one"));
        assert_eq!(selected[1].custom_title.as_deref(), Some("two"));
    }

    #[test]
    fn selection_filters_to_current_workspace_when_local() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(dir.path()).expect("set cwd");
        let project = crate::history::convert_path_to_project_dir_name(
            &std::env::current_dir().expect("current temp cwd"),
        );
        let conversations = vec![
            test_conversation(
                &format!("projects/{project}/session-1.jsonl"),
                "local",
                vec!["local".to_string()],
            ),
            test_conversation(
                "projects/other-project/session-2.jsonl",
                "other",
                vec!["other".to_string()],
            ),
        ];

        let selected = select_conversations(&conversations, true).expect("select conversations");
        std::env::set_current_dir(cwd).expect("restore cwd");

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].custom_title.as_deref(), Some("local"));
    }

    #[test]
    fn empty_conversation_message_mentions_local_scope() {
        assert_eq!(
            no_conversations_message(true),
            "No conversations available for semantic search in the current workspace."
        );
        assert_eq!(
            no_conversations_message(false),
            "No conversations available for semantic search."
        );
    }

    #[test]
    fn semantic_index_candidates_use_selected_slice_indices() {
        let conversations = vec![
            test_conversation(
                "/projects/project-a/session-1.jsonl",
                "one",
                vec!["one".to_string()],
            ),
            test_conversation(
                "/projects/project-a/session-2.jsonl",
                "two",
                vec!["two".to_string()],
            ),
        ];
        let selected = vec![&conversations[1], &conversations[0]];

        let candidates = semantic_index_candidates(&selected);

        assert_eq!(candidates[0].index, 0);
        assert_eq!(
            candidates[0].conversation.custom_title.as_deref(),
            Some("two")
        );
        assert_eq!(candidates[1].index, 1);
        assert_eq!(
            candidates[1].conversation.custom_title.as_deref(),
            Some("one")
        );
    }

    #[test]
    fn empty_corpus_returns_before_model_initialization() {
        run("cache", &[], 1, false).expect("empty corpus returns");
    }

    #[test]
    fn quoted_only_exact_fallback_returns_literal_matches_newest_first() {
        let mut older = test_conversation(
            "/projects/project-a/session-1.jsonl",
            "literal needle",
            vec![],
        );
        older.timestamp = Local::now() - chrono::Duration::days(1);
        let newer = test_conversation(
            "/projects/project-a/session-2.jsonl",
            "literal needle",
            vec![],
        );
        let miss = test_conversation("/projects/project-a/session-3.jsonl", "other", vec![]);
        let conversations = vec![older, newer, miss];
        let selected = conversations.iter().collect::<Vec<_>>();
        let parsed = ParsedQuery::parse("\"literal needle\"");

        let results = exact_literal_indices(&selected, &parsed);

        assert_eq!(results, vec![1, 0]);
    }

    #[test]
    fn quoted_uuid_exact_fallback_is_literal_search() {
        let uuid = "e7d318b1-4274-4ee2-a341-e94893b5df49";
        let conversations = vec![
            test_conversation("/projects/project-a/session-1.jsonl", uuid, vec![]),
            test_conversation("/projects/project-a/session-2.jsonl", "other", vec![]),
        ];
        let selected = conversations.iter().collect::<Vec<_>>();
        let parsed = ParsedQuery::parse(&format!("\"{uuid}\""));

        let results = exact_literal_indices(&selected, &parsed);

        assert_eq!(parsed.semantic_text(), "");
        assert_eq!(results, vec![0]);
    }

    #[test]
    fn semantic_filters_use_smart_case_literals() {
        let conversations = vec![
            test_conversation(
                "/projects/project-a/session-1.jsonl",
                "restaurant_signals lower phrase",
                vec!["visible alpha restaurant_signals".to_string()],
            ),
            test_conversation(
                "/projects/project-a/session-2.jsonl",
                "RESTAURANT_SIGNALS lower phrase",
                vec!["visible alpha".to_string()],
            ),
        ];
        let chunks = conversations
            .iter()
            .enumerate()
            .map(
                |(conversation_index, conversation)| crate::semantic::types::EmbeddedChunk {
                    conversation_index,
                    source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
                    session: format!("session-{}", conversation_index + 1),
                    chunk_index: 0,
                    key: format!("session-{conversation_index}:0"),
                    text: conversation.semantic_turns[0].clone(),
                    message_range: crate::agent::refs::MessageRange::single(1),
                    embedding: vec![1.0, 0.0],
                },
            )
            .collect::<Vec<_>>();

        let insensitive = filter_embedded_chunks_by_literals(
            chunks.clone(),
            ParsedQuery::parse("alpha \"restaurant_signals\"").literals(),
        );
        let sensitive = filter_embedded_chunks_by_literals(
            chunks,
            ParsedQuery::parse("alpha \"RESTAURANT_SIGNALS\"").literals(),
        );

        assert_eq!(insensitive.len(), 1);
        assert_eq!(insensitive[0].conversation_index, 0);
        assert!(sensitive.is_empty());
    }

    #[test]
    fn parsed_query_debug_output_reports_intent_and_literals() {
        let parsed = ParsedQuery::parse("alpha \"RESTAURANT_SIGNALS\" \"lower phrase\"");

        let output = format_parsed_query(&parsed);

        assert!(output.contains("intent: \"alpha\""));
        assert!(output.contains("\"RESTAURANT_SIGNALS\" (Sensitive)"));
        assert!(output.contains("\"lower phrase\" (Insensitive)"));
    }

    #[test]
    fn the_cache_covers_a_conversation_s_annotations() {
        let conversation =
            SemanticConversationFixture::new("/tmp/session.jsonl", ["dialogue"]).build();
        let selected = vec![&conversation];
        let mut annotations = std::collections::HashMap::new();
        annotations.insert(
            conversation.path.clone(),
            crate::annotations::ConversationAnnotations::from_flat(vec![
                crate::annotations::Annotation {
                    id: "a".to_string(),
                    targets: vec![crate::annotations::TargetSpan::single(4)],
                    kind: "recap".to_string(),
                    text: "cache rewrite reverted".to_string(),
                    annotator: "chsum".to_string(),
                },
            ]),
        );

        let candidates = cache_candidates_with(&selected, &annotations);

        // Without an annotation candidate here, the note's chunk embeds at the
        // first query that scopes it rather than during prewarm.
        let annotation_candidates = candidates
            .iter()
            .filter(|candidate| {
                candidate.source == crate::semantic::types::SemanticChunkSource::Annotation
            })
            .collect::<Vec<_>>();
        assert_eq!(annotation_candidates.len(), 1);
        assert_eq!(
            annotation_candidates[0].conversation.semantic_turns,
            vec!["cache rewrite reverted".to_string()]
        );
    }

    #[test]
    fn a_conversation_without_annotations_adds_no_candidate() {
        let conversation =
            SemanticConversationFixture::new("/tmp/session.jsonl", ["dialogue"]).build();

        let candidates = cache_candidates_with(&[&conversation], &std::collections::HashMap::new());

        assert!(candidates.iter().all(|candidate| {
            candidate.source != crate::semantic::types::SemanticChunkSource::Annotation
        }));
    }
}
