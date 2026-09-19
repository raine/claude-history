use crate::agent;
use crate::agent::diagnostic::{AgentError, AgentErrorKind, AgentWarning, AgentWarningKind};
use crate::agent::visibility::ContentVisibility;
use crate::cli::{self, AgentCommand, AgentOutlineArgs, AgentOutputFlags, AgentReadArgs};
use crate::config;
use crate::config::{AgentConfig, AgentScopeConfig, ConfigFile};
use crate::error::{AppError, Result};
use crate::history;
use crate::search;
use crate::search::mode::{SearchMode, SearchModeResolution, resolve_search_mode};
use crate::search::query::ParsedQuery;
use crate::semantic;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

type ResolvedReadRefs = Vec<(agent::refs::ReadRef, agent::refs::ResolvedConversation)>;

const DEFAULT_OUTPUT_CHARS: usize = 6000;
const DEFAULT_SEARCH_TOP: usize = 10;
const DEFAULT_WITHIN_TOP: usize = 20;
const DEFAULT_HITS_PER_CONVERSATION: usize = 2;

fn configured_usize(cli_value: Option<usize>, default: usize, configured: Option<usize>) -> usize {
    cli_value.or(configured).unwrap_or(default)
}

fn configured_visibility(config: &AgentConfig) -> ContentVisibility {
    ContentVisibility {
        tools: config.tools.unwrap_or(false),
        tool_results: config.tool_results.unwrap_or(false),
        thinking: config.thinking.unwrap_or(false),
        subagents: config.subagents.unwrap_or(false),
    }
}

fn apply_render_policy(output: &mut agent::search::AgentSearchOutput, policy: ContentVisibility) {
    for hit in &mut output.hits {
        hit.render_options.merge(policy);
    }
    for hit in output
        .groups
        .iter_mut()
        .flat_map(|group| group.hits.iter_mut())
    {
        hit.render_options.merge(policy);
    }
}

fn configured_budget(
    no_budget: bool,
    cli_budget: Option<usize>,
    configured: Option<usize>,
) -> Option<usize> {
    (!no_budget).then(|| configured_usize(cli_budget, DEFAULT_OUTPUT_CHARS, configured))
}

fn configured_scope(
    local: bool,
    all: bool,
    config: &AgentConfig,
) -> agent::search::AgentSearchScope {
    if local {
        agent::search::AgentSearchScope::Local
    } else if all {
        agent::search::AgentSearchScope::Global
    } else {
        match config.scope.unwrap_or(AgentScopeConfig::Global) {
            AgentScopeConfig::Global => agent::search::AgentSearchScope::Global,
            AgentScopeConfig::Local => agent::search::AgentSearchScope::Local,
        }
    }
}

/// Everything an agent command takes from CLI flags and config, resolved once
/// per command by [`AgentSettings::resolve`]: CLI wins, then `[agent]`, then
/// `[search]`, then built-in defaults. A command that does not use a field
/// sees its default.
#[derive(Clone, Debug)]
pub(crate) struct AgentSettings {
    /// `--mode` > `[agent].mode` > `[search].mode`. A quoted-only query still
    /// runs exact; see `agent::search::effective_agent_mode`.
    pub mode: SearchMode,
    /// Conversations for grouped search; message hits for `--flat` and within.
    pub top: usize,
    pub hits_per_conversation: usize,
    pub budget: Option<usize>,
    pub scope: agent::search::AgentSearchScope,
    /// Content reads reveal and hit recipes request: CLI flags OR config, so
    /// config can reveal but never hide.
    pub visibility: ContentVisibility,
    pub exclude_projects: Vec<String>,
}

impl AgentSettings {
    pub(crate) fn resolve(command: &AgentCommand, config: ConfigFile) -> Self {
        let flags = match command {
            AgentCommand::Search(args) => CommandFlags {
                mode: args.mode_override(),
                top: args.top,
                top_default: TopDefault::Search,
                budget: args.budget,
                no_budget: args.no_budget,
                hits_per_conversation: args.hits_per_conv,
                local: args.local,
                all: args.all,
                visibility: ContentVisibility::default(),
            },
            AgentCommand::Within(args) => CommandFlags {
                mode: args.mode_override(),
                top: args.top,
                top_default: TopDefault::Within,
                budget: args.budget,
                no_budget: args.no_budget,
                ..CommandFlags::default()
            },
            AgentCommand::Read(args) => CommandFlags::output(&args.output),
            AgentCommand::Outline(args) => CommandFlags::output(&args.output),
        };
        Self::from_flags(flags, config)
    }

    fn from_flags(flags: CommandFlags, config: ConfigFile) -> Self {
        let search_config = config.search.unwrap_or_default();
        let agent = config.agent.unwrap_or_default();
        let mut visibility = flags.visibility;
        visibility.merge(configured_visibility(&agent));
        Self {
            mode: resolve_search_mode(SearchModeResolution {
                cli_mode: flags.mode,
                config_mode: agent.mode.or(search_config.mode),
                tui_semantic_search: None,
            }),
            top: flags.top_default.resolve(flags.top, &agent),
            hits_per_conversation: configured_usize(
                flags.hits_per_conversation,
                DEFAULT_HITS_PER_CONVERSATION,
                agent.hits_per_conversation,
            ),
            budget: configured_budget(flags.no_budget, flags.budget, agent.output_chars),
            scope: configured_scope(flags.local, flags.all, &agent),
            visibility,
            exclude_projects: agent.exclude_projects,
        }
    }

    fn protocol_options(&self) -> agent::protocol::ProtocolOptions {
        agent::protocol::ProtocolOptions {
            budget: self.budget,
            visibility: self.visibility,
        }
    }
}

/// The flags each subcommand exposes, in one shape so [`AgentSettings`] has
/// one resolution path.
#[derive(Default)]
struct CommandFlags {
    mode: Option<SearchMode>,
    top: Option<usize>,
    top_default: TopDefault,
    budget: Option<usize>,
    no_budget: bool,
    hits_per_conversation: Option<usize>,
    local: bool,
    all: bool,
    visibility: ContentVisibility,
}

impl CommandFlags {
    fn output(flags: &AgentOutputFlags) -> Self {
        Self {
            budget: flags.budget,
            no_budget: flags.no_budget,
            visibility: ContentVisibility {
                tools: flags.tools,
                tool_results: flags.tool_results,
                thinking: flags.thinking,
                subagents: flags.subagents,
            },
            ..Self::default()
        }
    }
}

/// Which `--top` a command means: search and within have separate defaults
/// and config keys.
#[derive(Clone, Copy, Default)]
enum TopDefault {
    #[default]
    Search,
    Within,
}

impl TopDefault {
    fn resolve(self, cli_value: Option<usize>, config: &AgentConfig) -> usize {
        match self {
            Self::Search => configured_usize(cli_value, DEFAULT_SEARCH_TOP, config.top),
            Self::Within => configured_usize(cli_value, DEFAULT_WITHIN_TOP, config.within_top),
        }
    }
}

fn project_is_excluded(path: &Path, excluded: &[String]) -> bool {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|project| {
            excluded
                .iter()
                .any(|excluded| history::is_same_project(project, excluded))
        })
}

#[derive(Default)]
pub struct AgentService {
    transcripts: RefCell<
        HashMap<PathBuf, std::result::Result<agent::transcript::AgentTranscript, AgentError>>,
    >,
    #[cfg(test)]
    transcript_parse_count: std::cell::Cell<usize>,
}

pub fn execute(command: AgentCommand) -> Result<String> {
    AgentService::default().execute(command)
}

