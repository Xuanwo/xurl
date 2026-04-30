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
    let requested_uri = cli.uri.clone();

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {}", user_facing_error(&requested_uri, &err));
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
            | xurl_core::ProviderKind::Copilot
            | xurl_core::ProviderKind::Claude
            | xurl_core::ProviderKind::Amp
            | xurl_core::ProviderKind::Cursor
            | xurl_core::ProviderKind::Gemini
            | xurl_core::ProviderKind::Kimi
            | xurl_core::ProviderKind::Opencode
            | xurl_core::ProviderKind::Openclaw => uri.agent_id.is_some(),
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

const ISSUE_CREATE_URL: &str = "https://github.com/Xuanwo/xurl/issues/new";
const SUPPORTED_PROVIDERS: &[&str] = &[
    "amp", "copilot", "codex", "claude", "cursor", "gemini", "kimi", "openclaw", "pi", "opencode",
];

#[derive(Default)]
struct ErrorReport {
    summary: String,
    fields: Vec<(String, String)>,
    lists: Vec<(String, Vec<String>)>,
    next_steps: Vec<String>,
}

impl ErrorReport {
    fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            ..Self::default()
        }
    }

    fn field(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.push((key.into(), flatten_line(&value.into())));
        self
    }

    fn list(mut self, key: impl Into<String>, values: Vec<String>) -> Self {
        if !values.is_empty() {
            self.lists.push((key.into(), values));
        }
        self
    }

    fn steps(mut self, values: Vec<String>) -> Self {
        self.next_steps.extend(values);
        self
    }

    fn render(self) -> String {
        let mut output = self.summary;
        for (key, value) in self.fields {
            output.push('\n');
            output.push_str(&format!("{key}: {value}"));
        }
        for (key, values) in self.lists {
            output.push('\n');
            output.push_str(&format!("{key}:"));
            for value in values {
                output.push('\n');
                output.push_str(&format!("  - {}", flatten_line(&value)));
            }
        }
        if !self.next_steps.is_empty() {
            output.push('\n');
            output.push_str("next_steps:");
            for step in self.next_steps {
                output.push('\n');
                output.push_str(&format!("  - {}", flatten_line(&step)));
            }
        }
        output
    }
}

fn flatten_line(value: &str) -> String {
    value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" | ")
}

fn requested_provider(input: &str) -> Option<&str> {
    let target = if let Some(rest) = input.strip_prefix("agents://") {
        rest
    } else if let Some((scheme, _)) = input.split_once("://") {
        return Some(scheme);
    } else {
        input
    };

    target
        .split(['/', '?'])
        .find(|segment| !segment.is_empty())
        .filter(|segment| *segment != "." && *segment != ".." && *segment != "~")
}

fn expected_session_id_shape(provider: Option<&str>) -> Option<&'static str> {
    match provider {
        Some("amp") => Some("T-<uuid>"),
        Some("opencode") => Some("ses_<id>"),
        Some("codex") | Some("copilot") | Some("claude") | Some("cursor") | Some("gemini")
        | Some("kimi") | Some("openclaw") | Some("pi") => Some("<uuid>"),
        _ => None,
    }
}

fn provider_root_check(provider: &str) -> Option<String> {
    match provider {
        "amp" => {
            Some("verify XDG_DATA_HOME/amp or ~/.local/share/amp contains this thread".to_string())
        }
        "copilot" => Some("verify COPILOT_HOME or ~/.copilot contains this thread".to_string()),
        "codex" => Some("verify CODEX_HOME or ~/.codex contains this thread".to_string()),
        "claude" => Some("verify CLAUDE_CONFIG_DIR or ~/.claude contains this thread".to_string()),
        "cursor" => Some(
            "verify CURSOR_DATA_DIR, CURSOR_CONFIG_DIR, or ~/.cursor contains this thread"
                .to_string(),
        ),
        "gemini" => {
            Some("verify GEMINI_CLI_HOME/.gemini or ~/.gemini contains this thread".to_string())
        }
        "kimi" => Some("verify KIMI_SHARE_DIR or ~/.kimi contains this thread".to_string()),
        "openclaw" => Some(
            "verify OPENCLAW_HOME or ~/.openclaw contains this thread".to_string(),
        ),
        "pi" => Some("verify PI_CODING_AGENT_DIR or ~/.pi/agent contains this thread".to_string()),
        "opencode" => Some(
            "verify XDG_DATA_HOME/opencode or ~/.local/share/opencode contains this thread"
                .to_string(),
        ),
        _ => None,
    }
}

