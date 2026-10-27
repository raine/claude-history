//! Claude Code: one JSONL file per session in each project directory of a
//! `projects` root, with the transcripts of the session's subagents and
//! `/btw` forks beside it as sidecars:
//!
//! ```text
//! <project>/<session id>.jsonl
//! <project>/<session id>/subagents/agent-<agent id>.jsonl
//! <project>/<session id>/subagents/agent-<agent id>.meta.json
//! ```
//!
//! Records are Claude's `LogEntry` wire format, which only this module
//! decodes into events.

use crate::claude::{
    AgentContent, ContentBlock, LogEntry, TokenUsage, UserContent, UserProvenance,
};
use crate::error::{AppError, Result};
use crate::history::ParseError;
use crate::history::Provenance;
use crate::history::actions::{Capabilities, Launch, Launcher, Resume, SessionCopy, Target};
use crate::history::adapter::{Adapter, Completeness, Discovery, Located, Stamp};
use crate::history::model::{
    ConversationKey, Event, EventKind, ExecutionMode, HeadStatus, Message, Origin, Part,
    Relationship, Revision, Role, Segment, SessionMeta, SourceInstance, SourceKind, TitleKind,
    Transcript, Usage, Visibility, push_cwd,
};
use crate::history::path::decode_project_dir_name_to_path;
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use std::collections::VecDeque;
use std::fs;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub struct ClaudeAdapter {
    instance: SourceInstance,
}

impl ClaudeAdapter {
    /// The adapter of a `projects` directory.
    pub fn new(root: PathBuf) -> Self {
        Self {
            instance: SourceInstance {
                kind: SourceKind::Claude,
                root,
            },
        }
    }
}

impl Adapter for ClaudeAdapter {
    fn instance(&self) -> &SourceInstance {
        &self.instance
    }

    fn discover(&self, scan_started: SystemTime) -> Discovery {
        let mut discovery = Discovery::default();
        let files = list_sessions(&self.instance.root, &mut discovery);
        discovery.found = super::locate_files(&self.instance, files, scan_started)
            .into_par_iter()
            .map(|located| with_meta_stamp(located, scan_started))
            .collect();
        discovery
    }

    fn resolve(&self, native_id: &str, scan_started: SystemTime) -> Result<Located> {
        super::resolve_file(&self.instance, native_id, scan_started)
            .map(|located| with_meta_stamp(located, scan_started))
    }

    fn decode_into(
        &self,
        located: &Located,
        events: &mut dyn FnMut(Event),
    ) -> Result<Option<Transcript>> {
        super::decode_file(located, false, events)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALL
    }

    /// Claude Code opens a sidecar only from within its session: resuming
    /// one resumes that session, and it cannot be forked or renamed.
    fn capabilities_for(&self, provenance: Provenance) -> Capabilities {
        if provenance.child || provenance.branch {
            Capabilities {
                resume: true,
                fork: false,
                delete: true,
                rename: false,
            }
        } else {
            Capabilities::ALL
        }
    }

    fn resume(&self, target: Target<'_>, fork: bool, launch: &Launch) -> Result<Resume> {
        let cwd = std::env::current_dir().map_err(|e| {
            AppError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Failed to get current directory: {}", e),
            ))
        })?;
        resume_plan(target, &cwd, fork, launch)
    }

    /// Deletes every transcript of a session, in any project, with its
    /// session directory, except the `/btw` forks there, which are
    /// conversations of their own. A sidecar is deleted alone, with its
    /// metadata file.
    fn delete(&self, path: &Path, _launcher: &Launcher) -> Result<()> {
        if sidecar(path).is_some() {
            return delete_sidecar(path);
        }
        let uuid = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        crate::history::delete_session_by_uuid(uuid).map(|_| ())
    }

    fn rename(&self, path: &Path, title: &str) -> Result<()> {
        crate::history::append_session_rename(path, title)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ResumeAction {
    Run { current_dir: PathBuf },
    CopyToCurrent { cwd_projects_dir: PathBuf },
}

/// Claude resumes a session only from the project directory that holds it.
/// Run there when `cwd` or the conversation's project maps to that
/// directory; otherwise (and to fork into another project) copy the session
/// into `cwd`'s project directory first.
fn resolve_resume_action(
    selected_path: &Path,
    project_path: Option<&Path>,
    cwd: &Path,
    fork_session: bool,
) -> Result<ResumeAction> {
    let conv_projects_dir = selected_path.parent().ok_or_else(|| {
        AppError::ClaudeExecutionError(
            "Cannot determine conversation's project directory".to_string(),
        )
    })?;
    let cwd_projects_dir = crate::history::get_claude_projects_dir(cwd)?;
    let project_dir = project_path.filter(|p| p.exists() && p.is_dir());

    let same_dir = |dir: &Path| crate::history::path::same_path(dir, conv_projects_dir);
    if project_dir.is_none() || (fork_session && !same_dir(&cwd_projects_dir)) {
        return Ok(ResumeAction::CopyToCurrent { cwd_projects_dir });
    }

    if same_dir(&cwd_projects_dir) {
        return Ok(ResumeAction::Run {
            current_dir: cwd.to_path_buf(),
        });
    }

    let project_dir = project_dir.unwrap();
    let project_projects_dir = crate::history::get_claude_projects_dir(project_dir)?;
    if same_dir(&project_projects_dir) {
        Ok(ResumeAction::Run {
            current_dir: project_dir.to_path_buf(),
        })
    } else {
        Ok(ResumeAction::CopyToCurrent { cwd_projects_dir })
    }
}

fn resume_plan(
    target: Target<'_>,
    cwd: &Path,
    fork_session: bool,
    launch: &Launch,
) -> Result<Resume> {
    let default_args = &launch.default_args;
    // A sidecar is resumed through the session that holds it.
    let parent = parent_transcript(target.path);
    if let Some(parent) = &parent
        && !parent.is_file()
    {
        return Err(AppError::ClaudeExecutionError(format!(
            "The session holding this transcript no longer exists: {}",
            parent.display()
        )));
    }
    let selected_path = parent.as_deref().unwrap_or(target.path);
    let conversation_id = selected_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| {
            AppError::ClaudeExecutionError("Conversation filename is not valid Unicode".to_string())
        })?
        .to_owned();

    let conv_projects_dir = selected_path.parent().ok_or_else(|| {
        AppError::ClaudeExecutionError(
            "Cannot determine conversation's project directory".to_string(),
        )
    })?;

    let mut command = launch.launcher.command();
    command.args(["--resume", &conversation_id]);
    match resolve_resume_action(selected_path, target.project, cwd, fork_session)? {
        ResumeAction::CopyToCurrent { cwd_projects_dir } => {
            command.args(default_args);
            command.current_dir(cwd);
            Ok(Resume {
                copy: Some(SessionCopy {
                    transcript: selected_path.to_path_buf(),
                    session_id: conversation_id,
                    from_dir: conv_projects_dir.to_path_buf(),
                    to_dir: cwd_projects_dir,
                }),
                command,
            })
        }
        ResumeAction::Run { current_dir } => {
            if fork_session {
                command.arg("--fork-session");
            }
            command.args(default_args);
            command.current_dir(current_dir);
            Ok(Resume {
                copy: None,
                command,
            })
        }
    }
}

