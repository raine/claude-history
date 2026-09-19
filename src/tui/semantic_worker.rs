use crate::error::Result;
use crate::history::Conversation;
use crate::search::literal::{build_literal_corpus, exact_fallback};
use crate::search::query::ParsedQuery;
use crate::semantic::cache::write_embedding_cache;
use crate::semantic::fastembed::FastembedEmbedder;
use crate::semantic::index::{
    SemanticIndexCandidate, SemanticIndexProgress, SemanticIndexRequest, SemanticIndexResponse,
    SemanticIndexState,
};
use crate::semantic::types::{
    SemanticCancellationToken, SemanticChunkIdentity, SemanticExplanation, SemanticQuality,
    SemanticRationaleKind, SemanticScoreBreakdown,
};
use crate::tui::app::{SemanticProgress, SemanticResultMetadata};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc;

#[derive(Clone)]
pub enum SemanticWorkerCommand {
    UpdateCorpus {
        corpus_version: u64,
        conversations: Arc<Vec<Arc<Conversation>>>,
    },
    UpdateScope {
        corpus_version: u64,
        scope_version: u64,
        indices: Arc<Vec<usize>>,
    },
    Search {
        generation: u64,
        query: ParsedQuery,
        corpus_version: u64,
        scope_version: u64,
        prewarm: bool,
    },
}

#[derive(Clone)]
struct SemanticSearchRequest {
    generation: u64,
    query: ParsedQuery,
    corpus_version: u64,
    scope_version: u64,
    prewarm: bool,
}

pub enum SemanticSearchMessage {
    Progress {
        generation: u64,
        progress: SemanticProgress,
    },
    Complete(SemanticSearchResponse),
}

pub struct SemanticSearchResponse {
    pub generation: u64,
    pub filtered: Vec<usize>,
    pub metadata: HashMap<usize, SemanticResultMetadata>,
    pub error: Option<String>,
    pub progress: SemanticProgress,
    pub prewarm: bool,
}

pub fn spawn_semantic_worker() -> (
    mpsc::Sender<SemanticWorkerCommand>,
    mpsc::Receiver<SemanticSearchMessage>,
    SemanticCancellationToken,
) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<SemanticWorkerCommand>();
    let (res_tx, res_rx) = mpsc::channel::<SemanticSearchMessage>();
    let cancellation = SemanticCancellationToken::new();
    let worker_cancellation = cancellation.clone();

    std::thread::Builder::new()
        .name("semantic-search-worker".into())
        .spawn(move || {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .thread_name(|_| "semantic-rank-worker".to_string())
                .build()
                .expect("failed to create semantic ranking thread pool");
            pool.install(|| run_semantic_worker(cmd_rx, res_tx, worker_cancellation));
        })
        .expect("failed to spawn semantic search worker thread");

    (cmd_tx, res_rx, cancellation)
}