fn provider_command_steps(command: &str, failed: bool) -> Vec<String> {
    let program = command.split_whitespace().next().unwrap_or(command);
    match program {
        "amp" => {
            if failed {
                vec![
                    "verify authentication with `amp login`".to_string(),
                    "retry the provider command directly once to inspect its stderr".to_string(),
                ]
            } else {
                vec![
                    "run `amp --version`".to_string(),
                    "install Amp CLI if missing, then run `amp login`".to_string(),
                ]
            }
        }
        "codex" => {
            if failed {
                vec![
                    "verify authentication with `codex login`".to_string(),
                    "retry the provider command directly once to inspect its stderr".to_string(),
                ]
            } else {
                vec![
                    "run `codex --version`".to_string(),
                    "install Codex CLI if missing, then run `codex login`".to_string(),
                ]
            }
        }
        "copilot" => {
            if failed {
                vec![
                    "verify authentication with `copilot login`".to_string(),
                    "retry the equivalent `copilot -p ... --output-format json` command directly once".to_string(),
                ]
            } else {
                vec![
                    "run `copilot --version`".to_string(),
                    "install GitHub Copilot CLI if missing, then authenticate with `copilot login`"
                        .to_string(),
                ]
            }
        }
        "claude" => {
            if failed {
                vec![
                    "verify authentication with `claude auth` or your configured login flow"
                        .to_string(),
                    "retry the provider command directly once to inspect its stderr".to_string(),
                ]
            } else {
                vec![
                    "run `claude --version`".to_string(),
                    "install Claude Code if missing, then authenticate".to_string(),
                ]
            }
        }
        "cursor-agent" => {
            if failed {
                vec![
                    "verify authentication with `cursor-agent login`".to_string(),
                    "confirm the workspace is trusted for headless mode, then retry".to_string(),
                ]
            } else {
                vec![
                    "run `cursor-agent --version`".to_string(),
                    "install Cursor Agent if missing, then authenticate with `cursor-agent login`"
                        .to_string(),
                ]
            }
        }
        "gemini" => {
            if failed {
                vec![
                    "verify Gemini authentication and configuration".to_string(),
                    "retry the provider command directly once to inspect its stderr".to_string(),
                ]
            } else {
                vec![
                    "run `gemini --version`".to_string(),
                    "install Gemini CLI if missing, then authenticate".to_string(),
                ]
            }
        }
        "openclaw" => {
            if failed {
                vec![
                    "verify OpenClaw gateway and authentication configuration".to_string(),
                    "retry with `openclaw agent --message \"hello\" --json` to inspect provider output"
                        .to_string(),
                ]
            } else {
                vec![
                    "run `openclaw --version` (or check XURL_OPENCLAW_BIN)".to_string(),
                    "install OpenClaw if missing, ensure `node` is available in PATH".to_string(),
                ]
            }
        }
        "pi" => {
            if failed {
                vec![
                    "verify pi provider or model credentials".to_string(),
                    "retry with `pi -p \"hello\" --mode json` to inspect provider output"
                        .to_string(),
                ]
            } else {
                vec![
                    "run `pi --version`".to_string(),
                    "install pi if missing, then configure provider credentials".to_string(),
                ]
            }
        }
        "opencode" => {
            if failed {
                vec![
                    "verify OpenCode provider and model configuration".to_string(),
                    "retry with `opencode run \"hello\" --format json` to inspect provider output"
                        .to_string(),
                ]
            } else {
                vec![
                    "run `opencode --version`".to_string(),
                    "install OpenCode CLI if missing, then configure providers and models"
                        .to_string(),
                ]
            }
        }
        _ => {
            if failed {
                vec!["retry the provider command directly once to inspect its stderr".to_string()]
            } else {
                vec!["install the required provider CLI and make sure it is on PATH".to_string()]
            }
        }
    }
}

