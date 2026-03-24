use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;
use xurl_core::{
    ProviderKind, ProviderRoots, ThreadQuery, maintain_search_index_for_uri, query_threads,
};

const TARGET_QUERY_KEYWORD: &str = "bench-hit";
const INCREMENTAL_QUERY_KEYWORD: &str = "bench-incremental-hit";
const FILLER_TEXT: &str = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega repeated search filler text";

fn main() {
    match run() {
        Ok(()) => {}
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    }
}

fn run() -> xurl_core::Result<()> {
    let options = BenchOptions::from_args(env::args().skip(1))?;

    let mut cold_query_ms = Vec::with_capacity(options.samples);
    let mut no_index_query_ms = Vec::with_capacity(options.samples);
    let mut full_build_ms = Vec::with_capacity(options.samples);
    let mut fts_rebuild_ms = Vec::with_capacity(options.samples);
    let mut warm_query_ms = Vec::with_capacity(options.samples);
    let mut incremental_refresh_ms = Vec::with_capacity(options.samples);
    let mut incremental_query_ms = Vec::with_capacity(options.samples);

    for _ in 0..options.samples {
        let workspace = SearchIndexBenchWorkspace::create(options.threads)?;
        cold_query_ms.push(duration_ms(measure(|| {
            workspace.run_query(TARGET_QUERY_KEYWORD, false)
        })?));
    }

    for _ in 0..options.samples {
        let workspace = SearchIndexBenchWorkspace::create(options.threads)?;
        no_index_query_ms.push(duration_ms(measure(|| {
            workspace.run_query(TARGET_QUERY_KEYWORD, true)
        })?));
    }

    for _ in 0..options.samples {
        let workspace = SearchIndexBenchWorkspace::create(options.threads)?;
        full_build_ms.push(duration_ms(measure(|| workspace.maintain_index())?));
    }

    for _ in 0..options.samples {
        let workspace = SearchIndexBenchWorkspace::create(options.threads)?;
        workspace.maintain_index()?;
        workspace.clear_search_documents()?;
        fts_rebuild_ms.push(duration_ms(measure(|| workspace.maintain_index())?));
    }

    for _ in 0..options.samples {
        let workspace = SearchIndexBenchWorkspace::create(options.threads)?;
        workspace.maintain_index()?;
        warm_query_ms.push(duration_ms(measure(|| {
            workspace.run_query(TARGET_QUERY_KEYWORD, false)
        })?));
    }

    for _ in 0..options.samples {
        let workspace = SearchIndexBenchWorkspace::create(options.threads)?;
        workspace.maintain_index()?;
        workspace.rewrite_incremental_thread()?;
        incremental_refresh_ms.push(duration_ms(measure(|| workspace.maintain_index())?));
        incremental_query_ms.push(duration_ms(measure(|| {
            workspace.run_query(INCREMENTAL_QUERY_KEYWORD, false)
        })?));
    }

    let cold_query_avg_ms = average(&cold_query_ms);
    let no_index_query_avg_ms = average(&no_index_query_ms);
    let full_build_avg_ms = average(&full_build_ms);
    let fts_rebuild_avg_ms = average(&fts_rebuild_ms);
    let warm_query_avg_ms = average(&warm_query_ms);
    let incremental_refresh_avg_ms = average(&incremental_refresh_ms);
    let incremental_query_avg_ms = average(&incremental_query_ms);

    let output = json!({
        "benchmark": "search_index",
        "threads": options.threads,
        "samples": options.samples,
        "results": {
            "cold_query_ms": cold_query_ms,
            "no_index_query_ms": no_index_query_ms,
            "full_build_ms": full_build_ms,
            "fts_rebuild_ms": fts_rebuild_ms,
            "warm_query_ms": warm_query_ms,
            "incremental_refresh_ms": incremental_refresh_ms,
            "incremental_query_ms": incremental_query_ms,
        },
        "summary": {
            "cold_query_avg_ms": cold_query_avg_ms,
            "no_index_query_avg_ms": no_index_query_avg_ms,
            "full_build_avg_ms": full_build_avg_ms,
            "fts_rebuild_avg_ms": fts_rebuild_avg_ms,
            "warm_query_avg_ms": warm_query_avg_ms,
            "incremental_refresh_avg_ms": incremental_refresh_avg_ms,
            "incremental_query_avg_ms": incremental_query_avg_ms,
            "cold_overhead_ms": cold_query_avg_ms - no_index_query_avg_ms,
            "warm_speedup_vs_no_index": if warm_query_avg_ms > 0.0 {
                no_index_query_avg_ms / warm_query_avg_ms
            } else {
                0.0
            },
        },
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&output).expect("search index bench json must serialize")
    );
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct BenchOptions {
    threads: usize,
    samples: usize,
}

impl Default for BenchOptions {
    fn default() -> Self {
        Self {
            threads: 2_500,
            samples: 5,
        }
    }
}

impl BenchOptions {
    fn from_args(args: impl Iterator<Item = String>) -> xurl_core::Result<Self> {
        let mut options = Self::default();
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--bench" => {}
                "--threads" => {
                    let value = args.next().ok_or_else(|| {
                        xurl_core::XurlError::InvalidMode(
                            "search_index bench requires a value after --threads".to_string(),
                        )
                    })?;
                    options.threads = value.parse::<usize>().map_err(|_| {
                        xurl_core::XurlError::InvalidMode(
                            "search_index bench requires --threads to be a positive integer"
                                .to_string(),
                        )
                    })?;
                }
                "--samples" => {
                    let value = args.next().ok_or_else(|| {
                        xurl_core::XurlError::InvalidMode(
                            "search_index bench requires a value after --samples".to_string(),
                        )
                    })?;
                    options.samples = value.parse::<usize>().map_err(|_| {
                        xurl_core::XurlError::InvalidMode(
                            "search_index bench requires --samples to be a positive integer"
                                .to_string(),
                        )
                    })?;
                }
                other => {
                    return Err(xurl_core::XurlError::InvalidMode(format!(
                        "unknown search_index bench argument: {other}"
                    )));
                }
            }
        }

        if options.threads == 0 {
            return Err(xurl_core::XurlError::InvalidMode(
                "search_index bench requires --threads >= 1".to_string(),
            ));
        }
        if options.samples == 0 {
            return Err(xurl_core::XurlError::InvalidMode(
                "search_index bench requires --samples >= 1".to_string(),
            ));
        }

        Ok(options)
    }
}

