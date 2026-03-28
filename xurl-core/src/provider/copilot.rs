use std::cmp::Reverse;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use serde_json::Value;

use crate::error::{Result, XurlError};
use crate::model::{ProviderKind, ResolutionMeta, ResolvedThread, WriteRequest, WriteResult};
use crate::provider::{
    Provider, WriteEventSink, append_passthrough_args, append_passthrough_args_excluding,
};

#[derive(Debug, Clone)]
pub struct CopilotProvider {
    root: PathBuf,
}

impl CopilotProvider {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn session_state_root(&self) -> PathBuf {
        self.root.join("session-state")
    }

    fn direct_event_path(root: &Path, session_id: &str) -> PathBuf {
        root.join(session_id).join("events.jsonl")
    }

    fn legacy_event_path(root: &Path, session_id: &str) -> PathBuf {
        root.join(format!("{session_id}.jsonl"))
    }

    fn candidate_paths(root: &Path, session_id: &str) -> Vec<PathBuf> {
        [
            Self::direct_event_path(root, session_id),
            Self::legacy_event_path(root, session_id),
        ]
        .into_iter()
        .filter(|path| path.exists())
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

    fn copilot_bin() -> String {
        std::env::var("XURL_COPILOT_BIN").unwrap_or_else(|_| "copilot".to_string())
    }

    fn spawn_copilot_command(args: &[String]) -> Result<std::process::Child> {
        let bin = Self::copilot_bin();
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

    fn session_id_from_event(value: &Value) -> Option<&str> {
        value.get("sessionId").and_then(Value::as_str).or_else(|| {
            value
                .get("data")
                .and_then(|data| data.get("sessionId"))
                .and_then(Value::as_str)
        })
    }

    fn assistant_delta(value: &Value) -> Option<&str> {
        if value.get("type").and_then(Value::as_str) != Some("assistant.message_delta") {
            return None;
        }

        value
            .get("data")
            .and_then(|data| data.get("deltaContent"))
            .and_then(Value::as_str)
    }

    fn assistant_message_text(value: &Value) -> Option<&str> {
        if value.get("type").and_then(Value::as_str) != Some("assistant.message") {
            return None;
        }

        value
            .get("data")
            .and_then(|data| data.get("content"))
            .and_then(Value::as_str)
    }

    fn run_write(
        &self,
        args: &[String],
        req: &WriteRequest,
        sink: &mut dyn WriteEventSink,
        warnings: Vec<String>,
    ) -> Result<WriteResult> {
        let mut child = Self::spawn_copilot_command(args)?;
        let stdout = child.stdout.take().ok_or_else(|| {
            XurlError::WriteProtocol("copilot stdout pipe is unavailable".to_string())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            XurlError::WriteProtocol("copilot stderr pipe is unavailable".to_string())
        })?;
        let stderr_handle = std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut content = String::new();
            let _ = reader.read_to_string(&mut content);
            content
        });

        let stream_path = Path::new("<copilot:stdout>");
        let mut session_id = req.session_id.clone();
        let mut final_text = None::<String>;
        let mut streamed_text = String::new();
        let mut saw_json_event = false;
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
            saw_json_event = true;

            if let Some(current_session_id) = Self::session_id_from_event(&value)
                && session_id.as_deref() != Some(current_session_id)
            {
                sink.on_session_ready(ProviderKind::Copilot, current_session_id)?;
                session_id = Some(current_session_id.to_string());
            }

            if let Some(delta) = Self::assistant_delta(&value)
                && !delta.is_empty()
            {
                sink.on_text_delta(delta)?;
                streamed_text.push_str(delta);
                final_text = Some(streamed_text.clone());
            }

            if let Some(text) = Self::assistant_message_text(&value)
                && !text.is_empty()
            {
                final_text = Some(text.to_string());
            }
        }

        let status = child.wait().map_err(|source| XurlError::Io {
            path: PathBuf::from(Self::copilot_bin()),
            source,
        })?;
        let stderr_content = stderr_handle.join().unwrap_or_default();
        if !status.success() {
            return Err(XurlError::CommandFailed {
                command: format!("{} {}", Self::copilot_bin(), args.join(" ")),
                code: status.code(),
                stderr: stderr_content.trim().to_string(),
            });
        }

