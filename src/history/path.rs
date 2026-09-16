//! Path encoding/decoding utilities for Claude project directories.
//!
//! Claude encodes project paths as directory names by replacing non-alphanumeric
//! characters (except `-`) with `-`. This module provides utilities to convert
//! between paths and their encoded forms.

use std::path::{Path, PathBuf};

/// Convert the current working directory into Claude's project directory name.
pub fn convert_path_to_project_dir_name(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Format a path into a short display name.
///
/// For worktree paths like `/Users/raine/code/claude-history__worktrees/claude-search`,
/// returns `claude-history/claude-search` to show both the main project and worktree name.
///
/// For regular paths, returns just the folder name.
pub fn format_short_name_from_path(path: &Path) -> String {
    let path_str = path.to_string_lossy();

    // Check for worktree pattern in the path
    if let Some(wt_pos) = path_str
        .find("__worktrees/")
        .or_else(|| path_str.find("/.worktrees/"))
    {
        let is_hidden = path_str[wt_pos..].starts_with("/.");
        let separator_len = if is_hidden {
            "/.worktrees/".len()
        } else {
            "__worktrees/".len()
        };

        // Get main project (folder before __worktrees)
        let before = &path_str[..wt_pos];
        let main_project = Path::new(before)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        // Get worktree name (folder after __worktrees/)
        let after = &path_str[wt_pos + separator_len..];
        let worktree = after.split('/').next().unwrap_or("");

        if !main_project.is_empty() && !worktree.is_empty() {
            return format!("{}/{}", main_project, worktree);
        }
    }

    // Not a worktree, just return the folder name
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path_str.into_owned())
}

/// The encoded worktree marker that appears in Claude project directory names.
///
/// workmux creates worktrees at `<project>/.worktrees/<branch>/` (or
/// `<project>__worktrees/<branch>/` on older setups). Both encode to
/// `--worktrees-` because `.`, `_`, and `/` all become `-` in Claude's
/// encoding scheme.
const WORKTREE_MARKER: &str = "--worktrees-";

/// Extract the encoded project root from an encoded project directory name.
///
/// If the encoded name contains a worktree marker (`--worktrees-`), returns
/// everything before it (the project root portion). Otherwise returns the
/// full encoded name as-is.
///
/// # Examples
/// ```
/// # use claude_history::history::path::encoded_project_root;
/// assert_eq!(
///     encoded_project_root("-Users-raine-code-project--worktrees-branch"),
///     "-Users-raine-code-project"
/// );
/// assert_eq!(
///     encoded_project_root("-Users-raine-code-project"),
///     "-Users-raine-code-project"
/// );
/// ```
pub fn encoded_project_root(encoded: &str) -> &str {
    encoded
        .split_once(WORKTREE_MARKER)
        .map_or(encoded, |(root, _)| root)
}

/// Check if two encoded project directory names belong to the same project.
///
/// Two names are considered part of the same project if they share the same
/// encoded project root (i.e., stripping any workmux `--worktrees-<branch>`
/// suffix yields the same string).
///
/// On Windows the comparison is case-insensitive. NTFS is case-insensitive
/// and case-preserving, so the same logical directory can be reached via
/// differently-cased paths (e.g. `cd myproject` vs `cd MyProject`) and reported
/// back as differently-cased cwd strings. Because the encoded form preserves
/// the original casing, a case-sensitive compare causes the workspace filter
/// (`-L` / `Tab`) to drop every conversation when the current shell's cwd
/// casing differs from the casing recorded when the sessions were created.
/// The encoded form contains only ASCII alphanumeric characters and `-`, so
/// `eq_ignore_ascii_case` is sufficient.
pub fn is_same_project(a: &str, b: &str) -> bool {
    let root_a = encoded_project_root(a);
    let root_b = encoded_project_root(b);
    #[cfg(windows)]
    {
        root_a.eq_ignore_ascii_case(root_b)
    }
    #[cfg(not(windows))]
    {
        root_a == root_b
    }
}

