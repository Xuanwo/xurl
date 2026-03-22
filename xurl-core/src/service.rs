use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use grep::regex::RegexMatcherBuilder;
use grep::searcher::{BinaryDetection, SearcherBuilder, sinks::Lossy};
use regex::RegexBuilder;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use walkdir::WalkDir;

use crate::error::{Result, XurlError};
use crate::jsonl;
use crate::model::{
    MessageRole, PathThreadQuery, PathThreadQueryResult, PiEntryListItem, PiEntryListView,
    PiEntryQuery, ProviderKind, ResolvedThread, SubagentDetailView, SubagentExcerptMessage,
    SubagentLifecycleEvent, SubagentListItem, SubagentListView, SubagentQuery, SubagentRelation,
    SubagentThreadRef, SubagentView, ThreadQuery, ThreadQueryItem, ThreadQueryResult, WriteRequest,
    WriteResult,
};
use crate::provider::amp::AmpProvider;
use crate::provider::claude::ClaudeProvider;
use crate::provider::codex::CodexProvider;
use crate::provider::gemini::GeminiProvider;
use crate::provider::opencode::OpencodeProvider;
use crate::provider::pi::PiProvider;
use crate::provider::{Provider, ProviderRoots, WriteEventSink};
use crate::render;
use crate::uri::{AgentsUri, is_uuid_session_id};

const STATUS_PENDING_INIT: &str = "pendingInit";
const STATUS_RUNNING: &str = "running";
const STATUS_COMPLETED: &str = "completed";
const STATUS_ERRORED: &str = "errored";
const STATUS_SHUTDOWN: &str = "shutdown";
const STATUS_NOT_FOUND: &str = "notFound";
const QUERY_METADATA_LINE_BUDGET: usize = 64;

#[derive(Debug, Default, Clone)]
struct AgentTimeline {
    events: Vec<SubagentLifecycleEvent>,
    states: Vec<String>,
    has_spawn: bool,
    has_activity: bool,
    last_update: Option<String>,
}

#[derive(Debug, Clone)]
struct ClaudeAgentRecord {
    agent_id: String,
    path: PathBuf,
    status: String,
    last_update: Option<String>,
    relation: SubagentRelation,
    excerpt: Vec<SubagentExcerptMessage>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct GeminiChatRecord {
    session_id: String,
    path: PathBuf,
    last_update: Option<String>,
    status: String,
    explicit_parent_ids: Vec<String>,
}

#[derive(Debug, Clone)]
struct GeminiLogEntry {
    session_id: String,
    message: Option<String>,
    timestamp: Option<String>,
    entry_type: Option<String>,
    explicit_parent_ids: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct GeminiChildRecord {
    relation: SubagentRelation,
    relation_timestamp: Option<String>,
}

#[derive(Debug, Clone)]
struct AmpHandoff {
    thread_id: String,
    role: Option<String>,
    timestamp: Option<String>,
}

#[derive(Debug, Clone)]
struct AmpChildAnalysis {
    thread: SubagentThreadRef,
    status: String,
    status_source: String,
    excerpt: Vec<SubagentExcerptMessage>,
    lifecycle: Vec<SubagentLifecycleEvent>,
    relation_evidence: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PiSessionHintKind {
    Parent,
    Child,
}

#[derive(Debug, Clone)]
struct PiSessionHint {
    kind: PiSessionHintKind,
    session_id: String,
    evidence: String,
}

#[derive(Debug, Clone)]
struct PiSessionRecord {
    session_id: String,
    path: PathBuf,
    last_update: Option<String>,
    hints: Vec<PiSessionHint>,
}

#[derive(Debug, Clone)]
struct PiDiscoveredChild {
    relation: SubagentRelation,
    status: String,
    status_source: String,
    last_update: Option<String>,
    child_thread: Option<SubagentThreadRef>,
    excerpt: Vec<SubagentExcerptMessage>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct OpencodeAgentRecord {
    agent_id: String,
    relation: SubagentRelation,
    message_count: usize,
}

#[derive(Debug, Clone)]
struct OpencodeChildAnalysis {
    child_thread: Option<SubagentThreadRef>,
    status: String,
    status_source: String,
    last_update: Option<String>,
    excerpt: Vec<SubagentExcerptMessage>,
    warnings: Vec<String>,
}

impl Default for PiDiscoveredChild {
    fn default() -> Self {
        Self {
            relation: SubagentRelation::default(),
            status: STATUS_NOT_FOUND.to_string(),
            status_source: "inferred".to_string(),
            last_update: None,
            child_thread: None,
            excerpt: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

pub fn resolve_thread(uri: &AgentsUri, roots: &ProviderRoots) -> Result<ResolvedThread> {
    let session_id = uri.require_session_id()?;
    match uri.provider {
        ProviderKind::Amp => AmpProvider::new(&roots.amp_root).resolve(session_id),
        ProviderKind::Codex => CodexProvider::new(&roots.codex_root).resolve(session_id),
        ProviderKind::Claude => ClaudeProvider::new(&roots.claude_root).resolve(session_id),
        ProviderKind::Gemini => GeminiProvider::new(&roots.gemini_root).resolve(session_id),
        ProviderKind::Pi => PiProvider::new(&roots.pi_root).resolve(session_id),
        ProviderKind::Opencode => OpencodeProvider::new(&roots.opencode_root).resolve(session_id),
    }
}

pub fn write_thread(
    provider: ProviderKind,
    roots: &ProviderRoots,
    req: &WriteRequest,
    sink: &mut dyn WriteEventSink,
) -> Result<WriteResult> {
    match provider {
        ProviderKind::Amp => AmpProvider::new(&roots.amp_root).write(req, sink),
        ProviderKind::Codex => CodexProvider::new(&roots.codex_root).write(req, sink),
        ProviderKind::Claude => ClaudeProvider::new(&roots.claude_root).write(req, sink),
        ProviderKind::Gemini => GeminiProvider::new(&roots.gemini_root).write(req, sink),
        ProviderKind::Pi => PiProvider::new(&roots.pi_root).write(req, sink),
        ProviderKind::Opencode => OpencodeProvider::new(&roots.opencode_root).write(req, sink),
    }
}

#[derive(Debug, Clone)]
enum QuerySearchTarget {
    File(PathBuf),
    Text(String),
}

#[derive(Debug, Clone)]
struct QueryCandidate {
    provider: ProviderKind,
    thread_id: String,
    uri: String,
    thread_source: String,
    updated_at: Option<String>,
    updated_epoch: Option<u64>,
    scope_path: Option<PathBuf>,
    search_target: QuerySearchTarget,
}

pub fn query_threads(query: &ThreadQuery, roots: &ProviderRoots) -> Result<ThreadQueryResult> {
    let mut warnings = query
        .ignored_params
        .iter()
        .map(|key| format!("ignored query parameter: {key}"))
        .collect::<Vec<_>>();

    let mut candidates = match query.provider {
        ProviderKind::Amp => collect_amp_query_candidates(roots, &mut warnings),
        ProviderKind::Codex => collect_codex_query_candidates(roots, &mut warnings),
        ProviderKind::Claude => collect_claude_query_candidates(roots, &mut warnings),
        ProviderKind::Gemini => collect_gemini_query_candidates(roots, &mut warnings),
        ProviderKind::Pi => collect_pi_query_candidates(roots, &mut warnings),
        ProviderKind::Opencode => collect_opencode_query_candidates(
            roots,
            &mut warnings,
            query.q.as_deref().is_some_and(|q| !q.trim().is_empty())
                || query
                    .role
                    .as_deref()
                    .is_some_and(|role| !role.trim().is_empty()),
        )?,
    };

    candidates.sort_by_key(|candidate| Reverse(candidate.updated_epoch.unwrap_or(0)));

    if query.limit == 0 {
        return Ok(ThreadQueryResult {
            query: query.clone(),
            items: Vec::new(),
            warnings,
        });
    }

    let role_filter = query
        .role
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty());
    let keyword_filter = query.q.as_deref().map(str::trim).filter(|q| !q.is_empty());
    let mut items = Vec::new();
    for candidate in &candidates {
        if items.len() >= query.limit {
            break;
        }

        let mut role_preview = None::<String>;
        if let Some(role_filter) = role_filter {
            role_preview = match_candidate_preview(candidate, role_filter)?;
            if role_preview.is_none() {
                continue;
            }
        }

        let matched_preview = if let Some(keyword_filter) = keyword_filter {
            let matched_preview = match_candidate_preview(candidate, keyword_filter)?;
            if matched_preview.is_none() {
                continue;
            }
            matched_preview
        } else {
            role_preview
        };

        items.push(ThreadQueryItem {
            provider: candidate.provider,
            thread_id: candidate.thread_id.clone(),
            uri: candidate.uri.clone(),
            thread_source: candidate.thread_source.clone(),
            updated_at: candidate.updated_at.clone(),
            matched_preview,
            thread_metadata: match &candidate.search_target {
                QuerySearchTarget::File(path) => {
                    collect_query_thread_metadata(query.provider, path)
                }
                QuerySearchTarget::Text(_) => None,
            },
        });
    }

    Ok(ThreadQueryResult {
        query: query.clone(),
        items,
        warnings,
    })
}

pub fn query_threads_by_path(
    query: &PathThreadQuery,
    roots: &ProviderRoots,
) -> Result<PathThreadQueryResult> {
    let mut warnings = query
        .ignored_params
        .iter()
        .map(|key| format!("ignored query parameter: {key}"))
        .collect::<Vec<_>>();

    let providers = query.providers.clone().unwrap_or_else(all_provider_kinds);
    let keyword_filter = query.q.as_deref().map(str::trim).filter(|q| !q.is_empty());
    let requested_path = PathBuf::from(&query.scope_path);
    let mut candidates = Vec::new();
    for provider in providers.iter().copied() {
        candidates.extend(collect_candidates_for_provider(
            provider,
            roots,
            &mut warnings,
            keyword_filter.is_some(),
        )?);
    }

    candidates.retain(|candidate| {
        candidate
            .scope_path
            .as_deref()
            .is_some_and(|scope_path| path_matches_scope(scope_path, &requested_path))
    });
    candidates.sort_by_key(|candidate| Reverse(candidate.updated_epoch.unwrap_or(0)));

    if query.limit == 0 {
        return Ok(PathThreadQueryResult {
            query: query.clone(),
            items: Vec::new(),
            warnings,
        });
    }

    let mut items = Vec::new();
    for candidate in &candidates {
        if items.len() >= query.limit {
            break;
        }

        let matched_preview = if let Some(keyword_filter) = keyword_filter {
            let matched_preview = match_candidate_preview(candidate, keyword_filter)?;
            if matched_preview.is_none() {
                continue;
            }
            matched_preview
        } else {
            None
        };

        items.push(ThreadQueryItem {
            provider: candidate.provider,
            thread_id: candidate.thread_id.clone(),
            uri: candidate.uri.clone(),
            thread_source: candidate.thread_source.clone(),
            updated_at: candidate.updated_at.clone(),
            matched_preview,
            thread_metadata: match &candidate.search_target {
                QuerySearchTarget::File(path) => {
                    collect_query_thread_metadata(candidate.provider, path)
                }
                QuerySearchTarget::Text(_) => None,
            },
        });
    }

    Ok(PathThreadQueryResult {
        query: query.clone(),
        items,
        warnings,
    })
}

pub fn render_thread_query_head_markdown(result: &ThreadQueryResult) -> String {
    let mut output = String::new();
    output.push_str("---\n");
    push_yaml_string(&mut output, "uri", &result.query.uri);
    push_yaml_string(&mut output, "provider", &result.query.provider.to_string());
    push_yaml_string(&mut output, "mode", "thread_query");
    push_yaml_string(&mut output, "limit", &result.query.limit.to_string());
    if let Some(role) = &result.query.role {
        push_yaml_string(&mut output, "role", role);
    }

    if let Some(q) = &result.query.q {
        push_yaml_string(&mut output, "q", q);
    }

    output.push_str("threads:\n");
    if result.items.is_empty() {
        output.push_str("  []\n");
    } else {
        for item in &result.items {
            push_yaml_string_with_indent(&mut output, 2, "provider", &item.provider.to_string());
            push_yaml_string_with_indent(&mut output, 2, "thread_id", &item.thread_id);
            push_yaml_string_with_indent(&mut output, 2, "uri", &item.uri);
            push_yaml_string_with_indent(&mut output, 2, "thread_source", &item.thread_source);
            if let Some(updated_at) = &item.updated_at {
                push_yaml_string_with_indent(&mut output, 2, "updated_at", updated_at);
            }
            if let Some(matched_preview) = &item.matched_preview {
                push_yaml_string_with_indent(&mut output, 2, "matched_preview", matched_preview);
            }
            if let Some(thread_metadata) = &item.thread_metadata {
                render_thread_metadata_with_indent(&mut output, 2, thread_metadata);
            }
        }
    }

    render_warnings(&mut output, &result.warnings);
    output.push_str("---\n");
    output
}

pub fn render_thread_query_markdown(result: &ThreadQueryResult) -> String {
    let mut output = render_thread_query_head_markdown(result);
    output.push('\n');
    output.push_str("# Threads\n\n");
    output.push_str(&format!("- Provider: `{}`\n", result.query.provider));
    if let Some(role) = &result.query.role {
        output.push_str(&format!("- Role: `{}`\n", role));
    } else {
        output.push_str("- Role: `_none_`\n");
    }
    output.push_str(&format!("- Limit: `{}`\n", result.query.limit));
    if let Some(q) = &result.query.q {
        output.push_str(&format!("- Query: `{}`\n", q));
    } else {
        output.push_str("- Query: `_none_`\n");
    }
    output.push_str(&format!("- Matched: `{}`\n\n", result.items.len()));

    if result.items.is_empty() {
        output.push_str("_No threads found._\n");
        return output;
    }

    for (index, item) in result.items.iter().enumerate() {
        output.push_str(&format!("## {}. `{}`\n\n", index + 1, item.uri));
        output.push_str(&format!("- Provider: `{}`\n", item.provider));
        output.push_str(&format!("- Thread ID: `{}`\n", item.thread_id));
        output.push_str(&format!("- Thread Source: `{}`\n", item.thread_source));
        if let Some(updated_at) = &item.updated_at {
            output.push_str(&format!("- Updated At: `{}`\n", updated_at));
        }
        if let Some(matched_preview) = &item.matched_preview {
            output.push_str(&format!("- Match: `{}`\n", matched_preview));
        }
        output.push('\n');
    }

    output
}

pub fn render_path_thread_query_head_markdown(result: &PathThreadQueryResult) -> String {
    let mut output = String::new();
    output.push_str("---\n");
    push_yaml_string(&mut output, "uri", &result.query.uri);
    push_yaml_string(&mut output, "scope_path", &result.query.scope_path);
    push_yaml_string(&mut output, "mode", "path_thread_query");
    push_yaml_string(&mut output, "limit", &result.query.limit.to_string());
    if let Some(q) = &result.query.q {
        push_yaml_string(&mut output, "q", q);
    }
    render_provider_filter(&mut output, result.query.providers.as_deref());

    output.push_str("threads:\n");
    if result.items.is_empty() {
        output.push_str("  []\n");
    } else {
        for item in &result.items {
            push_yaml_string_with_indent(&mut output, 2, "provider", &item.provider.to_string());
            push_yaml_string_with_indent(&mut output, 2, "thread_id", &item.thread_id);
            push_yaml_string_with_indent(&mut output, 2, "uri", &item.uri);
            push_yaml_string_with_indent(&mut output, 2, "thread_source", &item.thread_source);
            if let Some(updated_at) = &item.updated_at {
                push_yaml_string_with_indent(&mut output, 2, "updated_at", updated_at);
            }
            if let Some(matched_preview) = &item.matched_preview {
                push_yaml_string_with_indent(&mut output, 2, "matched_preview", matched_preview);
            }
            if let Some(thread_metadata) = &item.thread_metadata {
                render_thread_metadata_with_indent(&mut output, 2, thread_metadata);
            }
        }
    }

    render_warnings(&mut output, &result.warnings);
    output.push_str("---\n");
    output
}

pub fn render_path_thread_query_markdown(result: &PathThreadQueryResult) -> String {
    let mut output = render_path_thread_query_head_markdown(result);
    output.push('\n');
    output.push_str("# Threads\n\n");
    output.push_str(&format!("- Scope Path: `{}`\n", result.query.scope_path));
    output.push_str(&format!(
        "- Providers: `{}`\n",
        format_provider_filter(result.query.providers.as_deref())
    ));
    output.push_str(&format!("- Limit: `{}`\n", result.query.limit));
    if let Some(q) = &result.query.q {
        output.push_str(&format!("- Query: `{}`\n", q));
    } else {
        output.push_str("- Query: `_none_`\n");
    }
    output.push_str(&format!("- Matched: `{}`\n\n", result.items.len()));

    if result.items.is_empty() {
        output.push_str("_No threads found._\n");
        return output;
    }

    for (index, item) in result.items.iter().enumerate() {
        output.push_str(&format!("## {}. `{}`\n\n", index + 1, item.uri));
        output.push_str(&format!("- Provider: `{}`\n", item.provider));
        output.push_str(&format!("- Thread ID: `{}`\n", item.thread_id));
        output.push_str(&format!("- Thread Source: `{}`\n", item.thread_source));
        if let Some(updated_at) = &item.updated_at {
            output.push_str(&format!("- Updated At: `{}`\n", updated_at));
        }
        if let Some(matched_preview) = &item.matched_preview {
            output.push_str(&format!("- Match: `{}`\n", matched_preview));
        }
        output.push('\n');
    }

    output
}

fn match_candidate_preview(candidate: &QueryCandidate, keyword: &str) -> Result<Option<String>> {
    match &candidate.search_target {
        QuerySearchTarget::File(path) => match_first_preview_in_file(path, keyword),
        QuerySearchTarget::Text(text) => Ok(match_first_preview_in_text(text, keyword)),
    }
}

fn match_first_preview_in_file(path: &Path, keyword: &str) -> Result<Option<String>> {
    let mut matcher_builder = RegexMatcherBuilder::new();
    matcher_builder.fixed_strings(true).case_insensitive(true);
    let matcher = matcher_builder
        .build(keyword)
        .map_err(|err| XurlError::InvalidMode(format!("invalid keyword query: {err}")))?;
    let mut searcher = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .line_number(true)
        .build();
    let mut preview = None::<String>;
    searcher
        .search_path(
            &matcher,
            path,
            Lossy(|_, line| {
                let line = line.trim();
                if line.is_empty() {
                    return Ok(true);
                }
                preview = Some(truncate_preview(line, 160));
                Ok(false)
            }),
        )
        .map_err(|source| XurlError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(preview)
}

fn match_first_preview_in_text(text: &str, keyword: &str) -> Option<String> {
    let matcher = RegexBuilder::new(&regex::escape(keyword))
        .case_insensitive(true)
        .build()
        .ok()?;
    let found = matcher.find(text)?;
    let line_start = text[..found.start()].rfind('\n').map_or(0, |idx| idx + 1);
    let line_end = text[found.end()..]
        .find('\n')
        .map_or(text.len(), |idx| found.end() + idx);
    let line = text[line_start..line_end].trim();
    if line.is_empty() {
        Some(truncate_preview(text, 160))
    } else {
        Some(truncate_preview(line, 160))
    }
}

fn read_thread_raw(path: &Path) -> Result<String> {
    let bytes = fs::read(path).map_err(|source| XurlError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    if bytes.is_empty() {
        return Err(XurlError::EmptyThreadFile {
            path: path.to_path_buf(),
        });
    }

    String::from_utf8(bytes).map_err(|_| XurlError::NonUtf8ThreadFile {
        path: path.to_path_buf(),
    })
}

pub fn render_thread_markdown(uri: &AgentsUri, resolved: &ResolvedThread) -> Result<String> {
    let raw = read_thread_raw(&resolved.path)?;
    let markdown = render::render_markdown(uri, &resolved.path, &raw)?;
    Ok(strip_frontmatter(markdown))
}

pub fn render_thread_head_markdown(uri: &AgentsUri, roots: &ProviderRoots) -> Result<String> {
    let mut output = String::new();
    output.push_str("---\n");
    push_yaml_string(&mut output, "uri", &uri.as_agents_string());
    push_yaml_string(&mut output, "provider", &uri.provider.to_string());
    push_yaml_string(&mut output, "session_id", &uri.session_id);

    match (uri.provider, uri.agent_id.as_deref()) {
        (
            ProviderKind::Amp
            | ProviderKind::Codex
            | ProviderKind::Claude
            | ProviderKind::Gemini
            | ProviderKind::Opencode,
            None,
        ) => {
            let resolved_main = resolve_thread(uri, roots)?;
            push_yaml_string(
                &mut output,
                "thread_source",
                &resolved_main.path.display().to_string(),
            );
            let (thread_metadata, metadata_warnings) =
                collect_thread_metadata(uri.provider, &resolved_main.path);
            render_thread_metadata(&mut output, &thread_metadata);
            push_yaml_string(&mut output, "mode", "subagent_index");

            let view = resolve_subagent_view(uri, roots, true)?;
            let mut warnings = resolved_main.metadata.warnings.clone();
            warnings.extend(metadata_warnings);

            if let SubagentView::List(list) = view {
                render_subagents_head(&mut output, &list);
                warnings.extend(list.warnings);
            }

            render_warnings(&mut output, &warnings);
        }
        (ProviderKind::Pi, None) => {
            let resolved = resolve_thread(uri, roots)?;
            push_yaml_string(
                &mut output,
                "thread_source",
                &resolved.path.display().to_string(),
            );
            let (thread_metadata, metadata_warnings) =
                collect_thread_metadata(uri.provider, &resolved.path);
            render_thread_metadata(&mut output, &thread_metadata);
            push_yaml_string(&mut output, "mode", "pi_entry_index");

            let list = resolve_pi_entry_list_view(uri, roots)?;
            render_pi_entries_head(&mut output, &list);
            let mut warnings = resolved.metadata.warnings.clone();
            warnings.extend(metadata_warnings);
            warnings.extend(list.warnings);

            if let SubagentView::List(subagents) = resolve_subagent_view(uri, roots, true)? {
                render_subagents_head(&mut output, &subagents);
                warnings.extend(subagents.warnings);
            }

            render_warnings(&mut output, &warnings);
        }
        (
            ProviderKind::Amp
            | ProviderKind::Codex
            | ProviderKind::Claude
            | ProviderKind::Gemini
            | ProviderKind::Opencode,
            Some(_),
        ) => {
            let main_uri = main_thread_uri(uri);
            let resolved_main = resolve_thread(&main_uri, roots)?;

            let view = resolve_subagent_view(uri, roots, false)?;
            if let SubagentView::Detail(detail) = view {
                let thread_source = detail
                    .child_thread
                    .as_ref()
                    .and_then(|thread| thread.path.as_deref())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| resolved_main.path.display().to_string());
                let (thread_metadata, metadata_warnings) =
                    collect_thread_metadata(uri.provider, Path::new(&thread_source));
                push_yaml_string(&mut output, "thread_source", &thread_source);
                render_thread_metadata(&mut output, &thread_metadata);
                push_yaml_string(&mut output, "mode", "subagent_detail");

                if let Some(agent_id) = &detail.query.agent_id {
                    push_yaml_string(&mut output, "agent_id", agent_id);
                    push_yaml_string(
                        &mut output,
                        "subagent_uri",
                        &agents_thread_uri(
                            &detail.query.provider,
                            &detail.query.main_thread_id,
                            Some(agent_id),
                        ),
                    );
                }
                push_yaml_string(&mut output, "status", &detail.status);
                push_yaml_string(&mut output, "status_source", &detail.status_source);

                if let Some(child_thread) = &detail.child_thread {
                    push_yaml_string(&mut output, "child_thread_id", &child_thread.thread_id);
                    if let Some(path) = &child_thread.path {
                        push_yaml_string(&mut output, "child_thread_source", path);
                    }
                    if let Some(last_updated_at) = &child_thread.last_updated_at {
                        push_yaml_string(&mut output, "child_last_updated_at", last_updated_at);
                    }
                }

                let mut warnings = detail.warnings.clone();
                warnings.extend(metadata_warnings);
                render_warnings(&mut output, &warnings);
            }
        }
        (ProviderKind::Pi, Some(agent_id)) if is_uuid_session_id(agent_id) => {
            let main_uri = main_thread_uri(uri);
            let resolved_main = resolve_thread(&main_uri, roots)?;

            let view = resolve_subagent_view(uri, roots, false)?;
            if let SubagentView::Detail(detail) = view {
                let thread_source = detail
                    .child_thread
                    .as_ref()
                    .and_then(|thread| thread.path.as_deref())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| resolved_main.path.display().to_string());
                let (thread_metadata, metadata_warnings) =
                    collect_thread_metadata(uri.provider, Path::new(&thread_source));
                push_yaml_string(&mut output, "thread_source", &thread_source);
                render_thread_metadata(&mut output, &thread_metadata);
                push_yaml_string(&mut output, "mode", "subagent_detail");
                push_yaml_string(&mut output, "agent_id", agent_id);
                push_yaml_string(
                    &mut output,
                    "subagent_uri",
                    &agents_thread_uri("pi", &uri.session_id, Some(agent_id)),
                );
                push_yaml_string(&mut output, "status", &detail.status);
                push_yaml_string(&mut output, "status_source", &detail.status_source);

                if let Some(child_thread) = &detail.child_thread {
                    push_yaml_string(&mut output, "child_thread_id", &child_thread.thread_id);
                    if let Some(path) = &child_thread.path {
                        push_yaml_string(&mut output, "child_thread_source", path);
                    }
                    if let Some(last_updated_at) = &child_thread.last_updated_at {
                        push_yaml_string(&mut output, "child_last_updated_at", last_updated_at);
                    }
                }

                let mut warnings = detail.warnings.clone();
                warnings.extend(metadata_warnings);
                render_warnings(&mut output, &warnings);
            }
        }
        (ProviderKind::Pi, Some(entry_id)) => {
            let resolved = resolve_thread(uri, roots)?;
            let (thread_metadata, metadata_warnings) =
                collect_thread_metadata(uri.provider, &resolved.path);
            push_yaml_string(
                &mut output,
                "thread_source",
                &resolved.path.display().to_string(),
            );
            render_thread_metadata(&mut output, &thread_metadata);
            push_yaml_string(&mut output, "mode", "pi_entry");
            push_yaml_string(&mut output, "entry_id", entry_id);
            let mut warnings = resolved.metadata.warnings.clone();
            warnings.extend(metadata_warnings);
            render_warnings(&mut output, &warnings);
        }
    }

    output.push_str("---\n");
    Ok(output)
}

pub fn resolve_subagent_view(
    uri: &AgentsUri,
    roots: &ProviderRoots,
    list: bool,
) -> Result<SubagentView> {
    if list && uri.agent_id.is_some() {
        return Err(XurlError::InvalidMode(
            "subagent index mode requires agents://<provider>/<main_thread_id>".to_string(),
        ));
    }

    if !list && uri.agent_id.is_none() {
        return Err(XurlError::InvalidMode(
            "subagent drill-down requires agents://<provider>/<main_thread_id>/<agent_id>"
                .to_string(),
        ));
    }

    match uri.provider {
        ProviderKind::Amp => resolve_amp_subagent_view(uri, roots, list),
        ProviderKind::Codex => resolve_codex_subagent_view(uri, roots, list),
        ProviderKind::Claude => resolve_claude_subagent_view(uri, roots, list),
        ProviderKind::Gemini => resolve_gemini_subagent_view(uri, roots, list),
        ProviderKind::Pi => resolve_pi_subagent_view(uri, roots, list),
        ProviderKind::Opencode => resolve_opencode_subagent_view(uri, roots, list),
    }
}

fn push_yaml_string(output: &mut String, key: &str, value: &str) {
    output.push_str(&format!("{key}: '{}'\n", yaml_single_quoted(value)));
}

fn render_provider_filter(output: &mut String, providers: Option<&[ProviderKind]>) {
    output.push_str("providers:\n");
    if let Some(providers) = providers {
        for provider in providers {
            output.push_str(&format!(
                "  - '{}'\n",
                yaml_single_quoted(&provider.to_string())
            ));
        }
    } else {
        output.push_str("  - 'all'\n");
    }
}

fn format_provider_filter(providers: Option<&[ProviderKind]>) -> String {
    providers.map_or_else(
        || "all".to_string(),
        |providers| {
            providers
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        },
    )
}

fn yaml_single_quoted(value: &str) -> String {
    value.replace('\'', "''")
}

fn render_warnings(output: &mut String, warnings: &[String]) {
    let mut unique = BTreeSet::<String>::new();
    unique.extend(warnings.iter().cloned());

    if unique.is_empty() {
        return;
    }

    output.push_str("warnings:\n");
    for warning in unique {
        output.push_str(&format!("  - '{}'\n", yaml_single_quoted(&warning)));
    }
}

fn render_thread_metadata(output: &mut String, metadata: &[String]) {
    if metadata.is_empty() {
        return;
    }
    render_thread_metadata_with_indent(output, 0, metadata);
}

fn render_thread_metadata_with_indent(output: &mut String, indent: usize, metadata: &[String]) {
    if metadata.is_empty() {
        return;
    }

    let prefix = " ".repeat(indent);
    output.push_str(&format!("{prefix}thread_metadata:\n"));
    for value in metadata {
        output.push_str(&format!("{prefix}  - '{}'\n", yaml_single_quoted(value)));
    }
}

fn collect_thread_metadata(provider: ProviderKind, path: &Path) -> (Vec<String>, Vec<String>) {
    let raw = match read_thread_raw(path) {
        Ok(raw) => raw,
        Err(err) => {
            return (
                Vec::new(),
                vec![format!(
                    "failed reading thread metadata {}: {err}",
                    path.display()
                )],
            );
        }
    };

    match provider {
        ProviderKind::Amp => collect_amp_thread_metadata(path, &raw),
        ProviderKind::Codex => collect_codex_thread_metadata(path, &raw),
        ProviderKind::Claude => collect_claude_thread_metadata(path, &raw),
        ProviderKind::Gemini => collect_gemini_thread_metadata(path, &raw),
        ProviderKind::Pi => collect_pi_thread_metadata(path, &raw),
        ProviderKind::Opencode => collect_opencode_thread_metadata(path, &raw),
    }
}

fn collect_query_thread_metadata(provider: ProviderKind, path: &Path) -> Option<Vec<String>> {
    let metadata = match provider {
        ProviderKind::Codex => {
            collect_query_jsonl_thread_metadata(path, |value, metadata, seen| {
                match value.get("type").and_then(Value::as_str) {
                    Some("session_meta") | Some("turn_context") => {
                        push_thread_metadata_record(metadata, seen, &value)
                    }
                    _ => false,
                }
            })
        }
        ProviderKind::Claude => {
            collect_query_jsonl_thread_metadata(path, |value, metadata, seen| {
                if looks_like_claude_metadata(&value) {
                    let mut metadata_value = value;
                    if let Some(object) = metadata_value.as_object_mut() {
                        object.remove("message");
                    }
                    push_thread_metadata_record(metadata, seen, &metadata_value)
                } else {
                    false
                }
            })
        }
        ProviderKind::Pi => collect_query_jsonl_thread_metadata(path, |value, metadata, seen| {
            match value.get("type").and_then(Value::as_str) {
                Some("session") | Some("model_change") | Some("thinking_level_change") => {
                    push_thread_metadata_record(metadata, seen, &value)
                }
                _ => false,
            }
        }),
        ProviderKind::Amp | ProviderKind::Gemini | ProviderKind::Opencode => {
            collect_thread_metadata(provider, path).0
        }
    };

    if metadata.is_empty() {
        None
    } else {
        Some(metadata)
    }
}

fn collect_query_jsonl_thread_metadata<F>(path: &Path, mut on_value: F) -> Vec<String>
where
    F: FnMut(Value, &mut Vec<String>, &mut BTreeSet<String>) -> bool,
{
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };

    let reader = BufReader::new(file);
    let mut metadata = Vec::new();
    let mut seen = BTreeSet::<String>::new();

    for line in reader.lines().take(QUERY_METADATA_LINE_BUDGET) {
        let Ok(line) = line else {
            break;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };

        if on_value(value, &mut metadata, &mut seen) {
            break;
        }
    }

    metadata
}

fn collect_codex_thread_metadata(path: &Path, raw: &str) -> (Vec<String>, Vec<String>) {
    let mut metadata = Vec::new();
    let mut warnings = Vec::new();
    let mut seen = BTreeSet::<String>::new();

    for (line_idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let value = match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => value,
            Err(err) => {
                warnings.push(format!(
                    "failed parsing codex metadata line {} in {}: {err}",
                    line_idx + 1,
                    path.display()
                ));
                continue;
            }
        };

        match value.get("type").and_then(Value::as_str) {
            Some("session_meta") | Some("turn_context") => {
                if push_thread_metadata_record(&mut metadata, &mut seen, &value) {
                    break;
                }
            }
            _ => {}
        }
    }