/// Session files: `*.jsonl` directly inside each project directory, except
/// the subagent transcripts older versions kept there (`agent-*`), and the
/// sidecars in each session directory. A missing root holds none.
fn list_sessions(root: &Path, discovery: &mut Discovery) -> Vec<PathBuf> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(_) => {
            discovery.completeness = Completeness::Degraded;
            return Vec::new();
        }
    };
    let mut dirs = Vec::new();
    for entry in entries {
        match entry {
            // The listing's file type saves a stat per project; a symlink
            // is followed to see whether it leads to a directory.
            Ok(entry) => match entry.file_type() {
                Ok(kind) if kind.is_dir() => dirs.push(entry.path()),
                Ok(kind) if kind.is_symlink() && entry.path().is_dir() => dirs.push(entry.path()),
                Ok(_) => {}
                Err(_) if entry.path().is_dir() => dirs.push(entry.path()),
                Err(_) => {}
            },
            Err(_) => discovery.completeness = Completeness::Degraded,
        }
    }
    let listed: Vec<(PathBuf, io::Result<Listed>)> = dirs
        .into_par_iter()
        .map(|dir| {
            let files = list_project_dir(&dir);
            (dir, files)
        })
        .collect();
    let mut files = Vec::new();
    for (dir, listed) in listed {
        match listed {
            Ok(listed) => {
                files.extend(listed.files);
                for dir in listed.missed {
                    discovery.missed_dir(dir);
                }
            }
            Err(_) => discovery.missed_dir(dir),
        }
    }
    files
}

/// The session files of one project directory, and the sidecar directories
/// in it that could not be listed.
#[derive(Default)]
struct Listed {
    files: Vec<PathBuf>,
    missed: Vec<PathBuf>,
}

fn list_project_dir(dir: &Path) -> io::Result<Listed> {
    let mut listed = Listed::default();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if is_session_file(&path) {
            listed.files.push(path);
        } else if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            let sidecars = path.join(SIDECAR_DIR);
            match fs::read_dir(&sidecars) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(entry) if is_sidecar_file(&entry.path()) => {
                                listed.files.push(entry.path());
                            }
                            Ok(_) => {}
                            Err(_) => listed.missed.push(sidecars.clone()),
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) if error.kind() == io::ErrorKind::NotADirectory => {}
                Err(_) => listed.missed.push(sidecars),
            }
        }
    }
    Ok(listed)
}

fn is_session_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "jsonl")
        && !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(SIDECAR_PREFIX))
}

/// Directory of a session directory holding its sidecars.
const SIDECAR_DIR: &str = "subagents";
const SIDECAR_PREFIX: &str = "agent-";

/// Agents Claude Code runs for itself rather than for the conversation:
/// prompt suggestions and compaction.
const INTERNAL_AGENTS: &[&str] = &["aprompt_suggestion-", "acompact-"];

fn is_sidecar_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "jsonl")
        && path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.strip_prefix(SIDECAR_PREFIX))
            .is_some_and(|agent| {
                !agent.is_empty() && !INTERNAL_AGENTS.iter().any(|kind| agent.starts_with(kind))
            })
}

/// Where a sidecar transcript sits: `<project>/<session id>/subagents/
/// agent-<agent id>.jsonl`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sidecar<'a> {
    /// The project directory holding the session.
    pub project_dir: &'a Path,
    /// Id of the session holding the sidecar.
    pub session_id: &'a str,
    /// Claude Code's id of the agent that wrote it.
    pub agent_id: &'a str,
}

/// Where `path` sits, when it is a sidecar transcript.
pub fn sidecar(path: &Path) -> Option<Sidecar<'_>> {
    if !is_sidecar_file(path) {
        return None;
    }
    let agent_id = path.file_stem()?.to_str()?.strip_prefix(SIDECAR_PREFIX)?;
    let sidecars = path.parent()?;
    if sidecars.file_name()? != SIDECAR_DIR {
        return None;
    }
    let session_dir = sidecars.parent()?;
    Some(Sidecar {
        project_dir: session_dir.parent()?,
        session_id: session_dir.file_name()?.to_str()?,
        agent_id,
    })
}

/// Transcript of the session holding the sidecar at `path`.
pub fn parent_transcript(path: &Path) -> Option<PathBuf> {
    let sidecar = sidecar(path)?;
    Some(
        sidecar
            .project_dir
            .join(format!("{}.jsonl", sidecar.session_id)),
    )
}

/// The project directory holding the session file or sidecar at `path`.
pub fn project_dir(path: &Path) -> Option<&Path> {
    match sidecar(path) {
        Some(sidecar) => Some(sidecar.project_dir),
        None => path.parent(),
    }
}

/// The metadata file beside the sidecar transcript at `path`.
fn meta_path(path: &Path) -> PathBuf {
    path.with_extension("meta.json")
}

/// What Claude Code records about a sidecar when it starts the agent.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SidecarMeta {
    agent_type: Option<String>,
    description: Option<String>,
    /// The Agent tool call that started the agent.
    tool_use_id: Option<String>,
    #[serde(default)]
    is_fork: bool,
}

