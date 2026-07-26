use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_claude-history"))
}

fn run(config: &Path, args: &[&str]) -> Output {
    Command::new(binary())
        .env("CLAUDE_CONFIG_DIR", config)
        .args(args)
        .output()
        .expect("run claude-history")
}

fn project(config: &Path) -> PathBuf {
    let project = config.join("projects").join("-tmp-agent-phase3-tests");
    std::fs::create_dir_all(&project).expect("create project");
    project
}

fn write_transcript(path: &Path, needle: &str) {
    write_transcript_at(path, needle, "2026-07-20");
}

/// Backdate a transcript's modification time.
///
/// Conversation timestamps come from the file's mtime (see
/// `history::parser`), not from the records inside it, so time filtering can
/// only be exercised by changing the mtime. `stamp` is `YYYYMMDDhhmm`.
fn set_modified(path: &Path, stamp: &str) {
    let status = Command::new("touch")
        .args(["-t", stamp])
        .arg(path)
        .status()
        .expect("run touch");
    assert!(status.success(), "touch -t {stamp} failed");
}

fn write_transcript_at(path: &Path, needle: &str, date: &str) {
    let user = serde_json::json!({
        "type": "user",
        "timestamp": format!("{date}T00:00:00Z"),
        "cwd": "/tmp/agent-phase3-tests",
        "message": {"role": "user", "content": needle}
    });
    let assistant = serde_json::json!({
        "type": "assistant",
        "timestamp": format!("{date}T00:00:01Z"),
        "message": {"role": "assistant", "content": [{"type": "text", "text": "answer"}]}
    });
    std::fs::write(path, format!("{user}\n{assistant}\n")).expect("write transcript");
}

fn first_ref(output: &[u8]) -> String {
    String::from_utf8_lossy(output)
        .split_whitespace()
        .find_map(|field| field.strip_prefix("ref="))
        .expect("search ref")
        .trim_end_matches(|character: char| !character.is_ascii_hexdigit())
        .to_string()
}

#[test]
fn malformed_and_missing_refs_have_structured_stderr_and_nonzero_exit() {
    let config = tempfile::tempdir().expect("config");
    project(config.path());

    let invalid = run(config.path(), &["agent", "read", "not-a-ref"]);
    assert!(!invalid.status.success());
    assert!(invalid.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&invalid.stderr)
            .starts_with("protocol agent-error kind=invalid-ref ref=not-a-ref")
    );

    let missing = run(config.path(), &["agent", "read", "ch_12345678"]);
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr)
            .starts_with("protocol agent-error kind=not-found ref=ch_12345678")
    );
}

#[test]
fn target_transcript_and_range_failures_have_precise_kinds() {
    let config = tempfile::tempdir().expect("config");
    let transcript = project(config.path()).join("12345678-1234-4234-9234-123456789abc.jsonl");
    write_transcript(&transcript, "phase three needle");

    let search = run(
        config.path(),
        &["agent", "search", "--lexical", "phase three needle"],
    );
    assert!(
        search.status.success(),
        "{}",
        String::from_utf8_lossy(&search.stderr)
    );
    let reference = first_ref(&search.stdout);

    let range = run(
        config.path(),
        &["agent", "read", &format!("{reference}:m99")],
    );
    assert!(!range.status.success());
    assert!(String::from_utf8_lossy(&range.stderr).starts_with(&format!(
        "protocol agent-error kind=out-of-range ref={reference}"
    )));

    std::fs::write(&transcript, "{malformed\n").expect("malform transcript");
    let malformed = run(config.path(), &["agent", "read", &reference]);
    assert!(!malformed.status.success());
    assert!(
        String::from_utf8_lossy(&malformed.stderr).starts_with(&format!(
            "protocol agent-error kind=malformed-transcript ref={reference}"
        ))
    );
}

