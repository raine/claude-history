# AGENTS.md

Guidance for coding agents working in this repository. Written for agents, not
humans; the README covers user-facing behaviour.

`claude-history` is a Rust CLI (edition 2024, single binary crate) that
discovers, indexes, searches, renders, and manages coding-agent transcripts
(Claude Code, Pi, OMP). It has three consumers of one corpus: an interactive
TUI, a non-interactive display/export path, and a machine-readable `agent`
protocol consumed by the companion skill in `skills/claude-history/SKILL.md`.

Update this file in the same commit as any major architecture change it describes.

## Commands

`checkle.toml` is the source of truth for project checks (`just check` and the
pre-commit hook both run `checkle`). Neither `just` nor `checkle` may be
installed; the raw equivalents are:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked
cargo test --all --locked
cargo build --all --locked
```

- Single test: `cargo test --locked --bin claude-history <name>` (unit tests,
  including `agent_command_tests` in `src/main.rs`);
  `cargo test --locked --test agent_cli <name>` (integration).
- Run the app: `cargo run -- <args>`; `just install-dev` symlinks the debug
  binary into `~/.cargo/bin`.
- Cold `cargo test --no-run` is ~3 min (fastembed/ort); a warm full suite
  runs in ~1.5 s.

Facts that shape how to treat check results:

- **CI does not run tests, clippy, or fmt.** `.github/workflows/nix.yml` only
  runs `nix build`, and `flake.nix` sets `doCheck = false`. The gates are
  local: the checkle pre-commit hook (opt-in via `just install-hooks`) and
  workmux `pre_merge: just check-ci`. Run the suite yourself.
- A clean tree has ~30 clippy warnings on current stable. Do not mass-fix
  unrelated warnings in a feature change.
- **First run on a fresh machine fails one test**:
  `tui::semantic_worker::tests::empty_prewarm_search_builds_cache_without_idle_short_circuit`
  hits its 10 s timeout while fastembed downloads the ~127 MB BGE-small model
  into `~/.cache/claude-history/semantic/fastembed`. It passes once the model
  is cached; it fails permanently offline. It is not a code regression.
- Tests are not hermetic against `$HOME`: `src/history/cache.rs` tests and the
  semantic worker tests read/write the real `~/.cache/claude-history`;
  `tests/agent_cli.rs` sets `CLAUDE_CONFIG_DIR` and
  `PI_CODING_AGENT_SESSION_DIR` to temp dirs but not `HOME`, so a personal
  `~/.config/claude-history/config.toml` leaks into integration tests.
- Release (`just release`) needs `cargo-release` from raine's
  `rust-release-tools` (pipx), not the crates.io tool; see `RELEASE.md`. Do
  not run releases; `CHANGELOG.md` entries are user-facing bullets per version.
- `CUA_SANDBOX.md` describes an ARM64 TUI verification sandbox that needs a
  macOS host with Xpra and Docker; do not assume it is available.

Commit subjects are lowercase imperative with no type prefix
(`add first-class OMP session support`); bodies are common.

## Architecture

```
history/   discover → parse → bincode cache ─┐
                                              ▼
                                        Conversation  (derived summary + search text)
                                              │
              ┌───────────────────────────────┼──────────────────────────┐
              ▼                               ▼                          ▼
        search/ (lexical)             semantic/ (embeddings)        display.rs / tui/export.rs
              └──────────┬────────────────────┘
                         ▼
             tui/ (interactive)   agent/ (protocol CLI)
