use std::fs;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::{env, os::unix::fs::PermissionsExt};

use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::{Connection, params};
use tempfile::tempdir;

const SESSION_ID: &str = "019c871c-b1f9-7f60-9c4f-87ed09f13592";
const SUBAGENT_ID: &str = "019c87fb-38b9-7843-92b1-832f02598495";
const REAL_FIXTURE_MAIN_ID: &str = "55fe4488-c6bd-46fa-9390-dab3b8860b95";
const REAL_FIXTURE_AGENT_ID: &str = "29bf19c3-b83e-401d-8f38-5660b7f67152";
const AMP_SESSION_ID: &str = "T-019c0797-c402-7389-bd80-d785c98df295";
const AMP_SUBAGENT_ID: &str = "T-1abc0797-c402-7389-bd80-d785c98df295";
const GEMINI_SESSION_ID: &str = "29d207db-ca7e-40ba-87f7-e14c9de60613";
const GEMINI_CHILD_SESSION_ID: &str = "2b112c8a-d80a-4cff-9c8a-6f3e6fbaf7fb";
const GEMINI_MISSING_CHILD_SESSION_ID: &str = "62f9f98d-c578-4d3a-b4bf-3aaed19889d6";
const GEMINI_REAL_SESSION_ID: &str = "da2ab190-85f8-4d5c-bcce-8292921a33bf";
const PI_SESSION_ID: &str = "12cb4c19-2774-4de4-a0d0-9fa32fbae29f";
const PI_ENTRY_ID: &str = "d1b2c3d4";
const PI_CHILD_SESSION_ID: &str = "72b3a4a8-4f08-40af-8d7f-8b2c77584e89";
const PI_MISSING_CHILD_SESSION_ID: &str = "b200f2f0-5291-4b89-a1e7-7c6a95f11011";
const PI_REAL_SESSION_ID: &str = "bc6ea3d9-0e40-4942-a490-3e0aa7f125de";
const CLAUDE_SESSION_ID: &str = "2823d1df-720a-4c31-ac55-ae8ba726721f";
const CLAUDE_AGENT_ID: &str = "acompact-69d537";
const CLAUDE_REAL_MAIN_ID: &str = "b90fc33d-33cb-4027-8558-119e2b56c74e";
const CLAUDE_REAL_AGENT_ID: &str = "a4f21c7";
const OPENCODE_REAL_SESSION_ID: &str = "ses_7v2md9kx3c1p";
const OPENCODE_MAIN_SESSION_ID: &str = "ses_5x7md9kx3c1p";
const OPENCODE_CHILD_SESSION_ID: &str = "ses_5x7md9kx3c2p";
const OPENCODE_CHILD_EMPTY_SESSION_ID: &str = "ses_5x7md9kx3c3p";

fn setup_codex_tree() -> tempfile::TempDir {
    let temp = tempdir().expect("tempdir");
    let thread_path = temp.path().join(format!(
        "sessions/2026/02/23/rollout-2026-02-23T04-48-50-{SESSION_ID}.jsonl"
    ));
    fs::create_dir_all(thread_path.parent().expect("parent")).expect("mkdir");
    fs::write(
        &thread_path,
        "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hello\"}]}}\n{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"world\"}]}}\n",
    )
    .expect("write");

    temp
}

fn setup_codex_tree_with_sqlite_missing_threads() -> tempfile::TempDir {
    let temp = setup_codex_tree();
    fs::write(temp.path().join("state.sqlite"), "").expect("write sqlite");
    temp
}

fn setup_codex_role_query_tree() -> tempfile::TempDir {
    let temp = tempdir().expect("tempdir");
    let thread_path = temp.path().join(format!(
        "sessions/2026/02/23/rollout-2026-02-23T04-48-50-{SESSION_ID}.jsonl"
    ));
    fs::create_dir_all(thread_path.parent().expect("parent")).expect("mkdir");
    fs::write(
        &thread_path,
        "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"run reviewer role\"}]}}\n{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"reviewer done\"}]}}\n",
    )
    .expect("write");
    temp
}

fn setup_codex_role_configs(root: &Path) {
    fs::write(
        root.join("config.toml"),
        r#"
[agents.reviewer]
description = "Find issues."
config_file = "agents/reviewer.toml"
model_reasoning_effort = "high"
developer_instructions = "Focus on high priority issues."
"#,
    )
    .expect("write config");

    let role_dir = root.join("agents");
    fs::create_dir_all(&role_dir).expect("mkdir");
    fs::write(
        role_dir.join("reviewer.toml"),
        r#"
model = "gpt-5.3-codex"
"#,
    )
    .expect("write role config");
}

fn setup_amp_tree() -> tempfile::TempDir {
    let temp = tempdir().expect("tempdir");
    let thread_path = temp
        .path()
        .join(format!("amp/threads/{AMP_SESSION_ID}.json"));
    fs::create_dir_all(thread_path.parent().expect("parent")).expect("mkdir");
    fs::write(
        &thread_path,
        r#"{"id":"T-019c0797-c402-7389-bd80-d785c98df295","messages":[{"role":"user","content":[{"type":"text","text":"hello"}]},{"role":"assistant","content":[{"type":"thinking","thinking":"analyze"},{"type":"text","text":"world"}]}]}"#,
    )
    .expect("write");
    temp
}

fn setup_amp_subagent_tree_with_role(main_role: Option<&str>) -> tempfile::TempDir {
    let temp = tempdir().expect("tempdir");
    let main_path = temp
        .path()
        .join(format!("amp/threads/{AMP_SESSION_ID}.json"));
    fs::create_dir_all(main_path.parent().expect("parent")).expect("mkdir");
    let role_field = main_role
        .map(|role| format!(r#","role":"{role}""#))
        .unwrap_or_default();
    fs::write(
        &main_path,
        format!(
            r#"{{"id":"{AMP_SESSION_ID}","status":"running","updatedAt":"2026-02-23T00:00:03Z","messages":[{{"role":"user","timestamp":"2026-02-23T00:00:00Z","content":[{{"type":"text","text":"main task"}}]}}],"relationships":[{{"type":"handoff","threadID":"{AMP_SUBAGENT_ID}"{role_field},"timestamp":"2026-02-23T00:00:02Z"}}]}}"#
        ),
    )
    .expect("write main");

    let child_path = temp
        .path()
        .join(format!("amp/threads/{AMP_SUBAGENT_ID}.json"));
    fs::create_dir_all(child_path.parent().expect("parent")).expect("mkdir");
    fs::write(
        &child_path,
        format!(
            r#"{{"id":"{AMP_SUBAGENT_ID}","status":"completed","lastUpdated":"2026-02-23T00:00:14Z","messages":[{{"role":"user","timestamp":"2026-02-23T00:00:11Z","content":[{{"type":"text","text":"hello child"}}]}},{{"role":"assistant","timestamp":"2026-02-23T00:00:12Z","content":[{{"type":"text","text":"done child"}}]}}],"relationships":[{{"type":"handoff","threadID":"{AMP_SESSION_ID}","role":"child","timestamp":"2026-02-23T00:00:12Z"}}]}}"#
        ),
    )
    .expect("write child");

    temp
}

fn setup_amp_subagent_tree() -> tempfile::TempDir {
    setup_amp_subagent_tree_with_role(Some("parent"))
}

fn setup_amp_subagent_tree_missing_role() -> tempfile::TempDir {
    setup_amp_subagent_tree_with_role(None)
}

fn setup_codex_subagent_tree() -> tempfile::TempDir {
    let temp = tempdir().expect("tempdir");
    let main_thread_path = temp.path().join(format!(
        "sessions/2026/02/23/rollout-2026-02-23T04-48-50-{SESSION_ID}.jsonl"
    ));
    fs::create_dir_all(main_thread_path.parent().expect("parent")).expect("mkdir");
    fs::write(
        &main_thread_path,
        format!(
            "{{\"timestamp\":\"2026-02-23T00:00:00Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"function_call\",\"name\":\"spawn_agent\",\"arguments\":\"{{}}\",\"call_id\":\"call_spawn\"}}}}\n{{\"timestamp\":\"2026-02-23T00:00:01Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"function_call_output\",\"call_id\":\"call_spawn\",\"output\":\"{{\\\"agent_id\\\":\\\"{SUBAGENT_ID}\\\"}}\"}}}}\n{{\"timestamp\":\"2026-02-23T00:00:02Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"function_call\",\"name\":\"wait\",\"arguments\":\"{{\\\"ids\\\":[\\\"{SUBAGENT_ID}\\\"],\\\"timeout_ms\\\":120000}}\",\"call_id\":\"call_wait\"}}}}\n{{\"timestamp\":\"2026-02-23T00:00:03Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"function_call_output\",\"call_id\":\"call_wait\",\"output\":\"{{\\\"status\\\":{{\\\"running\\\":\\\"in progress\\\"}},\\\"timed_out\\\":false}}\"}}}}\n{{\"timestamp\":\"2026-02-23T00:00:04Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"function_call\",\"name\":\"close_agent\",\"arguments\":\"{{\\\"id\\\":\\\"{SUBAGENT_ID}\\\"}}\",\"call_id\":\"call_close\"}}}}\n{{\"timestamp\":\"2026-02-23T00:00:05Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"function_call_output\",\"call_id\":\"call_close\",\"output\":\"{{\\\"status\\\":{{\\\"completed\\\":\\\"done\\\"}}}}\"}}}}\n"
        ),
    )
    .expect("write main");

    let child_thread_path = temp.path().join(format!(
        "sessions/2026/02/23/rollout-2026-02-23T04-49-10-{SUBAGENT_ID}.jsonl"
    ));
    fs::create_dir_all(child_thread_path.parent().expect("parent")).expect("mkdir");
    fs::write(
        &child_thread_path,
        format!(
            "{{\"timestamp\":\"2026-02-23T00:00:10Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{SUBAGENT_ID}\",\"source\":{{\"subagent\":{{\"thread_spawn\":{{\"parent_thread_id\":\"{SESSION_ID}\",\"depth\":1}}}}}}}}}}\n{{\"timestamp\":\"2026-02-23T00:00:11Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"hello child\"}}]}}}}\n{{\"timestamp\":\"2026-02-23T00:00:12Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"done child\"}}]}}}}\n"
        ),
    )
    .expect("write child");

    temp
}

fn setup_codex_subagent_tree_with_sqlite_missing_threads() -> tempfile::TempDir {
    let temp = setup_codex_subagent_tree();
    fs::write(temp.path().join("state.sqlite"), "").expect("write sqlite");
    temp
}

fn setup_claude_subagent_tree() -> tempfile::TempDir {
    let temp = tempdir().expect("tempdir");
    let project = temp.path().join("projects/project-subagent");
    fs::create_dir_all(&project).expect("mkdir");

    let main_thread = project.join(format!("{CLAUDE_SESSION_ID}.jsonl"));
    fs::write(
        &main_thread,
        format!(
            "{{\"timestamp\":\"2026-02-23T00:00:00Z\",\"type\":\"user\",\"sessionId\":\"{CLAUDE_SESSION_ID}\",\"message\":{{\"role\":\"user\",\"content\":\"root thread\"}}}}\n"
        ),
    )
    .expect("write main");

    let subagents_dir = project.join(CLAUDE_SESSION_ID).join("subagents");
    fs::create_dir_all(&subagents_dir).expect("mkdir");
    let agent_thread = subagents_dir.join(format!("agent-{CLAUDE_AGENT_ID}.jsonl"));
    fs::write(
        &agent_thread,
        format!(
            "{{\"timestamp\":\"2026-02-23T00:00:10Z\",\"type\":\"user\",\"sessionId\":\"{CLAUDE_SESSION_ID}\",\"isSidechain\":true,\"agentId\":\"{CLAUDE_AGENT_ID}\",\"message\":{{\"role\":\"user\",\"content\":\"agent task\"}}}}\n{{\"timestamp\":\"2026-02-23T00:00:11Z\",\"type\":\"assistant\",\"sessionId\":\"{CLAUDE_SESSION_ID}\",\"isSidechain\":true,\"agentId\":\"{CLAUDE_AGENT_ID}\",\"message\":{{\"role\":\"assistant\",\"content\":\"agent done\"}}}}\n"
        ),
    )
    .expect("write agent");

    temp
}

fn setup_gemini_tree() -> tempfile::TempDir {
    let temp = tempdir().expect("tempdir");
    let thread_path = temp.path().join(
        ".gemini/tmp/0c0d7b04c22749f3687ea60b66949fd32bcea2551d4349bf72346a9ccc9a9ba4/chats/session-2026-01-08T11-55-29-29d207db.json",
    );
    fs::create_dir_all(thread_path.parent().expect("parent")).expect("mkdir");
    fs::write(
        &thread_path,
        format!(
            r#"{{
  "sessionId": "{GEMINI_SESSION_ID}",
  "projectHash": "0c0d7b04c22749f3687ea60b66949fd32bcea2551d4349bf72346a9ccc9a9ba4",
  "startTime": "2026-01-08T11:55:12.379Z",
  "lastUpdated": "2026-01-08T12:31:14.881Z",
  "messages": [
    {{ "type": "info", "content": "ignored" }},
    {{ "type": "user", "content": "hello" }},
    {{ "type": "gemini", "content": "world" }}
  ]
}}"#
        ),
    )
    .expect("write");
    temp
}

