use super::{App, ListSearchMode, SemanticProgress, SemanticResultMetadata};
use crate::history::{Conversation, format_short_name_from_path};
use crate::search::query::ParsedQuery;
use crate::search::{self, SearchableConversation};
use crate::semantic::types::SemanticCancellationToken;
use crate::store;
use crate::tui::semantic_worker::{
    SemanticIndexing, SemanticSearchMessage, SemanticWorkerCommand, spawn_semantic_worker,
};
use chrono::Local;
use rayon::prelude::*;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::mpsc;

#[allow(dead_code)]
#[derive(Default)]
pub(super) struct SemanticSearchState {
    pub(super) available: bool,
    pub(super) pending_generation: Option<u64>,
    pub(super) pending_status: Option<SemanticProgress>,
    pub(super) prewarm_generation: Option<u64>,
    pub(super) prewarm_status: Option<SemanticProgress>,
    /// Progress of embedding the conversations the worker has no
    /// embeddings for, while it does.
    pub(super) indexing: Option<SemanticIndexing>,
    /// Generation of the newest search sent to the worker until the worker
    /// reports it has nothing left to do for it. Its messages arrive on
    /// their own channel, so they are polled for meanwhile.
    pub(super) busy_generation: Option<u64>,
    pub(super) last_status: SemanticProgress,
    pub(super) error: Option<String>,
    pub(super) results: HashMap<usize, SemanticResultMetadata>,
    pub(super) worker_tx: Option<mpsc::Sender<SemanticWorkerCommand>>,
    pub(super) worker_rx: Option<mpsc::Receiver<SemanticSearchMessage>>,
    pub(super) cancellation: Option<SemanticCancellationToken>,
}

/// Conversations with semantic data that semantic search ranks in
/// generation mode, where rows are cards without it.
#[derive(Default)]
pub(super) struct SemanticCorpus {
    /// Cards of the newest generation read so far.
    pub(super) loaded: Option<Arc<store::SemanticCards>>,
    /// Generation being read, while one is.
    loading: Option<(String, mpsc::Receiver<Option<store::SemanticCards>>)>,
    /// Generation doc of each loaded conversation, for the generation named.
    docs: Option<(String, Vec<Option<u32>>)>,
}

pub(super) enum SearchCommand {
    /// Search these loaded conversations when there is no index.
    UpdateData {
        conversations: Arc<Vec<Conversation>>,
    },
    /// Search this generation index instead of the loaded conversations, or
    /// stop using one.
    UseIndex(Option<IndexScope>),
    Search {
        query: String,
        generation: u64,
        mode: ListSearchMode,
        /// Search only the text the person typed (`Conversation::own_text`).
        user_only: bool,
    },
}

/// A generation index and the documents a query may return.
#[derive(Clone)]
pub(super) struct IndexScope {
    pub(super) index: Arc<store::SearchIndex>,
    /// Whether each document is in scope, by doc id.
    pub(super) docs: Arc<Vec<bool>>,
}

pub(super) struct SearchResponse {
    /// Conversation indices in rank order, or doc ids of `index` when it is
    /// set.
    pub(super) filtered: Vec<usize>,
    /// The index the doc ids belong to.
    pub(super) index: Option<Arc<store::SearchIndex>>,
    /// Whether `filtered` holds every result rather than the first page.
    pub(super) complete: bool,
    pub(super) generation: u64,
    pub(super) mode: ListSearchMode,
    pub(super) user_only: bool,
    /// Evidence keyed like `filtered`.
    pub(super) evidence: HashMap<usize, search::LexicalEvidence>,
}

/// Results ranked before the first response of an index search: more than
/// a screen holds.
const TOP_PAGE: usize = 50;
/// Results ranked between checks for a newer command while completing.
const COMPLETE_STEP: usize = 256;
const MAX_EVIDENCE_ROWS: usize = 200;

/// With `user_only`, evidence comes from the text the person typed, shown
/// even where the preview already holds the match, since the preview mixes
/// in other messages.
fn build_search_evidence(
    conversations: &[Conversation],
    filtered: &[usize],
    parsed: &ParsedQuery,
    user_only: bool,
) -> HashMap<usize, search::LexicalEvidence> {
    filtered
        .iter()
        .take(MAX_EVIDENCE_ROWS)
        .filter_map(|&index| {
            let conversation = &conversations[index];
            if user_only {
                search::build_lexical_evidence(&conversation.own_text, "", parsed)
            } else {
                search::build_lexical_evidence(
                    &conversation.full_text,
                    &conversation.preview,
                    parsed,
                )
            }
            .map(|evidence| (index, evidence))
        })
        .collect()
}

/// Evidence for card `doc` of `index`, as `build_search_evidence` builds it.
pub(super) fn index_evidence(
    index: &store::SearchIndex,
    doc: u32,
    parsed: &ParsedQuery,
    user_only: bool,
) -> Option<search::LexicalEvidence> {
    if user_only {
        search::build_lexical_evidence(&index.own_text(doc), "", parsed)
    } else {
        search::build_lexical_evidence(&index.text(doc), index.cards().preview(doc), parsed)
    }
}

