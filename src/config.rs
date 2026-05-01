use crate::error::{AppError, Result};
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

    /// Format for status bar display (e.g. "^F", "M-F")
    pub fn short_label(&self) -> String {
        let prefix = if self.modifiers.contains(KeyModifiers::CONTROL) {
            "^"
        } else if self.modifiers.contains(KeyModifiers::ALT) {
            "M-"
        } else {
            ""
        };
        match self.code {
            KeyCode::Char(c) => format!("{}{}", prefix, c.to_ascii_uppercase()),
            _ => String::new(),
        }
    }

    /// Format for help overlay (e.g. "Ctrl+F", "Alt+F")
    pub fn help_label(&self) -> String {
        let prefix = if self.modifiers.contains(KeyModifiers::CONTROL) {
            "Ctrl+"
        } else if self.modifiers.contains(KeyModifiers::ALT) {
            "Alt+"
        } else {
            ""
        };
        match self.code {
            KeyCode::Char(c) => format!("{}{}", prefix, c.to_ascii_uppercase()),
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

fn parse_key_binding(s: &str) -> std::result::Result<KeyBinding, String> {
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    match parts.as_slice() {
        [modifier, key] => {
            let modifiers = match modifier.to_lowercase().as_str() {
                "ctrl" | "control" => KeyModifiers::CONTROL,
                "alt" | "meta" => KeyModifiers::ALT,
                _ => return Err(format!("Unknown modifier: {modifier}")),
            };
            let code = match key.to_lowercase().as_str() {
                k if k.len() == 1 => KeyCode::Char(k.chars().next().unwrap()),
                _ => return Err(format!("Unknown key: {key}")),
            };
            Ok(KeyBinding { code, modifiers })
        }
        [key] => {
            let code = match key.to_lowercase().as_str() {
                k if k.len() == 1 => KeyCode::Char(k.chars().next().unwrap()),
                _ => return Err(format!("Unknown key: {key}")),
            };
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
