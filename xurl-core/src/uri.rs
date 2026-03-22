use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use dirs::home_dir;
use once_cell::sync::Lazy;
use regex::Regex;

use crate::error::{Result, XurlError};
use crate::model::{PathThreadQuery, ProviderKind, ThreadQuery};

static SESSION_ID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
        .expect("valid regex")
});
static AMP_SESSION_ID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^t-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
        .expect("valid regex")
});
static OPENCODE_SESSION_ID_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^ses_[0-9A-Za-z]+$").expect("valid regex"));
static PI_SHORT_ENTRY_ID_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^[0-9a-f]{8}$").expect("valid regex"));

pub fn is_uuid_session_id(input: &str) -> bool {
    SESSION_ID_RE.is_match(input)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsUri {
    pub provider: ProviderKind,
    pub session_id: String,
    pub agent_id: Option<String>,
    pub query: Vec<(String, Option<String>)>,
}

impl AgentsUri {
    pub fn parse(input: &str) -> Result<Self> {
        input.parse()
    }

    pub fn is_collection(&self) -> bool {
        self.session_id.is_empty() && self.agent_id.is_none()
    }

    pub fn require_session_id(&self) -> Result<&str> {
        if self.session_id.is_empty() {
            return Err(XurlError::InvalidMode(
                "session id is required for this operation".to_string(),
            ));
        }
        Ok(&self.session_id)
    }

    pub fn as_agents_string(&self) -> String {
        if self.is_collection() {
            return format!("agents://{}", self.provider);
        }

        match &self.agent_id {
            Some(agent_id) => format!(
                "agents://{}/{}/{}",
                self.provider, self.session_id, agent_id
            ),
            None => format!("agents://{}/{}", self.provider, self.session_id),
        }
    }

    pub fn as_string(&self) -> String {
        if self.is_collection() {
            return self.as_agents_string();
        }

        match &self.agent_id {
            Some(agent_id) => format!("{}://{}/{}", self.provider, self.session_id, agent_id),
            None => format!("{}://{}", self.provider, self.session_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleUri {
    pub provider: ProviderKind,
    pub role: String,
    pub query: Vec<(String, Option<String>)>,
}

impl RoleUri {
    pub fn parse(input: &str) -> Result<Option<Self>> {
        parse_role_uri(input)
    }

    pub fn as_agents_string(&self) -> String {
        format!("agents://{}/{}", self.provider, self.role)
    }
}

type ParsedTarget<'a> = (ProviderKind, &'a str, Option<String>, bool);

fn parse_agents_target<'a>(target: &'a str, input: &str) -> Result<ParsedTarget<'a>> {
    let mut segments = target.split('/');
    let provider_scheme = segments
        .next()
        .ok_or_else(|| XurlError::InvalidUri(input.to_string()))?;
    if provider_scheme.is_empty() {
        return Err(XurlError::InvalidUri(input.to_string()));
    }
    let provider = parse_provider(provider_scheme)?;
    let mut remaining = segments.collect::<Vec<_>>();
    if remaining.iter().any(|segment| segment.is_empty()) {
        return Err(XurlError::InvalidUri(input.to_string()));
    }

    if provider == ProviderKind::Codex
        && remaining.len() >= 2
        && remaining.first().copied() == Some("threads")
    {
        remaining.remove(0);
    }

    match remaining.as_slice() {
        [] => Ok((provider, "", None, true)),
        [main_id] => Ok((provider, *main_id, None, true)),
        [main_id, agent_id] => Ok((provider, *main_id, Some((*agent_id).to_string()), true)),
        _ => Err(XurlError::InvalidUri(input.to_string())),
    }
}

fn parse_legacy_target<'a>(scheme: &str, target: &'a str, input: &str) -> Result<ParsedTarget<'a>> {
    let provider = parse_provider(scheme)?;
    let normalized_target = match provider {
        ProviderKind::Amp => target,
        ProviderKind::Codex => target.strip_prefix("threads/").unwrap_or(target),
        ProviderKind::Claude | ProviderKind::Gemini | ProviderKind::Pi | ProviderKind::Opencode => {
            target
        }
    };
    let mut segments = normalized_target.split('/');
    let main_id = segments.next().unwrap_or_default();
    let agent_id = segments.next().map(str::to_string);

    if main_id.is_empty()
        || segments.next().is_some()
        || agent_id.as_deref().is_some_and(str::is_empty)
    {
        return Err(XurlError::InvalidUri(input.to_string()));
    }

    Ok((provider, main_id, agent_id, false))
}

impl FromStr for AgentsUri {
    type Err = XurlError;

