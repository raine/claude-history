//! Path-based filtering for conversation project directories.
//!
//! This module provides filtering of conversations based on their project
//! directory paths using regex patterns for include/exclude matching.
//!
//! # Overview
//!
//! Path filtering allows users to:
//! - Include only conversations from specific project paths
//! - Exclude conversations from certain paths (e.g., test directories)
//! - Combine multiple include/exclude patterns
//!
//! # Matching Logic
//!
//! - **Include patterns**: If any include patterns are specified, at least one
//!   must match for the path to be included. Empty include list means "include all".
//! - **Exclude patterns**: If any exclude pattern matches, the path is excluded.
//! - Both conditions must be satisfied: included AND not excluded.
//!
//! # Example
//!
//! ```rust
//! use claude_history::path_filter::PathFilter;
//! use std::path::Path;
//!
//! // Include only work projects, exclude test directories
//! let filter = PathFilter::new()
//!     .with_include("/home/user/work/")
//!     .unwrap()
//!     .with_exclude("test")
//!     .unwrap();
//!
//! assert!(filter.matches(Path::new("/home/user/work/myproject")));
//! assert!(!filter.matches(Path::new("/home/user/work/myproject-test")));
//! assert!(!filter.matches(Path::new("/home/user/personal/hobby")));
//! ```

use regex::Regex;
use std::fmt;
use std::path::Path;

/// Errors that can occur when building a PathFilter.
#[derive(Debug, Clone)]
pub enum PathFilterError {
    /// Invalid regex pattern
    InvalidPattern(String),
}

impl fmt::Display for PathFilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathFilterError::InvalidPattern(msg) => write!(f, "invalid regex pattern: {}", msg),
        }
    }
}

impl std::error::Error for PathFilterError {}

/// Filter for matching project paths using regex patterns.
///
/// # Include/Exclude Logic
///
/// The filter uses the following logic to determine if a path matches:
///
/// 1. If include patterns are specified, at least one must match
/// 2. No exclude pattern may match
/// 3. Both conditions must be true
///
/// # Example
///
/// ```rust
/// use claude_history::path_filter::PathFilter;
/// use std::path::Path;
///
/// // Filter for Rust projects, excluding vendor directories
/// let filter = PathFilter::new()
///     .with_include("rust")
///     .unwrap()
///     .with_exclude("vendor")
///     .unwrap();
///
/// assert!(filter.matches(Path::new("/projects/rust-app")));
/// assert!(!filter.matches(Path::new("/projects/rust-app/vendor")));
/// assert!(!filter.matches(Path::new("/projects/python-app")));
/// ```
#[derive(Debug, Default, Clone)]
pub struct PathFilter {
    include_patterns: Vec<Regex>,
    exclude_patterns: Vec<Regex>,
}

impl PathFilter {
    /// Create a new empty PathFilter that matches all paths.
    ///
    /// # Example
    ///
    /// ```rust
    /// use claude_history::path_filter::PathFilter;
    /// use std::path::Path;
    ///
    /// let filter = PathFilter::new();
    /// assert!(filter.matches(Path::new("/any/path")));
    /// assert!(!filter.is_active());
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a PathFilter from include and exclude pattern strings.
    ///
    /// # Arguments
    ///
    /// * `include_patterns` - Regex patterns; path must match at least one (if any specified)
    /// * `exclude_patterns` - Regex patterns; path must not match any
    ///
    /// # Errors
    ///
    /// Returns `PathFilterError::InvalidPattern` if any pattern is not valid regex.
    ///
    /// # Example
    ///
    /// ```rust
    /// use claude_history::path_filter::PathFilter;
    /// use std::path::Path;
    ///
    /// let filter = PathFilter::from_patterns(
    ///     &["work", "projects"],
    ///     &["test", "tmp"],
    /// ).unwrap();
    ///
    /// assert!(filter.matches(Path::new("/home/user/work/app")));
    /// assert!(!filter.matches(Path::new("/home/user/work/app-test")));
    /// ```
    pub fn from_patterns(
        include_patterns: &[impl AsRef<str>],
        exclude_patterns: &[impl AsRef<str>],
    ) -> Result<Self, PathFilterError> {
        let mut filter = Self::new();

        for pattern in include_patterns {
            filter = filter.with_include(pattern.as_ref())?;
        }

        for pattern in exclude_patterns {
            filter = filter.with_exclude(pattern.as_ref())?;
        }

        Ok(filter)
    }