fn invalid_mode_report(requested_uri: &str, detail: &str) -> ErrorReport {
    let provider = requested_provider(requested_uri);

    if detail.contains("does not support role-based write URI") {
        let provider_name = provider.unwrap_or("this provider");
        return ErrorReport::new(format!(
            "provider `{provider_name}` does not support role-based create in write mode"
        ))
        .field("requested_uri", requested_uri)
        .field("detail", detail)
        .steps(vec![
            format!("create without a role: `xurl agents://{provider_name} -d \"...\"`"),
            format!(
                "if you need role-based create for `{provider_name}`, open an issue: {ISSUE_CREATE_URL}"
            ),
        ]);
    }

    if detail.contains("read mode requires a thread URI") {
        return ErrorReport::new("read mode requires a thread URI")
            .field("requested_uri", requested_uri)
            .steps(vec![
                "read one conversation with `xurl agents://<provider>/<session_id>`".to_string(),
                "query conversations with `xurl agents://<provider>`".to_string(),
            ]);
    }

    if detail.contains("head mode (-I/--head) cannot be combined with write mode") {
        return ErrorReport::new("head mode cannot be combined with write mode")
            .field("requested_uri", requested_uri)
            .steps(vec![
                "remove `-I/--head` to write".to_string(),
                "remove `-d/--data` to inspect frontmatter only".to_string(),
            ]);
    }

    if detail.contains("write mode does not support path-scoped query URIs") {
        return ErrorReport::new("write mode does not support path-scoped query URIs")
            .field("requested_uri", requested_uri)
            .steps(vec![
                "write to a provider URI such as `xurl agents://codex -d \"...\"`".to_string(),
                "use path-scoped URIs only for query mode".to_string(),
            ]);
    }

    if detail.contains("write mode only supports main thread URIs") {
        return ErrorReport::new("write mode only supports provider or main thread URIs")
            .field("requested_uri", requested_uri)
            .steps(vec![
                "create with `xurl agents://<provider> -d \"...\"`".to_string(),
                "append with `xurl agents://<provider>/<session_id> -d \"...\"`".to_string(),
            ]);
    }

    if detail.contains("subagent index mode requires") {
        return ErrorReport::new("subagent index mode requires a main thread URI")
            .field("requested_uri", requested_uri)
            .steps(vec![
                "use `xurl -I agents://<provider>/<main_thread_id>`".to_string(),
            ]);
    }

    if detail.contains("subagent drill-down requires") {
        return ErrorReport::new("subagent drill-down requires a child URI")
            .field("requested_uri", requested_uri)
            .steps(vec![
                "discover child ids first with `xurl -I agents://<provider>/<main_thread_id>`"
                    .to_string(),
                "then read one child with `xurl agents://<provider>/<main_thread_id>/<agent_id>`"
                    .to_string(),
            ]);
    }

    ErrorReport::new("invalid mode")
        .field("requested_uri", requested_uri)
        .field("detail", detail)
}

