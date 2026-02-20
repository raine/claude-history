pub mod claude;
pub mod cursor;

use crate::claude::LogEntry;
use crate::error::Result;
use crate::history::{Conversation, LoaderMessage, ProviderKind};
use std::sync::mpsc::Receiver;

#[allow(dead_code)]
pub trait Provider: Send + Sync {
    fn kind(&self) -> ProviderKind;
    fn name(&self) -> &str;
    fn detect(&self) -> bool;

    /// Load conversations (synchronous, for single-project mode)
    fn load_conversations(&self, show_last: bool, debug: Option<crate::cli::DebugLevel>) -> Result<Vec<Conversation>>;

    /// Load conversations with streaming (for global mode)
    fn load_conversations_streaming(&self, show_last: bool, debug: Option<crate::cli::DebugLevel>) -> Receiver<LoaderMessage>;

    /// Read log entries for viewing/export (the core abstraction)
    fn read_entries(&self, conversation: &Conversation) -> Result<Vec<LogEntry>>;

    /// Resume a conversation in the original tool
    fn resume(&self, conversation: &Conversation, default_args: &[String]) -> Result<()>;

    /// Delete a conversation
    fn delete(&self, conversation: &Conversation) -> Result<()>;
}
