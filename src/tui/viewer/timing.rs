//! Timing-column types and state machine used above the ledger layer.
//!
//! The viewer's "timing column" is per-block, all-or-nothing: a block
//! either renders with a timestamp column (with the first row carrying
//! the stamp text and continuation rows padded so the name column stays
//! aligned), or it renders without a timing column at all. There is no
//! sentinel-padding string above the ledger writer — column presence is
//! a typed property of [`TimingSlot`].

/// Per-row description of the timing column.
///
/// Above the ledger layer this is the only shape callers pass to
/// describe "what does this row's timing column look like." The ledger
/// writer turns it into either zero spans (`Disabled`),
/// `TIMESTAMP_WIDTH` blank spaces (`Pad`), or a styled
/// `" Mon DD HH:MM "` span (`Stamp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TimingSlot<'a> {
    /// Timing column entirely absent for this row.
    Disabled,
    /// Timing column present; this row pads under a previously-emitted
    /// stamp (or carries the column's blank reservation).
    Pad,
    /// Timing column present; this row carries the timestamp text.
    Stamp(&'a str),
}

impl<'a> TimingSlot<'a> {
    /// Map a "is the timing column present?" bool to either `Pad`
    /// (column on, no stamp) or `Disabled` (column off).
    pub(super) fn from_show_timing(show_timing: bool) -> Self {
        if show_timing {
            Self::Pad
        } else {
            Self::Disabled
        }
    }

    /// Continuation row for a block whose first row was `self`.
    ///
    /// When the column is present (`Pad` or `Stamp`), continuation rows
    /// pad to keep alignment; when it's `Disabled`, continuation rows
    /// also stay disabled — the whole block is column-less.
    pub(super) fn continuation(self) -> Self {
        match self {
            Self::Disabled => Self::Disabled,
            Self::Pad | Self::Stamp(_) => Self::Pad,
        }
    }
}

/// Internal state of [`RowTiming`].
///
/// Distinguishes "no column at all" (`Off`) from "column present but
/// this entry has no stamp by design" (`PadOnly`). The two collapse to
/// the same `TimingSlot::Disabled` from the caller's view of `take_once`,
/// but they differ for `pad`/`consume`: `PadOnly` continuation rows
/// still occupy the column, while `Off` continuation rows do not.
enum TimingState<'a> {
    /// Timing globally off, or this entry's intended stamp was missing
    /// or invalid (which renders as no timing column for the whole
    /// block, matching pre-refactor behavior).
    Off,
    /// Column present but no stamp will ever be emitted (e.g. nested
    /// agent-progress blocks that share the global timing setting but
    /// don't carry their own timestamp).
    PadOnly,
    /// Column present, stamp not yet emitted. The first eligible row
    /// will carry it.
    Pending(&'a str),
    /// Stamp was already emitted; later rows pad under it.
    Consumed,
}

/// Per-block timing cursor.
///
/// Top-level entries hand the first eligible row their timestamp once
/// via [`RowTiming::consume`] or [`RowTiming::take_once`]; later rows
/// align under it with [`RowTiming::pad`] when the column is present,
/// or emit no timing slot at all when it isn't. `RowTiming` owns the
/// state machine so render-pipeline call sites no longer reproduce the
/// "consume timestamp or pad" branch at every row.
pub(super) struct RowTiming<'a> {
    state: TimingState<'a>,
}

impl<'a> RowTiming<'a> {
    /// Top-level entry constructor.
    ///
    /// `show_timing` reflects the global render flag; `timestamp` is the
    /// already-formatted stamp text for this entry, or `None` when the
    /// raw timestamp was missing or could not be parsed. A missing /
    /// invalid stamp with timing enabled produces a column-less block
    /// (matching pre-refactor behavior).
    pub(super) fn new(show_timing: bool, timestamp: Option<&'a str>) -> Self {
        let state = match (show_timing, timestamp) {
            (false, _) => TimingState::Off,
            (true, Some(ts)) => TimingState::Pending(ts),
            (true, None) => TimingState::Off,
        };
        Self { state }
    }

    /// Constructor for blocks that share the global timing setting but
    /// never carry their own stamp (e.g. agent-progress entries). When
    /// `show_timing` is true the column is present and every row pads;
    /// when false there is no column.
    pub(super) fn column_only(show_timing: bool) -> Self {
        let state = if show_timing {
            TimingState::PadOnly
        } else {
            TimingState::Off
        };
        Self { state }
    }

