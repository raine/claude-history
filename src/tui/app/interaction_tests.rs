use super::semantic_test_helpers::*;
use super::*;
use crate::config::KeyBinding;
use chrono::TimeZone;

fn test_conversation(path: PathBuf, custom_title: Option<String>) -> Conversation {
    let mut full_text = "hello body".to_string();
    if let Some(title) = &custom_title {
        full_text = format!("{} {}", title, full_text);
    }
    Conversation {
        source: crate::history::model::SourceKind::Claude,
        session_id: path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned(),
        path,
        index: 0,
        timestamp: Local.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
        preview: "hello body".into(),
        preview_first: "hello body".into(),
        preview_last: "hello body".into(),
        semantic_turns: vec!["hello body".to_string()].into(),
        semantic_turn_ranges: vec![crate::agent::refs::MessageRange::single(1)],
        full_text: full_text.into(),
        own_text: "".into(),
        agent_search_text: "".into(),
        semantic_route_text: "".into(),
        project_name: Some("project".to_string()),
        project_path: None,
        cwd: None,
        cwds: Vec::new(),
        message_count: 1,
        prompt_count: 1,
        tool_call_count: 0,
        parse_errors: Vec::new(),
        summary: None,
        custom_title,
        model: None,
        total_tokens: 0,
        duration_minutes: None,
        provenance: Default::default(),
        parent: None,
    }
}

fn app_with_conversation(path: PathBuf, custom_title: Option<String>) -> App {
    App::new(
        vec![test_conversation(path, custom_title)],
        ToolDisplayMode::Summary,
        false,
        KeyBindings::default(),
        vec![],
    )
}

fn write_conversation(path: &std::path::Path, title: Option<&str>) {
    let mut lines = vec![r#"{"type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"hello body"}}"#.to_string()];
    if let Some(title) = title {
        lines.push(format!(
            r#"{{"type":"custom-title","customTitle":"{}","sessionId":"abc123"}}"#,
            title
        ));
    }
    std::fs::write(path, lines.join("\n") + "\n").unwrap();
}

fn write_named_conversation(path: &std::path::Path, text: &str) {
    let line = serde_json::json!({
        "type": "user",
        "timestamp": "2024-01-01T00:00:00Z",
        "message": {"role": "user", "content": text}
    })
    .to_string();
    std::fs::write(path, format!("{line}\n")).unwrap();
}

fn write_tool_conversation(path: &std::path::Path) {
    let line = r#"{"type":"assistant","timestamp":"2024-01-01T00:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"one\ntwo\nthree\nfour\nfive"}}]}}"#;
    std::fs::write(path, format!("{line}\n")).unwrap();
}

fn app_with_tool_conversation(path: PathBuf) -> App {
    let mut app = App::new(
        vec![test_conversation(path, None)],
        ToolDisplayMode::Truncated,
        false,
        KeyBindings::default(),
        vec![],
    );
    app.selected = Some(0);
    app.enter_view_mode(80);
    app
}

fn tool_click_row(app: &App, frame: Rect) -> u16 {
    if let AppMode::View(state) = app.app_mode() {
        let layout = ui::view_layout_rects(frame, app, state);
        let idx = state
            .rendered_lines
            .iter()
            .position(|line| line.clickable)
            .unwrap();
        layout.content.y + (idx - state.scroll_offset) as u16
    } else {
        unreachable!()
    }
}

fn view_text(app: &App) -> String {
    if let AppMode::View(state) = app.app_mode() {
        state
            .rendered_lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|(text, _)| text.as_str()))
            .collect()
    } else {
        unreachable!()
    }
}

fn view_expanded_tool_id(app: &App) -> ToolOutputId {
    if let AppMode::View(state) = app.app_mode() {
        assert_eq!(state.expanded_tool_outputs.len(), 1);
        state.expanded_tool_outputs.iter().next().unwrap().clone()
    } else {
        unreachable!()
    }
}

