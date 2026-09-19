use crate::search::literal::Literal;
use crate::text_match::normalize_for_search;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedQuery {
    raw: String,
    unquoted: String,
    literals: Vec<Literal>,
}

impl ParsedQuery {
    pub fn parse(query: &str) -> Self {
        let mut unquoted = String::new();
        let mut literals = Vec::new();
        let mut literal = String::new();
        let mut in_quote = false;

        for ch in query.chars() {
            if ch == '"' {
                if in_quote {
                    if !literal.trim().is_empty() {
                        literals.push(Literal::new(literal.clone()));
                    }
                    literal.clear();
                    in_quote = false;
                } else {
                    in_quote = true;
                }
            } else if in_quote {
                literal.push(ch);
            } else {
                unquoted.push(ch);
            }
        }

        if in_quote && !literal.trim().is_empty() {
            literals.push(Literal::new(literal));
        }

        Self {
            raw: query.to_string(),
            unquoted: unquoted.trim().to_string(),
            literals,
        }
    }

    /// The whole query as one exact literal, as if the caller had quoted it:
    /// what exact mode matches for an unquoted query. Embedded quotes are
    /// dropped; `raw` stays the text as typed.
    pub fn exact_phrase(query: &str) -> Self {
        let phrase = query.replace('"', "");
        let literals = if phrase.trim().is_empty() {
            Vec::new()
        } else {
            vec![Literal::new(phrase)]
        };
        Self {
            raw: query.to_string(),
            unquoted: String::new(),
            literals,
        }
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }

    pub fn unquoted(&self) -> &str {
        &self.unquoted
    }

    pub fn lexical_text(&self) -> &str {
        if self.is_effectively_empty() {
            ""
        } else if self.unquoted.is_empty() {
            self.raw.trim()
        } else {
            &self.unquoted
        }
    }

    pub fn semantic_text(&self) -> &str {
        &self.unquoted
    }

    pub fn is_effectively_empty(&self) -> bool {
        self.literals.is_empty() && self.unquoted.split_whitespace().next().is_none()
    }

    pub fn literals(&self) -> &[Literal] {
        &self.literals
    }

