//! Time-based filtering for Claude conversations.
//!
//! This module provides functionality to filter conversations by timestamp,
//! supporting both human-friendly duration strings and explicit date ranges.
//!
//! # Duration Syntax
//!
//! Durations are specified as a number followed by a unit:
//! - `h` - hours (e.g., `3h` = 3 hours ago)
//! - `d` - days (e.g., `2d` = 2 days ago)
//! - `w` - weeks (e.g., `1w` = 1 week ago)
//! - `m` - months, approximately 30 days (e.g., `1m` = ~30 days ago)
//!
//! # Examples
//!
//! ```
//! use claude_history::time_filter::{TimeFilter, parse_duration};
//! use chrono::{Duration, Local};
//!
//! // Parse a duration string
//! let duration = parse_duration("2d").unwrap();
//! assert_eq!(duration, Duration::days(2));
//!
//! // Create a filter for "last 2 days"
//! let filter = TimeFilter::from_since("2d").unwrap();
//! assert!(filter.after.is_some());
//! assert!(filter.before.is_none());
//!
//! // Check if a timestamp matches
//! let recent = Local::now() - Duration::hours(1);
//! assert!(filter.matches(recent));
//!
//! let old = Local::now() - Duration::days(10);
//! assert!(!filter.matches(old));
//! ```

use chrono::{DateTime, Duration, Local, NaiveDate};
use std::fmt;

/// Error type for time filter parsing failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeFilterError {
    /// Duration string is empty
    EmptyDuration,
    /// Duration has no numeric part (e.g., "d" instead of "2d")
    MissingNumber,
    /// Duration has no unit part (e.g., "2" instead of "2d")
    MissingUnit,
    /// The numeric part couldn't be parsed
    InvalidNumber(String),
    /// The unit is not recognized (must be h, d, w, or m)
    InvalidUnit(char),
    /// Date string couldn't be parsed (expected YYYY-MM-DD)
    InvalidDate(String),
}

impl fmt::Display for TimeFilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyDuration => write!(f, "duration string is empty"),
            Self::MissingNumber => write!(f, "duration must start with a number (e.g., '2d')"),
            Self::MissingUnit => {
                write!(f, "duration must end with a unit: h, d, w, or m (e.g., '2d')")
            }
            Self::InvalidNumber(s) => write!(f, "invalid number in duration: '{}'", s),
            Self::InvalidUnit(c) => {
                write!(f, "invalid unit '{}', expected: h (hours), d (days), w (weeks), m (months)", c)
            }
            Self::InvalidDate(s) => write!(f, "invalid date '{}', expected format: YYYY-MM-DD", s),
        }
    }
}

impl std::error::Error for TimeFilterError {}

/// A filter for matching conversations by timestamp.
///
/// Conversations match if their timestamp is:
/// - After `after` (if specified), AND
/// - Before `before` (if specified)
///
/// # Examples
///
/// ```
/// use claude_history::time_filter::TimeFilter;
/// use chrono::{Duration, Local};
///
/// // Filter for conversations in the last week
/// let filter = TimeFilter::from_since("1w").unwrap();
///
/// let yesterday = Local::now() - Duration::days(1);
/// assert!(filter.matches(yesterday));
///
/// let last_month = Local::now() - Duration::days(30);
/// assert!(!filter.matches(last_month));
/// ```
#[derive(Debug, Clone, Default)]
pub struct TimeFilter {
    /// Only include conversations after this timestamp
    pub after: Option<DateTime<Local>>,
    /// Only include conversations before this timestamp
    pub before: Option<DateTime<Local>>,
}

impl TimeFilter {
    /// Create a new empty filter (matches all timestamps).
    ///
    /// # Examples
    ///
    /// ```
    /// use claude_history::time_filter::TimeFilter;
    /// use chrono::Local;
    ///
    /// let filter = TimeFilter::new();
    /// assert!(filter.matches(Local::now()));
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a filter from a "since" duration string.
    ///
    /// The filter will match conversations from `now - duration` until now.
    ///
    /// # Arguments
    ///
    /// * `since` - A duration string like "2d", "1w", "3h"
    ///
    /// # Examples
    ///
    /// ```
    /// use claude_history::time_filter::TimeFilter;
    /// use chrono::{Duration, Local};
    ///
    /// let filter = TimeFilter::from_since("2d").unwrap();
    ///
    /// // Recent conversations match
    /// let recent = Local::now() - Duration::hours(6);
    /// assert!(filter.matches(recent));
    ///
    /// // Old conversations don't match
    /// let old = Local::now() - Duration::days(5);
    /// assert!(!filter.matches(old));
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `TimeFilterError` if the duration string is invalid.
    pub fn from_since(since: &str) -> Result<Self, TimeFilterError> {
        let duration = parse_duration(since)?;
        Ok(Self {
            after: Some(Local::now() - duration),
            before: None,
        })
    }