fn assert_cached_tool_output_survives_file_removal(
    app: &mut App,
    expect_hovered_tool_output_retained: bool,
) {
    let frame = Rect::new(0, 0, 120, 20);
    let row = tool_click_row(app, frame);

    assert!(app.handle_view_click(row, frame, 17));
    let expanded_id = if expect_hovered_tool_output_retained {
        Some(view_expanded_tool_id(app))
    } else {
        None
    };
    assert!(view_text(app).contains("five"));

    assert!(app.handle_view_click(row, frame, 17));
    if let AppMode::View(state) = app.app_mode() {
        assert!(state.expanded_tool_outputs.is_empty());
        if expect_hovered_tool_output_retained {
            assert_eq!(state.hovered_tool_output, expanded_id);
        }
    }
    assert!(!view_text(app).contains("five"));
}

#[test]
fn semantic_ranked_selection_opens_selected_conversation_and_returns() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.jsonl");
    let second = dir.path().join("second.jsonl");
    write_named_conversation(&first, "first body");
    write_named_conversation(&second, "second body");
    let mut app = App::new_with_options(
        vec![
            test_conversation(first.clone(), None),
            test_conversation(second.clone(), None),
        ],
        ToolDisplayMode::Summary,
        false,
        KeyBindings::default(),
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
            ..Default::default()
        },
    );
    let (_request_tx, _request_rx, response_tx) = connect_semantic_search_channels(&mut app);
    app.list_search_mode = ListSearchMode::Semantic;
    app.search_generation = 7;
    app.semantic_search.pending_generation = Some(7);

    send_semantic_complete_response(
        &response_tx,
        7,
        vec![1, 0],
        HashMap::from([(1, test_semantic_metadata(1, "second semantic preview"))]),
        SemanticProgress::Complete,
    );

    assert!(app.receive_search_results());
    assert_eq!(app.filtered(), &[1, 0]);
    assert_eq!(app.selected(), Some(0));
    app.enter_view_mode(80);
    assert!(matches!(app.app_mode(), AppMode::View(_)));
    if let AppMode::View(state) = app.app_mode() {
        assert_eq!(state.conversation_path, second);
    }
    assert!(view_text(&app).contains("second body"));

    app.exit_view_mode();

    assert!(matches!(app.app_mode(), AppMode::List));
    assert_eq!(app.filtered(), &[1, 0]);
    assert_eq!(app.selected(), Some(0));
}

#[test]
fn semantic_list_click_uses_three_line_rows() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.jsonl");
    let second = dir.path().join("second.jsonl");
    write_named_conversation(&first, "first body");
    write_named_conversation(&second, "second body");
    let mut app = App::new_with_options(
        vec![
            test_conversation(first, None),
            test_conversation(second, None),
        ],
        ToolDisplayMode::Summary,
        false,
        KeyBindings::default(),
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
            ..Default::default()
        },
    );
    let (_request_tx, request_rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();
    app.semantic_search.worker_tx = Some(_request_tx);
    app.semantic_search.worker_rx = Some(response_rx);
    app.list_search_mode = ListSearchMode::Semantic;
    app.query = "needle".to_string();
    app.cursor_pos = app.query.chars().count();
    app.search_generation = 7;
    app.semantic_search.pending_generation = Some(7);
    drop(request_rx);
    send_semantic_complete_response(
        &response_tx,
        7,
        vec![0, 1],
        HashMap::from([
            (0, test_semantic_metadata(0, "first")),
            (1, test_semantic_metadata(1, "second")),
        ]),
        SemanticProgress::Complete,
    );
    app.receive_search_results();
    let frame = Rect::new(0, 0, 80, 20);

    assert!(app.handle_list_click(1, 6, frame));

    assert_eq!(app.selected(), Some(1));
}

#[test]
fn view_click_toggles_clickable_output() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tool.jsonl");
    write_tool_conversation(&path);
    let mut app = app_with_tool_conversation(path);
    let frame = Rect::new(0, 0, 120, 20);
    let row = tool_click_row(&app, frame);

    assert!(app.handle_view_click(row, frame, 17));
    let expanded_id = view_expanded_tool_id(&app);
    assert!(view_text(&app).contains("five"));

    assert!(app.handle_view_click(row, frame, 17));
    if let AppMode::View(state) = app.app_mode() {
        assert!(state.expanded_tool_outputs.is_empty());
        assert_eq!(state.hovered_tool_output, Some(expanded_id));
    }
}

