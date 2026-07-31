//! Time-based filtering for conversations.
//!
//! A [`TimeFilter`] holds an optional lower and upper bound and tests a
//! conversation's `timestamp` against them. Bounds come from [`TimePoint`],
//! which accepts either a relative span (`2d`, `1w6h`, `6mo`) or an absolute
//! local datetime (`2026-07-20`, `2026-07-20T14:30`).
//!
//! Two details worth knowing:
//!
//! * `m` means minutes, not months. Months are `mo`. Bare `m` for months (as
//!   some tools use) makes `30m` ambiguous between half an hour and two and a
//!   half years, so this module refuses to guess.
//! * Months and years are calendar-accurate, not 30- or 365-day
//!   approximations. `1mo` before 2026-03-31 is 2026-02-28, and clamping to a
//!   short month is what [`chrono::Months`] already does.

use chrono::{DateTime, Duration, Local, LocalResult, Months, NaiveDateTime, TimeZone};
use std::fmt;
use std::str::FromStr;

/// Error type for time filter parsing and resolution failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeFilterError {
    /// The input was empty or only whitespace.
    Empty,
    /// A span component had a unit with no leading count (e.g. `"d"`, `"1dh"`).
    MissingNumber(String),
    /// A span ended with a count and no unit (e.g. `"2"`, `"1d2"`).
    MissingUnit(String),
    /// A count did not fit in the supported range.
    NumberOverflow(String),
    /// The unit is not one this module recognises.
    UnknownUnit { unit: String, input: String },
    /// The input looked absolute but matched none of the accepted formats.
    UnknownDateFormat(String),
    /// The local time does not exist because of a timezone transition.
    UnresolvableLocalTime(String),
    /// The resolved span or datetime fell outside the representable range.
    OutOfRange(String),
    /// The lower bound resolved later than the upper bound.
    InvertedRange {
        after: DateTime<Local>,
        before: DateTime<Local>,
    },
}

impl fmt::Display for TimeFilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "expected a duration or date, got an empty value"),
            Self::MissingNumber(input) => write!(
                f,
                "'{input}': every unit needs a count before it (e.g. '2d', not 'd')"
            ),
            Self::MissingUnit(input) => write!(
                f,
                "'{input}': every count needs a unit after it ({})",
                UNIT_SUMMARY
            ),
            Self::NumberOverflow(input) => {
                write!(f, "'{input}': count is too large")
            }
            Self::UnknownUnit { unit, input } => {
                write!(f, "'{input}': unknown unit '{unit}' ({})", UNIT_SUMMARY)
            }
            Self::UnknownDateFormat(input) => write!(
                f,
                "'{input}': expected YYYY-MM-DD, YYYY-MM-DDTHH:MM, or YYYY-MM-DDTHH:MM:SS"
            ),
            Self::UnresolvableLocalTime(input) => write!(
                f,
                "'{input}': that local time does not exist in this timezone (daylight saving gap)"
            ),
            Self::OutOfRange(input) => {
                write!(
                    f,
                    "'{input}': resolves to a time outside the supported range"
                )
            }
            Self::InvertedRange { after, before } => write!(
                f,
                "lower bound {} is later than upper bound {}",
                after.format("%Y-%m-%d %H:%M"),
                before.format("%Y-%m-%d %H:%M")
            ),
        }
    }
}

impl std::error::Error for TimeFilterError {}

const UNIT_SUMMARY: &str = "s, m (minutes), h, d, w, mo (months), y";

/// Which end of a range a [`TimePoint`] is being resolved for.
///
/// Upper bounds extend to the end of the precision the user typed, so
/// `--before 2026-07-20` includes everything on the 20th rather than cutting
/// off at midnight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bound {
    Lower,
    Upper,
}

/// How precisely an absolute datetime was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Precision {
    Date,
    Minute,
    Second,
}

impl Precision {
    /// The span this precision covers, used to extend upper bounds.
    fn remainder(self) -> Duration {
        match self {
            Self::Date => Duration::days(1),
            Self::Minute => Duration::minutes(1),
            Self::Second => Duration::seconds(1),
        }
    }
}

