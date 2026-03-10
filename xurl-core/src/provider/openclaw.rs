use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{Result, XurlError};
use crate::model::{ProviderKind, ResolutionMeta, ResolvedThread, WriteRequest, WriteResult};
use crate::provider::{Provider, WriteEventSink, append_passthrough_args};

#[derive(Debug, Clone)]
pub struct OpenClawProvider {
    root: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenClawSessionEntry {
    role: String,
    content: Value,
}

#[derive(Debug, Clone)]
enum OpenClawSessionSource {
    Jsonl(PathBuf),
    LegacyJson(PathBuf),
}

impl OpenClawProvider {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn sessions_dir(&self) -> PathBuf {
        self.root.join("agents")
    }

    fn legacy_sessions_dir(&self) -> PathBuf {
        self.root.join("data/sessions")
    }

    #[cfg(test)]
    fn session_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir()
            .join("main")
            .join("sessions")
            .join(format!("{session_id}.jsonl"))
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

    fn collect_sessions_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        let agents_root = self.sessions_dir();
        dirs.push(agents_root.join("main").join("sessions"));

        if let Ok(entries) = fs::read_dir(&agents_root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path.join("sessions"));
                }
            }
        }

        dirs.sort();
        dirs.dedup();
        dirs
    }

    fn find_case_insensitive_file(dir: &Path, session_id: &str, ext: &str) -> Option<PathBuf> {
        let entries = fs::read_dir(dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(candidate_ext) = path.extension().and_then(|value| value.to_str()) else {
                continue;
            };
            if !candidate_ext.eq_ignore_ascii_case(ext) {
                continue;
            }
            let Some(candidate_id) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            if candidate_id.eq_ignore_ascii_case(session_id) {
                return Some(path);
            }
        }
        None
    }

    fn find_jsonl_session_path(
        &self,
        session_id: &str,
        searched: &mut Vec<PathBuf>,
    ) -> Option<PathBuf> {
        let dirs = self.collect_sessions_dirs();
        for dir in &dirs {
            let path = dir.join(format!("{session_id}.jsonl"));
            searched.push(path.clone());
            if path.exists() {
                return Some(path);
            }
        }

        for dir in &dirs {
            if let Some(path) = Self::find_case_insensitive_file(dir, session_id, "jsonl") {
                return Some(path);
            }
        }

        None
    }

    fn find_legacy_session_path(
        &self,
        session_id: &str,
        searched: &mut Vec<PathBuf>,
    ) -> Option<PathBuf> {
        let dir = self.legacy_sessions_dir();
        let path = dir.join(format!("{session_id}.json"));
        searched.push(path.clone());
        if path.exists() {
            return Some(path);
        }
        Self::find_case_insensitive_file(&dir, session_id, "json")
    }

    fn load_session_entries(&self, session_path: &Path) -> Result<Vec<OpenClawSessionEntry>> {
        let content = fs::read_to_string(session_path).map_err(|source| XurlError::Io {
            path: session_path.to_path_buf(),
            source,
        })?;

        let entries: Vec<OpenClawSessionEntry> =
            serde_json::from_str(&content).map_err(|source| XurlError::InvalidJsonLine {
                path: session_path.to_path_buf(),
                line: 1,
                source,
            })?;

        Ok(entries)
    }

    fn render_jsonl(session_id: &str, entries: Vec<OpenClawSessionEntry>) -> String {
        let mut lines = Vec::with_capacity(entries.len() + 1);
        lines.push(json!({
            "type": "session",
            "id": session_id,
        }));

        for entry in entries {
            lines.push(json!({
                "type": "message",
                "id": format!("msg_{}", entry.role),
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

    fn find_session_source(&self, session_id: &str, searched: &mut Vec<PathBuf>) -> Option<OpenClawSessionSource> {
        if let Some(path) = self.find_jsonl_session_path(session_id, searched) {
            return Some(OpenClawSessionSource::Jsonl(path));
        }
        if let Some(path) = self.find_legacy_session_path(session_id, searched) {
            return Some(OpenClawSessionSource::LegacyJson(path));
        }
        None
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
            .or_else(|| value.get("session_id").and_then(Value::as_str))
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
        let mut searched = Vec::new();
        match self.find_session_source(session_id, &mut searched) {
            Some(OpenClawSessionSource::Jsonl(path)) => Ok(ResolvedThread {
                provider: ProviderKind::Openclaw,
                session_id: session_id.to_string(),
                path,
                metadata: ResolutionMeta {
                    source: "openclaw:jsonl".to_string(),
                    candidate_count: 1,
                    warnings: Vec::new(),
                },
            }),
            Some(OpenClawSessionSource::LegacyJson(path)) => {
                let entries = self.load_session_entries(&path)?;
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
            None => Err(XurlError::ThreadNotFound {
                provider: ProviderKind::Openclaw.to_string(),
                session_id: session_id.to_string(),
                searched_roots: searched,
            }),
        }
    }

    fn write(&self, req: &WriteRequest, sink: &mut dyn WriteEventSink) -> Result<WriteResult> {
        let warnings = Vec::new();
        // Use: openclaw agent --message '...' [--agent ...]
        let mut args = vec![
            "agent".to_string(),
            "--message".to_string(),
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
    use std::path::{Path, PathBuf};

    use tempfile::tempdir;

    use crate::provider::Provider;
    use crate::provider::openclaw::OpenClawProvider;

    fn prepare_jsonl_session_file(path: &Path, agent_id: &str, session_id: &str) -> PathBuf {
        let sessions_dir = path.join(format!("agents/{agent_id}/sessions"));
        fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        let session_path = sessions_dir.join(format!("{session_id}.jsonl"));

        fs::write(
            &session_path,
            format!(
                "{{\"type\":\"session\",\"id\":\"{session_id}\"}}\n{{\"type\":\"message\",\"id\":\"m1\",\"parentId\":null,\"timestamp\":\"2026-03-09T09:34:20.014Z\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"hello\"}}]}}}}\n{{\"type\":\"message\",\"id\":\"m2\",\"parentId\":\"m1\",\"timestamp\":\"2026-03-09T09:34:21.014Z\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"world\"}}]}}}}\n"
            ),
        )
        .expect("write session file");
        session_path
    }

    #[test]
    fn resolves_from_jsonl_session_file() {
        let temp = tempdir().expect("tempdir");
        let session_id = "0139048b-6a00-4636-8125-336ba5ed1cf9";
        prepare_jsonl_session_file(temp.path(), "primary", session_id);

        let provider = OpenClawProvider::new(temp.path());
        let resolved = provider
            .resolve(session_id)
            .expect("resolve should succeed");

        assert_eq!(resolved.metadata.source, "openclaw:jsonl");
        assert!(resolved.path.exists());

        let raw = fs::read_to_string(&resolved.path).expect("read materialized");
        assert!(raw.contains(r#""type":"session""#));
        assert!(raw.contains(r#""type":"message""#));
        assert!(raw.contains(r#""role":"user""#));
        assert!(raw.contains(r#""role":"assistant""#));
    }

    #[test]
    fn resolves_from_legacy_json_session_file() {
        let temp = tempdir().expect("tempdir");
        let session_id = "0139048b-6a00-4636-8125-336ba5ed1cf9";
        let sessions_dir = temp.path().join("data/sessions");
        fs::create_dir_all(&sessions_dir).expect("create legacy sessions dir");
        let session_path = sessions_dir.join(format!("{session_id}.json"));
        let legacy_entries = r#"[
            {"role": "user", "content": "hello", "timestamp": "2024-01-01T00:00:01Z"},
            {"role": "assistant", "content": "world", "timestamp": "2024-01-01T00:00:02Z"}
        ]"#;
        fs::write(&session_path, legacy_entries).expect("write legacy session file");

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
    fn resolves_case_insensitive_session_id() {
        let temp = tempdir().expect("tempdir");
        let session_id = "0139048b-6a00-4636-8125-336ba5ed1cf9";
        let upper_session_id = session_id.to_ascii_uppercase();
        prepare_jsonl_session_file(temp.path(), "alpha", &upper_session_id);

        let provider = OpenClawProvider::new(temp.path());
        let resolved = provider
            .resolve(session_id)
            .expect("resolve should succeed");

        assert!(resolved.path.exists());
    }

    #[test]
    fn returns_not_found_when_session_file_missing() {
        let temp = tempdir().expect("tempdir");
        let provider = OpenClawProvider::new(temp.path());
        let err = provider
            .resolve("00000000-0000-4000-8000-000000000000")
            .expect_err("must fail");
        assert!(format!("{err}").contains("thread not found"));
    }

    #[test]
    fn session_path_uses_openclaw_jsonl_layout() {
        let temp = tempdir().expect("tempdir");
        let provider = OpenClawProvider::new(temp.path());
        let path = provider.session_path("0139048b-6a00-4636-8125-336ba5ed1cf9");
        assert!(path.ends_with("agents/main/sessions/0139048b-6a00-4636-8125-336ba5ed1cf9.jsonl"));
    }
}

#[cfg(test)]
mod integration_tests {
    use std::process::Command;

    /// Integration test: Verify openclaw CLI is available and responds to --version
    #[test]
    #[ignore]
    fn test_openclaw_cli_version() {
        let output = Command::new("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                "`$env:Path = 'C:\\Program Files\\nodejs' + ';' + `$env:Path; openclaw --version",
            ])
            .output()
            .expect("Failed to execute openclaw --version");

        assert!(output.status.success(), "openclaw CLI should be available");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let output_str = format!("{}{}", stdout, stderr);
        println!("output_str: {:?}", output_str);
        assert!(
            output_str.contains("2026"),
            "Should return OpenClaw version info"
        );
    }

    /// Integration test: Verify openclaw agent --help works
    #[test]
    #[ignore]
    fn test_openclaw_agent_help() {
        let output = Command::new("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe")
            .args(["-NoProfile", "-Command", "`$env:Path = 'C:\\Program Files\\nodejs' + ';' + `$env:Path; openclaw agent --help"])
            .output()
            .expect("Failed to execute openclaw agent --help");

        assert!(output.status.success(), "openclaw agent --help should work");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let output_str = format!("{}{}", stdout, stderr);
        assert!(
            output_str.contains("--message"),
            "Should have --message option"
        );
        assert!(
            output_str.contains("--session-id"),
            "Should have --session-id option"
        );
        assert!(output_str.contains("--json"), "Should have --json option");
    }

    /// Integration test: Verify openclaw sessions --help works
    #[test]
    #[ignore]
    fn test_openclaw_sessions_help() {
        let output = Command::new("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe")
            .args(["-NoProfile", "-Command", "`$env:Path = 'C:\\Program Files\\nodejs' + ';' + `$env:Path; openclaw sessions --help"])
            .output()
            .expect("Failed to execute openclaw sessions --help");

        assert!(
            output.status.success(),
            "openclaw sessions --help should work"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let output_str = format!("{}{}", stdout, stderr);
        assert!(
            output_str.contains("cleanup"),
            "Should have cleanup subcommand"
        );
        assert!(
            output_str.contains("--active"),
            "Should have --active option"
        );
    }

    /// Integration test: Verify openclaw status works
    #[test]
    #[ignore]
    fn test_openclaw_status() {
        let output = Command::new("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                "`$env:Path = 'C:\\Program Files\\nodejs' + ';' + `$env:Path; openclaw status",
            ])
            .output()
            .expect("Failed to execute openclaw status");

        assert!(output.status.success(), "openclaw status should work");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let output_str = format!("{}{}", stdout, stderr);
        assert!(
            output_str.contains("OpenClaw")
                || output_str.contains("gateway")
                || !output_str.is_empty(),
            "Should return status info"
        );
    }
}
