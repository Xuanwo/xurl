use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{Result, XurlError};
use crate::model::{ProviderKind, ResolutionMeta, ResolvedThread, WriteRequest, WriteResult};
use crate::provider::{Provider, WriteEventSink, append_passthrough_args};

#[derive(Debug, Clone, Deserialize)]
struct OpenClawSessionEntry {
    role: String,
    content: Value,
}

#[derive(Debug, Clone)]
pub struct OpenClawProvider {
    root: PathBuf,
}

impl OpenClawProvider {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn sessions_dir(&self) -> PathBuf {
        self.root.join("data/sessions")
    }

    fn session_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir().join(format!("{session_id}.json"))
    }

    fn materialized_path(&self, session_id: &str) -> PathBuf {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.root.hash(&mut hasher);
        let root_key = format!("{:016x}", hasher.finish());

        std::env::temp_dir()
            .join("xurl-openclaw")
            .join(root_key)
            .join(format!("{session_id}.jsonl"))
    }

    fn load_session_entries(&self, session_id: &str) -> Result<Vec<OpenClawSessionEntry>> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Err(XurlError::ThreadNotFound {
                provider: ProviderKind::Openclaw.to_string(),
                session_id: session_id.to_string(),
                searched_roots: vec![path],
            });
        }

        let content = fs::read_to_string(&path).map_err(|source| XurlError::Io {
            path: path.clone(),
            source,
        })?;

        // Session JSON is an array of entries
        let entries: Vec<OpenClawSessionEntry> =
            serde_json::from_str(&content).map_err(|source| XurlError::InvalidJsonLine {
                path: path.clone(),
                line: 1,
                source,
            })?;

        Ok(entries)
    }

    fn render_jsonl(session_id: &str, entries: Vec<OpenClawSessionEntry>) -> String {
        let mut lines = Vec::with_capacity(entries.len() + 1);
        lines.push(json!({
            "type": "session",
            "sessionId": session_id,
        }));

        for entry in entries {
            lines.push(json!({
                "type": "message",
                "id": format!("msg_{}", entry.role),
                "sessionId": session_id,
                "message": {
                    "role": entry.role,
                    "content": entry.content,
                },
                "parts": [],
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

    fn openclaw_bin() -> String {
        std::env::var("XURL_OPENCLAW_BIN").unwrap_or_else(|_| "openclaw".to_string())
    }

    fn spawn_openclaw_command(args: &[String]) -> Result<std::process::Child> {
        let bin = Self::openclaw_bin();
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

    fn collect_text(value: Option<&Value>) -> String {
        match value {
            Some(Value::String(text)) => text.to_string(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| Self::collect_text(Some(item)))
                .collect::<Vec<_>>()
                .join(""),
            Some(Value::Object(map)) => {
                if map.get("type").and_then(Value::as_str) == Some("text")
                    && let Some(text) = map.get("text").and_then(Value::as_str)
                {
                    return text.to_string();
                }

                if let Some(text) = map.get("text").and_then(Value::as_str) {
                    return text.to_string();
                }

                if let Some(content) = map.get("content") {
                    return Self::collect_text(Some(content));
                }

                String::new()
            }
            _ => String::new(),
        }
    }

    fn extract_session_id(value: &Value) -> Option<&str> {
        value
            .get("sessionID")
            .and_then(Value::as_str)
            .or_else(|| value.get("sessionId").and_then(Value::as_str))
    }

    fn extract_delta_text(value: &Value) -> Option<String> {
        value
            .get("delta")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(ToString::to_string)
            .or_else(|| {
                value
                    .get("textDelta")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                    .map(ToString::to_string)
            })
            .or_else(|| {
                value
                    .get("message")
                    .and_then(Value::as_object)
                    .and_then(|message| message.get("delta"))
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                    .map(ToString::to_string)
            })
    }

    fn extract_assistant_text(value: &Value) -> Option<String> {
        if value.get("role").and_then(Value::as_str) == Some("assistant") {
            let text = Self::collect_text(value.get("content"));
            if !text.is_empty() {
                return Some(text);
            }
        }

        if let Some(message) = value.get("message")
            && message.get("role").and_then(Value::as_str) == Some("assistant")
        {
            let text = Self::collect_text(message.get("content"));
            if !text.is_empty() {
                return Some(text);
            }
        }

        value
            .get("response")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(ToString::to_string)
    }

    fn run_write(
        &self,
        args: &[String],
        req: &WriteRequest,
        sink: &mut dyn WriteEventSink,
        warnings: Vec<String>,
    ) -> Result<WriteResult> {
        let mut child = Self::spawn_openclaw_command(args)?;
        let stdout = child.stdout.take().ok_or_else(|| {
            XurlError::WriteProtocol("openclaw stdout pipe is unavailable".to_string())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            XurlError::WriteProtocol("openclaw stderr pipe is unavailable".to_string())
        })?;
        let stderr_handle = std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut content = String::new();
            let _ = reader.read_to_string(&mut content);
            content
        });

        let stream_path = PathBuf::from("<openclaw:stdout>");
        let mut session_id = req.session_id.clone();
        let mut final_text = None::<String>;
        let mut streamed_text = String::new();
        let mut streamed_delta = false;
        let mut stream_error = None::<String>;
        let mut saw_json_event = false;
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let line = line.map_err(|source| XurlError::Io {
                path: stream_path.clone(),
                source,
            })?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                continue;
            };
            saw_json_event = true;

            if let Some(current_session_id) = Self::extract_session_id(&value)
                && session_id.as_deref() != Some(current_session_id)
            {
                sink.on_session_ready(ProviderKind::Openclaw, current_session_id)?;
                session_id = Some(current_session_id.to_string());
            }

            if value.get("type").and_then(Value::as_str) == Some("error") {
                stream_error = value
                    .get("error")
                    .and_then(Value::as_object)
                    .and_then(|error| {
                        error
                            .get("data")
                            .and_then(Value::as_object)
                            .and_then(|data| data.get("message"))
                            .and_then(Value::as_str)
                            .or_else(|| error.get("message").and_then(Value::as_str))
                    })
                    .or_else(|| value.get("message").and_then(Value::as_str))
                    .map(ToString::to_string);
                continue;
            }

            if let Some(delta) = Self::extract_delta_text(&value) {
                sink.on_text_delta(&delta)?;
                streamed_text.push_str(&delta);
                final_text = Some(streamed_text.clone());
                streamed_delta = true;
                continue;
            }

            if !streamed_delta && let Some(text) = Self::extract_assistant_text(&value) {
                sink.on_text_delta(&text)?;
                final_text = Some(text);
            }
        }

        let status = child.wait().map_err(|source| XurlError::Io {
            path: PathBuf::from(Self::openclaw_bin()),
            source,
        })?;
        let stderr_content = stderr_handle.join().unwrap_or_default();
        if !status.success() {
            return Err(XurlError::CommandFailed {
                command: format!("{} {}", Self::openclaw_bin(), args.join(" ")),
                code: status.code(),
                stderr: stderr_content.trim().to_string(),
            });
        }

        if !saw_json_event {
            return Err(XurlError::WriteProtocol(
                "openclaw output does not contain JSON events".to_string(),
            ));
        }

        if let Some(stream_error) = stream_error {
            return Err(XurlError::WriteProtocol(format!(
                "openclaw stream returned an error: {stream_error}"
            )));
        }

        let session_id = if let Some(session_id) = session_id {
            session_id
        } else {
            return Err(XurlError::WriteProtocol(
                "missing session id in openclaw event stream".to_string(),
            ));
        };

        Ok(WriteResult {
            provider: ProviderKind::Openclaw,
            session_id,
            final_text,
            warnings,
        })
    }
}

impl Provider for OpenClawProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Openclaw
    }

    fn resolve(&self, session_id: &str) -> Result<ResolvedThread> {
        let entries = self.load_session_entries(session_id)?;
        let raw = Self::render_jsonl(session_id, entries);
        let path = self.materialized_path(session_id);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| XurlError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        fs::write(&path, raw).map_err(|source| XurlError::Io {
            path: path.clone(),
            source,
        })?;

        Ok(ResolvedThread {
            provider: ProviderKind::Openclaw,
            session_id: session_id.to_string(),
            path,
            metadata: ResolutionMeta {
                source: "openclaw:json".to_string(),
                candidate_count: 1,
                warnings: Vec::new(),
            },
        })
    }

    fn write(&self, req: &WriteRequest, sink: &mut dyn WriteEventSink) -> Result<WriteResult> {
        let warnings = Vec::new();
        // Use: openclaw agent --message '...' [--agent ...]
        let mut args = vec![
            "agent".to_string(), "--message".to_string(),
            req.prompt.clone(),
        ];
        if let Some(session_id) = req.session_id.as_deref() {
            args.push("--session-id".to_string());
            args.push(session_id.to_string());
        }
        if let Some(label) = req.options.role.as_deref() {
            args.push("--agent".to_string());
            args.push(label.to_string());
        }
        args.push("--json".to_string());
        append_passthrough_args(&mut args, &req.options.params);
        self.run_write(&args, req, sink, warnings)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;

    use crate::provider::Provider;
    use crate::provider::openclaw::OpenClawProvider;

    fn prepare_session_file(path: &Path, session_id: &str) {
        let sessions_dir = path.join("data/sessions");
        fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        let session_path = sessions_dir.join(format!("{session_id}.json"));

        let entries = r#"[
            {"role": "user", "content": "hello", "timestamp": "2024-01-01T00:00:01Z"},
            {"role": "assistant", "content": "world", "timestamp": "2024-01-01T00:00:02Z"}
        ]"#;
        fs::write(&session_path, entries).expect("write session file");
    }

    #[test]
    fn resolves_from_json_session_file() {
        let temp = tempdir().expect("tempdir");
        let session_id = "test_session_123";
        prepare_session_file(temp.path(), session_id);

        let provider = OpenClawProvider::new(temp.path());
        let resolved = provider
            .resolve(session_id)
            .expect("resolve should succeed");

        assert_eq!(resolved.metadata.source, "openclaw:json");
        assert!(resolved.path.exists());

        let raw = fs::read_to_string(&resolved.path).expect("read materialized");
        assert!(raw.contains(r#""type":"session""#));
        assert!(raw.contains(r#""type":"message""#));
        assert!(raw.contains(r#""role":"user""#));
        assert!(raw.contains(r#""role":"assistant""#));
    }

    #[test]
    fn returns_not_found_when_session_file_missing() {
        let temp = tempdir().expect("tempdir");
        let provider = OpenClawProvider::new(temp.path());
        let err = provider
            .resolve("nonexistent_session")
            .expect_err("must fail");
        assert!(format!("{err}").contains("thread not found"));
    }

    #[test]
    fn materialized_paths_are_isolated_by_root() {
        let first_root = tempdir().expect("first tempdir");
        let second_root = tempdir().expect("second tempdir");
        let first = OpenClawProvider::new(first_root.path());
        let second = OpenClawProvider::new(second_root.path());
        let session_id = "test_session_123";

        let first_path = first.materialized_path(session_id);
        let second_path = second.materialized_path(session_id);

        assert_ne!(first_path, second_path);
    }
}