    (metadata, warnings)
}

fn collect_claude_thread_metadata(path: &Path, raw: &str) -> (Vec<String>, Vec<String>) {
    let mut metadata = Vec::new();
    let mut warnings = Vec::new();
    let mut seen = BTreeSet::<String>::new();

    for (line_idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let value = match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => value,
            Err(err) => {
                warnings.push(format!(
                    "failed parsing claude metadata line {} in {}: {err}",
                    line_idx + 1,
                    path.display()
                ));
                continue;
            }
        };

        if looks_like_claude_metadata(&value) {
            let mut metadata_value = value;
            if let Some(object) = metadata_value.as_object_mut() {
                object.remove("message");
            }
            if push_thread_metadata_record(&mut metadata, &mut seen, &metadata_value) {
                break;
            }
        }
    }

    (metadata, warnings)
}

fn collect_pi_thread_metadata(path: &Path, raw: &str) -> (Vec<String>, Vec<String>) {
    let mut metadata = Vec::new();
    let mut warnings = Vec::new();
    let mut seen = BTreeSet::<String>::new();

    for (line_idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let value = match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => value,
            Err(err) => {
                warnings.push(format!(
                    "failed parsing pi metadata line {} in {}: {err}",
                    line_idx + 1,
                    path.display()
                ));
                continue;
            }
        };

        match value.get("type").and_then(Value::as_str) {
            Some("session") | Some("model_change") | Some("thinking_level_change") => {
                if push_thread_metadata_record(&mut metadata, &mut seen, &value) {
                    break;
                }
            }
            _ => {}
        }
    }

    (metadata, warnings)
}

fn collect_amp_thread_metadata(path: &Path, raw: &str) -> (Vec<String>, Vec<String>) {
    collect_json_object_thread_metadata(path, raw, ProviderKind::Amp, &["messages"])
}

fn collect_gemini_thread_metadata(path: &Path, raw: &str) -> (Vec<String>, Vec<String>) {
    collect_json_object_thread_metadata(path, raw, ProviderKind::Gemini, &["messages"])
}

fn collect_opencode_thread_metadata(_path: &Path, raw: &str) -> (Vec<String>, Vec<String>) {
    let mut metadata = Vec::new();
    let mut seen = BTreeSet::<String>::new();

    if let Some(first_non_empty) = raw.lines().find(|line| !line.trim().is_empty())
        && let Ok(value) = serde_json::from_str::<Value>(first_non_empty)
        && value.get("type").and_then(Value::as_str) == Some("session")
    {
        let _ = push_thread_metadata_record(&mut metadata, &mut seen, &value);
    }

    (metadata, Vec::new())
}

fn collect_json_object_thread_metadata(
    path: &Path,
    raw: &str,
    provider: ProviderKind,
    strip_keys: &[&str],
) -> (Vec<String>, Vec<String>) {
    let mut metadata = Vec::new();
    let mut seen = BTreeSet::<String>::new();
    let value = match serde_json::from_str::<Value>(raw) {
        Ok(value) => value,
        Err(err) => {
            return (
                metadata,
                vec![format!(
                    "failed parsing {provider} metadata payload {}: {err}",
                    path.display()
                )],
            );
        }
    };

    let mut metadata_value = value;
    if let Some(object) = metadata_value.as_object_mut() {
        for key in strip_keys {
            object.remove(*key);
        }
    }

    if !metadata_value.is_null() {
        let should_emit = metadata_value
            .as_object()
            .is_none_or(|object| !object.is_empty());
        if should_emit {
            let _ = push_thread_metadata_record(&mut metadata, &mut seen, &metadata_value);
        }
    }

    (metadata, Vec::new())
}

fn looks_like_claude_metadata(value: &Value) -> bool {
    value.get("cwd").is_some()
        || value.get("gitBranch").is_some()
        || value.get("version").is_some()
        || value.get("sessionId").is_some()
        || value.get("agentId").is_some()
        || value.get("isSidechain").is_some()
}

fn scope_path_from_str(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

fn extract_json_string_at_paths<'a>(value: &'a Value, paths: &[&[&str]]) -> Option<&'a str> {
    for path in paths {
        let mut current = value;
        let mut found = true;
        for key in *path {
            let Some(next) = current.get(*key) else {
                found = false;
                break;
            };
            current = next;
        }
        if found && let Some(text) = current.as_str() {
            return Some(text);
        }
    }

