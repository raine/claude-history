use super::*;

#[test]
fn ranks_original_indices_and_records_hits() {
    let conversations = vec![
        conversation("/projects/project-a/session-a.jsonl", vec!["visible alpha"]),
        conversation("/projects/project-a/session-b.jsonl", vec!["visible beta"]),
    ];
    let (query, candidates) = request("beta", conversations, vec![1, 0]);
    let request = index_request(&query, &candidates);
    let (mut state, mut embedder) = prepare_indexed_state(&request, ChunkConfig::default());

    let response = run_refresh(&mut state, &request, &mut embedder).expect("rank succeeds");

    assert_hit_indices(&response, &[1, 0]);
    let metadata = response
        .hits
        .iter()
        .find(|hit| hit.conversation_index == 1)
        .expect("beta hit");
    let (expected_score_breakdown, expected_explanation) = beta_hit_metadata(1, "session-b");
    assert_eq!(metadata.score_breakdown, expected_score_breakdown);
    assert_eq!(metadata.explanation, expected_explanation);
}

#[test]
fn literal_filters_require_hit_local_text() {
    let mut first = conversation("/projects/project-a/session-a.jsonl", vec!["visible alpha"]);
    first.full_text = "visible alpha conversation-level literal needle".to_string();
    let conversations = vec![first];
    let all = candidates_from(&conversations);
    let literals = vec![Literal::new("literal needle".to_string())];
    let query = "alpha".to_string();
    let request = SemanticIndexRequest {
        query: &query,
        literal_filters: &literals,
        full_corpus: &all,
        scope: &all,
        corpus_version: 1,
        prewarm: false,
    };
    let (mut state, mut embedder) = prepare_indexed_state(&request, ChunkConfig::default());

    let response = run_refresh(&mut state, &request, &mut embedder).expect("rank succeeds");

    assert_eq!(embedder.query_calls, 1);
    assert_eq!(response.progress, SemanticIndexProgress::Complete);
    assert!(response.hits.is_empty());
}

#[test]
fn lower_scoring_literal_chunk_can_survive_filtering() {
    let conversations = vec![conversation(
        "/projects/project-a/session-a.jsonl",
        vec!["visible alpha", "visible gamma literal needle"],
    )];
    let all = candidates_from(&conversations);
    let literals = vec![Literal::new("literal needle".to_string())];
    let query = "alpha".to_string();
    let request = SemanticIndexRequest {
        query: &query,
        literal_filters: &literals,
        full_corpus: &all,
        scope: &all,
        corpus_version: 1,
        prewarm: false,
    };
    let config = ChunkConfig {
        target_chars: 30,
        overlap_chars: 0,
        context_turns: 0,
    };
    let mut cache = empty_embedding_cache(config);
    for chunk in full_corpus_chunks(&request, config) {
        let embedding = if chunk.text.contains("alpha") {
            vec![1.0, 0.0]
        } else {
            vec![0.5, 0.5]
        };
        cache_passage(&mut cache, chunk.key, chunk.text, embedding);
    }
    let mut state = SemanticIndexState::with_cache(config, cache);
    let mut embedder = FakeEmbedder::new();

    let response = state
        .refresh_or_prewarm(
            &request,
            &mut embedder,
            &SemanticCancellationToken::new(),
            |_| {},
            |_| {},
        )
        .expect("rank succeeds");

    assert_eq!(response.hits.len(), 1);
    assert_eq!(
        response.hits[0].explanation.evidence_preview,
        "visible gamma literal needle"
    );
    assert_eq!(
        response.hits[0].message_range,
        crate::agent::refs::MessageRange::single(2)
    );
}

#[test]
fn literal_filter_no_match_is_not_empty_corpus() {
    let mut conversation =
        conversation("/projects/project-a/session-a.jsonl", vec!["visible alpha"]);
    conversation.full_text = "missing literal".to_string();
    let all = candidates_from(&[conversation]);
    let literals = vec![Literal::new("absent needle".to_string())];
    let query = "alpha".to_string();
    let request = SemanticIndexRequest {
        query: &query,
        literal_filters: &literals,
        full_corpus: &all,
        scope: &all,
        corpus_version: 1,
        prewarm: false,
    };
    let (mut state, mut embedder) = prepare_indexed_state(&request, ChunkConfig::default());

    let response = run_refresh(&mut state, &request, &mut embedder).expect("rank succeeds");

    assert!(response.hits.is_empty());
    assert_eq!(response.progress, SemanticIndexProgress::Complete);
}

#[test]
fn cache_hits_preserve_message_ranges() {
    let conversations = vec![conversation(
        "/projects/project-a/session-a.jsonl",
        vec!["visible alpha"],
    )];
    let (query, candidates) = request("alpha", conversations, vec![0]);
    let request = index_request(&query, &candidates);
    let (mut state, mut embedder) = prepare_indexed_state(&request, ChunkConfig::default());

    let response = run_refresh(&mut state, &request, &mut embedder).expect("cached rank succeeds");

    assert_eq!(embedder.passage_calls, 0);
    assert_eq!(
        response.hits[0].message_range,
        crate::agent::refs::MessageRange::single(1)
    );
}