/// Evidence for `docs` that `evidence` does not hold yet, read from their
/// text frames in parallel.
fn add_index_evidence(
    index: &store::SearchIndex,
    docs: &[u32],
    parsed: &ParsedQuery,
    user_only: bool,
    evidence: &mut HashMap<usize, search::LexicalEvidence>,
) {
    let missing: Vec<u32> = docs
        .iter()
        .copied()
        .filter(|&doc| !evidence.contains_key(&(doc as usize)))
        .collect();
    let found: Vec<(usize, search::LexicalEvidence)> = missing
        .par_iter()
        .filter_map(|&doc| {
            index_evidence(index, doc, parsed, user_only).map(|evidence| (doc as usize, evidence))
        })
        .collect();
    evidence.extend(found);
}

struct SearchWorker {
    commands: mpsc::Receiver<SearchCommand>,
    responses: mpsc::Sender<SearchResponse>,
    /// Commands received while a search ran, handled before new ones.
    queued: VecDeque<SearchCommand>,
    conversations: Arc<Vec<Conversation>>,
    searchable: Arc<Vec<SearchableConversation>>,
    /// `search::user_only_view` of `conversations` and its search data,
    /// built on the first user-only search.
    own: Option<(Vec<Conversation>, Vec<SearchableConversation>)>,
    index: Option<IndexScope>,
    /// Ranks loaded conversations one query at a time, leaving the other
    /// cores to the UI and loader.
    memory_pool: rayon::ThreadPool,
    /// Ranks index queries and prepares search data.
    index_pool: rayon::ThreadPool,
}

impl SearchWorker {
    fn next_command(&mut self) -> Option<SearchCommand> {
        self.queued
            .pop_front()
            .or_else(|| self.commands.recv().ok())
    }

    fn run(mut self) {
        while let Some(command) = self.next_command() {
            match command {
                SearchCommand::Search {
                    mut query,
                    mut generation,
                    mut mode,
                    mut user_only,
                } => {
                    // Only the newest query matters; apply everything sent
                    // before it.
                    let pending: Vec<SearchCommand> = self
                        .queued
                        .drain(..)
                        .chain(self.commands.try_iter())
                        .collect();
                    for pending in pending {
                        match pending {
                            SearchCommand::Search {
                                query: q,
                                generation: g,
                                mode: m,
                                user_only: u,
                            } => {
                                query = q;
                                generation = g;
                                mode = m;
                                user_only = u;
                            }
                            other => self.apply(other),
                        }
                    }
                    match self.index.clone() {
                        Some(scope) => {
                            self.search_index(&scope, &query, generation, mode, user_only)
                        }
                        None => self.search_memory(&query, generation, mode, user_only),
                    }
                }
                other => self.apply(other),
            }
        }
    }

    fn apply(&mut self, command: SearchCommand) {
        match command {
            SearchCommand::UpdateData { conversations } => {
                // Text is normalized once per conversation text: an `Arc`
                // still held by the previous corpus has the same contents.
                let normalized: HashMap<usize, Arc<str>> = self
                    .conversations
                    .iter()
                    .zip(self.searchable.iter())
                    .map(|(conversation, searchable)| {
                        (
                            conversation.full_text.as_ptr() as usize,
                            searchable.body_lower.clone(),
                        )
                    })
                    .collect();
                self.searchable = Arc::new(self.index_pool.install(|| {
                    search::precompute_search_text_reusing(&conversations, |conversation| {
                        normalized
                            .get(&(conversation.full_text.as_ptr() as usize))
                            .cloned()
                    })
                }));
                self.conversations = conversations;
                self.own = None;
            }
            SearchCommand::UseIndex(index) => self.index = index,
            SearchCommand::Search { .. } => unreachable!("searches are not applied"),
        }
    }

    fn search_memory(
        &mut self,
        query: &str,
        generation: u64,
        mode: ListSearchMode,
        user_only: bool,
    ) {
        if user_only && self.own.is_none() {
            let conversations = &self.conversations;
            self.own = Some(self.index_pool.install(|| {
                let view = search::user_only_view(conversations);
                let searchable = search::precompute_search_text(&view);
                (view, searchable)
            }));
        }
        let (searched, searchable) = match &self.own {
            Some((view, searchable)) if user_only => (view.as_slice(), searchable.as_slice()),
            _ => (self.conversations.as_slice(), self.searchable.as_slice()),
        };
        let conversations = &self.conversations;
        let (filtered, evidence) = self.memory_pool.install(|| {
            let now = chrono::Local::now();
            let filtered = search::search(searched, searchable, query, now);
            let parsed = ParsedQuery::parse(query);
            let evidence = build_search_evidence(conversations, &filtered, &parsed, user_only);
            (filtered, evidence)
        });
        let _ = self.responses.send(SearchResponse {
            filtered,
            index: None,
            complete: true,
            generation,
            mode,
            user_only,
            evidence,
        });
    }