    None
}

fn find_first_string_by_key<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    match value {
        Value::Object(map) => {
            if let Some(text) = map.get(key).and_then(Value::as_str) {
                return Some(text);
            }
            for child in map.values() {
                if let Some(text) = find_first_string_by_key(child, key) {
                    return Some(text);
                }
            }
            None
        }
        Value::Array(items) => items
            .iter()
            .find_map(|item| find_first_string_by_key(item, key)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

fn extract_json_scope_path(
    path: &Path,
    field_paths: &[&[&str]],
    fallback_keys: &[&str],
) -> Option<PathBuf> {
    let raw = fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<Value>(&raw).ok()?;
    extract_json_string_at_paths(&value, field_paths)
        .and_then(scope_path_from_str)
        .or_else(|| {
            fallback_keys
                .iter()
                .find_map(|key| find_first_string_by_key(&value, key))
                .and_then(scope_path_from_str)
        })
}

fn extract_codex_scope_path(path: &Path) -> Option<PathBuf> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(QUERY_METADATA_LINE_BUDGET).flatten() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value = serde_json::from_str::<Value>(trimmed).ok()?;
        match value.get("type").and_then(Value::as_str) {
            Some("session_meta") | Some("turn_context") => {
                if let Some(text) =
                    extract_json_string_at_paths(&value, &[&["payload", "cwd"], &["cwd"]])
                {
                    return scope_path_from_str(text);
                }
            }
            _ => {}
        }
    }
    None
}

fn extract_claude_scope_path(path: &Path) -> Option<PathBuf> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(QUERY_METADATA_LINE_BUDGET).flatten() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value = serde_json::from_str::<Value>(trimmed).ok()?;
        if looks_like_claude_metadata(&value)
            && let Some(text) = extract_json_string_at_paths(
                &value,
                &[&["cwd"], &["projectPath"], &["originalPath"]],
            )
        {
            return scope_path_from_str(text);
        }
    }
    None
}

fn extract_pi_scope_path(path: &Path) -> Option<PathBuf> {
    let file = fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let value = serde_json::from_str::<Value>(line.trim()).ok()?;
    value
        .get("cwd")
        .and_then(Value::as_str)
        .and_then(scope_path_from_str)
}

fn extract_amp_scope_path(path: &Path) -> Option<PathBuf> {
    extract_json_scope_path(path, &[&["cwd"]], &["cwd"])
}

fn extract_gemini_scope_path(path: &Path) -> Option<PathBuf> {
    if let Some(project_root_marker_path) = path
        .ancestors()
        .skip(1)
        .map(|ancestor| ancestor.join(".project_root"))
        .find(|candidate| candidate.exists())
        && let Ok(contents) = fs::read_to_string(project_root_marker_path)
        && let Some(scope_path) = scope_path_from_str(contents.trim())
    {
        return Some(scope_path);
    }

    extract_json_scope_path(path, &[&["projectRoot"], &["cwd"]], &["projectRoot", "cwd"])
}

fn push_thread_metadata_record(
    metadata: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    value: &Value,
) -> bool {
    let before = metadata.len();
    flatten_thread_metadata_value(metadata, seen, None, value);
    metadata.len() > before
}

fn flatten_thread_metadata_value(
    metadata: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    path: Option<&str>,
    value: &Value,
) {
    if let Some(path) = path
        && should_ignore_thread_metadata_path(path)
    {
        return;
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            let Some(path) = path else {
                return;
            };
            let entry = format!("{path} = {}", format_thread_metadata_value(value));
            if seen.insert(entry.clone()) {
                metadata.push(entry);
            }
        }
        Value::Array(items) => {
            let Some(path) = path else {
                return;
            };
            if items.is_empty() {
                let entry = format!("{path} = []");
                if seen.insert(entry.clone()) {
                    metadata.push(entry);
                }
                return;
            }

            for (index, item) in items.iter().enumerate() {
                let child_path = format!("{path}[{index}]");
                flatten_thread_metadata_value(metadata, seen, Some(&child_path), item);
            }
        }
        Value::Object(map) => {
            if map.is_empty() {
                if let Some(path) = path {
                    let entry = format!("{path} = {{}}");
                    if seen.insert(entry.clone()) {
                        metadata.push(entry);
                    }
                }
                return;
            }

            for (key, child) in map {
                let child_path = match path {
                    Some(path) => format!("{path}.{key}"),
                    None => key.clone(),
                };
                flatten_thread_metadata_value(metadata, seen, Some(&child_path), child);
            }
        }
    }
}

fn should_ignore_thread_metadata_path(path: &str) -> bool {
    const IGNORED_PREFIXES: &[&str] = &[
        "base_instructions",
        "user_instructions",
        "developer_instructions",
        "payload.base_instructions",
        "payload.user_instructions",
        "payload.developer_instructions",
    ];

    IGNORED_PREFIXES.iter().any(|prefix| {
        path == *prefix
            || path.starts_with(&format!("{prefix}."))
            || path.starts_with(&format!("{prefix}["))
    })
}
fn format_thread_metadata_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => format_thread_metadata_string(text),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn all_provider_kinds() -> Vec<ProviderKind> {
    vec![
        ProviderKind::Amp,
        ProviderKind::Codex,
        ProviderKind::Claude,
        ProviderKind::Gemini,
        ProviderKind::Pi,
        ProviderKind::Opencode,
    ]
}

fn path_matches_scope(scope_path: &Path, requested_path: &Path) -> bool {
    scope_path == requested_path || scope_path.starts_with(requested_path)
}

fn collect_candidates_for_provider(
    provider: ProviderKind,
    roots: &ProviderRoots,
    warnings: &mut Vec<String>,
    with_search_text: bool,
) -> Result<Vec<QueryCandidate>> {
    match provider {
        ProviderKind::Amp => Ok(collect_amp_query_candidates(roots, warnings)),
        ProviderKind::Codex => Ok(collect_codex_query_candidates(roots, warnings)),
        ProviderKind::Claude => Ok(collect_claude_query_candidates(roots, warnings)),
        ProviderKind::Gemini => Ok(collect_gemini_query_candidates(roots, warnings)),
        ProviderKind::Pi => Ok(collect_pi_query_candidates(roots, warnings)),
        ProviderKind::Opencode => {
            collect_opencode_query_candidates(roots, warnings, with_search_text)
        }
    }
}

fn format_thread_metadata_string(text: &str) -> String {
    if text.is_empty()
        || text.contains('\n')
        || text.starts_with(char::is_whitespace)
        || text.ends_with(char::is_whitespace)
    {
        serde_json::to_string(text).unwrap_or_else(|_| text.to_string())
    } else {
        text.to_string()
    }
}

fn render_subagents_head(output: &mut String, list: &SubagentListView) {
    output.push_str("subagents:\n");
    if list.agents.is_empty() {
        output.push_str("  []\n");
        return;
    }

    for agent in &list.agents {
        output.push_str(&format!(
            "  - agent_id: '{}'\n",
            yaml_single_quoted(&agent.agent_id)
        ));
        output.push_str(&format!(
            "    uri: '{}'\n",
            yaml_single_quoted(&agents_thread_uri(
                &list.query.provider,
                &list.query.main_thread_id,
                Some(&agent.agent_id),
            ))
        ));
        push_yaml_string_with_indent(output, 4, "status", &agent.status);
        push_yaml_string_with_indent(output, 4, "status_source", &agent.status_source);
        if let Some(last_update) = &agent.last_update {
            push_yaml_string_with_indent(output, 4, "last_update", last_update);
        }
        if let Some(child_thread) = &agent.child_thread
            && let Some(path) = &child_thread.path
        {
            push_yaml_string_with_indent(output, 4, "thread_source", path);
        }
    }
}

fn render_pi_entries_head(output: &mut String, list: &PiEntryListView) {
    output.push_str("entries:\n");
    if list.entries.is_empty() {
        output.push_str("  []\n");
        return;
    }

    for entry in &list.entries {
        output.push_str(&format!(
            "  - entry_id: '{}'\n",
            yaml_single_quoted(&entry.entry_id)
        ));
        output.push_str(&format!(
            "    uri: '{}'\n",
            yaml_single_quoted(&agents_thread_uri(
                &list.query.provider,
                &list.query.session_id,
                Some(&entry.entry_id),
            ))
        ));
        push_yaml_string_with_indent(output, 4, "entry_type", &entry.entry_type);
        if let Some(parent_id) = &entry.parent_id {
            push_yaml_string_with_indent(output, 4, "parent_id", parent_id);
        }
        if let Some(timestamp) = &entry.timestamp {
            push_yaml_string_with_indent(output, 4, "timestamp", timestamp);
        }
        if let Some(preview) = &entry.preview {
            push_yaml_string_with_indent(output, 4, "preview", preview);
        }
        push_yaml_bool_with_indent(output, 4, "is_leaf", entry.is_leaf);
    }
}

fn push_yaml_string_with_indent(output: &mut String, indent: usize, key: &str, value: &str) {
    output.push_str(&format!(
        "{}{key}: '{}'\n",
        " ".repeat(indent),
        yaml_single_quoted(value)
    ));
}

fn push_yaml_bool_with_indent(output: &mut String, indent: usize, key: &str, value: bool) {
    output.push_str(&format!("{}{key}: {value}\n", " ".repeat(indent)));
}

fn strip_frontmatter(markdown: String) -> String {
    let Some(rest) = markdown.strip_prefix("---\n") else {
        return markdown;
    };
    let Some((_, body)) = rest.split_once("\n---\n\n") else {
        return markdown;
    };
    body.to_string()
}

pub fn render_subagent_view_markdown(view: &SubagentView) -> String {
    match view {
        SubagentView::List(list_view) => render_subagent_list_markdown(list_view),
        SubagentView::Detail(detail_view) => render_subagent_detail_markdown(detail_view),
    }
}

pub fn resolve_pi_entry_list_view(
    uri: &AgentsUri,
    roots: &ProviderRoots,
) -> Result<PiEntryListView> {
    if uri.provider != ProviderKind::Pi {
        return Err(XurlError::InvalidMode(
            "pi entry listing requires agents://pi/<session_id> (legacy pi://<session_id> is also supported)".to_string(),
        ));
    }
    if uri.agent_id.is_some() {
        return Err(XurlError::InvalidMode(
            "pi entry index mode requires agents://pi/<session_id>".to_string(),
        ));
    }

    let resolved = resolve_thread(uri, roots)?;
    let raw = read_thread_raw(&resolved.path)?;

    let mut warnings = resolved.metadata.warnings;
    let mut entries = Vec::<PiEntryListItem>::new();
    let mut parent_ids = BTreeSet::<String>::new();

    for (line_idx, line) in raw.lines().enumerate() {
        let value = match jsonl::parse_json_line(Path::new("<pi:session>"), line_idx + 1, line) {
            Ok(Some(value)) => value,
            Ok(None) => continue,
            Err(err) => {
                warnings.push(format!(
                    "failed to parse pi session line {}: {err}",
                    line_idx + 1,
                ));
                continue;
            }
        };

        if value.get("type").and_then(Value::as_str) == Some("session") {
            continue;
        }

        let Some(entry_id) = value
            .get("id")
            .and_then(Value::as_str)
            .map(ToString::to_string)
        else {
            continue;
        };
        let parent_id = value
            .get("parentId")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        if let Some(parent_id) = &parent_id {
            parent_ids.insert(parent_id.clone());
        }

        let entry_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();

        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .map(ToString::to_string);

        let preview = match entry_type.as_str() {
            "message" => value
                .get("message")
                .and_then(|message| message.get("content"))
                .map(|content| render_preview_text(content, 96))
                .filter(|text| !text.is_empty()),
            "compaction" | "branch_summary" => value
                .get("summary")
                .and_then(Value::as_str)
                .map(|text| truncate_preview(text, 96))
                .filter(|text| !text.is_empty()),
            _ => None,
        };

        entries.push(PiEntryListItem {
            entry_id,
            entry_type,
            parent_id,
            timestamp,
            is_leaf: false,
            preview,
        });
    }

    for entry in &mut entries {
        entry.is_leaf = !parent_ids.contains(&entry.entry_id);
    }

    Ok(PiEntryListView {
        query: PiEntryQuery {
            provider: uri.provider.to_string(),
            session_id: uri.session_id.clone(),
            list: true,
        },
        entries,
        warnings,
    })
}

pub fn render_pi_entry_list_markdown(view: &PiEntryListView) -> String {
    let session_uri = agents_thread_uri(&view.query.provider, &view.query.session_id, None);
    let mut output = String::new();
    output.push_str("# Pi Session Entries\n\n");
    output.push_str(&format!("- Provider: `{}`\n", view.query.provider));
    output.push_str(&format!("- Session: `{}`\n", session_uri));
    output.push_str("- Mode: `list`\n\n");

    if view.entries.is_empty() {
        output.push_str("_No entries found in this session._\n");
        return output;
    }

    for (index, entry) in view.entries.iter().enumerate() {
        let entry_uri = format!("{session_uri}/{}", entry.entry_id);
        output.push_str(&format!("## {}. `{}`\n\n", index + 1, entry_uri));
        output.push_str(&format!("- Type: `{}`\n", entry.entry_type));
        output.push_str(&format!(
            "- Parent: `{}`\n",
            entry.parent_id.as_deref().unwrap_or("root")
        ));
        output.push_str(&format!(
            "- Timestamp: `{}`\n",
            entry.timestamp.as_deref().unwrap_or("unknown")
        ));
        output.push_str(&format!(
            "- Leaf: `{}`\n",
            if entry.is_leaf { "yes" } else { "no" }
        ));
        if let Some(preview) = &entry.preview {
            output.push_str(&format!("- Preview: {}\n", preview));
        }
        output.push('\n');
    }

    output
}

fn resolve_pi_subagent_view(
    uri: &AgentsUri,
    roots: &ProviderRoots,
    list: bool,
) -> Result<SubagentView> {
    if uri.provider != ProviderKind::Pi {
        return Err(XurlError::InvalidMode(
            "pi child-session view requires agents://pi/<main_session_id>/<child_session_id>"
                .to_string(),
        ));
    }

    if !list
        && uri
            .agent_id
            .as_deref()
            .is_some_and(|agent_id| !is_uuid_session_id(agent_id))
    {
        return Err(XurlError::InvalidMode(
            "pi child-session drill-down requires UUID child_session_id".to_string(),
        ));
    }

    let main_uri = main_thread_uri(uri);
    let resolved_main = resolve_thread(&main_uri, roots)?;
    let mut warnings = resolved_main.metadata.warnings.clone();

    let records = discover_pi_session_records(&roots.pi_root, &mut warnings);
    let main_record = records.get(&uri.session_id);
    let mut discovered = discover_pi_children(&uri.session_id, main_record, &records);

    if list {
        warnings.extend(
            discovered
                .values()
                .flat_map(|child| child.warnings.clone())
                .collect::<Vec<_>>(),
        );

        let agents = discovered
            .into_iter()
            .map(|(agent_id, child)| SubagentListItem {
                agent_id: agent_id.clone(),
                status: child.status,
                status_source: child.status_source,
                last_update: child.last_update,
                relation: child.relation,
                child_thread: child.child_thread,
            })
            .collect();

        return Ok(SubagentView::List(SubagentListView {
            query: make_query(uri, None, true),
            agents,
            warnings,
        }));
    }

    let requested_agent = uri
        .agent_id
        .clone()
        .ok_or_else(|| XurlError::InvalidMode("missing child session id".to_string()))?;

    if let Some(child) = discovered.remove(&requested_agent) {
        warnings.extend(child.warnings.clone());
        let lifecycle = child
            .relation
            .evidence
            .iter()
            .map(|evidence| SubagentLifecycleEvent {
                timestamp: child.last_update.clone(),
                event: "session_relation_hint".to_string(),
                detail: evidence.clone(),
            })
            .collect::<Vec<_>>();

        return Ok(SubagentView::Detail(SubagentDetailView {
            query: make_query(uri, Some(requested_agent), false),
            relation: child.relation,
            lifecycle,
            status: child.status,
            status_source: child.status_source,
            child_thread: child.child_thread,
            excerpt: child.excerpt,
            warnings,
        }));
    }

    if let Some(record) = records.get(&requested_agent) {
        warnings.push(format!(
            "child session file exists but no relation hint links it to main_session_id={} (child path: {})",
            uri.session_id,
            record.path.display()
        ));
    } else {
        warnings.push(format!(
            "child session not found for main_session_id={} child_session_id={requested_agent}",
            uri.session_id
        ));
    }

    Ok(SubagentView::Detail(SubagentDetailView {
        query: make_query(uri, Some(requested_agent), false),
        relation: SubagentRelation::default(),
        lifecycle: Vec::new(),
        status: STATUS_NOT_FOUND.to_string(),
        status_source: "inferred".to_string(),
        child_thread: None,
        excerpt: Vec::new(),
        warnings,
    }))
}

