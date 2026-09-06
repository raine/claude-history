//! The `claude-history annotate` command.

use super::{Annotation, TargetSpan};
use crate::cli::AnnotateArgs;
use crate::error::{AppError, Result};

/// Parse one `--line` value: `7` names a single line, `7..9` a run of them.
fn parse_line_argument(value: &str) -> Result<TargetSpan> {
    let Some((start, end)) = value.split_once("..") else {
        return value
            .trim()
            .parse::<usize>()
            .map(TargetSpan::single)
            .map_err(|_| {
                AppError::ConfigError(format!("--line {value} is not a line number or a range"))
            });
    };
    let start = start
        .trim()
        .parse::<usize>()
        .map_err(|_| AppError::ConfigError(format!("--line {value} has a non-numeric start")))?;
    let end = end
        .trim()
        .parse::<usize>()
        .map_err(|_| AppError::ConfigError(format!("--line {value} has a non-numeric end")))?;
    if end < start {
        return Err(AppError::ConfigError(format!(
            "--line {value} ends before it starts"
        )));
    }
    Ok(TargetSpan { start, end })
}

/// An identifier for an annotation the caller did not name.
///
/// The clock supplies the leading half and a counter the trailing half: two
/// writes inside one clock tick read the same nanosecond, and an id repeated
/// there would address the earlier annotation on a later delete.
pub fn generated_id() -> String {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("an_{now:x}_{sequence:x}")
}

/// The transcript the running session writes to.
///
/// Claude Code exports the session id, and the project directory follows from
/// the working directory, so the two compose into a path without a search. An
/// unset id or a missing file leaves the command without a target, and the
/// error names which of the two is absent.
fn live_session_transcript() -> Result<std::path::PathBuf> {
    let session = std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_default();
    if session.is_empty() {
        return Err(AppError::ConfigError(
            "no conversation named and CLAUDE_CODE_SESSION_ID is unset; name a ch_ ref or a transcript path"
                .to_string(),
        ));
    }
    let working = std::env::current_dir().map_err(|error| {
        AppError::ConfigError(format!("working directory does not resolve: {error}"))
    })?;
    let path = crate::history::get_claude_projects_dir(&working)?.join(format!("{session}.jsonl"));
    if !path.is_file() {
        return Err(AppError::ConfigError(format!(
            "session {session} has no transcript at {}",
            path.display()
        )));
    }
    Ok(path)
}

/// The line carrying the last prompt a person typed into the transcript.
///
/// Claude Code marks a typed prompt with `promptSource: "typed"`. A `!` shell
/// invocation, its output, and a tool result carry no such mark, so the mark
/// separates what the person said from what the session recorded around it. A
/// transcript holding no typed prompt yields no line and the annotation covers
/// the conversation as a whole.
fn latest_typed_prompt_line(path: &std::path::Path) -> Result<Option<usize>> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).map_err(|error| {
        AppError::ConfigError(format!("{} does not open: {error}", path.display()))
    })?;
    let mut latest = None;
    for (index, line) in std::io::BufReader::new(file).lines().enumerate() {
        let Ok(line) = line else { continue };
        let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let is_user = record.get("type").and_then(serde_json::Value::as_str) == Some("user");
        let is_typed = record
            .get("promptSource")
            .and_then(serde_json::Value::as_str)
            == Some("typed");
        if is_user && is_typed {
            latest = Some(index + 1);
        }
    }
    Ok(latest)
}