struct SearchIndexBenchWorkspace {
    _tempdir: TempDir,
    roots: ProviderRoots,
    index_path: PathBuf,
    incremental_thread_path: PathBuf,
}

impl SearchIndexBenchWorkspace {
    fn create(threads: usize) -> xurl_core::Result<Self> {
        let tempdir = tempfile::tempdir().map_err(|source| xurl_core::XurlError::Io {
            path: env::temp_dir(),
            source,
        })?;
        let codex_root = tempdir.path().join("codex-home");
        let index_path = tempdir.path().join("search-index.sqlite3");
        let sessions_root = codex_root.join("sessions/2026/02/23");
        fs::create_dir_all(&sessions_root).map_err(|source| xurl_core::XurlError::Io {
            path: sessions_root.clone(),
            source,
        })?;
        let state_db_path = codex_root.join("state.sqlite");

        let incremental_thread_path = sessions_root.join(format!(
            "rollout-2026-02-23T04-48-50-{}.jsonl",
            bench_session_id(threads.saturating_sub(1))
        ));
        let scope_path = Path::new("/tmp/xurl-search-bench");
        let mut state_rows = Vec::with_capacity(threads);
        for index in 0..threads {
            let session_id = bench_session_id(index);
            let thread_path =
                sessions_root.join(format!("rollout-2026-02-23T04-48-50-{session_id}.jsonl"));
            let keyword = if index == 0 {
                Some(TARGET_QUERY_KEYWORD)
            } else {
                None
            };
            write_codex_thread(&thread_path, scope_path, index, keyword)?;
            state_rows.push((session_id, thread_path));
        }
        write_codex_state_db(&state_db_path, &state_rows)?;

        let unused_root = tempdir.path().join("unused");
        let roots = ProviderRoots {
            amp_root: unused_root.join("amp"),
            copilot_root: unused_root.join("copilot"),
            codex_root,
            claude_root: unused_root.join("claude"),
            cursor_root: unused_root.join("cursor"),
            gemini_root: unused_root.join("gemini"),
            kimi_root: unused_root.join("kimi"),
            pi_root: unused_root.join("pi"),
            opencode_root: unused_root.join("opencode"),
        };

        Ok(Self {
            _tempdir: tempdir,
            roots,
            index_path,
            incremental_thread_path,
        })
    }

    fn run_query(&self, keyword: &str, disable_index: bool) -> xurl_core::Result<()> {
        let _index_path_guard = ScopedEnvVar::set("XURL_SEARCH_INDEX_PATH", Some(&self.index_path));
        let _disable_index_guard = if disable_index {
            Some(ScopedEnvVar::set(
                "XURL_DISABLE_SEARCH_INDEX",
                Some(Path::new("1")),
            ))
        } else {
            Some(ScopedEnvVar::set(
                "XURL_DISABLE_SEARCH_INDEX",
                None::<&Path>,
            ))
        };
        let query = ThreadQuery {
            uri: format!("agents://codex?q={keyword}&limit=1"),
            provider: ProviderKind::Codex,
            role: None,
            q: Some(keyword.to_string()),
            limit: 1,
            ignored_params: Vec::new(),
        };
        let _ = query_threads(&query, &self.roots)?;
        Ok(())
    }

    fn maintain_index(&self) -> xurl_core::Result<()> {
        let _index_path_guard = ScopedEnvVar::set("XURL_SEARCH_INDEX_PATH", Some(&self.index_path));
        let _disable_index_guard = ScopedEnvVar::set("XURL_DISABLE_SEARCH_INDEX", None::<&Path>);
        maintain_search_index_for_uri("agents://codex", &self.roots)
    }