fn run_semantic_worker(
    cmd_rx: mpsc::Receiver<SemanticWorkerCommand>,
    res_tx: mpsc::Sender<SemanticSearchMessage>,
    cancellation_source: SemanticCancellationToken,
) {
    let mut worker = SemanticWorkerState::default();
    let mut state = SemanticIndexState::new();
    let mut embedder: Option<FastembedEmbedder> = None;

    while let Ok(command) = cmd_rx.recv() {
        let mut request = worker.apply_command(command, &cancellation_source);
        while let Ok(pending) = cmd_rx.try_recv() {
            if let Some(search) = worker.apply_command(pending, &cancellation_source) {
                request = Some(search);
            }
        }

        let Some(request) = request else {
            continue;
        };
        let Some(version_state) = worker.version_state_for(&request) else {
            continue;
        };
        match version_state {
            VersionState::Future => {
                worker.pending_search = Some(request);
                continue;
            }
            VersionState::Stale => continue,
            VersionState::Current => {}
        }

        let cancellation = cancellation_source.child();
        if request.query.is_effectively_empty() && !request.prewarm {
            let _ = res_tx.send(SemanticSearchMessage::Complete(SemanticSearchResponse {
                generation: request.generation,
                filtered: Vec::new(),
                metadata: HashMap::new(),
                error: None,
                progress: SemanticProgress::Idle,
                prewarm: request.prewarm,
            }));
            continue;
        }
        if request.query.is_quoted_only() {
            let response = exact_literal_semantic_response(
                request.generation,
                request.prewarm,
                worker.corpus.as_ref(),
                worker.scope.as_ref(),
                &request.query,
            );
            let _ = res_tx.send(SemanticSearchMessage::Complete(response));
            continue;
        }
        let full_corpus = worker.full_corpus_candidates();
        let scope = worker.semantic_candidates();
        let index_request = SemanticIndexRequest {
            query: request.query.semantic_text(),
            literal_filters: request.query.literals(),
            full_corpus: &full_corpus,
            scope: &scope,
            corpus_version: request.corpus_version,
            prewarm: request.prewarm,
            include_chunk_hits: false,
        };
        let response = match state.has_chunks(&index_request, &cancellation) {
            Ok(true) => {
                if embedder.is_none() {
                    let _ = res_tx.send(SemanticSearchMessage::Progress {
                        generation: request.generation,
                        progress: SemanticProgress::InitializingModel,
                    });
                    embedder = match FastembedEmbedder::new_quiet() {
                        Ok(embedder) => Some(embedder),
                        Err(error) => {
                            let _ = res_tx.send(SemanticSearchMessage::Complete(
                                failed_semantic_response(
                                    request.generation,
                                    request.prewarm,
                                    error.to_string(),
                                ),
                            ));
                            continue;
                        }
                    };
                }
                rank_or_prewarm_semantic_request(
                    request.generation,
                    &index_request,
                    &request.query,
                    &mut state,
                    embedder.as_mut().unwrap(),
                    &cancellation,
                    &res_tx,
                )
                .unwrap_or_else(|error| {
                    failed_semantic_response(request.generation, request.prewarm, error.to_string())
                })
            }
            Ok(false) => match state.clear_empty(&index_request, &cancellation) {
                Ok(()) => empty_semantic_response(request.generation, request.prewarm),
                Err(error) => {
                    failed_semantic_response(request.generation, request.prewarm, error.to_string())
                }
            },
            Err(error) => {
                failed_semantic_response(request.generation, request.prewarm, error.to_string())
            }
        };

        let _ = res_tx.send(SemanticSearchMessage::Complete(response));
    }
}

#[derive(Default)]
struct SemanticWorkerState {
    corpus_version: u64,
    scope_version: u64,
    scope_corpus_version: u64,
    corpus: Arc<Vec<Arc<Conversation>>>,
    scope: Arc<Vec<usize>>,
    pending_search: Option<SemanticSearchRequest>,
}

enum VersionState {
    Current,
    Future,
    Stale,
}

impl SemanticWorkerState {
    fn apply_command(
        &mut self,
        command: SemanticWorkerCommand,
        cancellation: &SemanticCancellationToken,
    ) -> Option<SemanticSearchRequest> {
        match command {
            SemanticWorkerCommand::UpdateCorpus {
                corpus_version,
                conversations,
            } => {
                if corpus_version >= self.corpus_version {
                    self.corpus_version = corpus_version;
                    self.corpus = conversations;
                    cancellation.cancel();
                }
                self.take_ready_pending()
            }
            SemanticWorkerCommand::UpdateScope {
                corpus_version,
                scope_version,
                indices,
            } => {
                if scope_version >= self.scope_version {
                    self.scope_corpus_version = corpus_version;
                    self.scope_version = scope_version;
                    self.scope = indices;
                    cancellation.cancel();
                }
                self.take_ready_pending()
            }
            SemanticWorkerCommand::Search {
                generation,
                query,
                corpus_version,
                scope_version,
                prewarm,
            } => {
                cancellation.cancel();
                Some(SemanticSearchRequest {
                    generation,
                    query,
                    corpus_version,
                    scope_version,
                    prewarm,
                })
            }
        }
    }

    fn take_ready_pending(&mut self) -> Option<SemanticSearchRequest> {
        let request = self.pending_search.take()?;
        match self.version_state_for(&request) {
            Some(VersionState::Current) | Some(VersionState::Stale) => Some(request),
            Some(VersionState::Future) | None => {
                self.pending_search = Some(request);
                None
            }
        }
    }

