use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::Value;

use crate::error::{Result, XurlError};
use crate::jsonl;
use crate::model::{MessageRole, ProviderKind, ThreadMessage};
use crate::uri::AgentsUri;

const TOOL_TYPES: &[&str] = &[
    "tool_call",
    "tool_result",
    "tool_use",
    "function_call",
    "function_result",
    "function_response",
];
const COMPACT_PLACEHOLDER: &str = "Context was compacted.";

enum TimelineEntry {
    Message(ThreadMessage),
    Compact { summary: Option<String> },
}

pub fn render_markdown(uri: &AgentsUri, source_path: &Path, raw_jsonl: &str) -> Result<String> {
    let entries = extract_timeline_entries(
        uri.provider,
        source_path,
        raw_jsonl,
        &uri.session_id,
        uri.agent_id.as_deref(),
    )?;

    let mut output = String::new();
    let thread_uri = uri.as_agents_string();
    let source = source_path.to_string_lossy();
    output.push_str("---\n");
    output.push_str(&format!("uri: '{}'\n", yaml_single_quoted(&thread_uri)));
    output.push_str(&format!(
        "thread_source: '{}'\n",
        yaml_single_quoted(source.as_ref())
    ));
    output.push_str("---\n\n");
    output.push_str("# Thread\n\n");
    output.push_str("## Timeline\n\n");

    if entries.is_empty() {
        output.push_str("_No user/assistant messages or compact events found._\n");
        return Ok(output);
    }

    for (idx, entry) in entries.iter().enumerate() {
        let title = match entry {
            TimelineEntry::Message(message) => match message.role {
                MessageRole::User => "User",
                MessageRole::Assistant => "Assistant",
            },
            TimelineEntry::Compact { .. } => "Context Compacted",
        };

        output.push_str(&format!("## {}. {}\n\n", idx + 1, title));
        match entry {
            TimelineEntry::Message(message) => output.push_str(message.text.trim()),
            TimelineEntry::Compact { summary } => {
                let summary = summary.as_deref().unwrap_or(COMPACT_PLACEHOLDER);
                output.push_str(summary.trim());
            }
        }
        output.push_str("\n\n");
    }

    Ok(output)
}

fn yaml_single_quoted(value: &str) -> String {
    value.replace('\'', "''")
}

pub fn extract_messages(
    provider: ProviderKind,
    path: &Path,
    raw_jsonl: &str,
) -> Result<Vec<ThreadMessage>> {
    Ok(
        extract_timeline_entries(provider, path, raw_jsonl, "", None)?
            .into_iter()
            .filter_map(|entry| match entry {
                TimelineEntry::Message(message) => Some(message),
                TimelineEntry::Compact { .. } => None,
            })
            .collect(),
    )
}

fn extract_timeline_entries(
    provider: ProviderKind,
    path: &Path,
    raw_jsonl: &str,
    session_id: &str,
    target_entry_id: Option<&str>,
) -> Result<Vec<TimelineEntry>> {
    if provider == ProviderKind::Amp {
        return Ok(messages_to_entries(extract_amp_messages(path, raw_jsonl)?));
    }
    if provider == ProviderKind::Gemini {
        return Ok(messages_to_entries(extract_gemini_messages(
            path, raw_jsonl,
        )?));
    }
    if provider == ProviderKind::Pi {
        return extract_pi_entries(path, raw_jsonl, session_id, target_entry_id);
    }

    let mut entries = Vec::new();

    for (line_idx, line) in raw_jsonl.lines().enumerate() {
        let line_no = line_idx + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Some(value) = jsonl::parse_json_line(path, line_no, trimmed)? else {
            continue;
        };

        let extracted = match provider {
            ProviderKind::Amp => None,
            ProviderKind::Codex => extract_codex_entry(&value),
            ProviderKind::Claude => extract_claude_entry(&value),
            ProviderKind::Gemini => None,
            ProviderKind::Pi => None,
            ProviderKind::Opencode => extract_opencode_message(&value).map(TimelineEntry::Message),
            ProviderKind::Openclaw => extract_opencode_message(&value).map(TimelineEntry::Message),
        };

        if let Some(entry) = extracted {
            entries.push(entry);
        }
    }

    Ok(entries)
}

fn messages_to_entries(messages: Vec<ThreadMessage>) -> Vec<TimelineEntry> {
    messages.into_iter().map(TimelineEntry::Message).collect()
}

