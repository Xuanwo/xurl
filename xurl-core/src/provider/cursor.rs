use std::cmp::Reverse;
use std::collections::HashSet;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use serde_json::{Value, json};
use walkdir::WalkDir;

use crate::error::{Result, XurlError};
use crate::model::{ProviderKind, ResolutionMeta, ResolvedThread, WriteRequest, WriteResult};
use crate::provider::{Provider, WriteEventSink, append_passthrough_args};

static FILE_URI_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"file:///[^\x00-\x1f"']+"#).expect("file uri regex must be valid"));

const MAX_PROTO_SCAN_DEPTH: usize = 8;

#[derive(Debug, Clone)]
pub struct CursorProvider {
    root: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CursorMaterializedMetadata {
    pub name: Option<String>,
    pub mode: Option<String>,
    pub model: Option<String>,
    pub workspace_path: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CursorMaterialization {
    pub path: PathBuf,
    pub search_text: String,
    pub metadata: CursorMaterializedMetadata,
}

#[derive(Debug, Deserialize)]
struct CursorChatMeta {
    #[serde(rename = "agentId")]
    agent_id: String,
    #[serde(rename = "latestRootBlobId")]
    latest_root_blob_id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(rename = "lastUsedModel", default)]
    last_used_model: Option<String>,
}

#[derive(Debug, Clone)]
struct CursorMessage {
    id: String,
    role: String,
    parts: Vec<Value>,
}

#[derive(Debug, Default)]
struct CursorCollector {
    known_blob_ids: HashSet<String>,
    visited_blob_ids: HashSet<String>,
    seen_message_keys: HashSet<String>,
    messages: Vec<CursorMessage>,
    workspace_path: Option<String>,
}

impl CursorProvider {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn chats_root(&self) -> PathBuf {
        self.root.join("chats")
    }

    fn materialized_path(&self, session_id: &str) -> PathBuf {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.root.hash(&mut hasher);
        let root_key = format!("{:016x}", hasher.finish());

        std::env::temp_dir()
            .join("xurl-cursor")
            .join(root_key)
            .join(format!("{session_id}.jsonl"))
    }

    pub(crate) fn find_store_candidates(&self, session_id: &str) -> Vec<PathBuf> {
        let chats_root = self.chats_root();
        if !chats_root.exists() {
            return Vec::new();
        }

        WalkDir::new(chats_root)
            .min_depth(3)
            .max_depth(3)
            .into_iter()
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.into_path())
            .filter(|path| {
                path.file_name().and_then(|name| name.to_str()) == Some("store.db")
                    && path
                        .parent()
                        .and_then(Path::file_name)
                        .and_then(|name| name.to_str())
                        == Some(session_id)
            })
            .collect()
    }

    fn choose_latest(paths: Vec<PathBuf>) -> Option<(PathBuf, usize)> {
        if paths.is_empty() {
            return None;
        }

        let mut scored = paths
            .into_iter()
            .map(|path| {
                let modified = fs::metadata(&path)
                    .and_then(|meta| meta.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                (path, modified)
            })
            .collect::<Vec<_>>();

        scored.sort_by_key(|(_, modified)| Reverse(*modified));
        let count = scored.len();
        scored.into_iter().next().map(|(path, _)| (path, count))
    }

    fn open_store(path: &Path) -> Result<Connection> {
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|source| {
            XurlError::Sqlite {
                path: path.to_path_buf(),
                source,
            }
        })
    }

    fn decode_hex_bytes(raw: &str, path: &Path) -> Result<Vec<u8>> {
        if raw.len() % 2 != 0 {
            return Err(XurlError::InvalidMode(format!(
                "cursor meta payload in {} is not valid hex",
                path.display()
            )));
        }

        let bytes = raw.as_bytes();
        let mut output = Vec::with_capacity(bytes.len() / 2);
        let mut index = 0;
        while index < bytes.len() {
            let hi = Self::decode_hex_nibble(bytes[index]).ok_or_else(|| {
                XurlError::InvalidMode(format!(
                    "cursor meta payload in {} is not valid hex",
                    path.display()
                ))
            })?;
            let lo = Self::decode_hex_nibble(bytes[index + 1]).ok_or_else(|| {
                XurlError::InvalidMode(format!(
                    "cursor meta payload in {} is not valid hex",
                    path.display()
                ))
            })?;
            output.push((hi << 4) | lo);
            index += 2;
        }

        Ok(output)
    }