    /// Add an include pattern (builder pattern).
    ///
    /// Paths must match at least one include pattern (if any are specified).
    ///
    /// # Errors
    ///
    /// Returns `PathFilterError::InvalidPattern` if the pattern is not valid regex.
    ///
    /// # Example
    ///
    /// ```rust
    /// use claude_history::path_filter::PathFilter;
    /// use std::path::Path;
    ///
    /// let filter = PathFilter::new()
    ///     .with_include("/work/")
    ///     .unwrap()
    ///     .with_include("/projects/")
    ///     .unwrap();
    ///
    /// // Matches either pattern
    /// assert!(filter.matches(Path::new("/home/user/work/app")));
    /// assert!(filter.matches(Path::new("/home/user/projects/app")));
    /// assert!(!filter.matches(Path::new("/home/user/personal/app")));
    /// ```
    pub fn with_include(mut self, pattern: &str) -> Result<Self, PathFilterError> {
        let regex = Regex::new(pattern)
            .map_err(|e| PathFilterError::InvalidPattern(e.to_string()))?;
        self.include_patterns.push(regex);
        Ok(self)
    }

    /// Add an exclude pattern (builder pattern).
    ///
    /// Paths matching any exclude pattern are filtered out.
    ///
    /// # Errors
    ///
    /// Returns `PathFilterError::InvalidPattern` if the pattern is not valid regex.
    ///
    /// # Example
    ///
    /// ```rust
    /// use claude_history::path_filter::PathFilter;
    /// use std::path::Path;
    ///
    /// let filter = PathFilter::new()
    ///     .with_exclude("node_modules")
    ///     .unwrap()
    ///     .with_exclude("target")
    ///     .unwrap();
    ///
    /// assert!(filter.matches(Path::new("/projects/app/src")));
    /// assert!(!filter.matches(Path::new("/projects/app/node_modules")));
    /// assert!(!filter.matches(Path::new("/projects/rust/target")));
    /// ```
    pub fn with_exclude(mut self, pattern: &str) -> Result<Self, PathFilterError> {
        let regex = Regex::new(pattern)
            .map_err(|e| PathFilterError::InvalidPattern(e.to_string()))?;
        self.exclude_patterns.push(regex);
        Ok(self)
    }

    /// Check if a path matches the filter criteria.
    ///
    /// # Matching Rules
    ///
    /// 1. If include patterns exist, at least one must match
    /// 2. No exclude pattern may match
    /// 3. Both conditions must be satisfied
    ///
    /// # Example
    ///
    /// ```rust
    /// use claude_history::path_filter::PathFilter;
    /// use std::path::Path;
    ///
    /// let filter = PathFilter::new()
    ///     .with_include("work")
    ///     .unwrap()
    ///     .with_exclude("test")
    ///     .unwrap();
    ///
    /// // Included and not excluded
    /// assert!(filter.matches(Path::new("/home/user/work/app")));
    ///
    /// // Excluded (even though it matches include)
    /// assert!(!filter.matches(Path::new("/home/user/work/test")));
    ///
    /// // Not included
    /// assert!(!filter.matches(Path::new("/home/user/personal")));
    /// ```
    pub fn matches(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();

        // If include patterns specified, at least one must match
        let include_ok = self.include_patterns.is_empty()
            || self.include_patterns.iter().any(|p| p.is_match(&path_str));

        // All exclude patterns must NOT match
        let exclude_ok = !self.exclude_patterns.iter().any(|p| p.is_match(&path_str));

        include_ok && exclude_ok
    }

    /// Check if the filter has any patterns configured.
    ///
    /// Returns `false` if no include or exclude patterns are set,
    /// meaning the filter will match all paths.
    ///
    /// # Example
    ///
    /// ```rust
    /// use claude_history::path_filter::PathFilter;
    ///
    /// let empty = PathFilter::new();
    /// assert!(!empty.is_active());
    ///
    /// let with_include = PathFilter::new()
    ///     .with_include("work")
    ///     .unwrap();
    /// assert!(with_include.is_active());
    /// ```
    pub fn is_active(&self) -> bool {
        !self.include_patterns.is_empty() || !self.exclude_patterns.is_empty()
    }

    /// Get the number of include patterns.
    pub fn include_count(&self) -> usize {
        self.include_patterns.len()
    }