fn setup_gemini_subagent_tree() -> tempfile::TempDir {
    let temp = tempdir().expect("tempdir");
    let project_hash = "0c0d7b04c22749f3687ea60b66949fd32bcea2551d4349bf72346a9ccc9a9ba4";
    let project_root = temp.path().join(format!(".gemini/tmp/{project_hash}"));
    let chats_dir = project_root.join("chats");
    fs::create_dir_all(&chats_dir).expect("mkdir chats");

    let main_chat_path = chats_dir.join("session-2026-01-08T11-55-main.json");
    fs::write(
        &main_chat_path,
        format!(
            r#"{{
  "sessionId": "{GEMINI_SESSION_ID}",
  "projectHash": "{project_hash}",
  "startTime": "2026-01-08T11:55:12.379Z",
  "lastUpdated": "2026-01-08T12:31:14.881Z",
  "messages": [
    {{ "type": "user", "content": "hello main" }},
    {{ "type": "gemini", "content": "main done" }}
  ]
}}"#
        ),
    )
    .expect("write main chat");

    let child_chat_path = chats_dir.join("session-2026-01-08T12-12-child.json");
    fs::write(
        &child_chat_path,
        format!(
            r#"{{
  "sessionId": "{GEMINI_CHILD_SESSION_ID}",
  "parentSessionId": "{GEMINI_SESSION_ID}",
  "projectHash": "{project_hash}",
  "startTime": "2026-01-08T12:12:00.000Z",
  "lastUpdated": "2026-01-08T12:20:00.000Z",
  "messages": [
    {{ "type": "user", "content": "/resume" }},
    {{ "type": "gemini", "content": "child done" }}
  ]
}}"#
        ),
    )
    .expect("write child chat");

    let logs_path = project_root.join("logs.json");
    fs::write(
        &logs_path,
        format!(
            r#"[
  {{
    "sessionId": "{GEMINI_SESSION_ID}",
    "messageId": 0,
    "type": "user",
    "message": "hello main",
    "timestamp": "2026-01-08T11:59:09.195Z"
  }},
  {{
    "sessionId": "{GEMINI_MISSING_CHILD_SESSION_ID}",
    "messageId": 0,
    "type": "user",
    "message": "/resume",
    "timestamp": "2026-01-08T12:00:09.195Z"
  }},
  {{
    "sessionId": "{GEMINI_CHILD_SESSION_ID}",
    "messageId": 0,
    "type": "user",
    "message": "/resume",
    "timestamp": "2026-01-08T12:11:44.907Z"
  }}
]"#
        ),
    )
    .expect("write logs");

    temp
}

fn setup_gemini_subagent_tree_with_ndjson_logs() -> tempfile::TempDir {
    let temp = setup_gemini_subagent_tree();
    let project_hash = "0c0d7b04c22749f3687ea60b66949fd32bcea2551d4349bf72346a9ccc9a9ba4";
    let logs_path = temp
        .path()
        .join(format!(".gemini/tmp/{project_hash}/logs.json"));
    fs::write(
        &logs_path,
        format!(
            r#"{{"sessionId":"{GEMINI_SESSION_ID}","messageId":0,"type":"user","message":"hello main","timestamp":"2026-01-08T11:59:09.195Z"}}
{{"sessionId":"{GEMINI_MISSING_CHILD_SESSION_ID}","messageId":0,"type":"user","message":"/resume","timestamp":"2026-01-08T12:00:09.195Z"}}
{{"sessionId":"{GEMINI_CHILD_SESSION_ID}","messageId":0,"type":"user","message":"/resume","timestamp":"2026-01-08T12:11:44.907Z"}}"#
        ),
    )
    .expect("write ndjson logs");

    temp
}

fn setup_pi_tree() -> tempfile::TempDir {
    let temp = tempdir().expect("tempdir");
    let thread_path = temp.path().join(
        "agent/sessions/--Users-xuanwo-Code-pi-project--/2026-02-23T13-00-12-780Z_12cb4c19-2774-4de4-a0d0-9fa32fbae29f.jsonl",
    );
    fs::create_dir_all(thread_path.parent().expect("parent")).expect("mkdir");
    fs::write(
        &thread_path,
        format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"{PI_SESSION_ID}\",\"timestamp\":\"2026-02-23T13:00:12.780Z\",\"cwd\":\"/tmp/project\"}}\n{{\"type\":\"message\",\"id\":\"a1b2c3d4\",\"parentId\":null,\"timestamp\":\"2026-02-23T13:00:13.000Z\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"root\"}}]}}}}\n{{\"type\":\"message\",\"id\":\"b1b2c3d4\",\"parentId\":\"a1b2c3d4\",\"timestamp\":\"2026-02-23T13:00:14.000Z\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"root done\"}}]}}}}\n{{\"type\":\"message\",\"id\":\"c1b2c3d4\",\"parentId\":\"b1b2c3d4\",\"timestamp\":\"2026-02-23T13:00:15.000Z\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"branch one\"}}]}}}}\n{{\"type\":\"message\",\"id\":\"d1b2c3d4\",\"parentId\":\"c1b2c3d4\",\"timestamp\":\"2026-02-23T13:00:16.000Z\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"branch one done\"}}]}}}}\n{{\"type\":\"message\",\"id\":\"e1b2c3d4\",\"parentId\":\"b1b2c3d4\",\"timestamp\":\"2026-02-23T13:00:17.000Z\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"branch two\"}}]}}}}\n{{\"type\":\"message\",\"id\":\"f1b2c3d4\",\"parentId\":\"e1b2c3d4\",\"timestamp\":\"2026-02-23T13:00:18.000Z\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"branch two done\"}}]}}}}\n"
        ),
    )
    .expect("write");
    temp
}

fn setup_pi_tree_with_child_sessions() -> tempfile::TempDir {
    let temp = tempdir().expect("tempdir");
    let main_thread_path = temp.path().join(
        "agent/sessions/--Users-xuanwo-Code-pi-project--/2026-02-23T13-00-12-780Z_12cb4c19-2774-4de4-a0d0-9fa32fbae29f.jsonl",
    );
    fs::create_dir_all(main_thread_path.parent().expect("parent")).expect("mkdir");
    fs::write(
        &main_thread_path,
        format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"{PI_SESSION_ID}\",\"timestamp\":\"2026-02-23T13:00:12.780Z\",\"cwd\":\"/tmp/project\",\"childSessionIds\":[\"{PI_CHILD_SESSION_ID}\",\"{PI_MISSING_CHILD_SESSION_ID}\"]}}\n{{\"type\":\"message\",\"id\":\"a1b2c3d4\",\"parentId\":null,\"timestamp\":\"2026-02-23T13:00:13.000Z\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"root\"}}]}}}}\n{{\"type\":\"message\",\"id\":\"b1b2c3d4\",\"parentId\":\"a1b2c3d4\",\"timestamp\":\"2026-02-23T13:00:14.000Z\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"root done\"}}]}}}}\n"
        ),
    )
    .expect("write main");

    let child_thread_path = temp.path().join(format!(
        "agent/sessions/--Users-xuanwo-Code-pi-project--/2026-02-23T13-10-12-780Z_{PI_CHILD_SESSION_ID}.jsonl"
    ));
    fs::create_dir_all(child_thread_path.parent().expect("parent")).expect("mkdir");
    fs::write(
        &child_thread_path,
        format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"{PI_CHILD_SESSION_ID}\",\"timestamp\":\"2026-02-23T13:10:12.780Z\",\"cwd\":\"/tmp/project\",\"parent_session_id\":\"{PI_SESSION_ID}\"}}\n{{\"type\":\"message\",\"id\":\"b1c2d3e4\",\"parentId\":null,\"timestamp\":\"2026-02-23T13:10:13.000Z\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"child prompt\"}}]}}}}\n{{\"type\":\"message\",\"id\":\"c1d2e3f4\",\"parentId\":\"b1c2d3e4\",\"timestamp\":\"2026-02-23T13:10:14.000Z\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"child done\"}}]}}}}\n"
        ),
    )
    .expect("write child");
    temp
}

