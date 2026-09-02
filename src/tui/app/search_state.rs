use super::{App, ListSearchMode, SemanticProgress, SemanticResultMetadata};
use crate::history::{Conversation, format_short_name_from_path};
use crate::search::query::ParsedQuery;
use crate::search::{self, SearchableConversation};
use crate::semantic::types::SemanticCancellationToken;
use crate::tui::semantic_worker::{
    SemanticSearchMessage, SemanticWorkerCommand, spawn_semantic_worker,
};
use chrono::Local;
use std::collections::HashMap;
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
    pub(super) last_status: SemanticProgress,
    pub(super) error: Option<String>,
    pub(super) results: HashMap<usize, SemanticResultMetadata>,
    pub(super) worker_tx: Option<mpsc::Sender<SemanticWorkerCommand>>,
    pub(super) worker_rx: Option<mpsc::Receiver<SemanticSearchMessage>>,
    pub(super) cancellation: Option<SemanticCancellationToken>,
}

pub(super) enum SearchCommand {
    UpdateData {
        conversations: Arc<Vec<Conversation>>,
        searchable: Arc<Vec<SearchableConversation>>,
    },
    Search {
        query: String,
        generation: u64,
        mode: ListSearchMode,
    },
}

pub(super) struct SearchResponse {
    pub(super) filtered: Vec<usize>,
    pub(super) generation: u64,
    pub(super) mode: ListSearchMode,
    pub(super) evidence: HashMap<usize, search::LexicalEvidence>,
}

fn build_search_evidence(
    conversations: &[Conversation],
    filtered: &[usize],
    parsed: &ParsedQuery,
) -> HashMap<usize, search::LexicalEvidence> {
    const MAX_EVIDENCE_ROWS: usize = 200;

    filtered
        .iter()
        .take(MAX_EVIDENCE_ROWS)
        .filter_map(|&index| {
            search::build_lexical_evidence(&conversations[index], parsed)
                .map(|evidence| (index, evidence))
        })
        .collect()
}

pub(super) fn spawn_search_worker() -> (mpsc::Sender<SearchCommand>, mpsc::Receiver<SearchResponse>)
{
    let (cmd_tx, cmd_rx) = mpsc::channel::<SearchCommand>();
    let (res_tx, res_rx) = mpsc::channel::<SearchResponse>();

    std::thread::Builder::new()
        .name("search-worker".into())
        .spawn(move || {
            let mut conversations: Arc<Vec<Conversation>> = Arc::new(Vec::new());
            let mut searchable: Arc<Vec<SearchableConversation>> = Arc::new(Vec::new());

            while let Ok(cmd) = cmd_rx.recv() {
                match cmd {
                    SearchCommand::UpdateData {
                        conversations: c,
                        searchable: s,
                    } => {
                        conversations = c;
                        searchable = s;
                    }
                    SearchCommand::Search {
                        mut query,
                        mut generation,
                        mut mode,
                    } => {
                        while let Ok(pending) = cmd_rx.try_recv() {
                            match pending {
                                SearchCommand::UpdateData {
                                    conversations: c,
                                    searchable: s,
                                } => {
                                    conversations = c;
                                    searchable = s;
                                }
                                SearchCommand::Search {
                                    query: q,
                                    generation: g,
                                    mode: m,
                                } => {
                                    query = q;
                                    generation = g;
                                    mode = m;
                                }
                            }
                        }

                        let now = chrono::Local::now();
                        let filtered = search::search(&conversations, &searchable, &query, now);
                        let parsed = ParsedQuery::parse(&query);
                        let evidence = build_search_evidence(&conversations, &filtered, &parsed);

                        let _ = res_tx.send(SearchResponse {
                            filtered,
                            generation,
                            mode,
                            evidence,
                        });
                    }
                }
            }
        })
        .expect("failed to spawn search worker thread");

    (cmd_tx, res_rx)
}

impl App {
    pub(super) fn invalidate_search_generation(&mut self) {
        self.search_generation += 1;
        self.search_in_flight = false;
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
        self.semantic_conversations_snapshot = Arc::new(
            self.conversations
                .iter()
                .cloned()
                .map(Arc::new)
                .collect::<Vec<_>>(),
        );
        self.semantic_corpus_version += 1;
        self.semantic_sent_scope_signature = None;
    }

    pub(super) fn semantic_scope_indices(&self) -> Arc<Vec<usize>> {
        Arc::new(self.filter_indices(0..self.conversations.len()))
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
            if !self.send_semantic_command(SemanticWorkerCommand::UpdateCorpus {
                corpus_version: self.semantic_corpus_version,
                conversations: self.semantic_conversations_snapshot.clone(),
            }) {
                return None;
            }
            self.semantic_sent_corpus_version = self.semantic_corpus_version;
            self.semantic_sent_scope_signature = None;
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

        self.semantic_search.results.clear();
        self.search_generation += 1;
        self.search_in_flight = true;
        self.semantic_search.error = None;
        let _ = self.search_tx.send(SearchCommand::Search {
            query,
            generation: self.search_generation,
            mode: self.list_search_mode,
        });
    }

