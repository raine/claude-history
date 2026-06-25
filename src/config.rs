use crate::error::{AppError, Result};
use crate::search::mode::SearchMode;
use crossterm::event::{KeyCode, KeyModifiers};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

/// Defines the structure of the config.toml file.
/// Using `Option` allows distinguishing between a value being unset
/// vs. explicitly set to `false`.
#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    /// Deprecated: global is now the default. Use `--local` flag or Tab toggle instead.
    /// Kept for backwards compatibility with existing config files.
    #[allow(dead_code)]
    pub global: Option<bool>,
    pub display: Option<DisplayConfig>,
    pub resume: Option<ResumeConfig>,
    pub keys: Option<KeysConfig>,
    pub tui: Option<TuiConfig>,
    pub search: Option<SearchConfig>,
    pub agent: Option<AgentConfig>,
    pub filter: Option<FilterConfig>,
}

impl ConfigFile {
    /// Effective list of first-line substrings used to exclude machine-generated
    /// sessions from loading. ICM (persistent-memory) background sessions are
    /// excluded by default; set `[filter] exclude_icm = false` to keep them, and
    /// add extra substrings via `[filter] exclude_markers = [...]`.
    pub fn exclude_markers(&self) -> Vec<String> {
        let filter = self.filter.as_ref();
        let exclude_icm = filter.and_then(|f| f.exclude_icm).unwrap_or(true);
        let mut markers = Vec::new();
        if exclude_icm {
            markers.push(crate::history::ICM_SESSION_MARKER.to_string());
        }
        if let Some(extra) = filter.and_then(|f| f.exclude_markers.as_ref()) {
            markers.extend(extra.iter().cloned());
        }
        markers
    }
}

#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    pub mode: Option<SearchMode>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AgentScopeConfig {
    Global,
    Local,
}

#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub scope: Option<AgentScopeConfig>,
    pub mode: Option<SearchMode>,
    #[serde(default, deserialize_with = "deserialize_nonzero_option")]
    pub output_chars: Option<usize>,
    #[serde(default, deserialize_with = "deserialize_nonzero_option")]
    pub top: Option<usize>,
    #[serde(default, deserialize_with = "deserialize_nonzero_option")]
    pub within_top: Option<usize>,
    #[serde(default, deserialize_with = "deserialize_nonzero_option")]
    pub hits_per_conversation: Option<usize>,
    #[serde(default)]
    pub exclude_projects: Vec<String>,
    pub tools: Option<bool>,
    pub tool_results: Option<bool>,
    pub thinking: Option<bool>,
    pub subagents: Option<bool>,
}

fn deserialize_nonzero_option<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<usize>::deserialize(deserializer)?;
    if value == Some(0) {
        return Err(serde::de::Error::custom("value must be greater than zero"));
    }
    Ok(value)
}

