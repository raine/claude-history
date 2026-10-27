use crate::error::{AppError, Result};
use crate::history::actions::{Launch, Launcher};
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
    pub semantic: Option<SemanticConfig>,
    pub safety: Option<SafetyConfig>,
}

#[derive(Deserialize, Debug, Default, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticConfig {
    /// Keep the caches warm in a background process that any launch starts.
    pub daemon: Option<bool>,
    /// Whether the background process pauses embedding on battery power.
    pub pause_on_battery: Option<bool>,
}

impl SemanticConfig {
    pub fn daemon(&self) -> bool {
        self.daemon.unwrap_or(false)
    }

    pub fn pause_on_battery(&self) -> bool {
        self.pause_on_battery.unwrap_or(true)
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
    #[serde(default)]
    pub exclude_paths: Vec<String>,
    pub semantic_search: Option<bool>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct SafetyConfig {
    /// Refuse every delete path: the TUI delete key, `--delete`, and `delete-empty --yes`
    #[serde(default)]
    pub disable_delete: bool,
}

impl SafetyConfig {
    pub fn ensure_delete_allowed(&self) -> Result<()> {
        if self.disable_delete {
            Err(AppError::DeleteDisabled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_is_allowed_without_safety_table() {
        let config: ConfigFile = toml::from_str("").unwrap();
        assert!(
            config
                .safety
                .unwrap_or_default()
                .ensure_delete_allowed()
                .is_ok()
        );
    }

    #[test]
    fn safety_disable_delete_refuses_delete() {
        let config: ConfigFile = toml::from_str(
            r#"
[safety]
disable_delete = true
"#,
        )
        .unwrap();

        let error = config.safety.unwrap().ensure_delete_allowed().unwrap_err();
        assert!(matches!(error, AppError::DeleteDisabled));
    }

    #[test]
    fn safety_rejects_misspelled_options() {
        // A typo must fail loudly rather than silently leave deletion enabled.
        let error = toml::from_str::<ConfigFile>("[safety]\ndisable_deletes = true\n")
            .expect_err("unknown safety option should fail");

        assert!(error.to_string().contains("unknown field"));
    }

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
    fn tui_exclude_paths_defaults_and_preserves_literal_prefixes() {
        for input in ["", "[tui]", "[tui]\nexclude_paths = []"] {
            let config: ConfigFile = toml::from_str(input).unwrap();
            assert!(config.tui.unwrap_or_default().exclude_paths.is_empty());
        }
        let config: ConfigFile = toml::from_str(
            r#"[tui]
exclude_paths = ["/tmp/", "/private/tmp/", "", " spaced "]
"#,
        )
        .unwrap();
        assert_eq!(
            config.tui.unwrap().exclude_paths,
            ["/tmp/", "/private/tmp/", "", " spaced "]
        );
        assert!(toml::from_str::<ConfigFile>("[tui]\nexclude_paths = [1]").is_err());
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
    fn semantic_daemon_is_off_and_pauses_on_battery_by_default() {
        let config: ConfigFile = toml::from_str("").unwrap();
        let semantic = config.semantic.unwrap_or_default();
        assert!(!semantic.daemon());
        assert!(semantic.pause_on_battery());

        let config: ConfigFile =
            toml::from_str("[semantic]\ndaemon = true\npause_on_battery = false\n").unwrap();
        let semantic = config.semantic.unwrap();
        assert!(semantic.daemon());
        assert!(!semantic.pause_on_battery());
        assert!(toml::from_str::<ConfigFile>("[semantic]\ndeamon = true\n").is_err());
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
    fn codex_resume_args_are_separate_from_claude_ones() {
        let config: ConfigFile = toml::from_str(
            r#"
[resume]
default_args = ["--dangerously-skip-permissions"]

[resume.codex]
default_args = ["--yolo"]
"#,
        )
        .unwrap();
        let resume = config.resume.unwrap();
        use crate::history::model::SourceKind;
        assert_eq!(
            resume.default_args(SourceKind::Claude),
            ["--dangerously-skip-permissions"]
        );
        assert_eq!(resume.default_args(SourceKind::Codex), ["--yolo"]);
        assert!(resume.default_args(SourceKind::Pi).is_empty());
        assert!(
            ResumeConfig::default()
                .default_args(SourceKind::Codex)
                .is_empty()
        );
    }

    #[test]
    fn agent_commands_default_to_the_agent_binaries() {
        use crate::history::model::SourceKind;
        let resume = ResumeConfig::default();
        for source in [
            SourceKind::Claude,
            SourceKind::Pi,
            SourceKind::Omp,
            SourceKind::Codex,
        ] {
            assert_eq!(resume.launcher(source), Launcher::default_for(source));
        }
    }

    #[test]
    fn agent_commands_are_configured_per_agent() {
        use crate::history::model::SourceKind;
        let config: ConfigFile = toml::from_str(
            r#"
[resume]
command = "my claude"
default_args = ["--verbose"]

[resume.pi]
command = ["mise", "exec", "--", "pi"]

[resume.omp]
command = "/opt/omp/bin/omp"

[resume.codex]
command = ["codex-wrapper", "--profile", "work"]
default_args = ["--yolo"]
"#,
        )
        .unwrap();
        let resume = config.resume.unwrap();

        let claude = resume.launch(SourceKind::Claude);
        assert_eq!(claude.launcher.program, PathBuf::from("my claude"));
        assert!(claude.launcher.args.is_empty());
        assert_eq!(claude.launcher.configured_at, Some("[resume]"));
        assert_eq!(claude.default_args, ["--verbose"]);

        let pi = resume.launch(SourceKind::Pi);
        assert_eq!(pi.launcher.program, PathBuf::from("mise"));
        assert_eq!(pi.launcher.args, ["exec", "--", "pi"]);
        assert_eq!(pi.launcher.configured_at, Some("[resume.pi]"));
        assert!(pi.default_args.is_empty());

        let omp = resume.launcher(SourceKind::Omp);
        assert_eq!(omp.program, PathBuf::from("/opt/omp/bin/omp"));
        assert_eq!(omp.configured_at, Some("[resume.omp]"));

        let codex = resume.launch(SourceKind::Codex);
        assert_eq!(codex.launcher.program, PathBuf::from("codex-wrapper"));
        assert_eq!(codex.launcher.args, ["--profile", "work"]);
        assert_eq!(codex.default_args, ["--yolo"]);
    }

    #[test]
    fn agent_commands_must_name_a_program() {
        for command in [r#""""#, "[]", r#"[" "]"#, "1"] {
            let error =
                toml::from_str::<ConfigFile>(&format!("[resume.pi]\ncommand = {command}\n"))
                    .expect_err(command);
            assert!(error.to_string().contains("command must"), "{error}");
        }
        assert!(toml::from_str::<ConfigFile>("[resume.pi]\ndefault_args = []\n").is_err());
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

    #[test]
    fn applies_refresh_key_config() {
        let keys = KeyBindings::from_config(Some(KeysConfig {
            refresh: Some(parse_key_binding("alt+u").unwrap()),
            ..Default::default()
        }));

        assert!(keys.refresh.matches(KeyCode::Char('u'), KeyModifiers::ALT));
    }
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
    /// The command Claude Code is started with.
    pub command: Option<AgentCommand>,
    /// Arguments Claude Code is resumed with.
    pub default_args: Option<Vec<String>>,
    pub pi: Option<LauncherConfig>,
    pub omp: Option<LauncherConfig>,
    pub codex: Option<AgentResumeConfig>,
}

/// How the conversations of another agent are resumed.
#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct AgentResumeConfig {
    /// The command the agent is started with.
    pub command: Option<AgentCommand>,
    /// Arguments the agent is resumed with, before its own subcommand.
    pub default_args: Option<Vec<String>>,
}

/// The command an agent is started with.
#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct LauncherConfig {
    pub command: Option<AgentCommand>,
}

/// A program and the arguments that precede the agent's own: a string
/// names the program alone (never split on whitespace), an array the
/// program and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCommand(Vec<String>);

impl<'de> Deserialize<'de> for AgentCommand {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Program(String),
            Command(Vec<String>),
        }
        let command = match Raw::deserialize(deserializer).map_err(|_| {
            serde::de::Error::custom(
                "command must be a program (\"my-claude\") or an array of a program and its arguments ([\"mise\", \"exec\", \"--\", \"claude\"])",
            )
        })? {
            Raw::Program(program) => vec![program],
            Raw::Command(command) => command,
        };
        if command
            .first()
            .is_none_or(|program| program.trim().is_empty())
        {
            return Err(serde::de::Error::custom("command must name a program"));
        }
        Ok(Self(command))
    }
}

impl ResumeConfig {
    /// The configured arguments for resuming a conversation of `source`.
    pub fn default_args(&self, source: crate::history::model::SourceKind) -> &[String] {
        use crate::history::model::SourceKind;
        let args = match source {
            SourceKind::Claude => self.default_args.as_ref(),
            SourceKind::Codex => self
                .codex
                .as_ref()
                .and_then(|codex| codex.default_args.as_ref()),
            SourceKind::Pi | SourceKind::Omp => None,
        };
        args.map(Vec::as_slice).unwrap_or(&[])
    }

    /// The program that starts the agent of `source`: the configured
    /// command, else the agent's own binary.
    pub fn launcher(&self, source: crate::history::model::SourceKind) -> Launcher {
        use crate::history::model::SourceKind;
        let command = match source {
            SourceKind::Claude => self.command.as_ref(),
            SourceKind::Pi => self.pi.as_ref().and_then(|pi| pi.command.as_ref()),
            SourceKind::Omp => self.omp.as_ref().and_then(|omp| omp.command.as_ref()),
            SourceKind::Codex => self.codex.as_ref().and_then(|codex| codex.command.as_ref()),
        };
        match command {
            Some(AgentCommand(command)) => {
                Launcher::configured(command, crate::history::actions::config_table(source))
            }
            None => Launcher::default_for(source),
        }
    }

    /// How to start the agent of `source` to resume or fork one of its
    /// conversations.
    pub fn launch(&self, source: crate::history::model::SourceKind) -> Launch {
        Launch {
            launcher: self.launcher(source),
            default_args: self.default_args(source).to_vec(),
        }
    }
}

#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct KeysConfig {
    pub resume: Option<KeyBinding>,
    pub fork: Option<KeyBinding>,
    pub rename: Option<KeyBinding>,
    pub delete: Option<KeyBinding>,
    pub refresh: Option<KeyBinding>,
    pub all_kinds: Option<KeyBinding>,
    pub user_only: Option<KeyBinding>,
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
    pub refresh: KeyBinding,
    /// Lists or hides automated and child conversations
    pub all_kinds: KeyBinding,
    /// Searches only the user's own messages, or everything
    pub user_only: KeyBinding,
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
            refresh: KeyBinding {
                code: KeyCode::Char('l'),
                modifiers: KeyModifiers::CONTROL,
            },
            all_kinds: KeyBinding {
                code: KeyCode::Char('g'),
                modifiers: KeyModifiers::CONTROL,
            },
            user_only: KeyBinding {
                code: KeyCode::Char('v'),
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
                refresh: cfg.refresh.unwrap_or(defaults.refresh),
                all_kinds: cfg.all_kinds.unwrap_or(defaults.all_kinds),
                user_only: cfg.user_only.unwrap_or(defaults.user_only),
            },
        }
    }
}

/// Returns the path to the configuration file: ~/.config/claude-history/config.toml
/// This path is used for all platforms.
pub fn get_config_path() -> Option<PathBuf> {
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
