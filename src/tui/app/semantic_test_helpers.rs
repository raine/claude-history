use std::collections::HashMap;
use std::sync::mpsc;

use super::*;
use crate::semantic::types::{SemanticChunkIdentity, SemanticQuality, SemanticRationaleKind};

pub(crate) fn connect_semantic_search_channels(
    app: &mut App,
) -> (
    mpsc::Sender<crate::tui::semantic_worker::SemanticWorkerCommand>,
    mpsc::Receiver<crate::tui::semantic_worker::SemanticWorkerCommand>,
    mpsc::Sender<SemanticSearchMessage>,
) {
    let (request_tx, request_rx) =
        mpsc::channel::<crate::tui::semantic_worker::SemanticWorkerCommand>();
    let (response_tx, response_rx) = mpsc::channel::<SemanticSearchMessage>();
    app.semantic_search.worker_tx = Some(request_tx);
    app.semantic_search.worker_rx = Some(response_rx);
    (
        app.semantic_search.worker_tx.clone().unwrap(),
        request_rx,
        response_tx,
    )
}

pub(crate) fn send_semantic_complete_response(
    response_tx: &mpsc::Sender<SemanticSearchMessage>,
    generation: u64,
    filtered: Vec<usize>,
    metadata: HashMap<usize, SemanticResultMetadata>,
    progress: SemanticProgress,
) {
    response_tx
        .send(SemanticSearchMessage::Complete(
            crate::tui::semantic_worker::SemanticSearchResponse {
                generation,
                filtered,
                metadata,
                error: None,
                progress,
                prewarm: false,
            },
        ))
        .unwrap();
}

pub(crate) fn send_semantic_progress_response(
    response_tx: &mpsc::Sender<SemanticSearchMessage>,
    generation: u64,
    progress: SemanticProgress,
) {
    response_tx
        .send(SemanticSearchMessage::Progress {
            generation,
            progress,
        })
        .unwrap();
}

pub(crate) fn test_semantic_metadata(
    conversation_index: usize,
    evidence_preview: &str,
) -> SemanticResultMetadata {
    SemanticResultMetadata {
        score_breakdown: SemanticScoreBreakdown {
            hybrid: 1.0,
            semantic: 1.0,
            lexical: 0.0,
        },
        explanation: SemanticExplanation {
            quality: SemanticQuality::Strong,
            quality_label: "strong",
            matched_terms: Vec::new(),
            evidence_preview: evidence_preview.to_string(),
            rationale_kind: SemanticRationaleKind::SemanticOnly,
            chunk: SemanticChunkIdentity {
                conversation_index,
                source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
                session: "test-session".to_string(),
                chunk_index: 0,
                message_range: crate::history::MessageRange::single(1),
            },
        },
    }
}
