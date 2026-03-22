use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::{fs, io};

use std::io::{Read, Write};

use clap::Parser;
use xurl_core::uri::{
    is_uuid_session_id, parse_collection_query_uri, parse_path_query_uri, parse_role_query_uri,
    parse_role_uri,
};
use xurl_core::{
    AgentsUri, ProviderKind, ProviderRoots, WriteEventSink, WriteOptions, WriteRequest,
    WriteResult, XurlError, query_threads, query_threads_by_path,
    render_path_thread_query_head_markdown, render_path_thread_query_markdown,
    render_subagent_view_markdown, render_thread_head_markdown, render_thread_markdown,
    render_thread_query_head_markdown, render_thread_query_markdown, resolve_subagent_view,
    resolve_thread, write_thread,
};

#[derive(Debug, Parser)]
#[command(name = "xurl", version, about = "Resolve and read code-agent threads")]
struct Cli {
    /// Thread URI like agents://codex/<session_id>, codex/<session_id>, agents://claude/<session_id>, agents://pi/<session_id>/<child_or_entry_id>, or legacy forms like codex://<session_id>
    uri: String,

    /// Output frontmatter only (header mode)
    #[arg(short = 'I', long)]
    head: bool,

    /// Send write-mode payload data; may be repeated. Prefix with @file or @- for stdin.
    #[arg(short = 'd', long = "data", value_name = "DATA")]
    data: Vec<String>,

