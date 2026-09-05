use super::markdown::render_markdown_to_lines;
use super::tools::{ToolCallRenderSpec, ToolOutputKind, make_tool_output_id, render_tool_call};
use super::*;

/// Helper to render markdown and extract just the content text (without styling)
fn render_to_text(input: &str, width: usize) -> String {
    let lines = render_markdown_to_lines(input, width);
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|(text, _)| text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn test_plain_text() {
    let result = render_to_text("Hello world", 80);
    assert_eq!(result.trim(), "Hello world");
}

#[test]
fn test_heading() {
    let result = render_to_text("# Heading 1", 80);
    assert!(result.contains("Heading 1"));
}

#[test]
fn test_heading_with_paragraph() {
    let result = render_to_text("# Heading\n\nSome text", 80);
    let lines: Vec<&str> = result.lines().collect();
    // Should have: heading, blank, text
    assert_eq!(lines.len(), 3, "Expected 3 lines, got:\n{}", result);
    assert!(lines[0].contains("Heading"));
    assert_eq!(lines[1], "");
    assert_eq!(lines[2], "Some text");
}

#[test]
fn test_paragraph_with_list() {
    let result = render_to_text("Some intro:\n\n- Item 1\n- Item 2", 80);
    let lines: Vec<&str> = result.lines().collect();
    // Should have: para, blank, item1, item2
    assert_eq!(lines.len(), 4, "Expected 4 lines, got:\n{}", result);
    assert_eq!(lines[0], "Some intro:");
    assert_eq!(lines[1], "");
    assert!(lines[2].contains("- Item 1"));
    assert!(lines[3].contains("- Item 2"));
}

#[test]
fn test_numbered_list_with_bold() {
    // This is the bug case: numbered list item starting with bold text
    let result = render_to_text("1. **Task 10:** description\n2. **Task 11:** more", 80);
    let lines: Vec<&str> = result.lines().collect();
    // Should have: item1, item2 (NO blank lines between number and content)
    assert_eq!(lines.len(), 2, "Expected 2 lines, got:\n{}", result);
    assert!(
        lines[0].starts_with("1. "),
        "Line should start with '1. ': {:?}",
        lines[0]
    );
    assert!(
        lines[0].contains("Task 10"),
        "Line should contain 'Task 10': {:?}",
        lines[0]
    );
    assert!(
        lines[1].starts_with("2. "),
        "Line should start with '2. ': {:?}",
        lines[1]
    );
    assert!(
        lines[1].contains("Task 11"),
        "Line should contain 'Task 11': {:?}",
        lines[1]
    );
}

#[test]
fn test_numbered_list_no_extra_blank_lines() {
    let input = "## Changes\n\n1. **First change:**\n   - details\n2. **Second change:**\n   - more details";
    let result = render_to_text(input, 80);
    let lines: Vec<&str> = result.lines().collect();

    // Verify no blank lines between "1." and "First change"
    let line1_idx = lines
        .iter()
        .position(|l| l.starts_with("1. "))
        .expect("Should find '1. '");
    assert!(
        lines[line1_idx].contains("First change"),
        "First item should be on same line as '1. '"
    );

    // Verify no blank lines between "2." and "Second change"
    let line2_idx = lines
        .iter()
        .position(|l| l.starts_with("2. "))
        .expect("Should find '2. '");
    assert!(
        lines[line2_idx].contains("Second change"),
        "Second item should be on same line as '2. '"
    );
}

#[test]
fn test_consecutive_list_items_no_blanks() {
    let result = render_to_text("- First\n- Second\n- Third", 80);
    let lines: Vec<&str> = result.lines().collect();
    // Should be exactly 3 lines, no blanks between items
    assert_eq!(
        lines.len(),
        3,
        "Expected 3 lines with no blanks, got:\n{}",
        result
    );
    assert!(lines[0].contains("- First"));
    assert!(lines[1].contains("- Second"));
    assert!(lines[2].contains("- Third"));
}

#[test]
fn test_nested_list() {
    let result = render_to_text("- Item 1\n  - Nested 1\n  - Nested 2\n- Item 2", 80);
    let lines: Vec<&str> = result.lines().collect();
    // Should have: item1, nested1, nested2, item2 (no extra blanks)
    assert_eq!(lines.len(), 4, "Expected 4 lines, got:\n{}", result);
    assert!(lines[0].contains("- Item 1"));
    assert!(lines[1].contains("- Nested 1"));
    assert!(lines[2].contains("- Nested 2"));
    assert!(lines[3].contains("- Item 2"));
}

#[test]
fn test_code_block() {
    let result = render_to_text("Text\n\n```rust\nlet x = 1;\n```\n\nMore text", 80);
    let lines: Vec<&str> = result.lines().collect();
    // TUI strips fence markers (signaled via color instead).
    assert!(!result.contains("```"));
    assert!(result.contains("let x = 1;"));

    // Check for proper spacing
    let text_idx = lines.iter().position(|l| l == &"Text").unwrap();
    let more_idx = lines.iter().position(|l| l == &"More text").unwrap();
    assert_eq!(lines[text_idx + 1], "", "Should have blank line after Text");
    assert_eq!(
        lines[more_idx - 1],
        "",
        "Should have blank line before More text"
    );
}

#[test]
fn test_block_quote() {
    let result = render_to_text("Text\n\n> Quote here", 80);
    let lines: Vec<&str> = result.lines().collect();
    // Block quote renders with quote prefix on one line, blank, then content
    // This is due to how the markdown parser handles block quotes
    assert_eq!(lines[0], "Text");
    assert_eq!(lines[1], ""); // blank before quote
    assert!(lines[2].starts_with("> "), "Should have quote prefix");
    // Content may be on same line or next line depending on parser
    let has_content =
        lines[2].contains("Quote here") || (lines.len() > 4 && lines[4].contains("Quote here"));
    assert!(has_content, "Should contain quote content");
}

#[test]
fn test_horizontal_rule() {
    let result = render_to_text("Before\n\n---\n\nAfter", 80);
    let lines: Vec<&str> = result.lines().collect();
    // Should have proper spacing around rule
    let before_idx = lines.iter().position(|l| l == &"Before").unwrap();
    let after_idx = lines.iter().position(|l| l == &"After").unwrap();
    // Rule should be on its own with blanks around it
    assert_eq!(
        lines[before_idx + 1],
        "",
        "Should have blank line after Before"
    );
    assert!(lines[before_idx + 2].contains("─"), "Should have rule");
    assert_eq!(
        lines[after_idx - 1],
        "",
        "Should have blank line before After"
    );
}

#[test]
fn test_multiple_paragraphs() {
    let result = render_to_text(
        "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.",
        80,
    );
    let lines: Vec<&str> = result.lines().collect();
    // Should have: p1, blank, p2, blank, p3
    assert_eq!(lines.len(), 5, "Expected 5 lines, got:\n{}", result);
    assert_eq!(lines[0], "First paragraph.");
    assert_eq!(lines[1], "");
    assert_eq!(lines[2], "Second paragraph.");
    assert_eq!(lines[3], "");
    assert_eq!(lines[4], "Third paragraph.");
}

#[test]
fn test_list_with_multiline_items() {
    let input = "1. First item\n   with continuation\n2. Second item\n   also continued";
    let result = render_to_text(input, 80);
    let lines: Vec<&str> = result.lines().collect();

    // First item should start with "1. "
    assert!(lines[0].starts_with("1. "), "First line: {:?}", lines[0]);
    // Soft breaks join continuation to the same paragraph, so all first-item
    // text may appear on a single line at wide widths
    let first_item_text = lines.join(" ");
    assert!(
        first_item_text.contains("First item"),
        "Should contain first item text"
    );
    assert!(
        first_item_text.contains("with continuation"),
        "Should contain continuation"
    );

    // Second item should start with "2. "
    let line2_idx = lines
        .iter()
        .position(|l| l.starts_with("2. "))
        .expect("Should find '2. '");
    assert!(line2_idx >= 1, "Second item should appear after first");
}

#[test]
fn test_no_trailing_blank_lines() {
    let result = render_to_text("Text\n\n## Heading\n\nParagraph", 80);
    // Should not end with blank lines
    assert!(
        !result.ends_with("\n\n"),
        "Should not have trailing blank lines: {:?}",
        result
    );
}

#[test]
fn test_inline_code() {
    let result = render_to_text("Use `code` here", 80);
    assert!(result.contains("code"));
}

#[test]
fn test_bold_and_italic() {
    let result = render_to_text("**bold** and *italic* text", 80);
    // Just verify it renders without panicking and contains the text
    assert!(result.contains("bold"));
    assert!(result.contains("italic"));
}

#[test]
fn test_table_basic() {
    let input = "| A | B |\n|---|---|\n| 1 | 2 |";
    let result = render_to_text(input, 80);
    eprintln!("Table output:\n{}", result);
    assert!(result.contains('┌'), "Expected top-left corner");
    assert!(result.contains('│'), "Expected vertical border");
    assert!(result.contains('└'), "Expected bottom-left corner");
    assert!(result.contains(" A "), "Expected cell A");
    assert!(result.contains(" B "), "Expected cell B");
    assert!(result.contains(" 1 "), "Expected cell 1");
    assert!(result.contains(" 2 "), "Expected cell 2");
}

#[test]
fn test_table_column_widths() {
    let input = "| Short | Longer text |\n|---|---|\n| A | B |";
    let result = render_to_text(input, 80);
    eprintln!("Table output:\n{}", result);
    assert!(result.contains("Short"), "Expected Short");
    assert!(result.contains("Longer text"), "Expected Longer text");
    // Columns should be sized to fit longest content
    let lines: Vec<&str> = result.lines().collect();
    // All border lines should be same width
    let border_widths: Vec<usize> = lines
        .iter()
        .filter(|l| l.starts_with('┌') || l.starts_with('├') || l.starts_with('└'))
        .map(|l| l.chars().count())
        .collect();
    assert!(
        border_widths.windows(2).all(|w| w[0] == w[1]),
        "Border lines should be same width: {:?}",
        border_widths
    );
}

#[test]
fn test_table_multiple_rows() {
    let input = "| H1 | H2 | H3 |\n|----|----|----|\n| A | B | C |\n| D | E | F |";
    let result = render_to_text(input, 80);
    eprintln!("Table output:\n{}", result);
    assert!(result.contains('├'), "Expected row separators");
    assert!(result.contains('┼'), "Expected cross junctions");
}

fn line_text(line: &RenderedLine) -> String {
    line.spans.iter().map(|(text, _)| text.as_str()).collect()
}

fn rendered_text(conversation: &RenderedConversation) -> String {
    conversation
        .lines
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn test_render_options(tool_display: ToolDisplayMode) -> RenderOptions {
    RenderOptions {
        tool_display,
        show_thinking: false,
        show_timing: false,
        content_width: 80,
        expanded_tool_outputs: BTreeSet::new(),
        show_annotations: false,
        annotations: crate::annotations::ConversationAnnotations::default(),
        annotator_labels: std::collections::HashMap::new(),
        focused_annotation: None,
    }
}

fn annotation(
    id: &str,
    kind: &str,
    text: &str,
    targets: Vec<usize>,
) -> crate::annotations::Annotation {
    crate::annotations::Annotation {
        id: id.to_string(),
        targets: targets
            .into_iter()
            .map(crate::annotations::TargetSpan::single)
            .collect(),
        kind: kind.to_string(),
        text: text.to_string(),
        annotator: String::new(),
        origin: None,
    }
}

fn annotated_options(
    annotations: Vec<crate::annotations::Annotation>,
    show: bool,
) -> RenderOptions {
    let mut options = test_render_options(ToolDisplayMode::Hidden);
    options.show_annotations = show;
    options.annotations = crate::annotations::ConversationAnnotations::from_flat(annotations);
    options
}

/// Index of the first rendered line containing `needle`, for asserting the
/// order annotations and entries print in.
fn position_of(text: &str, needle: &str) -> Option<usize> {
    text.lines().position(|line| line.contains(needle))
}

#[test]
fn each_annotation_reports_the_lines_it_occupies() {
    let entries = vec![
        user_entry(0, "first message", None),
        assistant_text_entry(1, "second message", None),
    ];
    let options = annotated_options(
        vec![
            annotation("s", "note", "session note", vec![]),
            annotation("a", "recap", "about the second turn", vec![2]),
        ],
        true,
    );

    let rendered = render_parsed_conversation(&entries, &options);

    let ids = rendered
        .annotations
        .iter()
        .map(|range| range.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["s", "a"]);
    for range in &rendered.annotations {
        assert!(range.end_line > range.start_line);
    }
    // Message ranges stay free of annotation blocks: the scroll anchor binary
    // searches them by entry_index, which an entry-less block would corrupt.
    assert_eq!(rendered.messages.len(), 2);
}

#[test]
fn session_annotations_print_ahead_of_the_conversation() {
    // entry_index 0 and 1 carry jsonl_line 1 and 2.
    let entries = vec![
        user_entry(0, "first message", None),
        assistant_text_entry(1, "second message", None),
    ];
    let options = annotated_options(
        vec![annotation("s", "note", "whole session note", vec![])],
        true,
    );

    let rendered = render_parsed_conversation(&entries, &options);
    let text = rendered_text(&rendered);

    let note = position_of(&text, "whole session note").expect("annotation rendered");
    let first = position_of(&text, "first message").expect("conversation rendered");
    assert!(
        note < first,
        "session annotation should precede the conversation"
    );
}

#[test]
fn a_positioned_annotation_prints_after_the_entry_at_its_line() {
    let entries = vec![
        user_entry(0, "first message", None),
        assistant_text_entry(1, "second message", None),
    ];
    // jsonl_line 2 belongs to the second entry.
    let options = annotated_options(
        vec![annotation("a", "recap", "about the second turn", vec![2])],
        true,
    );

    let rendered = render_parsed_conversation(&entries, &options);
    let text = rendered_text(&rendered);

    let first = position_of(&text, "first message").unwrap();
    let second = position_of(&text, "second message").unwrap();
    let note = position_of(&text, "about the second turn").unwrap();
    // The note follows the entry it names.
    assert!(first < second && second < note, "{text}");
}

#[test]
fn an_annotation_past_the_last_entry_prints_at_the_end() {
    let entries = vec![user_entry(0, "only message", None)];
    let options = annotated_options(
        vec![annotation(
            "a",
            "recap",
            "names a line that has no entry",
            vec![99],
        )],
        true,
    );

    let rendered = render_parsed_conversation(&entries, &options);
    let text = rendered_text(&rendered);

    let only = position_of(&text, "only message").unwrap();
    let note = position_of(&text, "names a line that has no entry")
        .expect("an annotation past the last entry is kept, not dropped");
    assert!(note > only);
}

#[test]
fn the_trailer_names_the_origin_when_the_note_carries_one() {
    let entries = vec![user_entry(0, "only message", None)];
    let mut note = annotation("a", "recap", "agent work", vec![1]);
    note.origin = Some(crate::annotations::AnnotationOrigin {
        path: std::path::PathBuf::from("/tmp/subagents/agent-ac1cffaa.jsonl"),
        lines: "412..430".to_string(),
    });
    let options = annotated_options(vec![note], true);

    let rendered = render_parsed_conversation(&entries, &options);
    let text = rendered_text(&rendered);

    let labelled = text
        .lines()
        .find(|line| line.contains("agent work"))
        .expect("annotation line rendered");
    assert!(
        labelled.contains("@1 · recap · agent-ac1cffaa @412..430"),
        "{labelled}"
    );
}

#[test]
fn the_label_names_the_kind_and_the_lines_targeted() {
    let entries = vec![user_entry(0, "only message", None)];
    let options = annotated_options(vec![annotation("a", "recap", "body", vec![1])], true);

    let rendered = render_parsed_conversation(&entries, &options);
    let text = rendered_text(&rendered);

    // The kind sits in the name column and the target follows the body, so the
    // two are no longer adjacent in the rendered line.
    let labelled = text
        .lines()
        .find(|line| line.contains("recap") && line.contains("body"))
        .expect("annotation line rendered");
    assert!(labelled.contains("@1"), "target named on the label row");
}

#[test]
fn annotations_are_absent_when_the_toggle_is_off() {
    let entries = vec![user_entry(0, "only message", None)];
    let options = annotated_options(
        vec![
            annotation("a", "recap", "positioned note", vec![1]),
            annotation("s", "note", "session note", vec![]),
        ],
        false,
    );

    let rendered = render_parsed_conversation(&entries, &options);
    let text = rendered_text(&rendered);

    assert!(position_of(&text, "positioned note").is_none());
    assert!(position_of(&text, "session note").is_none());
    assert!(position_of(&text, "only message").is_some());
}

fn tool_summary_entries() -> Vec<RenderableEntry> {
    vec![
        RenderableEntry {
            entry_index: 0,
            jsonl_line: 1,
            entry: serde_json::from_str(
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Grep","input":{"pattern":"one"}}]}}"#,
            )
            .unwrap(),
        },
        RenderableEntry {
            entry_index: 1,
            jsonl_line: 2,
            entry: serde_json::from_str(
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"grep result"}]}}"#,
            )
            .unwrap(),
        },
        RenderableEntry {
            entry_index: 2,
            jsonl_line: 3,
            entry: serde_json::from_str(
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_2","name":"Read","input":{"file_path":"src/main.rs"}},{"type":"tool_use","id":"toolu_3","name":"Bash","input":{"command":"cargo test"}}]}}"#,
            )
            .unwrap(),
        },
        RenderableEntry {
            entry_index: 3,
            jsonl_line: 4,
            entry: serde_json::from_str(
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_3","content":"bash result"}]}}"#,
            )
            .unwrap(),
        },
    ]
}

