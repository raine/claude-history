use crate::history::Conversation;
use crate::semantic::types::SemanticHit;

pub fn format_hit(rank: usize, hit: &SemanticHit, conversations: &[&Conversation]) -> String {
    let conversation = conversations[hit.conversation_index];
    let project = conversation.project_name.as_deref().unwrap_or("(none)");
    let title = conversation
        .custom_title
        .as_deref()
        .or(conversation.summary.as_deref())
        .unwrap_or(&conversation.preview);
    let session_is_ambiguous = hit.session != "?"
        && conversations
            .iter()
            .filter(|conversation| {
                conversation.path.file_stem().and_then(|stem| stem.to_str())
                    == Some(hit.session.as_str())
            })
            .take(2)
            .count()
            > 1;
    let session_or_path = if hit.session == "?" || session_is_ambiguous {
        conversation.path.display().to_string()
    } else {
        hit.session.clone()
    };

    format!(
        "#{rank:2} hybrid={:.3} semantic={:.3} lexical={:.3} | {project} | {}\n     {title}\n     {}\n",
        hit.score_breakdown.hybrid,
        hit.score_breakdown.semantic,
        hit.score_breakdown.lexical,
        session_or_path,
        truncate(&hit.explanation.evidence_preview, 260)
    )
}

pub fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }

    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::types::{
        SemanticChunkIdentity, SemanticExplanation, SemanticQuality, SemanticRationaleKind,
        SemanticScoreBreakdown,
    };
    use chrono::Local;
    use std::path::PathBuf;

    fn hit(session: &str, snippet: &str) -> SemanticHit {
        let score_breakdown = SemanticScoreBreakdown {
            hybrid: 0.7,
            semantic: 0.5,
            lexical: 0.2,
        };
        let explanation = SemanticExplanation {
            quality: SemanticQuality::Good,
            quality_label: SemanticQuality::Good.label(),
            matched_terms: Vec::new(),
            evidence_preview: snippet.to_string(),
            rationale_kind: SemanticRationaleKind::LexicalBoosted,
            chunk: SemanticChunkIdentity {
                conversation_index: 0,
                source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
                session: session.to_string(),
                chunk_index: 0,
                message_range: crate::agent::refs::MessageRange::single(1),
            },
        };
        SemanticHit::new(score_breakdown, explanation)
    }

    fn conversation() -> Conversation {
        Conversation {
            path: PathBuf::from("/projects/project-a/session-1.jsonl"),
            index: 0,
            timestamp: Local::now(),
            preview: "preview title".to_string(),
            preview_first: "preview title".to_string(),
            preview_last: "preview title".to_string(),
            full_text: String::new(),
            agent_search_text: String::new(),
            semantic_turns: vec![],
            semantic_turn_ranges: vec![],
            search_text_lower: String::new(),
            project_name: Some("project-a".to_string()),
            project_path: None,
            cwd: None,
            message_count: 1,
            parse_errors: vec![],
            summary: None,
            custom_title: Some("custom title".to_string()),
            model: None,
            total_tokens: 0,
            duration_minutes: None,
            source_label: None,
        }
    }

    #[test]
    fn formats_hit_with_selected_conversation_metadata() {
        let conversation = conversation();
        let hit = hit("session-1", "snippet text");

        let formatted = format_hit(1, &hit, &[&conversation]);

        assert!(formatted.contains("# 1 hybrid=0.700 semantic=0.500 lexical=0.200"));
        assert!(formatted.contains("project-a | session-1"));
        assert!(formatted.contains("custom title"));
        assert!(formatted.contains("snippet text"));
    }

    #[test]
    fn formats_path_when_session_is_unknown() {
        let conversation = conversation();
        let hit = hit("?", "snippet text");

        let formatted = format_hit(1, &hit, &[&conversation]);

        assert!(formatted.contains("project-a | /projects/project-a/session-1.jsonl"));
    }

    #[test]
    fn formats_path_when_session_is_ambiguous() {
        let first = conversation();
        let mut second = conversation();
        second.path = PathBuf::from("/projects/project-b/session-1.jsonl");
        let hit = hit("session-1", "snippet text");

        let formatted = format_hit(1, &hit, &[&first, &second]);

        assert!(formatted.contains("project-a | /projects/project-a/session-1.jsonl"));
    }

    #[test]
    fn truncates_long_snippets() {
        let text = "a".repeat(300);
        let truncated = truncate(&text, 10);

        assert_eq!(truncated.chars().count(), 10);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn formatted_hit_truncates_long_evidence_preview() {
        let conversation = conversation();
        let snippet = format!("{}tail sentinel", "a".repeat(300));
        let hit = hit("session-1", &snippet);

        let formatted = format_hit(1, &hit, &[&conversation]);

        assert!(formatted.contains('…'));
        assert!(!formatted.contains("tail sentinel"));
    }
}
