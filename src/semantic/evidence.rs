use crate::text_match::{contains_cjk, contains_prefix_match, normalize_for_search};

pub fn query_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let normalized = normalize_for_search(query);
    for term in normalized.split_whitespace() {
        if !terms.iter().any(|existing| existing == term) {
            terms.push(term.to_string());
        }
    }
    terms
}

#[cfg(test)]
pub fn matched_terms(query: &str, text: &str) -> Vec<String> {
    matched_terms_prepared(&query_terms(query), &normalize_for_search(text))
}

/// Same as `matched_terms`, for callers that already hold the normalized
/// query terms and the normalized text (ranking does this once per chunk
/// instead of once per chunk per query).
pub fn matched_terms_prepared(terms: &[String], normalized_text: &str) -> Vec<String> {
    terms
        .iter()
        .filter(|term| {
            contains_prefix_match(normalized_text, term)
                || (contains_cjk(term) && normalized_text.contains(term.as_str()))
        })
        .cloned()
        .collect()
}
/// True when `evidence_preview(text)` would return `text` unchanged: no tag
/// opener, no whitespace other than single ASCII spaces, nothing to trim.
/// Lets callers skip the preview pass (and its copy) for normalized turns.
pub fn preview_is_identity(text: &str) -> bool {
    if text.is_empty() {
        return true;
    }
    let mut last_was_space = true; // leading space would be trimmed
    for ch in text.chars() {
        if ch == '<' {
            return false;
        }
        if ch.is_whitespace() {
            if ch != ' ' || last_was_space {
                return false;
            }
            last_was_space = true;
        } else {
            last_was_space = false;
        }
    }
    !last_was_space
}
pub fn evidence_preview(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut last_was_space = false;
    let mut index = 0;

    while index < text.len() {
        let Some(ch) = text[index..].chars().next() else {
            break;
        };
        if ch != '<' {
            push_collapsed(&mut result, ch, &mut last_was_space);
            index += ch.len_utf8();
            continue;
        }

        let Some(relative_end) = text[index..].find('>') else {
            push_collapsed(&mut result, ch, &mut last_was_space);
            index += ch.len_utf8();
            continue;
        };
        let tag_end = index + relative_end;
        let tag = &text[index + 1..tag_end];
        let Some(name) = structural_tag_name(tag) else {
            for tag_ch in text[index..=tag_end].chars() {
                push_collapsed(&mut result, tag_ch, &mut last_was_space);
            }
            index = tag_end + 1;
            continue;
        };

        if !last_was_space {
            result.push(' ');
            last_was_space = true;
        }
        index = tag_end + 1;
        if !tag.trim_start().starts_with('/') {
            let closing = format!("</{name}>");
            if let Some(close_start) = text[index..].find(&closing) {
                index += close_start + closing.len();
            }
        }
    }

    result.trim().to_string()
}

fn structural_tag_name(tag: &str) -> Option<&str> {
    let name = tag
        .trim()
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or_default();
    name.contains('-').then_some(name)
}

fn push_collapsed(result: &mut String, ch: char, last_was_space: &mut bool) {
    if ch.is_whitespace() {
        if !*last_was_space {
            result.push(' ');
            *last_was_space = true;
        }
    } else {
        result.push(ch);
        *last_was_space = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_terms_are_normalized_unique_and_ordered() {
        assert_eq!(
            query_terms("Rust-cache rust/cache RUST"),
            vec!["rust", "cache"]
        );
    }

    #[test]
    fn matched_terms_use_shared_normalization() {
        assert_eq!(
            matched_terms(
                "Audio generation audio",
                "The audio_generation pipeline works"
            ),
            vec!["audio", "generation"]
        );
    }

    #[test]
    fn evidence_preview_removes_structural_tags_and_collapses_space() {
        assert_eq!(
            evidence_preview("alpha\n<system-reminder>hidden</system-reminder>\t beta"),
            "alpha beta"
        );
    }

    #[test]
    fn evidence_preview_preserves_code_like_angles() {
        assert_eq!(evidence_preview("Vec<T> where x < y"), "Vec<T> where x < y");
    }
}