fn extract_pi_entries(
    path: &Path,
    raw_jsonl: &str,
    session_id: &str,
    target_entry_id: Option<&str>,
) -> Result<Vec<TimelineEntry>> {
    let mut entries_by_id = HashMap::<String, Value>::new();
    let mut last_entry_id = None::<String>;

    for (line_idx, line) in raw_jsonl.lines().enumerate() {
        let line_no = line_idx + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Some(value) = jsonl::parse_json_line(path, line_no, trimmed)? else {
            continue;
        };

        if value.get("type").and_then(Value::as_str) == Some("session") {
            continue;
        }

        let Some(id) = value
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase)
        else {
            continue;
        };

        last_entry_id = Some(id.clone());
        entries_by_id.insert(id, value);
    }

    if entries_by_id.is_empty() {
        return Ok(Vec::new());
    }

    let leaf_id = target_entry_id
        .map(str::to_ascii_lowercase)
        .or(last_entry_id)
        .unwrap_or_default();

    if !entries_by_id.contains_key(&leaf_id) {
        return Err(XurlError::EntryNotFound {
            provider: ProviderKind::Pi.to_string(),
            session_id: session_id.to_string(),
            entry_id: leaf_id,
        });
    }

    let mut path_ids = Vec::new();
    let mut seen = HashSet::new();
    let mut current = Some(leaf_id);

    while let Some(entry_id) = current {
        if !seen.insert(entry_id.clone()) {
            break;
        }

        let Some(entry) = entries_by_id.get(&entry_id) else {
            break;
        };
        path_ids.push(entry_id);

        current = entry
            .get("parentId")
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase);
    }

    path_ids.reverse();

    let mut entries = Vec::new();
    for entry_id in path_ids {
        let Some(entry) = entries_by_id.get(&entry_id) else {
            continue;
        };
        if let Some(timeline_entry) = extract_pi_entry(entry) {
            entries.push(timeline_entry);
        }
    }

    Ok(entries)
}

fn extract_pi_entry(value: &Value) -> Option<TimelineEntry> {
    let entry_type = value.get("type").and_then(Value::as_str)?;

    if entry_type == "message" {
        let message = value.get("message")?;
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .and_then(parse_role)?;
        let text = extract_text(message.get("content"));
        if text.trim().is_empty() {
            return None;
        }

        return Some(TimelineEntry::Message(ThreadMessage { role, text }));
    }

    if entry_type == "compaction" || entry_type == "branch_summary" {
        let summary = value
            .get("summary")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        return Some(TimelineEntry::Compact { summary });
    }

    None
}

fn extract_amp_messages(path: &Path, raw_json: &str) -> Result<Vec<ThreadMessage>> {
    let value =
        serde_json::from_str::<Value>(raw_json).map_err(|source| XurlError::InvalidJsonLine {
            path: path.to_path_buf(),
            line: 1,
            source,
        })?;

    let mut messages = Vec::new();
    for message in value
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(role) = message
            .get("role")
            .and_then(Value::as_str)
            .and_then(parse_role)
        else {
            continue;
        };

        let text = extract_amp_text(message.get("content"));
        if text.trim().is_empty() {
            continue;
        }

        messages.push(ThreadMessage { role, text });
    }

    Ok(messages)
}

fn extract_gemini_messages(path: &Path, raw_json: &str) -> Result<Vec<ThreadMessage>> {
    let value =
        serde_json::from_str::<Value>(raw_json).map_err(|source| XurlError::InvalidJsonLine {
            path: path.to_path_buf(),
            line: 1,
            source,
        })?;

    let mut messages = Vec::new();
    for message in value
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(role) = message
            .get("type")
            .and_then(Value::as_str)
            .and_then(parse_gemini_role)
        else {
            continue;
        };

        let text = extract_text(message.get("displayContent"));
        let text = if text.trim().is_empty() {
            extract_text(message.get("content"))
        } else {
            text
        };

        if text.trim().is_empty() {
            continue;
        }

        messages.push(ThreadMessage { role, text });
    }

    Ok(messages)
}

