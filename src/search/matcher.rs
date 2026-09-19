//! Locating a parsed query inside text.
//!
//! Everything the search vocabulary knows about *where* a query matches lives
//! here: highlight ranges for the list view, "is this literal visible in the
//! preview" checks, and the hidden-context evidence the lexical worker
//! precomputes per hit. Scoring (`search/lexical.rs`) and word-boundary rules
//! (`text_match.rs`) are the other two halves of the vocabulary; this module
//! must agree with them on what counts as a match.

use crate::history::Conversation;
use crate::search::literal::Literal;
use crate::search::query::ParsedQuery;

/// Byte ranges into `full_text` worth showing when the preview hides a match.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LexicalEvidence {
    pub context_ranges: Vec<(usize, usize)>,
}

pub fn build_lexical_evidence(
    conversation: &Conversation,
    parsed: &ParsedQuery,
) -> Option<LexicalEvidence> {
    let ranges =
        QueryMatcher::new(parsed).hidden_context(&conversation.full_text, &conversation.preview)?;
    Some(LexicalEvidence {
        context_ranges: ranges,
    })
}

/// One thing the query asks for: a normalized, prefix-matched word or an
/// exact (smart-case) literal.
#[derive(Clone, Debug)]
enum Term {
    Word(String),
    Literal(Literal),
}

impl Term {
    fn key(&self) -> (&str, bool) {
        match self {
            Self::Word(word) => (word.as_str(), false),
            Self::Literal(literal) => (literal.text(), true),
        }
    }

    fn ranges(&self, text: &str) -> Vec<(usize, usize)> {
        match self {
            Self::Word(word) => find_word_ranges(text, word),
            Self::Literal(literal) => literal.match_ranges(text),
        }
    }

    fn matches(&self, text: &str) -> bool {
        match self {
            Self::Word(word) => find_first_word_range(text, word).is_some(),
            Self::Literal(literal) => literal.matches(text),
        }
    }
}

/// A [`ParsedQuery`] prepared for locating matches in arbitrary text.
#[derive(Clone, Debug, Default)]
pub struct QueryMatcher {
    terms: Vec<Term>,
}

/// Cluster ranking tracks terms in a `u64` bitmask.
const MAX_TERMS: usize = 64;

impl QueryMatcher {
    pub fn new(parsed: &ParsedQuery) -> Self {
        let terms = parsed
            .words()
            .into_iter()
            .map(Term::Word)
            .chain(parsed.all_literals().into_iter().map(Term::Literal));
        Self {
            terms: dedupe_terms(terms),
        }
    }

