use crate::history::Conversation;
use chrono::{DateTime, Duration, Local};
use rayon::prelude::*;

/// Precomputed search data for a conversation
pub struct SearchableConversation {
    /// Lowercased full text for searching
    pub text_lower: String,
    /// Original full text (moved from Conversation to avoid duplication)
    pub full_text: String,
    /// Original conversation index
    pub index: usize,
}

/// Normalize text for search: lowercase and replace underscores with spaces
pub fn normalize_for_search(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == '_' {
            out.push(' ');
        } else {
            out.extend(ch.to_lowercase());
        }
    }
    out
}

/// Check if a character is a word separator for search purposes
pub fn is_word_separator(c: char) -> bool {
    c.is_whitespace() || c == '_'
}

/// Precompute lowercased search text for all conversations.
/// Moves `full_text` ownership from each Conversation into SearchableConversation
/// to avoid storing the same text twice in memory.
pub fn precompute_search_text(conversations: &mut [Conversation]) -> Vec<SearchableConversation> {
    conversations
        .iter_mut()
        .enumerate()
        .map(|(idx, conv)| {
            let full_text = std::mem::take(&mut conv.full_text);
            let text_lower = normalize_for_search(&full_text);
            SearchableConversation {
                text_lower,
                full_text,
                index: idx,
            }
        })
        .collect()
}

/// Filter and score conversations based on query.
/// Returns indices into the original conversations vec, sorted by score descending.
/// When `narrow_from` is provided, only scores those indices (used when query extends previous).
pub fn search(
    conversations: &[Conversation],
    searchable: &[SearchableConversation],
    query: &str,
    now: DateTime<Local>,
    narrow_from: Option<&[usize]>,
) -> Vec<usize> {
    let query = query.trim();
    if query.is_empty() {
        // Return all indices sorted by timestamp (already sorted in history.rs)
        return (0..conversations.len()).collect();
    }

    let query_lower = normalize_for_search(query);
    let query_words: Vec<&str> = query_lower.split_whitespace().collect();
    if query_words.is_empty() {
        return (0..conversations.len()).collect();
    }

    // Score conversations in parallel, optionally narrowing to a subset
    let mut scored: Vec<(usize, f64, DateTime<Local>)> = if let Some(indices) = narrow_from {
        let allowed: std::collections::HashSet<usize> = indices.iter().copied().collect();
        searchable
            .par_iter()
            .filter(|s| allowed.contains(&s.index))
            .filter_map(|s| {
                let score = score_text(
                    &s.text_lower,
                    &query_words,
                    conversations[s.index].timestamp,
                    now,
                );
                if score > 0.0 {
                    Some((s.index, score, conversations[s.index].timestamp))
                } else {
                    None
                }
            })
            .collect()
    } else {
        searchable
            .par_iter()
            .filter_map(|s| {
                let score = score_text(
                    &s.text_lower,
                    &query_words,
                    conversations[s.index].timestamp,
                    now,
                );
                if score > 0.0 {
                    Some((s.index, score, conversations[s.index].timestamp))
                } else {
                    None
                }
            })
            .collect()
    };

    // Sort by score descending, then by timestamp descending for stability
    scored.sort_unstable_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.2.cmp(&a.2))
    });

    scored.into_iter().map(|(idx, _, _)| idx).collect()
}

/// Score a conversation based on word prefix matching and recency.
/// Each query word must be a prefix of at least one word in the text (AND logic).
fn score_text(
    text_lower: &str,
    query_words: &[&str],
    timestamp: DateTime<Local>,
    now: DateTime<Local>,
) -> f64 {
    if query_words.is_empty() {
        return 0.0;
    }

    // Fast rejection: if a query word isn't present as substring, skip expensive checking
    for &qw in query_words {
        if !text_lower.contains(qw) {
            return 0.0;
        }
    }

    // Single-pass word matching with tracking
    let mut matched = vec![false; query_words.len()];
    let mut remaining = query_words.len();

    for text_word in text_lower.split_whitespace() {
        for (i, &qw) in query_words.iter().enumerate() {
            if !matched[i] && text_word.starts_with(qw) {
                matched[i] = true;
                remaining -= 1;
                if remaining == 0 {
                    return (query_words.len() as f64) * recency_multiplier(timestamp, now);
                }
            }
        }
    }

    0.0
}