#[test]
fn search_reports_partial_warnings_and_preserves_compact_success_output() {
    let config = tempfile::tempdir().expect("config");
    let project = project(config.path());
    write_transcript(
        &project.join("12345678-1234-4234-9234-123456789abc.jsonl"),
        "warning contract needle",
    );
    std::fs::write(
        project.join("87654321-1234-4234-9234-123456789abc.jsonl"),
        "{malformed\n",
    )
    .expect("write malformed transcript");

    let output = run(
        config.path(),
        &["agent", "search", "--lexical", "warning contract needle"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("protocol agent-search mode=lexical"));
    assert!(stdout.contains("protocol agent-warning kind=malformed-transcript ref=ch_"));
    assert!(stdout.contains("read ref=ch_"));
}

#[test]
fn ref_only_commands_parse_only_the_selected_transcript() {
    let config = tempfile::tempdir().expect("config");
    let project = project(config.path());
    let selected = project.join("12345678-1234-4234-9234-123456789abc.jsonl");
    write_transcript(&selected, "selected transcript needle");

    let search = run(
        config.path(),
        &["agent", "search", "--lexical", "selected transcript needle"],
    );
    assert!(search.status.success());
    let reference = first_ref(&search.stdout);
    std::fs::write(
        project.join("87654321-1234-4234-9234-123456789abc.jsonl"),
        "{malformed\n",
    )
    .expect("write unrelated malformed transcript");

    let outline = run(config.path(), &["agent", "outline", &reference]);

    assert!(
        outline.status.success(),
        "{}",
        String::from_utf8_lossy(&outline.stderr)
    );
    assert!(outline.stderr.is_empty());
    let stdout = String::from_utf8_lossy(&outline.stdout);
    assert!(stdout.contains("m1 role=user"));
    assert!(stdout.contains("m2 role=assistant"));
    assert!(!stdout.contains("malformed-transcript"));
}

#[test]
fn selected_partial_transcript_recovers_records_and_reports_warning() {
    let config = tempfile::tempdir().expect("config");
    let transcript = project(config.path()).join("12345678-1234-4234-9234-123456789abc.jsonl");
    write_transcript(&transcript, "partial transcript needle");
    let search = run(
        config.path(),
        &["agent", "search", "--lexical", "partial transcript needle"],
    );
    assert!(search.status.success());
    let reference = first_ref(&search.stdout);
    let content = std::fs::read_to_string(&transcript).expect("read transcript");
    let (first, second) = content.split_once('\n').expect("two records");
    std::fs::write(&transcript, format!("{first}\n{{malformed\n{second}"))
        .expect("write partial transcript");

    let recovered_search = run(
        config.path(),
        &["agent", "search", "--lexical", "partial transcript needle"],
    );
    assert!(recovered_search.status.success());
    let search_stdout = String::from_utf8_lossy(&recovered_search.stdout);
    assert!(search_stdout.contains("focus=m1..m1"));
    assert!(search_stdout.contains("kind=malformed-transcript"));

    let within = run(
        config.path(),
        &[
            "agent",
            "within",
            &reference,
            "partial transcript needle",
            "--lexical",
        ],
    );
    assert!(within.status.success());
    assert!(String::from_utf8_lossy(&within.stdout).contains("focus=m1..m1"));

    let read = run(
        config.path(),
        &["agent", "read", &format!("{reference}:m1")],
    );
    assert!(read.status.success());
    assert!(String::from_utf8_lossy(&read.stdout).contains("partial transcript needle"));

    let outline = run(config.path(), &["agent", "outline", &reference]);

    assert!(outline.status.success());
    let stdout = String::from_utf8_lossy(&outline.stdout);
    assert!(stdout.contains("warnings=1"));
    assert!(stdout.contains("kind=malformed-transcript"));
    assert!(stdout.contains("m1 role=user"));
    assert!(stdout.contains("m2 role=assistant"));

    let bounded = run(
        config.path(),
        &["agent", "read", &reference, "--budget", "180"],
    );
    assert!(bounded.status.success());
    let bounded_stdout = String::from_utf8_lossy(&bounded.stdout);
    assert!(bounded_stdout.chars().count() <= 180);
    assert!(bounded_stdout.contains("warnings=1"));
    assert!(bounded_stdout.contains("continue read"));
    assert_eq!(bounded_stdout.lines().count(), 2);
}

#[test]
fn agent_filesystem_failures_use_io_envelope() {
    let config = tempfile::tempdir().expect("config");
    std::fs::write(config.path().join("projects"), "not a directory").expect("write projects file");

    let output = run(config.path(), &["agent", "search", "--lexical", "needle"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).starts_with("protocol agent-error kind=io"));
}

#[test]
fn search_time_range_narrows_the_corpus_without_reporting_skips() {
    let config = tempfile::tempdir().expect("config");
    let project = project(config.path());

    let recent = project.join("11111111-1111-4111-9111-111111111111.jsonl");
    let old = project.join("22222222-2222-4222-9222-222222222222.jsonl");
    write_transcript_at(&recent, "time filter needle", "2026-07-20");
    write_transcript_at(&old, "time filter needle", "2020-01-15");
    set_modified(&recent, "202607200000");
    set_modified(&old, "202001150000");

    let unfiltered = run(
        config.path(),
        &["agent", "search", "--lexical", "time filter needle"],
    );
    assert!(
        unfiltered.status.success(),
        "{}",
        String::from_utf8_lossy(&unfiltered.stderr)
    );
    let unfiltered_stdout = String::from_utf8_lossy(&unfiltered.stdout);
    assert!(unfiltered_stdout.contains("uuid=11111111"));
    assert!(unfiltered_stdout.contains("uuid=22222222"));

    let filtered = run(
        config.path(),
        &[
            "agent",
            "search",
            "--lexical",
            "time filter needle",
            "--since",
            "2026-01-01",
        ],
    );
    assert!(
        filtered.status.success(),
        "{}",
        String::from_utf8_lossy(&filtered.stderr)
    );
    let filtered_stdout = String::from_utf8_lossy(&filtered.stdout);
    assert!(filtered_stdout.contains("uuid=11111111"));
    assert!(
        !filtered_stdout.contains("uuid=22222222"),
        "out-of-window conversation still returned: {filtered_stdout}"
    );

    // Key discovery walks the projects directory independently of the time
    // filter, so an unfiltered key list would report every excluded
    // conversation as a skipped transcript and claim partial coverage.
    assert!(
        !filtered_stdout.contains("kind=skipped"),
        "filtered-out conversations were reported as skipped: {filtered_stdout}"
    );

    // The converse: narrowing the key list must not hide diagnostics for files
    // that are inside the window but failed to parse, or a filtered search would
    // claim full coverage it does not have.
    let unparseable = project.join("33333333-3333-4333-9333-333333333333.jsonl");
    std::fs::write(&unparseable, "{malformed\n").expect("write malformed transcript");
    set_modified(&unparseable, "202607200000");

    let with_malformed = run(
        config.path(),
        &[
            "agent",
            "search",
            "--lexical",
            "time filter needle",
            "--since",
            "2026-01-01",
        ],
    );
    assert!(with_malformed.status.success());
    let with_malformed_stdout = String::from_utf8_lossy(&with_malformed.stdout);
    assert!(
        with_malformed_stdout.contains("kind=malformed-transcript"),
        "in-window malformed transcript was silently dropped: {with_malformed_stdout}"
    );
}

#[test]
fn search_rejects_an_inverted_time_range() {
    let config = tempfile::tempdir().expect("config");
    let transcript = project(config.path()).join("33333333-3333-4333-9333-333333333333.jsonl");
    write_transcript(&transcript, "inverted range needle");

    let output = run(
        config.path(),
        &[
            "agent",
            "search",
            "--lexical",
            "inverted range needle",
            "--after",
            "2026-07-20",
            "--before",
            "2026-01-01",
        ],
    );

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .starts_with("protocol agent-error kind=out-of-range"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
