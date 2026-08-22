fn is_cjk_punctuation(c: char) -> bool {
    matches!(
        c,
        '\u{3000}'
            | '\u{3001}'
            | '\u{3002}'
            | '\u{3008}'
            | '\u{3009}'
            | '\u{300A}'
            | '\u{300B}'
            | '\u{300C}'
            | '\u{300D}'
            | '\u{300E}'
            | '\u{300F}'
            | '\u{3010}'
            | '\u{3011}'
            | '\u{3014}'
            | '\u{3015}'
            | '\u{3016}'
            | '\u{3017}'
            | '\u{FF01}'
            | '\u{FF08}'
            | '\u{FF09}'
            | '\u{FF0C}'
            | '\u{FF1A}'
            | '\u{FF1B}'
            | '\u{FF1F}'
            | '\u{201C}'
            | '\u{201D}'
            | '\u{2018}'
            | '\u{2019}'
            | '\u{2014}'
            | '\u{2026}'
            | '\u{00B7}'
    )
}

pub fn normalize_for_search(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if is_word_separator(ch) {
            out.push(' ');
        } else if ch.is_ascii() {
            // Same result as `ch.to_lowercase()` for ASCII, without the iterator.
            out.push(ch.to_ascii_lowercase());
        } else {
            out.extend(ch.to_lowercase());
        }
    }
    out
}
pub fn is_word_separator(c: char) -> bool {
    c.is_whitespace() || c == '_' || c == '-' || c == '/' || is_cjk_punctuation(c)
}

pub fn is_word_start(text: &str, pos: usize) -> bool {
    pos == 0
        || text[..pos]
            .chars()
            .next_back()
            .is_some_and(|c| !c.is_alphanumeric())
}

pub fn contains_prefix_match(text: &str, word: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = text[start..].find(word) {
        let actual_pos = start + pos;
        if is_word_start(text, actual_pos) {
            return true;
        }
        start = actual_pos + word.len().max(1);
    }
    false
}

pub fn contains_cjk(text: &str) -> bool {
    text.chars().any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_search_separators_and_case() {
        assert_eq!(
            normalize_for_search("HARDENED_RUNTIME/main-worktree"),
            "hardened runtime main worktree"
        );
    }

    #[test]
    fn normalizes_cjk_punctuation() {
        assert_eq!(
            normalize_for_search("SIGTERM\u{FF0C}\u{5C5E}\u{4E8E}"),
            "sigterm \u{5C5E}\u{4E8E}"
        );
    }

    #[test]
    fn prefix_match_respects_word_start() {
        assert!(contains_prefix_match("redaction plan", "red"));
        assert!(!contains_prefix_match("fired plan", "red"));
    }
}
