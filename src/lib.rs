//! # claude-history
//!
//! A library for searching and browsing Claude Code conversation history.
//!
//! This library provides functionality to:
//! - Load and parse Claude Code conversation files (JSONL format)
//! - Search conversations with various filters
//! - Display conversations in the terminal
//!
//! ## Modules
//!
//! - [`time_filter`] - Time-based filtering with duration and date range support
//! - [`path_filter`] - Path-based filtering with regex include/exclude patterns
//! - [`query`] - Boolean query parsing and evaluation for content search
//!
//! ## Example
//!
//! ```rust,no_run
//! use claude_history::time_filter::TimeFilter;
//! use claude_history::path_filter::PathFilter;
//! use claude_history::query::{parse_query, evaluate};
//! use chrono::{Duration, Local};
//! use std::path::Path;
//!
//! // Create a time filter for conversations from the last 2 days
//! let time_filter = TimeFilter::from_since("2d").unwrap();
//!
//! // Check if a timestamp matches
//! let recent = Local::now() - Duration::hours(6);
//! assert!(time_filter.matches(recent));
//!
//! // Create a path filter to include only work projects
//! let path_filter = PathFilter::new()
//!     .with_include("/work/")
//!     .unwrap()
//!     .with_exclude("test")
//!     .unwrap();
//!
//! assert!(path_filter.matches(Path::new("/home/user/work/myapp")));
//!
//! // Create a boolean query for content search
//! let query = parse_query("rust && !deprecated").unwrap();
//! assert!(evaluate(&query, "new rust api"));
//! assert!(!evaluate(&query, "deprecated rust code"));
//! ```

pub mod path_filter;
pub mod query;
pub mod time_filter;