impl AgentService {
    pub fn execute(&mut self, command: AgentCommand) -> Result<String> {
        self.execute_inner(command)
            .and_then(ensure_complete_compact_output)
            .map_err(|error| {
                let error = structured_agent_error(error);
                if let AppError::Agent(agent_error) = error {
                    AppError::AgentProtocol(agent::diagnostic::format_error(&agent_error))
                } else {
                    error
                }
            })
    }

    fn execute_inner(&mut self, command: AgentCommand) -> Result<String> {
        let settings = AgentSettings::resolve(&command, config::load_config()?);
        match command {
            AgentCommand::Search(args) => self.run_search(&args, &settings),
            AgentCommand::Within(args) => self.run_within(&args, &settings),
            AgentCommand::Read(args) => self.run_read(&args, None, &settings),
            AgentCommand::Outline(args) => self.run_outline(&args, None, &settings),
        }
    }

    fn load_transcript(&self, path: &Path) -> Result<agent::transcript::AgentTranscript> {
        if let Some(cached) = self.transcripts.borrow().get(path) {
            return cached.clone().map_err(AppError::from);
        }
        #[cfg(test)]
        self.transcript_parse_count
            .set(self.transcript_parse_count.get() + 1);
        let loaded = agent::transcript::AgentTranscript::load(path).map_err(|error| match error {
            AppError::Agent(error) => error,
            AppError::Io(error) => AgentError::io(
                Some(&path.to_string_lossy()),
                format!("failed to read transcript: {error}"),
            ),
            AppError::Json(error) => AgentError::malformed_transcript(
                Some(&path.to_string_lossy()),
                format!("failed to parse transcript JSONL: {error}"),
            ),
            error => {
                AgentError::malformed_transcript(Some(&path.to_string_lossy()), error.to_string())
            }
        });
        self.transcripts
            .borrow_mut()
            .insert(path.to_path_buf(), loaded.clone());
        loaded.map_err(AppError::from)
    }

    fn run_search(&self, args: &cli::AgentSearchArgs, settings: &AgentSettings) -> Result<String> {
        // Resolved before loading so an inverted range fails without paying for
        // a full corpus parse.
        let time = args.time.resolve()?;
        let mut conversations = history::load_all_conversations(false, None)?;
        conversations.retain(|conversation| {
            !project_is_excluded(&conversation.path, &settings.exclude_projects)
                && time.matches(conversation.timestamp)
        });
        conversations.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        let current_project_dir_name = if settings.scope == agent::search::AgentSearchScope::Local {
            std::env::current_dir()
                .ok()
                .map(|dir| history::convert_path_to_project_dir_name(&dir))
        } else {
            None
        };
        let scoped = agent::search::scoped_conversation_inputs(
            &conversations,
            settings.scope,
            current_project_dir_name.as_deref(),
        )?;
        let query = ParsedQuery::parse(&args.query);
        let request = agent::search::AgentSearchRequest {
            mode: agent::search::effective_agent_mode(&query, settings.mode),
            query,
            top: settings.top,
            flat: args.flat,
            hits_per_conversation: settings.hits_per_conversation,
            retrieval_hits_per_conversation: None,
            all_hits: args.all_hits,
            budget: settings.budget,
        };
        let (mut keys, mut base_warnings) =
            discover_agent_keys(current_project_dir_name.as_deref())?;
        keys.retain(|key| !project_is_excluded(&key.path, &settings.exclude_projects));
        if time.is_active() {
            // Key discovery walks the projects directory independently, so
            // without this every conversation outside the window would be
            // reported as a skipped transcript rather than simply filtered out.
            // Tested against each file's own timestamp, not against membership
            // in `conversations`, so that transcripts inside the window which
            // failed to parse still report their diagnostics.
            keys.retain(|key| transcript_timestamp(&key.path).is_none_or(|at| time.matches(at)));
        }
        base_warnings.extend(warnings_for_skipped_transcripts(
            self,
            &conversations,
            &keys,
        ));
        match request.mode {
            SearchMode::Lexical | SearchMode::Exact => {
                let ranked = lexically_rank_scoped(&conversations, &request.query, &scoped);
                let warnings = RefCell::new(base_warnings.clone());
                let mut output = agent::search::run_global_lexical_search_reporting(
                    &request,
                    &conversations,
                    &keys,
                    &ranked,
                    |key| self.load_transcript(&key.path),
                    |key, error| {
                        warnings.borrow_mut().push(AgentWarning::from_app_error(
                            error,
                            Some(
                                &agent::refs::resolved_conversation_for_key(&keys, key)
                                    .reference
                                    .canonical(),
                            ),
                        ));
                    },
                )?;
                apply_render_policy(&mut output, settings.visibility);
                return Ok(agent::search::format_agent_output_with_warnings(
                    &output,
                    &warnings.into_inner(),
                ));
            }
            SearchMode::Semantic => {
                let (mut output, mut warnings) =
                    run_agent_semantic_search(self, &request, &conversations, &keys, &scoped)?;
                apply_render_policy(&mut output, settings.visibility);
                warnings.splice(0..0, base_warnings);
                return Ok(agent::search::format_agent_output_with_warnings(
                    &output, &warnings,
                ));
            }
            SearchMode::Hybrid => {
                // Lexical candidates for fusion: deeper, flat, and bounded
                // per conversation, ranked as a plain lexical search.
                let lexical_request = agent::search::AgentSearchRequest {
                    mode: SearchMode::Lexical,
                    top: agent::search::modality_candidate_depth(&request),
                    flat: true,
                    retrieval_hits_per_conversation: Some(
                        request.hits_per_conversation.saturating_mul(4).max(1),
                    ),
                    ..request.clone()
                };
                let ranked = lexically_rank_scoped(&conversations, &request.query, &scoped);
                let warnings = RefCell::new(base_warnings.clone());
                let lexical = agent::search::run_global_lexical_search_reporting(
                    &lexical_request,
                    &conversations,
                    &keys,
                    &ranked,
                    |key| self.load_transcript(&key.path),
                    |key, error| {
                        warnings.borrow_mut().push(AgentWarning::from_app_error(
                            error,
                            Some(
                                &agent::refs::resolved_conversation_for_key(&keys, key)
                                    .reference
                                    .canonical(),
                            ),
                        ));
                    },
                )?;
                let inputs = agent_inputs_for_indices(&conversations, &keys, &scoped)?;
                match run_agent_semantic_hits(&request.query, &inputs) {
                    Ok((semantic, semantic_warnings)) => {
                        warnings.borrow_mut().extend(semantic_warnings);
                        let mut output = agent::search::run_global_hybrid_search(
                            &request, lexical, &semantic, &inputs,
                        );
                        attach_input_transcript_metadata(self, &mut output, &inputs);
                        apply_render_policy(&mut output, settings.visibility);
                        return Ok(agent::search::format_agent_output_with_warnings(
                            &output,
                            &warnings.into_inner(),
                        ));
                    }
                    Err(error) => {
                        // Semantic search is unavailable: answer with lexical
                        // evidence while still reporting the hybrid request.
                        warnings
                            .borrow_mut()
                            .push(AgentWarning::from_app_error(&error, None));
                        let mut output = agent::search::run_global_lexical_search_reporting(
                            &request,
                            &conversations,
                            &keys,
                            &ranked,
                            |key| self.load_transcript(&key.path),
                            |key, error| {
                                warnings.borrow_mut().push(AgentWarning::from_app_error(
                                    error,
                                    Some(
                                        &agent::refs::resolved_conversation_for_key(&keys, key)
                                            .reference
                                            .canonical(),
                                    ),
                                ));
                            },
                        )?;
                        apply_render_policy(&mut output, settings.visibility);
                        return Ok(agent::search::format_agent_output_with_warnings(
                            &output,
                            &warnings.into_inner(),
                        ));
                    }
                }
            }
        }
    }