#[test]
fn hidden_tool_mode_renders_activity_summary() {
    let entry = RenderableEntry {
        entry_index: 0,
        jsonl_line: 1,
        entry: serde_json::from_str(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Grep","input":{"pattern":"one"}},{"type":"tool_use","id":"toolu_2","name":"Grep","input":{"pattern":"two"}},{"type":"tool_use","id":"toolu_3","name":"Read","input":{"file_path":"src/main.rs"}}]}}"#,
        )
        .unwrap(),
    };
    let rendered =
        render_parsed_conversation(&[entry], &test_render_options(ToolDisplayMode::Hidden));
    let text = rendered_text(&rendered);

    assert!(text.contains("Searched for 2 patterns"));
    assert!(text.contains("read 1 file"));
    assert!(!text.contains("Grep:"));
    assert!(!text.contains("Read:"));
}

#[test]
fn hidden_tool_mode_coalesces_tool_only_entries_across_results() {
    let entries = tool_summary_entries();
    let rendered =
        render_parsed_conversation(&entries, &test_render_options(ToolDisplayMode::Hidden));
    let text = rendered_text(&rendered);

    assert!(text.contains("Searched for 1 pattern, read 1 file, ran 1 shell command"));
    assert_eq!(text.matches("Claude").count(), 1);
    assert!(!text.contains("Result"));
}