    /// Create a filter from an "after" date string.
    ///
    /// # Arguments
    ///
    /// * `date` - A date string in YYYY-MM-DD format
    ///
    /// # Examples
    ///
    /// ```
    /// use claude_history::time_filter::TimeFilter;
    ///
    /// let filter = TimeFilter::from_after("2026-01-15").unwrap();
    /// assert!(filter.after.is_some());
    /// ```
    pub fn from_after(date: &str) -> Result<Self, TimeFilterError> {
        let datetime = parse_date(date)?;
        Ok(Self {
            after: Some(datetime),
            before: None,
        })
    }

    /// Create a filter from a "before" date string.
    ///
    /// # Arguments
    ///
    /// * `date` - A date string in YYYY-MM-DD format
    ///
    /// # Examples
    ///
    /// ```
    /// use claude_history::time_filter::TimeFilter;
    ///
    /// let filter = TimeFilter::from_before("2026-02-01").unwrap();
    /// assert!(filter.before.is_some());
    /// ```
    pub fn from_before(date: &str) -> Result<Self, TimeFilterError> {
        let datetime = parse_date(date)?;
        Ok(Self {
            after: None,
            before: Some(datetime),
        })
    }

    /// Set the "after" bound from a date string.
    ///
    /// # Examples
    ///
    /// ```
    /// use claude_history::time_filter::TimeFilter;
    ///
    /// let filter = TimeFilter::new()
    ///     .with_after("2026-01-15").unwrap()
    ///     .with_before("2026-02-01").unwrap();
    ///
    /// assert!(filter.after.is_some());
    /// assert!(filter.before.is_some());
    /// ```
    pub fn with_after(mut self, date: &str) -> Result<Self, TimeFilterError> {
        self.after = Some(parse_date(date)?);
        Ok(self)
    }

    /// Set the "before" bound from a date string.
    pub fn with_before(mut self, date: &str) -> Result<Self, TimeFilterError> {
        self.before = Some(parse_date(date)?);
        Ok(self)
    }

    /// Set the "after" bound from a "since" duration string.
    ///
    /// # Examples
    ///
    /// ```
    /// use claude_history::time_filter::TimeFilter;
    ///
    /// let filter = TimeFilter::new().with_since("1w").unwrap();
    /// assert!(filter.after.is_some());
    /// ```
    pub fn with_since(mut self, since: &str) -> Result<Self, TimeFilterError> {
        let duration = parse_duration(since)?;
        self.after = Some(Local::now() - duration);
        Ok(self)
    }

    /// Check if a timestamp matches this filter.
    ///
    /// A timestamp matches if it is:
    /// - Greater than or equal to `after` (if specified), AND
    /// - Less than or equal to `before` (if specified)
    ///
    /// # Examples
    ///
    /// ```
    /// use claude_history::time_filter::TimeFilter;
    /// use chrono::{Duration, Local, TimeZone};
    ///
    /// // Filter: after 2026-01-15, before 2026-02-01
    /// let filter = TimeFilter::new()
    ///     .with_after("2026-01-15").unwrap()
    ///     .with_before("2026-02-01").unwrap();
    ///
    /// // January 20 is in range
    /// let jan_20 = Local.with_ymd_and_hms(2026, 1, 20, 12, 0, 0).unwrap();
    /// assert!(filter.matches(jan_20));
    ///
    /// // January 10 is before the range
    /// let jan_10 = Local.with_ymd_and_hms(2026, 1, 10, 12, 0, 0).unwrap();
    /// assert!(!filter.matches(jan_10));
    ///
    /// // February 10 is after the range
    /// let feb_10 = Local.with_ymd_and_hms(2026, 2, 10, 12, 0, 0).unwrap();
    /// assert!(!filter.matches(feb_10));
    /// ```
    pub fn matches(&self, timestamp: DateTime<Local>) -> bool {
        if let Some(after) = self.after {
            if timestamp < after {
                return false;
            }
        }
        if let Some(before) = self.before {
            if timestamp > before {
                return false;
            }
        }
        true
    }