    fn run_within(&self, args: &cli::AgentWithinArgs, settings: &AgentSettings) -> Result<String> {
        let (keys, _) = discover_agent_keys(None)?;
        let resolved = resolve_agent_conversation_arg(&args.conversation, Some(&keys))?;
        let transcript = self
            .load_transcript(&resolved.key.path)
            .map_err(|error| target_error(error, &resolved))?;
        let conversation = conversation_from_agent_transcript(&transcript, resolved.key.source);
        let mut warnings = transcript_warning(&transcript, &resolved.reference.canonical())
            .into_iter()
            .collect::<Vec<_>>();
        let query = ParsedQuery::parse(&args.query);
        let request = agent::search::AgentWithinRequest {
            mode: agent::search::effective_agent_mode(&query, settings.mode),
            query,
            top: settings.top,
            budget: settings.budget,
        };
        let mut output = match request.mode {
            SearchMode::Lexical | SearchMode::Exact => agent::search::run_within_lexical_search(
                &request,
                &conversation,
                &resolved,
                &transcript,
            ),
            SearchMode::Semantic => run_agent_within_semantic(
                &request,
                &conversation,
                &resolved,
                &transcript,
                SemanticToolContent::All,
            )?,
            SearchMode::Hybrid => {
                match run_agent_within_semantic(
                    &request,
                    &conversation,
                    &resolved,
                    &transcript,
                    SemanticToolContent::MatchingQuery(request.query.raw()),
                ) {
                    Ok(output) => output,
                    Err(error) => {
                        // Semantic search is unavailable: answer with lexical
                        // evidence while still reporting the hybrid request.
                        warnings.push(AgentWarning::from_app_error(&error, None));
                        agent::search::run_within_lexical_search(
                            &request,
                            &conversation,
                            &resolved,
                            &transcript,
                        )
                    }
                }
            }
        };
        agent::search::attach_transcript_metadata(&mut output, &resolved, &transcript);
        apply_render_policy(&mut output, settings.visibility);
        Ok(agent::search::format_agent_output_with_warnings(
            &output, &warnings,
        ))
    }
}

fn discover_agent_keys(
    project_filter: Option<&str>,
) -> Result<(Vec<agent::refs::AgentConversationKey>, Vec<AgentWarning>)> {
    let root = history::get_claude_projects_root().map_err(structured_agent_error)?;
    let projects = if root.exists() {
        std::fs::read_dir(&root)
            .map_err(|error| {
                AgentError::io(
                    Some(&root.to_string_lossy()),
                    format!("failed to list projects: {error}"),
                )
            })?
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let mut keys = Vec::new();
    let mut warnings = Vec::new();
    for project in projects {
        let project = match project {
            Ok(project) => project,
            Err(error) => {
                warnings.push(AgentWarning {
                    kind: AgentWarningKind::Io,
                    reference: None,
                    detail: format!("failed to read project entry: {error}"),
                });
                continue;
            }
        };
        let project_path = project.path();
        if !project_path.is_dir() {
            continue;
        }
        let Some(project_name) = project_path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if project_filter.is_some_and(|filter| !history::is_same_project(project_name, filter)) {
            continue;
        }
        let entries = match std::fs::read_dir(&project_path) {
            Ok(entries) => entries,
            Err(error) => {
                warnings.push(AgentWarning {
                    kind: AgentWarningKind::Io,
                    reference: None,
                    detail: format!(
                        "failed to list project transcripts at {}: {error}",
                        project_path.display()
                    ),
                });
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    warnings.push(AgentWarning {
                        kind: AgentWarningKind::Io,
                        reference: None,
                        detail: format!(
                            "failed to read transcript entry in {}: {error}",
                            project_path.display()
                        ),
                    });
                    continue;
                }
            };
            let path = entry.path();
            let Some(filename) = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            if path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
                && !filename.starts_with("agent-")
            {
                keys.push(agent::refs::AgentConversationKey::new(
                    project_name,
                    filename,
                    path,
                ));
            }
        }
    }
    if let Ok(pi_root) = history::pi_loader::session_root()
        && let Ok(pi_files) = history::pi_loader::discover_files(&pi_root)
    {
        let current = std::env::current_dir()
            .ok()
            .map(|path| path.canonicalize().unwrap_or(path));
        for path in pi_files {
            let Ok(Some(projection)) = history::pi::parse_file(&path) else {
                continue;
            };
            if projection.source != history::Source::Pi {
                continue;
            }
            if let Some(filter) = project_filter {
                let cwd = projection
                    .header
                    .cwd
                    .canonicalize()
                    .unwrap_or_else(|_| projection.header.cwd.clone());
                let current_matches_filter = current.as_ref().is_some_and(|current| {
                    history::is_same_project(
                        &history::convert_path_to_project_dir_name(current),
                        filter,
                    )
                });
                if !current_matches_filter || current.as_ref() != Some(&cwd) {
                    continue;
                }
            }
            let project_identity = projection
                .header
                .cwd
                .canonicalize()
                .unwrap_or(projection.header.cwd)
                .to_string_lossy()
                .into_owned();
            let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            keys.push(agent::refs::AgentConversationKey {
                source: history::Source::Pi,
                project_dir_name: project_identity,
                session_filename: filename.to_owned(),
                session_id: projection.header.id,
                path,
            });
        }
    }
    if let Ok(omp_root) = history::omp_loader::session_root()
        && let Ok(omp_files) = history::omp_loader::discover_files(&omp_root)
    {
        let current = std::env::current_dir()
            .ok()
            .map(|path| path.canonicalize().unwrap_or(path));
        for path in omp_files {
            let Ok(Some(projection)) = history::pi::parse_omp_file(&path) else {
                continue;
            };
            if let Some(filter) = project_filter {
                let cwd = projection
                    .header
                    .cwd
                    .canonicalize()
                    .unwrap_or_else(|_| projection.header.cwd.clone());
                let current_matches_filter = current.as_ref().is_some_and(|current| {
                    history::is_same_project(
                        &history::convert_path_to_project_dir_name(current),
                        filter,
                    )
                });
                if !current_matches_filter || current.as_ref() != Some(&cwd) {
                    continue;
                }
            }
            let project_identity = projection
                .header
                .cwd
                .canonicalize()
                .unwrap_or(projection.header.cwd)
                .to_string_lossy()
                .into_owned();
            let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            keys.push(agent::refs::AgentConversationKey {
                source: history::Source::Omp,
                project_dir_name: project_identity,
                session_filename: filename.to_owned(),
                session_id: projection.header.id,
                path,
            });
        }
    }
    if keys.is_empty() && !root.exists() {
        return Err(AgentError::io(
            Some(&root.to_string_lossy()),
            "no Claude, Pi, or OMP history storage is available",
        )
        .into());
    }
    keys.sort_by(|left, right| left.path.cmp(&right.path));
    Ok((keys, warnings))
}