fn discover_pi_children(
    main_session_id: &str,
    main_record: Option<&PiSessionRecord>,
    records: &BTreeMap<String, PiSessionRecord>,
) -> BTreeMap<String, PiDiscoveredChild> {
    let mut children = BTreeMap::<String, PiDiscoveredChild>::new();

    for record in records.values() {
        for hint in record.hints.iter().filter(|hint| {
            hint.kind == PiSessionHintKind::Parent && hint.session_id == main_session_id
        }) {
            let child = children.entry(record.session_id.clone()).or_default();
            child.relation.validated = true;
            child.relation.evidence.push(format!(
                "{} (from {})",
                hint.evidence,
                record.path.display()
            ));
            child.last_update = child
                .last_update
                .clone()
                .or_else(|| record.last_update.clone());
            child.child_thread = Some(SubagentThreadRef {
                thread_id: record.session_id.clone(),
                path: Some(record.path.display().to_string()),
                last_updated_at: record.last_update.clone(),
            });
        }
    }

    if let Some(main_record) = main_record {
        for hint in main_record
            .hints
            .iter()
            .filter(|hint| hint.kind == PiSessionHintKind::Child)
        {
            let child = children.entry(hint.session_id.clone()).or_default();
            child.relation.validated = true;
            child.relation.evidence.push(format!(
                "{} (from {})",
                hint.evidence,
                main_record.path.display()
            ));

            if let Some(record) = records.get(&hint.session_id) {
                child.last_update = child
                    .last_update
                    .clone()
                    .or_else(|| record.last_update.clone());
                child.child_thread = Some(SubagentThreadRef {
                    thread_id: record.session_id.clone(),
                    path: Some(record.path.display().to_string()),
                    last_updated_at: record.last_update.clone(),
                });
            } else {
                child.status = STATUS_NOT_FOUND.to_string();
                child.status_source = "inferred".to_string();
                child.warnings.push(format!(
                    "relation hint references child_session_id={} but transcript file is missing for main_session_id={} ({})",
                    hint.session_id, main_session_id, hint.evidence
                ));
            }
        }
    }

    for (child_id, child) in &mut children {
        let Some(path) = child
            .child_thread
            .as_ref()
            .and_then(|thread| thread.path.as_deref())
            .map(ToString::to_string)
        else {
            continue;
        };

        match read_thread_raw(Path::new(&path)) {
            Ok(raw) => {
                if child.last_update.is_none() {
                    child.last_update = extract_last_timestamp(&raw);
                }

                let messages = render::extract_messages(ProviderKind::Pi, Path::new(&path), &raw)
                    .unwrap_or_default();

                let has_assistant = messages
                    .iter()
                    .any(|message| matches!(message.role, crate::model::MessageRole::Assistant));
                let has_user = messages
                    .iter()
                    .any(|message| matches!(message.role, crate::model::MessageRole::User));

                child.status = if has_assistant {
                    STATUS_COMPLETED.to_string()
                } else if has_user {
                    STATUS_RUNNING.to_string()
                } else {
                    STATUS_PENDING_INIT.to_string()
                };
                child.status_source = "child_rollout".to_string();
                child.excerpt = messages
                    .into_iter()
                    .rev()
                    .take(3)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .map(|message| SubagentExcerptMessage {
                        role: message.role,
                        text: message.text,
                    })
                    .collect();
            }
            Err(err) => {
                child.status = STATUS_NOT_FOUND.to_string();
                child.status_source = "inferred".to_string();
                child.warnings.push(format!(
                    "failed to read child session transcript for child_session_id={child_id}: {err}"
                ));
            }
        }
    }

    children
}

fn discover_pi_session_records(
    pi_root: &Path,
    warnings: &mut Vec<String>,
) -> BTreeMap<String, PiSessionRecord> {
    let sessions_root = pi_root.join("sessions");
    if !sessions_root.exists() {
        return BTreeMap::new();
    }

    let mut latest = BTreeMap::<String, (u64, PiSessionRecord)>::new();
    for entry in WalkDir::new(&sessions_root)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| {
            entry
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext == "jsonl")
        })
    {
        let path = entry.path();
        let Some(record) = parse_pi_session_record(path, warnings) else {
            continue;
        };

        let stamp = file_modified_epoch(path).unwrap_or(0);
        match latest.get(&record.session_id) {
            Some((existing_stamp, existing)) => {
                if stamp > *existing_stamp {
                    warnings.push(format!(
                        "multiple pi transcripts found for session_id={}; selected latest: {}",
                        record.session_id,
                        record.path.display()
                    ));
                    latest.insert(record.session_id.clone(), (stamp, record));
                } else {
                    warnings.push(format!(
                        "multiple pi transcripts found for session_id={}; kept latest: {}",
                        existing.session_id,
                        existing.path.display()
                    ));
                }
            }
            None => {
                latest.insert(record.session_id.clone(), (stamp, record));
            }
        }
    }

    latest
        .into_values()
        .map(|(_, record)| (record.session_id.clone(), record))
        .collect()
}

fn parse_pi_session_record(path: &Path, warnings: &mut Vec<String>) -> Option<PiSessionRecord> {
    let raw = match read_thread_raw(path) {
        Ok(raw) => raw,
        Err(err) => {
            warnings.push(format!(
                "failed to read pi session transcript {}: {err}",
                path.display()
            ));
            return None;
        }
    };

    let first_non_empty = raw.lines().find(|line| !line.trim().is_empty())?;

    let header = match serde_json::from_str::<Value>(first_non_empty) {
        Ok(value) => value,
        Err(err) => {
            warnings.push(format!(
                "failed to parse pi session header {}: {err}",
                path.display()
            ));
            return None;
        }
    };

    if header.get("type").and_then(Value::as_str) != Some("session") {
        return None;
    }

    let Some(session_id) = header
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
    else {
        warnings.push(format!(
            "pi session header missing id in {}",
            path.display()
        ));
        return None;
    };

    if !is_uuid_session_id(&session_id) {
        warnings.push(format!(
            "pi session header id is not UUID in {}: {}",
            path.display(),
            session_id
        ));
        return None;
    }

    let hints = collect_pi_session_hints(&header);
    let last_update = header
        .get("timestamp")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| modified_timestamp_string(path));

    Some(PiSessionRecord {
        session_id,
        path: path.to_path_buf(),
        last_update,
        hints,
    })
}

fn collect_pi_session_hints(header: &Value) -> Vec<PiSessionHint> {
    let mut hints = Vec::new();
    collect_pi_session_hints_rec(header, "", &mut hints);

    let mut seen = BTreeSet::new();
    hints
        .into_iter()
        .filter(|hint| seen.insert((hint.kind, hint.session_id.clone(), hint.evidence.clone())))
        .collect()
}

fn collect_pi_session_hints_rec(value: &Value, path: &str, out: &mut Vec<PiSessionHint>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let key_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };

                if let Some(kind) = classify_pi_hint_key(key) {
                    let mut ids = Vec::new();
                    collect_uuid_strings(child, &mut ids);
                    for session_id in ids {
                        out.push(PiSessionHint {
                            kind,
                            session_id,
                            evidence: format!("session header key `{key_path}`"),
                        });
                    }
                }

                collect_pi_session_hints_rec(child, &key_path, out);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                let key_path = format!("{path}[{index}]");
                collect_pi_session_hints_rec(child, &key_path, out);
            }
        }
        _ => {}
    }
}

fn classify_pi_hint_key(key: &str) -> Option<PiSessionHintKind> {
    let normalized = normalize_hint_key(key);

    const PARENT_HINTS: &[&str] = &[
        "parentsessionid",
        "parentsessionids",
        "parentthreadid",
        "parentthreadids",
        "mainsessionid",
        "rootsessionid",
        "parentid",
    ];
    const CHILD_HINTS: &[&str] = &[
        "childsessionid",
        "childsessionids",
        "childthreadid",
        "childthreadids",
        "childid",
        "subsessionid",
        "subsessionids",
        "subagentsessionid",
        "subagentsessionids",
        "subagentthreadid",
        "subagentthreadids",
    ];

    if PARENT_HINTS.contains(&normalized.as_str()) {
        return Some(PiSessionHintKind::Parent);
    }
    if CHILD_HINTS.contains(&normalized.as_str()) {
        return Some(PiSessionHintKind::Child);
    }

    let has_session_scope = normalized.contains("session") || normalized.contains("thread");
    if has_session_scope
        && (normalized.contains("parent")
            || normalized.contains("main")
            || normalized.contains("root"))
    {
        return Some(PiSessionHintKind::Parent);
    }
    if has_session_scope
        && (normalized.contains("child")
            || normalized.contains("subagent")
            || normalized.contains("subsession"))
    {
        return Some(PiSessionHintKind::Child);
    }

    None
}

fn normalize_hint_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn collect_uuid_strings(value: &Value, ids: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            if is_uuid_session_id(text) {
                ids.push(text.to_ascii_lowercase());
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_uuid_strings(item, ids);
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                collect_uuid_strings(item, ids);
            }
        }
        _ => {}
    }
}

fn resolve_amp_subagent_view(
    uri: &AgentsUri,
    roots: &ProviderRoots,
    list: bool,
) -> Result<SubagentView> {
    let main_uri = main_thread_uri(uri);
    let resolved_main = resolve_thread(&main_uri, roots)?;
    let main_raw = read_thread_raw(&resolved_main.path)?;
    let main_value =
        serde_json::from_str::<Value>(&main_raw).map_err(|source| XurlError::InvalidJsonLine {
            path: resolved_main.path.clone(),
            line: 1,
            source,
        })?;

    let mut warnings = resolved_main.metadata.warnings.clone();
    let handoffs = extract_amp_handoffs(&main_value, "main", &mut warnings);

    if list {
        return Ok(SubagentView::List(build_amp_list_view(
            uri, roots, &handoffs, warnings,
        )));
    }

    let agent_id = uri
        .agent_id
        .clone()
        .ok_or_else(|| XurlError::InvalidMode("missing agent id".to_string()))?;

    Ok(SubagentView::Detail(build_amp_detail_view(
        uri, roots, &agent_id, &handoffs, warnings,
    )))
}

fn build_amp_list_view(
    uri: &AgentsUri,
    roots: &ProviderRoots,
    handoffs: &[AmpHandoff],
    mut warnings: Vec<String>,
) -> SubagentListView {
    let mut grouped = BTreeMap::<String, Vec<&AmpHandoff>>::new();
    for handoff in handoffs {
        if handoff.thread_id == uri.session_id || handoff.role.as_deref() == Some("child") {
            continue;
        }
        grouped
            .entry(handoff.thread_id.clone())
            .or_default()
            .push(handoff);
    }

    let mut agents = Vec::new();
    for (agent_id, relations) in grouped {
        let mut relation = SubagentRelation::default();

        for handoff in relations {
            match handoff.role.as_deref() {
                Some("parent") => {
                    relation.validated = true;
                    push_unique(
                        &mut relation.evidence,
                        "main relationships includes handoff(role=parent) to child thread"
                            .to_string(),
                    );
                }
                Some(role) => {
                    push_unique(
                        &mut relation.evidence,
                        format!("main relationships includes handoff(role={role}) to child thread"),
                    );
                }
                None => {
                    push_unique(
                        &mut relation.evidence,
                        "main relationships includes handoff(role missing) to child thread"
                            .to_string(),
                    );
                }
            }
        }

        let mut status = if relation.validated {
            STATUS_PENDING_INIT.to_string()
        } else {
            STATUS_NOT_FOUND.to_string()
        };
        let mut status_source = "inferred".to_string();
        let mut last_update = None::<String>;
        let mut child_thread = None::<SubagentThreadRef>;

        if let Some(analysis) =
            analyze_amp_child_thread(&agent_id, &uri.session_id, roots, &mut warnings)
        {
            for evidence in analysis.relation_evidence {
                push_unique(&mut relation.evidence, evidence);
            }
            if !relation.evidence.is_empty() {
                relation.validated = true;
            }

            status = analysis.status;
            status_source = analysis.status_source;
            last_update = analysis.thread.last_updated_at.clone();
            child_thread = Some(analysis.thread);
        }

        agents.push(SubagentListItem {
            agent_id,
            status,
            status_source,
            last_update,
            relation,
            child_thread,
        });
    }

    SubagentListView {
        query: make_query(uri, None, true),
        agents,
        warnings,
    }
}

fn build_amp_detail_view(
    uri: &AgentsUri,
    roots: &ProviderRoots,
    agent_id: &str,
    handoffs: &[AmpHandoff],
    mut warnings: Vec<String>,
) -> SubagentDetailView {
    let mut relation = SubagentRelation::default();
    let mut lifecycle = Vec::<SubagentLifecycleEvent>::new();

    let matches = handoffs
        .iter()
        .filter(|handoff| handoff.thread_id == agent_id)
        .collect::<Vec<_>>();

    if matches.is_empty() {
        warnings.push(format!(
            "no handoff relationship found in main thread for child_thread_id={agent_id}"
        ));
    }

    for handoff in matches {
        match handoff.role.as_deref() {
            Some("parent") => {
                relation.validated = true;
                push_unique(
                    &mut relation.evidence,
                    "main relationships includes handoff(role=parent) to child thread".to_string(),
                );
                lifecycle.push(SubagentLifecycleEvent {
                    timestamp: handoff.timestamp.clone(),
                    event: "handoff".to_string(),
                    detail: "main handoff relationship discovered (role=parent)".to_string(),
                });
            }
            Some(role) => {
                push_unique(
                    &mut relation.evidence,
                    format!("main relationships includes handoff(role={role}) to child thread"),
                );
                lifecycle.push(SubagentLifecycleEvent {
                    timestamp: handoff.timestamp.clone(),
                    event: "handoff".to_string(),
                    detail: format!("main handoff relationship discovered (role={role})"),
                });
            }
            None => {
                push_unique(
                    &mut relation.evidence,
                    "main relationships includes handoff(role missing) to child thread".to_string(),
                );
                lifecycle.push(SubagentLifecycleEvent {
                    timestamp: handoff.timestamp.clone(),
                    event: "handoff".to_string(),
                    detail: "main handoff relationship discovered (role missing)".to_string(),
                });
            }
        }
    }

    let mut child_thread = None::<SubagentThreadRef>;
    let mut excerpt = Vec::<SubagentExcerptMessage>::new();
    let mut status = if relation.validated {
        STATUS_PENDING_INIT.to_string()
    } else {
        STATUS_NOT_FOUND.to_string()
    };
    let mut status_source = "inferred".to_string();

    if let Some(analysis) =
        analyze_amp_child_thread(agent_id, &uri.session_id, roots, &mut warnings)
    {
        for evidence in analysis.relation_evidence {
            push_unique(&mut relation.evidence, evidence);
        }
        if !relation.evidence.is_empty() {
            relation.validated = true;
        }
        lifecycle.extend(analysis.lifecycle);
        status = analysis.status;
        status_source = analysis.status_source;
        child_thread = Some(analysis.thread);
        excerpt = analysis.excerpt;
    }

    SubagentDetailView {
        query: make_query(uri, Some(agent_id.to_string()), false),
        relation,
        lifecycle,
        status,
        status_source,
        child_thread,
        excerpt,
        warnings,
    }
}

fn analyze_amp_child_thread(
    child_thread_id: &str,
    main_thread_id: &str,
    roots: &ProviderRoots,
    warnings: &mut Vec<String>,
) -> Option<AmpChildAnalysis> {
    let resolved_child = match AmpProvider::new(&roots.amp_root).resolve(child_thread_id) {
        Ok(resolved) => resolved,
        Err(err) => {
            warnings.push(format!(
                "failed resolving amp child thread child_thread_id={child_thread_id}: {err}"
            ));
            return None;
        }
    };

    let child_raw = match read_thread_raw(&resolved_child.path) {
        Ok(raw) => raw,
        Err(err) => {
            warnings.push(format!(
                "failed reading amp child thread child_thread_id={child_thread_id}: {err}"
            ));
            return None;
        }
    };

    let child_value = match serde_json::from_str::<Value>(&child_raw) {
        Ok(value) => value,
        Err(err) => {
            warnings.push(format!(
                "failed parsing amp child thread {}: {err}",
                resolved_child.path.display()
            ));
            return None;
        }
    };

    let mut relation_evidence = Vec::<String>::new();
    let mut lifecycle = Vec::<SubagentLifecycleEvent>::new();
    for handoff in extract_amp_handoffs(&child_value, "child", warnings) {
        if handoff.thread_id != main_thread_id {
            continue;
        }

        match handoff.role.as_deref() {
            Some("child") => {
                push_unique(
                    &mut relation_evidence,
                    "child relationships includes handoff(role=child) back to main thread"
                        .to_string(),
                );
                lifecycle.push(SubagentLifecycleEvent {
                    timestamp: handoff.timestamp.clone(),
                    event: "handoff_backlink".to_string(),
                    detail: "child handoff relationship discovered (role=child)".to_string(),
                });
            }
            Some(role) => {
                push_unique(
                    &mut relation_evidence,
                    format!(
                        "child relationships includes handoff(role={role}) back to main thread"
                    ),
                );
                lifecycle.push(SubagentLifecycleEvent {
                    timestamp: handoff.timestamp.clone(),
                    event: "handoff_backlink".to_string(),
                    detail: format!("child handoff relationship discovered (role={role})"),
                });
            }
            None => {
                push_unique(
                    &mut relation_evidence,
                    "child relationships includes handoff(role missing) back to main thread"
                        .to_string(),
                );
                lifecycle.push(SubagentLifecycleEvent {
                    timestamp: handoff.timestamp.clone(),
                    event: "handoff_backlink".to_string(),
                    detail: "child handoff relationship discovered (role missing)".to_string(),
                });
            }
        }
    }

    let messages =
        match render::extract_messages(ProviderKind::Amp, &resolved_child.path, &child_raw) {
            Ok(messages) => messages,
            Err(err) => {
                warnings.push(format!(
                    "failed extracting amp child messages from {}: {err}",
                    resolved_child.path.display()
                ));
                Vec::new()
            }
        };
    let has_user = messages
        .iter()
        .any(|message| message.role == MessageRole::User);
    let has_assistant = messages
        .iter()
        .any(|message| message.role == MessageRole::Assistant);

    let excerpt = messages
        .into_iter()
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|message| SubagentExcerptMessage {
            role: message.role,
            text: message.text,
        })
        .collect::<Vec<_>>();

    let (status, status_source) = infer_amp_status(&child_value, has_user, has_assistant);
    let last_updated_at = extract_amp_last_update(&child_value)
        .or_else(|| modified_timestamp_string(&resolved_child.path));

    Some(AmpChildAnalysis {
        thread: SubagentThreadRef {
            thread_id: child_thread_id.to_string(),
            path: Some(resolved_child.path.display().to_string()),
            last_updated_at,
        },
        status,
        status_source,
        excerpt,
        lifecycle,
        relation_evidence,
    })
}