fn setup_opencode_subagent_tree() -> tempfile::TempDir {
    let temp = tempdir().expect("tempdir");
    let opencode_root = temp.path().join("opencode");
    fs::create_dir_all(&opencode_root).expect("mkdir");
    let db_path = opencode_root.join("opencode.db");

    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute_batch(
        "
        CREATE TABLE session (
            id TEXT PRIMARY KEY,
            parent_id TEXT
        );
        CREATE TABLE message (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            time_created INTEGER NOT NULL,
            data TEXT NOT NULL
        );
        CREATE TABLE part (
            id TEXT PRIMARY KEY,
            message_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            time_created INTEGER NOT NULL,
            data TEXT NOT NULL
        );
        ",
    )
    .expect("create schema");

    conn.execute(
        "INSERT INTO session (id, parent_id) VALUES (?1, NULL)",
        [OPENCODE_MAIN_SESSION_ID],
    )
    .expect("insert main session");
    conn.execute(
        "INSERT INTO session (id, parent_id) VALUES (?1, ?2)",
        params![OPENCODE_CHILD_SESSION_ID, OPENCODE_MAIN_SESSION_ID],
    )
    .expect("insert child session");
    conn.execute(
        "INSERT INTO session (id, parent_id) VALUES (?1, ?2)",
        params![OPENCODE_CHILD_EMPTY_SESSION_ID, OPENCODE_MAIN_SESSION_ID],
    )
    .expect("insert empty child session");

    conn.execute(
        "INSERT INTO message (id, session_id, time_created, data) VALUES (?1, ?2, ?3, ?4)",
        params![
            "main_msg_1",
            OPENCODE_MAIN_SESSION_ID,
            1_i64,
            r#"{"role":"user","time":{"created":1}}"#
        ],
    )
    .expect("insert main user");
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            "main_part_1",
            "main_msg_1",
            OPENCODE_MAIN_SESSION_ID,
            1_i64,
            r#"{"type":"text","text":"main root prompt"}"#
        ],
    )
    .expect("insert main user part");

    conn.execute(
        "INSERT INTO message (id, session_id, time_created, data) VALUES (?1, ?2, ?3, ?4)",
        params![
            "child_msg_1",
            OPENCODE_CHILD_SESSION_ID,
            2_i64,
            r#"{"role":"user","time":{"created":2}}"#
        ],
    )
    .expect("insert child user");
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            "child_part_1",
            "child_msg_1",
            OPENCODE_CHILD_SESSION_ID,
            2_i64,
            r#"{"type":"text","text":"child asks for help"}"#
        ],
    )
    .expect("insert child user part");

    conn.execute(
        "INSERT INTO message (id, session_id, time_created, data) VALUES (?1, ?2, ?3, ?4)",
        params![
            "child_msg_2",
            OPENCODE_CHILD_SESSION_ID,
            3_i64,
            r#"{"role":"assistant","time":{"created":3,"completed":4}}"#
        ],
    )
    .expect("insert child assistant");
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            "child_part_2",
            "child_msg_2",
            OPENCODE_CHILD_SESSION_ID,
            3_i64,
            r#"{"type":"text","text":"child completed"}"#
        ],
    )
    .expect("insert child assistant part");

    temp
}

fn codex_real_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_real_sanitized")
}

fn claude_real_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude_real_sanitized")
}

fn gemini_real_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gemini_real_sanitized")
}

fn opencode_real_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/opencode_real_sanitized")
}

fn pi_real_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi_real_sanitized")
}

fn codex_uri() -> String {
    format!("codex://{SESSION_ID}")
}

fn agents_codex_uri() -> String {
    format!("agents://codex/{SESSION_ID}")
}

fn shorthand_codex_uri() -> String {
    format!("codex/{SESSION_ID}")
}

fn codex_deeplink_uri() -> String {
    format!("codex://threads/{SESSION_ID}")
}

fn agents_codex_deeplink_uri() -> String {
    format!("agents://codex/threads/{SESSION_ID}")
}

fn amp_uri() -> String {
    format!("amp://{AMP_SESSION_ID}")
}

fn amp_subagent_uri() -> String {
    format!("amp://{AMP_SESSION_ID}/{AMP_SUBAGENT_ID}")
}

fn agents_amp_subagent_uri() -> String {
    format!("agents://amp/{AMP_SESSION_ID}/{AMP_SUBAGENT_ID}")
}

fn codex_subagent_uri() -> String {
    format!("codex://{SESSION_ID}/{SUBAGENT_ID}")
}

fn agents_codex_subagent_uri() -> String {
    format!("agents://codex/{SESSION_ID}/{SUBAGENT_ID}")
}

fn claude_subagent_uri() -> String {
    format!("claude://{CLAUDE_SESSION_ID}/{CLAUDE_AGENT_ID}")
}

fn agents_uri(provider: &str, session_id: &str) -> String {
    format!("agents://{provider}/{session_id}")
}

fn agents_child_uri(provider: &str, session_id: &str, child_id: &str) -> String {
    format!("agents://{provider}/{session_id}/{child_id}")
}

fn gemini_uri() -> String {
    format!("gemini://{GEMINI_SESSION_ID}")
}

fn agents_gemini_subagent_uri() -> String {
    format!("agents://gemini/{GEMINI_SESSION_ID}/{GEMINI_CHILD_SESSION_ID}")
}

fn gemini_missing_subagent_uri() -> String {
    format!("gemini://{GEMINI_SESSION_ID}/{GEMINI_MISSING_CHILD_SESSION_ID}")
}

fn gemini_real_uri() -> String {
    format!("gemini://{GEMINI_REAL_SESSION_ID}")
}

fn pi_uri() -> String {
    format!("pi://{PI_SESSION_ID}")
}

fn pi_entry_uri() -> String {
    format!("pi://{PI_SESSION_ID}/{PI_ENTRY_ID}")
}

fn pi_child_session_uri() -> String {
    format!("pi://{PI_SESSION_ID}/{PI_CHILD_SESSION_ID}")
}

fn pi_missing_child_session_uri() -> String {
    format!("pi://{PI_SESSION_ID}/{PI_MISSING_CHILD_SESSION_ID}")
}

fn pi_real_uri() -> String {
    format!("pi://{PI_REAL_SESSION_ID}")
}

fn claude_real_uri() -> String {
    format!("claude://{CLAUDE_REAL_MAIN_ID}")
}

fn claude_real_subagent_uri() -> String {
    format!("claude://{CLAUDE_REAL_MAIN_ID}/{CLAUDE_REAL_AGENT_ID}")
}

fn opencode_real_uri() -> String {
    format!("opencode://{OPENCODE_REAL_SESSION_ID}")
}

#[cfg(unix)]
fn setup_mock_bins(entries: &[(&str, &str)]) -> tempfile::TempDir {
    let temp = tempdir().expect("tempdir");
    for (name, body) in entries {
        let path = temp.path().join(name);
        let script = format!("#!/bin/sh\nset -eu\n{body}\n");
        fs::write(&path, script).expect("write mock script");
        let mut perms = fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("chmod");
    }
    temp
}

#[cfg(unix)]
fn path_with_mock(mock_root: &std::path::Path) -> String {
    let current = env::var("PATH").unwrap_or_default();
    format!("{}:{current}", mock_root.display())
}

fn encode_query_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

#[test]
fn default_outputs_markdown() {
    let temp = setup_codex_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .env("CLAUDE_CONFIG_DIR", temp.path().join("missing-claude"))
        .arg(codex_uri())
        .assert()
        .success()
        .stdout(predicate::str::contains("---\n"))
        .stdout(predicate::str::contains("uri: 'agents://codex/"))
        .stdout(predicate::str::contains("thread_source: '"))
        .stdout(predicate::str::contains("# Thread"))
        .stdout(predicate::str::contains("## Timeline"))
        .stdout(predicate::str::contains("## 1. User"))
        .stdout(predicate::str::contains("hello"));
}

#[test]
fn output_flag_writes_markdown_to_file() {
    let temp = setup_codex_tree();
    let output_dir = tempdir().expect("tempdir");
    let output_path = output_dir.path().join("thread.md");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .env("CLAUDE_CONFIG_DIR", temp.path().join("missing-claude"))
        .arg(codex_uri())
        .arg("-o")
        .arg(&output_path)
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    let written = fs::read_to_string(output_path).expect("read output");
    assert!(written.contains("---\n"));
    assert!(written.contains("# Thread"));
    assert!(written.contains("hello"));
}

#[test]
fn output_flag_returns_error_when_parent_directory_missing() {
    let temp = setup_codex_tree();
    let missing_parent = temp.path().join("missing-parent");
    let output_path = missing_parent.join("thread.md");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .env("CLAUDE_CONFIG_DIR", temp.path().join("missing-claude"))
        .arg(codex_uri())
        .arg("--output")
        .arg(&output_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("error: i/o error on"));
}

#[test]
fn agents_uri_outputs_markdown() {
    let temp = setup_codex_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .env("CLAUDE_CONFIG_DIR", temp.path().join("missing-claude"))
        .arg(agents_codex_uri())
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "uri: 'agents://codex/{SESSION_ID}'"
        )))
        .stdout(predicate::str::contains("## 1. User"))
        .stdout(predicate::str::contains("hello"));
}

#[test]
fn shorthand_uri_outputs_markdown() {
    let temp = setup_codex_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .env("CLAUDE_CONFIG_DIR", temp.path().join("missing-claude"))
        .arg(shorthand_codex_uri())
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "uri: 'agents://codex/{SESSION_ID}'"
        )))
        .stdout(predicate::str::contains("## 1. User"))
        .stdout(predicate::str::contains("hello"));
}

#[test]
fn skills_scheme_is_rejected() {
    let temp = setup_codex_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .arg("skills://xurl")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "error: unsupported scheme: skills",
        ));
}

#[test]
fn raw_flag_is_rejected() {
    let temp = setup_codex_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .env("CLAUDE_CONFIG_DIR", temp.path().join("missing-claude"))
        .arg(codex_uri())
        .arg("--raw")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument '--raw'"));
}

#[test]
fn amp_collection_query_outputs_markdown() {
    let temp = setup_amp_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("XDG_DATA_HOME", temp.path())
        .arg("agents://amp?q=world&limit=1")
        .assert()
        .success()
        .stdout(predicate::str::contains("# Threads"))
        .stdout(predicate::str::contains("- Limit: `1`"))
        .stdout(predicate::str::contains(format!(
            "agents://amp/{AMP_SESSION_ID}"
        )))
        .stdout(predicate::str::contains("- Match:"));
}

#[test]
fn codex_collection_query_outputs_markdown() {
    let temp = setup_codex_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .arg("agents://codex?q=hello&limit=1")
        .assert()
        .success()
        .stdout(predicate::str::contains("# Threads"))
        .stdout(predicate::str::contains("- Limit: `1`"))
        .stdout(predicate::str::contains(format!(
            "agents://codex/{SESSION_ID}"
        )))
        .stdout(predicate::str::contains("- Match:"));
}