    fn from_str(input: &str) -> Result<Self> {
        let (scheme, target_with_query) = input
            .split_once("://")
            .map_or((None, input), |(scheme, target)| (Some(scheme), target));
        let (target, raw_query) = split_target_and_query(target_with_query);

        let query = parse_query(raw_query, input)?;

        let (provider, raw_id, raw_agent_id, allows_collection) = match scheme {
            Some("agents") => parse_agents_target(target, input)?,
            Some(scheme) => parse_legacy_target(scheme, target, input)?,
            None => parse_agents_target(target, input)?,
        };

        if raw_id.is_empty() {
            if !(allows_collection && raw_agent_id.is_none()) {
                return Err(XurlError::InvalidUri(input.to_string()));
            }

            return Ok(Self {
                provider,
                session_id: String::new(),
                agent_id: None,
                query,
            });
        }

        match provider {
            ProviderKind::Amp if !AMP_SESSION_ID_RE.is_match(raw_id) => {
                return Err(XurlError::InvalidSessionId(raw_id.to_string()));
            }
            ProviderKind::Codex
            | ProviderKind::Claude
            | ProviderKind::Gemini
            | ProviderKind::Pi
                if !is_uuid_session_id(raw_id) =>
            {
                return Err(XurlError::InvalidSessionId(raw_id.to_string()));
            }
            ProviderKind::Opencode if !OPENCODE_SESSION_ID_RE.is_match(raw_id) => {
                return Err(XurlError::InvalidSessionId(raw_id.to_string()));
            }
            _ => {}
        }

        if provider == ProviderKind::Amp
            && let Some(agent_id) = raw_agent_id.as_deref()
            && !AMP_SESSION_ID_RE.is_match(agent_id)
        {
            return Err(XurlError::InvalidSessionId(agent_id.to_string()));
        }

        let session_id = match provider {
            ProviderKind::Amp => format!("T-{}", raw_id[2..].to_ascii_lowercase()),
            ProviderKind::Codex
            | ProviderKind::Claude
            | ProviderKind::Gemini
            | ProviderKind::Pi => raw_id.to_ascii_lowercase(),
            ProviderKind::Opencode => raw_id.to_string(),
        };

        let agent_id = raw_agent_id.map(|agent_id| {
            if provider == ProviderKind::Amp && AMP_SESSION_ID_RE.is_match(&agent_id) {
                format!("T-{}", agent_id[2..].to_ascii_lowercase())
            } else if ((provider == ProviderKind::Codex || provider == ProviderKind::Gemini)
                && SESSION_ID_RE.is_match(&agent_id))
                || (provider == ProviderKind::Pi
                    && (is_uuid_session_id(&agent_id) || PI_SHORT_ENTRY_ID_RE.is_match(&agent_id)))
            {
                agent_id.to_ascii_lowercase()
            } else {
                agent_id
            }
        });

        if provider == ProviderKind::Opencode
            && let Some(child_id) = agent_id.as_deref()
            && !OPENCODE_SESSION_ID_RE.is_match(child_id)
        {
            return Err(XurlError::InvalidSessionId(child_id.to_string()));
        }

        Ok(Self {
            provider,
            session_id,
            agent_id,
            query,
        })
    }
}

fn split_target_and_query(input: &str) -> (&str, Option<&str>) {
    if let Some((target, query)) = input.split_once('?') {
        (target, Some(query))
    } else {
        (input, None)
    }
}

fn parse_query(raw_query: Option<&str>, full_input: &str) -> Result<Vec<(String, Option<String>)>> {
    let Some(raw_query) = raw_query else {
        return Ok(Vec::new());
    };

    if raw_query.is_empty() {
        return Ok(Vec::new());
    }

    let mut query = Vec::new();
    for pair in raw_query.split('&') {
        if pair.is_empty() {
            continue;
        }

        let (raw_key, raw_value) = if let Some((key, value)) = pair.split_once('=') {
            (key, Some(value))
        } else {
            (pair, None)
        };

        let key =
            percent_decode(raw_key).ok_or_else(|| XurlError::InvalidUri(full_input.to_string()))?;
        if key.is_empty() {
            return Err(XurlError::InvalidUri(full_input.to_string()));
        }

        let value = raw_value
            .map(|value| {
                percent_decode(value).ok_or_else(|| XurlError::InvalidUri(full_input.to_string()))
            })
            .transpose()?;

        query.push((key, value));
    }

    Ok(query)
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut idx = 0;

    while idx < bytes.len() {
        if bytes[idx] == b'%' {
            if idx + 2 >= bytes.len() {
                return None;
            }
            let hi = hex_value(bytes[idx + 1])?;
            let lo = hex_value(bytes[idx + 2])?;
            out.push((hi << 4) | lo);
            idx += 3;
        } else {
            out.push(bytes[idx]);
            idx += 1;
        }
    }

    String::from_utf8(out).ok()
}

fn hex_value(ch: u8) -> Option<u8> {
    match ch {
        b'0'..=b'9' => Some(ch - b'0'),
        b'a'..=b'f' => Some(10 + ch - b'a'),
        b'A'..=b'F' => Some(10 + ch - b'A'),
        _ => None,
    }
}

fn parse_provider(scheme: &str) -> Result<ProviderKind> {
    match scheme {
        "amp" => Ok(ProviderKind::Amp),
        "codex" => Ok(ProviderKind::Codex),
        "claude" => Ok(ProviderKind::Claude),
        "gemini" => Ok(ProviderKind::Gemini),
        "pi" => Ok(ProviderKind::Pi),
        "opencode" => Ok(ProviderKind::Opencode),
        _ => Err(XurlError::UnsupportedScheme(scheme.to_string())),
    }
}

