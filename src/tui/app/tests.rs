use super::semantic_test_helpers::*;
use super::*;
use crate::history::Conversation;
use chrono::{Local, TimeZone};
use std::collections::HashMap;
use std::path::PathBuf;

fn conversation(project: Option<&str>, project_dir: &str, uuid: &str, text: &str) -> Conversation {
    Conversation {
        source: crate::history::Source::Claude,
        session_id: uuid.to_owned(),
        path: PathBuf::from(format!("/tmp/claude-projects/{project_dir}/{uuid}.jsonl")),
        index: 0,
        timestamp: Local.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap(),
        preview: text.to_string(),
        preview_first: text.to_string(),
        preview_last: text.to_string(),
        full_text: text.to_string(),
        agent_search_text: String::new(),
        semantic_route_text: String::new(),
        semantic_turns: vec![text.to_string()],
        semantic_turn_ranges: vec![crate::agent::refs::MessageRange::single(1)],
        search_text_lower: search::normalize_for_search(text),
        project_name: project.map(str::to_string),
        project_path: None,
        cwd: None,
        message_count: 1,
        parse_errors: Vec::new(),
        summary: None,
        custom_title: None,
        model: None,
        total_tokens: 0,
        duration_minutes: None,
    }
}

#[test]
fn annotating_takes_its_line_from_the_focused_message() {
    let root = tempfile::tempdir().unwrap();
    let transcript = root.path().join("abc.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"summary","summary":"dropped from the message list"}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}"#,
            "\n",
        ),
    )
    .unwrap();

    let mut app = App::new_single_file(
        transcript,
        crate::tui::ToolDisplayMode::Hidden,
        false,
        crate::config::KeyBindings::default(),
    );
    app.re_render_view(20);
    if let AppMode::View(ref mut state) = app.app_mode {
        state.focused_message = Some(0);
    }

    app.start_annotate();

    // The first message is on file line 2: line 1 is a summary record, which
    // carries no ordinal but still consumes a line.
    match app.dialog_mode {
        DialogMode::Annotate { line, .. } => assert_eq!(line, Some(2)),
        _ => panic!("annotate prompt opened"),
    }
}

#[test]
fn annotating_without_a_focused_message_attaches_to_the_session() {
    let root = tempfile::tempdir().unwrap();
    let transcript = root.path().join("abc.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}"#,
            "\n",
        ),
    )
    .unwrap();

    let mut app = App::new_single_file(
        transcript,
        crate::tui::ToolDisplayMode::Hidden,
        false,
        crate::config::KeyBindings::default(),
    );
    if let AppMode::View(ref mut state) = app.app_mode {
        state.focused_message = None;
    }

    app.start_annotate();

    match app.dialog_mode {
        DialogMode::Annotate { line, .. } => assert_eq!(line, None),
        _ => panic!("annotate prompt opened"),
    }
}

#[test]
fn an_empty_annotation_closes_the_prompt_without_writing() {
    let root = tempfile::tempdir().unwrap();
    let transcript = root.path().join("abc.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}"#,
            "\n",
        ),
    )
    .unwrap();

    let mut app = App::new_single_file(
        transcript,
        crate::tui::ToolDisplayMode::Hidden,
        false,
        crate::config::KeyBindings::default(),
    );
    app.start_annotate();
    app.submit_annotate(20);

    assert!(matches!(app.dialog_mode, DialogMode::None));
    let AppMode::View(state) = &app.app_mode else {
        panic!("still in view mode");
    };
    assert!(state.annotations.is_empty());
}

#[test]
fn mixed_sources_are_identified_and_pi_local_filter_uses_header_cwd() {
    let mut claude = conversation(Some("project"), "-tmp-project", "claude-id", "claude");
    let mut pi = conversation(Some("project"), "ignored", "pi-id", "pi");
    pi.source = crate::history::Source::Pi;
    pi.project_path = Some(std::env::current_dir().unwrap());
    pi.path = PathBuf::from("/tmp/flat-pi-sessions/session.jsonl");
    claude.project_path = Some(PathBuf::from("/tmp/project"));

    let mut app = app(vec![claude, pi], vec![]);
    assert!(app.has_multiple_sources());
    app.workspace_filter = true;
    app.current_project_dir_name = Some(crate::history::convert_path_to_project_dir_name(
        &std::env::current_dir().unwrap(),
    ));
    let filtered = app.filter_indices(0..app.conversations.len());
    assert_eq!(filtered, vec![1]);
}

fn app(conversations: Vec<Conversation>, excluded: Vec<&str>) -> App {
    App::new(
        conversations,
        ToolDisplayMode::Truncated,
        false,
        KeyBindings::default(),
        excluded.into_iter().map(str::to_string).collect(),
    )
}

fn app_with_options(
    conversations: Vec<Conversation>,
    excluded: Vec<&str>,
    search_options: TuiSearchOptions,
) -> App {
    App::new_with_options(
        conversations,
        ToolDisplayMode::Truncated,
        false,
        KeyBindings::default(),
        excluded.into_iter().map(str::to_string).collect(),
        search_options,
    )
}

fn filtered_projects(app: &App) -> Vec<Option<&str>> {
    app.filtered()
        .iter()
        .map(|&idx| app.conversations()[idx].project_name.as_deref())
        .collect()
}

fn app_with_semantic_mode(conversations: Vec<Conversation>) -> App {
    app_with_options(
        conversations,
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    )
}

#[test]
fn default_app_uses_lexical_search_with_semantic_available() {
    let app = app(vec![], vec![]);

    assert_eq!(app.list_search_mode(), ListSearchMode::Lexical);
    assert!(app.semantic_search_available());
    assert_eq!(app.semantic_search.pending_generation, None);
    assert_eq!(app.semantic_search_error(), None);
    assert!(app.semantic_search.results.is_empty());
    assert!(app.semantic_search.worker_tx.is_none());
    assert!(app.semantic_search.worker_rx.is_none());
}

#[test]
fn configured_search_default_uses_semantic_mode() {
    let app = app_with_options(
        vec![],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );

    assert_eq!(app.list_search_mode(), ListSearchMode::Semantic);
    assert!(app.semantic_search_available());
    assert_eq!(app.semantic_search.pending_generation, None);
    assert_eq!(app.semantic_search_error(), None);
}

#[test]
fn semantic_mode_toggle_switches_from_default_lexical() {
    let mut app = app(vec![], vec![]);
    let generation = app.search_generation();

    app.toggle_list_search_mode();

    assert_eq!(app.list_search_mode(), ListSearchMode::Semantic);
    assert!(app.search_generation() > generation);
}

