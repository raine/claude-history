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
