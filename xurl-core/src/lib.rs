pub mod error;
pub mod jsonl;
pub mod model;
pub mod provider;
pub mod render;
pub mod service;
pub mod uri;

pub use error::{Result, XurlError};
pub use model::{
    MessageRole, PiEntryListView, ProviderKind, ResolutionMeta, ResolvedThread, SubagentDetailView,
    SubagentListView, SubagentView, ThreadMessage, ThreadQuery, ThreadQueryItem, ThreadQueryResult,
    WriteOptions, WriteRequest, WriteResult,
};
pub use provider::{ProviderRoots, WriteEventSink};
pub use service::{
    query_threads, render_subagent_view_markdown, render_thread_head_markdown,
    render_thread_markdown, render_thread_query_head_markdown, render_thread_query_markdown,
    resolve_subagent_view, resolve_thread, write_thread,
};
pub use uri::AgentsUri;