#[test]
fn tool_summary_uses_source_agent_label() {
    let entries = vec![
        RenderableEntry {
            entry_index: 0,
            jsonl_line: 1,
            entry: serde_json::from_str(
                r#"{"type":"assistant","agent":"Pi","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"bash","input":{"command":"pwd"}}]}}"#,
            )
            .unwrap(),
        },
        RenderableEntry {
            entry_index: 1,
            jsonl_line: 2,
            entry: serde_json::from_str(
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"result"}]}}"#,
            )
            .unwrap(),
        },
    ];
    let summary_id = make_tool_summary_output_id(0, None);

    let collapsed =
        render_parsed_conversation(&entries, &test_render_options(ToolDisplayMode::Hidden));
    let collapsed_text = rendered_text(&collapsed);
    assert!(collapsed_text.contains("Pi"));
    assert!(!collapsed_text.contains("Claude"));

    let mut options = test_render_options(ToolDisplayMode::Hidden);
    options.expanded_tool_outputs.insert(summary_id);
    let expanded = render_parsed_conversation(&entries, &options);
    let expanded_text = rendered_text(&expanded);
    assert!(expanded_text.contains("Pi"));
    assert!(!expanded_text.contains("Claude"));
}

#[test]
fn hidden_pi_thinking_allows_clickable_grouped_tool_summary() {
    let entries = vec![
        RenderableEntry {
            entry_index: 0,
            jsonl_line: 1,
            entry: serde_json::from_str(
                r#"{"type":"assistant","agent":"Pi","message":{"role":"assistant","content":[{"type":"thinking","thinking":"inspect files","signature":""},{"type":"tool_use","id":"call_1","name":"bash","input":{"command":"pwd"}}]}}"#,
            )
            .unwrap(),
        },
        RenderableEntry {
            entry_index: 1,
            jsonl_line: 2,
            entry: serde_json::from_str(
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_1","content":"/tmp/project"}]}}"#,
            )
            .unwrap(),
        },
        RenderableEntry {
            entry_index: 2,
            jsonl_line: 3,
            entry: serde_json::from_str(
                r#"{"type":"assistant","agent":"Pi","message":{"role":"assistant","content":[{"type":"thinking","thinking":"inspect status","signature":""},{"type":"tool_use","id":"call_2","name":"bash","input":{"command":"git status"}}]}}"#,
            )
            .unwrap(),
        },
        RenderableEntry {
            entry_index: 3,
            jsonl_line: 4,
            entry: serde_json::from_str(
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_2","content":"clean"}]}}"#,
            )
            .unwrap(),
        },
    ];
    let summary_id = make_tool_summary_output_id(0, None);

    let collapsed =
        render_parsed_conversation(&entries, &test_render_options(ToolDisplayMode::Hidden));
    let collapsed_text = rendered_text(&collapsed);
    assert_eq!(collapsed_text.matches("Pi").count(), 1);
    assert!(
        collapsed
            .lines
            .iter()
            .any(|line| { line.clickable && line.tool_output_id.as_ref() == Some(&summary_id) })
    );

    let mut options = test_render_options(ToolDisplayMode::Hidden);
    options.expanded_tool_outputs.insert(summary_id);
    let expanded = render_parsed_conversation(&entries, &options);
    let expanded_text = rendered_text(&expanded);
    assert!(expanded_text.contains("pwd"));
    assert!(expanded_text.contains("git status"));
    assert!(expanded_text.contains("/tmp/project"));
    assert!(expanded_text.contains("clean"));
}