    pub fn from_query(query: &str) -> Self {
        Self::new(&ParsedQuery::parse(query))
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    pub fn has_literals(&self) -> bool {
        self.terms
            .iter()
            .any(|term| matches!(term, Term::Literal(_)))
    }

    /// The same query restricted to its exact literals.
    pub fn literals_only(&self) -> Self {
        Self {
            terms: self
                .terms
                .iter()
                .filter(|term| matches!(term, Term::Literal(_)))
                .cloned()
                .collect(),
        }
    }

    /// True when any term occurs in `text`.
    pub fn matches(&self, text: &str) -> bool {
        self.terms.iter().any(|term| term.matches(text))
    }

    /// True when at least one literal is absent from `text`, i.e. the row
    /// needs a context line to show why it matched.
    pub fn literals_missing_from(&self, text: &str) -> bool {
        self.terms.iter().any(|term| match term {
            Term::Literal(literal) => !literal.matches(text),
            Term::Word(_) => false,
        })
    }

    /// Sorted, non-overlapping byte ranges to highlight in `text`. Word hits
    /// separated only by separator characters merge into one span so
    /// `run_with_loader` highlights whole when searching `run with loader`.
    pub fn ranges(&self, text: &str) -> Vec<(usize, usize)> {
        let mut word_ranges = Vec::new();
        let mut ranges = Vec::new();
        for term in &self.terms {
            match term {
                Term::Word(word) => word_ranges.extend(find_word_ranges(text, word)),
                Term::Literal(literal) => ranges.extend(literal.match_ranges(text)),
            }
        }
        ranges.extend(merge_separator_adjacent(text, word_ranges));
        merge_overlapping(ranges)
    }

    /// Byte ranges of `full_text` that best show matches the `preview` hides.
    ///
    /// Selection is cluster-based: collect every term hit in `full_text`, group
    /// nearby hits into clusters, then rank clusters by:
    ///
    /// 1. how many *missing* (not-in-preview) terms they cover,
    /// 2. how many adjacent term pairs they contain (e.g. literal phrase match),
    /// 3. total unique-term coverage,
    /// 4. tighter span,
    /// 5. earlier position.
    ///
    /// This makes the literal phrase `audio generation` win over a far-apart
    /// pair of `audio` + `generation` occurrences in unrelated boilerplate.
    /// Returns `None` when the preview already shows everything there is.
    pub fn hidden_context(&self, full_text: &str, preview: &str) -> Option<Vec<(usize, usize)>> {
        if self.terms.is_empty() {
            return None;
        }

        let mut missing_mask: u64 = 0;
        let mut missing_count = 0u32;
        for (i, term) in self.terms.iter().enumerate() {
            if !term.matches(preview) {
                missing_mask |= 1 << i;
                missing_count += 1;
            }
        }

        let all_hits = self.term_hits(full_text);
        if all_hits.is_empty() {
            return None;
        }

        // If every term is already visible in the preview, only emit context
        // when full_text contains *more* hits than the preview does. We don't
        // try to skip positionally — `preview` is sanitized/truncated and
        // `full_text` is raw, so alignment between the two hit streams is
        // unreliable. Worst case the ranker picks a cluster that overlaps
        // preview content, which is still the best snippet we have.
        if missing_count == 0 && all_hits.len() <= self.term_hits(preview).len() {
            return None;
        }

        select_hidden_context_ranges(full_text, &all_hits, missing_mask, missing_count)
    }

    fn term_hits(&self, text: &str) -> Vec<TermHit> {
        let mut hits = Vec::new();
        for (term_idx, term) in self.terms.iter().enumerate() {
            hits.extend(term.ranges(text).into_iter().map(|(start, end)| TermHit {
                start,
                end,
                term_idx,
            }));
        }
        hits.sort_unstable_by_key(|hit| hit.start);
        hits
    }
}

fn dedupe_terms(terms: impl IntoIterator<Item = Term>) -> Vec<Term> {
    let mut deduped: Vec<Term> = Vec::new();
    for term in terms {
        let (text, is_literal) = term.key();
        let seen = deduped.iter().any(|existing| {
            let (existing_text, existing_is_literal) = existing.key();
            existing_is_literal == is_literal && existing_text.eq_ignore_ascii_case(text)
        });
        if !seen {
            deduped.push(term);
            if deduped.len() == MAX_TERMS {
                break;
            }
        }
    }
    deduped
}

/// One match used to compute term coverage and phrase density per cluster.
#[derive(Clone, Copy, Debug)]
struct TermHit {
    start: usize,
    end: usize,
    term_idx: usize,
}

#[derive(Clone, Debug)]
struct HitCluster {
    start: usize,
    end: usize,
    unique_terms: u64,
    missing_terms: u64,
    adjacent_pairs: u32,
    last_hit_end: usize,
    last_term_idx: usize,
}

impl HitCluster {
    fn span(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    fn unique_count(&self) -> u32 {
        self.unique_terms.count_ones()
    }

    fn missing_count(&self) -> u32 {
        self.missing_terms.count_ones()
    }
}

fn select_hidden_context_ranges(
    full_text: &str,
    hits: &[TermHit],
    missing_mask: u64,
    missing_count: u32,
) -> Option<Vec<(usize, usize)>> {
    let merge_gap_bytes: usize = 50;
    let max_cluster_span_bytes: usize = 200;
    let max_clusters: usize = 3;

    if hits.is_empty() {
        return None;
    }

    let mut clusters: Vec<HitCluster> = Vec::new();
    for &TermHit {
        start,
        end,
        term_idx,
    } in hits
    {
        let term_bit: u64 = 1u64 << term_idx;
        let is_missing = (missing_mask & term_bit) != 0;
        let mut extended = false;

        if let Some(last) = clusters.last_mut() {
            let close_enough = start <= last.end.saturating_add(merge_gap_bytes);
            let new_end = last.end.max(end);
            let new_span = new_end.saturating_sub(last.start);
            if close_enough && new_span <= max_cluster_span_bytes {
                if term_idx != last.last_term_idx && start >= last.last_hit_end {
                    let gap = &full_text[last.last_hit_end..start];
                    if !gap.is_empty() && gap.chars().all(|c| !c.is_alphanumeric()) {
                        last.adjacent_pairs += 1;
                    }
                }

                last.end = new_end;
                last.unique_terms |= term_bit;
                if is_missing {
                    last.missing_terms |= term_bit;
                }
                last.last_hit_end = end;
                last.last_term_idx = term_idx;
                extended = true;
            }
        }

        if !extended {
            clusters.push(HitCluster {
                start,
                end,
                unique_terms: term_bit,
                missing_terms: if is_missing { term_bit } else { 0 },
                adjacent_pairs: 0,
                last_hit_end: end,
                last_term_idx: term_idx,
            });
        }
    }

    if missing_count > 0 {
        clusters.retain(|cluster| cluster.missing_count() > 0);
    }
    if clusters.is_empty() {
        return None;
    }

    clusters.sort_unstable_by(|a, b| {
        b.missing_count()
            .cmp(&a.missing_count())
            .then_with(|| b.adjacent_pairs.cmp(&a.adjacent_pairs))
            .then_with(|| b.unique_count().cmp(&a.unique_count()))
            .then_with(|| a.span().cmp(&b.span()))
            .then_with(|| a.start.cmp(&b.start))
    });

    let mut selected: Vec<HitCluster> = Vec::new();
    let mut covered_missing: u64 = 0;

    for cluster in &clusters {
        if selected.len() >= max_clusters {
            break;
        }
        let new_missing = cluster.missing_terms & !covered_missing;
        if new_missing != 0 {
            covered_missing |= cluster.missing_terms;
            selected.push(cluster.clone());
        }
    }

    for cluster in &clusters {
        if selected.len() >= max_clusters {
            break;
        }
        if !selected
            .iter()
            .any(|selected| selected.start == cluster.start && selected.end == cluster.end)
        {
            selected.push(cluster.clone());
        }
    }

    selected.sort_unstable_by_key(|cluster| cluster.start);
    Some(
        selected
            .into_iter()
            .map(|cluster| (cluster.start, cluster.end))
            .collect(),
    )
}

fn find_first_word_range(text: &str, word: &str) -> Option<(usize, usize)> {
    let mut first = None;
    scan_word(text, word, |range| {
        first = Some(range);
        false
    });
    first
}

/// All non-overlapping, left-word-bounded matches of a normalized `word` in
/// raw `text`, as byte ranges into `text`.
fn find_word_ranges(text: &str, word: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    scan_word(text, word, |range| {
        ranges.push(range);
        true
    });
    ranges
}

/// Streams over `text`, lowercasing each char with full Unicode expansion (so
/// `İ` matches the two chars `normalize_for_search` produces for it), and
/// reports each match to `on_match` until it returns `false`. A word starting
/// alphanumerically must start at a word boundary; a word starting with
/// punctuation (`.rs`) carries its own boundary.
fn scan_word(text: &str, word: &str, mut on_match: impl FnMut((usize, usize)) -> bool) {
    let word_chars: Vec<char> = word.chars().collect();
    let Some(&first_word_char) = word_chars.first() else {
        return;
    };
    let word_starts_alnum = first_word_char.is_alphanumeric();

    let mut prev_is_alnum = false;
    let mut iter = text.char_indices().peekable();

    while let Some(&(byte_start, ch)) = iter.peek() {
        let valid_start = !word_starts_alnum || !prev_is_alnum;
        if valid_start && let Some(end_byte) = match_word_at(&mut iter.clone(), &word_chars) {
            if !on_match((byte_start, end_byte)) {
                return;
            }
            // Skip the consumed chars; the last one decides the boundary for
            // the next candidate.
            let mut last_consumed = ch;
            while let Some(&(pos, consumed)) = iter.peek() {
                if pos >= end_byte {
                    break;
                }
                last_consumed = consumed;
                iter.next();
            }
            prev_is_alnum = last_consumed.is_alphanumeric();
            continue;
        }

        prev_is_alnum = ch.is_alphanumeric();
        iter.next();
    }
}

/// Tries to match `word_chars` starting at the iterator's current position,
/// returning the byte offset just past the last text char consumed.
fn match_word_at(
    iter: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    word_chars: &[char],
) -> Option<usize> {
    let mut remaining = word_chars.iter();
    let mut expected = remaining.next()?;
    for (byte_start, ch) in iter.by_ref() {
        for lowered in ch.to_lowercase() {
            if lowered != *expected {
                return None;
            }
            match remaining.next() {
                Some(next) => expected = next,
                None => return Some(byte_start + ch.len_utf8()),
            }
        }
    }
    None
}

/// Merges word ranges whose gap consists only of `_`, `-`, `/` or spaces, so
/// the underscores in `run_with_loader` highlight as part of the match.
fn merge_separator_adjacent(text: &str, mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.sort_unstable_by_key(|range| range.0);
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if let Some(last) = merged.last_mut() {
            if range.0 <= last.1 {
                last.1 = last.1.max(range.1);
                continue;
            }
            let gap = &text[last.1..range.0];
            if gap.chars().all(|c| matches!(c, ' ' | '_' | '-' | '/')) {
                last.1 = range.1;
                continue;
            }
        }
        merged.push(range);
    }
    merged
}

/// Sorts ranges and merges any that overlap, dropping empty ones.
pub fn merge_overlapping(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.sort_unstable_by_key(|range| range.0);
    let mut merged = Vec::<(usize, usize)>::new();
    for (start, end) in ranges {
        if start >= end {
            continue;
        }
        if let Some(last) = merged.last_mut()
            && start <= last.1
        {
            last.1 = last.1.max(end);
            continue;
        }
        merged.push((start, end));
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::test_fixtures::conversation_with_text;

    fn matched<'a>(text: &'a str, query: &str) -> Vec<&'a str> {
        QueryMatcher::from_query(query)
            .ranges(text)
            .into_iter()
            .map(|(start, end)| &text[start..end])
            .collect()
    }

