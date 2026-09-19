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

/// Returns true if byte offset `end` in `text` is at the end of a word.
pub fn is_word_end(text: &str, end: usize) -> bool {
    end >= text.len()
        || text[end..]
            .chars()
            .next()
            .is_some_and(|c| !c.is_alphanumeric())
}

/// A query word that starts with punctuation (`.rs`, `@scope`, `#[derive`)
/// carries its own boundary, so it may match inside a longer token such as
/// `lexical.rs`. Words that start alphanumerically must start a word. This is
/// the same rule `search/matcher.rs` uses for highlighting.
pub fn requires_word_start(word: &str) -> bool {
    word.chars().next().is_some_and(char::is_alphanumeric)
}

pub fn contains_prefix_match(text: &str, word: &str) -> bool {
    count_prefix_matches(text, word, 1) > 0
}

/// Count prefix matches of `word` in `text`, up to `max_count`.
pub fn count_prefix_matches(text: &str, word: &str, max_count: usize) -> usize {
    count_matches(text, word, max_count, false)
}

/// Count whole-word matches of `word` in `text` (a prefix match that also
/// ends at a word boundary), up to `max_count`.
pub fn count_exact_word_matches(text: &str, word: &str, max_count: usize) -> usize {
    count_matches(text, word, max_count, true)
}

fn count_matches(text: &str, word: &str, max_count: usize, require_end: bool) -> usize {
    if word.is_empty() {
        return 0;
    }
    let require_start = requires_word_start(word);
    let mut start = 0;
    let mut count = 0;
    while let Some(pos) = text[start..].find(word) {
        let actual_pos = start + pos;
        if (!require_start || is_word_start(text, actual_pos))
            && (!require_end || is_word_end(text, actual_pos + word.len()))
        {
            count += 1;
            if count >= max_count {
                break;
            }
        }
        start = actual_pos + word.len();
    }
    count
}

/// Case-insensitive ASCII substring test on raw (un-normalized) text.
/// Non-ASCII bytes must match exactly.
pub fn contains_ignore_ascii_case(text: &str, needle: &str) -> bool {
    let needle = needle.as_bytes();
    let text = text.as_bytes();
    if needle.is_empty() || needle.len() > text.len() {
        return false;
    }
    let first = needle[0].to_ascii_lowercase();
    text.windows(needle.len()).any(|window| {
        window[0].to_ascii_lowercase() == first && window.eq_ignore_ascii_case(needle)
    })
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

    #[test]
    fn punctuation_led_word_matches_inside_token() {
        assert!(contains_prefix_match("src search lexical.rs", ".rs"));
        assert!(contains_prefix_match("use @scope pkg", "@scope"));
        assert!(!contains_prefix_match("src search lexical.rs", "cal"));
    }

    #[test]
    fn exact_word_requires_end_boundary() {
        assert_eq!(
            count_exact_word_matches("cache cached caches cache", "cache", 10),
            2
        );
        assert_eq!(count_exact_word_matches("cached", "cache", 10), 0);
        assert_eq!(count_exact_word_matches("lexical.rs done", ".rs", 10), 1);
    }

    #[test]
    fn ascii_case_insensitive_contains() {
        assert!(contains_ignore_ascii_case(
            "run --Debug-Search now",
            "--debug-search"
        ));
        assert!(!contains_ignore_ascii_case(
            "debug search",
            "--debug-search"
        ));
        assert!(!contains_ignore_ascii_case("abc", ""));
    }
}
