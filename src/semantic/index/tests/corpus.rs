use super::*;

#[test]
fn warm_full_corpus_reuses_embeddings_across_scope_toggles() {
    let conversations = vec![
        conversation("/projects/project-a/session-a.jsonl", vec!["visible alpha"]),
        conversation("/projects/project-a/session-b.jsonl", vec!["visible beta"]),
    ];
    let all = (0..conversations.len())
        .map(|index| SemanticIndexCandidate {
            index,
            source: SemanticChunkSource::VisibleDialogue,
            conversation: Arc::new(conversations[index].clone()),
        })
        .collect::<Vec<_>>();
    let alpha_scope = vec![all[0].clone()];
    let beta_scope = vec![all[1].clone()];
    let alpha_query = "alpha".to_string();
    let alpha_request = SemanticIndexRequest {
        query: &alpha_query,
        literal_filters: &[],
        full_corpus: &all,
        scope: &alpha_scope,
        corpus_version: 1,
        prewarm: false,
    };
    let (mut state, mut embedder) = prepare_indexed_state(&alpha_request, ChunkConfig::default());

    let alpha = run_refresh(&mut state, &alpha_request, &mut embedder).expect("alpha scope ranks");
    let beta_query = "beta".to_string();
    let beta_request = SemanticIndexRequest {
        query: &beta_query,
        literal_filters: &[],
        full_corpus: &all,
        scope: &beta_scope,
        corpus_version: 1,
        prewarm: false,
    };
    let beta = run_refresh(&mut state, &beta_request, &mut embedder).expect("beta scope ranks");

    assert_eq!(embedder.passage_calls, 0);
    assert_eq!(embedder.query_calls, 2);
    assert_eq!(alpha.indexed_chunk_count, 2);
    assert_eq!(beta.indexed_chunk_count, 2);
    assert_eq!(alpha.hits[0].conversation_index, 0);
    assert_eq!(beta.hits[0].conversation_index, 1);
}

#[test]
fn empty_scope_reuses_warm_corpus_without_query_embedding() {
    let conversations = vec![conversation(
        "/projects/project-a/session-a.jsonl",
        vec!["visible alpha"],
    )];
    let (query, candidates) = request("alpha", conversations, vec![0]);
    let populated = index_request(&query, &candidates);
    let (mut state, mut embedder) = prepare_indexed_state(&populated, ChunkConfig::default());
    run_refresh(&mut state, &populated, &mut embedder).expect("warm corpus");
    let empty_request = SemanticIndexRequest {
        query: &query,
        literal_filters: &[],
        full_corpus: &candidates,
        scope: &[],
        corpus_version: 1,
        prewarm: false,
    };

    let response =
        run_refresh(&mut state, &empty_request, &mut embedder).expect("empty scope returns");

    assert!(response.hits.is_empty());
    assert_eq!(response.indexed_chunk_count, 1);
    assert_eq!(response.progress, SemanticIndexProgress::EmptyCorpus);
    assert_eq!(embedder.passage_calls, 0);
    assert_eq!(embedder.query_calls, 1);
}

#[test]
fn persistent_scoped_ranking_matches_request_scoped_ranking() {
    let conversations = vec![
        conversation("/projects/project-a/session-a.jsonl", vec!["visible alpha"]),
        conversation("/projects/project-a/session-b.jsonl", vec!["visible beta"]),
        conversation("/projects/project-a/session-c.jsonl", vec!["visible gamma"]),
    ];
    let all = candidates_from(&conversations);
    let scope = vec![all[1].clone(), all[0].clone()];
    let query = "beta".to_string();
    let persistent_request = SemanticIndexRequest {
        query: &query,
        literal_filters: &[],
        full_corpus: &all,
        scope: &scope,
        corpus_version: 1,
        prewarm: false,
    };
    let scoped_request = index_request(&query, &scope);
    let (mut persistent_state, mut persistent_embedder) =
        prepare_indexed_state(&persistent_request, ChunkConfig::default());
    let (mut scoped_state, mut scoped_embedder) =
        prepare_indexed_state(&scoped_request, ChunkConfig::default());

    let persistent = run_refresh(
        &mut persistent_state,
        &persistent_request,
        &mut persistent_embedder,
    )
    .expect("persistent rank succeeds");
    let scoped = run_refresh(&mut scoped_state, &scoped_request, &mut scoped_embedder)
        .expect("scoped rank succeeds");

    assert_eq!(persistent.hits, scoped.hits);
}