/// A span measured backwards from a reference instant.
///
/// Calendar months are kept separate from the fixed-length remainder because
/// they cannot be expressed as a [`Duration`] without picking an anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct RelativeSpan {
    months: u32,
    duration: Duration,
}

impl RelativeSpan {
    /// Subtract this span from `now`.
    fn before(
        &self,
        now: DateTime<Local>,
        bound: Bound,
    ) -> Result<DateTime<Local>, TimeFilterError> {
        let shifted = if self.months == 0 {
            now
        } else {
            let naive = subtract_calendar_months(now.naive_local(), self.months)
                .ok_or_else(|| TimeFilterError::OutOfRange(format!("{self:?}")))?;
            resolve_local(naive, bound)?
        };
        shifted
            .checked_sub_signed(self.duration)
            .ok_or_else(|| TimeFilterError::OutOfRange(format!("{self:?}")))
    }
}

/// Subtract calendar months without resolving the resulting wall-clock time.
fn subtract_calendar_months(naive: NaiveDateTime, months: u32) -> Option<NaiveDateTime> {
    naive.checked_sub_months(Months::new(months))
}

/// One end of a time range, parsed from a CLI value.
///
/// Opaque by design: callers only ever parse one and hand it to
/// [`TimeFilter::resolve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimePoint(Point);

/// Either a span backwards from now, or an absolutely pinned wall-clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Point {
    Relative(RelativeSpan),
    /// A local datetime, with the precision it was written at.
    Absolute {
        naive: NaiveDateTime,
        precision: Precision,
    },
}

impl TimePoint {
    /// Resolve to a concrete instant, relative to `now` for spans.
    fn resolve_at(
        &self,
        now: DateTime<Local>,
        bound: Bound,
    ) -> Result<DateTime<Local>, TimeFilterError> {
        match &self.0 {
            Point::Relative(span) => span.before(now, bound),
            Point::Absolute { naive, precision } => {
                // An upper bound written to day precision should include the
                // whole day, so extend to the last instant it covers.
                let naive = match bound {
                    Bound::Lower => *naive,
                    Bound::Upper => naive
                        .checked_add_signed(precision.remainder())
                        .and_then(|end| end.checked_sub_signed(Duration::nanoseconds(1)))
                        .ok_or_else(|| TimeFilterError::OutOfRange(naive.to_string()))?,
                };
                resolve_local(naive, bound)
            }
        }
    }
}

impl FromStr for TimePoint {
    type Err = TimeFilterError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(TimeFilterError::Empty);
        }
        if looks_absolute(trimmed) {
            let (naive, precision) = parse_absolute(trimmed)?;
            Ok(Self(Point::Absolute { naive, precision }))
        } else {
            Ok(Self(Point::Relative(parse_span(trimmed)?)))
        }
    }
}

/// Whether the input opens like a date: four digits then a separator.
///
/// Checked before span parsing so date-shaped input gets a date-format error.
/// Any non-alphanumeric separator counts, not just `-`, so a near-miss like
/// `2026/07/20` is told the expected date format instead of being read as a
/// span and complaining about a missing unit. A digit run followed by a letter
/// (`2026d`) is still a span.
fn looks_absolute(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() >= 5
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && !bytes[4].is_ascii_alphanumeric()
}

/// Parse an absolute local datetime, reporting the precision it was written at.
///
/// `T` and a space are interchangeable separators so shell-quoted values and
/// ISO-8601 both work.
fn parse_absolute(s: &str) -> Result<(NaiveDateTime, Precision), TimeFilterError> {
    const FORMATS: &[(&str, Precision)] = &[
        ("%Y-%m-%dT%H:%M:%S", Precision::Second),
        ("%Y-%m-%d %H:%M:%S", Precision::Second),
        ("%Y-%m-%dT%H:%M", Precision::Minute),
        ("%Y-%m-%d %H:%M", Precision::Minute),
    ];

    for (format, precision) in FORMATS {
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, format) {
            return Ok((naive, *precision));
        }
    }

    let date = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| TimeFilterError::UnknownDateFormat(s.to_string()))?;
    let naive = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| TimeFilterError::OutOfRange(s.to_string()))?;
    Ok((naive, Precision::Date))
}

