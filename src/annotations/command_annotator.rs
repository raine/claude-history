//! An annotator backed by an external command.
//!
//! The command is invoked as `<command> <op>` with one JSON object on stdin and
//! one on stdout. A non-zero exit is the store refusing the operation and is
//! reported as an error: a read drops the annotator from the merge, a write
//! surfaces at the keystroke that caused it.

use super::{Annotation, Annotator, ConversationAnnotations};
use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Invokes a configured command for each operation.
pub struct CommandAnnotator {
    command_line: String,
}

#[derive(Serialize)]
struct ReadRequest<'a> {
    conversations: Vec<&'a Path>,
}

#[derive(Deserialize)]
struct ReadResponse {
    #[serde(default)]
    annotations: Vec<ReadAnnotation>,
}

#[derive(Deserialize)]
struct ReadAnnotation {
    conversation: PathBuf,
    #[serde(flatten)]
    annotation: Annotation,
}

#[derive(Serialize)]
struct WriteRequest<'a> {
    conversation: &'a Path,
    #[serde(flatten)]
    annotation: &'a Annotation,
}

#[derive(Deserialize)]
struct WriteResponse {
    id: String,
}

#[derive(Serialize)]
struct DeleteRequest<'a> {
    conversation: &'a Path,
    id: &'a str,
}

#[derive(Deserialize)]
struct DeleteResponse {
    #[serde(default)]
    deleted: bool,
}

impl CommandAnnotator {
    pub fn new(command_line: impl Into<String>) -> Self {
        Self {
            command_line: command_line.into(),
        }
    }

    /// Run one operation, writing `payload` to stdin and parsing stdout.
    fn invoke<T: for<'de> Deserialize<'de>>(&self, operation: &str, payload: &str) -> Result<T> {
        let mut parts = self.command_line.split_whitespace();
        let Some(program) = parts.next() else {
            return Err(AppError::ConfigError(
                "annotator command is empty".to_string(),
            ));
        };

        let mut child = Command::new(program)
            .args(parts)
            .arg(operation)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        if let Some(stdin) = child.stdin.as_mut() {
            stdin.write_all(payload.as_bytes())?;
        }
        // stdin closes before the wait, so a command reading to end of input
        // reaches it rather than blocking while this process blocks on exit.
        drop(child.stdin.take());

        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(AppError::ConfigError(format!(
                "annotator command `{}` exited with {}",
                self.command_line, output.status
            )));
        }
        serde_json::from_slice(&output.stdout).map_err(|error| {
            AppError::ConfigError(format!(
                "annotator command `{}` returned unreadable {operation} output: {error}",
                self.command_line
            ))
        })
    }
}

impl Annotator for CommandAnnotator {
    fn read(&self, conversations: &[&Path]) -> Result<Vec<(PathBuf, ConversationAnnotations)>> {
        if conversations.is_empty() {
            return Ok(Vec::new());
        }
        let payload = serde_json::to_string(&ReadRequest {
            conversations: conversations.to_vec(),
        })?;
        let response: ReadResponse = self.invoke("read", &payload)?;

        // The request set bounds the response: an annotation naming a
        // conversation outside it would render inside a transcript it does not
        // describe.
        let requested = conversations
            .iter()
            .map(|path| path.to_path_buf())
            .collect::<HashSet<_>>();
        let mut by_conversation: Vec<(PathBuf, Vec<Annotation>)> = Vec::new();
        for entry in response.annotations {
            if !requested.contains(&entry.conversation) {
                continue;
            }
            match by_conversation
                .iter_mut()
                .find(|(path, _)| *path == entry.conversation)
            {
                Some((_, annotations)) => annotations.push(entry.annotation),
                None => by_conversation.push((entry.conversation, vec![entry.annotation])),
            }
        }

        Ok(by_conversation
            .into_iter()
            .map(|(path, annotations)| (path, ConversationAnnotations::from_flat(annotations)))
            .collect())
    }

    fn write(&self, conversation: &Path, annotation: &Annotation) -> Result<String> {
        let payload = serde_json::to_string(&WriteRequest {
            conversation,
            annotation,
        })?;
        let response: WriteResponse = self.invoke("write", &payload)?;
        Ok(response.id)
    }

    fn delete(&self, conversation: &Path, id: &str) -> Result<bool> {
        let payload = serde_json::to_string(&DeleteRequest { conversation, id })?;
        let response: DeleteResponse = self.invoke("delete", &payload)?;
        Ok(response.deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Writes an executable shell script that prints `stdout_body` and exits
    /// with `code`, and returns a command line invoking it.
    fn stub(dir: &Path, stdout_body: &str, code: i32) -> String {
        let path = dir.join("stub.sh");
        std::fs::write(
            &path,
            format!("#!/bin/sh\ncat > /dev/null\nprintf '%s' '{stdout_body}'\nexit {code}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    #[test]
    fn a_read_returns_the_annotations_the_command_prints() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{"annotations":[{"conversation":"/tmp/a.jsonl","id":"n1","targets":[3],"kind":"recap","text":"hello"}]}"#;
        let annotator = CommandAnnotator::new(stub(dir.path(), body, 0));

        let read = annotator.read(&[Path::new("/tmp/a.jsonl")]).unwrap();

        assert_eq!(read.len(), 1);
        assert_eq!(read[0].0, PathBuf::from("/tmp/a.jsonl"));
        assert_eq!(read[0].1.positioned[0].text, "hello");
    }

    #[test]
    fn a_conversation_outside_the_request_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{"annotations":[{"conversation":"/tmp/other.jsonl","id":"n1","kind":"recap","text":"hello"}]}"#;
        let annotator = CommandAnnotator::new(stub(dir.path(), body, 0));

        let read = annotator.read(&[Path::new("/tmp/a.jsonl")]).unwrap();

        assert!(read.is_empty());
    }

    #[test]
    fn a_non_zero_exit_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let annotator = CommandAnnotator::new(stub(dir.path(), "", 3));

        assert!(annotator.read(&[Path::new("/tmp/a.jsonl")]).is_err());
    }

    #[test]
    fn a_write_returns_the_id_the_command_minted() {
        let dir = tempfile::tempdir().unwrap();
        let annotator = CommandAnnotator::new(stub(dir.path(), r#"{"id":"an_from_store"}"#, 0));

        let id = annotator
            .write(
                Path::new("/tmp/a.jsonl"),
                &Annotation {
                    id: "proposed".to_string(),
                    ..Annotation::default()
                },
            )
            .unwrap();

        assert_eq!(id, "an_from_store");
    }

    #[test]
    fn a_delete_reports_what_the_command_found() {
        let dir = tempfile::tempdir().unwrap();
        let annotator = CommandAnnotator::new(stub(dir.path(), r#"{"deleted":false}"#, 0));

        assert!(!annotator.delete(Path::new("/tmp/a.jsonl"), "n1").unwrap());
    }

    #[test]
    fn unreadable_output_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let annotator = CommandAnnotator::new(stub(dir.path(), "not json", 0));

        assert!(annotator.read(&[Path::new("/tmp/a.jsonl")]).is_err());
    }
}
