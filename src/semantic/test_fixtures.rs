use std::path::Path;
use std::path::PathBuf;

use crate::agent::refs::MessageRange;
use crate::history::Conversation;
use crate::search::normalize_for_search;
use chrono::Local;

#[cfg(test)]
use crate::semantic::types::{
    SemanticChunkIdentity, SemanticChunkSource, SemanticExplanation, SemanticQuality,
    SemanticRationaleKind, SemanticScoreBreakdown,
};

pub struct SemanticConversationFixture {
    path: PathBuf,
    semantic_turns: Vec<String>,
    preview: String,
    preview_first: String,
    preview_last: String,
    full_text: String,
    search_text_lower: String,
    project_name: String,
    project_path: PathBuf,
    cwd: Option<PathBuf>,
    summary: Option<String>,
    custom_title: Option<String>,
}

impl SemanticConversationFixture {
    pub fn new<I, T, P>(path: P, semantic_turns: I) -> Self
    where
        P: AsRef<Path>,
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let preview = "title sentinel";
        let full_text =
            "title sentinel summary sentinel cwd sentinel project sentinel tool output sentinel";
        let summary = "summary sentinel";

        Self {
            path: path.as_ref().to_path_buf(),
            semantic_turns: semantic_turns
                .into_iter()
                .map(|turn| turn.as_ref().to_string())
                .collect(),
            preview: preview.to_string(),
            preview_first: preview.to_string(),
            preview_last: preview.to_string(),
            full_text: full_text.to_string(),
            search_text_lower: full_text.to_string(),
            project_name: "project sentinel".to_string(),
            project_path: PathBuf::from("/projects/project-sentinel"),
            cwd: Some(PathBuf::from("/cwd/sentinel")),
            summary: Some(summary.to_string()),
            custom_title: Some(preview.to_string()),
        }
    }

    pub fn with_title(mut self, title: &str) -> Self {
        let title = title.to_string();
        self.preview = title.clone();
        self.preview_first = title.clone();
        self.preview_last = title.clone();
        self.full_text = title.clone();
        self.search_text_lower = title.clone();
        self.custom_title = Some(title);
        self
    }

    pub fn with_project(mut self, name: &str, project_path: &str) -> Self {
        self.project_name = name.to_string();
        self.project_path = PathBuf::from(project_path);
        self
    }

    pub fn with_cwd(mut self, cwd: Option<&str>) -> Self {
        self.cwd = cwd.map(PathBuf::from);
        self
    }

    pub fn with_summary(mut self, summary: Option<&str>) -> Self {
        self.summary = summary.map(ToString::to_string);
        self
    }

    pub fn with_normalized_search_text_lower(mut self) -> Self {
        self.search_text_lower = normalize_for_search(&self.full_text);
        self
    }

    pub fn build(self) -> Conversation {
        Conversation {
            path: self.path,
            index: 0,
            timestamp: Local::now(),
            preview: self.preview,
            preview_first: self.preview_first,
            preview_last: self.preview_last,
            full_text: self.full_text,
            agent_search_text: String::new(),
            semantic_turn_ranges: (1..=self.semantic_turns.len())
                .map(MessageRange::single)
                .collect(),
            semantic_turns: self.semantic_turns,
            search_text_lower: self.search_text_lower,
            project_name: Some(self.project_name),
            project_path: Some(self.project_path),
            cwd: self.cwd,
            message_count: 1,
            parse_errors: Vec::new(),
            summary: self.summary,
            custom_title: self.custom_title,
            model: None,
            total_tokens: 0,
            duration_minutes: None,
            source_label: None,
        }
    }
}

#[cfg(test)]
pub fn beta_hit_metadata(
    chunk_conversation_index: usize,
    session: &str,
) -> (SemanticScoreBreakdown, SemanticExplanation) {
    (
        SemanticScoreBreakdown {
            hybrid: 1.2,
            semantic: 1.0,
            lexical: 0.2,
        },
        SemanticExplanation {
            quality: SemanticQuality::Strong,
            quality_label: "strong",
            matched_terms: vec!["beta".to_string()],
            evidence_preview: "visible beta".to_string(),
            rationale_kind: SemanticRationaleKind::LexicalBoosted,
            chunk: SemanticChunkIdentity {
                conversation_index: chunk_conversation_index,
                source: SemanticChunkSource::VisibleDialogue,
                session: session.to_string(),
                chunk_index: 0,
                message_range: MessageRange::single(1),
            },
        },
    )
}