#[test]
fn expanded_tool_summary_renders_truncated_details() {
    let entries = tool_summary_entries();
    let mut options = test_render_options(ToolDisplayMode::Hidden);
    options
        .expanded_tool_outputs
        .insert(make_tool_summary_output_id(0, None));
    let rendered = render_parsed_conversation(&entries, &options);
    let text = rendered_text(&rendered);

    assert!(!text.contains("Searched for 1 pattern, read 1 file, ran 1 shell command"));
    assert!(text.contains("Grep: \"one\" in ."));
    assert!(text.contains("Read: src/main.rs"));
    assert!(text.contains("Bash: cargo test"));
    assert!(text.contains("↳ Result"));
    assert!(text.contains("bash result"));
    assert!(rendered.lines.iter().any(|line| {
        line.clickable
            && line.tool_output_id.as_ref() == Some(&make_tool_summary_output_id(0, None))
    }));
}

#[test]
fn subagent_summary_label_parity() {
    // Subagent tool-only assistant followed by its tool-result. The
    // collapsed summary uses the nested ↳ label; the expanded detail
    // rows must use the same nested label, not the literal "Claude".
    let entries = vec![
        RenderableEntry {
            entry_index: 0,
            jsonl_line: 1,
            entry: serde_json::from_str(
                r#"{"type":"assistant","parent_tool_use_id":"toolu_parent_abc","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Grep","input":{"pattern":"one"}}]}}"#,
            )
            .unwrap(),
        },
        RenderableEntry {
            entry_index: 1,
            jsonl_line: 2,
            entry: serde_json::from_str(
                r#"{"type":"user","parent_tool_use_id":"toolu_parent_abc","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"grep result"}]}}"#,
            )
            .unwrap(),
        },
    ];
    let expected_label = "↳parent_";
    let summary_id = make_tool_summary_output_id(0, Some("toolu_parent_abc"));

    // Collapsed: subagent label appears, no literal "Claude".
    let mut options = test_render_options(ToolDisplayMode::Hidden);
    options.show_thinking = true;
    let collapsed = render_parsed_conversation(&entries, &options);
    let collapsed_text = rendered_text(&collapsed);
    assert!(
        collapsed_text.contains(expected_label),
        "collapsed summary should use nested label: {}",
        collapsed_text
    );
    assert!(
        !collapsed_text.contains("Claude"),
        "collapsed subagent summary should not use literal Claude label: {}",
        collapsed_text
    );

    // Expanded: detail rows must use the same nested label, not "Claude".
    let mut options = test_render_options(ToolDisplayMode::Hidden);
    options.show_thinking = true;
    options.expanded_tool_outputs.insert(summary_id);
    let expanded = render_parsed_conversation(&entries, &options);
    let expanded_text = rendered_text(&expanded);
    assert!(
        expanded_text.contains("Grep: \"one\" in ."),
        "expanded detail row should render tool call: {}",
        expanded_text
    );
    assert!(
        expanded_text.contains(expected_label),
        "expanded detail row should use nested label: {}",
        expanded_text
    );
    assert!(
        !expanded_text.contains("Claude"),
        "expanded subagent detail rows must not use literal Claude label: {}",
        expanded_text
    );
}

#[test]
fn hidden_tool_mode_status_label_is_summary() {
    assert_eq!(ToolDisplayMode::Hidden.status_label(), "sum");
}

#[test]
fn tool_output_ids_use_stable_literal_format() {
    assert_eq!(
        make_tool_output_id(0, None, 0, ToolOutputKind::ToolCall, Some("toolu_1")).0,
        "entry:0:parent:top:block:0:kind:call:id:toolu_1"
    );
    assert_eq!(
        make_tool_output_id(
            1,
            Some("toolu_parent"),
            2,
            ToolOutputKind::ToolResult,
            Some("toolu_2"),
        )
        .0,
        "entry:1:parent:toolu_parent:block:2:kind:result:id:toolu_2"
    );
    assert_eq!(
        make_tool_summary_output_id(3, Some("toolu_parent")).0,
        "entry:3:parent:toolu_parent:kind:summary"
    );
}

#[test]
fn parse_conversation_file_preserves_entry_indices() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conversation.jsonl");
    std::fs::write(
        &path,
        concat!(
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"first"}}"#,
            "\n",
            "not json\n",
            r#"{"type":"file-history-snapshot","messageId":"m1","snapshot":{},"isSnapshotUpdate":false}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"second"}]}}"#,
            "\n",
        ),
    )
    .unwrap();

    let entries = parse_conversation_file(&path).unwrap();

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].entry_index, 0);
    assert_eq!(entries[1].entry_index, 2);
}

#[test]
fn show_thinking_controls_subagent_entries() {
    let entries = vec![
        RenderableEntry {
            entry_index: 0,
            jsonl_line: 1,
            entry: serde_json::from_str(
                r#"{"type":"assistant","parent_tool_use_id":"toolu_parent","message":{"role":"assistant","content":[{"type":"text","text":"subagent text"}]}}"#,
            )
            .unwrap(),
        },
        RenderableEntry {
            entry_index: 1,
            jsonl_line: 2,
            entry: serde_json::from_str(
                r#"{"type":"progress","data":{"type":"agent_progress","agentId":"agent-abcdef123456","message":{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"agent progress text"}]}}}}"#,
            )
            .unwrap(),
        },
    ];
    let hidden =
        render_parsed_conversation(&entries, &test_render_options(ToolDisplayMode::Hidden));
    assert!(!rendered_text(&hidden).contains("subagent text"));
    assert!(!rendered_text(&hidden).contains("agent progress text"));

    let mut options = test_render_options(ToolDisplayMode::Hidden);
    options.show_thinking = true;
    let shown = render_parsed_conversation(&entries, &options);
    let text = rendered_text(&shown);
    assert!(text.contains("subagent text"));
    assert!(text.contains("agent progress text"));
}