    fn version_state_for(&self, request: &SemanticSearchRequest) -> Option<VersionState> {
        if self.scope_corpus_version != self.corpus_version {
            return Some(VersionState::Future);
        }
        if self.corpus_version == request.corpus_version
            && self.scope_version == request.scope_version
        {
            Some(VersionState::Current)
        } else if self.corpus_version < request.corpus_version
            || self.scope_version < request.scope_version
        {
            Some(VersionState::Future)
        } else {
            Some(VersionState::Stale)
        }
    }

    fn full_corpus_candidates(&self) -> Vec<SemanticIndexCandidate> {
        self.corpus
            .iter()
            .enumerate()
            .map(|(index, conversation)| SemanticIndexCandidate {
                index,
                source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
                conversation: conversation.clone(),
            })
            .collect()
    }

    fn semantic_candidates(&self) -> Vec<SemanticIndexCandidate> {
        self.scope
            .iter()
            .filter_map(|&index| {
                self.corpus
                    .get(index)
                    .map(|conversation| SemanticIndexCandidate {
                        index,
                        source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
                        conversation: conversation.clone(),
                    })
            })
            .collect()
    }
}

fn rank_or_prewarm_semantic_request(
    generation: u64,
    request: &SemanticIndexRequest<'_>,
    parsed: &ParsedQuery,
    state: &mut SemanticIndexState,
    embedder: &mut FastembedEmbedder,
    cancellation: &SemanticCancellationToken,
    res_tx: &mpsc::Sender<SemanticSearchMessage>,
) -> Result<SemanticSearchResponse> {
    let response = state.refresh_or_prewarm(
        request,
        embedder,
        cancellation,
        |progress| {
            let _ = res_tx.send(SemanticSearchMessage::Progress {
                generation,
                progress: semantic_progress(progress),
            });
        },
        write_embedding_cache,
    )?;
    Ok(semantic_search_response(
        generation,
        response,
        request.full_corpus,
        parsed,
    ))
}

fn semantic_search_response(
    generation: u64,
    response: SemanticIndexResponse,
    _conversations: &[SemanticIndexCandidate],
    _parsed: &ParsedQuery,
) -> SemanticSearchResponse {
    let filtered = response
        .hits
        .iter()
        .map(|hit| hit.conversation_index)
        .collect::<Vec<_>>();
    let metadata = response
        .hits
        .into_iter()
        .map(|hit| {
            let explanation = hit.explanation;
            (
                hit.conversation_index,
                SemanticResultMetadata {
                    score_breakdown: hit.score_breakdown,
                    explanation,
                },
            )
        })
        .collect();

    SemanticSearchResponse {
        generation,
        filtered,
        metadata,
        error: None,
        progress: semantic_progress(response.progress),
        prewarm: response.prewarm,
    }
}

fn empty_semantic_response(generation: u64, prewarm: bool) -> SemanticSearchResponse {
    SemanticSearchResponse {
        generation,
        filtered: Vec::new(),
        metadata: HashMap::new(),
        error: None,
        progress: SemanticProgress::EmptyCorpus,
        prewarm,
    }
}

fn exact_literal_semantic_response(
    generation: u64,
    prewarm: bool,
    conversations: &[Arc<Conversation>],
    scope: &[usize],
    parsed: &ParsedQuery,
) -> SemanticSearchResponse {
    let plain_conversations = conversations
        .iter()
        .map(|conversation| conversation.as_ref().clone())
        .collect::<Vec<_>>();
    let corpus = build_literal_corpus(&plain_conversations);
    let scope = scope
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let filtered = exact_fallback(&plain_conversations, &corpus, parsed.literals(), |index| {
        scope.contains(&index)
    });

    let metadata = filtered
        .iter()
        .filter_map(|index| {
            conversations
                .get(*index)
                .map(|conversation| (*index, exact_literal_metadata(conversation, parsed)))
        })
        .collect();

    SemanticSearchResponse {
        generation,
        filtered,
        metadata,
        error: None,
        progress: SemanticProgress::Complete,
        prewarm,
    }
}

