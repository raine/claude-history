//! Boolean query parsing and evaluation for conversation search.
//!
//! This module provides a simple boolean query language for searching
//! conversation content with AND, OR, and NOT operators.
//!
//! # Syntax
//!
//! The query language supports:
//! - **Terms**: Words or quoted phrases (`foo`, `"hello world"`)
//! - **AND**: `&&` operator (`foo && bar`)
//! - **OR**: `||` operator (`foo || bar`)
//! - **NOT**: `!` operator (`!foo`)
//! - **Grouping**: Parentheses for precedence (`(foo || bar) && !baz`)
//!
//! # Operator Precedence
//!
//! From lowest to highest:
//! 1. `||` (OR)
//! 2. `&&` (AND)
//! 3. `!` (NOT)
//!
//! # Examples
//!
//! ```rust
//! use claude_history::query::{parse_query, evaluate};
//!
//! // Simple term search
//! let query = parse_query("rust").unwrap();
//! assert!(evaluate(&query, "I love rust programming"));
//! assert!(!evaluate(&query, "I love python programming"));
//!
//! // AND query - both terms must match
//! let query = parse_query("rust && programming").unwrap();
//! assert!(evaluate(&query, "rust programming is fun"));
//! assert!(!evaluate(&query, "rust is a language"));
//!
//! // OR query - either term matches
//! let query = parse_query("rust || python").unwrap();
//! assert!(evaluate(&query, "I use rust"));
//! assert!(evaluate(&query, "I use python"));
//! assert!(!evaluate(&query, "I use javascript"));
//!
//! // NOT query - term must not match
//! let query = parse_query("programming && !javascript").unwrap();
//! assert!(evaluate(&query, "rust programming"));
//! assert!(!evaluate(&query, "javascript programming"));
//!
//! // Quoted phrase - exact phrase match
//! let query = parse_query(r#""hello world""#).unwrap();
//! assert!(evaluate(&query, "say hello world today"));
//! assert!(!evaluate(&query, "hello there world"));
//!
//! // Complex query with grouping
//! let query = parse_query("(rust || go) && !java").unwrap();
//! assert!(evaluate(&query, "rust is great"));
//! assert!(evaluate(&query, "go is great"));
//! assert!(!evaluate(&query, "java is great"));
//! assert!(!evaluate(&query, "rust and java together"));
//! ```

mod parser;

pub use parser::{evaluate, parse_query, QueryError, QueryExpr};