#[test]
fn tool_call_metadata_tracks_truncated_and_expanded_state() {
    let input = serde_json::json!({"command":"one\ntwo\nthree\nfour\nfive"});
    let output_id = make_tool_output_id(0, None, 0, ToolOutputKind::ToolCall, Some("toolu_1"));
    let mut lines = Vec::new();
    render_tool_call(
        &mut lines,
        &ToolCallRenderSpec {
            name: "Bash",
            input: &input,
            label: "Claude",
            label_color: th().accent_dim,
            dimmed: false,
            content_width: 80,
            timing: timing::TimingSlot::Disabled,
            tool_display: ToolDisplayMode::Truncated,
            tool_output_id: &output_id,
            expanded: false,
        },
    );
    assert!(
        lines
            .iter()
            .any(|line| line_text(line).contains("more lines"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.clickable && line.tool_output_id.as_ref() == Some(&output_id))
    );
    assert!(!lines.iter().any(|line| line_text(line).contains("five")));

    let mut expanded = Vec::new();
    render_tool_call(
        &mut expanded,
        &ToolCallRenderSpec {
            name: "Bash",
            input: &input,
            label: "Claude",
            label_color: th().accent_dim,
            dimmed: false,
            content_width: 80,
            timing: timing::TimingSlot::Disabled,
            tool_display: ToolDisplayMode::Truncated,
            tool_output_id: &output_id,
            expanded: true,
        },
    );
    assert!(
        !expanded
            .iter()
            .any(|line| line_text(line).contains("more lines"))
    );
    assert!(expanded.iter().any(|line| line_text(line).contains("five")));
}

#[test]
fn test_format_timestamp() {
    // UTC timestamp with Z suffix
    let ts = "2026-02-04T19:46:38.440Z";
    let result = format_timestamp(ts);
    assert!(result.is_some(), "Should parse UTC timestamp");
    let formatted = result.unwrap();
    // Should be HH:MM format (local time)
    assert_eq!(formatted.len(), 5, "Should be HH:MM format: {}", formatted);
    assert!(
        formatted.contains(':'),
        "Should contain colon: {}",
        formatted
    );

    // Timestamp with timezone offset
    let ts2 = "2026-02-04T14:46:38-05:00";
    let result2 = format_timestamp(ts2);
    assert!(result2.is_some(), "Should parse timestamp with offset");
}

// -----------------------------------------------------------------
// Render regression harness
//
// These tests pin the observable behavior of the rendering pipeline
// (text, span styles, clickability, tool output IDs, message ranges)
// so subsequent refactors of the viewer can detect drift.
// -----------------------------------------------------------------

fn line_style_at<'a>(line: &'a RenderedLine, text: &str) -> &'a LineStyle {
    &line
        .spans
        .iter()
        .find(|(t, _)| t == text)
        .unwrap_or_else(|| panic!("span {:?} not found in line {:?}", text, line_text(line)))
        .1
}

fn user_entry(entry_index: usize, text: &str, timestamp: Option<&str>) -> RenderableEntry {
    let ts_field = match timestamp {
        Some(t) => format!(r#","timestamp":"{}""#, t),
        None => String::new(),
    };
    let json = format!(
        r#"{{"type":"user","message":{{"role":"user","content":"{}"}}{}}}"#,
        text, ts_field
    );
    RenderableEntry {
        entry_index,
        jsonl_line: entry_index + 1,
        entry: serde_json::from_str(&json).unwrap(),
    }
}

fn assistant_text_entry(
    entry_index: usize,
    text: &str,
    timestamp: Option<&str>,
) -> RenderableEntry {
    let ts_field = match timestamp {
        Some(t) => format!(r#","timestamp":"{}""#, t),
        None => String::new(),
    };
    let json = format!(
        r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{}"}}]}}{}}}"#,
        text, ts_field
    );
    RenderableEntry {
        entry_index,
        jsonl_line: entry_index + 1,
        entry: serde_json::from_str(&json).unwrap(),
    }
}

#[test]
fn message_ranges_track_user_and_assistant_entries() {
    let entries = vec![
        user_entry(0, "Hello", None),
        assistant_text_entry(1, "Hi there", None),
    ];
    let rendered =
        render_parsed_conversation(&entries, &test_render_options(ToolDisplayMode::Hidden));

    // Two messages, each with one content line and one trailing blank.
    assert_eq!(rendered.messages.len(), 2);

    let user = &rendered.messages[0];
    assert_eq!(user.entry_index, 0);
    assert_eq!(user.start_line, 0);
    assert_eq!(user.end_line, 1, "user range excludes trailing blank");

    let assistant = &rendered.messages[1];
    assert_eq!(assistant.entry_index, 1);
    assert_eq!(assistant.start_line, 2);
    assert_eq!(assistant.end_line, 3);

    // Lines: [user, blank, assistant, blank]
    assert_eq!(rendered.lines.len(), 4);
    assert!(rendered.lines[1].spans.is_empty());
    assert!(rendered.lines[3].spans.is_empty());
}

#[test]
fn message_ranges_skip_non_message_entries() {
    // A summary-only entry produces no rendered output and no MessageRange.
    let entries = vec![
        RenderableEntry {
            entry_index: 0,
            jsonl_line: 1,
            entry: serde_json::from_str(r#"{"type":"summary","summary":"ignored"}"#).unwrap(),
        },
        user_entry(1, "Hello", None),
    ];
    let rendered =
        render_parsed_conversation(&entries, &test_render_options(ToolDisplayMode::Hidden));

    assert_eq!(rendered.messages.len(), 1);
    assert_eq!(rendered.messages[0].entry_index, 1);
    assert_eq!(rendered.messages[0].start_line, 0);
}

#[test]
fn timing_enabled_renders_timestamp_prefix_span() {
    let entries = vec![user_entry(0, "Hello", Some("2026-02-04T12:34:56Z"))];
    let mut options = test_render_options(ToolDisplayMode::Hidden);
    options.show_timing = true;

    let rendered = render_parsed_conversation(&entries, &options);
    let first = &rendered.lines[0];
    let ts_span = &first.spans[0].0;

    assert_eq!(
        ts_span.len(),
        TIMESTAMP_WIDTH,
        "timestamp prefix span width: {:?}",
        ts_span
    );
    assert!(
        ts_span.starts_with(' ') && ts_span.ends_with(' ') && ts_span.contains(':'),
        "timestamp prefix should be ' HH:MM ', got {:?}",
        ts_span
    );
}

#[test]
fn timing_disabled_omits_timestamp_prefix_span() {
    let entries = vec![user_entry(0, "Hello", Some("2026-02-04T12:34:56Z"))];
    let rendered =
        render_parsed_conversation(&entries, &test_render_options(ToolDisplayMode::Hidden));
    let first = &rendered.lines[0];

    // First span is the right-aligned name column, not a timestamp.
    assert_eq!(first.spans[0].0.trim(), "You");
}

#[test]
fn invalid_timestamp_skips_timestamp_prefix() {
    // Even with show_timing=true, a non-RFC3339 timestamp produces no time prefix.
    let entries = vec![user_entry(0, "Hello", Some("not-a-timestamp"))];
    let mut options = test_render_options(ToolDisplayMode::Hidden);
    options.show_timing = true;

    let rendered = render_parsed_conversation(&entries, &options);
    let first = &rendered.lines[0];
    assert_eq!(
        first.spans[0].0.trim(),
        "You",
        "first span should be name column, not timestamp"
    );
}

#[test]
fn assistant_continuation_line_aligns_under_timestamp() {
    // Multi-line assistant text should pad continuation lines to the
    // timestamp width so the name column stays aligned.
    let entries = vec![assistant_text_entry(
        0,
        "line one\\n\\nline two",
        Some("2026-02-04T12:34:56Z"),
    )];
    let mut options = test_render_options(ToolDisplayMode::Hidden);
    options.show_timing = true;

    let rendered = render_parsed_conversation(&entries, &options);
    // Expect at least: header line + blank-paragraph + content line.
    let timestamp_span = &rendered.lines[0].spans[0].0;
    assert_eq!(timestamp_span.len(), TIMESTAMP_WIDTH);

    // Find a continuation line (one whose first span is whitespace of TIMESTAMP_WIDTH).
    let has_padded_continuation = rendered.lines.iter().skip(1).any(|line| {
        line.spans
            .first()
            .is_some_and(|(t, _)| t.len() == TIMESTAMP_WIDTH && t.trim().is_empty())
    });
    assert!(
        has_padded_continuation,
        "expected a continuation line padded to TIMESTAMP_WIDTH"
    );
}

#[test]
fn user_label_uses_text_primary_bold() {
    let entries = vec![user_entry(0, "Hello", None)];
    let rendered =
        render_parsed_conversation(&entries, &test_render_options(ToolDisplayMode::Hidden));
    let line = &rendered.lines[0];
    let name_text = format!("{:>width$}", "You", width = NAME_WIDTH);
    let style = line_style_at(line, &name_text);

    assert_eq!(style.fg, Some(th().text_primary));
    assert!(style.bold);
    assert!(!style.dimmed);
}

#[test]
fn assistant_label_uses_accent_bold() {
    let entries = vec![assistant_text_entry(0, "Hi", None)];
    let rendered =
        render_parsed_conversation(&entries, &test_render_options(ToolDisplayMode::Hidden));
    let line = &rendered.lines[0];
    let name_text = format!("{:>width$}", "Claude", width = NAME_WIDTH);
    let style = line_style_at(line, &name_text);

    assert_eq!(style.fg, Some(th().accent));
    assert!(style.bold);
}

#[test]
fn subagent_assistant_uses_nested_label_when_thinking_shown() {
    let entries = vec![RenderableEntry {
        entry_index: 0,
        jsonl_line: 1,
        entry: serde_json::from_str(
            r#"{"type":"assistant","parent_tool_use_id":"toolu_parent_abc","message":{"role":"assistant","content":[{"type":"text","text":"sub text"}]}}"#,
        )
        .unwrap(),
    }];
    let mut options = test_render_options(ToolDisplayMode::Hidden);
    options.show_thinking = true;

    let rendered = render_parsed_conversation(&entries, &options);
    let text = rendered_text(&rendered);
    assert!(text.contains("sub text"));
    assert!(
        text.contains('↳'),
        "subagent rows should use the nested-label arrow: {}",
        text
    );
}

#[test]
fn truncated_tool_call_header_carries_expected_tool_output_id() {
    let entries = vec![RenderableEntry {
        entry_index: 7,
        jsonl_line: 8,
        entry: serde_json::from_str(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_xyz","name":"Bash","input":{"command":"ls"}}]}}"#,
        )
        .unwrap(),
    }];
    let rendered =
        render_parsed_conversation(&entries, &test_render_options(ToolDisplayMode::Truncated));

    let expected = make_tool_output_id(7, None, 0, ToolOutputKind::ToolCall, Some("toolu_xyz"));
    assert_eq!(
        expected.0,
        "entry:7:parent:top:block:0:kind:call:id:toolu_xyz"
    );
    assert!(
        rendered
            .lines
            .iter()
            .any(|line| line.tool_output_id.as_ref() == Some(&expected)),
        "expected at least one rendered line tagged with the tool output id"
    );
}

#[test]
fn full_tool_mode_lines_are_not_clickable() {
    let entries = vec![RenderableEntry {
        entry_index: 0,
        jsonl_line: 1,
        entry: serde_json::from_str(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"one\ntwo\nthree\nfour\nfive"}}]}}"#,
        )
        .unwrap(),
    }];
    let rendered =
        render_parsed_conversation(&entries, &test_render_options(ToolDisplayMode::Full));

    assert!(
        rendered.lines.iter().all(|line| !line.clickable),
        "Full tool display mode should not produce clickable lines"
    );
    // Body should be fully visible — no truncation indicator.
    let text = rendered_text(&rendered);
    assert!(text.contains("five"));
    assert!(!text.contains("more lines"));
}