fn warnings_for_skipped_transcripts(
    service: &AgentService,
    conversations: &[history::Conversation],
    keys: &[agent::refs::AgentConversationKey],
) -> Vec<AgentWarning> {
    let key_paths = keys
        .iter()
        .map(|key| key.path.as_path())
        .collect::<std::collections::HashSet<_>>();
    let known = conversations
        .iter()
        .map(|conversation| conversation.path.as_path())
        .collect::<std::collections::HashSet<_>>();
    let mut warnings = Vec::new();
    for conversation in conversations {
        if key_paths.contains(conversation.path.as_path()) && !conversation.parse_errors.is_empty()
        {
            let key = agent::refs::AgentConversationKey::from_conversation(conversation).ok();
            warnings.push(AgentWarning {
                kind: crate::agent::diagnostic::AgentWarningKind::MalformedTranscript,
                reference: key.as_ref().map(|key| {
                    agent::refs::resolved_conversation_for_key(keys, key)
                        .reference
                        .canonical()
                }),
                detail: format!(
                    "transcript contains {} malformed JSONL record(s)",
                    conversation.parse_errors.len()
                ),
            });
        }
    }
    for key in keys {
        if known.contains(key.path.as_path()) {
            continue;
        }
        let reference = agent::refs::resolved_conversation_for_key(keys, key)
            .reference
            .canonical();
        match service.load_transcript(&key.path) {
            Ok(transcript) => warnings.push(AgentWarning::skipped(
                Some(&reference),
                if transcript.is_empty() {
                    "transcript has no visible messages"
                } else {
                    "transcript has no searchable conversation metadata"
                },
            )),
            Err(error) => warnings.push(AgentWarning::from_app_error(&error, Some(&reference))),
        }
    }
    warnings
}