fn extract_amp_handoffs(
    value: &Value,
    source: &str,
    warnings: &mut Vec<String>,
) -> Vec<AmpHandoff> {
    let mut handoffs = Vec::new();
    for relationship in value
        .get("relationships")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if relationship.get("type").and_then(Value::as_str) != Some("handoff") {
            continue;
        }

        let Some(thread_id_raw) = relationship.get("threadID").and_then(Value::as_str) else {
            warnings.push(format!(
                "{source} thread handoff relationship missing threadID field"
            ));
            continue;
        };
        let Some(thread_id) = normalize_amp_thread_id(thread_id_raw) else {
            warnings.push(format!(
                "{source} thread handoff relationship has invalid threadID={thread_id_raw}"
            ));
            continue;
        };

        let role = relationship
            .get("role")
            .and_then(Value::as_str)
            .map(|role| role.to_ascii_lowercase());
        let timestamp = relationship
            .get("timestamp")
            .or_else(|| relationship.get("updatedAt"))
            .or_else(|| relationship.get("createdAt"))
            .and_then(Value::as_str)
            .map(ToString::to_string);

        handoffs.push(AmpHandoff {
            thread_id,
            role,
            timestamp,
        });
    }

    handoffs
}

fn normalize_amp_thread_id(thread_id: &str) -> Option<String> {
    AgentsUri::parse(&format!("amp://{thread_id}"))
        .ok()
        .map(|uri| uri.session_id)
}

fn infer_amp_status(value: &Value, has_user: bool, has_assistant: bool) -> (String, String) {
    if let Some(status) = extract_amp_status(value) {
        return (status, "child_thread".to_string());
    }
    if has_assistant {
        return (STATUS_COMPLETED.to_string(), "inferred".to_string());
    }
    if has_user {
        return (STATUS_RUNNING.to_string(), "inferred".to_string());
    }
    (STATUS_PENDING_INIT.to_string(), "inferred".to_string())
}

fn extract_amp_status(value: &Value) -> Option<String> {
    let status = value.get("status");
    if let Some(status) = status {
        if let Some(status_str) = status.as_str() {
            return Some(status_str.to_string());
        }
        if let Some(status_obj) = status.as_object() {
            for key in [
                STATUS_PENDING_INIT,
                STATUS_RUNNING,
                STATUS_COMPLETED,
                STATUS_ERRORED,
                STATUS_SHUTDOWN,
                STATUS_NOT_FOUND,
            ] {
                if status_obj.contains_key(key) {
                    return Some(key.to_string());
                }
            }
        }
    }

    value
        .get("state")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn extract_amp_last_update(value: &Value) -> Option<String> {
    for key in ["lastUpdated", "updatedAt", "timestamp", "createdAt"] {
        if let Some(stamp) = value.get(key).and_then(Value::as_str) {
            return Some(stamp.to_string());
        }
    }

    for message in value
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .rev()
    {
        if let Some(stamp) = message.get("timestamp").and_then(Value::as_str) {
            return Some(stamp.to_string());
        }
    }

    None
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn resolve_codex_subagent_view(
    uri: &AgentsUri,
    roots: &ProviderRoots,
    list: bool,
) -> Result<SubagentView> {
    let main_uri = main_thread_uri(uri);
    let resolved_main = resolve_thread(&main_uri, roots)?;
    let main_raw = read_thread_raw(&resolved_main.path)?;

    let mut warnings = resolved_main.metadata.warnings.clone();
    let mut timelines = BTreeMap::<String, AgentTimeline>::new();
    warnings.extend(parse_codex_parent_lifecycle(&main_raw, &mut timelines));

    if list {
        return Ok(SubagentView::List(build_codex_list_view(
            uri, roots, &timelines, warnings,
        )));
    }

    let agent_id = uri
        .agent_id
        .clone()
        .ok_or_else(|| XurlError::InvalidMode("missing agent id".to_string()))?;

    Ok(SubagentView::Detail(build_codex_detail_view(
        uri, roots, &agent_id, &timelines, warnings,
    )))
}

fn build_codex_list_view(
    uri: &AgentsUri,
    roots: &ProviderRoots,
    timelines: &BTreeMap<String, AgentTimeline>,
    warnings: Vec<String>,
) -> SubagentListView {
    let mut agents = Vec::new();

    for (agent_id, timeline) in timelines {
        let mut relation = SubagentRelation::default();
        if timeline.has_spawn {
            relation.validated = true;
            relation
                .evidence
                .push("parent rollout contains spawn_agent output".to_string());
        }

        let mut child_ref = None;
        let mut last_update = timeline.last_update.clone();
        if let Some((thread_ref, relation_evidence, thread_last_update)) =
            resolve_codex_child_thread(agent_id, &uri.session_id, roots)
        {
            if !relation_evidence.is_empty() {
                relation.validated = true;
                relation.evidence.extend(relation_evidence);
            }
            if last_update.is_none() {
                last_update = thread_last_update;
            }
            child_ref = Some(thread_ref);
        }

        let (status, status_source) = infer_status_from_timeline(timeline, child_ref.is_some());

        agents.push(SubagentListItem {
            agent_id: agent_id.clone(),
            status,
            status_source,
            last_update,
            relation,
            child_thread: child_ref,
        });
    }

    SubagentListView {
        query: make_query(uri, None, true),
        agents,
        warnings,
    }
}

fn build_codex_detail_view(
    uri: &AgentsUri,
    roots: &ProviderRoots,
    agent_id: &str,
    timelines: &BTreeMap<String, AgentTimeline>,
    mut warnings: Vec<String>,
) -> SubagentDetailView {
    let timeline = timelines.get(agent_id).cloned().unwrap_or_default();
    let mut relation = SubagentRelation::default();
    if timeline.has_spawn {
        relation.validated = true;
        relation
            .evidence
            .push("parent rollout contains spawn_agent output".to_string());
    }

    let mut child_thread = None;
    let mut excerpt = Vec::new();
    let mut child_status = None;

    if let Some((resolved_child, relation_evidence, thread_ref)) =
        resolve_codex_child_resolved(agent_id, &uri.session_id, roots)
    {
        if !relation_evidence.is_empty() {
            relation.validated = true;
            relation.evidence.extend(relation_evidence);
        }

        match read_thread_raw(&resolved_child.path) {
            Ok(child_raw) => {
                if let Some(inferred) = infer_codex_child_status(&child_raw, &resolved_child.path) {
                    child_status = Some(inferred);
                }

                if let Ok(messages) =
                    render::extract_messages(ProviderKind::Codex, &resolved_child.path, &child_raw)
                {
                    excerpt = messages
                        .into_iter()
                        .rev()
                        .take(3)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .map(|message| SubagentExcerptMessage {
                            role: message.role,
                            text: message.text,
                        })
                        .collect();
                }
            }
            Err(err) => warnings.push(format!(
                "failed reading child thread for agent_id={agent_id}: {err}"
            )),
        }

        child_thread = Some(thread_ref);
    }

    let (status, status_source) =
        infer_status_for_detail(&timeline, child_status, child_thread.is_some());

    SubagentDetailView {
        query: make_query(uri, Some(agent_id.to_string()), false),
        relation,
        lifecycle: timeline.events,
        status,
        status_source,
        child_thread,
        excerpt,
        warnings,
    }
}

fn resolve_codex_child_thread(
    agent_id: &str,
    main_thread_id: &str,
    roots: &ProviderRoots,
) -> Option<(SubagentThreadRef, Vec<String>, Option<String>)> {
    let resolved = CodexProvider::new(&roots.codex_root)
        .resolve(agent_id)
        .ok()?;
    let raw = read_thread_raw(&resolved.path).ok()?;

    let mut evidence = Vec::new();
    if extract_codex_parent_thread_id(&raw)
        .as_deref()
        .is_some_and(|parent| parent == main_thread_id)
    {
        evidence.push("child session_meta points to main thread".to_string());
    }

    let last_update = extract_last_timestamp(&raw);
    let thread_ref = SubagentThreadRef {
        thread_id: agent_id.to_string(),
        path: Some(resolved.path.display().to_string()),
        last_updated_at: last_update.clone(),
    };

    Some((thread_ref, evidence, last_update))
}

fn resolve_codex_child_resolved(
    agent_id: &str,
    main_thread_id: &str,
    roots: &ProviderRoots,
) -> Option<(ResolvedThread, Vec<String>, SubagentThreadRef)> {
    let resolved = CodexProvider::new(&roots.codex_root)
        .resolve(agent_id)
        .ok()?;
    let raw = read_thread_raw(&resolved.path).ok()?;

    let mut evidence = Vec::new();
    if extract_codex_parent_thread_id(&raw)
        .as_deref()
        .is_some_and(|parent| parent == main_thread_id)
    {
        evidence.push("child session_meta points to main thread".to_string());
    }

    let thread_ref = SubagentThreadRef {
        thread_id: agent_id.to_string(),
        path: Some(resolved.path.display().to_string()),
        last_updated_at: extract_last_timestamp(&raw),
    };

    Some((resolved, evidence, thread_ref))
}

fn infer_codex_child_status(raw: &str, path: &Path) -> Option<String> {
    let mut has_assistant_message = false;
    let mut has_error = false;

    for (line_idx, line) in raw.lines().enumerate() {
        let Ok(Some(value)) = jsonl::parse_json_line(path, line_idx + 1, line) else {
            continue;
        };

        if value.get("type").and_then(Value::as_str) == Some("event_msg") {
            let payload_type = value
                .get("payload")
                .and_then(|payload| payload.get("type"))
                .and_then(Value::as_str);
            if payload_type == Some("turn_aborted") {
                has_error = true;
            }
        }

        if render::extract_messages(ProviderKind::Codex, path, line)
            .ok()
            .is_some_and(|messages| {
                messages
                    .iter()
                    .any(|message| matches!(message.role, crate::model::MessageRole::Assistant))
            })
        {
            has_assistant_message = true;
        }
    }

    if has_error {
        Some(STATUS_ERRORED.to_string())
    } else if has_assistant_message {
        Some(STATUS_COMPLETED.to_string())
    } else {
        None
    }
}

fn parse_codex_parent_lifecycle(
    raw: &str,
    timelines: &mut BTreeMap<String, AgentTimeline>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut calls: HashMap<String, (String, Value, Option<String>)> = HashMap::new();

    for (line_idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let value = match jsonl::parse_json_line(Path::new("<codex:parent>"), line_idx + 1, trimmed)
        {
            Ok(Some(value)) => value,
            Ok(None) => continue,
            Err(err) => {
                warnings.push(format!(
                    "failed to parse parent rollout line {}: {err}",
                    line_idx + 1
                ));
                continue;
            }
        };

        if value.get("type").and_then(Value::as_str) != Some("response_item") {
            continue;
        }

        let Some(payload) = value.get("payload") else {
            continue;
        };
        let Some(payload_type) = payload.get("type").and_then(Value::as_str) else {
            continue;
        };

        if payload_type == "function_call" {
            let call_id = payload
                .get("call_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if call_id.is_empty() {
                continue;
            }

            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if name.is_empty() {
                continue;
            }

            let args = payload
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok())
                .unwrap_or_else(|| Value::Object(Default::default()));

            let timestamp = value
                .get("timestamp")
                .and_then(Value::as_str)
                .map(ToString::to_string);

            calls.insert(call_id, (name, args, timestamp));
            continue;
        }

        if payload_type != "function_call_output" {
            continue;
        }

        let Some(call_id) = payload.get("call_id").and_then(Value::as_str) else {
            continue;
        };

        let Some((name, args, timestamp)) = calls.remove(call_id) else {
            continue;
        };

        let output_raw = payload
            .get("output")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let output_value =
            serde_json::from_str::<Value>(&output_raw).unwrap_or(Value::String(output_raw));

        match name.as_str() {
            "spawn_agent" => {
                let Some(agent_id) = output_value
                    .get("agent_id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
                else {
                    warnings.push(
                        "spawn_agent output did not include agent_id; skipping subagent mapping"
                            .to_string(),
                    );
                    continue;
                };

                let timeline = timelines.entry(agent_id).or_default();
                timeline.has_spawn = true;
                timeline.has_activity = true;
                timeline.last_update = timestamp.clone();
                timeline.events.push(SubagentLifecycleEvent {
                    timestamp,
                    event: "spawn_agent".to_string(),
                    detail: "subagent spawned".to_string(),
                });
            }
            "wait" => {
                let ids = args
                    .get("ids")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();

                let timed_out = output_value
                    .get("timed_out")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);

                for agent_id in ids {
                    let timeline = timelines.entry(agent_id).or_default();
                    timeline.has_activity = true;
                    timeline.last_update = timestamp.clone();

                    let mut detail = if timed_out {
                        "wait timed out".to_string()
                    } else {
                        "wait returned".to_string()
                    };

                    if let Some(state) = infer_state_from_status_payload(&output_value) {
                        timeline.states.push(state.clone());
                        detail = format!("wait state={state}");
                    } else if timed_out {
                        timeline.states.push(STATUS_RUNNING.to_string());
                    }

                    timeline.events.push(SubagentLifecycleEvent {
                        timestamp: timestamp.clone(),
                        event: "wait".to_string(),
                        detail,
                    });
                }
            }
            "send_input" | "resume_agent" | "close_agent" => {
                let Some(agent_id) = args
                    .get("id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
                else {
                    continue;
                };

                let timeline = timelines.entry(agent_id).or_default();
                timeline.has_activity = true;
                timeline.last_update = timestamp.clone();

                if name == "close_agent" {
                    if let Some(state) = infer_state_from_status_payload(&output_value) {
                        timeline.states.push(state.clone());
                    } else {
                        timeline.states.push(STATUS_SHUTDOWN.to_string());
                    }
                }

                timeline.events.push(SubagentLifecycleEvent {
                    timestamp,
                    event: name,
                    detail: "agent lifecycle event".to_string(),
                });
            }
            _ => {}
        }
    }

    warnings
}

fn infer_state_from_status_payload(payload: &Value) -> Option<String> {
    let status = payload.get("status")?;

    if let Some(object) = status.as_object() {
        for key in object.keys() {
            if [
                STATUS_PENDING_INIT,
                STATUS_RUNNING,
                STATUS_COMPLETED,
                STATUS_ERRORED,
                STATUS_SHUTDOWN,
                STATUS_NOT_FOUND,
            ]
            .contains(&key.as_str())
            {
                return Some(key.clone());
            }
        }

        if object.contains_key("completed") {
            return Some(STATUS_COMPLETED.to_string());
        }
    }

    None
}

fn infer_status_from_timeline(timeline: &AgentTimeline, child_exists: bool) -> (String, String) {
    if timeline.states.iter().any(|state| state == STATUS_ERRORED) {
        return (STATUS_ERRORED.to_string(), "parent_rollout".to_string());
    }
    if timeline.states.iter().any(|state| state == STATUS_SHUTDOWN) {
        return (STATUS_SHUTDOWN.to_string(), "parent_rollout".to_string());
    }
    if timeline
        .states
        .iter()
        .any(|state| state == STATUS_COMPLETED)
    {
        return (STATUS_COMPLETED.to_string(), "parent_rollout".to_string());
    }
    if timeline.states.iter().any(|state| state == STATUS_RUNNING) || timeline.has_activity {
        return (STATUS_RUNNING.to_string(), "parent_rollout".to_string());
    }
    if timeline.has_spawn {
        return (
            STATUS_PENDING_INIT.to_string(),
            "parent_rollout".to_string(),
        );
    }
    if child_exists {
        return (STATUS_RUNNING.to_string(), "child_rollout".to_string());
    }

    (STATUS_NOT_FOUND.to_string(), "inferred".to_string())
}

fn infer_status_for_detail(
    timeline: &AgentTimeline,
    child_status: Option<String>,
    child_exists: bool,
) -> (String, String) {
    let (status, source) = infer_status_from_timeline(timeline, child_exists);
    if status == STATUS_NOT_FOUND
        && let Some(child_status) = child_status
    {
        return (child_status, "child_rollout".to_string());
    }

    (status, source)
}

fn extract_codex_parent_thread_id(raw: &str) -> Option<String> {
    let first = raw.lines().find(|line| !line.trim().is_empty())?;
    let value = serde_json::from_str::<Value>(first).ok()?;

    value
        .get("payload")
        .and_then(|payload| payload.get("source"))
        .and_then(|source| source.get("subagent"))
        .and_then(|subagent| subagent.get("thread_spawn"))
        .and_then(|thread_spawn| thread_spawn.get("parent_thread_id"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn resolve_claude_subagent_view(
    uri: &AgentsUri,
    roots: &ProviderRoots,
    list: bool,
) -> Result<SubagentView> {
    let main_uri = main_thread_uri(uri);
    let resolved_main = resolve_thread(&main_uri, roots)?;

    let mut warnings = resolved_main.metadata.warnings.clone();
    let records = discover_claude_agents(&resolved_main, &uri.session_id, &mut warnings);

    if list {
        return Ok(SubagentView::List(SubagentListView {
            query: make_query(uri, None, true),
            agents: records
                .iter()
                .map(|record| SubagentListItem {
                    agent_id: record.agent_id.clone(),
                    status: record.status.clone(),
                    status_source: "inferred".to_string(),
                    last_update: record.last_update.clone(),
                    relation: record.relation.clone(),
                    child_thread: Some(SubagentThreadRef {
                        thread_id: record.agent_id.clone(),
                        path: Some(record.path.display().to_string()),
                        last_updated_at: record.last_update.clone(),
                    }),
                })
                .collect(),
            warnings,
        }));
    }

    let requested_agent = uri
        .agent_id
        .clone()
        .ok_or_else(|| XurlError::InvalidMode("missing agent id".to_string()))?;

    let normalized_requested = normalize_agent_id(&requested_agent);

    if let Some(record) = records
        .into_iter()
        .find(|record| normalize_agent_id(&record.agent_id) == normalized_requested)
    {
        let lifecycle = vec![SubagentLifecycleEvent {
            timestamp: record.last_update.clone(),
            event: "discovered_agent_file".to_string(),
            detail: "agent transcript discovered and analyzed".to_string(),
        }];

        warnings.extend(record.warnings.clone());

        return Ok(SubagentView::Detail(SubagentDetailView {
            query: make_query(uri, Some(requested_agent), false),
            relation: record.relation.clone(),
            lifecycle,
            status: record.status.clone(),
            status_source: "inferred".to_string(),
            child_thread: Some(SubagentThreadRef {
                thread_id: record.agent_id.clone(),
                path: Some(record.path.display().to_string()),
                last_updated_at: record.last_update.clone(),
            }),
            excerpt: record.excerpt,
            warnings,
        }));
    }

    warnings.push(format!(
        "agent not found for main_session_id={} agent_id={requested_agent}",
        uri.session_id
    ));

    Ok(SubagentView::Detail(SubagentDetailView {
        query: make_query(uri, Some(requested_agent), false),
        relation: SubagentRelation::default(),
        lifecycle: Vec::new(),
        status: STATUS_NOT_FOUND.to_string(),
        status_source: "inferred".to_string(),
        child_thread: None,
        excerpt: Vec::new(),
        warnings,
    }))
}

fn resolve_gemini_subagent_view(
    uri: &AgentsUri,
    roots: &ProviderRoots,
    list: bool,
) -> Result<SubagentView> {
    let main_uri = main_thread_uri(uri);
    let resolved_main = resolve_thread(&main_uri, roots)?;
    let mut warnings = resolved_main.metadata.warnings.clone();

    let (chats, mut children) =
        discover_gemini_children(&resolved_main, &uri.session_id, &mut warnings);

    if list {
        let agents = children
            .iter_mut()
            .map(|(child_session_id, record)| {
                if let Some(chat) = chats.get(child_session_id) {
                    return SubagentListItem {
                        agent_id: child_session_id.clone(),
                        status: chat.status.clone(),
                        status_source: "child_rollout".to_string(),
                        last_update: chat.last_update.clone(),
                        relation: record.relation.clone(),
                        child_thread: Some(SubagentThreadRef {
                            thread_id: child_session_id.clone(),
                            path: Some(chat.path.display().to_string()),
                            last_updated_at: chat.last_update.clone(),
                        }),
                    };
                }

                let missing_warning = format!(
                    "child session {child_session_id} discovered from local Gemini data but chat file was not found in project chats"
                );
                warnings.push(missing_warning);
                let missing_evidence =
                    "child session could not be materialized to a chat file".to_string();
                if !record.relation.evidence.contains(&missing_evidence) {
                    record.relation.evidence.push(missing_evidence);
                }

                SubagentListItem {
                    agent_id: child_session_id.clone(),
                    status: STATUS_NOT_FOUND.to_string(),
                    status_source: "inferred".to_string(),
                    last_update: record.relation_timestamp.clone(),
                    relation: record.relation.clone(),
                    child_thread: None,
                }
            })
            .collect::<Vec<_>>();

        return Ok(SubagentView::List(SubagentListView {
            query: make_query(uri, None, true),
            agents,
            warnings,
        }));
    }

    let requested_child = uri
        .agent_id
        .clone()
        .ok_or_else(|| XurlError::InvalidMode("missing agent id".to_string()))?;

    let mut relation = SubagentRelation::default();
    let mut lifecycle = Vec::new();
    let mut status = STATUS_NOT_FOUND.to_string();
    let mut status_source = "inferred".to_string();
    let mut child_thread = None;
    let mut excerpt = Vec::new();

    if let Some(record) = children.get_mut(&requested_child) {
        relation = record.relation.clone();
        if !relation.evidence.is_empty() {
            lifecycle.push(SubagentLifecycleEvent {
                timestamp: record.relation_timestamp.clone(),
                event: "discover_child".to_string(),
                detail: if relation.validated {
                    "child relation validated from local Gemini payload".to_string()
                } else {
                    "child relation inferred from logs.json /resume sequence".to_string()
                },
            });
        }

        if let Some(chat) = chats.get(&requested_child) {
            status = chat.status.clone();
            status_source = "child_rollout".to_string();
            child_thread = Some(SubagentThreadRef {
                thread_id: requested_child.clone(),
                path: Some(chat.path.display().to_string()),
                last_updated_at: chat.last_update.clone(),
            });
            excerpt = extract_child_excerpt(ProviderKind::Gemini, &chat.path, &mut warnings);
        } else {
            warnings.push(format!(
                "child session {requested_child} discovered from local Gemini data but chat file was not found in project chats"
            ));
            let missing_evidence =
                "child session could not be materialized to a chat file".to_string();
            if !relation.evidence.contains(&missing_evidence) {
                relation.evidence.push(missing_evidence);
            }
        }
    } else if let Some(chat) = chats.get(&requested_child) {
        warnings.push(format!(
            "unable to validate Gemini parent-child relation for main_session_id={} child_session_id={requested_child}",
            uri.session_id
        ));
        lifecycle.push(SubagentLifecycleEvent {
            timestamp: chat.last_update.clone(),
            event: "discover_child_chat".to_string(),
            detail: "child chat exists but relation to main thread is unknown".to_string(),
        });
        status = chat.status.clone();
        status_source = "child_rollout".to_string();
        child_thread = Some(SubagentThreadRef {
            thread_id: requested_child.clone(),
            path: Some(chat.path.display().to_string()),
            last_updated_at: chat.last_update.clone(),
        });
        excerpt = extract_child_excerpt(ProviderKind::Gemini, &chat.path, &mut warnings);
    } else {
        warnings.push(format!(
            "child session not found for main_session_id={} child_session_id={requested_child}",
            uri.session_id
        ));
    }

    Ok(SubagentView::Detail(SubagentDetailView {
        query: make_query(uri, Some(requested_child), false),
        relation,
        lifecycle,
        status,
        status_source,
        child_thread,
        excerpt,
        warnings,
    }))
}

fn discover_gemini_children(
    resolved_main: &ResolvedThread,
    main_session_id: &str,
    warnings: &mut Vec<String>,
) -> (
    BTreeMap<String, GeminiChatRecord>,
    BTreeMap<String, GeminiChildRecord>,
) {
    let Some(project_dir) = resolved_main.path.parent().and_then(Path::parent) else {
        warnings.push(format!(
            "cannot determine Gemini project directory from resolved main thread path: {}",
            resolved_main.path.display()
        ));
        return (BTreeMap::new(), BTreeMap::new());
    };

    let chats = load_gemini_project_chats(project_dir, warnings);
    let logs = read_gemini_log_entries(project_dir, warnings);

    let mut children = BTreeMap::<String, GeminiChildRecord>::new();

    for chat in chats.values() {
        if chat.session_id == main_session_id {
            continue;
        }
        if chat
            .explicit_parent_ids
            .iter()
            .any(|parent_id| parent_id == main_session_id)
        {
            push_explicit_gemini_relation(
                &mut children,
                &chat.session_id,
                "child chat payload includes explicit parent session reference",
                chat.last_update.clone(),
            );
        }
    }

    for entry in &logs {
        if entry.session_id == main_session_id {
            continue;
        }
        if entry
            .explicit_parent_ids
            .iter()
            .any(|parent_id| parent_id == main_session_id)
        {
            push_explicit_gemini_relation(
                &mut children,
                &entry.session_id,
                "logs.json entry includes explicit parent session reference",
                entry.timestamp.clone(),
            );
        }
    }

    for (child_session_id, parent_session_id, timestamp) in infer_gemini_relations_from_logs(&logs)
    {
        if child_session_id == main_session_id || parent_session_id != main_session_id {
            continue;
        }
        push_inferred_gemini_relation(
            &mut children,
            &child_session_id,
            "logs.json shows child session starts with /resume after main session activity",
            timestamp,
        );
    }

    (chats, children)
}

fn load_gemini_project_chats(
    project_dir: &Path,
    warnings: &mut Vec<String>,
) -> BTreeMap<String, GeminiChatRecord> {
    let chats_dir = project_dir.join("chats");
    if !chats_dir.exists() {
        warnings.push(format!(
            "Gemini project chats directory not found: {}",
            chats_dir.display()
        ));
        return BTreeMap::new();
    }

    let mut chats = BTreeMap::<String, GeminiChatRecord>::new();
    let Ok(entries) = fs::read_dir(&chats_dir) else {
        warnings.push(format!(
            "failed to read Gemini chats directory: {}",
            chats_dir.display()
        ));
        return chats;
    };

    for entry in entries.filter_map(std::result::Result::ok) {
        let path = entry.path();
        let is_chat_file = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("session-") && name.ends_with(".json"));
        if !is_chat_file || !path.is_file() {
            continue;
        }

        let Some(chat) = parse_gemini_chat_file(&path, warnings) else {
            continue;
        };

        match chats.get(&chat.session_id) {
            Some(existing) => {
                let existing_stamp = file_modified_epoch(&existing.path).unwrap_or(0);
                let new_stamp = file_modified_epoch(&chat.path).unwrap_or(0);
                if new_stamp > existing_stamp {
                    chats.insert(chat.session_id.clone(), chat);
                }
            }
            None => {
                chats.insert(chat.session_id.clone(), chat);
            }
        }
    }

    chats
}

fn parse_gemini_chat_file(path: &Path, warnings: &mut Vec<String>) -> Option<GeminiChatRecord> {
    let raw = match read_thread_raw(path) {
        Ok(raw) => raw,
        Err(err) => {
            warnings.push(format!(
                "failed to read Gemini chat {}: {err}",
                path.display()
            ));
            return None;
        }
    };

    let value = match serde_json::from_str::<Value>(&raw) {
        Ok(value) => value,
        Err(err) => {
            warnings.push(format!(
                "failed to parse Gemini chat JSON {}: {err}",
                path.display()
            ));
            return None;
        }
    };

    let Some(session_id) = value
        .get("sessionId")
        .and_then(Value::as_str)
        .and_then(parse_session_id_like)
    else {
        warnings.push(format!(
            "Gemini chat missing valid sessionId: {}",
            path.display()
        ));
        return None;
    };

    let last_update = value
        .get("lastUpdated")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            value
                .get("startTime")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| modified_timestamp_string(path));

    let status = infer_gemini_chat_status(&value);
    let explicit_parent_ids = parse_parent_session_ids(&value);

    Some(GeminiChatRecord {
        session_id,
        path: path.to_path_buf(),
        last_update,
        status,
        explicit_parent_ids,
    })
}

fn infer_gemini_chat_status(value: &Value) -> String {
    let Some(messages) = value.get("messages").and_then(Value::as_array) else {
        return STATUS_PENDING_INIT.to_string();
    };

    let mut has_error = false;
    let mut has_assistant = false;
    let mut has_user = false;

    for message in messages {
        let message_type = message
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if message_type == "error" || !message.get("error").is_none_or(Value::is_null) {
            has_error = true;
        }
        if message_type == "gemini" || message_type == "assistant" {
            has_assistant = true;
        }
        if message_type == "user" {
            has_user = true;
        }
    }

    if has_error {
        STATUS_ERRORED.to_string()
    } else if has_assistant {
        STATUS_COMPLETED.to_string()
    } else if has_user {
        STATUS_RUNNING.to_string()
    } else {
        STATUS_PENDING_INIT.to_string()
    }
}

fn read_gemini_log_entries(project_dir: &Path, warnings: &mut Vec<String>) -> Vec<GeminiLogEntry> {
    let logs_path = project_dir.join("logs.json");
    if !logs_path.exists() {
        return Vec::new();
    }

    let raw = match read_thread_raw(&logs_path) {
        Ok(raw) => raw,
        Err(err) => {
            warnings.push(format!(
                "failed to read Gemini logs file {}: {err}",
                logs_path.display()
            ));
            return Vec::new();
        }
    };

    if raw.trim().is_empty() {
        return Vec::new();
    }

    if let Ok(value) = serde_json::from_str::<Value>(&raw) {
        return parse_gemini_logs_value(&logs_path, value, warnings);
    }

    let mut parsed = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(value) => {
                if let Some(entry) = parse_gemini_log_entry(&logs_path, index + 1, &value, warnings)
                {
                    parsed.push(entry);
                }
            }
            Err(err) => warnings.push(format!(
                "failed to parse Gemini logs line {} in {}: {err}",
                index + 1,
                logs_path.display()
            )),
        }
    }
    parsed
}

fn parse_gemini_logs_value(
    logs_path: &Path,
    value: Value,
    warnings: &mut Vec<String>,
) -> Vec<GeminiLogEntry> {
    match value {
        Value::Array(entries) => entries
            .into_iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                parse_gemini_log_entry(logs_path, index + 1, &entry, warnings)
            })
            .collect(),
        Value::Object(object) => {
            if let Some(entries) = object.get("entries").and_then(Value::as_array) {
                return entries
                    .iter()
                    .enumerate()
                    .filter_map(|(index, entry)| {
                        parse_gemini_log_entry(logs_path, index + 1, entry, warnings)
                    })
                    .collect();
            }

            parse_gemini_log_entry(logs_path, 1, &Value::Object(object), warnings)
                .into_iter()
                .collect()
        }
        _ => {
            warnings.push(format!(
                "unsupported Gemini logs format in {}: expected JSON array or object",
                logs_path.display()
            ));
            Vec::new()
        }
    }
}

