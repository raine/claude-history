use crate::error::{AppError, Result};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

/// Defines the structure of the config.toml file.
/// Using `Option` allows distinguishing between a value being unset
/// vs. explicitly set to `false`.
#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    pub global: Option<bool>,
    pub display: Option<DisplayConfig>,
    pub resume: Option<ResumeConfig>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct DisplayConfig {
    pub no_tools: Option<bool>,
    pub last: Option<bool>,
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

/// Returns the path to the configuration file: ~/.config/mnemonai/config.toml
/// Also checks the legacy path ~/.config/claude-history/config.toml for backwards compatibility.
fn get_config_path() -> Option<PathBuf> {
    home::home_dir().map(|home| {
        let new_path = home.join(".config").join("mnemonai").join("config.toml");
        if new_path.exists() {
            return new_path;
        }
        // Fall back to legacy path
        let legacy_path = home.join(".config").join("claude-history").join("config.toml");
        if legacy_path.exists() {
            return legacy_path;
        }
        // Default to new path (even if it doesn't exist yet)
        new_path
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