#[test]
fn cycling_tools_to_off_keeps_focus_on_a_visible_message() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tools.jsonl");
    let lines = [
        r#"{"type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"please check"}}"#,
        r#"{"type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"cargo test"}}]}}"#,
        r#"{"type":"user","timestamp":"2024-01-01T00:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"ok"}]}}"#,
        r#"{"type":"assistant","timestamp":"2024-01-01T00:00:03Z","message":{"role":"assistant","content":[{"type":"text","text":"all good"}]}}"#,
    ];
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    let mut app = app_with_conversation(path, None);
    app.selected = Some(0);
    app.enter_view_mode(80);

    // Focus the tool summary row, then cycle summary → calls → truncated →
    // full → off.
    if let AppMode::View(ref mut state) = app.app_mode {
        assert_eq!(state.message_ranges[1].entry_index, 1);
        state.focused_message = Some(1);
        state.message_nav_active = true;
    }
    assert!(view_text(&app).contains("Ran 1 shell command"));
    for _ in 0..4 {
        app.toggle_view_tools(20);
    }

    assert_eq!(app.tool_display, ToolDisplayMode::Off);
    let text = view_text(&app);
    assert!(text.contains("please check"));
    assert!(text.contains("all good"));
    assert!(!text.contains("cargo test"));
    assert!(!text.contains("shell command"));
    if let AppMode::View(state) = app.app_mode() {
        assert_eq!(state.message_ranges.len(), 2);
        let focused = state.focused_message.unwrap();
        assert_eq!(state.message_ranges[focused].entry_index, 0);
    }

    app.toggle_view_tools(20);
    assert_eq!(app.tool_display, ToolDisplayMode::Summary);
    assert!(view_text(&app).contains("Ran 1 shell command"));
}

#[test]
fn view_click_uses_cached_entries_after_file_removed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tool.jsonl");
    write_tool_conversation(&path);
    let mut app = app_with_tool_conversation(path.clone());
    std::fs::remove_file(&path).unwrap();
    assert_cached_tool_output_survives_file_removal(&mut app, true);
}

#[test]
fn single_file_view_click_uses_cached_entries_after_file_removed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tool.jsonl");
    write_tool_conversation(&path);
    let mut app = App::new_single_file(
        path.clone(),
        ToolDisplayMode::Truncated,
        false,
        KeyBindings::default(),
    );
    app.check_view_resize(80, 17);
    std::fs::remove_file(&path).unwrap();
    assert_cached_tool_output_survives_file_removal(&mut app, false);
}

#[test]
fn view_hover_tracks_clickable_output() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tool.jsonl");
    write_tool_conversation(&path);
    let mut app = app_with_tool_conversation(path);
    let frame = Rect::new(0, 0, 120, 20);
    let (row, id) = if let AppMode::View(state) = app.app_mode() {
        let layout = ui::view_layout_rects(frame, &app, state);
        let idx = state
            .rendered_lines
            .iter()
            .position(|line| line.clickable)
            .unwrap();
        let id = state.rendered_lines[idx].tool_output_id.clone().unwrap();
        (layout.content.y + (idx - state.scroll_offset) as u16, id)
    } else {
        unreachable!()
    };

    assert!(app.handle_view_mouse_move(row, frame));
    if let AppMode::View(state) = app.app_mode() {
        assert_eq!(state.hovered_tool_output, Some(id));
    }
    assert!(app.handle_view_mouse_move(0, frame));
    if let AppMode::View(state) = app.app_mode() {
        assert_eq!(state.hovered_tool_output, None);
    }
}

#[test]
fn cancel_rename_keeps_existing_title() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("abc123.jsonl");
    write_conversation(&path, Some("old"));
    let mut app = app_with_conversation(path, Some("old".to_string()));

    app.start_rename();
    assert!(matches!(app.dialog_mode, DialogMode::Rename { .. }));
    app.handle_rename_key(KeyCode::Esc, KeyModifiers::empty());

    assert_eq!(app.conversations[0].custom_title, Some("old".to_string()));
    assert_eq!(app.dialog_mode, DialogMode::None);
}