#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct TuiConfig {
    #[serde(default)]
    pub exclude_projects: Vec<String>,
    pub semantic_search: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_without_tui_defaults_to_no_excluded_projects() {
        let config: ConfigFile = toml::from_str("").unwrap();
        assert!(config.tui.unwrap_or_default().exclude_projects.is_empty());
    }

    #[test]
    fn empty_tui_table_defaults_to_no_excluded_projects() {
        let config: ConfigFile = toml::from_str("[tui]\n").unwrap();
        assert!(config.tui.unwrap_or_default().exclude_projects.is_empty());
    }

    #[test]
    fn tui_exclude_projects_preserves_exact_strings() {
        let config: ConfigFile = toml::from_str(
            r#"
[tui]
exclude_projects = ["Hidden", "hidden", " spaced "]
"#,
        )
        .unwrap();

        assert_eq!(
            config.tui.unwrap().exclude_projects,
            vec!["Hidden", "hidden", " spaced "]
        );
    }

    #[test]
    fn tui_semantic_search_parses_default_mode() {
        let config: ConfigFile = toml::from_str(
            r#"
[tui]
semantic_search = true
"#,
        )
        .unwrap();

        assert_eq!(config.tui.unwrap().semantic_search, Some(true));
    }

    #[test]
    fn search_mode_parses_hybrid_mode() {
        let config: ConfigFile = toml::from_str(
            r#"
[search]
mode = "hybrid"
"#,
        )
        .unwrap();

        assert_eq!(config.search.unwrap().mode, Some(SearchMode::Hybrid));
    }

    #[test]
    fn search_mode_rejects_unknown_mode() {
        let err = toml::from_str::<ConfigFile>(
            r#"
[search]
mode = "vector"
"#,
        )
        .expect_err("unknown search mode should fail");

        assert!(err.to_string().contains("unknown variant"));
    }

    #[test]
    fn agent_settings_parse_independently_from_tui() {
        let config: ConfigFile = toml::from_str(
            r#"
[search]
mode = "lexical"

[tui]
semantic_search = true
exclude_projects = ["tui-only"]

[agent]
scope = "local"
mode = "hybrid"
output_chars = 9000
top = 12
within_top = 24
hits_per_conversation = 3
exclude_projects = ["agent-only"]
tools = true
tool_results = true
thinking = true
subagents = true
"#,
        )
        .unwrap();

        let agent = config.agent.unwrap();
        assert_eq!(agent.scope, Some(AgentScopeConfig::Local));
        assert_eq!(agent.mode, Some(SearchMode::Hybrid));
        assert_eq!(agent.output_chars, Some(9000));
        assert_eq!(agent.top, Some(12));
        assert_eq!(agent.within_top, Some(24));
        assert_eq!(agent.hits_per_conversation, Some(3));
        assert_eq!(agent.exclude_projects, vec!["agent-only"]);
        assert_eq!(agent.tools, Some(true));
        assert_eq!(agent.tool_results, Some(true));
        assert_eq!(agent.thinking, Some(true));
        assert_eq!(agent.subagents, Some(true));
    }

    #[test]
    fn agent_counts_reject_zero() {
        let error = toml::from_str::<ConfigFile>("[agent]\noutput_chars = 0\n")
            .expect_err("zero output budget should fail");

        assert!(error.to_string().contains("greater than zero"));
    }

    #[test]
    fn parses_function_key_binding() {
        let binding = parse_key_binding("f2").unwrap();
        assert_eq!(binding.code, KeyCode::F(2));
        assert_eq!(binding.modifiers, KeyModifiers::NONE);
        assert_eq!(binding.short_label(), "F2");
        assert_eq!(binding.help_label(), "F2");
    }

    #[test]
    fn applies_rename_key_config() {
        let keys = KeyBindings::from_config(Some(KeysConfig {
            rename: Some(parse_key_binding("alt+r").unwrap()),
            ..Default::default()
        }));

        assert!(keys.rename.matches(KeyCode::Char('r'), KeyModifiers::ALT));
    }
}