/// Attach the local timezone to a wall-clock time.
///
/// Daylight saving makes this fallible in two ways. When the clocks go back a
/// local time happens twice; lower bounds take the earlier instant and upper
/// bounds take the later one so inclusive ranges cover both occurrences. When
/// the clocks go forward it never happens at all, and there is no sensible
/// answer, so that is an error.
fn resolve_local(naive: NaiveDateTime, bound: Bound) -> Result<DateTime<Local>, TimeFilterError> {
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(resolved) => Ok(resolved),
        LocalResult::Ambiguous(first, second) => Ok(select_ambiguous(first, second, bound)),
        LocalResult::None => Err(TimeFilterError::UnresolvableLocalTime(naive.to_string())),
    }
}

fn select_ambiguous<T: Copy + Ord>(first: T, second: T, bound: Bound) -> T {
    let (earlier, later) = if first <= second {
        (first, second)
    } else {
        (second, first)
    };
    match bound {
        Bound::Lower => earlier,
        Bound::Upper => later,
    }
}

/// Parse a relative span such as `3h`, `1w`, or `1d6h`.
///
/// Components are count/unit pairs and may repeat; their values add up.
fn parse_span(s: &str) -> Result<RelativeSpan, TimeFilterError> {
    let lowered = s.to_ascii_lowercase();
    let bytes = lowered.as_bytes();
    let mut span = RelativeSpan::default();
    let mut cursor = 0;

    while cursor < bytes.len() {
        let digits_start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if cursor == digits_start {
            return Err(TimeFilterError::MissingNumber(s.to_string()));
        }
        let count: i64 = lowered[digits_start..cursor]
            .parse()
            .map_err(|_| TimeFilterError::NumberOverflow(s.to_string()))?;

        let unit_start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_alphabetic() {
            cursor += 1;
        }
        if cursor == unit_start {
            return Err(TimeFilterError::MissingUnit(s.to_string()));
        }
        let unit = &lowered[unit_start..cursor];

        add_component(&mut span, count, unit, s)?;
    }

    Ok(span)
}

/// Fold one count/unit pair into `span`, rejecting unknown units and overflow.
fn add_component(
    span: &mut RelativeSpan,
    count: i64,
    unit: &str,
    input: &str,
) -> Result<(), TimeFilterError> {
    let overflow = || TimeFilterError::NumberOverflow(input.to_string());

    // `None` means "not a calendar unit"; the inner `Result` carries overflow
    // from the year-to-month conversion.
    let months = match unit {
        "mo" | "mon" | "mos" | "month" | "months" => Some(Ok(count)),
        "y" | "yr" | "yrs" | "year" | "years" => Some(count.checked_mul(12).ok_or_else(overflow)),
        _ => None,
    };

    if let Some(months) = months {
        let months = u32::try_from(months?).map_err(|_| overflow())?;
        span.months = span.months.checked_add(months).ok_or_else(overflow)?;
        return Ok(());
    }

    let duration = match unit {
        "s" | "sec" | "secs" | "second" | "seconds" => Duration::try_seconds(count),
        "m" | "min" | "mins" | "minute" | "minutes" => Duration::try_minutes(count),
        "h" | "hr" | "hrs" | "hour" | "hours" => Duration::try_hours(count),
        "d" | "day" | "days" => Duration::try_days(count),
        "w" | "wk" | "wks" | "week" | "weeks" => Duration::try_weeks(count),
        _ => {
            return Err(TimeFilterError::UnknownUnit {
                unit: unit.to_string(),
                input: input.to_string(),
            });
        }
    };

    span.duration = span
        .duration
        .checked_add(&duration.ok_or_else(overflow)?)
        .ok_or_else(overflow)?;
    Ok(())
}

