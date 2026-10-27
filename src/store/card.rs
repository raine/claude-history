//! Fixed-width records stored in a generation manifest.
//!
//! Every record is `Pod`, so a validated manifest is read in place from its
//! memory map. Strings live in the manifest's string heap and are referenced
//! by `StrRef`.

use bytemuck::{Pod, Zeroable};

/// Longest stored preview, in bytes. Previews are cut on a char boundary.
pub const PREVIEW_CAP: usize = 400;

/// Byte range in the manifest's string heap.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct StrRef {
    pub off: u32,
    pub len: u32,
}

impl StrRef {
    pub(super) fn end(self) -> u64 {
        u64::from(self.off) + u64::from(self.len)
    }
}

/// Frame of a conversation's text in a generation's text segments (see
/// `text`). An empty ref stands for empty text.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct BlobRef {
    /// Segment number, `text-NNNN.zst`.
    pub seg: u32,
    /// Compressed length in bytes.
    pub len: u32,
    pub off: u64,
}

/// One conversation in the list, pre-sorted newest first.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct CardRow {
    /// Hash of the session path, a stable key for the conversation.
    pub key_hash: u64,
    /// `Conversation::timestamp` in epoch milliseconds; the sort key.
    pub timestamp_ms: i64,
    pub total_tokens: u64,
    /// Hash of `Conversation::full_text`, so an identical manifest implies
    /// identical text.
    pub text_hash: u64,
    /// `Conversation::full_text`.
    pub text: BlobRef,
    pub path: StrRef,
    pub session_id: StrRef,
    pub custom_title: StrRef,
    pub summary: StrRef,
    pub model: StrRef,
    pub cwd: StrRef,
    /// The distinct cwds recorded after `cwd` (`Conversation::cwds`),
    /// separated by NUL; empty when the conversation recorded one at most.
    pub cwds: StrRef,
    pub preview_first: StrRef,
    pub preview_last: StrRef,
    /// `Conversation::parent`, empty when there is none.
    pub parent: StrRef,
    /// Index into the manifest's project table.
    pub project_id: u32,
    pub message_count: u32,
    pub prompt_count: u32,
    pub tool_call_count: u32,
    /// Duration in minutes, or `NO_DURATION`.
    pub duration_minutes: u32,
    /// `source_to_u8` encoding of `Source`.
    pub source: u8,
    /// `card_flags` bits recording which optional fields are present.
    pub flags: u8,
    pub _pad: [u8; 2],
}

pub const NO_DURATION: u32 = u32::MAX;

pub mod card_flags {
    pub const CUSTOM_TITLE: u8 = 1 << 0;
    pub const SUMMARY: u8 = 1 << 1;
    pub const MODEL: u8 = 1 << 2;
    pub const CWD: u8 = 1 << 3;
    /// `Provenance` bits.
    pub const AUTOMATED: u8 = 1 << 4;
    pub const CHILD: u8 = 1 << 5;
    pub const ORPHANED: u8 = 1 << 6;
    pub const BRANCH: u8 = 1 << 7;
    pub const PROVENANCE: u8 = AUTOMATED | CHILD | ORPHANED | BRANCH;
    /// Cards the list leaves out unless asked for.
    pub const HIDDEN: u8 = AUTOMATED | CHILD;

    pub fn of_provenance(provenance: crate::history::Provenance) -> u8 {
        let bit = |set: bool, flag: u8| if set { flag } else { 0 };
        bit(provenance.automated, AUTOMATED)
            | bit(provenance.child, CHILD)
            | bit(provenance.orphaned, ORPHANED)
            | bit(provenance.branch, BRANCH)
    }

    pub fn provenance(flags: u8) -> crate::history::Provenance {
        crate::history::Provenance {
            automated: flags & AUTOMATED != 0,
            child: flags & CHILD != 0,
            orphaned: flags & ORPHANED != 0,
            branch: flags & BRANCH != 0,
        }
    }
}

/// Project shared by any number of cards.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct ProjectRow {
    /// `Conversation::project_name`, which `exclude_projects` matches.
    pub name: StrRef,
    /// `Conversation::project_path`.
    pub path: StrRef,
    /// What the workspace filter compares: the encoded project directory name
    /// for Claude sessions, or the canonical project path for other sources.
    pub workspace_key: StrRef,
    /// `project_flags` bits.
    pub flags: u8,
    pub _pad: [u8; 7],
}

pub mod project_flags {
    pub const NAME: u8 = 1 << 0;
    pub const PATH: u8 = 1 << 1;
    /// `workspace_key` is a canonical path rather than a Claude directory name.
    pub const PATH_KEY: u8 = 1 << 2;
}

/// Native file state a card was built from, parallel to the card rows.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct Fingerprint {
    pub size: u64,
    /// Modification time in epoch nanoseconds, or 0 when the file changed
    /// while it was being loaded and the card may not match it.
    pub mtime_ns: i64,
}