fn looks_like_session_id(provider: ProviderKind, token: &str) -> bool {
    match provider {
        ProviderKind::Amp => AMP_SESSION_ID_RE.is_match(token),
        ProviderKind::Codex | ProviderKind::Claude | ProviderKind::Gemini | ProviderKind::Pi => {
            is_uuid_session_id(token)
        }
        ProviderKind::Opencode => OPENCODE_SESSION_ID_RE.is_match(token),
    }
}

pub fn parse_role_uri(input: &str) -> Result<Option<RoleUri>> {
    let (scheme, target_with_query) = input
        .split_once("://")
        .map_or((None, input), |(scheme, target)| (Some(scheme), target));
    let (target, raw_query) = split_target_and_query(target_with_query);
    let query = parse_query(raw_query, input)?;

    let (provider, raw_id, raw_agent_id, _) = match scheme {
        Some("agents") => parse_agents_target(target, input)?,
        Some(scheme) => parse_legacy_target(scheme, target, input)?,
        None => parse_agents_target(target, input)?,
    };

    if raw_id.is_empty() || raw_agent_id.is_some() || looks_like_session_id(provider, raw_id) {
        return Ok(None);
    }

    Ok(Some(RoleUri {
        provider,
        role: raw_id.to_string(),
        query,
    }))
}

fn parse_thread_query_pairs(
    input: &str,
    query_raw: &str,
) -> Result<(Option<String>, usize, Vec<String>)> {
    let mut q = None::<String>;
    let mut limit = None::<usize>;
    let mut ignored_params = Vec::<String>::new();

    for pair in query_raw.split('&').filter(|pair| !pair.is_empty()) {
        let (raw_key, raw_value) = pair.split_once('=').map_or((pair, ""), |parts| parts);
        let key = percent_decode_component(raw_key)?;
        let value = percent_decode_component(raw_value)?;

        match key.as_str() {
            "q" => {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    q = Some(trimmed.to_string());
                }
            }
            "limit" => {
                limit = Some(value.parse::<usize>().map_err(|_| {
                    XurlError::InvalidUri(format!("{input} (invalid limit={value})"))
                })?);
            }
            _ => {
                if !ignored_params.iter().any(|existing| existing == &key) {
                    ignored_params.push(key);
                }
            }
        }
    }

    Ok((q, limit.unwrap_or(10), ignored_params))
}

struct PathThreadQueryParams {
    q: Option<String>,
    limit: usize,
    providers: Option<Vec<ProviderKind>>,
    ignored_params: Vec<String>,
}

fn parse_path_thread_query_pairs(input: &str, query_raw: &str) -> Result<PathThreadQueryParams> {
    let mut q = None::<String>;
    let mut limit = None::<usize>;
    let mut providers = None::<Vec<ProviderKind>>;
    let mut ignored_params = Vec::<String>::new();

    for pair in query_raw.split('&').filter(|pair| !pair.is_empty()) {
        let (raw_key, raw_value) = pair.split_once('=').map_or((pair, ""), |parts| parts);
        let key = percent_decode_component(raw_key)?;
        let value = percent_decode_component(raw_value)?;

        match key.as_str() {
            "q" => {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    q = Some(trimmed.to_string());
                }
            }
            "limit" => {
                limit = Some(value.parse::<usize>().map_err(|_| {
                    XurlError::InvalidUri(format!("{input} (invalid limit={value})"))
                })?);
            }
            "providers" => {
                providers = Some(parse_provider_list(input, &value)?);
            }
            _ => {
                if !ignored_params.iter().any(|existing| existing == &key) {
                    ignored_params.push(key);
                }
            }
        }
    }

    Ok(PathThreadQueryParams {
        q,
        limit: limit.unwrap_or(10),
        providers,
        ignored_params,
    })
}

fn parse_provider_list(input: &str, raw: &str) -> Result<Vec<ProviderKind>> {
    let mut providers = Vec::new();
    for entry in raw.split(',') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            return Err(XurlError::InvalidUri(format!(
                "{input} (invalid providers={raw})"
            )));
        }
        let provider = parse_provider(trimmed)?;
        if !providers.contains(&provider) {
            providers.push(provider);
        }
    }

    if providers.is_empty() {
        return Err(XurlError::InvalidUri(format!(
            "{input} (invalid providers={raw})"
        )));
    }

    Ok(providers)
}

pub fn parse_path_query_uri(input: &str) -> Result<Option<PathThreadQuery>> {
    let Some(target_with_query) = input.strip_prefix("agents://") else {
        return Ok(None);
    };

    let (target, raw_query) = split_target_and_query(target_with_query);
    let Some(scope_path) = normalize_path_query_target(target)? else {
        return Ok(None);
    };
    let raw_query = raw_query.unwrap_or_default();
    let params = parse_path_thread_query_pairs(input, raw_query)?;

    Ok(Some(PathThreadQuery {
        uri: canonical_path_query_uri(&scope_path, raw_query),
        scope_path: scope_path.display().to_string(),
        providers: params.providers,
        q: params.q,
        limit: params.limit,
        ignored_params: params.ignored_params,
    }))
}