fn user_facing_error(requested_uri: &str, err: &XurlError) -> String {
    match err {
        XurlError::InvalidUri(detail) => ErrorReport::new("invalid URI")
            .field("requested_uri", requested_uri)
            .field("detail", detail)
            .steps(vec![
                "use `xurl agents://<provider>` to query conversations".to_string(),
                "use `xurl agents://<provider>/<session_id>` to read one conversation".to_string(),
            ])
            .render(),
        XurlError::UnsupportedScheme(scheme) => {
            let kind = if requested_uri.starts_with("agents://") || !requested_uri.contains("://")
            {
                "provider"
            } else {
                "scheme"
            };
            ErrorReport::new(format!("unsupported {kind} `{scheme}`"))
                .field("requested_uri", requested_uri)
                .field("supported_providers", SUPPORTED_PROVIDERS.join(", "))
                .steps(vec![
                    if kind == "provider" {
                        "use one of the supported providers above".to_string()
                    } else {
                        "use the `agents://<provider>/...` URI family".to_string()
                    },
                    format!(
                        "if you need `{scheme}` support, open an issue: {ISSUE_CREATE_URL}"
                    ),
                ])
                .render()
        }
        XurlError::InvalidSessionId(session_id) => {
            let provider = requested_provider(requested_uri);
            let report = ErrorReport::new(format!("invalid session id `{session_id}`"))
                .field("requested_uri", requested_uri)
                .field("provider", provider.unwrap_or("unknown"));
            let report = if let Some(shape) = expected_session_id_shape(provider) {
                report.field("expected_format", shape)
            } else {
                report
            };
            report
                .steps(vec![
                    "verify that the session or child id matches the provider format above".to_string(),
                    "if this token is a role, use it in query mode or with `-d` instead of read mode".to_string(),
                ])
                .render()
        }
        XurlError::InvalidMode(detail) => invalid_mode_report(requested_uri, detail).render(),
        XurlError::UnsupportedSubagentProvider(provider) => ErrorReport::new(format!(
            "provider `{provider}` does not support child/subagent drill-down"
        ))
        .field("requested_uri", requested_uri)
        .steps(vec![
            format!("read the main conversation with `xurl agents://{provider}/<session_id>`"),
            format!(
                "if you need subagent support for `{provider}`, open an issue: {ISSUE_CREATE_URL}"
            ),
        ])
        .render(),
        XurlError::UnsupportedProviderWrite(provider) => ErrorReport::new(format!(
            "provider `{provider}` does not support write mode"
        ))
        .field("requested_uri", requested_uri)
        .steps(vec![
            format!("use `{provider}` for read, query, or discover only"),
            format!(
                "if you need write support for `{provider}`, open an issue: {ISSUE_CREATE_URL}"
            ),
        ])
        .render(),
        XurlError::CommandNotFound { command } => ErrorReport::new(format!(
            "required provider CLI `{command}` is not available"
        ))
        .field("requested_uri", requested_uri)
        .field("command", command)
        .steps(provider_command_steps(command, false))
        .render(),
        XurlError::CommandFailed {
            command,
            code,
            stderr,
        } => {
            let report = ErrorReport::new("provider CLI command failed")
                .field("requested_uri", requested_uri)
                .field("command", command)
                .field(
                    "exit_code",
                    code.map_or_else(|| "unknown".to_string(), |value| value.to_string()),
                );
            let report = if stderr.trim().is_empty() {
                report
            } else {
                report.field("stderr", stderr)
            };
            report.steps(provider_command_steps(command, true)).render()
        }
        XurlError::WriteProtocol(detail) => ErrorReport::new(
            "provider CLI output did not match the xurl write protocol",
        )
        .field("requested_uri", requested_uri)
        .field("detail", detail)
        .steps(vec![
            "retry the provider command directly once to confirm its current output format"
                .to_string(),
            format!(
                "if the provider CLI output format changed, open an issue: {ISSUE_CREATE_URL}"
            ),
        ])
        .render(),
        XurlError::Serialization(detail) => ErrorReport::new("failed to serialize xurl data")
            .field("requested_uri", requested_uri)
            .field("detail", detail)
            .steps(vec![format!(
                "open an issue with the failing URI and provider output: {ISSUE_CREATE_URL}"
            )])
            .render(),
        XurlError::HomeDirectoryNotFound => ErrorReport::new("cannot determine home directory")
            .field("requested_uri", requested_uri)
            .steps(vec![
                "set `HOME` before running xurl".to_string(),
                "or set provider-specific root env vars such as `CODEX_HOME` or `CLAUDE_CONFIG_DIR`".to_string(),
            ])
            .render(),
        XurlError::ThreadNotFound {
            provider,
            session_id,
            searched_roots,
        } => {
            let report = ErrorReport::new(format!(
                "thread not found for provider `{provider}` session `{session_id}`"
            ))
            .field("requested_uri", requested_uri)
            .list(
                "searched_roots",
                searched_roots
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect(),
            );
            let mut steps = vec![
                format!("run `xurl agents://{provider}` to list local conversations"),
                "verify that the session id is correct".to_string(),
            ];
            if let Some(root_step) = provider_root_check(provider) {
                steps.push(root_step);
            }
            report.steps(steps).render()
        }
        XurlError::EntryNotFound {
            provider,
            session_id,
            entry_id,
        } => ErrorReport::new(format!(
            "entry not found for provider `{provider}` session `{session_id}` entry `{entry_id}`"
        ))
        .field("requested_uri", requested_uri)
        .steps(vec![
            format!(
                "run `xurl -I agents://{provider}/{session_id}` to discover valid entry or child ids"
            ),
            "verify that the entry id is correct".to_string(),
        ])
        .render(),
        XurlError::EmptyThreadFile { path } => ErrorReport::new("thread file is empty")
            .field("requested_uri", requested_uri)
            .field("path", path.display().to_string())
            .steps(vec![
                "inspect whether the provider wrote an incomplete local transcript".to_string(),
                "regenerate or restore the thread file, then retry".to_string(),
            ])
            .render(),
        XurlError::NonUtf8ThreadFile { path } => {
            ErrorReport::new("thread file is not valid UTF-8")
                .field("requested_uri", requested_uri)
                .field("path", path.display().to_string())
                .steps(vec![
                    "inspect the local thread file encoding".to_string(),
                    "regenerate the provider transcript if the file is corrupted".to_string(),
                ])
                .render()
        }
        XurlError::Io { path, source } => ErrorReport::new("i/o error")
            .field("requested_uri", requested_uri)
            .field("path", path.display().to_string())
            .field("detail", source.to_string())
            .steps(vec![
                "verify that the path exists and parent directories are writable".to_string(),
                "check file permissions, then retry".to_string(),
            ])
            .render(),
        XurlError::Sqlite { path, source } => ErrorReport::new("sqlite error")
            .field("requested_uri", requested_uri)
            .field("path", path.display().to_string())
            .field("detail", source.to_string())
            .steps(vec![
                "inspect the sqlite file for corruption or schema mismatch".to_string(),
                "retry after the provider finishes writing to the database".to_string(),
            ])
            .render(),
        XurlError::InvalidJsonLine { path, line, source } => {
            ErrorReport::new("invalid JSON line in local thread data")
                .field("requested_uri", requested_uri)
                .field("path", path.display().to_string())
                .field("line", line.to_string())
                .field("detail", source.to_string())
                .steps(vec![
                    "inspect the local thread file at the line above".to_string(),
                    "regenerate the provider transcript if the file is truncated or corrupted"
                        .to_string(),
                ])
                .render()
        }
    }
}