impl From<crate::history::adapter::Stamp> for Fingerprint {
    fn from(stamp: crate::history::adapter::Stamp) -> Self {
        Self {
            size: stamp.size,
            mtime_ns: stamp.mtime_ns,
        }
    }
}

/// Where a card's document lives in the generation's index segments,
/// parallel to the card rows.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct IndexLoc {
    /// Index segment id (see `lexindex::segment_stem`).
    pub seg: u32,
    /// Doc id inside that segment.
    pub doc: u32,
}

/// Where a card's semantic data lives in the generation's semantic segments
/// (see `semantic`), parallel to the card rows.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct SemRow {
    /// Frame in `sem-NNNN.zst`; empty for a card without semantic data.
    pub blob: BlobRef,
    /// Hash of the encoded semantic data, 0 when there is none, so equal
    /// hashes imply equal data.
    pub hash: u64,
}

/// Where a card's own text (`Conversation::own_text`) lives in the
/// generation's own-text segments, parallel to the card rows.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct OwnRow {
    /// Frame in `own-NNNN.zst`; empty for a card without own text.
    pub blob: BlobRef,
    /// Hash of the own text, so equal hashes imply equal text.
    pub hash: u64,
}

/// One index segment of a generation. Docs of the segment that no card
/// points at are dead and skipped by queries.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct IndexSegRow {
    pub id: u32,
    pub n_docs: u32,
}

/// A native file that holds no conversation (empty, or not one of its
/// source's sessions), remembered so it is not parsed again while its
/// fingerprint holds.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct IgnoredRow {
    pub path: StrRef,
    pub fingerprint: Fingerprint,
}

/// Fixed-size manifest header. Section offsets are 8-byte aligned.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Header {
    pub magic: [u8; 8],
    pub version: u32,
    pub row_size: u32,
    pub row_count: u32,
    pub project_count: u32,
    pub rows_off: u64,
    pub projects_off: u64,
    pub fingerprints_off: u64,
    /// `IndexLoc[row_count]`.
    pub locs_off: u64,
    /// `SemRow[row_count]`.
    pub sems_off: u64,
    /// `OwnRow[row_count]`.
    pub owns_off: u64,
    /// `IndexSegRow[seg_count]`, ascending ids.
    pub segs_off: u64,
    /// `IgnoredRow[ignored_count]`.
    pub ignored_off: u64,
    pub heap_off: u64,
    pub heap_len: u64,
    pub seg_count: u32,
    pub ignored_count: u32,
}

pub const MAGIC: [u8; 8] = *b"CHGEN\0\0\0";
pub const VERSION: u32 = 15;

pub fn source_to_u8(source: crate::history::model::SourceKind) -> u8 {
    match source {
        crate::history::model::SourceKind::Claude => 0,
        crate::history::model::SourceKind::Pi => 1,
        crate::history::model::SourceKind::Omp => 2,
        crate::history::model::SourceKind::Codex => 3,
    }
}

pub fn source_from_u8(value: u8) -> Option<crate::history::model::SourceKind> {
    match value {
        0 => Some(crate::history::model::SourceKind::Claude),
        1 => Some(crate::history::model::SourceKind::Pi),
        2 => Some(crate::history::model::SourceKind::Omp),
        3 => Some(crate::history::model::SourceKind::Codex),
        _ => None,
    }
}

/// Returns at most `PREVIEW_CAP` bytes of `text`, cut on a char boundary.
pub fn cap_preview(text: &str) -> &str {
    if text.len() <= PREVIEW_CAP {
        return text;
    }
    let mut end = PREVIEW_CAP;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_have_no_implicit_padding() {
        assert_eq!(std::mem::size_of::<CardRow>(), 152);
        assert_eq!(std::mem::size_of::<ProjectRow>(), 32);
        assert_eq!(std::mem::size_of::<Fingerprint>(), 16);
        assert_eq!(std::mem::size_of::<IndexLoc>(), 8);
        assert_eq!(std::mem::size_of::<SemRow>(), 24);
        assert_eq!(std::mem::size_of::<OwnRow>(), 24);
        assert_eq!(std::mem::size_of::<IndexSegRow>(), 8);
        assert_eq!(std::mem::size_of::<IgnoredRow>(), 24);
        assert_eq!(std::mem::size_of::<Header>(), 112);
    }

    #[test]
    fn cap_preview_cuts_on_char_boundary() {
        assert_eq!(cap_preview("short"), "short");
        let text = format!("{}é", "a".repeat(PREVIEW_CAP - 1));
        let capped = cap_preview(&text);
        assert_eq!(capped.len(), PREVIEW_CAP - 1);
        assert!(capped.chars().all(|c| c == 'a'));
        let exact = "b".repeat(PREVIEW_CAP + 10);
        assert_eq!(cap_preview(&exact).len(), PREVIEW_CAP);
    }
}
