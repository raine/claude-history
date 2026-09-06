//! The annotator contract, exercised against fixture annotators in
//! `tests/fixtures/annotators`.
//!
//! Each test names the property it holds. The fixtures stand in for any tool
//! implementing the contract, so the suite runs without an installed
//! annotator.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_claude-history"))
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/annotators")
        .join(name)
}

/// A home directory holding a claude config root, a transcript, and the
/// annotator registrations under test.
struct Harness {
    home: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("temp home");
        let project = home.path().join("claude/projects/-tmp-contract");
        std::fs::create_dir_all(&project).expect("create project");
        std::fs::write(
            project.join("11111111-1111-4111-8111-111111111111.jsonl"),
            concat!(
                r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"transcript body"}]},"timestamp":"2026-09-01T10:00:00.000Z","sessionId":"11111111-1111-4111-8111-111111111111","cwd":"/tmp/contract"}"#,
                "\n",
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"reply body"}]},"timestamp":"2026-09-01T10:00:05.000Z","sessionId":"11111111-1111-4111-8111-111111111111","cwd":"/tmp/contract"}"#,
                "\n",
            ),
        )
        .expect("write transcript");
        Self { home }
    }

    fn transcript(&self) -> PathBuf {
        self.home
            .path()
            .join("claude/projects/-tmp-contract/11111111-1111-4111-8111-111111111111.jsonl")
    }

    /// Register one annotator as the write target.
    fn register(&self, key: &str, script: &str) {
        let config = self.home.path().join(".config/claude-history");
        std::fs::create_dir_all(&config).expect("create config dir");
        std::fs::write(
            config.join("config.toml"),
            format!(
                "[annotations]\nwrite_to = \"{key}\"\n\n[annotators.{key}]\ncommand = \"{}\"\n",
                fixture(script).display()
            ),
        )
        .expect("write config");
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_logged(args, None)
    }

    fn run_logged(&self, args: &[&str], call_log: Option<&Path>) -> Output {
        let mut command = Command::new(binary());
        command
            .env("HOME", self.home.path())
            .env("CLAUDE_CONFIG_DIR", self.home.path().join("claude"))
            .env(
                "PI_CODING_AGENT_SESSION_DIR",
                self.home.path().join("empty-agent-sessions"),
            )
            .args(args);
        if let Some(path) = call_log {
            command.env("ANNOTATOR_CALL_LOG", path);
        }
        command.output().expect("run claude-history")
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_note_held_only_by_an_annotator_is_found_by_search() {
    let harness = Harness::new();
    harness.register("fixture", "conforming.sh");

    let output = harness.run(&["agent", "search", "pelican", "--mode", "lexical"]);
    let text = stdout(&output);

    // The word appears in no transcript, so the hit exists only because the
    // annotator was read and its text reached the lexical fields.
    assert!(text.contains("hits=1"), "{text}");
    assert!(text.contains("source=annotation"), "{text}");
    assert!(text.contains("pelican crossing from the fixture"), "{text}");
}

#[test]
fn a_write_reports_the_id_the_annotator_stored_under() {
    let harness = Harness::new();
    harness.register("fixture", "conforming.sh");
    let transcript = harness.transcript();

    let output = harness.run(&[
        "annotate",
        transcript.to_str().unwrap(),
        "--text",
        "written through the contract",
        "--line",
        "2",
    ]);
    let text = stdout(&output);

    // The id is the annotator's, not the one claude-history supplied: a later
    // delete names what the store holds.
    assert!(text.contains("fixture_written"), "{text}");
}

#[test]
fn a_delete_reaches_the_annotator_holding_the_note() {
    let harness = Harness::new();
    harness.register("fixture", "conforming.sh");
    let transcript = harness.transcript();

    let output = harness.run(&[
        "annotate",
        transcript.to_str().unwrap(),
        "--delete",
        "fixture_1",
    ]);
    let text = stdout(&output);

    assert!(text.contains("removed fixture_1"), "{text}");
}

#[test]
fn a_query_invokes_an_annotator_once() {
    let harness = Harness::new();
    harness.register("fixture", "conforming.sh");
    let log = harness.home.path().join("calls.log");

    harness.run_logged(
        &["agent", "search", "pelican", "--mode", "lexical"],
        Some(&log),
    );

    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    let reads = calls.lines().filter(|line| *line == "read").count();
    // One invocation carries every conversation in scope. One per conversation
    // would spawn a subprocess per row of a real history.
    assert_eq!(reads, 1, "{calls}");
}

/// A note naming a conversation outside the request is dropped by
/// `CommandAnnotator::read`, covered by
/// `annotations::command_annotator::tests::a_conversation_outside_the_request_is_dropped`.
/// It is not asserted here: the search path matches annotations to loaded
/// conversations by path and drops an unknown one again, so this suite does
/// not separate the filter working from the filter absent.
#[test]
fn an_annotator_returning_beyond_the_request_still_covers_the_conversations_requested() {
    let harness = Harness::new();
    harness.register("trespasser", "foreign.sh");

    let output = harness.run(&["agent", "search", "avocet", "--mode", "lexical"]);
    let text = stdout(&output);

    assert!(
        text.contains("avocet note for a requested conversation"),
        "{text}"
    );
}

#[test]
fn an_annotator_that_fails_leaves_the_conversation_searchable() {
    let harness = Harness::new();
    harness.register("broken", "failing.sh");

    let output = harness.run(&["agent", "search", "transcript", "--mode", "lexical"]);
    let text = stdout(&output);

    assert!(output.status.success(), "{text}");
    assert!(text.contains("hits=1"), "{text}");
}

#[test]
fn notes_in_the_file_annotator_stay_readable_once_another_is_registered() {
    let harness = Harness::new();
    let sidecar_dir = harness
        .home
        .path()
        .join(".local/share/claude-history/annotations/-tmp-contract");
    std::fs::create_dir_all(&sidecar_dir).expect("create sidecar dir");
    std::fs::write(
        sidecar_dir.join("11111111-1111-4111-8111-111111111111.jsonl"),
        "{\"id\":\"older\",\"targets\":[1],\"kind\":\"note\",\"text\":\"kingfisher from before\"}\n",
    )
    .expect("write sidecar");
    harness.register("fixture", "conforming.sh");

    let output = harness.run(&["agent", "search", "kingfisher", "--mode", "lexical"]);
    let text = stdout(&output);

    // Registering an annotator moves nothing: notes written earlier keep being
    // read from where they sit.
    assert!(text.contains("kingfisher from before"), "{text}");
}