    fn decode_hex_nibble(ch: u8) -> Option<u8> {
        match ch {
            b'0'..=b'9' => Some(ch - b'0'),
            b'a'..=b'f' => Some(10 + ch - b'a'),
            b'A'..=b'F' => Some(10 + ch - b'A'),
            _ => None,
        }
    }

    fn bytes_to_hex(bytes: &[u8]) -> String {
        let mut output = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            output.push_str(&format!("{byte:02x}"));
        }
        output
    }

    fn parse_chat_meta(conn: &Connection, db_path: &Path) -> Result<CursorChatMeta> {
        let raw = conn
            .query_row(
                "SELECT value FROM meta WHERE key = '0' LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .map_err(|source| XurlError::Sqlite {
                path: db_path.to_path_buf(),
                source,
            })?;
        let bytes = Self::decode_hex_bytes(&raw, db_path)?;
        serde_json::from_slice::<CursorChatMeta>(&bytes).map_err(|source| {
            XurlError::InvalidMode(format!(
                "failed parsing cursor chat meta {}: {source}",
                db_path.display()
            ))
        })
    }

    fn fetch_blob_bytes(
        conn: &Connection,
        db_path: &Path,
        blob_id: &str,
    ) -> Result<Option<Vec<u8>>> {
        conn.query_row(
            "SELECT data FROM blobs WHERE id = ?1 LIMIT 1",
            [blob_id],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .map(Some)
        .or_else(|source| match source {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(XurlError::Sqlite {
                path: db_path.to_path_buf(),
                source: other,
            }),
        })
    }

    fn load_blob_index(conn: &Connection, db_path: &Path) -> Result<HashSet<String>> {
        let mut stmt =
            conn.prepare("SELECT id FROM blobs")
                .map_err(|source| XurlError::Sqlite {
                    path: db_path.to_path_buf(),
                    source,
                })?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|source| XurlError::Sqlite {
                path: db_path.to_path_buf(),
                source,
            })?;

        let mut ids = HashSet::new();
        for row in rows {
            ids.insert(row.map_err(|source| XurlError::Sqlite {
                path: db_path.to_path_buf(),
                source,
            })?);
        }

        Ok(ids)
    }

    pub(crate) fn materialize_store(
        &self,
        store_path: &Path,
        session_id: &str,
    ) -> Result<CursorMaterialization> {
        let conn = Self::open_store(store_path)?;
        let chat_meta = Self::parse_chat_meta(&conn, store_path)?;
        if chat_meta.agent_id.to_ascii_lowercase() != session_id {
            return Err(XurlError::InvalidMode(format!(
                "cursor store {} belongs to session {} instead of {}",
                store_path.display(),
                chat_meta.agent_id,
                session_id
            )));
        }

        let known_blob_ids = Self::load_blob_index(&conn, store_path)?;
        let mut collector = CursorCollector {
            known_blob_ids,
            ..CursorCollector::default()
        };
        collector.walk_blob(&conn, store_path, &chat_meta.latest_root_blob_id)?;

        let metadata = CursorMaterializedMetadata {
            name: chat_meta.name,
            mode: chat_meta.mode,
            model: chat_meta.last_used_model,
            workspace_path: collector.workspace_path.clone(),
        };
        let search_text = collector.search_text();
        let output = Self::render_jsonl(session_id, &metadata, &collector.messages);
        let materialized_path = self.materialized_path(session_id);
        if let Some(parent) = materialized_path.parent() {
            fs::create_dir_all(parent).map_err(|source| XurlError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::write(&materialized_path, output).map_err(|source| XurlError::Io {
            path: materialized_path.clone(),
            source,
        })?;

        Ok(CursorMaterialization {
            path: materialized_path,
            search_text,
            metadata,
        })
    }

    fn render_jsonl(
        session_id: &str,
        metadata: &CursorMaterializedMetadata,
        messages: &[CursorMessage],
    ) -> String {
        let mut session_metadata = serde_json::Map::new();
        if let Some(name) = &metadata.name {
            session_metadata.insert("name".to_string(), Value::String(name.clone()));
        }
        if let Some(mode) = &metadata.mode {
            session_metadata.insert("mode".to_string(), Value::String(mode.clone()));
        }
        if let Some(model) = &metadata.model {
            session_metadata.insert("model".to_string(), Value::String(model.clone()));
        }
        if let Some(cwd) = &metadata.workspace_path {
            session_metadata.insert("cwd".to_string(), Value::String(cwd.clone()));
        }

        let mut lines = Vec::with_capacity(messages.len() + 1);
        lines.push(json!({
            "type": "session",
            "sessionId": session_id,
            "metadata": session_metadata,
        }));

        for message in messages {
            lines.push(json!({
                "type": "message",
                "id": message.id,
                "sessionId": session_id,
                "message": {
                    "role": message.role,
                },
                "parts": message.parts,
            }));
        }

        let mut output = String::new();
        for line in lines {
            let encoded = serde_json::to_string(&line).expect("json serialization should succeed");
            output.push_str(&encoded);
            output.push('\n');
        }
        output
    }

    fn cursor_bin() -> String {
        std::env::var("XURL_CURSOR_BIN").unwrap_or_else(|_| "cursor-agent".to_string())
    }

    fn spawn_cursor_command(args: &[String]) -> Result<std::process::Child> {
        let bin = Self::cursor_bin();
        let mut command = Command::new(&bin);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.spawn().map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                XurlError::CommandNotFound { command: bin }
            } else {
                XurlError::Io {
                    path: PathBuf::from(bin),
                    source,
                }
            }
        })
    }

    fn run_create_chat() -> Result<String> {
        let bin = Self::cursor_bin();
        let output = Command::new(&bin)
            .arg("create-chat")
            .output()
            .map_err(|source| {
                if source.kind() == std::io::ErrorKind::NotFound {
                    XurlError::CommandNotFound {
                        command: bin.clone(),
                    }
                } else {
                    XurlError::Io {
                        path: PathBuf::from(&bin),
                        source,
                    }
                }
            })?;

        if !output.status.success() {
            return Err(XurlError::CommandFailed {
                command: format!("{bin} create-chat"),
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }

        let session_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if session_id.is_empty() {
            return Err(XurlError::WriteProtocol(
                "cursor create-chat did not return a session id".to_string(),
            ));
        }

        Ok(session_id)
    }

    fn collect_text_part_texts(parts: &[Value]) -> String {
        parts
            .iter()
            .filter_map(|part| {
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    part.get("text").and_then(Value::as_str)
                } else {
                    None
                }
            })
            .filter(|text| !text.trim().is_empty())
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn extract_assistant_text(value: &Value) -> Option<String> {
        if value.get("type").and_then(Value::as_str) == Some("assistant")
            && let Some(message) = value.get("message")
        {
            let parts = message.get("content")?.as_array()?;
            let text = Self::collect_text_part_texts(parts);
            if !text.is_empty() {
                return Some(text);
            }
        }

        if value.get("type").and_then(Value::as_str) == Some("result") {
            return value
                .get("result")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(ToString::to_string);
        }

        None
    }

    fn run_write(
        &self,
        session_id: String,
        args: &[String],
        req: &WriteRequest,
        sink: &mut dyn WriteEventSink,
        warnings: Vec<String>,
    ) -> Result<WriteResult> {
        let mut child = Self::spawn_cursor_command(args)?;
        let stdout = child.stdout.take().ok_or_else(|| {
            XurlError::WriteProtocol("cursor-agent stdout pipe is unavailable".to_string())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            XurlError::WriteProtocol("cursor-agent stderr pipe is unavailable".to_string())
        })?;
        let stderr_handle = std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut content = String::new();
            let _ = reader.read_to_string(&mut content);
            content
        });

        let stream_path = Path::new("<cursor-agent:stdout>");
        let mut current_session_id = Some(session_id);
        let mut final_text = None::<String>;
        let mut last_emitted_assistant = None::<String>;
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let line = line.map_err(|source| XurlError::Io {
                path: stream_path.to_path_buf(),
                source,
            })?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                continue;
            };

            if value.get("type").and_then(Value::as_str) == Some("system")
                && value.get("subtype").and_then(Value::as_str) == Some("init")
                && let Some(found_session_id) = value.get("session_id").and_then(Value::as_str)
                && current_session_id.as_deref() != Some(found_session_id)
            {
                sink.on_session_ready(ProviderKind::Cursor, found_session_id)?;
                current_session_id = Some(found_session_id.to_string());
            }

            if let Some(text) = Self::extract_assistant_text(&value) {
                if last_emitted_assistant.as_deref() != Some(text.as_str()) {
                    sink.on_text_delta(&text)?;
                    last_emitted_assistant = Some(text.clone());
                }
                final_text = Some(text);
            }
        }

        let status = child.wait().map_err(|source| XurlError::Io {
            path: PathBuf::from(Self::cursor_bin()),
            source,
        })?;
        let stderr_content = stderr_handle.join().unwrap_or_default();
        if !status.success() {
            return Err(XurlError::CommandFailed {
                command: format!("{} {}", Self::cursor_bin(), args.join(" ")),
                code: status.code(),
                stderr: stderr_content.trim().to_string(),
            });
        }

        let session_id = current_session_id
            .or(req.session_id.clone())
            .ok_or_else(|| {
                XurlError::WriteProtocol("missing session id in cursor-agent output".to_string())
            })?;

        Ok(WriteResult {
            provider: ProviderKind::Cursor,
            session_id,
            final_text,
            warnings,
        })
    }
}