fn literal_evidence_preview(
    conversation: &Conversation,
    parsed: &ParsedQuery,
    visible_preview: &str,
) -> String {
    parsed
        .literals()
        .iter()
        .filter(|literal| !literal.matches(visible_preview))
        .chain(parsed.literals().iter())
        .find_map(|literal| {
            literal
                .match_ranges(&conversation.full_text)
                .first()
                .map(|range| evidence_window(&conversation.full_text, *range))
                .or_else(|| {
                    conversation.project_name.as_deref().and_then(|project| {
                        literal
                            .match_ranges(project)
                            .first()
                            .map(|range| evidence_window(project, *range))
                    })
                })
        })
        .unwrap_or_else(|| conversation.preview.clone())
}

fn evidence_window(text: &str, range: (usize, usize)) -> String {
    const CONTEXT_CHARS: usize = 80;
    let (start, end) = range;
    let prefix_start = text[..start]
        .char_indices()
        .rev()
        .nth(CONTEXT_CHARS)
        .map(|(index, _)| index)
        .unwrap_or(0);
    let suffix_end = text[end..]
        .char_indices()
        .nth(CONTEXT_CHARS)
        .map(|(index, _)| end + index)
        .unwrap_or(text.len());
    let mut preview = String::new();
    if prefix_start > 0 {
        preview.push('…');
    }
    preview.push_str(text[prefix_start..suffix_end].trim());
    if suffix_end < text.len() {
        preview.push('…');
    }
    preview
}

fn exact_literal_metadata(
    conversation: &Conversation,
    parsed: &ParsedQuery,
) -> SemanticResultMetadata {
    SemanticResultMetadata {
        score_breakdown: SemanticScoreBreakdown {
            hybrid: 0.0,
            semantic: 0.0,
            lexical: 0.0,
        },
        explanation: SemanticExplanation {
            quality: SemanticQuality::Strong,
            quality_label: "exact",
            matched_terms: parsed
                .literals()
                .iter()
                .map(|literal| literal.text().to_string())
                .collect(),
            evidence_preview: literal_evidence_preview(conversation, parsed, ""),
            rationale_kind: SemanticRationaleKind::LexicalBoosted,
            chunk: SemanticChunkIdentity {
                conversation_index: conversation.index,
                source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
                session: conversation
                    .path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("?")
                    .to_string(),
                chunk_index: 0,
                message_range: crate::history::MessageRange::single(1),
            },
        },
    }
}

fn failed_semantic_response(
    generation: u64,
    prewarm: bool,
    error: String,
) -> SemanticSearchResponse {
    SemanticSearchResponse {
        generation,
        filtered: Vec::new(),
        metadata: HashMap::new(),
        error: Some(error),
        progress: SemanticProgress::Failed,
        prewarm,
    }
}