#[test]
fn semantic_mode_toggle_returns_to_lexical_when_enabled() {
    let mut app = app_with_options(
        vec![],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    let generation = app.search_generation();

    app.toggle_list_search_mode();

    assert_eq!(app.list_search_mode(), ListSearchMode::Lexical);
    assert!(app.search_generation() > generation);
}

#[test]
fn exclude_projects_filters_browse_list_exactly() {
    let app = app(
        vec![
            conversation(
                Some("Hidden"),
                "-tmp-hidden",
                "11111111-1111-4111-8111-111111111111",
                "needle",
            ),
            conversation(
                Some("Visible"),
                "-tmp-visible",
                "22222222-2222-4222-8222-222222222222",
                "needle",
            ),
            conversation(
                Some("hidden"),
                "-tmp-lower",
                "33333333-3333-4333-8333-333333333333",
                "needle",
            ),
        ],
        vec!["Hidden"],
    );

    assert_eq!(
        filtered_projects(&app),
        vec![Some("Visible"), Some("hidden")]
    );
}

#[test]
fn exclude_projects_filters_worktrees_by_parent_project() {
    let app = app(
        vec![
            conversation(
                Some("claude-history/exclude-projects"),
                "-tmp-claude-history--worktrees-exclude-projects",
                "11111111-1111-4111-8111-111111111111",
                "needle",
            ),
            conversation(
                Some("other/exclude-projects"),
                "-tmp-other--worktrees-exclude-projects",
                "22222222-2222-4222-8222-222222222222",
                "needle",
            ),
        ],
        vec!["claude-history"],
    );

    assert_eq!(
        filtered_projects(&app),
        vec![Some("other/exclude-projects")]
    );
}

#[test]
fn exclude_projects_filters_search_results() {
    let mut app = app(
        vec![
            conversation(
                Some("Hidden"),
                "-tmp-hidden",
                "11111111-1111-4111-8111-111111111111",
                "shared needle",
            ),
            conversation(
                Some("Visible"),
                "-tmp-visible",
                "22222222-2222-4222-8222-222222222222",
                "shared needle",
            ),
        ],
        vec!["Hidden"],
    );

    app.query = "needle".to_string();
    app.update_filter();

    assert_eq!(filtered_projects(&app), vec![Some("Visible")]);
}

#[test]
fn exclude_projects_apply_before_workspace_filter() {
    let mut app = app(
        vec![
            conversation(
                Some("Hidden"),
                "-tmp-project--worktrees-a",
                "11111111-1111-4111-8111-111111111111",
                "needle",
            ),
            conversation(
                Some("Visible"),
                "-tmp-project",
                "22222222-2222-4222-8222-222222222222",
                "needle",
            ),
        ],
        vec!["Hidden"],
    );
    app.workspace_filter = true;
    app.current_project_dir_name = Some("-tmp-project".to_string());
    app.update_filter();

    assert_eq!(filtered_projects(&app), vec![Some("Visible")]);
}

#[test]
fn uuid_lookup_bypasses_excluded_projects() {
    let uuid = "11111111-1111-4111-8111-111111111111";
    let mut app = app(
        vec![conversation(Some("Hidden"), "-tmp-hidden", uuid, "needle")],
        vec!["Hidden"],
    );
    assert!(app.filtered().is_empty());

    app.query = uuid.to_string();
    app.update_filter();
    assert_eq!(filtered_projects(&app), vec![Some("Hidden")]);

    app.query.clear();
    app.update_filter();
    assert!(app.filtered().is_empty());
    assert_eq!(app.conversations().len(), 1);
    assert_eq!(app.searchable.len(), 1);
}

#[test]
fn uuid_lookup_uses_pi_header_id_instead_of_timestamped_filename() {
    let uuid = "01a016dd-caa0-7ab1-873e-661d81757152";
    let mut pi = conversation(Some("workmux"), "-tmp-workmux", uuid, "needle");
    pi.source = crate::history::Source::Pi;
    pi.path = PathBuf::from(format!(
        "/tmp/pi-sessions/2026-08-18T21-53-49-216Z_{uuid}.jsonl"
    ));
    let mut app = app(vec![pi], vec![]);

    app.query = uuid.to_ascii_uppercase();
    app.update_filter();

    assert_eq!(filtered_projects(&app), vec![Some("workmux")]);
}

#[test]
fn stale_response_with_current_generation_but_old_mode_is_ignored() {
    let mut app = app_with_options(
        vec![conversation(
            Some("Visible"),
            "-tmp-visible",
            "22222222-2222-4222-8222-222222222222",
            "needle",
        )],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );

    let (tx, rx) = mpsc::channel();
    app.search_rx = rx;
    app.list_search_mode = ListSearchMode::Semantic;
    app.search_generation = 7;
    app.filtered.clear();
    app.selected = None;

    tx.send(SearchResponse {
        filtered: vec![0],
        generation: 7,
        mode: ListSearchMode::Lexical,
        evidence: HashMap::new(),
    })
    .unwrap();

    assert!(!app.receive_search_results());
    assert!(app.filtered().is_empty());
    assert_eq!(app.selected(), None);
}

#[test]
fn semantic_empty_query_preserves_default_browse_behavior() {
    let mut app = app_with_options(
        vec![conversation(
            Some("Visible"),
            "-tmp-visible",
            "22222222-2222-4222-8222-222222222222",
            "needle",
        )],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );

    app.query.clear();
    app.dispatch_search();

    assert_eq!(app.list_search_mode(), ListSearchMode::Semantic);
    assert_eq!(filtered_projects(&app), vec![Some("Visible")]);
    assert_eq!(app.semantic_search_error(), None);
    assert!(app.semantic_search.worker_tx.is_none());
    assert!(app.semantic_search.worker_rx.is_none());
}

#[test]
fn semantic_effectively_empty_query_preserves_default_browse_behavior() {
    let mut app = app_with_options(
        vec![conversation(
            Some("Visible"),
            "-tmp-visible",
            "22222222-2222-4222-8222-222222222222",
            "needle",
        )],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );

    app.set_query_for_test("\"\"");
    app.dispatch_search();

    assert_eq!(app.list_search_mode(), ListSearchMode::Semantic);
    assert_eq!(filtered_projects(&app), vec![Some("Visible")]);
    assert_eq!(app.semantic_search_error(), None);
    assert!(app.semantic_search.worker_tx.is_none());
    assert!(app.semantic_search.worker_rx.is_none());
}

#[test]
fn stale_semantic_response_is_ignored_while_lexical_mode_is_active() {
    let mut app = app(vec![], vec![]);
    let (_request_tx, request_rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();
    app.semantic_search.worker_tx = Some(_request_tx);
    app.semantic_search.worker_rx = Some(response_rx);
    app.search_generation = 3;
    app.semantic_search.pending_generation = Some(3);
    drop(request_rx);

    send_semantic_complete_response(
        &response_tx,
        3,
        vec![0],
        HashMap::new(),
        SemanticProgress::Complete,
    );

    assert!(!app.receive_search_results());
    assert!(app.filtered().is_empty());
    assert_eq!(app.selected(), None);
    assert_eq!(app.semantic_search.pending_generation, Some(3));
}

#[test]
fn semantic_response_after_mode_toggle_is_ignored() {
    let mut app = app_with_semantic_mode(vec![conversation(
        Some("Visible"),
        "-tmp-visible",
        "22222222-2222-4222-8222-222222222222",
        "needle",
    )]);
    let (_request_tx, request_rx, response_tx) = connect_semantic_search_channels(&mut app);
    app.query = "needle".to_string();
    app.dispatch_search();
    drop(request_rx);
    let semantic_generation = app.search_generation;

    app.toggle_list_search_mode();
    assert_eq!(app.list_search_mode(), ListSearchMode::Lexical);
    assert_eq!(filtered_projects(&app), vec![Some("Visible")]);

    send_semantic_complete_response(
        &response_tx,
        semantic_generation,
        vec![0],
        HashMap::from([(0, test_semantic_metadata(0, "stale"))]),
        SemanticProgress::Complete,
    );

    assert!(!app.receive_search_results());
    assert_eq!(filtered_projects(&app), vec![Some("Visible")]);
    assert!(app.semantic_search.results.is_empty());
    assert_eq!(app.semantic_search.pending_generation, None);
}

#[test]
fn current_generation_semantic_response_is_ignored_while_lexical_mode_is_active() {
    let mut app = app_with_options(
        vec![conversation(
            Some("Visible"),
            "-tmp-visible",
            "22222222-2222-4222-8222-222222222222",
            "needle",
        )],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    let (_request_tx, request_rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();
    app.semantic_search.worker_tx = Some(_request_tx);
    app.semantic_search.worker_rx = Some(response_rx);
    app.list_search_mode = ListSearchMode::Lexical;
    app.search_generation = 7;
    app.filtered = vec![0];
    app.selected = Some(0);
    drop(request_rx);

    send_semantic_complete_response(
        &response_tx,
        7,
        Vec::new(),
        HashMap::from([(0, test_semantic_metadata(0, "stale"))]),
        SemanticProgress::Complete,
    );

    assert!(!app.receive_search_results());
    assert_eq!(app.filtered(), &[0]);
    assert_eq!(app.selected(), Some(0));
    assert!(app.semantic_search.results.is_empty());
}

#[test]
fn stale_semantic_response_with_old_generation_is_ignored() {
    let mut app = app_with_options(
        vec![conversation(
            Some("Visible"),
            "-tmp-visible",
            "22222222-2222-4222-8222-222222222222",
            "needle",
        )],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    let (_request_tx, request_rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();
    app.semantic_search.worker_tx = Some(_request_tx);
    app.semantic_search.worker_rx = Some(response_rx);
    app.list_search_mode = ListSearchMode::Semantic;
    app.search_generation = 3;
    app.semantic_search.pending_generation = Some(3);
    app.filtered.clear();
    app.selected = None;
    drop(request_rx);

    send_semantic_complete_response(
        &response_tx,
        2,
        vec![0],
        HashMap::from([(0, test_semantic_metadata(0, "stale"))]),
        SemanticProgress::Complete,
    );

    assert!(!app.receive_search_results());
    assert!(app.filtered().is_empty());
    assert_eq!(app.selected(), None);
    assert!(app.semantic_search.results.is_empty());
    assert_eq!(app.semantic_search.pending_generation, Some(3));
}

fn drain_semantic_commands(
    rx: &mpsc::Receiver<SemanticWorkerCommand>,
) -> Vec<SemanticWorkerCommand> {
    let mut commands = Vec::new();
    while let Ok(command) = rx.try_recv() {
        commands.push(command);
    }
    commands
}

fn last_semantic_search(commands: &[SemanticWorkerCommand]) -> Option<(u64, &str, u64, u64, bool)> {
    commands.iter().rev().find_map(|command| match command {
        SemanticWorkerCommand::Search {
            generation,
            query,
            corpus_version,
            scope_version,
            prewarm,
        } => Some((
            *generation,
            query.raw(),
            *corpus_version,
            *scope_version,
            *prewarm,
        )),
        _ => None,
    })
}

fn last_semantic_scope(commands: &[SemanticWorkerCommand]) -> Option<(u64, u64, Vec<usize>)> {
    commands.iter().rev().find_map(|command| match command {
        SemanticWorkerCommand::UpdateScope {
            corpus_version,
            scope_version,
            indices,
        } => Some((*corpus_version, *scope_version, indices.as_ref().clone())),
        _ => None,
    })
}

fn app_with_single_visible_conversation_and_semantic_worker() -> (
    App,
    mpsc::Receiver<SemanticWorkerCommand>,
    mpsc::Sender<crate::tui::semantic_worker::SemanticSearchMessage>,
) {
    let mut app = app_with_options(
        vec![conversation(
            Some("Visible"),
            "-tmp-visible",
            "22222222-2222-4222-8222-222222222222",
            "needle",
        )],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    let (request_tx, request_rx, response_tx) = connect_semantic_search_channels(&mut app);
    drop(request_tx);
    (app, request_rx, response_tx)
}

#[test]
fn semantic_nonempty_query_dispatches_worker_request() {
    let (mut app, request_rx, _response_tx) =
        app_with_single_visible_conversation_and_semantic_worker();

    app.query = "needle".to_string();
    app.dispatch_search();

    let commands = drain_semantic_commands(&request_rx);
    let request = last_semantic_search(&commands).expect("semantic search");
    assert_eq!(app.list_search_mode(), ListSearchMode::Semantic);
    assert!(app.semantic_search_available());
    assert_eq!(app.semantic_search.pending_generation, Some(request.0));
    assert_eq!(request.1, "needle");
    assert!(!request.4);
    assert!(
        commands
            .iter()
            .any(|command| matches!(command, SemanticWorkerCommand::UpdateCorpus { .. }))
    );
    assert_eq!(last_semantic_scope(&commands).unwrap().2, vec![0]);
    assert_eq!(app.semantic_search_error(), None);
}

#[test]
fn semantic_search_dispatches_lexical_fallback() {
    let (mut app, request_rx, _response_tx) =
        app_with_single_visible_conversation_and_semantic_worker();
    let (search_tx, search_rx) = mpsc::channel();
    app.search_tx = search_tx;
    app.query = "needle".to_string();

    app.dispatch_search();

    assert!(last_semantic_search(&drain_semantic_commands(&request_rx)).is_some());
    let SearchCommand::Search {
        query,
        generation,
        mode,
    } = search_rx.try_recv().unwrap()
    else {
        panic!("expected lexical fallback search");
    };
    assert_eq!(query, "needle");
    assert_eq!(generation, app.search_generation());
    assert_eq!(mode, ListSearchMode::Semantic);
}

#[test]
fn semantic_keypress_dispatches_immediately() {
    let mut app = app_with_options(
        vec![conversation(
            Some("Visible"),
            "-tmp-visible",
            "22222222-2222-4222-8222-222222222222",
            "needle",
        )],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    let (request_tx, request_rx) = mpsc::channel();
    let (_response_tx, response_rx) = mpsc::channel();
    app.semantic_search.worker_tx = Some(request_tx);
    app.semantic_search.worker_rx = Some(response_rx);
    let previous_generation = app.search_generation();

    app.handle_key(KeyCode::Char('n'), KeyModifiers::NONE, 10);

    let commands = drain_semantic_commands(&request_rx);
    let request = last_semantic_search(&commands).expect("semantic search");
    assert_eq!(app.query(), "n");
    assert_eq!(app.cursor_pos(), 1);
    assert_eq!(app.search_generation(), previous_generation + 1);
    assert_eq!(app.semantic_search.pending_generation, Some(request.0));
    assert_eq!(app.semantic_search.pending_status, None);
    assert_eq!(app.semantic_activity_status_text(), None);
    assert_eq!(request.1, "n");
    assert!(!request.4);
}

#[test]
fn finish_loading_dispatches_buffered_semantic_query() {
    let mut app = App::new_loading_with_options(
        ToolDisplayMode::Truncated,
        false,
        KeyBindings::default(),
        false,
        None,
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    app.append_conversations(vec![conversation(
        Some("Visible"),
        "-tmp-visible",
        "22222222-2222-4222-8222-222222222222",
        "needle",
    )]);
    let (request_tx, request_rx) = mpsc::channel();
    let (_response_tx, response_rx) = mpsc::channel();
    app.semantic_search.worker_tx = Some(request_tx);
    app.semantic_search.worker_rx = Some(response_rx);
    app.query = "needle".to_string();
    app.cursor_pos = app.query.chars().count();

    app.finish_loading();

    let commands = drain_semantic_commands(&request_rx);
    let request = last_semantic_search(&commands).expect("semantic search");
    assert_eq!(request.1, "needle");
    assert!(!request.4);
    assert_eq!(app.semantic_search.pending_generation, Some(request.0));
}

#[test]
fn semantic_dispatch_after_loading_keeps_snapshot_aligned() {
    let mut app = App::new_loading_with_options(
        ToolDisplayMode::Truncated,
        false,
        KeyBindings::default(),
        false,
        None,
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    app.append_conversations(vec![conversation(
        Some("Visible"),
        "-tmp-visible",
        "22222222-2222-4222-8222-222222222222",
        "needle",
    )]);
    assert!(app.semantic_conversations_snapshot.is_empty());
    let (request_tx, request_rx) = mpsc::channel();
    let (_response_tx, response_rx) = mpsc::channel();
    app.semantic_search.worker_tx = Some(request_tx);
    app.semantic_search.worker_rx = Some(response_rx);

    app.finish_loading();

    let commands = drain_semantic_commands(&request_rx);
    let corpus = commands
        .iter()
        .find_map(|command| match command {
            SemanticWorkerCommand::UpdateCorpus { conversations, .. } => Some(conversations),
            _ => None,
        })
        .expect("semantic corpus");
    assert_eq!(corpus[0].semantic_turns, vec!["needle"]);
}

#[test]
fn semantic_keypress_preserves_browse_rows_while_pending() {
    let (mut app, request_rx, _response_tx) =
        app_with_single_visible_conversation_and_semantic_worker();

    app.handle_key(KeyCode::Char('n'), KeyModifiers::NONE, 10);

    assert!(last_semantic_search(&drain_semantic_commands(&request_rx)).is_some());
    assert_eq!(filtered_projects(&app), vec![Some("Visible")]);
    assert_eq!(app.selected(), Some(0));
}

#[test]
fn semantic_search_worker_returns_lexical_fallback() {
    let app = app_with_options(
        vec![
            conversation(
                Some("Emoji"),
                "-tmp-emoji",
                "11111111-1111-4111-8111-111111111111",
                "emoji picker",
            ),
            conversation(
                Some("Other"),
                "-tmp-other",
                "22222222-2222-4222-8222-222222222222",
                "unrelated",
            ),
        ],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    let (tx, rx) = spawn_search_worker();
    tx.send(SearchCommand::UpdateData {
        conversations: app.conversations_snapshot.clone(),
        searchable: Arc::new(app.searchable.clone()),
    })
    .unwrap();
    tx.send(SearchCommand::Search {
        query: "emoji".to_string(),
        generation: 7,
        mode: ListSearchMode::Semantic,
    })
    .unwrap();

    let response = rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();

    assert_eq!(response.filtered, vec![0]);
    assert_eq!(response.generation, 7);
    assert_eq!(response.mode, ListSearchMode::Semantic);
}

#[test]
fn semantic_search_applies_lexical_fallback_while_pending() {
    let mut app = app_with_options(
        vec![
            conversation(
                Some("Emoji"),
                "-tmp-emoji",
                "11111111-1111-4111-8111-111111111111",
                "emoji picker",
            ),
            conversation(
                Some("Other"),
                "-tmp-other",
                "22222222-2222-4222-8222-222222222222",
                "unrelated",
            ),
        ],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    let (tx, rx) = mpsc::channel();
    app.search_rx = rx;
    app.search_generation = 7;
    app.semantic_search.pending_generation = Some(7);
    app.semantic_search.results = HashMap::from([(1, test_semantic_metadata(1, "old"))]);
    app.filtered = vec![1];
    app.selected = Some(0);
    tx.send(SearchResponse {
        filtered: vec![0],
        generation: 7,
        mode: ListSearchMode::Semantic,
        evidence: HashMap::new(),
    })
    .unwrap();

    assert!(app.receive_search_results());
    assert_eq!(app.filtered(), &[0]);
    assert_eq!(app.selected(), Some(0));
    assert!(app.semantic_search.results.is_empty());
}

#[test]
fn semantic_search_ignores_lexical_fallback_after_completion() {
    let mut app = app_with_semantic_mode(vec![conversation(
        Some("Visible"),
        "-tmp-visible",
        "22222222-2222-4222-8222-222222222222",
        "needle",
    )]);
    let (tx, rx) = mpsc::channel();
    app.search_rx = rx;
    app.search_generation = 7;
    app.semantic_search.pending_generation = None;
    app.filtered = vec![0];
    app.selected = Some(0);
    tx.send(SearchResponse {
        filtered: Vec::new(),
        generation: 7,
        mode: ListSearchMode::Semantic,
        evidence: HashMap::new(),
    })
    .unwrap();

    assert!(!app.receive_search_results());
    assert_eq!(app.filtered(), &[0]);
    assert_eq!(app.selected(), Some(0));
}

#[test]
fn semantic_keypress_does_not_clone_full_corpus_on_ui_thread() {
    let conversations = (0..150)
        .map(|index| {
            conversation(
                Some("Visible"),
                &format!("-tmp-visible-{index}"),
                &format!("22222222-2222-4222-8222-{index:012}"),
                "needle",
            )
        })
        .collect::<Vec<_>>();
    let mut app = app_with_options(
        conversations,
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    let snapshot = app.semantic_conversations_snapshot.clone();
    let (request_tx, request_rx) = mpsc::channel();
    let (_response_tx, response_rx) = mpsc::channel();
    app.semantic_search.worker_tx = Some(request_tx);
    app.semantic_search.worker_rx = Some(response_rx);

    app.handle_key(KeyCode::Char('n'), KeyModifiers::NONE, 10);

    let commands = drain_semantic_commands(&request_rx);
    let corpus = commands
        .iter()
        .find_map(|command| match command {
            SemanticWorkerCommand::UpdateCorpus { conversations, .. } => Some(conversations),
            _ => None,
        })
        .expect("semantic corpus");
    assert_eq!(corpus.len(), 150);
    for (index, conversation) in corpus.iter().enumerate() {
        assert!(Arc::ptr_eq(conversation, &snapshot[index]));
    }
    let request = last_semantic_search(&commands).expect("semantic search");
    assert_eq!(request.1, "n");
}

#[test]
fn semantic_mode_prewarms_cache_without_query() {
    let (mut app, request_rx, _response_tx) =
        app_with_single_visible_conversation_and_semantic_worker();
    app.invalidate_search_generation();

    app.prewarm_semantic_cache();

    let commands = drain_semantic_commands(&request_rx);
    let request = last_semantic_search(&commands).expect("semantic prewarm request");
    assert_eq!(request.1, "");
    assert!(request.4);
    assert_eq!(last_semantic_scope(&commands).unwrap().2, vec![0]);
    assert_eq!(app.semantic_search.pending_generation, Some(request.0));
}

#[test]
fn semantic_request_uses_live_conversations_not_stale_snapshot() {
    let mut app = app_with_options(
        vec![conversation(
            Some("Visible"),
            "-tmp-visible",
            "22222222-2222-4222-8222-222222222222",
            "needle",
        )],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    app.conversations_snapshot = Arc::new(Vec::new());
    let (request_tx, request_rx) = mpsc::channel();
    let (_response_tx, response_rx) = mpsc::channel();
    app.semantic_search.worker_tx = Some(request_tx);
    app.semantic_search.worker_rx = Some(response_rx);

    app.query = "needle".to_string();
    app.dispatch_search();

    let commands = drain_semantic_commands(&request_rx);
    let corpus = commands
        .iter()
        .find_map(|command| match command {
            SemanticWorkerCommand::UpdateCorpus { conversations, .. } => Some(conversations),
            _ => None,
        })
        .expect("semantic corpus");
    assert_eq!(corpus.len(), 1);
    assert_eq!(corpus[0].semantic_turns, vec!["needle"]);
}

#[test]
fn semantic_query_keeps_existing_metadata_while_pending() {
    let mut app = app_with_options(
        vec![conversation(
            Some("Visible"),
            "-tmp-visible",
            "22222222-2222-4222-8222-222222222222",
            "needle",
        )],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    app.list_search_mode = ListSearchMode::Semantic;
    app.semantic_search.results = HashMap::from([(0, test_semantic_metadata(0, "old"))]);
    let (request_tx, request_rx) = mpsc::channel();
    let (_response_tx, response_rx) = mpsc::channel();
    app.semantic_search.worker_tx = Some(request_tx);
    app.semantic_search.worker_rx = Some(response_rx);

    app.query = "needle".to_string();
    app.dispatch_search();

    let commands = drain_semantic_commands(&request_rx);
    assert!(last_semantic_search(&commands).is_some());
    assert!(app.semantic_search.results.contains_key(&0));
}

#[test]
fn semantic_scope_indices_apply_scope() {
    let mut app = app_with_options(
        vec![
            conversation(
                Some("Hidden"),
                "-tmp-hidden",
                "11111111-1111-4111-8111-111111111111",
                "hidden",
            ),
            conversation(
                Some("Visible"),
                "-tmp-visible",
                "22222222-2222-4222-8222-222222222222",
                "visible",
            ),
            conversation(
                Some("Other"),
                "-tmp-other",
                "33333333-3333-4333-8333-333333333333",
                "other",
            ),
        ],
        vec!["Hidden"],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    app.current_project_dir_name = Some("-tmp-visible".to_string());
    app.workspace_filter = true;

    let indices = app.semantic_scope_indices();
    assert_eq!(indices.as_ref(), &vec![1]);
}

#[test]
fn semantic_response_applies_ranked_indices_and_metadata() {
    let mut app = app_with_options(
        vec![
            conversation(
                Some("Visible"),
                "-tmp-visible",
                "22222222-2222-4222-8222-222222222222",
                "needle",
            ),
            conversation(
                Some("Other"),
                "-tmp-other",
                "33333333-3333-4333-8333-333333333333",
                "other",
            ),
        ],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    let (_request_tx, request_rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();
    app.semantic_search.worker_tx = Some(_request_tx);
    app.semantic_search.worker_rx = Some(response_rx);
    app.list_search_mode = ListSearchMode::Semantic;
    app.search_generation = 7;
    app.search_in_flight = true;
    app.semantic_search.pending_generation = Some(7);
    app.filtered.clear();
    app.selected = None;
    drop(request_rx);
    let metadata = HashMap::from([(1, test_semantic_metadata(1, "visible preview"))]);

    send_semantic_complete_response(
        &response_tx,
        7,
        vec![1],
        metadata,
        SemanticProgress::Complete,
    );

    assert!(app.receive_search_results());
    assert_eq!(app.filtered(), &[1]);
    assert_eq!(app.selected(), Some(0));
    assert_eq!(app.semantic_search.pending_generation, None);
    assert!(!app.search_in_flight);
    assert_eq!(
        app.semantic_search.results[&1].explanation.evidence_preview,
        "visible preview"
    );
    assert_eq!(app.semantic_search.results[&1].score_breakdown.hybrid, 1.0);
}

#[test]
fn semantic_empty_query_clears_error() {
    let mut app = app_with_options(
        vec![conversation(
            Some("Visible"),
            "-tmp-visible",
            "22222222-2222-4222-8222-222222222222",
            "needle",
        )],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );

    app.toggle_list_search_mode();
    app.semantic_search.error = Some("failed".to_string());

    app.query.clear();
    app.dispatch_search();

    assert_eq!(filtered_projects(&app), vec![Some("Visible")]);
    assert_eq!(app.semantic_search_error(), None);
}

#[test]
fn semantic_uuid_query_uses_uuid_lookup_and_clears_unsupported_error() {
    let uuid = "11111111-1111-4111-8111-111111111111";
    let mut app = app_with_options(
        vec![conversation(Some("Hidden"), "-tmp-hidden", uuid, "needle")],
        vec!["Hidden"],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    app.toggle_list_search_mode();
    app.semantic_search.error = Some("failed".to_string());

    app.query = uuid.to_string();
    app.dispatch_search();

    assert_eq!(filtered_projects(&app), vec![Some("Hidden")]);
    assert_eq!(app.semantic_search_error(), None);
    assert!(app.semantic_search.worker_tx.is_none());
    assert!(app.semantic_search.worker_rx.is_none());
}

#[test]
fn semantic_progress_messages_update_activity_status_text() {
    let mut app = app_with_semantic_mode(vec![conversation(
        Some("Visible"),
        "-tmp-visible",
        "22222222-2222-4222-8222-222222222222",
        "needle",
    )]);
    let (_request_tx, request_rx, response_tx) = connect_semantic_search_channels(&mut app);
    app.list_search_mode = ListSearchMode::Semantic;
    app.search_generation = 7;
    app.semantic_search.pending_generation = Some(7);
    drop(request_rx);

    send_semantic_progress_response(
        &response_tx,
        7,
        SemanticProgress::Embedding {
            completed: 1,
            total: 2,
        },
    );

    assert!(app.receive_search_results());
    assert_eq!(app.semantic_status_text(), None);
    assert_eq!(
        app.semantic_activity_status_text().as_deref(),
        Some("sem embedding 50%  1/2 chunks")
    );
    assert_eq!(app.semantic_search.pending_generation, Some(7));
}

#[test]
fn clearing_query_preserves_in_flight_prewarm_preparing_status() {
    let mut app = app_with_options(
        vec![conversation(
            Some("Visible"),
            "-tmp-visible",
            "22222222-2222-4222-8222-222222222222",
            "needle",
        )],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    app.list_search_mode = ListSearchMode::Semantic;
    app.search_generation = 10;
    app.semantic_search.pending_generation = Some(10);
    app.semantic_search.prewarm_generation = Some(9);
    app.semantic_search.prewarm_status = Some(SemanticProgress::InitializingModel);
    app.query = "needle".to_string();

    app.query.clear();
    app.dispatch_search();

    assert_eq!(
        app.semantic_activity_status_text().as_deref(),
        Some("sem preparing embeddings")
    );
}

#[test]
fn clearing_query_preserves_in_flight_prewarm_progress() {
    let mut app = app_with_options(
        vec![conversation(
            Some("Visible"),
            "-tmp-visible",
            "22222222-2222-4222-8222-222222222222",
            "needle",
        )],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    app.list_search_mode = ListSearchMode::Semantic;
    app.search_generation = 10;
    app.semantic_search.pending_generation = Some(10);
    app.semantic_search.prewarm_generation = Some(9);
    app.semantic_search.prewarm_status = Some(SemanticProgress::Embedding {
        completed: 3,
        total: 10,
    });
    app.query = "needle".to_string();

    app.query.clear();
    app.dispatch_search();

    assert_eq!(
        app.semantic_activity_status_text().as_deref(),
        Some("sem embedding 30%  3/10 chunks")
    );
}

#[test]
fn query_ranking_status_does_not_use_activity_bar() {
    let mut app = app_with_options(
        vec![conversation(
            Some("Visible"),
            "-tmp-visible",
            "22222222-2222-4222-8222-222222222222",
            "needle",
        )],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    app.list_search_mode = ListSearchMode::Semantic;
    app.semantic_search.prewarm_generation = None;
    app.semantic_search.prewarm_status = None;
    app.semantic_search.pending_generation = Some(10);
    app.semantic_search.pending_status = Some(SemanticProgress::Ranking);

    assert_eq!(app.semantic_activity_status_text(), None);
}

#[test]
fn prewarm_generation_keeps_search_polling_until_completion() {
    let mut app = app_with_options(
        vec![conversation(
            Some("Visible"),
            "-tmp-visible",
            "22222222-2222-4222-8222-222222222222",
            "needle",
        )],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    app.list_search_mode = ListSearchMode::Semantic;
    app.search_generation = 10;
    app.semantic_search.prewarm_generation = Some(9);
    app.semantic_search.prewarm_status = Some(SemanticProgress::Embedding {
        completed: 10,
        total: 10,
    });

    assert!(app.has_search_work_in_flight());
}

#[test]
fn semantic_query_interrupts_prewarm_and_keeps_activity_until_query_starts() {
    let mut app = app_with_options(
        vec![conversation(
            Some("Visible"),
            "-tmp-visible",
            "22222222-2222-4222-8222-222222222222",
            "needle",
        )],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    let (request_tx, _request_rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();
    app.semantic_search.worker_tx = Some(request_tx);
    app.semantic_search.worker_rx = Some(response_rx);
    app.list_search_mode = ListSearchMode::Semantic;
    app.search_generation = 9;
    app.semantic_search.prewarm_generation = Some(9);
    app.semantic_search.prewarm_status = Some(SemanticProgress::Embedding {
        completed: 3,
        total: 10,
    });
    let cancellation = crate::semantic::types::SemanticCancellationToken::new();
    let active_cancellation = cancellation.child();
    app.semantic_search.cancellation = Some(cancellation);
    app.query = "needle".to_string();

    app.dispatch_search();
    let real_generation = app.search_generation();

    assert!(active_cancellation.is_cancelled());
    assert_eq!(app.semantic_search.prewarm_generation, Some(9));
    assert_eq!(
        app.semantic_activity_status_text().as_deref(),
        Some("sem embedding 30%  3/10 chunks")
    );

    send_semantic_progress_response(&response_tx, real_generation, SemanticProgress::Ranking);
    assert!(app.receive_search_results());
    assert_eq!(app.semantic_search.prewarm_generation, None);
    assert_eq!(app.semantic_search.prewarm_status, None);

    send_semantic_complete_response(
        &response_tx,
        real_generation,
        vec![0],
        HashMap::new(),
        SemanticProgress::Complete,
    );

    assert!(app.receive_search_results());
    assert!(!app.has_search_work_in_flight());
    assert_eq!(app.semantic_activity_status_text(), None);
}

#[test]
fn semantic_empty_corpus_status_is_visible_after_completion() {
    let mut app = app_with_semantic_mode(vec![conversation(
        Some("Visible"),
        "-tmp-visible",
        "22222222-2222-4222-8222-222222222222",
        "needle",
    )]);
    let (_request_tx, request_rx, response_tx) = connect_semantic_search_channels(&mut app);
    app.list_search_mode = ListSearchMode::Semantic;
    app.search_generation = 7;
    app.semantic_search.pending_generation = Some(7);
    drop(request_rx);

    send_semantic_complete_response(
        &response_tx,
        7,
        Vec::new(),
        HashMap::new(),
        SemanticProgress::EmptyCorpus,
    );

    assert!(app.receive_search_results());
    assert_eq!(app.semantic_status_text().as_deref(), Some("sem no text"));
    assert!(app.filtered().is_empty());
    assert_eq!(app.selected(), None);
}

#[test]
fn lexical_toggle_clears_semantic_error_and_pending_status() {
    let mut app = app_with_options(
        vec![conversation(
            Some("Visible"),
            "-tmp-visible",
            "22222222-2222-4222-8222-222222222222",
            "needle",
        )],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    app.list_search_mode = ListSearchMode::Semantic;
    app.query = "needle".to_string();
    app.semantic_search.pending_generation = Some(3);
    app.semantic_search.pending_status = Some(SemanticProgress::Ranking);
    app.semantic_search.error = Some("failed".to_string());

    app.toggle_list_search_mode();

    assert_eq!(app.list_search_mode(), ListSearchMode::Lexical);
    assert_eq!(app.semantic_search.pending_generation, None);
    assert_eq!(app.semantic_search.pending_status, None);
    assert_eq!(app.semantic_search_error(), None);
    assert_eq!(filtered_projects(&app), vec![Some("Visible")]);
}

#[test]
fn ctrl_t_toggles_to_lexical_mode_when_semantic_session_active() {
    let mut app = app_with_options(
        vec![],
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );

    app.handle_key(KeyCode::Char('t'), KeyModifiers::CONTROL, 10);

    assert_eq!(app.list_search_mode(), ListSearchMode::Lexical);
}

#[test]
fn configured_ctrl_t_binding_takes_precedence_over_semantic_toggle() {
    let keys = KeyBindings {
        rename: crate::config::KeyBinding {
            code: KeyCode::Char('t'),
            modifiers: KeyModifiers::CONTROL,
        },
        ..Default::default()
    };
    let mut app = App::new_with_options(
        vec![conversation(
            Some("Visible"),
            "-tmp-visible",
            "22222222-2222-4222-8222-222222222222",
            "needle",
        )],
        ToolDisplayMode::Truncated,
        false,
        keys,
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );

    app.handle_key(KeyCode::Char('t'), KeyModifiers::CONTROL, 10);

    assert_eq!(app.list_search_mode(), ListSearchMode::Semantic);
    assert!(matches!(app.dialog_mode, DialogMode::Rename { .. }));
}

#[test]
fn workspace_toggle_dispatches_new_semantic_request() {
    let (mut app, request_rx, _response_tx) =
        app_with_single_visible_conversation_and_semantic_worker();
    app.current_project_dir_name = Some("-tmp-visible".to_string());
    app.query = "needle".to_string();

    app.toggle_workspace_filter();

    let commands = drain_semantic_commands(&request_rx);
    assert_eq!(last_semantic_scope(&commands).unwrap().2, vec![0]);
    assert_eq!(last_semantic_search(&commands).unwrap().1, "needle");
    assert_eq!(filtered_projects(&app), vec![Some("Visible")]);
    assert_eq!(app.selected(), Some(0));
    assert_eq!(app.semantic_search_error(), None);
}

#[test]
fn uuid_dispatch_invalidates_stale_search_response() {
    let uuid = "11111111-1111-4111-8111-111111111111";
    let mut app = app(
        vec![
            conversation(Some("Hidden"), "-tmp-hidden", uuid, "needle"),
            conversation(
                Some("Visible"),
                "-tmp-visible",
                "22222222-2222-4222-8222-222222222222",
                "needle",
            ),
        ],
        vec!["Hidden"],
    );

    let (tx, rx) = mpsc::channel();
    app.search_rx = rx;
    app.search_generation = 1;
    app.search_in_flight = true;

    app.query = uuid.to_string();
    app.dispatch_search();
    assert_eq!(filtered_projects(&app), vec![Some("Hidden")]);

    tx.send(SearchResponse {
        filtered: vec![1],
        generation: 1,
        mode: ListSearchMode::Lexical,
        evidence: HashMap::new(),
    })
    .unwrap();

    app.receive_search_results();
    assert_eq!(filtered_projects(&app), vec![Some("Hidden")]);
}

#[test]
fn finish_loading_invalidates_stale_loading_search_response() {
    let mut app = App::new_loading_with_options(
        ToolDisplayMode::Truncated,
        false,
        KeyBindings::default(),
        false,
        None,
        vec![],
        TuiSearchOptions::default(),
    );

    let (tx, rx) = mpsc::channel();
    app.search_rx = rx;
    app.search_generation = 1;
    app.search_in_flight = true;

    app.append_conversations(vec![conversation(
        Some("Visible"),
        "-tmp-visible",
        "22222222-2222-4222-8222-222222222222",
        "needle",
    )]);
    app.finish_loading();
    assert_eq!(filtered_projects(&app), vec![Some("Visible")]);

    tx.send(SearchResponse {
        filtered: vec![],
        generation: 1,
        mode: ListSearchMode::Lexical,
        evidence: HashMap::new(),
    })
    .unwrap();

    app.receive_search_results();
    assert_eq!(filtered_projects(&app), vec![Some("Visible")]);
}

#[test]
fn workspace_filter_without_project_context_keeps_rows() {
    let mut app = App::new_loading_with_options(
        ToolDisplayMode::Truncated,
        false,
        KeyBindings::default(),
        true,
        None,
        vec![],
        TuiSearchOptions::default(),
    );

    app.append_conversations(vec![conversation(
        Some("Visible"),
        "-tmp-visible",
        "22222222-2222-4222-8222-222222222222",
        "needle",
    )]);

    assert_eq!(filtered_projects(&app), vec![Some("Visible")]);
}

#[test]
fn exclude_projects_filters_incremental_loading() {
    let mut app = App::new_loading_with_options(
        ToolDisplayMode::Truncated,
        false,
        KeyBindings::default(),
        false,
        None,
        vec!["Hidden".to_string()],
        TuiSearchOptions::default(),
    );

    app.append_conversations(vec![
        conversation(
            Some("Hidden"),
            "-tmp-hidden",
            "11111111-1111-4111-8111-111111111111",
            "needle",
        ),
        conversation(
            Some("Visible"),
            "-tmp-visible",
            "22222222-2222-4222-8222-222222222222",
            "needle",
        ),
    ]);

    assert_eq!(filtered_projects(&app), vec![Some("Visible")]);
}

#[test]
fn empty_exclusions_preserve_browse_results() {
    let app = app(
        vec![
            conversation(
                Some("Hidden"),
                "-tmp-hidden",
                "11111111-1111-4111-8111-111111111111",
                "needle",
            ),
            conversation(
                None,
                "-tmp-none",
                "22222222-2222-4222-8222-222222222222",
                "needle",
            ),
        ],
        vec![],
    );

    assert_eq!(filtered_projects(&app), vec![Some("Hidden"), None]);
}

#[test]
fn project_without_name_is_never_excluded() {
    let app = app(
        vec![conversation(
            None,
            "-tmp-none",
            "11111111-1111-4111-8111-111111111111",
            "needle",
        )],
        vec![""],
    );

    assert_eq!(filtered_projects(&app), vec![None]);
}

#[test]
fn single_file_mode_has_no_project_exclusions() {
    let app = App::new_single_file(
        PathBuf::from("/tmp/hidden.jsonl"),
        ToolDisplayMode::Truncated,
        false,
        KeyBindings::default(),
    );

    assert!(app.excluded_projects.is_empty());
    assert!(app.is_single_file_mode());
}

/// Writes one sidecar for `conversation` under a fresh annotations root and
/// returns that root, mirroring the layout the file annotator reads.
fn annotations_root_with_note(
    dir: &std::path::Path,
    conversation: &Conversation,
    text: &str,
) -> PathBuf {
    let root = dir.join("annotations");
    let project = conversation
        .path
        .parent()
        .and_then(|parent| parent.file_name())
        .expect("conversation sits under a project directory");
    let project_dir = root.join(project);
    std::fs::create_dir_all(&project_dir).unwrap();
    let sidecar = project_dir.join(format!("{}.jsonl", conversation.session_id));
    std::fs::write(
        sidecar,
        format!(
            "{}\n",
            serde_json::json!({"id": "note-1", "kind": "note", "text": text})
        ),
    )
    .unwrap();
    root
}

#[test]
fn a_note_makes_its_conversation_match_a_list_search() {
    let dir = tempfile::tempdir().unwrap();
    let conv = conversation(
        Some("Visible"),
        "-tmp-visible",
        "22222222-2222-4222-8222-222222222222",
        "transcript body with no such word",
    );
    let root = annotations_root_with_note(dir.path(), &conv, "pelican crossing");

    let mut app = app(vec![conv], vec![]);
    app.set_annotations_root_for_test(root);
    app.finish_loading();

    app.query = "pelican".to_string();
    app.update_filter();

    assert_eq!(filtered_projects(&app), vec![Some("Visible")]);
}

#[test]
fn note_text_reaches_the_field_the_evidence_line_is_drawn_from() {
    let dir = tempfile::tempdir().unwrap();
    let conv = conversation(
        Some("Visible"),
        "-tmp-visible",
        "22222222-2222-4222-8222-222222222222",
        "transcript body with no such word",
    );
    let root = annotations_root_with_note(dir.path(), &conv, "pelican crossing");

    let mut app = app(vec![conv], vec![]);
    app.set_annotations_root_for_test(root);
    app.finish_loading();

    // The evidence builder reads full_text and skips what the preview already
    // shows, so the note's presence there is what puts it on the row.
    assert!(
        app.conversations()[0]
            .full_text
            .contains("pelican crossing"),
        "{}",
        app.conversations()[0].full_text
    );
    let evidence = crate::search::build_lexical_evidence(
        &app.conversations()[0],
        &crate::search::query::ParsedQuery::parse("pelican"),
    )
    .expect("evidence for a note-only match");
    assert!(!evidence.context_ranges.is_empty());
}

#[test]
fn enrichment_appends_one_copy_of_a_note() {
    let dir = tempfile::tempdir().unwrap();
    let conv = conversation(
        Some("Visible"),
        "-tmp-visible",
        "22222222-2222-4222-8222-222222222222",
        "transcript body",
    );
    let root = annotations_root_with_note(dir.path(), &conv, "pelican crossing");

    let mut app = app(vec![conv], vec![]);
    app.set_annotations_root_for_test(root);
    app.finish_loading();
    // A second pass would double the note's text and with it its lexical weight.
    app.finish_loading();

    assert_eq!(
        app.conversations()[0].full_text.matches("pelican").count(),
        1,
        "{}",
        app.conversations()[0].full_text
    );
}