    fn dispatch_semantic_search(&mut self, query: String, prewarm: bool) {
        self.search_generation += 1;
        self.search_in_flight = !prewarm;
        self.semantic_search.pending_generation = Some(self.search_generation);
        self.semantic_search.pending_status = None;
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
            });
        }
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
            }
        }
    }

    pub(super) fn prewarm_semantic_cache(&mut self) {
        if self.list_search_mode == ListSearchMode::Semantic
            && self.query.trim().is_empty()
            && !self.is_loading()
        {
            self.dispatch_semantic_search(String::new(), true);
        }
    }

    pub fn receive_search_results(&mut self) -> bool {
        let mut applied = false;
        while let Ok(response) = self.search_rx.try_recv() {
            let semantic_fallback_pending = response.mode == ListSearchMode::Semantic
                && self.semantic_search.pending_generation == Some(response.generation);
            if response.generation == self.search_generation
                && response.mode == self.list_search_mode
                && (response.mode == ListSearchMode::Lexical || semantic_fallback_pending)
            {
                let filtered = self.filter_indices(response.filtered);
                self.lexical_evidence = response.evidence;
                if response.mode == ListSearchMode::Semantic {
                    self.semantic_search.results.clear();
                }
                self.apply_filtered(filtered);
                self.search_in_flight = false;
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
                                    self.semantic_search.results = response.metadata;
                                    if !self.query.trim().is_empty() {
                                        self.dispatch_semantic_search(
                                            self.query.trim().to_string(),
                                            false,
                                        );
                                    }
                                }
                                applied = true;
                            } else if response.generation == active_generation {
                                self.search_in_flight = false;
                                self.semantic_search.pending_generation = None;
                                self.semantic_search.pending_status = None;
                                self.semantic_search.last_status = response.progress;
                                self.semantic_search.error = response.error;
                                self.semantic_search.results = response.metadata;
                                let filtered = self.filter_indices(response.filtered);
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

    pub fn has_search_work_in_flight(&self) -> bool {
        self.search_in_flight
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
            .or(self.semantic_search.pending_status.as_ref())?;
        match status {
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
        }
    }

    pub(super) fn toggle_workspace_filter(&mut self) {
        if self.current_project_dir_name.is_some() {
            self.workspace_filter = !self.workspace_filter;
            self.invalidate_search_generation();
            if self.list_search_mode == ListSearchMode::Semantic && !self.query.trim().is_empty() {
                self.dispatch_search();
            } else {
                self.update_filter();
            }
        }
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

    pub(super) fn apply_lexical_filter(&mut self) {
        let now = Local::now();
        let filtered = search::search(&self.conversations, &self.searchable, &self.query, now);
        let filtered = self.filter_indices(filtered);
        self.apply_filtered(filtered);
    }

    pub(super) fn update_filter(&mut self) {
        let query = self.query.trim().to_string();

        if ParsedQuery::parse(&query).is_effectively_empty() {
            self.semantic_search.error = None;
            self.apply_lexical_filter();
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

        self.semantic_search.error = None;
        self.semantic_search.results.clear();
        self.apply_lexical_filter();
    }

    pub(super) fn find_or_load_uuid(&mut self, uuid: &str) -> Option<usize> {
        for (idx, conv) in self.conversations.iter().enumerate() {
            if conv.session_id.eq_ignore_ascii_case(uuid) {
                return Some(idx);
            }
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
        self.conversations.push(conv);

        self.searchable = search::precompute_search_text(&self.conversations);
        self.conversations_snapshot = Arc::new(self.conversations.clone());
        self.rebuild_semantic_conversations_snapshot();

        let _ = self.search_tx.send(SearchCommand::UpdateData {
            conversations: self.conversations_snapshot.clone(),
            searchable: Arc::new(self.searchable.clone()),
        });

        Some(idx)
    }

    pub(super) fn refresh_search_data(&mut self) {
        self.conversations_snapshot = Arc::new(self.conversations.clone());
        self.rebuild_semantic_conversations_snapshot();
        self.searchable = search::precompute_search_text(&self.conversations);
        let _ = self.search_tx.send(SearchCommand::UpdateData {
            conversations: self.conversations_snapshot.clone(),
            searchable: Arc::new(self.searchable.clone()),
        });
        self.invalidate_search_generation();
    }
}