    /// Check if this filter has any constraints.
    ///
    /// Returns `false` if both `after` and `before` are `None`,
    /// meaning the filter matches all timestamps.
    ///
    /// # Examples
    ///
    /// ```
    /// use claude_history::time_filter::TimeFilter;
    ///
    /// let empty = TimeFilter::new();
    /// assert!(!empty.is_active());
    ///
    /// let with_since = TimeFilter::from_since("1d").unwrap();
    /// assert!(with_since.is_active());
    /// ```
    pub fn is_active(&self) -> bool {
        self.after.is_some() || self.before.is_some()
    }
}

/// Parse a duration string into a `chrono::Duration`.
///
/// # Format
///
/// A duration is a positive integer followed by a unit character:
/// - `h` - hours
/// - `d` - days
/// - `w` - weeks (7 days)
/// - `m` - months (approximately 30 days)
///
/// # Examples
///
/// ```
/// use claude_history::time_filter::parse_duration;
/// use chrono::Duration;
///
/// assert_eq!(parse_duration("3h").unwrap(), Duration::hours(3));
/// assert_eq!(parse_duration("2d").unwrap(), Duration::days(2));
/// assert_eq!(parse_duration("1w").unwrap(), Duration::weeks(1));
/// assert_eq!(parse_duration("1m").unwrap(), Duration::days(30));
/// ```
///
/// # Errors
///
/// Returns `TimeFilterError` if:
/// - The string is empty
/// - There's no numeric part
/// - There's no unit part
/// - The number can't be parsed
/// - The unit is not recognized
///
/// ```
/// use claude_history::time_filter::{parse_duration, TimeFilterError};
///
/// assert!(matches!(parse_duration(""), Err(TimeFilterError::EmptyDuration)));
/// assert!(matches!(parse_duration("d"), Err(TimeFilterError::MissingNumber)));
/// assert!(matches!(parse_duration("2"), Err(TimeFilterError::MissingUnit)));
/// assert!(matches!(parse_duration("2x"), Err(TimeFilterError::InvalidUnit('x'))));
/// ```
pub fn parse_duration(s: &str) -> Result<Duration, TimeFilterError> {
    let s = s.trim();

    if s.is_empty() {
        return Err(TimeFilterError::EmptyDuration);
    }

    // Find where the number ends and unit begins
    let unit_pos = s
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| i);

    let (num_str, unit_str) = match unit_pos {
        None => return Err(TimeFilterError::MissingUnit),
        Some(0) => return Err(TimeFilterError::MissingNumber),
        Some(i) => (&s[..i], &s[i..]),
    };

    let n: i64 = num_str
        .parse()
        .map_err(|_| TimeFilterError::InvalidNumber(num_str.to_string()))?;

    // Unit should be exactly one character
    let unit = unit_str.chars().next().ok_or(TimeFilterError::MissingUnit)?;

    match unit {
        'h' => Ok(Duration::hours(n)),
        'd' => Ok(Duration::days(n)),
        'w' => Ok(Duration::weeks(n)),
        'm' => Ok(Duration::days(n * 30)), // Approximate month
        _ => Err(TimeFilterError::InvalidUnit(unit)),
    }
}