fn parse_gemini_log_entry(
    logs_path: &Path,
    line: usize,
    value: &Value,
    warnings: &mut Vec<String>,
) -> Option<GeminiLogEntry> {
    let Some(object) = value.as_object() else {
        warnings.push(format!(
            "invalid Gemini log entry at {} line {}: expected JSON object",
            logs_path.display(),
            line
        ));
        return None;
    };

    let session_id = object
        .get("sessionId")
        .and_then(Value::as_str)
        .or_else(|| object.get("session_id").and_then(Value::as_str))
        .and_then(parse_session_id_like)?;

    Some(GeminiLogEntry {
        session_id,
        message: object
            .get("message")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        timestamp: object
            .get("timestamp")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        entry_type: object
            .get("type")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        explicit_parent_ids: parse_parent_session_ids(value),
    })
}

fn infer_gemini_relations_from_logs(
    logs: &[GeminiLogEntry],
) -> Vec<(String, String, Option<String>)> {
    let mut first_user_seen = BTreeSet::<String>::new();
    let mut latest_session = None::<String>;
    let mut relations = Vec::new();

    for entry in logs {
        let session_id = entry.session_id.clone();
        let is_user_like = entry
            .entry_type
            .as_deref()
            .is_none_or(|kind| kind == "user");

        if is_user_like && !first_user_seen.contains(&session_id) {
            first_user_seen.insert(session_id.clone());
            if entry
                .message
                .as_deref()
                .map(str::trim_start)
                .is_some_and(|message| message.starts_with("/resume"))
                && let Some(parent_session_id) = latest_session.clone()
                && parent_session_id != session_id
            {
                relations.push((
                    session_id.clone(),
                    parent_session_id,
                    entry.timestamp.clone(),
                ));
            }
        }

        latest_session = Some(session_id);
    }

    relations
}

fn push_explicit_gemini_relation(
    children: &mut BTreeMap<String, GeminiChildRecord>,
    child_session_id: &str,
    evidence: &str,
    timestamp: Option<String>,
) {
    let record = children.entry(child_session_id.to_string()).or_default();
    record.relation.validated = true;
    if !record.relation.evidence.iter().any(|item| item == evidence) {
        record.relation.evidence.push(evidence.to_string());
    }
    if record.relation_timestamp.is_none() {
        record.relation_timestamp = timestamp;
    }
}

fn push_inferred_gemini_relation(
    children: &mut BTreeMap<String, GeminiChildRecord>,
    child_session_id: &str,
    evidence: &str,
    timestamp: Option<String>,
) {
    let record = children.entry(child_session_id.to_string()).or_default();
    if record.relation.validated {
        return;
    }
    if !record.relation.evidence.iter().any(|item| item == evidence) {
        record.relation.evidence.push(evidence.to_string());
    }
    if record.relation_timestamp.is_none() {
        record.relation_timestamp = timestamp;
    }
}

fn parse_parent_session_ids(value: &Value) -> Vec<String> {
    let mut parent_ids = BTreeSet::new();
    collect_parent_session_ids(value, &mut parent_ids);
    parent_ids.into_iter().collect()
}

fn collect_parent_session_ids(value: &Value, parent_ids: &mut BTreeSet<String>) {
    match value {
        Value::Object(object) => {
            for (key, nested) in object {
                let normalized_key = key.to_ascii_lowercase();
                let is_parent_key = normalized_key.contains("parent")
                    && (normalized_key.contains("session")
                        || normalized_key.contains("thread")
                        || normalized_key.contains("id"));
                if is_parent_key {
                    maybe_collect_session_id(nested, parent_ids);
                }
                if normalized_key == "parent" {
                    maybe_collect_session_id(nested, parent_ids);
                }
                collect_parent_session_ids(nested, parent_ids);
            }
        }
        Value::Array(values) => {
            for nested in values {
                collect_parent_session_ids(nested, parent_ids);
            }
        }
        _ => {}
    }
}

fn maybe_collect_session_id(value: &Value, parent_ids: &mut BTreeSet<String>) {
    match value {
        Value::String(raw) => {
            if let Some(session_id) = parse_session_id_like(raw) {
                parent_ids.insert(session_id);
            }
        }
        Value::Object(object) => {
            for key in ["sessionId", "session_id", "threadId", "thread_id", "id"] {
                if let Some(session_id) = object
                    .get(key)
                    .and_then(Value::as_str)
                    .and_then(parse_session_id_like)
                {
                    parent_ids.insert(session_id);
                }
            }
        }
        _ => {}
    }
}

fn parse_session_id_like(raw: &str) -> Option<String> {
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.len() != 36 {
        return None;
    }

    for (index, byte) in normalized.bytes().enumerate() {
        if [8, 13, 18, 23].contains(&index) {
            if byte != b'-' {
                return None;
            }
            continue;
        }

        if !byte.is_ascii_hexdigit() {
            return None;
        }
    }

    Some(normalized)
}

fn extract_child_excerpt(
    provider: ProviderKind,
    path: &Path,
    warnings: &mut Vec<String>,
) -> Vec<SubagentExcerptMessage> {
    let raw = match read_thread_raw(path) {
        Ok(raw) => raw,
        Err(err) => {
            warnings.push(format!(
                "failed reading child thread {}: {err}",
                path.display()
            ));
            return Vec::new();
        }
    };

    match render::extract_messages(provider, path, &raw) {
        Ok(messages) => messages
            .into_iter()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|message| SubagentExcerptMessage {
                role: message.role,
                text: message.text,
            })
            .collect(),
        Err(err) => {
            warnings.push(format!(
                "failed extracting child messages from {}: {err}",
                path.display()
            ));
            Vec::new()
        }
    }
}