#[test]
fn shorthand_collection_query_outputs_markdown() {
    let temp = setup_codex_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .arg("codex?q=hello&limit=1")
        .assert()
        .success()
        .stdout(predicate::str::contains("# Threads"))
        .stdout(predicate::str::contains("- Limit: `1`"))
        .stdout(predicate::str::contains(format!(
            "agents://codex/{SESSION_ID}"
        )))
        .stdout(predicate::str::contains("- Match:"));
}

#[test]
fn role_query_outputs_markdown() {
    let temp = setup_codex_role_query_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .arg("agents://codex/reviewer")
        .assert()
        .success()
        .stdout(predicate::str::contains("# Threads"))
        .stdout(predicate::str::contains("- Role: `reviewer`"))
        .stdout(predicate::str::contains(format!(
            "agents://codex/{SESSION_ID}"
        )))
        .stdout(predicate::str::contains("- Match:"));
}

#[test]
fn shorthand_role_query_outputs_markdown() {
    let temp = setup_codex_role_query_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .arg("codex/reviewer")
        .assert()
        .success()
        .stdout(predicate::str::contains("# Threads"))
        .stdout(predicate::str::contains("- Role: `reviewer`"))
        .stdout(predicate::str::contains(format!(
            "agents://codex/{SESSION_ID}"
        )))
        .stdout(predicate::str::contains("- Match:"));
}

#[test]
fn claude_collection_query_outputs_markdown() {
    let temp = setup_claude_subagent_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CLAUDE_CONFIG_DIR", temp.path())
        .arg("agents://claude?q=agent&limit=1")
        .assert()
        .success()
        .stdout(predicate::str::contains("# Threads"))
        .stdout(predicate::str::contains("- Limit: `1`"))
        .stdout(predicate::str::contains("agents://claude/"))
        .stdout(predicate::str::contains("- Match:"));
}

#[test]
fn gemini_collection_query_outputs_markdown() {
    let temp = setup_gemini_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("GEMINI_CLI_HOME", temp.path())
        .arg("agents://gemini?q=hello&limit=1")
        .assert()
        .success()
        .stdout(predicate::str::contains("# Threads"))
        .stdout(predicate::str::contains("- Limit: `1`"))
        .stdout(predicate::str::contains(format!(
            "agents://gemini/{GEMINI_SESSION_ID}"
        )))
        .stdout(predicate::str::contains("- Match:"));
}

#[test]
fn pi_collection_query_outputs_markdown() {
    let temp = setup_pi_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PI_CODING_AGENT_DIR", temp.path().join("agent"))
        .arg("agents://pi?q=root&limit=1")
        .assert()
        .success()
        .stdout(predicate::str::contains("# Threads"))
        .stdout(predicate::str::contains("- Limit: `1`"))
        .stdout(predicate::str::contains(format!(
            "agents://pi/{PI_SESSION_ID}"
        )))
        .stdout(predicate::str::contains("- Match:"));
}

#[test]
fn opencode_collection_query_outputs_markdown() {
    let temp = setup_opencode_subagent_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("XDG_DATA_HOME", temp.path())
        .arg("agents://opencode?q=help&limit=1")
        .assert()
        .success()
        .stdout(predicate::str::contains("# Threads"))
        .stdout(predicate::str::contains("- Limit: `1`"))
        .stdout(predicate::str::contains("agents://opencode/"))
        .stdout(predicate::str::contains("- Match:"));
}

#[test]
fn collection_query_not_found_outputs_empty_list() {
    let temp = setup_codex_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .arg("agents://codex?q=not-exist")
        .assert()
        .success()
        .stdout(predicate::str::contains("_No threads found._"));
}

#[test]
fn head_flag_outputs_frontmatter_only() {
    let temp = setup_codex_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .env("CLAUDE_CONFIG_DIR", temp.path().join("missing-claude"))
        .arg(codex_uri())
        .arg("-I")
        .assert()
        .success()
        .stdout(predicate::str::contains("---\n"))
        .stdout(predicate::str::contains("mode: 'subagent_index'"))
        .stdout(predicate::str::contains("subagents:"))
        .stdout(predicate::str::contains("# Thread").not());
}

#[test]
fn codex_subagent_head_outputs_header_only() {
    let temp = setup_codex_subagent_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .env("CLAUDE_CONFIG_DIR", temp.path().join("missing-claude"))
        .arg(codex_subagent_uri())
        .arg("--head")
        .assert()
        .success()
        .stdout(predicate::str::contains("mode: 'subagent_detail'"))
        .stdout(predicate::str::contains(format!(
            "agent_id: '{SUBAGENT_ID}'"
        )))
        .stdout(predicate::str::contains("status:"))
        .stdout(predicate::str::contains("# Subagent Thread").not());
}

#[test]
fn codex_deeplink_outputs_markdown() {
    let temp = setup_codex_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .env("CLAUDE_CONFIG_DIR", temp.path().join("missing-claude"))
        .arg(codex_deeplink_uri())
        .assert()
        .success()
        .stdout(predicate::str::contains("# Thread"))
        .stdout(predicate::str::contains("## 1. User"))
        .stdout(predicate::str::contains("hello"));
}

#[test]
fn agents_codex_deeplink_outputs_markdown() {
    let temp = setup_codex_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .env("CLAUDE_CONFIG_DIR", temp.path().join("missing-claude"))
        .arg(agents_codex_deeplink_uri())
        .assert()
        .success()
        .stdout(predicate::str::contains("# Thread"))
        .stdout(predicate::str::contains("## 1. User"))
        .stdout(predicate::str::contains("hello"));
}

#[test]
fn codex_subagent_outputs_markdown_view() {
    let temp = setup_codex_subagent_tree();
    let main_uri = agents_uri("codex", SESSION_ID);
    let subagent_uri = agents_child_uri("codex", SESSION_ID, SUBAGENT_ID);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .env("CLAUDE_CONFIG_DIR", temp.path().join("missing-claude"))
        .arg(codex_subagent_uri())
        .assert()
        .success()
        .stdout(predicate::str::contains("# Subagent Thread"))
        .stdout(predicate::str::contains(format!(
            "- Main Thread: `{main_uri}`"
        )))
        .stdout(predicate::str::contains(format!(
            "- Subagent Thread: `{subagent_uri}`"
        )))
        .stdout(predicate::str::contains("## Lifecycle (Parent Thread)"))
        .stdout(predicate::str::contains("## Thread Excerpt (Child Thread)"));
}

#[test]
fn agents_codex_subagent_outputs_markdown_view() {
    let temp = setup_codex_subagent_tree();
    let main_uri = agents_uri("codex", SESSION_ID);
    let subagent_uri = agents_child_uri("codex", SESSION_ID, SUBAGENT_ID);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .env("CLAUDE_CONFIG_DIR", temp.path().join("missing-claude"))
        .arg(agents_codex_subagent_uri())
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "- Main Thread: `{main_uri}`"
        )))
        .stdout(predicate::str::contains(format!(
            "- Subagent Thread: `{subagent_uri}`"
        )));
}

#[test]
fn codex_outputs_no_warning_text_for_markdown() {
    let temp = setup_codex_tree_with_sqlite_missing_threads();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .env("CLAUDE_CONFIG_DIR", temp.path().join("missing-claude"))
        .arg(codex_uri())
        .assert()
        .success()
        .stderr(predicate::str::contains("warning:").not());
}

#[test]
fn codex_subagent_outputs_no_warning_text_for_markdown() {
    let temp = setup_codex_subagent_tree_with_sqlite_missing_threads();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .env("CLAUDE_CONFIG_DIR", temp.path().join("missing-claude"))
        .arg(codex_subagent_uri())
        .assert()
        .success()
        .stderr(predicate::str::contains("warning:").not());
}

#[test]
fn codex_real_fixture_head_includes_subagents() {
    let fixture_root = codex_real_fixture_root();
    assert!(fixture_root.exists(), "fixture root must exist");
    let subagent_uri = agents_child_uri("codex", REAL_FIXTURE_MAIN_ID, REAL_FIXTURE_AGENT_ID);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", fixture_root)
        .env("CLAUDE_CONFIG_DIR", "/tmp/missing-claude")
        .arg(format!("codex://{REAL_FIXTURE_MAIN_ID}"))
        .arg("--head")
        .assert()
        .success()
        .stdout(predicate::str::contains("mode: 'subagent_index'"))
        .stdout(predicate::str::contains("subagents:"))
        .stdout(predicate::str::contains(subagent_uri))
        .stdout(predicate::str::contains("# Subagent Status").not());
}

#[test]
fn codex_real_fixture_head_includes_thread_metadata() {
    let fixture_root = codex_real_fixture_root();
    assert!(fixture_root.exists(), "fixture root must exist");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", fixture_root)
        .env("CLAUDE_CONFIG_DIR", "/tmp/missing-claude")
        .arg(format!("codex://{REAL_FIXTURE_MAIN_ID}"))
        .arg("--head")
        .assert()
        .success()
        .stdout(predicate::str::contains("thread_metadata:"))
        .stdout(predicate::str::contains("type = session_meta"))
        .stdout(predicate::str::contains(
            "payload.cwd = /redacted/5fc12f120e/eaf99e1a0891",
        ))
        .stdout(predicate::str::contains(
            "payload.git.branch = txt_1ee2ff8bde628ccd",
        ))
        .stdout(predicate::str::contains(
            "payload.model_provider = txt_e55535ca2bfc02d0",
        ))
        .stdout(predicate::str::contains("base_instructions").not())
        .stdout(predicate::str::contains("user_instructions").not());
}

#[test]
fn codex_real_fixture_subagent_detail_outputs_markdown() {
    let fixture_root = codex_real_fixture_root();
    assert!(fixture_root.exists(), "fixture root must exist");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", fixture_root)
        .env("CLAUDE_CONFIG_DIR", "/tmp/missing-claude")
        .arg(format!(
            "codex://{REAL_FIXTURE_MAIN_ID}/{REAL_FIXTURE_AGENT_ID}"
        ))
        .assert()
        .success()
        .stdout(predicate::str::contains("# Subagent Thread"))
        .stdout(predicate::str::contains("## Lifecycle (Parent Thread)"));
}

#[test]
fn list_flag_is_rejected() {
    let temp = setup_codex_subagent_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .env("CLAUDE_CONFIG_DIR", temp.path().join("missing-claude"))
        .arg(codex_subagent_uri())
        .arg("--list")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument '--list'"));
}

#[test]
fn missing_thread_returns_non_zero() {
    let temp = tempdir().expect("tempdir");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CODEX_HOME", temp.path())
        .env("CLAUDE_CONFIG_DIR", temp.path())
        .arg(codex_uri())
        .assert()
        .failure()
        .stderr(predicate::str::contains("thread not found"));
}

#[test]
fn amp_outputs_markdown() {
    let temp = setup_amp_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("XDG_DATA_HOME", temp.path())
        .env("CODEX_HOME", temp.path().join("missing-codex"))
        .env("CLAUDE_CONFIG_DIR", temp.path().join("missing-claude"))
        .arg(amp_uri())
        .assert()
        .success()
        .stdout(predicate::str::contains("# Thread"))
        .stdout(predicate::str::contains("## 1. User"))
        .stdout(predicate::str::contains("hello"))
        .stdout(predicate::str::contains("analyze"))
        .stdout(predicate::str::contains("world"));
}

