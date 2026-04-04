use crate::agent::refs::MessageRange;
use crate::history::Conversation;
use crate::search::normalize_for_search;
use chrono::{DateTime, Local};
use std::path::PathBuf;

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
        path: PathBuf::new(),
        index: 0,
        timestamp,
        preview: text.to_string(),
        preview_first: text.to_string(),
        preview_last: text.to_string(),
        full_text: full_text.clone(),
        agent_search_text: String::new(),
        semantic_turns: vec![text.to_string()],
        semantic_turn_ranges: vec![MessageRange::single(1)],
        search_text_lower: normalize_for_search(&full_text),
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
        source_label: None,
    }
}