fn conversation_from_agent_transcript(
    transcript: &agent::transcript::AgentTranscript,
    source: history::Source,
) -> history::Conversation {
    let message_text = transcript
        .messages
        .iter()
        .map(|message| {
            message
                .parts
                .iter()
                .filter_map(agent::transcript::agent_part_search_text)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    let preview = message_text
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ... ");
    let preview_last = message_text
        .iter()
        .rev()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ... ");
    let full_text = message_text.join(" ");
    let dialogue_text = transcript
        .messages
        .iter()
        .flat_map(|message| message.parts.iter())
        .filter_map(|part| match part {
            agent::transcript::AgentMessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let timestamp = std::fs::metadata(&transcript.path)
        .and_then(|metadata| metadata.modified())
        .map(chrono::DateTime::<chrono::Local>::from)
        .unwrap_or_else(|_| chrono::Local::now());
    let semantic_route_text = history::semantic_route_text(&full_text, "");
    history::Conversation {
        source,
        session_id: transcript
            .path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned(),
        path: transcript.path.clone(),
        index: 0,
        timestamp,
        preview: preview.clone(),
        preview_first: preview,
        preview_last,
        search_text_lower: crate::search::normalize_for_search(&full_text),
        dialogue_text_lower: crate::search::normalize_for_search(&dialogue_text),
        full_text,
        agent_search_text: String::new(),
        semantic_route_text,
        semantic_turns: Vec::new(),
        semantic_turn_ranges: Vec::new(),
        project_name: None,
        project_path: None,
        cwd: None,
        message_count: transcript.messages.len(),
        parse_errors: Vec::new(),
        summary: transcript.summary.clone(),
        custom_title: transcript.custom_title.clone(),
        model: None,
        total_tokens: 0,
        duration_minutes: None,
    }
}

fn transcript_warning(
    transcript: &agent::transcript::AgentTranscript,
    reference: &str,
) -> Option<AgentWarning> {
    transcript
        .malformed_warning_detail()
        .map(|detail| AgentWarning {
            kind: AgentWarningKind::MalformedTranscript,
            reference: Some(reference.to_string()),
            detail,
        })
}

fn target_error(error: AppError, resolved: &agent::refs::ResolvedConversation) -> AppError {
    match error {
        AppError::Agent(mut error) => {
            error.reference = Some(resolved.reference.canonical());
            error.into()
        }
        error => structured_agent_error(error),
    }
}

fn ensure_complete_compact_output(output: String) -> Result<String> {
    if output.ends_with('\n') {
        Ok(output)
    } else {
        Err(AgentError::new(
            AgentErrorKind::BudgetTooSmall,
            None,
            "output budget cannot fit a complete compact record; increase --budget",
        )
        .into())
    }
}

/// The timestamp a transcript is filtered on, matching how conversation
/// timestamps are derived. `None` when the file cannot be inspected, so callers
/// keep it rather than silently dropping it.
fn transcript_timestamp(path: &std::path::Path) -> Option<chrono::DateTime<chrono::Local>> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .map(chrono::DateTime::<chrono::Local>::from)
}

fn structured_agent_error(error: AppError) -> AppError {
    match error {
        AppError::Agent(_) => error,
        AppError::SessionNotFound(reference) => AgentError::new(
            AgentErrorKind::NotFound,
            Some(&reference),
            "conversation was not found",
        )
        .into(),
        AppError::Json(error) => {
            AgentError::malformed_transcript(None, format!("failed to parse transcript: {error}"))
                .into()
        }
        AppError::Io(error) => AgentError::io(None, error.to_string()).into(),
        AppError::ConfigError(detail) => AgentError::invalid_ref("command", detail).into(),
        AppError::TimeFilter(error) => {
            AgentError::out_of_range(Some("command"), error.to_string()).into()
        }
        error => AgentError::io(None, error.to_string()).into(),
    }
}

fn run_agent_semantic_search(
    service: &AgentService,
    request: &agent::search::AgentSearchRequest,
    conversations: &[history::Conversation],
    keys: &[agent::refs::AgentConversationKey],
    indices: &[usize],
) -> Result<(agent::search::AgentSearchOutput, Vec<AgentWarning>)> {
    let inputs = agent_inputs_for_indices(conversations, keys, indices)?;
    let (semantic, warnings) = run_agent_semantic_hits(&request.query, &inputs)?;
    let mut output = agent::search::run_global_semantic_search(request, &inputs, &semantic);
    attach_input_transcript_metadata(service, &mut output, &inputs);
    Ok((output, warnings))
}

fn attach_input_transcript_metadata(
    service: &AgentService,
    output: &mut agent::search::AgentSearchOutput,
    inputs: &[agent::search::AgentConversationInput<'_>],
) {
    let referenced = output
        .hits
        .iter()
        .chain(output.groups.iter().flat_map(|group| group.hits.iter()))
        .map(|hit| hit.conversation_ref.clone())
        .collect::<std::collections::HashSet<_>>();
    for input in inputs {
        if !referenced.contains(input.resolved.reference.canonical().as_str()) {
            continue;
        }
        if let Ok(transcript) = service.load_transcript(&input.resolved.key.path) {
            agent::search::attach_transcript_metadata(output, &input.resolved, &transcript);
        }
    }
}

fn run_agent_semantic_hits(
    query: &ParsedQuery,
    inputs: &[agent::search::AgentConversationInput<'_>],
) -> Result<(Vec<semantic::types::SemanticHit>, Vec<AgentWarning>)> {
    let mut candidates = Vec::with_capacity(inputs.len().saturating_mul(2));
    for input in inputs {
        candidates.push(semantic::index::SemanticIndexCandidate {
            index: input.original_index,
            source: semantic::types::SemanticChunkSource::VisibleDialogue,
            conversation: std::sync::Arc::new(stripped_semantic_conversation(
                input.conversation,
                input.conversation.path.clone(),
                input.conversation.semantic_turns.clone(),
                input.conversation.semantic_turn_ranges.clone(),
            )),
        });
        if !input.conversation.semantic_turns.is_empty()
            && let Some(route) = agent_route_semantic_conversation(input.conversation)
        {
            candidates.push(semantic::index::SemanticIndexCandidate {
                index: input.original_index,
                source: semantic::types::SemanticChunkSource::AgentRoute,
                conversation: std::sync::Arc::new(route),
            });
        }
    }
    run_agent_semantic_hits_for_candidates(
        query,
        &candidates,
        semantic::types::MAX_GLOBAL_INTERACTIVE_PASSAGE_EMBEDDINGS,
    )
    .map(|hits| (hits, Vec::new()))
}

pub(crate) fn agent_route_semantic_conversation(
    conversation: &history::Conversation,
) -> Option<history::Conversation> {
    let route_text = if conversation.semantic_route_text.is_empty() {
        conversation
            .custom_title
            .as_deref()
            .or(conversation.summary.as_deref())?
            .to_string()
    } else {
        conversation.semantic_route_text.clone()
    };
    let file_name = conversation
        .path
        .file_name()
        .map(|name| format!("{}.agent-route-semantic", name.to_string_lossy()))?;
    Some(stripped_semantic_conversation(
        conversation,
        conversation.path.with_file_name(file_name),
        vec![route_text],
        vec![
            conversation
                .semantic_turn_ranges
                .first()
                .copied()
                .unwrap_or_else(|| crate::history::MessageRange::single(1)),
        ],
    ))
}

fn stripped_semantic_conversation(
    conversation: &history::Conversation,
    path: PathBuf,
    semantic_turns: Vec<String>,
    semantic_turn_ranges: Vec<crate::history::MessageRange>,
) -> history::Conversation {
    history::Conversation {
        source: conversation.source,
        session_id: conversation.session_id.clone(),
        path,
        index: conversation.index,
        timestamp: conversation.timestamp,
        preview: String::new(),
        preview_first: String::new(),
        preview_last: String::new(),
        full_text: String::new(),
        agent_search_text: String::new(),
        semantic_route_text: String::new(),
        semantic_turns,
        semantic_turn_ranges,
        search_text_lower: String::new(),
        dialogue_text_lower: String::new(),
        project_name: None,
        project_path: None,
        cwd: None,
        message_count: conversation.message_count,
        parse_errors: Vec::new(),
        summary: None,
        custom_title: None,
        model: None,
        total_tokens: 0,
        duration_minutes: None,
    }
}

fn run_agent_semantic_hits_for_candidates(
    parsed: &ParsedQuery,
    candidates: &[semantic::index::SemanticIndexCandidate],
    max_new_embeddings: usize,
) -> Result<Vec<semantic::types::SemanticHit>> {
    let request = semantic::index::SemanticIndexRequest {
        query: parsed.semantic_text(),
        literal_filters: parsed.literals(),
        full_corpus: candidates,
        scope: candidates,
        corpus_version: 3,
        prewarm: false,
        include_chunk_hits: true,
    };
    let mut state = semantic::index::SemanticIndexState::new();
    let mut embedder = semantic::fastembed::FastembedEmbedder::new().map_err(|error| {
        AgentError::semantic_unavailable(format!("failed to initialize semantic search: {error}"))
    })?;
    let cancellation = semantic::types::SemanticCancellationToken::new();
    let response = state
        .refresh_or_prewarm_with_budget(
            &request,
            &mut embedder,
            &cancellation,
            Some(max_new_embeddings),
            |progress| eprintln!("Semantic search: {progress:?}"),
            semantic::cache::write_embedding_cache,
        )
        .map_err(|error| {
            AgentError::semantic_unavailable(format!("semantic search failed: {error}"))
        })?;
    if response.missing_chunk_count > 0 {
        if max_new_embeddings == 0 {
            eprintln!(
                "Semantic search: {} passage(s) are not cached; run claude-history --generate-semantic-cache",
                response.missing_chunk_count
            );
        } else {
            eprintln!(
                "Semantic search: {} passage(s) remain uncached after the bounded top-up; rerun this search to cache more",
                response.missing_chunk_count
            );
        }
    }
    Ok(response.chunk_hits)
}

#[cfg(test)]
pub(crate) fn agent_semantic_candidates(
    inputs: &[agent::search::AgentConversationInput<'_>],
) -> (
    Vec<semantic::index::SemanticIndexCandidate>,
    Vec<AgentWarning>,
) {
    agent_semantic_candidates_with_loader(
        inputs,
        |path| agent::transcript::AgentTranscript::load(path),
        SemanticToolContent::All,
    )
}

#[cfg(test)]
pub(crate) fn agent_semantic_candidates_matching_tools(
    inputs: &[agent::search::AgentConversationInput<'_>],
    query: &str,
) -> (
    Vec<semantic::index::SemanticIndexCandidate>,
    Vec<AgentWarning>,
) {
    agent_semantic_candidates_with_loader(
        inputs,
        |path| agent::transcript::AgentTranscript::load(path),
        SemanticToolContent::MatchingQuery(query),
    )
}

#[cfg(test)]
fn agent_semantic_candidates_with_loader<'a>(
    inputs: &[agent::search::AgentConversationInput<'_>],
    load_transcript: impl Fn(&Path) -> Result<agent::transcript::AgentTranscript>,
    tool_content: SemanticToolContent<'a>,
) -> (
    Vec<semantic::index::SemanticIndexCandidate>,
    Vec<AgentWarning>,
) {
    let mut candidates = Vec::new();
    let mut warnings = Vec::new();
    for input in inputs {
        match load_transcript(&input.resolved.key.path) {
            Ok(transcript) => {
                if let Some(warning) =
                    transcript_warning(&transcript, &input.resolved.reference.canonical())
                {
                    warnings.push(warning);
                }
                push_agent_semantic_candidates(&mut candidates, input, &transcript, tool_content)
            }
            Err(error) => warnings.push(AgentWarning::from_app_error(
                &error,
                Some(&input.resolved.reference.canonical()),
            )),
        }
    }
    (candidates, warnings)
}

#[derive(Clone, Copy)]
enum SemanticToolContent<'a> {
    All,
    MatchingQuery(&'a str),
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SemanticAgentPartKind {
    Dialogue,
    Tool,
    Thinking,
}

fn push_agent_semantic_candidates<'a>(
    candidates: &mut Vec<semantic::index::SemanticIndexCandidate>,
    input: &agent::search::AgentConversationInput<'_>,
    transcript: &agent::transcript::AgentTranscript,
    tool_content: SemanticToolContent<'a>,
) {
    for (subagents, kind, source) in [
        (
            false,
            SemanticAgentPartKind::Dialogue,
            semantic::types::SemanticChunkSource::VisibleDialogue,
        ),
        (
            false,
            SemanticAgentPartKind::Tool,
            semantic::types::SemanticChunkSource::AgentTool,
        ),
        (
            false,
            SemanticAgentPartKind::Thinking,
            semantic::types::SemanticChunkSource::AgentThinking,
        ),
        (
            true,
            SemanticAgentPartKind::Dialogue,
            semantic::types::SemanticChunkSource::AgentSubagentDialogue,
        ),
        (
            true,
            SemanticAgentPartKind::Tool,
            semantic::types::SemanticChunkSource::AgentSubagentTool,
        ),
        (
            true,
            SemanticAgentPartKind::Thinking,
            semantic::types::SemanticChunkSource::AgentSubagentThinking,
        ),
    ] {
        push_agent_semantic_candidate(
            candidates,
            input,
            transcript,
            subagents,
            kind,
            source,
            tool_content,
        );
    }
}

fn push_agent_semantic_candidate<'a>(
    candidates: &mut Vec<semantic::index::SemanticIndexCandidate>,
    input: &agent::search::AgentConversationInput<'_>,
    transcript: &agent::transcript::AgentTranscript,
    subagents: bool,
    kind: SemanticAgentPartKind,
    source: semantic::types::SemanticChunkSource,
    tool_content: SemanticToolContent<'a>,
) {
    let Some(conversation) = agent_semantic_conversation(
        input.conversation,
        transcript,
        subagents,
        kind,
        tool_content,
    ) else {
        return;
    };
    candidates.push(semantic::index::SemanticIndexCandidate {
        index: input.original_index,
        source,
        conversation: std::sync::Arc::new(conversation),
    });
}

fn agent_semantic_conversation<'a>(
    conversation: &history::Conversation,
    transcript: &agent::transcript::AgentTranscript,
    subagents: bool,
    kind: SemanticAgentPartKind,
    tool_content: SemanticToolContent<'a>,
) -> Option<history::Conversation> {
    let mut semantic_turns = Vec::new();
    let mut semantic_turn_ranges = Vec::new();
    for message in &transcript.messages {
        if message.parent_tool_use_id.is_some() != subagents {
            continue;
        }
        let role = match message.role {
            agent::transcript::AgentMessageRole::User => semantic::filter::SemanticTurnRole::User,
            agent::transcript::AgentMessageRole::Assistant => {
                semantic::filter::SemanticTurnRole::Assistant
            }
        };
        for part in &message.parts {
            let matches_kind = matches!(
                (kind, part),
                (
                    SemanticAgentPartKind::Dialogue,
                    agent::transcript::AgentMessagePart::Text { .. }
                ) | (
                    SemanticAgentPartKind::Tool,
                    agent::transcript::AgentMessagePart::ToolUse { .. }
                        | agent::transcript::AgentMessagePart::ToolResult { .. }
                ) | (
                    SemanticAgentPartKind::Thinking,
                    agent::transcript::AgentMessagePart::Thinking { .. }
                )
            );
            if !matches_kind {
                continue;
            }
            let texts = match (kind, part) {
                (SemanticAgentPartKind::Tool, part) => {
                    agent::transcript::agent_part_search_text(part)
                        .map(|text| semantic_tool_texts(&text, tool_content))
                        .unwrap_or_default()
                }
                _ => agent::transcript::agent_part_search_text(part)
                    .into_iter()
                    .collect(),
            };
            for text in texts {
                if let Some(turn) = semantic::filter::filter_turn(role, &text) {
                    semantic_turns.push(turn);
                    semantic_turn_ranges
                        .push(crate::history::MessageRange::single(message.ordinal));
                }
            }
        }
    }
    if semantic_turns.is_empty() {
        return None;
    }
    let mut conversation = conversation.clone();
    let scope = if subagents { "subagent" } else { "main" };
    let content = match kind {
        SemanticAgentPartKind::Dialogue => "dialogue",
        SemanticAgentPartKind::Tool => "tool",
        SemanticAgentPartKind::Thinking => "thinking",
    };
    let file_name = conversation.path.file_name().map(|name| {
        format!(
            "{}.agent-{scope}-{content}-semantic",
            name.to_string_lossy()
        )
    })?;
    conversation.path = conversation.path.with_file_name(file_name);
    conversation.semantic_turns = semantic_turns;
    conversation.semantic_turn_ranges = semantic_turn_ranges;
    Some(conversation)
}

fn semantic_tool_texts(text: &str, tool_content: SemanticToolContent<'_>) -> Vec<String> {
    let segments = stable_text_segments(text, semantic::types::DEFAULT_CHUNK_TARGET_CHARS);
    match tool_content {
        SemanticToolContent::All => segments,
        SemanticToolContent::MatchingQuery(query) => {
            let terms = semantic_tool_query_terms(query);
            let required_matches = if terms.len() >= 3 { 2 } else { 1 };
            segments
                .into_iter()
                .filter_map(|segment| {
                    let matches = semantic_tool_text_match_count(&segment, &terms);
                    (matches >= required_matches).then_some((matches, segment))
                })
                .max_by_key(|(matches, _)| *matches)
                .map(|(_, segment)| vec![segment])
                .unwrap_or_default()
        }
    }
}

fn semantic_tool_text_match_count(text: &str, terms: &[String]) -> usize {
    let normalized = normalize_semantic_tool_text(text);
    terms
        .iter()
        .filter(|term| crate::text_match::contains_prefix_match(&normalized, term))
        .count()
}

fn normalize_semantic_tool_text(text: &str) -> String {
    let mut expanded = String::with_capacity(text.len());
    let mut previous_was_lowercase = false;
    for ch in text.chars() {
        if previous_was_lowercase && ch.is_uppercase() {
            expanded.push(' ');
        }
        expanded.push(ch);
        previous_was_lowercase = ch.is_lowercase();
    }
    crate::text_match::normalize_for_search(&expanded)
}

fn semantic_tool_query_terms(query: &str) -> Vec<String> {
    const STOP_WORDS: &[&str] = &[
        "after", "all", "an", "and", "are", "as", "at", "be", "before", "but", "by", "can", "do",
        "does", "for", "from", "has", "have", "how", "if", "in", "is", "it", "its", "my", "no",
        "not", "of", "on", "or", "our", "so", "than", "that", "the", "them", "then", "they",
        "this", "to", "up", "was", "we", "what", "when", "where", "which", "why", "with", "would",
        "you", "your",
    ];

    normalize_semantic_tool_text(query)
        .split_whitespace()
        .map(|term| term.trim_matches(|ch: char| !ch.is_alphanumeric()))
        .filter(|term| term.chars().count() >= 2)
        .filter(|term| !STOP_WORDS.contains(term))
        .map(str::to_string)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect()
}

fn stable_text_segments(mut text: &str, max_bytes: usize) -> Vec<String> {
    let mut segments = Vec::new();
    while !text.is_empty() {
        let mut end = text.len().min(max_bytes);
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            end = text
                .char_indices()
                .nth(1)
                .map_or(text.len(), |(index, _)| index);
        }
        segments.push(text[..end].to_string());
        text = &text[end..];
    }
    segments
}

fn run_agent_within_semantic<'a>(
    request: &agent::search::AgentWithinRequest,
    conversation: &history::Conversation,
    resolved: &agent::refs::ResolvedConversation,
    transcript: &agent::transcript::AgentTranscript,
    tool_content: SemanticToolContent<'a>,
) -> Result<agent::search::AgentSearchOutput> {
    let input = agent::search::AgentConversationInput {
        conversation,
        resolved: resolved.clone(),
        original_index: 0,
    };
    let mut candidates = Vec::new();
    push_agent_semantic_candidates(&mut candidates, &input, transcript, tool_content);
    let semantic = run_agent_semantic_hits_for_candidates(
        &request.query,
        &candidates,
        semantic::types::MAX_WITHIN_INTERACTIVE_PASSAGE_EMBEDDINGS,
    )?;
    Ok(agent::search::run_within_search(
        request,
        conversation,
        resolved,
        transcript,
        &semantic,
    ))
}

fn agent_inputs_for_indices<'a>(
    conversations: &'a [history::Conversation],
    keys: &[agent::refs::AgentConversationKey],
    indices: &[usize],
) -> Result<Vec<agent::search::AgentConversationInput<'a>>> {
    let resolved_by_path = agent::refs::resolved_conversations_for_keys(keys)
        .into_iter()
        .map(|resolved| (resolved.key.path.clone(), resolved))
        .collect::<std::collections::HashMap<_, _>>();
    indices
        .iter()
        .filter_map(|index| {
            let conversation = conversations.get(*index)?;
            let resolved = resolved_by_path.get(&conversation.path)?;
            Some(Ok(agent::search::AgentConversationInput {
                conversation,
                resolved: resolved.clone(),
                original_index: *index,
            }))
        })
        .collect()
}

impl AgentService {
    pub(crate) fn run_read(
        &self,
        args: &AgentReadArgs,
        keys: Option<&[agent::refs::AgentConversationKey]>,
        settings: &AgentSettings,
    ) -> Result<String> {
        let discovered;
        let keys = match keys {
            Some(keys) => keys,
            None => {
                discovered = discover_agent_keys(None)?.0;
                &discovered
            }
        };
        let (mut resolved_refs, focus) = resolve_agent_read_args(args, Some(keys))?;
        let options = settings.protocol_options();
        let transcripts = resolved_refs
            .iter()
            .map(|(_, resolved)| {
                self.load_transcript(&resolved.key.path)
                    .map_err(|error| target_error(error, resolved))
            })
            .collect::<Result<Vec<_>>>()?;
        if let Some(anchor) = args.anchor.as_deref() {
            let ordinal = transcripts[0].resolve_anchor(&resolved_refs[0].1, anchor)?;
            resolved_refs[0].0.range = Some(crate::history::MessageRange::single(ordinal));
        }
        let requests = resolved_refs
            .iter()
            .zip(transcripts.iter())
            .map(
                |((read_ref, resolved), transcript)| agent::protocol::ReadRequest {
                    resolved,
                    transcript,
                    range: read_ref.range,
                },
            )
            .collect::<Vec<_>>();
        let protocol_focus = focus
            .map(|focus| -> Result<agent::protocol::ProtocolFocus> {
                let conversation_full_ref = focus
                    .conversation
                    .as_ref()
                    .map(|conversation| {
                        agent::refs::resolve_conversation_ref(keys, conversation)
                            .map(|resolved| resolved.reference.full_ref())
                    })
                    .transpose()?;
                Ok(agent::protocol::ProtocolFocus {
                    conversation_full_ref,
                    range: focus.range,
                })
            })
            .transpose()?;
        let slice = if let Some(range) = args.lines {
            Some(agent::protocol::ReadSlice::Lines(range))
        } else {
            args.match_query
                .as_ref()
                .map(|query| agent::protocol::ReadSlice::Match {
                    query: query.clone(),
                    context: args.context,
                })
        };
        let warnings = resolved_refs
            .iter()
            .zip(transcripts.iter())
            .filter_map(|((_, resolved), transcript)| {
                transcript_warning(transcript, &resolved.reference.canonical())
            })
            .collect::<Vec<_>>();
        agent::protocol::format_read_with_warnings(
            &requests,
            protocol_focus,
            slice.as_ref(),
            options,
            &warnings,
        )
        .map_err(|error| match resolved_refs.first() {
            Some((_, resolved)) => target_error(error, resolved),
            None => structured_agent_error(error),
        })
    }

    pub(crate) fn run_outline(
        &self,
        args: &AgentOutlineArgs,
        keys: Option<&[agent::refs::AgentConversationKey]>,
        settings: &AgentSettings,
    ) -> Result<String> {
        let discovered;
        let keys = match keys {
            Some(keys) => keys,
            None => {
                discovered = discover_agent_keys(None)?.0;
                &discovered
            }
        };
        let resolved = resolve_agent_conversation_arg(&args.conversation, Some(keys))?;
        let transcript = self
            .load_transcript(&resolved.key.path)
            .map_err(|error| target_error(error, &resolved))?;
        let warning = transcript_warning(&transcript, &resolved.reference.canonical());
        Ok(agent::protocol::format_outline_with_warnings(
            &resolved,
            &transcript,
            settings.protocol_options(),
            warning.as_slice(),
        ))
    }
}

pub(crate) fn resolve_agent_read_args(
    args: &AgentReadArgs,
    keys: Option<&[agent::refs::AgentConversationKey]>,
) -> Result<(ResolvedReadRefs, Option<agent::refs::FocusRef>)> {
    let refs = args
        .refs
        .iter()
        .map(|reference| agent::refs::parse_read_ref(reference))
        .collect::<Result<Vec<_>>>()?;
    if args.anchor.is_some() && (refs.len() != 1 || refs[0].range.is_some()) {
        return Err(AppError::ConfigError(
            "--anchor requires exactly one conversation ref without an mN range".to_string(),
        ));
    }
    if (args.lines.is_some() || args.match_query.is_some())
        && (refs.len() != 1
            || (!refs[0].range.is_some_and(|range| range.start == range.end)
                && args.anchor.is_none()))
    {
        return Err(AppError::ConfigError(
            "--lines and --match require exactly one single-message ref such as ch_...:m7"
                .to_string(),
        ));
    }
    let loaded_keys;
    let keys = if let Some(keys) = keys {
        keys
    } else {
        let conversations = history::load_all_conversations(false, None)?;
        loaded_keys = agent::refs::conversation_keys_from_conversations(&conversations)?;
        &loaded_keys
    };
    let resolved_refs = refs
        .iter()
        .map(|reference| {
            agent::refs::resolve_conversation_ref(keys, &reference.conversation)
                .map(|resolved| (reference.clone(), resolved))
        })
        .collect::<Result<Vec<_>>>()?;
    let focus = args
        .focus
        .as_deref()
        .map(agent::refs::parse_focus_ref)
        .transpose()?;
    if let Some(focus) = &focus {
        let focus_conversation = focus
            .conversation
            .as_ref()
            .map(|conversation| agent::refs::resolve_conversation_ref(keys, conversation))
            .transpose()?;
        agent::refs::validate_resolved_focus_in_ranges(
            &resolved_refs,
            focus,
            focus_conversation.as_ref(),
        )?;
    }
    Ok((resolved_refs, focus))
}

fn lexically_rank_scoped(
    conversations: &[history::Conversation],
    query: &ParsedQuery,
    scoped: &[usize],
) -> Vec<usize> {
    let searchable = search::precompute_agent_search_text(conversations);
    let ranked_all =
        search::agent_search_parsed(conversations, &searchable, query, chrono::Local::now());
    let scoped_set = scoped
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    ranked_all
        .into_iter()
        .filter(|index| scoped_set.contains(index))
        .collect()
}

pub(crate) fn resolve_agent_conversation_arg(
    reference: &str,
    keys: Option<&[agent::refs::AgentConversationKey]>,
) -> Result<agent::refs::ResolvedConversation> {
    let loaded_keys;
    let keys = if let Some(keys) = keys {
        keys
    } else {
        let conversations = history::load_all_conversations(false, None)?;
        loaded_keys = agent::refs::conversation_keys_from_conversations(&conversations)?;
        &loaded_keys
    };
    agent::refs::resolve_conversation_ref(keys, reference)
}

/// Settings for a read or outline driven from tests, resolved from the same
/// user config the binary would load.
#[cfg(test)]
pub(crate) fn output_settings(flags: &AgentOutputFlags) -> Result<AgentSettings> {
    Ok(AgentSettings::from_flags(
        CommandFlags::output(flags),
        config::load_config()?,
    ))
}

#[cfg(test)]
pub(crate) fn run_agent_read(
    args: &AgentReadArgs,
    keys: Option<&[agent::refs::AgentConversationKey]>,
) -> Result<String> {
    AgentService::default().run_read(args, keys, &output_settings(&args.output)?)
}

#[cfg(test)]
pub(crate) fn run_agent_outline(
    args: &AgentOutlineArgs,
    keys: Option<&[agent::refs::AgentConversationKey]>,
) -> Result<String> {
    AgentService::default().run_outline(args, keys, &output_settings(&args.output)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::test_support::user_jsonl_line;
    use crate::cli::AgentOutputFlags;

    fn output_flags() -> AgentOutputFlags {
        AgentOutputFlags {
            budget: Some(6000),
            no_budget: false,
            tools: false,
            tool_results: false,
            thinking: false,
            subagents: false,
        }
    }

    fn settings() -> AgentSettings {
        output_settings(&output_flags()).unwrap()
    }

    fn read_args(reference: String) -> AgentReadArgs {
        AgentReadArgs {
            refs: vec![reference],
            anchor: None,
            focus: None,
            lines: None,
            match_query: None,
            context: 3,
            output: output_flags(),
        }
    }

    #[test]
    fn agent_route_uses_compact_precomputed_text() {
        let mut conversation = crate::semantic::test_fixtures::SemanticConversationFixture::new(
            "/tmp/session.jsonl",
            ["dialogue that should not be copied into the route"],
        )
        .with_title("Align step calibration and treadmill UX")
        .with_summary(Some(
            "Calibrate automatic counting across treadmill sessions",
        ))
        .build();
        conversation.full_text =
            "calibration isolation and per-device capability memory from bounded tool evidence"
                .to_string();
        conversation.semantic_route_text =
            history::semantic_route_text(&conversation.full_text, "");

        let route = agent_route_semantic_conversation(&conversation).unwrap();

        assert_eq!(route.semantic_turns.len(), 1);
        assert!(route.semantic_turns[0].chars().count() <= 1_805);
        assert!(route.semantic_turns[0].contains("per-device capability memory"));
        assert_eq!(
            route.semantic_turn_ranges,
            conversation.semantic_turn_ranges[..1]
        );
        assert!(
            route
                .path
                .to_string_lossy()
                .ends_with("session.jsonl.agent-route-semantic")
        );
        assert!(!route.semantic_turns[0].contains("should not be copied"));
    }

    #[test]
    fn anchor_read_resolves_message_after_unrelated_prefix() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        std::fs::write(
            &path,
            [user_jsonl_line("unrelated"), user_jsonl_line("target")].join("\n"),
        )
        .unwrap();
        let key = agent::refs::AgentConversationKey::new("project", "session.jsonl", path);
        let resolved = agent::refs::resolved_conversation_for_key(std::slice::from_ref(&key), &key);
        let transcript = agent::transcript::AgentTranscript::load(&resolved.key.path).unwrap();
        let anchor = transcript.message_anchor(&resolved, &transcript.messages[1]);
        let mut args = read_args(resolved.reference.canonical());
        args.anchor = Some(anchor.clone());

        let output = AgentService::default()
            .run_read(&args, Some(std::slice::from_ref(&key)), &settings())
            .unwrap();

        assert!(output.contains("message m2 role=user"));
        assert!(output.contains(&format!("anchor={anchor}")));
        assert!(output.contains("| target\n"));
        assert!(!output.contains("| unrelated\n"));
    }

    #[test]
    fn agent_mode_ignores_tui_semantic_search() {
        let config: ConfigFile = toml::from_str(
            r#"
[search]
mode = "lexical"
[tui]
semantic_search = true
"#,
        )
        .unwrap();

        let settings = AgentSettings::from_flags(CommandFlags::default(), config);

        assert_eq!(settings.mode, SearchMode::Lexical);
    }

    #[test]
    fn agent_config_overrides_general_search_mode() {
        let config: ConfigFile = toml::from_str(
            r#"
[search]
mode = "lexical"
[agent]
mode = "hybrid"
"#,
        )
        .unwrap();

        let settings = AgentSettings::from_flags(CommandFlags::default(), config);

        assert_eq!(settings.mode, SearchMode::Hybrid);
    }

    #[test]
    fn cli_flags_override_config_and_config_only_reveals_visibility() {
        let config: ConfigFile = toml::from_str(
            r#"
[agent]
mode = "hybrid"
top = 3
within_top = 4
hits_per_conversation = 5
output_chars = 700
scope = "local"
tools = true
"#,
        )
        .unwrap();
        let flags = CommandFlags {
            mode: Some(SearchMode::Exact),
            top: Some(9),
            top_default: TopDefault::Within,
            budget: Some(100),
            hits_per_conversation: None,
            all: true,
            visibility: ContentVisibility {
                thinking: true,
                ..ContentVisibility::default()
            },
            ..CommandFlags::default()
        };

        let settings = AgentSettings::from_flags(flags, config);

        assert_eq!(settings.mode, SearchMode::Exact);
        assert_eq!(settings.top, 9);
        assert_eq!(settings.hits_per_conversation, 5);
        assert_eq!(settings.budget, Some(100));
        assert_eq!(settings.scope, agent::search::AgentSearchScope::Global);
        assert_eq!(
            settings.visibility,
            ContentVisibility {
                tools: true,
                thinking: true,
                ..ContentVisibility::default()
            }
        );

        let defaults = AgentSettings::from_flags(CommandFlags::default(), ConfigFile::default());
        assert_eq!(defaults.mode, SearchMode::Lexical);
        assert_eq!(defaults.top, DEFAULT_SEARCH_TOP);
        assert_eq!(defaults.budget, Some(DEFAULT_OUTPUT_CHARS));
        assert_eq!(defaults.visibility, ContentVisibility::default());
    }

    #[test]
    fn explicit_agent_values_override_agent_defaults() {
        assert_eq!(configured_usize(Some(7), 10, Some(12)), 7);
        assert_eq!(configured_usize(None, 10, Some(12)), 12);
        assert_eq!(configured_budget(false, Some(6000), Some(9000)), Some(6000));
        assert_eq!(configured_budget(false, None, Some(9000)), Some(9000));
        assert_eq!(configured_budget(true, Some(6000), Some(9000)), None);
    }

    #[test]
    fn invocation_cache_reuses_loaded_target_transcript() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        std::fs::write(&path, user_jsonl_line("cached message")).unwrap();
        let key = agent::refs::AgentConversationKey::new("project", "session.jsonl", path.clone());
        let reference = key.conversation_ref().canonical();
        let args = AgentReadArgs {
            refs: vec![format!("{reference}:m1")],
            anchor: None,
            focus: None,
            lines: None,
            match_query: None,
            context: 3,
            output: output_flags(),
        };
        let service = AgentService::default();

        assert!(
            service
                .run_read(&args, Some(std::slice::from_ref(&key)), &settings())
                .is_ok()
        );
        std::fs::write(&path, "{malformed").unwrap();
        let output = service
            .run_read(&args, Some(std::slice::from_ref(&key)), &settings())
            .unwrap();

        assert!(output.contains("cached message"));
        assert_eq!(service.transcript_parse_count.get(), 1);
    }

    #[test]
    fn partial_target_reports_structured_warning_and_stable_ordinals() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        std::fs::write(
            &path,
            [
                user_jsonl_line("first"),
                "{malformed".to_string(),
                user_jsonl_line("third"),
            ]
            .join("\n"),
        )
        .unwrap();
        let key = agent::refs::AgentConversationKey::new("project", "session.jsonl", path);
        let reference = key.conversation_ref().canonical();
        let args = AgentOutlineArgs {
            conversation: reference.clone(),
            output: output_flags(),
        };

        let output = AgentService::default()
            .run_outline(&args, Some(std::slice::from_ref(&key)), &settings())
            .unwrap();

        assert!(output.contains("warnings=1"));
        assert!(output.contains("kind=malformed-transcript"));
        assert!(output.contains("lines%202"));
        assert!(output.contains("m1 role=user"));
        assert!(output.contains("m2 role=user"));
    }

    #[test]
    fn malformed_target_is_a_typed_service_failure() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        std::fs::write(&path, "{malformed").unwrap();
        let key = agent::refs::AgentConversationKey::new("project", "session.jsonl", path);
        let reference = key.conversation_ref().canonical();
        let args = AgentOutlineArgs {
            conversation: reference.clone(),
            output: output_flags(),
        };

        let error = AgentService::default()
            .run_outline(&args, Some(std::slice::from_ref(&key)), &settings())
            .unwrap_err();
        let AppError::Agent(error) = error else {
            panic!("expected typed agent error");
        };
        assert_eq!(error.kind, AgentErrorKind::MalformedTranscript);
        assert_eq!(error.reference.as_deref(), Some(reference.as_str()));
    }
}