/// A resolved half-open-at-neither-end range used to filter conversations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TimeFilter {
    /// Include conversations at or after this instant.
    pub after: Option<DateTime<Local>>,
    /// Include conversations at or before this instant.
    pub before: Option<DateTime<Local>>,
}

impl TimeFilter {
    /// Resolve CLI arguments into a filter, relative to `now`.
    ///
    /// `since` and `after` are aliases for the lower bound, and the CLI declares
    /// them mutually exclusive.
    pub fn resolve_at(
        now: DateTime<Local>,
        since: Option<&TimePoint>,
        after: Option<&TimePoint>,
        before: Option<&TimePoint>,
    ) -> Result<Self, TimeFilterError> {
        let lower = after.or(since);
        let filter = Self {
            after: lower
                .map(|point| point.resolve_at(now, Bound::Lower))
                .transpose()?,
            before: before
                .map(|point| point.resolve_at(now, Bound::Upper))
                .transpose()?,
        };

        if let (Some(after), Some(before)) = (filter.after, filter.before)
            && after > before
        {
            return Err(TimeFilterError::InvertedRange { after, before });
        }
        Ok(filter)
    }

    /// Resolve CLI arguments into a filter, relative to the current instant.
    pub fn resolve(
        since: Option<&TimePoint>,
        after: Option<&TimePoint>,
        before: Option<&TimePoint>,
    ) -> Result<Self, TimeFilterError> {
        Self::resolve_at(Local::now(), since, after, before)
    }

    /// Whether `timestamp` falls inside both bounds. Bounds are inclusive.
    pub fn matches(&self, timestamp: DateTime<Local>) -> bool {
        if self.after.is_some_and(|after| timestamp < after) {
            return false;
        }
        if self.before.is_some_and(|before| timestamp > before) {
            return false;
        }
        true
    }

    /// Whether this filter constrains anything.
    ///
    /// Callers use this to skip filtering work entirely on the common
    /// unfiltered path.
    pub fn is_active(&self) -> bool {
        self.after.is_some() || self.before.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};

    fn span(s: &str) -> RelativeSpan {
        match s.parse::<TimePoint>().unwrap() {
            TimePoint(Point::Relative(span)) => span,
            other => panic!("expected a relative span, got {other:?}"),
        }
    }