#[test]
fn configured_rename_key_starts_rename() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("abc123.jsonl");
    write_conversation(&path, None);
    let keys = KeyBindings {
        rename: KeyBinding {
            code: KeyCode::Char('t'),
            modifiers: KeyModifiers::CONTROL,
        },
        ..Default::default()
    };
    let mut app = App::new(
        vec![test_conversation(path, None)],
        ToolDisplayMode::Summary,
        false,
        keys,
        vec![],
    );

    app.handle_key(KeyCode::Char('t'), KeyModifiers::CONTROL, 10);

    assert!(matches!(app.dialog_mode, DialogMode::Rename { .. }));
}

#[test]
fn bare_r_remains_search_input() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("abc123.jsonl");
    write_conversation(&path, None);
    let mut app = app_with_conversation(path, None);

    app.handle_key(KeyCode::Char('r'), KeyModifiers::empty(), 10);

    assert_eq!(app.query(), "r");
    assert_eq!(app.dialog_mode, DialogMode::None);
}

#[test]
fn delete_key_asks_for_confirmation_by_default() {
    let mut app = app_with_conversation(PathBuf::from("/tmp/abc123.jsonl"), None);

    app.handle_key(KeyCode::Char('x'), KeyModifiers::CONTROL, 10);

    assert_eq!(app.dialog_mode, DialogMode::ConfirmDelete);
}

#[test]
fn disabled_delete_key_explains_instead_of_confirming() {
    let mut app = app_with_conversation(PathBuf::from("/tmp/abc123.jsonl"), None);
    app.delete_enabled = false;

    let action = app.handle_key(KeyCode::Char('x'), KeyModifiers::CONTROL, 10);

    assert!(action.is_none());
    assert_eq!(app.dialog_mode, DialogMode::None);
    // The refused key must not fall through to the search input either.
    assert_eq!(app.query(), "");
    assert!(
        app.status_message()
            .is_some_and(|(message, _)| message.contains("disable_delete"))
    );
}

#[test]
fn disabled_delete_key_is_refused_in_viewer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("abc123.jsonl");
    write_conversation(&path, None);
    let mut app = app_with_conversation(path.clone(), None);
    app.delete_enabled = false;
    app.selected = Some(0);
    app.enter_view_mode(80);

    app.handle_key(KeyCode::Char('x'), KeyModifiers::CONTROL, 10);

    assert_eq!(app.dialog_mode, DialogMode::None);
    assert!(app.status_message().is_some());
    assert!(path.exists());
}

#[test]
fn submit_rename_reparses_and_updates_search_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("abc123.jsonl");
    write_conversation(&path, Some("old"));
    let mut app = app_with_conversation(path.clone(), Some("old".to_string()));

    app.start_rename();
    app.handle_rename_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
    app.handle_rename_key(KeyCode::Char('n'), KeyModifiers::empty());
    app.handle_rename_key(KeyCode::Char('e'), KeyModifiers::empty());
    app.handle_rename_key(KeyCode::Char('w'), KeyModifiers::empty());
    app.handle_rename_key(KeyCode::Enter, KeyModifiers::empty());

    assert_eq!(app.conversations[0].custom_title, Some("new".to_string()));
    assert!(
        search::search(
            &app.conversations,
            &search::precompute_search_text(&app.conversations),
            "new",
            Local::now()
        )
        .contains(&0)
    );
    assert!(
        search::search(
            &app.conversations,
            &search::precompute_search_text(&app.conversations),
            "old",
            Local::now()
        )
        .is_empty()
    );
}

#[test]
fn submit_rename_preserves_selected_path() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.jsonl");
    let second = dir.path().join("second.jsonl");
    write_conversation(&first, None);
    write_conversation(&second, None);
    let mut app = App::new(
        vec![
            test_conversation(first, None),
            test_conversation(second.clone(), None),
        ],
        ToolDisplayMode::Summary,
        false,
        KeyBindings::default(),
        vec![],
    );
    app.selected = Some(1);

    app.start_rename();
    app.handle_rename_key(KeyCode::Char('n'), KeyModifiers::empty());
    app.handle_rename_key(KeyCode::Enter, KeyModifiers::empty());

    assert_eq!(app.get_selected_path().as_deref(), Some(second.as_path()));
}