#[test]
fn tool_result_string_content_renders_as_text() {
    let entries = vec![
        RenderableEntry {
            entry_index: 0,
            jsonl_line: 1,
            entry: serde_json::from_str(
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"echo hi"}}]}}"#,
            )
            .unwrap(),
        },
        RenderableEntry {
            entry_index: 1,
            jsonl_line: 2,
            entry: serde_json::from_str(
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"hello-world-output"}]}}"#,
            )
            .unwrap(),
        },
    ];
    let rendered =
        render_parsed_conversation(&entries, &test_render_options(ToolDisplayMode::Truncated));

    let text = rendered_text(&rendered);
    assert!(
        text.contains("hello-world-output"),
        "tool result string content should render verbatim: {}",
        text
    );
}

fn assistant_with_reordered_blocks_entry() -> RenderableEntry {
    // Source order is intentionally the reverse of the rendered
    // ordering contract (thinking → tool_use → text). The renderer
    // must reorder to text → tool/summary → thinking.
    RenderableEntry {
        entry_index: 0,
        jsonl_line: 1,
        entry: serde_json::from_str(
            r#"{"type":"assistant","message":{"role":"assistant","content":[
                {"type":"thinking","thinking":"THINK_BLOCK","signature":"sig"},
                {"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"echo hi"}},
                {"type":"text","text":"TEXT_BLOCK"}
            ]}}"#,
        )
        .unwrap(),
    }
}

#[test]
fn assistant_block_order_hidden_mode_text_then_summary_then_thinking() {
    let entries = vec![assistant_with_reordered_blocks_entry()];
    let mut options = test_render_options(ToolDisplayMode::Hidden);
    options.show_thinking = true;

    let rendered = render_parsed_conversation(&entries, &options);
    let text = rendered_text(&rendered);

    let pos_text = text.find("TEXT_BLOCK").expect("text block rendered");
    let pos_thinking = text.find("THINK_BLOCK").expect("thinking block rendered");
    // Hidden mode emits a tool-activity summary row instead of tool calls.
    let pos_summary = text
        .find("shell command")
        .or_else(|| text.find("ran 1"))
        .expect("tool summary row rendered");

    assert!(
        pos_text < pos_summary,
        "text must precede tool summary in Hidden mode: {}",
        text
    );
    assert!(
        pos_summary < pos_thinking,
        "thinking must follow tool summary in Hidden mode: {}",
        text
    );
}

#[test]
fn assistant_block_order_truncated_mode_text_then_tools_then_thinking() {
    let entries = vec![assistant_with_reordered_blocks_entry()];
    let mut options = test_render_options(ToolDisplayMode::Truncated);
    options.show_thinking = true;

    let rendered = render_parsed_conversation(&entries, &options);
    let text = rendered_text(&rendered);

    let pos_text = text.find("TEXT_BLOCK").expect("text block rendered");
    let pos_tool = text.find("Bash").expect("tool call rendered");
    let pos_thinking = text.find("THINK_BLOCK").expect("thinking block rendered");

    assert!(
        pos_text < pos_tool,
        "text must precede tool call in Truncated mode: {}",
        text
    );
    assert!(
        pos_tool < pos_thinking,
        "thinking must follow tool call in Truncated mode: {}",
        text
    );
}

#[test]
fn assistant_block_order_full_mode_text_then_tools_then_thinking() {
    let entries = vec![assistant_with_reordered_blocks_entry()];
    let mut options = test_render_options(ToolDisplayMode::Full);
    options.show_thinking = true;

    let rendered = render_parsed_conversation(&entries, &options);
    let text = rendered_text(&rendered);

    let pos_text = text.find("TEXT_BLOCK").expect("text block rendered");
    let pos_tool = text.find("Bash").expect("tool call rendered");
    let pos_thinking = text.find("THINK_BLOCK").expect("thinking block rendered");

    assert!(pos_text < pos_tool, "text must precede tool: {}", text);
    assert!(
        pos_tool < pos_thinking,
        "thinking must follow tool: {}",
        text
    );
}

