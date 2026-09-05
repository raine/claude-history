//! Annotations: external text attached to a conversation, optionally at one or
//! more line positions within it.
//!
//! claude-history reads, indexes, and renders annotations. Authoring happens
//! outside it: `kind` is a free string the producer sets, and `id` is the
//! producer's handle for deletion.

use crate::error::Result;
use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

mod command;
mod command_annotator;
mod file_annotator;
mod registry_command;
mod set;
mod write;

pub use command::{generated_id, run_annotate};
pub use command_annotator::CommandAnnotator;
pub use file_annotator::{FileAnnotator, sidecar_counts, sidecar_path};
pub use registry_command::run as run_annotators;
pub use set::AnnotatorSet;

/// A contiguous run of JSONL lines an annotation attaches to.
///
/// Deserializes from either a bare number (`3`) or a range string (`"7..9"`).
/// A bare number is the degenerate span whose start and end are equal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TargetSpan {
    pub start: usize,
    pub end: usize,
}

impl TargetSpan {
    pub fn single(line: usize) -> Self {
        Self {
            start: line,
            end: line,
        }
    }
}

impl Serialize for TargetSpan {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        if self.start == self.end {
            serializer.serialize_u64(self.start as u64)
        } else {
            serializer.serialize_str(&format!("{}..{}", self.start, self.end))
        }
    }
}

impl<'de> Deserialize<'de> for TargetSpan {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct SpanVisitor;

        impl<'de> Visitor<'de> for SpanVisitor {
            type Value = TargetSpan;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a line number or a \"start..end\" range string")
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> std::result::Result<TargetSpan, E> {
                Ok(TargetSpan::single(value as usize))
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> std::result::Result<TargetSpan, E> {
                if value < 0 {
                    return Err(E::custom("line number is negative"));
                }
                Ok(TargetSpan::single(value as usize))
            }

            fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<TargetSpan, E> {
                let Some((start, end)) = value.split_once("..") else {
                    return value
                        .parse::<usize>()
                        .map(TargetSpan::single)
                        .map_err(|_| E::custom(format!("target {value} is not a line or range")));
                };
                let start = start
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| E::custom(format!("target {value} has a non-numeric start")))?;
                let end = end
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| E::custom(format!("target {value} has a non-numeric end")))?;
                if end < start {
                    return Err(E::custom(format!("target {value} ends before it starts")));
                }
                Ok(TargetSpan { start, end })
            }
        }

        deserializer.deserialize_any(SpanVisitor)
    }
}

/// One annotation attached to a conversation.
///
/// An empty `targets` list makes the annotation session-level: it names no line
/// and carries no position within the conversation.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Default)]
pub struct Annotation {
    pub id: String,
    #[serde(default)]
    pub targets: Vec<TargetSpan>,
    pub kind: String,
    pub text: String,
    /// Key of the annotator holding this annotation, filled in after a read.
    /// Skipped by serde so the stored record carries no annotator name: the
    /// same file read through a differently named entry stays valid, and a
    /// delete reaches the annotator the note came from.
    #[serde(skip)]
    pub annotator: String,
    /// Where the annotated material sits outside this conversation: an agent's
    /// transcript and the rows a recap summarised. Supplied by the annotator on
    /// the read wire; absent for a note about the conversation itself, so a
    /// reader without it renders the note as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<AnnotationOrigin>,
}

/// A path and, when the annotator states them, the rows within it, as `412` or
/// `412..430`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Default)]
pub struct AnnotationOrigin {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub lines: String,
}

impl AnnotationOrigin {
    /// The file stem and rows, the form short enough for a trailer.
    pub fn short(&self) -> String {
        let stem = self
            .path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        if self.lines.is_empty() {
            stem
        } else {
            format!("{stem} @{}", self.lines)
        }
    }

    /// The full path and rows, the form a reader opens.
    pub fn long(&self) -> String {
        if self.lines.is_empty() {
            self.path.display().to_string()
        } else {
            format!("{} @{}", self.path.display(), self.lines)
        }
    }
}

impl Annotation {
    /// An annotation with no targets attaches to the conversation as a whole.
    pub fn is_session_level(&self) -> bool {
        self.targets.is_empty()
    }

    /// The lowest line this annotation names. Session-level annotations return
    /// `None`, which places them ahead of every positioned annotation.
    pub fn anchor_line(&self) -> Option<usize> {
        self.targets.iter().map(|span| span.start).min()
    }
}