fn resolve_opencode_subagent_view(
    uri: &AgentsUri,
    roots: &ProviderRoots,
    list: bool,
) -> Result<SubagentView> {
    let main_uri = main_thread_uri(uri);
    let resolved_main = resolve_thread(&main_uri, roots)?;

    let mut warnings = resolved_main.metadata.warnings.clone();
    let records = discover_opencode_agents(roots, &uri.session_id, &mut warnings)?;

    if list {
        let mut agents = Vec::new();
        for record in records {
            let analysis = inspect_opencode_child(&record.agent_id, roots, record.message_count);
            warnings.extend(analysis.warnings);

            agents.push(SubagentListItem {
                agent_id: record.agent_id.clone(),
                status: analysis.status,
                status_source: analysis.status_source,
                last_update: analysis.last_update.clone(),
                relation: record.relation,
                child_thread: analysis.child_thread,
            });
        }

        return Ok(SubagentView::List(SubagentListView {
            query: make_query(uri, None, true),
            agents,
            warnings,
        }));
    }

    let requested_agent = uri
        .agent_id
        .clone()
        .ok_or_else(|| XurlError::InvalidMode("missing agent id".to_string()))?;

    if let Some(record) = records
        .into_iter()
        .find(|record| record.agent_id == requested_agent)
    {
        let analysis = inspect_opencode_child(&record.agent_id, roots, record.message_count);
        warnings.extend(analysis.warnings);

        let lifecycle = vec![SubagentLifecycleEvent {
            timestamp: analysis.last_update.clone(),
            event: "session_parent_link".to_string(),
            detail: "session.parent_id points to main thread".to_string(),
        }];

        return Ok(SubagentView::Detail(SubagentDetailView {
            query: make_query(uri, Some(requested_agent), false),
            relation: record.relation,
            lifecycle,
            status: analysis.status,
            status_source: analysis.status_source,
            child_thread: analysis.child_thread,
            excerpt: analysis.excerpt,
            warnings,
        }));
    }

    warnings.push(format!(
        "agent not found for main_session_id={} agent_id={requested_agent}",
        uri.session_id
    ));

    Ok(SubagentView::Detail(SubagentDetailView {
        query: make_query(uri, Some(requested_agent), false),
        relation: SubagentRelation::default(),
        lifecycle: Vec::new(),
        status: STATUS_NOT_FOUND.to_string(),
        status_source: "inferred".to_string(),
        child_thread: None,
        excerpt: Vec::new(),
        warnings,
    }))
}

fn discover_opencode_agents(
    roots: &ProviderRoots,
    main_session_id: &str,
    warnings: &mut Vec<String>,
) -> Result<Vec<OpencodeAgentRecord>> {
    let db_path = opencode_db_path(roots);
    let conn = open_opencode_read_only_db(&db_path)?;

    let has_parent_id =
        opencode_session_table_has_parent_id(&conn).map_err(|source| XurlError::Sqlite {
            path: db_path.clone(),
            source,
        })?;
    if !has_parent_id {
        warnings.push(
            "opencode sqlite session table does not expose parent_id; cannot discover subagent relations"
                .to_string(),
        );
        return Ok(Vec::new());
    }

    let rows =
        query_opencode_children(&conn, main_session_id).map_err(|source| XurlError::Sqlite {
            path: db_path,
            source,
        })?;

    Ok(rows
        .into_iter()
        .map(|(agent_id, message_count)| {
            let mut relation = SubagentRelation {
                validated: true,
                ..SubagentRelation::default()
            };
            relation
                .evidence
                .push("opencode sqlite relation validated via session.parent_id".to_string());

            OpencodeAgentRecord {
                agent_id,
                relation,
                message_count,
            }
        })
        .collect())
}

fn query_opencode_children(
    conn: &Connection,
    main_session_id: &str,
) -> std::result::Result<Vec<(String, usize)>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT s.id, COUNT(m.id) AS message_count
         FROM session AS s
         LEFT JOIN message AS m ON m.session_id = s.id
         WHERE s.parent_id = ?1
         GROUP BY s.id
         ORDER BY s.id ASC",
    )?;

    let rows = stmt.query_map([main_session_id], |row| {
        let id = row.get::<_, String>(0)?;
        let message_count = row.get::<_, i64>(1)?;
        Ok((id, usize::try_from(message_count).unwrap_or(0)))
    })?;

    let mut children = Vec::new();
    for row in rows {
        children.push(row?);
    }
    Ok(children)
}

fn opencode_db_path(roots: &ProviderRoots) -> PathBuf {
    roots.opencode_root.join("opencode.db")
}

fn open_opencode_read_only_db(db_path: &Path) -> Result<Connection> {
    Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|source| {
        XurlError::Sqlite {
            path: db_path.to_path_buf(),
            source,
        }
    })
}

fn opencode_session_table_has_parent_id(
    conn: &Connection,
) -> std::result::Result<bool, rusqlite::Error> {
    let mut stmt = conn.prepare("PRAGMA table_info(session)")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;

    let mut has_parent_id = false;
    for row in rows {
        if row? == "parent_id" {
            has_parent_id = true;
            break;
        }
    }
    Ok(has_parent_id)
}

fn inspect_opencode_child(
    child_session_id: &str,
    roots: &ProviderRoots,
    message_count: usize,
) -> OpencodeChildAnalysis {
    let mut warnings = Vec::new();
    let resolved_child = match OpencodeProvider::new(&roots.opencode_root).resolve(child_session_id)
    {
        Ok(resolved) => resolved,
        Err(err) => {
            warnings.push(format!(
                "failed to materialize child session_id={child_session_id}: {err}"
            ));
            return OpencodeChildAnalysis {
                child_thread: None,
                status: STATUS_NOT_FOUND.to_string(),
                status_source: "inferred".to_string(),
                last_update: None,
                excerpt: Vec::new(),
                warnings,
            };
        }
    };

    let raw = match read_thread_raw(&resolved_child.path) {
        Ok(raw) => raw,
        Err(err) => {
            warnings.push(format!(
                "failed reading child session transcript session_id={child_session_id}: {err}"
            ));
            return OpencodeChildAnalysis {
                child_thread: Some(SubagentThreadRef {
                    thread_id: child_session_id.to_string(),
                    path: Some(resolved_child.path.display().to_string()),
                    last_updated_at: None,
                }),
                status: STATUS_NOT_FOUND.to_string(),
                status_source: "inferred".to_string(),
                last_update: None,
                excerpt: Vec::new(),
                warnings,
            };
        }
    };

    let messages =
        match render::extract_messages(ProviderKind::Opencode, &resolved_child.path, &raw) {
            Ok(messages) => messages,
            Err(err) => {
                warnings.push(format!(
                "failed extracting child transcript messages session_id={child_session_id}: {err}"
            ));
                Vec::new()
            }
        };

    if message_count == 0 {
        warnings.push(format!(
            "child session_id={child_session_id} has no materialized messages in sqlite"
        ));
    }

    let (status, status_source) = infer_opencode_status(&messages);
    let last_update = extract_opencode_last_update(&raw);

    let excerpt = messages
        .into_iter()
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|message| SubagentExcerptMessage {
            role: message.role,
            text: message.text,
        })
        .collect::<Vec<_>>();

    OpencodeChildAnalysis {
        child_thread: Some(SubagentThreadRef {
            thread_id: child_session_id.to_string(),
            path: Some(resolved_child.path.display().to_string()),
            last_updated_at: last_update.clone(),
        }),
        status,
        status_source,
        last_update,
        excerpt,
        warnings,
    }
}

fn infer_opencode_status(messages: &[crate::model::ThreadMessage]) -> (String, String) {
    let has_assistant = messages
        .iter()
        .any(|message| message.role == crate::model::MessageRole::Assistant);
    if has_assistant {
        return (STATUS_COMPLETED.to_string(), "child_rollout".to_string());
    }

    let has_user = messages
        .iter()
        .any(|message| message.role == crate::model::MessageRole::User);
    if has_user {
        return (STATUS_RUNNING.to_string(), "child_rollout".to_string());
    }

    (STATUS_PENDING_INIT.to_string(), "inferred".to_string())
}

fn extract_opencode_last_update(raw: &str) -> Option<String> {
    for line in raw.lines().rev() {
        if line.trim().is_empty() {
            continue;
        }

        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };

        if value.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }

        let Some(message) = value.get("message") else {
            continue;
        };

        let Some(time) = message.get("time") else {
            continue;
        };

        if let Some(completed) = value_to_timestamp_string(time.get("completed")) {
            return Some(completed);
        }
        if let Some(created) = value_to_timestamp_string(time.get("created")) {
            return Some(created);
        }
    }

    None
}

fn value_to_timestamp_string(value: Option<&Value>) -> Option<String> {
    let value = value?;
    value
        .as_str()
        .map(ToString::to_string)
        .or_else(|| value.as_i64().map(|number| number.to_string()))
        .or_else(|| value.as_u64().map(|number| number.to_string()))
}

fn discover_claude_agents(
    resolved_main: &ResolvedThread,
    main_session_id: &str,
    warnings: &mut Vec<String>,
) -> Vec<ClaudeAgentRecord> {
    let Some(project_dir) = resolved_main.path.parent() else {
        warnings.push(format!(
            "cannot determine project directory from resolved main thread path: {}",
            resolved_main.path.display()
        ));
        return Vec::new();
    };

    let mut candidate_files = BTreeSet::new();

    let nested_subagent_dir = project_dir.join(main_session_id).join("subagents");
    if nested_subagent_dir.exists()
        && let Ok(entries) = fs::read_dir(&nested_subagent_dir)
    {
        for entry in entries.filter_map(std::result::Result::ok) {
            let path = entry.path();
            if is_claude_agent_filename(&path) {
                candidate_files.insert(path);
            }
        }
    }

    if let Ok(entries) = fs::read_dir(project_dir) {
        for entry in entries.filter_map(std::result::Result::ok) {
            let path = entry.path();
            if is_claude_agent_filename(&path) {
                candidate_files.insert(path);
            }
        }
    }

    let mut latest_by_agent = BTreeMap::<String, ClaudeAgentRecord>::new();

    for path in candidate_files {
        let Some(record) = analyze_claude_agent_file(&path, main_session_id, warnings) else {
            continue;
        };

        match latest_by_agent.get(&record.agent_id) {
            Some(existing) => {
                let new_stamp = file_modified_epoch(&record.path).unwrap_or(0);
                let old_stamp = file_modified_epoch(&existing.path).unwrap_or(0);
                if new_stamp > old_stamp {
                    latest_by_agent.insert(record.agent_id.clone(), record);
                }
            }
            None => {
                latest_by_agent.insert(record.agent_id.clone(), record);
            }
        }
    }

    latest_by_agent.into_values().collect()
}

fn analyze_claude_agent_file(
    path: &Path,
    main_session_id: &str,
    warnings: &mut Vec<String>,
) -> Option<ClaudeAgentRecord> {
    let raw = match read_thread_raw(path) {
        Ok(raw) => raw,
        Err(err) => {
            warnings.push(format!(
                "failed to read Claude agent transcript {}: {err}",
                path.display()
            ));
            return None;
        }
    };

    let mut agent_id = None::<String>;
    let mut is_sidechain = false;
    let mut session_matches = false;
    let mut has_error = false;
    let mut has_assistant = false;
    let mut has_user = false;
    let mut last_update = None::<String>;

    for (line_idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }

        let value = match jsonl::parse_json_line(path, line_idx + 1, line) {
            Ok(Some(value)) => value,
            Ok(None) => continue,
            Err(err) => {
                warnings.push(format!(
                    "failed to parse Claude agent transcript line {} in {}: {err}",
                    line_idx + 1,
                    path.display()
                ));
                continue;
            }
        };

        if line_idx == 0 {
            agent_id = value
                .get("agentId")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            is_sidechain = value
                .get("isSidechain")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            session_matches = value
                .get("sessionId")
                .and_then(Value::as_str)
                .is_some_and(|session_id| session_id == main_session_id);
        }

        if let Some(timestamp) = value
            .get("timestamp")
            .and_then(Value::as_str)
            .map(ToString::to_string)
        {
            last_update = Some(timestamp);
        }

        if value
            .get("isApiErrorMessage")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || !value.get("error").is_none_or(Value::is_null)
        {
            has_error = true;
        }

        if let Some(kind) = value.get("type").and_then(Value::as_str) {
            if kind == "assistant" {
                has_assistant = true;
            }
            if kind == "user" {
                has_user = true;
            }
        }
    }

    if !is_sidechain || !session_matches {
        return None;
    }

    let Some(agent_id) = agent_id else {
        warnings.push(format!(
            "missing agentId in Claude sidechain transcript: {}",
            path.display()
        ));
        return None;
    };

    let status = if has_error {
        STATUS_ERRORED.to_string()
    } else if has_assistant {
        STATUS_COMPLETED.to_string()
    } else if has_user {
        STATUS_RUNNING.to_string()
    } else {
        STATUS_PENDING_INIT.to_string()
    };

    let excerpt = render::extract_messages(ProviderKind::Claude, path, &raw)
        .map(|messages| {
            messages
                .into_iter()
                .rev()
                .take(3)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(|message| SubagentExcerptMessage {
                    role: message.role,
                    text: message.text,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut relation = SubagentRelation {
        validated: true,
        ..SubagentRelation::default()
    };
    relation
        .evidence
        .push("agent transcript is sidechain and sessionId matches main thread".to_string());

    Some(ClaudeAgentRecord {
        agent_id,
        path: path.to_path_buf(),
        status,
        last_update: last_update.or_else(|| modified_timestamp_string(path)),
        relation,
        excerpt,
        warnings: Vec::new(),
    })
}

fn is_claude_agent_filename(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext == "jsonl")
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("agent-"))
}

fn file_modified_epoch(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
}

fn modified_timestamp_string(path: &Path) -> Option<String> {
    file_modified_epoch(path).map(|stamp| stamp.to_string())
}

fn normalize_agent_id(agent_id: &str) -> String {
    agent_id
        .strip_prefix("agent-")
        .unwrap_or(agent_id)
        .to_string()
}

fn extract_last_timestamp(raw: &str) -> Option<String> {
    for line in raw.lines().rev() {
        let Ok(Some(value)) = jsonl::parse_json_line(Path::new("<timestamp>"), 1, line) else {
            continue;
        };
        if let Some(timestamp) = value
            .get("timestamp")
            .and_then(Value::as_str)
            .map(ToString::to_string)
        {
            return Some(timestamp);
        }
    }

    None
}
fn collect_amp_query_candidates(
    roots: &ProviderRoots,
    warnings: &mut Vec<String>,
) -> Vec<QueryCandidate> {
    let threads_root = roots.amp_root.join("threads");
    collect_simple_file_candidates(
        ProviderKind::Amp,
        &threads_root,
        |path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        },
        |path| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(ToString::to_string)
        },
        extract_amp_scope_path,
        warnings,
    )
}

fn collect_codex_query_candidates(
    roots: &ProviderRoots,
    warnings: &mut Vec<String>,
) -> Vec<QueryCandidate> {
    let mut candidates = Vec::new();
    candidates.extend(collect_simple_file_candidates(
        ProviderKind::Codex,
        &roots.codex_root.join("sessions"),
        |path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
        },
        extract_codex_rollout_id,
        extract_codex_scope_path,
        warnings,
    ));
    candidates.extend(collect_simple_file_candidates(
        ProviderKind::Codex,
        &roots.codex_root.join("archived_sessions"),
        |path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
        },
        extract_codex_rollout_id,
        extract_codex_scope_path,
        warnings,
    ));
    candidates
}

fn collect_claude_query_candidates(
    roots: &ProviderRoots,
    warnings: &mut Vec<String>,
) -> Vec<QueryCandidate> {
    let projects_root = roots.claude_root.join("projects");
    if !projects_root.exists() {
        return Vec::new();
    }

    let mut candidates = Vec::new();
    for entry in WalkDir::new(&projects_root)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.into_path();
        if path.file_name().and_then(|name| name.to_str()) == Some("sessions-index.json") {
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }

        if let Some((thread_id, uri)) = extract_claude_thread_identity(&path) {
            let scope_path = extract_claude_scope_path(&path);
            candidates.push(make_file_candidate(
                ProviderKind::Claude,
                thread_id,
                uri,
                path,
                scope_path,
            ));
        } else {
            warnings.push(format!(
                "skipped claude transcript with unknown thread identity: {}",
                path.display()
            ));
        }
    }

    candidates
}

fn collect_gemini_query_candidates(
    roots: &ProviderRoots,
    warnings: &mut Vec<String>,
) -> Vec<QueryCandidate> {
    let tmp_root = roots.gemini_root.join("tmp");
    if !tmp_root.exists() {
        return Vec::new();
    }

    let mut candidates = Vec::new();
    for entry in WalkDir::new(&tmp_root)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.into_path();
        let is_session_file = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("session-") && name.ends_with(".json"));
        let in_chats_dir = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "chats");
        if !(is_session_file && in_chats_dir) {
            continue;
        }

        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) => {
                warnings.push(format!(
                    "failed reading gemini transcript {}: {err}",
                    path.display()
                ));
                continue;
            }
        };
        let value = match serde_json::from_str::<Value>(&raw) {
            Ok(value) => value,
            Err(err) => {
                warnings.push(format!(
                    "failed parsing gemini transcript {} as json: {err}",
                    path.display()
                ));
                continue;
            }
        };
        let Some(session_id) = value.get("sessionId").and_then(Value::as_str) else {
            warnings.push(format!(
                "gemini transcript does not contain sessionId: {}",
                path.display()
            ));
            continue;
        };
        if !is_uuid_session_id(session_id) {
            warnings.push(format!(
                "gemini transcript contains non-uuid sessionId={session_id}: {}",
                path.display()
            ));
            continue;
        }
        let session_id = session_id.to_ascii_lowercase();
        let scope_path = extract_gemini_scope_path(&path);
        candidates.push(make_file_candidate(
            ProviderKind::Gemini,
            session_id.clone(),
            format!("agents://gemini/{session_id}"),
            path,
            scope_path,
        ));
    }

    candidates
}

