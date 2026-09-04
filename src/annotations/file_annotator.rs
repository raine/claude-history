//! The file annotator: one JSONL sidecar per conversation, under a configured
//! root laid out by project directory.
//!
//! An absent root and an absent sidecar each yield no annotations, so a user who
//! has configured nothing pays one directory check per project and reads no
//! files.

use super::{Annotation, Annotator, ConversationAnnotations};
use crate::error::Result;
use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};

/// Reads annotations from `<root>/<project dir>/<session>.jsonl`.
pub struct FileAnnotator {
    root: PathBuf,
}

impl FileAnnotator {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

/// The sidecar path for a conversation, mirroring the project directory name the
/// conversation itself sits under.
///
/// A conversation whose path has no parent directory or no file stem has no
/// sidecar, because neither half of the layout can be formed.
pub fn sidecar_path(root: &Path, conversation: &Path) -> Option<PathBuf> {
    let project = conversation.parent()?.file_name()?;
    let session = conversation.file_stem()?;
    let mut file_name = session.to_os_string();
    file_name.push(".jsonl");
    Some(root.join(project).join(file_name))
}

/// Parse one sidecar. Lines that do not parse are skipped rather than failing
/// the read, so one malformed record does not hide every annotation in the file.
fn read_sidecar(path: &Path) -> Vec<Annotation> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let reader = std::io::BufReader::new(file);
    let mut annotations = Vec::new();
    for line in reader.lines() {
        let Ok(line) = line else {
            continue;
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(annotation) = serde_json::from_str::<Annotation>(&line) {
            annotations.push(annotation);
        }
    }
    annotations
}

impl Annotator for FileAnnotator {
    fn read(&self, conversations: &[&Path]) -> Result<Vec<(PathBuf, ConversationAnnotations)>> {
        if !self.root.is_dir() {
            return Ok(Vec::new());
        }

        // One existence check per project directory rather than per
        // conversation: a user with annotations in one project pays a single
        // stat for every other project instead of one per conversation in it.
        let mut project_exists: HashMap<PathBuf, bool> = HashMap::new();
        let mut results = Vec::new();

        for conversation in conversations {
            let Some(path) = sidecar_path(&self.root, conversation) else {
                continue;
            };
            let Some(project_dir) = path.parent() else {
                continue;
            };
            let exists = *project_exists
                .entry(project_dir.to_path_buf())
                .or_insert_with(|| project_dir.is_dir());
            if !exists {
                continue;
            }
            let annotations = read_sidecar(&path);
            if annotations.is_empty() {
                continue;
            }
            results.push((
                conversation.to_path_buf(),
                ConversationAnnotations::from_flat(annotations),
            ));
        }

        Ok(results)
    }

    fn write(&self, conversation: &Path, annotation: &Annotation) -> Result<String> {
        super::write::append_to_file(&self.root, conversation, annotation)?;
        Ok(annotation.id.clone())
    }

    fn delete(&self, conversation: &Path, id: &str) -> Result<bool> {
        super::write::remove_from_file(&self.root, conversation, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotations::TargetSpan;

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn sidecar_path_mirrors_the_project_directory() {
        let conversation = PathBuf::from("/home/u/.claude/projects/-home-u-code/abc.jsonl");
        let path = sidecar_path(Path::new("/root"), &conversation).unwrap();
        assert_eq!(path, PathBuf::from("/root/-home-u-code/abc.jsonl"));
    }

    #[test]
    fn absent_root_yields_no_annotations() {
        let annotator = FileAnnotator::new("/does/not/exist");
        let conversation = PathBuf::from("/home/u/.claude/projects/p/abc.jsonl");
        let read = annotator.read(&[conversation.as_path()]).unwrap();
        assert!(read.is_empty());
    }

    #[test]
    fn absent_project_directory_yields_no_annotations() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("other")).unwrap();
        let annotator = FileAnnotator::new(root.path());
        let conversation = PathBuf::from("/home/u/.claude/projects/p/abc.jsonl");
        let read = annotator.read(&[conversation.as_path()]).unwrap();
        assert!(read.is_empty());
    }

    #[test]
    fn reads_positioned_and_session_annotations() {
        let root = tempfile::tempdir().unwrap();
        write(
            &root.path().join("p/abc.jsonl"),
            concat!(
                r#"{"id":"a","targets":[12],"kind":"recap","text":"later"}"#,
                "\n",
                r#"{"id":"b","targets":[],"kind":"note","text":"whole session"}"#,
                "\n",
                r#"{"id":"c","targets":[3,"7..9"],"kind":"recap","text":"earlier"}"#,
                "\n",
            ),
        );
        let annotator = FileAnnotator::new(root.path());
        let conversation = PathBuf::from("/home/u/.claude/projects/p/abc.jsonl");
        let read = annotator.read(&[conversation.as_path()]).unwrap();

        assert_eq!(read.len(), 1);
        let (path, annotations) = &read[0];
        assert_eq!(path, &conversation);
        assert_eq!(annotations.session.len(), 1);
        assert_eq!(annotations.session[0].id, "b");

        // Positioned annotations sort by anchor line, so a caller merges them
        // against messages in one pass.
        let ids = annotations
            .positioned
            .iter()
            .map(|annotation| annotation.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["c", "a"]);
        assert_eq!(
            annotations.positioned[0].targets,
            vec![TargetSpan::single(3), TargetSpan { start: 7, end: 9 }]
        );
    }

    #[test]
    fn malformed_lines_are_skipped_without_hiding_the_rest() {
        let root = tempfile::tempdir().unwrap();
        write(
            &root.path().join("p/abc.jsonl"),
            concat!(
                "{not json\n",
                "\n",
                r#"{"id":"a","targets":[1],"kind":"note","text":"kept"}"#,
                "\n",
            ),
        );
        let annotator = FileAnnotator::new(root.path());
        let conversation = PathBuf::from("/home/u/.claude/projects/p/abc.jsonl");
        let read = annotator.read(&[conversation.as_path()]).unwrap();

        assert_eq!(read.len(), 1);
        assert_eq!(read[0].1.positioned.len(), 1);
        assert_eq!(read[0].1.positioned[0].text, "kept");
    }

    #[test]
    fn a_range_ending_before_it_starts_is_rejected() {
        let parsed = serde_json::from_str::<Annotation>(
            r#"{"id":"a","targets":["9..7"],"kind":"note","text":"t"}"#,
        );
        assert!(parsed.is_err());
    }
}

/// Annotation count per sidecar path, from one walk of the root.
///
/// The list screen states a count for every conversation it draws. Reading each
/// conversation's sidecar on its own opens one file per row and one directory
/// check per project; the sidecars that exist are the only ones carrying a
/// count, so the walk visits exactly them.
pub fn sidecar_counts(root: &Path) -> HashMap<PathBuf, usize> {
    let mut counts = HashMap::new();
    let Ok(projects) = std::fs::read_dir(root) else {
        return counts;
    };
    for project in projects.flatten() {
        let Ok(sidecars) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for sidecar in sidecars.flatten() {
            let path = sidecar.path();
            if path.extension().is_some_and(|ext| ext == "jsonl") {
                let count = read_sidecar(&path).len();
                if count > 0 {
                    counts.insert(path, count);
                }
            }
        }
    }
    counts
}