#[test]
fn amp_head_outputs_subagent_index() {
    let temp = setup_amp_subagent_tree();
    let subagent_uri = agents_child_uri("amp", AMP_SESSION_ID, AMP_SUBAGENT_ID);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("XDG_DATA_HOME", temp.path())
        .env("CODEX_HOME", temp.path().join("missing-codex"))
        .env("CLAUDE_CONFIG_DIR", temp.path().join("missing-claude"))
        .arg(agents_uri("amp", AMP_SESSION_ID))
        .arg("--head")
        .assert()
        .success()
        .stdout(predicate::str::contains("mode: 'subagent_index'"))
        .stdout(predicate::str::contains("subagents:"))
        .stdout(predicate::str::contains(subagent_uri))
        .stdout(predicate::str::contains("# Subagent Status").not());
}

#[test]
fn amp_head_discovery_supports_missing_role_fallback() {
    let temp = setup_amp_subagent_tree_missing_role();
    let subagent_uri = agents_child_uri("amp", AMP_SESSION_ID, AMP_SUBAGENT_ID);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("XDG_DATA_HOME", temp.path())
        .env("CODEX_HOME", temp.path().join("missing-codex"))
        .env("CLAUDE_CONFIG_DIR", temp.path().join("missing-claude"))
        .arg(agents_uri("amp", AMP_SESSION_ID))
        .arg("--head")
        .assert()
        .success()
        .stdout(predicate::str::contains("mode: 'subagent_index'"))
        .stdout(predicate::str::contains("subagents:"))
        .stdout(predicate::str::contains(subagent_uri));
}

#[test]
fn amp_subagent_head_outputs_header_only() {
    let temp = setup_amp_subagent_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("XDG_DATA_HOME", temp.path())
        .env("CODEX_HOME", temp.path().join("missing-codex"))
        .env("CLAUDE_CONFIG_DIR", temp.path().join("missing-claude"))
        .arg(amp_subagent_uri())
        .arg("--head")
        .assert()
        .success()
        .stdout(predicate::str::contains("mode: 'subagent_detail'"))
        .stdout(predicate::str::contains(format!(
            "agent_id: '{AMP_SUBAGENT_ID}'"
        )))
        .stdout(predicate::str::contains("status:"))
        .stdout(predicate::str::contains("# Subagent Thread").not());
}

#[test]
fn amp_subagent_outputs_markdown_view() {
    let temp = setup_amp_subagent_tree();
    let main_uri = agents_uri("amp", AMP_SESSION_ID);
    let subagent_uri = agents_child_uri("amp", AMP_SESSION_ID, AMP_SUBAGENT_ID);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("XDG_DATA_HOME", temp.path())
        .env("CODEX_HOME", temp.path().join("missing-codex"))
        .env("CLAUDE_CONFIG_DIR", temp.path().join("missing-claude"))
        .arg(agents_amp_subagent_uri())
        .assert()
        .success()
        .stdout(predicate::str::contains("# Subagent Thread"))
        .stdout(predicate::str::contains(format!(
            "- Main Thread: `{main_uri}`"
        )))
        .stdout(predicate::str::contains(format!(
            "- Subagent Thread: `{subagent_uri}`"
        )))
        .stdout(predicate::str::contains("- Relation: `validated`"))
        .stdout(predicate::str::contains("## Lifecycle (Parent Thread)"))
        .stdout(predicate::str::contains("## Thread Excerpt (Child Thread)"));
}

#[test]
fn gemini_outputs_markdown() {
    let temp = setup_gemini_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("GEMINI_CLI_HOME", temp.path())
        .arg(gemini_uri())
        .assert()
        .success()
        .stdout(predicate::str::contains("# Thread"))
        .stdout(predicate::str::contains("## 1. User"))
        .stdout(predicate::str::contains("hello"))
        .stdout(predicate::str::contains("world"));
}

#[test]
fn gemini_head_outputs_subagent_discovery() {
    let temp = setup_gemini_subagent_tree();
    let main_uri = agents_uri("gemini", GEMINI_SESSION_ID);
    let child_uri = agents_child_uri("gemini", GEMINI_SESSION_ID, GEMINI_CHILD_SESSION_ID);
    let missing_uri =
        agents_child_uri("gemini", GEMINI_SESSION_ID, GEMINI_MISSING_CHILD_SESSION_ID);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("GEMINI_CLI_HOME", temp.path())
        .arg(main_uri)
        .arg("--head")
        .assert()
        .success()
        .stdout(predicate::str::contains("mode: 'subagent_index'"))
        .stdout(predicate::str::contains("subagents:"))
        .stdout(predicate::str::contains(child_uri))
        .stdout(predicate::str::contains(missing_uri))
        .stdout(predicate::str::contains("status: 'notFound'"))
        .stdout(predicate::str::contains("warnings:"));
}

#[test]
fn gemini_head_outputs_subagent_discovery_from_ndjson_logs() {
    let temp = setup_gemini_subagent_tree_with_ndjson_logs();
    let main_uri = agents_uri("gemini", GEMINI_SESSION_ID);
    let child_uri = agents_child_uri("gemini", GEMINI_SESSION_ID, GEMINI_CHILD_SESSION_ID);
    let missing_uri =
        agents_child_uri("gemini", GEMINI_SESSION_ID, GEMINI_MISSING_CHILD_SESSION_ID);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("GEMINI_CLI_HOME", temp.path())
        .arg(main_uri)
        .arg("--head")
        .assert()
        .success()
        .stdout(predicate::str::contains("mode: 'subagent_index'"))
        .stdout(predicate::str::contains("subagents:"))
        .stdout(predicate::str::contains(child_uri))
        .stdout(predicate::str::contains(missing_uri))
        .stdout(predicate::str::contains("status: 'notFound'"));
}

#[test]
fn gemini_subagent_outputs_markdown_view() {
    let temp = setup_gemini_subagent_tree();
    let main_uri = agents_uri("gemini", GEMINI_SESSION_ID);
    let subagent_uri = agents_child_uri("gemini", GEMINI_SESSION_ID, GEMINI_CHILD_SESSION_ID);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("GEMINI_CLI_HOME", temp.path())
        .arg(agents_gemini_subagent_uri())
        .assert()
        .success()
        .stdout(predicate::str::contains("# Subagent Thread"))
        .stdout(predicate::str::contains(format!(
            "- Main Thread: `{main_uri}`"
        )))
        .stdout(predicate::str::contains(format!(
            "- Subagent Thread: `{subagent_uri}`"
        )))
        .stdout(predicate::str::contains("## Thread Excerpt (Child Thread)"));
}

#[test]
fn gemini_missing_subagent_outputs_not_found_markdown() {
    let temp = setup_gemini_subagent_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("GEMINI_CLI_HOME", temp.path())
        .arg(gemini_missing_subagent_uri())
        .assert()
        .success()
        .stdout(predicate::str::contains("# Subagent Thread"))
        .stdout(predicate::str::contains(
            "- Status: `notFound` (`inferred`)",
        ))
        .stdout(predicate::str::contains(
            "_No child thread messages found._",
        ));
}

#[test]
fn pi_outputs_markdown_from_latest_leaf() {
    let temp = setup_pi_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PI_CODING_AGENT_DIR", temp.path().join("agent"))
        .arg(pi_uri())
        .assert()
        .success()
        .stdout(predicate::str::contains("# Thread"))
        .stdout(predicate::str::contains("## Timeline"))
        .stdout(predicate::str::contains("root"))
        .stdout(predicate::str::contains("branch two done"));
}

#[test]
fn pi_entry_outputs_markdown_from_requested_leaf() {
    let temp = setup_pi_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PI_CODING_AGENT_DIR", temp.path().join("agent"))
        .arg(pi_entry_uri())
        .assert()
        .success()
        .stdout(predicate::str::contains("# Thread"))
        .stdout(predicate::str::contains("branch one done"))
        .stdout(predicate::str::contains("branch two done").not());
}

#[test]
fn pi_head_outputs_entries() {
    let temp = setup_pi_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PI_CODING_AGENT_DIR", temp.path().join("agent"))
        .arg(pi_uri())
        .arg("--head")
        .assert()
        .success()
        .stdout(predicate::str::contains("mode: 'pi_entry_index'"))
        .stdout(predicate::str::contains("entries:"))
        .stdout(predicate::str::contains(format!(
            "uri: 'agents://pi/{PI_SESSION_ID}/a1b2c3d4'"
        )))
        .stdout(predicate::str::contains("is_leaf: true"));
}

#[test]
fn pi_head_outputs_entries_and_child_sessions() {
    let temp = setup_pi_tree_with_child_sessions();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PI_CODING_AGENT_DIR", temp.path().join("agent"))
        .arg(pi_uri())
        .arg("--head")
        .assert()
        .success()
        .stdout(predicate::str::contains("mode: 'pi_entry_index'"))
        .stdout(predicate::str::contains("entries:"))
        .stdout(predicate::str::contains("subagents:"))
        .stdout(predicate::str::contains(format!(
            "uri: 'agents://pi/{PI_SESSION_ID}/{PI_CHILD_SESSION_ID}'"
        )))
        .stdout(predicate::str::contains(format!(
            "uri: 'agents://pi/{PI_SESSION_ID}/{PI_MISSING_CHILD_SESSION_ID}'"
        )))
        .stdout(predicate::str::contains("status: 'completed'"))
        .stdout(predicate::str::contains("status: 'notFound'"))
        .stdout(predicate::str::contains("warnings:"));
}

#[test]
fn pi_child_session_outputs_subagent_markdown_view() {
    let temp = setup_pi_tree_with_child_sessions();
    let main_uri = agents_uri("pi", PI_SESSION_ID);
    let child_uri = agents_child_uri("pi", PI_SESSION_ID, PI_CHILD_SESSION_ID);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PI_CODING_AGENT_DIR", temp.path().join("agent"))
        .arg(&child_uri)
        .assert()
        .success()
        .stdout(predicate::str::contains("# Subagent Thread"))
        .stdout(predicate::str::contains(format!(
            "- Main Thread: `{main_uri}`"
        )))
        .stdout(predicate::str::contains(format!(
            "- Subagent Thread: `{child_uri}`"
        )))
        .stdout(predicate::str::contains("child done"))
        .stdout(predicate::str::contains("## Thread Excerpt (Child Thread)"));
}