    /// Sends the first page as soon as it is final, then every result,
    /// unless a newer search arrives first.
    fn search_index(
        &mut self,
        scope: &IndexScope,
        query: &str,
        generation: u64,
        mode: ListSearchMode,
        user_only: bool,
    ) {
        let index = &scope.index;
        let in_scope = |doc: u32| scope.docs[doc as usize];
        let parsed = ParsedQuery::parse(query);
        let send = |docs: &[u32], complete, evidence: &HashMap<_, _>| {
            let _ = self.responses.send(SearchResponse {
                filtered: docs.iter().map(|&doc| doc as usize).collect(),
                index: Some(index.clone()),
                complete,
                generation,
                mode,
                user_only,
                evidence: HashMap::clone(evidence),
            });
        };
        let pool = &self.index_pool;
        let mut evidence = HashMap::new();
        let mut ranking =
            pool.install(|| index.search(query, chrono::Local::now(), &in_scope, user_only));

        let top: Vec<u32> =
            pool.install(|| ranking.top(TOP_PAGE).iter().map(|hit| hit.doc).collect());
        pool.install(|| add_index_evidence(index, &top, &parsed, user_only, &mut evidence));
        if top.len() < TOP_PAGE {
            send(&top, true, &evidence);
            return;
        }
        send(&top, false, &evidence);

        let mut ranked = TOP_PAGE;
        loop {
            for command in self.commands.try_iter() {
                let newer = matches!(command, SearchCommand::Search { .. });
                self.queued.push_back(command);
                if newer {
                    return;
                }
            }
            ranked += COMPLETE_STEP;
            if pool.install(|| ranking.top(ranked).len()) < ranked {
                break;
            }
        }
        let docs: Vec<u32> =
            pool.install(|| ranking.complete().iter().map(|hit| hit.doc).collect());
        let shown = &docs[..docs.len().min(MAX_EVIDENCE_ROWS)];
        pool.install(|| add_index_evidence(index, shown, &parsed, user_only, &mut evidence));
        send(&docs, true, &evidence);
    }
}

pub(super) fn spawn_search_worker() -> (mpsc::Sender<SearchCommand>, mpsc::Receiver<SearchResponse>)
{
    let (cmd_tx, cmd_rx) = mpsc::channel::<SearchCommand>();
    let (res_tx, res_rx) = mpsc::channel::<SearchResponse>();

    std::thread::Builder::new()
        .name("search-worker".into())
        .spawn(move || {
            let memory_pool = rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .thread_name(|_| "lexical-rank-worker".to_string())
                .build()
                .expect("failed to create lexical ranking thread pool");
            let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
            let index_pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .thread_name(|i| format!("index-search-{i}"))
                .build()
                .expect("failed to create index search thread pool");
            SearchWorker {
                commands: cmd_rx,
                responses: res_tx,
                queued: VecDeque::new(),
                conversations: Arc::default(),
                searchable: Arc::default(),
                own: None,
                index: None,
                memory_pool,
                index_pool,
            }
            .run();
        })
        .expect("failed to spawn search worker thread");

    (cmd_tx, res_rx)
}

impl App {
    pub(super) fn invalidate_search_generation(&mut self) {
        self.search_generation += 1;
        self.search_in_flight = false;
        self.search_page_shown = false;
        self.lexical_evidence.clear();
        self.semantic_search.pending_generation = None;
        self.semantic_search.pending_status = None;
        self.semantic_search.prewarm_generation = None;
        self.semantic_search.prewarm_status = None;
        self.semantic_search.last_status = SemanticProgress::Idle;
        self.semantic_search.error = None;
        self.semantic_search.results.clear();
    }

    fn ensure_semantic_worker(&mut self) {
        if self.semantic_search.worker_tx.is_none() || self.semantic_search.worker_rx.is_none() {
            let (tx, rx, cancellation) = spawn_semantic_worker();
            self.semantic_search.worker_tx = Some(tx);
            self.semantic_search.worker_rx = Some(rx);
            self.semantic_search.cancellation = Some(cancellation);
            self.semantic_sent_corpus_version = 0;
            self.semantic_sent_scope_signature = None;
        }
    }

    pub(super) fn rebuild_semantic_conversations_snapshot(&mut self) {
        self.semantic_conversations_snapshot = if self.snapshot.is_some() {
            match &self.semantic_corpus.loaded {
                Some(cards) => cards.conversations().clone(),
                None => Arc::default(),
            }
        } else {
            Arc::new(self.conversations.iter().cloned().map(Arc::new).collect())
        };
        self.semantic_corpus_version += 1;
        self.semantic_sent_scope_signature = None;
    }

    pub(super) fn semantic_scope_indices(&self) -> Arc<Vec<usize>> {
        if let Some(snapshot) = &self.snapshot {
            let Some(cards) = &self.semantic_corpus.loaded else {
                return Arc::default();
            };
            let conversations = cards.conversations();
            let removed = |conversation: &Conversation| {
                self.removed.contains(&store::generation::key_hash(
                    &conversation.path.to_string_lossy(),
                ))
            };
            let indices = super::list_state::filter_conversation_indices(
                0..conversations.len(),
                conversations.as_slice(),
                &self.excluded_projects,
                &self.excluded_paths,
                self.all_kinds,
                self.scope_workspace(),
                self.pinned_only,
                &self.pinned,
            );
            return Arc::new(
                indices
                    .into_iter()
                    .filter(|&index| {
                        let conversation = &conversations[index];
                        snapshot.in_time(conversation.timestamp) && !removed(conversation)
                    })
                    .collect(),
            );
        }
        Arc::new(self.filter_indices(0..self.conversations.len()))
    }

    /// Starts reading the semantic data of the generation shown when the
    /// corpus is of an older one or none. Returns whether a corpus is
    /// loaded; an older generation's serves until the new one is read.
    pub(super) fn ensure_semantic_corpus(&mut self) -> bool {
        let Some(snapshot) = &self.snapshot else {
            return true;
        };
        let corpus = &mut self.semantic_corpus;
        let name = snapshot.generation_name();
        let current = corpus
            .loaded
            .as_ref()
            .is_some_and(|cards| cards.generation_name() == name)
            || corpus
                .loading
                .as_ref()
                .is_some_and(|(loading, _)| loading == name);
        if !current {
            let (tx, rx) = mpsc::channel();
            let cards = snapshot.clone();
            let previous = corpus.loaded.clone();
            let spawned = std::thread::Builder::new()
                .name("semantic-corpus".into())
                .spawn(move || {
                    let _ = tx.send(store::SemanticCards::load(&cards, previous.as_deref()));
                });
            if spawned.is_ok() {
                corpus.loading = Some((name.to_owned(), rx));
                if corpus.loaded.is_none() {
                    self.semantic_search.pending_status = Some(SemanticProgress::LoadingSessions);
                }
            } else {
                self.semantic_search.error = Some("semantic corpus unavailable".to_string());
            }
        }
        self.semantic_corpus.loaded.is_some()
    }

