use std::env;
use std::path::PathBuf;

use dirs::home_dir;

use crate::error::{Result, XurlError};
use crate::model::{ProviderKind, ResolvedThread, WriteRequest, WriteResult};

pub mod amp;
pub mod claude;
pub mod codex;
pub mod gemini;
pub mod openclaw;
pub mod opencode;
pub mod pi;

pub(crate) fn append_passthrough_args(args: &mut Vec<String>, params: &[(String, Option<String>)]) {
    append_passthrough_args_excluding(args, params, &[]);
}

pub(crate) fn append_passthrough_args_excluding(
    args: &mut Vec<String>,
    params: &[(String, Option<String>)],
    excluded_keys: &[&str],
) -> Vec<String> {
    let mut excluded = Vec::new();
    for (key, value) in params {
        if excluded_keys.iter().any(|candidate| candidate == key) {
            excluded.push(key.clone());
            continue;
        }
        args.push(format!("--{key}"));
        if let Some(value) = value
            && !value.is_empty()
        {
            args.push(value.clone());
        }
    }
    excluded
}

pub trait WriteEventSink {
    fn on_session_ready(&mut self, provider: ProviderKind, session_id: &str) -> Result<()>;
    fn on_text_delta(&mut self, text: &str) -> Result<()>;
}

pub trait Provider {
    fn kind(&self) -> ProviderKind;
    fn resolve(&self, session_id: &str) -> Result<ResolvedThread>;
    fn write(&self, req: &WriteRequest, sink: &mut dyn WriteEventSink) -> Result<WriteResult> {
        let _ = (req, sink);
        Err(XurlError::UnsupportedProviderWrite(self.kind().to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRoots {
    pub amp_root: PathBuf,
    pub codex_root: PathBuf,
    pub claude_root: PathBuf,
    pub gemini_root: PathBuf,
    pub pi_root: PathBuf,
    pub opencode_root: PathBuf,
    pub openclaw_root: PathBuf,
}

impl ProviderRoots {
    pub fn from_env_or_home() -> Result<Self> {
        let home = home_dir().ok_or(XurlError::HomeDirectoryNotFound)?;

        // Precedence:
        // 1) XDG_DATA_HOME/amp
        // 2) ~/.local/share/amp
        let amp_root = env::var_os("XDG_DATA_HOME")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .map(|path| path.join("amp"))
            .unwrap_or_else(|| home.join(".local/share/amp"));

        // Precedence:
        // 1) CODEX_HOME (official Codex home env)
        // 2) ~/.codex (Codex default)
        let codex_root = env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));

        // Precedence:
        // 1) CLAUDE_CONFIG_DIR (official Claude Code config/data root env)
        // 2) ~/.claude (Claude default)
        let claude_root = env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"));

        // Precedence:
        // 1) GEMINI_CLI_HOME/.gemini (official Gemini CLI home env)
        // 2) ~/.gemini (Gemini default)
        let gemini_root = env::var_os("GEMINI_CLI_HOME")
            .map(PathBuf::from)
            .map(|path| path.join(".gemini"))
            .unwrap_or_else(|| home.join(".gemini"));

        // Precedence:
        // 1) PI_CODING_AGENT_DIR (official pi coding agent root env)
        // 2) ~/.pi/agent (pi default)
        let pi_root = env::var_os("PI_CODING_AGENT_DIR")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".pi/agent"));

        // Precedence:
        // 1) XDG_DATA_HOME/opencode
        // 2) ~/.local/share/opencode
        let opencode_root = env::var_os("XDG_DATA_HOME")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .map(|path| path.join("opencode"))
            .unwrap_or_else(|| home.join(".local/share/opencode"));

        // Precedence:
        // 1) OPENCLAW_HOME
        // 2) ~/.openclaw
        let openclaw_root = env::var_os("OPENCLAW_HOME")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".openclaw"));

        Ok(Self {
            amp_root,
            codex_root,
            claude_root,
            gemini_root,
            pi_root,
            opencode_root,
            openclaw_root,
        })
    }
}