```

`src/main.rs::run` is the dispatcher: `agent`, `delete-empty`, `update`
subcommands return early; the flag-driven paths (`--debug-search`,
`--semantic-search`, `--generate-semantic-cache`, `--render`, direct file
input) follow; otherwise it streams conversations into the TUI and acts on the
returned `tui::Action` (select/resume/fork). Resume execs `claude`/`pi`/`omp`
after the terminal guard is dropped.

### history/ — three sources, one model

- `Conversation` (`history/mod.rs`) is a summary of a transcript file, not the
  message list: identity, timestamp, preview/title, `full_text` +
  `search_text_lower` (lexical), `semantic_turns` + `semantic_turn_ranges`
  (semantic), project info. Message entries are re-read from disk when viewed
  (`normalized_log_entries`).
- Claude transcripts are parsed directly as `claude::LogEntry`. Pi and OMP
  files go through `history/pi.rs::parse_reader`, which projects the active
  branch (following `parentId` from the leaf) into `LogEntry`s and feeds the
  same parser. Control records (summaries, titles, Pi/OMP session metadata,
  `/clear` wrappers, warmups) are kept out of previews, `message_count`, and
  semantic turns but may remain in `full_text`.
- Message ordinals (`mN` in the agent protocol) are assigned by one module:
  `history/messages.rs::MessageOrdinals`. Both walkers — `history/parser.rs`
  (`message_count`, cached `MessageRange`s) and `agent/transcript.rs` — feed
  it records in file order and act on the returned `Placement`
  (`Message`/`Replaces`/`Control`); the rule (warmups, `/clear` wrappers,
  streamed assistant dedupe by id, subagent `progress` records, searchable
  Pi/OMP metadata) lives nowhere else, and `messages.rs` has a parity test
  over both walkers. Changing the rule shifts every agent reference and
  requires a history cache bump. `MessageRange` lives here too, so `history/`
  no longer depends on `agent/refs.rs` (it still borrows text-bounding
  helpers from `agent/transcript.rs`).
- Discovery roots and env vars: `CLAUDE_CONFIG_DIR` (Claude),
  `PI_CODING_AGENT_SESSION_DIR`, `PI_CODING_AGENT_DIR` (Pi),
  `OMP_PROFILE`/`PI_PROFILE`, `PI_CONFIG_DIR`, `XDG_DATA_HOME` (OMP). Root
  resolution functions take env values as parameters so they are testable.
- Claude `timestamp` is file mtime, so rename (which appends records) or any
  tool that touches the file reorders the list and invalidates its cache entry.

### Two caches, two version constants

- `history/cache.rs`: per-project bincode files under
  `~/.cache/claude-history/`. Validity = size + mtime match. `SCHEMA_VERSION`
  (Claude) and `PI_SCHEMA_VERSION`/`OMP_SCHEMA_VERSION` guard a shared
  `CacheEntry`. bincode has no field names: any change to `CacheEntry`,
  `MessageRange`, or to how the parser computes a cached field requires
  bumping the relevant constant(s), plus updating `entry_from_conversation`,
  `conversation_from_entry`, `empty_entry`, and the test helper.
- `semantic/cache.rs`: `~/.cache/claude-history/semantic/embeddings-v1.bin`,
  keyed by `blake3(chunk text)`, header guarded by
  `semantic/types.rs::CACHE_SCHEMA_VERSION` + model + chunk config. Only header
  changes invalidate explicitly; edits to `semantic/filter.rs::filter_turn`,
  chunking, or `semantic_route_text` silently make every entry a miss. Global
  agent search never embeds at query time
  (`MAX_GLOBAL_INTERACTIVE_PASSAGE_EMBEDDINGS = 0`), so a text-keying change
  yields zero semantic results until `--generate-semantic-cache` is rerun.
  Changing embedding meaning (model, query prefix in `semantic/fastembed.rs`)
  requires a `CACHE_SCHEMA_VERSION` bump.

### search/ and semantic/

- `search/query.rs::ParsedQuery` is the shared handoff: quoted spans are
  smart-case exact `Literal`s (hard filters), unquoted words are scored. An
  unquoted term containing `_` is promoted to an exact literal
  (`search/lexical.rs`) *and* its split words are still scored.
- Lexical score (`search/lexical.rs::score_impl`): per field (title 5,
  project 4, summary 4, dialogue 3, body 1) `ln(1+tf)` capped at tf=10, plus
  whole-word, adjacency and ≥3-word phrase bonuses scaled by field weight; a
  flat verbatim bonus (raw unquoted query found in `full_text`/project name,
  `VerbatimNeedle` decides case sensitivity) and additive freshness (max 2.0,
  7-day half-life). Body tf saturates on tool output for almost every hit,
  so the `dialogue_text_lower` field (visible user/assistant prose, tag spans
  stripped in `history/parser.rs`) is what separates "about X" from "mentions
  X". Word-boundary rules live in `text_match.rs` and are shared with
  `search/matcher.rs` and the agent retrieval paths: a query word starting
  with punctuation does not require a word start.
- `search/matcher.rs::QueryMatcher` is the one place that locates a
  `ParsedQuery` in text: highlight ranges, "is this literal visible in the
  preview", and the hidden-context evidence (`LexicalEvidence`) the lexical
  worker precomputes per hit. `ParsedQuery::words`/`identifier_literals` own
  the `_`-promotion rule. `tui/snippet.rs` only fits text around ranges the
  matcher returns; it never re-derives matches.
- Mode precedence (`search/mode.rs::resolve_search_mode`): CLI > `[agent].mode`
  > `[search].mode` > deprecated `[tui].semantic_search`. The agent resolves it
  once per command in `agent/service.rs::AgentSettings::resolve` (with every
  other agent setting); `agent/search.rs::effective_agent_mode` then forces
  Exact for a quoted-only query. The TUI collapses Hybrid/Exact to Lexical.
- "Hybrid" inside `semantic/rank.rs` is cosine + word-overlap bonus. RRF fusion
  of lexical and semantic rankings exists only in `agent/search.rs`.
- Semantic tests use per-module fake embedders; no test needs the model except
  the semantic worker prewarm test noted above.

### agent/ — a public protocol

`skills/claude-history/SKILL.md` is the consumer contract; `tests/agent_cli.rs`
is its regression surface (runs the built binary via `CARGO_BIN_EXE`).
Consumers parse records by named atoms and must tolerate extra atoms, so:

- Adding an atom is safe. Renaming, removing, or reordering atoms, changing
  `kind=` strings, or inserting a line between a `hit` record and its `read`
  recipe breaks the skill. Update SKILL.md in the same change.
- `ch_` refs (`agent/refs.rs`) and `ma_` anchors (`agent/transcript.rs`) are
  hashes over namespaced inputs; `ma_` embeds the full `ch_` digest. Changing
  `REF_NAMESPACE` or the digest scheme invalidates every handle and anchor.
- Output is budgeted to a hard character cap (`chars=`) by
  `agent/records.rs::Response`: formatters supply a header (whole and cut
  forms), record units and a recovery footer; a `hit` and its `read` recipe
  are one unit so truncation never separates them. `read` keeps its own
  body-trimming selection (`protocol.rs::select_for_budget`). Output that
  does not end in a newline is reported as `budget-too-small`. All errors exit 1 with
  one `protocol agent-error` line on stderr. Semantic modes also print
  progress to stderr on success.
- Visibility flags (`--tools` etc.) are OR'd with `[agent]` config; config can
  reveal but never hide.

### tui/ and rendering

- `tui/runtime.rs` owns the loop and `TerminalGuard` (raw mode + alternate
  screen on **stderr**). `App` state is split by concern under `tui/app/`;
  `tui/app/types.rs::Action` is what returns to main.
- Streaming load: batches arrive over a channel and are appended, but search
  text is only precomputed and search re-dispatched at `finish_loading`.
- Lexical and semantic workers are threads with generation counters. Any
  mutation of `conversations` must go through `refresh_search_data` /
  `invalidate_search_generation` or stale worker responses get applied.
  Semantic mode shows the lexical result for the same generation first, then
  replaces it.
- Two transcript walkers, both over `history::normalized_log_entries` (so
  every renderer understands Claude, Pi and OMP): `tui/viewer/` produces
  styled `RenderedLine`s and is the only ledger — the TUI, post-selection
  display, `--render` and the ledger export are all sinks over its lines;
  `turns.rs` projects visible `Turn`s/`Part`s (visibility applied, assistant
  parts ordered prose → tools → thinking) for the text renderers (`--plain`,
  plain/markdown export, clipboard). Command-tag grammar (`/clear` wrappers,
  skill prompts, `<command-name>`) lives only in `command_tags.rs`.
  `viewer::NAME_WIDTH`/`SEPARATOR_WIDTH` are the single width constants.
- `ToolDisplayMode::Hidden` means "summary". `show_thinking` also gates
  subagent and agent-progress visibility in both renderers.
- Width math must use `unicode-width`, not byte length.

### stdout must stay clean

`--show-id`/`--show-path`/`--show-dir` are used in command substitution, and
`CUA_SANDBOX.md` exists to verify that. Consequences: the TUI and OSC52
clipboard writes go to stderr; `tui::theme::detect_theme()` runs once in
`main.rs` *before* raw mode (terminal-light queries the tty) and is skipped when
stdout is not a tty or under `cfg!(test)`; `syntax.rs` reads the theme lazily,
so highlighting before detection locks in dark. Never `println!` from inside
the alternate screen.

### config.rs

`~/.config/claude-history/config.toml`; every table is
`#[serde(deny_unknown_fields)]`, so removing a deprecated key (`global`,
`display.relative_time`, `[tui].semantic_search`) breaks existing user configs.
Config only parses; CLI/config/default merging happens in callers.

## Test conventions

- Unit tests are inline `#[cfg(test)] mod tests` per file; `tui/app/tests.rs`
  and `tui/app/interaction_tests.rs` are child modules that poke private
  state. No snapshot/golden files. Fixtures: `tests/fixtures/{pi,omp}/*.jsonl`.
- Parser tests build JSONL strings and use `process_conversation_reader`;
  filesystem tests use `tempfile`. Helpers are per-file (`search/test_fixtures.rs`,
  `semantic/test_fixtures.rs`, `agent/test_support.rs`,
  `tui/app/semantic_test_helpers.rs`); there is no shared test crate.
- `tests/cargo_manifest.rs` asserts fastembed stays non-optional; do not
  reintroduce a semantic feature gate. The only feature is
  `release-dynamic-ort` (release builds load a packaged ONNX Runtime beside
  the binary).