impl Provider for CursorProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Cursor
    }

    fn resolve(&self, session_id: &str) -> Result<ResolvedThread> {
        let candidates = self.find_store_candidates(session_id);
        if let Some((selected, count)) = Self::choose_latest(candidates) {
            let materialized = self.materialize_store(&selected, session_id)?;
            let mut metadata = ResolutionMeta {
                source: "cursor:store.db".to_string(),
                candidate_count: count,
                warnings: Vec::new(),
            };
            if count > 1 {
                metadata.warnings.push(format!(
                    "multiple cursor stores found ({count}) for session_id={session_id}; selected latest: {}",
                    selected.display()
                ));
            }

            return Ok(ResolvedThread {
                provider: ProviderKind::Cursor,
                session_id: session_id.to_string(),
                path: materialized.path,
                metadata,
            });
        }

        Err(XurlError::ThreadNotFound {
            provider: ProviderKind::Cursor.to_string(),
            session_id: session_id.to_string(),
            searched_roots: vec![self.chats_root()],
        })
    }

    fn write(&self, req: &WriteRequest, sink: &mut dyn WriteEventSink) -> Result<WriteResult> {
        if req.options.role.is_some() {
            return Err(XurlError::InvalidMode(
                "cursor does not support role-based write URI".to_string(),
            ));
        }

        let session_id = match &req.session_id {
            Some(session_id) => session_id.clone(),
            None => {
                let session_id = Self::run_create_chat()?;
                sink.on_session_ready(ProviderKind::Cursor, &session_id)?;
                session_id
            }
        };

        let mut args = vec![
            "--resume".to_string(),
            session_id.clone(),
            "--print".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--trust".to_string(),
        ];
        append_passthrough_args(&mut args, &req.options.params);
        args.push(req.prompt.clone());

        self.run_write(session_id, &args, req, sink, Vec::new())
    }
}

