mod agent;
mod claude;
mod cli;
mod config;
mod debug;
mod debug_log;
mod display;
mod error;
mod history;
mod markdown;
mod pager;
pub mod search;
mod semantic;
mod semantic_cli;
mod syntax;
mod text_match;
mod time_filter;
mod tool_format;
mod tui;
mod update;

use clap::Parser;
use cli::{Args, Commands, DeleteEmptyArgs};
use error::{AppError, Result};
use search::mode::{SearchModeResolution, TuiSearchMode};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use std::process::Command;
use tui::ListSearchMode;

fn main() {
    if let Err(e) = run() {
        match e {
            AppError::SelectionCancelled => {
                // User cancelled, exit silently
                std::process::exit(0);
            }
            AppError::AgentProtocol(output) => {
                eprint!("{output}");
                std::process::exit(1);
            }
            AppError::Agent(error) => {
                eprint!("{}", agent::diagnostic::format_error(&error));
                std::process::exit(1);
            }
            _ => {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
    }
}

/// Helper function to resolve a boolean setting by merging CLI flags and config values.
///
/// Priority: enable_flag > disable_flag > config_value > default_value
fn resolve_bool_setting(
    enable_flag: bool,
    disable_flag: bool,
    config_value: Option<bool>,
    default_value: bool,
) -> bool {
    if enable_flag {
        true
    } else if disable_flag {
        false
    } else {
        config_value.unwrap_or(default_value)
    }
}

fn run_delete_empty_command(args: DeleteEmptyArgs) -> Result<()> {
    let scope = if args.local {
        history::DeleteEmptyScope::Local
    } else {
        history::DeleteEmptyScope::All
    };
    let summary = history::delete_empty_transcripts(scope, args.yes)?;

    if summary.candidates.is_empty() {
        println!("No empty transcripts found.");
        println!("{}", delete_empty_summary_line(args.yes, 0));
        return Ok(());
    }

    if args.yes {
        println!("Deleted {} empty transcript(s):", summary.deleted);
    } else {
        println!(
            "Found {} empty transcript(s). Re-run with --yes to delete:",
            summary.candidates.len()
        );
    }

    for transcript in &summary.candidates {
        let preview = transcript
            .preview
            .as_deref()
            .map(truncate_delete_empty_preview)
            .unwrap_or_else(|| "(no user messages)".to_string());
        println!(
            "{}\t{}\tusers={}\tlines={}\t{}",
            transcript.session_id,
            transcript.project_name,
            transcript.user_messages,
            transcript.line_count,
            preview
        );
        println!("  {}", transcript.path.display());
    }

    println!(
        "{}",
        delete_empty_summary_line(args.yes, summary.candidates.len())
    );

    Ok(())
}

fn delete_empty_summary_line(delete: bool, count: usize) -> String {
    if delete {
        format!("Summary: deleted {count} empty transcript(s).")
    } else {
        format!("Summary: {count} empty transcript(s) would be deleted.")
    }
}

fn truncate_delete_empty_preview(preview: &str) -> String {
    const MAX_CHARS: usize = 120;
    let mut chars = preview.chars();
    let truncated = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{}...", truncated)
    } else {
        truncated
    }
}

fn run() -> Result<()> {
    let args = Args::parse();

    // Handle subcommands
    if let Some(command) = args.command {
        return match command {
            Commands::Agent { command } => agent::service::execute(command).map(|output| {
                print!("{output}");
            }),
            Commands::DeleteEmpty { yes, local, all } => {
                run_delete_empty_command(DeleteEmptyArgs { yes, local, all })
            }
            Commands::Update => update::run(),
        };
    }

    // Detect terminal theme before entering raw mode / alternate screen,
    // as terminal_light queries the terminal for background color
    tui::theme::detect_theme();

    let config = config::load_config()?;

    // Merge CLI arguments with config file settings. CLI takes precedence.
    let display_config = config.display.unwrap_or_default();

    // Extract resume config
    let resume_config = config.resume.unwrap_or_default();
    let default_args = resume_config.default_args.as_deref().unwrap_or(&[]);

    let search_config = config.search.unwrap_or_default();
    let tui_config = config.tui.unwrap_or_default();
    let search_mode = search::mode::resolve_tui_search_mode(SearchModeResolution {
        cli_mode: None,
        config_mode: search_config.mode,
        tui_semantic_search: tui_config.semantic_search,
    });
    let exclude_projects = tui_config.exclude_projects;

    // Disable colors globally when --no-color is passed
    if args.no_color {
        colored::control::set_override(false);
    }

    // Resolve keybindings
    let keys = config::KeyBindings::from_config(config.keys);

    // Use positive names internally for clarity
    let show_tools = resolve_bool_setting(
        args.show_tools,
        args.no_tools,
        display_config.no_tools.map(|b| !b),
        false, // Default: hide tools
    );
    // Map CLI flag to ToolDisplayMode
    // --show-tools → Full, --no-tools → Hidden, default → Hidden summary
    let tool_display = if args.show_tools {
        tui::ToolDisplayMode::Full
    } else if args.no_tools {
        tui::ToolDisplayMode::Hidden
    } else {
        match display_config.no_tools {
            Some(true) => tui::ToolDisplayMode::Hidden,
            Some(false) => tui::ToolDisplayMode::Full,
            None => tui::ToolDisplayMode::Hidden,
        }
    };
    let show_last = resolve_bool_setting(args.last, args.first, display_config.last, true);
    let show_thinking = resolve_bool_setting(
        args.show_thinking,
        args.hide_thinking,
        display_config.show_thinking,
        false,
    );
    let plain_mode = resolve_bool_setting(args.plain, false, display_config.plain, false);
    let use_pager = resolve_bool_setting(
        args.pager,
        args.no_pager,
        display_config.pager,
        std::io::stdout().is_terminal(),
    );

    // Handle --delete flag: delete a session by UUID and exit
    if let Some(ref session_id) = args.delete {
        match history::delete_session_by_uuid(session_id) {
            Ok(count) => {
                if count == 1 {
                    eprintln!("Deleted session {}", session_id);
                } else {
                    eprintln!(
                        "Deleted session {} ({} copies across projects)",
                        session_id, count
                    );
                }
                return Ok(());
            }
            Err(e) => return Err(e),
        }
    }

    // Resolved before the search paths below so every one of them honours the
    // window rather than silently ignoring it.
    let time_filter = args.time.resolve()?;

    // Handle --debug-search flag: debug search result scoring
    if let Some(ref query) = args.debug_search {
        let mut conversations = history::load_all_conversations(show_last, args.debug)?;
        conversations.retain(|conversation| time_filter.matches(conversation.timestamp));
        conversations.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

        let searchable = search::precompute_search_text(&conversations);
        let now = chrono::Local::now();

        // Optionally filter to local workspace
        let current_project_dir_name = if args.local {
            std::env::current_dir()
                .ok()
                .map(|d| history::convert_path_to_project_dir_name(&d))
        } else {
            None
        };

        let debug_search = search::debug_search(&conversations, &searchable, query, now, |index| {
            if let Some(ref proj) = current_project_dir_name {
                let conv = &conversations[index];
                return conv
                    .path
                    .parent()
                    .and_then(|p| p.file_name())
                    .is_some_and(|name| history::is_same_project(&name.to_string_lossy(), proj));
            }
            true
        });
        let results = debug_search.results;

        eprintln!("intent: {:?}", debug_search.parsed.unquoted());
        if debug_search.parsed.literals().is_empty() {
            eprintln!("literals: none");
        } else {
            eprintln!("literals:");
            for literal in debug_search.parsed.literals() {
                eprintln!("  {:?} ({:?})", literal.text(), literal.case_mode());
            }
        }
        eprintln!();

        for (rank, (idx, debug)) in results.iter().take(30).enumerate() {
            let conv = &conversations[*idx];
            let age = now.signed_duration_since(conv.timestamp);
            let project = conv.project_name.as_deref().unwrap_or("(none)");
            let age_str = format_debug_age(age);
            let session = conv
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("?");
            eprintln!(
                "#{:2} score={:.2} freshness={:.2} | {} | {} | {} ago",
                rank + 1,
                debug.total,
                debug.freshness,
                project,
                session,
                age_str
            );

            for field in &debug.fields {
                if field.tf_score > 0.0 || field.adjacency_score > 0.0 {
                    eprintln!(
                        "     {}: tf={:.2} adj={:.2} (w={:.1})",
                        field.name, field.tf_score, field.adjacency_score, field.weight
                    );
                    for (word, tf, ln_score) in &field.word_details {
                        if *tf > 0 {
                            eprintln!("       \"{}\" tf={} ln={:.2}", word, tf, ln_score);
                        }
                    }
                }
            }
            eprintln!();
        }

        return Ok(());
    }

    if let Some(ref query) = args.debug_semantic_search {
        let mut conversations = history::load_all_conversations(show_last, args.debug)?;
        conversations.retain(|conversation| time_filter.matches(conversation.timestamp));
        conversations.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        return semantic_cli::debug_search(query, &conversations, args.local);
    }

    if args.generate_semantic_cache {
        // Cache generation covers the whole corpus by definition; a partial
        // cache would be silently wrong on later unfiltered searches.
        if time_filter.is_active() {
            return Err(AppError::ConfigError(
                "--generate-semantic-cache cannot be combined with a time filter".to_string(),
            ));
        }
        let mut conversations = history::load_all_conversations(show_last, args.debug)?;
        conversations.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        return semantic_cli::generate_cache(&conversations, args.local);
    }

    if args.clear_semantic_cache {
        return semantic_cli::clear_cache();
    }

    // Handle --semantic-search flag
    if let Some(ref query) = args.semantic_search {
        let mut conversations = history::load_all_conversations(show_last, args.debug)?;
        conversations.retain(|conversation| time_filter.matches(conversation.timestamp));
        conversations.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        return semantic_cli::run(query, &conversations, args.semantic_top, args.local);
    }

    // Handle --render flag: render a JSONL file in ledger format and exit
    if let Some(ref render_path) = args.render {
        let display_options = display::DisplayOptions {
            no_tools: !show_tools,
            show_thinking,
            debug_level: args.debug,
            use_pager,
            no_color: args.no_color,
        };
        return display::render_to_terminal(render_path, &display_options);
    }

    // Handle direct file input mode
    if let Some(ref input_file) = args.input_file {
        if !input_file.exists() {
            return Err(AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("File not found: {}", input_file.display()),
            )));
        }
        if !input_file.is_file() {
            return Err(AppError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Not a file: {}", input_file.display()),
            )));
        }
        tui::run_single_file(input_file.clone(), tool_display, show_thinking, keys)?;
        return Ok(());
    }

    let use_local = args.local;

    // Determine the current workspace's project directory name (for workspace filter)
    let current_dir = std::env::current_dir().ok();
    let current_project_dir_name = current_dir
        .as_ref()
        .map(|d| history::convert_path_to_project_dir_name(d));

    // Handle --show-dir flag (needs current_dir)
    if args.show_dir {
        if let Some(ref dir) = current_dir {
            let projects_dir = history::get_claude_projects_dir(dir)?;
            println!("{}", projects_dir.display());
            return Ok(());
        } else {
            return Err(AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Failed to get current directory",
            )));
        }
    }

    // --local starts with workspace filter on; default is global (filter off)
    let workspace_filter = use_local;

    // Always use streaming global loader for all conversations
    let rx = history::load_all_conversations_streaming(show_last, args.debug, time_filter);

    // An empty result is far more often the time filter than an empty history,
    // so say which when a filter is in play.
    let describe_empty = |error| match error {
        AppError::NoHistoryFound(scope) if time_filter.is_active() => {
            AppError::NoHistoryFound(format!("{scope} within the requested time range"))
        }
        other => other,
    };

    let (conversations, selected_path) = match tui::run_with_loader(
        rx,
        tool_display,
        show_thinking,
        keys,
        workspace_filter,
        current_project_dir_name,
        exclude_projects,
        tui::TuiSearchOptions {
            default_mode: tui_search_mode(search_mode),
        },
    )
    .map_err(describe_empty)?
    {
        (tui::Action::Select(path), convs) => (convs, path),
        (tui::Action::Resume(path), convs) => {
            let conv = convs.iter().find(|c| c.path == path);
            let project_path = conv.and_then(|c| c.project_path.as_ref());
            resume_with_claude(&path, project_path, default_args, false)?;
            return Ok(());
        }
        (tui::Action::ForkResume(path), convs) => {
            let conv = convs.iter().find(|c| c.path == path);
            let project_path = conv.and_then(|c| c.project_path.as_ref());
            resume_with_claude(&path, project_path, default_args, true)?;
            return Ok(());
        }
        (tui::Action::Quit, _) => return Err(AppError::SelectionCancelled),
        (tui::Action::Delete(_), _) => unreachable!("Delete is handled internally"),
    };

    if args.show_path {
        println!("{}", selected_path.display());
        return Ok(());
    }

    if args.show_id {
        let conversation_id = selected_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| {
                AppError::ClaudeExecutionError(
                    "Conversation filename is not valid Unicode".to_string(),
                )
            })?;
        println!("{}", conversation_id);
        return Ok(());
    }

    if args.resume {
        // Find the selected conversation to get its project_path
        let conv = conversations.iter().find(|c| c.path == selected_path);
        debug::debug(
            args.debug,
            &format!("Selected path: {}", selected_path.display()),
        );
        debug::debug(
            args.debug,
            &format!("Found conversation: {}", conv.is_some()),
        );
        if let Some(c) = conv {
            debug::debug(args.debug, &format!("project_path: {:?}", c.project_path));
            if let Some(p) = &c.project_path {
                debug::debug(args.debug, &format!("project_path exists: {}", p.exists()));
            }
        }
        let project_path = conv.and_then(|c| c.project_path.as_ref());
        resume_with_claude(
            &selected_path,
            project_path,
            default_args,
            args.fork_session,
        )?;
        return Ok(());
    }

    // Log parse errors to debug log if debug mode is enabled
    if args.debug.is_some()
        && let Some(conv) = conversations.iter().find(|c| c.path == selected_path)
    {
        if let Err(e) = debug_log::log_parse_errors(conv) {
            debug::warn(
                args.debug,
                &format!("Failed to write parse errors to log: {}", e),
            );
        } else if !conv.parse_errors.is_empty() {
            debug::info(
                args.debug,
                &format!(
                    "Logged {} parse error(s) to ~/.local/state/claude-history/debug.log",
                    conv.parse_errors.len()
                ),
            );
        }
    }

    // Display the selected conversation
    let display_options = display::DisplayOptions {
        no_tools: !show_tools,
        show_thinking,
        debug_level: args.debug,
        use_pager,
        no_color: args.no_color,
    };

    if plain_mode {
        display::display_conversation_plain(&selected_path, &display_options)?;
    } else {
        display::display_conversation(&selected_path, &display_options)?;
    }

    Ok(())
}

