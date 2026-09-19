use crate::cli::DebugLevel;

/// Check if a message at `msg_level` should be printed given the configured `min_level`
pub fn should_log(min_level: DebugLevel, msg_level: DebugLevel) -> bool {
    msg_level >= min_level
}

/// Print a debug-level message if the minimum level allows it
pub fn debug(min_level: Option<DebugLevel>, message: &str) {
    if let Some(level) = min_level
        && should_log(level, DebugLevel::Debug)
    {
        eprintln!("[DEBUG] {}", message);
    }
}

/// Print an info-level message if the minimum level allows it
pub fn info(min_level: Option<DebugLevel>, message: &str) {
    if let Some(level) = min_level
        && should_log(level, DebugLevel::Info)
    {
        eprintln!("[INFO] {}", message);
    }
}

/// Print a warn-level message if the minimum level allows it
pub fn warn(min_level: Option<DebugLevel>, message: &str) {
    if let Some(level) = min_level
        && should_log(level, DebugLevel::Warn)
    {
        eprintln!("[WARN] {}", message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_log_same_level() {
        assert!(should_log(DebugLevel::Debug, DebugLevel::Debug));
        assert!(should_log(DebugLevel::Info, DebugLevel::Info));
        assert!(should_log(DebugLevel::Warn, DebugLevel::Warn));
        assert!(should_log(DebugLevel::Error, DebugLevel::Error));
    }

    #[test]
    fn test_should_log_higher_level() {
        assert!(should_log(DebugLevel::Debug, DebugLevel::Info));
        assert!(should_log(DebugLevel::Debug, DebugLevel::Warn));
        assert!(should_log(DebugLevel::Debug, DebugLevel::Error));
        assert!(should_log(DebugLevel::Info, DebugLevel::Warn));
        assert!(should_log(DebugLevel::Info, DebugLevel::Error));
        assert!(should_log(DebugLevel::Warn, DebugLevel::Error));
    }

    #[test]
    fn test_should_not_log_lower_level() {
        assert!(!should_log(DebugLevel::Info, DebugLevel::Debug));
        assert!(!should_log(DebugLevel::Warn, DebugLevel::Debug));
        assert!(!should_log(DebugLevel::Warn, DebugLevel::Info));
        assert!(!should_log(DebugLevel::Error, DebugLevel::Debug));
        assert!(!should_log(DebugLevel::Error, DebugLevel::Info));
        assert!(!should_log(DebugLevel::Error, DebugLevel::Warn));
    }
}
