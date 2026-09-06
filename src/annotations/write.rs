//! Writing annotations to the file annotator's directory.

use super::{Annotation, sidecar_path};
use crate::error::{AppError, Result};
use std::io::Write;
use std::path::Path;

/// Append an annotation to a conversation's sidecar, creating the project
/// directory and the file when neither exists.
pub fn append_to_file(root: &Path, conversation: &Path, annotation: &Annotation) -> Result<()> {
    let Some(path) = sidecar_path(root, conversation) else {
        return Err(AppError::ConfigError(format!(
            "conversation {} has no project directory or session name, so it has no sidecar",
            conversation.display()
        )));
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_string(annotation)?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

/// Remove the annotation carrying `id`, rewriting the sidecar without it.
///
/// Returns whether a matching annotation was found, so a caller reports a
/// missing id rather than a silent success.
pub fn remove_from_file(root: &Path, conversation: &Path, id: &str) -> Result<bool> {
    let Some(path) = sidecar_path(root, conversation) else {
        return Ok(false);
    };
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };

    let mut kept = Vec::new();
    let mut removed = false;
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // A line that does not parse is kept: a write must not discard a record
        // this version cannot read.
        let matches_id =
            serde_json::from_str::<Annotation>(line).is_ok_and(|annotation| annotation.id == id);
        if matches_id {
            removed = true;
            continue;
        }
        kept.push(line.to_string());
    }
    if !removed {
        return Ok(false);
    }

    // Write through a temporary file in the same directory, so an interrupted
    // write leaves the previous sidecar rather than a truncated one.
    let mut body = kept.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    let temporary = path.with_extension("jsonl.tmp");
    std::fs::write(&temporary, body)?;
    std::fs::rename(&temporary, &path)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotations::{Annotator, FileAnnotator, TargetSpan};

    fn annotation(id: &str, text: &str) -> Annotation {
        Annotation {
            id: id.to_string(),
            targets: vec![TargetSpan::single(4)],
            kind: "note".to_string(),
            text: text.to_string(),
            annotator: String::new(),
            origin: None,
        }
    }

    #[test]
    fn append_creates_the_project_directory_and_file() {
        let root = tempfile::tempdir().unwrap();
        let conversation = Path::new("/home/u/.claude/projects/p/abc.jsonl");

        append_to_file(root.path(), conversation, &annotation("a", "first")).unwrap();
        append_to_file(root.path(), conversation, &annotation("b", "second")).unwrap();

        let annotator = FileAnnotator::new(root.path());
        let read = annotator.read(&[conversation]).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].1.positioned.len(), 2);
    }

    #[test]
    fn remove_deletes_only_the_named_annotation() {
        let root = tempfile::tempdir().unwrap();
        let conversation = Path::new("/home/u/.claude/projects/p/abc.jsonl");
        append_to_file(root.path(), conversation, &annotation("a", "first")).unwrap();
        append_to_file(root.path(), conversation, &annotation("b", "second")).unwrap();

        assert!(remove_from_file(root.path(), conversation, "a").unwrap());

        let read = FileAnnotator::new(root.path())
            .read(&[conversation])
            .unwrap();
        assert_eq!(read[0].1.positioned.len(), 1);
        assert_eq!(read[0].1.positioned[0].id, "b");
    }

    #[test]
    fn remove_reports_a_missing_id_rather_than_succeeding() {
        let root = tempfile::tempdir().unwrap();
        let conversation = Path::new("/home/u/.claude/projects/p/abc.jsonl");
        append_to_file(root.path(), conversation, &annotation("a", "first")).unwrap();

        assert!(!remove_from_file(root.path(), conversation, "absent").unwrap());
        let read = FileAnnotator::new(root.path())
            .read(&[conversation])
            .unwrap();
        assert_eq!(read[0].1.positioned.len(), 1);
    }

    #[test]
    fn remove_keeps_records_this_version_cannot_parse() {
        let root = tempfile::tempdir().unwrap();
        let conversation = Path::new("/home/u/.claude/projects/p/abc.jsonl");
        append_to_file(root.path(), conversation, &annotation("a", "first")).unwrap();
        let path = sidecar_path(root.path(), conversation).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"{\"from\":\"a later version\"}\n").unwrap();
        drop(file);

        assert!(remove_from_file(root.path(), conversation, "a").unwrap());

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("a later version"));
        assert!(!contents.contains("\"id\":\"a\""));
    }

    #[test]
    fn a_note_without_an_origin_is_stored_without_the_key() {
        let root = tempfile::tempdir().unwrap();
        let conversation = Path::new("/home/u/.claude/projects/p/abc.jsonl");
        append_to_file(root.path(), conversation, &annotation("a", "first")).unwrap();
        let path = sidecar_path(root.path(), conversation).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        // A hand-written note describes the conversation itself, so the record
        // stays as it was before origins existed and older readers parse it.
        assert!(!contents.contains("origin"), "{contents}");
    }
}