#[cfg(test)]
mod agent_command_tests {
    use super::*;
    use crate::agent::refs::AgentConversationKey;
    use crate::agent::service::{
        agent_semantic_candidates, resolve_agent_conversation_arg, resolve_agent_read_args,
        run_agent_outline, run_agent_read,
    };
    use crate::agent::test_support::{assistant_jsonl_line as assistant, user_jsonl_line as user};
    use crate::search::mode::SearchMode;
    use cli::{AgentOutlineArgs, AgentOutputFlags, AgentReadArgs};

    fn key(project: &str, filename: &str) -> AgentConversationKey {
        AgentConversationKey::new(
            project,
            filename,
            PathBuf::from(format!("/{project}/{filename}")),
        )
    }

    fn default_output() -> AgentOutputFlags {
        AgentOutputFlags {
            budget: Some(6000),
            no_budget: false,
            tools: false,
            tool_results: false,
            thinking: false,
            subagents: false,
        }
    }

    fn read_args(refs: Vec<String>, focus: Option<String>) -> AgentReadArgs {
        AgentReadArgs {
            refs,
            anchor: None,
            focus,
            lines: None,
            match_query: None,
            context: 3,
            output: default_output(),
        }
    }

    #[test]
    fn delete_empty_summary_line_reports_dry_run_count() {
        assert_eq!(
            delete_empty_summary_line(false, 7),
            "Summary: 7 empty transcript(s) would be deleted."
        );
    }