    /// Unquoted words as the lexical index sees them: normalized and split,
    /// with identifier-shaped terms (containing `_`) left out because they are
    /// matched exactly via [`Self::identifier_literals`].
    pub fn words(&self) -> Vec<String> {
        let plain = self
            .unquoted
            .split_whitespace()
            .filter(|term| !term.contains('_'))
            .collect::<Vec<_>>()
            .join(" ");
        normalize_for_search(&plain)
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    /// Unquoted terms containing `_` are promoted to exact literals so
    /// `api_key` does not match `api key`.
    pub fn identifier_literals(&self) -> Vec<Literal> {
        self.unquoted
            .split_whitespace()
            .filter(|term| term.contains('_'))
            .map(|term| Literal::new(term.to_string()))
            .collect()
    }

    /// Quoted literals followed by promoted identifier literals: every exact
    /// filter a conversation must satisfy.
    pub fn all_literals(&self) -> Vec<Literal> {
        self.literals
            .iter()
            .cloned()
            .chain(self.identifier_literals())
            .collect()
    }

    pub fn is_quoted_only(&self) -> bool {
        !self.literals.is_empty() && self.unquoted.split_whitespace().next().is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::literal::CaseMode;

    #[test]
    fn parses_quoted_phrase() {
        let parsed = ParsedQuery::parse("\"exact phrase\"");
        assert_eq!(parsed.unquoted(), "");
        assert_eq!(parsed.lexical_text(), "\"exact phrase\"");
        assert_eq!(parsed.semantic_text(), "");
        assert!(parsed.is_quoted_only());
        assert_eq!(parsed.literals()[0].text(), "exact phrase");
        assert_eq!(parsed.literals()[0].case_mode(), CaseMode::Insensitive);
    }

    #[test]
    fn parses_mixed_text_and_literals() {
        let parsed = ParsedQuery::parse("alpha \"Beta Gamma\" delta");
        assert_eq!(parsed.unquoted(), "alpha  delta");
        assert_eq!(parsed.lexical_text(), "alpha  delta");
        assert_eq!(parsed.semantic_text(), "alpha  delta");
        assert!(!parsed.is_quoted_only());
        assert_eq!(parsed.literals()[0].text(), "Beta Gamma");
        assert_eq!(parsed.literals()[0].case_mode(), CaseMode::Sensitive);
    }

    #[test]
    fn parses_trailing_open_quote_as_literal() {
        let parsed = ParsedQuery::parse("alpha \"open phrase");
        assert_eq!(parsed.unquoted(), "alpha");
        assert_eq!(parsed.literals()[0].text(), "open phrase");
    }

    #[test]
    fn drops_empty_quotes() {
        let parsed = ParsedQuery::parse("\"\"");
        assert!(parsed.literals().is_empty());
        assert!(!parsed.is_quoted_only());
        assert!(parsed.is_effectively_empty());
        assert_eq!(parsed.lexical_text(), "");
    }

    #[test]
    fn drops_whitespace_only_quotes() {
        let parsed = ParsedQuery::parse("\"  \t \"");
        assert!(parsed.literals().is_empty());
        assert!(!parsed.is_quoted_only());
        assert!(parsed.is_effectively_empty());
        assert_eq!(parsed.lexical_text(), "");
    }

    #[test]
    fn drops_trailing_empty_open_quote() {
        let parsed = ParsedQuery::parse("alpha \"");
        assert_eq!(parsed.unquoted(), "alpha");
        assert!(parsed.literals().is_empty());
    }

    #[test]
    fn parses_quoted_uuid_as_literal() {
        let parsed = ParsedQuery::parse("\"e7d318b1-4274-4ee2-a341-e94893b5df49\"");
        assert_eq!(
            parsed.literals()[0].text(),
            "e7d318b1-4274-4ee2-a341-e94893b5df49"
        );
        assert_eq!(parsed.semantic_text(), "");
    }

    #[test]
    fn splits_words_from_identifier_literals() {
        let parsed = ParsedQuery::parse("Audio_Generation deploy-Token \"Exact\"");
        assert_eq!(parsed.words(), vec!["deploy", "token"]);
        let identifiers = parsed.identifier_literals();
        assert_eq!(identifiers.len(), 1);
        assert_eq!(identifiers[0].text(), "Audio_Generation");
        assert_eq!(identifiers[0].case_mode(), CaseMode::Sensitive);
        let all = parsed.all_literals();
        assert_eq!(all[0].text(), "Exact");
        assert_eq!(all[1].text(), "Audio_Generation");
    }

    #[test]
    fn exact_phrase_matches_quoting_the_whole_query() {
        for query in [
            "cache warming",
            "cache \"Quoted\" tail",
            " padded ",
            "\"\"",
            "   ",
        ] {
            let quoted = ParsedQuery::parse(&format!("\"{}\"", query.replace('"', "")));
            let phrase = ParsedQuery::exact_phrase(query);
            assert_eq!(phrase.literals(), quoted.literals(), "{query:?}");
            assert_eq!(phrase.unquoted(), "");
            assert_eq!(
                phrase.is_quoted_only(),
                quoted.is_quoted_only(),
                "{query:?}"
            );
            assert_eq!(phrase.raw(), query);
        }
        assert_eq!(
            ParsedQuery::exact_phrase("cache \"Quoted\" tail").literals()[0].case_mode(),
            CaseMode::Sensitive
        );
    }

    #[test]
    fn assigns_smart_case_per_literal() {
        let parsed = ParsedQuery::parse("\"lower phrase\" \"Upper Phrase\"");
        assert_eq!(parsed.literals()[0].case_mode(), CaseMode::Insensitive);
        assert_eq!(parsed.literals()[1].case_mode(), CaseMode::Sensitive);
    }
}