#[test]
fn fixture_file_round_trip_renders_user_and_assistant() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.jsonl");
    std::fs::write(
        &path,
        concat!(
            r#"{"type":"user","message":{"role":"user","content":"hello"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"world"}]}}"#,
            "\n",
        ),
    )
    .unwrap();

    let rendered =
        render_conversation(&path, &test_render_options(ToolDisplayMode::Hidden)).unwrap();
    let text = rendered_text(&rendered);
    assert!(text.contains("hello"));
    assert!(text.contains("world"));
    assert_eq!(rendered.messages.len(), 2);
}

// -----------------------------------------------------------------
// Postprocess pipeline unit tests
//
// These exercise the blank-line dedup and message-range remap pass
// directly, without going through the full render pipeline. They
// pin the exact behavior of the extracted helpers.
// -----------------------------------------------------------------

fn nonblank_line(text: &str) -> RenderedLine {
    RenderedLine::new(vec![(text.to_string(), LineStyle::default())])
}
fn blank_line() -> RenderedLine {
    RenderedLine::new(vec![])
}

#[test]
fn postprocess_collapses_runs_of_blanks_to_one() {
    let mut lines = vec![
        nonblank_line("a"),
        blank_line(),
        blank_line(),
        blank_line(),
        nonblank_line("b"),
    ];
    let mut messages = Vec::new();
    postprocess_blank_lines(&mut lines, &mut messages);

    assert_eq!(lines.len(), 3);
    assert!(!lines[0].spans.is_empty());
    assert!(lines[1].spans.is_empty());
    assert!(!lines[2].spans.is_empty());
}

#[test]
fn postprocess_remaps_range_spanning_removed_blank() {
    // Lines: 0=a, 1=blank, 2=blank (removed), 3=b
    // Range covers 0..3 (a + blank), should become 0..2 in compacted output.
    let mut lines = vec![
        nonblank_line("a"),
        blank_line(),
        blank_line(),
        nonblank_line("b"),
    ];
    let mut messages = vec![MessageRange {
        entry_index: 0,
        start_line: 0,
        end_line: 3,
    }];
    postprocess_blank_lines(&mut lines, &mut messages);

    assert_eq!(lines.len(), 3);
    assert_eq!(messages.len(), 1);
    // end_line walks back off the removed blank, which lands on the
    // surviving blank at original index 1 (new index 1), so end = 2.
    assert_eq!(messages[0].start_line, 0);
    assert_eq!(messages[0].end_line, 2);
}

#[test]
fn postprocess_clamps_range_ending_on_removed_blank() {
    // Range ends exactly on a removed blank.
    let mut lines = vec![
        nonblank_line("a"),
        blank_line(),
        blank_line(),
        nonblank_line("b"),
    ];
    let mut messages = vec![MessageRange {
        entry_index: 0,
        start_line: 0,
        end_line: 2,
    }];
    postprocess_blank_lines(&mut lines, &mut messages);

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].start_line, 0);
    // last non-removed before end_line is 1 (kept blank); new index 1 + 1 = 2.
    // That is fine: it includes the kept blank.
    assert!(messages[0].end_line <= lines.len());
    assert!(messages[0].end_line > messages[0].start_line);
}

#[test]
fn postprocess_remaps_first_message_adjacent_to_removed_blank() {
    // Two messages back-to-back, with a doubled blank between them.
    let mut lines = vec![
        nonblank_line("first"),
        blank_line(),
        blank_line(),
        nonblank_line("second"),
        blank_line(),
    ];
    let mut messages = vec![
        MessageRange {
            entry_index: 0,
            start_line: 0,
            end_line: 1,
        },
        MessageRange {
            entry_index: 1,
            start_line: 3,
            end_line: 4,
        },
    ];
    postprocess_blank_lines(&mut lines, &mut messages);

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].start_line, 0);
    assert_eq!(messages[0].end_line, 1);
    // Second message should shift by 1 (one blank removed before it).
    assert_eq!(messages[1].start_line, 2);
    assert_eq!(messages[1].end_line, 3);
}

#[test]
fn postprocess_handles_trailing_blanks() {
    let mut lines = vec![nonblank_line("a"), blank_line(), blank_line(), blank_line()];
    let mut messages = vec![MessageRange {
        entry_index: 0,
        start_line: 0,
        end_line: 1,
    }];
    postprocess_blank_lines(&mut lines, &mut messages);

    // Two of the three trailing blanks collapse out.
    assert_eq!(lines.len(), 2);
    assert_eq!(messages[0].end_line, 1);
}

#[test]
fn postprocess_drops_empty_range_collapsed_to_zero() {
    // start_line == end_line after clamping → range removed.
    let mut lines = vec![nonblank_line("a"), blank_line()];
    let mut messages = vec![MessageRange {
        entry_index: 0,
        start_line: 1,
        end_line: 2,
    }];
    postprocess_blank_lines(&mut lines, &mut messages);

    // start_line was a kept blank at original index 1 → new index 1.
    // end_line maps to new_index[1] + 1 = 2, total_after = 2.
    // Range survives because 1 < 2.
    assert_eq!(messages.len(), 1);
    assert!(messages[0].start_line < messages[0].end_line);
}

#[test]
fn message_range_helper_excludes_trailing_blank() {
    let lines = vec![nonblank_line("hi"), blank_line()];
    let r = message_range_excluding_trailing_blank(&lines, 0, 2, 7).unwrap();
    assert_eq!(r.entry_index, 7);
    assert_eq!(r.start_line, 0);
    assert_eq!(r.end_line, 1);
}

#[test]
fn message_range_helper_returns_none_for_empty_slice() {
    let lines = vec![nonblank_line("hi")];
    assert!(message_range_excluding_trailing_blank(&lines, 1, 1, 0).is_none());
}

#[test]
fn message_range_helper_returns_none_when_only_trailing_blank() {
    let lines = vec![blank_line()];
    assert!(message_range_excluding_trailing_blank(&lines, 0, 1, 0).is_none());
}

// -----------------------------------------------------------------
// Pipeline tests for pending summary flush boundaries
// -----------------------------------------------------------------

#[test]
fn pending_summary_flushes_at_eof() {
    // A trailing tool-only assistant entry with no following user
    // result still flushes at EOF and produces a message range.
    let entries = vec![RenderableEntry {
        entry_index: 0,
        jsonl_line: 1,
        entry: serde_json::from_str(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Grep","input":{"pattern":"x"}}]}}"#,
        )
        .unwrap(),
    }];
    let rendered =
        render_parsed_conversation(&entries, &test_render_options(ToolDisplayMode::Hidden));

    let text = rendered_text(&rendered);
    assert!(text.contains("Searched for 1 pattern"));
    assert_eq!(rendered.messages.len(), 1);
    assert_eq!(rendered.messages[0].entry_index, 0);
}

#[test]
fn pending_summary_flushes_before_non_tool_message() {
    // Tool-only assistant followed by a user text message: the
    // pending summary must flush before the user message renders,
    // and both must produce distinct, non-overlapping ranges.
    let entries = vec![
        RenderableEntry {
            entry_index: 0,
            jsonl_line: 1,
            entry: serde_json::from_str(
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Grep","input":{"pattern":"x"}}]}}"#,
            )
            .unwrap(),
        },
        user_entry(1, "follow up", None),
    ];
    let rendered =
        render_parsed_conversation(&entries, &test_render_options(ToolDisplayMode::Hidden));

    assert_eq!(rendered.messages.len(), 2);
    assert_eq!(rendered.messages[0].entry_index, 0);
    assert_eq!(rendered.messages[1].entry_index, 1);
    assert!(rendered.messages[0].end_line <= rendered.messages[1].start_line);
    let text = rendered_text(&rendered);
    assert!(text.contains("Searched for 1 pattern"));
    assert!(text.contains("follow up"));
}

