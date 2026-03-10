use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum XurlError {
    #[error("invalid uri: {0}")]
    InvalidUri(String),

    #[error("unsupported scheme: {0}")]
    UnsupportedScheme(String),

    #[error("invalid session id: {0}")]
    InvalidSessionId(String),

    #[error("invalid mode: {0}")]
    InvalidMode(String),

    #[error("provider does not support subagent queries: {0}")]
    UnsupportedSubagentProvider(String),

    #[error("provider does not support write mode: {0}")]
    UnsupportedProviderWrite(String),

    #[error("command not found: {command}")]
    CommandNotFound { command: String },

    #[error("command failed: {command} (exit code: {code:?}): {stderr}")]
    CommandFailed {
        command: String,
        code: Option<i32>,
        stderr: String,
    },

    #[error("write protocol error: {0}")]
    WriteProtocol(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("cannot determine home directory")]
    HomeDirectoryNotFound,

    #[error("thread not found for provider={provider} session_id={session_id}")]
    ThreadNotFound {
        provider: String,
        session_id: String,
        searched_roots: Vec<PathBuf>,
    },

    #[error("entry not found for provider={provider} session_id={session_id} entry_id={entry_id}")]
    EntryNotFound {
        provider: String,
        session_id: String,
        entry_id: String,
    },

    #[error("thread file is empty: {path}")]
    EmptyThreadFile { path: PathBuf },

    #[error("thread file is not valid UTF-8: {path}")]
    NonUtf8ThreadFile { path: PathBuf },

    #[error("i/o error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("sqlite error on {path}: {source}")]
    Sqlite {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },

    #[error("invalid json line in {path} at line {line}: {source}")]
    InvalidJsonLine {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
}

pub type Result<T> = std::result::Result<T, XurlError>;