#[test]
fn cache_hits_preserve_candidate_source() {
    let conversations = vec![conversation(
        "/projects/project-a/session-a.jsonl",
        vec!["visible alpha"],
    )];
    let mut candidates = candidates_from(&conversations);
    candidates[0].source = SemanticChunkSource::AgentSubagentDialogue;
    let query = "alpha".to_string();
    let request = index_request(&query, &candidates);
    let (mut state, mut embedder) = prepare_indexed_state(&request, ChunkConfig::default());

    let response = run_refresh(&mut state, &request, &mut embedder).expect("cached rank succeeds");

    assert_eq!(embedder.passage_calls, 0);
    assert_eq!(
        response.hits[0].explanation.chunk.source,
        SemanticChunkSource::AgentSubagentDialogue
    );
}

#[test]
fn source_aware_scope_filters_same_conversation_index_chunks() {
    let conversations = vec![conversation(
        "/projects/project-a/session-a.jsonl",
        vec!["visible alpha"],
    )];
    let visible = candidates_from(&conversations);
    let mut subagent = visible.clone();
    subagent[0].source = SemanticChunkSource::AgentSubagentDialogue;
    let mut all = visible.clone();
    all.extend(subagent);
    let query = "alpha".to_string();
    let request = SemanticIndexRequest {
        query: &query,
        literal_filters: &[],
        full_corpus: &all,
        scope: &visible,
        corpus_version: 1,
        prewarm: false,
    };
    let (mut state, mut embedder) = prepare_indexed_state(&request, ChunkConfig::default());

    let response = run_refresh(&mut state, &request, &mut embedder).expect("scoped rank succeeds");

    assert!(!response.hits.is_empty());
    assert!(
        response
            .hits
            .iter()
            .all(|hit| hit.explanation.chunk.source == SemanticChunkSource::VisibleDialogue)
    );
}

#[test]
fn reuses_passage_embeddings_for_same_candidate_signature() {
    let conversations = vec![
        conversation("/projects/project-a/session-a.jsonl", vec!["visible alpha"]),
        conversation("/projects/project-a/session-b.jsonl", vec!["visible beta"]),
    ];
    let (mut query, candidates) = request("alpha", conversations, vec![0, 1]);
    let mut request = index_request(&query, &candidates);
    let (mut state, mut embedder) = prepare_indexed_state(&request, ChunkConfig::default());

    run_refresh(&mut state, &request, &mut embedder).expect("first rank succeeds");
    query = "beta".to_string();
    request = index_request(&query, &candidates);
    run_refresh(&mut state, &request, &mut embedder).expect("second rank succeeds");

    assert_eq!(embedder.passage_calls, 0);
    assert_eq!(embedder.query_calls, 2);
}

#[test]
fn unchanged_signature_reuses_embeddings_until_semantic_turns_change() {
    let (mut query, mut candidates) = request(
        "alpha",
        vec![conversation(
            "/projects/project-a/session-a.jsonl",
            vec!["visible alpha"],
        )],
        vec![0],
    );
    let mut request = index_request(&query, &candidates);
    let (mut state, mut embedder) = prepare_indexed_state(&request, ChunkConfig::default());

    run_refresh(&mut state, &request, &mut embedder).expect("first rank succeeds");
    query = "beta".to_string();
    request = index_request(&query, &candidates);
    run_refresh(&mut state, &request, &mut embedder).expect("same signature rank succeeds");
    candidates = vec![SemanticIndexCandidate {
        index: 0,
        source: SemanticChunkSource::VisibleDialogue,
        conversation: Arc::new(conversation(
            "/projects/project-a/session-a.jsonl",
            vec!["visible beta"],
        )),
    }];
    request = index_request(&query, &candidates);
    cache_request_passages(&mut state.cache, &request);
    run_refresh(&mut state, &request, &mut embedder).expect("changed signature rank succeeds");

    assert_eq!(embedder.passage_calls, 0);
    assert_eq!(embedder.query_calls, 3);
    assert!(embedder.embedded_passages.is_empty());
}

#[test]
fn snippets_and_embeddings_use_only_semantic_turns() {
    let conversations = vec![conversation(
        "/projects/project-a/session-a.jsonl",
        vec!["visible alpha"],
    )];
    let (query, candidates) = request("alpha", conversations, vec![0]);
    let request = index_request(&query, &candidates);
    let (mut state, mut embedder) = prepare_indexed_state(&request, ChunkConfig::default());

    let response = run_refresh(&mut state, &request, &mut embedder).expect("rank succeeds");

    assert!(embedder.embedded_passages.is_empty());
    assert_eq!(
        response.hits[0].explanation.evidence_preview,
        "visible alpha"
    );
    assert!(
        !response.hits[0]
            .explanation
            .evidence_preview
            .contains("sentinel")
    );
}
