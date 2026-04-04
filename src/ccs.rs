//! CCS (Claude Code Switch) integration.
//!
//! Discovers CCS configuration and enumerates instance project roots
//! for unified conversation loading across all CCS profiles.

use crate::error::{AppError, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Minimal representation of ~/.ccs/config.yaml (only fields we need)
#[derive(Deserialize)]
struct CcsConfigFile {
    default: Option<String>,
    #[serde(default)]
    accounts: HashMap<String, CcsAccount>,
}

#[derive(Deserialize)]
struct CcsAccount {
    #[allow(dead_code)]
    context_mode: Option<String>,
    #[allow(dead_code)]
    context_group: Option<String>,
}

/// A resolved CCS profile with its config dir and projects root
#[derive(Clone, Debug)]
pub struct CcsProfile {
    pub name: String,
    /// Instance config directory, e.g. ~/.ccs/instances/team
    pub config_dir: PathBuf,
    /// Canonical (symlink-resolved) projects root
    pub canonical_root: PathBuf,
}

/// CCS discovery result
#[derive(Clone, Debug)]
pub struct CcsInfo {
    pub default_profile: Option<String>,
    pub profiles: Vec<CcsProfile>,
}

impl CcsInfo {
    /// Get the config_dir for a profile by name
    pub fn profile_config_dir(&self, name: &str) -> Option<&PathBuf> {
        self.profiles
            .iter()
            .find(|p| p.name == name)
            .map(|p| &p.config_dir)
    }

    /// Build a mapping from session UUID to CCS profile name(s).
    ///
    /// Scans each instance's `session-env/` directory for UUID-named subdirectories,
    /// which correspond to sessions that were run under that profile.
    /// A session used across multiple profiles will have all profile names joined.
    pub fn build_session_profile_map(&self) -> HashMap<String, String> {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for profile in &self.profiles {
            let session_env_dir = profile.config_dir.join("session-env");
            let Ok(entries) = std::fs::read_dir(&session_env_dir) else {
                continue;
            };
            for entry in entries {
                let Ok(entry) = entry else { continue };
                if !entry.path().is_dir() {
                    continue;
                }
                if let Some(name) = entry.file_name().to_str()
                    && name.contains('-')
                    && name.len() > 30
                {
                    map.entry(name.to_string())
                        .or_default()
                        .push(profile.name.clone());
                }
            }
        }
        map.into_iter()
            .map(|(uuid, profiles)| (uuid, profiles.join(",")))
            .collect()
    }
}

/// A unique project root to scan for conversations
#[derive(Clone, Debug)]
pub struct ProjectRoot {
    /// Canonical filesystem path
    pub path: PathBuf,
    /// Key for cache namespacing
    pub cache_key: String,
}

/// Attempt to discover CCS configuration. Returns None if CCS is not installed.
pub fn discover_ccs() -> Option<CcsInfo> {
    let home = home::home_dir()?;
    let config_path = home.join(".ccs").join("config.yaml");
    if !config_path.exists() {
        return None;
    }

    let content = std::fs::read_to_string(&config_path).ok()?;
    let config: CcsConfigFile = serde_yaml::from_str(&content).ok()?;

    let instances_dir = home.join(".ccs").join("instances");
    let mut profiles = Vec::new();

    for name in config.accounts.keys() {
        let instance_dir = instances_dir.join(name);
        let projects_dir = instance_dir.join("projects");

        if !projects_dir.exists() {
            continue;
        }

        let canonical_root = std::fs::canonicalize(&projects_dir).unwrap_or(projects_dir);

        profiles.push(CcsProfile {
            name: name.clone(),
            config_dir: instance_dir,
            canonical_root,
        });
    }

    // Sort profiles so the default one comes first
    if let Some(ref default_name) = config.default {
        profiles.sort_by(|a, b| {
            let a_is_default = &a.name == default_name;
            let b_is_default = &b.name == default_name;
            b_is_default.cmp(&a_is_default)
        });
    }

    if profiles.is_empty() {
        return None;
    }

    Some(CcsInfo {
        default_profile: config.default,
        profiles,
    })
}

/// Get all unique project roots to scan for conversations.
///
/// With CCS: returns deduplicated roots from all CCS instances.
/// Without CCS: returns the single default root.
pub fn get_all_project_roots(ccs_info: Option<&CcsInfo>) -> Result<Vec<ProjectRoot>> {
    if let Some(info) = ccs_info {
        let mut seen = HashSet::new();
        let mut roots = Vec::new();

        for profile in &info.profiles {
            if seen.insert(profile.canonical_root.clone()) {
                let cache_key = make_cache_key(&profile.canonical_root);
                roots.push(ProjectRoot {
                    path: profile.canonical_root.clone(),
                    cache_key,
                });
            }
        }

        // Also include the default ~/.claude/projects if it exists and isn't already covered
        if let Some(default_root) = default_projects_root()
            && default_root.exists()
        {
            let canonical = std::fs::canonicalize(&default_root).unwrap_or(default_root.clone());
            if seen.insert(canonical.clone()) {
                roots.push(ProjectRoot {
                    path: canonical,
                    cache_key: "default".to_string(),
                });
            }
        }

        if roots.is_empty() {
            return Err(AppError::ProjectsDirNotFound(
                "No CCS project roots found".to_string(),
            ));
        }

        Ok(roots)
    } else {
        // No CCS — fall back to default root
        let root = crate::history::get_claude_projects_root()?;
        Ok(vec![ProjectRoot {
            path: root,
            cache_key: "default".to_string(),
        }])
    }
}

/// Get the default ~/.claude/projects path (ignoring CLAUDE_CONFIG_DIR)
fn default_projects_root() -> Option<PathBuf> {
    home::home_dir().map(|h| h.join(".claude").join("projects"))
}

/// Generate a cache key from a canonical root path
fn make_cache_key(path: &std::path::Path) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    path.to_string_lossy().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}