impl SidecarMeta {
    fn read(path: &Path) -> Self {
        fs::read(meta_path(path))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Whether the person forked the conversation off (`/btw`, then `f`).
    /// The Agent tool's forks carry the call that started them.
    fn is_user_fork(&self) -> bool {
        self.is_fork && self.tool_use_id.is_none()
    }

    /// What the sidecar is listed as, when its own records name nothing.
    fn summary(&self) -> Option<String> {
        let description = self.description.as_deref().map(str::trim);
        let description = description.filter(|text| !text.is_empty());
        if self.is_user_fork() {
            return description.map(str::to_owned);
        }
        match (self.agent_type.as_deref(), description) {
            (Some(kind), Some(text)) => Some(format!("{kind}: {text}")),
            (Some(kind), None) => Some(kind.to_owned()),
            (None, text) => text.map(str::to_owned),
        }
    }
}

/// `located` with a sidecar's metadata file folded into its stamp, so a
/// changed metadata file reads as a changed conversation.
fn with_meta_stamp(mut located: Located, scan_started: SystemTime) -> Located {
    if sidecar(&located.path).is_none() {
        return located;
    }
    let Ok(meta) = fs::metadata(meta_path(&located.path)) else {
        return located;
    };
    let modified = located.modified.max(meta.modified().ok());
    located.stamp = Stamp::of(located.size + meta.len(), modified, scan_started);
    located
}

/// Removes the sidecar transcript at `path` and its metadata file.
fn delete_sidecar(path: &Path) -> Result<()> {
    fs::remove_file(path)?;
    match fs::remove_file(meta_path(path)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

/// Removes the session directory `dir`, except the `/btw` forks among its
/// sidecars, which stay with their metadata.
pub(crate) fn remove_session_dir(dir: &Path) -> io::Result<()> {
    let sidecars = dir.join(SIDECAR_DIR);
    let forks: Vec<PathBuf> = match fs::read_dir(&sidecars) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| is_sidecar_file(path) && SidecarMeta::read(path).is_user_fork())
            .collect(),
        Err(_) => Vec::new(),
    };
    if forks.is_empty() {
        return fs::remove_dir_all(dir);
    }
    let kept: Vec<PathBuf> = forks
        .iter()
        .flat_map(|fork| [fork.clone(), meta_path(fork)])
        .collect();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path == sidecars {
            continue;
        }
        if path.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }
    for entry in fs::read_dir(&sidecars)? {
        let path = entry?.path();
        if kept.contains(&path) {
            continue;
        }
        if path.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Decodes a Claude session from `reader`, the contents of `path`, passing
/// its events to `events`. The session was last active at its latest
/// message, else when it was `modified`.
pub(crate) fn decode_reader(
    key: ConversationKey,
    path: &Path,
    reader: impl BufRead,
    modified: Option<SystemTime>,
    events: &mut dyn FnMut(Event),
) -> Result<Transcript> {
    let mut lines = Lines::new(reader);
    // The current line, plus up to 2 lines after it for parse error context.
    let mut window: VecDeque<String> = VecDeque::with_capacity(3);
    let mut context_before: VecDeque<String> = VecDeque::with_capacity(2);
    for _ in 0..2 {
        match lines.next_line()? {
            Some(line) => window.push_back(line),
            None => break,
        }
    }
    let mut records = Records::default();
    let mut activity = super::LastActivity::default();
    let mut diagnostics = Vec::new();
    let mut line_index: usize = 0;
    while let Some(line) = window.pop_front() {
        if let Some(next) = lines.next_line()? {
            window.push_back(next);
        }
        if !line.trim().is_empty() {
            match serde_json::from_str::<LogEntry>(&line) {
                Ok(entry) => {
                    if let Some(event) = records.push(entry, line_number(line_index)) {
                        activity.observe(&event);
                        events(event);
                    }
                }
                Err(error) => diagnostics.push(ParseError {
                    line_number: line_index + 1,
                    line_content: line.clone(),
                    error_message: error.to_string(),
                    context_before: context_before.iter().cloned().collect(),
                    context_after: window.iter().cloned().collect(),
                }),
            }
        }
        context_before.push_back(line);
        if context_before.len() > 2 {
            context_before.pop_front();
        }
        line_index += 1;
    }

    let segments = vec![Segment {
        artifact: path.to_path_buf(),
        start: 0,
        end: lines.bytes,
        inherited: false,
    }];
    let cwd = records.cwds.first().cloned();
    let project = cwd.clone().unwrap_or_else(|| {
        project_dir(path)
            .and_then(Path::file_name)
            .map(|dir| decode_project_dir_name_to_path(&dir.to_string_lossy()))
            .unwrap_or_default()
    });
    let (session_id, relationship, summary) = match sidecar(path) {
        Some(sidecar) => {
            let meta = SidecarMeta::read(path);
            let parent = sidecar.session_id.to_owned();
            let relationship = if meta.is_user_fork() {
                Relationship::Branch { parent }
            } else {
                Relationship::Child { parent }
            };
            let summary = records.summary.take().or_else(|| meta.summary());
            (sidecar.agent_id.to_owned(), relationship, summary)
        }
        None => (
            path.file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned(),
            Relationship::Root,
            records.summary.take(),
        ),
    };
    let meta = SessionMeta {
        session_id,
        cwd,
        cwds: std::mem::take(&mut records.cwds),
        project: Some(project),
        model: records.model.take(),
        summary,
        custom_title: records.custom_title.take(),
        updated_at: activity
            .at()
            .or_else(|| modified.map(DateTime::<Utc>::from)),
    };
    Ok(Transcript {
        revision: Revision::of(SourceKind::Claude, &segments, &[mtime_ns(modified)]),
        key,
        source: SourceKind::Claude,
        head: HeadStatus::Current,
        mode: ExecutionMode::Unknown,
        relationship,
        meta,
        segments,
        records: records.count,
        events: Vec::new(),
        diagnostics,
    })
}

/// Decodes `reader` as the Claude session at `path`, outside any root.
#[cfg(test)]
pub(crate) fn decode_test_reader(path: &Path, reader: impl BufRead) -> Result<Transcript> {
    let instance = SourceInstance {
        kind: SourceKind::Claude,
        root: PathBuf::new(),
    };
    let key = ConversationKey {
        instance,
        native_id: path.to_string_lossy().into_owned(),
    };
    let mut decoded = Vec::new();
    let mut transcript = decode_reader(key, path, reader, None, &mut |event| decoded.push(event))?;
    transcript.events = decoded;
    Ok(transcript)
}

fn line_number(index: usize) -> Origin {
    Origin {
        segment: 0,
        line: u32::try_from(index + 1).unwrap_or(u32::MAX),
    }
}

pub(super) fn mtime_ns(modified: Option<SystemTime>) -> i64 {
    modified
        .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |since| i64::try_from(since.as_nanos()).unwrap_or(0))
}

/// Lines of a reader as `BufRead::lines` splits them, counting the bytes
/// read.
struct Lines<R> {
    reader: R,
    bytes: u64,
}

impl<R: BufRead> Lines<R> {
    fn new(reader: R) -> Self {
        Self { reader, bytes: 0 }
    }