/// Annotations for one conversation, in the order the annotator returned them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConversationAnnotations {
    pub session: Vec<Annotation>,
    pub positioned: Vec<Annotation>,
}

impl ConversationAnnotations {
    pub fn is_empty(&self) -> bool {
        self.session.is_empty() && self.positioned.is_empty()
    }

    /// The number of notes attached, both session-level and positioned.
    pub fn len(&self) -> usize {
        self.session.len() + self.positioned.len()
    }

    /// Split a flat list into session-level and positioned, with positioned
    /// sorted by anchor line so a caller can merge them against messages in one
    /// pass.
    pub fn from_flat(annotations: Vec<Annotation>) -> Self {
        let (session, mut positioned): (Vec<_>, Vec<_>) = annotations
            .into_iter()
            .partition(Annotation::is_session_level);
        positioned.sort_by_key(|annotation| annotation.anchor_line().unwrap_or(0));
        Self {
            session,
            positioned,
        }
    }

    /// Every annotation's text, session-level first, for appending to a
    /// conversation's search fields.
    pub fn texts(&self) -> impl Iterator<Item = &str> {
        self.session
            .iter()
            .chain(self.positioned.iter())
            .map(|annotation| annotation.text.as_str())
    }
}

/// A source of annotations. Reads are batched: one call carries every
/// conversation in scope, so an annotator backed by a subprocess runs once per
/// query rather than once per conversation.
pub trait Annotator {
    fn read(&self, conversations: &[&Path]) -> Result<Vec<(PathBuf, ConversationAnnotations)>>;

    /// Store one annotation and return the id it was stored under. The id is
    /// the annotator's, so a later delete names what this annotator holds
    /// rather than the id the caller supplied.
    fn write(&self, conversation: &Path, annotation: &Annotation) -> Result<String>;

    /// Remove the annotation carrying `id`. The bool reports whether one was
    /// found, so a caller states a missing id rather than a silent success.
    fn delete(&self, conversation: &Path, id: &str) -> Result<bool>;
}

/// Annotations for one conversation, merged across every registered annotator.
///
/// Every failure yields no annotations rather than an error: a viewer opening a
/// transcript shows the transcript whether or not annotations are readable.
pub fn for_conversation(conversation: &Path) -> ConversationAnnotations {
    let Ok(config) = crate::config::load_config() else {
        return ConversationAnnotations::default();
    };
    AnnotatorSet::from_config(&config).read_one(conversation)
}

/// Read annotations for the scoped conversations and append their text to the
/// lexical search fields, returning the annotations by conversation index.
///
/// The append reaches `agent_search_text` because agent search shortlists
/// conversations from that field before loading any transcript; an annotation
/// absent from it leaves its conversation unshortlisted and unfindable however
/// well it matches. `full_text` and `search_text_lower` carry lexical scoring.
///
/// Semantic indexing takes the returned annotations and builds a separate
/// candidate, because a `SemanticChunkSource` applies per conversation and an
/// append here would be absorbed into dialogue chunks.
pub fn enrich_scoped(
    conversations: &mut [crate::history::Conversation],
    scoped: &[usize],
    annotators: &AnnotatorSet,
) -> Result<std::collections::HashMap<usize, ConversationAnnotations>> {
    let paths = scoped
        .iter()
        .filter_map(|index| conversations.get(*index))
        .map(|conversation| conversation.path.as_path())
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    let read = annotators.read_all(&paths);
    if read.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let by_path = read
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>();

    let mut by_index = std::collections::HashMap::new();
    for index in scoped {
        let Some(conversation) = conversations.get_mut(*index) else {
            continue;
        };
        let Some(annotations) = by_path.get(&conversation.path) else {
            continue;
        };
        if annotations.is_empty() {
            continue;
        }

        let appended = annotations.texts().collect::<Vec<_>>().join("\n");
        conversation.agent_search_text.push('\n');
        conversation.agent_search_text.push_str(&appended);
        conversation.full_text.push('\n');
        conversation.full_text.push_str(&appended);
        conversation
            .search_text_lower
            .push_str(&crate::text_match::normalize_for_search(&format!(
                "\n{appended}"
            )));

        by_index.insert(*index, annotations.clone());
    }

    Ok(by_index)
}
