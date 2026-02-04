//! Recursive descent parser for boolean queries.
//!
//! Grammar:
//! ```text
//! expr      = or_expr
//! or_expr   = and_expr ("||" and_expr)*
//! and_expr  = not_expr ("&&" not_expr)*
//! not_expr  = "!" primary | primary
//! primary   = "(" expr ")" | quoted_string | word
//! ```

use std::fmt;

/// Error type for query parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryError {
    /// Empty query string
    EmptyQuery,
    /// Unexpected end of input
    UnexpectedEnd,
    /// Unexpected character at position
    UnexpectedChar(usize, char),
    /// Unclosed parenthesis
    UnclosedParen,
    /// Unclosed quote
    UnclosedQuote,
    /// Empty term (e.g., "" or just whitespace)
    EmptyTerm,
    /// Missing operand for operator
    MissingOperand(String),
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QueryError::EmptyQuery => write!(f, "query cannot be empty"),
            QueryError::UnexpectedEnd => write!(f, "unexpected end of query"),
            QueryError::UnexpectedChar(pos, c) => {
                write!(f, "unexpected character '{}' at position {}", c, pos)
            }
            QueryError::UnclosedParen => write!(f, "unclosed parenthesis"),
            QueryError::UnclosedQuote => write!(f, "unclosed quote"),
            QueryError::EmptyTerm => write!(f, "empty search term"),
            QueryError::MissingOperand(op) => write!(f, "missing operand for '{}'", op),
        }
    }
}

impl std::error::Error for QueryError {}

/// Abstract syntax tree for boolean queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryExpr {
    /// A search term (word or phrase)
    Term(String),
    /// Logical AND of two expressions
    And(Box<QueryExpr>, Box<QueryExpr>),
    /// Logical OR of two expressions
    Or(Box<QueryExpr>, Box<QueryExpr>),
    /// Logical NOT of an expression
    Not(Box<QueryExpr>),
}

impl QueryExpr {
    /// Create an AND expression.
    pub fn and(left: QueryExpr, right: QueryExpr) -> Self {
        QueryExpr::And(Box::new(left), Box::new(right))
    }

    /// Create an OR expression.
    pub fn or(left: QueryExpr, right: QueryExpr) -> Self {
        QueryExpr::Or(Box::new(left), Box::new(right))
    }

    /// Create a NOT expression.
    pub fn not(expr: QueryExpr) -> Self {
        QueryExpr::Not(Box::new(expr))
    }

    /// Create a Term expression.
    pub fn term(s: &str) -> Self {
        QueryExpr::Term(s.to_string())
    }
}