#[test]
fn consecutive_blank_lines_collapse_and_remap_ranges() {
    // Two adjacent user messages each emit a trailing blank; the dedup pass
    // collapses any double-blank that would arise from this sequence.
    let entries = vec![user_entry(0, "first", None), user_entry(1, "second", None)];
    let rendered =
        render_parsed_conversation(&entries, &test_render_options(ToolDisplayMode::Hidden));

    // No two consecutive empty lines should remain.
    for pair in rendered.lines.windows(2) {
        assert!(
            !(pair[0].spans.is_empty() && pair[1].spans.is_empty()),
            "consecutive empty lines should be collapsed"
        );
    }

    // Both user message ranges should still target valid, distinct lines.
    assert_eq!(rendered.messages.len(), 2);
    assert!(rendered.messages[0].end_line <= rendered.messages[1].start_line);
    assert!(rendered.messages[0].start_line < rendered.messages[0].end_line);
    assert!(rendered.messages[1].start_line < rendered.messages[1].end_line);
}

// -----------------------------------------------------------------
// Message renderer template ordering
//
// Pin the template-order contract: regardless of raw JSON block
// order, assistant messages render text first, then tool activity,
// then thinking. Skill-marker user messages render dimmed but
// remain top-level (not nested with the ↳ arrow).
// -----------------------------------------------------------------

fn first_index(text: &str, needle: &str) -> Option<usize> {
    text.find(needle)
}

#[test]
fn assistant_template_order_is_text_then_tool_then_thinking() {
    // Raw JSON order: thinking, tool_use, text. Template ordering
    // (text → tool → thinking) must override that.
    let entries = vec![RenderableEntry {
        entry_index: 0,
        jsonl_line: 1,
        entry: serde_json::from_str(
            r#"{"type":"assistant","message":{"role":"assistant","content":[
                {"type":"thinking","thinking":"deep thought","signature":"sig"},
                {"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}},
                {"type":"text","text":"text reply"}
            ]}}"#,
        )
        .unwrap(),
    }];
    let mut options = test_render_options(ToolDisplayMode::Truncated);
    options.show_thinking = true;
    let rendered = render_parsed_conversation(&entries, &options);
    let text = rendered_text(&rendered);

    let i_text = first_index(&text, "text reply").expect("text block rendered");
    let i_tool = first_index(&text, "Bash").expect("tool call rendered");
    let i_think = first_index(&text, "deep thought").expect("thinking rendered");

    assert!(i_text < i_tool, "text must precede tool call: {}", text);
    assert!(
        i_tool < i_think,
        "tool call must precede thinking: {}",
        text
    );
}

#[test]
fn skill_marker_user_message_renders_dimmed_but_top_level() {
    let entries = vec![RenderableEntry {
        entry_index: 0,
        jsonl_line: 1,
        entry: serde_json::from_str(
            r#"{"type":"user","message":{"role":"user","content":"Base directory for this skill: /tmp/x"}}"#,
        )
        .unwrap(),
    }];
    let rendered =
        render_parsed_conversation(&entries, &test_render_options(ToolDisplayMode::Hidden));
    let line = &rendered.lines[0];
    let name_text = format!("{:>width$}", "You", width = NAME_WIDTH);
    let style = line_style_at(line, &name_text);

    // Skill messages keep the "You" label (not ↳…), but render
    // dimmed without bold.
    assert!(style.dimmed);
    assert!(!style.bold);
}

#[test]
fn agent_progress_user_with_text_and_result_keeps_template_order() {
    // agent_progress user blocks aggregate text then render tool
    // results. Both must appear, in that order.
    let entries = vec![RenderableEntry {
        entry_index: 0,
        jsonl_line: 1,
        entry: serde_json::from_str(
            r#"{"type":"progress","data":{"type":"agent_progress","agentId":"agent-abc1234","message":{"type":"user","message":{"role":"user","content":[
                {"type":"text","text":"agent says hi"},
                {"type":"tool_result","tool_use_id":"toolu_1","content":"result body"}
            ]}}}}"#,
        )
        .unwrap(),
    }];
    let mut options = test_render_options(ToolDisplayMode::Truncated);
    options.show_thinking = true;
    let rendered = render_parsed_conversation(&entries, &options);
    let text = rendered_text(&rendered);

    let i_text = first_index(&text, "agent says hi").expect("text rendered");
    let i_result = first_index(&text, "result body").expect("result rendered");
    assert!(i_text < i_result, "text must precede result: {}", text);
}

#[test]
fn excluded_entry_kinds_produce_no_lines() {
    // Summary, system, custom-title, file-history, agent-name
    // entries are inert — they render nothing and do not produce
    // message ranges.
    let entries = vec![
        RenderableEntry {
            entry_index: 0,
            jsonl_line: 1,
            entry: serde_json::from_str(r#"{"type":"summary","summary":"x"}"#).unwrap(),
        },
        RenderableEntry {
            entry_index: 1,
            jsonl_line: 2,
            entry: serde_json::from_str(r#"{"type":"system","subtype":"info","message":"sys"}"#)
                .unwrap(),
        },
    ];
    let rendered =
        render_parsed_conversation(&entries, &test_render_options(ToolDisplayMode::Hidden));
    assert!(rendered.lines.is_empty());
    assert!(rendered.messages.is_empty());
}

#[test]
fn a_note_prints_the_label_of_the_annotator_holding_it() {
    let entries = vec![user_entry(0, "first message", None)];
    let mut note = annotation("n1", "recap", "cache rewrite reverted", vec![1]);
    note.annotator = "chsum".to_string();
    let mut options = annotated_options(vec![note], true);
    options
        .annotator_labels
        .insert("chsum".to_string(), "Chsum".to_string());

    let rendered = rendered_text(&render_parsed_conversation(&entries, &options));

    assert!(rendered.contains("Chsum"), "{rendered}");
    assert!(!rendered.contains("Note"), "{rendered}");
}

#[test]
fn a_note_from_an_unlabelled_annotator_prints_its_key_capitalised() {
    let entries = vec![user_entry(0, "first message", None)];
    let mut note = annotation("n1", "note", "by hand", vec![1]);
    note.annotator = "file".to_string();
    let options = annotated_options(vec![note], true);

    let rendered = rendered_text(&render_parsed_conversation(&entries, &options));

    assert!(rendered.contains("File"), "{rendered}");
}

#[test]
fn a_label_longer_than_the_name_column_is_truncated() {
    let entries = vec![user_entry(0, "first message", None)];
    let mut note = annotation("n1", "finding", "unchecked index", vec![1]);
    note.annotator = "review".to_string();
    let mut options = annotated_options(vec![note], true);
    options
        .annotator_labels
        .insert("review".to_string(), "CodeReviewer".to_string());

    let rendered = rendered_text(&render_parsed_conversation(&entries, &options));

    // Nine characters is the width every other name in the ledger occupies;
    // a wider one would push this row's text out of the shared column.
    assert!(rendered.contains("CodeRevie"), "{rendered}");
    assert!(!rendered.contains("CodeReviewer"), "{rendered}");
}