    /// Slot for a row that may carry the first-row timestamp.
    ///
    /// The first call returns `Stamp(ts)` when a stamp is pending; later
    /// calls return `Pad` when the column is present, or `Disabled` when
    /// it isn't.
    pub(super) fn consume(&mut self) -> TimingSlot<'a> {
        match self.state {
            TimingState::Off => TimingSlot::Disabled,
            TimingState::PadOnly => TimingSlot::Pad,
            TimingState::Pending(ts) => {
                self.state = TimingState::Consumed;
                TimingSlot::Stamp(ts)
            }
            TimingState::Consumed => TimingSlot::Pad,
        }
    }

    /// Slot for a `render_ledger_block_styled`-style text block.
    ///
    /// Returns `Stamp(ts)` once for the first text block and `Disabled`
    /// thereafter — even when the column is present. This mirrors the
    /// pre-refactor behavior of multi-text-block messages, where every
    /// text block past the first rendered with the timing column
    /// entirely disabled.
    pub(super) fn take_once(&mut self) -> TimingSlot<'a> {
        match self.state {
            TimingState::Off => TimingSlot::Disabled,
            TimingState::PadOnly => {
                self.state = TimingState::Consumed;
                TimingSlot::Disabled
            }
            TimingState::Pending(ts) => {
                self.state = TimingState::Consumed;
                TimingSlot::Stamp(ts)
            }
            TimingState::Consumed => TimingSlot::Disabled,
        }
    }

    /// Padding-only slot — never marks the timestamp as consumed and
    /// never returns the original timestamp. Use this for nested rows
    /// whose entry already had its timestamp (or lack thereof) handled
    /// by an earlier block.
    pub(super) fn pad(&self) -> TimingSlot<'a> {
        match self.state {
            TimingState::Off => TimingSlot::Disabled,
            _ => TimingSlot::Pad,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_disabled_yields_disabled_for_all_calls() {
        let mut t = RowTiming::new(false, Some("12:34"));
        assert_eq!(t.pad(), TimingSlot::Disabled);
        assert_eq!(t.consume(), TimingSlot::Disabled);
        assert_eq!(t.consume(), TimingSlot::Disabled);
        assert_eq!(t.take_once(), TimingSlot::Disabled);
        assert_eq!(t.pad(), TimingSlot::Disabled);
    }

    #[test]
    fn timing_disabled_ignores_show_timing_flag_when_off() {
        // column_only with show_timing=false also yields Off behavior.
        let mut t = RowTiming::column_only(false);
        assert_eq!(t.pad(), TimingSlot::Disabled);
        assert_eq!(t.consume(), TimingSlot::Disabled);
        assert_eq!(t.take_once(), TimingSlot::Disabled);
    }

    #[test]
    fn valid_timestamp_first_consume_emits_stamp_then_pads() {
        let mut t = RowTiming::new(true, Some("12:34"));
        assert_eq!(t.consume(), TimingSlot::Stamp("12:34"));
        assert_eq!(t.consume(), TimingSlot::Pad);
        assert_eq!(t.consume(), TimingSlot::Pad);
    }

    #[test]
    fn missing_timestamp_with_timing_enabled_renders_disabled_block() {
        // This is the pre-refactor behavior: a top-level entry whose
        // stamp is missing/invalid renders without a timing column at
        // all, not with blank padding.
        let mut t = RowTiming::new(true, None);
        assert_eq!(t.pad(), TimingSlot::Disabled);
        assert_eq!(t.consume(), TimingSlot::Disabled);
        assert_eq!(t.consume(), TimingSlot::Disabled);
        assert_eq!(t.take_once(), TimingSlot::Disabled);
        assert_eq!(t.pad(), TimingSlot::Disabled);
    }

    #[test]
    fn repeated_consume_calls_pad_after_first_stamp() {
        let mut t = RowTiming::new(true, Some("09:00"));
        assert_eq!(t.consume(), TimingSlot::Stamp("09:00"));
        for _ in 0..5 {
            assert_eq!(t.consume(), TimingSlot::Pad);
        }
    }

    #[test]
    fn repeated_take_once_calls_only_emit_stamp_once() {
        let mut t = RowTiming::new(true, Some("09:00"));
        assert_eq!(t.take_once(), TimingSlot::Stamp("09:00"));
        // Subsequent calls return Disabled (block-after-first-text-block).
        assert_eq!(t.take_once(), TimingSlot::Disabled);
        assert_eq!(t.take_once(), TimingSlot::Disabled);
    }

    #[test]
    fn pad_before_consumption_does_not_consume_pending_stamp() {
        let mut t = RowTiming::new(true, Some("09:00"));
        assert_eq!(t.pad(), TimingSlot::Pad);
        assert_eq!(t.pad(), TimingSlot::Pad);
        // Stamp is still available because pad does not advance state.
        assert_eq!(t.consume(), TimingSlot::Stamp("09:00"));
        assert_eq!(t.pad(), TimingSlot::Pad);
    }

    #[test]
    fn pad_after_consumption_keeps_padding_when_column_present() {
        let mut t = RowTiming::new(true, Some("09:00"));
        let _ = t.consume();
        assert_eq!(t.pad(), TimingSlot::Pad);
        assert_eq!(t.pad(), TimingSlot::Pad);
    }

    #[test]
    fn take_once_then_pad_returns_pad_when_column_was_present() {
        let mut t = RowTiming::new(true, Some("09:00"));
        assert_eq!(t.take_once(), TimingSlot::Stamp("09:00"));
        // Subsequent rows align under the consumed stamp.
        assert_eq!(t.pad(), TimingSlot::Pad);
        // And further `take_once` calls still report Disabled — they
        // describe a *new* text block, not a continuation row.
        assert_eq!(t.take_once(), TimingSlot::Disabled);
        // pad stays Pad because the column is still present.
        assert_eq!(t.pad(), TimingSlot::Pad);
    }

    #[test]
    fn column_only_pads_every_row_without_emitting_stamp() {
        // Agent-progress entries: column present, no stamp.
        let mut t = RowTiming::column_only(true);
        assert_eq!(t.pad(), TimingSlot::Pad);
        assert_eq!(t.take_once(), TimingSlot::Disabled);
        // After take_once, pad still pads (column remains present).
        assert_eq!(t.pad(), TimingSlot::Pad);
        assert_eq!(t.consume(), TimingSlot::Pad);
        assert_eq!(t.consume(), TimingSlot::Pad);
    }

    #[test]
    fn from_show_timing_maps_bool_to_pad_or_disabled() {
        assert_eq!(TimingSlot::from_show_timing(true), TimingSlot::Pad);
        assert_eq!(TimingSlot::from_show_timing(false), TimingSlot::Disabled);
    }

    #[test]
    fn continuation_pads_when_column_present_else_disabled() {
        assert_eq!(TimingSlot::Stamp("12:00").continuation(), TimingSlot::Pad);
        assert_eq!(TimingSlot::Pad.continuation(), TimingSlot::Pad);
        assert_eq!(TimingSlot::Disabled.continuation(), TimingSlot::Disabled);
    }
}
