use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ProviderKind {
    Amp,
    Codex,
    Claude,
    Gemini,
    Pi,
    Opencode,
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Amp => write!(f, "amp"),
            Self::Codex => write!(f, "codex"),
            Self::Claude => write!(f, "claude"),
            Self::Gemini => write!(f, "gemini"),
            Self::Pi => write!(f, "pi"),
            Self::Opencode => write!(f, "opencode"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolutionMeta {
    pub source: String,
    pub candidate_count: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedThread {
    pub provider: ProviderKind,
    pub session_id: String,
    pub path: PathBuf,
    pub metadata: ResolutionMeta,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteRequest {
    pub prompt: String,
    pub session_id: Option<String>,
    pub options: WriteOptions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteResult {
    pub provider: ProviderKind,
    pub session_id: String,
    pub final_text: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriteOptions {
    pub params: Vec<(String, Option<String>)>,
    pub role: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MessageRole {
    User,
    Assistant,
}

impl fmt::Display for MessageRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => write!(f, "user"),
            Self::Assistant => write!(f, "assistant"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadMessage {
    pub role: MessageRole,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubagentQuery {
    pub provider: String,
    pub main_thread_id: String,
    pub agent_id: Option<String>,
    pub list: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SubagentRelation {
    pub validated: bool,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubagentLifecycleEvent {
    pub timestamp: Option<String>,
    pub event: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubagentExcerptMessage {
    pub role: MessageRole,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubagentThreadRef {
    pub thread_id: String,
    pub path: Option<String>,
    pub last_updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubagentDetailView {
    pub query: SubagentQuery,
    pub relation: SubagentRelation,
    pub lifecycle: Vec<SubagentLifecycleEvent>,
    pub status: String,
    pub status_source: String,
    pub child_thread: Option<SubagentThreadRef>,
    pub excerpt: Vec<SubagentExcerptMessage>,
    #[serde(skip_serializing)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubagentListItem {
    pub agent_id: String,
    pub status: String,
    pub status_source: String,
    pub last_update: Option<String>,
    pub relation: SubagentRelation,
    pub child_thread: Option<SubagentThreadRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubagentListView {
    pub query: SubagentQuery,
    pub agents: Vec<SubagentListItem>,
    #[serde(skip_serializing)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubagentView {
    List(SubagentListView),
    Detail(SubagentDetailView),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PiEntryQuery {
    pub provider: String,
    pub session_id: String,
    pub list: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PiEntryListItem {
    pub entry_id: String,
    pub entry_type: String,
    pub parent_id: Option<String>,
    pub timestamp: Option<String>,
    pub is_leaf: bool,
    pub preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PiEntryListView {
    pub query: PiEntryQuery,
    pub entries: Vec<PiEntryListItem>,
    #[serde(skip_serializing)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThreadQuery {
    pub uri: String,
    pub provider: ProviderKind,
    pub role: Option<String>,
    pub q: Option<String>,
    pub limit: usize,
    pub ignored_params: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PathThreadQuery {
    pub uri: String,
    pub scope_path: String,
    pub providers: Option<Vec<ProviderKind>>,
    pub q: Option<String>,
    pub limit: usize,
    pub ignored_params: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThreadQueryItem {
    pub provider: ProviderKind,
    pub thread_id: String,
    pub uri: String,
    pub thread_source: String,
    pub updated_at: Option<String>,
    pub matched_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_metadata: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThreadQueryResult {
    pub query: ThreadQuery,
    pub items: Vec<ThreadQueryItem>,
    #[serde(skip_serializing)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PathThreadQueryResult {
    pub query: PathThreadQuery,
    pub items: Vec<ThreadQueryItem>,
    #[serde(skip_serializing)]
    pub warnings: Vec<String>,
}