#[test]
fn corpus_reorder_updates_hit_indices_without_reembedding() {
    let first = vec![
        conversation("/projects/project-a/session-a.jsonl", vec!["visible alpha"]),
        conversation("/projects/project-a/session-b.jsonl", vec!["visible beta"]),
    ];
    let first_all = candidates_from(&first);
    let query = "alpha".to_string();
    let first_request = SemanticIndexRequest {
        query: &query,
        literal_filters: &[],
        full_corpus: &first_all,
        scope: &first_all,
        corpus_version: 1,
        prewarm: false,
    };
    let (mut state, mut embedder) = prepare_indexed_state(&first_request, ChunkConfig::default());
    run_refresh(&mut state, &first_request, &mut embedder).expect("first corpus ranks");
    let reordered = vec![first[1].clone(), first[0].clone()];
    let reordered_all = candidates_from(&reordered);
    let reordered_request = SemanticIndexRequest {
        query: &query,
        literal_filters: &[],
        full_corpus: &reordered_all,
        scope: &reordered_all,
        corpus_version: 2,
        prewarm: false,
    };

    let response =
        run_refresh(&mut state, &reordered_request, &mut embedder).expect("reordered corpus ranks");

    assert_eq!(embedder.passage_calls, 0);
    assert_eq!(response.hits[0].conversation_index, 1);
    assert_eq!(response.hits[0].session, "session-a");
}

#[test]
fn changed_and_new_conversations_embed_without_reembedding_unchanged() {
    let first = vec![conversation(
        "/projects/project-a/session-a.jsonl",
        vec!["visible alpha"],
    )];
    let first_all = candidates_from(&first);
    let query = "alpha".to_string();
    let first_request = SemanticIndexRequest {
        query: &query,
        literal_filters: &[],
        full_corpus: &first_all,
        scope: &first_all,
        corpus_version: 1,
        prewarm: false,
    };
    let (mut state, mut embedder) = prepare_indexed_state(&first_request, ChunkConfig::default());
    run_refresh(&mut state, &first_request, &mut embedder).expect("first corpus ranks");
    let updated = vec![
        conversation("/projects/project-a/session-a.jsonl", vec!["visible delta"]),
        conversation("/projects/project-a/session-b.jsonl", vec!["visible beta"]),
        conversation("/projects/project-a/session-c.jsonl", vec!["visible gamma"]),
    ];
    let updated_all = candidates_from(&updated);
    let updated_request = SemanticIndexRequest {
        query: &query,
        literal_filters: &[],
        full_corpus: &updated_all,
        scope: &updated_all,
        corpus_version: 2,
        prewarm: false,
    };

    run_refresh(&mut state, &updated_request, &mut embedder).expect("updated corpus ranks");

    assert_eq!(embedder.passage_calls, 1);
    assert_eq!(
        embedder.embedded_passages,
        vec![vec![
            "visible delta".to_string(),
            "visible beta".to_string(),
            "visible gamma".to_string()
        ]]
    );
}

#[test]
fn empty_and_removed_conversations_are_excluded_after_corpus_update() {
    let first = vec![
        conversation("/projects/project-a/session-a.jsonl", vec!["visible alpha"]),
        conversation("/projects/project-a/session-b.jsonl", vec!["visible beta"]),
    ];
    let first_all = candidates_from(&first);
    let query = "alpha".to_string();
    let first_request = SemanticIndexRequest {
        query: &query,
        literal_filters: &[],
        full_corpus: &first_all,
        scope: &first_all,
        corpus_version: 1,
        prewarm: false,
    };
    let (mut state, mut embedder) = prepare_indexed_state(&first_request, ChunkConfig::default());
    run_refresh(&mut state, &first_request, &mut embedder).expect("first corpus ranks");
    let updated = vec![conversation("/projects/project-a/session-a.jsonl", vec![])];
    let updated_all = candidates_from(&updated);
    let updated_request = SemanticIndexRequest {
        query: &query,
        literal_filters: &[],
        full_corpus: &updated_all,
        scope: &updated_all,
        corpus_version: 2,
        prewarm: false,
    };

    let response = run_refresh(&mut state, &updated_request, &mut embedder)
        .expect("empty corpus update succeeds");

    assert!(response.hits.is_empty());
    assert_eq!(response.indexed_chunk_count, 0);
    assert_eq!(embedder.passage_calls, 0);
}