fn canonical_path_query_uri(scope_path: &Path, raw_query: &str) -> String {
    let mut uri = format!("agents://{}", scope_path.display());
    if !raw_query.is_empty() {
        uri.push('?');
        uri.push_str(raw_query);
    }
    uri
}

fn normalize_path_query_target(target: &str) -> Result<Option<PathBuf>> {
    if target.is_empty() {
        return Ok(None);
    }

    if target.starts_with('/') {
        return Ok(Some(normalize_lexical_path(Path::new(target))));
    }

    if target == "." || target == ".." || target.starts_with("./") || target.starts_with("../") {
        let cwd = std::env::current_dir().map_err(|source| XurlError::Io {
            path: PathBuf::from("."),
            source,
        })?;
        return Ok(Some(normalize_lexical_path(&cwd.join(target))));
    }

    if target == "~" || target.starts_with("~/") {
        let home = home_dir().ok_or(XurlError::HomeDirectoryNotFound)?;
        let suffix = target.strip_prefix('~').unwrap_or_default();
        return Ok(Some(normalize_lexical_path(
            &home.join(suffix.strip_prefix('/').unwrap_or(suffix)),
        )));
    }

    Ok(None)
}

fn normalize_lexical_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    let mut saw_root = false;

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => {
                normalized.push(Path::new(component.as_os_str()));
                saw_root = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() && !saw_root {
                    normalized.push("..");
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    if normalized.as_os_str().is_empty() && saw_root {
        PathBuf::from(Path::new("/"))
    } else {
        normalized
    }
}

pub fn parse_collection_query_uri(input: &str) -> Result<Option<ThreadQuery>> {
    let target = if let Some(target) = input.strip_prefix("agents://") {
        target
    } else if input.contains("://") {
        return Ok(None);
    } else {
        input
    };

    let (provider_part, query_raw) = target.split_once('?').map_or((target, ""), |parts| parts);
    if provider_part.is_empty() || provider_part.contains('/') {
        return Ok(None);
    }

    let provider = parse_provider(provider_part)?;
    let (q, limit, ignored_params) = parse_thread_query_pairs(input, query_raw)?;

    Ok(Some(ThreadQuery {
        uri: input.to_string(),
        provider,
        role: None,
        q,
        limit,
        ignored_params,
    }))
}

pub fn parse_role_query_uri(input: &str) -> Result<Option<ThreadQuery>> {
    let Some(role_uri) = parse_role_uri(input)? else {
        return Ok(None);
    };

    let target = if let Some(target) = input.strip_prefix("agents://") {
        target
    } else if input.contains("://") {
        input.split_once("://").map_or("", |(_, target)| target)
    } else {
        input
    };
    let (_, query_raw) = target.split_once('?').map_or((target, ""), |parts| parts);
    let (q, limit, ignored_params) = parse_thread_query_pairs(input, query_raw)?;

    Ok(Some(ThreadQuery {
        uri: input.to_string(),
        provider: role_uri.provider,
        role: Some(role_uri.role),
        q,
        limit,
        ignored_params,
    }))
}

fn percent_decode_component(input: &str) -> Result<String> {
    let mut output = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        match bytes[idx] {
            b'+' => {
                output.push(b' ');
                idx += 1;
            }
            b'%' => {
                if idx + 2 >= bytes.len() {
                    return Err(XurlError::InvalidUri(format!(
                        "invalid percent encoding in query component: {input}"
                    )));
                }
                let h1 = hex_nibble(bytes[idx + 1]).ok_or_else(|| {
                    XurlError::InvalidUri(format!(
                        "invalid percent encoding in query component: {input}"
                    ))
                })?;
                let h2 = hex_nibble(bytes[idx + 2]).ok_or_else(|| {
                    XurlError::InvalidUri(format!(
                        "invalid percent encoding in query component: {input}"
                    ))
                })?;
                output.push((h1 << 4) | h2);
                idx += 3;
            }
            value => {
                output.push(value);
                idx += 1;
            }
        }
    }

    String::from_utf8(output).map_err(|_| {
        XurlError::InvalidUri(format!(
            "query component is not valid UTF-8 after percent decoding: {input}"
        ))
    })
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use dirs::home_dir;
    use tempfile::tempdir;

    use super::{
        AgentsUri, parse_collection_query_uri, parse_path_query_uri, parse_role_query_uri,
        parse_role_uri,
    };
    use crate::model::ProviderKind;

    #[test]
    fn parse_valid_uri() {
        let uri = AgentsUri::parse("codex://019c871c-b1f9-7f60-9c4f-87ed09f13592").expect("parse");
        assert_eq!(uri.provider, ProviderKind::Codex);
        assert_eq!(uri.session_id, "019c871c-b1f9-7f60-9c4f-87ed09f13592");
        assert_eq!(uri.agent_id, None);
        assert!(uri.query.is_empty());
    }

    #[test]
    fn parse_agents_collection_uri() {
        let uri = AgentsUri::parse("agents://codex").expect("parse");
        assert_eq!(uri.provider, ProviderKind::Codex);
        assert!(uri.session_id.is_empty());
        assert!(uri.is_collection());
    }

    #[test]
    fn parse_collection_uri_without_agents_prefix() {
        let uri = AgentsUri::parse("codex").expect("parse");
        assert_eq!(uri.provider, ProviderKind::Codex);
        assert!(uri.session_id.is_empty());
        assert!(uri.is_collection());
    }

    #[test]
    fn parse_agents_collection_with_query() {
        let uri = AgentsUri::parse("agents://codex?workdir=%2Ftmp&flag").expect("parse");
        assert_eq!(uri.provider, ProviderKind::Codex);
        assert!(uri.session_id.is_empty());
        assert_eq!(uri.query.len(), 2);
        assert_eq!(
            uri.query[0],
            ("workdir".to_string(), Some("/tmp".to_string()))
        );
        assert_eq!(uri.query[1], ("flag".to_string(), None));
    }

    #[test]
    fn parse_agents_uri_with_query_repeated_keys() {
        let uri = AgentsUri::parse(
            "agents://codex/019c871c-b1f9-7f60-9c4f-87ed09f13592?add_dir=%2Fa&add_dir=%2Fb",
        )
        .expect("parse should succeed");
        assert_eq!(uri.query.len(), 2);
        assert_eq!(
            uri.query[0],
            ("add_dir".to_string(), Some("/a".to_string()))
        );
        assert_eq!(
            uri.query[1],
            ("add_dir".to_string(), Some("/b".to_string()))
        );
    }

    #[test]
    fn parse_rejects_invalid_query_percent_encoding() {
        let err = AgentsUri::parse("agents://codex?workdir=%2").expect_err("must fail");
        assert!(format!("{err}").contains("invalid uri"));
    }

    #[test]
    fn parse_rejects_empty_query_key() {
        let err = AgentsUri::parse("agents://codex?=value").expect_err("must fail");
        assert!(format!("{err}").contains("invalid uri"));
    }

    #[test]
    fn parse_valid_amp_uri() {
        let uri = AgentsUri::parse("amp://T-019C0797-C402-7389-BD80-D785C98DF295").expect("parse");
        assert_eq!(uri.provider, ProviderKind::Amp);
        assert_eq!(uri.session_id, "T-019c0797-c402-7389-bd80-d785c98df295");
        assert_eq!(uri.agent_id, None);
    }

    #[test]
    fn parse_codex_deeplink_uri() {
        let uri = AgentsUri::parse("codex://threads/019c871c-b1f9-7f60-9c4f-87ed09f13592")
            .expect("parse should succeed");
        assert_eq!(uri.provider, ProviderKind::Codex);
        assert_eq!(uri.session_id, "019c871c-b1f9-7f60-9c4f-87ed09f13592");
        assert_eq!(uri.agent_id, None);
    }

    #[test]
    fn parse_agents_uri() {
        let uri = AgentsUri::parse("agents://codex/019c871c-b1f9-7f60-9c4f-87ed09f13592")
            .expect("parse should succeed");
        assert_eq!(uri.provider, ProviderKind::Codex);
        assert_eq!(uri.session_id, "019c871c-b1f9-7f60-9c4f-87ed09f13592");
        assert_eq!(uri.agent_id, None);
    }

    #[test]
    fn parse_agents_uri_without_agents_prefix() {
        let uri = AgentsUri::parse("codex/019c871c-b1f9-7f60-9c4f-87ed09f13592")
            .expect("parse should succeed");
        assert_eq!(uri.provider, ProviderKind::Codex);
        assert_eq!(uri.session_id, "019c871c-b1f9-7f60-9c4f-87ed09f13592");
        assert_eq!(uri.agent_id, None);
    }

    #[test]
    fn parse_agents_codex_deeplink_uri() {
        let uri = AgentsUri::parse("agents://codex/threads/019c871c-b1f9-7f60-9c4f-87ed09f13592")
            .expect("parse should succeed");
        assert_eq!(uri.provider, ProviderKind::Codex);
        assert_eq!(uri.session_id, "019c871c-b1f9-7f60-9c4f-87ed09f13592");
        assert_eq!(uri.agent_id, None);
    }

    #[test]
    fn parse_codex_subagent_uri() {
        let uri = AgentsUri::parse(
            "codex://019c871c-b1f9-7f60-9c4f-87ed09f13592/019c87fb-38b9-7843-92b1-832f02598495",
        )
        .expect("parse should succeed");
        assert_eq!(uri.provider, ProviderKind::Codex);
        assert_eq!(uri.session_id, "019c871c-b1f9-7f60-9c4f-87ed09f13592");
        assert_eq!(
            uri.agent_id,
            Some("019c87fb-38b9-7843-92b1-832f02598495".to_string())
        );
    }

    #[test]
    fn parse_agents_subagent_uri_without_agents_prefix() {
        let uri = AgentsUri::parse(
            "codex/019c871c-b1f9-7f60-9c4f-87ed09f13592/019c87fb-38b9-7843-92b1-832f02598495",
        )
        .expect("parse should succeed");
        assert_eq!(uri.provider, ProviderKind::Codex);
        assert_eq!(uri.session_id, "019c871c-b1f9-7f60-9c4f-87ed09f13592");
        assert_eq!(
            uri.agent_id,
            Some("019c87fb-38b9-7843-92b1-832f02598495".to_string())
        );
    }

    #[test]
    fn parse_agents_codex_subagent_uri() {
        let uri = AgentsUri::parse(
            "agents://codex/019c871c-b1f9-7f60-9c4f-87ed09f13592/019c87fb-38b9-7843-92b1-832f02598495",
        )
        .expect("parse should succeed");
        assert_eq!(uri.provider, ProviderKind::Codex);
        assert_eq!(uri.session_id, "019c871c-b1f9-7f60-9c4f-87ed09f13592");
        assert_eq!(
            uri.agent_id,
            Some("019c87fb-38b9-7843-92b1-832f02598495".to_string())
        );
    }

    #[test]
    fn parse_amp_subagent_uri() {
        let uri = AgentsUri::parse(
            "amp://T-019C0797-C402-7389-BD80-D785C98DF295/T-1ABC0797-C402-7389-BD80-D785C98DF295",
        )
        .expect("parse should succeed");
        assert_eq!(uri.provider, ProviderKind::Amp);
        assert_eq!(uri.session_id, "T-019c0797-c402-7389-bd80-d785c98df295");
        assert_eq!(
            uri.agent_id,
            Some("T-1abc0797-c402-7389-bd80-d785c98df295".to_string())
        );
    }

    #[test]
    fn parse_agents_amp_subagent_uri() {
        let uri = AgentsUri::parse(
            "agents://amp/T-019C0797-C402-7389-BD80-D785C98DF295/T-1ABC0797-C402-7389-BD80-D785C98DF295",
        )
        .expect("parse should succeed");
        assert_eq!(uri.provider, ProviderKind::Amp);
        assert_eq!(uri.session_id, "T-019c0797-c402-7389-bd80-d785c98df295");
        assert_eq!(
            uri.agent_id,
            Some("T-1abc0797-c402-7389-bd80-d785c98df295".to_string())
        );
    }

    #[test]
    fn parse_claude_subagent_uri() {
        let uri = AgentsUri::parse("claude://2823d1df-720a-4c31-ac55-ae8ba726721f/acompact-69d537")
            .expect("parse should succeed");
        assert_eq!(uri.provider, ProviderKind::Claude);
        assert_eq!(uri.session_id, "2823d1df-720a-4c31-ac55-ae8ba726721f");
        assert_eq!(uri.agent_id, Some("acompact-69d537".to_string()));
    }

    #[test]
    fn parse_rejects_extra_path_segments() {
        let err = AgentsUri::parse("codex://019c871c-b1f9-7f60-9c4f-87ed09f13592/a/b")
            .expect_err("must reject nested path");
        assert!(format!("{err}").contains("invalid uri"));
    }

    #[test]
    fn parse_rejects_invalid_child_id_for_amp() {
        let err = AgentsUri::parse("amp://T-019c0797-c402-7389-bd80-d785c98df295/child")
            .expect_err("must reject amp path segment");
        assert!(format!("{err}").contains("invalid session id"));
    }

    #[test]
    fn parse_rejects_extra_path_segments_for_amp() {
        let err = AgentsUri::parse(
            "amp://T-019c0797-c402-7389-bd80-d785c98df295/T-1abc0797-c402-7389-bd80-d785c98df295/extra",
        )
        .expect_err("must reject nested path");
        assert!(format!("{err}").contains("invalid uri"));
    }

    #[test]
    fn parse_rejects_unsupported_scheme() {
        let err = AgentsUri::parse("cursor://019c871c-b1f9-7f60-9c4f-87ed09f13592")
            .expect_err("must reject unsupported scheme");
        assert!(format!("{err}").contains("unsupported scheme"));
    }

    #[test]
    fn parse_rejects_invalid_agents_provider() {
        let err = AgentsUri::parse("agents://cursor/019c871c-b1f9-7f60-9c4f-87ed09f13592")
            .expect_err("must reject provider");
        assert!(format!("{err}").contains("unsupported scheme"));
    }

    #[test]
    fn parse_rejects_invalid_session_id_for_codex() {
        let err = AgentsUri::parse("codex://agent-a1b2c3").expect_err("must reject non-session id");
        assert!(format!("{err}").contains("invalid session id"));
    }

    #[test]
    fn parse_valid_opencode_uri() {
        let uri = AgentsUri::parse("opencode://ses_43a90e3adffejRgrTdlJa48CtE")
            .expect("parse should succeed");
        assert_eq!(uri.provider, ProviderKind::Opencode);
        assert_eq!(uri.session_id, "ses_43a90e3adffejRgrTdlJa48CtE");
        assert_eq!(uri.agent_id, None);
    }

    #[test]
    fn parse_valid_gemini_uri() {
        let uri = AgentsUri::parse("gemini://29D207DB-CA7E-40BA-87F7-E14C9DE60613")
            .expect("parse should succeed");
        assert_eq!(uri.provider, ProviderKind::Gemini);
        assert_eq!(uri.session_id, "29d207db-ca7e-40ba-87f7-e14c9de60613");
        assert_eq!(uri.agent_id, None);
    }

    #[test]
    fn parse_gemini_subagent_uri() {
        let uri = AgentsUri::parse(
            "gemini://29D207DB-CA7E-40BA-87F7-E14C9DE60613/2B112C8A-D80A-4CFF-9C8A-6F3E6FBAF7FB",
        )
        .expect("parse should succeed");
        assert_eq!(uri.provider, ProviderKind::Gemini);
        assert_eq!(uri.session_id, "29d207db-ca7e-40ba-87f7-e14c9de60613");
        assert_eq!(
            uri.agent_id,
            Some("2b112c8a-d80a-4cff-9c8a-6f3e6fbaf7fb".to_string())
        );
    }

    #[test]
    fn parse_agents_gemini_subagent_uri() {
        let uri = AgentsUri::parse(
            "agents://gemini/29d207db-ca7e-40ba-87f7-e14c9de60613/2b112c8a-d80a-4cff-9c8a-6f3e6fbaf7fb",
        )
        .expect("parse should succeed");
        assert_eq!(uri.provider, ProviderKind::Gemini);
        assert_eq!(uri.session_id, "29d207db-ca7e-40ba-87f7-e14c9de60613");
        assert_eq!(
            uri.agent_id,
            Some("2b112c8a-d80a-4cff-9c8a-6f3e6fbaf7fb".to_string())
        );
    }

    #[test]
    fn parse_valid_pi_uri() {
        let uri = AgentsUri::parse("pi://12CB4C19-2774-4DE4-A0D0-9FA32FBAE29F")
            .expect("parse should succeed");
        assert_eq!(uri.provider, ProviderKind::Pi);
        assert_eq!(uri.session_id, "12cb4c19-2774-4de4-a0d0-9fa32fbae29f");
        assert_eq!(uri.agent_id, None);
    }

    #[test]
    fn parse_valid_pi_entry_uri() {
        let uri = AgentsUri::parse("pi://12cb4c19-2774-4de4-a0d0-9fa32fbae29f/1C130174")
            .expect("parse should succeed");
        assert_eq!(uri.provider, ProviderKind::Pi);
        assert_eq!(uri.session_id, "12cb4c19-2774-4de4-a0d0-9fa32fbae29f");
        assert_eq!(uri.agent_id, Some("1c130174".to_string()));
    }

    #[test]
    fn parse_valid_pi_child_session_uri() {
        let uri = AgentsUri::parse(
            "pi://12cb4c19-2774-4de4-a0d0-9fa32fbae29f/72B3A4A8-4F08-40AF-8D7F-8B2C77584E89",
        )
        .expect("parse should succeed");
        assert_eq!(uri.provider, ProviderKind::Pi);
        assert_eq!(uri.session_id, "12cb4c19-2774-4de4-a0d0-9fa32fbae29f");
        assert_eq!(
            uri.agent_id,
            Some("72b3a4a8-4f08-40af-8d7f-8b2c77584e89".to_string())
        );
    }

    #[test]
    fn parse_rejects_nested_pi_path() {
        let err = AgentsUri::parse("pi://12cb4c19-2774-4de4-a0d0-9fa32fbae29f/a/b")
            .expect_err("must reject nested path");
        assert!(format!("{err}").contains("invalid uri"));
    }

    #[test]
    fn parse_collection_query_uri_with_defaults() {
        let query =
            parse_collection_query_uri("agents://codex").expect("collection query parse must work");
        let query = query.expect("query should be present");
        assert_eq!(query.provider, ProviderKind::Codex);
        assert_eq!(query.role, None);
        assert_eq!(query.q, None);
        assert_eq!(query.limit, 10);
        assert!(query.ignored_params.is_empty());
    }

    #[test]
    fn parse_collection_query_uri_with_q_and_limit() {
        let query = parse_collection_query_uri("agents://claude?q=spawn+agent&limit=7")
            .expect("collection query parse must work");
        let query = query.expect("query should be present");
        assert_eq!(query.provider, ProviderKind::Claude);
        assert_eq!(query.role, None);
        assert_eq!(query.q, Some("spawn agent".to_string()));
        assert_eq!(query.limit, 7);
    }

    #[test]
    fn parse_path_query_uri_with_absolute_path() {
        let query = parse_path_query_uri("agents:///tmp/project?q=spawn+agent&limit=7")
            .expect("path query parse must work");
        let query = query.expect("query should be present");
        assert_eq!(query.uri, "agents:///tmp/project?q=spawn+agent&limit=7");
        assert_eq!(query.scope_path, "/tmp/project");
        assert_eq!(query.q, Some("spawn agent".to_string()));
        assert_eq!(query.limit, 7);
        assert_eq!(query.providers, None);
    }

    #[test]
    fn parse_path_query_uri_with_relative_current_dir() {
        let temp = tempdir().expect("tempdir");
        let cwd = temp.path().join("workspace").join("subdir");
        std::fs::create_dir_all(&cwd).expect("mkdir");
        let old = env::current_dir().expect("cwd");
        env::set_current_dir(&cwd).expect("chdir");
        let actual_cwd = env::current_dir().expect("actual cwd");

        let query = parse_path_query_uri("agents://../repo?q=hello")
            .expect("path query parse must work")
            .expect("query should be present");

        env::set_current_dir(old).expect("restore cwd");
        assert_eq!(
            query.scope_path,
            actual_cwd
                .parent()
                .expect("parent")
                .join("repo")
                .display()
                .to_string()
        );
        assert_eq!(
            query.uri,
            format!(
                "agents://{}?q=hello",
                actual_cwd.parent().expect("parent").join("repo").display()
            )
        );
    }

    #[test]
    fn parse_path_query_uri_with_home_shorthand() {
        let home = home_dir().expect("home");
        let query = parse_path_query_uri("agents://~/work/xurl?providers=codex,claude")
            .expect("path query parse must work")
            .expect("query should be present");
        assert_eq!(
            query.scope_path,
            home.join("work/xurl").display().to_string()
        );
        assert_eq!(
            query.providers,
            Some(vec![ProviderKind::Codex, ProviderKind::Claude])
        );
    }

    #[test]
    fn parse_path_query_uri_returns_none_for_global_form() {
        let query = parse_path_query_uri("agents://?q=hello").expect("parse must succeed");
        assert_eq!(query, None);
    }

    #[test]
    fn parse_collection_query_uri_without_agents_prefix() {
        let query = parse_collection_query_uri("claude?q=spawn+agent&limit=7")
            .expect("collection query parse must work");
        let query = query.expect("query should be present");
        assert_eq!(query.provider, ProviderKind::Claude);
        assert_eq!(query.role, None);
        assert_eq!(query.q, Some("spawn agent".to_string()));
        assert_eq!(query.limit, 7);
    }

    #[test]
    fn parse_collection_query_uri_ignores_unknown_keys() {
        let query = parse_collection_query_uri("agents://pi?q=hello&foo=bar&foo=baz")
            .expect("collection query parse must work");
        let query = query.expect("query should be present");
        assert_eq!(query.provider, ProviderKind::Pi);
        assert_eq!(query.role, None);
        assert_eq!(query.ignored_params, vec!["foo".to_string()]);
    }

    #[test]
    fn parse_collection_query_uri_rejects_invalid_limit() {
        let err = parse_collection_query_uri("agents://gemini?limit=abc")
            .expect_err("invalid limit should fail");
        assert!(format!("{err}").contains("invalid uri"));
    }

    #[test]
    fn parse_collection_query_uri_is_none_for_thread_uri() {
        let query =
            parse_collection_query_uri("agents://codex/019c871c-b1f9-7f60-9c4f-87ed09f13592")
                .expect("parsing must succeed");
        assert_eq!(query, None);
    }

    #[test]
    fn parse_collection_query_uri_is_none_for_thread_uri_without_agents_prefix() {
        let query = parse_collection_query_uri("codex/019c871c-b1f9-7f60-9c4f-87ed09f13592")
            .expect("parsing must succeed");
        assert_eq!(query, None);
    }

    #[test]
    fn parse_role_uri_with_agents_prefix() {
        let role_uri = parse_role_uri("agents://codex/reviewer").expect("parse must succeed");
        let role_uri = role_uri.expect("role uri must exist");
        assert_eq!(role_uri.provider, ProviderKind::Codex);
        assert_eq!(role_uri.role, "reviewer");
    }

    #[test]
    fn parse_role_uri_without_agents_prefix() {
        let role_uri = parse_role_uri("codex/reviewer").expect("parse must succeed");
        let role_uri = role_uri.expect("role uri must exist");
        assert_eq!(role_uri.provider, ProviderKind::Codex);
        assert_eq!(role_uri.role, "reviewer");
    }

    #[test]
    fn parse_role_uri_returns_none_for_valid_session() {
        let role_uri = parse_role_uri("codex/019c871c-b1f9-7f60-9c4f-87ed09f13592")
            .expect("parse must succeed");
        assert_eq!(role_uri, None);
    }

    #[test]
    fn parse_role_query_uri_with_q_and_limit() {
        let query = parse_role_query_uri("agents://codex/reviewer?q=spawn+agent&limit=3")
            .expect("role query parse must succeed");
        let query = query.expect("query must exist");
        assert_eq!(query.provider, ProviderKind::Codex);
        assert_eq!(query.role, Some("reviewer".to_string()));
        assert_eq!(query.q, Some("spawn agent".to_string()));
        assert_eq!(query.limit, 3);
    }

    #[test]
    fn parse_role_query_uri_returns_none_for_collection() {
        let query = parse_role_query_uri("agents://codex").expect("parse must succeed");
        assert_eq!(query, None);
    }
}