impl CursorCollector {
    fn walk_blob(&mut self, conn: &Connection, db_path: &Path, blob_id: &str) -> Result<()> {
        if !self.visited_blob_ids.insert(blob_id.to_string()) {
            return Ok(());
        }

        let Some(bytes) = CursorProvider::fetch_blob_bytes(conn, db_path, blob_id)? else {
            return Ok(());
        };
        self.inspect_bytes(conn, db_path, &bytes, Some(blob_id), 0)
    }

    fn inspect_bytes(
        &mut self,
        conn: &Connection,
        db_path: &Path,
        bytes: &[u8],
        source_id: Option<&str>,
        depth: usize,
    ) -> Result<()> {
        if let Some(value) = parse_json_bytes(bytes) {
            self.handle_json_value(source_id, &value);
        }

        if self.workspace_path.is_none() {
            self.workspace_path = extract_workspace_path_from_bytes(bytes);
        }

        if depth >= MAX_PROTO_SCAN_DEPTH {
            return Ok(());
        }

        let mut index = 0;
        while index < bytes.len() {
            let Some(field_key) = read_varint(bytes, &mut index) else {
                break;
            };

            match field_key & 0x07 {
                0 => {
                    if read_varint(bytes, &mut index).is_none() {
                        break;
                    }
                }
                1 => {
                    if index + 8 > bytes.len() {
                        break;
                    }
                    index += 8;
                }
                2 => {
                    let Some(length) = read_varint(bytes, &mut index) else {
                        break;
                    };
                    let Ok(length) = usize::try_from(length) else {
                        break;
                    };
                    if index + length > bytes.len() {
                        break;
                    }
                    let payload = &bytes[index..index + length];
                    if length == 32 {
                        let referenced_id = CursorProvider::bytes_to_hex(payload);
                        if self.known_blob_ids.contains(&referenced_id) {
                            self.walk_blob(conn, db_path, &referenced_id)?;
                            index += length;
                            continue;
                        }
                    }

                    self.inspect_bytes(conn, db_path, payload, None, depth + 1)?;
                    index += length;
                }
                5 => {
                    if index + 4 > bytes.len() {
                        break;
                    }
                    index += 4;
                }
                _ => break,
            }
        }

        Ok(())
    }