#[test]
fn cached_signature_reports_cache_ready_before_ranking() {
    let conversations = vec![conversation(
        "/projects/project-a/session-a.jsonl",
        vec!["visible alpha"],
    )];
    let (query, candidates) = request("alpha", conversations, vec![0]);
    let request = index_request(&query, &candidates);
    let (mut state, mut embedder) = prepare_indexed_state(&request, ChunkConfig::default());
    run_refresh(&mut state, &request, &mut embedder).expect("first rank succeeds");
    let mut progress = Vec::new();

    run_refresh_with_observers(
        &mut state,
        &request,
        &mut embedder,
        |status| progress.push(status),
        |_| {},
    )
    .expect("second rank succeeds");

    assert_eq!(
        progress,
        vec![
            SemanticIndexProgress::CacheReady,
            SemanticIndexProgress::Ranking
        ]
    );
    assert_eq!(embedder.passage_calls, 0);
}

#[test]
fn clear_empty_replaces_populated_index_state() {
    let populated = vec![conversation(
        "/projects/project-a/session-a.jsonl",
        vec!["visible alpha"],
    )];
    let (query, populated_candidates) = request("alpha", populated, vec![0]);
    let populated_request = index_request(&query, &populated_candidates);
    let (mut state, mut embedder) =
        prepare_indexed_state(&populated_request, ChunkConfig::default());
    run_refresh(&mut state, &populated_request, &mut embedder).expect("populated index succeeds");
    let empty = vec![conversation("/projects/project-a/session-a.jsonl", vec![])];
    let (empty_query, empty_candidates) = request("alpha", empty, vec![0]);
    let empty_request = index_request(&empty_query, &empty_candidates);

    let empty_signature = semantic_index_signature(&empty_request, ChunkConfig::default());
    state
        .clear_empty(&empty_request, &SemanticCancellationToken::new())
        .unwrap();

    assert_eq!(state.signature, Some(empty_signature));
    assert_eq!(state.resident_conversation_count(), 0);
    assert!(
        !state
            .has_chunks(&empty_request, &SemanticCancellationToken::new())
            .unwrap()
    );
}

#[test]
fn empty_visible_dialogue_returns_without_embedding() {
    let conversations = vec![conversation("/projects/project-a/session-a.jsonl", vec![])];
    let (query, candidates) = request("alpha", conversations, vec![0]);
    let request = index_request(&query, &candidates);
    let (mut state, mut embedder) = prepare_empty_state(ChunkConfig::default());

    let response = run_refresh(&mut state, &request, &mut embedder).expect("empty corpus succeeds");

    assert!(response.hits.is_empty());
    assert_eq!(response.progress, SemanticIndexProgress::EmptyCorpus);
    assert_eq!(embedder.passage_calls, 0);
    assert_eq!(embedder.query_calls, 0);
    assert!(
        !state
            .has_chunks(&request, &SemanticCancellationToken::new())
            .unwrap()
    );
}
/// Embeds like `FakeEmbedder`, then cancels the shared token: the first
/// batch succeeds and the refresh is cancelled before the second one.
struct CancelAfterFirstBatchEmbedder {
    inner: FakeEmbedder,
    token: SemanticCancellationToken,
}

impl SemanticEmbedder for CancelAfterFirstBatchEmbedder {
    fn embed_passages(&mut self, passages: &[String]) -> Result<Vec<Vec<f32>>> {
        let embeddings = self.inner.embed_passages(passages)?;
        self.token.cancel();
        Ok(embeddings)
    }

