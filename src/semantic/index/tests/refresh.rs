use super::*;

#[test]
fn missing_cached_passages_are_embedded_and_ranked() {
    let conversations = vec![conversation(
        "/projects/project-a/session-a.jsonl",
        vec!["visible alpha"],
    )];
    let (query, candidates) = request("alpha", conversations, vec![0]);
    let request = index_request(&query, &candidates);
    let (mut state, mut embedder) = prepare_empty_state(ChunkConfig::default());
    let mut save_calls = 0;
    let mut progress = Vec::new();

    let response = run_refresh_with_observers(
        &mut state,
        &request,
        &mut embedder,
        |status| progress.push(status),
        |_| save_calls += 1,
    )
    .expect("missing cache embeds and ranks");

    assert_eq!(response.hits[0].conversation_index, 0);
    assert_eq!(response.progress, SemanticIndexProgress::Complete);
    assert_eq!(embedder.passage_calls, 1);
    assert_eq!(embedder.query_calls, 1);
    assert_eq!(save_calls, 1);
    assert_eq!(
        cache_miss_count(
            &semantic_chunks(&request, ChunkConfig::default()),
            &state.cache
        ),
        0
    );
    assert!(progress.contains(&SemanticIndexProgress::Embedding {
        completed: 0,
        total: 1,
    }));
}

#[test]
fn partial_cached_passages_embed_missing_chunks_and_rank_all() {
    let conversations = vec![
        conversation("/projects/project-a/session-a.jsonl", vec!["visible alpha"]),
        conversation("/projects/project-a/session-b.jsonl", vec!["visible beta"]),
    ];
    let (query, candidates) = request("alpha", conversations, vec![0, 1]);
    let request = index_request(&query, &candidates);
    let mut cache = empty_embedding_cache(ChunkConfig::default());
    let first_chunk = semantic_chunks(&request, ChunkConfig::default())
        .into_iter()
        .find(|chunk| chunk.text == "visible alpha")
        .expect("alpha chunk");
    cache_passage(
        &mut cache,
        first_chunk.key,
        first_chunk.text,
        vec![1.0, 0.0],
    );
    let mut state = SemanticIndexState::with_cache(ChunkConfig::default(), cache);
    let mut embedder = FakeEmbedder::new();
    let mut progress = Vec::new();

    let response = state
        .refresh_or_prewarm(
            &request,
            &mut embedder,
            &SemanticCancellationToken::new(),
            |status| progress.push(status),
            |_| {},
        )
        .expect("partial cache embeds misses and ranks all chunks");

    let filtered = response
        .hits
        .iter()
        .map(|hit| hit.conversation_index)
        .collect::<Vec<_>>();
    assert_eq!(filtered, vec![0, 1]);
    assert_eq!(response.progress, SemanticIndexProgress::Complete);
    assert_eq!(embedder.passage_calls, 1);
    assert_eq!(embedder.query_calls, 1);
    assert_eq!(
        embedder.embedded_passages,
        vec![vec!["visible beta".to_string()]]
    );
    assert!(progress.contains(&SemanticIndexProgress::Embedding {
        completed: 0,
        total: 1,
    }));
}

#[test]
fn refresh_passages_can_skip_per_batch_cache_saves() {
    let conversations = vec![conversation(
        "/projects/project-a/session-a.jsonl",
        vec!["visible alpha"],
    )];
    let (query, candidates) = request("alpha", conversations, vec![0]);
    let request = index_request(&query, &candidates);
    let (mut state, mut embedder) = prepare_empty_state(ChunkConfig::default());

    let response =
        run_refresh_passages(&mut state, &request, &mut embedder).expect("refresh succeeds");

    assert_eq!(response.indexed_chunk_count, 1);
    assert_eq!(response.progress, SemanticIndexProgress::CacheReady);
    assert_eq!(embedder.passage_calls, 1);
    assert_eq!(embedder.query_calls, 0);
    assert_eq!(
        cache_miss_count(
            &semantic_chunks(&request, ChunkConfig::default()),
            &state.cache
        ),
        0
    );
}

#[test]
fn rank_refreshed_index_reports_missing_query_embedding() {
    let conversations = vec![conversation(
        "/projects/project-a/session-a.jsonl",
        vec!["visible alpha"],
    )];
    let (query, candidates) = request("alpha", conversations, vec![0]);
    let request = index_request(&query, &candidates);
    let (mut state, mut embedder) = prepare_empty_state(ChunkConfig::default());
    run_refresh_passages(&mut state, &request, &mut embedder).expect("refresh succeeds");
    embedder.query_embedding = None;

    let response = state
        .rank_refreshed(
            &request,
            &mut embedder,
            &SemanticCancellationToken::new(),
            |_| {},
        )
        .expect("rank succeeds");

    assert!(response.hits.is_empty());
    assert!(!response.query_embedding_returned);
    assert_eq!(response.indexed_chunk_count, 1);
    assert_eq!(embedder.passage_calls, 1);
    assert_eq!(embedder.query_calls, 1);
}

