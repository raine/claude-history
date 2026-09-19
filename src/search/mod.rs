pub mod lexical;
pub mod literal;
pub mod matcher;
pub mod mode;
pub mod query;
#[cfg(test)]
pub mod test_fixtures;

pub use crate::text_match::{is_word_separator, normalize_for_search};
pub use lexical::{
    LexicalDebugSearch, SearchableConversation, agent_search, agent_search_parsed,
    debug_agent_search, debug_search, is_uuid, precompute_agent_search_text,
    precompute_search_text, search,
};
pub use matcher::{LexicalEvidence, QueryMatcher, build_lexical_evidence};