    fn handle_json_value(&mut self, source_id: Option<&str>, value: &Value) {
        if self.workspace_path.is_none() {
            self.workspace_path = extract_workspace_path_from_value(value);
        }

        let Some(message) = build_cursor_message(source_id, value) else {
            return;
        };
        let message_key = serde_json::to_string(value).unwrap_or_else(|_| message.id.clone());
        if self.seen_message_keys.insert(message_key) {
            self.messages.push(message);
        }
    }

    fn search_text(&self) -> String {
        self.messages
            .iter()
            .map(|message| {
                message
                    .parts
                    .iter()
                    .filter_map(|part| {
                        if part.get("type").and_then(Value::as_str) == Some("text") {
                            part.get("text").and_then(Value::as_str)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn parse_json_bytes(bytes: &[u8]) -> Option<Value> {
    serde_json::from_slice::<Value>(bytes).ok()
}

fn read_varint(bytes: &[u8], index: &mut usize) -> Option<u64> {
    let mut shift = 0_u32;
    let mut value = 0_u64;
    while *index < bytes.len() && shift <= 63 {
        let byte = bytes[*index];
        *index += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
    }

    None
}

fn extract_workspace_path_from_value(value: &Value) -> Option<String> {
    value
        .get("cwd")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            extract_workspace_path_from_bytes(serde_json::to_string(value).ok()?.as_bytes())
        })
}

fn extract_workspace_path_from_bytes(bytes: &[u8]) -> Option<String> {
    let haystack = String::from_utf8_lossy(bytes);
    let matched = FILE_URI_RE.find(&haystack)?.as_str();
    decode_file_uri_path(matched)
}

fn decode_file_uri_path(uri: &str) -> Option<String> {
    let path = uri.strip_prefix("file://")?;
    let mut output = Vec::with_capacity(path.len());
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hi = CursorProvider::decode_hex_nibble(bytes[index + 1])?;
                let lo = CursorProvider::decode_hex_nibble(bytes[index + 2])?;
                output.push((hi << 4) | lo);
                index += 3;
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }

    String::from_utf8(output).ok()
}

fn build_cursor_message(source_id: Option<&str>, value: &Value) -> Option<CursorMessage> {
    let role = value.get("role").and_then(Value::as_str)?;
    if role != "user" && role != "assistant" {
        return None;
    }
    if value
        .get("providerOptions")
        .and_then(|options| options.get("cursor"))
        .and_then(|cursor| cursor.get("isSummary"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }

    let mut parts = Vec::new();
    match value.get("content") {
        Some(Value::String(text)) => {
            if !text.trim().is_empty() {
                parts.push(json!({
                    "type": "text",
                    "text": text,
                }));
            }
        }
        Some(Value::Array(items)) => {
            for item in items {
                let Some(item_type) = item.get("type").and_then(Value::as_str) else {
                    continue;
                };
                match item_type {
                    "text" => {
                        if let Some(text) = item.get("text").and_then(Value::as_str)
                            && !text.trim().is_empty()
                        {
                            parts.push(json!({
                                "type": "text",
                                "text": text,
                            }));
                        }
                    }
                    "file" => {
                        let label = item
                            .get("filename")
                            .and_then(Value::as_str)
                            .map(|name| format!("[File: {name}]"))
                            .unwrap_or_else(|| "[File]".to_string());
                        parts.push(json!({
                            "type": "text",
                            "text": label,
                        }));
                    }
                    "image" => {
                        parts.push(json!({
                            "type": "text",
                            "text": "[Image]",
                        }));
                    }
                    "reasoning" | "redacted-reasoning" => {}
                    _ => {}
                }
            }
        }
        _ => {}
    }

    if parts.is_empty() {
        return None;
    }

    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| source_id.map(ToString::to_string))
        .unwrap_or_else(|| {
            serde_json::to_string(value)
                .unwrap_or_else(|_| role.to_string())
                .chars()
                .take(64)
                .collect()
        });

    Some(CursorMessage {
        id,
        role: role.to_string(),
        parts,
    })
}
