use std::cmp::Reverse;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use toml::Table as TomlTable;
use toml::Value as TomlValue;
use walkdir::WalkDir;

use crate::error::{Result, XurlError};
use crate::jsonl;
use crate::model::{ProviderKind, ResolutionMeta, ResolvedThread, WriteRequest, WriteResult};
use crate::provider::{Provider, WriteEventSink, append_passthrough_args};

#[derive(Debug, Clone)]
pub struct CodexProvider {
    root: PathBuf,
}

#[derive(Debug, Clone)]
struct SqliteThreadRecord {
    rollout_path: PathBuf,
    archived: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct CodexStateThreadRecord {
    pub(crate) session_id: String,
    pub(crate) rollout_path: PathBuf,
    pub(crate) archived: bool,
}

impl CodexProvider {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn sessions_root(&self) -> PathBuf {
        self.root.join("sessions")
    }

    fn archived_root(&self) -> PathBuf {
        self.root.join("archived_sessions")
    }

    fn state_db_paths(&self) -> Vec<PathBuf> {
        let mut paths = if let Ok(entries) = fs::read_dir(&self.root) {
            entries
                .filter_map(std::result::Result::ok)
                .filter_map(|entry| {
                    let path = entry.path();
                    let name = path.file_name()?.to_str()?;
                    let is_state_db = name == "state.sqlite"
                        || (name.starts_with("state_") && name.ends_with(".sqlite"));
                    if is_state_db && path.is_file() {
                        Some(path)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        paths.sort_by_key(|path| {
            let version = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| {
                    name.strip_prefix("state_")
                        .and_then(|name| name.strip_suffix(".sqlite"))
                })
                .and_then(|raw| raw.parse::<u32>().ok())
                .unwrap_or(0);
            let modified = fs::metadata(path)
                .and_then(|meta| meta.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (Reverse(version), Reverse(modified))
        });

        paths
    }

    fn query_thread_record(
        db_path: &Path,
        session_id: &str,
    ) -> std::result::Result<Option<SqliteThreadRecord>, rusqlite::Error> {
        let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut stmt =
            conn.prepare("SELECT rollout_path, archived FROM threads WHERE id = ?1 LIMIT 1")?;
        let row = stmt
            .query_row([session_id], |row| {
                Ok(SqliteThreadRecord {
                    rollout_path: PathBuf::from(row.get::<_, String>(0)?),
                    archived: row.get::<_, i64>(1)? != 0,
                })
            })
            .optional()?;
        Ok(row)
    }

    fn query_all_thread_records(
        db_path: &Path,
    ) -> std::result::Result<Vec<CodexStateThreadRecord>, rusqlite::Error> {
        let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut stmt = conn.prepare("SELECT id, rollout_path, archived FROM threads")?;
        let rows = stmt.query_map([], |row| {
            Ok(CodexStateThreadRecord {
                session_id: row.get::<_, String>(0)?,
                rollout_path: PathBuf::from(row.get::<_, String>(1)?),
                archived: row.get::<_, i64>(2)? != 0,
            })
        })?;
        rows.collect()
    }

    fn lookup_thread_from_state_db(
        state_dbs: &[PathBuf],
        session_id: &str,
        warnings: &mut Vec<String>,
    ) -> Option<SqliteThreadRecord> {
        for db_path in state_dbs {
            match Self::query_thread_record(db_path, session_id) {
                Ok(Some(record)) => return Some(record),
                Ok(None) => continue,
                Err(err) => warnings.push(format!(
                    "failed reading sqlite thread index {}: {err}",
                    db_path.display()
                )),
            }
        }

        None
    }

    pub(crate) fn collect_state_thread_records(
        &self,
        warnings: &mut Vec<String>,
    ) -> Vec<CodexStateThreadRecord> {
        let mut seen = HashSet::new();
        let mut records = Vec::new();
        for db_path in self.state_db_paths() {
            match Self::query_all_thread_records(&db_path) {
                Ok(items) => {
                    for record in items {
                        if !seen.insert(record.session_id.clone()) {
                            continue;
                        }
                        records.push(record);
                    }
                }
                Err(err) => warnings.push(format!(
                    "failed reading sqlite thread index {}: {err}",
                    db_path.display()
                )),
            }
        }
        records
    }

    pub(crate) fn state_manifest_watermark(&self, warnings: &mut Vec<String>) -> Option<String> {
        let paths = self.state_db_paths();
        if paths.is_empty() {
            return None;
        }

        let mut parts = Vec::with_capacity(paths.len());
        for path in paths {
            match fs::metadata(&path) {
                Ok(metadata) => {
                    let modified_ns = metadata
                        .modified()
                        .ok()
                        .and_then(|value| value.duration_since(SystemTime::UNIX_EPOCH).ok())
                        .map(|value| value.as_nanos())
                        .unwrap_or(0);
                    parts.push(format!(
                        "{}:{}:{}",
                        path.display(),
                        metadata.len(),
                        modified_ns
                    ));
                }
                Err(err) => warnings.push(format!(
                    "failed reading codex state db metadata {}: {err}",
                    path.display()
                )),
            }
        }

        if parts.is_empty() {
            None
        } else {
            Some(parts.join("|"))
        }
    }

    fn find_candidates(root: &Path, session_id: &str) -> Vec<PathBuf> {
        let needle = format!("{session_id}.jsonl");
        if !root.exists() {
            return Vec::new();
        }

        WalkDir::new(root)
            .into_iter()
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.into_path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(&needle))
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

    fn codex_bin() -> String {
        std::env::var("XURL_CODEX_BIN").unwrap_or_else(|_| "codex".to_string())
    }

    fn config_path(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    fn load_role_overrides(&self, role: &str) -> Result<Vec<(String, String)>> {
        let config_path = self.config_path();
        let raw = fs::read_to_string(&config_path).map_err(|source| XurlError::Io {
            path: config_path.clone(),
            source,
        })?;
        let config = toml::from_str::<TomlTable>(&raw).map_err(|err| {
            XurlError::InvalidMode(format!(
                "failed parsing codex config {}: {err}",
                config_path.display()
            ))
        })?;
        let role_config = config
            .get("agents")
            .and_then(TomlValue::as_table)
            .and_then(|agents| agents.get(role))
            .and_then(TomlValue::as_table)
            .ok_or_else(|| {
                XurlError::InvalidMode(format!(
                    "codex role `{role}` is not defined in {}",
                    config_path.display()
                ))
            })?;

        let mut merged = BTreeMap::<String, String>::new();
        if let Some(config_file) = role_config.get("config_file").and_then(TomlValue::as_str) {
            let config_file_path = if Path::new(config_file).is_absolute() {
                PathBuf::from(config_file)
            } else {
                self.root.join(config_file)
            };
            let raw = fs::read_to_string(&config_file_path).map_err(|source| XurlError::Io {
                path: config_file_path.clone(),
                source,
            })?;
            let config = toml::from_str::<TomlTable>(&raw).map_err(|err| {
                XurlError::InvalidMode(format!(
                    "failed parsing codex role config {}: {err}",
                    config_file_path.display()
                ))
            })?;
            for (key, value) in config {
                Self::flatten_codex_config(&key, &value, &mut merged);
            }
        }

        for (key, value) in role_config {
            if key == "description" || key == "config_file" {
                continue;
            }
            Self::flatten_codex_config(key, value, &mut merged);
        }

        if merged.is_empty() {
            return Err(XurlError::InvalidMode(format!(
                "codex role `{role}` does not define writable config overrides"
            )));
        }

        Ok(merged.into_iter().collect())
    }

    fn flatten_codex_config(
        prefix: &str,
        value: &TomlValue,
        output: &mut BTreeMap<String, String>,
    ) {
        if let TomlValue::Table(table) = value {
            for (key, child) in table {
                let next_prefix = format!("{prefix}.{key}");
                Self::flatten_codex_config(&next_prefix, child, output);
            }
            return;
        }

        output.insert(prefix.to_string(), Self::encode_codex_config_value(value));
    }

    fn encode_codex_config_value(value: &TomlValue) -> String {
        match value {
            TomlValue::String(text) => text.clone(),
            TomlValue::Integer(number) => number.to_string(),
            TomlValue::Float(number) => number.to_string(),
            TomlValue::Boolean(flag) => flag.to_string(),
            TomlValue::Datetime(datetime) => datetime.to_string(),
            TomlValue::Array(_) | TomlValue::Table(_) => {
                serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
            }
        }
    }

    fn spawn_codex_command(args: &[String]) -> Result<std::process::Child> {
        let bin = Self::codex_bin();
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

    fn run_write(
        &self,
        args: &[String],
        req: &WriteRequest,
        sink: &mut dyn WriteEventSink,
        warnings: Vec<String>,
    ) -> Result<WriteResult> {
        let mut child = Self::spawn_codex_command(args)?;
        let stdout = child.stdout.take().ok_or_else(|| {
            XurlError::WriteProtocol("codex stdout pipe is unavailable".to_string())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            XurlError::WriteProtocol("codex stderr pipe is unavailable".to_string())
        })?;
        let stderr_handle = std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut content = String::new();
            let _ = reader.read_to_string(&mut content);
            content
        });

        let mut session_id = req.session_id.clone();
        let mut final_text = None::<String>;
        let stream_path = Path::new("<codex:stdout>");
        let reader = BufReader::new(stdout);
        jsonl::parse_jsonl_reader(stream_path, reader, |_, value| {
            let Some(event_type) = value.get("type").and_then(Value::as_str) else {
                return Ok(());
            };

            if event_type == "thread.started" {
                if let Some(thread_id) = value.get("thread_id").and_then(Value::as_str) {
                    sink.on_session_ready(ProviderKind::Codex, thread_id)?;
                    session_id = Some(thread_id.to_string());
                }
                return Ok(());
            }

            if event_type != "item.completed" {
                return Ok(());
            }

            let Some(item) = value.get("item") else {
                return Ok(());
            };
            if item.get("type").and_then(Value::as_str) != Some("agent_message") {
                return Ok(());
            }

            if let Some(text) = item.get("text").and_then(Value::as_str) {
                sink.on_text_delta(text)?;
                final_text = Some(text.to_string());
            }
            Ok(())
        })?;

        let status = child.wait().map_err(|source| XurlError::Io {
            path: PathBuf::from(Self::codex_bin()),
            source,
        })?;
        let stderr_content = stderr_handle.join().unwrap_or_default();

        if !status.success() {
            return Err(XurlError::CommandFailed {
                command: format!("{} {}", Self::codex_bin(), args.join(" ")),
                code: status.code(),
                stderr: stderr_content.trim().to_string(),
            });
        }

        let session_id = if let Some(session_id) = session_id {
            session_id
        } else {
            return Err(XurlError::WriteProtocol(
                "missing thread id in codex event stream".to_string(),
            ));
        };

        Ok(WriteResult {
            provider: ProviderKind::Codex,
            session_id,
            final_text,
            warnings,
        })
    }
}

impl Provider for CodexProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Codex
    }

    fn resolve(&self, session_id: &str) -> Result<ResolvedThread> {
        let sessions = self.sessions_root();
        let archived = self.archived_root();
        let state_dbs = self.state_db_paths();
        let mut warnings = Vec::new();
        let sqlite_record =
            Self::lookup_thread_from_state_db(&state_dbs, session_id, &mut warnings);

        if let Some(record) = sqlite_record.as_ref().filter(|record| !record.archived) {
            if record.rollout_path.exists() {
                return Ok(ResolvedThread {
                    provider: ProviderKind::Codex,
                    session_id: session_id.to_string(),
                    path: record.rollout_path.clone(),
                    metadata: ResolutionMeta {
                        source: "codex:sqlite:sessions".to_string(),
                        candidate_count: 1,
                        warnings,
                    },
                });
            }

            warnings.push(format!(
                "sqlite thread index points to a missing rollout for session_id={session_id}: {}",
                record.rollout_path.display()
            ));
        }

        let active_candidates = Self::find_candidates(&sessions, session_id);
        if let Some((selected, count)) = Self::choose_latest(active_candidates) {
            if count > 1 {
                warnings.push(format!(
                    "multiple matches found ({count}) for session_id={session_id}; selected latest: {}",
                    selected.display()
                ));
            }

            let meta = ResolutionMeta {
                source: "codex:sessions".to_string(),
                candidate_count: count,
                warnings,
            };

            return Ok(ResolvedThread {
                provider: ProviderKind::Codex,
                session_id: session_id.to_string(),
                path: selected,
                metadata: meta,
            });
        }

        if let Some(record) = sqlite_record.as_ref().filter(|record| record.archived) {
            if record.rollout_path.exists() {
                return Ok(ResolvedThread {
                    provider: ProviderKind::Codex,
                    session_id: session_id.to_string(),
                    path: record.rollout_path.clone(),
                    metadata: ResolutionMeta {
                        source: "codex:sqlite:archived_sessions".to_string(),
                        candidate_count: 1,
                        warnings,
                    },
                });
            }

            warnings.push(format!(
                "sqlite thread index points to a missing archived rollout for session_id={session_id}: {}",
                record.rollout_path.display()
            ));
        }

        let archived_candidates = Self::find_candidates(&archived, session_id);
        if let Some((selected, count)) = Self::choose_latest(archived_candidates) {
            if count > 1 {
                warnings.push(format!(
                    "multiple archived matches found ({count}) for session_id={session_id}; selected latest: {}",
                    selected.display()
                ));
            }

            let meta = ResolutionMeta {
                source: "codex:archived_sessions".to_string(),
                candidate_count: count,
                warnings,
            };

            return Ok(ResolvedThread {
                provider: ProviderKind::Codex,
                session_id: session_id.to_string(),
                path: selected,
                metadata: meta,
            });
        }

        Err(XurlError::ThreadNotFound {
            provider: ProviderKind::Codex.to_string(),
            session_id: session_id.to_string(),
            searched_roots: vec![sessions, archived]
                .into_iter()
                .chain(state_dbs)
                .collect(),
        })
    }

    fn write(&self, req: &WriteRequest, sink: &mut dyn WriteEventSink) -> Result<WriteResult> {
        let warnings = Vec::new();
        let role_overrides = if let Some(role) = req.options.role.as_deref() {
            self.load_role_overrides(role)?
        } else {
            Vec::new()
        };
        let mut args = Vec::new();
        args.push("exec".to_string());

        if let Some(session_id) = req.session_id.as_deref() {
            args.push("resume".to_string());
            args.push("--json".to_string());
            append_passthrough_args(&mut args, &req.options.params);
            for (key, value) in &role_overrides {
                args.push("--config".to_string());
                args.push(format!("{key}={value}"));
            }
            args.push(session_id.to_string());
            args.push(req.prompt.clone());
            self.run_write(&args, req, sink, warnings)
        } else {
            args.push("--json".to_string());
            append_passthrough_args(&mut args, &req.options.params);
            for (key, value) in &role_overrides {
                args.push("--config".to_string());
                args.push(format!("{key}={value}"));
            }
            args.push(req.prompt.clone());
            self.run_write(&args, req, sink, warnings)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use rusqlite::Connection;
    use tempfile::tempdir;

    use crate::provider::Provider;
    use crate::provider::codex::CodexProvider;

    fn prepare_state_db(path: &Path) -> Connection {
        let conn = Connection::open(path).expect("open sqlite");
        conn.execute_batch(
            "
            CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                rollout_path TEXT NOT NULL,
                archived INTEGER NOT NULL DEFAULT 0
            );
            ",
        )
        .expect("create schema");
        conn
    }

    #[test]
    fn resolves_from_sessions() {
        let temp = tempdir().expect("tempdir");
        let path = temp
            .path()
            .join("sessions/2026/02/23/rollout-2026-02-23T04-48-50-019c871c-b1f9-7f60-9c4f-87ed09f13592.jsonl");
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, "{}\n").expect("write");

        let provider = CodexProvider::new(temp.path());
        let resolved = provider
            .resolve("019c871c-b1f9-7f60-9c4f-87ed09f13592")
            .expect("resolve should succeed");
        assert_eq!(resolved.path, path);
    }

    #[test]
    fn resolves_from_archived_when_not_in_sessions() {
        let temp = tempdir().expect("tempdir");
        let path = temp
            .path()
            .join("archived_sessions/rollout-2026-02-22T01-05-36-019c8129-f668-7951-8d56-cc5513541c26.jsonl");
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, "{}\n").expect("write");

        let provider = CodexProvider::new(temp.path());
        let resolved = provider
            .resolve("019c8129-f668-7951-8d56-cc5513541c26")
            .expect("resolve should succeed");
        assert_eq!(resolved.path, path);
        assert_eq!(resolved.metadata.source, "codex:archived_sessions");
    }

    #[test]
    fn returns_not_found_when_missing() {
        let temp = tempdir().expect("tempdir");
        let provider = CodexProvider::new(temp.path());
        let err = provider
            .resolve("019c8129-f668-7951-8d56-cc5513541c26")
            .expect_err("should fail");
        assert!(format!("{err}").contains("thread not found"));
    }

    #[test]
    fn resolves_from_sqlite_state_index() {
        let temp = tempdir().expect("tempdir");
        let state_db = temp.path().join("state_5.sqlite");
        let conn = prepare_state_db(&state_db);

        let session_id = "019c871c-b1f9-7f60-9c4f-87ed09f13592";
        let rollout = temp.path().join("sessions/custom/path/thread.jsonl");
        fs::create_dir_all(rollout.parent().expect("parent")).expect("mkdir");
        fs::write(&rollout, "{}\n").expect("write");

        conn.execute(
            "INSERT INTO threads (id, rollout_path, archived) VALUES (?1, ?2, 0)",
            (&session_id, rollout.display().to_string()),
        )
        .expect("insert thread");

        let provider = CodexProvider::new(temp.path());
        let resolved = provider
            .resolve(session_id)
            .expect("resolve should succeed");
        assert_eq!(resolved.path, rollout);
        assert_eq!(resolved.metadata.source, "codex:sqlite:sessions");
    }

    #[test]
    fn resolves_archived_from_sqlite_state_index() {
        let temp = tempdir().expect("tempdir");
        let state_db = temp.path().join("state.sqlite");
        let conn = prepare_state_db(&state_db);

        let session_id = "019c8129-f668-7951-8d56-cc5513541c26";
        let rollout = temp
            .path()
            .join("archived_sessions/custom/path/thread.jsonl");
        fs::create_dir_all(rollout.parent().expect("parent")).expect("mkdir");
        fs::write(&rollout, "{}\n").expect("write");

        conn.execute(
            "INSERT INTO threads (id, rollout_path, archived) VALUES (?1, ?2, 1)",
            (&session_id, rollout.display().to_string()),
        )
        .expect("insert thread");

        let provider = CodexProvider::new(temp.path());
        let resolved = provider
            .resolve(session_id)
            .expect("resolve should succeed");
        assert_eq!(resolved.path, rollout);
        assert_eq!(resolved.metadata.source, "codex:sqlite:archived_sessions");
    }

    #[test]
    fn falls_back_to_filesystem_when_sqlite_rollout_missing() {
        let temp = tempdir().expect("tempdir");
        let state_db = temp.path().join("state_5.sqlite");
        let conn = prepare_state_db(&state_db);

        let session_id = "019c871c-b1f9-7f60-9c4f-87ed09f13592";
        let stale_rollout = temp.path().join("sessions/stale/path/thread.jsonl");
        conn.execute(
            "INSERT INTO threads (id, rollout_path, archived) VALUES (?1, ?2, 0)",
            (&session_id, stale_rollout.display().to_string()),
        )
        .expect("insert thread");

        let fs_rollout = temp.path().join(
            "sessions/2026/02/23/rollout-2026-02-23T04-48-50-019c871c-b1f9-7f60-9c4f-87ed09f13592.jsonl",
        );
        fs::create_dir_all(fs_rollout.parent().expect("parent")).expect("mkdir");
        fs::write(&fs_rollout, "{}\n").expect("write");

        let provider = CodexProvider::new(temp.path());
        let resolved = provider
            .resolve(session_id)
            .expect("resolve should succeed");
        assert_eq!(resolved.path, fs_rollout);
        assert_eq!(resolved.metadata.source, "codex:sessions");
        assert_eq!(resolved.metadata.warnings.len(), 1);
        assert!(resolved.metadata.warnings[0].contains("missing rollout"));
    }

    #[test]
    fn loads_role_overrides_from_main_and_config_file() {
        let temp = tempdir().expect("tempdir");
        fs::write(
            temp.path().join("config.toml"),
            r#"
[agents.reviewer]
description = "review role"
config_file = "agents/reviewer.toml"
model_reasoning_effort = "high"
developer_instructions = "Focus on high priority issues."
"#,
        )
        .expect("write config");
        let role_config_dir = temp.path().join("agents");
        fs::create_dir_all(&role_config_dir).expect("mkdir role config");
        fs::write(
            role_config_dir.join("reviewer.toml"),
            r#"
model = "gpt-5.3-codex"
"#,
        )
        .expect("write role config");

        let provider = CodexProvider::new(temp.path());
        let overrides = provider
            .load_role_overrides("reviewer")
            .expect("must load role");

        assert_eq!(
            overrides,
            vec![
                (
                    "developer_instructions".to_string(),
                    "Focus on high priority issues.".to_string(),
                ),
                ("model".to_string(), "gpt-5.3-codex".to_string()),
                ("model_reasoning_effort".to_string(), "high".to_string()),
            ]
        );
    }

    #[test]
    fn missing_role_override_returns_error() {
        let temp = tempdir().expect("tempdir");
        fs::write(
            temp.path().join("config.toml"),
            r#"
[agents.default]
description = "default role"
"#,
        )
        .expect("write config");

        let provider = CodexProvider::new(temp.path());
        let err = provider
            .load_role_overrides("reviewer")
            .expect_err("must fail");
        assert!(format!("{err}").contains("is not defined"));
    }
}