#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct FilterConfig {
    /// Exclude ICM persistent-memory background sessions from loading. Default: true.
    pub exclude_icm: Option<bool>,
    /// Additional first-line substrings; sessions whose head contains any are excluded.
    pub exclude_markers: Option<Vec<String>>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct DisplayConfig {
    pub no_tools: Option<bool>,
    pub last: Option<bool>,
    /// Deprecated: timestamps now always use hybrid relative/absolute format.
    /// Kept for backwards compatibility with existing config files.
    #[allow(dead_code)]
    pub relative_time: Option<bool>,
    pub show_thinking: Option<bool>,
    pub plain: Option<bool>,
    pub pager: Option<bool>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct ResumeConfig {
    pub default_args: Option<Vec<String>>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct KeysConfig {
    pub resume: Option<KeyBinding>,
    pub fork: Option<KeyBinding>,
    pub rename: Option<KeyBinding>,
    pub delete: Option<KeyBinding>,
}

#[derive(Debug, Clone, Copy)]
pub struct KeyBinding {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyBinding {
    pub fn matches(&self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        self.code == code && self.modifiers == modifiers
    }

    /// Format for status bar display (e.g. "^F", "M-F", "F2")
    pub fn short_label(&self) -> String {
        self.format_label("^", "M-")
    }

    /// Format for help overlay (e.g. "Ctrl+F", "Alt+F", "F2")
    pub fn help_label(&self) -> String {
        self.format_label("Ctrl+", "Alt+")
    }

    fn format_label(&self, ctrl_prefix: &str, alt_prefix: &str) -> String {
        let prefix = if self.modifiers.contains(KeyModifiers::CONTROL) {
            ctrl_prefix
        } else if self.modifiers.contains(KeyModifiers::ALT) {
            alt_prefix
        } else {
            ""
        };
        match self.code {
            KeyCode::Char(c) => format!("{}{}", prefix, c.to_ascii_uppercase()),
            KeyCode::F(n) if self.modifiers.is_empty() => format!("F{}", n),
            _ => String::new(),
        }
    }
}

impl<'de> Deserialize<'de> for KeyBinding {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        parse_key_binding(&s).map_err(serde::de::Error::custom)
    }
}

fn parse_key_code(key: &str) -> std::result::Result<KeyCode, String> {
    match key.to_lowercase().as_str() {
        k if k.len() == 1 => Ok(KeyCode::Char(k.chars().next().unwrap())),
        k if k.starts_with('f') => {
            let number = k[1..]
                .parse::<u8>()
                .map_err(|_| format!("Unknown key: {key}"))?;
            if (1..=12).contains(&number) {
                Ok(KeyCode::F(number))
            } else {
                Err(format!("Unknown key: {key}"))
            }
        }
        _ => Err(format!("Unknown key: {key}")),
    }
}

fn parse_key_binding(s: &str) -> std::result::Result<KeyBinding, String> {
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    match parts.as_slice() {
        [modifier, key] => {
            let modifiers = match modifier.to_lowercase().as_str() {
                "ctrl" | "control" => KeyModifiers::CONTROL,
                "alt" | "meta" => KeyModifiers::ALT,
                _ => return Err(format!("Unknown modifier: {modifier}")),
            };
            let code = parse_key_code(key)?;
            if matches!(code, KeyCode::F(_)) {
                return Err(format!("Function keys do not support modifiers: {s}"));
            }
            Ok(KeyBinding { code, modifiers })
        }
        [key] => {
            let code = parse_key_code(key)?;
            Ok(KeyBinding {
                code,
                modifiers: KeyModifiers::NONE,
            })
        }
        _ => Err(format!("Invalid key binding: {s}")),
    }
}

/// Resolved keybindings with defaults applied
#[derive(Debug, Clone, Copy)]
pub struct KeyBindings {
    pub resume: KeyBinding,
    pub fork: KeyBinding,
    pub rename: KeyBinding,
    pub delete: KeyBinding,
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self {
            resume: KeyBinding {
                code: KeyCode::Char('r'),
                modifiers: KeyModifiers::CONTROL,
            },
            fork: KeyBinding {
                code: KeyCode::Char('f'),
                modifiers: KeyModifiers::CONTROL,
            },
            rename: KeyBinding {
                code: KeyCode::F(2),
                modifiers: KeyModifiers::NONE,
            },
            delete: KeyBinding {
                code: KeyCode::Char('x'),
                modifiers: KeyModifiers::CONTROL,
            },
        }
    }
}

impl KeyBindings {
    pub fn from_config(config: Option<KeysConfig>) -> Self {
        let defaults = Self::default();
        match config {
            None => defaults,
            Some(cfg) => Self {
                resume: cfg.resume.unwrap_or(defaults.resume),
                fork: cfg.fork.unwrap_or(defaults.fork),
                rename: cfg.rename.unwrap_or(defaults.rename),
                delete: cfg.delete.unwrap_or(defaults.delete),
            },
        }
    }
}

/// Returns the path to the configuration file: ~/.config/claude-history/config.toml
/// This path is used for all platforms.
fn get_config_path() -> Option<PathBuf> {
    home::home_dir().map(|mut path| {
        path.push(".config");
        path.push("claude-history");
        path.push("config.toml");
        path
    })
}

/// Loads the configuration from the config file.
///
/// Returns a default `ConfigFile` if the file or home directory doesn't exist.
/// Returns an error if the file exists but cannot be read or parsed.
pub fn load_config() -> Result<ConfigFile> {
    let config_path = match get_config_path() {
        Some(path) => path,
        None => return Ok(ConfigFile::default()), // No home dir, so no config.
    };

    if !config_path.exists() {
        return Ok(ConfigFile::default()); // Config is optional.
    }

    let content = fs::read_to_string(&config_path).map_err(|e| {
        AppError::ConfigError(format!(
            "Failed to read config file at '{}': {}",
            config_path.display(),
            e
        ))
    })?;

    toml::from_str(&content).map_err(|e| {
        AppError::ConfigError(format!(
            "Failed to parse config file at '{}': {}",
            config_path.display(),
            e
        ))
    })
}