    fn next_line(&mut self) -> io::Result<Option<String>> {
        let mut line = String::new();
        let read = self.reader.read_line(&mut line)?;
        if read == 0 {
            return Ok(None);
        }
        self.bytes += read as u64;
        if line.ends_with('\n') {
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
        }
        Ok(Some(line))
    }
}

/// Converts Claude-format records into events, in order, collecting the
/// session metadata they define: the distinct cwds of user records, the first
/// assistant model, the first summary unless a generated title replaces
/// it, and the last custom title. A file-history snapshot, which records
/// file states for undo rather than the conversation, yields no event.
#[derive(Default)]
pub(crate) struct Records {
    count: usize,
    cwds: Vec<PathBuf>,
    model: Option<String>,
    summary: Option<String>,
    custom_title: Option<String>,
}

impl Records {
    pub(crate) fn push(&mut self, entry: LogEntry, origin: Origin) -> Option<Event> {
        self.count += 1;
        let mut visibility = Visibility::Dialogue;
        let (timestamp, kind) = match entry {
            LogEntry::User {
                message,
                provenance,
                timestamp,
                cwd,
                parent_tool_use_id,
                usage,
                ..
            } => {
                if let Some(cwd) = cwd {
                    push_cwd(&mut self.cwds, PathBuf::from(cwd));
                }
                let mut parts = match message.content {
                    UserContent::String(text) => vec![Part::Text(text)],
                    UserContent::Blocks(blocks) => parts(blocks),
                };
                let generated = generated_user(&provenance);
                let before = parts.len();
                parts.retain(|part| match part {
                    Part::Text(text) => !generated.unwrap_or_else(|| task_notification(text)),
                    _ => true,
                });
                if parts.is_empty() && before > 0 {
                    visibility = Visibility::Injected;
                }
                for part in &mut parts {
                    if let Part::Text(text) = part
                        && let Some(typed) = unframed(text)
                    {
                        *text = typed;
                    }
                }
                (
                    timestamp,
                    EventKind::Message(Message {
                        role: Role::User,
                        id: None,
                        parts,
                        model: None,
                        usage: usage.map(usage_of),
                        parent_tool_use_id,
                    }),
                )
            }
            LogEntry::Assistant {
                message,
                timestamp,
                parent_tool_use_id,
                ..
            } => {
                if self.model.is_none() {
                    self.model.clone_from(&message.model);
                }
                (
                    timestamp,
                    EventKind::Message(Message {
                        role: Role::Assistant,
                        id: message.id,
                        parts: parts(message.content),
                        model: message.model,
                        usage: message.usage.map(usage_of),
                        parent_tool_use_id,
                    }),
                )
            }
            LogEntry::Summary { summary } => {
                if self.summary.is_none() {
                    self.summary = Some(summary.clone());
                }
                (None, title(TitleKind::Summary, summary))
            }
            LogEntry::AiTitle { ai_title } => {
                let trimmed = ai_title.trim();
                if !trimmed.is_empty() {
                    self.summary = Some(trimmed.to_owned());
                }
                (None, title(TitleKind::Generated, ai_title))
            }
            LogEntry::CustomTitle { custom_title } => {
                let trimmed = custom_title.trim();
                self.custom_title = (!trimmed.is_empty()).then(|| trimmed.to_owned());
                (None, title(TitleKind::Custom, custom_title))
            }
            LogEntry::Progress { data, .. } => (None, subagent(&data)),
            LogEntry::FileHistorySnapshot { .. } => return None,
            LogEntry::System { subtype, extra, .. } if subtype == "local_command" => {
                let timestamp = extra
                    .get("timestamp")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                (timestamp, local_command(&extra))
            }
            LogEntry::System { .. }
            | LogEntry::AgentName { .. }
            | LogEntry::PermissionMode { .. }
            | LogEntry::Unknown => (None, EventKind::Other),
        };
        Some(Event {
            origin,
            timestamp: timestamp.as_deref().and_then(parse_timestamp),
            visibility,
            kind,
        })
    }
}

/// Explicit human authorship wins over conflicting origin signals, except
/// compaction summaries, which can retain the origin of the summarized turn.
fn generated_user(provenance: &UserProvenance) -> Option<bool> {
    if provenance.is_compact_summary.as_bool() == Some(true) {
        return Some(true);
    }
    let kind = provenance
        .origin
        .get("kind")
        .and_then(serde_json::Value::as_str);
    let turn = provenance.turn_origin.as_str();
    if kind == Some("human") || turn == Some("human") {
        return Some(false);
    }
    if matches!(
        kind,
        Some("task-notification" | "peer" | "coordinator" | "auto-continuation")
    ) || matches!(turn, Some("task_notification" | "auto_continuation"))
    {
        return Some(true);
    }
    // Unrecognized provenance is not evidence of generated authorship.
    if kind.is_some() || turn.is_some() {
        return Some(false);
    }
    None
}

/// Legacy task notifications occupy a complete text part and identify a task.
/// Inline mentions, quoted examples and incomplete wrappers remain readable.
fn task_notification(text: &str) -> bool {
    text.trim()
        .strip_prefix("<task-notification>")
        .and_then(|text| text.strip_suffix("</task-notification>"))
        .is_some_and(|body| {
            body.trim_start().starts_with("<task-id>")
                && body.contains("</task-id>")
                && !body.contains("</task-notification>")
        })
}

/// Opens what Claude Code writes, ahead of the person's question, as the
/// first message of a `/btw` fork.
const FORK_BOILERPLATE: (&str, &str) = ("<fork-boilerplate>", "</fork-boilerplate>");
const FORK_DIRECTIVE: &str = "Your directive:";
/// How Claude Code hands an agent a message the person sent mid-turn.
const MID_TURN_OPEN: &str = "The user sent a new message while you were working:";
const MID_TURN_CLOSE: &str =
    "\n\nThis is how Claude Code surfaces messages the user sends mid-turn";

/// What the person wrote in a user text that Claude Code framed for an
/// agent: the question after a fork's boilerplate, or a message sent
/// mid-turn without its wrapper. `None` for other texts.
fn unframed(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    let typed = if let Some(rest) = trimmed.strip_prefix(FORK_BOILERPLATE.0) {
        let (_, rest) = rest.split_once(FORK_BOILERPLATE.1)?;
        let rest = rest.trim_start();
        rest.strip_prefix(FORK_DIRECTIVE).unwrap_or(rest)
    } else {
        let rest = trimmed.strip_prefix(MID_TURN_OPEN)?;
        rest.rfind(MID_TURN_CLOSE).map_or(rest, |end| &rest[..end])
    };
    Some(typed.trim().to_owned())
}

/// A `/btw` side question, which Claude Code records as a local command;
/// other local commands are bookkeeping.
fn local_command(extra: &serde_json::Value) -> EventKind {
    let content = extra
        .get("content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    match crate::history::extract_skill_preview(content) {
        Some(command) => match command.strip_prefix("/btw ") {
            Some(question) => EventKind::Note {
                label: "/btw".to_owned(),
                text: question.trim().to_owned(),
                searchable: true,
                usage: None,
            },
            None => EventKind::Other,
        },
        None => EventKind::Other,
    }
}

fn title(kind: TitleKind, text: String) -> EventKind {
    EventKind::Title { kind, text }
}

/// The subagent message a progress record reports, if it reports one.
fn subagent(data: &serde_json::Value) -> EventKind {
    let Some(progress) = crate::claude::parse_agent_progress(data) else {
        return EventKind::Other;
    };
    let role = match progress.message.message_type.as_str() {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return EventKind::Other,
    };
    let AgentContent::Blocks(blocks) = progress.message.message.content;
    EventKind::Subagent {
        agent_id: progress.agent_id,
        message: Message {
            role,
            id: None,
            parts: parts(blocks),
            model: None,
            usage: None,
            parent_tool_use_id: None,
        },
    }
}

fn parts(blocks: Vec<ContentBlock>) -> Vec<Part> {
    blocks
        .into_iter()
        .map(|block| match block {
            ContentBlock::Text { text } => Part::Text(text),
            ContentBlock::ToolUse { id, name, input } => Part::ToolCall { id, name, input },
            ContentBlock::ToolResult {
                tool_use_id,
                content,
            } => Part::ToolResult {
                call_id: tool_use_id,
                output: content,
            },
            ContentBlock::Thinking { thinking, .. } => Part::Thinking(thinking),
            ContentBlock::Image { .. } => Part::Image,
            ContentBlock::Other => Part::Other,
        })
        .collect()
}

fn usage_of(usage: TokenUsage) -> Usage {
    Usage {
        input: usage.input_tokens,
        output: usage.output_tokens,
        cache_creation: usage.cache_creation_input_tokens,
        cache_read: usage.cache_read_input_tokens,
    }
}

pub(super) fn parse_timestamp(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_event(metadata: serde_json::Value, content: serde_json::Value) -> Event {
        let mut value = metadata;
        value["type"] = serde_json::json!("user");
        value["message"] = serde_json::json!({"role":"user", "content":content});
        Records::default()
            .push(serde_json::from_value(value).unwrap(), Origin::default())
            .unwrap()
    }

    #[test]
    fn generated_user_provenance_filters_text_but_preserves_tool_results() {
        use serde_json::json;
        for metadata in [
            json!({"origin":{"kind":"task-notification"}}),
            json!({"origin":{"kind":"peer"}}),
            json!({"origin":{"kind":"coordinator"}}),
            json!({"origin":{"kind":"auto-continuation"}}),
            json!({"turnOrigin":"task_notification"}),
            json!({"turnOrigin":"auto_continuation"}),
            json!({"isCompactSummary":true,"turnOrigin":"human"}),
        ] {
            for content in [
                json!("generated sentinel"),
                json!([{"type":"text","text":"generated sentinel"}]),
            ] {
                assert_eq!(
                    user_event(metadata.clone(), content).visibility,
                    Visibility::Injected
                );
            }
            let event = user_event(
                metadata,
                json!([
                    {"type":"text","text":"generated sentinel"},
                    {"type":"tool_result","tool_use_id":"t1","content":"preserved output"}
                ]),
            );
            assert_eq!(event.visibility, Visibility::Dialogue);
            let EventKind::Message(message) = event.kind else {
                panic!("message")
            };
            assert!(matches!(
                message.parts.as_slice(),
                [Part::ToolResult { .. }]
            ));
        }
    }

    #[test]
    fn human_unknown_and_legacy_records_remain_readable() {
        use serde_json::json;
        let notification = "<task-notification>\n<task-id>t1</task-id>done</task-notification>";
        for metadata in [
            json!({"origin":{"kind":"human"},"isMeta":true}),
            json!({"origin":{"kind":"human"},"turnOrigin":"task_notification"}),
            json!({"origin":{"kind":"task-notification"},"turnOrigin":"human"}),
            json!({"origin":{"kind":"future-origin"}}),
        ] {
            assert_eq!(
                user_event(metadata, json!(notification)).visibility,
                Visibility::Dialogue
            );
        }
        for metadata in [
            json!({}),
            json!({"promptSource":"queued"}),
            json!({"promptSource":"sdk"}),
            json!({"promptSource":"system","isMeta":true}),
            json!({"origin":42,"turnOrigin":{}}),
        ] {
            assert_eq!(
                user_event(metadata, json!("human prompt")).visibility,
                Visibility::Dialogue
            );
        }
        assert_eq!(
            user_event(json!({}), json!(notification)).visibility,
            Visibility::Injected
        );
        for text in [
            "explain <task-notification><task-id>t1</task-id></task-notification>",
            "<task-notification><task-id>t1</task-id>",
            "<task-notification>example</task-notification>",
            "<task-notification><task-id>t1</task-id></task-notification> keep this",
        ] {
            assert_eq!(
                user_event(json!({}), json!(text)).visibility,
                Visibility::Dialogue
            );
        }
        let event = user_event(
            json!({}),
            json!([
                {"type":"text","text":notification},
                {"type":"text","text":"also check this"}
            ]),
        );
        let EventKind::Message(message) = event.kind else {
            panic!("message")
        };
        assert_eq!(
            message.parts,
            vec![Part::Text("also check this".to_owned())]
        );
    }

    #[test]
    fn notification_filtering_agrees_across_views_search_and_agent_ordinals() {
        let lines = [
            r#"{"type":"user","timestamp":"2026-10-15T14:00:00Z","message":{"role":"user","content":"human question"}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"first answer"}]}}"#,
            r#"{"type":"user","origin":{"kind":"task-notification"},"timestamp":"2026-10-15T15:00:00Z","message":{"role":"user","content":"notification sentinel"}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"continued answer"}]}}"#,
        ].join("\n");
        let transcript =
            decode_test_reader(Path::new("/test/session.jsonl"), lines.as_bytes()).unwrap();
        assert_eq!(crate::history::view::entries(&transcript).len(), 3);
        let indexed = crate::history::index::index(&transcript).unwrap();
        assert!(!indexed.full_text.contains("notification sentinel"));
        assert!(!indexed.own_text.contains("notification sentinel"));
        assert_eq!(indexed.prompt_count, 1);
        let agent =
            crate::agent::transcript::AgentTranscript::from_transcript(&transcript).unwrap();
        assert_eq!(agent.messages.len(), 3);
        assert_eq!(
            agent
                .messages
                .iter()
                .map(|message| message.ordinal)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(
            indexed.semantic_turn_ranges,
            (1..=3)
                .map(crate::agent::refs::MessageRange::single)
                .collect::<Vec<_>>()
        );
        assert!(
            indexed
                .semantic_turns
                .iter()
                .all(|text| !text.contains("notification sentinel"))
        );
        assert_eq!(
            transcript.meta.updated_at,
            parse_timestamp("2026-10-15T14:00:00Z")
        );
    }

    fn decode_with_mtime(lines: &[&str], modified: SystemTime) -> Transcript {
        let path = Path::new("/projects/-work/session.jsonl");
        let key = ConversationKey {
            instance: SourceInstance {
                kind: SourceKind::Claude,
                root: PathBuf::new(),
            },
            native_id: path.to_string_lossy().into_owned(),
        };
        let contents = lines.join("\n");
        decode_reader(key, path, contents.as_bytes(), Some(modified), &mut |_| {}).unwrap()
    }

    #[test]
    fn last_activity_is_the_newest_message_not_the_file_mtime() {
        let modified = SystemTime::now();
        let transcript = decode_with_mtime(
            &[
                r#"{"type":"user","timestamp":"2026-10-15T14:50:00Z","message":{"role":"user","content":"hi"}}"#,
                r#"{"type":"assistant","timestamp":"2026-10-15T14:51:00Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"t","name":"Bash","input":{}}]}}"#,
                r#"{"type":"user","timestamp":"2026-10-15T14:51:56Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}}"#,
                r#"{"type":"system","subtype":"turn_duration","timestamp":"2026-10-15T14:52:30Z"}"#,
                r#"{"type":"system","subtype":"away_summary","timestamp":"2026-10-16T14:44:00Z"}"#,
            ],
            modified,
        );
        assert_eq!(
            transcript.meta.updated_at.unwrap().to_rfc3339(),
            "2026-10-15T14:51:56+00:00"
        );
    }

    #[test]
    fn records_every_distinct_cwd_with_the_first_as_cwd() {
        let transcript = decode_with_mtime(
            &[
                r#"{"type":"user","cwd":"/work","message":{"role":"user","content":"a"}}"#,
                r#"{"type":"user","cwd":"/work/repo","message":{"role":"user","content":"b"}}"#,
                r#"{"type":"user","cwd":"/work","message":{"role":"user","content":"c"}}"#,
                r#"{"type":"user","message":{"role":"user","content":"d"}}"#,
            ],
            SystemTime::now(),
        );
        assert_eq!(transcript.meta.cwd, Some(PathBuf::from("/work")));
        assert_eq!(
            transcript.meta.cwds,
            vec![PathBuf::from("/work"), PathBuf::from("/work/repo")]
        );
    }

    #[test]
    fn a_session_without_message_times_was_last_active_at_its_mtime() {
        let modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let transcript = decode_with_mtime(
            &[r#"{"type":"user","message":{"role":"user","content":"hi"}}"#],
            modified,
        );
        assert_eq!(
            transcript.meta.updated_at,
            Some(DateTime::<Utc>::from(modified))
        );
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    const PROMPT: &str = r#"{"type":"user","timestamp":"2026-10-15T14:50:00Z","cwd":"/work","message":{"role":"user","content":"hi"}}"#;
    const FORK_META: &str = r#"{"agentType":"fork","isFork":true,"description":"Ask the narwhal"}"#;

    #[test]
    fn sidecars_sit_in_the_subagents_directory_of_a_session() {
        let path = Path::new("/p/-work/s1/subagents/agent-a1.jsonl");
        assert_eq!(
            sidecar(path),
            Some(Sidecar {
                project_dir: Path::new("/p/-work"),
                session_id: "s1",
                agent_id: "a1",
            })
        );
        assert_eq!(
            parent_transcript(path),
            Some(PathBuf::from("/p/-work/s1.jsonl"))
        );
        assert_eq!(project_dir(path), Some(Path::new("/p/-work")));
        assert_eq!(
            project_dir(Path::new("/p/-work/s1.jsonl")),
            Some(Path::new("/p/-work"))
        );
        for other in [
            "/p/-work/agent-a1.jsonl",
            "/p/-work/s1/other/agent-a1.jsonl",
            "/p/-work/s1/subagents/agent-a1.meta.json",
            "/p/-work/s1/subagents/agent-aprompt_suggestion-1.jsonl",
            "/p/-work/s1/subagents/agent-acompact-1.jsonl",
        ] {
            assert_eq!(sidecar(Path::new(other)), None, "{other}");
        }
    }

    #[test]
    fn discovery_lists_sidecars_and_folds_their_metadata_into_the_stamp() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("-work");
        write(&project.join("s1.jsonl"), PROMPT);
        let fork = project.join("s1/subagents/agent-afork-1.jsonl");
        write(&fork, PROMPT);
        write(
            &project.join("s1/subagents/agent-aprompt_suggestion-2.jsonl"),
            PROMPT,
        );
        write(&project.join("s1/tool-results/out.txt"), "x");
        write(&project.join("agent-old.jsonl"), PROMPT);
        let old = SystemTime::now() - std::time::Duration::from_secs(3600);
        let settle = |path: &Path| {
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(old)
                .unwrap();
        };
        settle(&fork);
        let adapter = ClaudeAdapter::new(root.path().to_path_buf());
        let mut found: Vec<PathBuf> = adapter
            .discover(SystemTime::now())
            .found
            .into_iter()
            .map(|located| located.path)
            .collect();
        found.sort();
        assert_eq!(found, vec![fork.clone(), project.join("s1.jsonl")]);

        let native_id = "-work/s1/subagents/agent-afork-1.jsonl";
        let before = adapter.resolve(native_id, SystemTime::now()).unwrap().stamp;
        let meta = fork.with_extension("meta.json");
        write(&meta, FORK_META);
        settle(&meta);
        let after = adapter.resolve(native_id, SystemTime::now()).unwrap().stamp;
        assert!(after.is_known());
        assert_eq!(after.size, before.size + FORK_META.len() as u64);
    }

    #[test]
    fn a_user_fork_branches_off_its_session_and_a_subagent_is_its_child() {
        let root = tempfile::tempdir().unwrap();
        let sidecars = root.path().join("-work/s1/subagents");
        let fork = sidecars.join("agent-afork-1.jsonl");
        let agent_fork = sidecars.join("agent-a2.jsonl");
        let subagent = sidecars.join("agent-a3.jsonl");
        let contents = format!(
            "{}\n{PROMPT}\n",
            r#"{"type":"fork-context-ref","agentId":"afork-1","parentSessionId":"s1"}"#
        );
        write(&fork, &contents);
        write(&fork.with_extension("meta.json"), FORK_META);
        write(&agent_fork, PROMPT);
        write(
            &agent_fork.with_extension("meta.json"),
            r#"{"agentType":"fork","isFork":true,"toolUseId":"toolu_1"}"#,
        );
        write(&subagent, PROMPT);
        write(
            &subagent.with_extension("meta.json"),
            r#"{"agentType":"Explore","description":"Find the walrus","toolUseId":"toolu_2"}"#,
        );
        let adapter = ClaudeAdapter::new(root.path().to_path_buf());
        let decode = |path: &Path| {
            let native_id = path.strip_prefix(root.path()).unwrap().to_string_lossy();
            let located = adapter.resolve(&native_id, SystemTime::now()).unwrap();
            adapter.decode(&located).unwrap().unwrap()
        };

        let transcript = decode(&fork);
        assert_eq!(
            transcript.relationship,
            Relationship::Branch {
                parent: "s1".to_owned()
            }
        );
        assert_eq!(transcript.meta.session_id, "afork-1");
        assert_eq!(transcript.meta.summary.as_deref(), Some("Ask the narwhal"));
        assert_eq!(
            decode(&agent_fork).relationship,
            Relationship::Child {
                parent: "s1".to_owned()
            }
        );
        let transcript = decode(&subagent);
        assert_eq!(
            transcript.relationship,
            Relationship::Child {
                parent: "s1".to_owned()
            }
        );
        assert_eq!(
            transcript.meta.summary.as_deref(),
            Some("Explore: Find the walrus")
        );
    }

    #[test]
    fn framed_user_texts_read_as_what_the_person_wrote() {
        assert_eq!(
            unframed(
                "<fork-boilerplate>\nYou are a forked agent.\n</fork-boilerplate>\n\nYour directive: why is the sky blue?"
            )
            .as_deref(),
            Some("why is the sky blue?")
        );
        assert_eq!(
            unframed(
                "The user sent a new message while you were working:\nalso check tides\n\nThis is how Claude Code surfaces messages the user sends mid-turn. Address it."
            )
            .as_deref(),
            Some("also check tides")
        );
        assert_eq!(unframed("plain question"), None);
    }

    #[test]
    fn a_btw_question_is_a_searchable_note_and_other_local_commands_are_not() {
        let entry = |content: &str| -> LogEntry {
            serde_json::from_value(serde_json::json!({
                "type": "system",
                "subtype": "local_command",
                "timestamp": "2026-10-15T14:50:00Z",
                "content": content,
            }))
            .unwrap()
        };
        let mut records = Records::default();
        let event = records
            .push(
                entry("<command-name>/btw</command-name>\n<command-message>btw</command-message>\n<command-args>is the walrus fed?</command-args>"),
                Origin::default(),
            )
            .unwrap();
        assert_eq!(
            event.kind,
            EventKind::Note {
                label: "/btw".to_owned(),
                text: "is the walrus fed?".to_owned(),
                searchable: true,
                usage: None,
            }
        );
        assert!(event.timestamp.is_some());
        for content in [
            "<command-name>/model</command-name>\n<command-args>opus</command-args>",
            "<local-command-stdout>Set model</local-command-stdout>",
        ] {
            let event = records.push(entry(content), Origin::default()).unwrap();
            assert_eq!(event.kind, EventKind::Other, "{content}");
        }
    }

    #[test]
    fn sidecars_resume_their_session_and_cannot_be_forked() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("-work/s1.jsonl");
        let fork = root.path().join("-work/s1/subagents/agent-afork-1.jsonl");
        write(&fork, PROMPT);
        let target = Target {
            path: &fork,
            project: None,
        };
        let cwd = root.path().join("elsewhere");
        let launch = Launch {
            launcher: Launcher::default_for(SourceKind::Claude),
            default_args: Vec::new(),
        };
        assert!(resume_plan(target, &cwd, false, &launch).is_err());

        write(&parent, PROMPT);
        let resume = resume_plan(target, &cwd, false, &launch).unwrap();
        let copy = resume.copy.unwrap();
        assert_eq!(copy.transcript, parent);
        assert_eq!(copy.session_id, "s1");
        let args: Vec<_> = resume.command.get_args().collect();
        assert_eq!(args, ["--resume", "s1"]);

        let adapter = ClaudeAdapter::new(root.path().to_path_buf());
        let branch = Provenance {
            branch: true,
            ..Provenance::default()
        };
        let capabilities = adapter.capabilities_for(branch);
        assert!(capabilities.resume && capabilities.delete);
        assert!(!capabilities.fork && !capabilities.rename);
        assert_eq!(
            adapter.capabilities_for(Provenance::default()),
            Capabilities::ALL
        );
    }

    #[test]
    fn deleting_a_session_keeps_its_forks_and_deleting_a_sidecar_keeps_its_session() {
        let root = tempfile::tempdir().unwrap();
        let session_dir = root.path().join("-work/s1");
        let sidecars = session_dir.join("subagents");
        let fork = sidecars.join("agent-afork-1.jsonl");
        let subagent = sidecars.join("agent-a2.jsonl");
        write(&fork, PROMPT);
        write(&fork.with_extension("meta.json"), FORK_META);
        write(&subagent, PROMPT);
        write(
            &subagent.with_extension("meta.json"),
            r#"{"agentType":"Explore"}"#,
        );
        write(&session_dir.join("tool-results/out.txt"), "x");

        remove_session_dir(&session_dir).unwrap();
        let mut left: Vec<PathBuf> = fs::read_dir(&sidecars)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        left.sort();
        assert_eq!(left, vec![fork.clone(), fork.with_extension("meta.json")]);
        assert!(!session_dir.join("tool-results").exists());

        delete_sidecar(&fork).unwrap();
        assert!(fs::read_dir(&sidecars).unwrap().next().is_none());

        write(&subagent, PROMPT);
        remove_session_dir(&session_dir).unwrap();
        assert!(!session_dir.exists());
    }

    #[test]
    fn resume_runs_the_configured_command() {
        let cwd = tempfile::tempdir().unwrap();
        let selected_path = crate::history::get_claude_projects_dir(cwd.path())
            .unwrap()
            .join("12345678-1234-4234-9234-123456789abc.jsonl");
        let target = Target {
            path: &selected_path,
            project: Some(cwd.path()),
        };
        let launch = Launch {
            launcher: Launcher::configured(
                &[
                    "nix".to_owned(),
                    "run".to_owned(),
                    ".#claude".to_owned(),
                    "--".to_owned(),
                ],
                "[resume]",
            ),
            default_args: vec!["--verbose".to_owned()],
        };

        let resume = resume_plan(target, cwd.path(), true, &launch).unwrap();

        assert!(resume.copy.is_none());
        assert_eq!(resume.command.get_program(), "nix");
        assert_eq!(
            resume.command.get_args().collect::<Vec<_>>(),
            [
                "run",
                ".#claude",
                "--",
                "--resume",
                "12345678-1234-4234-9234-123456789abc",
                "--fork-session",
                "--verbose"
            ]
        );
    }

    #[test]
    fn resume_action_uses_cwd_when_it_maps_to_selected_project_dir() {
        let cwd = tempfile::tempdir().unwrap();
        let stale_project = tempfile::tempdir().unwrap();
        let stale_project_path = stale_project.path().to_path_buf();
        let selected_path = crate::history::get_claude_projects_dir(cwd.path())
            .unwrap()
            .join("12345678-1234-4234-9234-123456789abc.jsonl");

        let action = resolve_resume_action(
            &selected_path,
            Some(stale_project_path.as_path()),
            cwd.path(),
            false,
        )
        .unwrap();

        assert_eq!(
            action,
            ResumeAction::Run {
                current_dir: cwd.path().to_path_buf()
            }
        );
    }

    #[test]
    fn resume_action_uses_project_path_when_it_maps_to_selected_project_dir() {
        let cwd = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let project_path = project.path().to_path_buf();
        let selected_path = crate::history::get_claude_projects_dir(project.path())
            .unwrap()
            .join("12345678-1234-4234-9234-123456789abc.jsonl");

        let action = resolve_resume_action(
            &selected_path,
            Some(project_path.as_path()),
            cwd.path(),
            false,
        )
        .unwrap();

        assert_eq!(
            action,
            ResumeAction::Run {
                current_dir: project.path().to_path_buf()
            }
        );
    }

    #[test]
    fn resume_action_copies_selected_transcript_when_project_path_maps_elsewhere() {
        let cwd = tempfile::tempdir().unwrap();
        let selected_project = tempfile::tempdir().unwrap();
        let stale_project = tempfile::tempdir().unwrap();
        let stale_project_path = stale_project.path().to_path_buf();
        let selected_path = crate::history::get_claude_projects_dir(selected_project.path())
            .unwrap()
            .join("12345678-1234-4234-9234-123456789abc.jsonl");
        let cwd_projects_dir = crate::history::get_claude_projects_dir(cwd.path()).unwrap();

        let action = resolve_resume_action(
            &selected_path,
            Some(stale_project_path.as_path()),
            cwd.path(),
            false,
        )
        .unwrap();

        assert_eq!(action, ResumeAction::CopyToCurrent { cwd_projects_dir });
    }
}