    /// Takes the semantic corpus being read once it is. Returns whether it
    /// arrived.
    fn receive_semantic_corpus(&mut self) -> bool {
        let Some((_, rx)) = &self.semantic_corpus.loading else {
            return false;
        };
        let cards = match rx.try_recv() {
            Ok(cards) => cards,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        self.semantic_corpus.loading = None;
        match cards {
            Some(cards) => {
                self.semantic_corpus.loaded = Some(Arc::new(cards));
                self.semantic_corpus.docs = None;
            }
            None if self.semantic_corpus.loaded.is_none() => {
                self.semantic_search.error = Some("semantic corpus unavailable".to_string());
            }
            None => return false,
        }
        self.readiness.semantic = true;
        if matches!(
            self.semantic_search.pending_status,
            Some(SemanticProgress::LoadingSessions)
        ) {
            self.semantic_search.pending_status = None;
        }
        self.rebuild_semantic_conversations_snapshot();
        if self.list_search_mode == ListSearchMode::Semantic {
            if self.query.trim().is_empty() {
                self.prewarm_semantic_cache();
            } else {
                self.dispatch_semantic_search(self.query.trim().to_string(), false);
            }
        }
        true
    }

    /// Rows of semantic corpus conversations `indices`, in order, with
    /// `metadata` re-keyed by row. In generation mode conversations map to
    /// cards by path; others are dropped.
    pub(super) fn semantic_rows(
        &mut self,
        indices: Vec<usize>,
        metadata: HashMap<usize, SemanticResultMetadata>,
    ) -> (Vec<usize>, HashMap<usize, SemanticResultMetadata>) {
        let Some(snapshot) = self.snapshot.clone() else {
            return (self.filter_indices(indices), metadata);
        };
        let SemanticCorpus {
            loaded: Some(cards),
            docs,
            ..
        } = &mut self.semantic_corpus
        else {
            return (Vec::new(), HashMap::new());
        };
        if docs
            .as_ref()
            .is_none_or(|(name, _)| name != snapshot.generation_name())
        {
            let mapped = if cards.generation_name() == snapshot.generation_name() {
                (0..cards.conversations().len() as u32).map(Some).collect()
            } else {
                let by_key = snapshot.docs_by_key();
                cards
                    .conversations()
                    .iter()
                    .map(|conversation| {
                        let key = store::generation::key_hash(&conversation.path.to_string_lossy());
                        by_key.get(&key).copied()
                    })
                    .collect()
            };
            *docs = Some((snapshot.generation_name().to_owned(), mapped));
        }
        let doc_of: Vec<Option<u32>> = docs.as_ref().map(|(_, d)| d.clone()).unwrap_or_default();
        let mut rows = Vec::with_capacity(indices.len());
        let mut by_row = HashMap::new();
        let mut metadata = metadata;
        for index in indices {
            let Some(doc) = doc_of.get(index).copied().flatten() else {
                continue;
            };
            if self.is_removed(doc) {
                continue;
            }
            let row = self.card_row(doc);
            rows.push(row);
            if let Some(meta) = metadata.remove(&index) {
                by_row.insert(row, meta);
            }
        }
        (rows, by_row)
    }

    fn reset_semantic_worker(&mut self) {
        let (tx, rx, cancellation) = spawn_semantic_worker();
        self.semantic_search.worker_tx = Some(tx);
        self.semantic_search.worker_rx = Some(rx);
        self.semantic_search.cancellation = Some(cancellation);
        self.semantic_sent_corpus_version = 0;
        self.semantic_sent_scope_signature = None;
    }

    fn send_semantic_command(&mut self, command: SemanticWorkerCommand) -> bool {
        self.ensure_semantic_worker();
        self.semantic_search
            .worker_tx
            .as_ref()
            .is_some_and(|tx| tx.send(command).is_ok())
    }

    fn send_semantic_state(&mut self) -> Option<(u64, u64)> {
        if self.semantic_sent_corpus_version != self.semantic_corpus_version {
            let texts = self.semantic_corpus.loaded.as_ref().and_then(|cards| {
                self.search_index
                    .clone()
                    .filter(|index| index.cards().generation_name() == cards.generation_name())
            });
            if !self.send_semantic_command(SemanticWorkerCommand::UpdateCorpus {
                corpus_version: self.semantic_corpus_version,
                conversations: self.semantic_conversations_snapshot.clone(),
                texts,
            }) {
                return None;
            }
            self.semantic_sent_corpus_version = self.semantic_corpus_version;
            self.semantic_sent_scope_signature = None;
        } else if self
            .semantic_sent_scope_signature
            .as_ref()
            .is_some_and(|(corpus_version, _)| *corpus_version == self.semantic_corpus_version)
        {
            return Some((self.semantic_corpus_version, self.semantic_scope_version));
        }

        let scope = self.semantic_scope_indices();
        let current_signature = (self.semantic_corpus_version, scope.clone());
        let scope_changed =
            self.semantic_sent_scope_signature
                .as_ref()
                .is_none_or(|(corpus_version, previous)| {
                    *corpus_version != self.semantic_corpus_version
                        || previous.as_ref() != scope.as_ref()
                });
        if scope_changed {
            self.semantic_scope_version += 1;
            if !self.send_semantic_command(SemanticWorkerCommand::UpdateScope {
                corpus_version: self.semantic_corpus_version,
                scope_version: self.semantic_scope_version,
                indices: scope,
            }) {
                return None;
            }
            self.semantic_sent_scope_signature = Some(current_signature);
        }

        Some((self.semantic_corpus_version, self.semantic_scope_version))
    }

    pub(super) fn dispatch_search(&mut self) {
        let query = self.query.trim().to_string();

        if ParsedQuery::parse(&query).is_effectively_empty() {
            let prewarm_generation = self.semantic_search.prewarm_generation;
            let prewarm_status = self.semantic_search.prewarm_status.clone();
            self.invalidate_search_generation();
            if self.list_search_mode == ListSearchMode::Semantic {
                self.semantic_search.prewarm_generation = prewarm_generation;
                self.semantic_search.prewarm_status = prewarm_status;
            }
            self.semantic_search.error = None;
            self.semantic_search.results.clear();
            self.update_filter();
            return;
        }

        if search::is_uuid(&query) {
            self.invalidate_search_generation();
            self.semantic_search.error = None;
            self.apply_uuid_filter(&query);
            return;
        }

        if self.list_search_mode == ListSearchMode::Semantic {
            self.dispatch_semantic_search(query, false);
            return;
        }

        if !self.readiness.lexical {
            // The query runs once the generation's index opens.
            self.invalidate_search_generation();
            self.apply_browse_filter();
            return;
        }

        self.semantic_search.results.clear();
        self.search_generation += 1;
        self.search_in_flight = true;
        self.search_page_shown = false;
        self.semantic_search.error = None;
        let _ = self.search_tx.send(SearchCommand::Search {
            query,
            generation: self.search_generation,
            mode: self.list_search_mode,
            user_only: self.searches_user_only(),
        });
    }

    fn dispatch_semantic_search(&mut self, query: String, prewarm: bool) {
        if !self.ensure_semantic_corpus() {
            // Dispatched again once the corpus arrives.
            self.invalidate_search_generation();
            self.semantic_search.pending_status = Some(SemanticProgress::LoadingSessions);
            return;
        }
        self.search_generation += 1;
        self.search_page_shown = false;
        self.search_in_flight = !prewarm;
        self.semantic_search.pending_generation = Some(self.search_generation);
        // Embedding activity remains visible while a replacement query waits for the worker.
        if !matches!(
            self.semantic_search.pending_status,
            Some(SemanticProgress::InitializingModel | SemanticProgress::Embedding { .. })
        ) {
            self.semantic_search.pending_status = None;
        }
        if prewarm {
            self.semantic_search.prewarm_generation = Some(self.search_generation);
            self.semantic_search.prewarm_status = None;
        }
        self.semantic_search.last_status = SemanticProgress::Idle;
        self.semantic_search.error = None;
        if prewarm {
            self.semantic_search.results.clear();
        }
        if let Some(cancellation) = &self.semantic_search.cancellation {
            cancellation.cancel();
        }
        let (corpus_version, scope_version) = match self.send_semantic_state() {
            Some(versions) => versions,
            None => {
                self.reset_semantic_worker();
                let Some(versions) = self.send_semantic_state() else {
                    self.semantic_search.error = Some("semantic worker unavailable".to_string());
                    self.semantic_search.pending_status = Some(SemanticProgress::Failed);
                    return;
                };
                versions
            }
        };
        let generation = self.search_generation;
        if !prewarm {
            let _ = self.search_tx.send(SearchCommand::Search {
                query: query.clone(),
                generation,
                mode: ListSearchMode::Semantic,
                user_only: false,
            });
        }
        self.semantic_search.busy_generation = Some(generation);
        if !self.send_semantic_command(SemanticWorkerCommand::Search {
            generation,
            query: ParsedQuery::parse(&query),
            corpus_version,
            scope_version,
            prewarm,
        }) {
            self.reset_semantic_worker();
            let Some((corpus_version, scope_version)) = self.send_semantic_state() else {
                self.semantic_search.error = Some("semantic worker unavailable".to_string());
                self.semantic_search.pending_status = Some(SemanticProgress::Failed);
                self.semantic_search.busy_generation = None;
                return;
            };
            if !self.send_semantic_command(SemanticWorkerCommand::Search {
                generation,
                query: ParsedQuery::parse(&query),
                corpus_version,
                scope_version,
                prewarm,
            }) {
                self.semantic_search.error = Some("semantic worker unavailable".to_string());
                self.semantic_search.pending_status = Some(SemanticProgress::Failed);
                self.semantic_search.busy_generation = None;
            }
        }
    }

    pub(super) fn prewarm_semantic_cache(&mut self) {
        if self.list_search_mode == ListSearchMode::Semantic
            && self.query.trim().is_empty()
            && self.readiness.semantic
        {
            self.dispatch_semantic_search(String::new(), true);
        }
    }

    pub fn receive_search_results(&mut self) -> bool {
        let mut applied = self.receive_semantic_corpus();
        while let Ok(response) = self.search_rx.try_recv() {
            let semantic_fallback_pending = response.mode == ListSearchMode::Semantic
                && self.semantic_search.pending_generation == Some(response.generation);
            let index_current = match (&response.index, &self.search_index) {
                (None, _) => true,
                (Some(answered), Some(current)) => Arc::ptr_eq(answered, current),
                (Some(_), None) => false,
            };
            if response.generation == self.search_generation
                && response.mode == self.list_search_mode
                && response.user_only == self.searches_user_only()
                && (response.mode == ListSearchMode::Lexical || semantic_fallback_pending)
                && index_current
            {
                let (filtered, evidence) = if response.index.is_some() {
                    let filtered = self.rows_of_docs(&response.filtered);
                    let evidence = response
                        .evidence
                        .into_iter()
                        .filter_map(|(doc, evidence)| {
                            let row = *self.doc_rows.get(doc)?;
                            Some((row?, evidence))
                        })
                        .collect();
                    (filtered, evidence)
                } else {
                    (self.filter_indices(response.filtered), response.evidence)
                };
                if response.mode == ListSearchMode::Semantic && filtered.is_empty() {
                    self.search_in_flight = !response.complete;
                    continue;
                }
                self.lexical_evidence = evidence;
                if response.mode == ListSearchMode::Semantic {
                    self.semantic_search.results.clear();
                }
                if self.search_in_flight
                    && !self.filtered.is_empty()
                    && self.selection_anchor.is_none()
                    && self.search_page_shown
                {
                    // The complete ranking extends the page already shown;
                    // keep what the user selected on it.
                    self.selection_anchor = self.get_selected_path();
                }
                self.apply_filtered(filtered);
                self.search_page_shown = !response.complete;
                self.search_in_flight = !response.complete;
                applied = true;
            }
        }
        if let Some(rx) = self.semantic_search.worker_rx.take() {
            let active_generation = self.search_generation;
            while let Ok(message) = rx.try_recv() {
                match message {
                    SemanticSearchMessage::Progress {
                        generation,
                        progress,
                    } => {
                        if self.list_search_mode == ListSearchMode::Semantic {
                            if Some(generation) == self.semantic_search.prewarm_generation {
                                self.semantic_search.prewarm_status = Some(progress.clone());
                                self.semantic_search.pending_status = Some(progress);
                                applied = true;
                            } else if generation == active_generation {
                                self.semantic_search.prewarm_generation = None;
                                self.semantic_search.prewarm_status = None;
                                self.semantic_search.pending_status = Some(progress);
                                applied = true;
                            }
                        }
                    }
                    SemanticSearchMessage::Idle { generation } => {
                        self.semantic_search.indexing = None;
                        if self
                            .semantic_search
                            .busy_generation
                            .is_some_and(|busy| busy <= generation)
                        {
                            self.semantic_search.busy_generation = None;
                        }
                        applied = true;
                    }
                    SemanticSearchMessage::Indexing(indexing) => {
                        // A search waiting for indexing shows its progress.
                        if let Some(SemanticProgress::Embedding { .. }) =
                            &self.semantic_search.pending_status
                        {
                            self.semantic_search.pending_status =
                                Some(SemanticProgress::Embedding {
                                    completed: indexing.completed,
                                    total: indexing.total,
                                });
                        }
                        self.semantic_search.indexing = Some(indexing);
                        applied = true;
                    }
                    SemanticSearchMessage::Complete(response) => {
                        if self.list_search_mode == ListSearchMode::Semantic {
                            if response.prewarm
                                && Some(response.generation)
                                    == self.semantic_search.prewarm_generation
                            {
                                self.semantic_search.prewarm_generation = None;
                                self.semantic_search.prewarm_status = None;
                                if response.generation == active_generation {
                                    self.semantic_search.pending_generation = None;
                                    self.semantic_search.pending_status = None;
                                    self.semantic_search.last_status = response.progress;
                                    self.semantic_search.error = response.error;
                                    let (_, metadata) =
                                        self.semantic_rows(Vec::new(), response.metadata);
                                    self.semantic_search.results = metadata;
                                    if !self.query.trim().is_empty() {
                                        self.dispatch_semantic_search(
                                            self.query.trim().to_string(),
                                            false,
                                        );
                                    }
                                }
                                applied = true;
                            } else if response.generation == active_generation {
                                // Results refreshed after indexing replace ones
                                // already shown; keep a row the user moved to.
                                if self.semantic_search.pending_generation.is_none()
                                    && self.selection_anchor.is_none()
                                    && self.selected.is_some_and(|selected| selected > 0)
                                {
                                    self.selection_anchor = self.get_selected_path();
                                }
                                self.search_in_flight = false;
                                self.semantic_search.pending_generation = None;
                                self.semantic_search.pending_status = None;
                                self.semantic_search.last_status = response.progress;
                                self.semantic_search.error = response.error;
                                let (filtered, metadata) =
                                    self.semantic_rows(response.filtered, response.metadata);
                                self.semantic_search.results = metadata;
                                self.apply_filtered(filtered);
                                applied = true;
                            }
                        }
                    }
                }
            }
            self.semantic_search.worker_rx = Some(rx);
        }
        applied
    }

    /// Progress of the semantic worker's background indexing, while it runs.
    pub fn semantic_indexing(&self) -> Option<SemanticIndexing> {
        self.semantic_search.indexing
    }

    /// Whether the semantic worker may still send messages for the searches
    /// sent to it.
    pub fn semantic_worker_busy(&self) -> bool {
        self.semantic_search.busy_generation.is_some() || self.semantic_search.indexing.is_some()
    }

    pub fn has_search_work_in_flight(&self) -> bool {
        self.semantic_corpus.loading.is_some()
            || self.search_in_flight
            || self.semantic_search.pending_generation.is_some()
            || self.semantic_search.prewarm_generation.is_some()
    }

    #[cfg(test)]
    pub(super) fn search_generation(&self) -> u64 {
        self.search_generation
    }

    #[cfg(test)]
    pub(super) fn semantic_search_error(&self) -> Option<&str> {
        self.semantic_search.error.as_deref()
    }

    /// Replaces the whole query and dispatches a single search for it.
    pub(crate) fn replace_query(&mut self, query: &str) {
        self.query = query.to_string();
        self.cursor_pos = self.query.chars().count();
        self.clear_selection_anchor();
        self.dispatch_search();
    }

    #[cfg(test)]
    pub fn set_query_for_test(&mut self, query: &str) {
        self.query = query.to_string();
        self.cursor_pos = self.query.chars().count();
    }

    #[cfg(test)]
    pub fn set_semantic_receiver_for_test(
        &mut self,
        generation: u64,
        worker_rx: mpsc::Receiver<SemanticSearchMessage>,
    ) {
        self.search_generation = generation;
        self.semantic_search.pending_generation = Some(generation);
        self.semantic_search.prewarm_generation = None;
        self.semantic_search.prewarm_status = None;
        self.semantic_search.worker_rx = Some(worker_rx);
    }

    #[cfg(test)]
    pub fn set_semantic_prewarm_generation_for_test(&mut self, generation: u64) {
        self.semantic_search.prewarm_generation = Some(generation);
    }

    pub fn semantic_result_metadata(
        &self,
        conversation_index: usize,
    ) -> Option<&SemanticResultMetadata> {
        self.semantic_search.results.get(&conversation_index)
    }

    pub fn lexical_evidence(&self, conversation_index: usize) -> Option<&search::LexicalEvidence> {
        self.lexical_evidence.get(&conversation_index)
    }

    #[cfg(test)]
    pub fn set_lexical_evidence_for_test(
        &mut self,
        conversation_index: usize,
        evidence: search::LexicalEvidence,
    ) {
        self.lexical_evidence.insert(conversation_index, evidence);
    }

    pub fn semantic_result_metadata_for_selection(&self) -> Option<&SemanticResultMetadata> {
        if self.list_search_mode != ListSearchMode::Semantic {
            return None;
        }
        let selected = self.selected?;
        let conversation_index = *self.filtered.get(selected)?;
        self.semantic_result_metadata(conversation_index)
    }

    pub fn semantic_status_text(&self) -> Option<String> {
        if self.list_search_mode != ListSearchMode::Semantic {
            return None;
        }
        if self.semantic_search.error.is_some() {
            return Some("sem failed".to_string());
        }
        let status = self
            .semantic_search
            .pending_status
            .as_ref()
            .unwrap_or(&self.semantic_search.last_status);
        match status {
            SemanticProgress::EmptyCorpus => Some("sem no text".to_string()),
            SemanticProgress::Failed => Some("sem failed".to_string()),
            _ => None,
        }
    }

    pub fn semantic_activity_status_text(&self) -> Option<String> {
        if self.list_search_mode != ListSearchMode::Semantic || self.semantic_search.error.is_some()
        {
            return None;
        }
        let status = self
            .semantic_search
            .prewarm_status
            .as_ref()
            .or(self.semantic_search.pending_status.as_ref());
        let search = status.and_then(|status| match status {
            SemanticProgress::InitializingModel => Some("sem preparing embeddings".to_string()),
            SemanticProgress::Embedding { completed, total } => {
                let percent = if *total == 0 {
                    100
                } else {
                    completed.min(total).saturating_mul(100) / total
                };
                Some(format!(
                    "sem embedding {percent}%  {}/{total} chunks",
                    completed.min(total)
                ))
            }
            _ => None,
        });
        search.or_else(|| {
            let SemanticIndexing { completed, total } = self.semantic_search.indexing?;
            let percent = completed.min(total).saturating_mul(100) / total.max(1);
            Some(format!(
                "sem indexing {percent}%  {}/{total} chunks",
                completed.min(total)
            ))
        })
    }

    pub(super) fn toggle_workspace_filter(&mut self) {
        if self.workspace.is_some() {
            self.workspace_filter = !self.workspace_filter;
            self.semantic_sent_scope_signature = None;
            self.invalidate_search_generation();
            self.send_index_scope();
            if self.query.trim().is_empty() || !self.readiness.lexical {
                self.update_filter();
            } else {
                self.dispatch_search();
            }
        }
    }

    /// Lists or hides automated and child conversations, keeping the query
    /// and the selected session.
    pub(super) fn toggle_all_kinds(&mut self) {
        self.all_kinds = !self.all_kinds;
        if self.selection_anchor.is_none() {
            self.selection_anchor = self.get_selected_path();
        }
        self.semantic_sent_scope_signature = None;
        self.invalidate_search_generation();
        self.send_index_scope();
        if self.query.trim().is_empty() || !self.readiness.lexical {
            self.update_filter();
        } else {
            self.dispatch_search();
        }
    }

    /// Searches only the text the person typed, or everything again,
    /// keeping the query and the selected session.
    pub(super) fn toggle_user_only(&mut self) {
        self.user_only = !self.user_only;
        if self.list_search_mode == ListSearchMode::Semantic {
            // Semantic search ranks whole conversations; see
            // `searches_user_only`.
            return;
        }
        if self.selection_anchor.is_none() {
            self.selection_anchor = self.get_selected_path();
        }
        self.invalidate_search_generation();
        if self.query.trim().is_empty() || !self.readiness.lexical {
            self.update_filter();
        } else {
            self.dispatch_search();
        }
    }

    pub fn user_only(&self) -> bool {
        self.user_only
    }

    /// Shows only pinned conversations, or everything again, keeping the
    /// query and the selected session. Composes with the scope and
    /// `all_kinds` filters and the search query exactly like they compose
    /// with each other, rather than replacing them.
    pub(super) fn toggle_pinned_only(&mut self) {
        self.pinned_only = !self.pinned_only;
        if self.selection_anchor.is_none() {
            self.selection_anchor = self.get_selected_path();
        }
        self.semantic_sent_scope_signature = None;
        self.invalidate_search_generation();
        self.send_index_scope();
        if self.query.trim().is_empty() || !self.readiness.lexical {
            self.update_filter();
        } else {
            self.dispatch_search();
        }
    }

    pub fn pinned_only(&self) -> bool {
        self.pinned_only
    }

    /// Whether queries match only the text the person typed: user-only is
    /// on and the mode is lexical. Semantic search embeds chunks that mix
    /// both sides of the conversation, so it ignores the toggle.
    pub(crate) fn searches_user_only(&self) -> bool {
        self.user_only && self.list_search_mode == ListSearchMode::Lexical
    }

    /// Switches to `mode` the way toggling does, when it is not the mode.
    pub(crate) fn set_list_search_mode(&mut self, mode: ListSearchMode) {
        if self.list_search_mode != mode {
            self.toggle_list_search_mode();
        }
    }

    pub(crate) fn semantic_error(&self) -> Option<&str> {
        self.semantic_search.error.as_deref()
    }

    pub(super) fn toggle_list_search_mode(&mut self) {
        if !self.semantic_search.available {
            return;
        }
        self.list_search_mode = match self.list_search_mode {
            ListSearchMode::Lexical => ListSearchMode::Semantic,
            ListSearchMode::Semantic => ListSearchMode::Lexical,
        };
        self.invalidate_search_generation();
        if self.list_search_mode == ListSearchMode::Semantic && self.query.trim().is_empty() {
            self.ensure_semantic_corpus();
            self.prewarm_semantic_cache();
        } else {
            self.dispatch_search();
        }
    }

    pub(super) fn apply_uuid_filter(&mut self, query: &str) -> bool {
        if let Some(idx) = self.find_or_load_uuid(query) {
            self.filtered = vec![idx];
            self.selected = Some(0);
            true
        } else {
            false
        }
    }

    /// Lists every conversation in scope, newest first.
    pub(super) fn apply_browse_filter(&mut self) {
        if self.snapshot.is_some() {
            self.show_cards(usize::MAX);
            return;
        }
        let now = Local::now();
        let filtered = search::search(&self.conversations, &[], "", now);
        let filtered = self.filter_indices(filtered);
        self.apply_filtered(filtered);
    }

    pub(super) fn update_filter(&mut self) {
        let query = self.query.trim().to_string();

        if ParsedQuery::parse(&query).is_effectively_empty() || !self.readiness.lexical {
            self.semantic_search.error = None;
            self.apply_browse_filter();
            return;
        }

        if search::is_uuid(&query) {
            self.semantic_search.error = None;
            self.apply_uuid_filter(&query);
            return;
        }

        if self.list_search_mode == ListSearchMode::Semantic {
            return;
        }

        self.dispatch_search();
    }

    pub(super) fn find_or_load_uuid(&mut self, uuid: &str) -> Option<usize> {
        for (idx, conv) in self.conversations.iter().enumerate() {
            if conv.session_id.eq_ignore_ascii_case(uuid) {
                return Some(idx);
            }
        }
        if let Some(doc) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.find_session(uuid))
        {
            return Some(self.card_row(doc));
        }

        let path = crate::history::find_jsonl_by_uuid(uuid).ok()??;
        let modified = path.metadata().ok().and_then(|m| m.modified().ok());
        let mut conv = crate::history::process_conversation_file(path, modified, None).ok()??;

        let fallback_path = conv
            .path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| crate::history::path::decode_project_dir_name_to_path(&n.to_string_lossy()))
            .unwrap_or_default();
        let project_path = conv.cwd.clone().unwrap_or(fallback_path);
        conv.project_name = Some(format_short_name_from_path(&project_path));
        conv.project_path = Some(project_path);

        let idx = self.conversations.len();
        Arc::make_mut(&mut self.conversations).push(conv);
        if self.snapshot.is_some() {
            self.row_docs.push(super::NO_DOC);
        }
        self.send_search_data();

        Some(idx)
    }

    pub(super) fn refresh_search_data(&mut self) {
        self.send_search_data();
        self.invalidate_search_generation();
    }

    /// Shares the loaded corpus with the background workers. In generation
    /// mode lexical queries run on the generation index and semantic ones on
    /// the semantic corpus instead.
    fn send_search_data(&mut self) {
        if self.snapshot.is_some() {
            return;
        }
        self.rebuild_semantic_conversations_snapshot();
        let _ = self.search_tx.send(SearchCommand::UpdateData {
            conversations: self.conversations.clone(),
        });
    }
}