    /// Get the number of exclude patterns.
    pub fn exclude_count(&self) -> usize {
        self.exclude_patterns.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== PathFilter construction tests ====================

    #[test]
    fn new_filter_matches_everything() {
        let filter = PathFilter::new();
        assert!(filter.matches(Path::new("/any/path")));
        assert!(filter.matches(Path::new("/another/path/here")));
        assert!(!filter.is_active());
    }

    #[test]
    fn from_patterns_creates_filter() {
        let filter = PathFilter::from_patterns(&["work"], &["test"]).unwrap();
        assert!(filter.is_active());
        assert_eq!(filter.include_count(), 1);
        assert_eq!(filter.exclude_count(), 1);
    }

    #[test]
    fn invalid_regex_returns_error() {
        let result = PathFilter::new().with_include("[invalid");
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(matches!(err, PathFilterError::InvalidPattern(_)));
    }

    // ==================== Include pattern tests ====================

    #[test]
    fn single_include_pattern() {
        let filter = PathFilter::new().with_include("work").unwrap();

        assert!(filter.matches(Path::new("/home/user/work/project")));
        assert!(filter.matches(Path::new("/work/app")));
        assert!(!filter.matches(Path::new("/home/user/personal")));
    }

    #[test]
    fn multiple_include_patterns_or_logic() {
        let filter = PathFilter::new()
            .with_include("work")
            .unwrap()
            .with_include("projects")
            .unwrap();

        // Either pattern matches
        assert!(filter.matches(Path::new("/home/user/work/app")));
        assert!(filter.matches(Path::new("/home/user/projects/app")));

        // Neither matches
        assert!(!filter.matches(Path::new("/home/user/personal")));
    }

    #[test]
    fn include_pattern_with_regex_special_chars() {
        // Test regex anchors
        let filter = PathFilter::new().with_include("^/home/user/").unwrap();

        assert!(filter.matches(Path::new("/home/user/work")));
        assert!(!filter.matches(Path::new("/other/home/user/work")));
    }

    // ==================== Exclude pattern tests ====================

    #[test]
    fn single_exclude_pattern() {
        let filter = PathFilter::new().with_exclude("test").unwrap();

        assert!(filter.matches(Path::new("/home/user/work/app")));
        assert!(!filter.matches(Path::new("/home/user/work/app-test")));
        assert!(!filter.matches(Path::new("/home/user/test/app")));
    }

    #[test]
    fn multiple_exclude_patterns_and_logic() {
        let filter = PathFilter::new()
            .with_exclude("test")
            .unwrap()
            .with_exclude("tmp")
            .unwrap();

        // Neither excluded
        assert!(filter.matches(Path::new("/home/user/work/app")));

        // First pattern excludes
        assert!(!filter.matches(Path::new("/home/user/test")));

        // Second pattern excludes
        assert!(!filter.matches(Path::new("/home/user/tmp")));
    }

    #[test]
    fn exclude_common_directories() {
        let filter = PathFilter::new()
            .with_exclude("node_modules")
            .unwrap()
            .with_exclude("target")
            .unwrap()
            .with_exclude("\\.git")
            .unwrap();

        assert!(filter.matches(Path::new("/projects/app/src")));
        assert!(!filter.matches(Path::new("/projects/app/node_modules")));
        assert!(!filter.matches(Path::new("/projects/rust/target")));
        assert!(!filter.matches(Path::new("/projects/app/.git")));
    }

    // ==================== Combined include/exclude tests ====================

    #[test]
    fn include_and_exclude_combined() {
        let filter = PathFilter::new()
            .with_include("work")
            .unwrap()
            .with_exclude("test")
            .unwrap();

        // Matches include, not excluded
        assert!(filter.matches(Path::new("/home/user/work/app")));

        // Matches include but also excluded
        assert!(!filter.matches(Path::new("/home/user/work/app-test")));

        // Doesn't match include
        assert!(!filter.matches(Path::new("/home/user/personal/app")));
    }

    #[test]
    fn exclude_takes_precedence() {
        let filter = PathFilter::new()
            .with_include("work")
            .unwrap()
            .with_exclude("work/test")
            .unwrap();

        assert!(filter.matches(Path::new("/home/user/work/app")));
        assert!(!filter.matches(Path::new("/home/user/work/test")));
    }

    // ==================== Edge case tests ====================

    #[test]
    fn empty_path() {
        let filter = PathFilter::new().with_include("work").unwrap();
        assert!(!filter.matches(Path::new("")));
    }

    #[test]
    fn case_sensitive_matching() {
        let filter = PathFilter::new().with_include("Work").unwrap();

        assert!(filter.matches(Path::new("/home/user/Work/app")));
        assert!(!filter.matches(Path::new("/home/user/work/app")));
    }

    #[test]
    fn case_insensitive_with_regex_flag() {
        let filter = PathFilter::new().with_include("(?i)work").unwrap();

        assert!(filter.matches(Path::new("/home/user/Work/app")));
        assert!(filter.matches(Path::new("/home/user/work/app")));
        assert!(filter.matches(Path::new("/home/user/WORK/app")));
    }

    // ==================== Error display tests ====================

    #[test]
    fn error_display() {
        let err = PathFilterError::InvalidPattern("test error".to_string());
        assert!(err.to_string().contains("invalid regex pattern"));
        assert!(err.to_string().contains("test error"));
    }
}