/// Parse a date string in YYYY-MM-DD format into a `DateTime<Local>`.
///
/// The time is set to midnight (00:00:00) in the local timezone.
///
/// # Examples
///
/// ```
/// use claude_history::time_filter::parse_date;
/// use chrono::{Datelike, Timelike};
///
/// let dt = parse_date("2026-01-15").unwrap();
/// assert_eq!(dt.year(), 2026);
/// assert_eq!(dt.month(), 1);
/// assert_eq!(dt.day(), 15);
/// assert_eq!(dt.hour(), 0);
/// assert_eq!(dt.minute(), 0);
/// ```
///
/// # Errors
///
/// Returns `TimeFilterError::InvalidDate` if the string doesn't match YYYY-MM-DD format.
///
/// ```
/// use claude_history::time_filter::{parse_date, TimeFilterError};
///
/// assert!(matches!(parse_date("01-15-2026"), Err(TimeFilterError::InvalidDate(_))));
/// assert!(matches!(parse_date("2026/01/15"), Err(TimeFilterError::InvalidDate(_))));
/// assert!(matches!(parse_date("not-a-date"), Err(TimeFilterError::InvalidDate(_))));
/// ```
pub fn parse_date(s: &str) -> Result<DateTime<Local>, TimeFilterError> {
    let naive_date = NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
        .map_err(|_| TimeFilterError::InvalidDate(s.to_string()))?;

    // Convert to DateTime at midnight local time
    let naive_datetime = naive_date.and_hms_opt(0, 0, 0).ok_or_else(|| {
        TimeFilterError::InvalidDate(format!("{} (failed to create midnight time)", s))
    })?;

    Ok(naive_datetime
        .and_local_timezone(Local)
        .single()
        .ok_or_else(|| TimeFilterError::InvalidDate(format!("{} (ambiguous local time)", s)))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone, Timelike};

    // ==================== parse_duration tests ====================

    #[test]
    fn parse_duration_hours() {
        assert_eq!(parse_duration("1h").unwrap(), Duration::hours(1));
        assert_eq!(parse_duration("24h").unwrap(), Duration::hours(24));
        assert_eq!(parse_duration("100h").unwrap(), Duration::hours(100));
    }

    #[test]
    fn parse_duration_days() {
        assert_eq!(parse_duration("1d").unwrap(), Duration::days(1));
        assert_eq!(parse_duration("7d").unwrap(), Duration::days(7));
        assert_eq!(parse_duration("30d").unwrap(), Duration::days(30));
    }

    #[test]
    fn parse_duration_weeks() {
        assert_eq!(parse_duration("1w").unwrap(), Duration::weeks(1));
        assert_eq!(parse_duration("2w").unwrap(), Duration::weeks(2));
        assert_eq!(parse_duration("4w").unwrap(), Duration::weeks(4));
    }

    #[test]
    fn parse_duration_months() {
        // Months are approximated as 30 days
        assert_eq!(parse_duration("1m").unwrap(), Duration::days(30));
        assert_eq!(parse_duration("3m").unwrap(), Duration::days(90));
    }

    #[test]
    fn parse_duration_with_whitespace() {
        assert_eq!(parse_duration("  2d  ").unwrap(), Duration::days(2));
    }

    #[test]
    fn parse_duration_empty_returns_error() {
        assert!(matches!(
            parse_duration(""),
            Err(TimeFilterError::EmptyDuration)
        ));
        assert!(matches!(
            parse_duration("   "),
            Err(TimeFilterError::EmptyDuration)
        ));
    }

    #[test]
    fn parse_duration_missing_number_returns_error() {
        assert!(matches!(
            parse_duration("d"),
            Err(TimeFilterError::MissingNumber)
        ));
        assert!(matches!(
            parse_duration("h"),
            Err(TimeFilterError::MissingNumber)
        ));
    }

    #[test]
    fn parse_duration_missing_unit_returns_error() {
        assert!(matches!(
            parse_duration("2"),
            Err(TimeFilterError::MissingUnit)
        ));
        assert!(matches!(
            parse_duration("100"),
            Err(TimeFilterError::MissingUnit)
        ));
    }

    #[test]
    fn parse_duration_invalid_unit_returns_error() {
        assert!(matches!(
            parse_duration("2x"),
            Err(TimeFilterError::InvalidUnit('x'))
        ));
        assert!(matches!(
            parse_duration("2y"),
            Err(TimeFilterError::InvalidUnit('y'))
        ));
        assert!(matches!(
            parse_duration("2D"), // Case sensitive
            Err(TimeFilterError::InvalidUnit('D'))
        ));
    }

    // ==================== parse_date tests ====================

    #[test]
    fn parse_date_valid() {
        let dt = parse_date("2026-01-15").unwrap();
        assert_eq!(dt.year(), 2026);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 15);
        assert_eq!(dt.hour(), 0);
        assert_eq!(dt.minute(), 0);
        assert_eq!(dt.second(), 0);
    }

    #[test]
    fn parse_date_with_whitespace() {
        let dt = parse_date("  2026-02-28  ").unwrap();
        assert_eq!(dt.year(), 2026);
        assert_eq!(dt.month(), 2);
        assert_eq!(dt.day(), 28);
    }

    #[test]
    fn parse_date_invalid_format() {
        assert!(matches!(
            parse_date("01-15-2026"),
            Err(TimeFilterError::InvalidDate(_))
        ));
        assert!(matches!(
            parse_date("2026/01/15"),
            Err(TimeFilterError::InvalidDate(_))
        ));
        assert!(matches!(
            parse_date("January 15, 2026"),
            Err(TimeFilterError::InvalidDate(_))
        ));
    }

    #[test]
    fn parse_date_invalid_values() {
        // Invalid month
        assert!(matches!(
            parse_date("2026-13-01"),
            Err(TimeFilterError::InvalidDate(_))
        ));
        // Invalid day
        assert!(matches!(
            parse_date("2026-01-32"),
            Err(TimeFilterError::InvalidDate(_))
        ));
    }

    // ==================== TimeFilter tests ====================

    #[test]
    fn time_filter_new_matches_everything() {
        let filter = TimeFilter::new();
        assert!(!filter.is_active());

        // Should match any timestamp
        assert!(filter.matches(Local::now()));
        assert!(filter.matches(Local::now() - Duration::days(1000)));
        assert!(filter.matches(Local::now() + Duration::days(1000)));
    }

    #[test]
    fn time_filter_from_since() {
        let filter = TimeFilter::from_since("1d").unwrap();
        assert!(filter.is_active());
        assert!(filter.after.is_some());
        assert!(filter.before.is_none());

        // Recent should match
        let recent = Local::now() - Duration::hours(6);
        assert!(filter.matches(recent));

        // Old should not match
        let old = Local::now() - Duration::days(5);
        assert!(!filter.matches(old));
    }

    #[test]
    fn time_filter_from_after() {
        let filter = TimeFilter::from_after("2026-01-15").unwrap();
        assert!(filter.is_active());

        let jan_20 = Local.with_ymd_and_hms(2026, 1, 20, 12, 0, 0).unwrap();
        assert!(filter.matches(jan_20));

        let jan_10 = Local.with_ymd_and_hms(2026, 1, 10, 12, 0, 0).unwrap();
        assert!(!filter.matches(jan_10));
    }

    #[test]
    fn time_filter_from_before() {
        let filter = TimeFilter::from_before("2026-02-01").unwrap();
        assert!(filter.is_active());

        let jan_20 = Local.with_ymd_and_hms(2026, 1, 20, 12, 0, 0).unwrap();
        assert!(filter.matches(jan_20));

        let feb_10 = Local.with_ymd_and_hms(2026, 2, 10, 12, 0, 0).unwrap();
        assert!(!filter.matches(feb_10));
    }

    #[test]
    fn time_filter_range() {
        // After 2026-01-15, before 2026-02-01
        let filter = TimeFilter::new()
            .with_after("2026-01-15")
            .unwrap()
            .with_before("2026-02-01")
            .unwrap();

        assert!(filter.is_active());

        // In range
        let jan_20 = Local.with_ymd_and_hms(2026, 1, 20, 12, 0, 0).unwrap();
        assert!(filter.matches(jan_20));

        // Before range
        let jan_10 = Local.with_ymd_and_hms(2026, 1, 10, 12, 0, 0).unwrap();
        assert!(!filter.matches(jan_10));

        // After range
        let feb_10 = Local.with_ymd_and_hms(2026, 2, 10, 12, 0, 0).unwrap();
        assert!(!filter.matches(feb_10));

        // Boundary: exactly at "after" should match (>=)
        let jan_15 = Local.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        assert!(filter.matches(jan_15));

        // Boundary: exactly at "before" should match (<=)
        let feb_1 = Local.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        assert!(filter.matches(feb_1));
    }

    #[test]
    fn time_filter_with_since() {
        let filter = TimeFilter::new().with_since("2d").unwrap();
        assert!(filter.is_active());

        // Recent matches
        let recent = Local::now() - Duration::hours(12);
        assert!(filter.matches(recent));

        // Old doesn't match
        let old = Local::now() - Duration::days(5);
        assert!(!filter.matches(old));
    }

    #[test]
    fn time_filter_error_display() {
        assert_eq!(
            TimeFilterError::EmptyDuration.to_string(),
            "duration string is empty"
        );
        assert_eq!(
            TimeFilterError::InvalidUnit('x').to_string(),
            "invalid unit 'x', expected: h (hours), d (days), w (weeks), m (months)"
        );
    }
}
