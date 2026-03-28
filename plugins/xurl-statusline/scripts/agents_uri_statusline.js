#!/usr/bin/env node

const fs = require("node:fs");
const path = require("node:path");
const readline = require("node:readline");

const AGENT_FILE_RE = /^agent-(?<agentId>[^/]+)\.jsonl$/;

async function main() {
  const payload = readPayload();
  if (!payload) {
    return;
  }

  const sessionId = payload.session_id;
  if (typeof sessionId !== "string" || sessionId.length === 0) {
    return;
  }

  const agentId = await inferAgentId(sessionId, payload.transcript_path);
  if (agentId) {
    process.stdout.write(`agents://claude/${sessionId}/${agentId}\n`);
    return;
  }

  process.stdout.write(`agents://claude/${sessionId}\n`);
}

function readPayload() {
  let raw;
  try {
    raw = fs.readFileSync(0, "utf8");
  } catch {
    return null;
  }

  if (raw.trim().length === 0) {
    return null;
  }

  try {
    const payload = JSON.parse(raw);
    return payload && typeof payload === "object" ? payload : null;
  } catch {
    return null;
  }
}

async function inferAgentId(sessionId, transcriptPath) {
  if (typeof transcriptPath !== "string" || transcriptPath.length === 0) {
    return null;
  }

  const agentIdFromHeader = await inferAgentIdFromHeader(sessionId, transcriptPath);
  if (agentIdFromHeader) {
    return agentIdFromHeader;
  }

  const match = AGENT_FILE_RE.exec(path.basename(transcriptPath));
  return match?.groups?.agentId ?? null;
}

async function inferAgentIdFromHeader(sessionId, transcriptPath) {
  let stream;
  try {
    stream = fs.createReadStream(transcriptPath, { encoding: "utf8" });
  } catch {
    return null;
  }

  const rl = readline.createInterface({
    input: stream,
    crlfDelay: Infinity,
  });

  let lineCount = 0;
  try {
    for await (const line of rl) {
      lineCount += 1;
      if (lineCount > 30) {
        break;
      }
      if (line.trim().length === 0) {
        continue;
      }

      let entry;
      try {
        entry = JSON.parse(line);
      } catch {
        continue;
      }

      if (
        entry &&
        typeof entry === "object" &&
        typeof entry.agentId === "string" &&
        entry.agentId.length > 0 &&
        typeof entry.sessionId === "string" &&
        entry.sessionId === sessionId &&
        entry.isSidechain === true
      ) {
        return entry.agentId;
      }

      return null;
    }
  } catch {
    return null;
  } finally {
    rl.close();
    stream.destroy();
  }

  return null;
}

main().catch(() => process.exit(0));