    #[test]
    fn delete_empty_summary_line_reports_deleted_count() {
        assert_eq!(
            delete_empty_summary_line(true, 7),
            "Summary: deleted 7 empty transcript(s)."
        );
    }

    #[test]
    fn read_validation_rejects_focus_outside_range() {
        let keys = vec![key(
            "project-a",
            "12345678-1234-4234-9234-123456789abc.jsonl",
        )];
        let conversation = keys[0].conversation_ref().canonical();
        let args = read_args(
            vec![format!("{conversation}:m2..m4")],
            Some("m5".to_string()),
        );
        let err = resolve_agent_read_args(&args, Some(&keys)).unwrap_err();
        assert!(err.to_string().contains("outside"));
    }

    fn write_jsonl(dir: &tempfile::TempDir, filename: &str, lines: &[String]) -> PathBuf {
        let path = dir.path().join(filename);
        std::fs::write(&path, lines.join("\n")).unwrap();
        path
    }

    #[test]
    fn command_refs_reject_uuid_args() {
        let keys = vec![key(
            "project-a",
            "12345678-1234-4234-9234-123456789abc.jsonl",
        )];
        let uuid = "12345678-1234-4234-9234-123456789abc";

        let read = read_args(vec![format!("{uuid}:m1..m2")], None);
        assert!(
            resolve_agent_read_args(&read, Some(&keys))
                .unwrap_err()
                .to_string()
                .contains("use ref=ch_...")
        );

        let focus = read_args(
            vec![format!("{}:m1..m2", keys[0].conversation_ref().canonical())],
            Some(format!("{uuid}:m1")),
        );
        assert!(
            resolve_agent_read_args(&focus, Some(&keys))
                .unwrap_err()
                .to_string()
                .contains("use ref=ch_...")
        );

        assert!(
            resolve_agent_conversation_arg(uuid, Some(&keys))
                .unwrap_err()
                .to_string()
                .contains("use ref=ch_...")
        );
    }