    /// Write output to a file instead of stdout
    #[arg(short = 'o', long = "output", value_name = "PATH")]
    output: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {}", user_facing_error(&err));
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> xurl_core::Result<()> {
    let Cli {
        uri,
        head,
        data,
        output,
    } = cli;
    let roots = ProviderRoots::from_env_or_home()?;
    let output = output.as_deref();

    if data.is_empty() {
        if let Some(query) = parse_path_query_uri(&uri)? {
            let result = query_threads_by_path(&query, &roots)?;
            let output_body = if head {
                render_path_thread_query_head_markdown(&result)
            } else {
                render_path_thread_query_markdown(&result)
            };
            return write_output(output, &output_body);
        }

        if let Some(query) = parse_collection_query_uri(&uri)? {
            let result = query_threads(&query, &roots)?;
            let output_body = if head {
                render_thread_query_head_markdown(&result)
            } else {
                render_thread_query_markdown(&result)
            };
            return write_output(output, &output_body);
        }

        if let Some(query) = parse_role_query_uri(&uri)? {
            let result = query_threads(&query, &roots)?;
            let output_body = if head {
                render_thread_query_head_markdown(&result)
            } else {
                render_thread_query_markdown(&result)
            };
            return write_output(output, &output_body);
        }

        let uri = AgentsUri::parse(&uri)?;
        if uri.is_collection() {
            return Err(XurlError::InvalidMode(
                "read mode requires a thread URI: agents://<provider>/<session_id>".to_string(),
            ));
        }
        if head {
            let head = render_thread_head_markdown(&uri, &roots)?;
            return write_output(output, &head);
        }

        let is_subagent_drilldown = match uri.provider {
            xurl_core::ProviderKind::Codex
            | xurl_core::ProviderKind::Claude
            | xurl_core::ProviderKind::Gemini
            | xurl_core::ProviderKind::Amp
            | xurl_core::ProviderKind::Opencode => uri.agent_id.is_some(),
            xurl_core::ProviderKind::Pi => uri.agent_id.as_deref().is_some_and(is_uuid_session_id),
        };
        let markdown = if is_subagent_drilldown {
            let head = render_thread_head_markdown(&uri, &roots)?;
            let view = resolve_subagent_view(&uri, &roots, false)?;
            let body = render_subagent_view_markdown(&view);
            format!("{head}\n{body}")
        } else {
            let head = render_thread_head_markdown(&uri, &roots)?;
            let resolved = resolve_thread(&uri, &roots)?;
            let body = render_thread_markdown(&uri, &resolved)?;
            format!("{head}\n{body}")
        };

        return write_output(output, &markdown);
    }

    if head {
        return Err(XurlError::InvalidMode(
            "head mode (-I/--head) cannot be combined with write mode (-d/--data)".to_string(),
        ));
    }

    let prompt = build_prompt(&data)?;
    let target = parse_write_target(&uri)?;
    for warning in &target.warnings {
        eprintln!("warning: {warning}");
    }
    let mut sink = CliWriteSink::new(output, target.action)?;
    let result = write_thread(
        target.provider,
        &roots,
        &WriteRequest {
            prompt,
            session_id: target.session_id,
            options: target.options,
        },
        &mut sink,
    )?;
    sink.finish(&result)?;
    Ok(())
}

fn write_output(path: Option<&Path>, content: &str) -> xurl_core::Result<()> {
    if let Some(path) = path {
        std::fs::write(path, content).map_err(|source| XurlError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    } else {
        print!("{content}");
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum WriteAction {
    Create,
    Append,
}

#[derive(Debug, Clone)]
struct WriteTarget {
    provider: ProviderKind,
    session_id: Option<String>,
    action: WriteAction,
    options: WriteOptions,
    warnings: Vec<String>,
}

fn parse_write_target(input: &str) -> xurl_core::Result<WriteTarget> {
    if parse_path_query_uri(input)?.is_some() {
        return Err(XurlError::InvalidMode(
            "write mode does not support path-scoped query URIs".to_string(),
        ));
    }

    if let Some(role_uri) = parse_role_uri(input)? {
        let (options, warnings) = build_write_options(role_uri.query, Some(role_uri.role));
        return Ok(WriteTarget {
            provider: role_uri.provider,
            session_id: None,
            action: WriteAction::Create,
            options,
            warnings,
        });
    }

    let uri = AgentsUri::parse(input)?;
    if uri.agent_id.is_some() {
        return Err(XurlError::InvalidMode(
            "write mode only supports main thread URIs: agents://<provider>/<session_id>"
                .to_string(),
        ));
    }
    let action = if uri.is_collection() {
        WriteAction::Create
    } else {
        WriteAction::Append
    };
    let (options, warnings) = build_write_options(uri.query, None);

    let session_id = if uri.session_id.is_empty() {
        None
    } else {
        Some(uri.session_id)
    };

    Ok(WriteTarget {
        provider: uri.provider,
        session_id,
        action,
        options,
        warnings,
    })
}

fn build_write_options(
    params: Vec<(String, Option<String>)>,
    role: Option<String>,
) -> (WriteOptions, Vec<String>) {
    (WriteOptions { params, role }, Vec::new())
}

fn build_prompt(data: &[String]) -> xurl_core::Result<String> {
    let mut chunks = Vec::with_capacity(data.len());
    for raw in data {
        chunks.push(load_data(raw)?);
    }
    Ok(chunks.join("\n"))
}

fn load_data(raw: &str) -> xurl_core::Result<String> {
    if raw == "@-" {
        let mut input = String::new();
        io::stdin()
            .read_to_string(&mut input)
            .map_err(|source| XurlError::Io {
                path: PathBuf::from("<stdin>"),
                source,
            })?;
        return Ok(input);
    }

    if let Some(path) = raw.strip_prefix('@') {
        let path = PathBuf::from(path);
        return fs::read_to_string(&path).map_err(|source| XurlError::Io { path, source });
    }

    Ok(raw.to_string())
}

enum WriteDestination {
    Stdout,
    File { path: PathBuf, file: fs::File },
}

struct CliWriteSink {
    destination: WriteDestination,
    action: WriteAction,
    uri_emitted: bool,
    text_emitted: bool,
}

impl CliWriteSink {
    fn new(output: Option<&Path>, action: WriteAction) -> xurl_core::Result<Self> {
        let destination = if let Some(path) = output {
            let file = fs::File::create(path).map_err(|source| XurlError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            WriteDestination::File {
                path: path.to_path_buf(),
                file,
            }
        } else {
            WriteDestination::Stdout
        };

        Ok(Self {
            destination,
            action,
            uri_emitted: false,
            text_emitted: false,
        })
    }

    fn emit_uri_once(&mut self, provider: ProviderKind, session_id: &str) {
        if self.uri_emitted {
            return;
        }
        let verb = match self.action {
            WriteAction::Create => "created",
            WriteAction::Append => "updated",
        };
        eprintln!("{verb}: agents://{provider}/{session_id}");
        self.uri_emitted = true;
    }

    fn write_delta(&mut self, text: &str) -> xurl_core::Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        match &mut self.destination {
            WriteDestination::Stdout => {
                let mut stdout = io::stdout();
                stdout
                    .write_all(text.as_bytes())
                    .map_err(|source| XurlError::Io {
                        path: PathBuf::from("<stdout>"),
                        source,
                    })?;
                stdout.flush().map_err(|source| XurlError::Io {
                    path: PathBuf::from("<stdout>"),
                    source,
                })?;
            }
            WriteDestination::File { path, file } => {
                file.write_all(text.as_bytes())
                    .map_err(|source| XurlError::Io {
                        path: path.clone(),
                        source,
                    })?;
                file.flush().map_err(|source| XurlError::Io {
                    path: path.clone(),
                    source,
                })?;
            }
        }
        self.text_emitted = true;
        Ok(())
    }

    fn finish(&mut self, result: &WriteResult) -> xurl_core::Result<()> {
        for warning in &result.warnings {
            eprintln!("warning: {warning}");
        }
        self.emit_uri_once(result.provider, &result.session_id);
        if !self.text_emitted
            && let Some(text) = result.final_text.as_deref()
        {
            self.write_delta(text)?;
        }
        Ok(())
    }
}

impl WriteEventSink for CliWriteSink {
    fn on_session_ready(
        &mut self,
        provider: ProviderKind,
        session_id: &str,
    ) -> xurl_core::Result<()> {
        self.emit_uri_once(provider, session_id);
        Ok(())
    }

    fn on_text_delta(&mut self, text: &str) -> xurl_core::Result<()> {
        self.write_delta(text)
    }
}

fn user_facing_error(err: &XurlError) -> String {
    match err {
        XurlError::CommandNotFound { command } if command.contains("amp") => format!(
            "{err}\nhint: write mode needs Amp CLI; run `amp --version`, install Amp CLI if missing, then run `amp login`."
        ),
        XurlError::CommandNotFound { command } if command.contains("codex") => format!(
            "{err}\nhint: write mode needs Codex CLI; run `codex --version`, install Codex CLI if missing, then run `codex login`."
        ),
        XurlError::CommandNotFound { command } if command.contains("claude") => format!(
            "{err}\nhint: write mode needs Claude CLI; run `claude --version`, install Claude Code if missing, then authenticate."
        ),
        XurlError::CommandNotFound { command } if command.contains("gemini") => format!(
            "{err}\nhint: write mode needs Gemini CLI; run `gemini --version`, install Gemini CLI if missing, then authenticate."
        ),
        XurlError::CommandNotFound { command } if command.contains("pi") => format!(
            "{err}\nhint: write mode needs pi CLI; run `pi --version`, install pi if missing, then configure provider credentials."
        ),
        XurlError::CommandNotFound { command } if command.contains("opencode") => format!(
            "{err}\nhint: write mode needs OpenCode CLI; run `opencode --version`, install OpenCode if missing, then configure providers/models."
        ),
        XurlError::CommandFailed { command, .. } if command.contains("amp") => {
            format!("{err}\nhint: verify authentication with `amp login` and retry.")
        }
        XurlError::CommandFailed { command, .. } if command.contains("codex") => {
            format!("{err}\nhint: verify authentication with `codex login` and retry.")
        }
        XurlError::CommandFailed { command, .. } if command.contains("claude") => format!(
            "{err}\nhint: verify authentication with `claude auth` (or your configured login flow) and retry."
        ),
        XurlError::CommandFailed { command, .. } if command.contains("gemini") => format!(
            "{err}\nhint: verify Gemini authentication/configuration and retry the command directly once."
        ),
        XurlError::CommandFailed { command, .. } if command.contains("pi") => format!(
            "{err}\nhint: verify pi provider/model credentials and retry with `pi -p \"hello\" --mode json`."
        ),
        XurlError::CommandFailed { command, .. } if command.contains("opencode") => format!(
            "{err}\nhint: verify OpenCode provider/model configuration and retry with `opencode run \"hello\" --format json`."
        ),
        _ => err.to_string(),
    }
}