#[test]
fn submit_empty_rename_clears_searchable_title() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("abc123.jsonl");
    write_conversation(&path, Some("old"));
    let mut app = app_with_conversation(path.clone(), Some("old".to_string()));

    app.start_rename();
    app.handle_rename_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
    app.handle_rename_key(KeyCode::Enter, KeyModifiers::empty());

    assert_eq!(app.conversations[0].custom_title, None);
    assert!(
        search::search(
            &app.conversations,
            &search::precompute_search_text(&app.conversations),
            "old",
            Local::now()
        )
        .is_empty()
    );
}

#[test]
fn refresh_key_requests_a_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("abc123.jsonl");
    let mut app = app_with_conversation(path, None);

    let action = app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL, 10);

    assert!(matches!(action, Some(Action::Refresh)));
}

#[test]
fn replacing_conversations_preserves_selection_by_path() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.jsonl");
    let second = dir.path().join("second.jsonl");
    let mut app = App::new(
        vec![
            test_conversation(first.clone(), None),
            test_conversation(second.clone(), None),
        ],
        ToolDisplayMode::Summary,
        false,
        KeyBindings::default(),
        vec![],
    );
    app.selected = Some(1);

    let mut refreshed_second = test_conversation(second.clone(), Some("renamed".to_string()));
    refreshed_second.timestamp = Local.with_ymd_and_hms(2024, 1, 2, 0, 0, 0).unwrap();
    app.show_conversations(vec![test_conversation(first, None), refreshed_second]);

    assert_eq!(app.get_selected_path().as_deref(), Some(second.as_path()));
    let selected = app.get_selected_conversation_index().unwrap();
    assert_eq!(
        app.conversations[selected].custom_title.as_deref(),
        Some("renamed")
    );
}

#[test]
fn replacing_conversations_removes_deleted_sessions_and_deduplicates_paths() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old.jsonl");
    let new = dir.path().join("new.jsonl");
    let mut app = app_with_conversation(old, None);

    app.show_conversations(vec![
        test_conversation(new.clone(), None),
        test_conversation(new.clone(), None),
    ]);

    assert_eq!(app.conversations.len(), 1);
    assert_eq!(app.get_selected_path().as_deref(), Some(new.as_path()));
}

#[test]
fn orphaned_conversations_refuse_every_action() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("abc123.jsonl");
    write_conversation(&path, None);
    let mut conversation = test_conversation(path, None);
    conversation.source = crate::history::model::SourceKind::Codex;
    conversation.provenance.orphaned = true;
    let mut app = App::new(
        vec![conversation],
        ToolDisplayMode::Summary,
        false,
        KeyBindings::default(),
        vec![],
    );

    for (code, modifiers) in [
        (KeyCode::Char('r'), KeyModifiers::CONTROL),
        (KeyCode::Char('f'), KeyModifiers::CONTROL),
        (KeyCode::Char('x'), KeyModifiers::CONTROL),
        (KeyCode::F(2), KeyModifiers::NONE),
    ] {
        assert!(app.handle_key(code, modifiers, 10).is_none());
        assert_eq!(app.dialog_mode, DialogMode::None);
        let (message, _) = app.status_message.take().unwrap();
        assert!(message.contains("unavailable"), "{message}");
    }
}

#[test]
fn codex_conversations_resume_but_do_not_rename() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("abc123.jsonl");
    write_conversation(&path, None);
    let mut conversation = test_conversation(path.clone(), None);
    conversation.source = crate::history::model::SourceKind::Codex;
    let mut app = App::new(
        vec![conversation],
        ToolDisplayMode::Summary,
        false,
        KeyBindings::default(),
        vec![],
    );

    app.handle_key(KeyCode::F(2), KeyModifiers::NONE, 10);
    assert_eq!(app.dialog_mode, DialogMode::None);
    let (message, _) = app.status_message.take().unwrap();
    assert_eq!(message, "Renaming CX sessions is not supported");

    app.handle_key(KeyCode::Char('x'), KeyModifiers::CONTROL, 10);
    assert_eq!(app.dialog_mode, DialogMode::ConfirmDelete);
    app.dialog_mode = DialogMode::None;
    assert!(matches!(
        app.handle_key(KeyCode::Char('r'), KeyModifiers::CONTROL, 10),
        Some(Action::Resume(resumed)) if resumed == path
    ));
}