/// Decode a project directory name back to a path (simple heuristic fallback).
///
/// Claude's encoding replaces all non-alphanumeric characters (except `-`) with `-`.
/// This means `/`, `_`, and `.` all become `-`, making the encoding lossy.
///
/// This is only used as a fallback for old JSONL files that don't have the cwd field.
/// The cwd field from JSONL provides the accurate path and should be preferred.
pub fn decode_project_dir_name_to_path(encoded: &str) -> PathBuf {
    PathBuf::from(decode_with_double_dash_as(encoded, "__"))
}

/// Decode with a specific replacement for double dashes
fn decode_with_double_dash_as(encoded: &str, double_dash_replacement: &str) -> String {
    let mut result = String::with_capacity(encoded.len());
    let mut chars = encoded.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '-' {
            let mut count = 1;
            while chars.peek() == Some(&'-') {
                chars.next();
                count += 1;
            }

            match count {
                1 => result.push('/'),
                2 => result.push_str(double_dash_replacement),
                n => {
                    result.push('/');
                    for _ in 0..((n - 1) / 2) {
                        result.push_str(double_dash_replacement);
                    }
                    if (n - 1) % 2 == 1 {
                        result.push('/');
                    }
                }
            }
        } else {
            result.push(c);
        }
    }

    result
}

