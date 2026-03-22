use crate::error::{AppError, Result};
use std::path::Path;
use std::process::Command;

const REPO: &str = "raine/claude-history";
const BIN_NAME: &str = "claude-history";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Map OS/arch to the release artifact suffix used in GitHub releases.
fn platform_suffix() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("darwin-arm64"),
        ("macos", "x86_64") => Ok("darwin-amd64"),
        ("linux", "x86_64") => Ok("linux-amd64"),
        (os, arch) => Err(AppError::UpdateError(format!(
            "Unsupported platform: {os}/{arch}"
        ))),
    }
}

/// Check if the binary is managed by Homebrew.
fn is_homebrew_install(exe_path: &Path) -> bool {
    let path_str = exe_path.to_string_lossy();
    path_str.contains("/Cellar/")
}

/// Fetch the latest release tag from GitHub API using curl.
fn fetch_latest_version() -> Result<String> {
    let output = Command::new("curl")
        .args([
            "-sSf",
            "--connect-timeout",
            "10",
            "--max-time",
            "30",
            &format!("https://api.github.com/repos/{REPO}/releases/latest"),
        ])
        .output()
        .map_err(|e| AppError::UpdateError(format!("Failed to run curl: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::UpdateError(format!(
            "Failed to fetch latest release: {}",
            stderr.trim()
        )));
    }

    let body: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| AppError::UpdateError(format!("Failed to parse GitHub API response: {e}")))?;

    let tag = body["tag_name"]
        .as_str()
        .ok_or_else(|| AppError::UpdateError("No tag_name in GitHub API response".to_string()))?;

    Ok(tag.strip_prefix('v').unwrap_or(tag).to_string())
}

/// Download a URL to a file path using curl.
fn download(url: &str, dest: &Path) -> Result<()> {
    let status = Command::new("curl")
        .args([
            "-sSLf",
            "--connect-timeout",
            "10",
            "--max-time",
            "120",
            "-o",
        ])
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|e| AppError::UpdateError(format!("Failed to run curl: {e}")))?;

    if !status.success() {
        return Err(AppError::UpdateError(format!("Download failed: {url}")));
    }
    Ok(())
}

/// Extract a tar.gz archive into a directory.
fn extract_tar(archive: &Path, dest: &Path) -> Result<()> {
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .status()
        .map_err(|e| AppError::UpdateError(format!("Failed to run tar: {e}")))?;

    if !status.success() {
        return Err(AppError::UpdateError(
            "Failed to extract archive".to_string(),
        ));
    }
    Ok(())
}

/// Compute SHA-256 hash of a file using system tools.
fn sha256_of(path: &Path) -> Result<String> {
    // Try sha256sum first (common on Linux)
    if let Ok(output) = Command::new("sha256sum").arg(path).output()
        && output.status.success()
    {
        let out = String::from_utf8_lossy(&output.stdout);
        if let Some(hash) = out.split_whitespace().next() {
            return Ok(hash.to_string());
        }
    }

    // Fall back to shasum -a 256 (macOS)
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .map_err(|e| {
            AppError::UpdateError(format!(
                "Neither sha256sum nor shasum found. Cannot verify checksum: {e}"
            ))
        })?;

    if !output.status.success() {
        return Err(AppError::UpdateError("Checksum command failed".to_string()));
    }

    let out = String::from_utf8_lossy(&output.stdout);
    out.split_whitespace()
        .next()
        .map(|s| s.to_string())
        .ok_or_else(|| AppError::UpdateError("Could not parse checksum output".to_string()))
}

/// Verify SHA-256 checksum of a file against the expected checksum line.
fn verify_checksum(file: &Path, expected_line: &str) -> Result<()> {
    let expected_hash = expected_line
        .split_whitespace()
        .next()
        .ok_or_else(|| AppError::UpdateError("Invalid checksum file format".to_string()))?;

    let actual_hash = sha256_of(file)?;
    if actual_hash != expected_hash {
        return Err(AppError::UpdateError(format!(
            "Checksum mismatch!\n  Expected: {expected_hash}\n  Got:      {actual_hash}"
        )));
    }
    Ok(())
}

/// Replace the current binary with the new one, with rollback on failure.
fn replace_binary(new_binary: &Path, current_exe: &Path) -> Result<()> {
    let exe_dir = current_exe
        .parent()
        .ok_or_else(|| AppError::UpdateError("Could not determine binary directory".to_string()))?;

    // Copy to destination directory to avoid EXDEV (cross-device rename)
    let staged = exe_dir.join(format!(".{BIN_NAME}.new"));
    std::fs::copy(new_binary, &staged)
        .map_err(|e| AppError::UpdateError(format!("Failed to copy new binary: {e}")))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| AppError::UpdateError(format!("Failed to set permissions: {e}")))?;
    }

    // Rename current -> .old, then staged -> current
    let backup = exe_dir.join(format!(".{BIN_NAME}.old"));
    std::fs::rename(current_exe, &backup)
        .map_err(|e| AppError::UpdateError(format!("Failed to move current binary aside: {e}")))?;

    if let Err(e) = std::fs::rename(&staged, current_exe) {
        // Rollback: restore the original
        let _ = std::fs::rename(&backup, current_exe);
        return Err(AppError::UpdateError(format!(
            "Failed to install new binary (rolled back): {e}"
        )));
    }

    // Cleanup
    let _ = std::fs::remove_file(&backup);
    Ok(())
}