    #[test]
    fn read_command_loads_transcript_and_formats_protocol() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            &dir,
            "12345678-1234-4234-9234-123456789abc.jsonl",
            &[user("question"), assistant("answer")],
        );
        let keys = vec![AgentConversationKey::new(
            "project-a",
            "12345678-1234-4234-9234-123456789abc.jsonl",
            path,
        )];
        let conversation = keys[0].conversation_ref().canonical();
        let args = read_args(vec![format!("{conversation}:m1..m2")], None);

        let output = run_agent_read(&args, Some(&keys)).unwrap();

        assert!(output.starts_with("protocol agent-read"));
        assert!(output.contains("conversation project=pr_"));
        assert!(output.contains("uuid=12345678-1234-4234-9234-123456789abc ref=ch_"));
        assert!(output.contains("message m1 role=user line=1"));
        assert!(output.contains("| question\n"));
        assert!(output.contains("message m2 role=assistant line=2"));
        assert!(!output.contains("not implemented"));
    }

    #[test]
    fn read_slice_requires_one_single_message_ref() {
        let keys = vec![key(
            "project-a",
            "12345678-1234-4234-9234-123456789abc.jsonl",
        )];
        let conversation = keys[0].conversation_ref().canonical();
        let mut args = read_args(vec![format!("{conversation}:m1..m2")], None);
        args.match_query = Some("needle".to_string());

        let error = resolve_agent_read_args(&args, Some(&keys)).unwrap_err();

        assert!(error.to_string().contains("single-message ref"));
    }

    #[test]
    fn read_command_slices_a_large_message_around_matches() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            &dir,
            "12345678-1234-4234-9234-123456789abc.jsonl",
            &[user("one\ntwo\nhistorical correction\nfour\nfive")],
        );
        let keys = vec![AgentConversationKey::new(
            "project-a",
            "12345678-1234-4234-9234-123456789abc.jsonl",
            path,
        )];
        let conversation = keys[0].conversation_ref().canonical();
        let mut args = read_args(vec![format!("{conversation}:m1")], None);
        args.match_query = Some("historical correction".to_string());
        args.context = 1;

        let output = run_agent_read(&args, Some(&keys)).unwrap();

        assert!(output.contains("slice=match=historical%20correction context=1 hits=1"));
        assert!(output.contains("|  2: two\n| >3: historical correction\n|  4: four\n"));
        assert!(!output.contains("1: one"));
        assert!(!output.contains("5: five"));
    }

    #[test]
    fn outline_command_loads_transcript_and_formats_protocol() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            &dir,
            "12345678-1234-4234-9234-123456789abc.jsonl",
            &[user("question"), assistant("answer")],
        );
        let keys = vec![AgentConversationKey::new(
            "project-a",
            "12345678-1234-4234-9234-123456789abc.jsonl",
            path,
        )];
        let args = AgentOutlineArgs {
            conversation: keys[0].conversation_ref().canonical(),
            output: default_output(),
        };

        let output = run_agent_outline(&args, Some(&keys)).unwrap();

        assert!(output.starts_with("protocol agent-outline"));
        assert!(output.contains("conversation project=pr_"));
        assert!(output.contains("uuid=12345678-1234-4234-9234-123456789abc ref=ch_"));
        assert!(output.contains("m1 role=user chars=8 anchor=ma_"));
        assert!(output.contains(" | question"));
        assert!(output.contains("m2 role=assistant chars=6 anchor=ma_"));
        assert!(output.contains(" | answer"));
    }

    #[test]
    fn search_output_emits_read_ref_with_focus_recipe() {
        let output = agent::search::AgentSearchOutput {
            protocol: agent::search::AgentProtocolKind::Search,
            target: None,
            query: "cache warming".to_string(),
            mode: SearchMode::Lexical,
            hits: vec![agent::search::AgentOutputHit {
                conversation_ref: "ch_1234abcd5678".to_string(),
                project_id: "pr_test".to_string(),
                conversation_uuid: "12345678-1234-4234-9234-123456789abc".to_string(),
                session: "12345678-1234-4234-9234-123456789abc.jsonl".to_string(),
                anchors: vec!["ma_0000000000000000".to_string()],
                title: "cache session".to_string(),
                score: 12.5,
                source: agent::search::AgentHitKind::Lexical,
                evidence_source: agent::retrieval::AgentHitSource::Dialogue,
                render_options: agent::retrieval::AgentHitRenderOptions::default(),
                preview: "cache warming answer".to_string(),
                focus_range: agent::refs::MessageRange::single(2),
                read_range: agent::refs::MessageRange { start: 1, end: 3 },
            }],
            groups: vec![],
            flat: true,
            budget: None,
            stats: agent::search::AgentSearchStats::default(),
        };

        let rendered = agent::search::format_agent_output(&output);

        assert!(rendered.starts_with(
            "protocol agent-search mode=lexical cut=none chars=none policy=per-hit hits=1\n"
        ));
        assert!(rendered.contains(
            "hit project=pr_test uuid=12345678-1234-4234-9234-123456789abc ref=ch_1234abcd5678"
        ));
        assert!(rendered.contains("read ref=ch_1234abcd5678:m1..m3 focus=m2..m2 tools=false tool-results=false thinking=false subagents=false\n"));
    }

    fn read_args_from_line(read_line: &str) -> AgentReadArgs {
        let mut read_ref = None;
        let mut focus = None;
        let mut tools = false;
        let mut tool_results = false;
        let mut thinking = false;
        let mut subagents = false;
        for field in read_line.split_whitespace().skip(1) {
            if let Some(value) = field.strip_prefix("ref=") {
                read_ref = Some(value.to_string());
            } else if let Some(value) = field.strip_prefix("focus=") {
                focus = Some(value.to_string());
            } else if field == "tools=true" {
                tools = true;
            } else if field == "tool-results=true" {
                tool_results = true;
            } else if field == "thinking=true" {
                thinking = true;
            } else if field == "subagents=true" {
                subagents = true;
            }
        }
        AgentReadArgs {
            refs: vec![read_ref.expect("read ref field")],
            anchor: None,
            focus,
            lines: None,
            match_query: None,
            context: 3,
            output: AgentOutputFlags {
                budget: Some(6000),
                no_budget: false,
                tools,
                tool_results,
                thinking,
                subagents,
            },
        }
    }

    #[test]
    fn within_read_ref_can_drive_focused_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            &dir,
            "12345678-1234-4234-9234-123456789abc.jsonl",
            &[
                user("question"),
                assistant("cache warming answer"),
                user("follow up"),
            ],
        );
        let (keys, resolved) = resolved_test_conversation(path.clone());
        let conversation = stubbed_conversation(path, 3);
        let transcript = load_transcript(&resolved.key.path);
        let within_args = cli::AgentWithinArgs {
            conversation: resolved.reference.canonical(),
            query: "cache warming".to_string(),
            top: Some(1),
            budget: Some(6000),
            no_budget: false,
            search_mode: cli::AgentSearchModeArgs::explicit(SearchMode::Lexical),
        };
        let within_request = agent::search::AgentWithinRequest {
            query: within_args.query.clone(),
            top: within_args.top.unwrap(),
            cli_mode: within_args.mode_override(),
            config_mode: None,
            tui_semantic_search: None,
            budget: None,
        };
        let within = agent::search::format_agent_output(&agent::search::run_within_search(
            &within_request,
            &conversation,
            &resolved,
            &transcript,
            &[],
        ));
        let read_line = within
            .lines()
            .find(|line| line.starts_with("read ref="))
            .expect("within output should include a read ref");
        let read_args = read_args_from_line(read_line);

        let output = run_agent_read(&read_args, Some(&keys)).unwrap();

        assert!(output.starts_with("protocol agent-read"));
        assert!(output.contains("message m2 role=assistant line=2"));
        assert!(output.contains("| cache warming answer\n"));
    }

    fn tool_result_user(tool_output: &str) -> String {
        serde_json::json!({
            "type": "user",
            "timestamp": "2024-01-01T00:00:02Z",
            "message": {
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": tool_output}]
            }
        })
        .to_string()
    }

    fn resolved_test_conversation(
        path: PathBuf,
    ) -> (Vec<AgentConversationKey>, agent::refs::ResolvedConversation) {
        let key = AgentConversationKey::new(
            "project-a",
            "12345678-1234-4234-9234-123456789abc.jsonl",
            path,
        );
        let resolved = agent::refs::ResolvedConversation {
            key: key.clone(),
            reference: key.conversation_ref(),
        };
        (vec![key], resolved)
    }

    fn stubbed_conversation(path: PathBuf, message_count: usize) -> history::Conversation {
        history::Conversation {
            path,
            index: 0,
            timestamp: chrono::Local::now(),
            preview: "session".to_string(),
            preview_first: "session".to_string(),
            preview_last: "session".to_string(),
            full_text: "session".to_string(),
            agent_search_text: String::new(),
            semantic_turns: vec!["session".to_string()],
            semantic_turn_ranges: vec![agent::refs::MessageRange::single(1)],
            search_text_lower: "session".to_string(),
            project_name: Some("project-a".to_string()),
            project_path: None,
            cwd: None,
            message_count,
            parse_errors: Vec::new(),
            summary: None,
            custom_title: Some("session".to_string()),
            model: None,
            total_tokens: 0,
            duration_minutes: None,
        }
    }

    fn parsed_conversation(path: &Path) -> history::Conversation {
        history::parser::process_conversation_reader(
            path.to_path_buf(),
            std::io::Cursor::new(std::fs::read_to_string(path).unwrap()),
            None,
            None,
        )
        .unwrap()
        .unwrap()
    }

    fn load_transcript(path: &Path) -> agent::transcript::AgentTranscript {
        agent::transcript::AgentTranscript::load(path).unwrap()
    }

    fn semantic_hit_for_test(
        source: crate::semantic::types::SemanticChunkSource,
        message_range: agent::refs::MessageRange,
        evidence_preview: &str,
    ) -> crate::semantic::types::SemanticHit {
        crate::semantic::types::SemanticHit::new(
            crate::semantic::types::SemanticScoreBreakdown {
                hybrid: 0.9,
                semantic: 0.9,
                lexical: 0.0,
            },
            crate::semantic::types::SemanticExplanation {
                quality: crate::semantic::types::SemanticQuality::Good,
                quality_label: "good",
                matched_terms: vec![],
                evidence_preview: evidence_preview.to_string(),
                rationale_kind: crate::semantic::types::SemanticRationaleKind::SemanticOnly,
                chunk: crate::semantic::types::SemanticChunkIdentity {
                    conversation_index: 0,
                    source,
                    session: "session".to_string(),
                    chunk_index: 0,
                    message_range,
                },
            },
        )
    }

    fn progress_text_message(text: &str) -> String {
        serde_json::json!({
            "type": "progress",
            "data": {
                "type": "agent_progress",
                "agentId": "agent-abcdef",
                "message": {
                    "type": "assistant",
                    "message": {
                        "role": "assistant",
                        "content": [{"type": "text", "text": text}]
                    }
                }
            }
        })
        .to_string()
    }

    #[test]
    fn tool_result_read_recipe_replays_without_manual_flags() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            &dir,
            "12345678-1234-4234-9234-123456789abc.jsonl",
            &[
                user("question"),
                assistant("I'll inspect it"),
                tool_result_user("hidden_exact_tool_needle"),
                assistant("done"),
            ],
        );
        let (keys, resolved) = resolved_test_conversation(path.clone());
        let conversation = stubbed_conversation(path, 4);
        let transcript = load_transcript(&resolved.key.path);
        let within_request = agent::search::AgentWithinRequest {
            query: "\"hidden_exact_tool_needle\"".to_string(),
            top: 1,
            cli_mode: None,
            config_mode: None,
            tui_semantic_search: None,
            budget: None,
        };
        let within = agent::search::format_agent_output(&agent::search::run_within_search(
            &within_request,
            &conversation,
            &resolved,
            &transcript,
            &[],
        ));

        assert!(within.contains("hit project=pr_"));
        assert!(within.contains(" ref="));
        assert!(within.contains("source=tool"));
        let read_line = within
            .lines()
            .find(|line| line.starts_with("read ref="))
            .expect("within output should include a read ref");
        assert!(read_line.contains("tool-results=true"));
        let output = run_agent_read(&read_args_from_line(read_line), Some(&keys)).unwrap();

        assert!(output.contains("hidden_exact_tool_needle"));
    }

    #[test]
    fn semantic_read_ref_uses_canonical_assistant_ordinal() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            &dir,
            "12345678-1234-4234-9234-123456789abc.jsonl",
            &[
                user("first visible"),
                serde_json::json!({
                    "type": "assistant",
                    "timestamp": "2024-01-01T00:00:01Z",
                    "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "pwd"}}]}
                })
                .to_string(),
                tool_result_user("tool output only"),
                progress_text_message("subagent hidden text"),
                assistant("final assistant text"),
            ],
        );
        let (keys, resolved) = resolved_test_conversation(path.clone());
        let conversation = parsed_conversation(&path);
        let transcript = load_transcript(&resolved.key.path);
        let semantic_range = *conversation
            .semantic_turn_ranges
            .iter()
            .find(|range| **range == agent::refs::MessageRange::single(5))
            .expect("assistant text should use canonical m5");
        let semantic_hit = semantic_hit_for_test(
            crate::semantic::types::SemanticChunkSource::VisibleDialogue,
            semantic_range,
            "final assistant text",
        );
        let within_request = agent::search::AgentWithinRequest {
            query: "final assistant".to_string(),
            top: 1,
            cli_mode: Some(SearchMode::Semantic),
            config_mode: None,
            tui_semantic_search: None,
            budget: None,
        };
        let within = agent::search::format_agent_output(&agent::search::run_within_search(
            &within_request,
            &conversation,
            &resolved,
            &transcript,
            &[semantic_hit],
        ));

        assert!(within.contains("focus=m5..m5"));
        let read_line = within
            .lines()
            .find(|line| line.starts_with("read ref="))
            .expect("within output should include a read ref");
        let output = run_agent_read(&read_args_from_line(read_line), Some(&keys)).unwrap();

        assert!(output.contains("message m5 role=assistant"));
        assert!(output.contains("final assistant text"));
    }

    #[test]
    fn semantic_read_ref_skips_assistant_image_only_ordinal() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            &dir,
            "12345678-1234-4234-9234-123456789abc.jsonl",
            &[
                user("first visible"),
                serde_json::json!({
                    "type": "assistant",
                    "timestamp": "2024-01-01T00:00:01Z",
                    "message": {"role": "assistant", "content": [{"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "abc"}}]}
                })
                .to_string(),
                assistant("final assistant text"),
            ],
        );
        let (keys, resolved) = resolved_test_conversation(path.clone());
        let conversation = parsed_conversation(&path);
        let transcript = load_transcript(&resolved.key.path);
        let semantic_range = *conversation
            .semantic_turn_ranges
            .iter()
            .find(|range| **range == agent::refs::MessageRange::single(2))
            .expect("assistant text should use canonical m2");
        let semantic_hit = semantic_hit_for_test(
            crate::semantic::types::SemanticChunkSource::VisibleDialogue,
            semantic_range,
            "final assistant text",
        );
        let within_request = agent::search::AgentWithinRequest {
            query: "final assistant".to_string(),
            top: 1,
            cli_mode: Some(SearchMode::Semantic),
            config_mode: None,
            tui_semantic_search: None,
            budget: None,
        };
        let within = agent::search::format_agent_output(&agent::search::run_within_search(
            &within_request,
            &conversation,
            &resolved,
            &transcript,
            &[semantic_hit],
        ));

        assert!(within.contains("focus=m2..m2"));
        let read_line = within
            .lines()
            .find(|line| line.starts_with("read ref="))
            .expect("within output should include a read ref");
        let output = run_agent_read(&read_args_from_line(read_line), Some(&keys)).unwrap();

        assert!(output.contains("message m2 role=assistant"));
        assert!(output.contains("final assistant text"));
    }

    #[test]
    fn global_search_finds_progress_only_subagent_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            &dir,
            "12345678-1234-4234-9234-123456789abc.jsonl",
            &[
                user("question"),
                progress_text_message("subagent_unique_needle"),
                assistant("done"),
            ],
        );
        let (keys, _) = resolved_test_conversation(path.clone());
        let conversation = parsed_conversation(&path);
        assert!(!conversation.full_text.contains("subagent_unique_needle"));
        assert!(
            conversation
                .agent_search_text
                .contains("subagent_unique_needle")
        );
        let conversations = vec![conversation];
        let searchable = search::precompute_agent_search_text(&conversations);
        let ranked = search::agent_search(
            &conversations,
            &searchable,
            "\"subagent_unique_needle\"",
            chrono::Local::now(),
        );
        let request = agent::search::AgentSearchRequest {
            query: "\"subagent_unique_needle\"".to_string(),
            top: 1,
            cli_mode: None,
            config_mode: None,
            tui_semantic_search: None,
            flat: false,
            hits_per_conversation: 2,
            all_hits: false,
            budget: None,
        };
        let output = agent::search::run_global_lexical_search(
            &request,
            &conversations,
            &keys,
            &ranked,
            |key| agent::transcript::AgentTranscript::load(&key.path),
        )
        .unwrap();
        let rendered = agent::search::format_agent_output(&output);

        assert!(rendered.contains("protocol agent-search"));
        assert!(rendered.contains("conversation rank=1"));
        assert!(rendered.contains("subagents=true"));
        let read_line = rendered
            .lines()
            .find(|line| line.starts_with("read ref="))
            .expect("search output should include a read ref");
        let read_output = run_agent_read(&read_args_from_line(read_line), Some(&keys)).unwrap();

        assert!(read_output.contains("subagent_unique_needle"));
    }

    #[test]
    fn agent_semantic_candidates_include_progress_only_subagent_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            &dir,
            "12345678-1234-4234-9234-123456789abc.jsonl",
            &[
                user("question"),
                r#"{"type":"progress","data":{"type":"agent_progress","agentId":"agent-abcdef","message":{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"progress_only_semantic_needle"}]}}}}"#.to_string(),
                assistant("done"),
            ],
        );
        let key = AgentConversationKey::new(
            "project-a",
            "12345678-1234-4234-9234-123456789abc.jsonl",
            path.clone(),
        );
        let resolved = agent::refs::ResolvedConversation {
            key: key.clone(),
            reference: key.conversation_ref(),
        };
        let conversation = history::parser::process_conversation_reader(
            path.clone(),
            std::io::Cursor::new(std::fs::read_to_string(&path).unwrap()),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        assert!(
            !conversation
                .semantic_turns
                .join(" ")
                .contains("progress_only_semantic_needle")
        );
        let input = agent::search::AgentConversationInput {
            conversation: &conversation,
            resolved,
            original_index: 0,
        };

        let candidates = agent_semantic_candidates(&[input]).0;
        let candidate = candidates[1].conversation.as_ref();

        assert_eq!(candidates.len(), 2);
        assert_eq!(
            candidates[0].source,
            crate::semantic::types::SemanticChunkSource::VisibleDialogue
        );
        assert_eq!(
            candidates[1].source,
            crate::semantic::types::SemanticChunkSource::AgentSubagentDialogue
        );
        assert!(
            candidate
                .semantic_turns
                .join(" ")
                .contains("progress_only_semantic_needle")
        );
        assert_eq!(
            candidate.semantic_turn_ranges.last().copied(),
            Some(agent::refs::MessageRange::single(2))
        );
        assert!(
            !conversation
                .semantic_turns
                .join(" ")
                .contains("progress_only_semantic_needle")
        );
    }

    #[test]
    fn agent_semantic_candidates_require_canonical_transcript_projection() {
        let key = AgentConversationKey::new(
            "project-a",
            "missing.jsonl",
            std::path::PathBuf::from("/missing/session.jsonl"),
        );
        let resolved = agent::refs::ResolvedConversation {
            key: key.clone(),
            reference: key.conversation_ref(),
        };
        let conversation = history::Conversation {
            path: key.path.clone(),
            index: 0,
            timestamp: chrono::Local::now(),
            preview: "visible semantic".to_string(),
            preview_first: "visible semantic".to_string(),
            preview_last: "visible semantic".to_string(),
            full_text: "visible semantic".to_string(),
            agent_search_text: String::new(),
            semantic_turns: vec!["visible semantic".to_string()],
            semantic_turn_ranges: vec![agent::refs::MessageRange::single(1)],
            search_text_lower: "visible semantic".to_string(),
            project_name: Some("project-a".to_string()),
            project_path: None,
            cwd: None,
            message_count: 1,
            parse_errors: Vec::new(),
            summary: None,
            custom_title: Some("visible semantic".to_string()),
            model: None,
            total_tokens: 0,
            duration_minutes: None,
        };
        let input = agent::search::AgentConversationInput {
            conversation: &conversation,
            resolved,
            original_index: 0,
        };

        let (candidates, warnings) = agent_semantic_candidates(&[input]);

        assert!(candidates.is_empty());
        assert_eq!(warnings.len(), 1);
        assert_eq!(
            warnings[0].reference,
            Some(key.conversation_ref().canonical())
        );
    }

    #[test]
    fn agent_semantic_candidates_recover_malformed_transcripts() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            &dir,
            "12345678-1234-4234-9234-123456789abc.jsonl",
            &[
                user("visible semantic"),
                r#"{"type":"progress","data":{"type":"agent_progress","agentId":"agent-abcdef","message":{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"progress_only_semantic_needle"}]}}}}"#.to_string(),
                "not json".to_string(),
                assistant("done"),
            ],
        );
        let key = AgentConversationKey::new(
            "project-a",
            "12345678-1234-4234-9234-123456789abc.jsonl",
            path.clone(),
        );
        let resolved = agent::refs::ResolvedConversation {
            key: key.clone(),
            reference: key.conversation_ref(),
        };
        let conversation = history::parser::process_conversation_reader(
            path,
            std::io::Cursor::new([
                user("visible semantic"),
                r#"{"type":"progress","data":{"type":"agent_progress","agentId":"agent-abcdef","message":{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"progress_only_semantic_needle"}]}}}}"#.to_string(),
                "not json".to_string(),
                assistant("done"),
            ].join("\n")),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        assert!(
            conversation
                .agent_search_text
                .contains("progress_only_semantic_needle")
        );
        let input = agent::search::AgentConversationInput {
            conversation: &conversation,
            resolved,
            original_index: 0,
        };

        let (candidates, warnings) = agent_semantic_candidates(&[input]);

        assert_eq!(candidates.len(), 2);
        assert_eq!(warnings.len(), 1);
        assert_eq!(
            warnings[0].kind,
            agent::diagnostic::AgentWarningKind::MalformedTranscript
        );
        assert_eq!(
            candidates[0].source,
            crate::semantic::types::SemanticChunkSource::VisibleDialogue
        );
        assert_eq!(
            candidates[1].source,
            crate::semantic::types::SemanticChunkSource::AgentSubagentDialogue
        );
        assert!(
            candidates[1]
                .conversation
                .semantic_turns
                .join(" ")
                .contains("progress_only_semantic_needle")
        );
    }

    #[test]
    fn semantic_progress_hit_read_recipe_enables_subagents() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            &dir,
            "12345678-1234-4234-9234-123456789abc.jsonl",
            &[
                user("question"),
                progress_text_message("progress_only_semantic_needle"),
                assistant("done"),
            ],
        );
        let (keys, resolved) = resolved_test_conversation(path.clone());
        let conversation = parsed_conversation(&path);
        let transcript = load_transcript(&resolved.key.path);
        let semantic_hit = semantic_hit_for_test(
            crate::semantic::types::SemanticChunkSource::AgentSubagentDialogue,
            agent::refs::MessageRange::single(2),
            "progress_only_semantic_needle",
        );
        let within_request = agent::search::AgentWithinRequest {
            query: "progress semantic".to_string(),
            top: 1,
            cli_mode: Some(SearchMode::Semantic),
            config_mode: None,
            tui_semantic_search: None,
            budget: None,
        };
        let rendered = agent::search::format_agent_output(&agent::search::run_within_search(
            &within_request,
            &conversation,
            &resolved,
            &transcript,
            &[semantic_hit],
        ));

        assert!(rendered.contains("focus=m2..m2"));
        assert!(rendered.contains("tools=false tool-results=false thinking=false subagents=true"));
        let read_line = rendered
            .lines()
            .find(|line| line.starts_with("read ref="))
            .expect("within output should include a read ref");
        let read_output = run_agent_read(&read_args_from_line(read_line), Some(&keys)).unwrap();
        assert!(read_output.contains("progress_only_semantic_needle"));
    }

    #[test]
    fn read_command_rejects_out_of_range_loaded_messages() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jsonl(
            &dir,
            "12345678-1234-4234-9234-123456789abc.jsonl",
            &[user("question")],
        );
        let keys = vec![AgentConversationKey::new(
            "project-a",
            "12345678-1234-4234-9234-123456789abc.jsonl",
            path,
        )];
        let conversation = keys[0].conversation_ref().canonical();
        let args = read_args(vec![format!("{conversation}:m1..m2")], None);

        let err = run_agent_read(&args, Some(&keys)).unwrap_err();

        assert!(err.to_string().contains("exceeds transcript length"));
    }

    #[test]
    fn resume_action_uses_cwd_when_it_maps_to_selected_project_dir() {
        let cwd = tempfile::tempdir().unwrap();
        let stale_project = tempfile::tempdir().unwrap();
        let stale_project_path = stale_project.path().to_path_buf();
        let selected_path = history::get_claude_projects_dir(cwd.path())
            .unwrap()
            .join("12345678-1234-4234-9234-123456789abc.jsonl");

        let action = resolve_claude_resume_action(
            &selected_path,
            Some(&stale_project_path),
            cwd.path(),
            false,
        )
        .unwrap();

        assert_eq!(
            action,
            ClaudeResumeAction::Run {
                current_dir: cwd.path().to_path_buf()
            }
        );
    }

    #[test]
    fn resume_action_uses_project_path_when_it_maps_to_selected_project_dir() {
        let cwd = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let project_path = project.path().to_path_buf();
        let selected_path = history::get_claude_projects_dir(project.path())
            .unwrap()
            .join("12345678-1234-4234-9234-123456789abc.jsonl");

        let action =
            resolve_claude_resume_action(&selected_path, Some(&project_path), cwd.path(), false)
                .unwrap();

        assert_eq!(
            action,
            ClaudeResumeAction::Run {
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
        let selected_path = history::get_claude_projects_dir(selected_project.path())
            .unwrap()
            .join("12345678-1234-4234-9234-123456789abc.jsonl");
        let cwd_projects_dir = history::get_claude_projects_dir(cwd.path()).unwrap();

        let action = resolve_claude_resume_action(
            &selected_path,
            Some(&stale_project_path),
            cwd.path(),
            false,
        )
        .unwrap();

        assert_eq!(
            action,
            ClaudeResumeAction::CopyToCurrent { cwd_projects_dir }
        );
    }
}

fn tui_search_mode(mode: TuiSearchMode) -> ListSearchMode {
    match mode {
        TuiSearchMode::Lexical => ListSearchMode::Lexical,
        TuiSearchMode::Semantic => ListSearchMode::Semantic,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ClaudeResumeAction {
    Run { current_dir: PathBuf },
    CopyToCurrent { cwd_projects_dir: PathBuf },
}

fn resolve_claude_resume_action(
    selected_path: &Path,
    project_path: Option<&PathBuf>,
    cwd: &Path,
    fork_session: bool,
) -> Result<ClaudeResumeAction> {
    let conv_projects_dir = selected_path.parent().ok_or_else(|| {
        AppError::ClaudeExecutionError(
            "Cannot determine conversation's project directory".to_string(),
        )
    })?;
    let cwd_projects_dir = history::get_claude_projects_dir(cwd)?;
    let project_dir = project_path.filter(|p| p.exists() && p.is_dir());

    if project_dir.is_none() || (fork_session && cwd_projects_dir != conv_projects_dir) {
        return Ok(ClaudeResumeAction::CopyToCurrent { cwd_projects_dir });
    }

    if cwd_projects_dir == conv_projects_dir {
        return Ok(ClaudeResumeAction::Run {
            current_dir: cwd.to_path_buf(),
        });
    }

    let project_dir = project_dir.unwrap();
    let project_projects_dir = history::get_claude_projects_dir(project_dir)?;
    if project_projects_dir == conv_projects_dir {
        Ok(ClaudeResumeAction::Run {
            current_dir: project_dir.clone(),
        })
    } else {
        Ok(ClaudeResumeAction::CopyToCurrent { cwd_projects_dir })
    }
}

fn resume_with_claude(
    selected_path: &Path,
    project_path: Option<&PathBuf>,
    default_args: &[String],
    fork_session: bool,
) -> Result<()> {
    let conversation_id = selected_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| {
            AppError::ClaudeExecutionError("Conversation filename is not valid Unicode".to_string())
        })?
        .to_owned();

    let cwd = std::env::current_dir().map_err(|e| {
        AppError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Failed to get current directory: {}", e),
        ))
    })?;

    let conv_projects_dir = selected_path.parent().ok_or_else(|| {
        AppError::ClaudeExecutionError(
            "Cannot determine conversation's project directory".to_string(),
        )
    })?;

    match resolve_claude_resume_action(selected_path, project_path, &cwd, fork_session)? {
        ClaudeResumeAction::CopyToCurrent { cwd_projects_dir } => {
            std::fs::create_dir_all(&cwd_projects_dir).map_err(AppError::Io)?;
            copy_session_files(
                selected_path,
                &conversation_id,
                conv_projects_dir,
                &cwd_projects_dir,
            )?;

            let mut command = Command::new("claude");
            command.args(["--resume", &conversation_id]);
            command.args(default_args);
            command.current_dir(&cwd);
            run_claude_command(command)
        }
        ClaudeResumeAction::Run { current_dir } => {
            let mut command = Command::new("claude");
            command.args(["--resume", &conversation_id]);
            if fork_session {
                command.arg("--fork-session");
            }
            command.args(default_args);
            command.current_dir(current_dir);

            run_claude_command(command)
        }
    }
}

