use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SESSION_ID: &str = "11111111-2222-4333-8444-555555555555";

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_claude-history"))
}

/// Run against a throwaway HOME (for `~/.config/claude-history/config.toml`)
/// and a throwaway Claude config dir, so real files are never involved.
fn run(home: &Path, claude: &Path, args: &[&str]) -> Output {
    Command::new(binary())
        .env("HOME", home)
        .env("CLAUDE_CONFIG_DIR", claude)
        .env(
            "PI_CODING_AGENT_SESSION_DIR",
            claude.join("empty-agent-sessions"),
        )
        .args(args)
        .output()
        .expect("run claude-history")
}

fn disable_delete(home: &Path) {
    let config_dir = home.join(".config").join("claude-history");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    std::fs::write(
        config_dir.join("config.toml"),
        "[safety]\ndisable_delete = true\n",
    )
    .expect("write config");
}

/// A transcript with a user message and no assistant reply, which
/// `delete-empty` treats as empty.
fn write_empty_transcript(claude: &Path) -> PathBuf {
    let project = claude.join("projects").join("-tmp-delete-guard");
    std::fs::create_dir_all(&project).expect("create project");
    let path = project.join(format!("{SESSION_ID}.jsonl"));
    let user = serde_json::json!({
        "type": "user",
        "timestamp": "2026-07-20T00:00:00Z",
        "cwd": "/tmp/delete-guard",
        "message": {"role": "user", "content": "/status"}
    });
    std::fs::write(&path, format!("{user}\n")).expect("write transcript");
    path
}

#[test]
fn delete_flag_is_refused_when_disabled() {
    let home = tempfile::tempdir().expect("home");
    let claude = tempfile::tempdir().expect("claude");
    disable_delete(home.path());
    let transcript = write_empty_transcript(claude.path());

    let output = run(home.path(), claude.path(), &["--delete", SESSION_ID]);

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("disable_delete"),
        "{output:?}"
    );
    assert!(transcript.exists());
}

#[test]
fn delete_empty_is_refused_when_disabled() {
    let home = tempfile::tempdir().expect("home");
    let claude = tempfile::tempdir().expect("claude");
    disable_delete(home.path());
    let transcript = write_empty_transcript(claude.path());

    let output = run(home.path(), claude.path(), &["delete-empty", "--yes"]);

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("disable_delete"),
        "{output:?}"
    );
    assert!(transcript.exists());
}

#[test]
fn delete_empty_dry_run_still_lists_when_disabled() {
    let home = tempfile::tempdir().expect("home");
    let claude = tempfile::tempdir().expect("claude");
    disable_delete(home.path());
    let transcript = write_empty_transcript(claude.path());

    let output = run(home.path(), claude.path(), &["delete-empty"]);

    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(SESSION_ID),
        "{output:?}"
    );
    assert!(transcript.exists());
}
