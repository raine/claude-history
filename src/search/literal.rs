use crate::history::Conversation;
use rayon::prelude::*;
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseMode {
    Sensitive,
    Insensitive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Literal {
    text: String,
    case_mode: CaseMode,
}

impl Literal {
    pub fn new(text: String) -> Self {
        let case_mode = if text.chars().any(char::is_uppercase) {
            CaseMode::Sensitive
        } else {
            CaseMode::Insensitive
        };
        Self { text, case_mode }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn case_mode(&self) -> CaseMode {
        self.case_mode
    }

    pub fn matches(&self, text: &str) -> bool {
        if self.text.is_empty() {
            return false;
        }

        match self.case_mode {
            CaseMode::Sensitive => {
                text.contains(&self.text) || contains_ignoring_markdown_noise(text, &self.text)
            }
            CaseMode::Insensitive => contains_case_insensitive(text, &self.text),
        }
    }

    pub fn match_ranges(&self, text: &str) -> Vec<(usize, usize)> {
        if self.text.is_empty() {
            return Vec::new();
        }

        match self.case_mode {
            CaseMode::Sensitive => {
                let ranges = find_substring_ranges(text, &self.text);
                if !ranges.is_empty() {
                    ranges
                } else {
                    find_ranges_ignoring_markdown_noise(text, &self.text)
                }
            }
            CaseMode::Insensitive => find_case_insensitive_ranges(text, &self.text),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiteralCorpusEntry {
    pub index: usize,
    pub text: String,
}

pub fn build_literal_corpus(conversations: &[Conversation]) -> Vec<LiteralCorpusEntry> {
    build_literal_corpus_with(conversations, false)
}

pub fn build_agent_literal_corpus(conversations: &[Conversation]) -> Vec<LiteralCorpusEntry> {
    build_literal_corpus_with(conversations, true)
}

fn build_literal_corpus_with(
    conversations: &[Conversation],
    include_agent_text: bool,
) -> Vec<LiteralCorpusEntry> {
    conversations
        .par_iter()
        .enumerate()
        .map(|(index, conversation)| LiteralCorpusEntry {
            index,
            text: literal_text(conversation, include_agent_text),
        })
        .collect()
}

pub fn matches_all_literals(text: &str, literals: &[Literal]) -> bool {
    literals.iter().all(|literal| literal.matches(text))
}

pub fn conversation_matches_all_literals(
    conversation: &Conversation,
    literals: &[Literal],
) -> bool {
    matches_all_literals(&literal_text(conversation, false), literals)
}

pub fn match_literal_ranges(text: &str, literals: &[Literal]) -> Vec<(usize, usize)> {
    literals
        .iter()
        .flat_map(|literal| literal.match_ranges(text))
        .collect()
}

pub fn exact_fallback(
    conversations: &[Conversation],
    corpus: &[LiteralCorpusEntry],
    literals: &[Literal],
    scope: impl Fn(usize) -> bool + Sync,
) -> Vec<usize> {
    if literals.is_empty() {
        return Vec::new();
    }

    let mut matches = corpus
        .par_iter()
        .filter(|entry| scope(entry.index) && matches_all_literals(&entry.text, literals))
        .map(|entry| (entry.index, conversations[entry.index].timestamp))
        .collect::<Vec<_>>();

    matches.sort_unstable_by(|a, b| b.1.cmp(&a.1));
    matches.into_iter().map(|(index, _)| index).collect()
}

/// Markdown punctuation the rendered viewer strips away (inline code
/// backticks, emphasis asterisks) so a phrase copied from the viewer
/// (e.g. `person {name}`) doesn't include it even though the stored
/// transcript text does (`` `person {name}` ``). Ignored on both sides of
/// a literal comparison; callers that need highlight ranges must map
/// matches found this way back through the original (unstripped) text,
/// which is what the `_ignoring_markdown_noise` range functions below do.
fn is_markdown_noise(c: char) -> bool {
    matches!(c, '`' | '*')
}

fn contains_case_insensitive(text: &str, needle: &str) -> bool {
    let needle_chars: Vec<char> = needle
        .chars()
        .filter(|c| !is_markdown_noise(*c))
        .flat_map(char::to_lowercase)
        .collect();
    if needle_chars.is_empty() {
        return false;
    }

    let mut window = VecDeque::with_capacity(needle_chars.len());
    for folded in text
        .chars()
        .filter(|c| !is_markdown_noise(*c))
        .flat_map(char::to_lowercase)
    {
        window.push_back(folded);
        if window.len() > needle_chars.len() {
            window.pop_front();
        }
        if window.len() == needle_chars.len()
            && window.iter().copied().eq(needle_chars.iter().copied())
        {
            return true;
        }
    }
    false
}

fn contains_ignoring_markdown_noise(text: &str, needle: &str) -> bool {
    let needle_chars: Vec<char> = needle.chars().filter(|c| !is_markdown_noise(*c)).collect();
    if needle_chars.is_empty() {
        return false;
    }

    let mut window = VecDeque::with_capacity(needle_chars.len());
    for ch in text.chars().filter(|c| !is_markdown_noise(*c)) {
        window.push_back(ch);
        if window.len() > needle_chars.len() {
            window.pop_front();
        }
        if window.len() == needle_chars.len()
            && window.iter().copied().eq(needle_chars.iter().copied())
        {
            return true;
        }
    }
    false
}

fn find_substring_ranges(text: &str, needle: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0;
    while let Some(pos) = text[start..].find(needle) {
        let range_start = start + pos;
        let range_end = range_start + needle.len();
        ranges.push((range_start, range_end));
        start = range_end;
    }
    ranges
}

/// Like `find_substring_ranges`, but ignores markdown noise (backticks,
/// `*`) on both sides of the comparison, mapping any match back to the
/// byte range it spans in the original (unstripped) `text`.
fn find_ranges_ignoring_markdown_noise(text: &str, needle: &str) -> Vec<(usize, usize)> {
    let needle_chars: Vec<char> = needle.chars().filter(|c| !is_markdown_noise(*c)).collect();
    if needle_chars.is_empty() {
        return Vec::new();
    }

    let mut chars = Vec::new();
    let mut map = Vec::new();
    for (start, ch) in text.char_indices() {
        if is_markdown_noise(ch) {
            continue;
        }
        let end = start + ch.len_utf8();
        chars.push(ch);
        map.push((start, end));
    }

    let mut ranges = Vec::new();
    let mut i = 0;
    while i + needle_chars.len() <= chars.len() {
        if chars[i..i + needle_chars.len()] == needle_chars[..] {
            ranges.push((map[i].0, map[i + needle_chars.len() - 1].1));
            i += needle_chars.len();
        } else {
            i += 1;
        }
    }
    ranges
}

fn find_case_insensitive_ranges(text: &str, needle: &str) -> Vec<(usize, usize)> {
    let needle_chars: Vec<char> = needle
        .chars()
        .filter(|c| !is_markdown_noise(*c))
        .flat_map(char::to_lowercase)
        .collect();
    if needle_chars.is_empty() {
        return Vec::new();
    }

    let mut folded_chars = Vec::new();
    let mut folded_map = Vec::new();
    for (start, ch) in text.char_indices() {
        if is_markdown_noise(ch) {
            continue;
        }
        let end = start + ch.len_utf8();
        for folded in ch.to_lowercase() {
            folded_chars.push(folded);
            folded_map.push((start, end));
        }
    }

    let mut ranges = Vec::new();
    let mut i = 0;
    while i + needle_chars.len() <= folded_chars.len() {
        if folded_chars[i..i + needle_chars.len()] == needle_chars[..] {
            ranges.push((folded_map[i].0, folded_map[i + needle_chars.len() - 1].1));
            i += needle_chars.len();
        } else {
            i += 1;
        }
    }
    ranges
}

fn literal_text(conversation: &Conversation, include_agent_text: bool) -> String {
    let mut text = String::new();
    push_part(&mut text, Some(&conversation.full_text));
    if include_agent_text {
        push_part(&mut text, Some(&conversation.agent_search_text));
    }
    push_part(&mut text, conversation.project_name.as_deref());
    text
}

fn push_part(text: &mut String, part: Option<&str>) {
    let Some(part) = part else {
        return;
    };
    if part.is_empty() {
        return;
    }
    if !text.is_empty() {
        text.push(' ');
    }
    text.push_str(part);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::test_fixtures::one_message_conversation;
    use chrono::{Duration, Local};

    fn make_conv_full(
        text: &str,
        project: Option<&str>,
        title: Option<&str>,
        summary: Option<&str>,
        timestamp: chrono::DateTime<Local>,
    ) -> Conversation {
        one_message_conversation(text, timestamp, summary, title, project)
    }

    #[test]
    fn literal_uses_smart_case() {
        assert!(Literal::new("exact_phrase".to_string()).matches("EXACT_PHRASE"));
        assert!(!Literal::new("Exact_Phrase".to_string()).matches("exact_phrase"));
        assert!(Literal::new("Exact_Phrase".to_string()).matches("Exact_Phrase"));
    }

    #[test]
    fn literal_ranges_use_smart_case() {
        let insensitive = Literal::new("exact_phrase".to_string());
        let sensitive = Literal::new("Exact_Phrase".to_string());

        assert_eq!(insensitive.match_ranges("EXACT_PHRASE"), vec![(0, 12)]);
        assert_eq!(sensitive.match_ranges("exact_phrase"), Vec::new());
        assert_eq!(sensitive.match_ranges("Exact_Phrase"), vec![(0, 12)]);
    }

    #[test]
    fn insensitive_literal_ranges_are_original_text_boundaries() {
        let text = "pre İSTANBUL post";
        let literal = Literal::new("i\u{307}stanbul".to_string());
        let ranges = literal.match_ranges(text);

        assert_eq!(ranges, vec![(4, 13)]);
        assert_eq!(&text[ranges[0].0..ranges[0].1], "İSTANBUL");
    }

    #[test]
    fn insensitive_literal_match_uses_smart_case() {
        let literal = Literal::new("i\u{307}stanbul".to_string());

        assert!(literal.matches("pre İSTANBUL post"));
        assert!(!literal.matches("pre constantinople post"));
    }

    #[test]
    fn quoted_literal_matches_phrase_copied_from_rendered_markdown() {
        // The viewer renders inline code away, so a phrase copied from it
        // (no backticks) must still match the stored markdown source
        // (backticks intact). This literal has an uppercase letter, so
        // it exercises the case-sensitive path.
        let text = "What the example holds today: `person {name}`, `message {said, by}`, both `delivery: held`";
        let literal = Literal::new("What the example holds today: person {name}".to_string());

        assert!(literal.matches(text));
    }

    #[test]
    fn quoted_literal_ignoring_backticks_matches_lowercase_too() {
        let text = "notes: `person {name}` shows up twice";
        let literal = Literal::new("person {name}".to_string());

        assert!(literal.matches(text));
    }

    #[test]
    fn backtick_spanning_match_range_is_original_text_boundary() {
        let text = "today: `person {name}`, more text";
        let literal = Literal::new("person {name}".to_string());
        let ranges = literal.match_ranges(text);

        assert_eq!(ranges.len(), 1);
        let (start, end) = ranges[0];
        // Backticks are ignored, not highlighted: the range covers the
        // matched characters' own positions in the original text, which
        // must be a valid (non-panicking) boundary either side of them.
        assert_eq!(&text[start..end], "person {name}");
    }

    #[test]
    fn corpus_preserves_punctuation_and_metadata() {
        let now = Local::now();
        let conversations = vec![make_conv_full(
            "body_with_under_score and punctuation: yes",
            Some("project_name/raw-value"),
            Some("Title: Raw_Value"),
            Some("Summary.with punctuation"),
            now,
        )];

        let corpus = build_literal_corpus(&conversations);

        assert!(corpus[0].text.contains("body_with_under_score"));
        assert!(corpus[0].text.contains("punctuation: yes"));
        assert!(corpus[0].text.contains("project_name/raw-value"));
        assert!(corpus[0].text.contains("Title: Raw_Value"));
        assert!(corpus[0].text.contains("Summary.with punctuation"));
    }

    #[test]
    fn corpus_does_not_duplicate_title_and_summary() {
        let now = Local::now();
        let conversations = vec![make_conv_full(
            "body sentinel",
            None,
            Some("TitleA"),
            Some("SummaryB"),
            now,
        )];

        let corpus = build_literal_corpus(&conversations);

        assert!(!corpus[0].text.contains("SummaryB TitleA"));
    }

    #[test]
    fn exact_fallback_returns_scoped_matches_newest_first() {
        let now = Local::now();
        let conversations = vec![
            make_conv_full("needle phrase", None, None, None, now - Duration::days(1)),
            make_conv_full("needle phrase", None, None, None, now),
            make_conv_full("needle phrase", None, None, None, now - Duration::hours(1)),
        ];
        let corpus = build_literal_corpus(&conversations);
        let literal = Literal::new("needle phrase".to_string());

        let results = exact_fallback(&conversations, &corpus, &[literal], |index| index != 1);

        assert_eq!(results, vec![2, 0]);
    }

    #[test]
    fn exact_fallback_requires_all_literals() {
        let now = Local::now();
        let conversations = vec![
            make_conv_full("alpha beta", None, None, None, now),
            make_conv_full("alpha gamma", None, None, None, now),
        ];
        let corpus = build_literal_corpus(&conversations);
        let literals = vec![
            Literal::new("alpha".to_string()),
            Literal::new("beta".to_string()),
        ];

        let results = exact_fallback(&conversations, &corpus, &literals, |_| true);

        assert_eq!(results, vec![0]);
    }
}