#[test]
fn pi_child_session_head_outputs_subagent_detail() {
    let temp = setup_pi_tree_with_child_sessions();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PI_CODING_AGENT_DIR", temp.path().join("agent"))
        .arg(pi_child_session_uri())
        .arg("--head")
        .assert()
        .success()
        .stdout(predicate::str::contains("mode: 'subagent_detail'"))
        .stdout(predicate::str::contains(format!(
            "agent_id: '{PI_CHILD_SESSION_ID}'"
        )))
        .stdout(predicate::str::contains("status: 'completed'"))
        .stdout(predicate::str::contains("# Subagent Thread").not());
}

#[test]
fn pi_missing_child_session_head_reports_not_found_with_evidence() {
    let temp = setup_pi_tree_with_child_sessions();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PI_CODING_AGENT_DIR", temp.path().join("agent"))
        .arg(pi_missing_child_session_uri())
        .arg("--head")
        .assert()
        .success()
        .stdout(predicate::str::contains("mode: 'subagent_detail'"))
        .stdout(predicate::str::contains(format!(
            "agent_id: '{PI_MISSING_CHILD_SESSION_ID}'"
        )))
        .stdout(predicate::str::contains("status: 'notFound'"))
        .stdout(predicate::str::contains("warnings:"))
        .stdout(predicate::str::contains(
            "relation hint references child_session_id",
        ));
}

#[test]
fn pi_head_entry_outputs_header_only() {
    let temp = setup_pi_tree();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PI_CODING_AGENT_DIR", temp.path().join("agent"))
        .arg(pi_entry_uri())
        .arg("--head")
        .assert()
        .success()
        .stdout(predicate::str::contains("mode: 'pi_entry'"))
        .stdout(predicate::str::contains(format!(
            "entry_id: '{PI_ENTRY_ID}'"
        )))
        .stdout(predicate::str::contains("# Thread").not());
}

#[test]
fn pi_real_fixture_outputs_markdown() {
    let fixture_root = pi_real_fixture_root();
    assert!(fixture_root.exists(), "fixture root must exist");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PI_CODING_AGENT_DIR", fixture_root)
        .arg(pi_real_uri())
        .assert()
        .success()
        .stdout(predicate::str::contains("# Thread"))
        .stdout(predicate::str::contains("## 1. User"))
        .stdout(predicate::str::contains("## 2. Assistant"));
}

#[test]
fn pi_real_fixture_head_includes_thread_metadata() {
    let fixture_root = pi_real_fixture_root();
    assert!(fixture_root.exists(), "fixture root must exist");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PI_CODING_AGENT_DIR", fixture_root)
        .arg(pi_real_uri())
        .arg("--head")
        .assert()
        .success()
        .stdout(predicate::str::contains("thread_metadata:"))
        .stdout(predicate::str::contains("type = session"))
        .stdout(predicate::str::contains(
            "cwd = /redacted/workspace/project",
        ))
        .stdout(predicate::str::contains("type = model_change").not())
        .stdout(predicate::str::contains("modelId = gpt-5.3-codex").not())
        .stdout(predicate::str::contains("type = thinking_level_change").not())
        .stdout(predicate::str::contains("thinkingLevel = medium").not());
}

#[test]
fn claude_subagent_outputs_markdown_view() {
    let temp = setup_claude_subagent_tree();
    let main_uri = agents_uri("claude", CLAUDE_SESSION_ID);
    let subagent_uri = agents_child_uri("claude", CLAUDE_SESSION_ID, CLAUDE_AGENT_ID);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CLAUDE_CONFIG_DIR", temp.path())
        .env("CODEX_HOME", temp.path().join("missing-codex"))
        .arg(claude_subagent_uri())
        .assert()
        .success()
        .stdout(predicate::str::contains("# Subagent Thread"))
        .stdout(predicate::str::contains(format!(
            "- Main Thread: `{main_uri}`"
        )))
        .stdout(predicate::str::contains(format!(
            "- Subagent Thread: `{subagent_uri}`"
        )))
        .stdout(predicate::str::contains("## Agent Status Summary"));
}

#[test]
fn claude_real_fixture_head_includes_subagents() {
    let fixture_root = claude_real_fixture_root();
    assert!(fixture_root.exists(), "fixture root must exist");
    let subagent_uri = agents_child_uri("claude", CLAUDE_REAL_MAIN_ID, CLAUDE_REAL_AGENT_ID);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CLAUDE_CONFIG_DIR", fixture_root)
        .env("CODEX_HOME", "/tmp/missing-codex")
        .arg(claude_real_uri())
        .arg("--head")
        .assert()
        .success()
        .stdout(predicate::str::contains("mode: 'subagent_index'"))
        .stdout(predicate::str::contains("subagents:"))
        .stdout(predicate::str::contains(subagent_uri))
        .stdout(predicate::str::contains("# Subagent Status").not());
}

#[test]
fn claude_real_fixture_subagent_head_includes_thread_metadata() {
    let fixture_root = claude_real_fixture_root();
    assert!(fixture_root.exists(), "fixture root must exist");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CLAUDE_CONFIG_DIR", fixture_root)
        .env("CODEX_HOME", "/tmp/missing-codex")
        .arg(claude_real_subagent_uri())
        .arg("--head")
        .assert()
        .success()
        .stdout(predicate::str::contains("thread_metadata:"))
        .stdout(predicate::str::contains(
            "cwd = /redacted/57843fe62b/667def971841",
        ))
        .stdout(predicate::str::contains("gitBranch = txt_1ee2ff8bde628ccd"))
        .stdout(predicate::str::contains("version = txt_3be394b47d685e0a"));
}

#[test]
fn claude_real_fixture_subagent_detail_outputs_markdown() {
    let fixture_root = claude_real_fixture_root();
    assert!(fixture_root.exists(), "fixture root must exist");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("CLAUDE_CONFIG_DIR", fixture_root)
        .env("CODEX_HOME", "/tmp/missing-codex")
        .arg(claude_real_subagent_uri())
        .assert()
        .success()
        .stdout(predicate::str::contains("# Subagent Thread"))
        .stdout(predicate::str::contains("## Thread Excerpt (Child Thread)"));
}

#[test]
fn opencode_subagent_head_includes_subagents_and_warnings() {
    let temp = setup_opencode_subagent_tree();
    let child_uri = agents_child_uri(
        "opencode",
        OPENCODE_MAIN_SESSION_ID,
        OPENCODE_CHILD_SESSION_ID,
    );
    let empty_child_uri = agents_child_uri(
        "opencode",
        OPENCODE_MAIN_SESSION_ID,
        OPENCODE_CHILD_EMPTY_SESSION_ID,
    );

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("XDG_DATA_HOME", temp.path())
        .arg(agents_uri("opencode", OPENCODE_MAIN_SESSION_ID))
        .arg("--head")
        .assert()
        .success()
        .stdout(predicate::str::contains("mode: 'subagent_index'"))
        .stdout(predicate::str::contains("subagents:"))
        .stdout(predicate::str::contains(child_uri))
        .stdout(predicate::str::contains(empty_child_uri))
        .stdout(predicate::str::contains("status: 'completed'"))
        .stdout(predicate::str::contains("status: 'pendingInit'"))
        .stdout(predicate::str::contains("warnings:"))
        .stdout(predicate::str::contains(format!(
            "child session_id={OPENCODE_CHILD_EMPTY_SESSION_ID} has no materialized messages in sqlite"
        )));
}

#[test]
fn opencode_subagent_outputs_markdown_view() {
    let temp = setup_opencode_subagent_tree();
    let main_uri = agents_uri("opencode", OPENCODE_MAIN_SESSION_ID);
    let subagent_uri = agents_child_uri(
        "opencode",
        OPENCODE_MAIN_SESSION_ID,
        OPENCODE_CHILD_SESSION_ID,
    );

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("XDG_DATA_HOME", temp.path())
        .arg(&subagent_uri)
        .assert()
        .success()
        .stdout(predicate::str::contains("# Subagent Thread"))
        .stdout(predicate::str::contains(format!(
            "- Main Thread: `{main_uri}`"
        )))
        .stdout(predicate::str::contains(format!(
            "- Subagent Thread: `{subagent_uri}`"
        )))
        .stdout(predicate::str::contains(
            "- Status: `completed` (`child_rollout`)",
        ))
        .stdout(predicate::str::contains(
            "- Evidence: opencode sqlite relation validated via session.parent_id",
        ))
        .stdout(predicate::str::contains("child completed"));
}

#[test]
fn opencode_subagent_not_found_outputs_markdown_view() {
    let temp = setup_opencode_subagent_tree();
    let missing_child = "ses_5x7md9kx3c9p";
    let missing_uri = agents_child_uri("opencode", OPENCODE_MAIN_SESSION_ID, missing_child);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("XDG_DATA_HOME", temp.path())
        .arg(&missing_uri)
        .assert()
        .success()
        .stdout(predicate::str::contains("# Subagent Thread"))
        .stdout(predicate::str::contains(format!(
            "- Subagent Thread: `{missing_uri}`"
        )))
        .stdout(predicate::str::contains("- Status: `notFound` (`inferred`)"))
        .stdout(predicate::str::contains("_No child thread messages found._"))
        .stdout(predicate::str::contains(format!(
            "agent not found for main_session_id={OPENCODE_MAIN_SESSION_ID} agent_id={missing_child}"
        )));
}

#[test]
fn gemini_real_fixture_outputs_markdown() {
    let fixture_root = gemini_real_fixture_root();
    assert!(fixture_root.exists(), "fixture root must exist");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("GEMINI_CLI_HOME", fixture_root)
        .arg(gemini_real_uri())
        .assert()
        .success()
        .stdout(predicate::str::contains("# Thread"))
        .stdout(predicate::str::contains("## 1. User"));
}

#[test]
fn opencode_real_fixture_outputs_markdown() {
    let fixture_root = opencode_real_fixture_root();
    assert!(fixture_root.exists(), "fixture root must exist");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("XDG_DATA_HOME", fixture_root)
        .arg(opencode_real_uri())
        .assert()
        .success()
        .stdout(predicate::str::contains("# Thread"))
        .stdout(predicate::str::contains("## 1. User"));
}

#[cfg(unix)]
#[test]
fn write_create_streams_output_and_prints_uri() {
    let mock = setup_mock_bins(&[(
        "codex",
        r#"
if [ "$1" = "exec" ] && [ "$2" = "--json" ]; then
  echo '{"type":"thread.started","thread_id":"11111111-1111-4111-8111-111111111111"}'
  echo '{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"hello from create"}}'
  exit 0
fi
echo "unexpected args: $*" >&2
exit 7
"#,
    )]);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PATH", path_with_mock(mock.path()))
        .arg("agents://codex")
        .arg("-d")
        .arg("hello")
        .assert()
        .success()
        .stdout(predicate::str::contains("hello from create"))
        .stderr(predicate::str::contains(
            "created: agents://codex/11111111-1111-4111-8111-111111111111",
        ));
}