fn extract_codex_message(value: &Value) -> Option<ThreadMessage> {
    let record_type = value.get("type").and_then(Value::as_str)?;

    if record_type == "response_item" {
        let payload = value.get("payload")?;
        let payload_type = payload.get("type").and_then(Value::as_str)?;
        if payload_type != "message" {
            return None;
        }

        let role = payload.get("role").and_then(Value::as_str)?;
        let role = parse_role(role)?;
        let text = extract_text(payload.get("content"));
        if text.trim().is_empty() {
            return None;
        }

        return Some(ThreadMessage { role, text });
    }

    if record_type == "event_msg"
        && value
            .get("payload")
            .and_then(|payload| payload.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|t| t == "agent_message")
    {
        let text = value
            .get("payload")
            .and_then(|payload| payload.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        if text.trim().is_empty() {
            return None;
        }

        return Some(ThreadMessage {
            role: MessageRole::Assistant,
            text,
        });
    }

    None
}

fn extract_codex_entry(value: &Value) -> Option<TimelineEntry> {
    if let Some(message) = extract_codex_message(value) {
        return Some(TimelineEntry::Message(message));
    }

    if is_codex_compact_event(value) {
        return Some(TimelineEntry::Compact { summary: None });
    }

    None
}

fn is_codex_compact_event(value: &Value) -> bool {
    let record_type = value.get("type").and_then(Value::as_str);

    if record_type == Some("compacted") {
        return true;
    }

    record_type == Some("event_msg")
        && value
            .get("payload")
            .and_then(|payload| payload.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|payload_type| payload_type == "context_compacted")
}

fn extract_claude_message(value: &Value) -> Option<ThreadMessage> {
    let record_type = value.get("type").and_then(Value::as_str)?;
    if record_type != "user" && record_type != "assistant" {
        return None;
    }

    let message = value.get("message")?;
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .or(Some(record_type))?;
    let role = parse_role(role)?;

    let text = extract_text(message.get("content"));
    if text.trim().is_empty() {
        return None;
    }

    Some(ThreadMessage { role, text })
}

fn extract_claude_entry(value: &Value) -> Option<TimelineEntry> {
    if is_claude_compact_boundary(value) {
        return Some(TimelineEntry::Compact { summary: None });
    }

    if is_claude_compact_summary(value) {
        let summary = extract_claude_message(value).map(|message| message.text);
        return Some(TimelineEntry::Compact { summary });
    }

    extract_claude_message(value).map(TimelineEntry::Message)
}

fn is_claude_compact_boundary(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("system")
        && value.get("subtype").and_then(Value::as_str) == Some("compact_boundary")
}

fn is_claude_compact_summary(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("user")
        && value
            .get("isCompactSummary")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn extract_opencode_message(value: &Value) -> Option<ThreadMessage> {
    let record_type = value.get("type").and_then(Value::as_str)?;
    if record_type != "message" {
        return None;
    }

    let message = value.get("message")?;
    let role = message.get("role").and_then(Value::as_str)?;
    let role = parse_role(role)?;

    let mut chunks = Vec::new();
    for part in value
        .get("parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(part_type) = part.get("type").and_then(Value::as_str) else {
            continue;
        };

        if part_type != "text" && part_type != "reasoning" {
            continue;
        }

        if let Some(text) = part.get("text").and_then(Value::as_str)
            && !text.trim().is_empty()
        {
            chunks.push(text.trim().to_string());
        }
    }

    if chunks.is_empty() {
        return None;
    }

    Some(ThreadMessage {
        role,
        text: chunks.join("\n\n"),
    })
}

fn extract_amp_text(content: Option<&Value>) -> String {
    let Some(items) = content.and_then(Value::as_array) else {
        return String::new();
    };

    let mut chunks = Vec::new();
    for item in items {
        let Some(item_type) = item.get("type").and_then(Value::as_str) else {
            continue;
        };

        match item_type {
            "text" => {
                if let Some(text) = item.get("text").and_then(Value::as_str)
                    && !text.trim().is_empty()
                {
                    chunks.push(text.trim().to_string());
                }
            }
            "thinking" => {
                if let Some(thinking) = item.get("thinking").and_then(Value::as_str)
                    && !thinking.trim().is_empty()
                {
                    chunks.push(thinking.trim().to_string());
                }
            }
            _ => {}
        }
    }

    chunks.join("\n\n")
}

fn parse_role(role: &str) -> Option<MessageRole> {
    match role {
        "user" => Some(MessageRole::User),
        "assistant" => Some(MessageRole::Assistant),
        _ => None,
    }
}

fn parse_gemini_role(role: &str) -> Option<MessageRole> {
    match role {
        "user" => Some(MessageRole::User),
        "gemini" => Some(MessageRole::Assistant),
        _ => None,
    }
}

fn extract_text(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };

    if let Some(text) = content.as_str() {
        return text.to_string();
    }

    let Some(items) = content.as_array() else {
        return String::new();
    };

    let mut chunks = Vec::new();

    for item in items {
        if let Some(text) = item.as_str()
            && !text.trim().is_empty()
        {
            chunks.push(text.trim().to_string());
            continue;
        }

        if let Some(item_type) = item.get("type").and_then(Value::as_str)
            && TOOL_TYPES.contains(&item_type)
        {
            continue;
        }

        if let Some(text) = item.get("text").and_then(Value::as_str)
            && !text.trim().is_empty()
        {
            chunks.push(text.trim().to_string());
            continue;
        }

        if let Some(text) = item.get("input_text").and_then(Value::as_str)
            && !text.trim().is_empty()
        {
            chunks.push(text.trim().to_string());
            continue;
        }

        if let Some(text) = item.get("output_text").and_then(Value::as_str)
            && !text.trim().is_empty()
        {
            chunks.push(text.trim().to_string());
        }
    }

    chunks.join("\n\n")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::model::ProviderKind;
    use crate::render::{extract_messages, render_markdown};
    use crate::uri::AgentsUri;

    #[test]
    fn render_outputs_frontmatter() {
        let raw = r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}}"#;
        let uri =
            AgentsUri::parse("codex://019c871c-b1f9-7f60-9c4f-87ed09f13592").expect("parse uri");
        let output = render_markdown(&uri, Path::new("/tmp/mock"), raw).expect("render");

        assert!(output.starts_with("---\n"));
        assert!(output.contains("uri: 'agents://codex/019c871c-b1f9-7f60-9c4f-87ed09f13592'"));
        assert!(output.contains("thread_source: '/tmp/mock'"));
        assert!(output.contains("## Timeline"));
    }

    #[test]
    fn codex_filters_function_calls() {
        let raw = r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}}
{"type":"response_item","payload":{"type":"function_call","name":"ls"}}
{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"world"}]}}"#;

        let messages =
            extract_messages(ProviderKind::Codex, Path::new("/tmp/mock"), raw).expect("extract");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].text, "hello");
        assert_eq!(messages[1].text, "world");
    }

    #[test]
    fn claude_filters_tool_use() {
        let raw = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"search"},{"type":"text","text":"done"}]}}"#;

        let messages =
            extract_messages(ProviderKind::Claude, Path::new("/tmp/mock"), raw).expect("extract");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].text, "done");
    }

    #[test]
    fn opencode_extracts_text_and_reasoning_parts() {
        let raw = r#"{"type":"session","sessionId":"ses_43a90e3adffejRgrTdlJa48CtE"}
{"type":"message","id":"msg_1","sessionId":"ses_43a90e3adffejRgrTdlJa48CtE","message":{"role":"user","time":{"created":1}},"parts":[{"type":"text","text":"hello"}]}
{"type":"message","id":"msg_2","sessionId":"ses_43a90e3adffejRgrTdlJa48CtE","message":{"role":"assistant","time":{"created":2}},"parts":[{"type":"reasoning","text":"thinking"},{"type":"tool","tool":"read"},{"type":"text","text":"world"}]}"#;

        let messages =
            extract_messages(ProviderKind::Opencode, Path::new("/tmp/mock"), raw).expect("extract");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].text, "hello");
        assert_eq!(messages[1].text, "thinking\n\nworld");
    }

    #[test]
    fn amp_extracts_text_and_thinking_content() {
        let raw = r#"{"id":"T-019c0797-c402-7389-bd80-d785c98df295","messages":[{"role":"user","content":[{"type":"text","text":"hello"}]},{"role":"assistant","content":[{"type":"thinking","thinking":"step by step"},{"type":"tool_use","name":"finder"},{"type":"text","text":"done"}]},{"role":"user","content":[{"type":"tool_result","toolUseID":"tool_1","run":{"status":"done","result":"ignored"}}]}]}"#;

        let messages =
            extract_messages(ProviderKind::Amp, Path::new("/tmp/mock"), raw).expect("extract");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].text, "hello");
        assert_eq!(messages[1].text, "step by step\n\ndone");
    }

    #[test]
    fn gemini_extracts_user_and_assistant_messages() {
        let raw = r#"{"sessionId":"29d207db-ca7e-40ba-87f7-e14c9de60613","messages":[{"type":"info","content":"ignored"},{"type":"user","content":"hello"},{"type":"gemini","content":"world"},{"type":"gemini","content":[{"type":"thinking","text":"step by step"},{"type":"tool_call","name":"list_directory"},{"type":"text","text":"done"}]}]}"#;

        let messages =
            extract_messages(ProviderKind::Gemini, Path::new("/tmp/mock"), raw).expect("extract");
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].text, "hello");
        assert_eq!(messages[1].text, "world");
        assert_eq!(messages[2].text, "step by step\n\ndone");
    }

    #[test]
    fn pi_default_leaf_renders_latest_branch() {
        let raw = r#"{"type":"session","version":3,"id":"12cb4c19-2774-4de4-a0d0-9fa32fbae29f","timestamp":"2026-02-23T13:00:12.780Z","cwd":"/tmp/project"}
{"type":"message","id":"a1b2c3d4","parentId":null,"timestamp":"2026-02-23T13:00:13.000Z","message":{"role":"user","content":[{"type":"text","text":"root"}]}}
{"type":"message","id":"b1b2c3d4","parentId":"a1b2c3d4","timestamp":"2026-02-23T13:00:14.000Z","message":{"role":"assistant","content":[{"type":"text","text":"root done"}]}}
{"type":"message","id":"c1b2c3d4","parentId":"b1b2c3d4","timestamp":"2026-02-23T13:00:15.000Z","message":{"role":"user","content":[{"type":"text","text":"branch one"}]}}
{"type":"message","id":"d1b2c3d4","parentId":"c1b2c3d4","timestamp":"2026-02-23T13:00:16.000Z","message":{"role":"assistant","content":[{"type":"text","text":"branch one done"}]}}
{"type":"message","id":"e1b2c3d4","parentId":"b1b2c3d4","timestamp":"2026-02-23T13:00:17.000Z","message":{"role":"user","content":[{"type":"text","text":"branch two"}]}}
{"type":"compaction","id":"f1b2c3d4","parentId":"e1b2c3d4","timestamp":"2026-02-23T13:00:18.000Z","summary":"compact summary","firstKeptEntryId":"b1b2c3d4","tokensBefore":128}
{"type":"message","id":"g1b2c3d4","parentId":"f1b2c3d4","timestamp":"2026-02-23T13:00:19.000Z","message":{"role":"assistant","content":[{"type":"text","text":"branch two done"}]}}"#;

        let uri = AgentsUri::parse("pi://12cb4c19-2774-4de4-a0d0-9fa32fbae29f").expect("parse uri");
        let output = render_markdown(&uri, Path::new("/tmp/mock"), raw).expect("render");

        assert!(output.contains("root"));
        assert!(output.contains("branch two"));
        assert!(output.contains("compact summary"));
        assert!(!output.contains("branch one done"));
    }

    #[test]
    fn pi_entry_leaf_renders_requested_branch() {
        let raw = r#"{"type":"session","version":3,"id":"12cb4c19-2774-4de4-a0d0-9fa32fbae29f","timestamp":"2026-02-23T13:00:12.780Z","cwd":"/tmp/project"}
{"type":"message","id":"a1b2c3d4","parentId":null,"timestamp":"2026-02-23T13:00:13.000Z","message":{"role":"user","content":[{"type":"text","text":"root"}]}}
{"type":"message","id":"b1b2c3d4","parentId":"a1b2c3d4","timestamp":"2026-02-23T13:00:14.000Z","message":{"role":"assistant","content":[{"type":"text","text":"root done"}]}}
{"type":"message","id":"c1b2c3d4","parentId":"b1b2c3d4","timestamp":"2026-02-23T13:00:15.000Z","message":{"role":"user","content":[{"type":"text","text":"branch one"}]}}
{"type":"message","id":"d1b2c3d4","parentId":"c1b2c3d4","timestamp":"2026-02-23T13:00:16.000Z","message":{"role":"assistant","content":[{"type":"text","text":"branch one done"}]}}
{"type":"message","id":"e1b2c3d4","parentId":"b1b2c3d4","timestamp":"2026-02-23T13:00:17.000Z","message":{"role":"user","content":[{"type":"text","text":"branch two"}]}}
{"type":"message","id":"f1b2c3d4","parentId":"e1b2c3d4","timestamp":"2026-02-23T13:00:18.000Z","message":{"role":"assistant","content":[{"type":"text","text":"branch two done"}]}}"#;

        let uri = AgentsUri::parse("pi://12cb4c19-2774-4de4-a0d0-9fa32fbae29f/d1b2c3d4")
            .expect("parse uri");
        let output = render_markdown(&uri, Path::new("/tmp/mock"), raw).expect("render");

        assert!(output.contains("branch one done"));
        assert!(!output.contains("branch two done"));
    }

    #[test]
    fn pi_entry_leaf_renders_requested_branch_with_uppercase_ids() {
        let raw = r#"{"type":"session","version":3,"id":"12cb4c19-2774-4de4-a0d0-9fa32fbae29f","timestamp":"2026-02-23T13:00:12.780Z","cwd":"/tmp/project"}
{"type":"message","id":"A1B2C3D4","parentId":null,"timestamp":"2026-02-23T13:00:13.000Z","message":{"role":"user","content":[{"type":"text","text":"root"}]}}
{"type":"message","id":"B1B2C3D4","parentId":"A1B2C3D4","timestamp":"2026-02-23T13:00:14.000Z","message":{"role":"assistant","content":[{"type":"text","text":"root done"}]}}
{"type":"message","id":"C1B2C3D4","parentId":"B1B2C3D4","timestamp":"2026-02-23T13:00:15.000Z","message":{"role":"user","content":[{"type":"text","text":"branch one"}]}}
{"type":"message","id":"D1B2C3D4","parentId":"C1B2C3D4","timestamp":"2026-02-23T13:00:16.000Z","message":{"role":"assistant","content":[{"type":"text","text":"branch one done"}]}}
{"type":"message","id":"E1B2C3D4","parentId":"B1B2C3D4","timestamp":"2026-02-23T13:00:17.000Z","message":{"role":"user","content":[{"type":"text","text":"branch two"}]}}
{"type":"message","id":"F1B2C3D4","parentId":"E1B2C3D4","timestamp":"2026-02-23T13:00:18.000Z","message":{"role":"assistant","content":[{"type":"text","text":"branch two done"}]}}"#;

        let uri = AgentsUri::parse("pi://12cb4c19-2774-4de4-a0d0-9fa32fbae29f/d1b2c3d4")
            .expect("parse uri");
        let output = render_markdown(&uri, Path::new("/tmp/mock"), raw).expect("render");

        assert!(output.contains("branch one done"));
        assert!(!output.contains("branch two done"));
    }

    #[test]
    fn pi_entry_leaf_reports_not_found() {
        let raw = r#"{"type":"session","version":3,"id":"12cb4c19-2774-4de4-a0d0-9fa32fbae29f","timestamp":"2026-02-23T13:00:12.780Z","cwd":"/tmp/project"}
{"type":"message","id":"a1b2c3d4","parentId":null,"timestamp":"2026-02-23T13:00:13.000Z","message":{"role":"user","content":[{"type":"text","text":"root"}]}}"#;

        let uri = AgentsUri::parse("pi://12cb4c19-2774-4de4-a0d0-9fa32fbae29f/deadbeef")
            .expect("parse uri");
        let err = render_markdown(&uri, Path::new("/tmp/mock"), raw).expect_err("must fail");
        assert!(format!("{err}").contains("entry not found"));
    }

    #[test]
    fn codex_renders_compact_events_in_timeline() {
        let raw = r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}}
{"type":"event_msg","payload":{"type":"context_compacted"}}
{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"world"}]}}"#;

        let uri =
            AgentsUri::parse("codex://019c871c-b1f9-7f60-9c4f-87ed09f13592").expect("parse uri");
        let output = render_markdown(&uri, Path::new("/tmp/mock"), raw).expect("render");

        assert!(output.contains("## 1. User"));
        assert!(output.contains("## 2. Context Compacted"));
        assert!(output.contains("Context was compacted."));
        assert!(output.contains("## 3. Assistant"));
    }

    #[test]
    fn claude_compact_summary_renders_as_compact_entry() {
        let raw = r#"{"type":"user","isCompactSummary":true,"message":{"role":"user","content":[{"type":"text","text":"Summary: old conversation"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"New answer"}]}}"#;

        let uri =
            AgentsUri::parse("claude://2823d1df-720a-4c31-ac55-ae8ba726721f").expect("parse uri");
        let output = render_markdown(&uri, Path::new("/tmp/mock"), raw).expect("render");

        assert!(output.contains("## 1. Context Compacted"));
        assert!(output.contains("Summary: old conversation"));
        assert!(!output.contains("## 1. User"));
        assert!(output.contains("## 2. Assistant"));
    }
}
