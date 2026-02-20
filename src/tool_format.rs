//! Tool-specific formatting for nicer display of tool calls.
//!
//! Instead of showing raw JSON, this module formats each tool's input
//! in a human-readable way that highlights the most relevant information.

use serde_json::Value;

/// Formatted tool call representation
pub struct FormattedToolCall {
    /// The header line (e.g., "Task (Explore): description" or "$ command")
    pub header: String,
    /// Optional continuation lines (e.g., prompt text, diff lines)
    pub body: Option<String>,
}

/// Format a tool call for display
///
/// The `max_width` parameter controls line wrapping for tools with long content (e.g., Bash commands).
pub fn format_tool_call(name: &str, input: &Value, max_width: usize) -> FormattedToolCall {
    match name {
        // Claude Code tools
        "Task" => format_task(input),
        "Bash" => format_bash(input, max_width),
        "Read" => format_read(input),
        "Grep" => format_grep(input),
        "Glob" => format_glob(input),
        "Edit" => format_edit(input),
        "Write" => format_write(input),
        "WebFetch" => format_web_fetch(input),
        "WebSearch" => format_web_search(input),
        // Cursor tools — map to equivalent formatting
        "run_terminal_cmd" | "run_terminal_command_v2" => format_cursor_terminal(input, max_width),
        "read_file" | "read_file_v2" => format_cursor_read(input),
        "edit_file" | "edit_file_v2" | "edit_file_v2_search_replace"
        | "edit_file_v2_apply_based" | "edit_file_v2_write" | "search_replace"
        | "apply_patch" | "MultiEdit" => format_cursor_edit(input),
        "grep" | "grep_search" | "rg" | "ripgrep" | "ripgrep_raw_search"
        | "codebase_search" | "semantic_search_full" => format_cursor_search(input),
        "list_dir" | "list_dir_v2" | "glob_file_search" | "file_search" => {
            format_cursor_list(input)
        }
        "web_fetch" => format_web_fetch(input),
        "web_search" => format_web_search(input),
        "write" | "delete_file" => format_cursor_write(input),
        "task_v2" | "create_plan" => format_cursor_task(input),
        _ => format_fallback(name, input),
    }
}

fn format_task(input: &Value) -> FormattedToolCall {
    let subagent_type = input
        .get("subagent_type")
        .and_then(|v| v.as_str())
        .unwrap_or("agent");
    let description = input
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let prompt = input.get("prompt").and_then(|v| v.as_str());

    FormattedToolCall {
        header: format!("Task ({}): {}", subagent_type, description),
        body: prompt.map(|p| p.to_string()),
    }
}

fn format_bash(input: &Value, max_width: usize) -> FormattedToolCall {
    let command = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
    let prefix = "Bash: ";
    let prefix_len = prefix.len();

    // Available width for command text (accounting for prefix on first line)
    let available_width = max_width.saturating_sub(prefix_len);

    // No wrapping if width is too small or command fits
    if available_width == 0 || command.chars().count() <= available_width {
        return FormattedToolCall {
            header: format!("{}{}", prefix, command),
            body: None,
        };
    }

    // Wrap the command text
    let wrapped: Vec<_> = textwrap::wrap(command, available_width)
        .into_iter()
        .map(|cow| cow.into_owned())
        .collect();

    if wrapped.len() <= 1 {
        return FormattedToolCall {
            header: format!("{}{}", prefix, command),
            body: None,
        };
    }

    // First line goes in header, rest in body
    let header = format!("{}{}", prefix, wrapped[0]);
    let body = wrapped[1..].join("\n");

    FormattedToolCall {
        header,
        body: Some(body),
    }
}

fn format_read(input: &Value) -> FormattedToolCall {
    let file_path = input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let offset = input.get("offset").and_then(|v| v.as_u64());
    let limit = input.get("limit").and_then(|v| v.as_u64());

    let header = match (offset, limit) {
        (Some(o), Some(l)) => format!("Read: {}:{}-{}", file_path, o, o + l),
        (Some(o), None) => format!("Read: {}:{}", file_path, o),
        _ => format!("Read: {}", file_path),
    };

    FormattedToolCall { header, body: None }
}