#[cfg(unix)]
#[test]
fn write_create_supports_shorthand_collection_uri() {
    let mock = setup_mock_bins(&[(
        "codex",
        r#"
if [ "$1" = "exec" ] && [ "$2" = "--json" ]; then
  echo '{"type":"thread.started","thread_id":"11111111-1111-4111-8111-111111111111"}'
  echo '{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"hello from create"}}'
  exit 0
fi
echo "unexpected args: $*" >&2
exit 7
"#,
    )]);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PATH", path_with_mock(mock.path()))
        .arg("codex")
        .arg("-d")
        .arg("hello")
        .assert()
        .success()
        .stdout(predicate::str::contains("hello from create"))
        .stderr(predicate::str::contains(
            "created: agents://codex/11111111-1111-4111-8111-111111111111",
        ));
}

#[cfg(unix)]
#[test]
fn write_create_with_codex_role_loads_role_config() {
    let mock = setup_mock_bins(&[(
        "codex",
        r#"
if [ "$1" != "exec" ] || [ "$2" != "--json" ]; then
  echo "unexpected args: $*" >&2
  exit 7
fi
seen_model=0
seen_effort=0
seen_instructions=0
seen_prompt=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --config)
      shift
      if [ "$1" = "model=gpt-5.3-codex" ]; then
        seen_model=1
      fi
      if [ "$1" = "model_reasoning_effort=high" ]; then
        seen_effort=1
      fi
      if [ "$1" = "developer_instructions=Focus on high priority issues." ]; then
        seen_instructions=1
      fi
      ;;
    hello)
      seen_prompt=1
      ;;
  esac
  shift
done
[ "$seen_model" -eq 1 ] || exit 8
[ "$seen_effort" -eq 1 ] || exit 9
[ "$seen_instructions" -eq 1 ] || exit 10
[ "$seen_prompt" -eq 1 ] || exit 11
echo '{"type":"thread.started","thread_id":"12345678-1111-4111-8111-111111111111"}'
echo '{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"role create ok"}}'
"#,
    )]);
    setup_codex_role_configs(mock.path());

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PATH", path_with_mock(mock.path()))
        .env("CODEX_HOME", mock.path())
        .arg("agents://codex/reviewer")
        .arg("-d")
        .arg("hello")
        .assert()
        .success()
        .stdout(predicate::str::contains("role create ok"))
        .stderr(predicate::str::contains(
            "created: agents://codex/12345678-1111-4111-8111-111111111111",
        ));
}

#[cfg(unix)]
#[test]
fn write_append_uses_resume_and_prints_updated_uri() {
    let mock = setup_mock_bins(&[(
        "codex",
        r#"
if [ "$1" = "exec" ] && [ "$2" = "resume" ] && [ "$3" = "--json" ]; then
  echo "{\"type\":\"thread.started\",\"thread_id\":\"$4\"}"
  echo '{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"hello from append"}}'
  exit 0
fi
echo "unexpected args: $*" >&2
exit 7
"#,
    )]);
    let target = "agents://codex/22222222-2222-4222-8222-222222222222";

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PATH", path_with_mock(mock.path()))
        .arg(target)
        .arg("--data")
        .arg("continue")
        .assert()
        .success()
        .stdout(predicate::str::contains("hello from append"))
        .stderr(predicate::str::contains(
            "updated: agents://codex/22222222-2222-4222-8222-222222222222",
        ));
}

#[cfg(unix)]
#[test]
fn write_create_passthroughs_all_query_options_without_normalization() {
    let workdir_text = "/tmp/workdir".to_string();
    let add_dir_a_text = "/tmp/add-a".to_string();
    let add_dir_b_text = "/tmp/add-b".to_string();
    let script = format!(
        r#"
if [ "$1" != "exec" ] || [ "$2" != "--json" ]; then
  echo "unexpected args: $*" >&2
  exit 7
fi
found_workdir=0
found_model=0
found_flag=0
count_add_dir=0
count_json=0
count_json_with_value=0
prompt_seen=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --workdir)
      shift
      [ "$1" = "{workdir_text}" ] || exit 9
      found_workdir=1
      ;;
    --add_dir)
      shift
      if [ "$1" = "{add_dir_a_text}" ] || [ "$1" = "{add_dir_b_text}" ]; then
        count_add_dir=$((count_add_dir + 1))
      else
        echo "unexpected add dir: $1" >&2
        exit 10
      fi
      ;;
    --model)
      shift
      [ "$1" = "gpt-5" ] || exit 11
      found_model=1
      ;;
    --flag)
      found_flag=1
      ;;
    --json)
      count_json=$((count_json + 1))
      if [ "$2" = "1" ]; then
        shift
        count_json_with_value=$((count_json_with_value + 1))
      fi
      ;;
    hello)
      prompt_seen=1
      ;;
  esac
  shift
done
if [ "$found_workdir" -ne 1 ] || [ "$count_add_dir" -ne 2 ] || [ "$found_model" -ne 1 ] || [ "$found_flag" -ne 1 ] || [ "$count_json" -ne 2 ] || [ "$count_json_with_value" -ne 1 ] || [ "$prompt_seen" -ne 1 ]; then
  echo "missing expected flags" >&2
  exit 12
fi
echo '{{"type":"thread.started","thread_id":"66666666-6666-4666-8666-666666666666"}}'
echo '{{"type":"item.completed","item":{{"id":"item_1","type":"agent_message","text":"query options ok"}}}}'
"#,
    );
    let mock = setup_mock_bins(&[("codex", script.as_str())]);

    let target = format!(
        "agents://codex?workdir={}&add_dir={}&add_dir={}&model=gpt-5&flag&json=1",
        encode_query_component(&workdir_text),
        encode_query_component(&add_dir_a_text),
        encode_query_component(&add_dir_b_text),
    );
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PATH", path_with_mock(mock.path()))
        .arg(target)
        .arg("-d")
        .arg("hello")
        .assert()
        .success()
        .stdout(predicate::str::contains("query options ok"))
        .stderr(predicate::str::contains("reserved by xurl").not())
        .stderr(predicate::str::contains(
            "created: agents://codex/66666666-6666-4666-8666-666666666666",
        ));
}

#[cfg(unix)]
#[test]
fn write_append_passthroughs_query_options() {
    let target_session = "22222222-2222-4222-8222-222222222222";
    let script = format!(
        r#"
if [ "$1" != "exec" ] || [ "$2" != "resume" ] || [ "$3" != "--json" ]; then
  echo "unexpected args: $*" >&2
  exit 7
fi
count_workdir=0
found_flag=0
found_prompt=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --workdir)
      shift
      if [ "$1" = "/tmp/a" ] || [ "$1" = "/tmp/b" ]; then
        count_workdir=$((count_workdir + 1))
      else
        exit 8
      fi
      ;;
    --flag)
      found_flag=1
      ;;
    "{target_session}")
      ;;
    continue)
      found_prompt=1
      ;;
  esac
  shift
done
[ "$count_workdir" -eq 2 ] || exit 9
[ "$found_flag" -eq 1 ] || exit 10
[ "$found_prompt" -eq 1 ] || exit 11
echo '{{"type":"thread.started","thread_id":"{target_session}"}}'
echo '{{"type":"item.completed","item":{{"id":"item_1","type":"agent_message","text":"append passthrough query"}}}}'
"#,
    );
    let mock = setup_mock_bins(&[("codex", script.as_str())]);
    let target = format!(
        "agents://codex/{target_session}?workdir={}&workdir={}&flag",
        encode_query_component("/tmp/a"),
        encode_query_component("/tmp/b"),
    );

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PATH", path_with_mock(mock.path()))
        .arg(target)
        .arg("--data")
        .arg("continue")
        .assert()
        .success()
        .stdout(predicate::str::contains("append passthrough query"))
        .stderr(predicate::str::contains("ignored query parameter").not())
        .stderr(predicate::str::contains(format!(
            "updated: agents://codex/{target_session}",
        )));
}

#[cfg(unix)]
#[test]
fn write_amp_passthroughs_workdir_and_add_dir_query_parameters() {
    let workdir_text = "/tmp/amp-workdir".to_string();
    let add_dir_text = "/tmp/amp-add".to_string();
    let script = format!(
        r#"
if [ "$1" != "-x" ] || [ "$2" != "hello" ] || [ "$3" != "--stream-json" ]; then
  echo "unexpected args: $*" >&2
  exit 7
fi
seen_workdir=0
seen_add_dir=0
seen_foo=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --workdir)
      shift
      [ "$1" = "{workdir_text}" ] || exit 8
      seen_workdir=1
      ;;
    --add_dir)
      shift
      [ "$1" = "{add_dir_text}" ] || exit 9
      seen_add_dir=1
      ;;
    --foo)
      shift
      [ "$1" = "bar" ] || exit 10
      seen_foo=1
      ;;
    *)
      ;;
  esac
  shift
done
[ "$seen_workdir" -eq 1 ] || exit 11
[ "$seen_add_dir" -eq 1 ] || exit 12
[ "$seen_foo" -eq 1 ] || exit 13
echo '{{"type":"system","subtype":"init","session_id":"T-77777777-7777-4777-8777-777777777777"}}'
echo '{{"type":"assistant","session_id":"T-77777777-7777-4777-8777-777777777777","message":{{"content":[{{"type":"text","text":"passthrough-ok"}}]}}}}'
echo '{{"type":"result","subtype":"success","session_id":"T-77777777-7777-4777-8777-777777777777","result":"ok"}}'
"#,
    );
    let mock = setup_mock_bins(&[("amp", script.as_str())]);
    let target = format!(
        "agents://amp?workdir={}&add_dir={}&foo=bar",
        encode_query_component(&workdir_text),
        encode_query_component(&add_dir_text),
    );

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PATH", path_with_mock(mock.path()))
        .arg(target)
        .arg("-d")
        .arg("hello")
        .assert()
        .success()
        .stdout(predicate::str::contains("passthrough-ok"))
        .stderr(predicate::str::contains("ignored query parameter `add_dir`").not())
        .stderr(predicate::str::contains(
            "created: agents://amp/T-77777777-7777-4777-8777-777777777777",
        ));
}

#[cfg(unix)]
#[test]
fn write_data_file_and_stdin_are_supported() {
    let mock = setup_mock_bins(&[(
        "codex",
        r#"
if [ "$1" != "exec" ] || [ "$2" != "--json" ]; then
  echo "unexpected args: $*" >&2
  exit 7
fi
if [ "$3" = "from-file" ]; then
  echo '{"type":"thread.started","thread_id":"33333333-3333-4333-8333-333333333333"}'
  echo '{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"file-ok"}}'
  exit 0
fi
if [ "$3" = "from-stdin" ]; then
  echo '{"type":"thread.started","thread_id":"44444444-4444-4444-8444-444444444444"}'
  echo '{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"stdin-ok"}}'
  exit 0
fi
echo "unexpected prompt: $3" >&2
exit 8
"#,
    )]);

    let prompt_file_dir = tempdir().expect("tempdir");
    let prompt_file = prompt_file_dir.path().join("prompt.txt");
    fs::write(&prompt_file, "from-file").expect("write prompt");

    let mut from_file = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    from_file
        .env("PATH", path_with_mock(mock.path()))
        .arg("agents://codex")
        .arg("-d")
        .arg(format!("@{}", prompt_file.display()))
        .assert()
        .success()
        .stdout(predicate::str::contains("file-ok"));

    let mut from_stdin = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    from_stdin
        .env("PATH", path_with_mock(mock.path()))
        .arg("agents://codex")
        .arg("-d")
        .arg("@-")
        .write_stdin("from-stdin")
        .assert()
        .success()
        .stdout(predicate::str::contains("stdin-ok"));
}

