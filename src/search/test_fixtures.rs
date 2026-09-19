use crate::history::Conversation;
use crate::history::MessageRange;
use crate::search::normalize_for_search;
use chrono::{DateTime, Local, TimeZone};
use std::path::PathBuf;

/// A conversation whose visible `preview` differs from its `full_text`.
pub fn conversation_with_text(preview: &str, full_text: &str) -> Conversation {
    let mut conversation = one_message_conversation(
        preview,
        Local.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
        None,
        None,
        None,
    );
    conversation.full_text = full_text.to_string();
    conversation.search_text_lower = normalize_for_search(full_text);
    conversation.dialogue_text_lower = normalize_for_search(full_text);
    conversation.semantic_turns = vec![full_text.to_string()];
    conversation
}

pub fn one_message_conversation(
    text: &str,
    timestamp: DateTime<Local>,
    summary: Option<&str>,
    title: Option<&str>,
    project: Option<&str>,
) -> Conversation {
    let mut full_text = text.to_string();
    if let Some(summary) = summary {
        full_text = format!("{} {}", summary, full_text);
    }
    if let Some(title) = title {
        full_text = format!("{} {}", title, full_text);
    }

    Conversation {
        source: crate::history::Source::Claude,
        session_id: String::new(),
        path: PathBuf::new(),
        index: 0,
        timestamp,
        preview: text.to_string(),
        preview_first: text.to_string(),
        preview_last: text.to_string(),
        full_text: full_text.clone(),
        agent_search_text: String::new(),
        semantic_route_text: String::new(),
        semantic_turns: vec![text.to_string()],
        semantic_turn_ranges: vec![MessageRange::single(1)],
        search_text_lower: normalize_for_search(&full_text),
        dialogue_text_lower: normalize_for_search(text),
        project_name: project.map(str::to_string),
        project_path: None,
        cwd: None,
        message_count: 1,
        parse_errors: vec![],
        summary: summary.map(str::to_string),
        custom_title: title.map(str::to_string),
        model: None,
        total_tokens: 0,
        duration_minutes: None,
    }
}