#[test]
fn the_viewer_opens_subagents_and_forks_and_returns_to_their_session() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("-work");
    let sidecars = project.join("s1").join("subagents");
    std::fs::create_dir_all(&sidecars).unwrap();
    let parent_path = project.join("s1.jsonl");
    let subagent_path = sidecars.join("agent-a1.jsonl");
    let fork_path = sidecars.join("agent-afork.jsonl");
    write_named_conversation(&parent_path, "parent body");
    write_named_conversation(&subagent_path, "subagent body");
    write_named_conversation(&fork_path, "fork body");

    let parent = test_conversation(parent_path.clone(), None);
    let mut subagent = test_conversation(subagent_path.clone(), None);
    subagent.parent = Some("s1".into());
    subagent.provenance.child = true;
    let mut fork = test_conversation(fork_path.clone(), None);
    fork.parent = Some("s1".into());
    fork.provenance.branch = true;
    fork.timestamp = Local.with_ymd_and_hms(2024, 1, 2, 0, 0, 0).unwrap();
    let mut app = App::new(
        vec![parent, subagent, fork],
        ToolDisplayMode::Summary,
        false,
        KeyBindings::default(),
        vec![],
    );
    let viewed = |app: &App| app.viewed_conversation().map(|c| c.path.clone());

    app.selected = app.filtered.iter().position(|&row| row == 0);
    app.enter_view_mode(80);
    assert_eq!(app.view_family().parent, None);
    assert_eq!(app.view_family().children, vec![1, 2]);

    app.handle_key(KeyCode::Char('s'), KeyModifiers::NONE, 10);
    assert_eq!(app.dialog_mode, DialogMode::Children { selected: 0 });
    app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE, 10);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE, 10);
    assert_eq!(app.dialog_mode, DialogMode::None);
    assert_eq!(viewed(&app), Some(fork_path.clone()));
    assert_eq!(app.view_family().parent, Some(0));

    // Actions apply to the conversation in the viewer.
    assert!(matches!(
        app.handle_key(KeyCode::Char('r'), KeyModifiers::CONTROL, 10),
        Some(Action::Resume(resumed)) if resumed == fork_path
    ));
    app.handle_key(KeyCode::Char('f'), KeyModifiers::CONTROL, 10);
    let (message, _) = app.status_message.take().unwrap();
    assert!(message.contains("not supported"), "{message}");

    app.handle_key(KeyCode::Char('P'), KeyModifiers::NONE, 10);
    assert_eq!(viewed(&app), Some(parent_path.clone()));
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE, 10);
    assert_eq!(viewed(&app), Some(fork_path));
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE, 10);
    assert_eq!(viewed(&app), Some(parent_path.clone()));
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE, 10);
    assert!(matches!(app.app_mode, AppMode::List));

    // A fork outlives its session but has nothing left to resume.
    std::fs::remove_file(&parent_path).unwrap();
    app.selected = app.filtered.iter().position(|&row| row == 2);
    assert!(
        app.handle_key(KeyCode::Char('r'), KeyModifiers::CONTROL, 10)
            .is_none()
    );
    let (message, _) = app.status_message.take().unwrap();
    assert_eq!(message, "The session this came from no longer exists");
}

#[test]
fn a_session_without_relatives_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("abc123.jsonl");
    write_conversation(&path, None);
    let mut app = app_with_conversation(path, None);
    app.selected = Some(0);
    app.enter_view_mode(80);

    app.handle_key(KeyCode::Char('s'), KeyModifiers::NONE, 10);
    assert_eq!(app.dialog_mode, DialogMode::None);
    let (message, _) = app.status_message.take().unwrap();
    assert_eq!(message, "No subagents or forks in this session");
    app.handle_key(KeyCode::Char('P'), KeyModifiers::NONE, 10);
    let (message, _) = app.status_message.take().unwrap();
    assert_eq!(message, "This session has no parent");
}

fn write_dialogue(path: &std::path::Path, turns: &[(&str, &str)]) {
    let lines: Vec<String> = turns
        .iter()
        .map(|(role, text)| {
            serde_json::json!({
                "type": role,
                "timestamp": "2024-01-01T00:00:00Z",
                "message": {"role": role, "content": text}
            })
            .to_string()
        })
        .collect();
    std::fs::write(path, lines.join("\n") + "\n").unwrap();
}