        if !saw_json_event {
            return Err(XurlError::WriteProtocol(
                "copilot output does not contain JSON events".to_string(),
            ));
        }

        let session_id = if let Some(session_id) = session_id {
            session_id
        } else {
            return Err(XurlError::WriteProtocol(
                "missing session id in copilot event stream".to_string(),
            ));
        };

        Ok(WriteResult {
            provider: ProviderKind::Copilot,
            session_id,
            final_text,
            warnings,
        })
    }
}

impl Provider for CopilotProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Copilot
    }

    fn resolve(&self, session_id: &str) -> Result<ResolvedThread> {
        let sessions_root = self.session_state_root();
        let candidates = Self::candidate_paths(&sessions_root, session_id);

        if let Some((selected, count)) = Self::choose_latest(candidates) {
            let mut metadata = ResolutionMeta {
                source: "copilot:session-state".to_string(),
                candidate_count: count,
                warnings: Vec::new(),
            };
            if count > 1 {
                metadata.warnings.push(format!(
                    "multiple matches found ({count}) for session_id={session_id}; selected latest: {}",
                    selected.display()
                ));
            }

            return Ok(ResolvedThread {
                provider: ProviderKind::Copilot,
                session_id: session_id.to_string(),
                path: selected,
                metadata,
            });
        }

        Err(XurlError::ThreadNotFound {
            provider: ProviderKind::Copilot.to_string(),
            session_id: session_id.to_string(),
            searched_roots: vec![
                Self::direct_event_path(&sessions_root, session_id),
                Self::legacy_event_path(&sessions_root, session_id),
            ],
        })
    }

    fn write(&self, req: &WriteRequest, sink: &mut dyn WriteEventSink) -> Result<WriteResult> {
        let mut warnings = Vec::new();
        let mut args = vec![
            "-p".to_string(),
            req.prompt.clone(),
            "--output-format".to_string(),
            "json".to_string(),
            "--allow-all-tools".to_string(),
        ];

        if let Some(role) = req.options.role.as_deref() {
            args.push("--agent".to_string());
            args.push(role.to_string());
            let ignored =
                append_passthrough_args_excluding(&mut args, &req.options.params, &["agent"]);
            if !ignored.is_empty() {
                warnings.push(
                    "ignored query parameter `agent` because URI role is already set".to_string(),
                );
            }
        } else {
            append_passthrough_args(&mut args, &req.options.params);
        }

        if let Some(session_id) = req.session_id.as_deref() {
            args.push("--resume".to_string());
            args.push(session_id.to_string());
        }

        self.run_write(&args, req, sink, warnings)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::provider::Provider;
    use crate::provider::copilot::CopilotProvider;

    #[test]
    fn resolves_from_session_state_events_jsonl() {
        let temp = tempdir().expect("tempdir");
        let thread_file = temp
            .path()
            .join("session-state/688628a1-407a-4b4e-b24a-1a250ebf864f/events.jsonl");
        fs::create_dir_all(thread_file.parent().expect("parent")).expect("mkdir");
        fs::write(&thread_file, "{}\n").expect("write");

        let provider = CopilotProvider::new(temp.path());
        let resolved = provider
            .resolve("688628a1-407a-4b4e-b24a-1a250ebf864f")
            .expect("resolve should succeed");

        assert_eq!(resolved.path, thread_file);
        assert_eq!(resolved.metadata.source, "copilot:session-state");
    }

    #[test]
    fn resolves_from_legacy_jsonl_file() {
        let temp = tempdir().expect("tempdir");
        let thread_file = temp
            .path()
            .join("session-state/688628a1-407a-4b4e-b24a-1a250ebf864f.jsonl");
        fs::create_dir_all(thread_file.parent().expect("parent")).expect("mkdir");
        fs::write(&thread_file, "{}\n").expect("write");

        let provider = CopilotProvider::new(temp.path());
        let resolved = provider
            .resolve("688628a1-407a-4b4e-b24a-1a250ebf864f")
            .expect("resolve should succeed");

        assert_eq!(resolved.path, thread_file);
    }
}