/// Decode a project directory name back to a readable path (for display purposes).
///
/// Claude's encoding replaces all non-alphanumeric characters (except `-`) with `-`.
/// This means `/`, `_`, and `.` all become `-`, making the encoding lossy and
/// impossible to reverse perfectly.
///
/// We use a heuristic based on consecutive dash count:
/// - Odd (1, 3, 5...): `/` followed by underscores (e.g. `-` -> `/`, `---` -> `/__`)
/// - Even (2, 4...): All underscores (e.g. `--` -> `__`)
///
/// This prioritizes `__` (common in directory names like git worktrees) over `/_`.
/// Single underscores and dots in the original path will be incorrectly decoded as `/`,
/// but the result is still recognizable enough for project selection in fzf.
pub fn decode_project_dir_name(encoded: &str) -> String {
    let mut result = String::with_capacity(encoded.len());
    let mut chars = encoded.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '-' {
            // Count consecutive dashes
            let mut count = 1;
            while chars.peek() == Some(&'-') {
                chars.next();
                count += 1;
            }

            if count % 2 == 1 {
                // Odd: first is '/', rest are '_'
                result.push('/');
                for _ in 0..(count - 1) {
                    result.push('_');
                }
            } else {
                // Even: all are '_'
                for _ in 0..count {
                    result.push('_');
                }
            }
        } else {
            result.push(c);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // === Encoding tests ===

    #[test]
    fn converts_various_separators_and_punctuation() {
        let path = Path::new("/Users/raine/code/workmux/.worktrees/uncommitted");
        let converted = convert_path_to_project_dir_name(path);
        assert_eq!(
            converted,
            "-Users-raine-code-workmux--worktrees-uncommitted"
        );
    }

    #[test]
    fn preserves_alphanumeric_and_existing_dashes() {
        let path = Path::new("/tmp/foo-Bar123");
        let converted = convert_path_to_project_dir_name(path);
        assert_eq!(converted, "-tmp-foo-Bar123");
    }

    #[test]
    fn encodes_worktree_with_double_underscore() {
        let path = Path::new("/Users/raine/code/claude-history__worktrees/claude-search");
        let converted = convert_path_to_project_dir_name(path);
        assert_eq!(
            converted,
            "-Users-raine-code-claude-history--worktrees-claude-search"
        );
    }

    #[test]
    fn encodes_hidden_directory() {
        let path = Path::new("/Users/raine/dotfiles/.config/karabiner");
        let converted = convert_path_to_project_dir_name(path);
        assert_eq!(converted, "-Users-raine-dotfiles--config-karabiner");
    }

    // === Display decode tests (decode_project_dir_name) ===

    #[test]
    fn decodes_consecutive_dashes_to_underscores() {
        // Double dash -> __ (even count = all underscores)
        let encoded = "-Users-raine-code-myproject--worktrees-feature";
        let decoded = decode_project_dir_name(encoded);
        assert_eq!(decoded, "/Users/raine/code/myproject__worktrees/feature");

        // Triple dash -> /__ (odd count = slash + underscores)
        let encoded = "-Users-raine-code-myproject---worktrees-feature";
        let decoded = decode_project_dir_name(encoded);
        assert_eq!(decoded, "/Users/raine/code/myproject/__worktrees/feature");
    }

    #[test]
    fn decodes_single_dashes_to_slashes() {
        let encoded = "-tmp-foo-Bar123";
        let decoded = decode_project_dir_name(encoded);
        assert_eq!(decoded, "/tmp/foo/Bar123");
    }

    // === Fallback decode tests (decode_with_double_dash_as) ===

    #[test]
    fn decode_with_double_dash_as_underscore() {
        let encoded = "-Users-raine-code-project--worktrees-feature";
        let decoded = decode_with_double_dash_as(encoded, "__");
        assert_eq!(decoded, "/Users/raine/code/project__worktrees/feature");
    }

    #[test]
    fn decode_with_double_dash_as_hidden_dir() {
        let encoded = "-Users-raine-dotfiles--config-karabiner";
        let decoded = decode_with_double_dash_as(encoded, "/.");
        assert_eq!(decoded, "/Users/raine/dotfiles/.config/karabiner");
    }

    #[test]
    fn decode_preserves_dashes_in_folder_names_in_fallback() {
        // Note: The fallback decode can't distinguish dashes in folder names
        // from path separators - this is expected behavior
        let encoded = "-Users-raine-code-claude-history";
        let decoded = decode_with_double_dash_as(encoded, "__");
        // This incorrectly decodes to /Users/raine/code/claude/history
        // because single dashes are treated as path separators
        assert_eq!(decoded, "/Users/raine/code/claude/history");
    }

    // === Worktree path structure tests ===

    #[test]
    fn worktree_encoded_pattern() {
        // Verify the encoding pattern for worktrees
        let path = Path::new("/Users/raine/code/WalkingMate__worktrees/template-engine");
        let encoded = convert_path_to_project_dir_name(path);
        assert_eq!(
            encoded,
            "-Users-raine-code-WalkingMate--worktrees-template-engine"
        );

        // The --worktrees- pattern should be detectable
        assert!(encoded.contains("--worktrees-"));
    }

    #[test]
    fn extract_worktree_name_from_encoded() {
        let encoded = "-Users-raine-code-WalkingMate--worktrees-template-engine";

        // Find the worktree marker
        let wt_pos = encoded.find("--worktrees-").unwrap();

        // Extract worktree name (everything after --worktrees-)
        let worktree_name = &encoded[wt_pos + "--worktrees-".len()..];
        assert_eq!(worktree_name, "template-engine");
    }

    #[test]
    fn extract_project_name_before_worktrees() {
        let encoded = "-Users-raine-code-WalkingMate--worktrees-template-engine";

        // Find the worktree marker
        let wt_pos = encoded.find("--worktrees-").unwrap();

        // Extract the part before --worktrees
        let before_wt = &encoded[..wt_pos];
        assert_eq!(before_wt, "-Users-raine-code-WalkingMate");

        // When decoded with filesystem check, this should give us WalkingMate as the project name
        // For fallback, it decodes to a path ending in WalkingMate
        let decoded = decode_with_double_dash_as(before_wt, "__");
        assert_eq!(decoded, "/Users/raine/code/WalkingMate");
    }

    // === format_project_short_name tests (worktree display) ===

    #[test]
    fn format_short_name_extracts_worktree_pattern() {
        // Test the worktree pattern detection in decoded paths
        let path = "/Users/raine/code/WalkingMate__worktrees/template-engine";

        // Check for worktree pattern
        assert!(path.contains("__worktrees/"));

        // Extract main project
        let wt_pos = path.find("__worktrees/").unwrap();
        let before = &path[..wt_pos];
        let main_project = Path::new(before)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap();
        assert_eq!(main_project, "WalkingMate");

        // Extract worktree name
        let after = &path[wt_pos + "__worktrees/".len()..];
        let worktree = after.split('/').next().unwrap();
        assert_eq!(worktree, "template-engine");

        // Combined display
        let display = format!("{}/{}", main_project, worktree);
        assert_eq!(display, "WalkingMate/template-engine");
    }

    #[test]
    fn format_short_name_hidden_worktrees() {
        // Test .worktrees pattern (hidden worktrees folder)
        let path = "/Users/raine/code/workmux/.worktrees/uncommitted";

        // Check for hidden worktree pattern
        assert!(path.contains("/.worktrees/"));

        let wt_pos = path.find("/.worktrees/").unwrap();
        let before = &path[..wt_pos];
        let main_project = Path::new(before)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap();
        assert_eq!(main_project, "workmux");

        let after = &path[wt_pos + "/.worktrees/".len()..];
        let worktree = after.split('/').next().unwrap();
        assert_eq!(worktree, "uncommitted");
    }

    // === Project root and same-project matching tests ===

    #[test]
    fn encoded_project_root_strips_worktree_suffix() {
        assert_eq!(
            encoded_project_root("-Users-raine-code-project--worktrees-branch"),
            "-Users-raine-code-project"
        );
    }

    #[test]
    fn encoded_project_root_returns_full_name_without_worktree() {
        assert_eq!(
            encoded_project_root("-Users-raine-code-project"),
            "-Users-raine-code-project"
        );
    }

    #[test]
    fn is_same_project_matches_main_and_worktree() {
        let main = "-Users-raine-code-project";
        let worktree = "-Users-raine-code-project--worktrees-fix-search";
        assert!(is_same_project(main, worktree));
        assert!(is_same_project(worktree, main));
    }

    #[test]
    fn is_same_project_matches_two_worktrees() {
        let wt1 = "-Users-raine-code-project--worktrees-branch-a";
        let wt2 = "-Users-raine-code-project--worktrees-branch-b";
        assert!(is_same_project(wt1, wt2));
    }

    #[test]
    fn is_same_project_matches_identical() {
        let name = "-Users-raine-code-project";
        assert!(is_same_project(name, name));
    }

    #[test]
    fn is_same_project_rejects_different_projects() {
        let a = "-Users-raine-code-project-a";
        let b = "-Users-raine-code-project-b";
        assert!(!is_same_project(a, b));
    }

    #[test]
    fn is_same_project_hidden_worktrees() {
        // .worktrees encodes to --worktrees- (dot becomes dash)
        let main = convert_path_to_project_dir_name(Path::new("/Users/raine/code/myproject"));
        let worktree = convert_path_to_project_dir_name(Path::new(
            "/Users/raine/code/myproject/.worktrees/feature",
        ));
        assert!(is_same_project(&main, &worktree));
    }

    #[test]
    fn is_same_project_double_underscore_worktrees() {
        // __worktrees also encodes to --worktrees-
        let main = convert_path_to_project_dir_name(Path::new("/Users/raine/code/myproject"));
        let worktree = convert_path_to_project_dir_name(Path::new(
            "/Users/raine/code/myproject__worktrees/feature",
        ));
        assert!(is_same_project(&main, &worktree));
    }

    // === Platform-specific case sensitivity tests ===
    //
    // Windows filesystems are case-insensitive, so the same on-disk directory
    // can be reached via differently-cased paths and the workspace filter must
    // treat them as the same project. POSIX filesystems are typically
    // case-sensitive, so differently-cased encoded names refer to genuinely
    // distinct directories and must remain not-equal.

    #[cfg(windows)]
    #[test]
    fn is_same_project_case_insensitive_on_windows() {
        let lowercase = "-Users-Foo-myproject";
        let mixed = "-Users-Foo-MyProject";
        assert!(is_same_project(lowercase, mixed));
        assert!(is_same_project(mixed, lowercase));

        // Worktree on the lowercase variant matches main on the uppercase variant
        let worktree_lower = "-Users-Foo-myproject--worktrees-feature";
        let main_upper = "-Users-Foo-MyProject";
        assert!(is_same_project(worktree_lower, main_upper));
    }

    #[cfg(not(windows))]
    #[test]
    fn is_same_project_case_sensitive_on_unix() {
        let lowercase = "-home-user-myproject";
        let mixed = "-home-user-MyProject";
        assert!(!is_same_project(lowercase, mixed));
    }
}