fn format_grep(input: &Value) -> FormattedToolCall {
    let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
    let path = input.get("path").and_then(|v| v.as_str());
    let glob = input.get("glob").and_then(|v| v.as_str());

    let location = match (path, glob) {
        (Some(p), Some(g)) => format!("{}/{}", p, g),
        (Some(p), None) => p.to_string(),
        (None, Some(g)) => g.to_string(),
        (None, None) => ".".to_string(),
    };

    FormattedToolCall {
        header: format!("Grep: \"{}\" in {}", pattern, location),
        body: None,
    }
}

fn format_glob(input: &Value) -> FormattedToolCall {
    let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
    let path = input.get("path").and_then(|v| v.as_str());

    let header = match path {
        Some(p) => format!("Glob: {} in {}", pattern, p),
        None => format!("Glob: {}", pattern),
    };

    FormattedToolCall { header, body: None }
}

fn format_edit(input: &Value) -> FormattedToolCall {
    let file_path = input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let old_string = input.get("old_string").and_then(|v| v.as_str());
    let new_string = input.get("new_string").and_then(|v| v.as_str());

    let body = match (old_string, new_string) {
        (Some(old), Some(new)) => {
            let mut diff = String::new();
            for line in old.lines() {
                diff.push_str(&format!("- {}\n", line));
            }
            for line in new.lines() {
                diff.push_str(&format!("+ {}\n", line));
            }
            // Remove trailing newline
            if diff.ends_with('\n') {
                diff.pop();
            }
            Some(diff)
        }
        _ => None,
    };

    FormattedToolCall {
        header: format!("Edit: {}", file_path),
        body,
    }
}

fn format_write(input: &Value) -> FormattedToolCall {
    let file_path = input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    FormattedToolCall {
        header: format!("Write: {}", file_path),
        body: None,
    }
}

fn format_web_fetch(input: &Value) -> FormattedToolCall {
    let url = input.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let prompt = input.get("prompt").and_then(|v| v.as_str());

    FormattedToolCall {
        header: format!("Fetch: {}", url),
        body: prompt.map(|p| p.to_string()),
    }
}

fn format_web_search(input: &Value) -> FormattedToolCall {
    let query = input.get("query").and_then(|v| v.as_str()).unwrap_or("");

    FormattedToolCall {
        header: format!("Search: \"{}\"", query),
        body: None,
    }
}

// --- Cursor tool formatters ---

fn format_cursor_terminal(input: &Value, max_width: usize) -> FormattedToolCall {
    let command = input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let prefix = "Bash: ";
    let prefix_len = prefix.len();
    let available_width = max_width.saturating_sub(prefix_len);

    if available_width == 0 || command.chars().count() <= available_width {
        return FormattedToolCall {
            header: format!("{}{}", prefix, command),
            body: None,
        };
    }

    let wrapped: Vec<_> = textwrap::wrap(command, available_width)
        .into_iter()
        .map(|cow| cow.into_owned())
        .collect();
    if wrapped.len() <= 1 {
        return FormattedToolCall {
            header: format!("{}{}", prefix, command),
            body: None,
        };
    }
    let header = format!("{}{}", prefix, wrapped[0]);
    let body = wrapped[1..].join("\n");
    FormattedToolCall {
        header,
        body: Some(body),
    }
}