    fn context<'a>(full_text: &'a str, preview: &str, query: &str) -> Option<Vec<&'a str>> {
        QueryMatcher::from_query(query)
            .hidden_context(full_text, preview)
            .map(|ranges| {
                ranges
                    .into_iter()
                    .map(|(start, end)| &full_text[start..end])
                    .collect()
            })
    }

    #[test]
    fn word_requires_left_boundary_but_allows_prefix() {
        assert_eq!(matched("deployment redeploy", "deploy"), vec!["deploy"]);
        assert!(matched("redeploy", "deploy").is_empty());
    }

    #[test]
    fn punctuation_led_word_matches_inside_token() {
        assert_eq!(matched("see lexical.rs now", ".rs"), vec![".rs"]);
    }

    #[test]
    fn words_are_case_insensitive_with_full_lowercase_expansion() {
        assert_eq!(matched("Hello WORLD", "hello world"), vec!["Hello WORLD"]);
        // `İ` lowercases to `i` + U+0307, which is how the query side sees it.
        let text = "İstanbul";
        let query = crate::text_match::normalize_for_search("İstanbul");
        let ranges = find_word_ranges(text, &query);
        assert_eq!(ranges, vec![(0, text.len())]);
    }

    #[test]
    fn separator_adjacent_words_merge_into_one_range() {
        assert_eq!(
            matched("call run_with_loader here", "run with loader"),
            vec!["run_with_loader"]
        );
        assert_eq!(matched("foo-bar", "foo bar"), vec!["foo-bar"]);
    }

    #[test]
    fn noncontiguous_words_stay_separate() {
        assert_eq!(
            matched("alpha then beta", "alpha beta"),
            vec!["alpha", "beta"]
        );
    }

    #[test]
    fn literals_match_exactly_with_smart_case() {
        assert_eq!(
            matched(
                "DEPLOYMENT_TOKEN and deployment_token",
                "\"DEPLOYMENT_TOKEN\""
            ),
            vec!["DEPLOYMENT_TOKEN"]
        );
        assert_eq!(
            matched(
                "Deployment_Token and deployment_token",
                "\"deployment_token\""
            ),
            vec!["Deployment_Token", "deployment_token"]
        );
    }

    #[test]
    fn unquoted_identifier_is_a_literal_not_words() {
        assert_eq!(
            matched("deployment token vs deployment_token", "deployment_token"),
            vec!["deployment_token"]
        );
    }

    #[test]
    fn overlapping_literal_and_word_ranges_merge() {
        assert_eq!(
            matched("audio_generation", "audio \"audio_generation\""),
            vec!["audio_generation"]
        );
    }

    #[test]
    fn empty_query_matches_nothing() {
        let matcher = QueryMatcher::from_query("   ");
        assert!(matcher.is_empty());
        assert!(matcher.ranges("anything").is_empty());
        assert!(!matcher.matches("anything"));
        assert!(matcher.hidden_context("anything", "").is_none());
    }

    #[test]
    fn literals_missing_from_preview_requests_context() {
        let matcher = QueryMatcher::from_query("alpha \"exact_literal\"");
        assert!(matcher.literals_missing_from("alpha only"));
        assert!(!matcher.literals_missing_from("alpha exact_literal"));
        assert!(!QueryMatcher::from_query("alpha").literals_missing_from(""));
    }

    #[test]
    fn literals_only_drops_words() {
        let matcher = QueryMatcher::from_query("alpha \"Beta\" gamma_delta").literals_only();
        assert!(matcher.has_literals());
        assert!(matcher.ranges("alpha").is_empty());
        assert_eq!(matcher.ranges("alpha Beta gamma_delta").len(), 2);
    }

    #[test]
    fn dedupes_terms_case_insensitively_per_kind() {
        let matcher = QueryMatcher::from_query("alpha Alpha \"alpha\" \"ALPHA\"");
        assert_eq!(matcher.terms.len(), 2);
    }

    #[test]
    fn context_none_when_all_visible() {
        assert!(context("hello world", "hello world", "hello").is_none());
    }

    #[test]
    fn context_finds_one_hidden_match() {
        let full = "some preview text and then much later the hidden keyword appears here";
        let snippets = context(full, "some preview text", "keyword").unwrap();
        assert_eq!(snippets, vec!["keyword"]);
    }

    #[test]
    fn context_prioritizes_missing_terms() {
        let full = "alpha is visible here. far away beta appears with more text. and beta again";
        let snippets = context(full, "alpha is visible", "alpha beta").unwrap();
        assert!(snippets.iter().all(|snippet| snippet.contains("beta")));
    }

    #[test]
    fn context_prefers_adjacent_phrase_over_distant_terms() {
        // "audio" and "generation" each appear early in unrelated boilerplate,
        // and the literal phrase "audio generation" appears much later. The
        // phrase cluster must be selected alongside the early independent hits.
        let mut full = String::new();
        full.push_str("Card generation is supported. -field:Audio is a filter. ");
        full.push_str(&"junk ".repeat(20));
        full.push_str("First-class audio generation (OpenAI TTS) and image support is missing. ");
        full.push_str(&"junk ".repeat(20));
        let snippets = context(&full, "Some unrelated preview line", "audio generation").unwrap();
        assert!(snippets.contains(&"audio generation"), "{snippets:?}");
    }

    #[test]
    fn context_distant_terms_do_not_count_as_adjacent() {
        // Two clusters with the same unique_count, but only one has the
        // terms actually adjacent. The adjacent one must be selected.
        let mut full = String::new();
        full.push_str("alpha aaaa bbbb cccc dddd beta ");
        full.push_str(&"x ".repeat(40));
        full.push_str("alpha beta together here");
        let snippets = context(&full, "boring preview line", "alpha beta").unwrap();
        assert!(snippets.contains(&"alpha beta"), "{snippets:?}");
    }

    #[test]
    fn context_skips_clusters_covering_only_visible_terms() {
        let filler = "z ".repeat(80);
        let full = format!("alpha near start {filler} beta hides here");
        let snippets = context(&full, "alpha near start", "alpha beta").unwrap();
        assert_eq!(snippets, vec!["beta"]);
    }

    #[test]
    fn context_underscore_phrase_is_detected() {
        let full = format!("{} run_with_loader called", "y ".repeat(80));
        let snippets = context(&full, "visible preview", "run with loader").unwrap();
        assert!(
            snippets
                .iter()
                .any(|snippet| snippet.contains("run_with_loader"))
        );
    }

    #[test]
    fn context_phrase_inside_markdown_bold() {
        let full = format!("{} **audio generation** rocks", "w ".repeat(80));
        let snippets = context(&full, "visible preview", "audio generation").unwrap();
        assert!(
            snippets
                .iter()
                .any(|snippet| snippet.contains("audio generation"))
        );
    }

    #[test]
    fn evidence_for_unquoted_body_match() {
        let parsed = ParsedQuery::parse("deepgram");
        let conversation = conversation_with_text(
            "unrelated preview",
            "unrelated preview followed by hidden deepgram evidence",
        );
        let evidence = build_lexical_evidence(&conversation, &parsed).unwrap();
        assert_eq!(evidence.context_ranges.len(), 1);
        let (start, end) = evidence.context_ranges[0];
        assert!(conversation.full_text[start..end].contains("deepgram"));
    }

    #[test]
    fn evidence_for_quoted_literal_preserves_punctuation() {
        let parsed = ParsedQuery::parse("\"audio_generation\"");
        let conversation = conversation_with_text(
            "audio generation normalized preview",
            "audio generation normalized preview and exact audio_generation evidence",
        );
        let evidence = build_lexical_evidence(&conversation, &parsed).unwrap();
        let (start, end) = evidence.context_ranges[0];
        assert_eq!(&conversation.full_text[start..end], "audio_generation");
    }

    #[test]
    fn evidence_for_mixed_query_surfaces_words_and_literals() {
        let parsed = ParsedQuery::parse("hidden_unquoted \"exact_literal\"");
        let full_text = format!("hidden_unquoted {} exact_literal", "x ".repeat(120));
        let conversation = conversation_with_text("visible preview", &full_text);
        let evidence = build_lexical_evidence(&conversation, &parsed).unwrap();
        let snippets = evidence
            .context_ranges
            .iter()
            .map(|(start, end)| &conversation.full_text[*start..*end])
            .collect::<Vec<_>>();
        assert!(snippets.iter().any(|s| s.contains("hidden_unquoted")));
        assert!(snippets.iter().any(|s| s.contains("exact_literal")));
    }

    #[test]
    fn merge_overlapping_drops_empty_and_joins() {
        assert_eq!(
            merge_overlapping(vec![(5, 9), (0, 3), (2, 4), (7, 7)]),
            vec![(0, 4), (5, 9)]
        );
    }
}