    fn embed_query(&mut self, query: &str) -> Result<Option<Vec<f32>>> {
        self.inner.embed_query(query)
    }
}

fn many_conversations(count: usize) -> Vec<Conversation> {
    (0..count)
        .map(|index| {
            let path = format!("/projects/project-a/session-{index}.jsonl");
            let turn = if index == 0 {
                "visible alpha".to_string()
            } else {
                format!("filler {index}")
            };
            conversation(&path, vec![turn.as_str()])
        })
        .collect()
}

#[test]
fn cancelled_refresh_resumes_from_the_cache_without_reembedding() {
    // 33 single-chunk conversations: batch 1 covers 0..32, batch 2 only 32.
    let conversations = many_conversations(33);
    let candidates = candidates_from(&conversations);
    let request = index_request("alpha", &candidates);
    let (mut state, _) = prepare_empty_state(ChunkConfig::default());
    let token = SemanticCancellationToken::new();
    let mut embedder = CancelAfterFirstBatchEmbedder {
        inner: FakeEmbedder::new(),
        token: token.child(),
    };
    let Err(error) = state.refresh_passages(&request, &mut embedder, &token, |_| {}, |_| {}) else {
        panic!("second batch is cancelled");
    };

    assert!(matches!(error, AppError::SemanticSearchCancelled));
    assert_eq!(embedder.inner.passage_calls, 1);
    assert_eq!(state.resident_conversation_count(), 0);

    let mut resumed = FakeEmbedder::new();
    let response = state
        .refresh_or_prewarm(
            &request,
            &mut resumed,
            &SemanticCancellationToken::new(),
            |_| {},
            |_| {},
        )
        .expect("resumed refresh completes");
    assert_eq!(resumed.passage_calls, 1);
    assert_eq!(
        resumed.embedded_passages,
        vec![vec!["filler 32".to_string()]]
    );
    assert_eq!(state.resident_conversation_count(), 33);
    assert_eq!(response.indexed_chunk_count, 33);
    assert_eq!(response.hits[0].conversation_index, 0);
}

#[test]
fn bounded_refresh_keeps_partial_corpus_rankable_and_completes_later() {
    let conversations = vec![
        conversation("/projects/project-a/session-a.jsonl", vec!["visible alpha"]),
        conversation("/projects/project-a/session-b.jsonl", vec!["visible beta"]),
    ];
    let (query, cached_only) = request("alpha", conversations.clone(), vec![0]);
    let cached_request = index_request(&query, &cached_only);
    let (mut state, mut embedder) = prepare_indexed_state(&cached_request, ChunkConfig::default());

    let candidates = candidates_from(&conversations);
    let full_request = index_request("alpha", &candidates);
    let bounded = state
        .refresh_or_prewarm_with_budget(
            &full_request,
            &mut embedder,
            &SemanticCancellationToken::new(),
            Some(0),
            |_| {},
            |_| {},
        )
        .expect("bounded refresh ranks what is cached");

    assert_eq!(embedder.passage_calls, 0);
    assert_eq!(bounded.missing_chunk_count, 1);
    assert_eq!(bounded.indexed_chunk_count, 1);
    assert_hit_indices(&bounded, &[0]);

    let completed = state
        .refresh_or_prewarm_with_budget(
            &full_request,
            &mut embedder,
            &SemanticCancellationToken::new(),
            Some(1),
            |_| {},
            |_| {},
        )
        .expect("next refresh completes the missing conversation");

    assert_eq!(embedder.passage_calls, 1);
    assert_eq!(
        embedder.embedded_passages,
        vec![vec!["visible beta".to_string()]]
    );
    assert_eq!(completed.missing_chunk_count, 0);
    assert_eq!(completed.indexed_chunk_count, 2);
    assert_hit_indices(&completed, &[0, 1]);

    let warm = state
        .refresh_or_prewarm(
            &full_request,
            &mut embedder,
            &SemanticCancellationToken::new(),
            |_| {},
            |_| {},
        )
        .expect("warm corpus ranks");
    assert_eq!(embedder.passage_calls, 1);
    assert_hit_indices(&warm, &[0, 1]);
}