#[cfg(unix)]
#[test]
fn write_rejects_head_mode_and_child_uri() {
    let mock = setup_mock_bins(&[(
        "codex",
        r#"
echo "should not run" >&2
exit 99
"#,
    )]);

    let mut head_cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    head_cmd
        .env("PATH", path_with_mock(mock.path()))
        .arg("agents://codex")
        .arg("-I")
        .arg("-d")
        .arg("x")
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be combined"));

    let mut child_cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    child_cmd
        .env("PATH", path_with_mock(mock.path()))
        .arg(format!("agents://codex/{SESSION_ID}/{SUBAGENT_ID}"))
        .arg("-d")
        .arg("x")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "write mode only supports main thread URIs",
        ));
}

#[cfg(unix)]
#[test]
fn write_command_not_found_has_hint() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PATH", "")
        .env("XURL_CODEX_BIN", "codex")
        .arg("agents://codex")
        .arg("-d")
        .arg("hello")
        .assert()
        .failure()
        .stderr(predicate::str::contains("hint: write mode needs Codex CLI"));
}

#[cfg(unix)]
#[test]
fn write_amp_create_stream_json_path_works() {
    let mock = setup_mock_bins(&[(
        "amp",
        r#"
if [ "$1" = "-x" ] && [ "$3" = "--stream-json" ]; then
  echo '{"type":"system","subtype":"init","session_id":"T-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"}'
  echo '{"type":"assistant","session_id":"T-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","message":{"content":[{"type":"text","text":"hello from amp"}]}}'
  echo '{"type":"result","subtype":"success","session_id":"T-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","result":"hello from amp"}'
  exit 0
fi
echo "unexpected args: $*" >&2
exit 7
"#,
    )]);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PATH", path_with_mock(mock.path()))
        .arg("agents://amp")
        .arg("-d")
        .arg("hello")
        .assert()
        .success()
        .stdout(predicate::str::contains("hello from amp"))
        .stderr(predicate::str::contains(
            "created: agents://amp/T-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        ));
}

#[cfg(unix)]
#[test]
fn write_amp_role_uri_is_rejected_with_clear_error() {
    let mock = setup_mock_bins(&[(
        "amp",
        r#"
echo "should not run" >&2
exit 99
"#,
    )]);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PATH", path_with_mock(mock.path()))
        .arg("agents://amp/reviewer")
        .arg("-d")
        .arg("hello")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "does not support role-based write URI",
        ));
}

#[cfg(unix)]
#[test]
fn write_gemini_create_tolerates_non_json_prefix() {
    let mock = setup_mock_bins(&[(
        "gemini",
        r#"
if [ "$1" = "-p" ] && [ "$3" = "--output-format" ] && [ "$4" = "stream-json" ]; then
  echo 'YOLO mode is enabled.'
  echo '{"type":"init","session_id":"99999999-9999-4999-8999-999999999999"}'
  echo '{"type":"message","role":"assistant","content":"hello from gemini","delta":true}'
  echo '{"type":"result","status":"success"}'
  exit 0
fi
echo "unexpected args: $*" >&2
exit 7
"#,
    )]);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PATH", path_with_mock(mock.path()))
        .arg("agents://gemini")
        .arg("-d")
        .arg("hello")
        .assert()
        .success()
        .stdout(predicate::str::contains("hello from gemini"))
        .stderr(predicate::str::contains(
            "created: agents://gemini/99999999-9999-4999-8999-999999999999",
        ));
}

#[cfg(unix)]
#[test]
fn write_gemini_role_uri_is_rejected_with_clear_error() {
    let mock = setup_mock_bins(&[(
        "gemini",
        r#"
echo "should not run" >&2
exit 99
"#,
    )]);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PATH", path_with_mock(mock.path()))
        .arg("agents://gemini/reviewer")
        .arg("-d")
        .arg("hello")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "does not support role-based write URI",
        ));
}

#[cfg(unix)]
#[test]
fn write_pi_create_stream_json_path_works() {
    let mock = setup_mock_bins(&[(
        "pi",
        r#"
if [ "$1" = "-p" ] && [ "$3" = "--mode" ] && [ "$4" = "json" ]; then
  echo '{"type":"session","id":"aaaaaaaa-1111-4222-8333-bbbbbbbbbbbb"}'
  echo '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"hello from "}}'
  echo '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"pi"}}'
  exit 0
fi
echo "unexpected args: $*" >&2
exit 7
"#,
    )]);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PATH", path_with_mock(mock.path()))
        .arg("agents://pi")
        .arg("-d")
        .arg("hello")
        .assert()
        .success()
        .stdout(predicate::str::contains("hello from pi"))
        .stderr(predicate::str::contains(
            "created: agents://pi/aaaaaaaa-1111-4222-8333-bbbbbbbbbbbb",
        ));
}

#[cfg(unix)]
#[test]
fn write_pi_role_uri_is_rejected_with_clear_error() {
    let mock = setup_mock_bins(&[(
        "pi",
        r#"
echo "should not run" >&2
exit 99
"#,
    )]);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PATH", path_with_mock(mock.path()))
        .arg("agents://pi/reviewer")
        .arg("-d")
        .arg("hello")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "does not support role-based write URI",
        ));
}

#[cfg(unix)]
#[test]
fn write_opencode_create_tolerates_non_json_prefix() {
    let mock = setup_mock_bins(&[(
        "opencode",
        r#"
if [ "$1" = "run" ] && [ "$3" = "--format" ] && [ "$4" = "json" ]; then
  echo 'ProviderModelNotFoundError: ignored bootstrap log'
  echo '{"type":"session.start","sessionID":"ses_43a90e3adffejRgrTdlJa48CtE"}'
  echo '{"type":"assistant.delta","delta":"hello from "}'
  echo '{"type":"assistant.delta","delta":"opencode"}'
  exit 0
fi
echo "unexpected args: $*" >&2
exit 7
"#,
    )]);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PATH", path_with_mock(mock.path()))
        .arg("agents://opencode")
        .arg("-d")
        .arg("hello")
        .assert()
        .success()
        .stdout(predicate::str::contains("hello from opencode"))
        .stderr(predicate::str::contains(
            "created: agents://opencode/ses_43a90e3adffejRgrTdlJa48CtE",
        ));
}

#[cfg(unix)]
#[test]
fn write_opencode_role_uri_sets_agent_flag() {
    let mock = setup_mock_bins(&[(
        "opencode",
        r#"
if [ "$1" != "run" ] || [ "$3" != "--agent" ] || [ "$4" != "reviewer" ] || [ "$5" != "--format" ] || [ "$6" != "json" ]; then
  echo "unexpected args: $*" >&2
  exit 7
fi
echo '{"type":"session.start","sessionID":"ses_43a90e3adffejRgrTdlJa48CtE"}'
echo '{"type":"assistant.delta","delta":"role ok"}'
"#,
    )]);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PATH", path_with_mock(mock.path()))
        .arg("agents://opencode/reviewer")
        .arg("-d")
        .arg("hello")
        .assert()
        .success()
        .stdout(predicate::str::contains("role ok"))
        .stderr(predicate::str::contains(
            "created: agents://opencode/ses_43a90e3adffejRgrTdlJa48CtE",
        ));
}

#[cfg(unix)]
#[test]
fn write_claude_create_stream_json_path_works() {
    let mock = setup_mock_bins(&[(
        "claude",
        r#"
if [ "$1" = "-p" ] && [ "$2" = "--verbose" ] && [ "$3" = "--output-format" ] && [ "$4" = "stream-json" ]; then
  echo '{"type":"system","subtype":"init","session_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"}'
  echo '{"type":"assistant","session_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","message":{"content":[{"type":"text","text":"hello from claude"}]}}'
  echo '{"type":"result","subtype":"success","session_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","result":"hello from claude"}'
  exit 0
fi
echo "unexpected args: $*" >&2
exit 7
"#,
    )]);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PATH", path_with_mock(mock.path()))
        .arg("agents://claude")
        .arg("-d")
        .arg("hello")
        .assert()
        .success()
        .stdout(predicate::str::contains("hello from claude"))
        .stderr(predicate::str::contains(
            "created: agents://claude/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        ));
}

#[cfg(unix)]
#[test]
fn write_claude_role_uri_sets_agent_flag() {
    let mock = setup_mock_bins(&[(
        "claude",
        r#"
if [ "$1" != "-p" ] || [ "$2" != "--verbose" ] || [ "$3" != "--output-format" ] || [ "$4" != "stream-json" ]; then
  echo "unexpected args: $*" >&2
  exit 7
fi
seen_agent=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --agent)
      shift
      [ "$1" = "reviewer" ] || exit 8
      seen_agent=1
      ;;
  esac
  shift
done
[ "$seen_agent" -eq 1 ] || exit 9
echo '{"type":"system","subtype":"init","session_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"}'
echo '{"type":"assistant","session_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","message":{"content":[{"type":"text","text":"claude role ok"}]}}'
"#,
    )]);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PATH", path_with_mock(mock.path()))
        .arg("agents://claude/reviewer")
        .arg("-d")
        .arg("hello")
        .assert()
        .success()
        .stdout(predicate::str::contains("claude role ok"))
        .stderr(predicate::str::contains(
            "created: agents://claude/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        ));
}

#[cfg(unix)]
#[test]
fn write_output_flag_writes_assistant_text_to_file() {
    let mock = setup_mock_bins(&[(
        "codex",
        r#"
if [ "$1" = "exec" ] && [ "$2" = "--json" ]; then
  echo '{"type":"thread.started","thread_id":"55555555-5555-4555-8555-555555555555"}'
  echo '{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"file target"}}'
  exit 0
fi
echo "unexpected args: $*" >&2
exit 7
"#,
    )]);
    let output_dir = tempdir().expect("tempdir");
    let output = output_dir.path().join("write.txt");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("xurl"));
    cmd.env("PATH", path_with_mock(mock.path()))
        .arg("agents://codex")
        .arg("-d")
        .arg("hello")
        .arg("-o")
        .arg(&output)
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "created: agents://codex/55555555-5555-4555-8555-555555555555",
        ));

    let written = fs::read_to_string(output).expect("read output");
    assert_eq!(written, "file target");
}