/// Copy session files from one project directory to another for cross-project forking.
///
/// Copies:
/// 1. The .jsonl transcript file
/// 2. The session subdirectory (tool-results/, subagents/) if it exists
/// 3. The file-history directory for undo support if it exists
fn copy_session_files(
    jsonl_path: &Path,
    session_id: &str,
    source_projects_dir: &Path,
    target_projects_dir: &Path,
) -> Result<()> {
    // 1. Copy the .jsonl file
    let target_jsonl = target_projects_dir.join(jsonl_path.file_name().unwrap());
    std::fs::copy(jsonl_path, &target_jsonl).map_err(AppError::Io)?;

    // 2. Copy the session subdirectory (tool-results/, subagents/)
    let session_dir = source_projects_dir.join(session_id);
    if session_dir.is_dir() {
        let target_session_dir = target_projects_dir.join(session_id);
        copy_dir_recursive(&session_dir, &target_session_dir)?;
    }

    // Note: file-history (~/.claude/file-history/<uuid>/) is global, not per-project.
    // Claude Code finds it by session ID, so no copy needed.

    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst).map_err(AppError::Io)?;
    for entry in std::fs::read_dir(src).map_err(AppError::Io)? {
        let entry = entry.map_err(AppError::Io)?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(AppError::Io)?;
        }
    }
    Ok(())
}

fn format_debug_age(age: chrono::Duration) -> String {
    let hours = age.num_hours();
    if hours < 24 {
        format!("{}h", hours)
    } else {
        format!("{}d", hours / 24)
    }
}

#[cfg(unix)]
fn run_claude_command(mut command: Command) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let err = command.exec();
    Err(AppError::ClaudeExecutionError(err.to_string()))
}

#[cfg(not(unix))]
fn run_claude_command(mut command: Command) -> Result<()> {
    let status = command
        .status()
        .map_err(|e| AppError::ClaudeExecutionError(e.to_string()))?;

    if !status.success() {
        return Err(AppError::ClaudeExecutionError(format!(
            "claude CLI exited with status {}",
            status
        )));
    }

    Ok(())
}
