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
//!
//! ## Example
//!
//! ```rust,no_run
//! use claude_history::time_filter::TimeFilter;
//! use chrono::{Duration, Local};
//!
//! // Create a filter for conversations from the last 2 days
//! let filter = TimeFilter::from_since("2d").unwrap();
//!
//! // Check if a timestamp matches
//! let recent = Local::now() - Duration::hours(6);
//! assert!(filter.matches(recent));
//! ```

pub mod time_filter;