fn semantic_progress(progress: SemanticIndexProgress) -> SemanticProgress {
    match progress {
        SemanticIndexProgress::Embedding { completed, total } => {
            SemanticProgress::Embedding { completed, total }
        }
        SemanticIndexProgress::CacheReady => SemanticProgress::CacheReady,
        SemanticIndexProgress::Ranking => SemanticProgress::Ranking,
        SemanticIndexProgress::Complete => SemanticProgress::Complete,
        SemanticIndexProgress::EmptyCorpus => SemanticProgress::EmptyCorpus,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::Conversation;
    use crate::semantic::test_fixtures::{SemanticConversationFixture, beta_hit_metadata};
    use chrono::{Duration as ChronoDuration, Local};
    use std::time::Duration;

    const WORKER_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

    fn conversation(path: &str, semantic_turns: Vec<&str>) -> Conversation {
        SemanticConversationFixture::new(path, semantic_turns)
            .with_normalized_search_text_lower()
            .build()
    }

    fn corpus(conversations: Vec<Conversation>) -> Arc<Vec<Arc<Conversation>>> {
        Arc::new(conversations.into_iter().map(Arc::new).collect())
    }

    fn send_worker_setup(
        tx: &mpsc::Sender<SemanticWorkerCommand>,
        semantic_turns: Vec<&str>,
        query: &str,
        prewarm: bool,
    ) {
        tx.send(SemanticWorkerCommand::UpdateCorpus {
            corpus_version: 1,
            conversations: corpus(vec![conversation(
                "/projects/project-a/session-a.jsonl",
                semantic_turns,
            )]),
        })
        .expect("send corpus");
        tx.send(SemanticWorkerCommand::UpdateScope {
            corpus_version: 1,
            scope_version: 1,
            indices: Arc::new(vec![0]),
        })
        .expect("send scope");
        tx.send(SemanticWorkerCommand::Search {
            generation: 1,
            query: ParsedQuery::parse(query),
            corpus_version: 1,
            scope_version: 1,
            prewarm,
        })
        .expect("send semantic request");
    }

    fn recv_empty_complete(rx: &mpsc::Receiver<SemanticSearchMessage>) -> SemanticSearchResponse {
        let message = rx
            .recv_timeout(WORKER_RESPONSE_TIMEOUT)
            .expect("empty response");
        match message {
            SemanticSearchMessage::Complete(response) => {
                assert!(response.filtered.is_empty());
                assert!(response.metadata.is_empty());
                assert_eq!(response.error, None);
                response
            }
            SemanticSearchMessage::Progress { progress, .. } => {
                panic!("unexpected progress before empty response: {progress:?}");
            }
        }
    }

    #[test]
    fn maps_domain_hits_to_original_indices_and_metadata() {
        let (expected_score_breakdown, expected_explanation) = beta_hit_metadata(42, "session-b");
        let response = semantic_search_response(
            7,
            SemanticIndexResponse {
                hits: vec![crate::semantic::types::SemanticHit::new(
                    expected_score_breakdown.clone(),
                    expected_explanation.clone(),
                )],
                chunk_hits: Vec::new(),
                indexed_chunk_count: 1,
                missing_chunk_count: 0,
                query_embedding_returned: true,
                progress: SemanticIndexProgress::Complete,
                prewarm: false,
            },
            &[],
            &ParsedQuery::parse("beta"),
        );

        assert_eq!(response.generation, 7);
        assert_eq!(response.filtered, vec![42]);
        assert_eq!(response.progress, SemanticProgress::Complete);
        let metadata = &response.metadata[&42];
        assert_eq!(metadata.score_breakdown, expected_score_breakdown);
        assert_eq!(metadata.explanation, expected_explanation);
    }

    #[test]
    fn empty_visible_dialogue_returns_before_embedder_initialization() {
        let (tx, rx, _cancellation) = spawn_semantic_worker();
        send_worker_setup(&tx, vec![], "alpha", false);
        let response = recv_empty_complete(&rx);
        assert_eq!(response.progress, SemanticProgress::EmptyCorpus);
    }

    #[test]
    fn empty_quoted_search_returns_idle_without_embedding() {
        let (tx, rx, _cancellation) = spawn_semantic_worker();
        send_worker_setup(&tx, vec!["visible dialogue"], "\"\"", false);
        let response = recv_empty_complete(&rx);
        assert_eq!(response.progress, SemanticProgress::Idle);
    }

    #[test]
    fn empty_prewarm_search_builds_cache_without_idle_short_circuit() {
        let (tx, rx, _cancellation) = spawn_semantic_worker();
        send_worker_setup(&tx, vec!["visible dialogue"], "\"\"", true);

        loop {
            match rx
                .recv_timeout(WORKER_RESPONSE_TIMEOUT)
                .expect("prewarm response")
            {
                SemanticSearchMessage::Progress { .. } => continue,
                SemanticSearchMessage::Complete(response) => {
                    assert!(response.prewarm);
                    assert!(response.filtered.is_empty());
                    assert!(response.metadata.is_empty());
                    assert_eq!(response.error, None);
                    assert_ne!(response.progress, SemanticProgress::Idle);
                    break;
                }
            }
        }
    }

    #[test]
    fn quoted_only_search_uses_literal_fallback_without_embedding() {
        let (tx, rx, _cancellation) = spawn_semantic_worker();
        send_worker_setup(&tx, vec![], "\"title sentinel\"", false);
        let message = rx
            .recv_timeout(WORKER_RESPONSE_TIMEOUT)
            .expect("literal response");

        match message {
            SemanticSearchMessage::Complete(response) => {
                assert_eq!(response.filtered, vec![0]);
                assert!(
                    response.metadata[&0]
                        .explanation
                        .evidence_preview
                        .contains("title sentinel")
                );
                assert_eq!(response.error, None);
                assert_eq!(response.progress, SemanticProgress::Complete);
            }
            SemanticSearchMessage::Progress { progress, .. } => {
                panic!("unexpected progress before literal response: {progress:?}");
            }
        }
    }

    #[test]
    fn quoted_only_search_respects_scope_and_orders_newest_first() {
        let mut old = conversation("/projects/project-a/session-a.jsonl", vec![]);
        old.timestamp = Local::now() - ChronoDuration::days(1);
        let mut newest = conversation("/projects/project-a/session-b.jsonl", vec![]);
        newest.timestamp = Local::now();
        let mut hidden = conversation("/projects/project-a/session-c.jsonl", vec![]);
        hidden.timestamp = Local::now() + ChronoDuration::days(1);
        let (tx, rx, _cancellation) = spawn_semantic_worker();
        tx.send(SemanticWorkerCommand::UpdateCorpus {
            corpus_version: 1,
            conversations: corpus(vec![old, newest, hidden]),
        })
        .expect("send corpus");
        tx.send(SemanticWorkerCommand::UpdateScope {
            corpus_version: 1,
            scope_version: 1,
            indices: Arc::new(vec![0, 1]),
        })
        .expect("send scope");
        tx.send(SemanticWorkerCommand::Search {
            generation: 1,
            query: ParsedQuery::parse("\"title sentinel\""),
            corpus_version: 1,
            scope_version: 1,
            prewarm: false,
        })
        .expect("send semantic request");

        match rx
            .recv_timeout(WORKER_RESPONSE_TIMEOUT)
            .expect("literal response")
        {
            SemanticSearchMessage::Complete(response) => {
                assert_eq!(response.filtered, vec![1, 0]);
                assert!(
                    response.metadata[&1]
                        .explanation
                        .evidence_preview
                        .contains("title sentinel")
                );
                assert!(
                    response.metadata[&0]
                        .explanation
                        .evidence_preview
                        .contains("title sentinel")
                );
            }
            SemanticSearchMessage::Progress { progress, .. } => {
                panic!("unexpected progress before literal response: {progress:?}");
            }
        }
    }

    #[test]
    fn worker_builds_distinct_full_corpus_and_scoped_candidates() {
        let mut worker = SemanticWorkerState::default();
        let cancellation = SemanticCancellationToken::new();
        worker.apply_command(
            SemanticWorkerCommand::UpdateCorpus {
                corpus_version: 1,
                conversations: corpus(vec![
                    conversation("/projects/project-a/session-a.jsonl", vec!["hidden"]),
                    conversation("/projects/project-a/session-b.jsonl", vec!["visible"]),
                ]),
            },
            &cancellation,
        );
        worker.apply_command(
            SemanticWorkerCommand::UpdateScope {
                corpus_version: 1,
                scope_version: 1,
                indices: Arc::new(vec![1]),
            },
            &cancellation,
        );

        let full_corpus = worker.full_corpus_candidates();
        let candidates = worker.semantic_candidates();

        assert_eq!(full_corpus.len(), 2);
        assert_eq!(full_corpus[0].index, 0);
        assert_eq!(full_corpus[1].index, 1);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].index, 1);
        assert_eq!(candidates[0].conversation.semantic_turns, vec!["visible"]);
    }

    #[test]
    fn stale_search_is_not_current_after_scope_advances() {
        let mut worker = SemanticWorkerState::default();
        let cancellation = SemanticCancellationToken::new();
        worker.apply_command(
            SemanticWorkerCommand::UpdateCorpus {
                corpus_version: 1,
                conversations: corpus(vec![conversation(
                    "/projects/project-a/session-a.jsonl",
                    vec!["visible"],
                )]),
            },
            &cancellation,
        );
        worker.apply_command(
            SemanticWorkerCommand::UpdateScope {
                corpus_version: 1,
                scope_version: 2,
                indices: Arc::new(vec![0]),
            },
            &cancellation,
        );
        let request = SemanticSearchRequest {
            generation: 1,
            query: ParsedQuery::parse("visible"),
            corpus_version: 1,
            scope_version: 1,
            prewarm: false,
        };

        assert!(matches!(
            worker.version_state_for(&request),
            Some(VersionState::Stale)
        ));
    }
}