fn app_searching(path: PathBuf, query: &str) -> App {
    let mut app = app_with_conversation(path, None);
    app.query = query.to_string();
    app.selected = Some(0);
    app
}

#[test]
fn list_preview_sits_beside_wide_lists_and_below_tall_ones() {
    let app = app_searching(PathBuf::from("/tmp/none.jsonl"), "hello");

    let wide = ui::list_layout_rects(Rect::new(0, 0, 160, 30), &app);
    let preview = wide.preview.unwrap();
    assert_eq!(preview.y, wide.list.y);
    assert_eq!(preview.x, wide.list.right());

    let tall = ui::list_layout_rects(Rect::new(0, 0, 100, 40), &app);
    let preview = tall.preview.unwrap();
    assert_eq!(preview.x, tall.list.x);
    // The preview's top border replaces the list's last row separator.
    assert_eq!(preview.y, tall.list.bottom() - 1);
    assert_eq!(tall.list.height % 3, 0);

    assert!(
        ui::list_layout_rects(Rect::new(0, 0, 100, 20), &app)
            .preview
            .is_none()
    );
}

#[test]
fn list_preview_needs_a_query() {
    let app = app_searching(PathBuf::from("/tmp/none.jsonl"), " ");

    assert!(
        ui::list_layout_rects(Rect::new(0, 0, 160, 30), &app)
            .preview
            .is_none()
    );
}

#[test]
fn list_preview_anchors_on_the_first_match() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dialogue.jsonl");
    write_dialogue(
        &path,
        &[
            ("user", "set up the tunnel"),
            ("assistant", "the tunnel is up"),
            ("user", "now rotate the token"),
            ("assistant", "rotated the token and restarted"),
        ],
    );
    let mut app = app_searching(path, "token");

    app.fill_list_preview(60, std::time::Duration::from_secs(30));

    let Some(PreviewView::Ready { doc, anchor }) = app.list_preview() else {
        panic!("preview not ready");
    };
    let anchor = anchor.unwrap();
    assert!(
        doc.texts[anchor].contains("now rotate the token"),
        "{}",
        doc.texts[anchor]
    );
    assert!(doc.scroll_offset(Some(anchor), 20) <= anchor);
}

#[test]
fn list_preview_keeps_showing_the_conversation_while_the_query_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dialogue.jsonl");
    write_dialogue(
        &path,
        &[("user", "set up the tunnel"), ("user", "rotate the token")],
    );
    let mut app = app_searching(path, "tunnel");
    app.fill_list_preview(60, std::time::Duration::from_secs(30));

    app.query = "token".to_string();
    // Requested without waiting: the previous reply stays on screen.
    app.fill_list_preview(60, std::time::Duration::ZERO);
    assert!(matches!(
        app.list_preview(),
        Some(PreviewView::Ready { .. })
    ));

    app.fill_list_preview(60, std::time::Duration::from_secs(30));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let Some(PreviewView::Ready { doc, anchor }) = app.list_preview() else {
            panic!("preview not ready");
        };
        if anchor.is_some_and(|line| doc.texts[line].contains("token")) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "anchor never moved");
        std::thread::sleep(std::time::Duration::from_millis(5));
        app.fill_list_preview(60, std::time::Duration::ZERO);
    }
}

#[test]
fn list_preview_of_a_missing_file_says_why() {
    let mut app = app_searching(PathBuf::from("/nonexistent/gone.jsonl"), "hello");

    app.fill_list_preview(60, std::time::Duration::from_secs(30));

    assert!(matches!(app.list_preview(), Some(PreviewView::Failed(_))));
}

#[test]
fn list_click_in_preview_pane_selects_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("one.jsonl");
    write_named_conversation(&path, "hello body");
    let mut app = app_searching(path, "hello");
    let frame = Rect::new(0, 0, 160, 30);
    let layout = ui::list_layout_rects(frame, &app);
    let preview = layout.preview.unwrap();

    assert!(!app.handle_list_click(preview.x + 5, layout.list.y, frame));
    assert!(app.handle_list_click(layout.list.x + 5, layout.list.y, frame));
}