    fn clear_search_documents(&self) -> xurl_core::Result<()> {
        let conn =
            Connection::open(&self.index_path).map_err(|source| xurl_core::XurlError::Sqlite {
                path: self.index_path.clone(),
                source,
            })?;
        conn.execute("DELETE FROM thread_fts", [])
            .map_err(|source| xurl_core::XurlError::Sqlite {
                path: self.index_path.clone(),
                source,
            })?;
        Ok(())
    }

    fn rewrite_incremental_thread(&self) -> xurl_core::Result<()> {
        write_codex_thread(
            &self.incremental_thread_path,
            Path::new("/tmp/xurl-search-bench"),
            9_999_999,
            Some(INCREMENTAL_QUERY_KEYWORD),
        )
    }
}

struct ScopedEnvVar {
    key: &'static str,
    previous: Option<OsString>,
}

impl ScopedEnvVar {
    fn set(key: &'static str, value: Option<impl AsRef<Path>>) -> Self {
        let previous = env::var_os(key);
        match value {
            Some(value) => {
                // SAFETY: the benchmark runs as a single-threaded process and restores the value on drop.
                unsafe { env::set_var(key, value.as_ref()) };
            }
            None => {
                // SAFETY: the benchmark runs as a single-threaded process and restores the value on drop.
                unsafe { env::remove_var(key) };
            }
        }
        Self { key, previous }
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        match &self.previous {
            Some(previous) => {
                // SAFETY: the benchmark runs as a single-threaded process and restores the prior value on drop.
                unsafe { env::set_var(self.key, previous) };
            }
            None => {
                // SAFETY: the benchmark runs as a single-threaded process and restores the prior value on drop.
                unsafe { env::remove_var(self.key) };
            }
        }
    }
}

fn write_codex_thread(
    path: &Path,
    scope_path: &Path,
    index: usize,
    keyword: Option<&str>,
) -> xurl_core::Result<()> {
    let scope = json_escape(scope_path.to_string_lossy().as_ref());
    let keyword_suffix = keyword.map_or(String::new(), |value| format!(" {value}"));
    let mut content = format!(
        "{{\"type\":\"session_meta\",\"payload\":{{\"cwd\":\"{scope}\",\"git\":{{\"branch\":\"bench-main\"}}}}}}\n"
    );
    for round in 0..8 {
        content.push_str(&format!(
            concat!(
                "{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",",
                "\"content\":[{{\"type\":\"input_text\",\"text\":\"search corpus filler user {index}-{round} {filler}\"}}]}}}}\n",
                "{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",",
                "\"content\":[{{\"type\":\"output_text\",\"text\":\"search corpus filler assistant {index}-{round} {filler}\"}}]}}}}\n"
            ),
            index = index,
            round = round,
            filler = FILLER_TEXT
        ));
    }
    content.push_str(&format!(
        concat!(
            "{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"search corpus item {index}{keyword_suffix}\"}}]}}}}\n",
            "{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"search corpus reply {index}\"}}]}}}}\n"
        ),
        index = index,
        keyword_suffix = keyword_suffix
    ));
    fs::write(path, content).map_err(|source| xurl_core::XurlError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write_codex_state_db(path: &Path, rows: &[(String, PathBuf)]) -> xurl_core::Result<()> {
    let conn = Connection::open(path).map_err(|source| xurl_core::XurlError::Sqlite {
        path: path.to_path_buf(),
        source,
    })?;
    conn.execute_batch(
        "
        CREATE TABLE threads (
            id TEXT PRIMARY KEY,
            rollout_path TEXT NOT NULL,
            archived INTEGER NOT NULL DEFAULT 0
        );
        ",
    )
    .map_err(|source| xurl_core::XurlError::Sqlite {
        path: path.to_path_buf(),
        source,
    })?;
    let mut stmt = conn
        .prepare("INSERT INTO threads (id, rollout_path, archived) VALUES (?1, ?2, 0)")
        .map_err(|source| xurl_core::XurlError::Sqlite {
            path: path.to_path_buf(),
            source,
        })?;
    for (session_id, rollout_path) in rows {
        stmt.execute((session_id, rollout_path.display().to_string()))
            .map_err(|source| xurl_core::XurlError::Sqlite {
                path: path.to_path_buf(),
                source,
            })?;
    }
    Ok(())
}

fn bench_session_id(index: usize) -> String {
    let value = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
    format!(
        "{:08x}-{:04x}-4000-8000-{:012x}",
        (value >> 16) & 0xffff_ffff,
        value & 0xffff,
        value
    )
}

fn json_escape(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            _ => output.push(ch),
        }
    }
    output
}

fn measure(operation: impl FnOnce() -> xurl_core::Result<()>) -> xurl_core::Result<Duration> {
    let started = Instant::now();
    operation()?;
    Ok(started.elapsed())
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn average(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}