fn format_cursor_read(input: &Value) -> FormattedToolCall {
    let file_path = input
        .get("filePath")
        .or_else(|| input.get("file_path"))
        .or_else(|| input.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    FormattedToolCall {
        header: format!("Read: {}", file_path),
        body: None,
    }
}

fn format_cursor_edit(input: &Value) -> FormattedToolCall {
    let file_path = input
        .get("filePath")
        .or_else(|| input.get("file_path"))
        .or_else(|| input.get("target_file"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    FormattedToolCall {
        header: format!("Edit: {}", file_path),
        body: None,
    }
}

fn format_cursor_search(input: &Value) -> FormattedToolCall {
    let query = input
        .get("query")
        .or_else(|| input.get("pattern"))
        .or_else(|| input.get("regex"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let path = input
        .get("path")
        .or_else(|| input.get("directory"))
        .and_then(|v| v.as_str());
    let header = if let Some(p) = path {
        format!("Search: \"{}\" in {}", query, p)
    } else {
        format!("Search: \"{}\"", query)
    };
    FormattedToolCall {
        header,
        body: None,
    }
}

fn format_cursor_list(input: &Value) -> FormattedToolCall {
    let path = input
        .get("path")
        .or_else(|| input.get("directory"))
        .or_else(|| input.get("pattern"))
        .and_then(|v| v.as_str())
        .unwrap_or(".");
    FormattedToolCall {
        header: format!("List: {}", path),
        body: None,
    }
}

fn format_cursor_write(input: &Value) -> FormattedToolCall {
    let file_path = input
        .get("filePath")
        .or_else(|| input.get("file_path"))
        .or_else(|| input.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    FormattedToolCall {
        header: format!("Write: {}", file_path),
        body: None,
    }
}

fn format_cursor_task(input: &Value) -> FormattedToolCall {
    let description = input
        .get("description")
        .or_else(|| input.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    FormattedToolCall {
        header: format!("Task: {}", description),
        body: None,
    }
}

fn format_fallback(name: &str, input: &Value) -> FormattedToolCall {
    let body = serde_json::to_string_pretty(input).ok();

    FormattedToolCall {
        header: format!("{}:", name),
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_format_task() {
        let input = json!({
            "subagent_type": "Explore",
            "description": "Find the bug",
            "prompt": "Look for issues in the code"
        });
        let result = format_tool_call("Task", &input, 80);
        assert_eq!(result.header, "Task (Explore): Find the bug");
        assert_eq!(result.body, Some("Look for issues in the code".to_string()));
    }

    #[test]
    fn test_format_bash() {
        let input = json!({
            "command": "git status",
            "description": "Check repo status"
        });
        let result = format_tool_call("Bash", &input, 80);
        assert_eq!(result.header, "Bash: git status");
        assert_eq!(result.body, None);
    }

    #[test]
    fn test_format_bash_wrapping() {
        let long_command = "cargo build --release --features 'feature1 feature2 feature3' --target x86_64-unknown-linux-gnu";
        let input = json!({
            "command": long_command
        });
        // With width 40, command should wrap (available width is 40 - 6 = 34 for command text)
        let result = format_tool_call("Bash", &input, 40);
        assert!(result.header.starts_with("Bash: cargo"));
        assert!(
            result.body.is_some(),
            "Long command should have body for continuation"
        );
    }

    #[test]
    fn test_format_bash_no_wrap_when_fits() {
        let input = json!({
            "command": "ls -la"
        });
        let result = format_tool_call("Bash", &input, 80);
        assert_eq!(result.header, "Bash: ls -la");
        assert_eq!(result.body, None);
    }

    #[test]
    fn test_format_read_with_range() {
        let input = json!({
            "file_path": "/src/main.rs",
            "offset": 100,
            "limit": 50
        });
        let result = format_tool_call("Read", &input, 80);
        assert_eq!(result.header, "Read: /src/main.rs:100-150");
    }

    #[test]
    fn test_format_grep() {
        let input = json!({
            "pattern": "fn main",
            "path": "src",
            "glob": "*.rs"
        });
        let result = format_tool_call("Grep", &input, 80);
        assert_eq!(result.header, "Grep: \"fn main\" in src/*.rs");
    }

    #[test]
    fn test_format_edit() {
        let input = json!({
            "file_path": "/src/lib.rs",
            "old_string": "old code",
            "new_string": "new code"
        });
        let result = format_tool_call("Edit", &input, 80);
        assert_eq!(result.header, "Edit: /src/lib.rs");
        assert_eq!(result.body, Some("- old code\n+ new code".to_string()));
    }
}