/// Parser state for recursive descent parsing.
struct Parser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn remaining(&self) -> &'a str {
        &self.input[self.pos..]
    }

    fn peek_char(&self) -> Option<char> {
        self.remaining().chars().next()
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek_char() {
            if c.is_whitespace() {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }

    fn expect_char(&mut self, expected: char) -> Result<(), QueryError> {
        self.skip_whitespace();
        match self.peek_char() {
            Some(c) if c == expected => {
                self.pos += c.len_utf8();
                Ok(())
            }
            Some(c) => Err(QueryError::UnexpectedChar(self.pos, c)),
            None => Err(QueryError::UnexpectedEnd),
        }
    }

    fn try_consume(&mut self, s: &str) -> bool {
        self.skip_whitespace();
        if self.remaining().starts_with(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    fn at_end(&self) -> bool {
        self.skip_whitespace_peek();
        self.pos >= self.input.len()
    }

    fn skip_whitespace_peek(&self) -> usize {
        let mut pos = self.pos;
        for c in self.input[pos..].chars() {
            if c.is_whitespace() {
                pos += c.len_utf8();
            } else {
                break;
            }
        }
        pos
    }

    /// Parse the complete expression.
    fn parse_expr(&mut self) -> Result<QueryExpr, QueryError> {
        self.parse_or_expr()
    }

    /// Parse OR expressions: and_expr ("||" and_expr)*
    fn parse_or_expr(&mut self) -> Result<QueryExpr, QueryError> {
        let mut left = self.parse_and_expr()?;

        while self.try_consume("||") {
            let right = self.parse_and_expr()?;
            left = QueryExpr::or(left, right);
        }

        Ok(left)
    }

    /// Parse AND expressions: not_expr ("&&" not_expr)*
    fn parse_and_expr(&mut self) -> Result<QueryExpr, QueryError> {
        let mut left = self.parse_not_expr()?;

        while self.try_consume("&&") {
            let right = self.parse_not_expr()?;
            left = QueryExpr::and(left, right);
        }

        Ok(left)
    }

    /// Parse NOT expressions: "!" primary | primary
    fn parse_not_expr(&mut self) -> Result<QueryExpr, QueryError> {
        self.skip_whitespace();

        if self.try_consume("!") {
            let expr = self.parse_not_expr()?;
            Ok(QueryExpr::not(expr))
        } else {
            self.parse_primary()
        }
    }

    /// Parse primary expressions: "(" expr ")" | quoted_string | word
    fn parse_primary(&mut self) -> Result<QueryExpr, QueryError> {
        self.skip_whitespace();

        match self.peek_char() {
            None => Err(QueryError::UnexpectedEnd),
            Some('(') => {
                self.pos += 1;
                let expr = self.parse_expr()?;
                self.expect_char(')')?;
                Ok(expr)
            }
            Some('"') => self.parse_quoted_string(),
            Some(c) if is_word_char(c) => self.parse_word(),
            Some(c) => Err(QueryError::UnexpectedChar(self.pos, c)),
        }
    }

    /// Parse a quoted string: "..."
    fn parse_quoted_string(&mut self) -> Result<QueryExpr, QueryError> {
        self.pos += 1; // skip opening quote

        let start = self.pos;

        for (i, c) in self.input[start..].char_indices() {
            if c == '"' {
                let term = &self.input[start..start + i];
                self.pos = start + i + 1; // skip closing quote

                if term.is_empty() {
                    return Err(QueryError::EmptyTerm);
                }

                return Ok(QueryExpr::Term(term.to_string()));
            }
        }

        Err(QueryError::UnclosedQuote)
    }

    /// Parse a word (unquoted term).
    fn parse_word(&mut self) -> Result<QueryExpr, QueryError> {
        let start = self.pos;

        while let Some(c) = self.peek_char() {
            if is_word_char(c) {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }

        let term = &self.input[start..self.pos];
        if term.is_empty() {
            return Err(QueryError::EmptyTerm);
        }

        Ok(QueryExpr::Term(term.to_string()))
    }
}

/// Check if a character is valid in an unquoted word.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/'
}

/// Parse a query string into an expression tree.
///
/// # Arguments
///
/// * `input` - The query string to parse
///
/// # Returns
///
/// A `QueryExpr` tree on success, or a `QueryError` on failure.
///
/// # Example
///
/// ```rust
/// use claude_history::query::parse_query;
///
/// let query = parse_query("rust && !java").unwrap();
/// ```
pub fn parse_query(input: &str) -> Result<QueryExpr, QueryError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(QueryError::EmptyQuery);
    }

    let mut parser = Parser::new(trimmed);
    let expr = parser.parse_expr()?;

    // Ensure we consumed all input
    parser.skip_whitespace();
    if !parser.at_end() {
        if let Some(c) = parser.peek_char() {
            return Err(QueryError::UnexpectedChar(parser.pos, c));
        }
    }

    Ok(expr)
}

/// Evaluate a query expression against text content.
///
/// Matching is case-insensitive. Terms match if the text contains
/// the term as a substring.
///
/// # Arguments
///
/// * `expr` - The query expression to evaluate
/// * `text` - The text content to search
///
/// # Returns
///
/// `true` if the query matches the text, `false` otherwise.
///
/// # Example
///
/// ```rust
/// use claude_history::query::{parse_query, evaluate};
///
/// let query = parse_query("rust && programming").unwrap();
/// assert!(evaluate(&query, "Rust programming is great"));
/// assert!(!evaluate(&query, "Rust is a language"));
/// ```
pub fn evaluate(expr: &QueryExpr, text: &str) -> bool {
    let text_lower = text.to_lowercase();

    match expr {
        QueryExpr::Term(term) => {
            let term_lower = term.to_lowercase();
            text_lower.contains(&term_lower)
        }
        QueryExpr::And(left, right) => evaluate(left, text) && evaluate(right, text),
        QueryExpr::Or(left, right) => evaluate(left, text) || evaluate(right, text),
        QueryExpr::Not(inner) => !evaluate(inner, text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== Parser tests ====================

    #[test]
    fn parse_single_word() {
        let expr = parse_query("rust").unwrap();
        assert_eq!(expr, QueryExpr::term("rust"));
    }

    #[test]
    fn parse_quoted_phrase() {
        let expr = parse_query(r#""hello world""#).unwrap();
        assert_eq!(expr, QueryExpr::term("hello world"));
    }

    #[test]
    fn parse_and_expression() {
        let expr = parse_query("rust && python").unwrap();
        assert_eq!(
            expr,
            QueryExpr::and(QueryExpr::term("rust"), QueryExpr::term("python"))
        );
    }

    #[test]
    fn parse_or_expression() {
        let expr = parse_query("rust || python").unwrap();
        assert_eq!(
            expr,
            QueryExpr::or(QueryExpr::term("rust"), QueryExpr::term("python"))
        );
    }

    #[test]
    fn parse_not_expression() {
        let expr = parse_query("!java").unwrap();
        assert_eq!(expr, QueryExpr::not(QueryExpr::term("java")));
    }

    #[test]
    fn parse_complex_expression() {
        // (rust || go) && !java
        let expr = parse_query("(rust || go) && !java").unwrap();
        assert_eq!(
            expr,
            QueryExpr::and(
                QueryExpr::or(QueryExpr::term("rust"), QueryExpr::term("go")),
                QueryExpr::not(QueryExpr::term("java"))
            )
        );
    }

    #[test]
    fn parse_chained_and() {
        let expr = parse_query("a && b && c").unwrap();
        // Left-associative: ((a && b) && c)
        assert_eq!(
            expr,
            QueryExpr::and(
                QueryExpr::and(QueryExpr::term("a"), QueryExpr::term("b")),
                QueryExpr::term("c")
            )
        );
    }

    #[test]
    fn parse_chained_or() {
        let expr = parse_query("a || b || c").unwrap();
        // Left-associative: ((a || b) || c)
        assert_eq!(
            expr,
            QueryExpr::or(
                QueryExpr::or(QueryExpr::term("a"), QueryExpr::term("b")),
                QueryExpr::term("c")
            )
        );
    }

    #[test]
    fn parse_operator_precedence() {
        // a || b && c should be a || (b && c) because && binds tighter
        let expr = parse_query("a || b && c").unwrap();
        assert_eq!(
            expr,
            QueryExpr::or(
                QueryExpr::term("a"),
                QueryExpr::and(QueryExpr::term("b"), QueryExpr::term("c"))
            )
        );
    }

    #[test]
    fn parse_double_not() {
        let expr = parse_query("!!foo").unwrap();
        assert_eq!(expr, QueryExpr::not(QueryExpr::not(QueryExpr::term("foo"))));
    }

    #[test]
    fn parse_with_extra_whitespace() {
        let expr = parse_query("  rust   &&   python  ").unwrap();
        assert_eq!(
            expr,
            QueryExpr::and(QueryExpr::term("rust"), QueryExpr::term("python"))
        );
    }

    #[test]
    fn parse_word_with_special_chars() {
        let expr = parse_query("my-project_name.rs").unwrap();
        assert_eq!(expr, QueryExpr::term("my-project_name.rs"));
    }

    #[test]
    fn parse_path_like_term() {
        let expr = parse_query("/home/user/project").unwrap();
        assert_eq!(expr, QueryExpr::term("/home/user/project"));
    }

    // ==================== Parser error tests ====================

    #[test]
    fn parse_empty_query_error() {
        assert!(matches!(parse_query(""), Err(QueryError::EmptyQuery)));
        assert!(matches!(parse_query("   "), Err(QueryError::EmptyQuery)));
    }

    #[test]
    fn parse_unclosed_paren_error() {
        assert!(matches!(
            parse_query("(foo && bar"),
            Err(QueryError::UnexpectedEnd)
        ));
    }

    #[test]
    fn parse_unclosed_quote_error() {
        assert!(matches!(
            parse_query(r#""hello world"#),
            Err(QueryError::UnclosedQuote)
        ));
    }

    #[test]
    fn parse_empty_quoted_term_error() {
        assert!(matches!(parse_query(r#""""#), Err(QueryError::EmptyTerm)));
    }

    #[test]
    fn parse_unexpected_char_error() {
        // Unexpected closing paren
        assert!(matches!(
            parse_query("foo)"),
            Err(QueryError::UnexpectedChar(_, ')'))
        ));
    }

    // ==================== Evaluation tests ====================

    #[test]
    fn evaluate_term_match() {
        let expr = parse_query("rust").unwrap();
        assert!(evaluate(&expr, "I love rust programming"));
        assert!(!evaluate(&expr, "I love python programming"));
    }

    #[test]
    fn evaluate_term_case_insensitive() {
        let expr = parse_query("RUST").unwrap();
        assert!(evaluate(&expr, "rust is great"));
        assert!(evaluate(&expr, "Rust is great"));
        assert!(evaluate(&expr, "RUST is great"));
    }

    #[test]
    fn evaluate_quoted_phrase() {
        let expr = parse_query(r#""hello world""#).unwrap();
        assert!(evaluate(&expr, "say hello world today"));
        assert!(!evaluate(&expr, "hello there world"));
    }

    #[test]
    fn evaluate_and() {
        let expr = parse_query("rust && programming").unwrap();
        assert!(evaluate(&expr, "rust programming is fun"));
        assert!(!evaluate(&expr, "rust is a language"));
        assert!(!evaluate(&expr, "python programming is fun"));
    }

    #[test]
    fn evaluate_or() {
        let expr = parse_query("rust || python").unwrap();
        assert!(evaluate(&expr, "I use rust"));
        assert!(evaluate(&expr, "I use python"));
        assert!(evaluate(&expr, "I use rust and python"));
        assert!(!evaluate(&expr, "I use javascript"));
    }

    #[test]
    fn evaluate_not() {
        let expr = parse_query("!java").unwrap();
        assert!(evaluate(&expr, "I use rust"));
        assert!(!evaluate(&expr, "I use java"));
    }

    #[test]
    fn evaluate_and_not() {
        let expr = parse_query("programming && !javascript").unwrap();
        assert!(evaluate(&expr, "rust programming"));
        assert!(!evaluate(&expr, "javascript programming"));
        // "just rust" doesn't contain "programming"
        assert!(!evaluate(&expr, "just rust"));
    }

    #[test]
    fn evaluate_complex_query() {
        // (rust || go) && !java
        let expr = parse_query("(rust || go) && !java").unwrap();
        assert!(evaluate(&expr, "rust is great"));
        assert!(evaluate(&expr, "go is great"));
        assert!(!evaluate(&expr, "java is great"));
        assert!(!evaluate(&expr, "rust and java together"));
        assert!(!evaluate(&expr, "go and java together"));
    }

    #[test]
    fn evaluate_double_not() {
        let expr = parse_query("!!rust").unwrap();
        assert!(evaluate(&expr, "rust is great"));
        assert!(!evaluate(&expr, "python is great"));
    }

    #[test]
    fn evaluate_nested_groups() {
        // ((foo && bar) || (baz && qux))
        let expr = parse_query("((foo && bar) || (baz && qux))").unwrap();
        assert!(evaluate(&expr, "foo bar here"));
        assert!(evaluate(&expr, "baz qux there"));
        // Has foo but not bar, has baz but not qux
        assert!(!evaluate(&expr, "foo baz"));
        // Has bar but not foo, has qux but not baz
        assert!(!evaluate(&expr, "bar qux"));
    }

    // ==================== Real-world query tests ====================

    #[test]
    fn evaluate_file_search_pattern() {
        let expr = parse_query(r#""error" && (rust || python) && !test"#).unwrap();
        assert!(evaluate(&expr, "error handling in rust code"));
        assert!(evaluate(&expr, "python error logging"));
        assert!(!evaluate(&expr, "rust test error"));
        assert!(!evaluate(&expr, "error in javascript"));
    }

    #[test]
    fn evaluate_project_filter() {
        let expr = parse_query("api && !deprecated && !legacy").unwrap();
        assert!(evaluate(&expr, "new api endpoint"));
        assert!(!evaluate(&expr, "deprecated api endpoint"));
        assert!(!evaluate(&expr, "legacy api code"));
    }
}