pub fn run_annotate(args: AnnotateArgs) -> Result<String> {
    let annotators = super::AnnotatorSet::from_current_config();
    // A transcript path is accepted alongside a ch_ ref. The viewer and a tool
    // watching the file already hold a path; without this arm each one runs a
    // search that returns the reference the path already names. Anything else
    // goes through the shared agent resolver, so annotate accepts the same
    // reference forms as read and outline.
    let (conversation, label) = match args.reference.as_deref() {
        Some(reference) if std::path::Path::new(reference).is_file() => {
            (std::path::PathBuf::from(reference), reference.to_string())
        }
        Some(reference) => {
            let (keys, _) = crate::agent::service::discover_agent_keys(None)?;
            let resolved =
                crate::agent::service::resolve_agent_conversation_arg(reference, Some(&keys))?;
            let label = resolved.reference.canonical();
            (resolved.key.path.clone(), label)
        }
        None => {
            let path = live_session_transcript()?;
            (path, "this session".to_string())
        }
    };

    if let Some(id) = args.delete {
        // The annotation is addressed by id rather than by position, because a
        // store is rewritten whenever anything is removed from it and every
        // position after the removal would shift. The annotator holding it is
        // found by reading, so a delete reaches the store the id came from.
        let annotations = annotators.read_one(&conversation);
        let holder = annotations
            .session
            .iter()
            .chain(annotations.positioned.iter())
            .find(|annotation| annotation.id == id)
            .map(|annotation| annotation.annotator.clone())
            .unwrap_or_default();
        if annotators.delete(&conversation, &id, &holder)? {
            return Ok(format!("removed {id}\n"));
        }
        return Err(AppError::ConfigError(format!(
            "no annotation with id {id} on {label}"
        )));
    }

    let Some(text) = args.text else {
        return Err(AppError::ConfigError(
            "--text is required when not deleting".to_string(),
        ));
    };

    // A note with no line named follows the last thing the person typed, which
    // is where they were when they wrote it. `--session` covers the whole
    // conversation instead, and named lines override both.
    let mut targets = Vec::with_capacity(args.lines.len());
    for line in &args.lines {
        targets.push(parse_line_argument(line)?);
    }
    if targets.is_empty() && !args.session {
        if let Some(line) = latest_typed_prompt_line(&conversation)? {
            targets.push(TargetSpan::single(line));
        }
    }
    let placement = match targets.first() {
        Some(span) => format!(" at line {}", span.start),
        None => String::new(),
    };

    let annotation = Annotation {
        id: args.id.unwrap_or_else(generated_id),
        targets,
        kind: args.kind,
        text,
        annotator: String::new(),
        origin: None,
    };

    // The id reported is the one the annotator stored under, which is what a
    // later delete names.
    let stored = annotators.write(&conversation, &annotation)?;

    Ok(format!("annotated {label}{placement} as {stored}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_number_is_a_single_line() {
        assert_eq!(parse_line_argument("7").unwrap(), TargetSpan::single(7));
    }

    #[test]
    fn a_range_covers_its_whole_run() {
        assert_eq!(
            parse_line_argument("7..9").unwrap(),
            TargetSpan { start: 7, end: 9 }
        );
    }

    #[test]
    fn a_range_ending_before_it_starts_is_refused() {
        assert!(parse_line_argument("9..7").is_err());
    }

    #[test]
    fn a_non_numeric_line_is_refused() {
        assert!(parse_line_argument("m7").is_err());
    }

    #[test]
    fn generated_ids_do_not_collide_across_calls() {
        assert_ne!(generated_id(), generated_id());
    }

    /// Write `lines` as one transcript and return its path.
    fn transcript(dir: &tempfile::TempDir, lines: &[&str]) -> std::path::PathBuf {
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
        path
    }

    #[test]
    fn typed_prompt_line_is_the_last_typed_user_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = transcript(
            &dir,
            &[
                r#"{"type":"user","promptSource":"typed"}"#,
                r#"{"type":"assistant"}"#,
                r#"{"type":"user","promptSource":"typed"}"#,
                r#"{"type":"assistant"}"#,
            ],
        );
        assert_eq!(latest_typed_prompt_line(&path).unwrap(), Some(3));
    }

    #[test]
    fn shell_invocation_and_tool_result_carry_no_typed_mark() {
        let dir = tempfile::tempdir().unwrap();
        let path = transcript(
            &dir,
            &[
                r#"{"type":"user","promptSource":"typed"}"#,
                r#"{"type":"user"}"#,
                r#"{"type":"user","toolUseResult":{"stdout":"ok"}}"#,
            ],
        );
        assert_eq!(latest_typed_prompt_line(&path).unwrap(), Some(1));
    }

    #[test]
    fn transcript_without_a_typed_prompt_yields_no_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = transcript(
            &dir,
            &[r#"{"type":"user"}"#, r#"{"type":"assistant"}"#, "not json"],
        );
        assert_eq!(latest_typed_prompt_line(&path).unwrap(), None);
    }
}