#[test]
fn prewarm_request_builds_cache_without_ranking_query() {
    let conversations = vec![conversation(
        "/projects/project-a/session-a.jsonl",
        vec!["visible alpha"],
    )];
    let (query, candidates) = request("", conversations, vec![0]);
    let request = SemanticIndexRequest {
        query: &query,
        literal_filters: &[],
        full_corpus: &candidates,
        scope: &candidates,
        corpus_version: 1,
        prewarm: true,
    };
    let (mut state, mut embedder) = prepare_empty_state(ChunkConfig::default());

    let response = run_refresh(&mut state, &request, &mut embedder).expect("prewarm succeeds");

    assert!(response.hits.is_empty());
    assert_eq!(response.indexed_chunk_count, 1);
    assert_eq!(response.progress, SemanticIndexProgress::CacheReady);
    assert_eq!(embedder.passage_calls, 1);
    assert_eq!(embedder.query_calls, 0);
    assert_eq!(
        cache_miss_count(
            &semantic_chunks(&request, ChunkConfig::default()),
            &state.cache
        ),
        0
    );
}

#[test]
#[ignore]
fn bench_rank_refreshed_20k_chunks() {
    const N: usize = 20_000;
    const DIM: usize = 384;
    let filler = "The assistant explained how the Index keeps resident embeddings per conversation \
and why Semantic ranking borrows chunks instead of copying them, then listed the cache \
invalidation rules with Examples in Portuguese: acentuação, configuração e validação. ";
    let conversations = (0..N)
        .map(|i| {
            let text = format!("passage {i}: {}", filler.repeat(6));
            conversation(
                &format!("/projects/bench/session-{i}.jsonl"),
                vec![text.as_str()],
            )
        })
        .collect::<Vec<_>>();
    let candidates = candidates_from(&conversations);
    let query = "semantic index cache".to_string();
    let request = index_request(&query, &candidates);
    let mut cache = empty_embedding_cache(ChunkConfig::default());
    for (i, chunk) in full_corpus_chunks(&request, ChunkConfig::default())
        .into_iter()
        .enumerate()
    {
        let embedding = (0..DIM)
            .map(|d| ((i * 31 + d * 7) % 97) as f32 / 97.0)
            .collect::<Vec<_>>();
        cache_passage(&mut cache, chunk.key, chunk.text, embedding);
    }
    let mut state = SemanticIndexState::with_cache(ChunkConfig::default(), cache);
    let mut embedder = FakeEmbedder::new();
    embedder.query_embedding = Some((0..DIM).map(|d| (d % 13) as f32 / 13.0).collect());

    let t0 = std::time::Instant::now();
    run_refresh_passages(&mut state, &request, &mut embedder).expect("refresh");
    let refresh_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let t1 = std::time::Instant::now();
    let rounds = 5;
    let mut hits = 0;
    for _ in 0..rounds {
        let response = state
            .rank_refreshed(
                &request,
                &mut embedder,
                &SemanticCancellationToken::new(),
                |_| {},
            )
            .expect("rank");
        hits = response.hits.len();
    }
    let rank_ms = t1.elapsed().as_secs_f64() * 1000.0 / rounds as f64;

    let t2 = std::time::Instant::now();
    run_refresh_passages(&mut state, &request, &mut embedder).expect("refresh again");
    let refresh_again_ms = t2.elapsed().as_secs_f64() * 1000.0;

    eprintln!(
        "BENCH chunks={} text_bytes={} refresh_ms={refresh_ms:.1} rank_ms={rank_ms:.1} refresh_again_ms={refresh_again_ms:.2} hits={hits}",
        state.indexed_chunk_count(),
        conversations[0].semantic_turns[0].len()
    );
}
#[test]
fn reports_indexed_chunk_count() {
    let conversations = vec![
        conversation("/projects/project-a/session-a.jsonl", vec!["visible alpha"]),
        conversation("/projects/project-a/session-b.jsonl", vec!["visible beta"]),
    ];
    let (query, candidates) = request("beta", conversations, vec![1, 0]);
    let request = index_request(&query, &candidates);
    let (mut state, mut embedder) = prepare_indexed_state(&request, ChunkConfig::default());

    let response = run_refresh(&mut state, &request, &mut embedder).expect("rank succeeds");

    assert_eq!(response.indexed_chunk_count, 2);
    assert_hit_indices(&response, &[1, 0]);
}

#[test]
fn semantic_index_ranks_more_than_legacy_limit_without_cap() {
    const LEGACY_LIMIT: usize = 100;
    let conversations = (0..LEGACY_LIMIT + 25)
        .map(|index| {
            conversation(
                &format!("/projects/project-a/session-{index}.jsonl"),
                vec!["visible alpha"],
            )
        })
        .collect::<Vec<_>>();
    let candidate_indices = (0..conversations.len()).collect::<Vec<_>>();
    let (query, candidates) = request("alpha", conversations, candidate_indices);
    let request = index_request(&query, &candidates);
    let (mut state, mut embedder) = prepare_indexed_state(&request, ChunkConfig::default());

    let response = run_refresh(&mut state, &request, &mut embedder).expect("rank succeeds");

    assert_eq!(response.indexed_chunk_count, LEGACY_LIMIT + 25);
    assert_eq!(response.hits.len(), LEGACY_LIMIT + 25);
    assert_hit_indices(&response, &(0..LEGACY_LIMIT + 25).collect::<Vec<_>>());
}
