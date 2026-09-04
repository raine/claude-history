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
mod file_annotator;
mod write;

pub use command::run_annotate;
pub use file_annotator::{FileAnnotator, sidecar_counts, sidecar_path};

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
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Annotation {
    pub id: String,
    #[serde(default)]
    pub targets: Vec<TargetSpan>,
    pub kind: String,
    pub text: String,
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
}

/// Write one annotation against a conversation, into the file annotator's root.
///
/// `line` names the JSONL line it attaches to; `None` attaches it to the
/// session. The kind is `mark`, because a caller writing through this path is a
/// person choosing to note something rather than a tool generating a summary.
pub fn write_one(conversation: &Path, line: Option<usize>, text: &str) -> Result<()> {
    let config = crate::config::load_config()?;
    let annotation = Annotation {
        id: command::generated_id(),
        targets: line.map(TargetSpan::single).into_iter().collect(),
        kind: "mark".to_string(),
        text: text.to_string(),
    };

    let Some(root) = crate::config::annotations_root(&config) else {
        return Err(crate::error::AppError::ConfigError(
            "no annotations root: set annotations.root, or make a home directory reachable"
                .to_string(),
        ));
    };
    write::append_to_file(&root, conversation, &annotation)
}

/// Remove one annotation by id from the file annotator's root.
///
/// Returns whether a matching annotation was found, so a caller reports a
/// missing id rather than a silent success.
pub fn delete_one(conversation: &Path, id: &str) -> Result<bool> {
    let config = crate::config::load_config()?;
    let Some(root) = crate::config::annotations_root(&config) else {
        return Ok(false);
    };
    write::remove_from_file(&root, conversation, id)
}

/// Annotations for one conversation, read from the configured file root.
///
/// Every failure yields no annotations rather than an error: a viewer opening a
/// transcript shows the transcript whether or not annotations are readable.
pub fn for_conversation(conversation: &Path) -> ConversationAnnotations {
    let Ok(config) = crate::config::load_config() else {
        return ConversationAnnotations::default();
    };
    let Some(root) = crate::config::annotations_root(&config) else {
        return ConversationAnnotations::default();
    };
    FileAnnotator::new(root)
        .read(&[conversation])
        .ok()
        .and_then(|read| read.into_iter().next())
        .map(|(_, annotations)| annotations)
        .unwrap_or_default()
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
    annotator: &dyn Annotator,
) -> Result<std::collections::HashMap<usize, ConversationAnnotations>> {
    let paths = scoped
        .iter()
        .filter_map(|index| conversations.get(*index))
        .map(|conversation| conversation.path.as_path())
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    let read = annotator.read(&paths)?;
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