    fn absolute(s: &str, bound: Bound) -> DateTime<Local> {
        s.parse::<TimePoint>()
            .unwrap()
            .resolve_at(Local::now(), bound)
            .unwrap()
    }

    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .unwrap()
    }

    // ==================== span parsing ====================

    #[test]
    fn parses_fixed_length_units() {
        assert_eq!(span("45s").duration, Duration::seconds(45));
        assert_eq!(span("30m").duration, Duration::minutes(30));
        assert_eq!(span("3h").duration, Duration::hours(3));
        assert_eq!(span("2d").duration, Duration::days(2));
        assert_eq!(span("1w").duration, Duration::weeks(1));
    }

    #[test]
    fn parses_long_unit_spellings() {
        assert_eq!(span("30minutes").duration, Duration::minutes(30));
        assert_eq!(span("2days").duration, Duration::days(2));
        assert_eq!(span("3weeks").duration, Duration::weeks(3));
        assert_eq!(span("6months").months, 6);
        assert_eq!(span("2years").months, 24);
    }

    #[test]
    fn unit_parsing_is_case_insensitive() {
        assert_eq!(span("2D").duration, Duration::days(2));
        assert_eq!(span("6MO").months, 6);
    }

    #[test]
    fn m_means_minutes_and_mo_means_months() {
        assert_eq!(span("1m").duration, Duration::minutes(1));
        assert_eq!(span("1m").months, 0);
        assert_eq!(span("1mo").months, 1);
        assert_eq!(span("1mo").duration, Duration::zero());
    }

    #[test]
    fn years_become_calendar_months() {
        assert_eq!(span("1y").months, 12);
        assert_eq!(span("1y").duration, Duration::zero());
    }

    #[test]
    fn parses_compound_spans() {
        let compound = span("1d6h");
        assert_eq!(compound.duration, Duration::days(1) + Duration::hours(6));

        let mixed = span("1mo2w3h");
        assert_eq!(mixed.months, 1);
        assert_eq!(mixed.duration, Duration::weeks(2) + Duration::hours(3));
    }

    #[test]
    fn repeated_components_accumulate() {
        assert_eq!(span("1h1h").duration, Duration::hours(2));
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(span("  2d  ").duration, Duration::days(2));
    }

    // ==================== span parsing failures ====================

    #[test]
    fn rejects_empty_input() {
        assert_eq!("".parse::<TimePoint>(), Err(TimeFilterError::Empty));
        assert_eq!("   ".parse::<TimePoint>(), Err(TimeFilterError::Empty));
    }

    #[test]
    fn rejects_missing_number() {
        assert_eq!(
            "d".parse::<TimePoint>(),
            Err(TimeFilterError::MissingNumber("d".to_string()))
        );
    }

    #[test]
    fn rejects_missing_unit() {
        assert_eq!(
            "2".parse::<TimePoint>(),
            Err(TimeFilterError::MissingUnit("2".to_string()))
        );
        assert_eq!(
            "1d2".parse::<TimePoint>(),
            Err(TimeFilterError::MissingUnit("1d2".to_string()))
        );
    }

    #[test]
    fn rejects_unknown_unit() {
        assert_eq!(
            "2x".parse::<TimePoint>(),
            Err(TimeFilterError::UnknownUnit {
                unit: "x".to_string(),
                input: "2x".to_string(),
            })
        );
    }

    #[test]
    fn rejects_negative_and_fractional_counts() {
        // A leading '-' is not a digit, so this fails as a missing count
        // rather than silently shifting the bound the wrong way.
        assert!(matches!(
            "-2d".parse::<TimePoint>(),
            Err(TimeFilterError::MissingNumber(_))
        ));
        assert!(matches!(
            "1.5d".parse::<TimePoint>(),
            Err(TimeFilterError::MissingUnit(_))
        ));
    }

    #[test]
    fn rejects_counts_that_do_not_fit_the_parser() {
        assert!(matches!(
            "99999999999999999999d".parse::<TimePoint>(),
            Err(TimeFilterError::NumberOverflow(_))
        ));
        assert!(matches!(
            "99999999999y".parse::<TimePoint>(),
            Err(TimeFilterError::NumberOverflow(_))
        ));
    }

    #[test]
    fn rejects_spans_that_parse_but_cannot_be_subtracted() {
        // A day count can be large enough to be a valid chrono Duration yet
        // still run off the start of the representable calendar, so the
        // failure surfaces at resolution rather than at parse time.
        let point: TimePoint = "9999999999d".parse().unwrap();
        assert!(matches!(
            point.resolve_at(at(2026, 7, 26, 12, 0), Bound::Lower),
            Err(TimeFilterError::OutOfRange(_))
        ));
    }

    // ==================== absolute parsing ====================

    #[test]
    fn parses_date_only_at_midnight() {
        let parsed = absolute("2026-07-20", Bound::Lower);
        assert_eq!((parsed.year(), parsed.month(), parsed.day()), (2026, 7, 20));
        assert_eq!((parsed.hour(), parsed.minute(), parsed.second()), (0, 0, 0));
    }

    #[test]
    fn parses_time_of_day_with_either_separator() {
        let t_form = absolute("2026-07-20T14:30", Bound::Lower);
        let space_form = absolute("2026-07-20 14:30", Bound::Lower);
        assert_eq!(t_form, space_form);
        assert_eq!((t_form.hour(), t_form.minute()), (14, 30));
    }

    #[test]
    fn parses_seconds_precision() {
        let parsed = absolute("2026-07-20T14:30:45", Bound::Lower);
        assert_eq!(
            (parsed.hour(), parsed.minute(), parsed.second()),
            (14, 30, 45)
        );
    }

    #[test]
    fn upper_bound_extends_to_end_of_stated_precision() {
        let end_of_day = absolute("2026-07-20", Bound::Upper);
        assert_eq!(end_of_day.day(), 20);
        assert_eq!(
            (end_of_day.hour(), end_of_day.minute(), end_of_day.second()),
            (23, 59, 59)
        );

        let end_of_minute = absolute("2026-07-20T14:30", Bound::Upper);
        assert_eq!(
            (
                end_of_minute.hour(),
                end_of_minute.minute(),
                end_of_minute.second()
            ),
            (14, 30, 59)
        );
    }

    #[test]
    fn rejects_unrecognised_date_formats() {
        for input in ["2026/07/20", "2026-13-01", "2026-07-20T99:99"] {
            assert!(
                matches!(
                    input.parse::<TimePoint>(),
                    Err(TimeFilterError::UnknownDateFormat(_))
                ),
                "expected {input} to be rejected as a date format"
            );
        }
    }

    #[test]
    fn year_prefixed_input_is_never_read_as_a_span() {
        // Without the leading-year check this would complain about the unit
        // "-07-20", which tells the user nothing useful.
        assert!(matches!(
            "2026-07-20".parse::<TimePoint>(),
            Ok(TimePoint(Point::Absolute { .. }))
        ));
        assert!(matches!(
            "2026-99-99".parse::<TimePoint>(),
            Err(TimeFilterError::UnknownDateFormat(_))
        ));
    }

    #[test]
    fn four_digit_span_is_still_a_span() {
        assert_eq!(span("2026d").duration, Duration::days(2026));
    }

    // ==================== resolution ====================

    #[test]
    fn relative_spans_resolve_backwards_from_now() {
        let now = at(2026, 7, 26, 12, 0);
        let point: TimePoint = "2d".parse().unwrap();
        assert_eq!(
            point.resolve_at(now, Bound::Lower).unwrap(),
            at(2026, 7, 24, 12, 0)
        );
    }

    #[test]
    fn months_are_calendar_accurate_not_thirty_days() {
        let now = at(2026, 3, 31, 12, 0);
        let point: TimePoint = "1mo".parse().unwrap();
        let resolved = point.resolve_at(now, Bound::Lower).unwrap();

        // February has no 31st, so chrono clamps to the 28th. A 30-day
        // approximation would have landed on March 1st instead.
        assert_eq!(
            (resolved.year(), resolved.month(), resolved.day()),
            (2026, 2, 28)
        );
    }

    #[test]
    fn years_cross_leap_days_correctly() {
        let now = at(2025, 3, 1, 9, 0);
        let point: TimePoint = "1y".parse().unwrap();
        let resolved = point.resolve_at(now, Bound::Lower).unwrap();
        assert_eq!(
            (resolved.year(), resolved.month(), resolved.day()),
            (2024, 3, 1)
        );
    }

    #[test]
    fn compound_span_applies_months_then_duration() {
        let now = at(2026, 7, 26, 12, 0);
        let point: TimePoint = "1mo1d".parse().unwrap();
        assert_eq!(
            point.resolve_at(now, Bound::Lower).unwrap(),
            at(2026, 6, 25, 12, 0)
        );
    }

    #[test]
    fn calendar_month_subtraction_preserves_wall_clock_time() {
        let start = chrono::NaiveDate::from_ymd_opt(2026, 12, 1)
            .unwrap()
            .and_hms_opt(1, 30, 0)
            .unwrap();
        let expected = chrono::NaiveDate::from_ymd_opt(2026, 11, 1)
            .unwrap()
            .and_hms_opt(1, 30, 0)
            .unwrap();

        assert_eq!(subtract_calendar_months(start, 1), Some(expected));
    }

    #[test]
    fn ambiguous_bounds_cover_both_possible_instants() {
        let earlier = chrono::Utc.with_ymd_and_hms(2026, 11, 1, 5, 30, 0).unwrap();
        let later = chrono::Utc.with_ymd_and_hms(2026, 11, 1, 6, 30, 0).unwrap();

        assert_eq!(select_ambiguous(later, earlier, Bound::Lower), earlier);
        assert_eq!(select_ambiguous(later, earlier, Bound::Upper), later);
    }

    // ==================== filter construction ====================

    #[test]
    fn default_filter_matches_everything() {
        let filter = TimeFilter::default();
        assert!(!filter.is_active());
        assert!(filter.matches(at(1999, 1, 1, 0, 0)));
        assert!(filter.matches(at(2099, 1, 1, 0, 0)));
    }

    #[test]
    fn since_sets_the_lower_bound_only() {
        let now = at(2026, 7, 26, 12, 0);
        let since: TimePoint = "2d".parse().unwrap();
        let filter = TimeFilter::resolve_at(now, Some(&since), None, None).unwrap();

        assert_eq!(filter.after, Some(at(2026, 7, 24, 12, 0)));
        assert_eq!(filter.before, None);
        assert!(filter.is_active());
    }

    #[test]
    fn after_accepts_a_relative_span_too() {
        let now = at(2026, 7, 26, 12, 0);
        let after: TimePoint = "3h".parse().unwrap();
        let filter = TimeFilter::resolve_at(now, None, Some(&after), None).unwrap();

        assert_eq!(filter.after, Some(at(2026, 7, 26, 9, 0)));
    }

    #[test]
    fn rejects_inverted_ranges() {
        let now = at(2026, 7, 26, 12, 0);
        let after: TimePoint = "2026-07-20".parse().unwrap();
        let before: TimePoint = "2026-07-10".parse().unwrap();

        assert!(matches!(
            TimeFilter::resolve_at(now, None, Some(&after), Some(&before)),
            Err(TimeFilterError::InvertedRange { .. })
        ));
    }

    #[test]
    fn a_single_day_range_is_not_inverted() {
        // Both bounds parse to the same date, but the upper one extends to the
        // end of the day, so the range is valid and covers exactly that day.
        let now = at(2026, 7, 26, 12, 0);
        let day: TimePoint = "2026-07-20".parse().unwrap();
        let filter = TimeFilter::resolve_at(now, None, Some(&day), Some(&day)).unwrap();

        assert!(filter.matches(at(2026, 7, 20, 0, 0)));
        assert!(filter.matches(at(2026, 7, 20, 23, 59)));
        assert!(!filter.matches(at(2026, 7, 21, 0, 0)));
    }

    // ==================== matching ====================

    #[test]
    fn bounds_are_inclusive() {
        let now = at(2026, 7, 26, 12, 0);
        let after: TimePoint = "2026-07-20T08:00".parse().unwrap();
        let filter = TimeFilter::resolve_at(now, None, Some(&after), None).unwrap();

        assert!(filter.matches(at(2026, 7, 20, 8, 0)));
        assert!(!filter.matches(at(2026, 7, 20, 7, 59)));
    }

    #[test]
    fn range_excludes_both_sides() {
        let now = at(2026, 7, 26, 12, 0);
        let after: TimePoint = "2026-07-10".parse().unwrap();
        let before: TimePoint = "2026-07-20".parse().unwrap();
        let filter = TimeFilter::resolve_at(now, None, Some(&after), Some(&before)).unwrap();

        assert!(!filter.matches(at(2026, 7, 9, 23, 59)));
        assert!(filter.matches(at(2026, 7, 15, 12, 0)));
        assert!(!filter.matches(at(2026, 7, 21, 0, 0)));
    }

    #[test]
    fn future_timestamps_match_an_open_upper_bound() {
        // Clock skew between machines can leave transcripts stamped ahead of
        // now; --since should still surface them.
        let now = at(2026, 7, 26, 12, 0);
        let since: TimePoint = "1d".parse().unwrap();
        let filter = TimeFilter::resolve_at(now, Some(&since), None, None).unwrap();

        assert!(filter.matches(at(2026, 7, 27, 12, 0)));
    }
}