fn collect_pi_query_candidates(
    roots: &ProviderRoots,
    warnings: &mut Vec<String>,
) -> Vec<QueryCandidate> {
    let sessions_root = roots.pi_root.join("sessions");
    if !sessions_root.exists() {
        return Vec::new();
    }

    let mut candidates = Vec::new();
    for entry in WalkDir::new(&sessions_root)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.into_path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }

        match extract_pi_session_id_from_header(&path) {
            Ok(Some(session_id)) => {
                let session_id = session_id.to_ascii_lowercase();
                let scope_path = extract_pi_scope_path(&path);
                candidates.push(make_file_candidate(
                    ProviderKind::Pi,
                    session_id.clone(),
                    format!("agents://pi/{session_id}"),
                    path,
                    scope_path,
                ));
            }
            Ok(None) => {}
            Err(err) => warnings.push(err),
        }
    }

    candidates
}

fn collect_opencode_query_candidates(
    roots: &ProviderRoots,
    warnings: &mut Vec<String>,
    with_search_text: bool,
) -> Result<Vec<QueryCandidate>> {
    let db_path = roots.opencode_root.join("opencode.db");
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(
        |source| XurlError::Sqlite {
            path: db_path.clone(),
            source,
        },
    )?;

    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.directory, COALESCE(MAX(m.time_created), 0)
             FROM session s
             LEFT JOIN message m ON m.session_id = s.id
             GROUP BY s.id, s.directory
             ORDER BY COALESCE(MAX(m.time_created), 0) DESC, s.id DESC",
        )
        .map_err(|source| XurlError::Sqlite {
            path: db_path.clone(),
            source,
        })?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)
                    .ok()
                    .and_then(|stamp| u64::try_from(stamp).ok()),
            ))
        })
        .map_err(|source| XurlError::Sqlite {
            path: db_path.clone(),
            source,
        })?;

    let mut candidates = Vec::new();
    for row in rows {
        let (session_id, directory, updated_epoch) = row.map_err(|source| XurlError::Sqlite {
            path: db_path.clone(),
            source,
        })?;
        if AgentsUri::parse(&format!("opencode://{session_id}")).is_err() {
            warnings.push(format!(
                "skipped opencode session with invalid id={session_id} from {}",
                db_path.display()
            ));
            continue;
        }
        let search_target = if with_search_text {
            QuerySearchTarget::Text(fetch_opencode_search_text(&conn, &db_path, &session_id)?)
        } else {
            QuerySearchTarget::Text(String::new())
        };

        candidates.push(QueryCandidate {
            provider: ProviderKind::Opencode,
            thread_id: session_id.clone(),
            uri: format!("agents://opencode/{session_id}"),
            thread_source: format!("{}#session:{session_id}", db_path.display()),
            updated_at: updated_epoch.map(|value| value.to_string()),
            updated_epoch,
            scope_path: directory.as_deref().and_then(scope_path_from_str),
            search_target,
        });
    }

    Ok(candidates)
}

fn fetch_opencode_search_text(
    conn: &Connection,
    db_path: &Path,
    session_id: &str,
) -> Result<String> {
    let mut chunks = Vec::new();

    let mut message_stmt = conn
        .prepare(
            "SELECT data
             FROM message
             WHERE session_id = ?1
             ORDER BY time_created ASC, id ASC",
        )
        .map_err(|source| XurlError::Sqlite {
            path: db_path.to_path_buf(),
            source,
        })?;
    let message_rows = message_stmt
        .query_map([session_id], |row| row.get::<_, String>(0))
        .map_err(|source| XurlError::Sqlite {
            path: db_path.to_path_buf(),
            source,
        })?;
    for row in message_rows {
        let value = row.map_err(|source| XurlError::Sqlite {
            path: db_path.to_path_buf(),
            source,
        })?;
        chunks.push(value);
    }

    let mut part_stmt = conn
        .prepare(
            "SELECT data
             FROM part
             WHERE session_id = ?1
             ORDER BY time_created ASC, id ASC",
        )
        .map_err(|source| XurlError::Sqlite {
            path: db_path.to_path_buf(),
            source,
        })?;
    let part_rows = part_stmt
        .query_map([session_id], |row| row.get::<_, String>(0))
        .map_err(|source| XurlError::Sqlite {
            path: db_path.to_path_buf(),
            source,
        })?;
    for row in part_rows {
        let value = row.map_err(|source| XurlError::Sqlite {
            path: db_path.to_path_buf(),
            source,
        })?;
        chunks.push(value);
    }

    Ok(chunks.join("\n"))
}

fn collect_simple_file_candidates<F, G, H>(
    provider: ProviderKind,
    root: &Path,
    path_filter: F,
    thread_id_extractor: G,
    scope_path_extractor: H,
    warnings: &mut Vec<String>,
) -> Vec<QueryCandidate>
where
    F: Fn(&Path) -> bool,
    G: Fn(&Path) -> Option<String>,
    H: Fn(&Path) -> Option<PathBuf>,
{
    if !root.exists() {
        return Vec::new();
    }

    let mut candidates = Vec::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.into_path();
        if !path_filter(&path) {
            continue;
        }
        let Some(thread_id) = thread_id_extractor(&path) else {
            warnings.push(format!(
                "skipped {} transcript with unknown thread id: {}",
                provider,
                path.display()
            ));
            continue;
        };
        candidates.push(make_file_candidate(
            provider,
            thread_id.clone(),
            format!("agents://{provider}/{thread_id}"),
            path.clone(),
            scope_path_extractor(&path),
        ));
    }

    candidates
}

fn make_file_candidate(
    provider: ProviderKind,
    thread_id: String,
    uri: String,
    path: PathBuf,
    scope_path: Option<PathBuf>,
) -> QueryCandidate {
    QueryCandidate {
        provider,
        thread_id,
        uri,
        thread_source: path.display().to_string(),
        updated_at: modified_timestamp_string(&path),
        updated_epoch: file_modified_epoch(&path),
        scope_path,
        search_target: QuerySearchTarget::File(path),
    }
}

fn extract_codex_rollout_id(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".jsonl")?;
    if stem.len() < 36 {
        return None;
    }
    let thread_id = &stem[stem.len() - 36..];
    if is_uuid_session_id(thread_id) {
        Some(thread_id.to_ascii_lowercase())
    } else {
        None
    }
}

fn extract_claude_thread_identity(path: &Path) -> Option<(String, String)> {
    let file_name = path.file_name()?.to_str()?;
    if let Some(agent_id) = file_name
        .strip_prefix("agent-")
        .and_then(|name| name.strip_suffix(".jsonl"))
    {
        let subagents_dir = path.parent()?;
        if subagents_dir.file_name()?.to_str()? != "subagents" {
            return None;
        }
        let main_thread_id = subagents_dir.parent()?.file_name()?.to_str()?.to_string();
        return Some((
            format!("{main_thread_id}/{agent_id}"),
            format!("agents://claude/{main_thread_id}/{agent_id}"),
        ));
    }

    if let Some(session_id) = extract_claude_session_id_from_header(path) {
        return Some((session_id.clone(), format!("agents://claude/{session_id}")));
    }

    let file_stem = path.file_stem()?.to_str()?;
    if is_uuid_session_id(file_stem) {
        let session_id = file_stem.to_ascii_lowercase();
        return Some((session_id.clone(), format!("agents://claude/{session_id}")));
    }

    None
}

fn extract_claude_session_id_from_header(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(30).flatten() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let session_id = value.get("sessionId").and_then(Value::as_str)?;
        if is_uuid_session_id(session_id) {
            return Some(session_id.to_ascii_lowercase());
        }
    }
    None
}

fn extract_pi_session_id_from_header(path: &Path) -> std::result::Result<Option<String>, String> {
    let file =
        fs::File::open(path).map_err(|err| format!("failed opening {}: {err}", path.display()))?;
    let reader = BufReader::new(file);
    let Some(first_non_empty) = reader
        .lines()
        .take(30)
        .filter_map(std::result::Result::ok)
        .find(|line| !line.trim().is_empty())
    else {
        return Ok(None);
    };
    let value = serde_json::from_str::<Value>(&first_non_empty)
        .map_err(|err| format!("failed parsing pi header {}: {err}", path.display()))?;
    if value.get("type").and_then(Value::as_str) != Some("session") {
        return Ok(None);
    }
    let Some(session_id) = value.get("id").and_then(Value::as_str) else {
        return Ok(None);
    };
    if !is_uuid_session_id(session_id) {
        return Err(format!(
            "pi session header contains invalid session id={session_id}: {}",
            path.display()
        ));
    }
    Ok(Some(session_id.to_ascii_lowercase()))
}

fn main_thread_uri(uri: &AgentsUri) -> AgentsUri {
    AgentsUri {
        provider: uri.provider,
        session_id: uri.session_id.clone(),
        agent_id: None,
        query: Vec::new(),
    }
}

fn make_query(uri: &AgentsUri, agent_id: Option<String>, list: bool) -> SubagentQuery {
    SubagentQuery {
        provider: uri.provider.to_string(),
        main_thread_id: uri.session_id.clone(),
        agent_id,
        list,
    }
}

fn agents_thread_uri(provider: &str, thread_id: &str, agent_id: Option<&str>) -> String {
    match agent_id {
        Some(agent_id) => format!("agents://{provider}/{thread_id}/{agent_id}"),
        None => format!("agents://{provider}/{thread_id}"),
    }
}

fn render_preview_text(content: &Value, max_chars: usize) -> String {
    let text = if content.is_string() {
        content.as_str().unwrap_or_default().to_string()
    } else if let Some(items) = content.as_array() {
        items
            .iter()
            .filter_map(|item| {
                item.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| item.as_str())
            })
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        String::new()
    };

    truncate_preview(&text, max_chars)
}

fn truncate_preview(input: &str, max_chars: usize) -> String {
    let normalized = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }

    let mut out = String::new();
    for (idx, ch) in normalized.chars().enumerate() {
        if idx >= max_chars.saturating_sub(1) {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

fn render_subagent_list_markdown(view: &SubagentListView) -> String {
    let main_thread_uri = agents_thread_uri(&view.query.provider, &view.query.main_thread_id, None);
    let mut output = String::new();
    output.push_str("# Subagent Status\n\n");
    output.push_str(&format!("- Provider: `{}`\n", view.query.provider));
    output.push_str(&format!("- Main Thread: `{}`\n", main_thread_uri));
    output.push_str("- Mode: `list`\n\n");

    if view.agents.is_empty() {
        output.push_str("_No subagents found for this thread._\n");
        return output;
    }

    for (index, agent) in view.agents.iter().enumerate() {
        let agent_uri = format!("{}/{}", main_thread_uri, agent.agent_id);
        output.push_str(&format!("## {}. `{}`\n\n", index + 1, agent_uri));
        output.push_str(&format!(
            "- Status: `{}` (`{}`)\n",
            agent.status, agent.status_source
        ));
        output.push_str(&format!(
            "- Last Update: `{}`\n",
            agent.last_update.as_deref().unwrap_or("unknown")
        ));
        output.push_str(&format!(
            "- Relation: `{}`\n",
            if agent.relation.validated {
                "validated"
            } else {
                "inferred"
            }
        ));
        if let Some(thread) = &agent.child_thread
            && let Some(path) = &thread.path
        {
            output.push_str(&format!("- Thread Path: `{}`\n", path));
        }
        output.push('\n');
    }

    output
}

fn render_subagent_detail_markdown(view: &SubagentDetailView) -> String {
    let main_thread_uri = agents_thread_uri(&view.query.provider, &view.query.main_thread_id, None);
    let mut output = String::new();
    output.push_str("# Subagent Thread\n\n");
    output.push_str(&format!("- Provider: `{}`\n", view.query.provider));
    output.push_str(&format!("- Main Thread: `{}`\n", main_thread_uri));
    if let Some(agent_id) = &view.query.agent_id {
        output.push_str(&format!(
            "- Subagent Thread: `{}/{}`\n",
            main_thread_uri, agent_id
        ));
    }
    output.push_str(&format!(
        "- Status: `{}` (`{}`)\n\n",
        view.status, view.status_source
    ));

    output.push_str("## Agent Status Summary\n\n");
    output.push_str(&format!(
        "- Relation: `{}`\n",
        if view.relation.validated {
            "validated"
        } else {
            "inferred"
        }
    ));
    for evidence in &view.relation.evidence {
        output.push_str(&format!("- Evidence: {}\n", evidence));
    }
    if let Some(thread) = &view.child_thread {
        if let Some(path) = &thread.path {
            output.push_str(&format!("- Child Path: `{}`\n", path));
        }
        if let Some(last_updated_at) = &thread.last_updated_at {
            output.push_str(&format!("- Child Last Update: `{}`\n", last_updated_at));
        }
    }
    output.push('\n');

    output.push_str("## Lifecycle (Parent Thread)\n\n");
    if view.lifecycle.is_empty() {
        output.push_str("_No lifecycle events found in parent thread._\n\n");
    } else {
        for event in &view.lifecycle {
            output.push_str(&format!(
                "- `{}` `{}` {}\n",
                event.timestamp.as_deref().unwrap_or("unknown"),
                event.event,
                event.detail
            ));
        }
        output.push('\n');
    }

    output.push_str("## Thread Excerpt (Child Thread)\n\n");
    if view.excerpt.is_empty() {
        output.push_str("_No child thread messages found._\n\n");
    } else {
        for (index, message) in view.excerpt.iter().enumerate() {
            let title = match message.role {
                crate::model::MessageRole::User => "User",
                crate::model::MessageRole::Assistant => "Assistant",
            };
            output.push_str(&format!("### {}. {}\n\n", index + 1, title));
            output.push_str(message.text.trim());
            output.push_str("\n\n");
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;

    use crate::service::{
        collect_claude_thread_metadata, collect_codex_thread_metadata, collect_pi_thread_metadata,
        extract_last_timestamp, read_thread_raw,
    };
    use crate::{
        ProviderKind, ThreadQuery, ThreadQueryItem, ThreadQueryResult,
        render_thread_query_head_markdown,
    };

    #[test]
    fn empty_file_returns_error() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("thread.jsonl");
        fs::write(&path, "").expect("write");

        let err = read_thread_raw(&path).expect_err("must fail");
        assert!(format!("{err}").contains("thread file is empty"));
    }

    #[test]
    fn extract_last_timestamp_from_jsonl() {
        let raw =
            "{\"timestamp\":\"2026-02-23T00:00:01Z\"}\n{\"timestamp\":\"2026-02-23T00:00:02Z\"}\n";
        let timestamp = extract_last_timestamp(raw).expect("must extract timestamp");
        assert_eq!(timestamp, "2026-02-23T00:00:02Z");
    }

    #[test]
    fn codex_thread_metadata_flattens_records_to_key_value_lines() {
        let raw = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/tmp/project\",\"model_provider\":\"openai\",\"base_instructions\":{\"text\":\"very long\"},\"git\":{\"branch\":\"main\",\"commit_hash\":\"deadbeef\"}}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.3-codex\",\"approval_policy\":\"never\",\"sandbox_policy\":{\"type\":\"danger-full-access\"}}}\n",
        );

        let (metadata, warnings) = collect_codex_thread_metadata(Path::new("/tmp/mock"), raw);
        assert!(warnings.is_empty());
        assert!(metadata.iter().any(|item| item == "type = session_meta"));
        assert!(
            metadata
                .iter()
                .any(|item| item == "payload.cwd = /tmp/project")
        );
        assert!(
            metadata
                .iter()
                .any(|item| item == "payload.git.branch = main")
        );
        assert!(
            metadata
                .iter()
                .any(|item| item == "payload.git.commit_hash = deadbeef")
        );
        assert!(
            !metadata
                .iter()
                .any(|item| item.contains("base_instructions"))
        );
        assert!(!metadata.iter().any(|item| item.contains("payload.model =")));
    }

    #[test]
    fn claude_thread_metadata_flattens_raw_keys() {
        let raw = "{\"type\":\"user\",\"cwd\":\"/tmp/project\",\"gitBranch\":\"feature/x\",\"version\":\"1.2.3\"}\n";

        let (metadata, warnings) = collect_claude_thread_metadata(Path::new("/tmp/mock"), raw);
        assert!(warnings.is_empty());
        assert!(metadata.iter().any(|item| item == "type = user"));
        assert!(metadata.iter().any(|item| item == "cwd = /tmp/project"));
        assert!(metadata.iter().any(|item| item == "gitBranch = feature/x"));
        assert!(metadata.iter().any(|item| item == "version = 1.2.3"));
    }

    #[test]
    fn pi_thread_metadata_flattens_raw_records() {
        let raw = concat!(
            "{\"type\":\"session\",\"id\":\"12cb4c19-2774-4de4-a0d0-9fa32fbae29f\",\"cwd\":\"/tmp/project\"}\n",
            "{\"type\":\"model_change\",\"modelId\":\"gpt-5.3-codex\"}\n",
            "{\"type\":\"thinking_level_change\",\"thinkingLevel\":\"medium\"}\n",
        );

        let (metadata, warnings) = collect_pi_thread_metadata(Path::new("/tmp/mock"), raw);
        assert!(warnings.is_empty());
        assert!(metadata.iter().any(|item| item == "type = session"));
        assert!(
            metadata
                .iter()
                .any(|item| item == "id = 12cb4c19-2774-4de4-a0d0-9fa32fbae29f")
        );
        assert!(metadata.iter().any(|item| item == "cwd = /tmp/project"));
        assert!(!metadata.iter().any(|item| item.contains("model_change")));
        assert!(
            !metadata
                .iter()
                .any(|item| item.contains("thinking_level_change"))
        );
    }

    #[test]
    fn render_thread_query_head_renders_metadata_entries() {
        let result = ThreadQueryResult {
            query: ThreadQuery {
                uri: "agents://codex?limit=1".to_string(),
                provider: ProviderKind::Codex,
                role: None,
                q: None,
                limit: 1,
                ignored_params: Vec::new(),
            },
            items: vec![ThreadQueryItem {
                provider: ProviderKind::Codex,
                thread_id: "019c871c-b1f9-7f60-9c4f-87ed09f13592".to_string(),
                uri: "agents://codex/019c871c-b1f9-7f60-9c4f-87ed09f13592".to_string(),
                thread_source: "/tmp/mock.jsonl".to_string(),
                updated_at: Some("123".to_string()),
                matched_preview: None,
                thread_metadata: Some(vec![
                    "type = session_meta".to_string(),
                    "payload.cwd = /tmp/project".to_string(),
                ]),
            }],
            warnings: Vec::new(),
        };

        let output = render_thread_query_head_markdown(&result);
        assert!(output.contains("thread_metadata:"));
        assert!(output.contains("payload.cwd = /tmp/project"));
    }
}