fn do_update(
    pb: &indicatif::ProgressBar,
    artifact_name: &str,
    current_exe: &Path,
) -> Result<String> {
    let latest_version = fetch_latest_version()?;

    if latest_version == CURRENT_VERSION {
        return Ok(format!("Already up to date (v{CURRENT_VERSION})"));
    }

    pb.set_message(format!("Downloading v{latest_version}..."));

    let tmp = tempfile::tempdir()
        .map_err(|e| AppError::UpdateError(format!("Failed to create temp directory: {e}")))?;
    let tar_path = tmp.path().join(format!("{artifact_name}.tar.gz"));
    let sha_path = tmp.path().join(format!("{artifact_name}.sha256"));

    let base_url = format!("https://github.com/{REPO}/releases/download/v{latest_version}");

    download(&format!("{base_url}/{artifact_name}.tar.gz"), &tar_path)?;
    download(&format!("{base_url}/{artifact_name}.sha256"), &sha_path)?;

    pb.set_message("Verifying checksum...");
    let sha_content = std::fs::read_to_string(&sha_path)
        .map_err(|e| AppError::UpdateError(format!("Failed to read checksum file: {e}")))?;
    verify_checksum(&tar_path, &sha_content)?;

    pb.set_message("Installing...");
    let extract_dir = tmp.path().join("extract");
    std::fs::create_dir(&extract_dir)
        .map_err(|e| AppError::UpdateError(format!("Failed to create extract dir: {e}")))?;
    extract_tar(&tar_path, &extract_dir)?;

    let new_binary = extract_dir.join(BIN_NAME);
    if !new_binary.exists() {
        return Err(AppError::UpdateError(format!(
            "Extracted archive does not contain '{BIN_NAME}' binary"
        )));
    }

    replace_binary(&new_binary, current_exe)?;

    Ok(format!(
        "Updated {BIN_NAME} v{CURRENT_VERSION} -> v{latest_version}"
    ))
}

pub fn run() -> Result<()> {
    let current_exe = std::env::current_exe()
        .map_err(|e| AppError::UpdateError(format!("Could not determine executable path: {e}")))?;

    // Guard: Homebrew-managed installs (canonicalize to resolve symlinks)
    let canonical_exe = std::fs::canonicalize(&current_exe).unwrap_or(current_exe.clone());
    if is_homebrew_install(&canonical_exe) {
        return Err(AppError::UpdateError(
            "claude-history is managed by Homebrew. Run `brew upgrade claude-history` instead."
                .to_string(),
        ));
    }

    let platform = platform_suffix()?;
    let artifact_name = format!("{BIN_NAME}-{platform}");

    let pb = indicatif::ProgressBar::new_spinner();
    pb.set_style(
        indicatif::ProgressStyle::default_spinner()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
            .template("{spinner:.blue} {msg}")
            .unwrap(),
    );
    pb.enable_steady_tick(std::time::Duration::from_millis(120));
    pb.set_message("Checking for updates...");

    match do_update(&pb, &artifact_name, &canonical_exe) {
        Ok(msg) => {
            pb.finish_with_message(format!("✔ {msg}"));
            Ok(())
        }
        Err(e) => {
            pb.finish_with_message("✘ Update failed".to_string());
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_suffix_current() {
        let suffix = platform_suffix().unwrap();
        assert!(["darwin-arm64", "darwin-amd64", "linux-amd64"].contains(&suffix));
    }

    #[test]
    fn test_is_homebrew_cellar() {
        assert!(is_homebrew_install(Path::new(
            "/opt/homebrew/Cellar/claude-history/0.1.42/bin/claude-history"
        )));
    }

    #[test]
    fn test_is_homebrew_prefix() {
        assert!(is_homebrew_install(Path::new(
            "/usr/local/Cellar/claude-history/0.1.42/bin/claude-history"
        )));
    }

    #[test]
    fn test_is_not_homebrew_local_bin() {
        assert!(!is_homebrew_install(Path::new(
            "/usr/local/bin/claude-history"
        )));
    }

    #[test]
    fn test_is_not_homebrew_home() {
        assert!(!is_homebrew_install(Path::new(
            "/home/user/.local/bin/claude-history"
        )));
    }
}