/// Calculate recency multiplier based on age
fn recency_multiplier(timestamp: DateTime<Local>, now: DateTime<Local>) -> f64 {
    let age = now.signed_duration_since(timestamp);

    // Handle future timestamps (shouldn't happen, but be safe)
    if age < Duration::zero() {
        return 3.0;
    }

    if age < Duration::days(1) {
        3.0
    } else if age < Duration::days(7) {
        2.0
    } else if age < Duration::days(30) {
        1.5
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::Conversation;
    use std::path::PathBuf;

    fn make_conv(text: &str, timestamp: DateTime<Local>) -> Conversation {
        Conversation {
            path: PathBuf::new(),
            index: 0,
            provider: crate::history::ProviderKind::Claude,
            id: String::new(),
            timestamp,
            preview: text.to_string(),
            full_text: text.to_string(),
            project_name: None,
            project_path: None,
            cwd: None,
            message_count: 1,
            parse_errors: vec![],
            summary: None,
            model: None,
            total_tokens: 0,
            duration_minutes: None,
        }
    }

    #[test]
    fn search_matches_underscore_separated() {
        let now = Local::now();
        let mut convs = vec![make_conv("HARDENED_RUNTIME config", now)];
        let searchable = precompute_search_text(&mut convs);
        let results = search(&convs, &searchable, "harden runtime", now, None);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_matches_different_case() {
        let now = Local::now();
        let mut convs = vec![make_conv("Hardened Runtime enabled", now)];
        let searchable = precompute_search_text(&mut convs);
        let results = search(&convs, &searchable, "harden runtime", now, None);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_prefix_matches_words() {
        let now = Local::now();
        let mut convs = vec![make_conv("hardened security", now)];
        let searchable = precompute_search_text(&mut convs);
        let results = search(&convs, &searchable, "harden", now, None);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_requires_all_words() {
        let now = Local::now();
        let mut convs = vec![make_conv("hardened security", now)];
        let searchable = precompute_search_text(&mut convs);
        let results = search(&convs, &searchable, "harden runtime", now, None);
        assert_eq!(results.len(), 0); // "runtime" not present
    }

    #[test]
    fn search_with_underscore_in_query() {
        let now = Local::now();
        let mut convs = vec![make_conv("hardened runtime enabled", now)];
        let searchable = precompute_search_text(&mut convs);
        // Query with underscore should still match space-separated text
        let results = search(&convs, &searchable, "hardened_runtime", now, None);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn recency_today_gets_highest_multiplier() {
        let now = Local::now();
        let timestamp = now - Duration::hours(1);
        assert_eq!(recency_multiplier(timestamp, now), 3.0);
    }

    #[test]
    fn recency_this_week_gets_medium_multiplier() {
        let now = Local::now();
        let timestamp = now - Duration::days(3);
        assert_eq!(recency_multiplier(timestamp, now), 2.0);
    }

    #[test]
    fn recency_this_month_gets_low_multiplier() {
        let now = Local::now();
        let timestamp = now - Duration::days(15);
        assert_eq!(recency_multiplier(timestamp, now), 1.5);
    }

    #[test]
    fn recency_older_gets_base_multiplier() {
        let now = Local::now();
        let timestamp = now - Duration::days(60);
        assert_eq!(recency_multiplier(timestamp, now), 1.0);
    }

    #[test]
    fn future_timestamp_gets_highest_multiplier() {
        let now = Local::now();
        let timestamp = now + Duration::hours(1);
        assert_eq!(recency_multiplier(timestamp, now), 3.0);
    }
}
